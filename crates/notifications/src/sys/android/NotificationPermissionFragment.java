/* This file is compiled by build.rs. */

package robius.notifications;

import android.app.Activity;
import android.app.Fragment;
import android.app.FragmentManager;
import android.app.NotificationManager;
import android.content.Context;
import android.content.pm.PackageManager;
import android.os.Build;
import android.os.Bundle;

/*
 * Headless fragment for the POST_NOTIFICATIONS runtime permission request (Android 13+).
 * Fragment.requestPermissions sends the result straight back here, so we don't have to touch the
 * activity's own onRequestPermissionsResult. Nothing here blocks, since the Rust caller might be
 * on a thread that must never wait on the Android UI thread.
 */
public class NotificationPermissionFragment extends Fragment {
    // Must be <= 0xffff: android.app.Fragment encodes its index in the upper 16 bits of the code.
    private static final int REQUEST_CODE = 0x4e74;
    private static final String TAG = "robius.notifications.NotificationPermissionFragment";
    private static final String POST_NOTIFICATIONS = "android.permission.POST_NOTIFICATIONS";

    // True while a request's fragment is in flight (or about to be). The by-tag lookup alone
    // can miss one, since commitAllowingStateLoss only enqueues the add. Only touched on the
    // UI thread.
    private static boolean requestInFlight = false;

    // Raw pointer to a boxed Rust callback. 0 means no callback - a framework-recreated
    // instance, or we already delivered.
    private long callbackPtr;
    private boolean launched;

    /*
     * The name and signature of this function must be kept in sync with
     * `PERMISSION_CALLBACK_NAME` and `PERMISSION_CALLBACK_SIGNATURE` in `class.rs`.
     */
    static native void rustPermissionCallback(long callbackPtr, boolean granted);

    // The framework needs this to recreate the fragment (e.g. after the process dies). A recreated
    // one has no callback pointer and just removes itself in `onResume`.
    public NotificationPermissionFragment() {
        this.callbackPtr = 0;
        this.launched = false;
    }

    private NotificationPermissionFragment(long callbackPtr) {
        this.callbackPtr = callbackPtr;
        this.launched = false;
    }

    // Reports the standing permission state, or shows the system prompt on Android 13+.
    // Doesn't block - posts to the UI thread and returns. Java owns `callbackPtr` now and
    // always delivers it exactly once.
    public static void request(Activity activity, long callbackPtr) {
        activity.runOnUiThread(() -> requestOnUiThread(activity, callbackPtr));
    }

    private static void requestOnUiThread(Activity activity, long callbackPtr) {
        // Once the fragment owns the pointer, only it delivers the callback, so we deliver just
        // once. Before that, we deliver here if anything fails.
        boolean fragmentOwnsPtr = false;
        try {
            // Below Android 13 there's no permission prompt; the user's standing setting is all
            // there is.
            if (Build.VERSION.SDK_INT < 33) {
                rustPermissionCallback(callbackPtr, notificationsEnabled(activity));
                return;
            }
            if (activity.checkSelfPermission(POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED
                    || notificationsEnabled(activity)) {
                rustPermissionCallback(callbackPtr, true);
                return;
            }
            if (activity.isFinishing() || activity.isDestroyed()) {
                rustPermissionCallback(callbackPtr, false);
                return;
            }

            if (requestInFlight) {
                // Another request is already in flight; drop this duplicate.
                rustPermissionCallback(callbackPtr, false);
                return;
            }

            FragmentManager fm = activity.getFragmentManager();
            Fragment existing = fm.findFragmentByTag(TAG);
            if (existing instanceof NotificationPermissionFragment) {
                if (((NotificationPermissionFragment) existing).hasCallback()) {
                    // Backstop for the same in-flight case; shouldn't happen with the flag.
                    rustPermissionCallback(callbackPtr, false);
                    return;
                }
                // Remove any old leftover before adding a fresh one.
                fm.beginTransaction().remove(existing).commitNowAllowingStateLoss();
            }

            NotificationPermissionFragment fragment =
                    new NotificationPermissionFragment(callbackPtr);
            requestInFlight = true;
            fragmentOwnsPtr = true;
            // Commit async so this can't throw right here and race the fragment's own delivery.
            fm.beginTransaction().add(fragment, TAG).commitAllowingStateLoss();
        } catch (Throwable t) {
            if (!fragmentOwnsPtr) {
                requestInFlight = false;
                rustPermissionCallback(callbackPtr, false);
            }
            // Else the fragment owns the pointer and its onDestroy will deliver.
        }
    }

    // The standing permission state, same checks as the already-granted paths in
    // `requestOnUiThread`, but without ever prompting. Used for provisional requests.
    public static boolean currentPermissionState(Context context) {
        try {
            if (Build.VERSION.SDK_INT >= 33
                    && context.checkSelfPermission(POST_NOTIFICATIONS)
                            == PackageManager.PERMISSION_GRANTED) {
                return true;
            }
            return notificationsEnabled(context);
        } catch (Throwable t) {
            return false;
        }
    }

    private static boolean notificationsEnabled(Context context) {
        NotificationManager manager =
                (NotificationManager) context.getSystemService(Context.NOTIFICATION_SERVICE);
        return manager != null && manager.areNotificationsEnabled();
    }

    @Override
    public void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        // Survive rotation as the same instance, so the pending request keeps its callback pointer.
        setRetainInstance(true);
    }

    @Override
    public void onResume() {
        super.onResume();

        // A framework-recreated instance has no callback, so it shouldn't stick around.
        if (callbackPtr == 0) {
            removeSelf();
            return;
        }

        if (!launched) {
            launched = true;
            // Use the Fragment's own `requestPermissions` so the result routes back to us.
            requestPermissions(new String[] { POST_NOTIFICATIONS }, REQUEST_CODE);
        }
    }

    @Override
    public void onRequestPermissionsResult(int requestCode, String[] permissions, int[] grantResults) {
        if (requestCode != REQUEST_CODE) {
            return;
        }

        // An empty array (an interrupted request) counts as denied.
        boolean granted = false;
        for (int result : grantResults) {
            if (result == PackageManager.PERMISSION_GRANTED) {
                granted = true;
                break;
            }
        }

        deliver(granted);
        removeSelf();
    }

    @Override
    public void onDestroy() {
        super.onDestroy();

        // A config change recreates us from the retained instance, so don't fire a spurious "denied".
        Activity activity = getActivity();
        if (activity != null && activity.isChangingConfigurations()) {
            return;
        }

        // Real teardown before a result: deliver "denied" so the callback always runs exactly once
        // and the boxed Rust callback gets freed (does nothing if already delivered).
        deliver(false);
    }

    private synchronized boolean hasCallback() {
        return callbackPtr != 0;
    }

    private synchronized void deliver(boolean granted) {
        long ptr = callbackPtr;
        callbackPtr = 0;
        if (ptr != 0) {
            // This fragment's request is done, so the next one may start.
            requestInFlight = false;
            rustPermissionCallback(ptr, granted);
        }
    }

    private void removeSelf() {
        FragmentManager fm = getFragmentManager();
        if (fm != null) {
            fm.beginTransaction().remove(this).commitAllowingStateLoss();
        }
    }
}
