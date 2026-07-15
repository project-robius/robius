// For the `uwp` feature gates.
#![allow(unexpected_cfgs)]

mod fallback;

use windows::{
    core::HSTRING,
    Foundation::IAsyncOperation,
    Security::Credentials::UI::{
        UserConsentVerificationResult, UserConsentVerifier, UserConsentVerifierAvailability,
    },
    Win32::{
        Foundation::RPC_E_CHANGED_MODE,
        System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED},
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
        // WindowsText borrows from the caller, so copy the strings for the thread.
        let title = message.windows.title.to_owned();
        let description = message.windows.description.to_owned();
        // The availability check and the prompts all block until the user answers,
        // so run them off the caller thread and don't freeze the UI.
        std::thread::Builder::new()
            .name("robius-authentication".into())
            .spawn(move || {
                let text = WindowsText {
                    title: &title,
                    description: &description,
                };
                // New thread has no COM apartment, and blocking on
                // IAsyncOperation::get() needs an MTA, so set one up for it.
                let com = ComGuard::new_multithreaded();
                callback(authenticate_blocking(text));
                drop(com);
            })
            .map_err(|_| Error::Unavailable)?;
        Ok(())
    }
}

/// Sets up a COM multithreaded apartment for this thread and uninitializes it
/// on drop, unless the thread was already in a different apartment.
struct ComGuard {
    should_uninit: bool,
}

impl ComGuard {
    fn new_multithreaded() -> Self {
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        // S_OK/S_FALSE mean we init'd and owe a CoUninitialize. RPC_E_CHANGED_MODE
        // means the thread's already in another apartment, so leave it alone.
        Self {
            should_uninit: hr != RPC_E_CHANGED_MODE,
        }
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.should_uninit {
            unsafe { CoUninitialize() };
        }
    }
}

fn authenticate_blocking(text: WindowsText<'_, '_>) -> Result<()> {
    // NOTE: If we don't check availability, `request_verification` will hang.
    let available =
        check_availability()?.get() == Ok(UserConsentVerifierAvailability::Available);

    if available {
        let verification = request_verification(text)?;
        convert(verification.get()?)
    } else {
        fallback::authenticate(text)
    }
}

#[derive(Debug)]
pub(crate) struct Policy;

impl Policy {
    #[inline]
    pub(crate) fn set_action_id(&mut self, _: String) -> Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct PolicyBuilder {
    biometrics: bool,
    password: bool,
}

impl PolicyBuilder {
    pub(crate) const fn new() -> Self {
        Self {
            biometrics: true,
            password: true,
        }
    }

    pub(crate) const fn biometrics(self, biometrics: Option<BiometricStrength>) -> Self {
        Self {
            biometrics: biometrics.is_some(),
            ..self
        }
    }

    pub(crate) const fn password(self, password: bool) -> Self {
        Self { password, ..self }
    }

    pub(crate) const fn companion(self, _: bool) -> Self {
        self
    }

    pub(crate) const fn wrist_detection(self, _: bool) -> Self {
        self
    }

    pub(crate) fn action_ids(self, _: Vec<String>) -> Self {
        self
    }

    pub(crate) const fn build(self) -> Option<Policy> {
        // Windows Hello always allows PIN and the fallback is password-based, so
        // we can't honor a policy that turns either one off.
        if self.biometrics && self.password {
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
    UserConsentVerifier::RequestVerificationAsync(&HSTRING::from(text.description))
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

    let factory = factory::<UserConsentVerifier, IUserConsentVerifierInterop>()?;

    let op = unsafe {
        IUserConsentVerifierInterop::RequestVerificationForWindowAsync(
            &factory,
            window,
            // NOTE: HSTRING is length-prefixed, so no null terminator; `from`
            // does the UTF-16 conversion.
            &HSTRING::from(text.description),
        )
    }?;

    // Focusing is just a nice-to-have and the prompt's already up, so don't fail
    // auth if it doesn't work.
    let _ = focus_security_prompt();

    Ok(op)
}

fn convert(result: UserConsentVerificationResult) -> Result<()> {
    match result {
        UserConsentVerificationResult::Verified => Ok(()),
        UserConsentVerificationResult::DeviceNotPresent => Err(Error::Unavailable),
        UserConsentVerificationResult::NotConfiguredForUser => Err(Error::NotConfigured),
        UserConsentVerificationResult::DisabledByPolicy => Err(Error::DisabledByPolicy),
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
