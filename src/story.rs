//! Stories: an e2e script read as a flow (CR-006).
//!
//! The scripts under `e2e/` already document the UI's behaviour, one walk
//! per file. Read as a story, every `shot` is a **named state** — a node on
//! the panels-library canvas — the steps between two shots are the **arrow**
//! from one node to the next, and the `#` comments are the **annotations**.
//! Nothing is authored twice: a story stays a suite the harness can run, and
//! the canvas is only a second reading of the same file.
//!
//! A script may open with a `#!` header naming what its mounts need, the
//! way the book's prose used to ("run with `--window 380x780 --grid 4x3`"):
//!
//! ```text
//! #! window 380x780 grid 4x3      # the mount's viewport and unit grid
//! #! send-delay 1                 # the send-undo window, seconds
//! #! outside real                 # deny (default) | fake | real
//! #! library                      # on the shelf: what `--library` shows by default
//! #! canvas                       # drives the canvas itself; never mounted
//! ```
//!
//! The harness ignores the header (it is a comment), so a story runs under
//! `--e2e` exactly as before.
//!
//! This module is std-only: parsing and the canvas layout are pure, so the
//! shape of the library is unit-tested without a window.

use std::path::{Path, PathBuf};

use crate::core::{Grid, Rect};
use crate::e2e::{self, Step};

/// Which outside a story's mounts get. `Deny` is the default (CR-004's
/// proposal for the library): a panel that quietly reaches the network
/// while you look at it fails loudly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutsideKind {
    /// Every verb fails; the clock still runs.
    #[default]
    Deny,
    /// The in-memory mail world.
    Fake,
    /// The real network — what the harness itself runs against.
    Real,
}

/// What a story's mounts need: the shell's e2e flags, per file.
#[derive(Debug, Clone, PartialEq)]
pub struct MountCfg {
    /// The mount's viewport in points — the window the suite runs in.
    pub window: (f64, f64),
    /// A forced unit grid (`--grid`), or the platform default.
    pub grid: Option<Grid>,
    /// The send-undo window, seconds (`--send-delay`).
    pub send_delay: f64,
    /// The outside.
    pub outside: OutsideKind,
}

impl Default for MountCfg {
    fn default() -> Self {
        MountCfg {
            // The window the headless harness renders: the DSL's inner size.
            window: (1440.0, 900.0),
            grid: None,
            send_delay: 10.0,
            outside: OutsideKind::Deny,
        }
    }
}

/// One named state: a `shot`, with what leads up to it.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    /// The shot's name.
    pub name: String,
    /// The comment lines in this node's segment — its annotation.
    pub note: Vec<String>,
    /// The steps since the previous shot, verbatim (waits left out): the
    /// arrow's label.
    pub labels: Vec<String>,
    /// Index of this node's `Shot` step in [`Story::steps`]; a mount replays
    /// `steps[..=until]` to reach the state.
    pub until: usize,
}

/// A script, read as a flow.
#[derive(Debug, Clone, PartialEq)]
pub struct Story {
    /// The file stem.
    pub name: String,
    /// The comment block the file opens with — the story's description.
    pub intro: Vec<String>,
    /// What its mounts need.
    pub cfg: MountCfg,
    /// True for a script that drives the canvas itself (`#! canvas`); the
    /// loader leaves those out.
    pub canvas: bool,
    /// On the shelf (`#! library`): what the canvas shows when asked for no
    /// stories in particular. There will be many more scripts than anyone
    /// wants to review at once; the shelf is the few that matter now.
    pub shelf: bool,
    /// Every step through the last shot. No `quit`: a mount never ends.
    pub steps: Vec<Step>,
    /// The nodes, in shot order.
    pub nodes: Vec<Node>,
}

fn parse_wxh(s: &str) -> Option<(f64, f64)> {
    let (w, h) = s.split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?))
}

/// Parses a `#!` header line into the config. Errors carry the line.
fn parse_header(
    line: &str,
    lineno: usize,
    cfg: &mut MountCfg,
    canvas: &mut bool,
    shelf: &mut bool,
) -> Result<(), String> {
    let err = |m: &str| format!("line {lineno}: {m}: {line}");
    let mut it = line.trim_start_matches("#!").split_whitespace();
    while let Some(key) = it.next() {
        match key {
            "canvas" => *canvas = true,
            "library" => *shelf = true,
            "window" => {
                cfg.window = it
                    .next()
                    .and_then(parse_wxh)
                    .ok_or_else(|| err("expected `window WxH`"))?;
            }
            "grid" => {
                let (w, h) = it
                    .next()
                    .and_then(parse_wxh)
                    .ok_or_else(|| err("expected `grid WxH`"))?;
                cfg.grid = Some(Grid {
                    w: w as u32,
                    h: h as u32,
                });
            }
            "send-delay" => {
                cfg.send_delay = it
                    .next()
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| err("expected `send-delay SECONDS`"))?;
            }
            "outside" => {
                cfg.outside = match it.next() {
                    Some("deny") => OutsideKind::Deny,
                    Some("fake") => OutsideKind::Fake,
                    Some("real") => OutsideKind::Real,
                    _ => return Err(err("expected `outside deny|fake|real`")),
                };
            }
            _ => return Err(err("unknown header key")),
        }
    }
    Ok(())
}

/// Reads a script as a story. `name` is the file stem.
pub fn parse(name: &str, src: &str) -> Result<Story, String> {
    let mut story = Story {
        name: name.to_string(),
        intro: Vec::new(),
        cfg: MountCfg::default(),
        canvas: false,
        shelf: false,
        steps: Vec::new(),
        nodes: Vec::new(),
    };
    // The intro is the comment block the file opens with; it ends at the
    // first line that is not a comment. Every later comment annotates the
    // segment it sits in.
    let mut in_intro = true;
    let mut note: Vec<String> = Vec::new();
    let mut labels: Vec<String> = Vec::new();
    for (i, raw) in src.lines().enumerate() {
        let line = raw.trim();
        if line.starts_with("#!") {
            parse_header(line, i + 1, &mut story.cfg, &mut story.canvas, &mut story.shelf)?;
            continue;
        }
        if let Some(c) = line.strip_prefix('#') {
            let text = c.trim().to_string();
            if in_intro {
                story.intro.push(text);
            } else if !text.is_empty() || !note.is_empty() {
                note.push(text);
            }
            continue;
        }
        in_intro = false;
        let Some(step) = e2e::parse_line(raw, i + 1)? else {
            continue;
        };
        match step {
            Step::Quit => break,
            Step::Shot(shot) => {
                story.steps.push(Step::Shot(shot.clone()));
                while note.last().is_some_and(String::is_empty) {
                    note.pop();
                }
                story.nodes.push(Node {
                    name: shot,
                    note: std::mem::take(&mut note),
                    labels: std::mem::take(&mut labels),
                    until: story.steps.len() - 1,
                });
            }
            Step::Wait(_) => story.steps.push(step),
            other => {
                labels.push(line.to_string());
                story.steps.push(other);
            }
        }
    }
    // Steps past the last shot lead nowhere the canvas can show.
    if let Some(last) = story.nodes.last() {
        story.steps.truncate(last.until + 1);
    } else {
        story.steps.clear();
    }
    Ok(story)
}

/// Loads stories from files and directories (a directory contributes its
/// `*.txt`, sorted). Canvas scripts are left out; with `shelf_only`, so is
/// everything not marked `#! library`.
pub fn load(paths: &[String], shelf_only: bool) -> Result<Vec<Story>, String> {
    let mut files: Vec<PathBuf> = Vec::new();
    for p in paths {
        let p = Path::new(p);
        if p.is_dir() {
            let mut in_dir: Vec<PathBuf> = std::fs::read_dir(p)
                .map_err(|e| format!("{}: {e}", p.display()))?
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|f| f.extension().is_some_and(|x| x == "txt"))
                .collect();
            in_dir.sort();
            files.extend(in_dir);
        } else {
            files.push(p.to_path_buf());
        }
    }
    let mut stories = Vec::new();
    for f in files {
        let src = std::fs::read_to_string(&f).map_err(|e| format!("{}: {e}", f.display()))?;
        let name = f
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let story = parse(&name, &src).map_err(|e| format!("{}: {e}", f.display()))?;
        if !story.canvas && !story.nodes.is_empty() && (story.shelf || !shelf_only) {
            stories.push(story);
        }
    }
    Ok(stories)
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
/// a caption sits beside a 1440-wide mount and has to survive zooming out.
pub const TITLE_PT: f64 = 30.0;
pub const TEXT_PT: f64 = 16.0;

/// Canvas margins and gaps, points at zoom 1.
pub const MARGIN: f64 = 120.0;
/// Rows are far apart: story and node names are laid in screen space at a
/// legible minimum, and the gap has to hold two of those lines at the
/// zoom that fits a whole canvas.
pub const ROW_GAP: f64 = 700.0;
pub const NODE_GAP_MIN: f64 = 240.0;
const LABEL_PAD: f64 = 80.0;
const CAPTION_GAP: f64 = 14.0;

/// A node's place on the canvas.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeBox {
    pub node: usize,
    /// The mount.
    pub rect: Rect,
    /// Top-left of the caption block (name, then the note) above the mount.
    pub caption: (f64, f64),
}

/// An arrow from one node to the next, with its labels stacked above.
#[derive(Debug, Clone, PartialEq)]
pub struct Arrow {
    pub from: (f64, f64),
    pub to: (f64, f64),
    /// Top-left of the label block; lines flow down from here.
    pub labels_at: (f64, f64),
    pub labels: Vec<String>,
}

/// One story's row.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub story: usize,
    pub title: (f64, f64),
    pub intro: (f64, f64),
    pub nodes: Vec<NodeBox>,
    pub arrows: Vec<Arrow>,
}

/// The whole canvas, in points at zoom 1.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Canvas {
    pub rows: Vec<Row>,
    pub w: f64,
    pub h: f64,
}

/// Lays the stories out: one row per story, its nodes left to right in
/// shot order with the arrows between them, rows stacked. Deterministic
/// from the scripts, so there is nothing to persist and nothing to drag.
pub fn layout(stories: &[Story], m: &Metrics) -> Canvas {
    let text_w = |s: &str, pt: f64| s.chars().count() as f64 * m.adv * pt;
    let title_h = m.line * TITLE_PT;
    let line_h = m.line * TEXT_PT;
    let mut rows = Vec::new();
    let mut y = MARGIN;
    let mut w_max: f64 = 0.0;
    for (si, story) in stories.iter().enumerate() {
        let title = (MARGIN, y);
        let intro = (MARGIN, y + title_h + 6.0);
        let intro_h = story.intro.len() as f64 * line_h;
        // The caption block is as tall as the longest note in the row, so
        // every mount in the row sits on one baseline.
        let notes = story
            .nodes
            .iter()
            .map(|n| n.note.len())
            .max()
            .unwrap_or(0) as f64;
        let caption_h = line_h * (1.0 + notes);
        let top = intro.1 + intro_h + 48.0 + caption_h + CAPTION_GAP;
        let (mw, mh) = story.cfg.window;
        let mut x = MARGIN;
        let mut nodes = Vec::new();
        let mut arrows = Vec::new();
        for (ni, node) in story.nodes.iter().enumerate() {
            if ni > 0 {
                let widest = node
                    .labels
                    .iter()
                    .map(|l| text_w(l, TEXT_PT))
                    .fold(0.0, f64::max);
                let gap = NODE_GAP_MIN.max(widest + LABEL_PAD);
                let from = (x, top + mh / 2.0);
                let to = (x + gap, top + mh / 2.0);
                let block_h = node.labels.len() as f64 * line_h;
                arrows.push(Arrow {
                    from,
                    to,
                    labels_at: (x + (gap - widest) / 2.0, from.1 - 16.0 - block_h),
                    labels: node.labels.clone(),
                });
                x += gap;
            }
            nodes.push(NodeBox {
                node: ni,
                rect: Rect {
                    x,
                    y: top,
                    w: mw,
                    h: mh,
                },
                caption: (x, top - CAPTION_GAP - caption_h),
            });
            x += mw;
        }
        w_max = w_max.max(x + MARGIN).max(
            MARGIN
                + story
                    .intro
                    .iter()
                    .map(|l| text_w(l, TEXT_PT))
                    .fold(text_w(&story.name, TITLE_PT), f64::max)
                + MARGIN,
        );
        rows.push(Row {
            story: si,
            title,
            intro,
            nodes,
            arrows,
        });
        y = top + mh + ROW_GAP;
    }
    Canvas {
        rows,
        w: w_max,
        h: (y - ROW_GAP + MARGIN).max(MARGIN * 2.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCRIPT: &str = "\
# The join/replace journey — the model's whole grammar in one run.
wait 900
shot 01-boot

# A mail row: the message opens to the right, joined.
click \"Q3 infra\"
wait 700
shot 02-message-joined

# Reply opens a compose; typing goes to its body.
click \"reply\"
wait 600
type \"On it\"
wait 400
shot 03-compose
key cmd+w
quit
";

    #[test]
    fn shots_become_nodes_with_notes_and_labels() {
        let s = parse("basic", SCRIPT).unwrap();
        assert_eq!(s.name, "basic");
        assert_eq!(
            s.intro,
            ["The join/replace journey — the model's whole grammar in one run."]
        );
        assert_eq!(s.nodes.len(), 3);
        assert_eq!(s.nodes[0].name, "01-boot");
        assert!(s.nodes[0].note.is_empty());
        assert!(s.nodes[0].labels.is_empty());
        assert_eq!(
            s.nodes[1].note,
            ["A mail row: the message opens to the right, joined."]
        );
        assert_eq!(s.nodes[1].labels, ["click \"Q3 infra\""]);
        assert_eq!(s.nodes[2].labels, ["click \"reply\"", "type \"On it\""]);
        // The steps end at the last shot; the trailing close and quit go.
        assert_eq!(s.steps.len(), s.nodes[2].until + 1);
        assert_eq!(s.steps.last(), Some(&Step::Shot("03-compose".into())));
        assert_eq!(s.steps[s.nodes[1].until], Step::Shot("02-message-joined".into()));
    }

    #[test]
    fn the_header_configures_the_mount() {
        let s = parse(
            "phone",
            "#! window 380x780 grid 4x3\n#! send-delay 1 outside fake\nwait 1\nshot a\n",
        )
        .unwrap();
        assert_eq!(s.cfg.window, (380.0, 780.0));
        assert_eq!(s.cfg.grid, Some(Grid { w: 4, h: 3 }));
        assert_eq!(s.cfg.send_delay, 1.0);
        assert_eq!(s.cfg.outside, OutsideKind::Fake);
        assert!(!s.canvas);
        assert!(!s.shelf);
        assert!(parse("x", "#! canvas\nwait 1\nshot a\n").unwrap().canvas);
        assert!(parse("x", "#! library\nwait 1\nshot a\n").unwrap().shelf);
        assert!(parse("x", "#! outside moon\n").is_err());
        assert!(parse("x", "#! wibble\n").unwrap_err().starts_with("line 1"));
    }

    #[test]
    fn a_bad_step_carries_its_line() {
        let e = parse("x", "wait 1\nfrobnicate\n").unwrap_err();
        assert!(e.starts_with("line 2"), "{e}");
    }

    #[test]
    fn rows_stack_and_nodes_run_left_to_right() {
        let a = parse("a", SCRIPT).unwrap();
        let b = parse("b", "#! window 380x780\nwait 1\nshot p1\nclick \"x\"\nshot p2\n").unwrap();
        let m = Metrics {
            adv: 0.6,
            line: 1.2,
        };
        let c = layout(&[a, b], &m);
        assert_eq!(c.rows.len(), 2);
        let r0 = &c.rows[0];
        assert_eq!(r0.nodes.len(), 3);
        assert_eq!(r0.arrows.len(), 2);
        // Left to right, the gap wide enough for the widest label.
        assert!(r0.nodes[1].rect.x >= r0.nodes[0].rect.x + 1440.0 + NODE_GAP_MIN);
        assert_eq!(r0.arrows[0].from.0, r0.nodes[0].rect.x + 1440.0);
        assert_eq!(r0.arrows[0].to.0, r0.nodes[1].rect.x);
        // Captions sit above their mount.
        assert!(r0.nodes[1].caption.1 < r0.nodes[1].rect.y);
        // The second row is below the first, at the phone's size.
        let r1 = &c.rows[1];
        assert!(r1.nodes[0].rect.y > r0.nodes[0].rect.y + 900.0);
        assert_eq!((r1.nodes[0].rect.w, r1.nodes[0].rect.h), (380.0, 780.0));
        assert!(c.w > r0.nodes[2].rect.x + 1440.0);
        assert!(c.h > r1.nodes[0].rect.y + 780.0);
    }
}
