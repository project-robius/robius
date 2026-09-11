#!/usr/bin/env python3
"""Runs the real Android session logic against deterministic host-side fakes.

You need a JDK and a C compiler. Note that this never talks to a device or opens
a microphone: the fakes stand in for the Android APIs, and the JNI shim just
records the events that the production NativeSpeech.java emits.
"""

import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile


SOURCES = {
    "android/Manifest.java": """package android;
public class Manifest { public static class permission { public static final String RECORD_AUDIO = "audio"; } }
""",
    "android/content/Context.java": "package android.content; public class Context {}",
    "android/content/Intent.java": """package android.content;
public class Intent {
    public Intent(String action) {}
    public Intent putExtra(String key, String value) { return this; }
    public Intent putExtra(String key, boolean value) { return this; }
    public Intent putExtra(String key, int value) { return this; }
}
""",
    "android/content/pm/PackageManager.java": """package android.content.pm;
public class PackageManager { public static final int PERMISSION_GRANTED = 0; }
""",
    "android/app/Activity.java": """package android.app;
public class Activity extends android.content.Context {
    private final Application application;
    private final FragmentManager fragments = new FragmentManager(this);
    // Set by tests: 0 grants, -1 denies until the user answers the prompt.
    public static int permission = 0;
    public boolean resumed = true;
    private boolean destroyed, changingConfigurations;
    public Activity() { this(new Application()); }
    public Activity(Application application) { this.application = application; }
    public boolean isFinishing() { return false; }
    public boolean isDestroyed() { return destroyed; }
    public boolean isChangingConfigurations() { return changingConfigurations; }
    public int checkSelfPermission(String name) { return permission; }
    public Application getApplication() { return application; }
    public FragmentManager getFragmentManager() { return fragments; }
    public ClassLoader getClassLoader() { return Activity.class.getClassLoader(); }
    public void runOnUiThread(Runnable runnable) { runnable.run(); }
    public void pause() {
        if (!resumed) return;
        resumed = false;
        fragments.pauseAll();
        application.paused(this);
    }
    public void resume() {
        if (destroyed || resumed) return;
        resumed = true;
        application.resumed(this);
        fragments.resumeAll();
    }
    public void destroy(boolean configurationChange) {
        changingConfigurations = configurationChange;
        pause();
        destroyed = true;
        fragments.destroy();
        application.destroyed(this);
    }
}
""",
    "android/app/Fragment.java": """package android.app;
public class Fragment {
    private Activity activity;
    private FragmentManager manager;
    private final FragmentManager children = new FragmentManager(this);
    public static Fragment pending;
    public static int requests;
    void attach(Activity activity, FragmentManager manager) {
        this.activity = activity; this.manager = manager;
    }
    void detach() { this.activity = null; this.manager = null; }
    public Activity getActivity() { return activity; }
    public FragmentManager getFragmentManager() { return manager; }
    public FragmentManager getChildFragmentManager() { return children; }
    public boolean isAdded() { return manager != null && manager.contains(this); }
    public void setRetainInstance(boolean retain) {
        if (retain) throw new AssertionError("permission helpers must not retain discarded Activities");
    }
    public void onCreate(android.os.Bundle state) {}
    public void onResume() {}
    public void onPause() {}
    public void onDestroy() {}
    public void onDetach() {}
    public void onSaveInstanceState(android.os.Bundle state) {}
    public void onRequestPermissionsResult(int code, String[] names, int[] results) {}
    // Records the in-flight request so a test can answer it, as the OS would.
    public void requestPermissions(String[] names, int code) {
        if (pending != null) throw new AssertionError("a second system prompt was requested while one was pending");
        requests++;
        pending = this; pendingCode = code;
        activity.pause();
    }
    public static int pendingCode;
    /** Answer the prompt the way the user would. */
    public static void answer(boolean granted) {
        Fragment fragment = pending; pending = null;
        if (fragment == null) throw new AssertionError("no permission request is pending");
        Activity.permission = granted ? 0 : -1;
        Activity owner = fragment.getActivity();
        // Activity.findFragmentByWho drops results for a detached request fragment.
        if (owner != null) {
            fragment.onRequestPermissionsResult(pendingCode, new String[] {"audio"},
                new int[] { granted ? 0 : -1 });
            owner.resume();
        }
    }
}
""",
    "android/app/FragmentManager.java": """package android.app;
public class FragmentManager {
    private final Activity activity;
    private final Fragment parent;
    private boolean destroyed, executing;
    private final java.util.LinkedHashMap<String, Fragment> byTag = new java.util.LinkedHashMap<>();
    private final java.util.ArrayList<FragmentLifecycleCallbacks> callbacks = new java.util.ArrayList<>();
    private final java.util.ArrayList<Runnable> transactions = new java.util.ArrayList<>();
    public FragmentManager(Activity activity) { this.activity = activity; this.parent = null; }
    public FragmentManager(Fragment parent) { this.activity = null; this.parent = parent; }
    private Activity activity() { return parent == null ? activity : parent.getActivity(); }
    public boolean isDestroyed() { return destroyed; }
    public boolean isStateSaved() { return false; }
    public Fragment findFragmentByTag(String tag) { return byTag.get(tag); }
    public FragmentTransaction beginTransaction() { return new FragmentTransaction(this); }
    public boolean contains(Fragment fragment) { return byTag.containsValue(fragment); }
    public void registerFragmentLifecycleCallbacks(FragmentLifecycleCallbacks callback, boolean recursive) {
        callbacks.add(callback);
    }
    public void unregisterFragmentLifecycleCallbacks(FragmentLifecycleCallbacks callback) { callbacks.remove(callback); }
    public int callbackCount() { return callbacks.size(); }
    public static abstract class FragmentLifecycleCallbacks {
        public void onFragmentSaveInstanceState(FragmentManager manager, Fragment fragment, android.os.Bundle state) {}
        public void onFragmentDetached(FragmentManager manager, Fragment fragment) {}
        public void onFragmentDestroyed(FragmentManager manager, Fragment fragment) {}
    }
    void commit(Runnable action, boolean immediate) {
        if (destroyed) return; // commitAllowingStateLoss drops work on a destroyed host.
        if (immediate) { execute(action); return; }
        transactions.add(action);
        new android.os.Handler(android.os.Looper.getMainLooper()).post(() -> {
            if (transactions.remove(action) && !destroyed) execute(action);
        });
    }
    private void execute(Runnable action) {
        if (executing) throw new IllegalStateException("FragmentManager is already executing transactions");
        executing = true;
        try { action.run(); } finally { executing = false; }
    }
    public boolean executePendingTransactions() {
        boolean any = !transactions.isEmpty();
        for (Runnable action : new java.util.ArrayList<>(transactions)) {
            transactions.remove(action);
            if (!destroyed) execute(action);
        }
        return any;
    }
    void add(String tag, Fragment fragment) {
        byTag.put(tag, fragment);
        fragment.attach(activity(), this);
        fragment.onCreate(null);
        if (activity().resumed) fragment.onResume();
    }
    void remove(Fragment fragment) {
        if (!contains(fragment)) return;
        byTag.values().remove(fragment);
        fragment.getChildFragmentManager().destroy();
        fragment.onDestroy();
        for (FragmentLifecycleCallbacks callback : new java.util.ArrayList<>(callbacks)) {
            callback.onFragmentDestroyed(this, fragment);
        }
        fragment.onDetach();
        fragment.detach();
        for (FragmentLifecycleCallbacks callback : new java.util.ArrayList<>(callbacks)) {
            callback.onFragmentDetached(this, fragment);
        }
    }
    public void pauseAll() {
        for (Fragment fragment : new java.util.ArrayList<>(byTag.values())) {
            fragment.getChildFragmentManager().pauseAll(); fragment.onPause();
        }
    }
    public void resumeAll() {
        for (Fragment fragment : new java.util.ArrayList<>(byTag.values())) {
            fragment.onResume(); fragment.getChildFragmentManager().resumeAll();
        }
    }
    public void destroy() {
        destroyed = true;
        transactions.clear();
        for (Fragment fragment : new java.util.ArrayList<>(byTag.values())) remove(fragment);
    }
    public static final class SavedFragment {
        public final String className, tag;
        public final android.os.Bundle state;
        SavedFragment(Fragment fragment, String tag, android.os.Bundle state) {
            this.className = fragment.getClass().getName(); this.tag = tag; this.state = state;
        }
    }
    public java.util.List<SavedFragment> saveAllState() {
        executePendingTransactions();
        java.util.List<SavedFragment> result = new java.util.ArrayList<>();
        for (java.util.Map.Entry<String, Fragment> entry : byTag.entrySet()) {
            Fragment fragment = entry.getValue();
            android.os.Bundle state = new android.os.Bundle();
            // The framework snapshots the class name before requesting this Bundle.
            SavedFragment saved = new SavedFragment(fragment, entry.getKey(), state);
            fragment.onSaveInstanceState(state);
            java.util.List<SavedFragment> childState = fragment.getChildFragmentManager().saveAllState();
            if (!childState.isEmpty()) state.put("children", childState);
            // Android's callback runs after performSaveInstanceState saved children.
            for (FragmentLifecycleCallbacks callback : new java.util.ArrayList<>(callbacks)) {
                callback.onFragmentSaveInstanceState(this, fragment, state);
            }
            result.add(saved);
        }
        return result;
    }
    @SuppressWarnings("unchecked")
    public void restoreAllState(java.util.List<SavedFragment> saved, ClassLoader loader) throws Exception {
        for (SavedFragment entry : saved) {
            Fragment fragment = (Fragment) loader.loadClass(entry.className).getConstructor().newInstance();
            byTag.put(entry.tag, fragment);
            fragment.attach(activity(), this);
            fragment.onCreate(entry.state);
            Object children = entry.state.get("children");
            if (children != null) fragment.getChildFragmentManager().restoreAllState((java.util.List<SavedFragment>) children, loader);
        }
    }
    public int count() { return byTag.size(); }
}
""",
    "android/app/FragmentTransaction.java": """package android.app;
public class FragmentTransaction {
    private final FragmentManager manager;
    private String tag; private Fragment added, removed;
    public FragmentTransaction(FragmentManager manager) { this.manager = manager; }
    public FragmentTransaction add(Fragment fragment, String tag) {
        this.added = fragment; this.tag = tag; return this;
    }
    public FragmentTransaction remove(Fragment fragment) { this.removed = fragment; return this; }
    public void commitAllowingStateLoss() { manager.commit(this::apply, false); }
    public void commitNowAllowingStateLoss() { manager.commit(this::apply, true); }
    private void apply() {
        if (removed != null) manager.remove(removed);
        if (added != null) manager.add(tag, added);
    }
}
""",
    "android/app/Application.java": """package android.app;
public class Application {
    private final java.util.ArrayList<ActivityLifecycleCallbacks> callbacks = new java.util.ArrayList<>();
    public void registerActivityLifecycleCallbacks(ActivityLifecycleCallbacks callback) { callbacks.add(callback); }
    public void unregisterActivityLifecycleCallbacks(ActivityLifecycleCallbacks callback) { callbacks.remove(callback); }
    public void paused(Activity activity) {
        for (ActivityLifecycleCallbacks callback : new java.util.ArrayList<>(callbacks)) callback.onActivityPaused(activity);
    }
    public void resumed(Activity activity) {
        for (ActivityLifecycleCallbacks callback : new java.util.ArrayList<>(callbacks)) callback.onActivityResumed(activity);
    }
    public void destroyed(Activity activity) {
        for (ActivityLifecycleCallbacks callback : new java.util.ArrayList<>(callbacks)) callback.onActivityDestroyed(activity);
    }
    public interface ActivityLifecycleCallbacks {
        void onActivityCreated(Activity activity, android.os.Bundle state);
        void onActivityStarted(Activity activity);
        void onActivityResumed(Activity activity);
        void onActivityPaused(Activity activity);
        void onActivityStopped(Activity activity);
        void onActivitySaveInstanceState(Activity activity, android.os.Bundle state);
        void onActivityDestroyed(Activity activity);
    }
}
""",
    "android/os/Build.java": """package android.os;
public class Build { public static class VERSION { public static int SDK_INT = 33; } }
""",
    "android/os/Bundle.java": """package android.os;
public class Bundle {
    private java.util.ArrayList<String> values;
    private final java.util.HashMap<String, Object> data = new java.util.HashMap<>();
    public void clear() { data.clear(); values = null; }
    public boolean isEmpty() { return data.isEmpty() && values == null; }
    public void put(String key, Object value) { data.put(key, value); }
    public Object get(String key) { return data.get(key); }
    public void putString(String key, String value) { data.put(key, value); }
    public String getString(String key) { return (String) data.get(key); }
    public void putStringArrayList(String key, java.util.ArrayList<String> value) { values = value; }
    public java.util.ArrayList<String> getStringArrayList(String key) { return values; }
}
""",
    "android/os/Looper.java": """package android.os;
public class Looper { public static Looper getMainLooper() { return new Looper(); } }
""",
    "android/os/Handler.java": """package android.os;
public class Handler {
    private static final class Task {
        Runnable runnable; long time;
        Task(Runnable runnable, long time) { this.runnable = runnable; this.time = time; }
    }
    private static final java.util.ArrayList<Task> tasks = new java.util.ArrayList<>();
    private static long now;
    public Handler(Looper looper) {}
    public boolean post(Runnable runnable) { return postDelayed(runnable, 0); }
    public boolean postDelayed(Runnable runnable, long delay) {
        tasks.add(new Task(runnable, now + delay));
        tasks.sort(java.util.Comparator.comparingLong(task -> task.time));
        return true;
    }
    public void removeCallbacks(Runnable runnable) { tasks.removeIf(task -> task.runnable == runnable); }
    public static int count() { return tasks.size(); }
    public static long nextDelay() { return tasks.get(0).time - now; }
    public static void runNext() {
        Task task = tasks.remove(0);
        now = task.time;
        task.runnable.run();
    }
    public static void reset() { tasks.clear(); now = 0; }
    public static void drain() {
        int limit = 100;
        while (!tasks.isEmpty() && tasks.get(0).time == now) {
            if (--limit == 0) throw new AssertionError("unbounded immediate work");
            runNext();
        }
    }
}
""",
    "harness/HostStateFragment.java": """package harness;
public class HostStateFragment extends android.app.Fragment {
    public String value = "host draft must survive";
    @Override public void onCreate(android.os.Bundle state) {
        if (state != null) value = state.getString("draft");
    }
    @Override public void onSaveInstanceState(android.os.Bundle state) { state.putString("draft", value); }
}
""",
    "harness/Launcher.java": """package harness;
public class Launcher {
    public static void main(String[] args) throws Exception {
        java.net.URL url = new java.io.File(args[0]).toURI().toURL();
        try (java.net.URLClassLoader child = new java.net.URLClassLoader(new java.net.URL[] {url}, Launcher.class.getClassLoader())) {
            Class<?> test = child.loadClass("dev.robius.speech.NativeSpeechRetryTest");
            test.getMethod("main", String[].class).invoke(null, (Object) new String[] {args[1]});
        }
    }
}
""",
    "android/speech/RecognitionListener.java": """package android.speech;
public interface RecognitionListener {
    void onReadyForSpeech(android.os.Bundle params);
    void onBeginningOfSpeech();
    void onRmsChanged(float rms);
    void onBufferReceived(byte[] buffer);
    void onEndOfSpeech();
    void onError(int error);
    void onResults(android.os.Bundle results);
    void onPartialResults(android.os.Bundle results);
    void onEvent(int type, android.os.Bundle params);
}
""",
    "android/speech/RecognizerIntent.java": """package android.speech;
public class RecognizerIntent {
    public static final String ACTION_RECOGNIZE_SPEECH = "speech", EXTRA_LANGUAGE_MODEL = "model",
        LANGUAGE_MODEL_FREE_FORM = "free", EXTRA_PARTIAL_RESULTS = "partial", EXTRA_MAX_RESULTS = "max",
        EXTRA_LANGUAGE = "language", EXTRA_PREFER_OFFLINE = "offline";
}
""",
    "android/speech/SpeechRecognizer.java": """package android.speech;
public class SpeechRecognizer {
    public static final String RESULTS_RECOGNITION = "results";
    public static final int ERROR_NETWORK_TIMEOUT = 1, ERROR_NETWORK = 2, ERROR_AUDIO = 3,
        ERROR_CLIENT = 5, ERROR_SPEECH_TIMEOUT = 6, ERROR_NO_MATCH = 7,
        ERROR_RECOGNIZER_BUSY = 8, ERROR_INSUFFICIENT_PERMISSIONS = 9;
    public static SpeechRecognizer last;
    public static int starts;
    public RecognitionListener listener;
    public boolean destroyed;
    public static boolean isRecognitionAvailable(android.content.Context context) { return true; }
    public static boolean isOnDeviceRecognitionAvailable(android.content.Context context) { return false; }
    public static SpeechRecognizer createSpeechRecognizer(android.content.Context context) {
        return last = new SpeechRecognizer();
    }
    public static SpeechRecognizer createOnDeviceSpeechRecognizer(android.content.Context context) {
        return createSpeechRecognizer(context);
    }
    public void setRecognitionListener(RecognitionListener listener) { this.listener = listener; }
    public void startListening(android.content.Intent intent) { starts++; }
    public void stopListening() {}
    public void cancel() {}
    public void destroy() { destroyed = true; }
}
""",
    "dev/robius/speech/NativeSpeechRetryTest.java": """package dev.robius.speech;
import android.app.Activity;
import android.os.Bundle;
import android.os.Handler;
import android.speech.RecognitionListener;
import android.speech.SpeechRecognizer;

public class NativeSpeechRetryTest {
    private static final java.util.ArrayList<String> events = new java.util.ArrayList<>();
    private static final java.util.ArrayList<Long> eventIds = new java.util.ArrayList<>();
    public static void record(long id, int kind, String text, float level) {
        events.add(kind + ":" + (text == null ? "" : text));
        eventIds.add(id);
    }
    private static void check(boolean condition, String message) {
        if (!condition) throw new AssertionError(message + " events=" + events);
    }
    private static Bundle words(String text) {
        Bundle bundle = new Bundle();
        bundle.putStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION,
            new java.util.ArrayList<>(java.util.Arrays.asList(text)));
        return bundle;
    }
    private static void start(long id) {
        Handler.reset(); events.clear(); eventIds.clear(); SpeechRecognizer.starts = 0;
        NativeSpeech.start(new Activity(), id, "", false);
        Handler.runNext();
        SpeechRecognizer.last.listener.onReadyForSpeech(new Bundle());
    }
    private static void busyIsBounded() {
        start(1);
        for (int retry = 0; retry < 3; retry++) {
            RecognitionListener old = SpeechRecognizer.last.listener;
            old.onError(SpeechRecognizer.ERROR_RECOGNIZER_BUSY);
            check(Handler.count() == 1 && Handler.nextDelay() == (500L << retry), "bounded exponential backoff");
            int count = events.size();
            old.onPartialResults(words("stale"));
            old.onError(SpeechRecognizer.ERROR_RECOGNIZER_BUSY);
            check(events.size() == count && Handler.count() == 1, "late callbacks cannot schedule retries or insert words");
            Handler.runNext();
            SpeechRecognizer.last.listener.onReadyForSpeech(new Bundle());
        }
        SpeechRecognizer.last.listener.onError(SpeechRecognizer.ERROR_RECOGNIZER_BUSY);
        check(Handler.count() == 0 && SpeechRecognizer.last.destroyed, "exhausted retry budget releases recognizer");
        check(events.stream().filter(event -> event.startsWith("5:")).count() == 1, "one terminal error after retries");
        check(SpeechRecognizer.starts == 4, "initial attempt and only three retries");
    }
    private static void clientRecreatesAndSuccessResetsBudget() {
        start(2);
        SpeechRecognizer old = SpeechRecognizer.last;
        old.listener.onPartialResults(words("keep this"));
        old.listener.onError(SpeechRecognizer.ERROR_CLIENT);
        check(old.destroyed && events.contains("2:keep this"), "client reconnect commits visible text and destroys old client");
        Handler.runNext();
        check(SpeechRecognizer.last != old, "client error creates a fresh recognizer");
        int count = events.size();
        old.listener.onResults(words("obsolete"));
        check(events.size() == count, "old client cannot append late final results");
        SpeechRecognizer.last.listener.onResults(words("next utterance"));
        check(Handler.nextDelay() == 150, "successful utterance resumes normal cadence");
        Handler.runNext();
        SpeechRecognizer.last.listener.onError(SpeechRecognizer.ERROR_RECOGNIZER_BUSY);
        check(Handler.nextDelay() == 500, "completed utterance resets transient failure budget");
        NativeSpeech.stop(2, true);
        Handler.runNext();
        check(Handler.count() == 0, "cancel removes delayed reconnect");
    }
    private static void stopDuringBackoffNeverRestarts() {
        start(3);
        SpeechRecognizer.last.listener.onError(SpeechRecognizer.ERROR_RECOGNIZER_BUSY);
        NativeSpeech.stop(3, false);
        Handler.runNext();
        check(Handler.count() == 0 && SpeechRecognizer.starts == 1, "stop during backoff does not restart capture");
        check(events.stream().filter(event -> event.equals("4:")).count() == 1, "graceful stop emits one terminal event");
    }
    private static void permissionIsRequestedThenSessionProceeds() {
        Handler.reset(); events.clear(); SpeechRecognizer.starts = 0;
        android.app.Activity.permission = -1;   // not granted yet
        android.app.Activity activity = new android.app.Activity();
        NativeSpeech.start(activity, 10, "", false);
        Handler.runNext();
        check(android.app.Fragment.pending != null, "a missing permission must raise the system prompt");
        check(SpeechRecognizer.starts == 0, "recognition must not begin before the prompt is answered");
        android.app.Fragment.answer(true);
        check(activity.getFragmentManager().callbackCount() == 1,
            "the save-state guard remains until queued removal actually detaches its host");
        Handler.drain();
        check(activity.getFragmentManager().count() == 0, "the fragment removes itself once answered");
        SpeechRecognizer.last.listener.onReadyForSpeech(new Bundle());
        check(SpeechRecognizer.starts == 1 && events.contains("0:"), "granting resumes the session");
        NativeSpeech.stop(10, true);
        Handler.drain();
        android.app.Activity.permission = 0;
    }
    private static void permissionDenialIsReportedOnce() {
        Handler.reset(); events.clear(); SpeechRecognizer.starts = 0;
        android.app.Activity.permission = -1;
        android.app.Activity activity = new android.app.Activity();
        NativeSpeech.start(activity, 11, "", false);
        Handler.runNext();
        android.app.Fragment.answer(false);
        Handler.drain();
        check(SpeechRecognizer.starts == 0, "a denied prompt must not open the microphone");
        check(events.size() == 1 && events.get(0).startsWith("6:"), "denial reports PermissionDenied once, events=" + events);
        check(activity.getFragmentManager().count() == 0, "the fragment removes itself after a denial");
        android.app.Activity.permission = 0;
    }
    private static void permissionsAreNotRetried() {
        start(4);
        SpeechRecognizer.last.listener.onError(SpeechRecognizer.ERROR_INSUFFICIENT_PERMISSIONS);
        check(Handler.count() == 0 && SpeechRecognizer.last.destroyed, "permission errors stay terminal");
    }
    private static Activity missingPermission() {
        Handler.reset(); events.clear(); eventIds.clear(); SpeechRecognizer.starts = 0;
        Activity.permission = -1;
        check(android.app.Fragment.pending == null, "the preceding case must release its system prompt");
        android.app.Fragment.requests = 0;
        return new Activity();
    }
    private static void cancelDiscardsLateGrant() {
        Activity activity = missingPermission();
        NativeSpeech.start(activity, 20, "", false);
        Handler.runNext();
        NativeSpeech.stop(20, true);
        Handler.drain();
        check(events.isEmpty() && SpeechRecognizer.starts == 0, "canceling a pending session is silent");
        android.app.Fragment.answer(true);
        Handler.drain();
        check(events.isEmpty() && SpeechRecognizer.starts == 0, "a late grant cannot restart a canceled session");
        check(activity.getFragmentManager().count() == 0 && activity.getFragmentManager().callbackCount() == 0,
            "answering an abandoned request releases its parent and save listener");
    }
    private static void nextSessionAdoptsPendingPrompt() {
        Activity activity = missingPermission();
        NativeSpeech.start(activity, 21, "", false);
        Handler.runNext();
        android.app.Fragment pending = android.app.Fragment.pending;
        // Both operations are queued before the old request receives its answer.
        NativeSpeech.stop(21, true);
        NativeSpeech.start(activity, 22, "", false);
        Handler.drain();
        check(events.isEmpty(), "a successor must not receive a false permission denial");
        check(android.app.Fragment.pending == pending && android.app.Fragment.requests == 1,
            "the successor adopts the existing system prompt without requesting a second one");
        android.app.Fragment.answer(true);
        Handler.drain();
        check(SpeechRecognizer.starts == 1, "grant starts only the successor");
        SpeechRecognizer.last.listener.onReadyForSpeech(new Bundle());
        check(events.size() == 1 && events.get(0).equals("0:") && eventIds.get(0) == 22L,
            "the canceled session cannot receive the successor's result");
        check(activity.getFragmentManager().count() == 0 && activity.getFragmentManager().callbackCount() == 0,
            "adopted prompt cleanup releases all helper resources");
        NativeSpeech.stop(22, true);
        Handler.drain();
    }
    private static void cancelBeforeResumeDoesNotAsk() {
        Activity activity = missingPermission();
        activity.resumed = false;
        NativeSpeech.start(activity, 23, "", false);
        Handler.runNext();
        check(android.app.Fragment.pending == null, "a paused owner does not launch the prompt yet");
        NativeSpeech.stop(23, true);
        Handler.runNext();
        // Resume before the queued fragment removal executes.
        activity.resume();
        Handler.drain();
        check(android.app.Fragment.requests == 0 && SpeechRecognizer.starts == 0 && events.isEmpty(),
            "cancel must suppress a deferred prompt even before its transaction is removed");
        check(activity.getFragmentManager().count() == 0 && activity.getFragmentManager().callbackCount() == 0,
            "cancel before prompt launch releases the parent and listener");
    }
    private static void restartWhileOldParentRemovalIsQueued() {
        Activity activity = missingPermission();
        activity.resumed = false;
        NativeSpeech.start(activity, 24, "", false);
        Handler.runNext();
        NativeSpeech.stop(24, true);
        NativeSpeech.start(activity, 25, "", false);
        Handler.runNext(); // cancel queues a removal behind the already queued start
        Handler.runNext();
        activity.resume();
        Handler.drain();
        check(events.isEmpty() && android.app.Fragment.requests == 1,
            "an old removal cannot cancel the replacement request or cause a false denial");
        android.app.Fragment.answer(true);
        Handler.drain();
        check(SpeechRecognizer.starts == 1, "the replacement still starts after old removal drains");
        SpeechRecognizer.last.listener.onReadyForSpeech(new Bundle());
        check(eventIds.size() == 1 && eventIds.get(0) == 25L, "only the replacement receives Started");
        NativeSpeech.stop(25, true);
        Handler.drain();
    }
    private static void configurationTeardownStopsPendingSession() {
        Activity activity = missingPermission();
        NativeSpeech.start(activity, 26, "", false);
        Handler.runNext();
        java.util.List<android.app.FragmentManager.SavedFragment> saved = activity.getFragmentManager().saveAllState();
        activity.destroy(true);
        Handler.drain();
        check(events.size() == 1 && events.get(0).equals("4:") && eventIds.get(0) == 26L,
            "configuration teardown ends the pending session once with Stopped, not permission denial");
        check(activity.getFragmentManager().callbackCount() == 0, "discarded Activity releases its save listener");
        android.app.Fragment.answer(true);
        Handler.drain();
        check(events.size() == 1 && SpeechRecognizer.starts == 0, "a grant cannot restart the discarded Activity");
        Activity replacement = new Activity(activity.getApplication());
        try { replacement.getFragmentManager().restoreAllState(saved, replacement.getClassLoader()); }
        catch (Exception error) { throw new AssertionError("configuration state must be restorable", error); }
        NativeSpeech.start(replacement, 27, "", false);
        Handler.drain();
        check(SpeechRecognizer.starts == 1, "the replacement Activity can start a new session after grant");
        SpeechRecognizer.last.listener.onReadyForSpeech(new Bundle());
        check(eventIds.get(eventIds.size() - 1) == 27L, "the new Activity owns the restarted session");
        NativeSpeech.stop(27, true);
        Handler.drain();
    }
    private static void processRestoreContainsOnlyHostVisibleClasses() {
        Activity activity = missingPermission();
        check(NativeSpeech.class.getClassLoader() != activity.getClassLoader(),
            "production speech must be loaded in a separate child classloader");
        try {
            activity.getClassLoader().loadClass("dev.robius.speech.SpeechPermissionFragment");
            throw new AssertionError("host loader must be unable to load the embedded helper");
        } catch (ClassNotFoundException expected) {}
        activity.getFragmentManager().beginTransaction().add(new harness.HostStateFragment(), "unrelated")
            .commitNowAllowingStateLoss();
        NativeSpeech.start(activity, 28, "", false);
        Handler.runNext();
        java.util.List<android.app.FragmentManager.SavedFragment> saved = activity.getFragmentManager().saveAllState();
        check(saved.size() == 2, "saving preserves the unrelated app fragment and framework permission parent");
        // Repeated saves must remain safe while the same OS request is pending.
        saved = activity.getFragmentManager().saveAllState();
        Activity restored = new Activity();
        try { restored.getFragmentManager().restoreAllState(saved, restored.getClassLoader()); }
        catch (Exception error) { throw new AssertionError("process restoration cannot load child DEX classes", error); }
        harness.HostStateFragment unrelated = (harness.HostStateFragment) restored.getFragmentManager().findFragmentByTag("unrelated");
        check(unrelated != null && "host draft must survive".equals(unrelated.value),
            "clearing permission state cannot alter an unrelated app fragment's state");
        android.app.Fragment host = restored.getFragmentManager().findFragmentByTag("dev.robius.speech.PermissionHost");
        check(host != null && host.getClass() == android.app.Fragment.class && host.getChildFragmentManager().count() == 0,
            "the restored helper host contains no custom child fragment");
        // A fresh process would lose these callbacks; explicitly release the old process's fake state.
        NativeSpeech.stop(28, true);
        Handler.drain();
        android.app.Fragment.answer(false);
        Handler.drain();
        check(events.isEmpty(), "an abandoned old-process request has no recipient");
        NativeSpeech.start(restored, 29, "", false);
        Handler.drain();
        check(android.app.Fragment.pending != null && restored.getFragmentManager().count() == 2,
            "a new request replaces the empty restored host without disturbing app fragments");
        android.app.Fragment.answer(true);
        Handler.drain();
        check(SpeechRecognizer.starts == 1, "speech works after restoring a process with a pending permission request");
        NativeSpeech.stop(29, true);
        Handler.drain();
    }
    private interface Case { void run(); }
    public static void main(String[] args) {
        System.load(args[0]);
        Object[][] cases = {
            {"a busy recognizer retries with bounded backoff", (Case) NativeSpeechRetryTest::busyIsBounded},
            {"a client error recreates the recognizer", (Case) NativeSpeechRetryTest::clientRecreatesAndSuccessResetsBudget},
            {"stopping during backoff never restarts", (Case) NativeSpeechRetryTest::stopDuringBackoffNeverRestarts},
            {"permission errors are not retried", (Case) NativeSpeechRetryTest::permissionsAreNotRetried},
            {"a missing permission is requested, then the session proceeds", (Case) NativeSpeechRetryTest::permissionIsRequestedThenSessionProceeds},
            {"a denied permission is reported once", (Case) NativeSpeechRetryTest::permissionDenialIsReportedOnce},
            {"canceling a pending prompt discards a late grant", (Case) NativeSpeechRetryTest::cancelDiscardsLateGrant},
            {"a new session adopts an abandoned system prompt", (Case) NativeSpeechRetryTest::nextSessionAdoptsPendingPrompt},
            {"cancel before resume never launches the deferred prompt", (Case) NativeSpeechRetryTest::cancelBeforeResumeDoesNotAsk},
            {"queued old removal cannot remove a new permission request", (Case) NativeSpeechRetryTest::restartWhileOldParentRemovalIsQueued},
            {"configuration teardown stops the old pending session", (Case) NativeSpeechRetryTest::configurationTeardownStopsPendingSession},
            {"saved permission state restores through the host classloader", (Case) NativeSpeechRetryTest::processRestoreContainsOnlyHostVisibleClasses},
        };
        for (Object[] entry : cases) {
            System.out.println("  " + entry[0]);
            ((Case) entry[1]).run();
        }
        System.out.println("Passed " + cases.length + " Android speech regressions; no microphone used.");
    }
}
""",
}

JNI = """#include <jni.h>
JNIEXPORT void JNICALL Java_dev_robius_speech_NativeSpeech_event(
    JNIEnv *env, jclass owner, jlong id, jint kind, jstring text, jfloat level) {
    (void)owner;
    jclass test = (*env)->FindClass(env, "dev/robius/speech/NativeSpeechRetryTest");
    jmethodID record = (*env)->GetStaticMethodID(env, test, "record", "(JILjava/lang/String;F)V");
    (*env)->CallStaticVoidMethod(env, test, record, id, kind, text, level);
}
"""


def main():
    java_home = os.environ.get("JAVA_HOME")
    if not java_home and sys.platform == "darwin":
        java_home = subprocess.check_output(["/usr/libexec/java_home"], text=True).strip()
    if not java_home:
        java_home = str(Path(shutil.which("javac") or "javac").resolve().parent.parent)
    java_home = Path(java_home)
    crate = Path(__file__).resolve().parents[1]
    with tempfile.TemporaryDirectory(prefix="native-speech-retry-") as temp:
        root = Path(temp)
        for name, source in SOURCES.items():
            path = root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(source)
        for name in ("NativeSpeech.java", "SpeechPermissionFragment.java"):
            shutil.copyfile(crate / "java/dev/robius/speech" / name, root / "dev/robius/speech" / name)
        # Keep platform/host classes on the parent loader and embedded speech
        # classes on a child loader, matching the Android InMemoryDexClassLoader.
        host_classes = root / "host-classes"
        speech_classes = root / "speech-classes"
        subprocess.run([str(java_home / "bin/javac"), "-d", str(host_classes),
            *map(str, (root / "android").rglob("*.java")),
            *map(str, (root / "harness").rglob("*.java"))], check=True)
        subprocess.run([str(java_home / "bin/javac"), "-classpath", str(host_classes), "-d", str(speech_classes),
            *map(str, (root / "dev").rglob("*.java"))], check=True)
        c_file = root / "events.c"
        c_file.write_text(JNI)
        platform = "darwin" if sys.platform == "darwin" else "linux"
        library = root / ("events.dylib" if platform == "darwin" else "events.so")
        subprocess.run([os.environ.get("CC", "cc"), "-shared", "-fPIC",
            "-I" + str(java_home / "include"), "-I" + str(java_home / "include" / platform),
            str(c_file), "-o", str(library)], check=True)
        subprocess.run([str(java_home / "bin/java"), "-cp", str(host_classes),
            "harness.Launcher", str(speech_classes), str(library)], check=True)


if __name__ == "__main__":
    main()
