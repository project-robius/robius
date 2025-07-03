cfg_if::cfg_if! {
    if #[cfg(target_os = "android")] {
        mod android;
        pub(crate) use android::*;
    } else if #[cfg(target_vendor = "apple")] {
        mod apple;
        pub(crate) use apple::*;
    // } else if #[cfg(target_os = "linux")] { // linux is currently unsupported
    //     mod linux;
    //     pub(crate) use linux::*;
    } else if #[cfg(target_os = "windows")] {
        mod windows;
        pub(crate) use windows::*;
    } else {
        mod unsupported;
        pub(crate) use unsupported::*;
    }
}
