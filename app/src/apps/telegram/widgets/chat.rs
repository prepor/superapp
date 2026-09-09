//! The transcript and the composer, drawn.
//!
//! The rows are the instance's ([`Chat::snapshot`]): a day caption, the unread
//! line, a service line, or a message — the last with a header where a
//! writer's run begins and none where it goes on. One row template carries
//! all four and shows one; the message part hangs its body in the table's
//! four twins, so the cursor wash and the mark bar stay the shell's. What a
//! line carries is drawn through the shell's media kit: a picture, a
//! player over a recording, a map for a place.
//!
//! The composer is a field at the foot. Enter sends and shift+enter breaks
//! a line — answered here, before the field sees the key, because a
//! multiline field's own enter is a newline. A chat that takes focus — by
//! `enter` on its row in the list, by `cmd+arrows`, by a click on it —
//! starts in the field, as the client does; `enter` or `tab` on the lines
//! puts the caret there again, and so do a reply and an edit, from the bar
//! or from the line's card, because both are written; `esc` hands the
//! keyboard back to the lines, whose cursor the arrows walk. Above the
//! field: the reply line, or which line is being edited, and what the
//! composer carries. While the caret is in the field it keeps the text
//! chords and nothing else: `cmd+h` from the caret is still *attach*.
//!
//! Presses are answered by the row rectangles of the last draw: items of a
//! portal list are rebuilt every draw, and a synthesized press has to land
//! the way a finger does. So are the play buttons and the pictures inside
//! them.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use kernel::nav::Nav;
use kernel::panel::{Panel, PanelId, Tag};
use kernel::session::Session;
use makepad_widgets::*;

use crate::shell::dsl::LinkViewExt;
use crate::shell::hosted::PanelProps;
use crate::shell::hits::visible;
use crate::shell::keys::Letters;
use crate::shell::widgets::media::{self, PlayerState, SeekBar};
use crate::shell::widgets::table;
use crate::shell::widgets::reveal::Reveal;

use super::super::model::{self, fmt_count, fmt_hour, state_mark, Msg, MsgKey};
use super::super::panels::{Chat, Line, Row, Viewer};
use super::RenderContext;
use super::inline_video::{self, InlineVideo};

#[cfg(all(test, headless))]
#[path = "chat_scroll_tests.rs"]
mod scroll_tests;

/// The children the transcript expects in its template.
const STATUS: &[LiveId] = ids!(status_lbl);
const EMPTY: &[LiveId] = ids!(empty_lbl);
const LIST: &[LiveId] = ids!(list);
const REPLY_ROW: &[LiveId] = ids!(reply_row);
const REPLY: &[LiveId] = ids!(reply_row.reply_lbl);
const CARRIES: &[LiveId] = ids!(carries);
const COMPOSER: &[LiveId] = ids!(composer);
const INPUT: &[LiveId] = ids!(composer.input);
const CANNOT: &[LiveId] = ids!(cannot_lbl);

/// How many carried files the `CARRIES` line names. Past this it says how
/// many more there are.
const CARRY_SLOTS: usize = 5;
const CARRY_LBLS: [LiveId; CARRY_SLOTS] = [
    live_id!(f0),
    live_id!(f1),
    live_id!(f2),
    live_id!(f3),
    live_id!(f4),
];

/// The card over a path on this machine. Named by tag rather than by app.
const FILE_TAG: Tag = Tag("file");

type InheritedViews = std::collections::BTreeMap<i64, super::super::runtime::MessageView>;

/// Where one message of the last draw landed.
struct RowHit {
    id: MsgKey,
    rect: Rect,
    /// Read acknowledgment measures how much of the full message was seen.
    unclipped: Rect,
}

impl RowHit {
    fn viewed_in(&self, viewport: Rect) -> bool {
        let full = self.unclipped;
        let height = (full.pos.y + full.size.y).min(viewport.pos.y + viewport.size.y)
            - full.pos.y.max(viewport.pos.y);
        // A long message can be taller than the viewport; showing a
        // substantial part of it must still let the reader dismiss it.
        viewport.size.y > 0.0 && height + 1.0 >= full.size.y.min(viewport.size.y * 0.5)
            && full.pos.x >= viewport.pos.x - 1.0
            && full.pos.x + full.size.x <= viewport.pos.x + viewport.size.x + 1.0
    }
}

/// A control inside a row, by the rectangle of the last draw.
struct InnerHit {
    rect: Rect,
    act: Inner,
}

/// What a press inside a row does.
#[derive(Clone)]
enum Inner {
    /// Play or pause in the line.
    Play(Box<Msg>),
    /// Seek this line using the full bar, even when its hit is clipped.
    Seek(Box<Msg>, SeekBar),
    /// Open the line's media in the viewer.
    View(MsgKey),
    /// Open the line's card — a place's ways out are on it.
    Card(MsgKey),
    /// Jump to the line this one answers — a press on the quoted reply, as
    /// on the client.
    Original(MsgKey),
}

/// The widget.
#[derive(Script, ScriptHook, Widget)]
pub struct ChatPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[rust]
    rows: Vec<RowHit>,
    /// The instance that produced the drawn rows. A slot can be replaced
    /// before the shell rebuilds its retained widget on the next draw.
    #[rust]
    drawn_for: Weak<RefCell<Box<dyn Panel>>>,
    #[rust]
    inner: Vec<InnerHit>,
    /// The first look at a live panel has happened: the field took the
    /// draft and, where the panel had focus, the keyboard.
    #[rust]
    mounted: bool,
    #[rust]
    positioned: bool,
    /// Space below a short unread run keeps its divider near the top on
    /// opening. It shrinks as messages arrive and ends when reading reaches
    /// the bottom explicitly, by scrolling down there, sending, or End.
    #[rust]
    unread_space: Option<f64>,
    /// The draft as this widget last wrote it into the field, so the field
    /// is only rewritten when the instance's text moved without a keystroke
    /// of this widget's.
    #[rust]
    shown: String,
    #[rust]
    draft_timer: Timer,
    /// Whether the panel had focus at the last event: the moment it takes
    /// focus is when the caret goes to the composer, once, so a press on a
    /// line — which hands the keyboard to the lines on purpose — is not
    /// undone by the focus its own click brought.
    #[rust]
    had_focus: bool,
    /// A caret asked for and not yet landed: the field is re-asked every
    /// event and frame until it has the keyboard, since a focus set inside
    /// the press that asked is undone by that press's own default.
    #[rust]
    refocus: bool,
    /// Where the last draw stood, as a line rather than an index: the first
    /// message row on screen and the index it had. Rows shift under the
    /// view — a backfill lands a hundred older lines above it at a time —
    /// and an index would then name a different line; the next draw sees
    /// by how much this line moved and moves the list with it.
    #[rust]
    anchor: Option<(MsgKey, usize)>,
    #[rust]
    reveal: Reveal<MsgKey>,
    #[rust]
    dragging_files: bool,
    #[rust]
    viewed: Option<super::super::runtime::MessageView>,
    #[rust]
    background: bool,
    #[rust]
    inherited_viewed: InheritedViews,
    #[rust]
    video: InlineVideo,
    #[rust]
    scrubbing: Option<(Box<Msg>, SeekBar)>,
}

impl Widget for ChatPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        // Direct children keep this stable when the same video is indexed
        // under a visible row by Makepad's single-parent widget search tree.
        let clip_box = self.view.child(live_id!(video_source)).child(live_id!(clip_box));
        let video_before = media::video_word(cx, &clip_box);
        if super::pictures::changed(cx, event) { self.view.redraw(cx); }
        match event {
            Event::WindowLostFocus(_) | Event::Background => {
                self.background = true;
                self.viewed = None;
                self.inherited_viewed.clear();
                self.scrubbing = None;
            }
            Event::WindowGotFocus(_) | Event::Foreground => {
                self.background = false;
                self.view.redraw(cx);
            }
            _ => {}
        }
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return;
        };
        // Row geometry and input belong to the instance last drawn, even
        // when another chat or target reuses this slot and its message ids.
        if !self.drawn_for.ptr_eq(&Rc::downgrade(&props.panel)) {
            self.viewed = None;
            self.inherited_viewed.clear();
            return;
        }
        if self.draft_timer.is_event(event).is_some() {
            self.draft_timer = Timer::default();
            with_chat(&props, Chat::save_pending_draft);
            if let Some(s) = scope.data.get_mut::<Session>() { s.redraw(); }
        }
        if matches!(event, Event::WindowLostFocus(_) | Event::Background | Event::Shutdown) {
            with_chat(&props, Chat::flush_draft);
        }
        let panel_visible = scope.data.get_mut::<Session>()
            .is_some_and(|s| super::message_panel_visible(s, props.slot));
        if !panel_visible {
            self.viewed = None;
            self.inherited_viewed.clear();
        }

        if let Event::Scroll(e) = event {
            self.reveal.cancel();
            let list = self.view.widget(cx, LIST).as_portal_list();
            if self.unread_space.is_some() && e.scroll.y > e.scroll.x.abs()
                && list.area().clipped_rect(cx).contains(e.abs) && list.is_at_end()
            {
                self.unread_space = None;
                list.set_tail_range(true);
                self.anchor = None;
                self.view.redraw(cx);
            }
        }
        if matches!(event, Event::Drag(_) | Event::Drop(_) | Event::DragEnd) {
            match event.drag_hits(cx, self.view.area()) {
                DragHit::Drag(e) => {
                    self.dragging_files = e.state != DragState::Out
                        && e.items
                            .iter()
                            .any(|item| matches!(item, DragItem::FilePath { .. }))
                        && can_post(&props)
                        && !with_chat(&props, |c| c.editing().is_some()).unwrap_or(true);
                    if e.state != DragState::Out {
                        *e.response.lock().unwrap() = if self.dragging_files {
                            DragResponse::Copy
                        } else {
                            DragResponse::None
                        };
                    }
                }
                DragHit::Drop(e) => {
                    self.dragging_files = false;
                    let paths: Vec<String> = e
                        .items
                        .iter()
                        .filter_map(|item| match item {
                            DragItem::FilePath { path, .. } => Some(path.clone()),
                            _ => None,
                        })
                        .collect();
                    if let Some(s) = scope.data.get_mut::<Session>() {
                        with_chat(&props, |c| c.drop_files(s, &paths));
                        s.nav(Nav::Focus(props.slot));
                        self.refocus = can_post(&props);
                    }
                }
                _ => self.dragging_files = false,
            }
            self.view.redraw(cx);
        }

        if let Some(s) = scope.data.get_mut::<Session>() {
            if with_chat(&props, |c| c.poll(s)).unwrap_or(false) {
                self.view.redraw(cx);
                s.redraw();
            }
        }

        let field = self.view.text_input(cx, INPUT);
        // The panel taking focus puts the caret in the composer, the way
        // the client starts in its input: `enter` on the list's row, the
        // walk of `cmd+arrows`, a press on the panel. Once, at the moment
        // it happens — a caret parked on the lines afterwards stays there.
        let has_focus = scope
            .data
            .get_mut::<Session>()
            .is_some_and(|s| s.focus() == Some(props.slot));
        if has_focus && !self.had_focus && self.mounted {
            let can_post = with_chat(&props, |c| c.card().is_none_or(|k| k.can_post()))
                .unwrap_or(false);
            if can_post && !field.key_focus(cx) {
                field.set_key_focus(cx);
            }
        } else if !has_focus && self.had_focus && self.mounted {
            // Leaving the chat sends the draft to the server, as the client
            // does, so the phone shows what was half-written here.
            with_chat(&props, Chat::flush_draft);
        }
        self.had_focus = has_focus;
        if has_focus && self.mounted && panel_visible && !self.background {
            let viewport = self.view.widget(cx, LIST).area().clipped_rect(cx);
            let visible: Vec<MsgKey> = self.rows.iter().filter(|r| r.viewed_in(viewport))
                .map(|r| r.id).collect();
            with_chat(&props, |c| c.view_messages(&visible, super::now(scope)));
        }
        // A reply or an edit asked for the caret — the bar's verb over the
        // cursor, or the line's card through the join — since both are
        // written.
        // A reply, an edit or a press on the composer asked for the caret.
        // The field is re-asked until it has it: a focus set inside the
        // press that put the wish there is undone by that event's own
        // default, so once is not enough — `refocus` holds until the caret
        // has landed.
        if self.mounted && with_chat(&props, Chat::take_field_wish).unwrap_or(false) {
            self.refocus = can_post(&props);
        }
        if self.refocus {
            if field.key_focus(cx) {
                self.refocus = false;
            } else {
                field.set_key_focus(cx);
                self.view.redraw(cx);
            }
        }

        let focused = field.key_focus(cx);
        if let Event::KeyDown(k) = event {
            if has_focus && k.key_code == KeyCode::Escape
                && with_chat(&props, Chat::cancel_reactions).unwrap_or(false)
            {
                self.view.redraw(cx);
                if let Some(s) = scope.data.get_mut::<Session>() {
                    s.redraw();
                }
                return;
            }
            // Ctrl+E reaches the newest line with the transcript focused.
            // The composer's caret chords stay with the field.
            let chat_end = k.key_code == KeyCode::KeyE && k.modifiers.control
                && !(k.modifiers.shift || k.modifiers.alt || k.modifiers.logo);
            if has_focus && !focused
                && (chat_end || matches!(k.key_code, KeyCode::Home | KeyCode::End))
            {
                let to_end = chat_end || k.key_code == KeyCode::End;
                let landed = with_chat(&props, |c| {
                    let hist = c.history();
                    let pick = if to_end {
                        hist.iter().rev().find(|m| !m.service)
                    } else {
                        hist.iter().find(|m| !m.service)
                    };
                    let id = pick.map(|m| m.key())?;
                    c.set_cursor(id);
                    Some(id)
                })
                .flatten();
                if to_end {
                    self.reveal.cancel();
                    self.unread_space = None;
                    let list = self.view.widget(cx, LIST).as_portal_list();
                    list.scroll_to_end(cx);
                    list.set_tail_range(true);
                    self.anchor = None;
                } else if let Some(id) = landed {
                    self.follow(cx, id);
                }
                self.view.redraw(cx);
                if let Some(s) = scope.data.get_mut::<Session>() {
                    s.redraw();
                }
                return;
            }
            if focused {
                match k.key_code {
                    // Enter sends; shift+enter is the field's newline, and
                    // cmd+enter is the workspace's.
                    KeyCode::ReturnKey | KeyCode::NumpadEnter
                        if !k.modifiers.shift && !k.modifiers.logo && !k.modifiers.control =>
                    {
                        self.send(cx, &props, scope);
                        return;
                    }
                    // Up in an empty composer edits my last line, as it does
                    // on the client.
                    KeyCode::ArrowUp
                        if field.text().trim().is_empty()
                            && !with_chat(&props, |c| c.editing().is_some()).unwrap_or(true) =>
                    {
                        let last = with_chat(&props, |c| {
                            c.history().iter().rev().find(|m| m.out && !m.service).map(|m| m.key())
                        })
                        .flatten();
                        if let Some(id) = last {
                            if with_chat(&props, |c| c.edit(id)).unwrap_or(false) {
                                self.view.redraw(cx);
                                if let Some(s) = scope.data.get_mut::<Session>() {
                                    s.redraw();
                                }
                            }
                        }
                        return;
                    }
                    // Esc lets an edit go, then takes the reply line away,
                    // then hands the keyboard back to the rows.
                    KeyCode::Escape => {
                        let (editing, replying) = with_chat(&props, |c| {
                            (c.editing().is_some(), c.reply_to().is_some())
                        })
                        .unwrap_or((false, false));
                        if editing {
                            with_chat(&props, Chat::cancel_edit);
                        } else if replying {
                            with_chat(&props, Chat::cancel_reply);
                        } else {
                            self.refocus = false;
                            leave_field(cx, &self.view);
                        }
                        self.view.redraw(cx);
                        if let Some(s) = scope.data.get_mut::<Session>() {
                            s.redraw();
                        }
                        return;
                    }
                    _ => {}
                }
            } else {
                match k.key_code {
                    // Match the shared list grammar: the press marks once,
                    // even if the input context also emits a space as text.
                    KeyCode::Space
                        if !(k.modifiers.shift || k.modifiers.control
                            || k.modifiers.alt || k.modifiers.logo) =>
                    {
                        with_chat(&props, Chat::toggle_mark);
                        self.view.redraw(cx);
                        if let Some(s) = scope.data.get_mut::<Session>() {
                            s.redraw();
                        }
                    }
                    KeyCode::ArrowDown | KeyCode::ArrowUp => {
                        let d: isize = if k.key_code == KeyCode::ArrowDown {
                            1
                        } else {
                            -1
                        };
                        let landed = with_chat(&props, |c| {
                            if k.modifiers.shift {
                                c.mark_range(d);
                                c.cursor()
                            } else {
                                c.walk(d)
                            }
                        })
                        .flatten();
                        if let Some(id) = landed {
                            self.follow(cx, id);
                        }
                        self.view.redraw(cx);
                        // The cursor and the marks feed the bar, which the
                        // stage draws.
                        if let Some(s) = scope.data.get_mut::<Session>() {
                            s.redraw();
                        }
                    }
                    // Esc lets an edit go first, then the reply line, then
                    // the marks.
                    KeyCode::Escape => {
                        with_chat(&props, |c| {
                            if c.editing().is_some() {
                                c.cancel_edit();
                            } else if c.reply_to().is_some() {
                                c.cancel_reply();
                            } else {
                                c.clear_marks();
                            }
                        });
                        self.view.redraw(cx);
                        if let Some(s) = scope.data.get_mut::<Session>() {
                            s.redraw();
                        }
                    }
                    // Enter is the way into the composer from the lines, as
                    // the placeholder says, and so is tab.
                    KeyCode::ReturnKey | KeyCode::NumpadEnter | KeyCode::Tab if can_post(&props) => {
                        field.set_key_focus(cx);
                        self.view.redraw(cx);
                    }
                    _ => {}
                }
            }
        }

        let actions = cx.capture_actions(|cx| super::text::handle_event(&mut self.view, cx, event, scope));
        if self.video.handle_actions(cx, &clip_box, &actions) {
            self.view.redraw(cx);
        }
        cx.extend_actions(actions);
        let video_after = media::video_word(cx, &clip_box);
        if video_before != video_after {
            self.view.redraw(cx);
            if let Event::VideoDecodingError(error) = event {
                if let Some(s) = scope.data.get_mut::<Session>() {
                    with_chat(&props, |c| c.pause(s.now()));
                    super::super::runtime::of(s.store()).operations.report(
                        s.store(), "playing video", &error.error,
                    );
                    s.redraw();
                }
            }
        }
        if let Some(s) = scope.data.get_mut::<Session>() {
            if self.background || !super::message_panel_visible(s, props.slot) {
                with_chat(&props, |c| c.pause(s.now()));
                media::pause_video(cx, &clip_box);
                self.scrubbing = None;
            }
        }
        self.mount(cx, &props, scope);

        // Once a bar owns the drag, keep seeking outside its rectangle.
        // A fresh press or release ends that capture.
        let seek = match event {
            Event::MouseDown(e) if e.button == MouseButton::PRIMARY => {
                self.scrubbing = None;
                None
            }
            Event::MouseMove(e) => self.scrubbing.as_ref()
                .map(|(m, bar)| (m.clone(), bar.position(e.abs.x))),
            Event::MouseUp(e) if e.button == MouseButton::PRIMARY => self.scrubbing.take()
                .map(|(m, bar)| (m, bar.position(e.abs.x))),
            _ => None,
        };
        if let Some((m, position)) = seek {
            let now = super::now(scope);
            with_chat(&props, |c| self.video.seek(cx, c.select_playback(m.key()), &m, position, now));
            self.view.redraw(cx);
            if let Some(s) = scope.data.get_mut::<Session>() { s.redraw(); }
            return;
        }

        // A press on the composer's field takes the keyboard, the way a
        // press on any field does. Makepad's own TextInput grabs focus off
        // its finger-hit path, which a synthesized press does not drive, and
        // a focus set here inside the press is undone by the event's own
        // default. So the wish is left on the instance and taken at the top
        // of the next event, the very path a reply's caret rides — which is
        // why a press above the field, on the reply or editing line, keeps
        // the caret where it is.
        if let Event::MouseDown(e) = event {
            let on_field = props
                .hits
                .at(e.abs)
                .filter(|h| h.slot == Some(props.slot))
                .map(|h| h.rect)
                .is_some_and(|r| same_rect(r, field.area().rect(cx)));
            if on_field && can_post(&props) && !field.key_focus(cx) {
                self.refocus = true;
                self.view.redraw(cx);
                if let Some(s) = scope.data.get_mut::<Session>() {
                    s.redraw();
                }
            }
        }

        // A press: on a control inside a line first — the play button, the
        // picture, the map — then on the line, which puts the cursor there
        // and the keyboard with the rows. Only this panel's own rows, and
        // only where nothing was drawn over them: the hit table settles
        // that, as it does for a human — and it is the hit's own rectangle
        // the press is matched against, not the point alone, so a press on
        // the reply line or the composer, which stand where a row stood a
        // frame earlier, never moves the cursor.
        if let Event::MouseDown(e) = event {
            let hit = props
                .hits
                .at(e.abs)
                .filter(|h| h.slot == Some(props.slot))
                .map(|h| h.rect);
            if let Some(rect) = hit {
                if let Some(act) = self.inner_at(rect) {
                    if e.button != MouseButton::PRIMARY { return; }
                    let now = scope.data.get_mut::<Session>().map_or(0.0, |s| s.now());
                    match act {
                        Inner::Play(m) => {
                            with_chat(&props, |c| c.toggle_play(&m, now));
                        }
                        Inner::Seek(m, bar) => {
                            with_chat(&props, |c| self.video.seek(
                                cx, c.select_playback(m.key()), &m, bar.position(e.abs.x), now,
                            ));
                            self.scrubbing = Some((m, bar));
                        }
                        Inner::Original(id) => {
                            if let Some(s) = scope.data.get_mut::<Session>() {
                                with_chat(&props, |c| {
                                    c.set_cursor(id);
                                    c.jump_to_original(s);
                                });
                            }
                            self.refocus = false;
                            leave_field(cx, &self.view);
                        }
                        Inner::View(id) | Inner::Card(id) => {
                            let target = with_chat(&props, |c| match act {
                                Inner::View(_) => {
                                    c.pause(now);
                                    media::pause_video(cx, &clip_box);
                                    Viewer::id(id.0, id.1)
                                }
                                _ => Line::id(id.0, id.1),
                            });
                            if let (Some(id), Some(s)) = (target, scope.data.get_mut::<Session>()) {
                                s.nav(Nav::Open {
                                    from: props.slot,
                                    id,
                                    fresh: e.modifiers.logo,
                                });
                            }
                        }
                    }
                    self.view.redraw(cx);
                    if let Some(s) = scope.data.get_mut::<Session>() {
                        s.redraw();
                    }
                } else if let Some(id) = self.row_at(rect) {
                    with_chat(&props, |c| c.set_cursor(id));
                    self.refocus = false;
                    leave_field(cx, &self.view);
                    // The focus this press brings is not the panel *taking*
                    // focus: the keyboard was put on the lines on purpose.
                    self.had_focus = true;
                    self.view.redraw(cx);
                    if let Some(s) = scope.data.get_mut::<Session>() {
                        s.nav(Nav::Focus(props.slot));
                        s.redraw();
                    }
                }
            }
        }

        if let Event::Actions(actions) = event {
            if field.changed(actions).is_some() {
                let text = field.text();
                self.shown = text.clone();
                with_chat(&props, |c| c.typed(&text));
                cx.stop_timer(self.draft_timer);
                self.draft_timer = cx.start_timeout(0.3);
                if let Some(s) = scope.data.get_mut::<Session>() {
                    s.redraw();
                }
            }
            if field.key_focus_lost(actions) {
                field.set_cursor(cx, field.cursor(), false);
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let render = RenderContext::from_scope(scope);
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let now = render.now;
        // A bar action runs after this widget handles its input event.
        // Take its navigation request on the first draw it invalidated,
        // without waiting for another key, pointer event or timer tick.
        if self.mounted && with_chat(&props, |c| c.transcript_ready()).unwrap_or(false) {
            if let Some(id) = with_chat(&props, Chat::take_follow_wish).flatten() {
                self.unread_space = None;
                self.reveal.request(id);
            }
        }
        self.view
            .label(cx, ids!(drop_hint))
            .set_visible(cx, self.dragging_files);
        // Cloned out of the instance: the row loop hands `scope` on to each
        // item, so nothing may still be borrowing it by then.
        let Some((card, snapshot, loading, cursor, marks, above, text, carrying, moving)) = ({
            let mut borrow = props.panel.borrow_mut();
            borrow.as_any().downcast_mut::<Chat>().map(|c| {
                // Capture loading first: the reader may finish between these
                // calls, but unloaded rows must never look like an empty chat.
                let loading = c.loading();
                let snapshot = c.snapshot(now);
                (
                    c.card(),
                    snapshot,
                    loading,
                    c.cursor(),
                    c.marks().clone(),
                    c.above_line(now),
                    c.field_text().to_string(),
                    c.carrying().iter().map(|f| (f.label(), f.path.clone())).collect::<Vec<_>>(),
                    c.playing(now),
                )
            })
        }) else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let rows = &snapshot.rows;

        // The peer's status line, and *loading…* beside it while the wire
        // is still filling the transcript — a transcript that is short is
        // seen to be short for now.
        let mut status = card.as_ref().map(model::PeerCard::status_line).unwrap_or_default();
        if loading {
            if !status.is_empty() {
                status.push_str(" · ");
            }
            status.push_str("loading…");
        }
        self.view.label(cx, STATUS).set_text(cx, &status);
        self.view.label(cx, EMPTY).set_visible(cx, !loading && rows.is_empty());

        // The composer, or the line that stands where it cannot.
        let can_post = card.as_ref().is_none_or(model::PeerCard::can_post);
        self.view.view(cx, COMPOSER).set_visible(cx, can_post);
        self.view.label(cx, CANNOT).set_visible(cx, !can_post);
        self.view.label(cx, CANNOT).set_text(cx,
            if card.as_ref().is_some_and(|c| c.blocked) {
                "unblock this user to send messages"
            } else {
                "you can't post here"
            }
        );
        let field = self.view.text_input(cx, INPUT);
        props.keyboard.keep(&field, Letters::TEXT);
        if !can_post && field.key_focus(cx) {
            self.refocus = false;
            leave_field(cx, &self.view);
        }
        let placeholder = card
            .as_ref()
            .map_or("write a message…  ( enter )", model::PeerCard::placeholder);
        if field.empty_text() != placeholder {
            field.set_empty_text(cx, placeholder.to_string());
        }
        // The instance's text — the draft, or the edit under way — put back
        // into the field when it moved without a keystroke of this
        // widget's.
        if self.mounted && text != self.shown {
            field.set_text(cx, &text);
            self.shown = text.clone();
        }
        self.view.view(cx, REPLY_ROW).set_visible(cx, above.is_some());
        self.view
            .label(cx, REPLY)
            .set_text(cx, above.as_deref().unwrap_or(""));
        // Hide staged files with the composer; the chat keeps them for unblocking.
        self.carries(cx, if can_post { &carrying } else { &[] }, props.slot);

        let n = rows.len();
        let video = self.view.child(live_id!(video_source)).child(live_id!(clip_box));
        let active = with_chat(&props, |c| c.active_media()).flatten();
        let mut video_drawn = false;
        let mut video_redraw = false;
        let mut drawn: Vec<(usize, WidgetRef, Option<PlayerState>)> = Vec::new();
        let anchor = self.anchor.take();
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            let list_ref = item.as_portal_list();
            let Some(mut list) = list_ref.borrow_mut() else {
                continue;
            };
            if !self.positioned && n > 0 {
                let unread = rows.iter().position(|r| matches!(r, Row::Unread));
                match unread.filter(|_| cursor.is_none() && self.reveal.target().is_none()) {
                    Some(idx) => {
                        let height = cx.turtle().inner_rect().size.y;
                        let offset = unread_offset(height);
                        self.unread_space = Some((height - offset).max(0.0));
                        list.set_tail_range(false);
                        // Bound the context in pixels: the preceding message
                        // may be a tall picture or several screens of text.
                        list.set_first_id_and_scroll(idx, offset);
                    }
                    None => list.set_tail_range(true),
                }
                self.positioned = true;
            }
            // Rows shifted under a view that is not following the end — a
            // backfill landed older lines above it — so the line that stood
            // at the list's first row now has another index: move the list
            // by the same amount, so it stays on the line it was on. Only
            // the shift is corrected; a scroll of the reader's, or one still
            // animating towards a cursor, is left exactly where it got to.
            if let Some((id, first_then)) = anchor {
                if !list.is_at_end() || self.unread_space.is_some() {
                    let now_at = snapshot.row_index(id);
                    if let Some(shift) = now_at.map(|i| i as isize - first_then as isize).filter(|&d| d != 0) {
                        let first = (list.first_id() as isize + shift).max(0) as usize;
                        let scroll = list.first_scroll();
                        list.set_first_id_and_scroll(first, scroll);
                    }
                }
            }
            if self.unread_space.is_some() {
                list.set_tail_range(false);
            }
            list.set_item_range(cx, 0, n + usize::from(self.unread_space.is_some()));
            let first = list.first_id();
            self.anchor = rows
                .iter()
                .enumerate()
                .skip(first)
                .find_map(|(i, r)| r.msg().map(|m| (m.key(), i)));
            while let Some(idx) = list.next_visible_item(cx) {
                if idx == n {
                    if let Some(height) = self.unread_space {
                        let space = list.item(cx, idx, live_id!(end_space));
                        if let Some(mut view) = space.borrow_mut::<View>() {
                            view.walk.height = Size::Fixed(height);
                        }
                        space.draw_all(cx, scope);
                    }
                    continue;
                }
                let Some(r) = rows.get(idx) else { continue };
                let row = list.item(cx, idx, live_id!(row));
                let id = r.msg().map(|m| m.key());
                let selected = id.is_some() && id == cursor;
                let marked = id.is_some_and(|i| marks.contains(&i));
                let mut player = r.msg().and_then(|m| with_chat(&props, |c| c.player_state(m, now)).flatten());
                populate(
                    cx,
                    &row,
                    r,
                    (selected, marked),
                    player,
                    &render,
                );
                if let Some(m) = r.msg() {
                    self.want_picture(cx, m, &render);
                    let message = row.view(cx, ids!(msg));
                    let line = table::line(cx, &message, selected, marked);
                    let slot = line.widget(cx, ids!(body.clip_box));
                    let result = with_chat(&props, |c| {
                        c.playback(m.key()).map(|p| self.video.drive(cx, &video, p, m, now))
                    }).flatten();
                    let shown = result.as_ref().is_some_and(|d| d.shown);
                    let note = result.as_ref().and_then(|d| d.note.as_deref());
                    inline_video::fill_slot(cx, &slot, &video, shown, note);
                    if let Some(result) = result {
                        video_drawn = true;
                        video_redraw |= result.redraw;
                        player = result.player;
                        let player = line.widget(cx, ids!(body.player));
                        media::fill_player(cx, &player, result.player.as_ref());
                    }
                }
                row.draw_all(cx, scope);
                drawn.push((idx, row, player));
            }
        }

        let portal = self.view.widget(cx, LIST).as_portal_list();
        if let Some(space) = self.unread_space {
            if let Some((unread_idx, unread, _)) = drawn.iter().find(|(idx, _, _)| matches!(rows[*idx], Row::Unread)) {
                let height = portal.area().rect(cx).size.y;
                let top = unread.area().rect(cx).pos.y;
                let content = drawn.iter().filter(|(idx, _, _)| idx >= unread_idx)
                    .map(|(_, row, _)| { let r = row.area().rect(cx); r.pos.y + r.size.y - top })
                    .fold(0.0, f64::max);
                let needed = (height - unread_offset(height) - content).max(0.0);
                // Measure the complete run, or enough of it to fill the view.
                // Scrolling up may leave only the divider at the bottom;
                // that partial reading cannot resize the space below it.
                let complete = drawn.iter().any(|(idx, _, _)| *idx == n - 1);
                if needed == 0.0 || complete && (needed - space).abs() > 0.5 {
                    self.unread_space = (needed > 0.0).then_some(needed);
                    cx.redraw_area_in_draw(portal.area());
                }
            }
        }
        let target_index = self.reveal.target().and_then(|id| snapshot.row_index(id));
        let target_rect = target_index.and_then(|target| {
            drawn.iter().find(|(idx, _, _)| *idx == target).map(|(_, row, _)| row.area().rect(cx))
        });
        if self.reveal.apply(cx, &portal, target_index, target_rect) {
            self.anchor = None;
        }

        // The hits, once the rows have landed: every line is addressable by
        // its writer and its first words, and a finger's mark lands on it;
        // the controls inside a line — the play button, the picture, the
        // map — are addressable by what they are, and answered first.
        self.rows.clear();
        self.inner.clear();
        let clip = self.view.widget(cx, LIST).area().rect(cx);
        let mut active_visible = false;
        let mut visible_ids = Vec::new();
        for (idx, row, player) in drawn {
            let Some(r) = rows.get(idx) else { continue };
            let full = row.area().rect(cx);
            if full.size.x <= 0.0 || full.size.y <= 0.0 {
                continue;
            }
            match r {
                Row::Message { msg, .. } => {
                    let Some(rect) = props.hits.add_row_clipped(
                        row_label(r, now), full, clip, MouseCursor::Hand, props.slot,
                    ) else { continue };
                    if msg.id > 0 && !msg.service && !matches!(msg.state.as_deref(), Some("sending" | "failed"))
                    {
                        visible_ids.push(msg.key());
                    }
                    self.rows.push(RowHit { id: msg.key(), rect, unclipped: full });
                    active_visible |= Some(msg.key()) == active;
                    let id = msg.key();
                    let twin = usize::from(Some(id) == cursor) + 2 * usize::from(marks.contains(&id));
                    self.inner_hits(cx, &props, &row, msg, twin, player, &render);
                }
                Row::Service(_) | Row::Day(_) | Row::Unread => {
                    props.hits.add_clipped(
                        row_label(r, now), full, clip, MouseCursor::Default, props.slot,
                    );
                }
            }
        }
        if !video_drawn || !active_visible {
            with_chat(&props, |c| c.pause(now));
            media::pause_video(cx, &video);
            self.scrubbing = None;
            video_redraw = false;
        }
        if let Some(s) = scope.data.get_mut::<Session>() {
            if let Some((chat, topic)) = with_chat(&props, |c| (c.peer(), c.topic_id())) {
                if self.background || !super::message_panel_visible(s, props.slot) {
                    self.viewed = None;
                    self.inherited_viewed.clear();
                } else {
                    super::super::runtime::show_history_messages(&mut self.viewed, &mut self.inherited_viewed, s.world(), chat, topic, visible_ids);
                }
            }
        }
        for (label, path, cursor) in [
            (status.clone(), STATUS, MouseCursor::Default),
            (placeholder.to_string(), INPUT, MouseCursor::Text),
            (above.clone().unwrap_or_default(), REPLY, MouseCursor::Default),
        ] {
            if label.is_empty() {
                continue;
            }
            let r = self.view.widget(cx, path).area().rect(cx);
            if r.size.x > 0.0 && r.size.y > 0.0 {
                props.hits.add(label, r, cursor, props.slot);
            }
        }
        // A player's progress moves — and a caret still landing asks to be
        // re-asked — so the next frame draws it further along.
        if (moving && active_visible) || video_redraw || self.refocus {
            self.view.redraw(cx);
        }
        self.drawn_for = Rc::downgrade(&props.panel);
        DrawStep::done()
    }
}

impl ChatPanel {
    /// The first look at a live panel: the field takes the draft and,
    /// where the panel has focus, the keyboard. Held until the
    /// field has a rectangle: focus on a field that has never been drawn is
    /// focus on nothing.
    fn mount(&mut self, cx: &mut Cx, props: &PanelProps, scope: &mut Scope) {
        if self.mounted {
            return;
        }
        let field = self.view.text_input(cx, INPUT);
        if field.area().rect(cx).size.x <= 0.0 {
            return;
        }
        self.mounted = true;
        let (text, can_post) = with_chat(props, |c| {
            (
                c.field_text().to_string(),
                c.card().is_none_or(|k| k.can_post()),
            )
        })
        .unwrap_or_default();
        field.set_text(cx, &text);
        self.shown = text;
        let focused = scope
            .data
            .get_mut::<Session>()
            .is_some_and(|s| s.focus() == Some(props.slot));
        self.had_focus = focused;
        if focused && can_post {
            field.set_key_focus(cx);
        }
    }

    /// Enter in the composer: the instance sends — an edit is written, a
    /// message is a toast this round — and the field shows what is left,
    /// the emptied draft or the draft an edit had set aside. The keyboard
    /// stays in the field, as it does on the client.
    fn send(&mut self, cx: &mut Cx, props: &PanelProps, scope: &mut Scope) {
        let field = self.view.text_input(cx, INPUT);
        let text = field.text();
        with_chat(props, |c| c.typed(&text));
        if let Some(s) = scope.data.get_mut::<Session>() {
            let mut borrow = props.panel.borrow_mut();
            if let Some(c) = borrow.as_any().downcast_mut::<Chat>() {
                c.send(s);
            }
        }
        let after = with_chat(props, |c| c.field_text().to_string()).unwrap_or_default();
        if after != text {
            field.set_text(cx, &after);
            self.shown = after;
        }
        // The sent line lands at the end; the transcript goes there to meet
        // it, and stays there as the echo settles into the server's copy.
        self.unread_space = None;
        self.view
            .widget(cx, LIST)
            .as_portal_list()
            .set_tail_range(true);
        self.view.redraw(cx);
    }

    /// The `CARRIES` line: one link a file, each opening the card over that
    /// path — the files app's when it is in the build — and a count for the
    /// rest.
    fn carries(&mut self, cx: &mut Cx2d, files: &[(String, String)], slot: kernel::layout::SlotId) {
        let v = &self.view;
        v.widget(cx, CARRIES).set_visible(cx, !files.is_empty());
        for (i, name) in CARRY_LBLS.iter().enumerate() {
            let link = v.link(cx, &[live_id!(carries), live_id!(files), *name]);
            match files.get(i) {
                Some((label, path)) => {
                    link.set(
                        cx,
                        label,
                        Nav::Open {
                            from: slot,
                            id: PanelId::new(FILE_TAG, [path.clone()]),
                            fresh: false,
                        },
                        false,
                        None,
                    );
                    link.set_visible(cx, true);
                }
                None => link.set_visible(cx, false),
            }
        }
        let rest = files.len().saturating_sub(CARRY_SLOTS);
        let more = v.label(cx, ids!(carries.files.more_lbl));
        more.set_text(cx, &format!("+{rest} more"));
        more.set_visible(cx, rest > 0);
    }

    /// The controls inside one drawn line, registered by what they are.
    /// `twin` is which of the row's four twins was drawn — plain, washed,
    /// marked, both — in the order the table picks them; the rectangles
    /// come off that one.
    #[allow(clippy::too_many_arguments)]
    fn inner_hits(
        &mut self,
        cx: &mut Cx,
        props: &PanelProps,
        row: &WidgetRef,
        m: &Msg,
        twin: usize,
        player: Option<PlayerState>,
        render: &RenderContext,
    ) {
        let now = render.now;
        let clip = self.view.widget(cx, LIST).area().rect(cx);
        const TWINS: [LiveId; 4] = [
            live_id!(line),
            live_id!(line_sel),
            live_id!(line_mark),
            live_id!(line_mark_sel),
        ];
        let line = row.widget(cx, &[live_id!(msg), TWINS[twin.min(3)]]);
        super::text::hits(
            cx,
            &line.widget(cx, ids!(body.text_wrap.body_txt)),
            props,
            clip,
        );
        // The quoted line a reply carries is the way to what it answers.
        if m.reply_to.is_some() {
            let quote = line.widget(cx, ids!(reply_lbl));
            if let Some(r) = rect_of(cx, &quote).and_then(|r| visible(r, clip)) {
                props
                    .hits
                    .add("the line it answers", r, MouseCursor::Hand, props.slot);
                self.inner.push(InnerHit {
                    rect: r,
                    act: Inner::Original(m.key()),
                });
            }
        }
        let Some(md) = m.media.as_ref() else { return };
        if md.kind == "file" {
            let label = line.widget(cx, ids!(body.media_lbl));
            if let Some(r) = rect_of(cx, &label).and_then(|r| visible(r, clip)) {
                props.hits.add(md.label.as_deref().unwrap_or("file"), r, MouseCursor::Hand, props.slot);
                self.inner.push(InnerHit { rect: r, act: Inner::View(m.key()) });
            }
        }
        if let Some(st) = player {
            let player_w = line.widget(cx, ids!(body.player));
            if let Some(r) = media::play_rect(cx, &player_w).and_then(|r| visible(r, clip)) {
                props.hits.add(
                    if st.playing { "pause" } else { "play" },
                    r,
                    MouseCursor::Hand,
                    props.slot,
                );
                props.hits.add(
                    format!("{} {}", if st.playing { "pause" } else { "play" }, md.word()),
                    r, MouseCursor::Hand, props.slot,
                );
                self.inner.push(InnerHit {
                    rect: r,
                    act: Inner::Play(Box::new(m.clone())),
                });
                props.hits.add_clipped(
                    st.time_line(), player_w.label(cx, ids!(time_lbl)).area().rect(cx),
                    clip, MouseCursor::Default, props.slot,
                );
            }
            if let Some(bar) = SeekBar::from_player(cx, &player_w, st) {
                if let Some(r) = visible(bar.rect, clip) {
                    props.hits.add(format!("seek {}", md.word()), r, MouseCursor::Hand, props.slot);
                    self.inner.push(InnerHit { rect: r, act: Inner::Seek(Box::new(m.clone()), bar) });
                }
            }
        }
        let video = line.widget(cx, ids!(body.clip_box));
        let img = if video.visible() { video } else { line.widget(cx, ids!(body.img_box)) };
        if img.visible() {
            if let Some(r) = rect_of(cx, &img).and_then(|r| visible(r, clip)) {
                props.hits.add(md.word(), r, MouseCursor::Hand, props.slot);
                self.inner.push(InnerHit {
                    rect: r,
                    act: Inner::View(m.key()),
                });
            }
        }
        if matches!(md.kind.as_str(), "location" | "live") {
            let map_w = line.widget(cx, ids!(body.map));
            if let Some(r) = rect_of(cx, &map_w).and_then(|r| visible(r, clip)) {
                props.hits.add(md.line(now), r, MouseCursor::Hand, props.slot);
                self.inner.push(InnerHit {
                    rect: r,
                    act: Inner::Card(m.key()),
                });
            }
        }
    }

    /// The draw resolves the message's current index and measured rectangle,
    /// including rows inserted by a backfill between the request and the draw.
    fn follow(&mut self, cx: &mut Cx, id: MsgKey) {
        self.reveal.request(id);
        self.view.redraw(cx);
    }

    /// Asks for the picture a row was drawn without.
    ///
    /// A photo arrives with its line — but the worker fetches only the
    /// newest forty lines' pictures as a chat opens, and the blob cache
    /// evicts what it must, so a line further up may have no bytes on this
    /// device at all and no way to draw any. The row keeps the file's
    /// durable remote id for exactly this: the worker turns it into a
    /// download on its next pass ([`super::super::runtime::want_view_file`]), the bytes land under
    /// the key the row already names, and the next draw finds them. Once per
    /// viewport, since a row without its picture is drawn again every frame; and
    /// only for what is drawn as a picture — a file's or a sticker's bytes
    /// are the opener's to ask for, not the transcript's.
    fn want_picture(&self, cx: &mut Cx, m: &Msg, render: &RenderContext) {
        if !super::super::runtime::view_settled(&self.viewed, render.now) { return; }
        let Some(md) = m.media.as_ref() else { return };
        if !matches!(md.kind.as_str(), "photo" | "video" | "circle") {
            return;
        }
        let (Some(reference), Some(rid)) = (md.reference.as_deref(), md.rid.as_deref()) else {
            return;
        };
        if !reference.starts_with("tg:") {
            return;
        }
        // The picture worker reports cache misses; no filesystem stat is
        // needed in this row loop, including while the decode is pending.
        if !super::pictures::missing(cx, md, render.store_dir.as_deref()) { return; }
        super::super::runtime::want_view_file(&self.viewed, rid);
    }

    /// The message whose rectangle the shell's hit is, by the rectangles
    /// of the last draw.
    fn row_at(&self, hit: Rect) -> Option<MsgKey> {
        self.rows
            .iter()
            .rev()
            .find(|r| same_rect(r.rect, hit))
            .map(|r| r.id)
    }

    /// The control inside a line whose rectangle the shell's hit is, if any.
    fn inner_at(&self, hit: Rect) -> Option<Inner> {
        self.inner
            .iter()
            .rev()
            .find(|h| same_rect(h.rect, hit))
            .map(|h| h.act.clone())
    }
}

/// A little preceding context, independent of that message's height.
fn unread_offset(height: f64) -> f64 { (height * 0.1).min(64.0) }

/// What a script addresses a row by: the writer and the first words of the
/// line, a caption's own word, a service line's text.
#[must_use]
pub fn row_label(r: &Row, now: f64) -> String {
    match r {
        Row::Day(caption) => caption.clone(),
        Row::Unread => "UNREAD".to_string(),
        Row::Service(m) => m.text.clone(),
        Row::Message { msg, .. } => {
            let line = model::media_or_text(msg.media.as_ref(), &msg.text, now);
            format!("{}: {line}", msg.writer())
        }
    }
}

/// Fills one row: shows the one part it is and stands the others down.
///
/// Pictures share a bounded cache across row twins and chats. `player` is where the
/// line's recording stands, for a line that has one. Public because the
/// library draws a fixture through the very same function.
pub fn populate(
    cx: &mut Cx,
    row: &WidgetRef,
    r: &Row,
    selection: (bool, bool),
    player: Option<PlayerState>,
    render: &RenderContext,
) {
    let (selected, marked) = selection;
    let (day, unread, service, msg) = match r {
        Row::Day(_) => (true, false, false, false),
        Row::Unread => (false, true, false, false),
        Row::Service(_) => (false, false, true, false),
        Row::Message { .. } => (false, false, false, true),
    };
    row.view(cx, ids!(day)).set_visible(cx, day);
    row.view(cx, ids!(unread)).set_visible(cx, unread);
    row.view(cx, ids!(service)).set_visible(cx, service);
    row.view(cx, ids!(msg)).set_visible(cx, msg);
    match r {
        Row::Day(caption) => {
            row.label(cx, ids!(day.day_lbl)).set_text(cx, caption);
        }
        Row::Unread => {}
        Row::Service(m) => {
            row.label(cx, ids!(service.service_lbl))
                .set_text(cx, &m.text);
        }
        Row::Message { msg: m, run } => {
            let now = render.now;
            let line = table::line(cx, &row.view(cx, ids!(msg)), selected, marked);
            // The header: the writer, then at the right what the line
            // carries about itself and the time. A run's second line has
            // none of it.
            line.view(cx, ids!(body.head)).set_visible(cx, !run);
            let mine = m.out;
            let name = line.label(cx, ids!(body.head.name_b));
            name.set_text(cx, if mine { "" } else { &m.sender_name });
            name.set_visible(cx, !mine);
            line.label(cx, ids!(body.head.name_me))
                .set_visible(cx, mine);
            line.label(cx, ids!(body.head.edited_lbl))
                .set_visible(cx, m.edited);
            let views = line.label(cx, ids!(body.head.views_lbl));
            views.set_text(
                cx,
                &m.views.map(|v| format!("{} views", fmt_count(v))).unwrap_or_default(),
            );
            views.set_visible(cx, m.views.is_some());
            let mark = if mine { state_mark(m.state.as_deref()) } else { "" };
            let failed = mark == "failed";
            let state = line.label(cx, ids!(body.head.state_lbl));
            state.set_text(cx, if failed { "" } else { mark });
            state.set_visible(cx, !mark.is_empty() && !failed);
            line.label(cx, ids!(body.head.state_err))
                .set_visible(cx, failed);
            line.label(cx, ids!(body.head.time_lbl))
                .set_text(cx, &fmt_hour(m.date));

            let fwd = line.label(cx, ids!(body.fwd_lbl));
            fwd.set_text(
                cx,
                &m.fwd_from.as_deref().map(|f| format!("↪ forwarded from {f}")).unwrap_or_default(),
            );
            fwd.set_visible(cx, m.fwd_from.is_some());
            let reply = line.label(cx, ids!(body.reply_lbl));
            let quoted = m.reply_to.is_some();
            reply.set_text(
                cx,
                &if quoted {
                    format!("› {}: {}", m.reply_name, model::one_line(&m.reply_text))
                } else {
                    String::new()
                },
            );
            reply.set_visible(cx, quoted);

            let has_text = !m.text.trim().is_empty();
            line.view(cx, ids!(body.text_wrap)).set_visible(cx, has_text);
            let body = line.widget(cx, ids!(body.text_wrap.body_txt));
            super::text::set(
                cx,
                &body,
                if has_text { &m.text } else { "" },
                m.entities.as_deref(),
            );

            // Disk reads, decoding and map construction belong to the picture
            // workers. A source-keyed texture is shared across recycled twins.
            let img_box = line.widget(cx, ids!(body.img_box));
            let slot = line.widget(cx, ids!(body.clip_box));
            let video = inline_video::has_video(m);
            inline_video::fill_poster(cx, &slot, m, render.store_dir.as_deref());
            let shown = super::pictures::photo(cx, &img_box, m.media.as_ref().filter(|_| !video), render.store_dir.as_deref());
            let map_box = line.widget(cx, ids!(body.map));
            super::pictures::place(cx, &map_box, m.media.as_ref(), render.store_dir.is_none());
            let player_w = line.widget(cx, ids!(body.player));
            media::fill_player(cx, &player_w, player.as_ref());
            let sticker = m.media.as_ref().filter(|md| md.is_sticker());
            let sticker_lbl = line.label(cx, ids!(body.sticker_lbl));
            sticker_lbl.set_text(cx, sticker.and_then(|s| s.label.as_deref()).unwrap_or(""));
            sticker_lbl.set_visible(cx, sticker.is_some_and(|s| s.label.is_some()));
            let media_line = m.media.as_ref().and_then(|md| {
                let is_photo = md.kind == "photo";
                let is_sticker = md.is_sticker() && md.label.is_some();
                ((!is_photo || !shown) && !is_sticker).then(|| md.line(now))
            });
            let media_lbl = line.label(cx, ids!(body.media_lbl));
            media_lbl.set_text(cx, media_line.as_deref().unwrap_or(""));
            media_lbl.set_visible(cx, media_line.is_some());

            let reactions = m.reactions.clone().unwrap_or_default();
            let comments = m
                .comments
                .filter(|c| *c > 0)
                .map(|c| format!("{c} comment{}", if c == 1 { "" } else { "s" }))
                .unwrap_or_default();
            line.view(cx, ids!(body.foot))
                .set_visible(cx, !reactions.is_empty() || !comments.is_empty());
            let re = line.label(cx, ids!(body.foot.reactions_lbl));
            re.set_text(cx, &reactions);
            re.set_visible(cx, !reactions.is_empty());
            let co = line.label(cx, ids!(body.foot.comments_lbl));
            co.set_text(cx, &comments);
            co.set_visible(cx, !comments.is_empty());
        }
    }
}

/// Whether two rectangles are the one rectangle, give or take a hair.
fn same_rect(a: Rect, b: Rect) -> bool {
    (a.pos.x - b.pos.x).abs() < 0.5
        && (a.pos.y - b.pos.y).abs() < 0.5
        && (a.size.x - b.size.x).abs() < 0.5
        && (a.size.y - b.size.y).abs() < 0.5
}

/// A drawn widget's rectangle, or none for one that took no space.
fn rect_of(cx: &mut Cx, w: &WidgetRef) -> Option<Rect> {
    let r = w.area().rect(cx);
    (r.size.x > 0.0 && r.size.y > 0.0).then_some(r)
}

/// Whether the chat behind the props takes a composer at all.
fn can_post(props: &PanelProps) -> bool {
    with_chat(props, |c| c.card().is_none_or(|k| k.can_post())).unwrap_or(false)
}

/// Runs `f` on the instance. The borrow lasts exactly as long as the call:
/// a navigation taken while it stood would find the session walking the
/// same instance.
fn with_chat<R>(props: &PanelProps, f: impl FnOnce(&mut Chat) -> R) -> Option<R> {
    let mut borrow = props.panel.borrow_mut();
    let c = borrow.as_any().downcast_mut::<Chat>()?;
    Some(f(c))
}

/// Hands the keyboard from the field back to the rows: the panel's own
/// view, never `Area::Empty`.
fn leave_field(cx: &mut Cx, view: &View) {
    cx.set_key_focus(view.area());
}

#[cfg(all(test, headless))]
mod reading_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipped_hits_only_acknowledge_reads_after_enough_of_the_message_is_shown() {
        let rect = |y, h| Rect { pos: dvec2(0.0, y), size: dvec2(300.0, h) };
        let viewport = rect(100.0, 200.0);
        for (full, viewed) in [
            (rect(130.0, 40.0), true),
            (rect(280.0, 60.0), false),
            (rect(0.0, 120.0), false),
            (rect(180.0, 400.0), true),
            (rect(280.0, 400.0), false),
        ] {
            let row = RowHit { id: (0, 1), rect: visible(full, viewport).unwrap(), unclipped: full };
            assert_eq!(row.viewed_in(viewport), viewed, "full: {full:?}, hit: {:?}", row.rect);
        }
    }
}
