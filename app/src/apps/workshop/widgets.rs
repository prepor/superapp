//! Native rich tables and ordinary detail panels. Every mutation delegates to
//! the same command dispatcher used by Workshop tools.

use crate::apps::terminal::TerminalViewWidgetRefExt;

use super::{
    harness, model,
    panels::{self, Comparison, Detail, DetailType, Projects, Review, WorkspaceSource, Workspaces},
    runtime::{self, Command},
};
use crate::shell::{
    dsl::LinkViewExt,
    hosted::PanelProps,
    keys::Letters,
    widgets::{
        select::{self, SelectOption, SelectWidgetExt},
        table::{self, RowSpec, TableView},
    },
};
use kernel::{
    nav::Nav,
    panel::{Panel, PanelId},
    richtable::{ListState, SqlSource},
    session::Session,
    time::fmt_date,
};
use makepad_widgets::*;
use std::{collections::HashSet, sync::Arc};

/// How long ago, in a word: what the workspaces table says about activity.
pub(super) fn fmt_ago(now: f64, at: f64) -> String {
    let ago = now - at;
    if ago < 60.0 {
        "now".into()
    } else if ago < 3600.0 {
        format!("{}m", (ago / 60.0) as i64)
    } else if ago < 86400.0 {
        format!("{}h", (ago / 3600.0) as i64)
    } else if ago < 7.0 * 86400.0 {
        format!("{}d", (ago / 86400.0) as i64)
    } else {
        fmt_date(at)
    }
}

fn fill_row(
    cx: &mut Cx,
    row: &WidgetRef,
    state: (bool, bool),
    title: &str,
    detail: &str,
    meta: &str,
    unread: bool,
) {
    let line = table::line(cx, row, state.0, state.1);
    for (path, show) in [
        (ids!(body.title_lbl), !unread),
        (ids!(body.unread_lbl), unread),
    ] {
        let label = line.label(cx, path);
        label.set_text(cx, if show { title } else { "" });
        label.set_visible(cx, show);
    }
    line.label(cx, ids!(body.detail_lbl)).set_text(cx, detail);
    line.widget(cx, ids!(body.detail_lbl))
        .set_visible(cx, !detail.is_empty());
    line.label(cx, ids!(body.meta_lbl)).set_text(cx, meta);
}
pub struct ProjectRows;
impl RowSpec for ProjectRows {
    type Src = &'static SqlSource<model::ProjectRow, i64>;
    type Panel = Projects;
    fn list(p: &mut Projects) -> &mut ListState<Self::Src> {
        &mut p.list
    }
    fn row_tpl() -> LiveId {
        live_id!(row)
    }
    fn seed_filter(p: &Projects) -> String {
        p.filter.clone()
    }
    fn populate(
        cx: &mut Cx,
        row: &WidgetRef,
        r: &model::ProjectRow,
        selected: bool,
        marked: bool,
        _: f64,
    ) {
        fill_row(
            cx,
            row,
            (selected, marked),
            &r.name,
            if r.error.is_empty() {
                &r.path
            } else {
                &r.error
            },
            if r.error.is_empty() { "" } else { "error" },
            false,
        );
    }
    fn label(r: &model::ProjectRow, _: f64) -> String {
        r.name.clone()
    }
    fn target(r: &model::ProjectRow) -> PanelId {
        Workspaces::project(r)
    }
    fn empty_line(_: &Projects, filter: &str) -> String {
        if filter.is_empty() {
            "no repositories yet"
        } else {
            "no matching repositories"
        }
        .into()
    }
}
pub struct WorkspaceRows;
impl RowSpec for WorkspaceRows {
    type Src = WorkspaceSource;
    type Panel = Workspaces;
    fn list(p: &mut Workspaces) -> &mut ListState<Self::Src> {
        &mut p.list
    }
    fn row_tpl() -> LiveId {
        live_id!(row)
    }
    fn seed_filter(p: &Workspaces) -> String {
        p.filter.clone()
    }
    fn populate(
        cx: &mut Cx,
        row: &WidgetRef,
        r: &model::WorkspaceRow,
        selected: bool,
        marked: bool,
        now: f64,
    ) {
        // The workspace by what it is about — its PR title, or its named
        // branch as words — over the city it started as and its project. A
        // workspace nothing has named yet is still just its city.
        let title = model::workspace_title(r);
        let mut detail = if title == r.label {
            r.project.clone()
        } else {
            format!("{} · {}", r.label, r.project)
        };
        if r.status == "preparing" {
            detail.push_str(" · preparing");
        } else if !r.error.is_empty() {
            detail.push_str(" · error");
        }
        fill_row(
            cx,
            row,
            (selected, marked),
            &title,
            &detail,
            &fmt_ago(now, r.activity),
            r.unread,
        );
    }
    fn label(r: &model::WorkspaceRow, _: f64) -> String {
        r.label.clone()
    }
    fn target(r: &model::WorkspaceRow) -> PanelId {
        Detail::workspace(r.id)
    }
    fn empty_line(_: &Workspaces, filter: &str) -> String {
        if filter.is_empty() {
            "no workspaces yet"
        } else {
            "no workspaces under this filter"
        }
        .into()
    }
}
pub struct ChangeRows;
impl RowSpec for ChangeRows {
    type Src = Comparison;
    type Panel = Review;
    fn list(p: &mut Review) -> &mut ListState<Self::Src> {
        &mut p.list
    }
    fn row_tpl() -> LiveId {
        live_id!(row)
    }
    fn seed_filter(p: &Review) -> String {
        p.filter.clone()
    }
    fn populate(
        cx: &mut Cx,
        row: &WidgetRef,
        r: &model::ChangeRow,
        selected: bool,
        marked: bool,
        _: f64,
    ) {
        let count = r.added + r.deleted;
        let meta = if r.reviewed {
            format!("{count} done")
        } else if r.needs_recheck {
            format!("{count} recheck")
        } else {
            count.to_string()
        };
        // A file still to review is bold, as an unread row is; the path is
        // on the diff's own first line.
        fill_row(
            cx,
            row,
            (selected, marked),
            panels::filename(&r.path),
            "",
            &meta,
            !r.reviewed,
        );
    }
    fn label(r: &model::ChangeRow, _: f64) -> String {
        r.path.clone()
    }
    fn target(r: &model::ChangeRow) -> PanelId {
        Detail::diff(r.id)
    }
    fn empty_line(_: &Review, filter: &str) -> String {
        if filter.is_empty() {
            "no changed files"
        } else {
            "no files under this filter"
        }
        .into()
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct WorkshopProjects {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[live]
    suggest: View,
    #[rust]
    table: TableView<ProjectRows>,
}
impl Widget for WorkshopProjects {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.table.handle_event(cx, event, scope, &mut self.view);
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.table
            .draw(cx, scope, walk, &mut self.view, &mut self.suggest)
    }
}
#[derive(Script, ScriptHook, Widget)]
pub struct WorkshopWorkspaces {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[live]
    suggest: View,
    #[rust]
    table: TableView<WorkspaceRows>,
}
impl Widget for WorkshopWorkspaces {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.table.handle_event(cx, event, scope, &mut self.view);
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.table
            .draw(cx, scope, walk, &mut self.view, &mut self.suggest)
    }
}
#[derive(Script, ScriptHook, Widget)]
pub struct WorkshopReview {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[live]
    suggest: View,
    #[rust]
    table: TableView<ChangeRows>,
    #[rust]
    options: Arc<[SelectOption]>,
}
impl Widget for WorkshopReview {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let control = self.view.select(cx, ids!(comparison));
        if select::handle_open(cx, event, scope, std::slice::from_ref(&control)) {
            return;
        }
        self.table.handle_event(cx, event, scope, &mut self.view);
        if let Event::Actions(actions) = event {
            if let Some(snapshot) = control.changed(actions).and_then(|v| v.parse::<i64>().ok()) {
                if let Some(props) = scope.props.get::<PanelProps>().cloned() {
                    if let Some(p) = props.panel.borrow_mut().as_any().downcast_mut::<Review>() {
                        p.select(snapshot);
                        if let Some(s) = scope.data.get_mut::<Session>() {
                            if let Some(row) = model::changes(&p.store, snapshot).first() {
                                s.nav(Nav::Preview {
                                    from: props.slot,
                                    id: Detail::diff(row.id),
                                });
                            }
                            s.redraw();
                        }
                    }
                }
            }
        }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let props = scope.props.get::<PanelProps>().cloned();
        if let Some(props) = &props {
            if let Some(p) = props.panel.borrow_mut().as_any().downcast_mut::<Review>() {
                p.observe();
                let progress = model::progress(&p.store, p.snapshot_id);
                let fresh = (progress.total - progress.reviewed - progress.recheck).max(0);
                self.view.label(cx, ids!(meter.progress_lbl)).set_text(
                    cx,
                    &format!("{} / {} lines reviewed", progress.reviewed, progress.total),
                );
                self.view.label(cx, ids!(meter.left_lbl)).set_text(
                    cx,
                    &format!("{} left", (progress.total - progress.reviewed).max(0)),
                );
                let total = progress.total.max(1) as f32;
                let bar = self.view.view(cx, ids!(meter.bar));
                bar.set_uniform(cx, live_id!(done), &[progress.reviewed as f32 / total]);
                bar.set_uniform(cx, live_id!(changed), &[progress.recheck as f32 / total]);
                let mut detail = Vec::new();
                if progress.recheck > 0 {
                    detail.push(format!("{} to recheck", progress.recheck));
                }
                if progress.recheck > 0 || progress.other > 0 {
                    detail.push(format!("{fresh} new"));
                }
                if progress.other > 0 {
                    detail.push(format!("{} other changes", progress.other));
                }
                let detail = detail.join(" · ");
                self.view
                    .label(cx, ids!(meter.meter_detail_lbl))
                    .set_text(cx, &detail);
                self.view
                    .widget(cx, ids!(meter.meter_detail_lbl))
                    .set_visible(cx, !detail.is_empty());
                let newer = p.latest().is_some_and(|id| id != p.snapshot_id);
                self.view
                    .label(cx, ids!(meter.notice_lbl))
                    .set_text(cx, if newer { "new changes available" } else { "" });
                self.view
                    .widget(cx, ids!(meter.notice_lbl))
                    .set_visible(cx, newer);
                let mut opts = Vec::new();
                if let Some(latest) = p.latest() {
                    opts.push(SelectOption::new(latest.to_string(), "current changes"));
                }
                for step in model::steps(&p.store, p.workspace_id).iter() {
                    if step.has_changes && !opts.iter().any(|o| o.value == step.diff_id.to_string())
                    {
                        opts.push(SelectOption::new(
                            step.diff_id.to_string(),
                            format!("step {} · {}", step.id, fmt_date(step.created)),
                        ));
                    }
                }
                if p.snapshot_id > 0 && !opts.iter().any(|o| o.value == p.snapshot_id.to_string()) {
                    opts.push(SelectOption::new(
                        p.snapshot_id.to_string(),
                        "displayed comparison",
                    ));
                }
                update_options(&mut self.options, opts);
                self.view.select(cx, ids!(comparison)).set_options(
                    cx,
                    "comparison",
                    self.options.clone(),
                    &p.snapshot_id.to_string(),
                    "waiting for changes",
                );
            }
        }
        let step = self
            .table
            .draw(cx, scope, walk, &mut self.view, &mut self.suggest);
        if let Some(props) = &props {
            props.hits.add(
                "comparison",
                self.view.widget(cx, ids!(comparison)).area().rect(cx),
                MouseCursor::Hand,
                props.slot,
            );
            select::draw_open(
                cx,
                scope,
                self.view.area().rect(cx),
                props,
                &[self.view.select(cx, ids!(comparison))],
            );
        }
        step
    }
}

/// One line of a detail panel's list.
#[derive(Clone)]
enum Row {
    /// A chat in the workspace hub, with the last thing said in it.
    Chat(model::ChatRow, String),
    /// The person's turn.
    User { text: String },
    /// The agent's prose, with who is speaking on the first line of a turn.
    Text { who: String, html: String },
    /// A tool call, todo list, subagent, background task, denial or error.
    Card(Card),
    /// What a turn changed.
    Step { id: i64, added: i64, deleted: i64 },
    /// One line of an activity, GitHub or settings list.
    Message {
        author: String,
        text: String,
        detail: String,
        step: Option<i64>,
    },
    Code {
        kind: CodeKind,
        old: Option<u64>,
        new: Option<u64>,
        text: String,
        path: String,
        old_path: String,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CodeKind {
    Hunk,
    Context,
    Add,
    Delete,
    Note,
}
/// A card as it is drawn: one line saying what and how far, and behind it
/// what came of it.
#[derive(Clone, Debug, Default, PartialEq)]
struct Card {
    /// A transcript item, or an app tool call waiting on the person.
    key: CardKey,
    depth: u8,
    who: String,
    kind: String,
    name: String,
    title: String,
    status: String,
    body: String,
    todo: String,
    progress: String,
    /// True while an app tool call asks for approval.
    asking: bool,
    /// Nested calls under a subagent, drawn when the card is open.
    children: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
enum CardKey {
    #[default]
    None,
    Item(i64),
    AppCall(i64),
}
impl Card {
    fn failed(&self) -> bool {
        matches!(
            self.status.as_str(),
            "failed" | "denied" | "interrupted" | "stopped" | "refused"
        )
    }
    /// Whether the card has anything behind its line.
    fn has_body(&self) -> bool {
        !self.body.is_empty() || self.children > 0
    }
    /// The word at the right of the line.
    fn state(&self) -> &str {
        match self.status.as_str() {
            "running" => "running…",
            "background" => "in background…",
            "pending" => "waiting for approval",
            "done" | "approved" => "",
            other => other,
        }
    }
    fn label(&self) -> String {
        if self.title.is_empty() {
            self.name.clone()
        } else {
            format!("{} · {}", self.name, self.title)
        }
    }
}
#[derive(Clone)]
enum RowAction {
    Verb(&'static str),
    Open(PanelId),
    Step(i64),
    Copy(String),
    Approve(i64),
    Refuse(i64),
    Toggle(CardKey),
}
/// A read receipt names the completed result actually drawn at the visible
/// tail. The writer compares this version again before clearing unread.
#[derive(Default)]
struct ChatReadTracker {
    drawn: Option<(i64, i64)>,
    queued: Option<(i64, i64)>,
}
impl ChatReadTracker {
    fn displayed(&mut self, result: Option<(i64, i64)>, focused: bool, at_tail: bool) -> bool {
        self.drawn = result.filter(|_| focused && at_tail);
        self.drawn.is_some() && self.drawn != self.queued
    }
    fn acknowledge(&mut self, focused: bool, at_tail: bool) -> Option<(i64, i64)> {
        if !focused || !at_tail {
            return None;
        }
        let drawn = self.drawn?;
        if self.queued == Some(drawn) {
            return None;
        }
        self.queued = Some(drawn);
        Some(drawn)
    }
}
/// How much of a card's output is drawn. A log is read, not audited.
const OUTPUT_MAX: usize = 2000;
/// The tables a chat transcript is built from; a draw rebuilds its rows only
/// when one of them has moved.
const TRANSCRIPT_TABLES: &[&str] = &[
    "workshop_message",
    "workshop_item",
    "workshop_tool_call",
    "workshop_step",
    "workshop_change",
    "workshop_run",
];

#[derive(Script, ScriptHook, Widget)]
pub struct WorkshopDetail {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    /// Measured, never drawn: one mono advance, for the code column's width.
    #[live]
    draw_mono: DrawText,
    #[rust]
    adv: f64,
    #[rust]
    hits: Vec<(Rect, RowAction)>,
    #[rust]
    provider_options: Arc<[SelectOption]>,
    #[rust]
    model_options: Arc<[SelectOption]>,
    #[rust]
    merge_options: Arc<[SelectOption]>,
    #[rust]
    mode_options: Arc<[SelectOption]>,
    #[rust]
    shown_panel: Option<PanelId>,
    #[rust]
    read_tracker: ChatReadTracker,
    #[rust]
    tailing: bool,
    #[rust]
    last_rows: usize,
    #[rust]
    shown_draft: String,
    #[rust]
    primed: bool,
    #[rust]
    patch: String,
    #[rust]
    shown_change: Option<i64>,
    #[rust]
    code: Vec<Row>,
    /// The longest code line, in characters, of the shown diff.
    #[rust]
    code_cols: usize,
    /// The cards whose output is open.
    #[rust]
    open_cards: HashSet<CardKey>,
    #[rust]
    cards_gen: u64,
    /// The transcript as last built, and what it was built from.
    #[rust]
    rows_cache: Vec<Row>,
    #[rust]
    rows_rev: Vec<u64>,
    #[rust]
    rows_gen: u64,
}

const BUTTONS: &[(&str, &[LiveId], &str)] = &[
    ("push", ids!(hub.push_line.push_btn), "workshop.push"),
    ("create PR", ids!(hub.base_line.pr_btn), "workshop.create_pr"),
    (
        "open terminal panel",
        ids!(terminal_btn),
        "workshop.terminal",
    ),
    ("send", ids!(send_btn), "workshop.send"),
    ("set model", ids!(apply_model_btn), "workshop.apply_model"),
    ("stop", ids!(stop_btn), "workshop.stop"),
];
impl WorkshopDetail {
    fn controls(&self, cx: &Cx) -> [select::SelectRef; 4] {
        [
            self.view.select(cx, ids!(provider_btn)),
            self.view.select(cx, ids!(model_btn)),
            self.view.select(cx, ids!(merge_btn)),
            self.view.select(cx, ids!(mode_btn)),
        ]
    }
    fn run(&mut self, scope: &mut Scope, props: &PanelProps, verb: &str) {
        if let Some(s) = scope.data.get_mut::<Session>() {
            if let Some(p) = props.panel.borrow_mut().as_any().downcast_mut::<Detail>() {
                p.run(verb, s);
            }
        }
    }
    /// One character's advance in the mono face, measured once.
    fn measure(&mut self, cx: &mut Cx2d) {
        if self.adv > 0.0 {
            return;
        }
        self.draw_mono.text_style.font_size = 10.5;
        if let Some(run) = self
            .draw_mono
            .prepare_single_line_run(cx, "MMMMMMMMMMMMMMMM")
        {
            self.adv = f64::from(run.width_in_lpxs) / 16.0;
        }
    }
    fn row_action(
        &mut self,
        cx: &mut Cx,
        scope: &mut Scope,
        props: &PanelProps,
        action: RowAction,
        fresh: bool,
    ) {
        if let RowAction::Verb(verb) = action {
            self.run(scope, props, verb);
            return;
        }
        if let RowAction::Copy(reference) = action {
            cx.copy_to_clipboard(&reference);
            return;
        }
        if let RowAction::Toggle(key) = action {
            if !self.open_cards.remove(&key) {
                self.open_cards.insert(key);
            }
            self.cards_gen += 1;
            self.view.redraw(cx);
            return;
        }
        let Some(s) = scope.data.get_mut::<Session>() else {
            return;
        };
        let target = match action {
            RowAction::Open(id) => Some(id),
            RowAction::Step(id) => model::step(s.store(), id)
                .filter(|step| step.has_changes)
                .map(|step| {
                    let files = model::changes(s.store(), step.diff_id);
                    if files.len() == 1 {
                        Detail::diff(files[0].id)
                    } else {
                        Review::id(step.workspace_id, Some(step.diff_id))
                    }
                }),
            RowAction::Copy(_) | RowAction::Verb(_) | RowAction::Toggle(_) => None,
            RowAction::Approve(call_id) => {
                runtime::dispatch(s, props.slot, Command::ApproveTool { call_id });
                None
            }
            RowAction::Refuse(call_id) => {
                runtime::dispatch(s, props.slot, Command::RefuseTool { call_id });
                None
            }
        };
        if let Some(id) = target {
            s.nav(Nav::Open {
                from: props.slot,
                id,
                fresh,
            });
        }
    }
    fn rows(&mut self, p: &Detail) -> Vec<Row> {
        match p.kind {
            DetailType::Workspace | DetailType::ClosedChats => {
                let previews = model::chat_previews(&p.store, p.subject);
                let chats = if p.kind == DetailType::Workspace {
                    model::chats(&p.store, p.subject)
                } else {
                    model::closed_chats(&p.store, p.subject)
                };
                chats
                    .iter()
                    .map(|c| {
                        let preview = previews
                            .iter()
                            .find(|(id, _, _)| *id == c.id)
                            .map(|(_, role, body)| preview_line(role, body))
                            .unwrap_or_default();
                        Row::Chat(c.clone(), preview)
                    })
                    .collect()
            }
            DetailType::Chat => {
                let rev = p.store.revision(TRANSCRIPT_TABLES);
                if rev != self.rows_rev || self.rows_gen != self.cards_gen {
                    self.rows_cache = chat_rows(p, &self.open_cards);
                    self.rows_rev = rev;
                    self.rows_gen = self.cards_gen;
                }
                self.rows_cache.clone()
            }
            DetailType::Diff => {
                if let Some(change) = model::change(&p.store, p.subject) {
                    if self.shown_change != Some(change.id) || self.patch != change.patch {
                        self.code = patch_rows(
                            &displayed_change_patch(&p.store, &change),
                            &change.path,
                            change.old_path.as_deref().unwrap_or(&change.path),
                        );
                        self.code_cols = self
                            .code
                            .iter()
                            .map(|row| match row {
                                Row::Code { text, .. } => text.chars().count(),
                                _ => 0,
                            })
                            .max()
                            .unwrap_or(0);
                        self.patch = change.patch;
                        self.shown_change = Some(change.id);
                    }
                    self.code.clone()
                } else {
                    vec![]
                }
            }
            DetailType::Activity => model::steps(&p.store, p.subject)
                .iter()
                .map(|step| {
                    let progress = model::progress(&p.store, step.diff_id);
                    Row::Message {
                        author: format!(
                            "chat {}",
                            model::chat(&p.store, step.chat_id).map_or(0, |c| c.ordinal)
                        ),
                        text: format!("{} changed lines · {}", progress.total, step.status),
                        detail: fmt_date(step.created),
                        step: step.has_changes.then_some(step.id),
                    }
                })
                .collect(),
            DetailType::Github => github_rows(p),
            DetailType::Settings => ["codex", "claude"]
                .iter()
                .map(|provider| Row::Message {
                    author: panels::provider_label(provider).into(),
                    text: model::provider_status(&p.store, provider),
                    detail: String::new(),
                    step: None,
                })
                .collect(),
            _ => vec![],
        }
    }
    fn compose(&mut self, cx: &mut Cx, p: &mut Detail, slot: kernel::layout::SlotId) {
        if self.shown_panel.as_ref() != Some(p.id()) {
            self.primed = false;
            self.read_tracker = ChatReadTracker::default();
            self.shown_draft.clear();
            self.patch.clear();
            self.shown_change = None;
            self.code.clear();
            self.code_cols = 0;
            self.last_rows = 0;
            self.open_cards.clear();
            self.rows_rev.clear();
            self.rows_cache.clear();
            self.shown_panel = Some(p.id().clone());
        }
        self.tailing = p.kind == DetailType::Chat;
        let workspace = model::workspace(&p.store, p.workspace_id());
        let chat = (p.kind == DetailType::Chat)
            .then(|| model::chat(&p.store, p.subject))
            .flatten();
        let archived = workspace.as_ref().is_some_and(|w| w.archived);
        let writable_chat = chat.as_ref().is_some_and(|c| !c.closed) && !archived;
        let detail = (p.kind == DetailType::Diff)
            .then(|| model::change(&p.store, p.subject))
            .flatten();
        for (id, visible) in [
            (ids!(hub), p.kind == DetailType::Workspace),
            (ids!(providers), writable_chat),
            (
                ids!(composer),
                writable_chat || (p.kind == DetailType::Comment && !archived),
            ),
            (ids!(terminal), p.kind == DetailType::Workspace && !archived),
            (
                ids!(form),
                matches!(p.kind, DetailType::AddProject | DetailType::Settings)
                    || (p.custom_model && writable_chat),
            ),
            (ids!(diff), p.kind == DetailType::Diff),
            (ids!(list), p.kind != DetailType::Diff),
        ] {
            self.view.widget(cx, id).set_visible(cx, visible);
        }
        let (meta, mut status, error) = match p.kind {
            DetailType::Workspace => workspace
                .as_ref()
                .map(|w| (w.branch.clone(), String::new(), w.error.clone()))
                .unwrap_or_else(|| (String::new(), "workspace unavailable".into(), String::new())),
            DetailType::Chat => chat
                .as_ref()
                .map(|c| {
                    (
                        String::new(),
                        if archived {
                            "workspace archived".into()
                        } else if c.closed {
                            "closed".into()
                        } else {
                            String::new()
                        },
                        c.error.clone(),
                    )
                })
                .unwrap_or_else(|| (String::new(), "chat unavailable".into(), String::new())),
            DetailType::ClosedChats => (String::new(), String::new(), String::new()),
            DetailType::Diff => detail
                .as_ref()
                .map(|c| {
                    let base = workspace
                        .as_ref()
                        .map(|w| w.base_ref.clone())
                        .unwrap_or_else(|| "base".into());
                    let current = workspace
                        .as_ref()
                        .is_some_and(|w| w.snapshot_id == Some(c.snapshot_id));
                    (
                        c.path.clone(),
                        format!(
                            "{base} → {} · {} changed lines · {}",
                            if current { "working tree" } else { "step snapshot" },
                            c.added + c.deleted,
                            if c.reviewed {
                                "reviewed"
                            } else if c.needs_recheck {
                                "needs recheck"
                            } else {
                                "unreviewed"
                            },
                        ),
                        String::new(),
                    )
                })
                .unwrap_or_else(|| {
                    (
                        String::new(),
                        "comparison unavailable".into(),
                        String::new(),
                    )
                }),
            DetailType::Activity => (
                String::new(),
                "Changes captured at agent turn boundaries.".into(),
                String::new(),
            ),
            DetailType::Github => workspace
                .as_ref()
                .map(|w| (w.branch.clone(), workspace_status(w), w.error.clone()))
                .unwrap_or_default(),
            DetailType::Comment => (
                p.path
                    .clone()
                    .unwrap_or_else(|| "pull request comment".into()),
                if workspace.as_ref().is_some_and(has_pr) {
                    "Posted comments go directly to GitHub."
                } else {
                    "Create a draft PR first. This unsent comment is saved."
                }
                .into(),
                workspace
                    .as_ref()
                    .map(|w| w.error.clone())
                    .unwrap_or_default(),
            ),
            DetailType::AddProject => (
                String::new(),
                "Choose a local Git repository.".into(),
                String::new(),
            ),
            DetailType::Settings => (String::new(), "Default for new chats".into(), String::new()),
        };
        if p.kind == DetailType::Comment {
            if let Some((id, state, body)) = p.comment_operation() {
                if state == "done"
                    && p.submitted_after.is_some_and(|previous| id > previous)
                    && p.draft == body
                {
                    p.draft.clear();
                    p.submitted_after = None;
                    status = "posted to GitHub".into();
                } else if matches!(state.as_str(), "pending" | "running") {
                    status = "posting to GitHub…".into();
                }
            }
        }
        for (id, text) in [
            (ids!(meta_lbl), meta.as_str()),
            (ids!(status_lbl), status.as_str()),
            (ids!(error_lbl), error.as_str()),
        ] {
            self.view.label(cx, id).set_text(cx, text);
            self.view.widget(cx, id).set_visible(cx, !text.is_empty());
        }
        if p.kind == DetailType::Workspace {
            if let Some(w) = workspace.as_ref() {
                self.compose_hub(cx, p, w, slot);
            }
        }
        let merge = p.kind == DetailType::Github && p.pr().is_some() && !archived;
        self.view
            .widget(cx, ids!(merge_picker))
            .set_visible(cx, merge);
        if merge {
            update_options(
                &mut self.merge_options,
                vec![
                    SelectOption::new("squash", "squash"),
                    SelectOption::new("merge", "merge commit"),
                    SelectOption::new("rebase", "rebase"),
                ],
            );
            self.view.select(cx, ids!(merge_btn)).set_options(
                cx,
                "merge method",
                self.merge_options.clone(),
                if p.field.is_empty() {
                    "squash"
                } else {
                    &p.field
                },
                "squash",
            );
        }
        let running = chat.as_ref().is_some_and(|c| {
            matches!(
                c.status.as_str(),
                "running" | "queued" | "pending" | "waiting"
            )
        });
        self.view
            .widget(cx, ids!(stop_btn))
            .set_visible(cx, running);
        self.view
            .widget(cx, ids!(send_btn))
            .set_visible(cx, writable_chat);
        self.view
            .widget(cx, ids!(mode_btn))
            .set_visible(cx, writable_chat);
        if let Some(chat) = &chat {
            update_options(
                &mut self.provider_options,
                vec![
                    SelectOption::new("codex", "Codex"),
                    SelectOption::new("claude", "Claude Code"),
                ],
            );
            let mut models = model::models(&p.store, &chat.provider)
                .iter()
                .map(|m| SelectOption::new(m, m))
                .collect::<Vec<_>>();
            if !models.iter().any(|m| m.value == chat.model) {
                models.push(SelectOption::new(&chat.model, &chat.model));
            }
            models.push(SelectOption::new("__custom__", "custom model…"));
            update_options(&mut self.model_options, models);
            update_options(
                &mut self.mode_options,
                vec![
                    SelectOption::new("work", "work"),
                    SelectOption::new("plan", "plan"),
                ],
            );
            self.view.select(cx, ids!(provider_btn)).set_options(
                cx,
                "provider",
                self.provider_options.clone(),
                &chat.provider,
                "provider",
            );
            self.view.select(cx, ids!(model_btn)).set_options(
                cx,
                "model",
                self.model_options.clone(),
                &chat.model,
                "default",
            );
            self.view.select(cx, ids!(mode_btn)).set_options(
                cx,
                "mode",
                self.mode_options.clone(),
                &p.mode,
                "work",
            );
            self.view
                .label(cx, ids!(providers.chat_status_lbl))
                .set_text(cx, chat_state(chat));
            let messages = model::messages(&p.store, p.subject);
            let started = messages
                .iter()
                .any(|m| matches!(m.role.as_str(), "user" | "You"));
            p.observe_chat_submission();
            self.view
                .widget(cx, ids!(provider_hint))
                .set_visible(cx, started && writable_chat);
            for control in self.controls(cx).iter().take(2) {
                control.set_disabled(cx, running);
            }
        } else {
            self.view
                .widget(cx, ids!(provider_hint))
                .set_visible(cx, false);
        }
        if self.shown_draft != p.draft || !self.primed {
            self.view
                .text_input(cx, ids!(ask_input))
                .set_text(cx, &p.draft);
            self.shown_draft = p.draft.clone();
        }
        if !self.primed {
            self.view
                .text_input(cx, ids!(field_input))
                .set_text(cx, &p.field);
            self.view
                .text_input(cx, ids!(second_input))
                .set_text(cx, &p.second);
            self.primed = true;
        }
        self.view.label(cx, ids!(field_label)).set_text(
            cx,
            if p.custom_model {
                "MODEL ID"
            } else if p.kind == DetailType::Settings {
                "PROVIDER · codex / claude"
            } else {
                "REPOSITORY PATH"
            },
        );
        self.view
            .widget(cx, ids!(apply_model_btn))
            .set_visible(cx, p.custom_model);
        self.view
            .label(cx, ids!(second_label))
            .set_text(cx, "MODEL");
        for id in [ids!(second_label), ids!(second_field)] {
            self.view
                .widget(cx, id)
                .set_visible(cx, p.kind == DetailType::Settings);
        }
    }
    /// The hub's Git lines: base, pull request or the button that asks for
    /// one, its state, and what is not pushed yet.
    fn compose_hub(
        &mut self,
        cx: &mut Cx,
        p: &Detail,
        w: &model::WorkspaceRow,
        slot: kernel::layout::SlotId,
    ) {
        self.view
            .label(cx, ids!(hub.base_line.base_lbl))
            .set_text(cx, &format!("← {}", w.base_ref));
        let pr = pr_value(w);
        let number = pr.as_ref().and_then(|pr| pr["number"].as_u64());
        let link = self.view.link(cx, ids!(hub.base_line.pr_link));
        link.set_visible(cx, number.is_some());
        if let Some(n) = number {
            link.set(
                cx,
                &format!("#{n}"),
                Nav::Open {
                    from: slot,
                    id: Detail::github(w.id),
                    fresh: false,
                },
                false,
                None,
            );
        }
        self.view
            .widget(cx, ids!(hub.base_line.pr_btn))
            .set_visible(cx, number.is_none() && !w.archived);
        let (state, bad) = pr_state(w, pr.as_ref());
        for (id, show) in [
            (ids!(hub.base_line.pr_state_lbl), !bad),
            (ids!(hub.base_line.pr_state_err), bad),
        ] {
            self.view
                .label(cx, id)
                .set_text(cx, if show { &state } else { "" });
            self.view
                .widget(cx, id)
                .set_visible(cx, show && !state.is_empty());
        }
        let unpushed = pr
            .as_ref()
            .and_then(|pr| pr["head"].as_str().map(str::to_owned))
            .zip(model::latest_snapshot(&p.store, w.id))
            .is_some_and(|(head, snapshot)| head != snapshot.head);
        self.view
            .widget(cx, ids!(hub.push_line))
            .set_visible(cx, unpushed && !w.archived);
    }
    fn fill_card(&self, cx: &mut Cx, row: &WidgetRef, card: &Card) {
        let open = self.open_cards.contains(&card.key) || card.failed() && !card.body.is_empty();
        row.label(cx, ids!(line.fold_lbl))
            .set_visible(cx, card.has_body() && !open);
        row.label(cx, ids!(line.name_lbl)).set_text(cx, &card.name);
        row.label(cx, ids!(line.title_lbl)).set_text(cx, &card.title);
        let state = card.state();
        let bad = card.failed();
        row.label(cx, ids!(line.state_lbl))
            .set_text(cx, if bad { "" } else { state });
        row.widget(cx, ids!(line.state_lbl))
            .set_visible(cx, !bad && !state.is_empty());
        row.label(cx, ids!(line.state_err))
            .set_text(cx, if bad { state } else { "" });
        row.widget(cx, ids!(line.state_err)).set_visible(cx, bad);
        let todo = row.text_input(cx, ids!(todo_wrap.todo_txt));
        if todo.text() != card.todo {
            todo.set_text(cx, &card.todo);
        }
        row.widget(cx, ids!(todo_wrap))
            .set_visible(cx, !card.todo.is_empty());
        row.label(cx, ids!(progress_lbl))
            .set_text(cx, &card.progress);
        row.widget(cx, ids!(progress_lbl))
            .set_visible(cx, !card.progress.is_empty());
        let (out, err) = if !open || card.body.is_empty() {
            (String::new(), String::new())
        } else if bad {
            (String::new(), harness::clip(&card.body, OUTPUT_MAX))
        } else {
            (harness::clip(&card.body, OUTPUT_MAX), String::new())
        };
        for (wrap, id, text) in [
            (ids!(out), ids!(out.body_txt), &out),
            (ids!(err), ids!(err.err_txt), &err),
        ] {
            let field = row.text_input(cx, id);
            if field.text() != *text {
                field.set_text(cx, text);
            }
            row.widget(cx, wrap).set_visible(cx, !text.is_empty());
        }
        row.widget(cx, ids!(approval)).set_visible(cx, card.asking);
    }
}
impl Widget for WorkshopDetail {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return;
        };
        if select::handle_open(cx, event, scope, &self.controls(cx)) {
            return;
        }
        let field = self.view.text_input(cx, ids!(ask_input));
        if let Event::KeyDown(key) = event {
            if props.has_keyboard
                && field.key_focus(cx)
                && key.key_code == KeyCode::ReturnKey
                && !key.modifiers.shift
            {
                let is_chat = props
                    .panel
                    .borrow_mut()
                    .as_any()
                    .downcast_mut::<Detail>()
                    .is_some_and(|p| p.kind == DetailType::Chat);
                if is_chat {
                    self.run(scope, &props, "workshop.send");
                    return;
                }
            }
        }
        if let Event::MouseDown(mouse) = event {
            if mouse.button == MouseButton::PRIMARY {
                if let Some((_, action)) = self
                    .hits
                    .iter()
                    .rev()
                    .find(|(rect, _)| rect.contains(mouse.abs))
                    .cloned()
                {
                    self.row_action(cx, scope, &props, action, mouse.modifiers.logo);
                    return;
                }
            }
        }
        self.view.handle_event(cx, event, scope);
        if let Event::Actions(actions) = event {
            if self.view.button(cx, ids!(github_open_btn)).clicked(actions) {
                let url = props
                    .panel
                    .borrow_mut()
                    .as_any()
                    .downcast_mut::<Detail>()
                    .and_then(|p| p.pr())
                    .and_then(|v| v["url"].as_str().map(str::to_owned));
                if let Some(url) = url.filter(|u| u.starts_with("https://")) {
                    cx.open_url(&url, OpenUrlInPlace::No);
                }
            }
            for (_, id, verb) in BUTTONS {
                if self.view.button(cx, id).clicked(actions) {
                    self.run(scope, &props, verb);
                }
            }
            if let Some(s) = scope.data.get_mut::<Session>() {
                if let Some(p) = props.panel.borrow_mut().as_any().downcast_mut::<Detail>() {
                    if let Some(text) = field.changed(actions) {
                        self.shown_draft = text.clone();
                        p.edited(s, text);
                    }
                    if let Some(text) = self.view.text_input(cx, ids!(field_input)).changed(actions)
                    {
                        p.field = text;
                    }
                    if let Some(text) = self
                        .view
                        .text_input(cx, ids!(second_input))
                        .changed(actions)
                    {
                        p.second = text;
                    }
                    let controls = self.controls(cx);
                    if let Some(method) = controls[2].changed(actions) {
                        p.field = method;
                    }
                    if let Some(mode) = controls[3].changed(actions) {
                        if matches!(mode.as_str(), "work" | "plan") {
                            p.mode = mode;
                            s.redraw();
                        }
                    }
                    if let Some(provider) = controls[0].changed(actions) {
                        p.command(
                            s,
                            Command::SetProvider {
                                chat_id: p.subject,
                                provider,
                            },
                        );
                    }
                    if let Some(model) = controls[1].changed(actions) {
                        if model == "__custom__" {
                            p.custom_model = true;
                            p.field = model::chat(&p.store, p.subject)
                                .map(|c| c.model)
                                .unwrap_or_else(|| "default".into());
                            self.view
                                .text_input(cx, ids!(field_input))
                                .set_text(cx, &p.field);
                            self.view.redraw(cx);
                        } else {
                            p.custom_model = false;
                            p.command(
                                s,
                                Command::SetModel {
                                    chat_id: p.subject,
                                    model,
                                },
                            );
                        }
                    }
                }
            }
        }
        if props.has_keyboard {
            if let Some(s) = scope.data.get_mut::<Session>() {
                if let Some(p) = props.panel.borrow_mut().as_any().downcast_mut::<Detail>() {
                    if p.kind == DetailType::Chat && !p.mounted {
                        p.mounted = true;
                        p.command(
                            s,
                            Command::TouchChat {
                                chat_id: p.subject,
                                viewed_version: None,
                            },
                        );
                        if model::chat(&p.store, p.subject).is_some_and(|c| !c.closed)
                            && model::workspace(&p.store, p.workspace_id())
                                .is_some_and(|w| !w.archived)
                        {
                            field.set_key_focus(cx);
                        }
                    }
                }
            }
        }
        if let Some((chat_id, version)) = self.read_tracker.acknowledge(
            props.has_keyboard,
            self.view.portal_list(cx, ids!(list)).is_at_end(),
        ) {
            if let Some(s) = scope.data.get_mut::<Session>() {
                runtime::dispatch(
                    s,
                    props.slot,
                    Command::TouchChat {
                        chat_id,
                        viewed_version: Some(version),
                    },
                );
            }
        }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        self.measure(cx);
        let terminal_world = scope.data.get_mut::<Session>().map(|s| s.world().clone());
        let (rows, drawn_result, kind) = {
            let mut borrow = props.panel.borrow_mut();
            let Some(p) = borrow.as_any().downcast_mut::<Detail>() else {
                return DrawStep::done();
            };
            self.compose(cx, p, props.slot);
            if p.kind == DetailType::Workspace
                && model::workspace(&p.store, p.subject).is_some_and(|w| !w.archived)
            {
                if let Some(world) = terminal_world {
                    let workspace_id = p.workspace_id();
                    match super::terminal::embedded(&world, workspace_id) {
                        Ok(handle) => {
                            let view = self.view.widget(cx, ids!(terminal_host)).as_terminal_view();
                            if let Some(mut terminal) = view.borrow_mut() {
                                terminal.bind_session(handle);
                            };
                        }
                        Err(error) => {
                            self.view.label(cx, ids!(error_lbl)).set_text(cx, &error);
                            self.view.widget(cx, ids!(error_lbl)).set_visible(cx, true);
                        }
                    }
                }
            }
            let result = if p.kind == DetailType::Chat {
                model::chat(&p.store, p.subject)
                    .filter(|c| c.unread && !c.closed)
                    .map(|c| (c.id, c.unread_version))
            } else {
                None
            };
            (self.rows(p), result, p.kind)
        };
        if kind == DetailType::Diff {
            // The code column is as wide as its longest line, and never
            // narrower than the panel: one horizontal scroll for the file.
            let viewport = self
                .view
                .widget(cx, ids!(diff.diff_scroll))
                .area()
                .rect(cx)
                .size
                .x;
            #[allow(clippy::cast_precision_loss)]
            let want = self.adv.mul_add(self.code_cols as f64, 40.0 + 40.0 + 19.0 + 12.0);
            if let Some(mut wrap) = self
                .view
                .widget(cx, ids!(diff.diff_scroll.code_wrap))
                .borrow_mut::<View>()
            {
                // Before the first draw the viewport is unmeasured; a frame
                // later the wider of the two wins.
                wrap.walk.width = Size::Fixed(want.max(viewport));
            }
        }
        self.hits.clear();
        let mut rendered = Vec::new();
        // Exactly one of the two lists is visible for a given panel kind — the
        // code list for a diff, the transcript/table list otherwise — so the
        // step that arrives is always the right one; its identity need not be
        // matched (a nested list yields no uid of its own).
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            let list_ref = item.as_portal_list();
            let Some(mut list) = list_ref.borrow_mut() else {
                continue;
            };
            let follow = self.tailing && (self.last_rows == 0 || list.is_at_end());
            list.set_item_range(cx, 0, rows.len());
            if follow {
                list.set_tail_range(true);
            } else if !self.tailing {
                list.set_tail_range(false);
            }
            while let Some(index) = list.next_visible_item(cx) {
                let Some(value) = rows.get(index) else {
                    continue;
                };
                let tpl = match value {
                    Row::Chat(..) => live_id!(row),
                    Row::User { .. } => live_id!(user),
                    Row::Text { .. } => live_id!(agent),
                    Row::Card(card) if card.depth > 0 => live_id!(child),
                    Row::Card(_) => live_id!(card),
                    Row::Step { .. } => live_id!(step),
                    Row::Message { .. } => live_id!(message),
                    Row::Code { kind, .. } => match kind {
                        CodeKind::Hunk => live_id!(hunk),
                        CodeKind::Context => live_id!(ctx),
                        CodeKind::Add => live_id!(add),
                        CodeKind::Delete => live_id!(del),
                        CodeKind::Note => live_id!(note),
                    },
                };
                let row = list.item(cx, index, tpl);
                match value {
                    Row::Chat(c, preview) => fill_row(
                        cx,
                        &row,
                        (false, false),
                        &format!("chat {}", c.ordinal),
                        if !c.closed
                            && matches!(c.status.as_str(), "running" | "pending" | "waiting")
                        {
                            chat_state(c)
                        } else if preview.is_empty() {
                            "no messages yet"
                        } else {
                            preview
                        },
                        if c.closed {
                            "closed"
                        } else {
                            short_provider(&c.provider)
                        },
                        c.unread && !c.closed,
                    ),
                    Row::User { text } => {
                        let field = row.text_input(cx, ids!(wash.user_txt));
                        if field.text() != *text {
                            field.set_text(cx, text);
                        }
                    }
                    Row::Text { who, html } => {
                        row.label(cx, ids!(who_lbl)).set_text(cx, who);
                        row.widget(cx, ids!(who_lbl))
                            .set_visible(cx, !who.is_empty());
                        row.widget(cx, ids!(answer)).set_text(cx, html);
                    }
                    Row::Card(card) => self.fill_card(cx, &row, card),
                    Row::Step { added, deleted, .. } => {
                        row.label(cx, ids!(count_lbl))
                            .set_text(cx, &format!("+{added} −{deleted}"));
                    }
                    Row::Message {
                        author,
                        text,
                        detail,
                        step,
                    } => {
                        row.label(cx, ids!(author_lbl)).set_text(cx, author);
                        let field = row.text_input(cx, ids!(text_lbl));
                        if field.text() != *text {
                            field.set_text(cx, text);
                        }
                        row.label(cx, ids!(detail_lbl)).set_text(cx, detail);
                        row.widget(cx, ids!(detail_lbl))
                            .set_visible(cx, !detail.is_empty());
                        row.widget(cx, ids!(open_btn))
                            .set_visible(cx, step.is_some());
                    }
                    Row::Code {
                        kind,
                        old,
                        new,
                        text,
                        ..
                    } => match kind {
                        CodeKind::Hunk => row.label(cx, ids!(hunk_lbl)).set_text(cx, text),
                        CodeKind::Note => row.label(cx, ids!(note_lbl)).set_text(cx, text),
                        _ => {
                            row.label(cx, ids!(old_box.old_line))
                                .set_text(cx, &old.map(|n| n.to_string()).unwrap_or_default());
                            row.label(cx, ids!(new_box.new_line))
                                .set_text(cx, &new.map(|n| n.to_string()).unwrap_or_default());
                            row.label(cx, ids!(sign_box.sign_lbl)).set_text(
                                cx,
                                match kind {
                                    CodeKind::Add => "+",
                                    CodeKind::Delete => "−",
                                    _ => "",
                                },
                            );
                            let field = row.text_input(cx, ids!(code_lbl));
                            if field.text() != *text {
                                field.set_text(cx, text);
                            }
                        }
                    },
                }
                row.draw_all(cx, scope);
                rendered.push((row, value.clone()));
            }
        }
        let terminal = self.view.widget(cx, ids!(terminal_host)).as_terminal_view();
        if self.view.widget(cx, ids!(terminal)).visible() {
            if let Some(terminal) = terminal.borrow() {
                terminal.register_hits(cx, &props);
            }
        }
        self.last_rows = rows.len();
        let clip = if kind == DetailType::Diff {
            self.view.widget(cx, ids!(diff)).area().rect(cx)
        } else {
            self.view.widget(cx, ids!(list)).area().rect(cx)
        };
        for (row, value) in rendered {
            match value {
                Row::Chat(c, _) => {
                    let label = format!(
                        "chat {}: {}",
                        c.ordinal,
                        panels::provider_label(&c.provider)
                    );
                    if let Some(rect) = props.hits.add_clipped(
                        label,
                        row.area().rect(cx),
                        clip,
                        MouseCursor::Hand,
                        props.slot,
                    ) {
                        self.hits.push((rect, RowAction::Open(Detail::chat(c.id))));
                    }
                }
                Row::Message { step: Some(id), .. } => {
                    if let Some(rect) = props.hits.add_clipped(
                        "view changes",
                        row.widget(cx, ids!(open_btn)).area().rect(cx),
                        clip,
                        MouseCursor::Hand,
                        props.slot,
                    ) {
                        self.hits.push((rect, RowAction::Step(id)));
                    }
                }
                Row::Step { id, .. } => {
                    if let Some(rect) = props.hits.add_clipped(
                        "view changes",
                        row.widget(cx, ids!(link)).area().rect(cx),
                        clip,
                        MouseCursor::Hand,
                        props.slot,
                    ) {
                        self.hits.push((rect, RowAction::Step(id)));
                    }
                }
                Row::Card(card) => {
                    if card.asking {
                        if let CardKey::AppCall(call_id) = card.key {
                            for (label, id, action) in [
                                ("approve", ids!(approval.approve_btn), RowAction::Approve(call_id)),
                                ("refuse", ids!(approval.refuse_btn), RowAction::Refuse(call_id)),
                            ] {
                                if let Some(rect) = props.hits.add_clipped(
                                    format!("{label} {}", card.name),
                                    row.widget(cx, id).area().rect(cx),
                                    clip,
                                    MouseCursor::Hand,
                                    props.slot,
                                ) {
                                    self.hits.push((rect, action));
                                }
                            }
                        }
                    }
                    if card.has_body() {
                        if let Some(rect) = props.hits.add_clipped(
                            card.label(),
                            row.widget(cx, ids!(line)).area().rect(cx),
                            clip,
                            MouseCursor::Hand,
                            props.slot,
                        ) {
                            self.hits.push((rect, RowAction::Toggle(card.key)));
                        }
                        for (wrap, id) in [
                            (ids!(out), ids!(out.body_txt)),
                            (ids!(err), ids!(err.err_txt)),
                        ] {
                            let field = row.text_input(cx, id);
                            if !row.widget(cx, wrap).visible() {
                                continue;
                            }
                            let text = field.text();
                            if let Some(line) = text.lines().find(|l| !l.trim().is_empty()) {
                                props.hits.add_clipped(
                                    harness::clip(line.trim(), 120),
                                    row.widget(cx, id).area().rect(cx),
                                    clip,
                                    MouseCursor::Text,
                                    props.slot,
                                );
                            }
                        }
                    } else if let Some(rect) = props.hits.add_clipped(
                        card.label(),
                        row.widget(cx, ids!(line)).area().rect(cx),
                        clip,
                        MouseCursor::Default,
                        props.slot,
                    ) {
                        let _ = rect;
                    }
                }
                Row::Code {
                    kind,
                    old,
                    new,
                    path,
                    old_path,
                    ..
                } if !matches!(kind, CodeKind::Hunk | CodeKind::Note) => {
                    for (id, reference) in [
                        (
                            ids!(old_box.old_line),
                            old.map(|n| format!("{old_path}:{n} (base)")),
                        ),
                        (ids!(new_box.new_line), new.map(|n| format!("{path}:{n}"))),
                    ] {
                        if let Some(reference) = reference {
                            if let Some(rect) = props.hits.add_clipped(
                                format!("copy {reference}"),
                                row.widget(cx, id).area().rect(cx),
                                clip,
                                MouseCursor::Hand,
                                props.slot,
                            ) {
                                self.hits.push((rect, RowAction::Copy(reference)));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        for (label, id, verb) in BUTTONS {
            let widget = self.view.widget(cx, id);
            let parent: &[LiveId] = match *verb {
                "workshop.push" => &ids!(hub.push_line)[..],
                "workshop.create_pr" => &ids!(hub)[..],
                "workshop.terminal" => &ids!(terminal)[..],
                "workshop.apply_model" => &ids!(form)[..],
                _ => &ids!(composer)[..],
            };
            if widget.visible() && self.view.widget(cx, parent).visible() {
                if let Some(rect) = props.hits.add_clipped(
                    label.to_string(),
                    widget.area().rect(cx),
                    self.view.area().rect(cx),
                    MouseCursor::Hand,
                    props.slot,
                ) {
                    self.hits.push((rect, RowAction::Verb(verb)));
                }
            }
        }
        let external = self.view.widget(cx, ids!(github_open_btn));
        if self.view.widget(cx, ids!(merge_picker)).visible() {
            props.hits.add_clipped(
                "open on GitHub",
                external.area().rect(cx),
                self.view.area().rect(cx),
                MouseCursor::Hand,
                props.slot,
            );
            props.hits.add_clipped(
                "merge method",
                self.view.widget(cx, ids!(merge_btn)).area().rect(cx),
                self.view.area().rect(cx),
                MouseCursor::Hand,
                props.slot,
            );
        }
        if self.view.widget(cx, ids!(providers)).visible() {
            for (label, id) in [("provider", ids!(provider_btn)), ("model", ids!(model_btn))] {
                props.hits.add_clipped(
                    label,
                    self.view.widget(cx, id).area().rect(cx),
                    self.view.area().rect(cx),
                    MouseCursor::Hand,
                    props.slot,
                );
            }
        }
        if self.view.widget(cx, ids!(composer)).visible() && self.view.widget(cx, ids!(mode_btn)).visible() {
            props.hits.add_clipped(
                "mode",
                self.view.widget(cx, ids!(mode_btn)).area().rect(cx),
                self.view.area().rect(cx),
                MouseCursor::Hand,
                props.slot,
            );
        }
        for (label, id, parent) in [
            ("message", ids!(ask_input), ids!(composer)),
            ("repository path", ids!(field_input), ids!(form)),
            ("default model", ids!(second_input), ids!(second_field)),
        ] {
            let widget = self.view.widget(cx, id);
            if self.view.widget(cx, parent).visible() {
                props
                    .hits
                    .add(label, widget.area().rect(cx), MouseCursor::Text, props.slot);
                props.keyboard.keep(&widget, Letters::ALL);
            }
        }
        select::draw_open(
            cx,
            scope,
            self.view.area().rect(cx),
            &props,
            &self.controls(cx),
        );
        let visible_tail = clip.size.x > 0.0
            && clip.size.y > 0.0
            && self.view.portal_list(cx, ids!(list)).is_at_end();
        if self
            .read_tracker
            .displayed(drawn_result, props.has_keyboard, visible_tail)
        {
            // Receipt dispatch belongs to the next event, after this version
            // was painted. Do not mutate the transcript while drawing it.
            cx.new_next_frame();
        }
        DrawStep::done()
    }
}

fn displayed_change_patch(store: &kernel::store::Store, change: &model::ChangeRow) -> String {
    if change.patch.starts_with("diff --git ") {
        return change.patch.clone();
    }
    model::snapshot(store, change.snapshot_id)
        .and_then(|snapshot| serde_json::from_str::<super::git::Snapshot>(&snapshot.json).ok())
        .and_then(|snapshot| {
            snapshot
                .files
                .into_iter()
                .find(|file| file.path == change.path)
        })
        .map(|file| super::git::displayed_patch(&file))
        .unwrap_or_else(|| change.patch.clone())
}

/// The word beside a chat: what its agent is doing now.
fn chat_state(chat: &model::ChatRow) -> &'static str {
    match chat.status.as_str() {
        "running" => "working…",
        "pending" | "queued" => "queued",
        "waiting" => "waiting for approval",
        "ready" | "done" | "idle" => "ready",
        "stopped" => "stopped",
        "failed" => "failed",
        "interrupted" => "interrupted",
        _ => "",
    }
}
fn short_provider(provider: &str) -> &'static str {
    if provider == "claude" {
        "Claude"
    } else {
        "Codex"
    }
}
/// The first line of the last thing said in a chat, for the hub's row.
fn preview_line(role: &str, body: &str) -> String {
    let line = body.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    if line.is_empty() {
        return String::new();
    }
    let prefix = if matches!(role, "user" | "You") {
        "you: "
    } else {
        ""
    };
    harness::clip(&format!("{prefix}{line}"), 120)
}
/// The pull request's state in a few words, and whether it is bad news.
fn pr_state(workspace: &model::WorkspaceRow, pr: Option<&serde_json::Value>) -> (String, bool) {
    let Some(pr) = pr else {
        return if !workspace.error.is_empty() {
            ("GitHub status unavailable".into(), true)
        } else if workspace.status == "preparing" {
            ("preparing".into(), false)
        } else {
            (String::new(), false)
        };
    };
    let state = pr["state"].as_str().unwrap_or("").to_ascii_lowercase();
    if state == "merged" {
        return ("merged".into(), false);
    }
    if pr["mergeable"].as_str() == Some("CONFLICTING") || pr["merge_state"].as_str() == Some("DIRTY") {
        return ("rebase conflict".into(), true);
    }
    if pr["draft"] == true {
        return ("draft".into(), false);
    }
    let checks = pr["checks"].as_array().cloned().unwrap_or_default();
    let failed = checks
        .iter()
        .filter(|c| {
            matches!(
                c["conclusion"].as_str().unwrap_or(""),
                "FAILURE" | "failure" | "TIMED_OUT" | "timed_out" | "CANCELLED" | "cancelled" | "ERROR" | "error"
            )
        })
        .count();
    let pending = checks
        .iter()
        .filter(|c| {
            c["conclusion"].as_str().unwrap_or("").is_empty()
                || matches!(c["status"].as_str().unwrap_or(""), "IN_PROGRESS" | "QUEUED" | "PENDING" | "in_progress" | "queued" | "pending")
                    && c["conclusion"].as_str().unwrap_or("").is_empty()
        })
        .count();
    if failed > 0 {
        return (
            format!("{failed} check{} failed", if failed == 1 { "" } else { "s" }),
            true,
        );
    }
    if !checks.is_empty() && pending > 0 {
        return ("checks running".into(), false);
    }
    if !checks.is_empty() {
        return ("checks passed".into(), false);
    }
    (state, false)
}

/// The transcript as rows: each turn of the person, and for each turn of the
/// agent its prose and cards in the order they happened, then the link to
/// what it changed. A subagent's calls sit under its card, shown when it is
/// open.
fn chat_rows(p: &Detail, open: &HashSet<CardKey>) -> Vec<Row> {
    let messages = model::messages(&p.store, p.subject);
    let items = model::items(&p.store, p.subject);
    let calls = model::tool_calls(&p.store, p.subject);
    let mut rows = Vec::new();
    for m in messages.iter() {
        if matches!(m.role.as_str(), "user" | "You") {
            rows.push(Row::User {
                text: m.body.clone(),
            });
            continue;
        }
        if matches!(m.role.as_str(), "tool" | "permission_denied" | "error") {
            // Older transcripts kept these as bare lines.
            let mut card = Card {
                key: CardKey::Item(-m.id),
                kind: m.role.clone(),
                name: if m.role == "error" {
                    "error".into()
                } else if m.role == "permission_denied" {
                    "denied".into()
                } else {
                    "tool".into()
                },
                status: if m.role == "tool" { "done" } else { "failed" }.into(),
                ..Default::default()
            };
            card.title = harness::clip(m.body.lines().next().unwrap_or(""), 120);
            card.body = m.body.clone();
            rows.push(Row::Card(card));
            continue;
        }
        let who = m.role.clone();
        let run_items: Vec<&model::ItemRow> = items
            .iter()
            .filter(|i| Some(i.run_id) == m.run_id)
            .collect();
        let run_calls: Vec<&model::ToolCallRow> = calls
            .iter()
            .filter(|c| Some(c.run_id) == m.run_id)
            .collect();
        let start = rows.len();
        if run_items.is_empty() && run_calls.is_empty() {
            if !m.body.trim().is_empty() {
                rows.push(Row::Text {
                    who: who.clone(),
                    html: crate::apps::agent::text::html(&m.body),
                });
            }
        } else {
            let mut lines: Vec<(f64, i64, Vec<Row>)> = Vec::new();
            for item in run_items.iter().filter(|i| i.parent.is_empty()) {
                let mut group = Vec::new();
                item_rows(item, &run_items, 0, open, &mut group);
                lines.push((item.created, item.id, group));
            }
            for call in &run_calls {
                lines.push((call.created, i64::MAX / 2 + call.id, vec![Row::Card(app_card(call))]));
            }
            lines.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
            for (_, _, group) in lines {
                rows.extend(group);
            }
        }
        if let Some(first) = rows.get_mut(start) {
            match first {
                Row::Text { who: w, .. } => *w = who.clone(),
                Row::Card(card) => card.who = who.clone(),
                _ => {}
            }
        }
        if let Some(step) = m
            .step_id
            .and_then(|id| model::step(&p.store, id))
            .filter(|step| step.has_changes)
        {
            let (added, deleted) = model::changes(&p.store, step.diff_id)
                .iter()
                .fold((0, 0), |(a, d), f| (a + f.added, d + f.deleted));
            rows.push(Row::Step {
                id: step.id,
                added,
                deleted,
            });
        }
    }
    rows
}
/// One item as rows: prose or a card, then, for an open subagent, what it
/// called, one level in.
fn item_rows(
    item: &model::ItemRow,
    all: &[&model::ItemRow],
    depth: u8,
    open: &HashSet<CardKey>,
    out: &mut Vec<Row>,
) {
    let children: Vec<&model::ItemRow> = all.iter().copied().filter(|c| c.parent == item.key).collect();
    if item.kind == "text" {
        if !item.body.trim().is_empty() {
            out.push(Row::Text {
                who: String::new(),
                html: crate::apps::agent::text::html(&item.body),
            });
        }
        return;
    }
    if item.kind == "reasoning" {
        // Folded thinking: one muted card, opened on a press.
        let mut card = item_card(item, depth, 0);
        card.name = "reasoning".into();
        out.push(Row::Card(card));
        return;
    }
    let card = item_card(item, depth, children.len());
    let key = card.key;
    out.push(Row::Card(card));
    if !children.is_empty() && open.contains(&key) {
        for child in children {
            item_rows(child, all, depth.saturating_add(1), open, out);
        }
    }
}
fn item_card(item: &model::ItemRow, depth: u8, children: usize) -> Card {
    let meta: serde_json::Value = serde_json::from_str(&item.meta).unwrap_or(serde_json::Value::Null);
    let mut card = Card {
        key: CardKey::Item(item.id),
        depth,
        kind: item.kind.clone(),
        name: item.name.clone(),
        title: item.title.clone(),
        status: item.status.clone(),
        ..Default::default()
    };
    match item.kind.as_str() {
        "todo" => {
            card.name = "todo".into();
            card.todo = item.body.clone();
        }
        "agent" | "task" => {
            if let Some(kind) = meta["subagent_type"].as_str().filter(|k| !k.is_empty()) {
                card.name = format!("{} · {kind}", card.name);
            }
            let progress = &meta["progress"];
            let mut parts = Vec::new();
            if let Some(n) = progress["tool_uses"].as_u64() {
                parts.push(format!("{n} tool use{}", if n == 1 { "" } else { "s" }));
            } else if children > 0 {
                parts.push(format!("{children} call{}", if children == 1 { "" } else { "s" }));
            }
            if let Some(tokens) = progress["total_tokens"].as_u64() {
                parts.push(format!("{}k tokens", tokens / 1000));
            }
            if let Some(tool) = progress["last_tool_name"].as_str().filter(|t| !t.is_empty()) {
                parts.push(format!("now {tool}"));
            }
            if let Some(summary) = progress["summary"].as_str().filter(|t| !t.is_empty()) {
                parts.push(summary.to_owned());
            }
            if meta["background"] == true && parts.is_empty() {
                parts.push("in background".into());
            }
            card.progress = parts.join(" · ");
            card.body = item.body.clone();
        }
        "denied" | "error" => {
            card.name = if item.kind == "denied" {
                "denied".into()
            } else {
                "error".into()
            };
            if card.title.is_empty() {
                card.title = harness::clip(item.body.lines().next().unwrap_or(""), 120);
            }
            card.body = item.body.clone();
            if card.status.is_empty() || card.status == "done" {
                card.status = "failed".into();
            }
        }
        _ => {
            card.body = item.body.clone();
            if meta["background"] == true && item.status == "done" {
                card.progress = "ran in background".into();
            }
        }
    }
    card
}
/// An app tool call: the same card, with the two words that answer it while
/// it waits for the person.
fn app_card(call: &model::ToolCallRow) -> Card {
    let input: serde_json::Value = serde_json::from_str(&call.arguments).unwrap_or(serde_json::Value::Null);
    let title = match &input {
        serde_json::Value::Object(map) => map
            .values()
            .filter_map(|v| match v {
                serde_json::Value::String(s) => Some(harness::clip(s, 60)),
                serde_json::Value::Null => None,
                other => Some(harness::clip(&other.to_string(), 60)),
            })
            .collect::<Vec<_>>()
            .join(" "),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    };
    let mut body = call.result.clone();
    if !call.error.is_empty() {
        body = call.error.clone();
    }
    let status = match call.status.as_str() {
        "done" => "done",
        "pending" => "pending",
        "approved" | "running" => "running",
        "refused" => "refused",
        "failed" => "failed",
        "interrupted" => "interrupted",
        other => other,
    };
    Card {
        key: CardKey::AppCall(call.id),
        kind: "tool".into(),
        name: call.name.clone(),
        title: harness::clip(&title, 160),
        status: status.into(),
        body,
        asking: call.status == "pending",
        ..Default::default()
    }
}

fn update_options(current: &mut Arc<[SelectOption]>, options: Vec<SelectOption>) {
    if current.as_ref() != options.as_slice() {
        *current = options.into();
    }
}
fn pr_value(workspace: &model::WorkspaceRow) -> Option<serde_json::Value> {
    serde_json::from_str(&workspace.pr_json)
        .ok()
        .filter(|v: &serde_json::Value| v.is_object())
}
fn has_pr(workspace: &model::WorkspaceRow) -> bool {
    pr_value(workspace).is_some_and(|v| v.get("number").and_then(|n| n.as_u64()).is_some())
}
fn workspace_status(workspace: &model::WorkspaceRow) -> String {
    if workspace.archived {
        return "archived".into();
    }
    if let Some(pr) = pr_value(workspace) {
        let number = pr
            .get("number")
            .and_then(|v| v.as_u64())
            .unwrap_or_default();
        let state = pr
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        format!(
            "#{number} · {}{}",
            state.to_lowercase(),
            if workspace.error.is_empty() {
                ""
            } else {
                " · stale"
            }
        )
    } else if !workspace.error.is_empty() {
        "GitHub status unavailable".into()
    } else if workspace.pr_json.is_empty() {
        "GitHub status unknown".into()
    } else if workspace.status == "preparing" {
        workspace.status.clone()
    } else {
        "no pull request".into()
    }
}
fn github_rows(panel: &Detail) -> Vec<Row> {
    let Some(workspace) = model::workspace(&panel.store, panel.subject) else {
        return vec![];
    };
    let Some(pr) = pr_value(&workspace) else {
        return vec![];
    };
    let mut rows = vec![];
    for (label, key) in [
        ("pull request", "title"),
        ("head", "head"),
        ("merge state", "merge_state"),
        ("review decision", "review_decision"),
    ] {
        if let Some(value) = pr
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
        {
            rows.push(Row::Message {
                author: label.into(),
                text: value.into(),
                detail: String::new(),
                step: None,
            });
        }
    }
    if let Some(checks) = pr.get("checks").and_then(|v| v.as_array()) {
        if checks.is_empty() {
            rows.push(Row::Message {
                author: "checks".into(),
                text: "No check results reported.".into(),
                detail: String::new(),
                step: None,
            });
        }
        for check in checks {
            let name = check
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("check");
            let status = check
                .get("conclusion")
                .and_then(|v| v.as_str())
                .filter(|v| !v.is_empty())
                .or_else(|| check.get("status").and_then(|v| v.as_str()))
                .unwrap_or("unknown");
            rows.push(Row::Message {
                author: name.into(),
                text: status.into(),
                detail: String::new(),
                step: None,
            });
        }
    }
    if let Some(snapshot) = workspace.snapshot_id {
        let coverage = model::progress(&panel.store, snapshot);
        rows.push(Row::Message {
            author: "personal review".into(),
            text: format!(
                "{} / {} changed lines reviewed · {} left",
                coverage.reviewed,
                coverage.total,
                (coverage.total - coverage.reviewed).max(0)
            ),
            detail: "Your review progress does not prevent merging.".into(),
            step: None,
        });
    }
    rows
}
/// A patch as rows: hunk locations, then each line with its numbers, its
/// sign in a column of its own, and the code without it. The file header is
/// what the panel's own lines say; only a diff with no hunks — a rename, a
/// mode change, a binary — shows its metadata as notes.
fn patch_rows(patch: &str, path: &str, old_path: &str) -> Vec<Row> {
    let (mut old, mut new) = (0u64, 0u64);
    let mut in_hunk = false;
    let mut rows = Vec::new();
    let has_hunks = patch.lines().any(|l| l.starts_with("@@ "));
    let code = |kind, old, new, text: &str| Row::Code {
        kind,
        old,
        new,
        text: text.replace('\t', "    "),
        path: path.into(),
        old_path: old_path.into(),
    };
    for text in patch.lines() {
        if text.starts_with("@@ ") {
            let mut terms = text.split_whitespace();
            let _ = terms.next();
            old = terms
                .next()
                .and_then(|v| v.trim_start_matches('-').split(',').next())
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            new = terms
                .next()
                .and_then(|v| v.trim_start_matches('+').split(',').next())
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            in_hunk = true;
            rows.push(code(CodeKind::Hunk, None, None, text));
            continue;
        }
        if !in_hunk {
            if !has_hunks
                && !text.starts_with("diff --git ")
                && !text.starts_with("index ")
                && !text.starts_with("--- ")
                && !text.starts_with("+++ ")
                && !text.trim().is_empty()
            {
                rows.push(code(CodeKind::Note, None, None, text));
            }
            continue;
        }
        match text.as_bytes().first() {
            Some(b'+') => {
                let row = code(CodeKind::Add, None, Some(new), &text[1..]);
                new += 1;
                rows.push(row);
            }
            Some(b'-') => {
                let row = code(CodeKind::Delete, Some(old), None, &text[1..]);
                old += 1;
                rows.push(row);
            }
            Some(b' ') => {
                let row = code(CodeKind::Context, Some(old), Some(new), &text[1..]);
                old += 1;
                new += 1;
                rows.push(row);
            }
            Some(b'\\') => rows.push(code(CodeKind::Note, None, None, text)),
            _ => rows.push(code(CodeKind::Note, None, None, text)),
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel::app::App;
    use rusqlite::params;

    #[test]
    fn historical_metadata_only_comparisons_render_their_saved_modes_without_changing_history() {
        static APPS: &[&dyn App] = &[&super::super::WORKSHOP];
        let session = Session::fake(APPS);
        let current = model::latest_snapshot(session.store(), 1).unwrap();
        let historical = session
            .store()
            .write(move |c| {
                let mut snapshot = super::super::snapshots::get(c, current.id)?;
                snapshot.files.truncate(1);
                let file = &mut snapshot.files[0];
                file.patch.clear();
                file.hunks.clear();
                file.added = 0;
                file.deleted = 0;
                file.old_mode = "100644".into();
                file.new_mode = "100755".into();
                file.status = "M".into();
                super::super::snapshots::put(c, 1, snapshot, 110.0, true)
            })
            .unwrap();
        let change = model::changes(session.store(), historical)[0].clone();
        let stored = model::snapshot(session.store(), historical).unwrap();
        assert!(change.patch.is_empty());
        let displayed = displayed_change_patch(session.store(), &change);
        assert!(displayed.contains("old mode 100644\nnew mode 100755"));
        assert_eq!(
            model::snapshot(session.store(), historical).unwrap(),
            stored
        );
        assert!(model::change(session.store(), change.id)
            .unwrap()
            .patch
            .is_empty());
        assert_eq!(
            model::latest_snapshot(session.store(), 1).unwrap().id,
            current.id
        );
    }

    #[test]
    fn chat_reads_only_acknowledge_focused_results_drawn_at_the_tail_once_per_version() {
        let mut reads = ChatReadTracker::default();
        assert!(!reads.displayed(Some((1, 1)), false, true));
        assert_eq!(reads.acknowledge(false, true), None);
        assert!(!reads.displayed(Some((1, 1)), true, false));
        assert_eq!(reads.acknowledge(true, false), None);
        assert!(reads.displayed(Some((1, 1)), true, true));
        assert_eq!(reads.acknowledge(false, true), None);
        assert_eq!(reads.acknowledge(true, true), Some((1, 1)));
        assert!(!reads.displayed(Some((1, 1)), true, true));
        assert_eq!(reads.acknowledge(true, true), None);
        assert!(reads.displayed(Some((1, 2)), true, true));
        assert_eq!(reads.acknowledge(true, false), None);
        assert_eq!(reads.acknowledge(true, true), Some((1, 2)));
        assert!(!reads.displayed(None, true, true));
        assert_eq!(reads.acknowledge(true, true), None);
    }

    #[test]
    fn a_read_receipt_cannot_acknowledge_a_newer_result_that_has_not_been_drawn() {
        static APPS: &[&dyn App] = &[&super::super::WORKSHOP];
        let mut session = Session::fake(APPS);
        let touch = |session: &mut Session, viewed_version| {
            session.act_async(
                runtime::command_edit(
                    Command::TouchChat {
                        chat_id: 1,
                        viewed_version,
                    },
                    300.0,
                    "/sample/store".into(),
                    None,
                    "human".into(),
                )
                .wake_if(|_| false),
                |_, result| assert!(result.is_some()),
            );
        };
        session
            .store()
            .write(|c| {
                c.execute(
                    "UPDATE workshop_chat SET unread=1,unread_version=1 WHERE id=1",
                    [],
                )
            })
            .unwrap();
        let mut reads = ChatReadTracker::default();
        reads.displayed(Some((1, 1)), true, true);
        session
            .store()
            .write(|c| {
                c.execute(
                    "UPDATE workshop_chat SET unread=1,unread_version=2 WHERE id=1",
                    [],
                )
            })
            .unwrap();
        let (_, version) = reads.acknowledge(true, true).unwrap();
        touch(&mut session, Some(version));
        assert!(model::chat(session.store(), 1).unwrap().unread);
        touch(&mut session, None);
        assert!(model::chat(session.store(), 1).unwrap().unread);
        reads.displayed(Some((1, 2)), true, true);
        let (_, version) = reads.acknowledge(true, true).unwrap();
        touch(&mut session, Some(version));
        assert!(!model::chat(session.store(), 1).unwrap().unread);
        session
            .store()
            .write(|c| {
                c.execute(
                    "UPDATE workshop_chat SET unread=1,unread_version=3 WHERE id=1",
                    [],
                )
            })
            .unwrap();
        reads.displayed(Some((1, 3)), true, true);
        let (_, version) = reads.acknowledge(true, true).unwrap();
        touch(&mut session, Some(version));
        assert!(!model::chat(session.store(), 1).unwrap().unread);
    }

    #[test]
    fn chat_previews_hide_empty_turns_and_keep_non_text_changes() {
        static APPS: &[&dyn App] = &[&super::super::WORKSHOP];
        let mut session = Session::fake(APPS);
        let current = model::latest_snapshot(session.store(), 1).unwrap();
        assert!(model::progress(session.store(), current.id).total > 0);
        let no_change = session.store().write(move |c| {
            let mut snapshot = super::super::snapshots::get(c, current.id)?;
            snapshot.base_oid = snapshot.tree_oid.clone();
            snapshot.files.clear();
            let empty = super::super::snapshots::put(c, 1, snapshot, 110.0, true)?;
            c.execute("INSERT INTO workshop_step(workspace_id,chat_id,before_id,after_id,diff_id,status,created) VALUES(1,1,?1,?1,?2,'done',110)", params![current.id, empty])?;
            let step = c.last_insert_rowid();
            c.execute("INSERT INTO workshop_message(chat_id,role,body,step_id,created) VALUES(1,'Codex','No edits were needed.',?1,110)", [step])?;
            Ok(step)
        }).unwrap();
        assert!(!model::step(session.store(), no_change).unwrap().has_changes);

        let non_text = session.store().write(move |c| {
            let mut snapshot = super::super::snapshots::get(c, current.id)?;
            snapshot.files.truncate(1);
            let file = &mut snapshot.files[0];
            file.path = "image.bin".into();
            file.patch = "Binary files a/image.bin and b/image.bin differ".into();
            file.hunks.clear();
            file.added = 0;
            file.deleted = 0;
            file.binary = true;
            file.non_text = true;
            let diff = super::super::snapshots::put(c, 1, snapshot, 120.0, true)?;
            c.execute("INSERT INTO workshop_step(workspace_id,chat_id,before_id,after_id,diff_id,status,created) VALUES(1,1,?1,?1,?2,'done',120)", params![current.id, diff])?;
            let step = c.last_insert_rowid();
            c.execute("INSERT INTO workshop_message(chat_id,role,body,step_id,created) VALUES(1,'Codex','Updated the image.',?1,120)", [step])?;
            Ok(step)
        }).unwrap();
        assert!(model::step(session.store(), non_text).unwrap().has_changes);

        session.nav(Nav::Open {
            from: 0,
            id: Detail::chat(1),
            fresh: true,
        });
        session.settle();
        let instance = session.panel(session.focus().unwrap()).unwrap();
        let mut borrowed = instance.borrow_mut();
        let panel = borrowed.as_any().downcast_mut::<Detail>().unwrap();
        let rows = chat_rows(panel, &HashSet::new());
        // The step link, when the turn has one, follows the turn's prose.
        let preview = |body: &str| {
            let html = crate::apps::agent::text::html(body);
            let at = rows
                .iter()
                .position(|row| matches!(row, Row::Text { html: h, .. } if *h == html))
                .expect("the transcript still includes the turn");
            match rows.get(at + 1) {
                Some(Row::Step { id, .. }) => Some(*id),
                _ => None,
            }
        };
        assert_eq!(preview("No edits were needed."), None);
        assert_eq!(preview("Updated the image."), Some(non_text));
        assert!(rows
            .iter()
            .any(|row| matches!(row, Row::Step { id, .. } if *id != non_text)));
    }

    #[test]
    fn gutter_references_follow_both_sides_of_each_hunk() {
        let rows = patch_rows(
            "@@ -8,2 +11,3 @@\n same\n-old\n+new\n+extra\n@@ -90 +94 @@\n-before\n+after",
            "new.rs",
            "old.rs",
        );
        let refs = rows
            .into_iter()
            .filter_map(|r| {
                if let Row::Code { old, new, .. } = r {
                    Some((old, new))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(
            refs,
            vec![
                (None, None),
                (Some(8), Some(11)),
                (Some(9), None),
                (None, Some(12)),
                (None, Some(13)),
                (None, None),
                (Some(90), None),
                (None, Some(94))
            ]
        );
    }
}
