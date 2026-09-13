//! One grammar topic: the rule, its tables, examples and tips, the
//! learner's own notes on it, and the topics it leads to.

use std::any::Any;
use std::rc::Rc;

use kernel::layout::SlotId;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag};
use kernel::store::Store;

use super::super::model::{self, Section, TopicNote, TopicRow};

pub struct Topic {
    id: PanelId,
    slot: SlotId,
    topic: String,
    store: Rc<Store>,
}

impl Topic {
    pub const TAG: Tag = Tag("topic");

    #[must_use]
    pub fn id(topic: &str) -> PanelId {
        PanelId::new(Self::TAG, [topic])
    }

    #[must_use]
    pub fn slot(&self) -> SlotId {
        self.slot
    }

    #[must_use]
    pub fn topic(&self) -> Option<TopicRow> {
        model::topic(&self.store, &self.topic)
    }

    #[must_use]
    pub fn sections(&self) -> Vec<Section> {
        self.topic().map(|t| model::sections(&t.sections)).unwrap_or_default()
    }

    #[must_use]
    pub fn notes(&self) -> Rc<Vec<TopicNote>> {
        model::topic_notes(&self.store, &self.topic)
    }

    /// The titles of the related topics, for the links at the foot.
    #[must_use]
    pub fn related(&self) -> Vec<(String, String)> {
        self.topic()
            .map(|t| {
                t.related
                    .iter()
                    .filter_map(|id| model::topic(&self.store, id).map(|r| (id.clone(), r.title)))
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl Panel for Topic {
    fn id(&self) -> &PanelId {
        &self.id
    }
    fn title(&self) -> String {
        self.topic().map_or_else(|| "topic".into(), |t| t.title)
    }
    fn about(&self) -> String {
        format!(
            "The grammar topic {}: the rule in the course's language with its level, \
             category and mastery stamp, then its sections in order — explanation, declension \
             tables, examples with notes, a tip — the lessons it was introduced and last \
             practiced in, the learner's own stumbles on it, and links to related topics, \
             which replace this panel in place.",
            self.topic
        )
    }
    fn context_text_columns(&self) -> &'static [&'static str] {
        &["sections", "note"]
    }
    fn wish(&self, _: usize) -> (u32, u32) {
        (5, 6)
    }
    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

pub struct TopicKind;
impl PanelKind for TopicKind {
    fn tag(&self) -> Tag {
        Topic::TAG
    }
    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        Box::new(Topic {
            id: id.clone(),
            slot: 0,
            topic: id.arg(0).unwrap_or("").to_string(),
            store: cx.session().store().clone(),
        })
    }
}
