//! Where a call is heard on the phone: the earpiece, or the loudspeaker.
//!
//! Android picks a route from the *mode* the app is in, and the call
//! engine's C API says not one word about any of it — the route is the
//! app's, as it is in every client. So two things over JNI: the mode while a
//! call runs (`MODE_IN_COMMUNICATION`, which puts the sound in the earpiece
//! and wakes the echo canceller a call wants), and the speakerphone the call
//! panel's `speaker` turns on and off.
//!
//! Anywhere else there is one output and both of these are nothing at all: a
//! Mac's call comes out of whatever the system is playing through, and the
//! bar has no `speaker` on it to say otherwise.

/// `AudioManager.MODE_IN_COMMUNICATION` — the mode a call runs in.
#[cfg(target_os = "android")]
const MODE_IN_COMMUNICATION: i32 = 3;

/// `AudioManager.MODE_NORMAL` — the mode everything else runs in.
#[cfg(target_os = "android")]
const MODE_NORMAL: i32 = 0;

/// One setter on the system's `AudioManager`, and what it takes.
#[cfg(target_os = "android")]
enum Set {
    /// `setMode`.
    Mode(i32),
    /// `setSpeakerphoneOn`.
    Speakerphone(bool),
}

/// A call's media is starting, or has ended.
///
/// The mode is set before the first sample deliberately: android decides
/// where a stream goes when the stream opens, so a mode set afterwards would
/// route the next call rather than this one. The end puts the phone back as
/// it was found — the mode normal and the speakerphone off — because a
/// speakerphone left on is the next app's surprise.
pub fn in_call(on: bool) {
    #[cfg(target_os = "android")]
    {
        tell(Set::Mode(if on { MODE_IN_COMMUNICATION } else { MODE_NORMAL }));
        if !on {
            tell(Set::Speakerphone(false));
        }
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = on;
    }
}

/// The call panel's `speaker`: the loudspeaker rather than the earpiece.
pub fn speaker(on: bool) {
    #[cfg(target_os = "android")]
    tell(Set::Speakerphone(on));
    #[cfg(not(target_os = "android"))]
    {
        let _ = on;
    }
}

/// Says it to the `AudioManager`.
///
/// A phone that has not got as far as an activity yet — or one that refuses
/// the call — is silently left alone: the call is still a call, it is simply
/// in whichever ear android put it, and there is nothing a person could do
/// about a message saying so.
#[cfg(target_os = "android")]
fn tell(what: Set) {
    use jni::objects::{JObject, JValue};
    use jni::JavaVM;

    let vm = makepad_android_state::get_java_vm();
    let activity = makepad_android_state::get_activity();
    if vm.is_null() || activity.is_null() {
        return;
    }
    // Makepad owns the VM and the global activity; a local reference for this
    // call, never one kept across a recreation.
    let Ok(vm) = (unsafe { JavaVM::from_raw(vm.cast()) }) else { return };
    let Ok(mut env) = vm.attach_current_thread() else { return };
    let done = env.with_local_frame(8, |env| -> jni::errors::Result<()> {
        let activity = env.new_local_ref(unsafe { JObject::from_raw(activity.cast()) })?;
        let audio = env.new_string("audio")?;
        let manager = env
            .call_method(
                activity,
                "getSystemService",
                "(Ljava/lang/String;)Ljava/lang/Object;",
                &[(&audio).into()],
            )?
            .l()?;
        match what {
            Set::Mode(mode) => {
                env.call_method(&manager, "setMode", "(I)V", &[JValue::Int(mode)])?;
            }
            Set::Speakerphone(on) => {
                env.call_method(
                    &manager,
                    "setSpeakerphoneOn",
                    "(Z)V",
                    &[JValue::Bool(u8::from(on))],
                )?;
            }
        }
        Ok(())
    });
    if done.is_err() {
        // Never leave a pending Java exception on makepad's render thread.
        let _ = env.exception_clear();
    }
}
