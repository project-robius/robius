use std::marker::PhantomData;

use crate::{Error, Result};

pub(crate) struct Uri<'a, 'b> {
    phantom: PhantomData<(&'a (), &'b ())>,
}

impl<'a, 'b> Uri<'a, 'b> {
    pub(crate) fn new(_: &'a str) -> Self {
        Self {
            phantom: PhantomData,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        false
    }

    pub fn action(self, _: &'b str) -> Self {
        self
    }

    pub fn open(self) -> Result<()> {
        #[cfg(feature = "log")]
        log::error!("Failed to open URI; this platform is unsupported.");
        Err(Error::Unknown)
    }
}
