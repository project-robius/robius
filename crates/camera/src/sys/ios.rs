use std::cell::Cell;

use block2::{DynBlock, RcBlock};
use dispatch2::run_on_main;
use objc2::{
    define_class, msg_send, rc::Retained, runtime::{Bool, ProtocolObject}, ClassType, DeclaredClass,
    MainThreadMarker, MainThreadOnly,
};
use objc2_core_foundation::{CGPoint, CGRect};
use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeVideo};
use objc2_foundation::{NSDictionary, NSObject, NSObjectProtocol, NSString};
use objc2_ui_kit::{
    UIApplication, UIGraphicsBeginImageContextWithOptions, UIGraphicsEndImageContext,
    UIGraphicsGetImageFromCurrentImageContext, UIImage, UIImageOrientation,
    UIImagePickerController, UIImagePickerControllerCameraCaptureMode,
    UIImagePickerControllerCameraDevice, UIImagePickerControllerDelegate,
    UIImagePickerControllerSourceType, UINavigationControllerDelegate, UIViewController,
    UIWindow, UIWindowScene,
};

use crate::{CameraPosition, Error, PhotoData, Result};

/// JPEG compression quality (0.0 to 1.0)
const JPEG_QUALITY: f64 = 0.9;

/// Normalize image orientation by redrawing it with the correct orientation applied.
/// iOS camera images often have EXIF orientation metadata but the pixels aren't rotated.
/// This function creates a new image with the orientation baked into the pixels.
#[allow(deprecated)] // UIGraphicsBeginImageContextWithOptions is deprecated but still works
unsafe fn normalize_image_orientation(image: &UIImage) -> Option<Retained<UIImage>> {
    let orientation = image.imageOrientation();

    // If already upright, no need to redraw
    if orientation == UIImageOrientation::Up {
        return None;
    }

    let size = image.size();

    // Create a graphics context with the image size
    // false = not opaque (supports transparency)
    UIGraphicsBeginImageContextWithOptions(size, false, image.scale());

    // Draw the image - this applies the orientation transform
    let rect = CGRect::new(CGPoint::new(0.0, 0.0), size);
    image.drawInRect(rect);

    // Get the resulting image
    let normalized = UIGraphicsGetImageFromCurrentImageContext();

    // Clean up the context
    UIGraphicsEndImageContext();

    normalized
}

/// Check if camera capture is available on this device.
pub(crate) fn is_available() -> bool {
    if let Some(mtm) = MainThreadMarker::new() {
        // Safety: Checking source type availability is safe and has no side effects.
        unsafe {
            UIImagePickerController::isSourceTypeAvailable(
                UIImagePickerControllerSourceType::Camera,
                mtm,
            )
        }
    } else {
        // Can't check from non-main thread, conservatively return false
        // since we can't verify availability without main thread access
        false
    }
}

/// Capture a photo using the system camera UI.
pub(crate) fn capture_photo<F>(position: CameraPosition, callback: F) -> Result<()>
where
    F: FnOnce(Result<PhotoData>) + Send + 'static,
{
    // Get the media type string
    // Safety: AVMediaTypeVideo is a valid static string constant
    let Some(media_type) = (unsafe { AVMediaTypeVideo }) else {
        callback(Err(Error::Unknown));
        return Ok(());
    };

    // Check authorization status
    let status = unsafe { AVCaptureDevice::authorizationStatusForMediaType(media_type) };

    match status {
        AVAuthorizationStatus::Authorized => {
            // Already authorized, proceed to capture
            run_on_main(move |mtm| capture_photo_on_main(mtm, position, callback))
        }
        AVAuthorizationStatus::NotDetermined => {
            // Need to request permission
            let callback = std::sync::Arc::new(std::sync::Mutex::new(Some(callback)));
            let callback_clone = callback.clone();

            // The completion handler signature is (Bool) -> Void
            let request_block: RcBlock<dyn Fn(Bool)> = RcBlock::new(move |granted: Bool| {
                if granted.as_bool() {
                    // Permission granted, proceed to capture on main thread
                    let callback_inner = callback_clone.clone();
                    let _ = run_on_main(move |mtm| {
                        if let Some(cb) = callback_inner.lock().unwrap().take() {
                            let _ = capture_photo_on_main(mtm, position, cb);
                        }
                        Ok::<(), Error>(())
                    });
                } else {
                    // Permission denied
                    if let Some(cb) = callback_clone.lock().unwrap().take() {
                        cb(Err(Error::PermissionDenied));
                    }
                }
            });

            unsafe {
                AVCaptureDevice::requestAccessForMediaType_completionHandler(
                    media_type,
                    &request_block,
                );
            }
            Ok(())
        }
        AVAuthorizationStatus::Denied | AVAuthorizationStatus::Restricted => {
            // Permission denied or restricted
            callback(Err(Error::PermissionDenied));
            Ok(())
        }
        _ => {
            // Unknown status
            callback(Err(Error::Unknown));
            Ok(())
        }
    }
}

fn capture_photo_on_main<F>(
    mtm: MainThreadMarker,
    position: CameraPosition,
    callback: F,
) -> Result<()>
where
    F: FnOnce(Result<PhotoData>) + Send + 'static,
{
    // Check if camera is available
    // Safety: Checking source type availability is safe and has no side effects.
    if !unsafe {
        UIImagePickerController::isSourceTypeAvailable(
            UIImagePickerControllerSourceType::Camera,
            mtm,
        )
    } {
        callback(Err(Error::CameraUnavailable));
        return Ok(());
    }

    // Map position to camera device
    let camera_device = match position {
        CameraPosition::Back => UIImagePickerControllerCameraDevice::Rear,
        CameraPosition::Front => UIImagePickerControllerCameraDevice::Front,
    };

    // Check if the requested camera device is available
    // Safety: Checking camera device availability is safe and has no side effects.
    if !unsafe { UIImagePickerController::isCameraDeviceAvailable(camera_device, mtm) } {
        callback(Err(Error::RequestedCameraUnavailable));
        return Ok(());
    }

    // Create and configure the image picker controller
    // Safety: We're creating and configuring the picker before presenting it.
    let picker = unsafe {
        let picker = UIImagePickerController::new(mtm);
        picker.setSourceType(UIImagePickerControllerSourceType::Camera);
        picker.setCameraDevice(camera_device);
        picker.setCameraCaptureMode(UIImagePickerControllerCameraCaptureMode::Photo);
        picker
    };

    // Create and set the delegate
    let delegate = ImagePickerDelegate::new(mtm, callback);
    let delegate_proto: Retained<ProtocolObject<dyn UIImagePickerControllerDelegate>> =
        ProtocolObject::from_retained(delegate);

    unsafe {
        // The delegate is a weak reference, so we need to keep it alive
        // We'll store it in a static or leak it for the duration of the picker
        let delegate_ref: &ProtocolObject<dyn UIImagePickerControllerDelegate> = &delegate_proto;
        picker.setDelegate(Some(msg_send![delegate_ref, self]));
    }

    // We need to keep the delegate alive for the duration of the picker.
    // UIImagePickerController holds a weak reference to its delegate, so we must
    // prevent the delegate from being deallocated. We leak it here because:
    // 1. The delegate will be called exactly once (either didFinishPicking or didCancel)
    // 2. After the callback, the picker is dismissed and the delegate is no longer needed
    // 3. The leaked memory is small (~48 bytes) and happens once per capture
    // TODO: Consider using a static storage slot that gets reused across captures
    std::mem::forget(delegate_proto);

    // Get the root view controller to present from
    let Some(window) = (unsafe { get_key_window(mtm) }) else {
        return Err(Error::Unknown);
    };

    let Some(root_vc) = window.rootViewController() else {
        return Err(Error::Unknown);
    };

    // Present the picker
    let completion_block: RcBlock<dyn Fn()> = RcBlock::new(|| {
        #[cfg(feature = "log")]
        log::debug!("Camera picker presented");
    });

    unsafe {
        // Cast picker to UIViewController for presentation
        let picker_vc: &UIViewController = msg_send![&picker, self];
        root_vc.presentViewController_animated_completion(
            picker_vc,
            true,
            Some(&completion_block),
        );
    }

    Ok(())
}

/// Get the key window for presenting view controllers.
unsafe fn get_key_window(mtm: MainThreadMarker) -> Option<Retained<UIWindow>> {
    let app = UIApplication::sharedApplication(mtm);
    let scenes = app.connectedScenes();

    // Iterate through connected scenes to find the key window
    for scene in scenes.iter() {
        // Check if it's a UIWindowScene
        let is_window_scene: bool = msg_send![&scene, isKindOfClass: UIWindowScene::class()];
        if is_window_scene {
            let window_scene: Retained<UIWindowScene> = msg_send![&scene, self];
            if let Some(key_window) = window_scene.keyWindow() {
                return Some(key_window);
            }
        }
    }

    None
}

// Key for extracting the original image from the info dictionary
fn original_image_key() -> &'static NSString {
    // UIImagePickerControllerOriginalImage
    unsafe {
        extern "C" {
            static UIImagePickerControllerOriginalImage: &'static NSString;
        }
        UIImagePickerControllerOriginalImage
    }
}

/// Type alias for the callback function.
type Callback = Box<dyn FnOnce(Result<PhotoData>) + Send>;

/// Delegate for handling UIImagePickerController callbacks.
struct Ivars {
    callback: Cell<Option<Callback>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    struct ImagePickerDelegate;

    unsafe impl NSObjectProtocol for ImagePickerDelegate {}

    unsafe impl UINavigationControllerDelegate for ImagePickerDelegate {}

    unsafe impl UIImagePickerControllerDelegate for ImagePickerDelegate {
        #[unsafe(method(imagePickerController:didFinishPickingMediaWithInfo:))]
        #[allow(non_snake_case)]
        unsafe fn imagePickerController_didFinishPickingMediaWithInfo(
            &self,
            picker: &UIImagePickerController,
            info: &NSDictionary,
        ) {
            // Dismiss the picker first
            dismiss_picker(picker);

            // Extract the callback
            let callback = self.ivars().callback.take();

            // Get the original image from the info dictionary
            let result = (|| {
                let image_key = original_image_key();
                let image_obj = info.objectForKey(image_key).ok_or(Error::ProcessingFailed)?;

                // Cast to UIImage
                let image: &UIImage = msg_send![&image_obj, self];

                // Normalize the image orientation (iOS camera images often need rotation)
                let normalized_image = unsafe { normalize_image_orientation(image) };
                let final_image: &UIImage = normalized_image.as_deref().unwrap_or(image);

                // Get image dimensions from the final image
                let size = final_image.size();
                let width = size.width as u32;
                let height = size.height as u32;

                // Convert to JPEG data
                let jpeg_data = final_image
                    .jpeg_representation(JPEG_QUALITY)
                    .ok_or(Error::ProcessingFailed)?;

                // Copy the bytes using to_vec()
                let data = jpeg_data.to_vec();

                Ok(PhotoData::new(data, width, height))
            })();

            if let Some(cb) = callback {
                cb(result);
            }
        }

        #[unsafe(method(imagePickerControllerDidCancel:))]
        #[allow(non_snake_case)]
        unsafe fn imagePickerControllerDidCancel(&self, picker: &UIImagePickerController) {
            // Dismiss the picker
            dismiss_picker(picker);

            // Call the callback with cancelled error
            if let Some(cb) = self.ivars().callback.take() {
                cb(Err(Error::Cancelled));
            }
        }
    }
);

impl ImagePickerDelegate {
    fn new<F>(mtm: MainThreadMarker, callback: F) -> Retained<Self>
    where
        F: FnOnce(Result<PhotoData>) + Send + 'static,
    {
        let this = Self::alloc(mtm).set_ivars(Ivars {
            callback: Cell::new(Some(Box::new(callback))),
        });
        unsafe { msg_send![super(this), init] }
    }
}

/// Dismiss the image picker controller.
unsafe fn dismiss_picker(picker: &UIImagePickerController) {
    let completion: Option<&DynBlock<dyn Fn()>> = None;
    let picker_vc: &UIViewController = msg_send![picker, self];
    picker_vc.dismissViewControllerAnimated_completion(true, completion);
}
