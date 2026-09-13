//! The desk, drawn.

use makepad_widgets::*;

use crate::shell::hosted::PanelProps;

use super::super::model::{self, percent, relative_day};
use super::super::panels::{Desk, ShelfState};
use super::super::sm2;
use super::{dots, text_hit, tutor_line, with};

#[derive(Script, ScriptHook, Widget)]
pub struct DeskPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
}

impl Widget for DeskPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let now = scope
            .data
            .get::<kernel::session::Session>()
            .map_or(kernel::time::virtual_epoch(), |s| s.now());
        let Some((store, shelf)) = with::<Desk, _>(&props, |d| {
            d.tick(now);
            (d.store().clone(), d.shelf())
        }) else {
            return self.view.draw_walk(cx, scope, walk);
        };

        let learner = model::learner(&store);
        let name = learner.as_ref().map_or("there".to_string(), |l| l.name.clone());
        self.view
            .label(cx, ids!(greet_lbl))
            .set_text(cx, &format!("{}, {name}!", model::greeting(now)));
        let streak = match &learner {
            Some(l) if l.streak_active(now) && l.streak > 0 => {
                format!("day {} of the streak — dranbleiben!", l.streak)
            }
            Some(l) if l.streak > 0 => "streak paused — today revives it".to_string(),
            _ => "a fresh start — today begins the streak".to_string(),
        };
        self.view.label(cx, ids!(streak_lbl)).set_text(cx, &streak);

        let (cap, title, focus, meta, note) = match &shelf {
            ShelfState::Ready(s) => (
                if sm2::days_between(now, s.for_date) <= 0 { "TODAY'S LESSON" } else { "TOMORROW'S LESSON" }.to_string()
                    + if s.started { " · STARTED" } else { " · READY" },
                s.title.clone(),
                dots(&s.focus),
                format!(
                    "{} exercises · ~{} min · {} reviews woven in · for {}",
                    s.exercises,
                    (s.exercises as f64 * 2.2).round() as i64,
                    s.reviews,
                    relative_day(s.for_date, now)
                ),
                String::new(),
            ),
            ShelfState::Stale(s, away) => (
                format!("BUILT FOR {} — YOU'VE BEEN AWAY {away} DAYS", model::fmt_day_month(s.for_date).to_uppercase()),
                s.title.clone(),
                dots(&s.focus),
                format!("{} exercises · {} reviews woven in", s.exercises, s.reviews),
                "play it anyway, or build a fresh one matched to today's reviews".to_string(),
            ),
            ShelfState::Building(s) => (
                if sm2::days_between(now, s.for_date) <= 0 { "TODAY'S LESSON" } else { "TOMORROW'S LESSON" }.to_string()
                    + " · BUILDING",
                String::new(),
                String::new(),
                format!("for {}", relative_day(s.for_date, now)),
                tutor_line(scope.data.get::<kernel::session::Session>(), s.chat),
            ),
            ShelfState::Empty => (
                "NO LESSON ON THE SHELF".to_string(),
                String::new(),
                String::new(),
                String::new(),
                "let's build one now — the one wait in fluent, about a minute".to_string(),
            ),
            ShelfState::Unset => (
                "SET UP THE COURSE".to_string(),
                String::new(),
                String::new(),
                String::new(),
                "who is learning what, and how long a day — the tutor is told all of it"
                    .to_string(),
            ),
        };
        self.view.label(cx, ids!(shelf.shelf_cap)).set_text(cx, &cap);
        let t = self.view.label(cx, ids!(shelf.shelf_title));
        t.set_text(cx, &title);
        t.set_visible(cx, !title.is_empty());
        let f = self.view.label(cx, ids!(shelf.shelf_focus));
        f.set_text(cx, &focus);
        f.set_visible(cx, !focus.is_empty());
        let m = self.view.label(cx, ids!(shelf.shelf_meta));
        m.set_text(cx, &meta);
        m.set_visible(cx, !meta.is_empty());
        let n = self.view.label(cx, ids!(shelf.shelf_note));
        n.set_text(cx, &note);
        n.set_visible(cx, !note.is_empty());

        let (due_all, due_cards) = model::due_counts(&store, now);
        let tile = |cx: &mut Cx2d, view: &View, id: &[LiveId], cap: &str, val: &str, sub: &str| {
            let t = view.widget(cx, id);
            t.label(cx, ids!(cap_lbl)).set_text(cx, cap);
            t.label(cx, ids!(val_lbl)).set_text(cx, val);
            t.label(cx, ids!(sub_lbl)).set_text(cx, sub);
        };
        let new_cards = model::due_cards(&store, now).iter().filter(|c| c.reps == 0).count();
        tile(
            cx,
            &self.view,
            ids!(tiles.due_tile),
            "DUE TODAY",
            &format!("{due_cards} cards"),
            &format!("{new_cards} new · {} grammar", due_all - due_cards),
        );
        let recent = model::recent_accuracy(&store, 5);
        let (acc, sub) = match recent.as_slice() {
            [] => ("—".to_string(), "no lesson played yet".to_string()),
            all => {
                let mean = all.iter().map(|(_, a)| a).sum::<f64>() / all.len() as f64;
                let trend = if all.len() >= 2 {
                    let last = all[all.len() - 1].1;
                    let prev = all[all.len() - 2].1;
                    if last > prev + 0.01 {
                        format!("up from {}", percent(prev))
                    } else if last + 0.01 < prev {
                        format!("down from {}", percent(prev))
                    } else {
                        "steady".to_string()
                    }
                } else {
                    "one lesson".to_string()
                };
                (percent(mean), format!("last {} lessons · {trend}", all.len()))
            }
        };
        tile(cx, &self.view, ids!(tiles.acc_tile), "ACCURACY", &acc, &sub);
        let (learned, this_week) = model::words(&store, now);
        tile(
            cx,
            &self.view,
            ids!(tiles.words_tile),
            "WORDS",
            &format!("{learned} learned"),
            &if this_week > 0 { format!("+{this_week} this week") } else { "keep going".to_string() },
        );

        let step = self.view.draw_walk(cx, scope, walk);
        for path in [ids!(greet_lbl) as &[LiveId], ids!(shelf.shelf_title), ids!(shelf.shelf_cap), ids!(shelf.shelf_note)] {
            let l = self.view.label(cx, path);
            if l.visible() {
                text_hit(cx, &props, &l, None);
            }
        }
        for path in [ids!(tiles.due_tile) as &[LiveId], ids!(tiles.acc_tile), ids!(tiles.words_tile)] {
            let l = self.view.widget(cx, path).label(cx, ids!(val_lbl));
            text_hit(cx, &props, &l, None);
        }
        step
    }
}
