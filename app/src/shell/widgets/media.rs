//! The media kit: what a panel embeds to show a picture, a sound, a moving
//! picture, a place, or a recording under way.
//!
//! Five templates and the functions that fill them, in the shell so that a
//! voice note in a chat, an audio part of a letter and an `.mp3` on a card
//! are one player, and a place shared in a chat and a photo's coordinates
//! are one map. A sound is drawn here and played elsewhere: the *state* of a
//! player — where it is, whether it runs — belongs to the panel instance,
//! which ticks it against the session's clock, and this draws it; playing
//! the bytes is the [`Playback`] capability's, which the kernel owns and a
//! platform supplies. A moving picture is the one exception, and only
//! because it cannot be split — a video *is* its own picture, so one widget
//! draws and plays it, and what is here is the wish, run or hold, carried
//! across to it.
//!
//! - [`MediaPicture`]: a picture at a width, the box shown only while there
//!   is one.
//! - [`MediaVideo`]: a moving picture, played from a file on this device;
//!   the one thing here that does play its own bytes, the platform's player
//!   doing it.
//! - [`MediaPlayer`]: play or pause, the progress as a filled hairline, and
//!   the time — `0:17 / 0:42`.
//! - [`MediaMeter`]: the level of a recording under way, as bars.
//! - [`MediaMap`]: a place, on a snapshot of the map around it with the pin
//!   at its centre; see [`map`](super::map).
//!
//! [`Playback`]: kernel::caps
//! [`MediaPicture`]: struct@MediaPicture
//! [`MediaVideo`]: struct@MediaVideo
//! [`MediaPlayer`]: struct@MediaPlayer
//! [`MediaMeter`]: struct@MediaMeter
//! [`MediaMap`]: struct@MediaMap

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::time::Instant;

use makepad_widgets::makepad_platform::{Texture, TextureFormat, TextureUpdated};
use makepad_widgets::*;

use super::map::Snapshot;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    /** A picture at the text's width, at most 320 wide: what the client
        draws a photo at, and what a phone's column holds. `Image` carries
        no `visible` of its own; the box is what shows and hides it. */
    mod.widgets.MediaPicture = View {
        visible: false
        width: 320, height: Fit
        margin: Inset{top: 2, bottom: 2}
        img := mod.widgets.Image {
            width: Fill, height: Fit
            fit: ImageFit.Horizontal
        }
    }

    /** A moving picture at the size its box is given — the clip itself,
        where a `MediaPicture` shows a still of it. Hidden until there is a
        file on this device to point it at: media is never in the store, and
        a clip is never downloaded until somebody opens it.

        The player's own overlay controls are off. Play and pause are the
        panel's, on the `MediaPlayer` beneath, so a moving picture is
        transported the same way a sound is and there is one grammar for
        both. */
    mod.widgets.MediaVideo = View {
        visible: false
        width: Fill, height: Fill
        align: Align{x: 0.5, y: 0.5}
        clip := mod.widgets.Video {
            width: Fill, height: Fill
            show_controls: false
            draw_bg +: {
                // Video's default shader stretches a Fill walk. Fit against
                // the current draw rectangle so playback and window resizing
                // keep the source proportions, just like the poster.
                get_color_scale_pan: fn() {
                    let source = max(self.source_size, vec2(1.0, 1.0))
                    let target = max(self.rect_size, vec2(1.0, 1.0))
                    let fit = min(target.x / source.x, target.y / source.y)
                    let size = source * fit
                    let coord = (self.pos * target - (target - size) * 0.5) / size
                    if coord.x < 0.0 || coord.x > 1.0 || coord.y < 0.0 || coord.y > 1.0 {
                        return vec4(0.0, 0.0, 0.0, 0.0)
                    }
                    if self.show_thumbnail > 0.5 {
                        return self.thumbnail_texture.sample_as_bgra(coord).xyzw
                    } else if self.yuv_enabled > 0.5 {
                        return self.sample_yuv(coord)
                    } else if self.video_rgba_2d > 0.5 {
                        return self.video_texture_2d.sample(coord)
                    } else {
                        return self.sample_oes(coord)
                    }
                }
            }
        }
    }

    /** The player: play or pause, the progress as a filled hairline, and
        the time. The button is a bordered box like every button; inside a
        list's row it is resolved by the panel's own rectangles, like every
        control in a row. */
    mod.widgets.MediaPlayer = View {
        visible: false
        width: Fill, height: Fit
        flow: Right
        align: Align{y: 0.5}
        spacing: 8
        margin: Inset{top: 3, bottom: 3}
        play_btn := mod.widgets.SBtn { text: "play" }
        bar := View {
            width: Fill, height: 3
            show_bg: true
            draw_bg +: {
                progress: uniform(0.0)
                color: #141414
                pixel: fn() {
                    if self.pos.x <= self.progress {
                        return vec4(0.078, 0.078, 0.078, 1.0)
                    }
                    return vec4(0.863, 0.863, 0.863, 1.0)
                }
            }
        }
        time_lbl := mod.widgets.SLabel {
            width: Fit, text: "", draw_text +: { color: #909090 }
        }
    }

    /** The level of a recording under way: thirty-two bars, filled up to
        the level. */
    mod.widgets.MediaMeter = View {
        width: Fill, height: 12
        show_bg: true
        draw_bg +: {
            level: uniform(0.0)
            color: #141414
            pixel: fn() {
                let bars = 32.0
                let i = floor(self.pos.x * bars)
                let gap = fract(self.pos.x * bars)
                if gap > 0.7 {
                    return vec4(0.0, 0.0, 0.0, 0.0)
                }
                if (i + 0.5) / bars <= self.level {
                    return vec4(0.078, 0.078, 0.078, 1.0)
                }
                return vec4(0.863, 0.863, 0.863, 1.0)
            }
        }
    }

    /** A place: a snapshot of the map around it, the pin at its centre,
        320 by 160. */
    mod.widgets.MediaMap = View {
        visible: false
        width: 320, height: 160
        margin: Inset{top: 2, bottom: 2}
        img := mod.widgets.Image {
            width: Fill, height: Fill
            fit: ImageFit.Stretch
        }
    }
}

/// Where a player stands: the position and the length in seconds, and
/// whether it runs. The panel instance owns one per thing it can play and
/// ticks it against the clock; this is what a draw is handed.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PlayerState {
    pub playing: bool,
    pub position: f64,
    pub length: f64,
}

impl PlayerState {
    /// How far along, 0 to 1.
    #[must_use]
    pub fn fraction(&self) -> f32 {
        if self.length <= 0.0 {
            return 0.0;
        }
        (self.position / self.length).clamp(0.0, 1.0) as f32
    }

    /// `0:17 / 0:42`.
    #[must_use]
    pub fn time_line(&self) -> String {
        format!("{} / {}", fmt_secs(self.position), fmt_secs(self.length))
    }
}

/// A length as a player spells it: `0:42`, `3:41`, `1:02:05`.
#[must_use]
pub fn fmt_secs(secs: f64) -> String {
    let secs = secs.max(0.0).floor() as i64;
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Fills a `MediaPlayer`: shown with a state, hidden without one. The
/// button reads `pause` while it runs.
pub fn fill_player(cx: &mut Cx, player: &WidgetRef, st: Option<&PlayerState>) {
    player.set_visible(cx, st.is_some());
    let Some(st) = st else { return };
    player
        .button(cx, ids!(play_btn))
        .set_text(cx, if st.playing { "pause" } else { "play" });
    if let Some(mut bar) = player.view(cx, ids!(bar)).borrow_mut() {
        bar.draw_bg
            .set_uniform(cx, live_id!(progress), &[st.fraction()]);
    }
    player
        .label(cx, ids!(time_lbl))
        .set_text(cx, &st.time_line());
}

/// The play button's rectangle, for the hit a row registers it under.
#[must_use]
pub fn play_rect(cx: &mut Cx, player: &WidgetRef) -> Option<Rect> {
    let r = player.button(cx, ids!(play_btn)).area().rect(cx);
    (r.size.x > 0.0 && r.size.y > 0.0).then_some(r)
}

/// The progress bar's width with the full control row as its hit height.
/// The drawn hairline stays thin; seeking does not require hitting three pixels.
#[must_use]
pub fn seek_rect(cx: &Cx, player: &WidgetRef) -> Option<Rect> {
    let bar = player.view(cx, ids!(bar)).area().rect(cx);
    let row = player.area().rect(cx);
    (bar.size.x > 0.0 && row.size.y > 0.0).then_some(Rect {
        pos: dvec2(bar.pos.x, row.pos.y),
        size: dvec2(bar.size.x, row.size.y),
    })
}

/// Sets a `MediaMeter`'s level, 0 to 1.
pub fn fill_meter(cx: &mut Cx, meter: &WidgetRef, level: f32) {
    if let Some(mut m) = meter.borrow_mut::<View>() {
        m.draw_bg
            .set_uniform(cx, live_id!(level), &[level.clamp(0.0, 1.0)]);
    }
}

/// A recording's level as a fake microphone hears it: a wave over the
/// seconds, so a level that never moves is never mistaken for a live one.
#[must_use]
pub fn fake_level(elapsed: f64) -> f32 {
    let t = elapsed * 7.3;
    (0.45 + 0.35 * (t.sin() * (t * 0.37).cos())).clamp(0.05, 1.0) as f32
}

/// Fills a `MediaPicture` from encoded bytes — PNG or JPEG, decoded by
/// what they are — and answers whether there is a picture to show. With
/// `decode` false the box is shown as it stands, for a caller that knows
/// the picture is already in it.
pub fn fill_picture(cx: &mut Cx, picture: &WidgetRef, bytes: Option<&[u8]>, decode: bool) -> bool {
    let shown = bytes.is_some_and(|b| {
        !decode
            || picture
                .widget(cx, ids!(img))
                .as_image()
                .load_image_from_data(cx, b)
                .is_ok()
    });
    picture.set_visible(cx, shown);
    shown
}

/// What one draw over a `MediaVideo` found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VideoDrawn {
    /// The player has a picture in it — so the panel can put its poster
    /// away and let the moving one have the box.
    pub shown: bool,
    /// The wish as it stands afterwards: `false` once the clip has run to
    /// its end, so the button goes back to reading *play*.
    pub playing: bool,
}

/// Owns playback for the lifetime of a panel's widget. Makepad's `Video`
/// does not stop its native player on drop, so its textures must survive
/// the panel until the platform acknowledges cleanup.
#[derive(Default)]
pub struct VideoPlayback {
    lease: Option<VideoLease>,
    seek: Option<VideoSeek>,
}

struct VideoSeek {
    position: f64,
    sent: Option<Instant>,
}

struct VideoLease {
    clip: WidgetRef,
    retired: Rc<RefCell<Vec<WidgetRef>>>,
}

impl Drop for VideoLease {
    fn drop(&mut self) {
        self.retired.borrow_mut().push(self.clip.clone());
        SignalToUI::set_ui_signal();
    }
}

#[derive(Default)]
struct RetiredVideos(Rc<RefCell<Vec<WidgetRef>>>);

/// Runs at the app root, including after a panel or a whole stage has gone.
/// A dropped owner hands its player here; cleanup is queued with a `Cx`,
/// and the player keeps its textures until ResourcesReleased arrives.
/// Late YUV allocations must reach it too, including when a panel closes
/// before the platform has processed its prepare request.
pub fn cleanup_videos(cx: &mut Cx, event: Option<&Event>) {
    if super::super::boot::frame_log() {
        match event {
            Some(Event::VideoPlaybackPrepared(e)) => eprintln!("video {:?}: prepared", e.video_id),
            Some(Event::VideoPlaybackResourcesReleased(e)) => {
                eprintln!("video {:?}: released", e.video_id);
            }
            _ => {}
        }
    }
    if !cx.has_global::<RetiredVideos>() {
        return;
    }
    let queue = cx.get_global::<RetiredVideos>().0.clone();
    let mut retired = std::mem::take(&mut *queue.borrow_mut());
    retired.retain(|clip| {
        let player = clip.as_video();
        player.stop_and_cleanup_resources(cx);
        if let Some(
            event @ (Event::VideoYuvTexturesReady(_) | Event::VideoPlaybackResourcesReleased(_)),
        ) = event
        {
            clip.handle_event(cx, event, &mut Scope::empty());
        }
        !player.is_unprepared()
    });
    queue.borrow_mut().extend(retired);
}

impl VideoPlayback {
    fn own(&mut self, cx: &mut Cx, clip: WidgetRef) {
        if clip.is_empty() || self.lease.as_ref().is_some_and(|lease| lease.clip == clip) {
            return;
        }
        self.lease = Some(VideoLease {
            clip,
            retired: cx.global::<RetiredVideos>().0.clone(),
        });
    }

    /// Keeps the latest seek until the file and the native player are ready.
    /// Seeking leaves the panel's play/pause wish alone.
    pub fn seek(&mut self, position: f64) {
        if position.is_finite() {
            self.seek = Some(VideoSeek { position: position.max(0.0), sent: None });
        }
    }

    /// A paused player still needs draws while a seek waits for preparation.
    #[must_use]
    pub fn awaiting_seek(&self) -> bool {
        self.seek.as_ref().is_some_and(|s| s.sent.is_none())
    }

    /// Keep drawing until preparation finishes and the nonzero preview clears,
    /// even if a paused player sends no further frames after landing on a keyframe.
    #[must_use]
    pub fn seek_needs_redraw(&self) -> bool {
        self.seek.as_ref().is_some_and(|s| s.sent.is_none() || s.position > 0.0)
    }

    /// Show the requested position while the native seek catches up, with a
    /// one-second timeout for nonzero targets even while paused: macOS may land
    /// on a nearby keyframe. A paused seek to zero must keep that position,
    /// since Makepad ignores zero timestamps.
    #[must_use]
    pub fn state(&mut self, cx: &Cx, video: &WidgetRef, length: f64) -> PlayerState {
        let mut st = video_state(cx, video, length);
        if let Some(seek) = &self.seek {
            let position = seek.position.min(st.length.max(0.0));
            if seek.sent.is_some_and(|sent| {
                (st.position - position).abs() < 0.1
                    || ((st.playing || position > 0.0) && sent.elapsed().as_secs_f64() > 1.0)
            }) {
                self.seek = None;
            } else {
                st.position = position;
            }
        }
        st
    }

    fn apply_seek(&mut self, cx: &mut Cx, clip: &VideoRef) {
        let Some(seek) = self.seek.as_mut().filter(|s| s.sent.is_none()) else { return };
        if clip.is_unprepared() {
            clip.prepare_playback(cx);
        }
        if clip.is_prepared() {
            clip.pause_playback(cx);
        }
        if clip.is_playing() || clip.is_paused() {
            let length = clip.total_duration_ms() as f64 / 1000.0;
            if length > 0.0 {
                seek.position = seek.position.min(length);
            }
            clip.seek_to(cx, (seek.position * 1000.0).round() as u64);
            seek.sent = Some(Instant::now());
        }
    }

    /// Points a `MediaVideo` at the clip at `path` — at nothing, where there is
    /// no clip — and carries the panel's wish across to its player: `true` runs
    /// it, `false` holds it.
    ///
    /// A verb has no `Cx` to reach a player through, so a panel keeps the wish
    /// and the draw is where it is made so, which is why this is the one thing
    /// in the kit that does more than draw.
    ///
    /// Three rules hold it together. A player only takes a source while it has
    /// nothing prepared, so the file is handed over then and only then — which
    /// is also how a player that has let a finished clip go is pointed back at
    /// it. A clip that has run to its end is released here and the wish falls to
    /// `false`, so the next press of *play* starts it from the top rather than
    /// doing nothing. And the box is shown only once the player really has a
    /// picture: while it is still preparing there is nothing in it but a
    /// rectangle, and the panel's poster is the better thing to be looking at.
    pub fn drive(
        &mut self,
        cx: &mut Cx,
        video: &WidgetRef,
        path: Option<&Path>,
        playing: bool,
    ) -> VideoDrawn {
        let widget = video.widget(cx, ids!(clip));
        self.own(cx, widget.clone());
        let clip = widget.as_video();
        let mut playing = playing;
        match path {
            Some(path) => {
                if clip.is_unprepared() {
                    clip.set_source(VideoDataSource::Filesystem {
                        path: path.to_string_lossy().into_owned(),
                    });
                }
                if clip.has_completed() {
                    clip.stop_and_cleanup_resources(cx);
                    playing = false;
                    if !self.awaiting_seek() {
                        self.seek = None;
                    }
                } else {
                    self.apply_seek(cx, &clip);
                    if playing {
                        if clip.is_paused() {
                            clip.resume_playback(cx);
                        } else {
                            clip.begin_playback(cx);
                        }
                    } else if clip.is_playing() {
                        clip.pause_playback(cx);
                    }
                }
            }
            // No clip on this line yet — a box left over a player from the
            // line before is given back, so nothing plays on unseen. The wish
            // to play stands: it is what starts the clip the draw that finds
            // its file (review, 2026-09-07: play pressed mid-download was lost).
            None => {
                clip.stop_and_cleanup_resources(cx);
            }
        }
        // Shown once the platform has it playing or paused. Until then the
        // caller's poster stands, and the caller keeps drawing to look again:
        // a box that is not visible is never drawn, and a player's own redraw
        // request from inside it is a request over no area.
        let shown = clip.is_playing() || clip.is_paused();
        video.set_visible(cx, shown);
        VideoDrawn { shown, playing }
    }
}

/// The platform player's state, in a word — for a trace, so a clip that
/// never shows can be read off: `unprepared`, `preparing`, `prepared`,
/// `playing`, `paused`, `completed`.
#[must_use]
pub fn video_word(cx: &Cx, video: &WidgetRef) -> &'static str {
    let clip = video.widget(cx, ids!(clip)).as_video();
    if clip.is_playing() {
        "playing"
    } else if clip.is_paused() {
        "paused"
    } else if clip.has_completed() {
        "completed"
    } else if clip.is_prepared() {
        "prepared"
    } else if clip.is_preparing() {
        "preparing"
    } else if clip.is_unprepared() {
        "unprepared"
    } else {
        "releasing"
    }
}

/// Where a `MediaVideo`'s own player stands, in the same words a drawn
/// [`PlayerState`] uses. The position and the length are the platform's,
/// in seconds; until it reports a length — a player is prepared a moment
/// after it is asked, not at once — `length` stands in, which is the
/// seconds the row itself carries.
#[must_use]
pub fn video_state(cx: &Cx, video: &WidgetRef, length: f64) -> PlayerState {
    let clip = video.widget(cx, ids!(clip)).as_video();
    let known = clip.total_duration_ms() as f64 / 1000.0;
    PlayerState {
        playing: clip.is_playing(),
        position: clip.current_position_ms() as f64 / 1000.0,
        length: if known > 0.0 { known } else { length },
    }
}

/// Fills a `MediaMap` with a snapshot, as a texture of its own.
pub fn fill_map(cx: &mut Cx, map: &WidgetRef, snapshot: Option<&Snapshot>) {
    map.set_visible(cx, snapshot.is_some());
    let Some(s) = snapshot else { return };
    let texture = Texture::new_with_format(
        cx,
        TextureFormat::VecBGRAu8_32 {
            width: s.width,
            height: s.height,
            data: Some(s.pixels.clone()),
            updated: TextureUpdated::Full,
        },
    );
    map.widget(cx, ids!(img))
        .as_image()
        .set_texture(cx, Some(texture));
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_widgets::makepad_platform::event::video_playback::{
        VideoPlaybackPreparedEvent, VideoPlaybackResourcesReleasedEvent, VideoTextureUpdatedEvent,
        VideoYuvTexturesReady,
    };

    fn test_video(cx: &mut Cx) -> WidgetRef {
        // Construct without applying a script: the player retains its default
        // id (0), so platform acknowledgements can be supplied deterministically.
        cx.with_vm(|vm| WidgetRef::new_with_inner(Box::new(Video::script_new(vm))))
    }

    fn released(video_id: LiveId) -> Event {
        Event::VideoPlaybackResourcesReleased(VideoPlaybackResourcesReleasedEvent { video_id })
    }

    fn prepared() -> Event {
        Event::VideoPlaybackPrepared(VideoPlaybackPreparedEvent {
            video_id: LiveId(0),
            video_width: 16,
            video_height: 16,
            duration: 1000,
            is_seekable: true,
            video_tracks: Vec::new(),
            audio_tracks: Vec::new(),
        })
    }

    fn test_video_box(cx: &mut Cx) -> WidgetRef {
        let clip = test_video(cx);
        let video = cx.with_vm(|vm| {
            let mut view = View::script_new(vm);
            view.children.push((live_id!(clip), clip));
            WidgetRef::new_with_inner(Box::new(view))
        });
        // Give this Cx its own tree, as the app root does. An uninitialized
        // Cx's fallback tree is shared across concurrent tests.
        makepad_widgets::widget_tree::set_ui_root(cx, &video);
        video
    }

    fn position(position_ms: u128) -> Event {
        Event::VideoTextureUpdated(VideoTextureUpdatedEvent {
            video_id: LiveId(0),
            current_position_ms: position_ms,
            yuv: Default::default(),
            rgba_gl_2d: false,
        })
    }

    #[test]
    fn seeking_waits_for_the_file_and_player_and_keeps_it_paused() {
        let cx = &mut Cx::new(Box::new(|_, _| {}));
        let video = test_video_box(cx);
        let clip = video.widget(cx, ids!(clip));
        let mut owner = VideoPlayback::default();
        let path = Some(Path::new("test.mp4"));

        owner.seek(0.75);
        assert_eq!(owner.drive(cx, &video, None, false), VideoDrawn::default());
        assert!(owner.awaiting_seek());
        owner.drive(cx, &video, path, false);
        assert!(clip.as_video().is_preparing());
        owner.seek(0.4); // Only the latest position survives preparation.
        clip.handle_event(cx, &prepared(), &mut Scope::empty());
        assert_eq!(owner.drive(cx, &video, path, false), VideoDrawn { shown: true, playing: false });
        assert!(clip.as_video().is_paused());
        assert!(!owner.awaiting_seek());
        assert_eq!(owner.state(cx, &video, 1.0).position, 0.4);

        // A stale frame must not snap the progress back. Once the seek lands,
        // subsequent frames control the position again.
        clip.handle_event(cx, &position(100), &mut Scope::empty());
        assert_eq!(owner.state(cx, &video, 1.0).position, 0.4);
        clip.handle_event(cx, &position(400), &mut Scope::empty());
        assert_eq!(owner.state(cx, &video, 1.0).position, 0.4);
        clip.handle_event(cx, &position(550), &mut Scope::empty());
        assert_eq!(owner.state(cx, &video, 1.0).position, 0.55);
    }

    #[test]
    fn seeking_preserves_playback_and_clamps_to_the_native_duration() {
        let cx = &mut Cx::new(Box::new(|_, _| {}));
        let video = test_video_box(cx);
        let clip = video.widget(cx, ids!(clip));
        let mut owner = VideoPlayback::default();
        let path = Some(Path::new("test.mp4"));
        owner.drive(cx, &video, path, true);
        clip.handle_event(cx, &prepared(), &mut Scope::empty());

        owner.seek(5.0);
        assert_eq!(owner.drive(cx, &video, path, true), VideoDrawn { shown: true, playing: true });
        assert_eq!(owner.state(cx, &video, 14.0), PlayerState {
            position: 1.0, length: 1.0, playing: true,
        });

        clip.handle_event(cx, &position(800), &mut Scope::empty());
        owner.drive(cx, &video, path, false);
        owner.seek(-1.0);
        owner.drive(cx, &video, path, false);
        clip.handle_event(cx, &position(0), &mut Scope::empty());
        assert_eq!(owner.state(cx, &video, 14.0), PlayerState {
            position: 0.0, length: 1.0, playing: false,
        });
        owner.drive(cx, &video, path, true);
        clip.handle_event(cx, &position(50), &mut Scope::empty());
        assert_eq!(owner.state(cx, &video, 14.0).position, 0.05);
        assert!(clip.as_video().is_playing());
    }

    #[test]
    fn a_paused_seek_adopts_the_native_position_after_the_preview_times_out() {
        let cx = &mut Cx::new(Box::new(|_, _| {}));
        let video = test_video_box(cx);
        let clip = video.widget(cx, ids!(clip));
        let mut owner = VideoPlayback::default();
        let path = Some(Path::new("test.mp4"));
        owner.seek(0.75);
        assert!(owner.seek_needs_redraw());
        owner.drive(cx, &video, path, false);
        clip.handle_event(cx, &prepared(), &mut Scope::empty());
        owner.drive(cx, &video, path, false);

        // macOS can land on a sync frame outside the target's tolerance.
        // Initially show the request, then the actual frame, still paused.
        clip.handle_event(cx, &position(500), &mut Scope::empty());
        assert_eq!(owner.state(cx, &video, 1.0).position, 0.75);
        assert!(owner.seek_needs_redraw(), "a paused preview still needs a draw at timeout");
        owner.seek.as_mut().unwrap().sent = Some(Instant::now() - std::time::Duration::from_secs(2));
        assert!(owner.seek_needs_redraw(), "the final draw must replace the expired preview");
        assert_eq!(owner.state(cx, &video, 1.0), PlayerState {
            position: 0.5, length: 1.0, playing: false,
        });
        assert!(!owner.seek_needs_redraw(), "the paused player can stop drawing once it settles");

        // Zero is special: Makepad ignores its timestamp, so the old native
        // position must not replace a paused seek to the start after timeout.
        owner.seek(0.0);
        assert!(owner.seek_needs_redraw(), "a queued zero seek still needs to be sent");
        owner.drive(cx, &video, path, false);
        clip.handle_event(cx, &position(0), &mut Scope::empty());
        owner.seek.as_mut().unwrap().sent = Some(Instant::now() - std::time::Duration::from_secs(2));
        assert_eq!(owner.state(cx, &video, 1.0), PlayerState {
            position: 0.0, length: 1.0, playing: false,
        });
        assert!(!owner.seek_needs_redraw(), "keeping zero pinned must not draw forever");
        owner.drive(cx, &video, path, true);
        assert_eq!(owner.state(cx, &video, 1.0), PlayerState {
            position: 0.5, length: 1.0, playing: true,
        });
    }

    #[test]
    fn a_closed_video_survives_until_native_cleanup_finishes() {
        let cx = &mut Cx::new(Box::new(|_, _| {}));
        for cycle in 0..32 {
            let clip = test_video(cx);
            let weak = clip.downgrade();
            let mut owner = VideoPlayback::default();
            owner.own(cx, clip.clone());
            clip.as_video().set_source(VideoDataSource::Filesystem {
                path: "test.mp4".into(),
            });
            clip.as_video().begin_playback(cx);
            assert!(clip.as_video().is_preparing());
            if cycle % 4 != 0 {
                clip.handle_event(cx, &prepared(), &mut Scope::empty());
                assert!(clip.as_video().is_playing());
                if cycle % 4 == 2 {
                    clip.as_video().pause_playback(cx);
                } else if cycle % 4 == 3 {
                    clip.as_video().stop_and_cleanup_resources(cx);
                }
            }
            drop(clip);
            drop(owner);

            cleanup_videos(cx, None);
            assert!(weak.upgrade().unwrap().as_video().is_cleaning_up());

            // A prepare queued before close can still allocate the YUV planes.
            let planes = Event::VideoYuvTexturesReady(VideoYuvTexturesReady::planes(
                LiveId(0),
                Texture::new_with_format(cx, TextureFormat::VideoYuvPlane),
                Texture::new_with_format(cx, TextureFormat::VideoYuvPlane),
                Texture::new_with_format(cx, TextureFormat::VideoYuvPlane),
            ));
            cleanup_videos(cx, Some(&planes));
            drop(planes);

            // A late prepared event must not restart the retired player.
            cleanup_videos(cx, Some(&prepared()));
            assert!(weak.upgrade().unwrap().as_video().is_cleaning_up());
            cleanup_videos(cx, Some(&released(LiveId(1))));
            assert!(weak.upgrade().is_some());
            cleanup_videos(cx, Some(&released(LiveId(0))));
            assert!(weak.upgrade().is_none());
            assert!(cx.get_global::<RetiredVideos>().0.borrow().is_empty());
        }
    }

    #[test]
    fn an_idle_video_needs_no_native_cleanup_acknowledgement() {
        let cx = &mut Cx::new(Box::new(|_, _| {}));
        let clip = test_video(cx);
        let weak = clip.downgrade();
        let mut owner = VideoPlayback::default();
        owner.own(cx, clip.clone());
        drop(clip);
        drop(owner);
        cleanup_videos(cx, None);
        assert!(weak.upgrade().is_none());
        assert!(cx.get_global::<RetiredVideos>().0.borrow().is_empty());
    }

    #[test]
    fn a_player_spells_where_it_stands() {
        let st = PlayerState {
            playing: true,
            position: 17.0,
            length: 42.0,
        };
        assert_eq!(st.time_line(), "0:17 / 0:42");
        assert!((st.fraction() - 17.0 / 42.0).abs() < 1e-6);
        let past = PlayerState {
            playing: false,
            position: 50.0,
            length: 42.0,
        };
        assert_eq!(past.fraction(), 1.0);
        assert_eq!(fmt_secs(221.0), "3:41");
        assert_eq!(fmt_secs(3725.0), "1:02:05");
        assert_eq!(PlayerState::default().fraction(), 0.0);
    }

    #[test]
    fn the_fake_level_moves_and_stays_in_range() {
        let a = fake_level(0.0);
        let b = fake_level(0.5);
        assert_ne!(a, b);
        for i in 0..200 {
            let l = fake_level(f64::from(i) * 0.1);
            assert!((0.05..=1.0).contains(&l));
        }
    }
}
