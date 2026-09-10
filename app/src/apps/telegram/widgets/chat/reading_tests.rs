use super::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use kernel::session::Action;
use crate::apps::telegram::{panels::{Chats, Messages}, runtime, seed::{FAMILY, STELAXIS}, sync, transport::FakeTd, TELEGRAM};
use crate::shell::{hits::Hits, hosted::Grab, keyboard::Keyboard};

fn props(session: &Session, slot: kernel::layout::SlotId) -> PanelProps {
    PanelProps {
        slot,
        panel: session.panel(slot).unwrap(),
        hits: Hits::default(),
        keyboard: Keyboard::default(),
        has_keyboard: session.focus() == Some(slot),
        grab: Grab::default(),
    }
}

fn widget(cx: &mut Cx) -> WidgetRef {
    cx.with_vm(|vm| {
        let value = script_eval!(vm, { mod.widgets.TelegramChatPanel {} });
        WidgetRef::script_from_value(vm, value)
    })
}

#[test]
fn visible_previews_read_arrivals_and_replies_without_input() {
    for focused in [false, true] {
        static APPS: &[&dyn kernel::app::App] = &[&TELEGRAM];
        let mut session = Session::fake(APPS);
        session.set_viewport((1000.0, 600.0));
        let now = session.now();
        session.store().write(move |c| {
            c.execute("DELETE FROM tg_message WHERE chat = ?1", [STELAXIS])?;
            c.execute("INSERT INTO tg_message(chat, id, date, text, unread_mention)
                VALUES(?1, 5, ?2, 'an older unseen reply', 1)", (STELAXIS, now))?;
            c.execute("INSERT INTO tg_message(chat, id, date, text)
                VALUES(?1, 10, ?2, ?3)",
                rusqlite::params![STELAXIS, now + 1.0, "Earlier history.\n".repeat(80)])?;
            c.execute("UPDATE tg_chat SET unread = 0, last_read = 10, mention = 1 WHERE peer = ?1", [STELAXIS])?;
            Ok(())
        }).unwrap();
        session.act(Action::new("open", "chats").moving(|wm| {
            wm.open(Chats::id(), None, false);
        }));
        session.settle();
        let list_slot = session.focus().unwrap();
        session.nav(Nav::Preview { from: list_slot, id: Chat::id(STELAXIS) });
        session.settle();
        let slot = session.joined_child(list_slot).unwrap();
        if focused { session.nav(Nav::Focus(slot)); session.settle(); }
        let current = props(&session, slot);
        let inbox = runtime::of(session.store()).connect();
        let finished = Rc::new(Cell::new(false));
        let seen = finished.clone();
        let mut root = WidgetRef::empty();
        let mut pass = None;
        let mut draw_list: Option<DrawList> = None;
        let mut frame = 0;
        let cx = Rc::new(RefCell::new(Cx::new(Box::new(move |cx, event| match event {
            Event::Startup => {
                cx.with_vm(|vm| {
                    makepad_widgets::script_mod(vm);
                    crate::shell::script_mod(vm);
                    crate::apps::telegram::ui::script_mod(vm);
                });
                root = widget(cx);
                makepad_widgets::widget_tree::set_ui_root(cx, &root);
                let p = DrawPass::new(cx);
                p.set_size(cx, dvec2(600.0, 600.0));
                pass = Some(p);
                draw_list = Some(DrawList::new(cx));
                cx.redraw_all();
            }
            Event::Draw(event) if frame < 2 => {
                {
                    let mut draw = CxDraw::new(cx, event);
                    let pass = pass.as_ref().unwrap();
                    draw.begin_pass(pass, Some(1.0));
                    let list = draw_list.as_mut().unwrap();
                    list.begin_always(&mut draw);
                    let mut cx = Cx2d::new(&mut draw);
                    cx.begin_root_turtle(dvec2(600.0, 600.0), Layout::default());
                    current.hits.clear();
                    root.draw_all(&mut cx, &mut Scope::with_data_props(&mut session, &current));
                    cx.end_pass_sized_turtle();
                    assert!(root.borrow::<ChatPanel>().unwrap().rows.iter().all(|r| r.id != (STELAXIS, 5)),
                        "the older reply must remain outside the viewport");
                    list.end(&mut draw);
                    draw.end_pass(pass);
                }
                if frame == 0 {
                    session.store().write(move |c| {
                        for (id, mention) in [(20, false), (30, true)] {
                            c.execute("INSERT INTO tg_message(chat, id, date, text, unread_mention)
                                VALUES(?1, ?2, ?3, 'arrived in the open chat', ?4)",
                                rusqlite::params![STELAXIS, id, now + id as f64, mention])?;
                        }
                        c.execute("UPDATE tg_chat SET unread = 2, mention = 2 WHERE peer = ?1", [STELAXIS])?;
                        Ok(())
                    }).unwrap();
                    cx.redraw_all();
                }
                frame += 1;
            }
            Event::NextFrame(_) => {
                root.handle_event(cx, event, &mut Scope::with_data_props(&mut session, &current));
                if frame < 2 {
                    assert!(inbox.try_recv().is_err(), "arrivals must be drawn before they are read");
                    return;
                }
                if seen.get() { return; }
                let request: serde_json::Value = serde_json::from_str(&inbox.try_recv()
                    .expect("visible arrivals must be read without clicking or focusing the chat")).unwrap();
                assert_eq!(request["@type"], "viewMessages");
                assert_eq!(request["message_ids"], serde_json::json!([20, 30]));
                assert_eq!(request["force_read"], true);
                assert_eq!(session.focus(), Some(if focused { slot } else { list_slot }));
                let card = model::peer(session.store(), STELAXIS).unwrap();
                assert_eq!((card.unread, card.unread_mentions), (2, 2), "wait for Telegram's receipt");
                let acc = sync::Account::new(FakeTd::new(), 17844,
                    std::env::temp_dir().join("superapp-tg-viewport-tests"), None);
                acc.on_update(session.world(), &serde_json::json!({
                    "@type": "ok", "@extra": request["@extra"],
                }).to_string());
                acc.on_update(session.world(), &serde_json::json!({
                    "@type": "updateMessageMentionRead", "chat_id": STELAXIS,
                    "message_id": 30, "unread_mention_count": 1,
                }).to_string());
                let card = model::peer(session.store(), STELAXIS).unwrap();
                assert_eq!((card.unread, card.last_read, card.unread_mentions), (0, Some(30), 1));
                assert!(model::line(session.store(), STELAXIS, 5).unwrap().unread_mention);
                assert!(!model::line(session.store(), STELAXIS, 30).unwrap().unread_mention);
                seen.set(true);
            }
            _ => {}
        }))));
        Cx::headless_no_draw_event_loop_for_draw_cycles(cx, 4);
        assert!(finished.get(), "drawing new messages must schedule their read check");
    }
}

#[test]
fn replacing_a_transcript_waits_for_its_own_draw_before_reading() {
    // A different chat can share every message id. A different target in
    // the same chat is also a new panel instance with an undrawn viewport.
    for same_chat in [false, true] {
        static APPS: &[&dyn kernel::app::App] = &[&TELEGRAM];
        let mut session = Session::fake(APPS);
        session.set_viewport((1000.0, 600.0));
        session.store().write(|c| {
            for chat in [STELAXIS, FAMILY] {
                c.execute("DELETE FROM tg_message WHERE chat = ?1", [chat])?;
                for id in [10, 20] {
                    c.execute("INSERT INTO tg_message(chat, id, date, text)
                        VALUES(?1, ?2, ?2, 'a visible unread message')", [chat, id])?;
                }
                c.execute("UPDATE tg_chat SET unread = 2, last_read = 0, mention = 0 WHERE peer = ?1", [chat])?;
            }
            Ok(())
        }).unwrap();
        session.act(Action::new("open", "replies").moving(|wm| {
            wm.open(Messages::replies(None), None, false);
        }));
        session.settle();
        let inbox_slot = session.focus().unwrap();
        session.nav(Nav::Open { from: inbox_slot, id: Chat::at(STELAXIS, 10), fresh: false });
        session.settle();
        let slot = session.joined_child(inbox_slot).unwrap();
        assert_eq!(session.focus(), Some(slot));
        let target = if same_chat { STELAXIS } else { FAMILY };
        let next = Chat::at(target, if same_chat { 20 } else { 10 });
        let inbox = runtime::of(session.store()).connect();
        let finished = Rc::new(Cell::new(false));
        let seen = finished.clone();
        let mut root = WidgetRef::empty();
        let mut pass = None;
        let mut draw_list: Option<DrawList> = None;
        let mut frame = 0;
        let cx = Rc::new(RefCell::new(Cx::new(Box::new(move |cx, event| match event {
            Event::Startup => {
                cx.with_vm(|vm| {
                    makepad_widgets::script_mod(vm);
                    crate::shell::script_mod(vm);
                    crate::apps::telegram::ui::script_mod(vm);
                });
                root = widget(cx);
                makepad_widgets::widget_tree::set_ui_root(cx, &root);
                let p = DrawPass::new(cx);
                p.set_size(cx, dvec2(600.0, 600.0));
                pass = Some(p);
                draw_list = Some(DrawList::new(cx));
                cx.redraw_all();
            }
            Event::Draw(event) if frame < 2 => {
                if frame == 1 {
                    // draw_hosted replaces the stale widget on this draw.
                    root = widget(cx);
                    makepad_widgets::widget_tree::set_ui_root(cx, &root);
                }
                let current = props(&session, slot);
                {
                    let mut draw = CxDraw::new(cx, event);
                    let pass = pass.as_ref().unwrap();
                    draw.begin_pass(pass, Some(1.0));
                    let list = draw_list.as_mut().unwrap();
                    list.begin_always(&mut draw);
                    let mut cx = Cx2d::new(&mut draw);
                    cx.begin_root_turtle(dvec2(600.0, 600.0), Layout::default());
                    root.draw_all(&mut cx, &mut Scope::with_data_props(&mut session, &current));
                    cx.end_pass_sized_turtle();
                    list.end(&mut draw);
                    draw.end_pass(pass);
                }
                if frame == 0 {
                    session.switch(1);
                    session.settle();
                    root.handle_event(cx, &Event::Signal, &mut Scope::with_data_props(&mut session, &current));
                    assert!(inbox.try_recv().is_err(), "hidden workspaces cannot read retained rows");
                    session.switch(0);
                    session.settle();
                    root.handle_event(cx, &Event::Background, &mut Scope::with_data_props(&mut session, &current));
                    root.handle_event(cx, &Event::Signal, &mut Scope::with_data_props(&mut session, &current));
                    assert!(inbox.try_recv().is_err(), "background windows cannot read visible rows");
                    root.handle_event(cx, &Event::Foreground, &mut Scope::with_data_props(&mut session, &current));
                }
                // Repeated events must not duplicate a pending receipt.
                for _ in 0..2 {
                    root.handle_event(cx, &Event::Signal, &mut Scope::with_data_props(&mut session, &current));
                }
                let request: serde_json::Value = serde_json::from_str(&inbox.try_recv()
                    .expect("a drawn focused transcript sends its read receipt")).unwrap();
                assert_eq!(request["@type"], "viewMessages");
                assert_eq!(request["chat_id"], if frame == 0 { STELAXIS } else { target });
                assert_eq!(request["message_ids"], serde_json::json!([10, 20]));
                assert!(inbox.try_recv().is_err());

                if frame == 0 {
                    assert!(root.borrow::<ChatPanel>().unwrap().mounted);
                    session.nav(Nav::Open { from: inbox_slot, id: next.clone(), fresh: false });
                    session.settle();
                    assert_eq!(session.joined_child(inbox_slot), Some(slot), "the slot is reused");
                    assert_eq!(session.focus(), Some(slot));
                    let replacement = props(&session, slot);
                    assert!(!Rc::ptr_eq(&current.panel, &replacement.panel));
                    // This is forward_one's event path between Nav::Open
                    // and draw_hosted: old widget, new instance, same ids.
                    for _ in 0..2 {
                        root.handle_event(cx, &Event::Signal, &mut Scope::with_data_props(&mut session, &replacement));
                    }
                    assert!(inbox.try_recv().is_err(), "old row ids must not read the replacement chat");
                    let card = model::peer(session.store(), target).unwrap();
                    assert_eq!((card.unread, card.last_read), (2, Some(0)));
                    assert!(root.borrow::<ChatPanel>().unwrap().viewed.is_none(), "the old viewport is released");
                    cx.redraw_all();
                } else {
                    let acc = sync::Account::new(FakeTd::new(), 17844,
                        std::env::temp_dir().join("superapp-tg-viewport-tests"), None);
                    acc.on_update(session.world(), &serde_json::json!({
                        "@type": "ok", "@extra": request["@extra"],
                    }).to_string());
                    let card = model::peer(session.store(), target).unwrap();
                    assert_eq!((card.unread, card.last_read), (0, Some(20)), "the replacement reads after its own draw");
                    seen.set(true);
                }
                frame += 1;
            }
            _ => {}
        }))));
        Cx::headless_event_loop_for_draw_cycles(cx, 3);
        assert!(finished.get(), "both panel instances must draw");
    }
}
