/// A specialized [`Result`](std::result::Result) type for camera operations.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors encountered when capturing a photo.
#[derive(Debug)]
pub enum Error {
    /// Could not acquire the Android environment.
    ///
    /// See the `robius-android-env` crate for more details.
    #[cfg(target_os = "android")]
    AndroidEnvironment(robius_android_env::Error),

    /// A JNI error occurred on Android.
    #[cfg(target_os = "android")]
    Java(jni::errors::Error),

    /// Camera hardware is not available on this device.
    CameraUnavailable,

    /// The requested camera (front/back) is not available.
    RequestedCameraUnavailable,

    /// User cancelled the capture operation.
    Cancelled,

    /// Camera permission was denied by the user.
    PermissionDenied,

    /// Failed to process the captured image (e.g., JPEG conversion failed).
    ProcessingFailed,

    /// The operation must be called on the main UI thread.
    NotMainThread,

    /// This platform is not supported.
    Unsupported,

    /// An unknown error occurred.
    Unknown,
}

#[cfg(target_os = "android")]
impl From<jni::errors::Error> for Error {
    fn from(value: jni::errors::Error) -> Self {
        Self::Java(value)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(target_os = "android")]
            Error::AndroidEnvironment(e) => write!(f, "could not acquire Android environment: {:?}", e),
            #[cfg(target_os = "android")]
            Error::Java(e) => write!(f, "JNI error: {}", e),
            Error::CameraUnavailable => write!(f, "camera hardware is not available"),
            Error::RequestedCameraUnavailable => write!(f, "requested camera is not available"),
            Error::Cancelled => write!(f, "user cancelled the capture"),
            Error::PermissionDenied => write!(f, "camera permission denied"),
            Error::ProcessingFailed => write!(f, "failed to process captured image"),
            Error::NotMainThread => write!(f, "must be called on main thread"),
            Error::Unsupported => write!(f, "platform not supported"),
            Error::Unknown => write!(f, "unknown error"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            #[cfg(target_os = "android")]
            Error::AndroidEnvironment(e) => Some(e),
            #[cfg(target_os = "android")]
            Error::Java(e) => Some(e),
            _ => None,
        }
    }
}
