//! What the phone answers for that a Mac does not.
//!
//! Three things: the application context the crates that reach the system
//! ask for, [`recorder`], the video encoder the fork carries no android
//! backend for, and [`screen_turns`], which is how the camera's picture is
//! stood upright. The first and the last are JNI calls on makepad's own
//! handles.
//!
//! **The context.** Iroh's DNS resolver reads the active network's servers
//! over JNI through `ndk_context`, which ndk-glue and android-activity fill
//! in before `main`. Makepad is neither, so the app fills it in itself,
//! once, from the handles Makepad already owns — with the application
//! context rather than the activity, because a folded or rotated activity is
//! recreated and the process outlives it.

use std::sync::OnceLock;

pub mod recorder;

static INSTALLED: OnceLock<Result<(), String>> = OnceLock::new();

/// Installs the process's Android context for `ndk_context` readers. The
/// first call does the work; later calls answer the same result.
pub fn install_context() -> Result<(), String> {
    INSTALLED.get_or_init(install).clone()
}

fn install() -> Result<(), String> {
    use jni::{objects::JObject, JavaVM};

    let vm = makepad_android_state::get_java_vm();
    let activity = makepad_android_state::get_activity();
    if vm.is_null() || activity.is_null() {
        return Err("Android's activity is not up yet".into());
    }
    let java_vm = unsafe { JavaVM::from_raw(vm.cast()) }.map_err(|e| e.to_string())?;
    let mut env = java_vm.attach_current_thread().map_err(|e| e.to_string())?;
    let context = env.with_local_frame_returning_local(8, |env| {
        let activity = env.new_local_ref(unsafe { JObject::from_raw(activity.cast()) })?;
        env.call_method(activity, "getApplicationContext", "()Landroid/content/Context;", &[])?
            .l()
    });
    let context = match context {
        Ok(context) => context,
        Err(e) => {
            // Never leave a pending Java exception on Makepad's thread.
            let _ = env.exception_clear();
            return Err(e.to_string());
        }
    };
    // Kept for the life of the process: `ndk_context` holds the raw pointer.
    let global = env.new_global_ref(context).map_err(|e| e.to_string())?;
    let raw = global.as_raw();
    std::mem::forget(global);
    unsafe { ndk_context::initialize_android_context(vm.cast(), raw.cast()) };
    Ok(())
}

/// How the screen is turned right now, in quarter turns anticlockwise from
/// the device's natural orientation: `Display.getRotation()`, whose
/// `Surface.ROTATION_0`, `_90`, `_180` and `_270` are 0, 1, 2 and 3.
///
/// The camera is what asks. A frame comes out of the sensor lying the way
/// the sensor is bolted to the body, and which quarter turn stands it
/// upright depends on how the person is holding the phone at that moment —
/// so this is read when a camera session opens and again on every geometry
/// change, and never assumed.
///
/// Nought is the answer to every failure — a phone with no activity up yet,
/// a window manager that refuses the call — because nought is the phone
/// held the way phones are held, and a picture that is a quarter turn out
/// is worth more than a panel that says so.
pub fn screen_turns() -> u8 {
    use jni::objects::JObject;
    use jni::JavaVM;

    let vm = makepad_android_state::get_java_vm();
    let activity = makepad_android_state::get_activity();
    if vm.is_null() || activity.is_null() {
        return 0;
    }
    // Makepad owns the VM and the global activity; a local reference for this
    // call, never one kept across a recreation.
    let Ok(vm) = (unsafe { JavaVM::from_raw(vm.cast()) }) else { return 0 };
    let Ok(mut env) = vm.attach_current_thread() else { return 0 };
    let turned = env.with_local_frame(8, |env| -> jni::errors::Result<i32> {
        let activity = env.new_local_ref(unsafe { JObject::from_raw(activity.cast()) })?;
        let windows = env
            .call_method(
                activity,
                "getWindowManager",
                "()Landroid/view/WindowManager;",
                &[],
            )?
            .l()?;
        let display = env
            .call_method(windows, "getDefaultDisplay", "()Landroid/view/Display;", &[])?
            .l()?;
        env.call_method(display, "getRotation", "()I", &[])?.i()
    });
    match turned {
        Ok(rotation) => rotation.rem_euclid(4) as u8,
        Err(_) => {
            // Never leave a pending Java exception on makepad's render thread.
            let _ = env.exception_clear();
            0
        }
    }
}
