use std::path::Path;

use dispatch2::run_on_main;
use objc2::{
    rc::Retained,
    runtime::AnyObject,
    ClassType, MainThreadMarker,
};
use objc2_foundation::{NSArray, NSObjectProtocol, NSString, NSURL};
use objc2_ui_kit::{
    UIApplication, UIActivityViewController, UIViewController, UIWindow,
};

use crate::{Error, Result, ShareItem, ShareOptions, SharedFile};

pub(crate) fn share(options: ShareOptions) -> Result<()> {
    run_on_main(move |mtm| share_inner(mtm, options))
}

fn share_inner(mtm: MainThreadMarker, options: ShareOptions) -> Result<()> {
    let presenter = presenting_view_controller(mtm)?;
    let items = activity_items(&options)?;
    let activity_items = NSArray::from_retained_slice(&items);
    let controller = unsafe {
        UIActivityViewController::initWithActivityItems_applicationActivities(
            mtm.alloc(),
            &activity_items,
            None,
        )
    };

    let controller: Retained<UIViewController> = controller.into_super();
    configure_popover(&controller, &presenter);
    unsafe {
        presenter.presentViewController_animated_completion(&controller, true, None);
    }

    Ok(())
}

fn activity_items(options: &ShareOptions) -> Result<Vec<Retained<AnyObject>>> {
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
    let path = std::fs::canonicalize(path)?;
    let path = path.to_str().ok_or(Error::InvalidItem)?;
    let url = unsafe { NSURL::fileURLWithPath(&NSString::from_str(path)) };
    Ok(url.into_super().into_super())
}

fn string_object(text: &str) -> Retained<AnyObject> {
    NSString::from_str(text).into_super().into_super()
}

fn configure_popover(controller: &UIViewController, presenter: &UIViewController) {
    let Some(popover) = (unsafe { controller.popoverPresentationController() }) else {
        return;
    };
    let Some(view) = presenter.view() else {
        return;
    };

    unsafe {
        popover.setSourceView(Some(&view));
        popover.setSourceRect(view.bounds());
    }
}

fn presenting_view_controller(mtm: MainThreadMarker) -> Result<Retained<UIViewController>> {
    let application = UIApplication::sharedApplication(mtm);
    let window = active_window(&application).ok_or(Error::Unknown)?;
    let root = window.rootViewController().ok_or(Error::Unknown)?;
    top_presenting_view_controller(root)
}

fn active_window(application: &UIApplication) -> Option<Retained<UIWindow>> {
    #[allow(deprecated)]
    if let Some(window) = unsafe { application.keyWindow() } {
        return Some(window);
    }

    #[allow(deprecated)]
    let windows = application.windows();
    windows
        .iter()
        .find(|window| window.isKeyWindow())
        .or_else(|| windows.iter().next())
}

fn top_presenting_view_controller(
    mut controller: Retained<UIViewController>,
) -> Result<Retained<UIViewController>> {
    loop {
        if is_share_controller(&controller) {
            return Err(Error::AlreadyOpen);
        }
        let Some(presented) = (unsafe { controller.presentedViewController() }) else {
            return Ok(controller);
        };
        controller = presented;
    }
}

fn is_share_controller(controller: &UIViewController) -> bool {
    controller.isKindOfClass(UIActivityViewController::class())
}
