//! The three modal surfaces: the launcher, the workspaces list, and the
//! history tree.
//!
//! They share a chassis — an ink wash that owns every hit, a sheet on it,
//! and one presence spring that carries wash, sheet and contents together.
//! Their rows live in a `PortalList`, whose item areas go stale the moment
//! a mid-gesture redraw lands, so the shell owns their clicks: a real press
//! and a scripted one resolve through the same hit table.

use kernel::nav::Nav;
use kernel::search::Go;
use kernel::session::Action;
use kernel::theme;
use kernel::time::fmt_date;
use makepad_widgets::*;

use super::draw::{rect, rgba_a, Style};
use super::dsl::LauncherOverlayWidgetRefExt;
use super::hits::{Act, Hit};
use super::hosted::{OVERLAY_LAUNCHER, OVERLAY_ROWS};
use super::stage::{Shell, Stage};

pub use super::dsl::{OverlayProps, OverlayRowData, OVERLAY_ROW_H};

/// Which modal surface is up. While one is up it owns every hit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Overlay {
    #[default]
    None,
    /// The workspaces list.
    Ws,
    /// The launcher: a query over everything (double-cmd).
    Launcher,
    /// The history tree: every action, walkable (cmd+u).
    History,
}

impl Stage {
    /// Keys while an overlay is up. The launcher owns the keyboard; the
    /// other two only take `esc`, so cmd chords still work through them.
    /// Answers whether the overlay took the key.
    pub(super) fn overlay_key(&mut self, cx: &mut Cx, sh: &mut Shell, k: &KeyEvent) -> bool {
        match sh.overlay {
            Overlay::Launcher => {
                match k.key_code {
                    KeyCode::Escape => {
                        sh.overlay = Overlay::None;
                        sh.session.redraw();
                    }
                    KeyCode::ReturnKey => {
                        if let Some(hit) = sh.launcher.selected().cloned() {
                            self.launcher_go(sh, hit.go);
                        }
                    }
                    // The hits are a ring: past the last is the first.
                    KeyCode::ArrowDown => {
                        sh.launcher.step(1);
                        sh.session.redraw();
                    }
                    KeyCode::ArrowUp => {
                        sh.launcher.step(-1);
                        sh.session.redraw();
                    }
                    // The query's own editing, caret and selection belong to
                    // its field now, so the key is forwarded, not re-read.
                    _ => self.forward_to_overlay(cx, sh, &Event::KeyDown(*k)),
                }
                true
            }
            Overlay::Ws | Overlay::History if k.key_code == KeyCode::Escape => {
                sh.overlay = Overlay::None;
                sh.session.redraw();
                true
            }
            _ => false,
        }
    }

    /// Double-cmd: raise the launcher, or put it away if it is up.
    pub(super) fn toggle_launcher(&mut self, cx: &mut Cx, sh: &mut Shell) {
        if sh.overlay == Overlay::Launcher {
            sh.overlay = Overlay::None;
            sh.session.redraw();
            return;
        }
        self.open_launcher(cx, sh);
    }

    /// Raises the launcher idempotently — tapping its own field must not
    /// reset a typed query.
    pub(super) fn open_launcher(&mut self, _cx: &mut Cx, sh: &mut Shell) {
        if sh.overlay != Overlay::Launcher {
            // A blank question, asked now: the switcher is on screen before
            // the key comes back up.
            let (windows, roots) = (sh.session.windows(), sh.session.roots());
            sh.launcher.open(&windows, &roots);
            sh.overlay = Overlay::Launcher;
        }
        // Typing lands in the query the moment it opens — but key focus set
        // during a draw does not take, so the next event tick does it.
        self.pending_focus = Some(OVERLAY_LAUNCHER);
        sh.session.redraw();
    }

    /// Asks the launcher's question again, with whatever was typed.
    pub(super) fn launcher_ask(&mut self, sh: &mut Shell, query: &str) {
        let (windows, roots) = (sh.session.windows(), sh.session.roots());
        sh.launcher.ask(&windows, &roots, query);
        sh.session.redraw();
    }

    /// Activates a hit: go to the panel wherever it lives, or open a fresh
    /// un-joined one on the active workspace. Never a second copy.
    pub(super) fn launcher_go(&mut self, sh: &mut Shell, go: Go) {
        sh.overlay = Overlay::None;
        match go {
            Go::Focus(slot) => sh.session.nav(Nav::Focus(slot)),
            Go::Open(id) => self.open_root(sh, id),
        }
        // The overlay coming down is a change of its own, as it is wherever
        // else one is put away: a hit that moves nothing marks nothing, and
        // the launcher would be left standing on a screen nobody redraws.
        sh.session.redraw();
    }

    /// Goes to a panel: focused wherever it already is — another workspace
    /// included — or opened beside what has focus. The launcher's verb, for
    /// everything else that reaches a root: a chord, a menu item, the
    /// problems mark in the corner of the chrome. Never a second copy.
    pub(super) fn go_to(&mut self, sh: &mut Shell, id: kernel::panel::PanelId) {
        let windows = sh.session.windows();
        self.launcher_go(sh, kernel::launcher::locate(&windows, &id));
    }

    /// Opens a root: beside whatever has focus, un-joined, or as the first
    /// panel of an empty workspace.
    pub(super) fn open_root(&mut self, sh: &mut Shell, id: kernel::panel::PanelId) {
        match sh.session.focus() {
            Some(from) => sh.session.nav(Nav::Open {
                from,
                id,
                fresh: true,
            }),
            None => {
                let label = format!("open “{id}”");
                sh.session.act(Action::new("open", label).moving(move |wm| {
                    wm.open(id, None, false);
                }));
            }
        }
    }

    /// Hands an event to whichever overlay widget is up. Its rows are
    /// presentation, so in practice this feeds the launcher's query field.
    pub(super) fn forward_to_overlay(&mut self, cx: &mut Cx, sh: &mut Shell, event: &Event) {
        let key = match sh.overlay {
            Overlay::Launcher => OVERLAY_LAUNCHER,
            Overlay::Ws | Overlay::History => OVERLAY_ROWS,
            Overlay::None => return,
        };
        let Some(w) = self.hosted.get(&key).cloned() else {
            return;
        };
        // Rows come from the shell each draw; event handling needs none.
        let props = OverlayProps::default();
        let mut scope = Scope::with_props(&props);
        w.handle_event(cx, event, &mut scope);
    }

    /// Draws whichever overlay is up, and registers its rows as hits.
    ///
    /// The chassis' presence rides one spring, 0 (away) → 1 (up): an open
    /// rises in, a close fades out with the last overlay still drawn,
    /// hit-less, until the spring has run out.
    pub(super) fn draw_overlay(&mut self, cx: &mut Cx2d, sh: &mut Shell, vp: Rect) {
        let live = sh.overlay != Overlay::None;
        sh.anim.overlay().retarget(if live { 1.0 } else { 0.0 });
        let p = sh.anim.overlay().value();
        if !sh.anim.overlay().is_done() {
            self.next_frame = cx.new_next_frame();
        }
        if live {
            sh.overlay_last = sh.overlay;
        } else if p <= 0.0 {
            self.hosted.remove(&OVERLAY_ROWS);
            self.hosted.remove(&OVERLAY_LAUNCHER);
            return;
        }
        let kind = if live { sh.overlay } else { sh.overlay_last };
        let launcher = kind == Overlay::Launcher;

        // The wash owns every hit while the overlay is live: a tap outside
        // the sheet dismisses it.
        if live {
            self.hits.clear();
        }
        self.draw_flat.new_draw_call(cx);
        self.draw_flat.color = rgba_a(theme::INK, 0.30 * p);
        self.draw_flat.draw_abs(cx, vp);
        if live {
            let label = match kind {
                Overlay::Ws => "workspaces",
                Overlay::History => "history",
                _ => "launcher",
            };
            self.hits
                .push(Hit::act(label, vp, MouseCursor::Default, Act::OverlayClose));
        }

        let (rows, acts, labels) = self.overlay_rows(sh, kind);

        // A centred sheet, hung a little below the top edge — a palette,
        // not a toolbar — that rises its last few points into place as it
        // fades in, and is as tall as its rows, up to the viewport.
        let w = (vp.size.x - 4.0 * theme::GAP).min(560.0);
        let x = vp.pos.x + (vp.size.x - w) / 2.0;
        let rise = (1.0 - p) * -12.0;
        let top = vp.pos.y + (vp.size.y * 0.14).max(2.0 * theme::GAP) + rise;
        let search_h = if kind == Overlay::Ws { 48.0 } else { 0.0 };
        let bottom = vp.pos.y + vp.size.y - 2.0 * theme::GAP;

        let tpl = if launcher {
            live_id!(launcher_overlay_tpl)
        } else {
            live_id!(rows_overlay_tpl)
        };
        let key = if launcher {
            OVERLAY_LAUNCHER
        } else {
            OVERLAY_ROWS
        };
        let Some((widget, created)) = self.hosted_widget(cx, key, tpl) else {
            return;
        };
        if created && launcher {
            self.pending_focus = Some(OVERLAY_LAUNCHER);
        }
        if launcher {
            widget
                .as_launcher_overlay()
                .scroll_to(cx, sh.launcher.sel());
        }

        // Fit height: the field and its rule (measured — a guess serves the
        // frame it is born on, at an alpha nobody sees), the rows, the frame.
        let field_h = if launcher {
            let fh = widget.as_launcher_overlay().query_rect(cx).size.y;
            if fh > 0.0 {
                fh + 1.0
            } else {
                50.0
            }
        } else {
            0.0
        };
        let n = rows.len().max(usize::from(launcher)) as f64;
        let h = (2.0 + field_h + n * OVERLAY_ROW_H).min((bottom - top - search_h).max(80.0));
        let r = rect(x, top + search_h, w, h);

        // Keep the sheet in its own draw call: a merged one would paint it
        // below the wash.
        self.draw_panel.new_draw_call(cx);
        self.draw_panel.color = rgba_a(theme::BG, 1.0);
        self.draw_panel.border_color = rgba_a(theme::INK, 1.0);
        self.draw_panel.border_size = 1.0;
        self.draw_panel.alpha = p as f32;
        self.draw_panel.draw_abs(cx, r);
        // The workspaces overlay's search row: the launcher's entry on
        // glass, above the roster. It says what the launcher searches —
        // the panels — because searching *into* what the apps hold is the
        // search panel's, and one word for two things is one too few.
        let sr = rect(x, top, w, 40.0);
        if kind == Overlay::Ws {
            self.draw_panel.draw_abs(cx, sr);
            self.draw_mono.new_draw_call(cx);
            self.set_text(Style::Muted, p);
            self.draw_mono.draw_abs(
                cx,
                dvec2(sr.pos.x + 16.0, sr.pos.y + (40.0 - self.cell.natural) / 2.0),
                "search panels",
            );
        }

        let props = OverlayProps {
            rows,
            query: sh.launcher.query().to_string(),
            alpha: p as f32,
        };
        let mut scope = Scope::with_props(&props);
        let inner = rect(r.pos.x + 1.0, r.pos.y + 1.0, r.size.x - 2.0, r.size.y - 2.0);
        cx.begin_turtle(
            Walk::abs_rect(inner),
            Layout {
                clip_x: true,
                clip_y: true,
                ..Default::default()
            },
        );
        widget.draw_all(cx, &mut scope);
        cx.end_turtle();

        // A closing overlay takes no clicks.
        if !live {
            return;
        }
        // The rows that actually drew become hits, above the wash.
        if let Some(list) = widget.widget(cx, ids!(list)).as_portal_list().borrow() {
            for (idx, item) in list.items().iter() {
                let ir = item.widget.area().rect(cx);
                if ir.size.x <= 0.0 {
                    continue;
                }
                if let (Some(act), Some(label)) = (acts.get(*idx), labels.get(*idx)) {
                    self.hits
                        .push(Hit::act(label.clone(), ir, MouseCursor::Hand, act.clone()));
                }
            }
        }
        if launcher {
            // A real field: a click only has to reach it, since the widget
            // owns its focus and its caret.
            let fr = widget.as_launcher_overlay().query_rect(cx);
            if fr.size.x > 0.0 {
                self.hits
                    .push(Hit::act("search", fr, MouseCursor::Text, Act::LauncherOpen));
            }
        } else if kind == Overlay::Ws {
            self.hits.push(Hit::act(
                "search panels",
                sr,
                MouseCursor::Hand,
                Act::LauncherOpen,
            ));
        }
    }

    /// The rows one overlay shows, with the act and the label of each.
    fn overlay_rows(
        &self,
        sh: &Shell,
        kind: Overlay,
    ) -> (Vec<OverlayRowData>, Vec<Act>, Vec<String>) {
        let mut rows = Vec::new();
        let mut acts = Vec::new();
        let mut labels = Vec::new();
        let hover = sh.hover.clone();
        match kind {
            Overlay::Ws => {
                let wm = sh.session.ws();
                for k in wm.roster() {
                    let ws = &wm.wss[k];
                    let summary = if ws.is_empty() {
                        "new".to_string()
                    } else {
                        ws.columns
                            .iter()
                            .flat_map(|c| c.slots.iter())
                            .filter_map(|s| sh.session.panel(*s))
                            .map(|p| p.borrow().title())
                            .collect::<Vec<_>>()
                            .join(" · ")
                    };
                    rows.push(OverlayRowData {
                        num: format!("{}", k + 1),
                        main: summary,
                        current: k == wm.active,
                        hovered: hover == Some(Act::WsRow(k)),
                        ..Default::default()
                    });
                    acts.push(Act::WsRow(k));
                    labels.push(format!("workspace {}", k + 1));
                }
            }
            Overlay::History => {
                let (nodes, head) = sh.session.history().rows();
                let mut depth: std::collections::HashMap<i64, usize> =
                    std::collections::HashMap::new();
                for n in &nodes {
                    let d = depth.get(&n.parent).map_or(0, |d| d + 1);
                    depth.insert(n.id, d);
                }
                for n in nodes.iter().rev() {
                    let ind = "  ".repeat((*depth.get(&n.id).unwrap_or(&0)).min(6));
                    rows.push(OverlayRowData {
                        main: format!("{ind}{}", n.label),
                        right: fmt_date(n.ts),
                        current: n.id == head,
                        muted: n.state != "applied",
                        hovered: hover == Some(Act::HistoryRow(n.id)),
                        ..Default::default()
                    });
                    acts.push(Act::HistoryRow(n.id));
                    labels.push(n.label.clone());
                }
                rows.push(OverlayRowData {
                    main: "the beginning".into(),
                    current: head == 0,
                    hovered: hover == Some(Act::HistoryRow(0)),
                    ..Default::default()
                });
                acts.push(Act::HistoryRow(0));
                labels.push("the beginning".into());
            }
            Overlay::Launcher => {
                // Pure read: the list was settled by the event that changed
                // it — a keystroke, or an answer arriving.
                for (i, hit) in sh.launcher.hits().iter().enumerate() {
                    rows.push(OverlayRowData {
                        main: hit.label.clone(),
                        detail: if hit.detail == hit.label {
                            String::new()
                        } else {
                            hit.detail.clone()
                        },
                        right: match hit.ws {
                            Some(k) => format!("#{}", k + 1),
                            None => "new".into(),
                        },
                        current: i == sh.launcher.sel(),
                        hovered: hover == Some(Act::LauncherRow(i)),
                        ..Default::default()
                    });
                    acts.push(Act::LauncherRow(i));
                    labels.push(hit.label.clone());
                }
            }
            Overlay::None => {}
        }
        (rows, acts, labels)
    }

    /// Walks the history tree to a node — the overlay's own verb.
    pub(super) fn travel(&mut self, sh: &mut Shell, node: i64) {
        sh.overlay = Overlay::None;
        if !sh.session.travel(node) {
            sh.session.notify("already there", false);
        }
    }
}
