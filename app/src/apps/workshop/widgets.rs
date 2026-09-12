//! Native rich tables and ordinary detail panels. Every mutation delegates to
//! the same command dispatcher used by Workshop tools.

use crate::apps::terminal::TerminalViewWidgetRefExt;

use super::{
    model,
    panels::{self, Comparison, Detail, DetailType, Projects, Review, WorkspaceSource, Workspaces},
    runtime::{self, Command},
};
use crate::shell::{
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
use std::sync::Arc;

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
        Workspaces::project(&r.name)
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
        _: f64,
    ) {
        fill_row(
            cx,
            row,
            (selected, marked),
            &r.label,
            &r.project,
            &fmt_date(r.activity),
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
            format!("{count} reviewed")
        } else if r.needs_recheck {
            format!("{count} recheck")
        } else {
            count.to_string()
        };
        let parent = r.path.rsplit_once('/').map_or("", |(p, _)| p);
        fill_row(
            cx,
            row,
            (selected, marked),
            panels::filename(&r.path),
            parent,
            &meta,
            false,
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
                let mut text = format!(
                    "{} / {} lines reviewed\n{} new · {} need recheck",
                    progress.reviewed, progress.total, fresh, progress.recheck
                );
                if progress.other > 0 {
                    text.push_str(&format!("\n{} other changes", progress.other));
                }
                if p.latest().is_some_and(|id| id != p.snapshot_id) {
                    text.push_str("\nnew changes available");
                }
                self.view.label(cx, ids!(progress_lbl)).set_text(cx, &text);
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

#[derive(Clone)]
enum Row {
    Chat(model::ChatRow),
    Tool(model::ToolCallRow),
    Message {
        author: String,
        text: String,
        detail: String,
        step: Option<i64>,
    },
    Code {
        old: Option<u64>,
        new: Option<u64>,
        text: String,
        path: String,
        old_path: String,
    },
}
#[derive(Clone)]
enum RowAction {
    Verb(&'static str),
    Open(PanelId),
    Step(i64),
    Copy(String),
    Approve(i64),
    Refuse(i64),
}
#[derive(Script, ScriptHook, Widget)]
pub struct WorkshopDetail {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[rust]
    hits: Vec<(Rect, RowAction)>,
    #[rust]
    provider_options: Arc<[SelectOption]>,
    #[rust]
    model_options: Arc<[SelectOption]>,
    #[rust]
    merge_options: Arc<[SelectOption]>,
    #[rust]
    shown_panel: Option<PanelId>,
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
    code: Vec<Row>,
}

const BUTTONS: &[(&str, &[LiveId], &str)] = &[
    ("push", ids!(push_btn), "workshop.push"),
    ("create PR", ids!(pr_btn), "workshop.create_pr"),
    ("review changes", ids!(review_btn), "workshop.review"),
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
    fn controls(&self, cx: &Cx) -> [select::SelectRef; 3] {
        [
            self.view.select(cx, ids!(provider_btn)),
            self.view.select(cx, ids!(model_btn)),
            self.view.select(cx, ids!(merge_btn)),
        ]
    }
    fn run(&mut self, scope: &mut Scope, props: &PanelProps, verb: &str) {
        if let Some(s) = scope.data.get_mut::<Session>() {
            if let Some(p) = props.panel.borrow_mut().as_any().downcast_mut::<Detail>() {
                p.run(verb, s);
            }
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
            RowAction::Copy(_) | RowAction::Verb(_) => None,
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
            DetailType::Workspace => model::chats(&p.store, p.subject)
                .iter()
                .cloned()
                .map(Row::Chat)
                .collect(),
            DetailType::ClosedChats => model::closed_chats(&p.store, p.subject)
                .iter()
                .cloned()
                .map(Row::Chat)
                .collect(),
            DetailType::Chat => chat_rows(p),
            DetailType::Diff => {
                if let Some(change) = model::change(&p.store, p.subject) {
                    if self.patch != change.patch {
                        self.code = patch_rows(
                            &change.patch,
                            &change.path,
                            change.old_path.as_deref().unwrap_or(&change.path),
                        );
                        self.patch = change.patch;
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
    fn compose(&mut self, cx: &mut Cx, p: &mut Detail) {
        if self.shown_panel.as_ref() != Some(p.id()) {
            self.primed = false;
            self.shown_draft.clear();
            self.patch.clear();
            self.code.clear();
            self.last_rows = 0;
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
            (
                ids!(workspace_actions),
                matches!(p.kind, DetailType::Workspace | DetailType::Github) && !archived,
            ),
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
        ] {
            self.view.widget(cx, id).set_visible(cx, visible);
        }
        let (meta, mut status, error) = match p.kind {
            DetailType::Workspace => workspace
                .as_ref()
                .map(|w| (w.branch.clone(), workspace_status(w), w.error.clone()))
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
                            format!("{} · {}", c.status, p.mode)
                        },
                        c.error.clone(),
                    )
                })
                .unwrap_or_else(|| (String::new(), "chat unavailable".into(), String::new())),
            DetailType::ClosedChats => (String::new(), String::new(), String::new()),
            DetailType::Diff => detail
                .as_ref()
                .map(|c| {
                    let snapshot = model::snapshot(&p.store, c.snapshot_id);
                    let comparison = snapshot
                        .map(|s| format!("{} → {}", short(&s.base_oid), short(&s.tree_oid)))
                        .unwrap_or_default();
                    (
                        c.path.clone(),
                        format!(
                            "{} changed lines · {}\n{}",
                            c.added + c.deleted,
                            if c.reviewed {
                                "reviewed"
                            } else if c.needs_recheck {
                                "needs recheck"
                            } else {
                                "unreviewed"
                            },
                            comparison
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
        if let Some(w) = workspace.as_ref() {
            let number = pr_value(w).and_then(|pr| pr["number"].as_u64());
            self.view.button(cx, ids!(pr_btn)).set_text(
                cx,
                &number
                    .map(|n| format!("#{n}"))
                    .unwrap_or_else(|| "create PR".into()),
            );
            self.view
                .widget(cx, ids!(pr_btn))
                .set_visible(cx, p.kind == DetailType::Workspace || number.is_none());
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
        self.view.widget(cx, ids!(stop_btn)).set_visible(
            cx,
            chat.as_ref().is_some_and(|c| {
                matches!(
                    c.status.as_str(),
                    "running" | "queued" | "pending" | "waiting"
                )
            }),
        );
        self.view
            .widget(cx, ids!(send_btn))
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
            let messages = model::messages(&p.store, p.subject);
            let started = messages
                .iter()
                .any(|m| matches!(m.role.as_str(), "user" | "You"));
            p.observe_chat_submission();
            self.view
                .widget(cx, ids!(provider_hint))
                .set_visible(cx, started && writable_chat);
            let running = matches!(
                chat.status.as_str(),
                "running" | "queued" | "pending" | "waiting"
            );
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
                        p.command(s, Command::TouchChat { chat_id: p.subject });
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
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let terminal_world = scope.data.get_mut::<Session>().map(|s| s.world().clone());
        let rows = {
            let mut borrow = props.panel.borrow_mut();
            let Some(p) = borrow.as_any().downcast_mut::<Detail>() else {
                return DrawStep::done();
            };
            self.compose(cx, p);
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
            self.rows(p)
        };
        self.hits.clear();
        let mut rendered = Vec::new();
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
                    Row::Chat(_) => live_id!(row),
                    Row::Message { .. } | Row::Tool(_) => live_id!(message),
                    Row::Code { .. } => live_id!(code),
                };
                let row = list.item(cx, index, tpl);
                match value {
                    Row::Chat(c) => fill_row(
                        cx,
                        &row,
                        (false, false),
                        &format!(
                            "{} · {} · {}",
                            c.ordinal,
                            panels::provider_label(&c.provider),
                            c.model
                        ),
                        "",
                        if c.closed {
                            "closed"
                        } else if matches!(c.status.as_str(), "ready" | "idle" | "done") {
                            ""
                        } else {
                            &c.status
                        },
                        c.unread && !c.closed,
                    ),
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
                        row.widget(cx, ids!(approval)).set_visible(cx, false);
                    }
                    Row::Tool(call) => {
                        row.label(cx, ids!(author_lbl)).set_text(cx, &call.name);
                        let detail = if call.status == "pending" {
                            "waiting for approval"
                        } else {
                            &call.status
                        };
                        row.label(cx, ids!(detail_lbl)).set_text(cx, detail);
                        row.widget(cx, ids!(detail_lbl)).set_visible(cx, true);
                        row.widget(cx, ids!(open_btn)).set_visible(cx, false);
                        row.widget(cx, ids!(approval))
                            .set_visible(cx, call.status == "pending");
                        let mut body = call.arguments.clone();
                        if !call.result.is_empty() {
                            body.push_str("\n\n");
                            body.push_str(&call.result);
                        }
                        if !call.error.is_empty() {
                            body.push_str("\n\n");
                            body.push_str(&call.error);
                        }
                        let preview = if body.chars().count() > 5000 {
                            format!("{}\n…", body.chars().take(5000).collect::<String>())
                        } else {
                            body
                        };
                        let field = row.text_input(cx, ids!(text_lbl));
                        if field.text() != preview {
                            field.set_text(cx, &preview);
                        }
                    }
                    Row::Code { old, new, text, .. } => {
                        row.label(cx, ids!(old_line))
                            .set_text(cx, &old.map(|n| n.to_string()).unwrap_or_default());
                        row.label(cx, ids!(new_line))
                            .set_text(cx, &new.map(|n| n.to_string()).unwrap_or_default());
                        let field = row.text_input(cx, ids!(code_lbl));
                        if field.text() != *text {
                            field.set_text(cx, text);
                        }
                    }
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
        let clip = self.view.widget(cx, ids!(list)).area().rect(cx);
        for (row, value) in rendered {
            match value {
                Row::Chat(c) => {
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
                        "preview diff",
                        row.widget(cx, ids!(open_btn)).area().rect(cx),
                        clip,
                        MouseCursor::Hand,
                        props.slot,
                    ) {
                        self.hits.push((rect, RowAction::Step(id)));
                    }
                }
                Row::Tool(call) => {
                    if call.status == "pending" {
                        for (label, id, action) in [
                            ("approve", ids!(approve_btn), RowAction::Approve(call.id)),
                            ("refuse", ids!(refuse_btn), RowAction::Refuse(call.id)),
                        ] {
                            if let Some(rect) = props.hits.add_clipped(
                                format!("{label} {}", call.name),
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
                Row::Code {
                    old,
                    new,
                    path,
                    old_path,
                    ..
                } => {
                    for (id, reference) in [
                        (
                            ids!(old_line),
                            old.map(|n| format!("{old_path}:{n} (base)")),
                        ),
                        (ids!(new_line), new.map(|n| format!("{path}:{n}"))),
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
            let parent = match *verb {
                "workshop.push" | "workshop.create_pr" | "workshop.review" => {
                    ids!(workspace_actions)
                }
                "workshop.terminal" => ids!(terminal),
                "workshop.apply_model" => ids!(form),
                _ => ids!(composer),
            };
            if widget.visible() && self.view.widget(cx, parent).visible() {
                if let Some(rect) = props.hits.add_clipped(
                    if *label == "create PR" {
                        self.view.button(cx, id).text()
                    } else {
                        label.to_string()
                    },
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
        DrawStep::done()
    }
}

fn chat_rows(p: &Detail) -> Vec<Row> {
    let mut rows = model::messages(&p.store, p.subject)
        .iter()
        .map(|m| {
            (
                m.created,
                m.id,
                Row::Message {
                    author: if matches!(m.role.as_str(), "user" | "You") {
                        "you".into()
                    } else {
                        m.role.clone()
                    },
                    text: m.body.clone(),
                    detail: fmt_date(m.created),
                    step: m.step_id.filter(|id| {
                        model::step(&p.store, *id).is_some_and(|step| step.has_changes)
                    }),
                },
            )
        })
        .collect::<Vec<_>>();
    rows.extend(
        model::tool_calls(&p.store, p.subject)
            .iter()
            .map(|call| (call.created, call.id, Row::Tool(call.clone()))),
    );
    rows.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    rows.into_iter().map(|(_, _, row)| row).collect()
}

fn update_options(current: &mut Arc<[SelectOption]>, options: Vec<SelectOption>) {
    if current.as_ref() != options.as_slice() {
        *current = options.into();
    }
}
fn short(value: &str) -> &str {
    value.get(..8).unwrap_or(value)
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
fn patch_rows(patch: &str, path: &str, old_path: &str) -> Vec<Row> {
    let (mut old, mut new) = (0u64, 0u64);
    let mut in_hunk = false;
    let mut rows = Vec::new();
    for text in patch.lines() {
        let (mut old_line, mut new_line) = (None, None);
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
        } else if in_hunk {
            match text.as_bytes().first() {
                Some(b'+') => {
                    new_line = Some(new);
                    new += 1;
                }
                Some(b'-') => {
                    old_line = Some(old);
                    old += 1;
                }
                Some(b' ') => {
                    old_line = Some(old);
                    new_line = Some(new);
                    old += 1;
                    new += 1;
                }
                _ => {}
            }
        }
        rows.push(Row::Code {
            old: old_line,
            new: new_line,
            text: text.into(),
            path: path.into(),
            old_path: old_path.into(),
        });
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel::app::App;
    use rusqlite::params;

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
        let rows = chat_rows(panel);
        let preview = |body: &str| {
            rows.iter()
                .find_map(|row| match row {
                    Row::Message { text, step, .. } if text == body => Some(*step),
                    _ => None,
                })
                .expect("the transcript still includes the turn")
        };
        assert_eq!(preview("No edits were needed."), None);
        assert_eq!(preview("Updated the image."), Some(non_text));
        assert!(rows
            .iter()
            .any(|row| matches!(row, Row::Message { step: Some(id), .. } if *id != non_text)));
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
