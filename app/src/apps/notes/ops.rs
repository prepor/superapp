//! Undoable agent edits use the same writes as autosave, checked in the
//! writer's transaction so a stale response cannot overwrite newer typing.

use super::model::{self, NoteText, StoredDraft};
use kernel::effect::World;
use kernel::history::Intent;
use kernel::tool::Prepared;
use ring::digest::{digest, SHA256};
use rusqlite::Connection;
use serde::Serialize;

pub const CHANGED: &str = "the text changed; read it again and use the new revision";

/// Include the identity and complete stored state, including file originals
/// and timestamps. A token for another note/path cannot authorize this edit.
pub fn revision(state: &impl Serialize) -> String {
    let bytes = serde_json::to_vec(state).expect("serializable note state");
    digest(&SHA256, &bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[derive(Clone)]
pub enum Edit {
    Note {
        id: i64,
        before: NoteText,
        after: NoteText,
    },
    Draft {
        path: String,
        before: Option<StoredDraft>,
        after: Option<StoredDraft>,
    },
}
impl Edit {
    fn check(&self, c: &Connection, forward: bool) -> Result<(), String> {
        let matches = match self {
            Self::Note { id, before, after } => {
                model::note_text(c, *id)
                    .map_err(|e| e.to_string())?
                    .as_ref()
                    == Some(if forward { before } else { after })
            }
            Self::Draft {
                path,
                before,
                after,
            } => {
                model::stored_draft(c, path)
                    .map_err(|e| e.to_string())?
                    .as_ref()
                    == if forward {
                        before.as_ref()
                    } else {
                        after.as_ref()
                    }
            }
        };
        if matches {
            Ok(())
        } else {
            Err(CHANGED.into())
        }
    }

    fn apply(&self, c: &Connection, forward: bool) -> Result<(), String> {
        self.check(c, forward)?;
        match self {
            Self::Note { id, before, after } => {
                model::put_note(c, *id, if forward { after } else { before })
            }
            Self::Draft {
                path,
                before,
                after,
            } => {
                let state = if forward { after } else { before };
                model::put_draft(
                    c,
                    path,
                    state.as_ref().map(|d| &d.text),
                    state.as_ref().map_or(0., |d| d.modified),
                )
            }
        }
        .map_err(|e| e.to_string())
    }

    pub fn prepared(self, reply: serde_json::Value) -> Prepared {
        let kind = match &self {
            Self::Note { .. } => "notes.update",
            Self::Draft { before: None, .. } => "notes.create_draft",
            Self::Draft { .. } => "notes.update_draft",
        };
        let edit = self.clone();
        Prepared::Edit(kernel::session::Edit::writing(kind, self.describe(), move |c| {
            edit.apply(c, true).map_err(sql_error)?;
            Ok(reply)
        }).claiming(vec![Box::new(self)]))
    }

    fn restore(&self, world: &World, forward: bool) -> Result<(), String> {
        let edit = self.clone();
        world
            .store()
            .write(move |c| edit.apply(c, forward).map_err(sql_error))
            .map_err(|e| e.to_string())
    }
}

fn sql_error(why: String) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
        Some(why),
    )
}

impl Intent for Edit {
    fn describe(&self) -> String {
        match self {
            Self::Note { after, .. } => format!("edit note {}", after.title),
            Self::Draft { path, .. } => format!("edit draft {path}"),
        }
    }
    fn blocked(&self, world: &World) -> Option<String> {
        self.check(world.store().conn(), false).err()
    }
    fn reverse(&self, world: &World) -> Result<(), String> {
        self.restore(world, false)
    }
    fn reapply(&self, world: &World) -> Result<(), String> {
        self.restore(world, true)
    }
}
