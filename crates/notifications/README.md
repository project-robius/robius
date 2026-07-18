# robius-notifications

`robius-notifications` provides a Rust builder API for showing system
notifications from an app, and for handling how the user interacts with them:
tapping the notification, pressing one of its action buttons, submitting a
quick reply, or dismissing it.

```rust,no_run
use robius_notifications::{Action, Interaction, InteractionKind, Notification};

// Do this once, as early as possible at app startup.
robius_notifications::set_interaction_handler(|interaction: Interaction| {
    match interaction.kind {
        InteractionKind::Activated => {
            println!("notification {:?} was tapped", interaction.notification_id);
        }
        InteractionKind::Reply { text, .. } => println!("user replied: {text}"),
        _ => {}
    }
})?;

// Ask once at startup; platforms without a permission prompt report granted immediately.
robius_notifications::request_permission(|granted| {
    if !matches!(granted, Ok(true)) {
        return;
    }
    Notification::new()
        .set_id("new-message")
        .set_title("New message")
        .set_body("Hello from Robius!")
        .add_action(Action::button("mark-read", "Mark as read"))
        .add_action(Action::reply("reply", "Reply").set_placeholder("Type a reply…"))
        .add_metadata("conversation", "robius")
        .show()
        .expect("failed to show notification");
})?;
# Ok::<(), robius_notifications::Error>(())
```

The metadata you attach with `add_metadata` comes back on every interaction,
so you can route it (e.g., which conversation to open) without keeping your
own bookkeeping.

## Platform behavior

- Android posts notifications via `NotificationManager` with notification
  channels; actions use broadcast `PendingIntent`s and `RemoteInput` quick
  replies.
- iOS and macOS use `UNUserNotificationCenter` from the UserNotifications
  framework, with actions and quick replies via notification categories.
- Windows uses WinRT toast notifications (`ToastNotificationManager`), with
  action buttons and quick-reply text boxes in the toast XML.
- Linux uses the standard `org.freedesktop.Notifications` D-Bus service,
  which works on both X11 and Wayland.

A successful `show()` means the notification was handed off to the OS, not
that it was displayed: the OS may still suppress it (Do-Not-Disturb, missing
permission, etc.). On Android, a missing notification permission is reported
synchronously as `Error::PermissionDenied`.

## Terminology

Notification APIs use overlapping words for different things, so here's how
this crate's names map onto each platform's:

| This crate                       | What it is                                                    | Native name |
| -------------------------------- | ------------------------------------------------------------- | ----------- |
| `NotificationChannel` / `set_channel` | a user-manageable kind of notification                   | Android "notification channel", shown to users as a notification **"category"** in system settings |
| `NotificationChannel::set_group` | a titled **section of categories** in system settings (e.g., one per account) | Android "notification channel group" |
| `Notification::set_group`        | visual stacking of related notifications on screen            | iOS/macOS "thread identifier", Android "notification group" |
| `Conversation` / `set_conversation` | an ongoing chat with a person or group                     | Android "conversation" (the Conversations section + per-conversation settings), the conversation id in Apple's communication notifications |
| `Urgency`                        | how prominently a notification interrupts                     | Android channel "importance", Apple "interruption level", Linux "urgency" hint |

One caution: iOS's `UNNotificationCategory` is *not* the "category" above —
it's Apple's action-set descriptor, which this crate manages internally when
you `add_action`; it never appears in this API.

## Conversations

Attach a `Conversation` to give messaging notifications the platform's
conversation treatment:

```rust,no_run
use robius_notifications::{Conversation, Notification};

Notification::new()
    .set_id("chat-42-message-7")
    .set_title("Riley")
    .set_body("See you at 10?")
    .set_conversation(Conversation::new("chat-42", "Team chat").set_group_conversation(true))
    .show()?;
# Ok::<(), robius_notifications::Error>(())
```

- **Android 11+**: the notification appears in the dedicated Conversations
  section of the shade, styled as a message from the sender, and the user gets
  per-conversation settings (priority, silent, bubble). Android 9/10 get the
  message styling without the conversation section; Android 8 ignores it.
- **iOS/macOS**: the conversation id groups notifications like `set_group`
  does (an explicit `set_group` wins). Enable the **`apple-communication`**
  cargo feature to upgrade conversation notifications into Apple
  communication notifications on iOS 15+/macOS 12+: the conversation icon
  becomes the sender's (or the group's) avatar, and the donated intents feed
  Siri suggestions and Focus. Note that Focus's "allowed people" breakthrough
  matches against the user's *contacts*, which needs contact details
  (phone/email) that `Conversation` doesn't currently carry — so the rendering
  upgrade applies, but per-contact breakthrough doesn't yet. Requires the
  entitlement and `Info.plist` entry listed under packaging requirements;
  without them, the plain rendering is used.
- **Windows**: ignored. **Linux**: sent as the advisory `im.received`
  category hint, which some daemons use for theming.

## Progress, scheduling, and other modes

Every option below is safe to use unconditionally: it maps to the native
feature where one exists, keeps each platform's default when unset, and is a
no-op where the platform has no such concept.

```rust,no_run
use std::time::{Duration, SystemTime};
use robius_notifications::{Notification, Progress};

// A download notification with a progress bar...
Notification::new()
    .set_id("download-42")
    .set_title("Downloading report.pdf")
    .set_progress(Progress::Determinate { current: 0, total: 100 })
    .show()?;
// ...advanced quietly in place (no re-alert) as bytes arrive:
robius_notifications::update_progress(
    "download-42",
    Progress::Determinate { current: 30, total: 100 },
)?;

// A reminder shown 5 minutes from now:
Notification::new()
    .set_title("Meeting in 5 minutes")
    .set_scheduled_time(SystemTime::now() + Duration::from_secs(300))
    .show()?;
# Ok::<(), robius_notifications::Error>(())
```

- **Progress** (`set_progress` + `update_progress`): real progress bars on
  Android and Windows, updated in place without re-alerting (progress
  notifications alert only when first shown). Linux daemons that support the
  `value` hint show the percentage; iOS/macOS have no notification progress
  concept (no-op).
- **Scheduling** (`set_scheduled_time`): on iOS/macOS/Windows the OS shows the
  notification at the scheduled time, even if the app has exited (Windows
  needs a registered AUMID for that, and interactions with a scheduled toast
  aren't delivered in-process). Android and Linux use an in-process timer, so
  the showing dies with the app. Past times show immediately; `cancel` also
  cancels a pending scheduled showing.
- **Persistent** (`set_persistent`): Android "ongoing" (not swipeable away),
  Windows reminder-style (stays on screen — Windows only honors this when the
  toast has at least one action button), Linux never-auto-expires;
  iOS/macOS have no equivalent (no-op).
- **Lock-screen privacy** (`set_lock_screen_visibility`): Android
  public/private/secret; the other platforms manage lock-screen privacy at
  the OS level (no-op). Unset = the user's own lock-screen setting decides.
- **Timestamps** (`set_timestamp`): Android shows the event time instead of
  the post time; others always show delivery time (no-op).
- **Quiet permission** (`request_provisional_permission`): never prompts.
  iOS/macOS grant provisional authorization (quiet delivery straight to
  Notification Center); Android reports the standing state; Windows/Linux
  behave like `request_permission`.
- **Do-Not-Disturb bypass** (`set_bypass_do_not_disturb` +
  `set_uses_critical_alerts`): Apple critical alerts and Android
  DnD-bypassing channels — both gated by the packaging requirements listed
  above; the OS silently downgrades without them. Windows/Linux have no
  override (no-op).
- **Active notifications** (`active_notification_ids`): reports which of the
  app's notifications are still showing. Android and iOS/macOS report all of
  them; Windows and Linux only those shown by this run of the app.
- **Group summaries**: Android only bundles `set_group` notifications when a
  summary notification exists, so the crate posts and prunes one
  automatically — no API needed.

## Notification preferences

The OS owns notification preferences, but apps can read them back, deep-link
into them, and (on Apple platforms) be linked back *from* them:

- `notification_settings(scope, callback)` reports the user's current
  settings for `SettingsScope::App`, one `Channel`, or one `Conversation`.
  Android reports real per-channel/per-conversation state (urgency, sound,
  badge, whether the user customized it, priority-conversation); iOS, macOS,
  and Windows report app-level state for every scope; Linux only reports
  whether a notification service is reachable.
- `open_notification_settings(scope)` opens the OS settings UI: Android down
  to a single channel's or conversation's page, iOS the app's Settings page,
  Windows the system notification settings, macOS best-effort. Linux returns
  `Error::Unsupported`.
- `set_provides_notification_settings(true)` (call before
  `request_permission`) tells iOS/macOS that the app has its own notification
  settings screen; the OS then links to the app from its settings UI, and the
  user choosing that link arrives at your interaction handler as
  `InteractionKind::OpenSettings` — navigate to your settings screen when it
  does.

## Handling interactions

Register your handler with `set_interaction_handler` **as early as possible**
during app startup. Interactions that arrive before a handler is set are
queued and delivered once one is set, but an interaction can only be delivered
at all if the app process learns about it:

- **iOS/macOS**: interactions are delivered even when they launched the app,
  via the notification center delegate.
- **Android**: tapping the notification body launches (or re-focuses) the
  app's launcher activity, and the tap is delivered on startup. Action
  presses, replies, and dismissals are delivered only while the app process
  is running.
- **Windows and Linux**: interactions are delivered only while the app is
  running; activating a notification after the app exits does not relaunch it.

The handler may run on any thread (a platform callback thread, the main UI
thread, or a background thread), so use a channel or your UI toolkit's
equivalent (e.g., `Cx::post_action` in Makepad) to get interactions over to
your app logic.

## Permission

Call `request_permission` before showing notifications.

- **Android 13+** shows the system prompt (see the manifest requirement
  below); Android 12 and older report whether notifications are enabled.
- **iOS/macOS** show the system prompt once; afterwards it reports the user's
  standing decision (changeable in system settings).
- **Windows** reports whether toasts are currently enabled for the app.
- **Linux** has no permission concept and always reports granted.

## App packaging requirements

Most of this crate works with zero app configuration, but some features need
an entry in your app's manifest, entitlements, or packaging. Everything that
does is listed here (and on the corresponding function's docs):

| Requirement | Needed for | Where |
| ----------- | ---------- | ----- |
| `minSdk` 26 (Android 8.0) | the whole crate on Android | Android build config |
| `<uses-permission android:name="android.permission.POST_NOTIFICATIONS" />` | showing any notification on Android 13+ (`request_permission`) | Android manifest |
| `com.apple.developer.usernotifications.time-sensitive` | full `Urgency::Critical` treatment on iOS/macOS (downgraded without it) | Apple entitlements file |
| `com.apple.developer.usernotifications.critical-alerts` (granted by Apple on request) | `set_bypass_do_not_disturb` + `set_uses_critical_alerts` on iOS/macOS (downgraded without it) | Apple entitlements file |
| `com.apple.developer.usernotifications.communication` | communication-notification rendering of conversations on iOS/macOS (the `apple-communication` cargo feature; plain rendering without it) | Apple entitlements file |
| `NSUserActivityTypes` array containing `INSendMessageIntent` | the intent donations behind the `apple-communication` feature (Siri suggestions, Focus integration); donations fail silently without it | Apple Info.plist |
| Running from a bundled `.app` | the whole crate on macOS (`Error::NoAppBundle` otherwise) | macOS packaging |
| `<uses-permission android:name="android.permission.ACCESS_NOTIFICATION_POLICY" />` + the user granting Do-Not-Disturb access in settings | `set_bypass_do_not_disturb` on Android (silently ignored without the grant) | Android manifest + user grant |
| A registered AppUserModelID, passed to `set_app_id` | proper toast attribution on unpackaged Windows apps (dev fallback works without it); **required** for a `set_scheduled_time` toast to fire after the app exits | Windows installer/shortcut or MSIX |
| A `.desktop` file, its basename passed to `set_app_id` | the app's name/icon on Linux notifications | Linux packaging |

Everything else — channels, groups, conversations, actions, replies, progress,
scheduling, settings read-back and deep links — needs no app-side
configuration on any platform.

## Android integration

The **minimum supported Android API level is 26 (Android 8.0)**: the bundled
Java helper is loaded via `InMemoryDexClassLoader`, which requires API 26, so
set `minSdk` to at least 26 in your app.

On Android 13 and newer, your app manifest must declare the notification
permission:

```xml
<uses-permission android:name="android.permission.POST_NOTIFICATIONS" />
```

Notable Android behaviors:

- Every notification belongs to a channel — shown to users as a notification
  "category" in system settings, where they can tune or disable each one.
  Use `Notification::set_channel` to control the channel's id,
  user-visible name, and importance; without one, a per-urgency default
  channel is used ("Notifications", "Quiet notifications", or "Urgent
  notifications"), so one notification's urgency can't affect another's.
  Android applies a channel's importance (from `Urgency`) only when the
  channel is first created; after that, only the user can change it, and
  this crate never alters an existing channel's importance.
- `NotificationChannel::set_group` gathers related channels under a titled
  section in system settings; the group is created on first use and its name
  refreshes on later shows.
- Conversations publish a long-lived dynamic launcher shortcut per
  conversation (that's how Android models them; no manifest changes needed).
  If the user has customized a conversation in system settings, the crate
  automatically posts under the system-created per-conversation channel.
- `Sound::Silent` posts through a low-importance `<channel-id>.silent`
  variant channel (sound lives on the channel on Android 8+), which shows up
  as its own channel in system settings. `Sound::Named` falls back to the
  default sound for the same reason.
- Body taps: if the app's activity was already alive and the OS delivers the
  tap via `onNewIntent` without recreating the activity, the app is brought
  to the foreground but the `Activated` interaction is only observable if the
  host activity calls `setIntent()` in `onNewIntent` (Makepad and most
  NativeActivity-style hosts recreate or cold-start instead, where delivery
  works).
- After a quick reply, the notification is removed automatically — Android
  requires a replied-to notification to be updated or removed, otherwise its
  reply UI spins forever.
- `Action::set_foreground` is ignored: broadcast-based actions can't bring
  the app to the foreground since Android 12's notification-trampoline ban.

## iOS and macOS integration

On macOS, the app must be running from a bundled `.app`: a bare binary run
via `cargo run` gets `Error::NoAppBundle` from every function, because the
OS has no app registration to attribute notifications to.

- `Urgency::Critical` maps to the time-sensitive interruption level, which
  needs the `com.apple.developer.usernotifications.time-sensitive`
  entitlement to take full effect (the system downgrades it otherwise).
  Urgency is ignored on iOS 14/macOS 11 and older.
- Images must be a file type and size that UserNotifications accepts as an
  attachment (PNG/JPEG/GIF, up to ~10 MB). The crate attaches a temporary
  copy, so your original file stays where it is (the system consumes the
  attached file).
- While the app is in the foreground, notifications are still presented
  (banner + sound + badge) via the delegate.
- iOS shows at most 4 action buttons; extras aren't displayed.
- If your app has its own notification settings screen, call
  `set_provides_notification_settings(true)` before `request_permission` and
  handle `InteractionKind::OpenSettings`; the OS settings UI then links users
  straight to it.

## Windows integration

Windows attributes toasts to an AppUserModelID (AUMID):

- Packaged apps (MSIX) have one automatically.
- Unpackaged apps should call `set_app_id` with the AUMID their installer
  registered (e.g., via a Start Menu shortcut). Without one, a built-in
  system AUMID is borrowed so toasts still show during development — but
  they're attributed to "Windows PowerShell".

Toast activation only reaches the app while it's running; relaunch-on-click
would require a registered COM activator, which this crate doesn't provide.
`set_badge_count` and `set_group` are ignored on Windows.

### Windows: known gaps (TBD)

Windows has the largest distance between what the OS offers packaged/registered
apps and what an unpackaged app gets, so a few things are known gaps we plan to
address rather than silent limitations:

- **Relaunch-on-click**: interactions are lost once the app exits. The planned
  fix is opt-in protocol activation (toasts carrying a custom URL scheme the
  app registers as its handler), which would deliver taps and button presses
  after a relaunch — though quick-reply text can only ever arrive while the
  app is running.
- **Scheduled toasts deliver no interactions**: `ScheduledToastNotification`
  cannot carry the in-process event handlers at all, so clicks on a scheduled
  toast are currently lost even while the app runs. The same protocol
  activation work would fix this.
- **AUMID registration**: without `set_app_id` and a registered AppUserModelID,
  toasts are attributed to "Windows PowerShell" (dev fallback) and scheduled
  toasts won't fire after the app exits. A registration helper may be provided
  separately.
- **Persistent toasts need a button**: Windows only honors the stays-on-screen
  reminder mode when the toast has at least one action.
- **Per-section settings**: Windows "toast collections" could give named
  sub-groups with their own row in Settings, but appear to require packaged
  (MSIX) identity and offer no read-back of the user's per-collection choices.
- **`active_notification_ids` only covers this run**: toast tags are hashes,
  so notifications from a previous run can't be mapped back to their ids.

## Linux integration

Call `set_app_id` with the basename of your app's `.desktop` file so the
notification daemon can look up your app's name and icon; without it, the
daemon shows the executable's name and no icon.

- Action buttons (and body-click `Activated` interactions, which use the
  spec's `"default"` action) require the daemon to advertise the `actions`
  capability — GNOME, KDE, and most others do.
- Quick replies use the `inline-reply` capability (KDE Plasma has it); on
  daemons without it, reply actions degrade to plain buttons that arrive as
  `InteractionKind::Action`.
- Some daemons emit a close event right after an action is invoked, so an
  `Activated`/`Action` interaction may be followed by a spurious `Dismissed`.

## What maps where

Not every builder option exists on every platform; unsupported options are
simply ignored there:

| Option              | Android | iOS/macOS | Windows | Linux |
| ------------------- | ------- | --------- | ------- | ----- |
| title/body          | ✓       | ✓         | ✓       | ✓     |
| subtitle            | ✓ (sub-text) | ✓    | ✓ (3rd line) | ✓ (appended to body) |
| actions & replies   | ✓       | ✓         | ✓       | ✓ (daemon-dependent) |
| id replace/cancel   | ✓       | ✓         | ✓       | ✓ (within one app run) |
| urgency             | ✓ (channel importance) | ✓ (interruption level) | ✓ (scenario/suppress) | ✓ (urgency hint) |
| sound               | default/silent | ✓  | ✓       | ✓     |
| image               | ✓ (big picture) | ✓ (attachment) | ✓ (hero image) | ✓ (image-path hint) |
| badge count         | ✓ (setNumber) | ✓   | –       | –     |
| group/thread        | ✓       | ✓         | –       | –     |
| timeout             | –       | –         | ✓ (expiration) | ✓ (expire timeout) |
| metadata round-trip | ✓       | ✓         | ✓       | ✓     |
| dismissed events    | ✓       | ✓         | ✓ (explicit dismissal) | ✓ (reason 2) |
| channels (categories) & groups | ✓ | –    | –       | –     |
| conversations       | ✓ (11+, with message history) | ✓ (thread grouping; full communication rendering with the `apple-communication` feature) | – | ✓ (advisory hint) |
| progress bars       | ✓       | –         | ✓ (in-place updates) | ✓ (`value` hint) |
| scheduled delivery  | ✓ (in-process) | ✓ (OS-side) | ✓ (OS-side) | ✓ (in-process) |
| persistent/ongoing  | ✓       | –         | ✓ (stays on screen) | ✓ (never expires) |
| lock-screen privacy | ✓       | –         | –       | –     |
| event timestamps    | ✓       | –         | –       | –     |
| DnD bypass          | ✓ (with user grant) | ✓ (with entitlement) | – | – |
| quiet permission    | ✓ (reports state) | ✓ (provisional) | ✓ | ✓ |
| active-notification query | ✓ | ✓        | ✓ (this run) | ✓ (this run) |
| settings read-back  | ✓ (per channel/conversation) | ✓ (app-level) | ✓ (app-level) | ✓ (service reachability) |
| open settings UI    | ✓ (down to one conversation) | ✓ (iOS: app page; macOS: system pane, best-effort) | ✓ (system page) | –     |
| `OpenSettings` hook | –       | ✓         | –       | –     |
