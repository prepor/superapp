//! Undoable local edits and deletes for offline fixtures.
//!
//! Live panels queue requests instead and let TDLib updates settle the store.
//! These intents reverse only the local fixture changes, not server actions.

use std::sync::{Arc, Mutex};

use kernel::effect::World;
use kernel::history::Intent;
use kernel::session::{Action, Session};

use super::model::{self, LineCopy, MsgId, PeerId};

/// Writes a new text over one of my lines, as one undoable action.
pub fn edit_line(
    s: &mut Session,
    chat: PeerId,
    msg: MsgId,
    before: &str,
    was_edited: bool,
    after: &str,
) {
    let (before, after) = (before.to_string(), after.to_string());
    let write = after.clone();
    let _ = s.act(
        Action::writing("edit", format!("edit “{}”", short(&before)), move |tx| {
            model::edit_tx(tx, chat, msg, &write, true)
        })
        .about(format!("chat:{chat}"))
        .claiming(vec![Box::new(Edited {
            chat,
            msg,
            before,
            was_edited,
            after,
        })]),
    );
}

/// Deletes lines, copied out whole first, so undo puts them back exactly
/// as they were — the same ids, the same dates, in the same places.
pub fn delete_lines(s: &mut Session, chat: PeerId, ids: Vec<MsgId>) {
    if ids.is_empty() {
        return;
    }
    let n = ids.len();
    let copies: Arc<Mutex<Vec<LineCopy>>> = Arc::default();
    let fill = Arc::clone(&copies);
    let gone = ids.clone();
    let _ = s.act(
        Action::writing(
            "delete",
            format!("delete {n} line{}", if n == 1 { "" } else { "s" }),
            move |tx| {
                let rows = model::copy_lines_tx(tx, chat, &gone)?;
                *fill.lock().expect("the copies of the deleted lines") = rows;
                model::delete_lines_tx(tx, chat, &gone).map(|_| ())
            },
        )
        .about(format!("chat:{chat}"))
        .claiming(vec![Box::new(Deleted { chat, ids, copies })]),
    );
}

/// The first words of a line, for a label.
fn short(text: &str) -> String {
    let line = model::one_line(text);
    let mut out: String = line.chars().take(40).collect();
    if out.len() < line.len() {
        out.push('…');
    }
    out
}

/// An edit of one line, and how it is given back: the old text and the old
/// flag, since a line edited before stays edited.
struct Edited {
    chat: PeerId,
    msg: MsgId,
    before: String,
    was_edited: bool,
    after: String,
}

impl Intent for Edited {
    fn describe(&self) -> String {
        format!("chat:{} line {} edited", self.chat, self.msg)
    }

    fn reverse(&self, w: &World) -> Result<(), String> {
        let (chat, msg, text, was) = (self.chat, self.msg, self.before.clone(), self.was_edited);
        w.store()
            .write(move |c| model::edit_tx(c, chat, msg, &text, was))
            .map_err(|e| e.to_string())
    }

    fn reapply(&self, w: &World) -> Result<(), String> {
        let (chat, msg, text) = (self.chat, self.msg, self.after.clone());
        w.store()
            .write(move |c| model::edit_tx(c, chat, msg, &text, true))
            .map_err(|e| e.to_string())
    }
}

/// Lines deleted, and their copies: the write fills them as it deletes, and
/// undo puts them back.
struct Deleted {
    chat: PeerId,
    ids: Vec<MsgId>,
    copies: Arc<Mutex<Vec<LineCopy>>>,
}

impl Intent for Deleted {
    fn describe(&self) -> String {
        let n = self.ids.len();
        format!("chat:{} {n} line{} deleted", self.chat, if n == 1 { "" } else { "s" })
    }

    fn reverse(&self, w: &World) -> Result<(), String> {
        let rows = self.copies.lock().expect("the copies of the deleted lines").clone();
        w.store()
            .write(move |c| model::restore_lines_tx(c, &rows))
            .map_err(|e| e.to_string())
    }

    fn reapply(&self, w: &World) -> Result<(), String> {
        let (chat, ids) = (self.chat, self.ids.clone());
        w.store()
            .write(move |c| model::delete_lines_tx(c, chat, &ids).map(|_| ()))
            .map_err(|e| e.to_string())
    }
}
