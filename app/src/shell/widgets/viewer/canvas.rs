//! A clipped image surface with one transform for drawing, gestures and links.

use std::collections::BTreeMap;
use makepad_widgets::*;
use makepad_widgets::makepad_platform::event::{TouchState, TouchUpdateEvent};
use crate::{reader::pdf::{Link, Target}, shell::hosted::PanelProps};

#[cfg(all(test, headless))]
#[path = "canvas_tests.rs"]
mod input_tests;

#[derive(Debug, Clone)]
pub(super) struct Camera {
    pub zoom: f64,
    pub offset: DVec2,
}

impl Default for Camera {
    fn default() -> Self { Self { zoom: 1.0, offset: DVec2::default() } }
}

impl Camera {
    fn size(&self, viewport: Rect, source: DVec2) -> DVec2 {
        let fit = (viewport.size.x / source.x.max(1.0)).min(viewport.size.y / source.y.max(1.0));
        source * fit.max(0.0) * self.zoom
    }

    fn rect(&self, viewport: Rect, source: DVec2) -> Rect {
        let size = self.size(viewport, source);
        Rect { pos: viewport.pos + (viewport.size - size) * 0.5 + self.offset, size }
    }

    fn clamp(&mut self, viewport: Rect, source: DVec2) {
        let over = (self.size(viewport, source) - viewport.size) * 0.5;
        self.offset.x = self.offset.x.clamp(-over.x.max(0.0), over.x.max(0.0));
        self.offset.y = self.offset.y.clamp(-over.y.max(0.0), over.y.max(0.0));
    }

    fn zoom_at(&mut self, viewport: Rect, source: DVec2, anchor: DVec2, zoom: f64) {
        if !zoom.is_finite() { return; }
        let zoom = zoom.clamp(1.0, 8.0);
        let anchor = anchor - viewport.pos - viewport.size * 0.5;
        self.offset = anchor - (anchor - self.offset) * (zoom / self.zoom);
        self.zoom = zoom;
        self.clamp(viewport, source);
    }

    fn pan(&mut self, viewport: Rect, source: DVec2, delta: DVec2) {
        self.offset += delta;
        self.clamp(viewport, source);
    }
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
struct DrawPicture {
    #[deref]
    draw_super: DrawQuad,
}

#[derive(Script, ScriptHook, Widget)]
pub struct ViewerImage {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_picture: DrawPicture,
    #[area]
    #[rust]
    area: Area,
    #[rust]
    texture: Option<Texture>,
    #[rust]
    dimensions: DVec2,
    #[rust]
    camera: Camera,
    #[rust]
    links: Vec<Link>,
    #[rust]
    pdf: bool,
    #[rust]
    active: bool,
    #[rust]
    label: Option<String>,
    #[rust]
    press: Option<Press>,
    #[rust]
    touches: BTreeMap<u64, DVec2>,
    #[rust]
    clicked: Option<Target>,
}

impl ViewerImage {
    fn viewport(&self, cx: &Cx) -> Rect { self.area.rect(cx) }

    fn owns(&self, cx: &Cx, props: &PanelProps, point: DVec2) -> bool {
        self.active && self.area.is_valid(cx) && self.area.clipped_rect(cx).contains(point)
            && props.hits.at(point).and_then(|h| h.slot) == Some(props.slot)
    }

    fn link_rect(&self, viewport: Rect, link: &Link) -> Rect {
        let image = self.camera.rect(viewport, self.dimensions);
        let [x0, y0, x1, y1] = link.rect;
        Rect { pos: image.pos + dvec2(x0 * image.size.x, y0 * image.size.y),
            size: dvec2((x1 - x0) * image.size.x, (y1 - y0) * image.size.y) }
    }

    fn link(&self, viewport: Rect, point: DVec2) -> Option<Target> {
        if !viewport.contains(point) { return None; }
        self.links.iter().rev().find(|l| self.link_rect(viewport, l).contains(point)).map(|l| l.target.clone())
    }

    fn down(&mut self, viewport: Rect, point: DVec2) {
        self.press = Some(Press { start: point, last: point, moved: false, link: self.link(viewport, point) });
    }

    fn drag(&mut self, viewport: Rect, point: DVec2) {
        if let Some(press) = &mut self.press {
            let delta = point - press.last;
            press.moved |= (point - press.start).length() >= crate::shell::touch::TOUCH_SLOP;
            press.last = point;
            if press.moved { self.camera.pan(viewport, self.dimensions, delta); }
        }
    }

    fn up(&mut self, viewport: Rect, point: DVec2) {
        if let Some(press) = self.press.take() {
            if !press.moved && (point - press.start).length() < crate::shell::touch::TOUCH_SLOP
                && press.link == self.link(viewport, point) {
                self.clicked = press.link;
            }
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
                    self.camera.zoom_at(viewport, self.dimensions, a, self.camera.zoom * db / da);
                    self.camera.pan(viewport, self.dimensions, b - a);
                }
            }
        } else if let Some(point) = self.touches.values().next().copied() {
            self.drag(viewport, point);
        }
        for touch in &event.touches {
            if touch.state == TouchState::Stop && self.touches.remove(&touch.uid).is_some() {
                if self.touches.is_empty() { self.up(viewport, touch.abs); }
                else if let Some(press) = &mut self.press {
                    press.last = *self.touches.values().next().unwrap();
                }
            }
        }
        if claimed { props.grab.claim_touch(); }
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
                    self.camera.zoom_at(viewport, self.dimensions, e.abs,
                        self.camera.zoom * 2f64.powf(-e.scroll.y * 0.01));
                } else if self.camera.zoom > 1.0 {
                    self.camera.pan(viewport, self.dimensions, -e.scroll);
                } else { return; }
                e.handled_x.set(true);
                e.handled_y.set(true);
            }
            Event::MouseDown(e) if e.button == MouseButton::PRIMARY && self.owns(cx, props, e.abs) => self.down(viewport, e.abs),
            Event::MouseMove(e) if self.press.is_some() && self.touches.is_empty() => self.drag(viewport, e.abs),
            Event::MouseUp(e) if e.button == MouseButton::PRIMARY => self.up(viewport, e.abs),
            Event::TouchUpdate(e) => {
                if e.touches.iter().any(|t| t.state == TouchState::Start && self.owns(cx, props, t.abs)) {
                    if let Some(session) = scope.data.get_mut::<kernel::session::Session>() {
                        session.nav(kernel::nav::Nav::Focus(props.slot));
                    }
                }
                self.touch(cx, props, e);
            }
            Event::WindowLostFocus(_) | Event::Background => {
                self.press = None;
                self.touches.clear();
            }
            _ => return,
        }
        self.area.redraw(cx);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let viewport = cx.peek_walk_turtle(walk);
        cx.begin_turtle(walk, Layout { clip_x: true, clip_y: true, ..Default::default() });
        self.camera.clamp(viewport, self.dimensions);
        if let Some(texture) = &self.texture {
            self.draw_picture.draw_vars.set_texture(0, texture);
            self.draw_picture.draw_abs(cx, self.camera.rect(viewport, self.dimensions));
        }
        cx.end_turtle_with_area(&mut self.area);
        if self.active {
            if let Some(props) = scope.props.get::<PanelProps>() {
                // Area clips are finalized when the enclosing pass ends.
                // During drawing, use this surface's bounds and the viewport.
                let clip = viewport.clip((DVec2::default(), cx.current_pass_size()));
                if clip.size.x > 0.0 && clip.size.y > 0.0 {
                    let cursor = if self.camera.zoom > 1.0 { MouseCursor::Move } else { MouseCursor::Default };
                    props.hits.add(if self.pdf { "pdf page" } else { "picture" }, clip, cursor, props.slot);
                    if let Some(label) = &self.label { props.hits.add(label, clip, cursor, props.slot); }
                    for link in &self.links {
                        let rect = self.link_rect(viewport, link).clip((clip.pos, clip.pos + clip.size));
                        if rect.size.x > 0.0 && rect.size.y > 0.0 {
                            let label = match &link.target { Target::Url(url) => url.clone(), Target::Page(page) => format!("go to page {}", page + 1) };
                            props.hits.add(label, rect, MouseCursor::Hand, props.slot);
                        }
                    }
                }
            }
        }
        DrawStep::done()
    }
}

impl ViewerImageRef {
    pub fn set(&self, cx: &mut Cx, texture: Option<Texture>, dimensions: DVec2, pdf: bool, links: Vec<Link>) {
        if let Some(mut image) = self.borrow_mut() {
            image.texture = texture;
            image.dimensions = dimensions;
            image.pdf = pdf;
            image.links = links;
            image.camera = Camera::default();
            image.press = None;
            image.touches.clear();
            image.clicked = None;
            image.area.redraw(cx);
        }
    }

    pub fn enable(&self, enabled: bool) {
        if let Some(mut image) = self.borrow_mut() {
            image.active = enabled;
            if !enabled { image.press = None; image.touches.clear(); image.clicked = None; }
        }
    }

    pub fn label(&self, label: String) { if let Some(mut image) = self.borrow_mut() { image.label = Some(label); } }
    pub fn clicked(&self) -> Option<Target> { self.borrow_mut().and_then(|mut image| image.clicked.take()) }
    pub fn zoom(&self) -> f64 { self.borrow().map_or(1.0, |image| image.camera.zoom) }
    pub fn zoom_to(&self, cx: &mut Cx, zoom: f64) {
        if let Some(mut image) = self.borrow_mut() {
            let viewport = image.viewport(cx);
            let dimensions = image.dimensions;
            image.camera.zoom_at(viewport, dimensions, viewport.pos + viewport.size * 0.5, zoom);
            image.area.redraw(cx);
        }
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_keeps_the_anchor_and_pan_stays_inside_the_page() {
        let viewport = Rect { pos: dvec2(10.0, 20.0), size: dvec2(600.0, 400.0) };
        let source = dvec2(1200.0, 800.0);
        let mut camera = Camera::default();
        let anchor = dvec2(210.0, 120.0);
        let before = camera.rect(viewport, source);
        camera.zoom_at(viewport, source, anchor, 3.0);
        let after = camera.rect(viewport, source);
        assert!(((anchor - before.pos) / before.size - (anchor - after.pos) / after.size).length() < 1e-10);
        camera.pan(viewport, source, dvec2(10000.0, -10000.0));
        let page = camera.rect(viewport, source);
        assert!(page.pos.x <= viewport.pos.x && page.pos.y <= viewport.pos.y);
        assert!(page.pos.x + page.size.x >= viewport.pos.x + viewport.size.x);
        assert!(page.pos.y + page.size.y >= viewport.pos.y + viewport.size.y);
        camera.zoom_at(viewport, source, anchor, 0.1);
        assert_eq!(camera.zoom, 1.0);
        assert_eq!(camera.offset, DVec2::default());
    }
}
