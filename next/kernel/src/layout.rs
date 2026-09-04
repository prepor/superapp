//! Slot, column, join, and workspace state with no rendering or I/O.
//!
//! This module calculates target layouts. The Makepad shell animates toward
//! them. Panel sizes use grid units and are limited by the active screen grid.
//! A join exists only while its child remains in the next column. Replacing a
//! panel closes its joined descendants.
//!
//! A **slot** is a place in a column holding one panel instance; [`SlotId`] is
//! the number joins, focus and history refer to. What a slot *shows* is a
//! [`PanelId`], which this module compares, hashes and stores, and never
//! reads.

use std::collections::HashMap;

use crate::panel::PanelId;

/// Stable slot identity.
pub type SlotId = u64;

/// What a slot asks for when nothing has measured it: the size a panel with
/// no opinion gets, and the floor a restored slot draws at.
pub const DEFAULT_WISH: (u32, u32) = (4, 3);

/// The workspace grid: how many unit columns and rows the viewport is cut
/// into. A target picks its grid from screen size — desktop 12×6, a folded
/// phone as little as 4×3 — and may switch at runtime (fold/unfold).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grid {
    pub w: u32,
    pub h: u32,
}

impl Default for Grid {
    fn default() -> Self {
        Self { w: 12, h: 6 }
    }
}

/// One slot.
#[derive(Debug, Clone)]
pub struct Slot {
    pub id: SlotId,
    /// What it shows. Replacement swaps this in place, keeping the id.
    pub show: PanelId,
}

/// One column: slot ids, top to bottom, plus its display mode.
#[derive(Debug, Clone, Default)]
pub struct Column {
    pub slots: Vec<SlotId>,
    /// niri-style tabbed display: only the active slot shows, full height,
    /// under a tab strip.
    pub tabbed: bool,
    /// Which slot a tabbed column shows while unfocused. Kept in sync with
    /// focus by [`Ws::normalize`].
    pub active: usize,
}

impl Column {
    fn of(sid: SlotId) -> Self {
        Column {
            slots: vec![sid],
            ..Default::default()
        }
    }
}

/// A focus/move direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

/// A rectangle in strip coordinates, logical points.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    #[must_use]
    pub fn right(&self) -> f64 {
        self.x + self.w
    }
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
    /// Stack into column `col` before its `row`-th slot (the dragged slot
    /// itself excluded from the count).
    Into {
        /// Target column index in the current layout.
        col: usize,
        /// Insertion row among the column's other slots.
        row: usize,
    },
    /// A fresh column at boundary `at` (`0..=columns.len()`).
    Boundary {
        /// Boundary index: before column `at`.
        at: usize,
    },
}

/// One slot's discrete layout target.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SlotScene {
    /// The slot.
    pub id: SlotId,
    /// Target rect in strip coordinates.
    pub rect: Rect,
    /// Whether the slot is shown. Hidden tabs of a tabbed column keep their
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
    /// Every slot's target, in column order.
    pub slots: Vec<SlotScene>,
    /// Live joins, `(parent, child)`, both guaranteed present in `slots`.
    pub bridges: Vec<(SlotId, SlotId)>,
    /// The focused slot.
    pub focus: Option<SlotId>,
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

/// The workspace: every slot, their columns, joins and focus.
#[derive(Debug, Clone, Default)]
pub struct Ws {
    next_id: u64,
    /// The active grid. Swapped by [`Ws::set_grid`] when the screen changes.
    pub grid: Grid,
    /// Columns, left to right.
    pub columns: Vec<Column>,
    /// Slots by id.
    pub slots: HashMap<SlotId, Slot>,
    /// Joins, parent → child. At most one child per parent.
    pub joins: HashMap<SlotId, SlotId>,
    /// The focused slot.
    pub focus: Option<SlotId>,
    /// Camera x target in strip coordinates.
    pub camera_x: f64,
    /// Size wishes, keyed by what a slot shows. This module cannot measure
    /// a letter, so the session asks each instance ([`crate::panel::Panel::wish`])
    /// and leaves the answers here.
    ///
    /// Ephemeral physics, like the camera and the grid: never snapshotted,
    /// re-derived by the session whenever it relayouts.
    pub wishes: HashMap<PanelId, (u32, u32)>,
}

impl Ws {
    /// An empty workspace.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `(column index, row index)` of a slot.
    #[must_use]
    pub fn locate(&self, sid: SlotId) -> Option<(usize, usize)> {
        for (c, col) in self.columns.iter().enumerate() {
            if let Some(r) = col.slots.iter().position(|&p| p == sid) {
                return Some((c, r));
            }
        }
        None
    }

    fn col_of(&self, sid: SlotId) -> Option<usize> {
        self.locate(sid).map(|(c, _)| c)
    }

    /// Switches the layout grid (fold/unfold, desktop ⇄ phone). Targets are
    /// recomputed on the next [`Ws::scene`]; the shell springs there.
    pub fn set_grid(&mut self, grid: Grid) {
        self.grid = grid;
    }

    /// What a panel asks for: the wish its instance answered with if one
    /// was recorded, else [`DEFAULT_WISH`].
    #[must_use]
    pub fn wish_of(&self, id: &PanelId) -> (u32, u32) {
        self.wishes.get(id).copied().unwrap_or(DEFAULT_WISH)
    }

    /// Records one wish. The session calls this before opening a panel too
    /// — placement consults the wish, and a panel about to be born has no
    /// slot to hang one on yet.
    pub fn wish(&mut self, id: PanelId, size: (u32, u32)) {
        self.wishes.insert(id, size);
    }

    /// Replaces every wish at once — the session re-asks the instances each
    /// time it relayouts, so panels nothing shows any more drop out.
    pub fn set_wishes(&mut self, wishes: HashMap<PanelId, (u32, u32)>) {
        self.wishes = wishes;
    }

    /// A slot's requested grid size, clamped to the active grid.
    fn slot_grid(&self, sid: SlotId) -> (u32, u32) {
        let (w, h) = self
            .slots
            .get(&sid)
            .map(|p| self.wish_of(&p.show))
            .unwrap_or((1, 1));
        (w.min(self.grid.w), h.min(self.grid.h))
    }

    /// Sum of requested (clamped) grid rows in a column.
    #[must_use]
    pub fn col_used_h(&self, col: &Column) -> u32 {
        col.slots.iter().map(|&sid| self.slot_grid(sid).1).sum()
    }

    /// Requested (clamped) grid width of a column: its widest slot.
    #[must_use]
    pub fn col_w(&self, col: &Column) -> u32 {
        col.slots
            .iter()
            .map(|&sid| self.slot_grid(sid).0)
            .max()
            .unwrap_or(1)
    }

    /// The joined child of `sid`, if the join is alive.
    #[must_use]
    pub fn joined_child(&self, sid: SlotId) -> Option<SlotId> {
        self.joins.get(&sid).copied()
    }

    /// The parent `sid` is joined to, if any.
    #[must_use]
    pub fn join_parent_of(&self, sid: SlotId) -> Option<SlotId> {
        self.joins.iter().find(|&(_, &c)| c == sid).map(|(&p, _)| p)
    }

    /// A join is alive only while the child sits in the column immediately
    /// right of its parent — any move or insert that breaks adjacency breaks
    /// the join.
    fn validate_joins(&mut self) {
        let stale: Vec<SlotId> = self
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

    fn mk_slot(&mut self, show: PanelId) -> SlotId {
        self.next_id += 1;
        let id = self.next_id;
        self.slots.insert(id, Slot { id, show });
        id
    }

    fn remove_from_layout(&mut self, sid: SlotId) {
        if let Some((c, r)) = self.locate(sid) {
            self.columns[c].slots.remove(r);
            if self.columns[c].slots.is_empty() {
                self.columns.remove(c);
            }
        }
    }

    /// Replacing a panel closes everything joined to it, transitively — the
    /// chain to its right is context derived from content that just changed.
    fn close_joined_chain(&mut self, sid: SlotId) {
        if let Some(child) = self.joins.remove(&sid) {
            self.close_joined_chain(child);
            // Deepest first, and each one out through the same door as any
            // other close: [`Ws::detach`] is the one place focus falls to a
            // neighbour, and focus may well be sitting on the descendant
            // that is going. A focus left naming a slot that is gone draws
            // nothing and swallows every key aimed at it.
            self.detach(child);
            // A slot the columns never held — an odd restore — still leaves
            // the set; `detach` bails before it can take one.
            self.slots.remove(&child);
        }
    }

    /// "Open to the right": reuse the right-hand column if the new panel's
    /// rows fit, otherwise insert a fresh column. A joined child must land in
    /// the column immediately right of its parent (a join only lives there);
    /// an un-joined open respects an existing pair and goes after it instead.
    fn place_near(&mut self, sid: SlotId, from_col: usize, joined: bool) {
        let h = self.slot_grid(sid).1;
        if let Some(right) = self.columns.get(from_col + 1) {
            if self.col_used_h(right) + h <= self.grid.h {
                self.columns[from_col + 1].slots.push(sid);
                return;
            }
        }
        let mut at = from_col + 1;
        if !joined {
            let sources: Vec<SlotId> = self.columns[from_col].slots.clone();
            for src in sources {
                if let Some(child) = self.joins.get(&src) {
                    if self.col_of(*child) == Some(from_col + 1) {
                        at = from_col + 2;
                    }
                }
            }
        }
        let at = at.min(self.columns.len());
        self.columns.insert(at, Column::of(sid));
    }

    /// Opens a panel in a new slot. With `from`, it lands to the right of
    /// that slot's column; with `join`, it becomes that slot's joined child.
    /// Returns the new slot's id.
    pub fn open(&mut self, show: PanelId, from: Option<SlotId>, join: bool) -> SlotId {
        let sid = self.mk_slot(show);
        match from.and_then(|f| self.col_of(f)) {
            Some(c) => {
                self.place_near(sid, c, join);
                if join {
                    if let Some(f) = from {
                        self.joins.insert(f, sid);
                    }
                }
            }
            None => self.columns.push(Column::of(sid)),
        }
        self.validate_joins();
        self.focus = Some(sid);
        sid
    }

    /// Follows a solid link from `slot`: re-targets the existing joined child
    /// if one is alive, otherwise opens a new joined slot. `alt` always opens
    /// a fresh, un-joined slot. Returns the id of the slot that now shows
    /// `show`.
    pub fn follow_open(&mut self, slot: SlotId, show: PanelId, alt: bool) -> SlotId {
        if alt {
            return self.open(show, Some(slot), false);
        }
        match self.joined_child(slot) {
            Some(child) if self.slots.contains_key(&child) => {
                self.replace(child, show);
                child
            }
            _ => self.open(show, Some(slot), true),
        }
    }

    /// Follows a dotted link: replaces `slot` in place. `alt` opens a fresh,
    /// un-joined slot instead. Returns the id of the slot that now shows
    /// `show`.
    pub fn follow_replace(&mut self, slot: SlotId, show: PanelId, alt: bool) -> SlotId {
        if alt {
            return self.open(show, Some(slot), false);
        }
        self.replace(slot, show);
        slot
    }

    /// Replaces what a slot shows, in place (same id, same place), closing
    /// its joined chain first.
    pub fn replace(&mut self, sid: SlotId, show: PanelId) {
        if !self.slots.contains_key(&sid) {
            return;
        }
        self.close_joined_chain(sid);
        if let Some(p) = self.slots.get_mut(&sid) {
            p.show = show;
        }
        self.validate_joins();
        self.focus = Some(sid);
    }

    /// Closes a slot; focus falls to its nearest surviving neighbour, this
    /// slot's or a chained descendant's. This one workspace only:
    /// [`Wm::close`] is what everything outside the module says, and it
    /// finds the workspace first.
    pub fn close(&mut self, sid: SlotId) {
        // A join is one-way context: the child is what this panel pointed
        // at, so it goes with it, transitively — the same reason replacing
        // a panel closes its chain. Closing the inbox takes the message it
        // was previewing and the contact card that message opened; what
        // survives is what someone opened for its own sake.
        self.close_joined_chain(sid);
        self.detach(sid);
    }

    /// Detaches a slot — layout, joins, focus fallback — and hands it back.
    /// [`Ws::close`] is this plus its joined chain; a workspace move
    /// re-homes the slot and deliberately does **not** cascade: the panel
    /// travels, its joins stay behind and die with the lost adjacency.
    pub fn detach(&mut self, sid: SlotId) -> Option<Slot> {
        let (c, r) = self.locate(sid)?;
        self.remove_from_layout(sid);
        let slot = self.slots.remove(&sid);
        self.joins.retain(|&a, &mut b| a != sid && b != sid);
        self.validate_joins();
        if self.focus == Some(sid) {
            self.focus = None;
            if !self.columns.is_empty() {
                let c = c.min(self.columns.len() - 1);
                let col = &self.columns[c];
                let r = r.min(col.slots.len().saturating_sub(1));
                self.focus = col.slots.get(r).copied();
            }
        }
        slot
    }

    /// Raises `sid` to be its column's shown tab. [`Ws::normalize`] does this
    /// for the *focused* slot only, so a panel opened without focus — a
    /// preview — would otherwise land as a hidden tab and draw at
    /// alpha 0.
    pub fn activate(&mut self, sid: SlotId) {
        if let Some((c, r)) = self.locate(sid) {
            self.columns[c].active = r;
        }
    }

    /// Keeps per-column invariants: `active` clamped, and following focus.
    fn normalize(&mut self) {
        // A column with nothing in it is a gap on the strip, never a
        // place: whatever left it empty — a close, a move, a restore that
        // dropped a slot this build cannot read — it goes here, so no
        // path has to remember to prune.
        self.columns.retain(|c| !c.slots.is_empty());
        for col in &mut self.columns {
            col.active = col.active.min(col.slots.len().saturating_sub(1));
        }
        if let Some(f) = self.focus {
            if let Some((c, r)) = self.locate(f) {
                self.columns[c].active = r;
            }
        }
    }

    /// niri's `consume-or-expel-window-left/right`: a lone slot is consumed
    /// into the neighbouring column on that side (at its bottom); a stacked
    /// slot is expelled into a fresh column on that side.
    pub fn consume_or_expel(&mut self, sid: SlotId, dir: Dir) {
        let Some((c, r)) = self.locate(sid) else {
            return;
        };
        let d: isize = match dir {
            Dir::Left => -1,
            Dir::Right => 1,
            _ => return,
        };
        if self.columns[c].slots.len() == 1 {
            let t = c as isize + d;
            if t < 0 || t as usize >= self.columns.len() {
                return;
            }
            let t = t as usize;
            self.columns.remove(c);
            let t = if t > c { t - 1 } else { t };
            self.columns[t].slots.push(sid);
            self.columns[t].active = self.columns[t].slots.len() - 1;
        } else {
            self.columns[c].slots.remove(r);
            let at = if d < 0 { c } else { c + 1 };
            self.columns.insert(at, Column::of(sid));
        }
        self.validate_joins();
        self.normalize();
    }

    /// niri's `consume-window-into-column`: the first slot of the column to
    /// the right joins the bottom of `sid`'s column.
    pub fn consume_from_right(&mut self, sid: SlotId) {
        let Some((c, _)) = self.locate(sid) else {
            return;
        };
        if c + 1 >= self.columns.len() {
            return;
        }
        let moved = self.columns[c + 1].slots.remove(0);
        if self.columns[c + 1].slots.is_empty() {
            self.columns.remove(c + 1);
        }
        self.columns[c].slots.push(moved);
        self.validate_joins();
        self.normalize();
    }

    /// niri's `expel-window-from-column`: the bottom slot of `sid`'s column
    /// expels into a fresh column to the right.
    pub fn expel_bottom(&mut self, sid: SlotId) {
        let Some((c, _)) = self.locate(sid) else {
            return;
        };
        if self.columns[c].slots.len() < 2 {
            return;
        }
        let moved = self.columns[c].slots.pop().expect("len checked");
        self.columns.insert(c + 1, Column::of(moved));
        self.validate_joins();
        self.normalize();
    }

    /// niri's `toggle-column-tabbed-display` on the slot's column.
    pub fn toggle_tabbed(&mut self, sid: SlotId) {
        if let Some((c, _)) = self.locate(sid) {
            self.columns[c].tabbed = !self.columns[c].tabbed;
            self.normalize();
        }
    }

    /// Moves a slot one step. Within a column it swaps; across columns it
    /// merges into the neighbour (or swaps whole columns when it travels
    /// alone); at the edges a stacked slot expels into a fresh column.
    pub fn move_slot(&mut self, sid: SlotId, dir: Dir) {
        let Some((c, r)) = self.locate(sid) else {
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
                    if t < self.columns[c].slots.len() {
                        self.columns[c].slots.swap(r, t);
                    }
                }
            }
            Dir::Left | Dir::Right => {
                let d: isize = if dir == Dir::Left { -1 } else { 1 };
                let t = c as isize + d;
                if self.columns[c].slots.len() == 1 {
                    if t < 0 || t as usize >= self.columns.len() {
                        return; // lone column at the edge
                    }
                    self.columns.swap(c, t as usize);
                } else {
                    self.columns[c].slots.remove(r);
                    if t < 0 || t as usize >= self.columns.len() {
                        let at = if d < 0 { c } else { c + 1 };
                        self.columns.insert(at, Column::of(sid));
                    } else {
                        let tc = &mut self.columns[t as usize];
                        let at = r.min(tc.slots.len());
                        tc.slots.insert(at, sid);
                    }
                }
            }
        }
        self.validate_joins();
    }

    /// Resolves what dropping the dragged slot at a strip point would do,
    /// judged by the *finger* point (not the panel), plus the insertion bar
    /// to preview it, in strip coordinates: a horizontal bar across a column
    /// (stack at that row) or a vertical bar in a gap (a fresh column
    /// there). `None` over the slot's own lone column: the drop goes home.
    #[must_use]
    pub fn drop_target(
        &self,
        sid: SlotId,
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
                let rows: Vec<SlotId> = self.columns[tc]
                    .slots
                    .iter()
                    .copied()
                    .filter(|&p| p != sid)
                    .collect();
                if rows.is_empty() {
                    return None; // its own lone column
                }
                let (slots, _) = self.layout_slots(viewport, opts);
                let rect_of = |p: SlotId| {
                    slots
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

    /// Drops a dragged slot at a strip point — the mutation half of
    /// [`Ws::drop_target`].
    pub fn place_at(
        &mut self,
        sid: SlotId,
        x: f64,
        y: f64,
        viewport: (f64, f64),
        opts: LayoutOpts,
    ) {
        if self.locate(sid).is_none() {
            return;
        }
        let Some((target, _)) = self.drop_target(sid, x, y, viewport, opts) else {
            self.focus = Some(sid);
            return; // its own lone column: stays put
        };
        match target {
            DropTarget::Into { col, row } => {
                // Anchored on a slot id — indices shift when the source
                // column empties out.
                let anchor = self.columns[col].slots.iter().copied().find(|&p| p != sid);
                let Some(anchor) = anchor else {
                    return;
                };
                self.remove_from_layout(sid);
                if let Some((c2, _)) = self.locate(anchor) {
                    let at = row.min(self.columns[c2].slots.len());
                    self.columns[c2].slots.insert(at, sid);
                }
            }
            DropTarget::Boundary { at } => {
                let anchor = self
                    .columns
                    .get(at)
                    .and_then(|c| c.slots.iter().copied().find(|&p| p != sid));
                self.remove_from_layout(sid);
                let ins = match anchor {
                    Some(a) => self.locate(a).map(|(c, _)| c).unwrap_or(self.columns.len()),
                    None => at,
                };
                self.columns
                    .insert(ins.min(self.columns.len()), Column::of(sid));
            }
        }
        self.validate_joins();
        self.normalize();
        self.focus = Some(sid);
    }

    /// Each column's `(x, width)` on the strip plus the strip's total width —
    /// exactly as [`Ws::layout_slots`] walks them.
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
    /// slot with the nearest vertical centre in the neighbouring column,
    /// judged on the scene's target geometry.
    pub fn focus_dir(&mut self, dir: Dir, viewport: (f64, f64), opts: LayoutOpts) {
        let Some(cur) = self.focus else {
            self.focus = self.columns.first().and_then(|c| c.slots.first()).copied();
            return;
        };
        let Some((c, r)) = self.locate(cur) else {
            return;
        };
        match dir {
            Dir::Up => {
                if r > 0 {
                    self.focus = Some(self.columns[c].slots[r - 1]);
                }
            }
            Dir::Down => {
                if r + 1 < self.columns[c].slots.len() {
                    self.focus = Some(self.columns[c].slots[r + 1]);
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
                // slots have no geometry to be "nearest" by.
                if self.columns[t].tabbed {
                    let col = &self.columns[t];
                    if let Some(&sid) = col.slots.get(col.active.min(col.slots.len() - 1)) {
                        self.focus = Some(sid);
                        self.normalize();
                    }
                    return;
                }
                let scene = self.scene(viewport, opts);
                let rect_of = |sid: SlotId| {
                    scene
                        .slots
                        .iter()
                        .find(|p| p.id == sid)
                        .map(|p| p.rect)
                        .unwrap_or_default()
                };
                let cur_mid = {
                    let r = rect_of(cur);
                    r.y + r.h / 2.0
                };
                let best = self.columns[t]
                    .slots
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

    fn layout_slots(&self, viewport: (f64, f64), opts: LayoutOpts) -> (Vec<SlotScene>, f64) {
        let (vw, vh) = viewport;
        let gap = opts.gap;
        let unit_w = (vw - gap) / f64::from(self.grid.w);
        let row_u = (vh - 2.0 * gap - f64::from(self.grid.h - 1) * gap) / f64::from(self.grid.h);

        let mut slots = Vec::new();
        let mut x = gap;
        for col in &self.columns {
            let cw = (unit_w * f64::from(self.col_w(col)) - gap).max(40.0);
            if col.tabbed {
                // Tabbed: every slot targets the same full-height rect under
                // the strip; only the active one is visible.
                let active = col.active.min(col.slots.len().saturating_sub(1));
                let top = gap + crate::theme::TAB_H + crate::theme::TAB_GAP;
                let rect = Rect {
                    x,
                    y: top,
                    w: cw,
                    h: (vh - top - gap).max(40.0),
                };
                for (i, sid) in col.slots.iter().enumerate() {
                    slots.push(SlotScene {
                        id: *sid,
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
            let n = col.slots.len();
            let even = self.col_used_h(col) > self.grid.h && n > 0;
            let even_h = (vh - 2.0 * gap - (n.saturating_sub(1)) as f64 * gap) / n.max(1) as f64;
            let mut y = gap;
            for sid in &col.slots {
                if !self.slots.contains_key(sid) {
                    continue;
                }
                let (_, gh) = self.slot_grid(*sid);
                let ph = if even {
                    even_h.max(40.0)
                } else {
                    f64::from(gh) * row_u + f64::from(gh - 1) * gap
                };
                slots.push(SlotScene {
                    id: *sid,
                    rect: Rect { x, y, w: cw, h: ph },
                    visible: true,
                });
                y += ph + gap;
            }
            x += cw + gap;
        }
        (slots, x)
    }

    /// Scrolls the camera the minimal amount that makes `sid` fully visible,
    /// with one gap of margin. Called after mutations — never while the user
    /// pans, which must stay free.
    pub fn ensure_visible(&mut self, sid: SlotId, viewport: (f64, f64), opts: LayoutOpts) {
        let (slots, _) = self.layout_slots(viewport, opts);
        if let Some(ps) = slots.iter().find(|p| p.id == sid) {
            let lo = ps.rect.x - opts.gap;
            let hi = ps.rect.right() + opts.gap - viewport.0;
            self.camera_x = self.camera_x.clamp(hi.min(lo), lo);
        }
    }

    /// As [`Ws::ensure_visible`], for whatever holds focus.
    pub fn ensure_focus_visible(&mut self, viewport: (f64, f64), opts: LayoutOpts) {
        if let Some(f) = self.focus {
            self.ensure_visible(f, viewport, opts);
        }
    }

    /// Whether two slots can be on screen at the same time. A preview only
    /// makes sense where the pair fits — on a phone grid each of them is the
    /// whole screen, so an open that kept focus behind would just look like
    /// nothing happened.
    #[must_use]
    pub fn fit_together(
        &self,
        a: SlotId,
        b: SlotId,
        viewport: (f64, f64),
        opts: LayoutOpts,
    ) -> bool {
        let (slots, _) = self.layout_slots(viewport, opts);
        let find = |id| slots.iter().find(|p| p.id == id).map(|p| p.rect);
        match (find(a), find(b)) {
            (Some(ra), Some(rb)) => ra.right().max(rb.right()) - ra.x.min(rb.x) <= viewport.0,
            _ => false,
        }
    }

    /// Computes the discrete layout targets for a viewport. The camera is
    /// clamped to the strip's bounds only; focus-following is
    /// [`Ws::ensure_focus_visible`]'s job.
    pub fn scene(&mut self, viewport: (f64, f64), opts: LayoutOpts) -> Scene {
        self.normalize();
        let (slots, strip_w) = self.layout_slots(viewport, opts);
        let max_cam = (strip_w - viewport.0).max(0.0);
        self.camera_x = self.camera_x.clamp(0.0, max_cam);

        let bridges = self
            .joins
            .iter()
            .map(|(&a, &b)| (a, b))
            .filter(|&(a, b)| self.slots.contains_key(&a) && self.slots.contains_key(&b))
            .collect();

        Scene {
            camera_x: self.camera_x,
            slots,
            bridges,
            focus: self.focus,
        }
    }

    /// Pans the camera by `dx` points (trackpad), clamped by the next
    /// `scene()` call.
    pub fn pan(&mut self, dx: f64) {
        self.camera_x += dx;
    }

    /// Whether the workspace holds any slots.
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
/// Slot ids are minted per workspace in disjoint ranges, which keeps them
/// unique across the whole set — the shell keys springs and per-slot ui
/// state by `SlotId` alone, even mid-move between workspaces.
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

    /// Moves the focused slot to workspace `k` as its own trailing column
    /// and follows it there (niri's default for move-to-workspace). The
    /// slot leaves its joins behind; focus in the old workspace falls to a
    /// neighbour, exactly as on close.
    pub fn send_focused_to(&mut self, k: usize) -> Option<SlotId> {
        if k >= WS_N || k == self.active {
            return None;
        }
        let sid = self.wss[self.active].focus?;
        let slot = self.wss[self.active].detach(sid)?;
        let ws = &mut self.wss[k];
        ws.slots.insert(sid, slot);
        ws.columns.push(Column::of(sid));
        ws.focus = Some(sid);
        ws.normalize();
        self.active = k;
        Some(sid)
    }

    /// A slot by id, wherever it lives.
    #[must_use]
    pub fn slot(&self, sid: SlotId) -> Option<&Slot> {
        self.wss.iter().find_map(|w| w.slots.get(&sid))
    }

    /// Which workspace holds a slot.
    #[must_use]
    pub fn ws_of(&self, sid: SlotId) -> Option<usize> {
        self.wss.iter().position(|w| w.slots.contains_key(&sid))
    }

    /// Every slot showing exactly this identity, on any workspace. A mail
    /// that leaves the inbox closes its readers wherever they were opened,
    /// so this deliberately looks past the active workspace `Wm` derefs to.
    #[must_use]
    pub fn showing(&self, id: &PanelId) -> Vec<SlotId> {
        self.wss
            .iter()
            .flat_map(|w| w.slots.values())
            .filter(|p| p.show == *id)
            .map(|p| p.id)
            .collect()
    }

    /// Closes a slot with its joined chain (see [`Ws::close`]) on whichever
    /// workspace holds it. Deliberately shadows the [`Ws::close`] this
    /// derefs to, which only ever sees the active workspace and would be a
    /// silent no-op anywhere else: closing is one rule, so there is one
    /// spelling of it, and a verb whose action says `wm.close(slot)` is
    /// right wherever its panel was opened.
    pub fn close(&mut self, sid: SlotId) {
        if let Some(k) = self.ws_of(sid) {
            self.wss[k].close(sid);
        }
    }

    /// Focuses a slot wherever it lives, switching workspaces if needed
    /// (the launcher's "go to"). Returns the workspace it landed on.
    pub fn focus_slot(&mut self, sid: SlotId) -> Option<usize> {
        let k = self.ws_of(sid)?;
        self.active = k;
        self.wss[k].focus = Some(sid);
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

    /// Records a wish everywhere (see [`Ws::wish`]): what a mail asks for is
    /// a property of the mail, not of the space it happens to be read in.
    pub fn wish(&mut self, id: &PanelId, size: (u32, u32)) {
        for w in &mut self.wss {
            w.wish(id.clone(), size);
        }
    }

    /// Replaces every workspace's wishes (see [`Ws::set_wishes`]). Every
    /// space lays out on every relayout — the one being switched away from
    /// included — so they all need the same measurements.
    pub fn set_wishes(&mut self, wishes: HashMap<PanelId, (u32, u32)>) {
        for w in &mut self.wss {
            w.set_wishes(wishes.clone());
        }
    }

    /// The logical state worth keeping: what the store persists and boot
    /// restores. Ephemeral physics — cameras, grids, wishes — deliberately
    /// absent; the session re-derives all three.
    #[must_use]
    pub fn snapshot(&self) -> WmSnap {
        WmSnap {
            active: self.active,
            wss: self.wss.iter().map(Ws::snapshot).collect(),
        }
    }

    /// Rebuilds the whole set from a snapshot (boot restore). Id minting
    /// resumes above every id already used in each workspace's range —
    /// counted across *all* spaces, because a moved slot keeps its id but
    /// not its home.
    #[must_use]
    pub fn restore(snap: WmSnap) -> Self {
        let mut wm = Wm::new();
        wm.active = snap.active.min(WS_N - 1);
        let mut max_id = [0u64; WS_N];
        for ws in &snap.wss {
            for (id, _) in &ws.slots {
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
                .map(|(slots, tabbed, active)| Column {
                    slots,
                    tabbed,
                    active,
                })
                .collect();
            w.slots = s
                .slots
                .into_iter()
                .map(|(id, show)| (id, Slot { id, show }))
                .collect();
            w.joins = s.joins.into_iter().collect();
            w.focus = s.focus.filter(|f| w.slots.contains_key(f));
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
    /// Columns left to right: `(slot ids top to bottom, tabbed, active)`.
    pub columns: Vec<(Vec<SlotId>, bool, usize)>,
    /// Every slot and what it shows, sorted by id.
    pub slots: Vec<(SlotId, PanelId)>,
    /// Joins, `(parent, child)`, sorted.
    pub joins: Vec<(SlotId, SlotId)>,
    /// The focused slot.
    pub focus: Option<SlotId>,
}

/// The whole set's logical state (see [`Wm::snapshot`]).
#[derive(Debug, Clone, PartialEq)]
pub struct WmSnap {
    /// Index of the active workspace.
    pub active: usize,
    /// All [`WS_N`] workspaces, in order.
    pub wss: Vec<WsSnap>,
}

impl Default for WmSnap {
    /// An empty session: every workspace present and empty, active 0 — the
    /// shape `Wm::new().snapshot()` gives, without needing a [`Wm`].
    /// History's floor uses it once the tree has been trimmed away.
    fn default() -> Self {
        WmSnap {
            active: 0,
            wss: vec![WsSnap::default(); WS_N],
        }
    }
}

impl Ws {
    /// This workspace's logical state (see [`Wm::snapshot`]).
    #[must_use]
    pub fn snapshot(&self) -> WsSnap {
        let mut slots: Vec<(SlotId, PanelId)> = self
            .slots
            .values()
            .map(|p| (p.id, p.show.clone()))
            .collect();
        slots.sort_by_key(|(id, _)| *id);
        let mut joins: Vec<(SlotId, SlotId)> = self.joins.iter().map(|(&a, &b)| (a, b)).collect();
        joins.sort_unstable();
        WsSnap {
            columns: self
                .columns
                .iter()
                .map(|c| (c.slots.clone(), c.tabbed, c.active))
                .collect(),
            slots,
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
    use crate::panel::Tag;

    const VP: (f64, f64) = (1440.0, 900.0);

    fn opts() -> LayoutOpts {
        LayoutOpts { gap: 8.0 }
    }

    // Five fake kinds, standing in for whatever an app registers. The
    // kernel knows nothing about them but their tags.
    fn help() -> PanelId {
        PanelId::bare(Tag("help"))
    }
    fn about() -> PanelId {
        PanelId::bare(Tag("about"))
    }
    fn inbox() -> PanelId {
        PanelId::bare(Tag("inbox"))
    }
    fn msg(id: i64) -> PanelId {
        PanelId::new(Tag("msg"), [id.to_string()])
    }
    fn contact(email: &str) -> PanelId {
        PanelId::new(Tag("contact"), [email])
    }

    /// What each fake kind's instance would answer from `Panel::wish`. The
    /// session records these before it lays out; here the test helpers do.
    fn wish_of(id: &PanelId) -> (u32, u32) {
        match id.tag.as_str() {
            "help" | "inbox" => (4, 6),
            "about" | "contact" => (3, 2),
            _ => DEFAULT_WISH,
        }
    }

    /// [`Ws::open`] with the instance's wish recorded first, as the session
    /// does: placement consults the wish, so it has to be there by then.
    fn open(ws: &mut Ws, show: PanelId, from: Option<SlotId>, join: bool) -> SlotId {
        ws.wish(show.clone(), wish_of(&show));
        ws.open(show, from, join)
    }

    fn follow_open(ws: &mut Ws, slot: SlotId, show: PanelId, alt: bool) -> SlotId {
        ws.wish(show.clone(), wish_of(&show));
        ws.follow_open(slot, show, alt)
    }

    fn follow_replace(ws: &mut Ws, slot: SlotId, show: PanelId, alt: bool) -> SlotId {
        ws.wish(show.clone(), wish_of(&show));
        ws.follow_replace(slot, show, alt)
    }

    fn replace(ws: &mut Ws, slot: SlotId, show: PanelId) {
        ws.wish(show.clone(), wish_of(&show));
        ws.replace(slot, show);
    }

    /// The same over a whole set, so a slot that travels to another
    /// workspace still finds its wish there.
    fn wm_open(wm: &mut Wm, show: PanelId, from: Option<SlotId>, join: bool) -> SlotId {
        wm.wish(&show, wish_of(&show));
        wm.open(show, from, join)
    }

    fn wm_follow_open(wm: &mut Wm, slot: SlotId, show: PanelId) -> SlotId {
        wm.wish(&show, wish_of(&show));
        wm.follow_open(slot, show, false)
    }

    /// The strip as tags, column by column — what every layout test reads.
    fn tags(ws: &Ws) -> Vec<Vec<&'static str>> {
        ws.columns
            .iter()
            .map(|c| {
                c.slots
                    .iter()
                    .map(|sid| ws.slots[sid].show.tag.as_str())
                    .collect()
            })
            .collect()
    }

    fn boot() -> (Ws, SlotId, SlotId) {
        let mut ws = Ws::new();
        let h = open(&mut ws, help(), None, false);
        let i = open(&mut ws, inbox(), None, false);
        ws.focus = Some(i);
        (ws, h, i)
    }

    /// The full web-prototype smoke scenario, transcribed.
    #[test]
    fn smoke_scenario() {
        let (mut ws, _help, inbox_id) = boot();
        assert_eq!(tags(&ws), [vec!["help"], vec!["inbox"]]);

        // Open m1 from the inbox: new column right, joined.
        let m = follow_open(&mut ws, inbox_id, msg(1), false);
        assert_eq!(tags(&ws), [vec!["help"], vec!["inbox"], vec!["msg"]]);
        assert_eq!(ws.joined_child(inbox_id), Some(m));

        // Open m2: must replace the joined slot, not open another.
        let m2 = follow_open(&mut ws, inbox_id, msg(2), false);
        assert_eq!(m2, m);
        assert_eq!(ws.slots[&m].show, msg(2));
        assert_eq!(tags(&ws), [vec!["help"], vec!["inbox"], vec!["msg"]]);

        // Contact from the message: a joined chain.
        let c = follow_open(&mut ws, m, contact("e"), false);
        assert_eq!(ws.joined_child(m), Some(c));
        assert_eq!(
            tags(&ws),
            [vec!["help"], vec!["inbox"], vec!["msg"], vec!["contact"]]
        );

        // Open m3 from the inbox: replaces joined AND cascade-closes contact.
        follow_open(&mut ws, inbox_id, msg(3), false);
        assert_eq!(tags(&ws), [vec!["help"], vec!["inbox"], vec!["msg"]]);
        assert!(ws.joined_child(m).is_none());

        // Contact again, then a dotted replace on the message: cascade again.
        follow_open(&mut ws, m, contact("e2"), false);
        follow_replace(&mut ws, m, msg(4), false);
        assert_eq!(tags(&ws), [vec!["help"], vec!["inbox"], vec!["msg"]]);

        // Move the INBOX left: pair no longer adjacent → join must drop.
        ws.move_slot(inbox_id, Dir::Left);
        assert_eq!(tags(&ws), [vec!["inbox"], vec!["help"], vec!["msg"]]);
        assert!(ws.joins.is_empty());

        // No join left → the next open must create a NEW joined slot right of
        // the inbox, not touch the far-away message.
        let m5 = follow_open(&mut ws, inbox_id, msg(5), false);
        assert_ne!(m5, m);
        assert_eq!(
            tags(&ws),
            [vec!["inbox"], vec!["msg"], vec!["help"], vec!["msg"]]
        );
        assert_eq!(ws.joined_child(inbox_id), Some(m5));

        // Alt-open m6: separate slot; it stacks into the joined child's
        // column (3+3 rows fit) and the join must survive.
        follow_open(&mut ws, inbox_id, msg(6), true);
        assert_eq!(
            tags(&ws),
            [vec!["inbox"], vec!["msg", "msg"], vec!["help"], vec!["msg"]]
        );
        assert_eq!(ws.joined_child(inbox_id), Some(m5));

        // Closing the joined child drops its join.
        ws.close(m5);
        assert!(ws.joined_child(inbox_id).is_none());
    }

    #[test]
    fn literal_heights_leave_empty_space() {
        let mut ws = Ws::new();
        let i = open(&mut ws, inbox(), None, false);
        let m = follow_open(&mut ws, i, msg(1), false);
        let scene = ws.scene(VP, opts());
        let inbox_r = scene.slots.iter().find(|p| p.id == i).unwrap().rect;
        let msg_r = scene.slots.iter().find(|p| p.id == m).unwrap().rect;
        // Inbox requests 6 rows, message 3: the message is about half as tall.
        assert!((msg_r.h / inbox_r.h - 0.5).abs() < 0.02);
    }

    /// A wish measured from content overrides the default — a long letter
    /// takes the rows a short one leaves empty — and clamps to the grid like
    /// any other request.
    #[test]
    fn a_measured_wish_overrides_the_default() {
        let mut ws = Ws::new();
        let i = open(&mut ws, inbox(), None, false);
        ws.wish(msg(1), (4, 6));
        let m = ws.follow_open(i, msg(1), false);
        let tall = |ws: &mut Ws, sid| {
            let scene = ws.scene(VP, opts());
            scene.slots.iter().find(|p| p.id == sid).unwrap().rect.h
        };
        assert!(
            (tall(&mut ws, m) - tall(&mut ws, i)).abs() < 0.01,
            "six rows asked, six rows given"
        );
        // The wish is a wish: a folded screen clamps it like the inbox's.
        ws.set_grid(Grid { w: 4, h: 3 });
        assert!((tall(&mut ws, m) - tall(&mut ws, i)).abs() < 0.01);
        assert!(tall(&mut ws, m) < 900.0);
        // And the panel it was measured for is what carries the wish:
        // another letter in the same slot is back on the default.
        ws.set_grid(Grid::default());
        replace(&mut ws, m, msg(2));
        assert!((tall(&mut ws, m) / tall(&mut ws, i) - 0.5).abs() < 0.02);
    }

    /// Placement consults the wish: a letter that wants the whole column
    /// stops fitting under its neighbour and earns a column of its own.
    #[test]
    fn a_tall_letter_earns_its_own_column() {
        let mut ws = Ws::new();
        let i = open(&mut ws, inbox(), None, false);
        let c = open(&mut ws, contact("vera@kovac.io"), Some(i), false);
        // Short (3 rows) under the contact (2): 5 of 6 rows, it fits.
        let short = follow_open(&mut ws, i, msg(1), false);
        assert_eq!(ws.locate(short).unwrap().0, ws.locate(c).unwrap().0);
        ws.close(short);
        // Long (6 rows): it cannot share, so a column is inserted for it.
        ws.wish(msg(2), (4, 6));
        let long = ws.follow_open(i, msg(2), false);
        assert_ne!(ws.locate(long).unwrap().0, ws.locate(c).unwrap().0);
        assert_eq!(tags(&ws), [vec!["inbox"], vec!["msg"], vec!["contact"]]);
    }

    #[test]
    fn camera_follows_focus() {
        let mut ws = Ws::new();
        let mut last = open(&mut ws, help(), None, false);
        for id in 1..=4 {
            last = open(&mut ws, msg(id), Some(last), false);
        }
        ws.ensure_focus_visible(VP, opts());
        let scene = ws.scene(VP, opts());
        let f = scene
            .slots
            .iter()
            .find(|p| Some(p.id) == scene.focus)
            .unwrap();
        assert!(f.rect.x - scene.camera_x >= 0.0);
        assert!(f.rect.right() - scene.camera_x <= VP.0);
    }

    #[test]
    fn focus_dir_walks_columns_geometrically() {
        let (mut ws, help_id, inbox_id) = boot();
        ws.focus_dir(Dir::Left, VP, opts());
        assert_eq!(ws.focus, Some(help_id));
        ws.focus_dir(Dir::Right, VP, opts());
        assert_eq!(ws.focus, Some(inbox_id));
        ws.focus_dir(Dir::Right, VP, opts()); // at the edge: stays
        assert_eq!(ws.focus, Some(inbox_id));
    }

    #[test]
    fn close_moves_focus_to_neighbour() {
        let (mut ws, help_id, inbox_id) = boot();
        ws.close(inbox_id);
        assert_eq!(ws.focus, Some(help_id));
        assert_eq!(tags(&ws), [vec!["help"]]);
    }

    /// Closing a slot takes its joined chain with it, transitively — the
    /// child is context this panel pointed at, exactly as with a replace.
    /// A panel opened for its own sake is nobody's context and stays.
    #[test]
    fn close_takes_the_joined_chain() {
        let (mut ws, help_id, inbox_id) = boot();
        let m = follow_open(&mut ws, inbox_id, msg(1), false);
        let c = follow_open(&mut ws, m, contact("e"), false);
        // …and one un-joined slot, to show what survives.
        let a = open(&mut ws, about(), Some(c), false);
        assert_eq!(
            tags(&ws),
            [
                vec!["help"],
                vec!["inbox"],
                vec!["msg"],
                vec!["contact"],
                vec!["about"]
            ]
        );

        ws.close(inbox_id);
        assert_eq!(tags(&ws), [vec!["help"], vec!["about"]]);
        assert!(!ws.slots.contains_key(&m) && !ws.slots.contains_key(&c));
        assert!(ws.slots.contains_key(&a));
        assert!(ws.joins.is_empty());
        // Focus falls to the slot now standing where the closed one
        // did — the chain went with it, so that is `about`.
        assert_eq!(ws.focus, Some(a));
        let _ = help_id;
    }

    /// Focus sitting on a joined descendant when an ancestor closes goes
    /// with it, and falls to what is left standing — the same rule any
    /// other close follows. A focus naming a slot that is gone would draw
    /// nothing and swallow every key aimed at it.
    #[test]
    fn closing_a_chain_takes_the_focus_with_it() {
        let (mut ws, help_id, inbox_id) = boot();
        let m = follow_open(&mut ws, inbox_id, msg(1), false);
        let c = follow_open(&mut ws, m, contact("e"), false);
        assert_eq!(ws.focus, Some(c), "the last open took focus");

        ws.close(inbox_id);
        assert_eq!(tags(&ws), [vec!["help"]], "the whole chain went");
        assert!(!ws.slots.contains_key(&m) && !ws.slots.contains_key(&c));
        assert!(ws.joins.is_empty());
        assert_eq!(ws.focus, Some(help_id), "focus fell to what survived");
    }

    /// The same through a replace, which closes the chain under the slot it
    /// keeps: focus lands on that slot, showing what it now shows.
    #[test]
    fn replacing_takes_the_focus_off_the_chain() {
        let (mut ws, _help, inbox_id) = boot();
        let m = follow_open(&mut ws, inbox_id, msg(1), false);
        let c = follow_open(&mut ws, m, contact("e"), false);
        assert_eq!(ws.focus, Some(c));

        replace(&mut ws, inbox_id, about());
        assert_eq!(tags(&ws), [vec!["help"], vec!["about"]]);
        assert!(!ws.slots.contains_key(&m) && !ws.slots.contains_key(&c));
        assert_eq!(ws.focus, Some(inbox_id), "the slot that stayed");
        assert!(ws.slots.contains_key(&inbox_id), "and it is still there");
    }

    /// What a verb's action says — `wm.close(slot)` — reaches the workspace
    /// the slot is on, not the one being looked at. Through the deref this
    /// was a silent no-op: the chain was hunted in the active workspace's
    /// join map, and the detach bailed at `locate`.
    #[test]
    fn close_reaches_a_slot_on_another_workspace() {
        let mut wm = Wm::new();
        let h = wm_open(&mut wm, help(), None, false);
        wm.switch(1);
        let i = wm_open(&mut wm, inbox(), None, false);
        let m = wm_follow_open(&mut wm, i, msg(1));
        let c = wm_follow_open(&mut wm, m, contact("e"));
        wm.switch(0);

        wm.close(i);

        assert_eq!(wm.active, 0, "the close did not move anybody");
        assert!(wm.slot(i).is_none(), "the slot went");
        assert!(
            wm.slot(m).is_none() && wm.slot(c).is_none(),
            "and its chain with it"
        );
        assert!(wm.wss[1].columns.is_empty() && wm.wss[1].joins.is_empty());
        assert_eq!(wm.wss[1].focus, None, "nothing left there to focus");
        assert!(wm.slot(h).is_some(), "the other workspace is untouched");
    }

    /// A workspace move is not a close: the slot travels alone and its
    /// joins die with the lost adjacency, rather than dragging the chain.
    #[test]
    fn a_move_between_workspaces_leaves_the_chain_behind() {
        let mut wm = Wm::new();
        let i = wm_open(&mut wm, inbox(), None, false);
        let m = wm_follow_open(&mut wm, i, msg(1));
        wm.focus = Some(i);
        wm.send_focused_to(2);
        assert!(wm.wss[2].slots.contains_key(&i));
        assert!(wm.wss[0].slots.contains_key(&m), "the child stays put");
        assert!(wm.wss[0].joins.is_empty() && wm.wss[2].joins.is_empty());
    }

    /// niri's bracket binds: alone → consume into the neighbour; stacked →
    /// expel into a fresh column.
    #[test]
    fn consume_or_expel_round_trip() {
        let (mut ws, _help, inbox_id) = boot();
        let m = follow_open(&mut ws, inbox_id, msg(1), false);
        // msg is alone right of the inbox: cmd+[ consumes it into the inbox
        // column, at the bottom.
        ws.consume_or_expel(m, Dir::Left);
        assert_eq!(tags(&ws), [vec!["help"], vec!["inbox", "msg"]]);
        // Consuming broke column adjacency, so the join died with it.
        assert!(ws.joins.is_empty());
        // Stacked now: cmd+] expels it back out to the right.
        ws.consume_or_expel(m, Dir::Right);
        assert_eq!(tags(&ws), [vec!["help"], vec!["inbox"], vec!["msg"]]);
    }

    #[test]
    fn consume_from_right_and_expel_bottom() {
        let (mut ws, _help, inbox_id) = boot();
        follow_open(&mut ws, inbox_id, msg(1), false);
        // cmd+, pulls the message into the inbox column.
        ws.consume_from_right(inbox_id);
        assert_eq!(tags(&ws), [vec!["help"], vec!["inbox", "msg"]]);
        // cmd+. pushes the bottom slot back out.
        ws.expel_bottom(inbox_id);
        assert_eq!(tags(&ws), [vec!["help"], vec!["inbox"], vec!["msg"]]);
    }

    /// A column asked to hold more grid rows than exist distributes its
    /// height evenly instead of overflowing.
    #[test]
    fn overfull_column_distributes_evenly() {
        let (mut ws, _help, inbox_id) = boot();
        let m = follow_open(&mut ws, inbox_id, msg(1), false);
        ws.consume_or_expel(m, Dir::Left); // inbox(6) + msg(3) = 9 > 6
        let scene = ws.scene(VP, opts());
        let inbox_r = scene.slots.iter().find(|p| p.id == inbox_id).unwrap().rect;
        let msg_r = scene.slots.iter().find(|p| p.id == m).unwrap().rect;
        assert!((inbox_r.h - msg_r.h).abs() < 0.01, "even split");
    }

    /// On a small grid every oversized request clamps: the inbox (asking 4×6)
    /// fills a 4×3 screen exactly, and a second 4×3 panel cannot share its
    /// column.
    #[test]
    fn small_grid_clamps_requests() {
        let vp = (400.0, 700.0);
        let mut ws = Ws::new();
        ws.set_grid(Grid { w: 4, h: 3 });
        let i = open(&mut ws, inbox(), None, false);
        let scene = ws.scene(vp, opts());
        let r = scene.slots.iter().find(|p| p.id == i).unwrap().rect;
        // 4 of 4 units wide, 3 of 3 rows tall: the whole viewport minus gaps.
        assert!(
            (r.w - (vp.0 - 2.0 * 8.0)).abs() < 0.5,
            "full width, got {}",
            r.w
        );
        assert!(
            (r.h - (vp.1 - 2.0 * 8.0)).abs() < 0.5,
            "full height, got {}",
            r.h
        );
        // A message (4×3 clamped) doesn't fit under it → its own column.
        let m = follow_open(&mut ws, i, msg(1), false);
        assert_ne!(ws.locate(i).unwrap().0, ws.locate(m).unwrap().0);
    }

    /// Fold/unfold: switching the grid relayouts the same workspace.
    #[test]
    fn set_grid_relayouts() {
        let vp = (840.0, 700.0);
        let mut ws = Ws::new();
        let i = open(&mut ws, inbox(), None, false);
        ws.set_grid(Grid { w: 8, h: 4 });
        let scene = ws.scene(vp, opts());
        let r = scene.slots.iter().find(|p| p.id == i).unwrap().rect;
        // 4 of 8 units: about half the viewport.
        let unit = (vp.0 - 8.0) / 8.0;
        assert!(
            (r.w - (unit * 4.0 - 8.0)).abs() < 0.5,
            "half width, got {}",
            r.w
        );
        ws.set_grid(Grid { w: 4, h: 3 });
        let scene = ws.scene(vp, opts());
        let r = scene.slots.iter().find(|p| p.id == i).unwrap().rect;
        assert!(
            (r.w - (vp.0 - 2.0 * 8.0)).abs() < 0.5,
            "full width, got {}",
            r.w
        );
    }

    /// Touch drag-and-drop: a drop inside a column stacks by y; a drop in the
    /// space past the strip makes a fresh trailing column.
    #[test]
    fn place_at_stacks_and_inserts() {
        let (mut ws, help_id, inbox_id) = boot();
        // Drop help into the inbox column's middle, below the inbox's centre.
        let scene = ws.scene(VP, opts());
        let ir = scene.slots.iter().find(|p| p.id == inbox_id).unwrap().rect;
        ws.place_at(help_id, ir.x + ir.w / 2.0, ir.bottom() - 1.0, VP, opts());
        assert_eq!(tags(&ws), [vec!["inbox", "help"]]);
        // Drop it far right of everything: a new trailing column.
        ws.place_at(help_id, VP.0 * 3.0, 10.0, VP, opts());
        assert_eq!(tags(&ws), [vec!["inbox"], vec!["help"]]);
        // Drop it at the strip's left edge: first column again.
        ws.place_at(help_id, 0.0, 10.0, VP, opts());
        assert_eq!(tags(&ws), [vec!["help"], vec!["inbox"]]);
        assert_eq!(ws.focus, Some(help_id));
    }

    /// The drop is judged by the finger: a point in the gap between columns
    /// previews (and lands) a fresh column there; a point inside a column
    /// previews the stacking row.
    #[test]
    fn drop_target_finds_gaps_and_rows() {
        let (mut ws, help_id, inbox_id) = boot();
        let scene = ws.scene(VP, opts());
        let hr = scene.slots.iter().find(|p| p.id == help_id).unwrap().rect;
        let ir = scene.slots.iter().find(|p| p.id == inbox_id).unwrap().rect;
        // The gap between the two columns: a boundary, bar centred in it.
        let gx = (hr.right() + ir.x) / 2.0;
        let (t, bar) = ws.drop_target(help_id, gx, 100.0, VP, opts()).unwrap();
        assert_eq!(t, DropTarget::Boundary { at: 1 });
        assert!(
            (bar.x + bar.w / 2.0 - gx).abs() < 1.0,
            "bar centred in the gap"
        );
        assert!(bar.w < bar.h, "vertical bar");
        // Inside the inbox column, below its centre: stack after it.
        let (t, bar) = ws
            .drop_target(help_id, ir.x + ir.w / 2.0, ir.bottom() - 1.0, VP, opts())
            .unwrap();
        assert_eq!(t, DropTarget::Into { col: 1, row: 1 });
        assert!(bar.w > bar.h, "horizontal bar");
        // Over its own lone column: no target, the drop goes home.
        assert!(ws
            .drop_target(help_id, hr.x + hr.w / 2.0, 100.0, VP, opts())
            .is_none());
    }

    /// A released two-finger pan magnetises the camera to the nearest column
    /// alignment; the pan itself stays free.
    #[test]
    fn snap_camera_aligns_to_columns() {
        let mut ws = Ws::new();
        let mut last = open(&mut ws, help(), None, false);
        for id in 1..=3 {
            last = open(&mut ws, msg(id), Some(last), false);
        }
        // Four 4-unit columns on a 12-unit grid: one column of overflow.
        let unit = (VP.0 - 8.0) / 12.0;
        let col2 = unit * 4.0; // camera with column 2's left edge at the left gap
        ws.camera_x = 100.0;
        ws.snap_camera(VP, opts());
        assert!(
            (ws.camera_x - 0.0).abs() < 0.5,
            "close to home snaps home, got {}",
            ws.camera_x
        );
        ws.camera_x = 400.0;
        ws.snap_camera(VP, opts());
        assert!(
            (ws.camera_x - col2).abs() < 0.5,
            "snaps to column 2, got {} want {col2}",
            ws.camera_x
        );
    }

    /// Tabbed columns lay out only the active slot, and left/right focus
    /// enters them on it.
    #[test]
    fn tabbed_column_shows_active_only() {
        let (mut ws, help_id, inbox_id) = boot();
        let m = follow_open(&mut ws, inbox_id, msg(1), false);
        ws.consume_or_expel(m, Dir::Left); // [help][inbox+msg], focus msg
        ws.toggle_tabbed(m);
        let scene = ws.scene(VP, opts());
        // Every tab is in the scene at the SAME rect (a switch must be a pure
        // crossfade, no movement); only the active one is visible.
        let msg_s = scene.slots.iter().find(|p| p.id == m).unwrap();
        let inbox_s = scene.slots.iter().find(|p| p.id == inbox_id).unwrap();
        assert!(msg_s.visible);
        assert!(!inbox_s.visible);
        assert_eq!(msg_s.rect, inbox_s.rect);
        // Up switches tabs; visibility follows, rects stay put.
        ws.focus_dir(Dir::Up, VP, opts());
        assert_eq!(ws.focus, Some(inbox_id));
        let scene = ws.scene(VP, opts());
        assert!(
            scene
                .slots
                .iter()
                .find(|p| p.id == inbox_id)
                .unwrap()
                .visible
        );
        assert!(!scene.slots.iter().find(|p| p.id == m).unwrap().visible);
        // Entering from the left lands on the active tab, not "nearest".
        ws.focus = Some(help_id);
        ws.focus_dir(Dir::Right, VP, opts());
        assert_eq!(ws.focus, Some(inbox_id));
    }

    /// Workspaces: switching remembers focus and camera per space; a move
    /// re-homes the slot, follows it, and leaves old focus on a neighbour.
    #[test]
    fn workspaces_switch_and_move() {
        let mut wm = Wm::new();
        let help_id = wm_open(&mut wm, help(), None, false);
        let inbox_id = wm_open(&mut wm, inbox(), None, false);
        wm.focus = Some(inbox_id);
        wm.camera_x = 120.0;

        // Switch to an empty workspace and back: both are intact.
        assert!(wm.switch(1));
        assert!(wm.is_empty());
        assert_eq!(wm.focus, None);
        assert!(!wm.switch(1), "already there");
        wm.switch(0);
        assert_eq!(wm.focus, Some(inbox_id));
        assert_eq!(wm.camera_x, 120.0);

        // Move the focused slot to 3: it follows, its own trailing column.
        assert_eq!(wm.send_focused_to(3), Some(inbox_id));
        assert_eq!(wm.active, 3);
        assert_eq!(wm.focus, Some(inbox_id));
        assert_eq!(tags(&wm.wss[3]), [vec!["inbox"]]);
        // The old workspace keeps help, focus fell to it.
        assert_eq!(tags(&wm.wss[0]), [vec!["help"]]);
        assert_eq!(wm.wss[0].focus, Some(help_id));
        // A move to the active workspace is a no-op.
        assert_eq!(wm.send_focused_to(3), None);

        // Ids stay unique across workspaces (disjoint ranges per space).
        let a = wm_open(&mut wm, about(), None, false);
        assert_ne!(a, help_id);
        assert!(wm.slot(help_id).is_some(), "cross-space lookup");

        // Roster: occupied 0 and 3, plus the first empty slot 1.
        assert_eq!(wm.roster(), vec![0, 1, 3]);
    }

    /// An empty column in a snapshot — a store restore that dropped a
    /// slot another build wrote, keeping the column it sat in — is a gap
    /// on the strip, one unit wide and drawn as nothing. It does not come
    /// back: the strip has columns of panels, never places.
    #[test]
    fn restore_drops_empty_columns() {
        let mut snap = WmSnap::default();
        snap.wss[0] = WsSnap {
            columns: vec![
                (vec![1], false, 0),
                (Vec::new(), false, 0),
                (Vec::new(), true, 0),
                (vec![2], false, 0),
            ],
            slots: vec![(1, help()), (2, inbox())],
            joins: Vec::new(),
            focus: Some(2),
        };
        let wm = Wm::restore(snap);
        let ws = &wm.wss[0];
        assert_eq!(ws.columns.len(), 2);
        assert_eq!(ws.columns[0].slots, vec![1]);
        assert_eq!(ws.columns[1].slots, vec![2]);
        assert_eq!(ws.focus, Some(2));
        assert!(wm.snapshot().wss[0]
            .columns
            .iter()
            .all(|(p, _, _)| !p.is_empty()));
    }

    /// Snapshot → restore is lossless for the logical state, and id minting
    /// resumes above every restored id — even for a slot that moved into a
    /// foreign workspace's range.
    #[test]
    fn snapshot_restore_round_trips() {
        let mut wm = Wm::new();
        let inbox_id = wm_open(&mut wm, inbox(), None, false);
        let m = wm_follow_open(&mut wm, inbox_id, msg(1));
        wm.toggle_tabbed(m);
        wm.send_focused_to(2); // msg (a ws-1 id) now lives on ws 3
        wm.switch(0);
        wm.focus = Some(inbox_id);

        let snap = wm.snapshot();
        let back = Wm::restore(snap.clone());
        assert_eq!(back.snapshot(), snap, "lossless round trip");
        assert_eq!(back.active, 0);
        assert_eq!(back.focus, Some(inbox_id));
        assert_eq!(back.wss[2].focus, Some(m));

        // Fresh ids never collide with restored ones, in either space.
        let mut back = back;
        let a = wm_open(&mut back, about(), None, false);
        assert!(a > inbox_id && a != m);
        back.switch(2);
        let b = wm_open(&mut back, about(), None, false);
        assert!(b != m && b != a);

        // A stale focus (corrupt store) is dropped instead of trusted.
        let mut bad = snap;
        bad.wss[0].focus = Some(0xdead);
        assert_eq!(Wm::restore(bad).wss[0].focus, None);
    }

    /// Moving a joined child re-homes just the slot; the join dies with the
    /// adjacency. The grid applies to every workspace at once.
    #[test]
    fn workspace_move_breaks_joins_and_grid_is_global() {
        let mut wm = Wm::new();
        let inbox_id = wm_open(&mut wm, inbox(), None, false);
        let m = wm_follow_open(&mut wm, inbox_id, msg(1));
        assert_eq!(wm.joined_child(inbox_id), Some(m));
        wm.send_focused_to(1);
        assert_eq!(wm.active, 1);
        assert!(wm.wss[0].joins.is_empty(), "join died with the move");
        assert_eq!(tags(&wm.wss[1]), [vec!["msg"]]);

        wm.set_grid(Grid { w: 4, h: 3 });
        assert_eq!(wm.wss[0].grid, Grid { w: 4, h: 3 });
        assert_eq!(wm.wss[8].grid, Grid { w: 4, h: 3 });
    }

    /// A panel nobody measured gets the kernel's default, and the wish map
    /// is what overrides it.
    #[test]
    fn an_unmeasured_panel_gets_the_default_wish() {
        let mut ws = Ws::new();
        assert_eq!(ws.wish_of(&msg(1)), DEFAULT_WISH);
        ws.wish(msg(1), (5, 5));
        assert_eq!(ws.wish_of(&msg(1)), (5, 5));
        assert_eq!(ws.wish_of(&msg(2)), DEFAULT_WISH, "keyed by the identity");
        ws.set_wishes(HashMap::new());
        assert_eq!(ws.wish_of(&msg(1)), DEFAULT_WISH, "a relayout drops them");
    }
}
