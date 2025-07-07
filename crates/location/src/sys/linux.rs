use std::marker::PhantomData;
use std::time::SystemTime;

use crate::{Access, Accuracy, Coordinates, Handler, Result, Error};

pub(crate) struct Manager;

impl Manager {
    pub fn new<T>(_handler: T) -> Result<Self>
    where
        T: Handler,
    {
        Ok(Self)
    }

    pub fn request_authorization(&self, _access: Access, _accuracy: Accuracy) -> Result<()> {
        Err(Error::PermanentlyUnavailable)
    }

    pub fn update_once(&self) -> Result<()> {
        Ok(())
    }

    pub fn start_updates(&self) -> Result<()> {
        Ok(())
    }

    pub fn stop_updates(&self) -> Result<()> {
        Ok(())
    }
}

pub struct Location<'a> {
    _phantom_data: PhantomData<&'a ()>,
}

impl Location<'_> {
    pub fn coordinates(&self) -> Result<Coordinates> {
        Err(Error::PermanentlyUnavailable)
    }

    pub fn altitude(&self) -> Result<f64> {
        Err(Error::PermanentlyUnavailable)
    }

    pub fn bearing(&self) -> Result<f64> {
        Err(Error::PermanentlyUnavailable)
    }

    pub fn speed(&self) -> Result<f64> {
        Err(Error::PermanentlyUnavailable)
    }

    pub fn time(&self) -> Result<SystemTime> {
        Err(Error::PermanentlyUnavailable)
    }
}
