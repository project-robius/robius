//! Platforms with no system theme that we know how to read.

use crate::{Error, Result, Theme};

pub(crate) fn query() -> Result<Option<Theme>> {
    Err(Error::Unsupported)
}

pub(crate) fn start() -> Result<()> {
    Err(Error::Unsupported)
}

pub(crate) fn read_now() -> Option<Option<Theme>> {
    None
}
