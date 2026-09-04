//! One effect of the log, in full — what the log previews into.
//!
//! The sentence the effect describes itself with reads as the subject, then
//! what went wrong if anything did, then the row as `sqlite3` would show
//! it: the job's own facts, the payload it was filed as, and the answer the
//! world gave back. Everything below the subject is a selectable run; a
//! payload is something one copies into a report.
//!
//! An in-memory effect has fewer of those, and the sections it has no
//! answer for are absent rather than empty: no payload was ever written,
//! and no reply was ever kept.

use std::any::Any;

use kernel::effect::{self, Job as JobRow};
use kernel::layout::SlotId;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag};
use kernel::session::Session;
use kernel::time::fmt_date;
use makepad_widgets::*;

use crate::shell::hosted::PanelProps;

/// One job of the queue, or one effect of the ring.
pub struct Job {
    id: PanelId,
    slot: SlotId,
    job: i64,
}

impl Job {
    pub const TAG: Tag = Tag("job");

    /// The panel over one row of the log. Positive ids are the queue's
    /// rowids; negative ones are the ring's.
    #[must_use]
    pub fn id(job: i64) -> PanelId {
        PanelId::new(Self::TAG, [job.to_string()])
    }

    /// The row a `job` panel names; `None` for any other tag, or for an
    /// argument this build cannot read.
    #[must_use]
    pub fn of(id: &PanelId) -> Option<i64> {
        (id.tag == Self::TAG)
            .then(|| id.arg(0)?.parse().ok())
            .flatten()
    }
}

impl Panel for Job {
    fn id(&self) -> &PanelId {
        &self.id
    }

    fn title(&self) -> String {
        if self.job < 0 {
            "in memory".into()
        } else {
            format!("job #{}", self.job)
        }
    }

    fn wish(&self, _cols: usize) -> (u32, u32) {
        (4, 5)
    }

    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }

    /// A ring id does not survive the process, so the session saves the
    /// list it came out of instead.
    fn persist(&self) -> PanelId {
        if self.job < 0 {
            super::Effects::id()
        } else {
            self.id.clone()
        }
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

/// Its factory.
pub struct JobKind;

impl PanelKind for JobKind {
    fn tag(&self) -> Tag {
        Job::TAG
    }

    fn open(&self, id: &PanelId, _cx: &mut Opening<'_>) -> Box<dyn Panel> {
        Box::new(Job {
            id: id.clone(),
            slot: 0,
            job: Job::of(id).unwrap_or(0),
        })
    }
}

/// The job's own facts, as its panel lists them: what the row says about
/// itself once the effect has had its say.
///
/// A ring row says fewer, and says why: it has no id anyone could look up,
/// no attempts anyone counted, and no promise about repeating, because
/// nothing was ever going to repeat it.
fn facts(j: &JobRow) -> String {
    let reach = if j.writes {
        "changed something out there"
    } else {
        "only asked: nothing out there is different"
    };
    if j.transient() {
        return [
            "kept in memory · never filed",
            &format!("ran {}", fmt_date(j.created)),
            reach,
            "this session only: a restart forgets it",
        ]
        .join("\n");
    }
    let mut lines = vec![
        format!("#{}", j.id),
        format!("filed {}", fmt_date(j.created)),
        format!("last touched {}", fmt_date(j.updated)),
        reach.to_string(),
        format!(
            "{} attempt{}",
            j.attempts,
            if j.attempts == 1 { "" } else { "s" }
        ),
        if j.idempotent {
            "safe to repeat after a crash".to_string()
        } else {
            "not safe to repeat: a crash asks a human".to_string()
        },
    ];
    // Only worth saying while it is still ahead of the job: a closed row's
    // `not_before` is the backoff it never needed again.
    if j.status == "pending" && j.not_before > j.created {
        lines.push(format!("not before {}", fmt_date(j.not_before)));
    }
    lines.join("\n")
}

/// The runs this panel registers, so a payload can be dragged over and
/// copied. A run with nothing in it is addressable by nothing: an empty
/// field still reserves its box, so the text is what has to be asked, and
/// that is what makes a scripted click an assertion.
const RUNS: [(&str, &[LiveId], &[LiveId]); 5] = [
    ("job effect", &[], ids!(what_txt)),
    ("job error", ids!(err_row), ids!(err_row.err_txt)),
    ("job facts", &[], ids!(meta_txt)),
    (
        "job payload",
        ids!(payload_block),
        ids!(payload_block.payload_txt),
    ),
    ("job reply", ids!(reply_block), ids!(reply_block.reply_txt)),
];

/// The widget: the row read fresh on every draw, so a job that finishes
/// while it is open finishes on screen.
#[derive(Script, ScriptHook, Widget)]
pub struct JobPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
}

impl Widget for JobPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let id = Job::of(props.panel.borrow().id()).unwrap_or(0);
        let Some(store) = scope.data.get_mut::<Session>().map(|s| s.store().clone()) else {
            return self.view.draw_walk(cx, scope, walk);
        };
        match effect::job(&store, id) {
            Some(j) => self.fill(cx, &j),
            None => self.gone(cx, id),
        }
        let step = self.view.draw_walk(cx, scope, walk);
        for (label, fold, path) in RUNS {
            if !fold.is_empty() && !self.view.widget(cx, fold).visible() {
                continue;
            }
            let w = self.view.widget(cx, path);
            let r = w.area().rect(cx);
            if r.size.x > 0.0 && !w.as_text_input().text().is_empty() {
                props.hits.add(label, r, MouseCursor::Text, props.slot);
            }
        }
        step
    }
}

impl JobPanel {
    /// One row, in full.
    fn fill(&mut self, cx: &mut Cx2d, j: &JobRow) {
        let v = &self.view;
        v.label(cx, ids!(kind_lbl)).set_text(cx, &j.kind);
        v.label(cx, ids!(entity_lbl))
            .set_text(cx, j.entity.as_deref().unwrap_or(""));
        v.label(cx, ids!(status_lbl)).set_text(cx, &j.status_line());
        v.text_input(cx, ids!(what_txt))
            .set_text(cx, &super::effects::job_line(j));
        v.text_input(cx, ids!(err_row.err_txt))
            .set_text(cx, j.error.as_deref().unwrap_or(""));
        v.widget(cx, ids!(err_row))
            .set_visible(cx, j.error.is_some());
        v.text_input(cx, ids!(meta_txt)).set_text(cx, &facts(j));
        v.text_input(cx, ids!(payload_block.payload_txt))
            .set_text(cx, &j.payload);
        v.widget(cx, ids!(payload_block))
            .set_visible(cx, !j.payload.is_empty());
        let reply = j.reply.as_deref().unwrap_or("");
        v.text_input(cx, ids!(reply_block.reply_txt))
            .set_text(cx, reply);
        v.widget(cx, ids!(reply_block))
            .set_visible(cx, !reply.is_empty());
    }

    /// An effect the log no longer holds. This build cannot invent one, so
    /// the panel says what it is looking at and nothing else — and a
    /// negative id, one the ring dropped, is a different sentence.
    fn gone(&mut self, cx: &mut Cx2d, id: i64) {
        let (title, why) = if id < 0 {
            (
                "an effect kept in memory".to_string(),
                "the ring no longer holds it: it ran in this process, or in one that has since gone",
            )
        } else {
            (format!("job #{id}"), "no such row in the effect queue")
        };
        let v = &self.view;
        v.label(cx, ids!(kind_lbl)).set_text(cx, &title);
        v.label(cx, ids!(entity_lbl)).set_text(cx, "");
        v.label(cx, ids!(status_lbl)).set_text(cx, "gone");
        v.text_input(cx, ids!(what_txt)).set_text(cx, why);
        v.text_input(cx, ids!(meta_txt)).set_text(cx, "");
        for path in [ids!(err_row), ids!(payload_block), ids!(reply_block)] {
            v.widget(cx, path).set_visible(cx, false);
        }
    }
}
