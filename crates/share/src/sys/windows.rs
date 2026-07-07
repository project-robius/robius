use std::{
    collections::BTreeMap,
    ffi::OsString,
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use windows::{
    ApplicationModel::DataTransfer::{
        DataPackageOperation, DataRequest, DataRequestedEventArgs, DataTransferManager,
    },
    Foundation::{Collections::IIterable, EventRegistrationToken, TypedEventHandler, Uri},
    Storage::{IStorageItem, StorageFile},
    Win32::{
        Foundation::HWND,
        UI::{
            Shell::IDataTransferManagerInterop,
            WindowsAndMessaging::GetForegroundWindow,
        },
    },
    core::{factory, AgileReference, HSTRING, Interface},
};

use crate::{file_items, shared_text, Error, Result, ShareItem, ShareOptions};

/// Tracks the `DataRequested` handler registered for each window, keyed by HWND.
///
/// This mostly exist to avoid one app window messing with another's registered shares.
static ACTIVE_REGISTRATIONS: Mutex<BTreeMap<isize, i64>> = Mutex::new(BTreeMap::new());

pub(crate) fn share(options: ShareOptions) -> Result<()> {
    if !DataTransferManager::IsSupported()? {
        return Err(Error::NoHandler);
    }

    // Resolve every file attachment to a WinRT storage item up front,
    // otherwise we won't know what error actually occurred.
    let storage_items = resolve_storage_items(&options)?;

    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.0 == 0 {
        return Err(Error::Unknown);
    }

    // Drop any handler left registered by a previous share on this same window.
    remove_stale_registration(hwnd.0);

    let interop: IDataTransferManagerInterop =
        factory::<DataTransferManager, IDataTransferManagerInterop>()?;
    let manager: DataTransferManager = unsafe { interop.GetForWindow(hwnd)? };
    let active_token = Arc::new(Mutex::new(None));
    let handler_token = active_token.clone();
    let handler_hwnd = hwnd.0;

    // The handler does no async work now that the storage items are pre-resolved,
    // so it can fill the request synchronously without a deferral or worker thread.
    let handler = TypedEventHandler::new(
        move |_sender: &Option<DataTransferManager>,
              args: &Option<DataRequestedEventArgs>| {
            let result = (|| {
                if let Some(args) = args {
                    let request = args.Request()?;
                    if fill_request(&options, &storage_items, &request).is_err() {
                        let _ = request.FailWithDisplayText(&HSTRING::from(
                            "Unable to prepare the share payload.",
                        ));
                    }
                }
                Ok(())
            })();
            remove_current_registration(handler_hwnd, handler_token.as_ref());
            result
        },
    );

    let token = manager.DataRequested(&handler)?;
    if let Ok(mut active_token) = active_token.lock() {
        *active_token = Some(token);
    }
    record_registration(hwnd.0, token);

    if let Err(error) = unsafe { interop.ShowShareUIForWindow(hwnd) } {
        // The UI never appeared, so the handler will never fire: remove it now.
        remove_current_registration(hwnd.0, active_token.as_ref());
        return Err(error.into());
    }

    Ok(())
}

fn record_registration(hwnd: isize, token: EventRegistrationToken) {
    if let Ok(mut registrations) = ACTIVE_REGISTRATIONS.lock() {
        registrations.insert(hwnd, token.Value);
    }
}

fn forget_registration(hwnd: isize, token: EventRegistrationToken) {
    if let Ok(mut registrations) = ACTIVE_REGISTRATIONS.lock() {
        if registrations.get(&hwnd) == Some(&token.Value) {
            registrations.remove(&hwnd);
        }
    }
}

fn remove_stale_registration(hwnd: isize) {
    let stale = ACTIVE_REGISTRATIONS
        .lock()
        .ok()
        .and_then(|mut registrations| registrations.remove(&hwnd));
    let Some(value) = stale else {
        return;
    };

    let token = EventRegistrationToken { Value: value };
    let _ = factory::<DataTransferManager, IDataTransferManagerInterop>().and_then(|interop| {
        let manager: DataTransferManager = unsafe { interop.GetForWindow(HWND(hwnd)) }?;
        manager.RemoveDataRequested(token)
    });
}

fn remove_current_registration(
    hwnd: isize,
    active_token: &Mutex<Option<EventRegistrationToken>>,
) {
    let Some(token) = active_token.lock().ok().and_then(|mut token| token.take()) else {
        return;
    };

    let _ = factory::<DataTransferManager, IDataTransferManagerInterop>().and_then(|interop| {
        let manager: DataTransferManager = unsafe { interop.GetForWindow(HWND(hwnd)) }?;
        manager.RemoveDataRequested(token)
    });
    forget_registration(hwnd, token);
}

/// Resolves the share payload's file attachments to WinRT [`IStorageItem`]s.
///
/// Windows sharing only accepts filesystem paths, and they need to be
/// canonicalized to avoid the weird "//?/" prefix.
///
/// The items are returned as [`AgileReference`]s because the `TypedEventHandler`
/// closure they're stored in must be `Send`, and raw WinRT interfaces aren't Send.
fn resolve_storage_items(options: &ShareOptions) -> Result<Vec<AgileReference<IStorageItem>>> {
    let mut storage_items = Vec::new();
    for file in file_items(options) {
        let Some(path) = file.path() else {
            return Err(Error::UnsupportedItem);
        };

        // `canonicalize` resolves the path, but adds a `\\?\` prefix to the result.
        // The `StorageFile::GetFileFromPathAsync` API rejects verbatim paths, so strip it out.
        let path = std::fs::canonicalize(path)?;
        let path = strip_verbatim_prefix(&path);
        let storage_file =
            StorageFile::GetFileFromPathAsync(&HSTRING::from(path.as_path()))?.get()?;
        let item: IStorageItem = storage_file.cast()?;
        storage_items.push(AgileReference::new(&item)?);
    }

    Ok(storage_items)
}

fn fill_request(
    payload: &ShareOptions,
    storage_items: &[AgileReference<IStorageItem>],
    request: &DataRequest,
) -> windows::core::Result<()> {
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
    if !storage_items.is_empty() {
        let items = storage_items.iter()
            .map(|item| item.resolve().map(Some))
            .collect::<windows::core::Result<Vec<Option<IStorageItem>>>>()?;
        let items = IIterable::<IStorageItem>::try_from(items)?;
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

/// Converts a `std::fs::canonicalize` result into a normal absolute path that the
/// WinRT `StorageFile` API can aceept.
///
/// On Windows, `canonicalize` returns an extended-length (verbatim) path,
/// which has a weird `\\?\` prefix.
/// For example, `\\?\UNC\server\share` --> `\\server\share`,
/// or `\\?\C:\dir\file` --> `C:\dir\file`.
///
/// The conversion operates on the raw UTF-16s that Windows uses.
fn strip_verbatim_prefix(path: &Path) -> PathBuf {
    const VERBATIM: &str = r"\\?\";
    const VERBATIM_UNC: &str = r"\\?\UNC\";

    let wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    let verbatim: Vec<u16> = VERBATIM.encode_utf16().collect();
    let verbatim_unc: Vec<u16> = VERBATIM_UNC.encode_utf16().collect();

    if wide.starts_with(&verbatim_unc) {
        // `\\?\UNC\server\share` -> `\\server\share`
        let mut rebuilt: Vec<u16> = r"\\".encode_utf16().collect();
        rebuilt.extend_from_slice(&wide[verbatim_unc.len()..]);
        PathBuf::from(OsString::from_wide(&rebuilt))
    } else if wide.starts_with(&verbatim) {
        // `\\?\C:\dir\file` -> `C:\dir\file`
        PathBuf::from(OsString::from_wide(&wide[verbatim.len()..]))
    } else {
        path.to_path_buf()
    }
}
