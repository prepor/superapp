//! Progress, drawn.

use kernel::session::Session;
use makepad_widgets::*;

use crate::shell::hosted::PanelProps;

use super::super::model::{self, percent, relative_day};
use super::super::panels::Progress;
use super::{set_level, set_stars, text_hit, with};

#[derive(Script, ScriptHook, Widget)]
pub struct ProgressPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
}

impl Widget for ProgressPanel {
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
        let Some(store) = with::<Progress, _>(&props, |p| {
            p.tick(now);
            p.store().clone()
        }) else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let learner = model::learner(&store);
        let activity = model::activity(&store, now);
        let skills = model::skills(&store);
        let recent = model::recent_accuracy(&store, 10);
        let mistakes = model::top_mistakes(&store, 3);
        let (learned, this_week) = model::words(&store, now);
        let (due_all, due_cards) = model::due_counts(&store, now);
        let daily = learner.as_ref().map_or(30.0, |l| l.daily_minutes as f64);

        let mut body: Option<WidgetRef> = None;
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            let list_ref = item.as_portal_list();
            let Some(mut list) = list_ref.borrow_mut() else {
                continue;
            };
            list.set_item_range(cx, 0, 1);
            while let Some(i) = list.next_visible_item(cx) {
                if i != 0 {
                    continue;
                }
                let row = list.item(cx, i, live_id!(body));
                let (streak, note) = match &learner {
                    Some(l) if l.streak > 0 && l.streak_active(now) => (
                        format!("{} days", l.streak),
                        "the streak is alive — today keeps it".to_string(),
                    ),
                    Some(l) if l.streak > 0 => (format!("{} days", l.streak), "paused — today revives it".to_string()),
                    _ => ("no streak yet".to_string(), "play a lesson to start one".to_string()),
                };
                row.label(cx, ids!(streak_lbl)).set_text(cx, &streak);
                row.label(cx, ids!(streak_note)).set_text(cx, &note);

                let strip = row.view(cx, ids!(strip));
                let weeks = [live_id!(w0), live_id!(w1), live_id!(w2), live_id!(w3), live_id!(w4), live_id!(w5), live_id!(w6), live_id!(w7)];
                let days = [live_id!(d0), live_id!(d1), live_id!(d2), live_id!(d3), live_id!(d4), live_id!(d5), live_id!(d6)];
                for (w, wid) in weeks.iter().enumerate() {
                    for (d, did) in days.iter().enumerate() {
                        let minutes = activity.get(w * 7 + d).copied().unwrap_or(0.0);
                        let level = if minutes <= 0.0 { 0.0 } else { (0.25 + 0.75 * (minutes / daily)).min(1.0) };
                        let cell = strip.view(cx, &[*wid, *did]);
                        set_level(cx, &cell, level);
                    }
                }

                for (n, path) in [ids!(sk0), ids!(sk1), ids!(sk2), ids!(sk3), ids!(sk4)].into_iter().enumerate() {
                    let r = row.widget(cx, path);
                    let Some(sk) = skills.get(n) else {
                        r.set_visible(cx, false);
                        continue;
                    };
                    r.set_visible(cx, true);
                    r.label(cx, ids!(name_lbl)).set_text(cx, &sk.name);
                    let stars = r.view(cx, ids!(stars));
                    set_stars(cx, &stars, sk.mastery);
                    r.label(cx, ids!(val_lbl))
                        .set_text(cx, &format!("{}/5 · {} · {} lessons", sk.mastery, percent(sk.accuracy), sk.lessons));
                }

                let bars = row.view(cx, ids!(bars));
                let ids = [live_id!(b0), live_id!(b1), live_id!(b2), live_id!(b3), live_id!(b4), live_id!(b5), live_id!(b6), live_id!(b7), live_id!(b8), live_id!(b9)];
                let offset = 10usize.saturating_sub(recent.len());
                for (n, id) in ids.iter().enumerate() {
                    let v = bars.view(cx, &[*id]);
                    match n.checked_sub(offset).and_then(|k| recent.get(k)) {
                        Some((_, acc)) => {
                            v.set_visible(cx, true);
                            set_level(cx, &v, *acc);
                        }
                        None => v.set_visible(cx, false),
                    }
                }
                let acc_line = match recent.as_slice() {
                    [] => "no lesson played yet".to_string(),
                    all => {
                        let last = all[all.len() - 1].1;
                        let best = all.iter().map(|(_, a)| *a).fold(0.0, f64::max);
                        let mean = all.iter().map(|(_, a)| a).sum::<f64>() / all.len() as f64;
                        format!("{} last · {} mean · {} best", percent(last), percent(mean), percent(best))
                    }
                };
                row.label(cx, ids!(acc_lbl)).set_text(cx, &acc_line);

                for (n, path) in [ids!(e0), ids!(e1), ids!(e2)].into_iter().enumerate() {
                    let r = row.widget(cx, path);
                    let Some(m) = mistakes.get(n) else {
                        r.set_visible(cx, false);
                        continue;
                    };
                    r.set_visible(cx, true);
                    r.label(cx, ids!(id_lbl))
                        .set_text(cx, &format!("{} · {}× · mastery {}/5", m.id.replace('_', " "), m.frequency, m.mastery));
                    let last = m.last.map_or(String::new(), |t| format!(" · last {}", relative_day(t, now)));
                    r.label(cx, ids!(ex_lbl))
                        .set_text(cx, &format!("{} → {}{last}", m.wrong, m.right));
                }
                row.label(cx, ids!(words_lbl)).set_text(
                    cx,
                    &format!(
                        "{learned} learned · +{this_week} this week · {due_cards} cards due · {} grammar due",
                        due_all - due_cards
                    ),
                );
                row.draw_all(cx, scope);
                body = Some(row);
            }
        }
        if let Some(row) = body {
            let clip = self.view.widget(cx, ids!(list)).area().rect(cx);
            for path in [ids!(streak_lbl), ids!(streak_note), ids!(acc_lbl), ids!(words_lbl)] {
                let l = row.label(cx, path);
                text_hit(cx, &props, &l, Some(clip));
            }
            for path in [ids!(sk0), ids!(sk1), ids!(sk2), ids!(sk3), ids!(sk4)] {
                let r = row.widget(cx, path);
                if r.visible() {
                    let l = r.label(cx, ids!(name_lbl));
                    text_hit(cx, &props, &l, Some(clip));
                }
            }
            for path in [ids!(e0), ids!(e1), ids!(e2)] {
                let r = row.widget(cx, path);
                if r.visible() {
                    let l = r.label(cx, ids!(id_lbl));
                    text_hit(cx, &props, &l, Some(clip));
                }
            }
        }
        DrawStep::done()
    }
}
