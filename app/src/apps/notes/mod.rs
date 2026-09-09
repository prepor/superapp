//! Notes live in the store. File edits live in drafts until an explicit save.

use kernel::app::{App, Root, Schema, Step};
use kernel::panel::{PanelId, PanelKind};
use std::any::Any;

mod file_text;
mod io;
mod markdown;
mod model;
mod ops;
mod panels;
#[cfg(test)]
mod tests;
mod tools;
mod ui;
mod widgets;

pub use ui::UI;
pub struct Notes;
pub static NOTES: Notes = Notes;

impl Notes {
    /// The file browser offers this through the app registry.
    pub fn editor(&self, path: &str) -> PanelId {
        panels::Editor::file(path)
    }
}

pub static SCHEMA: Schema = Schema {
    app: "notes",
    steps: &[Step::Sql(
        "
        CREATE TABLE notes_note (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            title TEXT NOT NULL DEFAULT 'Untitled',
            body TEXT NOT NULL DEFAULT '',
            created REAL NOT NULL,
            modified REAL NOT NULL,
            deleted INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX notes_order ON notes_note(deleted, modified DESC, id DESC);
        CREATE TABLE notes_draft (
            path TEXT PRIMARY KEY NOT NULL,
            original TEXT NOT NULL,
            body TEXT NOT NULL,
            modified REAL NOT NULL
        );
    ",
    )],
};

impl App for Notes {
    fn flush(
        &self,
        db: std::sync::Arc<kernel::store::Db>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        Box::pin(io::flush(db))
    }

    fn id(&self) -> &'static str {
        "notes"
    }
    fn kinds(&self) -> &'static [&'static dyn PanelKind] {
        panels::KINDS
    }
    fn schema(&self) -> Option<&'static Schema> {
        Some(&SCHEMA)
    }
    fn roots(&self) -> Vec<Root> {
        vec![Root::new(
            panels::NoteList::id(),
            "notes",
            "text editor markdown writing",
        )]
    }
    fn describe(&self) -> Option<&'static str> {
        Some("notes_note: plain Markdown notes, independent of files. The first nonempty line supplies the title; deleted=1 retains a note for undo. Discover notes and drafts with sql.query; prefer notes.create/read/update and notes.read_draft/create_draft/update_draft for their contents. Read every body page at the same revision before replacing text. These tools derive titles, retain file originals and support undo. File tool bodies use LF without a BOM; the original BOM and individual line endings are preserved internally. notes_draft contains unsaved file edits keyed by path. Only the editor's explicit Save writes a draft to disk. Never update a draft's original to bypass a conflict.")
    }
    fn tools(&self) -> Vec<kernel::tool::Tool> {
        tools::all()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
