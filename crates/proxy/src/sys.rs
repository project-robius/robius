cfg_if::cfg_if! {
    if #[cfg(target_os = "macos")] {
        mod macos;
        pub(crate) use macos::Manager;
    } else {
        mod unsupported;
        pub(crate) use unsupported::Manager;
    }
}
