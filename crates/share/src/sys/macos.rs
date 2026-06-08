#![allow(unused_unsafe)]

use std::path::Path;

use dispatch2::run_on_main;
use objc2::{
    rc::Retained,
    runtime::AnyObject,
    MainThreadMarker,
};
use objc2_app_kit::{NSApplication, NSSharingServicePicker, NSView};
use objc2_foundation::{NSArray, NSString, NSURL, NSRectEdge};

use crate::{Error, Result, ShareItem, ShareOptions, SharedFile};

pub(crate) fn share(options: ShareOptions) -> Result<()> {
    run_on_main(move |mtm| share_inner(mtm, options))
}

fn share_inner(mtm: MainThreadMarker, options: ShareOptions) -> Result<()> {
    let view = active_content_view(mtm)?;
    let items = sharing_items(&options)?;
    let items = NSArray::from_retained_slice(&items);
    let picker = unsafe {
        NSSharingServicePicker::initWithItems(mtm.alloc(), &items)
    };

    unsafe {
        picker.showRelativeToRect_ofView_preferredEdge(
            view.bounds(),
            &view,
            NSRectEdge::NSMinYEdge,
        );
    }

    Ok(())
}

fn sharing_items(options: &ShareOptions) -> Result<Vec<Retained<AnyObject>>> {
    let mut items = Vec::new();

    for item in &options.items {
        items.push(share_item_object(item)?);
    }

    if items.is_empty() {
        return Err(Error::Empty);
    }

    Ok(items)
}

fn share_item_object(item: &ShareItem) -> Result<Retained<AnyObject>> {
    match item {
        ShareItem::Text(text) => Ok(string_object(text)),
        ShareItem::Url(url) => url_object(url),
        ShareItem::File(file) => file_object(file),
    }
}

fn url_object(url: &str) -> Result<Retained<AnyObject>> {
    let url = unsafe { NSURL::URLWithString(&NSString::from_str(url)) }
        .ok_or(Error::InvalidItem)?;
    Ok(url.into_super().into_super())
}

fn file_object(file: &SharedFile) -> Result<Retained<AnyObject>> {
    if let Some(path) = file.path() {
        return file_url_object(path);
    }

    url_object(file.uri().ok_or(Error::InvalidItem)?)
}

fn file_url_object(path: &Path) -> Result<Retained<AnyObject>> {
    std::fs::metadata(path)?;
    let path = path.to_str().ok_or(Error::InvalidItem)?;
    let url = unsafe { NSURL::fileURLWithPath(&NSString::from_str(path)) };
    Ok(url.into_super().into_super())
}

fn string_object(text: &str) -> Retained<AnyObject> {
    NSString::from_str(text).into_super().into_super()
}

fn active_content_view(mtm: MainThreadMarker) -> Result<Retained<NSView>> {
    let application = NSApplication::sharedApplication(mtm);
    let window = application
        .keyWindow()
        .or_else(|| unsafe { application.mainWindow() })
        .ok_or(Error::Unknown)?;
    window.contentView().ok_or(Error::Unknown)
}
