//! A library to access system location data.
//!
//! ## Linux
//!
//! Uses the XDG Desktop Portal, falling back to GeoClue directly if the portal isn't there (never
//! after a portal denial). No setup needed on a normal desktop install; see the README for what
//! minimal distros need. Both [`Access`] variants map to the same portal permission, and one-shot
//! requests give up after 60 seconds.
//!
//! **Note:** every `Manager` method blocks on D-Bus round trips. Usually milliseconds, but a wedged
//! service can stall [`Manager::new`] for 45s and the rest for 30s (portal) or 45s (GeoClue), so
//! keep them off your UI thread.
//!
//! If the provider dies, you get [`Error::TemporarilyUnavailable`] and updates stop. The next call
//! reconnects, so just retry. Nothing carries over though, so the user may be asked to authorize
//! again.
//!
//! ## Android
//!
//! On Android the following must be added to the manifest:
//! ```xml
//! <manifest ... >
//!   <!-- Always include this permission -->
//!   <uses-permission android:name="android.permission.ACCESS_COARSE_LOCATION" />
//!
//!   <!-- Include only if your app benefits from precise location access. -->
//!   <uses-permission android:name="android.permission.ACCESS_FINE_LOCATION" />
//! </manifest>
//! ```
//! As specified in the [Android documentation][android-docs].
//!
//! ### Minimum API level
//!
//! The minimum supported Android API level is **26 (Android 8.0)**: the bundled
//! Java helper is loaded via `InMemoryDexClassLoader`, which requires API 26.
//! Newer location APIs (e.g. `getCurrentLocation`) are used only when the device
//! supports them, with a fallback for older versions, so set `minSdk` to at least
//! 26 in your app.
//!
//! [android-docs]: https://developer.android.com/develop/sensors-and-location/location/permissions

mod error;
mod sys;

use std::time::{Duration, SystemTime};

pub use crate::error::{Error, Result};

/// How old a cached fix may be before we drop it instead of handing it over.
///
/// Every backend uses this same bound, so a [`Freshness::Cached`] fix means the same thing
/// everywhere. Deliberately generous: a cached fix is only ever the first of two deliveries, and
/// dropping it just costs the caller the instant answer they would otherwise have had.
// The allow here and on the check below is for platforms with no backend, which use neither.
#[allow(dead_code)]
pub(crate) const MAX_CACHED_AGE: Duration = Duration::from_secs(60 * 60);

/// Whether a cached fix is recent enough to bother delivering.
///
/// A fix we can't date is rejected, and so is one dated in the future — if we can't tell how old it
/// is, we don't hand it back as if it were current. This only ever gates the cached shortcut; a live
/// fix is delivered whatever its timestamp says.
#[allow(dead_code)]
pub(crate) fn cached_fix_is_recent(time: Option<SystemTime>) -> bool {
    time.and_then(|time| SystemTime::now().duration_since(time).ok())
        .is_some_and(|age| age <= MAX_CACHED_AGE)
}

/// A manager for dealing with location data and handling location updates.
///
/// All functions must be called from the main thread, except on Linux, where any thread works
/// (including from inside a handler callback).
///
/// As soon as the handler is registered, it may receive updates immediately,
/// even if `update_once` or `start_updates` are not called.
/// When the manager is dropped, the handler is no longer guaranteed to receive updates.
pub struct Manager {
    inner: sys::Manager,
}

impl Manager {
    /// Creates a new location manager with the given handler.
    ///
    /// Must be called from the main thread, except on Linux.
    pub fn new<T>(handler: T) -> Result<Self>
    where
        T: Handler,
    {
        Ok(Manager {
            inner: sys::Manager::new(handler)?,
        })
    }

    /// Like [`Manager::new`], but with an explicit GeoClue desktop ID.
    ///
    /// Prefer [`Manager::new`], which works this out itself. This is an escape hatch for odd or
    /// legacy GeoClue setups. Pass an installed desktop-file ID without the `.desktop` suffix
    /// (ASCII letters, digits, `.`, `_`, `-`); it's validated either way.
    ///
    /// Available everywhere so cross-platform code can call it unconditionally; off Linux the ID is
    /// ignored.
    #[cfg(target_os = "linux")]
    pub fn new_with_desktop_id<T>(handler: T, desktop_id: &str) -> Result<Self>
    where
        T: Handler,
    {
        Ok(Manager {
            inner: sys::Manager::new_with_desktop_id(handler, desktop_id)?,
        })
    }

    /// See the Linux variant; off Linux the desktop ID is ignored.
    #[cfg(not(target_os = "linux"))]
    pub fn new_with_desktop_id<T>(handler: T, _desktop_id: &str) -> Result<Self>
    where
        T: Handler,
    {
        Self::new(handler)
    }

    /// Requests authorization to access location data.
    ///
    /// Linux (portal), iOS, and Android return immediately; a request made beforehand waits for the
    /// user, and a denial goes to [`Handler::error`]. Linux (GeoClue) and Windows block and return
    /// the result.
    pub fn request_authorization(&self, access: Access, accuracy: Accuracy) -> Result<()> {
        self.inner.request_authorization(access, accuracy)
    }

    /// Requests the device's current location, delivered to the handler. May deliver a cached fix
    /// immediately, then a fresher one once acquired; [`Location::freshness`] says which is which.
    /// On Linux, gives up after 60 seconds with [`Error::TemporarilyUnavailable`] if nothing
    /// arrives.
    pub fn update_once(&self) -> Result<()> {
        self.inner.update_once()
    }

    /// Begins delivering continuous updates to the handler.
    pub fn start_updates(&mut self) -> Result<()> {
        self.inner.start_updates()
    }

    /// Stops delivering continuous updates to the handler.
    pub fn stop_updates(&mut self) -> Result<()> {
        self.inner.stop_updates()
    }
}

/// A handler that handles location events and errors.
///
/// The handler should be registered with [`Manager::new`].
pub trait Handler: 'static + Send + Sync {
    fn handle(&self, location: Location<'_>);

    fn error(&self, error: Error);
}

/// Data about the device's current whereabouts.
///
/// Despite the name, `Location` contains more than just the location of the
/// device. See the methods for all available information.
pub struct Location<'a> {
    inner: sys::Location<'a>,
}

impl Location<'_> {
    pub fn coordinates(&self) -> Result<Coordinates> {
        self.inner.coordinates()
    }

    pub fn altitude(&self) -> Result<f64> {
        self.inner.altitude()
    }

    /// The direction in which the device is travelling, measured in degrees and
    /// relative to due north.
    pub fn bearing(&self) -> Result<f64> {
        self.inner.bearing()
    }

    /// The instantaneous speed of the device measured in meters per second.
    pub fn speed(&self) -> Result<f64> {
        self.inner.speed()
    }

    /// The time at which the location was acquired.
    ///
    /// Every platform reports this against the same clock: the system's wall clock, in UTC, counted
    /// from the Unix epoch. The resolution underneath differs (milliseconds on Android, microseconds
    /// on Linux, 100ns on Windows, a float of seconds on Apple), but the meaning doesn't.
    ///
    /// Being wall-clock time, it can jump if the clock is adjusted, so don't use differences between
    /// two of these to measure elapsed time.
    ///
    /// On Linux this is [`Error::TemporarilyUnavailable`] if the provider didn't send a timestamp.
    pub fn time(&self) -> Result<SystemTime> {
        self.inner.time()
    }

    /// Whether the OS had this fix on hand already, or measured it for this request.
    pub fn freshness(&self) -> Freshness {
        self.inner.freshness()
    }

    /// Shorthand for `self.freshness() == Freshness::Cached`.
    pub fn is_cached(&self) -> bool {
        self.freshness() == Freshness::Cached
    }
}

/// Where a fix came from.
///
/// [`Manager::update_once`] hands back whatever the OS already had before it hands back the fix it
/// goes on to acquire, so one request can reach the handler twice. This is how you tell the two
/// apart — ignore the cached one if you only want a real measurement, or take it and stop if you
/// wanted an answer fast.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Freshness {
    /// A fix the OS already had, handed back right away without measuring anything.
    ///
    /// Never more than an hour old, on every platform; call [`Location::time`] if you need to know
    /// how old exactly. A [`Freshness::Live`] fix usually follows, but if the OS never manages one,
    /// a one-shot can end here without an error.
    Cached,
    /// A fix the OS went and got for this request. Everything from [`Manager::start_updates`] is
    /// one too.
    ///
    /// What makes it live is that we asked for a new fix, not that a sensor definitely ran — some
    /// platforms will answer a request like that from a very recent fix of their own.
    Live,
}

#[derive(Copy, Clone, Debug)]
pub struct Coordinates {
    pub latitude: f64,
    pub longitude: f64,
}

/// The kind of location access.
#[derive(Copy, Clone, Debug)]
pub enum Access {
    Foreground,
    Background,
}

/// The accuracy of the location data.
#[derive(Copy, Clone, Debug)]
pub enum Accuracy {
    /// Approximate location accuracy.
    ///
    /// Corresponds to
    /// [`ACCESS_COARSE_LOCATION`](https://developer.android.com/reference/android/Manifest.permission#ACCESS_COARSE_LOCATION)
    /// on Android.
    Approximate,
    /// Precise location accuracy.
    ///
    /// Corresponds to
    /// [`ACCESS_FINE_LOCATION`](https://developer.android.com/reference/android/Manifest.permission#ACCESS_FINE_LOCATION)
    /// on Android.
    Precise,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_cached_fixes_are_usable() {
        assert!(cached_fix_is_recent(Some(SystemTime::now())));
        assert!(cached_fix_is_recent(Some(
            SystemTime::now() - Duration::from_secs(30)
        )));
        assert!(cached_fix_is_recent(Some(
            SystemTime::now() - MAX_CACHED_AGE + Duration::from_secs(60)
        )));
    }

    #[test]
    fn stale_undated_and_future_fixes_are_not() {
        assert!(!cached_fix_is_recent(Some(
            SystemTime::now() - MAX_CACHED_AGE - Duration::from_secs(1)
        )));
        // No timestamp means we can't tell how old it is, so we don't pass it off as current.
        assert!(!cached_fix_is_recent(None));
        // Neither is a fix from the future, which means the clock moved under us.
        assert!(!cached_fix_is_recent(Some(
            SystemTime::now() + Duration::from_secs(60)
        )));
    }
}
