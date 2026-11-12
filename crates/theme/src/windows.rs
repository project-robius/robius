//! Windows keeps its app theme in the registry, as the `AppsUseLightTheme` value
//! of the current user's `Personalize` key. Microsoft doesn't document it, but
//! the system's own theme API reads it the same way as `read` does here. While
//! a contrast theme is on, the theme is the one that its window color has.
//!
//! The system sets an event when a registry key changes, and one of its
//! thread-pool threads then calls `on_change`. A key that is followed stays
//! followed for good: nothing is ever unregistered or closed again.

use std::{
    ffi::c_void,
    io,
    mem::size_of,
    ptr,
    sync::{Mutex, PoisonError},
};

use crate::{Error, Result, Theme};

/// `HANDLE` and `HKEY`
type Handle = *mut c_void;
/// `WAITORTIMERCALLBACK`. Its second argument is a `BOOLEAN`.
type WaitCallback = unsafe extern "system" fn(context: *mut c_void, was_timed_out: u8);

/// `HIGHCONTRASTW`
#[repr(C)]
struct HighContrast {
    size: u32,
    flags: u32,
    /// The system points this at a buffer that it keeps, so it must not be freed.
    default_scheme: *mut u16,
}

/// `0x8000_0001` as a `LONG`, sign-extended to the size of a pointer.
const HKEY_CURRENT_USER: Handle = 0x8000_0001_u32 as i32 as isize as Handle;
const KEY_NOTIFY: u32 = 0x0010;
const REG_OPTION_NON_VOLATILE: u32 = 0;
const RRF_RT_REG_DWORD: u32 = 0x0010;
const REG_NOTIFY_CHANGE_NAME: u32 = 0x0001;
const REG_NOTIFY_CHANGE_LAST_SET: u32 = 0x0004;
/// Lets a thread-pool thread, which may exit at any time, ask for the next change.
const REG_NOTIFY_THREAD_AGNOSTIC: u32 = 0x1000_0000;
const INFINITE: u32 = u32::MAX;
const WT_EXECUTEDEFAULT: u32 = 0;
const SPI_GETHIGHCONTRAST: u32 = 0x0042;
const HCF_HIGHCONTRASTON: u32 = 0x0001;
const COLOR_WINDOW: i32 = 5;

#[link(name = "advapi32")]
extern "system" {
    fn RegCreateKeyExW(
        key: Handle,
        sub_key: *const u16,
        reserved: u32,
        class: *const u16,
        options: u32,
        access: u32,
        security: *const c_void,
        result: *mut Handle,
        disposition: *mut u32,
    ) -> i32;
    fn RegGetValueW(key: Handle, sub_key: *const u16, value: *const u16, flags: u32, kind: *mut u32, data: *mut c_void, size: *mut u32) -> i32;
    fn RegNotifyChangeKeyValue(key: Handle, is_for_subtree: i32, filter: u32, event: Handle, is_asynchronous: i32) -> i32;
    fn RegCloseKey(key: Handle) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn CreateEventW(security: *const c_void, is_manual_reset: i32, is_set: i32, name: *const u16) -> Handle;
    fn RegisterWaitForSingleObject(
        wait: *mut Handle,
        object: Handle,
        callback: WaitCallback,
        context: *mut c_void,
        milliseconds: u32,
        flags: u32,
    ) -> i32;
    fn CloseHandle(handle: Handle) -> i32;
}

#[link(name = "user32")]
extern "system" {
    fn SystemParametersInfoW(action: u32, param: u32, data: *mut c_void, update: u32) -> i32;
    fn GetSysColor(index: i32) -> u32;
}

/// The key of the app theme, and the key of the colors that a contrast theme sets.
static PERSONALIZE: [u16; 64] = wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
static COLORS: [u16; 64] = wide(r"Control Panel\Colors");
static APPS_USE_LIGHT_THEME: [u16; 64] = wide("AppsUseLightTheme");

pub(crate) fn query() -> Result<Option<Theme>> {
    Ok(Some(read()))
}

pub(crate) fn read_now() -> Option<Option<Theme>> {
    Some(Some(read()))
}

pub(crate) fn start() -> Result<()> {
    // How many of the two keys are followed so far, so that none is followed twice.
    static FOLLOWED: Mutex<usize> = Mutex::new(0);
    let mut followed = FOLLOWED.lock().unwrap_or_else(PoisonError::into_inner);
    for path in [&PERSONALIZE, &COLORS].into_iter().skip(*followed) {
        follow(path).map_err(|error| Error::Unavailable(format!("couldn't subscribe to registry changes: {error}")))?;
        *followed += 1;
    }
    Ok(())
}

/// A registry key, and the event that the system sets when the key changes.
struct FollowedKey {
    key: Handle,
    event: Handle,
}

impl FollowedKey {
    /// Asks for the event to be set at the key's next change. The system reports one change per call.
    fn arm(&self) -> io::Result<()> {
        const FILTER: u32 = REG_NOTIFY_CHANGE_NAME | REG_NOTIFY_CHANGE_LAST_SET | REG_NOTIFY_THREAD_AGNOSTIC;
        // SAFETY: both handles are open, and the call keeps no pointer of ours.
        let status = unsafe { RegNotifyChangeKeyValue(self.key, 0, FILTER, self.event, 1) };
        if status == 0 {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(status))
        }
    }
}

impl Drop for FollowedKey {
    /// Only runs if following the key failed, because a followed key is leaked.
    fn drop(&mut self) {
        // SAFETY: no wait is registered for the event, so nothing else uses either handle.
        unsafe {
            RegCloseKey(self.key);
            if !self.event.is_null() {
                CloseHandle(self.event);
            }
        }
    }
}

/// Follows one of the current user's registry keys for good, creating the key if it's missing.
fn follow(path: &[u16]) -> io::Result<()> {
    let mut key = ptr::null_mut();
    // SAFETY: `path` is NUL-terminated, `key` receives the handle, and the null arguments are optional.
    let status = unsafe {
        RegCreateKeyExW(HKEY_CURRENT_USER, path.as_ptr(), 0, ptr::null(), REG_OPTION_NON_VOLATILE, KEY_NOTIFY, ptr::null(), &mut key, ptr::null_mut())
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status));
    }
    // SAFETY: both pointers are optional. The event has no name, resets itself and starts unset.
    let followed = FollowedKey { key, event: unsafe { CreateEventW(ptr::null(), 0, 0, ptr::null()) } };
    if followed.event.is_null() {
        return Err(io::Error::last_os_error());
    }
    followed.arm()?;
    let event = followed.event;
    let followed = Box::into_raw(Box::new(followed));
    let mut wait = ptr::null_mut();
    // SAFETY: `followed` is leaked, so it outlives every call of `on_change`, which only reads it.
    if unsafe { RegisterWaitForSingleObject(&mut wait, event, on_change, followed.cast(), INFINITE, WT_EXECUTEDEFAULT) } == 0 {
        let error = io::Error::last_os_error();
        // SAFETY: the system didn't take the pointer, which `Box::into_raw` made just above.
        drop(unsafe { Box::from_raw(followed) });
        return Err(error);
    }
    Ok(())
}

/// The system calls this on a thread-pool thread each time the key has changed.
// Nothing in here panics, which matters because a panic must not unwind into the system.
unsafe extern "system" fn on_change(context: *mut c_void, _was_timed_out: u8) {
    // SAFETY: `context` is the leaked `FollowedKey` that `follow` registered.
    let followed = unsafe { &*context.cast::<FollowedKey>() };
    // Ask for the next change before reading, so that none is missed. If that fails, this was the last one.
    let _ = followed.arm();
    crate::refresh(|| Some(read()));
}

fn read() -> Theme {
    let is_light = match contrast_window_color() {
        Some(color) => is_light_color(color),
        None => is_app_theme_light(),
    };
    if is_light {
        Theme::Light
    } else {
        Theme::Dark
    }
}

/// The window color of the contrast theme, if one is on. Windows then ignores the app theme.
fn contrast_window_color() -> Option<u32> {
    let mut contrast = HighContrast { size: size_of::<HighContrast>() as u32, flags: 0, default_scheme: ptr::null_mut() };
    // SAFETY: `contrast` is the structure that this action fills in, and it states its own size.
    let is_filled = unsafe { SystemParametersInfoW(SPI_GETHIGHCONTRAST, contrast.size, ptr::addr_of_mut!(contrast).cast(), 0) } != 0;
    if !is_filled || contrast.flags & HCF_HIGHCONTRASTON == 0 {
        return None;
    }
    // SAFETY: this only takes a number.
    Some(unsafe { GetSysColor(COLOR_WINDOW) })
}

/// Windows itself takes the app theme for light unless a 0 is stored, so a failed read is light too.
fn is_app_theme_light() -> bool {
    let (mut number, mut size) = (1_u32, size_of::<u32>() as u32);
    // SAFETY: both strings are NUL-terminated, and `size` is the size of `number`, which receives the value.
    let status = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, PERSONALIZE.as_ptr(), APPS_USE_LIGHT_THEME.as_ptr(), RRF_RT_REG_DWORD, ptr::null_mut(), ptr::addr_of_mut!(number).cast(), &mut size)
    };
    status != 0 || number != 0
}

/// Microsoft's test of whether a color is a light one. A `COLORREF` is `0x00BBGGRR`.
fn is_light_color(color: u32) -> bool {
    let [red, green, blue, _] = color.to_le_bytes().map(u32::from);
    5 * green + 2 * red + blue > 8 * 128
}

/// Makes a NUL-terminated UTF-16 string from ASCII text of fewer than 64 characters.
const fn wide(text: &str) -> [u16; 64] {
    let (text, mut wide, mut i) = (text.as_bytes(), [0; 64], 0);
    assert!(text.len() < wide.len());
    while i < text.len() {
        assert!(text[i].is_ascii());
        wide[i] = text[i] as u16;
        i += 1;
    }
    wide
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tells_light_colors_from_dark_ones() {
        assert!(is_light_color(0x00FF_FFFF));
        assert!(!is_light_color(0x0000_0000));
        // Grey 128 is exactly on the limit, which still counts as dark.
        assert!(!is_light_color(0x0080_8080));
        assert!(is_light_color(0x0081_8181));
        // Green counts five times, red twice and blue once, so the order of the bytes matters.
        assert!(is_light_color(0x0000_99FF));
        assert!(!is_light_color(0x00FF_9900));
    }

    #[test]
    fn reads_the_same_theme_from_any_thread() {
        let theme = read();
        assert_eq!(std::thread::spawn(read).join().unwrap(), theme);
        assert_eq!(query(), Ok(Some(theme)));
        assert_eq!(read_now(), Some(Some(theme)));
    }
}
