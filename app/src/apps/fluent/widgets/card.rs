//! One card, drawn.

use kernel::session::Session;
use makepad_widgets::*;

use crate::shell::hosted::PanelProps;

use super::super::model::{self, relative_day};
use super::super::panels::Card;
use super::super::sm2::WORDS;
use super::{set_stars, text_hit, with};

#[derive(Script, ScriptHook, Widget)]
pub struct CardPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
}

impl Widget for CardPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
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
        let Some((card, reviews)) = with::<Card, _>(&props, |c| (c.card(), c.reviews())) else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let Some(c) = card else {
            self.view.label(cx, ids!(front_txt)).set_text(cx, "no such card");
            for path in [ids!(back_txt) as &[LiveId], ids!(example_txt), ids!(notes_lbl), ids!(sched_lbl)] {
                self.view.label(cx, path).set_text(cx, "");
            }
            return self.view.draw_walk(cx, scope, walk);
        };
        self.view.label(cx, ids!(front_txt)).set_text(cx, &c.front);
        self.view.label(cx, ids!(back_txt)).set_text(cx, &c.back);
        self.view.label(cx, ids!(example_txt)).set_text(cx, &c.example);
        self.view.label(cx, ids!(notes_lbl)).set_text(cx, &c.notes);
        let due = if c.reps == 0 {
            "new — due today".to_string()
        } else {
            format!("due {}", relative_day(c.due, now))
        };
        self.view.label(cx, ids!(sched_lbl)).set_text(
            cx,
            &format!("ease {:.2} · {} reviews · {due} · mastery {}/5", c.ease, reviews.len(), c.mastery),
        );
        let stars = self.view.view(cx, ids!(stars));
        set_stars(cx, &stars, c.mastery);
        self.view.widget(cx, ids!(none_lbl)).set_visible(cx, reviews.is_empty());

        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            let list_ref = item.as_portal_list();
            let Some(mut list) = list_ref.borrow_mut() else {
                continue;
            };
            list.set_item_range(cx, 0, reviews.len());
            while let Some(i) = list.next_visible_item(cx) {
                let Some(r) = reviews.get(i) else { continue };
                let row = list.item(cx, i, live_id!(rev));
                row.label(cx, ids!(date_lbl)).set_text(cx, &model::fmt_day(r.at));
                row.label(cx, ids!(q_lbl))
                    .set_text(cx, &format!("{} {}", r.quality, WORDS[r.quality.clamp(0, 5) as usize]));
                row.label(cx, ids!(where_lbl)).set_text(
                    cx,
                    &r.lesson.map_or_else(|| r.device.clone(), |l| format!("lesson {l}")),
                );
                row.draw_all(cx, scope);
            }
        }
        for path in [ids!(front_txt) as &[LiveId], ids!(back_txt), ids!(example_txt), ids!(sched_lbl)] {
            let l = self.view.label(cx, path);
            text_hit(cx, &props, &l, None);
        }
        DrawStep::done()
    }
}
