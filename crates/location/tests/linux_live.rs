//! Opt-in end-to-end tests against a real location provider.
//!
//! Ignored by default: needs a working portal or GeoClue, an actual location source, and maybe a
//! click from you.

#![cfg(target_os = "linux")]

use std::{
    sync::{
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
        Arc, Mutex, Weak,
    },
    time::{Duration, SystemTime},
};

use robius_location::{Access, Accuracy, Coordinates, Error, Location, Manager};

#[derive(Debug)]
enum Event {
    Location(Snapshot),
    Error(Error),
}

#[derive(Debug)]
struct Snapshot {
    coordinates: robius_location::Result<Coordinates>,
    altitude: robius_location::Result<f64>,
    bearing: robius_location::Result<f64>,
    speed: robius_location::Result<f64>,
    time: robius_location::Result<SystemTime>,
}

struct Handler(Sender<Event>);

impl robius_location::Handler for Handler {
    fn handle(&self, location: Location<'_>) {
        let _ = self.0.send(Event::Location(Snapshot {
            coordinates: location.coordinates(),
            altitude: location.altitude(),
            bearing: location.bearing(),
            speed: location.speed(),
            time: location.time(),
        }));
    }

    fn error(&self, error: Error) {
        let _ = self.0.send(Event::Error(error));
    }
}

fn receive_location(receiver: &Receiver<Event>, timeout: Duration) -> Snapshot {
    match receiver.recv_timeout(timeout) {
        Ok(Event::Location(location)) => location,
        Ok(Event::Error(error)) => panic!("location provider returned {error:?}"),
        Err(RecvTimeoutError::Timeout) => panic!("timed out waiting for a location"),
        Err(RecvTimeoutError::Disconnected) => panic!("location handler disconnected"),
    }
}

fn validate_location(location: Snapshot) {
    let coordinates = location.coordinates.expect("coordinates must be present");
    assert!(coordinates.latitude.is_finite());
    assert!((-90.0..=90.0).contains(&coordinates.latitude));
    assert!(coordinates.longitude.is_finite());
    assert!((-180.0..=180.0).contains(&coordinates.longitude));

    if let Ok(altitude) = location.altitude {
        assert!(altitude.is_finite());
    }
    if let Ok(speed) = location.speed {
        assert!(speed.is_finite() && speed >= 0.0);
    }
    if let Ok(bearing) = location.bearing {
        assert!((0.0..360.0).contains(&bearing));
    }
    if let Ok(time) = location.time {
        assert!(time >= SystemTime::UNIX_EPOCH + Duration::from_secs(1_577_836_800));
        assert!(time <= SystemTime::now() + Duration::from_secs(5 * 60));
    }
}

#[test]
#[ignore = "requires a live Linux location service and provider"]
fn linux_one_shot_continuous_stop_restart_and_drop() {
    let (sender, receiver) = mpsc::channel();
    let mut manager = Manager::new(Handler(sender)).expect("create Linux location manager");

    manager
        .request_authorization(Access::Foreground, Accuracy::Precise)
        .expect("request provider authorization");
    manager.update_once().expect("start one-shot request");
    validate_location(receive_location(&receiver, Duration::from_secs(30)));

    // Starting twice must be idempotent and must not create competing provider state.
    manager.start_updates().expect("start continuous updates");
    manager.start_updates().expect("start updates idempotently");
    validate_location(receive_location(&receiver, Duration::from_secs(30)));

    manager.stop_updates().expect("stop continuous updates");
    while receiver.try_recv().is_ok() {}
    assert!(matches!(
        receiver.recv_timeout(Duration::from_secs(1)),
        Err(RecvTimeoutError::Timeout)
    ));

    // Restart a one-shot after stopping the prior provider session/client.
    manager.update_once().expect("restart a one-shot request");
    validate_location(receive_location(&receiver, Duration::from_secs(30)));

    // A session can be upgraded from a pending one-shot to continuous updates and stopped again.
    manager.start_updates().expect("restart continuous updates");
    validate_location(receive_location(&receiver, Duration::from_secs(30)));

    // Changing accuracy replaces an active session without losing the continuous-update intent.
    manager
        .request_authorization(Access::Background, Accuracy::Approximate)
        .expect("reconfigure active session accuracy");
    while receiver.try_recv().is_ok() {}
    validate_location(receive_location(&receiver, Duration::from_secs(30)));
    manager.stop_updates().expect("stop restarted updates");
    drop(manager);
}

struct ReentrantHandler {
    manager: Arc<Mutex<Option<Weak<Manager>>>>,
    result: Sender<robius_location::Result<()>>,
}

impl robius_location::Handler for ReentrantHandler {
    fn handle(&self, _location: Location<'_>) {
        let result = self
            .manager
            .lock()
            .expect("manager slot poisoned")
            .as_ref()
            .and_then(Weak::upgrade)
            .ok_or(Error::TemporarilyUnavailable)
            .and_then(|manager| {
                manager.request_authorization(Access::Foreground, Accuracy::Precise)
            });
        let _ = self.result.send(result);
    }

    fn error(&self, error: Error) {
        let _ = self.result.send(Err(error));
    }
}

#[test]
#[ignore = "requires a live Linux location service and provider"]
fn handler_can_reenter_manager_without_deadlock() {
    let manager_slot = Arc::new(Mutex::new(None));
    let (sender, receiver) = mpsc::channel();
    let manager = Arc::new(
        Manager::new(ReentrantHandler {
            manager: manager_slot.clone(),
            result: sender,
        })
        .expect("create Linux location manager"),
    );
    *manager_slot.lock().unwrap() = Some(Arc::downgrade(&manager));

    manager
        .request_authorization(Access::Foreground, Accuracy::Precise)
        .expect("request authorization");
    manager.update_once().expect("start one-shot request");
    receiver
        .recv_timeout(Duration::from_secs(30))
        .expect("reentrant handler call deadlocked")
        .expect("reentrant manager call failed");
}

#[test]
#[ignore = "requires a live Linux location service"]
fn construction_and_drop_work_inside_unrelated_tokio_runtimes() {
    let current_thread = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    current_thread.block_on(async {
        drop(Manager::new(Handler(mpsc::channel().0)).expect("current-thread runtime"));
    });

    let multi_thread = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .build()
        .unwrap();
    multi_thread.block_on(async {
        drop(Manager::new(Handler(mpsc::channel().0)).expect("multi-thread runtime"));
    });
}
