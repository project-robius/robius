pub type Result<T> = std::result::Result<T, Error>;

/// An error that can occur when fetching the location.
#[derive(Copy, Clone, Debug)]
pub enum Error {
    /// An error occurred with the Android Java environment.
    AndroidEnvironment,
    /// The user denied authorization.
    AuthorizationDenied,
    /// A network error occurred.
    Network,
    /// The function was not called from the main thread.
    NotMainThread,
    /// Location data is temporarily unavailable.
    TemporarilyUnavailable,
    /// Location is unsupported, or a required platform service or interface is unavailable.
    PermanentlyUnavailable,
    /// An unknown error occurred.
    Unknown,
}
