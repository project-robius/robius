use std::sync::OnceLock;

use jni::{
    objects::{GlobalRef, JClass, JObject, JValueGen},
    JNIEnv,
};

use crate::Result;

const SHARE_SHEET_BYTECODE: &[u8] = include_bytes!(
    concat!(env!("OUT_DIR"), "/classes.dex")
);

static SHARE_CLASS: OnceLock<GlobalRef> = OnceLock::new();

pub(super) fn get_share_class(env: &mut JNIEnv<'_>) -> Result<&'static GlobalRef> {
    if let Some(class) = SHARE_CLASS.get() {
        return Ok(class);
    }

    let share_class = load_share_class(env)?;
    let global = env.new_global_ref(share_class)?;

    Ok(SHARE_CLASS.get_or_init(|| global))
}

fn load_share_class<'a>(env: &mut JNIEnv<'a>) -> Result<JClass<'a>> {
    const IN_MEMORY_LOADER: &str = "dalvik/system/InMemoryDexClassLoader";

    let byte_buffer = unsafe {
        env.new_direct_byte_buffer(
            SHARE_SHEET_BYTECODE.as_ptr() as *mut u8,
            SHARE_SHEET_BYTECODE.len(),
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
            env.new_string("robius.share.ShareSheet")?,
        ))],
    )?.l()?.into())
}
