//! Workshop: device-local repository workspaces, subscription agents and review.
use kernel::app::{App, Capabilities, Env, Mode, Root};
use kernel::panel::PanelKind;
use std::any::Any;
mod bridge;
mod git;
mod harness;
#[cfg(test)]
mod lifecycle_tests;
mod model;
mod panels;
mod runtime;
mod schema;
mod seed;
mod snapshots;
mod terminal;
#[cfg(test)]
mod tests;
mod tools;
mod ui;
mod widgets;
pub use ui::UI;
pub struct Workshop;
pub static WORKSHOP: Workshop = Workshop;
impl App for Workshop {
    fn id(&self) -> &'static str {
        "workshop"
    }
    fn kinds(&self) -> &'static [&'static dyn PanelKind] {
        panels::KINDS
    }
    fn protected_sql_tables(&self) -> &'static [&'static str] {
        schema::PROTECTED
    }
    fn schema(&self) -> Option<&'static kernel::app::Schema> {
        Some(&schema::SCHEMA)
    }
    // Deliberately no replicated(): snapshots, local paths, chats and processes
    // remain on this device under the new opt-in cell-operation sync model.
    fn roots(&self) -> Vec<Root> {
        vec![
            Root::new(
                panels::Projects::id(),
                "projects",
                "workshop agents repositories orchestration",
            ),
            Root::new(
                panels::Workspaces::id(),
                "workspaces",
                "workshop local branches worktrees",
            ),
            Root::new(
                panels::Detail::settings(),
                "Workshop settings",
                "codex claude subscriptions provider models",
            ),
        ]
    }
    fn outside(&self, mode: Mode, env: &Env, caps: &mut Capabilities) {
        caps.insert(Box::new(runtime::RuntimeMode(if env.scripted {
            Mode::Fake
        } else {
            mode
        })));
    }
    fn poll(&self, s: &mut kernel::session::Session) {
        bridge::poll(s);
    }
    fn workers(&self, store: &kernel::store::Store) -> Vec<Box<dyn kernel::app::Worker>> {
        runtime::workers(store)
    }
    fn seed(&self, store: &kernel::store::Store, mode: Mode) -> rusqlite::Result<()> {
        if mode == Mode::Fake {
            seed::seed(store)?;
        }
        Ok(())
    }
    fn tools(&self) -> Vec<kernel::tool::Tool> {
        tools::all()
    }
    fn describe(&self) -> Option<&'static str> {
        Some("Workshop is local to this device and never replicates. workshop_project registers repositories; workshop_workspace stores generated labels, worktree paths, branches, archived state and cached GitHub status (empty unknown, null no PR). workshop_chat has NO titles: ordinal, provider, model, provider session ID, closed state and local draft; workshop_message is its transcript, workshop_run accepted/pending/running/done/failed/stopped requests. workshop_snapshot and workshop_change hold immutable comparison content; workshop_step names shared before/after intervals. Use workshop.steps.diff/read tools rather than writing snapshots. workshop_review records whole-file marks and actor provenance; only human marks contribute to personal changed-line progress. Rebase/rename transfers only unique complete diff matches; edited files reopen entirely. workshop_job tracks accepted external operations and errors; interrupted writes never retry automatically. workshop_comment_draft is unsent GitHub text, not an internal discussion. workshop_setting owns default provider/model. workshop_terminal is local PTY metadata. Prefer workshop tools over raw SQL writes so checks, session ownership and review provenance are enforced. Personal coverage never blocks merging.")
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
