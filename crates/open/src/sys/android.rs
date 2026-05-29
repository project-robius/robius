use jni::objects::{JObject, JValueGen};
use jni::JNIEnv;

use crate::{Error, Result};

pub(crate) struct Uri<'a, 'b> {
    inner: &'a str,
    action: &'b str,
}

impl<'a, 'b> Uri<'a, 'b> {
    pub(crate) fn new(inner: &'a str) -> Self {
        Self {
            inner,
            action: "ACTION_VIEW",
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    pub(crate) fn action(self, action: &'b str) -> Self {
        Self { action, ..self }
    }

    pub fn open<F>(self, on_completion: F) -> Result<()>
    where
        F: Fn(bool) + 'static,
    {
        let action = self.action;
        let uri = self.inner;
        let res = robius_android_env::with_activity(|env, current_activity| {
            let outcome = open_intent(env, current_activity, action, uri);
            // Clear any pending exceptions to avoid misinterpreting stale/wrong exceptions.
            if matches!(env.exception_check(), Ok(true)) {
                let _ = env.exception_clear();
            }
            outcome
        });

        match res {
            Ok(Ok(opened)) => {
                on_completion(opened);
                if opened {
                    Ok(())
                } else {
                    Err(Error::NoHandler)
                }
            }
            Ok(Err(e)) => {
                #[cfg(feature = "log")]
                log::error!("robius-open: failed to start activity: {e:?}");
                Err(e)
            }
            Err(_e) => {
                #[cfg(feature = "log")]
                log::error!(
                    "Couldn't get current activity or JVM/JNI. Did you set up `robius_android_env` correctly?"
                );
                Err(Error::AndroidEnvironment)
            }
        }
    }
}

/// Builds and launches an `ACTION_VIEW` intent for the given `uri`.
///
/// This catches an `ActivityNotFoundException` and returns it as a no handler error.
///
/// * Returns `Ok(true)` if successfully launched and handled
/// * Returns `Ok(false)` if nothing was able to handle the URI
/// * Returns an `Err` otherwise
fn open_intent(
    env: &mut JNIEnv<'_>,
    current_activity: &JObject<'_>,
    action: &str,
    uri: &str,
) -> Result<bool> {
    let action = env.get_static_field("android/content/Intent", action, "Ljava/lang/String;")?.l()?;

    let string = env.new_string(uri).map_err(|_| Error::MalformedUri)?;
    let uri = env.call_static_method(
        "android/net/Uri",
        "parse",
        "(Ljava/lang/String;)Landroid/net/Uri;",
        &[JValueGen::Object(&string)],
    )?
    .l()?;

    let intent = env.new_object(
        "android/content/Intent",
        "(Ljava/lang/String;Landroid/net/Uri;)V",
        &[JValueGen::Object(&action), JValueGen::Object(&uri)],
    )?;

    match env.call_method(
        current_activity,
        "startActivity",
        "(Landroid/content/Intent;)V",
        &[JValueGen::Object(&intent)],
    ) {
        Ok(_) => Ok(true),
        Err(jni::errors::Error::JavaException) => Ok(false),
        Err(other) => Err(Error::Java(other)),
    }
}
