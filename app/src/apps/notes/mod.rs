//! Notes live in the store. File edits live in drafts until an explicit save.

use kernel::app::{App, Root, Schema, Step};
use kernel::panel::{PanelId, PanelKind};
use kernel::sync::Replicated;
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
    steps: &[Step::Sql(V1), Step::Run(add_uid)],
};

/// The first rung, as every store that has one already climbed it.
const V1: &str = "
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
    ";

/// A note is the same note on every device, and `uid` is what says so. The
/// local `id` stays where it is — in panel arguments, in drafts, in every
/// tool call.
pub static REPLICATED: &[Replicated] = &[Replicated {
    table: "notes_note",
    key: &["uid"],
    columns: &["title", "body", "created", "modified", "deleted"],
}];

/// Notes gained a `uid`: sixteen random bytes in hex, defaulted by the
/// column so that no insert site changes. `ALTER TABLE` cannot add a column
/// with an expression default, so the table is rebuilt.
///
/// Rows that were already here derive theirs from the row itself rather
/// than from chance — two devices that once shared one store produce the
/// same uid for the same note, so pairing them merges the two copies of
/// that note instead of keeping both.
///
/// The rebuild is one transaction of its own, which a ladder may have
/// because it is climbed at open and not inside somebody else's write.
fn add_uid(c: &rusqlite::Connection) -> rusqlite::Result<()> {
    let done: i64 = c.query_row(
        "SELECT count(*) FROM pragma_table_info('notes_note') WHERE name = 'uid'",
        [],
        |r| r.get(0),
    )?;
    if done > 0 {
        return Ok(());
    }
    c.execute_batch(
        "BEGIN IMMEDIATE;
         CREATE TABLE notes_note_rebuilt (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             uid TEXT NOT NULL UNIQUE DEFAULT (lower(hex(randomblob(16)))),
             title TEXT NOT NULL DEFAULT 'Untitled',
             body TEXT NOT NULL DEFAULT '',
             created REAL NOT NULL DEFAULT 0,
             modified REAL NOT NULL DEFAULT 0,
             deleted INTEGER NOT NULL DEFAULT 0
         );
         INSERT INTO notes_note_rebuilt(id, uid, title, body, created, modified, deleted)
              SELECT id, printf('%016x%016x', id, CAST(created * 1000 AS INTEGER)),
                     title, body, created, modified, deleted FROM notes_note;
         DROP TABLE notes_note;
         ALTER TABLE notes_note_rebuilt RENAME TO notes_note;
         CREATE INDEX notes_order ON notes_note(deleted, modified DESC, id DESC);
         COMMIT;",
    )
}

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
    fn replicated(&self) -> &'static [Replicated] {
        REPLICATED
    }
    fn roots(&self) -> Vec<Root> {
        vec![Root::new(
            panels::NoteList::id(),
            "notes",
            "text editor markdown writing",
        )]
    }
    fn describe(&self) -> Option<&'static str> {
        Some("notes_note: plain Markdown notes, independent of files. The first nonempty line supplies the title; deleted=1 retains a note for undo. uid names the note on every one of this person's devices and is never written by hand. Discover notes and drafts with sql.query; prefer notes.create/read/update and notes.read_draft/create_draft/update_draft for their contents. Read every body page at the same revision before replacing text. These tools derive titles, retain file originals and support undo. File tool bodies use LF without a BOM; the original BOM and individual line endings are preserved internally. notes_draft contains unsaved file edits keyed by path. Only the editor's explicit Save writes a draft to disk. Never update a draft's original to bypass a conflict.")
    }
    fn tools(&self) -> Vec<kernel::tool::Tool> {
        tools::all()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
