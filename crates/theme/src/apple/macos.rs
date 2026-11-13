//! macOS mirrors its theme in the `AppleInterfaceStyle` preference and posts
//! `AppleInterfaceThemeChangedNotification` after each change. Apple documents
//! neither, but AppKit and Apple's own apps depend on both.
//!
//! An app's own `NSApp.appearance` doesn't affect either one. The system only
//! posts the change inside processes that are connected to the window server,
//! which every app with an `NSApplication` is.

use std::{
    ffi::{c_char, c_void, CStr},
    ptr,
    sync::Once,
};

use crate::{Result, Theme};

type CFStringRef = *const c_void;
type CFTypeRef = *const c_void;
type CFNotificationCallback =
    extern "C" fn(center: *const c_void, token: *mut c_void, name: CFStringRef, object: *const c_void, user_info: *const c_void);

/// `kCFStringEncodingUTF8`
const UTF8: u32 = 0x0800_0100;
/// `CFNotificationSuspensionBehaviorDeliverImmediately`: don't wait until the app is active.
const DELIVER_IMMEDIATELY: isize = 4;

#[link(name = "CoreFoundation", kind = "framework")]
#[allow(non_upper_case_globals)]
extern "C" {
    static kCFPreferencesAnyApplication: CFStringRef;
    static kCFPreferencesCurrentUser: CFStringRef;
    static kCFPreferencesAnyHost: CFStringRef;
    fn CFPreferencesCopyValue(key: CFStringRef, application: CFStringRef, user: CFStringRef, host: CFStringRef) -> CFTypeRef;
    fn CFStringCreateWithCString(allocator: *const c_void, text: *const c_char, encoding: u32) -> CFStringRef;
    fn CFStringGetTypeID() -> usize;
    fn CFGetTypeID(value: CFTypeRef) -> usize;
    fn CFEqual(a: CFTypeRef, b: CFTypeRef) -> u8;
    fn CFRelease(value: CFTypeRef);
    fn CFNotificationCenterGetDistributedCenter() -> *const c_void;
    fn CFNotificationCenterAddObserver(
        center: *const c_void,
        token: *const c_void,
        callback: CFNotificationCallback,
        name: CFStringRef,
        object: *const c_void,
        suspension_behavior: isize,
    );
}

pub(crate) fn query() -> Result<Option<Theme>> {
    Ok(Some(read()))
}

pub(crate) fn read_now() -> Option<Option<Theme>> {
    Some(Some(read()))
}

pub(crate) fn start() -> Result<()> {
    static START: Once = Once::new();
    START.call_once(|| {
        // Only its address is used, to name this registration.
        static TOKEN: u8 = 0;
        // SAFETY: the name is a valid string that the center copies, and the
        // callback is a plain function that stays registered for good.
        unsafe {
            let name = cf_string(c"AppleInterfaceThemeChangedNotification");
            CFNotificationCenterAddObserver(
                CFNotificationCenterGetDistributedCenter(),
                ptr::addr_of!(TOKEN).cast(),
                on_change,
                name,
                ptr::null(),
                DELIVER_IMMEDIATELY,
            );
            CFRelease(name);
        }
    });
    Ok(())
}

/// The system calls this on the main thread, after it has saved the new theme.
extern "C" fn on_change(_: *const c_void, _: *mut c_void, _: CFStringRef, _: *const c_void, _: *const c_void) {
    crate::refresh(|| Some(read()));
}

/// The system itself only treats the string `Dark` as dark; anything else, or nothing, is light.
fn read() -> Theme {
    // SAFETY: the three domain constants live as long as the process, and
    // everything created or copied here is released exactly once.
    let is_dark = unsafe {
        let key = cf_string(c"AppleInterfaceStyle");
        let value = CFPreferencesCopyValue(key, kCFPreferencesAnyApplication, kCFPreferencesCurrentUser, kCFPreferencesAnyHost);
        CFRelease(key);
        if value.is_null() {
            return Theme::Light;
        }
        let dark = cf_string(c"Dark");
        let is_dark = CFGetTypeID(value) == CFStringGetTypeID() && CFEqual(value, dark) != 0;
        CFRelease(dark);
        CFRelease(value);
        is_dark
    };
    if is_dark {
        Theme::Dark
    } else {
        Theme::Light
    }
}

/// Makes a string that the caller must release.
fn cf_string(text: &CStr) -> CFStringRef {
    // SAFETY: `text` is a valid NUL-terminated UTF-8 string, which is copied.
    unsafe { CFStringCreateWithCString(ptr::null(), text.as_ptr(), UTF8) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_same_theme_from_any_thread() {
        let theme = read();
        assert_eq!(std::thread::spawn(read).join().unwrap(), theme);
        assert_eq!(query(), Ok(Some(theme)));
    }
}
