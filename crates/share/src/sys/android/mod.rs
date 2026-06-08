mod class;

use jni::{
    objects::{JObject, JObjectArray, JValueGen},
    JNIEnv,
};

use crate::{file_items, shared_text, Error, Result, ShareOptions};

const RESULT_OK: i32 = 0;
const RESULT_NO_HANDLER: i32 = 1;
const RESULT_ERROR: i32 = 2;

pub(crate) fn share(options: ShareOptions) -> Result<()> {
    validate_android_items(&options)?;

    let result =
        robius_android_env::with_activity(|env, activity| share_inner(env, activity, &options));

    match result {
        Ok(inner) => inner,
        Err(_) => Err(Error::AndroidEnvironment),
    }
}

fn validate_android_items(options: &ShareOptions) -> Result<()> {
    for file in file_items(options) {
        if file.path().is_some() {
            return Err(Error::UnsupportedItem);
        }
        let Some(uri) = file.uri() else {
            return Err(Error::InvalidItem);
        };
        if !uri.starts_with("content://") {
            return Err(Error::UnsupportedItem);
        }
    }
    Ok(())
}

fn share_inner(
    env: &mut JNIEnv<'_>,
    activity: &JObject<'_>,
    options: &ShareOptions,
) -> Result<()> {
    let share_class = class::get_share_class(env)?;

    let title = options
        .title
        .as_deref()
        .map(|title| env.new_string(title))
        .transpose()?;
    let subject = options
        .subject
        .as_deref()
        .map(|subject| env.new_string(subject))
        .transpose()?;
    let text = shared_text(options)
        .map(|text| env.new_string(text))
        .transpose()?;

    let files = file_items(options).collect::<Vec<_>>();
    let uri_strings = files
        .iter()
        .filter_map(|file| file.uri().map(ToOwned::to_owned))
        .collect::<Vec<_>>();
    let mime_types = files
        .iter()
        .map(|file| file.mime_type().map(ToOwned::to_owned))
        .collect::<Vec<_>>();

    let uri_strings = string_array(env, &uri_strings)?;
    let mime_types = optional_string_array(env, &mime_types)?;

    let null = JObject::null();
    let title = title.as_ref().map(|title| title.as_ref()).unwrap_or(&null);
    let subject = subject
        .as_ref()
        .map(|subject| subject.as_ref())
        .unwrap_or(&null);
    let text = text.as_ref().map(|text| text.as_ref()).unwrap_or(&null);
    let uri_strings = uri_strings
        .as_ref()
        .map(|uris| uris.as_ref())
        .unwrap_or(&null);
    let mime_types = mime_types
        .as_ref()
        .map(|mimes| mimes.as_ref())
        .unwrap_or(&null);

    let result = env.call_static_method(
        share_class,
        "share",
        "(Landroid/app/Activity;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;[Ljava/lang/String;[Ljava/lang/String;)I",
        &[
            JValueGen::Object(activity),
            JValueGen::Object(title),
            JValueGen::Object(subject),
            JValueGen::Object(text),
            JValueGen::Object(uri_strings),
            JValueGen::Object(mime_types),
        ],
    )?.i()?;

    match result {
        RESULT_OK => Ok(()),
        RESULT_NO_HANDLER => Err(Error::NoHandler),
        RESULT_ERROR => Err(Error::Unknown),
        _ => Err(Error::Unknown),
    }
}

fn string_array<'a>(env: &mut JNIEnv<'a>, strings: &[String]) -> Result<Option<JObjectArray<'a>>> {
    if strings.is_empty() {
        return Ok(None);
    }

    let array = env.new_object_array(strings.len() as i32, "java/lang/String", JObject::null())?;
    for (index, string) in strings.iter().enumerate() {
        let string = env.new_string(string)?;
        env.set_object_array_element(&array, index as i32, string)?;
    }

    Ok(Some(array))
}

fn optional_string_array<'a>(
    env: &mut JNIEnv<'a>,
    strings: &[Option<String>],
) -> Result<Option<JObjectArray<'a>>> {
    if strings.is_empty() {
        return Ok(None);
    }

    let array = env.new_object_array(strings.len() as i32, "java/lang/String", JObject::null())?;
    for (index, string) in strings.iter().enumerate() {
        if let Some(string) = string {
            let string = env.new_string(string)?;
            env.set_object_array_element(&array, index as i32, string)?;
        }
    }

    Ok(Some(array))
}
