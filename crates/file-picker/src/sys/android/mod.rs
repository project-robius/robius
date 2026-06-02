mod callback;

use std::{collections::BTreeSet, path::{Path, PathBuf}};
use jni::{objects::{JByteArray, JObject, JObjectArray, JString, JValueGen}, JNIEnv};
use crate::{DialogCallback, DialogData, DialogOptions, Error, MediaKind, Result, StartLocation};

/// Maps a well-known folder to its external-storage directory name.
///
/// This is used to supply the `EXTRA_INITIAL_URI` hint on the Java side.
///
/// Returns `None` for `Desktop`, which doesn't exist on Android.
fn android_start_location_dir(location: Option<StartLocation>) -> Option<&'static str> {
    match location? {
        StartLocation::Documents => Some("Documents"),
        // The Downloads folder is named "Download" (singular) in external storage.
        StartLocation::Downloads => Some("Download"),
        StartLocation::Pictures => Some("Pictures"),
        StartLocation::Music => Some("Music"),
        // Android's videos folder is named "Movies".
        StartLocation::Videos => Some("Movies"),
        // Android has no Desktop folder.
        StartLocation::Desktop => None,
    }
}

pub(crate) fn pick_file(options: DialogOptions, on_completion: DialogCallback) -> Result<()> {
    show(options, false, None, on_completion)
}

pub(crate) fn read_uri_bytes(uri: &str) -> Result<Vec<u8>> {
    let result = robius_android_env::with_activity(
        |env, activity| read_uri_bytes_inner(env, activity, uri)
    );
    match result {
        Ok(inner) => inner,
        Err(_) => Err(Error::AndroidEnvironment),
    }
}

fn read_uri_bytes_inner(
    env: &mut JNIEnv<'_>,
    activity: &JObject<'_>,
    uri: &str,
) -> Result<Vec<u8>> {
    let fragment_class = callback::get_fragment_class(env)?;
    let uri_jstring = env.new_string(uri)?;

    let result = env.call_static_method(
        fragment_class,
        "readUriBytes",
        "(Landroid/app/Activity;Ljava/lang/String;)[B",
        &[
            JValueGen::Object(activity),
            JValueGen::Object(uri_jstring.as_ref()),
        ],
    )?;

    let array_obj = result.l()?;
    if array_obj.is_null() {
        return Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("failed to read content URI: {uri}"),
        )));
    }
    let array: JByteArray<'_> = array_obj.into();
    let len = env.get_array_length(&array)? as usize;
    // Allocate the `Vec<u8>` directly and hand JNI a transient `&mut [i8]`
    // view of the same storage. This avoids ever constructing a `Vec<i8>` and
    // transmuting it (which would violate `Vec::from_raw_parts`'s contract).
    let mut bytes = vec![0u8; len];
    // SAFETY: `i8` and `u8` share size and alignment; the slice is only used
    // for the duration of this JNI call and aliases the live `bytes` buffer.
    let view = unsafe { std::slice::from_raw_parts_mut(bytes.as_mut_ptr() as *mut i8, len) };
    env.get_byte_array_region(&array, 0, view)?;
    Ok(bytes)
}

pub(crate) fn app_temp_dir() -> Result<PathBuf> {
    let result =
        robius_android_env::with_activity(|env, activity| app_cache_dir_inner(env, activity));
    match result {
        Ok(inner) => inner,
        Err(_) => Err(Error::AndroidEnvironment),
    }
}

pub(crate) fn copy_uri_to_path(uri: &str, dest: &Path) -> Result<()> {
    let dest = dest.to_str().ok_or(Error::InvalidFileName)?;
    let result = robius_android_env::with_activity(|env, activity| {
        copy_uri_to_path_inner(env, activity, uri, dest)
    });
    match result {
        Ok(inner) => inner,
        Err(_) => Err(Error::AndroidEnvironment),
    }
}

fn copy_uri_to_path_inner(
    env: &mut JNIEnv<'_>,
    activity: &JObject<'_>,
    uri: &str,
    dest: &str,
) -> Result<()> {
    let fragment_class = callback::get_fragment_class(env)?;
    let uri = env.new_string(uri)?;
    let dest = env.new_string(dest)?;

    let ok = env
        .call_static_method(
            fragment_class,
            "copyUriToFile",
            "(Landroid/app/Activity;Ljava/lang/String;Ljava/lang/String;)Z",
            &[
                JValueGen::Object(activity),
                JValueGen::Object(uri.as_ref()),
                JValueGen::Object(dest.as_ref()),
            ],
        )?
        .z()?;

    if ok {
        Ok(())
    } else {
        Err(Error::Io(std::io::Error::other(
            "failed to copy content URI to local file",
        )))
    }
}

fn app_cache_dir_inner(env: &mut JNIEnv<'_>, activity: &JObject<'_>) -> Result<PathBuf> {
    let file = env
        .call_method(activity, "getCacheDir", "()Ljava/io/File;", &[])?
        .l()?;
    if file.is_null() {
        return Err(Error::Io(std::io::Error::other(
            "Activity.getCacheDir() returned null",
        )));
    }
    let path = env
        .call_method(&file, "getAbsolutePath", "()Ljava/lang/String;", &[])?
        .l()?;
    let path: JString<'_> = path.into();
    let path = env.get_string(&path)?;
    let path = path
        .to_str()
        .map_err(|_| Error::Io(std::io::Error::other("cache dir path is not valid UTF-8")))?;
    Ok(PathBuf::from(path))
}

pub(crate) fn save_data(
    options: DialogOptions,
    data: DialogData,
    on_completion: DialogCallback,
) -> Result<()> {
    let file_name = options.output_file_name_only()?;
    let mime_type = save_data_mime_type(&options, &file_name);
    let mime_types = mime_types(&options);

    let callback_ptr = Box::into_raw(Box::new(on_completion));

    let res = robius_android_env::with_activity(|env, activity| {
        let result = save_data_inner(
            env,
            activity,
            callback_ptr,
            &file_name,
            &mime_type,
            &mime_types,
            options.title.as_deref(),
            android_start_location_dir(options.start_location),
            (*data).as_ref(),
        );
        if result.is_err() {
            // SAFETY: Java will not receive this pointer if `save_data_inner` fails.
            let _ = unsafe { Box::from_raw(callback_ptr) };
        }
        result
    });

    match res {
        Ok(result) => result,
        Err(_) => {
            // SAFETY: Java will not receive this pointer if we failed to get
            // the Android environment.
            let _ = unsafe { Box::from_raw(callback_ptr) };
            Err(Error::AndroidEnvironment)
        }
    }
}

pub(crate) fn pick_media(
    options: DialogOptions,
    media_kind: MediaKind,
    on_completion: DialogCallback,
) -> Result<()> {
    let callback_ptr = Box::into_raw(Box::new(on_completion));

    let res = robius_android_env::with_activity(|env, activity| {
        let result = pick_media_inner(env, activity, callback_ptr, media_kind, options.title);
        if result.is_err() {
            // SAFETY: Java will not receive this pointer if `pick_media_inner` fails.
            let _ = unsafe { Box::from_raw(callback_ptr) };
        }
        result
    });

    match res {
        Ok(result) => result,
        Err(_) => {
            // SAFETY: Java will not receive this pointer if we failed to get
            // the Android environment.
            let _ = unsafe { Box::from_raw(callback_ptr) };
            Err(Error::AndroidEnvironment)
        }
    }
}

pub(crate) fn save_to_downloads(
    options: DialogOptions,
    source_path: PathBuf,
    on_completion: DialogCallback,
) -> Result<()> {
    let file_name = options.output_file_name(&source_path)?;
    let mime_type = save_mime_type(&options, &source_path);
    let source_path = source_path
        .to_str()
        .ok_or(Error::InvalidFileName)?
        .to_owned();

    let callback_ptr = Box::into_raw(Box::new(on_completion));

    let res = robius_android_env::with_activity(|env, activity| {
        let result = save_to_downloads_inner(
            env,
            activity,
            callback_ptr,
            &file_name,
            &mime_type,
            &source_path,
        );
        if result.is_err() {
            // SAFETY: Java will not receive this pointer if
            // `save_to_downloads_inner` fails.
            let _ = unsafe { Box::from_raw(callback_ptr) };
        }
        result
    });

    match res {
        Ok(result) => result,
        Err(_) => {
            // SAFETY: Java will not receive this pointer if we failed to get
            // the Android environment.
            let _ = unsafe { Box::from_raw(callback_ptr) };
            Err(Error::AndroidEnvironment)
        }
    }
}

fn show(
    options: DialogOptions,
    save: bool,
    source_path: Option<PathBuf>,
    on_completion: DialogCallback,
) -> Result<()> {
    let callback_ptr = Box::into_raw(Box::new(on_completion));

    let res = robius_android_env::with_activity(|env, activity| {
        let result = show_inner(env, activity, options, save, source_path, callback_ptr);
        if result.is_err() {
            // SAFETY: Java will not receive this pointer if `show_inner` fails.
            let _ = unsafe { Box::from_raw(callback_ptr) };
        }
        result
    });

    match res {
        Ok(result) => result,
        Err(_) => {
            // SAFETY: Java will not receive this pointer if we failed to get
            // the Android environment.
            let _ = unsafe { Box::from_raw(callback_ptr) };
            Err(Error::AndroidEnvironment)
        }
    }
}

fn show_inner(
    env: &mut JNIEnv<'_>,
    activity: &JObject<'_>,
    options: DialogOptions,
    save: bool,
    source_path: Option<PathBuf>,
    callback_ptr: *mut DialogCallback,
) -> Result<()> {
    let fragment_class = callback::get_fragment_class(env)?;
    let mime_types = mime_types(&options);
    let mime_type = source_path
        .as_deref()
        .map(|source_path| save_mime_type(&options, source_path))
        .unwrap_or_else(|| primary_mime_type(options.mime_type.as_deref(), &mime_types));
    let file_name = if save {
        let source_path = source_path.as_deref().ok_or(Error::InvalidFileName)?;
        Some(options.output_file_name(source_path)?)
    } else {
        options.file_name.clone()
    };
    let source_path = source_path
        .as_deref()
        .map(|path| path.to_str().ok_or(Error::InvalidFileName))
        .transpose()?;

    let null = JObject::null();
    let title = options
        .title
        .as_deref()
        .map(|title| env.new_string(title))
        .transpose()?;
    let file_name = file_name
        .as_deref()
        .map(|file_name| env.new_string(file_name))
        .transpose()?;
    let source_path = source_path
        .map(|source_path| env.new_string(source_path))
        .transpose()?;
    let mime_type = env.new_string(mime_type)?;
    let mime_types = string_array(env, &mime_types)?;
    let initial_location = android_start_location_dir(options.start_location)
        .map(|dir| env.new_string(dir))
        .transpose()?;

    let title = title.as_ref().map(|title| title.as_ref()).unwrap_or(&null);
    let file_name = file_name
        .as_ref()
        .map(|file_name| file_name.as_ref())
        .unwrap_or(&null);
    let source_path = source_path
        .as_ref()
        .map(|source_path| source_path.as_ref())
        .unwrap_or(&null);
    let mime_types = mime_types
        .as_ref()
        .map(|mime_types| mime_types.as_ref())
        .unwrap_or(&null);
    let initial_location = initial_location
        .as_ref()
        .map(|loc| loc.as_ref())
        .unwrap_or(&null);

    let shown = env.call_static_method(
        fragment_class,
        "show",
        "(Landroid/app/Activity;JZLjava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;[Ljava/lang/String;Ljava/lang/String;)Z",
        &[
            JValueGen::Object(activity),
            JValueGen::Long(callback_ptr as i64),
            JValueGen::Bool(if save { 1 } else { 0 }),
            JValueGen::Object(title),
            JValueGen::Object(file_name),
            JValueGen::Object(source_path),
            JValueGen::Object(mime_type.as_ref()),
            JValueGen::Object(mime_types),
            JValueGen::Object(initial_location),
        ],
    )?.z()?;

    shown.then_some(()).ok_or(Error::AlreadyOpen)
}

fn save_to_downloads_inner(
    env: &mut JNIEnv<'_>,
    activity: &JObject<'_>,
    callback_ptr: *mut DialogCallback,
    file_name: &str,
    mime_type: &str,
    source_path: &str,
) -> Result<()> {
    let fragment_class = callback::get_fragment_class(env)?;
    let file_name = env.new_string(file_name)?;
    let mime_type = env.new_string(mime_type)?;
    let source_path = env.new_string(source_path)?;

    let accepted = env.call_static_method(
        fragment_class,
        "saveToDownloads",
        "(Landroid/app/Activity;JLjava/lang/String;Ljava/lang/String;Ljava/lang/String;)Z",
        &[
            JValueGen::Object(activity),
            JValueGen::Long(callback_ptr as i64),
            JValueGen::Object(file_name.as_ref()),
            JValueGen::Object(mime_type.as_ref()),
            JValueGen::Object(source_path.as_ref()),
        ],
    )?.z()?;

    accepted.then_some(()).ok_or(Error::AlreadyOpen)
}

#[allow(clippy::too_many_arguments)]
fn save_data_inner(
    env: &mut JNIEnv<'_>,
    activity: &JObject<'_>,
    callback_ptr: *mut DialogCallback,
    file_name: &str,
    mime_type: &str,
    mime_types: &[String],
    title: Option<&str>,
    initial_location: Option<&str>,
    data: &[u8],
) -> Result<()> {
    let fragment_class = callback::get_fragment_class(env)?;

    let file_name = env.new_string(file_name)?;
    let mime_type = env.new_string(mime_type)?;
    let mime_types = string_array(env, mime_types)?;
    let title = title.map(|title| env.new_string(title)).transpose()?;
    let initial_location = initial_location
        .map(|loc| env.new_string(loc))
        .transpose()?;

    let data_array: JByteArray<'_> = env.new_byte_array(data.len() as i32)?;
    // SAFE: `data` is a valid slice for the lifetime of this call,
    // and `data.len()` matches the byte array's length.
    env.set_byte_array_region(&data_array, 0, unsafe {
        std::slice::from_raw_parts(data.as_ptr() as *const i8, data.len())
    })?;

    let null = JObject::null();
    let title = title.as_ref().map(|title| title.as_ref()).unwrap_or(&null);
    let mime_types = mime_types
        .as_ref()
        .map(|mime_types| mime_types.as_ref())
        .unwrap_or(&null);
    let initial_location = initial_location
        .as_ref()
        .map(|loc| loc.as_ref())
        .unwrap_or(&null);

    let shown = env.call_static_method(
        fragment_class,
        "saveData",
        "(Landroid/app/Activity;JLjava/lang/String;Ljava/lang/String;[Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;[B)Z",
        &[
            JValueGen::Object(activity),
            JValueGen::Long(callback_ptr as i64),
            JValueGen::Object(file_name.as_ref()),
            JValueGen::Object(mime_type.as_ref()),
            JValueGen::Object(mime_types),
            JValueGen::Object(title),
            JValueGen::Object(initial_location),
            JValueGen::Object(&data_array.into()),
        ],
    )?.z()?;

    shown.then_some(()).ok_or(Error::AlreadyOpen)
}

fn pick_media_inner(
    env: &mut JNIEnv<'_>,
    activity: &JObject<'_>,
    callback_ptr: *mut DialogCallback,
    media_kind: MediaKind,
    title: Option<String>,
) -> Result<()> {
    let fragment_class = callback::get_fragment_class(env)?;
    let null = JObject::null();
    let title = title
        .as_deref()
        .map(|title| env.new_string(title))
        .transpose()?;
    let title = title.as_ref().map(|title| title.as_ref()).unwrap_or(&null);

    let shown = env.call_static_method(
        fragment_class,
        "pickMedia",
        "(Landroid/app/Activity;JILjava/lang/String;)Z",
        &[
            JValueGen::Object(activity),
            JValueGen::Long(callback_ptr as i64),
            JValueGen::Int(android_media_kind(media_kind)),
            JValueGen::Object(title),
        ],
    )?.z()?;

    shown.then_some(()).ok_or(Error::AlreadyOpen)
}

fn android_media_kind(media_kind: MediaKind) -> i32 {
    match media_kind {
        MediaKind::Image => 1,
        MediaKind::Video => 2,
        MediaKind::ImageOrVideo => 3,
    }
}

fn mime_types(options: &DialogOptions) -> Vec<String> {
    let mut mime_types = BTreeSet::new();
    if let Some(mime_type) = options.mime_type.as_deref().filter(|mime| !mime.is_empty()) {
        mime_types.insert(mime_type.to_owned());
    }

    for extension in options.filters.iter().flat_map(|f| f.extensions.iter()) {
        let extension = extension.trim_start_matches('.');
        if extension.is_empty() {
            continue;
        }
        for mime in mime_guess::from_ext(extension).iter() {
            mime_types.insert(mime.essence_str().to_owned());
        }
    }

    mime_types.into_iter().collect()
}

fn primary_mime_type(explicit: Option<&str>, mime_types: &[String]) -> String {
    if let Some(mime_type) = explicit.filter(|mime| !mime.is_empty()) {
        return mime_type.to_owned();
    }

    if mime_types.len() == 1 {
        return mime_types[0].clone();
    }

    let mut primary = None;
    for mime_type in mime_types {
        let Some((next_primary, _)) = mime_type.split_once('/') else {
            return "*/*".to_owned();
        };
        match primary {
            Some(primary) if primary != next_primary => return "*/*".to_owned(),
            Some(_) => {}
            None => primary = Some(next_primary),
        }
    }

    primary.map(|primary| format!("{primary}/*"))
        .unwrap_or_else(|| "*/*".to_owned())
}

fn save_mime_type(options: &DialogOptions, source_path: &Path) -> String {
    if let Some(mime_type) = options.mime_type.as_deref().filter(|mime| !mime.is_empty()) {
        return mime_type.to_owned();
    }

    if let Some(mime) = mime_guess::from_path(source_path).first() {
        return mime.essence_str().to_owned();
    }

    primary_mime_type(None, &mime_types(options))
}

fn save_data_mime_type(options: &DialogOptions, file_name: &str) -> String {
    if let Some(mime_type) = options.mime_type.as_deref().filter(|mime| !mime.is_empty()) {
        return mime_type.to_owned();
    }

    if let Some(mime) = mime_guess::from_path(Path::new(file_name)).first() {
        return mime.essence_str().to_owned();
    }

    primary_mime_type(None, &mime_types(options))
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
