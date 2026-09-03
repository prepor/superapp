//! Search: many sources, one list, and none of them on the frame.
//!
//! A launcher over a real world asks several different things at once — the
//! windows that are open, the mail index, tomorrow the files on disk and the
//! chats on a server — and they answer at wildly different speeds. So
//! nothing here is a call that returns a list. A question is **put**
//! ([`Engine::ask`]) and answers arrive when they arrive
//! ([`Engine::collect`]), each stamped with the question it belongs to;
//! anything stamped with an older one goes on the floor.
//!
//! Each [`Provider`] gets a thread of its own and a store connection of its
//! own, so the one that waits on a server never delays the one that reads an
//! index. A provider that can take a while is handed an [`Abandoned`] and is
//! expected to look at it: the moment a newer question is put, whatever it
//! is still doing is worth nothing.
//!
//! Under a headless build the same providers answer **inline**, on the
//! calling thread, at ask time — so a scripted `type` is followed by its
//! rows in the same tick, and a suite stays reproducible. It is the split
//! [`crate::sync::Pump`] already makes for the mail passes, for the same
//! reason.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};

use crate::core::{Kind, PanelId};
use crate::store::{Db, Store};

/// What activating a hit does.
#[derive(Debug, Clone, PartialEq)]
pub enum Go {
    /// Switch to the workspace holding this panel and focus it.
    Focus(PanelId),
    /// Open a fresh un-joined panel on the active workspace.
    Open(Kind),
}

/// One row of the list.
///
/// A provider fills in what it knows — the words and the kind — and leaves
/// the verb alone. Whether a kind is opened or *gone to* depends on the
/// windows, which live on the UI thread and are none of a worker's
/// business; [`crate::launcher::Search`] settles it on the merge.
#[derive(Debug, Clone)]
pub struct Hit {
    /// Primary text: the panel title / subject / person.
    pub label: String,
    /// Muted secondary text: sender, address, kind.
    pub detail: String,
    /// The workspace the panel lives on, when it is already open. Filled in
    /// by the merge for anything a provider found.
    pub ws: Option<usize>,
    /// What enter does.
    pub go: Go,
}

impl Hit {
    /// A hit as a provider knows it: something to open, wherever it turns
    /// out to already live.
    pub fn found(label: impl Into<String>, detail: impl Into<String>, kind: Kind) -> Self {
        Hit {
            label: label.into(),
            detail: detail.into(),
            ws: None,
            go: Go::Open(kind),
        }
    }
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
/// it owns, exactly as a [`crate::sync`] worker owns its own `World`.
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
        let mut asks = Vec::new();
        for (slot, p) in providers.into_iter().enumerate() {
            let (ask_tx, ask_rx) = mpsc::channel::<Ask>();
            let (db, tx, newest, notify) = (db.clone(), tx.clone(), newest.clone(), notify.clone());
            let name = p.id();
            match std::thread::Builder::new()
                .name(format!("search-{name}"))
                .spawn(move || provider_loop(db, slot, p, &ask_rx, &tx, &newest, notify))
            {
                Ok(_) => asks.push(ask_tx),
                // A source that could not be spawned is simply one the list
                // never shows; the rest still answer.
                Err(e) => eprintln!("search: {name} did not start: {e}"),
            }
        }
        Engine {
            newest,
            mode: Mode::Threads { asks, answers },
        }
    }

    /// The same providers, run where they are asked.
    #[must_use]
    pub fn inline(providers: Vec<Box<dyn Provider>>) -> Engine {
        Engine {
            newest: Arc::default(),
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
    // The worker joins the *one* writer (CR-005 phase 0) — its own reader
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
            vec![Hit::found(format!("{}:{query}", self.id), "", Kind::About)]
        }
    }

    fn store() -> Store {
        Store::open(None).expect("in-memory store")
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
}
