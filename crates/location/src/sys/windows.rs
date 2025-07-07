use std::{
    marker::PhantomData,
    sync::{Arc, Mutex},
    time::SystemTime,
};

use windows::{
    Devices::Geolocation::{
        Geocoordinate, GeolocationAccessStatus, Geolocator, PositionStatus, StatusChangedEventArgs,
    },
    Foundation::{EventRegistrationToken, TypedEventHandler},
};

use crate::{Access, Accuracy, Coordinates, Error, Handler, Result};

pub(crate) struct Manager {
    inner: Arc<Geolocator>,
    handler: TypedEventHandler<Geolocator, StatusChangedEventArgs>,
    // NOTE: Technically the Mutex isn't necessary, but removing it requires some finnicky unsafe.
    rust_handler: Arc<Mutex<dyn Handler>>,
    token: Option<EventRegistrationToken>,
}

impl Manager {
    pub fn new<T>(handler: T) -> Result<Self>
    where
        T: Handler,
    {
        let geolocator = Arc::new(Geolocator::new()?);
        let rust_handler = Arc::new(Mutex::new(handler));
        let rust_handler_cloned = rust_handler.clone();

        let event_handler: TypedEventHandler<Geolocator, StatusChangedEventArgs> =
            TypedEventHandler::new(
                move |geolocator: &Option<Geolocator>, status: &Option<StatusChangedEventArgs>| {
                    if let Ok(handler) = rust_handler_cloned.lock() {
                        match status.as_ref() {
                            Some(status) => match status.Status() {
                                Ok(status) => match status {
                                    PositionStatus::Ready => {
                                        if let Some(geolocator) = geolocator.as_ref() {
                                            if let Ok(location) = get_location(geolocator) {
                                                handler.handle(location)
                                            }
                                        } else {
                                            handler.error(Error::Unknown);
                                        }
                                    }
                                    PositionStatus::Initializing => {}
                                    PositionStatus::NoData => {
                                        handler.error(Error::TemporarilyUnavailable)
                                    }
                                    PositionStatus::Disabled => {
                                        handler.error(Error::AuthorizationDenied)
                                    }
                                    // PositionStatus::NotInitialized => {}
                                    PositionStatus::NotAvailable => {
                                        handler.error(Error::PermanentlyUnavailable)
                                    }
                                    _ => handler.error(Error::Unknown),
                                },
                                Err(_) => handler.error(Error::Unknown),
                            },
                            None => handler.error(Error::Unknown),
                        }
                    }

                    Ok(())
                },
            );

        Ok(Self {
            inner: geolocator,
            handler: event_handler,
            rust_handler,
            token: None,
        })
    }

    pub fn request_authorization(&self, _access: Access, _accuracy: Accuracy) -> Result<()> {
        match Geolocator::RequestAccessAsync()?.get()? {
            GeolocationAccessStatus::Allowed => Ok(()),
            GeolocationAccessStatus::Denied => Err(Error::AuthorizationDenied),
            _ => Err(Error::Unknown),
        }
    }

    pub fn update_once(&self) -> Result<()> {
        #[cfg(not(feature = "async"))]
        use std::thread::spawn;

        #[cfg(feature = "async")]
        use tokio::task::spawn_blocking as spawn;

        let handler = self.rust_handler.clone();
        let inner_cloned = self.inner.clone();

        spawn(move || {
            if let Ok(handler) = handler.lock() {
                if let Ok(location) = get_location(inner_cloned.as_ref()) {
                    handler.handle(location)
                }
            }
        });

        Ok(())
    }

    pub fn start_updates(&mut self) -> Result<()> {
        let token = self.inner.StatusChanged(&self.handler)?;
        self.token = Some(token);
        Ok(())
    }

    pub fn stop_updates(&mut self) -> Result<()> {
        if let Some(token) = self.token.take() {
            self.inner.RemoveStatusChanged(token)?;
        }
        Ok(())
    }
}

impl Drop for Manager {
    fn drop(&mut self) {
        let _ = self.stop_updates();
    }
}

pub struct Location<'a> {
    inner: Geocoordinate,
    _phantom_data: PhantomData<&'a ()>,
}

impl Location<'_> {
    pub fn coordinates(&self) -> Result<Coordinates> {
        Ok(Coordinates {
            latitude: self.inner.Latitude()?,
            longitude: self.inner.Longitude()?,
        })
    }

    pub fn altitude(&self) -> Result<f64> {
        self.inner.Altitude()?.Value().map_err(|e| e.into())
    }

    pub fn bearing(&self) -> Result<f64> {
        self.inner.Heading()?.Value().map_err(|e| e.into())
    }

    pub fn speed(&self) -> Result<f64> {
        self.inner.Speed()?.Value().map_err(|e| e.into())
    }

    pub fn time(&self) -> Result<SystemTime> {
        // TODO
        // Of the form:
        // https://learn.microsoft.com/en-us/windows/win32/api/minwinbase/ns-minwinbase-systemtime
        // which is non-trivial to convert to unix time so that we can convert to
        // SystemTime let _ = self
        //     .inner
        //     .Timestamp()?
        //     .UniversalTime
        //     .try_into()
        //     .map_err(|_| Error::Unknown)?;
        Err(Error::Unknown)
    }
}

fn get_location(geolocator: &Geolocator) -> Result<crate::Location> {
    Ok(crate::Location {
        inner: Location {
            inner: geolocator.GetGeopositionAsync()?.get()?.Coordinate()?,
            _phantom_data: PhantomData,
        },
    })
}

impl From<windows::core::Error> for Error {
    fn from(_: windows::core::Error) -> Self {
        Error::Unknown
    }
}
