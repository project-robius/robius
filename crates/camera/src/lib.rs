//! This crate provides Rust interfaces to capture photos using the system camera UI.
//!
//! Instead of implementing a custom camera view, this crate opens the native system
//! camera interface (similar to tapping "take photo" in the Photos app), letting the
//! system handle the UI, permissions, and capture experience. The resulting image
//! is returned to your app as JPEG data.
//!
//! ## Supported Platforms
//!
//! - **iOS**: Uses `UIImagePickerController` with the camera source type
//! - **Android**: Uses `Intent` with `ACTION_IMAGE_CAPTURE`
//! - **Windows**: Uses `CameraCaptureUI`
//!
//! ## Examples
//!
//! ```rust,no_run
//! use robius_camera::{capture_photo, CameraPosition};
//!
//! // Capture a photo using the back camera
//! capture_photo(CameraPosition::Back, |result| {
//!     match result {
//!         Ok(photo) => {
//!             let jpeg_data = photo.into_jpeg_data();
//!             println!("Captured photo: {} bytes", jpeg_data.len());
//!         }
//!         Err(robius_camera::Error::Cancelled) => {
//!             println!("User cancelled");
//!         }
//!         Err(e) => {
//!             eprintln!("Error: {:?}", e);
//!         }
//!     }
//! }).expect("failed to open camera");
//! ```
//!
//! ## Platform Requirements
//!
//! ### iOS
//!
//! Add the following to your `Info.plist`:
//! ```xml
//! <key>NSCameraUsageDescription</key>
//! <string>This app needs camera access to take photos.</string>
//! ```
//!
//! ### Android
//!
//! Add the following to your `AndroidManifest.xml`:
//! ```xml
//! <uses-feature android:name="android.hardware.camera" android:required="false" />
//! <uses-permission android:name="android.permission.CAMERA" />
//! ```

#![allow(clippy::result_unit_err)]
#![deny(missing_docs)]

mod error;
mod sys;

pub use error::{Error, Result};

/// Specifies which camera to use for capture.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum CameraPosition {
    /// Use the rear-facing (back) camera.
    #[default]
    Back,
    /// Use the front-facing camera.
    Front,
}

/// Contains the captured photo data.
///
/// The photo is stored as JPEG-encoded data.
pub struct PhotoData {
    jpeg_data: Vec<u8>,
    width: u32,
    height: u32,
}

impl PhotoData {
    /// Creates a new `PhotoData` from the given JPEG data and dimensions.
    #[allow(dead_code)] // Used by platform-specific implementations
    pub(crate) fn new(jpeg_data: Vec<u8>, width: u32, height: u32) -> Self {
        Self {
            jpeg_data,
            width,
            height,
        }
    }

    /// Returns a reference to the JPEG-encoded image data.
    pub fn jpeg_data(&self) -> &[u8] {
        &self.jpeg_data
    }

    /// Returns the width of the captured image in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Returns the height of the captured image in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Consumes this `PhotoData` and returns the JPEG data.
    pub fn into_jpeg_data(self) -> Vec<u8> {
        self.jpeg_data
    }
}

impl std::fmt::Debug for PhotoData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PhotoData")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("jpeg_data_len", &self.jpeg_data.len())
            .finish()
    }
}

/// Opens the system camera UI to capture a photo.
///
/// This function presents the native camera interface to the user. Once the user
/// captures a photo (or cancels), the provided callback is invoked with the result.
///
/// # Arguments
///
/// * `position` - Which camera to use (front or back)
/// * `callback` - Called with the capture result when complete
///
/// # Returns
///
/// Returns `Ok(())` if the camera UI was successfully presented, or an error if
/// the camera could not be opened.
///
/// # Platform Behavior
///
/// - **iOS**: Must be called from the main thread. Uses `UIImagePickerController`.
/// - **Android**: Uses `ACTION_IMAGE_CAPTURE` intent.
/// - **Windows**: Uses `CameraCaptureUI`.
///
/// # Examples
///
/// ```rust,no_run
/// use robius_camera::{capture_photo, CameraPosition};
///
/// capture_photo(CameraPosition::Front, |result| {
///     if let Ok(photo) = result {
///         println!("Captured {}x{} photo", photo.width(), photo.height());
///     }
/// }).expect("failed to open camera");
/// ```
pub fn capture_photo<F>(position: CameraPosition, callback: F) -> Result<()>
where
    F: FnOnce(Result<PhotoData>) + Send + 'static,
{
    sys::capture_photo(position, callback)
}

/// Returns whether camera capture is available on this device.
///
/// This checks if the device has camera hardware and the camera source type
/// is available for use.
///
/// # Examples
///
/// ```rust,no_run
/// use robius_camera::is_available;
///
/// if is_available() {
///     println!("Camera is available");
/// } else {
///     println!("Camera is not available on this device");
/// }
/// ```
#[must_use]
pub fn is_available() -> bool {
    sys::is_available()
}
