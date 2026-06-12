cfg_if::cfg_if! {
    if #[cfg(target_os = "ios")] {
        mod ios;
        pub(crate) use ios::*;
    } else {
        mod unsupported;
        pub(crate) use unsupported::*;
    }
}
