use crate::{Error, ProxyState, Result};

pub(crate) struct Manager;

impl Manager {
    pub(crate) fn new() -> Result<Self> {
        Err(Error::Unsupported)
    }

    pub(crate) fn current(&self) -> Result<ProxyState> {
        Err(Error::Unsupported)
    }

    pub(crate) fn apply(&self, _state: ProxyState) -> Result<()> {
        Err(Error::Unsupported)
    }
}
