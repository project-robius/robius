/* This file is compiled by build.rs. */

package robius.location;

import android.app.Activity;
import android.app.Fragment;
import android.app.FragmentManager;
import android.content.pm.PackageManager;
import android.os.Bundle;

/*
 * Headless fragment for the runtime permission request. Fragment.requestPermissions sends the result
 * straight back here, so we don't have to touch the activity's own onRequestPermissionsResult. Nothing
 * here blocks, since the Rust caller might be on a thread that must never wait on the Android UI thread.
 */
public class LocationPermissionFragment extends Fragment {
    // Must be <= 0xffff: android.app.Fragment encodes its index in the upper 16 bits of the code.
    private static final int REQUEST_CODE = 0x4c6f;
    private static final String TAG = "robius.location.LocationPermissionFragment";

    // Raw pointer to a Rust `Arc<Shared>` ref. 0 means no callback — a recreated instance, or we already delivered.
    private long callbackPtr;
    private String[] perms;
    private boolean launched;

    /*
     * The name and signature of this function must be kept in sync with
     * `PERMISSION_CALLBACK_NAME` and `PERMISSION_CALLBACK_SIGNATURE` in `callback.rs`.
     */
    private static native void rustPermissionCallback(long callbackPtr, boolean granted);

    // The framework needs this to recreate the fragment (e.g. after the process dies). A recreated
    // one has no callback pointer and just removes itself in `onResume`.
    public LocationPermissionFragment() {
        this.callbackPtr = 0;
        this.perms = null;
        this.launched = false;
    }

    private LocationPermissionFragment(long callbackPtr, String[] perms) {
        this.callbackPtr = callbackPtr;
        this.perms = perms;
        this.launched = false;
    }

    // Attaches the fragment and asks for `perms`. Doesn't block — posts to the UI thread and returns.
    // Java owns `callbackPtr` now and always delivers it exactly once (from the fragment's result or
    // `onDestroy`, or from a failure here).
    public static void show(Activity activity, long callbackPtr, String[] perms) {
        activity.runOnUiThread(() -> showOnUiThread(activity, callbackPtr, perms));
    }

    private static void showOnUiThread(Activity activity, long callbackPtr, String[] perms) {
        // Once the fragment owns the pointer, only it delivers the callback, so we deliver just once.
        // Before that, we deliver here if anything fails.
        boolean fragmentOwnsPtr = false;
        try {
            if (activity.isFinishing() || activity.isDestroyed()) {
                rustPermissionCallback(callbackPtr, false);
                return;
            }

            FragmentManager fm = activity.getFragmentManager();
            Fragment existing = fm.findFragmentByTag(TAG);
            if (existing instanceof LocationPermissionFragment) {
                if (((LocationPermissionFragment) existing).hasCallback()) {
                    // Another request is already in flight; drop this duplicate.
                    rustPermissionCallback(callbackPtr, false);
                    return;
                }
                // Remove any old leftover before adding a fresh one.
                fm.beginTransaction().remove(existing).commitNowAllowingStateLoss();
            }

            LocationPermissionFragment fragment = new LocationPermissionFragment(callbackPtr, perms);
            fragmentOwnsPtr = true;
            // Commit async so this can't throw right here and race the fragment's own delivery.
            fm.beginTransaction().add(fragment, TAG).commitAllowingStateLoss();
        } catch (Throwable t) {
            if (!fragmentOwnsPtr) {
                rustPermissionCallback(callbackPtr, false);
            }
            // Else the fragment owns the pointer and its onDestroy will deliver.
        }
    }

    // Removes any in-flight fragment (called when the `Manager` is dropped mid-request). Its
    // `onDestroy` delivers "denied" and frees the Rust ref (does nothing if a result already came in).
    public static void removeExisting(Activity activity) {
        activity.runOnUiThread(() -> {
            try {
                if (activity.isFinishing() || activity.isDestroyed()) {
                    return;
                }
                FragmentManager fm = activity.getFragmentManager();
                Fragment existing = fm.findFragmentByTag(TAG);
                if (existing instanceof LocationPermissionFragment) {
                    fm.beginTransaction().remove(existing).commitNowAllowingStateLoss();
                }
            } catch (Throwable ignored) {}
        });
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

        if (!launched && perms != null) {
            launched = true;
            // Use the Fragment's own `requestPermissions` so the result routes back to us.
            requestPermissions(perms, REQUEST_CODE);
        }
    }

    @Override
    public void onRequestPermissionsResult(int requestCode, String[] permissions, int[] grantResults) {
        if (requestCode != REQUEST_CODE) {
            return;
        }

        // Granted if ANY requested permission was granted (e.g. "Approximate" grants COARSE, denies
        // FINE). An empty array (an interrupted request) counts as denied.
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
        // and the Rust ref gets freed (does nothing if already delivered, since `deliver` claims it).
        deliver(false);
    }

    private synchronized boolean hasCallback() {
        return callbackPtr != 0;
    }

    private synchronized void deliver(boolean granted) {
        long ptr = callbackPtr;
        callbackPtr = 0;
        if (ptr != 0) {
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
