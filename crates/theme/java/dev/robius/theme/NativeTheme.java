package dev.robius.theme;

import android.content.ComponentCallbacks;
import android.content.res.Configuration;

/** Passes the `uiMode` of each new configuration to `changed` in `src/android.rs`. */
public final class NativeTheme implements ComponentCallbacks {
    private static native void changed(int uiMode);

    @Override
    public void onConfigurationChanged(Configuration configuration) {
        changed(configuration.uiMode);
    }

    @Override
    public void onLowMemory() {}
}
