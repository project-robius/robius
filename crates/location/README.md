# Archived: this has moved to https://github.com/project-robius/robius

# `robius-location`

A library to access system location data.

## Android

On Android the following must be added to the manifest:
```xml
<manifest ... >
  <!-- Always include this permission -->
  <uses-permission android:name="android.permission.ACCESS_COARSE_LOCATION" />

  <!-- Include only if your app benefits from precise location access. -->
  <uses-permission android:name="android.permission.ACCESS_FINE_LOCATION" />
</manifest>
```
As specified in the [Android documentation][android-docs].

[android-docs]: https://developer.android.com/develop/sensors-and-location/location/permissions
