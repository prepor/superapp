use super::model;
use kernel::caps::{basename, display_path, real_path};
use kernel::effect::World;
use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::richtable::{ListState, SqlSource};
use kernel::session::Session;
use std::any::Any;
use std::rc::Rc;

pub static KINDS: &[&dyn PanelKind] = &[&ListKind, &EditorKind];

pub struct NoteList {
    id: PanelId,
    slot: SlotId,
    pub list: ListState<&'static SqlSource<model::Note, i64>>,
    pub filter: String,
}
impl NoteList {
    pub const TAG: Tag = Tag("notes");
    pub fn id() -> PanelId {
        PanelId::bare(Self::TAG)
    }
}
impl Panel for NoteList {
    fn id(&self) -> &PanelId {
        &self.id
    }
    fn title(&self) -> String {
        "notes".into()
    }
    fn about(&self) -> String {
        "Notes stored independently of the filesystem, most recently edited first. Filter by any text, open a note to write, or create a new one. Deletion is undoable.".into()
    }
    fn wish(&self, _: usize) -> (u32, u32) {
        (4, 6)
    }
    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }
    fn persist(&self) -> PanelId {
        PanelId::new(Self::TAG, [self.list.table().filter()])
    }
    fn verbs(&self) -> Vec<Verb> {
        let mut verbs = vec![Verb::run("notes.new", "new note", Some('n'))];
        if self.list.cursor_key().is_some() || !self.list.marks().keys().is_empty() {
            verbs.push(Verb::run("notes.delete", "delete", Some('d')));
        }
        verbs
    }
    fn run(&mut self, verb: &str, s: &mut Session) {
        match verb {
            "notes.new" => {
                if let Some(id) = model::create(s) {
                    s.nav_within(Nav::Open {
                        from: self.slot,
                        id: Editor::note(id),
                        fresh: false,
                    });
                }
            }
            "notes.delete" => {
                let marked = self.list.marks().keys();
                let ids = if marked.is_empty() {
                    self.list.cursor_key().copied().into_iter().collect()
                } else {
                    marked
                };
                if model::delete(s, ids) {
                    self.list.clear_marks();
                }
            }
            _ => {}
        }
    }
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}
struct ListKind;
impl PanelKind for ListKind {
    fn tag(&self) -> Tag {
        NoteList::TAG
    }
    fn open(&self, id: &PanelId, _: &mut Opening<'_>) -> Box<dyn Panel> {
        let filter = id.arg(0).unwrap_or("").to_string();
        let mut list = ListState::new(&model::NOTES, 50);
        list.set_filter(&filter);
        Box::new(NoteList {
            id: id.clone(),
            slot: 0,
            list,
            filter,
        })
    }
}

#[derive(Clone)]
enum Source {
    Note(i64),
    File(String),
    Invalid,
}

pub struct Editor {
    id: PanelId,
    world: Rc<World>,
    source: Source,
    pub text: String,
    original: String,
    pub error: String,
    pub available: bool,
    /// Incremented only when text comes from outside this widget.
    pub revision: u64,
    failed_autosave: bool,
    dirty: bool,
    observed: Vec<u64>,
}
impl Editor {
    pub const TAG: Tag = Tag("editor");
    pub fn note(id: i64) -> PanelId {
        PanelId::new(Self::TAG, ["note".into(), id.to_string()])
    }
    pub fn file(path: &str) -> PanelId {
        PanelId::new(Self::TAG, ["file".into(), display_path(&real_path(path))])
    }
    pub fn is_file(&self) -> bool {
        matches!(self.source, Source::File(_))
    }
    pub fn path(&self) -> Option<&str> {
        if let Source::File(p) = &self.source {
            Some(p)
        } else {
            None
        }
    }
    fn bytes_text(&self) -> String {
        if !self.is_file() {
            return self.text.clone();
        }
        let mut text = self.text.clone();
        if self.original.contains("\r\n") {
            text = text.replace('\n', "\r\n");
        }
        if self.original.starts_with('\u{feff}') {
            text.insert(0, '\u{feff}');
        }
        text
    }
    pub fn dirty(&self) -> bool {
        self.dirty
    }
    pub fn status(&self) -> &str {
        if !self.available {
            "unavailable"
        } else if self.failed_autosave {
            "not saved — retry by editing"
        } else if self.dirty() {
            "draft saved · save to update file"
        } else if self.is_file() {
            "saved to file"
        } else {
            "saved automatically"
        }
    }
    pub fn edited(&mut self, text: String) {
        if !self.available || (text == self.text && !self.failed_autosave) {
            return;
        }
        self.text = text;
        self.dirty = self.is_file() && self.bytes_text() != self.original;
        let result = match &self.source {
            Source::Note(id) => {
                model::edit(self.world.store(), *id, self.text.clone(), self.world.now())
            }
            Source::File(path) => model::save_draft(
                self.world.store(),
                path.clone(),
                model::Draft {
                    original: self.original.clone(),
                    body: self.bytes_text(),
                },
                self.world.now(),
            ),
            Source::Invalid => return,
        };
        self.failed_autosave = result.is_err();
        self.error = result
            .err()
            .map(|e| format!("could not autosave: {e}"))
            .unwrap_or_default();
        self.observed = self.world.store().revision(&["notes_note", "notes_draft"]);
    }
    pub fn observe(&mut self) {
        // Register the source on every draw for panel context. Cached Rc
        // results keep idle frames from cloning the document's strings.
        match &self.source {
            Source::Note(id) => {
                model::note_source(self.world.store(), *id);
            }
            Source::File(path) => {
                model::draft_source(self.world.store(), path);
            }
            Source::Invalid => {}
        }
        if self.failed_autosave {
            return;
        }
        let observed = self.world.store().revision(&["notes_note", "notes_draft"]);
        if observed == self.observed {
            return;
        }
        self.observed = observed;
        if let Source::Note(id) = self.source {
            if let Some(body) = model::body(self.world.store(), id) {
                self.available = true;
                self.error.clear();
                if self.text != body {
                    self.text = body;
                    self.revision += 1;
                }
            } else {
                self.available = false;
                self.error = "this note was deleted — undo restores it".into();
            }
        } else if let Source::File(path) = &self.source {
            if let Some(draft) = model::draft(self.world.store(), path) {
                let text = editable(&draft.body);
                if text != self.text || draft.original != self.original {
                    self.text = text;
                    self.original = draft.original;
                    self.dirty = draft.body != self.original;
                    self.revision += 1;
                }
            } else if self.dirty {
                // Another open editor saved or reverted the shared draft.
                if let Ok(original) = self.world.run(&model::ReadFile(path.clone())) {
                    self.text = editable(&original);
                    self.original = original;
                    self.dirty = false;
                    self.revision += 1;
                }
            }
        }
    }
    fn save(&mut self, s: &mut Session) {
        let Source::File(path) = &self.source else {
            return;
        };
        if !self.available || !self.dirty() {
            return;
        }
        let path = path.clone();
        let body = self.bytes_text();
        // Persist even when the previous autosave failed, before touching disk.
        let saved_draft = model::save_draft(
            self.world.store(),
            path.clone(),
            model::Draft {
                original: self.original.clone(),
                body: body.clone(),
            },
            self.world.now(),
        );
        self.failed_autosave = saved_draft.is_err();
        let result = saved_draft.and_then(|_| {
            self.world.run(&model::SaveFile {
                path: path.clone(),
                original: self.original.clone(),
                body: body.clone(),
            })
        });
        match result {
            Ok(()) => {
                self.original = body.clone();
                self.dirty = false;
                let clean = model::save_draft(
                    self.world.store(),
                    path,
                    model::Draft {
                        original: body.clone(),
                        body,
                    },
                    self.world.now(),
                );
                self.failed_autosave = clean.is_err();
                self.error = clean.err().unwrap_or_default();
                self.observed = self.world.store().revision(&["notes_note", "notes_draft"]);
                s.notify("file saved", false);
            }
            Err(e) => {
                self.error = e.clone();
                s.notify(e, true);
            }
        }
        s.redraw();
    }
}
fn editable(text: &str) -> String {
    text.strip_prefix('\u{feff}')
        .unwrap_or(text)
        .replace("\r\n", "\n")
}
impl Panel for Editor {
    fn id(&self) -> &PanelId {
        &self.id
    }
    fn title(&self) -> String {
        match &self.source {
            Source::File(path) => {
                format!("{}{}", basename(path), if self.dirty() { " *" } else { "" })
            }
            _ => model::title(&self.text),
        }
    }
    fn about(&self) -> String {
        if self.is_file() { "A plain text file editor with Markdown source highlighting. Every edit is kept as a database draft; only save writes the file, after checking the original. Closing retains the draft." }
        else { "An autosaved plain Markdown note. Formatting marks remain editable source text. Text undo belongs to the editor; there is no save action." }.into()
    }
    fn context_text_columns(&self) -> &'static [&'static str] {
        &["body"]
    }
    fn wish(&self, _: usize) -> (u32, u32) {
        (6, 6)
    }
    fn verbs(&self) -> Vec<Verb> {
        if self.is_file() && self.available {
            vec![Verb::run("notes.save", "save", Some('s'))]
        } else {
            vec![]
        }
    }
    fn run(&mut self, verb: &str, s: &mut Session) {
        if verb == "notes.save" {
            self.save(s);
        }
    }
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}
struct EditorKind;
impl PanelKind for EditorKind {
    fn tag(&self) -> Tag {
        Editor::TAG
    }
    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        let world = cx.session().world().clone();
        let source = match (id.arg(0), id.arg(1)) {
            (Some("note"), Some(id)) => id.parse().map(Source::Note).unwrap_or(Source::Invalid),
            (Some("file"), Some(path)) if path.starts_with('/') || path.starts_with("~/") => {
                Source::File(path.into())
            }
            _ => Source::Invalid,
        };
        let loaded = match &source {
            Source::Note(id) => model::body(world.store(), *id)
                .map(|body| model::Draft {
                    original: body.clone(),
                    body,
                })
                .ok_or_else(|| "this note is unavailable".into()),
            Source::File(path) => model::draft(world.store(), path)
                .map(Ok)
                .unwrap_or_else(|| {
                    world
                        .run(&model::ReadFile(path.clone()))
                        .map(|body| model::Draft {
                            original: body.clone(),
                            body,
                        })
                }),
            Source::Invalid => Err("invalid editor address".into()),
        };
        let available = loaded.is_ok();
        let error = loaded.as_ref().err().cloned().unwrap_or_default();
        let draft = loaded.unwrap_or(model::Draft {
            original: String::new(),
            body: String::new(),
        });
        let text = if matches!(source, Source::File(_)) {
            editable(&draft.body)
        } else {
            draft.body
        };
        let dirty = matches!(source, Source::File(_)) && text != editable(&draft.original);
        let observed = world.store().revision(&["notes_note", "notes_draft"]);
        Box::new(Editor {
            id: id.clone(),
            world,
            source,
            text,
            original: draft.original,
            error,
            available,
            revision: 0,
            failed_autosave: false,
            dirty,
            observed,
        })
    }
}
