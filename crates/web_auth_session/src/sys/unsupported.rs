use crate::{Error, Result};

#[derive(Clone)]
pub(crate) struct Handle;

impl Handle {
    pub(crate) fn cancel(&self) {}
}

pub(crate) fn start<F>(
    _url: &str,
    _callback_scheme: &str,
    _prefers_ephemeral: bool,
    on_completion: F,
) -> Result<Handle>
where
    F: FnOnce(Result<String>) + Send + 'static,
{
    on_completion(Err(Error::Unsupported));
    Err(Error::Unsupported)
}
