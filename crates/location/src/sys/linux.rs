//! Linux location via the XDG Desktop Portal, falling back to GeoClue directly.
//!
//! The portal is preferred: it works in sandboxes too and gives us the desktop's permission UI.
//! We only fall back to GeoClue if the portal is missing at startup, never after a portal denial.

mod geoclue;

use std::{
    collections::{HashMap, VecDeque},
    env,
    fmt::Write as _,
    fs::File,
    io::Read,
    panic::{catch_unwind, AssertUnwindSafe},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
        Arc, Condvar, Mutex, MutexGuard, PoisonError,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime},
};

use zbus::{
    blocking::{connection::Builder as ConnectionBuilder, Connection, MessageIterator, Proxy},
    message::{Message, Type as MessageType},
    names::OwnedUniqueName,
    zvariant::{OwnedObjectPath, OwnedValue, Value},
    MatchRule,
};

use crate::{
    cached_fix_is_recent, Access, Accuracy, Coordinates, Error, Freshness, Handler, Result,
};

const PORTAL_DESTINATION: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const LOCATION_INTERFACE: &str = "org.freedesktop.portal.Location";
const REQUEST_INTERFACE: &str = "org.freedesktop.portal.Request";
const SESSION_INTERFACE: &str = "org.freedesktop.portal.Session";
const DBUS_DESTINATION: &str = "org.freedesktop.DBus";
const DBUS_PATH: &str = "/org/freedesktop/DBus";
const DBUS_INTERFACE: &str = "org.freedesktop.DBus";
const METHOD_TIMEOUT: Duration = Duration::from_secs(30);
const CONSTRUCTOR_TIMEOUT: Duration = Duration::from_secs(45);
const ONE_SHOT_TIMEOUT: Duration = Duration::from_secs(60);
const CALLBACK_QUEUE_CAPACITY: usize = 64;

// Values from the version-1 XDG Location portal specification.
const ACCURACY_CITY: u32 = 2;
const ACCURACY_EXACT: u32 = 5;

/// All blocking zbus work happens on this thread, so we don't care what async runtime (if any)
/// the app uses.
pub(crate) struct Manager {
    commands: SyncSender<Command>,
    shutdown: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

enum Command {
    RequestAuthorization(Access, Accuracy, SyncSender<Result<()>>),
    UpdateOnce(SyncSender<Result<()>>),
    StartUpdates(SyncSender<Result<()>>),
    StopUpdates(SyncSender<Result<()>>),
    Shutdown(SyncSender<()>),
}

enum Backend {
    Portal(PortalManager),
    GeoClue(geoclue::Manager),
}

enum DesktopId {
    Automatic,
    Explicit(String),
}

#[derive(Copy, Clone, Eq, PartialEq)]
enum BackendKind {
    Portal,
    GeoClue,
}

impl Backend {
    /// Builds a backend, optionally pinned to whatever the first construction picked.
    ///
    /// NOTE: pinning is a privacy thing, not tidiness. A portal denial doesn't fail us closed, so if
    /// the portal later dies an unpinned rebuild would see it missing and quietly switch to GeoClue,
    /// which is a different consent path that never saw the user's "no".
    fn new(
        callbacks: CallbackSender,
        desktop_id: &DesktopId,
        pinned: Option<BackendKind>,
    ) -> Result<Self> {
        let resolve_desktop_id = || match desktop_id {
            DesktopId::Automatic => Ok(automatic_desktop_id()),
            DesktopId::Explicit(desktop_id) => validate_desktop_id(desktop_id),
        };
        match pinned {
            Some(BackendKind::Portal) => PortalManager::new(callbacks)
                .map(Self::Portal)
                .map_err(|portal_error| portal_error.error),
            Some(BackendKind::GeoClue) => {
                if !direct_fallback_allowed() {
                    return Err(Error::PermanentlyUnavailable);
                }
                geoclue::Manager::try_new(callbacks, resolve_desktop_id()?).map(Self::GeoClue)
            }
            None => match PortalManager::new(callbacks.clone()) {
                Ok(manager) => Ok(Self::Portal(manager)),
                Err(portal_error) if portal_error.fallback_allowed && direct_fallback_allowed() => {
                    geoclue::Manager::try_new(callbacks, resolve_desktop_id()?).map(Self::GeoClue)
                }
                Err(portal_error) => Err(portal_error.error),
            },
        }
    }

    fn kind(&self) -> BackendKind {
        match self {
            Self::Portal(_) => BackendKind::Portal,
            Self::GeoClue(_) => BackendKind::GeoClue,
        }
    }

    /// A failed-closed backend can never serve another request, so the worker replaces it.
    fn is_usable(&self) -> bool {
        match self {
            Self::Portal(manager) => manager.is_usable(),
            Self::GeoClue(manager) => manager.is_usable(),
        }
    }

    fn request_authorization(&self, access: Access, accuracy: Accuracy) -> Result<()> {
        match self {
            Self::Portal(manager) => manager.request_authorization(access, accuracy),
            Self::GeoClue(manager) => manager.request_authorization(access, accuracy),
        }
    }

    fn update_once(&self) -> Result<()> {
        match self {
            Self::Portal(manager) => manager.update_once(),
            Self::GeoClue(manager) => manager.update_once(),
        }
    }

    fn start_updates(&self) -> Result<()> {
        match self {
            Self::Portal(manager) => manager.start_updates(),
            Self::GeoClue(manager) => manager.start_updates(),
        }
    }

    fn stop_updates(&self) -> Result<()> {
        match self {
            Self::Portal(manager) => manager.stop_updates(),
            Self::GeoClue(manager) => manager.stop_updates(),
        }
    }

    fn one_shot_deadline(&self) -> Option<Instant> {
        match self {
            Self::Portal(manager) => manager.one_shot_deadline(),
            Self::GeoClue(manager) => manager.one_shot_deadline(),
        }
    }

    fn expire_one_shot(&self) {
        match self {
            Self::Portal(manager) => manager.expire_one_shot(),
            Self::GeoClue(manager) => manager.expire_one_shot(),
        }
    }
}

impl Manager {
    pub fn new<T>(handler: T) -> Result<Self>
    where
        T: Handler,
    {
        Self::start_worker(Arc::new(handler), DesktopId::Automatic)
    }

    pub fn new_with_desktop_id<T>(handler: T, desktop_id: &str) -> Result<Self>
    where
        T: Handler,
    {
        // Validate eagerly so this constructor has deterministic input semantics: an invalid ID
        // is rejected even on a machine where the portal happens to make the value unnecessary.
        let desktop_id = validate_desktop_id(desktop_id)?;
        Self::start_worker(Arc::new(handler), DesktopId::Explicit(desktop_id))
    }

    fn start_worker(handler: Arc<dyn Handler>, desktop_id: DesktopId) -> Result<Self> {
        let (commands, receiver) = mpsc::sync_channel(8);
        let (ready_sender, ready_receiver) = mpsc::sync_channel(0);
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = cancelled.clone();
        let worker = thread::Builder::new()
            .name("robius-location-linux".into())
            .spawn(move || {
                run_backend(
                    receiver,
                    ready_sender,
                    handler,
                    desktop_id,
                    worker_cancelled,
                )
            })
            .map_err(|_| Error::PermanentlyUnavailable)?;

        match ready_receiver.recv_timeout(CONSTRUCTOR_TIMEOUT) {
            Ok(Ok(())) => Ok(Self {
                commands,
                shutdown: cancelled,
                worker: Some(worker),
            }),
            Ok(Err(error)) => {
                let _ = worker.join();
                Err(error)
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                if worker.is_finished() {
                    let _ = worker.join();
                }
                Err(Error::PermanentlyUnavailable)
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                cancelled.store(true, Ordering::Release);
                drop(commands);
                drop(worker);
                Err(Error::TemporarilyUnavailable)
            }
        }
    }

    pub fn request_authorization(&self, access: Access, accuracy: Accuracy) -> Result<()> {
        self.call(|reply| Command::RequestAuthorization(access, accuracy, reply))
    }

    pub fn update_once(&self) -> Result<()> {
        self.call(Command::UpdateOnce)
    }

    pub fn start_updates(&self) -> Result<()> {
        self.call(Command::StartUpdates)
    }

    pub fn stop_updates(&self) -> Result<()> {
        self.call(Command::StopUpdates)
    }

    fn call(&self, command: impl FnOnce(SyncSender<Result<()>>) -> Command) -> Result<()> {
        let (reply, result) = mpsc::sync_channel(0);
        self.commands
            .send(command(reply))
            .map_err(|_| Error::TemporarilyUnavailable)?;
        result.recv().unwrap_or(Err(Error::TemporarilyUnavailable))
    }
}

impl Drop for Manager {
    fn drop(&mut self) {
        // If the bounded command queue is full, the worker observes this after its current D-Bus
        // call and skips all queued work. This bounds post-drop collection to one method timeout.
        self.shutdown.store(true, Ordering::Release);
        let (finished, completion) = mpsc::sync_channel(0);
        if self.commands.try_send(Command::Shutdown(finished)).is_ok()
            && completion.recv_timeout(Duration::from_secs(1)).is_ok()
        {
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
        // Dropping an unfinished JoinHandle detaches it. This is intentional: shutdown must never
        // deadlock behind application handler code that is already executing on a signal thread.
    }
}

fn run_backend(
    receiver: Receiver<Command>,
    ready: SyncSender<Result<()>>,
    handler: Arc<dyn Handler>,
    desktop_id: DesktopId,
    cancelled: Arc<AtomicBool>,
) {
    let callbacks = match CallbackDispatcher::new(handler) {
        Ok(callbacks) => callbacks,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let mut backend = match Backend::new(callbacks.sender(), &desktop_id, None) {
        Ok(backend) => backend,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    // Whatever was selected now is what every later rebuild must use.
    let pinned = Some(backend.kind());
    if cancelled.load(Ordering::Acquire) {
        return;
    }
    if ready.send(Ok(())).is_err() {
        return;
    }

    loop {
        let command = if let Some(deadline) = backend.one_shot_deadline() {
            if deadline <= Instant::now() {
                backend.expire_one_shot();
                continue;
            }
            match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(command) => command,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    backend.expire_one_shot();
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        } else {
            match receiver.recv() {
                Ok(command) => command,
                Err(_) => return,
            }
        };
        if cancelled.load(Ordering::Acquire) {
            if let Command::Shutdown(finished) = command {
                drop(backend);
                drop(callbacks);
                let _ = finished.send(());
            }
            return;
        }
        // Rebuild a failed-closed backend (provider crash, upgrade, restart) so `TemporarilyUnavailable`
        // actually means temporary. Starts from scratch: no cached fix, session, or authorization.
        if !matches!(command, Command::Shutdown(_)) && !backend.is_usable() {
            // Kill off the dead backend's senders before the replacement restarts generations at zero.
            callbacks.retire_senders();
            match Backend::new(callbacks.sender(), &desktop_id, pinned) {
                Ok(replacement) => backend = replacement,
                Err(error) => {
                    match command {
                        Command::RequestAuthorization(_, _, reply)
                        | Command::UpdateOnce(reply)
                        | Command::StartUpdates(reply)
                        | Command::StopUpdates(reply) => {
                            let _ = reply.send(Err(error));
                        }
                        Command::Shutdown(_) => unreachable!("filtered out above"),
                    }
                    continue;
                }
            }
        }
        match command {
            Command::RequestAuthorization(access, accuracy, reply) => {
                let _ = reply.send(backend.request_authorization(access, accuracy));
            }
            Command::UpdateOnce(reply) => {
                let _ = reply.send(backend.update_once());
            }
            Command::StartUpdates(reply) => {
                let _ = reply.send(backend.start_updates());
            }
            Command::StopUpdates(reply) => {
                let _ = reply.send(backend.stop_updates());
            }
            Command::Shutdown(finished) => {
                drop(backend);
                drop(callbacks);
                let _ = finished.send(());
                return;
            }
        }
    }
}

/// Owns the callback thread. Everyone else just enqueues, which keeps callbacks serialized, keeps
/// slow app code off the D-Bus signal threads, and lets a callback re-enter `Manager`.
struct CallbackDispatcher {
    shared: Arc<CallbackShared>,
    worker: Option<JoinHandle<()>>,
}

#[derive(Clone)]
pub(super) struct CallbackSender {
    shared: Arc<CallbackShared>,
    /// Epoch this sender was made in. A replaced backend's listener threads are detached and may
    /// still be running, so their senders have to go inert.
    epoch: u64,
}

struct CallbackShared {
    queue: Mutex<CallbackQueue>,
    wake: Condvar,
}

struct CallbackQueue {
    events: VecDeque<CallbackEvent>,
    in_flight: Option<InFlightCallback>,
    accepted_generation: u64,
    /// Bumped whenever a backend is retired, invalidating every sender it handed to its threads.
    epoch: u64,
    shutdown: bool,
}

impl CallbackQueue {
    fn shut_down(&mut self) {
        self.shutdown = true;
        self.events.clear();
        if let Some(in_flight) = self.in_flight.as_mut() {
            if in_flight.phase == CallbackPhase::Pending {
                in_flight.cancelled = true;
            }
        }
    }
}

#[derive(Copy, Clone, Eq, PartialEq)]
enum CallbackPhase {
    Pending,
    Invoking,
}

struct InFlightCallback {
    location: bool,
    one_shot: bool,
    generation: u64,
    phase: CallbackPhase,
    cancelled: bool,
}

enum CallbackEvent {
    Location {
        location: LocationData,
        /// Also completes an outstanding `update_once`, so `stop_updates` knows not to drop it.
        one_shot: bool,
        generation: u64,
    },
    Error(Error),
}

impl CallbackDispatcher {
    fn new(handler: Arc<dyn Handler>) -> Result<Self> {
        let shared = Arc::new(CallbackShared {
            queue: Mutex::new(CallbackQueue {
                events: VecDeque::with_capacity(CALLBACK_QUEUE_CAPACITY),
                in_flight: None,
                accepted_generation: 0,
                epoch: 0,
                shutdown: false,
            }),
            wake: Condvar::new(),
        });
        let worker_shared = shared.clone();
        let worker = thread::Builder::new()
            .name("robius-location-callbacks".into())
            .spawn(move || dispatch_callbacks(worker_shared, handler))
            .map_err(|_| Error::PermanentlyUnavailable)?;
        Ok(Self {
            shared,
            worker: Some(worker),
        })
    }

    fn sender(&self) -> CallbackSender {
        CallbackSender {
            shared: self.shared.clone(),
            epoch: lock(&self.shared.queue).epoch,
        }
    }

    /// Makes every sender handed out so far inert.
    ///
    /// Otherwise a dead backend's listener could call `advance_generation` after the new one restarts
    /// at zero, pinning `accepted_generation` to a dead chain and silently dropping every later fix.
    /// Queued locations go too (wrong authorization), but errors stay, since the app needs those.
    fn retire_senders(&self) {
        let mut queue = lock(&self.shared.queue);
        queue.epoch = queue.epoch.wrapping_add(1);
        queue.accepted_generation = 0;
        queue
            .events
            .retain(|event| !matches!(event, CallbackEvent::Location { .. }));
        if let Some(in_flight) = queue.in_flight.as_mut() {
            if in_flight.location && in_flight.phase == CallbackPhase::Pending {
                in_flight.cancelled = true;
            }
        }
    }
}

impl Drop for CallbackDispatcher {
    fn drop(&mut self) {
        {
            let mut queue = lock(&self.shared.queue);
            queue.shut_down();
        }
        self.shared.wake.notify_all();

        // Application callback code may block forever. Detach rather than turning Manager::drop
        // into a deadlock; once that callback returns it observes `shutdown` and releases Handler.
        if self.worker.as_ref().is_some_and(JoinHandle::is_finished) {
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }
}

impl CallbackSender {
    pub(super) fn location(&self, location: LocationData, one_shot: bool, generation: u64) {
        self.enqueue(CallbackEvent::Location {
            location,
            one_shot,
            generation,
        });
    }

    pub(super) fn error(&self, error: Error) {
        self.enqueue(CallbackEvent::Error(error));
    }

    /// Drops fixes from an old accuracy request. Returns true if one of them was the pending
    /// `update_once` result, so the backend knows to keep waiting for a fresh one.
    pub(super) fn advance_generation(&self, generation: u64) -> bool {
        let mut queue = lock(&self.shared.queue);
        if queue.epoch != self.epoch {
            return false;
        }
        queue.accepted_generation = generation;
        let mut discarded_one_shot = false;
        queue.events.retain(|event| match event {
            CallbackEvent::Location {
                one_shot,
                generation: queued_generation,
                ..
            } if *queued_generation != generation => {
                discarded_one_shot |= *one_shot;
                false
            }
            _ => true,
        });
        if let Some(in_flight) = queue.in_flight.as_mut() {
            if in_flight.location
                && in_flight.generation != generation
                && in_flight.phase == CallbackPhase::Pending
            {
                in_flight.cancelled = true;
                discarded_one_shot |= in_flight.one_shot;
            }
        }
        discarded_one_shot
    }

    /// Suppresses fixes that were queued solely for a continuous stream. A fix that also completed
    /// `update_once` remains deliverable; stopping one mode must not cancel the other.
    pub(super) fn discard_continuous_locations(&self) {
        let mut queue = lock(&self.shared.queue);
        if queue.epoch != self.epoch {
            return;
        }
        queue.events.retain(|event| {
            !matches!(
                event,
                CallbackEvent::Location {
                    one_shot: false,
                    ..
                }
            )
        });
        if let Some(in_flight) = queue.in_flight.as_mut() {
            if in_flight.location
                && !in_flight.one_shot
                && in_flight.phase == CallbackPhase::Pending
            {
                in_flight.cancelled = true;
            }
        }
    }

    fn enqueue(&self, event: CallbackEvent) {
        let mut queue = lock(&self.shared.queue);
        if queue.shutdown {
            return;
        }
        // A retired backend's fixes are from a session whose authorization no longer applies. Errors are
        // exempt: `fail_closed` clears `available` first, so fencing them could swallow the only notice.
        if queue.epoch != self.epoch && matches!(event, CallbackEvent::Location { .. }) {
            return;
        }
        if matches!(
            &event,
            CallbackEvent::Location { generation, .. }
                if *generation != queue.accepted_generation
        ) {
            return;
        }
        if let CallbackEvent::Error(new) = &event {
            // Same error twice in a row tells the app nothing new, and coalescing keeps us bounded.
            if queue.events.iter().any(
                |queued| matches!(queued, CallbackEvent::Error(old) if same_error_kind(*new, *old)),
            ) {
                return;
            }
        }

        // Providers can outpace app code, so coalesce adjacent fixes and keep the newest.
        let coalesced = if let (
            CallbackEvent::Location {
                location,
                one_shot,
                generation,
            },
            Some(CallbackEvent::Location {
                location: queued_location,
                one_shot: queued_one_shot,
                generation: queued_generation,
            }),
        ) = (&event, queue.events.back_mut())
        {
            if generation == queued_generation {
                // Keep only the newest adjacent fix, but preserve the fact that an older fix had
                // completed a one-shot request.
                let keep_protected_newer = *queued_one_shot
                    && !*one_shot
                    && queued_location
                        .time
                        .zip(location.time)
                        .is_some_and(|(queued, incoming)| incoming <= queued);
                *queued_one_shot |= *one_shot;
                if !keep_protected_newer {
                    *queued_location = location.clone();
                }
                true
            } else {
                false
            }
        } else {
            false
        };
        if coalesced {
            return;
        }
        // Belt-and-braces bound; we can't actually get here (same-generation locations coalesce, errors
        // dedup by kind). Here so a future change degrades by dropping a fix instead of growing forever.
        if queue.events.len() >= CALLBACK_QUEUE_CAPACITY {
            // Never evict a one-shot result, since nothing will resend it. Continuous fixes and
            // errors are fine to drop.
            if let Some(position) = queue.events.iter().position(|queued| {
                matches!(
                    queued,
                    CallbackEvent::Location {
                        one_shot: false,
                        ..
                    } | CallbackEvent::Error(_)
                )
            }) {
                queue.events.remove(position);
            } else {
                return;
            }
        }
        queue.events.push_back(event);
        drop(queue);
        self.shared.wake.notify_one();
    }
}

fn dispatch_callbacks(shared: Arc<CallbackShared>, handler: Arc<dyn Handler>) {
    loop {
        let event = {
            let mut queue = lock(&shared.queue);
            while queue.events.is_empty() && !queue.shutdown {
                queue = shared
                    .wake
                    .wait(queue)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            if queue.shutdown {
                return;
            }
            let event = queue.events.pop_front();
            queue.in_flight = event.as_ref().map(|event| match event {
                CallbackEvent::Location {
                    one_shot,
                    generation,
                    ..
                } => InFlightCallback {
                    location: true,
                    one_shot: *one_shot,
                    generation: *generation,
                    phase: CallbackPhase::Pending,
                    cancelled: false,
                },
                CallbackEvent::Error(_) => InFlightCallback {
                    location: false,
                    one_shot: false,
                    generation: 0,
                    phase: CallbackPhase::Pending,
                    cancelled: false,
                },
            });
            event
        };

        if let Some(event) = event {
            let invoke = {
                let mut queue = lock(&shared.queue);
                let invoke = queue
                    .in_flight
                    .as_ref()
                    .is_some_and(|in_flight| !queue.shutdown && !in_flight.cancelled);
                if invoke {
                    if let Some(in_flight) = queue.in_flight.as_mut() {
                        in_flight.phase = CallbackPhase::Invoking;
                    }
                } else {
                    queue.in_flight = None;
                }
                invoke
            };
            if invoke {
                let _ = catch_unwind(AssertUnwindSafe(|| match event {
                    CallbackEvent::Location { location, .. } => handler.handle(crate::Location {
                        inner: Location { inner: &location },
                    }),
                    CallbackEvent::Error(error) => handler.error(error),
                }));
                lock(&shared.queue).in_flight = None;
            }
        }
    }
}

fn same_error_kind(left: Error, right: Error) -> bool {
    std::mem::discriminant(&left) == std::mem::discriminant(&right)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn direct_fallback_allowed() -> bool {
    // Sandboxed applications must remain on the portal so its permission decision cannot be
    // bypassed through an unusually exposed system bus.
    !Path::new("/.flatpak-info").exists()
        && env::var_os("FLATPAK_ID").is_none()
        && env::var_os("SNAP").is_none()
        && env::var_os("SNAP_NAME").is_none()
}

fn automatic_desktop_id() -> String {
    desktop_id_from_gio()
        .or_else(desktop_id_from_executable)
        .unwrap_or_else(|| "robius.location".to_owned())
}

fn desktop_id_from_gio() -> Option<String> {
    let launched_pid = env::var("GIO_LAUNCHED_DESKTOP_FILE_PID")
        .ok()?
        .parse::<u32>()
        .ok()?;
    if launched_pid != std::process::id() {
        return None;
    }
    let path = PathBuf::from(env::var_os("GIO_LAUNCHED_DESKTOP_FILE")?);
    desktop_id_for_path(&path, &xdg_application_dirs())
}

fn desktop_id_from_executable() -> Option<String> {
    let executable = env::current_exe().ok()?;
    let basename = executable.file_name()?.to_str()?;
    let candidate = sanitize_desktop_id_component(basename);
    if candidate.is_empty() {
        return None;
    }

    // Don't borrow some unrelated app's authorization just because it shares our executable name.
    Some(format!("robius.{candidate}"))
}

fn xdg_application_dirs() -> Vec<PathBuf> {
    let mut directories = Vec::new();
    if let Some(data_home) = env::var_os("XDG_DATA_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
    {
        if data_home.is_absolute() {
            directories.push(data_home.join("applications"));
        }
    }

    let data_dirs = env::var_os("XDG_DATA_DIRS")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    directories.extend(
        env::split_paths(&data_dirs)
            .filter(|path| path.is_absolute())
            .map(|path| path.join("applications")),
    );
    directories
}

fn desktop_id_for_path(path: &Path, application_dirs: &[PathBuf]) -> Option<String> {
    if !path.is_absolute() || !path.is_file() || path.extension()? != "desktop" {
        return None;
    }
    for directory in application_dirs {
        let Ok(relative) = path.strip_prefix(directory) else {
            continue;
        };
        if !relative
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
        {
            continue;
        }
        let relative = relative.to_str()?;
        let id = relative
            .strip_suffix(".desktop")?
            .replace(std::path::MAIN_SEPARATOR, "-");
        if let Ok(id) = validate_desktop_id(&id) {
            return Some(id);
        }
    }
    None
}

fn validate_desktop_id(desktop_id: &str) -> Result<String> {
    if desktop_id.is_empty()
        || desktop_id.len() > 255
        || desktop_id.ends_with(".desktop")
        || !desktop_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(Error::Unknown);
    }
    Ok(desktop_id.to_owned())
}

fn sanitize_desktop_id_component(value: &str) -> String {
    value
        .bytes()
        .take(200)
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-') {
                char::from(byte)
            } else {
                '_'
            }
        })
        .collect()
}

struct PortalManager {
    connection: Connection,
    shared: Arc<Shared>,
    token_prefix: String,
    next_token: Mutex<u64>,
    listeners: Vec<JoinHandle<()>>,
}

struct Shared {
    callbacks: CallbackSender,
    owner: OwnedUniqueName,
    // Serializes session-changing calls without holding the state mutex during D-Bus I/O.
    operations: Mutex<()>,
    state: Mutex<State>,
    available: AtomicBool,
    dropped: AtomicBool,
}

struct State {
    accuracy: Accuracy,
    callback_generation: u64,
    authorization_pending: bool,
    continuous: bool,
    one_shot: Option<OneShot>,
    session: Option<Session>,
    last_location: Option<LocationData>,
}

struct Session {
    handle: OwnedObjectPath,
    request: OwnedObjectPath,
    accuracy: Accuracy,
    phase: SessionPhase,
}

struct SessionCleanup {
    handle: OwnedObjectPath,
    request: Option<OwnedObjectPath>,
}

impl Session {
    fn into_cleanup(self) -> SessionCleanup {
        SessionCleanup {
            handle: self.handle,
            request: (self.phase == SessionPhase::Starting).then_some(self.request),
        }
    }
}

#[derive(Copy, Clone, Eq, PartialEq)]
enum SessionPhase {
    Starting,
    Active,
}

struct OneShot {
    deadline: Instant,
    delivered_cached: bool,
    /// A cached fix already delivered by this request. A portal update with an
    /// equal or older timestamp completes the request but is not delivered twice.
    newer_than: Option<SystemTime>,
}

fn portal_session_is_active(state: &State, path: Option<&OwnedObjectPath>) -> bool {
    state.session.as_ref().is_some_and(|session| {
        session.phase == SessionPhase::Active && path.map_or(true, |path| path == &session.handle)
    })
}

/// Keeps the public error separate from the security-sensitive decision to try GeoClue directly.
/// Only failures proving that the portal capability is absent are eligible for fallback.
struct PortalInitError {
    error: Error,
    fallback_allowed: bool,
}

impl PortalInitError {
    fn unavailable(error: Error) -> Self {
        Self {
            error,
            fallback_allowed: true,
        }
    }

    fn other(error: Error) -> Self {
        Self {
            error,
            fallback_allowed: false,
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct LocationData {
    coordinates: Coordinates,
    altitude: Option<f64>,
    bearing: Option<f64>,
    speed: Option<f64>,
    time: Option<SystemTime>,
    freshness: Freshness,
}

impl LocationData {
    /// The same fix, marked as a replay of one we already had rather than a new measurement.
    fn as_cached(&self) -> Self {
        Self {
            freshness: Freshness::Cached,
            ..self.clone()
        }
    }
}

impl PortalManager {
    fn new(callbacks: CallbackSender) -> std::result::Result<Self, PortalInitError> {
        let connection = ConnectionBuilder::session()
            .map_err(classify_session_connection_error)?
            .method_timeout(METHOD_TIMEOUT)
            .build()
            .map_err(classify_session_connection_error)?;

        // Subscribe before probing the service. If the portal changes owner during the probe, the
        // queued NameOwnerChanged signal will make this manager terminal before it can be used.
        let rule = MatchRule::builder()
            .msg_type(MessageType::Signal)
            .sender(PORTAL_DESTINATION)
            .map_err(|error| PortalInitError::other(map_zbus_error(error)))?
            .path_namespace(PORTAL_PATH)
            .map_err(|error| PortalInitError::other(map_zbus_error(error)))?
            .build();
        let messages = MessageIterator::for_match_rule(rule, &connection, Some(64))
            .map_err(|error| PortalInitError::other(map_zbus_error(error)))?;

        // Explicit iterator rather than `Proxy::receive_owner_changed`: precise match, and it stays safe
        // if another dependency turns on zbus's Tokio feature.
        let owner_rule = MatchRule::builder()
            .msg_type(MessageType::Signal)
            .sender(DBUS_DESTINATION)
            .map_err(|error| PortalInitError::other(map_zbus_error(error)))?
            .path(DBUS_PATH)
            .map_err(|error| PortalInitError::other(map_zbus_error(error)))?
            .interface(DBUS_INTERFACE)
            .map_err(|error| PortalInitError::other(map_zbus_error(error)))?
            .member("NameOwnerChanged")
            .map_err(|error| PortalInitError::other(map_zbus_error(error)))?
            .add_arg(PORTAL_DESTINATION)
            .map_err(|error| PortalInitError::other(map_zbus_error(error)))?
            .build();
        let owner_changes = MessageIterator::for_match_rule(owner_rule, &connection, Some(4))
            .map_err(|error| PortalInitError::other(map_zbus_error(error)))?;

        // Bind everything to the unique owner we probed, so a portal restart can't splice into our session.
        let (version, owner) =
            portal_version_and_owner(&connection).map_err(classify_portal_capability_error)?;
        if version < 1 {
            return Err(PortalInitError::unavailable(Error::PermanentlyUnavailable));
        }

        let shared = Arc::new(Shared {
            callbacks,
            owner,
            operations: Mutex::new(()),
            state: Mutex::new(State {
                accuracy: Accuracy::Approximate,
                callback_generation: 0,
                authorization_pending: false,
                continuous: false,
                one_shot: None,
                session: None,
                last_location: None,
            }),
            available: AtomicBool::new(true),
            dropped: AtomicBool::new(false),
        });

        let message_shared = shared.clone();
        let message_connection = connection.clone();
        let message_listener = thread::Builder::new()
            .name("robius-location-portal".into())
            .spawn(move || listen(messages, message_connection, message_shared))
            .map_err(|_| PortalInitError::other(Error::PermanentlyUnavailable))?;

        let owner_shared = shared.clone();
        let owner_connection = connection.clone();
        let owner_listener = match thread::Builder::new()
            .name("robius-location-portal-owner".into())
            .spawn(move || monitor_portal_owner(owner_changes, owner_connection, owner_shared))
        {
            Ok(listener) => listener,
            Err(_) => {
                shared.dropped.store(true, Ordering::Release);
                let _ = connection.clone().close();
                drop(message_listener);
                return Err(PortalInitError::other(Error::PermanentlyUnavailable));
            }
        };

        Ok(Self {
            connection,
            shared,
            token_prefix: random_token_prefix(),
            next_token: Mutex::new(0),
            listeners: vec![message_listener, owner_listener],
        })
    }

    pub fn request_authorization(&self, _access: Access, accuracy: Accuracy) -> Result<()> {
        let _operation = self.operation();
        ensure_available(&self.shared)?;

        let old_session = {
            let mut state = self.state();
            let accuracy_changed = portal_accuracy(state.accuracy) != portal_accuracy(accuracy);
            state.accuracy = accuracy;
            if accuracy_changed {
                // Never replay a fix obtained under a different privacy/precision request.
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

            match state.session.as_ref() {
                Some(session) if portal_accuracy(session.accuracy) == portal_accuracy(accuracy) => {
                    // An active session has already passed portal authorization. If it is still
                    // starting, attach this authorization request to its pending response.
                    if session.phase == SessionPhase::Starting {
                        state.authorization_pending = true;
                    }
                    return Ok(());
                }
                Some(_) => state.session.take().map(Session::into_cleanup),
                None => None,
            }
        };

        if let Some(session) = old_session {
            if let Err(error) = close_session(&self.connection, &self.shared.owner, session) {
                self.clear_intents_after_start_failure();
                fail_closed(&self.connection, &self.shared);
                return Err(error);
            }
        }

        self.state().authorization_pending = true;
        if let Err(error) = self.ensure_session() {
            self.clear_intents_after_start_failure();
            return Err(error);
        }

        Ok(())
    }

    pub fn update_once(&self) -> Result<()> {
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
        if let Err(error) = self.ensure_session() {
            if !had_one_shot {
                self.state().one_shot = None;
            }
            return Err(error);
        }

        // Reuse only a fix acquired by this same already-authorized portal session. A newly
        // starting session must first receive its permission response and a provider update.
        let mut state = self.state();
        let session_active = portal_session_is_active(&state, None);
        let cached = session_active
            .then(|| {
                state
                    .last_location
                    .as_ref()
                    .filter(|location| cached_fix_is_recent(location.time))
                    .map(LocationData::as_cached)
            })
            .flatten();

        if let Some(location) = cached.as_ref() {
            if let Some(one_shot) = state.one_shot.as_mut() {
                one_shot.newer_than = location.time;
                one_shot.delivered_cached = true;
            }
        }
        let generation = state.callback_generation;
        drop(state);
        if let Some(location) = cached {
            self.shared.callbacks.location(location, true, generation);
        }

        Ok(())
    }

    pub fn start_updates(&self) -> Result<()> {
        let _operation = self.operation();
        ensure_available(&self.shared)?;

        let was_continuous = {
            let mut state = self.state();
            let previous = state.continuous;
            state.continuous = true;
            previous
        };

        if let Err(error) = self.ensure_session() {
            if !was_continuous {
                self.state().continuous = false;
            }
            return Err(error);
        }

        Ok(())
    }

    pub fn stop_updates(&self) -> Result<()> {
        let _operation = self.operation();
        ensure_available(&self.shared)?;

        let session = {
            let mut state = self.state();
            state.continuous = false;
            if state.one_shot.is_none() && !state.authorization_pending {
                state.session.take().map(Session::into_cleanup)
            } else {
                None
            }
        };
        self.shared.callbacks.discard_continuous_locations();

        if let Some(session) = session {
            if let Err(error) = close_session(&self.connection, &self.shared.owner, session) {
                fail_closed(&self.connection, &self.shared);
                return Err(error);
            }
        }
        Ok(())
    }

    fn is_usable(&self) -> bool {
        ensure_available(&self.shared).is_ok()
    }

    fn one_shot_deadline(&self) -> Option<Instant> {
        self.state()
            .one_shot
            .as_ref()
            .map(|one_shot| one_shot.deadline)
    }

    fn expire_one_shot(&self) {
        let _operation = self.operation();
        if ensure_available(&self.shared).is_err() {
            return;
        }
        let (report, session_to_close) = {
            let mut state = self.state();
            let Some(one_shot) = state.one_shot.as_ref() else {
                return;
            };
            if one_shot.deadline > Instant::now() {
                return;
            }
            let one_shot = state.one_shot.take().expect("one-shot was checked above");
            let close = if !state.continuous && !state.authorization_pending {
                state.session.take().map(Session::into_cleanup)
            } else {
                None
            };
            (!one_shot.delivered_cached, close)
        };

        if report {
            self.shared.callbacks.error(Error::TemporarilyUnavailable);
        }
        if let Some(session) = session_to_close {
            if close_session(&self.connection, &self.shared.owner, session).is_err() {
                fail_closed(&self.connection, &self.shared);
            }
        }
    }

    /// Creates and starts a portal session if there isn't one. Caller must hold `operations`.
    fn ensure_session(&self) -> Result<()> {
        ensure_available(&self.shared)?;
        if self.state().session.is_some() {
            return Ok(());
        }

        let accuracy = self.state().accuracy;
        // Cached locations are scoped to the session that acquired them. A future authorization
        // may grant less accuracy or be denied, so never carry a fix across a session boundary.
        self.state().last_location = None;
        let session_token = self.next_portal_token("session")?;
        let mut options = HashMap::new();
        options.insert("session_handle_token", Value::from(session_token.as_str()));
        options.insert("accuracy", Value::from(portal_accuracy(accuracy)));
        // Zero thresholds ask the portal to forward every provider update. This is important for
        // both a prompt one-shot fix and true continuous updates.
        options.insert("distance-threshold", Value::from(0_u32));
        options.insert("time-threshold", Value::from(0_u32));

        let portal =
            location_proxy(&self.connection, &self.shared.owner).map_err(map_zbus_error)?;
        let session_handle: OwnedObjectPath = portal
            .call("CreateSession", &options)
            .map_err(map_zbus_error)?;
        if session_handle.as_str() == "/" {
            return Err(Error::Unknown);
        }
        if let Err(error) = ensure_available(&self.shared) {
            let _ = close_session_handle(&self.connection, &self.shared.owner, &session_handle);
            return Err(error);
        }

        let request_token = self.next_portal_token("request")?;
        let predicted_request = match predicted_request_path(&self.connection, &request_token) {
            Ok(path) => path,
            Err(error) => {
                if close_session_handle(&self.connection, &self.shared.owner, &session_handle)
                    .is_err()
                {
                    fail_closed(&self.connection, &self.shared);
                }
                return Err(error);
            }
        };
        {
            let mut state = self.state();
            state.session = Some(Session {
                handle: session_handle.clone(),
                request: predicted_request.clone(),
                accuracy,
                phase: SessionPhase::Starting,
            });
            // `Start` can raise a dialog even for a bare `update_once`. Mark it pending so the one-shot
            // timeout doesn't close the request out from under a dialog the user is still reading.
            // That dismisses it and we never get a `Response`, so no permission is ever stored.
            state.authorization_pending = true;
        }

        let mut start_options = HashMap::new();
        start_options.insert("handle_token", Value::from(request_token.as_str()));
        let result: std::result::Result<OwnedObjectPath, zbus::Error> =
            portal.call("Start", &(session_handle.clone(), "", start_options));

        match result {
            Ok(actual_request) => {
                ensure_available(&self.shared)?;
                // Usually identical to the predicted path. Keep the returned path in case a
                // conforming implementation chose to rewrite the handle token.
                if let Some(session) = self.state().session.as_mut() {
                    if session.handle == session_handle && session.phase == SessionPhase::Starting {
                        session.request = actual_request;
                    }
                }
                if self
                    .state()
                    .session
                    .as_ref()
                    .is_some_and(|session| session.handle == session_handle)
                {
                    Ok(())
                } else {
                    Err(Error::TemporarilyUnavailable)
                }
            }
            Err(error) => {
                {
                    let mut state = self.state();
                    // `Start` failed on the wire, so no `Response` will ever arrive and nothing is
                    // waiting on the user. Clear the guard set above so the session can be reaped.
                    state.authorization_pending = false;
                    if state
                        .session
                        .as_ref()
                        .is_some_and(|session| session.handle == session_handle)
                    {
                        state.session = None;
                    }
                }
                if close_request(&self.connection, &self.shared.owner, &predicted_request).is_err()
                    || close_session_handle(&self.connection, &self.shared.owner, &session_handle)
                        .is_err()
                {
                    fail_closed(&self.connection, &self.shared);
                }
                Err(map_zbus_error(error))
            }
        }
    }

    fn clear_intents_after_start_failure(&self) {
        let mut state = self.state();
        state.authorization_pending = false;
        state.continuous = false;
        state.one_shot = None;
    }

    fn next_portal_token(&self, kind: &str) -> Result<String> {
        let mut next = lock(&self.next_token);
        let sequence = *next;
        *next = next.checked_add(1).ok_or(Error::Unknown)?;
        Ok(format!("{}_{}_{sequence}", self.token_prefix, kind))
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn operation(&self) -> MutexGuard<'_, ()> {
        lock(&self.shared.operations)
    }
}

impl Drop for PortalManager {
    fn drop(&mut self) {
        self.shared.dropped.store(true, Ordering::Release);
        self.shared.available.store(false, Ordering::Release);

        let cleanup = self
            .shared
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .session
            .take()
            .map(Session::into_cleanup);

        // Send no-reply cleanup before disconnecting, so a wedged portal can't delay teardown.
        if let Some(cleanup) = cleanup {
            close_session_noreply(&self.connection, &self.shared.owner, cleanup);
        }

        // Closing any clone wakes the blocking signal iterator. Listeners never run app callbacks, but
        // can be mid-D-Bus-call, so don't make Drop wait.
        let _ = self.connection.clone().close();
        for listener in self.listeners.drain(..) {
            if listener.is_finished() {
                let _ = listener.join();
            }
        }
    }
}

pub struct Location<'a> {
    inner: &'a LocationData,
}

impl Location<'_> {
    pub fn coordinates(&self) -> Result<Coordinates> {
        Ok(self.inner.coordinates)
    }

    pub fn altitude(&self) -> Result<f64> {
        self.inner.altitude.ok_or(Error::TemporarilyUnavailable)
    }

    pub fn bearing(&self) -> Result<f64> {
        self.inner.bearing.ok_or(Error::TemporarilyUnavailable)
    }

    pub fn speed(&self) -> Result<f64> {
        self.inner.speed.ok_or(Error::TemporarilyUnavailable)
    }

    pub fn time(&self) -> Result<SystemTime> {
        self.inner.time.ok_or(Error::TemporarilyUnavailable)
    }

    pub fn freshness(&self) -> Freshness {
        self.inner.freshness
    }
}

/// Drains the match-rule queue and nothing else.
///
/// NOTE: this must never block. zbus drives every channel of a connection from one socket-reader
/// task with no overflow policy, so if this queue fills, the whole connection stops receiving
/// anything, including the method replies the worker is waiting on while `handle_message` wants
/// `operations`.
fn listen(messages: MessageIterator, connection: Connection, shared: Arc<Shared>) {
    let (work, queue) = mpsc::channel();
    let handler_shared = shared.clone();
    let handler_connection = connection.clone();
    let handler = thread::Builder::new()
        .name("robius-location-portal-signals".into())
        .spawn(move || {
            for message in queue {
                if handler_shared.dropped.load(Ordering::Acquire) {
                    return;
                }
                handle_message(&handler_connection, &handler_shared, &message);
            }
        });
    let Ok(handler) = handler else {
        fail_closed(&connection, &shared);
        return;
    };

    for message in messages {
        if shared.dropped.load(Ordering::Acquire) {
            break;
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
                fail_closed(&connection, &shared);
                return;
            }
        }
    }

    // Closing the channel ends the handler loop once it drains what it already has.
    drop(work);
    let _ = handler.join();
    if !shared.dropped.load(Ordering::Acquire) {
        fail_closed(&connection, &shared);
    }
}

fn monitor_portal_owner(changes: MessageIterator, connection: Connection, shared: Arc<Shared>) {
    for change in changes {
        if shared.dropped.load(Ordering::Acquire) {
            break;
        }
        let Ok(change) = change else {
            fail_closed(&connection, &shared);
            return;
        };
        // Only the bus driver may report ownership changes; match rules don't constrain the sender of a
        // directed signal, so without this any peer could forge a portal disappearance and kill us.
        if change.header().sender().map(|sender| sender.as_str()) != Some(DBUS_DESTINATION) {
            continue;
        }
        let Ok((name, old_owner, new_owner)) =
            change.body().deserialize::<(String, String, String)>()
        else {
            continue;
        };
        if name == PORTAL_DESTINATION
            && old_owner == shared.owner.as_str()
            && old_owner != new_owner
        {
            fail_closed(&connection, &shared);
            return;
        }
    }

    if !shared.dropped.load(Ordering::Acquire) {
        fail_closed(&connection, &shared);
    }
}

fn handle_message(connection: &Connection, shared: &Shared, message: &Message) {
    let header = message.header();
    if header.sender().map(|sender| sender.as_str()) != Some(shared.owner.as_str()) {
        return;
    }
    let Some(interface) = header.interface().map(|value| value.as_str()) else {
        return;
    };
    let Some(member) = header.member().map(|value| value.as_str()) else {
        return;
    };
    let _operation = lock(&shared.operations);
    if ensure_available(shared).is_err() {
        return;
    }

    match (interface, member) {
        (LOCATION_INTERFACE, "LocationUpdated") => {
            match message
                .body()
                .deserialize::<(OwnedObjectPath, HashMap<String, OwnedValue>)>()
            {
                Ok((session, values)) => handle_location(connection, shared, &session, values),
                Err(_) => handle_location_error(connection, shared, None, Error::Unknown),
            }
        }
        (REQUEST_INTERFACE, "Response") => {
            let Some(path) = header
                .path()
                .map(|path| OwnedObjectPath::from(path.to_owned()))
            else {
                return;
            };
            match message
                .body()
                .deserialize::<(u32, HashMap<String, OwnedValue>)>()
            {
                Ok((response, _results)) => {
                    handle_start_response(connection, shared, &path, response)
                }
                Err(_) => handle_start_response(connection, shared, &path, u32::MAX),
            }
        }
        (SESSION_INTERFACE, "Closed") => {
            if let Some(path) = header
                .path()
                .map(|path| OwnedObjectPath::from(path.to_owned()))
            {
                handle_session_closed(shared, &path);
            }
        }
        _ => {}
    }
}

fn handle_start_response(
    connection: &Connection,
    shared: &Shared,
    request: &OwnedObjectPath,
    response: u32,
) {
    let (session_to_close, error) = {
        let mut state = shared.state.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(session) = state.session.as_mut() else {
            return;
        };
        if &session.request != request {
            return;
        }

        if response == 0 {
            session.phase = SessionPhase::Active;
            state.authorization_pending = false;
            if !state.continuous && state.one_shot.is_none() {
                (state.session.take().map(Session::into_cleanup), None)
            } else {
                (None, None)
            }
        } else {
            let error = start_response_error(response).unwrap_or(Error::Unknown);
            state.callback_generation = state.callback_generation.wrapping_add(1);
            let _ = shared
                .callbacks
                .advance_generation(state.callback_generation);
            state.authorization_pending = false;
            state.continuous = false;
            state.one_shot = None;
            state.last_location = None;
            (state.session.take().map(Session::into_cleanup), Some(error))
        }
    };
    if let Some(error) = error {
        shared.callbacks.error(error);
    }
    if let Some(session) = session_to_close {
        if close_session(connection, &shared.owner, session).is_err() {
            fail_closed(connection, shared);
        }
    }
}

fn handle_location(
    connection: &Connection,
    shared: &Shared,
    session_path: &OwnedObjectPath,
    values: HashMap<String, OwnedValue>,
) {
    if !portal_session_is_active(
        &shared.state.lock().unwrap_or_else(PoisonError::into_inner),
        Some(session_path),
    ) {
        return;
    }
    let location = match parse_location(&values) {
        Ok(location) => location,
        Err(error) => {
            handle_location_error(connection, shared, Some(session_path), error);
            return;
        }
    };

    let session_to_close = {
        let mut state = shared.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !portal_session_is_active(&state, Some(session_path)) {
            return;
        }

        state.last_location = Some(location.clone());
        let continuous = state.continuous;
        let one_shot = state.one_shot.take();
        let deliver_one_shot = one_shot
            .as_ref()
            .is_some_and(|one_shot| one_shot_is_fresher(one_shot, location.time));
        let deliver = continuous || deliver_one_shot;

        let close = if !continuous && one_shot.is_some() && !state.authorization_pending {
            state.session.take().map(Session::into_cleanup)
        } else {
            None
        };
        if deliver {
            shared
                .callbacks
                .location(location, deliver_one_shot, state.callback_generation);
        }
        close
    };
    if let Some(session) = session_to_close {
        if close_session(connection, &shared.owner, session).is_err() {
            fail_closed(connection, shared);
        }
    }
}

fn handle_location_error(
    connection: &Connection,
    shared: &Shared,
    session_path: Option<&OwnedObjectPath>,
    error: Error,
) {
    let (should_report, session_to_close) = {
        let mut state = shared.state.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(session) = state.session.as_ref() else {
            return;
        };
        if session.phase != SessionPhase::Active
            || session_path.is_some_and(|path| path != &session.handle)
        {
            return;
        }

        let continuous = state.continuous;
        let had_one_shot = state.one_shot.take().is_some();
        let should_report = continuous || had_one_shot;
        let close = if had_one_shot && !continuous && !state.authorization_pending {
            state.session.take().map(Session::into_cleanup)
        } else {
            None
        };
        (should_report, close)
    };
    if should_report {
        shared.callbacks.error(error);
    }
    if let Some(session) = session_to_close {
        if close_session(connection, &shared.owner, session).is_err() {
            fail_closed(connection, shared);
        }
    }
}

fn handle_session_closed(shared: &Shared, session_path: &OwnedObjectPath) {
    let should_report = {
        let mut state = shared.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !state
            .session
            .as_ref()
            .is_some_and(|session| &session.handle == session_path)
        {
            return;
        }

        let active = state.authorization_pending || state.continuous || state.one_shot.is_some();
        state.callback_generation = state.callback_generation.wrapping_add(1);
        let discarded_one_shot = shared
            .callbacks
            .advance_generation(state.callback_generation);
        state.session = None;
        state.authorization_pending = false;
        state.continuous = false;
        state.one_shot = None;
        state.last_location = None;
        active || discarded_one_shot
    };
    if should_report {
        shared.callbacks.error(Error::TemporarilyUnavailable);
    }
}

fn provider_lost(shared: &Shared) {
    let should_report = {
        let mut state = shared.state.lock().unwrap_or_else(PoisonError::into_inner);
        let active = state.authorization_pending || state.continuous || state.one_shot.is_some();
        state.callback_generation = state.callback_generation.wrapping_add(1);
        let discarded_one_shot = shared
            .callbacks
            .advance_generation(state.callback_generation);
        state.session = None;
        state.authorization_pending = false;
        state.continuous = false;
        state.one_shot = None;
        // Provider loss is an authorization/session boundary. Never replay its cache.
        state.last_location = None;
        active || discarded_one_shot
    };
    if should_report {
        shared.callbacks.error(Error::TemporarilyUnavailable);
    }
}

fn fail_closed(connection: &Connection, shared: &Shared) {
    if shared.available.swap(false, Ordering::AcqRel) {
        provider_lost(shared);
    }
    let _ = connection.clone().close();
}

fn ensure_available(shared: &Shared) -> Result<()> {
    if shared.dropped.load(Ordering::Acquire) || !shared.available.load(Ordering::Acquire) {
        Err(Error::TemporarilyUnavailable)
    } else {
        Ok(())
    }
}

fn parse_location(values: &HashMap<String, OwnedValue>) -> Result<LocationData> {
    let latitude = required_f64(values, "Latitude")?;
    let longitude = required_f64(values, "Longitude")?;
    if !latitude.is_finite()
        || !longitude.is_finite()
        || !(-90.0..=90.0).contains(&latitude)
        || !(-180.0..=180.0).contains(&longitude)
    {
        return Err(Error::Unknown);
    }

    let altitude = optional_f64(values, "Altitude")?.and_then(|value| {
        if value == f64::MIN {
            None
        } else {
            Some(value)
        }
    });
    if altitude.is_some_and(|value| !value.is_finite()) {
        return Err(Error::Unknown);
    }

    let speed =
        optional_f64(values, "Speed")?.and_then(
            |value| {
                if value == -1.0 {
                    None
                } else {
                    Some(value)
                }
            },
        );
    if speed.is_some_and(|value| !value.is_finite() || value < 0.0) {
        return Err(Error::Unknown);
    }

    let bearing =
        optional_f64(values, "Heading")?.and_then(
            |value| {
                if value == -1.0 {
                    None
                } else {
                    Some(value)
                }
            },
        );
    if bearing.is_some_and(|value| !value.is_finite() || !(0.0..360.0).contains(&value)) {
        return Err(Error::Unknown);
    }

    let time = match values.get("Timestamp") {
        Some(value) => {
            let (seconds, microseconds): (u64, u64) = value
                .try_clone()
                .map_err(|_| Error::Unknown)?
                .try_into()
                .map_err(|_| Error::Unknown)?;
            if microseconds >= 1_000_000 {
                return Err(Error::Unknown);
            }
            let duration = Duration::from_secs(seconds)
                .checked_add(Duration::from_micros(microseconds))
                .ok_or(Error::Unknown)?;
            Some(
                SystemTime::UNIX_EPOCH
                    .checked_add(duration)
                    .ok_or(Error::Unknown)?,
            )
        }
        None => None,
    };

    Ok(LocationData {
        coordinates: Coordinates {
            latitude,
            longitude,
        },
        altitude,
        bearing,
        speed,
        time,
        freshness: Freshness::Live,
    })
}

fn required_f64(values: &HashMap<String, OwnedValue>, key: &str) -> Result<f64> {
    let value = values.get(key).ok_or(Error::Unknown)?;
    f64::try_from(value).map_err(|_| Error::Unknown)
}

fn optional_f64(values: &HashMap<String, OwnedValue>, key: &str) -> Result<Option<f64>> {
    values
        .get(key)
        .map(|value| f64::try_from(value).map_err(|_| Error::Unknown))
        .transpose()
}

fn one_shot_is_fresher(one_shot: &OneShot, current: Option<SystemTime>) -> bool {
    one_shot
        .newer_than
        .zip(current)
        .map_or(true, |(previous, current)| current > previous)
}

fn start_response_error(response: u32) -> Option<Error> {
    match response {
        0 => None,
        1 => Some(Error::AuthorizationDenied),
        2 => Some(Error::TemporarilyUnavailable),
        _ => Some(Error::Unknown),
    }
}

fn portal_version_and_owner(connection: &Connection) -> zbus::Result<(u32, OwnedUniqueName)> {
    let properties = Proxy::new(
        connection,
        PORTAL_DESTINATION,
        PORTAL_PATH,
        "org.freedesktop.DBus.Properties",
    )?;
    let reply = properties.call_method("Get", &(LOCATION_INTERFACE, "version"))?;
    let owner = reply
        .header()
        .sender()
        .cloned()
        .map(OwnedUniqueName::from)
        .ok_or(zbus::Error::MissingField)?;
    let value: OwnedValue = reply.body().deserialize()?;
    let version = u32::try_from(value).map_err(zbus::Error::Variant)?;
    Ok((version, owner))
}

fn location_proxy(
    connection: &Connection,
    owner: &OwnedUniqueName,
) -> zbus::Result<Proxy<'static>> {
    Proxy::new_owned(
        connection.clone(),
        owner.clone(),
        PORTAL_PATH,
        LOCATION_INTERFACE,
    )
}

fn close_session(
    connection: &Connection,
    owner: &OwnedUniqueName,
    cleanup: SessionCleanup,
) -> Result<()> {
    if let Some(request) = cleanup.request {
        close_request(connection, owner, &request)?;
    }
    close_session_handle(connection, owner, &cleanup.handle)
}

fn close_session_handle(
    connection: &Connection,
    owner: &OwnedUniqueName,
    path: &OwnedObjectPath,
) -> Result<()> {
    let proxy = Proxy::new_owned(
        connection.clone(),
        owner.clone(),
        path.clone(),
        SESSION_INTERFACE,
    )
    .map_err(map_zbus_error)?;

    match proxy.call::<_, _, ()>("Close", &()) {
        Ok(()) => Ok(()),
        Err(zbus::Error::MethodError(name, _, _))
            if name.as_str() == "org.freedesktop.DBus.Error.UnknownObject" =>
        {
            // The portal may have closed the session immediately before our explicit close.
            Ok(())
        }
        Err(error) => Err(map_zbus_error(error)),
    }
}

fn close_request(
    connection: &Connection,
    owner: &OwnedUniqueName,
    path: &OwnedObjectPath,
) -> Result<()> {
    let proxy = Proxy::new_owned(
        connection.clone(),
        owner.clone(),
        path.clone(),
        REQUEST_INTERFACE,
    )
    .map_err(map_zbus_error)?;

    match proxy.call::<_, _, ()>("Close", &()) {
        Ok(()) => Ok(()),
        Err(zbus::Error::MethodError(name, _, _))
            if name.as_str() == "org.freedesktop.DBus.Error.UnknownObject" =>
        {
            Ok(())
        }
        Err(error) => Err(map_zbus_error(error)),
    }
}

fn close_session_noreply(
    connection: &Connection,
    owner: &OwnedUniqueName,
    cleanup: SessionCleanup,
) {
    if let Some(request) = cleanup.request {
        if let Ok(proxy) = Proxy::new_owned(
            connection.clone(),
            owner.clone(),
            request,
            REQUEST_INTERFACE,
        ) {
            let _ = proxy.call_noreply("Close", &());
        }
    }
    if let Ok(proxy) = Proxy::new_owned(
        connection.clone(),
        owner.clone(),
        cleanup.handle,
        SESSION_INTERFACE,
    ) {
        let _ = proxy.call_noreply("Close", &());
    }
}

fn predicted_request_path(connection: &Connection, token: &str) -> Result<OwnedObjectPath> {
    let sender = connection
        .unique_name()
        .ok_or(Error::PermanentlyUnavailable)?
        .as_str()
        .trim_start_matches(':')
        .replace('.', "_");
    OwnedObjectPath::try_from(format!(
        "/org/freedesktop/portal/desktop/request/{sender}/{token}"
    ))
    .map_err(|_| Error::Unknown)
}

fn portal_accuracy(accuracy: Accuracy) -> u32 {
    match accuracy {
        Accuracy::Approximate => ACCURACY_CITY,
        Accuracy::Precise => ACCURACY_EXACT,
    }
}

fn classify_session_connection_error(error: zbus::Error) -> PortalInitError {
    let session_bus_absent = matches!(&error, zbus::Error::Address(_) | zbus::Error::Unsupported)
        || matches!(
            &error,
            zbus::Error::InputOutput(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                )
        );
    let public = map_zbus_error(error);
    if session_bus_absent {
        PortalInitError::unavailable(public)
    } else {
        PortalInitError::other(public)
    }
}

/// Does a failed Location probe mean the portal capability is absent (GeoClue is fair game), or
/// just that the portal is unhealthy right now (don't bypass it)?
///
/// NOTE: activation failures are not absence. A `Spawn.*` other than `ServiceNotFound` means a
/// `.service` file exists, and `NameHasNoOwner` just means nobody owns the name this second.
fn classify_portal_capability_error(error: zbus::Error) -> PortalInitError {
    let capability_absent = match &error {
        zbus::Error::InterfaceNotFound => true,
        zbus::Error::MethodError(name, _, _) => matches!(
            name.as_str(),
            "org.freedesktop.DBus.Error.ServiceUnknown"
                | "org.freedesktop.DBus.Error.NoServer"
                | "org.freedesktop.DBus.Error.UnknownInterface"
                | "org.freedesktop.DBus.Error.UnknownMethod"
                | "org.freedesktop.DBus.Error.UnknownObject"
                | "org.freedesktop.DBus.Error.UnknownProperty"
                // GDBus reports an unregistered interface as `InvalidArgs`, i.e. no Location backend.
                | "org.freedesktop.DBus.Error.InvalidArgs"
                | "org.freedesktop.DBus.Error.Spawn.ServiceNotFound"
        ),
        zbus::Error::FDO(error) => matches!(
            error.as_ref(),
            zbus::fdo::Error::ServiceUnknown(_)
                | zbus::fdo::Error::NoServer(_)
                | zbus::fdo::Error::UnknownInterface(_)
                | zbus::fdo::Error::UnknownMethod(_)
                | zbus::fdo::Error::UnknownObject(_)
                | zbus::fdo::Error::UnknownProperty(_)
                | zbus::fdo::Error::InvalidArgs(_)
        ),
        _ => false,
    };
    let error = map_zbus_error(error);
    if capability_absent {
        PortalInitError::unavailable(error)
    } else {
        PortalInitError::other(error)
    }
}

fn map_zbus_error(error: zbus::Error) -> Error {
    match error {
        zbus::Error::InterfaceNotFound
        | zbus::Error::Address(_)
        | zbus::Error::Handshake(_)
        | zbus::Error::Unsupported => Error::PermanentlyUnavailable,
        zbus::Error::InputOutput(_) => Error::TemporarilyUnavailable,
        zbus::Error::MethodError(name, _, _) => match name.as_str() {
            "org.freedesktop.DBus.Error.AccessDenied"
            | "org.freedesktop.DBus.Error.AuthFailed"
            | "org.freedesktop.portal.Error.NotAllowed"
            | "org.freedesktop.portal.Error.Cancelled" => Error::AuthorizationDenied,
            "org.freedesktop.DBus.Error.ServiceUnknown"
            | "org.freedesktop.DBus.Error.NameHasNoOwner"
            | "org.freedesktop.DBus.Error.NoServer"
            | "org.freedesktop.DBus.Error.UnknownInterface"
            | "org.freedesktop.DBus.Error.UnknownMethod"
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
            | "org.freedesktop.DBus.Error.IOError" => Error::TemporarilyUnavailable,
            _ => Error::Unknown,
        },
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

fn random_token_prefix() -> String {
    let mut bytes = [0_u8; 16];
    if File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .is_err()
    {
        // Fallback for odd containers. Unpredictability is only a portal recommendation; we just need
        // uniqueness, and the unique bus name already scopes these.
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        bytes.copy_from_slice(&now.to_le_bytes());
        let pid = std::process::id().to_le_bytes();
        for (byte, pid_byte) in bytes.iter_mut().zip(pid.iter().cycle()) {
            *byte ^= pid_byte;
        }
    }

    let mut token = String::with_capacity("robius_location_".len() + bytes.len() * 2);
    token.push_str("robius_location_");
    for byte in bytes {
        // Writing to a `String` cannot fail.
        let _ = write!(token, "{byte:02x}");
    }
    token
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    struct ErrorRecorder(Arc<Mutex<Vec<Error>>>);

    impl Handler for ErrorRecorder {
        fn handle(&self, _location: crate::Location<'_>) {}

        fn error(&self, error: Error) {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(error);
        }
    }

    struct PanicThenRecord {
        calls: AtomicUsize,
        result: SyncSender<Error>,
    }

    impl Handler for PanicThenRecord {
        fn handle(&self, _location: crate::Location<'_>) {}

        fn error(&self, error: Error) {
            if self.calls.fetch_add(1, Ordering::Relaxed) == 0 {
                panic!("intentional callback panic");
            }
            let _ = self.result.send(error);
        }
    }

    fn value<T>(value: T) -> OwnedValue
    where
        OwnedValue: From<T>,
    {
        OwnedValue::from(value)
    }

    fn valid_values() -> HashMap<String, OwnedValue> {
        HashMap::from([
            ("Latitude".into(), value(37.7749_f64)),
            ("Longitude".into(), value(-122.4194_f64)),
            ("Altitude".into(), value(16.25_f64)),
            ("Speed".into(), value(2.5_f64)),
            ("Heading".into(), value(270.0_f64)),
            (
                "Timestamp".into(),
                OwnedValue::try_from(Value::from((1_700_000_000_u64, 123_456_u64))).unwrap(),
            ),
        ])
    }

    fn test_location(latitude: f64) -> LocationData {
        LocationData {
            coordinates: Coordinates {
                latitude,
                longitude: 0.0,
            },
            altitude: None,
            bearing: None,
            speed: None,
            time: None,
            freshness: Freshness::Live,
        }
    }

    #[test]
    fn replaying_a_stored_fix_marks_it_cached() {
        let stored = test_location(1.0);
        assert_eq!(stored.as_cached().freshness, Freshness::Cached);
        // The stored copy is untouched, so a later provider update is still a live fix.
        assert_eq!(stored.freshness, Freshness::Live);
    }

    fn test_callback_sender() -> (CallbackSender, Arc<CallbackShared>) {
        let shared = Arc::new(CallbackShared {
            queue: Mutex::new(CallbackQueue {
                events: VecDeque::new(),
                in_flight: None,
                accepted_generation: 0,
                epoch: 0,
                shutdown: false,
            }),
            wake: Condvar::new(),
        });
        (
            CallbackSender {
                shared: shared.clone(),
                epoch: 0,
            },
            shared,
        )
    }

    #[test]
    fn parses_every_location_field() {
        let location = parse_location(&valid_values()).unwrap();
        assert_eq!(location.coordinates.latitude, 37.7749);
        assert_eq!(location.coordinates.longitude, -122.4194);
        assert_eq!(location.altitude, Some(16.25));
        assert_eq!(location.speed, Some(2.5));
        assert_eq!(location.bearing, Some(270.0));
        assert_eq!(
            location.time,
            Some(
                SystemTime::UNIX_EPOCH
                    + Duration::from_secs(1_700_000_000)
                    + Duration::from_micros(123_456)
            )
        );
    }

    #[test]
    fn optional_fields_may_be_absent() {
        let values = HashMap::from([
            ("Latitude".into(), value(0.0_f64)),
            ("Longitude".into(), value(0.0_f64)),
        ]);
        let location = parse_location(&values).unwrap();
        assert_eq!(location.altitude, None);
        assert_eq!(location.speed, None);
        assert_eq!(location.bearing, None);
        assert_eq!(location.time, None);
    }

    #[test]
    fn geoclue_unknown_sentinels_are_unavailable() {
        let mut values = valid_values();
        values.insert("Altitude".into(), value(f64::MIN));
        values.insert("Speed".into(), value(-1.0_f64));
        values.insert("Heading".into(), value(-1.0_f64));
        let location = parse_location(&values).unwrap();
        assert_eq!(location.altitude, None);
        assert_eq!(location.speed, None);
        assert_eq!(location.bearing, None);
    }

    #[test]
    fn rejects_missing_or_invalid_coordinates() {
        let mut values = valid_values();
        values.remove("Latitude");
        assert!(matches!(parse_location(&values), Err(Error::Unknown)));

        for (key, invalid) in [
            ("Latitude", 90.000_001),
            ("Latitude", f64::NAN),
            ("Longitude", -180.000_001),
            ("Longitude", f64::INFINITY),
        ] {
            let mut values = valid_values();
            values.insert(key.into(), value(invalid));
            assert!(matches!(parse_location(&values), Err(Error::Unknown)));
        }
    }

    #[test]
    fn rejects_malformed_optional_fields_and_timestamps() {
        for (key, invalid) in [
            ("Altitude", f64::NAN),
            ("Speed", -2.0),
            ("Speed", f64::INFINITY),
            ("Heading", 360.0),
            ("Heading", -2.0),
        ] {
            let mut values = valid_values();
            values.insert(key.into(), value(invalid));
            assert!(matches!(parse_location(&values), Err(Error::Unknown)));
        }

        let mut values = valid_values();
        values.insert(
            "Speed".into(),
            OwnedValue::try_from(Value::from("fast")).unwrap(),
        );
        assert!(matches!(parse_location(&values), Err(Error::Unknown)));

        let mut values = valid_values();
        values.insert(
            "Timestamp".into(),
            OwnedValue::try_from(Value::from((1_u64, 1_000_000_u64))).unwrap(),
        );
        assert!(matches!(parse_location(&values), Err(Error::Unknown)));
    }

    #[test]
    fn maps_accuracy_to_portal_levels() {
        assert_eq!(portal_accuracy(Accuracy::Approximate), ACCURACY_CITY);
        assert_eq!(portal_accuracy(Accuracy::Precise), ACCURACY_EXACT);
    }

    #[test]
    fn maps_portal_response_codes() {
        assert!(start_response_error(0).is_none());
        assert!(matches!(
            start_response_error(1),
            Some(Error::AuthorizationDenied)
        ));
        assert!(matches!(
            start_response_error(2),
            Some(Error::TemporarilyUnavailable)
        ));
        assert!(matches!(start_response_error(3), Some(Error::Unknown)));
        assert!(matches!(
            start_response_error(u32::MAX),
            Some(Error::Unknown)
        ));
    }

    #[test]
    fn maps_transport_and_service_errors() {
        let unavailable = zbus::Error::FDO(Box::new(zbus::fdo::Error::ServiceUnknown(
            "not installed".into(),
        )));
        assert!(matches!(
            map_zbus_error(unavailable),
            Error::PermanentlyUnavailable
        ));

        let denied = zbus::Error::FDO(Box::new(zbus::fdo::Error::AccessDenied("denied".into())));
        assert!(matches!(map_zbus_error(denied), Error::AuthorizationDenied));

        let network = zbus::Error::FDO(Box::new(zbus::fdo::Error::NoNetwork("offline".into())));
        assert!(matches!(map_zbus_error(network), Error::Network));

        let io = zbus::Error::InputOutput(Arc::new(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "reset",
        )));
        assert!(matches!(map_zbus_error(io), Error::TemporarilyUnavailable));
    }

    #[test]
    fn callback_panics_are_isolated_and_dispatch_continues() {
        let (result, receiver) = mpsc::sync_channel(1);
        let callbacks = CallbackDispatcher::new(Arc::new(PanicThenRecord {
            calls: AtomicUsize::new(0),
            result,
        }))
        .unwrap();
        let sender = callbacks.sender();
        sender.error(Error::Unknown);
        sender.error(Error::Network);
        assert!(matches!(
            receiver.recv_timeout(Duration::from_secs(1)),
            Ok(Error::Network)
        ));
    }

    #[test]
    fn callback_queue_coalesces_locations_and_stays_bounded() {
        let (sender, shared) = test_callback_sender();
        for latitude in 0..100 {
            sender.location(test_location(f64::from(latitude)), false, 0);
        }
        let queue = lock(&shared.queue);
        assert_eq!(queue.events.len(), 1);
        let Some(CallbackEvent::Location { location, .. }) = queue.events.front() else {
            panic!("expected a coalesced location");
        };
        assert_eq!(location.coordinates.latitude, 99.0);
        assert!(queue.events.len() <= CALLBACK_QUEUE_CAPACITY);
    }

    #[test]
    fn older_continuous_fix_does_not_replace_a_protected_one_shot() {
        let (sender, shared) = test_callback_sender();
        let mut newer = test_location(1.0);
        newer.time = Some(SystemTime::UNIX_EPOCH + Duration::from_secs(200));
        let mut older = test_location(2.0);
        older.time = Some(SystemTime::UNIX_EPOCH + Duration::from_secs(100));
        sender.location(newer, true, 0);
        sender.location(older, false, 0);

        let queue = lock(&shared.queue);
        let Some(CallbackEvent::Location {
            location, one_shot, ..
        }) = queue.events.front()
        else {
            panic!("expected a protected location");
        };
        assert!(*one_shot);
        assert_eq!(location.coordinates.latitude, 1.0);
    }

    #[test]
    fn only_starting_sessions_cancel_their_request_on_cleanup() {
        let handle =
            OwnedObjectPath::try_from("/org/freedesktop/portal/desktop/session/test/one").unwrap();
        let request =
            OwnedObjectPath::try_from("/org/freedesktop/portal/desktop/request/test/one").unwrap();
        let starting = Session {
            handle: handle.clone(),
            request: request.clone(),
            accuracy: Accuracy::Precise,
            phase: SessionPhase::Starting,
        }
        .into_cleanup();
        assert_eq!(starting.handle, handle);
        assert_eq!(starting.request, Some(request.clone()));

        let active = Session {
            handle,
            request,
            accuracy: Accuracy::Precise,
            phase: SessionPhase::Active,
        }
        .into_cleanup();
        assert!(active.request.is_none());
    }

    #[test]
    fn advancing_callback_generation_cancels_and_rejects_stale_fixes() {
        let (sender, shared) = test_callback_sender();
        sender.location(test_location(1.0), false, 0);
        sender.error(Error::Network);
        sender.location(test_location(2.0), true, 0);
        lock(&shared.queue).in_flight = Some(InFlightCallback {
            location: true,
            one_shot: true,
            generation: 0,
            phase: CallbackPhase::Pending,
            cancelled: false,
        });

        assert!(sender.advance_generation(1));
        {
            let queue = lock(&shared.queue);
            assert!(queue
                .events
                .iter()
                .all(|event| !matches!(event, CallbackEvent::Location { .. })));
            assert!(queue.in_flight.as_ref().unwrap().cancelled);
            assert_eq!(queue.accepted_generation, 1);
        }

        sender.location(test_location(3.0), true, 0);
        assert!(lock(&shared.queue)
            .events
            .iter()
            .all(|event| !matches!(event, CallbackEvent::Location { .. })));
        sender.location(test_location(4.0), true, 1);
        assert!(lock(&shared.queue)
            .events
            .iter()
            .any(|event| matches!(event, CallbackEvent::Location { generation: 1, .. })));
    }

    #[test]
    fn stopping_continuous_callbacks_preserves_one_shot_results() {
        let (sender, shared) = test_callback_sender();
        sender.location(test_location(1.0), false, 0);
        sender.error(Error::Network);
        sender.location(test_location(2.0), true, 0);
        lock(&shared.queue).in_flight = Some(InFlightCallback {
            location: true,
            one_shot: false,
            generation: 0,
            phase: CallbackPhase::Pending,
            cancelled: false,
        });

        sender.discard_continuous_locations();
        let queue = lock(&shared.queue);
        assert!(queue.in_flight.as_ref().unwrap().cancelled);
        assert_eq!(
            queue
                .events
                .iter()
                .filter(|event| matches!(event, CallbackEvent::Location { .. }))
                .count(),
            1
        );
        assert!(queue
            .events
            .iter()
            .any(|event| matches!(event, CallbackEvent::Location { one_shot: true, .. })));
    }

    #[test]
    fn callback_shutdown_cancels_pending_but_not_invoking_work() {
        let (_sender, shared) = test_callback_sender();
        {
            let mut queue = lock(&shared.queue);
            queue.events.push_back(CallbackEvent::Error(Error::Network));
            queue.in_flight = Some(InFlightCallback {
                location: true,
                one_shot: true,
                generation: 0,
                phase: CallbackPhase::Pending,
                cancelled: false,
            });
            queue.shut_down();
            assert!(queue.events.is_empty());
            assert!(queue.in_flight.as_ref().unwrap().cancelled);
        }

        let (_sender, shared) = test_callback_sender();
        let mut queue = lock(&shared.queue);
        queue.in_flight = Some(InFlightCallback {
            location: true,
            one_shot: true,
            generation: 0,
            phase: CallbackPhase::Invoking,
            cancelled: false,
        });
        queue.shut_down();
        assert!(!queue.in_flight.as_ref().unwrap().cancelled);
    }

    #[test]
    fn portal_locations_require_an_active_matching_session() {
        let handle =
            OwnedObjectPath::try_from("/org/freedesktop/portal/desktop/session/test/one").unwrap();
        let other =
            OwnedObjectPath::try_from("/org/freedesktop/portal/desktop/session/test/two").unwrap();
        let mut state = State {
            accuracy: Accuracy::Precise,
            callback_generation: 0,
            authorization_pending: true,
            continuous: false,
            one_shot: None,
            session: Some(Session {
                handle: handle.clone(),
                request: OwnedObjectPath::try_from(
                    "/org/freedesktop/portal/desktop/request/test/one",
                )
                .unwrap(),
                accuracy: Accuracy::Precise,
                phase: SessionPhase::Starting,
            }),
            last_location: None,
        };
        assert!(!portal_session_is_active(&state, None));
        assert!(!portal_session_is_active(&state, Some(&handle)));
        state.session.as_mut().unwrap().phase = SessionPhase::Active;
        assert!(portal_session_is_active(&state, None));
        assert!(portal_session_is_active(&state, Some(&handle)));
        assert!(!portal_session_is_active(&state, Some(&other)));
    }

    #[test]
    fn direct_fallback_requires_definitive_portal_absence() {
        let refused = zbus::Error::InputOutput(Arc::new(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "no session bus",
        )));
        assert!(classify_session_connection_error(refused).fallback_allowed);

        let reset = zbus::Error::InputOutput(Arc::new(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "portal crashed",
        )));
        assert!(!classify_session_connection_error(reset).fallback_allowed);

        let missing = zbus::Error::FDO(Box::new(zbus::fdo::Error::ServiceUnknown(
            "not installed".into(),
        )));
        assert!(classify_portal_capability_error(missing).fallback_allowed);

        let denied = zbus::Error::FDO(Box::new(zbus::fdo::Error::AccessDenied("denied".into())));
        assert!(!classify_portal_capability_error(denied).fallback_allowed);

        // A portal built without the Location backend reports `InvalidArgs`, so fall back.
        let no_location_backend =
            zbus::Error::FDO(Box::new(zbus::fdo::Error::InvalidArgs("no such iface".into())));
        assert!(classify_portal_capability_error(no_location_backend).fallback_allowed);
        let no_location_backend = zbus::Error::MethodError(
            "org.freedesktop.DBus.Error.InvalidArgs".try_into().unwrap(),
            None,
            Message::method_call("/", "Get").unwrap().build(&()).unwrap(),
        );
        assert!(classify_portal_capability_error(no_location_backend).fallback_allowed);

        // Installed but not starting right now still counts as present, so don't downgrade.
        for transient in [
            "org.freedesktop.DBus.Error.Spawn.ExecFailed",
            "org.freedesktop.DBus.Error.Spawn.ChildExited",
            "org.freedesktop.DBus.Error.Spawn.ChildSignaled",
            "org.freedesktop.DBus.Error.Spawn.Failed",
            "org.freedesktop.DBus.Error.NameHasNoOwner",
        ] {
            let error = zbus::Error::MethodError(
                transient.try_into().unwrap(),
                None,
                Message::method_call("/", "Get").unwrap().build(&()).unwrap(),
            );
            assert!(
                !classify_portal_capability_error(error).fallback_allowed,
                "{transient} must not be treated as capability absence"
            );
        }

        // A missing `.service` file, by contrast, does prove the portal is not installed.
        let not_installed = zbus::Error::MethodError(
            "org.freedesktop.DBus.Error.Spawn.ServiceNotFound"
                .try_into()
                .unwrap(),
            None,
            Message::method_call("/", "Get").unwrap().build(&()).unwrap(),
        );
        assert!(classify_portal_capability_error(not_installed).fallback_allowed);
    }

    #[test]
    fn provider_restart_clears_active_work_and_cache_once() {
        let errors = Arc::new(Mutex::new(Vec::new()));
        let callbacks = CallbackDispatcher::new(Arc::new(ErrorRecorder(errors.clone()))).unwrap();
        let shared = Shared {
            callbacks: callbacks.sender(),
            owner: OwnedUniqueName::try_from(":1.4242").unwrap(),
            operations: Mutex::new(()),
            state: Mutex::new(State {
                accuracy: Accuracy::Precise,
                callback_generation: 0,
                authorization_pending: true,
                continuous: true,
                one_shot: Some(OneShot {
                    deadline: Instant::now() + ONE_SHOT_TIMEOUT,
                    delivered_cached: false,
                    newer_than: None,
                }),
                session: None,
                last_location: Some(parse_location(&valid_values()).unwrap()),
            }),
            available: AtomicBool::new(true),
            dropped: AtomicBool::new(false),
        };

        provider_lost(&shared);
        {
            let state = shared.state.lock().unwrap();
            assert!(!state.authorization_pending);
            assert!(!state.continuous);
            assert!(state.one_shot.is_none());
            assert!(state.last_location.is_none());
        }
        let deadline = Instant::now() + Duration::from_secs(1);
        while errors.lock().unwrap().is_empty() && Instant::now() < deadline {
            thread::yield_now();
        }
        assert!(matches!(
            errors.lock().unwrap().as_slice(),
            [Error::TemporarilyUnavailable]
        ));

        // Both portal-owner and message streams can notice one disconnect; only the first reports.
        provider_lost(&shared);
        assert_eq!(errors.lock().unwrap().len(), 1);
        drop(shared);
        drop(callbacks);
    }

    #[test]
    fn terminal_invalidation_replaces_a_queued_one_shot_with_an_error() {
        let (callbacks, callback_shared) = test_callback_sender();
        callbacks.location(test_location(1.0), true, 0);
        let shared = Shared {
            callbacks,
            owner: OwnedUniqueName::try_from(":1.4242").unwrap(),
            operations: Mutex::new(()),
            state: Mutex::new(State {
                accuracy: Accuracy::Precise,
                callback_generation: 0,
                authorization_pending: false,
                continuous: false,
                one_shot: None,
                session: None,
                last_location: Some(test_location(1.0)),
            }),
            available: AtomicBool::new(true),
            dropped: AtomicBool::new(false),
        };

        provider_lost(&shared);
        let queue = lock(&callback_shared.queue);
        assert!(queue
            .events
            .iter()
            .all(|event| !matches!(event, CallbackEvent::Location { .. })));
        assert!(queue
            .events
            .iter()
            .any(|event| matches!(event, CallbackEvent::Error(Error::TemporarilyUnavailable))));
    }

    #[test]
    fn cached_one_shot_only_redelivers_a_newer_timestamp() {
        let previous = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        let one_shot = OneShot {
            deadline: Instant::now() + ONE_SHOT_TIMEOUT,
            delivered_cached: true,
            newer_than: Some(previous),
        };
        assert!(!one_shot_is_fresher(
            &one_shot,
            Some(previous - Duration::from_secs(1))
        ));
        assert!(!one_shot_is_fresher(&one_shot, Some(previous)));
        assert!(one_shot_is_fresher(
            &one_shot,
            Some(previous + Duration::from_micros(1))
        ));
        // If a provider omits timestamps, it is safer to deliver the fix than silently discard it.
        assert!(one_shot_is_fresher(&one_shot, None));
        assert!(one_shot_is_fresher(
            &OneShot {
                deadline: Instant::now() + ONE_SHOT_TIMEOUT,
                delivered_cached: false,
                newer_than: None,
            },
            Some(previous)
        ));
    }

    #[test]
    fn location_accessors_report_unavailable_optional_values() {
        let values = HashMap::from([
            ("Latitude".into(), value(12.5_f64)),
            ("Longitude".into(), value(-45.25_f64)),
        ]);
        let data = parse_location(&values).unwrap();
        let location = Location { inner: &data };
        let coordinates = location.coordinates().unwrap();
        assert_eq!(coordinates.latitude, 12.5);
        assert_eq!(coordinates.longitude, -45.25);
        assert!(matches!(
            location.altitude(),
            Err(Error::TemporarilyUnavailable)
        ));
        assert!(matches!(
            location.bearing(),
            Err(Error::TemporarilyUnavailable)
        ));
        assert!(matches!(
            location.speed(),
            Err(Error::TemporarilyUnavailable)
        ));
        assert!(matches!(
            location.time(),
            Err(Error::TemporarilyUnavailable)
        ));
    }

    #[test]
    fn generated_tokens_are_valid_object_path_elements_and_unique() {
        let first = random_token_prefix();
        let second = random_token_prefix();
        assert_ne!(first, second);
        for token in [first, second] {
            assert!(token
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'));
            assert!(zbus::zvariant::ObjectPath::try_from(format!("/{token}")).is_ok());
        }
    }

    #[test]
    fn validates_explicit_desktop_ids() {
        for valid in ["firefox", "org.example.Location_App", "vendor-app"] {
            assert_eq!(validate_desktop_id(valid).unwrap(), valid);
        }
        for invalid in [
            "",
            ".desktop",
            "org.example.App.desktop",
            "contains/slash",
            "contains space",
            "unicode-☃",
        ] {
            assert!(matches!(validate_desktop_id(invalid), Err(Error::Unknown)));
        }
        assert!(matches!(
            validate_desktop_id(&"a".repeat(256)),
            Err(Error::Unknown)
        ));
    }

    #[test]
    fn sanitizes_executable_names_for_synthetic_ids() {
        assert_eq!(
            sanitize_desktop_id_component("my app/location"),
            "my_app_location"
        );
        assert_eq!(sanitize_desktop_id_component("normal-app"), "normal-app");
        assert!(sanitize_desktop_id_component(&"a".repeat(300)).len() <= 200);
    }

    #[test]
    fn derives_nested_xdg_desktop_file_ids() {
        let root = env::temp_dir().join(format!("robius-location-{}", random_token_prefix()));
        let applications = root.join("applications");
        let nested = applications.join("vendor/org.example.App.desktop");
        std::fs::create_dir_all(nested.parent().unwrap()).unwrap();
        std::fs::write(&nested, b"[Desktop Entry]\nType=Application\nName=Test\n").unwrap();

        assert_eq!(
            desktop_id_for_path(&nested, std::slice::from_ref(&applications)).as_deref(),
            Some("vendor-org.example.App")
        );
        assert!(desktop_id_for_path(&nested, &[root.join("elsewhere")]).is_none());

        std::fs::remove_dir_all(root).unwrap();
    }
}
