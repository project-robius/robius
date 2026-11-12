//! Prints the system theme, and then each change until you press Ctrl+C.

fn main() -> robius_theme::Result<()> {
    println!("Right now: {:?}", robius_theme::query()?);
    let _subscription = robius_theme::subscribe(|theme| println!("Subscribed: {theme:?}"))?;
    run();
    Ok(())
}

/// macOS only reports changes to an app that is running its event loop.
#[cfg(target_os = "macos")]
fn run() {
    use objc2::{class, msg_send, runtime::AnyObject};

    #[link(name = "AppKit", kind = "framework")]
    extern "C" {}

    // SAFETY: this is the main thread, and both messages take no arguments.
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![app, run];
    }
}

#[cfg(not(target_os = "macos"))]
fn run() {
    loop {
        std::thread::park();
    }
}
