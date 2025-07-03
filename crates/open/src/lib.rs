//! This crate provides easy Rust interfaces to open URIs across multiple platforms.
//!
//! Supports:
//! - macOS (via `NSWorkspace`)
//! - Android (via `android/content/Intent`)
//! - Linux (via `xdg-open`)
//! - Windows (via `start`)
//! - iOS (via `UIApplication`)
//! 
//! URIs take many different forms: URLs (`http://`), `tel:`, `mailto:`, `file://`,
//! and more (see the [official list of schemes](https://www.iana.org/assignments/uri-schemes/uri-schemes.xhtml)).
//! 
//! ## Examples
//! 
//! ```rust
//! use robius_open::Uri;
//! Uri::new("tel:+61 123 456 789")
//!    .open()
//!    .expect("failed to open telephone URI");
//! ```
//! 
//! ```rust
//! use robius_open::Uri;
//! Uri::new("http://www.google.com")
//!    .open_with_completion(|success| {
//!       log!("Opened URI? {success}");
//!    })
//!    .expect("failed to open URL");
//! ```
//! 
//! 
//! ## Android usage
//! To use this crate on Android with the default `android-result` feature enabled,
//! you must add the following to your app manifest:
//! ```xml
//! <uses-permission android:name="android.permission.QUERY_ALL_PACKAGES"
//!    tools:ignore="QueryAllPackagesPermission" />
//! 
//! <queries>
//!    <intent>
//!       <action android:name="android.intent.action.MAIN" />
//!    </intent>
//! </queries>
//! ```
//! 
//! Alternatively, you can omit those permissions if you disable the `android-result` feature,
//! but that will then cause `Uri::open()` to always return `Ok`
//! (and the `on_completion` closure to always receive a success value of `true`)
//! regardless of whether the URI was actually opened successfully.

#![allow(clippy::result_unit_err)]

mod error;
mod sys;

pub use error::{Error, Result};

/// A uniform resource identifier.
pub struct Uri<'a, 'b> {
    inner: sys::Uri<'a, 'b>,
}

impl<'a, 'b> Uri<'a, 'b> {
    /// Constructs a new URI.
    pub fn new(s: &'a str) -> Self {
        Self {
            inner: sys::Uri::new(s),
        }
    }

    /// Sets the action to perform with this URI.
    ///
    /// This only has an effect on Android, and corresponds to an [action
    /// activity][aa]. By default, it is set to `"ACTION_VIEW"`.
    ///
    /// # Examples
    ///
    /// ```
    /// # use robius_open::Uri;
    /// Uri::new("tel:+61 123 456 789")
    ///     .action("ACTION_DIAL")
    ///     .open()
    ///     .expect("failed to open telephone URI");
    /// ```
    ///
    /// [aa]: https://developer.android.com/reference/android/content/Intent#standard-activity-actions
    pub fn action(self, action: &'b str) -> Self {
        Self {
            inner: self.inner.action(action),
        }
    }

    /// Opens this URI.
    ///
    /// This must be called on the main UI thread, or it will return [Error::NotMainThread].
    ///
    /// Note that the returned `Result` does not necessarily indicate whether
    /// the URI was successfully opened by the system.
    /// For that purpose, you should use [`Self::open_with_completion()`].
    pub fn open(self) -> Result<()> {
        self.open_with_completion(|_success| {
            #[cfg(feature = "log")]
            log::debug!("Uri::open(): called on_completion closure, success: {}", _success);
        })
    }

    /// Opens this URI, with a callback for determining if the URI was successfully opened.
    ///
    /// This must be called on the main UI thread, or it will return [Error::NotMainThread].
    ///
    /// Note that the returned `Result` does not *necessarily* indicate whether
    /// the URI was successfully opened by the system.
    /// For that purpose, the given `on_completion` callback will be called
    /// with a boolean indicating whether the URI was successfully opened.
    /// Note that the callback may be not be called at all,
    /// but should typically be called upon success.
    ///
    /// Thus, the URI was *not* successfully opened if this function returns an error,
    /// **OR** if the `on_completion` callback is invoked with `false`.
    pub fn open_with_completion<F>(self, on_completion: F) -> Result<()>
    where
        F: Fn(bool) + Send + 'static,
    {
        // Passing an empty URI can cause your app to be killed on certain platforms.
        if self.inner.is_empty() {
            #[cfg(feature = "log")]
            log::error!("Error: cannot open an empty URI.");

            return Err(Error::MalformedUri);
        }
        self.inner.open(on_completion)
    }
}
