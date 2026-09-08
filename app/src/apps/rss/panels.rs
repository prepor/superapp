use super::model::{self, Flag, Flags};
use kernel::effect::World;
use kernel::history::Intent;
use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::richtable::{Datasource, ListState, SqlSource};
use kernel::session::{Instance, Session};
use kernel::store::Store;
use std::any::Any;
use std::rc::Rc;

pub static KINDS: &[&dyn PanelKind] = &[
    &FeedsKind,
    &ArticlesKind,
    &ArticleKind,
    &AddFeedKind,
    &ImportFeedsKind,
];
pub type FeedList = ListState<&'static SqlSource<model::Feed, i64>>;
pub type ArticleList = ListState<&'static SqlSource<model::Article, i64>>;

fn selected<D: Datasource<Key = i64>>(list: &ListState<D>) -> Vec<i64> {
    let marked = list.marks().keys();
    if marked.is_empty() {
        list.cursor_key().copied().into_iter().collect()
    } else {
        marked
    }
}

fn change_list<D: Datasource<Key = i64>>(
    s: &mut Session,
    slot: SlotId,
    list: &mut ListState<D>,
    kind: Flag,
    after: bool,
) {
    let marks = list.marks().keys();
    let ids = selected(list);
    list.clear_marks();
    if model::change(s, kind, &ids, after) {
        if let Some(panel) = s.panel(slot).filter(|_| !marks.is_empty()) {
            s.claim(Box::new(ConsumedMarks { panel, keys: marks }));
        }
    } else {
        list.marks_mut().extend(marks);
    }
}

struct ConsumedMarks {
    panel: Instance,
    keys: Vec<i64>,
}
impl ConsumedMarks {
    fn edit(&self, restore: bool) {
        let mut p = self.panel.borrow_mut();
        let Some(p) = p.as_any().downcast_mut::<Feeds>() else {
            return;
        };
        let marks = p.list.marks_mut();
        if restore {
            marks.extend(self.keys.iter().copied());
        } else {
            marks.clear();
        }
    }
}
impl Intent for ConsumedMarks {
    fn describe(&self) -> String {
        format!("{} marked RSS rows", self.keys.len())
    }
    fn reverse(&self, _: &World) -> Result<(), String> {
        self.edit(true);
        Ok(())
    }
    fn reapply(&self, _: &World) -> Result<(), String> {
        self.edit(false);
        Ok(())
    }
}

pub struct Feeds {
    id: PanelId,
    slot: SlotId,
    pub list: FeedList,
}
impl Feeds {
    pub const TAG: Tag = Tag("rss-feeds");
    pub fn id() -> PanelId {
        PanelId::bare(Self::TAG)
    }
}
impl Panel for Feeds {
    fn id(&self) -> &PanelId {
        &self.id
    }
    fn title(&self) -> String {
        "feeds".into()
    }
    fn about(&self) -> String {
        "RSS subscriptions, unseen counts and refresh errors. Open a feed to read its articles; add a URL or remove selected feeds. Removal is undoable.".into()
    }
    fn wish(&self, _: usize) -> (u32, u32) {
        (4, 6)
    }
    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }
    fn verbs(&self) -> Vec<Verb> {
        let mut verbs = vec![
            Verb::go(
                "rss.add_feed",
                "add feed",
                Some('a'),
                Nav::Open {
                    from: self.slot,
                    id: AddFeed::id(),
                    fresh: false,
                },
            ),
            Verb::run("rss.refresh", "refresh", Some('r')),
            Verb::go(
                "rss.import",
                "import OPML",
                Some('o'),
                Nav::Open {
                    from: self.slot,
                    id: ImportFeeds::id(),
                    fresh: false,
                },
            ),
            Verb::go(
                "rss.articles",
                "articles",
                Some('c'),
                Nav::Open {
                    from: self.slot,
                    id: Articles::id(),
                    fresh: false,
                },
            ),
        ];
        let n = selected(&self.list).len();
        if n > 0 {
            verbs.push(Verb::run("rss.remove", format!("remove {n}"), Some('d')));
        }
        verbs
    }
    fn run(&mut self, verb: &str, s: &mut Session) {
        match verb {
            "rss.refresh" => model::refresh(s),
            "rss.remove" => {
                change_list(s, self.slot, &mut self.list, Flag::Subscribed, false);
            }
            _ => {}
        }
    }
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}
pub struct FeedsKind;
impl PanelKind for FeedsKind {
    fn tag(&self) -> Tag {
        Feeds::TAG
    }
    fn open(&self, id: &PanelId, _: &mut Opening<'_>) -> Box<dyn Panel> {
        Box::new(Feeds {
            id: id.clone(),
            slot: 0,
            list: ListState::new(&model::FEEDS, 50),
        })
    }
}

pub struct Articles {
    id: PanelId,
    slot: SlotId,
    pub list: ArticleList,
    pub filter: String,
}
impl Articles {
    pub const TAG: Tag = Tag("rss");
    pub fn id() -> PanelId {
        PanelId::bare(Self::TAG)
    }
    pub fn for_feed(id: i64) -> PanelId {
        PanelId::new(Self::TAG, [format!("@unseen @feed_id:{id}")])
    }
}
impl Panel for Articles {
    fn id(&self) -> &PanelId {
        &self.id
    }
    fn title(&self) -> String {
        "rss".into()
    }
    fn about(&self) -> String {
        "Articles from subscribed RSS and Atom feeds, oldest first. The default @unseen filter is editable: clear it for the archive. Tags include @feed, @author and @date. Opening an article marks it seen; undo restores that state.".into()
    }
    fn wish(&self, _: usize) -> (u32, u32) {
        (4, 6)
    }
    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }
    fn persist(&self) -> PanelId {
        PanelId::new(Self::TAG, [self.list.table().filter().to_string()])
    }
    fn verbs(&self) -> Vec<Verb> {
        vec![
            Verb::go(
                "rss.feeds",
                "feeds",
                Some('f'),
                Nav::Open {
                    from: self.slot,
                    id: Feeds::id(),
                    fresh: false,
                },
            ),
            Verb::run("rss.refresh", "refresh", Some('r')),
        ]
    }
    fn run(&mut self, verb: &str, s: &mut Session) {
        if verb == "rss.refresh" {
            model::refresh(s);
        }
    }
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}
pub struct ArticlesKind;
impl PanelKind for ArticlesKind {
    fn tag(&self) -> Tag {
        Articles::TAG
    }
    fn open(&self, id: &PanelId, _: &mut Opening<'_>) -> Box<dyn Panel> {
        let filter = id.args.first().cloned().unwrap_or_else(|| "@unseen".into());
        let mut list = ListState::new(&model::ARTICLES, 50);
        list.set_filter(&filter);
        Box::new(Articles {
            id: id.clone(),
            slot: 0,
            list,
            filter,
        })
    }
}

pub struct Article {
    id: PanelId,
    article: i64,
    store: Rc<Store>,
    slot: SlotId,
    open_url: Option<String>,
}
impl Article {
    pub const TAG: Tag = Tag("rss-article");
    pub fn id(id: i64) -> PanelId {
        PanelId::new(Self::TAG, [id.to_string()])
    }
    pub fn reading(&self) -> Option<(model::Article, String)> {
        model::article(&self.store, self.article)
            .map(|a| (a, model::body(&self.store, self.article)))
    }
    /// The widget opens the requested publisher page once.
    pub fn take_url(&mut self) -> Option<String> {
        self.open_url.take()
    }
}
impl Panel for Article {
    fn id(&self) -> &PanelId {
        &self.id
    }
    fn title(&self) -> String {
        model::article(&self.store, self.article)
            .map(|a| a.title)
            .unwrap_or_else(|| "article".into())
    }
    fn about(&self) -> String {
        format!("RSS article {}: its cached full content or publisher summary, read with the shared HTML viewer. Opening marks it seen. The original link opens the publisher's website.",self.article)
    }
    fn wish(&self, _: usize) -> (u32, u32) {
        (5, 6)
    }
    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }
    fn verbs(&self) -> Vec<Verb> {
        let Some(a) = model::article(&self.store, self.article) else {
            return Vec::new();
        };
        let mut verbs = Vec::new();
        if !a.url.is_empty() {
            verbs.push(Verb::run("rss.original", "show original", Some('o')));
        }
        verbs.push(Verb::go(
            "rss.feed",
            "feed",
            Some('f'),
            Nav::Open {
                from: self.slot,
                id: Articles::for_feed(a.feed),
                fresh: false,
            },
        ));
        verbs
    }
    fn run(&mut self, verb: &str, s: &mut Session) {
        if verb == "rss.original" {
            self.open_url = model::article(&self.store, self.article)
                .map(|a| a.url)
                .filter(|url| !url.is_empty());
            s.redraw();
        }
    }
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}
pub struct ArticleKind;
impl PanelKind for ArticleKind {
    fn tag(&self) -> Tag {
        Article::TAG
    }
    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        let article = id.args.first().and_then(|s| s.parse().ok()).unwrap_or(0);
        let store = cx.session().store().clone();
        if model::article(&store, article).is_some() {
            let flags = Flags::of(&store, Flag::Seen, &[article], true);
            if !flags.before.is_empty() {
                cx.claim(flags.write(), vec![Box::new(flags)]);
            }
        }
        Box::new(Article {
            id: id.clone(),
            article,
            store,
            slot: 0,
            open_url: None,
        })
    }
}

pub struct AddFeed {
    id: PanelId,
    slot: SlotId,
    pub url: String,
    pub error: String,
}
impl AddFeed {
    pub const TAG: Tag = Tag("rss-add-feed");
    pub fn id() -> PanelId {
        PanelId::bare(Self::TAG)
    }
    pub fn submit(&mut self, s: &mut Session) {
        match model::add(s, &self.url) {
            Ok(id) => {
                self.error.clear();
                s.notify("feed added", false);
                s.nav_within(Nav::Open {
                    from: self.slot,
                    id: Articles::for_feed(id),
                    fresh: false,
                });
            }
            Err(why) => {
                self.error = why;
                s.redraw();
            }
        }
    }
}
impl Panel for AddFeed {
    fn id(&self) -> &PanelId {
        &self.id
    }
    fn title(&self) -> String {
        "add feed".into()
    }
    fn about(&self) -> String {
        "Subscribe to an RSS or Atom feed by its HTTP or HTTPS URL. Fetching happens in the background; the feeds list shows any error.".into()
    }
    fn wish(&self, _: usize) -> (u32, u32) {
        (4, 3)
    }
    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }
    fn verbs(&self) -> Vec<Verb> {
        vec![Verb::run("rss.subscribe", "subscribe", Some('s'))]
    }
    fn run(&mut self, verb: &str, s: &mut Session) {
        if verb == "rss.subscribe" {
            self.submit(s);
        }
    }
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}
pub struct AddFeedKind;
impl PanelKind for AddFeedKind {
    fn tag(&self) -> Tag {
        AddFeed::TAG
    }
    fn open(&self, id: &PanelId, _: &mut Opening<'_>) -> Box<dyn Panel> {
        Box::new(AddFeed {
            id: id.clone(),
            slot: 0,
            url: String::new(),
            error: String::new(),
        })
    }
}

pub struct ImportFeeds {
    id: PanelId,
    slot: SlotId,
    pub path: String,
    pub error: String,
    pub status: String,
}
impl ImportFeeds {
    pub const TAG: Tag = Tag("rss-import");
    pub fn id() -> PanelId {
        PanelId::bare(Self::TAG)
    }
    pub fn submit(&mut self, s: &mut Session) {
        let result = s
            .world()
            .run(&super::opml::Read(self.path.clone()))
            .and_then(|doc| model::import(s, doc));
        self.error.clear();
        self.status.clear();
        match result {
            Ok(imported) => self.status = imported.summary(),
            Err(why) => self.error = why,
        }
        s.redraw();
    }
}
impl Panel for ImportFeeds {
    fn id(&self) -> &PanelId {
        &self.id
    }
    fn title(&self) -> String {
        "import OPML".into()
    }
    fn about(&self) -> String {
        "Import feed subscriptions from a local OPML file. Paste its path, then import. Nested folders are flattened, duplicates are skipped, and existing read state is preserved. One undo removes the imported subscriptions.".into()
    }
    fn wish(&self, _: usize) -> (u32, u32) {
        (4, 3)
    }
    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }
    fn verbs(&self) -> Vec<Verb> {
        vec![
            Verb::run("rss.import_opml", "import", Some('o')),
            Verb::go(
                "rss.feeds",
                "feeds",
                Some('f'),
                Nav::Open {
                    from: self.slot,
                    id: Feeds::id(),
                    fresh: false,
                },
            ),
        ]
    }
    fn run(&mut self, verb: &str, s: &mut Session) {
        if verb == "rss.import_opml" {
            self.submit(s);
        }
    }
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}
pub struct ImportFeedsKind;
impl PanelKind for ImportFeedsKind {
    fn tag(&self) -> Tag {
        ImportFeeds::TAG
    }
    fn open(&self, id: &PanelId, _: &mut Opening<'_>) -> Box<dyn Panel> {
        Box::new(ImportFeeds {
            id: id.clone(),
            slot: 0,
            path: String::new(),
            error: String::new(),
            status: String::new(),
        })
    }
}
