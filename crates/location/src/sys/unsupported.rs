use std::marker::PhantomData;

use crate::{Access, Accuracy, Coordinates, Handler, Result};

pub(crate) struct Manager;

impl Manager {
    pub fn new<T>(_handler: T) -> Result<Self>
    where
        T: Handler,
    {
        Err(Error::Unknown)
    }

    pub fn request_authorization(&self, _access: Access, _accuracy: Accuracy) -> Result<()> {
        Err(Error::Unknown)
    }

    pub fn update_once(&self) -> Result<()> {
        Err(Error::Unknown)
    }

    pub fn start_updates(&self) -> Result<()> {
        Err(Error::Unknown)
    }

    pub fn stop_updates(&self) -> Result<()> {
        Err(Error::Unknown)
    }
}

pub struct Location<'a> {
    _phantom_data: PhantomData<&'a ()>,
}

impl Location<'_> {
    pub fn coordinates(&self) -> Result<Coordinates> {
        Err(Error::Unknown)
    }

    pub fn altitude(&self) -> Result<f64> {
        Err(Error::Unknown)
    }

    pub fn bearing(&self) -> Result<f64> {
        Err(Error::Unknown)
    }

    pub fn speed(&self) -> Result<f64> {
        Err(Error::Unknown)
    }

    pub fn time(&self) -> Result<SystemTime> {
        Err(Error::Unknown)
    }
}
