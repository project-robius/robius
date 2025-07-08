extern crate dirs_sys;

use std::path::PathBuf;

use jni::{
    objects::{JObject, JString},
    JNIEnv,
};
use BaseDirs;
use ProjectDirs;
use UserDirs;

pub fn base_dirs() -> Option<BaseDirs> {
    todo!()
}

pub fn user_dirs() -> Option<UserDirs> {
    todo!()
}

pub fn project_dirs_from_path(project_path: PathBuf) -> Option<ProjectDirs> {
    robius_android_env::with_activity(|env, context| {
        let cache_dir = get_path_from_fn(env, context, "getCacheDir")?;
        let config_dir = get_path_from_fn(env, context, "getFilesDir")?;
        let config_local_dir = config_dir.clone();
        let data_dir = config_dir.clone();
        let data_local_dir = config_dir.clone();
        let preference_dir = config_dir.clone();

        Some(ProjectDirs {
            project_path: project_path.clone(),
            cache_dir,
            config_dir,
            config_local_dir,
            data_dir,
            data_local_dir,
            preference_dir,
            runtime_dir: None,
            state_dir: None,
        })
    })
    .ok()
    .flatten()
}

fn get_path_from_fn(env: &mut JNIEnv, context: &JObject, fn_name: &'static str) -> Option<PathBuf> {
    let file = env
        .call_method(context, fn_name, "()Ljava/io/File;", &[])
        .ok()?
        .l()
        .ok()?;
    let jobject = env
        .call_method(file, "getAbsolutePath", "()Ljava/lang/String;", &[])
        .ok()?
        .l()
        .ok()?;
    let jstring = JString::from(jobject);
    let java_str = env.get_string(&jstring).ok()?;
    Some(PathBuf::from(java_str.to_str().ok()?))
}

pub fn project_dirs_from(
    _qualifier: &str,
    _organization: &str,
    _application: &str,
) -> Option<ProjectDirs> {
    ProjectDirs::from_path(PathBuf::new())
}
