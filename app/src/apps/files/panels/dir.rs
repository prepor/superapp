//! The `files` panel: one directory as a rich table.

use std::any::Any;
use std::cell::RefCell;
use std::rc::{Rc, Weak};

use kernel::effect::World;
use kernel::history::Intent;
use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::panel::{Open, Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::richtable::ListState;
use kernel::session::{Action, Instance, Session};

use super::super::completion::PathCompletion;
use super::super::model::{
    basename, crumbs, id_in, is_dir_in, is_root, join, list_in, normalize, parent, plural,
    real_path, stat_in, DirRow, DirSource, Entry, Watch, HOME, PAGE,
};
use super::super::ops;
use super::super::run::{self, Landed, Run, Task};
use super::super::{Op, Seen, FILES};
use super::Card;

/// Where the panel stands in the join chain, as of the last look.
///
/// [`Panel::verbs`] is pulled with no session to ask, so the answer is kept
/// here and refreshed by [`Dir::observe`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Chain {
    /// Something drives this panel: it hangs under a list, and that list's
    /// chords reach this bar.
    pub under: bool,
    /// It drives something itself: its own cursor is walking a child.
    pub driving: bool,
}

/// One directory, listed.
///
/// The listing is read through the disk when the panel opens and again
/// whenever a verb — anyone's — writes, or the watcher says another
/// program has; the filter, the cursor and the marks are the rich table's,
/// owned here.
pub struct Dir {
    id: PanelId,
    dir: String,
    slot: SlotId,
    world: Rc<World>,
    list: ListState<DirSource>,
    chain: Chain,
    /// The `new dir` field, while it is open: the name as typed.
    naming: Option<String>,
    /// The `rename` field, while it is open: the new name as typed.
    renaming: Option<String>,
    /// The `go to` field, while it is open: the path as typed.
    pathing: Option<String>,
    /// The line under the header: what a verb refused, until the next one.
    status: Option<String>,
    /// What the listing was read at, on both counts.
    seen: Seen,
    /// The run the line under the header was about, as of the last time
    /// this panel was **drawn**, and the line itself. What its *cancel* is
    /// a button for: a run that finished between the frame and the press is
    /// not its successor's to answer for. Read together, so the words and
    /// the number are never about two different runs.
    drew: u64,
    doing: Option<String>,
    /// The directory watched for as long as this panel shows it. Held, not
    /// read: dropping it is what lets the watcher go.
    _watch: Watch,
}

impl Dir {
    /// The persisted spelling. One argument: the directory, in the display
    /// spelling (`~/Downloads`).
    pub const TAG: Tag = Tag("files");

    /// The panel that lists a directory.
    #[must_use]
    pub fn id(dir: &str) -> PanelId {
        PanelId::new(Self::TAG, [dir])
    }

    /// The directory a `files` panel lists; `None` for any other tag.
    #[must_use]
    pub fn of(id: &PanelId) -> Option<&str> {
        (id.tag == Self::TAG).then(|| id.arg(0)).flatten()
    }

    /// The table, the cursor and the marks, read-only. The widget drives
    /// them through [`Dir::list_mut`]; this read is the tests' own door onto
    /// what it did.
    #[cfg(test)]
    #[must_use]
    pub fn list(&self) -> &ListState<DirSource> {
        &self.list
    }

    pub fn list_mut(&mut self) -> &mut ListState<DirSource> {
        &mut self.list
    }

    /// The crumb line above the listing, each segment with the panel it
    /// goes to: `~ / Downloads / 2026`. A crumb replaces this panel in
    /// place — it is the same walk, one directory up.
    #[must_use]
    pub fn crumbs(&self) -> Vec<(String, PanelId)> {
        crumbs(&self.dir)
            .into_iter()
            .map(|(label, path)| (label, Dir::id(&path)))
            .collect()
    }

    /// The completion the `go to` field offers.
    #[must_use]
    pub fn completion(&self) -> PathCompletion {
        PathCompletion {
            world: self.world.clone(),
        }
    }

    // -- what the widget tells it ---------------------------------------------

    /// Called on every draw and every event: where the panel stands in the
    /// join chain, and whether the disk has moved since this listing was
    /// read — by a verb of the app's, or under it.
    ///
    /// Neither answer can be had from inside [`Panel::verbs`], which has no
    /// session — so both are pushed in from where a session is at hand.
    /// The watcher is asked here rather than pushed from outside for the
    /// same reason it is asked at all: a draw is the one signal, and this
    /// runs on every one of them.
    pub fn observe(&mut self, s: &Session) {
        self.chain = Chain {
            under: s.join_parent_of(self.slot).is_some(),
            driving: s.joined_child(self.slot).is_some(),
        };
        if self.seen != FILES.seen(&self.world, &self.dir) {
            self.relist();
        }
    }

    /// Whether this panel is the object under someone's cursor: it hangs
    /// under a list and drives nothing itself.
    ///
    /// The end of a chain is the thing the cursor is on. A row previews the
    /// directory's own panel beside the list, and *that* panel wears
    /// `copy`, `move`, `delete` and `copy path` for the directory it shows,
    /// which the list borrows through the chord routing. A root, an
    /// un-joined panel, or a list that is itself driving a preview is
    /// nobody's object right now: `~` cannot be deleted, and a chord in a
    /// list may not hit the directory the list shows when it means the row
    /// under the cursor.
    #[must_use]
    pub fn object(&self) -> bool {
        self.chain.under && !self.chain.driving && !is_root(&self.dir)
    }

    /// Lists the directory again, keeping the filter, the cursor and the
    /// marks.
    ///
    /// Marks are not pruned here: a mark whose row has gone altogether is
    /// dropped by [`ListState::sync`], which is the draw's own step — so
    /// what a verb leaves marked is exactly what it could not do, until
    /// the next draw reads the listing under it.
    pub fn relist(&mut self) {
        // Stamped before the directory is read, not after: a change that
        // lands while it is being read then leaves the panel one reading
        // behind rather than believing it is up to date.
        self.seen = FILES.seen(&self.world, &self.dir);
        let entries = match list_in(&self.world, &self.dir) {
            Ok(v) => v,
            Err(e) => {
                self.status = Some(e);
                Vec::new()
            }
        };
        self.list
            .table_mut()
            .retarget(DirSource::new(&self.dir, entries));
    }

    // -- the three fields ------------------------------------------------------

    /// The `new dir` field's text, while it is open.
    #[must_use]
    pub fn naming(&self) -> Option<&str> {
        self.naming.as_deref()
    }

    /// Opens, closes, or edits it. `None` closes.
    pub fn set_naming(&mut self, text: Option<String>) {
        self.naming = text;
    }

    /// The `rename` field's text, while it is open.
    #[must_use]
    pub fn renaming(&self) -> Option<&str> {
        self.renaming.as_deref()
    }

    pub fn set_renaming(&mut self, text: Option<String>) {
        self.renaming = text;
    }

    /// The `go to` field's text, while it is open.
    #[must_use]
    pub fn pathing(&self) -> Option<&str> {
        self.pathing.as_deref()
    }

    pub fn set_pathing(&mut self, text: Option<String>) {
        self.pathing = text;
    }

    /// What the last verb refused, until the next one.
    #[must_use]
    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    pub fn set_status(&mut self, line: Option<String>) {
        self.status = line;
    }

    /// The line the panel actually draws under its header: what a run is
    /// doing, or — while nothing is running — what the last verb refused.
    ///
    /// The run wins because it is happening *now*, on the one disk every
    /// files panel is looking at, and because it is what the *cancel* on
    /// the bar is about. The refusal is not lost: it comes back when the
    /// run is over.
    /// Called from the draw, and only from the draw, and before anything
    /// reads [`Dir::note`]: what to draw under the header, and which run it
    /// is about.
    pub fn drawn(&mut self) {
        let (drew, doing) = FILES.drawing(run::whose_world(&self.world));
        self.drew = drew;
        self.doing = doing;
    }

    /// The run this panel last drew a line for; zero for none. The verb
    /// reads the field itself — this is the tests' door onto it.
    #[cfg(test)]
    #[must_use]
    pub fn drew(&self) -> u64 {
        self.drew
    }

    #[must_use]
    pub fn note(&self) -> Option<String> {
        self.doing
            .clone()
            .or_else(|| self.status().map(str::to_string))
    }

    // -- where a row goes ------------------------------------------------------

    /// What a row names: a directory is a list of its own, a file is a
    /// card. The table asks [`row_target`] instead, which needs no instance.
    #[cfg(test)]
    #[must_use]
    pub fn row_id(&self, e: &Entry) -> PanelId {
        target_of(&self.dir, e)
    }

    /// The preview a cursor walk sends when it lands on a row: focus stays
    /// on the list, and the child shows what the cursor is on.
    #[cfg(test)]
    #[must_use]
    pub fn preview(&self, e: &Entry) -> Nav {
        Nav::Preview {
            from: self.slot,
            id: self.row_id(e),
        }
    }

    /// Where the `go to` field's text leads: a directory replaces this
    /// panel in place — it is the same walk — and a file is previewed
    /// beside it. `None` for a spelling that names nothing, and then the
    /// status line says what happened.
    pub fn go_to(&mut self, typed: &str) -> Option<Nav> {
        let Some(path) = normalize(typed) else {
            self.status = Some(format!("“{}” is not a path", typed.trim()));
            return None;
        };
        if stat_in(&self.world, &path).is_none() {
            self.status = Some(format!("“{path}” is not there"));
            return None;
        }
        self.status = None;
        self.pathing = None;
        Some(if is_dir_in(&self.world, &path) {
            Nav::Replace {
                slot: self.slot,
                id: Dir::id(&path),
            }
        } else {
            Nav::Preview {
                from: self.slot,
                id: Card::id(&path),
            }
        })
    }

    /// `new dir`: one directory, where nothing is yet — one undoable
    /// action, whose reversal trashes it while it is still empty.
    ///
    /// The write gate is asked here and the lease again when the run lands:
    /// a change with no node behind it is a change nobody can undo. What
    /// happens in between is [`run`](super::super::run)'s — one `mkdir` is
    /// hardly a freeze, but a directory on a volume that has gone to sleep
    /// is, and there is no second way to write a disk in this app.
    ///
    /// The widget calls this from the field's submit, on the instance it is
    /// already holding — the same `&mut self` a verb of the bar has. The
    /// field stays up, with the name in it, until the disk has answered.
    pub fn new_dir(&mut self, s: &mut Session, name: &str) {
        let name = name.trim().to_string();
        if name.is_empty() {
            return;
        }
        if !s.writable() {
            s.notify("read-only — acquire the lease to write", true);
            return;
        }
        self.status = None;
        // The field holds what is being made, for as long as it is out:
        // that is what it says on screen, and what the run compares against
        // when it comes back to close it. A field that was never open stays
        // shut — a submit is not a way to raise one.
        if self.naming.is_some() {
            self.naming = Some(name.clone());
        }
        let path = join(&self.dir, &name);
        FILES.start(s, Task::MakeDir { path }, self.slot, self.id.clone());
    }

    /// `rename`: the directory this panel shows, under a new name — one
    /// undoable action, whose reversal puts the old name back. The panel
    /// goes with it: its identity is the path, so the layout half of the
    /// same node points this slot at the new one.
    ///
    /// The widget calls this from the field's submit, on the instance it is
    /// already holding — the same `&mut self` a verb of the bar has.
    pub fn rename(&mut self, s: &mut Session, name: &str) {
        let (slot, path) = (self.slot, self.dir.clone());
        match rename_path(s, slot, &path, name, Dir::id) {
            Said::Went => {
                self.renaming = None;
                self.status = None;
            }
            Said::Refused(line) => self.status = Some(line),
            // The field holds what is being made, for as long as the run
            // is out: that is what it says on screen, and what the run
            // compares against when it comes back to close it.
            Said::Doing => {
                self.renaming = Some(name.trim().to_string());
                self.status = None;
            }
            Said::Nothing => {}
        }
        self.relist();
    }
}

/// What a row of a listing names, given where the listing stands: a
/// directory is a list of its own, a file is a card.
fn target_of(dir: &str, e: &Entry) -> PanelId {
    let path = join(dir, &e.name);
    if e.is_dir {
        Dir::id(&path)
    } else {
        Card::id(&path)
    }
}

/// The same off a row alone. A row carries the directory it is in, so the
/// rich table can ask it what it opens while the instance that owns the
/// listing is borrowed for the draw.
#[must_use]
pub fn row_target(r: &DirRow) -> PanelId {
    target_of(&r.dir, &r.entry)
}

impl Panel for Dir {
    fn id(&self) -> &PanelId {
        &self.id
    }

    fn title(&self) -> String {
        basename(&self.dir).to_string()
    }

    /// A list is a column: as wide as a panel gets by default, and tall
    /// enough that a directory is read rather than scrolled.
    fn wish(&self, _cols: usize) -> (u32, u32) {
        (4, 6)
    }

    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }

    /// `new dir` and `go to` open two of the three fields; the object verbs
    /// act on the directory this panel shows, or — while rows are marked —
    /// on the marked set, which is the same verb over more than one thing
    /// and so wears the same letter; then the two verbs about the set
    /// itself; and while the clipboard holds something, `copy here` or `move
    /// here` names what will happen to it.
    ///
    /// `rename` is the exception to the batch rule: a name is a name, and
    /// two things cannot both wear it — so it is offered over the one
    /// directory this panel shows and never over a marked set. It opens the
    /// third field.
    ///
    /// *mark all* wears `a` rather than the obvious `l` — this shell keeps
    /// `cmd+l` for itself (see [`keys`](crate::shell::keys)), and a bar may
    /// not promise a chord that never arrives — and rather than mail's `m`,
    /// which here is *move*: no bar may wear one letter twice. *clear* wears
    /// none, because `esc` is the table's own. *copy path* wears the `c`
    /// that *copy* could not: `p` is the disk copy's, and `c` is the letter
    /// that copies text wherever text is selected — which is what this verb
    /// does with a path.
    fn verbs(&self) -> Vec<Verb> {
        let mut v = Vec::new();
        // A run belongs to the app rather than to the panel that started it
        // — it is one disk — so *cancel* is on every files bar while one is
        // on, and stops it from wherever anybody is looking. It wears no
        // letter: it is rare, it is the only verb here that undoes nothing,
        // and no chord should be a keystroke away from stopping a copy.
        //
        // First, and not last, for exactly that reason: a bar wraps only so
        // far, and past that a verb that would run off the end is a verb
        // that is not drawn — and the one control with no chord behind it
        // may not be the one a narrow panel drops.
        if FILES.busy(run::whose_world(&self.world)) {
            v.push(Verb::run("files.cancel", "cancel", None));
        }
        v.push(Verb::run("files.new_dir", "new dir", Some('n')));
        v.push(Verb::run("files.go_to", "go to", Some('g')));
        let marked = self.list.marks().len();
        if marked > 0 {
            v.push(Verb::run("files.copy", format!("copy {marked}"), Some('p')));
            v.push(Verb::run("files.move", format!("move {marked}"), Some('m')));
            v.push(Verb::run(
                "files.delete",
                format!("delete {marked}"),
                Some('d'),
            ));
            v.push(Verb::run(
                "files.copy_path",
                format!("copy {marked} paths"),
                Some('c'),
            ));
            v.push(Verb::run("files.all", "mark all", Some('a')));
            v.push(Verb::run("files.clear", "clear", None));
        } else if self.object() {
            v.push(Verb::run("files.copy", "copy", Some('p')));
            v.push(Verb::run("files.move", "move", Some('m')));
            v.push(Verb::run("files.rename", "rename", Some('r')));
            v.push(Verb::run("files.delete", "delete", Some('d')));
            v.push(Verb::run("files.copy_path", "copy path", Some('c')));
        }
        let clip = FILES.clipboard();
        if !clip.is_empty() {
            v.push(Verb::run("files.here", clip.verb.here_label(), Some('h')));
        }
        v
    }

    /// Each of them on the panel's own state: the two fields it raises, the
    /// marked set or the directory it shows, and the clipboard laid down
    /// here.
    fn run(&mut self, verb: &str, s: &mut Session) {
        match verb {
            "files.new_dir" => {
                self.naming = match self.naming {
                    Some(_) => None,
                    None => Some(String::new()),
                };
                self.status = None;
                s.redraw();
            }
            "files.go_to" => {
                // Seeded with where the panel stands, so a walk starts from
                // here and a typed absolute path still wins.
                self.pathing = match self.pathing {
                    Some(_) => None,
                    None => Some(format!("{}/", self.dir.trim_end_matches('/'))),
                };
                self.status = None;
                s.redraw();
            }
            "files.rename" => {
                // Seeded with the name the directory has, which the field
                // lands with all of it selected: a rename is a value typed
                // over, not one typed after. Focus follows the field,
                // because this verb reaches a previewed panel through the
                // list above it and a caret needs the keyboard.
                self.renaming = match self.renaming {
                    Some(_) => None,
                    None => Some(basename(&self.dir).to_string()),
                };
                self.status = None;
                if self.renaming.is_some() {
                    s.nav(Nav::Focus(self.slot));
                }
                s.redraw();
            }
            "files.copy" => self.hold(s, Op::Copy),
            "files.move" => self.hold(s, Op::Move),
            "files.delete" => self.delete(s),
            "files.copy_path" => self.copy_path(s),
            "files.here" => self.here(s),
            "files.cancel" => cancel(s, &self.world, self.drew),
            // The two about the set itself. Neither writes anything, so
            // neither is an action: a mark is the panel's own context.
            "files.all" => {
                let store = s.store().clone();
                self.list.mark_all(&store);
                s.redraw();
            }
            "files.clear" => {
                self.list.clear_marks();
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
pub struct DirKind;

impl PanelKind for DirKind {
    fn tag(&self) -> Tag {
        Dir::TAG
    }

    /// The listing is read here, in the action that is opening the panel:
    /// a directory with nothing in it is a panel that says so, not a panel
    /// that has not looked yet.
    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        let dir = Dir::of(id).unwrap_or(HOME).to_string();
        let world = cx.session().world().clone();
        // Watched before it is read, and stamped in between: a change that
        // lands while the directory is being listed is one this panel will
        // look again for.
        let _watch = Watch::on(&world, &dir);
        let seen = FILES.seen(&world, &dir);
        let (entries, status) = match list_in(&world, &dir) {
            Ok(v) => (v, None),
            Err(e) => (Vec::new(), Some(e)),
        };
        Box::new(Dir {
            id: id.clone(),
            world,
            list: ListState::new(DirSource::new(&dir, entries), PAGE),
            dir,
            slot: 0,
            // A previewed panel hangs under the list that previewed it,
            // which is the one thing an open already knows about the chain.
            chain: Chain {
                under: cx.how() == Open::Preview,
                driving: false,
            },
            naming: None,
            renaming: None,
            pathing: None,
            status,
            seen,
            drew: 0,
            doing: None,
            _watch,
        })
    }
}

// -- the verbs -----------------------------------------------------------------

impl Dir {
    /// What an object verb acts on: the marked rows, or — where nothing is
    /// marked and this panel is the object under somebody's cursor — the
    /// directory it shows. Empty when it is neither, which is exactly when
    /// the bar wears no such verb.
    fn objects(&self) -> Vec<String> {
        if !self.list.marks().is_empty() {
            let dir = self.dir.clone();
            self.list
                .marks()
                .keys()
                .into_iter()
                .map(|n| join(&dir, &n))
                .collect()
        } else if self.object() {
            vec![self.dir.clone()]
        } else {
            Vec::new()
        }
    }

    /// `copy` / `move`, over the marked set or over what the panel shows:
    /// the paths are held, every files panel offers `… here`, and the
    /// destination is still to be walked to. The marks stand — nothing has
    /// been consumed.
    fn hold(&mut self, s: &mut Session, op: Op) {
        hold(s, op, self.objects());
    }

    /// `copy path`, over the same set the other object verbs take: their
    /// names on this machine, onto the system clipboard. Nothing here is
    /// written, so the marks stand and the listing is as it was.
    fn copy_path(&mut self, s: &mut Session) {
        match copy_paths(s, self.objects()) {
            Said::Went => self.status = None,
            Said::Refused(line) => self.status = Some(line),
            Said::Doing | Said::Nothing => {}
        }
    }

    /// `delete`, over the marked set or over what the panel shows. A batch
    /// takes rows, and the list stays; the directory this panel *is* takes
    /// the panel with it — when the run lands, which is the same node.
    fn delete(&mut self, s: &mut Session) {
        let marked = !self.list.marks().is_empty();
        let paths = self.objects();
        if delete_paths(s, self.slot, &self.id, paths, !marked, marked) {
            // A run is on its way; the line it will write is its own. One
            // that was never queued leaves the line standing as it was.
            self.status = None;
        }
    }

    /// `copy here` / `move here`: the held set performed into the directory
    /// this panel shows.
    ///
    /// What is held is snapshotted here and planned when the run reaches
    /// the front of the queue, against the disk as it is *then* — the
    /// clipboard may have waited while another program moved things, or
    /// while an earlier run wrote the very directory this one lands in, and
    /// a watch says that a directory changed, never what is still in it.
    /// What the plan refuses, it refuses path by path, exactly as it does
    /// for one; what it can do becomes **one** undoable action, so a single
    /// cmd+z takes the whole batch back.
    fn here(&mut self, s: &mut Session) {
        let clip = FILES.clipboard();
        if clip.is_empty() {
            return;
        }
        if !s.writable() {
            s.notify("read-only — acquire the lease to write", true);
            return;
        }
        self.status = None;
        let task = Task::Here {
            verb: clip.verb,
            clip,
            dir: self.dir.clone(),
        };
        FILES.start(s, task, self.slot, self.id.clone());
    }
}

/// `copy` / `move`: the paths are held, every files panel offers `… here`,
/// and the destination is still to be walked to.
pub(super) fn hold(s: &mut Session, op: Op, paths: Vec<String>) {
    if paths.is_empty() {
        return;
    }
    FILES.set(op, paths);
    let clip = FILES.clipboard();
    s.notify(
        format!(
            "{} {}: choose where, then {}",
            op.verb(),
            clip.what(),
            op.here_label()
        ),
        false,
    );
    // The bars of every panel change with the clipboard; a redraw is the
    // one signal they need.
    s.redraw();
}

/// `copy path`, from a card or from a listing: what the paths are called
/// on this machine, one to a line, onto the system clipboard.
///
/// Nothing of ours changes — no disk, no store — so there is no action and
/// nothing to undo; what happened is the toast, and the effect log has the
/// row. The write gate is not asked either: a clipboard is not a device's
/// to lease.
pub(super) fn copy_paths(s: &mut Session, paths: Vec<String>) -> Said {
    if paths.is_empty() {
        return Said::Nothing;
    }
    let world = s.world().clone();
    if let Err(e) = ops::clip_paths(&world, &paths) {
        s.notify(e.clone(), true);
        return Said::Refused(e);
    }
    let what = match paths.as_slice() {
        [one] => format!("“{}”", real_path(one).display()),
        many => format!("{} paths", many.len()),
    };
    s.notify(format!("copied {what}"), false);
    Said::Went
}

/// What a verb leaves on the panel's own line: the panel is the one that
/// has one, so the shared half answers rather than writing it.
pub(super) enum Said {
    /// It happened: the line under the header goes.
    Went,
    /// It did not: this says why, where the field was.
    Refused(String),
    /// Nothing was attempted, and the line stands as it was.
    Nothing,
    /// It is on its way. The disk is a [run](super::super::run)'s to write,
    /// so the field stays out holding the name that is being made until the
    /// run comes back to close it — and what closes it is that it still
    /// holds *that* name.
    Doing,
}

/// `delete`, from a card or from a listing: to the trash, never `rm`.
///
/// The paths are handed to [`run`](super::super::run) and performed off
/// this thread; what comes back is [`land`]ed as **one** node, so one cmd+z
/// puts all of it back — and the reversal expires honestly, on a trash that
/// was emptied or a name something else has taken since.
///
/// `own` is whether `by` is the panel showing what went, and so closes with
/// the action. Answers whether a run was queued at all — a panel clears its
/// own line for one that was, and leaves it standing for one that was not.
pub(super) fn delete_paths(
    s: &mut Session,
    by: SlotId,
    showing: &PanelId,
    paths: Vec<String>,
    own: bool,
    marked: bool,
) -> bool {
    if paths.is_empty() {
        return false;
    }
    if !s.writable() {
        s.notify("read-only — acquire the lease to write", true);
        return false;
    }
    FILES.start(s, Task::Delete { paths, own, marked }, by, showing.clone());
    true
}

/// *cancel*: the run in hand stops where it is, and what was waiting behind
/// it never starts. What it managed is still recorded — see [`land`].
///
/// A run that had not begun leaves nothing to record and so nothing to say
/// for itself; this says it instead, rather than dropping work in silence.
pub(super) fn cancel(s: &mut Session, world: &World, drew: u64) {
    let dropped = FILES.stop(run::whose_world(world), drew);
    match dropped {
        0 => {}
        1 => s.notify("one run dropped — it had not started", false),
        n => s.notify(format!("{n} runs dropped — they had not started"), false),
    }
    s.redraw();
}

// -- what a run leaves behind --------------------------------------------------

/// A run that is over, recorded.
///
/// This is the other half of every verb above, and the *whole* of what they
/// used to do after the disk: the history node with its intents, the lease
/// check, the marks a delete consumed, the panel a delete closes, the
/// clipboard a move lets go of, the toast, and the listings that went stale.
/// It runs on the UI thread from [`Files::poll`](super::super::Files::poll),
/// which is [`Session::settle`] — so a background pass claims, closes and
/// toasts exactly where a verb did, one frame later.
///
/// The panel that ran the verb may have closed while the run was going. Its
/// line is then nobody's to write, and the run lands all the same: what
/// matters is that what happened can be undone.
pub fn land(s: &mut Session, l: Landed) {
    match l.run.task {
        Task::Here { .. } => landed_here(s, l),
        Task::Delete { .. } => landed_delete(s, l),
        Task::MakeDir { .. } => landed_dir(s, l),
        Task::Rename { .. } => landed_rename(s, l),
    }
}

/// `copy here` / `move here`, landed.
fn landed_here(s: &mut Session, l: Landed) {
    let Task::Here { verb, clip, dir } = &l.run.task else {
        return;
    };
    let (verb, here) = (*verb, basename(dir).to_string());
    // What the clipboard held when the verb ran. A move lets go of it at
    // the end — but only of the one it was carrying: somebody may have
    // pressed `copy` on something else while this was going, and that
    // clipboard is not this run's to empty.
    let held = clip.clone();
    let ran = Ran::of(&l.run);
    let missed = l.missed();
    let Landed {
        done,
        refused,
        stopped,
        dropped,
        ..
    } = l;
    if done.is_empty() {
        // Nothing could be done: the refusal is the word, and the
        // clipboard stands as it was.
        let msg = match (stopped, refused.len()) {
            // Whatever was waiting behind it went too, and this is the only
            // line there is to say so in: a run that did nothing still
            // answers for the ones it took with it.
            (true, _) => format!(
                "nothing {} into {here}{}",
                verb.done(),
                halted(true, dropped)
            ),
            (false, 1) => refused[0].clone(),
            (false, n) => format!("nothing to {} into {here} — {n} refused", verb.verb()),
        };
        ran.say(s, Some(msg.clone()));
        s.notify(msg, true);
        super::refresh(s, None);
        return;
    }
    let landed: Vec<String> = done.iter().map(|d| d.to.clone()).collect();
    let what = tally(&landed, missed);
    let tail = format!("{}{}", but(&refused), halted(stopped, dropped));
    let intent: Box<dyn Intent> = match verb {
        Op::Copy => Box::new(ops::Copied::new(done)),
        Op::Move => Box::new(ops::Moved::new(done)),
    };
    if let Some(why) = s.give_back(intent.as_ref()) {
        s.notify(why, true);
        super::refresh(s, None);
        return;
    }
    s.act_done(
        // No coalescing scope: a verb that wrote a disk is its own node,
        // however fast the next one follows. Two copies into one
        // directory are two things that happened, and cmd+z takes them
        // back one at a time.
        //
        // Nothing closes: a move empties the paths it came from, and a
        // panel elsewhere that was showing one of them keeps showing it
        // and says so — that is its own business, not this verb's.
        Action::new(verb.verb(), format!("{} {what} into {here}", verb.verb()))
            .claiming(vec![intent]),
    );
    ran.say(s, None);
    s.notify(
        format!("{} {what} into {here}{tail} — cmd+z undoes", verb.done()),
        false,
    );
    // A move consumes the clipboard; a copy keeps it, so the same set
    // can be laid down in another directory too. Only the one it carried,
    // though — a clipboard filled since this started is somebody else's
    // gesture, and it stands.
    if verb == Op::Move && FILES.clipboard() == held {
        FILES.clear();
    }
    super::refresh(s, None);
}

/// `delete`, landed.
fn landed_delete(s: &mut Session, l: Landed) {
    let Task::Delete { own, marked, .. } = &l.run.task else {
        return;
    };
    let marked = *marked;
    let ran = Ran::of(&l.run);
    // A panel that closed — or walked somewhere else — while the run was
    // going closes nothing now.
    let own = *own && ran.still(s).is_some();
    let missed = l.missed();
    let Landed {
        done,
        refused,
        stopped,
        dropped,
        ..
    } = l;
    if done.is_empty() {
        let msg = match (stopped, refused.len()) {
            (true, _) => format!("nothing deleted{}", halted(true, dropped)),
            (false, 1) => refused[0].clone(),
            (false, n) => format!("nothing deleted — {n} refused"),
        };
        ran.say(s, Some(msg.clone()));
        s.notify(msg, true);
        super::refresh(s, None);
        return;
    }
    let gone: Vec<String> = done.iter().map(|d| d.from.clone()).collect();
    let what = tally(&gone, missed);
    let tail = format!("{}{}", but(&refused), halted(stopped, dropped));
    let trashed: Box<dyn Intent> = Box::new(ops::Deleted::new(done));
    if let Some(why) = s.give_back(trashed.as_ref()) {
        // The lease turned over and the trash was given back, so the rows
        // are there again — and their marks must be too. The draws that
        // went by while the run was out took them off the table one at a
        // time, and nothing else is going to put them back: the node that
        // would have carried them was never recorded.
        if marked {
            ran.mark_again(s, &gone);
        }
        s.notify(why, true);
        super::refresh(s, None);
        return;
    }
    // The marks this delete consumed — the ones whose row went, never one
    // that stayed because its path refused. Taken only once the action is
    // certain, and undo puts exactly these back.
    let mut intents: Vec<Box<dyn Intent>> = vec![trashed];
    intents.extend(marked.then(|| ran.take_marks(s, &gone)).flatten());
    let closes = ran.by;
    s.act_done(
        Action::new("delete", format!("delete {what}"))
            .claiming(intents)
            // The layout half of the same node: the panel that was showing
            // this goes with it, and its joined chain goes with the panel.
            .moving(move |wm| {
                if own {
                    wm.close(closes);
                }
            }),
    );
    // What a verb took away is not there to be held any more.
    prune_clipboard(&s.world().clone());
    ran.say(s, None);
    s.notify(format!("{what} to the trash{tail} — cmd+z undoes"), false);
    super::refresh(s, None);
}

/// `new dir`, landed: the field closes when the directory is there, and
/// keeps what was typed when it is not.
fn landed_dir(s: &mut Session, l: Landed) {
    let Task::MakeDir { path } = &l.run.task else {
        return;
    };
    let ran = Ran::of(&l.run);
    let name = basename(path).to_string();
    let here = basename(parent(path).unwrap_or(HOME)).to_string();
    let Some(made) = l.done.first() else {
        // A name the directory already has, a directory that has gone, a
        // run that was stopped before it started: the panel's own line,
        // where the field is.
        let msg = l
            .refused
            .first()
            .cloned()
            .unwrap_or_else(|| format!("“{name}/” was not created{}", halted(true, l.dropped)));
        ran.say(s, Some(msg.clone()));
        s.notify(msg, true);
        return;
    };
    let intent: Box<dyn Intent> = Box::new(ops::MadeDir::made(made));
    if let Some(why) = s.give_back(intent.as_ref()) {
        s.notify(why, true);
        super::refresh(s, None);
        return;
    }
    s.act_done(
        Action::new("new dir", format!("new dir “{name}/” in {here}")).claiming(vec![intent]),
    );
    ran.with(s, |p| {
        let Some(d) = p.as_any().downcast_mut::<Dir>() else {
            return;
        };
        // The field stayed open while the run was out, so somebody may
        // have typed the next name into it — or submitted it, and be
        // waiting on a run of their own. It closes on the name it made and
        // on no other; the line goes either way, since a refusal from
        // before this went through is a refusal about nothing.
        if d.naming() == Some(name.as_str()) {
            d.set_naming(None);
        }
        d.set_status(None);
    });
    s.notify(format!("created “{name}/” in {here} — cmd+z undoes"), false);
    super::refresh(s, None);
}

/// The panel that ran the verb, as the run remembers it: a slot, and what
/// stood in it at the time.
///
/// Every reach back into the panel goes through here, because a slot is a
/// place and not a panel. While a long run is going, the listing that
/// started it may have walked somewhere else — a crumb and `go to` both
/// replace what a slot shows, in place, and neither closes anything — and
/// the panel standing there when the run lands is a stranger. It gets no
/// status line of ours, none of its marks taken, and above all no close.
struct Ran {
    by: SlotId,
    showing: PanelId,
}

impl Ran {
    fn of(run: &Run) -> Ran {
        Ran {
            by: run.by,
            showing: run.showing.clone(),
        }
    }

    /// The instance, if that slot is still showing what ran the verb.
    fn still(&self, s: &Session) -> Option<Instance> {
        let inst = s.panel(self.by)?;
        let same = inst.try_borrow().is_ok_and(|p| *p.id() == self.showing);
        same.then_some(inst)
    }

    /// Runs `f` on it, if it is still there and nobody else has it. Nothing
    /// here is worth a panic: a line that cannot be written is a line, and
    /// the node is recorded either way.
    fn with(&self, s: &Session, f: impl FnOnce(&mut dyn Panel)) {
        let Some(inst) = self.still(s) else {
            return;
        };
        let Ok(mut p) = inst.try_borrow_mut() else {
            return;
        };
        f(&mut **p);
    }

    /// The line under its header: what the run refused, or nothing where it
    /// went through.
    fn say(&self, s: &Session, line: Option<String>) {
        self.with(s, |p| {
            if let Some(d) = p.as_any().downcast_mut::<Dir>() {
                d.set_status(line);
            } else if let Some(c) = p.as_any().downcast_mut::<Card>() {
                c.set_status(line);
            }
        });
    }

    /// Puts marks back on the table the run took them from — what a
    /// reversal owes a panel when the node that would have carried them is
    /// never recorded.
    fn mark_again(&self, s: &Session, rows: &[String]) {
        self.with(s, |p| {
            let Some(d) = p.as_any().downcast_mut::<Dir>() else {
                return;
            };
            let dir = d.dir.clone();
            let back: Vec<String> = rows
                .iter()
                .filter(|g| parent(g) == Some(dir.as_str()))
                .map(|g| basename(g).to_string())
                .collect();
            d.list.marks_mut().extend(back);
        });
    }

    /// Takes the marks the run consumed off the table it ran on, and
    /// answers the intent that puts them back. `None` when it consumed
    /// none, and none where the panel has gone — there is nowhere left for
    /// a mark to be.
    ///
    /// What was taken is worked out from what *went*, not from what is
    /// still marked: the rows went one at a time while the panel was being
    /// drawn, and a row that has gone takes its mark with it on the draw
    /// after. The marks are removed here all the same — the ones the draw
    /// took are simply not there to remove — and undo puts back exactly the
    /// set the run consumed.
    fn take_marks(&self, s: &Session, gone: &[String]) -> Option<Box<dyn Intent>> {
        let inst = self.still(s)?;
        let mut p = inst.try_borrow_mut().ok()?;
        let d = p.as_any().downcast_mut::<Dir>()?;
        let dir = d.dir.clone();
        let taken: Vec<String> = gone
            .iter()
            .filter(|g| parent(g) == Some(dir.as_str()))
            .map(|g| basename(g).to_string())
            .collect();
        for n in &taken {
            d.list.marks_mut().remove(n);
        }
        drop(p);
        (!taken.is_empty()).then(|| {
            Box::new(Marked {
                panel: Rc::downgrade(&inst),
                keys: taken,
            }) as Box<dyn Intent>
        })
    }
}

/// `rename`, from a listing or from a card: one path under a new name, in
/// the directory it is already in.
///
/// The one verb that never takes a set — a name is a name, and two things
/// cannot both wear it — so it acts on the panel's own object and never on
/// the marks. `by` is the panel showing that object and `id_of` is what its
/// identity is for a path, so the layout half of the same node points the
/// slot at the new name: a panel is on the thing, not on the spelling.
/// Every *other* panel on the old name keeps it and says so, exactly as one
/// does after a delete.
///
/// Everything it can answer is answered here, where a person is waiting on
/// it; the move is a [run](super::super::run)'s, like every other write.
///
/// Nothing is copied and nothing is trashed: the disk verb is the move that
/// undo reverses, and the reversal is the move back.
pub(super) fn rename_path(
    s: &mut Session,
    by: SlotId,
    path: &str,
    name: &str,
    id_of: fn(&str) -> PanelId,
) -> Said {
    let was = basename(path).to_string();
    // The name it already has: the field has done its work, which was
    // nothing at all. No disk is asked and no node is made.
    //
    // Asked of the text exactly as typed, before the trim below, because
    // the field was seeded with this name: a file whose name really does
    // end in a space would otherwise be shortened by a submit that changed
    // nothing.
    if name == was {
        return Said::Went;
    }
    let name = name.trim();
    // Nothing typed: the field stands as it was, with nothing said.
    if name.is_empty() {
        return Said::Nothing;
    }
    // The same, once the trim has had it: a stray space either side of the
    // name it already has is not a rename either.
    if name == was {
        return Said::Went;
    }
    // A root is where the browser starts, and a name that is a path would
    // carry the thing off somewhere else under the word *rename*: both are
    // refused before any disk is asked.
    if is_root(path) {
        return refuse(s, format!("“{path}” is a root"));
    }
    if let Err(e) = ops::check_name(name) {
        return refuse(s, e);
    }
    if !s.writable() {
        s.notify("read-only — acquire the lease to write", true);
        return Said::Nothing;
    }
    let world = s.world().clone();
    let Some(dir) = parent(path) else {
        return refuse(s, format!("“{path}” is a root"));
    };
    let to = join(dir, name);
    // The disk as it is right now: nothing watches one, so this is the
    // first look since the field went up.
    if stat_in(&world, path).is_none() {
        return refuse(s, format!("“{was}” is no longer there"));
    }
    if stat_in(&world, &to).is_some() && !only_the_case(&world, path, &to) {
        return refuse(s, format!("“{name}” is already here"));
    }
    // Everything above is asked here, where a person is waiting on the
    // answer; the move itself is a run's, like every other write. One
    // `rename(2)` is hardly a freeze — but a path on a volume that has gone
    // to sleep is, and there is no second way to write a disk in this app.
    let task = Task::Rename {
        becomes: id_of(&to),
        path: path.to_string(),
        to,
    };
    FILES.start(s, task, by, id_of(path));
    Said::Doing
}

/// `rename`, landed.
fn landed_rename(s: &mut Session, l: Landed) {
    let Task::Rename { path, to, becomes } = &l.run.task else {
        return;
    };
    let (was, name) = (basename(path).to_string(), basename(to).to_string());
    let ran = Ran::of(&l.run);
    let Some(done) = l.done.first() else {
        let msg = l
            .refused
            .first()
            .cloned()
            .unwrap_or_else(|| format!("“{was}” was not renamed{}", halted(true, l.dropped)));
        ran.say(s, Some(msg.clone()));
        s.notify(msg, true);
        return;
    };
    let intent: Box<dyn Intent> = Box::new(ops::Renamed::new(done.clone()));
    if let Some(why) = s.give_back(intent.as_ref()) {
        s.notify(why, true);
        super::refresh(s, None);
        return;
    }
    // The layout half of the same node: a panel is on the thing and not on
    // the spelling, so the slot that ran this points at the new name — as
    // long as it is still the slot that ran it.
    let (by, id) = (ran.by, becomes.clone());
    let moves = ran.still(s).is_some();
    s.act_done(
        Action::new("rename", format!("rename “{was}” to “{name}”"))
            .claiming(vec![intent])
            .moving(move |wm| {
                if moves {
                    wm.replace(by, id);
                }
            }),
    );
    // A path that has just changed its name is not the path that was held.
    prune_clipboard(&s.world().clone());
    // The field needs no closing: the panel is on the thing and not on the
    // spelling, so the slot is pointed at the new name and the instance
    // that held the field — and whatever was typed into it while the run
    // was out — goes with the old one.
    s.notify(format!("renamed “{was}” to “{name}” — cmd+z undoes"), false);
    super::refresh(s, None);
}

/// Whether the name a rename asks for is no clash at all but the source
/// under another case — `notes.md` to `Notes.md` on a volume that does not
/// tell the two apart, which is what macOS formats by default. There the
/// destination *stats*, because it is the very file being renamed, and a
/// plain “is it there” would refuse the one rename nobody else can make.
///
/// Both halves are needed: the two spellings must differ only in case, and
/// the disk must call them one object. A name that is genuinely somebody
/// else's is a clash however it is spelt, and two hard links to one inode
/// are two names — moving one onto the other would take a name away.
fn only_the_case(world: &World, from: &str, to: &str) -> bool {
    if !from.eq_ignore_ascii_case(to) {
        return false;
    }
    match (id_in(world, from), id_in(world, to)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// A refusal said twice: once as the toast every verb gives, once as the
/// line the panel keeps under its header until the next verb.
fn refuse(s: &mut Session, why: String) -> Said {
    s.notify(why.clone(), true);
    Said::Refused(why)
}

/// What is held after a verb took a path away from where it was: whatever
/// is still on the disk. A clipboard with nothing left in it is no
/// clipboard.
fn prune_clipboard(world: &World) {
    let clip = FILES.clipboard();
    if clip.is_empty() {
        return;
    }
    let left: Vec<String> = clip
        .paths
        .into_iter()
        .filter(|p| stat_in(world, p).is_some())
        .collect();
    if left.is_empty() {
        FILES.clear();
    } else {
        FILES.set(clip.verb, left);
    }
}

/// The row's own wording where the set is one, as a batch has it:
/// *“notes.txt”*, *3 files*, *2 of 3 files*. `missed` is everything that
/// did not go — refused, or never reached because the run was stopped.
fn tally(done: &[String], missed: usize) -> String {
    match done {
        [one] if missed == 0 => format!("“{}”", basename(one)),
        many if missed == 0 => plural(many.len()),
        many => format!("{} of {}", many.len(), plural(many.len() + missed)),
    }
}

/// The tail of a toast: what would not go, or nothing at all.
fn but(refused: &[String]) -> String {
    if refused.is_empty() {
        String::new()
    } else {
        format!(" — {}", refused.join(", "))
    }
}

/// The other tail: that somebody pressed *cancel*, and what that cost the
/// runs waiting behind this one.
fn halted(stopped: bool, dropped: usize) -> String {
    match (stopped, dropped) {
        (false, _) => String::new(),
        (true, 0) => " — stopped".to_string(),
        (true, 1) => " — stopped, and one more never started".to_string(),
        (true, n) => format!(" — stopped, and {n} more never started"),
    }
}

/// The marks a batch verb consumed, put back by undo.
///
/// Marks are context, not data: they live in the panel's own memory and go
/// with the process. So the intent holds a weak handle to the very
/// instance it took them from, and does nothing at all once that panel has
/// closed — there is nowhere left for a mark to be.
pub struct Marked {
    panel: Weak<RefCell<Box<dyn Panel>>>,
    keys: Vec<String>,
}

impl Marked {
    fn edit(&self, f: impl FnOnce(&mut Dir)) {
        let Some(inst) = self.panel.upgrade() else {
            return;
        };
        let mut p = inst.borrow_mut();
        if let Some(d) = p.as_any().downcast_mut::<Dir>() {
            f(d);
        }
    }
}

impl Intent for Marked {
    fn describe(&self) -> String {
        format!("{} marked", plural(self.keys.len()))
    }

    fn reverse(&self, _w: &World) -> Result<(), String> {
        self.edit(|d| d.list.marks_mut().extend(self.keys.iter().cloned()));
        Ok(())
    }

    fn reapply(&self, _w: &World) -> Result<(), String> {
        self.edit(|d| {
            for k in &self.keys {
                d.list.marks_mut().remove(k);
            }
        });
        Ok(())
    }
}
