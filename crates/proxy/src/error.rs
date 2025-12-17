use std::{fmt, io};

/// Result alias for the proxy crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors that can arise while interacting with system proxy settings.
#[derive(Debug)]
pub enum Error {
    /// The operation is not supported on this platform.
    Unsupported,
    /// Input data was invalid for the target platform.
    InvalidInput(&'static str),
    /// A command failed to execute successfully.
    CommandFailed {
        command: String,
        stderr: String,
    },
    /// Parsing system proxy data failed.
    Parse(String),
    /// I/O failure while invoking platform facilities.
    Io(io::Error),
}

impl From<io::Error> for Error {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported => write!(f, "operation not supported on this platform"),
            Self::InvalidInput(reason) => write!(f, "invalid proxy input: {reason}"),
            Self::CommandFailed { command, stderr } => {
                write!(f, "command `{command}` failed: {stderr}")
            }
            Self::Parse(reason) => write!(f, "failed to parse proxy data: {reason}"),
            Self::Io(e) => write!(f, "io error: {e}"),
        }
    }
}

impl std::error::Error for Error {}
