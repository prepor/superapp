//! Notes live in the store. File edits live in drafts until an explicit save.

use kernel::app::{App, Root, Schema, Step};
use kernel::panel::{PanelId, PanelKind};
use std::any::Any;

mod markdown;
mod model;
mod panels;
#[cfg(test)]
mod tests;
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
        Some("notes_note: plain Markdown notes, independent of files. The first nonempty line supplies the title. Typing autosaves body and modified; deleted=1 retains a note for undo. notes_draft: unsaved file edits keyed by path, with the original bytes as UTF-8 text for conflict detection. Only the editor's explicit save writes a file. Never update a draft's original to bypass a conflict.")
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
