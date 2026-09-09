//! File viewing shared by attachments, the file browser and media panels.
//! Sources supply bytes or a reader and a panel-owned controller.

use crate::shell::hosted::PanelProps;
use kernel::caps::{DiskFactory, FileKind};
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
        match preview {
            Preview::Text(text) => self.take(cx, Ok(Ready::Text(text))),
            Preview::Loading(note) => self.note(cx, &note),
            Preview::None => self.note(cx, "no preview — open shows it"),
            Preview::Error(error) => self.fail(cx, &error),
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
        self.image(cx).enable(self.picture);
        self.view.view(cx, ids!(text_box)).set_visible(cx, self.text);
        self.view.view(cx, ids!(image_box)).set_visible(cx, self.picture);
        self.view.label(cx, ids!(note)).set_visible(cx, !self.text && !self.picture);
        self.view.label(cx, ids!(position)).set_visible(cx, self.picture);
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
        self.view.handle_event(cx, event, scope);
        let image = self.image(cx);
        if let Some(target) = image.clicked() {
            match target {
                Target::Url(url) => cx.open_url(&url, OpenUrlInPlace::No),
                Target::Page(page) => image.go_to(cx, page),
            }
        }
        self.copy_request(cx);
        if self.publish(cx, scope) { self.view.redraw(cx); }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let mut changed = self.poll(cx);
        let image = self.image(cx);
        for command in self.control.take() { image.command(cx, command); changed = true; }
        let step = self.view.draw_walk(cx, scope, walk);
        if self.pdf {
            let page = image.request();
            let text = image.text_request();
            if self.rendering.is_none() && !self.reading {
                if let Some(worker) = &mut self.worker {
                    if let Some(page) = text {
                        self.reading = true;
                        if let Err(error) = worker.request(Request::Text(page)) {
                            self.take(cx, Ok(Ready::PdfText(page, crate::reader::pdf::TextPage::failed(error))));
                        }
                        changed = true;
                    } else if let Some(page) = page {
                        self.rendering = Some(page);
                        if let Err(error) = worker.request(Request::Page(page)) { self.take(cx, Err(error)); }
                        changed = true;
                    }
                }
            }
        }
        changed |= self.copy_request(cx);
        changed |= self.publish(cx, scope);
        if let Some(props) = scope.props.get::<PanelProps>() {
            for (visible, label, path, cursor) in [
                (self.text, "preview", ids!(text_box.text) as &[LiveId], MouseCursor::Text),
                (self.picture, self.status.as_str(), ids!(position), MouseCursor::Default),
                (!self.text && !self.picture, "viewer status", ids!(note), MouseCursor::Default),
            ] {
                if visible {
                    let rect = self.view.widget(cx, path).area().rect(cx);
                    let clip = self.view.area().rect(cx).clip((DVec2::default(), cx.current_pass_size()));
                    props.hits.add_clipped(label, rect, clip, cursor, props.slot);
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
        position := mod.widgets.SLabel { visible: false, width: Fill, text: "", draw_text +: { color: #909090 } }
        note := mod.widgets.SLabel { width: Fill, text: "", draw_text +: { color: #909090 } }
    }
}
