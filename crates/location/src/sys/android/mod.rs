mod callback;

use std::{
    marker::PhantomData,
    sync::Mutex,
    time::{Duration, SystemTime},
};

use jni::{
    objects::{GlobalRef, JObject, JValueGen},
    JNIEnv,
};

use crate::{Access, Accuracy, Coordinates, Error, Handler, Result};

type InnerHandler = Mutex<dyn Handler>;

pub struct Manager {
    callback: GlobalRef,
    // We "leak" the handler so that `rust_callback` can safely access it, and then when dropping
    // the manager we make sure that `rust_callback` will never be called again before reboxing
    // (and hence deallocating) the handler. See the `Drop` implementation for more details.
    inner: *const InnerHandler,
}

impl Manager {
    pub fn new<T>(handler: T) -> Result<Self>
    where
        T: Handler,
    {
        let inner = Box::into_raw(Box::new(Mutex::new(handler)));

        Ok(Manager {
            callback: robius_android_env::with_activity(|env, _| {
                let callback = construct_callback(env, inner)?;
                env.new_global_ref(callback).map_err(|e| e.into())
            })
            .map_err(|_| Error::AndroidEnvironment)
            .and_then(|x| x)?,
            inner,
        })
    }

    pub fn request_authorization(&self, _access: Access, accuracy: Accuracy) -> Result<()> {
        robius_android_env::with_activity(|env, current_activity| {
            let permissions = env.new_string(match accuracy {
                Accuracy::Approximate => "android.permission.ACCESS_COARSE_LOCATION",
                Accuracy::Precise => "android.permission.ACCESS_FINE_LOCATION",
            })?;

            let array = env.new_object_array(1, "java/lang/String", permissions)?;
            // TODO: Ideally we would provide functionality to wait for for authorization.
            let request_code = 3;

            env.call_method(
                current_activity,
                "requestPermissions",
                "([Ljava/lang/String;I)V",
                &[JValueGen::Object(&array), JValueGen::Int(request_code)],
            )?;

            Ok(())
        })
        .map_err(|_| Error::AndroidEnvironment)
        .and_then(|x| x)
    }

    pub fn update_once(&self) -> Result<()> {
        robius_android_env::with_activity(|env, context| {
            let manager = get_location_manager(env, context)?;
            let provider = env.new_string("fused")?;
            let executor = get_executor(env, context)?;

            env.call_method(
                manager,
                "getCurrentLocation",
                "(Ljava/lang/String;Landroid/os/CancellationSignal;Ljava/util/concurrent/Executor;\
                 Ljava/util/function/Consumer;)V",
                &[
                    JValueGen::Object(&provider),
                    JValueGen::Object(&JObject::null()),
                    JValueGen::Object(&executor),
                    JValueGen::Object(&self.callback),
                ],
            )?;

            Ok(())
        })
        .map_err(|_| Error::AndroidEnvironment)
        .and_then(|x| x)
    }

    pub fn start_updates(&self) -> Result<()> {
        robius_android_env::with_activity(|env, context| {
            let manager = get_location_manager(env, context)?;
            let provider = env.new_string("fused")?;
            let request = construct_location_request(env)?;
            let executor = get_executor(env, context)?;

            env.call_method(
                manager,
                "requestLocationUpdates",
                "(Ljava/lang/String;Landroid/location/LocationRequest;Ljava/util/concurrent/\
                 Executor;Landroid/location/LocationListener;)V",
                &[
                    JValueGen::Object(&provider),
                    JValueGen::Object(&request),
                    JValueGen::Object(&executor),
                    JValueGen::Object(&self.callback),
                ],
            )?;

            Ok(())
        })
        .map_err(|_| Error::AndroidEnvironment)
        .and_then(|x| x)
    }

    pub fn stop_updates(&self) -> Result<()> {
        robius_android_env::with_activity(|env, context| {
            let manager = get_location_manager(env, context)?;
            env.call_method(
                manager,
                "removeUpdates",
                "(Landroid/location/LocationListener;)V",
                &[JValueGen::Object(&self.callback)],
            )?;
            Ok(())
        })
        .map_err(|_| Error::AndroidEnvironment)
        .and_then(|x| x)
    }
}

impl Drop for Manager {
    fn drop(&mut self) {
        // NOTE: We want to unwrap in this function as otherwise could lead to memory
        // unsafety.

        // From https://developer.android.com/reference/android/location/LocationManager#removeUpdates(android.location.LocationListener):
        // The given listener is guaranteed not to receive any invocations that
        // happens-after this method is invoked.
        self.stop_updates().unwrap();

        // This is just to avoid some funky race conditions with the Java function. By
        // using two variables we ensure that if our check happens to occur
        // between the function start and `this.executing = true` (in e.g.
        // `onLocationChanged`), `rustCallback` still won't be called. This could
        // probably be done more efficiently in Rust using atomics.
        robius_android_env::with_activity(|env, _| {
            env.call_method(&self.callback, "disableExecution", "()V", &[])
                .unwrap();
        })
        .unwrap();

        // So now we are mostly ok to drop the handler except for the fact that a
        // `rust_callback` invocation may currently be executing. Hence, we have to keep
        // track of that (in Java).
        let mut executing = true;

        while executing {
            executing = robius_android_env::with_activity(|env, _| {
                env.call_method(&self.callback, "isExecuting", "()Z", &[])?
                    .z()
            })
            .unwrap()
            .unwrap();
        }

        // SAFETY: We have stopped updates and so `rust_callback` will never be invoked
        // again. Moreover we have waited for any `rust_callback` invocations to
        // finish executing. Hence, nothing else will ever touch the data behind
        // this pointer and so we can safely deallocate it.
        let _ = unsafe { Box::from_raw(self.inner as *mut InnerHandler) };
    }
}

fn get_location_manager<'a>(env: &mut JNIEnv<'a>, context: &JObject<'_>) -> Result<JObject<'a>> {
    let service_name = env.new_string("location")?;

    env.call_method(
        context,
        "getSystemService",
        "(Ljava/lang/String;)Ljava/lang/Object;",
        &[JValueGen::Object(&service_name)],
    )?
    .l()
    .map_err(|e| e.into())
}

fn get_executor<'a>(env: &mut JNIEnv<'a>, context: &JObject<'_>) -> Result<JObject<'a>> {
    env.call_method(
        context,
        "getMainExecutor",
        "()Ljava/util/concurrent/Executor;",
        &[],
    )?
    .l()
    .map_err(|e| e.into())
}

fn construct_callback<'a>(
    env: &mut JNIEnv<'a>,
    handler_ptr: *const InnerHandler,
) -> Result<JObject<'a>> {
    let callback_class = callback::get_callback_class(env)?;

    // TODO: Is there a better way without the provenance API?
    let transmuted: [i64; 2] = unsafe { std::mem::transmute(handler_ptr) };
    env.new_object(
        callback_class,
        "(JJ)V",
        &[
            JValueGen::Long(transmuted[0]),
            JValueGen::Long(transmuted[1]),
        ],
    )
    .map_err(|e| e.into())
}

fn construct_location_request<'a>(env: &mut JNIEnv<'a>) -> Result<JObject<'a>> {
    let builder = env.new_object(
        "android/location/LocationRequest$Builder",
        "(J)V",
        // TODO: Don't hardcode
        // TODO: Could use:
        // https://developer.android.com/reference/android/location/LocationRequest#PASSIVE_INTERVAL
        // but then we have to determine a minupdateinterval.
        &[JValueGen::Long(1000)],
    )?;

    env.call_method(
        builder,
        "build",
        "()Landroid/location/LocationRequest;",
        &[],
    )?
    .l()
    .map_err(|e| e.into())
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
                .call_method(&self.inner, "getLatitude", "()D", &[])?
                .d()?;
            let longitude = env
                .call_method(&self.inner, "getLongitude", "()D", &[])?
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
            env.call_method(&self.inner, "getAltitude", "()D", &[])?
                .d()
                .map_err(|e| e.into())
        })
        .map_err(|_| Error::AndroidEnvironment)
        .and_then(|x| x)
    }

    pub fn bearing(&self) -> Result<f64> {
        robius_android_env::with_activity(|env, _| {
            match env.call_method(&self.inner, "getBearing", "()F", &[])?.f() {
                Ok(bearing) => Ok(bearing as f64),
                Err(e) => Err(e.into()),
            }
        })
        .map_err(|_| Error::AndroidEnvironment)
        .and_then(|x| x)
    }

    pub fn speed(&self) -> Result<f64> {
        robius_android_env::with_activity(|env, _| {
            match env.call_method(&self.inner, "getSpeed", "()F", &[])?.f() {
                Ok(speed) => Ok(speed as f64),
                Err(e) => Err(e.into()),
            }
        })
        .map_err(|_| Error::AndroidEnvironment)
        .and_then(|x| x)
    }

    pub fn time(&self) -> Result<SystemTime> {
        robius_android_env::with_activity(|env, _| {
            match env.call_method(&self.inner, "getTime", "()J", &[])?.f() {
                Ok(time) => Ok(time as f64),
                Err(e) => Err(e.into()),
            }
        })
        .map_err(|_| Error::AndroidEnvironment)
        .and_then(|x| x)
        .map(|secs| SystemTime::UNIX_EPOCH + Duration::from_secs_f64(secs))
    }
}

impl From<jni::errors::Error> for Error {
    fn from(_: jni::errors::Error) -> Self {
        Error::Unknown
    }
}
