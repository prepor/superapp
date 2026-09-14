//! The three rich tables: the deck, the grammar, and the lessons played.

use std::any::Any;

use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::richtable::{ListState, SqlSource};

use super::super::model::{self, CardRow, LessonRow, TopicRow};
use super::Review;

pub type CardList = ListState<&'static SqlSource<CardRow, String>>;
pub type TopicList = ListState<&'static SqlSource<TopicRow, String>>;
pub type LessonList = ListState<&'static SqlSource<LessonRow, i64>>;

pub struct Cards {
    id: PanelId,
    slot: SlotId,
    pub list: CardList,
    pub filter: String,
}

impl Cards {
    pub const TAG: Tag = Tag("cards");

    #[must_use]
    pub fn id() -> PanelId {
        PanelId::bare(Self::TAG)
    }
}

impl Panel for Cards {
    fn id(&self) -> &PanelId {
        &self.id
    }
    fn title(&self) -> String {
        "cards".into()
    }
    fn about(&self) -> String {
        "The deck: every flashcard with its schedule, soonest due first. A row is the word \
         and its meaning, with when it comes due and its mastery; @new, @weak, @learned and \
         @mastery narrow the list, free text matches the word, the meaning and the example. \
         The cursor previews the card; review plays the ones due today."
            .into()
    }
    fn wish(&self, _: usize) -> (u32, u32) {
        (5, 6)
    }
    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }
    fn persist(&self) -> PanelId {
        PanelId::new(Self::TAG, [self.list.table().filter().to_string()])
    }
    fn root(&self) -> PanelId {
        Self::id()
    }
    fn verbs(&self) -> Vec<Verb> {
        vec![Verb::go(
            "fluent.review",
            "review",
            Some('r'),
            Nav::Open { from: self.slot, id: Review::id(), fresh: false },
        )]
    }
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

pub struct CardsKind;
impl PanelKind for CardsKind {
    fn tag(&self) -> Tag {
        Cards::TAG
    }
    fn open(&self, id: &PanelId, _: &mut Opening<'_>) -> Box<dyn Panel> {
        let filter = id.arg(0).unwrap_or("").to_string();
        let mut list = ListState::new(&model::CARDS, 50);
        list.set_filter(&filter);
        Box::new(Cards { id: id.clone(), slot: 0, list, filter })
    }
}

pub struct Grammar {
    id: PanelId,
    pub list: TopicList,
    pub filter: String,
}

impl Grammar {
    pub const TAG: Tag = Tag("grammar");

    #[must_use]
    pub fn id() -> PanelId {
        PanelId::bare(Self::TAG)
    }
}

impl Panel for Grammar {
    fn id(&self) -> &PanelId {
        &self.id
    }
    fn title(&self) -> String {
        "grammar".into()
    }
    fn about(&self) -> String {
        "The grammar reference the tutor keeps: one topic per rule a lesson has covered, \
         grouped by category, each with its CEFR level and a mastery stamp from the items it \
         links to. @level and @category narrow it; the cursor previews the topic with its \
         tables, examples, tips and the learner's own notes."
            .into()
    }
    fn wish(&self, _: usize) -> (u32, u32) {
        (5, 6)
    }
    fn persist(&self) -> PanelId {
        PanelId::new(Self::TAG, [self.list.table().filter().to_string()])
    }
    fn root(&self) -> PanelId {
        Self::id()
    }
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

pub struct GrammarKind;
impl PanelKind for GrammarKind {
    fn tag(&self) -> Tag {
        Grammar::TAG
    }
    fn open(&self, id: &PanelId, _: &mut Opening<'_>) -> Box<dyn Panel> {
        let filter = id.arg(0).unwrap_or("").to_string();
        let mut list = ListState::new(&model::TOPICS, 50);
        list.set_filter(&filter);
        Box::new(Grammar { id: id.clone(), list, filter })
    }
}

pub struct History {
    id: PanelId,
    pub list: LessonList,
    pub filter: String,
}

impl History {
    pub const TAG: Tag = Tag("lessons");

    #[must_use]
    pub fn id() -> PanelId {
        PanelId::bare(Self::TAG)
    }
}

impl Panel for History {
    fn id(&self) -> &PanelId {
        &self.id
    }
    fn title(&self) -> String {
        "history".into()
    }
    fn about(&self) -> String {
        "Every lesson played, newest first: the day, the title, the accuracy and the minutes. \
         @date and @accuracy narrow it; the cursor previews a lesson's summary with its \
         corrections and the tutor's notes."
            .into()
    }
    fn wish(&self, _: usize) -> (u32, u32) {
        (5, 6)
    }
    fn persist(&self) -> PanelId {
        PanelId::new(Self::TAG, [self.list.table().filter().to_string()])
    }
    fn root(&self) -> PanelId {
        Self::id()
    }
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

pub struct HistoryKind;
impl PanelKind for HistoryKind {
    fn tag(&self) -> Tag {
        History::TAG
    }
    fn open(&self, id: &PanelId, _: &mut Opening<'_>) -> Box<dyn Panel> {
        let filter = id.arg(0).unwrap_or("").to_string();
        let mut list = ListState::new(&model::LESSONS, 50);
        list.set_filter(&filter);
        Box::new(History { id: id.clone(), list, filter })
    }
}
