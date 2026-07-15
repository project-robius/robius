mod delegate;

use std::time::{Duration, SystemTime};

use delegate::RobiusLocationDelegate as Delegate;
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_core_location::{
    kCLLocationAccuracyBest, kCLLocationAccuracyKilometer, CLAuthorizationStatus, CLLocation,
    CLLocationCoordinate2D, CLLocationManager, CLLocationManagerDelegate,
};

use crate::{Access, Accuracy, Coordinates, Handler, Result};

pub(crate) struct Manager {
    inner: Retained<CLLocationManager>,
    // Held for the manager's lifetime, and kept as the concrete type so we can queue up held requests.
    delegate: Retained<Delegate>,
}

impl Manager {
    pub(crate) fn new<T>(handler: T) -> Result<Self>
    where
        T: Handler,
    {
        // Although `CLLocationManager::new()` does not require a MainThreadMarker,
        // it actually does require that it is initialized from the main thread.
        let mtm = objc2::MainThreadMarker::new()
            .ok_or(crate::Error::NotMainThread)?;
        let inner = unsafe { CLLocationManager::new() };
        let delegate = Delegate::new(mtm, handler);
        let protocol: &ProtocolObject<dyn CLLocationManagerDelegate> =
            ProtocolObject::from_ref(&*delegate);
        unsafe { inner.setDelegate(Some(protocol)) };
        Ok(Self { inner, delegate })
    }

    pub(crate) fn request_authorization(&self, access: Access, accuracy: Accuracy) -> Result<()> {
        // Hint the desired precision; `kCLLocationAccuracyKilometer` matches an approximate request.
        let desired = unsafe {
            match accuracy {
                Accuracy::Precise => kCLLocationAccuracyBest,
                Accuracy::Approximate => kCLLocationAccuracyKilometer,
            }
        };
        unsafe { self.inner.setDesiredAccuracy(desired) };
        match access {
            Access::Foreground => unsafe { self.inner.requestWhenInUseAuthorization(); },
            Access::Background => unsafe { self.inner.requestAlwaysAuthorization(); },
        }
        Ok(())
    }

    pub(crate) fn update_once(&self) -> Result<()> {
        // `requestLocation` fails (rather than waiting) if called before authorization is granted,
        // so while the status is undetermined we defer it until `didChangeAuthorization` fires.
        match unsafe { self.inner.authorizationStatus() } {
            CLAuthorizationStatus::NotDetermined => self.delegate.defer_update_once(),
            _ => {
                self.delegate.begin_one_shot(&self.inner); // deliver the cached fix first, then refine
                unsafe { self.inner.requestLocation() };
            }
        }
        Ok(())
    }

    pub(crate) fn start_updates(&self) -> Result<()> {
        match unsafe { self.inner.authorizationStatus() } {
            CLAuthorizationStatus::NotDetermined => self.delegate.defer_start_updates(),
            _ => {
                self.delegate.begin_continuous();
                unsafe { self.inner.startUpdatingLocation() };
            }
        }
        Ok(())
    }

    pub(crate) fn stop_updates(&self) -> Result<()> {
        self.delegate.cancel_deferred_start_updates();
        unsafe { self.inner.stopUpdatingLocation(); }
        Ok(())
    }
}

pub(crate) struct Location<'a> {
    inner: &'a CLLocation,
}

impl Location<'_> {
    pub(crate) fn coordinates(&self) -> Result<Coordinates> {
        let CLLocationCoordinate2D {
            latitude,
            longitude,
        } = unsafe { self.inner.coordinate() };

        Ok(Coordinates {
            latitude,
            longitude,
        })
    }

    pub(crate) fn altitude(&self) -> Result<f64> {
        Ok(unsafe { self.inner.altitude() })
    }

    pub(crate) fn bearing(&self) -> Result<f64> {
        Ok(unsafe { self.inner.course() })
    }

    pub(crate) fn speed(&self) -> Result<f64> {
        Ok(unsafe { self.inner.speed() })
    }

    pub(crate) fn time(&self) -> Result<SystemTime> {
        let secs = unsafe { self.inner.timestamp().timeIntervalSince1970() };
        Ok(SystemTime::UNIX_EPOCH + Duration::from_secs_f64(secs))
    }
}
