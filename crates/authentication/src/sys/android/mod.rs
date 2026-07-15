mod callback;

use jni::{
    objects::{GlobalRef, JObject, JValueGen},
    JNIEnv,
};

use crate::{BiometricStrength, Error, Result, Text};

pub(crate) type RawContext = ();

// The Android system context is handled by the `robius-android-env` crate,
// so we don't need to store any state here within `Context`.
#[derive(Debug)]
pub(crate) struct Context;

impl Context {
    pub(crate) fn new(_: RawContext) -> Self {
        Self
    }

    // TODO: fix the async authenticate function
    //
    // #[cfg(feature = "async")]
    // pub(crate) async fn authenticate_async(
    //     &self,
    //     text: Text<'_, '_, '_, '_, '_, '_>,
    //     policy: &Policy,
    // ) -> Result<()> {
    //     if let Ok(inner) = self.authenticate_inner(text, policy)?.await {
    //         inner
    //     } else {
    //         Err(Error::Unknown)
    //     }
    // }

    pub(crate) fn authenticate<F>(
        &self,
        text: Text,
        policy: &Policy,
        callback: F,
    ) -> Result<()>
    where
        F: Fn(Result<()>) + Send + 'static,
    {
        self.authenticate_inner(text, policy, callback)
    }

    fn authenticate_inner<F>(
        &self,
        text: Text,
        policy: &Policy,
        callback: F,
    ) -> Result<()>
    where
        F: Fn(Result<()>) + Send + 'static,
    {
        if text.android.title.is_empty() {
            // Builder.build() throws on an empty title.
            return Err(Error::InvalidText);
        }
        robius_android_env::with_activity(|env, context| {
            let callback_class = callback::get_callback_class(env)?;
            let callback_boxed_dyn = Box::new(callback) as Box<dyn Fn(Result<()>) + Send>;
            let callback_boxed_boxed_ptr = Box::into_raw(Box::new(callback_boxed_dyn));

            // with_activity attaches the thread permanently, so on a long-lived
            // native thread the local refs never get freed. Scope them in a local
            // frame so the ref table doesn't overflow.
            let result = env.with_local_frame(16, |env| {
                show_prompt(
                    env,
                    context,
                    callback_class,
                    callback_boxed_boxed_ptr as i64,
                    policy,
                    &text,
                )
            });

            if result.is_err() {
                // Prompt never showed, so Java won't invoke or free the callback.
                // Free it here so it doesn't leak.
                if env.exception_check().unwrap_or(false) {
                    let _ = env.exception_clear();
                }
                drop(unsafe { Box::from_raw(callback_boxed_boxed_ptr) });
            }
            result
        })
        .map_err(Error::from)?
    }
}

fn show_prompt(
    env: &mut JNIEnv<'_>,
    context: &JObject<'_>,
    callback_class: &GlobalRef,
    callback_box_ptr: i64,
    policy: &Policy,
    text: &Text,
) -> Result<()> {
    let callback_instance = construct_callback(env, callback_class, callback_box_ptr)?;
    let cancellation_signal = construct_cancellation_signal(env)?;
    let executor = get_executor(env, context)?;

    let biometric_prompt =
        construct_biometric_prompt(env, context, policy, text, &executor, &callback_instance)?;

    // Once authenticate returns, the framework holds its own ref to the callback,
    // so our local refs can be dropped.
    env.call_method(
        biometric_prompt,
        "authenticate",
        "(\
         Landroid/os/CancellationSignal;\
         Ljava/util/concurrent/Executor;\
         Landroid/hardware/biometrics/BiometricPrompt$AuthenticationCallback;\
         )V",
        &[
            JValueGen::Object(&cancellation_signal),
            JValueGen::Object(&executor),
            JValueGen::Object(&callback_instance),
        ],
    )?;

    Ok(())
}

#[derive(Debug)]
pub(crate) struct Policy {
    strength: Option<BiometricStrength>,
    password: bool,
}

impl Policy {
    #[inline]
    pub(crate) fn set_action_id(&mut self, _: String) -> Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct PolicyBuilder {
    biometrics: Option<BiometricStrength>,
    password: bool,
}

impl PolicyBuilder {
    pub(crate) const fn new() -> Self {
        Self {
            biometrics: Some(BiometricStrength::Strong),
            password: true,
        }
    }

    pub(crate) const fn biometrics(self, biometrics: Option<BiometricStrength>) -> Self {
        Self { biometrics, ..self }
    }

    pub(crate) const fn password(self, password: bool) -> Self {
        Self { password, ..self }
    }

    pub(crate) const fn companion(self, _: bool) -> Self {
        self
    }

    pub(crate) const fn wrist_detection(self, _: bool) -> Self {
        self
    }

    pub(crate) fn action_ids(self, _: Vec<String>) -> Self {
        self
    }

    pub(crate) const fn build(self) -> Option<Policy> {
        // Need at least one method on. Biometrics-only, credential-only (API 29+),
        // and both are all fine with BiometricPrompt.
        if self.biometrics.is_none() && !self.password {
            return None;
        }
        Some(Policy {
            strength: self.biometrics,
            password: self.password,
        })
    }
}

fn construct_callback<'a>(
    env: &mut JNIEnv<'a>,
    class: &GlobalRef,
    callback_box_ptr: i64,
) -> Result<JObject<'a>> {
    env.new_object(class, "(J)V", &[JValueGen::Long(callback_box_ptr)])
        .map_err(|e| e.into())
}

fn construct_cancellation_signal<'a>(env: &mut JNIEnv<'a>) -> Result<JObject<'a>> {
    env.new_object("android/os/CancellationSignal", "()V", &[])
        .map_err(|e| e.into())
}

fn get_executor<'a, 'o, O>(env: &mut JNIEnv<'a>, context: O) -> Result<JObject<'a>>
where
    O: AsRef<JObject<'o>>,
{
    env.call_method(
        context,
        "getMainExecutor",
        "()Ljava/util/concurrent/Executor;",
        &[],
    )?
    .l()
    .map_err(|e| e.into())
}

fn construct_biometric_prompt<'a>(
    env: &mut JNIEnv<'a>,
    context: &JObject<'_>,
    policy: &Policy,
    text: &Text,
    executor: &JObject<'_>,
    negative_button_listener: &JObject<'_>,
) -> Result<JObject<'a>> {
    // `BiometricManager.Authenticators` constants.
    const STRONG: i32 = 0xf;
    const WEAK: i32 = 0xff;
    const CREDENTIAL: i32 = 0x8000;
    // `android.R.string.cancel`, a stable public resource ID.
    const ANDROID_R_STRING_CANCEL: i32 = 17039360;

    let sdk_int = env
        .get_static_field("android/os/Build$VERSION", "SDK_INT", "I")?
        .i()?;

    let biometrics_mask = match policy.strength {
        Some(BiometricStrength::Strong) => STRONG,
        Some(BiometricStrength::Weak) => WEAK,
        None => 0,
    };
    // Device credential (PIN/pattern/password) fallback needs API 29+
    // (setDeviceCredentialAllowed) or API 30+ (setAllowedAuthenticators).
    let credential_allowed = policy.password && sdk_int >= 29;

    if biometrics_mask == 0 && !credential_allowed {
        // Credential-only doesn't work before API 29.
        return Err(Error::Unavailable);
    }

    let builder = env.new_object(
        "android/hardware/biometrics/BiometricPrompt$Builder",
        "(Landroid/content/Context;)V",
        &[JValueGen::Object(context)],
    )?;

    env.call_method(
        &builder,
        "setTitle",
        "(Ljava/lang/CharSequence;)Landroid/hardware/biometrics/BiometricPrompt$Builder;",
        &[JValueGen::Object(
            &env.new_string(text.android.title)?.into(),
        )],
    )?;

    if let Some(subtitle) = text.android.subtitle {
        env.call_method(
            &builder,
            "setSubtitle",
            "(Ljava/lang/CharSequence;)Landroid/hardware/biometrics/BiometricPrompt$Builder;",
            &[JValueGen::Object(&env.new_string(subtitle)?.into())],
        )?;
    }
    if let Some(description) = text.android.description {
        env.call_method(
            &builder,
            "setDescription",
            "(Ljava/lang/CharSequence;)Landroid/hardware/biometrics/BiometricPrompt$Builder;",
            &[JValueGen::Object(&env.new_string(description)?.into())],
        )?;
    }

    if sdk_int >= 30 {
        env.call_method(
            &builder,
            "setAllowedAuthenticators",
            "(I)Landroid/hardware/biometrics/BiometricPrompt$Builder;",
            &[JValueGen::Int(
                biometrics_mask | if credential_allowed { CREDENTIAL } else { 0 },
            )],
        )?;
    } else if credential_allowed {
        // API 29: `setAllowedAuthenticators` doesn't exist yet.
        env.call_method(
            &builder,
            "setDeviceCredentialAllowed",
            "(Z)Landroid/hardware/biometrics/BiometricPrompt$Builder;",
            &[JValueGen::Bool(1)],
        )?;
    }

    if !credential_allowed {
        // Framework wants a negative button when there's no credential fallback;
        // build() throws otherwise.
        let cancel_text = env
            .call_method(
                context,
                "getString",
                "(I)Ljava/lang/String;",
                &[JValueGen::Int(ANDROID_R_STRING_CANCEL)],
            )?
            .l()?;
        env.call_method(
            &builder,
            "setNegativeButton",
            "(\
             Ljava/lang/CharSequence;\
             Ljava/util/concurrent/Executor;\
             Landroid/content/DialogInterface$OnClickListener;\
             )Landroid/hardware/biometrics/BiometricPrompt$Builder;",
            &[
                JValueGen::Object(&cancel_text),
                JValueGen::Object(executor),
                JValueGen::Object(negative_button_listener),
            ],
        )?;
    }

    env.call_method(
        builder,
        "build",
        "()Landroid/hardware/biometrics/BiometricPrompt;",
        &[],
    )?
    .l()
    .map_err(|e| e.into())
}
