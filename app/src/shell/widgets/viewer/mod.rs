//! File viewing shared by attachments, the file browser and media panels.
//! Sources supply bytes or a reader; rendering, paging and measurement live here.

use crate::shell::hosted::PanelProps;
use kernel::caps::{DiskFactory, FileKind};
use makepad_widgets::*;
use std::path::PathBuf;

mod measure;
mod worker;
pub use measure::Measure;
use worker::{Ready, Worker};

#[derive(Debug, Clone, Default)]
pub enum Preview {
    Loading,
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
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[rust]
    measure: Measure,
    #[rust]
    worker: Option<Worker>,
    #[rust]
    pending: bool,
    #[rust]
    page: usize,
    #[rust]
    pages: usize,
    #[rust]
    text: bool,
    #[rust]
    picture: bool,
}

impl FileViewer {
    fn show(&mut self, cx: &mut Cx, preview: Preview) {
        // A new source owns a new receiver: an old render can never land on it.
        self.worker = None;
        self.pending = false;
        self.page = 0;
        self.pages = 0;
        self.text = false;
        self.picture = false;
        self.measure = Measure::Empty;
        self.view
            .image(cx, ids!(image_box.image))
            .set_texture(cx, None);
        self.view
            .text_input(cx, ids!(text_box.text))
            .set_text(cx, "");
        match preview {
            Preview::Text(text) => self.take(cx, Ok(Ready::Text(text))),
            Preview::Loading => self.note(cx, "loading preview…"),
            Preview::None => self.note(cx, "no preview — open shows it"),
            Preview::Error(error) => self.note(cx, &error),
            source => {
                self.note(cx, "loading preview…");
                match Worker::start(source) {
                    Ok(worker) => {
                        self.worker = Some(worker);
                        self.pending = true;
                    }
                    Err(error) => self.note(cx, &error),
                }
            }
        }
        self.sync(cx);
        self.view.redraw(cx);
    }

    fn note(&mut self, cx: &mut Cx, text: &str) {
        self.view.label(cx, ids!(note)).set_text(cx, text);
    }

    fn take(&mut self, cx: &mut Cx, result: Result<Ready, String>) {
        self.pending = false;
        match result {
            Ok(Ready::Text(text)) => {
                self.measure = Measure::text(&text);
                self.view
                    .text_input(cx, ids!(text_box.text))
                    .set_text(cx, &text);
                self.text = true;
            }
            Ok(Ready::Picture {
                width,
                height,
                pixels,
            }) => {
                self.measure = Measure::Image(width as u32, height as u32);
                self.bitmap(cx, width, height, pixels);
            }
            Ok(Ready::Pdf(page)) => {
                self.measure = Measure::Pdf(page.size.0, page.size.1);
                self.page = page.number;
                self.pages = page.count;
                self.bitmap(cx, page.width, page.height, page.pixels);
            }
            Err(error) => {
                self.picture = false;
                self.note(cx, &error);
            }
        }
        self.sync(cx);
        self.view.redraw(cx);
    }

    fn bitmap(&mut self, cx: &mut Cx, width: usize, height: usize, pixels: Vec<u32>) {
        let texture = Texture::new_with_format(
            cx,
            TextureFormat::VecBGRAu8_32 {
                width,
                height,
                data: Some(pixels),
                updated: TextureUpdated::Full,
            },
        );
        self.view
            .image(cx, ids!(image_box.image))
            .set_texture(cx, Some(texture));
        self.picture = true;
    }

    fn sync(&mut self, cx: &mut Cx) {
        self.view
            .view(cx, ids!(text_box))
            .set_visible(cx, self.text);
        self.view
            .view(cx, ids!(image_box))
            .set_visible(cx, self.picture);
        self.view
            .label(cx, ids!(note))
            .set_visible(cx, !self.text && !self.picture);
        self.view
            .view(cx, ids!(pages))
            .set_visible(cx, self.pages > 0);
        self.view
            .button(cx, ids!(pages.previous))
            .set_visible(cx, self.page > 0 && !self.pending);
        self.view
            .button(cx, ids!(pages.next))
            .set_visible(cx, self.page + 1 < self.pages && !self.pending);
        self.view
            .label(cx, ids!(pages.count))
            .set_text(cx, &format!("page {} of {}", self.page + 1, self.pages));
    }

    fn turn(&mut self, cx: &mut Cx, next: bool) {
        if self.pending {
            return;
        }
        let page = if next {
            self.page.checked_add(1)
        } else {
            self.page.checked_sub(1)
        };
        let Some(page) = page.filter(|p| *p < self.pages) else {
            return;
        };
        let Some(worker) = self.worker.as_mut() else {
            return;
        };
        match worker.page(page) {
            Ok(()) => {
                self.pending = true;
                self.page = page;
                self.picture = false;
                self.note(cx, "loading page…");
            }
            Err(error) => self.take(cx, Err(error)),
        }
        self.sync(cx);
        self.view.redraw(cx);
    }
}

impl Widget for FileViewer {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        if matches!(event, Event::Signal) && self.pending {
            self.view.redraw(cx);
        }
        if let Event::MouseDown(e) = event {
            let Some(props) = scope.props.get::<PanelProps>() else {
                return;
            };
            if e.button != MouseButton::PRIMARY
                || self.pending
                || props.hits.at(e.abs).and_then(|hit| hit.slot) != Some(props.slot)
            {
                return;
            }
            if self.page > 0
                && self
                    .view
                    .widget(cx, ids!(pages.previous))
                    .area()
                    .rect(cx)
                    .contains(e.abs)
            {
                self.turn(cx, false);
            } else if self.page + 1 < self.pages
                && self
                    .view
                    .widget(cx, ids!(pages.next))
                    .area()
                    .rect(cx)
                    .contains(e.abs)
            {
                self.turn(cx, true);
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if self.pending {
            if let Some(result) = self.worker.as_mut().and_then(Worker::poll) {
                self.take(cx, result);
            }
        }
        let step = self.view.draw_walk(cx, scope, walk);
        if let Some(props) = scope.props.get::<PanelProps>() {
            let count = format!("page {} of {}", self.page + 1, self.pages);
            for (visible, label, path, cursor) in [
                (
                    self.text,
                    "preview",
                    ids!(text_box.text) as &[LiveId],
                    MouseCursor::Text,
                ),
                (
                    self.picture,
                    if self.pages > 0 {
                        "pdf page"
                    } else {
                        "picture"
                    },
                    ids!(image_box.image),
                    MouseCursor::Default,
                ),
                (
                    self.pages > 0,
                    count.as_str(),
                    ids!(pages.count),
                    MouseCursor::Default,
                ),
                (
                    self.page > 0 && !self.pending,
                    "previous page",
                    ids!(pages.previous),
                    MouseCursor::Hand,
                ),
                (
                    self.page + 1 < self.pages && !self.pending,
                    "next page",
                    ids!(pages.next),
                    MouseCursor::Hand,
                ),
                (
                    !self.text && !self.picture,
                    "viewer status",
                    ids!(note),
                    MouseCursor::Default,
                ),
            ] {
                if visible {
                    let rect = self.view.widget(cx, path).area().rect(cx);
                    if rect.size.x > 0.0 && rect.size.y > 0.0 {
                        props.hits.add(label, rect, cursor, props.slot);
                    }
                }
            }
        }
        step
    }
}

impl FileViewerRef {
    pub fn show(&self, cx: &mut Cx, preview: Preview) {
        if let Some(mut viewer) = self.borrow_mut() {
            viewer.show(cx, preview);
        }
    }

    pub fn measure(&self) -> Measure {
        self.borrow()
            .map(|viewer| viewer.measure.clone())
            .unwrap_or_default()
    }
}

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.FileViewer = set_type_default() do #(FileViewer::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        spacing: 8
        pages := View {
            visible: false, width: Fill, height: Fit
            flow: Right, spacing: 10, align: Align{y: 0.5}
            previous := mod.widgets.SBtn { text: "previous page" }
            count := mod.widgets.SLabel { text: "" }
            next := mod.widgets.SBtn { text: "next page" }
        }
        text_box := View {
            visible: false, width: Fill, height: Fill
            text := mod.widgets.SText { width: Fill, height: Fill, is_multiline: true }
        }
        image_box := View {
            visible: false, width: Fill, height: Fill
            align: Align{x: 0.5, y: 0.5}
            image := mod.widgets.Image { width: Fill, height: Fill, fit: ImageFit.Smallest }
        }
        note := mod.widgets.SLabel { width: Fill, text: "", draw_text +: { color: #909090 } }
    }
}
