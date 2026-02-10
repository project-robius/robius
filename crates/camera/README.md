# `robius-camera`

[![Latest Version](https://img.shields.io/crates/v/robius-camera.svg)](https://crates.io/crates/robius-camera)
[![Docs](https://docs.rs/robius-camera/badge.svg)](https://docs.rs/robius-camera/latest/robius_camera/)
[![Project Robius Matrix Chat](https://img.shields.io/matrix/robius-general%3Amatrix.org?server_fqdn=matrix.org&style=flat&logo=matrix&label=Project%20Robius%20Matrix%20Chat&color=B7410E)](https://matrix.to/#/#robius:matrix.org)

Rust abstractions for capturing photos using the native system camera UI.

This crate is part of the [Robius](https://robius.rs/) project, providing cross-platform
access to camera functionality for Rust applications.

## Overview

Instead of implementing a custom camera view, this crate opens the native system camera
interface (similar to tapping "take photo" in a messaging app), letting the operating system
handle the UI, permissions, and capture experience. The resulting image is returned to your
app as JPEG data.

This approach is similar to Flutter's [`image_picker`](https://pub.dev/packages/image_picker)
plugin, prioritizing simplicity and native UX over full camera control.

## Supported Platforms

| Platform | Implementation | Notes |
|----------|----------------|-------|
| iOS | [`UIImagePickerController`] | Full support |
| Android | [`ACTION_IMAGE_CAPTURE`] Intent | Full support |
| Windows | - | Not supported (CameraCaptureUI requires UWP context) |
| macOS | - | Not yet supported (no system camera UI) |
| Linux | - | Not yet supported (no system camera UI) |

[`UIImagePickerController`]: https://developer.apple.com/documentation/uikit/uiimagepickercontroller
[`ACTION_IMAGE_CAPTURE`]: https://developer.android.com/reference/android/provider/MediaStore#ACTION_IMAGE_CAPTURE

## Feature Flags

| Feature | Description |
|---------|-------------|
| `log` | Enables logging via the `log` crate |


## Usage on iOS

Add the following to your app's `Info.plist`:
```xml
<key>NSCameraUsageDescription</key>
<string>This app needs camera access to take photos.</string>
```

## Usage on Android

Add the following to your app's `AndroidManifest.xml`:
```xml
<uses-feature android:name="android.hardware.camera" android:required="false" />
<uses-permission android:name="android.permission.CAMERA" />
```

**Note:** Android support currently requires [Makepad](https://github.com/makepad/makepad).
The crate integrates with Makepad's activity result callback infrastructure via `robius-android-env`.


## Example

```rust
use robius_camera::{capture_photo, CameraPosition};

// Check if camera is available
if robius_camera::is_available() {
    // Capture a photo using the back camera
    capture_photo(CameraPosition::Back, |result| {
        match result {
            Ok(photo) => {
                println!("Captured {}x{} photo", photo.width(), photo.height());
                let jpeg_data = photo.into_jpeg_data();
                // Use the JPEG data...
            }
            Err(robius_camera::Error::Cancelled) => {
                println!("User cancelled");
            }
            Err(robius_camera::Error::PermissionDenied) => {
                println!("Camera permission denied");
            }
            Err(e) => {
                eprintln!("Error: {:?}", e);
            }
        }
    }).expect("failed to open camera");
}
```

### Front Camera (Selfie)

```rust
use robius_camera::{capture_photo, CameraPosition};

capture_photo(CameraPosition::Front, |result| {
    if let Ok(photo) = result {
        // Handle selfie photo
    }
}).ok();
```


## API

### Functions

| Function | Description |
|----------|-------------|
| `capture_photo(position, callback)` | Opens the system camera UI to capture a photo |
| `is_available()` | Returns whether camera capture is available on this device |

### Types

| Type | Description |
|------|-------------|
| `CameraPosition` | Enum: `Back` (rear camera) or `Front` (selfie camera) |
| `PhotoData` | Contains JPEG data, width, and height of captured photo |
| `Error` | Error types including `Unsupported`, `Cancelled`, `PermissionDenied`, `CameraUnavailable`, `ProcessingFailed` |


## Design Philosophy

This crate intentionally uses the **system camera UI** rather than providing a custom
camera preview. This design choice offers:

- **Familiar UX**: Users see the same camera interface they use in other apps
- **Automatic permissions**: The system handles permission prompts and settings
- **Reliability**: No need to handle camera hardware quirks across devices
- **Simplicity**: Minimal API surface, easy to integrate

For use cases requiring a custom camera viewfinder, frame streaming, or advanced camera
controls, consider using platform-specific camera APIs directly or a crate like
[`nokhwa`](https://crates.io/crates/nokhwa) (desktop only).


## Future Considerations

Features that may be added in future versions:

- **macOS/Linux support**: Direct camera access via AVFoundation (macOS) and V4L2 (Linux)
  for platforms without a system camera UI
- **Frame streaming**: Continuous frame callbacks for live preview
- **Video capture**: Record videos using the system camera UI
- **Gallery picking**: Select existing photos/videos from the device gallery
- **Photo settings**: Flash mode, aspect ratio, resolution preferences
- **Permission APIs**: Explicit permission request and status checking


## License

Licensed under the MIT License. See [LICENSE](LICENSE) for details.
