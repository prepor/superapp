//! The flashcards, drawn: the front, the back, the finish.

use kernel::session::Session;
use makepad_widgets::*;

use crate::shell::hosted::PanelProps;

use super::super::model;
use super::super::panels::Review;
use super::{text_hit, with};

#[derive(Script, ScriptHook, Widget)]
pub struct ReviewPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[rust]
    card_rect: Option<Rect>,
}

impl Widget for ReviewPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return;
        };
        let has_focus = scope
            .data
            .get::<Session>()
            .is_some_and(|s| s.focus() == Some(props.slot));
        if let Event::KeyDown(k) = event {
            if has_focus && !k.modifiers.logo && !k.modifiers.control && !k.modifiers.alt {
                let (flipped, has_card) = with::<Review, _>(&props, |r| (r.flipped(), r.current().is_some()))
                    .unwrap_or((false, false));
                let digit = match k.key_code {
                    KeyCode::Key0 => Some(0),
                    KeyCode::Key1 => Some(1),
                    KeyCode::Key2 => Some(2),
                    KeyCode::Key3 => Some(3),
                    KeyCode::Key4 => Some(4),
                    KeyCode::Key5 => Some(5),
                    _ => None,
                };
                let flip_key = matches!(k.key_code, KeyCode::ReturnKey | KeyCode::NumpadEnter | KeyCode::Space);
                if has_card {
                    if let Some(s) = scope.data.get_mut::<Session>() {
                        if !flipped && flip_key {
                            with::<Review, _>(&props, |r| r.flip(s));
                            self.view.redraw(cx);
                            return;
                        }
                        if flipped {
                            if let Some(q) = digit {
                                with::<Review, _>(&props, |r| r.grade(q, s));
                                self.view.redraw(cx);
                                return;
                            }
                        }
                    }
                }
            }
        }
        if let Event::MouseDown(e) = event {
            if e.button == MouseButton::PRIMARY
                && self.card_rect.is_some_and(|r| r.contains(e.abs))
                && props.hits.at(e.abs).is_some_and(|h| h.slot == Some(props.slot))
            {
                if let Some(s) = scope.data.get_mut::<Session>() {
                    with::<Review, _>(&props, |r| r.flip(s));
                }
                self.view.redraw(cx);
                return;
            }
        }
        self.view.handle_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let now = scope
            .data
            .get::<Session>()
            .map_or(kernel::time::virtual_epoch(), |s| s.now());
        let Some((card, pos, total, flipped, finished, outcome, split, store)) = with::<Review, _>(&props, |r| {
            r.tick(now);
            (r.current().cloned(), r.pos(), r.total(), r.flipped(), r.finished(), r.outcome(), r.split(), r.store().clone())
        }) else {
            return self.view.draw_walk(cx, scope, walk);
        };

        let count = if total == 0 {
            "NOTHING DUE".to_string()
        } else if finished {
            format!("{total} CARDS")
        } else {
            format!("CARD {} OF {total}", pos + 1)
        };
        self.view.label(cx, ids!(head.count_lbl)).set_text(cx, &count);
        self.view.label(cx, ids!(head.split_lbl)).set_text(
            cx,
            &if total == 0 { String::new() } else { format!("{} NEW · {} REVIEW", split.0, split.1) },
        );

        let card_view = self.view.widget(cx, ids!(card));
        card_view.set_visible(cx, card.is_some());
        if let Some(c) = &card {
            card_view.label(cx, ids!(front_txt)).set_text(cx, &c.front);
            for (path, text, show) in [
                (ids!(back_txt) as &[LiveId], c.back.clone(), flipped),
                (ids!(example_txt), c.example.clone(), flipped && !c.example.is_empty()),
                (ids!(notes_lbl), c.notes.clone(), flipped && !c.notes.is_empty()),
            ] {
                let l = card_view.label(cx, path);
                l.set_visible(cx, show);
                l.set_text(cx, &text);
            }
        }
        let fin = self.view.widget(cx, ids!(finished));
        fin.set_visible(cx, finished);
        if finished {
            let (graded, right, minutes) = outcome;
            fin.label(cx, ids!(stats_lbl))
                .set_text(cx, &format!("{graded} cards · {right} richtig · {minutes} min"));
        }
        let empty = self.view.widget(cx, ids!(empty));
        empty.set_visible(cx, total == 0);
        if total == 0 {
            let next = model::next_due(&store, now)
                .map_or("nothing scheduled — play a lesson to grow the deck".to_string(), |d| {
                    format!("next card: {}, {}", model::weekday(d), model::relative_day(d, now))
                });
            empty.label(cx, ids!(next_lbl)).set_text(cx, &next);
        }

        let step = self.view.draw_walk(cx, scope, walk);
        self.card_rect = None;
        if card.is_some() {
            let r = card_view.area().rect(cx);
            if r.size.x > 0.0 {
                props.hits.add("card", r, MouseCursor::Hand, props.slot);
                self.card_rect = Some(r);
            }
            for path in [ids!(front_txt) as &[LiveId], ids!(back_txt), ids!(example_txt)] {
                let l = card_view.label(cx, path);
                if l.visible() {
                    text_hit(cx, &props, &l, None);
                }
            }
        }
        for (w, path) in [(&fin, ids!(done_lbl) as &[LiveId]), (&fin, ids!(stats_lbl)), (&empty, ids!(nothing_lbl)), (&empty, ids!(next_lbl))] {
            if w.visible() {
                let l = w.label(cx, path);
                text_hit(cx, &props, &l, None);
            }
        }
        step
    }
}
