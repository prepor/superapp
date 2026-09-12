//! A clip or a sound in a reading: the item the `Html` widget places in
//! its flow for a `<video>` or an `<audio>` the narrowing kept, drawn and
//! played through the shell's media kit.
//!
//! The item is its own host. It owns a transport and a driver, keeps the
//! platform's player in a hidden holder of its own and lends it to the
//! surface while there is a picture, fills the poster from the shared
//! picture cache, and answers the strip's button and hairline itself: a
//! press on *play* sets the wish, a drag on the hairline seeks, and the
//! draw is where the wish is made so. The panel around it registers the
//! button and the hairline as hits from the rectangles the item leaves with
//! [`pictures`](super::pictures), the way a linked picture leaves its own.
//!
//! What plays is an address on the web the platform's player streams
//! itself. A clip that says `autoplay muted` runs on sight, looping if it
//! says `loop`, as the silent moving picture it was published as, and
//! stands outside the one-at-a-time rule. Everything else waits for *play*.
//! Where the platform says it cannot play the type — or fails to — the
//! item draws a link to the source in the clip's place.

use kernel::session::Session;
use makepad_widgets::makepad_platform::can_play_type;
use makepad_widgets::*;

use crate::shell::hosted::{self, PanelProps};
use crate::shell::widgets::media::{self, Clip, PlayerState, SeekBar, Source, Transport};

use super::pictures;

/// How the item's box is decided: the size hints, the poster's own pixels,
/// what the platform said on preparing, or nothing — in that order of
/// arrival, and the first that arrives holds.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
enum Shape {
    #[default]
    Unknown,
    Known(f64),
}

/// Whether the platform can play a type, as the browser asks it. An empty
/// type is not a refusal, and neither is a platform with no player at all
/// — headless says no to everything — which keeps the surface and the
/// strip, and runs nothing, rather than a link.
fn playable(kind: &str) -> bool {
    kind.trim().is_empty()
        || !can_play_type(kind).is_empty()
        || can_play_type("video/mp4").is_empty()
}

/// How many readings' players may stand prepared at once, across every
/// reading open. A prepared player is a decoder and its buffers, and a
/// long reading with a clip in every paragraph would otherwise hold one
/// per clip, playing or not.
const MAX_PREPARED: usize = 3;

/// The readings' players that are prepared, most recently used first, and
/// the ones told to let go. An item is minted from a template and cannot
/// see its siblings, so the bound is kept here, on `Cx`, and an item reads
/// its own verdict on its next draw.
#[derive(Default)]
struct Prepared {
    used: Vec<(WidgetUid, bool)>,
    released: Vec<WidgetUid>,
}

/// Marks a player used — on preparing, and on every draw while it plays —
/// and lets the least recently used paused one go past the bound. A
/// playing one is never let go: one plays at a time, and a silent loop is
/// what a page published, not a cost to trim.
fn touch(cx: &mut Cx, uid: WidgetUid, playing: bool) {
    let p = cx.global::<Prepared>();
    p.used.retain(|(u, _)| *u != uid);
    p.used.insert(0, (uid, playing));
    while p.used.len() > MAX_PREPARED {
        let Some(at) = p.used.iter().rposition(|(_, playing)| !playing) else { break };
        let (old, _) = p.used.remove(at);
        p.released.push(old);
        // An item that has gone never collects its verdict; keep the list
        // from remembering every one there ever was.
        if p.released.len() > 64 {
            p.released.remove(0);
        }
    }
}

/// Whether this player was told to let go, taking the verdict.
fn let_go(cx: &mut Cx, uid: WidgetUid) -> bool {
    let p = cx.global::<Prepared>();
    match p.released.iter().position(|u| *u == uid) {
        Some(at) => {
            p.released.remove(at);
            true
        }
        None => false,
    }
}

#[derive(Script, Widget)]
pub struct ReaderClip {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    /// The box's caps: a reading-sized clip, the same as a picture's.
    #[live(360.0)]
    max_width: f64,
    #[live(320.0)]
    max_height: f64,
    /// A sound: the strip alone, no box.
    #[live(false)]
    sound: bool,
    #[rust]
    src: String,
    #[rust]
    kind: String,
    #[rust]
    poster: String,
    #[rust]
    width: Option<f64>,
    #[rust]
    height: Option<f64>,
    #[rust]
    autoplay: bool,
    #[rust]
    looping: bool,
    #[rust]
    muted: bool,
    #[rust]
    clip: Clip,
    /// Made on the first press, with the store's registry; an animation's
    /// on the first draw, since it takes no slot.
    #[rust]
    transport: Option<Transport>,
    #[rust]
    shape: Shape,
    #[rust]
    poster_shown: bool,
    /// The platform cannot play this, or gave up on it: a link stands in.
    #[rust]
    unplayable: bool,
    #[rust]
    state: PlayerState,
    #[rust]
    play: Option<Rect>,
    #[rust]
    seek: Option<SeekBar>,
    #[rust]
    scrubbing: bool,
    #[rust]
    background: bool,
    #[rust]
    primed: bool,
}

impl ScriptHook for ReaderClip {
    fn on_after_new_scoped(&mut self, _vm: &mut ScriptVm, scope: &mut Scope) {
        // The tag's attributes, the way `HtmlLink` reads its href.
        let Some(doc) = scope.props.get::<makepad_html::HtmlDoc>() else {
            return;
        };
        let mut walker = doc.new_walker_with_index(scope.index + 1);
        while let Some((lc, attr)) = walker.while_attr_lc() {
            match lc {
                live_id!(src) => self.src = attr.into(),
                live_id!(type) => self.kind = attr.into(),
                live_id!(poster) => self.poster = attr.into(),
                live_id!(width) => self.width = attr.parse().ok(),
                live_id!(height) => self.height = attr.parse().ok(),
                live_id!(autoplay) => self.autoplay = true,
                live_id!(loop) => self.looping = true,
                live_id!(muted) => self.muted = true,
                _ => {}
            }
        }
        self.unplayable = !playable(&self.kind);
        self.shape = match (self.width, self.height) {
            (Some(w), Some(h)) if w >= 1.0 && h >= 1.0 => Shape::Known(w / h),
            _ => Shape::Unknown,
        };
    }
}

impl ReaderClip {
    /// A silent moving picture: what plays on sight, looping, and pauses
    /// nothing.
    fn animation(&self) -> bool {
        self.autoplay && self.muted && !self.sound
    }

    /// The holder whose player this item drives: the silent looping one
    /// for an animation, the plain one for everything else.
    fn video(&self, cx: &Cx) -> WidgetRef {
        if self.animation() {
            self.view.widget(cx, ids!(loop_source.clip_box))
        } else {
            self.view.widget(cx, ids!(video_source.clip_box))
        }
    }

    fn wish(&self) -> bool {
        self.transport.as_ref().is_some_and(Transport::running)
    }

    /// The transport, made on first need: an animation's takes no slot and
    /// needs no store; any other waits for a press, which has a session.
    fn transport(&mut self, s: Option<&Session>) -> Option<&mut Transport> {
        if self.transport.is_none() {
            self.transport = if self.animation() {
                Some(Transport::animation())
            } else {
                s.map(|s| Transport::new(s.store()))
            };
        }
        self.transport.as_mut()
    }

    fn pause(&mut self, cx: &mut Cx, now: f64) {
        if let Some(t) = self.transport.as_mut() {
            t.pause(now);
        }
        let video = self.video(cx);
        media::pause_video(cx, &video);
    }

    /// The box: the width hint, never wider than the column, capped, and
    /// tall by the shape as it is known.
    fn box_width(&self, cx: &Cx2d) -> f64 {
        let avail = cx.turtle().inner_width();
        let mut w = self.max_width;
        if avail.is_finite() && avail > 1.0 {
            w = w.min(avail);
        }
        if let Some(hint) = self.width.filter(|w| *w >= 1.0) {
            w = w.min(hint);
        }
        let aspect = match self.shape {
            Shape::Known(a) => a,
            Shape::Unknown => 16.0 / 9.0,
        };
        w.min(self.max_height * aspect)
    }

    /// Fills the poster from the picture cache, once its bytes are here;
    /// the poster's own pixels decide the shape where nothing else has.
    fn fill_poster(&mut self, cx: &mut Cx) {
        if self.poster.is_empty() || self.sound {
            return;
        }
        let poster = self.view.widget(cx, ids!(surface.poster));
        if self.poster_shown {
            media::fill_picture(cx, &poster, Some(&[][..]), false);
            return;
        }
        let Some(bytes) = pictures::web_bytes(cx, &self.poster) else {
            return;
        };
        self.poster_shown = media::fill_picture(cx, &poster, Some(&bytes), true);
        if self.poster_shown && self.shape == Shape::Unknown {
            if let Some((w, h)) = poster.widget(cx, ids!(img)).as_image().size_in_pixels(cx) {
                if w > 0 && h > 0 {
                    self.shape = Shape::Known(w as f64 / h as f64);
                }
            }
        }
    }

    /// A press on the strip's button.
    fn toggle(&mut self, cx: &mut Cx, scope: &mut Scope) {
        let now = scope.data.get_mut::<Session>().map_or(0.0, |s| s.now());
        let session = scope.data.get_mut::<Session>().map(|s| &*s);
        if let Some(t) = self.transport(session) {
            t.toggle(now);
        }
        self.view.redraw(cx);
    }
}

impl Widget for ReaderClip {
    /// A control to the flow around it: a press taps or drag-scrolls, and a
    /// selection never starts on it.
    fn is_interactive(&self) -> bool {
        true
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let video = self.video(cx);
        let before = media::video_word(cx, &video);
        let actions = cx.capture_actions(|cx| self.view.handle_event(cx, event, scope));
        if self.clip.handle_actions(cx, &video, &actions) {
            self.view.redraw(cx);
        }
        let prepared = actions
            .filter_widget_actions(video.widget(cx, ids!(clip)).widget_uid())
            .any(|a| matches!(a.cast(), VideoAction::PlaybackPrepared));
        if self.view.button(cx, ids!(strip.play_btn)).clicked(&actions) {
            self.toggle(cx, scope);
        }
        cx.extend_actions(actions);
        let after = media::video_word(cx, &video);
        if before != after {
            self.view.redraw(cx);
        }
        match event {
            // The platform said what shape the clip is: the box takes it,
            // once, where nothing else had decided.
            Event::VideoPlaybackPrepared(e) if prepared => {
                if self.shape == Shape::Unknown && e.video_width > 0 && e.video_height > 0 {
                    self.shape = Shape::Known(e.video_width as f64 / e.video_height as f64);
                    self.view.redraw(cx);
                }
            }
            // The player gave up on this source: a link stands in for it.
            Event::VideoDecodingError(_) if before != "unprepared" && after == "unprepared" => {
                self.unplayable = true;
                if let Some(t) = self.transport.as_mut() {
                    t.set_running(false);
                }
                self.view.redraw(cx);
            }
            Event::WindowLostFocus(_) | Event::Background => {
                self.background = true;
                self.scrubbing = false;
                let now = scope.data.get_mut::<Session>().map_or(0.0, |s| s.now());
                self.pause(cx, now);
            }
            Event::WindowGotFocus(_) | Event::Foreground => {
                self.background = false;
                self.view.redraw(cx);
            }
            _ => {}
        }
        // Its panel scrolled or switched away: nothing plays unseen.
        if let (Some(props), Some(s)) = (
            scope.props.get::<PanelProps>().cloned(),
            scope.data.get_mut::<Session>(),
        ) {
            if !hosted::on_screen(s, props.slot) && self.wish() {
                let now = s.now();
                self.pause(cx, now);
            }
        }
        // The hairline: a press seeks, and the drag that follows keeps
        // seeking until the finger lifts, even off the bar.
        if let Some(bar) = self.seek {
            let strip = self.view.widget(cx, ids!(strip));
            let area = strip.widget(cx, ids!(bar)).area();
            let at = match event.hits(cx, area) {
                Hit::FingerDown(fe) if fe.is_primary_hit() => {
                    self.scrubbing = true;
                    Some(fe.abs.x)
                }
                Hit::FingerMove(fe) if self.scrubbing => Some(fe.abs.x),
                Hit::FingerUp(fe) if self.scrubbing => {
                    self.scrubbing = false;
                    Some(fe.abs.x)
                }
                Hit::FingerHoverIn(_) => {
                    cx.set_cursor(MouseCursor::Hand);
                    None
                }
                Hit::FingerHoverOut(_) => {
                    cx.set_cursor(MouseCursor::Default);
                    None
                }
                _ => None,
            };
            if let Some(x) = at {
                self.clip.seek(bar.position(x));
                self.view.redraw(cx);
            }
        }
        // The link that stands in for what cannot play.
        if self.unplayable {
            let link = self.view.widget(cx, ids!(link_lbl));
            match event.hits(cx, link.area()) {
                Hit::FingerHoverIn(_) => cx.set_cursor(MouseCursor::Hand),
                Hit::FingerHoverOut(_) => cx.set_cursor(MouseCursor::Default),
                Hit::FingerUp(fe) if fe.is_over && fe.is_primary_hit() && fe.was_tap() => {
                    cx.widget_action(
                        self.widget_uid(),
                        HtmlLinkAction::Clicked { url: self.src.clone(), key_modifiers: fe.modifiers },
                    );
                }
                _ => {}
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, _walk: Walk) -> DrawStep {
        let video = self.video(cx);
        let surface = self.view.widget(cx, ids!(surface));
        let strip = self.view.widget(cx, ids!(strip));
        let link = self.view.widget(cx, ids!(link_lbl));
        if self.unplayable {
            // The word for it, as a link to the source: opened in the
            // browser, which is the sure way to what this build cannot play.
            self.clip.drive(cx, &video, None, false, 0.0);
            media::fill_clip(cx, &surface, &video, false, None);
            surface.set_visible(cx, false);
            strip.set_visible(cx, false);
            link.set_visible(cx, true);
            link.set_text(cx, if self.sound { "audio ↗" } else { "video ↗" });
            let step = self.view.draw_walk(cx, scope, Walk::fit());
            let r = link.area().rect(cx);
            if r.size.x > 0.0 && r.size.y > 0.0 {
                cx.global::<pictures::Pictures>().links.push(r);
            }
            self.play = None;
            self.seek = None;
            return step;
        }
        link.set_visible(cx, false);
        let width = self.box_width(cx);
        let aspect = match self.shape {
            Shape::Known(a) => Some(a),
            Shape::Unknown => None,
        };
        media::surface_aspect(cx, &surface, !self.sound, aspect, Some(width));
        self.fill_poster(cx);

        // The wish, made so. An animation wishes on sight; a clip without
        // a poster is prepared on sight, paused at its start, so its first
        // frame stands as the poster rather than a dark box.
        if self.animation() && self.transport.is_none() {
            if let Some(t) = self.transport(None) {
                t.play(0.0);
            }
        }
        let wish = self.wish();
        let source = Source::Web(self.src.clone());
        self.clip.point_at(cx, &self.src);
        let uid = self.widget_uid();
        // Past the bound, a paused player lets go: the poster, or the dark
        // box, stands again, and the next press prepares it afresh.
        if let_go(cx, uid) && !wish {
            self.clip.reset(cx);
            self.clip.point_at(cx, &self.src);
            self.primed = true;
        }
        if !wish && self.poster.is_empty() && !self.sound && !self.primed && !self.clip.awaiting_seek() {
            self.clip.seek(0.0);
            self.primed = true;
            touch(cx, uid, false);
        }
        if wish {
            touch(cx, uid, true);
        }
        let length = self.state.length;
        let drawn = self.clip.drive(cx, &video, Some(&source), wish, length);
        if let Some(t) = self.transport.as_mut() {
            t.set_running(drawn.playing);
            if let Some(st) = drawn.state {
                t.set_native(st);
            }
        }
        self.state = match self.transport.as_ref() {
            Some(t) => t.state(0.0, length),
            None => PlayerState { length, ..drawn.state.unwrap_or_default() },
        };
        media::fill_clip(cx, &surface, &video, drawn.shown, None);
        media::prime_video(cx, &video);
        surface.set_visible(cx, !self.sound);
        strip.set_visible(cx, true);
        media::fill_player(cx, &strip, Some(&self.state));

        let step = self.view.draw_walk(
            cx,
            scope,
            Walk { width: Size::Fixed(width), height: Size::fit(), ..Walk::default() },
        );

        // Where the controls landed, for the panel's hit table; and whether
        // the box is on the screen at all.
        self.play = media::play_rect(cx, &strip);
        self.seek = SeekBar::from_player(cx, &strip, self.state);
        let p = cx.global::<pictures::Pictures>();
        if let Some(r) = self.play {
            p.controls.push((if self.state.playing { "pause" } else { "play" }.into(), r));
        }
        if let Some(bar) = self.seek {
            p.controls.push(("seek".into(), bar.rect));
        }
        let viewport = p.viewport;
        let mine = self.view.area().rect(cx);
        let off_screen = viewport.is_some_and(|v| {
            mine.size.y > 0.0
                && (mine.pos.y + mine.size.y <= v.pos.y || mine.pos.y >= v.pos.y + v.size.y)
        });
        if off_screen && wish && !self.animation() {
            self.pause(cx, 0.0);
        }
        if drawn.redraw || (wish && !drawn.shown) || self.clip.seek_needs_redraw() {
            self.view.redraw(cx);
        }
        step
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Past the bound, the least recently used paused player is told to let
    /// go — never a playing one, and never twice.
    #[test]
    fn prepared_players_are_bounded_and_the_playing_one_stays() {
        let cx = &mut Cx::new(Box::new(|_, _| {}));
        let uids: Vec<WidgetUid> = (1..=5).map(WidgetUid).collect();
        touch(cx, uids[0], true);
        for uid in &uids[1..=MAX_PREPARED] {
            touch(cx, *uid, false);
        }
        assert!(let_go(cx, uids[1]), "the oldest paused one goes, not the playing one");
        assert!(!let_go(cx, uids[0]));
        assert!(!let_go(cx, uids[1]), "a verdict is taken once");
        // Using one again keeps it; the next one past the bound is the
        // oldest of the rest.
        touch(cx, uids[2], false);
        touch(cx, uids[4], false);
        assert!(let_go(cx, uids[3]));
        assert!(!let_go(cx, uids[2]));
        // Everything playing: nothing is let go, however many.
        let cx = &mut Cx::new(Box::new(|_, _| {}));
        for uid in &uids {
            touch(cx, *uid, true);
        }
        assert!(uids.iter().all(|uid| !let_go(cx, *uid)));
    }
}
