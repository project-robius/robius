//! Native OS-managed web authentication sessions.
//!
//! See the crate-level README for motivation. The short version: this crate
//! wraps the OS APIs that present an in-app browser sheet for OAuth / SSO
//! flows and route the post-login redirect back to your app via a callback,
//! without needing a local HTTP server or globally-registered URL scheme.
//!
//! Currently supported on:
//! - **iOS**, backed by [`ASWebAuthenticationSession`].
//!
//! All other platforms return [`Error::Unsupported`] for now.
//!
//! [`ASWebAuthenticationSession`]: https://developer.apple.com/documentation/authenticationservices/aswebauthenticationsession

#![allow(clippy::result_unit_err)]

mod error;
mod sys;

pub use error::{Error, Result};

/// Configures and starts a web authentication session.
pub struct AuthSession<'a> {
    url: &'a str,
    callback_scheme: &'a str,
    prefers_ephemeral: bool,
}

impl<'a> AuthSession<'a> {
    /// Build a session for `url`, expecting the IdP to redirect back to a
    /// `<callback_scheme>://...` URL (no `://` in the scheme arg).
    pub fn new(url: &'a str, callback_scheme: &'a str) -> Self {
        Self {
            url,
            callback_scheme,
            prefers_ephemeral: false,
        }
    }

    /// Don't share cookies with Safari. Off by default, so users can take
    /// advantage of an existing Google/etc. login on the device.
    pub fn prefers_ephemeral_web_browser_session(mut self, prefers_ephemeral: bool) -> Self {
        self.prefers_ephemeral = prefers_ephemeral;
        self
    }

    /// Start the auth session. Must be called from the main UI thread.
    ///
    /// `on_completion` fires once on main with either the callback URL or an
    /// [`Error`]. The returned handle lets you cancel.
    pub fn start<F>(self, on_completion: F) -> Result<AuthSessionHandle>
    where
        F: FnOnce(Result<String>) + Send + 'static,
    {
        if self.url.is_empty() {
            return Err(Error::MalformedUri);
        }
        if self.callback_scheme.is_empty() {
            return Err(Error::MalformedUri);
        }
        sys::start(self.url, self.callback_scheme, self.prefers_ephemeral, on_completion)
            .map(|inner| AuthSessionHandle { inner })
    }
}

/// Handle to a running auth session, returned by [`AuthSession::start`].
/// Cheap to clone since it just bumps a ref count internally.
#[derive(Clone)]
pub struct AuthSessionHandle {
    inner: sys::Handle,
}

impl AuthSessionHandle {
    /// Cancel the session. The completion callback fires with [`Error::UserCancelled`].
    /// No-op if the session has already finished. Safe to call from any thread.
    pub fn cancel(&self) {
        self.inner.cancel();
    }
}
