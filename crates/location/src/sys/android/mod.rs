mod callback;

use std::{
    marker::PhantomData,
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock, PoisonError,
    },
    time::{Duration, SystemTime},
};

use jni::{
    errors::Error as JniError,
    objects::{GlobalRef, JObject, JValueGen},
    sys::jlong,
    JNIEnv,
};

use crate::{Access, Accuracy, Coordinates, Error, Handler, Result};

const COARSE_LOCATION_PERMISSION: &str = "android.permission.ACCESS_COARSE_LOCATION";
const FINE_LOCATION_PERMISSION: &str = "android.permission.ACCESS_FINE_LOCATION";
const PERMISSION_GRANTED: i32 = 0;

/// Shared via `Arc` with the Java `LocationCallback` and permission fragment. Each raw `Arc` ref we
/// hand to Java gets freed exactly once, and everything runs on the main thread.
pub(super) struct Shared {
    handler: Box<dyn Handler>,
    /// The Java `LocationCallback`. Set once in `new`, before any callback could use it.
    callback: OnceLock<GlobalRef>,
    /// Requests we're holding until the user grants permission.
    pending: Mutex<Pending>,
    /// Accuracy from the last `request_authorization`, reused when we prompt on our own.
    accuracy: Mutex<Accuracy>,
    /// Whether a permission request is currently in flight.
    requesting: AtomicBool,
    /// Set on drop; a late permission result must then not touch the handler.
    dropped: AtomicBool,
}

#[derive(Default)]
struct Pending {
    update_once: bool,
    start_updates: bool,
}

pub struct Manager {
    shared: Arc<Shared>,
    /// The `Arc` ref we handed to the Java `LocationCallback`; freed once in `drop`.
    location_callback_ptr: *const Shared,
}

impl Manager {
    pub fn new<T>(handler: T) -> Result<Self>
    where
        T: Handler,
    {
        let shared = Arc::new(Shared {
            handler: Box::new(handler),
            callback: OnceLock::new(),
            pending: Mutex::new(Pending::default()),
            accuracy: Mutex::new(Accuracy::Precise),
            requesting: AtomicBool::new(false),
            dropped: AtomicBool::new(false),
        });

        // An owning `Arc` ref for the Java `LocationCallback`; freed in `drop`.
        let location_callback_ptr = Arc::into_raw(shared.clone());

        let global = robius_android_env::with_activity(|env, _| {
            let callback = construct_callback(env, location_callback_ptr)?;
            env.new_global_ref(callback).map_err(Error::from)
        })
        .map_err(|_| Error::AndroidEnvironment)
        .and_then(|x| x);

        let global = match global {
            Ok(global) => global,
            Err(error) => {
                // SAFETY: Java never took the pointer, so free the `into_raw` ref here, just once.
                unsafe { drop(Arc::from_raw(location_callback_ptr)) };
                return Err(error);
            }
        };

        // Set before the `Manager` (and so any callback) can be used.
        let _ = shared.callback.set(global);

        Ok(Manager {
            shared,
            location_callback_ptr,
        })
    }

    pub fn request_authorization(&self, _access: Access, accuracy: Accuracy) -> Result<()> {
        *self.accuracy_slot() = accuracy;
        robius_android_env::with_activity(|env, activity| {
            // Already allowed (coarse or fine)? Nothing to ask for. Coarse-only counts as enough even
            // for a `Precise` request, so we don't nag the user every call.
            if has_location_permission(env, activity)? {
                return Ok(());
            }
            self.launch_permission_request(env, activity, accuracy)
        })
        .map_err(|_| Error::AndroidEnvironment)
        .and_then(|x| x)
    }

    pub fn update_once(&self) -> Result<()> {
        robius_android_env::with_activity(|env, context| {
            if has_location_permission(env, context)? {
                run_update_once(env, context, &self.shared)
            } else {
                // Hold it and (re-)ask, so a request made while denied or undecided can still go through.
                self.pending().update_once = true;
                self.launch_permission_request(env, context, self.accuracy())
            }
        })
        .map_err(|_| Error::AndroidEnvironment)
        .and_then(|x| x)
    }

    pub fn start_updates(&self) -> Result<()> {
        robius_android_env::with_activity(|env, context| {
            if has_location_permission(env, context)? {
                run_start_updates(env, context, &self.shared)
            } else {
                // Hold it and (re-)ask, so a request made while denied or undecided can still go through.
                self.pending().start_updates = true;
                self.launch_permission_request(env, context, self.accuracy())
            }
        })
        .map_err(|_| Error::AndroidEnvironment)
        .and_then(|x| x)
    }

    pub fn stop_updates(&self) -> Result<()> {
        // Cancel a deferred `start_updates` so a later grant doesn't start updates after a stop.
        self.pending().start_updates = false;

        robius_android_env::with_activity(|env, context| run_remove_updates(env, context, &self.shared))
            .map_err(|_| Error::AndroidEnvironment)
            .and_then(|x| x)
    }

    fn pending(&self) -> std::sync::MutexGuard<'_, Pending> {
        self.shared.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn accuracy_slot(&self) -> std::sync::MutexGuard<'_, Accuracy> {
        self.shared.accuracy.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn accuracy(&self) -> Accuracy {
        *self.accuracy_slot()
    }

    /// Shows the permission dialog unless one is already in flight. The `requesting` flag makes it
    /// idempotent, so two callers asking at once still show only one dialog.
    fn launch_permission_request(
        &self,
        env: &mut JNIEnv<'_>,
        activity: &JObject<'_>,
        accuracy: Accuracy,
    ) -> Result<()> {
        if self.shared.requesting.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        let permissions = match accuracy {
            Accuracy::Approximate => &[COARSE_LOCATION_PERMISSION][..],
            // Android 12+ ignores fine-only runtime requests, so ask for both and let the system show
            // the precise/approximate choice in one dialog.
            Accuracy::Precise => &[COARSE_LOCATION_PERMISSION, FINE_LOCATION_PERMISSION][..],
        };

        let array = match permission_array(env, permissions) {
            Ok(array) => array,
            Err(error) => {
                self.shared.requesting.store(false, Ordering::SeqCst);
                return Err(error);
            }
        };

        // `show` doesn't block; Java now owns `ptr` and will call the callback once. Only free `ptr`
        // here if the JNI call itself failed, so Java never got it.
        let ptr = Arc::into_raw(self.shared.clone());
        match launch_permission_fragment(env, activity, ptr as jlong, &array) {
            Ok(()) => Ok(()),
            Err(error) => {
                // SAFETY: `ptr` came from `Arc::into_raw` and Java never got it, so free it once.
                unsafe { drop(Arc::from_raw(ptr)) };
                self.shared.requesting.store(false, Ordering::SeqCst);
                Err(error)
            }
        }
    }
}

impl Drop for Manager {
    fn drop(&mut self) {
        // Silence any late permission result first, so it can't touch the handler.
        self.shared.dropped.store(true, Ordering::SeqCst);

        // Clean up as best we can (Drop must not panic). Draining the callback makes sure it's
        // finished, so it's safe to free its `Arc` ref below.
        let drained = robius_android_env::with_activity(|env, context| {
            let _ = run_remove_updates(env, context, &self.shared);

            if let Some(callback) = self.shared.callback.get() {
                // `removeUpdates` stops future callbacks; `disableExecution` blocks one already queued.
                let _ = env.call_method(callback, "disableExecution", "()V", &[]);

                // Wait for any `rust_callback` still running (usually already finished on the main thread).
                while let Ok(true) = env
                    .call_method(callback, "isExecuting", "()Z", &[])
                    .and_then(|value| value.z())
                {}
            }

            // Remove any in-flight fragment, so its `onDestroy` frees its `Arc` ref right away.
            if self.shared.requesting.load(Ordering::SeqCst) {
                let _ = remove_permission_fragment(env, context);
            }
        })
        .is_ok();

        if drained {
            // SAFETY: the drain waited for `rust_callback` to finish, so it's safe to free the `new` `into_raw` ref.
            unsafe { drop(Arc::from_raw(self.location_callback_ptr)) };
        }
        // Otherwise the activity was gone and we couldn't make sure the callback had finished, so
        // leaking this ref is the safe choice.
    }
}

/// Runs a held location request, sending any failure to the handler (there's no caller to return an
/// error to).
fn run_and_report<F>(shared: &Shared, f: F)
where
    F: FnOnce(&mut JNIEnv, &JObject, &Shared) -> Result<()>,
{
    let result = robius_android_env::with_activity(|env, context| f(env, context, shared))
        .map_err(|_| Error::AndroidEnvironment)
        .and_then(|x| x);
    if let Err(error) = result {
        report_error(shared, error);
    }
}

/// Reports an error to the handler, catching any panic so it can't unwind across the JNI boundary.
fn report_error(shared: &Shared, error: Error) {
    let _ = catch_unwind(AssertUnwindSafe(|| shared.handler.error(error)));
}

/// Handles the result of a permission request (main thread, and only while the `Manager` is still alive).
pub(super) fn handle_permission_result(shared: &Shared, granted: bool) {
    shared.requesting.store(false, Ordering::SeqCst);

    // Grab the pending flags and drop the lock before calling into JNI or the handler — the handler
    // might call back into a `Manager` method, which would deadlock if we still held it.
    let pending = std::mem::take(
        &mut *shared.pending.lock().unwrap_or_else(PoisonError::into_inner),
    );

    if granted {
        // One-shot first, so a deferred `start_updates` (continuous) wins last.
        if pending.update_once {
            run_and_report(shared, run_update_once);
        }
        if pending.start_updates {
            run_and_report(shared, run_start_updates);
        }
    } else {
        report_error(shared, Error::AuthorizationDenied);
    }
}

fn run_update_once(env: &mut JNIEnv, context: &JObject, shared: &Shared) -> Result<()> {
    let manager = get_location_manager(env, context)?;
    // With fine permission the Java side can wait a moment for a new fix; coarse is too slow for that.
    let precise = has_permission(env, context, FINE_LOCATION_PERMISSION)?;
    let callback = location_callback(shared);

    // The Java side picks the newest available API for this device (see `LocationCallback.java`).
    let started = env
        .call_method(
            callback,
            "requestSingleLocation",
            "(Landroid/location/LocationManager;Z)Z",
            &[JValueGen::Object(&manager), JValueGen::Bool(precise as u8)],
        )
        .map_err(|e| map_android_error(env, e))?
        .z()?;

    // `false` means no location provider is currently enabled.
    if started {
        Ok(())
    } else {
        Err(Error::TemporarilyUnavailable)
    }
}

fn run_start_updates(env: &mut JNIEnv, context: &JObject, shared: &Shared) -> Result<()> {
    let manager = get_location_manager(env, context)?;
    // Fine permission lets the Java side pick a provider that works with the granted accuracy.
    let precise = has_permission(env, context, FINE_LOCATION_PERMISSION)?;
    let callback = location_callback(shared);

    let started = env
        .call_method(
            callback,
            "startLocationUpdates",
            "(Landroid/location/LocationManager;JZ)Z",
            &[JValueGen::Object(&manager), JValueGen::Long(1000), JValueGen::Bool(precise as u8)],
        )
        .map_err(|e| map_android_error(env, e))?
        .z()?;

    if started {
        Ok(())
    } else {
        Err(Error::TemporarilyUnavailable)
    }
}

fn run_remove_updates(env: &mut JNIEnv, context: &JObject, shared: &Shared) -> Result<()> {
    let manager = get_location_manager(env, context)?;
    let callback = location_callback(shared);
    env.call_method(
        callback,
        "stopLocationUpdates",
        "(Landroid/location/LocationManager;)V",
        &[JValueGen::Object(&manager)],
    )
    .map_err(|e| map_android_error(env, e))?;
    Ok(())
}

fn location_callback(shared: &Shared) -> &JObject<'static> {
    shared
        .callback
        .get()
        .expect("LocationCallback is set in Manager::new before any use")
        .as_obj()
}

/// When `getCurrentLocation` gives us nothing: hand back the most recent cached location, or report
/// [`Error::TemporarilyUnavailable`] if there isn't one.
pub(super) fn deliver_last_known_or_error(shared: &Shared) {
    let result = robius_android_env::with_activity(last_known_location)
        .map_err(|_| Error::AndroidEnvironment)
        .and_then(|x| x);

    match result {
        Ok(Some(location)) => {
            let location = crate::Location {
                inner: Location {
                    inner: location,
                    phantom: PhantomData,
                },
            };
            shared.handler.handle(location);
        }
        _ => shared.handler.error(Error::TemporarilyUnavailable),
    }
}

/// Returns the most recent cached location from any provider, if one exists.
fn last_known_location(env: &mut JNIEnv<'_>, context: &JObject<'_>) -> Result<Option<GlobalRef>> {
    let manager = get_location_manager(env, context)?;

    for provider in ["fused", "gps", "network"] {
        let Ok(provider) = env.new_string(provider) else {
            let _ = env.exception_clear();
            continue;
        };
        // An unregistered provider throws; treat that (and any error) as "no location" and move on.
        let location = match env.call_method(
            &manager,
            "getLastKnownLocation",
            "(Ljava/lang/String;)Landroid/location/Location;",
            &[JValueGen::Object(&provider)],
        ) {
            Ok(value) => value.l().unwrap_or_else(|_| JObject::null()),
            Err(error) => {
                let _ = map_android_error(env, error);
                continue;
            }
        };

        if !location.as_raw().is_null() {
            return env
                .new_global_ref(&location)
                .map(Some)
                .map_err(|e| map_android_error(env, e));
        }
    }

    Ok(None)
}

fn get_location_manager<'a>(env: &mut JNIEnv<'a>, context: &JObject<'_>) -> Result<JObject<'a>> {
    let service_name = env
        .new_string("location")
        .map_err(|e| map_android_error(env, e))?;

    env.call_method(
        context,
        "getSystemService",
        "(Ljava/lang/String;)Ljava/lang/Object;",
        &[JValueGen::Object(&service_name)],
    )
    .map_err(|e| map_android_error(env, e))?
    .l()
    .map_err(|e| e.into())
}

fn permission_array<'a>(env: &mut JNIEnv<'a>, permissions: &[&str]) -> Result<JObject<'a>> {
    let first_permission = env
        .new_string(permissions[0])
        .map_err(|e| map_android_error(env, e))?;
    let array = env
        .new_object_array(permissions.len() as i32, "java/lang/String", &first_permission)
        .map_err(|e| map_android_error(env, e))?;

    for (index, permission) in permissions.iter().enumerate().skip(1) {
        let permission = env
            .new_string(*permission)
            .map_err(|e| map_android_error(env, e))?;
        env.set_object_array_element(&array, index as i32, &permission)
            .map_err(|e| map_android_error(env, e))?;
    }

    Ok(array.into())
}

fn launch_permission_fragment(
    env: &mut JNIEnv<'_>,
    activity: &JObject<'_>,
    ptr: jlong,
    permissions: &JObject<'_>,
) -> Result<()> {
    let fragment_class = callback::get_permission_fragment_class(env)?;

    env.call_static_method(
        fragment_class,
        "show",
        "(Landroid/app/Activity;J[Ljava/lang/String;)V",
        &[
            JValueGen::Object(activity),
            JValueGen::Long(ptr),
            JValueGen::Object(permissions),
        ],
    )
    .map_err(|e| map_android_error(env, e))?;
    Ok(())
}

fn remove_permission_fragment(env: &mut JNIEnv<'_>, activity: &JObject<'_>) -> Result<()> {
    let fragment_class = callback::get_permission_fragment_class(env)?;
    env.call_static_method(
        fragment_class,
        "removeExisting",
        "(Landroid/app/Activity;)V",
        &[JValueGen::Object(activity)],
    )
    .map_err(|e| map_android_error(env, e))?;
    Ok(())
}

fn has_location_permission(env: &mut JNIEnv<'_>, context: &JObject<'_>) -> Result<bool> {
    if has_permission(env, context, COARSE_LOCATION_PERMISSION)? {
        return Ok(true);
    }

    has_permission(env, context, FINE_LOCATION_PERMISSION)
}

fn has_permission(env: &mut JNIEnv<'_>, context: &JObject<'_>, permission: &str) -> Result<bool> {
    let permission = env
        .new_string(permission)
        .map_err(|e| map_android_error(env, e))?;
    let result = env
        .call_method(
            context,
            "checkSelfPermission",
            "(Ljava/lang/String;)I",
            &[JValueGen::Object(&permission)],
        )
        .map_err(|e| map_android_error(env, e))?
        .i()?;

    Ok(result == PERMISSION_GRANTED)
}

/// Turns a JNI error into our [`Error`], clearing any pending Java exception first — a leftover
/// exception would blow up the next JNI call (that was the original crash in this crate).
fn map_android_error(env: &mut JNIEnv<'_>, error: JniError) -> Error {
    match error {
        JniError::JavaException => {
            let throwable = env.exception_occurred().ok();
            let _ = env.exception_clear();

            if throwable
                .as_ref()
                .filter(|throwable| !throwable.as_raw().is_null())
                .and_then(|throwable| {
                    env.is_instance_of(throwable, "java/lang/SecurityException").ok()
                })
                .unwrap_or(false)
            {
                Error::AuthorizationDenied
            } else {
                Error::Unknown
            }
        }
        _ => error.into(),
    }
}

fn construct_callback<'a>(
    env: &mut JNIEnv<'a>,
    shared_ptr: *const Shared,
) -> Result<JObject<'a>> {
    let callback_class = callback::get_callback_class(env)?;

    // `Shared` is `Sized`, so `*const Shared` is a thin pointer that fits in one `jlong`.
    env.new_object(
        callback_class,
        "(J)V",
        &[JValueGen::Long(shared_ptr as jlong)],
    )
    .map_err(|e| map_android_error(env, e))
}

// TODO: Could inner be JObject<'a>?
pub struct Location<'a> {
    inner: GlobalRef,
    phantom: PhantomData<&'a ()>,
}

impl Location<'_> {
    pub fn coordinates(&self) -> Result<Coordinates> {
        robius_android_env::with_activity(|env, _| {
            let latitude = env
                .call_method(&self.inner, "getLatitude", "()D", &[])
                .map_err(|e| map_android_error(env, e))?
                .d()?;
            let longitude = env
                .call_method(&self.inner, "getLongitude", "()D", &[])
                .map_err(|e| map_android_error(env, e))?
                .d()?;
            Ok(Coordinates {
                latitude,
                longitude,
            })
        })
        .map_err(|_| Error::AndroidEnvironment)
        // Poor man's `flatten`
        .and_then(|x| x)
    }

    pub fn altitude(&self) -> Result<f64> {
        robius_android_env::with_activity(|env, _| {
            env.call_method(&self.inner, "getAltitude", "()D", &[])
                .map_err(|e| map_android_error(env, e))?
                .d()
                .map_err(|e| e.into())
        })
        .map_err(|_| Error::AndroidEnvironment)
        .and_then(|x| x)
    }

    pub fn bearing(&self) -> Result<f64> {
        robius_android_env::with_activity(|env, _| {
            let bearing = env
                .call_method(&self.inner, "getBearing", "()F", &[])
                .map_err(|e| map_android_error(env, e))?
                .f()?;
            Ok(bearing as f64)
        })
        .map_err(|_| Error::AndroidEnvironment)
        .and_then(|x| x)
    }

    pub fn speed(&self) -> Result<f64> {
        robius_android_env::with_activity(|env, _| {
            let speed = env
                .call_method(&self.inner, "getSpeed", "()F", &[])
                .map_err(|e| map_android_error(env, e))?
                .f()?;
            Ok(speed as f64)
        })
        .map_err(|_| Error::AndroidEnvironment)
        .and_then(|x| x)
    }

    pub fn time(&self) -> Result<SystemTime> {
        robius_android_env::with_activity(|env, _| {
            // `Location.getTime()` returns milliseconds since the Unix epoch, as a `long`.
            let millis = env
                .call_method(&self.inner, "getTime", "()J", &[])
                .map_err(|e| map_android_error(env, e))?
                .j()?;
            Ok(SystemTime::UNIX_EPOCH + Duration::from_millis(millis as u64))
        })
        .map_err(|_| Error::AndroidEnvironment)
        .and_then(|x| x)
    }
}

impl From<jni::errors::Error> for Error {
    fn from(_: jni::errors::Error) -> Self {
        Error::Unknown
    }
}
