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
        let action_id = policy.action_id;
        let res = do_polkit_check(action_id);
        callback(res);
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
        AuthorityProxyBlocking::new(&conn).map_err(|e| {
            let _ = e;
            Error::Unavailable
        })
    });

    match r {
        Ok(p) => Ok(p.clone()),
        Err(e) => Err(e.clone()),
    }
}

fn do_polkit_check(action_id: &'static str) -> Result<()> {
    // Get authority proxy (and cache it for future usage).
    let auth = get_authority_proxy()?;

    let pid = std::process::id() as u32;
    let subject = Subject::new_for_owner(pid, None, None)
        .map_err(|_| Error::Unavailable)?;

    let mut details = HashMap::new();
    details.insert("polkit.message", "Hello");


    // If details is non-empty then the request will fail with POLKIT_ERROR_FAILED unless the process doing the check itsef is sufficiently authorized (e.g. running as uid 0).
    let result: AuthorizationResult = auth
        .check_authorization(
            &subject,
            action_id,
            &details,
            CheckAuthorizationFlags::AllowUserInteraction.into(),
            "",
        )
        .map_err(|err| match err {
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
        })?;

    if result.is_authorized {
        return Ok(());
    }

    if result.details.keys().any(|k| k.contains("dismissed")) {
        return Err(Error::UserCanceled);
    }

    if result.is_challenge {
        // No agent available or UI interaction not possible even though we requested it.
        return Err(Error::Unavailable);
    }

    Err(Error::Authentication)
}

#[derive(Debug, Clone)]
pub struct Policy {
    pub(crate) action_id: &'static str,
}

#[derive(Debug, Clone)]
pub struct PolicyBuilder {
    action_id: Option<&'static str>,
}

impl PolicyBuilder {
    #[inline]
    pub const fn new() -> Self {
        Self { action_id: None }
    }

    #[inline]
    pub const fn action_id(self, id: &'static str) -> Self {
        Self { action_id: Some(id) }
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
    pub const fn build(self) -> Option<Policy> {
        match self.action_id {
            Some(id) => Some(Policy { action_id: id }),
            None => Some(Policy { action_id: "Use" }),
        }
    }
}
