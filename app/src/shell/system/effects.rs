//! The effect log: everything that left the process, under a filter.
//!
//! A rich table over [`effect::LOG`], which is the queue and the in-memory
//! ring joined in SQL — so one list holds both, and a ring row is narrowed
//! by exactly the same `@kind:` a filed one is. Read-only by construction:
//! the queue is the executor's to move and the ring is the past's. What the
//! marks are for is taking a copy of what one is looking at.

use std::any::Any;
use std::cell::OnceCell;

use kernel::app::registry_for;
use kernel::caps::Clip;
use kernel::effect::{self, Job as JobRow, Registry};
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::richtable::{ListState, SqlSource};
use kernel::session::Session;
use kernel::time::fmt_date;
use makepad_widgets::*;

use crate::shell::widgets::table::{self, RowSpec, TableView};

/// The log's list state: the shared engine over one static source.
type LogList = ListState<&'static SqlSource<JobRow, i64>>;

/// The effect log panel.
pub struct Effects {
    id: PanelId,
    list: LogList,
}

impl Effects {
    pub const TAG: Tag = Tag("effects");

    /// The identity of the one effects list.
    #[must_use]
    pub fn id() -> PanelId {
        PanelId::bare(Self::TAG)
    }

    /// The batch verb: the marked rows' sentences onto the clipboard, and
    /// the set let go. An in-memory effect, so the log gains a row of its
    /// own for it — which is the log demonstrating itself.
    fn copy_marked(&mut self, s: &mut Session) {
        let store = s.store().clone();
        let lines: Vec<String> = self
            .list
            .marks()
            .keys()
            .iter()
            .filter_map(|k| self.list.table().by_key(&store, k))
            .map(|j| job_line(&j))
            .collect();
        self.list.clear_marks();
        if lines.is_empty() {
            return;
        }
        let n = lines.len();
        let world = s.world().clone();
        let said = match world.run(&Clip {
            text: &lines.join("\n"),
            what: "the marked effects",
        }) {
            Ok(()) => (format!("copied {n} · the log has the row"), false),
            Err(e) => (e, true),
        };
        s.notify(said.0, said.1);
        s.redraw();
    }
}

impl Panel for Effects {
    fn id(&self) -> &PanelId {
        &self.id
    }

    fn title(&self) -> String {
        "effects".into()
    }

    fn wish(&self, _cols: usize) -> (u32, u32) {
        (4, 5)
    }

    /// The batch verbs over the marked set, with their count — up with the
    /// first mark and gone with the last. A log changes nothing, so what it
    /// offers is a copy of what is marked, and the way to let the set go.
    fn verbs(&self) -> Vec<Verb> {
        let n = self.list.marks().len();
        if n == 0 {
            return Vec::new();
        }
        vec![
            Verb::run("system.copy", format!("copy {n}"), Some('c')),
            Verb::run("system.unmark", "clear", Some('r')),
        ]
    }

    fn run(&mut self, verb: &str, s: &mut Session) {
        match verb {
            "system.copy" => self.copy_marked(s),
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
pub struct EffectsKind;

impl PanelKind for EffectsKind {
    fn tag(&self) -> Tag {
        Effects::TAG
    }

    fn open(&self, id: &PanelId, _cx: &mut Opening<'_>) -> Box<dyn Panel> {
        Box::new(Effects {
            id: id.clone(),
            // The default filter is typed into the field by the table on
            // its first draw, not folded in here: what narrows the list is
            // on screen, and one `cmd+a` clears it.
            list: ListState::new(&effect::LOG, effect::LOG_PAGE),
        })
    }
}

/// The registry this process decodes payloads with — the same one every
/// world builds, from the same app list. Held per thread because a
/// `Registry` carries closures and only the drawing thread wants one.
fn describe(kind: &str, payload: &str) -> Option<String> {
    thread_local! {
        static REG: OnceCell<Registry> = const { OnceCell::new() };
    }
    REG.with(|r| {
        r.get_or_init(|| registry_for(crate::shell::apps()))
            .describe(kind, payload)
    })
}

/// The sentence a row shows: the effect decoded from its payload and asked
/// to describe itself, or the payload as it stands when this build cannot
/// read the kind — so a row is never nameless, whatever wrote it.
///
/// A ring row carries its own sentence: it never had a payload for the
/// registry to decode, which is what made it in-memory in the first place.
#[must_use]
pub fn job_line(j: &JobRow) -> String {
    j.what
        .clone()
        .or_else(|| describe(&j.kind, &j.payload))
        .unwrap_or_else(|| j.payload.clone())
}

/// What the table needs to know about the log's rows.
pub struct LogRows;

impl RowSpec for LogRows {
    type Src = &'static SqlSource<JobRow, i64>;
    type Panel = Effects;

    fn list(panel: &mut Effects) -> &mut LogList {
        &mut panel.list
    }

    fn row_tpl() -> LiveId {
        live_id!(row)
    }

    fn populate(cx: &mut Cx, row: &WidgetRef, j: &JobRow, selected: bool, marked: bool) {
        let line = table::line(cx, row, selected, marked);
        line.label(cx, ids!(body.kind_lbl)).set_text(cx, &j.kind);
        line.label(cx, ids!(body.entity_lbl))
            .set_text(cx, j.entity.as_deref().unwrap_or(""));
        line.label(cx, ids!(body.status_lbl))
            .set_text(cx, &j.status_line());
        // Filed at, not last touched: the log is a record of what was asked
        // for, in the order it was asked.
        line.label(cx, ids!(body.date_lbl))
            .set_text(cx, &fmt_date(j.created));
        line.label(cx, ids!(body.what_lbl))
            .set_text(cx, &job_line(j));
        let err = line.label(cx, ids!(body.err_lbl));
        err.set_text(cx, j.error.as_deref().unwrap_or(""));
        err.set_visible(cx, j.error.is_some());
    }

    fn label(j: &JobRow) -> String {
        job_line(j)
    }

    fn target(j: &JobRow) -> PanelId {
        super::Job::id(j.id)
    }

    fn default_filter() -> &'static str {
        effect::LOG_DEFAULT
    }

    fn empty_line(_panel: &Self::Panel, filter: &str) -> String {
        match filter.trim() {
            "" => "nothing has left the process yet",
            // The default is not a filter the operator typed, so an empty
            // list under it is not a failed search — it is the ordinary
            // state of an app that has not changed anything out there yet.
            f if f == effect::LOG_DEFAULT => "nothing has been changed out there yet",
            _ => "no effect under this filter",
        }
        .to_string()
    }
}

/// The widget: the shared table, and nothing else.
#[derive(Script, ScriptHook, Widget)]
pub struct EffectsPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    /// The completion box, drawn over the rows after everything else.
    #[live]
    suggest: View,
    #[rust]
    table: TableView<LogRows>,
}

impl Widget for EffectsPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let Self { view, table, .. } = self;
        table.handle_event(cx, event, scope, view);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Self {
            view,
            suggest,
            table,
            ..
        } = self;
        table.draw(cx, scope, walk, view, suggest)
    }
}
