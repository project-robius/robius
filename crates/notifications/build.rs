use std::{env, fs, path::PathBuf};

const JAVA_FILES_RELATIVE_PATHS: &[&str] = &[
    "src/sys/android/Notifications.java",
    "src/sys/android/NotificationPermissionFragment.java",
];

fn main() {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();

    if target_os == "android" {
        let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
        let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());

        let android_jar_path =
            android_build::android_jar(None).expect("Failed to find android.jar");

        // Compile all `.java` files into `.class` files.
        let mut java_build = android_build::JavaBuild::new();
        java_build
            .class_path(android_jar_path.clone())
            .classes_out_dir(out_dir.clone());
        for relative_path in JAVA_FILES_RELATIVE_PATHS {
            println!("cargo:rerun-if-changed={relative_path}");
            java_build.file(manifest_dir.join(relative_path));
        }
        assert!(
            java_build
                .compile()
                .expect("failed to acquire exit status for javac invocation")
                .success(),
            "javac invocation failed"
        );

        // Collect every generated `.class` file (there may be more than one per source file, e.g.
        // inner and synthetic classes) so they all land in the single `classes.dex`.
        let classes_dir = out_dir.join("robius").join("notifications");
        let class_files: Vec<PathBuf> = fs::read_dir(&classes_dir)
            .expect("failed to read compiled classes directory")
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension().is_some_and(|ext| ext == "class"))
            .collect();
        assert!(
            !class_files.is_empty(),
            "no compiled .class files found in {}",
            classes_dir.display()
        );

        let d8_jar_path = android_build::android_d8_jar(None).expect("Failed to find d8.jar");

        let mut d8 = android_build::JavaRun::new();
        d8.class_path(d8_jar_path)
            .main_class("com.android.tools.r8.D8")
            .arg("--classpath")
            .arg(android_jar_path)
            .arg("--min-api")
            .arg("26")
            .arg("--output")
            .arg(&out_dir);
        for class_file in &class_files {
            d8.arg(class_file);
        }
        assert!(
            d8.run()
                .expect("failed to acquire exit status for java d8.jar invocation")
                .success(),
            "java d8.jar invocation failed"
        );
    }
}
