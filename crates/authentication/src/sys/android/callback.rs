use std::sync::OnceLock;

use jni::{
    objects::{GlobalRef, JClass, JObject, JValueGen},
    sys::{jint, jlong},
    JNIEnv, NativeMethod,
};

use crate::{Error, Result};

const AUTHENTICATION_CALLBACK_BYTECODE: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/classes.dex"));


// NOTE: This must be kept in sync with the signature of `rust_callback`.
const RUST_CALLBACK_SIGNATURE: &str = "(JII)V";

// NOTE: The signature of this function must be kept in sync with
// `RUST_CALLBACK_SIGNATURE`.
unsafe extern "C" fn rust_callback<'a>(
    _: JNIEnv<'a>,
    _: JObject<'a>,
    callback_ptr_ptr: jlong,
    error_code: jint,
    help_code: jint,
) {
    // When we constructed the callback, we double-boxed it.
    let callback_ptr_boxed = unsafe {
        Box::from_raw(callback_ptr_ptr as *mut Box<dyn Fn(Result<()>)>)
    };
    let callback = *callback_ptr_boxed;

    let result = if error_code != 0 {
        Err(match error_code {
            BIOMETRIC_ERROR_CANCELED => Error::SystemCanceled,
            // TODO: Differentiate between not present and unavailable?
            BIOMETRIC_ERROR_HW_NOT_PRESENT => Error::Unavailable,
            BIOMETRIC_ERROR_HW_UNAVAILABLE => Error::Unavailable,
            BIOMETRIC_ERROR_LOCKOUT => Error::Exhausted,
            // TODO: Differentiate between lockout and lockout permanent?
            BIOMETRIC_ERROR_LOCKOUT_PERMANENT => Error::Exhausted,
            BIOMETRIC_ERROR_NO_BIOMETRICS => Error::Unavailable,
            BIOMETRIC_ERROR_NO_DEVICE_CREDENTIAL => Error::Unavailable,
            BIOMETRIC_ERROR_NO_SPACE => Error::Unknown,
            BIOMETRIC_ERROR_SECURITY_UPDATE_REQUIRED => Error::UpdateRequired,
            BIOMETRIC_ERROR_TIMEOUT => Error::Timeout,
            BIOMETRIC_ERROR_UNABLE_TO_PROCESS => Error::Unknown,
            BIOMETRIC_ERROR_USER_CANCELED => Error::UserCanceled,
            BIOMETRIC_ERROR_VENDOR => Error::Unknown,
            BIOMETRIC_NO_AUTHENTICATION => Error::Unavailable,
            _ => Error::Unknown,
        })
    } else if help_code != 0 {
        // TODO: consider returning a specific retry-able error here.
        Err(Error::Unknown)
    } else {
        Ok(())
    };
    callback(result);
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
            name: "rustCallback".into(),
            sig: RUST_CALLBACK_SIGNATURE.into(),
            fn_ptr: rust_callback as *mut _,
        }],
    )
    .map_err(|e| e.into())
}

fn load_callback_class<'a>(env: &mut JNIEnv<'a>) -> Result<JClass<'a>> {
    const LOADER_CLASS: &str = "dalvik/system/InMemoryDexClassLoader";

    let byte_buffer = unsafe {
        env.new_direct_byte_buffer(
            AUTHENTICATION_CALLBACK_BYTECODE.as_ptr() as *mut u8,
            AUTHENTICATION_CALLBACK_BYTECODE.len(),
        )
    }?;

    let dex_class_loader = env.new_object(
        LOADER_CLASS,
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
                env.new_string("robius/authentication/AuthenticationCallback")
                    .unwrap(),
            ))],
        )?
        .l()?
        .into())
}

// https://developer.android.com/reference/android/hardware/biometrics/BiometricPrompt#BIOMETRIC_ERROR_CANCELED
const BIOMETRIC_ERROR_CANCELED: i32 = 5;
const BIOMETRIC_ERROR_HW_NOT_PRESENT: i32 = 0xc;
const BIOMETRIC_ERROR_HW_UNAVAILABLE: i32 = 1;
const BIOMETRIC_ERROR_LOCKOUT: i32 = 7;
const BIOMETRIC_ERROR_LOCKOUT_PERMANENT: i32 = 9;
const BIOMETRIC_ERROR_NO_BIOMETRICS: i32 = 0xb;
const BIOMETRIC_ERROR_NO_DEVICE_CREDENTIAL: i32 = 0xe;
const BIOMETRIC_ERROR_NO_SPACE: i32 = 4;
const BIOMETRIC_ERROR_SECURITY_UPDATE_REQUIRED: i32 = 0xf;
const BIOMETRIC_ERROR_TIMEOUT: i32 = 3;
const BIOMETRIC_ERROR_UNABLE_TO_PROCESS: i32 = 2;
const BIOMETRIC_ERROR_USER_CANCELED: i32 = 0xa;
const BIOMETRIC_ERROR_VENDOR: i32 = 8;
// NOTE: I don't think onAuthenticationError is ever actually called with this
// value.
const BIOMETRIC_NO_AUTHENTICATION: i32 = -1;
