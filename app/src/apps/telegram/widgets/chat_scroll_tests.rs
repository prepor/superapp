//! Opening positions measured on the actual chat template and virtual list.

use super::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use kernel::session::Action;
use makepad_widgets::makepad_platform::event::{ScrollEvent, ScrollPhase};
use crate::apps::telegram::{seed::ELENA, TELEGRAM};

#[test]
fn unread_openings_preserve_reading_and_allow_following_the_latest_message() {
    static APPS: &[&dyn kernel::app::App] = &[&TELEGRAM];
    for (height, unread, long_previous) in [
        (300.0, 1, false), (800.0, 1, true),
        (800.0, 3, false), (300.0, 40, true), (800.0, 0, true),
    ] {
        let mut session = Session::fake(APPS);
        let now = session.now();
        session.store().write(move |c| {
            c.execute("DELETE FROM tg_message WHERE chat = ?1", [ELENA])?;
            for id in 1..=30 + unread {
                let text = if id == 30 && long_previous {
                    "A tall preceding message.\n".repeat(80)
                } else { format!("message {id}") };
                c.execute("INSERT INTO tg_message(chat, id, sender, date, text)
                    VALUES(?1, ?2, ?1, ?3, ?4)",
                    rusqlite::params![ELENA, id, now + id as f64, text])?;
            }
            c.execute("UPDATE tg_chat SET unread = ?2, last_read = 30 WHERE peer = ?1",
                rusqlite::params![ELENA, unread])?;
            Ok(())
        }).unwrap();
        session.act(Action::new("open", "open unread chat").moving(|wm| {
            wm.open(Chat::id(ELENA), None, false);
        }));
        session.settle();
        let slot = session.focus().unwrap();
        let props = PanelProps {
            slot, panel: session.panel(slot).unwrap(), hits: Default::default(),
            keyboard: Default::default(), grab: Default::default(),
        };
        let finished = Rc::new(Cell::new(false));
        let seen = finished.clone();
        let mut root = WidgetRef::empty();
        let mut pass = None;
        let mut draw_list: Option<DrawList> = None;
        let mut frame = 0;
        let mut opening_y = None;
        let cx = Rc::new(RefCell::new(Cx::new(Box::new(move |cx, event| match event {
            Event::Startup => {
                root = cx.with_vm(|vm| {
                    makepad_widgets::script_mod(vm);
                    crate::shell::script_mod(vm);
                    crate::apps::telegram::ui::script_mod(vm);
                    let value = script_eval!(vm, { mod.widgets.TelegramChatPanel {} });
                    WidgetRef::script_from_value(vm, value)
                });
                makepad_widgets::widget_tree::set_ui_root(cx, &root);
                let p = DrawPass::new(cx);
                p.set_size(cx, dvec2(360.0, height));
                pass = Some(p);
                draw_list = Some(DrawList::new(cx));
                cx.redraw_all();
            }
            Event::Draw(event) if frame < 10 => {
                if frame == 3 && unread > 0 {
                    // Older history arriving and a new message must preserve
                    // the reading position, including a short unread run.
                    session.store().write(move |c| {
                        for (id, date) in [(0, now - 86400.0), (31 + unread, now + 100.0)] {
                            c.execute("INSERT INTO tg_message(chat, id, sender, date, text)
                                VALUES(?1, ?2, ?1, ?3, 'another message')",
                                rusqlite::params![ELENA, id, date])?;
                        }
                        Ok(())
                    }).unwrap();
                }
                if frame == 7 {
                    session.store().write(move |c| {
                        let id = 31 + unread + i64::from(unread > 0);
                        c.execute("INSERT INTO tg_message(chat, id, sender, date, text)
                            VALUES(?1, ?2, ?1, ?3, 'newest message')",
                            rusqlite::params![ELENA, id, now + 200.0])?;
                        Ok(())
                    }).unwrap();
                }
                let mut draw = CxDraw::new(cx, event);
                let pass = pass.as_ref().unwrap();
                draw.begin_pass(pass, Some(1.0));
                let list = draw_list.as_mut().unwrap();
                list.begin_always(&mut draw);
                let mut cx = Cx2d::new(&mut draw);
                cx.begin_root_turtle(dvec2(360.0, height), Layout::default());
                props.hits.clear();
                root.draw_all(&mut cx, &mut Scope::with_data_props(&mut session, &props));
                cx.end_pass_sized_turtle();
                let portal = root.widget(&cx, LIST).as_portal_list();
                let viewport = portal.area().rect(&cx);
                if matches!(frame, 2..=4) && unread > 0 {
                    let panel = root.borrow::<ChatPanel>().unwrap();
                    let first = panel.rows.iter().find(|row| row.id == 31)
                        .expect("the first unread message must be visible");
                    let y = first.unclipped.pos.y - viewport.pos.y;
                    assert!(y >= 0.0 && y < viewport.size.y * 0.3,
                        "first unread at {y} in {}px viewport ({unread} unread, tall predecessor: {long_previous})",
                        viewport.size.y);
                    if frame == 2 { opening_y = Some(y); }
                    else { assert!((y - opening_y.unwrap()).abs() < 1.0, "updates moved the first unread message"); }
                }
                if matches!(frame, 6 | 9) || (frame == 2 && unread == 0) {
                    let panel = root.borrow::<ChatPanel>().unwrap();
                    assert!(panel.unread_space.is_none(), "the latest view has no reserved space");
                    let last_id = 30 + unread + i64::from(unread > 0) + i64::from(frame >= 7);
                    let last = panel.rows.iter().find(|row| row.id == last_id).unwrap();
                    assert!((last.unclipped.pos.y + last.unclipped.size.y
                        - viewport.pos.y - viewport.size.y).abs() < 1.0,
                        "the latest message must rest at the bottom");
                }
                if matches!(frame, 2 | 4) && unread == 1 && !long_previous {
                    assert!(portal.is_at_end(), "reserved space fills the viewport before a pan");
                    let space = root.borrow::<ChatPanel>().unwrap().unread_space
                        .expect("a short unread run reserves space");
                    let event = Event::Scroll(ScrollEvent {
                        window_id: CxWindowPool::id_zero(),
                        scroll: dvec2(if frame == 2 { 30.0 } else { -30.0 }, 0.1),
                        abs: viewport.pos + viewport.size / 2.0, modifiers: Default::default(),
                        handled_x: Cell::new(false), handled_y: Cell::new(false),
                        is_mouse: false, time: now, phase: ScrollPhase::Changed,
                    });
                    root.handle_event(&mut cx, &event, &mut Scope::with_data_props(&mut session, &props));
                    assert_eq!(root.borrow::<ChatPanel>().unwrap().unread_space, Some(space),
                        "a horizontal pan with vertical drift must preserve the unread space");
                }
                if frame == 4 {
                    // Dispatch after the pass has resolved its hit rectangles.
                    let event = if unread == 1 && !long_previous {
                        Event::Scroll(ScrollEvent {
                            window_id: CxWindowPool::id_zero(), scroll: dvec2(0.0, 30.0),
                            abs: viewport.pos + viewport.size / 2.0, modifiers: Default::default(),
                            handled_x: Cell::new(false), handled_y: Cell::new(false),
                            is_mouse: true, time: now, phase: ScrollPhase::None,
                        })
                    } else { Event::KeyDown(KeyEvent { key_code: KeyCode::End, ..Default::default() }) };
                    root.handle_event(&mut cx, &event, &mut Scope::with_data_props(&mut session, &props));
                }
                frame += 1;
                if frame < 10 { cx.redraw_area_in_draw(root.area()); }
                else { seen.set(true); }
                list.end(&mut draw);
                draw.end_pass(pass);
            }
            _ => {}
        }))));
        Cx::headless_event_loop_for_draw_cycles(cx, 10);
        assert!(finished.get(), "every opening and navigation phase must draw");
    }
}
