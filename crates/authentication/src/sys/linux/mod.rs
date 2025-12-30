use std::{collections::HashMap, sync::OnceLock};
use zbus::{blocking::Connection, fdo, Error as ZbusError};
use zbus_polkit::policykit1::{AuthorityProxyBlocking, AuthorizationResult, CheckAuthorizationFlags, Subject};

use crate::{Error, Result, Text};

pub(crate) type RawContext = ();

#[derive(Debug)]
pub(crate) struct Context;

impl Context {
    #[inline]
    pub(crate) fn new(_: RawContext) -> Self {
        Self
    }

    pub(crate) fn authenticate<F>(
        &self,
        _message: Text,
        policy: &Policy,
        callback: F,
    ) -> Result<()>
    where
        F: Fn(Result<()>) + Send + 'static,
    {
        let action_id = policy.action_id.clone();
        // CheckAuthorization may block (D-Bus round-trips and potential user interaction).
        // Run it off the caller thread to avoid blocking UI/event-loop threads.
        std::thread::Builder::new()
            .name("robius-authentication-polkit".into())
            .spawn(move || {
                let res = do_polkit_check(&action_id);
                callback(res);
            })
            .map_err(|_| Error::Unavailable)?;
        Ok(())
    }
}

static SYSTEM_CONNECTION: OnceLock<Result<Connection>> = OnceLock::new(); // Cached system bus connection
static AUTHORITY_PROXY: OnceLock<Result<AuthorityProxyBlocking<'static>>> = OnceLock::new(); // Cached authority proxy

pub(crate) fn get_system_connection() -> Result<Connection> {
    let r: &Result<Connection> = SYSTEM_CONNECTION.get_or_init(|| {
        Connection::system()
            .map_err(|_| Error::Unavailable)
    });

    match r {
        Ok(conn) => Ok(conn.clone()),
        Err(e) => Err(e.clone()),
    }
}

pub(crate) fn get_authority_proxy() -> Result<AuthorityProxyBlocking<'static>> {
    let r: &Result<AuthorityProxyBlocking<'static>> = AUTHORITY_PROXY.get_or_init(|| {
        let conn = get_system_connection()?;
        AuthorityProxyBlocking::new(&conn).map_err(|_| Error::Unavailable)
    });

    match r {
        Ok(p) => Ok(p.clone()),
        Err(e) => Err(e.clone()),
    }
}

fn do_polkit_check(action_id: &str) -> Result<()> {
    // Get authority proxy (and cache it for future usage).
    let auth = get_authority_proxy()?;
    let details = HashMap::new();

    // Use a unix-process subject including pid start-time and real uid.
    // This avoids pid reuse ambiguity and matches polkit's recommended subject format.
    let subject = Subject::new_for_owner(std::process::id(), None, None)
        .map_err(|_| Error::Unavailable)?;

    // If details is non-empty then the request will fail with POLKIT_ERROR_FAILED
    // unless the process doing the check itsef is sufficiently authorized (e.g. running as uid 0).
    let result: AuthorizationResult = auth
        .check_authorization(
            &subject,
            action_id,
            &details,
            CheckAuthorizationFlags::AllowUserInteraction.into(),
            "",
        )
        .map_err(|err| {
            eprintln!("polkit check_authorization error: {:?}", err);
            match err {
                ZbusError::MethodError(name, _, _) => match name.as_str() {
                    "org.freedesktop.PolicyKit1.Error.Cancelled" => Error::UserCanceled,
                    "org.freedesktop.PolicyKit1.Error.NotAuthorized" => Error::Authentication,
                    "org.freedesktop.PolicyKit1.Error.NotSupported" => Error::Unavailable,
                    "org.freedesktop.PolicyKit1.Error.NoAgent" => Error::Unavailable,
                    _ => Error::Authentication,
                },
                ZbusError::FDO(fdo_err) => match *fdo_err {
                    fdo::Error::TimedOut(_) | fdo::Error::NoReply(_) => Error::Unavailable,
                    _ => Error::Authentication,
                },
                ZbusError::InputOutput(_) => Error::Unavailable,
                _ => Error::Authentication,
            }
        })?;

    if result.is_authorized {
        return Ok(());
    }

    if result
        .details
        .get("polkit.dismissed")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
    {
        return Err(Error::UserCanceled);
    }


    // If we're not authorized, treat as authentication failure.
    // "NoAgent" is handled as an error (org.freedesktop.PolicyKit1.Error.NoAgent).
    Err(Error::Authentication)
}

#[derive(Debug)]
pub struct Policy {
    pub(crate) action_id: String,
    allowed_action_ids: Vec<String>,
}

impl Policy {
    #[inline]
    pub(crate) fn set_action_id(&mut self, id: String) -> Result<()> {
        if self.allowed_action_ids.iter().any(|allowed| allowed == &id) {
            self.action_id = id;
            Ok(())
        } else {
            Err(Error::InvalidActionId)
        }
    }
}

#[derive(Debug)]
pub(crate) struct PolicyBuilder {
    action_ids: Option<Vec<String>>,
}

impl PolicyBuilder {
    #[inline]
    pub const fn new() -> Self {
        Self { action_ids: None }
    }

    #[inline]
    pub fn action_ids(self, ids: Vec<String>) -> Self {
        Self { action_ids: Some(ids) }
    }

    // The following are no-ops on Linux but kept for cross-platform API.
    #[inline]
    pub const fn biometrics(self, _: Option<crate::BiometricStrength>) -> Self { self }
    #[inline]
    pub const fn password(self, _: bool) -> Self { self }
    #[inline]
    pub const fn companion(self, _: bool) -> Self { self }
    #[inline]
    pub const fn wrist_detection(self, _: bool) -> Self { self }

    #[inline]
    pub fn build(self) -> Option<Policy> {
        let action_ids = self.action_ids?;
        if action_ids.is_empty() {
            return None;
        }
        let action_id = action_ids[0].clone();
        Some(Policy {
            action_id,
            allowed_action_ids: action_ids,
        })
    }
}
