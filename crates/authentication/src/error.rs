/// The result of an authentication operation.
pub type Result<T> = std::result::Result<T, Error>;

/// An error produced during authentication.
#[derive(Debug, Clone)]
pub enum Error {
    // TODO: Reexport jni::errors::Error
    // TODO: Remove target cfg
    #[cfg(target_os = "android")]
    Java(std::sync::Arc<jni::errors::Error>),

    // Common errors
    /// The user failed to provide valid credentials.
    Authentication,
    /// Authentication failed because there were too many failed attempts.
    #[doc(alias = "lockout")]
    Exhausted,
    /// The requested authentication method was unavailable.
    Unavailable,
    /// The user canceled authentication.
    UserCanceled,
    /// The provided action ID is not in the policy's allowed list.
    InvalidActionId,
    /// The prompt text is invalid for this platform — e.g. an empty
    /// [`Text::apple`] reason or an empty [`AndroidText::title`], which the
    /// platform APIs reject.
    ///
    /// [`Text::apple`]: crate::Text::apple
    /// [`AndroidText::title`]: crate::AndroidText::title
    InvalidText,

    // Apple-specific errors
    /// The app canceled authentication.
    ///
    /// This error can occur on:
    /// - [Apple]
    ///
    /// [Apple]: https://developer.apple.com/documentation/localauthentication/laerror/laerrorappcancel
    AppCanceled,
    /// The system canceled authentication.
    ///
    /// This error can occur on:
    /// - [Apple]
    ///
    /// [Apple]: https://developer.apple.com/documentation/localauthentication/laerror/laerrorsystemcancel
    SystemCanceled,
    /// The device supports biometry only using a removable accessory, but the
    /// paired accessory isn’t connected.
    ///
    /// This error can occur on:
    /// - [Apple]
    ///
    /// [Apple]: https://developer.apple.com/documentation/localauthentication/laerror/laerrorbiometrydisconnected
    BiometryDisconnected,
    /// The device supports biometry only using a removable accessory, but no
    /// accessory is paired.
    ///
    /// This error can occur on:
    /// - [Apple]
    ///
    /// [Apple]: https://developer.apple.com/documentation/localauthentication/laerror/laerrorbiometrynotpaired
    NotPaired,
    /// The user has no enrolled biometric identities.
    ///
    /// This error can occur on:
    /// - [Apple]
    ///
    /// [Apple]: https://developer.apple.com/documentation/localauthentication/laerror/laerrorbiometrynotenrolled
    NotEnrolled,
    /// Displaying the required authentication user interface is forbidden.
    ///
    /// This error can occur on:
    /// - [Apple]
    ///
    /// [Apple]: https://developer.apple.com/documentation/localauthentication/laerror/laerrornotinteractive
    NotInteractive,
    /// An attempt to authenticate with an Apple companion device (e.g., Apple Watch) failed.
    ///
    /// This error can occur on:
    /// - [Apple], formerly known as `WatchNotAvailable`
    ///
    /// [Apple]: https://developer.apple.com/documentation/localauthentication/laerror-swift.struct/companionnotavailable
    #[doc(alias = "WatchNotAvailable")]
    CompanionNotAvailable,
    /// This error can occur on:
    /// - [Apple]
    ///
    /// [Apple]: https://developer.apple.com/documentation/localauthentication/laerror/laerrorinvaliddimensions
    InvalidDimensions,
    /// A passcode isn’t set on the device.
    ///
    /// This error can occur on:
    /// - [Apple]
    ///
    /// [Apple]: https://developer.apple.com/documentation/localauthentication/laerror/laerrorpasscodenotset
    PasscodeNotSet,
    /// The user tapped the fallback button in the authentication dialog (e.g., "Use Password" instead),
    /// but you selected an authentication policy that does not support password fallback.
    ///
    /// If you get this error, you either must handle the fallback yourself or enable the `password` option
    /// in the policy builder, which will instruct the system to enable a password fallback option
    /// in the authentication dialog.
    ///
    /// This error can occur on:
    /// - [Apple]
    ///
    /// [Apple]: https://developer.apple.com/documentation/localauthentication/laerror/userfallback
    UserFallback,

    // Android-specific errors
    UpdateRequired,
    Timeout,

    // Windows-specific errors
    /// The biometric verifier device is performing an operation and is
    /// unavailable.
    ///
    /// This error can occur on:
    /// - [Windows]
    ///
    /// [Windows]: https://learn.microsoft.com/en-us/uwp/api/windows.security.credentials.ui.userconsentverificationresult
    Busy,
    /// Group policy has disabled the biometric verifier device.
    ///
    /// This error can occur on:
    /// - [Windows]
    ///
    /// [Windows]: https://learn.microsoft.com/en-us/uwp/api/windows.security.credentials.ui.userconsentverificationresult
    DisabledByPolicy,
    /// A biometric verifier device is not configured for this user.
    ///
    /// This error can occur on:
    /// - [Windows]
    ///
    /// [Windows]: https://learn.microsoft.com/en-us/uwp/api/windows.security.credentials.ui.userconsentverificationresult
    NotConfigured,

    /// An unknown error occurred.
    Unknown,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let description = match self {
            #[cfg(target_os = "android")]
            Self::Java(e) => return write!(f, "Java error during authentication: {e}"),
            Self::Authentication => "the user failed to provide valid credentials",
            Self::Exhausted => "authentication failed due to too many failed attempts",
            Self::Unavailable => "the requested authentication method was unavailable",
            Self::UserCanceled => "the user canceled authentication",
            Self::InvalidActionId => "the provided action ID is not in the policy's allowed list",
            Self::InvalidText => "the provided prompt text is invalid for the current platform",
            Self::AppCanceled => "the app canceled authentication",
            Self::SystemCanceled => "the system canceled authentication",
            Self::BiometryDisconnected => {
                "the paired biometric accessory required for authentication is not connected"
            }
            Self::NotPaired => "no biometric accessory required for authentication is paired",
            Self::NotEnrolled => "the user has no enrolled biometric identities",
            Self::NotInteractive => "displaying the required authentication UI is forbidden",
            Self::CompanionNotAvailable => {
                "authentication with a companion device (e.g., Apple Watch) failed"
            }
            Self::InvalidDimensions => "the authentication input has invalid dimensions",
            Self::PasscodeNotSet => "no passcode is set on the device",
            Self::UserFallback => {
                "the user chose the fallback authentication method, \
                 but the policy does not support fallback"
            }
            Self::UpdateRequired => "a security update is required before authenticating",
            Self::Timeout => "authentication timed out",
            Self::Busy => "the biometric verifier device is busy",
            Self::DisabledByPolicy => "group policy has disabled the biometric verifier device",
            Self::NotConfigured => "no biometric verifier device is configured for this user",
            Self::Unknown => "an unknown authentication error occurred",
        };
        f.write_str(description)
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        #[cfg(target_os = "android")]
        if let Self::Java(e) = self {
            return Some(&**e);
        }
        None
    }
}

#[cfg(target_os = "android")]
impl From<jni::errors::Error> for Error {
    fn from(value: jni::errors::Error) -> Self {
        Self::Java(std::sync::Arc::new(value))
    }
}
