//! In-memory undo and redo tree.
//!
//! A node stores the layout before an action and any [`Intent`] values needed
//! to reverse data changes. If an intent can no longer be reversed, the node
//! expires and history moves past it. Database rows remain durable, but history
//! is lost on restart.

use std::collections::BTreeMap;

use crate::effect::World;
use crate::layout::WmSnap;

/// A node's identity. `0` is the root — "the beginning", before anything.
pub type NodeId = i64;

/// How many actions the tree remembers. Older ones fall off the bottom and
/// the oldest survivor becomes the new root: you cannot undo past it.
const KEEP: usize = 200;

/// Actions of the same kind on the same entity within this window amend the
/// head node instead of growing the tree (a burst of moves is one action).
pub const COALESCE_S: f64 = 2.5;

/// One claim an action made on the world.
pub trait Intent {
    /// One line, for the label and for a status UI.
    fn describe(&self) -> String;

    /// Why this can no longer be given back, if it cannot — *"already
    /// sent"*. Checked for every intent on a node **before** any of them is
    /// reversed, so a node with several claims never half-reverts.
    fn blocked(&self, _w: &World) -> Option<String> {
        None
    }

    /// Give it back.
    ///
    /// # Errors
    ///
    /// If the store refuses the write.
    fn reverse(&self, w: &World) -> Result<(), String>;

    /// Claim it again (redo).
    ///
    /// # Errors
    ///
    /// If the store refuses the write.
    fn reapply(&self, w: &World) -> Result<(), String>;
}

/// Where a node stands relative to the cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Applied,
    Undone,
    /// The world will not take it back — transparent to the walk.
    Expired,
}

impl State {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            State::Applied => "applied",
            State::Undone => "undone",
            State::Expired => "expired",
        }
    }
}

/// One action.
pub struct Node {
    pub parent: NodeId,
    pub kind: String,
    pub label: String,
    /// Coalescing scope, in the `slot:7` / `outbox:9` vocabulary.
    pub entity: Option<String>,
    pub ts: f64,
    /// The layout before the action — what undo restores.
    pub before: WmSnap,
    /// The layout after it — what redo restores.
    pub after: WmSnap,
    pub intents: Vec<Box<dyn Intent>>,
    pub state: State,
}

/// What a walk's failures read as, appended to its toast: nothing at all
/// when a claim gave everything back, which is the ordinary case.
#[must_use]
pub fn said(failed: &[String]) -> String {
    if failed.is_empty() {
        String::new()
    } else {
        format!(" — but {}", failed.join(", "))
    }
}

/// One action, as history is asked to record it.
pub struct Action<'a> {
    pub kind: &'a str,
    pub label: String,
    /// Coalescing scope, in the `slot:7` / `outbox:9` vocabulary.
    pub entity: Option<String>,
    /// The layout before the action, and after it.
    pub before: WmSnap,
    pub after: WmSnap,
    /// What it claimed of the world, if anything.
    pub intents: Vec<Box<dyn Intent>>,
    pub ts: f64,
}

/// A node as the overlay draws it.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub id: NodeId,
    pub parent: NodeId,
    pub ts: f64,
    pub kind: String,
    pub label: String,
    pub state: String,
}

/// What a walk produced: what to say and the layout to restore.
pub struct Step {
    pub label: String,
    pub snap: WmSnap,
    /// Whether the step undid the node or applied it.
    pub undone: bool,
    /// What the node's claims would **not** give back. The layout walks
    /// either way — the snapshot is ours to restore, and stopping the
    /// walk on a half-reversed claim would strand the tree — but a claim
    /// that failed is not a walk that worked, so it travels up to the
    /// shell instead of ending on stderr.
    pub failed: Vec<String>,
}

/// The tree and its cursor.
pub struct History {
    nodes: BTreeMap<NodeId, Node>,
    head: NodeId,
    next: NodeId,
}

impl Default for History {
    fn default() -> Self {
        History::new()
    }
}

impl History {
    #[must_use]
    pub fn new() -> History {
        History {
            nodes: BTreeMap::new(),
            head: 0,
            next: 1,
        }
    }

    #[must_use]
    pub fn head(&self) -> NodeId {
        self.head
    }

    #[must_use]
    pub fn can_undo(&self) -> bool {
        let mut cur = self.head;
        while cur != 0 {
            let Some(n) = self.nodes.get(&cur) else {
                return false;
            };
            if n.state != State::Expired {
                return true;
            }
            cur = n.parent;
        }
        false
    }

    #[must_use]
    pub fn can_redo(&self) -> bool {
        self.newest_undone_child().is_some()
    }

    /// Records an action as a new leaf under the cursor, and moves the
    /// cursor onto it. A same-kind, same-entity action inside
    /// [`COALESCE_S`] amends the head instead — keeping the **earlier**
    /// `before`, so one undo reverts the whole burst.
    pub fn apply(&mut self, a: Action<'_>) -> NodeId {
        let Action {
            kind,
            label,
            entity,
            before,
            after,
            intents,
            ts,
        } = a;
        if let Some(head) = self.nodes.get_mut(&self.head) {
            if entity.is_some()
                && head.entity == entity
                && head.kind == kind
                && head.state == State::Applied
                && (ts - head.ts) < COALESCE_S
            {
                head.after = after;
                head.ts = ts;
                head.intents.extend(intents);
                return self.head;
            }
        }
        let id = self.next;
        self.next += 1;
        self.nodes.insert(
            id,
            Node {
                parent: self.head,
                kind: kind.to_string(),
                label,
                entity,
                ts,
                before,
                after,
                intents,
                state: State::Applied,
            },
        );
        self.head = id;
        self.trim();
        id
    }

    /// Attaches a claim to the node just applied. For the actions whose
    /// claim is only knowable *after* the transaction — a freshly inserted
    /// row's id, say.
    pub fn claim(&mut self, intent: Box<dyn Intent>) {
        if let Some(n) = self.nodes.get_mut(&self.head) {
            n.intents.push(intent);
        }
    }

    /// Undoes the nearest undoable ancestor, walking transparently past any
    /// node the world will not take back. Answers what was undone and the
    /// layout to restore.
    pub fn undo(&mut self, w: &World) -> Option<Step> {
        loop {
            if self.head == 0 {
                return None;
            }
            let id = self.head;
            let (parent, state) = {
                let n = self.nodes.get(&id)?;
                (n.parent, n.state)
            };
            if state == State::Expired {
                self.head = parent;
                continue;
            }
            // Ask every claim first: a node never half-reverts.
            let blocked = {
                let n = self.nodes.get(&id)?;
                n.intents.iter().find_map(|i| i.blocked(w))
            };
            if blocked.is_some() {
                self.nodes.get_mut(&id)?.state = State::Expired;
                self.head = parent;
                continue;
            }
            let failed = {
                let n = self.nodes.get(&id)?;
                let mut failed = Vec::new();
                for i in &n.intents {
                    if let Err(e) = i.reverse(w) {
                        eprintln!("history: reversing {} failed: {e}", i.describe());
                        failed.push(e);
                    }
                }
                failed
            };
            let n = self.nodes.get_mut(&id)?;
            // A claim that would not go back leaves the world somewhere
            // between this node and its parent, and nobody — least of all
            // a redo — can say where. The node is **expired**, not undone:
            // transparent to the walk from here on, exactly as a claim the
            // world refused outright is. The layout still lands, because
            // the snapshot is ours and stranding the tree helps nobody.
            n.state = if failed.is_empty() {
                State::Undone
            } else {
                State::Expired
            };
            let step = Step {
                label: n.label.clone(),
                snap: n.before.clone(),
                undone: true,
                failed,
            };
            self.head = parent;
            return Some(step);
        }
    }

    fn newest_undone_child(&self) -> Option<NodeId> {
        self.nodes
            .iter()
            .filter(|(_, n)| n.parent == self.head && n.state == State::Undone)
            .map(|(id, _)| *id)
            .max()
    }

    /// Redoes the most recent undone child of the cursor — the default
    /// branch.
    pub fn redo(&mut self, w: &World) -> Option<Step> {
        let id = self.newest_undone_child()?;
        let failed = {
            let n = self.nodes.get(&id)?;
            let mut failed = Vec::new();
            for i in &n.intents {
                if let Err(e) = i.reapply(w) {
                    eprintln!("history: reapplying {} failed: {e}", i.describe());
                    failed.push(e);
                }
            }
            failed
        };
        let n = self.nodes.get_mut(&id)?;
        // The same on the way forward: a re-application that failed is not
        // a node the tree may claim to have applied.
        n.state = if failed.is_empty() {
            State::Applied
        } else {
            State::Expired
        };
        let step = Step {
            label: n.label.clone(),
            snap: n.after.clone(),
            undone: false,
            failed,
        };
        self.head = id;
        Some(step)
    }

    /// Walks to any node: undo up to the lowest common ancestor, then
    /// re-apply down the target's branch. `0` is the beginning.
    pub fn travel(&mut self, w: &World, target: NodeId) -> Option<Step> {
        if target == self.head {
            return None;
        }
        if target != 0 && !self.nodes.contains_key(&target) {
            return None;
        }
        let chain = |from: NodeId| -> Vec<NodeId> {
            let mut v = vec![from];
            let mut cur = from;
            while cur != 0 {
                let Some(n) = self.nodes.get(&cur) else { break };
                cur = n.parent;
                v.push(cur);
            }
            v
        };
        let hc = chain(self.head);
        let tc = chain(target);
        let lca = *hc.iter().find(|id| tc.contains(id))?;

        // What every leg of the walk could not give back, kept for the one
        // step that comes out of it.
        let mut walked: Vec<String> = Vec::new();
        while self.head != lca {
            let before = self.head;
            match self.undo(w) {
                Some(step) => walked.extend(step.failed),
                None if self.head == before => break, // nothing left to walk
                None => {}
            }
        }

        let down: Vec<NodeId> = tc.iter().take_while(|&&id| id != lca).copied().collect();
        let mut last = None;
        for &id in down.iter().rev() {
            let expired = self
                .nodes
                .get(&id)
                .is_some_and(|n| n.state == State::Expired);
            if expired {
                // Its effects never left; walking past it is the honest move.
                self.head = id;
                continue;
            }
            let failed = {
                let Some(n) = self.nodes.get(&id) else {
                    continue;
                };
                let mut failed = Vec::new();
                for i in &n.intents {
                    if let Err(e) = i.reapply(w) {
                        eprintln!("history: reapplying {} failed: {e}", i.describe());
                        failed.push(e);
                    }
                }
                failed
            };
            let Some(n) = self.nodes.get_mut(&id) else {
                continue;
            };
            n.state = if failed.is_empty() {
                State::Applied
            } else {
                State::Expired
            };
            walked.extend(failed);
            last = Some(Step {
                label: n.label.clone(),
                snap: n.after.clone(),
                undone: false,
                failed: Vec::new(),
            });
            self.head = id;
        }
        if target == 0 {
            // Undoing everything: the layout is the oldest node's `before`.
            let snap = self
                .nodes
                .values()
                .min_by_key(|n| n.ts as i64)
                .map(|n| n.before.clone())
                .unwrap_or_default();
            self.head = 0;
            return Some(Step {
                label: "the beginning".into(),
                snap,
                undone: true,
                failed: walked,
            });
        }
        // Every leg's failures land on the one step the shell sees: a
        // travel is one move as far as anyone watching is concerned, and a
        // reversal that would not go halfway up must not be swallowed by
        // the legs after it.
        let mut step = last.or_else(|| {
            // A travel that only walked *up* has no node to re-apply, and
            // it has still moved the head: the landing is where it stopped.
            let n = self.nodes.get(&self.head)?;
            Some(Step {
                label: n.label.clone(),
                snap: n.after.clone(),
                undone: false,
                failed: Vec::new(),
            })
        })?;
        step.failed = walked;
        Some(step)
    }

    /// The whole tree plus the cursor — what the overlay draws.
    #[must_use]
    pub fn rows(&self) -> (Vec<Row>, NodeId) {
        let rows = self
            .nodes
            .iter()
            .map(|(id, n)| Row {
                id: *id,
                parent: n.parent,
                ts: n.ts,
                kind: n.kind.clone(),
                label: n.label.clone(),
                state: n.state.as_str().to_string(),
            })
            .collect();
        (rows, self.head)
    }

    /// Bounded: the oldest nodes fall off and whatever survives them becomes
    /// a root. A dropped node's `before` goes with it, which is exactly why
    /// you cannot undo past the floor.
    fn trim(&mut self) {
        while self.nodes.len() > KEEP {
            let Some(oldest) = self.nodes.keys().next().copied() else {
                return;
            };
            self.nodes.remove(&oldest);
            for n in self.nodes.values_mut() {
                if n.parent == oldest {
                    n.parent = 0;
                }
            }
            if self.head == oldest {
                self.head = 0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::{Registry, World};
    use crate::layout::Wm;
    use crate::panel::{PanelId, Tag};
    use std::cell::RefCell;
    use std::rc::Rc;

    /// A claim that records what was done to it, and can be told to refuse.
    struct Spy {
        log: Rc<RefCell<Vec<String>>>,
        name: &'static str,
        blocked: bool,
        /// A claim that lets itself be tried and then cannot do it — the
        /// disk moved under a reversal `blocked` had already passed.
        breaks: bool,
    }

    impl Intent for Spy {
        fn describe(&self) -> String {
            self.name.into()
        }
        fn blocked(&self, _w: &World) -> Option<String> {
            self.blocked.then(|| "already sent".to_string())
        }
        fn reverse(&self, _w: &World) -> Result<(), String> {
            self.log.borrow_mut().push(format!("-{}", self.name));
            if self.breaks {
                return Err(format!("{} would not go back", self.name));
            }
            Ok(())
        }
        fn reapply(&self, _w: &World) -> Result<(), String> {
            self.log.borrow_mut().push(format!("+{}", self.name));
            if self.breaks {
                return Err(format!("{} would not go again", self.name));
            }
            Ok(())
        }
    }

    /// The positional form the tests read better in.
    #[allow(clippy::too_many_arguments)]
    fn act(
        h: &mut History,
        kind: &str,
        label: String,
        entity: Option<String>,
        before: WmSnap,
        after: WmSnap,
        intents: Vec<Box<dyn Intent>>,
        ts: f64,
    ) -> NodeId {
        h.apply(Action {
            kind,
            label,
            entity,
            before,
            after,
            intents,
            ts,
        })
    }

    fn help() -> PanelId {
        PanelId::bare(Tag("help"))
    }
    fn about() -> PanelId {
        PanelId::bare(Tag("about"))
    }
    fn inbox() -> PanelId {
        PanelId::bare(Tag("inbox"))
    }

    fn snap(shows: &[PanelId]) -> WmSnap {
        let mut wm = Wm::new();
        for s in shows {
            wm.open(s.clone(), None, false);
        }
        wm.snapshot()
    }

    fn world() -> World {
        World::fake(Registry::new())
    }

    /// The walk: leaves under the cursor, undo restores `before`, redo
    /// restores `after`, and acting mid-tree branches without losing
    /// anything.
    #[test]
    fn the_tree_walks_and_branches() {
        let w = world();
        let mut h = History::new();
        let a = snap(&[help()]);
        let b = snap(&[help(), about()]);
        act(
            &mut h,
            "open",
            "open help".into(),
            None,
            WmSnap::default(),
            a.clone(),
            vec![],
            1.0,
        );
        act(
            &mut h,
            "open",
            "open about".into(),
            None,
            a.clone(),
            b.clone(),
            vec![],
            2.0,
        );

        assert_eq!(
            h.undo(&w).map(|s| (s.label, s.snap)),
            Some(("open about".into(), a.clone()))
        );
        assert_eq!(h.undo(&w).map(|s| s.snap), Some(WmSnap::default()));
        assert!(h.undo(&w).is_none(), "nothing left");
        assert!(!h.can_undo());

        assert_eq!(h.redo(&w).map(|s| s.snap), Some(a.clone()));
        // A new action mid-tree branches; redo prefers the newer branch.
        let fork = snap(&[help(), inbox()]);
        act(
            &mut h,
            "open",
            "open inbox".into(),
            None,
            a.clone(),
            fork.clone(),
            vec![],
            3.0,
        );
        h.undo(&w);
        assert_eq!(h.redo(&w).map(|s| s.label), Some("open inbox".into()));

        let (rows, head) = h.rows();
        assert_eq!(rows.len(), 3, "the abandoned branch stays");
        assert_eq!(head, 3);
        assert_eq!(rows[1].parent, 1, "about and inbox share a parent");
        assert_eq!(rows[1].state, "undone");
    }

    /// Claims are given back on undo and re-made on redo, in that order.
    #[test]
    fn claims_are_reversed_and_remade() {
        let w = world();
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut h = History::new();
        act(
            &mut h,
            "archive",
            "archive".into(),
            None,
            WmSnap::default(),
            snap(&[help()]),
            vec![Box::new(Spy {
                log: log.clone(),
                name: "archive",
                blocked: false,
                breaks: false,
            })],
            1.0,
        );
        h.undo(&w);
        h.redo(&w);
        assert_eq!(*log.borrow(), vec!["-archive", "+archive"]);
    }

    /// A claim that fails *while* it is being given back — `blocked` let
    /// it through and the disk moved under it — is not a walk that
    /// worked. The layout still lands (the snapshot is ours, and stopping
    /// here would strand the tree), but the failure travels up with the
    /// step instead of ending on stderr.
    #[test]
    fn a_reversal_that_fails_says_so_on_the_step() {
        let w = world();
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut h = History::new();
        act(
            &mut h,
            "delete",
            "delete “notes.txt”".into(),
            None,
            WmSnap::default(),
            snap(&[help()]),
            vec![Box::new(Spy {
                log: log.clone(),
                name: "delete",
                blocked: false,
                breaks: true,
            })],
            1.0,
        );
        let step = h.undo(&w).expect("the layout walks either way");
        assert_eq!(step.label, "delete “notes.txt”");
        assert_eq!(step.failed, ["delete would not go back"]);
        assert_eq!(said(&step.failed), " — but delete would not go back");
        // …and the node is expired, not undone: the world is somewhere
        // between it and its parent, and a redo may not pretend to know
        // where. The walk goes past it from here on.
        assert_eq!(h.rows().0[0].state, "expired");
        assert!(h.redo(&w).is_none(), "nothing to re-apply");
        assert!(!h.can_undo(), "and nothing behind it either");
        // A walk that gave everything back says nothing extra.
        assert_eq!(said(&[]), "");
    }

    /// A claim the world will not take back makes its node transparent —
    /// the walk marks it and undoes the one before instead. It must never
    /// be a barrier, and never a silent lie.
    #[test]
    fn an_irreversible_claim_is_transparent_not_a_barrier() {
        let w = world();
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut h = History::new();
        let a = snap(&[help()]);
        act(
            &mut h,
            "open",
            "open help".into(),
            None,
            WmSnap::default(),
            a.clone(),
            vec![Box::new(Spy {
                log: log.clone(),
                name: "open",
                blocked: false,
                breaks: false,
            })],
            1.0,
        );
        act(
            &mut h,
            "send",
            "send “Hi”".into(),
            Some("outbox:9".into()),
            a.clone(),
            snap(&[help(), about()]),
            vec![Box::new(Spy {
                log: log.clone(),
                name: "send",
                blocked: true,
                breaks: false,
            })],
            2.0,
        );

        // cmd+z walks past the send and undoes the open underneath it.
        let step = h.undo(&w).expect("something undoable below");
        assert_eq!(step.label, "open help");
        assert_eq!(*log.borrow(), vec!["-open"], "the send was never reversed");
        let (rows, head) = h.rows();
        assert_eq!(rows[1].state, "expired");
        assert_eq!(head, 0);
    }

    /// Same kind and entity inside the window amend the head, keeping the
    /// earliest `before`, so one undo reverts the whole burst.
    #[test]
    fn a_burst_coalesces_into_one_step() {
        let w = world();
        let mut h = History::new();
        let a = snap(&[help()]);
        let b = snap(&[help(), about()]);
        act(
            &mut h,
            "move",
            "move".into(),
            Some("slot:1".into()),
            WmSnap::default(),
            a.clone(),
            vec![],
            10.0,
        );
        act(
            &mut h,
            "move",
            "move".into(),
            Some("slot:1".into()),
            a,
            b.clone(),
            vec![],
            11.0,
        );
        assert_eq!(h.rows().0.len(), 1, "the burst is one node");

        act(
            &mut h,
            "move",
            "move".into(),
            Some("slot:1".into()),
            b.clone(),
            snap(&[help()]),
            vec![],
            99.0,
        );
        assert_eq!(h.rows().0.len(), 2, "past the window, a new node");

        h.undo(&w);
        assert_eq!(
            h.undo(&w).map(|s| s.snap),
            Some(WmSnap::default()),
            "one undo for the burst"
        );
    }

    /// Travel crosses branches and reaches the beginning.
    #[test]
    fn travel_walks_branches() {
        let w = world();
        let mut h = History::new();
        let a = snap(&[help()]);
        let b = snap(&[help(), about()]);
        act(
            &mut h,
            "open",
            "open help".into(),
            None,
            WmSnap::default(),
            a.clone(),
            vec![],
            1.0,
        );
        act(
            &mut h,
            "open",
            "open about".into(),
            None,
            a.clone(),
            b.clone(),
            vec![],
            2.0,
        );
        h.undo(&w);
        let fork = snap(&[help(), inbox()]);
        act(
            &mut h,
            "open",
            "open inbox".into(),
            None,
            a.clone(),
            fork.clone(),
            vec![],
            3.0,
        );

        assert_eq!(h.travel(&w, 2).map(|s| s.snap), Some(b), "across the fork");
        assert_eq!(
            h.travel(&w, 0).map(|s| s.label),
            Some("the beginning".into())
        );
        assert_eq!(h.travel(&w, 3).map(|s| s.snap), Some(fork));
        assert!(h.travel(&w, 3).is_none(), "already there");
    }

    /// The tree is bounded: past the floor, older actions are simply gone.
    #[test]
    fn the_tree_is_bounded() {
        let mut h = History::new();
        for i in 0..(KEEP + 25) {
            act(
                &mut h,
                "open",
                format!("open {i}"),
                None,
                WmSnap::default(),
                WmSnap::default(),
                vec![],
                i as f64,
            );
        }
        let (rows, _) = h.rows();
        assert_eq!(rows.len(), KEEP);
        assert_eq!(rows[0].label, "open 25", "the oldest fell off");
        assert_eq!(rows[0].parent, 0, "and its survivor became a root");
    }
}
