//! Open external links in the system browser. The pinned Makepad Android
//! backend leaves `Cx::open_url` unimplemented, so Android uses ACTION_VIEW.

use makepad_widgets::Cx;

pub fn open_or_notify(cx: &mut Cx, url: &str, scope: &mut makepad_widgets::Scope) {
    if let Err(error) = open(cx, url) {
        if let Some(session) = scope.data.get_mut::<kernel::session::Session>() {
            session.notify(error, true);
        }
    }
}

pub fn open(cx: &mut Cx, url: &str) -> Result<(), String> {
    #[cfg(target_os = "android")]
    {
        let _ = cx;
        android_open(url)
    }
    #[cfg(not(target_os = "android"))]
    {
        use makepad_widgets::{CxOsApi, OpenUrlInPlace};
        cx.open_url(url, OpenUrlInPlace::No);
        Ok(())
    }
}

#[cfg(target_os = "android")]
fn android_open(url: &str) -> Result<(), String> {
    use jni::{objects::JObject, JavaVM};

    let vm = makepad_android_state::get_java_vm();
    let activity = makepad_android_state::get_activity();
    if vm.is_null() || activity.is_null() {
        return Err("Android's browser is not available yet; try again".into());
    }
    // Makepad owns these VM/global activity handles. Take a fresh local
    // reference for this call; never retain an activity across recreation.
    let vm =
        unsafe { JavaVM::from_raw(vm.cast()) }.map_err(|_| "could not reach Android's browser")?;
    let mut env = vm
        .attach_current_thread()
        .map_err(|_| "could not reach Android's browser")?;
    let result = env.with_local_frame(16, |env| -> jni::errors::Result<()> {
        let activity = env.new_local_ref(unsafe { JObject::from_raw(activity.cast()) })?;
        let address = env.new_string(url)?;
        let uri = env
            .call_static_method(
                "android/net/Uri",
                "parse",
                "(Ljava/lang/String;)Landroid/net/Uri;",
                &[(&address).into()],
            )?
            .l()?;
        let action = env.new_string("android.intent.action.VIEW")?;
        let intent = env.new_object(
            "android/content/Intent",
            "(Ljava/lang/String;Landroid/net/Uri;)V",
            &[(&action).into(), (&uri).into()],
        )?;
        env.call_method(
            activity,
            "startActivity",
            "(Landroid/content/Intent;)V",
            &[(&intent).into()],
        )?;
        Ok(())
    });
    if result.is_err() {
        // A missing browser must not leave a pending Java exception on
        // Makepad's render thread. Do not log a URL carrying OAuth state.
        let _ = env.exception_clear();
    }
    result.map_err(|_| "Android could not open the link; enable a browser and try again".into())
}
