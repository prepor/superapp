//! The message a person asks about must reach the agent in full, even when
//! it changes after the panel chip was made.

use super::*;
use crate::apps::agent::chip::Chip;
use kernel::context;
use serde_json::json;

const MESSAGE_ID: i64 = 9002;
const REPLY_ID: i64 = 9001;

fn line_session(body: &str, reply: &str) -> (Session, SlotId) {
    let mut s = session();
    let (body, reply) = (body.to_string(), reply.to_string());
    s.store().write(move |tx| {
        for (chat, id, text, reply_to) in [
            (VERA, REPLY_ID, reply.as_str(), None),
            (VERA, MESSAGE_ID, body.as_str(), Some(REPLY_ID)),
            (ELENA, REPLY_ID, "another chat's quoted message", None),
            (ELENA, MESSAGE_ID, "another chat's message", Some(REPLY_ID)),
        ] {
            tx.execute(
                "INSERT INTO tg_message(id, chat, sender, date, text, reply_to)
                 VALUES(?1, ?2, ?2, ?3, ?4, ?5)",
                rusqlite::params![id, chat, ts(2026, 9, 1, 11, 52), text, reply_to],
            )?;
        }
        Ok(())
    }).unwrap();
    let slot = open_root(&mut s, Line::id(VERA, MESSAGE_ID));
    // The reads made by the shell's title/bar and the line widget's body.
    s.store().trace_begin(slot);
    let inst = s.panel(slot).unwrap();
    let mut panel = inst.borrow_mut();
    let _ = panel.title();
    let _ = panel.verbs();
    let _ = panel.as_any().downcast_mut::<Line>().unwrap().msg();
    s.store().trace_end();
    drop(panel);
    (s, slot)
}

#[test]
fn line_context_carries_the_full_message_and_reply_with_a_description() {
    // Near the size of two Telegram messages, with multibyte text, code,
    // pipes and whitespace that the table preview would clip or flatten.
    let body = format!("  {}\n\n```rust\nlet value = \"a | b\";\n```\n끝 🦀\t  ", "界".repeat(3900));
    let reply = format!("{}\nquoted reply tail", "文".repeat(4000));
    let (mut s, slot) = line_session(&body, &reply);
    let tool = s.apps().tool("panels.context").unwrap().clone();
    let result = (tool.run)(&mut s, &json!({"slot": slot})).unwrap();
    let text = result["context"].as_str().unwrap();

    assert!(text.contains("One Telegram message as a card"));
    assert!(text.contains(&format!("chat id ({VERA}, `tg_peer.id`)")));
    assert!(text.contains(&format!("message id ({MESSAGE_ID}, `tg_message.id`)")));
    assert!(text.contains("Message ids are only unique within a chat"));
    assert!(text.contains("Telegram conversation's composer"));
    assert!(text.contains(&format!("\n````text\n{body}\n````\n")), "complete message, safely fenced");
    assert!(text.contains(&format!("\n```text\n{reply}\n```\n")), "complete quoted reply");
    assert!(text.contains("#### text (row 1)"));
    assert!(text.contains("#### reply_text (row 1)"));
    assert!(text.contains("### tg line — one message and its reply"));
    assert!(text.contains("WHERE m.chat = ?1 AND m.id = ?2"));
    assert!(text.contains("rows (1 of 1, the panel's own page)"));
    assert!(!text.contains("another chat's"), "message ids are scoped to their chat");
    assert!(text.len() < context::CAP, "both full texts fit the panel budget");

    let chip = Chip::panel(&s, slot).unwrap();
    assert_eq!(chip.render(&s), text, "the chip and tool use the same rendering");
}

#[test]
fn line_chips_reread_full_text_after_edits_and_after_the_panel_closes() {
    let (mut s, slot) = line_session("original message", "original quote");
    let chip = Chip::panel(&s, slot).unwrap();
    let pasted = Chip::from_paste(&s, &context::header_line(&Line::id(VERA, MESSAGE_ID))).unwrap();
    let saved = chip.to_json();
    assert_eq!(saved["text_columns"], json!(["text", "reply_text"]));
    assert!(!saved.to_string().contains("original message"), "the chip stores no text snapshot");
    let restored = Chip::from_json(&saved).unwrap();
    let mut legacy = saved;
    legacy.as_object_mut().unwrap().remove("text_columns");
    let legacy = Chip::from_json(&legacy).unwrap();

    let body = format!("{}\n  edited message tail\n", "new body ".repeat(60));
    let reply = format!("{}\n  edited quote tail\n", "new quote ".repeat(60));
    let (changed_body, changed_reply) = (body.clone(), reply.clone());
    s.store().write(move |tx| {
        for (id, text) in [(MESSAGE_ID, changed_body), (REPLY_ID, changed_reply)] {
            tx.execute(
                "UPDATE tg_message SET text = ?1, edited = 1 WHERE chat = ?2 AND id = ?3",
                rusqlite::params![text, VERA, id],
            )?;
        }
        Ok(())
    }).unwrap();

    for reference in [&chip, &pasted, &restored, &legacy] {
        let text = reference.render(&s);
        assert!(text.contains(&body), "the complete edited message is read at send time");
        assert!(text.contains(&reply), "the complete edited reply is read at send time");
        assert!(!text.contains("original message"));
        assert!(!text.contains("original quote"));
    }

    s.act(Action::new("close", "close").moving(move |wm| { wm.close(slot); }));
    s.settle();
    assert!(s.panel(slot).is_none());
    let text = chip.render(&s);
    assert!(text.contains(&body));
    assert!(text.contains(&reply));

    s.store().write(|tx| {
        tx.execute("DELETE FROM tg_message WHERE chat = ?1 AND id = ?2", [VERA, MESSAGE_ID])?;
        Ok(())
    }).unwrap();
    let text = chip.render(&s);
    assert!(!text.contains(&body), "a deleted message must not survive in a text snapshot");
    assert!(!text.contains(&reply));
    assert!(text.contains("rows (0 of 1, the panel's own page)"));
}
