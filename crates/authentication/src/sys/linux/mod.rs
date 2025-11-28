use std::collections::HashMap;
use std::thread;
use zbus::blocking::Connection;
use zbus_polkit::policykit1::{AuthorityProxyBlocking, CheckAuthorizationFlags, Subject};

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

        thread::spawn(move || {
            let res = do_polkit_check(action_id);
            callback(res);
        });
        Ok(())
    }
}

fn do_polkit_check(action_id: &'static str) -> Result<()> {
    // 1) system bus (blocking)
    let conn = Connection::system().map_err(|_| Error::Unavailable)?;

    // 2) polkit authority proxy (blocking)
    let auth = AuthorityProxyBlocking::new(&conn).map_err(|_| Error::Unavailable)?;

    // 3) Subject: current process owner
    let pid = std::process::id() as u32;
    let subject = Subject::new_for_owner(pid, None, None)
        .map_err(|_| Error::Unavailable)?;

    // 4) check authorization, allow interaction => system prompt
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
        Ok(())
    } else {
        Err(Error::Authentication)
    }
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

const DEFAULT_ACTION_ID: &str = "com.yourapp.authenticate";

/// Authentication policy on Linux.
/// Only action id matters.
#[derive(Debug, Clone)]
pub struct Policy {
    pub(crate) action_id: &'static str,
}

/// Policy builder for Linux.
/// polkit doesn't really understand "biometrics/password/companion" flags from mobile,
/// so we keep them for API consistency but only store an action id.
#[derive(Debug, Clone)]
pub struct PolicyBuilder {
    action_id: Option<&'static str>,
}

impl PolicyBuilder {
    #[inline]
    pub const fn new() -> Self {
        Self { action_id: None }
    }

    /// Optional: allow caller to override polkit action id.
    #[inline]
    pub const fn action_id(self, id: &'static str) -> Self {
        Self { action_id: Some(id) }
    }

    // The following are no-ops on Linux but kept for cross-platform API.
    #[inline]
    pub const fn biometrics(self, _strength: Option<crate::BiometricStrength>) -> Self {
        self
    }
    #[inline]
    pub const fn password(self, _password: bool) -> Self {
        self
    }
    #[inline]
    pub const fn companion(self, _companion: bool) -> Self {
        self
    }
    #[inline]
    pub const fn wrist_detection(self, _wrist: bool) -> Self {
        self
    }

    #[inline]
    pub const fn build(self) -> Option<Policy> {
        Some(Policy {
            action_id: match self.action_id {
                Some(id) => id,
                None => DEFAULT_ACTION_ID,
            },
        })
    }
}