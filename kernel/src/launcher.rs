//! The switcher: every open panel, then every root, as one list.
//!
//! An open panel becomes [`Go::Focus`]; a root nothing is showing becomes
//! [`Go::Open`]. Roots marked fresh always offer another instance beside
//! the open panels. Nothing here reads a store, a layout, or a source:
//! it is a slice of open slots and a slice of roots,
//! both handed over by the session, sifted by the words in the query.
//!
//! Searching *into* what the apps hold — the letters, the people, the files
//! — is a panel of its own ([`crate::search::Query`]), and reaches nothing
//! from here. The launcher answers in the tick it is asked in, always.

use crate::app::Root;
use crate::layout::SlotId;
use crate::panel::PanelId;
use crate::search::{self, Go, Hit};

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
        let existing = if root.fresh {
            None
        } else {
            windows.iter().find(|w| w.id == root.id)
        };
        hits.push(Hit {
            label: root.label.clone(),
            detail: tag.to_string(),
            ws: existing.map(|w| w.ws),
            go: existing.map_or_else(|| Go::Open(root.id.clone()), |w| Go::Focus(w.slot)),
        });
    }

    // A named panel leads keyword matches: "notes" opens notes even when
    // another root also advertises that word (for example saved messages).
    if !query.trim().is_empty() {
        hits.sort_by_key(|hit| !hit.label.eq_ignore_ascii_case(query.trim()));
    }
    hits
}

/// The launcher's live question: what was asked, and the list the overlay
/// draws for it.
///
/// Every mutation happens on an event — a keystroke through [`Self::ask`].
/// The draw only ever reads [`Self::hits`], and the whole answer is two
/// slices sifted in memory, so the list is never on the frame.
#[derive(Debug, Default)]
pub struct Search {
    query: String,
    hits: Vec<Hit>,
    sel: usize,
    /// What the selection is *of*, as against where it last sat. A panel
    /// opening or closing under an open launcher moves the rows about; the
    /// index alone would then point at a different thing than the one a
    /// person had picked, and enter would open it. So the row is
    /// remembered, and the index is re-derived from it on every merge.
    anchor: Option<Go>,
    /// Keep the workspace that led on opening first while live focus moves
    /// between workspaces. Otherwise arrowing through rows reorders them.
    workspace: Option<usize>,
}

impl Search {
    #[must_use]
    pub fn new() -> Self {
        Search::default()
    }

    /// Puts a question and answers it, here and now.
    ///
    /// Re-asking the same query is how the launcher takes account of a
    /// commit that landed under it — the selection stays where the person
    /// left it, because only their typing moves it.
    pub fn ask(&mut self, open: &[Window], roots: &[Root], query: &str) {
        let changed = self.query != query;
        if changed {
            self.query = query.to_string();
            self.sel = 0;
            self.anchor = None;
        }
        self.merge(open, roots);
        if changed {
            self.anchor = self.selected().map(|hit| hit.go.clone());
        }
    }

    /// Raised fresh: a blank question with the current panel selected.
    /// An empty workspace starts at the top; a previous launcher's query
    /// and selection do not carry over.
    pub fn open(&mut self, open: &[Window], roots: &[Root], focus: Option<SlotId>) {
        self.query.clear();
        self.sel = 0;
        self.anchor = focus.map(Go::Focus);
        self.workspace = open.first().map(|w| w.ws);
        self.merge(open, roots);
    }

    /// Asks the current question again — what a foreign commit means for a
    /// launcher that is already up. The selection stays on its row.
    pub fn again(&mut self, open: &[Window], roots: &[Root]) {
        self.merge(open, roots);
    }

    /// Each open slot is listed once. Explicit creation roots keep their
    /// Open action beside the existing panels' Focus actions.
    fn merge(&mut self, open: &[Window], roots: &[Root]) {
        let mut open = open.to_vec();
        if let Some(workspace) = self.workspace {
            open.sort_by_key(|w| (w.ws != workspace, w.ws));
        }
        let mut hits: Vec<Hit> = Vec::new();
        let mut seen: Vec<SlotId> = Vec::new();
        for hit in windows(&open, roots, &self.query) {
            if hits.len() >= MAX_HITS {
                break;
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
        // list put it. A row that is gone — typed past, or closed under the
        // launcher — leaves the index alone: it is still the person's
        // intent, and the row may well come back.
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
    /// because a panel closed under it must not take the selection down with
    /// it and give back a different one when the row comes back.
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

    #[test]
    fn an_exact_name_leads_other_roots_keywords() {
        let roots = vec![
            Root::new(id("saved"), "saved messages", "notes personal"),
            Root::new(id("notes"), "notes", "text editor"),
        ];
        let hits = windows(&[], &roots, "NOTES");
        assert_eq!(hits[0].label, "notes");
        assert_eq!(hits[1].label, "saved messages");
        assert_eq!(windows(&[], &roots, "")[0].label, "saved messages");
    }

    /// An empty query is the switcher: the open panels, then the roots that
    /// are not open. Nothing is listed twice.
    #[test]
    fn an_empty_query_is_the_switcher() {
        let open = vec![win(1, 0, "help", "help"), win(2, 0, "inbox", "inbox")];
        let mut search = Search::new();
        search.ask(&open, &roots(), "");
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

    /// A root something is already showing is listed once, as the *go to*
    /// for it, badged with the workspace it is on — never as a second copy
    /// beside the window that holds it.
    #[test]
    fn an_open_root_is_listed_once_as_a_go_to() {
        let open = vec![win(7, 2, "inbox", "inbox")];
        let mut search = Search::new();
        search.ask(&open, &roots(), "inbox");
        let listed: Vec<&Hit> = search
            .hits()
            .iter()
            .filter(|h| h.label == "inbox")
            .collect();
        assert_eq!(listed.len(), 1, "once, as a go to: {:?}", search.hits());
        assert_eq!(listed[0].go, Go::Focus(7));
        assert_eq!(listed[0].ws, Some(2));
    }

    #[test]
    fn a_fresh_root_stays_available_beside_every_open_instance() {
        let roots = vec![Root::new(id("terminal"), "new terminal", "shell").fresh()];
        let open = vec![
            win(7, 0, "terminal", "terminal: zsh"),
            win(8, 2, "terminal", "terminal: nvim"),
        ];
        let mut search = Search::new();
        search.ask(&open, &roots, "terminal");
        let hits = search.hits();
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].go, Go::Focus(7));
        assert_eq!(hits[1].go, Go::Focus(8));
        assert_eq!(hits[2].go, Go::Open(id("terminal")));
        assert_eq!(hits[2].label, "new terminal");
        assert_eq!(hits[2].ws, None);
        search.ask(&open, &roots, "new terminal");
        assert_eq!(search.selected().unwrap().go, Go::Open(id("terminal")));
    }

    #[test]
    fn normal_and_fresh_roots_can_share_a_panel_identity() {
        let roots = vec![
            Root::new(id("terminal"), "terminal", "shell"),
            Root::new(id("terminal"), "new terminal", "shell").fresh(),
        ];
        let open = vec![win(7, 2, "terminal", "terminal: zsh")];
        let mut search = Search::new();
        search.ask(&open, &roots, "");
        assert_eq!(search.hits().len(), 2);
        assert_eq!(search.hits()[0].go, Go::Focus(7));
        assert_eq!(search.hits()[1].go, Go::Open(id("terminal")));
        search.ask(&open, &roots, "shell");
        assert_eq!(search.hits().len(), 2);
        assert_eq!(search.hits()[0].go, Go::Focus(7));
        assert_eq!(search.hits()[1].go, Go::Open(id("terminal")));
    }

    #[test]
    fn opening_selects_the_current_slot_even_among_identical_panels() {
        let mut open = vec![
            win(1, 0, "terminal", "terminal: zsh"),
            win(2, 0, "terminal", "terminal: zsh"),
            win(3, 1, "help", "help"),
        ];
        let mut search = Search::new();
        search.open(&open, &roots(), Some(2));
        assert_eq!(search.sel(), 1);
        assert_eq!(search.selected().unwrap().go, Go::Focus(2));
        open.swap(0, 1);
        open[0].title = "terminal: cargo".into();
        search.again(&open, &roots());
        assert_eq!(search.sel(), 0);
        assert_eq!(search.selected().unwrap().label, "terminal: cargo");
        assert_eq!(search.selected().unwrap().go, Go::Focus(2));
        search.ask(&open, &roots(), "help");
        assert_eq!(search.selected().unwrap().go, Go::Focus(3));
        search.open(&open, &roots(), Some(1));
        assert_eq!(search.query(), "");
        assert_eq!(search.selected().unwrap().go, Go::Focus(1));
        search.open(&open, &roots(), None);
        assert_eq!(search.sel(), 0);
    }

    #[test]
    fn following_focus_across_workspaces_keeps_the_result_order() {
        let a = win(1, 0, "terminal", "terminal: zsh");
        let b = win(2, 0, "terminal", "terminal: cargo");
        let c = win(3, 2, "terminal", "terminal: nvim");
        let roots = vec![Root::new(id("terminal"), "new terminal", "shell").fresh()];
        let mut search = Search::new();
        search.open(&[a.clone(), b.clone(), c.clone()], &roots, Some(2));
        search.ask(&[a.clone(), b.clone(), c.clone()], &roots, "terminal");
        search.step(2);
        assert_eq!(search.selected().unwrap().go, Go::Focus(3));

        // Live focus makes workspace 2 lead the session's next window list.
        let switched = vec![c.clone(), a.clone(), b.clone()];
        search.again(&switched, &roots);
        assert_eq!(search.sel(), 2);
        search.step(-1);
        assert_eq!(search.selected().unwrap().go, Go::Focus(2));
        search.step(2);
        assert_eq!(search.selected().unwrap().go, Go::Open(id("terminal")));

        // A newly typed query anchors its first result too. A title update
        // introducing another match must not move it to a different shell.
        search.ask(&switched, &roots, "nvim");
        assert_eq!(search.selected().unwrap().go, Go::Focus(3));
        let renamed = win(1, 0, "terminal", "terminal: nvim");
        search.again(&[renamed, b, c], &roots);
        assert_eq!(search.selected().unwrap().go, Go::Focus(3));
    }

    /// A panel opening or closing under an open launcher moves the rows;
    /// the selection stays on the row a person picked, not on the number it
    /// sat at — otherwise enter opens something else.
    #[test]
    fn a_list_that_moved_does_not_move_the_selection_off_its_row() {
        let roots = roots();
        let mut open = vec![win(1, 0, "help", "help")];
        let mut search = Search::new();
        search.ask(&open, &roots, "");
        // The rows are: help (open), then inbox, effects, about.
        search.step(2);
        let picked = search.selected().expect("a row").go.clone();
        assert_eq!(search.selected().expect("a row").label, "effects");

        // A second panel opens above it: one more window row, and every
        // root below it shifts down.
        open.push(win(2, 0, "inbox", "inbox"));
        search.again(&open, &roots);
        assert_eq!(search.sel(), 2, "the index moved with the row");
        assert_eq!(search.selected().expect("a row").go, picked);
        assert_eq!(search.selected().expect("a row").label, "effects");
    }

    /// The selection is a ring, and it survives a re-ask that was not a
    /// keystroke.
    #[test]
    fn the_selection_rings_and_survives_a_commit() {
        let open = vec![win(1, 0, "help", "help")];
        let mut search = Search::new();
        search.ask(&open, &roots(), "");
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
        search.again(&open, &roots());
        assert_eq!(search.sel(), 2, "only typing moves the selection");
        assert_eq!(search.query(), "");
        // Typing resets it.
        search.ask(&open, &roots(), "help");
        assert_eq!(search.sel(), 0);
        // …and raising it fresh does too.
        search.step(0);
        search.open(&open, &roots(), None);
        assert_eq!(search.query(), "");
        assert_eq!(search.sel(), 0);
    }

    /// However many panels are open, the list stops at a palette's worth —
    /// every row past that is a widget on every frame.
    #[test]
    fn the_merge_stops_at_a_screenful() {
        let open: Vec<Window> = (0..MAX_HITS as SlotId * 2)
            .map(|i| Window {
                slot: i + 1,
                ws: 0,
                id: PanelId::new(Tag("job"), [i.to_string()]),
                title: format!("job {i}"),
            })
            .collect();
        let mut search = Search::new();
        search.ask(&open, &[], "job");
        assert_eq!(search.hits().len(), MAX_HITS);
        assert!(format!("{search:?}").contains("Search"));
    }
}
