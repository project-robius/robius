use std::{env, fs, path::PathBuf};

const JAVA_FILES: &[&str] = &["src/sys/android/CameraResultCallback.java"];

fn main() {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();

    if target_os == "android" {
        for java_file in JAVA_FILES {
            println!("cargo:rerun-if-changed={java_file}");
        }

        let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
        let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());

        let android_jar_path =
            android_build::android_jar(None).expect("Failed to find android.jar");

        // We need MakepadActivity.class to compile against (for the interface)
        // Get the Makepad classes from the build environment
        let makepad_classes_dir = get_makepad_classes_dir();

        // Compile all .java files into .class files.
        let java_files: Vec<PathBuf> = JAVA_FILES
            .iter()
            .map(|f| manifest_dir.join(f))
            .collect();

        let mut java_build = android_build::JavaBuild::new();
        java_build
            .class_path(&android_jar_path)
            .classes_out_dir(&out_dir);

        // Add Makepad classes to classpath if available
        if let Some(makepad_dir) = makepad_classes_dir {
            java_build.class_path(makepad_dir);
        }

        assert!(
            java_build
                .files(&java_files)
                .compile()
                .expect("failed to acquire exit status for javac invocation")
                .success(),
            "javac invocation failed"
        );

        // Find all generated class files (including anonymous inner classes like $1, $2, etc.)
        let class_dir = out_dir.join("robius/camera");
        let class_files: Vec<PathBuf> = fs::read_dir(&class_dir)
            .expect("Failed to read class output directory")
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let path = entry.path();
                if path.extension().map_or(false, |ext| ext == "class") {
                    Some(path)
                } else {
                    None
                }
            })
            .collect();

        assert!(
            !class_files.is_empty(),
            "No class files found in {:?}",
            class_dir
        );

        let d8_jar_path = android_build::android_d8_jar(None).expect("Failed to find d8.jar");

        // Convert all class files to dex
        let mut java_run = android_build::JavaRun::new();
        java_run
            .class_path(d8_jar_path)
            .main_class("com.android.tools.r8.D8")
            .arg("--classpath")
            .arg(&android_jar_path)
            .arg("--output")
            .arg(&out_dir);

        for class_file in &class_files {
            java_run.arg(class_file);
        }

        assert!(
            java_run
                .run()
                .expect("failed to acquire exit status for java d8.jar invocation")
                .success(),
            "java d8.jar invocation failed"
        );
    }
}

/// Try to find Makepad's compiled classes directory.
/// This is needed because CameraResultCallback implements MakepadActivity.ActivityResultCallback.
fn get_makepad_classes_dir() -> Option<PathBuf> {
    // In a typical cargo makepad build, the classes are compiled to
    // target/makepad-android-apk/{crate}/apk/dev/makepad/android/
    // But we need the parent directory that contains the 'dev' folder.

    // Try to find it via environment variable or common paths
    if let Ok(dir) = env::var("MAKEPAD_ANDROID_CLASSES") {
        let path = PathBuf::from(dir);
        if path.exists() {
            return Some(path);
        }
    }

    // For now, we'll compile without the Makepad classes and rely on runtime linking.
    // The interface is simple enough that this should work.
    None
}
