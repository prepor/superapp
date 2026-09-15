//! The attach panel, drawn: the files the composer will send, one row each
//! under a `CARRIES` caption with the cursor's wash — a picture under the
//! name where the file is one; or, while a capture is being made, that
//! alone — what the camera sees, and for a recording the strip under it,
//! what and how long with the level. One thing at a time: a voice note or a
//! video message is a message of its own, so the list gives way while one
//! is being made and comes back when it has gone; the camera's shots go on
//! the list, so it comes back with them on it.
//!
//! The level and the camera are the panel's, which reads them off the
//! capture capability: what the meter draws is what the microphone hears,
//! and the picture is the very camera the recording is made from.
//!
//! Presses are answered by the row rectangles of the last draw, matched
//! against the shell's own hit, as the transcript's are.

use kernel::nav::Nav;
use kernel::session::Session;
use makepad_widgets::*;

use crate::shell::hosted::PanelProps;
use crate::shell::widgets::media;
use crate::shell::widgets::table;

use super::super::model::{self, Carried, Recording};
use super::super::panels::Attach;
use super::pictures;

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
const CAMERA: &[LiveId] = ids!(camera);
const CAM_LINE: &[LiveId] = ids!(camera.cam_lbl);
const CAM_PICTURE: &[LiveId] = ids!(camera.picture);

/// What one draw reads off the panel in one borrow: the list and the
/// cursor, and what the camera and the microphone are doing.
struct Seen {
    items: Vec<Carried>,
    cursor: Option<usize>,
    joined: bool,
    recording: Option<Recording>,
    shooting: bool,
    previewing: bool,
    level: f32,
    /// Whether the camera is open, which the line says…
    camera_open: bool,
    /// …and which camera, where the platform has named one. A run with no
    /// device — every scripted one — is open with none, and the box draws
    /// empty.
    camera: Option<kernel::caps::CameraId>,
}

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
        let (recording, capturing) = with_attach(&props, |a| {
            (a.recording().is_some(), a.recording().is_some() || a.previewing())
        })
        .unwrap_or((false, false));
        if self.recording_timer.is_event(event).is_some() {
            self.recording_timer = Timer::default();
            if capturing { self.view.redraw(cx); }
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
                        let mut borrow = props.panel.borrow_mut();
                        if let Some(a) = borrow.as_any().downcast_mut::<Attach>() {
                            a.send_recording(s);
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
        let Some(Seen {
            items, cursor, joined, recording, shooting, previewing, level, camera_open, camera,
        }) =
            with_attach(&props, |a| Seen {
                items: a.items().to_vec(),
                cursor: a.cursor(),
                joined: a.joined(),
                recording: a.recording(),
                shooting: a.shooting(),
                previewing: a.previewing(),
                level: a.level(),
                camera_open: a.camera_open(),
                camera: a.camera(),
            })
        else {
            return self.view.draw_walk(cx, scope, walk);
        };

        let v = &self.view;
        let listing = joined && recording.is_none() && !shooting;
        v.label(cx, GONE).set_visible(cx, !joined);
        v.label(cx, LEAD).set_visible(cx, listing && !items.is_empty());
        v.label(cx, EMPTY).set_visible(cx, listing && items.is_empty());
        v.view(cx, LIST_WRAP).set_visible(cx, listing);

        // What the camera sees, while it is on: over the strip for a video
        // message, alone for a photograph. A run with no camera in it draws
        // the line and no box — which is every scripted run, where the
        // capture is open and names no camera a widget could be pointed at.
        let cam_line = shooting.then_some(if camera_open {
            "the camera is on"
        } else {
            "asking for the camera…"
        });
        v.view(cx, CAMERA).set_visible(cx, previewing);
        let cam_lbl = v.label(cx, CAM_LINE);
        cam_lbl.set_visible(cx, cam_line.is_some());
        cam_lbl.set_text(cx, cam_line.unwrap_or(""));
        let picture = v.widget(cx, CAM_PICTURE);
        media::show_camera(cx, &picture, camera.filter(|_| previewing));
        // A player is given its texture on a draw on android, and the box
        // stays hidden until there is a picture in it: one empty quad a
        // frame is what breaks the circle.
        media::prime_camera(cx, &picture);

        // The strip, while a recording runs: what and how long with the
        // keys on the line, and the level the microphone is hearing — still
        // once the minute has stopped the capture.
        let rec_line = recording.map(|r| r.line(now));
        v.view(cx, RECORDING).set_visible(cx, recording.is_some());
        if recording.is_some() {
            v.label(cx, REC_LINE)
                .set_text(cx, rec_line.as_deref().unwrap_or(""));
            v.label(cx, REC_KEYS).set_text(cx, model::Recording::keys());
            let meter = v.widget(cx, REC_METER);
            media::fill_meter(cx, &meter, level);
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
        let lines: [(String, &[LiveId]); 5] = [
            (self.view.label(cx, LEAD).text(), LEAD),
            (self.view.label(cx, EMPTY).text(), EMPTY),
            (self.view.label(cx, GONE).text(), GONE),
            (rec_line.unwrap_or_default(), REC_LINE),
            (cam_line.unwrap_or_default().to_string(), CAM_LINE),
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
        if recording.is_some() || previewing {
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

/// Fills one row: the name, under it what it goes as and where it is, and
/// under that the picture itself where the file is one — decoded once and
/// kept in the transcript's own cache, which is what keeps a row that is
/// drawn sixty times a second from reading a file sixty times.
/// Public because the library draws a fixture through the very same
/// function.
pub fn populate(cx: &mut Cx, row: &WidgetRef, c: &Carried, selected: bool) {
    let line = table::line(cx, row, selected, false);
    line.label(cx, ids!(body.name_lbl)).set_text(cx, c.name());
    line.label(cx, ids!(body.detail_lbl))
        .set_text(cx, &c.detail());
    let shot = line.widget(cx, ids!(body.shot));
    let path = (c.kind() == "photo").then(|| kernel::caps::real_path(&c.path));
    pictures::local(cx, &shot, path);
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
