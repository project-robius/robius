//! iOS keeps the system theme in the main screen's traits, which a window or
//! scene with its own style doesn't change. UIKit reports a change to each
//! window scene (iOS 17 and later) or to a view in a window (before that), and
//! a scene hears of it before the screen has the new theme. So every report
//! only schedules a read of the screen on a later pass of the main run loop.
//!
//! Only the main thread may read the screen. Other threads get the theme that
//! subscribers were last given, or else the copy of the screen's traits that
//! UIKit keeps for them, which is unset until the app has used the screen.

use std::{
    ptr,
    sync::{
        atomic::{AtomicPtr, Ordering},
        Once,
    },
};

use dispatch2::DispatchQueue;
use objc2::{
    define_class, msg_send,
    rc::{autoreleasepool, Retained},
    runtime::{AnyClass, AnyObject},
    sel, ClassType, MainThreadMarker, MainThreadOnly,
};
use objc2_foundation::{NSArray, NSNotification, NSNotificationCenter};
use objc2_ui_kit::{
    UIApplication, UIApplicationDidBecomeActiveNotification, UISceneWillConnectNotification, UIScreen, UITraitCollection,
    UITraitEnvironment, UIUserInterfaceStyle, UIView, UIWindow, UIWindowDidBecomeKeyNotification, UIWindowScene,
};

use crate::{Result, Theme};

/// The object that receives the system's updates, once the main thread has installed it.
static UPDATES: AtomicPtr<ThemeUpdates> = AtomicPtr::new(ptr::null_mut());

pub(crate) fn query() -> Result<Option<Theme>> {
    let Some(mtm) = MainThreadMarker::new() else {
        return Ok(crate::latest().unwrap_or_else(read_off_main));
    };
    let theme = read(mtm);
    // SAFETY: the object is never released, and only the main thread uses it.
    if let Some(updates) = unsafe { UPDATES.load(Ordering::Relaxed).as_ref() } {
        // Before iOS 17, a window with its own style keeps changes from us.
        if crate::latest() != Some(theme) {
            updates.check_later();
        }
    }
    Ok(theme)
}

pub(crate) fn read_now() -> Option<Option<Theme>> {
    MainThreadMarker::new().map(read)
}

pub(crate) fn start() -> Result<()> {
    static START: Once = Once::new();
    START.call_once(|| match MainThreadMarker::new() {
        Some(mtm) => install(mtm),
        None => DispatchQueue::main().exec_async(|| {
            if let Some(mtm) = MainThreadMarker::new() {
                install(mtm);
            }
        }),
    });
    Ok(())
}

fn read(mtm: MainThreadMarker) -> Option<Theme> {
    // Its replacement needs a window or a scene, and there may be none yet.
    #[allow(deprecated)]
    let traits = UIScreen::mainScreen(mtm).traitCollection();
    theme_of(&traits)
}

fn read_off_main() -> Option<Theme> {
    autoreleasepool(|_| {
        // SAFETY: each thread has its own current traits.
        let traits = unsafe { UITraitCollection::currentTraitCollection() };
        theme_of(&traits)
    })
}

fn theme_of(traits: &UITraitCollection) -> Option<Theme> {
    // SAFETY: traits never change once made, so any thread may read them.
    match unsafe { traits.userInterfaceStyle() } {
        UIUserInterfaceStyle::Light => Some(Theme::Light),
        UIUserInterfaceStyle::Dark => Some(Theme::Dark),
        _ => None,
    }
}

/// The trait to register for, which exists on iOS 17 and later.
fn style_trait() -> Option<&'static AnyClass> {
    AnyClass::get(c"UITraitUserInterfaceStyle")
}

/// Makes the object that receives the system's updates. It's never removed.
fn install(mtm: MainThreadMarker) {
    let updates = ThemeUpdates::alloc(mtm).set_ivars(());
    // SAFETY: `init` is how UIView makes a view of no size.
    let updates: Retained<ThemeUpdates> = unsafe { msg_send![super(updates), init] };
    updates.setHidden(true);
    updates.setUserInteractionEnabled(false);

    let center = NSNotificationCenter::defaultCenter();
    // SAFETY: each selector is a method of `updates` that takes the one object the center passes.
    unsafe {
        center.addObserver_selector_name_object(
            &updates,
            sel!(robiusThemeMayHaveChanged:),
            Some(UIApplicationDidBecomeActiveNotification),
            None,
        );
        let (selector, name) = match style_trait() {
            Some(_) => (sel!(robiusThemeSceneWillConnect:), UISceneWillConnectNotification),
            None => (sel!(robiusThemeWindowDidBecomeKey:), UIWindowDidBecomeKeyNotification),
        };
        center.addObserver_selector_name_object(&updates, selector, Some(name), None);
    }
    // SAFETY: `sharedApplication` returns the app, or nil before `UIApplicationMain`.
    let app: Option<Retained<UIApplication>> = unsafe { msg_send![UIApplication::class(), sharedApplication] };
    if let Some(app) = app {
        for scene in app.connectedScenes().iter() {
            updates.follow(&scene);
        }
    }
    // Subscribers still need their first theme if `read_now` couldn't give it.
    updates.check_later();
    UPDATES.store(Retained::into_raw(updates), Ordering::Relaxed);
}

define_class!(
    // SAFETY: UIView may be subclassed, the one override calls it, and nothing here implements `Drop`.
    #[unsafe(super(UIView))]
    #[thread_kind = MainThreadOnly]
    struct ThemeUpdates;

    impl ThemeUpdates {
        /// A scene's theme changed (iOS 17 and later), or the app became active.
        #[unsafe(method(robiusThemeMayHaveChanged:))]
        fn may_have_changed(&self, _: Option<&AnyObject>) {
            self.check_later();
        }

        /// Before iOS 17, the window tells its views like this that any trait changed.
        #[unsafe(method(traitCollectionDidChange:))]
        fn trait_collection_did_change(&self, previous: Option<&UITraitCollection>) {
            // SAFETY: this passes UIView's own method the argument it was meant to get.
            let _: () = unsafe { msg_send![super(self), traitCollectionDidChange: previous] };
            self.check_later();
        }

        #[unsafe(method(robiusThemeSceneWillConnect:))]
        fn scene_will_connect(&self, event: &NSNotification) {
            if let Some(scene) = event.object() {
                self.follow(&scene);
            }
        }

        #[unsafe(method(robiusThemeWindowDidBecomeKey:))]
        fn window_did_become_key(&self, event: &NSNotification) {
            if let Some(window) = event.object().and_then(|window| window.downcast::<UIWindow>().ok()) {
                window.addSubview(self);
            }
        }

        #[unsafe(method(robiusThemeCheck))]
        fn check(&self) {
            crate::refresh(|| read(self.mtm()));
        }
    }
);

impl ThemeUpdates {
    /// Starts receiving the changes that `scene` is told about.
    fn follow(&self, scene: &AnyObject) {
        let Some(scene) = scene.downcast_ref::<UIWindowScene>() else {
            return;
        };
        let Some(style) = style_trait() else {
            // Before iOS 17, only a view in a window is told, so sit in the key one.
            for window in scene.windows().iter().filter(|window| window.isKeyWindow()) {
                window.addSubview(self);
            }
            return;
        };
        let style: &AnyObject = style.as_ref();
        let traits = NSArray::from_slice(&[style]);
        // SAFETY: a scene has this method wherever the trait exists, and passes the action one object.
        let _: Option<Retained<AnyObject>> = unsafe {
            msg_send![scene, registerForTraitChanges: &*traits, withTarget: self, action: sel!(robiusThemeMayHaveChanged:)]
        };
    }

    /// Reads the screen on a later pass of the main run loop, when it has the new theme.
    fn check_later(&self) {
        // SAFETY: `robiusThemeCheck` takes no arguments and returns nothing.
        let _: () = unsafe {
            msg_send![self, performSelectorOnMainThread: sel!(robiusThemeCheck), withObject: None::<&AnyObject>, waitUntilDone: false]
        };
    }
}
