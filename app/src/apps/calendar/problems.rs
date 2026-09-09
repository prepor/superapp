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
            .rows_sql(
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
            let target = draft
                .map(panels::Editor::id)
                .or_else(|| event.map(panels::Event::id))
                .unwrap_or_else(panels::Timeline::id);
            out.push(
                Problem::new(
                    format!("calendar-change:{id}"),
                    "Calendar change failed",
                    error,
                    "the draft and operation have been kept",
                )
                .with_verbs(vec![
                    Verb::call("calendar.retry", "retry", None, move |s| {
                        if let Err(e) = sync::retry(s, id) {
                            s.notify(e, true);
                        }
                    }),
                    Verb::call("calendar.review", "review", None, move |s| {
                        s.nav(Nav::Open {
                            from: 0,
                            id: target.clone(),
                            fresh: true,
                        })
                    }),
                ]),
            );
        }
        if let Some(error) = s
            .rows_sql(
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
