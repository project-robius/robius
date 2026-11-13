//! Get the system's light or dark theme, and subscribe to changes.
//!
//! * [`query`] returns the current theme.
//! * [`subscribe`] calls you back with the current theme, and again each time
//!   it changes, until you drop the returned [`Subscription`].
//!
//! The system reports each change, so nothing here runs a timer or checks the
//! theme on a schedule. The crate doesn't depend on any UI toolkit.
//!
//! ```no_run
//! use robius_theme::Theme;
//!
//! let theme = robius_theme::query()?.unwrap_or(Theme::Light);
//! let _subscription = robius_theme::subscribe(|theme| {
//!     // This runs on a system thread, so send the theme to your UI thread.
//!     println!("System theme: {theme:?}");
//! })?;
//! # Ok::<(), robius_theme::Error>(())
//! ```
//!
//! The callback runs on whichever thread the system reports changes on, which
//! is often the app's main thread, and its first call can happen inside
//! [`subscribe`] itself. So keep it short, and send the theme on to your UI thread.
//!
//! Each platform has one system-wide source for the theme, and one subscription
//! to it for the whole process, shared by every [`Subscription`]. The README
//! lists those sources and what each platform needs from your app.

use std::{
    fmt,
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, MutexGuard, PoisonError,
    },
};

#[cfg_attr(target_os = "android", path = "android.rs")]
#[cfg_attr(any(target_os = "ios", target_os = "macos"), path = "apple/mod.rs")]
#[cfg_attr(target_os = "linux", path = "linux/mod.rs")]
#[cfg_attr(target_os = "windows", path = "windows.rs")]
#[cfg_attr(
    not(any(
        target_os = "android",
        target_os = "ios",
        target_os = "linux",
        target_os = "macos",
        target_os = "windows",
    )),
    path = "unsupported.rs"
)]
mod sys;

/// The system's light or dark theme.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Theme {
    Light,
    Dark,
}

/// Why the system theme couldn't be read or subscribed to.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// This platform has no system theme that we know how to read.
    Unsupported,
    /// The system couldn't be asked right now. The message says why.
    Unavailable(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported => f.write_str("the system theme is not supported on this platform"),
            Self::Unavailable(reason) => write!(f, "the system theme is unavailable: {reason}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// Returns the current system theme, or `Ok(None)` if the system has no preference.
///
/// Any thread can call this. On Linux it can wait up to a second for the desktop to answer.
pub fn query() -> Result<Option<Theme>> {
    sys::query()
}

/// Calls `on_change` with the current system theme, and again each time it changes.
///
/// Updates stop once the returned [`Subscription`] is dropped.
pub fn subscribe(on_change: impl FnMut(Option<Theme>) + Send + 'static) -> Result<Subscription> {
    UPDATES.subscribe(Box::new(on_change), sys::start, sys::read_now)
}

/// Keeps a [`subscribe`] callback receiving updates until it's dropped.
///
/// A call that is already running on another thread may still finish after the drop.
#[must_use = "updates stop when the subscription is dropped"]
pub struct Subscription {
    updates: &'static Updates,
    subscriber: Arc<Subscriber>,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.subscriber.is_active.store(false, Ordering::Release);
        lock(&self.updates.0).subscribers.retain(|other| !Arc::ptr_eq(other, &self.subscriber));
    }
}

impl fmt::Debug for Subscription {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Subscription").finish_non_exhaustive()
    }
}

// A subscription must be usable from any thread on every platform.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Subscription>();
};

/// Gives every subscriber the theme that `read` returns, if it's a new one.
/// `read` runs while other reads are held off, so the newest theme always wins.
#[allow(dead_code)] // Not every platform has a backend that calls this.
pub(crate) fn refresh(read: impl FnOnce() -> Option<Theme>) {
    UPDATES.refresh(read);
}

/// The theme that subscribers were last given, if there is one yet.
#[allow(dead_code)] // Only some backends need this.
pub(crate) fn latest() -> Option<Option<Theme>> {
    lock(&UPDATES.0).latest
}

static UPDATES: Updates = Updates::new();

type OnChange = Box<dyn FnMut(Option<Theme>) + Send>;

/// The theme and its subscribers, shared by the whole process.
struct Updates(Mutex<State>);

struct State {
    /// The newest theme, once the system has told us one.
    latest: Option<Option<Theme>>,
    /// How many themes there have been, so that an older one never follows a newer one.
    version: u64,
    subscribers: Vec<Arc<Subscriber>>,
}

struct Subscriber {
    is_active: AtomicBool,
    delivery: Mutex<Delivery>,
}

struct Delivery {
    /// The version and theme that `on_change` was last called with.
    version: u64,
    theme: Option<Option<Theme>>,
    on_change: OnChange,
}

impl Updates {
    const fn new() -> Self {
        Self(Mutex::new(State { latest: None, version: 0, subscribers: Vec::new() }))
    }

    /// `start` begins the platform's updates, and `read_now` returns the theme
    /// if it can be read without waiting.
    fn subscribe(
        &'static self,
        on_change: OnChange,
        start: impl FnOnce() -> Result<()>,
        read_now: impl FnOnce() -> Option<Option<Theme>>,
    ) -> Result<Subscription> {
        let subscriber = Arc::new(Subscriber {
            is_active: AtomicBool::new(true),
            delivery: Mutex::new(Delivery { version: 0, theme: None, on_change }),
        });
        let (version, latest, recipients) = {
            let mut state = lock(&self.0);
            // Start before reading, so that a change between the two still reaches us.
            start()?;
            state.subscribers.push(subscriber.clone());
            if state.latest.is_some() {
                (state.version, state.latest, vec![subscriber.clone()])
            } else {
                // Earlier subscribers haven't been given a theme yet either.
                state.latest = read_now();
                state.version += u64::from(state.latest.is_some());
                (state.version, state.latest, state.subscribers.clone())
            }
        };
        if let Some(theme) = latest {
            for recipient in &recipients {
                recipient.deliver(version, theme);
            }
        }
        Ok(Subscription { updates: self, subscriber })
    }

    fn refresh(&self, read: impl FnOnce() -> Option<Theme>) {
        let (version, theme, subscribers) = {
            let mut state = lock(&self.0);
            let theme = read();
            if state.latest == Some(theme) {
                return;
            }
            state.latest = Some(theme);
            state.version += 1;
            (state.version, theme, state.subscribers.clone())
        };
        for subscriber in &subscribers {
            subscriber.deliver(version, theme);
        }
    }
}

impl Subscriber {
    fn deliver(&self, version: u64, theme: Option<Theme>) {
        let mut delivery = lock(&self.delivery);
        if delivery.version >= version || !self.is_active.load(Ordering::Acquire) {
            return;
        }
        delivery.version = version;
        if delivery.theme.replace(theme) == Some(theme) {
            return;
        }
        // A panic in the app's callback must not unwind into the system code that called us.
        let _ = catch_unwind(AssertUnwindSafe(|| (delivery.on_change)(theme)));
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{channel, Receiver};

    use Theme::{Dark, Light};

    fn updates() -> &'static Updates {
        Box::leak(Box::new(Updates::new()))
    }

    /// Subscribes with a callback that records every theme it's given.
    fn subscribe(updates: &'static Updates, now: Option<Option<Theme>>) -> (Subscription, Receiver<Option<Theme>>) {
        let (sender, themes) = channel();
        let on_change = Box::new(move |theme| sender.send(theme).unwrap());
        (updates.subscribe(on_change, || Ok(()), || now).unwrap(), themes)
    }

    fn received(themes: &Receiver<Option<Theme>>) -> Vec<Option<Theme>> {
        themes.try_iter().collect()
    }

    #[test]
    fn gives_the_current_theme_then_each_change_once() {
        let updates = updates();
        let (_subscription, themes) = subscribe(updates, Some(Some(Dark)));
        assert_eq!(received(&themes), [Some(Dark)]);
        updates.refresh(|| Some(Dark));
        assert_eq!(received(&themes), []);
        updates.refresh(|| Some(Light));
        updates.refresh(|| None);
        updates.refresh(|| None);
        assert_eq!(received(&themes), [Some(Light), None]);
    }

    #[test]
    fn a_later_subscriber_gets_the_latest_theme_without_reading_again() {
        let updates = updates();
        let (_first, _) = subscribe(updates, Some(Some(Light)));
        updates.refresh(|| Some(Dark));
        let (sender, themes) = channel();
        let on_change = Box::new(move |theme| sender.send(theme).unwrap());
        let _second = updates.subscribe(on_change, || Ok(()), || panic!("the theme is already known")).unwrap();
        assert_eq!(received(&themes), [Some(Dark)]);
    }

    #[test]
    fn a_theme_that_arrives_later_reaches_every_subscriber() {
        let updates = updates();
        let (_first, first) = subscribe(updates, None);
        let (_second, second) = subscribe(updates, None);
        assert_eq!(received(&first), []);
        updates.refresh(|| Some(Dark));
        assert_eq!(received(&first), [Some(Dark)]);
        assert_eq!(received(&second), [Some(Dark)]);
    }

    #[test]
    fn a_theme_read_by_one_subscriber_reaches_those_still_waiting() {
        let updates = updates();
        let (_first, first) = subscribe(updates, None);
        let (_second, second) = subscribe(updates, Some(None));
        assert_eq!(received(&first), [None]);
        assert_eq!(received(&second), [None]);
        // The platform then reports the same theme, which isn't a change.
        updates.refresh(|| None);
        assert_eq!(received(&first), []);
    }

    #[test]
    fn dropping_a_subscription_stops_only_its_updates() {
        let updates = updates();
        let (first, first_themes) = subscribe(updates, Some(Some(Light)));
        let (_second, second_themes) = subscribe(updates, Some(Some(Light)));
        drop(first);
        updates.refresh(|| Some(Dark));
        assert_eq!(received(&first_themes), [Some(Light)]);
        assert_eq!(received(&second_themes), [Some(Light), Some(Dark)]);
        assert_eq!(lock(&updates.0).subscribers.len(), 1);
    }

    #[test]
    fn a_failed_start_subscribes_nothing() {
        let updates = updates();
        let on_change = Box::new(|_| panic!("there is nothing to deliver"));
        let result = updates.subscribe(on_change, || Err(Error::Unsupported), || Some(Some(Dark)));
        assert_eq!(result.unwrap_err(), Error::Unsupported);
        assert!(lock(&updates.0).subscribers.is_empty());
        assert_eq!(lock(&updates.0).latest, None);
    }

    #[test]
    fn a_panicking_callback_stops_nothing() {
        let updates = updates();
        let on_change = Box::new(|theme| assert_ne!(theme, Some(Dark), "expected by this test"));
        let _panics_on_dark = updates.subscribe(on_change, || Ok(()), || Some(Some(Light))).unwrap();
        let (_other, themes) = subscribe(updates, None);
        updates.refresh(|| Some(Dark));
        updates.refresh(|| Some(Light));
        assert_eq!(received(&themes), [Some(Light), Some(Dark), Some(Light)]);
    }

    #[test]
    fn a_callback_can_drop_its_own_subscription() {
        let updates = updates();
        let slot = Arc::new(Mutex::new(None::<Subscription>));
        let (own, sender) = (slot.clone(), channel());
        let on_change = Box::new(move |theme| {
            sender.0.send(theme).unwrap();
            if theme == Some(Dark) {
                own.lock().unwrap().take();
            }
        });
        *slot.lock().unwrap() = Some(updates.subscribe(on_change, || Ok(()), || Some(Some(Light))).unwrap());
        updates.refresh(|| Some(Dark));
        updates.refresh(|| Some(Light));
        assert_eq!(received(&sender.1), [Some(Light), Some(Dark)]);
        assert!(lock(&updates.0).subscribers.is_empty());
    }

    #[test]
    fn an_older_theme_never_follows_a_newer_one() {
        let updates = updates();
        let (subscription, themes) = subscribe(updates, Some(Some(Light)));
        // Two threads can deliver out of order: version 3 arrives before version 2.
        subscription.subscriber.deliver(3, Some(Dark));
        subscription.subscriber.deliver(2, None);
        assert_eq!(received(&themes), [Some(Light), Some(Dark)]);
        // A skipped change can leave the newest theme equal to the last one given.
        subscription.subscriber.deliver(5, Some(Dark));
        assert_eq!(received(&themes), []);
    }
}
