//! A hit is a labelled rectangle with a cursor shape, nothing more.
//!
//! Shell components register their own into the collector the draw scope
//! carries: a link its text, a button its label, a field its name, a table
//! its rows. The e2e harness resolves a label to its rectangle and
//! synthesizes a real pointer event there; the widget under it handles the
//! click as it would a human's. A panel built from shell components is
//! addressable with no code of its own.
//!
//! Later hits win where they overlap, so a box drawn over rows registers
//! after them.

use std::cell::RefCell;
use std::rc::Rc;

use kernel::layout::SlotId;
use makepad_widgets::*;

/// What a click on a hit means to the shell. Hits a hosted widget
/// registered carry [`Act::Widget`]: the widget answers the press itself,
/// by its own geometry, so a synthesized press lands the way a finger does.
#[derive(Debug, Clone, PartialEq)]
pub enum Act {
    /// The panel-wide rectangle: a click focuses it.
    Focus(SlotId),
    /// The header's close box.
    Close(SlotId),
    /// One entry of a panel's bar, by the verb id it was drawn from. The
    /// verbs are pulled again when it fires: a bar is a view of the
    /// instance, never a copy of it.
    Verb(SlotId, &'static str),
    /// A tab of a tabbed column.
    Tab(SlotId),
    /// A row of the workspaces overlay.
    WsRow(usize),
    /// The workspaces overlay's search row: raise the launcher.
    LauncherOpen,
    /// The launcher's `i`-th visible hit.
    LauncherRow(usize),
    /// A node of the history overlay; `0` is the beginning.
    HistoryRow(i64),
    /// The overlay's backdrop: a tap outside the sheet dismisses it.
    OverlayClose,
    /// The locked screen's button: take the lease. Whether that is a plain
    /// acquire or an override is the driver's to decide.
    Acquire,
    /// The problems mark in the chrome's corner: go to the panel that lists
    /// them.
    Problems,
    /// A surface that absorbs the click and does nothing — the locked
    /// screen's backdrop, which owns every hit while it is up.
    Noop,
    /// A hosted widget's own element. The shell only routes the pointer to
    /// it; what the press means is the widget's business.
    Widget,
}

impl Act {
    /// The slot this act belongs to, where it belongs to one. A hosted
    /// widget's hits sit above the panel-wide `Focus` rect, so a press on
    /// one still names its panel.
    #[must_use]
    pub fn slot(&self) -> Option<SlotId> {
        match self {
            Act::Focus(s) | Act::Close(s) | Act::Verb(s, _) | Act::Tab(s) => Some(*s),
            _ => None,
        }
    }
}

/// One labelled rectangle.
#[derive(Debug, Clone)]
pub struct Hit {
    /// What an e2e script addresses this element by.
    pub label: String,
    pub rect: Rect,
    pub cursor: MouseCursor,
    /// The panel it belongs to, when it belongs to one.
    pub slot: Option<SlotId>,
    pub act: Act,
}

impl Hit {
    /// A hit a hosted widget registers: the shell puts the pointer on it
    /// and the widget does the rest.
    #[must_use]
    pub fn new(label: impl Into<String>, rect: Rect, cursor: MouseCursor, slot: SlotId) -> Hit {
        Hit {
            label: label.into(),
            rect,
            cursor,
            slot: Some(slot),
            act: Act::Widget,
        }
    }

    /// A hit the shell's own chrome registers, with what a click on it
    /// means.
    #[must_use]
    pub fn act(label: impl Into<String>, rect: Rect, cursor: MouseCursor, act: Act) -> Hit {
        Hit {
            label: label.into(),
            rect,
            cursor,
            slot: act.slot(),
            act,
        }
    }
}

/// The collector the draw scope carries. A cheap handle: components hold a
/// clone and register through it while the stage owns the list.
#[derive(Clone, Default)]
pub struct Hits(Rc<RefCell<Vec<Hit>>>);

impl std::fmt::Debug for Hits {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hits").field("n", &self.len()).finish()
    }
}

impl Hits {
    pub fn push(&self, hit: Hit) {
        self.0.borrow_mut().push(hit);
    }

    /// A hosted widget's element, in one line.
    pub fn add(&self, label: impl Into<String>, rect: Rect, cursor: MouseCursor, slot: SlotId) {
        self.push(Hit::new(label, rect, cursor, slot));
    }

    pub fn clear(&self) {
        self.0.borrow_mut().clear();
    }

    /// Drops everything registered after `n` — how a panel that turned out
    /// to be un-hittable (another workspace, a fading tab) takes its hits
    /// back.
    pub fn truncate(&self, n: usize) {
        self.0.borrow_mut().truncate(n);
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.borrow().len()
    }

    /// Nothing drew a rectangle worth clicking — which, past the first
    /// frame, means nothing drew at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.borrow().is_empty()
    }

    /// The hit under a point: the last registered one that contains it.
    #[must_use]
    pub fn at(&self, p: DVec2) -> Option<Hit> {
        self.0
            .borrow()
            .iter()
            .rev()
            .find(|h| h.rect.contains(p))
            .cloned()
    }

    /// The element a script means by `label`: the nearest [`label_rank`]
    /// offers, ties going to the last registered — a control drawn over
    /// another takes the click, the way [`Hits::at`] gives it the point.
    #[must_use]
    pub fn by_label(&self, label: &str) -> Option<Hit> {
        let needle = label.to_lowercase();
        self.0
            .borrow()
            .iter()
            .rev()
            .filter_map(|h| label_rank(h, label, &needle).map(|k| (k, h)))
            .min_by_key(|(k, _)| *k)
            .map(|(_, h)| h.clone())
    }

    /// Every label on offer, in draw order, deduped — what a failing step
    /// prints so a suite says which label to have asked for.
    #[must_use]
    pub fn labels(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for h in self.0.borrow().iter() {
            if !out.iter().any(|l| l == &h.label) {
                out.push(h.label.clone());
            }
        }
        out
    }
}

/// How well a hit answers to what a script asked for, smaller being nearer:
/// the whole label first, then a label that *starts* with what was asked,
/// then one that merely contains it; within a rung a panel's own `Focus`
/// rect yields to anything named, and the **tightest** label wins — the one
/// that says least beyond what was asked. `None` where the label does not
/// match at all.
#[must_use]
pub fn label_rank(h: &Hit, label: &str, needle: &str) -> Option<(u8, u8, usize)> {
    let rung = if h.label.eq_ignore_ascii_case(label) {
        0
    } else {
        let l = h.label.to_lowercase();
        if l.starts_with(needle) {
            1
        } else if l.contains(needle) {
            2
        } else {
            return None;
        }
    };
    let focus = u8::from(matches!(h.act, Act::Focus(_)));
    Some((rung, focus, h.label.chars().count()))
}
