//! Pure panel/column/join state machine + layout targets. No rendering, no I/O,
//! no makepad — mirrors mosaic's `wm-core` division of labour: this module owns
//! *what* the layout is; the shell owns *how it gets there* (springs).
//!
//! The model is the one the web prototype (`web/`) validated:
//! - a **panel** is kind + params and requests grid units on the workspace
//!   grid (12×6 on desktop; the android shell switches 8×4 ⇄ 4×3 with the
//!   fold); requests clamp to the active grid, and heights are honoured
//!   literally — unused rows stay empty;
//! - solid links **open joined**, dotted links **replace in place**, buttons
//!   are side effects (links live in panel content, i.e. the shell);
//! - a **join** is alive only while the child sits in the column immediately
//!   right of its parent; the next open from the parent replaces the joined
//!   child; **replacing a panel closes its joined chain**.

use std::collections::HashMap;

/// A mail's identity: its row id in the store.
pub type MailId = i64;

/// Stable panel identity.
pub type PanelId = u64;

/// The workspace grid: how many unit columns and rows the viewport is cut
/// into. A target picks its grid from screen size — desktop 12×6, a folded
/// phone as little as 4×3 — and may switch at runtime (fold/unfold).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grid {
    /// Unit columns across the viewport.
    pub w: u32,
    /// Unit rows down the viewport.
    pub h: u32,
}

impl Default for Grid {
    fn default() -> Self {
        Self { w: 12, h: 6 }
    }
}

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
        filter: Option<String>,
    },
    /// One mail.
    Message {
        /// Which mail.
        id: MailId,
    },
    /// A sender's card.
    Contact {
        /// The sender's address.
        email: String,
    },
    /// A reply draft.
    Compose {
        /// The mail being replied to.
        re: MailId,
    },
    /// Accounts and their sync state; the add-account form.
    Settings,
}

impl Kind {
    /// Requested grid size, width × height — a wish, clamped to the active
    /// [`Grid`] by the workspace (an inbox asking 4×6 fills a 4×3 screen).
    #[must_use]
    pub fn grid(&self) -> (u32, u32) {
        match self {
            Kind::Help => (4, 6),
            Kind::About => (3, 2),
            Kind::Inbox { .. } => (4, 6),
            Kind::Message { .. } => (4, 3),
            Kind::Contact { .. } => (3, 2),
            Kind::Compose { .. } => (4, 4),
            Kind::Settings => (4, 5),
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

/// Where a dragged panel would land if dropped (see [`Ws::drop_target`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropTarget {
    /// Stack into column `col` before its `row`-th panel (the dragged panel
    /// itself excluded from the count).
    Into {
        /// Target column index in the current layout.
        col: usize,
        /// Insertion row among the column's other panels.
        row: usize,
    },
    /// A fresh column at boundary `at` (`0..=columns.len()`).
    Boundary {
        /// Boundary index: before column `at`.
        at: usize,
    },
}

/// One panel's discrete layout target.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PanelScene {
    /// The panel.
    pub id: PanelId,
    /// Target rect in strip coordinates.
    pub rect: Rect,
    /// Whether the panel is shown. Hidden tabs of a tabbed column keep their
    /// target rect (the column's full tabbed rect) so a tab switch is a pure
    /// in-place crossfade — never an open/close animation.
    pub visible: bool,
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
    /// The active grid. Swapped by [`Ws::set_grid`] when the screen changes.
    pub grid: Grid,
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

    /// Switches the layout grid (fold/unfold, desktop ⇄ phone). Targets are
    /// recomputed on the next [`Ws::scene`]; the shell springs there.
    pub fn set_grid(&mut self, grid: Grid) {
        self.grid = grid;
    }

    /// A panel's requested grid size, clamped to the active grid.
    fn panel_grid(&self, pid: PanelId) -> (u32, u32) {
        let (w, h) = self
            .panels
            .get(&pid)
            .map(|p| p.kind.grid())
            .unwrap_or((1, 1));
        (w.min(self.grid.w), h.min(self.grid.h))
    }

    /// Sum of requested (clamped) grid rows in a column.
    #[must_use]
    pub fn col_used_h(&self, col: &Column) -> u32 {
        col.panels.iter().map(|&pid| self.panel_grid(pid).1).sum()
    }

    /// Requested (clamped) grid width of a column: its widest panel.
    #[must_use]
    pub fn col_w(&self, col: &Column) -> u32 {
        col.panels
            .iter()
            .map(|&pid| self.panel_grid(pid).0)
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
        let h = self.panel_grid(pid).1;
        if let Some(right) = self.columns.get(from_col + 1) {
            if self.col_used_h(right) + h <= self.grid.h {
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
        self.detach(pid);
    }

    /// Detaches a panel — layout, joins, focus fallback — and hands it back.
    /// [`Ws::close`] is detach-and-drop; a workspace move re-homes the panel.
    pub fn detach(&mut self, pid: PanelId) -> Option<Panel> {
        let (c, r) = self.locate(pid)?;
        self.remove_from_layout(pid);
        let panel = self.panels.remove(&pid);
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
        panel
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

    /// Resolves what dropping the dragged panel at a strip point would do,
    /// judged by the *finger* point (not the panel), plus the insertion bar
    /// to preview it, in strip coordinates: a horizontal bar across a column
    /// (stack at that row) or a vertical bar in a gap (a fresh column
    /// there). `None` over the panel's own lone column: the drop goes home.
    #[must_use]
    pub fn drop_target(
        &self,
        pid: PanelId,
        x: f64,
        y: f64,
        viewport: (f64, f64),
        opts: LayoutOpts,
    ) -> Option<(DropTarget, Rect)> {
        let (ranges, strip_end) = self.col_ranges(viewport, opts);
        let gap = opts.gap;
        let into = ranges
            .iter()
            .position(|&(rx, rw)| x >= rx + 0.18 * rw && x <= rx + 0.82 * rw);
        match into {
            Some(tc) => {
                let rows: Vec<PanelId> = self.columns[tc]
                    .panels
                    .iter()
                    .copied()
                    .filter(|&p| p != pid)
                    .collect();
                if rows.is_empty() {
                    return None; // its own lone column
                }
                let (panels, _) = self.layout_panels(viewport, opts);
                let rect_of = |p: PanelId| {
                    panels
                        .iter()
                        .find(|ps| ps.id == p)
                        .map(|ps| ps.rect)
                        .unwrap_or_default()
                };
                let row = rows
                    .iter()
                    .filter(|&&p| {
                        let r = rect_of(p);
                        r.y + r.h / 2.0 < y
                    })
                    .count();
                let bar_y = if row < rows.len() {
                    rect_of(rows[row]).y - gap / 2.0
                } else {
                    rect_of(rows[rows.len() - 1]).bottom() + gap / 2.0
                };
                let (rx, rw) = ranges[tc];
                Some((
                    DropTarget::Into { col: tc, row },
                    Rect {
                        x: rx,
                        y: bar_y - 1.5,
                        w: rw,
                        h: 3.0,
                    },
                ))
            }
            None => {
                // The nearest boundary: each column's left edge, plus one
                // past the end. The bar sits centred in that boundary's gap.
                let mut at = self.columns.len();
                let mut bd = (x - strip_end).abs();
                let mut bx = strip_end;
                for (j, &(rx, _)) in ranges.iter().enumerate() {
                    let d = (x - rx).abs();
                    if d < bd {
                        bd = d;
                        at = j;
                        bx = rx;
                    }
                }
                Some((
                    DropTarget::Boundary { at },
                    Rect {
                        x: bx - gap / 2.0 - 1.5,
                        y: gap,
                        w: 3.0,
                        h: (viewport.1 - 2.0 * gap).max(0.0),
                    },
                ))
            }
        }
    }

    /// Drops a dragged panel at a strip point — the mutation half of
    /// [`Ws::drop_target`].
    pub fn place_at(
        &mut self,
        pid: PanelId,
        x: f64,
        y: f64,
        viewport: (f64, f64),
        opts: LayoutOpts,
    ) {
        if self.locate(pid).is_none() {
            return;
        }
        let Some((target, _)) = self.drop_target(pid, x, y, viewport, opts) else {
            self.focus = Some(pid);
            return; // its own lone column: stays put
        };
        match target {
            DropTarget::Into { col, row } => {
                // Anchored on a panel id — indices shift when the source
                // column empties out.
                let anchor = self.columns[col].panels.iter().copied().find(|&p| p != pid);
                let Some(anchor) = anchor else {
                    return;
                };
                self.remove_from_layout(pid);
                if let Some((c2, _)) = self.locate(anchor) {
                    let at = row.min(self.columns[c2].panels.len());
                    self.columns[c2].panels.insert(at, pid);
                }
            }
            DropTarget::Boundary { at } => {
                let anchor = self
                    .columns
                    .get(at)
                    .and_then(|c| c.panels.iter().copied().find(|&p| p != pid));
                self.remove_from_layout(pid);
                let ins = match anchor {
                    Some(a) => self
                        .locate(a)
                        .map(|(c, _)| c)
                        .unwrap_or(self.columns.len()),
                    None => at,
                };
                self.columns.insert(ins.min(self.columns.len()), Column::of(pid));
            }
        }
        self.validate_joins();
        self.normalize();
        self.focus = Some(pid);
    }

    /// Each column's `(x, width)` on the strip plus the strip's total width —
    /// exactly as [`Ws::layout_panels`] walks them.
    fn col_ranges(&self, viewport: (f64, f64), opts: LayoutOpts) -> (Vec<(f64, f64)>, f64) {
        let gap = opts.gap;
        let unit_w = (viewport.0 - gap) / f64::from(self.grid.w);
        let mut ranges = Vec::new();
        let mut x = gap;
        for col in &self.columns {
            let cw = (unit_w * f64::from(self.col_w(col)) - gap).max(40.0);
            ranges.push((x, cw));
            x += cw + gap;
        }
        (ranges, x)
    }

    /// Magnetises a freely panned camera to the nearest column alignment —
    /// a column's left edge one gap in from the viewport's left, a column's
    /// right edge one gap in from its right, or a strip end. The pan itself
    /// stays free; the shell calls this when the fingers lift and springs
    /// towards the result.
    pub fn snap_camera(&mut self, viewport: (f64, f64), opts: LayoutOpts) {
        let (ranges, strip_w) = self.col_ranges(viewport, opts);
        let gap = opts.gap;
        let max_cam = (strip_w - viewport.0).max(0.0);
        let cur = self.camera_x.clamp(0.0, max_cam);
        let mut best = 0.0;
        let mut bd = f64::MAX;
        let mut consider = |c: f64| {
            let c = c.clamp(0.0, max_cam);
            let d = (cur - c).abs();
            if d < bd {
                bd = d;
                best = c;
            }
        };
        for &(rx, rw) in &ranges {
            consider(rx - gap);
            consider(rx + rw + gap - viewport.0);
        }
        self.camera_x = best;
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
        let unit_w = (vw - gap) / f64::from(self.grid.w);
        let row_u =
            (vh - 2.0 * gap - f64::from(self.grid.h - 1) * gap) / f64::from(self.grid.h);

        let mut panels = Vec::new();
        let mut x = gap;
        for col in &self.columns {
            let cw = (unit_w * f64::from(self.col_w(col)) - gap).max(40.0);
            if col.tabbed {
                // Tabbed: every panel targets the same full-height rect under
                // the strip; only the active one is visible.
                let active = col.active.min(col.panels.len().saturating_sub(1));
                let top = gap + crate::theme::TAB_H + crate::theme::TAB_GAP;
                let rect = Rect {
                    x,
                    y: top,
                    w: cw,
                    h: (vh - top - gap).max(40.0),
                };
                for (i, pid) in col.panels.iter().enumerate() {
                    panels.push(PanelScene {
                        id: *pid,
                        rect,
                        visible: i == active,
                    });
                }
                x += cw + gap;
                continue;
            }
            // Requested heights are honoured while they fit; a column asked to
            // hold more than the grid distributes its space evenly instead
            // (consume/expel deliberately over-fill columns).
            let n = col.panels.len();
            let even = self.col_used_h(col) > self.grid.h && n > 0;
            let even_h = (vh - 2.0 * gap - (n.saturating_sub(1)) as f64 * gap) / n.max(1) as f64;
            let mut y = gap;
            for pid in &col.panels {
                if !self.panels.contains_key(pid) {
                    continue;
                }
                let (_, gh) = self.panel_grid(*pid);
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
                    visible: true,
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

    /// Whether the workspace holds any panels.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.columns.is_empty()
    }
}

/// How many numbered workspaces exist — cmd+1 … cmd+9.
pub const WS_N: usize = 9;

/// The workspaces, niri/hyprland-style: nine numbered spaces, each a full
/// [`Ws`] (own columns, focus and camera — switching back restores both),
/// one active at a time. An empty workspace is just an empty slot; it costs
/// nothing and needs no creation step.
///
/// `Wm` derefs to the active workspace, so everything that mutates "the
/// workspace" (open, close, focus, drops) applies where the user is looking.
/// Panel ids are minted per workspace in disjoint ranges, which keeps them
/// unique across the whole set — the shell keys springs and per-panel ui
/// state by `PanelId` alone, even mid-move between workspaces.
#[derive(Debug, Clone)]
pub struct Wm {
    /// The workspaces, index = number − 1. Fixed [`WS_N`] entries.
    pub wss: Vec<Ws>,
    /// Index of the active workspace.
    pub active: usize,
}

impl Default for Wm {
    fn default() -> Self {
        Self::new()
    }
}

impl Wm {
    /// Nine empty workspaces, the first one active.
    #[must_use]
    pub fn new() -> Self {
        let wss = (0..WS_N)
            .map(|k| {
                let mut w = Ws::new();
                w.next_id = (k as u64) << 32;
                w
            })
            .collect();
        Wm { wss, active: 0 }
    }

    /// Switches to workspace `k`. Returns whether anything changed.
    pub fn switch(&mut self, k: usize) -> bool {
        if k >= WS_N || k == self.active {
            return false;
        }
        self.active = k;
        true
    }

    /// Moves the focused panel to workspace `k` as its own trailing column
    /// and follows it there (niri's default for move-to-workspace). The
    /// panel leaves its joins behind; focus in the old workspace falls to a
    /// neighbour, exactly as on close.
    pub fn send_focused_to(&mut self, k: usize) -> Option<PanelId> {
        if k >= WS_N || k == self.active {
            return None;
        }
        let pid = self.wss[self.active].focus?;
        let panel = self.wss[self.active].detach(pid)?;
        let ws = &mut self.wss[k];
        ws.panels.insert(pid, panel);
        ws.columns.push(Column::of(pid));
        ws.focus = Some(pid);
        ws.normalize();
        self.active = k;
        Some(pid)
    }

    /// A panel by id, wherever it lives.
    #[must_use]
    pub fn panel(&self, pid: PanelId) -> Option<&Panel> {
        self.wss.iter().find_map(|w| w.panels.get(&pid))
    }

    /// Which workspace holds a panel.
    #[must_use]
    pub fn ws_of(&self, pid: PanelId) -> Option<usize> {
        self.wss.iter().position(|w| w.panels.contains_key(&pid))
    }

    /// Focuses a panel wherever it lives, switching workspaces if needed
    /// (the launcher's "go to"). Returns the workspace it landed on.
    pub fn focus_panel(&mut self, pid: PanelId) -> Option<usize> {
        let k = self.ws_of(pid)?;
        self.active = k;
        self.wss[k].focus = Some(pid);
        Some(k)
    }

    /// The workspaces worth showing: every occupied one, the active one, and
    /// the first empty slot (the "fresh workspace" target) — what the macOS
    /// menubar and the touch overlay list.
    #[must_use]
    pub fn roster(&self) -> Vec<usize> {
        let mut v: Vec<usize> = (0..WS_N)
            .filter(|&k| k == self.active || !self.wss[k].is_empty())
            .collect();
        if let Some(empty) = (0..WS_N).find(|k| !v.contains(k)) {
            v.push(empty);
            v.sort_unstable();
        }
        v
    }

    /// Switches the layout grid on every workspace — a fold/unfold reshapes
    /// them all, not just the visible one.
    pub fn set_grid(&mut self, grid: Grid) {
        for w in &mut self.wss {
            w.set_grid(grid);
        }
    }

    /// The logical state worth keeping: what the store persists and boot
    /// restores. Ephemeral physics — cameras, grids — deliberately absent;
    /// the shell re-derives both.
    #[must_use]
    pub fn snapshot(&self) -> WmSnap {
        WmSnap {
            active: self.active,
            wss: self.wss.iter().map(Ws::snapshot).collect(),
        }
    }

    /// Rebuilds the whole set from a snapshot (boot restore). Id minting
    /// resumes above every id already used in each workspace's range —
    /// counted across *all* spaces, because a moved panel keeps its id but
    /// not its home.
    #[must_use]
    pub fn restore(snap: WmSnap) -> Self {
        let mut wm = Wm::new();
        wm.active = snap.active.min(WS_N - 1);
        let mut max_id = vec![0u64; WS_N];
        for ws in &snap.wss {
            for (id, _) in &ws.panels {
                let k = (id >> 32) as usize;
                if k < WS_N {
                    max_id[k] = max_id[k].max(*id);
                }
            }
        }
        for (k, s) in snap.wss.into_iter().take(WS_N).enumerate() {
            let w = &mut wm.wss[k];
            w.columns = s
                .columns
                .into_iter()
                .map(|(panels, tabbed, active)| Column {
                    panels,
                    tabbed,
                    active,
                })
                .collect();
            w.panels = s
                .panels
                .into_iter()
                .map(|(id, kind)| (id, Panel { id, kind }))
                .collect();
            w.joins = s.joins.into_iter().collect();
            w.focus = s.focus.filter(|f| w.panels.contains_key(f));
            w.next_id = w.next_id.max(max_id[k]);
            w.validate_joins();
            w.normalize();
        }
        wm
    }
}

/// One workspace's logical state, detached from behaviour (see
/// [`Wm::snapshot`]). Plain data: the store serializes it, tests round-trip
/// it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct WsSnap {
    /// Columns left to right: `(panel ids top to bottom, tabbed, active)`.
    pub columns: Vec<(Vec<PanelId>, bool, usize)>,
    /// Every panel, sorted by id.
    pub panels: Vec<(PanelId, Kind)>,
    /// Joins, `(parent, child)`, sorted.
    pub joins: Vec<(PanelId, PanelId)>,
    /// The focused panel.
    pub focus: Option<PanelId>,
}

/// The whole set's logical state (see [`Wm::snapshot`]).
#[derive(Debug, Clone, PartialEq)]
pub struct WmSnap {
    /// Index of the active workspace.
    pub active: usize,
    /// All [`WS_N`] workspaces, in order.
    pub wss: Vec<WsSnap>,
}

impl Ws {
    /// This workspace's logical state (see [`Wm::snapshot`]).
    #[must_use]
    pub fn snapshot(&self) -> WsSnap {
        let mut panels: Vec<(PanelId, Kind)> = self
            .panels
            .values()
            .map(|p| (p.id, p.kind.clone()))
            .collect();
        panels.sort_by_key(|(id, _)| *id);
        let mut joins: Vec<(PanelId, PanelId)> =
            self.joins.iter().map(|(&a, &b)| (a, b)).collect();
        joins.sort_unstable();
        WsSnap {
            columns: self
                .columns
                .iter()
                .map(|c| (c.panels.clone(), c.tabbed, c.active))
                .collect(),
            panels,
            joins,
            focus: self.focus,
        }
    }
}

impl std::ops::Deref for Wm {
    type Target = Ws;
    fn deref(&self) -> &Ws {
        &self.wss[self.active]
    }
}

impl std::ops::DerefMut for Wm {
    fn deref_mut(&mut self) -> &mut Ws {
        &mut self.wss[self.active]
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
                        Kind::Settings => "settings",
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
        let msg = ws.follow_open(inbox, Kind::Message { id: 1 }, false);
        assert_eq!(kinds(&ws), [vec!["help"], vec!["inbox"], vec!["msg"]]);
        assert_eq!(ws.joined_child(inbox), Some(msg));

        // Open m2: must replace the joined panel, not open another.
        let msg2 = ws.follow_open(inbox, Kind::Message { id: 2 }, false);
        assert_eq!(msg2, msg);
        assert_eq!(ws.panels[&msg].kind, Kind::Message { id: 2 });
        assert_eq!(kinds(&ws), [vec!["help"], vec!["inbox"], vec!["msg"]]);

        // Contact from the message: a joined chain.
        let contact = ws.follow_open(msg, Kind::Contact { email: "e".into() }, false);
        assert_eq!(ws.joined_child(msg), Some(contact));
        assert_eq!(
            kinds(&ws),
            [vec!["help"], vec!["inbox"], vec!["msg"], vec!["contact"]]
        );

        // Open m3 from the inbox: replaces joined AND cascade-closes contact.
        ws.follow_open(inbox, Kind::Message { id: 3 }, false);
        assert_eq!(kinds(&ws), [vec!["help"], vec!["inbox"], vec!["msg"]]);
        assert!(ws.joined_child(msg).is_none());

        // Contact again, then a dotted replace on the message: cascade again.
        ws.follow_open(msg, Kind::Contact { email: "e2".into() }, false);
        ws.follow_replace(msg, Kind::Message { id: 4 }, false);
        assert_eq!(kinds(&ws), [vec!["help"], vec!["inbox"], vec!["msg"]]);

        // Move the INBOX left: pair no longer adjacent → join must drop.
        ws.move_panel(inbox, Dir::Left);
        assert_eq!(kinds(&ws), [vec!["inbox"], vec!["help"], vec!["msg"]]);
        assert!(ws.joins.is_empty());

        // No join left → the next open must create a NEW joined panel right of
        // the inbox, not touch the far-away message.
        let m5 = ws.follow_open(inbox, Kind::Message { id: 5 }, false);
        assert_ne!(m5, msg);
        assert_eq!(
            kinds(&ws),
            [vec!["inbox"], vec!["msg"], vec!["help"], vec!["msg"]]
        );
        assert_eq!(ws.joined_child(inbox), Some(m5));

        // Alt-open m6: separate panel; it stacks into the joined child's
        // column (3+3 rows fit) and the join must survive.
        ws.follow_open(inbox, Kind::Message { id: 6 }, true);
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
        let msg = ws.follow_open(inbox, Kind::Message { id: 1 }, false);
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
        for id in 1..=4 {
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
        let msg = ws.follow_open(inbox, Kind::Message { id: 1 }, false);
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
        ws.follow_open(inbox, Kind::Message { id: 1 }, false);
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
        let msg = ws.follow_open(inbox, Kind::Message { id: 1 }, false);
        ws.consume_or_expel(msg, Dir::Left); // inbox(6) + msg(3) = 9 > 6
        let scene = ws.scene(VP, opts());
        let inbox_r = scene.panels.iter().find(|p| p.id == inbox).unwrap().rect;
        let msg_r = scene.panels.iter().find(|p| p.id == msg).unwrap().rect;
        assert!((inbox_r.h - msg_r.h).abs() < 0.01, "even split");
        // And a fitting column still honours requests (help alone: 6 rows).
    }

    /// On a small grid every oversized request clamps: the inbox (asking 4×6)
    /// fills a 4×3 screen exactly, and a second 4×3 panel cannot share its
    /// column.
    #[test]
    fn small_grid_clamps_requests() {
        let vp = (400.0, 700.0);
        let mut ws = Ws::new();
        ws.set_grid(Grid { w: 4, h: 3 });
        let inbox = ws.open(Kind::Inbox { filter: None }, None, false);
        let scene = ws.scene(vp, opts());
        let r = scene.panels.iter().find(|p| p.id == inbox).unwrap().rect;
        // 4 of 4 units wide, 3 of 3 rows tall: the whole viewport minus gaps.
        assert!((r.w - (vp.0 - 2.0 * 8.0)).abs() < 0.5, "full width, got {}", r.w);
        assert!((r.h - (vp.1 - 2.0 * 8.0)).abs() < 0.5, "full height, got {}", r.h);
        // A message (4×3 clamped) doesn't fit under it → its own column.
        let msg = ws.follow_open(inbox, Kind::Message { id: 1 }, false);
        assert_ne!(ws.locate(inbox).unwrap().0, ws.locate(msg).unwrap().0);
    }

    /// Fold/unfold: switching the grid relayouts the same workspace.
    #[test]
    fn set_grid_relayouts() {
        let vp = (840.0, 700.0);
        let mut ws = Ws::new();
        let inbox = ws.open(Kind::Inbox { filter: None }, None, false);
        ws.set_grid(Grid { w: 8, h: 4 });
        let scene = ws.scene(vp, opts());
        let r = scene.panels.iter().find(|p| p.id == inbox).unwrap().rect;
        // 4 of 8 units: about half the viewport.
        let unit = (vp.0 - 8.0) / 8.0;
        assert!((r.w - (unit * 4.0 - 8.0)).abs() < 0.5, "half width, got {}", r.w);
        ws.set_grid(Grid { w: 4, h: 3 });
        let scene = ws.scene(vp, opts());
        let r = scene.panels.iter().find(|p| p.id == inbox).unwrap().rect;
        assert!((r.w - (vp.0 - 2.0 * 8.0)).abs() < 0.5, "full width, got {}", r.w);
    }

    /// Touch drag-and-drop: a drop inside a column stacks by y; a drop in the
    /// space past the strip makes a fresh trailing column.
    #[test]
    fn place_at_stacks_and_inserts() {
        let (mut ws, help, inbox) = boot();
        // Drop help into the inbox column's middle, below the inbox's centre.
        let scene = ws.scene(VP, opts());
        let ir = scene.panels.iter().find(|p| p.id == inbox).unwrap().rect;
        ws.place_at(help, ir.x + ir.w / 2.0, ir.bottom() - 1.0, VP, opts());
        assert_eq!(kinds(&ws), [vec!["inbox", "help"]]);
        // Drop it far right of everything: a new trailing column.
        ws.place_at(help, VP.0 * 3.0, 10.0, VP, opts());
        assert_eq!(kinds(&ws), [vec!["inbox"], vec!["help"]]);
        // Drop it at the strip's left edge: first column again.
        ws.place_at(help, 0.0, 10.0, VP, opts());
        assert_eq!(kinds(&ws), [vec!["help"], vec!["inbox"]]);
        assert_eq!(ws.focus, Some(help));
    }

    /// The drop is judged by the finger: a point in the gap between columns
    /// previews (and lands) a fresh column there; a point inside a column
    /// previews the stacking row.
    #[test]
    fn drop_target_finds_gaps_and_rows() {
        let (mut ws, help, inbox) = boot();
        let scene = ws.scene(VP, opts());
        let hr = scene.panels.iter().find(|p| p.id == help).unwrap().rect;
        let ir = scene.panels.iter().find(|p| p.id == inbox).unwrap().rect;
        // The gap between the two columns: a boundary, bar centred in it.
        let gx = (hr.right() + ir.x) / 2.0;
        let (t, bar) = ws.drop_target(help, gx, 100.0, VP, opts()).unwrap();
        assert_eq!(t, DropTarget::Boundary { at: 1 });
        assert!((bar.x + bar.w / 2.0 - gx).abs() < 1.0, "bar centred in the gap");
        assert!(bar.w < bar.h, "vertical bar");
        // Inside the inbox column, below its centre: stack after it.
        let (t, bar) = ws
            .drop_target(help, ir.x + ir.w / 2.0, ir.bottom() - 1.0, VP, opts())
            .unwrap();
        assert_eq!(t, DropTarget::Into { col: 1, row: 1 });
        assert!(bar.w > bar.h, "horizontal bar");
        // Over its own lone column: no target, the drop goes home.
        assert!(ws
            .drop_target(help, hr.x + hr.w / 2.0, 100.0, VP, opts())
            .is_none());
    }

    /// A released two-finger pan magnetises the camera to the nearest column
    /// alignment; the pan itself stays free.
    #[test]
    fn snap_camera_aligns_to_columns() {
        let mut ws = Ws::new();
        let mut last = ws.open(Kind::Help, None, false);
        for id in 1..=3 {
            last = ws.open(Kind::Message { id }, Some(last), false);
        }
        // Four 4-unit columns on a 12-unit grid: one column of overflow.
        let unit = (VP.0 - 8.0) / 12.0;
        let col2 = unit * 4.0; // camera with column 2's left edge at the left gap
        ws.camera_x = 100.0;
        ws.snap_camera(VP, opts());
        assert!((ws.camera_x - 0.0).abs() < 0.5, "close to home snaps home, got {}", ws.camera_x);
        ws.camera_x = 400.0;
        ws.snap_camera(VP, opts());
        assert!(
            (ws.camera_x - col2).abs() < 0.5,
            "snaps to column 2, got {} want {col2}",
            ws.camera_x
        );
    }

    /// Tabbed columns lay out only the active panel, and left/right focus
    /// enters them on it.
    #[test]
    fn tabbed_column_shows_active_only() {
        let (mut ws, help, inbox) = boot();
        let msg = ws.follow_open(inbox, Kind::Message { id: 1 }, false);
        ws.consume_or_expel(msg, Dir::Left); // [help][inbox+msg], focus msg
        ws.toggle_tabbed(msg);
        let scene = ws.scene(VP, opts());
        // Every tab is in the scene at the SAME rect (a switch must be a pure
        // crossfade, no movement); only the active one is visible.
        let msg_s = scene.panels.iter().find(|p| p.id == msg).unwrap();
        let inbox_s = scene.panels.iter().find(|p| p.id == inbox).unwrap();
        assert!(msg_s.visible);
        assert!(!inbox_s.visible);
        assert_eq!(msg_s.rect, inbox_s.rect);
        // Up switches tabs; visibility follows, rects stay put.
        ws.focus_dir(Dir::Up, VP, opts());
        assert_eq!(ws.focus, Some(inbox));
        let scene = ws.scene(VP, opts());
        assert!(scene.panels.iter().find(|p| p.id == inbox).unwrap().visible);
        assert!(!scene.panels.iter().find(|p| p.id == msg).unwrap().visible);
        // Entering from the left lands on the active tab, not "nearest".
        ws.focus = Some(help);
        ws.focus_dir(Dir::Right, VP, opts());
        assert_eq!(ws.focus, Some(inbox));
    }

    /// Workspaces: switching remembers focus and camera per space; a move
    /// re-homes the panel, follows it, and leaves old focus on a neighbour.
    #[test]
    fn workspaces_switch_and_move() {
        let mut wm = Wm::new();
        let help = wm.open(Kind::Help, None, false);
        let inbox = wm.open(Kind::Inbox { filter: None }, None, false);
        wm.focus = Some(inbox);
        wm.camera_x = 120.0;

        // Switch to an empty workspace and back: both are intact.
        assert!(wm.switch(1));
        assert!(wm.is_empty());
        assert_eq!(wm.focus, None);
        assert!(!wm.switch(1), "already there");
        wm.switch(0);
        assert_eq!(wm.focus, Some(inbox));
        assert_eq!(wm.camera_x, 120.0);

        // Move the focused panel to 3: it follows, its own trailing column.
        assert_eq!(wm.send_focused_to(3), Some(inbox));
        assert_eq!(wm.active, 3);
        assert_eq!(wm.focus, Some(inbox));
        assert_eq!(kinds(&wm.wss[3]), [vec!["inbox"]]);
        // The old workspace keeps help, focus fell to it.
        assert_eq!(kinds(&wm.wss[0]), [vec!["help"]]);
        assert_eq!(wm.wss[0].focus, Some(help));
        // A move to the active workspace is a no-op.
        assert_eq!(wm.send_focused_to(3), None);

        // Ids stay unique across workspaces (disjoint ranges per space).
        let about = wm.open(Kind::About, None, false);
        assert_ne!(about, help);
        assert!(wm.panel(help).is_some(), "cross-space lookup");

        // Roster: occupied 0 and 3, plus the first empty slot 1.
        assert_eq!(wm.roster(), vec![0, 1, 3]);
    }

    /// Snapshot → restore is lossless for the logical state, and id minting
    /// resumes above every restored id — even for a panel that moved into a
    /// foreign workspace's range.
    #[test]
    fn snapshot_restore_round_trips() {
        let mut wm = Wm::new();
        let inbox = wm.open(Kind::Inbox { filter: None }, None, false);
        let msg = wm.follow_open(inbox, Kind::Message { id: 1 }, false);
        wm.toggle_tabbed(msg);
        wm.send_focused_to(2); // msg (a ws-1 id) now lives on ws 3
        wm.switch(0);
        wm.focus = Some(inbox);

        let snap = wm.snapshot();
        let back = Wm::restore(snap.clone());
        assert_eq!(back.snapshot(), snap, "lossless round trip");
        assert_eq!(back.active, 0);
        assert_eq!(back.focus, Some(inbox));
        assert_eq!(back.wss[2].focus, Some(msg));

        // Fresh ids never collide with restored ones, in either space.
        let mut back = back;
        let a = back.open(Kind::About, None, false);
        assert!(a > inbox && a != msg);
        back.switch(2);
        let b = back.open(Kind::About, None, false);
        assert!(b != msg && b != a);

        // A stale focus (corrupt store) is dropped instead of trusted.
        let mut bad = snap;
        bad.wss[0].focus = Some(0xdead);
        assert_eq!(Wm::restore(bad).wss[0].focus, None);
    }

    /// Moving a joined child re-homes just the panel; the join dies with the
    /// adjacency. The grid applies to every workspace at once.
    #[test]
    fn workspace_move_breaks_joins_and_grid_is_global() {
        let mut wm = Wm::new();
        let inbox = wm.open(Kind::Inbox { filter: None }, None, false);
        let msg = wm.follow_open(inbox, Kind::Message { id: 1 }, false);
        assert_eq!(wm.joined_child(inbox), Some(msg));
        wm.send_focused_to(1);
        assert_eq!(wm.active, 1);
        assert!(wm.wss[0].joins.is_empty(), "join died with the move");
        assert_eq!(kinds(&wm.wss[1]), [vec!["msg"]]);

        wm.set_grid(Grid { w: 4, h: 3 });
        assert_eq!(wm.wss[0].grid, Grid { w: 4, h: 3 });
        assert_eq!(wm.wss[8].grid, Grid { w: 4, h: 3 });
    }
}
