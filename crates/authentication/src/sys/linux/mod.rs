use std::collections::HashMap;
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

pub(crate) fn get_system_connection() -> Result<Connection> {
    Connection::system().map_err(|_| Error::Unavailable)
}

pub(crate) fn get_authority_proxy(
    conn: &Connection,
) -> Result<AuthorityProxyBlocking<'_>> {
    AuthorityProxyBlocking::new(conn).map_err(|_| Error::Unavailable)
}

fn do_polkit_check(action_id: &str) -> Result<()> {
    // Create a fresh system connection and proxy per request to avoid stale handles
    // after sleep/resume or bus restarts.
    let conn = get_system_connection()?;
    let auth = get_authority_proxy(&conn)?;
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
