//! This module is not public yet because it is a work in progress.

use polkit::{Authority, CheckAuthorizationFlags, Details, UnixProcess};

use crate::{BiometricStrength, Result};

#[derive(Debug)]
pub struct Policy;

#[derive(Debug)]
pub(crate) struct PolicyBuilder;

impl PolicyBuilder {
    pub(crate) const fn new() -> Self {
        Self
    }

    pub(crate) const fn biometrics(self, _: Option<BiometricStrength>) -> Self {
        Self
    }

    pub(crate) const fn password(self, _: bool) -> Self {
        Self
    }

    pub(crate) const fn companion(self, _: bool) -> Self {
        Self
    }

    pub(crate) const fn wrist_detection(self, _: bool) -> Self {
        Self
    }

    pub(crate) const fn build(self) -> Option<Policy> {
        Some(Policy)
    }
}

pub(crate) async fn authenticate_async(_message: &str, _: &Policy) -> Result<()> {
    unimplemented!()
}

pub(crate) fn blocking_authenticate(_message: &str, _: &Policy) -> Result<()> {
    // TODO: None?
    let authority = Authority::sync(Option::<&gio::Cancellable>::None).unwrap();

    let current_user = "klim";

    let details = Details::new();
    details.insert("user", Some(current_user));
    // TODO: user.gecos
    details.insert("user.display", Some(current_user));
    // TODO: program
    // TODO: command_line
    details.insert("polkit.message", Some("Testing robius authentication"));
    // TODO: polkit.gettext_domain

    // for action in authority
    //     .enumerate_actions_sync(Option::<&gio::Cancellable>::None)
    //     .unwrap()
    // {
    //     println!("-- action --");
    //     println!("id: {}", action.action_id());
    //     println!("description: {}", action.description());
    //     println!(
    //         "allow_gui: {:?}",
    //         action.annotation("org.freedesktop.policykit.exec.allow_gui")
    //     );
    // }

    let subject = UnixProcess::new(std::process::id() as i32);
    authority
        .check_authorization_sync(
            &subject,
            "org.hello-world.authenticate",
            Some(&details),
            // None,
            CheckAuthorizationFlags::ALLOW_USER_INTERACTION,
            Option::<&gio::Cancellable>::None,
        )
        .unwrap();

    Ok(())
}
