use std::{
    marker::PhantomData,
    sync::{Arc, Mutex},
    time::SystemTime,
};

use windows::{
    Devices::Geolocation::{
        Geocoordinate, GeolocationAccessStatus, Geolocator, PositionAccuracy,
        PositionChangedEventArgs, PositionStatus, StatusChangedEventArgs,
    },
    Foundation::{EventRegistrationToken, TimeSpan, TypedEventHandler},
};

use crate::{Access, Accuracy, Coordinates, Error, Handler, Result};

pub(crate) struct Manager {
    inner: Arc<Geolocator>,
    // Delivers status transitions and errors.
    status_handler: TypedEventHandler<Geolocator, StatusChangedEventArgs>,
    // Delivers continuous position updates (`StatusChanged` alone never streams positions).
    position_handler: TypedEventHandler<Geolocator, PositionChangedEventArgs>,
    // NOTE: Technically the Mutex isn't necessary, but removing it requires some finnicky unsafe.
    rust_handler: Arc<Mutex<dyn Handler>>,
    status_token: Option<EventRegistrationToken>,
    position_token: Option<EventRegistrationToken>,
}

impl Manager {
    pub fn new<T>(handler: T) -> Result<Self>
    where
        T: Handler,
    {
        let geolocator = Arc::new(Geolocator::new()?);
        let rust_handler = Arc::new(Mutex::new(handler));
        let rust_handler_cloned = rust_handler.clone();

        let status_handler: TypedEventHandler<Geolocator, StatusChangedEventArgs> =
            TypedEventHandler::new(
                move |_geolocator: &Option<Geolocator>, status: &Option<StatusChangedEventArgs>| {
                    if let Ok(handler) = rust_handler_cloned.lock() {
                        match status.as_ref() {
                            Some(status) => match status.Status() {
                                Ok(status) => match status {
                                    // `position_handler` owns location delivery; this only reports status.
                                    PositionStatus::Ready => {}
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

        let rust_handler_position = rust_handler.clone();
        let position_handler: TypedEventHandler<Geolocator, PositionChangedEventArgs> =
            TypedEventHandler::new(
                move |_geolocator: &Option<Geolocator>, args: &Option<PositionChangedEventArgs>| {
                    if let Ok(handler) = rust_handler_position.lock() {
                        if let Some(coordinate) = args
                            .as_ref()
                            .and_then(|args| args.Position().ok())
                            .and_then(|position| position.Coordinate().ok())
                        {
                            handler.handle(crate::Location {
                                inner: Location {
                                    inner: coordinate,
                                    _phantom_data: PhantomData,
                                },
                            });
                        }
                    }

                    Ok(())
                },
            );

        Ok(Self {
            inner: geolocator,
            status_handler,
            position_handler,
            rust_handler,
            status_token: None,
            position_token: None,
        })
    }

    pub fn request_authorization(&self, _access: Access, accuracy: Accuracy) -> Result<()> {
        // Hint the desired precision (best-effort): `High` for precise, `Default` for approximate.
        let desired = match accuracy {
            Accuracy::Precise => PositionAccuracy::High,
            Accuracy::Approximate => PositionAccuracy::Default,
        };
        let _ = self.inner.SetDesiredAccuracy(desired);
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
                // Cached-first, then refine; error only if nothing was delivered (never hang).
                let delivered_cached = match get_cached_location(inner_cloned.as_ref()) {
                    Ok(location) => { handler.handle(location); true }
                    Err(_) => false,
                };
                match get_location(inner_cloned.as_ref()) {
                    Ok(location) => handler.handle(location),
                    Err(e) => if !delivered_cached { handler.error(e); }
                }
            }
        });

        Ok(())
    }

    pub fn start_updates(&mut self) -> Result<()> {
        self.stop_updates()?; // idempotent: drop any earlier registrations first

        // Hint the provider to stream positions (else PositionChanged may fire once); may fail.
        let _ = self.inner.SetReportInterval(1000);

        self.position_token = Some(self.inner.PositionChanged(&self.position_handler)?);
        self.status_token = Some(self.inner.StatusChanged(&self.status_handler)?);
        Ok(())
    }

    pub fn stop_updates(&mut self) -> Result<()> {
        if let Some(token) = self.position_token.take() {
            self.inner.RemovePositionChanged(token)?;
        }
        if let Some(token) = self.status_token.take() {
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

fn get_location(geolocator: &Geolocator) -> Result<crate::Location<'_>> {
    Ok(crate::Location {
        inner: Location {
            inner: geolocator.GetGeopositionAsync()?.get()?.Coordinate()?,
            _phantom_data: PhantomData,
        },
    })
}

// A recently-cached fix, returned near-instantly (short timeout = don't acquire a new one).
fn get_cached_location(geolocator: &Geolocator) -> Result<crate::Location<'_>> {
    let max_age = TimeSpan { Duration: 3600 * 10_000_000 }; // accept a fix up to ~1h old
    let timeout = TimeSpan { Duration: 1_000_000 };         // 100ms: return cached, don't acquire anew
    Ok(crate::Location {
        inner: Location {
            inner: geolocator
                .GetGeopositionAsyncWithAgeAndTimeout(max_age, timeout)?
                .get()?
                .Coordinate()?,
            _phantom_data: PhantomData,
        },
    })
}

impl From<windows::core::Error> for Error {
    fn from(_: windows::core::Error) -> Self {
        Error::Unknown
    }
}
