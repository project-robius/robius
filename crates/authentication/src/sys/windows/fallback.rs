use std::ffi::c_void;

use windows::{
    core::{PCWSTR, PSTR},
    Win32::{
        Foundation::{CloseHandle, ERROR_CANCELLED, HANDLE, HWND, NO_ERROR, WIN32_ERROR},
        Graphics::Gdi::HBITMAP,
        Security::{
            Authentication::Identity::{
                GetUserNameExW, LsaConnectUntrusted, LsaDeregisterLogonProcess,
                LsaLookupAuthenticationPackage, NameSamCompatible, LSA_STRING,
                MSV1_0_PACKAGE_NAME,
            },
            Credentials::{
                CredUIParseUserNameW, CredUIPromptForWindowsCredentialsW,
                CredUnPackAuthenticationBufferW, CREDUIWIN_ENUMERATE_CURRENT_USER, CREDUI_INFOW,
                CREDUI_MAX_DOMAIN_TARGET_LENGTH, CREDUI_MAX_USERNAME_LENGTH,
                CRED_PACK_PROTECTED_CREDENTIALS,
            },
            LogonUserW, LOGON32_LOGON_INTERACTIVE, LOGON32_PROVIDER_DEFAULT,
        },
        System::Com::CoTaskMemFree,
    },
};
use windows_core::PWSTR;

use crate::{text::WindowsText, Error, Result};

// Add one to include null byte.
// NOTE: the unpacked name is DOMAIN\user, so size it for the whole thing
// (CREDUI_MAX_USERNAME_LENGTH), not just the account name.
const MAX_USERNAME_LENGTH: usize = CREDUI_MAX_USERNAME_LENGTH as usize + 1;
const MAX_DOMAIN_LENGTH: usize = CREDUI_MAX_DOMAIN_TARGET_LENGTH as usize + 1;
const MAX_PASSWORD_LENGTH: usize = 256 + 1;

type Username = [u16; MAX_USERNAME_LENGTH];
type Domain = [u16; MAX_DOMAIN_LENGTH];
type Password = [u16; MAX_PASSWORD_LENGTH];

pub(super) fn authenticate(text: WindowsText) -> Result<()> {
    let (auth_buf, auth_buf_size) = ui_prompt(text)?;
    let unpacked = unpack_authentication_buffer(auth_buf, auth_buf_size);
    // CredUI docs say to zero the credential blob and free it with CoTaskMemFree
    // once we're done with it.
    unsafe {
        std::ptr::write_bytes(auth_buf as *mut u8, 0, auth_buf_size as usize);
        CoTaskMemFree(Some(auth_buf));
    }
    let ((username, username_size), mut password) = unpacked?;

    let result = check_and_logon(username, username_size, &mut password);
    // Zero our plaintext password copy, best effort.
    for c in password.iter_mut() {
        unsafe { std::ptr::write_volatile(c, 0) };
    }
    result
}

fn check_and_logon(
    username: Username,
    username_size: usize,
    password: &mut Password,
) -> Result<()> {
    let (current_user, current_user_size) = current_user()?;
    // Windows account names are case-insensitive.
    if !eq_ignore_case(&username[..username_size], &current_user[..current_user_size]) {
        return Err(Error::Authentication);
    }

    let (account_name, domain) = parse_username(username)?;
    logon_user(account_name, domain, password)
}

/// Compares two UTF-16 strings case-insensitively, ignoring trailing nulls.
fn eq_ignore_case(a: &[u16], b: &[u16]) -> bool {
    let a = String::from_utf16_lossy(a);
    let b = String::from_utf16_lossy(b);
    a.trim_end_matches('\0').to_lowercase() == b.trim_end_matches('\0').to_lowercase()
}

fn ui_prompt(text: WindowsText) -> Result<(*mut c_void, u32)> {
    // _message and _caption can only be dropped after we've used ui.
    let (_message, _caption, ui) = ui(text);

    let handle = handle()?;
    let auth_package_result = auth_package(handle);
    // We only needed the LSA handle to look up the auth package, so close it
    // either way — otherwise it leaks a kernel handle per prompt.
    unsafe {
        let _ = LsaDeregisterLogonProcess(handle);
    }
    let mut auth_package = auth_package_result?;

    let mut auth_buf = std::ptr::null_mut();
    let mut auth_buf_size = 0u32;

    let err = unsafe {
        CredUIPromptForWindowsCredentialsW(
            Some(&ui as *const _),
            0,
            &mut auth_package as *mut _,
            None,
            0,
            &mut auth_buf as *mut _,
            &mut auth_buf_size as *mut _,
            None,
            CREDUIWIN_ENUMERATE_CURRENT_USER,
        )
    };

    match WIN32_ERROR(err) {
        NO_ERROR => Ok((auth_buf, auth_buf_size)),
        ERROR_CANCELLED => Err(Error::UserCanceled),
        _ => Err(Error::Unknown),
    }
}

fn ui(text: WindowsText) -> (Vec<u16>, Vec<u16>, CREDUI_INFOW) {
    let mut message = Vec::with_capacity(text.description.len() + 1);
    message.extend(text.description.encode_utf16());
    message.push(0);

    let mut caption = Vec::with_capacity(text.title.len() + 1);
    caption.extend(text.title.encode_utf16());
    caption.push(0);

    let ui = CREDUI_INFOW {
        cbSize: core::mem::size_of::<CREDUI_INFOW>() as u32,
        hwndParent: HWND(0),
        pszMessageText: PCWSTR(message.as_ptr()),
        pszCaptionText: PCWSTR(caption.as_ptr()),
        hbmBanner: HBITMAP(0),
    };

    (message, caption, ui)
}

fn handle() -> Result<HANDLE> {
    let mut handle = HANDLE(0);

    let result = unsafe { LsaConnectUntrusted(&mut handle as *mut _) };

    if result.is_ok() {
        Ok(handle)
    } else {
        Err(Error::Unknown)
    }
}

fn auth_package(handle: HANDLE) -> Result<u32> {
    let mut auth_package = 0u32;

    let auth_package_bytes = unsafe { MSV1_0_PACKAGE_NAME.as_bytes() };
    let auth_package_len = auth_package_bytes.len();
    let auth_package_max_len = auth_package_len + 1;

    let mut auth_package_name = vec![0; auth_package_max_len];
    auth_package_name[..auth_package_len].copy_from_slice(auth_package_bytes);
    auth_package_name[auth_package_len] = 0;

    let str = LSA_STRING {
        Length: auth_package_len as u16,
        MaximumLength: auth_package_max_len as u16,
        Buffer: PSTR(auth_package_name.as_ptr() as *mut _),
    };

    let is_ok = unsafe {
        LsaLookupAuthenticationPackage(handle, &str as *const _, &mut auth_package as *mut _)
    }
    .is_ok();

    if is_ok {
        Ok(auth_package)
    } else {
        Err(Error::Unknown)
    }
}

fn unpack_authentication_buffer(
    out_auth_buffer: *mut c_void,
    out_cred_size: u32,
) -> Result<((Username, usize), Password)> {
    // Length is wrong? This username includes domain.
    let mut username = [0u16; MAX_USERNAME_LENGTH];
    let mut username_size = username.len() as u32;

    let mut password = [0u16; MAX_PASSWORD_LENGTH];
    let mut password_size = password.len() as u32;

    unsafe {
        CredUnPackAuthenticationBufferW(
            CRED_PACK_PROTECTED_CREDENTIALS,
            out_auth_buffer,
            out_cred_size,
            PWSTR(username.as_mut_ptr()),
            &mut username_size as *mut _,
            PWSTR(std::ptr::null_mut()),
            None,
            PWSTR(password.as_mut_ptr()),
            &mut password_size as *mut _,
        )
    }
    .map_err(|_| Error::Unknown)?;

    Ok(((username, username_size as usize), password))
}

fn current_user() -> Result<(Username, usize)> {
    let mut username = [0u16; MAX_USERNAME_LENGTH];
    let mut username_size = username.len() as u32;

    let is_ok = unsafe {
        GetUserNameExW(
            NameSamCompatible,
            PWSTR(username.as_mut_ptr()),
            &mut username_size as *mut _,
        )
    }
    .as_bool();

    if is_ok {
        // `GetUserNameExW` reports the length excluding the null terminator.
        Ok((username, username_size as usize))
    } else {
        Err(Error::Unknown)
    }
}

fn parse_username(
    mut username: Username,
) -> Result<([u16; MAX_USERNAME_LENGTH], [u16; MAX_DOMAIN_LENGTH])> {
    let mut account_name = [0; MAX_USERNAME_LENGTH];
    let mut domain = [0; MAX_DOMAIN_LENGTH];

    let err = unsafe {
        CredUIParseUserNameW(
            PCWSTR(username.as_mut_ptr()),
            &mut account_name,
            &mut domain,
        )
    };

    if err == 0 {
        Ok((account_name, domain))
    } else {
        Err(Error::Unknown)
    }
}

fn logon_user(
    mut account_name: Username,
    mut domain: Domain,
    password: &mut Password,
) -> Result<()> {
    let mut token = HANDLE(0);
    let result = unsafe {
        LogonUserW(
            PWSTR(account_name.as_mut_ptr()),
            PWSTR(domain.as_mut_ptr()),
            PWSTR(password.as_mut_ptr()),
            LOGON32_LOGON_INTERACTIVE,
            LOGON32_PROVIDER_DEFAULT,
            // If we pass in a null pointer here, Windows silently succeeds regardless of the
            // password provided ... thanks Windows.
            &mut token as *mut _,
        )
    }
    .map_err(|_| Error::Authentication);

    // LogonUserW hands back a token handle on success that we have to close.
    if !token.is_invalid() {
        unsafe {
            let _ = CloseHandle(token);
        }
    }

    result
}
