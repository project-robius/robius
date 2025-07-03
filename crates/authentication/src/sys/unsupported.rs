use crate::{BiometricStrength, Error, Result, Text};

pub(crate) type RawContext = ();

#[derive(Debug)]
pub(crate) struct Context;

impl Context {
    pub(crate) fn new(_: RawContext) -> Self {
        Self
    }

    // TODO: fix the async authenticate function
    //
    // #[cfg(feature = "async")]
    // pub(crate) async fn authenticate_async(
    //     &self,
    //     _: Text<'_, '_, '_, '_, '_, '_>,
    //     _: &Policy,
    // ) -> Result<()> {
    //     Err(Error::Unknown)
    // }

    pub(crate) fn authenticate(&self, _: Text, _: &Policy) -> Result<()> {
        Err(Error::Unknown)
    }
}

#[derive(Debug)]
pub(crate) struct Policy;

#[derive(Debug)]
pub(crate) struct PolicyBuilder;

impl PolicyBuilder {
    pub(crate) const fn new() -> Self {
        Self
    }

    pub(crate) const fn biometrics(self, _: Option<BiometricStrength>) -> Self {
        Self
    }

    pub(crate) const fn password(self, _: bool) -> Self {
        Self
    }

    pub(crate) const fn companion(self, _: bool) -> Self {
        Self
    }

    pub(crate) const fn wrist_detection(self, _: bool) -> Self {
        Self
    }

    pub(crate) const fn build(self) -> Option<Policy> {
        None
    }
}
