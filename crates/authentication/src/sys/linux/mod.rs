use std::{collections::HashMap, sync::OnceLock};
use std::thread;
use zbus::blocking::Connection;
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
        _message: Text, // _message is unused on Linux, see reasons below:  auth.check_authorization();
        policy: &Policy,
        callback: F,
    ) -> Result<()>
    where
        F: Fn(Result<()>) + Send + 'static,
    {
        let action_id = policy.action_id;
        // Here, we perform a simple and direct thread creation because the current calls are infrequent.
        thread::Builder::new()
            .name(String::from("robius-authentication-polkit"))
            .spawn(move || {
                let res = do_polkit_check(action_id);
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

    // If details is non-empty then the request will fail with POLKIT_ERROR_FAILED unless the process doing the check itsef is sufficiently authorized (e.g. running as uid 0).
    let result = auth
        .check_authorization(
            &subject,
            action_id,
            &HashMap::new(), 
            CheckAuthorizationFlags::AllowUserInteraction.into(),
            "",
        )
        .map_err(|e| map_polkit_error(e.to_string()))?;

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

fn map_polkit_error(msg: String) -> Error {
    let m = msg.to_lowercase();
    if m.contains("cancel") || m.contains("canceled") || m.contains("cancelled") {
        Error::UserCanceled
    } else if m.contains("locked") || m.contains("exhaust") || m.contains("too many") {
        Error::Exhausted
    } else if m.contains("unavailable") || m.contains("no agent") || m.contains("not supported") {
        Error::Unavailable
    } else {
        Error::Authentication
    }
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

    /// Required on Linux: caller must provide a polkit action id.
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
            None => None,
        }
    }
}