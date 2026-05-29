pub type Result<T> = std::result::Result<T, Error>;

/// Errors encountered when showing a native file dialog/picker.
#[derive(Debug)]
pub enum Error {
    /// Couldn't acquire the android environment.
    ///
    /// See the `robius-android-env` crate for more details.
    AndroidEnvironment,
    #[cfg(target_os = "android")]
    Java(jni::errors::Error),
    /// The wasn't started from the main UI thread, as is required.
    NotMainThread,
    /// A temporary file or other filesystem operation failed.
    Io(std::io::Error),
    /// The provided file name isn't valid on the current platform.
    InvalidFileName,
    /// Another dialog/picker is already open. Only one can be shown at a time.
    AlreadyOpen,
    /// This platform is unsupported.
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

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::AndroidEnvironment => f.write_str("couldn't access the Android/Java environment"),
            #[cfg(target_os = "android")]
            Error::Java(err) => write!(f, "Java error: {err}"),
            Error::NotMainThread => f.write_str("must be called from the main UI thread"),
            Error::Io(err) => write!(f, "I/O error: {err}"),
            Error::InvalidFileName => f.write_str("invalid file name"),
            Error::AlreadyOpen => f.write_str("another file picker is already open"),
            Error::Unsupported => f.write_str("this platform is unsupported"),
            Error::Unknown => f.write_str("unknown error"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(err) => Some(err),
            #[cfg(target_os = "android")]
            Error::Java(err) => Some(err),
            _ => None,
        }
    }
}
