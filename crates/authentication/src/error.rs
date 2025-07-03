/// The result of an authentication operation.
pub type Result<T> = std::result::Result<T, Error>;

/// An error produced during authentication.
#[derive(Debug)]
pub enum Error {
    // TODO: Reexport jni::errors::Error
    // TODO: Remove target cfg
    #[cfg(target_os = "android")]
    Java(jni::errors::Error),

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

#[cfg(target_os = "android")]
impl From<jni::errors::Error> for Error {
    fn from(value: jni::errors::Error) -> Self {
        Self::Java(value)
    }
}
