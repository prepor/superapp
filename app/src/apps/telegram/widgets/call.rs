//! A call, drawn: the name, where it stands, the four emoji, the other
//! side's picture with mine under it — and, out of sight, whichever of the
//! four sounds the state asks for.
//!
//! The pictures come off the engine's shared slot as frames, already BGRA
//! ([`calls::frames`]); this only uploads the newest into a texture when its
//! stamp has moved. Mine is the exception where the camera is the app's to
//! hold ([`calls::camera_is_ours`]): there is nothing coming back to draw,
//! so the preview is the open camera itself, the same session every frame
//! is being sent from.
//!
//! The sounds are two hidden players, one that loops and one that does not,
//! because makepad is told whether a player loops when it is made and not
//! after.

use makepad_widgets::makepad_platform::{Texture, TextureFormat, TextureUpdated};
use makepad_widgets::*;

use crate::shell::hosted::PanelProps;
use crate::shell::widgets::media::{self, Source, VideoPlayback};

use super::super::calls::{self, sounds};
use super::super::panels::Call;

/// The children the panel expects in its template.
const NAME: &[LiveId] = ids!(name_lbl);
const STATE: &[LiveId] = ids!(state_lbl);
const EMOJI: &[LiveId] = ids!(emoji_lbl);
const REMOTE: &[LiveId] = ids!(remote);
const LOCAL: &[LiveId] = ids!(local);
const MINE: &[LiveId] = ids!(mine);
const RING: &[LiveId] = ids!(ring_source.clip_box);
const NOTE: &[LiveId] = ids!(note_source.clip_box);

/// How often a connected call redraws: once a second for the timer, and
/// fifteen times for a picture that is moving.
const TICK: f64 = 1.0;
const FRAME_TICK: f64 = 1.0 / 15.0;

/// The widget.
#[derive(Script, ScriptHook, Widget)]
pub struct CallPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    /// The two players: one made looping, one not.
    #[rust]
    ring: VideoPlayback,
    #[rust]
    note: VideoPlayback,
    /// Which sound is wanted, and for which call.
    #[rust]
    sounding: Option<(sounds::Ring, i32)>,
    /// Whether that sound is still to play. A loop stays true; a short note
    /// goes false when the platform says it has run out.
    #[rust]
    playing: bool,
    /// The two pictures, and which frame each was filled from.
    #[rust]
    remote: Option<(Texture, u64)>,
    #[rust]
    local: Option<(Texture, u64)>,
    /// One pending refresh; a visible call renews it.
    #[rust]
    tick: Timer,
}

impl Widget for CallPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if self.tick.is_event(event).is_some() {
            self.tick = Timer::default();
            self.view.redraw(cx);
        }
        self.view.handle_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let now = super::now(scope);
        let Some((name, line, emoji, ring, dir, camera)) = with_call(&props, |c| {
            (
                c.name(),
                c.line(now),
                c.emoji(),
                c.ring(),
                c.sounds_dir().map(std::path::Path::to_path_buf),
                c.camera(),
            )
        }) else {
            return self.view.draw_walk(cx, scope, walk);
        };

        let v = &self.view;
        v.label(cx, NAME).set_text(cx, &name);
        v.label(cx, STATE).set_text(cx, &line);
        v.label(cx, EMOJI).set_visible(cx, !emoji.is_empty());
        v.label(cx, EMOJI).set_text(cx, &emoji);

        // My own picture, where nothing is coming back to draw it with. The
        // box hides itself until the platform really has one, and the primed
        // quad is what gets a player its texture on android.
        let mine = v.widget(cx, MINE);
        media::show_camera(cx, &mine, camera);
        media::prime_camera(cx, &mine);

        let moving = self.pictures(cx);
        self.sound(cx, ring, dir.as_deref());

        let step = self.view.draw_walk(cx, scope, walk);
        for (text, path) in [(name, NAME), (line, STATE), (emoji, EMOJI)] {
            if text.is_empty() {
                continue;
            }
            let r = self.view.widget(cx, path).area().rect(cx);
            if r.size.x > 0.0 && r.size.y > 0.0 {
                props.hits.add(text, r, MouseCursor::Default, props.slot);
            }
        }
        // A native window sleeps between changes, and a call is a thing that
        // changes on its own: the timer is what makes the seconds count and
        // the picture move.
        if self.tick.0 == 0 {
            self.tick = cx.start_timeout(if moving { FRAME_TICK } else { TICK });
        }
        step
    }
}

impl CallPanel {
    /// Uploads whichever of the two pictures has a frame the last draw did
    /// not; answers whether either is moving, which is what asks for the
    /// faster clock.
    fn pictures(&mut self, cx: &mut Cx2d) -> bool {
        // A scripted run has no engine and therefore no frames, so this is
        // an empty slot and both boxes stay hidden.
        let (remote, local) = {
            let frames = calls::frames().lock().expect("call frames");
            (frames.remote.clone(), frames.local.clone())
        };
        let mut moving = false;
        for (picture, slot, path) in [
            (remote, &mut self.remote, REMOTE),
            (local, &mut self.local, LOCAL),
        ] {
            let box_ = self.view.widget(cx, path);
            let Some(picture) = picture else {
                *slot = None;
                box_.set_visible(cx, false);
                continue;
            };
            moving = true;
            match slot {
                Some((texture, stamp)) if *stamp == picture.stamp => {}
                Some((texture, stamp)) => {
                    texture.set_data_u32(cx, picture.width, picture.height, picture.pixels);
                    *stamp = picture.stamp;
                }
                None => {
                    let texture = Texture::new_with_format(
                        cx,
                        TextureFormat::VecBGRAu8_32 {
                            width: picture.width,
                            height: picture.height,
                            data: Some(picture.pixels),
                            updated: TextureUpdated::Full,
                        },
                    );
                    box_.widget(cx, ids!(img)).as_image().set_texture(cx, Some(texture.clone()));
                    *slot = Some((texture, picture.stamp));
                }
            }
            box_.set_visible(cx, true);
        }
        moving
    }

    /// Points the right player at the right file and lets it run. The loop
    /// and the note are two players because that is decided when a player is
    /// made; only one of them is ever pointed at anything.
    fn sound(&mut self, cx: &mut Cx2d, want: Option<(sounds::Ring, i32)>, dir: Option<&std::path::Path>) {
        if self.sounding != want {
            self.sounding = want;
            self.ring.reset(cx);
            self.note.reset(cx);
            self.playing = want.is_some();
        }
        let source = want
            .filter(|_| self.playing)
            .and_then(|(ring, _)| sounds::file(dir, ring))
            .map(Source::File);
        let repeats = want.is_some_and(|(ring, _)| ring.repeats());
        let (ring_src, note_src) = if repeats { (source, None) } else { (None, source) };
        let ring_box = self.view.widget(cx, RING);
        let note_box = self.view.widget(cx, NOTE);
        let rang = self.ring.drive(cx, &ring_box, ring_src.as_ref(), ring_src.is_some());
        let noted = self.note.drive(cx, &note_box, note_src.as_ref(), note_src.is_some());
        // A short note runs out on its own; once it has, it is not started
        // again until the state changes.
        if self.playing && !(rang.playing || noted.playing) && !repeats {
            self.playing = false;
        }
    }
}

/// Runs `f` on the instance. The borrow lasts exactly as long as the call.
fn with_call<R>(props: &PanelProps, f: impl FnOnce(&mut Call) -> R) -> Option<R> {
    let mut borrow = props.panel.borrow_mut();
    let c = borrow.as_any().downcast_mut::<Call>()?;
    Some(f(c))
}
