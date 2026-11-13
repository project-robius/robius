//! Linux desktops share the theme through the XDG Desktop Portal: the
//! `color-scheme` setting of `org.freedesktop.portal.Settings` on the session bus.
//! Desktops from before that setting (such as GNOME before 42, KDE Plasma before
//! 5.24, and older Cinnamon, MATE and Xfce) only name a theme, and window managers
//! without a desktop use GTK's own settings file, so `desktop.rs` reads those.
//!
//! One thread, `robius-theme`, keeps a bus connection and reads the theme again
//! each time the portal or one of those settings files says that it changed. zbus
//! is only used inside `zbus::block_on` on our own threads, and subscribers are only
//! called between two such calls, which works the same with zbus on async-io or on Tokio.

mod desktop;

use std::{
    future::{poll_fn, Future},
    panic::catch_unwind,
    pin::{pin, Pin},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Mutex, PoisonError,
    },
    task::{Poll, Waker},
    thread,
    time::Duration,
};

use zbus::{connection::Builder, export::futures_core::Stream, zvariant::Value, Connection, Message, MessageStream};

use crate::{Error, Result, Theme};
use desktop::Desktop;

const BUS: &str = "org.freedesktop.DBus";
const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const SETTINGS: &str = "org.freedesktop.portal.Settings";
const COLOR_SCHEME: (&str, &str) = ("org.freedesktop.appearance", "color-scheme");

/// The bus sends this when the portal starts, exits or is replaced.
const PORTAL_OWNER_CHANGED: &str = "type='signal',sender='org.freedesktop.DBus',\
    path='/org/freedesktop/DBus',interface='org.freedesktop.DBus',\
    member='NameOwnerChanged',arg0='org.freedesktop.portal.Desktop'";

/// How long `query` lets the portal take to answer, which can include starting it.
const QUERY_TIMEOUT: Duration = Duration::from_secs(1);

/// Whether the worker thread is alive. It ends if the session bus goes away.
static IS_RUNNING: AtomicBool = AtomicBool::new(false);
/// Whether that worker has given subscribers a theme, which it then keeps current.
static IS_LATEST_CURRENT: AtomicBool = AtomicBool::new(false);
/// Whether one of the desktop's settings files changed since the worker last read the theme.
/// Only older desktops need those files, so on others nothing follows them.
static ARE_FILES_CHANGED: AtomicBool = AtomicBool::new(false);
/// Wakes the worker, for when the settings files change.
static WORKER: Mutex<Option<Waker>> = Mutex::new(None);

pub(crate) fn query() -> Result<Option<Theme>> {
    // The worker keeps the latest theme current, so the bus isn't asked again.
    if IS_LATEST_CURRENT.load(Ordering::Acquire) {
        if let Some(theme) = crate::latest() {
            return Ok(theme);
        }
    }
    let (sender, receiver) = mpsc::channel();
    spawn("robius-theme-query", move || {
        let _ = sender.send(zbus::block_on(read_once()));
    })?;
    // zbus has no time limit for connecting, so a little extra covers that.
    let theme = receiver.recv_timeout(QUERY_TIMEOUT + Duration::from_millis(250));
    theme.unwrap_or_else(|_| Err(Error::Unavailable("the session bus did not answer in time".into())))
}

pub(crate) fn read_now() -> Option<Option<Theme>> {
    // The worker gives subscribers the first theme once the portal has answered.
    None
}

pub(crate) fn start() -> Result<()> {
    if IS_RUNNING.swap(true, Ordering::AcqRel) {
        return Ok(());
    }
    let run = || {
        // Even a panic inside zbus must let a later `start` try again.
        let _ = catch_unwind(work);
        IS_LATEST_CURRENT.store(false, Ordering::Release);
        IS_RUNNING.store(false, Ordering::Release);
    };
    spawn("robius-theme", run).inspect_err(|_| IS_RUNNING.store(false, Ordering::Release))
}

fn spawn(name: &str, run: impl FnOnce() + Send + 'static) -> Result<()> {
    let thread = thread::Builder::new().name(name.into()).spawn(run);
    thread.map(drop).map_err(|error| Error::Unavailable(format!("could not start a thread: {error}")))
}

/// Called from the thread that follows the desktop's settings files, when one of them changed.
fn files_changed() {
    if IS_RUNNING.load(Ordering::Acquire) {
        // The worker reads the theme again, starting with the portal.
        ARE_FILES_CHANGED.store(true, Ordering::Release);
        if let Some(worker) = WORKER.lock().unwrap_or_else(PoisonError::into_inner).take() {
            worker.wake();
        }
    } else {
        // Without a session bus, there are only the files.
        crate::refresh(|| Desktop::current().theme());
    }
}

/// Runs on the worker thread until the session bus goes away.
fn work() {
    let Ok(mut portal) = zbus::block_on(Portal::subscribe()) else {
        // Without a session bus, subscribers get the theme from the desktop's own settings.
        desktop::follow_files(files_changed);
        crate::refresh(|| Desktop::current().theme());
        return;
    };
    // Subscribers are called here, between two `block_on` calls.
    while let Some(theme) = zbus::block_on(portal.next_theme()) {
        crate::refresh(|| {
            // Set before subscribers are called, so that a `query` from one of them is answered at once.
            IS_LATEST_CURRENT.store(true, Ordering::Release);
            theme
        });
    }
}

/// Reads the theme once over a connection of its own, for `query`.
async fn read_once() -> Result<Option<Theme>> {
    match connect(Some(QUERY_TIMEOUT)).await {
        Ok(conn) => drive(&conn, read(conn.clone(), false)).await,
        // Without a session bus, there are only the desktop's own settings.
        Err(_) => Ok(Desktop::current().theme()),
    }
}

/// Connects to the session bus. zbus starts no thread for the connection: `drive` runs it.
async fn connect(method_timeout: Option<Duration>) -> zbus::Result<Connection> {
    let mut builder = Builder::session()?.internal_executor(false);
    if let Some(timeout) = method_timeout {
        builder = builder.method_timeout(timeout);
    }
    builder.build().await
}

/// Awaits `future` while running the connection's own tasks, such as reading its socket.
async fn drive<F: Future>(conn: &Connection, future: F) -> F::Output {
    let mut future = pin!(future);
    let mut tasks = pin!(async {
        loop {
            conn.executor().tick().await;
        }
    });
    poll_fn(|cx| {
        let _ = tasks.as_mut().poll(cx);
        future.as_mut().poll(cx)
    })
    .await
}

/// Reads the portal's `color-scheme`, or else the theme that the desktop's own settings name.
/// With `is_following`, it also starts following those other settings, the first time they're needed.
async fn read(conn: Connection, is_following: bool) -> Result<Option<Theme>> {
    let desktop = Desktop::current();
    match read_setting(&conn, COLOR_SCHEME).await? {
        Some(Setting::Number(1)) => return Ok(Some(Theme::Dark)),
        Some(Setting::Number(2)) => return Ok(Some(Theme::Light)),
        // Those desktops keep this setting current, so no preference is their answer.
        Some(_) if desktop.is_color_scheme_kept() => return Ok(None),
        // An older desktop: no such setting, or one that this desktop doesn't keep current.
        _ => {}
    }
    if let Some((namespace, key)) = desktop.portal_setting() {
        if is_following {
            add_match(&conn, &setting_changed(namespace, key)).await?;
        }
        if let Some(Setting::Text(text)) = read_setting(&conn, (namespace, key)).await? {
            if let Some(theme) = desktop::theme_of(&text) {
                return Ok(Some(theme));
            }
        }
    }
    if is_following {
        desktop::follow_files(files_changed);
    }
    Ok(desktop.theme())
}

/// A rule for the bus to pass on changes of one setting, and only from the portal.
fn setting_changed(namespace: &str, key: &str) -> String {
    format!(
        "type='signal',sender='{PORTAL}',path='{PORTAL_PATH}',interface='{SETTINGS}',\
        member='SettingChanged',arg0='{namespace}',arg1='{key}'"
    )
}

/// Asks the bus to pass on the messages that `rule` describes.
async fn add_match(conn: &Connection, rule: &str) -> Result<()> {
    let reply = conn.call_method(Some(BUS), "/org/freedesktop/DBus", Some(BUS), "AddMatch", &rule).await;
    reply.map(drop).map_err(|error| Error::Unavailable(format!("could not subscribe to the desktop portal: {error}")))
}

/// A setting's value: `color-scheme` is a number, and the older settings are text.
enum Setting {
    Number(u32),
    Text(String),
}

/// Reads one setting with `ReadOne`, or with `Read` from a portal too old to have that.
async fn read_setting(conn: &Connection, setting: (&str, &str)) -> Result<Option<Setting>> {
    for method in ["ReadOne", "Read"] {
        let reply = conn.call_method(Some(PORTAL), PORTAL_PATH, Some(SETTINGS), method, &setting).await;
        let error = match reply {
            Ok(reply) => return setting_in(&reply).map(Some),
            Err(error) => error,
        };
        let name = match &error {
            zbus::Error::MethodError(name, ..) => name.as_str(),
            _ => "",
        };
        match name {
            "org.freedesktop.DBus.Error.UnknownMethod" => {}
            // No portal, or no such setting in it.
            "org.freedesktop.DBus.Error.ServiceUnknown"
            | "org.freedesktop.DBus.Error.NameHasNoOwner"
            | "org.freedesktop.DBus.Error.UnknownInterface"
            | "org.freedesktop.DBus.Error.UnknownObject"
            | "org.freedesktop.portal.Error.NotFound" => return Ok(None),
            _ => return Err(Error::Unavailable(format!("could not read the desktop portal's settings: {error}"))),
        }
    }
    // A portal with neither method has no settings at all.
    Ok(None)
}

/// The setting that a reply from the portal holds.
fn setting_in(reply: &Message) -> Result<Setting> {
    let body = reply.body();
    // `Read` wraps the value in one more variant than `ReadOne` does.
    let mut value = body.deserialize::<Value<'_>>().ok();
    while let Some(Value::Value(inner)) = value {
        value = Some(*inner);
    }
    match value {
        Some(Value::U32(number)) => Ok(Setting::Number(number)),
        Some(Value::Str(text)) => Ok(Setting::Text(text.as_str().to_owned())),
        _ => Err(Error::Unavailable("the desktop portal's answer was not a theme setting".into())),
    }
}

/// A call to `read` that hasn't finished yet.
type Reading = Pin<Box<dyn Future<Output = Result<Option<Theme>>>>>;

/// The worker's connection to the session bus, subscribed to the portal's changes.
struct Portal {
    conn: Connection,
    /// Everything that the bus sends us.
    messages: MessageStream,
    reading: Option<Reading>,
    /// Whether to read the setting (again) once no read is in flight.
    is_stale: bool,
}

enum Event {
    /// The portal or a settings file says that the theme may have changed.
    Changed,
    /// The portal started, or another one replaced it.
    PortalStarted,
    Read(Result<Option<Theme>>),
    /// The session bus is gone.
    Closed,
}

impl Portal {
    async fn subscribe() -> zbus::Result<Self> {
        let conn = connect(None).await?;
        // This exists before the bus is asked for anything, so no change can be missed.
        let messages = MessageStream::from(&conn);
        for rule in [setting_changed(COLOR_SCHEME.0, COLOR_SCHEME.1), PORTAL_OWNER_CHANGED.to_owned()] {
            let add_match = conn.call_method(Some(BUS), "/org/freedesktop/DBus", Some(BUS), "AddMatch", &rule);
            drive(&conn, add_match).await?;
        }
        Ok(Self { conn, messages, reading: None, is_stale: true })
    }

    /// The next theme to give to subscribers, or `None` once the session bus is gone.
    async fn next_theme(&mut self) -> Option<Option<Theme>> {
        loop {
            if self.is_stale && self.reading.is_none() {
                self.is_stale = false;
                self.reading = Some(Box::pin(read(self.conn.clone(), true)));
            }
            match self.next_event().await {
                Event::Changed => self.is_stale = true,
                Event::PortalStarted => {
                    // The portal that was asked may never answer now.
                    self.reading = None;
                    self.is_stale = true;
                }
                Event::Read(theme) => {
                    self.reading = None;
                    match theme {
                        // The setting changed during the read, so this answer may be old.
                        _ if self.is_stale => {}
                        Ok(theme) => return Some(theme),
                        // Nothing was learned, so subscribers keep the theme they have.
                        Err(_) => {}
                    }
                }
                Event::Closed => return None,
            }
        }
    }

    async fn next_event(&mut self) -> Event {
        let Self { conn, messages, reading, .. } = self;
        let event = poll_fn(|cx| {
            if desktop::is_following_files() {
                *WORKER.lock().unwrap_or_else(PoisonError::into_inner) = Some(cx.waker().clone());
                if ARE_FILES_CHANGED.swap(false, Ordering::AcqRel) {
                    return Poll::Ready(Event::Changed);
                }
            }
            // zbus stops reading the socket once 64 messages wait here, so take them all.
            loop {
                match Pin::new(&mut *messages).poll_next(cx) {
                    Poll::Ready(Some(Ok(message))) => {
                        if let Some(event) = signal(&message) {
                            return Poll::Ready(event);
                        }
                    }
                    Poll::Ready(_) => return Poll::Ready(Event::Closed),
                    Poll::Pending => break,
                }
            }
            match reading {
                Some(reading) => reading.as_mut().poll(cx).map(Event::Read),
                None => Poll::Pending,
            }
        });
        drive(conn, event).await
    }
}

/// What a message from the bus means for the theme, if it's one of the two signals.
fn signal(message: &Message) -> Option<Event> {
    if message.message_type() != zbus::message::Type::Signal {
        return None;
    }
    match message.header().member()?.as_str() {
        // The setting is read again, as portals also pass on values that aren't in effect.
        "SettingChanged" => Some(Event::Changed),
        "NameOwnerChanged" => {
            // An empty new owner means that the portal exited, which changes nothing.
            let owners = message.body();
            let is_started = owners.deserialize::<(&str, &str, &str)>().is_ok_and(|(_, _, new)| !new.is_empty());
            is_started.then_some(Event::PortalStarted)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reply from the portal that holds `value`.
    fn reply(value: Value<'_>) -> Message {
        let call = Message::method_call(PORTAL_PATH, "ReadOne").unwrap().build(&()).unwrap();
        Message::method_return(&call.header()).unwrap().build(&value).unwrap()
    }

    #[test]
    fn a_reply_holds_a_number_or_text() {
        let setting = |value| setting_in(&reply(value));
        assert!(matches!(setting(Value::U32(1)), Ok(Setting::Number(1))));
        assert!(matches!(setting(Value::Value(Box::new(Value::U32(2)))), Ok(Setting::Number(2))));
        assert!(matches!(setting(Value::from("Yaru-dark")), Ok(Setting::Text(text)) if text == "Yaru-dark"));
        assert!(matches!(setting(Value::Value(Box::new(Value::from("Adwaita")))), Ok(Setting::Text(text)) if text == "Adwaita"));
        assert!(setting(Value::Bool(true)).is_err());
    }

    #[test]
    fn a_rule_names_its_setting_and_the_portal() {
        assert_eq!(
            setting_changed("org.freedesktop.appearance", "color-scheme"),
            "type='signal',sender='org.freedesktop.portal.Desktop',path='/org/freedesktop/portal/desktop',\
            interface='org.freedesktop.portal.Settings',member='SettingChanged',\
            arg0='org.freedesktop.appearance',arg1='color-scheme'"
        );
    }
}
