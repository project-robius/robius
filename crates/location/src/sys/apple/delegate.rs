use objc2::{
    define_class, msg_send, rc::Retained, DeclaredClass, MainThreadMarker, MainThreadOnly,
};
use objc2_core_location::{CLError, CLLocation, CLLocationManager, CLLocationManagerDelegate};
use objc2_foundation::{NSArray, NSError, NSObject, NSObjectProtocol};

use super::Location;
use crate::{Error, Handler};

type InnerHandler = dyn Handler;

pub(super) struct Ivars {
    handler: Box<InnerHandler>,
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
            for location in locations.iter() {
                self.ivars().handler.handle(
                    crate::Location {
                        inner: Location {
                            inner: &location,
                        },
                    }
                );
            }
        }

        #[unsafe(method(locationManager:didFailWithError:))]
        #[allow(non_snake_case)]
        unsafe fn locationManager_didFailWithError(&self, _: &CLLocationManager, error: &NSError) {
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
    }
);


impl RobiusLocationDelegate {
    /// Allocates a new `RobiusLocationDelegate` and initializes it with the given handler
    /// to be called upon location updates and errors.
    pub(super) fn new<T: Handler>(mtm: MainThreadMarker, handler: T) -> Retained<Self> {
        let this = Self::alloc(mtm)
            .set_ivars(Ivars {
                handler: Box::new(handler),
            });
        unsafe { msg_send![super(this), init] }
    }
}
