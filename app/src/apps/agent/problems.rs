//! What the agent can be standing wrong: a gateway that will not open.
//!
//! Derived from rows, never stored. The condition is the newest run of any
//! chat having failed with the gateway's own word in front of its sentence
//! — no token, the old S3 hash where the token's value should be, a bucket
//! whose host names no account, a 401. The next run that answers clears it,
//! because a problem here is a reading of the rows and not a row of its
//! own.
//!
//! One row, not one per chat: there is one gateway, and a person who has
//! not run `--r2-login` has not run it for every chat at once.

use kernel::app::{Problem, ProblemSource};
use kernel::store::{Store, Q};
use kernel::time::fmt_date;

use super::model::{FAILED, UNTITLED};

/// The word a gateway failure wears, which is what the run's `error` is
/// keyed on and what the row's line has taken off it.
const PREFIX: &str = "gateway:";

static Q_LATEST: Q = Q {
    id: "latest run overall",
    sql: "SELECT r.status, COALESCE(r.error, ''), COALESCE(c.title, ''), r.started
          FROM agent_run r LEFT JOIN agent_chat c ON c.id = r.chat
          ORDER BY r.id DESC LIMIT 1",
    describe: "the newest round of the agent in any chat, and which chat it was",
};

/// A gateway that will not open.
pub struct GatewayProblems;

impl ProblemSource for GatewayProblems {
    fn list(&self, store: &Store) -> Vec<Problem> {
        store
            .rows(&Q_LATEST, &[], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, f64>(3)?,
                ))
            })
            .iter()
            .filter_map(|(status, error, title, started)| {
                if status != FAILED {
                    return None;
                }
                let line = error.strip_prefix(PREFIX)?.trim().to_string();
                let title = if title.is_empty() {
                    UNTITLED.to_string()
                } else {
                    title.clone()
                };
                Some(
                    Problem::new(
                        "gateway",
                        "gateway",
                        line.clone(),
                        format!("“{title}”, {}", fmt_date(*started)),
                    )
                    .announcing(format!("the gateway did not answer: {line}")),
                )
            })
            .collect()
    }
}
