//! Compiles `NativeTheme.java` into the DEX file that `src/android.rs` embeds.
//!
//! That takes an Android SDK and a JDK. `ANDROID_HOME`, `ANDROID_PLATFORM`,
//! `ANDROID_BUILD_TOOLS_VERSION` and `JAVA_HOME` say which ones to use. Without
//! them it's the SDK in its default place, the newest platform and build tools
//! in it, and the system's JDK.

use std::{
    env::{self, consts::EXE_SUFFIX},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

const JAVA_FILE: &str = "java/dev/robius/theme/NativeTheme.java";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("android") {
        return;
    }
    println!("cargo:rerun-if-changed={JAVA_FILE}");

    let sdk = var("ANDROID_HOME").or_else(|| var("ANDROID_SDK_ROOT")).or_else(default_sdk).unwrap_or_default();
    assert!(sdk.is_dir(), "no Android SDK at `{}`: set ANDROID_HOME", sdk.display());
    let android_jar = installed(&sdk.join("platforms"), "ANDROID_PLATFORM", "android.jar");
    let d8_jar = installed(&sdk.join("build-tools"), "ANDROID_BUILD_TOOLS_VERSION", "lib/d8.jar");
    let jdk = jdk().expect("no JDK found: set JAVA_HOME");
    let tool = |name: &str| jdk.join(format!("{name}{EXE_SUFFIX}"));

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let classes = out_dir.join("classes");
    fs::create_dir_all(&classes).unwrap();
    // Every d8 can read Java 8 class files, whichever JDK compiles them.
    let mut javac = Command::new(tool("javac"));
    javac.args(["-source", "8", "-target", "8", "-Xlint:-options", "-classpath"]).arg(&android_jar);
    run(javac.arg("-d").arg(&classes).arg(JAVA_FILE));
    let mut d8 = Command::new(tool("java"));
    d8.arg("-cp").arg(d8_jar).args(["com.android.tools.r8.D8", "--min-api", "26", "--lib"]).arg(&android_jar);
    run(d8.arg("--output").arg(&out_dir).arg(classes.join("dev/robius/theme/NativeTheme.class")));
}

/// Reads a variable, and has cargo run this script again when it changes. An empty one counts as unset.
fn var(name: &str) -> Option<PathBuf> {
    println!("cargo:rerun-if-env-changed={name}");
    env::var_os(name).filter(|value| !value.is_empty()).map(PathBuf::from)
}

/// Where Android Studio puts the SDK.
fn default_sdk() -> Option<PathBuf> {
    match env::consts::OS {
        "macos" => Some(var("HOME")?.join("Library/Android/sdk")),
        "windows" => Some(var("LOCALAPPDATA")?.join("Android/Sdk")),
        _ => Some(var("HOME")?.join("Android/Sdk")),
    }
}

/// The `file` of the version that `variable` names, or else of the newest version in `dir` that has one.
fn installed(dir: &Path, variable: &str, file: &str) -> PathBuf {
    let Some(version) = var(variable) else {
        return newest(dir, file).unwrap_or_else(|| panic!("nothing in `{}` has {file}", dir.display()));
    };
    let path = dir.join(version).join(file);
    assert!(path.is_file(), "`{}` not found: check {variable}", path.display());
    path
}

fn newest(dir: &Path, file: &str) -> Option<PathBuf> {
    let names = fs::read_dir(dir).ok()?.filter_map(|entry| entry.ok()?.file_name().into_string().ok());
    let newest = names.filter(|name| dir.join(name).join(file).is_file()).max_by_key(|name| rank(name))?;
    Some(dir.join(newest).join(file))
}

/// Orders names like `android-33`, `android-33-ext4`, `android-36.1`, `36.0.0-rc5` and `36.0.0`.
/// A release comes after its own previews, and before its extensions.
fn rank(name: &str) -> (Vec<u32>, bool, u32) {
    let name = name.strip_prefix("android-").unwrap_or(name);
    let (version, suffix) = name.split_once('-').unwrap_or((name, ""));
    let number = |digits: &str| digits.parse::<u32>().ok();
    let is_release = suffix.is_empty() || suffix.starts_with("ext");
    let suffix = number(suffix.trim_start_matches(char::is_alphabetic)).unwrap_or(0);
    (version.split('.').filter_map(number).collect(), is_release, suffix)
}

/// The directory that has `javac` and `java`.
fn jdk() -> Option<PathBuf> {
    if let Some(home) = var("JAVA_HOME") {
        return Some(home.join("bin"));
    }
    // On macOS the ones in `/usr/bin` may be stubs that open an installer, so the system is asked for a JDK.
    let is_macos = cfg!(target_os = "macos");
    let java_home = if is_macos { Command::new("/usr/libexec/java_home").output().ok() } else { None };
    if let Some(output) = java_home.filter(|output| output.status.success()) {
        return Some(Path::new(String::from_utf8_lossy(&output.stdout).trim()).join("bin"));
    }
    let javac = format!("javac{EXE_SUFFIX}");
    let is_stub_dir = |dir: &Path| is_macos && dir == Path::new("/usr/bin");
    env::split_paths(&var("PATH")?).find(|dir| dir.join(&javac).is_file() && !is_stub_dir(dir))
}

/// Runs a JDK tool, and fails the build with what it printed if it fails.
fn run(command: &mut Command) {
    let tool = Path::new(command.get_program()).display().to_string();
    let output = command.output().unwrap_or_else(|error| panic!("could not run `{tool}`: {error}"));
    let printed = [output.stdout, output.stderr].concat();
    assert!(output.status.success(), "`{tool}` failed:\n{}", String::from_utf8_lossy(&printed));
}
