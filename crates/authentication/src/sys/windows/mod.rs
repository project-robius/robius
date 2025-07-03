// For the `uwp` feature gates.
#![allow(unexpected_cfgs)]

mod fallback;

use windows::{
    core::HSTRING,
    Foundation::IAsyncOperation,
    Security::Credentials::UI::{
        UserConsentVerificationResult, UserConsentVerifier, UserConsentVerifierAvailability,
    },
};

use crate::{text::WindowsText, BiometricStrength, Error, Result, Text};

pub(crate) type RawContext = ();

#[derive(Debug)]
pub(crate) struct Context;

impl Context {
    pub(crate) fn new(_: RawContext) -> Self {
        Self
    }

    // TODO: fix the async authenticate function
    //
    // #[cfg(feature = "async")]
    // pub(crate) async fn authenticate_async(
    //     &self,
    //     message: Text<'_, '_, '_, '_, '_, '_>,
    //     _: &Policy,
    // ) -> Result<()> {
    //     // NOTE: If we don't check availability, `request_verification` will hang.
    //     let available =
    //         check_availability()?.await == Ok(UserConsentVerifierAvailability::Available);

    //     if available {
    //         convert(request_verification(message.windows)?.await?)
    //     } else {
    //         fallback::authenticate(message.windows)
    //     }
    // }

    pub(crate) fn authenticate<F>(
        &self,
        message: Text,
        _: &Policy,
        callback: F,
    ) -> Result<()>
    where
        F: Fn(Result<()>) + Send + 'static,
    {
        // NOTE: If we don't check availability, `request_verification` will hang.
        let available =
            check_availability()?.get() == Ok(UserConsentVerifierAvailability::Available);

        let result = if available {
            let verification = request_verification(message.windows)?;
            convert(verification.get()?)
        } else {
            fallback::authenticate(message.windows)
        };
        callback(result);
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct Policy;

#[derive(Debug)]
pub(crate) struct PolicyBuilder {
    valid: bool,
}

impl PolicyBuilder {
    pub(crate) const fn new() -> Self {
        Self { valid: true }
    }

    pub(crate) const fn biometrics(self, biometrics: Option<BiometricStrength>) -> Self {
        if biometrics.is_none() {
            Self { valid: false }
        } else {
            self
        }
    }

    pub(crate) const fn password(self, password: bool) -> Self {
        if password {
            self
        } else {
            Self { valid: false }
        }
    }

    pub(crate) const fn companion(self, _: bool) -> Self {
        self
    }

    pub(crate) const fn wrist_detection(self, _: bool) -> Self {
        self
    }

    pub(crate) const fn build(self) -> Option<Policy> {
        if self.valid {
            Some(Policy)
        } else {
            None
        }
    }
}

fn check_availability() -> Result<IAsyncOperation<UserConsentVerifierAvailability>> {
    UserConsentVerifier::CheckAvailabilityAsync().map_err(|e| e.into())
}

#[cfg(feature = "uwp")]
fn request_verification(
    text: WindowsText,
) -> Result<IAsyncOperation<UserConsentVerificationResult>> {
    let caption = caption(text.description);

    UserConsentVerifier::RequestVerificationAsync(&HSTRING::from_wide(&caption[..])?)
        .map_err(|e| e.into())
}

#[cfg(not(feature = "uwp"))]
fn request_verification(
    text: WindowsText,
) -> Result<IAsyncOperation<UserConsentVerificationResult>> {
    use windows::{
        core::{factory, s},
        Win32::{
            Foundation::HWND,
            System::WinRT::IUserConsentVerifierInterop,
            UI::{
                Input::KeyboardAndMouse::{
                    keybd_event, GetAsyncKeyState, SetFocus, KEYEVENTF_EXTENDEDKEY,
                    KEYEVENTF_KEYUP, VK_MENU,
                },
                WindowsAndMessaging::{FindWindowA, GetDesktopWindow, SetForegroundWindow},
            },
        },
    };

    // Taken from Bitwarden:
    // https://github.com/bitwarden/clients/blob/fb7273beb894b33db8b62f853b3d056656342856/apps/desktop/desktop_native/src/biometric/windows.rs#L192
    fn focus_security_prompt() -> Result<()> {
        unsafe fn try_find_and_set_focus(
            class_name: windows::core::PCSTR,
        ) -> retry::OperationResult<(), ()> {
            let hwnd = unsafe { FindWindowA(class_name, None) };
            if hwnd.0 != 0 {
                set_focus(hwnd);
                return retry::OperationResult::Ok(());
            }
            retry::OperationResult::Retry(())
        }

        let class_name = s!("Credential Dialog Xaml Host");
        retry::retry_with_index(retry::delay::Fixed::from_millis(500), |current_try| {
            if current_try > 3 {
                return retry::OperationResult::Err(());
            }

            unsafe { try_find_and_set_focus(class_name) }
        })
        .map_err(|_| Error::Unknown)
    }

    // Taken from Bitwarden:
    // https://github.com/bitwarden/clients/blob/fb7273beb894b33db8b62f853b3d056656342856/apps/desktop/desktop_native/src/biometric/windows.rs#L215
    fn set_focus(window: HWND) {
        let mut pressed = false;

        unsafe {
            // Simulate holding down Alt key to bypass windows limitations
            //  https://docs.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getasynckeystate#return-value
            //  The most significant bit indicates if the key is currently being pressed.
            // This means the  value will be negative if the key is pressed.
            if GetAsyncKeyState(VK_MENU.0 as i32) >= 0 {
                pressed = true;
                keybd_event(VK_MENU.0 as u8, 0, KEYEVENTF_EXTENDEDKEY, 0);
            }
            let _ = SetForegroundWindow(window);
            SetFocus(window);
            if pressed {
                keybd_event(
                    VK_MENU.0 as u8,
                    0,
                    KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP,
                    0,
                );
            }
        }
    }

    let window = unsafe { GetDesktopWindow() };
    let caption = caption(text.description);

    let factory = factory::<UserConsentVerifier, IUserConsentVerifierInterop>()?;

    let op = unsafe {
        IUserConsentVerifierInterop::RequestVerificationForWindowAsync(
            &factory,
            window,
            &HSTRING::from_wide(&caption[..])?,
        )
    }?;

    focus_security_prompt()?;

    Ok(op)
}

fn caption(message: &str) -> Vec<u16> {
    let mut caption = Vec::with_capacity(message.len());

    for c in message.encode_utf16() {
        caption.push(c);
    }
    caption.push(0);

    caption
}

fn convert(result: UserConsentVerificationResult) -> Result<()> {
    match result {
        UserConsentVerificationResult::Verified => Ok(()),
        UserConsentVerificationResult::DeviceNotPresent => Err(Error::Unavailable),
        UserConsentVerificationResult::NotConfiguredForUser => Err(Error::Unavailable),
        UserConsentVerificationResult::DisabledByPolicy => Err(Error::Unavailable),
        UserConsentVerificationResult::DeviceBusy => Err(Error::Busy),
        UserConsentVerificationResult::RetriesExhausted => Err(Error::Exhausted),
        UserConsentVerificationResult::Canceled => Err(Error::UserCanceled),
        _ => Err(Error::Unknown),
    }
}

impl From<windows::core::Error> for Error {
    fn from(_value: windows::core::Error) -> Self {
        Self::Unknown
    }
}
