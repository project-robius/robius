use block2::RcBlock;
use objc2::rc::Retained;
use objc2_foundation::{NSError, NSString};
use objc2_local_authentication::{LAContext, LAError, LAPolicy};
// #[cfg(feature = "async")]
// use tokio::sync::oneshot as channel_impl;

use crate::{BiometricStrength, Error, Result, Text};

pub(crate) type RawContext = ();

#[derive(Debug)]
pub(crate) struct Context {
    inner: Retained<LAContext>,
}

impl Context {
    pub(crate) fn new(_: RawContext) -> Self {
        Self {
            inner: unsafe { LAContext::new() },
        }
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
        unsafe { self.inner.canEvaluatePolicy_error(policy.inner) }.map_err(|err| {
            Error::from(LAError(err.code()))
        })?;

        let block = RcBlock::new(move |is_success, error: *mut NSError| {
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
            callback(arg)
        });

        unsafe {
            self.inner.evaluatePolicy_localizedReason_reply(
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
                // This crashes the app on iOS (at least on the simulator).
                #[cfg(not(target_os = "ios"))] {
                    LAPolicy::DeviceOwnerAuthenticationWithCompanion
                }
                #[cfg(target_os = "ios")] {
                    LAPolicy::DeviceOwnerAuthentication
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
