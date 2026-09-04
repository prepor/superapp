//! The canvas's drawing half: what a frame renders, and what it shows.
//!
//! The rest of the panels library — the mounts, the camera, the input — is
//! in its [parent](super); this is the paint. A frame decides which mounts
//! may render into their own passes (the entered one, the one replaying, and
//! whatever the budget allows of the rest), draws every block's title, notes,
//! arrows and node captions in screen space, and composites each mount's
//! texture at the zoom it is shown at.

use std::collections::HashMap;

use kernel::scene::{TEXT_PT, TITLE_PT};
use kernel::theme;
use makepad_widgets::*;

use super::super::draw::{rgba_a, trunc};
use super::super::stage::Stage;
use super::{
    intersects, mount_dpi, status_h, to_rect, Budget, Hit, HitAct, Library, Live, MountPass,
    NAME_MIN_PT, SETTLE_TICKS,
};

impl Library {
    // -- drawing ----------------------------------------------------------------

    /// Canvas text: `pt` points at zoom 1, scaled with the camera. Below
    /// `min` screen points it is not drawn at all — unless `min` clamps it,
    /// which is how scene and node names stay legible from any height.
    pub(super) fn text(&mut self, cx: &mut Cx2d, pos: DVec2, pt: f64, color: theme::Rgba, s: &str) {
        self.text_min(cx, pos, pt, 2.0, false, color, s);
    }

    pub(super) fn label(
        &mut self,
        cx: &mut Cx2d,
        pos: DVec2,
        pt: f64,
        color: theme::Rgba,
        s: &str,
    ) {
        self.text_min(cx, pos, pt, 10.0, true, color, s);
    }

    #[allow(clippy::too_many_arguments)]
    fn text_min(
        &mut self,
        cx: &mut Cx2d,
        pos: DVec2,
        pt: f64,
        min: f32,
        clamp: bool,
        color: theme::Rgba,
        s: &str,
    ) {
        let mut size = (pt * self.zoom()) as f32;
        if size < min {
            if !clamp {
                return;
            }
            size = min;
        }
        if s.is_empty() {
            return;
        }
        // Only what the window can show: the canvas is mostly off-screen.
        let est_w = s.chars().count() as f64 * f64::from(size) * 0.7;
        if pos.x > self.vp.pos.x + self.vp.size.x
            || pos.y > self.vp.pos.y + self.vp.size.y
            || pos.x + est_w < self.vp.pos.x
            || pos.y + f64::from(size) * 1.5 < self.vp.pos.y
        {
            return;
        }
        self.draw_mono.new_draw_call(cx);
        self.draw_mono.text_style.font_size = size;
        self.draw_mono.color = rgba_a(color, 1.0);
        self.draw_mono.draw_abs(cx, pos, s);
    }

    pub(super) fn fill(&mut self, cx: &mut Cx2d, r: Rect, color: theme::Rgba) {
        self.draw_flat.color = rgba_a(color, 1.0);
        self.draw_flat.draw_abs(cx, r);
    }

    /// A one-pixel frame.
    pub(super) fn frame(&mut self, cx: &mut Cx2d, r: Rect, color: theme::Rgba) {
        let (x, y, w, h) = (r.pos.x, r.pos.y, r.size.x, r.size.y);
        for (rx, ry, rw, rh) in [
            (x, y, w, 1.0),
            (x, y + h - 1.0, w, 1.0),
            (x, y, 1.0, h),
            (x + w - 1.0, y, 1.0, h),
        ] {
            self.fill(
                cx,
                Rect {
                    pos: dvec2(rx, ry),
                    size: dvec2(rw, rh),
                },
                color,
            );
        }
    }

    /// A one-pixel line, horizontal or vertical, between two screen points.
    pub(super) fn line(&mut self, cx: &mut Cx2d, a: DVec2, b: DVec2, color: theme::Rgba) {
        if (a.y - b.y).abs() < 0.5 {
            let (x0, x1) = (a.x.min(b.x), a.x.max(b.x));
            if x1 - x0 < 0.5 {
                return;
            }
            self.fill(
                cx,
                Rect {
                    pos: dvec2(x0, a.y - 0.5),
                    size: dvec2(x1 - x0, 1.0),
                },
                color,
            );
        } else {
            let (y0, y1) = (a.y.min(b.y), a.y.max(b.y));
            self.fill(
                cx,
                Rect {
                    pos: dvec2(a.x - 0.5, y0),
                    size: dvec2(1.0, y1 - y0),
                },
                color,
            );
        }
    }

    /// Decides which mounts render this frame, and what the rest show.
    ///
    /// Live mounts — the entered one, and the stages still replaying —
    /// render whenever they drew or stepped; the entered one unbudgeted, the
    /// replaying ones within the frame's budget (a replay cannot step past a
    /// click until it has drawn). A frozen mount renders once more when its
    /// arrival is pending, and re-renders at a new zoom level only after the
    /// zoom has stood still, nearest the pointer first, within the same
    /// budget. Anything left over sets `more_work`, so the next frame comes.
    pub(super) fn plan_renders(&mut self, cx: &mut Cx2d, zoom: f64) -> Vec<bool> {
        let n = self.mounts.len();
        let mut render = vec![false; n];
        let win_dpi = cx.current_dpi_factor();
        let settled = self.zoom_ticks >= SETTLE_TICKS;
        let anchor = self.pointer.unwrap_or(self.vp.pos + self.vp.size * 0.5);
        let mut budget = Budget::new();
        let mut deferred: Vec<(f64, usize)> = Vec::new();
        let mut more_work = false;
        // `i` is the mount's identity here — `deferred` carries it to the
        // second pass, and the helpers take it — not a cursor into `render`.
        #[allow(clippy::needless_range_loop)]
        for i in 0..n {
            if self.mounts[i].live.is_none() || self.inline(i) {
                continue;
            }
            let replaying = self.mount_replaying(i);
            let entered = self.entered == Some(i);
            let screen = self.mount_rect(i).map(|r| self.screen_rect(r));
            let visible = screen.is_some_and(|r| intersects(r, self.vp));
            let want = mount_dpi(win_dpi, zoom, replaying);
            // Fold makepad's redraw mark into the mount's own flag: the mark
            // is consumed by this draw event whether or not the budget lets
            // the mount render in it.
            let walk = Walk::abs_rect(Rect {
                pos: dvec2(0.0, 0.0),
                size: self.mounts[i].size,
            });
            let marked = match self.mounts[i].pass.as_mut() {
                Some(mp) => cx.will_redraw(&mut mp.list, walk),
                None => true,
            };
            let m = &mut self.mounts[i];
            m.pending |= marked;
            let mismatch = (m.dpi - want).abs() > 1e-9;
            if entered {
                render[i] = m.pending || mismatch;
            } else if replaying || (visible && m.pending) {
                if m.pending {
                    if budget.ok() {
                        render[i] = true;
                        budget.spend();
                    } else {
                        more_work = true;
                    }
                }
            } else if visible && mismatch {
                if settled {
                    let c = screen.map_or(anchor, |r| r.pos + r.size * 0.5);
                    deferred.push(((c - anchor).length(), i));
                } else {
                    more_work = true;
                }
            }
        }
        deferred.sort_by(|a, b| a.0.total_cmp(&b.0));
        for (_, i) in deferred {
            if budget.ok() {
                render[i] = true;
                budget.spend();
            } else {
                more_work = true;
            }
        }
        self.more_work = more_work;
        render
    }

    /// An entered stage at 1:1 is drawn straight into the window, not
    /// through its texture: a render-to-texture pass and its composite
    /// double the GPU work of every animated frame, and a stage worked by
    /// hand animates on every beat. Drawn inline it costs exactly what the
    /// app costs. The texture path stays for everything else.
    pub(super) fn inline(&self, i: usize) -> bool {
        self.entered == Some(i)
            && (self.zoom() - 1.0).abs() < 1e-9
            && matches!(self.mounts[i].live, Some(Live::Stage(_)))
    }

    /// Draws an entered stage into the window at its screen rect. Its draw
    /// list is the same one its pass used, begun under the window's pass
    /// now, so its scoped redraws keep reaching only it.
    pub(super) fn draw_inline(&mut self, cx: &mut Cx2d, i: usize, screen: Rect) {
        let Some(Live::Stage(stage)) = self.mounts[i].live.clone() else {
            return;
        };
        let mut mp = self.mounts[i]
            .pass
            .take()
            .unwrap_or_else(|| MountPass::new(cx));
        if let (Some(mut st), Some(canvas)) = (stage.borrow_mut::<Stage>(), self.list_id) {
            st.set_lists(mp.list.id(), canvas);
        }
        mp.list.begin_always(cx);
        cx.begin_turtle(
            Walk::abs_rect(screen),
            Layout {
                clip_x: true,
                clip_y: true,
                ..Layout::default()
            },
        );
        stage.draw_all(cx, &mut Scope::empty());
        cx.end_turtle();
        mp.list.end(cx);
        // The texture is stale from here on; leaving renders it afresh.
        self.mounts[i].pending = true;
        self.mounts[i].pass = Some(mp);
    }

    /// Shows one mount: renders its pass if the plan says so, then draws its
    /// texture — the fresh one, or the last one scaled to the current zoom.
    /// A mount with no texture yet draws nothing but its frame.
    pub(super) fn draw_mount(&mut self, cx: &mut Cx2d, i: usize, screen: Rect, render: bool) {
        if self.inline(i) {
            self.draw_inline(cx, i, screen);
            return;
        }
        let visible = intersects(screen, self.vp);
        if !render && (!visible || self.mounts[i].pass.is_none()) {
            return;
        }
        let Some(live) = self.mounts[i].live.clone() else {
            return;
        };
        let win_dpi = cx.current_dpi_factor();
        let replaying = self.mount_replaying(i);
        let dpi = mount_dpi(win_dpi, self.zoom(), replaying);
        let size = self.mounts[i].size;
        let mut mp = self.mounts[i]
            .pass
            .take()
            .unwrap_or_else(|| MountPass::new(cx));

        // The pass rect comes from an area of the parent: a transparent quad
        // the mount's logical size, so the texture is `size × dpi` whatever
        // the canvas shows it at. Drawn every frame — the area is an
        // instance in this draw list, which is rebuilt with it.
        self.draw_flat.color = vec4(0.0, 0.0, 0.0, 0.0);
        self.draw_flat.draw_abs(
            cx,
            Rect {
                pos: screen.pos,
                size,
            },
        );
        let helper = self.draw_flat.area();

        if render {
            let walk = Walk::abs_rect(Rect {
                pos: dvec2(0.0, 0.0),
                size,
            });
            self.mounts[i].dpi = dpi;
            self.mounts[i].pending = false;
            let props = self.overlay_props(i);
            cx.make_child_pass(&mp.pass);
            cx.begin_pass(&mp.pass, Some(dpi));
            mp.list.begin_always(cx);
            match &live {
                Live::Stage(stage) => {
                    if let (Some(mut st), Some(canvas)) =
                        (stage.borrow_mut::<Stage>(), self.list_id)
                    {
                        st.set_lists(mp.list.id(), canvas);
                    }
                    cx.begin_turtle(walk, Layout::default());
                    stage.draw_all(cx, &mut Scope::empty());
                    cx.end_turtle();
                }
                Live::Widget(w) => {
                    let mut scope = match &props {
                        Some(p) => Scope::with_props(p),
                        None => Scope::empty(),
                    };
                    cx.begin_turtle(
                        walk,
                        Layout {
                            flow: Flow::Down,
                            ..Layout::default()
                        },
                    );
                    w.draw_all(cx, &mut scope);
                    cx.end_turtle();
                }
            }
            mp.list.end(cx);
            cx.end_pass(&mp.pass);
        }
        cx.set_pass_area_with_origin(&mp.pass, helper, dvec2(0.0, 0.0));
        if visible {
            self.draw_tex.draw_vars.set_texture(0, &mp.tex);
            self.draw_tex.draw_abs(cx, screen);
        }
        self.mounts[i].pass = Some(mp);
    }

    pub(super) fn draw_canvas(&mut self, cx: &mut Cx2d) {
        let Some(canvas) = self.canvas.clone() else {
            return;
        };
        // Components come up on the first draw: a widget each, no store.
        for i in 0..self.mounts.len() {
            if !self.is_stage(i) {
                self.ensure_booted(cx, i);
            }
        }
        let zoom = self.zoom();
        let line = self.metrics.map_or(20.0, |m| m.line * TEXT_PT);
        self.hits.clear();
        let entered = self.entered;
        let plan = self.plan_renders(cx, zoom);
        let scenes = self.scenes.clone();
        // Mounts by (scene, node), for the hits.
        let index: HashMap<(usize, usize), usize> = self
            .mounts
            .iter()
            .enumerate()
            .map(|(i, m)| ((m.scene, m.node), i))
            .collect();

        for block in &canvas.blocks {
            let sc = &scenes[block.scene];
            // Names are clamped to a legible size, so far out they would sit
            // on the frames they belong to: the block's labels are laid in
            // screen space instead — a node's name just above its mount, the
            // title just above the first row of those.
            let line_px = self.metrics.map_or(1.3, |m| m.line);
            let name_px = (TEXT_PT * zoom).max(10.0) * line_px;
            let first_top = block
                .nodes
                .iter()
                .map(|nb| self.to_screen(dvec2(nb.rect.x, nb.rect.y)).y)
                .fold(f64::INFINITY, f64::min);
            // Far out the names are left out (below), and the title sits
            // right over the first mount instead of over where they were.
            let names_shown = zoom * TEXT_PT >= NAME_MIN_PT;
            let names_y = if names_shown {
                block
                    .nodes
                    .iter()
                    .map(|nb| self.to_screen(dvec2(nb.caption.0, nb.caption.1)).y)
                    .fold(f64::INFINITY, f64::min)
                    .min(first_top - name_px - 4.0)
            } else {
                first_top - 4.0
            };
            let title_px = (TITLE_PT * zoom).max(10.0) * line_px;
            let title_canvas = self.to_screen(dvec2(block.title.0, block.title.1));
            let title_at = dvec2(title_canvas.x, title_canvas.y.min(names_y - title_px - 6.0));
            self.label(cx, title_at, TITLE_PT, theme::INK, &sc.name);
            let name_w = sc.name.chars().count() as f64
                * self.metrics.map_or(0.6, |m| m.adv)
                * (TITLE_PT * zoom).max(10.0);
            let count = format!(
                "{} state{}",
                sc.nodes.len(),
                if sc.nodes.len() == 1 { "" } else { "s" }
            );
            self.text(
                cx,
                title_at + dvec2(name_w + 24.0 * zoom, title_px - line * zoom),
                TEXT_PT,
                theme::MUTED,
                &count,
            );
            self.hits.push(Hit {
                label: sc.name.clone(),
                rect: Rect {
                    pos: title_at,
                    size: dvec2(name_w, title_px),
                },
                act: HitAct::Scene(block.scene),
            });
            let mut y = self.to_screen(dvec2(block.note.0, block.note.1)).y;
            for l in &sc.note {
                self.text(cx, dvec2(title_at.x, y), TEXT_PT, theme::TEXT2, l);
                y += line * zoom;
            }
            for a in &block.arrows {
                let from = self.to_screen(dvec2(a.from.0, a.from.1));
                let to = self.to_screen(dvec2(a.to.0, a.to.1));
                let ex = self.to_screen(dvec2(a.elbow_x, 0.0)).x;
                let head = (14.0 * zoom).max(4.0);
                // Out to the right, along the elbow, in from the left.
                self.line(cx, from, dvec2(ex, from.y), theme::INK);
                self.line(cx, dvec2(ex, from.y), dvec2(ex, to.y), theme::INK);
                self.line(cx, dvec2(ex, to.y), dvec2(to.x - head, to.y), theme::INK);
                self.draw_head.color = rgba_a(theme::INK, 1.0);
                self.draw_head.draw_abs(
                    cx,
                    Rect {
                        pos: dvec2(to.x - head, to.y - head / 2.0),
                        size: dvec2(head, head),
                    },
                );
                let at = self.to_screen(dvec2(a.label_at.0, a.label_at.1));
                self.text(cx, at, TEXT_PT, theme::INK, &a.label);
            }
            for nb in &block.nodes {
                let node = &sc.nodes[nb.node];
                let screen = self.screen_rect(to_rect(nb.rect));
                let i = index.get(&(block.scene, nb.node)).copied();
                let is_entered = i.is_some() && i == entered;
                // The caption: the node's name (inverted while entered, the
                // way a focused panel's header is), then the note.
                let cap_canvas = self.to_screen(dvec2(nb.caption.0, nb.caption.1));
                let cap = dvec2(cap_canvas.x, cap_canvas.y.min(screen.pos.y - name_px - 4.0));
                let name_w = node.name.chars().count() as f64
                    * self.metrics.map_or(0.6, |m| m.adv)
                    * (TEXT_PT * zoom).max(10.0);
                if is_entered {
                    self.fill(
                        cx,
                        Rect {
                            pos: cap - dvec2(4.0, 2.0),
                            size: dvec2(name_w + 8.0, name_px + 4.0),
                        },
                        theme::INK,
                    );
                }
                // The name stays legible from any height while there is room
                // for it: far out, names would pile onto the nodes above, so
                // only the scene titles remain until the zoom comes in. The
                // entered node always keeps its name.
                let adv_px = self.metrics.map_or(0.6, |m| m.adv) * (TEXT_PT * zoom).max(10.0);
                let fit = (screen.size.x / adv_px).floor() as usize;
                let shown = if !is_entered && !names_shown {
                    String::new()
                } else if name_w <= screen.size.x || zoom * TEXT_PT >= 10.0 {
                    node.name.clone()
                } else if fit >= 4 {
                    trunc(&node.name, fit)
                } else {
                    String::new()
                };
                self.label(
                    cx,
                    cap,
                    TEXT_PT,
                    if is_entered { theme::BG } else { theme::INK },
                    &shown,
                );
                let mut ny = cap_canvas.y + line * zoom;
                for l in &node.note {
                    self.text(cx, dvec2(cap.x, ny), TEXT_PT, theme::TEXT2, l);
                    ny += line * zoom;
                }
                if let Some(i) = i {
                    self.draw_mount(cx, i, screen, plan[i]);
                    let replaying = self.mount_replaying(i);
                    self.frame(
                        cx,
                        Rect {
                            pos: screen.pos - dvec2(1.0, 1.0),
                            size: screen.size + dvec2(2.0, 2.0),
                        },
                        if replaying { theme::MUTED } else { theme::INK },
                    );
                    if replaying {
                        // A node still on its way: a wash, so it is not
                        // mistaken for a state.
                        self.draw_flat.color = rgba_a(theme::BG, 0.6);
                        self.draw_flat.draw_abs(cx, screen);
                    }
                    let label = format!("{}/{}", sc.name, node.name);
                    self.hits.push(Hit {
                        label: label.clone(),
                        rect: Rect {
                            pos: cap,
                            size: dvec2(name_w, name_px),
                        },
                        act: HitAct::Enter(i),
                    });
                    self.hits.push(Hit {
                        label,
                        rect: screen,
                        act: HitAct::Enter(i),
                    });
                }
            }
        }
    }

    /// The strip at the foot, screen-fixed: what is still on its way, while
    /// anything is. Nothing else is written there — the canvas's keys are in
    /// its own chapter, not on a legend.
    pub(super) fn draw_status(&mut self, cx: &mut Cx2d) {
        let replaying = self.replaying();
        if replaying == 0 {
            return;
        }
        let status = format!("{replaying} of {} nodes to go", self.mounts.len());
        let size = theme::FONT_SIZE as f32;
        let h = status_h();
        self.fill(
            cx,
            Rect {
                pos: dvec2(self.vp.pos.x, self.vp.pos.y + self.vp.size.y - h),
                size: dvec2(self.vp.size.x, h),
            },
            theme::BG,
        );
        self.fill(
            cx,
            Rect {
                pos: dvec2(self.vp.pos.x, (self.vp.pos.y + self.vp.size.y - h).round()),
                size: dvec2(self.vp.size.x, 1.0),
            },
            theme::RULE,
        );
        self.draw_mono.new_draw_call(cx);
        self.draw_mono.text_style.font_size = size;
        self.draw_mono.color = rgba_a(theme::TEXT2, 1.0);
        self.draw_mono.draw_abs(
            cx,
            dvec2(
                self.vp.pos.x + theme::PAD_X,
                self.vp.pos.y + self.vp.size.y - h + f64::from(size) * 0.6,
            ),
            &status,
        );
    }
}
