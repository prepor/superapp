//! The search panel: one question, put to every app's sources, as a rich
//! table.
//!
//! The line above the rows is both halves of a search at once. Its **words**
//! are the question — they go to the sources, which is where the reading of
//! them lives (mail's index reads a letter's body; nothing here could).
//! Its **tags** narrow the answer that comes back: `@app:mail` is a filter
//! over rows already found, sifted in memory like any other table's.
//!
//! So the words are never sifted twice. A row the mail index found by a
//! word deep in a letter has none of that word in the line it draws, and
//! matching the line against the query again would hide exactly the rows
//! the index worked hardest for.
//!
//! The sources answer off the UI thread: the panel asks on the keystroke
//! that changed the question and takes whatever has arrived on every event
//! and every draw. The list is always in the order the sources were
//! registered in, whoever was quick — so one question gives one list, and
//! not a different one each time a thread is scheduled differently. A
//! source answering late inserts its band where it belongs, and the rows
//! below it move down; the cursor and the marks are keys rather than
//! numbers, so both follow their own rows through it. A list that is empty
//! because nobody has answered *yet* says so, rather than saying that
//! nothing was found.

use std::any::Any;
use std::rc::Rc;

use kernel::filter::{Ast, Op};
use kernel::layout::SlotId;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::richtable::{Datasource, ListState, Suggestion, TagDef, TagType, Values};
use kernel::search::{Engine, Found, Query};
use kernel::session::{Action, Session};
use kernel::store::Store;
use makepad_widgets::*;

use crate::shell::hosted::PanelProps;
use crate::shell::widgets::table::{self, RowSpec, TableView};

/// Rows per page. The rows are in memory — the sources have already been
/// asked — so the size only bounds a draw.
const PAGE: usize = 50;

/// The filter field, in this panel's template.
const FILTER: &[LiveId] = ids!(filter_input);

/// The tags a search panel's filter accepts. One: which source found the
/// row. Everything else in the line is the question itself.
static TAGS: &[TagDef] = &[TagDef {
    name: "app",
    kind: TagType::Text,
    ops: &[Op::Eq],
    describe: "the source that found it",
    values: Values::Dynamic,
}];

// -- the datasource ------------------------------------------------------------

/// What the sources have found, as a table reads it: the merged rows in the
/// order they were merged, and the names a `@app:` completes to.
///
/// Cheap to clone and cheap to replace: a fresh answer is a new source
/// handed to the table, which is how a list whose rows never came out of
/// the store is kept current ([`Search::observe`]).
#[derive(Clone)]
pub struct HitSource {
    found: Rc<Vec<Found>>,
    sources: Rc<Vec<&'static str>>,
}

impl HitSource {
    #[must_use]
    fn new(found: Vec<Found>, sources: Vec<&'static str>) -> HitSource {
        HitSource {
            found: Rc::new(found),
            sources: Rc::new(sources),
        }
    }

    fn filtered(&self, ast: Option<&Ast>) -> Vec<Found> {
        self.found
            .iter()
            .filter(|f| ast.is_none_or(|a| matches(f, a)))
            .cloned()
            .collect()
    }
}

/// The filter grammar over one row, with the SQL builder's semantics for
/// what does not bind: a tag this source does not know is **dropped** — and
/// so is the free text, which is not a predicate here at all but the
/// question the row is already an answer to.
fn holds(f: &Found, ast: &Ast) -> Option<bool> {
    match ast {
        Ast::Text(_) | Ast::Tag(_) => None,
        Ast::Op { tag, value, .. } => match tag.as_str() {
            "app" => Some(f.source.eq_ignore_ascii_case(value.trim())),
            _ => None,
        },
        Ast::Not(inner) => holds(f, inner).map(|b| !b),
        Ast::And(v) | Ast::Or(v) => {
            let parts: Vec<bool> = v.iter().filter_map(|a| holds(f, a)).collect();
            if parts.is_empty() {
                None
            } else if matches!(ast, Ast::And(_)) {
                Some(parts.iter().all(|b| *b))
            } else {
                Some(parts.iter().any(|b| *b))
            }
        }
    }
}

/// Whether a row passes the filter; a filter nothing in which binds passes
/// everything — which is the ordinary case here, the words having been put
/// to the sources instead.
#[must_use]
fn matches(f: &Found, ast: &Ast) -> bool {
    holds(f, ast).unwrap_or(true)
}

impl Datasource for HitSource {
    type Row = Found;
    /// A row is the panel it opens, spelled out: unique in one answer,
    /// since the merge lists a panel once.
    type Key = String;

    fn tags(&self) -> &'static [TagDef] {
        TAGS
    }

    fn key(&self, row: &Found) -> String {
        row.id.to_string()
    }

    fn key_text(&self, key: &String) -> String {
        key.clone()
    }

    fn key_parse(&self, text: &str) -> Option<String> {
        (!text.is_empty()).then(|| text.to_string())
    }

    fn count(&self, _store: &Store, ast: Option<&Ast>) -> Option<usize> {
        Some(self.filtered(ast).len())
    }

    fn page(
        &self,
        _store: &Store,
        ast: Option<&Ast>,
        offset: usize,
        limit: usize,
    ) -> Rc<Vec<Found>> {
        Rc::new(
            self.filtered(ast)
                .into_iter()
                .skip(offset)
                .take(limit)
                .collect(),
        )
    }

    /// Every row the filter shows, in the table's order — what `all` marks.
    fn keys(&self, _store: &Store, ast: Option<&Ast>) -> Option<Vec<String>> {
        Some(
            self.filtered(ast)
                .iter()
                .map(|f| f.id.to_string())
                .collect(),
        )
    }

    fn present(&self, _store: &Store, ast: Option<&Ast>, keys: &[String]) -> Vec<String> {
        let shown: std::collections::BTreeSet<String> = self
            .filtered(ast)
            .iter()
            .map(|f| f.id.to_string())
            .collect();
        keys.iter()
            .filter(|k| shown.contains(*k))
            .cloned()
            .collect()
    }

    /// The row for a key whatever the tags hide — a marked row of another
    /// source is still a marked row.
    fn by_key(&self, _store: &Store, key: &String) -> Option<Found> {
        self.found
            .iter()
            .find(|f| f.id.to_string() == *key)
            .cloned()
    }

    fn index_of(&self, _store: &Store, ast: Option<&Ast>, row: &Found) -> Option<usize> {
        self.filtered(ast).iter().position(|f| f.id == row.id)
    }

    /// The sources this build has, for `@app:`.
    fn suggest(&self, _store: &Store, tag: &str, prefix: &str) -> Vec<Suggestion> {
        if tag != "app" {
            return Vec::new();
        }
        self.sources
            .iter()
            .filter(|s| s.starts_with(prefix))
            .map(|s| Suggestion::value(*s))
            .collect()
    }
}

// -- the panel -----------------------------------------------------------------

/// One search, live.
pub struct Search {
    id: PanelId,
    /// Where it landed, so a batch open puts its panels beside this one.
    slot: Option<SlotId>,
    query: Query,
    /// What was last put to the sources: the words of the filter line,
    /// without its tags.
    asked: String,
    list: ListState<HitSource>,
}

impl Search {
    pub const TAG: Tag = Tag("search");

    /// The identity of the one search panel.
    #[must_use]
    pub fn id() -> PanelId {
        PanelId::bare(Self::TAG)
    }

    /// Called at the top of every draw and the foot of every event, with
    /// the filter line as the field has it.
    ///
    /// It is the whole of the panel's liveness: the question goes out when
    /// it changes, and whatever a source has answered with since the last
    /// look comes in. Answers whether the rows moved, which is the widget's
    /// cue to redraw.
    pub fn observe(&mut self, s: &Session, filter: &str) -> bool {
        let words = words_of(filter);
        if words != self.asked {
            self.asked = words;
            self.query.ask(s.store(), &self.asked);
            // A new question is a new list: the cursor and the marks were
            // about rows that are no longer on offer.
            let src = self.source();
            self.list.retarget(src);
            return true;
        }
        if self.query.collect() {
            // The same question, further answered: the rows already on
            // screen keep their cursor and their marks.
            let src = self.source();
            self.list.table_mut().retarget(src);
            return true;
        }
        false
    }

    /// The rows as they stand, as a source the table can read.
    fn source(&self) -> HitSource {
        HitSource::new(self.query.found().to_vec(), self.query.sources().to_vec())
    }

    /// Whether a source still owes an answer to the question standing.
    #[must_use]
    fn pending(&self) -> bool {
        self.query.pending()
    }

    /// The batch verb: every marked row opened, each a fresh un-joined
    /// column beside this panel, and the set let go. One action, so one
    /// undo closes the lot.
    ///
    /// The set is let go only once the action has landed. A locked device —
    /// another one holds the lease — refuses it and says so, and what a
    /// verb could not do stays marked, ready for the press after the lease
    /// comes back.
    fn open_marked(&mut self, s: &mut Session) {
        let store = s.store().clone();
        let ids: Vec<PanelId> = self
            .list
            .marks()
            .keys()
            .iter()
            .filter_map(|k| self.list.table().by_key(&store, k))
            .map(|f| f.id)
            .collect();
        if ids.is_empty() {
            self.list.clear_marks();
            return;
        }
        let (from, n) = (self.slot, ids.len());
        let done = s
            .act(
                Action::new("open", format!("open {n} found")).moving(move |wm| {
                    for id in ids {
                        wm.open(id, from, false);
                    }
                }),
            )
            .is_some();
        if done {
            self.list.clear_marks();
        }
    }
}

/// The words of a filter line, its tags left out: what the sources are
/// asked. `budget @app:mail` puts *budget* to every source and keeps mail's
/// answer.
///
/// Read off the parsed line rather than the raw text, so one grammar reads
/// the line and a quoted value is never mistaken for a word of the
/// question.
#[must_use]
fn words_of(filter: &str) -> String {
    fn walk(ast: &Ast, out: &mut Vec<String>) {
        match ast {
            Ast::Text(t) => out.push(t.clone()),
            Ast::Tag(_) | Ast::Op { .. } => {}
            Ast::Not(inner) => walk(inner, out),
            Ast::And(v) | Ast::Or(v) => v.iter().for_each(|a| walk(a, out)),
        }
    }
    let Some(ast) = kernel::filter::parse(filter).ast else {
        return String::new();
    };
    let mut words = Vec::new();
    walk(&ast, &mut words);
    words
        .iter()
        .flat_map(|w| w.split_whitespace())
        .collect::<Vec<_>>()
        .join(" ")
}

impl Panel for Search {
    fn id(&self) -> &PanelId {
        &self.id
    }

    fn title(&self) -> String {
        "search".into()
    }

    fn wish(&self, _cols: usize) -> (u32, u32) {
        (4, 5)
    }

    fn placed(&mut self, slot: SlotId) {
        self.slot = Some(slot);
    }

    /// The batch verbs over the marked set, with their count — up with the
    /// first mark and gone with the last. A search finds; what it offers to
    /// do with a set is to open it.
    fn verbs(&self) -> Vec<Verb> {
        let n = self.list.marks().len();
        if n == 0 {
            return Vec::new();
        }
        vec![
            Verb::run("system.open_found", format!("open {n}"), Some('o')),
            Verb::run("system.unmark", "clear", Some('r')),
        ]
    }

    fn run(&mut self, verb: &str, s: &mut Session) {
        match verb {
            "system.open_found" => self.open_marked(s),
            "system.unmark" => {
                self.list.clear_marks();
                s.redraw();
            }
            _ => {}
        }
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

/// Its factory.
pub struct SearchKind;

impl PanelKind for SearchKind {
    fn tag(&self) -> Tag {
        Search::TAG
    }

    /// Every source in this build, asked from this panel.
    ///
    /// A thread each in an ordinary run, so a source that walks a disk or
    /// calls a server never holds up one that answers out of an index — and
    /// inline wherever time only moves when it is moved, since a scripted
    /// keystroke must be followed by its rows in the same tick. That is the
    /// session's own answer and not the build's: a headless run and a
    /// panels-library mount in a window both run on a virtual clock, and a
    /// mount that froze before its answer arrived would be a picture of a
    /// search that never finished. The workers read the same fact to
    /// choose between threads and inline passes.
    ///
    /// The threads are this instance's: they retire with it, when the ask
    /// channel they wait on drops.
    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        let providers = cx.session().apps().providers();
        let engine = if cx.session().workers().is_inline() {
            Engine::inline(providers)
        } else {
            Engine::threads(&cx.session().store().db(), providers, || {
                SignalToUI::set_ui_signal();
            })
        };
        // The empty list already knows the sources' names, so `@app:`
        // completes before anything has been asked.
        let sources = engine.names().to_vec();
        Box::new(Search {
            id: id.clone(),
            slot: None,
            query: Query::new(engine),
            asked: String::new(),
            list: ListState::new(HitSource::new(Vec::new(), sources), PAGE),
        })
    }
}

// -- the widget ----------------------------------------------------------------

/// What the table needs to know about a search's rows.
pub struct HitRows;

impl RowSpec for HitRows {
    type Src = HitSource;
    type Panel = Search;

    fn list(panel: &mut Search) -> &mut ListState<HitSource> {
        &mut panel.list
    }

    fn row_tpl() -> LiveId {
        live_id!(row)
    }

    /// One row: what was found, the source that found it on the right, and
    /// the source's own second line under both.
    fn populate(cx: &mut Cx, row: &WidgetRef, f: &Found, selected: bool, marked: bool) {
        let line = table::line(cx, row, selected, marked);
        line.label(cx, ids!(body.label_lbl)).set_text(cx, &f.label);
        line.label(cx, ids!(body.source_lbl)).set_text(cx, f.source);
        let detail = line.label(cx, ids!(body.detail_lbl));
        detail.set_text(cx, &f.detail);
        detail.set_visible(cx, !f.detail.is_empty());
    }

    fn label(f: &Found) -> String {
        f.label.clone()
    }

    fn target(f: &Found) -> PanelId {
        f.id.clone()
    }

    /// Why the list is empty: nothing asked, nobody answered yet, or an
    /// answer of nothing.
    fn empty_line(panel: &Search, filter: &str) -> String {
        if panel.asked.is_empty() {
            // A tag narrows an answer; on its own it is not a question.
            return if filter.trim().is_empty() {
                "type to search".to_string()
            } else {
                "a tag narrows an answer — type a word to ask for one".to_string()
            };
        }
        if panel.pending() {
            return "searching…".to_string();
        }
        format!("nothing found for “{}”", panel.asked)
    }
}

/// The widget: the shared table, and the question put to the sources around
/// it.
#[derive(Script, ScriptHook, Widget)]
pub struct SearchPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    /// The completion box, drawn over the rows after everything else.
    #[live]
    suggest: View,
    #[rust]
    table: TableView<HitRows>,
    /// Whether the field has been handed the keyboard. Once, when the panel
    /// first stands where it will stand.
    #[rust]
    greeted: bool,
}

impl Widget for SearchPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let Self {
            view,
            table,
            greeted,
            ..
        } = self;
        table.handle_event(cx, event, scope, view);
        // After the table, which is what put the keystroke in the field —
        // and on every other event too, because a source answering rings
        // the UI signal and it arrives here as an ordinary one.
        observe(cx, view, scope);
        if !*greeted && greet(cx, view, scope) {
            *greeted = true;
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Self {
            view,
            suggest,
            table,
            ..
        } = self;
        observe(cx, view, scope);
        table.draw(cx, scope, walk, view, suggest)
    }
}

/// Hands the instance the line as the field has it, and takes whatever the
/// sources have answered with. Redraws when the rows moved.
fn observe(cx: &mut Cx, view: &mut View, scope: &mut Scope) {
    let Some(props) = scope.props.get::<PanelProps>().cloned() else {
        return;
    };
    let text = view.text_input(cx, FILTER).text();
    let Some(session) = scope.data.get_mut::<Session>() else {
        return;
    };
    let moved = {
        let session: &Session = session;
        let mut borrow = props.panel.borrow_mut();
        borrow
            .as_any()
            .downcast_mut::<Search>()
            .is_some_and(|p| p.observe(session, &text))
    };
    if moved {
        session.redraw();
    }
}

/// Hands the field the keyboard, once, when this panel has the focus and
/// has been drawn where it will stand — focus on a field with no rectangle
/// lands nowhere.
///
/// A panel for asking a question opens on the question: that is the whole
/// of what it is for, and one that had to be clicked into first would be a
/// slower launcher. Only when the panel is the focused one, so a search
/// restored into some other column at boot takes nobody's keyboard.
fn greet(cx: &mut Cx, view: &mut View, scope: &mut Scope) -> bool {
    let Some(props) = scope.props.get::<PanelProps>().cloned() else {
        return false;
    };
    if scope.data.get_mut::<Session>().map(|s| s.focus()) != Some(Some(props.slot)) {
        return false;
    }
    let field = view.text_input(cx, FILTER);
    if field.area().rect(cx).size.y <= 0.0 {
        return false;
    }
    field.set_key_focus(cx);
    true
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use kernel::app::App;
    use kernel::repl::object::MemBucket;
    use kernel::session::ReplMount;

    use super::*;

    static APPS: &[&dyn App] = &[&crate::shell::system::SYSTEM];

    /// The line is read twice over: its words are the question, its tags
    /// narrow the answer.
    #[test]
    fn the_words_are_the_question_and_the_tags_are_not() {
        assert_eq!(words_of("q3 budget"), "q3 budget");
        assert_eq!(words_of("  q3   budget  "), "q3 budget");
        assert_eq!(words_of("budget @app:mail"), "budget");
        assert_eq!(words_of("@app:mail budget draft"), "budget draft");
        assert_eq!(words_of("@app:mail"), "", "a tag alone asks nothing");
        assert_eq!(words_of(""), "");
    }

    fn found(source: &'static str, label: &str, tag: &'static str) -> Found {
        Found {
            source,
            label: label.to_string(),
            detail: String::new(),
            id: PanelId::bare(Tag(tag)),
        }
    }

    fn store() -> Store {
        Store::open(None, &[]).expect("in-memory store")
    }

    /// `@app:` keeps one source's rows; the words in the line are not a
    /// second sieve over what came back.
    #[test]
    fn a_tag_narrows_the_answer_and_the_words_do_not() {
        let src = HitSource::new(
            vec![
                found("mail", "Q3 infra budget", "message"),
                found("files", "budget.md", "files"),
            ],
            vec!["mail", "files"],
        );
        let s = store();
        let rows = |line: &str| -> Vec<String> {
            let ast = kernel::filter::parse(line).ast;
            src.page(&s, ast.as_ref(), 0, 10)
                .iter()
                .map(|f| f.label.clone())
                .collect()
        };
        assert_eq!(rows("").len(), 2);
        assert_eq!(rows("@app:mail"), vec!["Q3 infra budget".to_string()]);
        assert_eq!(rows("@app:files"), vec!["budget.md".to_string()]);
        // The words the sources were asked for: still both rows, whatever
        // the line says — a row found by a letter's body has none of the
        // question in the line it draws.
        assert_eq!(rows("thermos").len(), 2, "the words are not a sieve");
        assert_eq!(rows("thermos @app:mail").len(), 1);
        // A tag this source does not know is dropped, not answered.
        assert_eq!(rows("@bogus:x").len(), 2);
    }

    /// A search panel over rows it was handed, with two of them marked.
    fn marked_two() -> (Session, kernel::session::Instance) {
        let mut s = Session::fake(APPS);
        s.act(Action::new("open", "open search").moving(|wm| {
            wm.open(Search::id(), None, false);
        }))
        .expect("the panel opened");
        s.settle();
        let slot = s.focus().expect("the search panel is focused");
        let inst = s.panel(slot).expect("its instance");
        {
            let mut borrow = inst.borrow_mut();
            let p = borrow
                .as_any()
                .downcast_mut::<Search>()
                .expect("a search panel");
            let rows = vec![
                found("mail", "Q3 infra budget", "message"),
                found("mail", "Sat hike", "contact"),
            ];
            let keys: Vec<String> = rows.iter().map(|f| f.id.to_string()).collect();
            p.list
                .table_mut()
                .retarget(HitSource::new(rows, vec!["mail"]));
            p.list.marks_mut().extend(keys);
            assert_eq!(p.list.marks().len(), 2);
        }
        (s, inst)
    }

    /// A batch verb that could not run keeps its set: a locked device — one
    /// whose lease another holds — refuses the action, and the marks are
    /// still there for the press after the lease comes back.
    #[test]
    fn a_refused_batch_open_keeps_its_marks() {
        let (mut s, inst) = marked_two();
        let panels = s.panels().len();
        s.mount_repl(ReplMount::Inline, || {});
        s.start_repl_with(Arc::new(MemBucket::new()));
        assert!(!s.writable(), "shut until the first pass answers");

        inst.borrow_mut().run("system.open_found", &mut s);
        s.settle();
        assert_eq!(s.panels().len(), panels, "nothing was opened");
        {
            let mut borrow = inst.borrow_mut();
            let p = borrow.as_any().downcast_mut::<Search>().expect("the panel");
            assert_eq!(p.list.marks().len(), 2, "and the set is still marked");
        }

        // With the lease taken, the same press opens both and lets the set
        // go — one action, so one undo closes them again.
        s.repl_poll();
        assert!(s.writable(), "the first pass made it the holder");
        inst.borrow_mut().run("system.open_found", &mut s);
        s.settle();
        assert_eq!(s.panels().len(), panels + 2, "both rows opened");
        let mut borrow = inst.borrow_mut();
        let p = borrow.as_any().downcast_mut::<Search>().expect("the panel");
        assert!(p.list.marks().is_empty(), "the set was let go");
    }

    /// The completion offers the sources this build has.
    #[test]
    fn the_app_tag_completes_to_the_sources() {
        let src = HitSource::new(Vec::new(), vec!["mail", "files"]);
        let s = store();
        let names: Vec<String> = src
            .suggest(&s, "app", "")
            .into_iter()
            .map(|x| x.value)
            .collect();
        assert_eq!(names, vec!["mail".to_string(), "files".to_string()]);
        assert_eq!(src.suggest(&s, "app", "ma").len(), 1);
        assert!(src.suggest(&s, "kind", "").is_empty(), "one tag, no more");
    }
}
