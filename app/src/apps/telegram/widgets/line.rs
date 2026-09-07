//! A line's card, drawn: the writer and the time, the whole text as a
//! selectable run, and its media at the card's width — a picture, a player,
//! a map — through the shell's kit.

use kernel::nav::Nav;
use kernel::session::Session;
use makepad_widgets::*;

use crate::shell::hosted::PanelProps;
use crate::shell::widgets::map::{self, FakeTiles};
use crate::shell::widgets::media;

use super::super::model::{self, fmt_count, fmt_hour, state_mark};
use super::super::panels::{Line, Viewer};

/// The widget.
#[derive(Script, ScriptHook, Widget)]
pub struct LinePanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    /// Which line the picture box and the map last decoded for.
    #[rust]
    shown: Option<String>,
    /// The play button's rectangle of the last draw, and what a press on
    /// the picture opens.
    #[rust]
    play: Option<Rect>,
    #[rust]
    picture: Option<Rect>,
    #[rust]
    viewed: Option<super::super::runtime::MessageView>,
}

impl Widget for LinePanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        super::text::handle_event(&mut self.view, cx, event, scope);
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return;
        };
        if let Some(s) = scope.data.get_mut::<Session>() {
            let changed = props.panel.borrow_mut().as_any().downcast_mut::<Line>()
                .is_some_and(|l| l.poll_reactions(s));
            if changed {
                self.view.redraw(cx);
                s.redraw();
            }
        }
        if matches!(event, Event::KeyDown(k) if k.key_code == KeyCode::Escape)
            && scope.data.get_mut::<Session>().is_some_and(|s| s.focus() == Some(props.slot))
        {
            let cancelled = props.panel.borrow_mut().as_any().downcast_mut::<Line>()
                .is_some_and(Line::cancel_reactions);
            if cancelled {
                self.view.redraw(cx);
                if let Some(s) = scope.data.get_mut::<Session>() {
                    s.redraw();
                }
                return;
            }
        }
        let Event::MouseDown(e) = event else { return };
        if props.hits.at(e.abs).map(|h| h.slot) != Some(Some(props.slot)) {
            return;
        }
        let Some(session) = scope.data.get_mut::<Session>() else {
            return;
        };
        let now = session.now();
        if self.play.is_some_and(|r| r.contains(e.abs)) {
            let mut borrow = props.panel.borrow_mut();
            if let Some(l) = borrow.as_any().downcast_mut::<Line>() {
                if let Some(m) = l.msg() {
                    l.toggle_play(&m, now);
                }
            }
            drop(borrow);
            session.redraw();
            self.view.redraw(cx);
        } else if self.picture.is_some_and(|r| r.contains(e.abs)) {
            let target = {
                let mut borrow = props.panel.borrow_mut();
                borrow.as_any().downcast_mut::<Line>().and_then(|l| {
                    let m = l.msg()?;
                    Some(Viewer::id(m.chat, m.id))
                })
            };
            if let Some(id) = target {
                session.nav(Nav::Open {
                    from: props.slot,
                    id,
                    fresh: e.modifiers.logo,
                });
            }
        }
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
        let now = super::now(scope);
        let Some((m, player, playing)) = ({
            let mut borrow = props.panel.borrow_mut();
            borrow.as_any().downcast_mut::<Line>().and_then(|l| {
                let m = l.msg()?;
                let st = l.player_state(&m, now);
                Some((m, st, l.playing(now)))
            })
        }) else {
            self.viewed = None;
            self.view.label(cx, ids!(gone_lbl)).set_visible(cx, true);
            return self.view.draw_walk(cx, scope, walk);
        };
        if let Some(s) = scope.data.get_mut::<Session>() {
            let ids = if m.id > 0 && !m.service && !matches!(m.state.as_deref(), Some("sending" | "failed")) {
                vec![m.id]
            } else {
                Vec::new()
            };
            super::super::runtime::show_messages(&mut self.viewed, s.world(), m.chat, ids);
        }
        let v = &self.view;
        v.label(cx, ids!(gone_lbl)).set_visible(cx, false);

        // The header: the writer, then what the line says about itself and
        // the time — the transcript's, laid the same.
        let name = v.label(cx, ids!(head.name_b));
        name.set_text(cx, if m.out { "" } else { &m.sender_name });
        name.set_visible(cx, !m.out);
        v.label(cx, ids!(head.name_me)).set_visible(cx, m.out);
        v.label(cx, ids!(head.edited_lbl)).set_visible(cx, m.edited);
        let views = v.label(cx, ids!(head.views_lbl));
        views.set_text(cx, &m.views.map(|n| format!("{} views", fmt_count(n))).unwrap_or_default());
        views.set_visible(cx, m.views.is_some());
        let mark = if m.out { state_mark(m.state.as_deref()) } else { "" };
        let failed = mark == "failed";
        let state = v.label(cx, ids!(head.state_lbl));
        state.set_text(cx, if failed { "" } else { mark });
        state.set_visible(cx, !mark.is_empty() && !failed);
        v.label(cx, ids!(head.state_err)).set_visible(cx, failed);
        v.label(cx, ids!(head.time_lbl)).set_text(cx, &fmt_hour(m.date));

        let fwd = v.label(cx, ids!(fwd_lbl));
        fwd.set_text(cx, &m.fwd_from.as_deref().map(|f| format!("↪ forwarded from {f}")).unwrap_or_default());
        fwd.set_visible(cx, m.fwd_from.is_some());
        let reply = v.label(cx, ids!(reply_lbl));
        reply.set_text(
            cx,
            &if m.reply_to.is_some() {
                format!("› {}: {}", m.reply_name, model::one_line(&m.reply_text))
            } else {
                String::new()
            },
        );
        reply.set_visible(cx, m.reply_to.is_some());

        // The media, decoded when the line changed: a picture at the card's
        // width, a map for a place, and the player over a recording drawn
        // every frame, since it moves.
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
        let bytes = m
            .media
            .as_ref()
            .and_then(|md| md.picture_bytes(store_dir.as_deref()));
        let img_box = v.widget(cx, ids!(img_box));
        let decoded = media::fill_picture(cx, &img_box, bytes.as_deref(), fresh);
        self.shown = decoded.then_some(key);
        let place = m
            .media
            .as_ref()
            .filter(|md| matches!(md.kind.as_str(), "location" | "live"))
            .and_then(|md| Some((md.lat?, md.lon?)));
        if fresh || place.is_none() {
            let snap = place.map(|(lat, lon)| {
                map::snapshot(&mut FakeTiles, lat, lon, map::ZOOM, 320, 160)
            });
            let map_w = v.widget(cx, ids!(map));
            media::fill_map(cx, &map_w, snap.as_ref());
        }
        let player_w = v.widget(cx, ids!(player));
        media::fill_player(cx, &player_w, player.as_ref());
        let sticker = m.media.as_ref().filter(|md| md.is_sticker());
        let sticker_lbl = v.label(cx, ids!(sticker_lbl));
        sticker_lbl.set_text(cx, sticker.and_then(|s| s.label.as_deref()).unwrap_or(""));
        sticker_lbl.set_visible(cx, sticker.is_some_and(|s| s.label.is_some()));
        let media_line = m.media.as_ref().and_then(|md| {
            let pictured = bytes.is_some() && md.kind == "photo";
            let is_sticker = md.is_sticker() && md.label.is_some();
            (!pictured && !is_sticker).then(|| md.line(now))
        });
        let media_lbl = v.label(cx, ids!(media_lbl));
        media_lbl.set_text(cx, media_line.as_deref().unwrap_or(""));
        media_lbl.set_visible(cx, media_line.is_some());

        let has_text = !m.text.trim().is_empty();
        v.view(cx, ids!(text_wrap)).set_visible(cx, has_text);
        let body = v.widget(cx, ids!(text_wrap.body_txt));
        super::text::set(
            cx,
            &body,
            if has_text { &m.text } else { "" },
            m.entities.as_deref(),
        );

        let reactions = m.reactions.clone().unwrap_or_default();
        let comments = m
            .comments
            .filter(|c| *c > 0)
            .map(|c| format!("{c} comment{}", if c == 1 { "" } else { "s" }))
            .unwrap_or_default();
        v.view(cx, ids!(foot))
            .set_visible(cx, !reactions.is_empty() || !comments.is_empty());
        let re = v.label(cx, ids!(foot.reactions_lbl));
        re.set_text(cx, &reactions);
        re.set_visible(cx, !reactions.is_empty());
        let co = v.label(cx, ids!(foot.comments_lbl));
        co.set_text(cx, &comments);
        co.set_visible(cx, !comments.is_empty());

        let step = self.view.draw_walk(cx, scope, walk);

        // The hits: the text as a selectable run, the play button, the
        // picture as the way to the viewer.
        let text_w = self.view.widget(cx, ids!(text_wrap.body_txt));
        if let Some(r) = rect_of(cx, &text_w) {
            if has_text {
                props.hits.add("line text", r, MouseCursor::Text, props.slot);
                super::text::hits(cx, &text_w, &props, self.view.area().rect(cx));
            }
        }
        let player_w = self.view.widget(cx, ids!(player));
        self.play = player.and_then(|_| media::play_rect(cx, &player_w));
        if let (Some(r), Some(st)) = (self.play, player) {
            props.hits.add(
                if st.playing { "pause" } else { "play" },
                r,
                MouseCursor::Hand,
                props.slot,
            );
        }
        let img_w = self.view.widget(cx, ids!(img_box));
        self.picture = bytes.and(rect_of(cx, &img_w));
        if let Some(r) = self.picture {
            let word = m.media.as_ref().map_or("picture", |md| md.word());
            props.hits.add(word, r, MouseCursor::Hand, props.slot);
        }
        if playing {
            self.view.redraw(cx);
        }
        step
    }
}

/// A drawn widget's rectangle, or none for one that took no space.
fn rect_of(cx: &mut Cx, w: &WidgetRef) -> Option<Rect> {
    let r = w.area().rect(cx);
    (r.size.x > 0.0 && r.size.y > 0.0).then_some(r)
}
