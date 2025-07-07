use std::{env, path::PathBuf};

const JAVA_FILE_RELATIVE_PATH: &str = "src/sys/android/LocationCallback.java";

fn main() {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();

    if target_os == "android" {
        println!("cargo:rerun-if-changed={JAVA_FILE_RELATIVE_PATH}");

        let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
        let java_file =
            PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join(JAVA_FILE_RELATIVE_PATH);

        let android_jar_path =
            android_build::android_jar(None).expect("Failed to find android.jar");

        // Compile the .java file into a .class file.
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

        let class_file = out_dir
            .join("robius")
            .join("location")
            .join("LocationCallback.class");

        let d8_jar_path = android_build::android_d8_jar(None).expect("Failed to find d8.jar");

        assert!(
            android_build::JavaRun::new()
                .class_path(d8_jar_path)
                .main_class("com.android.tools.r8.D8")
                .arg("--classpath")
                .arg(android_jar_path)
                .arg("--output")
                .arg(&out_dir)
                .arg(&class_file)
                .run()
                .expect("failed to acquire exit status for java d8.jar invocation")
                .success(),
            "java d8.jar invocation failed"
        );
    }
}
