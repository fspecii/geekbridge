//! JNI export for `dev.geekbridge.android.NativeBridge.nativeRequest`. Compiled for Android only.
//!
//! Kotlin side: `object NativeBridge { external fun nativeRequest(request: String): String }`, so
//! the receiver is the `NativeBridge` instance (`JObject`), not a class.
//!
//! The panic guard is `EnvUnowned::with_env` (catches before the JVM frame) around
//! `handle_request`, which has its own guard. Request data never reaches an exception message.
use crate::{
    InputError, MAX_REQUEST_BYTES, handle_request, input_error_response,
    mutf8::has_unpaired_surrogate,
};
use jni::{
    Env, EnvUnowned,
    errors::{Error, ErrorPolicy, Result},
    jni_str,
    objects::{JObject, JString},
};
use std::panic::{AssertUnwindSafe, catch_unwind};

/// Modified UTF-8 is never shorter than UTF-8 and at most 1.5x longer (NUL is 2 bytes, a
/// supplementary char 6). Refuse anything beyond twice the request limit without decoding it.
const MAX_MUTF8_BYTES: usize = MAX_REQUEST_BYTES * 2;

/// Only reached when a response string cannot be created or the glue itself fails. Throws one
/// fixed-message `RuntimeException`; the JNI error and any panic payload are dropped unread.
struct FixedMessageThrow;

impl<T: Default> ErrorPolicy<T, Error> for FixedMessageThrow {
    type Captures<'unowned_env_local: 'native_method, 'native_method> = ();

    fn on_error<'unowned_env_local: 'native_method, 'native_method>(
        env: &mut Env<'unowned_env_local>,
        _cap: &mut Self::Captures<'unowned_env_local, 'native_method>,
        _err: Error,
    ) -> Result<T> {
        throw_fixed(env)
    }

    fn on_panic<'unowned_env_local: 'native_method, 'native_method>(
        env: &mut Env<'unowned_env_local>,
        _cap: &mut Self::Captures<'unowned_env_local, 'native_method>,
        payload: Box<dyn std::any::Any + Send + 'static>,
    ) -> Result<T> {
        // Dropping an arbitrary payload may itself panic.
        let _ = catch_unwind(AssertUnwindSafe(|| drop(payload)));
        throw_fixed(env)
    }
}

fn throw_fixed<T: Default>(env: &mut Env<'_>) -> Result<T> {
    // A pending exception (e.g. OutOfMemoryError) already tells Kotlin what happened.
    if !env.exception_check() {
        let _ = env.throw_new(
            jni_str!("java/lang/RuntimeException"),
            jni_str!("geekbridge native bridge failure"),
        );
    }
    Ok(T::default())
}

/// Read the Java string and run the dispatcher. Every failure becomes a structured response;
/// a pending Java exception from a failed read is cleared so the response can still be built.
fn respond(env: &mut Env<'_>, request: &JString<'_>) -> String {
    if request.is_null() {
        return input_error_response(InputError::Null);
    }
    let chars = match request.mutf8_chars(env) {
        Ok(chars) => chars,
        Err(_) => {
            if env.exception_check() {
                env.exception_clear();
            }
            return input_error_response(InputError::Unreadable);
        }
    };
    if chars.to_bytes().len() > MAX_MUTF8_BYTES {
        return input_error_response(InputError::TooLarge);
    }
    if has_unpaired_surrogate(chars.to_bytes()) {
        return input_error_response(InputError::IllFormedText);
    }
    // `to_str` decodes modified UTF-8 into a standard UTF-8 copy; the JVM buffer in `chars` is
    // released when the guard drops, after the response has been built.
    handle_request(&chars.to_str())
}

#[unsafe(no_mangle)]
#[allow(unsafe_code, non_snake_case)]
pub extern "system" fn Java_dev_geekbridge_android_NativeBridge_nativeRequest<'local>(
    mut unowned_env: EnvUnowned<'local>,
    _this: JObject<'local>,
    request: JString<'local>,
) -> JString<'local> {
    unowned_env
        .with_env(|env| -> Result<JString<'local>> {
            let response = respond(env, &request);
            JString::from_str(env, response)
        })
        .resolve::<FixedMessageThrow>()
}
