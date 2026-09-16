//! One file's card: what it is, where its bytes are, the pages that name
//! it, and the shared viewer over it.
//!
//! Where the bytes are is read off the prototype's `kb_cache`; **fetch**
//! moves a missing file to *fetching…* and, a moment of the world's clock
//! later, to *cached* — the shape phase 3's worker gives it, with nothing
//! behind it yet.

use std::any::Any;
use std::rc::Rc;

use kernel::caps::{fmt_size, FileKind as Kind};
use kernel::layout::SlotId;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::session::Session;
use kernel::store::Store;
use kernel::time::fmt_date_long;

use super::super::model::{self, Where};
use super::super::seed;
use super::ask;
use crate::shell::widgets::viewer::{Controller, Measure, Preview};

pub struct File {
    id: PanelId,
    slot: SlotId,
    path: String,
    store: Rc<Store>,
    viewer: Controller,
    /// When **fetch** was pressed, on the world's clock; the bytes land a
    /// second later.
    fetching_since: Option<f64>,
}

impl File {
    pub const TAG: Tag = Tag("kb-file");

    #[must_use]
    pub fn id(path: &str) -> PanelId {
        PanelId::new(Self::TAG, [path])
    }

    #[must_use]
    pub fn slot(&self) -> SlotId {
        self.slot
    }

    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    #[must_use]
    pub fn file(&self) -> Option<model::File> {
        model::file(&self.store, &self.path)
    }

    #[must_use]
    pub fn name(&self) -> String {
        self.path.rsplit('/').next().unwrap_or(&self.path).to_string()
    }

    #[must_use]
    pub fn kind(&self) -> Kind {
        match self.file() {
            Some(f) => Kind::of_metadata(&self.path, &f.mime),
            None => Kind::Other,
        }
    }

    #[must_use]
    pub fn kind_line(&self) -> (String, String) {
        match self.file() {
            Some(f) => (self.kind().word().to_string(), fmt_size(f.size)),
            None => ("gone".into(), String::new()),
        }
    }

    #[must_use]
    pub fn when(&self) -> String {
        match self.file() {
            Some(f) => format!("modified {}", fmt_date_long(f.updated)),
            None => "no such file in the kb".into(),
        }
    }

    #[must_use]
    pub fn where_is(&self) -> Where {
        match self.file() {
            Some(f) => model::where_is(&self.store, &f.hash),
            None => Where::Missing,
        }
    }

    /// The pages that name this file.
    #[must_use]
    pub fn named_by(&self) -> Rc<Vec<(String, String)>> {
        model::named_by(&self.store, &self.path)
    }

    #[must_use]
    pub fn viewer(&self) -> Controller {
        self.viewer.clone()
    }

    /// What the viewer is handed: the bytes where they are here, nothing
    /// where they are not.
    #[must_use]
    pub fn preview(&self) -> Preview {
        let Some(f) = self.file() else {
            return Preview::None;
        };
        if !self.where_is().here() {
            return Preview::None;
        }
        match seed::bytes_of(&f.hash) {
            Some(bytes) => Preview::Bytes {
                bytes: bytes.into(),
                name: self.name(),
                kind: self.kind(),
                size: f.size,
            },
            None if !f.text.is_empty() => Preview::Text(f.text),
            None => Preview::None,
        }
    }

    /// Whether the card should be written again: the bytes landed.
    pub fn poll(&mut self, s: &mut Session) -> bool {
        let Some(since) = self.fetching_since else {
            return false;
        };
        if s.now() - since < 1.0 {
            return false;
        }
        self.fetching_since = None;
        if let Some(f) = self.file() {
            model::set_where(&self.store, &f.hash, Where::Cached);
        }
        s.redraw();
        true
    }
}

impl Panel for File {
    fn id(&self) -> &PanelId {
        &self.id
    }
    fn title(&self) -> String {
        self.name()
    }
    fn about(&self) -> String {
        format!(
            "One file of the knowledge base, {}: its kind and size, where its bytes are — in the \
             bucket and cached here, on their way, waiting in the outbox for a bucket, or not on \
             this device — and the pages that name it. The viewer shows it where the bytes are \
             here.",
            self.path
        )
    }
    fn wish(&self, cols: usize) -> (u32, u32) {
        let measure = self.viewer.measure();
        if measure == Measure::Empty && self.kind() == Kind::Pdf {
            Measure::Pdf(595, 842).wish(cols, 8)
        } else {
            measure.wish(cols, 8)
        }
    }
    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }
    fn verbs(&self) -> Vec<Verb> {
        let mut v = vec![Verb::run("kb.ask", "ask", Some('a')), Verb::run("kb.open", "open", Some('o'))];
        if self.file().is_some() && !self.where_is().here() {
            v.push(Verb::run("kb.fetch", "fetch", Some('f')));
        } else {
            v.extend(self.viewer.verbs());
        }
        v
    }
    fn run(&mut self, verb: &str, s: &mut Session) {
        if self.viewer.run(verb) {
            s.redraw();
            return;
        }
        match verb {
            "kb.ask" => ask(s, self.slot),
            "kb.open" => s.notify("open hands the file to the system once the bucket is real", false),
            "kb.fetch" => {
                if let Some(f) = self.file() {
                    model::set_where(&self.store, &f.hash, Where::Fetching);
                    self.fetching_since = Some(s.now());
                    s.redraw();
                }
            }
            _ => {}
        }
    }
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

pub struct FileKind;
impl PanelKind for FileKind {
    fn tag(&self) -> Tag {
        File::TAG
    }
    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        Box::new(File {
            id: id.clone(),
            slot: 0,
            path: id.arg(0).unwrap_or("").to_string(),
            store: cx.session().store().clone(),
            viewer: Controller::default(),
            fetching_since: None,
        })
    }
}
