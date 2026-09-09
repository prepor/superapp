//! The attach panel, drawn: the files the composer will send, one row each
//! under a `CARRIES` caption with the cursor's wash; or, while a recording
//! runs, the strip alone — what and how long, the level under it, the
//! camera's picture for a video message. One thing at a time: a voice note
//! or a video message is a message of its own, so the list gives way while
//! one is being made and comes back when it has gone.
//!
//! Presses are answered by the row rectangles of the last draw, matched
//! against the shell's own hit, as the transcript's are.

use kernel::nav::Nav;
use kernel::session::Session;
use makepad_widgets::*;

use crate::shell::hosted::PanelProps;
use crate::shell::widgets::media;
use crate::shell::widgets::table;

use super::super::model::{self, Carried, RecKind};
use super::super::panels::Attach;
use super::super::seed;

/// The children the panel expects in its template.
const LEAD: &[LiveId] = ids!(lead_lbl);
const EMPTY: &[LiveId] = ids!(empty_lbl);
const GONE: &[LiveId] = ids!(gone_lbl);
const LIST_WRAP: &[LiveId] = ids!(list_wrap);
const LIST: &[LiveId] = ids!(list_wrap.list);
const RECORDING: &[LiveId] = ids!(recording);
const REC_LINE: &[LiveId] = ids!(recording.rec_lbl);
const REC_KEYS: &[LiveId] = ids!(recording.keys_lbl);
const REC_METER: &[LiveId] = ids!(recording.meter);
const REC_PREVIEW: &[LiveId] = ids!(recording.preview);

/// Where one row of the last draw landed.
struct RowHit {
    idx: usize,
    rect: Rect,
}

/// The widget.
#[derive(Script, ScriptHook, Widget)]
pub struct AttachPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[rust]
    rows: Vec<RowHit>,
    /// The recording strip's camera preview has been drawn.
    #[rust]
    previewed: bool,
    /// One pending refresh; a visible recording draw renews it. A closed or
    /// hidden strip leaves no repeating timer behind.
    #[rust]
    recording_timer: Timer,
}

impl Widget for AttachPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return;
        };
        observe(&props, scope);
        let recording = with_attach(&props, |a| a.recording().is_some()).unwrap_or(false);
        if self.recording_timer.is_event(event).is_some() {
            self.recording_timer = Timer::default();
            if recording { self.view.redraw(cx); }
        }

        if let Event::KeyDown(k) = event {
            match k.key_code {
                // The arrows walk the rows; the cursor feeds the bar.
                KeyCode::ArrowDown | KeyCode::ArrowUp => {
                    let d: isize = if k.key_code == KeyCode::ArrowDown { 1 } else { -1 };
                    with_attach(&props, |a| a.walk(d));
                    self.view.redraw(cx);
                    if let Some(s) = scope.data.get_mut::<Session>() {
                        s.redraw();
                    }
                }
                // Esc throws a recording away; enter sends it.
                KeyCode::Escape if recording => {
                    with_attach(&props, Attach::cancel_recording);
                    self.view.redraw(cx);
                    if let Some(s) = scope.data.get_mut::<Session>() {
                        s.redraw();
                    }
                }
                KeyCode::ReturnKey | KeyCode::NumpadEnter if recording => {
                    if let Some(s) = scope.data.get_mut::<Session>() {
                        let now = s.now();
                        let mut borrow = props.panel.borrow_mut();
                        if let Some(a) = borrow.as_any().downcast_mut::<Attach>() {
                            a.send_recording(s, now);
                        }
                    }
                    self.view.redraw(cx);
                }
                _ => {}
            }
        }

        self.view.handle_event(cx, event, scope);

        // A press on a row puts the cursor there and takes the panel with
        // it — matched against the shell's own hit, as the transcript's.
        if let Event::MouseDown(e) = event {
            let hit = props
                .hits
                .at(e.abs)
                .filter(|h| h.slot == Some(props.slot))
                .map(|h| h.rect);
            if let Some(idx) = hit.and_then(|rect| self.row_at(rect)) {
                with_attach(&props, |a| a.set_cursor(idx));
                self.view.redraw(cx);
                if let Some(s) = scope.data.get_mut::<Session>() {
                    s.nav(Nav::Focus(props.slot));
                    s.redraw();
                }
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        observe(&props, scope);
        let now = super::now(scope);
        let Some((items, cursor, joined, recording)) = with_attach(&props, |a| {
            (a.items().to_vec(), a.cursor(), a.joined(), a.recording())
        }) else {
            return self.view.draw_walk(cx, scope, walk);
        };

        let v = &self.view;
        let listing = joined && recording.is_none();
        v.label(cx, GONE).set_visible(cx, !joined);
        v.label(cx, LEAD).set_visible(cx, listing && !items.is_empty());
        v.label(cx, EMPTY).set_visible(cx, listing && items.is_empty());
        v.view(cx, LIST_WRAP).set_visible(cx, listing);

        // The strip, while a recording runs: what and how long with the
        // keys on the line, the level, and the camera's picture — faked by
        // the one picture the demo world has of a garden.
        let rec_line = recording.map(|r| r.line(now));
        v.view(cx, RECORDING).set_visible(cx, recording.is_some());
        if let Some(r) = recording {
            v.label(cx, REC_LINE)
                .set_text(cx, rec_line.as_deref().unwrap_or(""));
            v.label(cx, REC_KEYS).set_text(cx, model::Recording::keys());
            let meter = v.widget(cx, REC_METER);
            media::fill_meter(cx, &meter, media::fake_level(r.elapsed(now)));
            let preview = v.widget(cx, REC_PREVIEW);
            if r.kind == RecKind::Video {
                let fresh = !self.previewed;
                self.previewed = true;
                media::fill_picture(cx, &preview, seed::demo_bytes("demo:garden"), fresh);
            } else {
                preview.set_visible(cx, false);
            }
        } else {
            self.previewed = false;
        }

        let n = items.len();
        let mut drawn: Vec<(usize, WidgetRef)> = Vec::new();
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            let list_ref = item.as_portal_list();
            let Some(mut list) = list_ref.borrow_mut() else {
                continue;
            };
            list.set_item_range(cx, 0, n);
            while let Some(idx) = list.next_visible_item(cx) {
                let Some(c) = items.get(idx) else { continue };
                let row = list.item(cx, idx, live_id!(row));
                populate(cx, &row, c, cursor == Some(idx));
                row.draw_all(cx, scope);
                drawn.push((idx, row));
            }
        }

        // The hits: every row by its name and what it goes as, and the
        // lines that stand where rows would.
        self.rows.clear();
        let clip = self.view.widget(cx, LIST).area().rect(cx);
        for (idx, row) in drawn {
            let Some(rect) = props.hits.add_row_clipped(
                items[idx].label(), row.area().rect(cx), clip, MouseCursor::Hand, props.slot,
            ) else {
                continue;
            };
            self.rows.push(RowHit { idx, rect });
        }
        let lines: [(String, &[LiveId]); 4] = [
            (self.view.label(cx, LEAD).text(), LEAD),
            (self.view.label(cx, EMPTY).text(), EMPTY),
            (self.view.label(cx, GONE).text(), GONE),
            (rec_line.unwrap_or_default(), REC_LINE),
        ];
        for (label, path) in lines {
            if label.is_empty() {
                continue;
            }
            let r = self.view.widget(cx, path).area().rect(cx);
            if r.size.x > 0.0 && r.size.y > 0.0 {
                props.hits.add(label, r, MouseCursor::Default, props.slot);
            }
        }
        // Native windows sleep between changes. Refresh the elapsed label
        // once per second without depending on another control's animation.
        if recording.is_some() {
            if self.recording_timer.0 == 0 { self.recording_timer = cx.start_timeout(1.0); }
        } else if self.recording_timer.0 != 0 {
            cx.stop_timer(self.recording_timer);
            self.recording_timer = Timer::default();
        }
        DrawStep::done()
    }
}

impl AttachPanel {
    /// The row whose rectangle the shell's hit is, by the rectangles of the
    /// last draw.
    fn row_at(&self, hit: Rect) -> Option<usize> {
        self.rows
            .iter()
            .rev()
            .find(|r| same_rect(r.rect, hit))
            .map(|r| r.idx)
    }
}

/// Fills one row: the name, and under it what it goes as and where it is.
/// Public because the library draws a fixture through the very same
/// function.
pub fn populate(cx: &mut Cx, row: &WidgetRef, c: &Carried, selected: bool) {
    let line = table::line(cx, row, selected, false);
    line.label(cx, ids!(body.name_lbl)).set_text(cx, c.name());
    line.label(cx, ids!(body.detail_lbl))
        .set_text(cx, &c.detail());
}

/// Whether two rectangles are the one rectangle, give or take a hair.
fn same_rect(a: Rect, b: Rect) -> bool {
    (a.pos.x - b.pos.x).abs() < 0.5
        && (a.pos.y - b.pos.y).abs() < 0.5
        && (a.size.x - b.size.x).abs() < 0.5
        && (a.size.y - b.size.y).abs() < 0.5
}

/// Runs `f` on the instance. The borrow lasts exactly as long as the call.
fn with_attach<R>(props: &PanelProps, f: impl FnOnce(&mut Attach) -> R) -> Option<R> {
    let mut borrow = props.panel.borrow_mut();
    let a = borrow.as_any().downcast_mut::<Attach>()?;
    Some(f(a))
}

/// Hands the instance what it cannot ask for itself while it builds its
/// bar: the chat's list through the join, and what the files app holds.
fn observe(props: &PanelProps, scope: &mut Scope) {
    let Some(session) = scope.data.get_mut::<Session>() else {
        return;
    };
    let session: &Session = session;
    let mut borrow = props.panel.borrow_mut();
    if let Some(a) = borrow.as_any().downcast_mut::<Attach>() {
        a.observe(session);
    }
}
