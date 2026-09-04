//! What mail can be standing wrong: a send whose last attempt failed.
//!
//! Derived from rows, never stored. A send is a problem from its *first*
//! failed attempt, not from its sixth: a row the executor is still backing
//! off on has been wrong for as long as its error says. Fixing the condition
//! — retrying it, or taking the letter back — removes the row.

use kernel::app::{Problem, ProblemSource};
use kernel::effect::MAX_ATTEMPTS;
use kernel::nav::Nav;
use kernel::panel::Verb;
use kernel::session::Action;
use kernel::store::{Q, Store};
use kernel::time::fmt_date;

use super::effects::{outbox_entity, Sent};
use super::model::{self, MailId, Seed};
use super::panels::Compose;

/// The live job is the newest non-obsolete submit for the row (a retry
/// obsoletes the old one).
static Q_FAILING_SENDS: Q = Q {
    id: "failing_sends",
    sql: "SELECT o.id, o.status, COALESCE(o.error, e.error, 'send failed'),
                 COALESCE(d.subject, ''), COALESCE(d.to_addr, ''),
                 d.re_message, d.fwd_message, COALESCE(e.attempts, 0),
                 COALESCE(e.not_before, 0), COALESCE(e.status, '')
          FROM outbox o
          LEFT JOIN draft d ON d.panel = o.id
          LEFT JOIN effect e ON e.id = (SELECT MAX(id) FROM effect
                                        WHERE kind = 'submit' AND status != 'obsolete'
                                          AND payload ->> 'outbox' = o.id)
          WHERE o.status = 'failed'
             OR (o.status = 'sending' AND e.status IN ('pending', 'processing')
                 AND e.error IS NOT NULL)
          ORDER BY o.id",
    describe: "every send whose last attempt failed — retrying, trying now, or given up",
};

/// One row of the query, in the order the SQL lists them.
type SendRow = (
    i64,
    String,
    String,
    String,
    String,
    Option<MailId>,
    Option<MailId>,
    i64,
    f64,
    String,
);

/// The failing sends.
pub struct FailingSends;

impl ProblemSource for FailingSends {
    fn list(&self, store: &Store) -> Vec<Problem> {
        let rows = store.rows(&Q_FAILING_SENDS, &[], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
                r.get(9)?,
            ))
        });
        rows.iter().map(row_problem).collect()
    }
}

fn row_problem(row: &SendRow) -> Problem {
    let (outbox, status, error, subject, to, re, fwd, attempts, next, job) = row;
    let subject = if subject.is_empty() {
        "(no subject)".to_string()
    } else {
        subject.clone()
    };
    let given_up = status == "failed";
    let to = if to.is_empty() {
        "no recipient".to_string()
    } else {
        format!("to {to}")
    };
    let detail = if given_up {
        format!("{to} — gave up after {attempts} attempts")
    } else if job == "processing" {
        // Mid-attempt: the row stays, so a slow call never blinks the mark
        // off and announces the same failure twice.
        format!("{to} — attempt {attempts} of {MAX_ATTEMPTS}, trying now")
    } else {
        format!(
            "{to} — attempt {attempts} of {MAX_ATTEMPTS}, next at {}",
            fmt_date(*next)
        )
    };
    let seed = match (*re, *fwd) {
        (Some(id), _) => Seed::Reply(id),
        (None, Some(id)) => Seed::Forward(id),
        (None, None) => Seed::Blank,
    };
    Problem::new(
        outbox_entity(*outbox),
        format!("send “{subject}”"),
        error.clone(),
        detail,
    )
    .announcing(format!("“{subject}” could not be sent: {error}"))
    .with_verbs(verbs(*outbox, &subject, seed))
}

/// The row's controls, as data: a button that files the send again, and a
/// link back to a sheet on the same source.
///
/// The button is a [`VerbAct::Call`](kernel::panel::VerbAct::Call): a
/// problem belongs to no panel, so there is no instance for a `run` to
/// reach and the closure is the whole behaviour.
///
/// Neither wears a letter. A bar's letters are unique within one bar, and a
/// problems panel draws as many of these rows as there are failures.
fn verbs(outbox: i64, subject: &str, seed: Seed) -> Vec<Verb> {
    let said = subject.to_string();
    vec![
        Verb::call("mail.retry", "retry", None, move |s| {
            let delay = model::send_delay();
            let after = s.now() + delay;
            s.act(
                Action::writing("send", format!("retry “{said}”"), move |tx| {
                    model::file_send_tx(tx, outbox, after)
                })
                .about(outbox_entity(outbox))
                .claiming(vec![Box::new(Sent {
                    slot: outbox,
                    delay,
                })]),
            );
        }),
        // A problem row sits in no slot of its own, so the open has nowhere
        // to join to and lands in a column of its own. The prototype's
        // reopen starts a fresh sheet on the same source: carrying the failed
        // draft's text across needs the slot the link lands in, which a `Nav`
        // cannot know, and the port brings `reopen_send_tx` with it.
        Verb::go(
            "mail.reopen",
            "reopen",
            None,
            Nav::Open {
                from: 0,
                id: Compose::id(seed),
                fresh: true,
            },
        ),
    ]
}
