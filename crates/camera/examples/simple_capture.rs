//! Simple example demonstrating how to capture a photo using the system camera.
//!
//! Run with: `cargo run --example simple_capture`
//!
//! **Note for Windows:** This example may not work from a console app because
//! `CameraCaptureUI` is a UWP API that requires a proper windowed app context.
//! To test on Windows, integrate into a GUI app like Makepad instead.

use robius_camera::{capture_photo, is_available, CameraPosition};
use std::sync::mpsc;

fn main() {
    // Check if camera is available on this device
    if !is_available() {
        eprintln!("Camera is not available on this device");
        return;
    }

    println!("Camera is available, opening capture UI...");

    // Use a channel to wait for the async callback
    let (tx, rx) = mpsc::channel();

    // Capture a photo using the back camera
    let result = capture_photo(CameraPosition::Back, move |result| {
        tx.send(result).expect("failed to send result");
    });

    if let Err(e) = result {
        eprintln!("Failed to open camera: {:?}", e);
        return;
    }

    // Wait for the capture result
    match rx.recv() {
        Ok(Ok(photo)) => {
            println!(
                "Photo captured successfully: {}x{} ({} bytes)",
                photo.width(),
                photo.height(),
                photo.jpeg_data().len()
            );

            // Optionally save the photo to a file
            #[cfg(not(target_os = "android"))]
            {
                let path = "captured_photo.jpg";
                if let Err(e) = std::fs::write(path, photo.jpeg_data()) {
                    eprintln!("Failed to save photo: {}", e);
                } else {
                    println!("Photo saved to {}", path);
                }
            }
        }
        Ok(Err(robius_camera::Error::Cancelled)) => {
            println!("User cancelled the capture");
        }
        Ok(Err(e)) => {
            eprintln!("Capture error: {:?}", e);
        }
        Err(e) => {
            eprintln!("Failed to receive result: {:?}", e);
        }
    }
}
