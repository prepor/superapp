//! Undoable topic preferences and offline message actions.
//!
//! Live message changes queue requests and let TDLib updates settle the store.
//! Topic visibility is this app's preference in both live and offline accounts.

use rusqlite::OptionalExtension;

use kernel::effect::World;
use kernel::history::Intent;
use kernel::session::{Edit, Session};

use super::model::{self, LineCopy, MsgId, PeerId};
use super::topics;

/// Each gesture owns only the visibility flags it changes, so undo restores
/// a mixed selection without changing topic metadata or later discoveries.
pub fn select_topics(s: &mut Session, chat: PeerId, ids: Vec<i64>, selected: bool) {
    if ids.is_empty() { return; }
    let n = ids.len();
    let word = if selected { "show" } else { "hide" };
    s.act_async(Edit::writing(
        "topic selection",
        format!("{word} {n} topic{}", if n == 1 { "" } else { "s" }),
        move |tx| {
            let mut changed = Vec::new();
            for id in ids {
                if tx.execute("UPDATE tg_topic SET selected = ?3 WHERE chat = ?1 AND id = ?2 AND selected != ?3",
                    rusqlite::params![chat, id, selected])? != 0 { changed.push(id); }
            }
            Ok(changed)
        },
    ).record_if(|ids| !ids.is_empty()).claiming_with(move |ids| {
        vec![Box::new(TopicSelection { chat, ids: std::mem::take(ids), selected })]
    }), |_, _| {});
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
    _was_edited: bool,
    after: &str,
) {
    let after = after.to_string();
    s.act_async(Edit::writing("edit", format!("edit “{}”", short(before)), move |tx| {
        let old = tx.query_row("SELECT text, edited, entities, entities_known FROM tg_message WHERE chat = ?1 AND id = ?2",
            [chat, msg], |row| {
                let entities = if row.get::<_, bool>(3)? {
                    Some(serde_json::from_str(&row.get::<_, String>(2)?).unwrap_or_default())
                } else { None };
                Ok((row.get(0)?, row.get(1)?, entities))
            }).optional()?;
        let Some((before, was_edited, before_entities)) = old else { return Ok(None); };
        model::edit_tx(tx, chat, msg, &after, true, None)?;
        Ok(Some(Edited { chat, msg, before, before_entities, was_edited, after }))
    }).about(format!("chat:{chat}"))
        .record_if(Option::is_some)
        .claiming_with(|edited| vec![Box::new(edited.take().expect("committed edit"))]), |_, _| {});
}

/// Deletes lines, copied out whole first, so undo puts them back exactly
/// as they were — the same ids, the same dates, in the same places.
pub fn delete_lines(s: &mut Session, chat: PeerId, ids: Vec<MsgId>) {
    if ids.is_empty() {
        return;
    }
    let n = ids.len();
    s.act_async(Edit::writing(
        "delete",
        format!("delete {n} line{}", if n == 1 { "" } else { "s" }),
        move |tx| {
            let copies = model::copy_lines_tx(tx, chat, &ids)?;
            if copies.is_empty() { return Ok(None); }
            model::delete_lines_tx(tx, chat, &ids)?;
            Ok(Some(Deleted { chat, ids, copies }))
        },
    ).about(format!("chat:{chat}"))
        .record_if(Option::is_some)
        .claiming_with(|deleted| vec![Box::new(deleted.take().expect("committed deletion"))]), |_, _| {});
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
    copies: Vec<LineCopy>,
}

impl Intent for Deleted {
    fn describe(&self) -> String {
        let n = self.ids.len();
        format!("chat:{} {n} line{} deleted", self.chat, if n == 1 { "" } else { "s" })
    }

    fn reverse(&self, w: &World) -> Result<(), String> {
        let rows = self.copies.clone();
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
    let reaction = DemoReaction { chat, msg, emoji: emoji.into() };
    // Reserve the reaction while its transaction waits, so a second click
    // cannot count the same local reaction twice.
    rt.remember_demo_reaction(chat, msg, emoji);
    let accepted = std::rc::Rc::new(std::cell::Cell::new(true));
    let result = accepted.clone();
    let emoji = emoji.to_string();
    s.act_async(Edit::writing("react", format!("react {emoji}"), move |tx| {
        demo_reaction(tx, chat, msg, &chosen, 1)
    }).claiming(vec![Box::new(reaction)]), move |_, done| {
        if done.is_none() {
            rt.forget_demo_reaction(chat, msg, &emoji);
            result.set(false);
        }
    });
    accepted.get()
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
