use std::marker::PhantomData;
use crate::{Result, Error};

use windows::{
    core::HSTRING,
    Foundation,
    System::Launcher,
};


pub(crate) struct Uri<'a, 'b> {
    inner: &'a str,
    phantom: PhantomData<&'b ()>,
}

impl<'a, 'b> Uri<'a, 'b> {
    pub(crate) fn new(inner: &'a str) -> Self {
        Self {
            inner,
            phantom: PhantomData,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    pub fn action(self, _: &'b str) -> Self {
        self
    }

    pub fn open<F>(self, on_completion: F) -> Result<()>
    where
        F: Fn(bool) + 'static,
    {
        let win_uri = Foundation::Uri::CreateUri(&HSTRING::from(self.inner))
            .map_err(|_| Error::MalformedUri)?;

        let async_iop = Launcher::LaunchUriAsync(&win_uri).map_err(|_e| {
            #[cfg(feature = "log")]
            log::error!("Failed to call LaunchUriAsync; error: {_e}.");
            crate::Error::Unknown
        })?;
        match async_iop.get() {
            Ok(success) => {
                on_completion(success);
                success.then_some(()).ok_or(Error::NoHandler)
            }
            Err(_e) => {
                #[cfg(feature = "log")]
                log::error!("Failed to open URI. Error: {_e}.");
                Err(Error::Unknown)
            }
        }
    }
}
