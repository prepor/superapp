use super::{panels, sync};
use kernel::{
    app::{Problem, ProblemSource},
    nav::Nav,
    panel::Verb,
    store::Store,
};
pub struct Problems;
pub static PROBLEMS: Problems = Problems;
impl ProblemSource for Problems {
    fn list(&self, s: &Store) -> Vec<Problem> {
        let mut out = Vec::new();
        for (id, draft, event, error) in s
            .snapshot_rows_sql(
                "calendar failures",
                "failed Calendar operations",
                "SELECT id,draft,event,error FROM calendar_change WHERE state='failed'",
                &[],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, Option<i64>>(1)?,
                        r.get::<_, Option<i64>>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                },
            )
            .iter()
        {
            let id = *id;
            let review = sync::needs_review(error);
            let target = if review {
                event
                    .map(panels::Event::id)
                    .or_else(|| draft.map(panels::Editor::id))
            } else {
                draft
                    .map(panels::Editor::id)
                    .or_else(|| event.map(panels::Event::id))
            }
            .unwrap_or_else(panels::Timeline::id);
            out.push(
                Problem::new(
                    format!("calendar-change:{id}"),
                    "Calendar change failed",
                    error,
                    if review {
                        "review the latest event before trying the change again"
                    } else {
                        "the draft and change have been kept"
                    },
                )
                .with_verbs(vec![
                    Verb::call(
                        if review {
                            "calendar.dismiss"
                        } else {
                            "calendar.retry"
                        },
                        if review { "dismiss error" } else { "retry" },
                        None,
                        move |s| {
                            let plan = if review {
                                sync::dismiss_plan(id)
                            } else {
                                sync::retry_plan(id)
                            };
                            s.act_async(plan, |_, _| {});
                        },
                    ),
                    Verb::call(
                        "calendar.review",
                        if review { "review latest" } else { "review" },
                        None,
                        move |s| {
                            s.nav(Nav::Open {
                                from: 0,
                                id: target.clone(),
                                fresh: true,
                            })
                        },
                    ),
                ]),
            );
        }
        if let Some(error) = s
            .snapshot_rows_sql(
                "calendar sync failure",
                "Calendar synchronization status",
                "SELECT error FROM calendar_sync WHERE error<>''",
                &[],
                |r| r.get::<_, String>(0),
            )
            .first()
        {
            out.push(
                Problem::new(
                    "calendar-sync",
                    "Calendar sync",
                    error,
                    "cached events remain available",
                )
                .with_verbs(vec![Verb::call(
                    "calendar.accounts",
                    "accounts",
                    None,
                    |s| {
                        s.nav(Nav::Open {
                            from: 0,
                            id: kernel::panel::PanelId::bare(kernel::panel::Tag("accounts")),
                            fresh: true,
                        })
                    },
                )]),
            );
        }
        out
    }
}
