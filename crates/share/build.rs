use std::{env, path::PathBuf};

const JAVA_FILE_RELATIVE_PATH: &str = "src/sys/android/ShareSheet.java";

fn main() {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();

    if target_os == "android" {
        println!("cargo:rerun-if-changed={JAVA_FILE_RELATIVE_PATH}");

        let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
        let java_file =
            PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join(JAVA_FILE_RELATIVE_PATH);

        let android_jar_path =
            android_build::android_jar(None).expect("Failed to find android.jar");

        assert!(
            android_build::JavaBuild::new()
                .class_path(android_jar_path.clone())
                .classes_out_dir(out_dir.clone())
                .file(java_file)
                .compile()
                .expect("failed to acquire exit status for javac invocation")
                .success(),
            "javac invocation failed"
        );

        let class_dir = out_dir
            .join("robius")
            .join("share");
        let mut class_files = std::fs::read_dir(&class_dir)
            .expect("failed to read javac output directory")
            .map(|entry| entry.expect("failed to read javac output entry").path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "class"))
            .collect::<Vec<_>>();
        class_files.sort();

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
