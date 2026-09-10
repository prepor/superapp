//! The final screen owns only paint and a frame clock. It never mounts a
//! panel or reads the store while the session drains accepted work.

use kernel::theme;
use makepad_widgets::*;

use super::draw::{rect, rgba_a, DrawFlat};

#[derive(Script, ScriptHook, Widget)]
pub struct ClosingScreen {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_flat: DrawFlat,
    #[live]
    draw_title: DrawText,
    #[live]
    draw_label: DrawText,
    #[live]
    draw_status: DrawText,
    #[rust]
    area: Area,
    #[rust]
    next_frame: NextFrame,
    #[rust]
    started: Option<f64>,
    #[rust]
    animating: bool,
    /// Library fixtures can show a still without starting an animation.
    #[rust]
    elapsed: f64,
}

impl ClosingScreen {
    pub(super) fn set_elapsed(&mut self, elapsed: f64) {
        self.elapsed = elapsed;
    }

    pub(super) fn start(&mut self, cx: &mut Cx) {
        if self.animating {
            return;
        }
        self.animating = true;
        self.next_frame = cx.new_next_frame();
    }

    fn fill(&mut self, cx: &mut Cx2d, r: Rect, color: theme::Rgba) {
        // The window's depth bias is per draw call. Overlapping rectangles
        // must be separate calls so their fragments do not fight at equal depth.
        self.draw_flat.new_draw_call(cx);
        self.draw_flat.color = rgba_a(color, 1.0);
        self.draw_flat.draw_abs(cx, r);
    }

    fn frame(&mut self, cx: &mut Cx2d, r: Rect, stroke: f64, color: theme::Rgba) {
        let (x, y, w, h) = (r.pos.x, r.pos.y, r.size.x, r.size.y);
        for edge in [
            rect(x, y, w, stroke),
            rect(x, y + h - stroke, w, stroke),
            rect(x, y, stroke, h),
            rect(x + w - stroke, y, stroke, h),
        ] {
            self.fill(cx, edge, color);
        }
    }

    fn paint(&mut self, cx: &mut Cx2d, vp: Rect) {
        self.fill(cx, vp, theme::INK);

        // One composition, centred slightly above the optical middle. Scale
        // the spacing and illustration together on narrow or short windows.
        let scale = ((vp.size.x - 40.0) / 420.0)
            .min((vp.size.y - 40.0) / 340.0)
            .clamp(0.1, 1.0);
        let center = vp.pos.x + vp.size.x * 0.5;
        let top =
            vp.pos.y + (vp.size.y - 340.0 * scale) * 0.5 - (vp.size.y * 0.025).min(20.0) * scale;
        let origin = dvec2(center, top + 76.0 * scale);
        let guide = [0.22, 0.22, 0.22, 1.0];
        let secondary = [0.66, 0.66, 0.66, 1.0];

        // Quiet registration marks give the moving panels a fixed home.
        for (x, y) in [(-76.0, -64.0), (76.0, -64.0), (-76.0, 64.0), (76.0, 64.0)] {
            let p = origin + dvec2(x, y) * scale;
            self.fill(cx, rect(p.x - 3.0 * scale, p.y, 6.0 * scale, scale), guide);
            self.fill(cx, rect(p.x, p.y - 3.0 * scale, scale, 6.0 * scale), guide);
        }

        // The app icon's three-panel layout folds into a neatly filed stack.
        // This is a one-time gesture; only the small activity rule loops.
        for i in (0..3).rev() {
            let (from, size) = match i {
                0 => (dvec2(-48.0, -48.0), dvec2(44.0, 96.0)),
                1 => (dvec2(4.0, -48.0), dvec2(44.0, 44.0)),
                _ => (dvec2(4.0, 4.0), dvec2(44.0, 44.0)),
            };
            let t = ((self.elapsed - f64::from(2 - i) * 0.10) / 0.75).clamp(0.0, 1.0);
            let fold = 1.0 - (1.0 - t).powi(3);
            let offset = f64::from(i) * 10.0;
            let to = dvec2(-40.0 + offset, -30.0 - offset);
            let pos = origin + (from + (to - from) * fold) * scale;
            let size = (size + (dvec2(60.0, 80.0) - size) * fold) * scale;
            let panel = Rect { pos, size };
            let ink = if i == 0 { theme::BG } else { secondary };
            let stroke = 1.5 * scale;
            let header = 11.0 * scale;
            self.fill(cx, panel, theme::INK);
            self.frame(cx, panel, stroke, ink);
            if i == 0 {
                self.fill(cx, rect(pos.x, pos.y, size.x, header), ink);
                self.fill(
                    cx,
                    rect(
                        pos.x + 5.0 * scale,
                        pos.y + 5.0 * scale,
                        14.0 * scale,
                        scale,
                    ),
                    theme::INK,
                );
                // The last two lines settle with their panel, like a saved page.
                let line = [0.35, 0.35, 0.35, 1.0];
                self.fill(
                    cx,
                    rect(
                        pos.x + 9.0 * scale,
                        pos.y + 25.0 * scale,
                        size.x - 18.0 * scale,
                        scale,
                    ),
                    line,
                );
                self.fill(
                    cx,
                    rect(
                        pos.x + 9.0 * scale,
                        pos.y + 32.0 * scale,
                        (size.x - 18.0 * scale) * 0.62,
                        scale,
                    ),
                    line,
                );
            } else {
                self.fill(cx, rect(pos.x, pos.y + header, size.x, stroke), ink);
            }
        }

        self.draw_label.text_style.font_size = (theme::LABEL_SIZE * scale) as f32;
        self.draw_label.color = rgba_a(secondary, 1.0);
        // Tracking is explicit so the wordmark keeps the shell's label rhythm.
        let wordmark = "SUPERAPP";
        if let Some(run) = self.draw_label.prepare_single_line_run(cx, "M") {
            let advance = f64::from(run.width_in_lpxs);
            let step = advance + 2.5 * scale;
            let width = advance + step * (wordmark.len() - 1) as f64;
            for (i, ch) in wordmark.chars().enumerate() {
                self.draw_label.draw_abs(
                    cx,
                    dvec2(center - width * 0.5 + i as f64 * step, top + 172.0 * scale),
                    &ch.to_string(),
                );
            }
        }

        self.draw_title.text_style.font_size = (36.0 * scale) as f32;
        self.draw_title.color = rgba_a(theme::BG, 1.0);
        centered(
            &mut self.draw_title,
            cx,
            center,
            top + 200.0 * scale,
            "See you soon.",
        );

        self.draw_status.text_style.font_size = (theme::FONT_SIZE * scale) as f32;
        self.draw_status.color = rgba_a(secondary, 1.0);
        centered(
            &mut self.draw_status,
            cx,
            center,
            top + 267.0 * scale,
            "Closing your workspace",
        );

        // Indeterminate activity, with no invented percentage or finish time.
        let width = 104.0 * scale;
        let y = top + 312.0 * scale;
        self.fill(cx, rect(center - width * 0.5, y, width, scale), guide);
        let travel = 0.5 - 0.5 * (self.elapsed * std::f64::consts::TAU / 2.4).cos();
        let segment = 26.0 * scale;
        self.fill(
            cx,
            rect(
                center - width * 0.5 + (width - segment) * travel,
                y,
                segment,
                scale,
            ),
            theme::BG,
        );
    }
}

fn centered(draw: &mut DrawText, cx: &mut Cx2d, x: f64, y: f64, text: &str) {
    if let Some(run) = draw.prepare_single_line_run(cx, text) {
        draw.draw_abs(cx, dvec2(x - f64::from(run.width_in_lpxs) * 0.5, y), text);
    }
}

impl Widget for ClosingScreen {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        if !self.animating {
            return;
        }
        if let Some(frame) = self.next_frame.is_event(event) {
            let started = *self.started.get_or_insert(frame.time);
            self.elapsed = frame.time - started;
            self.next_frame = cx.new_next_frame();
            self.area.redraw(cx);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        cx.begin_turtle(walk, Layout::default());
        self.paint(cx, cx.turtle().rect());
        cx.end_turtle_with_area(&mut self.area);
        DrawStep::done()
    }
}
