# `robius-location`

A Rust library to access system-provided location/GPS data across Linux, Android, iOS, macOS, and
Windows.

## Cached and live fixes

`update_once()` usually calls your handler twice: first with whatever fix the OS already had lying
around, then with the one it goes on to actually measure. `Location::freshness()` tells you which of
the two you're looking at, and `is_cached()` is the shorthand for the first.

A cached fix is never more than an hour old on any platform. Anything older gets dropped and you just
wait for the real one. `Location::time()` says exactly how old it is; every platform reports that as
wall-clock UTC counted from the Unix epoch, so the number means the same thing everywhere.

So ignore the cached fixes if you only want real measurements. Just know that if the OS never manages
to get one, a single request can end on the cached fix without an error.

## Usage on Linux

Linux picks one of two paths automatically, and needs no setup from you:
* [XDG Desktop Portal Location API][location-portal] (v1), always preferred.
  Works sandboxed or not, and the desktop handles the permission prompt.
* [GeoClue] directly, only if the session bus, portal, or Location interface is missing at startup.
  * Never used after a portal denial, and never for Flatpak/Snap apps.

There's no C library or `-dev` package to link against. You just need a running GeoClue service,
which is already handled by the majority of distros.
* If you ship a `.deb`/`.rpm`, add a dependency on `geoclue-2.0` (Debian/Ubuntu), `geoclue2` (Fedora/RHEL),
  or `geoclue` (Arch/Alpine). Flatpaks don't need that, just use the host portal.

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
