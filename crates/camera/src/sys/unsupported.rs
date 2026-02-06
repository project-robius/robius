use crate::{CameraPosition, Error, PhotoData, Result};

pub(crate) fn capture_photo<F>(_position: CameraPosition, callback: F) -> Result<()>
where
    F: FnOnce(Result<PhotoData>) + Send + 'static,
{
    #[cfg(feature = "log")]
    log::error!("Failed to capture photo; this platform is unsupported.");
    callback(Err(Error::Unsupported));
    Err(Error::Unsupported)
}

pub(crate) fn is_available() -> bool {
    false
}
