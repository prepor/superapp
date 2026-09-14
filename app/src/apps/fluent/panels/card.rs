//! One card, whole: the word, the meaning, the example, the note, its
//! schedule, and every grade it was ever given.

use std::any::Any;
use std::rc::Rc;

use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::session::Session;
use kernel::store::Store;

use super::super::model::{self, CardRow, ReviewRow};
use super::speak;

pub struct Card {
    id: PanelId,
    item: String,
    store: Rc<Store>,
}

impl Card {
    pub const TAG: Tag = Tag("card");

    #[must_use]
    pub fn id(item: &str) -> PanelId {
        PanelId::new(Self::TAG, [item])
    }

    #[must_use]
    pub fn card(&self) -> Option<CardRow> {
        model::card(&self.store, &self.item)
    }

    #[must_use]
    pub fn reviews(&self) -> Rc<Vec<ReviewRow>> {
        model::reviews_of(&self.store, &self.item)
    }
}

impl Panel for Card {
    fn id(&self) -> &PanelId {
        &self.id
    }
    fn title(&self) -> String {
        self.card().map_or_else(|| "card".into(), |c| c.front)
    }
    fn about(&self) -> String {
        format!(
            "One flashcard, {}: the word with its article, the meaning in the learner's \
             language, an example sentence, the tutor's note, then its SM-2 state — ease, \
             reviews, when it is next due, mastery — and every grade it was given, newest \
             first.",
            self.item
        )
    }
    fn context_text_columns(&self) -> &'static [&'static str] {
        &["example", "notes"]
    }
    fn wish(&self, _: usize) -> (u32, u32) {
        (4, 4)
    }
    fn verbs(&self) -> Vec<Verb> {
        vec![Verb::run("fluent.play", "play", Some('y'))]
    }
    fn run(&mut self, verb: &str, s: &mut Session) {
        if verb == "fluent.play" {
            let audio = self.card().map(|c| c.audio).unwrap_or_default();
            speak(s, &audio);
        }
    }
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

pub struct CardKind;
impl PanelKind for CardKind {
    fn tag(&self) -> Tag {
        Card::TAG
    }
    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        Box::new(Card {
            id: id.clone(),
            item: id.arg(0).unwrap_or("").to_string(),
            store: cx.session().store().clone(),
        })
    }
}
