//! Abstractions for multi-platform native authentication.
//!
//! This crate supports:
//! - Apple: TouchID, FaceID, and regular username/password on macOS and iOS.
//! - Android: See below for additional steps.
//!   - Requires the `USE_BIOMETRIC` permission in your app's manifest.
//! - Windows: Windows Hello (face recognition, fingerprint, PIN), plus
//!   winrt-based fallback for username/password.
//! - Linux: [`polkit`]-based authentication using the desktop environment's
//!   prompt.
//!   - **Note: Linux support is currently incomplete.**
//!
//! # Example
//!
//! ```no_run
//! use robius_authentication::{
//!     AndroidText, BiometricStrength, Context, Policy, PolicyBuilder, Text, WindowsText,
//! };
//!
//! let policy: Policy = PolicyBuilder::new()
//!     .biometrics(Some(BiometricStrength::Strong))
//!     .password(true)
//!     .companion(true)
//!     .build()
//!     .unwrap();
//!
//! let text = Text {
//!     android: AndroidText {
//!         title: "Title",
//!         subtitle: None,
//!         description: None,
//!     },
//!     apple: "authenticate",
//!     windows: WindowsText::new_truncated("Title", "Description"),
//! };
//!
//! let callback = |auth_result| {
//!     match auth_result {
//!         Ok(_)  => log::info!("Authentication success!"),
//!         Err(_) => log::error!(Authentication failed!"),
//!     }
//! };
//!
//! Context::new(())
//!     .authenticate(text, &policy, callback)
//!     .expect("authentication failed");
//! ```
//!
//! The `Policy` and `Text` structs can also be constructed at compile-time to
//! avoid run-time unwraps:
//! ```
//! #![feature(const_option)]
//!
//! use robius_authentication::{
//!     AndroidText, BiometricStrength, Policy, PolicyBuilder, Text, WindowsText,
//! };
//!
//! const POLICY: Policy = PolicyBuilder::new()
//!     .biometrics(Some(BiometricStrength::Strong))
//!     .password(true)
//!     .companion(true)
//!     .build()
//!     .unwrap();
//!
//! const TEXT: Text = Text {
//!     android: AndroidText {
//!         title: "Title",
//!         subtitle: None,
//!         description: None,
//!     },
//!     apple: "authenticate",
//!     windows: WindowsText::new("Title", "Description").unwrap(),
//! };
//! ```
//!
//! For more details about the prompt text see the [`Text`] struct
//! which allows you to customize the prompt for each platform.
//!
//! ## Usage on Android
//!
//! For authentication to work, the following must be added to your app's
//! `AndroidManifest.xml`:
//! ```xml
//! <uses-permission android:name="android.permission.USE_BIOMETRIC" />
//! ```
//!
//! [`polkit`]: https://www.freedesktop.org/software/polkit/docs/latest/polkit.8.html

mod error;
mod sys;
mod text;

pub use crate::{
    error::{Error, Result},
    text::{AndroidText, Text, WindowsText},
};

/// A "raw" context that can be used to create a [`Context`].
///
/// Currently, all platforms define this as the void type `()`.
pub type RawContext = sys::RawContext;

/// Holds platform-specific contextual state required to display an authentication prompt.
#[derive(Debug)]
pub struct Context {
    inner: sys::Context,
}

impl Context {
    /// Creates a new context from the given "raw" context.
    #[inline]
    pub fn new(raw: RawContext) -> Self {
        Self {
            inner: sys::Context::new(raw),
        }
    }

    // Async authentication functions are currently not implemented. 
    //
    // /// Authenticates using the provided policy and message.
    // ///
    // /// Returns whether the authentication was successful.
    // #[inline]
    // #[cfg(feature = "async")]
    // pub async fn authenticate_async(
    //     &self,
    //     message: Text<'_, '_, '_, '_, '_, '_>,
    //     policy: &Policy,
    // ) -> Result<()> {
    //     self.inner.authenticate(message, &policy.inner).await
    // }

    /// Displays an authentication prompt using the provided policy and message.
    ///
    /// Note that the returned `Result` does not indicate whether
    /// authentication was successful. This function returns `Ok(())`
    /// to indicate that the authentication prompt was successfully displayed,
    /// not that the user successfully authenticated.
    ///
    /// For that purpose, the given `callback` will be called
    /// with a Result indicating whether authentication succeeded.
    /// Note that the callback may be not be called at all,
    /// but will always be called upon success.
    ///
    /// Thus, authentication failed if this function returns an error
    /// **OR** if the `callback` is invoked with `Err(_)`.
    #[inline]
    pub fn authenticate<F>(
        &self,
        message: Text,
        policy: &Policy,
        callback: F,
    ) -> Result<()>
    where
        F: Fn(Result<()>) + Send + 'static,
    {
        self.inner.authenticate(message, &policy.inner, callback)
    }
}

/// A biometric strength class.
///
/// This only has an effect on Android. On other targets, any biometric strength
/// setting will enable all biometric authentication devices. See the [Android
/// documentation][android-docs] for more details.
///
/// [android-docs]: https://source.android.com/docs/security/features/biometric
#[derive(Debug)]
pub enum BiometricStrength {
    Strong,
    Weak,
}

/// A builder for conveniently defining a policy.
///
/// It is **highly recommended** to use the [`Self::new()`] (default) value
/// to create a policy with all options enabled, because each platform acts differently
/// when being requested to enable/disable various authentication methods.
/// Enabling all options is the safest way to ensure that the authentication prompt
/// will be displayed correctly on all platforms.
#[derive(Debug)]
pub struct PolicyBuilder {
    inner: sys::PolicyBuilder,
}

impl Default for PolicyBuilder {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl PolicyBuilder {
    /// Returns a new policy with sane defaults.
    #[inline]
    pub const fn new() -> Self {
        Self {
            inner: sys::PolicyBuilder::new(),
        }
    }

    /// Configures biometric authentication with the given strength.
    ///
    /// The strength only has an effect on Android, see [`BiometricStrength`]
    /// for more details.
    #[inline]
    #[must_use]
    pub const fn biometrics(self, strength: Option<BiometricStrength>) -> Self {
        Self {
            inner: self.inner.biometrics(strength),
        }
    }

    /// Sets whether the policy supports passwords.
    #[inline]
    #[must_use]
    pub const fn password(self, password: bool) -> Self {
        Self {
            inner: self.inner.password(password),
        }
    }

    /// Sets whether the policy supports authentication via a proximity companion device, e.g., Apple Watch.
    ///
    /// This only has an effect on iOS and macOS.
    #[inline]
    #[must_use]
    pub const fn companion(self, companion: bool) -> Self {
        Self {
            inner: self.inner.companion(companion),
        }
    }

    /// Sets whether the policy requires the companion device (Apple Watch) to be on the user's wrist.
    ///
    /// This only has an effect on Apple watchOS.
    #[inline]
    #[must_use]
    pub const fn wrist_detection(self, wrist_detection: bool) -> Self {
        Self {
            inner: self.inner.wrist_detection(wrist_detection),
        }
    }

    /// Constructs the policy.
    ///
    /// Returns `None` if the specified configuration is not valid for the
    /// current target.
    #[inline]
    #[must_use]
    pub const fn build(self) -> Option<Policy> {
        Some(Policy {
            inner: match self.inner.build() {
                Some(inner) => inner,
                None => return None,
            },
        })
    }
}

/// An authentication policy.
#[derive(Debug)]
pub struct Policy {
    inner: sys::Policy,
}
