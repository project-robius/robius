use std::{
    marker::PhantomData,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use windows::{
    Devices::Geolocation::{
        Geocoordinate, GeolocationAccessStatus, Geolocator, PositionAccuracy,
        PositionChangedEventArgs, PositionStatus, StatusChangedEventArgs,
    },
    Foundation::{EventRegistrationToken, TimeSpan, TypedEventHandler},
};

use crate::{
    cached_fix_is_recent, Access, Accuracy, Coordinates, Error, Freshness, Handler, Result,
    MAX_CACHED_AGE,
};

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
                                    freshness: Freshness::Live,
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
    freshness: Freshness,
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
        system_time_from_winrt(self.inner.Timestamp()?.UniversalTime).ok_or(Error::Unknown)
    }

    pub fn freshness(&self) -> Freshness {
        self.freshness
    }
}

/// Converts a WinRT `DateTime` to a [`SystemTime`].
///
/// `DateTime` is just an `i64` count of 100ns ticks since 1601-01-01 UTC (the `FILETIME` epoch,
/// not the `SYSTEMTIME` struct), so all we do is move it onto the Unix epoch.
/// Returns `None` if that doesn't fit in a `SystemTime`, which shouldn't happen for a real fix.
fn system_time_from_winrt(universal_time: i64) -> Option<SystemTime> {
    /// Ticks between 1601-01-01 and 1970-01-01.
    const UNIX_EPOCH_TICKS: i64 = 11_644_473_600 * TICKS_PER_SEC;
    const TICKS_PER_SEC: i64 = 10_000_000;

    let ticks = universal_time.checked_sub(UNIX_EPOCH_TICKS)?;
    // Split into whole seconds + leftover ticks so we don't overflow the nanosecond count.
    let magnitude = ticks.unsigned_abs();
    let (secs, sub_sec_ticks) = (
        magnitude / TICKS_PER_SEC as u64,
        (magnitude % TICKS_PER_SEC as u64) as u32,
    );
    let delta = Duration::new(secs, sub_sec_ticks * 100);
    if ticks >= 0 {
        UNIX_EPOCH.checked_add(delta)
    } else {
        // A fix from before 1970 is nonsense, but the arithmetic is well-defined, so allow it.
        UNIX_EPOCH.checked_sub(delta)
    }
}

fn get_location(geolocator: &Geolocator) -> Result<crate::Location<'_>> {
    Ok(crate::Location {
        inner: Location {
            inner: geolocator.GetGeopositionAsync()?.get()?.Coordinate()?,
            freshness: Freshness::Live,
            _phantom_data: PhantomData,
        },
    })
}

// A recently-cached fix, returned near-instantly (short timeout = don't acquire a new one).
fn get_cached_location(geolocator: &Geolocator) -> Result<crate::Location<'_>> {
    // Ask WinRT for the same bound we enforce ourselves. It can still hand back something older,
    // since it uses whichever is larger of this and an age derived from the accuracy setting.
    let max_age = TimeSpan { Duration: MAX_CACHED_AGE.as_secs() as i64 * 10_000_000 };
    let timeout = TimeSpan { Duration: 1_000_000 };         // 100ms: return cached, don't acquire anew
    let location = Location {
        inner: geolocator
            .GetGeopositionAsyncWithAgeAndTimeout(max_age, timeout)?
            .get()?
            .Coordinate()?,
        freshness: Freshness::Cached,
        _phantom_data: PhantomData,
    };
    // So check the age ourselves, and drop it rather than pass off an ancient fix as current.
    if !cached_fix_is_recent(location.time().ok()) {
        return Err(Error::TemporarilyUnavailable);
    }
    Ok(crate::Location { inner: location })
}

impl From<windows::core::Error> for Error {
    fn from(_: windows::core::Error) -> Self {
        Error::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ticks from 1601-01-01 to 1970-01-01, i.e. what `DateTime` holds at the Unix epoch.
    const EPOCH: i64 = 11_644_473_600 * 10_000_000;

    fn unix_secs(universal_time: i64) -> f64 {
        let t = system_time_from_winrt(universal_time).unwrap();
        match t.duration_since(UNIX_EPOCH) {
            Ok(d) => d.as_secs_f64(),
            Err(e) => -e.duration().as_secs_f64(),
        }
    }

    #[test]
    fn unix_epoch_round_trips() {
        assert_eq!(system_time_from_winrt(EPOCH), Some(UNIX_EPOCH));
    }

    #[test]
    fn converts_a_real_timestamp() {
        // 2024-01-01T00:00:00Z.
        let ticks = EPOCH + 1_704_067_200 * 10_000_000;
        assert_eq!(unix_secs(ticks), 1_704_067_200.0);
    }

    #[test]
    fn keeps_sub_second_precision() {
        // 100ns is finer than a nanosecond count can lose, so this must come back exactly.
        let t = system_time_from_winrt(EPOCH + 1_234_567).unwrap();
        assert_eq!(
            t.duration_since(UNIX_EPOCH).unwrap(),
            Duration::from_nanos(123_456_700)
        );
    }

    #[test]
    fn handles_times_before_the_unix_epoch() {
        // Tick 0 is 1601-01-01, the start of the WinRT epoch.
        assert_eq!(unix_secs(0), -11_644_473_600.0);
    }

    #[test]
    fn rejects_values_that_cannot_be_shifted() {
        assert_eq!(system_time_from_winrt(i64::MIN), None);
    }
}
