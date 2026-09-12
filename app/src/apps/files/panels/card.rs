//! The `file` panel: one file as a card.

use std::any::Any;
use std::rc::Rc;

use kernel::effect::World;
use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::session::Session;
use kernel::time::fmt_date;

use super::super::model::{
    basename, parent, preview_of, read_in, stat_in, Entry, FileKind, Preview, Watch,
};
use super::super::ops;
use super::super::run;
use super::super::{Op, Seen, FILES};
use super::dir;
use crate::shell::widgets::viewer::{Controller, Measure, Preview as ViewPreview};

/// One file, shown.
///
/// File metadata is refreshed on opening and when the disk changes. The
/// shared viewer reads and renders the contents on a worker; demo worlds
/// retain an inline reading for deterministic tests.
pub struct Card {
    /// Optional editor supplied through the app registry.
    editor: Option<PanelId>,
    id: PanelId,
    path: String,
    /// The directory the file is in, which is what a watcher can be asked
    /// about: a file is told about through the directory that holds it.
    dir: String,
    slot: SlotId,
    world: Rc<World>,
    /// What the disk had when it was last asked; `None` once the path
    /// names nothing.
    entry: Option<Entry>,
    /// The inline reading for demo worlds. A native reader uses `viewer_disk`.
    preview: Preview,
    viewer: Controller,
    viewer_disk: Option<kernel::caps::DiskFactory>,
    /// The `rename` field, while it is open: the new name as typed.
    renaming: Option<String>,
    /// The line under the header: what a verb refused, until the next one.
    status: Option<String>,
    /// What this reading was taken at, on both counts.
    seen: Seen,
    statting: Option<tokio::sync::oneshot::Receiver<Result<Option<Entry>, String>>>,
    opening: Option<tokio::sync::oneshot::Receiver<Result<(), String>>>,
    /// Which reading is on the card: bumped when the card actually reads
    /// again, not every time somebody writes a disk. The widget decodes a
    /// picture once per reading, and a run copying elsewhere must not have
    /// it decode the same one once a frame for the length of the run.
    read: u64,
    /// The run the line under the header was about, as of the last time
    /// this card was **drawn**, and the line itself — as a listing's, and
    /// read together for the same reason.
    drew: u64,
    doing: Option<String>,
    /// The file's directory, watched for as long as this card shows it.
    /// Held, not read.
    _watch: Watch,
}

impl Card {
    /// The persisted spelling. One argument: the path, in the display
    /// spelling (`~/Downloads/README.txt`).
    pub const TAG: Tag = Tag("file");

    /// The card for a path.
    #[must_use]
    pub fn id(path: &str) -> PanelId {
        PanelId::new(Self::TAG, [path])
    }

    /// The path a `file` panel shows; `None` for any other tag.
    #[must_use]
    pub fn of(id: &PanelId) -> Option<&str> {
        (id.tag == Self::TAG).then(|| id.arg(0)).flatten()
    }

    // -- what the card draws ---------------------------------------------------

    /// The selectable line: where the file is.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The big line: the file's name.
    #[must_use]
    pub fn name(&self) -> String {
        basename(&self.path).to_string()
    }

    #[must_use]
    pub fn kind(&self) -> FileKind {
        self.entry.as_ref().map_or(FileKind::Other, Entry::kind)
    }

    /// The card's word for what it is: *pdf*, *text*, *directory*.
    #[must_use]
    pub fn kind_word(&self) -> &'static str {
        self.kind().word()
    }

    #[must_use]
    pub fn size(&self) -> u64 {
        self.entry.as_ref().map_or(0, |e| e.size)
    }

    /// The line beside the name: what it is and how big — `pdf · 96 KB`.
    /// The shell's card draws its own; this is what a test reads.
    #[cfg(test)]
    #[must_use]
    pub fn kind_line(&self) -> String {
        format!(
            "{} · {}",
            self.kind_word(),
            super::super::model::fmt_size(self.size())
        )
    }

    /// The muted line: when the file last changed.
    #[must_use]
    pub fn when(&self) -> String {
        match &self.entry {
            Some(e) => format!("modified {}", fmt_date(e.modified)),
            None => "not there any more".to_string(),
        }
    }

    /// The demo reading before it is handed to the shared viewer.
    #[must_use]
    #[cfg(test)]
    pub fn preview(&self) -> &Preview {
        &self.preview
    }

    pub fn viewer_preview(&self) -> ViewPreview {
        if matches!(
            self.kind(),
            FileKind::Text | FileKind::Image | FileKind::Pdf | FileKind::Video | FileKind::Audio
        ) {
            if let Some(factory) = &self.viewer_disk {
                return ViewPreview::Disk {
                    factory: factory.clone(),
                    path: kernel::caps::real_path(&self.path),
                    name: self.name(),
                    kind: self.kind(),
                    size: self.size(),
                };
            }
        }
        // A clip or a sound is played from its path, not read through the
        // world's disk; the demo tree has no bytes behind its names, so a
        // card on one of its clips draws the surface and the strip, and
        // *play* finds nothing to play.
        if self.kind().plays() && self.entry.is_some() {
            return ViewPreview::Path {
                path: kernel::caps::real_path(&self.path),
                name: self.name(),
                kind: self.kind(),
                size: self.size(),
            };
        }
        self.preview.clone().into()
    }

    pub fn viewer(&self) -> Controller {
        self.viewer.clone()
    }

    /// The reading, where the preview is a text file's.
    #[cfg(test)]
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        match &self.preview {
            Preview::Text(t) => Some(t),
            _ => None,
        }
    }

    /// Which reading is on the card. The widget decodes a picture once per
    /// reading rather than once a frame, so it needs to know when the
    /// reading *changed* — which is not the same question as whether
    /// anybody wrote a disk.
    #[must_use]
    pub fn read_at(&self) -> u64 {
        self.read
    }

    /// Whether the disk still has it.
    #[must_use]
    pub fn gone(&self) -> bool {
        self.entry.is_none()
    }

    /// The `rename` field's text, while it is open — the field the card
    /// raises under its own name.
    #[must_use]
    pub fn renaming(&self) -> Option<&str> {
        self.renaming.as_deref()
    }

    /// Opens, closes, or edits it. `None` closes.
    pub fn set_renaming(&mut self, text: Option<String>) {
        self.renaming = text;
    }

    /// What the last verb refused, until the next one.
    #[must_use]
    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    pub fn set_status(&mut self, line: Option<String>) {
        self.status = line;
    }

    /// Called from the draw, and only from the draw, and before anything
    /// reads [`Card::note`]: as a listing's
    /// [`Dir::drawn`](super::Dir::drawn).
    pub fn drawn(&mut self) {
        let (drew, doing) = FILES.drawing(run::whose_world(&self.world));
        self.drew = drew;
        self.doing = doing;
    }

    /// The line the card draws under its header: what a run is doing, or —
    /// while nothing is running — what the last verb refused. As a
    /// listing's [`Dir::note`](super::Dir::note), and for the same reason:
    /// the run is the app's, not the panel's.
    #[must_use]
    pub fn note(&self) -> Option<String> {
        self.doing
            .clone()
            .or_else(|| self.status().map(str::to_string))
    }

    // -- keeping up ------------------------------------------------------------

    /// Refresh metadata and invalidate the viewer only when this file changed.
    ///
    /// The kind decides whether anything is read at all, so a card over a
    /// 38 MB disk image costs one `stat`. The viewer measures loaded content;
    /// demo worlds seed the measurement from their inline preview.
    ///
    /// And a `stat` is all it costs when the file has not moved. Every path
    /// a run performs bumps the count that brings the card back here — a
    /// copy of forty thousand files does it forty thousand times, and none
    /// of them is about this file. A card that read again on each of them
    /// would hand its widget the same picture to decode once a frame, for
    /// the length of the run.
    pub fn restat(&mut self) {
        // Stamped before the file is read, as a listing is: what lands in
        // between leaves the card one reading behind, never wrongly fresh.
        if self.statting.is_some() {
            return;
        }
        self.seen = FILES.seen(&self.world, &self.dir);
        if let Some(disk) = self.viewer_disk.clone() {
            let path = super::super::model::real_path(&self.path);
            if self.entry.is_none() && self.status.is_none() {
                self.status = Some("loading…".into());
            }
            self.statting = Some(super::super::model::read_background(disk, move |d| {
                d.stat(&path)
            }));
            return;
        }
        self.take_stat(stat_in(&self.world, &self.path));
    }

    fn take_stat(&mut self, now: Option<Entry>) {
        if self.status.as_deref() == Some("loading…") {
            self.status = None;
        }
        if now == self.entry && self.read > 0 {
            return;
        }
        self.entry = now;
        self.read += 1;
        let (world, path) = (self.world.clone(), self.path.clone());
        self.preview = match &self.entry {
            Some(_) if self.viewer_disk.is_some() => Preview::None,
            Some(e) => preview_of(e.kind(), &e.name, e.size, |max| {
                read_in(&world, &path, max).ok()
            }),
            None => Preview::None,
        };
        self.viewer.measured(Measure::of(&self.preview));
    }

    /// Called on every draw and every event, as a list's is: the card asks
    /// again once anything has written the disk — a verb of the app's, or
    /// another program in the directory this file is in.
    pub fn observe(&mut self, _s: &Session) {
        if let Some(receive) = &mut self.opening {
            match receive.try_recv() {
                Ok(result) => {
                    self.opening = None;
                    self.status = result.err();
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {}
                Err(_) => {
                    self.opening = None;
                    self.status = Some("file opening stopped".into());
                }
            }
        }
        if let Some(rx) = &mut self.statting {
            match rx.try_recv() {
                Ok(Ok(entry)) => {
                    self.statting = None;
                    self.take_stat(entry);
                }
                Ok(Err(error)) => {
                    self.statting = None;
                    self.status = Some(error);
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {}
                Err(_) => {
                    self.statting = None;
                    self.status = Some("file read stopped".into());
                }
            }
        }
        if self.statting.is_none() && self.seen != FILES.seen(&self.world, &self.dir) {
            self.restat();
        }
    }
}

impl Panel for Card {
    fn id(&self) -> &PanelId {
        &self.id
    }

    fn title(&self) -> String {
        self.name()
    }

    /// One file, and where its facts come from.
    fn about(&self) -> String {
        format!(
            "One file as a card: {} — its name, its kind, its size, its date, \
             its path, and a shared viewer for text, images, and PDF pages. Its argument is that path; nothing is stored \
             about it, so every fact here comes off the disk when the panel \
             opens and again whenever a verb writes. The verbs are the file's \
             own: open it with the operating system, hold it for a copy or a \
             move, rename it, or put it in the trash.",
            self.path
        )
    }

    /// Three rows as the floor, more when the preview needs them — a long
    /// text file opens tall rather than scrolled, a tall picture is seen
    /// whole — up to what a grid is likely to hold. The layout clamps it to
    /// the grid there actually is.
    fn wish(&self, cols: usize) -> (u32, u32) {
        let measure = self.viewer.measure();
        if measure == Measure::Empty && self.kind() == FileKind::Pdf {
            Measure::Pdf(595, 842).wish(cols, 7)
        } else {
            measure.wish(cols, 7)
        }
    }

    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }

    /// Every verb acts on the file the card shows: `open` hands it to the
    /// OS, `copy` and `move` hold it for a `… here`, `rename` raises a field
    /// under the name, `delete` puts it in the trash and takes this card
    /// with it — it would be showing nothing — and `copy path` puts what it
    /// is called on this machine on the system clipboard. Reads *open copy
    /// move rename delete copy path*: the destructive one before the
    /// harmless one, as on a message.
    ///
    /// `copy` wears the `p` of "copy", not the `c`: a card's path is
    /// selectable, so cmd+c copies the path — the file clipboard is not the
    /// text one. Which is why `c` is exactly the letter `copy path` wears:
    /// the chord copies a path either way. Which of the two answers is the
    /// widget's — a caret in one of its runs keeps the text chords, and the
    /// bar has the letter the rest of the time.
    fn verbs(&self) -> Vec<Verb> {
        let mut v = Vec::new();
        // As a listing's, and first for the same reason: a run is the
        // app's, it stops from wherever anybody is looking, and the one
        // control with no chord behind it may not be the one a narrow
        // panel drops off the end of its last row.
        if FILES.busy(run::whose_world(&self.world)) {
            v.push(Verb::run("files.cancel", "cancel", None));
        }
        if self.kind() == FileKind::Text && !self.gone() {
            if let Some(id) = &self.editor {
                v.push(Verb::go(
                    "files.edit",
                    "edit",
                    Some('e'),
                    kernel::nav::Nav::Open {
                        from: self.slot,
                        id: id.clone(),
                        fresh: false,
                    },
                ));
            }
        }
        v.extend([
            Verb::run("files.open", "open", Some('o')),
            Verb::run("files.copy", "copy", Some('p')),
            Verb::run("files.move", "move", Some('m')),
            Verb::run("files.rename", "rename", Some('r')),
            Verb::run("files.delete", "delete", Some('d')),
            Verb::run("files.copy_path", "copy path", Some('c')),
        ]);
        v.extend(self.viewer.verbs());
        v
    }

    fn run(&mut self, verb: &str, s: &mut Session) {
        if self.viewer.run(verb) {
            s.redraw();
            return;
        }
        match verb {
            "files.open" => self.open(s),
            "files.copy" => dir::hold(s, Op::Copy, vec![self.path.clone()]),
            "files.move" => dir::hold(s, Op::Move, vec![self.path.clone()]),
            "files.rename" => self.start_rename(s),
            "files.delete" => self.delete(s),
            "files.copy_path" => self.copy_path(s),
            "files.cancel" => dir::cancel(s, &self.world, self.drew),
            _ => {}
        }
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

impl Card {
    /// `open`: the file handed to whatever the OS opens it with.
    fn open(&mut self, s: &mut Session) {
        if self.opening.is_some() {
            return;
        }
        if let Some(factory) = s.world().factory().filter(|_| s.store().ui_attached()) {
            let (path, name) = (self.path.clone(), self.name());
            let (send, receive) = tokio::sync::oneshot::channel();
            self.opening = Some(receive);
            self.status = Some("opening…".into());
            s.prepare_work(
                move |_| {
                    Box::pin(async move {
                        kernel::runtime::spawn_blocking(move || {
                            let world = factory.build().map_err(|error| error.to_string())?;
                            ops::open_in(&world, &path)
                        })
                        .await
                        .map_err(|error| error.to_string())?
                    })
                },
                move |s, result| {
                    match &result {
                        Ok(()) => s.notify(format!("opened “{name}”"), false),
                        Err(error) => s.notify(error.clone(), true),
                    }
                    let _ = send.send(result);
                },
            );
            return;
        }
        let world = s.world().clone();
        match ops::open_in(&world, &self.path) {
            Ok(()) => s.notify(format!("opened “{}”", self.name()), false),
            Err(e) => {
                self.status = Some(e.clone());
                s.notify(e, true);
            }
        }
    }

    /// `rename`: the field under the name, seeded with the name it has and
    /// landing with all of it selected — a rename is a value typed over,
    /// not one typed after.
    ///
    /// Focus follows the field. A card is usually the thing under a list's
    /// cursor, so this verb arrives through that list's chord — and a caret
    /// on an unfocused panel would never see a letter typed at it.
    fn start_rename(&mut self, s: &mut Session) {
        self.renaming = match self.renaming {
            Some(_) => None,
            None => Some(self.name()),
        };
        self.status = None;
        if self.renaming.is_some() {
            s.nav(Nav::Focus(self.slot));
        }
        s.redraw();
    }

    /// The name the field submitted: the file under it, in the directory it
    /// is already in. The card goes with the file — its identity is the
    /// path — so the layout half of the same action points this slot at the
    /// new one, and cmd+z brings back both the old name and the card on it.
    pub fn rename(&mut self, s: &mut Session, name: &str) {
        let (slot, path) = (self.slot, self.path.clone());
        match dir::rename_path(s, slot, &path, name, Card::id) {
            dir::Said::Went => {
                self.renaming = None;
                self.status = None;
            }
            dir::Said::Refused(line) => self.status = Some(line),
            // The field holds what is being made until the run comes back
            // to close it — as a listing's does.
            dir::Said::Doing => {
                self.renaming = Some(name.trim().to_string());
                self.status = None;
            }
            dir::Said::Nothing => {}
        }
        self.restat();
    }

    /// `copy path`: what this file is called on the machine, onto the
    /// system clipboard. Nothing is read and nothing written, so the card
    /// stands as it was.
    fn copy_path(&mut self, s: &mut Session) {
        match dir::copy_paths(s, vec![self.path.clone()]) {
            dir::Said::Went => self.status = None,
            dir::Said::Refused(line) => self.status = Some(line),
            dir::Said::Doing | dir::Said::Nothing => {}
        }
    }

    /// `delete`: the file to the trash, and this card closed in the layout
    /// half of the same action — it would be showing nothing. The trash is
    /// a run like any other, so the card stands until it lands, and the
    /// settle that records the node is what drops it. No other panel is
    /// looked for: one somewhere else showing the same path keeps showing
    /// it, and says so.
    fn delete(&mut self, s: &mut Session) {
        if dir::delete_paths(s, self.slot, &self.id, vec![self.path.clone()], true, false) {
            self.status = None;
        }
    }
}

/// Its factory.
pub struct CardKind;

impl PanelKind for CardKind {
    fn tag(&self) -> Tag {
        Card::TAG
    }

    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        let path = Card::of(id).unwrap_or_default().to_string();
        let world = cx.session().world().clone();
        // A path with no parent is a root, which is a directory itself:
        // watch it rather than nothing.
        let dir = parent(&path).unwrap_or(&path).to_string();
        let _watch = Watch::on(&world, &dir);
        let mut card = Card {
            editor: cx
                .session()
                .apps()
                .get_as::<crate::apps::notes::Notes>()
                .map(|app| app.editor(&path)),
            id: id.clone(),
            path,
            dir,
            slot: 0,
            world,
            entry: None,
            preview: Preview::None,
            viewer: Controller::default(),
            viewer_disk: cx
                .session()
                .world()
                .with_cap::<super::super::ViewerDisk, _>(|r| r.0.clone())
                .ok(),
            renaming: None,
            status: None,
            seen: Seen::default(),
            statting: None,
            opening: None,
            read: 0,
            drew: 0,
            doing: None,
            _watch,
        };
        card.restat();
        Box::new(card)
    }
}
