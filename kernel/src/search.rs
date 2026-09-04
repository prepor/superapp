//! Runs search providers and combines their results.
//!
//! Each provider normally owns a thread and read-only database connection.
//! Results include the query generation, so replies to older queries are
//! discarded. Providers receive [`Abandoned`] to stop expensive stale work.
//! Headless tests run providers inline for predictable timing.
//!
//! [`Query`] is the whole of it as a panel holds it: one question put to
//! every source, the answers as they land, and the merged rows a list draws.
//! The launcher does not come through here — it is the switcher over open
//! panels and roots ([`crate::launcher`]), and reads no source at all.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};

use crate::layout::SlotId;
use crate::panel::PanelId;
use crate::store::{Db, Store};

/// What activating a hit does.
#[derive(Debug, Clone, PartialEq)]
pub enum Go {
    /// Switch to the workspace holding this panel and focus it.
    Focus(SlotId),
    /// Open a fresh un-joined panel on the active workspace.
    Open(PanelId),
}

/// One row of the list.
///
/// A provider fills in what it knows — the words and the identity — and
/// leaves the verb alone: [`Hit::found`] is something to *open*, and where
/// the panel it names already stands is none of a worker's business. The
/// launcher settles that for the rows it lists itself; a search panel opens
/// the row beside itself, the way every other list opens what it shows.
#[derive(Debug, Clone)]
pub struct Hit {
    pub label: String,
    pub detail: String,
    /// The workspace the panel lives on, when it is already open. Filled in
    /// by the merge for anything a provider found.
    pub ws: Option<usize>,
    pub go: Go,
}

impl Hit {
    /// A hit as a provider knows it: something to open, wherever it turns
    /// out to already live.
    pub fn found(label: impl Into<String>, detail: impl Into<String>, id: PanelId) -> Self {
        Hit {
            label: label.into(),
            detail: detail.into(),
            ws: None,
            go: Go::Open(id),
        }
    }
}

/// The query cut into the words a source matches on: lowercased, split at
/// every non-alphanumeric boundary.
///
/// This is the *one* reading of a query the whole list shares. The windows
/// are sifted in memory and the rows are sifted by SQLite, and if the two
/// cut a person's typing differently then the same word finds a row or
/// not depending on whether a panel happens to be open on it — which is
/// exactly what a reader cannot see and cannot explain.
#[must_use]
pub fn terms(query: &str) -> Vec<String> {
    query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Every term a **prefix** of some word among `parts` — the match a source
/// that sifts in memory makes, and the one a full-text index is asked for
/// with a trailing `*`.
///
/// Prefix rather than substring, on both sides: it is what makes a launcher
/// answer while a word is still being typed, and it is the only infix-free
/// thing FTS5 can do without a trigram index three times the size. So
/// "kov" reaches Vera Kovac through her address — the words are cut at the
/// `@` and the `.` — and "ovac" reaches nobody, from either half.
#[must_use]
pub fn matches(terms: &[String], parts: &[&str]) -> bool {
    if terms.is_empty() {
        return true;
    }
    let words: Vec<String> = parts
        .iter()
        .flat_map(|p| p.split(|c: char| !c.is_alphanumeric()))
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect();
    terms
        .iter()
        .all(|t| words.iter().any(|w| w.starts_with(t.as_str())))
}

/// Whether the question being answered has already been replaced.
///
/// The cheap providers ignore this and finish; a directory walk or a server
/// call should look at it between steps and give up when it goes true,
/// because nothing it returns after that will be read.
pub struct Abandoned {
    newest: Arc<AtomicU64>,
    mine: u64,
}

impl Abandoned {
    #[must_use]
    pub fn yes(&self) -> bool {
        self.newest.load(Ordering::Relaxed) > self.mine
    }
}

/// A source of hits, asked away from the UI thread.
///
/// One implementation, one thread, one store connection. Whatever a
/// provider needs beyond the store — a filesystem root, a server client —
/// it owns, exactly as a [`Worker`](crate::app::Worker) owns its own
/// [`World`](crate::effect::World).
pub trait Provider: Send + 'static {
    /// Stable name. It names the thread and nothing else: the order of the
    /// list is the order the providers were registered in.
    fn id(&self) -> &'static str;

    /// Answer `query`. `store` is this thread's own reader over the one
    /// database — never a second writer.
    fn search(&self, store: &Store, query: &str, abandoned: &Abandoned) -> Vec<Hit>;
}

/// A question put to the providers.
struct Ask {
    gen: u64,
    query: String,
}

/// One provider's answer to one question.
pub struct Answer {
    /// The question it answers. Anything but the current one is dropped.
    pub gen: u64,
    /// Which provider: its index among the registered ones.
    pub slot: usize,
    pub hits: Vec<Hit>,
}

/// Who runs the providers.
#[derive(Default)]
pub struct Engine {
    /// The generation of the newest question put. A provider reads it
    /// through its [`Abandoned`] to find out that it is wasting its time.
    newest: Arc<AtomicU64>,
    /// [`Provider::id`] per slot, in the order the answers are listed in —
    /// which is how a row can say which source found it, once the provider
    /// itself has gone off to its own thread.
    names: Vec<&'static str>,
    mode: Mode,
}

enum Mode {
    /// Production: a thread each, so one slow source cannot hold up the
    /// rest.
    Threads {
        asks: Vec<mpsc::Sender<Ask>>,
        answers: mpsc::Receiver<Answer>,
    },
    /// Headless runs, tests and the components library: the same providers,
    /// answered on the calling thread at ask time.
    Inline {
        providers: Vec<Box<dyn Provider>>,
        done: VecDeque<Answer>,
    },
}

impl Default for Mode {
    fn default() -> Self {
        Mode::Inline {
            providers: Vec::new(),
            done: VecDeque::new(),
        }
    }
}

impl Engine {
    /// A thread per provider, each with its own reader over the one
    /// database. `notify` wakes the UI thread once an answer is in the
    /// channel (`SignalToUI` upstairs — this module stays makepad-free).
    #[must_use]
    pub fn threads(
        db: &Arc<Db>,
        providers: Vec<Box<dyn Provider>>,
        notify: impl Fn() + Send + Clone + 'static,
    ) -> Engine {
        let newest: Arc<AtomicU64> = Arc::default();
        let (tx, answers) = mpsc::channel::<Answer>();
        let (mut asks, mut names) = (Vec::new(), Vec::new());
        for (slot, p) in providers.into_iter().enumerate() {
            let (ask_tx, ask_rx) = mpsc::channel::<Ask>();
            let (db, tx, newest, notify) = (db.clone(), tx.clone(), newest.clone(), notify.clone());
            let name = p.id();
            match std::thread::Builder::new()
                .name(format!("search-{name}"))
                .spawn(move || provider_loop(db, slot, p, &ask_rx, &tx, &newest, notify))
            {
                Ok(_) => {
                    asks.push(ask_tx);
                    names.push(name);
                }
                // A source that could not be spawned is simply one the list
                // never shows; the rest still answer.
                Err(e) => eprintln!("search: {name} did not start: {e}"),
            }
        }
        Engine {
            newest,
            names,
            mode: Mode::Threads { asks, answers },
        }
    }

    /// The same providers, run where they are asked.
    #[must_use]
    pub fn inline(providers: Vec<Box<dyn Provider>>) -> Engine {
        Engine {
            newest: Arc::default(),
            names: providers.iter().map(|p| p.id()).collect(),
            mode: Mode::Inline {
                providers,
                done: VecDeque::new(),
            },
        }
    }

    /// How many sources answer — one slot each, in registration order.
    #[must_use]
    pub fn slots(&self) -> usize {
        match &self.mode {
            Mode::Threads { asks, .. } => asks.len(),
            Mode::Inline { providers, .. } => providers.len(),
        }
    }

    /// What each of them is called, in the same order: what a row shows for
    /// the source it came from, and what `@app:` completes to.
    #[must_use]
    pub fn names(&self) -> &[&'static str] {
        &self.names
    }

    /// A provider's view of whether generation `gen` is still the question.
    fn token(&self, gen: u64) -> Abandoned {
        Abandoned {
            newest: self.newest.clone(),
            mine: gen,
        }
    }

    /// Puts a question. Everything still working on an older one is
    /// abandoned; nothing blocks.
    pub fn ask(&mut self, store: &Store, gen: u64, query: &str) {
        self.newest.store(gen, Ordering::Relaxed);
        let abandoned = self.token(gen);
        match &mut self.mode {
            Mode::Threads { asks, .. } => {
                for tx in asks.iter() {
                    let _ = tx.send(Ask {
                        gen,
                        query: query.to_string(),
                    });
                }
            }
            Mode::Inline { providers, done } => {
                done.clear();
                for (slot, p) in providers.iter().enumerate() {
                    let hits = p.search(store, query, &abandoned);
                    done.push_back(Answer { gen, slot, hits });
                }
            }
        }
    }

    /// Everything that has come back since the last call. Never blocks.
    pub fn collect(&mut self) -> Vec<Answer> {
        match &mut self.mode {
            Mode::Threads { answers, .. } => answers.try_iter().collect(),
            Mode::Inline { done, .. } => done.drain(..).collect(),
        }
    }
}

// ---------------------------------------------------------------------------
// The question a panel holds
// ---------------------------------------------------------------------------

/// How many rows one question is worth. The sources rank what they find, so
/// the two hundred best are the two hundred anyone reads; past that the
/// answer is another word, not a longer scroll.
const MAX_FOUND: usize = 200;

/// One row of a search list: what a source found, and which source found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// [`Provider::id`] of the source that answered.
    pub source: &'static str,
    pub label: String,
    /// The muted line under it: whose letter, which folder — the source's
    /// own second thought about the row.
    pub detail: String,
    /// The panel this row opens.
    pub id: PanelId,
}

/// A live question over every source: what was asked, what has answered so
/// far, and the one merged list a panel draws.
///
/// Every mutation happens on an event — the question changing through
/// [`Query::ask`], an answer landing through [`Query::collect`] — so a draw
/// only ever reads [`Query::found`], however long a source takes.
#[derive(Default)]
pub struct Query {
    engine: Engine,
    /// Which question is current. An answer stamped with an older one is
    /// dropped: a slow source has nothing to say about a query that has
    /// already been typed past.
    gen: u64,
    query: String,
    /// One slot per source, in registration order. `None` until that source
    /// has spoken for this generation — which is how a list can be *not
    /// answered yet* rather than empty.
    answers: Vec<Option<Vec<Hit>>>,
    found: Vec<Found>,
}

impl std::fmt::Debug for Query {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Query")
            .field("query", &self.query)
            .field("gen", &self.gen)
            .field("found", &self.found.len())
            .field("pending", &self.pending())
            .finish()
    }
}

impl Query {
    #[must_use]
    pub fn new(engine: Engine) -> Query {
        Query {
            engine,
            ..Default::default()
        }
    }

    /// What the sources are called, in the order they answer in.
    #[must_use]
    pub fn sources(&self) -> &[&'static str] {
        self.engine.names()
    }

    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// The merged rows, sources in registration order and nothing listed
    /// twice.
    #[must_use]
    pub fn found(&self) -> &[Found] {
        &self.found
    }

    /// Whether a source still owes an answer to the question standing. What
    /// an empty list means turns on it: *searching*, not *nothing found*.
    #[must_use]
    pub fn pending(&self) -> bool {
        !self.query.trim().is_empty() && self.answers.iter().any(Option::is_none)
    }

    /// Puts a question. Nothing blocks: the sources are sent away and their
    /// rows arrive in [`Query::collect`].
    ///
    /// An empty question is asked of nobody — a list of everything there is
    /// answers nothing, and the sources have better things to do.
    pub fn ask(&mut self, store: &Store, query: &str) {
        // The generation moves even for a question nobody is asked, so that
        // the answers to the last one are dropped rather than shown under
        // an emptied field.
        self.gen += 1;
        self.query = query.to_string();
        self.answers.clear();
        self.found.clear();
        if query.trim().is_empty() {
            return;
        }
        self.answers.resize(self.engine.slots(), None);
        self.engine.ask(store, self.gen, query);
        // An inline engine has already answered by the time `ask` returns,
        // and nothing will ring the UI signal on its behalf: taking it here
        // is what makes a headless run's list complete in the tick that
        // asked for it. Against threads this is one `try_recv` that finds
        // nothing yet.
        self.collect();
    }

    /// Takes whatever has come back. Says whether the rows changed, which
    /// is the caller's cue to redraw.
    pub fn collect(&mut self) -> bool {
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
            if let Some(slot) = self.answers.get_mut(a.slot) {
                *slot = Some(a.hits);
            }
        }
        self.merge();
        true
    }

    /// The sources in registration order, each row stamped with the one
    /// that found it — whoever answered first.
    ///
    /// The order is the registration's and not the arrival's, so one
    /// question gives one list however the threads were scheduled, and a
    /// source's rows stay together. A source answering late therefore
    /// inserts its band where it belongs and pushes the rows under it down:
    /// what a reader is looking at moves, and what a reader has *picked*
    /// does not, because a list keys its cursor and its marks by the row
    /// rather than by the number the row sat at.
    fn merge(&mut self) {
        let names = self.engine.names();
        let mut out: Vec<Found> = Vec::new();
        let mut seen: Vec<&PanelId> = Vec::new();
        'sources: for (slot, hits) in self.answers.iter().enumerate() {
            let Some(hits) = hits else { continue };
            let source = names.get(slot).copied().unwrap_or("");
            for hit in hits {
                if out.len() >= MAX_FOUND {
                    break 'sources;
                }
                // A source offers something to open. Where that panel
                // already stands is the launcher's question, not a list's:
                // a row here opens beside the list, like every other row.
                let Go::Open(id) = &hit.go else { continue };
                if seen.contains(&id) {
                    continue;
                }
                seen.push(id);
                out.push(Found {
                    source,
                    label: hit.label.clone(),
                    detail: hit.detail.clone(),
                    id: id.clone(),
                });
            }
        }
        self.found = out;
    }
}

/// One provider's thread: wake, take the newest question, answer it, say so.
///
/// It exits when the engine drops — the ask channel closes — which is how a
/// closing app retires its search threads without a shutdown protocol.
fn provider_loop(
    db: Arc<Db>,
    slot: usize,
    p: Box<dyn Provider>,
    asks: &mpsc::Receiver<Ask>,
    answers: &mpsc::Sender<Answer>,
    newest: &Arc<AtomicU64>,
    notify: impl Fn(),
) {
    // The worker joins the *one* writer — its own reader
    // over the shared `Db`, never a second writable connection.
    let Ok(store) = Store::with_db(db) else {
        return;
    };
    while let Ok(mut ask) = asks.recv() {
        // A fast typist puts several questions before this thread wakes.
        // Only the last one is worth answering.
        while let Ok(next) = asks.try_recv() {
            ask = next;
        }
        let abandoned = Abandoned {
            newest: newest.clone(),
            mine: ask.gen,
        };
        let hits = p.search(&store, &ask.query, &abandoned);
        if abandoned.yes() {
            continue; // a newer question is already in the channel
        }
        if answers
            .send(Answer {
                gen: ask.gen,
                slot,
                hits,
            })
            .is_err()
        {
            return;
        }
        notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// A provider that answers with whatever it was asked, and counts the
    /// times it was asked at all.
    struct Echo {
        id: &'static str,
        asked: Arc<AtomicUsize>,
    }

    impl Provider for Echo {
        fn id(&self) -> &'static str {
            self.id
        }
        fn search(&self, _store: &Store, query: &str, _a: &Abandoned) -> Vec<Hit> {
            self.asked.fetch_add(1, Ordering::Relaxed);
            vec![Hit::found(
                format!("{}:{query}", self.id),
                "",
                PanelId::bare(crate::panel::Tag("about")),
            )]
        }
    }

    fn store() -> Store {
        Store::open(None, &[]).expect("in-memory store")
    }

    /// The one reading of a query the whole list shares.
    #[test]
    fn a_query_is_words_cut_at_the_punctuation_and_matched_by_prefix() {
        assert_eq!(terms("  vera@kovac.io "), vec!["vera", "kovac", "io"]);
        assert_eq!(terms("Re: Q3!"), vec!["re", "q3"]);
        assert_eq!(terms("Вера"), vec!["вера"]);
        assert!(terms("  *^\" ").is_empty(), "no word in it at all");

        let hay = &["Vera Kovac", "vera@kovac.io", "Q3 infra budget"];
        assert!(
            matches(&terms(""), hay),
            "an empty query matches everything"
        );
        assert!(matches(&terms("kov"), hay), "a word inside an address");
        assert!(matches(&terms("vera q3"), hay), "every term must land");
        assert!(matches(&terms("VERA"), hay), "case is nothing");
        assert!(
            !matches(&terms("ovac"), hay),
            "the middle of a word is not a prefix"
        );
        assert!(
            !matches(&terms("vera zzz"), hay),
            "one term short is no match"
        );
    }

    #[test]
    fn inline_answers_in_the_same_tick_in_registration_order() {
        let s = store();
        let asked = Arc::new(AtomicUsize::new(0));
        let mut e = Engine::inline(vec![
            Box::new(Echo {
                id: "a",
                asked: asked.clone(),
            }),
            Box::new(Echo {
                id: "b",
                asked: asked.clone(),
            }),
        ]);
        assert_eq!(e.slots(), 2);
        e.ask(&s, 1, "vera");
        let out = e.collect();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].slot, 0);
        assert_eq!(out[0].hits[0].label, "a:vera");
        assert_eq!(out[1].slot, 1);
        assert_eq!(out[1].hits[0].label, "b:vera");
        assert_eq!(asked.load(Ordering::Relaxed), 2);
        // Collected once, gone.
        assert!(e.collect().is_empty());
    }

    /// The question a provider is answering can go stale under it, and a
    /// slow one is expected to notice.
    #[test]
    fn a_newer_question_abandons_the_older_one() {
        let s = store();
        let mut e = Engine::inline(vec![Box::new(Echo {
            id: "a",
            asked: Arc::new(AtomicUsize::new(0)),
        })]);
        e.ask(&s, 1, "v");
        // Asking again drops what the first ask produced: an inline engine
        // has no channel for a stale answer to sit in.
        e.ask(&s, 2, "ve");
        let out = e.collect();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].gen, 2);

        assert!(e.token(1).yes(), "generation 1 is over");
        assert!(!e.token(2).yes(), "generation 2 is the question");
    }

    /// The threaded engine is the production one: every provider answers,
    /// off the calling thread, and the answers carry their generation.
    #[test]
    fn threads_answer_off_the_caller_and_stamp_their_generation() {
        let s = store();
        let db = s.db();
        let woke = Arc::new(AtomicUsize::new(0));
        let w = woke.clone();
        let mut e = Engine::threads(
            &db,
            vec![
                Box::new(Echo {
                    id: "a",
                    asked: Arc::new(AtomicUsize::new(0)),
                }),
                Box::new(Echo {
                    id: "b",
                    asked: Arc::new(AtomicUsize::new(0)),
                }),
            ],
            move || {
                w.fetch_add(1, Ordering::Relaxed);
            },
        );
        e.ask(&s, 7, "vera");
        let mut out: Vec<Answer> = Vec::new();
        for _ in 0..2000 {
            out.extend(e.collect());
            if out.len() == 2 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(out.len(), 2, "both providers answered");
        assert!(out.iter().all(|a| a.gen == 7));
        let mut slots: Vec<usize> = out.iter().map(|a| a.slot).collect();
        slots.sort_unstable();
        assert_eq!(slots, vec![0, 1]);
        assert!(woke.load(Ordering::Relaxed) >= 2, "the UI was woken");
    }

    // -- the question a panel holds ------------------------------------------

    /// A source that offers `n` rows named after itself, once released.
    struct Held {
        id: &'static str,
        base: i64,
        go: std::sync::Mutex<Option<mpsc::Receiver<()>>>,
    }

    impl Held {
        fn new(id: &'static str, base: i64) -> (Held, mpsc::Sender<()>) {
            let (tx, rx) = mpsc::channel();
            (
                Held {
                    id,
                    base,
                    go: std::sync::Mutex::new(Some(rx)),
                },
                tx,
            )
        }
    }

    impl Provider for Held {
        fn id(&self) -> &'static str {
            self.id
        }
        fn search(&self, _s: &Store, _q: &str, _a: &Abandoned) -> Vec<Hit> {
            if let Some(rx) = self.go.lock().expect("the gate").as_ref() {
                let _ = rx.recv();
            }
            (0..3)
                .map(|i| {
                    Hit::found(
                        format!("{}{i}", self.id),
                        "",
                        PanelId::new(crate::panel::Tag("job"), [(self.base + i).to_string()]),
                    )
                })
                .collect()
        }
    }

    fn settle(q: &mut Query, n: usize) {
        for _ in 0..2000 {
            q.collect();
            if q.found().len() == n {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("the list never reached {n} rows: {q:?}");
    }

    /// A question is put to every source at once; each row says which
    /// source found it, and the list is in the order the sources were
    /// registered in whoever answered first — so a late first source
    /// inserts its band above the rows a quick second one already put on
    /// screen.
    #[test]
    fn the_rows_are_in_registration_order_whoever_answers_first() {
        let s = store();
        let (a, release_a) = Held::new("a", 10);
        let (b, release_b) = Held::new("b", 20);
        let mut q = Query::new(Engine::threads(
            &s.db(),
            vec![Box::new(a), Box::new(b)],
            || {},
        ));
        assert_eq!(q.sources(), ["a", "b"]);

        q.ask(&s, "anything");
        assert!(q.found().is_empty(), "nobody has answered yet");
        assert!(q.pending(), "and the list says so");

        release_b.send(()).expect("release b");
        settle(&mut q, 3);
        assert_eq!(
            q.found()
                .iter()
                .map(|f| f.label.as_str())
                .collect::<Vec<_>>(),
            ["b0", "b1", "b2"]
        );
        assert!(q.pending(), "a still owes an answer");

        release_a.send(()).expect("release a");
        settle(&mut q, 6);
        assert_eq!(
            q.found()
                .iter()
                .map(|f| f.label.as_str())
                .collect::<Vec<_>>(),
            ["a0", "a1", "a2", "b0", "b1", "b2"],
            "the registration order: a's band lands above b's, late as it was"
        );
        assert_eq!(q.found()[0].source, "a");
        assert_eq!(q.found()[3].source, "b");
        assert!(!q.pending(), "both have spoken");
    }

    /// The question is asked of nobody when there is nothing in it, and an
    /// answer to a question typed past is not shown.
    #[test]
    fn an_empty_question_asks_nothing_and_a_stale_answer_is_dropped() {
        struct Echo;
        impl Provider for Echo {
            fn id(&self) -> &'static str {
                "echo"
            }
            fn search(&self, _s: &Store, q: &str, _a: &Abandoned) -> Vec<Hit> {
                vec![Hit::found(
                    format!("echo {q}"),
                    "said",
                    PanelId::new(crate::panel::Tag("job"), [q.to_string()]),
                )]
            }
        }
        let s = store();
        let mut q = Query::new(Engine::inline(vec![Box::new(Echo)]));

        q.ask(&s, "one");
        assert_eq!(q.found().len(), 1);
        assert_eq!(q.found()[0].label, "echo one");
        assert_eq!(q.found()[0].detail, "said");
        assert_eq!(q.found()[0].source, "echo");

        q.ask(&s, "two");
        q.collect();
        assert_eq!(q.found().len(), 1);
        assert_eq!(q.found()[0].label, "echo two", "the older answer is gone");

        q.ask(&s, "   ");
        assert!(q.found().is_empty(), "nothing is asked for nothing");
        assert!(!q.pending(), "and nothing is owed");
        assert!(format!("{q:?}").contains("Query"));
    }

    /// One panel, offered by two sources, is one row. However much they
    /// find, the list stops at a screenful of the best.
    #[test]
    fn a_panel_is_listed_once_and_the_list_stops_at_a_screenful() {
        struct Same;
        impl Provider for Same {
            fn id(&self) -> &'static str {
                "same"
            }
            fn search(&self, _s: &Store, _q: &str, _a: &Abandoned) -> Vec<Hit> {
                vec![Hit::found(
                    "about",
                    "",
                    PanelId::bare(crate::panel::Tag("about")),
                )]
            }
        }
        struct Flood;
        impl Provider for Flood {
            fn id(&self) -> &'static str {
                "flood"
            }
            fn search(&self, _s: &Store, _q: &str, _a: &Abandoned) -> Vec<Hit> {
                (0..MAX_FOUND * 3)
                    .map(|i| {
                        Hit::found(
                            format!("row {i}"),
                            "",
                            PanelId::new(crate::panel::Tag("job"), [i.to_string()]),
                        )
                    })
                    .collect()
            }
        }
        let s = store();
        let mut q = Query::new(Engine::inline(vec![Box::new(Same), Box::new(Same)]));
        q.ask(&s, "about");
        assert_eq!(q.found().len(), 1, "{:?}", q.found());

        let mut q = Query::new(Engine::inline(vec![Box::new(Flood)]));
        q.ask(&s, "anything");
        assert_eq!(q.found().len(), MAX_FOUND);
    }
}
