/* This file is compiled by build.rs and loaded dynamically at runtime. */

package robius.camera;

import android.app.Activity;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.graphics.Bitmap;
import android.graphics.BitmapFactory;
import android.graphics.Matrix;
import android.media.ExifInterface;
import android.net.Uri;
import android.provider.MediaStore;
import android.util.Log;

import java.io.ByteArrayOutputStream;
import java.io.InputStream;
import java.lang.reflect.Method;

/**
 * Callback class for handling camera capture results.
 *
 * This class is dynamically loaded at runtime. MakepadActivity invokes the
 * onActivityResult method via reflection when the camera intent completes.
 * It also handles permission requests via onPermissionResult.
 */
public class CameraResultCallback {
    private static final String TAG = "RobiusCamera";
    private static final String CAMERA_PERMISSION = "android.permission.CAMERA";

    private final Activity activity;
    private final long callbackPtr;
    private final boolean useFrontCamera;
    private int activityRequestCode = -1;

    /**
     * Native method to call the Rust callback.
     * This method is registered at runtime by robius-camera via JNI.
     */
    private native void rustCallback(long pointer, int resultCode, byte[] jpegData, int width, int height);

    public CameraResultCallback(Activity activity, long callbackPtr, boolean useFrontCamera) {
        this.activity = activity;
        this.callbackPtr = callbackPtr;
        this.useFrontCamera = useFrontCamera;
        Log.d(TAG, "CameraResultCallback created with callbackPtr=" + callbackPtr);
    }

    // For backwards compatibility - constructor without useFrontCamera
    public CameraResultCallback(Activity activity, long callbackPtr) {
        this(activity, callbackPtr, false);
    }

    /**
     * Called by MakepadActivity when permission result is received.
     */
    public void onPermissionResult(boolean granted) {
        Log.d(TAG, "onPermissionResult: granted=" + granted);
        if (granted) {
            // Permission granted - launch camera
            launchCamera();
        } else {
            // Permission denied
            notifyPermissionDenied();
        }
    }

    /**
     * Launch the camera intent.
     */
    public void launchCamera() {
        try {
            Log.d(TAG, "launchCamera: creating intent");
            Intent intent = new Intent(MediaStore.ACTION_IMAGE_CAPTURE);

            if (useFrontCamera) {
                intent.putExtra("android.intent.extras.CAMERA_FACING", 1);
            }

            // Check if there's a camera app
            if (intent.resolveActivity(activity.getPackageManager()) == null) {
                Log.e(TAG, "launchCamera: no camera app available");
                notifyError();
                return;
            }

            // Register for activity result
            activityRequestCode = registerForActivityResult();
            if (activityRequestCode < 0) {
                Log.e(TAG, "launchCamera: failed to register callback");
                notifyError();
                return;
            }

            Log.d(TAG, "launchCamera: starting activity with requestCode=" + activityRequestCode);
            activity.startActivityForResult(intent, activityRequestCode);
        } catch (Exception e) {
            Log.e(TAG, "launchCamera: exception", e);
            notifyError();
        }
    }

    private int registerForActivityResult() {
        try {
            // Call MakepadActivity.registerActivityResultCallback(this)
            Method method = activity.getClass().getMethod("registerActivityResultCallback", Object.class);
            return (int) method.invoke(null, this);
        } catch (Exception e) {
            Log.e(TAG, "registerForActivityResult: exception", e);
            return -1;
        }
    }

    public void onActivityResult(int resultCode, Intent data) {
        Log.d(TAG, "onActivityResult: resultCode=" + resultCode);

        if (resultCode == Activity.RESULT_OK) {
            processSuccessfulCapture(data);
        } else if (resultCode == Activity.RESULT_CANCELED) {
            Log.d(TAG, "onActivityResult: user cancelled");
            notifyCancelled();
        } else {
            Log.e(TAG, "onActivityResult: unknown result code");
            notifyError();
        }
    }

    private void processSuccessfulCapture(Intent data) {
        try {
            Bitmap bitmap = null;
            int rotation = 0;

            // Try to get the full-size image from data URI first
            if (data != null && data.getData() != null) {
                Uri imageUri = data.getData();
                Log.d(TAG, "processSuccessfulCapture: got URI " + imageUri);

                // Get rotation from EXIF
                try {
                    InputStream exifStream = activity.getContentResolver().openInputStream(imageUri);
                    if (exifStream != null) {
                        ExifInterface exif = new ExifInterface(exifStream);
                        rotation = getRotationFromExif(exif);
                        exifStream.close();
                    }
                } catch (Exception e) {
                    Log.w(TAG, "processSuccessfulCapture: failed to read EXIF", e);
                }

                // Load the bitmap
                try {
                    InputStream inputStream = activity.getContentResolver().openInputStream(imageUri);
                    if (inputStream != null) {
                        bitmap = BitmapFactory.decodeStream(inputStream);
                        inputStream.close();
                    }
                } catch (Exception e) {
                    Log.w(TAG, "processSuccessfulCapture: failed to load from URI", e);
                }
            }

            // Fall back to thumbnail from extras (this is low quality but works as fallback)
            if (bitmap == null && data != null && data.getExtras() != null) {
                Log.d(TAG, "processSuccessfulCapture: falling back to thumbnail");
                bitmap = (Bitmap) data.getExtras().get("data");
            }

            if (bitmap != null) {
                // Apply rotation if needed
                if (rotation != 0) {
                    Log.d(TAG, "processSuccessfulCapture: applying rotation " + rotation);
                    Matrix matrix = new Matrix();
                    matrix.postRotate(rotation);
                    Bitmap rotated = Bitmap.createBitmap(bitmap, 0, 0,
                        bitmap.getWidth(), bitmap.getHeight(), matrix, true);
                    if (rotated != bitmap) {
                        bitmap.recycle();
                        bitmap = rotated;
                    }
                }

                // Convert to JPEG
                ByteArrayOutputStream baos = new ByteArrayOutputStream();
                bitmap.compress(Bitmap.CompressFormat.JPEG, 90, baos);
                byte[] jpegData = baos.toByteArray();

                int width = bitmap.getWidth();
                int height = bitmap.getHeight();

                Log.d(TAG, "processSuccessfulCapture: success " + width + "x" + height + ", " + jpegData.length + " bytes");
                notifySuccess(jpegData, width, height);
                bitmap.recycle();
            } else {
                Log.e(TAG, "processSuccessfulCapture: no bitmap obtained");
                notifyError();
            }
        } catch (Exception e) {
            Log.e(TAG, "processSuccessfulCapture: exception", e);
            notifyError();
        }
    }

    private int getRotationFromExif(ExifInterface exif) {
        int orientation = exif.getAttributeInt(
            ExifInterface.TAG_ORIENTATION,
            ExifInterface.ORIENTATION_NORMAL
        );

        switch (orientation) {
            case ExifInterface.ORIENTATION_ROTATE_90:
                return 90;
            case ExifInterface.ORIENTATION_ROTATE_180:
                return 180;
            case ExifInterface.ORIENTATION_ROTATE_270:
                return 270;
            default:
                return 0;
        }
    }

    private void notifySuccess(byte[] jpegData, int width, int height) {
        if (callbackPtr != 0) {
            try {
                rustCallback(callbackPtr, 0, jpegData, width, height);
            } catch (Exception e) {
                Log.e(TAG, "notifySuccess: exception calling rustCallback", e);
            }
        }
    }

    private void notifyCancelled() {
        if (callbackPtr != 0) {
            try {
                rustCallback(callbackPtr, 1, null, 0, 0);
            } catch (Exception e) {
                Log.e(TAG, "notifyCancelled: exception calling rustCallback", e);
            }
        }
    }

    private void notifyError() {
        if (callbackPtr != 0) {
            try {
                rustCallback(callbackPtr, 2, null, 0, 0);
            } catch (Exception e) {
                Log.e(TAG, "notifyError: exception calling rustCallback", e);
            }
        }
    }

    private void notifyPermissionDenied() {
        if (callbackPtr != 0) {
            try {
                rustCallback(callbackPtr, 3, null, 0, 0);
            } catch (Exception e) {
                Log.e(TAG, "notifyPermissionDenied: exception calling rustCallback", e);
            }
        }
    }
}
