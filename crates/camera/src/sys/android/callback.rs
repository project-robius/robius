use std::sync::OnceLock;

use jni::{
    objects::{GlobalRef, JByteArray, JClass, JObject, JValueGen},
    sys::{jint, jlong},
    JNIEnv, NativeMethod,
};

use crate::{Error, PhotoData, Result};

const CAMERA_DEX_BYTECODE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/classes.dex"));

// NOTE: This must be kept in sync with the signature of `rust_callback`.
const RUST_CALLBACK_SIGNATURE: &str = "(JI[BII)V";

/// The callback function that will be called from Java.
///
/// NOTE: The signature of this function must be kept in sync with `RUST_CALLBACK_SIGNATURE`.
///
/// # Safety
/// The callback_ptr must be a valid pointer to a boxed callback function.
unsafe extern "C" fn rust_callback<'a>(
    env: JNIEnv<'a>,
    _obj: JObject<'a>,
    callback_ptr: jlong,
    result_code: jint,
    jpeg_data: JByteArray<'a>,
    width: jint,
    height: jint,
) {
    // Reconstruct the callback from the raw pointer.
    // When we created the callback, we double-boxed it.
    let callback_ptr_boxed = unsafe {
        Box::from_raw(callback_ptr as *mut Box<dyn FnOnce(Result<PhotoData>) + Send>)
    };
    let callback = *callback_ptr_boxed;

    let result = match result_code {
        0 => {
            // Success - extract the JPEG data
            if jpeg_data.is_null() {
                Err(Error::ProcessingFailed)
            } else {
                match env.convert_byte_array(jpeg_data) {
                    Ok(data) => Ok(PhotoData::new(data, width as u32, height as u32)),
                    Err(_) => Err(Error::ProcessingFailed),
                }
            }
        }
        1 => Err(Error::Cancelled),
        2 => Err(Error::Unknown),
        3 => Err(Error::PermissionDenied),
        _ => Err(Error::Unknown),
    };

    callback(result);
}

static CALLBACK_CLASS: OnceLock<GlobalRef> = OnceLock::new();

/// Get or initialize the CameraResultCallback class.
pub(super) fn get_callback_class(env: &mut JNIEnv<'_>) -> Result<&'static GlobalRef> {
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
    .map_err(|e| Error::Java(e))
}

fn load_callback_class<'a>(env: &mut JNIEnv<'a>) -> Result<JClass<'a>> {
    const LOADER_CLASS: &str = "dalvik/system/InMemoryDexClassLoader";

    let byte_buffer = unsafe {
        env.new_direct_byte_buffer(
            CAMERA_DEX_BYTECODE.as_ptr() as *mut u8,
            CAMERA_DEX_BYTECODE.len(),
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
                env.new_string("robius/camera/CameraResultCallback").unwrap(),
            ))],
        )?
        .l()?
        .into())
}

/// Create an instance of CameraResultCallback with the given parameters.
pub(super) fn create_callback_instance<'a>(
    env: &mut JNIEnv<'a>,
    callback_class: &GlobalRef,
    activity: &JObject<'_>,
    callback_ptr: i64,
    use_front_camera: bool,
) -> Result<JObject<'a>> {
    // Safety: GlobalRef contains a JObject that we know is a Class (from load_callback_class).
    // JClass and JObject are both wrappers around jobject, so this transmute is safe.
    let class: JClass = unsafe { JClass::from_raw(callback_class.as_obj().as_raw()) };

    env.new_object(
        &class,
        "(Landroid/app/Activity;JZ)V",
        &[
            JValueGen::Object(activity),
            JValueGen::Long(callback_ptr),
            JValueGen::Bool(use_front_camera as u8),
        ],
    )
    .map_err(|e| Error::Java(e))
}
