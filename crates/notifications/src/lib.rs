//! Multi-platform abstractions for showing system notifications
//! and handling the user's interactions with them.
//!
//! This crate covers the whole notification round trip: your app shows a
//! notification with [`Notification::show`], the user taps it, presses one of
//! its action buttons, or types into its quick-reply field, and your app gets
//! that [`Interaction`] back through the handler you registered with
//! [`set_interaction_handler`].
//!
//! ## Platform behavior
//! All types & functions in this crate are completely platform-independent,
//! so your app code doesn't need to deal with any of this, but here are some
//! details about how things are implemented on a per-platform basis.
//! * **Android**: notifications are posted via `NotificationManager` using
//!   notification channels, with action buttons and `RemoteInput` quick replies.
//!   * **Minimum API level: 26 (Android 8.0).** The bundled Java helper is loaded
//!     via `InMemoryDexClassLoader`, which requires API 26. Set `minSdk` to at
//!     least 26 in your app.
//!   * On Android 13 and newer, your app manifest must declare the
//!     `android.permission.POST_NOTIFICATIONS` permission, and you must call
//!     [`request_permission`] before notifications can be shown.
//!   * Tapping the notification body launches (or re-focuses) your app.
//!     See the README for the details of when the tap interaction itself
//!     can be delivered to your handler.
//! * **iOS** and **macOS**: notifications use `UNUserNotificationCenter` from the
//!   UserNotifications framework, with full support for action buttons,
//!   quick replies, and interactions delivered even after an app relaunch.
//!   * You must call [`request_permission`] before notifications can be shown.
//!   * On macOS, the app must be running from a bundled `.app` (a bare binary
//!     run via `cargo run` gets [`Error::NoAppBundle`], because the system
//!     has no app registration to attribute notifications to).
//! * **Windows**: notifications are WinRT toast notifications, with action
//!   buttons and quick-reply text boxes.
//!   * Interactions are delivered only while the app is running; a toast
//!     activated after the app exits doesn't relaunch it.
//!   * Unpackaged apps (no MSIX/sparse package) don't have a registered
//!     AppUserModelID by default; see [`set_app_id`] for how that's handled.
//! * **Linux**: notifications use the standard `org.freedesktop.Notifications`
//!   D-Bus service, which works on both X11 and Wayland.
//!   * Action buttons are shown if the notification daemon supports them
//!     (GNOME, KDE, and most others do).
//!   * Quick-reply actions degrade to plain buttons unless the daemon
//!     supports inline replies (KDE Plasma does).
//!   * Interactions are delivered only while the app is running.
//!
//! ## Terminology
//! Notification APIs use overlapping words for different things,
//! so here's how this crate's names map onto each platform's:
//! * [`NotificationChannel`] (via [`set_channel`](Notification::set_channel))
//!   is an Android notification channel, which Android's own settings UI
//!   presents to users as a notification **"category"** (e.g., "Messages").
//! * [`NotificationChannel::set_group`] is an Android notification channel
//!   *group*: a titled **section of categories** in that same settings UI
//!   (e.g., one section per account).
//! * [`Notification::set_group`] is unrelated to either of the above: it's the
//!   visual-stacking **thread** id (iOS/macOS "thread identifier", Android
//!   notification group) that piles related notifications together on screen.
//! * [`Conversation`] (via [`set_conversation`](Notification::set_conversation))
//!   is an ongoing chat with a person or group — "conversation" is the
//!   platforms' own term: Android's Conversations section and per-conversation
//!   settings, and the conversation id in Apple's communication notifications.
//! * iOS's `UNNotificationCategory` is none of the above: it's Apple's
//!   action-set descriptor, which this crate manages internally whenever you
//!   [`add_action`](Notification::add_action); it never appears in this API.
//!
//! ## Completion
//! A successful [`Notification::show`] means the notification was handed off
//! to the OS, not that it was displayed or seen: the OS may still suppress it,
//! e.g., due to a Do-Not-Disturb mode or missing permission. On Android, a
//! missing notification permission is reported synchronously as
//! [`Error::PermissionDenied`].
//!
//! ## Interactions and thread contexts
//! Register your handler with [`set_interaction_handler`] as early as possible
//! during app startup: interactions can arrive at any moment, including ones
//! that launched your app (e.g., the user tapped a notification of an app that
//! wasn't running, on platforms that support relaunch-on-tap).
//! Interactions that arrive before a handler is set are queued up
//! and delivered as soon as one is set.
//!
//! The handler may run on any thread (a platform callback thread, the main UI
//! thread, or a background thread), so use a communication primitive like a
//! channel to get interactions over to your app's UI/main logic, or something
//! similar from your UI toolkit, e.g., `Cx::post_action` in Makepad.
//!
//! ## Examples
//!
//! ```no_run
//! use robius_notifications::{Action, Interaction, InteractionKind, Notification};
//!
//! // Do this once, as early as possible at app startup.
//! robius_notifications::set_interaction_handler(|interaction: Interaction| {
//!     match interaction.kind {
//!         InteractionKind::Activated => {
//!             println!("notification {:?} was tapped", interaction.notification_id);
//!         }
//!         InteractionKind::Reply { text, .. } => println!("user replied: {text}"),
//!         _ => {}
//!     }
//! }).expect("failed to set interaction handler");
//!
//! // Ask once at startup; platforms without a permission prompt report granted immediately.
//! robius_notifications::request_permission(|granted| {
//!     if !matches!(granted, Ok(true)) {
//!         return;
//!     }
//!     Notification::new()
//!         .set_id("new-message")
//!         .set_title("New message")
//!         .set_body("Hello from Robius!")
//!         .add_action(Action::button("mark-read", "Mark as read"))
//!         .add_action(Action::reply("reply", "Reply").set_placeholder("Type a reply…"))
//!         .add_metadata("conversation", "robius")
//!         .show()
//!         .expect("failed to show notification");
//! }).expect("failed to request notification permission");
//! ```

mod error;
mod sys;

// Compile-checks the README's code examples along with the doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, SystemTime},
};

pub use error::{Error, Result};

pub(crate) type PermissionCallback = Box<dyn FnOnce(Result<bool>) + Send + 'static>;
pub(crate) type SettingsCallback = Box<dyn FnOnce(Result<NotificationSettings>) + Send + 'static>;
pub(crate) type ActiveIdsCallback = Box<dyn FnOnce(Result<Vec<String>>) + Send + 'static>;
type InteractionHandler = Arc<dyn Fn(Interaction) + Send + Sync + 'static>;

/// A system notification builder.
#[derive(Clone, Debug, Default)]
pub struct Notification {
    options: NotificationOptions,
}

impl Notification {
    /// Creates a new notification builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets a stable identifier for this notification.
    ///
    /// Showing another notification with the same id replaces the earlier one.
    /// The id is also how [`cancel`] and [`Interaction::notification_id`]
    /// refer back to this notification.
    /// If you don't set an id, a unique one is generated at [`show`](Self::show) time.
    #[must_use]
    pub fn set_id(mut self, id: impl Into<String>) -> Self {
        self.options.id = id.into();
        self
    }

    /// Sets the notification's title, its most prominent line of text.
    #[must_use]
    pub fn set_title(mut self, title: impl Into<String>) -> Self {
        self.options.title = Some(title.into());
        self
    }

    /// Sets the notification's main body text.
    #[must_use]
    pub fn set_body(mut self, body: impl Into<String>) -> Self {
        self.options.body = Some(body.into());
        self
    }

    /// Sets a secondary line of text shown near the title, where supported.
    ///
    /// iOS/macOS show this as the subtitle, Android as the sub-text,
    /// and Windows as an extra line. Linux appends it to the body.
    #[must_use]
    pub fn set_subtitle(mut self, subtitle: impl Into<String>) -> Self {
        self.options.subtitle = Some(subtitle.into());
        self
    }

    /// Sets the channel (notification category) this notification belongs to.
    ///
    /// This primarily matters on Android, where every notification belongs to a
    /// channel that users can manage in system settings; the channel is created
    /// on first use. Without one, notifications go to a default "Notifications"
    /// channel. Other platforms mostly ignore this.
    #[doc(alias = "set_category")]
    #[must_use]
    pub fn set_channel(mut self, channel: NotificationChannel) -> Self {
        self.options.channel = Some(channel);
        self
    }

    /// Sets how prominently this notification should interrupt the user.
    ///
    /// On iOS/macOS, [`Urgency::Critical`] maps to the time-sensitive
    /// interruption level, which only takes full effect if the app has the
    /// `com.apple.developer.usernotifications.time-sensitive` entitlement
    /// in its entitlements file; the system quietly downgrades it otherwise.
    #[must_use]
    pub fn set_urgency(mut self, urgency: Urgency) -> Self {
        self.options.urgency = Some(urgency);
        self
    }

    /// Sets the sound played when this notification is shown.
    ///
    /// The platform default sound is used if not set.
    #[must_use]
    pub fn set_sound(mut self, sound: Sound) -> Self {
        self.options.sound = Some(sound);
        self
    }

    /// Sets the number shown on the app's icon badge (iOS/macOS),
    /// or the notification count number (Android). Other platforms ignore it.
    #[must_use]
    pub fn set_badge_count(mut self, count: u32) -> Self {
        self.options.badge_count = Some(count);
        self
    }

    /// Sets a group/thread id used to visually group related notifications
    /// together (e.g., all messages in one conversation), where supported.
    /// This is iOS/macOS's "thread identifier"; it is not a notification
    /// category (for that, see [`set_channel`](Self::set_channel)).
    #[doc(alias = "thread")]
    #[must_use]
    pub fn set_group(mut self, group: impl Into<String>) -> Self {
        self.options.group = Some(group.into());
        self
    }

    /// Attaches an image from a filesystem path, shown alongside or below
    /// the notification text, where supported.
    #[must_use]
    pub fn set_image<P: AsRef<Path>>(mut self, path: P) -> Self {
        self.options.image = Some(path.as_ref().to_owned());
        self
    }

    /// Sets how long the notification stays on screen before auto-dismissing,
    /// on platforms that support an explicit timeout (Linux, Windows).
    /// Others let the OS decide.
    #[must_use]
    pub fn set_timeout(mut self, timeout: Duration) -> Self {
        self.options.timeout = Some(timeout);
        self
    }

    /// Attaches an app-defined key-value pair to this notification.
    ///
    /// Metadata isn't shown to the user; it comes back to you in
    /// [`Interaction::metadata`] so you can route the interaction, e.g.,
    /// which conversation to open when a message notification is tapped.
    #[must_use]
    pub fn add_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.options.metadata.push((key.into(), value.into()));
        self
    }

    /// Adds an interactive action (a button or quick-reply field) to this notification.
    ///
    /// Platforms limit how many actions are actually shown
    /// (Android: 3, iOS: 4, Windows: 5), so put the important ones first.
    #[must_use]
    pub fn add_action(mut self, action: Action) -> Self {
        self.options.actions.push(action);
        self
    }

    /// Associates this notification with an ongoing [`Conversation`].
    ///
    /// On Android 11 and newer, this gives the notification the platform's full
    /// conversation treatment: it appears in the dedicated "Conversations"
    /// section of the shade, and the user gets per-conversation settings
    /// (priority, silent, bubble). You can read the priority/urgency/sound
    /// state back with [`notification_settings`] and open those settings
    /// with [`open_notification_settings`].
    /// On iOS/macOS, the conversation groups notifications like
    /// [`set_group`](Self::set_group) does (an explicit `set_group` wins).
    /// With the `apple-communication` cargo feature enabled, it additionally
    /// gets Apple's communication-notification treatment on iOS 15+/macOS 12+:
    /// the conversation icon as the sender's (or group's) avatar, with the
    /// donated intents feeding Siri suggestions and Focus. (Focus's
    /// "allowed people" breakthrough matches the user's contacts, which needs
    /// contact details this API doesn't carry yet, so that part doesn't apply.)
    /// **That requires the app to hold the
    /// `com.apple.developer.usernotifications.communication` entitlement AND
    /// list `INSendMessageIntent` in the `NSUserActivityTypes` array of its
    /// `Info.plist`**; otherwise (or on older systems) it falls back to the
    /// plain rendering.
    /// Windows and Linux have no conversation concept and mostly ignore this.
    #[must_use]
    pub fn set_conversation(mut self, conversation: Conversation) -> Self {
        self.options.conversation = Some(conversation);
        self
    }

    /// Shows a progress bar on the notification, e.g., for a download.
    ///
    /// Use [`update_progress`] to advance it in place without re-alerting the
    /// user. Android and Windows render a real progress bar; some Linux
    /// daemons show it (the `value` hint); iOS/macOS have no notification
    /// progress concept, so it's a no-op there.
    #[must_use]
    pub fn set_progress(mut self, progress: Progress) -> Self {
        self.options.progress = Some(progress);
        self
    }

    /// Sets the event time this notification is about, shown in place of the
    /// time it was posted (Android). Other platforms always show the delivery
    /// time, so this is a no-op there. Unset = the platform default (posting time).
    #[must_use]
    pub fn set_timestamp(mut self, timestamp: SystemTime) -> Self {
        self.options.timestamp = Some(timestamp);
        self
    }

    /// Marks this notification as persistent/ongoing, where supported.
    ///
    /// * **Android**: an "ongoing" notification the user can't swipe away
    ///   (Android 14 lets users dismiss most of them anyway).
    /// * **Windows**: the toast stays on screen until acted on
    ///   (reminder-style) — but only when the notification has at least one
    ///   action button; Windows ignores the reminder mode otherwise.
    /// * **Linux**: the notification never auto-expires.
    /// * **iOS/macOS**: no such concept (banners are always transient); no-op.
    ///
    /// Off by default, matching every platform's default.
    #[must_use]
    pub fn set_persistent(mut self, persistent: bool) -> Self {
        self.options.persistent = persistent;
        self
    }

    /// Sets how much of this notification appears on the lock screen (Android).
    ///
    /// Unset = the platform default: the user's own lock-screen notification
    /// setting decides. The other platforms manage lock-screen privacy
    /// entirely at the OS level, so this is a no-op there.
    #[must_use]
    pub fn set_lock_screen_visibility(mut self, visibility: LockScreenVisibility) -> Self {
        self.options.lock_screen_visibility = Some(visibility);
        self
    }

    /// Asks for this notification to break through Do-Not-Disturb / Focus
    /// modes, where supported. Off by default (notifications respect
    /// Do-Not-Disturb everywhere by default).
    ///
    /// * **iOS/macOS**: delivered as a critical alert. **Requires the
    ///   Apple-granted `com.apple.developer.usernotifications.critical-alerts`
    ///   entitlement in the app**, and [`set_uses_critical_alerts`]`(true)`
    ///   must be called before [`request_permission`]; without both, the
    ///   system silently downgrades it to a normal notification.
    /// * **Android**: applied to the notification's channel when the channel
    ///   is first created (like importance, only the user can change it
    ///   afterwards). **It only takes effect if the user has granted the app
    ///   Do-Not-Disturb access** (Settings → Notifications → Do Not Disturb
    ///   access); declare `android.permission.ACCESS_NOTIFICATION_POLICY` in
    ///   the manifest so the app appears in that settings list. Without the
    ///   grant, the flag is silently ignored.
    /// * **Windows/Linux**: no per-notification override exists; no-op
    ///   (use [`Urgency::Critical`] for the closest behavior).
    #[must_use]
    pub fn set_bypass_do_not_disturb(mut self, bypass: bool) -> Self {
        self.options.bypass_dnd = bypass;
        self
    }

    /// Schedules this notification to be shown later instead of immediately.
    ///
    /// * **iOS/macOS/Windows**: scheduled by the OS, so it fires even if the
    ///   app has exited by then. (On Windows, interactions with a scheduled
    ///   toast are only delivered if the app is running when it fires.)
    /// * **Android/Linux**: scheduled by an in-process timer, so it only
    ///   fires while the app is still running.
    ///
    /// A time in the past shows the notification immediately. [`cancel`] with
    /// this notification's id also cancels a still-pending scheduled showing.
    #[must_use]
    pub fn set_scheduled_time(mut self, time: SystemTime) -> Self {
        self.options.scheduled_time = Some(time);
        self
    }

    /// Shows this notification.
    ///
    /// A successful return means the notification was handed off to the OS,
    /// not that it was displayed; see the crate-level docs on Completion.
    pub fn show(self) -> Result<()> {
        let mut options = self.options;
        options.validate()?;
        if options.id.is_empty() {
            options.id = generated_id();
        }

        // A past (or immediate) scheduled time just means "now".
        if let Some(time) = options.scheduled_time {
            if time.duration_since(SystemTime::now()).unwrap_or(Duration::ZERO)
                < Duration::from_millis(50)
            {
                options.scheduled_time = None;
            } else if sys::NATIVE_SCHEDULING {
                // The OS shows it later. Drop any stale progress entry from an
                // earlier same-id show: a scheduled notification can't be
                // progress-updated until it's showing (see [`update_progress`]).
                progress_cache().lock().unwrap().remove(&options.id);
                return sys::show(options);
            } else {
                // No OS-side scheduling here: an in-process timer fires it
                // later (and dies with the process; see set_scheduled_time).
                return schedule_fallback(options);
            }
        }

        // An immediate show supersedes any still-pending scheduled showing of this id.
        fallback_scheduled().lock().unwrap().remove(&options.id);
        show_now(options)
    }
}

/// An interactive element on a notification: a button or a quick-reply field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Action {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) kind: ActionKind,
    pub(crate) placeholder: Option<String>,
    pub(crate) destructive: bool,
    pub(crate) foreground: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ActionKind {
    Button,
    Reply,
}

impl Action {
    /// Creates a plain button action.
    ///
    /// The `id` is how [`InteractionKind::Action`] refers back to this action;
    /// the `title` is the button label the user sees.
    pub fn button(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            kind: ActionKind::Button,
            placeholder: None,
            destructive: false,
            foreground: false,
        }
    }

    /// Creates a quick-reply action: a button that opens an inline text input.
    ///
    /// The submitted text arrives as [`InteractionKind::Reply`]. On platforms
    /// without inline text input in notifications, this degrades to a plain
    /// button that arrives as [`InteractionKind::Action`].
    pub fn reply(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            kind: ActionKind::Reply,
            placeholder: None,
            destructive: false,
            foreground: false,
        }
    }

    /// Sets the placeholder text shown in an empty quick-reply input, where supported.
    #[must_use]
    pub fn set_placeholder(mut self, placeholder: impl Into<String>) -> Self {
        self.placeholder = Some(placeholder.into());
        self
    }

    /// Marks this action as destructive (e.g., "Delete"), where supported.
    /// iOS/macOS show destructive actions in red.
    #[must_use]
    pub fn set_destructive(mut self, destructive: bool) -> Self {
        self.destructive = destructive;
        self
    }

    /// Requests that pressing this action also brings the app to the foreground,
    /// where supported (iOS/macOS). By default, actions are handled without
    /// opening the app.
    #[must_use]
    pub fn set_foreground(mut self, foreground: bool) -> Self {
        self.foreground = foreground;
        self
    }
}

/// An ongoing conversation (a direct message or group chat with one or more
/// people) that notifications can belong to, via [`Notification::set_conversation`].
///
/// "Conversation" is the platforms' own term for this: Android shows these in
/// a dedicated "Conversations" section with per-conversation user settings
/// (Android 11+), and iOS/macOS model conversations in their communication
/// notification APIs. The `id` should be a stable identifier for the chat
/// (e.g., your app's chat/room/thread id), so that repeated notifications for
/// the same chat share one conversation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conversation {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) icon: Option<PathBuf>,
    pub(crate) group_conversation: bool,
}

impl Conversation {
    /// Creates a conversation with the given stable `id` and user-visible `name`.
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            icon: None,
            group_conversation: false,
        }
    }

    /// Sets the conversation's avatar/icon image from a filesystem path, where supported.
    #[must_use]
    pub fn set_icon<P: AsRef<Path>>(mut self, path: P) -> Self {
        self.icon = Some(path.as_ref().to_owned());
        self
    }

    /// Marks this as a group conversation (multiple people) rather than a 1:1 chat.
    #[must_use]
    pub fn set_group_conversation(mut self, group_conversation: bool) -> Self {
        self.group_conversation = group_conversation;
        self
    }
}

/// Progress shown on a notification, e.g., for a download or upload.
///
/// Set it with [`Notification::set_progress`], then advance it in place with
/// [`update_progress`]. Progress notifications alert the user only when first
/// shown, not on every update, matching the platforms' own behavior.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Progress {
    /// A busy indicator with no specific completion amount.
    Indeterminate,
    /// A bar filled to `current` out of `total`.
    /// A `current` greater than `total` is treated as complete.
    Determinate {
        /// How much is done so far.
        current: u32,
        /// The total amount of work; must be non-zero.
        total: u32,
    },
}

/// How much of a notification appears on the device's lock screen (Android).
///
/// Used with [`Notification::set_lock_screen_visibility`]; when unset, the
/// platform default applies (the user's own lock-screen setting decides).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockScreenVisibility {
    /// The full content is shown on the lock screen.
    Public,
    /// The notification's presence is shown, but the system redacts its content.
    Private,
    /// The notification doesn't appear on the lock screen at all.
    Secret,
}

/// One message of a conversation's accumulated history (see
/// [`Notification::set_conversation`]); only rendered on Android.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConversationMessage {
    pub(crate) sender: String,
    pub(crate) text: String,
    /// Milliseconds since the Unix epoch.
    pub(crate) timestamp_ms: u64,
}

/// A user-manageable category of notifications, aka an Android notification channel.
///
/// On Android, every notification belongs to a channel — shown to users as a
/// notification "category" in system settings — and users can tune or disable
/// each one separately. The channel is created the first time it's used; its
/// name and importance are user-visible. Related channels can be gathered
/// under a titled section via [`set_group`](Self::set_group).
/// Other platforms currently ignore channels.
#[doc(alias("category", "categories"))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotificationChannel {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) description: Option<String>,
    pub(crate) importance: Urgency,
    /// A `(group id, group name)` pair; see [`NotificationChannel::set_group`].
    pub(crate) group: Option<(String, String)>,
}

impl NotificationChannel {
    /// Creates a new channel with the given stable `id` and user-visible `name`.
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: None,
            importance: Urgency::Normal,
            group: None,
        }
    }

    /// Puts this channel in a user-visible group: a titled section that
    /// related channels (notification categories) appear under in the
    /// system settings UI (Android). Other platforms ignore this.
    #[doc(alias("category_group", "section"))]
    #[must_use]
    pub fn set_group(mut self, group_id: impl Into<String>, group_name: impl Into<String>) -> Self {
        self.group = Some((group_id.into(), group_name.into()));
        self
    }

    /// Sets the user-visible description of what this channel is for.
    #[must_use]
    pub fn set_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Sets the importance of all notifications in this channel.
    ///
    /// Note: Android only applies this when the channel is first created;
    /// after that, only the user can change it (in system settings).
    #[must_use]
    pub fn set_importance(mut self, importance: Urgency) -> Self {
        self.importance = importance;
        self
    }
}

/// How prominently a notification should interrupt the user.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Urgency {
    /// Delivered quietly: no popup banner, no sound.
    Low,
    /// The platform's regular notification behavior.
    #[default]
    Normal,
    /// Time-sensitive: pops up over other content and stays visible longer,
    /// where supported.
    Critical,
}

/// The sound played when a notification is shown.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Sound {
    /// The platform's default notification sound.
    #[default]
    Default,
    /// No sound.
    Silent,
    /// A named sound, resolved in a platform-specific way: a sound file name
    /// bundled with the app (iOS/macOS), a system sound name (macOS, e.g.,
    /// "Ping"), a freedesktop sound-theme name (Linux), or a `ms-winsoundevent`
    /// name (Windows). Platforms fall back to the default sound if the name
    /// can't be resolved.
    Named(String),
}

/// A user interaction with a previously shown notification,
/// delivered to the handler registered via [`set_interaction_handler`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Interaction {
    /// The id of the notification that was interacted with,
    /// as set (or generated) by [`Notification::set_id`].
    pub notification_id: String,
    /// What the user did.
    pub kind: InteractionKind,
    /// The metadata that was attached to the notification
    /// via [`Notification::add_metadata`].
    pub metadata: Vec<(String, String)>,
}

/// The ways a user can interact with a notification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InteractionKind {
    /// The user tapped/clicked the notification body itself.
    Activated,
    /// The user dismissed the notification without acting on it,
    /// on platforms that report dismissals.
    Dismissed,
    /// The user pressed one of the notification's action buttons.
    Action {
        /// The id of the [`Action`] that was pressed.
        id: String,
    },
    /// The user submitted text through a quick-reply action.
    Reply {
        /// The id of the quick-reply [`Action`].
        action_id: String,
        /// The text the user submitted.
        text: String,
    },
    /// The user asked to see this app's own notification settings screen from
    /// the OS settings UI (iOS/macOS; see [`set_provides_notification_settings`]).
    ///
    /// [`Interaction::notification_id`] is empty unless the user got there
    /// from one specific notification.
    OpenSettings,
}

/// Registers the handler called whenever the user interacts with one of this
/// app's notifications.
///
/// Call this once, as early as possible during app startup, so that
/// interactions that launched your app still reach the handler; see the
/// crate-level docs. Interactions that arrive before any handler is set are
/// queued and delivered once one is set.
///
/// The handler may run on any thread; use a channel or similar to communicate
/// with your app's UI thread.
///
/// Returns [`Error::HandlerAlreadySet`] if a handler was already registered.
pub fn set_interaction_handler<F>(handler: F) -> Result<()>
where
    F: Fn(Interaction) + Send + Sync + 'static,
{
    if matches!(&*handler_state().lock().unwrap(), HandlerState::Set(_)) {
        return Err(Error::HandlerAlreadySet);
    }

    // Let the backend set up its listener (delegate, receiver, etc.) first;
    // if that fails, no handler is installed at all.
    sys::init_interaction_listener()?;

    // Install the handler and take the queue in one critical section, so a
    // concurrent caller can't clobber an already-installed handler.
    let (handler, pending) = {
        let mut state = handler_state().lock().unwrap();
        match &mut *state {
            HandlerState::Set(_) => return Err(Error::HandlerAlreadySet),
            HandlerState::Pending(pending) => {
                let pending = std::mem::take(pending);
                let handler: InteractionHandler = Arc::new(handler);
                *state = HandlerState::Set(handler.clone());
                (handler, pending)
            }
        }
    };

    // Deliver anything queued before the handler existed, without holding the lock.
    for interaction in pending {
        handler(interaction);
    }

    Ok(())
}

/// Asks the user for permission to show notifications.
///
/// The callback receives `Ok(true)` if permission was granted (or isn't needed
/// on the current platform), and `Ok(false)` if the user or system denied it.
/// The callback may be invoked from any thread, before or after this returns.
///
/// * **Android 13+**: shows the system permission prompt; your app manifest
///   must declare `android.permission.POST_NOTIFICATIONS`. Android 12 and
///   older report `Ok(true)` unless the user disabled the app's notifications.
/// * **iOS/macOS**: shows the system permission prompt (once; afterwards it
///   just reports the user's standing decision).
/// * **Windows**: no permission prompt exists; reports whether toast
///   notifications are currently enabled for this app.
/// * **Linux**: no permission concept at all; always reports `Ok(true)`.
pub fn request_permission<F>(on_result: F) -> Result<()>
where
    F: FnOnce(Result<bool>) + Send + 'static,
{
    sys::request_permission(Box::new(on_result), false)
}

/// Like [`request_permission`], but never shows the user a prompt.
///
/// * **iOS/macOS**: requests provisional authorization: no prompt appears,
///   and notifications are delivered quietly (straight to Notification
///   Center, no banner or sound) until the user upgrades or disables them
///   from there. Reports `Ok(true)` immediately.
/// * **Android**: just reports the current permission state, without
///   prompting (Android has no quiet-delivery permission).
/// * **Windows/Linux**: identical to [`request_permission`], which never
///   prompts on these platforms anyway.
pub fn request_provisional_permission<F>(on_result: F) -> Result<()>
where
    F: FnOnce(Result<bool>) + Send + 'static,
{
    sys::request_permission(Box::new(on_result), true)
}

/// Updates the progress bar of an already-shown notification, in place and
/// without re-alerting the user.
///
/// The notification must have been shown with
/// [`Notification::set_progress`] during this app run; otherwise this
/// returns [`Error::InvalidNotification`]. On platforms that don't render
/// progress (iOS/macOS), this is a no-op.
///
/// If the user has already dismissed the notification, the update is
/// dropped (Android) or lands quietly in the notification center (Windows) —
/// it never re-alerts. A notification still pending via
/// [`Notification::set_scheduled_time`] can't be updated until it has
/// actually been shown.
pub fn update_progress(id: &str, progress: Progress) -> Result<()> {
    if id.is_empty() || matches!(progress, Progress::Determinate { total: 0, .. }) {
        return Err(Error::InvalidNotification);
    }
    let options = {
        let mut cache = progress_cache().lock().unwrap();
        let options = cache.get_mut(id).ok_or(Error::InvalidNotification)?;
        options.progress = Some(progress);
        options.clone()
    };
    sys::update_progress(&options)
}

/// Asks the OS which of this app's notifications are still showing (in the
/// system tray / notification center), reporting their ids to the callback.
/// The callback may be invoked from any thread, before or after this returns.
///
/// Platform notes: Android and iOS/macOS report all of the app's delivered,
/// still-visible notifications. Windows and Linux can only report
/// notifications shown by this run of the app. Notifications scheduled for
/// later (via [`Notification::set_scheduled_time`]) are not included.
pub fn active_notification_ids<F>(on_result: F) -> Result<()>
where
    F: FnOnce(Result<Vec<String>>) + Send + 'static,
{
    sys::active_notification_ids(Box::new(on_result))
}

/// What a notification-settings query or settings screen should be scoped to.
///
/// Only Android has real per-channel and per-conversation settings; the other
/// platforms treat every scope as [`SettingsScope::App`]. See
/// [`notification_settings`] and [`open_notification_settings`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SettingsScope {
    /// The app's overall notification settings.
    App,
    /// One [`NotificationChannel`]'s settings.
    Channel {
        /// The [`NotificationChannel`] id.
        channel_id: String,
    },
    /// One [`Conversation`]'s settings within a channel (Android 11+).
    Conversation {
        /// The id of the [`NotificationChannel`] the conversation's
        /// notifications were shown under.
        channel_id: String,
        /// The [`Conversation`] id.
        conversation_id: String,
    },
}

impl SettingsScope {
    fn validate(&self) -> Result<()> {
        let valid = match self {
            SettingsScope::App => true,
            SettingsScope::Channel { channel_id } => !channel_id.trim().is_empty(),
            SettingsScope::Conversation { channel_id, conversation_id } => {
                !channel_id.trim().is_empty() && !conversation_id.trim().is_empty()
            }
        };
        if valid {
            Ok(())
        } else {
            Err(Error::InvalidNotification)
        }
    }
}

/// A snapshot of the user's notification settings, as far as the current
/// platform reports them back to apps.
///
/// `enabled` is known everywhere (except Linux, which can only report whether
/// a notification service exists); every other field is `None` on platforms
/// that don't expose it. Android reports the most: per-channel and
/// per-conversation urgency, sound, badge, and user-customization state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotificationSettings {
    /// Whether notifications in the requested scope can be shown at all.
    pub enabled: bool,
    /// The user's effective urgency for the channel/conversation (Android).
    pub urgency: Option<Urgency>,
    /// Whether sound is allowed.
    pub sound_enabled: Option<bool>,
    /// Whether badges are allowed.
    pub badge_enabled: Option<bool>,
    /// Whether the user themselves changed these settings, as opposed to the
    /// values the app created the channel with (Android 10+).
    pub customized_by_user: Option<bool>,
    /// Whether the user marked this conversation as a priority
    /// conversation (Android 11+, [`SettingsScope::Conversation`] only).
    pub priority_conversation: Option<bool>,
}

/// Asks the OS what the user's notification settings currently are,
/// for the given `scope`.
///
/// The callback may be invoked from any thread, before or after this returns.
///
/// * **Android**: reports real per-channel/per-conversation settings the user
///   picked in system settings. A [`SettingsScope::Channel`] for a channel
///   that was never created reports app-level settings.
/// * **iOS/macOS/Windows**: every scope reports the app-level settings.
/// * **Linux**: `enabled` just reflects whether a notification service is
///   reachable; nothing else is reported.
pub fn notification_settings<F>(scope: SettingsScope, on_result: F) -> Result<()>
where
    F: FnOnce(Result<NotificationSettings>) + Send + 'static,
{
    scope.validate()?;
    sys::notification_settings(scope, Box::new(on_result))
}

/// Opens the OS's notification settings UI for this app, at the given `scope`.
///
/// * **Android**: opens the app's notification settings, one channel's
///   settings, or one conversation's settings (Android 11+; older versions
///   fall back to the channel).
/// * **iOS**: opens the app's page in the Settings app (every scope).
/// * **macOS/Windows**: opens the system notification settings (every scope),
///   where the user can find the app; macOS support is best-effort.
/// * **Linux**: no standard way to open notification settings; returns
///   [`Error::Unsupported`].
pub fn open_notification_settings(scope: SettingsScope) -> Result<()> {
    scope.validate()?;
    sys::open_notification_settings(scope)
}

/// Declares whether this app has its own in-app notification settings screen.
/// Call this before [`request_permission`].
///
/// On iOS/macOS, the system then offers a link to the app from its
/// notification settings UI; the user choosing it is delivered to your
/// interaction handler as [`InteractionKind::OpenSettings`], and your app
/// should navigate to its notification settings screen. Other platforms
/// ignore this.
pub fn set_provides_notification_settings(provides: bool) {
    PROVIDES_SETTINGS_UI.store(provides, Ordering::Relaxed);
}

static PROVIDES_SETTINGS_UI: AtomicBool = AtomicBool::new(false);

/// Whether the app declared an in-app notification settings screen.
#[cfg_attr(not(target_vendor = "apple"), allow(dead_code))]
pub(crate) fn provides_notification_settings() -> bool {
    PROVIDES_SETTINGS_UI.load(Ordering::Relaxed)
}

/// Declares that this app uses critical alerts, i.e., notifications that
/// break through Do-Not-Disturb via
/// [`Notification::set_bypass_do_not_disturb`]. Call this before
/// [`request_permission`]. Off by default.
///
/// This only matters on iOS/macOS, where critical-alert authorization must be
/// requested up front, **and the app must hold the Apple-granted
/// `com.apple.developer.usernotifications.critical-alerts` entitlement**
/// (requested from Apple, then added to the app's entitlements file);
/// without it, the authorization request and the alerts are silently
/// downgraded. Other platforms ignore this.
pub fn set_uses_critical_alerts(uses: bool) {
    USES_CRITICAL_ALERTS.store(uses, Ordering::Relaxed);
}

static USES_CRITICAL_ALERTS: AtomicBool = AtomicBool::new(false);

/// Whether the app declared that it uses critical alerts.
#[cfg_attr(not(target_vendor = "apple"), allow(dead_code))]
pub(crate) fn uses_critical_alerts() -> bool {
    USES_CRITICAL_ALERTS.load(Ordering::Relaxed)
}

/// Removes a previously shown notification from the system tray /
/// notification center, by the id it was shown with.
///
/// Removing an id that's no longer (or was never) shown is not an error.
pub fn cancel(id: &str) -> Result<()> {
    if id.is_empty() {
        return Err(Error::InvalidNotification);
    }
    // Also kills a still-pending fallback-scheduled showing and drops
    // whatever we remembered for progress updates.
    fallback_scheduled().lock().unwrap().remove(id);
    progress_cache().lock().unwrap().remove(id);
    sys::cancel(id)
}

/// Removes all of this app's notifications from the system tray / notification center.
pub fn cancel_all() -> Result<()> {
    fallback_scheduled().lock().unwrap().clear();
    progress_cache().lock().unwrap().clear();
    sys::cancel_all()
}

/// Sets the app identity used when showing notifications, on platforms that
/// need one. Call this before showing any notification.
///
/// * **Windows**: the AppUserModelID (AUMID) the toasts are attributed to.
///   Packaged apps (MSIX) don't need this. For unpackaged apps, pass the AUMID
///   your installer registered (e.g., via a Start Menu shortcut); if you don't
///   set one, a built-in system AUMID is borrowed so toasts still show during
///   development, but they'll be attributed to "Windows PowerShell".
/// * **Linux**: the basename of your app's `.desktop` file, which notification
///   daemons use to look up your app's name and icon.
/// * **Android/iOS/macOS**: ignored; identity comes from the app package/bundle.
pub fn set_app_id(app_id: impl Into<String>) {
    *app_id_state().lock().unwrap() = Some(app_id.into());
}

/// Options collected by [`Notification`].
#[derive(Clone, Debug, Default)]
pub(crate) struct NotificationOptions {
    /// Empty until explicitly set; [`Notification::show`] fills in a generated
    /// id, so backends can rely on this being non-empty.
    pub(crate) id: String,
    pub(crate) title: Option<String>,
    pub(crate) body: Option<String>,
    pub(crate) subtitle: Option<String>,
    pub(crate) channel: Option<NotificationChannel>,
    pub(crate) urgency: Option<Urgency>,
    pub(crate) sound: Option<Sound>,
    pub(crate) badge_count: Option<u32>,
    pub(crate) group: Option<String>,
    pub(crate) image: Option<PathBuf>,
    pub(crate) timeout: Option<Duration>,
    pub(crate) metadata: Vec<(String, String)>,
    pub(crate) actions: Vec<Action>,
    pub(crate) conversation: Option<Conversation>,
    /// The conversation's recent messages, filled in by `show()` from the
    /// process-wide history whenever `conversation` is set.
    pub(crate) conversation_messages: Vec<ConversationMessage>,
    pub(crate) progress: Option<Progress>,
    pub(crate) timestamp: Option<SystemTime>,
    pub(crate) persistent: bool,
    pub(crate) lock_screen_visibility: Option<LockScreenVisibility>,
    pub(crate) bypass_dnd: bool,
    pub(crate) scheduled_time: Option<SystemTime>,
}

impl NotificationOptions {
    fn validate(&self) -> Result<()> {
        fn is_empty_or_unset(text: &Option<String>) -> bool {
            match text.as_deref() {
                Some(text) => text.trim().is_empty(),
                None => true,
            }
        }

        if is_empty_or_unset(&self.title) && is_empty_or_unset(&self.body) {
            return Err(Error::Empty);
        }

        for action in &self.actions {
            if action.id.trim().is_empty() || action.title.trim().is_empty() {
                return Err(Error::InvalidNotification);
            }
            let duplicates = self
                .actions
                .iter()
                .filter(|other| other.id == action.id)
                .count();
            if duplicates > 1 {
                return Err(Error::InvalidNotification);
            }
        }

        for (key, _value) in &self.metadata {
            if key.trim().is_empty() {
                return Err(Error::InvalidNotification);
            }
        }

        if let Some(channel) = &self.channel {
            if channel.id.trim().is_empty() || channel.name.trim().is_empty() {
                return Err(Error::InvalidNotification);
            }
            if let Some((group_id, group_name)) = &channel.group {
                if group_id.trim().is_empty() || group_name.trim().is_empty() {
                    return Err(Error::InvalidNotification);
                }
            }
        }

        if let Some(conversation) = &self.conversation {
            if conversation.id.trim().is_empty() || conversation.name.trim().is_empty() {
                return Err(Error::InvalidNotification);
            }
            if conversation
                .icon
                .as_deref()
                .is_some_and(|path| path.as_os_str().is_empty())
            {
                return Err(Error::InvalidNotification);
            }
        }

        if self.image.as_deref().is_some_and(|path| path.as_os_str().is_empty()) {
            return Err(Error::InvalidNotification);
        }

        if let Some(Progress::Determinate { total: 0, .. }) = self.progress {
            return Err(Error::InvalidNotification);
        }

        Ok(())
    }
}

/// Generates a process-unique notification id for notifications without one.
fn generated_id() -> String {
    static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
    format!(
        "robius-notification-{}-{}",
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed),
    )
}

enum HandlerState {
    /// Interactions that arrived before any handler was set.
    Pending(Vec<Interaction>),
    Set(InteractionHandler),
}

fn handler_state() -> &'static Mutex<HandlerState> {
    static HANDLER_STATE: Mutex<HandlerState> = Mutex::new(HandlerState::Pending(Vec::new()));
    &HANDLER_STATE
}

fn app_id_state() -> &'static Mutex<Option<String>> {
    static APP_ID: Mutex<Option<String>> = Mutex::new(None);
    &APP_ID
}

/// The last-shown options of progress notifications, kept so
/// [`update_progress`] can re-render everything with just the bar moved.
fn progress_cache() -> &'static Mutex<HashMap<String, NotificationOptions>> {
    static CACHE: OnceLock<Mutex<HashMap<String, NotificationOptions>>> = OnceLock::new();
    CACHE.get_or_init(Mutex::default)
}

fn remember_progress(options: &NotificationOptions) {
    let mut cache = progress_cache().lock().unwrap();
    if options.progress.is_some() {
        cache.insert(options.id.clone(), options.clone());
    } else {
        // Re-showing an id without progress means it's no longer a progress notification.
        cache.remove(&options.id);
    }
}

/// Shows a notification right now: renders the conversation-history snapshot,
/// hands the notification to the backend, and commits the bookkeeping
/// (progress cache, shared history) only if the OS actually took it.
fn show_now(mut options: NotificationOptions) -> Result<()> {
    let pending_message = prepare_conversation_history(&mut options);
    let bookkeeping = options.clone();
    let result = sys::show(options);
    if result.is_ok() {
        remember_progress(&bookkeeping);
        commit_conversation_message(pending_message);
    }
    result
}

/// How many recent messages a conversation notification shows (Android).
const CONVERSATION_HISTORY_LIMIT: usize = 8;

/// Builds the message this notification adds to its conversation, filling
/// `options.conversation_messages` with the history plus it — WITHOUT
/// committing to the shared history yet (that happens once it's shown,
/// so failed or cancelled showings never pollute later notifications).
fn prepare_conversation_history(
    options: &mut NotificationOptions,
) -> Option<(String, ConversationMessage)> {
    let conversation = options.conversation.as_ref()?;
    // No body = nothing readable to accumulate (e.g. a bare title-only ping).
    let text = match options.body.as_deref().map(str::trim) {
        Some(text) if !text.is_empty() => text.to_owned(),
        _ => return None,
    };
    let sender = options
        .title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .unwrap_or(&conversation.name)
        .to_owned();
    let timestamp_ms = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis() as u64;
    let message = ConversationMessage { sender, text, timestamp_ms };

    let mut snapshot = conversation_histories()
        .lock()
        .unwrap()
        .get(&conversation.id)
        .cloned()
        .unwrap_or_default();
    snapshot.push(message.clone());
    if snapshot.len() > CONVERSATION_HISTORY_LIMIT {
        let excess = snapshot.len() - CONVERSATION_HISTORY_LIMIT;
        snapshot.drain(..excess);
    }
    options.conversation_messages = snapshot;
    Some((conversation.id.clone(), message))
}

/// Commits a shown notification's message to its conversation's shared history.
fn commit_conversation_message(pending: Option<(String, ConversationMessage)>) {
    let Some((conversation_id, message)) = pending else {
        return;
    };
    let mut histories = conversation_histories().lock().unwrap();
    let history = histories.entry(conversation_id).or_default();
    history.push(message);
    if history.len() > CONVERSATION_HISTORY_LIMIT {
        let excess = history.len() - CONVERSATION_HISTORY_LIMIT;
        history.drain(..excess);
    }
}

fn conversation_histories() -> &'static Mutex<HashMap<String, Vec<ConversationMessage>>> {
    static HISTORIES: OnceLock<Mutex<HashMap<String, Vec<ConversationMessage>>>> = OnceLock::new();
    HISTORIES.get_or_init(Mutex::default)
}

/// Pending fallback-scheduled notifications: id -> generation. A re-show or
/// cancel bumps/removes the entry, so the sleeping timer thread notices its
/// showing is stale and does nothing.
fn fallback_scheduled() -> &'static Mutex<HashMap<String, u64>> {
    static SCHEDULED: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();
    SCHEDULED.get_or_init(Mutex::default)
}

/// In-process scheduling for platforms without OS-side scheduling: one timer
/// thread per pending notification. Dies with the process, as documented.
fn schedule_fallback(mut options: NotificationOptions) -> Result<()> {
    static GENERATION: AtomicUsize = AtomicUsize::new(0);
    let generation = GENERATION.fetch_add(1, Ordering::Relaxed) as u64;
    let time = options.scheduled_time.take();
    let delay = time
        .and_then(|time| time.duration_since(SystemTime::now()).ok())
        .unwrap_or(Duration::ZERO);

    fallback_scheduled()
        .lock()
        .unwrap()
        .insert(options.id.clone(), generation);

    std::thread::Builder::new()
        .name("robius-notifications-timer".to_owned())
        .spawn(move || {
            std::thread::sleep(delay);
            // Hold the lock across the show, so a concurrent cancel() can't
            // slip in between our staleness check and the notification
            // actually appearing.
            let mut scheduled = fallback_scheduled().lock().unwrap();
            // Only fire if we're still the latest scheduled showing of this id.
            if scheduled.get(&options.id) != Some(&generation) {
                return;
            }
            scheduled.remove(&options.id);
            // Conversation history and progress bookkeeping happen at fire
            // time, inside show_now, so they reflect what actually displayed.
            let _ = show_now(options);
        })
        .map(|_| ())
        .map_err(Error::Io)
}

/// The app id set via [`set_app_id`], if any.
#[cfg_attr(not(any(target_os = "windows", target_os = "linux")), allow(dead_code))]
pub(crate) fn app_id() -> Option<String> {
    app_id_state().lock().unwrap().clone()
}

/// Called by the platform backends to hand a user interaction to the app,
/// or queue it up if the app hasn't registered its handler yet.
#[cfg_attr(target_family = "wasm", allow(dead_code))]
pub(crate) fn deliver_interaction(interaction: Interaction) {
    let handler = {
        let mut state = handler_state().lock().unwrap();
        match &mut *state {
            HandlerState::Pending(pending) => {
                pending.push(interaction);
                return;
            }
            HandlerState::Set(handler) => handler.clone(),
        }
    };
    // Run the app's handler outside the lock, in case it shows another notification.
    handler(interaction);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_notification_is_invalid() {
        assert!(matches!(
            Notification::new().options.validate(),
            Err(Error::Empty)
        ));
        assert!(matches!(
            Notification::new().set_title("  ").set_body("").options.validate(),
            Err(Error::Empty)
        ));
    }

    #[test]
    fn title_or_body_alone_is_valid() {
        assert!(Notification::new().set_title("t").options.validate().is_ok());
        assert!(Notification::new().set_body("b").options.validate().is_ok());
    }

    #[test]
    fn empty_and_duplicate_action_ids_are_invalid() {
        let invalid_notifications = [
            Notification::new().set_title("t").add_action(Action::button("", "OK")),
            Notification::new().set_title("t").add_action(Action::button("ok", " ")),
            Notification::new()
                .set_title("t")
                .add_action(Action::button("ok", "OK"))
                .add_action(Action::reply("ok", "Reply")),
            Notification::new().set_title("t").add_metadata("", "value"),
            Notification::new()
                .set_title("t")
                .set_channel(NotificationChannel::new("", "Messages")),
            Notification::new()
                .set_title("t")
                .set_channel(NotificationChannel::new("messages", "Messages").set_group("", "Work")),
            Notification::new()
                .set_title("t")
                .set_conversation(Conversation::new("", "Chat")),
            Notification::new()
                .set_title("t")
                .set_conversation(Conversation::new("chat-1", " ")),
        ];

        for notification in invalid_notifications {
            assert!(matches!(
                notification.options.validate(),
                Err(Error::InvalidNotification)
            ));
        }
    }

    #[test]
    fn valid_builder_options_pass_validation() {
        let notification = Notification::new()
            .set_id("new-message")
            .set_title("New message")
            .set_body("Hello from Robius!")
            .set_subtitle("Robius")
            .set_channel(
                NotificationChannel::new("messages", "Messages")
                    .set_description("New chat messages")
                    .set_importance(Urgency::Critical)
                    .set_group("work", "Work account"),
            )
            .set_conversation(
                Conversation::new("chat-42", "Team chat")
                    .set_group_conversation(true),
            )
            .set_urgency(Urgency::Critical)
            .set_sound(Sound::Default)
            .set_badge_count(3)
            .set_group("conversation-42")
            .set_timeout(Duration::from_secs(10))
            .add_metadata("conversation", "42")
            .add_action(Action::button("mark-read", "Mark as read").set_destructive(true))
            .add_action(Action::reply("reply", "Reply").set_placeholder("Type a reply…"));

        assert!(notification.options.validate().is_ok());
    }

    #[test]
    fn settings_scopes_validate_their_ids() {
        assert!(SettingsScope::App.validate().is_ok());
        assert!(SettingsScope::Channel { channel_id: "messages".to_owned() }.validate().is_ok());
        assert!(SettingsScope::Conversation {
            channel_id: "messages".to_owned(),
            conversation_id: "chat-42".to_owned(),
        }
        .validate()
        .is_ok());

        let invalid_scopes = [
            SettingsScope::Channel { channel_id: " ".to_owned() },
            SettingsScope::Conversation {
                channel_id: "messages".to_owned(),
                conversation_id: "".to_owned(),
            },
            SettingsScope::Conversation {
                channel_id: "".to_owned(),
                conversation_id: "chat-42".to_owned(),
            },
        ];
        for scope in invalid_scopes {
            assert!(matches!(scope.validate(), Err(Error::InvalidNotification)));
        }
    }

    #[test]
    fn zero_total_progress_is_invalid() {
        assert!(matches!(
            Notification::new()
                .set_title("t")
                .set_progress(Progress::Determinate { current: 0, total: 0 })
                .options
                .validate(),
            Err(Error::InvalidNotification)
        ));
        assert!(Notification::new()
            .set_title("t")
            .set_progress(Progress::Indeterminate)
            .options
            .validate()
            .is_ok());
        assert!(matches!(
            update_progress("dl", Progress::Determinate { current: 1, total: 0 }),
            Err(Error::InvalidNotification)
        ));
        // Never shown with progress in this run: nothing to update.
        assert!(matches!(
            update_progress("never-shown-progress-id", Progress::Indeterminate),
            Err(Error::InvalidNotification)
        ));
    }

    #[test]
    fn conversation_history_accumulates_and_trims() {
        // prepare + commit = what a successful show does.
        let show = |n: u32| {
            let mut options = Notification::new()
                .set_title(format!("sender {n}"))
                .set_body(format!("message {n}"))
                .set_conversation(Conversation::new("history-test", "Chat"))
                .options;
            let pending = prepare_conversation_history(&mut options);
            commit_conversation_message(pending);
            options
        };

        let first = show(0);
        assert_eq!(first.conversation_messages.len(), 1);
        assert_eq!(first.conversation_messages[0].sender, "sender 0");
        assert_eq!(first.conversation_messages[0].text, "message 0");

        let last = (1..=20).map(show).last().unwrap();
        assert_eq!(last.conversation_messages.len(), CONVERSATION_HISTORY_LIMIT);
        // Oldest entries got trimmed; the newest is last.
        assert_eq!(last.conversation_messages.last().unwrap().text, "message 20");
        assert_eq!(
            last.conversation_messages.first().unwrap().text,
            format!("message {}", 21 - CONVERSATION_HISTORY_LIMIT),
        );

        // A body-less notification adds nothing to the history.
        let mut silent = Notification::new()
            .set_title("sender")
            .set_conversation(Conversation::new("history-test", "Chat"))
            .options;
        assert!(prepare_conversation_history(&mut silent).is_none());
        assert!(silent.conversation_messages.is_empty());

        // An uncommitted prepare (a failed/cancelled show) must not leak
        // into the shared history that later notifications render.
        let mut failed = Notification::new()
            .set_title("sender")
            .set_body("never shown")
            .set_conversation(Conversation::new("history-test", "Chat"))
            .options;
        let _uncommitted = prepare_conversation_history(&mut failed);
        let after = show(99);
        assert!(!after
            .conversation_messages
            .iter()
            .any(|message| message.text == "never shown"));
    }

    #[test]
    fn cancel_kills_a_pending_fallback_scheduled_showing() {
        let options = Notification::new()
            .set_id("scheduled-test")
            .set_title("t")
            .set_scheduled_time(SystemTime::now() + Duration::from_secs(600))
            .options;
        schedule_fallback(options).unwrap();
        assert!(fallback_scheduled().lock().unwrap().contains_key("scheduled-test"));

        // sys::cancel fails on unsupported/unbundled hosts; the pending
        // entry must be gone regardless.
        let _ = cancel("scheduled-test");
        assert!(!fallback_scheduled().lock().unwrap().contains_key("scheduled-test"));
    }

    #[test]
    fn generated_ids_are_unique() {
        assert_ne!(generated_id(), generated_id());
    }

    #[test]
    fn interactions_are_queued_until_a_handler_is_set() {
        let interaction = Interaction {
            notification_id: "queued".to_owned(),
            kind: InteractionKind::Action { id: "ok".to_owned() },
            metadata: vec![("k".to_owned(), "v".to_owned())],
        };
        deliver_interaction(interaction.clone());

        let HandlerState::Pending(pending) = &*handler_state().lock().unwrap() else {
            panic!("expected interactions to be queued while no handler is set");
        };
        assert_eq!(pending.last(), Some(&interaction));
    }
}
