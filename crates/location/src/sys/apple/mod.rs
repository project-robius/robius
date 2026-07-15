mod delegate;

use std::time::{Duration, SystemTime};

use delegate::RobiusLocationDelegate as Delegate;
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_core_location::{
    kCLLocationAccuracyBest, kCLLocationAccuracyKilometer, CLAuthorizationStatus, CLLocation,
    CLLocationCoordinate2D, CLLocationManager, CLLocationManagerDelegate,
};
use objc2_foundation::{NSBundle, NSString};

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
        // `requestLocation` won't wait if called before authorization, so while it's undetermined we
        // defer — but only if we can actually prompt; otherwise let `requestLocation` fail now.
        if self.undetermined() && can_request_location_authorization() {
            self.delegate.defer_update_once();
        } else {
            self.delegate.begin_one_shot(&self.inner); // deliver the cached fix first, then refine
            unsafe { self.inner.requestLocation() };
        }
        Ok(())
    }

    pub(crate) fn start_updates(&self) -> Result<()> {
        if self.undetermined() && can_request_location_authorization() {
            self.delegate.defer_start_updates();
        } else {
            self.delegate.begin_continuous();
            unsafe { self.inner.startUpdatingLocation() };
        }
        Ok(())
    }

    fn undetermined(&self) -> bool {
        matches!(
            unsafe { self.inner.authorizationStatus() },
            CLAuthorizationStatus::NotDetermined
        )
    }

    pub(crate) fn stop_updates(&self) -> Result<()> {
        self.delegate.cancel_deferred_start_updates();
        unsafe { self.inner.stopUpdatingLocation(); }
        Ok(())
    }
}

// Can we actually prompt for location? Only if the app has a usage-description key in its Info.plist.
// A binary run directly (not in a .app) has none, so it can never get authorized.
fn can_request_location_authorization() -> bool {
    let bundle = NSBundle::mainBundle();
    [
        "NSLocationWhenInUseUsageDescription",
        "NSLocationAlwaysAndWhenInUseUsageDescription",
        "NSLocationAlwaysUsageDescription",
    ]
    .iter()
    .any(|key| bundle.objectForInfoDictionaryKey(&NSString::from_str(key)).is_some())
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

#[cfg(test)]
mod tests {
    // A `cargo test` binary is unpackaged (no Info.plist usage description), like running the app
    // directly — so it must report that it can't prompt, so we error instead of deferring forever.
    #[test]
    fn unpackaged_binary_cannot_request_location() {
        assert!(!super::can_request_location_authorization());
    }
}
