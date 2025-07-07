pub type Result<T> = std::result::Result<T, Error>;

/// An error that can occur when fetching the location.
#[derive(Copy, Clone, Debug)]
pub enum Error {
    /// An error occured with the Android Java environment.
    AndroidEnvironment,
    /// The user denied authorization.
    AuthorizationDenied,
    /// A network error occured.
    Network,
    /// The function was not called from the main thread.
    NotMainThread,
    /// Location data is temporarily unavailable.
    TemporarilyUnavailable,
    /// This device does not support location data.
    PermanentlyUnavailable,
    /// An unknown error occured.
    Unknown,
}
