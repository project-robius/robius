#[cfg(target_os = "ios")]
mod ios;
#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "ios")]
pub(crate) use ios::{query, read_now, start};
#[cfg(target_os = "macos")]
pub(crate) use macos::{query, read_now, start};
