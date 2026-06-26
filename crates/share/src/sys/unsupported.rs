use crate::{Error, Result, ShareOptions};

pub(crate) fn share(_: ShareOptions) -> Result<()> {
    Err(Error::Unsupported)
}
