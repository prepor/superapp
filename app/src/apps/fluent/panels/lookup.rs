//! One word, looked up: what it means, and where the answer came from.
//!
//! A selection in a lesson makes this panel, joined to the lesson it was
//! selected in. It resolves the word in the order that costs least:
//!
//! 1. **the deck** — a card whose front is this word, with or without the
//!    article either of them wears. Instant, offline, and already the
//!    learner's own.
//! 2. **the cache** — what the tutor said this word meant the last time
//!    anybody asked. A word costs one question ever.
//! 3. **the tutor** — one question to the model, with no chat behind it,
//!    asked on a worker while this panel says it is asking.
//!
//! The panel says which of the three answered, because *from your deck* and
//! *from the tutor* are worth different amounts to a person reading them.
//! A word that came from outside the deck can be put in it — **add** files
//! the item and the card, one undoable action, the way the tutor's own
//! cards land — and after that the bar offers the card instead.

use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;

use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::session::Session;
use kernel::store::Store;

use super::super::model::{self, CardRow, Entry};
use super::super::tutor;
use super::{speak, Card};

/// What answered, and what it said.
#[derive(Clone, Debug, PartialEq)]
pub enum Found {
    /// Nothing asked yet — the first poll resolves the word.
    Fresh,
    /// A card of the deck, under the item it is filed as.
    Deck(String, Entry),
    /// What the tutor said about this word before.
    Cache(Entry),
    /// A question is out.
    Asking,
    /// What the tutor has just said.
    Tutor(Entry),
    /// Why there is no answer, in the shell's red.
    Failed(String),
}

impl Found {
    /// The entry on the screen, whoever answered it.
    #[must_use]
    pub fn entry(&self) -> Option<&Entry> {
        match self {
            Found::Deck(_, e) | Found::Cache(e) | Found::Tutor(e) => Some(e),
            Found::Fresh | Found::Asking | Found::Failed(_) => None,
        }
    }

    /// The line under the word: which of the three answered, or what is
    /// happening instead.
    #[must_use]
    pub fn source(&self) -> &'static str {
        match self {
            Found::Deck(..) => "from your deck",
            Found::Cache(_) => "from the cache",
            Found::Tutor(_) => "from the tutor",
            Found::Asking | Found::Fresh => "asking the tutor…",
            Found::Failed(_) => "",
        }
    }
}

pub struct Lookup {
    id: PanelId,
    slot: SlotId,
    /// The word as it was selected, which is what the panel is named and
    /// titled by even once the tutor has given it its dictionary form.
    term: String,
    store: Rc<Store>,
    /// What the word came to. Shared with the question in flight, which
    /// lands on it from the completion — the panel is not borrowed there,
    /// and a panel that was closed meanwhile is simply not drawn again.
    found: Rc<RefCell<Found>>,
}

impl Lookup {
    pub const TAG: Tag = Tag("lookup");

    #[must_use]
    pub fn id(term: &str) -> PanelId {
        PanelId::new(Self::TAG, [term])
    }

    #[must_use]
    pub fn found(&self) -> Found {
        self.found.borrow().clone()
    }

    /// The word as it is shown: the dictionary form where there is one,
    /// and what was selected until then.
    #[must_use]
    pub fn shown_term(&self) -> String {
        self.found
            .borrow()
            .entry()
            .map_or_else(|| self.term.clone(), |e| e.term.clone())
    }

    /// Resolves the word, once. Called from the panel's own widget on
    /// every event, as the import form's poll is: the deck and the cache
    /// answer here and now, and only a word neither of them knows is asked
    /// of the tutor.
    pub fn poll(&mut self, s: &mut Session) {
        if *self.found.borrow() != Found::Fresh {
            return;
        }
        if let Some(card) = model::card_for_term(&self.store, &self.term) {
            self.settle(Found::Deck(card.item.clone(), of_card(&card)));
            return;
        }
        if let Some(entry) = model::cached_lookup(&self.store, &self.term) {
            self.settle(Found::Cache(entry));
            return;
        }
        self.settle(Found::Asking);
        let (cell, store, now) = (self.found.clone(), self.store.clone(), s.now());
        let asked = tutor::look_up(s, &self.term, move |s, answer| {
            *cell.borrow_mut() = match answer {
                Ok(entry) => {
                    // A cache and not a decision: a plain write, so no
                    // undo has a dictionary entry in it.
                    model::cache_lookup(&store, &entry, now);
                    Found::Tutor(entry)
                }
                Err(why) => Found::Failed(why),
            };
            s.redraw();
        });
        if let Err(why) = asked {
            self.settle(Found::Failed(why));
        }
        s.redraw();
    }

    /// The word put in the deck: one item and one card, one undo.
    fn add(&mut self, s: &mut Session) {
        let Some(entry) = self.found.borrow().entry().cloned() else {
            return;
        };
        match model::add_word(s, &entry) {
            Some(item) => {
                s.notify(format!("„{}“ added to the deck", entry.term), false);
                self.settle(Found::Deck(item, entry));
            }
            None => s.notify("that word is in the deck already", false),
        }
        s.redraw();
    }

    fn settle(&mut self, found: Found) {
        *self.found.borrow_mut() = found;
    }
}

/// A card of the deck, read as a dictionary entry. The part of speech is
/// the one thing a card does not carry — its note says what it has to say.
fn of_card(c: &CardRow) -> Entry {
    Entry {
        term: c.front.clone(),
        translation: c.back.clone(),
        pos: String::new(),
        note: c.notes.clone(),
    }
}

impl Panel for Lookup {
    fn id(&self) -> &PanelId {
        &self.id
    }
    fn title(&self) -> String {
        self.term.clone()
    }
    fn about(&self) -> String {
        format!(
            "The word „{}“, selected in a lesson and looked up: the dictionary form — a noun \
             with its article — what it means in the learner's language with an English gloss, \
             the part of speech and one note. It is resolved in the order that costs least: a \
             card of the deck whose front is this word, with or without an article; then this \
             device's cache of what the tutor once said; then one question to the tutor, asked \
             with no chat behind it and kept in the cache after. The line under the word says \
             which of the three answered. play speaks it; a word that is in the deck links to \
             its card, and one that is not can be added to the deck as an item and a card, \
             which one undo takes back.",
            self.term
        )
    }
    fn context_text_columns(&self) -> &'static [&'static str] {
        &["back", "notes", "translation", "note"]
    }
    fn wish(&self, _: usize) -> (u32, u32) {
        (4, 3)
    }
    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }
    fn verbs(&self) -> Vec<Verb> {
        let mut v = vec![Verb::run("fluent.play", "play", Some('y'))];
        match self.found() {
            Found::Deck(item, _) => v.push(Verb::go(
                "fluent.card",
                "card",
                Some('c'),
                Nav::Open {
                    from: self.slot,
                    id: Card::id(&item),
                    fresh: false,
                },
            )),
            Found::Cache(_) | Found::Tutor(_) => {
                v.push(Verb::run("fluent.add", "add", Some('d')));
            }
            Found::Fresh | Found::Asking | Found::Failed(_) => {}
        }
        v
    }
    fn run(&mut self, verb: &str, s: &mut Session) {
        match verb {
            "fluent.play" => {
                let word = self.shown_term();
                speak(s, &word);
            }
            "fluent.add" => self.add(s),
            _ => {}
        }
    }
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

pub struct LookupKind;
impl PanelKind for LookupKind {
    fn tag(&self) -> Tag {
        Lookup::TAG
    }
    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        Box::new(Lookup {
            id: id.clone(),
            slot: 0,
            term: id.arg(0).unwrap_or("").trim().to_string(),
            store: cx.session().store().clone(),
            found: Rc::new(RefCell::new(Found::Fresh)),
        })
    }
}
