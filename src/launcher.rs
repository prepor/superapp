//! The launcher: one question, several sources, one list.
//!
//! Results come in two verbs. A hit that is already open somewhere is a
//! **go to** ([`Go::Focus`] — switch workspace, focus it); anything else is
//! an **open** ([`Go::Open`] — a fresh un-joined column on the active
//! workspace). Which of the two a row gets is decided *here*, on the merge,
//! against the live windows — a [`crate::search::Provider`] answering on a
//! worker thread has no business knowing what is on screen, and says only
//! what it found.
//!
//! The list is made of two halves that behave nothing alike:
//!
//! - **The windows** — the open panels and the roots. Answered on the spot,
//!   in [`windows`], because double-cmd is a switcher first and it must be
//!   there before the key comes back up. It reads the layout and a handful
//!   of panel titles; there is no version of this worth waiting for.
//! - **The providers** — mail today ([`crate::mail::Provider`], over the
//!   FTS5 index), files and chats tomorrow. Asked through
//!   [`crate::search::Engine`], answered on their own threads, merged in
//!   underneath as they land. Nothing about them touches a frame.
//!
//! So the list grows: the switcher rows are there instantly, the found rows
//! arrive. The order never shuffles — a source owns its band of the list —
//! so nothing a person is reaching for moves under their hand.

use crate::core::{Kind, PanelId, Seed, Wm, WS_N};
use crate::mail;
use crate::search::{Answer, Engine, Go, Hit};
use crate::store::Store;

/// How many rows the list is allowed to reach. A palette is refined by
/// typing, not scrolled, and every row past a screenful costs a widget on
/// every frame the overlay draws.
const MAX_HITS: usize = 200;

/// A kind's one-word class, part of every haystack — "inbox" finds the inbox,
/// "draft" the composes.
fn kind_word(kind: &Kind) -> &'static str {
    match kind {
        Kind::Help => "help",
        Kind::About => "about",
        Kind::Inbox { .. } => "inbox",
        Kind::Message { .. } => "mail",
        Kind::Contact { .. } => "contact",
        Kind::Compose { .. } => "draft",
        Kind::Settings => "settings",
        Kind::AddAccount => "add account",
        Kind::Problems => "problems",
        Kind::Effects => "effects",
        Kind::Job { .. } => "job",
        Kind::Files { .. } => "files",
        Kind::File { .. } => "file",
    }
}

/// The muted line under/next to a hit: what identifies it beyond the title.
/// Only the windows reach this — there are a handful of open panels, so the
/// mail read it costs is one a panel, never one a letter.
fn kind_detail(store: &Store, kind: &Kind) -> String {
    match kind {
        Kind::Message { id } => mail::mail(store, *id)
            .map(|m| m.head.from_name)
            .unwrap_or_default(),
        Kind::Contact { email } => email.clone(),
        _ => kind_word(kind).to_string(),
    }
}

/// Sender words for an open mail panel's haystack, so "vera" finds the open
/// message the same way the index finds the unopened one.
fn mail_extra(store: &Store, kind: &Kind) -> String {
    match kind {
        Kind::Message { id } => mail::mail(store, *id)
            .map(|m| format!("{} {}", m.head.from_email, mail::fmt_date(m.head.date)))
            .unwrap_or_default(),
        Kind::Contact { email } => email.clone(),
        _ => String::new(),
    }
}

/// Does every token appear among a candidate's words? The parts are joined
/// before the search, but a token never spans two of them — the query is
/// split on whitespace, so it cannot contain the joining space.
fn matches(tokens: &[String], parts: &[&str]) -> bool {
    if tokens.is_empty() {
        return true;
    }
    let hay = parts.join(" ").to_lowercase();
    tokens.iter().all(|t| hay.contains(t.as_str()))
}

/// Where a kind is to be found: focus it if it is open on any workspace,
/// open it fresh otherwise — the launcher's verb, for anything else that
/// reaches a root panel (the problems mark, a menu item). Never a second
/// copy.
#[must_use]
pub fn locate(wm: &Wm, kind: &Kind) -> Go {
    match wm.showing(kind).first() {
        Some(pid) => Go::Focus(*pid),
        None => Go::Open(kind.clone()),
    }
}

/// The roots every launcher offers, whether or not they are open.
fn roots() -> [Kind; 8] {
    [
        Kind::Inbox { filter: None },
        Kind::Compose { seed: Seed::Blank },
        Kind::Settings,
        Kind::Problems,
        Kind::Effects,
        Kind::Files {
            dir: crate::files::HOME.into(),
        },
        Kind::Help,
        Kind::About,
    ]
}

/// The instant half of the list: the open panels, active workspace first,
/// then the roots. An empty query leaves exactly this — the pure switcher,
/// and the one answer that reads no mail at all.
#[must_use]
pub fn windows(wm: &Wm, store: &Store, query: &str) -> Vec<Hit> {
    let tokens: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let mut hits: Vec<Hit> = Vec::new();

    let mut order: Vec<usize> = (0..WS_N).collect();
    order.sort_by_key(|&k| (k != wm.active, k));
    for k in order {
        let w = &wm.wss[k];
        for pid in w.columns.iter().flat_map(|c| c.panels.iter()) {
            let Some(p) = w.panels.get(pid) else { continue };
            let label = mail::title(store, &p.kind);
            let detail = kind_detail(store, &p.kind);
            let extra = mail_extra(store, &p.kind);
            if !matches(&tokens, &[&label, &detail, kind_word(&p.kind), &extra]) {
                continue;
            }
            hits.push(Hit {
                label,
                detail,
                ws: Some(k),
                go: Go::Focus(*pid),
            });
        }
    }

    for kind in roots() {
        // The word a person reaches for is not always the panel's name:
        // the effect queue is what one goes looking for as "the log".
        let extra = match kind {
            Kind::Effects => "effects panel log queue".to_string(),
            _ => format!("{} panel", kind_word(&kind)),
        };
        let label = mail::title(store, &kind);
        let detail = kind_detail(store, &kind);
        if !matches(&tokens, &[&label, &detail, &extra]) {
            continue;
        }
        hits.push(Hit::found(label, detail, kind));
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
    pub fn ask(&mut self, wm: &Wm, store: &Store, query: &str) {
        if self.query != query {
            self.query = query.to_string();
            self.sel = 0;
        }
        self.gen += 1;
        self.answers.clear();
        self.answers.resize(1 + self.engine.slots(), None);
        self.answers[0] = Some(windows(wm, store, query));
        // The empty launcher is the switcher, not a directory dump: it asks
        // the mail world — and the file world, and whatever comes after —
        // nothing at all.
        if !query.trim().is_empty() {
            self.engine.ask(store, self.gen, query);
        }
        self.merge(wm);
        // An inline engine has already answered by the time `ask` returns,
        // and nothing will ring the UI signal on its behalf: taking it here
        // is what makes a headless run's list complete in the tick that
        // asked for it. Against threads this is one `try_recv` that finds
        // nothing yet.
        self.collect(wm);
    }

    /// Asks the current question again — what a foreign commit means for a
    /// launcher that is already up.
    pub fn again(&mut self, wm: &Wm, store: &Store) {
        // The clone is the point: `ask` reads the selection as the person's
        // if the query is the one already standing, and only a *changed*
        // query sends it back to the top.
        let q = self.query.clone();
        self.ask(wm, store, &q);
    }

    /// Takes whatever has come back. Says whether the list changed, which
    /// is the caller's cue to redraw.
    pub fn collect(&mut self, wm: &Wm) -> bool {
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
        self.merge(wm);
        true
    }

    /// Sources in order, each hit given its verb: a kind that is already
    /// open anywhere becomes a *go to* rather than a second copy, and a
    /// panel already listed by an earlier source is not listed twice.
    fn merge(&mut self, wm: &Wm) {
        let mut hits: Vec<Hit> = Vec::new();
        let mut seen: Vec<PanelId> = Vec::new();
        for hit in self.answers.iter().flatten().flatten() {
            if hits.len() >= MAX_HITS {
                break;
            }
            let mut hit = hit.clone();
            if let Go::Open(kind) = &hit.go {
                if let Go::Focus(pid) = locate(wm, kind) {
                    hit.ws = wm.ws_of(pid);
                    hit.go = Go::Focus(pid);
                }
            }
            if let Go::Focus(pid) = hit.go {
                if seen.contains(&pid) {
                    continue;
                }
                seen.push(pid);
            }
            hits.push(hit);
        }
        self.hits = hits;
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
            return;
        }
        self.sel = ((self.sel() as isize + by).rem_euclid(n)) as usize;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::Provider;

    /// A launcher over the demo world, with the mail provider answering
    /// inline — the arrangement a headless run uses, so a test sees the
    /// whole list the moment it asks for it.
    fn world() -> (Wm, std::rc::Rc<Store>, Search) {
        let store = std::rc::Rc::new(Store::open(None).expect("in-memory store"));
        mail::seed_if_empty(&store).expect("seed");
        let mut wm = Wm::new();
        wm.open(Kind::Help, None, false);
        let inbox = wm.open(Kind::Inbox { filter: None }, None, false);
        wm.focus = Some(inbox);
        let providers: Vec<Box<dyn Provider>> = vec![Box::new(mail::Provider)];
        (wm, store, Search::new(Engine::inline(providers)))
    }

    /// Everything the query matches, as one list — what the overlay draws
    /// once the inline providers have answered.
    fn hits(wm: &Wm, store: &Store, s: &mut Search, q: &str) -> Vec<Hit> {
        s.ask(wm, store, q);
        s.collect(wm);
        s.hits().to_vec()
    }

    #[test]
    fn empty_query_is_the_switcher() {
        let (wm, store, mut s) = world();
        let hits = hits(&wm, &store, &mut s, "");
        // Open help + inbox, then the unopened roots; no mails, no people.
        assert_eq!(hits.len(), 8);
        assert_eq!(hits[0].label, "help");
        assert_eq!(hits[1].label, "inbox");
        assert!(matches!(hits[0].go, Go::Focus(_)));
        assert!(matches!(hits[1].go, Go::Focus(_)));
        assert_eq!(hits[2].label, "new mail");
        assert!(matches!(
            hits[2].go,
            Go::Open(Kind::Compose { seed: Seed::Blank })
        ));
        assert_eq!(hits[3].label, "settings");
        assert!(matches!(hits[3].go, Go::Open(Kind::Settings)));
        assert_eq!(hits[4].label, "problems");
        assert!(matches!(hits[4].go, Go::Open(Kind::Problems)));
        assert_eq!(hits[5].label, "effects");
        assert!(matches!(hits[5].go, Go::Open(Kind::Effects)));
        assert_eq!(hits[6].label, "~");
        assert!(matches!(&hits[6].go, Go::Open(Kind::Files { dir }) if dir == "~"));
        assert_eq!(hits[7].label, "about");
        assert!(matches!(hits[7].go, Go::Open(Kind::About)));
    }

    /// The mark's and the menu's verb: never a second copy.
    #[test]
    fn locate_finds_an_open_panel_before_opening_one() {
        let (mut wm, _store, _s) = world();
        assert!(matches!(
            locate(&wm, &Kind::Problems),
            Go::Open(Kind::Problems)
        ));
        let pid = wm.open(Kind::Problems, None, false);
        assert!(matches!(locate(&wm, &Kind::Problems), Go::Focus(p) if p == pid));
    }

    #[test]
    fn mails_and_contacts_match_by_any_word() {
        let (wm, store, mut s) = world();
        let out = hits(&wm, &store, &mut s, "vera");
        // The contact first, then her mail — neither is open.
        assert_eq!(out[0].label, "Vera Kovac");
        assert!(matches!(
            &out[0].go,
            Go::Open(Kind::Contact { email }) if email == "vera@kovac.io"
        ));
        assert!(out[1..]
            .iter()
            .any(|h| matches!(h.go, Go::Open(Kind::Message { id: 1 }))));
        // Every token must match: sender + a subject word.
        let out = hits(&wm, &store, &mut s, "vera q3");
        assert!(out
            .iter()
            .all(|h| !matches!(h.go, Go::Open(Kind::Contact { .. }))));
        assert!(out
            .iter()
            .any(|h| matches!(h.go, Go::Open(Kind::Message { id: 1 }))));
    }

    /// What an index buys that a subject scan never did: the words inside
    /// the letter.
    #[test]
    fn a_word_from_the_body_finds_the_letter() {
        let (wm, store, mut s) = world();
        let id = store
            .write(|c| {
                c.execute(
                    "INSERT INTO message(account, folder, from_name, from_email,
                                         subject, date, unread, body)
                     VALUES(1, 1, 'Vera Kovac', 'vera@kovac.io', 'the tender', 0, 0,
                            'the awning over the yard needs replacing')",
                    [],
                )?;
                Ok(c.last_insert_rowid())
            })
            .expect("insert");
        let out = hits(&wm, &store, &mut s, "awning");
        assert!(
            out.iter().any(|h| h.go == Go::Open(Kind::Message { id })),
            "the index reads the letter, not just its subject"
        );
        // And the row is still labelled by the conversation, not the body.
        let hit = out
            .iter()
            .find(|h| h.go == Go::Open(Kind::Message { id }))
            .expect("the letter");
        assert_eq!(hit.label, "the tender");
        assert_eq!(hit.detail, "Vera Kovac");
    }

    /// Type-ahead: the index is asked for prefixes, so a half-typed word
    /// already answers.
    #[test]
    fn a_prefix_answers_before_the_word_is_finished() {
        let (wm, store, mut s) = world();
        for q in ["v", "ve", "ver", "vera"] {
            let out = hits(&wm, &store, &mut s, q);
            assert!(
                out.iter().any(|h| matches!(
                    &h.go,
                    Go::Open(Kind::Contact { email }) if email == "vera@kovac.io"
                )),
                "{q:?} did not reach Vera"
            );
        }
    }

    /// Nothing a person can type is syntax.
    #[test]
    fn punctuation_is_words_not_operators() {
        let (wm, store, mut s) = world();
        for q in ["vera@kovac.io", "re: q3", "*", "\"", "AND OR NOT", "^x"] {
            let _ = hits(&wm, &store, &mut s, q);
        }
        // The address is three words, all of which she matches.
        let out = hits(&wm, &store, &mut s, "vera@kovac.io");
        assert!(out.iter().any(|h| matches!(
            &h.go,
            Go::Open(Kind::Contact { email }) if email == "vera@kovac.io"
        )));
    }

    #[test]
    fn open_panels_win_over_second_copies() {
        let (mut wm, store, mut s) = world();
        // Open m1's message on workspace 3.
        wm.switch(2);
        let msg = wm.open(Kind::Message { id: 1 }, None, false);
        wm.switch(0);
        let out = hits(&wm, &store, &mut s, "q3");
        // Exactly one hit for m1: a Focus at workspace 3, not an Open.
        let m1: Vec<&Hit> = out
            .iter()
            .filter(|h| h.label.contains("Q3 infra"))
            .collect();
        assert_eq!(m1.len(), 1);
        assert_eq!(m1[0].ws, Some(2));
        assert_eq!(m1[0].go, Go::Focus(msg));
    }

    #[test]
    fn active_workspace_panels_lead() {
        let (mut wm, store, mut s) = world();
        wm.switch(4);
        wm.open(Kind::About, None, false);
        let out = hits(&wm, &store, &mut s, "");
        // Workspace 5 is active: its panel sorts before workspace 1's.
        assert_eq!(out[0].label, "about");
        assert_eq!(out[0].ws, Some(4));
    }

    /// The switcher is on screen before a slow source has said anything,
    /// and that source's rows land underneath without moving what a person
    /// is already looking at. The whole point of the split.
    #[test]
    fn the_windows_answer_before_a_slow_provider_does() {
        /// A provider that will not answer until the test lets it.
        struct Slow(std::sync::mpsc::Receiver<()>);
        impl Provider for Slow {
            fn id(&self) -> &'static str {
                "slow"
            }
            fn search(
                &self,
                _store: &Store,
                query: &str,
                _a: &crate::search::Abandoned,
            ) -> Vec<Hit> {
                let _ = self.0.recv();
                vec![Hit::found(format!("late {query}"), "", Kind::About)]
            }
        }

        let store = Store::open(None).expect("in-memory store");
        let mut wm = Wm::new();
        wm.open(Kind::Help, None, false);
        let (release, go) = std::sync::mpsc::channel();
        let mut s = Search::new(Engine::threads(
            &store.db(),
            vec![Box::new(Slow(go))],
            || {},
        ));

        s.ask(&wm, &store, "help");
        let switcher: Vec<String> = s.hits().iter().map(|h| h.label.clone()).collect();
        assert!(!switcher.is_empty(), "the windows answered inside `ask`");
        assert!(
            s.hits().iter().all(|h| !h.label.starts_with("late")),
            "the slow source has not spoken yet"
        );

        release.send(()).expect("let it answer");
        let mut landed = false;
        for _ in 0..2000 {
            if s.collect(&wm) {
                landed = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(landed, "the slow source answered eventually");
        let after: Vec<String> = s.hits().iter().map(|h| h.label.clone()).collect();
        assert_eq!(
            after[..switcher.len()],
            switcher[..],
            "the rows already on screen did not move"
        );
        assert!(
            after.iter().any(|l| l == "late help"),
            "and its row is under them"
        );
    }

    /// An answer to a question that has been typed past is not shown.
    #[test]
    fn a_stale_answer_is_dropped() {
        let (wm, store, mut s) = world();
        s.ask(&wm, &store, "vera");
        // Ask again before collecting: the first answer is now stale, and
        // an inline engine has already discarded it.
        s.ask(&wm, &store, "zzzznothing");
        s.collect(&wm);
        assert!(
            s.hits().iter().all(|h| !h.label.contains("Vera")),
            "the older question's rows must not survive it"
        );
    }

    /// The selection is a ring, and it survives a re-ask that was not a
    /// keystroke.
    #[test]
    fn the_selection_rings_and_survives_a_commit() {
        let (wm, store, mut s) = world();
        hits(&wm, &store, &mut s, "");
        assert_eq!(s.sel(), 0);
        s.step(1);
        s.step(1);
        assert_eq!(s.sel(), 2);
        s.step(-1);
        assert_eq!(s.sel(), 1);
        // Past the ends, both ways.
        let n = s.hits().len();
        s.step(-2);
        assert_eq!(s.sel(), n - 1);
        s.step(1);
        assert_eq!(s.sel(), 0);
        // A commit under an open launcher re-asks; the selection stays —
        // and it must stay under a *typed* query too, which is the case
        // that goes through every branch of `ask`.
        let n = hits(&wm, &store, &mut s, "vera").len();
        assert!(n >= 2, "her card and her letters: {n}");
        s.step(1);
        assert_eq!(s.sel(), 1);
        s.again(&wm, &store);
        s.collect(&wm);
        assert_eq!(s.sel(), 1, "only typing moves the selection");
        assert_eq!(s.hits().len(), n, "and the list came back whole");
        assert_eq!(s.query(), "vera", "and the question is still the question");
        // Typing resets it.
        s.ask(&wm, &store, "vera q3");
        assert_eq!(s.sel(), 0);
    }

    /// A one-letter query over a real mailbox matches nearly all of it. The
    /// index answers with its best hundred, not with the mailbox, and the
    /// windows still lead.
    #[test]
    fn the_index_answers_with_its_best_not_with_everything() {
        let (wm, store, mut s) = world();
        store
            .write(|c| {
                for i in 0..500 {
                    c.execute(
                        "INSERT INTO message(account, folder, from_name, from_email,
                                             subject, date, unread)
                         VALUES(1, 1, 'Ann', 'ann@x.io', ?1, 0, 0)",
                        [format!("inbox number {i}")],
                    )?;
                }
                Ok(())
            })
            .expect("insert");
        let out = hits(&wm, &store, &mut s, "inbox");
        assert!(out.len() <= MAX_HITS, "{}", out.len());
        assert!(
            out.len() < 500,
            "the mailbox is not the list: {}",
            out.len()
        );
        assert_eq!(
            out[0].label, "inbox",
            "the windows lead, whatever the providers found"
        );
        assert!(matches!(out[0].go, Go::Focus(_)));
    }

    /// However much a source finds, the merged list stops at a palette's
    /// worth — every row past that is a widget on every frame the overlay
    /// draws.
    #[test]
    fn the_merge_stops_at_a_screenful() {
        struct Flood;
        impl Provider for Flood {
            fn id(&self) -> &'static str {
                "flood"
            }
            fn search(
                &self,
                _store: &Store,
                _query: &str,
                _a: &crate::search::Abandoned,
            ) -> Vec<Hit> {
                (0..MAX_HITS * 3)
                    .map(|i| Hit::found(format!("row {i}"), "", Kind::Job { id: i as i64 }))
                    .collect()
            }
        }
        let store = Store::open(None).expect("in-memory store");
        let wm = Wm::new();
        let mut s = Search::new(Engine::inline(vec![Box::new(Flood)]));
        s.ask(&wm, &store, "anything");
        s.collect(&wm);
        assert_eq!(s.hits().len(), MAX_HITS);
    }

    #[test]
    fn focus_panel_switches_and_focuses() {
        let (mut wm, _store, _s) = world();
        let help = wm.columns[0].panels[0];
        wm.switch(2);
        let msg = wm.open(Kind::Message { id: 2 }, None, false);
        assert_eq!(wm.focus_panel(help), Some(0));
        assert_eq!(wm.active, 0);
        assert_eq!(wm.focus, Some(help));
        assert_eq!(wm.focus_panel(msg), Some(2));
        assert_eq!(wm.active, 2);
        assert_eq!(wm.focus, Some(msg));
        assert_eq!(wm.focus_panel(0xdead_beef), None);
    }
}

