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
        robius_android_env::with_activity(|env, context| {
            let callback_class = callback::get_callback_class(env)?;
            let callback_boxed_dyn = Box::new(callback) as Box<dyn Fn(Result<()>)>;
            let callback_boxed_boxed_ptr = Box::into_raw(Box::new(callback_boxed_dyn));
            let callback_instance = construct_callback(
                env,
                callback_class,
                callback_boxed_boxed_ptr as i64,
            )?;
            let cancellation_signal = construct_cancellation_signal(env)?;
            let executor = get_executor(env, context)?;

            let biometric_prompt = construct_biometric_prompt(env, context, policy, &text)?;

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
        })
        .map_err(|e| Error::Java(e))?
    }
}

#[derive(Debug)]
pub(crate) struct Policy {
    #[allow(dead_code)]
    strength: BiometricStrength,
    password: bool,
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

    pub(crate) const fn build(self) -> Option<Policy> {
        if let Some(strength) = self.biometrics {
            return Some(Policy {
                strength,
                password: self.password,
            });
        }
        None
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
) -> Result<JObject<'a>> {
    let context = env.new_global_ref(context).unwrap();

    let builder = env.new_object(
        "android/hardware/biometrics/BiometricPrompt$Builder",
        "(Landroid/content/Context;)V",
        &[JValueGen::Object(context.as_ref())],
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
    const STRONG: i32 = 0xf;
    const WEAK: i32 = 0xff;
    const CREDENTIAL: i32 = 0x8000;

    env.call_method(
        &builder,
        "setAllowedAuthenticators",
        "(I)Landroid/hardware/biometrics/BiometricPrompt$Builder;",
        &[JValueGen::Int(
            match policy.strength {
                BiometricStrength::Strong => STRONG,
                BiometricStrength::Weak => WEAK,
            } | if policy.password { CREDENTIAL } else { 0 },
        )],
    )?;

    env.call_method(
        builder,
        "build",
        "()Landroid/hardware/biometrics/BiometricPrompt;",
        &[],
    )?
    .l()
    .map_err(|e| e.into())
}
