//! One message's media. Text, images and PDFs share the file viewer;
//! recordings retain their playback controls. The panel follows the content's
//! dimensions, and previous/next walk the chat's media in place.
//!
//! A picture is usually here the moment the line is: the worker fetches a
//! photo, and a moving picture's poster, on arrival — though only for the
//! newest forty lines of a chat as it opens, and the cache evicts what it
//! must, so one may well be missing. Then it is asked for by the durable
//! remote id the row keeps, the same way the clip is. The clip behind a
//! poster is never fetched on arrival — a chat full of video would
//! otherwise pull every one of them — so
//! opening the viewer on a video is what asks for it, and the panel says
//! *downloading · 12 MB / 48 MB* until the file lands in the blob cache under
//! the key the row already names. Then the platform's own player draws and plays it, and
//! the one *play* button drives that instead of the timeline. A voice note
//! and a track still run the fake timeline against the clock; playing a
//! sound is not implemented.

use std::any::Any;
use std::path::PathBuf;
use std::rc::Rc;

use kernel::effect::World;
use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::session::Session;

use crate::shell::widgets::media::PlayerState;
use crate::shell::widgets::viewer::{Controller, Measure, Preview};
use kernel::caps::FileKind;
use super::super::{operations::Status, requests, runtime};

use super::super::draft_toast;
use super::super::model::{self, Msg, MsgId, PeerId};
use super::super::downloads;
use super::playback::Playback;

/// The viewer.
pub struct Viewer {
    id: PanelId,
    chat: PeerId,
    msg: MsgId,
    world: Rc<World>,
    slot: SlotId,
    playback: Playback,
    viewer: Controller,
    file_request: Option<String>,
}

impl Viewer {
    pub const TAG: Tag = Tag("media");

    /// The identity of the viewer over one line's media.
    #[must_use]
    pub fn id(chat: PeerId, msg: MsgId) -> PanelId {
        PanelId::new(Self::TAG, [chat.to_string(), msg.to_string()])
    }

    /// The chat and the line a `media` panel names; `None` for any other
    /// tag.
    #[must_use]
    pub fn of(id: &PanelId) -> Option<(PeerId, MsgId)> {
        if id.tag != Self::TAG {
            return None;
        }
        Some((id.arg(0)?.parse().ok()?, id.arg(1)?.parse().ok()?))
    }

    /// The line, off the store.
    #[must_use]
    pub fn msg(&self) -> Option<Msg> {
        model::line(self.world.store(), self.chat, self.msg)
    }

    /// The line's neighbours among the chat's media: the one before and
    /// the one after, where there are any.
    #[must_use]
    pub fn neighbours(&self) -> (Option<MsgId>, Option<MsgId>) {
        let ids = model::media_ids(self.world.store(), self.chat, self.msg);
        let Some(i) = ids.iter().position(|id| *id == self.msg) else {
            return (None, None);
        };
        (
            i.checked_sub(1).and_then(|j| ids.get(j).copied()),
            ids.get(i + 1).copied(),
        )
    }

    /// The source adapter supplies the same input as a disk file or mail part.
    /// Only cache lookup happens here; the viewer worker reads and decodes it.
    pub fn file_preview(&mut self, m: &Msg) -> (String, Preview) {
        let Some(md) = &m.media else { return ("gone".into(), Preview::None) };
        let name = downloads::name(m);
        let kind = if md.kind == "photo" { FileKind::Image } else { FileKind::of_name(&name) };
        let reference = md.reference.as_deref().unwrap_or("");
        let key = format!("{reference}:{name}");
        if !matches!(kind, FileKind::Pdf | FileKind::Text | FileKind::Image) {
            return (key, Preview::None);
        }
        if let Some(bytes) = super::super::seed::demo_bytes(reference) {
            return (key, kernel::caps::preview_of(kind, &name, bytes.len() as u64,
                |max| Some(bytes.iter().take(max).copied().collect())).into());
        }
        if reference.starts_with("tg:") {
            let reading = super::super::media_cache::read_blob(&self.world, reference);
            if let Some(path) = reading.clone().paths().and_then(|paths| paths.file) {
                return (format!("{key}:ready"), Preview::Path { path, name, kind, size: 0 });
            }
            if matches!(reading, super::super::media_cache::Reading::Pending(_)) {
                return (format!("{key}:preparing"), Preview::Loading("preparing preview…".into()));
            }
            let rt = runtime::of(self.world.store());
            let context = format!("cache:{}:{}", m.chat, m.id);
            if self.file_request.as_ref().is_none_or(|source| source != &key) {
                if !rt.can_send() {
                    let error = "Telegram is not connected; reconnect to load this file";
                    return (format!("{key}:{error}"), Preview::Error(error.into()));
                }
                let existing = rt.operations.list().into_iter()
                    .any(|o| o.context() == Some(&context) && o.status == Status::Pending);
                if !existing {
                    let request = rt.operations.track(&requests::cache_file(m.chat, m.id));
                    let value: serde_json::Value = serde_json::from_str(&request).expect("tracked request");
                    let id = value["@extra"]["operation"].as_u64().expect("operation id");
                    if !rt.send(&request) {
                        rt.operations.fail(self.world.store(), id, "Telegram disconnected; try again", false);
                    }
                }
                self.file_request = Some(key.clone());
            }
            // A retry from the shared feedback strip owns the latest status.
            let latest = rt.operations.list().into_iter().rev().find(|o| o.context() == Some(&context));
            let (note, failed) = match latest.map(|o| o.status) {
                Some(Status::Failed { error, .. }) => (error, true),
                Some(Status::Done) => ("Downloaded file is no longer cached; reopen it to try again".into(), true),
                _ => (rt.connection_note().or_else(|| rt.download(reference).map(|d| d.note()))
                    .unwrap_or_else(|| "downloading preview…".into()), false),
            };
            return (format!("{key}:{note}"), if failed { Preview::Error(note) } else { Preview::Loading(note) });
        }
        (key, Preview::Error("This attachment is not available on this device".into()))
    }

    pub fn viewer(&self) -> Controller { self.viewer.clone() }

    pub fn player_state(&self, m: &Msg, now: f64) -> Option<PlayerState> {
        self.playback.player_state(m, now)
    }

    pub fn clip_file(&self, m: &Msg) -> Option<PathBuf> {
        self.playback.clip_file(m)
    }

    pub fn file_to_open(&self, m: &Msg) -> Option<PathBuf> {
        self.playback.file_to_open(m)
    }

    pub fn plays_clip(&self, m: &Msg) -> bool {
        self.playback.plays_clip(m)
    }

    pub fn download_note(&self, m: &Msg) -> Option<String> {
        self.playback.download_note(m)
    }

    pub fn ask_for_clip(&mut self, m: &Msg) {
        self.playback.ask_for_clip(m)
    }

    pub fn ask_for_picture(&mut self, m: &Msg) {
        self.playback.ask_for_picture(m)
    }

    pub fn awaiting_picture(&self, m: &Msg) -> bool {
        self.playback.awaiting_picture(m)
    }

    pub fn toggle_play(&mut self, m: &Msg, now: f64) {
        self.playback.toggle_play(m, now)
    }

    pub fn seek(&mut self, m: &Msg, position: f64, now: f64) {
        self.playback.seek(m, position, now)
    }

    pub fn running(&self) -> bool {
        self.playback.running()
    }

    pub fn set_running(&mut self, running: bool) {
        self.playback.set_running(running)
    }

    pub fn playing(&self, now: f64) -> bool {
        self.playback.playing(now)
    }
}

impl Panel for Viewer {
    fn id(&self) -> &PanelId {
        &self.id
    }

    /// *photo · Vera Kovac · 16:40*.
    fn title(&self) -> String {
        match self.msg() {
            Some(m) => format!(
                "{} · {} · {}",
                m.media.as_ref().map_or("media", |md| md.word()),
                m.writer(),
                model::fmt_hour(m.date)
            ),
            None => "media".to_string(),
        }
    }

    fn about(&self) -> String {
        format!("A Telegram media attachment. Its arguments are chat id {} and message id {}. \
            Use telegram.file with chat = {} and message = {} to read the full attachment; \
            the displayed preview and caption are not its file contents.",
            self.chat, self.msg, self.chat, self.msg)
    }

    /// Loaded content supplies its own size; media dimensions seed the wish.
    fn wish(&self, cols: usize) -> (u32, u32) {
        let measure = self.viewer.measure();
        if measure != Measure::Empty { return measure.wish(cols, 3); }
        if self.msg().is_some_and(|m| FileKind::of_name(&downloads::name(&m)) == FileKind::Pdf) {
            return Measure::Pdf(595, 842).wish(cols, 3);
        }
        let size = self.msg().and_then(|m| m.media).and_then(|md| Some((md.w?, md.h?)));
        size.filter(|(w, h)| *w > 0 && *h > 0)
            .map_or(Measure::Empty, |(w, h)| Measure::Image(w as u32, h as u32)).wish(cols, 3)
    }

    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }

    /// The walk through the chat's media in place, the player's one button,
    /// and the system's own opener.
    fn verbs(&self) -> Vec<Verb> {
        let (prev, next) = self.neighbours();
        let m = self.msg();
        let mut v = Vec::new();
        if let Some(p) = prev {
            v.push(Verb::go(
                "telegram.previous",
                "previous",
                Some('p'),
                Nav::Replace {
                    slot: self.slot,
                    id: Viewer::id(self.chat, p),
                },
            ));
        }
        if let Some(n) = next {
            v.push(Verb::go(
                "telegram.next",
                "next",
                Some('n'),
                Nav::Replace {
                    slot: self.slot,
                    id: Viewer::id(self.chat, n),
                },
            ));
        }
        // The one button is wherever there is something to play: a
        // recording, or a clip this build can really play.
        if let Some(st) = m.as_ref().and_then(|m| self.player_state(m, self.world.now())) {
            v.push(Verb::run(
                "telegram.play",
                if st.playing { "pause" } else { "play" },
                Some('y'),
            ));
        }
        v.push(Verb::run("telegram.open", "open", Some('o')));
        v.extend(m.as_ref().and_then(downloads::verb));
        v.extend(self.viewer.verbs());
        v
    }

    fn run(&mut self, verb: &str, s: &mut Session) {
        if self.viewer.run(verb) { s.redraw(); return; }
        let now = s.now();
        match verb {
            "telegram.download" => {
                if let Some(m) = self.msg() {
                    downloads::request(s, &m);
                }
            }
            "telegram.play" => {
                if let Some(m) = self.msg() {
                    self.toggle_play(&m, now);
                    s.redraw();
                }
            }
            // The system's own player, which is the sure way to see a clip:
            // what the in-app player cannot show is still a file on disk.
            "telegram.open" => {
                let file = self.msg().and_then(|m| self.file_to_open(&m));
                match file {
                    Some(path) if cfg!(target_os = "macos") => {
                        drop(super::super::operations::run_local(
                            self.world.store(),
                            Some(self.chat),
                            "opening media",
                            move || {
                                let output = std::process::Command::new("open")
                                    .arg(&path)
                                    .output()
                                    .map_err(|e| e.to_string())?;
                                if output.status.success() {
                                    Ok(())
                                } else {
                                    Err(format!(
                                        "{}: {}",
                                        output.status,
                                        String::from_utf8_lossy(&output.stderr).trim()
                                    ))
                                }
                            },
                        ));
                        s.redraw();
                    }
                    Some(_) => s.notify(draft_toast("open with the system"), false),
                    None => s.notify("nothing here to open yet".to_string(), false),
                }
            }
            _ => {}
        }
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

/// Its factory.
pub struct ViewerKind;

impl PanelKind for ViewerKind {
    fn tag(&self) -> Tag {
        Viewer::TAG
    }

    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        let (chat, msg) = Viewer::of(id).unwrap_or_default();
        Box::new(Viewer {
            id: id.clone(),
            chat,
            msg,
            world: cx.session().world().clone(),
            slot: 0,
            viewer: Controller::default(),
            file_request: None,
            playback: Playback::new(cx.session().store().clone(), (chat, msg)),
        })
    }
}
