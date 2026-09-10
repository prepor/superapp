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
fn workspace_reads_arrivals_without_input() {
    use crate::shell::{boot::Boot, stage::Stage};
    use kernel::{app::Mode, store::Store};

    crate::install();
    let shared = Rc::new(RefCell::new(None));
    let saved = shared.clone();
    let finished = Rc::new(Cell::new(false));
    let seen = finished.clone();
    let mut root = WidgetRef::empty();
    let mut pass = None;
    let mut draw_list: Option<DrawList> = None;
    let mut frame = 0;
    let mut inbox = None;
    let cx = Rc::new(RefCell::new(Cx::new(Box::new(move |cx, event| match event {
        Event::Startup => {
            root = cx.with_vm(|vm| {
                makepad_widgets::script_mod(vm);
                crate::shell::script_mod(vm);
                crate::apps::telegram::ui::script_mod(vm);
                let value = script_eval!(vm, { mod.widgets.Stage {
                    telegram_chat_tpl := mod.widgets.TelegramChatPanel {}
                } });
                WidgetRef::script_from_value(vm, value)
            });
            makepad_widgets::widget_tree::set_ui_root(cx, &root);
            let saved = saved.clone();
            root.borrow_mut::<Stage>().unwrap().boot(cx, Boot {
                db: None, grid: None, virtual_time: true, steps: None,
                out: Default::default(), no_draw: true, mode: Mode::Fake,
                primary: true, tag: String::new(), solo: false, bucket: None,
                open: Some(Box::new(move |store| {
                    store.write(|c| {
                        c.execute("DELETE FROM tg_message WHERE chat = ?1", [STELAXIS])?;
                        c.execute("INSERT INTO tg_message(chat, id, date, text)
                            VALUES(?1, 10, 10, 'already read')", [STELAXIS])?;
                        c.execute("UPDATE tg_chat SET unread = 0, last_read = 10, mention = 0
                            WHERE peer = ?1", [STELAXIS])?;
                        Ok(())
                    }).unwrap();
                    *saved.borrow_mut() = Some(Store::with_db(store.db()).unwrap());
                    store.attach_ui(SignalToUI::set_ui_signal);
                    Chat::id(STELAXIS)
                })),
            });
            inbox = Some(runtime::of(shared.borrow().as_ref().unwrap()).connect());
            let p = DrawPass::new(cx);
            p.set_size(cx, dvec2(1000.0, 700.0));
            pass = Some(p);
            draw_list = Some(DrawList::new(cx));
            cx.redraw_all();
        }
        Event::Draw(event) => {
            if !event.draw_list_will_redraw(cx, draw_list.as_ref().unwrap().id()) { return; }
            {
                let mut draw = CxDraw::new(cx, event);
                let pass = pass.as_ref().unwrap();
                draw.begin_pass(pass, Some(1.0));
                let list = draw_list.as_mut().unwrap();
                list.begin_always(&mut draw);
                let mut cx = Cx2d::new(&mut draw);
                cx.begin_root_turtle(dvec2(1000.0, 700.0), Layout::default());
                root.draw_all(&mut cx, &mut Scope::empty());
                cx.end_pass_sized_turtle();
                list.end(&mut draw);
                draw.end_pass(pass);
            }
            frame += 1;
            if frame == 2 {
                shared.borrow().as_ref().unwrap().write(|c| {
                    for (id, mention) in [(20, false), (30, true)] {
                        c.execute("INSERT INTO tg_message(chat, id, date, text, unread_mention)
                            VALUES(?1, ?2, ?2, 'arrived in the workspace', ?3)",
                            rusqlite::params![STELAXIS, id, mention])?;
                    }
                    c.execute("UPDATE tg_chat SET unread = 2, mention = 1 WHERE peer = ?1", [STELAXIS])?;
                    Ok(())
                }).unwrap();
                root.handle_event(cx, &Event::Signal, &mut Scope::empty());
            }
        }
        _ => {
            root.handle_event(cx, event, &mut Scope::empty());
            if let Some(inbox) = &inbox {
                while let Ok(request) = inbox.try_recv() {
                    let request: serde_json::Value = serde_json::from_str(&request).unwrap();
                    if request["@type"] == "viewMessages" && request["force_read"] == true {
                        assert!(frame > 2, "arrivals must be drawn before being read");
                        assert_eq!(request["message_ids"], serde_json::json!([20, 30]));
                        seen.set(true);
                    }
                }
            }
        }
    }))));
    Cx::headless_no_draw_event_loop_for_draw_cycles(cx, 80);
    assert!(finished.get(), "the hosted chat must read visible arrivals without input");
}

#[test]
fn visible_previews_read_arrivals_and_replies_without_input() {
    for (focused, retry) in [(false, false), (true, false), (false, true)] {
        static APPS: &[&dyn kernel::app::App] = &[&TELEGRAM];
        let env = kernel::app::Env::default();
        let clock = env.clock.clone();
        let mut session = Session::fake_with(APPS, &env);
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
        let mut waiting_for_retry = false;
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
                p.set_size(cx, dvec2(1000.0, 700.0));
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
                    cx.begin_root_turtle(dvec2(1000.0, 700.0), Layout::default());
                    // The shell hosts a conversation in a clipped body next
                    // to its list, below the panel's title.
                    cx.begin_turtle(Walk::abs_rect(Rect {
                        pos: dvec2(350.0, 50.0), size: dvec2(600.0, 600.0),
                    }), Layout { clip_x: true, clip_y: true, ..Default::default() });
                    current.hits.clear();
                    root.draw_all(&mut cx, &mut Scope::with_data_props(&mut session, &current));
                    cx.end_turtle();
                    cx.end_pass_sized_turtle();
                    assert!(root.borrow::<ChatPanel>().unwrap().rows.iter().all(|r| r.id != (STELAXIS, 5)),
                        "the older reply must remain outside the viewport");
                    if frame == 1 {
                        assert!(root.borrow::<ChatPanel>().unwrap().rows.iter().any(|r| r.id == (STELAXIS, 40)),
                            "the trailing outgoing line must be drawn with the arrivals");
                    }
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
                        c.execute("INSERT INTO tg_message(chat, id, date, text, out)
                            VALUES(?1, 40, ?2, 'my trailing message', 1)",
                            rusqlite::params![STELAXIS, now + 40.0])?;
                        c.execute("UPDATE tg_chat SET unread = 2, mention = 2 WHERE peer = ?1", [STELAXIS])?;
                        Ok(())
                    }).unwrap();
                    cx.redraw_all();
                }
                frame += 1;
            }
            Event::NextFrame(_) | Event::Timer(_) => {
                if waiting_for_retry && !matches!(event, Event::Timer(_)) { return; }
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
                if retry && !waiting_for_retry {
                    // Lose the first receipt, then leave the window idle.
                    // Only a timer can deliver the retry: no input or draws.
                    clock.advance(6.0);
                    waiting_for_retry = true;
                    return;
                }
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
                root.handle_event(cx, &Event::Signal, &mut Scope::with_data_props(&mut session, &current));
                assert_eq!(root.borrow::<ChatPanel>().unwrap().read_timer.0, 0,
                    "acknowledged reads stop retrying despite an older unread reply and a trailing outgoing line");
                assert!(inbox.try_recv().is_err());
                seen.set(true);
                if retry { cx.quit(); }
            }
            _ => {}
        }))));
        Cx::headless_no_draw_event_loop_for_draw_cycles(cx, if retry { 6000 } else { 4 });
        assert!(finished.get(), "visible messages must be read and retried while idle");
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
                    assert_eq!(root.borrow::<ChatPanel>().unwrap().read_timer.0, 0,
                        "the old transcript cannot keep its retry timer");
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
