//! A panel owns one native video. Transcript rows only borrow its picture,
//! so replacing a virtual row or its selection twin never replaces the player.

use makepad_widgets::*;
use makepad_widgets::widget_tree::CxWidgetExt;

use crate::shell::widgets::media::{self, PlayerState, VideoPlayback};
use super::super::model::{Msg, MsgId};
use super::super::panels::playback::Playback;

/// One media surface for the poster, shared video and download feedback.
/// The panel's hidden source owns event delivery, even as rows are recycled.
#[derive(Script, ScriptHook, Widget)]
pub struct InlineVideoSlot {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[rust]
    aspect: f64,
}

impl Widget for InlineVideoSlot {
    fn handle_event(&mut self, _cx: &mut Cx, _event: &Event, _scope: &mut Scope) {}

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, mut walk: Walk) -> DrawStep {
        // Both the poster and the decoded frames fill this one rectangle.
        // Fit the whole surface, including portrait clips, into the column.
        let available = cx.peek_walk_turtle(Walk { width: Size::fill(), ..walk }).size.x;
        let aspect = if self.aspect > 0.0 { self.aspect } else { 16.0 / 9.0 };
        let width = available.clamp(0.0, 320.0).min(480.0 * aspect);
        walk.width = Size::Fixed(width);
        walk.height = Size::Fixed(width / aspect);
        self.view.draw_walk(cx, scope, walk)
    }
}

pub fn has_video(m: &Msg) -> bool {
    m.media.as_ref().is_some_and(|md| matches!(md.kind.as_str(), "video" | "circle" | "animation"))
}

/// Reserve the final video dimensions before downloading or preparing it.
/// Thumbnail dimensions must never determine a video's transcript height.
pub fn fill_poster(cx: &mut Cx, slot: &WidgetRef, m: &Msg, dir: Option<&std::path::Path>) -> bool {
    if let Some(mut slot) = slot.borrow_mut::<InlineVideoSlot>() {
        slot.view.visible = has_video(m);
        let fallback = if m.media.as_ref().is_some_and(|md| md.kind == "circle") { 1.0 } else { 16.0 / 9.0 };
        slot.aspect = m.media.as_ref().and_then(|md| Some((md.w?, md.h?)))
            .filter(|(w, h)| *w > 0 && *h > 0)
            .map_or(fallback, |(w, h)| w as f64 / h as f64);
    }
    let poster = slot.child(live_id!(poster));
    super::pictures::photo(cx, &poster, m.media.as_ref().filter(|_| has_video(m)), dir)
}

pub fn fill_slot(cx: &mut Cx, slot: &WidgetRef, video: &WidgetRef, shown: bool, note: Option<&str>) {
    if let Some(mut holder) = slot.child(live_id!(playback)).borrow_mut::<View>() {
        if holder.children.first().map(|(_, widget)| widget) != shown.then_some(video) {
            holder.children.clear();
            if shown {
                holder.children.push((live_id!(video), video.clone()));
            }
            cx.widget_tree_mark_dirty(holder.widget_uid());
        }
    }
    if shown {
        slot.child(live_id!(poster)).set_visible(cx, false);
    }
    let status = slot.child(live_id!(status));
    status.set_visible(cx, note.is_some());
    status.label(cx, ids!(download_lbl)).set_text(cx, note.unwrap_or(""));
}

#[derive(Default)]
pub struct InlineVideo {
    playback: VideoPlayback,
    source: Option<(MsgId, Option<String>)>,
    last_word: String,
    frame_ready: bool,
}

pub struct InlineDrawn {
    pub shown: bool,
    pub player: Option<PlayerState>,
    pub note: Option<String>,
    pub redraw: bool,
}

impl InlineVideo {
    pub fn reset(&mut self, cx: &mut Cx) {
        self.playback.reset(cx);
        self.source = None;
        self.frame_ready = false;
    }

    /// Prepared/playing only says the decoder has started. Keep the poster
    /// until this particular player has delivered its first texture, even
    /// when that frame's timestamp is zero.
    pub fn handle_actions(&mut self, cx: &mut Cx, video: &WidgetRef, actions: &Actions) -> bool {
        let clip = video.widget(cx, ids!(clip));
        for action in actions.filter_widget_actions(clip.widget_uid()) {
            if let VideoAction::TextureUpdated = action.cast() {
                if clip.as_video().is_playing() || clip.as_video().is_paused() {
                    self.frame_ready = true;
                    clip.as_video().should_dispatch_texture_updates(false);
                    return true;
                }
            }
        }
        false
    }

    pub fn drive(
        &mut self, cx: &mut Cx, video: &WidgetRef, player: &mut Playback, m: &Msg, now: f64,
    ) -> InlineDrawn {
        let source = (m.id, m.media.as_ref().and_then(|md| md.clip.clone()));
        if self.source.as_ref() != Some(&source) {
            self.playback.reset(cx);
            self.source = Some(source);
            self.frame_ready = false;
        }
        let native = player.plays_clip(m);
        let file = native.then(|| player.clip_file(m)).flatten();
        let drawn = self.playback.drive(cx, video, file.as_deref(), player.running());
        if !drawn.shown {
            self.frame_ready = false;
        }
        video.widget(cx, ids!(clip)).as_video().should_dispatch_texture_updates(!self.frame_ready);
        let shown = drawn.shown && self.frame_ready;
        video.set_visible(cx, shown);
        if crate::shell::boot::frame_log() {
            let word = media::video_word(cx, video);
            if self.last_word != word {
                eprintln!("inline video: line {} {word} (wanted={}, cached={}, widget={:?})",
                    m.id, player.running(), file.is_some(), video.widget_uid());
                self.last_word = word.to_string();
            }
        }
        player.set_running(drawn.playing);
        if drawn.shown {
            let length = m.media.as_ref().and_then(|md| md.secs).unwrap_or(0) as f64;
            let mut state = self.playback.state(cx, video, length);
            state.playing = drawn.playing;
            player.set_native_state(state);
        }
        let note = player.download_note(m);
        InlineDrawn {
            shown,
            player: player.player_state(m, now),
            redraw: player.playing(now) || note.is_some(),
            note,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;
    use kernel::{session::Session, store::Store};
    use makepad_widgets::makepad_platform::event::video_playback::{
        VideoPlaybackPreparedEvent, VideoPlaybackResourcesReleasedEvent, VideoTextureUpdatedEvent,
    };
    use crate::apps::telegram::{model, seed::STELAXIS, TELEGRAM};

    /// Draw the actual surface template across the poster/loading/video
    /// handoff. Its height is also the following message's scroll position.
    #[cfg(headless)]
    #[test]
    fn inline_media_keeps_its_rectangle_through_playback() {
        use std::cell::{Cell, RefCell};

        static APPS: &[&dyn kernel::app::App] = &[&TELEGRAM];
        let session = Session::fake(APPS);
        let mut msg = model::history(session.store(), STELAXIS).iter()
            .find(|m| has_video(m)).unwrap().clone();
        let reference = msg.media.as_ref().unwrap().reference.clone();
        let finished = Rc::new(Cell::new(false));
        let seen = finished.clone();
        let mut root = WidgetRef::empty();
        let mut video = WidgetRef::empty();
        let mut pass = None;
        let mut draw_list: Option<DrawList> = None;
        let mut frame = 0;
        let mut baseline = None;
        let formats = [
            (1920, 1080, "video"), (1080, 1920, "video"), (1080, 1080, "video"),
            (1920, 240, "video"), (0, 0, "video"), (0, 0, "circle"),
        ];
        let widths = [200.0, 640.0];
        let count = formats.len() * widths.len() * 7;
        let cx = Rc::new(RefCell::new(Cx::new(Box::new(move |cx, event| match event {
            Event::Startup => {
                (root, video) = cx.with_vm(|vm| {
                    makepad_widgets::script_mod(vm);
                    crate::shell::script_mod(vm);
                    crate::apps::telegram::ui::script_mod(vm);
                    let value = script_eval!(vm, {
                        use mod.prelude.widgets.*
                        mod.widgets.View {
                            width: Fill, height: Fit, flow: Down, spacing: 2
                            surface := mod.widgets.TelegramInlineVideo {}
                            following := mod.widgets.View { width: Fill, height: 30 }
                        }
                    });
                    let root = WidgetRef::script_from_value(vm, value);
                    let value = script_eval!(vm, { mod.widgets.MediaVideo {} });
                    (root, WidgetRef::script_from_value(vm, value))
                });
                makepad_widgets::widget_tree::set_ui_root(cx, &root);
                let p = DrawPass::new(cx);
                p.set_size(cx, dvec2(640.0, 800.0));
                pass = Some(p);
                draw_list = Some(DrawList::new(cx));
                cx.redraw_all();
            }
            Event::Draw(event) if frame < count => {
                let phase = frame % 7;
                let scenario = frame / 7;
                let width = widths[scenario / formats.len()];
                let (w, h, kind) = formats[scenario % formats.len()];
                let md = msg.media.as_mut().unwrap();
                md.w = Some(w);
                md.h = Some(h);
                md.kind = kind.into();
                md.reference = if phase > 0 { reference.clone() } else { None };
                let surface = root.child(live_id!(surface));
                assert_eq!(fill_poster(cx, &surface, &msg, None), phase > 0);
                let playing = matches!(phase, 4 | 5);
                video.set_visible(cx, playing);
                fill_slot(cx, &surface, &video, playing, (phase == 2).then_some("downloading 25%"));
                assert_eq!(surface.child(live_id!(poster)).visible(), phase > 0 && !playing);
                let mut draw = CxDraw::new(cx, event);
                let pass = pass.as_ref().unwrap();
                draw.begin_pass(pass, Some(1.0));
                let list = draw_list.as_mut().unwrap();
                list.begin_always(&mut draw);
                let mut cx = Cx2d::new(&mut draw);
                cx.begin_root_turtle(dvec2(width, 800.0), Layout::default());
                root.draw_all(&mut cx, &mut Scope::empty());
                cx.end_turtle();
                let rect = surface.area().rect(&cx);
                let following = root.child(live_id!(following)).area().rect(&cx);
                assert!(rect.size.x > 0.0 && rect.size.y > 0.0);
                assert!(rect.size.x <= width && rect.size.x <= 320.0 && rect.size.y <= 480.0);
                let aspect = if w > 0 && h > 0 { w as f64 / h as f64 }
                    else if kind == "circle" { 1.0 } else { 16.0 / 9.0 };
                assert!((rect.size.x / rect.size.y - aspect).abs() < 0.001);
                if phase == 0 {
                    baseline = Some((rect, following));
                } else {
                    assert_eq!((rect, following), baseline.unwrap(),
                        "phase {phase} moved the media or following row for {w}x{h} in a {width}px column");
                    let content = if playing {
                        video.widget(&cx, ids!(clip))
                    } else {
                        surface.widget(&cx, ids!(poster.img))
                    };
                    assert_eq!(content.area().rect(&cx), rect,
                        "poster and video must occupy the same rectangle, even with a mismatched thumbnail");
                }
                frame += 1;
                if frame < count {
                    cx.redraw_area_in_draw(root.area());
                } else {
                    seen.set(true);
                }
                list.end(&mut draw);
                draw.end_pass(pass);
            }
            _ => {}
        }))));
        Cx::headless_event_loop_for_draw_cycles(cx, count);
        assert!(finished.get(), "all media shapes and playback phases must draw");
    }

    #[test]
    fn native_players_pause_when_another_panel_takes_playback() {
        static APPS: &[&dyn kernel::app::App] = &[&TELEGRAM];
        let session = Session::fake(APPS);
        let mut msg = model::history(session.store(), STELAXIS).iter()
            .find(|m| has_video(m)).unwrap().clone();
        msg.media.as_mut().unwrap().clip = Some("tg:native-handoff".into());
        let dir = std::env::temp_dir().join(format!("superapp-native-handoff-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("blobs")).unwrap();
        std::fs::write(dir.join("blobs").join(kernel::caps::file_name("tg:native-handoff")),
            b"\x00\x00\x00\x18ftypisom").unwrap();
        let store = Rc::new(Store::open(Some(&dir.join("store.sqlite")), &[]).unwrap());
        let mut players = [Playback::new(store.clone(), msg.id), Playback::new(store, msg.id)];
        let mut owners = [InlineVideo::default(), InlineVideo::default()];
        let cx = &mut Cx::new(Box::new(|_, _| {}));
        let (root, videos) = cx.with_vm(|vm| {
            let mut root = View::script_new(vm);
            let videos = [0, 1].map(|i| {
                let mut view = View::script_new(vm);
                let clip = WidgetRef::new_with_inner(Box::new(Video::script_new(vm)));
                view.children.push((live_id!(clip), clip));
                let video = WidgetRef::new_with_inner(Box::new(view));
                root.children.push((LiveId(i), video.clone()));
                video
            });
            (WidgetRef::new_with_inner(Box::new(root)), videos)
        });
        makepad_widgets::widget_tree::set_ui_root(cx, &root);
        for (turn, current) in [0, 1, 0].into_iter().enumerate() {
            let now = turn as f64;
            players[current].toggle_play(&msg, now);
            // Exercise both draw orders as ownership moves between panels.
            for i in 0..2 {
                owners[i].drive(cx, &videos[i], &mut players[i], &msg, now);
                let clip = videos[i].widget(cx, ids!(clip)).as_video();
                if clip.is_preparing() {
                    videos[i].handle_event(cx, &Event::VideoPlaybackPrepared(VideoPlaybackPreparedEvent {
                        video_id: LiveId(0), video_width: 480, video_height: 300, duration: 14_000,
                        is_seekable: true, video_tracks: Vec::new(), audio_tracks: Vec::new(),
                    }), &mut Scope::empty());
                    owners[i].drive(cx, &videos[i], &mut players[i], &msg, now);
                }
            }
            assert!(videos[current].widget(cx, ids!(clip)).as_video().is_playing());
            assert!(!videos[1 - current].widget(cx, ids!(clip)).as_video().is_playing(),
                "the other panel's native player must pause, not just its control label");
        }
        drop(owners);
        media_cleanup(cx, &root);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn native_video_survives_row_replacement_and_releases_before_another_clip() {
        static APPS: &[&dyn kernel::app::App] = &[&TELEGRAM];
        let session = Session::fake(APPS);
        let mut msg = model::history(session.store(), STELAXIS).iter()
            .find(|m| m.media.as_ref().is_some_and(|md| md.kind == "video")).unwrap().clone();
        msg.media.as_mut().unwrap().clip = Some("tg:inline-native".into());
        let dir = std::env::temp_dir().join(format!("superapp-inline-native-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("blobs")).unwrap();
        std::fs::write(dir.join("blobs").join(kernel::caps::file_name("tg:inline-native")),
            b"\x00\x00\x00\x18ftypisom").unwrap();
        let store = Rc::new(Store::open(Some(&dir.join("store.sqlite")), &[]).unwrap());
        let mut player = Playback::new(store.clone(), msg.id);

        let cx = &mut Cx::new(Box::new(|_, _| {}));
        let (root, video, first, second) = cx.with_vm(|vm| {
            let clip = WidgetRef::new_with_inner(Box::new(Video::script_new(vm)));
            let mut video = View::script_new(vm);
            video.children.push((live_id!(clip), clip));
            let video = WidgetRef::new_with_inner(Box::new(video));
            let mut source = View::script_new(vm);
            source.visible = false;
            source.children.push((live_id!(clip_box), video.clone()));
            let slot = |vm: &mut ScriptVm| {
                let mut slot = InlineVideoSlot::script_new(vm);
                slot.view.children.push((live_id!(playback), WidgetRef::new_with_inner(Box::new(View::script_new(vm)))));
                WidgetRef::new_with_inner(Box::new(slot))
            };
            let first = slot(vm);
            let second = slot(vm);
            let mut root = View::script_new(vm);
            root.children.push((live_id!(source), WidgetRef::new_with_inner(Box::new(source))));
            root.children.push((live_id!(first), first.clone()));
            root.children.push((live_id!(second), second.clone()));
            (WidgetRef::new_with_inner(Box::new(root)), video, first, second)
        });
        makepad_widgets::widget_tree::set_ui_root(cx, &root);
        let clip = video.widget(cx, ids!(clip)).as_video();
        let mut owner = InlineVideo::default();
        media::pause_video(cx, &video);
        assert!(clip.is_unprepared(), "hiding an idle row must not prevent its first preparation");
        player.toggle_play(&msg, 0.0);
        assert!(!owner.drive(cx, &video, &mut player, &msg, 0.0).shown);
        assert!(clip.is_preparing());
        root.handle_event(cx, &Event::VideoPlaybackPrepared(VideoPlaybackPreparedEvent {
            video_id: LiveId(0), video_width: 480, video_height: 300, duration: 14_000,
            is_seekable: true, video_tracks: Vec::new(), audio_tracks: Vec::new(),
        }), &mut Scope::empty());
        assert!(!owner.drive(cx, &video, &mut player, &msg, 1.0).shown,
            "preparation alone must not replace the poster with an empty video");
        let frame = |id| Event::VideoTextureUpdated(VideoTextureUpdatedEvent {
            video_id: LiveId(id), current_position_ms: 0, yuv: Default::default(), rgba_gl_2d: false,
        });
        let actions = cx.capture_actions(|cx| root.handle_event(cx, &frame(123), &mut Scope::empty()));
        assert!(!owner.handle_actions(cx, &video, &actions));
        assert!(!owner.drive(cx, &video, &mut player, &msg, 1.0).shown,
            "another panel's first frame must not remove this poster");
        let actions = cx.capture_actions(|cx| root.handle_event(cx, &frame(0), &mut Scope::empty()));
        assert!(owner.handle_actions(cx, &video, &actions));
        assert!(owner.drive(cx, &video, &mut player, &msg, 1.0).shown,
            "the first decoded frame must reveal the video even at timestamp zero");
        fill_slot(cx, &first, &video, true, None);
        root.handle_event(cx, &Event::VideoTextureUpdated(VideoTextureUpdatedEvent {
            video_id: LiveId(0), current_position_ms: 2500, yuv: Default::default(), rgba_gl_2d: false,
        }), &mut Scope::empty());
        fill_slot(cx, &first, &video, false, None);
        fill_slot(cx, &second, &video, true, None);
        let state = owner.drive(cx, &video, &mut player, &msg, 3.0).player.unwrap();
        assert_eq!(state, PlayerState { playing: true, position: 2.5, length: 14.0 });
        assert!(first.child(live_id!(playback)).borrow_mut::<View>().unwrap().children.is_empty());
        assert_eq!(second.widget(cx, ids!(playback.video.clip)), video.widget(cx, ids!(clip)),
            "a different row or selection twin must draw the same native player");
        assert_eq!(root.child(live_id!(source)).child(live_id!(clip_box)), video,
            "the owner must keep finding its player after a row indexes it as a child");

        player.pause(4.0);
        assert!(owner.drive(cx, &video, &mut player, &msg, 4.0).shown,
            "pausing must retain the decoded picture");
        assert!(clip.is_paused());
        let mut next = msg.clone();
        next.id += 1;
        let mut next_player = Playback::new(store, next.id);
        next_player.toggle_play(&next, 5.0);
        assert!(!owner.drive(cx, &video, &mut next_player, &next, 5.0).shown);
        assert!(clip.is_cleaning_up(), "a new message must release the old native source first");
        root.handle_event(cx, &Event::VideoPlaybackPrepared(VideoPlaybackPreparedEvent {
            video_id: LiveId(0), video_width: 480, video_height: 300, duration: 14_000,
            is_seekable: true, video_tracks: Vec::new(), audio_tracks: Vec::new(),
        }), &mut Scope::empty());
        assert!(!owner.drive(cx, &video, &mut next_player, &next, 5.0).shown,
            "late preparation of the old clip must not show it on the new message");
        root.handle_event(cx, &Event::VideoPlaybackResourcesReleased(VideoPlaybackResourcesReleasedEvent {
            video_id: LiveId(0),
        }), &mut Scope::empty());
        owner.drive(cx, &video, &mut next_player, &next, 5.0);
        assert!(clip.is_preparing());
        drop(owner);
        media_cleanup(cx, &root);
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn media_cleanup(cx: &mut Cx, root: &WidgetRef) {
        crate::shell::widgets::media::cleanup_videos(cx, None);
        let released = Event::VideoPlaybackResourcesReleased(VideoPlaybackResourcesReleasedEvent { video_id: LiveId(0) });
        root.handle_event(cx, &released, &mut Scope::empty());
        crate::shell::widgets::media::cleanup_videos(cx, Some(&released));
    }
}
