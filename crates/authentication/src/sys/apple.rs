use block2::RcBlock;
use objc2_foundation::{NSError, NSString};
use objc2_local_authentication::{LAContext, LAError, LAPolicy};
// #[cfg(feature = "async")]
// use tokio::sync::oneshot as channel_impl;

use crate::{BiometricStrength, Error, Result, Text};

pub(crate) type RawContext = ();

#[derive(Debug)]
pub(crate) struct Context;

impl Context {
    pub(crate) fn new(_: RawContext) -> Self {
        Self
    }
    // TODO: Fix the async authenticate function
    //
    // #[cfg(feature = "async")]
    // pub(crate) async fn authenticate_async(
    //     &self,
    //     text: Text<'_, '_, '_, '_, '_, '_>,
    //     policy: &Policy,
    // ) -> Result<()> {
    //     // The callback should always execute and hence a message will always be sent.
    //     self.authenticate_inner(text, policy).await.unwrap()
    // }

    pub(crate) fn authenticate<F>(
        &self,
        text: Text,
        policy: &Policy,
        callback: F,
    ) -> Result<()>
    where
        F: Fn(Result<()>) + Send + 'static,
    {
        self.authenticate_inner(text, policy, callback)
    }

    fn authenticate_inner<F>(
        &self,
        text: Text<'_, '_, '_, '_, '_, '_>,
        policy: &Policy,
        callback: F,
    ) -> Result<()>
    where
        F: Fn(Result<()>) + Send + 'static
    {
        // An empty reason makes evaluatePolicy throw NSInvalidArgumentException,
        // which aborts the app, so bail early.
        if text.apple.is_empty() {
            return Err(Error::InvalidText);
        }

        // An LAContext is single-use: reuse one and a past success lets the next
        // call skip the prompt. Make a fresh one each time.
        let context = unsafe { LAContext::new() };

        unsafe { context.canEvaluatePolicy_error(policy.inner) }.map_err(|err| {
            Error::from(LAError(err.code()))
        })?;

        // The eval is async and the context has to stay alive until the reply
        // fires, so move a strong ref into the block.
        let context_keepalive = context.clone();
        let block = RcBlock::new(move |is_success, error: *mut NSError| {
            let _keep_alive = &context_keepalive;
            let arg = bool::from(is_success)
                .then_some(())
                .ok_or_else(|| {
                    if error.is_null() {
                        Error::Unknown
                    } else {
                        let code = unsafe { &*error }.code();
                        let laerror = LAError(code);
                        Error::from(laerror)
                    }
                });
            // This runs on a framework queue, so don't let a panicking callback
            // unwind across the ObjC frame.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| callback(arg)));
        });

        unsafe {
            context.evaluatePolicy_localizedReason_reply(
                policy.inner,
                &NSString::from_str(text.apple),
                &block,
            )
        };

        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct Policy {
    inner: LAPolicy,
}

impl Policy {
    #[inline]
    pub(crate) fn set_action_id(&mut self, _: String) -> Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct PolicyBuilder {
    _biometrics: bool,
    _password: bool,
    _companion: bool,
    _wrist_detection: bool,
}

impl PolicyBuilder {
    pub(crate) const fn new() -> Self {
        Self {
            _biometrics: true,
            _password: true,
            _companion: true,
            _wrist_detection: true,
        }
    }

    pub(crate) const fn biometrics(self, strength: Option<BiometricStrength>) -> Self {
        Self {
            _biometrics: strength.is_some(),
            ..self
        }
    }

    pub(crate) const fn password(self, password: bool) -> Self {
        Self {
            _password: password,
            ..self
        }
    }

    pub(crate) const fn companion(self, companion: bool) -> Self {
        Self {
            _companion: companion,
            ..self
        }
    }

    pub(crate) const fn wrist_detection(self, wrist_detection: bool) -> Self {
        Self {
            _wrist_detection: wrist_detection,
            ..self
        }
    }

    pub(crate) fn action_ids(self, _: Vec<String>) -> Self {
        self
    }

    pub(crate) const fn build(self) -> Option<Policy> {
        // TODO: Test watchos

        #[cfg(target_os = "watchos")]
        let policy = match self {
            Self {
                _password: true,
                _wrist_detection: true,
                ..
            } => LAPolicy::DeviceOwnerAuthenticationWithWristDetection,
            Self {
                _password: true,
                _wrist_detection: false,
                ..
            } => LAPolicy::DeviceOwnerAuthentication,
            _ => return None,
        };

        #[cfg(not(target_os = "watchos"))]
        let policy = match self {
            Self {
                _biometrics: true,
                _password: true,
                ..
            } => {
                LAPolicy::DeviceOwnerAuthentication
            },
            Self {
                _biometrics: true,
                _password: false,
                _companion: true,
                ..
            } => {
                // This crashes the app on iOS (at least on the simulator).
                #[cfg(not(target_os = "ios"))] {
                    LAPolicy::DeviceOwnerAuthenticationWithBiometricsOrCompanion
                }
                #[cfg(target_os = "ios")] {
                    LAPolicy::DeviceOwnerAuthenticationWithBiometrics
                }
            },
            Self {
                _biometrics: true,
                _password: false,
                _companion: false,
                ..
            } => {
                LAPolicy::DeviceOwnerAuthenticationWithBiometrics
            },
            Self {
                _biometrics: false,
                _password: false,
                _companion: true,
                ..
            } => {
                // Companion-only isn't supported on iOS (it crashes), so call it
                // invalid instead of silently swapping in passcode auth.
                #[cfg(not(target_os = "ios"))] {
                    LAPolicy::DeviceOwnerAuthenticationWithCompanion
                }
                #[cfg(target_os = "ios")] {
                    return None
                }
            },
            _ => return None,
        };
        Some(Policy { inner: policy })
    }
}

impl From<LAError> for Error {
    fn from(err: LAError) -> Self {
        match err {
            LAError::AppCancel => Error::AppCanceled,
            LAError::AuthenticationFailed => Error::Authentication,
            LAError::BiometryDisconnected => Error::BiometryDisconnected,
            LAError::BiometryLockout => Error::Exhausted,
            // NOTE: This is triggered when access to biometrics is denied.
            LAError::BiometryNotAvailable => Error::Unavailable,
            LAError::BiometryNotEnrolled => Error::NotEnrolled,
            LAError::BiometryNotPaired => Error::NotPaired,
            // This error shouldn't occur, because we never invalidate the context.
            LAError::InvalidContext => Error::Unknown,
            LAError::InvalidDimensions => Error::InvalidDimensions,
            LAError::NotInteractive => Error::NotInteractive,
            LAError::PasscodeNotSet => Error::PasscodeNotSet,
            LAError::SystemCancel => Error::SystemCanceled,
            LAError::UserCancel => Error::UserCanceled,
            LAError::UserFallback => Error::UserFallback,
            LAError::CompanionNotAvailable => Error::CompanionNotAvailable,
            _ => Error::Unknown,
        }
    }
}
