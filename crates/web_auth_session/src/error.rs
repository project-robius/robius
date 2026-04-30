pub type Result<T> = std::result::Result<T, Error>;

/// Errors that can occur when starting or running an auth session.
#[derive(Debug)]
pub enum Error {
    /// The provided authorization URL was malformed or otherwise invalid.
    MalformedUri,
    /// The session must be started from the main UI thread, but was started
    /// from a different thread.
    NotMainThread,
    /// The user cancelled the authentication session, or dismissed the
    /// presented sheet without completing it.
    UserCancelled,
    /// The OS reported an authentication error.
    ///
    /// On iOS this maps to a non-`canceledLogin` `ASWebAuthenticationSessionError`.
    AuthFailed(String),
    /// This platform does not yet have a `robius-web-auth-session` backend.
    Unsupported,
    /// An unknown error occurred.
    Unknown,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::MalformedUri => f.write_str("malformed authorization URL"),
            Error::NotMainThread => f.write_str("must be called from the main UI thread"),
            Error::UserCancelled => f.write_str("user cancelled the authentication session"),
            Error::AuthFailed(msg) => write!(f, "authentication failed: {msg}"),
            Error::Unsupported => f.write_str("web authentication sessions are not supported on this platform"),
            Error::Unknown => f.write_str("unknown error"),
        }
    }
}

impl std::error::Error for Error {}
