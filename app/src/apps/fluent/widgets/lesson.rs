//! The player, drawn: one exercise on the stage, or the summary.

use kernel::session::Session;
use makepad_widgets::*;

use crate::shell::hosted::PanelProps;

use super::super::model::{self, Closed, Exercise};
use super::super::panels::{Lesson, Phase};
use super::{set_level, text_hit, with};

/// What one draw reads off the instance, so the borrow is over before
/// anything is drawn.
struct Shown {
    phase: Phase,
    index: usize,
    total: usize,
    done: usize,
    ex: Option<Exercise>,
    exercises: std::rc::Rc<Vec<Exercise>>,
    choice: Option<usize>,
    hints_shown: i64,
    row: Option<model::LessonRow>,
    outcome: (usize, usize, f64),
    streak: Option<i64>,
    building: bool,
}

#[derive(Script, ScriptHook, Widget)]
pub struct LessonPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    /// The choices' rectangles of the last draw, for a press.
    #[rust]
    choice_rects: Vec<Option<Rect>>,
    /// Which exercise the field was last cleared for.
    #[rust]
    cleared_for: Option<i64>,
    /// The frame asked for while the caret is still on its way.
    #[rust]
    next_frame: NextFrame,
}

impl LessonPanel {
    fn fields(&self, cx: &mut Cx) -> (TextInputRef, TextInputRef) {
        let stage = self.view.widget(cx, ids!(list)).as_portal_list();
        let row = stage.get_item(0).map(|(_, w)| w).unwrap_or_default();
        (
            row.text_input(cx, ids!(field_wrap.answer_input)),
            row.text_input(cx, ids!(editor_wrap.editor_input)),
        )
    }

    /// Hands the keyboard from a field back to the panel, so plain keys
    /// reach the exercise and not a hidden field.
    fn leave_field(&self, cx: &mut Cx) {
        cx.set_key_focus(self.view.area());
    }
}

impl Widget for LessonPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return;
        };
        let (answer, editor) = self.fields(cx);
        let (phase, kind, typed_kind, choices, has_audio) = with::<Lesson, _>(&props, |p| {
            let ex = p.current();
            (
                p.phase(),
                ex.as_ref().map(|e| e.kind.clone()).unwrap_or_default(),
                ex.as_ref().is_some_and(Exercise::typed),
                ex.as_ref().map_or(0, |e| e.choices.len()),
                ex.as_ref().is_some_and(|e| !e.audio.is_empty()),
            )
        })
        .unwrap_or((Phase::Done, String::new(), false, 0, false));
        // The keys are the focused panel's: the shell forwards them there
        // and nowhere else. The session's focus is what says so — a mount
        // replaying in the library is focused without owning the window's
        // keyboard.
        let has_focus = scope
            .data
            .get::<Session>()
            .is_some_and(|s| s.focus() == Some(props.slot));
        let mut in_field = focused(cx, &answer) || focused(cx, &editor);
        // While the panel has the keyboard and the exercise is a typed one,
        // the caret belongs in the field: a press on the bar takes it away,
        // and this puts it back on the next event. A focus set inside a
        // press is undone by the press's own handling, so it is asked for
        // on every event until it has taken, and the draw keeps events
        // coming until then.
        if has_focus && phase == Phase::Answering && typed_kind && !in_field {
            let field = if kind == "free_write" { &editor } else { &answer };
            field.set_key_focus(cx);
            in_field = focused(cx, field);
        }

        if let Event::KeyDown(k) = event {
            if has_focus && !k.modifiers.logo && !k.modifiers.control && !k.modifiers.alt {
                let plain_digit = match k.key_code {
                    KeyCode::Key0 => Some(0),
                    KeyCode::Key1 => Some(1),
                    KeyCode::Key2 => Some(2),
                    KeyCode::Key3 => Some(3),
                    KeyCode::Key4 => Some(4),
                    KeyCode::Key5 => Some(5),
                    _ => None,
                };
                let enter = matches!(k.key_code, KeyCode::ReturnKey | KeyCode::NumpadEnter);
                let took = match phase {
                    Phase::Answering if typed_kind => {
                        // Enter checks; shift+enter is the editor's own newline.
                        if enter && !k.modifiers.shift && in_field {
                            let text = if kind == "free_write" { editor.text() } else { answer.text() };
                            with::<Lesson, _>(&props, |p| p.set_typed(text));
                            if let Some(s) = scope.data.get_mut::<Session>() {
                                with::<Lesson, _>(&props, |p| p.submit(s));
                            }
                            self.leave_field(cx);
                            true
                        } else {
                            false
                        }
                    }
                    Phase::Answering => match (k.key_code, plain_digit) {
                        (_, Some(d)) if d >= 1 && (d as usize) <= choices => {
                            if let Some(s) = scope.data.get_mut::<Session>() {
                                with::<Lesson, _>(&props, |p| p.choose(d as usize - 1, s));
                            }
                            true
                        }
                        (KeyCode::ArrowDown, _) | (KeyCode::ArrowUp, _) => {
                            let d = if k.key_code == KeyCode::ArrowDown { 1 } else { -1 };
                            if let Some(s) = scope.data.get_mut::<Session>() {
                                with::<Lesson, _>(&props, |p| p.move_choice(d, s));
                            }
                            true
                        }
                        (KeyCode::Space, _) if has_audio && !in_field => {
                            if let Some(s) = scope.data.get_mut::<Session>() {
                                with::<Lesson, _>(&props, |p| p.play(s));
                            }
                            true
                        }
                        _ if enter => {
                            if let Some(s) = scope.data.get_mut::<Session>() {
                                with::<Lesson, _>(&props, |p| {
                                    if let Some(c) = p.choice() {
                                        p.choose(c, s);
                                    }
                                });
                            }
                            true
                        }
                        _ => false,
                    },
                    Phase::Feedback(_) if enter => {
                        if let Some(s) = scope.data.get_mut::<Session>() {
                            with::<Lesson, _>(&props, |p| p.advance(s));
                        }
                        true
                    }
                    Phase::SelfGrade => match plain_digit {
                        Some(q) => {
                            if let Some(s) = scope.data.get_mut::<Session>() {
                                with::<Lesson, _>(&props, |p| p.grade(q, s));
                            }
                            true
                        }
                        None => false,
                    },
                    _ => false,
                };
                if took {
                    self.view.redraw(cx);
                    return;
                }
            }
        }

        if let Event::MouseDown(e) = event {
            if e.button == MouseButton::PRIMARY && phase == Phase::Answering {
                let mine = props.hits.at(e.abs).is_some_and(|h| h.slot == Some(props.slot));
                if let Some(i) = self
                    .choice_rects
                    .iter()
                    .position(|r| r.is_some_and(|r| r.contains(e.abs)))
                    .filter(|_| mine)
                {
                    if let Some(s) = scope.data.get_mut::<Session>() {
                        with::<Lesson, _>(&props, |p| p.choose(i, s));
                    }
                    self.view.redraw(cx);
                    return;
                }
            }
        }

        self.view.handle_event(cx, event, scope);

        if let Event::Actions(actions) = event {
            for field in [&answer, &editor] {
                if let Some(text) = field.changed(actions) {
                    with::<Lesson, _>(&props, |p| p.set_typed(text));
                }
            }
        }

    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let now = scope
            .data
            .get::<Session>()
            .map_or(kernel::time::virtual_epoch(), |s| s.now());
        let Some(shown) = with::<Lesson, _>(&props, |p| {
            p.tick(now);
            let exercises = p.exercises();
            let (done, total) = p.progress();
            Shown {
                phase: p.phase(),
                index: p.index(),
                total,
                done,
                ex: p.current(),
                exercises,
                choice: p.choice(),
                hints_shown: p.hints_shown(),
                row: p.row(),
                outcome: p.outcome(),
                streak: None,
                building: false,
            }
        }) else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let store = scope.data.get::<Session>().map(|s| s.store().clone());
        let streak = store.as_ref().and_then(|st| model::learner(st)).map(|l| l.streak);
        let building = store
            .as_ref()
            .and_then(|st| model::shelf(st))
            .is_some_and(|s| s.status == "building");
        let shown = Shown { streak, building, ..shown };

        // The head: the caption and the count, and the hairline.
        let (caption, count) = match (&shown.ex, shown.phase) {
            (Some(ex), Phase::Done) | (Some(ex), _) if shown.phase != Phase::Done => {
                (ex.caption(), format!("{} OF {}", shown.index + 1, shown.total))
            }
            _ => (
                "SUMMARY".to_string(),
                shown.row.as_ref().map_or(String::new(), |r| model::fmt_day(r.for_date).to_uppercase()),
            ),
        };
        self.view.label(cx, ids!(head.caption_lbl)).set_text(cx, &caption);
        self.view.label(cx, ids!(head.count_lbl)).set_text(cx, &count);
        let fraction = if shown.total == 0 { 1.0 } else { shown.done as f64 / shown.total as f64 };
        if let Some(mut v) = self.view.view(cx, ids!(progress)).borrow_mut() {
            v.draw_bg.set_uniform(cx, live_id!(progress), &[fraction as f32]);
        }

        // In a feedback phase the field must not keep the keyboard: plain
        // keys are the exercise's.
        if shown.phase != Phase::Answering {
            let (answer, editor) = self.fields(cx);
            if focused(cx, &answer) || focused(cx, &editor) {
                self.leave_field(cx);
            }
        }

        // What the list holds: the stage, or the summary and its lines.
        let corrections: Vec<&Exercise> = if shown.phase == Phase::Done {
            shown.exercises.iter().filter(|e| is_correction(e)).collect()
        } else {
            Vec::new()
        };
        let items = if shown.phase == Phase::Done { 1 + corrections.len() + 1 } else { 1 };

        let mut rows: Vec<(usize, WidgetRef)> = Vec::new();
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            let list_ref = item.as_portal_list();
            let Some(mut list) = list_ref.borrow_mut() else {
                continue;
            };
            list.set_item_range(cx, 0, items);
            while let Some(i) = list.next_visible_item(cx) {
                if i >= items {
                    continue;
                }
                let row = if shown.phase != Phase::Done {
                    let row = list.item(cx, i, live_id!(stage));
                    if let Some(ex) = &shown.ex {
                        self.populate_stage(cx, &row, ex, &shown);
                    }
                    row
                } else if i == 0 {
                    let row = list.item(cx, i, live_id!(summary));
                    populate_summary(cx, &row, &shown, corrections.is_empty());
                    row
                } else if i <= corrections.len() {
                    let row = list.item(cx, i, live_id!(fix));
                    populate_fix(cx, &row, corrections[i - 1]);
                    row
                } else {
                    let row = list.item(cx, i, live_id!(building));
                    row.label(cx, ids!(lbl)).set_text(
                        cx,
                        if shown.building {
                            "building tomorrow's lesson — the tutor grades your writing and authors the next one"
                        } else {
                            "the tutor's notes above are what tomorrow's lesson was built from"
                        },
                    );
                    row
                };
                row.draw_all(cx, scope);
                rows.push((i, row));
            }
        }

        // Hits: what a script asserts on and what a pointer presses.
        let clip = self.view.widget(cx, ids!(list)).area().rect(cx);
        self.choice_rects = vec![None; 4];
        for (i, row) in &rows {
            if shown.phase != Phase::Done {
                for path in [
                    ids!(prompt_txt) as &[LiveId],
                    ids!(passage_box.passage_txt),
                    ids!(transcript_txt),
                    ids!(h0),
                    ids!(h1),
                    ids!(h2),
                    ids!(verdict.head_lbl),
                    ids!(verdict.key_txt),
                    ids!(verdict.expl_txt),
                    ids!(model_box.model_txt),
                    ids!(model_box.mine_txt),
                ] {
                    let l = row.label(cx, path);
                    if l.visible() {
                        text_hit(cx, &props, &l, Some(clip));
                    }
                }
                for (path, label) in [
                    (ids!(field_wrap.answer_input) as &[LiveId], "answer"),
                    (ids!(editor_wrap.editor_input), "answer"),
                ] {
                    let w = row.widget(cx, path);
                    if w.visible() {
                        props.hits.add_clipped(label, w.area().rect(cx), clip, MouseCursor::Text, props.slot);
                    }
                }
                for (n, path) in [ids!(c0), ids!(c1), ids!(c2), ids!(c3)].into_iter().enumerate() {
                    let w = row.widget(cx, path);
                    if !w.visible() {
                        continue;
                    }
                    let text = w.label(cx, ids!(text_lbl)).text();
                    self.choice_rects[n] = props.hits.add_clipped(
                        format!("{} {text}", n + 1),
                        w.area().rect(cx),
                        clip,
                        MouseCursor::Hand,
                        props.slot,
                    );
                }
            } else if *i == 0 {
                for path in [ids!(done_lbl) as &[LiveId], ids!(stats_lbl), ids!(calib_lbl), ids!(notes_txt)] {
                    let l = row.label(cx, path);
                    if l.visible() {
                        text_hit(cx, &props, &l, Some(clip));
                    }
                }
            } else {
                for path in [ids!(line_txt) as &[LiveId], ids!(note_lbl), ids!(lbl)] {
                    let l = row.label(cx, path);
                    if l.visible() {
                        text_hit(cx, &props, &l, Some(clip));
                    }
                }
            }
        }
        // Until the caret has landed in a typed exercise's field, keep the
        // events coming: the next one puts it there.
        let has_focus = scope
            .data
            .get::<Session>()
            .is_some_and(|s| s.focus() == Some(props.slot));
        if has_focus && shown.phase == Phase::Answering && shown.ex.as_ref().is_some_and(Exercise::typed) {
            let (answer, editor) = self.fields(cx);
            if !focused(cx, &answer) && !focused(cx, &editor) {
                self.next_frame = cx.new_next_frame();
            }
        }
        DrawStep::done()
    }
}

impl LessonPanel {
    fn populate_stage(&mut self, cx: &mut Cx2d, row: &WidgetRef, ex: &Exercise, shown: &Shown) {
        let answering = shown.phase == Phase::Answering;
        let listen = ex.kind == "listen_mcq";

        // The field is cleared once per exercise, so what was typed for
        // the last one never leaks into this one.
        if self.cleared_for != Some(ex.id) {
            row.text_input(cx, ids!(field_wrap.answer_input)).set_text(cx, "");
            row.text_input(cx, ids!(editor_wrap.editor_input)).set_text(cx, "");
            self.cleared_for = Some(ex.id);
        }

        let passage = row.widget(cx, ids!(passage_box));
        passage.set_visible(cx, !ex.passage.is_empty());
        passage.label(cx, ids!(passage_txt)).set_text(cx, &ex.passage);

        let prompt = row.label(cx, ids!(prompt_txt));
        prompt.set_text(cx, if listen && answering { "Was hast du gehört?" } else { &ex.prompt });
        row.widget(cx, ids!(direction_lbl)).set_visible(cx, ex.kind == "translate");
        let transcript = row.label(cx, ids!(transcript_txt));
        transcript.set_visible(cx, listen && !answering);
        transcript.set_text(cx, &format!("„{}“", ex.audio));

        row.widget(cx, ids!(field_wrap))
            .set_visible(cx, answering && ex.typed() && ex.kind != "free_write");
        row.widget(cx, ids!(editor_wrap))
            .set_visible(cx, answering && ex.kind == "free_write");

        for (n, path) in [ids!(c0), ids!(c1), ids!(c2), ids!(c3)].into_iter().enumerate() {
            let w = row.widget(cx, path);
            let Some(choice) = ex.choices.get(n) else {
                w.set_visible(cx, false);
                continue;
            };
            w.set_visible(cx, true);
            w.label(cx, ids!(key.lbl)).set_text(cx, &(n + 1).to_string());
            w.label(cx, ids!(text_lbl)).set_text(cx, choice);
            let picked = ex.answer.as_deref() == Some(choice.as_str()) || (answering && shown.choice == Some(n));
            let right = ex.accepted.contains(choice);
            let (state, mark) = match shown.phase {
                Phase::Answering => (if shown.choice == Some(n) { 1.0 } else { 0.0 }, ""),
                _ if right => (2.0, "answer"),
                _ if picked => (3.0, "yours"),
                _ => (0.0, ""),
            };
            set_level_state(cx, &w.as_view(), state);
            w.label(cx, ids!(mark_lbl)).set_text(cx, mark);
        }

        for (n, path) in [ids!(h0), ids!(h1), ids!(h2)].into_iter().enumerate() {
            let l = row.label(cx, path);
            let show = (n as i64) < shown.hints_shown && n < ex.hints.len();
            l.set_visible(cx, show);
            if show {
                l.set_text(cx, &format!("hint: {}", ex.hints[n]));
            }
        }

        let verdict = row.widget(cx, ids!(verdict));
        match shown.phase {
            Phase::Feedback(g) => {
                verdict.set_visible(cx, true);
                verdict.label(cx, ids!(head_lbl)).set_text(cx, g.headline());
                let wrong = g != Closed::Correct;
                let yours = verdict.label(cx, ids!(yours_lbl));
                yours.set_visible(cx, wrong && ex.choices.is_empty());
                yours.set_text(cx, &format!("you: {}", ex.answer.clone().unwrap_or_default()));
                let key = verdict.label(cx, ids!(key_txt));
                let key_text = ex.key_answer();
                key.set_visible(cx, wrong && ex.choices.is_empty() && !key_text.is_empty());
                key.set_text(cx, &format!("→ {key_text}"));
                let expl = verdict.label(cx, ids!(expl_txt));
                expl.set_visible(cx, !ex.explanation.is_empty());
                expl.set_text(cx, &ex.explanation);
            }
            _ => verdict.set_visible(cx, false),
        }

        let model_box = row.widget(cx, ids!(model_box));
        if shown.phase == Phase::SelfGrade {
            model_box.set_visible(cx, true);
            model_box.label(cx, ids!(model_txt)).set_text(cx, &ex.model);
            let also = model_box.label(cx, ids!(also_lbl));
            let variants: Vec<String> = ex.accepted.iter().filter(|a| **a != ex.model).cloned().collect();
            also.set_visible(cx, !variants.is_empty());
            also.set_text(cx, &format!("also accepted: {}", variants.join(" · ")));
            model_box
                .label(cx, ids!(mine_txt))
                .set_text(cx, ex.answer.as_deref().unwrap_or("—"));
        } else {
            model_box.set_visible(cx, false);
        }
    }
}

/// Whether a field really has the caret. A field that is hidden and has
/// never been drawn has an empty area, and an empty area reads as focused
/// whenever nothing is — so the question is asked of the window first.
fn focused(cx: &Cx, field: &TextInputRef) -> bool {
    cx.key_focus() != Area::Empty && field.key_focus(cx)
}

/// Which exercises the summary lists: a closed one answered wrong or
/// almost, or a self-check one the tutor or the learner graded under 4.
fn is_correction(e: &Exercise) -> bool {
    match e.result.as_deref() {
        Some("wrong" | "almost") => true,
        Some(_) => false,
        None => e.answer.is_some() && (e.tutor_grade.is_some() || e.self_grade.is_some_and(|q| q < 4)),
    }
}

fn populate_summary(cx: &mut Cx2d, row: &WidgetRef, shown: &Shown, clean: bool) {
    let (right, total, minutes) = shown.outcome;
    let acc = if total == 0 { 0.0 } else { right as f64 / total as f64 };
    row.label(cx, ids!(stats_lbl)).set_text(
        cx,
        &format!("{right}/{total} first try · {} · {} min", model::percent(acc), minutes as i64),
    );
    row.label(cx, ids!(streak_lbl)).set_text(
        cx,
        &match shown.streak {
            Some(n) if n > 0 => format!("day {n} of the streak — dranbleiben!"),
            _ => "the first day of a streak".to_string(),
        },
    );
    let pairs: Vec<(i64, i64)> = shown
        .exercises
        .iter()
        .filter_map(|e| Some((e.self_grade?, e.tutor_grade?)))
        .collect();
    let calib = row.label(cx, ids!(calib_lbl));
    calib.set_visible(cx, !pairs.is_empty());
    if !pairs.is_empty() {
        let matched = pairs.iter().filter(|(a, b)| a == b).count();
        let off = pairs.iter().map(|(a, b)| (a - b).abs() as f64).sum::<f64>() / pairs.len() as f64;
        calib.set_text(
            cx,
            &format!("calibration: self vs tutor matched {matched} of {} · avg off {off:.1}", pairs.len()),
        );
    }
    let notes = row.label(cx, ids!(notes_txt));
    let text = shown.row.as_ref().map(|r| r.notes.clone()).unwrap_or_default();
    notes.set_visible(cx, !text.is_empty());
    notes.set_text(cx, &text);
    row.widget(cx, ids!(none_lbl)).set_visible(cx, clean);
}

fn populate_fix(cx: &mut Cx2d, row: &WidgetRef, e: &Exercise) {
    row.label(cx, ids!(q_lbl)).set_text(cx, &format!("Q{} · {}", e.seq, model::kind_word(&e.kind)));
    let yours = e.answer.clone().unwrap_or_default();
    let (line, note) = if let Some(q) = e.tutor_grade {
        let fix = if e.tutor_fix.is_empty() { e.model.clone() } else { e.tutor_fix.clone() };
        let said = e.self_grade.map(|s| format!(" (you said {s})")).unwrap_or_default();
        (
            format!("{yours} → {fix}"),
            format!("tutor {q}/5{said} — {}", e.tutor_note),
        )
    } else if e.result.is_none() {
        (
            format!("{yours} → {}", e.model),
            format!("self-graded {}/5 — the tutor has not checked this one yet", e.self_grade.unwrap_or(0)),
        )
    } else {
        (format!("{yours} → {}", e.key_answer()), e.explanation.clone())
    };
    row.label(cx, ids!(line_txt)).set_text(cx, &line);
    let n = row.label(cx, ids!(note_lbl));
    n.set_visible(cx, !note.trim().is_empty());
    n.set_text(cx, &note);
}

/// A choice's `state`, which shares the uniform mechanism the cells use.
fn set_level_state(cx: &mut Cx, view: &ViewRef, state: f64) {
    if let Some(mut v) = view.borrow_mut() {
        v.draw_bg.set_uniform(cx, live_id!(state), &[state as f32]);
    }
    let _ = set_level;
}
