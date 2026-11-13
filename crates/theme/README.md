# robius-theme

Get the system's appearance setting (light/dark theme) and subscribe to appearance changes, all from Rust without depending on a UI toolkit.

```rust
use robius_theme::{query, subscribe, Subscription, Theme};

fn follow_the_system() -> robius_theme::Result<Subscription> {
    let theme = query()?.unwrap_or(Theme::Light);
    println!("System appearance: {theme:?}");

    subscribe(|theme| {
        // Runs on a system thread, so send the theme to your UI thread.
        println!("System appearance: {theme:?}");
    })
}
```

`Some(Theme::Light)` and `Some(Theme::Dark)` are concrete preferences.
`None` means no preference is available; your application should choose a fallback.
`Err` means that the system couldn't be asked, so your app should keep whichever theme it already has.

## Receiving updates

Keep the `Subscription` alive for as long as you want updates, and drop it to stop them.

The callback receives the current theme first, possibly before `subscribe()` returns, and then each change.
It never receives the same theme twice in a row.

The system reports each change, so this crate never checks the theme on a timer.
All of your subscriptions share one system subscription per process.

The callback runs on a thread that the system picks.
That is your main thread on macOS, iOS, and Android, and a background thread on Linux and Windows.
Keep it short and send the theme to your UI thread or event loop.
A call that is already running may still finish after you drop the subscription.

## Platforms

| Platform | Where the theme comes from | Requirements |
| --- | --- | --- |
| macOS | ✅ the `AppleInterfaceStyle` preference and `AppleInterfaceThemeChangedNotification` | macOS 11+; updates need an `NSApplication` |
| iOS | ✅ the main `UIScreen`'s `userInterfaceStyle` and UIKit's trait changes | iOS 13+ |
| Android | ✅ `Configuration.uiMode` and the application's `onConfigurationChanged` | API 26+; an Android SDK and JDK to build |
| Windows | ✅ the `AppsUseLightTheme` registry value, or the contrast theme's colors | Windows 10+ |
| Linux | ✅ the XDG Settings portal's `color-scheme` | a desktop that offers it through `xdg-desktop-portal` |
| others | ❌ | `Error::Unsupported` |

macOS and Windows need no dependencies at all. iOS uses `objc2`, Android uses `jni`, and Linux uses `zbus`.

### macOS

Apple documents neither the preference nor the change it posts, but AppKit itself and Apple's own apps rely on both.

* Your app's own `NSApp.appearance` has no effect on what this crate reports.
* The system only reports changes to processes that have an `NSApplication`. A command-line tool can `query()`, but its callback is only called once.
* On macOS 11 to 13 with the Auto appearance, the preference can be a day out of date after the Mac was shut down across sunrise or sunset. This is a macOS bug that later releases fixed.

### iOS

The main screen always has the system's theme, whatever your windows and views override.

* On iOS 17 and later, changes are reported even while a window has `overrideUserInterfaceStyle` set.
* On iOS 13 to 16, a change that happens while your key window overrides its style is reported when your app next becomes active, or as soon as you call `query()` on the main thread.
* If your `Info.plist` sets `UIUserInterfaceStyle`, the system hides its theme from your app, so this crate reports the value you set there.
* `query()` works on any thread, but before your app's UI has started, a background thread gets `None`.

### Android

Android 8 and 9 have a night mode, and the system-wide dark theme arrived in Android 10. This crate reports whichever one the device has.

* From Android 12, `UiModeManager.setApplicationNightMode(YES/NO)` replaces the system's theme in everything your app can see, so this crate reports the theme you forced. Call it with `MODE_NIGHT_AUTO` to follow the system again, and the callback then receives the system's theme.
* The app's Android context must be set up before you use this crate, which `robius-android-env` does. A Makepad app must depend on `robius-use-makepad` for that.
* Building for Android needs the Android SDK (a platform and build-tools) and a JDK, because a small Java class is compiled into the crate. Set `ANDROID_HOME` and `JAVA_HOME` if they aren't found, and `ANDROID_PLATFORM` or `ANDROID_BUILD_TOOLS_VERSION` to choose versions other than the newest installed ones.

### Windows

Windows keeps the app theme in the registry value that this crate reads, and it's the value that the `UISettings` API reads too.

* While a contrast theme is on, the theme is dark if that theme's window background is dark.
* An app that forces its own title bars or menus to light or dark has no effect on what this crate reports.

### Linux

The [Settings portal](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.Settings.html) is the one source that every modern desktop shares, and the only one that works inside Flatpak and Snap.
GNOME 42+, KDE Plasma 5.24+, Cinnamon, Xfce 4.18+, MATE, COSMIC, and elementary OS all offer its `color-scheme`, and `darkman` adds it to any other desktop.

Older desktops only name a theme, so where the portal has no `color-scheme` (or one that the desktop doesn't keep current), this crate reads the desktop's own setting:

* GNOME before 42 (like Ubuntu 20.04) and KDE Plasma before 5.24: the GTK theme's name, or KDE's window color, through the same portal.
* Cinnamon, MATE and Xfce: their own GTK theme setting.
* Window managers without a desktop (i3, sway, Openbox, LXDE and others), and systems without the portal: GTK's `settings.ini`.

A theme whose name has "dark" in it (like `Yaru-dark`, but not `Arc-Darker`) counts as dark.
Changes to all of these are followed too, but only on desktops that need them: on a current desktop, none of this runs.

* GNOME 42+ reports its default light style as no preference (`None`).
* `query()` can wait up to a second while the desktop starts the portal. Once a subscription has received the theme, `query()` answers from memory.

## Following the system in an app with its own theme setting

Most apps let the user choose Light, Dark, or Auto. For Auto, your app should stop forcing a theme on the parts that the system draws for it:
that is required on Android 12+ (see above), and it lets title bars and keyboards change together with the system on macOS and iOS.
In Makepad, that is `NativeAppearance::System`.

When the user goes back to Auto, call `query()` again for the theme to show right away.

## Example

This prints the system theme and then each change:

```sh
cargo run -p robius-theme --example subscribe
```
