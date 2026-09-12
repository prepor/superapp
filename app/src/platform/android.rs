//! Android's application context, handed to the crates that ask for one.
//!
//! Iroh's DNS resolver reads the active network's servers over JNI through
//! `ndk_context`, which ndk-glue and android-activity fill in before `main`.
//! Makepad is neither, so the app fills it in itself, once, from the handles
//! Makepad already owns — with the application context rather than the
//! activity, because a folded or rotated activity is recreated and the
//! process outlives it.

use std::sync::OnceLock;

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
