//! Persistent panel identities and lightweight, cached presentation state.

use super::{
    model,
    runtime::{self, Command},
};
use kernel::{
    filter::{Ast, Op},
    layout::SlotId,
    nav::Nav,
    panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb},
    richtable::{Datasource, ListState, SqlSource, Suggestion, TagDef},
    session::Session,
    store::{Store, Val, Q},
};
use std::{any::Any, rc::Rc, task::Poll};

pub static KINDS: &[&dyn PanelKind] = &[
    &ProjectsKind,
    &WorkspacesKind,
    &ReviewKind,
    &DetailKind(DetailType::Workspace),
    &DetailKind(DetailType::Chat),
    &DetailKind(DetailType::ClosedChats),
    &DetailKind(DetailType::Diff),
    &DetailKind(DetailType::Activity),
    &DetailKind(DetailType::Github),
    &DetailKind(DetailType::Comment),
    &DetailKind(DetailType::Settings),
    &DetailKind(DetailType::AddProject),
];

pub struct Projects {
    id: PanelId,
    pub slot: SlotId,
    pub filter: String,
    pub list: ListState<&'static SqlSource<model::ProjectRow, i64>>,
}
impl Projects {
    pub const TAG: Tag = Tag("workshop_projects");
    pub fn id() -> PanelId {
        PanelId::bare(Self::TAG)
    }
}
impl Panel for Projects {
    fn id(&self) -> &PanelId {
        &self.id
    }
    fn title(&self) -> String {
        "projects".into()
    }
    fn wish(&self, _: usize) -> (u32, u32) {
        (3, 6)
    }
    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }
    fn persist(&self) -> PanelId {
        PanelId::new(Self::TAG, [self.list.table().filter()])
    }
    fn about(&self) -> String {
        "Workshop's local repositories. Opening a project opens the shared workspace table with an editable project filter. These repositories and all Workshop state remain on this device.".into()
    }
    fn verbs(&self) -> Vec<Verb> {
        vec![
            Verb::go(
                "workshop.add_project",
                "add repository",
                Some('a'),
                Nav::Open {
                    from: self.slot,
                    id: Detail::add_project(),
                    fresh: false,
                },
            ),
            Verb::go(
                "workshop.settings",
                "settings",
                None,
                Nav::Open {
                    from: self.slot,
                    id: Detail::settings(),
                    fresh: false,
                },
            ),
        ]
    }
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}
struct ProjectsKind;
impl PanelKind for ProjectsKind {
    fn tag(&self) -> Tag {
        Projects::TAG
    }
    fn open(&self, id: &PanelId, _: &mut Opening<'_>) -> Box<dyn Panel> {
        let filter = id.arg(0).unwrap_or("").to_owned();
        let mut list = ListState::new(&model::PROJECTS, 50);
        list.set_filter(&filter);
        Box::new(Projects {
            id: id.clone(),
            slot: 0,
            filter,
            list,
        })
    }
}

pub struct Workspaces {
    id: PanelId,
    pub slot: SlotId,
    pub store: Rc<Store>,
    pub filter: String,
    pub list: ListState<WorkspaceSource>,
}
impl Workspaces {
    pub const TAG: Tag = Tag("workshop_workspaces");
    pub fn id() -> PanelId {
        PanelId::bare(Self::TAG)
    }
    pub fn project(name: &str) -> PanelId {
        PanelId::new(
            Self::TAG,
            [format!("@project:\"{}\"", name.replace('"', "\\\""))],
        )
    }
    fn project_for_new(&self) -> Option<i64> {
        let projects = model::projects(&self.store);
        let ast = kernel::filter::parse(self.list.table().filter()).ast;
        fn project_term(ast: &Ast) -> Option<&str> {
            match ast {
                Ast::Op { tag, value, .. } if tag == "project" => Some(value),
                Ast::And(terms) => terms.iter().find_map(project_term),
                _ => None,
            }
        }
        ast.as_ref()
            .and_then(project_term)
            .and_then(|name| projects.iter().find(|p| p.name == name).map(|p| p.id))
            .or_else(|| {
                self.list
                    .cursor_key()
                    .and_then(|id| model::workspace(&self.store, *id))
                    .map(|w| w.project_id)
            })
            .or_else(|| model::workspaces(&self.store).first().map(|w| w.project_id))
            .or_else(|| projects.first().map(|p| p.id))
    }
}
impl Panel for Workspaces {
    fn id(&self) -> &PanelId {
        &self.id
    }
    fn title(&self) -> String {
        "workspaces".into()
    }
    fn wish(&self, _: usize) -> (u32, u32) {
        (4, 6)
    }
    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }
    fn persist(&self) -> PanelId {
        PanelId::new(Self::TAG, [self.list.table().filter()])
    }
    fn about(&self) -> String {
        "Local agent workspaces ordered by meaningful recent activity. Bold rows contain an unread agent result. Archived workspaces are hidden unless the filter includes @archived; combine it with @project to browse one repository. New workspace immediately creates an automatic city label and opens its workspace and default chat.".into()
    }
    fn verbs(&self) -> Vec<Verb> {
        vec![Verb::run(
            "workshop.new_workspace",
            "new workspace",
            Some('n'),
        )]
    }
    fn run(&mut self, verb: &str, s: &mut Session) {
        if verb == "workshop.new_workspace" {
            if let Some(project_id) = self.project_for_new() {
                runtime::dispatch(s, self.slot, Command::NewWorkspace { project_id });
            } else {
                s.nav_within(Nav::Open {
                    from: self.slot,
                    id: Detail::add_project(),
                    fresh: false,
                });
            }
        }
    }
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}
struct WorkspacesKind;
impl PanelKind for WorkspacesKind {
    fn tag(&self) -> Tag {
        Workspaces::TAG
    }
    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        let filter = id.arg(0).unwrap_or("").to_string();
        let mut list = ListState::new(WorkspaceSource, 50);
        list.set_filter(&filter);
        Box::new(Workspaces {
            id: id.clone(),
            slot: 0,
            store: cx.session().store().clone(),
            filter,
            list,
        })
    }
}

/// Archive visibility is a table default. Mentioning the archive tag takes
/// control of that choice without adding a hidden or duplicated filter token.
pub struct WorkspaceSource;
impl WorkspaceSource {
    fn scoped(ast: Option<&Ast>) -> Ast {
        if let Some(ast) = ast {
            if ast.tag_names().contains(&"archived") {
                return ast.clone();
            }
        }
        let active = Ast::Not(Box::new(Ast::Tag("archived".into())));
        match ast {
            Some(ast) => Ast::And(vec![active, ast.clone()]),
            None => active,
        }
    }
}
impl Datasource for WorkspaceSource {
    type Row = model::WorkspaceRow;
    type Key = i64;
    fn tags(&self) -> &'static [TagDef] {
        model::WORKSPACES.tags()
    }
    fn key(&self, row: &Self::Row) -> i64 {
        row.id
    }
    fn key_text(&self, key: &i64) -> String {
        key.to_string()
    }
    fn key_parse(&self, text: &str) -> Option<i64> {
        text.parse().ok()
    }
    fn count(&self, store: &Store, ast: Option<&Ast>) -> Option<usize> {
        model::WORKSPACES.count(store, Some(&Self::scoped(ast)))
    }
    fn page(
        &self,
        store: &Store,
        ast: Option<&Ast>,
        offset: usize,
        limit: usize,
    ) -> Rc<Vec<Self::Row>> {
        model::WORKSPACES.page(store, Some(&Self::scoped(ast)), offset, limit)
    }
    fn keys(&self, store: &Store, ast: Option<&Ast>) -> Option<Vec<i64>> {
        model::WORKSPACES.keys(store, Some(&Self::scoped(ast)))
    }
    fn present(&self, store: &Store, ast: Option<&Ast>, keys: &[i64]) -> Vec<i64> {
        model::WORKSPACES.present(store, Some(&Self::scoped(ast)), keys)
    }
    fn by_key(&self, store: &Store, key: &i64) -> Option<Self::Row> {
        model::WORKSPACES.by_key(store, key)
    }
    fn poll_keys(&self, store: &Store, ast: Option<&Ast>) -> Poll<Option<Vec<i64>>> {
        model::WORKSPACES.poll_keys(store, Some(&Self::scoped(ast)))
    }
    fn poll_present(&self, store: &Store, ast: Option<&Ast>, keys: &[i64]) -> Poll<Vec<i64>> {
        model::WORKSPACES.poll_present(store, Some(&Self::scoped(ast)), keys)
    }
    fn poll_by_key(&self, store: &Store, key: &i64) -> Poll<Option<Self::Row>> {
        model::WORKSPACES.poll_by_key(store, key)
    }
    fn index_of(&self, store: &Store, ast: Option<&Ast>, row: &Self::Row) -> Option<usize> {
        model::WORKSPACES.index_of(store, Some(&Self::scoped(ast)), row)
    }
    fn suggest(&self, store: &Store, tag: &str, prefix: &str) -> Vec<Suggestion> {
        model::WORKSPACES.suggest(store, tag, prefix)
    }
}

/// The comparison is a datasource boundary, not a removable filter token.
/// Clearing the ordinary filter cannot accidentally expose another workspace.
pub struct Comparison {
    pub snapshot: i64,
}
impl Comparison {
    fn scoped(&self, ast: Option<&Ast>) -> Ast {
        let scope = Ast::Op {
            tag: "snapshot".into(),
            op: Op::Eq,
            value: self.snapshot.to_string(),
        };
        match ast {
            Some(ast) => Ast::And(vec![scope, ast.clone()]),
            None => scope,
        }
    }
}
impl Datasource for Comparison {
    type Row = model::ChangeRow;
    type Key = i64;
    fn tags(&self) -> &'static [TagDef] {
        model::CHANGES.tags()
    }
    fn key(&self, row: &Self::Row) -> i64 {
        row.id
    }
    fn key_text(&self, key: &i64) -> String {
        key.to_string()
    }
    fn key_parse(&self, text: &str) -> Option<i64> {
        text.parse().ok()
    }
    fn count(&self, store: &Store, ast: Option<&Ast>) -> Option<usize> {
        model::CHANGES.count(store, Some(&self.scoped(ast)))
    }
    fn page(
        &self,
        store: &Store,
        ast: Option<&Ast>,
        offset: usize,
        limit: usize,
    ) -> Rc<Vec<Self::Row>> {
        model::CHANGES.page(store, Some(&self.scoped(ast)), offset, limit)
    }
    fn keys(&self, store: &Store, ast: Option<&Ast>) -> Option<Vec<i64>> {
        model::CHANGES.keys(store, Some(&self.scoped(ast)))
    }
    fn present(&self, store: &Store, ast: Option<&Ast>, keys: &[i64]) -> Vec<i64> {
        model::CHANGES.present(store, Some(&self.scoped(ast)), keys)
    }
    fn by_key(&self, store: &Store, key: &i64) -> Option<Self::Row> {
        model::CHANGES
            .by_key(store, key)
            .filter(|c| c.snapshot_id == self.snapshot)
    }
    fn poll_keys(&self, store: &Store, ast: Option<&Ast>) -> Poll<Option<Vec<i64>>> {
        model::CHANGES.poll_keys(store, Some(&self.scoped(ast)))
    }
    fn poll_present(&self, store: &Store, ast: Option<&Ast>, keys: &[i64]) -> Poll<Vec<i64>> {
        model::CHANGES.poll_present(store, Some(&self.scoped(ast)), keys)
    }
    fn poll_by_key(&self, store: &Store, key: &i64) -> Poll<Option<Self::Row>> {
        model::CHANGES
            .poll_by_key(store, key)
            .map(|row| row.filter(|c| c.snapshot_id == self.snapshot))
    }
    fn index_of(&self, store: &Store, ast: Option<&Ast>, row: &Self::Row) -> Option<usize> {
        model::CHANGES.index_of(store, Some(&self.scoped(ast)), row)
    }
    fn suggest(&self, store: &Store, tag: &str, prefix: &str) -> Vec<Suggestion> {
        model::CHANGES.suggest(store, tag, prefix)
    }
}
pub struct Review {
    id: PanelId,
    pub slot: SlotId,
    pub store: Rc<Store>,
    pub workspace_id: i64,
    pub snapshot_id: i64,
    pub filter: String,
    pub list: ListState<Comparison>,
}
impl Review {
    pub const TAG: Tag = Tag("workshop_review");
    pub fn id(workspace: i64, snapshot: Option<i64>) -> PanelId {
        let mut args = vec![workspace.to_string()];
        if let Some(id) = snapshot {
            args.push(id.to_string());
        }
        PanelId::new(Self::TAG, args)
    }
    pub fn observe(&mut self) {
        if self.snapshot_id == 0 {
            if let Some(snapshot) = model::latest_snapshot(&self.store, self.workspace_id) {
                self.select(snapshot.id);
            }
        }
    }
    pub fn select(&mut self, snapshot: i64) {
        self.snapshot_id = snapshot;
        self.list.retarget(Comparison { snapshot });
    }
    pub fn latest(&self) -> Option<i64> {
        model::latest_snapshot(&self.store, self.workspace_id).map(|s| s.id)
    }
}
impl Panel for Review {
    fn id(&self) -> &PanelId {
        &self.id
    }
    fn title(&self) -> String {
        "review changes".into()
    }
    fn wish(&self, _: usize) -> (u32, u32) {
        (3, 6)
    }
    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }
    fn persist(&self) -> PanelId {
        PanelId::new(
            Self::TAG,
            [
                self.workspace_id.to_string(),
                self.snapshot_id.to_string(),
                self.list.table().filter().into(),
            ],
        )
    }
    fn about(&self) -> String {
        "Whole-file personal review of a pinned comparison. Progress counts changed lines, including files hidden by filters. An edited reviewed file needs recheck in full; an unchanged rebase preserves its mark. Review progress never gates merging.".into()
    }
    fn verbs(&self) -> Vec<Verb> {
        let n = self.list.marks().len();
        if n == 0 {
            vec![]
        } else {
            vec![
                Verb::run(
                    "workshop.mark_files",
                    format!("mark {n} files reviewed"),
                    Some('r'),
                ),
                Verb::run(
                    "workshop.unmark_files",
                    format!("mark {n} files unreviewed"),
                    None,
                ),
            ]
        }
    }
    fn run(&mut self, verb: &str, s: &mut Session) {
        if matches!(verb, "workshop.mark_files" | "workshop.unmark_files") {
            for change_id in self.list.marks().keys() {
                runtime::dispatch(
                    s,
                    self.slot,
                    Command::MarkFile {
                        change_id,
                        reviewed: verb == "workshop.mark_files",
                    },
                );
            }
            self.list.clear_marks();
        }
    }
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}
struct ReviewKind;
impl PanelKind for ReviewKind {
    fn tag(&self) -> Tag {
        Review::TAG
    }
    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        let workspace_id = id.arg(0).and_then(|s| s.parse().ok()).unwrap_or(0);
        let snapshot_id = id.arg(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        let filter = id.arg(2).unwrap_or("").to_owned();
        let mut list = ListState::new(
            Comparison {
                snapshot: snapshot_id,
            },
            50,
        );
        list.set_filter(&filter);
        Box::new(Review {
            id: id.clone(),
            slot: 0,
            store: cx.session().store().clone(),
            workspace_id,
            snapshot_id,
            filter,
            list,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DetailType {
    Workspace,
    Chat,
    ClosedChats,
    Diff,
    Activity,
    Github,
    Comment,
    Settings,
    AddProject,
}
impl DetailType {
    pub fn tag(self) -> Tag {
        Tag(match self {
            Self::Workspace => "workshop_workspace",
            Self::Chat => "workshop_chat",
            Self::ClosedChats => "workshop_closed_chats",
            Self::Diff => "workshop_diff",
            Self::Activity => "workshop_activity",
            Self::Github => "workshop_github",
            Self::Comment => "workshop_comment",
            Self::Settings => "workshop_settings",
            Self::AddProject => "workshop_add_project",
        })
    }
}
pub struct Detail {
    id: PanelId,
    pub slot: SlotId,
    pub store: Rc<Store>,
    pub kind: DetailType,
    pub subject: i64,
    pub path: Option<String>,
    pub field: String,
    pub second: String,
    pub draft: String,
    pub mode: String,
    pub mounted: bool,
    pub custom_model: bool,
    pub submitted_after: Option<i64>,
}
impl Detail {
    pub fn workspace(id: i64) -> PanelId {
        PanelId::new(DetailType::Workspace.tag(), [id.to_string()])
    }
    pub fn chat(id: i64) -> PanelId {
        PanelId::new(DetailType::Chat.tag(), [id.to_string()])
    }
    pub fn closed_chats(workspace_id: i64) -> PanelId {
        PanelId::new(DetailType::ClosedChats.tag(), [workspace_id.to_string()])
    }
    pub fn diff(id: i64) -> PanelId {
        PanelId::new(DetailType::Diff.tag(), [id.to_string()])
    }
    pub fn activity(id: i64) -> PanelId {
        PanelId::new(DetailType::Activity.tag(), [id.to_string()])
    }
    pub fn github(id: i64) -> PanelId {
        PanelId::new(DetailType::Github.tag(), [id.to_string()])
    }
    pub fn comment(id: i64, path: Option<&str>) -> PanelId {
        let mut args = vec![id.to_string()];
        if let Some(path) = path {
            args.push(path.to_string());
        }
        PanelId::new(DetailType::Comment.tag(), args)
    }
    pub fn settings() -> PanelId {
        PanelId::bare(DetailType::Settings.tag())
    }
    pub fn add_project() -> PanelId {
        PanelId::bare(DetailType::AddProject.tag())
    }
    pub fn workspace_id(&self) -> i64 {
        match self.kind {
            DetailType::Chat => {
                model::chat(&self.store, self.subject).map_or(0, |c| c.workspace_id)
            }
            DetailType::Diff => {
                model::change(&self.store, self.subject).map_or(0, |c| c.workspace_id)
            }
            _ => self.subject,
        }
    }
    pub fn pr(&self) -> Option<serde_json::Value> {
        model::workspace(&self.store, self.workspace_id())
            .and_then(|w| serde_json::from_str::<serde_json::Value>(&w.pr_json).ok())
            .filter(|v| v.get("number").and_then(|n| n.as_u64()).is_some())
    }
    pub fn command(&self, s: &mut Session, command: Command) {
        runtime::dispatch(s, self.slot, command);
    }
    pub fn comment_operation(&self) -> Option<(i64, String, String)> {
        static QRY: Q = Q {
            id: "Workshop comment publication",
            describe: "publication state of the newest GitHub comment operation for this draft",
            sql: "SELECT id,status,COALESCE(json_extract(payload,'$.text'),'') FROM workshop_job WHERE kind='comment' AND workspace_id=? AND COALESCE(json_extract(payload,'$.path'),'')=? ORDER BY id DESC LIMIT 1",
        };
        self.store
            .rows(
                &QRY,
                &[
                    Val::I(self.subject),
                    Val::S(self.path.clone().unwrap_or_default()),
                ],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .first()
            .cloned()
    }
    pub fn open(&self, s: &mut Session, id: PanelId) {
        s.nav_within(Nav::Open {
            from: self.slot,
            id,
            fresh: false,
        });
    }
    pub fn send(&mut self, s: &mut Session) {
        if model::chat(&self.store, self.subject).is_none_or(|c| c.closed)
            || model::workspace(&self.store, self.workspace_id()).is_none_or(|w| w.archived)
        {
            return;
        }
        if !self.draft.trim().is_empty() {
            self.submitted_after = Some(
                model::messages(&self.store, self.subject)
                    .last()
                    .map_or(0, |message| message.id),
            );
            self.command(
                s,
                Command::Send {
                    chat_id: self.subject,
                    text: self.draft.clone(),
                    mode: self.mode.clone(),
                },
            );
        }
    }
    pub fn observe_chat_submission(&mut self) {
        let Some(previous) = self.submitted_after else {
            return;
        };
        if let Some(accepted) = model::messages(&self.store, self.subject)
            .iter()
            .find(|m| m.id > previous && matches!(m.role.as_str(), "user" | "You"))
        {
            if self.draft == accepted.body {
                self.draft.clear();
            }
            self.submitted_after = None;
        }
    }
    pub fn edited(&mut self, s: &mut Session, text: String) {
        self.draft = text.clone();
        match self.kind {
            DetailType::Chat => self.command(
                s,
                Command::SaveDraft {
                    chat_id: self.subject,
                    text,
                },
            ),
            DetailType::Comment => self.command(
                s,
                Command::SaveCommentDraft {
                    workspace_id: self.subject,
                    path: self.path.clone(),
                    text,
                },
            ),
            _ => {}
        }
    }
}
impl Panel for Detail {
    fn id(&self) -> &PanelId {
        &self.id
    }
    fn title(&self) -> String {
        match self.kind {
            DetailType::Workspace => model::workspace(&self.store, self.subject)
                .map_or_else(|| "workspace".into(), |w| w.label),
            DetailType::Chat => model::chat(&self.store, self.subject).map_or_else(
                || "chat".into(),
                |c| format!("chat {}: {}", c.ordinal, provider_label(&c.provider)),
            ),
            DetailType::ClosedChats => "closed chats".into(),
            DetailType::Diff => model::change(&self.store, self.subject)
                .map_or_else(|| "diff".into(), |c| filename(&c.path).into()),
            DetailType::Activity => "activity".into(),
            DetailType::Github => "pull request".into(),
            DetailType::Comment => "comment on GitHub".into(),
            DetailType::Settings => "Workshop settings".into(),
            DetailType::AddProject => "add repository".into(),
        }
    }
    fn wish(&self, _: usize) -> (u32, u32) {
        (
            if matches!(self.kind, DetailType::Chat | DetailType::Diff) {
                6
            } else {
                4
            },
            6,
        )
    }
    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }
    fn about(&self) -> String {
        match self.kind {
            DetailType::Chat => "Local Codex or Claude Code conversation with no title. Provider changes in an empty chat apply in place; after a user message they open a new conversation. Close chat stops its agent and preserves the transcript; reopen before sending again. Closing only the panel leaves the chat running.",
            DetailType::ClosedChats => "Closed chats in this workspace. Open a preserved transcript to read it or reopen the chat. Closed chats do not appear in the workspace's active chat list.",
            DetailType::Diff => "Complete file diff at an immutable snapshot. Mark the entire file reviewed or unreviewed. The gutter copies file:line; there are no partial-line review marks. Comments are sent directly to GitHub.",
            DetailType::Workspace => "Workspace hub: current branch and GitHub state, parallel untitled chats, and a local terminal. Review lives in a separate list and joined diff preview. Create PR sends a prompt to the joined, recent or new default chat in this workspace. Archive stops agents and retains the worktree and history; use @archived in the workspace table to find and restore it.",
            DetailType::Comment => "Unsent GitHub comment draft. There is no internal comment thread. If the workspace has no PR, create a draft PR first and preserve this draft until it is published.",
            _ => "Workshop's local agent orchestration controls.",
        }.into()
    }
    fn context_text_columns(&self) -> &'static [&'static str] {
        &["body", "patch"]
    }
    fn verbs(&self) -> Vec<Verb> {
        let wid = self.workspace_id();
        let archived = model::workspace(&self.store, wid).is_some_and(|w| w.archived);
        let go = |id, label, target| {
            Verb::go(
                id,
                label,
                None,
                Nav::Open {
                    from: self.slot,
                    id: target,
                    fresh: false,
                },
            )
        };
        match self.kind {
            DetailType::Workspace => {
                let mut verbs = if archived {
                    vec![
                        Verb::run("workshop.restore_workspace", "restore workspace", None),
                        go("workshop.review", "review changes", Review::id(wid, None)),
                    ]
                } else {
                    vec![
                        Verb::run("workshop.new_chat", "new chat", Some('n')),
                        Verb::run("workshop.ai_review", "AI review", None),
                    ]
                };
                verbs.push(go("workshop.activity", "activity", Self::activity(wid)));
                verbs.push(go("workshop.github", "GitHub", Self::github(wid)));
                if !model::closed_chats(&self.store, wid).is_empty() {
                    verbs.push(go(
                        "workshop.closed_chats",
                        "closed chats",
                        Self::closed_chats(wid),
                    ));
                }
                if !archived {
                    verbs.push(Verb::run("workshop.refresh", "refresh", None));
                    verbs.push(Verb::run(
                        "workshop.archive_workspace",
                        "archive workspace",
                        None,
                    ));
                }
                verbs
            }
            DetailType::Chat => {
                if archived {
                    vec![Verb::run(
                        "workshop.restore_workspace",
                        "restore workspace",
                        None,
                    )]
                } else if model::chat(&self.store, self.subject).is_some_and(|c| c.closed) {
                    vec![Verb::run("workshop.reopen_chat", "reopen chat", None)]
                } else {
                    vec![
                        Verb::run("workshop.new_chat", "new chat", Some('n')),
                        Verb::run(
                            "workshop.mode",
                            if self.mode == "plan" {
                                "switch to work"
                            } else {
                                "switch to plan"
                            },
                            None,
                        ),
                        Verb::run("workshop.close_chat", "close chat", None),
                    ]
                }
            }
            DetailType::ClosedChats => vec![],
            DetailType::Diff => model::change(&self.store, self.subject)
                .map(|c| {
                    vec![
                        Verb::run(
                            "workshop.mark_file",
                            if c.reviewed {
                                "mark file unreviewed"
                            } else {
                                "mark file reviewed"
                            },
                            Some('r'),
                        ),
                        go(
                            "workshop.comment",
                            "comment on GitHub",
                            Self::comment(wid, Some(&c.path)),
                        ),
                    ]
                })
                .unwrap_or_default(),
            DetailType::Activity => vec![],
            DetailType::Github => {
                if archived {
                    return vec![];
                }
                let mut verbs = vec![Verb::run("workshop.refresh", "refresh", None)];
                if let Some(pr) = self.pr() {
                    if pr["state"].as_str() == Some("OPEN") {
                        verbs.push(Verb::run("workshop.fix_errors", "fix errors", None));
                        verbs.push(Verb::run("workshop.merge", "merge", None));
                        verbs.push(if pr["auto_merge"].as_bool() == Some(true) {
                            Verb::run("workshop.cancel_auto_merge", "cancel auto-merge", None)
                        } else {
                            Verb::run("workshop.auto_merge", "enable auto-merge", None)
                        });
                    }
                    verbs.push(go(
                        "workshop.comment",
                        "comment on GitHub",
                        Self::comment(wid, None),
                    ));
                }
                verbs
            }
            DetailType::Comment => {
                if archived
                    || self.comment_operation().is_some_and(|(_, state, _)| {
                        matches!(state.as_str(), "pending" | "running")
                    })
                {
                    vec![]
                } else if self.pr().is_some() {
                    vec![Verb::run(
                        "workshop.post_comment",
                        "post to GitHub",
                        Some('s'),
                    )]
                } else {
                    vec![Verb::run("workshop.draft_pr", "create draft PR", None)]
                }
            }
            DetailType::Settings => vec![
                Verb::run("workshop.save_defaults", "save defaults", Some('s')),
                Verb::run("workshop.login_codex", "sign in Codex", None),
                Verb::run("workshop.login_claude", "sign in Claude Code", None),
            ],
            DetailType::AddProject => vec![Verb::run("workshop.add", "add repository", Some('s'))],
        }
    }
    fn run(&mut self, verb: &str, s: &mut Session) {
        let workspace_id = self.workspace_id();
        let command = match verb {
            "workshop.new_chat" => Some(Command::NewChat { workspace_id }),
            "workshop.close_chat" => Some(Command::CloseChat {
                chat_id: self.subject,
            }),
            "workshop.reopen_chat" => Some(Command::ReopenChat {
                chat_id: self.subject,
            }),
            "workshop.archive_workspace" => Some(Command::ArchiveWorkspace { workspace_id }),
            "workshop.restore_workspace" => Some(Command::RestoreWorkspace { workspace_id }),
            "workshop.ai_review" => Some(Command::AiReview { workspace_id }),
            "workshop.refresh" => Some(Command::Refresh { workspace_id }),
            "workshop.push" => Some(Command::Push { workspace_id }),
            "workshop.create_pr" => {
                if self.pr().is_some() {
                    self.open(s, Self::github(workspace_id));
                    None
                } else {
                    Some(Command::CreatePr {
                        workspace_id,
                        draft: false,
                    })
                }
            }
            "workshop.draft_pr" => Some(Command::CreatePr {
                workspace_id,
                draft: true,
            }),
            "workshop.fix_errors" => Some(Command::FixErrors { workspace_id }),
            "workshop.merge" => Some(Command::Merge {
                workspace_id,
                method: if self.field.is_empty() {
                    "squash".into()
                } else {
                    self.field.clone()
                },
                auto: false,
            }),
            "workshop.auto_merge" => Some(Command::Merge {
                workspace_id,
                method: if self.field.is_empty() {
                    "squash".into()
                } else {
                    self.field.clone()
                },
                auto: true,
            }),
            "workshop.cancel_auto_merge" => Some(Command::CancelAutoMerge { workspace_id }),
            "workshop.post_comment" => {
                self.submitted_after = Some(self.comment_operation().map_or(0, |(id, _, _)| id));
                Some(Command::PostComment {
                    workspace_id,
                    path: self.path.clone(),
                    text: self.draft.clone(),
                })
            }
            "workshop.terminal" => Some(Command::PromoteTerminal { workspace_id }),
            "workshop.apply_model" => {
                self.custom_model = false;
                Some(Command::SetModel {
                    chat_id: self.subject,
                    model: self.field.clone(),
                })
            }
            "workshop.stop" => Some(Command::Stop {
                chat_id: self.subject,
            }),
            "workshop.add" => Some(Command::AddProject {
                path: self.field.clone(),
            }),
            "workshop.save_defaults" => Some(Command::SaveDefaults {
                provider: self.field.clone(),
                model: self.second.clone(),
            }),
            "workshop.login_codex" => Some(Command::Login {
                provider: "codex".into(),
            }),
            "workshop.login_claude" => Some(Command::Login {
                provider: "claude".into(),
            }),
            "workshop.mark_file" => {
                model::change(&self.store, self.subject).map(|c| Command::MarkFile {
                    change_id: c.id,
                    reviewed: !c.reviewed,
                })
            }
            "workshop.mode" => {
                self.mode = if self.mode == "plan" { "work" } else { "plan" }.into();
                s.redraw();
                None
            }
            "workshop.review" => {
                self.open(s, Review::id(workspace_id, None));
                None
            }
            "workshop.send" => {
                self.send(s);
                None
            }
            _ => None,
        };
        if let Some(command) = command {
            self.command(s, command);
        }
    }
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}
struct DetailKind(DetailType);
impl PanelKind for DetailKind {
    fn tag(&self) -> Tag {
        self.0.tag()
    }
    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        let subject = id.arg(0).and_then(|s| s.parse().ok()).unwrap_or(0);
        let store = cx.session().store().clone();
        let (field, second) = if self.0 == DetailType::Settings {
            model::settings(&store)
        } else {
            (String::new(), String::new())
        };
        let draft = if self.0 == DetailType::Chat {
            model::chat(&store, subject)
                .map(|c| c.draft)
                .unwrap_or_default()
        } else if self.0 == DetailType::Comment {
            model::comment_draft(&store, subject, id.arg(1))
        } else {
            String::new()
        };
        Box::new(Detail {
            id: id.clone(),
            slot: 0,
            store,
            kind: self.0,
            subject,
            path: id.arg(1).map(str::to_string),
            field,
            second,
            draft,
            mode: "work".into(),
            mounted: false,
            custom_model: false,
            submitted_after: None,
        })
    }
}
pub fn filename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}
pub fn provider_label(provider: &str) -> &str {
    if provider == "claude" {
        "Claude Code"
    } else {
        "Codex"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel::app::App;
    static APPS: &[&dyn App] = &[&super::super::WORKSHOP];

    #[test]
    fn workspace_archive_tag_overrides_the_default_and_combines_with_project() {
        let session = Session::fake(APPS);
        let source = WorkspaceSource;
        assert_eq!(source.count(session.store(), None), Some(2));
        session
            .store()
            .write(|c| {
                c.execute("UPDATE workshop_workspace SET archived=1 WHERE id=1", [])?;
                Ok(())
            })
            .unwrap();
        assert_eq!(source.keys(session.store(), None), Some(vec![2]));
        assert_eq!(source.present(session.store(), None, &[1, 2]), vec![2]);
        for (filter, expected) in [
            ("@archived", vec![1]),
            ("@archived @project:superapp", vec![1]),
            ("@archived @project:reader", vec![]),
            ("@project:superapp", vec![]),
            ("@not:archived", vec![2]),
            ("(@archived @or @not:archived)", vec![1, 2]),
        ] {
            let parsed = kernel::filter::parse(filter);
            assert!(parsed.errors.is_empty(), "{filter}");
            assert_eq!(
                source.keys(session.store(), parsed.ast.as_ref()),
                Some(expected.clone()),
                "{filter}"
            );
            assert_eq!(
                source
                    .page(session.store(), parsed.ast.as_ref(), 0, 50)
                    .iter()
                    .map(|w| w.id)
                    .collect::<Vec<_>>(),
                expected,
                "{filter}"
            );
        }
        session
            .store()
            .write(|c| {
                c.execute("UPDATE workshop_workspace SET archived=0 WHERE id=1", [])?;
                Ok(())
            })
            .unwrap();
        assert_eq!(source.count(session.store(), None), Some(2));
        let archived = Ast::Tag("archived".into());
        assert_eq!(source.count(session.store(), Some(&archived)), Some(0));
    }

    #[test]
    fn closed_chats_keep_history_but_require_reopening_before_send() {
        let mut session = Session::fake(APPS);
        session.nav(Nav::Open {
            from: 0,
            id: Detail::chat(1),
            fresh: true,
        });
        session.settle();
        let instance = session.panel(session.focus().unwrap()).unwrap();
        let initial_messages = model::messages(session.store(), 1).len();
        assert!(instance
            .borrow()
            .verbs()
            .iter()
            .any(|v| v.id == "workshop.close_chat"));
        session
            .store()
            .write(|c| {
                c.execute("UPDATE workshop_chat SET closed=1 WHERE id=1", [])?;
                Ok(())
            })
            .unwrap();
        {
            let mut borrow = instance.borrow_mut();
            let panel = borrow.as_any().downcast_mut::<Detail>().unwrap();
            assert_eq!(
                panel.verbs().iter().map(|v| v.id).collect::<Vec<_>>(),
                vec!["workshop.reopen_chat"]
            );
            panel.draft = "Do not send from a closed chat".into();
            panel.send(&mut session);
            assert!(panel.submitted_after.is_none());
        }
        session.settle();
        assert_eq!(model::messages(session.store(), 1).len(), initial_messages);
        session.nav(Nav::Open {
            from: 0,
            id: Detail::closed_chats(1),
            fresh: true,
        });
        session.settle();
        assert_eq!(
            session
                .panel(session.focus().unwrap())
                .unwrap()
                .borrow()
                .title(),
            "closed chats"
        );
        assert_eq!(model::closed_chats(session.store(), 1)[0].id, 1);
        assert!(model::chats(session.store(), 1).iter().all(|c| c.id != 1));
    }

    #[test]
    fn archived_workspace_offers_restore_and_history_without_live_actions() {
        let mut session = Session::fake(APPS);
        session.nav(Nav::Open {
            from: 0,
            id: Detail::workspace(1),
            fresh: true,
        });
        session.settle();
        let instance = session.panel(session.focus().unwrap()).unwrap();
        assert!(instance
            .borrow()
            .verbs()
            .iter()
            .any(|v| v.id == "workshop.archive_workspace"));
        session
            .store()
            .write(|c| {
                c.execute("UPDATE workshop_workspace SET archived=1 WHERE id=1", [])?;
                c.execute("UPDATE workshop_chat SET closed=1 WHERE id=1", [])?;
                Ok(())
            })
            .unwrap();
        let verbs = instance
            .borrow()
            .verbs()
            .iter()
            .map(|v| v.id)
            .collect::<Vec<_>>();
        for id in [
            "workshop.restore_workspace",
            "workshop.review",
            "workshop.activity",
            "workshop.github",
            "workshop.closed_chats",
        ] {
            assert!(verbs.contains(&id), "{id}");
        }
        for id in [
            "workshop.archive_workspace",
            "workshop.new_chat",
            "workshop.ai_review",
            "workshop.refresh",
        ] {
            assert!(!verbs.contains(&id), "{id}");
        }
    }

    #[test]
    fn review_filter_cannot_escape_the_pinned_comparison() {
        let session = Session::fake(APPS);
        let snapshot = model::latest_snapshot(session.store(), 1).unwrap();
        let source = Comparison {
            snapshot: snapshot.id,
        };
        assert_eq!(source.count(session.store(), None), Some(4));
        let different = Ast::Op {
            tag: "snapshot".into(),
            op: Op::Eq,
            value: (snapshot.id + 99).to_string(),
        };
        assert_eq!(source.count(session.store(), Some(&different)), Some(0));
        let broaden = Ast::Or(vec![different, Ast::Text("anchors".into())]);
        let rows = source.page(session.store(), Some(&broaden), 0, 50);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].snapshot_id, snapshot.id);
        assert!(rows[0].path.ends_with("anchors.rs"));
    }

    #[test]
    fn pr_routing_can_read_layout_while_the_source_panel_is_borrowed() {
        let mut session = Session::fake(APPS);
        session.nav(Nav::Open {
            from: 0,
            id: Detail::workspace(1),
            fresh: true,
        });
        session.settle();
        let workspace = session.focus().unwrap();
        session.nav(Nav::Open {
            from: workspace,
            id: Detail::chat(1),
            fresh: false,
        });
        session.settle();
        let instance = session.panel(workspace).unwrap();
        let mut borrowed = instance.borrow_mut();
        let _source = borrowed.as_any().downcast_mut::<Detail>().unwrap();
        assert_eq!(super::runtime::joined_chat(&session, workspace, 1), Some(1));
        assert_eq!(super::runtime::joined_chat(&session, workspace, 2), None);
    }

    #[test]
    fn accepted_chat_submission_clears_its_draft_and_preserves_later_edits() {
        let mut session = Session::fake(APPS);
        session.nav(Nav::Open {
            from: 0,
            id: Detail::chat(1),
            fresh: true,
        });
        session.settle();
        let instance = session.panel(session.focus().unwrap()).unwrap();
        for (sent, later, expected) in [
            ("first turn", None, ""),
            ("second turn", Some("next draft"), "next draft"),
        ] {
            {
                let mut borrow = instance.borrow_mut();
                let panel = borrow.as_any().downcast_mut::<Detail>().unwrap();
                panel.draft = sent.into();
                panel.send(&mut session);
                if let Some(later) = later {
                    panel.draft = later.into();
                }
            }
            session.settle();
            let mut borrow = instance.borrow_mut();
            let panel = borrow.as_any().downcast_mut::<Detail>().unwrap();
            panel.observe_chat_submission();
            assert_eq!(panel.draft, expected);
            assert!(panel.submitted_after.is_none());
            assert!(model::messages(&panel.store, 1)
                .iter()
                .any(|message| message.role == "You" && message.body == sent));
        }
    }
}
