//! Apple communication notifications (the `apple-communication` cargo feature):
//! renders a conversation notification as a real person-to-person message,
//! with the sender's (or group's) avatar as the icon, and donates the intent
//! for Siri suggestions and Focus. (Focus's per-contact "allowed people"
//! breakthrough needs contact details our Conversation doesn't carry, so the
//! upgrade here is the rendering + donations, not contact matching.)
//!
//! Works by describing the message as an `INSendMessageIntent` (Intents
//! framework), donating it, and asking UserNotifications to re-render the
//! content from it. Needs iOS 15+/macOS 12+, the
//! `com.apple.developer.usernotifications.communication` entitlement, and
//! `INSendMessageIntent` listed in the app's Info.plist `NSUserActivityTypes`
//! (donations fail silently without that); anything short of the entitlement
//! quietly falls back to the plain rendering.

use objc2::{msg_send, rc::Retained, sel, AnyThread};
use objc2_foundation::{NSArray, NSData, NSError, NSObjectProtocol, NSString};
use objc2_intents::{
    INImage, INInteraction, INInteractionDirection, INOutgoingMessageType, INPerson,
    INPersonHandle, INPersonHandleType, INSendMessageIntent, INSpeakableString,
};
use objc2_user_notifications::{UNMutableNotificationContent, UNNotificationContent};

use crate::{Conversation, NotificationOptions};

/// Re-renders `content` as a communication notification, or `None` if the
/// system can't (old OS) or won't (no entitlement) do it.
pub(super) fn enrich(
    content: &UNMutableNotificationContent,
    options: &NotificationOptions,
) -> Option<Retained<UNNotificationContent>> {
    let conversation = options.conversation.as_ref()?;
    // The re-render call only exists on iOS 15+/macOS 12+.
    if !content.respondsToSelector(sel!(contentByUpdatingWithProvider:error:)) {
        return None;
    }

    let sender = sender_person(options, conversation);
    // A spoken/display name for group chats; 1:1 chats go by the sender.
    let group_name = conversation.group_conversation.then(|| unsafe {
        INSpeakableString::initWithSpokenPhrase(
            INSpeakableString::alloc(),
            &NSString::from_str(&conversation.name),
        )
    });
    // The system only treats the intent as a group message (and shows the
    // group name) when it carries multiple recipients. We don't know the
    // actual members, so hand it two nameless placeholders.
    let recipients = conversation.group_conversation.then(|| {
        NSArray::from_retained_slice(&[
            placeholder_person(conversation, 1),
            placeholder_person(conversation, 2),
        ])
    });
    let body = options.body.as_deref().map(NSString::from_str);

    // An incoming message in this conversation, from this sender.
    let intent = unsafe {
        INSendMessageIntent::initWithRecipients_outgoingMessageType_content_speakableGroupName_conversationIdentifier_serviceName_sender_attachments(
            INSendMessageIntent::alloc(),
            recipients.as_deref(),
            INOutgoingMessageType::OutgoingMessageText,
            body.as_deref(),
            group_name.as_deref(),
            Some(&NSString::from_str(&conversation.id)),
            None,
            Some(&sender),
            None,
        )
    };
    // A group chat's avatar hangs off the group-name parameter, not the sender.
    if conversation.group_conversation {
        if let Some(image) = conversation_image(conversation) {
            unsafe {
                intent.setImage_forParameterNamed(
                    Some(&image),
                    &NSString::from_str("speakableGroupName"),
                );
            }
        }
    }

    // Donating powers Focus's "allowed people" and Siri suggestions; the
    // notification renders fine even if the donation itself fails.
    unsafe {
        let interaction =
            INInteraction::initWithIntent_response(INInteraction::alloc(), &intent, None);
        interaction.setDirection(INInteractionDirection::Incoming);
        interaction.donateInteractionWithCompletion(None);
    }

    // The typed `INSendMessageIntent: UNNotificationContentProviding`
    // conformance lives in a cross-framework category the bindings don't
    // model, so this one call goes through a raw selector.
    let enriched: Result<Retained<UNNotificationContent>, Retained<NSError>> =
        unsafe { msg_send![content, contentByUpdatingWithProvider: &*intent, error: _] };
    // The usual error here is the missing communication entitlement.
    enriched.ok()
}

/// The conversation's icon as an `INImage`, if it has one that loads.
fn conversation_image(conversation: &Conversation) -> Option<Retained<INImage>> {
    conversation
        .icon
        .as_deref()
        .and_then(|path| std::fs::read(path).ok())
        .map(|bytes| unsafe { INImage::imageWithImageData(&NSData::with_bytes(&bytes)) })
}

/// A nameless stand-in group member; see the recipients comment in [`enrich`].
fn placeholder_person(conversation: &Conversation, n: u32) -> Retained<INPerson> {
    let value = NSString::from_str(&format!("{}#member-{n}", conversation.id));
    let handle = unsafe {
        INPersonHandle::initWithValue_type(
            INPersonHandle::alloc(),
            Some(&value),
            INPersonHandleType::Unknown,
        )
    };
    unsafe {
        INPerson::initWithPersonHandle_nameComponents_displayName_image_contactIdentifier_customIdentifier(
            INPerson::alloc(),
            &handle,
            None,
            None,
            None,
            None,
            Some(&value),
        )
    }
}

/// The message's sender: named like the conversation history's sender
/// (the notification title, else the conversation name), with the
/// conversation's icon as their avatar.
fn sender_person(options: &NotificationOptions, conversation: &Conversation) -> Retained<INPerson> {
    let name = options
        .title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .unwrap_or(&conversation.name);
    let avatar = conversation_image(conversation);

    // The handle ties messages of one conversation to one "person"; we key it
    // by the conversation id, since that's the stable identity we have.
    let handle = unsafe {
        INPersonHandle::initWithValue_type(
            INPersonHandle::alloc(),
            Some(&NSString::from_str(&conversation.id)),
            INPersonHandleType::Unknown,
        )
    };
    unsafe {
        INPerson::initWithPersonHandle_nameComponents_displayName_image_contactIdentifier_customIdentifier(
            INPerson::alloc(),
            &handle,
            None,
            Some(&NSString::from_str(name)),
            avatar.as_deref(),
            None,
            Some(&NSString::from_str(&conversation.id)),
        )
    }
}
