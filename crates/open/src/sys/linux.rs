use std::{marker::PhantomData, process::{Command, Stdio}};

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
        F: Fn(bool) + Send + 'static,
    {
        let child = Command::new("xdg-open")
            .arg(self.inner)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();

        match child {
            Ok(mut child) => {
                // Spawn a thread so we don't block the caller waiting on
                // the result of the xdg-open child process.
                std::thread::spawn(move || {
                    let success = matches!(child.wait(), Ok(status) if status.success());
                    #[cfg(feature = "log")]
                    if !success {
                        log::error!("`xdg-open` did not open the URI successfully");
                    }
                    on_completion(success);
                });
                Ok(())
            }
            Err(e) => {
                #[cfg(feature = "log")]
                log::error!("Failed to launch `xdg-open`; error: {e}");
                Err(if e.kind() == std::io::ErrorKind::NotFound {
                    Error::NoHandler
                } else {
                    Error::Unknown
                })
            }
        }
    }
}
