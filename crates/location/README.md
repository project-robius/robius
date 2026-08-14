# `robius-location`

A Rust library to access system-provided location/GPS data across Linux, Android, iOS, macOS, and
Windows.

## Usage on Linux

Linux picks one of two paths automatically, and needs no setup from you:
* [XDG Desktop Portal Location API][location-portal] (v1), always preferred.
  Works sandboxed or not, and the desktop handles the permission prompt.
* [GeoClue] directly, only if the session bus, portal, or Location interface is missing at startup.
  * Never used after a portal denial, and never for Flatpak/Snap apps.

There's no C library or `-dev` package to link against. What the *end user* needs is a running
GeoClue service and some location source (Wi-Fi/IP, modem GPS, NMEA, ...), which a library can't
install for them:
* Most full GNOME / KDE Plasma / COSMIC installs already have everything.
* The GeoClue fallback is what makes LXQt, wlroots/Hyprland, and minimal desktops work.
* If you ship a `.deb`/`.rpm`, depend on `geoclue-2.0` (Debian/Ubuntu), `geoclue2` (Fedora/RHEL),
  or `geoclue` (Arch/Alpine). Flatpaks should just use the host portal.
* **Note:** GeoClue usually wants a desktop authorization agent, so this isn't a headless/SSH fallback.

Some details worth knowing:
* Both [`Access`] variants map to the same portal permission, since it has no foreground/background
  split.
* `Accuracy::Approximate` asks for city-level, `Accuracy::Precise` for exact.
* Portal denials arrive asynchronously as `Error::AuthorizationDenied` on `Handler::error`;
  the GeoClue path can return it straight from `request_authorization`.
* `update_once` gives up after 60s with `Error::TemporarilyUnavailable`.
* Altitude, bearing, speed, and time are all optional and individually return
  `Error::TemporarilyUnavailable` when the provider doesn't supply them.
* If the provider dies (crash, upgrade, restart), you get `Error::TemporarilyUnavailable` and
  updates stop; the next call reconnects, so just retry.
* `Manager::new` figures out a desktop ID itself. `Manager::new_with_desktop_id` (Linux-only) is
  there if you need to pass a specific installed one, without the `.desktop` suffix.

Getting a fix is never guaranteed: privacy settings, disabled radios, VPNs, provider outages, and
machines with nothing to position against all fail the same way. We also deliberately don't ship
Wi-Fi data off to some third-party geolocation service behind the desktop's back.

There are end-to-end tests too, ignored by default since they need a real provider:

```console
cargo test -p robius-location --test linux_live -- --ignored --nocapture --test-threads=1
```

[`Access`]: https://docs.rs/robius-location/latest/robius_location/enum.Access.html
[GeoClue]: https://gitlab.freedesktop.org/geoclue/geoclue
[location-portal]: https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.Location.html

## Usage on iOS
To use this crate on iOS, you must add the following to your app's `Info.plist`:
```xml
  <key>NSLocationAlwaysAndWhenInUseUsageDescription</key>
	<string>Insert your usage description here.</string>
	<key>NSLocationWhenInUseUsageDescription</key>
	<string>Insert your usage description here.</string>
	<key>NSLocationUsageDescription</key>
	<string>Insert your usage description here.</string>
	<key>NSLocationDefaultAccuracyReduced</key>
	<false/>
```
Note that the last `NSLocationDefaultAccuracyReduced` key isn't required unless you always need fine-grained location detail. 

## Usage on Android
To use this crate on Android, you must add the following to your app's `AndroidManifest.xml`:
```xml
<manifest ... >
  <!-- Always include this permission -->
  <uses-permission android:name="android.permission.ACCESS_COARSE_LOCATION" />

  <!-- Include only if your app benefits from precise location access. -->
  <uses-permission android:name="android.permission.ACCESS_FINE_LOCATION" />
</manifest>
```
As specified in the [Android documentation][android-docs].

Note that these go in the `manifest` key section, not the `application` section.

### Minimum API level
The minimum supported Android API level is **26 (Android 8.0)**: the bundled Java helper is loaded via
`InMemoryDexClassLoader`, which requires API 26. Newer location APIs (e.g. `getCurrentLocation`) are used only
when the device supports them, with a fallback for older versions, so set `minSdk` to at least 26 in your app.

[android-docs]: https://developer.android.com/develop/sensors-and-location/location/permissions
