//! The viewer, drawn: the picture as large as the panel, or the clip
//! playing in its place, or the player over a recording under its word,
//! with the line's caption beneath.
//!
//! A clip is the one thing here a draw does more than draw. Its player is
//! the platform's, reachable only through a `Cx`, and a verb has none — so
//! the panel keeps the wish and this carries it out, hands back where it
//! left off, and looks again each frame for a file that was still
//! downloading the frame before.

use kernel::session::Session;
use makepad_widgets::*;

use crate::shell::hosted::PanelProps;
use crate::shell::widgets::media::{self, SeekBar, VideoPlayback};
use crate::shell::widgets::viewer::{FileViewerWidgetRefExt, Measure, Preview};

use super::super::model::{self};
use super::super::panels::Viewer;

/// The widget.
#[derive(Script, ScriptHook, Widget)]
pub struct ViewerPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[rust]
    shown: Option<String>,
    #[rust]
    file_shown: Option<String>,
    /// The player's state as last traced, so the trace says each change once.
    #[rust]
    last_word: String,
    #[rust]
    last_note: Option<String>,
    #[rust]
    play: Option<Rect>,
    #[rust]
    seek_bar: Option<SeekBar>,
    #[rust]
    scrubbing: Option<SeekBar>,
    #[rust]
    playback: VideoPlayback,
}

impl Widget for ViewerPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let clip_box = self.view.widget(cx, ids!(body.clip_box));
        let before = media::video_word(cx, &clip_box);
        self.view.handle_event(cx, event, scope);
        // Preparing happens while the clip is hidden and has no draw area.
        // Its own redraw cannot reveal it; the enclosing panel must redraw
        // when the platform changes the player's state.
        if media::video_word(cx, &clip_box) != before {
            self.view.redraw(cx);
        }
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return;
        };
        if let Event::VideoDecodingError(error) = event {
            // Only this player's matching error changes its state. The
            // native error overlay is hidden with the failed clip, so report
            // it on the panel and stop the playback wish until another play.
            if before != "unprepared" && media::video_word(cx, &clip_box) == "unprepared" {
                if let Some(s) = scope.data.get_mut::<Session>() {
                    super::super::runtime::of(s.store()).operations.report(
                        s.store(), "playing video", &error.error,
                    );
                    if let Some(viewer) = props.panel.borrow_mut().as_any().downcast_mut::<Viewer>() {
                        viewer.set_running(false);
                    }
                    s.redraw();
                }
            }
        }
        let position = match event {
            Event::MouseDown(e) if e.button == MouseButton::PRIMARY => {
                self.scrubbing = None;
                if props.hits.at(e.abs).map(|h| h.slot) != Some(Some(props.slot)) {
                    return;
                }
                if let Some(bar) = self.seek_bar.filter(|b| b.rect.contains(e.abs)) {
                    self.scrubbing = Some(bar);
                    Some(bar.position(e.abs.x))
                } else if self.play.is_some_and(|r| r.contains(e.abs)) {
                    None
                } else {
                    return;
                }
            }
            Event::MouseMove(e) => {
                let Some(bar) = self.scrubbing else { return };
                Some(bar.position(e.abs.x))
            }
            Event::MouseUp(e) if e.button == MouseButton::PRIMARY => {
                let Some(bar) = self.scrubbing.take() else { return };
                Some(bar.position(e.abs.x))
            }
            Event::WindowLostFocus(_) | Event::Background => {
                self.scrubbing = None;
                return;
            }
            _ => return,
        };
        let Some(session) = scope.data.get_mut::<Session>() else {
            return;
        };
        let now = session.now();
        {
            let mut borrow = props.panel.borrow_mut();
            if let Some(v) = borrow.as_any().downcast_mut::<Viewer>() {
                if let Some(m) = v.msg() {
                    if let Some(position) = position {
                        if v.plays_clip(&m) {
                            self.playback.seek(position);
                        } else {
                            v.seek(&m, position, now);
                        }
                    } else {
                        v.toggle_play(&m, now);
                    }
                }
            }
        }
        session.redraw();
        self.view.redraw(cx);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        // Where the blob cache sits, so a downloaded photo resolves; `None`
        // with the store in memory, and the demo pictures draw without it.
        let store_dir = scope
            .data
            .get_mut::<Session>()
            .and_then(|s| s.store().dir().map(|p| p.to_path_buf()));
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let message = props.panel.borrow_mut().as_any().downcast_mut::<Viewer>()
            .and_then(|viewer| viewer.msg());
        let gone = message.is_none();
        let file = message.filter(|m| m.media.as_ref()
            .is_some_and(|md| matches!(md.kind.as_str(), "file" | "photo")));
        self.view.view(cx, ids!(body)).set_visible(cx, file.is_none() && !gone);
        self.view.widget(cx, ids!(file_view)).set_visible(cx, file.is_some() || gone);
        if let Some(message) = file {
            if self.file_shown.is_none() {
                let clip = self.view.widget(cx, ids!(body.clip_box));
                self.playback.drive(cx, &clip, None, false);
            }
            self.play = None;
            self.seek_bar = None;
            self.scrubbing = None;
            return self.draw_file(cx, scope, walk, &props, &message);
        }
        self.view.label(cx, ids!(file_name)).set_visible(cx, false);
        if (gone && self.file_shown.as_deref() != Some("gone")) || (!gone && self.file_shown.is_some()) {
            self.file_shown = gone.then(|| "gone".into());
            self.view.widget(cx, ids!(file_view)).as_file_viewer().show(cx,
                if gone { Preview::Error("This message is no longer available".into()) } else { Preview::None });
            let changed = props.panel.borrow_mut().as_any().downcast_mut::<Viewer>()
                .is_some_and(|viewer| viewer.viewer().measured(Measure::Empty));
            if changed {
                if let Some(session) = scope.data.get_mut::<Session>() { session.relayout(); }
            }
        }
        if gone {
            let clip = self.view.widget(cx, ids!(body.clip_box));
            self.playback.drive(cx, &clip, None, false);
            self.view.label(cx, ids!(caption_lbl)).set_visible(cx, false);
            return self.view.draw_walk(cx, scope, walk);
        }
        let now = super::now(scope);
        // Everything this draw needs, in one borrow of the panel — and the
        // asking with it: opening the viewer on a clip is what fetches it,
        // and the first draw is the opening.
        let Some((m, player, clip, wanted, note, playing, awaiting)) = ({
            let mut borrow = props.panel.borrow_mut();
            borrow.as_any().downcast_mut::<Viewer>().and_then(|v| {
                let m = v.msg()?;
                v.ask_for_clip(&m);
                v.ask_for_picture(&m);
                let st = v.player_state(&m, now);
                let clip = v.plays_clip(&m).then(|| v.clip_file(&m)).flatten();
                let note = v.download_note(&m);
                let awaiting = v.awaiting_picture(&m);
                Some((m, st, clip, v.running(), note, v.playing(now), awaiting))
            })
        }) else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let v = &self.view;
        if note != self.last_note {
            super::super::trace::note(
                store_dir.as_deref(),
                &format!("media: line {} {}", m.id, note.as_deref().unwrap_or("download ready")),
            );
            self.last_note = note.clone();
        }
        // What is on the picture box, by the line and the file it shows —
        // recorded only once the bytes decoded, so a photo that arrives
        // after the panel opened is decoded then, and a line whose media
        // changed is decoded again (review, 2026-09-07).
        let key = format!(
            "{}:{}",
            m.id,
            m.media.as_ref().and_then(|md| md.reference.as_deref()).unwrap_or("")
        );
        let fresh = self.shown.as_deref() != Some(key.as_str());
        // The clip where its file has landed: the platform's player in the
        // poster's place, and the panel's wish carried across to it. What
        // the player is left at goes back to the panel, so a clip that has
        // run out puts the button to `play` on its own.
        let clip_box = v.widget(cx, ids!(body.clip_box));
        let drawn = self.playback.drive(cx, &clip_box, clip.as_deref(), wanted);
        // The player's state, to the trace, on every change: the sure way
        // to tell a clip that never prepared from one playing unseen.
        let word = media::video_word(cx, &clip_box);
        if word != self.last_word {
            super::super::trace::note(
                store_dir.as_deref(),
                &format!(
                    "video: line {} player {} (wanted={wanted}, file={})",
                    m.id,
                    word,
                    clip.as_deref().map(|p| p.display().to_string()).unwrap_or_default()
                ),
            );
            self.last_word = word.to_string();
        }
        let rolling = drawn.shown;
        let mut player = player;
        if drawn.playing != wanted {
            let mut borrow = props.panel.borrow_mut();
            if let Some(vw) = borrow.as_any().downcast_mut::<Viewer>() {
                vw.set_running(drawn.playing);
            }
        }
        if rolling || self.playback.awaiting_seek() {
            let secs = m.media.as_ref().and_then(|md| md.secs).unwrap_or(0);
            let mut st = self.playback.state(cx, &clip_box, secs as f64);
            st.playing = drawn.playing;
            player = Some(st);
        }
        // The poster is the still of a clip, so it gives way to the moving
        // one; it stays for everything else.
        let bytes = (!rolling)
            .then(|| {
                m.media
                    .as_ref()
                    .and_then(|md| md.picture_bytes(store_dir.as_deref()))
            })
            .flatten();
        let big = v.widget(cx, ids!(body.big));
        let shown = media::fill_picture(cx, &big, bytes.as_deref(), fresh);
        self.shown = shown.then_some(key);
        // Without a picture: a sticker is its emoji, drawn large; anything
        // else is its word over the player, the way a sound has no face.
        let sticker = m
            .media
            .as_ref()
            .filter(|md| md.is_sticker())
            .and_then(|md| md.label.clone());
        let sticker_lbl = v.label(cx, ids!(body.sticker_lbl));
        sticker_lbl.set_text(cx, sticker.as_deref().unwrap_or(""));
        sticker_lbl.set_visible(cx, sticker.is_some());
        let word_lbl = v.label(cx, ids!(body.word_lbl));
        word_lbl.set_text(cx, &m.media.as_ref().map(|md| md.line(now)).unwrap_or_default());
        word_lbl.set_visible(cx, !shown && !rolling && sticker.is_none());
        let note_lbl = v.label(cx, ids!(body.note_lbl));
        note_lbl.set_text(cx, note.as_deref().unwrap_or(""));
        note_lbl.set_visible(cx, note.is_some());
        let player_w = v.widget(cx, ids!(body.player_box.player));
        media::fill_player(cx, &player_w, player.as_ref());
        let caption = v.label(cx, ids!(caption_lbl));
        caption.set_text(cx, &model::one_line(&m.text));
        caption.set_visible(cx, !m.text.trim().is_empty());

        let step = self.view.draw_walk(cx, scope, walk);
        let player_w = self.view.widget(cx, ids!(body.player_box.player));
        self.play = player.and_then(|_| media::play_rect(cx, &player_w));
        if let (Some(r), Some(st)) = (self.play, player) {
            props.hits.add(
                if st.playing { "pause" } else { "play" },
                r,
                MouseCursor::Hand,
                props.slot,
            );
            props.hits.add(
                st.time_line(),
                player_w.label(cx, ids!(time_lbl)).area().rect(cx),
                MouseCursor::Default,
                props.slot,
            );
        }
        self.seek_bar = player.and_then(|st| SeekBar::from_player(cx, &player_w, st));
        if let Some(bar) = self.seek_bar {
            props.hits.add("seek", bar.rect, MouseCursor::Hand, props.slot);
        } else {
            self.scrubbing = None;
        }
        if let Some(md) = m.media.as_ref() {
            let path = if rolling {
                ids!(body.clip_box)
            } else if shown {
                ids!(body.big)
            } else if sticker.is_some() {
                ids!(body.sticker_lbl)
            } else {
                ids!(body.word_lbl)
            };
            let r = self.view.widget(cx, path).area().rect(cx);
            if r.size.x > 0.0 && r.size.y > 0.0 {
                props.hits.add(md.line(now), r, MouseCursor::Default, props.slot);
            }
        }
        // A running player wants the next frame; so does a download, which
        // nothing else announces — the file appears under the cache's name
        // and the next draw is what finds it, whether it is the clip or the
        // picture itself; and so does a player asked to play that is not
        // showing yet. The platform prepares a clip a moment after it is
        // asked and says so to a box nobody has drawn, whose redraw is of
        // nothing: only this widget drawing again finds it playing and shows
        // it (2026-09-07: the clip downloaded, the player prepared, and the
        // box stayed hidden). A paused seek also needs draws until its
        // requested position gives way to the actual frame or times out.
        if playing || note.is_some() || awaiting || (wanted && !rolling) || self.playback.seek_needs_redraw() {
            self.view.redraw(cx);
        }
        step
    }
}

impl ViewerPanel {
    fn draw_file(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk,
        props: &PanelProps, message: &model::Msg) -> DrawStep {
        let input = props.panel.borrow_mut().as_any().downcast_mut::<Viewer>()
            .map(|viewer| viewer.file_preview(message));
        let viewer = self.view.widget(cx, ids!(file_view)).as_file_viewer();
        if let Some(control) = props.panel.borrow_mut().as_any().downcast_mut::<Viewer>().map(|panel| panel.viewer()) {
            viewer.bind(control);
        }
        if let Some((key, preview)) = input {
            if self.file_shown.as_ref() != Some(&key) {
                viewer.show(cx, preview);
                self.file_shown = Some(key);
            }
        }
        let name = self.view.label(cx, ids!(file_name));
        name.set_text(cx, &super::super::downloads::name(message));
        name.set_visible(cx, message.media.as_ref().is_some_and(|md| md.kind == "file"));
        let caption = self.view.label(cx, ids!(caption_lbl));
        caption.set_text(cx, &model::one_line(&message.text));
        caption.set_visible(cx, !message.text.trim().is_empty());
        if let Some(md) = &message.media { viewer.image_label(cx, md.line(super::now(scope))); }
        self.view.draw_walk(cx, scope, walk)
    }
}
