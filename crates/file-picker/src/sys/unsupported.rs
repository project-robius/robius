use std::path::PathBuf;

use crate::{DialogCallback, DialogData, DialogOptions, Error, MediaKind, Result};

pub(crate) fn read_uri_bytes(_: &str) -> Result<Vec<u8>> {
    Err(Error::Unsupported)
}

pub(crate) fn app_temp_dir() -> std::result::Result<std::path::PathBuf, Error> {
    Err(Error::Unsupported)
}

pub(crate) fn copy_uri_to_path(_: &str, _: &std::path::Path) -> Result<()> {
    Err(Error::Unsupported)
}

pub(crate) fn pick_file(_: DialogOptions, on_completion: DialogCallback) -> Result<()> {
    on_completion(Err(Error::Unsupported));
    Err(Error::Unsupported)
}

pub(crate) fn save_data(
    _: DialogOptions,
    _: DialogData,
    on_completion: DialogCallback,
) -> Result<()> {
    on_completion(Err(Error::Unsupported));
    Err(Error::Unsupported)
}

pub(crate) fn pick_media(
    _: DialogOptions,
    _: MediaKind,
    on_completion: DialogCallback,
) -> Result<()> {
    on_completion(Err(Error::Unsupported));
    Err(Error::Unsupported)
}

pub(crate) fn save_to_downloads(
    _: DialogOptions,
    _: PathBuf,
    on_completion: DialogCallback,
) -> Result<()> {
    on_completion(Err(Error::Unsupported));
    Err(Error::Unsupported)
}
