mod class;

use std::time::SystemTime;

use jni::{
    objects::{JIntArray, JLongArray, JObject, JObjectArray, JString, JValueGen},
    sys::{jint, jlong},
    JNIEnv,
};

use crate::{
    ActionKind, ActiveIdsCallback, Error, LockScreenVisibility, NotificationOptions,
    NotificationSettings, PermissionCallback, Progress, Result, SettingsCallback, SettingsScope,
    Sound, Urgency,
};

/// Android has no OS-side scheduling here; lib.rs runs the fallback timer, so
/// `show` never sees a future `scheduled_time` and can ignore the field.
pub(crate) const NATIVE_SCHEDULING: bool = false;

// Result codes returned by the Java side; must match `Notifications.java`.
const RESULT_OK: i32 = 0;
const RESULT_PERMISSION_DENIED: i32 = 1;

// Channels used for notifications that didn't set one. Importance sticks to a
// channel once created, so each urgency gets its own default channel.
const DEFAULT_CHANNEL_QUIET: (&str, &str) =
    ("robius.notifications.default.quiet", "Quiet notifications");
const DEFAULT_CHANNEL_NORMAL: (&str, &str) = ("robius.notifications.default", "Notifications");
const DEFAULT_CHANNEL_URGENT: (&str, &str) =
    ("robius.notifications.default.urgent", "Urgent notifications");

pub(crate) fn show(options: NotificationOptions) -> Result<()> {
    // Catch a missing image file up front, with a useful I/O error.
    if let Some(image) = &options.image {
        std::fs::metadata(image)?;
    }

    robius_android_env::with_activity(|env, activity| {
        // The thread stays attached forever, so free our local refs via a frame
        // or they pile up until ART aborts.
        env.with_local_frame(64, |env| show_inner(env, activity, &options, false))
    })
    .map_err(|_| Error::AndroidEnvironment)
    .and_then(|x| x)
}

pub(crate) fn update_progress(options: &NotificationOptions) -> Result<()> {
    // The show path with the update-only flag: the same tag replaces the old
    // notification quietly (progress sets only-alert-once in Java), and Java
    // drops the post entirely if the user already dismissed it.
    robius_android_env::with_activity(|env, activity| {
        // Local frame: see `show`.
        env.with_local_frame(64, |env| show_inner(env, activity, options, true))
    })
    .map_err(|_| Error::AndroidEnvironment)
    .and_then(|x| x)
}

pub(crate) fn cancel(id: &str) -> Result<()> {
    robius_android_env::with_activity(|env, activity| {
        // Local frame: see `show`.
        env.with_local_frame(16, |env| {
            let class = class::get_notifications_class(env)?;
            let id = env.new_string(id)?;
            let result = env
                .call_static_method(
                    class,
                    "cancel",
                    "(Landroid/content/Context;Ljava/lang/String;)I",
                    &[JValueGen::Object(activity), JValueGen::Object(id.as_ref())],
                )
                .map_err(|e| map_jni_error(env, e))?
                .i()?;
            check_result(result)
        })
    })
    .map_err(|_| Error::AndroidEnvironment)
    .and_then(|x| x)
}

pub(crate) fn cancel_all() -> Result<()> {
    robius_android_env::with_activity(|env, activity| {
        // Local frame: see `show`.
        env.with_local_frame(16, |env| {
            let class = class::get_notifications_class(env)?;
            let result = env
                .call_static_method(
                    class,
                    "cancelAll",
                    "(Landroid/content/Context;)I",
                    &[JValueGen::Object(activity)],
                )
                .map_err(|e| map_jni_error(env, e))?
                .i()?;
            check_result(result)
        })
    })
    .map_err(|_| Error::AndroidEnvironment)
    .and_then(|x| x)
}

pub(crate) fn request_permission(callback: PermissionCallback, provisional: bool) -> Result<()> {
    // Android has no quiet-delivery permission: a provisional request just
    // reports the standing state, without ever showing the prompt fragment.
    if provisional {
        return report_permission_state(callback);
    }

    let callback_ptr = Box::into_raw(Box::new(callback));

    let result = robius_android_env::with_activity(|env, activity| {
        // Local frame: see `show`.
        env.with_local_frame(16, |env| -> Result<()> {
            let class = class::get_permission_fragment_class(env)?;
            env.call_static_method(
                class,
                "request",
                "(Landroid/app/Activity;J)V",
                &[
                    JValueGen::Object(activity),
                    JValueGen::Long(callback_ptr as jlong),
                ],
            )
            .map_err(|e| map_jni_error(env, e))?;
            Ok(())
        })
    });

    // Once the Java call went through, Java owns the pointer and delivers the
    // callback exactly once. On failure Java never got it, so free it here.
    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => {
            // SAFETY: `callback_ptr` came from `Box::into_raw` above and Java never got it.
            let _ = unsafe { Box::from_raw(callback_ptr) };
            Err(error)
        }
        Err(_) => {
            // SAFETY: same as above.
            let _ = unsafe { Box::from_raw(callback_ptr) };
            Err(Error::AndroidEnvironment)
        }
    }
}

/// Reports the standing permission state to the callback, without prompting.
fn report_permission_state(callback: PermissionCallback) -> Result<()> {
    let granted = robius_android_env::with_activity(|env, activity| {
        // Local frame: see `show`.
        env.with_local_frame(16, |env| -> Result<bool> {
            let class = class::get_permission_fragment_class(env)?;
            Ok(env
                .call_static_method(
                    class,
                    "currentPermissionState",
                    "(Landroid/content/Context;)Z",
                    &[JValueGen::Object(activity)],
                )
                .map_err(|e| map_jni_error(env, e))?
                .z()?)
        })
    })
    .map_err(|_| Error::AndroidEnvironment)
    .and_then(|x| x)?;

    callback(Ok(granted));
    Ok(())
}

pub(crate) fn active_notification_ids(callback: ActiveIdsCallback) -> Result<()> {
    // The whole query is synchronous, so the callback runs before we return.
    let ids = robius_android_env::with_activity(|env, activity| {
        // Local frame: see `show`.
        env.with_local_frame(16, |env| query_active_ids(env, activity))
    })
    .map_err(|_| Error::AndroidEnvironment)
    .and_then(|x| x)?;

    callback(Ok(ids));
    Ok(())
}

fn query_active_ids(env: &mut JNIEnv<'_>, activity: &JObject<'_>) -> Result<Vec<String>> {
    let class = class::get_notifications_class(env)?;
    let array = env
        .call_static_method(
            class,
            "activeNotificationIds",
            "(Landroid/content/Context;)[Ljava/lang/String;",
            &[JValueGen::Object(activity)],
        )
        .map_err(|e| map_jni_error(env, e))?
        .l()?;
    // The Java side returns null when the query failed.
    if array.as_raw().is_null() {
        return Err(Error::Unknown);
    }

    let array = JObjectArray::from(array);
    let count = env
        .get_array_length(&array)
        .map_err(|e| map_jni_error(env, e))?;
    let mut ids = Vec::with_capacity(count as usize);
    for index in 0..count {
        let element = env
            .get_object_array_element(&array, index)
            .map_err(|e| map_jni_error(env, e))?;
        // Java never puts nulls in the array, but don't crash if it did.
        if element.as_raw().is_null() {
            continue;
        }
        let element = JString::from(element);
        let id = match env.get_string(&element) {
            Ok(id) => String::from(id),
            Err(e) => return Err(map_jni_error(env, e)),
        };
        // Drop each element's ref right away, so big lists can't blow the ref table.
        env.delete_local_ref(element)?;
        ids.push(id);
    }
    Ok(ids)
}

pub(crate) fn init_interaction_listener() -> Result<()> {
    robius_android_env::with_activity(|env, activity| {
        // Local frame: see `show`.
        env.with_local_frame(16, |env| {
            let class = class::get_notifications_class(env)?;
            let result = env
                .call_static_method(
                    class,
                    "initListener",
                    "(Landroid/app/Activity;)I",
                    &[JValueGen::Object(activity)],
                )
                .map_err(|e| map_jni_error(env, e))?
                .i()?;
            check_result(result)
        })
    })
    .map_err(|_| Error::AndroidEnvironment)
    .and_then(|x| x)
}

pub(crate) fn notification_settings(scope: SettingsScope, callback: SettingsCallback) -> Result<()> {
    // The whole query is synchronous, so the callback runs before we return.
    let settings = robius_android_env::with_activity(|env, activity| {
        // Local frame: see `show`.
        env.with_local_frame(16, |env| query_settings(env, activity, &scope))
    })
    .map_err(|_| Error::AndroidEnvironment)
    .and_then(|x| x)?;

    callback(Ok(settings));
    Ok(())
}

fn query_settings(
    env: &mut JNIEnv<'_>,
    activity: &JObject<'_>,
    scope: &SettingsScope,
) -> Result<NotificationSettings> {
    let class = class::get_notifications_class(env)?;
    let (channel_id, conversation_id) = scope_ids(scope);
    let channel_id = optional_string(env, channel_id)?;
    let conversation_id = optional_string(env, conversation_id)?;
    let null = JObject::null();
    let channel_id = channel_id.as_ref().map(JString::as_ref).unwrap_or(&null);
    let conversation_id = conversation_id.as_ref().map(JString::as_ref).unwrap_or(&null);

    let array = env
        .call_static_method(
            class,
            "notificationSettings",
            "(Landroid/content/Context;Ljava/lang/String;Ljava/lang/String;)[I",
            &[
                JValueGen::Object(activity),
                JValueGen::Object(channel_id),
                JValueGen::Object(conversation_id),
            ],
        )
        .map_err(|e| map_jni_error(env, e))?
        .l()?;
    // The Java side returns null when the query failed.
    if array.as_raw().is_null() {
        return Err(Error::Unknown);
    }

    // [enabled, urgency, sound, badge, customized, priority], -1 = unknown;
    // must be kept in sync with `Notifications.java`.
    let mut values: [jint; 6] = [-1; 6];
    env.get_int_array_region(JIntArray::from(array), 0, &mut values)
        .map_err(|e| map_jni_error(env, e))?;

    Ok(NotificationSettings {
        enabled: values[0] == 1,
        urgency: match values[1] {
            0 => Some(Urgency::Low),
            1 => Some(Urgency::Normal),
            2 => Some(Urgency::Critical),
            _ => None,
        },
        sound_enabled: settings_bool(values[2]),
        badge_enabled: settings_bool(values[3]),
        customized_by_user: settings_bool(values[4]),
        priority_conversation: settings_bool(values[5]),
    })
}

pub(crate) fn open_notification_settings(scope: SettingsScope) -> Result<()> {
    let (channel_id, conversation_id) = scope_ids(&scope);
    robius_android_env::with_activity(|env, activity| {
        // Local frame: see `show`.
        env.with_local_frame(16, |env| {
            let class = class::get_notifications_class(env)?;
            let channel_id = optional_string(env, channel_id)?;
            let conversation_id = optional_string(env, conversation_id)?;
            let null = JObject::null();
            let channel_id = channel_id.as_ref().map(JString::as_ref).unwrap_or(&null);
            let conversation_id = conversation_id.as_ref().map(JString::as_ref).unwrap_or(&null);
            let result = env
                .call_static_method(
                    class,
                    "openSettings",
                    "(Landroid/content/Context;Ljava/lang/String;Ljava/lang/String;)I",
                    &[
                        JValueGen::Object(activity),
                        JValueGen::Object(channel_id),
                        JValueGen::Object(conversation_id),
                    ],
                )
                .map_err(|e| map_jni_error(env, e))?
                .i()?;
            check_result(result)
        })
    })
    .map_err(|_| Error::AndroidEnvironment)
    .and_then(|x| x)
}

/// The (channel id, conversation id) pair a scope passes to the Java side.
fn scope_ids(scope: &SettingsScope) -> (Option<&str>, Option<&str>) {
    match scope {
        SettingsScope::App => (None, None),
        SettingsScope::Channel { channel_id } => (Some(channel_id), None),
        SettingsScope::Conversation {
            channel_id,
            conversation_id,
        } => (Some(channel_id), Some(conversation_id)),
    }
}

/// Turns a Java-side settings value into an optional bool (-1 = unknown).
fn settings_bool(value: jint) -> Option<bool> {
    match value {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    }
}

fn show_inner(
    env: &mut JNIEnv<'_>,
    activity: &JObject<'_>,
    options: &NotificationOptions,
    update_only: bool,
) -> Result<()> {
    let class = class::get_notifications_class(env)?;

    // Importance (and sound) live on the channel on Android. An explicit channel
    // brings its own importance; the default channel takes it from the urgency.
    let (channel_id, channel_name, channel_description, importance) = match &options.channel {
        Some(channel) => (
            channel.id.as_str(),
            channel.name.as_str(),
            channel.description.as_deref(),
            channel.importance,
        ),
        None => {
            let urgency = options.urgency.unwrap_or_default();
            let (id, name) = match urgency {
                Urgency::Low => DEFAULT_CHANNEL_QUIET,
                Urgency::Normal => DEFAULT_CHANNEL_NORMAL,
                Urgency::Critical => DEFAULT_CHANNEL_URGENT,
            };
            (id, name, None, urgency)
        }
    };
    let importance: jint = match importance {
        Urgency::Low => 0,
        Urgency::Normal => 1,
        Urgency::Critical => 2,
    };
    // Sound is per-channel on Android, so a named per-notification sound has no
    // clean mapping; it falls back to the channel's default sound.
    let silent = matches!(options.sound, Some(Sound::Silent));

    // The path was checked in `show`, so a non-UTF-8 path is the only way to fail here.
    let image = options
        .image
        .as_deref()
        .map(|path| path.to_str().ok_or(Error::InvalidNotification))
        .transpose()?;

    // The channel's group crosses JNI as a {group id, group name} pair.
    let channel_group: Vec<&str> = options
        .channel
        .as_ref()
        .and_then(|channel| channel.group.as_ref())
        .map(|(group_id, group_name)| vec![group_id.as_str(), group_name.as_str()])
        .unwrap_or_default();

    // The conversation crosses as {id, name, icon path or null}.
    let conversation: Vec<Option<&str>> = match &options.conversation {
        Some(conversation) => vec![
            Some(conversation.id.as_str()),
            Some(conversation.name.as_str()),
            conversation
                .icon
                .as_deref()
                .map(|path| path.to_str().ok_or(Error::InvalidNotification))
                .transpose()?,
        ],
        None => Vec::new(),
    };
    let group_conversation = options
        .conversation
        .as_ref()
        .is_some_and(|conversation| conversation.group_conversation);

    // The conversation's history crosses as parallel {sender, text, timestamp} arrays.
    let message_senders: Vec<&str> = options
        .conversation_messages
        .iter()
        .map(|message| message.sender.as_str())
        .collect();
    let message_texts: Vec<&str> = options
        .conversation_messages
        .iter()
        .map(|message| message.text.as_str())
        .collect();
    let message_timestamps: Vec<jlong> = options
        .conversation_messages
        .iter()
        .map(|message| message.timestamp_ms.min(jlong::MAX as u64) as jlong)
        .collect();

    // -1 = no progress, 0 = indeterminate, > 0 = determinate; matches `Notifications.java`.
    let (progress_current, progress_total): (jint, jint) = match options.progress {
        None => (0, -1),
        Some(Progress::Indeterminate) => (0, 0),
        Some(Progress::Determinate { current, total }) => {
            let total = total.min(jint::MAX as u32);
            // An overshooting `current` just means done.
            (current.min(total) as jint, total as jint)
        }
    };

    // Event timestamp in ms since epoch, -1 when unset (pre-epoch clamps to 0).
    let when_ms: jlong = options
        .timestamp
        .map(|time| {
            time.duration_since(SystemTime::UNIX_EPOCH)
                .map(|since| since.as_millis().min(jlong::MAX as u128) as jlong)
                .unwrap_or(0)
        })
        .unwrap_or(-1);

    // -2 = unset; the codes must match the switch in `Notifications.java`.
    let visibility: jint = match options.lock_screen_visibility {
        None => -2,
        Some(LockScreenVisibility::Public) => 0,
        Some(LockScreenVisibility::Private) => 1,
        Some(LockScreenVisibility::Secret) => 2,
    };

    let metadata_keys: Vec<&str> = options.metadata.iter().map(|(key, _)| key.as_str()).collect();
    let metadata_values: Vec<&str> = options.metadata.iter().map(|(_, value)| value.as_str()).collect();

    let action_ids: Vec<&str> = options.actions.iter().map(|action| action.id.as_str()).collect();
    let action_titles: Vec<&str> = options.actions.iter().map(|action| action.title.as_str()).collect();
    let action_kinds: Vec<jint> = options
        .actions
        .iter()
        .map(|action| match action.kind {
            ActionKind::Button => 0,
            ActionKind::Reply => 1,
        })
        .collect();
    let action_placeholders: Vec<Option<&str>> = options
        .actions
        .iter()
        .map(|action| action.placeholder.as_deref())
        .collect();

    let badge_count: jint = options
        .badge_count
        .map(|count| count.min(jint::MAX as u32) as jint)
        .unwrap_or(-1);

    let id = env.new_string(&options.id)?;
    let title = optional_string(env, options.title.as_deref())?;
    let body = optional_string(env, options.body.as_deref())?;
    let subtitle = optional_string(env, options.subtitle.as_deref())?;
    let channel_id = env.new_string(channel_id)?;
    let channel_name = env.new_string(channel_name)?;
    let channel_description = optional_string(env, channel_description)?;
    let channel_group = string_array(env, &channel_group)?;
    let group = optional_string(env, options.group.as_deref())?;
    let image = optional_string(env, image)?;
    let conversation = optional_string_array(env, &conversation)?;
    let message_senders = string_array(env, &message_senders)?;
    let message_texts = string_array(env, &message_texts)?;
    let message_timestamps = long_array(env, &message_timestamps)?;
    let metadata_keys = string_array(env, &metadata_keys)?;
    let metadata_values = string_array(env, &metadata_values)?;
    let action_ids = string_array(env, &action_ids)?;
    let action_titles = string_array(env, &action_titles)?;
    let action_kinds = int_array(env, &action_kinds)?;
    let action_placeholders = optional_string_array(env, &action_placeholders)?;

    let null = JObject::null();
    let title = title.as_ref().map(JString::as_ref).unwrap_or(&null);
    let body = body.as_ref().map(JString::as_ref).unwrap_or(&null);
    let subtitle = subtitle.as_ref().map(JString::as_ref).unwrap_or(&null);
    let channel_description = channel_description
        .as_ref()
        .map(JString::as_ref)
        .unwrap_or(&null);
    let channel_group = channel_group.as_ref().map(JObjectArray::as_ref).unwrap_or(&null);
    let group = group.as_ref().map(JString::as_ref).unwrap_or(&null);
    let image = image.as_ref().map(JString::as_ref).unwrap_or(&null);
    let conversation = conversation.as_ref().map(JObjectArray::as_ref).unwrap_or(&null);
    let message_senders = message_senders.as_ref().map(JObjectArray::as_ref).unwrap_or(&null);
    let message_texts = message_texts.as_ref().map(JObjectArray::as_ref).unwrap_or(&null);
    let message_timestamps = message_timestamps
        .as_ref()
        .map(JLongArray::as_ref)
        .unwrap_or(&null);
    let metadata_keys = metadata_keys.as_ref().map(JObjectArray::as_ref).unwrap_or(&null);
    let metadata_values = metadata_values.as_ref().map(JObjectArray::as_ref).unwrap_or(&null);
    let action_ids = action_ids.as_ref().map(JObjectArray::as_ref).unwrap_or(&null);
    let action_titles = action_titles.as_ref().map(JObjectArray::as_ref).unwrap_or(&null);
    let action_kinds = action_kinds.as_ref().map(JIntArray::as_ref).unwrap_or(&null);
    let action_placeholders = action_placeholders
        .as_ref()
        .map(JObjectArray::as_ref)
        .unwrap_or(&null);

    let result = env
        .call_static_method(
            class,
            "show",
            "(Landroid/content/Context;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;\
             Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;\
             [Ljava/lang/String;IZZILjava/lang/String;Ljava/lang/String;[Ljava/lang/String;Z\
             [Ljava/lang/String;[Ljava/lang/String;[JIIJZI\
             [Ljava/lang/String;[Ljava/lang/String;[Ljava/lang/String;[Ljava/lang/String;\
             [I[Ljava/lang/String;Z)I",
            &[
                JValueGen::Object(activity),
                JValueGen::Object(id.as_ref()),
                JValueGen::Object(title),
                JValueGen::Object(body),
                JValueGen::Object(subtitle),
                JValueGen::Object(channel_id.as_ref()),
                JValueGen::Object(channel_name.as_ref()),
                JValueGen::Object(channel_description),
                JValueGen::Object(channel_group),
                JValueGen::Int(importance),
                JValueGen::Bool(silent as u8),
                JValueGen::Bool(options.bypass_dnd as u8),
                JValueGen::Int(badge_count),
                JValueGen::Object(group),
                JValueGen::Object(image),
                JValueGen::Object(conversation),
                JValueGen::Bool(group_conversation as u8),
                JValueGen::Object(message_senders),
                JValueGen::Object(message_texts),
                JValueGen::Object(message_timestamps),
                JValueGen::Int(progress_current),
                JValueGen::Int(progress_total),
                JValueGen::Long(when_ms),
                JValueGen::Bool(options.persistent as u8),
                JValueGen::Int(visibility),
                JValueGen::Object(metadata_keys),
                JValueGen::Object(metadata_values),
                JValueGen::Object(action_ids),
                JValueGen::Object(action_titles),
                JValueGen::Object(action_kinds),
                JValueGen::Object(action_placeholders),
                JValueGen::Bool(update_only as u8),
            ],
        )
        .map_err(|e| map_jni_error(env, e))?
        .i()?;

    check_result(result)
}

/// Maps a result code from the Java side into our `Result`.
fn check_result(result: i32) -> Result<()> {
    match result {
        RESULT_OK => Ok(()),
        RESULT_PERMISSION_DENIED => Err(Error::PermissionDenied),
        _ => Err(Error::Unknown),
    }
}

/// Turns a JNI error into our [`Error`], clearing any pending Java exception
/// first - a leftover exception would break the next JNI call.
fn map_jni_error(env: &mut JNIEnv<'_>, error: jni::errors::Error) -> Error {
    if matches!(error, jni::errors::Error::JavaException) {
        let _ = env.exception_clear();
    }
    error.into()
}

fn optional_string<'a>(env: &mut JNIEnv<'a>, text: Option<&str>) -> Result<Option<JString<'a>>> {
    text.map(|text| env.new_string(text).map_err(Error::from))
        .transpose()
}

fn string_array<'a>(env: &mut JNIEnv<'a>, strings: &[&str]) -> Result<Option<JObjectArray<'a>>> {
    if strings.is_empty() {
        return Ok(None);
    }

    let array = env.new_object_array(strings.len() as i32, "java/lang/String", JObject::null())?;
    for (index, string) in strings.iter().enumerate() {
        let string = env.new_string(string)?;
        env.set_object_array_element(&array, index as i32, &string)?;
        // Drop each element's ref right away, so big arrays can't blow the ref table.
        env.delete_local_ref(string)?;
    }

    Ok(Some(array))
}

fn optional_string_array<'a>(
    env: &mut JNIEnv<'a>,
    strings: &[Option<&str>],
) -> Result<Option<JObjectArray<'a>>> {
    if strings.is_empty() {
        return Ok(None);
    }

    let array = env.new_object_array(strings.len() as i32, "java/lang/String", JObject::null())?;
    for (index, string) in strings.iter().enumerate() {
        if let Some(string) = string {
            let string = env.new_string(string)?;
            env.set_object_array_element(&array, index as i32, &string)?;
            // Same as in `string_array`: don't hold refs for the whole call.
            env.delete_local_ref(string)?;
        }
    }

    Ok(Some(array))
}

fn int_array<'a>(env: &mut JNIEnv<'a>, values: &[jint]) -> Result<Option<JIntArray<'a>>> {
    if values.is_empty() {
        return Ok(None);
    }

    let array = env.new_int_array(values.len() as i32)?;
    env.set_int_array_region(&array, 0, values)?;
    Ok(Some(array))
}

fn long_array<'a>(env: &mut JNIEnv<'a>, values: &[jlong]) -> Result<Option<JLongArray<'a>>> {
    if values.is_empty() {
        return Ok(None);
    }

    let array = env.new_long_array(values.len() as i32)?;
    env.set_long_array_region(&array, 0, values)?;
    Ok(Some(array))
}
