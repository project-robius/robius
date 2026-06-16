use std::path::{Path, PathBuf};

/// The platform-specific location of a file-like item (a file or a content URI).
///
/// Most platforms expose selected or shareable files as normal fs paths,
/// but Android exposes them as platform URIs (like `content://...`),
/// so this type abstracts that away.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum FileLocation {
    /// A normal filesystem path.
    Path(PathBuf),
    /// A platform URI, such as an Android `content://` URI.
    Uri(String),
}

impl FileLocation {
    /// Creates a file location from a filesystem path.
    pub fn from_path<P: AsRef<Path>>(path: P) -> Self {
        Self::Path(path.as_ref().to_owned())
    }

    /// Creates a file location from a platform URI.
    pub fn from_uri(uri: impl Into<String>) -> Self {
        Self::Uri(uri.into())
    }

    /// Returns this location as a filesystem path, if it is one.
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Path(path) => Some(path),
            Self::Uri(_) => None,
        }
    }

    /// Returns this location as a platform URI, if it is one.
    pub fn uri(&self) -> Option<&str> {
        match self {
            Self::Path(_) => None,
            Self::Uri(uri) => Some(uri),
        }
    }

    /// Consumes this location and returns its filesystem path, if it is one.
    pub fn into_path(self) -> Option<PathBuf> {
        match self {
            Self::Path(path) => Some(path),
            Self::Uri(_) => None,
        }
    }

    /// Consumes this location and returns its platform URI, if it is one.
    pub fn into_uri(self) -> Option<String> {
        match self {
            Self::Path(_) => None,
            Self::Uri(uri) => Some(uri),
        }
    }
}
