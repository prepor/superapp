//! The topic picker. Checking a row changes this app's chat list immediately.

use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::session::Session;
use kernel::store::Store;
use std::{any::Any, rc::Rc};

use super::super::{
    model::{self, PeerId},
    runtime,
    topics::{self, Topic},
    verbs,
};
use super::{Chat, Chats};

pub struct Topics {
    id: PanelId,
    chat: PeerId,
    store: Rc<Store>,
    slot: SlotId,
    filter: String,
    cursor: Option<i64>,
}

impl Topics {
    pub const TAG: Tag = Tag("telegram-topics");

    pub fn id(chat: PeerId) -> PanelId {
        PanelId::new(Self::TAG, [chat.to_string()])
    }

    pub fn rows(&self) -> Vec<Topic> {
        let terms: Vec<String> = self
            .filter
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        topics::list(&self.store, self.chat)
            .iter()
            .filter(|t| terms.iter().all(|s| t.name.to_lowercase().contains(s)))
            .cloned()
            .collect()
    }

    pub fn set_filter(&mut self, filter: String) {
        self.filter = filter;
    }
    pub fn cursor(&self) -> Option<i64> {
        self.cursor
    }

    pub fn walk(&mut self, delta: isize) -> Option<usize> {
        let rows = self.rows();
        if rows.is_empty() {
            self.cursor = None;
            return None;
        }
        let index = self
            .cursor
            .and_then(|id| rows.iter().position(|t| t.id == id))
            .map_or(0, |i| {
                (i as isize + delta).clamp(0, rows.len() as isize - 1) as usize
            });
        self.cursor = Some(rows[index].id);
        Some(index)
    }

    pub fn toggle(&mut self, id: i64, s: &mut Session) {
        let Some(topic) = topics::get(&self.store, self.chat, id) else {
            return;
        };
        self.cursor = Some(id);
        self.select(vec![id], !topic.selected, s);
    }

    fn select(&self, ids: Vec<i64>, selected: bool, s: &mut Session) {
        verbs::select_topics(s, self.chat, ids, selected);
        s.redraw();
    }

    pub fn status(&self) -> String {
        match runtime::of(&self.store).topics_status(self.chat) {
            Ok(true) => "loading topics…".into(),
            Err(error) => error,
            Ok(false) => {
                let topics = topics::list(&self.store, self.chat);
                format!(
                    "{} of {} topics shown as chats",
                    topics.iter().filter(|t| t.selected).count(),
                    topics.len()
                )
            }
        }
    }

    pub fn empty(&self) -> &'static str {
        if self.filter.trim().is_empty() {
            "no topics loaded yet · refresh to load"
        } else {
            "no topics match this filter"
        }
    }
}

impl Panel for Topics {
    fn id(&self) -> &PanelId {
        &self.id
    }
    fn title(&self) -> String {
        let name = model::peer(&self.store, self.chat).map_or_else(|| "group".into(), |c| c.name);
        format!("topics · {name}")
    }
    fn wish(&self, _cols: usize) -> (u32, u32) {
        (4, 6)
    }
    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }
    fn verbs(&self) -> Vec<Verb> {
        let filtered = !self.filter.trim().is_empty();
        let mut v = vec![
            Verb::go(
                "telegram.chats",
                "chats",
                Some('c'),
                Nav::Open {
                    from: self.slot,
                    id: Chats::id(),
                    fresh: false,
                },
            ),
            Verb::run("telegram.refresh_topics", "refresh", Some('r')),
            Verb::run(
                "telegram.show_topics",
                if filtered { "show matches" } else { "show all" },
                Some('s'),
            ),
            Verb::run(
                "telegram.hide_topics",
                if filtered { "hide matches" } else { "hide all" },
                Some('h'),
            ),
        ];
        if let Some(id) = self.cursor {
            v.push(Verb::go(
                "telegram.open_topic",
                "open chat",
                Some('o'),
                Nav::Open {
                    from: self.slot,
                    id: Chat::topic(self.chat, id),
                    fresh: false,
                },
            ));
        }
        v
    }
    fn run(&mut self, verb: &str, s: &mut Session) {
        match verb {
            "telegram.refresh_topics" => runtime::of(&self.store).refresh_topics(self.chat),
            "telegram.show_topics" | "telegram.hide_topics" => {
                self.select(
                    self.rows().iter().map(|t| t.id).collect(),
                    verb == "telegram.show_topics",
                    s,
                );
            }
            _ => {}
        }
        s.redraw();
    }
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

pub struct TopicsKind;
impl PanelKind for TopicsKind {
    fn tag(&self) -> Tag {
        Topics::TAG
    }
    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        let store = cx.session().store().clone();
        let chat = id.arg(0).and_then(|s| s.parse().ok()).unwrap_or_default();
        runtime::of(&store).refresh_topics(chat);
        Box::new(Topics {
            id: id.clone(),
            chat,
            store,
            slot: 0,
            filter: String::new(),
            cursor: None,
        })
    }
}
