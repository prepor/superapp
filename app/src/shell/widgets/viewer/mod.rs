//! File viewing shared by attachments, the file browser and media panels.
//! Sources supply bytes or a reader and a panel-owned controller. A clip or
//! a sound is played rather than read: the platform's player, through the
//! media kit, over the file's path.

use crate::shell::hosted::PanelProps;
use crate::shell::widgets::media::{self, Clip, PlayerState, Scrub, SeekBar, Source, Transport};
use kernel::caps::{DiskFactory, FileKind};
use kernel::session::Session;
use makepad_widgets::*;
use std::path::PathBuf;

mod measure;
mod worker;
mod control;
mod geometry;
mod selection;
pub use control::{Command, Controller};
pub(crate) mod canvas;
pub use measure::Measure;
use canvas::{ViewerImageRef, ViewerImageWidgetRefExt};
use crate::reader::pdf::Target;
use worker::{Ready, Request, Worker};

#[derive(Debug, Clone, Default)]
pub enum Preview {
    Loading(String),
    Text(String),
    Image(Vec<u8>),
    Pdf(Vec<u8>),
    /// Hand cached attachment bytes across the thread boundary without copying.
    Bytes {
        bytes: std::sync::Arc<[u8]>,
        name: String,
        kind: FileKind,
        size: u64,
    },
    /// Cache files have opaque names; retain the source's name for dispatch.
    Path {
        path: PathBuf,
        name: String,
        kind: FileKind,
        size: u64,
    },
    Disk {
        factory: DiskFactory,
        path: PathBuf,
        name: String,
        kind: FileKind,
        size: u64,
    },
    Error(String),
    #[default]
    None,
}

impl From<kernel::caps::Preview> for Preview {
    fn from(preview: kernel::caps::Preview) -> Self {
        match preview {
            kernel::caps::Preview::Text(text) => Self::Text(text),
            kernel::caps::Preview::Image(bytes) => Self::Image(bytes),
            kernel::caps::Preview::Pdf(bytes) => Self::Pdf(bytes),
            kernel::caps::Preview::Error(error) => Self::Error(error),
            kernel::caps::Preview::None => Self::None,
        }
    }
}

/// A clip or a sound the viewer plays instead of reads.
struct Playing {
    source: Source,
    sound: bool,
    clip: Clip,
    /// Made on the first press, with the store's registry.
    transport: Option<Transport>,
    state: PlayerState,
    /// The clip's own proportions, once the platform has said.
    shape: Option<(u32, u32)>,
    seek: Option<SeekBar>,
    play: Option<Rect>,
    scrub: Scrub,
}

#[derive(Script, ScriptHook, Widget)]
pub struct FileViewer {
    #[source] source: ScriptObjectRef,
    #[deref] view: View,
    #[rust] control: Controller,
    #[rust] measure: Measure,
    #[rust] worker: Option<Worker>,
    #[rust] rendering: Option<usize>,
    #[rust] reading: bool,
    #[rust] copying: Option<selection::Span>,
    #[rust] text: bool,
    #[rust] picture: bool,
    #[rust] pdf: bool,
    #[rust] status: String,
    #[rust] playing: Option<Playing>,
}

impl FileViewer {
    fn image(&self, cx: &mut Cx) -> ViewerImageRef {
        self.view.widget(cx, ids!(image_box.image)).as_viewer_image()
    }

    fn show(&mut self, cx: &mut Cx, preview: Preview) {
        self.worker = None;
        self.rendering = None;
        self.reading = false;
        self.copying = None;
        self.text = false;
        self.picture = false;
        self.pdf = false;
        self.measure = Measure::Empty;
        self.control.reset();
        self.image(cx).set(cx, None, DVec2::default(), false, Vec::new());
        self.view.text_input(cx, ids!(text_box.text)).set_text(cx, "");
        if let Some(mut playing) = self.playing.take() {
            let video = self.view.widget(cx, ids!(media_box.video_source.clip_box));
            playing.clip.reset(cx);
            media::fill_clip(cx, &self.view.widget(cx, ids!(media_box.surface)), &video, false, None);
        }
        match preview {
            Preview::Text(text) => self.take(cx, Ok(Ready::Text(text))),
            Preview::Loading(note) => self.note(cx, &note),
            Preview::None => self.note(cx, "no preview — open shows it"),
            Preview::Error(error) => self.fail(cx, &error),
            // A clip or a sound: the platform's player over the file, not
            // a worker over its bytes. Until it has said what shape the
            // clip is, a card asks for a landscape box.
            Preview::Path { path, kind, .. } | Preview::Disk { path, kind, .. } if kind.plays() => {
                let sound = kind == FileKind::Audio;
                self.measure = if sound { Measure::text("sound") } else { Measure::Image(16, 9) };
                self.playing = Some(Playing {
                    source: Source::File(path),
                    sound,
                    clip: Clip::default(),
                    transport: None,
                    state: PlayerState::default(),
                    shape: None,
                    seek: None,
                    play: None,
                    scrub: Scrub::default(),
                });
                self.note(cx, "");
            }
            Preview::Bytes { kind, .. } if kind.plays() => {
                self.note(cx, "no preview — open shows it");
            }
            source => {
                self.note(cx, "loading preview…");
                match Worker::start(source) {
                    Ok(worker) => self.worker = Some(worker),
                    Err(error) => self.fail(cx, &error),
                }
            }
        }
        self.sync(cx);
        self.view.redraw(cx);
    }

    fn note(&mut self, cx: &mut Cx, text: &str) {
        self.view.label(cx, ids!(note)).set_text(cx, text);
    }

    fn fail(&mut self, cx: &mut Cx, error: &str) {
        self.measure = Measure::text(error);
        self.note(cx, error);
    }

    fn take(&mut self, cx: &mut Cx, result: Result<Ready, String>) {
        match result {
            Ok(Ready::Text(text)) => {
                self.worker = None;
                self.measure = Measure::text(&text);
                self.view.text_input(cx, ids!(text_box.text)).set_text(cx, &text);
                self.text = true;
            }
            Ok(Ready::Picture { width, height, pixels }) => {
                self.worker = None;
                self.measure = Measure::Image(width as u32, height as u32);
                let texture = texture(cx, width, height, pixels);
                self.image(cx).set(cx, Some(texture), dvec2(width as f64, height as f64), false, Vec::new());
                self.picture = true;
            }
            Ok(Ready::PdfInfo(sizes)) => {
                if let Some(&(w, h)) = sizes.first() { self.measure = Measure::Pdf(w, h); }
                self.image(cx).document(cx, sizes);
                self.picture = true;
                self.pdf = true;
            }
            Ok(Ready::Pdf(page)) => {
                self.rendering = None;
                let texture = texture(cx, page.width, page.height, page.pixels);
                self.image(cx).page(page.number, Some(texture), page.links, None);
            }
            Ok(Ready::PdfText(page, text)) => {
                self.reading = false;
                self.image(cx).text_page(cx, page, text);
            }
            Ok(Ready::Copied(span, result)) => self.image(cx).copied(cx, span, result),
            Err(error) => {
                if let Some(page) = self.rendering.take() {
                    self.image(cx).page(page, None, Vec::new(), Some(error));
                } else {
                    self.worker = None;
                    self.picture = false;
                    self.fail(cx, &error);
                }
            }
        }
        self.sync(cx);
    }

    fn sync(&mut self, cx: &mut Cx) {
        let playing = self.playing.is_some();
        self.image(cx).enable(self.picture);
        self.view.view(cx, ids!(text_box)).set_visible(cx, self.text);
        self.view.view(cx, ids!(image_box)).set_visible(cx, self.picture);
        self.view.view(cx, ids!(media_box)).set_visible(cx, playing);
        self.view.label(cx, ids!(note)).set_visible(cx, !self.text && !self.picture && !playing);
        self.view.label(cx, ids!(position)).set_visible(cx, self.picture);
    }

    /// Drives the player at the clip every draw: the wish made so, the
    /// surface and the strip filled, the platform's word taken back.
    fn draw_playing(&mut self, cx: &mut Cx2d, scope: &mut Scope) -> bool {
        let Some(playing) = self.playing.as_mut() else { return false };
        let media_box = self.view.widget(cx, ids!(media_box));
        let surface = media_box.widget(cx, ids!(surface));
        let video = media_box.widget(cx, ids!(video_source.clip_box));
        let strip = media_box.widget(cx, ids!(strip));
        let now = scope.data.get_mut::<Session>().map_or(0.0, |s| s.now());
        for command in self.control.take() {
            if command != Command::Play { continue; }
            if playing.transport.is_none() {
                playing.transport = scope.data.get_mut::<Session>().map(|s| Transport::new(s.store()));
            }
            if let Some(t) = playing.transport.as_mut() { t.toggle(now); }
        }
        let aspect = playing.shape.map(|(w, h)| w as f64 / h as f64);
        media::surface_aspect(cx, &surface, !playing.sound, aspect, None);
        let key = match &playing.source { Source::File(p) => p.to_string_lossy().into_owned(), Source::Web(u) => u.clone() };
        playing.clip.point_at(cx, &key);
        let wish = playing.transport.as_ref().is_some_and(Transport::running);
        let length = playing.state.length;
        let source = playing.source.clone();
        let drawn = playing.clip.drive(cx, &video, Some(&source), wish, length);
        if let Some(t) = playing.transport.as_mut() {
            t.set_running(drawn.playing);
            if let Some(st) = drawn.state { t.set_native(st); }
        }
        playing.state = match playing.transport.as_ref() {
            Some(t) => t.state(now, length),
            None => PlayerState { length, ..drawn.state.unwrap_or_default() },
        };
        media::fill_clip(cx, &surface, &video, drawn.shown, None);
        media::prime_video(cx, &video);
        media::fill_player(cx, &strip, Some(&playing.state));
        drawn.redraw || (wish && !drawn.shown)
    }

    fn poll(&mut self, cx: &mut Cx) -> bool {
        let mut changed = false;
        while let Some(result) = self.worker.as_mut().and_then(Worker::poll) {
            self.take(cx, result);
            changed = true;
        }
        changed
    }

    fn copy_request(&mut self, cx: &mut Cx) -> bool {
        let image = self.image(cx);
        let request = image.copy_request();
        if request == self.copying { return false; }
        self.copying = request;
        if let Some(worker) = &mut self.worker {
            if let Err(error) = worker.request(Request::Copy(request)) {
                if let Some(span) = request { image.copied(cx, span, Err(error)); }
            }
        }
        true
    }

    fn publish(&mut self, cx: &mut Cx, scope: &mut Scope) -> bool {
        let resized = self.control.measured(self.measure.clone());
        let status = self.image(cx).status();
        let mode = match status.fit {
            Some(control::Fit::Page) => if self.pdf { "fit page".into() } else { "fit image".into() },
            Some(control::Fit::Width) => "fit width".into(),
            None => format!("{:.0}%", status.scale * 100.0),
        };
        let mut line = if self.pdf { format!("page {} of {} · {mode}", status.page + 1, status.pages) }
            else if let Measure::Image(w, h) = self.measure { format!("{w} × {h} · {mode}") }
            else { mode };
        if status.text_pending { line.push_str(" · loading selected text…"); }
        if let Some(error) = &status.text_error { line.push_str(" · "); line.push_str(error); }
        let changed = self.status != line;
        if changed {
            self.status = line;
            self.view.label(cx, ids!(position)).set_text(cx, &self.status);
        }
        let mut status = status;
        if let Some(playing) = &self.playing {
            status.clip = true;
            status.playing = playing.state.playing;
        }
        let controls = self.control.status(status);
        let slot = scope.props.get::<PanelProps>().map(|props| props.slot);
        if let Some(session) = scope.data.get_mut::<kernel::session::Session>() {
            if resized {
                session.relayout();
                if let Some(slot) = slot.filter(|slot| session.focus().and_then(|focus| session.joined_child(focus)) == Some(*slot)) {
                    session.reveal(slot);
                }
            }
            else if controls { session.redraw(); }
        }
        changed || controls || resized
    }
}

fn texture(cx: &mut Cx, width: usize, height: usize, pixels: Vec<u32>) -> Texture {
    Texture::new_with_format(cx, TextureFormat::VecBGRAu8_32 {
        width, height, data: Some(pixels), updated: TextureUpdated::Full,
    })
}

impl Widget for FileViewer {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if matches!(event, Event::Signal) && self.poll(cx) { self.view.redraw(cx); }
        let video = self.view.widget(cx, ids!(media_box.video_source.clip_box));
        let before = media::video_word(cx, &video);
        let actions = cx.capture_actions(|cx| self.view.handle_event(cx, event, scope));
        if let Some(playing) = self.playing.as_mut() {
            if playing.clip.handle_actions(cx, &video, &actions) { self.view.redraw(cx); }
            let prepared = actions
                .filter_widget_actions(video.widget(cx, ids!(clip)).widget_uid())
                .any(|a| matches!(a.cast(), VideoAction::PlaybackPrepared));
            let after = media::video_word(cx, &video);
            if before != after { self.view.redraw(cx); }
            let now = scope.data.get_mut::<Session>().map_or(0.0, |s| s.now());
            // The strip's button, or the panel's own verb: the same toggle.
            if self.view.button(cx, ids!(media_box.strip.play_btn)).clicked(&actions) {
                self.control.run("viewer.play");
                self.view.redraw(cx);
            }
            match event {
                // The platform said what shape the clip is: the card asks
                // for that box from now on.
                Event::VideoPlaybackPrepared(e) if prepared && e.video_width > 0 && e.video_height > 0 => {
                    playing.shape = Some((e.video_width, e.video_height));
                    self.measure = Measure::Image(e.video_width, e.video_height);
                    self.view.redraw(cx);
                }
                Event::VideoDecodingError(e) if before != "unprepared" && after == "unprepared" => {
                    if let Some(t) = playing.transport.as_mut() { t.set_running(false); }
                    self.playing = None;
                    let error = format!("cannot play this here: {}", e.error);
                    self.fail(cx, &error);
                    self.sync(cx);
                    self.view.redraw(cx);
                }
                Event::WindowLostFocus(_) | Event::Background => {
                    if let Some(t) = playing.transport.as_mut() { t.pause(now); }
                    media::pause_video(cx, &video);
                    playing.scrub.cancel();
                }
                _ => {}
            }
            // Its panel scrolled or switched away: nothing plays unseen.
            if let (Some(playing), Some(props), Some(s)) = (
                self.playing.as_mut(),
                scope.props.get::<PanelProps>().cloned(),
                scope.data.get_mut::<Session>(),
            ) {
                if playing.transport.as_ref().is_some_and(Transport::running)
                    && !crate::shell::hosted::on_screen(s, props.slot)
                {
                    playing.transport.as_mut().unwrap().pause(s.now());
                    media::pause_video(cx, &video);
                }
                let seek_bar = playing.seek;
                let scrubbed = playing.scrub.handle(event, |at| {
                    let mine = props.hits.at(at).map(|h| h.slot) == Some(Some(props.slot));
                    seek_bar.filter(|_| mine).map(|bar| ((), bar))
                });
                if let Some((_, position)) = scrubbed {
                    playing.clip.seek(position);
                    self.view.redraw(cx);
                }
            }
        }
        cx.extend_actions(actions);
        let image = self.image(cx);
        if let Some(target) = image.clicked() {
            match target {
                Target::Url(url) => crate::platform::browser::open_or_notify(cx, &url, scope),
                Target::Page(page) => image.go_to(cx, page),
            }
        }
        self.copy_request(cx);
        if self.publish(cx, scope) { self.view.redraw(cx); }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let mut changed = self.poll(cx);
        let image = self.image(cx);
        if self.playing.is_some() {
            changed |= self.draw_playing(cx, scope);
        } else {
            for command in self.control.take() { image.command(cx, command); changed = true; }
        }
        let step = self.view.draw_walk(cx, scope, walk);
        if self.pdf {
            let page = image.request();
            let text = image.text_request();
            if self.rendering.is_none() && !self.reading {
                if let Some(worker) = &mut self.worker {
                    // Fill visible page placeholders before preparing selection
                    // geometry for pages that already have a bitmap.
                    if let Some(page) = page {
                        self.rendering = Some(page);
                        if let Err(error) = worker.request(Request::Page(page)) { self.take(cx, Err(error)); }
                        changed = true;
                    } else if let Some(page) = text {
                        self.reading = true;
                        if let Err(error) = worker.request(Request::Text(page)) {
                            self.take(cx, Ok(Ready::PdfText(page, crate::reader::pdf::TextPage::failed(error))));
                        }
                        changed = true;
                    }
                }
            }
        }
        changed |= self.copy_request(cx);
        changed |= self.publish(cx, scope);
        if let Some(props) = scope.props.get::<PanelProps>() {
            let playing = self.playing.is_some();
            for (visible, label, path, cursor) in [
                (self.text, "preview", ids!(text_box.text) as &[LiveId], MouseCursor::Text),
                (self.picture, self.status.as_str(), ids!(position), MouseCursor::Default),
                (!self.text && !self.picture && !playing, "viewer status", ids!(note), MouseCursor::Default),
            ] {
                if visible {
                    let rect = self.view.widget(cx, path).area().rect(cx);
                    let clip = self.view.area().rect(cx).clip((DVec2::default(), cx.current_pass_size()));
                    props.hits.add_clipped(label, rect, clip, cursor, props.slot);
                }
            }
            // The strip's controls, by their words, and the box by its.
            if let Some(playing) = self.playing.as_mut() {
                let clip = self.view.area().rect(cx).clip((DVec2::default(), cx.current_pass_size()));
                let strip = self.view.widget(cx, ids!(media_box.strip));
                playing.play = media::play_rect(cx, &strip);

                if let Some(r) = playing.play {
                    props.hits.add_clipped(
                        if playing.state.playing { "pause" } else { "play" }, r, clip, MouseCursor::Hand, props.slot,
                    );
                }
                playing.seek = SeekBar::from_player(cx, &strip, playing.state);
                match playing.seek {
                    Some(bar) => {
                        props.hits.add_clipped("seek", bar.rect, clip, MouseCursor::Hand, props.slot);
                    }
                    None => playing.scrub.cancel(),
                }
                if !playing.sound {
                    let r = self.view.widget(cx, ids!(media_box.surface)).area().rect(cx);
                    if r.size.x > 0.0 && r.size.y > 0.0 {
                        props.hits.add_clipped("clip", r, clip, MouseCursor::Default, props.slot);
                    }
                }
            }
        }
        // Loading and commands can complete during a draw. They must schedule
        // another pass even when no input or replay timer wakes the app.
        if changed { cx.redraw_area_in_draw(self.view.area()); }
        step
    }
}

impl FileViewerRef {
    pub fn bind(&self, control: Controller) { if let Some(mut viewer) = self.borrow_mut() { viewer.control = control; } }
    pub fn image_label(&self, cx: &mut Cx, label: String) {
        if let Some(viewer) = self.borrow() { viewer.image(cx).label(label); }
    }
    pub fn show(&self, cx: &mut Cx, preview: Preview) {
        if let Some(mut viewer) = self.borrow_mut() { viewer.show(cx, preview); }
    }
}

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*
    mod.widgets.FileViewer = set_type_default() do #(FileViewer::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down, spacing: 6
        text_box := View {
            visible: false, width: Fill, height: Fill
            text := mod.widgets.SText { width: Fill, height: Fill, is_multiline: true }
        }
        image_box := View {
            visible: false, width: Fill, height: Fill
            image := mod.widgets.ViewerImage {}
        }
        /* A clip or a sound: the kit's surface at the card's width, the
           strip beneath, and the player in a hidden holder of the viewer's
           own. */
        media_box := View {
            visible: false, width: Fill, height: Fit
            flow: Down, spacing: 6
            surface := mod.widgets.MediaClip { max_width: 4096, max_height: 4096 }
            video_source := View {
                visible: false
                clip_box := mod.widgets.MediaVideo {}
            }
            strip := mod.widgets.MediaPlayer {}
        }
        position := mod.widgets.SLabel { visible: false, width: Fill, text: "", draw_text +: { color: #909090 } }
        note := mod.widgets.SLabel { width: Fill, text: "", draw_text +: { color: #909090 } }
    }
}
