# Robius

A collection of crates for accessing system features (platform APIs) in a cross-platform manner from Rust applications.

These `robius` crates have a few design goals and major advantages compared to other platform feature abstraction crates:
* They cleanly abstract across both desktop and mobile platforms, so your app code doesn't have to worry about platform-specific code or special cases for different classes of platforms.
* They're quite easy to use from your app (or another Rust library, if desired).
   * In general, they "just work" without any special setup or plugin-like registration. You just add the crate as a dependency and start using it, especially with the [Makepad UI toolkit](https://github.com/makepad/makepad).
   * They should also easily work with other UI toolkits, but may require a small amount of setup. Feel free to file an issue if you find any of them difficult to use with your UI toolkit or Rust app setup.
* Each crate brings in minimal dependencies; we shoot for fast compile times and small object size.
   * If larger dependencies are needed for some features, we keep those optional wherever possible.
* Sane default behavior. We like to keep apps simple, so these crates are mildly opinionated and typically do what you would want by default. But you can customize their behavior as needed, too.

See the below [Summary](#summary-of-included-crates) section for more info, or look at each individual crates' docs in [`crates/`](crates/).
There's also a [status table](#crate--platform-status-table) that shows what each crate supports on each platform, and what platform API it uses under the hood.


## Summary of included crates

* [`robius-authentication`](crates/authentication/): asks the OS to show a native authentication prompt to verify the user.
  * TouchID & FaceID on Apple, Windows Hello on Windows, a BiometricPrompt on Android, and a `polkit` prompt on Linux.
  * Can also fall back to password entry on most platforms, if the OS permits it.
* [`robius-directories`](crates/directories/): provides the canonical directory location where each platform wants your app to store its config, cache, data, downloads, etc.
  * This uses XDG defaults on Linux, Known Folders on Windows, Standard Directories on macOS/iOS, and `Context.getFilesDir()` on Android.
  * Basically just a fork of the now-archived `directories` crate, with Android support added (using our [`robius-android-env`] crate). Note that only `ProjectDirs` is implemented on Android, which is really the only thing your app probably needs.
* [`robius-file-picker`](crates/file-picker/): shows the OS's native open file, save file, or media picker dialogs/popup.
  * On desktop, this is a thin wrapper around [`rfd`].
  * On iOS, we use `UIDocumentPickerViewController` and `PHPickerViewController`.
  * On Android, it uses `ACTION_OPEN_DOCUMENT`/`ACTION_CREATE_DOCUMENT`, the system Photo Picker, or `MediaStore.Downloads` based on the current Android version (that's automatic, you don't have to pick it yourself).
* [`robius-location`](crates/location/): gets the device's geolocation from the OS, and handles the permission prompts for you.
  * Differentiates between an OS's cached location and a fresh (just-retrieved) location.
* [`robius-open`](crates/open/): opens a URI/URL in the default system app.
  * Works with `http://`, `tel:`, `mailto:`, `file://`, custom schemes, and more.
  * This uses `NSWorkspace` on macOS, `UIApplication` on iOS, `Intent` on Android, `xdg-open` on Linux, and WinRT's `Launcher` on Windows.
* [`robius-share`](crates/share/): opens the native system share sheet so you can share a file or other content with a different app on your system.
  * This is implemented using `Intent.createChooser` on Android, `UIActivityViewController` on iOS, `NSSharingServicePicker` on macOS, and the WinRT Share UI on Windows.
  * Linux doesn't really have a system share sheet, so we wrote a custom XDG portal connection that still supports every possible type of share payload.
* [`robius-web-auth-session`](crates/web_auth_session/): runs an OAuth/SSO login process in the OS's own in-app browser session, and then sends the result back to your app.
  * Currently this is for iOS only, based on `ASWebAuthenticationSession`. There's no other safe/supported way to do web login on iOS, because otherwise iOS will suspend your app while showing the browser.

## Platform status table for each crate
Symbol legend: ✅ fully supported · ⚠️ partial, or has issues · 🚧 under construction · ❌ not supported

| Crate | macOS | iOS | Android | Windows | Linux |
| --- | --- | --- | --- | --- | --- |
| [`robius-authentication`](crates/authentication/) | ✅ `LAContext` (LocalAuthentication): TouchID + username/password | ✅ `LAContext` (LocalAuthentication): TouchID / FaceID / PIN | ✅ `BiometricPrompt` + screen lock | ✅ `UserConsentVerifier` (WinRT) for Windows Hello, with username/password fallback | ✅ `polkit` prompt via the desktop's auth agent (needs a `.policy` file) |
| [`robius-directories`](crates/directories/) | ✅ Apple Standard Directories (`~/Library/...`) | ✅ Apple Standard Directories (in app sandbox) | ✅ `Context.getFilesDir()`, `getCacheDir()` etc. via [`robius-android-env`] | ✅ Known Folders API | ✅ XDG Base Directory / User Directory |
| [`robius-file-picker`](crates/file-picker/) | ✅ `NSOpenPanel`, `NSSavePanel` (via [`rfd`]) | ✅ `UIDocumentPickerViewController`, `PHPickerViewController` | ✅ `ACTION_OPEN_DOCUMENT`, `ACTION_CREATE_DOCUMENT`, system Photo Picker, `MediaStore.Downloads` | ✅ `IFileDialog` (via [`rfd`]) | ✅ XDG portal `FileChooser` (via [`rfd`]) |
| [`robius-location`](crates/location/) | ✅ `CLLocationManager` (CoreLocation) | ✅ `CLLocationManager` (CoreLocation) | ✅ `LocationManager` | ✅ `Geolocator` (`Windows.Devices.Geolocation`, WinRT) | ✅ XDG Location portal, with a `GeoClue` fallback |
| [`robius-open`](crates/open/) | ✅ `NSWorkspace.openURL` | ✅ `UIApplication.openURL` | ✅ `Intent` (`ACTION_VIEW`) | ✅ `Launcher.LaunchUriAsync` (WinRT) | ✅ `xdg-open` |
| [`robius-share`](crates/share/) | ✅ `NSSharingServicePicker` | ✅ `UIActivityViewController` | ✅ `ACTION_SEND` / `ACTION_SEND_MULTIPLE` via `Intent.createChooser` | ✅ WinRT Share UI (`DataTransferManager`) | ✅ XDG portal "Open With" chooser (`OpenURI` / `OpenFile`), or its `SaveFiles` dialog for multi-item payloads; `xdg-open` fallback |
| [`robius-web-auth-session`](crates/web_auth_session/) | ❌ not supported | ✅ `ASWebAuthenticationSession` | 🚧 planned (custom chrome tabs) | ❌ not supported | ❌ not supported |


## Version requirements

Each crate has its own Minimum Supported Rust Version (MSRV), typically 1.77, but we try to keep it as low as possible.

On Android, most crates support API Level 26 and up (Android 8.0), but we're always extremely careful to check the API level at runtime and only use features that are available. So you generally don't need to worry about that at all.

| Crate | MSRV | Android API Level |
| --- | --- | --- |
| `robius-authentication` | 1.77 | 28 for biometrics, 29 for password fallback |
| `robius-directories` | 1.64 | None |
| `robius-file-picker` | 1.88 | 26 |
| `robius-location` | 1.77 | 26 |
| `robius-open` | 1.72 | None |
| `robius-share` | 1.77 | 26) |
| `robius-web-auth-session` | 1.71 | Unsupported |


## Common behavior/conventions in robius crates

### Calling from the main UI thread
Due to platform requirements, most functions exposed by `robius` crates need to be invoked from the main UI thread. This is enforced at runtime and will return an error if mis-used.

The one exception is `robius-file-picker`, which can be invoked on any thread (it will jump to the main thread if the platform requires it), and the "file picked" callback always gets run on a separate native OS thread.


[`robius-android-env`]: https://github.com/project-robius/robius-android-env
[`rfd`]: https://crates.io/crates/rfd
