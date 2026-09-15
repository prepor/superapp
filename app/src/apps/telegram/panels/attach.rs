//! What goes with the next message, and the ways to make more of it.
//!
//! The chat's bar carries `attach`, one link, and this panel behind it
//! carries the sending: the files the composer will send with the text —
//! added from what the files app holds, removed, put in the order they will
//! go — a voice note, a video message or a photograph made here, and the
//! way to the place to send. Gathered here so the chat's bar stays what a
//! chat is for.
//!
//! The list is the chat's own: the composer sends it and shows it on its
//! `CARRIES` line. This panel edits it through the join, as the line's card
//! replies through it, and opened away from its chat it says so. A capture
//! is the panel's own: the strip stands here, `enter` sends it and `esc`
//! throws it away, and closing the panel throws it away too — a recording
//! is the panel's, as the reply line is the chat's.
//!
//! The camera and the microphone are the kernel's [`Capture`] capability,
//! which is the platform's on a real run and the fake everywhere else. What
//! a capture leaves is a file under `captures/` beside the store: sent, it
//! stays there until the engine has uploaded it and the worker's sweep
//! takes it; discarded, it goes at once.

use std::any::Any;
use std::path::PathBuf;
use std::rc::Rc;

use kernel::caps::{CameraId, Capture, VideoNote, VoiceNote, CIRCLE_MAX};
use kernel::effect::World;
use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb, Want};
use kernel::session::Session;
use kernel::store::Store;

use crate::apps::files::Files;

use super::super::model::{self, Carried, PeerId, RecKind, Recording};
use super::super::{requests, runtime};
use super::{told, Chat, Place};

/// The files app's directory panel, where a file is held from. Named by
/// tag rather than by app: a build without it gets the shell's missing
/// card, which says whose panel it would have been.
const FILES_TAG: Tag = Tag("files");

/// What a stopped capture left behind, waiting for `send` or `discard`.
///
/// A video message stops itself at the minute, so a file can exist before
/// anybody has said what to do with it; a voice note is stopped by the send
/// itself and passes through here on its way out.
enum Taken {
    Voice(VoiceNote),
    Video(VideoNote),
}

impl Taken {
    /// What it wrote, so a discard can take it away again.
    fn files(&self) -> Vec<PathBuf> {
        match self {
            Taken::Voice(note) => vec![note.path.clone()],
            Taken::Video(note) => vec![note.path.clone(), note.thumbnail.clone()],
        }
    }

    /// The request that sends it into a chat — into one of its topics,
    /// where the chat is a forum — and what a build off the wire says
    /// would have left.
    fn request(&self, chat: PeerId, topic: i64) -> (String, String) {
        let (request, said) = match self {
            Taken::Voice(note) => (
                requests::send_voice_note(chat, None, note),
                said(RecKind::Voice, note.secs),
            ),
            Taken::Video(note) => (
                requests::send_video_note(chat, None, note),
                said(RecKind::Video, note.secs),
            ),
        };
        (requests::in_topic(request, topic), said)
    }
}

/// How long a video message waits for the camera before it gives up.
///
/// The camera is a wish: a device that has one answers within a frame or
/// two, and one that has not may never answer at all — a strip counting the
/// seconds of a recording that never started is the one thing worse than a
/// refusal.
const CAMERA_PATIENCE: f64 = 5.0;

/// *voice 0:02*, *video message 1:00* — what a capture is called with its
/// length, for the toast a build off the wire answers with.
fn said(kind: RecKind, secs: f64) -> String {
    format!("{} {}", kind.word(), model::fmt_secs(secs.floor() as i64))
}

/// The camera and the microphone of one world, as a verb reaches them.
///
/// Spelled out rather than elided because the capability bag holds them for
/// as long as the world lives: `dyn Capture` under a borrow of its own
/// lifetime is a different type, and the bag will not hand one out.
type Senses<'a> = &'a mut (dyn Capture + 'static);

/// The attach panel.
pub struct Attach {
    id: PanelId,
    chat: PeerId,
    store: Rc<Store>,
    /// The world the capture capability is reached through — the camera and
    /// the microphone of this run, one for every panel that asks.
    world: Rc<World>,
    slot: SlotId,
    /// The chat's list as it stood when the widget last looked: the bar is
    /// built without the session.
    items: Vec<Carried>,
    /// Whether a chat stands behind the join.
    joined: bool,
    /// The row the arrows walk, as an index into the list.
    cursor: Option<usize>,
    /// What the files app was holding when the widget last looked; *add*
    /// comes and goes with it.
    held: Vec<String>,
    /// Where this panel's captures are written.
    captures: PathBuf,
    /// A recording under way, or one the minute stopped.
    recording: Option<Recording>,
    /// What a stopped recording answered, until it is sent or discarded.
    taken: Option<Taken>,
    /// A video message that asked for the camera and is waiting for it: on
    /// a real device the picture arrives a moment after it is wished for.
    waiting: bool,
    /// The camera is up for photographs.
    shooting: bool,
}

impl Attach {
    pub const TAG: Tag = Tag("attach");

    /// The identity of the attach panel of one chat.
    #[must_use]
    pub fn id(chat: PeerId) -> PanelId {
        PanelId::new(Self::TAG, [chat.to_string()])
    }

    /// The same, in one of a forum's topics: the topic is the second
    /// argument, and nought is a chat that is not a forum.
    #[must_use]
    pub fn in_topic(chat: PeerId, topic: i64) -> PanelId {
        if topic == 0 { return Self::id(chat); }
        PanelId::new(Self::TAG, [chat.to_string(), topic.to_string()])
    }

    /// The chat an `attach` panel is for; `None` for any other tag.
    #[must_use]
    pub fn of(id: &PanelId) -> Option<PeerId> {
        (id.tag == Self::TAG)
            .then(|| id.arg(0)?.parse().ok())
            .flatten()
    }

    /// Which of a forum's topics this panel attaches to; nought for a chat
    /// that is not one.
    #[must_use]
    fn topic(&self) -> i64 {
        self.id.arg(1).and_then(|s| s.parse().ok()).unwrap_or(0)
    }

    /// The chat's title.
    #[must_use]
    pub fn chat_title(&self) -> String {
        super::super::topics::card(&self.store, self.chat, self.topic())
            .map_or_else(|| "chat".to_string(), |c| c.name)
    }

    /// What the composer will send with the text, in the order it will go.
    #[must_use]
    pub fn items(&self) -> &[Carried] {
        &self.items
    }

    /// Whether a chat stands behind the join, at the last look.
    #[must_use]
    pub fn joined(&self) -> bool {
        self.joined
    }

    /// Which row the arrows are on, where the list has one.
    #[must_use]
    pub fn cursor(&self) -> Option<usize> {
        self.cursor
    }

    /// Puts it on a row a press landed on. A row that is not there is not
    /// an error: the list may have shortened since the draw.
    pub fn set_cursor(&mut self, i: usize) {
        if i < self.items.len() {
            self.cursor = Some(i);
        }
    }

    /// Steps the cursor over the list; from nothing, either way lands on the
    /// first row. Answers where it landed.
    pub fn walk(&mut self, d: isize) -> Option<usize> {
        if self.items.is_empty() {
            self.cursor = None;
            return None;
        }
        let last = self.items.len() as isize - 1;
        let at = match self.cursor {
            Some(i) => (i as isize + d).clamp(0, last) as usize,
            None => 0,
        };
        self.cursor = Some(at);
        self.cursor
    }

    /// What the files app holds, said directly: a test's way round the
    /// clipboard, which every test in the binary shares.
    #[cfg(test)]
    pub fn set_held(&mut self, paths: Vec<String>) {
        self.held = paths;
    }

    // -- the camera and the microphone ------------------------------------------------

    /// Asks the capture capability for something that may refuse. A world
    /// with no senses at all refuses in the same shape, so a panel says one
    /// thing however it was denied.
    fn ask<R>(&self, f: impl FnOnce(Senses<'_>) -> Result<R, String>) -> Result<R, String> {
        self.world
            .with_cap::<dyn Capture, _>(f)
            .unwrap_or_else(|_| Err("this build has no camera or microphone".to_string()))
    }

    /// Tells it something that cannot fail: let the camera go, throw a
    /// recording away.
    fn tell(&self, f: impl FnOnce(Senses<'_>)) {
        let _ = self.world.with_cap::<dyn Capture, _>(f);
    }

    /// What the meter draws, 0 to 1. Zero once the capability has stopped,
    /// which is what makes the minute visible.
    #[must_use]
    pub fn level(&self) -> f32 {
        self.world
            .with_cap::<dyn Capture, _>(|c: Senses<'_>| c.level())
            .unwrap_or(0.0)
    }

    /// Which camera the preview is pointed at, once there is one. `None`
    /// under a script and in every library mount: there is no device behind
    /// a fake capture, and the box draws empty.
    #[must_use]
    pub fn camera(&self) -> Option<CameraId> {
        self.world
            .with_cap::<dyn Capture, _>(|c: Senses<'_>| c.camera())
            .ok()
            .flatten()
    }

    /// Whether the camera is open, which is what the line says and what a
    /// video message waits for — a device that has not yet named the camera
    /// it opened, and a fake that never names one, are both open.
    #[must_use]
    pub fn camera_open(&self) -> bool {
        self.world
            .with_cap::<dyn Capture, _>(|c: Senses<'_>| c.camera_open())
            .unwrap_or(false)
    }

    /// A recording under way, or one the minute stopped.
    #[must_use]
    pub fn recording(&self) -> Option<Recording> {
        self.recording
    }

    /// Whether the camera is up for photographs.
    #[must_use]
    pub fn shooting(&self) -> bool {
        self.shooting
    }

    /// Whether the picture belongs over the strip: a video message is made
    /// of what the camera sees, a voice note of what the room says.
    #[must_use]
    pub fn previewing(&self) -> bool {
        self.shooting || self.recording.is_some_and(|r| r.kind == RecKind::Video)
    }

    /// Starts a voice note, or asks for the camera a video message will be
    /// made from. Answers the capability's own words where it refuses.
    ///
    /// # Errors
    ///
    /// If there is no microphone or no camera, the permission was refused,
    /// or something is already being recorded.
    pub fn start_recording(&mut self, kind: RecKind, now: f64) -> Result<(), String> {
        let dir = self.captures.clone();
        match kind {
            RecKind::Voice => {
                self.ask(|c| c.start_voice(&dir))?;
                self.recording = Some(Recording::started(kind, now));
            }
            RecKind::Video => {
                // The camera is a wish: which one it turned out to be is
                // known a moment later, and the circle starts then.
                self.ask(|c| c.open_camera())?;
                self.recording = Some(Recording::started(kind, now));
                self.waiting = true;
                self.roll(now)?;
            }
        }
        Ok(())
    }

    /// Starts the circle once the camera has come. Does nothing while
    /// nothing is waiting for it, which is every other call.
    ///
    /// # Errors
    ///
    /// If the capability refuses to record.
    fn roll(&mut self, now: f64) -> Result<(), String> {
        if !self.waiting || !self.camera_open() {
            return Ok(());
        }
        self.waiting = false;
        let dir = self.captures.clone();
        self.ask(|c| c.start_circle(&dir))?;
        // The clock starts where the recording does, not where the wish was.
        self.recording = Some(Recording::started(RecKind::Video, now));
        Ok(())
    }

    /// The minute: the capability wrote no more, so the panel takes the
    /// file it kept and the strip stands with its two verbs, the meter
    /// still.
    fn hold(&mut self, now: f64) {
        match self.ask(|c| c.stop_circle()) {
            Ok(note) => {
                self.taken = Some(Taken::Video(note));
                if let Some(r) = &mut self.recording {
                    // The minute is where it stopped, however much later
                    // the panel got round to looking.
                    r.stopped = Some(now.min(r.since + CIRCLE_MAX));
                }
            }
            Err(why) => {
                self.trouble(&why);
                self.clear();
            }
        }
    }

    /// Throws a capture away, file and all: a capture nobody asked to keep
    /// is not left on the disk.
    pub fn cancel_recording(&mut self) {
        let Some(r) = self.recording.take() else { return };
        self.waiting = false;
        match self.taken.take() {
            // The minute's file is written already; take it away.
            Some(taken) => remove(&taken.files()),
            // What is still being written is the capability's to throw.
            None => self.tell(|c| c.discard()),
        }
        if r.kind == RecKind::Video {
            self.tell(|c| c.close_camera());
        }
    }

    /// A capture that never got going, or one the capability lost hold of:
    /// the strip goes, the file it had already written goes with it, and
    /// the camera is let go unless the panel is photographing.
    ///
    /// Nothing is *discarded* here, on purpose: a refusal is most often
    /// *something is already being recorded*, and what another panel is
    /// recording is not this one's to throw away.
    fn clear(&mut self) {
        self.recording = None;
        self.waiting = false;
        if let Some(taken) = self.taken.take() {
            remove(&taken.files());
        }
        if !self.shooting {
            self.tell(|c| c.close_camera());
        }
    }

    /// Something the panel noticed with no session to say it through — the
    /// camera refusing between two draws. The app's tick turns it into the
    /// same toast a verb would have made.
    fn trouble(&self, why: &str) {
        runtime::of(&self.store).notice(why.to_string(), true);
    }

    /// *send* on a capture: the recording is stopped where it has not
    /// stopped itself, and what it made goes as its own message — a voice
    /// note with its waveform, a video message with its length and its
    /// poster. Over the wire where the account is live; off it the toast
    /// says what would have left.
    ///
    /// The file stays where it is: the engine reads it as it uploads, and
    /// the worker's sweep takes it a day later.
    pub fn send_recording(&mut self, s: &mut Session) {
        let Some(r) = self.recording.take() else { return };
        self.waiting = false;
        let taken = match self.taken.take() {
            Some(taken) => Ok(taken),
            None => match r.kind {
                RecKind::Voice => self.ask(|c| c.stop_voice()).map(Taken::Voice),
                RecKind::Video => self.ask(|c| c.stop_circle()).map(Taken::Video),
            },
        };
        if r.kind == RecKind::Video {
            self.tell(|c| c.close_camera());
        }
        match taken {
            Ok(taken) => {
                let (request, what) = taken.request(self.chat, self.topic());
                told(s, &request, &what);
            }
            // Shorter than half a second, no microphone, the permission
            // refused: said in the panel's own words, and the strip goes.
            Err(why) => s.notify(why, true),
        }
        s.redraw();
    }

    /// A shot: the newest frame as a JPEG under `captures/`, onto the
    /// chat's list through the join — the phone's strip is this list — and
    /// the camera stays up for the next one.
    fn shoot(&mut self, s: &mut Session) {
        let dir = self.captures.clone();
        let photo = match self.ask(|c| c.take_photo(&dir)) {
            Ok(photo) => photo,
            Err(why) => {
                s.notify(why, true);
                return;
            }
        };
        let path = photo.path.to_string_lossy().into_owned();
        let Some(len) = self.with_chat(s, |c| {
            c.carry(std::slice::from_ref(&path));
            c.carrying().len()
        }) else {
            let _ = std::fs::remove_file(&photo.path);
            Self::orphan(s);
            return;
        };
        s.notify(
            format!("carrying {} shot{}", len, if len == 1 { "" } else { "s" }),
            false,
        );
        self.cursor = Some(len - 1);
        self.observe(s);
        s.redraw();
    }

    /// Looks at what this panel cannot ask for while it builds its bar: the
    /// chat's list through the join, and what the files app holds. Called
    /// by the widget at the top of every draw and event, and after every
    /// verb that changed the list, so the bar never speaks of a row that is
    /// gone. A build without the files app holds nothing.
    ///
    /// It is also where a capture is watched: the circle starts once the
    /// camera has come, and stops itself at the minute the clients cap a
    /// video message at.
    pub fn observe(&mut self, s: &Session) {
        let now = s.now();
        if let Err(why) = self.roll(now) {
            self.trouble(&why);
            self.clear();
        }
        if self.waiting && self.recording.is_some_and(|r| r.elapsed(now) > CAMERA_PATIENCE) {
            self.trouble("the camera never came");
            self.clear();
        }
        if self
            .recording
            .is_some_and(|r| r.kind == RecKind::Video && r.running() && r.elapsed(now) >= CIRCLE_MAX)
        {
            self.hold(now);
        }
        self.held = s
            .apps()
            .get_as::<Files>()
            .map(|f| f.clipboard().paths)
            .unwrap_or_default();
        let items = self.with_chat(s, |c| c.carrying().to_vec());
        self.joined = items.is_some();
        self.items = items.unwrap_or_default();
        self.cursor = match self.cursor {
            Some(_) if self.items.is_empty() => None,
            Some(i) => Some(i.min(self.items.len() - 1)),
            None => None,
        };
    }

    /// Runs `f` on the chat this panel hangs under, if it is one. The
    /// borrow lasts exactly as long as the call.
    fn with_chat<R>(&self, s: &Session, f: impl FnOnce(&mut Chat) -> R) -> Option<R> {
        let parent = s.join_parent_of(self.slot)?;
        let inst = s.panel(parent)?;
        let mut b = inst.borrow_mut();
        let c = b.as_any().downcast_mut::<Chat>()?;
        Some(f(c))
    }

    /// Paths onto the chat's list, wherever they came from — the files
    /// app's clipboard through *add*, or a picker's answer. The list is the
    /// chat's, so both go the same way.
    fn carry_paths(&mut self, paths: &[String], s: &mut Session) {
        let Some((added, len)) = self.with_chat(s, |c| {
            let added = c.carry(paths);
            (added, c.carrying().len())
        }) else {
            Self::orphan(s);
            return;
        };
        s.notify(
            if added == 0 {
                "already carrying it".to_string()
            } else {
                format!("carrying {added} file{}", if added == 1 { "" } else { "s" })
            },
            false,
        );
        if added > 0 {
            self.cursor = Some(len - 1);
        }
        self.observe(s);
        s.redraw();
    }

    /// What the panel says when no chat stands behind the join.
    fn orphan(s: &mut Session) {
        s.notify("open this from its chat to attach to it", true);
    }
}

/// Takes a capture's files off the disk. A file that is not there is not a
/// failure: a recorder that never got going wrote none.
fn remove(files: &[PathBuf]) {
    for path in files {
        let _ = std::fs::remove_file(path);
    }
}

/// Closing the panel discards: a capture is the panel's own, and the camera
/// goes off with it. The window may be closed with a recording running, and
/// a device left listening to an empty room is the one thing this must not
/// leave behind.
impl Drop for Attach {
    fn drop(&mut self) {
        self.cancel_recording();
        if self.shooting {
            self.tell(|c| c.close_camera());
        }
    }
}

impl Panel for Attach {
    fn id(&self) -> &PanelId {
        &self.id
    }

    /// *attach · Vera Kovac*, and what it is doing while it is doing it.
    fn title(&self) -> String {
        let doing = if self.recording.is_some() {
            " · recording"
        } else if self.shooting {
            " · camera"
        } else {
            ""
        };
        format!("attach · {}{doing}", self.chat_title())
    }

    /// A card with a list in it.
    fn wish(&self, _cols: usize) -> (u32, u32) {
        (4, 4)
    }

    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }

    /// While a capture runs, its two ways out and nothing else: *send* and
    /// *discard* for a recording, *shoot* and *done* for the camera. At
    /// rest: *browse*, a link to the files panel, always; *add* beside it
    /// while the files app holds something — the link keeps its place and
    /// the button comes and goes; the verbs on the cursor's row — *remove*,
    /// and *earlier* and *later* where there is a row to trade with,
    /// spelled by the order they will go rather than by the screen; the
    /// three ways to make one; and the place, a link.
    ///
    /// *add* wears `d` because *later* is `a`; *voice* wears `o` because
    /// `v` is the video message's.
    fn verbs(&self) -> Vec<Verb> {
        if self.recording.is_some() {
            return vec![
                Verb::run("telegram.send_rec", "send", Some('s')),
                Verb::run("telegram.discard", "discard", Some('d')),
            ];
        }
        if self.shooting {
            return vec![
                Verb::run("telegram.shoot", "shoot", Some('s')),
                Verb::run("telegram.done", "done", Some('n')),
            ];
        }
        let mut v = vec![Verb::go(
            "telegram.browse",
            "browse",
            Some('b'),
            Nav::Open {
                from: self.slot,
                id: PanelId::new(FILES_TAG, ["~", "pick"]),
                fresh: false,
            },
        )];
        if !self.held.is_empty() {
            let k = self.held.len();
            v.push(Verb::run(
                "telegram.add",
                if k == 1 { "add".to_string() } else { format!("add {k}") },
                Some('d'),
            ));
        }
        if let Some(i) = self.cursor.filter(|i| *i < self.items.len()) {
            v.push(Verb::run("telegram.remove", "remove", Some('r')));
            if i > 0 {
                v.push(Verb::run("telegram.earlier", "earlier", Some('e')));
            }
            if i + 1 < self.items.len() {
                v.push(Verb::run("telegram.later", "later", Some('a')));
            }
        }
        v.push(Verb::run("telegram.voice", "voice", Some('o')));
        v.push(Verb::run("telegram.video", "video", Some('v')));
        v.push(Verb::run("telegram.camera", "camera", Some('c')));
        v.push(Verb::go(
            "telegram.place",
            "place",
            Some('p'),
            Nav::Open {
                from: self.slot,
                id: Place::in_topic(self.chat, self.topic()),
                fresh: false,
            },
        ));
        v
    }

    /// What *browse* is for. The picker carries this panel's errand, and
    /// what it chooses is carried exactly as *add* carries the clipboard.
    fn wants(&self) -> Option<Want> {
        (self.recording.is_none())
            .then(|| Want::files("attach", Some('h'), "Choose what this message will carry."))
    }

    fn took(&mut self, paths: Vec<String>, s: &mut Session) {
        self.carry_paths(&paths, s);
    }

    fn run(&mut self, verb: &str, s: &mut Session) {
        let now = s.now();
        match verb {
            // What the files app holds goes to the chat's list, by path:
            // the send reads it as the message leaves, as a letter does.
            // The clipboard is not consumed — a copy is a copy.
            "telegram.add" => {
                let held = self.held.clone();
                self.carry_paths(&held, s);
            }
            "telegram.remove" => {
                let Some(i) = self.cursor else { return };
                if self.with_chat(s, |c| c.uncarry(i)).is_none() {
                    Self::orphan(s);
                    return;
                }
                self.observe(s);
                s.redraw();
            }
            "telegram.earlier" | "telegram.later" => {
                let Some(i) = self.cursor else { return };
                let d: isize = if verb == "telegram.earlier" { -1 } else { 1 };
                match self.with_chat(s, |c| c.move_carried(i, d)) {
                    Some(Some(j)) => self.cursor = Some(j),
                    Some(None) => {}
                    None => {
                        Self::orphan(s);
                        return;
                    }
                }
                self.observe(s);
                s.redraw();
            }
            "telegram.voice" | "telegram.video" => {
                let kind = if verb == "telegram.voice" { RecKind::Voice } else { RecKind::Video };
                if let Err(why) = self.start_recording(kind, now) {
                    self.clear();
                    s.notify(why, true);
                }
                s.redraw();
            }
            "telegram.camera" => {
                match self.ask(|c| c.open_camera()) {
                    Ok(()) => self.shooting = true,
                    Err(why) => s.notify(why, true),
                }
                s.redraw();
            }
            "telegram.shoot" => self.shoot(s),
            "telegram.done" => {
                self.shooting = false;
                self.tell(|c| c.close_camera());
                s.redraw();
            }
            "telegram.send_rec" => self.send_recording(s),
            "telegram.discard" => {
                self.cancel_recording();
                s.redraw();
            }
            _ => {}
        }
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

/// Its factory.
pub struct AttachKind;

impl PanelKind for AttachKind {
    fn tag(&self) -> Tag {
        Attach::TAG
    }

    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        let store = cx.session().store().clone();
        Box::new(Attach {
            chat: Attach::of(id).unwrap_or_default(),
            id: id.clone(),
            captures: model::captures_dir(store.dir()),
            store,
            world: cx.session().world().clone(),
            slot: 0,
            items: Vec::new(),
            joined: false,
            cursor: None,
            held: Vec::new(),
            recording: None,
            taken: None,
            waiting: false,
            shooting: false,
        })
    }
}
