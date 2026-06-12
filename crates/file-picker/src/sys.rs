cfg_if::cfg_if! {
    if #[cfg(target_os = "android")] {
        mod android;
        pub(crate) use android::*;
    } else if #[cfg(target_os = "ios")] {
        mod ios;
        pub(crate) use ios::*;
    } else if #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))] {
        mod desktop;
        pub(crate) use desktop::*;
    } else {
        mod unsupported;
        pub(crate) use unsupported::*;
    }
}
