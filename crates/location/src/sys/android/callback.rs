use std::{
    marker::PhantomData,
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{atomic::Ordering, Arc, OnceLock},
};

use jni::{
    objects::{GlobalRef, JClass, JObject, JValueGen},
    sys::{jboolean, jlong},
    JNIEnv, NativeMethod,
};

use super::Shared;
use crate::{Error, Result};

const CALLBACK_BYTECODE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/classes.dex"));

// NOTE: This must be kept in sync with `LocationCallback.java`.
const RUST_CALLBACK_NAME: &str = "rustCallback";
// NOTE: This must be kept in sync with the signature of `rust_callback`, and
// the signature specified in `LocationCallback.java`.
const RUST_CALLBACK_SIGNATURE: &str = "(JLandroid/location/Location;)V";

// NOTE: This must be kept in sync with `LocationPermissionFragment.java`.
const PERMISSION_CALLBACK_NAME: &str = "rustPermissionCallback";
// NOTE: This must be kept in sync with the signature of `rust_permission_callback`, and
// the signature specified in `LocationPermissionFragment.java`.
const PERMISSION_CALLBACK_SIGNATURE: &str = "(JZ)V";

// NOTE: The signature of this function must be kept in sync with
// `RUST_CALLBACK_SIGNATURE`.
unsafe extern "C" fn rust_callback<'a>(
    env: JNIEnv<'a>,
    _: JObject<'a>,
    shared_ptr: jlong,
    location: JObject<'a>,
) {
    #[cfg(not(target_pointer_width = "64"))]
    compile_error!("non-64-bit Android targets are not supported");

    if shared_ptr == 0 {
        return;
    }

    // SAFETY: the Java `LocationCallback` owns an `Arc<Shared>` ref that's only freed in `drop`, after
    // this callback has finished, so the pointer stays valid. We only take a shared reference.
    let shared = unsafe { &*(shared_ptr as *const Shared) };

    // A panic must never unwind across the JNI boundary.
    let _ = catch_unwind(AssertUnwindSafe(|| deliver_location(env, shared, location)));
}

fn deliver_location(env: JNIEnv<'_>, shared: &Shared, location: JObject<'_>) {
    // `getCurrentLocation` delivers `null` when it can't get a fresh fix; fall back to the last
    // known location before giving up.
    if location.as_raw().is_null() {
        super::deliver_last_known_or_error(shared);
        return;
    }

    let global = match env.new_global_ref(&location) {
        Ok(global) => global,
        Err(_) => {
            let _ = env.exception_clear();
            shared.handler.error(Error::Unknown);
            return;
        }
    };

    let location = crate::Location {
        inner: super::Location {
            inner: global,
            phantom: PhantomData,
        },
    };
    shared.handler.handle(location);
}

// NOTE: The signature of this function must be kept in sync with
// `PERMISSION_CALLBACK_SIGNATURE`.
unsafe extern "C" fn rust_permission_callback<'a>(
    _env: JNIEnv<'a>,
    _: JObject<'a>,
    callback_ptr: jlong,
    granted: jboolean,
) {
    if callback_ptr == 0 {
        return;
    }

    // SAFETY: this came from `Arc::into_raw` in `request_authorization`, and the fragment calls this
    // callback exactly once, so we take back exactly one strong reference here.
    let shared = unsafe { Arc::from_raw(callback_ptr as *const Shared) };

    // If the `Manager` is gone, drop the reference we took back without touching the handler.
    if !shared.dropped.load(Ordering::SeqCst) {
        let _ = catch_unwind(AssertUnwindSafe(|| {
            super::handle_permission_result(&shared, granted != 0);
        }));
    }
    // `shared` (the strong reference we took back) is dropped here.
}

static CALLBACK_CLASS: OnceLock<GlobalRef> = OnceLock::new();
static PERMISSION_FRAGMENT_CLASS: OnceLock<GlobalRef> = OnceLock::new();

pub(super) fn get_callback_class(env: &mut JNIEnv<'_>) -> Result<&'static GlobalRef> {
    // TODO: This can be optimised when the `once_cell_try` feature is stabilised.
    if let Some(class) = CALLBACK_CLASS.get() {
        return Ok(class);
    }
    let class = load_class(env, "robius.location.LocationCallback")?;
    register_native_method(
        env,
        &class,
        RUST_CALLBACK_NAME,
        RUST_CALLBACK_SIGNATURE,
        rust_callback as *mut _,
    )?;
    let global = env.new_global_ref(class)?;

    Ok(CALLBACK_CLASS.get_or_init(|| global))
}

pub(super) fn get_permission_fragment_class(env: &mut JNIEnv<'_>) -> Result<&'static GlobalRef> {
    if let Some(class) = PERMISSION_FRAGMENT_CLASS.get() {
        return Ok(class);
    }
    let class = load_class(env, "robius.location.LocationPermissionFragment")?;
    register_native_method(
        env,
        &class,
        PERMISSION_CALLBACK_NAME,
        PERMISSION_CALLBACK_SIGNATURE,
        rust_permission_callback as *mut _,
    )?;
    let global = env.new_global_ref(class)?;

    Ok(PERMISSION_FRAGMENT_CLASS.get_or_init(|| global))
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

fn load_class<'a>(env: &mut JNIEnv<'a>, class_name: &str) -> Result<JClass<'a>> {
    const IN_MEMORY_LOADER: &str = "dalvik/system/InMemoryDexClassLoader";

    let byte_buffer = unsafe {
        env.new_direct_byte_buffer(
            CALLBACK_BYTECODE.as_ptr() as *mut u8,
            CALLBACK_BYTECODE.len(),
        )
    }
    .map_err(|e| super::map_android_error(env, e))?;

    let dex_class_loader = env
        .new_object(
            IN_MEMORY_LOADER,
            "(Ljava/nio/ByteBuffer;Ljava/lang/ClassLoader;)V",
            &[
                JValueGen::Object(&JObject::from(byte_buffer)),
                JValueGen::Object(&JObject::null()),
            ],
        )
        .map_err(|e| super::map_android_error(env, e))?;

    let class_name = env
        .new_string(class_name)
        .map_err(|e| super::map_android_error(env, e))?;
    Ok(env
        .call_method(
            &dex_class_loader,
            "loadClass",
            "(Ljava/lang/String;)Ljava/lang/Class;",
            &[JValueGen::Object(&JObject::from(class_name))],
        )
        .map_err(|e| super::map_android_error(env, e))?
        .l()?
        .into())
}
