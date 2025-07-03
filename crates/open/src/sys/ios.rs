use std::marker::PhantomData;

use block2::RcBlock;
use dispatch2::run_on_main;
use objc2_foundation::{NSDictionary, NSString, NSURL};
use objc2::runtime::Bool;
use objc2_ui_kit::UIApplication;


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
        // iOS requires that we call openURL on the main thread.
        run_on_main(move |mtm| {
            let string = NSString::from_str(self.inner);
            let url = unsafe { NSURL::URLWithString(&string) }
                .ok_or(Error::MalformedUri)?;
            let application = UIApplication::sharedApplication(mtm);
    
            let block = RcBlock::new(move |success: Bool| {
                #[cfg(feature = "log")]
                log::trace!("Calling on_completion closure with success: {}", success.as_raw());
                on_completion(success.into());
            });

            unsafe {
                application.openURL_options_completionHandler(
                    &url,
                    &NSDictionary::new(), // no options used currently.
                    Some(&block),
                );
            }
            Ok(())
        })
    }
}
