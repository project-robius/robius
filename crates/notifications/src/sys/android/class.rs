use std::sync::OnceLock;

use jni::{
    objects::{GlobalRef, JClass, JObject, JObjectArray, JString, JValueGen},
    sys::{jboolean, jint, jlong},
    JNIEnv, NativeMethod,
};

use crate::{Interaction, InteractionKind, PermissionCallback, Result};

const NOTIFICATIONS_BYTECODE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/classes.dex"));

// NOTE: This must be kept in sync with `Notifications.java`.
const INTERACTION_CALLBACK_NAME: &str = "rustInteractionCallback";
// NOTE: This must be kept in sync with the signature of `rust_interaction_callback`,
//       and the signature specified in `Notifications.java`.
const INTERACTION_CALLBACK_SIGNATURE: &str =
    "(Ljava/lang/String;ILjava/lang/String;Ljava/lang/String;[Ljava/lang/String;[Ljava/lang/String;)V";

// NOTE: This must be kept in sync with `NotificationPermissionFragment.java`.
const PERMISSION_CALLBACK_NAME: &str = "rustPermissionCallback";
// NOTE: This must be kept in sync with the signature of `rust_permission_callback`,
//       and the signature specified in `NotificationPermissionFragment.java`.
const PERMISSION_CALLBACK_SIGNATURE: &str = "(JZ)V";

// Interaction kinds passed in from Java; must match the `KIND_*` constants in `Notifications.java`.
const KIND_ACTIVATED: jint = 0;
const KIND_DISMISSED: jint = 1;
const KIND_ACTION: jint = 2;
const KIND_REPLY: jint = 3;

// NOTE: The signature of this function must be kept in sync with
// `INTERACTION_CALLBACK_SIGNATURE` above.
unsafe extern "C" fn rust_interaction_callback<'a>(
    mut env: JNIEnv<'a>,
    _: JClass<'a>,
    notification_id: JString<'a>,
    kind: jint,
    action_id: JString<'a>,
    reply_text: JString<'a>,
    metadata_keys: JObjectArray<'a>,
    metadata_values: JObjectArray<'a>,
) {
    // Read all JNI data up front, while we still hold the env on the Java
    // callback thread (usually the Android main thread).
    let Some(notification_id) = optional_jstring(&mut env, &notification_id) else {
        return;
    };
    let kind = match kind {
        KIND_ACTIVATED => InteractionKind::Activated,
        KIND_DISMISSED => InteractionKind::Dismissed,
        KIND_ACTION => InteractionKind::Action {
            id: optional_jstring(&mut env, &action_id).unwrap_or_default(),
        },
        KIND_REPLY => InteractionKind::Reply {
            action_id: optional_jstring(&mut env, &action_id).unwrap_or_default(),
            text: optional_jstring(&mut env, &reply_text).unwrap_or_default(),
        },
        _ => return,
    };
    let metadata = read_metadata(&mut env, &metadata_keys, &metadata_values);

    let interaction = Interaction {
        notification_id,
        kind,
        metadata,
    };

    // Hand it to the app on a background thread, so its handler can block
    // without freezing the Android main thread.
    std::thread::spawn(move || crate::deliver_interaction(interaction));
}

// NOTE: The signature of this function must be kept in sync with
// `PERMISSION_CALLBACK_SIGNATURE` above.
unsafe extern "C" fn rust_permission_callback<'a>(
    _env: JNIEnv<'a>,
    _: JClass<'a>,
    callback_ptr: jlong,
    granted: jboolean,
) {
    let callback_ptr = callback_ptr as *mut PermissionCallback;
    if callback_ptr.is_null() {
        return;
    }

    // SAFETY: This pointer was created by `Box::into_raw` in `request_permission`.
    // The Java side invokes this callback exactly once.
    let callback = *unsafe { Box::from_raw(callback_ptr) };

    // Run the app's callback on a background thread, so it can block if needed
    // without freezing the Android main thread.
    std::thread::spawn(move || callback(Ok(granted != 0)));
}

fn optional_jstring(env: &mut JNIEnv<'_>, value: &JString<'_>) -> Option<String> {
    if value.as_raw().is_null() {
        return None;
    }
    env.get_string(value).ok().map(String::from)
}

/// Reads the parallel key/value arrays back into metadata pairs.
fn read_metadata(
    env: &mut JNIEnv<'_>,
    keys: &JObjectArray<'_>,
    values: &JObjectArray<'_>,
) -> Vec<(String, String)> {
    if keys.as_raw().is_null() || values.as_raw().is_null() {
        return Vec::new();
    }
    let count = match (env.get_array_length(keys), env.get_array_length(values)) {
        (Ok(keys_len), Ok(values_len)) => keys_len.min(values_len),
        _ => return Vec::new(),
    };

    let mut metadata = Vec::with_capacity(count as usize);
    for index in 0..count {
        let pair = env.get_object_array_element(keys, index).and_then(|key| {
            env.get_object_array_element(values, index)
                .map(|value| (key, value))
        });
        let Ok((key, value)) = pair else {
            let _ = env.exception_clear();
            return metadata;
        };
        let key = optional_jstring(env, &JString::from(key));
        let value = optional_jstring(env, &JString::from(value));
        if let (Some(key), Some(value)) = (key, value) {
            metadata.push((key, value));
        }
    }
    metadata
}

static NOTIFICATIONS_CLASS: OnceLock<GlobalRef> = OnceLock::new();
static PERMISSION_FRAGMENT_CLASS: OnceLock<GlobalRef> = OnceLock::new();

pub(super) fn get_notifications_class(env: &mut JNIEnv<'_>) -> Result<&'static GlobalRef> {
    load_classes(env)?;
    Ok(NOTIFICATIONS_CLASS.get().expect("set by load_classes"))
}

pub(super) fn get_permission_fragment_class(env: &mut JNIEnv<'_>) -> Result<&'static GlobalRef> {
    load_classes(env)?;
    Ok(PERMISSION_FRAGMENT_CLASS.get().expect("set by load_classes"))
}

/// Loads both Java classes from one dex loader (so they share a defining class
/// loader) and registers their Rust native methods.
fn load_classes(env: &mut JNIEnv<'_>) -> Result<()> {
    if NOTIFICATIONS_CLASS.get().is_some() && PERMISSION_FRAGMENT_CLASS.get().is_some() {
        return Ok(());
    }

    let loader = dex_class_loader(env)?;

    let notifications = load_class(env, &loader, "robius.notifications.Notifications")?;
    register_native_method(
        env,
        &notifications,
        INTERACTION_CALLBACK_NAME,
        INTERACTION_CALLBACK_SIGNATURE,
        rust_interaction_callback as *mut _,
    )?;
    let notifications = env.new_global_ref(notifications)?;

    let fragment = load_class(
        env,
        &loader,
        "robius.notifications.NotificationPermissionFragment",
    )?;
    register_native_method(
        env,
        &fragment,
        PERMISSION_CALLBACK_NAME,
        PERMISSION_CALLBACK_SIGNATURE,
        rust_permission_callback as *mut _,
    )?;
    let fragment = env.new_global_ref(fragment)?;

    // If another thread won the race, its classes are just as good as ours.
    let _ = NOTIFICATIONS_CLASS.set(notifications);
    let _ = PERMISSION_FRAGMENT_CLASS.set(fragment);
    Ok(())
}

fn register_native_method<'a>(
    env: &mut JNIEnv<'a>,
    class: &JClass<'a>,
    name: &str,
    signature: &str,
    fn_ptr: *mut std::ffi::c_void,
) -> Result<()> {
    env.register_native_methods(
        class,
        &[NativeMethod {
            name: name.into(),
            sig: signature.into(),
            fn_ptr,
        }],
    )
    .map_err(|e| e.into())
}

fn dex_class_loader<'a>(env: &mut JNIEnv<'a>) -> Result<JObject<'a>> {
    const IN_MEMORY_LOADER: &str = "dalvik/system/InMemoryDexClassLoader";

    let byte_buffer = unsafe {
        env.new_direct_byte_buffer(
            NOTIFICATIONS_BYTECODE.as_ptr() as *mut u8,
            NOTIFICATIONS_BYTECODE.len(),
        )
    }?;

    Ok(env.new_object(
        IN_MEMORY_LOADER,
        "(Ljava/nio/ByteBuffer;Ljava/lang/ClassLoader;)V",
        &[
            JValueGen::Object(&JObject::from(byte_buffer)),
            JValueGen::Object(&JObject::null()),
        ],
    )?)
}

fn load_class<'a>(env: &mut JNIEnv<'a>, loader: &JObject<'_>, name: &str) -> Result<JClass<'a>> {
    let name = env.new_string(name)?;
    Ok(env
        .call_method(
            loader,
            "loadClass",
            "(Ljava/lang/String;)Ljava/lang/Class;",
            &[JValueGen::Object(&JObject::from(name))],
        )?
        .l()?
        .into())
}
