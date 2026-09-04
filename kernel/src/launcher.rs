//! Combines open panels, roots, and search providers into the launcher list.
//!
//! Open results become [`Go::Focus`]; other results become [`Go::Open`]. Open
//! panels and roots are available immediately. Provider results arrive later
//! through [`crate::search::Engine`] without reordering earlier source groups.
//!
//! Nothing here reads a store or a layout: the instant half is a slice of
//! open slots and a slice of roots, both handed over by the session.

use crate::app::Root;
use crate::layout::SlotId;
use crate::panel::PanelId;
use crate::search::{self, Answer, Engine, Go, Hit};
use crate::store::Store;

/// How many rows the list is allowed to reach. A palette is refined by
/// typing, not scrolled, and every row past a screenful costs a widget on
/// every frame the overlay draws.
const MAX_HITS: usize = 200;

/// One open panel, as the launcher lists it: where it is, what it shows,
/// and what its instance calls itself. The session builds these — it is the
/// one thing that has both the layout and the instances.
#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    pub slot: SlotId,
    /// The workspace it lives on.
    pub ws: usize,
    pub id: PanelId,
    /// [`Panel::title`](crate::panel::Panel::title).
    pub title: String,
}

/// Where a panel is to be found: focus it if it is open on any workspace,
/// open it fresh otherwise — the launcher's verb, for anything else that
/// reaches a root panel (the problems mark, a menu item). Never a second
/// copy.
#[must_use]
pub fn locate(windows: &[Window], id: &PanelId) -> Go {
    match windows.iter().find(|w| w.id == *id) {
        Some(w) => Go::Focus(w.slot),
        None => Go::Open(id.clone()),
    }
}

/// The instant half of the list: the open panels, then the roots. An empty
/// query leaves exactly this — the pure switcher, and the one answer that
/// reads nothing at all.
///
/// `windows` are in the order they are to be offered; the session sorts
/// them so the active workspace leads. A panel's haystack is its title and
/// its tag, so "inbox" finds the inbox whatever a build titles it.
#[must_use]
pub fn windows(windows: &[Window], roots: &[Root], query: &str) -> Vec<Hit> {
    // The same reading of the query the providers get, so one word means
    // one thing whether the thing it names is open or not.
    let terms = search::terms(query);
    let mut hits: Vec<Hit> = Vec::new();

    for w in windows {
        let tag = w.id.tag.as_str();
        if !search::matches(&terms, &[&w.title, tag]) {
            continue;
        }
        hits.push(Hit {
            label: w.title.clone(),
            detail: tag.to_string(),
            ws: Some(w.ws),
            go: Go::Focus(w.slot),
        });
    }

    for root in roots {
        let tag = root.id.tag.as_str();
        if !search::matches(&terms, &[&root.label, tag, &root.words]) {
            continue;
        }
        hits.push(Hit {
            label: root.label.clone(),
            detail: tag.to_string(),
            ws: None,
            go: Go::Open(root.id.clone()),
        });
    }

    hits
}

/// The launcher's live question: what was asked, what has answered, and the
/// one merged list the overlay draws.
///
/// Every mutation happens on an event — a keystroke through [`Self::ask`],
/// an answer through [`Self::collect`]. The draw only ever reads
/// [`Self::hits`], so the search is never on the frame, however long a
/// provider takes.
#[derive(Default)]
pub struct Search {
    engine: Engine,
    /// Which question is current. Answers stamped with an older one are
    /// dropped: a provider that was slow has nothing to say about a query
    /// that has already been typed past.
    gen: u64,
    query: String,
    /// One slot per source, in list order: 0 is [`windows`], 1.. are the
    /// engine's providers. `None` until that source has spoken for this
    /// generation — which is how a row can simply be *not there yet*.
    answers: Vec<Option<Vec<Hit>>>,
    hits: Vec<Hit>,
    sel: usize,
    /// What the selection is *of*, as against where it last sat. A source
    /// answering late inserts its band into the middle of the list and
    /// pushes every later source's rows down; the index alone would then
    /// point at a different thing than the one a person had picked, and
    /// enter would open it. So the row is remembered, and the index is
    /// re-derived from it on every merge.
    anchor: Option<Go>,
}

impl std::fmt::Debug for Search {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Search")
            .field("query", &self.query)
            .field("gen", &self.gen)
            .field("hits", &self.hits.len())
            .field("sel", &self.sel)
            .finish()
    }
}

impl Search {
    #[must_use]
    pub fn new(engine: Engine) -> Self {
        Search {
            engine,
            ..Default::default()
        }
    }

    /// Puts a question. The windows answer before this returns; the
    /// providers are sent away, and their rows arrive in [`Self::collect`].
    ///
    /// Re-asking the same query is how the launcher takes account of a
    /// commit that landed under it — the selection stays where the person
    /// left it, because only their typing moves it.
    pub fn ask(&mut self, store: &Store, open: &[Window], roots: &[Root], query: &str) {
        if self.query != query {
            self.query = query.to_string();
            self.sel = 0;
            self.anchor = None;
        }
        self.gen += 1;
        self.answers.clear();
        self.answers.resize(1 + self.engine.slots(), None);
        self.answers[0] = Some(windows(open, roots, query));
        // The empty launcher is the switcher, not a directory dump: it asks
        // the providers nothing at all.
        if !query.trim().is_empty() {
            self.engine.ask(store, self.gen, query);
        }
        self.merge(open);
        // An inline engine has already answered by the time `ask` returns,
        // and nothing will ring the UI signal on its behalf: taking it here
        // is what makes a headless run's list complete in the tick that
        // asked for it. Against threads this is one `try_recv` that finds
        // nothing yet.
        self.collect(open);
    }

    /// Raised fresh: a blank question and the selection back at the top.
    /// Reopening is not the same act as re-asking — whatever was picked
    /// last time is not what this one is about.
    pub fn open(&mut self, store: &Store, open: &[Window], roots: &[Root]) {
        self.query.clear();
        self.sel = 0;
        self.anchor = None;
        self.ask(store, open, roots, "");
    }

    /// Asks the current question again — what a foreign commit means for a
    /// launcher that is already up.
    pub fn again(&mut self, store: &Store, open: &[Window], roots: &[Root]) {
        // The clone is the point: `ask` reads the selection as the person's
        // if the query is the one already standing, and only a *changed*
        // query sends it back to the top.
        let q = self.query.clone();
        self.ask(store, open, roots, &q);
    }

    /// Takes whatever has come back. Says whether the list changed, which
    /// is the caller's cue to redraw.
    pub fn collect(&mut self, open: &[Window]) -> bool {
        let landed: Vec<Answer> = self
            .engine
            .collect()
            .into_iter()
            .filter(|a| a.gen == self.gen)
            .collect();
        if landed.is_empty() {
            return false;
        }
        for a in landed {
            if let Some(slot) = self.answers.get_mut(a.slot + 1) {
                *slot = Some(a.hits);
            }
        }
        self.merge(open);
        true
    }

    /// Sources in order, each hit given its verb: a panel that is already
    /// open anywhere becomes a *go to* rather than a second copy, and a
    /// slot already listed by an earlier source is not listed twice.
    fn merge(&mut self, open: &[Window]) {
        let mut hits: Vec<Hit> = Vec::new();
        let mut seen: Vec<SlotId> = Vec::new();
        for hit in self.answers.iter().flatten().flatten() {
            if hits.len() >= MAX_HITS {
                break;
            }
            let mut hit = hit.clone();
            if let Go::Open(id) = &hit.go {
                if let Some(w) = open.iter().find(|w| w.id == *id) {
                    hit.ws = Some(w.ws);
                    hit.go = Go::Focus(w.slot);
                }
            }
            if let Go::Focus(slot) = hit.go {
                if seen.contains(&slot) {
                    continue;
                }
                seen.push(slot);
            }
            hits.push(hit);
        }
        self.hits = hits;
        // The selection follows the row it was made of, wherever the new
        // list put it. A row that is gone — typed past, or not answered for
        // yet — leaves the index alone: it is still the person's intent,
        // and the row may well come back.
        if let Some(go) = &self.anchor {
            if let Some(i) = self.hits.iter().position(|h| h.go == *go) {
                self.sel = i;
            }
        }
    }

    #[must_use]
    pub fn hits(&self) -> &[Hit] {
        &self.hits
    }

    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// The selected row. What is *stored* is the person's intent, and it is
    /// clamped only here, on the way out — a list that is momentarily short
    /// because a provider has not answered yet must not take the selection
    /// down with it and give back a different one when its rows land.
    #[must_use]
    pub fn sel(&self) -> usize {
        self.sel.min(self.hits.len().saturating_sub(1))
    }

    /// The hit enter would take.
    #[must_use]
    pub fn selected(&self) -> Option<&Hit> {
        self.hits.get(self.sel())
    }

    /// Moves the selection. The list is a ring: past the last is the first.
    /// It steps from what is on screen, so a list that really did shrink is
    /// where the next arrow starts from.
    pub fn step(&mut self, by: isize) {
        let n = self.hits.len() as isize;
        if n == 0 {
            self.sel = 0;
            self.anchor = None;
            return;
        }
        self.sel = ((self.sel() as isize + by).rem_euclid(n)) as usize;
        self.anchor = self.hits.get(self.sel).map(|h| h.go.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panel::Tag;
    use crate::search::{Abandoned, Provider};

    fn id(tag: &'static str) -> PanelId {
        PanelId::bare(Tag(tag))
    }

    fn win(slot: SlotId, ws: usize, tag: &'static str, title: &str) -> Window {
        Window {
            slot,
            ws,
            id: id(tag),
            title: title.into(),
        }
    }

    fn roots() -> Vec<Root> {
        vec![
            Root::new(id("inbox"), "inbox", "mailbox"),
            Root::new(id("effects"), "effects", "log queue"),
            Root::new(id("help"), "help", ""),
            Root::new(id("about"), "about", ""),
        ]
    }

    fn store() -> Store {
        Store::open(None, &[]).expect("in-memory store")
    }

    /// An empty query is the switcher: the open panels, then the roots that
    /// are not open. Nothing is listed twice.
    #[test]
    fn an_empty_query_is_the_switcher() {
        let open = vec![win(1, 0, "help", "help"), win(2, 0, "inbox", "inbox")];
        let s = store();
        let mut search = Search::new(Engine::inline(Vec::new()));
        search.ask(&s, &open, &roots(), "");
        let hits = search.hits();
        // help and inbox are open, so they are *go to*; effects and about
        // are not, and follow the roots' own order.
        assert_eq!(hits.len(), 4);
        assert_eq!(hits[0].label, "help");
        assert!(matches!(hits[0].go, Go::Focus(1)));
        assert_eq!(hits[1].label, "inbox");
        assert!(matches!(hits[1].go, Go::Focus(2)));
        assert_eq!(hits[2].label, "effects");
        assert!(matches!(&hits[2].go, Go::Open(p) if *p == id("effects")));
        assert_eq!(hits[3].label, "about");
    }

    /// The words a query may match: the title, the tag, and a root's extra
    /// words.
    #[test]
    fn a_query_matches_the_title_the_tag_and_the_extra_words() {
        let open = vec![win(1, 0, "message", "Q3 infra budget")];
        let hits = |q: &str| -> Vec<String> {
            windows(&open, &roots(), q)
                .into_iter()
                .map(|h| h.label)
                .collect()
        };
        assert_eq!(hits("q3"), vec!["Q3 infra budget".to_string()]);
        assert_eq!(hits("message"), vec!["Q3 infra budget".to_string()]);
        assert_eq!(
            hits("queue"),
            vec!["effects".to_string()],
            "a root's extra words"
        );
        assert!(hits("nfra").is_empty(), "the middle of a word is no prefix");
    }

    /// The mark's and the menu's verb: never a second copy.
    #[test]
    fn locate_finds_an_open_panel_before_opening_one() {
        let open = vec![win(4, 2, "inbox", "inbox")];
        assert_eq!(locate(&open, &id("inbox")), Go::Focus(4));
        assert_eq!(locate(&open, &id("about")), Go::Open(id("about")));
        assert_eq!(locate(&[], &id("inbox")), Go::Open(id("inbox")));
    }

    /// A provider that offers something already open is merged into the
    /// *go to* for it, wherever it lives.
    #[test]
    fn open_panels_win_over_second_copies() {
        struct Offers;
        impl Provider for Offers {
            fn id(&self) -> &'static str {
                "offers"
            }
            fn search(&self, _s: &Store, _q: &str, _a: &Abandoned) -> Vec<Hit> {
                vec![Hit::found("inbox", "", id("inbox"))]
            }
        }
        let open = vec![win(7, 2, "inbox", "inbox")];
        let s = store();
        let mut search = Search::new(Engine::inline(vec![Box::new(Offers)]));
        search.ask(&s, &open, &roots(), "inbox");
        let listed: Vec<&Hit> = search
            .hits()
            .iter()
            .filter(|h| h.label == "inbox")
            .collect();
        assert_eq!(listed.len(), 1, "once, as a go to: {:?}", search.hits());
        assert_eq!(listed[0].go, Go::Focus(7));
        assert_eq!(listed[0].ws, Some(2));
    }

    /// The switcher is on screen before a slow source has said anything,
    /// and that source's rows land underneath without moving what a person
    /// is already looking at. The whole point of the split.
    #[test]
    fn the_windows_answer_before_a_slow_provider_does() {
        struct Slow(std::sync::mpsc::Receiver<()>);
        impl Provider for Slow {
            fn id(&self) -> &'static str {
                "slow"
            }
            fn search(&self, _s: &Store, query: &str, _a: &Abandoned) -> Vec<Hit> {
                let _ = self.0.recv();
                vec![Hit::found(format!("late {query}"), "", id("about"))]
            }
        }

        let s = store();
        let open = vec![win(1, 0, "help", "help")];
        let (release, go) = std::sync::mpsc::channel();
        let mut search = Search::new(Engine::threads(&s.db(), vec![Box::new(Slow(go))], || {}));

        search.ask(&s, &open, &roots(), "help");
        let switcher: Vec<String> = search.hits().iter().map(|h| h.label.clone()).collect();
        assert!(!switcher.is_empty(), "the windows answered inside `ask`");
        assert!(
            search.hits().iter().all(|h| !h.label.starts_with("late")),
            "the slow source has not spoken yet"
        );

        release.send(()).expect("let it answer");
        let mut landed = false;
        for _ in 0..2000 {
            if search.collect(&open) {
                landed = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(landed, "the slow source answered eventually");
        let after: Vec<String> = search.hits().iter().map(|h| h.label.clone()).collect();
        assert_eq!(
            after[..switcher.len()],
            switcher[..],
            "the rows already on screen did not move"
        );
        assert!(after.iter().any(|l| l == "late help"));
    }

    /// A source answering late inserts its band above another's rows. The
    /// selection has to follow the row a person picked, not the number that
    /// row happened to sit at — otherwise enter opens something else.
    #[test]
    fn a_late_source_does_not_move_the_selection_off_its_row() {
        struct Held {
            id: &'static str,
            base: i64,
            go: std::sync::mpsc::Receiver<()>,
        }
        impl Provider for Held {
            fn id(&self) -> &'static str {
                self.id
            }
            fn search(&self, _s: &Store, _q: &str, _a: &Abandoned) -> Vec<Hit> {
                let _ = self.go.recv();
                (0..3)
                    .map(|i| {
                        Hit::found(
                            format!("{}{i}", self.id),
                            "",
                            PanelId::new(Tag("job"), [(self.base + i).to_string()]),
                        )
                    })
                    .collect()
            }
        }

        let s = store();
        let (release_a, go_a) = std::sync::mpsc::channel();
        let (release_b, go_b) = std::sync::mpsc::channel();
        let mut search = Search::new(Engine::threads(
            &s.db(),
            vec![
                Box::new(Held {
                    id: "a",
                    base: 10,
                    go: go_a,
                }),
                Box::new(Held {
                    id: "b",
                    base: 20,
                    go: go_b,
                }),
            ],
            || {},
        ));

        // A query no window and no root answers: the list is the providers'.
        search.ask(&s, &[], &roots(), "zzz");
        assert!(search.hits().is_empty());

        let settle = |search: &mut Search, n: usize| {
            for _ in 0..2000 {
                search.collect(&[]);
                if search.hits().len() == n {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            panic!("the list never reached {n} rows");
        };

        release_b.send(()).expect("release b");
        settle(&mut search, 3);
        assert_eq!(
            search
                .hits()
                .iter()
                .map(|h| h.label.clone())
                .collect::<Vec<_>>(),
            vec!["b0", "b1", "b2"]
        );
        search.step(1);
        let picked = search.selected().expect("a row").go.clone();

        release_a.send(()).expect("release a");
        settle(&mut search, 6);
        assert_eq!(
            search
                .hits()
                .iter()
                .map(|h| h.label.clone())
                .collect::<Vec<_>>(),
            vec!["a0", "a1", "a2", "b0", "b1", "b2"]
        );
        assert_eq!(search.sel(), 4, "the index moved with the row");
        assert_eq!(search.selected().expect("a row").go, picked);
    }

    /// An answer to a question that has been typed past is not shown.
    #[test]
    fn a_stale_answer_is_dropped() {
        struct Echo;
        impl Provider for Echo {
            fn id(&self) -> &'static str {
                "echo"
            }
            fn search(&self, _s: &Store, q: &str, _a: &Abandoned) -> Vec<Hit> {
                vec![Hit::found(format!("echo {q}"), "", id("about"))]
            }
        }
        let s = store();
        let mut search = Search::new(Engine::inline(vec![Box::new(Echo)]));
        search.ask(&s, &[], &[], "one");
        search.ask(&s, &[], &[], "two");
        search.collect(&[]);
        assert_eq!(search.hits().len(), 1);
        assert_eq!(search.hits()[0].label, "echo two");
    }

    /// The selection is a ring, and it survives a re-ask that was not a
    /// keystroke.
    #[test]
    fn the_selection_rings_and_survives_a_commit() {
        let s = store();
        let open = vec![win(1, 0, "help", "help")];
        let mut search = Search::new(Engine::inline(Vec::new()));
        search.ask(&s, &open, &roots(), "");
        let n = search.hits().len();
        assert_eq!(search.sel(), 0);
        search.step(1);
        search.step(1);
        assert_eq!(search.sel(), 2);
        search.step(-1);
        assert_eq!(search.sel(), 1);
        // Past the ends, both ways.
        search.step(-2);
        assert_eq!(search.sel(), n - 1);
        search.step(1);
        assert_eq!(search.sel(), 0);

        // A commit under an open launcher re-asks; the selection stays.
        search.step(2);
        search.again(&s, &open, &roots());
        assert_eq!(search.sel(), 2, "only typing moves the selection");
        assert_eq!(search.query(), "");
        // Typing resets it.
        search.ask(&s, &open, &roots(), "help");
        assert_eq!(search.sel(), 0);
        // …and raising it fresh does too.
        search.step(0);
        search.open(&s, &open, &roots());
        assert_eq!(search.query(), "");
        assert_eq!(search.sel(), 0);
    }

    /// However much a source finds, the merged list stops at a palette's
    /// worth — every row past that is a widget on every frame.
    #[test]
    fn the_merge_stops_at_a_screenful() {
        struct Flood;
        impl Provider for Flood {
            fn id(&self) -> &'static str {
                "flood"
            }
            fn search(&self, _s: &Store, _q: &str, _a: &Abandoned) -> Vec<Hit> {
                (0..MAX_HITS * 3)
                    .map(|i| {
                        Hit::found(
                            format!("row {i}"),
                            "",
                            PanelId::new(Tag("job"), [i.to_string()]),
                        )
                    })
                    .collect()
            }
        }
        let s = store();
        let mut search = Search::new(Engine::inline(vec![Box::new(Flood)]));
        search.ask(&s, &[], &[], "anything");
        assert_eq!(search.hits().len(), MAX_HITS);
        assert!(format!("{search:?}").contains("Search"));
    }
}
