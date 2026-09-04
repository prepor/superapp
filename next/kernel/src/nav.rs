//! Where a click goes, decided in the kernel.
//!
//! The join and replace rules, the preview's focus rule, and the history
//! kind and coalescing are applied here; the shell animates the result.

use std::cell::Cell;
use std::rc::Rc;

use crate::layout::SlotId;
use crate::panel::{slot_entity, Open, PanelId};
use crate::session::{Action, Session, Write};

/// An intent to change what a slot shows or which slot has focus. The join
/// and replace rules, the preview's focus rule, and the history kind and
/// coalescing are applied by the kernel; the shell animates the result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Nav {
    /// A new slot joined to `from`, or un-joined with `fresh`; the new
    /// panel takes focus.
    Open {
        from: SlotId,
        id: PanelId,
        fresh: bool,
    },
    /// `slot` opens `id` in place. Its joined descendants close.
    Replace {
        slot: SlotId,
        id: PanelId,
    },
    /// A new slot joined to `from`; focus stays where it was, and the camera
    /// shows the child once. Where the pair cannot share the screen, the
    /// open simply goes.
    Preview {
        from: SlotId,
        id: PanelId,
    },
    Close(SlotId),
    Focus(SlotId),
}

impl Nav {
    /// How the panel this navigation opens is being opened; `None` for the
    /// two that open nothing.
    #[must_use]
    pub fn how(&self) -> Option<Open> {
        match self {
            Nav::Open { .. } => Some(Open::Open),
            Nav::Replace { .. } => Some(Open::Replace),
            Nav::Preview { .. } => Some(Open::Preview),
            Nav::Close(_) | Nav::Focus(_) => None,
        }
    }

    /// The slot the navigation starts from, which is what it coalesces by.
    #[must_use]
    pub fn from(&self) -> SlotId {
        match self {
            Nav::Open { from, .. } | Nav::Preview { from, .. } => *from,
            Nav::Replace { slot, .. } => *slot,
            Nav::Close(slot) | Nav::Focus(slot) => *slot,
        }
    }
}

impl Session {
    /// Applies a navigation.
    ///
    /// An `Open` follows [`Ws::follow_open`](crate::layout::Ws::follow_open)
    /// and takes focus; a `Replace` follows
    /// [`Ws::follow_replace`](crate::layout::Ws::follow_replace); a
    /// `Preview` keeps focus where it was unless the pair cannot share the
    /// screen, activates the child's tab, and asks the camera to show the
    /// child once. `Close` closes the slot and its joined chain. `Focus` is
    /// not an action: nothing is claimed, so there is nothing to undo.
    ///
    /// The history kind is `open` for an open, `read` for a preview and for
    /// a replace whose new instance claimed something of the world, and
    /// `close` for a close — coalescing per originating slot, so a cursor
    /// walk that previews a row at a time is one undo that closes the whole
    /// walk.
    pub fn nav(&mut self, n: Nav) {
        match n {
            Nav::Focus(slot) => {
                if self.focus_slot(slot) {
                    self.unsettle();
                }
            }
            Nav::Close(slot) => {
                let label = format!("close {}", self.shows(slot));
                self.act(
                    Action::new("close", label)
                        .about(slot_entity(slot))
                        .moving(move |wm| wm.close(slot)),
                );
            }
            Nav::Open { from, id, fresh } => self.open_into(from, id, Open::Open, fresh),
            Nav::Replace { slot, id } => self.open_into(slot, id, Open::Replace, false),
            Nav::Preview { from, id } => self.open_into(from, id, Open::Preview, false),
        }
    }

    /// What a slot shows, as an action labels it.
    ///
    /// The identity rather than the instance's title: `nav` touches no
    /// instance, so that a panel may navigate while its own verb is still
    /// running as `&mut self`.
    fn shows(&self, slot: SlotId) -> String {
        self.ws()
            .slot(slot)
            .map(|s| s.show.to_string())
            .unwrap_or_else(|| "panel".into())
    }

    /// The three navigations that open an instance. The instance is built
    /// first, with an [`Opening`](crate::panel::Opening) that says how, and
    /// its claims travel into the action — so a mail marked read on open
    /// lands on the same undoable node as the layout change.
    fn open_into(&mut self, from: SlotId, id: PanelId, how: Open, fresh: bool) {
        let (instance, claimed) = self.open_instance(&id, how);
        let claimed_anything = !claimed.is_empty();
        let label = {
            let title = instance.title();
            match how {
                Open::Open | Open::Restore => format!("open “{title}”"),
                Open::Replace => format!("go to “{title}”"),
                Open::Preview => format!("read “{title}”"),
            }
        };
        // `read` is what a cursor walk records, so a burst of previews from
        // one slot is one node; a replace that claimed something of the
        // world is the same act by another gesture.
        let kind = match how {
            Open::Preview => "read",
            Open::Replace if claimed_anything => "read",
            _ => "open",
        };

        let (writes, intents): (Vec<Write>, Vec<Vec<Box<dyn crate::history::Intent>>>) =
            claimed.into_iter().unzip();
        let intents: Vec<Box<dyn crate::history::Intent>> = intents.into_iter().flatten().collect();

        // The layout half answers which slot it landed in; the camera and
        // the tab need it, and it is not known until the layout has run.
        let landed: Rc<Cell<Option<SlotId>>> = Rc::new(Cell::new(None));
        let out = landed.clone();
        let (viewport, opts) = (self.viewport(), self.opts());
        let show = id.clone();

        self.place(id, instance);
        self.act(
            Action::writing(kind, label, move |tx| {
                for w in writes {
                    w(tx)?;
                }
                Ok(())
            })
            .about(slot_entity(from))
            .claiming(intents)
            .moving(move |wm| {
                let was = wm.focus;
                let slot = match how {
                    Open::Replace => wm.follow_replace(from, show, false),
                    _ => wm.follow_open(from, show, fresh),
                };
                if how == Open::Preview {
                    // Focus stays behind only where the pair fits on one
                    // screen. On a phone grid each of them is the whole screen,
                    // so an open that kept focus behind would just look like
                    // nothing happened.
                    if wm.fit_together(from, slot, viewport, opts) {
                        wm.focus = was;
                    }
                    // A slot opened without focus would otherwise land as a
                    // hidden tab and draw at alpha 0.
                    wm.activate(slot);
                }
                out.set(Some(slot));
            }),
        );

        if let Some(slot) = landed.get() {
            if how == Open::Preview {
                self.show_camera_at(slot);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{App, Root};
    use crate::history::Intent;
    use crate::panel::{Opening, Panel, PanelKind, Tag, Verb};
    use crate::store::Store;
    use std::any::Any;

    // -- a tiny app: a list, and a card that claims on open ------------------

    const LIST: Tag = Tag("list");
    const CARD: Tag = Tag("card");

    fn list() -> PanelId {
        PanelId::bare(LIST)
    }
    fn card(n: i64) -> PanelId {
        PanelId::new(CARD, [n.to_string()])
    }

    struct ListPanel(PanelId);
    impl Panel for ListPanel {
        fn id(&self) -> &PanelId {
            &self.0
        }
        fn title(&self) -> String {
            "list".into()
        }
        fn wish(&self, _cols: usize) -> (u32, u32) {
            (4, 6)
        }
        fn as_any(&mut self) -> &mut dyn Any {
            self
        }
    }

    struct ListKind;
    impl PanelKind for ListKind {
        fn tag(&self) -> Tag {
            LIST
        }
        fn open(&self, id: &PanelId, _cx: &mut Opening<'_>) -> Box<dyn Panel> {
            Box::new(ListPanel(id.clone()))
        }
    }

    /// The card marks itself read on open, exactly as a message does.
    struct CardPanel(PanelId);
    impl Panel for CardPanel {
        fn id(&self) -> &PanelId {
            &self.0
        }
        fn title(&self) -> String {
            format!("card {}", self.0.arg(0).unwrap_or(""))
        }
        fn wish(&self, _cols: usize) -> (u32, u32) {
            (4, 3)
        }
        fn verbs(&self) -> Vec<Verb> {
            vec![Verb::run("demo.nothing", "nothing", None)]
        }
        fn as_any(&mut self) -> &mut dyn Any {
            self
        }
    }

    /// *The card was read* — reversed by putting the flag back.
    struct Read(i64);
    impl Intent for Read {
        fn describe(&self) -> String {
            format!("read card {}", self.0)
        }
        fn reverse(&self, w: &crate::effect::World) -> Result<(), String> {
            let n = self.0;
            w.store()
                .write(move |tx| {
                    tx.execute("UPDATE card SET seen = 0 WHERE id = ?1", [n])
                        .map(|_| ())
                })
                .map_err(|e| e.to_string())
        }
        fn reapply(&self, w: &crate::effect::World) -> Result<(), String> {
            let n = self.0;
            w.store()
                .write(move |tx| {
                    tx.execute("UPDATE card SET seen = 1 WHERE id = ?1", [n])
                        .map(|_| ())
                })
                .map_err(|e| e.to_string())
        }
    }

    struct CardKind;
    impl PanelKind for CardKind {
        fn tag(&self) -> Tag {
            CARD
        }
        fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
            if let Some(n) = id.arg(0).and_then(|a| a.parse::<i64>().ok()) {
                cx.claim(
                    Box::new(move |tx: &rusqlite::Transaction| {
                        tx.execute("UPDATE card SET seen = 1 WHERE id = ?1", [n])
                            .map(|_| ())
                    }),
                    vec![Box::new(Read(n))],
                );
            }
            Box::new(CardPanel(id.clone()))
        }
    }

    static LIST_KIND: ListKind = ListKind;
    static CARD_KIND: CardKind = CardKind;
    static KINDS: &[&dyn PanelKind] = &[&LIST_KIND, &CARD_KIND];

    static SCHEMA: crate::app::Schema = crate::app::Schema {
        app: "demo",
        steps: &[crate::app::Step::Sql(
            "CREATE TABLE card(id INTEGER PRIMARY KEY, seen INTEGER NOT NULL DEFAULT 0)",
        )],
    };

    struct Demo;
    impl App for Demo {
        fn id(&self) -> &'static str {
            "demo"
        }
        fn kinds(&self) -> &'static [&'static dyn PanelKind] {
            KINDS
        }
        fn schema(&self) -> Option<&'static crate::app::Schema> {
            Some(&SCHEMA)
        }
        fn seed(&self, store: &Store) -> rusqlite::Result<()> {
            store.write(|tx| {
                for i in 1..=5 {
                    tx.execute("INSERT OR IGNORE INTO card(id, seen) VALUES(?1, 0)", [i])?;
                }
                Ok(())
            })
        }
        fn roots(&self) -> Vec<Root> {
            vec![Root::new(list(), "list", "rows")]
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }
    static DEMO: Demo = Demo;
    static APPS: &[&dyn App] = &[&DEMO];

    fn session() -> (Session, SlotId) {
        let mut s = Session::fake(APPS);
        let root = s.act(Action::new("open", "open “list”").moving({
            let id = list();
            move |wm| {
                wm.open(id, None, false);
            }
        }));
        assert!(root.is_some());
        s.settle();
        let slot = s.focus().expect("the list is focused");
        (s, slot)
    }

    /// A navigation, settled — what the shell does after every event, and
    /// what a test does before it looks at the slots.
    fn go(s: &mut Session, n: Nav) {
        s.nav(n);
        s.settle();
    }

    fn seen(s: &Session, n: i64) -> i64 {
        s.store()
            .conn()
            .query_row("SELECT seen FROM card WHERE id = ?1", [n], |r| r.get(0))
            .unwrap_or(-1)
    }

    fn kinds(s: &Session) -> Vec<String> {
        s.history().rows().0.into_iter().map(|r| r.kind).collect()
    }

    /// An open joins a new slot to the one it came from and takes focus.
    #[test]
    fn open_joins_and_takes_focus() {
        let (mut s, list_slot) = session();
        go(&mut s, Nav::Open {
            from: list_slot,
            id: card(1),
            fresh: false,
        });
        let child = s.joined_child(list_slot).expect("a joined child");
        assert_eq!(s.focus(), Some(child), "the new panel takes focus");
        assert_eq!(s.panel(child).unwrap().borrow().title(), "card 1");
        assert_eq!(s.showing(&card(1)), vec![child]);
        assert_eq!(kinds(&s), vec!["open".to_string(), "open".to_string()]);

        // A second open from the same slot re-targets the joined child.
        go(&mut s, Nav::Open {
            from: list_slot,
            id: card(2),
            fresh: false,
        });
        assert_eq!(s.joined_child(list_slot), Some(child), "the same slot");
        assert_eq!(s.panel(child).unwrap().borrow().title(), "card 2");

        // …and `fresh` opens a slot of its own instead.
        go(&mut s, Nav::Open {
            from: list_slot,
            id: card(3),
            fresh: true,
        });
        assert_eq!(s.joined_child(list_slot), Some(child), "the join survived");
        assert_eq!(s.panels().len(), 3);
    }

    /// A replace shows something else in the same slot and closes the chain
    /// under it.
    #[test]
    fn replace_swaps_in_place_and_closes_the_chain() {
        let (mut s, list_slot) = session();
        go(&mut s, Nav::Open {
            from: list_slot,
            id: card(1),
            fresh: false,
        });
        let child = s.joined_child(list_slot).unwrap();
        go(&mut s, Nav::Open {
            from: child,
            id: card(2),
            fresh: false,
        });
        let grandchild = s.joined_child(child).unwrap();
        assert_eq!(s.panels().len(), 3);

        go(&mut s, Nav::Replace {
            slot: child,
            id: card(4),
        });
        assert_eq!(s.panel(child).unwrap().borrow().title(), "card 4");
        assert!(s.panel(grandchild).is_none(), "the chain went with it");
        assert_eq!(s.panels().len(), 2);
        assert_eq!(s.focus(), Some(child));
    }

    /// A preview leaves focus where it was, activates the child's tab, and
    /// asks the camera to show it once.
    #[test]
    fn preview_keeps_focus_and_shows_the_child_once() {
        let (mut s, list_slot) = session();
        go(&mut s, Nav::Preview {
            from: list_slot,
            id: card(1),
        });
        let child = s.joined_child(list_slot).expect("a joined child");
        assert_eq!(s.focus(), Some(list_slot), "focus stayed behind");
        assert_eq!(s.take_show_once(), Some(child), "the camera was asked once");
        assert_eq!(s.take_show_once(), None, "and only once");

        // On a grid where the pair cannot share the screen, the open goes.
        let mut s = Session::fake(APPS);
        s.set_viewport((400.0, 700.0));
        s.act(Action::new("grid", "fold").moving(|wm| {
            wm.set_grid(crate::layout::Grid { w: 4, h: 3 });
        }));
        let root = s.act(Action::new("open", "open “list”").moving(|wm| {
            wm.open(PanelId::bare(LIST), None, false);
        }));
        assert!(root.is_some());
        s.settle();
        let list_slot = s.focus().unwrap();
        go(&mut s, Nav::Preview {
            from: list_slot,
            id: card(1),
        });
        let child = s.joined_child(list_slot).unwrap();
        assert_eq!(s.focus(), Some(child), "the pair cannot share the screen");
    }

    /// The history kinds, and what coalesces: a walk of previews from one
    /// slot is one node, and an open beside it is another.
    #[test]
    fn a_walk_of_previews_is_one_node() {
        let (mut s, list_slot) = session();
        assert_eq!(kinds(&s), vec!["open".to_string()]);

        for n in 1..=4 {
            go(&mut s, Nav::Preview {
                from: list_slot,
                id: card(n),
            });
        }
        assert_eq!(
            kinds(&s),
            vec!["open".to_string(), "read".to_string()],
            "four previews from one slot are one node"
        );

        // An open from the same slot is a different kind: a new node.
        go(&mut s, Nav::Open {
            from: list_slot,
            id: card(5),
            fresh: true,
        });
        assert_eq!(kinds(&s).len(), 3);

        // A preview from *another* slot is another entity: another node.
        let other = s.panels()[1].0;
        go(&mut s, Nav::Preview {
            from: other,
            id: card(1),
        });
        assert_eq!(kinds(&s).len(), 4);
    }

    /// What an open claims of the world lands on the same node as the
    /// layout change, and undo gives it back.
    #[test]
    fn undo_reverses_what_the_open_claimed() {
        let (mut s, list_slot) = session();
        assert_eq!(seen(&s, 1), 0);
        go(&mut s, Nav::Open {
            from: list_slot,
            id: card(1),
            fresh: false,
        });
        assert_eq!(seen(&s, 1), 1, "the open claimed it, in the action");
        let child = s.joined_child(list_slot).unwrap();

        assert!(s.undo());
        assert_eq!(seen(&s, 1), 0, "and undo gave it back");
        assert!(s.panel(child).is_none(), "with the slot it opened");

        assert!(s.redo());
        assert_eq!(seen(&s, 1), 1, "redo claims it again");
        assert!(s.joined_child(list_slot).is_some());
    }

    /// A replace whose instance claimed something is a `read`; one that
    /// claimed nothing is an `open`.
    #[test]
    fn a_claiming_replace_is_a_read() {
        let (mut s, list_slot) = session();
        go(&mut s, Nav::Open {
            from: list_slot,
            id: card(1),
            fresh: false,
        });
        let child = s.joined_child(list_slot).unwrap();
        go(&mut s, Nav::Replace {
            slot: child,
            id: card(2),
        });
        assert_eq!(kinds(&s).last().map(String::as_str), Some("read"));

        // The list claims nothing on open, so replacing into one is an open.
        go(&mut s, Nav::Replace {
            slot: child,
            id: list(),
        });
        assert_eq!(kinds(&s).last().map(String::as_str), Some("open"));
    }

    /// A close takes the slot and its chain, and is its own kind.
    #[test]
    fn close_takes_the_chain_and_records_a_node() {
        let (mut s, list_slot) = session();
        go(&mut s, Nav::Open {
            from: list_slot,
            id: card(1),
            fresh: false,
        });
        assert_eq!(s.panels().len(), 2);
        go(&mut s, Nav::Close(list_slot));
        assert_eq!(s.panels().len(), 0, "the chain went with it");
        assert_eq!(kinds(&s).last().map(String::as_str), Some("close"));
    }

    /// A close moves focus off the chain it took: focus sitting on a joined
    /// descendant lands on a slot that is still open, never on a dead id.
    #[test]
    fn close_moves_focus_off_the_chain_it_took() {
        let (mut s, list_slot) = session();
        // One panel opened for its own sake — nobody's context, so it stays.
        go(&mut s, Nav::Open {
            from: list_slot,
            id: card(5),
            fresh: true,
        });
        let other = s.focus().expect("the fresh slot took focus");

        go(&mut s, Nav::Open {
            from: list_slot,
            id: card(1),
            fresh: false,
        });
        let child = s.joined_child(list_slot).expect("a joined child");
        go(&mut s, Nav::Open {
            from: child,
            id: card(2),
            fresh: false,
        });
        let grandchild = s.joined_child(child).expect("a grandchild");
        assert_eq!(s.focus(), Some(grandchild), "focus is deep in the chain");

        go(&mut s, Nav::Close(list_slot));
        assert!(s.panel(child).is_none() && s.panel(grandchild).is_none());
        assert_eq!(s.panels().len(), 1, "only what was opened for itself");
        let f = s.focus().expect("focus landed somewhere");
        assert_eq!(f, other, "on what survived");
        assert!(s.panel(f).is_some(), "which is a slot that is still open");
    }

    /// A verb's action closes its own slot with `wm.close`, and that reaches
    /// the workspace the panel is on rather than the one being looked at.
    #[test]
    fn a_verb_closes_its_slot_on_another_workspace() {
        let (mut s, list_slot) = session();
        go(&mut s, Nav::Open {
            from: list_slot,
            id: card(1),
            fresh: false,
        });
        let child = s.joined_child(list_slot).expect("a joined child");
        assert!(s.switch(1), "and now look somewhere else");

        // What a verb whose panel lost what it showed says, from its action.
        s.act(Action::new("close", "close “list”").moving(move |wm| wm.close(list_slot)));
        s.settle();

        assert!(s.panel(list_slot).is_none(), "the slot went");
        assert!(s.panel(child).is_none(), "and its chain with it");
        assert_eq!(s.panels().len(), 0);
        assert_eq!(s.ws().active, 1, "the close did not move anybody");
    }

    /// Focus is not an action: nothing was claimed, so there is nothing to
    /// undo.
    #[test]
    fn focus_is_not_an_action() {
        let (mut s, list_slot) = session();
        go(&mut s, Nav::Open {
            from: list_slot,
            id: card(1),
            fresh: true,
        });
        let before = s.history().rows().0.len();
        go(&mut s, Nav::Focus(list_slot));
        assert_eq!(s.focus(), Some(list_slot));
        assert_eq!(s.history().rows().0.len(), before, "no node");
        // …and going where you already are changes nothing either.
        go(&mut s, Nav::Focus(list_slot));
        assert_eq!(s.history().rows().0.len(), before);
    }

    /// `Nav` says what it is about, which is what the routing reads.
    #[test]
    fn a_nav_names_its_slot_and_its_open() {
        assert_eq!(
            Nav::Open {
                from: 3,
                id: list(),
                fresh: false
            }
            .how(),
            Some(Open::Open)
        );
        assert_eq!(
            Nav::Preview {
                from: 3,
                id: list()
            }
            .how(),
            Some(Open::Preview)
        );
        assert_eq!(
            Nav::Replace {
                slot: 3,
                id: list()
            }
            .how(),
            Some(Open::Replace)
        );
        assert_eq!(Nav::Close(3).how(), None);
        assert_eq!(Nav::Focus(3).how(), None);
        assert_eq!(Nav::Close(7).from(), 7);
        assert_eq!(
            Nav::Preview {
                from: 7,
                id: list()
            }
            .from(),
            7
        );
    }
}
