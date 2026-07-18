#[cfg(feature = "apple-communication")]
mod communication;
mod delegate;

use std::{
    path::{Path, PathBuf},
    ptr::NonNull,
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc, Mutex, OnceLock,
    },
    time::{Duration, SystemTime},
};

use block2::RcBlock;
use delegate::RobiusNotificationsDelegate as Delegate;
use objc2::{
    rc::Retained,
    runtime::{Bool, ProtocolObject},
    sel,
};
use objc2_foundation::{
    NSArray, NSBundle, NSDictionary, NSError, NSNumber, NSObjectProtocol, NSSet, NSString, NSURL,
};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNAuthorizationStatus, UNErrorCode, UNMutableNotificationContent,
    UNNotification, UNNotificationAction, UNNotificationActionOptions, UNNotificationAttachment,
    UNNotificationCategory, UNNotificationCategoryOptions, UNNotificationContent,
    UNNotificationInterruptionLevel, UNNotificationRequest, UNNotificationSetting,
    UNNotificationSettings, UNNotificationSound, UNNotificationTrigger,
    UNTextInputNotificationAction, UNTimeIntervalNotificationTrigger, UNUserNotificationCenter,
    UNUserNotificationCenterDelegate,
};

use crate::{
    Action, ActionKind, ActiveIdsCallback, Error, NotificationOptions, NotificationSettings,
    PermissionCallback, Result, SettingsCallback, SettingsScope, Sound, Urgency,
};

/// The userInfo key our metadata pairs are nested under.
const METADATA_KEY: &str = "robius.metadata";

/// The category used by notifications without actions, so their dismissals still reach us.
const BASE_CATEGORY_ID: &str = "robius-notifications-base";

/// The OS fires scheduled requests itself, even after the app exits.
pub(crate) const NATIVE_SCHEDULING: bool = true;

pub(crate) fn show(options: NotificationOptions) -> Result<()> {
    ensure_app_bundle()?;
    // Without our delegate installed, a foregrounded app shows nothing.
    ensure_delegate();
    let center = UNUserNotificationCenter::currentNotificationCenter();
    let content = build_content(&center, &options)?;
    // Conversation notifications get the full communication treatment
    // (sender avatar, Focus integration) when the feature is enabled.
    let content = enrich_content(content, &options);
    // A scheduled time becomes an OS-side trigger; no trigger = deliver right
    // away. Reusing an id replaces the older notification either way, and
    // cancel() also removes still-pending scheduled requests.
    let trigger = scheduled_trigger(&options);
    let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
        &NSString::from_str(&options.id),
        &content,
        trigger.as_deref(),
    );
    // Fire and forget: success just means we handed it off to the OS.
    center.addNotificationRequest_withCompletionHandler(&request, None);
    Ok(())
}

/// Upgrades a conversation notification to a communication notification,
/// falling back to the plain content when that isn't possible (feature off,
/// no conversation, old OS, or missing entitlement).
#[cfg(feature = "apple-communication")]
fn enrich_content(
    content: Retained<UNMutableNotificationContent>,
    options: &NotificationOptions,
) -> Retained<UNNotificationContent> {
    communication::enrich(&content, options).unwrap_or_else(|| Retained::into_super(content))
}

#[cfg(not(feature = "apple-communication"))]
fn enrich_content(
    content: Retained<UNMutableNotificationContent>,
    _options: &NotificationOptions,
) -> Retained<UNNotificationContent> {
    Retained::into_super(content)
}

// lib.rs guarantees the time is in the future when it's set at all.
fn scheduled_trigger(options: &NotificationOptions) -> Option<Retained<UNNotificationTrigger>> {
    let time = options.scheduled_time?;
    // The trigger interval must be > 0; clamp in case the deadline just passed.
    let seconds = time
        .duration_since(SystemTime::now())
        .unwrap_or(Duration::ZERO)
        .as_secs_f64()
        .max(0.001);
    Some(Retained::into_super(
        UNTimeIntervalNotificationTrigger::triggerWithTimeInterval_repeats(seconds, false),
    ))
}

// Nothing to update: these platforms don't render notification progress,
// so show() drew no bar in the first place.
pub(crate) fn update_progress(_options: &NotificationOptions) -> Result<()> {
    Ok(())
}

pub(crate) fn cancel(id: &str) -> Result<()> {
    ensure_app_bundle()?;
    let center = UNUserNotificationCenter::currentNotificationCenter();
    let ids = NSArray::from_retained_slice(&[NSString::from_str(id)]);
    center.removePendingNotificationRequestsWithIdentifiers(&ids);
    center.removeDeliveredNotificationsWithIdentifiers(&ids);
    Ok(())
}

pub(crate) fn cancel_all() -> Result<()> {
    ensure_app_bundle()?;
    let center = UNUserNotificationCenter::currentNotificationCenter();
    center.removeAllPendingNotificationRequests();
    center.removeAllDeliveredNotifications();
    Ok(())
}

pub(crate) fn request_permission(callback: PermissionCallback, provisional: bool) -> Result<()> {
    ensure_app_bundle()?;
    // Install the delegate now, so an OpenSettings event can reach us
    // even if the app never shows a notification first.
    ensure_delegate();
    let center = UNUserNotificationCenter::currentNotificationCenter();
    let mut options =
        UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound | UNAuthorizationOptions::Badge;
    if crate::provides_notification_settings() {
        // Makes the system's notification settings UI link back to the app.
        options |= UNAuthorizationOptions::ProvidesAppNotificationSettings;
    }
    if provisional {
        // Quiet delivery with no prompt; the OS grants this immediately.
        options |= UNAuthorizationOptions::Provisional;
    }
    if crate::uses_critical_alerts() && !provisional {
        // Needs the Apple-granted critical-alerts entitlement; without it,
        // the OS just ignores this bit. Never OR'd into provisional requests:
        // critical-alert authorization shows a prompt, and provisional
        // promises not to.
        options |= UNAuthorizationOptions::CriticalAlert;
    }
    // The block must be a `Fn`, but our callback is `FnOnce`: park it for the one call.
    let callback = Mutex::new(Some(callback));
    let block = RcBlock::new(move |granted: Bool, error: *mut NSError| {
        let Some(callback) = callback.lock().unwrap().take() else {
            return;
        };
        let result = permission_result(granted.as_bool(), error);
        // This runs on a framework queue, so don't let a panicking callback
        // unwind across the ObjC frame.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| callback(result)));
    });
    center.requestAuthorizationWithOptions_completionHandler(options, &block);
    Ok(())
}

pub(crate) fn init_interaction_listener() -> Result<()> {
    ensure_app_bundle()?;
    ensure_delegate();
    Ok(())
}

// Every scope reports app-level settings; that's all the OS exposes to apps here.
pub(crate) fn notification_settings(_scope: SettingsScope, callback: SettingsCallback) -> Result<()> {
    ensure_app_bundle()?;
    let center = UNUserNotificationCenter::currentNotificationCenter();
    // The block must be a `Fn`, but our callback is `FnOnce`: park it for the one call.
    let callback = Mutex::new(Some(callback));
    let block = RcBlock::new(move |settings: NonNull<UNNotificationSettings>| {
        let Some(callback) = callback.lock().unwrap().take() else {
            return;
        };
        let settings = map_settings(unsafe { settings.as_ref() });
        // This runs on a framework queue, so don't let a panicking callback
        // unwind across the ObjC frame.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| callback(Ok(settings))));
    });
    center.getNotificationSettingsWithCompletionHandler(&block);
    Ok(())
}

pub(crate) fn active_notification_ids(callback: ActiveIdsCallback) -> Result<()> {
    ensure_app_bundle()?;
    let center = UNUserNotificationCenter::currentNotificationCenter();
    // The block must be a `Fn`, but our callback is `FnOnce`: park it for the one call.
    let callback = Mutex::new(Some(callback));
    let block = RcBlock::new(move |notifications: NonNull<NSArray<UNNotification>>| {
        let Some(callback) = callback.lock().unwrap().take() else {
            return;
        };
        let ids: Vec<String> = unsafe { notifications.as_ref() }
            .to_vec()
            .into_iter()
            .map(|notification| notification.request().identifier().to_string())
            .collect();
        // This runs on a framework queue, so don't let a panicking callback
        // unwind across the ObjC frame.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| callback(Ok(ids))));
    });
    center.getDeliveredNotificationsWithCompletionHandler(&block);
    Ok(())
}

// No per-channel or per-conversation pages here: every scope opens the app-level one.
pub(crate) fn open_notification_settings(_scope: SettingsScope) -> Result<()> {
    ensure_app_bundle()?;
    open_app_settings()
}

// iOS: deep-link to our app's page in the Settings app. UIApplication is
// main-thread-only, so hop over there first.
#[cfg(target_os = "ios")]
fn open_app_settings() -> Result<()> {
    dispatch2::run_on_main(|mtm| {
        // "app-settings:" (iOS 8+). The notification-specific constant is
        // iOS 15.4+ only and might not even link on older systems.
        let url_string = unsafe { objc2_ui_kit::UIApplicationOpenSettingsURLString };
        let url = NSURL::URLWithString(url_string).ok_or(Error::Unknown)?;
        let app = objc2_ui_kit::UIApplication::sharedApplication(mtm);
        // An empty options dictionary, so nothing to get the types wrong on.
        unsafe { app.openURL_options_completionHandler(&url, &NSDictionary::new(), None) };
        Ok(())
    })
}

// macOS: jump to the Notifications pane of System Settings. This URL scheme
// is undocumented, so it's best-effort; openURL tells us whether it worked.
#[cfg(target_os = "macos")]
fn open_app_settings() -> Result<()> {
    let url = NSURL::URLWithString(&NSString::from_str(
        "x-apple.systempreferences:com.apple.Notifications-Settings.extension",
    ))
    .ok_or(Error::Unsupported)?;
    if objc2_app_kit::NSWorkspace::sharedWorkspace().openURL(&url) {
        Ok(())
    } else {
        Err(Error::Unsupported)
    }
}

// Other Apple platforms have no settings page we can open.
#[cfg(not(any(target_os = "ios", target_os = "macos")))]
fn open_app_settings() -> Result<()> {
    Err(Error::Unsupported)
}

fn map_settings(settings: &UNNotificationSettings) -> NotificationSettings {
    // Provisional/Ephemeral are limited grants, but notifications do get shown.
    let enabled = matches!(
        settings.authorizationStatus(),
        UNAuthorizationStatus::Authorized
            | UNAuthorizationStatus::Provisional
            | UNAuthorizationStatus::Ephemeral
    );
    NotificationSettings {
        enabled,
        urgency: None,
        sound_enabled: setting_flag(settings.soundSetting()),
        badge_enabled: setting_flag(settings.badgeSetting()),
        customized_by_user: None,
        priority_conversation: None,
    }
}

// NotSupported (or anything unknown) means "can't say".
fn setting_flag(setting: UNNotificationSetting) -> Option<bool> {
    match setting {
        UNNotificationSetting::Enabled => Some(true),
        UNNotificationSetting::Disabled => Some(false),
        _ => None,
    }
}

// Installs our delegate on the center. show() needs this too (for foreground
// presentation); interactions with no app handler just queue up harmlessly.
fn ensure_delegate() {
    // The center only holds its delegate weakly, so this static keeps ours alive forever.
    static DELEGATE: OnceLock<DelegateHolder> = OnceLock::new();
    let delegate = DELEGATE.get_or_init(|| DelegateHolder(Delegate::new()));
    let protocol: &ProtocolObject<dyn UNUserNotificationCenterDelegate> =
        ProtocolObject::from_ref(&*delegate.0);
    UNUserNotificationCenter::currentNotificationCenter().setDelegate(Some(protocol));
}

// After setup the delegate is only ever poked by ObjC callbacks, never from Rust.
struct DelegateHolder(Retained<Delegate>);
unsafe impl Send for DelegateHolder {}
unsafe impl Sync for DelegateHolder {}

// Bail out when not running from a .app bundle: UNUserNotificationCenter throws
// an ObjC exception (killing the process) in unbundled binaries, e.g. `cargo run`.
fn ensure_app_bundle() -> Result<()> {
    if NSBundle::mainBundle().bundleIdentifier().is_some() {
        Ok(())
    } else {
        Err(Error::NoAppBundle)
    }
}

fn build_content(
    center: &UNUserNotificationCenter,
    options: &NotificationOptions,
) -> Result<Retained<UNMutableNotificationContent>> {
    let content = UNMutableNotificationContent::new();
    if let Some(title) = &options.title {
        content.setTitle(&NSString::from_str(title));
    }
    if let Some(subtitle) = &options.subtitle {
        content.setSubtitle(&NSString::from_str(subtitle));
    }
    if let Some(body) = &options.body {
        content.setBody(&NSString::from_str(body));
    }
    if let Some(count) = options.badge_count {
        content.setBadge(Some(&NSNumber::new_u32(count)));
    }
    // An explicit group wins; otherwise a conversation groups its notifications.
    let thread_id = options
        .group
        .as_deref()
        .or_else(|| options.conversation.as_ref().map(|conversation| conversation.id.as_str()));
    if let Some(thread_id) = thread_id {
        content.setThreadIdentifier(&NSString::from_str(thread_id));
    }
    if let Some(sound) = notification_sound(options.sound.as_ref(), options.bypass_dnd) {
        content.setSound(Some(&sound));
    }
    // bypass_dnd = a critical alert, which punches through DND/Focus. Without
    // the critical-alerts entitlement the OS silently downgrades it — fine.
    if options.bypass_dnd {
        set_interruption_level(&content, UNNotificationInterruptionLevel::Critical);
    } else if let Some(urgency) = options.urgency {
        set_interruption_level(&content, interruption_level(urgency));
    }
    if !options.metadata.is_empty() {
        set_metadata(&content, &options.metadata);
    }
    if let Some(image) = &options.image {
        let attachment = build_attachment(image)?;
        content.setAttachments(&NSArray::from_retained_slice(&[attachment]));
    }
    // progress, timestamp, persistent, lock_screen_visibility, and
    // conversation_messages have no UNNotification equivalent: no-ops here.
    let category_id = ensure_categories_registered(center, &options.actions);
    content.setCategoryIdentifier(&NSString::from_str(&category_id));
    Ok(content)
}

fn notification_sound(sound: Option<&Sound>, critical: bool) -> Option<Retained<UNNotificationSound>> {
    // Critical alerts need the critical sound variants to play during DND.
    match (sound.unwrap_or(&Sound::Default), critical) {
        // A nil sound means silence.
        (Sound::Silent, _) => None,
        (Sound::Default, false) => Some(UNNotificationSound::defaultSound()),
        (Sound::Default, true) => Some(UNNotificationSound::defaultCriticalSound()),
        (Sound::Named(name), false) => {
            Some(UNNotificationSound::soundNamed(&NSString::from_str(name)))
        }
        (Sound::Named(name), true) => {
            Some(UNNotificationSound::criticalSoundNamed(&NSString::from_str(name)))
        }
    }
}

// interruptionLevel only exists on iOS 15+/macOS 12+; calling it on older systems would crash.
fn set_interruption_level(
    content: &UNMutableNotificationContent,
    level: UNNotificationInterruptionLevel,
) {
    if !content.respondsToSelector(sel!(setInterruptionLevel:)) {
        return;
    }
    content.setInterruptionLevel(level);
}

fn interruption_level(urgency: Urgency) -> UNNotificationInterruptionLevel {
    match urgency {
        Urgency::Low => UNNotificationInterruptionLevel::Passive,
        Urgency::Normal => UNNotificationInterruptionLevel::Active,
        Urgency::Critical => UNNotificationInterruptionLevel::TimeSensitive,
    }
}

// Stash the metadata pairs in userInfo, nested under one key so we can find them again.
// A flat [k1, v1, k2, v2] array, not a dict: keeps order and duplicate keys,
// matching the other backends.
fn set_metadata(content: &UNMutableNotificationContent, metadata: &[(String, String)]) {
    let flat: Vec<Retained<NSString>> = metadata
        .iter()
        .flat_map(|(key, value)| [NSString::from_str(key), NSString::from_str(value)])
        .collect();
    let user_info = NSDictionary::from_retained_objects(
        &[&*NSString::from_str(METADATA_KEY)],
        &[NSArray::from_retained_slice(&flat)],
    );
    // All plist-safe types (strings in an array in a dict), which is what userInfo wants.
    unsafe { content.setUserInfo(&Retained::cast_unchecked(user_info)) };
}

/// Reads back the metadata pairs that [`set_metadata`] stored in userInfo.
pub(super) fn metadata_from_content(content: &UNNotificationContent) -> Vec<(String, String)> {
    let Some(value) = content.userInfo().objectForKey(&NSString::from_str(METADATA_KEY)) else {
        return Vec::new();
    };
    let Some(flat) = value.downcast_ref::<NSArray>() else {
        return Vec::new();
    };
    let mut strings = flat
        .iter()
        .filter_map(|item| item.downcast::<NSString>().ok())
        .map(|string| string.to_string());
    let mut pairs = Vec::new();
    while let (Some(key), Some(value)) = (strings.next(), strings.next()) {
        pairs.push((key, value));
    }
    pairs
}

// UNNotificationCategory is immutable and only touched through ObjC.
struct RegisteredCategory {
    id: String,
    category: Retained<UNNotificationCategory>,
}
unsafe impl Send for RegisteredCategory {}

fn registered_categories() -> &'static Mutex<Vec<RegisteredCategory>> {
    static CATEGORIES: Mutex<Vec<RegisteredCategory>> = Mutex::new(Vec::new());
    &CATEGORIES
}

/// Makes sure a category matching `actions` (plus the base one) is registered,
/// and returns its id for the notification content.
fn ensure_categories_registered(center: &UNUserNotificationCenter, actions: &[Action]) -> String {
    let category_id = if actions.is_empty() {
        BASE_CATEGORY_ID.to_owned()
    } else {
        actions_category_id(actions)
    };

    let mut registered = registered_categories().lock().unwrap();
    seed_categories_from_system(center, &mut registered);
    let mut added = register_category(&mut registered, BASE_CATEGORY_ID, &[]);
    added |= register_category(&mut registered, &category_id, actions);
    if added {
        // setNotificationCategories replaces the whole set, so pass everything we've registered.
        let all: Vec<_> = registered.iter().map(|entry| entry.category.clone()).collect();
        center.setNotificationCategories(&NSSet::from_retained_slice(&all));
    }
    category_id
}

// The daemon's category set outlives us, but ours starts empty each run and
// setNotificationCategories replaces the whole set. So before our first replace,
// pull in what's already registered; otherwise notifications still up from a
// previous run would lose their action buttons.
fn seed_categories_from_system(
    center: &UNUserNotificationCenter,
    registered: &mut Vec<RegisteredCategory>,
) {
    static SEEDED: AtomicBool = AtomicBool::new(false);
    // One attempt per run; callers hold the registry lock, so no one races us.
    if SEEDED.swap(true, Ordering::Relaxed) {
        return;
    }
    let (tx, rx) = mpsc::channel();
    let block = RcBlock::new(move |set: NonNull<NSSet<UNNotificationCategory>>| {
        let existing: Vec<RegisteredCategory> = unsafe { set.as_ref() }
            .to_vec()
            .into_iter()
            .map(|category| RegisteredCategory {
                id: category.identifier().to_string(),
                category,
            })
            .collect();
        let _ = tx.send(existing);
    });
    center.getNotificationCategoriesWithCompletionHandler(&block);
    // The completion runs on a framework queue, so blocking here (even on the
    // main thread) is safe. On timeout just proceed unseeded.
    let Ok(existing) = rx.recv_timeout(Duration::from_secs(2)) else {
        return;
    };
    for entry in existing {
        if !registered.iter().any(|known| known.id == entry.id) {
            registered.push(entry);
        }
    }
}

fn register_category(registered: &mut Vec<RegisteredCategory>, id: &str, actions: &[Action]) -> bool {
    if registered.iter().any(|entry| entry.id == id) {
        return false;
    }
    registered.push(RegisteredCategory {
        id: id.to_owned(),
        category: build_category(id, actions),
    });
    true
}

// Same action set -> same id, so categories don't pile up across shows.
// FNV-1a by hand (like windows' tag()): the id must be identical across runs
// and Rust versions, which DefaultHasher doesn't guarantee.
fn actions_category_id(actions: &[Action]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for action in actions {
        let fields = [
            action.id.as_str(),
            action.title.as_str(),
            if matches!(action.kind, ActionKind::Reply) { "reply" } else { "button" },
            action.placeholder.as_deref().unwrap_or(""),
            if action.destructive { "1" } else { "0" },
            if action.foreground { "1" } else { "0" },
        ];
        for field in fields {
            // A 0 byte ends each field, so shifted boundaries can't collide.
            for byte in field.bytes().chain(std::iter::once(0)) {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
    }
    format!("robius-notifications-actions-{hash:016x}")
}

fn build_category(id: &str, actions: &[Action]) -> Retained<UNNotificationCategory> {
    let actions: Vec<Retained<UNNotificationAction>> = actions.iter().map(build_action).collect();
    // CustomDismissAction makes the system tell our delegate about dismissals.
    UNNotificationCategory::categoryWithIdentifier_actions_intentIdentifiers_options(
        &NSString::from_str(id),
        &NSArray::from_retained_slice(&actions),
        &NSArray::new(),
        UNNotificationCategoryOptions::CustomDismissAction,
    )
}

fn build_action(action: &Action) -> Retained<UNNotificationAction> {
    let mut options = UNNotificationActionOptions::empty();
    if action.destructive {
        options |= UNNotificationActionOptions::Destructive;
    }
    if action.foreground {
        options |= UNNotificationActionOptions::Foreground;
    }
    let id = NSString::from_str(&action.id);
    let title = NSString::from_str(&action.title);
    match action.kind {
        ActionKind::Button => {
            UNNotificationAction::actionWithIdentifier_title_options(&id, &title, options)
        }
        ActionKind::Reply => {
            let placeholder = NSString::from_str(action.placeholder.as_deref().unwrap_or(""));
            Retained::into_super(
                UNTextInputNotificationAction::actionWithIdentifier_title_options_textInputButtonTitle_textInputPlaceholder(
                    &id, &title, options, &title, &placeholder,
                ),
            )
        }
    }
}

// The system MOVES the attached file into its own store (only bundle-internal
// files get copied), so hand it a throwaway copy and keep the caller's file intact.
fn build_attachment(path: &Path) -> Result<Retained<UNNotificationAttachment>> {
    let copy = copy_for_attachment(path)?;
    let copy_path = copy.to_str().ok_or(Error::InvalidNotification)?;
    let url = NSURL::fileURLWithPath(&NSString::from_str(copy_path));
    // An empty identifier makes the system generate one.
    unsafe {
        UNNotificationAttachment::attachmentWithIdentifier_URL_options_error(
            &NSString::from_str(""),
            &url,
            None,
        )
    }
    // e.g. an unsupported file type or an over-sized image
    .map_err(|_| {
        let _ = std::fs::remove_file(&copy);
        Error::InvalidNotification
    })
}

// A unique temp path per attachment. The extension survives the copy because
// the system uses it to infer the file type.
fn copy_for_attachment(path: &Path) -> Result<PathBuf> {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut name = format!("robius-notification-{}-{count}", std::process::id());
    if let Some(ext) = path.extension().and_then(|ext| ext.to_str()) {
        name.push('.');
        name.push_str(ext);
    }
    let copy = std::env::temp_dir().join(name);
    std::fs::copy(path, &copy)?;
    Ok(copy)
}

fn permission_result(granted: bool, error: *mut NSError) -> Result<bool> {
    if granted {
        return Ok(true);
    }
    if error.is_null() {
        return Ok(false);
    }
    // "Not allowed" is just a denial; anything else we can't say much about.
    match UNErrorCode(unsafe { &*error }.code()) {
        UNErrorCode::NotificationsNotAllowed => Ok(false),
        _ => Err(Error::Unknown),
    }
}

#[cfg(test)]
mod tests {
    // A `cargo test` binary isn't a .app bundle, so the guard must say no — touching
    // UNUserNotificationCenter here would throw an ObjC exception and kill the process.
    #[test]
    fn unpackaged_binary_has_no_app_bundle() {
        assert!(matches!(
            super::ensure_app_bundle(),
            Err(crate::Error::NoAppBundle)
        ));
    }
}
