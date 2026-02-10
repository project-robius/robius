//! Windows implementation using CameraCaptureUI.
//!
//! This uses the Windows built-in camera UI which provides a consistent
//! user experience across Windows devices.

use std::thread;

use windows::{
    core::Interface,
    Foundation::IAsyncOperation,
    Graphics::Imaging::BitmapDecoder,
    Media::Capture::{CameraCaptureUI, CameraCaptureUIMode, CameraCaptureUIPhotoFormat},
    Storage::{FileAccessMode, StorageFile},
    Win32::UI::Shell::IInitializeWithWindow,
    Win32::UI::WindowsAndMessaging::GetForegroundWindow,
};

use crate::{CameraPosition, Error, PhotoData, Result};

/// Opens the system camera UI to capture a photo.
pub(crate) fn capture_photo<F>(position: CameraPosition, callback: F) -> Result<()>
where
    F: FnOnce(Result<PhotoData>) + Send + 'static,
{
    // CameraCaptureUI doesn't support selecting front/back camera directly.
    // The user can switch cameras within the UI if multiple cameras are available.
    // We log this limitation but proceed with the capture.
    if position == CameraPosition::Front {
        #[cfg(feature = "log")]
        log::info!(
            "CameraCaptureUI doesn't support programmatic camera selection. \
             User can switch cameras within the UI."
        );
    }

    // Spawn a thread to handle the async Windows API
    // The Windows camera UI must be run from an STA thread
    thread::spawn(move || {
        let result = capture_photo_sync();
        callback(result);
    });

    Ok(())
}

/// Synchronous implementation of photo capture.
fn capture_photo_sync() -> Result<PhotoData> {
    // Initialize COM for this thread (needed for WinRT)
    // CameraCaptureUI requires single-threaded apartment (STA)
    unsafe {
        windows::Win32::System::Com::CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
        )
        .ok()
        .map_err(|_| Error::Unknown)?;
    }

    let result = capture_photo_impl();

    // Uninitialize COM
    unsafe {
        windows::Win32::System::Com::CoUninitialize();
    }

    result
}

/// The actual capture implementation.
fn capture_photo_impl() -> Result<PhotoData> {
    // Create the camera capture UI
    let capture_ui = CameraCaptureUI::new().map_err(|e| {
        eprintln!("Failed to create CameraCaptureUI: {:?}", e);
        eprintln!("HRESULT: {:?}", e.code());
        Error::CameraUnavailable
    })?;
    eprintln!("CameraCaptureUI created successfully");

    // Try to initialize with the foreground window
    // This allows the UWP dialog to be associated with a Win32 window
    unsafe {
        let hwnd = GetForegroundWindow();
        eprintln!("Foreground window HWND: {:?}", hwnd);

        if hwnd.0 != 0 {
            match capture_ui.cast::<IInitializeWithWindow>() {
                Ok(init_window) => {
                    match init_window.Initialize(hwnd) {
                        Ok(()) => eprintln!("IInitializeWithWindow succeeded"),
                        Err(e) => eprintln!("IInitializeWithWindow.Initialize failed: {:?}", e),
                    }
                }
                Err(e) => {
                    eprintln!("CameraCaptureUI doesn't support IInitializeWithWindow: {:?}", e);
                    // Continue anyway - might work without it
                }
            }
        }
    }

    // Configure photo settings
    let photo_settings = capture_ui.PhotoSettings().map_err(|_e| {
        #[cfg(feature = "log")]
        log::error!("Failed to get photo settings: {:?}", _e);
        Error::Unknown
    })?;

    // Set format to JPEG
    photo_settings
        .SetFormat(CameraCaptureUIPhotoFormat::Jpeg)
        .map_err(|_e| {
            #[cfg(feature = "log")]
            log::error!("Failed to set photo format: {:?}", _e);
            Error::Unknown
        })?;

    // Launch the capture UI and wait for result
    eprintln!("Starting CaptureFileAsync...");
    let async_op: IAsyncOperation<StorageFile> = capture_ui
        .CaptureFileAsync(CameraCaptureUIMode::Photo)
        .map_err(|e| {
            eprintln!("Failed to start capture: {:?}", e);
            eprintln!("HRESULT: {:?}", e.code());
            Error::CameraUnavailable
        })?;
    eprintln!("CaptureFileAsync started, waiting for result...");

    // Wait for the async operation to complete
    // When user cancels, CameraCaptureUI returns null which windows-rs treats as an error
    // with S_OK (0x00000000). We detect this and return Cancelled.
    let file: StorageFile = match async_op.get() {
        Ok(f) => f,
        Err(e) => {
            eprintln!("Capture operation result: {:?}", e);
            eprintln!("HRESULT: {:?}", e.code());
            // S_OK with "error" means null result = user cancelled
            if e.code().0 == 0 {
                return Err(Error::Cancelled);
            }
            return Err(Error::Unknown);
        }
    };

    // Check if user cancelled (file will be null/empty path)
    let path = file.Path().map_err(|_| Error::Cancelled)?;
    if path.is_empty() {
        return Err(Error::Cancelled);
    }

    #[cfg(feature = "log")]
    log::debug!("Captured photo to: {}", path);

    // Read the file and get dimensions
    read_photo_from_file(&file)
}

/// Read photo data from the captured StorageFile.
fn read_photo_from_file(file: &StorageFile) -> Result<PhotoData> {
    // Open the file for reading
    let stream = file
        .OpenAsync(FileAccessMode::Read)
        .map_err(|_e| {
            #[cfg(feature = "log")]
            log::error!("Failed to open file: {:?}", _e);
            Error::ProcessingFailed
        })?
        .get()
        .map_err(|_e| {
            #[cfg(feature = "log")]
            log::error!("Failed to get stream: {:?}", _e);
            Error::ProcessingFailed
        })?;

    // Create a decoder to get image dimensions
    let decoder = BitmapDecoder::CreateAsync(&stream)
        .map_err(|_e| {
            #[cfg(feature = "log")]
            log::error!("Failed to create decoder: {:?}", _e);
            Error::ProcessingFailed
        })?
        .get()
        .map_err(|_e| {
            #[cfg(feature = "log")]
            log::error!("Failed to decode: {:?}", _e);
            Error::ProcessingFailed
        })?;

    let width = decoder.PixelWidth().unwrap_or(0);
    let height = decoder.PixelHeight().unwrap_or(0);

    // Read the file contents as bytes
    let size = stream.Size().map_err(|_| Error::ProcessingFailed)?;

    // Seek back to the beginning
    stream
        .Seek(0)
        .map_err(|_| Error::ProcessingFailed)?;

    // Read all bytes
    let reader = windows::Storage::Streams::DataReader::CreateDataReader(&stream)
        .map_err(|_| Error::ProcessingFailed)?;

    reader
        .LoadAsync(size as u32)
        .map_err(|_| Error::ProcessingFailed)?
        .get()
        .map_err(|_| Error::ProcessingFailed)?;

    let mut buffer = vec![0u8; size as usize];
    reader
        .ReadBytes(&mut buffer)
        .map_err(|_| Error::ProcessingFailed)?;

    #[cfg(feature = "log")]
    log::debug!(
        "Read photo: {}x{}, {} bytes",
        width,
        height,
        buffer.len()
    );

    Ok(PhotoData::new(buffer, width, height))
}

/// Returns whether camera capture is available on this device.
pub(crate) fn is_available() -> bool {
    // Initialize COM for this thread (needed for WinRT)
    // CameraCaptureUI requires single-threaded apartment (STA)
    let init_result = unsafe {
        windows::Win32::System::Com::CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
        )
    };

    // Check if COM init succeeded (or was already initialized)
    if init_result.is_err() && init_result != windows::Win32::Foundation::RPC_E_CHANGED_MODE {
        return false;
    }

    // Try to create a CameraCaptureUI instance
    // This will fail if no camera hardware is available
    let available = CameraCaptureUI::new().is_ok();

    // Uninitialize COM if we initialized it
    if init_result.is_ok() {
        unsafe {
            windows::Win32::System::Com::CoUninitialize();
        }
    }

    available
}
