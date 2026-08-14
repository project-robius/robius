//! Direct GeoClue backend, used when the XDG Location portal isn't available.
//!
//! GeoClue is on the system bus and wants a desktop-file ID its authorization agent recognizes.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex, MutexGuard,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use zbus::{
    blocking::{connection::Builder as ConnectionBuilder, Connection, MessageIterator, Proxy},
    message::{Message, Type as MessageType},
    names::OwnedUniqueName,
    zvariant::{OwnedObjectPath, OwnedValue},
    MatchRule,
};

// Shared with the portal backend so the two paths cannot silently drift apart. Anything defined
// locally below is deliberately GeoClue-specific and is named accordingly.
use super::{
    is_recent, lock, one_shot_is_fresher, parse_location, validate_desktop_id, CallbackSender,
    LocationData, OneShot, ONE_SHOT_TIMEOUT,
};
use crate::{Access, Accuracy, Error, Result};

const GEOCLUE_DESTINATION: &str = "org.freedesktop.GeoClue2";
const GEOCLUE_NAMESPACE: &str = "/org/freedesktop/GeoClue2";
const MANAGER_PATH: &str = "/org/freedesktop/GeoClue2/Manager";
const MANAGER_INTERFACE: &str = "org.freedesktop.GeoClue2.Manager";
const CLIENT_INTERFACE: &str = "org.freedesktop.GeoClue2.Client";
const LOCATION_INTERFACE: &str = "org.freedesktop.GeoClue2.Location";
const PROPERTIES_INTERFACE: &str = "org.freedesktop.DBus.Properties";
const DBUS_DESTINATION: &str = "org.freedesktop.DBus";
const DBUS_PATH: &str = "/org/freedesktop/DBus";
const DBUS_INTERFACE: &str = "org.freedesktop.DBus";

/// Per-connection because that's all zbus offers, and a GeoClue client belongs to the connection
/// that made it. zbus has no default, so leaving this unset would block forever.
///
/// Sized for the slowest call, `Client.Start`: it waits up to 5s for an agent to show up, then
/// gives it GDBus's default 25s to answer. So GeoClue replies within ~30s even if the user never
/// touches the prompt, since it returns `AccessDenied` rather than hanging. We just need room
/// to let its timeout fire first, since a denial is a definitive error and a transport timeout
/// isn't (see `is_definitive_start_error`).
const GEOCLUE_METHOD_TIMEOUT: Duration = Duration::from_secs(45);
const SIGNAL_QUEUE_CAPACITY: usize = 64;

// GClueAccuracyLevel: COUNTRY 1, CITY 4, STREET 6, EXACT 8.
// NOTE: these are *not* the portal's accuracy numbers. Approximate asks for CITY, not STREET.
const GEOCLUE_ACCURACY_CITY: u32 = 4;
const GEOCLUE_ACCURACY_EXACT: u32 = 8;

/// A direct connection to GeoClue. `desktop_id` has no `.desktop` suffix and is pre-validated.
pub(super) struct Manager {
    connection: Connection,
    shared: Arc<Shared>,
    listeners: Vec<JoinHandle<()>>,
}

struct Shared {
    connection: Connection,
    callbacks: CallbackSender,
    desktop_id: String,
    operations: Mutex<()>,
    state: Mutex<State>,
    available: AtomicBool,
    dropped: AtomicBool,
}

struct State {
    accuracy: Accuracy,
    callback_generation: u64,
    client: Option<Client>,
    starting: bool,
    started: bool,
    authorization_pending: bool,
    continuous: bool,
    one_shot: Option<OneShot>,
    last_location: Option<LocationData>,
}

#[derive(Clone)]
struct Client {
    path: OwnedObjectPath,
    /// The unique bus owner that created `path`. This prevents delivery of a
    /// queued signal from an old GeoClue process after the daemon restarts.
    owner: OwnedUniqueName,
    configured_accuracy: u32,
}


impl Manager {
    pub(super) fn try_new(callbacks: CallbackSender, desktop_id: String) -> Result<Self> {
        let desktop_id = validate_desktop_id(&desktop_id)?;
        let connection = ConnectionBuilder::system()
            .map_err(map_zbus_error)?
            .method_timeout(GEOCLUE_METHOD_TIMEOUT)
            .build()
            .map_err(map_zbus_error)?;

        // Raw match rules rather than `OwnerChangedIterator`: gives us old and new owner in one message,
        // and stays safe if something else turns on zbus's Tokio feature.
        let owner_messages = MessageIterator::for_match_rule(
            MatchRule::builder()
                .msg_type(MessageType::Signal)
                .sender(DBUS_DESTINATION)
                .map_err(map_zbus_error)?
                .path(DBUS_PATH)
                .map_err(map_zbus_error)?
                .interface(DBUS_INTERFACE)
                .map_err(map_zbus_error)?
                .member("NameOwnerChanged")
                .map_err(map_zbus_error)?
                .add_arg(GEOCLUE_DESTINATION)
                .map_err(map_zbus_error)?
                .build(),
            &connection,
            Some(4),
        )
        .map_err(map_zbus_error)?;

        // One namespace rule covers whichever client GetClient hands us, even after a daemon restart.
        // Paths and unique senders get checked again below.
        let geoclue_messages = MessageIterator::for_match_rule(
            MatchRule::builder()
                .msg_type(MessageType::Signal)
                .sender(GEOCLUE_DESTINATION)
                .map_err(map_zbus_error)?
                .path_namespace(GEOCLUE_NAMESPACE)
                .map_err(map_zbus_error)?
                .build(),
            &connection,
            Some(SIGNAL_QUEUE_CAPACITY),
        )
        .map_err(map_zbus_error)?;

        let shared = Arc::new(Shared {
            connection: connection.clone(),
            callbacks,
            desktop_id,
            operations: Mutex::new(()),
            state: Mutex::new(State {
                accuracy: Accuracy::Approximate,
                callback_generation: 0,
                client: None,
                starting: false,
                started: false,
                authorization_pending: false,
                continuous: false,
                one_shot: None,
                last_location: None,
            }),
            available: AtomicBool::new(true),
            dropped: AtomicBool::new(false),
        });

        // Configure a client up front so a successful constructor really means GeoClue is usable.
        // Doesn't start collecting anything.
        ensure_client(&shared)?;

        let signal_shared = shared.clone();
        let signal_listener = thread::Builder::new()
            .name("robius-location-geoclue".into())
            .spawn(move || listen_geoclue(geoclue_messages, signal_shared))
            .map_err(|_| Error::PermanentlyUnavailable)?;

        let owner_shared = shared.clone();
        let owner_listener = match thread::Builder::new()
            .name("robius-location-geoclue-owner".into())
            .spawn(move || listen_owner(owner_messages, owner_shared))
        {
            Ok(listener) => listener,
            Err(_) => {
                shared.dropped.store(true, Ordering::Release);
                let _ = connection.clone().close();
                drop(signal_listener);
                return Err(Error::PermanentlyUnavailable);
            }
        };

        Ok(Self {
            connection,
            shared,
            listeners: vec![signal_listener, owner_listener],
        })
    }

    pub(super) fn request_authorization(&self, _access: Access, accuracy: Accuracy) -> Result<()> {
        let _operation = self.operation();
        ensure_available(&self.shared)?;

        let accuracy_changed = {
            let state = self.state();
            geoclue_accuracy(state.accuracy) != geoclue_accuracy(accuracy)
        };

        if accuracy_changed {
            // Invalidate old-accuracy callbacks before teardown can block. If a queued one-shot fix
            // is cancelled, preserve that intent for the newly configured client.
            {
                let mut state = self.state();
                state.accuracy = accuracy;
                state.last_location = None;
                state.callback_generation = state.callback_generation.wrapping_add(1);
                let discarded_one_shot = self
                    .shared
                    .callbacks
                    .advance_generation(state.callback_generation);
                if state.one_shot.is_some() || discarded_one_shot {
                    state.one_shot = Some(OneShot {
                        deadline: Instant::now() + ONE_SHOT_TIMEOUT,
                        delivered_cached: false,
                        newer_than: None,
                    });
                }
            }
            // Prefer reconfiguring the existing, already-authorized client; only replace it when
            // that is impossible (it is already collecting, or the property set failed).
            if !reconfigure_client_accuracy(&self.shared, geoclue_accuracy(accuracy)) {
                if let Err(error) = retire_client(&self.shared) {
                    clear_intents(&self.shared);
                    return Err(error);
                }
                if let Err(error) = ensure_client(&self.shared) {
                    clear_intents(&self.shared);
                    return Err(error);
                }
            }
        }

        if self.state().started {
            return Ok(());
        }

        self.state().authorization_pending = true;
        let start_result = start_client(&self.shared);
        self.state().authorization_pending = false;
        if let Err(error) = start_result {
            clear_intents(&self.shared);
            return Err(error);
        }

        let idle = {
            let state = self.state();
            !state.continuous && state.one_shot.is_none()
        };
        if idle {
            // Stop collecting, but keep the client that just passed authorization.
            stop_client(&self.shared)?;
        }
        Ok(())
    }

    pub(super) fn update_once(&self) -> Result<()> {
        let _operation = self.operation();
        ensure_available(&self.shared)?;

        let had_one_shot = {
            let mut state = self.state();
            let had_one_shot = state.one_shot.is_some();
            if !had_one_shot {
                state.one_shot = Some(OneShot {
                    deadline: Instant::now() + ONE_SHOT_TIMEOUT,
                    delivered_cached: false,
                    newer_than: None,
                });
            }
            had_one_shot
        };
        if let Err(error) = start_client(&self.shared) {
            if !had_one_shot {
                self.state().one_shot = None;
            }
            return Err(error);
        }

        let mut state = self.state();
        let cached = if state.one_shot.is_some() {
            state
                .last_location
                .as_ref()
                .filter(|location| is_recent(location.time))
                .cloned()
        } else {
            None
        };
        if let (Some(one_shot), Some(location)) = (state.one_shot.as_mut(), cached.as_ref()) {
            one_shot.newer_than = location.time;
            one_shot.delivered_cached = true;
        }
        let generation = state.callback_generation;
        drop(state);
        if let Some(location) = cached {
            self.shared.callbacks.location(location, true, generation);
        }
        Ok(())
    }

    pub(super) fn start_updates(&self) -> Result<()> {
        let _operation = self.operation();
        ensure_available(&self.shared)?;

        let was_continuous = {
            let mut state = self.state();
            let previous = state.continuous;
            state.continuous = true;
            previous
        };
        if let Err(error) = start_client(&self.shared) {
            if !was_continuous {
                self.state().continuous = false;
            }
            return Err(error);
        }
        Ok(())
    }

    pub(super) fn stop_updates(&self) -> Result<()> {
        let _operation = self.operation();
        ensure_available(&self.shared)?;

        let idle = {
            let mut state = self.state();
            state.continuous = false;
            state.one_shot.is_none() && !state.authorization_pending
        };
        self.shared.callbacks.discard_continuous_locations();
        if idle {
            stop_client(&self.shared)?;
        }
        Ok(())
    }

    pub(super) fn is_usable(&self) -> bool {
        ensure_available(&self.shared).is_ok()
    }

    pub(super) fn one_shot_deadline(&self) -> Option<Instant> {
        self.state()
            .one_shot
            .as_ref()
            .map(|one_shot| one_shot.deadline)
    }

    pub(super) fn expire_one_shot(&self) {
        let _operation = self.operation();
        if ensure_available(&self.shared).is_err() {
            return;
        }
        let (report, should_retire) = {
            let mut state = self.state();
            let Some(one_shot) = state.one_shot.as_ref() else {
                return;
            };
            if one_shot.deadline > Instant::now() {
                return;
            }
            let one_shot = state.one_shot.take().expect("one-shot was checked above");
            (
                !one_shot.delivered_cached,
                !state.continuous && !state.authorization_pending,
            )
        };

        if report {
            self.shared.callbacks.error(Error::TemporarilyUnavailable);
        }
        if should_retire {
            let _ = stop_client(&self.shared);
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        lock(&self.shared.state)
    }

    fn operation(&self) -> MutexGuard<'_, ()> {
        lock(&self.shared.operations)
    }
}

impl Drop for Manager {
    fn drop(&mut self) {
        self.shared.dropped.store(true, Ordering::Release);
        self.shared.available.store(false, Ordering::Release);

        {
            let mut state = lock(&self.shared.state);
            state.client = None;
            state.starting = false;
            state.started = false;
            state.authorization_pending = false;
            state.continuous = false;
            state.one_shot = None;
        }

        // GeoClue deletes a peer's clients when it disconnects, so just close. Faster than courtesy
        // Stop/Delete round trips, and fail-safe if the daemon is wedged during shutdown.
        let _ = self.connection.clone().close();

        // Listener threads may be inside bounded D-Bus work. Never make Drop wait indefinitely.
        for listener in self.listeners.drain(..) {
            if listener.is_finished() {
                let _ = listener.join();
            }
        }
    }
}

fn ensure_client(shared: &Shared) -> Result<Client> {
    if let Some(client) = lock(&shared.state).client.clone() {
        return Ok(client);
    }
    ensure_available(shared)?;

    let manager = manager_proxy(&shared.connection).map_err(map_zbus_error)?;
    let reply = manager
        .call_method("GetClient", &())
        .map_err(map_zbus_error)?;
    // Bind the path to this reply's sender; a separate GetNameOwner would race a daemon restart.
    let owner = reply
        .header()
        .sender()
        .cloned()
        .map(OwnedUniqueName::from)
        .ok_or(Error::Unknown)?;
    let path: OwnedObjectPath = reply.body().deserialize().map_err(map_zbus_error)?;
    if path.as_str() == "/" {
        return Err(Error::Unknown);
    }

    let accuracy = lock(&shared.state).accuracy;
    let client = Client {
        path,
        owner,
        configured_accuracy: geoclue_accuracy(accuracy),
    };
    lock(&shared.state).client = Some(client.clone());

    // Publish owner/path before configuring, so a racing NameOwnerChanged can invalidate this exact
    // client. Every call is addressed to the unique owner, never a replacement daemon.
    let configured = client_proxy(&shared.connection, &client)
        .map_err(map_zbus_error)
        .and_then(|proxy| {
            proxy
                .set_property("DesktopId", shared.desktop_id.as_str())
                .map_err(map_fdo_error)?;
            proxy
                .set_property("DistanceThreshold", 0_u32)
                .map_err(map_fdo_error)?;
            proxy
                .set_property("TimeThreshold", 0_u32)
                .map_err(map_fdo_error)?;
            proxy
                .set_property("RequestedAccuracyLevel", geoclue_accuracy(accuracy))
                .map_err(map_fdo_error)
        });
    if let Err(error) = configured {
        {
            let mut state = lock(&shared.state);
            if client_is_current(&state, &client) {
                state.client = None;
            }
        }
        fail_closed(shared);
        return Err(error);
    }
    if !client_is_current(&lock(&shared.state), &client) {
        fail_closed(shared);
        return Err(Error::TemporarilyUnavailable);
    }
    Ok(client)
}

/// Starts the configured client. The caller holds `operations`, except during
/// recovery from an update signal, where `stop_if_idle` acquires it itself.
fn start_client(shared: &Shared) -> Result<()> {
    ensure_available(shared)?;
    if lock(&shared.state).started {
        return Ok(());
    }

    let client = ensure_client(shared)?;
    {
        let mut state = lock(&shared.state);
        if state.started {
            return Ok(());
        }
        if client.configured_accuracy != geoclue_accuracy(state.accuracy) {
            drop(state);
            fail_closed(shared);
            return Err(Error::Unknown);
        }
        state.starting = true;
    }

    let result = client_proxy(&shared.connection, &client)
        .and_then(|proxy| proxy.call::<_, _, ()>("Start", &()));
    let ambiguous_failure = result
        .as_ref()
        .err()
        .is_some_and(|error| !is_definitive_start_error(error));

    let mut state = lock(&shared.state);
    state.starting = false;
    let client_is_current = client_is_current(&state, &client);
    match result {
        Ok(()) if client_is_current => {
            state.started = true;
            Ok(())
        }
        Ok(()) => {
            drop(state);
            fail_closed(shared);
            Err(Error::TemporarilyUnavailable)
        }
        Err(error) => {
            state.started = false;
            drop(state);
            let error = map_zbus_error(error);
            // An ambiguous failure can happen after GeoClue already started collecting. Closing our peer
            // makes it destroy the client, so a failed call can't leave location collection running.
            if ambiguous_failure {
                fail_closed(shared);
            }
            Err(error)
        }
    }
}

/// Drops the client locally before telling GeoClue, so queued signals from it get rejected even if
/// Stop is slow. Ambiguous cleanup failures close our peer, which fail-closes every client.
fn retire_client(shared: &Shared) -> Result<()> {
    let (client, was_running) = {
        let mut state = lock(&shared.state);
        let client = state.client.take();
        let was_running = state.started || state.starting;
        state.started = false;
        state.starting = false;
        state.last_location = None;
        (client, was_running)
    };
    let Some(client) = client else {
        return Ok(());
    };

    if was_running {
        let result = client_proxy(&shared.connection, &client)
            .and_then(|proxy| proxy.call::<_, _, ()>("Stop", &()));
        match result {
            Ok(()) => {}
            Err(error) if is_gone_error(&error) => return Ok(()),
            Err(error) => {
                let error = map_zbus_error(error);
                fail_closed(shared);
                return Err(error);
            }
        }
    }

    let result = manager_proxy_for_owner(&shared.connection, &client.owner)
        .and_then(|manager| manager.call::<_, _, ()>("DeleteClient", &(client.path,)));
    match result {
        Ok(()) => Ok(()),
        Err(error) if is_gone_error(&error) => Ok(()),
        Err(error) => {
            let error = map_zbus_error(error);
            fail_closed(shared);
            Err(error)
        }
    }
}

/// Retunes accuracy in place, returning whether it worked.
///
/// `RequestedAccuracyLevel` is writable while inactive, so we can avoid throwing away a client
/// that already has the user's authorization. Otherwise the very first `request_authorization`
/// costs a fresh `AuthorizeApp`, since the constructor's client uses the default accuracy.
/// A client that's already collecting needs replacing though; GeoClue latches the level at Start.
fn reconfigure_client_accuracy(shared: &Shared, accuracy: u32) -> bool {
    let client = {
        let state = lock(&shared.state);
        if state.started || state.starting {
            return false;
        }
        match state.client.clone() {
            Some(client) => client,
            None => return false,
        }
    };

    let applied = client_proxy(&shared.connection, &client)
        .map_err(map_zbus_error)
        .and_then(|proxy| {
            proxy
                .set_property("RequestedAccuracyLevel", accuracy)
                .map_err(map_fdo_error)
        });

    let mut state = lock(&shared.state);
    if !client_is_current(&state, &client) {
        return false;
    }
    if applied.is_ok() {
        if let Some(current) = state.client.as_mut() {
            current.configured_accuracy = accuracy;
        }
        true
    } else {
        // Drop the handle so the caller's fallback path builds a correctly configured client.
        state.client = None;
        state.last_location = None;
        false
    }
}

/// Stops collecting but keeps the client.
///
/// GeoClue re-runs `AuthorizeApp` whenever an inactive client is started, and agents like
/// `geoclue-demo-agent` prompt every time and cache nothing, so deleting the client on each idle
/// transition meant a permission prompt per request. Only Drop, fail_closed, and accuracy changes
/// actually need a new client.
fn stop_client(shared: &Shared) -> Result<()> {
    let (client, was_running) = {
        let mut state = lock(&shared.state);
        let was_running = state.started || state.starting;
        state.started = false;
        state.starting = false;
        (state.client.clone(), was_running)
    };
    let Some(client) = client else {
        return Ok(());
    };
    if !was_running {
        return Ok(());
    }

    let result = client_proxy(&shared.connection, &client)
        .and_then(|proxy| proxy.call::<_, _, ()>("Stop", &()));
    match result {
        Ok(()) => Ok(()),
        // The client vanished underneath us. Forget it (and the fix it produced, which belonged to
        // its authorization) so the next request transparently acquires a fresh one.
        Err(error) if is_gone_error(&error) => {
            let mut state = lock(&shared.state);
            if client_is_current(&state, &client) {
                state.client = None;
                state.last_location = None;
            }
            Ok(())
        }
        Err(error) => {
            let error = map_zbus_error(error);
            fail_closed(shared);
            Err(error)
        }
    }
}

/// Drains the match-rule queue and nothing else.
///
/// NOTE: keep this free of locks and D-Bus calls. `handle_message` wants `operations` (held by the
/// worker across calls) and does its own blocking property reads. Doing that here would stall us:
/// zbus drives the whole connection from one socket-reader task, so an undrained signal queue
/// parks the method replies the worker is blocked on.
fn listen_geoclue(mut messages: MessageIterator, shared: Arc<Shared>) {
    let (work, queue) = mpsc::channel();
    let handler_shared = shared.clone();
    let handler = thread::Builder::new()
        .name("robius-location-geoclue-signals".into())
        .spawn(move || {
            for message in queue {
                if handler_shared.dropped.load(Ordering::Acquire) {
                    return;
                }
                handle_message(&handler_shared, &message);
            }
        });
    let Ok(handler) = handler else {
        fail_closed(&shared);
        return;
    };

    for message in &mut messages {
        if shared.dropped.load(Ordering::Acquire) {
            drop(work);
            let _ = handler.join();
            return;
        }
        match message {
            Ok(message) => {
                if work.send(message).is_err() {
                    break;
                }
            }
            Err(_) => {
                drop(work);
                let _ = handler.join();
                fail_closed(&shared);
                return;
            }
        }
    }
    drop(work);
    let _ = handler.join();
    if !shared.dropped.load(Ordering::Acquire) {
        fail_closed(&shared);
    }
}

fn listen_owner(mut messages: MessageIterator, shared: Arc<Shared>) {
    for message in &mut messages {
        if shared.dropped.load(Ordering::Acquire) {
            return;
        }
        match message {
            Ok(message) => handle_owner_changed(&shared, &message),
            Err(_) => {
                fail_closed(&shared);
                return;
            }
        }
    }
    if !shared.dropped.load(Ordering::Acquire) {
        fail_closed(&shared);
    }
}

fn handle_message(shared: &Shared, message: &Message) {
    let header = message.header();
    let Some(path) = header.path() else {
        return;
    };
    let Some(sender) = header.sender() else {
        return;
    };
    let Some(interface) = header.interface().map(|value| value.as_str()) else {
        return;
    };
    let Some(member) = header.member().map(|value| value.as_str()) else {
        return;
    };

    let relevant_client = {
        let state = lock(&shared.state);
        state
            .client
            .as_ref()
            .filter(|client| {
                client.path.as_str() == path.as_str() && client.owner.as_str() == sender.as_str()
            })
            .cloned()
    };
    let Some(client) = relevant_client else {
        return;
    };

    match (interface, member) {
        (CLIENT_INTERFACE, "LocationUpdated") => {
            match message
                .body()
                .deserialize::<(OwnedObjectPath, OwnedObjectPath)>()
            {
                Ok((_old, new)) if new.as_str() != "/" => handle_location(shared, &client, new),
                Ok(_) => handle_location_error(shared, &client, Error::TemporarilyUnavailable),
                Err(_) => handle_location_error(shared, &client, Error::Unknown),
            }
        }
        (PROPERTIES_INTERFACE, "PropertiesChanged") => {
            handle_properties_changed(shared, &client, message)
        }
        _ => {}
    }
}

fn handle_location(shared: &Shared, client: &Client, path: OwnedObjectPath) {
    let _operation = lock(&shared.operations);
    if ensure_available(shared).is_err() {
        return;
    }
    let values = match location_properties(&shared.connection, &client.owner, path) {
        Ok(values) => values,
        Err(error) => {
            handle_location_error_locked(shared, client, error);
            return;
        }
    };
    let location = match parse_location(&values) {
        Ok(location) => location,
        Err(error) => {
            handle_location_error_locked(shared, client, error);
            return;
        }
    };

    let should_stop = {
        let mut state = lock(&shared.state);
        if !client_is_current(&state, client) {
            return;
        }

        state.last_location = Some(location.clone());
        let continuous = state.continuous;
        let one_shot = state.one_shot.take();
        let deliver_one_shot = one_shot
            .as_ref()
            .is_some_and(|one_shot| one_shot_is_fresher(one_shot, location.time));
        if continuous || deliver_one_shot {
            shared
                .callbacks
                .location(location, deliver_one_shot, state.callback_generation);
        }
        one_shot.is_some() && !continuous && !state.authorization_pending
    };
    if should_stop {
        retire_if_idle(shared);
    }
}

fn handle_location_error(shared: &Shared, client: &Client, error: Error) {
    let _operation = lock(&shared.operations);
    if ensure_available(shared).is_err() {
        return;
    }
    handle_location_error_locked(shared, client, error);
}

fn handle_location_error_locked(shared: &Shared, client: &Client, error: Error) {
    let (report, should_stop) = {
        let mut state = lock(&shared.state);
        if !client_is_current(&state, client) {
            return;
        }
        let continuous = state.continuous;
        let had_one_shot = state.one_shot.take().is_some();
        (
            continuous || had_one_shot,
            had_one_shot && !continuous && !state.authorization_pending,
        )
    };
    if report {
        shared.callbacks.error(error);
    }
    if should_stop {
        retire_if_idle(shared);
    }
}

fn handle_properties_changed(shared: &Shared, client: &Client, message: &Message) {
    let Ok((interface, changed, invalidated)) =
        message
            .body()
            .deserialize::<(String, HashMap<String, OwnedValue>, Vec<String>)>()
    else {
        return;
    };
    if interface != CLIENT_INTERFACE {
        return;
    }

    let active = changed
        .get("Active")
        .and_then(|value| bool::try_from(value).ok())
        .or_else(|| {
            invalidated
                .iter()
                .any(|name| name == "Active")
                .then(|| current_active(shared, client).ok())
                .flatten()
        });
    if active == Some(false) {
        handle_unexpected_inactive(shared, client);
    }
}

fn handle_unexpected_inactive(shared: &Shared, client: &Client) {
    let _operation = lock(&shared.operations);
    if ensure_available(shared).is_err() || !client_is_current(&lock(&shared.state), client) {
        return;
    }
    // A delayed Active=false from an earlier transition must not cancel work that has since
    // restarted. Re-read the authoritative property after all public operations are serialized.
    if !matches!(current_active(shared, client), Ok(false)) {
        return;
    }
    let should_report = {
        let mut state = lock(&shared.state);
        if !client_is_current(&state, client) {
            return;
        }
        // False is expected after our own Stop. While Start is pending, its
        // method result is the authoritative authorization result.
        if !state.started || state.authorization_pending {
            return;
        }
        let active = state.continuous || state.one_shot.is_some();
        state.callback_generation = state.callback_generation.wrapping_add(1);
        let discarded_one_shot = shared
            .callbacks
            .advance_generation(state.callback_generation);
        state.started = false;
        state.continuous = false;
        state.one_shot = None;
        state.last_location = None;
        active || discarded_one_shot
    };
    if should_report {
        shared.callbacks.error(Error::AuthorizationDenied);
    }
}

fn handle_owner_changed(shared: &Shared, message: &Message) {
    // Only the bus driver may report ownership changes; match rules don't constrain the sender of a
    // directed signal, so without this any peer could forge a GeoClue disappearance and kill us.
    if message.header().sender().map(|sender| sender.as_str()) != Some(DBUS_DESTINATION) {
        return;
    }
    let Ok((name, old_owner, new_owner)) = message.body().deserialize::<(String, String, String)>()
    else {
        return;
    };
    if name != GEOCLUE_DESTINATION || old_owner.is_empty() {
        // Ignore the ordinary activation signal queued by GetClient.
        return;
    }

    let owner_is_current = lock(&shared.state)
        .client
        .as_ref()
        .is_some_and(|client| client.owner.as_str() == old_owner);
    if owner_is_current && old_owner != new_owner {
        // The old unique owner can remain alive briefly after relinquishing the well-known name.
        // Closing our dedicated peer guarantees that a racing Start cannot leave it collecting.
        fail_closed(shared);
    }
}

fn provider_lost(shared: &Shared) {
    let should_report = {
        let mut state = lock(&shared.state);
        let active = state.authorization_pending || state.continuous || state.one_shot.is_some();
        state.callback_generation = state.callback_generation.wrapping_add(1);
        let discarded_one_shot = shared
            .callbacks
            .advance_generation(state.callback_generation);
        state.client = None;
        state.starting = false;
        state.started = false;
        state.authorization_pending = false;
        state.continuous = false;
        state.one_shot = None;
        // Provider loss is an authorization/client boundary. Never replay its cache.
        state.last_location = None;
        active || discarded_one_shot
    };
    if should_report {
        shared.callbacks.error(Error::TemporarilyUnavailable);
    }
}

fn clear_intents(shared: &Shared) {
    let mut state = lock(&shared.state);
    state.authorization_pending = false;
    state.continuous = false;
    state.one_shot = None;
}

/// Retires an idle client while the caller holds `operations`.
fn retire_if_idle(shared: &Shared) {
    if ensure_available(shared).is_err() {
        return;
    }
    let idle = {
        let state = lock(&shared.state);
        !state.continuous && state.one_shot.is_none() && !state.authorization_pending
    };
    if idle {
        let _ = stop_client(shared);
    }
}

fn fail_closed(shared: &Shared) {
    if shared.available.swap(false, Ordering::AcqRel) {
        provider_lost(shared);
    }
    let _ = shared.connection.clone().close();
}

fn current_active(shared: &Shared, client: &Client) -> Result<bool> {
    client_proxy(&shared.connection, client)
        .map_err(map_zbus_error)?
        .get_property("Active")
        .map_err(map_zbus_error)
}

fn location_properties(
    connection: &Connection,
    owner: &OwnedUniqueName,
    path: OwnedObjectPath,
) -> Result<HashMap<String, OwnedValue>> {
    Proxy::new_owned(
        connection.clone(),
        owner.clone(),
        path,
        PROPERTIES_INTERFACE,
    )
    .map_err(map_zbus_error)?
    .call("GetAll", &(LOCATION_INTERFACE,))
    .map_err(map_zbus_error)
}

fn manager_proxy(connection: &Connection) -> zbus::Result<Proxy<'static>> {
    Proxy::new(
        connection,
        GEOCLUE_DESTINATION,
        MANAGER_PATH,
        MANAGER_INTERFACE,
    )
}

fn manager_proxy_for_owner(
    connection: &Connection,
    owner: &OwnedUniqueName,
) -> zbus::Result<Proxy<'static>> {
    Proxy::new_owned(
        connection.clone(),
        owner.clone(),
        MANAGER_PATH,
        MANAGER_INTERFACE,
    )
}

fn client_proxy(connection: &Connection, client: &Client) -> zbus::Result<Proxy<'static>> {
    Proxy::new_owned(
        connection.clone(),
        client.owner.clone(),
        client.path.clone(),
        CLIENT_INTERFACE,
    )
}

fn geoclue_accuracy(accuracy: Accuracy) -> u32 {
    match accuracy {
        Accuracy::Approximate => GEOCLUE_ACCURACY_CITY,
        Accuracy::Precise => GEOCLUE_ACCURACY_EXACT,
    }
}



fn client_is_current(state: &State, client: &Client) -> bool {
    state
        .client
        .as_ref()
        .is_some_and(|current| current.path == client.path && current.owner == client.owner)
}

fn ensure_available(shared: &Shared) -> Result<()> {
    if shared.dropped.load(Ordering::Acquire) || !shared.available.load(Ordering::Acquire) {
        Err(Error::TemporarilyUnavailable)
    } else {
        Ok(())
    }
}


fn map_fdo_error(error: zbus::fdo::Error) -> Error {
    map_zbus_error(zbus::Error::FDO(Box::new(error)))
}

fn map_zbus_error(error: zbus::Error) -> Error {
    match error {
        zbus::Error::InterfaceNotFound
        | zbus::Error::Address(_)
        | zbus::Error::Handshake(_)
        | zbus::Error::Unsupported => Error::PermanentlyUnavailable,
        zbus::Error::InputOutput(_) => Error::TemporarilyUnavailable,
        zbus::Error::MethodError(name, _, _) => map_method_error_name(name.as_str()),
        zbus::Error::FDO(error) => match *error {
            zbus::fdo::Error::AccessDenied(_)
            | zbus::fdo::Error::AuthFailed(_)
            | zbus::fdo::Error::InteractiveAuthorizationRequired(_) => Error::AuthorizationDenied,
            zbus::fdo::Error::ServiceUnknown(_)
            | zbus::fdo::Error::NameHasNoOwner(_)
            | zbus::fdo::Error::NoServer(_)
            | zbus::fdo::Error::UnknownInterface(_)
            | zbus::fdo::Error::UnknownMethod(_)
            | zbus::fdo::Error::UnknownObject(_)
            | zbus::fdo::Error::UnknownProperty(_) => Error::PermanentlyUnavailable,
            zbus::fdo::Error::NoNetwork(_) => Error::Network,
            zbus::fdo::Error::NoReply(_)
            | zbus::fdo::Error::Timeout(_)
            | zbus::fdo::Error::TimedOut(_)
            | zbus::fdo::Error::Disconnected(_)
            | zbus::fdo::Error::IOError(_) => Error::TemporarilyUnavailable,
            _ => Error::Unknown,
        },
        _ => Error::Unknown,
    }
}

fn map_method_error_name(name: &str) -> Error {
    match name {
        "org.freedesktop.DBus.Error.AccessDenied"
        | "org.freedesktop.DBus.Error.AuthFailed"
        | "org.freedesktop.GeoClue2.Error.AccessDenied"
        | "org.freedesktop.GeoClue2.Error.NotAuthorized"
        | "org.freedesktop.GeoClue2.Error.PermissionDenied" => Error::AuthorizationDenied,
        "org.freedesktop.DBus.Error.ServiceUnknown"
        | "org.freedesktop.DBus.Error.NameHasNoOwner"
        | "org.freedesktop.DBus.Error.NoServer"
        | "org.freedesktop.DBus.Error.UnknownInterface"
        | "org.freedesktop.DBus.Error.UnknownMethod"
        // Keep in sync with the FDO arm above and `map_zbus_error`: same wire error, same mapping.
        | "org.freedesktop.DBus.Error.UnknownObject"
        | "org.freedesktop.DBus.Error.UnknownProperty"
        | "org.freedesktop.DBus.Error.Spawn.ServiceNotFound"
        | "org.freedesktop.DBus.Error.Spawn.ExecFailed"
        | "org.freedesktop.DBus.Error.Spawn.ChildExited"
        | "org.freedesktop.DBus.Error.Spawn.ChildSignaled"
        | "org.freedesktop.DBus.Error.Spawn.Failed" => Error::PermanentlyUnavailable,
        "org.freedesktop.DBus.Error.NoNetwork" => Error::Network,
        "org.freedesktop.DBus.Error.NoReply"
        | "org.freedesktop.DBus.Error.Timeout"
        | "org.freedesktop.DBus.Error.TimedOut"
        | "org.freedesktop.DBus.Error.Disconnected"
        | "org.freedesktop.DBus.Error.IOError"
        | "org.freedesktop.GeoClue2.Error.NotAvailable"
        | "org.freedesktop.GeoClue2.Error.NoLocation" => Error::TemporarilyUnavailable,
        _ => Error::Unknown,
    }
}

/// Whether an error means the object we addressed is already gone, so tearing our own state down
/// is the correct and complete response.
fn is_gone_error(error: &zbus::Error) -> bool {
    let gone = |name: &str| {
        matches!(
            name,
            "org.freedesktop.DBus.Error.UnknownObject"
                | "org.freedesktop.DBus.Error.NameHasNoOwner"
                | "org.freedesktop.DBus.Error.ServiceUnknown"
                // DeleteClient arrived in GeoClue 2.5.2; older daemons say `UnknownMethod`, which means we
                // can't delete it, not that we're broken. Treat as already gone.
                | "org.freedesktop.DBus.Error.UnknownMethod"
                | "org.freedesktop.DBus.Error.UnknownInterface"
        )
    };
    match error {
        zbus::Error::MethodError(name, _, _) => gone(name.as_str()),
        zbus::Error::FDO(error) => matches!(
            error.as_ref(),
            zbus::fdo::Error::UnknownObject(_)
                | zbus::fdo::Error::NameHasNoOwner(_)
                | zbus::fdo::Error::ServiceUnknown(_)
                | zbus::fdo::Error::UnknownMethod(_)
                | zbus::fdo::Error::UnknownInterface(_)
        ),
        _ => false,
    }
}

/// Did `Start` definitively fail? Transport/no-reply failures are ambiguous and fail us closed.
fn is_definitive_start_error(error: &zbus::Error) -> bool {
    match error {
        zbus::Error::MethodError(name, _, _) => !matches!(
            name.as_str(),
            "org.freedesktop.DBus.Error.NoReply"
                | "org.freedesktop.DBus.Error.Timeout"
                | "org.freedesktop.DBus.Error.TimedOut"
                | "org.freedesktop.DBus.Error.Disconnected"
                | "org.freedesktop.DBus.Error.IOError"
        ),
        zbus::Error::FDO(error) => !matches!(
            error.as_ref(),
            zbus::fdo::Error::NoReply(_)
                | zbus::fdo::Error::Timeout(_)
                | zbus::fdo::Error::TimedOut(_)
                | zbus::fdo::Error::Disconnected(_)
                | zbus::fdo::Error::IOError(_)
        ),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use super::{super::MAX_CACHED_AGE, *};

    #[test]
    fn accuracy_maps_to_geoclue_levels() {
        assert_eq!(geoclue_accuracy(Accuracy::Approximate), GEOCLUE_ACCURACY_CITY);
        assert_eq!(geoclue_accuracy(Accuracy::Precise), GEOCLUE_ACCURACY_EXACT);
    }

    #[test]
    fn cache_rejects_future_and_stale_fixes() {
        let now = SystemTime::now();
        assert!(is_recent(Some(now - Duration::from_secs(30))));
        assert!(!is_recent(Some(
            now - MAX_CACHED_AGE - Duration::from_secs(1)
        )));
        assert!(!is_recent(Some(now + Duration::from_secs(1))));
        assert!(!is_recent(None));
    }

    #[test]
    fn cached_one_shot_only_delivers_a_newer_provider_fix() {
        let old = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        let one_shot = OneShot {
            deadline: Instant::now() + ONE_SHOT_TIMEOUT,
            delivered_cached: true,
            newer_than: Some(old),
        };
        assert!(!one_shot_is_fresher(&one_shot, Some(old)));
        assert!(!one_shot_is_fresher(
            &one_shot,
            Some(old - Duration::from_secs(1))
        ));
        assert!(one_shot_is_fresher(
            &one_shot,
            Some(old + Duration::from_secs(1))
        ));
        assert!(one_shot_is_fresher(&one_shot, None));
    }

    #[test]
    fn geoclue_and_transport_errors_are_classified() {
        assert!(matches!(
            map_method_error_name("org.freedesktop.GeoClue2.Error.NotAuthorized"),
            Error::AuthorizationDenied
        ));
        assert!(matches!(
            map_method_error_name("org.freedesktop.DBus.Error.NoNetwork"),
            Error::Network
        ));
        assert!(matches!(
            map_method_error_name("org.freedesktop.GeoClue2.Error.NotAvailable"),
            Error::TemporarilyUnavailable
        ));
        assert!(matches!(
            map_method_error_name("org.freedesktop.DBus.Error.ServiceUnknown"),
            Error::PermanentlyUnavailable
        ));
        assert!(matches!(
            map_method_error_name("org.example.Unrecognized"),
            Error::Unknown
        ));
    }
}
