pub type Result<T> = std::result::Result<T, Error>;

/// Errors encountered when showing a native share sheet.
#[derive(Debug)]
pub enum Error {
    /// Couldn't acquire the Android environment.
    ///
    /// See the `robius-android-env` crate for more details.
    AndroidEnvironment,
    #[cfg(target_os = "android")]
    Java(jni::errors::Error),
    #[cfg(target_os = "windows")]
    Windows(windows::core::Error),
    /// The share sheet wasn't started from the main UI thread, as is required.
    NotMainThread,
    /// A file attachment or other filesystem operation failed.
    Io(std::io::Error),
    /// The share sheet has no text, URL, or attachment to share.
    Empty,
    /// A URL, URI, file path, or MIME type was malformed or otherwise invalid.
    InvalidItem,
    /// No app or system service is available to receive this share.
    NoHandler,
    /// The native UI is already showing a share sheet.
    /// The user must dismiss that one and try again.
    AlreadyOpen,
    /// The platform supports sharing, but not the given share item.
    UnsupportedItem,
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

#[cfg(target_os = "windows")]
impl From<windows::core::Error> for Error {
    fn from(value: windows::core::Error) -> Self {
        Self::Windows(value)
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
            #[cfg(target_os = "windows")]
            Error::Windows(err) => write!(f, "Windows API error: {err}"),
            Error::NotMainThread => f.write_str("must be called from the main UI thread"),
            Error::Io(err) => write!(f, "I/O error: {err}"),
            Error::Empty => f.write_str("nothing to share"),
            Error::InvalidItem => f.write_str("invalid share item"),
            Error::NoHandler => f.write_str("no app is available to receive this share"),
            Error::AlreadyOpen => f.write_str("another share sheet is already open"),
            Error::UnsupportedItem => f.write_str("this share item is unsupported on the current platform"),
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
            #[cfg(target_os = "windows")]
            Error::Windows(err) => Some(err),
            _ => None,
        }
    }
}
