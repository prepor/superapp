//! Continuous page layout in logical points, independent of textures or input.

use makepad_widgets::{dvec2, DVec2, Rect};
use super::control::Fit;

const INSET: f64 = 12.0;
const GAP: f64 = 16.0;

#[derive(Default)]
pub(super) struct Camera {
    pub sizes: Vec<DVec2>,
    pub pages: Vec<Rect>,
    pub total: DVec2,
    pub viewport: DVec2,
    pub scale: f64,
    pub offset: DVec2,
    pub fit: Option<Fit>,
    panned: bool,
}

impl Camera {
    pub fn set(&mut self, sizes: Vec<DVec2>, fit: Fit) {
        self.sizes = sizes;
        self.pages.clear();
        self.offset = DVec2::default();
        self.panned = false;
        self.scale = 0.0;
        self.fit = Some(fit);
        self.resize(self.viewport);
    }

    fn layout(&mut self) {
        let width = self.sizes.iter().map(|s| s.x).fold(0.0, f64::max) * self.scale;
        let edge = |size: Option<&DVec2>| {
            if self.fit == Some(Fit::Page) {
                size.map_or(INSET, |s| ((self.viewport.y - s.y * self.scale) * 0.5).max(INSET))
            } else { INSET }
        };
        let mut y = edge(self.sizes.first());
        let bottom = edge(self.sizes.last());
        self.pages = self.sizes.iter().map(|size| {
            let size = *size * self.scale;
            let rect = Rect { pos: dvec2(INSET + (width - size.x) * 0.5, y), size };
            y += size.y + GAP;
            rect
        }).collect();
        self.total = dvec2(width + INSET * 2.0, (y - GAP).max(INSET) + bottom);
    }

    pub fn resize(&mut self, size: DVec2) {
        if self.sizes.is_empty() || size.x <= INSET * 2.0 || size.y <= INSET * 2.0 { self.viewport = size; return; }
        if self.viewport == size && self.scale > 0.0 { return; }
        let page = self.current();
        let position = self.panned.then(|| {
            let rect = self.rect(page);
            (self.viewport * 0.5 - rect.pos) / rect.size
        });
        let delta = size - self.viewport;
        self.viewport = size;
        if let Some(fit) = self.fit {
            self.fit_to(fit, page);
            if let Some(position) = position {
                // A wrapped verb bar or resized window must keep the place
                // being read, rather than jumping back to the page's start.
                let rect = self.pages[page];
                self.offset = size * 0.5 - rect.pos - position * rect.size;
                self.clamp();
                self.panned = true;
            }
        }
        else { self.offset += delta * 0.5; self.clamp(); }
    }

    fn page_scale(&self, page: usize) -> f64 {
        let size = self.sizes[page];
        ((self.viewport.x - INSET * 2.0) / size.x)
            .min((self.viewport.y - INSET * 2.0) / size.y).max(0.000001)
    }

    pub fn fit_to(&mut self, fit: Fit, page: usize) {
        if page >= self.sizes.len() { return; }
        if self.viewport.x <= INSET * 2.0 || self.viewport.y <= INSET * 2.0 { return; }
        self.scale = match fit {
            Fit::Page => self.page_scale(page),
            Fit::Width => (self.viewport.x - INSET * 2.0) / self.sizes.iter().map(|s| s.x).fold(1.0, f64::max),
        };
        self.fit = Some(fit);
        self.layout();
        self.go_to(page);
    }

    pub fn go_to(&mut self, page: usize) {
        let Some(rect) = self.pages.get(page) else { return; };
        self.panned = false;
        self.offset.x = (self.viewport.x - rect.size.x) * 0.5 - rect.pos.x;
        self.offset.y = if self.fit == Some(Fit::Page) {
            (self.viewport.y - rect.size.y).max(INSET * 2.0) * 0.5 - rect.pos.y
        } else { INSET - rect.pos.y };
        self.clamp();
    }

    pub fn rect(&self, page: usize) -> Rect {
        let rect = self.pages[page];
        Rect { pos: rect.pos + self.offset, ..rect }
    }

    pub fn current(&self) -> usize {
        self.pages.iter().enumerate().max_by(|(a, _), (b, _)| {
            let area = |i| { let r = self.rect(i).clip((DVec2::default(), self.viewport)); r.size.x.max(0.0) * r.size.y.max(0.0) };
            let (a_area, b_area) = (area(*a), area(*b));
            if (a_area - b_area).abs() < 1.0 { b.cmp(a) } else { a_area.total_cmp(&b_area) }
        }).map_or(0, |(i, _)| i)
    }

    pub fn zoom_at(&mut self, anchor: DVec2, scale: f64) {
        if self.pages.is_empty() || !scale.is_finite() { return; }
        let page = self.pages.iter().enumerate().min_by(|(a, _), (b, _)| {
            let distance = |i| { let r = self.rect(i); (anchor.y - anchor.y.clamp(r.pos.y, r.pos.y + r.size.y)).abs() };
            distance(*a).total_cmp(&distance(*b))
        }).map_or(0, |(i, _)| i);
        let before = self.rect(page);
        let position = (anchor - before.pos) / before.size;
        let fit = self.page_scale(page);
        self.scale = scale.clamp(fit * 0.25, fit * 8.0);
        self.fit = None;
        self.layout();
        let after = self.pages[page];
        self.offset = anchor - after.pos - position * after.size;
        self.clamp();
    }

    pub fn pan(&mut self, delta: DVec2) {
        let before = self.offset;
        self.offset += delta;
        self.clamp();
        self.panned |= self.offset != before;
    }

    pub fn clamp(&mut self) {
        let axis = |offset: f64, content: f64, view: f64| {
            if content <= view { (view - content) * 0.5 } else { offset.clamp(view - content, 0.0) }
        };
        self.offset.x = axis(self.offset.x, self.total.x, self.viewport.x);
        self.offset.y = axis(self.offset.y, self.total.y, self.viewport.y);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continuous_pages_scroll_and_fit_resets_scale_and_position() {
        let mut camera = Camera::default();
        camera.resize(dvec2(600.0, 700.0));
        camera.set(vec![dvec2(420.0, 595.0), dvec2(595.0, 420.0)], Fit::Width);
        let first = camera.rect(0);
        assert_eq!(first.pos.y, INSET);
        assert!((first.size.x - 576.0 * 420.0 / 595.0).abs() < 1e-6);
        assert_eq!(camera.rect(1).size.x, 576.0);
        assert!(camera.rect(1).pos.y > first.pos.y + first.size.y);
        camera.pan(dvec2(0.0, -850.0));
        assert_eq!(camera.current(), 1);
        camera.zoom_at(dvec2(300.0, 350.0), camera.scale * 2.0);
        camera.pan(dvec2(-150.0, 70.0));
        camera.fit_to(Fit::Page, 1);
        let page = camera.rect(1);
        assert!(page.pos.x >= INSET - 1e-6 && page.pos.y >= INSET - 1e-6);
        assert!(page.pos.x + page.size.x <= 600.0 - INSET + 1e-6);
        assert!(page.pos.y + page.size.y <= 700.0 - INSET + 1e-6);
        assert!((page.pos.y + page.size.y * 0.5 - 350.0).abs() < 1e-6,
            "fitting the last page centers it instead of pinning it to the document's bottom");
        let fitted = page;
        camera.pan(dvec2(-100.0, 200.0));
        camera.fit_to(Fit::Page, 1);
        assert_eq!(camera.rect(1), fitted, "fit resets panning even when already at the fitted scale");
        camera.resize(dvec2(320.0, 500.0));
        assert!(camera.rect(1).size.x <= 296.0 + 1e-6);
    }

    #[test]
    fn zoom_keeps_the_anchor_and_pan_is_bounded() {
        let mut camera = Camera::default();
        camera.resize(dvec2(600.0, 400.0));
        camera.set(vec![dvec2(1200.0, 800.0)], Fit::Page);
        let anchor = dvec2(200.0, 100.0);
        let before = camera.rect(0);
        camera.zoom_at(anchor, camera.scale * 3.0);
        let after = camera.rect(0);
        assert!(((anchor - before.pos) / before.size - (anchor - after.pos) / after.size).length() < 1e-10);
        camera.pan(dvec2(10000.0, -10000.0));
        assert_eq!(camera.offset.x, 0.0);
        assert_eq!(camera.offset.y, camera.viewport.y - camera.total.y);
        camera.fit_to(Fit::Page, 0);
        assert!(camera.rect(0).pos.x >= 0.0 && camera.rect(0).pos.y >= 0.0);
    }

    #[test]
    fn scrolling_survives_a_wrapped_verb_bar_and_window_resize() {
        let mut camera = Camera::default();
        camera.resize(dvec2(600.0, 700.0));
        camera.set(vec![dvec2(420.0, 595.0); 4], Fit::Width);
        camera.pan(dvec2(0.0, -600.0));
        assert_eq!(camera.current(), 1);
        let position = |camera: &Camera| {
            let rect = camera.rect(1);
            (camera.viewport * 0.5 - rect.pos) / rect.size
        };
        let reading = position(&camera);
        for viewport in [dvec2(600.0, 674.0), dvec2(380.0, 600.0)] {
            camera.resize(viewport);
            assert!((position(&camera) - reading).length() < 1e-10);
        }
        camera.fit_to(Fit::Width, 1);
        assert_eq!(camera.rect(1).pos.y, INSET);
    }
}
