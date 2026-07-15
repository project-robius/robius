# `robius-location`

A Rust library to access system-provided location/GPS data across multiple platforms.

Currently supports iOS, macOS, Windows, and Android, with Linux support coming soon.

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
