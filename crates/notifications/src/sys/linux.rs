//! Linux backend: the `org.freedesktop.Notifications` D-Bus service,
//! which works on both X11 and Wayland.

use std::{
    collections::HashMap,
    panic::AssertUnwindSafe,
    sync::{Mutex, OnceLock},
    time::Duration,
};

use zbus::{
    blocking::{Connection, Proxy},
    zvariant::Value,
    Message,
};

use crate::{
    ActionKind, ActiveIdsCallback, Error, Interaction, InteractionKind, NotificationOptions,
    NotificationSettings, PermissionCallback, Progress, Result, SettingsCallback, SettingsScope,
    Sound, Urgency,
};

/// No OS-side scheduling; lib.rs's in-process timer fires scheduled showings,
/// so `show()` never sees a future `scheduled_time`.
pub(crate) const NATIVE_SCHEDULING: bool = false;

const DEST: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";
const IFACE: &str = "org.freedesktop.Notifications";

/// Wire prefix for user action ids, so they can't collide with the
/// reserved "default" and "inline-reply" keys.
const ACTION_ID_PREFIX: &str = "app.";

/// What we remember about a shown notification, so that later signals
/// about it (by server id) can be turned back into `Interaction`s.
#[derive(Clone)]
struct Shown {
    /// Our notification id, from `NotificationOptions.id`.
    id: String,
    metadata: Vec<(String, String)>,
    /// The id of the action shown as an inline reply field, if any.
    reply_action_id: Option<String>,
}

/// Our notification id -> the server's u32 id, for replacing and canceling.
fn server_ids() -> &'static Mutex<HashMap<String, u32>> {
    static MAP: OnceLock<Mutex<HashMap<String, u32>>> = OnceLock::new();
    MAP.get_or_init(Mutex::default)
}

/// The server's u32 id -> our info about that notification.
fn shown_notifications() -> &'static Mutex<HashMap<u32, Shown>> {
    static MAP: OnceLock<Mutex<HashMap<u32, Shown>>> = OnceLock::new();
    MAP.get_or_init(Mutex::default)
}

/// Opens a session bus connection with a method call timeout,
/// so a hung daemon can't block callers forever.
fn open_session() -> Result<Connection> {
    zbus::blocking::connection::Builder::session()
        .and_then(|builder| builder.method_timeout(Duration::from_secs(25)).build())
        // no usable session bus means notifications can't work here
        .map_err(|_| Error::NoService)
}

/// The shared session bus connection used for method calls.
fn connection() -> Result<Connection> {
    static CONNECTION: OnceLock<Connection> = OnceLock::new();
    if let Some(connection) = CONNECTION.get() {
        return Ok(connection.clone());
    }
    let connection = open_session()?;
    Ok(CONNECTION.get_or_init(|| connection).clone())
}

fn notifications_proxy(connection: &Connection) -> Result<Proxy<'static>> {
    Proxy::new(connection, DEST, PATH, IFACE).map_err(map_zbus_error)
}

/// The daemon capabilities we care about.
#[derive(Clone, Copy)]
struct Capabilities {
    actions: bool,
    body_markup: bool,
    inline_reply: bool,
}

/// Asks the daemon what it supports, cached after the first successful ask.
fn capabilities(proxy: &Proxy<'_>) -> Capabilities {
    static CAPABILITIES: Mutex<Option<Capabilities>> = Mutex::new(None);
    if let Some(capabilities) = *CAPABILITIES.lock().unwrap() {
        return capabilities;
    }
    match proxy.call::<_, _, Vec<String>>("GetCapabilities", &()) {
        Ok(list) => {
            let capabilities = Capabilities {
                actions: list.iter().any(|cap| cap == "actions"),
                body_markup: list.iter().any(|cap| cap == "body-markup"),
                inline_reply: list.iter().any(|cap| cap == "inline-reply"),
            };
            *CAPABILITIES.lock().unwrap() = Some(capabilities);
            capabilities
        }
        // couldn't ask; assume the least, but don't cache that
        Err(_) => Capabilities {
            actions: false,
            body_markup: false,
            inline_reply: false,
        },
    }
}

/// Maps a zbus error, turning "no notification daemon on the bus" into NoService.
fn map_zbus_error(err: zbus::Error) -> Error {
    let no_service = match &err {
        zbus::Error::MethodError(name, _, _) => matches!(
            name.as_str(),
            "org.freedesktop.DBus.Error.ServiceUnknown"
                | "org.freedesktop.DBus.Error.NameHasNoOwner"
        ),
        zbus::Error::FDO(fdo_err) => matches!(
            &**fdo_err,
            zbus::fdo::Error::ServiceUnknown(_) | zbus::fdo::Error::NameHasNoOwner(_)
        ),
        _ => false,
    };
    if no_service {
        Error::NoService
    } else {
        Error::DBus(err)
    }
}

pub(crate) fn show(options: NotificationOptions) -> Result<()> {
    // Make sure the signal listener is up so interactions get delivered.
    let _ = ensure_listener();

    let connection = connection()?;
    let proxy = notifications_proxy(&connection)?;
    let capabilities = capabilities(&proxy);

    // The summary is the title, or the body when there's no title.
    let mut body_lines = Vec::new();
    // only escape body text when the daemon actually parses markup
    let escape = |text: &str| {
        if capabilities.body_markup {
            escape_markup(text)
        } else {
            text.to_owned()
        }
    };
    let summary = match options.title.as_deref().filter(|t| !t.trim().is_empty()) {
        Some(title) => {
            if let Some(body) = &options.body {
                body_lines.push(escape(body));
            }
            title.to_owned()
        }
        None => options.body.clone().unwrap_or_default(),
    };
    // Linux has no dedicated subtitle, so it goes on its own line in the body.
    if let Some(subtitle) = &options.subtitle {
        body_lines.push(escape(subtitle));
    }
    let body = body_lines.join("\n");

    // Actions are flat [key, label, ...] pairs; the "default" one is
    // what a click on the notification body itself invokes.
    let mut actions = Vec::new();
    let mut reply_action_id = None;
    let mut reply_placeholder = None;
    if capabilities.actions {
        actions.push("default".to_owned());
        actions.push("Open".to_owned());
        for action in &options.actions {
            let inline_reply = action.kind == ActionKind::Reply
                && capabilities.inline_reply
                && reply_action_id.is_none();
            if inline_reply {
                // KDE-style inline reply; the text comes back via NotificationReplied
                reply_action_id = Some(action.id.clone());
                reply_placeholder = action.placeholder.clone();
                actions.push("inline-reply".to_owned());
            } else {
                // reply actions degrade to plain buttons without inline-reply support
                actions.push(format!("{ACTION_ID_PREFIX}{}", action.id));
            }
            actions.push(action.title.clone());
        }
    }

    let mut hints: HashMap<&str, Value> = HashMap::new();
    let urgency = match options.urgency.unwrap_or_default() {
        Urgency::Low => 0u8,
        Urgency::Normal => 1,
        Urgency::Critical => 2,
    };
    hints.insert("urgency", Value::U8(urgency));
    if let Some(desktop_entry) = crate::app_id() {
        // lets the daemon find the app's name and icon from its .desktop file
        hints.insert("desktop-entry", Value::from(desktop_entry));
    }
    if options.conversation.is_some() {
        // advisory: tells the daemon this is a received chat message
        hints.insert("category", Value::from("im.received"));
    }
    if let Some(image) = &options.image {
        let image = std::fs::canonicalize(image)?;
        hints.insert("image-path", Value::from(image.to_string_lossy().into_owned()));
    }
    match &options.sound {
        Some(Sound::Silent) => {
            hints.insert("suppress-sound", Value::from(true));
        }
        Some(Sound::Named(name)) => {
            hints.insert("sound-name", Value::from(name.clone()));
        }
        _ => {}
    }
    if let Some(placeholder) = reply_placeholder {
        hints.insert("x-kde-reply-placeholder-text", Value::from(placeholder));
    }
    if let Some(Progress::Determinate { current, total }) = options.progress {
        // "value" is the daemon's percentage hint; total > 0 is pre-validated.
        // Indeterminate gets no hint: there's no daemon convention for it.
        let percent = (u64::from(current) * 100 / u64::from(total)).min(100) as i32;
        hints.insert("value", Value::I32(percent));
    }
    if options.persistent {
        hints.insert("resident", Value::from(true));
    }
    // no daemon equivalents: timestamp, lock_screen_visibility, bypass_dnd,
    // and conversation_messages (daemons render one body; history is Android-only)

    // 0 means "never expires" to the daemon, so round tiny timeouts up to 1ms;
    // persistent notifications always get 0, regardless of any timeout.
    let expire_timeout = if options.persistent {
        0
    } else {
        options
            .timeout
            .map_or(-1, |timeout| timeout.as_millis().clamp(1, i32::MAX as u128) as i32)
    };
    // Reusing the previous server id makes the daemon replace that notification.
    let replaces_id = server_ids()
        .lock()
        .unwrap()
        .get(&options.id)
        .copied()
        .unwrap_or(0);

    let server_id: u32 = proxy
        .call(
            "Notify",
            &(
                app_name(),
                replaces_id,
                "", // app_icon: the desktop-entry hint covers this
                summary,
                body,
                actions,
                hints,
                expire_timeout,
            ),
        )
        .map_err(map_zbus_error)?;

    server_ids().lock().unwrap().insert(options.id.clone(), server_id);
    let mut shown = shown_notifications().lock().unwrap();
    if replaces_id != 0 && replaces_id != server_id {
        // the daemon gave the replacement a fresh id; drop the stale entry
        shown.remove(&replaces_id);
    }
    shown.insert(
        server_id,
        Shown {
            id: options.id,
            metadata: options.metadata,
            reply_action_id,
        },
    );
    Ok(())
}

pub(crate) fn update_progress(options: &NotificationOptions) -> Result<()> {
    // Re-running Notify reuses the previous server id (replaces_id), which
    // updates the notification in place. Force the update itself silent
    // (suppress-sound hint), since some daemons re-alert on replacement.
    let mut options = options.clone();
    options.sound = Some(Sound::Silent);
    show(options)
}

pub(crate) fn cancel(id: &str) -> Result<()> {
    let Some(server_id) = server_ids().lock().unwrap().get(id).copied() else {
        // never shown (or already closed): nothing to cancel
        return Ok(());
    };
    let connection = connection()?;
    let proxy = notifications_proxy(&connection)?;
    close_notification(&proxy, server_id)
}

pub(crate) fn cancel_all() -> Result<()> {
    let ids: Vec<u32> = server_ids().lock().unwrap().values().copied().collect();
    if ids.is_empty() {
        return Ok(());
    }
    let connection = connection()?;
    let proxy = notifications_proxy(&connection)?;
    for server_id in ids {
        close_notification(&proxy, server_id)?;
    }
    Ok(())
}

fn close_notification(proxy: &Proxy<'_>, server_id: u32) -> Result<()> {
    match proxy
        .call::<_, _, ()>("CloseNotification", &server_id)
        .map_err(map_zbus_error)
    {
        // some daemons error when the notification is already gone; fine by us
        Err(Error::DBus(zbus::Error::MethodError(..))) => Ok(()),
        result => result,
    }
}

pub(crate) fn request_permission(callback: PermissionCallback, _provisional: bool) -> Result<()> {
    // Linux has no notification permission prompt, provisional or otherwise.
    callback(Ok(true));
    Ok(())
}

pub(crate) fn active_notification_ids(callback: ActiveIdsCallback) -> Result<()> {
    // Entries are pruned on NotificationClosed, so what's still tracked
    // approximates "still showing". Dedupe in case a replacement briefly
    // left two server ids pointing at the same crate id.
    let mut ids: Vec<String> = shown_notifications()
        .lock()
        .unwrap()
        .values()
        .map(|shown| shown.id.clone())
        .collect();
    ids.sort_unstable();
    ids.dedup();
    callback(Ok(ids));
    Ok(())
}

pub(crate) fn notification_settings(_scope: SettingsScope, callback: SettingsCallback) -> Result<()> {
    // Linux has no per-app/channel/conversation settings; "enabled" just
    // means the session bus and a notification daemon are reachable.
    let enabled = service_reachable()?;
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

/// Whether the session bus and a notification daemon are reachable.
fn service_reachable() -> Result<bool> {
    let proxy = match connection().and_then(|connection| notifications_proxy(&connection)) {
        Ok(proxy) => proxy,
        Err(Error::NoService) => return Ok(false),
        Err(err) => return Err(err),
    };
    // building the proxy doesn't touch the bus, so actually ask the daemon
    match proxy
        .call::<_, _, (String, String, String, String)>("GetServerInformation", &())
        .map_err(map_zbus_error)
    {
        Ok(_) => Ok(true),
        Err(Error::NoService) => Ok(false),
        Err(err) => Err(err),
    }
}

pub(crate) fn open_notification_settings(_scope: SettingsScope) -> Result<()> {
    // no standard cross-desktop way to open notification settings
    Err(Error::Unsupported)
}

pub(crate) fn init_interaction_listener() -> Result<()> {
    ensure_listener()
}

/// Starts the signal listener thread (once). It owns its own connection
/// so its blocking reads don't get in the way of method calls.
fn ensure_listener() -> Result<()> {
    static STARTED: Mutex<bool> = Mutex::new(false);
    let mut started = STARTED.lock().unwrap();
    if *started {
        return Ok(());
    }

    let connection = open_session()?;
    let proxy = notifications_proxy(&connection)?;
    // One iterator covers all of the daemon's signals; we dispatch by name.
    let signals = proxy.receive_all_signals().map_err(map_zbus_error)?;

    // A restarted daemon forgets our notifications and hands out low ids
    // again, so forget ours too whenever the name changes owner.
    let dbus_proxy = zbus::blocking::fdo::DBusProxy::new(&connection).map_err(map_zbus_error)?;
    let owner_changes = dbus_proxy
        .receive_name_owner_changed_with_args(&[(0, DEST)])
        .map_err(map_zbus_error)?;
    std::thread::Builder::new()
        .name("robius-notifications-watch".to_owned())
        .spawn(move || {
            for change in owner_changes {
                // A daemon first acquiring the name (e.g. activated by our own
                // first Notify) doesn't invalidate anything; a lost or replaced
                // owner does.
                let restarted = change
                    .args()
                    .map(|args| args.old_owner().is_some())
                    .unwrap_or(true);
                if restarted {
                    server_ids().lock().unwrap().clear();
                    shown_notifications().lock().unwrap().clear();
                }
            }
        })
        .map_err(Error::Io)?;

    std::thread::Builder::new()
        .name("robius-notifications".to_owned())
        .spawn(move || {
            // keep the connection alive for as long as we're listening
            let _connection = connection;
            for message in signals {
                // a panicking interaction handler shouldn't kill this thread
                let _ = std::panic::catch_unwind(AssertUnwindSafe(|| handle_signal(&message)));
            }
            // the connection died; let a later show()/init spawn a new listener
            *STARTED.lock().unwrap() = false;
        })
        .map_err(Error::Io)?;
    *started = true;
    Ok(())
}

/// Turns a daemon signal back into an app-facing interaction.
fn handle_signal(message: &Message) {
    let header = message.header();
    let Some(member) = header.member() else {
        return;
    };
    match member.as_str() {
        "ActionInvoked" => {
            let Ok((server_id, action)) = message.body().deserialize::<(u32, String)>() else {
                return;
            };
            let Some(shown) = lookup(server_id) else {
                return;
            };
            let kind = match action.as_str() {
                // "default" is our body-click action
                "default" => InteractionKind::Activated,
                // the daemon's inline-reply button maps back to our reply action
                "inline-reply" => InteractionKind::Action {
                    id: shown.reply_action_id.clone().unwrap_or(action),
                },
                // user action ids were prefixed on the wire; undo that here
                _ => InteractionKind::Action {
                    id: match action.strip_prefix(ACTION_ID_PREFIX) {
                        Some(id) => id.to_owned(),
                        None => action,
                    },
                },
            };
            deliver(shown, kind);
        }
        // KDE's inline reply: the daemon sends us the submitted text directly
        "NotificationReplied" => {
            let Ok((server_id, text)) = message.body().deserialize::<(u32, String)>() else {
                return;
            };
            let Some(shown) = lookup(server_id) else {
                return;
            };
            let Some(action_id) = shown.reply_action_id.clone() else {
                return;
            };
            deliver(shown, InteractionKind::Reply { action_id, text });
        }
        "NotificationClosed" => {
            let Ok((server_id, reason)) = message.body().deserialize::<(u32, u32)>() else {
                return;
            };
            let Some(shown) = remove(server_id) else {
                return;
            };
            // reason 2 = dismissed by the user; other reasons aren't interactions
            if reason == 2 {
                deliver(shown, InteractionKind::Dismissed);
            }
        }
        _ => {}
    }
}

fn deliver(shown: Shown, kind: InteractionKind) {
    crate::deliver_interaction(Interaction {
        notification_id: shown.id,
        kind,
        metadata: shown.metadata,
    });
}

fn lookup(server_id: u32) -> Option<Shown> {
    shown_notifications().lock().unwrap().get(&server_id).cloned()
}

/// Forgets a closed notification, in both maps.
fn remove(server_id: u32) -> Option<Shown> {
    let shown = shown_notifications().lock().unwrap().remove(&server_id)?;
    let mut ids = server_ids().lock().unwrap();
    // a replacement may have re-pointed our id at a newer server id; keep that
    if ids.get(&shown.id) == Some(&server_id) {
        ids.remove(&shown.id);
    }
    Some(shown)
}

/// The app name shown by the daemon: the app id if set, else the exe name.
fn app_name() -> String {
    crate::app_id().or_else(exe_name).unwrap_or_default()
}

fn exe_name() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.file_stem()?.to_string_lossy().into_owned())
}

/// Markup-capable daemons render the body as limited markup, so escape user text.
fn escape_markup(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}
