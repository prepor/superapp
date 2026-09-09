//! A continuous document surface; one transform drives pixels, links and input.

use std::collections::BTreeMap;
use makepad_widgets::*;
use makepad_widgets::makepad_platform::event::{TouchState, TouchUpdateEvent};
use crate::{reader::pdf::{self, Link, Target}, shell::{draw::DrawFlat, hosted::PanelProps}};
use super::{control::{Command, Fit, Status}, geometry::Camera};

#[cfg(all(test, headless))]
#[path = "canvas_tests.rs"]
mod input_tests;

const CACHE_BYTES: usize = 96 * 1024 * 1024;

struct Surface {
    texture: Option<Texture>,
    links: Vec<Link>,
    error: Option<String>,
}

#[derive(Default)]
struct Press {
    start: DVec2,
    last: DVec2,
    moved: bool,
    link: Option<Target>,
}

#[derive(Script, ScriptHook)]
#[repr(C)]
struct DrawPicture { #[deref] draw_super: DrawQuad }

#[derive(Script, ScriptHook, Widget)]
pub struct ViewerImage {
    #[uid] uid: WidgetUid,
    #[source] source: ScriptObjectRef,
    #[walk] walk: Walk,
    #[redraw] #[live] draw_picture: DrawPicture,
    #[live] draw_background: DrawFlat,
    #[live] draw_paper: DrawFlat,
    #[live] draw_scroll: DrawFlat,
    #[live] draw_text: DrawText,
    #[area] #[rust] area: Area,
    #[rust] surfaces: BTreeMap<usize, Surface>,
    #[rust] camera: Camera,
    #[rust] pdf: bool,
    #[rust] active: bool,
    #[rust] label: Option<String>,
    #[rust] press: Option<Press>,
    #[rust] bar_drag: Option<f64>,
    #[rust] touches: BTreeMap<u64, DVec2>,
    #[rust] clicked: Option<Target>,
}

impl ViewerImage {
    fn viewport(&self, cx: &Cx) -> Rect { self.area.rect(cx) }

    fn owns(&self, cx: &Cx, props: &PanelProps, point: DVec2) -> bool {
        self.active && self.area.is_valid(cx) && self.area.clipped_rect(cx).contains(point)
            && props.hits.at(point).and_then(|h| h.slot) == Some(props.slot)
    }

    fn page_rect(&self, viewport: Rect, page: usize) -> Rect {
        let rect = self.camera.rect(page);
        Rect { pos: viewport.pos + rect.pos, ..rect }
    }

    fn link_rect(&self, viewport: Rect, page: usize, link: &Link) -> Rect {
        let image = self.page_rect(viewport, page);
        let [x0, y0, x1, y1] = link.rect;
        Rect { pos: image.pos + dvec2(x0 * image.size.x, y0 * image.size.y),
            size: dvec2((x1 - x0) * image.size.x, (y1 - y0) * image.size.y) }
    }

    fn link(&self, viewport: Rect, point: DVec2) -> Option<Target> {
        if !viewport.contains(point) { return None; }
        self.surfaces.iter().find_map(|(page, surface)| surface.links.iter().rev()
            .find(|link| self.link_rect(viewport, *page, link).contains(point)).map(|link| link.target.clone()))
    }

    fn down(&mut self, viewport: Rect, point: DVec2) {
        self.press = Some(Press { start: point, last: point, moved: false, link: self.link(viewport, point) });
    }

    fn drag(&mut self, point: DVec2) {
        if let Some(press) = &mut self.press {
            let delta = point - press.last;
            press.moved |= (point - press.start).length() >= crate::shell::touch::TOUCH_SLOP;
            press.last = point;
            if press.moved { self.camera.pan(delta); }
        }
    }

    fn up(&mut self, viewport: Rect, point: DVec2) {
        if let Some(press) = self.press.take() {
            if !press.moved && (point - press.start).length() < crate::shell::touch::TOUCH_SLOP
                && press.link == self.link(viewport, point) { self.clicked = press.link; }
        }
    }

    fn touch(&mut self, cx: &Cx, props: &PanelProps, event: &TouchUpdateEvent) {
        let viewport = self.viewport(cx);
        let before = fingers(&self.touches);
        let mut claimed = !self.touches.is_empty();
        for touch in &event.touches {
            if touch.state == TouchState::Start && self.owns(cx, props, touch.abs) {
                if self.touches.is_empty() { self.down(viewport, touch.abs); }
                self.touches.insert(touch.uid, touch.abs);
            }
            if let Some(point) = self.touches.get_mut(&touch.uid) {
                claimed = true;
                *point = touch.abs;
                touch.handled.set(self.area);
            }
        }
        let after = fingers(&self.touches);
        if self.touches.len() > 1 {
            if let Some(press) = &mut self.press { press.moved = true; }
            if let (Some((a, da)), Some((b, db))) = (before, after) {
                if da > 0.0 && db > 0.0 {
                    self.camera.zoom_at(a - viewport.pos, self.camera.scale * db / da);
                    self.camera.pan(b - a);
                }
            }
        } else if let Some(point) = self.touches.values().next().copied() { self.drag(point); }
        for touch in &event.touches {
            if touch.state == TouchState::Stop && self.touches.remove(&touch.uid).is_some() {
                if self.touches.is_empty() { self.up(viewport, touch.abs); }
                else if let Some(press) = &mut self.press { press.last = *self.touches.values().next().unwrap(); }
            }
        }
        if claimed { props.grab.claim_touch(); }
    }

    fn command(&mut self, command: Command) {
        let page = self.camera.current();
        match command {
            Command::Fit => self.camera.fit_to(Fit::Page, page),
            Command::FitWidth => self.camera.fit_to(Fit::Width, page),
            Command::ZoomIn | Command::ZoomOut => self.camera.zoom_at(self.camera.viewport * 0.5,
                self.camera.scale * if command == Command::ZoomIn { 1.5 } else { 1.0 / 1.5 }),
            Command::Previous => self.go_to(page.saturating_sub(1)),
            Command::Next => self.go_to((page + 1).min(self.camera.sizes.len().saturating_sub(1))),
        }
    }

    fn go_to(&mut self, page: usize) {
        if let Some(fit) = self.camera.fit { self.camera.fit_to(fit, page); }
        else { self.camera.go_to(page); }
    }

    fn status(&self) -> Status {
        Status { ready: self.active, page: self.camera.current(),
            pages: if self.pdf { self.camera.sizes.len() } else { 0 }, scale: self.camera.scale, fit: self.camera.fit }
    }

    /// Visible pages first, then nearby pages. The working set has a strict
    /// bitmap budget even when a long document is zoomed far out.
    fn wanted(&self) -> Vec<usize> {
        if !self.pdf || self.camera.pages.is_empty() { return Vec::new(); }
        let distance = |page| {
            let r = self.camera.rect(page);
            (r.pos.y - self.camera.viewport.y).max(0.0).max((-r.pos.y - r.size.y).max(0.0))
        };
        let mut pages: Vec<usize> = (0..self.camera.pages.len())
            .filter(|&page| distance(page) < self.camera.viewport.y.max(1.0)).collect();
        let current = self.camera.current();
        pages.sort_by(|a, b| distance(*a).total_cmp(&distance(*b))
            .then_with(|| a.abs_diff(current).cmp(&b.abs_diff(current))));
        let mut bytes = 0;
        pages.retain(|&page| {
            let size = self.camera.sizes[page];
            let (_, w, h) = pdf::bitmap_size(size.x, size.y);
            let cost = w as usize * h as usize * 4;
            if bytes + cost > CACHE_BYTES { return false; }
            bytes += cost;
            true
        });
        pages
    }

    fn scroll_thumb(&self, viewport: Rect) -> Option<Rect> {
        if self.camera.total.y <= viewport.size.y { return None; }
        let height = (viewport.size.y * viewport.size.y / self.camera.total.y).max(24.0).min(viewport.size.y);
        let y = -self.camera.offset.y / (self.camera.total.y - viewport.size.y) * (viewport.size.y - height);
        Some(Rect { pos: viewport.pos + dvec2(viewport.size.x - 7.0, y), size: dvec2(5.0, height) })
    }

    fn scroll_to(&mut self, viewport: Rect, point: DVec2, grab: f64) {
        if let Some(thumb) = self.scroll_thumb(viewport) {
            let track = viewport.size.y - thumb.size.y;
            if track > 0.0 {
                let offset = -(point.y - viewport.pos.y - grab) / track
                    * (self.camera.total.y - viewport.size.y);
                self.camera.pan(dvec2(0.0, offset - self.camera.offset.y));
            }
        }
    }
}

fn fingers(points: &BTreeMap<u64, DVec2>) -> Option<(DVec2, f64)> {
    let mut points = points.values();
    let a = *points.next()?;
    let b = *points.next()?;
    Some(((a + b) * 0.5, (b - a).length()))
}

impl Widget for ViewerImage {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if !self.active { return; }
        let Some(props) = scope.props.get::<PanelProps>() else { return; };
        let viewport = self.viewport(cx);
        match event {
            Event::Scroll(e) if self.owns(cx, props, e.abs) && !e.handled_x.get() && !e.handled_y.get() => {
                if e.modifiers.logo || e.modifiers.control {
                    self.camera.zoom_at(e.abs - viewport.pos, self.camera.scale * 2f64.powf(-e.scroll.y * 0.01));
                } else if self.pdf || self.camera.total.x > viewport.size.x || self.camera.total.y > viewport.size.y {
                    self.camera.pan(-e.scroll);
                } else { return; }
                e.handled_x.set(true);
                e.handled_y.set(true);
            }
            Event::MouseDown(e) if e.button == MouseButton::PRIMARY && self.owns(cx, props, e.abs) => {
                if let Some(thumb) = self.scroll_thumb(viewport).filter(|_| e.abs.x >= viewport.pos.x + viewport.size.x - 12.0) {
                    let grab = if thumb.contains(e.abs) { e.abs.y - thumb.pos.y } else { thumb.size.y * 0.5 };
                    self.bar_drag = Some(grab);
                    self.scroll_to(viewport, e.abs, grab);
                } else { self.down(viewport, e.abs); }
            }
            Event::MouseMove(e) if self.bar_drag.is_some() => self.scroll_to(viewport, e.abs, self.bar_drag.unwrap()),
            Event::MouseMove(e) if self.press.is_some() && self.touches.is_empty() => self.drag(e.abs),
            Event::MouseUp(e) if e.button == MouseButton::PRIMARY => { self.bar_drag = None; self.up(viewport, e.abs); }
            Event::TouchUpdate(e) => {
                if e.touches.iter().any(|t| t.state == TouchState::Start && self.owns(cx, props, t.abs)) {
                    if let Some(session) = scope.data.get_mut::<kernel::session::Session>() {
                        session.nav(kernel::nav::Nav::Focus(props.slot));
                    }
                }
                self.touch(cx, props, e);
            }
            Event::WindowLostFocus(_) | Event::Background => {
                self.press = None; self.bar_drag = None; self.touches.clear();
            }
            _ => return,
        }
        self.area.redraw(cx);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let viewport = cx.peek_walk_turtle(walk);
        cx.begin_turtle(walk, Layout { clip_x: true, clip_y: true, ..Default::default() });
        self.camera.resize(viewport.size);
        if self.active {
            self.draw_background.draw_abs(cx, viewport);
            // Enclosing area clips are finalized after drawing. Use the
            // actual surface bounds for the hits registered in this pass.
            let clip = viewport.clip((DVec2::default(), cx.current_pass_size()));
            let props = scope.props.get::<PanelProps>();
            if let Some(props) = props.filter(|_| clip.size.x > 0.0 && clip.size.y > 0.0) {
                props.hits.add(if self.pdf { "pdf page" } else { "picture" }, clip, MouseCursor::Move, props.slot);
                if let Some(label) = &self.label { props.hits.add(label, clip, MouseCursor::Move, props.slot); }
            }
            for page in 0..self.camera.pages.len() {
                let rect = self.page_rect(viewport, page);
                let visible = rect.clip((clip.pos, clip.pos + clip.size));
                if visible.size.x <= 0.0 || visible.size.y <= 0.0 { continue; }
                self.draw_paper.draw_abs(cx, rect);
                let surface = self.surfaces.get(&page);
                if let Some(texture) = surface.and_then(|s| s.texture.as_ref()) {
                    self.draw_picture.draw_vars.set_texture(0, texture);
                    self.draw_picture.draw_abs(cx, rect);
                } else {
                    let text = surface.and_then(|s| s.error.as_deref()).unwrap_or("loading page…");
                    self.draw_text.draw_abs(cx, visible.pos + dvec2(12.0, 16.0), text);
                }
                if let Some(props) = props {
                    if self.pdf { props.hits.add_clipped(format!("PDF page {}", page + 1), rect, clip, MouseCursor::Move, props.slot); }
                    if let Some(surface) = surface {
                        for link in &surface.links {
                            let rect = self.link_rect(viewport, page, link);
                            let label = match &link.target { Target::Url(url) => url.clone(), Target::Page(page) => format!("go to page {}", page + 1) };
                            props.hits.add_clipped(label, rect, clip, MouseCursor::Hand, props.slot);
                        }
                    }
                }
            }
            if let Some(thumb) = self.scroll_thumb(viewport) {
                self.draw_scroll.draw_abs(cx, thumb);
                if let Some(props) = props { props.hits.add("document scrollbar", thumb, MouseCursor::Move, props.slot); }
            }
        }
        cx.end_turtle_with_area(&mut self.area);
        DrawStep::done()
    }
}

impl ViewerImageRef {
    pub fn set(&self, cx: &mut Cx, texture: Option<Texture>, dimensions: DVec2, pdf: bool, links: Vec<Link>) {
        if let Some(mut image) = self.borrow_mut() {
            image.reset();
            image.pdf = pdf;
            if dimensions.x > 0.0 && dimensions.y > 0.0 {
                image.camera.set(vec![dimensions], Fit::Page);
                image.surfaces.insert(0, Surface { texture, links, error: None });
            }
            image.area.redraw(cx);
        }
    }

    pub fn document(&self, cx: &mut Cx, sizes: Vec<(u32, u32)>) {
        if let Some(mut image) = self.borrow_mut() {
            image.reset();
            image.pdf = true;
            image.camera.set(sizes.into_iter().map(|(w, h)| dvec2(w as f64, h as f64)).collect(), Fit::Width);
            image.area.redraw(cx);
        }
    }

    pub fn page(&self, page: usize, texture: Option<Texture>, links: Vec<Link>, error: Option<String>) {
        if let Some(mut image) = self.borrow_mut() { image.surfaces.insert(page, Surface { texture, links, error }); }
    }

    pub fn request(&self) -> Option<usize> {
        let mut image = self.borrow_mut()?;
        let wanted = image.wanted();
        image.surfaces.retain(|page, _| wanted.contains(page));
        wanted.into_iter().find(|page| !image.surfaces.contains_key(page))
    }

    pub fn enable(&self, enabled: bool) {
        if let Some(mut image) = self.borrow_mut() {
            image.active = enabled;
            if !enabled { image.press = None; image.bar_drag = None; image.touches.clear(); image.clicked = None; }
        }
    }

    pub fn label(&self, label: String) { if let Some(mut image) = self.borrow_mut() { image.label = Some(label); } }
    pub fn clicked(&self) -> Option<Target> { self.borrow_mut().and_then(|mut image| image.clicked.take()) }
    pub(super) fn status(&self) -> Status { self.borrow().map_or_else(Status::default, |image| image.status()) }
    pub fn command(&self, cx: &mut Cx, command: Command) {
        if let Some(mut image) = self.borrow_mut() { image.command(command); image.area.redraw(cx); }
    }
    pub fn go_to(&self, cx: &mut Cx, page: usize) {
        if let Some(mut image) = self.borrow_mut() { image.go_to(page); image.area.redraw(cx); }
    }
}

impl ViewerImage {
    fn reset(&mut self) {
        self.camera = Camera::default(); self.surfaces.clear(); self.press = None;
        self.bar_drag = None; self.touches.clear(); self.clicked = None;
    }
}

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*
    set_type_default() do #(DrawPicture::script_shader(vm)) {
        ..mod.draw.DrawQuad
        image: texture_2d(float)
        pixel: fn() { return Pal.premul(self.image.sample_as_bgra(self.pos)) }
    }
    mod.widgets.ViewerImage = set_type_default() do #(ViewerImage::register_widget(vm)) {
        width: Fill, height: Fill
        draw_background +: { color: #f0f0f0 }
        draw_paper +: { color: #ffffff }
        draw_scroll +: { color: #a0a0a0 }
        draw_text +: { text_style: mod.widgets.SMonoStyle{}, color: #909090 }
    }
}
