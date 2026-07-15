/* This file is compiled by build.rs. */

package robius.location;

import android.location.Criteria;
import android.location.Location;
import android.location.LocationListener;
import android.location.LocationManager;
import android.os.Build;
import android.os.Handler;
import android.os.Looper;
import java.util.function.Consumer;
import java.util.List;
import java.util.concurrent.Executor;

/*
 * `Consumer<Location>` is implemented for `LocationManager.getCurrentLocation`.
 * `LocationListener` is implemented for `LocationManager.requestLocationUpdates`.
 */

public class LocationCallback implements Consumer<Location>, LocationListener {
    // If a cached fix is at least this recent, just use it instead of waiting for a new one.
    private static final long FRESH_ENOUGH_MILLIS = 60_000L;
    // How long to wait for a new fix before giving up and using the cached one.
    private static final long GRACE_MILLIS = 4_000L;
    // On API 26-29 requestSingleUpdate has no timeout of its own, so we add one.
    private static final long CLASSIC_GIVE_UP_MILLIS = 10_000L;
    // Just in case getCurrentLocation (API 30+) never calls us back.
    private static final long CURRENT_BACKSTOP_MILLIS = 45_000L;

    // Pointer to the Rust-side `Shared`. It's a thin pointer, so it fits in one `long`.
    private long sharedPtr;
    // volatile because Drop reads/sets these from another thread.
    private volatile boolean executing;
    private volatile boolean doNotExecute;

    private Handler handler;

    // State for a single-location request. If a new fix is slow we fall back to the newest cached
    // one, and we never hand back a fix older than one we already showed.
    private boolean oneShotMode;        // only used on API 26-29, where the fix comes via onLocationChanged
    private Location fallback;          // newest cached fix when the request started (may be null)
    private long lastDeliveredTime;     // getTime() of the newest fix we've handed back, or -1
    private Runnable graceRunnable;
    private boolean gracePending;
    private Runnable giveUpRunnable;
    private boolean giveUpPending;

    /*
     * The name and signature of this function must be kept in sync with `RUST_CALLBACK_NAME`, and
     * `RUST_CALLBACK_SIGNATURE` respectively.
     */
    private native void rustCallback(long sharedPtr, Location location);

    public LocationCallback(long sharedPtr) {
        this.sharedPtr = sharedPtr;
        this.executing = false;
        this.doNotExecute = false;
    }

    public boolean isExecuting() {
        return this.executing;
    }

    public void disableExecution() {
        this.doNotExecute = true;
        cancelGrace();
        cancelGiveUp();
    }

    // Hand a location (or null) to Rust. `executing` lets Drop wait for us instead of freeing us mid-call.
    private void deliver(Location location) {
        this.executing = true;
        if (!this.doNotExecute) {
            rustCallback(this.sharedPtr, location);
        }
        this.executing = false;
    }

    // Only hand it back if it's newer than the last one, so the location never jumps back in time.
    private void deliverIfNewer(Location location) {
        if (location == null || location.getTime() <= lastDeliveredTime) {
            return;
        }
        lastDeliveredTime = location.getTime();
        deliver(location);
    }

    private static Location newer(Location a, Location b) {
        if (a == null) return b;
        if (b == null) return a;
        return a.getTime() >= b.getTime() ? a : b;
    }

    // Finish the request with the newer of the new fix and the cached one, or null (an error) if we got neither.
    private void resolveOneShot(Location fresh) {
        Location best = newer(fresh, fallback);
        if (best != null) {
            deliverIfNewer(best);
        } else if (lastDeliveredTime < 0L) {
            deliver(null);
        }
    }

    public void accept(Location location) {
        // This is the getCurrentLocation result (API 30+); the request is done.
        cancelGrace();
        cancelGiveUp();
        resolveOneShot(location);
    }

    public void onLocationChanged(Location location) {
        if (oneShotMode) {
            cancelGrace();
            cancelGiveUp();
            resolveOneShot(location);
        } else {
            deliver(location); // continuous updates: just pass every fix along
        }
    }

    // NOTE: Technically implementing this function shouldn't be necessary as it has a default implementation
    // but if we don't we get the following error 🤷:
    //
    // NoClassDefFoundError for android/location/LocationListener$-CC
    public void onLocationChanged(List<Location> locations) {
        this.executing = true;
        if (!this.doNotExecute) {
                for (Location location : locations) {
                    rustCallback(this.sharedPtr, location);
                }
        }
        this.executing = false;
    }

    // These need empty overrides too, same reason as onLocationChanged(List) above: otherwise d8
    // generates $-CC stubs that crash when Android calls them.
    public void onProviderEnabled(String provider) {}
    public void onProviderDisabled(String provider) {}
    public void onStatusChanged(String provider, int status, android.os.Bundle extras) {}
    public void onFlushComplete(int requestCode) {}

    /*
     * We fetch location with the newest API the device supports (checked with SDK_INT), down to API
     * 26. Each version-specific call lives in its own method so older devices never try to verify the
     * newer ones.
     */

    // preciseGranted means we have FINE permission. Only then is a new fix fast enough to wait for.
    public boolean requestSingleLocation(LocationManager manager, boolean preciseGranted) {
        cancelGrace();
        cancelGiveUp();
        lastDeliveredTime = -1L;
        fallback = bestLastKnown(manager);

        boolean started;
        try {
            started = startFreshFix(manager, preciseGranted);
        } catch (RuntimeException ignored) {
            started = false; // e.g. permission got revoked mid-call; we'll fall back to the cached fix
        }

        if (fallback != null) {
            // Show the cached fix right away if it's recent, there's no new request coming, or we
            // only have coarse permission. Otherwise wait a moment to see if a new one beats it.
            long age = System.currentTimeMillis() - fallback.getTime();
            boolean showNow = !started || !preciseGranted || age < FRESH_ENOUGH_MILLIS;
            scheduleGrace(showNow ? 0L : GRACE_MILLIS);
        }
        if (started) {
            long timeout = Build.VERSION.SDK_INT >= Build.VERSION_CODES.R
                    ? CURRENT_BACKSTOP_MILLIS
                    : CLASSIC_GIVE_UP_MILLIS;
            scheduleGiveUp(manager, timeout);
        }
        return started || fallback != null;
    }

    // Kick off a request for a new fix, trying the newest API first. Returns false if no provider is on.
    private boolean startFreshFix(LocationManager manager, boolean preciseGranted) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            getCurrentLocation(manager, LocationManager.FUSED_PROVIDER); // API 31+
            return true;
        }
        String provider = bestProvider(manager, preciseGranted);
        if (provider == null) {
            return false;
        }
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            getCurrentLocation(manager, provider); // API 30
        } else {
            oneShotMode = true; // on 26-29 the fix arrives via onLocationChanged, which finishes the request
            manager.requestSingleUpdate(provider, this, Looper.getMainLooper());
        }
        return true;
    }

    // The most recent cached fix from any provider (null if there's none). FUSED_PROVIDER only exists on API 31+.
    private static Location bestLastKnown(LocationManager manager) {
        String[] providers = Build.VERSION.SDK_INT >= Build.VERSION_CODES.S
                ? new String[] { LocationManager.FUSED_PROVIDER, LocationManager.GPS_PROVIDER, LocationManager.NETWORK_PROVIDER }
                : new String[] { LocationManager.GPS_PROVIDER, LocationManager.NETWORK_PROVIDER };
        Location best = null;
        for (String provider : providers) {
            try {
                best = newer(best, manager.getLastKnownLocation(provider));
            } catch (SecurityException | IllegalArgumentException ignored) {
                // this provider isn't allowed or isn't on the device; skip it
            }
        }
        return best;
    }

    // After a delay, hand back the cached fix — unless a new one shows up first and cancels this.
    private void scheduleGrace(long delayMillis) {
        cancelGrace();
        gracePending = true;
        ensureHandler();
        graceRunnable = () -> {
            if (!gracePending) {
                return;
            }
            gracePending = false;
            deliverIfNewer(fallback);
        };
        handler.postDelayed(graceRunnable, delayMillis);
    }

    private void cancelGrace() {
        if (gracePending) {
            gracePending = false;
            if (handler != null && graceRunnable != null) {
                handler.removeCallbacks(graceRunnable);
            }
        }
    }

    // Last resort if the new fix takes too long: stop listening and finish with the cached fix, or an error.
    private void scheduleGiveUp(LocationManager manager, long delayMillis) {
        cancelGiveUp();
        giveUpPending = true;
        ensureHandler();
        giveUpRunnable = () -> {
            if (!giveUpPending) {
                return;
            }
            giveUpPending = false;
            if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R) {
                manager.removeUpdates(this); // only on 26-29; on 30+ this would stop a running continuous stream
            }
            cancelGrace();
            resolveOneShot(null);
        };
        handler.postDelayed(giveUpRunnable, delayMillis);
    }

    private void cancelGiveUp() {
        if (giveUpPending) {
            giveUpPending = false;
            if (handler != null && giveUpRunnable != null) {
                handler.removeCallbacks(giveUpRunnable);
            }
        }
    }

    // Start continuous location updates. Returns false if no provider is on.
    public boolean startLocationUpdates(LocationManager manager, long intervalMillis, boolean preciseGranted) {
        cancelGrace(); // a continuous stream takes over from any single request still in progress
        cancelGiveUp();
        oneShotMode = false;
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            requestFusedUpdates(manager, intervalMillis); // API 31+
            return true;
        }
        String provider = bestProvider(manager, preciseGranted);
        if (provider == null) {
            return false;
        }
        manager.requestLocationUpdates(provider, intervalMillis, 0f, this, Looper.getMainLooper());
        return true;
    }

    public void stopLocationUpdates(LocationManager manager) {
        manager.removeUpdates(this);
    }

    // In its own method because getCurrentLocation is API 30+.
    private void getCurrentLocation(LocationManager manager, String provider) {
        manager.getCurrentLocation(provider, null, mainThreadExecutor(), this);
    }

    // In its own method because LocationRequest.Builder and the fused provider are API 31+.
    private void requestFusedUpdates(LocationManager manager, long intervalMillis) {
        android.location.LocationRequest request =
                new android.location.LocationRequest.Builder(intervalMillis).build();
        manager.requestLocationUpdates(
                LocationManager.FUSED_PROVIDER, request, mainThreadExecutor(), this);
    }

    // Pick the best provider that's on. With coarse-only permission use NETWORK — GPS needs FINE and would throw.
    private static String bestProvider(LocationManager manager, boolean preciseGranted) {
        if (!preciseGranted && manager.isProviderEnabled(LocationManager.NETWORK_PROVIDER)) {
            return LocationManager.NETWORK_PROVIDER;
        }
        return manager.getBestProvider(new Criteria(), true);
    }

    private void ensureHandler() {
        if (handler == null) {
            handler = new Handler(Looper.getMainLooper());
        }
    }

    private Executor mainThreadExecutor() {
        ensureHandler();
        Handler h = handler;
        return command -> h.post(command);
    }
}
