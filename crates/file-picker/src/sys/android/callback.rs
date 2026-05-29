use std::sync::OnceLock;
use jni::{
    objects::{GlobalRef, JClass, JObject, JString, JValueGen},
    sys::{jint, jlong},
    JNIEnv, NativeMethod,
};
use crate::{DialogCallback, Error, PickedFile, Result};

const FILE_PICKER_FRAGMENT_BYTECODE: &[u8] = include_bytes!(
    concat!(env!("OUT_DIR"), "/classes.dex")
);

pub(super) const RESULT_OK: i32 = -1;
pub(super) const RESULT_CANCELED: i32 = 0;
pub(super) const RESULT_ERROR: i32 = -2;

// NOTE: This must be kept in sync with `FilePickerFragment.java`.
const RUST_CALLBACK_NAME: &str = "rustCallback";
// NOTE: This must be kept in sync with the signature of `rust_callback`,
//       and the signature specified in `FilePickerFragment.java`.
const RUST_CALLBACK_SIGNATURE: &str =
    "(JILjava/lang/String;Ljava/lang/String;Ljava/lang/String;J)V";

// NOTE: The signature of this function must be kept in sync with
// `RUST_CALLBACK_SIGNATURE` above
unsafe extern "C" fn rust_callback<'a>(
    mut env: JNIEnv<'a>,
    _: JObject<'a>,
    callback_ptr: jlong,
    result_code: jint,
    uri: JString<'a>,
    display_name: JString<'a>,
    mime_type: JString<'a>,
    size: jlong,
) {
    let callback_ptr = callback_ptr as *mut DialogCallback;
    if callback_ptr.is_null() {
        return;
    }

    // SAFETY: This pointer was created by `Box::into_raw` in `show`.
    // The Java fragment invokes this callback at most once.
    let callback = *unsafe { Box::from_raw(callback_ptr) };

    // All JNI strings must be read here "up front", while we still hold the JNI env
    // on the Java callback thread (which is the Android main/UI thread).
    let result = match result_code {
        RESULT_OK => {
            if uri.as_raw().is_null() {
                Err(Error::Unknown)
            } else {
                match env.get_string(&uri) {
                    Ok(uri) => {
                        let uri = String::from(uri);
                        let display_name = optional_jstring(&mut env, &display_name);
                        let mime_type = optional_jstring(&mut env, &mime_type);
                        let size = (size >= 0).then_some(size as u64);
                        Ok(Some(PickedFile::from_uri_with_metadata(
                            uri,
                            display_name,
                            mime_type,
                            size,
                        )))
                    }
                    Err(_) => Err(Error::Unknown),
                }
            }
        }
        RESULT_CANCELED => Ok(None),
        RESULT_ERROR => Err(Error::Unsupported),
        _ => Err(Error::Unknown),
    };

    // Run the user's callback on a background thread so it can block if needed
    // without freezing the main UI thread.
    std::thread::spawn(move || callback(result));
}

fn optional_jstring(env: &mut JNIEnv<'_>, value: &JString<'_>) -> Option<String> {
    if value.as_raw().is_null() {
        return None;
    }
    env.get_string(value)
        .ok()
        .map(String::from)
        .filter(|s| !s.is_empty())
}

static FRAGMENT_CLASS: OnceLock<GlobalRef> = OnceLock::new();

pub(super) fn get_fragment_class(env: &mut JNIEnv<'_>) -> Result<&'static GlobalRef> {
    // TODO: This can be optimised when the `once_cell_try` feature is stabilised.
    if let Some(class) = FRAGMENT_CLASS.get() {
        return Ok(class);
    }
    let fragment_class = load_fragment_class(env)?;
    register_rust_callback(env, &fragment_class)?;
    let global = env.new_global_ref(fragment_class)?;

    Ok(FRAGMENT_CLASS.get_or_init(|| global))
}

fn register_rust_callback<'a>(env: &mut JNIEnv<'a>, fragment_class: &JClass<'a>) -> Result<()> {
    env.register_native_methods(
        fragment_class,
        &[NativeMethod {
            name: RUST_CALLBACK_NAME.into(),
            sig: RUST_CALLBACK_SIGNATURE.into(),
            fn_ptr: rust_callback as *mut _,
        }],
    )
    .map_err(|e| e.into())
}

fn load_fragment_class<'a>(env: &mut JNIEnv<'a>) -> Result<JClass<'a>> {
    const IN_MEMORY_LOADER: &str = "dalvik/system/InMemoryDexClassLoader";

    let byte_buffer = unsafe {
        env.new_direct_byte_buffer(
            FILE_PICKER_FRAGMENT_BYTECODE.as_ptr() as *mut u8,
            FILE_PICKER_FRAGMENT_BYTECODE.len(),
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

    Ok(env.call_method(
        &dex_class_loader,
        "loadClass",
        "(Ljava/lang/String;)Ljava/lang/Class;",
        &[JValueGen::Object(&JObject::from(
            env.new_string("robius.file_picker.FilePickerFragment")?,
        ))],
    )?.l()?.into()) // yikes, this syntax lmao
}
