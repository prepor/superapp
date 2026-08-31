//! Pure panel/column/join state machine + layout targets. No rendering, no I/O,
//! no makepad — mirrors mosaic's `wm-core` division of labour: this module owns
//! *what* the layout is; the shell owns *how it gets there* (springs).
//!
//! The model is the one the web prototype (`web/`) validated:
//! - a **panel** is kind + params and requests grid units on a 12×6 grid;
//!   heights are honoured literally — unused rows stay empty;
//! - solid links **open joined**, dotted links **replace in place**, buttons
//!   are side effects (links live in panel content, i.e. the shell);
//! - a **join** is alive only while the child sits in the column immediately
//!   right of its parent; the next open from the parent replaces the joined
//!   child; **replacing a panel closes its joined chain**.

use std::collections::HashMap;

use crate::data::MailId;

/// Stable panel identity.
pub type PanelId = u64;

/// Grid columns across the viewport.
pub const GRID_W: u32 = 12;
/// Grid rows down the viewport.
pub const GRID_H: u32 = 6;

/// A panel's kind, parameters included: the whole identity of what it shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// The legend + keys panel.
    Help,
    /// One line about the prototype.
    About,
    /// The mail list, optionally pre-filtered (a contact's address).
    Inbox {
        /// Substring filter baked into the panel's params (not the typed one).
        filter: Option<&'static str>,
    },
    /// One mail.
    Message {
        /// Which mail.
        id: MailId,
    },
    /// A sender's card.
    Contact {
        /// The sender's address.
        email: &'static str,
    },
    /// A reply draft.
    Compose {
        /// The mail being replied to.
        re: MailId,
    },
}

impl Kind {
    /// Requested grid size, width × height.
    #[must_use]
    pub fn grid(&self) -> (u32, u32) {
        match self {
            Kind::Help => (4, 6),
            Kind::About => (3, 2),
            Kind::Inbox { .. } => (4, 6),
            Kind::Message { .. } => (4, 3),
            Kind::Contact { .. } => (3, 2),
            Kind::Compose { .. } => (4, 4),
        }
    }
}

/// One panel.
#[derive(Debug, Clone)]
pub struct Panel {
    /// Identity.
    pub id: PanelId,
    /// What it shows. Replacement swaps this in place, keeping the id.
    pub kind: Kind,
}

/// One column: panel ids, top to bottom, plus its display mode.
#[derive(Debug, Clone, Default)]
pub struct Column {
    /// Panels, top to bottom.
    pub panels: Vec<PanelId>,
    /// niri-style tabbed display: only the active panel shows, full height,
    /// under a tab strip.
    pub tabbed: bool,
    /// Which panel a tabbed column shows while unfocused. Kept in sync with
    /// focus by [`Ws::normalize`].
    pub active: usize,
}

impl Column {
    fn of(pid: PanelId) -> Self {
        Column {
            panels: vec![pid],
            ..Default::default()
        }
    }
}

/// A focus/move direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    /// Left.
    Left,
    /// Right.
    Right,
    /// Up.
    Up,
    /// Down.
    Down,
}

/// A rectangle in strip coordinates, logical points.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    /// Left edge.
    pub x: f64,
    /// Top edge.
    pub y: f64,
    /// Width.
    pub w: f64,
    /// Height.
    pub h: f64,
}

impl Rect {
    /// Right edge.
    #[must_use]
    pub fn right(&self) -> f64 {
        self.x + self.w
    }
    /// Bottom edge.
    #[must_use]
    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }
    /// Whether the point is inside.
    #[must_use]
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }
}

/// One panel's discrete layout target.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PanelScene {
    /// The panel.
    pub id: PanelId,
    /// Target rect in strip coordinates.
    pub rect: Rect,
}

/// Discrete layout targets for the whole workspace — the analogue of
/// `wm-core`'s `Scene`. The shell interpolates towards these with springs.
#[derive(Debug, Clone, PartialEq)]
pub struct Scene {
    /// Strip x at the viewport's left edge.
    pub camera_x: f64,
    /// Every panel's target, in column order.
    pub panels: Vec<PanelScene>,
    /// Live joins, `(parent, child)`, both guaranteed present in `panels`.
    pub bridges: Vec<(PanelId, PanelId)>,
    /// The focused panel.
    pub focus: Option<PanelId>,
}

/// Layout constants the scene is computed with.
#[derive(Debug, Clone, Copy)]
pub struct LayoutOpts {
    /// Gap between panels and edges, in points.
    pub gap: f64,
}

impl Default for LayoutOpts {
    fn default() -> Self {
        Self {
            gap: crate::theme::GAP,
        }
    }
}

/// The workspace: every panel, their columns, joins and focus.
#[derive(Debug, Clone, Default)]
pub struct Ws {
    next_id: u64,
    /// Columns, left to right.
    pub columns: Vec<Column>,
    /// Panels by id.
    pub panels: HashMap<PanelId, Panel>,
    /// Joins, parent → child. At most one child per parent.
    pub joins: HashMap<PanelId, PanelId>,
    /// The focused panel.
    pub focus: Option<PanelId>,
    /// Camera x target in strip coordinates.
    pub camera_x: f64,
}

impl Ws {
    /// An empty workspace.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `(column index, row index)` of a panel.
    #[must_use]
    pub fn locate(&self, pid: PanelId) -> Option<(usize, usize)> {
        for (c, col) in self.columns.iter().enumerate() {
            if let Some(r) = col.panels.iter().position(|&p| p == pid) {
                return Some((c, r));
            }
        }
        None
    }

    fn col_of(&self, pid: PanelId) -> Option<usize> {
        self.locate(pid).map(|(c, _)| c)
    }

    /// Sum of requested grid rows in a column.
    #[must_use]
    pub fn col_used_h(&self, col: &Column) -> u32 {
        col.panels
            .iter()
            .filter_map(|pid| self.panels.get(pid))
            .map(|p| p.kind.grid().1)
            .sum()
    }

    /// Requested grid width of a column: its widest panel.
    #[must_use]
    pub fn col_w(&self, col: &Column) -> u32 {
        col.panels
            .iter()
            .filter_map(|pid| self.panels.get(pid))
            .map(|p| p.kind.grid().0)
            .max()
            .unwrap_or(1)
    }

    /// The joined child of `pid`, if the join is alive.
    #[must_use]
    pub fn joined_child(&self, pid: PanelId) -> Option<PanelId> {
        self.joins.get(&pid).copied()
    }

    /// The parent `pid` is joined to, if any.
    #[must_use]
    pub fn join_parent_of(&self, pid: PanelId) -> Option<PanelId> {
        self.joins
            .iter()
            .find(|&(_, &c)| c == pid)
            .map(|(&p, _)| p)
    }

    /// A join is alive only while the child sits in the column immediately
    /// right of its parent — any move or insert that breaks adjacency breaks
    /// the join.
    fn validate_joins(&mut self) {
        let stale: Vec<PanelId> = self
            .joins
            .iter()
            .filter(|&(&a, &b)| {
                let (Some(ca), Some(cb)) = (self.col_of(a), self.col_of(b)) else {
                    return true;
                };
                cb != ca + 1
            })
            .map(|(&a, _)| a)
            .collect();
        for a in stale {
            self.joins.remove(&a);
        }
    }

    fn mk_panel(&mut self, kind: Kind) -> PanelId {
        self.next_id += 1;
        let id = self.next_id;
        self.panels.insert(id, Panel { id, kind });
        id
    }

    fn remove_from_layout(&mut self, pid: PanelId) {
        if let Some((c, r)) = self.locate(pid) {
            self.columns[c].panels.remove(r);
            if self.columns[c].panels.is_empty() {
                self.columns.remove(c);
            }
        }
    }

    /// Replacing a panel closes everything joined to it, transitively — the
    /// chain to its right is context derived from content that just changed.
    fn close_joined_chain(&mut self, pid: PanelId) {
        if let Some(child) = self.joins.remove(&pid) {
            self.close_joined_chain(child);
            self.remove_from_layout(child);
            self.panels.remove(&child);
        }
    }

    /// "Open to the right": reuse the right-hand column if the new panel's
    /// rows fit, otherwise insert a fresh column. A joined child must land in
    /// the column immediately right of its parent (a join only lives there);
    /// an un-joined open respects an existing pair and goes after it instead.
    fn place_near(&mut self, pid: PanelId, from_col: usize, joined: bool) {
        let h = self.panels[&pid].kind.grid().1;
        if let Some(right) = self.columns.get(from_col + 1) {
            if self.col_used_h(right) + h <= GRID_H {
                self.columns[from_col + 1].panels.push(pid);
                return;
            }
        }
        let mut at = from_col + 1;
        if !joined {
            let sources: Vec<PanelId> = self.columns[from_col].panels.clone();
            for src in sources {
                if let Some(child) = self.joins.get(&src) {
                    if self.col_of(*child) == Some(from_col + 1) {
                        at = from_col + 2;
                    }
                }
            }
        }
        let at = at.min(self.columns.len());
        self.columns.insert(at, Column::of(pid));
    }

    /// Opens a panel. With `from`, it lands to the right of that panel's
    /// column; with `join`, it becomes that panel's joined child. Returns the
    /// new panel's id.
    pub fn open(&mut self, kind: Kind, from: Option<PanelId>, join: bool) -> PanelId {
        let pid = self.mk_panel(kind);
        match from.and_then(|f| self.col_of(f)) {
            Some(c) => {
                self.place_near(pid, c, join);
                if join {
                    if let Some(f) = from {
                        self.joins.insert(f, pid);
                    }
                }
            }
            None => self.columns.push(Column::of(pid)),
        }
        self.validate_joins();
        self.focus = Some(pid);
        pid
    }

    /// Follows a solid link from `panel`: re-targets the existing joined child
    /// if one is alive, otherwise opens a new joined panel. `alt` always opens
    /// a fresh, un-joined panel. Returns the id of the panel that now shows
    /// `kind`.
    pub fn follow_open(&mut self, panel: PanelId, kind: Kind, alt: bool) -> PanelId {
        if alt {
            return self.open(kind, Some(panel), false);
        }
        match self.joined_child(panel) {
            Some(child) if self.panels.contains_key(&child) => {
                self.replace(child, kind);
                child
            }
            _ => self.open(kind, Some(panel), true),
        }
    }

    /// Follows a dotted link: replaces `panel` in place. `alt` opens a fresh,
    /// un-joined panel instead. Returns the id of the panel that now shows
    /// `kind`.
    pub fn follow_replace(&mut self, panel: PanelId, kind: Kind, alt: bool) -> PanelId {
        if alt {
            return self.open(kind, Some(panel), false);
        }
        self.replace(panel, kind);
        panel
    }

    /// Replaces a panel's kind in place (same id, same slot), closing its
    /// joined chain first.
    pub fn replace(&mut self, pid: PanelId, kind: Kind) {
        if !self.panels.contains_key(&pid) {
            return;
        }
        self.close_joined_chain(pid);
        if let Some(p) = self.panels.get_mut(&pid) {
            p.kind = kind;
        }
        self.validate_joins();
        self.focus = Some(pid);
    }

    /// Closes a panel; focus falls to its nearest surviving neighbour.
    pub fn close(&mut self, pid: PanelId) {
        let Some((c, r)) = self.locate(pid) else {
            return;
        };
        self.remove_from_layout(pid);
        self.panels.remove(&pid);
        self.joins.retain(|&a, &mut b| a != pid && b != pid);
        self.validate_joins();
        if self.focus == Some(pid) {
            self.focus = None;
            if !self.columns.is_empty() {
                let c = c.min(self.columns.len() - 1);
                let col = &self.columns[c];
                let r = r.min(col.panels.len().saturating_sub(1));
                self.focus = col.panels.get(r).copied();
            }
        }
    }

    /// Keeps per-column invariants: `active` clamped, and following focus.
    fn normalize(&mut self) {
        for col in &mut self.columns {
            col.active = col.active.min(col.panels.len().saturating_sub(1));
        }
        if let Some(f) = self.focus {
            if let Some((c, r)) = self.locate(f) {
                self.columns[c].active = r;
            }
        }
    }

    /// niri's `consume-or-expel-window-left/right`: a lone panel is consumed
    /// into the neighbouring column on that side (at its bottom); a stacked
    /// panel is expelled into a fresh column on that side.
    pub fn consume_or_expel(&mut self, pid: PanelId, dir: Dir) {
        let Some((c, r)) = self.locate(pid) else {
            return;
        };
        let d: isize = match dir {
            Dir::Left => -1,
            Dir::Right => 1,
            _ => return,
        };
        if self.columns[c].panels.len() == 1 {
            let t = c as isize + d;
            if t < 0 || t as usize >= self.columns.len() {
                return;
            }
            let t = t as usize;
            self.columns.remove(c);
            let t = if t > c { t - 1 } else { t };
            self.columns[t].panels.push(pid);
            self.columns[t].active = self.columns[t].panels.len() - 1;
        } else {
            self.columns[c].panels.remove(r);
            let at = if d < 0 { c } else { c + 1 };
            self.columns.insert(at, Column::of(pid));
        }
        self.validate_joins();
        self.normalize();
    }

    /// niri's `consume-window-into-column`: the first panel of the column to
    /// the right joins the bottom of `pid`'s column.
    pub fn consume_from_right(&mut self, pid: PanelId) {
        let Some((c, _)) = self.locate(pid) else {
            return;
        };
        if c + 1 >= self.columns.len() {
            return;
        }
        let moved = self.columns[c + 1].panels.remove(0);
        if self.columns[c + 1].panels.is_empty() {
            self.columns.remove(c + 1);
        }
        self.columns[c].panels.push(moved);
        self.validate_joins();
        self.normalize();
    }

    /// niri's `expel-window-from-column`: the bottom panel of `pid`'s column
    /// expels into a fresh column to the right.
    pub fn expel_bottom(&mut self, pid: PanelId) {
        let Some((c, _)) = self.locate(pid) else {
            return;
        };
        if self.columns[c].panels.len() < 2 {
            return;
        }
        let moved = self.columns[c].panels.pop().expect("len checked");
        self.columns.insert(c + 1, Column::of(moved));
        self.validate_joins();
        self.normalize();
    }

    /// niri's `toggle-column-tabbed-display` on the panel's column.
    pub fn toggle_tabbed(&mut self, pid: PanelId) {
        if let Some((c, _)) = self.locate(pid) {
            self.columns[c].tabbed = !self.columns[c].tabbed;
            self.normalize();
        }
    }

    /// Moves a panel one step. Within a column it swaps; across columns it
    /// merges into the neighbour (or swaps whole columns when it travels
    /// alone); at the edges a stacked panel expels into a fresh column.
    pub fn move_panel(&mut self, pid: PanelId, dir: Dir) {
        let Some((c, r)) = self.locate(pid) else {
            return;
        };
        match dir {
            Dir::Up | Dir::Down => {
                let t = if dir == Dir::Up {
                    r.checked_sub(1)
                } else {
                    Some(r + 1)
                };
                if let Some(t) = t {
                    if t < self.columns[c].panels.len() {
                        self.columns[c].panels.swap(r, t);
                    }
                }
            }
            Dir::Left | Dir::Right => {
                let d: isize = if dir == Dir::Left { -1 } else { 1 };
                let t = c as isize + d;
                if self.columns[c].panels.len() == 1 {
                    if t < 0 || t as usize >= self.columns.len() {
                        return; // lone column at the edge
                    }
                    self.columns.swap(c, t as usize);
                } else {
                    self.columns[c].panels.remove(r);
                    if t < 0 || t as usize >= self.columns.len() {
                        let at = if d < 0 { c } else { c + 1 };
                        self.columns.insert(at, Column::of(pid));
                    } else {
                        let tc = &mut self.columns[t as usize];
                        let at = r.min(tc.panels.len());
                        tc.panels.insert(at, pid);
                    }
                }
            }
        }
        self.validate_joins();
    }

    /// Moves focus one step. Up/down walk the column; left/right pick the
    /// panel with the nearest vertical centre in the neighbouring column,
    /// judged on the scene's target geometry.
    pub fn focus_dir(&mut self, dir: Dir, viewport: (f64, f64), opts: LayoutOpts) {
        let Some(cur) = self.focus else {
            self.focus = self.columns.first().and_then(|c| c.panels.first()).copied();
            return;
        };
        let Some((c, r)) = self.locate(cur) else {
            return;
        };
        match dir {
            Dir::Up => {
                if r > 0 {
                    self.focus = Some(self.columns[c].panels[r - 1]);
                }
            }
            Dir::Down => {
                if r + 1 < self.columns[c].panels.len() {
                    self.focus = Some(self.columns[c].panels[r + 1]);
                }
            }
            Dir::Left | Dir::Right => {
                let t = if dir == Dir::Left {
                    c.checked_sub(1)
                } else {
                    Some(c + 1)
                };
                let Some(t) = t.filter(|&t| t < self.columns.len()) else {
                    return;
                };
                // A tabbed column is entered on its active tab — the hidden
                // panels have no geometry to be "nearest" by.
                if self.columns[t].tabbed {
                    let col = &self.columns[t];
                    if let Some(&pid) = col.panels.get(col.active.min(col.panels.len() - 1)) {
                        self.focus = Some(pid);
                        self.normalize();
                    }
                    return;
                }
                let scene = self.scene(viewport, opts);
                let rect_of = |pid: PanelId| {
                    scene
                        .panels
                        .iter()
                        .find(|p| p.id == pid)
                        .map(|p| p.rect)
                        .unwrap_or_default()
                };
                let cur_mid = {
                    let r = rect_of(cur);
                    r.y + r.h / 2.0
                };
                let best = self.columns[t]
                    .panels
                    .iter()
                    .min_by(|&&a, &&b| {
                        let da = (rect_of(a).y + rect_of(a).h / 2.0 - cur_mid).abs();
                        let db = (rect_of(b).y + rect_of(b).h / 2.0 - cur_mid).abs();
                        da.total_cmp(&db)
                    })
                    .copied();
                if let Some(best) = best {
                    self.focus = Some(best);
                }
            }
        }
    }

    fn layout_panels(&self, viewport: (f64, f64), opts: LayoutOpts) -> (Vec<PanelScene>, f64) {
        let (vw, vh) = viewport;
        let gap = opts.gap;
        let unit_w = (vw - gap) / f64::from(GRID_W);
        let row_u = (vh - 2.0 * gap - f64::from(GRID_H - 1) * gap) / f64::from(GRID_H);

        let mut panels = Vec::new();
        let mut x = gap;
        for col in &self.columns {
            let cw = (unit_w * f64::from(self.col_w(col)) - gap).max(40.0);
            if col.tabbed {
                // Tabbed: only the active panel, full height under the strip.
                let active = col.active.min(col.panels.len().saturating_sub(1));
                if let Some(pid) = col.panels.get(active) {
                    let top = gap + crate::theme::TAB_H + crate::theme::TAB_GAP;
                    panels.push(PanelScene {
                        id: *pid,
                        rect: Rect {
                            x,
                            y: top,
                            w: cw,
                            h: (vh - top - gap).max(40.0),
                        },
                    });
                }
                x += cw + gap;
                continue;
            }
            // Requested heights are honoured while they fit; a column asked to
            // hold more than the grid distributes its space evenly instead
            // (consume/expel deliberately over-fill columns).
            let n = col.panels.len();
            let even = self.col_used_h(col) > GRID_H && n > 0;
            let even_h = (vh - 2.0 * gap - (n.saturating_sub(1)) as f64 * gap) / n.max(1) as f64;
            let mut y = gap;
            for pid in &col.panels {
                let Some(p) = self.panels.get(pid) else {
                    continue;
                };
                let (_, gh) = p.kind.grid();
                let ph = if even {
                    even_h.max(40.0)
                } else {
                    f64::from(gh) * row_u + f64::from(gh - 1) * gap
                };
                panels.push(PanelScene {
                    id: *pid,
                    rect: Rect {
                        x,
                        y,
                        w: cw,
                        h: ph,
                    },
                });
                y += ph + gap;
            }
            x += cw + gap;
        }
        (panels, x)
    }

    /// Scrolls the camera the minimal amount that makes the focused panel
    /// fully visible, with one gap of margin. Called after mutations — never
    /// while the user pans, which must stay free.
    pub fn ensure_focus_visible(&mut self, viewport: (f64, f64), opts: LayoutOpts) {
        let (panels, _) = self.layout_panels(viewport, opts);
        if let Some(f) = self.focus {
            if let Some(ps) = panels.iter().find(|p| p.id == f) {
                let lo = ps.rect.x - opts.gap;
                let hi = ps.rect.right() + opts.gap - viewport.0;
                self.camera_x = self.camera_x.clamp(hi.min(lo), lo);
            }
        }
    }

    /// Computes the discrete layout targets for a viewport. The camera is
    /// clamped to the strip's bounds only; focus-following is
    /// [`Ws::ensure_focus_visible`]'s job.
    pub fn scene(&mut self, viewport: (f64, f64), opts: LayoutOpts) -> Scene {
        self.normalize();
        let (panels, strip_w) = self.layout_panels(viewport, opts);
        let max_cam = (strip_w - viewport.0).max(0.0);
        self.camera_x = self.camera_x.clamp(0.0, max_cam);

        let bridges = self
            .joins
            .iter()
            .map(|(&a, &b)| (a, b))
            .filter(|&(a, b)| self.panels.contains_key(&a) && self.panels.contains_key(&b))
            .collect();

        Scene {
            camera_x: self.camera_x,
            panels,
            bridges,
            focus: self.focus,
        }
    }

    /// Pans the camera by `dx` points (trackpad), clamped by the next
    /// `scene()` call.
    pub fn pan(&mut self, dx: f64) {
        self.camera_x += dx;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VP: (f64, f64) = (1440.0, 900.0);

    fn opts() -> LayoutOpts {
        LayoutOpts { gap: 8.0 }
    }

    fn kinds(ws: &Ws) -> Vec<Vec<&'static str>> {
        ws.columns
            .iter()
            .map(|c| {
                c.panels
                    .iter()
                    .map(|pid| match ws.panels[pid].kind {
                        Kind::Help => "help",
                        Kind::About => "about",
                        Kind::Inbox { .. } => "inbox",
                        Kind::Message { .. } => "msg",
                        Kind::Contact { .. } => "contact",
                        Kind::Compose { .. } => "compose",
                    })
                    .collect()
            })
            .collect()
    }

    fn boot() -> (Ws, PanelId, PanelId) {
        let mut ws = Ws::new();
        let help = ws.open(Kind::Help, None, false);
        let inbox = ws.open(Kind::Inbox { filter: None }, None, false);
        ws.focus = Some(inbox);
        (ws, help, inbox)
    }

    /// The full web-prototype smoke scenario, transcribed.
    #[test]
    fn smoke_scenario() {
        let (mut ws, _help, inbox) = boot();
        assert_eq!(kinds(&ws), [vec!["help"], vec!["inbox"]]);

        // Open m1 from the inbox: new column right, joined.
        let msg = ws.follow_open(inbox, Kind::Message { id: "m1" }, false);
        assert_eq!(kinds(&ws), [vec!["help"], vec!["inbox"], vec!["msg"]]);
        assert_eq!(ws.joined_child(inbox), Some(msg));

        // Open m2: must replace the joined panel, not open another.
        let msg2 = ws.follow_open(inbox, Kind::Message { id: "m2" }, false);
        assert_eq!(msg2, msg);
        assert_eq!(ws.panels[&msg].kind, Kind::Message { id: "m2" });
        assert_eq!(kinds(&ws), [vec!["help"], vec!["inbox"], vec!["msg"]]);

        // Contact from the message: a joined chain.
        let contact = ws.follow_open(msg, Kind::Contact { email: "e" }, false);
        assert_eq!(ws.joined_child(msg), Some(contact));
        assert_eq!(
            kinds(&ws),
            [vec!["help"], vec!["inbox"], vec!["msg"], vec!["contact"]]
        );

        // Open m3 from the inbox: replaces joined AND cascade-closes contact.
        ws.follow_open(inbox, Kind::Message { id: "m3" }, false);
        assert_eq!(kinds(&ws), [vec!["help"], vec!["inbox"], vec!["msg"]]);
        assert!(ws.joined_child(msg).is_none());

        // Contact again, then a dotted replace on the message: cascade again.
        ws.follow_open(msg, Kind::Contact { email: "e2" }, false);
        ws.follow_replace(msg, Kind::Message { id: "m4" }, false);
        assert_eq!(kinds(&ws), [vec!["help"], vec!["inbox"], vec!["msg"]]);

        // Move the INBOX left: pair no longer adjacent → join must drop.
        ws.move_panel(inbox, Dir::Left);
        assert_eq!(kinds(&ws), [vec!["inbox"], vec!["help"], vec!["msg"]]);
        assert!(ws.joins.is_empty());

        // No join left → the next open must create a NEW joined panel right of
        // the inbox, not touch the far-away message.
        let m5 = ws.follow_open(inbox, Kind::Message { id: "m5" }, false);
        assert_ne!(m5, msg);
        assert_eq!(
            kinds(&ws),
            [vec!["inbox"], vec!["msg"], vec!["help"], vec!["msg"]]
        );
        assert_eq!(ws.joined_child(inbox), Some(m5));

        // Alt-open m6: separate panel; it stacks into the joined child's
        // column (3+3 rows fit) and the join must survive.
        ws.follow_open(inbox, Kind::Message { id: "m6" }, true);
        assert_eq!(
            kinds(&ws),
            [vec!["inbox"], vec!["msg", "msg"], vec!["help"], vec!["msg"]]
        );
        assert_eq!(ws.joined_child(inbox), Some(m5));

        // Closing the joined child drops its join.
        ws.close(m5);
        assert!(ws.joined_child(inbox).is_none());
    }

    #[test]
    fn literal_heights_leave_empty_space() {
        let mut ws = Ws::new();
        let inbox = ws.open(Kind::Inbox { filter: None }, None, false);
        let msg = ws.follow_open(inbox, Kind::Message { id: "m1" }, false);
        let scene = ws.scene(VP, opts());
        let inbox_r = scene.panels.iter().find(|p| p.id == inbox).unwrap().rect;
        let msg_r = scene.panels.iter().find(|p| p.id == msg).unwrap().rect;
        // Inbox requests 6 rows, message 3: the message is about half as tall.
        assert!((msg_r.h / inbox_r.h - 0.5).abs() < 0.02);
    }

    #[test]
    fn camera_follows_focus() {
        let mut ws = Ws::new();
        let mut last = ws.open(Kind::Help, None, false);
        for i in 0..4 {
            let id: MailId = ["m1", "m2", "m3", "m4"][i];
            last = ws.open(Kind::Message { id }, Some(last), false);
        }
        ws.ensure_focus_visible(VP, opts());
        let scene = ws.scene(VP, opts());
        let f = scene
            .panels
            .iter()
            .find(|p| Some(p.id) == scene.focus)
            .unwrap();
        assert!(f.rect.x - scene.camera_x >= 0.0);
        assert!(f.rect.right() - scene.camera_x <= VP.0);
    }

    #[test]
    fn focus_dir_walks_columns_geometrically() {
        let (mut ws, help, inbox) = boot();
        ws.focus_dir(Dir::Left, VP, opts());
        assert_eq!(ws.focus, Some(help));
        ws.focus_dir(Dir::Right, VP, opts());
        assert_eq!(ws.focus, Some(inbox));
        ws.focus_dir(Dir::Right, VP, opts()); // at the edge: stays
        assert_eq!(ws.focus, Some(inbox));
    }

    #[test]
    fn close_moves_focus_to_neighbour() {
        let (mut ws, help, inbox) = boot();
        ws.close(inbox);
        assert_eq!(ws.focus, Some(help));
        assert_eq!(kinds(&ws), [vec!["help"]]);
    }

    /// niri's bracket binds: alone → consume into the neighbour; stacked →
    /// expel into a fresh column.
    #[test]
    fn consume_or_expel_round_trip() {
        let (mut ws, _help, inbox) = boot();
        let msg = ws.follow_open(inbox, Kind::Message { id: "m1" }, false);
        // msg is alone right of the inbox: cmd+[ consumes it into the inbox
        // column, at the bottom.
        ws.consume_or_expel(msg, Dir::Left);
        assert_eq!(kinds(&ws), [vec!["help"], vec!["inbox", "msg"]]);
        // Consuming broke column adjacency, so the join died with it.
        assert!(ws.joins.is_empty());
        // Stacked now: cmd+] expels it back out to the right.
        ws.consume_or_expel(msg, Dir::Right);
        assert_eq!(kinds(&ws), [vec!["help"], vec!["inbox"], vec!["msg"]]);
    }

    #[test]
    fn consume_from_right_and_expel_bottom() {
        let (mut ws, _help, inbox) = boot();
        ws.follow_open(inbox, Kind::Message { id: "m1" }, false);
        // cmd+, pulls the message into the inbox column.
        ws.consume_from_right(inbox);
        assert_eq!(kinds(&ws), [vec!["help"], vec!["inbox", "msg"]]);
        // cmd+. pushes the bottom panel back out.
        ws.expel_bottom(inbox);
        assert_eq!(kinds(&ws), [vec!["help"], vec!["inbox"], vec!["msg"]]);
    }

    /// A column asked to hold more grid rows than exist distributes its
    /// height evenly instead of overflowing.
    #[test]
    fn overfull_column_distributes_evenly() {
        let (mut ws, _help, inbox) = boot();
        let msg = ws.follow_open(inbox, Kind::Message { id: "m1" }, false);
        ws.consume_or_expel(msg, Dir::Left); // inbox(6) + msg(3) = 9 > 6
        let scene = ws.scene(VP, opts());
        let inbox_r = scene.panels.iter().find(|p| p.id == inbox).unwrap().rect;
        let msg_r = scene.panels.iter().find(|p| p.id == msg).unwrap().rect;
        assert!((inbox_r.h - msg_r.h).abs() < 0.01, "even split");
        // And a fitting column still honours requests (help alone: 6 rows).
    }

    /// Tabbed columns lay out only the active panel, and left/right focus
    /// enters them on it.
    #[test]
    fn tabbed_column_shows_active_only() {
        let (mut ws, help, inbox) = boot();
        let msg = ws.follow_open(inbox, Kind::Message { id: "m1" }, false);
        ws.consume_or_expel(msg, Dir::Left); // [help][inbox+msg], focus msg
        ws.toggle_tabbed(msg);
        let scene = ws.scene(VP, opts());
        // Only the active tab (msg, focused) is in the scene.
        assert!(scene.panels.iter().any(|p| p.id == msg));
        assert!(!scene.panels.iter().any(|p| p.id == inbox));
        // Up switches tabs; the scene follows.
        ws.focus_dir(Dir::Up, VP, opts());
        assert_eq!(ws.focus, Some(inbox));
        let scene = ws.scene(VP, opts());
        assert!(scene.panels.iter().any(|p| p.id == inbox));
        assert!(!scene.panels.iter().any(|p| p.id == msg));
        // Entering from the left lands on the active tab, not "nearest".
        ws.focus = Some(help);
        ws.focus_dir(Dir::Right, VP, opts());
        assert_eq!(ws.focus, Some(inbox));
    }
}
