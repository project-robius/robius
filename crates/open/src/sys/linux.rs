use std::{marker::PhantomData, process::Command};

use crate::{Error, Result};

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
        match Command::new("xdg-open").arg(self.inner).status() {
            Ok(status) => {
                let success = status.success();
                on_completion(success);
                if success {
                    return Ok(());
                } else {
                    #[cfg(feature = "log")]
                    log::error!("Failed to open URI, `xdg-open` command returned {status}");
                }
            }
            Err(_e) => {
                #[cfg(feature = "log")]
                log::error!("Failed to open URI via `xdg-open`; error: {_e}");
            }
        }
        Err(Error::Unknown)
    }
}
