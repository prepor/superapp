//! Draw the real list with the native background query reader attached.
use super::*;
use crate::shell::hosted::PanelProps;
use makepad_widgets::*;
use std::sync::mpsc;
use std::time::{Duration, Instant};

struct NoteListView {
    session: Session,
    props: PanelProps,
    wake: mpsc::Receiver<()>,
    cx: Cx,
    widget: WidgetRef,
    pass: DrawPass,
    draw_list: DrawList,
    frame: u32,
}

impl NoteListView {
    fn new(filter: &str) -> Self {
        let mut session = Session::fake(APPS);
        session.store().write(|tx| {
            for id in 1..=4 {
                let title = if id == 2 { "Chosen".into() } else { format!("Note {id}") };
                tx.execute("INSERT INTO notes_note(id,title,body,created,modified,deleted) VALUES(?1,?2,?2,?1,?1,0)",
                    rusqlite::params![id, title])?;
            }
            Ok(())
        }).unwrap();
        let slot = open(&mut session, NoteList::id());
        let props = PanelProps {
            slot,
            panel: session.panel(slot).unwrap(),
            hits: Default::default(),
            keyboard: Default::default(),
            grab: Default::default(),
        };
        let (notify, wake) = mpsc::channel();
        session.store().attach_ui(move || {
            let _ = notify.send(());
        });
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let widget = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);
            crate::shell::script_mod(vm);
            super::super::ui::script_mod(vm);
            let value = script_eval!(vm, { mod.widgets.NotesPanel {} });
            WidgetRef::script_from_value(vm, value)
        });
        widget
            .text_input(&cx, ids!(filter_input))
            .set_text(&mut cx, filter);
        let pass = DrawPass::new(&mut cx);
        pass.set_size(&mut cx, dvec2(500.0, 400.0));
        let draw_list = DrawList::new(&mut cx);
        Self {
            session,
            props,
            wake,
            cx,
            widget,
            pass,
            draw_list,
            frame: 0,
        }
    }

    fn with_list<T>(
        &mut self,
        f: impl FnOnce(
            &mut ListState<&'static kernel::richtable::SqlSource<model::Note, i64>>,
            &Store,
        ) -> T,
    ) -> T {
        let mut panel = self.props.panel.borrow_mut();
        let panel = panel.as_any().downcast_mut::<NoteList>().unwrap();
        f(&mut panel.list, self.session.store())
    }

    fn draw(&mut self) -> Vec<(usize, String, bool)> {
        self.session.store().poll_external();
        self.frame += 1;
        self.cx.new_draw_event = DrawEvent::default();
        let event = DrawEvent {
            redraw_all: true,
            time: f64::from(self.frame) / 60.0,
            ..Default::default()
        };
        let mut draw = CxDraw::new(&mut self.cx, &event);
        draw.begin_pass(&self.pass, Some(1.0));
        self.draw_list.begin_always(&mut draw);
        {
            let mut cx = Cx2d::new(&mut draw);
            cx.begin_root_turtle(dvec2(500.0, 400.0), Layout::default());
            self.widget.draw_all(
                &mut cx,
                &mut Scope::with_data_props(&mut self.session, &self.props),
            );
            cx.end_pass_sized_turtle();
        }
        self.draw_list.end(&mut draw);
        draw.end_pass(&self.pass);
        drop(draw);
        let portal = self.widget.widget(&self.cx, ids!(list)).as_portal_list();
        let portal = portal.borrow().unwrap();
        let mut rows = Vec::new();
        for (index, item) in portal.items().iter() {
            for (path, selected) in [(ids!(line), false), (ids!(line_sel), true)] {
                let line = item.widget.widget(&self.cx, path);
                if line.visible() {
                    rows.push((
                        *index,
                        line.label(&self.cx, ids!(body.title_lbl)).text(),
                        selected,
                    ));
                }
            }
        }
        rows.sort_by_key(|row| row.0);
        if rows.iter().any(|(_, _, selected)| *selected) {
            assert!(
                !self.widget.widget(&self.cx, ids!(empty_lbl)).visible(),
                "a visible selected note must not be labelled as an empty list"
            );
        }
        rows
    }

    fn draw_until(&mut self, mut ready: impl FnMut(&[(usize, String, bool)]) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let rows = self.draw();
            if ready(&rows) && !self.session.store().queries_pending() {
                return;
            }
            self.wake
                .recv_timeout(
                    deadline
                        .checked_duration_since(Instant::now())
                        .expect("list refreshed"),
                )
                .expect("background query woke the list");
        }
    }
}

#[test]
fn typing_keeps_the_selected_note_highlighted_during_background_refresh() {
    check_edit_refresh("");
}

#[test]
fn typing_keeps_filtered_notes_stable_during_background_refresh() {
    check_edit_refresh("e");
}

#[test]
fn a_note_that_stops_matching_the_filter_keeps_its_latest_visible_title() {
    let mut view = NoteListView::new("Chosen");
    view.draw_until(|rows| rows.len() == 1);
    let selected = view.with_list(|list, store| list.set_cursor(store, 0).unwrap().id);
    view.draw_until(|rows| rows[0].2);
    let mut previous = "Chosen".to_owned();
    for edit in 0..8 {
        let title = format!("Edited {edit}");
        model::edit(
            view.session.store(),
            selected,
            title.clone(),
            10.0 + f64::from(edit),
        )
        .unwrap();
        view.draw_until(|rows| {
            assert_eq!(rows.len(), 1, "the selected note stays visible: {rows:?}");
            assert!(rows[0].2, "the note stays highlighted: {rows:?}");
            assert!(
                rows[0].1 == previous || rows[0].1 == title,
                "refreshing must not restore an older title: {rows:?}"
            );
            rows[0].1 == title
        });
        previous = title;
    }
    view.with_list(|list, _| list.clear_cursor());
    view.draw_until(|rows| rows.is_empty());
    view.session.shutdown();
}

fn check_edit_refresh(filter: &str) {
    let mut view = NoteListView::new(filter);
    view.draw_until(|rows| rows.len() == 4);
    let selected = view.with_list(|list, store| list.set_cursor(store, 2).unwrap().id);
    assert_eq!(selected, 2);
    view.draw_until(|rows| {
        rows.iter()
            .any(|(_, title, selected)| *selected && title == "Chosen")
    });
    for edit in 0..8 {
        let title = format!("Chosen {edit}");
        model::edit(
            view.session.store(),
            selected,
            format!("{title}\nBody {edit}"),
            10.0 + f64::from(edit),
        )
        .unwrap();
        view.draw_until(|rows| {
            assert_eq!(rows.len(), 4, "saving must keep the rows visible: {rows:?}");
            let highlighted: Vec<_> = rows.iter().filter(|(_, _, selected)| *selected).collect();
            assert_eq!(
                highlighted.len(),
                1,
                "the highlight must remain visible: {rows:?}"
            );
            assert!(
                highlighted[0].1.starts_with("Chosen"),
                "the highlight jumped to another note: {rows:?}"
            );
            for unchanged in ["Note 1", "Note 3", "Note 4"] {
                assert_eq!(
                    rows.iter()
                        .filter(|(_, title, _)| title == unchanged)
                        .count(),
                    1,
                    "refreshing must not duplicate or drop another note: {rows:?}"
                );
            }
            rows[0].1 == title
        });
        view.with_list(|list, store| {
            assert_eq!(
                list.cursor_index(store),
                Some(0),
                "navigation follows the position shown on screen"
            );
            assert_eq!(list.cursor_key(), Some(&selected));
        });
    }
    view.with_list(|list, store| {
        assert_eq!(
            list.move_cursor(store, 1).unwrap().id,
            4,
            "down opens the note below the edited note"
        );
    });
    view.with_list(|list, store| list.set_cursor(store, 0).unwrap());
    view.draw_until(|rows| rows[0].2);
    view.session
        .store()
        .write(move |tx| tx.execute("UPDATE notes_note SET deleted=1 WHERE id=?1", [selected]))
        .unwrap();
    view.draw_until(|rows| {
        rows.len() == 3
            && rows[0].1 == "Note 4"
            && rows[0].2
            && rows.iter().filter(|(_, _, selected)| *selected).count() == 1
    });
    view.session.shutdown();
}
