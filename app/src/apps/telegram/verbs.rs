//! Undoable topic preferences and offline message actions.
//!
//! Live message changes queue requests and let TDLib updates settle the store.
//! Topic visibility is this app's preference in both live and offline accounts.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use kernel::effect::World;
use kernel::history::Intent;
use kernel::session::{Action, Session};

use super::model::{self, LineCopy, MsgId, PeerId};
use super::topics;

/// Each gesture owns only the visibility flags it changes, so undo restores
/// a mixed selection without changing topic metadata or later discoveries.
pub fn select_topics(s: &mut Session, chat: PeerId, ids: Vec<i64>, selected: bool) {
    let wanted: HashSet<_> = ids.into_iter().collect();
    // Every retained row had !selected; unchanged rows must not be reversed.
    let ids: Vec<_> = topics::list(s.store(), chat)
        .iter()
        .filter(|t| wanted.contains(&t.id) && t.selected != selected)
        .map(|t| t.id)
        .collect();
    if ids.is_empty() {
        return;
    }
    let n = ids.len();
    let word = if selected { "show" } else { "hide" };
    let write = ids.clone();
    let _ = s.act(
        Action::writing(
            "topic selection",
            format!("{word} {n} topic{}", if n == 1 { "" } else { "s" }),
            move |tx| topics::select_tx(tx, chat, &write, selected),
        )
        .claiming(vec![Box::new(TopicSelection { chat, ids, selected })]),
    );
}

struct TopicSelection {
    chat: PeerId,
    ids: Vec<i64>,
    selected: bool,
}

impl Intent for TopicSelection {
    fn describe(&self) -> String {
        format!("chat:{} topic selection", self.chat)
    }

    fn reverse(&self, w: &World) -> Result<(), String> {
        let (chat, ids, selected) = (self.chat, self.ids.clone(), !self.selected);
        w.store()
            .write(move |c| topics::select_tx(c, chat, &ids, selected))
            .map_err(|e| e.to_string())
    }

    fn reapply(&self, w: &World) -> Result<(), String> {
        let (chat, ids, selected) = (self.chat, self.ids.clone(), self.selected);
        w.store()
            .write(move |c| topics::select_tx(c, chat, &ids, selected))
            .map_err(|e| e.to_string())
    }
}

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
    let before_entities = model::history(s.store(), chat).iter()
        .find(|m| m.id == msg).map(|m| m.entities.clone()).unwrap_or_default();
    let write = after.clone();
    let _ = s.act(
        Action::writing("edit", format!("edit “{}”", short(&before)), move |tx| {
            model::edit_tx(tx, chat, msg, &write, true, None)
        })
        .about(format!("chat:{chat}"))
        .claiming(vec![Box::new(Edited {
            chat,
            msg,
            before,
            before_entities,
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
    before_entities: Option<Vec<super::text::Entity>>,
    was_edited: bool,
    after: String,
}

impl Intent for Edited {
    fn describe(&self) -> String {
        format!("chat:{} line {} edited", self.chat, self.msg)
    }

    fn reverse(&self, w: &World) -> Result<(), String> {
        let (chat, msg, text, was) = (self.chat, self.msg, self.before.clone(), self.was_edited);
        let entities = self.before_entities.clone();
        w.store()
            .write(move |c| model::edit_tx(c, chat, msg, &text, was, entities.as_deref()))
            .map_err(|e| e.to_string())
    }

    fn reapply(&self, w: &World) -> Result<(), String> {
        let (chat, msg, text) = (self.chat, self.msg, self.after.clone());
        w.store()
            .write(move |c| model::edit_tx(c, chat, msg, &text, true, None))
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

/// Offline reactions obey the same history contract without using a transport.
pub(super) fn react_demo(s: &mut Session, chat: PeerId, msg: MsgId, emoji: &str) -> bool {
    let rt = super::runtime::of(s.store());
    if rt.demo_reacted(chat, msg, emoji) { return true; }
    let chosen = emoji.to_string();
    if s.act(Action::writing("react", format!("react {emoji}"), move |tx| {
        demo_reaction(tx, chat, msg, &chosen, 1)
    }).claiming(vec![Box::new(DemoReaction { chat, msg, emoji: emoji.into() })])).is_none() {
        return false;
    }
    rt.remember_demo_reaction(chat, msg, emoji);
    true
}

struct DemoReaction { chat: PeerId, msg: MsgId, emoji: String }

impl Intent for DemoReaction {
    fn describe(&self) -> String { format!("react {}", self.emoji) }
    fn reverse(&self, w: &World) -> Result<(), String> { self.change(w, -1) }
    fn reapply(&self, w: &World) -> Result<(), String> { self.change(w, 1) }
}

impl DemoReaction {
    fn change(&self, w: &World, delta: i64) -> Result<(), String> {
        let (chat, msg, emoji) = (self.chat, self.msg, self.emoji.clone());
        w.store().write(move |c| demo_reaction(c, chat, msg, &emoji, delta)).map_err(|e| e.to_string())?;
        let rt = super::runtime::of(w.store());
        if delta > 0 { rt.remember_demo_reaction(chat, msg, &self.emoji); }
        else { rt.forget_demo_reaction(chat, msg, &self.emoji); }
        Ok(())
    }
}

/// A fixture updates the same count line the real projection renders.
/// Only explicitly offline worlds reach this; a missing worker never does.
fn demo_reaction(
    c: &rusqlite::Connection,
    chat: PeerId,
    msg: MsgId,
    emoji: &str,
    delta: i64,
) -> rusqlite::Result<()> {
    let before: Option<String> = c.query_row(
        "SELECT reactions FROM tg_message WHERE chat = ?1 AND id = ?2",
        [chat, msg],
        |r| r.get(0),
    )?;
    let mut parts: Vec<String> = before
        .as_deref()
        .into_iter()
        .flat_map(|s| s.split(" · "))
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    if let Some(part) = parts
        .iter_mut()
        .find(|p| p.rsplit_once(' ').is_some_and(|(e, _)| e == emoji))
    {
        let count = part
            .rsplit_once(' ')
            .and_then(|(_, n)| n.parse::<i64>().ok())
            .unwrap_or(0);
        *part = format!("{emoji} {}", count.saturating_add(delta).max(0));
    } else if delta > 0 {
        parts.push(format!("{emoji} 1"));
    }
    parts.retain(|part| !part.ends_with(" 0"));
    let counts = (!parts.is_empty()).then(|| parts.join(" · "));
    super::reaction_state::set(c, chat, msg, counts.as_deref())
}
