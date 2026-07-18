pub type Result<T> = std::result::Result<T, Error>;

/// Errors encountered when showing or managing system notifications.
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
    #[cfg(target_os = "linux")]
    DBus(zbus::Error),
    /// A notification image or other filesystem operation failed.
    Io(std::io::Error),
    /// The notification has no title and no body.
    Empty,
    /// A notification id, action, channel, or other field was malformed
    /// or otherwise invalid (e.g., empty or duplicate action ids).
    InvalidNotification,
    /// The user or system hasn't permitted this app to show notifications.
    ///
    /// Use [`request_permission`](crate::request_permission) to ask for permission first.
    PermissionDenied,
    /// The app isn't running from a proper app bundle, so the OS has nowhere
    /// to attribute (or route interactions of) its notifications.
    ///
    /// This mainly happens on macOS when running a bare binary (e.g., via
    /// `cargo run`) instead of a bundled `.app`.
    NoAppBundle,
    /// An interaction handler has already been set; only one is allowed.
    HandlerAlreadySet,
    /// No notification service is available to show notifications,
    /// e.g., no notification daemon is running on Linux.
    NoService,
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

#[cfg(target_os = "linux")]
impl From<zbus::Error> for Error {
    fn from(value: zbus::Error) -> Self {
        Self::DBus(value)
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
            #[cfg(target_os = "linux")]
            Error::DBus(err) => write!(f, "D-Bus error: {err}"),
            Error::Io(err) => write!(f, "I/O error: {err}"),
            Error::Empty => f.write_str("the notification has no title or body"),
            Error::InvalidNotification => f.write_str("invalid notification field"),
            Error::PermissionDenied => f.write_str("no permission to show notifications"),
            Error::NoAppBundle => f.write_str("the app isn't running from an app bundle"),
            Error::HandlerAlreadySet => f.write_str("an interaction handler was already set"),
            Error::NoService => f.write_str("no notification service is available"),
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
            #[cfg(target_os = "linux")]
            Error::DBus(err) => Some(err),
            _ => None,
        }
    }
}
