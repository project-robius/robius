use crate::{
    ActiveIdsCallback, Error, NotificationOptions, PermissionCallback, Result, SettingsCallback,
    SettingsScope,
};

/// No OS-side scheduling; `show()` uses the crate's in-process fallback timer.
pub(crate) const NATIVE_SCHEDULING: bool = false;

pub(crate) fn show(_: NotificationOptions) -> Result<()> {
    Err(Error::Unsupported)
}

pub(crate) fn update_progress(_: &NotificationOptions) -> Result<()> {
    Err(Error::Unsupported)
}

pub(crate) fn cancel(_: &str) -> Result<()> {
    Err(Error::Unsupported)
}

pub(crate) fn cancel_all() -> Result<()> {
    Err(Error::Unsupported)
}

pub(crate) fn request_permission(_: PermissionCallback, _provisional: bool) -> Result<()> {
    Err(Error::Unsupported)
}

pub(crate) fn init_interaction_listener() -> Result<()> {
    Err(Error::Unsupported)
}

pub(crate) fn notification_settings(_: SettingsScope, _: SettingsCallback) -> Result<()> {
    Err(Error::Unsupported)
}

pub(crate) fn open_notification_settings(_: SettingsScope) -> Result<()> {
    Err(Error::Unsupported)
}

pub(crate) fn active_notification_ids(_: ActiveIdsCallback) -> Result<()> {
    Err(Error::Unsupported)
}
