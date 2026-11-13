//! Android keeps the theme in the night bits of `Configuration.uiMode`, and gives
//! each new configuration to the `ComponentCallbacks` that are registered with
//! the `Application`, on the main thread.
//!
//! `NativeTheme.java` is one of those. `build.rs` compiles it into a DEX file,
//! which is embedded here and loaded by the first subscription. It then stays
//! registered for the life of the process.
//!
//! The configuration is the app's own. Since Android 12, an app that has called
//! `UiModeManager.setApplicationNightMode` gets the mode it chose, not the system's.

use std::{
    ffi::c_void,
    panic::{catch_unwind, resume_unwind, AssertUnwindSafe},
    sync::{Mutex, PoisonError},
};

use jni::{
    objects::{JClass, JObject, JValue},
    sys::jint,
    JNIEnv, NativeMethod,
};

use crate::{Error, Result, Theme};

type JniResult<T> = jni::errors::Result<T>;

/// `NativeTheme.java`, as compiled by `build.rs`.
const DEX: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/classes.dex"));

pub(crate) fn query() -> Result<Option<Theme>> {
    with_context(|env, context| {
        let application = application(env, context)?;
        let resources = env.call_method(application, "getResources", "()Landroid/content/res/Resources;", &[])?.l()?;
        let configuration = env.call_method(resources, "getConfiguration", "()Landroid/content/res/Configuration;", &[])?.l()?;
        Ok(theme(env.get_field(configuration, "uiMode", "I")?.i()?))
    })
}

pub(crate) fn read_now() -> Option<Option<Theme>> {
    query().ok()
}

pub(crate) fn start() -> Result<()> {
    static IS_STARTED: Mutex<bool> = Mutex::new(false);
    let mut is_started = IS_STARTED.lock().unwrap_or_else(PoisonError::into_inner);
    if !*is_started {
        with_context(register)?;
        *is_started = true;
    }
    Ok(())
}

/// Loads `NativeTheme` and registers one with the `Application`, which keeps it from then on.
fn register(env: &mut JNIEnv<'_>, context: &JObject<'_>) -> JniResult<()> {
    let application = application(env, context)?;
    let dex = env.byte_array_from_slice(DEX)?;
    let dex = env.call_static_method("java/nio/ByteBuffer", "wrap", "([B)Ljava/nio/ByteBuffer;", &[JValue::Object(&dex)])?.l()?;
    // With a null parent, Android 8 and 9 can't resolve even `java.lang.Object`.
    let parent = env.call_static_method("java/lang/ClassLoader", "getSystemClassLoader", "()Ljava/lang/ClassLoader;", &[])?.l()?;
    let loader = env.new_object(
        "dalvik/system/InMemoryDexClassLoader",
        "(Ljava/nio/ByteBuffer;Ljava/lang/ClassLoader;)V",
        &[JValue::Object(&dex), JValue::Object(&parent)],
    )?;
    let name = env.new_string("dev.robius.theme.NativeTheme")?;
    let class = env.call_method(loader, "loadClass", "(Ljava/lang/String;)Ljava/lang/Class;", &[JValue::Object(&name)])?.l()?;
    let class = JClass::from(class);
    let changed = NativeMethod { name: "changed".into(), sig: "(I)V".into(), fn_ptr: changed as *mut c_void };
    env.register_native_methods(&class, &[changed])?;
    let callbacks = env.new_object(&class, "()V", &[])?;
    let callbacks = [JValue::Object(&callbacks)];
    env.call_method(application, "registerComponentCallbacks", "(Landroid/content/ComponentCallbacks;)V", &callbacks)?;
    Ok(())
}

/// `NativeTheme.onConfigurationChanged` calls this on the main thread, whatever it was that changed.
extern "system" fn changed(_env: JNIEnv<'_>, _class: JClass<'_>, ui_mode: jint) {
    crate::refresh(|| theme(ui_mode));
}

/// The night bits are `UI_MODE_NIGHT_NO` or `UI_MODE_NIGHT_YES`, or else undefined.
fn theme(ui_mode: jint) -> Option<Theme> {
    match ui_mode & 0x30 {
        0x10 => Some(Theme::Light),
        0x20 => Some(Theme::Dark),
        _ => None,
    }
}

/// The `Application`, which outlives every `Activity` and has none of their overrides.
fn application<'local>(env: &mut JNIEnv<'local>, context: &JObject<'_>) -> JniResult<JObject<'local>> {
    env.call_method(context, "getApplicationContext", "()Landroid/content/Context;", &[])?.l()
}

/// Runs `run` with the JNI environment and the host app's `Context`, and frees the references it makes.
fn with_context<T>(run: impl FnOnce(&mut JNIEnv<'_>, &JObject<'_>) -> JniResult<T>) -> Result<T> {
    let failed = |error| Error::Unavailable(format!("a JNI call failed: {error}"));
    let mut was_called = false;
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        robius_android_env::with_activity(|env, context| {
            was_called = true;
            if context.is_null() {
                return Err(Error::Unavailable("the app has no Android context yet".into()));
            }
            // An exception that was already pending is the caller's: it stays, and JNI can't be used.
            if env.exception_check().map_err(failed)? {
                return Err(Error::Unavailable("a Java exception is pending".into()));
            }
            let result = env.with_local_frame(16, |env| run(env, context));
            if result.is_err() {
                // Whatever is pending now is ours, so the caller mustn't be left with it.
                let _ = env.exception_clear();
            }
            result.map_err(failed)
        })
    }));
    match outcome {
        Ok(result) => result.map_err(failed)?,
        // Before it calls us, only the `ndk-context` backend can panic: the app gave it no context.
        Err(_) if !was_called => Err(Error::Unavailable("the app has not set its Android context".into())),
        Err(panic) => resume_unwind(panic),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_night_bits_decide_the_theme() {
        for other_bits in [0, 0x01, 0x0f, 0xc0] {
            assert_eq!(theme(other_bits | 0x10), Some(Theme::Light));
            assert_eq!(theme(other_bits | 0x20), Some(Theme::Dark));
            assert_eq!(theme(other_bits), None);
            assert_eq!(theme(other_bits | 0x30), None);
        }
    }
}
