//! A panel owns one native video. Transcript rows only borrow its picture,
//! so replacing a virtual row or its selection twin never replaces the player.

use makepad_widgets::*;

use crate::shell::widgets::media::{self, PlayerState, VideoPlayback};
use super::super::model::{Msg, MsgId};
use super::super::panels::playback::Playback;

/// Draw a shared video without forwarding events a second time. The hidden
/// source view in the chat owns event delivery, even while a row is recycled.
#[derive(Script, ScriptHook, Widget)]
pub struct InlineVideoSlot {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
}

impl Widget for InlineVideoSlot {
    fn handle_event(&mut self, _cx: &mut Cx, _event: &Event, _scope: &mut Scope) {}

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

pub fn fill_slot(_cx: &mut Cx, slot: &WidgetRef, video: &WidgetRef, m: &Msg, shown: bool) {
    if let Some(mut slot) = slot.borrow_mut::<InlineVideoSlot>() {
        slot.view.visible = shown;
        slot.view.children.clear();
        if shown {
            slot.view.children.push((live_id!(video), video.clone()));
            slot.view.walk.height = Size::Fixed(height(m));
        }
    }
}

pub fn height(m: &Msg) -> f64 {
    m.media.as_ref().and_then(|md| Some((md.w?, md.h?)))
        .filter(|(w, h)| *w > 0 && *h > 0)
        .map_or(180.0, |(w, h)| (320.0 * h as f64 / w as f64).clamp(80.0, 480.0))
}

#[derive(Default)]
pub struct InlineVideo {
    playback: VideoPlayback,
    source: Option<(MsgId, Option<String>)>,
    last_word: String,
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
    }

    pub fn drive(
        &mut self, cx: &mut Cx, video: &WidgetRef, player: &mut Playback, m: &Msg, now: f64,
    ) -> InlineDrawn {
        let source = (m.id, m.media.as_ref().and_then(|md| md.clip.clone()));
        if self.source.as_ref() != Some(&source) {
            self.playback.reset(cx);
            self.source = Some(source);
        }
        let native = player.plays_clip(m);
        let file = native.then(|| player.clip_file(m)).flatten();
        let drawn = self.playback.drive(cx, video, file.as_deref(), player.running());
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
            shown: drawn.shown,
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
            let first = WidgetRef::new_with_inner(Box::new(InlineVideoSlot::script_new(vm)));
            let second = WidgetRef::new_with_inner(Box::new(InlineVideoSlot::script_new(vm)));
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
        assert!(owner.drive(cx, &video, &mut player, &msg, 1.0).shown,
            "preparation must reach the hidden source before a row can show its video");
        fill_slot(cx, &first, &video, &msg, true);
        root.handle_event(cx, &Event::VideoTextureUpdated(VideoTextureUpdatedEvent {
            video_id: LiveId(0), current_position_ms: 2500, yuv: Default::default(), rgba_gl_2d: false,
        }), &mut Scope::empty());
        fill_slot(cx, &first, &video, &msg, false);
        fill_slot(cx, &second, &video, &msg, true);
        let state = owner.drive(cx, &video, &mut player, &msg, 3.0).player.unwrap();
        assert_eq!(state, PlayerState { playing: true, position: 2.5, length: 14.0 });
        assert!(first.borrow_mut::<InlineVideoSlot>().unwrap().view.children.is_empty());
        assert_eq!(second.widget(cx, ids!(video.clip)), video.widget(cx, ids!(clip)),
            "a different row or selection twin must draw the same native player");
        assert_eq!(root.child(live_id!(source)).child(live_id!(clip_box)), video,
            "the owner must keep finding its player after a row indexes it as a child");

        player.pause(4.0);
        owner.drive(cx, &video, &mut player, &msg, 4.0);
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
