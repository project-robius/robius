use std::cell::Cell;

use objc2::{
    define_class, msg_send, rc::Retained, DeclaredClass, MainThreadMarker, MainThreadOnly,
};
use objc2_core_location::{
    CLAuthorizationStatus, CLError, CLLocation, CLLocationManager, CLLocationManagerDelegate,
};
use objc2_foundation::{NSArray, NSError, NSObject, NSObjectProtocol};

use super::Location;
use crate::{Error, Handler};

type InnerHandler = dyn Handler;

/// Location requests we're holding until permission is granted. `requestLocation` just fails if
/// called before that, so we replay it from `didChangeAuthorization` once we're allowed.
#[derive(Clone, Copy, Default)]
struct Pending {
    update_once: bool,
    start_updates: bool,
}

pub(super) struct Ivars {
    handler: Box<InnerHandler>,
    // Only ever touched on the main thread (the delegate is `MainThreadOnly`).
    pending: Cell<Pending>,
    // While `one_shot`, deliver only fixes newer than `last_delivered` (never go backwards).
    one_shot: Cell<bool>,
    last_delivered: Cell<f64>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    pub(super) struct RobiusLocationDelegate;

    unsafe impl NSObjectProtocol for RobiusLocationDelegate {}

    unsafe impl CLLocationManagerDelegate for RobiusLocationDelegate {
        #[unsafe(method(locationManager:didUpdateLocations:))]
        #[allow(non_snake_case)]
        unsafe fn locationManager_didUpdateLocations(
            &self,
            _: &CLLocationManager,
            locations: &NSArray<CLLocation>,
        ) {
            // Note: `.iter()` here requires objc2-foundation's `NSEnumerator` feature.
            let one_shot = self.ivars().one_shot.get();
            for location in locations.iter() {
                if one_shot {
                    self.deliver_if_newer(&location);
                } else {
                    self.deliver(&location); // continuous updates: just pass every fix along
                }
            }
        }

        #[unsafe(method(locationManager:didFailWithError:))]
        #[allow(non_snake_case)]
        unsafe fn locationManager_didFailWithError(&self, _: &CLLocationManager, error: &NSError) {
            // In a one-shot that already delivered a cached fix, don't overwrite it with an error.
            if self.ivars().one_shot.get() && self.ivars().last_delivered.get().is_finite() {
                return;
            }
            self.ivars().handler.error(match CLError(error.code()) {
                // kCLErrorLocationUnknown
                CLError::LocationUnknown => Error::TemporarilyUnavailable,
                // kCLErrorDenied
                CLError::Denied => Error::AuthorizationDenied,
                // kCLErrorNetwork
                CLError::Network => Error::Network,
                _ => Error::Unknown,
            })
        }

        // Fires with the initial state and on every change; replays a deferred request once granted.
        #[unsafe(method(locationManagerDidChangeAuthorization:))]
        #[allow(non_snake_case)]
        unsafe fn locationManagerDidChangeAuthorization(&self, manager: &CLLocationManager) {
            let status = unsafe { manager.authorizationStatus() };
            self.authorization_changed(manager, status);
        }
    }
);

impl RobiusLocationDelegate {
    /// Allocates a new `RobiusLocationDelegate` and initializes it with the given handler
    /// to be called upon location updates and errors.
    pub(super) fn new<T: Handler>(mtm: MainThreadMarker, handler: T) -> Retained<Self> {
        let this = Self::alloc(mtm)
            .set_ivars(Ivars {
                handler: Box::new(handler),
                pending: Cell::new(Pending::default()),
                one_shot: Cell::new(false),
                last_delivered: Cell::new(f64::NEG_INFINITY),
            });
        unsafe { msg_send![super(this), init] }
    }

    fn deliver(&self, location: &CLLocation) {
        self.ivars().handler.handle(crate::Location { inner: Location { inner: location } });
    }

    /// Delivers only if newer than the last fix this one-shot, so it never jumps backwards.
    fn deliver_if_newer(&self, location: &CLLocation) {
        let ts = unsafe { location.timestamp().timeIntervalSince1970() };
        if ts <= self.ivars().last_delivered.get() {
            return;
        }
        self.ivars().last_delivered.set(ts);
        self.deliver(location);
    }

    /// Begins a one-shot: resets the guard and delivers the cached fix first, before `requestLocation` refines.
    pub(super) fn begin_one_shot(&self, manager: &CLLocationManager) {
        self.ivars().last_delivered.set(f64::NEG_INFINITY);
        self.ivars().one_shot.set(true);
        if let Some(location) = unsafe { manager.location() } {
            self.deliver_if_newer(&location);
        }
    }

    /// Start continuous updates; from here we just pass every fix along.
    pub(super) fn begin_continuous(&self) {
        self.ivars().one_shot.set(false);
    }

    /// Holds a one-shot request until permission is granted.
    pub(super) fn defer_update_once(&self) {
        let mut pending = self.ivars().pending.get();
        pending.update_once = true;
        self.ivars().pending.set(pending);
    }

    /// Holds off on continuous updates until permission is granted.
    pub(super) fn defer_start_updates(&self) {
        let mut pending = self.ivars().pending.get();
        pending.start_updates = true;
        self.ivars().pending.set(pending);
    }

    /// Cancels a held `start_updates`, so granting later won't start updates (a held one-shot stays).
    pub(super) fn cancel_deferred_start_updates(&self) {
        let mut pending = self.ivars().pending.get();
        pending.start_updates = false;
        self.ivars().pending.set(pending);
    }

    fn authorization_changed(&self, manager: &CLLocationManager, status: CLAuthorizationStatus) {
        match status {
            CLAuthorizationStatus::AuthorizedWhenInUse | CLAuthorizationStatus::AuthorizedAlways => {
                let pending = self.ivars().pending.replace(Pending::default());
                // Run the one-shot first, so a held `start_updates` leaves us in continuous mode.
                if pending.update_once {
                    self.begin_one_shot(manager);
                    unsafe { manager.requestLocation() };
                }
                if pending.start_updates {
                    self.begin_continuous();
                    unsafe { manager.startUpdatingLocation() };
                }
            }
            CLAuthorizationStatus::Denied | CLAuthorizationStatus::Restricted => {
                // Only report a denial if a request was pending, else the initial callback would
                // fire a spurious error.
                let pending = self.ivars().pending.replace(Pending::default());
                if pending.update_once || pending.start_updates {
                    self.ivars().handler.error(Error::AuthorizationDenied);
                }
            }
            // `NotDetermined` (or any future state): keep waiting, leave any pending request queued.
            _ => {}
        }
    }
}
