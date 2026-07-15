//! # Recommended usage
//!
//! `Context::authenticate()` is asynchronous: it returns immediately after *starting*
//! authentication and delivers the final result via the callback.
//!
//! **In GUI applications**, you typically do not need any additional synchronization.
//! The application's event loop keeps the process alive while the user interacts with
//! the authentication prompt.
//!
//! ```no_run
//! let context = Context::new(());
//! context.authenticate(TEXT, &POLICY, |result| {
//!     match result {
//!         Ok(()) => println!("Authentication successful"),
//!         Err(e) => eprintln!("Authentication failed: {:?}", e),
//!     }
//! });
//! ```
//!
//! # Why this example uses `mpsc::channel()`
//!
//! This file is a CLI-style demo. Without an event loop, `main()` would exit before the
//! authentication completes (especially on Linux where the polkit check runs in the
//! background). We therefore use an `mpsc::channel()` to wait for the callback.
//!
//! # Linux notes
//!
//! - Ensure the polkit policy file is installed for the chosen `action_id`
//!   (see README: "Usage on Linux").
//! - Ensure a polkit authentication agent is running in the current desktop session,
//!   otherwise no prompt will appear and the request will fail with `NoAgent`/`Unavailable`.

use std::sync::mpsc;

use robius_authentication::{
    AndroidText, BiometricStrength, Context, PolicyBuilder, Text, WindowsText,
};

const TEXT: Text = Text {
    android: AndroidText {
        title: "Title",
        subtitle: None,
        description: None,
    },
    apple: "authenticate",
    windows: match WindowsText::new("Title", "Description") {
        Some(text) => text,
        None => panic!("Windows text too long"),
    },
};

fn main() {
    let context = Context::new(());
    let mut policy = PolicyBuilder::new()
        // On Linux You need to set action_ids.
        // See: ./org.robius.authentication.policy file settings and (README: "Usage on Linux").
        .action_ids([
            "org.robius.authentication",
            "org.robius.authentication.settings",
        ])
        .biometrics(Some(BiometricStrength::Strong))
        .password(true)
        .companion(true)
        .build()
        .unwrap();

    if let Err(e) = policy.set_action_id("org.robius.authentication.settings") {
        eprintln!("Invalid action_id: {:?}", e);
        return;
    }

    let (tx, rx) = mpsc::channel();

    // Start authentication. `res` only indicates whether the request was successfully
    // initiated; the final result is delivered via the callback.
    let res = context.authenticate(TEXT, &policy, move |result| {
        let _ = tx.send(result);
    });

    if let Err(e) = res {
        eprintln!("Failed to start authentication: {:?}", e);
        return;
    }

    // Block until the callback produces a result.
    match rx.recv() {
        Ok(Ok(_)) => println!("Authentication successful"),
        Ok(Err(e)) => println!("Authentication failed: {:?}", e),
        Err(e) => eprintln!("Failed to receive auth result: {:?}", e),
    }
}
