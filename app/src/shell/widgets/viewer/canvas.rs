//! A continuous document surface; one transform drives pixels, links and input.

use std::collections::BTreeMap;
use makepad_widgets::*;
use makepad_widgets::makepad_platform::event::{TouchState, TouchUpdateEvent};
use crate::{reader::pdf::{self, Link, Target, TextPage}, shell::{draw::DrawFlat, hosted::PanelProps}};
use super::{control::{Command, Fit, Status}, geometry::Camera, selection::{Selection, Span}};

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
    selecting: bool,
}

#[derive(Script, ScriptHook)]
#[repr(C)]
struct DrawPicture { #[deref] draw_super: DrawQuad }

#[derive(Script, ScriptHook)]
#[repr(C)]
struct DrawHighlight {
    #[deref] draw_super: DrawQuad,
    #[live] a: Vec2f, #[live] b: Vec2f, #[live] c: Vec2f, #[live] d: Vec2f,
}

#[derive(Script, ScriptHook, WidgetRef, WidgetSet, WidgetRegister)]
pub struct ViewerImage {
    #[uid] uid: WidgetUid,
    #[source] source: ScriptObjectRef,
    #[walk] walk: Walk,
    #[live] draw_picture: DrawPicture,
    #[live] draw_background: DrawFlat,
    #[live] draw_paper: DrawFlat,
    #[live] draw_scroll: DrawFlat,
    #[live] draw_text: DrawText,
    #[live] draw_highlight: DrawHighlight,
    #[rust] area: Area,
    #[rust] surfaces: BTreeMap<usize, Surface>,
    #[rust] camera: Camera,
    #[rust] pdf: bool,
    #[rust] active: bool,
    #[rust] label: Option<String>,
    #[rust] press: Option<Press>,
    #[rust] bar_drag: Option<f64>,
    #[rust] touches: BTreeMap<u64, DVec2>,
    #[rust] clicked: Option<Target>,
    #[rust] selection: Selection,
    #[rust] touch_hold: Timer,
    #[rust] selection_frame: NextFrame,
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
        self.press = Some(Press { start: point, last: point, moved: false, link: self.link(viewport, point), selecting: false });
    }

    fn text_point(&self, viewport: Rect, point: DVec2, nearest: bool) -> Option<(usize, DVec2)> {
        let page = (0..self.camera.pages.len()).min_by(|a, b| {
            let distance = |i| { let r = self.page_rect(viewport, i); (point.y - point.y.clamp(r.pos.y, r.pos.y + r.size.y)).abs() };
            distance(*a).total_cmp(&distance(*b))
        })?;
        if !nearest && self.surfaces.get(&page).is_none_or(|surface| surface.texture.is_none()) { return None; }
        let rect = self.page_rect(viewport, page);
        let position = (point - rect.pos) / rect.size;
        (nearest || self.selection.hit(page, position)).then_some((page, position))
    }

    fn drag(&mut self, viewport: Rect, point: DVec2) {
        let selecting = self.press.as_ref().is_some_and(|press| press.selecting);
        if let Some(press) = &mut self.press {
            let delta = point - press.last;
            press.moved |= (point - press.start).length() >= crate::shell::touch::TOUCH_SLOP;
            press.last = point;
            if press.moved && !selecting { self.camera.pan(delta); }
        }
        if selecting {
            // Continue selecting beyond the reading area's edge. Newly visible
            // pages are requested by the same rendering path as normal scrolling.
            let y = point.y - point.y.clamp(viewport.pos.y, viewport.pos.y + viewport.size.y);
            if y != 0.0 { self.camera.pan(dvec2(0.0, -y.clamp(-40.0, 40.0))); }
            if let Some((page, point)) = self.text_point(viewport, point, true) { self.selection.extend(page, point); }
        }
    }

    fn up(&mut self, viewport: Rect, point: DVec2) {
        if let Some(press) = self.press.take() {
            if !press.moved && (point - press.start).length() < crate::shell::touch::TOUCH_SLOP
                && !self.selection.has_selection() && press.link == self.link(viewport, point) { self.clicked = press.link; }
        }
    }

    fn touch(&mut self, cx: &mut Cx, props: &PanelProps, event: &TouchUpdateEvent) {
        let viewport = self.viewport(cx);
        let before = fingers(&self.touches);
        let mut claimed = !self.touches.is_empty();
        for touch in &event.touches {
            if touch.state == TouchState::Start && self.owns(cx, props, touch.abs) {
                if self.touches.is_empty() {
                    self.down(viewport, touch.abs);
                    self.selection.clear();
                    cx.hide_clipboard_actions();
                    if self.text_point(viewport, touch.abs, false).is_some() { self.touch_hold = cx.start_timeout(0.45); }
                }
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
            if let Some(press) = &mut self.press { press.moved = true; press.selecting = false; }
            self.selection.clear();
            if let (Some((a, da)), Some((b, db))) = (before, after) {
                if da > 0.0 && db > 0.0 {
                    self.camera.zoom_at(a - viewport.pos, self.camera.scale * db / da);
                    self.camera.pan(b - a);
                }
            }
        } else if let Some(point) = self.touches.values().next().copied() { self.drag(viewport, point); }
        for touch in &event.touches {
            if touch.state == TouchState::Stop && self.touches.remove(&touch.uid).is_some() {
                if self.touches.is_empty() {
                    self.up(viewport, touch.abs);
                    if self.selection.has_selection() {
                        cx.set_key_focus(self.area);
                        cx.show_clipboard_actions(true, self.area.clipped_rect(cx), cx.keyboard_shift);
                    }
                }
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
        }
    }

    fn go_to(&mut self, page: usize) {
        if let Some(fit) = self.camera.fit { self.camera.fit_to(fit, page); }
        else { self.camera.go_to(page); }
    }

    fn status(&self) -> Status {
        Status { ready: self.active, page: self.camera.current(),
            pages: if self.pdf { self.camera.sizes.len() } else { 0 }, scale: self.camera.scale, fit: self.camera.fit,
            selected: self.has_selection(), text_pending: self.selection.copy_request().is_some(),
            text_error: self.selection.error(self.camera.current()) }
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

impl WidgetNode for ViewerImage {
    fn widget_uid(&self) -> WidgetUid { self.uid }
    fn walk(&mut self, _: &mut Cx) -> Walk { self.walk }
    fn area(&self) -> Area { self.area }
    fn redraw(&mut self, cx: &mut Cx) { self.area.redraw(cx); }
    fn selection_select_all(&mut self) { if self.pdf { self.selection.select_all(); } }
    fn selection_get_full_text(&self) -> String { self.selection.full_text(self.camera.pages.len()) }
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
                let taps = if let Hit::FingerDown(e) = event.hits(cx, self.area) { e.tap_count } else { 1 };
                if let Some(thumb) = self.scroll_thumb(viewport).filter(|_| e.abs.x >= viewport.pos.x + viewport.size.x - 12.0) {
                    let grab = if thumb.contains(e.abs) { e.abs.y - thumb.pos.y } else { thumb.size.y * 0.5 };
                    self.bar_drag = Some(grab);
                    self.scroll_to(viewport, e.abs, grab);
                } else {
                    self.down(viewport, e.abs);
                    if self.pdf { cx.set_key_focus(self.area); }
                    if let Some((page, point)) = self.text_point(viewport, e.abs, false) {
                        self.selection.begin(page, point, taps, e.modifiers.shift);
                        self.press.as_mut().unwrap().selecting = true;
                    } else { self.selection.clear(); }
                }
            }
            Event::MouseMove(e) if self.bar_drag.is_some() => self.scroll_to(viewport, e.abs, self.bar_drag.unwrap()),
            Event::MouseMove(e) if self.press.is_some() && self.touches.is_empty() => {
                event.hits(cx, self.area);
                self.drag(viewport, e.abs);
            }
            Event::MouseUp(e) if e.button == MouseButton::PRIMARY => {
                event.hits(cx, self.area);
                if self.pdf && self.press.is_some() { cx.set_key_focus(self.area); }
                self.bar_drag = None;
                self.up(viewport, e.abs);
            }
            Event::TouchUpdate(e) => {
                if e.touches.iter().any(|t| t.state == TouchState::Start && self.owns(cx, props, t.abs)) {
                    if let Some(session) = scope.data.get_mut::<kernel::session::Session>() {
                        session.nav(kernel::nav::Nav::Focus(props.slot));
                    }
                }
                self.touch(cx, props, e);
            }
            Event::Timer(_) if self.touch_hold.is_event(event).is_some() => {
                if self.touches.len() == 1 && self.press.as_ref().is_some_and(|p| !p.moved) {
                    let point = self.press.as_ref().unwrap().start;
                    if let Some((page, point)) = self.text_point(viewport, point, false) {
                        self.selection.begin(page, point, 2, false);
                        let press = self.press.as_mut().unwrap();
                        press.selecting = true;
                        press.moved = true;
                        cx.set_key_focus(self.area);
                    }
                }
            }
            Event::NextFrame(_) if self.selection_frame.is_event(event).is_some() => {
                if let Some(point) = self.press.as_ref().filter(|p| p.selecting).map(|p| p.last) { self.drag(viewport, point); }
            }
            Event::TextCopy(e) | Event::TextCut(e) if self.pdf && cx.has_key_focus(self.area) => {
                if let Some(text) = self.selection.copy(self.camera.pages.len()).filter(|s| !s.is_empty()) {
                    *e.response.borrow_mut() = Some(text);
                }
            }
            Event::KeyDown(e) if self.pdf && cx.has_key_focus(self.area) && e.modifiers.is_primary() && e.key_code == KeyCode::KeyA => {
                self.selection.select_all();
            }
            Event::KeyFocus(e) if e.prev == self.area && e.focus != self.area => {
                // A toolbar press temporarily gives focus to the shell. The
                // document keeps its selection and pending copy through fit/zoom.
                cx.hide_clipboard_actions();
            }
            Event::WindowLostFocus(_) | Event::Background => {
                self.press = None; self.bar_drag = None; self.touches.clear();
                self.selection.cancel_copy();
            }
            _ => return,
        }
        if self.press.as_ref().is_some_and(|p| p.selecting && !viewport.contains(p.last)) {
            self.selection_frame = cx.new_next_frame();
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
                    if let Some(text) = self.selection.pages.get(&page).filter(|_| surface.is_some_and(|s| s.texture.is_some())) {
                        for &[x0, y0, x1, y1] in &text.lines {
                            let bounds = Rect { pos: rect.pos + dvec2(x0, y0) * rect.size,
                                size: dvec2(x1 - x0, y1 - y0) * rect.size };
                            props.hits.add_clipped("PDF text", bounds, clip, MouseCursor::Text, props.slot);
                        }
                        let range = self.selection.range(page);
                        let glyphs = if range.is_empty() { &[][..] } else { text.glyphs.as_slice() };
                        for glyph in glyphs.iter().filter(|g| g.range.start < range.end && g.range.end > range.start) {
                            let quad = glyph.quad.map(|p| rect.pos + dvec2(p[0], p[1]) * rect.size);
                            let min = quad.iter().fold(dvec2(f64::INFINITY, f64::INFINITY), |a, b| dvec2(a.x.min(b.x), a.y.min(b.y)));
                            let max = quad.iter().fold(dvec2(f64::NEG_INFINITY, f64::NEG_INFINITY), |a, b| dvec2(a.x.max(b.x), a.y.max(b.y)));
                            let bounds = Rect { pos: min, size: max - min };
                            let points = quad.map(|p| (p - min).into_vec2());
                            self.draw_highlight.a = points[0]; self.draw_highlight.b = points[1];
                            self.draw_highlight.c = points[2]; self.draw_highlight.d = points[3];
                            self.draw_highlight.draw_abs(cx, bounds);
                        }
                    }
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

    pub fn text_page(&self, cx: &Cx, page: usize, text: TextPage) {
        if let Some(mut image) = self.borrow_mut() {
            image.selection.insert(page, text);
            // A selection dragged into a page before its text arrived catches up
            // without waiting for another pointer movement.
            if let Some(point) = image.press.as_ref().filter(|p| p.selecting).map(|p| p.last) {
                let viewport = image.viewport(cx);
                if let Some((page, point)) = image.text_point(viewport, point, true) { image.selection.extend(page, point); }
            }
        }
    }

    pub fn text_request(&self) -> Option<usize> {
        let mut image = self.borrow_mut()?;
        let wanted: Vec<_> = image.wanted().into_iter()
            .filter(|page| image.surfaces.get(page).is_some_and(|s| s.texture.is_some())).collect();
        image.selection.request(&wanted)
    }

    pub(super) fn copy_request(&self) -> Option<Span> { self.borrow()?.selection.copy_request() }

    pub(super) fn copied(&self, cx: &mut Cx, span: Span, result: Result<String, String>) {
        if let Some(mut image) = self.borrow_mut() {
            if let Some(text) = image.selection.copied(span, result).filter(|text| !text.is_empty()) {
                cx.copy_to_clipboard(text);
            }
            image.area.redraw(cx);
        }
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
            if !enabled { image.press = None; image.bar_drag = None; image.touches.clear(); image.clicked = None; image.selection.clear(); }
        }
    }

    pub fn label(&self, label: String) { if let Some(mut image) = self.borrow_mut() { image.label = Some(label); } }
    pub fn clicked(&self) -> Option<Target> { self.borrow_mut().and_then(|mut image| image.clicked.take()) }
    pub(super) fn status(&self) -> Status { self.borrow().map_or_else(Status::default, |image| image.status()) }
    pub fn command(&self, cx: &mut Cx, command: Command) {
        if let Some(mut image) = self.borrow_mut() {
            image.command(command);
            if image.has_selection() { cx.set_key_focus(image.area); }
            image.area.redraw(cx);
        }
    }
    pub fn go_to(&self, cx: &mut Cx, page: usize) {
        if let Some(mut image) = self.borrow_mut() { image.go_to(page); image.area.redraw(cx); }
    }
}

impl ViewerImage {
    pub(crate) fn has_selection(&self) -> bool { self.active && self.selection.has_selection() }
    pub(crate) fn text_selection(&self) -> Option<bool> { (self.active && self.pdf).then(|| self.has_selection()) }

    fn reset(&mut self) {
        self.camera = Camera::default(); self.surfaces.clear(); self.press = None;
        self.bar_drag = None; self.touches.clear(); self.clicked = None;
        self.selection = Selection::default();
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
    set_type_default() do #(DrawHighlight::script_shader(vm)) {
        ..mod.draw.DrawQuad
        pixel: fn() {
            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
            sdf.move_to(self.a.x, self.a.y)
            sdf.line_to(self.b.x, self.b.y)
            sdf.line_to(self.c.x, self.c.y)
            sdf.line_to(self.d.x, self.d.y)
            sdf.close_path()
            return sdf.fill(#3b82f640)
        }
    }
    mod.widgets.ViewerImage = set_type_default() do #(ViewerImage::register_widget(vm)) {
        width: Fill, height: Fill
        draw_background +: { color: #f0f0f0 }
        draw_paper +: { color: #ffffff }
        draw_scroll +: { color: #a0a0a0 }
        draw_text +: { text_style: mod.widgets.SMonoStyle{}, color: #909090 }
    }
}
