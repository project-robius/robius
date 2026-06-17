use std::sync::{Arc, Mutex};

use windows::{
    ApplicationModel::DataTransfer::{
        DataPackageOperation, DataRequest, DataRequestedEventArgs, DataTransferManager,
    },
    Foundation::{Collections::IIterable, TypedEventHandler, Uri},
    Storage::{IStorageItem, StorageFile},
    Win32::{
        Foundation::HWND,
        UI::{
            Shell::IDataTransferManagerInterop,
            WindowsAndMessaging::GetForegroundWindow,
        },
    },
    core::{factory, HSTRING, HRESULT, Interface},
};

use crate::{file_items, shared_text, Error, Result, ShareItem, ShareOptions};

static ACTIVE_REGISTRATION: Mutex<Option<(isize, windows::Foundation::EventRegistrationToken)>> =
    Mutex::new(None);

pub(crate) fn share(options: ShareOptions) -> Result<()> {
    if !DataTransferManager::IsSupported()? {
        return Err(Error::NoHandler);
    }
    validate_windows_items(&options)?;

    let payload = Arc::new(options);
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.0 == 0 {
        return Err(Error::Unknown);
    }
    remove_active_registration();

    let interop: IDataTransferManagerInterop =
        factory::<DataTransferManager, IDataTransferManagerInterop>()?;
    let manager: DataTransferManager = unsafe { interop.GetForWindow(hwnd)? };

    let payload_for_handler = payload.clone();
    let token_slot = Arc::new(Mutex::new(None));
    let token_slot_for_handler = token_slot.clone();

    let handler = TypedEventHandler::new(
        move |_sender: &Option<DataTransferManager>,
              args: &Option<DataRequestedEventArgs>| {
            if let Some(args) = args {
                if let Ok(request) = args.Request() {
                    if let Ok(deferral) = request.GetDeferral() {
                        let payload_for_thread = payload_for_handler.clone();
                        std::thread::spawn(move || {
                            if fill_request(&payload_for_thread, &request).is_err() {
                                let _ = request.FailWithDisplayText(&HSTRING::from(
                                    "Unable to prepare the share payload.",
                                ));
                            }
                            let _ = deferral.Complete();
                        });
                    } else {
                        let _ = request.FailWithDisplayText(&HSTRING::from(
                            "Unable to prepare the share payload.",
                        ));
                    }
                }
            }

            if let Ok(mut token_slot) = token_slot_for_handler.lock() {
                if let Some(token) = token_slot.take() {
                    remove_registration(hwnd.0, token);
                }
            }

            Ok(())
        },
    );

    let token = manager.DataRequested(&handler)?;
    if let Ok(mut token_slot) = token_slot.lock() {
        *token_slot = Some(token);
    }
    set_active_registration(hwnd.0, token);

    let result = unsafe { interop.ShowShareUIForWindow(hwnd) };
    if result.is_err() {
        if let Ok(mut token_slot) = token_slot.lock() {
            if let Some(token) = token_slot.take() {
                let _ = manager.RemoveDataRequested(token);
            }
        }
        clear_active_registration(hwnd.0, token);
    }

    Ok(result?)
}

fn set_active_registration(hwnd: isize, token: windows::Foundation::EventRegistrationToken) {
    if let Ok(mut active) = ACTIVE_REGISTRATION.lock() {
        *active = Some((hwnd, token));
    }
}

fn clear_active_registration(hwnd: isize, token: windows::Foundation::EventRegistrationToken) {
    if let Ok(mut active) = ACTIVE_REGISTRATION.lock() {
        if active
            .as_ref()
            .map(|(active_hwnd, active_token)| {
                *active_hwnd == hwnd && active_token.Value == token.Value
            })
            .unwrap_or(false)
        {
            *active = None;
        }
    }
}

fn remove_active_registration() {
    let Some((hwnd, token)) = ACTIVE_REGISTRATION.lock().ok().and_then(|mut active| active.take())
    else {
        return;
    };

    remove_registration(hwnd, token);
}

fn remove_registration(hwnd: isize, token: windows::Foundation::EventRegistrationToken) {
    let result = factory::<DataTransferManager, IDataTransferManagerInterop>()
        .and_then(|interop| {
            let manager: DataTransferManager = unsafe { interop.GetForWindow(HWND(hwnd)) }?;
            manager.RemoveDataRequested(token)
        });
    let _ = result;
    clear_active_registration(hwnd, token);
}

fn validate_windows_items(options: &ShareOptions) -> Result<()> {
    for file in file_items(options) {
        let Some(path) = file.path() else {
            return Err(Error::UnsupportedItem);
        };
        std::fs::canonicalize(path)?;
    }

    Ok(())
}

fn fill_request(payload: &ShareOptions, request: &DataRequest) -> windows::core::Result<()> {
    let data = request.Data()?;
    let properties = data.Properties()?;

    let title = payload
        .title
        .as_deref()
        .or(payload.subject.as_deref())
        .unwrap_or("Share");
    properties.SetTitle(&HSTRING::from(title))?;
    if let Some(description) = &payload.subject {
        properties.SetDescription(&HSTRING::from(description.as_str()))?;
    }

    data.SetRequestedOperation(DataPackageOperation::Copy)?;

    if let Some(text) = shared_text(payload) {
        data.SetText(&HSTRING::from(text.as_str()))?;
    }
    if let Some(uri) = first_url(payload) {
        let uri = Uri::CreateUri(&HSTRING::from(uri.as_str()))?;
        data.SetWebLink(&uri)?;
    }
    let storage_items = storage_items(payload)?;
    if !storage_items.is_empty() {
        let items = IIterable::<IStorageItem>::try_from(storage_items)?;
        data.SetStorageItemsReadOnly(&items)?;
    }

    Ok(())
}

fn first_url(options: &ShareOptions) -> Option<&String> {
    options.items.iter().find_map(|item| match item {
        ShareItem::Url(url) => Some(url),
        _ => None,
    })
}

fn storage_items(options: &ShareOptions) -> windows::core::Result<Vec<Option<IStorageItem>>> {
    let mut storage_items = Vec::new();
    for file in file_items(options) {
        let Some(path) = file.path() else {
            continue;
        };
        let path = std::fs::canonicalize(path).map_err(io_to_windows_error)?;
        let storage_file =
            StorageFile::GetFileFromPathAsync(&HSTRING::from(path.as_path()))?.get()?;
        let storage_item: IStorageItem = storage_file.cast()?;
        storage_items.push(Some(storage_item));
    }

    Ok(storage_items)
}

fn io_to_windows_error(error: std::io::Error) -> windows::core::Error {
    windows::core::Error::new(
        HRESULT(0x80004005u32 as i32),
        error.to_string(),
    )
}
