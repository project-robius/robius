use std::{marker::PhantomData, sync::OnceLock};

use jni::{
    objects::{GlobalRef, JClass, JObject, JValueGen},
    sys::jlong,
    JNIEnv, NativeMethod,
};

use crate::Result;

const CALLBACK_BYTECODE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/classes.dex"));

// NOTE: This must be kept in sync with `LocationCallback.java`.
const RUST_CALLBACK_NAME: &str = "rustCallback";
// NOTE: This must be kept in sync with the signature of `rust_callback`, and
// the signature specified in `LocationCallback.java`.
const RUST_CALLBACK_SIGNATURE: &str = "(JJLandroid/location/Location;)V";

// NOTE: The signature of this function must be kept in sync with
// `RUST_CALLBACK_SIGNATURE`.
unsafe extern "C" fn rust_callback<'a>(
    env: JNIEnv<'a>,
    _: JObject<'a>,
    handler_ptr_high: jlong,
    handler_ptr_low: jlong,
    location: JObject<'a>,
) {
    // TODO: 32-bit? What's that?
    #[cfg(not(target_pointer_width = "64"))]
    compiler_error!("non-64-bit Android targets are not supported");

    let handler_ptr: *const super::InnerHandler =
        unsafe { std::mem::transmute([handler_ptr_high, handler_ptr_low]) };
    // SAFETY: See `Drop` implementation for `sys::android::Manager`.
    let handler = unsafe { &*handler_ptr };

    if let Ok(handler) = handler.lock() {
        let location = crate::Location {
            inner: super::Location {
                inner: env.new_global_ref(location).unwrap(),
                phantom: PhantomData,
            },
        };
        handler.handle(location);
    }
}

static CALLBACK_CLASS: OnceLock<GlobalRef> = OnceLock::new();

pub(super) fn get_callback_class(env: &mut JNIEnv<'_>) -> Result<&'static GlobalRef> {
    // TODO: This can be optimised when the `once_cell_try` feature is stabilised.

    if let Some(class) = CALLBACK_CLASS.get() {
        return Ok(class);
    }
    let callback_class = load_callback_class(env)?;
    register_rust_callback(env, &callback_class)?;
    let global = env.new_global_ref(callback_class)?;

    Ok(CALLBACK_CLASS.get_or_init(|| global))
}

fn register_rust_callback<'a>(env: &mut JNIEnv<'a>, callback_class: &JClass<'a>) -> Result<()> {
    env.register_native_methods(
        callback_class,
        &[NativeMethod {
            name: RUST_CALLBACK_NAME.into(),
            sig: RUST_CALLBACK_SIGNATURE.into(),
            fn_ptr: rust_callback as *mut _,
        }],
    )
    .map_err(|e| e.into())
}

fn load_callback_class<'a>(env: &mut JNIEnv<'a>) -> Result<JClass<'a>> {
    const IN_MEMORY_LOADER: &str = "dalvik/system/InMemoryDexClassLoader";

    let byte_buffer = unsafe {
        env.new_direct_byte_buffer(
            CALLBACK_BYTECODE.as_ptr() as *mut u8,
            CALLBACK_BYTECODE.len(),
        )
    }?;

    let dex_class_loader = env.new_object(
        IN_MEMORY_LOADER,
        "(Ljava/nio/ByteBuffer;Ljava/lang/ClassLoader;)V",
        &[
            JValueGen::Object(&JObject::from(byte_buffer)),
            JValueGen::Object(&JObject::null()),
        ],
    )?;

    Ok(env
        .call_method(
            &dex_class_loader,
            "loadClass",
            "(Ljava/lang/String;)Ljava/lang/Class;",
            &[JValueGen::Object(&JObject::from(
                env.new_string("robius.location.LocationCallback").unwrap(),
            ))],
        )?
        .l()?
        .into())
}
