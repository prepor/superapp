//! Scenes: a subject in its named states, and the library canvas's layout.
//!
//! A **scene** is one subject — a component, a panel, the workspace — in
//! the states worth looking at while it is being worked on: an inbox row
//! read, unread, selected; a thread message collapsed, open, its quote
//! unfolded. The states are **nodes**; an **edge** names what takes one to
//! another; notes annotate both. The edges make a DAG, not a line: a
//! design review fans out — the same thing in its variants — where a
//! behaviour suite walks, which is why the library is not a reading of
//! the e2e scripts. Those check behaviour; this shows states.
//!
//! Scenes are authored in Rust ([`crate::catalog`]): fixtures are the real
//! structs and a state is set through the widget's own methods, so a
//! refactor that breaks a scene fails to compile rather than quietly
//! rearranging the canvas. This module holds the shape and the layout
//! only, generic over the setup payload — std-only, unit-tested without a
//! window.

use std::collections::{HashMap, VecDeque};

use crate::core::Rect;

/// One state of the subject.
#[derive(Debug, Clone, PartialEq)]
pub struct Node<S> {
    pub name: String,
    /// The annotation under the name.
    pub note: Vec<String>,
    /// The node's viewport, points: the size the subject is shown at.
    pub size: (f64, f64),
    /// How the node comes up — the payload the shell reads.
    pub setup: S,
}

/// What takes one state to another, by node name.
#[derive(Debug, Clone, PartialEq)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub label: String,
}

/// A subject and its states.
#[derive(Debug, Clone, PartialEq)]
pub struct Scene<S> {
    pub name: String,
    /// The description under the title.
    pub note: Vec<String>,
    /// The size a node has unless it says otherwise.
    pub size: (f64, f64),
    pub nodes: Vec<Node<S>>,
    pub edges: Vec<Edge>,
}

impl<S> Scene<S> {
    /// A scene whose nodes are `size` unless [`Scene::sized`] says otherwise.
    #[must_use]
    pub fn new(name: &str, size: (f64, f64)) -> Self {
        Scene {
            name: name.to_string(),
            note: Vec::new(),
            size,
            nodes: Vec::new(),
            edges: Vec::new(),
        }
    }

    /// A line of the scene's description.
    #[must_use]
    pub fn note(mut self, line: &str) -> Self {
        self.note.push(line.to_string());
        self
    }

    /// A state. Its name is what edges and the canvas's script address.
    #[must_use]
    pub fn node(mut self, name: &str, setup: S) -> Self {
        self.nodes.push(Node {
            name: name.to_string(),
            note: Vec::new(),
            size: self.size,
            setup,
        });
        self
    }

    /// Resizes the node just added — a row at another width, a taller
    /// message.
    #[must_use]
    pub fn sized(mut self, size: (f64, f64)) -> Self {
        if let Some(n) = self.nodes.last_mut() {
            n.size = size;
        }
        self
    }

    /// A line of annotation on the node just added (on the scene, before
    /// any node).
    #[must_use]
    pub fn about(mut self, line: &str) -> Self {
        match self.nodes.last_mut() {
            Some(n) => n.note.push(line.to_string()),
            None => self.note.push(line.to_string()),
        }
        self
    }

    /// What takes `from` to `to`: a chord, a click, an event in words.
    #[must_use]
    pub fn edge(mut self, from: &str, to: &str, label: &str) -> Self {
        self.edges.push(Edge {
            from: from.to_string(),
            to: to.to_string(),
            label: label.to_string(),
        });
        self
    }

    #[must_use]
    pub fn index(&self, name: &str) -> Option<usize> {
        self.nodes.iter().position(|n| n.name == name)
    }

    /// Every node named once, every edge between nodes that exist, and no
    /// way round: the shape the layout relies on. The catalogue's test
    /// runs this over every scene, so a bad edge fails there, not on the
    /// canvas.
    pub fn check(&self) -> Result<(), String> {
        let mut seen: HashMap<&str, usize> = HashMap::new();
        for n in &self.nodes {
            if seen.insert(&n.name, 1).is_some() {
                return Err(format!("{}: node {:?} named twice", self.name, n.name));
            }
            if n.name.is_empty() {
                return Err(format!("{}: a node with no name", self.name));
            }
        }
        for e in &self.edges {
            for end in [&e.from, &e.to] {
                if self.index(end).is_none() {
                    return Err(format!(
                        "{}: edge {:?} → {:?} names no node {:?}",
                        self.name, e.from, e.to, end
                    ));
                }
            }
            if e.from == e.to {
                return Err(format!("{}: edge {:?} → itself", self.name, e.from));
            }
        }
        self.layers().map(|_| ())
    }

    /// Each node's layer: 0 for a state nothing leads to, else one past
    /// the longest chain of edges into it. Errors on a cycle or an edge to
    /// nowhere.
    pub fn layers(&self) -> Result<Vec<usize>, String> {
        let n = self.nodes.len();
        let mut out: Vec<Vec<usize>> = vec![Vec::new(); n];
        let mut indeg = vec![0usize; n];
        for e in &self.edges {
            let (Some(a), Some(b)) = (self.index(&e.from), self.index(&e.to)) else {
                return Err(format!("{}: edge {:?} → {:?} names no node", self.name, e.from, e.to));
            };
            out[a].push(b);
            indeg[b] += 1;
        }
        let mut layer = vec![0usize; n];
        let mut queue: VecDeque<usize> = (0..n).filter(|&i| indeg[i] == 0).collect();
        let mut done = 0;
        while let Some(a) = queue.pop_front() {
            done += 1;
            for &b in &out[a] {
                layer[b] = layer[b].max(layer[a] + 1);
                indeg[b] -= 1;
                if indeg[b] == 0 {
                    queue.push_back(b);
                }
            }
        }
        if done < n {
            return Err(format!("{}: the edges go round in a cycle", self.name));
        }
        Ok(layer)
    }
}

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

/// The mono face, per point of font size: advance per character and the
/// line height. Measured by the shell; the layout only multiplies.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    pub adv: f64,
    pub line: f64,
}

/// Canvas text sizes, in points at zoom 1. Larger than the app's own type:
/// a caption has to survive zooming out.
pub const TITLE_PT: f64 = 30.0;
pub const TEXT_PT: f64 = 16.0;

/// Canvas margins and gaps, points at zoom 1.
pub const MARGIN: f64 = 120.0;
/// Scenes are far apart: titles and node names are laid in screen space
/// at a legible minimum, and the gap has to hold them at the zoom that
/// fits a whole canvas.
pub const BLOCK_GAP: f64 = 420.0;
/// Between a layer and the next — wider when an edge label needs it.
pub const COL_GAP_MIN: f64 = 200.0;
/// Between nodes stacked in one layer.
pub const ROW_GAP: f64 = 90.0;
const LABEL_PAD: f64 = 80.0;
const CAPTION_GAP: f64 = 14.0;
/// Where an arrow meets a node: its vertical centre, or this far down for
/// anything tall — through a panel's header, not its middle.
const ARROW_Y_MAX: f64 = 28.0;

/// A node's place on the canvas.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeBox {
    pub node: usize,
    /// The mount.
    pub rect: Rect,
    /// Top-left of the caption block (name, then the note) above the mount.
    pub caption: (f64, f64),
}

/// An arrow: out of `from` to the right, down or up along `elbow_x`, into
/// `to` from the left. Level when the two ends share a height.
#[derive(Debug, Clone, PartialEq)]
pub struct Arrow {
    pub edge: usize,
    pub from: (f64, f64),
    pub elbow_x: f64,
    pub to: (f64, f64),
    /// Top-left of the label: centred on the elbow, clear of the line.
    pub label_at: (f64, f64),
    pub label: String,
}

/// One scene's block.
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub scene: usize,
    pub title: (f64, f64),
    pub note: (f64, f64),
    pub nodes: Vec<NodeBox>,
    pub arrows: Vec<Arrow>,
    /// Everything the block draws, for fitting it in the viewport.
    pub bounds: Rect,
}

/// The whole canvas, in points at zoom 1.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Canvas {
    pub blocks: Vec<Block>,
    pub w: f64,
    pub h: f64,
}

/// The canvas's target width: blocks flow into rows this wide, so a
/// catalogue of any length comes out roughly square and fits the window
/// at a zoom that keeps its names apart.
pub const ROW_W: f64 = 5600.0;

/// Lays the scenes out: one block per scene, blocks flowing left to right
/// and wrapping into rows. Within a block the DAG is layered: roots in the
/// left column, a node one column right of the longest chain into it, a
/// layer's nodes stacked under one another, each beside the row its first
/// predecessor sits in. Deterministic from the catalogue, so there is
/// nothing to persist and nothing to drag.
pub fn layout<S>(scenes: &[Scene<S>], m: &Metrics) -> Canvas {
    let mut blocks = Vec::new();
    let (mut x, mut y, mut row_h, mut w_max) = (MARGIN, MARGIN, 0.0f64, 0.0f64);
    for (si, scene) in scenes.iter().enumerate() {
        let mut b = block(scene, si, m);
        if x > MARGIN && x + b.bounds.w > ROW_W {
            x = MARGIN;
            y += row_h + BLOCK_GAP;
            row_h = 0.0;
        }
        shift(&mut b, x, y);
        x += b.bounds.w + BLOCK_GAP;
        row_h = row_h.max(b.bounds.h);
        w_max = w_max.max(x - BLOCK_GAP + MARGIN);
        blocks.push(b);
    }
    Canvas {
        blocks,
        w: w_max.max(MARGIN * 2.0),
        h: (y + row_h + MARGIN).max(MARGIN * 2.0),
    }
}

fn shift(b: &mut Block, dx: f64, dy: f64) {
    let mv = |p: &mut (f64, f64)| {
        p.0 += dx;
        p.1 += dy;
    };
    mv(&mut b.title);
    mv(&mut b.note);
    for nb in &mut b.nodes {
        nb.rect.x += dx;
        nb.rect.y += dy;
        mv(&mut nb.caption);
    }
    for a in &mut b.arrows {
        mv(&mut a.from);
        mv(&mut a.to);
        a.elbow_x += dx;
        mv(&mut a.label_at);
    }
    b.bounds.x += dx;
    b.bounds.y += dy;
}

/// One scene's block, laid at the origin.
fn block<S>(scene: &Scene<S>, si: usize, m: &Metrics) -> Block {
    let text_w = |s: &str, pt: f64| s.chars().count() as f64 * m.adv * pt;
    let title_h = m.line * TITLE_PT;
    let line_h = m.line * TEXT_PT;
    let n = scene.nodes.len();
    let title = (0.0, 0.0);
    let note = (0.0, title_h + 6.0);
    let note_h = scene.note.len() as f64 * line_h;
    let top = note.1 + note_h + 48.0;
    let layer = scene.layers().unwrap_or_else(|_| vec![0; n]);
    let n_layers = layer.iter().copied().max().map_or(0, |l| l + 1);
    let edges: Vec<(usize, usize, usize)> = scene
        .edges
        .iter()
        .enumerate()
        .filter_map(|(ei, e)| Some((ei, scene.index(&e.from)?, scene.index(&e.to)?)))
        .collect();

    // Columns, and each node's row in its column: a node goes beside
    // its first predecessor's row where it can, authoring order
    // breaking ties.
    let mut cols: Vec<Vec<usize>> = vec![Vec::new(); n_layers];
    let mut row = vec![0usize; n];
    for l in 0..n_layers {
        let mut members: Vec<usize> = (0..n).filter(|&i| layer[i] == l).collect();
        let key = |i: usize| {
            edges
                .iter()
                .filter(|&&(_, _, b)| b == i)
                .map(|&(_, a, _)| row[a])
                .min()
                .unwrap_or(usize::MAX)
        };
        members.sort_by_key(|&i| (key(i), i));
        for (r, &i) in members.iter().enumerate() {
            row[i] = r;
        }
        cols[l] = members;
    }

    // Row bands across the block: a row's caption block is as tall as
    // the longest note in it, so a chain sits on one line.
    let n_rows = cols.iter().map(Vec::len).max().unwrap_or(0);
    let mut band_top = vec![0.0; n_rows];
    let mut node_top = vec![0.0; n_rows];
    let mut by = top;
    for r in 0..n_rows {
        let in_row = (0..n).filter(|&i| row[i] == r);
        let (mut notes, mut h) = (0usize, 0.0f64);
        for i in in_row {
            notes = notes.max(scene.nodes[i].note.len());
            h = h.max(scene.nodes[i].size.1);
        }
        let caption_h = line_h * (1.0 + notes as f64);
        band_top[r] = by;
        node_top[r] = by + caption_h + CAPTION_GAP;
        by = node_top[r] + h + ROW_GAP;
    }

    // Column x: each as wide as its widest node, the gap after it as
    // wide as the widest label leaving it.
    let mut col_x = vec![0.0; n_layers];
    let mut col_w = vec![0.0; n_layers];
    let mut gap = vec![COL_GAP_MIN; n_layers];
    let mut x = 0.0;
    for l in 0..n_layers {
        col_x[l] = x;
        col_w[l] = cols[l]
            .iter()
            .map(|&i| scene.nodes[i].size.0)
            .fold(0.0, f64::max);
        let widest = edges
            .iter()
            .filter(|&&(_, a, _)| layer[a] == l)
            .map(|&(ei, _, _)| text_w(&scene.edges[ei].label, TEXT_PT))
            .fold(0.0, f64::max);
        gap[l] = COL_GAP_MIN.max(widest + LABEL_PAD);
        x += col_w[l] + gap[l];
    }

    let mut nodes = Vec::new();
    for i in 0..n {
        let (w, h) = scene.nodes[i].size;
        let l = layer[i];
        let r = row[i];
        nodes.push(NodeBox {
            node: i,
            rect: Rect {
                x: col_x[l],
                y: node_top[r],
                w,
                h,
            },
            caption: (col_x[l], band_top[r]),
        });
    }
    let arrow_y = |i: usize| {
        let nb = &nodes[i];
        nb.rect.y + (nb.rect.h / 2.0).min(ARROW_Y_MAX)
    };
    let mut arrows = Vec::new();
    for &(ei, a, b) in &edges {
        let from = (nodes[a].rect.x + nodes[a].rect.w, arrow_y(a));
        let to = (nodes[b].rect.x, arrow_y(b));
        let la = layer[a];
        let elbow_x = col_x[la] + col_w[la] + gap[la] / 2.0;
        let label = scene.edges[ei].label.clone();
        let lw = text_w(&label, TEXT_PT);
        // The line comes down into a lower target, so the label goes
        // under that last run; otherwise above it.
        let ly = if from.1 < to.1 {
            to.1 + 10.0
        } else {
            to.1 - 10.0 - line_h
        };
        arrows.push(Arrow {
            edge: ei,
            from,
            elbow_x,
            to,
            label_at: (elbow_x - lw / 2.0, ly),
            label,
        });
    }

    let title_w = scene
        .note
        .iter()
        .map(|l| text_w(l, TEXT_PT))
        .fold(text_w(&scene.name, TITLE_PT), f64::max);
    let right = nodes
        .iter()
        .map(|nb| nb.rect.x + nb.rect.w)
        .fold(title_w, f64::max);
    let bottom = nodes
        .iter()
        .map(|nb| nb.rect.y + nb.rect.h)
        .fold(note.1 + note_h, f64::max);
    Block {
        scene: si,
        title,
        note,
        nodes,
        arrows,
        bounds: Rect {
            x: 0.0,
            y: 0.0,
            w: right,
            h: bottom,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m() -> Metrics {
        Metrics {
            adv: 0.6,
            line: 1.2,
        }
    }

    fn row_scene() -> Scene<()> {
        Scene::new("inbox row", (520.0, 56.0))
            .note("one conversation, two lines")
            .node("read", ())
            .node("unread", ())
            .about("bold while any message is unread")
            .node("selected", ())
            .node("narrow", ())
            .sized((320.0, 56.0))
            .edge("read", "unread", "a reply arrives")
            .edge("read", "selected", "j / click")
    }

    #[test]
    fn the_builder_keeps_names_notes_and_sizes() {
        let s = row_scene();
        assert_eq!(s.name, "inbox row");
        assert_eq!(s.note, ["one conversation, two lines"]);
        let names: Vec<&str> = s.nodes.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, ["read", "unread", "selected", "narrow"]);
        assert_eq!(s.nodes[1].note, ["bold while any message is unread"]);
        assert_eq!(s.nodes[0].size, (520.0, 56.0));
        assert_eq!(s.nodes[3].size, (320.0, 56.0));
        assert_eq!(s.edges.len(), 2);
        assert!(s.check().is_ok());
    }

    #[test]
    fn about_before_any_node_describes_the_scene() {
        let s: Scene<()> = Scene::new("x", (1.0, 1.0)).about("what it is");
        assert_eq!(s.note, ["what it is"]);
    }

    #[test]
    fn check_rejects_bad_shapes() {
        let twice: Scene<()> = Scene::new("s", (1.0, 1.0)).node("a", ()).node("a", ());
        assert!(twice.check().unwrap_err().contains("named twice"));
        let nowhere: Scene<()> = Scene::new("s", (1.0, 1.0))
            .node("a", ())
            .edge("a", "b", "");
        assert!(nowhere.check().unwrap_err().contains("names no node"));
        let selfloop: Scene<()> = Scene::new("s", (1.0, 1.0))
            .node("a", ())
            .edge("a", "a", "");
        assert!(selfloop.check().unwrap_err().contains("itself"));
        let cycle: Scene<()> = Scene::new("s", (1.0, 1.0))
            .node("a", ())
            .node("b", ())
            .node("c", ())
            .edge("a", "b", "")
            .edge("b", "c", "")
            .edge("c", "a", "");
        assert!(cycle.check().unwrap_err().contains("cycle"));
    }

    #[test]
    fn layers_follow_the_longest_chain() {
        let chain: Scene<()> = Scene::new("s", (1.0, 1.0))
            .node("a", ())
            .node("b", ())
            .node("c", ())
            .edge("a", "b", "")
            .edge("b", "c", "");
        assert_eq!(chain.layers().unwrap(), [0, 1, 2]);
        let diamond: Scene<()> = Scene::new("s", (1.0, 1.0))
            .node("a", ())
            .node("b", ())
            .node("c", ())
            .node("d", ())
            .node("lone", ())
            .edge("a", "b", "")
            .edge("a", "c", "")
            .edge("b", "d", "")
            .edge("c", "d", "")
            .edge("a", "d", "");
        assert_eq!(diamond.layers().unwrap(), [0, 1, 1, 2, 0]);
    }

    #[test]
    fn a_fan_out_stacks_in_one_column_and_a_chain_stays_level() {
        let c = layout(&[row_scene()], &m());
        assert_eq!(c.blocks.len(), 1);
        let b = &c.blocks[0];
        let nb = |name: &str| {
            let i = row_scene().index(name).unwrap();
            b.nodes.iter().find(|n| n.node == i).unwrap().clone()
        };
        let (read, unread, selected, narrow) =
            (nb("read"), nb("unread"), nb("selected"), nb("narrow"));
        // Roots in the left column, on one line; targets one column right.
        assert_eq!(read.rect.x, MARGIN);
        assert_eq!(narrow.rect.x, MARGIN);
        assert!(unread.rect.x >= read.rect.x + read.rect.w + COL_GAP_MIN);
        assert_eq!(unread.rect.x, selected.rect.x);
        // The fan-out stacks under itself, the first beside its source.
        assert_eq!(unread.rect.y, read.rect.y);
        assert!(selected.rect.y >= unread.rect.y + unread.rect.h + ROW_GAP);
        // The other root sits under the first, no overlap.
        assert!(narrow.rect.y >= read.rect.y + read.rect.h + ROW_GAP);
        assert_eq!((narrow.rect.w, narrow.rect.h), (320.0, 56.0));
        // Captions sit above their mount.
        assert!(unread.caption.1 < unread.rect.y);
        // Arrows leave the right edge and arrive at the left edge.
        assert_eq!(b.arrows.len(), 2);
        let level = &b.arrows[0];
        assert_eq!(level.from.0, read.rect.x + read.rect.w);
        assert_eq!(level.to.0, unread.rect.x);
        assert_eq!(level.from.1, level.to.1);
        assert!(level.elbow_x > level.from.0 && level.elbow_x < level.to.0);
        // A level arrow's label sits above the line, inside the gap.
        assert!(level.label_at.1 < level.from.1);
        assert!(level.label_at.0 > level.from.0);
        let down = &b.arrows[1];
        assert!(down.to.1 > down.from.1);
        // Coming down, the label goes under the last run.
        assert!(down.label_at.1 > down.to.1);
        assert_eq!(down.elbow_x, level.elbow_x);
        // The bounds hold everything.
        assert!(b.bounds.x + b.bounds.w >= unread.rect.x + unread.rect.w);
        assert!(b.bounds.y + b.bounds.h >= selected.rect.y + selected.rect.h);
        assert!(c.w > unread.rect.x + unread.rect.w);
        assert!(c.h > selected.rect.y + selected.rect.h);
    }

    #[test]
    fn a_wide_label_widens_the_gap() {
        let long = "a".repeat(60);
        let s: Scene<()> = Scene::new("s", (100.0, 20.0))
            .node("a", ())
            .node("b", ())
            .edge("a", "b", &long);
        let c = layout(&[s], &m());
        let b = &c.blocks[0];
        let gap = b.nodes[1].rect.x - (b.nodes[0].rect.x + b.nodes[0].rect.w);
        assert!(gap > 60.0 * 0.6 * TEXT_PT);
        // …and the label still fits inside it.
        let a = &b.arrows[0];
        assert!(a.label_at.0 >= a.from.0);
        assert!(a.label_at.0 + 60.0 * 0.6 * TEXT_PT <= a.to.0);
    }

    #[test]
    fn blocks_flow_into_a_row_and_wrap_when_it_is_full() {
        let a: Scene<()> = Scene::new("rows", (520.0, 56.0)).node("x", ());
        let b: Scene<()> = Scene::new("panel", (520.0, 640.0))
            .node("p", ())
            .node("q", ())
            .edge("p", "q", "j");
        let wide: Scene<()> = Scene::new("workspace", (ROW_W * 0.6, 900.0)).node("w", ());
        let c = layout(&[a, b, wide], &m());
        assert_eq!(c.blocks.len(), 3);
        let (b0, b1, b2) = (&c.blocks[0], &c.blocks[1], &c.blocks[2]);
        // The second block sits beside the first, on the same line.
        assert_eq!(b1.title.1, b0.title.1);
        assert!(b1.bounds.x >= b0.bounds.x + b0.bounds.w + BLOCK_GAP);
        assert_eq!(b1.nodes[0].rect.x, b1.bounds.x);
        // The wide one does not fit the row: a new one, under the tallest.
        assert_eq!(b2.bounds.x, MARGIN);
        assert!(b2.bounds.y >= b1.bounds.y + b1.bounds.h + BLOCK_GAP);
        // A tall node meets its arrow near the top, not at its middle.
        let p = &b1.nodes[0];
        assert_eq!(b1.arrows[0].from.1, p.rect.y + 28.0);
        assert!(c.w >= b1.bounds.x + b1.bounds.w + MARGIN);
        assert!(c.h >= b2.bounds.y + b2.bounds.h + MARGIN);
    }
}
