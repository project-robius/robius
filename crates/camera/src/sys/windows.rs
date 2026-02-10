//! Windows implementation using CameraCaptureUI.
//!
//! This uses the Windows built-in camera UI which provides a consistent
//! user experience across Windows devices.

use std::thread;

use windows::{
    core::Interface,
    Foundation::{AsyncStatus, IAsyncOperation},
    Graphics::Imaging::BitmapDecoder,
    Media::Capture::{CameraCaptureUI, CameraCaptureUIMode, CameraCaptureUIPhotoFormat},
    Storage::{FileAccessMode, StorageFile},
    Win32::UI::Shell::IInitializeWithWindow,
    Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetForegroundWindow, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
    },
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
    eprintln!("[debug] Creating CameraCaptureUI...");
    // Create the camera capture UI
    let capture_ui = CameraCaptureUI::new().map_err(|e| {
        eprintln!("Failed to create CameraCaptureUI: {:?}", e);
        #[cfg(feature = "log")]
        log::error!("Failed to create CameraCaptureUI: {:?}", e);
        Error::CameraUnavailable
    })?;

    // Try to initialize with window handle if available (required for some Win32 apps)
    // This may fail on some Windows versions where CameraCaptureUI doesn't support it
    if let Ok(init_with_window) = capture_ui.cast::<IInitializeWithWindow>() {
        let hwnd = unsafe { GetForegroundWindow() };
        eprintln!("[debug] Initializing with window handle: {:?}", hwnd);
        unsafe {
            if let Err(e) = init_with_window.Initialize(hwnd) {
                eprintln!("[debug] Initialize with window failed (non-fatal): {:?}", e);
            }
        }
    } else {
        eprintln!("[debug] IInitializeWithWindow not supported, continuing without it");
    }

    eprintln!("[debug] Getting photo settings...");
    // Configure photo settings
    let photo_settings = capture_ui.PhotoSettings().map_err(|e| {
        eprintln!("Failed to get photo settings: {:?}", e);
        #[cfg(feature = "log")]
        log::error!("Failed to get photo settings: {:?}", e);
        Error::Unknown
    })?;

    eprintln!("[debug] Setting format to JPEG...");
    // Set format to JPEG
    photo_settings
        .SetFormat(CameraCaptureUIPhotoFormat::Jpeg)
        .map_err(|e| {
            eprintln!("Failed to set photo format: {:?}", e);
            #[cfg(feature = "log")]
            log::error!("Failed to set photo format: {:?}", e);
            Error::Unknown
        })?;

    eprintln!("[debug] Launching capture UI...");
    // Launch the capture UI and wait for result
    let async_op: IAsyncOperation<StorageFile> = capture_ui
        .CaptureFileAsync(CameraCaptureUIMode::Photo)
        .map_err(|e| {
            eprintln!("Failed to start capture: {:?}", e);
            #[cfg(feature = "log")]
            log::error!("Failed to start capture: {:?}", e);
            Error::CameraUnavailable
        })?;

    eprintln!("[debug] Waiting for capture result (pumping messages)...");
    // Pump messages while waiting for the async operation to complete
    // This is required for the UI to show and respond to user input
    let file: StorageFile = loop {
        // Check if the operation is complete
        let status = async_op.Status().map_err(|e| {
            eprintln!("Failed to get async status: {:?}", e);
            Error::Unknown
        })?;

        match status {
            AsyncStatus::Completed => {
                eprintln!("[debug] Async operation completed");
                break async_op.GetResults().map_err(|e| {
                    eprintln!("Failed to get results: {:?}", e);
                    Error::Unknown
                })?;
            }
            AsyncStatus::Error => {
                eprintln!("[debug] Async operation error");
                return Err(Error::Unknown);
            }
            AsyncStatus::Canceled => {
                eprintln!("[debug] Async operation canceled");
                return Err(Error::Cancelled);
            }
            AsyncStatus::Started => {
                // Still running - pump messages
                unsafe {
                    let mut msg = MSG::default();
                    while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            _ => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
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
