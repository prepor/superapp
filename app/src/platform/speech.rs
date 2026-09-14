//! The voice this machine has: the kernel's [`Speech`] over whatever the
//! platform speaks with.
//!
//! macOS has `AVSpeechSynthesizer` and android `TextToSpeech`. Neither is
//! waited on — a card's *play* may not hold the frame for as long as a
//! sentence takes — so both are handed the words and answer at once, and
//! what [`Speech::speak`] answers is that the voice took them, never that
//! it reached the end of them.
//!
//! One speaker for the life of the app rather than one per sentence: making
//! one costs an audio session, and a second card ought to cut the first one
//! off rather than talk over it.
//!
//! Only a real run that nobody scripts gets this, beside the real clipboard
//! (`shell::boot`): a suite that spoke would be heard by whoever is at the
//! machine, and a library mount is somebody's scene catalogue. Everywhere
//! else — a scripted run, a mount, a platform with no synthesizer — there
//! is no voice at all, and the course says so in the panel rather than
//! going quiet and pretending.

use kernel::caps::Speech;

#[cfg(target_os = "macos")]
use mac::Voice;

#[cfg(target_os = "android")]
use droid::Voice;

#[cfg(not(any(target_os = "macos", target_os = "android")))]
use elsewhere::Voice;

/// The platform's own voice, as the kernel asks for it.
#[derive(Default)]
pub struct RealSpeech(Voice);

impl RealSpeech {
    #[must_use]
    pub fn new() -> RealSpeech {
        RealSpeech::default()
    }
}

impl Speech for RealSpeech {
    fn speak(&mut self, text: &str, lang: &str) -> Result<(), String> {
        self.0.speak(text, lang)
    }

    fn stop(&mut self) {
        self.0.stop();
    }
}

// -- macOS ---------------------------------------------------------------------

#[cfg(target_os = "macos")]
mod mac {
    use makepad_apple_sys::*;
    use makepad_objc_sys::rc::autoreleasepool;

    // The three classes below are reached by name — `class!` asks the
    // runtime for them — and the runtime knows a class only once the
    // framework that declares it is loaded. Named here rather than left to
    // whatever else in the build happens to link AVFoundation.
    #[link(name = "AVFoundation", kind = "framework")]
    extern "C" {}

    /// The rate the original course reads at: a shade under
    /// `AVSpeechUtteranceDefaultSpeechRate` (0.5), which is a learner's pace
    /// without being the slow one that turns a word into syllables.
    const RATE: f32 = 0.48;

    /// `AVSpeechBoundaryImmediate`: stop where the voice is, not at the end
    /// of the word it is in the middle of.
    const IMMEDIATE: i64 = 0;

    /// One `AVSpeechSynthesizer`, made on the first word and kept. Not
    /// `Send`: the capability belongs to the window's world, which is the
    /// thread that made it.
    pub(super) struct Voice {
        synthesizer: ObjcId,
    }

    impl Default for Voice {
        fn default() -> Voice {
            Voice { synthesizer: nil }
        }
    }

    impl Voice {
        pub(super) fn speak(&mut self, text: &str, lang: &str) -> Result<(), String> {
            let synthesizer = self.synthesizer()?;
            // The utterance and the voice come back autoreleased, and a
            // pool of this thread's own is what drains them — a plain Rust
            // thread has no ambient one. Draining here does not take the
            // sentence back: a synthesizer retains what it was handed until
            // it has finished reading it.
            autoreleasepool(|| unsafe {
                let utterance: ObjcId = msg_send![
                    class!(AVSpeechUtterance),
                    speechUtteranceWithString: str_to_nsstring(text)
                ];
                if utterance == nil {
                    return Err("AVFoundation made no utterance of it".to_string());
                }
                // A tag this Mac has no voice for is not silence: the
                // default voice reads it, badly, which is more use than a
                // card that does nothing because one language is missing.
                let voice: ObjcId = msg_send![
                    class!(AVSpeechSynthesisVoice),
                    voiceWithLanguage: str_to_nsstring(lang)
                ];
                if voice != nil {
                    let () = msg_send![utterance, setVoice: voice];
                }
                let () = msg_send![utterance, setRate: RATE];
                // Returns at once — AVFoundation reads it on its own
                // thread — so the frame this was asked from goes on.
                let () = msg_send![synthesizer, speakUtterance: utterance];
                Ok(())
            })
        }

        pub(super) fn stop(&mut self) {
            if self.synthesizer == nil {
                return;
            }
            // SAFETY: the synthesizer this made, on the thread that made
            // it. The answer is whether anything was being said, which is
            // not news.
            unsafe {
                let _: BOOL = msg_send![self.synthesizer, stopSpeakingAtBoundary: IMMEDIATE];
            }
        }

        /// The synthesizer, made on the first sentence and kept after.
        fn synthesizer(&mut self) -> Result<ObjcId, String> {
            if self.synthesizer == nil {
                // SAFETY: `+new` — alloc and init — on a class the linked
                // framework always has; owned here, released in `Drop`.
                self.synthesizer = unsafe { msg_send![class!(AVSpeechSynthesizer), new] };
            }
            if self.synthesizer == nil {
                return Err("AVFoundation gave no synthesizer".to_string());
            }
            Ok(self.synthesizer)
        }
    }

    impl Drop for Voice {
        fn drop(&mut self) {
            if self.synthesizer != nil {
                // SAFETY: released once, on the thread that made it.
                unsafe {
                    let () = msg_send![self.synthesizer, release];
                }
            }
        }
    }
}

// -- android -------------------------------------------------------------------

#[cfg(target_os = "android")]
mod droid {
    //! `android.speech.tts.TextToSpeech` over JNI, on a thread of its own.
    //!
    //! The engine is a service the app binds to, and it answers nothing
    //! until the binding is made — a sentence spoken in that first moment
    //! is refused rather than queued. So the first one waits for the engine
    //! and the rest do not, and the waiting is done on this thread instead
    //! of the one drawing frames. An utterance asked for before the engine
    //! is ready is therefore *held*, not lost, unless the engine never
    //! comes up at all: after [`READY_TRIES`] it is spoken anyway and
    //! whatever the platform says of it goes to the log.

    use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
    use std::thread;
    use std::time::Duration;

    use jni::objects::{GlobalRef, JObject, JValue};
    use jni::{JNIEnv, JavaVM};

    /// How long the first sentence waits for the engine to bind, and how
    /// often it asks: three seconds, fifty milliseconds apart.
    const READY_TRIES: u32 = 60;
    const READY_WAIT: Duration = Duration::from_millis(50);

    /// `TextToSpeech.QUEUE_FLUSH`: a card that speaks cuts off the one
    /// before it, as the Mac's single synthesizer does.
    const QUEUE_FLUSH: i32 = 0;

    /// What the thread is told to do. A sentence owns its words, because
    /// the thread outlives the frame that asked for it.
    enum Job {
        Say { text: String, lang: String },
        Stop,
    }

    /// The near end: a channel, and nothing until the first word.
    #[derive(Default)]
    pub(super) struct Voice {
        jobs: Option<SyncSender<Job>>,
    }

    impl Voice {
        pub(super) fn speak(&mut self, text: &str, lang: &str) -> Result<(), String> {
            let job = Job::Say {
                text: text.to_string(),
                lang: lang.to_string(),
            };
            match self.jobs.get_or_insert_with(start).try_send(job) {
                Ok(()) => Ok(()),
                Err(TrySendError::Full(_)) => {
                    Err("the voice has not finished what it was given".to_string())
                }
                Err(TrySendError::Disconnected(_)) => Err("this phone's voice stopped".to_string()),
            }
        }

        /// Silence. Never starts an engine: nothing has been said if there
        /// is no thread to say it.
        pub(super) fn stop(&mut self) {
            if let Some(jobs) = &self.jobs {
                let _ = jobs.try_send(Job::Stop);
            }
        }
    }

    /// The thread that holds the engine. Every JNI call in this file
    /// happens on it: the drawing thread hands over a sentence and goes
    /// back to the frame, and the one slow moment — waiting for the speech
    /// service to bind — is nobody's frame.
    fn start() -> SyncSender<Job> {
        // A short queue: more than a few sentences behind is a stuck
        // engine, and the panel should hear about that rather than pile on.
        let (send, receive) = sync_channel(4);
        thread::spawn(move || {
            let raw = makepad_android_state::get_java_vm();
            if raw.is_null() {
                eprintln!("speech: the JVM is not up yet");
                return;
            }
            // SAFETY: the process's own JavaVM, as `platform::android`
            // reads it.
            let vm = match unsafe { JavaVM::from_raw(raw.cast()) } {
                Ok(vm) => vm,
                Err(e) => {
                    eprintln!("speech: {e}");
                    return;
                }
            };
            // A daemon attachment: this thread lives as long as the process
            // and is never joined, so nothing waits on it at the end.
            let mut env = match vm.attach_current_thread_as_daemon() {
                Ok(env) => env,
                Err(e) => {
                    eprintln!("speech: {e}");
                    return;
                }
            };
            let mut engine: Option<GlobalRef> = None;
            while let Ok(job) = receive.recv() {
                if let Err(e) = perform(&mut env, &mut engine, job) {
                    // The panel was told a moment ago that the voice took
                    // it, and has moved on; the log is where a phone's
                    // failure is read.
                    let _ = env.exception_clear();
                    eprintln!("speech: {e}");
                }
            }
            if let Some(tts) = engine.take() {
                let _ = env.call_method(&tts, "shutdown", "()V", &[]);
                let _ = env.exception_clear();
            }
        });
        send
    }

    /// One job, inside a local frame of its own: this thread never returns
    /// to Java, so the references it makes are freed here or not at all.
    fn perform(
        env: &mut JNIEnv<'_>,
        engine: &mut Option<GlobalRef>,
        job: Job,
    ) -> Result<(), String> {
        env.with_local_frame(16, |env| -> jni::errors::Result<()> {
            match job {
                Job::Stop => {
                    if let Some(tts) = engine.as_ref() {
                        env.call_method(tts, "stop", "()I", &[])?;
                    }
                }
                Job::Say { text, lang } => {
                    let first = engine.is_none();
                    if first {
                        *engine = Some(open(env)?);
                    }
                    let tts = engine.as_ref().expect("the engine is open");
                    let tag = env.new_string(&lang)?;
                    let locale = env
                        .call_static_method(
                            "java/util/Locale",
                            "forLanguageTag",
                            "(Ljava/lang/String;)Ljava/util/Locale;",
                            &[JValue::Object(&tag)],
                        )?
                        .l()?;
                    // Before the service is bound every call answers its
                    // own error value, so a language that takes is also the
                    // sign that the engine is up. Only the first sentence
                    // waits; after that a refusal is a refusal.
                    let mut set = language(env, tts, &locale)?;
                    let mut waited = 0;
                    while first && set < 0 && waited < READY_TRIES {
                        thread::sleep(READY_WAIT);
                        set = language(env, tts, &locale)?;
                        waited += 1;
                    }
                    let words = env.new_string(&text)?;
                    let none = JObject::null();
                    let id = env.new_string("superapp")?;
                    env.call_method(
                        tts,
                        "speak",
                        "(Ljava/lang/CharSequence;ILandroid/os/Bundle;Ljava/lang/String;)I",
                        &[
                            JValue::Object(&words),
                            JValue::Int(QUEUE_FLUSH),
                            JValue::Object(&none),
                            JValue::Object(&id),
                        ],
                    )?;
                }
            }
            Ok(())
        })
        .map_err(|e: jni::errors::Error| e.to_string())
    }

    /// A `TextToSpeech` over the *application* context, with no listener.
    ///
    /// The application context and not the activity, because a fold or a
    /// rotation recreates the activity and the process outlives it — the
    /// same reason `platform::android` installs that one. The listener is
    /// null because there is no way to make a Java class from here; what it
    /// would have told us is polled for instead.
    fn open<'local>(env: &mut JNIEnv<'local>) -> jni::errors::Result<GlobalRef> {
        let activity = makepad_android_state::get_activity();
        if activity.is_null() {
            return Err(jni::errors::Error::NullPtr("the activity is not up yet"));
        }
        // SAFETY: the activity Makepad holds a global reference to, borrowed
        // for the length of this frame and never freed here.
        let activity = unsafe { JObject::from_raw(activity.cast()) };
        let activity = env.new_local_ref(&activity)?;
        let context = env
            .call_method(
                &activity,
                "getApplicationContext",
                "()Landroid/content/Context;",
                &[],
            )?
            .l()?;
        let none = JObject::null();
        let tts = env.new_object(
            "android/speech/tts/TextToSpeech",
            "(Landroid/content/Context;Landroid/speech/tts/TextToSpeech$OnInitListener;)V",
            &[JValue::Object(&context), JValue::Object(&none)],
        )?;
        env.new_global_ref(tts)
    }

    /// `setLanguage`, as an int: below zero is *not now* — either the
    /// engine is not bound yet or this phone has no voice for the tag.
    fn language(
        env: &mut JNIEnv<'_>,
        tts: &GlobalRef,
        locale: &JObject<'_>,
    ) -> jni::errors::Result<i32> {
        env.call_method(
            tts,
            "setLanguage",
            "(Ljava/util/Locale;)I",
            &[JValue::Object(locale)],
        )?
        .i()
    }
}

// -- everywhere else -----------------------------------------------------------

#[cfg(not(any(target_os = "macos", target_os = "android")))]
mod elsewhere {
    /// No synthesizer here. Said plainly, so a panel can offer the words
    /// instead of reading them.
    #[derive(Default)]
    pub(super) struct Voice;

    impl Voice {
        pub(super) fn speak(&mut self, text: &str, lang: &str) -> Result<(), String> {
            let _ = (text, lang);
            Err("no speech on this platform".to_string())
        }

        pub(super) fn stop(&mut self) {}
    }
}
