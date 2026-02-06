mod callback;

use jni::objects::JValueGen;

use crate::{CameraPosition, Error, PhotoData, Result};

/// Check if camera capture is available on this device.
pub(crate) fn is_available() -> bool {
    robius_android_env::with_activity(|env, context| {
        // Check if the device has a camera
        let package_manager = env
            .call_method(
                context,
                "getPackageManager",
                "()Landroid/content/pm/PackageManager;",
                &[],
            )?
            .l()?;

        // Check for camera feature
        let camera_feature = env.new_string("android.hardware.camera.any")?;
        let has_camera = env
            .call_method(
                &package_manager,
                "hasSystemFeature",
                "(Ljava/lang/String;)Z",
                &[JValueGen::Object(&camera_feature.into())],
            )?
            .z()?;

        Ok(has_camera)
    })
    .map(|r: std::result::Result<bool, jni::errors::Error>| r.unwrap_or(false))
    .unwrap_or(false)
}

/// Capture a photo using the system camera UI.
///
/// This creates a CameraResultCallback and requests camera permission via
/// MakepadActivity. When permission is granted, the callback's onPermissionResult
/// is invoked, which then launches the camera intent. When the camera returns,
/// MakepadActivity dispatches the result to our callback which processes the
/// image and calls back to Rust.
pub(crate) fn capture_photo<F>(position: CameraPosition, callback: F) -> Result<()>
where
    F: FnOnce(Result<PhotoData>) + Send + 'static,
{
    let use_front_camera = matches!(position, CameraPosition::Front);

    let result = robius_android_env::with_activity(|env, activity| {
        // Get the callback class (loads DEX and registers native methods if needed)
        let callback_class = callback::get_callback_class(env)?;

        // Double-box the callback and convert to raw pointer
        let callback_boxed: Box<dyn FnOnce(Result<PhotoData>) + Send> = Box::new(callback);
        let callback_ptr = Box::into_raw(Box::new(callback_boxed)) as i64;

        // Create an instance of CameraResultCallback
        let callback_instance = callback::create_callback_instance(
            env,
            callback_class,
            activity,
            callback_ptr,
            use_front_camera,
        )?;

        // Request camera permission via MakepadActivity
        // When permission is granted/denied, our callback's onPermissionResult will be called
        // If granted, it will launch the camera intent itself
        let camera_permission = env.new_string("android.permission.CAMERA")?;
        let request_result = env.call_method(
            activity,
            "requestPermissionWithCallback",
            "(Ljava/lang/Object;Ljava/lang/String;)I",
            &[
                JValueGen::Object(&callback_instance),
                JValueGen::Object(&camera_permission.into()),
            ],
        );

        // Check if there was a Java exception
        if env.exception_check().unwrap_or(false) {
            env.exception_clear().ok();
            // Clean up the callback
            unsafe {
                let _ = Box::from_raw(callback_ptr as *mut Box<dyn FnOnce(Result<PhotoData>) + Send>);
            }
            return Err(Error::Unknown);
        }

        if request_result.is_err() {
            // Clean up the callback
            unsafe {
                let _ = Box::from_raw(callback_ptr as *mut Box<dyn FnOnce(Result<PhotoData>) + Send>);
            }
            return Err(Error::Unknown);
        }

        Ok(())
    });

    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(e),
        Err(e) => Err(Error::AndroidEnvironment(e)),
    }
}
