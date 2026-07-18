//! Windows backend: WinRT toast notifications.
//!
//! Interactions are delivered via per-toast event handlers, so they only
//! arrive while the app is running; activating a toast after the app exits
//! won't relaunch it (that would need a registered COM activator).
//! Scheduled toasts fire even after the app exits, but they can't carry
//! those per-toast handlers at all, so interactions with them are lost.

use std::{
    collections::HashMap,
    fmt::Write as _,
    path::Path,
    sync::{Mutex, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use windows::{
    core::{IInspectable, Interface, HSTRING},
    Data::Xml::Dom::XmlDocument,
    Foundation::{DateTime, IPropertyValue, IReference, PropertyValue, TypedEventHandler, Uri},
    System::Launcher,
    UI::Notifications::{
        NotificationData, NotificationSetting, NotificationUpdateResult,
        ScheduledToastNotification, ToastActivatedEventArgs, ToastDismissalReason,
        ToastDismissedEventArgs, ToastNotification, ToastNotificationManager, ToastNotifier,
    },
    Win32::{System::Com::CoTaskMemFree, UI::Shell::GetCurrentProcessExplicitAppUserModelID},
};

use crate::{
    ActionKind, ActiveIdsCallback, Error, Interaction, InteractionKind, NotificationOptions,
    NotificationSettings, PermissionCallback, Progress, Result, SettingsCallback, SettingsScope,
    Sound, Urgency,
};

/// The OS schedules toasts for us (they even fire after the app exits).
pub(crate) const NATIVE_SCHEDULING: bool = true;

/// All our toasts share one group; the tag alone identifies each notification.
const GROUP: &str = "robius";

/// The built-in PowerShell AUMID, borrowed so unpackaged dev builds can still
/// show toasts (they get attributed to "Windows PowerShell").
const POWERSHELL_AUMID: &str =
    "{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\\WindowsPowerShell\\v1.0\\powershell.exe";

pub(crate) fn show(options: NotificationOptions) -> Result<()> {
    let notifier = ToastNotificationManager::CreateToastNotifierWithId(&aumid())?;
    if notifier.Setting()? != NotificationSetting::Enabled {
        return Err(Error::PermissionDenied);
    }

    // Lets active_notification_ids translate the OS tag back to our id.
    remember_tag(&options.id);

    if options.scheduled_time.is_some() {
        return add_to_schedule(&notifier, &options);
    }

    let toast_tag = tag(&options.id);
    // An immediate show supersedes a still-pending scheduled toast of this id,
    // just like scheduling one replaces its predecessor.
    let _ = remove_scheduled(&notifier, Some(&toast_tag));
    let quiet = if options.progress.is_some() {
        // A progress toast alerts only when first shown; re-shows stay quiet.
        progress_seqs().lock().unwrap().contains_key(&toast_tag)
    } else {
        // Re-shown without progress: no longer a progress notification.
        progress_seqs().lock().unwrap().remove(&toast_tag);
        false
    };
    show_toast(&notifier, &options, quiet)
}

/// Shows the toast right now. `quiet` (progress re-shows and updates)
/// suppresses the popup and sound so the user isn't re-alerted.
fn show_toast(notifier: &ToastNotifier, options: &NotificationOptions, quiet: bool) -> Result<()> {
    let document = XmlDocument::new()?;
    document.LoadXml(&HSTRING::from(build_toast_xml(options, quiet)?))?;

    let toast = ToastNotification::CreateToastNotification(&document)?;
    let toast_tag = tag(&options.id);
    // Same tag + group as an earlier toast makes the OS replace it.
    toast.SetTag(&HSTRING::from(toast_tag.as_str()))?;
    toast.SetGroup(&HSTRING::from(GROUP))?;

    if let Some(progress) = options.progress {
        // The XML's {progressValue}/{progressStatus} bindings read from this.
        let sequence = bump_progress_seq(&toast_tag);
        toast.SetData(&progress_data(progress, sequence)?)?;
    }

    if let Some(timeout) = options.timeout {
        set_expiration(&toast, timeout)?;
    }
    if quiet || matches!(options.urgency, Some(Urgency::Low)) {
        // No popup banner; the toast lands quietly in the action center.
        toast.SetSuppressPopup(true)?;
    }

    register_interaction_handlers(&toast, options)?;
    notifier.Show(&toast)?;
    Ok(())
}

/// Hands the toast to the OS to show at its scheduled time. Scheduled toasts
/// can't carry our per-toast event handlers, so interactions with them are
/// lost (see the module docs).
fn add_to_schedule(notifier: &ToastNotifier, options: &NotificationOptions) -> Result<()> {
    // Guaranteed Some and in the future by lib.rs.
    let time = options.scheduled_time.unwrap_or_else(SystemTime::now);
    let toast_tag = tag(&options.id);
    // Same id replaces the earlier pending toast, like Show does for shown ones.
    remove_scheduled(notifier, Some(&toast_tag))?;

    let document = XmlDocument::new()?;
    document.LoadXml(&HSTRING::from(build_toast_xml(options, false)?))?;

    let toast =
        ScheduledToastNotification::CreateScheduledToastNotification(&document, datetime(time))?;
    // Same tag/group scheme as regular toasts, so cancel() can find it.
    toast.SetTag(&HSTRING::from(toast_tag.as_str()))?;
    toast.SetGroup(&HSTRING::from(GROUP))?;
    if let Some(timeout) = options.timeout {
        let expiration = PropertyValue::CreateDateTime(datetime(time + timeout))?;
        // Best effort: older Windows versions don't support this on scheduled toasts.
        let _ = toast.SetExpirationTime(&expiration.cast::<IReference<DateTime>>()?);
    }
    if matches!(options.urgency, Some(Urgency::Low)) {
        toast.SetSuppressPopup(true)?;
    }

    notifier.AddToSchedule(&toast)?;
    Ok(())
}

pub(crate) fn update_progress(options: &NotificationOptions) -> Result<()> {
    // lib.rs only calls this with progress set; be safe anyway.
    let Some(progress) = options.progress else {
        return Err(Error::InvalidNotification);
    };
    let notifier = ToastNotificationManager::CreateToastNotifierWithId(&aumid())?;
    let toast_tag = tag(&options.id);
    // No sequence entry means cancel() got here first (or it was never
    // shown): drop the update rather than resurrect a cancelled toast.
    if !progress_seqs().lock().unwrap().contains_key(&toast_tag) {
        return Ok(());
    }
    let data = progress_data(progress, bump_progress_seq(&toast_tag))?;

    // Update moves the bar in place, silently; no new toast is shown.
    let result = notifier.UpdateWithTagAndGroup(
        &data,
        &HSTRING::from(toast_tag.as_str()),
        &HSTRING::from(GROUP),
    )?;
    if result == NotificationUpdateResult::Succeeded {
        Ok(())
    } else if result == NotificationUpdateResult::NotificationNotFound {
        // The toast is gone (dismissed or expired); quietly re-show the whole thing.
        show_toast(&notifier, options, true)
    } else {
        Err(Error::Unknown)
    }
}

pub(crate) fn cancel(id: &str) -> Result<()> {
    let toast_tag = tag(id);
    // A fresh show of this id afterwards should alert again.
    progress_seqs().lock().unwrap().remove(&toast_tag);

    // Also kill a matching still-pending scheduled toast.
    let notifier = ToastNotificationManager::CreateToastNotifierWithId(&aumid())?;
    remove_scheduled(&notifier, Some(&toast_tag))?;

    let history = ToastNotificationManager::History()?;
    ignore_not_found(history.RemoveGroupedTagWithId(
        &HSTRING::from(toast_tag.as_str()),
        &HSTRING::from(GROUP),
        &aumid(),
    ))
}

pub(crate) fn cancel_all() -> Result<()> {
    progress_seqs().lock().unwrap().clear();

    let notifier = ToastNotificationManager::CreateToastNotifierWithId(&aumid())?;
    remove_scheduled(&notifier, None)?;

    let history = ToastNotificationManager::History()?;
    // Only clear our own group; other apps may share the fallback AUMID.
    ignore_not_found(history.RemoveGroupWithId(&HSTRING::from(GROUP), &aumid()))
}

/// Removes pending scheduled toasts: the one with `tag`, or all of ours.
fn remove_scheduled(notifier: &ToastNotifier, tag: Option<&str>) -> Result<()> {
    for scheduled in notifier.GetScheduledToastNotifications()? {
        let matches = match tag {
            Some(tag) => scheduled
                .Tag()
                .is_ok_and(|t| t.to_string_lossy() == tag),
            // Match our group; if a toast doesn't even expose one, treat it
            // as ours, since everything this notifier scheduled came from us.
            None => scheduled
                .Group()
                .map(|group| group.to_string_lossy() == GROUP)
                .unwrap_or(true),
        };
        if matches {
            notifier.RemoveFromSchedule(&scheduled)?;
        }
    }
    Ok(())
}

pub(crate) fn request_permission(callback: PermissionCallback, _provisional: bool) -> Result<()> {
    // Windows has no permission prompt (provisional or otherwise); just
    // report whether toasts are enabled.
    callback(Ok(toasts_enabled()));
    Ok(())
}

pub(crate) fn notification_settings(
    _scope: SettingsScope,
    callback: SettingsCallback,
) -> Result<()> {
    // Windows only exposes the app-wide on/off state, so every scope
    // reports the same thing and the finer-grained fields stay None.
    // Unlike request_permission's optimistic default, a read-back that
    // can't even query the notifier reports the error honestly.
    let enabled = match ToastNotificationManager::CreateToastNotifierWithId(&aumid())
        .and_then(|notifier| notifier.Setting())
    {
        Ok(setting) => setting == NotificationSetting::Enabled,
        Err(error) => {
            callback(Err(error.into()));
            return Ok(());
        }
    };
    callback(Ok(NotificationSettings {
        enabled,
        urgency: None,
        sound_enabled: None,
        badge_enabled: None,
        customized_by_user: None,
        priority_conversation: None,
    }));
    Ok(())
}

pub(crate) fn open_notification_settings(_scope: SettingsScope) -> Result<()> {
    // Windows has no public per-app or per-channel settings URI, so every
    // scope lands on the system notification settings page.
    let uri = Uri::CreateUri(&HSTRING::from("ms-settings:notifications"))?;
    // Blocking is fine here; the launch resolves quickly.
    if Launcher::LaunchUriAsync(&uri)?.get()? {
        Ok(())
    } else {
        Err(Error::Unknown)
    }
}

pub(crate) fn init_interaction_listener() -> Result<()> {
    // Nothing to set up: interactions arrive via the per-toast event handlers.
    Ok(())
}

pub(crate) fn active_notification_ids(callback: ActiveIdsCallback) -> Result<()> {
    callback(collect_active_ids());
    Ok(())
}

/// The ids of our still-showing toasts. Only toasts shown by this run of the
/// app can be translated back from their OS tag; older tags are skipped.
fn collect_active_ids() -> Result<Vec<String>> {
    let history = ToastNotificationManager::History()?;
    let toasts = history.GetHistoryWithId(&aumid())?;
    let tags = tag_ids().lock().unwrap();
    let mut ids = Vec::new();
    for toast in toasts {
        let Ok(toast_tag) = toast.Tag() else { continue };
        if let Some(id) = tags.get(&toast_tag.to_string_lossy()) {
            ids.push(id.clone());
        }
    }
    Ok(ids)
}

/// tag -> id for every toast we've shown, so [`collect_active_ids`] can
/// translate the OS's tags back into our notification ids.
fn tag_ids() -> &'static Mutex<HashMap<String, String>> {
    static TAG_IDS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    TAG_IDS.get_or_init(Mutex::default)
}

fn remember_tag(id: &str) {
    tag_ids().lock().unwrap().insert(tag(id), id.to_owned());
}

/// Per-tag NotificationData sequence numbers; an entry also means the tag
/// was already shown with progress (so a re-show shouldn't alert).
fn progress_seqs() -> &'static Mutex<HashMap<String, u32>> {
    static SEQS: OnceLock<Mutex<HashMap<String, u32>>> = OnceLock::new();
    SEQS.get_or_init(Mutex::default)
}

/// Bumps and returns the tag's data sequence number (first use: 1).
fn bump_progress_seq(tag: &str) -> u32 {
    let mut seqs = progress_seqs().lock().unwrap();
    let seq = seqs.entry(tag.to_owned()).or_insert(0);
    *seq += 1;
    *seq
}

/// The data a progress toast's bound XML fields read their values from.
fn progress_data(progress: Progress, sequence: u32) -> Result<NotificationData> {
    let data = NotificationData::new()?;
    data.SetSequenceNumber(sequence)?;
    let values = data.Values()?;
    values.Insert(
        &HSTRING::from("progressValue"),
        &HSTRING::from(progress_value(progress)),
    )?;
    // No status line; the binding just needs the key to exist.
    values.Insert(&HSTRING::from("progressStatus"), &HSTRING::new())?;
    Ok(data)
}

/// The progressValue string: "indeterminate" or a 0..1 decimal.
fn progress_value(progress: Progress) -> String {
    match progress {
        Progress::Indeterminate => "indeterminate".to_owned(),
        // total is pre-validated non-zero; past-the-end counts as done.
        Progress::Determinate { current, total } => {
            format!("{:.4}", f64::from(current.min(total)) / f64::from(total))
        }
    }
}

/// Whether toasts are currently enabled for our AUMID. Optimistically
/// assumes yes if the notifier can't even be created.
fn toasts_enabled() -> bool {
    ToastNotificationManager::CreateToastNotifierWithId(&aumid())
        .and_then(|notifier| notifier.Setting())
        .map(|setting| setting == NotificationSetting::Enabled)
        .unwrap_or(true)
}

/// The AUMID our toasts get attributed to: the app-provided id (re-read
/// every time so a later `set_app_id` isn't ignored), else one registered
/// by a shortcut/installer, else the borrowed PowerShell one.
fn aumid() -> HSTRING {
    if let Some(id) = crate::app_id() {
        return HSTRING::from(id);
    }
    // Only the fallback is cached; looking it up involves an OS call.
    static FALLBACK: OnceLock<HSTRING> = OnceLock::new();
    FALLBACK
        .get_or_init(|| {
            let id = registered_aumid().unwrap_or_else(|| POWERSHELL_AUMID.to_owned());
            HSTRING::from(id)
        })
        .clone()
}

/// The AUMID a shortcut or installer registered for this process, if any.
fn registered_aumid() -> Option<String> {
    unsafe {
        let pwstr = GetCurrentProcessExplicitAppUserModelID().ok()?;
        let id = pwstr.to_hstring().ok().map(|id| id.to_string_lossy());
        CoTaskMemFree(Some(pwstr.as_ptr() as *const _));
        id
    }
}

/// Tags max out at 16 chars before Windows 10 1903, so hash the id down
/// to 16 hex chars. FNV-1a is tiny and stable across runs, which lets a
/// later process still cancel a toast by its id.
fn tag(id: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in id.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Builds the ToastGeneric XML payload for this notification.
/// Windows has no equivalent for conversations (and their message history),
/// event timestamps, lock-screen visibility, or a per-notification
/// Do-Not-Disturb bypass, so those options are ignored here.
fn build_toast_xml(options: &NotificationOptions, quiet: bool) -> Result<String> {
    let launch = encode_arguments(&options.id, None, &options.metadata);
    let mut xml = String::new();
    let _ = write!(xml, r#"<toast launch="{}""#, escape_xml(&launch));
    if matches!(options.urgency, Some(Urgency::Critical)) {
        xml.push_str(r#" scenario="urgent""#);
    } else if options.persistent {
        // Reminder toasts stay on screen until the user acts on them.
        xml.push_str(r#" scenario="reminder""#);
    }
    if options.timeout.is_some_and(|t| t >= Duration::from_secs(25)) {
        xml.push_str(r#" duration="long""#);
    }
    xml.push('>');

    xml.push_str(r#"<visual><binding template="ToastGeneric">"#);
    let texts = [&options.title, &options.body, &options.subtitle];
    for text in texts.into_iter().flatten() {
        let _ = write!(xml, "<text>{}</text>", escape_xml(text));
    }
    if let Some(image) = &options.image {
        let _ = write!(
            xml,
            r#"<image placement="hero" src="{}"/>"#,
            escape_xml(&file_uri(image)?),
        );
    }
    if let Some(progress) = options.progress {
        if options.scheduled_time.is_none() {
            // Bound to the attached NotificationData, so update_progress
            // can move the bar in place.
            xml.push_str(r#"<progress value="{progressValue}" status="{progressStatus}"/>"#);
        } else {
            // Scheduled toasts can't carry NotificationData; bake the values in.
            let _ = write!(
                xml,
                r#"<progress value="{}" status=""/>"#,
                progress_value(progress),
            );
        }
    }
    xml.push_str("</binding></visual>");

    // Windows rejects toasts with more than 5 actions or inputs, so drop the extras.
    let actions = &options.actions[..options.actions.len().min(5)];
    if !actions.is_empty() {
        xml.push_str("<actions>");
        // Inputs have to come before all the buttons.
        for action in actions {
            if action.kind == ActionKind::Reply {
                let _ = write!(xml, r#"<input id="{}" type="text""#, escape_xml(&action.id));
                if let Some(placeholder) = &action.placeholder {
                    let _ = write!(xml, r#" placeHolderContent="{}""#, escape_xml(placeholder));
                }
                xml.push_str("/>");
            }
        }
        for action in actions {
            let arguments = encode_arguments(&options.id, Some(&action.id), &options.metadata);
            let _ = write!(
                xml,
                r#"<action content="{}" arguments="{}""#,
                escape_xml(&action.title),
                escape_xml(&arguments),
            );
            if action.kind == ActionKind::Reply {
                // Ties the button to its text input.
                let _ = write!(xml, r#" hint-inputId="{}""#, escape_xml(&action.id));
            }
            xml.push_str("/>");
        }
        xml.push_str("</actions>");
    }

    if quiet {
        // Quiet re-shows (progress updates) must not re-alert the user.
        xml.push_str(r#"<audio silent="true"/>"#);
    } else {
        match &options.sound {
            Some(Sound::Silent) => xml.push_str(r#"<audio silent="true"/>"#),
            Some(Sound::Named(name))
                if name.starts_with("ms-winsoundevent:") || name.starts_with("ms-appx:") =>
            {
                let _ = write!(xml, r#"<audio src="{}"/>"#, escape_xml(name));
            }
            // Unrecognized names and Default both get the default sound.
            _ => {}
        }
    }

    xml.push_str("</toast>");
    Ok(xml)
}

/// Hooks up the per-toast Activated/Dismissed events that feed the user's
/// interactions back to the app. These must be registered before Show.
fn register_interaction_handlers(
    toast: &ToastNotification,
    options: &NotificationOptions,
) -> Result<()> {
    toast.Activated(&TypedEventHandler::new(
        |_sender: &Option<ToastNotification>, args: &Option<IInspectable>| {
            let Some(args) = args else { return Ok(()) };
            let Ok(args) = args.cast::<ToastActivatedEventArgs>() else {
                return Ok(());
            };
            let arguments = args
                .Arguments()
                .map(|arguments| arguments.to_string_lossy())
                .unwrap_or_default();
            let (notification_id, action_id, metadata) = decode_arguments(&arguments);
            let kind = match action_id {
                None => InteractionKind::Activated,
                Some(action_id) => match reply_text(&args, &action_id) {
                    Some(text) => InteractionKind::Reply { action_id, text },
                    None => InteractionKind::Action { id: action_id },
                },
            };
            crate::deliver_interaction(Interaction {
                notification_id,
                kind,
                metadata,
            });
            Ok(())
        },
    ))?;

    let notification_id = options.id.clone();
    let metadata = options.metadata.clone();
    toast.Dismissed(&TypedEventHandler::new(
        move |_sender: &Option<ToastNotification>, args: &Option<ToastDismissedEventArgs>| {
            let Some(args) = args else { return Ok(()) };
            // Only user dismissals count; timeouts and app hides aren't interactions.
            if args.Reason()? == ToastDismissalReason::UserCanceled {
                crate::deliver_interaction(Interaction {
                    notification_id: notification_id.clone(),
                    kind: InteractionKind::Dismissed,
                    metadata: metadata.clone(),
                });
            }
            Ok(())
        },
    ))?;
    Ok(())
}

/// The text the user typed into the quick-reply input, if this activation has any.
fn reply_text(args: &ToastActivatedEventArgs, action_id: &str) -> Option<String> {
    let inputs = args.UserInput().ok()?;
    let value = inputs.Lookup(&HSTRING::from(action_id)).ok()?;
    let text = value.cast::<IPropertyValue>().ok()?.GetString().ok()?;
    Some(text.to_string_lossy())
}

/// Converts to WinRT's DateTime: 100ns ticks since 1601-01-01, which is
/// 11644473600 seconds before the Unix epoch.
fn datetime(time: SystemTime) -> DateTime {
    const UNIX_EPOCH_TICKS: i64 = 116_444_736_000_000_000;
    let unix = time.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);
    DateTime {
        UniversalTime: UNIX_EPOCH_TICKS.saturating_add((unix.as_nanos() / 100) as i64),
    }
}

/// Windows only takes an absolute expiration time, so add the timeout to now.
fn set_expiration(toast: &ToastNotification, timeout: Duration) -> Result<()> {
    let expiration = PropertyValue::CreateDateTime(datetime(SystemTime::now() + timeout))?;
    toast.SetExpirationTime(&expiration.cast::<IReference<DateTime>>()?)?;
    Ok(())
}

/// Converts an image path into the `file:///` URI form toast XML wants.
fn file_uri(path: &Path) -> Result<String> {
    // `canonicalize` prepends a `\\?\` verbatim prefix; strip it back out.
    let path = std::fs::canonicalize(path)?;
    let text = path.to_string_lossy();
    let text = text
        .strip_prefix(r"\\?\UNC\")
        .map(|rest| format!(r"\\{rest}"))
        .or_else(|| text.strip_prefix(r"\\?\").map(str::to_owned))
        .unwrap_or_else(|| text.into_owned());

    let mut uri = String::from("file:///");
    for ch in text.chars() {
        match ch {
            '\\' => uri.push('/'),
            c if c.is_ascii_alphanumeric() || "/:-_.~".contains(c) => uri.push(c),
            c => {
                let mut buf = [0u8; 4];
                for byte in c.encode_utf8(&mut buf).bytes() {
                    let _ = write!(uri, "%{byte:02X}");
                }
            }
        }
    }
    Ok(uri)
}

/// Packs the notification id, the pressed action's id (if any), and the
/// metadata into a `k=v&k=v` string; it's all the OS hands back on activation.
fn encode_arguments(
    notification_id: &str,
    action_id: Option<&str>,
    metadata: &[(String, String)],
) -> String {
    let mut arguments = format!("id={}", percent_encode(notification_id));
    if let Some(action_id) = action_id {
        let _ = write!(arguments, "&action={}", percent_encode(action_id));
    }
    for (key, value) in metadata {
        let _ = write!(
            arguments,
            "&m={}:{}",
            percent_encode(key),
            percent_encode(value),
        );
    }
    arguments
}

/// The inverse of [`encode_arguments`].
fn decode_arguments(arguments: &str) -> (String, Option<String>, Vec<(String, String)>) {
    let mut notification_id = String::new();
    let mut action_id = None;
    let mut metadata = Vec::new();
    for pair in arguments.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        match key {
            "id" => notification_id = percent_decode(value),
            "action" => action_id = Some(percent_decode(value)),
            "m" => {
                if let Some((key, value)) = value.split_once(':') {
                    metadata.push((percent_decode(key), percent_decode(value)));
                }
            }
            _ => {}
        }
    }
    (notification_id, action_id, metadata)
}

/// Percent-encodes everything but unreserved URI characters.
fn percent_encode(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char)
            }
            _ => {
                let _ = write!(encoded, "%{byte:02X}");
            }
        }
    }
    encoded
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = (bytes[i] == b'%')
            .then(|| bytes.get(i + 1..i + 3))
            .flatten()
            .and_then(|hex| std::str::from_utf8(hex).ok())
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match hex {
            Some(byte) => {
                decoded.push(byte);
                i += 3;
            }
            None => {
                decoded.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

/// Escapes text for use in XML content and attribute values.
fn escape_xml(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            // XML 1.0 can't represent these at all (not even escaped),
            // and LoadXml would reject the whole toast.
            c if (c < ' ' && c != '\t' && c != '\n' && c != '\r')
                || c == '\u{FFFE}'
                || c == '\u{FFFF}' =>
            {
                escaped.push(' ')
            }
            _ => escaped.push(ch),
        }
    }
    escaped
}

/// Canceling something that's already gone is fine.
fn ignore_not_found(result: windows::core::Result<()>) -> Result<()> {
    match result {
        Ok(()) => Ok(()),
        // HRESULT_FROM_WIN32(ERROR_NOT_FOUND)
        Err(error) if error.code().0 as u32 == 0x8007_0490 => Ok(()),
        Err(error) => Err(error.into()),
    }
}
