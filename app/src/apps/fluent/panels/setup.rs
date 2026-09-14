//! Setting the course up: who is learning what, and how much a day.
//!
//! One row of `fluent_learner`, written whole by one verb. It is the
//! course's only form: everything else in fluent is authored by the tutor
//! or answered by the learner, and this is the one thing neither can know.

use std::any::Any;
use std::rc::Rc;

use kernel::layout::SlotId;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::session::Session;
use kernel::store::Store;

use super::super::model;

/// The CEFR ladder, which both the level and the goal are a rung of.
pub const LEVELS: [&str; 6] = ["A1", "A2", "B1", "B2", "C1", "C2"];

/// The form, field by field, as the widget mirrors it.
pub struct Setup {
    id: PanelId,
    slot: SlotId,
    pub name: String,
    pub native: String,
    pub target: String,
    pub level: String,
    pub goal: String,
    /// Kept as what was typed, so a half-typed number is not rounded under
    /// the caret; the save reads it.
    pub minutes: String,
    pub error: String,
}

impl Setup {
    pub const TAG: Tag = Tag("fluent-setup");

    #[must_use]
    pub fn id() -> PanelId {
        PanelId::bare(Self::TAG)
    }

    /// What the form holds when it opens: the learner's own answers where
    /// there is a row, and the course's defaults where there is not.
    fn of(store: &Rc<Store>) -> Setup {
        let learner = model::learner(store);
        Setup {
            id: Setup::id(),
            slot: 0,
            name: learner.as_ref().map(|l| l.name.clone()).unwrap_or_default(),
            native: learner.as_ref().map(|l| l.native.clone()).unwrap_or_default(),
            target: learner.as_ref().map(|l| l.target.clone()).unwrap_or_default(),
            level: learner.as_ref().map_or_else(|| "A1".into(), |l| l.level.clone()),
            goal: learner.as_ref().map_or_else(|| "B1".into(), |l| l.goal.clone()),
            minutes: learner.as_ref().map_or_else(|| "30".into(), |l| l.daily_minutes.to_string()),
            error: String::new(),
        }
    }

    /// Whether there is a learner row at all, which is what the desk's own
    /// *set up* turns on.
    #[must_use]
    pub fn learner_set(store: &Store) -> bool {
        model::learner(store).is_some()
    }

    /// *save*: one row, one undoable action. The first save stamps the day
    /// the course began; a later one leaves that where it is.
    pub fn save(&mut self, s: &mut Session) {
        let name = self.name.trim().to_string();
        let (native, target) = (self.native.trim().to_string(), self.target.trim().to_string());
        if name.is_empty() || native.is_empty() || target.is_empty() {
            self.error = "a name, a language you speak and one you are learning".into();
            s.redraw();
            return;
        }
        let level = normal_level(&self.level, "A1");
        let goal = normal_level(&self.goal, "B1");
        let minutes = self.minutes.trim().parse::<i64>().unwrap_or(0);
        if !(5..=240).contains(&minutes) {
            self.error = "daily minutes is a number between 5 and 240".into();
            s.redraw();
            return;
        }
        self.error.clear();
        if model::save_learner(
            s,
            &model::Learner {
                name,
                native,
                target,
                level,
                goal,
                daily_minutes: minutes,
                streak: 0,
                last_active: None,
            },
        ) {
            s.notify("the course is set up", false);
        } else {
            self.error = "the store refused the learner".into();
        }
        s.redraw();
    }
}

/// A level as the ladder spells it, or the default for anything else: a
/// typed field takes whatever is typed, and the row holds one of six.
fn normal_level(typed: &str, fallback: &str) -> String {
    let want = typed.trim().to_uppercase();
    LEVELS
        .iter()
        .find(|l| **l == want)
        .map_or_else(|| fallback.to_string(), |l| (*l).to_string())
}

impl Panel for Setup {
    fn id(&self) -> &PanelId {
        &self.id
    }
    fn title(&self) -> String {
        "fluent setup".into()
    }
    fn about(&self) -> String {
        "The course's one form: the learner's name, the language they speak and the one \
         they are learning, where they stand on the CEFR ladder and where they are going, \
         and how many minutes a day they study. Save writes the single fluent_learner row \
         — the tutor is told all of it before it authors anything — and one undo takes it \
         back."
            .into()
    }
    fn wish(&self, _: usize) -> (u32, u32) {
        (4, 4)
    }
    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }
    fn verbs(&self) -> Vec<Verb> {
        vec![Verb::run("fluent.save", "save", Some('s'))]
    }
    fn run(&mut self, verb: &str, s: &mut Session) {
        if verb == "fluent.save" {
            self.save(s);
        }
    }
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

pub struct SetupKind;
impl PanelKind for SetupKind {
    fn tag(&self) -> Tag {
        Setup::TAG
    }
    fn open(&self, _: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        Box::new(Setup::of(cx.session().store()))
    }
}
