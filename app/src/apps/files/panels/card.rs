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
    basename, image_lines, image_size, parent, preview_of, read_in, stat_in, text_lines, Entry,
    FileKind, Preview, Watch,
};
use super::super::ops;
use super::super::run;
use super::super::{Op, Seen, FILES};
use super::dir;

/// What the card spends on everything that is not the preview: the name,
/// the kind line, the date, the path, the rule and the padding around
/// them, in lines.
const CHROME_LINES: usize = 7;

/// How many lines of text one grid row holds, near enough for a wish. The
/// panel cannot measure the viewport — [`Panel::wish`] is given the column's
/// width in characters and nothing else — and the layout clamps whatever
/// this asks for to the grid it actually has.
const ROW_LINES: usize = 6;

/// The rows a card asks for at its shortest, and the most it will ask for.
const ROWS: (u32, u32) = (3, 6);

/// One file, shown.
///
/// Everything the card draws is read when the panel opens and again
/// whenever a verb writes the disk or the watcher says another program
/// has: the entry, and — for a text file or a picture small enough to be
/// worth it — the preview under the rule.
pub struct Card {
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
    /// What there is to show of the contents: a text file's reading, a
    /// picture's bytes, or nothing at all.
    preview: Preview,
    /// A picture's `(width, height)`, off the header of the bytes above.
    /// Kept because [`Panel::wish`] is asked on every relayout, and reading
    /// a header on each of them would be a read a frame.
    pixels: Option<(u32, u32)>,
    /// The `rename` field, while it is open: the new name as typed.
    renaming: Option<String>,
    /// The line under the header: what a verb refused, until the next one.
    status: Option<String>,
    /// What this reading was taken at, on both counts.
    seen: Seen,
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

    /// The preview: the first 64 KiB of a text file, a picture's bytes, or
    /// nothing at all — what the card draws under the rule.
    #[must_use]
    pub fn preview(&self) -> &Preview {
        &self.preview
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

    /// The bytes, where it is a picture's. Decoded by whoever draws it, off
    /// their own account of themselves rather than off the name.
    #[must_use]
    pub fn image(&self) -> Option<&[u8]> {
        match &self.preview {
            Preview::Image(b) => Some(b),
            _ => None,
        }
    }

    /// A picture's size in pixels, where the preview is one.
    #[cfg(test)]
    #[must_use]
    pub fn pixels(&self) -> Option<(u32, u32)> {
        self.pixels
    }

    /// What the card last read, as the two counts it was read at. The
    /// widget decodes a picture once per reading rather than once a frame,
    /// so it needs to know when the reading changed.
    #[must_use]
    pub fn read_at(&self) -> Seen {
        self.seen
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

    /// The line the card draws under its header: what a run is doing, or —
    /// while nothing is running — what the last verb refused. As a
    /// listing's [`Dir::note`](super::Dir::note), and for the same reason:
    /// the run is the app's, not the panel's.
    #[must_use]
    pub fn note(&self) -> Option<String> {
        FILES
            .running(run::whose_world(&self.world))
            .map(|at| at.line())
            .or_else(|| self.status().map(str::to_string))
    }


    // -- keeping up ------------------------------------------------------------

    /// Reads the file again: what it is, and what it shows.
    ///
    /// The kind decides whether anything is read at all, so a card over a
    /// 38 MB disk image costs one `stat`; a picture's size is taken off the
    /// same bytes the card will draw, so the header is read once and not
    /// again on every wish.
    pub fn restat(&mut self) {
        // Stamped before the file is read, as a listing is: what lands in
        // between leaves the card one reading behind, never wrongly fresh.
        self.seen = FILES.seen(&self.world, &self.dir);
        self.entry = stat_in(&self.world, &self.path);
        let (world, path) = (self.world.clone(), self.path.clone());
        self.preview = match &self.entry {
            Some(e) => preview_of(e.kind(), &e.name, e.size, |max| {
                read_in(&world, &path, max).ok()
            }),
            None => Preview::None,
        };
        self.pixels = self.image().and_then(image_size);
    }

    /// Called on every draw and every event, as a list's is: the card asks
    /// again once anything has written the disk — a verb of the app's, or
    /// another program in the directory this file is in.
    pub fn observe(&mut self, _s: &Session) {
        if self.seen != FILES.seen(&self.world, &self.dir) {
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

    /// Three rows as the floor, more when the preview needs them — a long
    /// text file opens tall rather than scrolled, a tall picture is seen
    /// whole — up to what a grid is likely to hold. The layout clamps it to
    /// the grid there actually is.
    fn wish(&self, cols: usize) -> (u32, u32) {
        let lines = match (&self.preview, self.pixels) {
            (Preview::Text(t), _) => text_lines(t, cols),
            // The picture is drawn at the text's width, so what it costs in
            // lines is its aspect at that width.
            (Preview::Image(_), Some((w, h))) => image_lines(cols, w, h).ceil() as usize,
            _ => 0,
        };
        let rows = (CHROME_LINES + lines).div_ceil(ROW_LINES) as u32;
        (4, rows.clamp(ROWS.0, ROWS.1))
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
        // panel drops off the end of its row.
        if FILES.busy(run::whose_world(&self.world)) {
            v.push(Verb::run("files.cancel", "cancel", None));
        }
        v.extend([
            Verb::run("files.open", "open", Some('o')),
            Verb::run("files.copy", "copy", Some('p')),
            Verb::run("files.move", "move", Some('m')),
            Verb::run("files.rename", "rename", Some('r')),
            Verb::run("files.delete", "delete", Some('d')),
            Verb::run("files.copy_path", "copy path", Some('c')),
        ]);
        v
    }

    fn run(&mut self, verb: &str, s: &mut Session) {
        match verb {
            "files.open" => self.open(s),
            "files.copy" => dir::hold(s, Op::Copy, vec![self.path.clone()]),
            "files.move" => dir::hold(s, Op::Move, vec![self.path.clone()]),
            "files.rename" => self.start_rename(s),
            "files.delete" => self.delete(s),
            "files.copy_path" => self.copy_path(s),
            "files.cancel" => dir::cancel(s, &self.world),
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
            dir::Said::Nothing => {}
        }
    }

    /// `delete`: the file to the trash, and this card closed in the layout
    /// half of the same action — it would be showing nothing. The trash is
    /// a run like any other, so the card stands until it lands, and the
    /// settle that records the node is what drops it. No other panel is
    /// looked for: one somewhere else showing the same path keeps showing
    /// it, and says so.
    fn delete(&mut self, s: &mut Session) {
        if dir::delete_paths(s, self.slot, &self.id, vec![self.path.clone()], true) {
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
            id: id.clone(),
            path,
            dir,
            slot: 0,
            world,
            entry: None,
            preview: Preview::None,
            pixels: None,
            renaming: None,
            status: None,
            seen: Seen::default(),
            _watch,
        };
        card.restat();
        Box::new(card)
    }
}

