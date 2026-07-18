use block2::DynBlock;
use objc2::{define_class, msg_send, rc::Retained, AnyThread};
use objc2_foundation::{NSObject, NSObjectProtocol};
use objc2_user_notifications::{
    UNNotification, UNNotificationDefaultActionIdentifier, UNNotificationDismissActionIdentifier,
    UNNotificationPresentationOptions, UNNotificationResponse, UNTextInputNotificationResponse,
    UNUserNotificationCenter, UNUserNotificationCenterDelegate,
};

use crate::{Interaction, InteractionKind};

define_class!(
    // Not MainThreadOnly: these callbacks can land on any thread.
    #[unsafe(super(NSObject))]
    pub(super) struct RobiusNotificationsDelegate;

    unsafe impl NSObjectProtocol for RobiusNotificationsDelegate {}

    unsafe impl UNUserNotificationCenterDelegate for RobiusNotificationsDelegate {
        // The user did something with a notification: map it and hand it to the app.
        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        #[allow(non_snake_case)]
        unsafe fn userNotificationCenter_didReceiveNotificationResponse_withCompletionHandler(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            completion_handler: &DynBlock<dyn Fn()>,
        ) {
            // A panicking app handler must not unwind across this ObjC frame,
            // and the OS completion handler has to run no matter what.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                crate::deliver_interaction(interaction_from(response));
            }));
            completion_handler.call(());
        }

        // Also show notifications while the app is in the foreground.
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        #[allow(non_snake_case)]
        unsafe fn userNotificationCenter_willPresentNotification_withCompletionHandler(
            &self,
            _center: &UNUserNotificationCenter,
            _notification: &UNNotification,
            completion_handler: &DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            completion_handler.call((presentation_options(),));
        }

        // The user asked to see the app's own notification settings screen.
        #[unsafe(method(userNotificationCenter:openSettingsForNotification:))]
        #[allow(non_snake_case)]
        unsafe fn userNotificationCenter_openSettingsForNotification(
            &self,
            _center: &UNUserNotificationCenter,
            notification: Option<&UNNotification>,
        ) {
            // Same as above: a panicking app handler must not unwind
            // across this ObjC frame.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                crate::deliver_interaction(open_settings_interaction(notification));
            }));
        }
    }
);

impl RobiusNotificationsDelegate {
    pub(super) fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

fn interaction_from(response: &UNNotificationResponse) -> Interaction {
    let request = response.notification().request();
    let action_id = response.actionIdentifier();
    // These statics are just marker identifiers; reading them is harmless.
    let (default_id, dismiss_id) = unsafe {
        (
            UNNotificationDefaultActionIdentifier,
            UNNotificationDismissActionIdentifier,
        )
    };
    let kind = if *action_id == *default_id {
        InteractionKind::Activated
    } else if *action_id == *dismiss_id {
        InteractionKind::Dismissed
    } else if let Some(reply) = response.downcast_ref::<UNTextInputNotificationResponse>() {
        InteractionKind::Reply {
            action_id: action_id.to_string(),
            text: reply.userText().to_string(),
        }
    } else {
        InteractionKind::Action {
            id: action_id.to_string(),
        }
    };

    Interaction {
        notification_id: request.identifier().to_string(),
        kind,
        metadata: super::metadata_from_content(&request.content()),
    }
}

// The user may get to the settings link from one specific notification, or from none.
fn open_settings_interaction(notification: Option<&UNNotification>) -> Interaction {
    let (notification_id, metadata) = match notification {
        Some(notification) => {
            let request = notification.request();
            (
                request.identifier().to_string(),
                super::metadata_from_content(&request.content()),
            )
        }
        None => (String::new(), Vec::new()),
    };
    Interaction {
        notification_id,
        kind: InteractionKind::OpenSettings,
        metadata,
    }
}

fn presentation_options() -> UNNotificationPresentationOptions {
    // Banner/List replaced Alert in iOS 14/macOS 11; fall back for anything older.
    let show = if objc2::available!(ios = 14.0, macos = 11.0, ..) {
        UNNotificationPresentationOptions::Banner | UNNotificationPresentationOptions::List
    } else {
        #[allow(deprecated)]
        let alert = UNNotificationPresentationOptions::Alert;
        alert
    };
    show | UNNotificationPresentationOptions::Sound | UNNotificationPresentationOptions::Badge
}
