//! Standing conditions derived from rows, never stored.
//!
//! Problems are not a table. Fixing the source condition removes the row, and
//! nothing has to remember to clear one.

use std::collections::BTreeSet;

use crate::panel::Verb;
use crate::store::Store;

/// Standing conditions derived from rows, never stored: fixing the source
/// condition removes the row.
pub trait ProblemSource: Sync + Send {
    fn list(&self, store: &Store) -> Vec<Problem>;
}

/// One standing problem, reduced to what a row draws.
pub struct Problem {
    /// Stable while the condition stands (`account:2`, `outbox:7`), so the
    /// shell can tell a new problem from a standing one.
    pub key: String,
    /// What it concerns, in one line.
    pub label: String,
    /// What is wrong, for a human. Drawn in the one colour.
    pub line: String,
    /// The muted line under it: last success, the recipient, the backlog.
    pub detail: String,
    /// The toast on first sight, or none for a source that announces itself
    /// another way.
    pub announce: Option<String>,
    /// The row's controls as data, so the Problems panel draws any source
    /// without a match.
    pub verbs: Vec<Verb>,
}

impl Problem {
    /// A problem with no controls and no toast — the shape most sources
    /// start at.
    #[must_use]
    pub fn new(
        key: impl Into<String>,
        label: impl Into<String>,
        line: impl Into<String>,
        detail: impl Into<String>,
    ) -> Problem {
        Problem {
            key: key.into(),
            label: label.into(),
            line: line.into(),
            detail: detail.into(),
            announce: None,
            verbs: Vec::new(),
        }
    }

    /// The same, with the sentence to toast the first time it stands.
    #[must_use]
    pub fn announcing(mut self, said: impl Into<String>) -> Problem {
        self.announce = Some(said.into());
        self
    }

    /// The same, with the row's controls.
    #[must_use]
    pub fn with_verbs(mut self, verbs: Vec<Verb>) -> Problem {
        self.verbs = verbs;
        self
    }
}

impl std::fmt::Debug for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Problem")
            .field("key", &self.key)
            .field("label", &self.label)
            .field("line", &self.line)
            .field("detail", &self.detail)
            .field("announce", &self.announce)
            .field("verbs", &self.verbs.len())
            .finish()
    }
}

/// Which problems have already been said out loud.
///
/// A problem is announced once per key and forgotten when its source stops
/// listing it, so a relapse is news again. Held by the session, reconciled
/// on every poll and again after an action — the moment the shell wants a
/// failure it just caused to count as announced now rather than at the next
/// poll.
#[derive(Debug, Default)]
pub struct Announced(BTreeSet<String>);

impl Announced {
    #[must_use]
    pub fn new() -> Announced {
        Announced::default()
    }

    /// Reconciles against what stands now: answers the sentences worth
    /// toasting, in the order the sources listed them, and forgets the keys
    /// that cleared.
    pub fn reconcile(&mut self, now: &[Problem]) -> Vec<String> {
        let said = now
            .iter()
            .filter(|p| !self.0.contains(&p.key))
            .filter_map(|p| p.announce.clone())
            .collect();
        self.0 = now.iter().map(|p| p.key.clone()).collect();
        said
    }

    /// Whether this key has been said.
    #[must_use]
    pub fn has(&self, key: &str) -> bool {
        self.0.contains(key)
    }

    /// How many stand, as the mark says it.
    #[must_use]
    pub fn count_line(n: usize) -> String {
        if n == 1 {
            "1 problem".into()
        } else {
            format!("{n} problems")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(key: &str, said: Option<&str>) -> Problem {
        let p = Problem::new(key, key, "wrong", "detail");
        match said {
            Some(s) => p.announcing(s),
            None => p,
        }
    }

    /// Once per key, and again after it has cleared.
    #[test]
    fn a_problem_is_announced_once_and_a_relapse_is_news_again() {
        let mut a = Announced::new();
        assert_eq!(
            a.reconcile(&[p("account:1", Some("sync failed"))]),
            vec!["sync failed".to_string()]
        );
        assert!(a.has("account:1"));
        assert!(
            a.reconcile(&[p("account:1", Some("sync failed"))])
                .is_empty(),
            "standing, not new"
        );

        // It clears…
        assert!(a.reconcile(&[]).is_empty());
        assert!(!a.has("account:1"));
        // …and coming back is news.
        assert_eq!(
            a.reconcile(&[p("account:1", Some("sync failed"))]),
            vec!["sync failed".to_string()]
        );
    }

    /// A source that announces itself another way says nothing here.
    #[test]
    fn a_silent_source_is_still_remembered() {
        let mut a = Announced::new();
        assert!(a.reconcile(&[p("sync", None)]).is_empty());
        assert!(a.has("sync"), "and it counts as standing");
    }

    #[test]
    fn the_mark_counts() {
        assert_eq!(Announced::count_line(1), "1 problem");
        assert_eq!(Announced::count_line(3), "3 problems");
    }

    /// The builders read as one sentence, and the row carries its own
    /// controls.
    #[test]
    fn a_problem_carries_its_own_controls() {
        let p = Problem::new("outbox:7", "send “Hi”", "no route", "to x@y")
            .announcing("send failed: no route")
            .with_verbs(vec![Verb::call("mail.retry", "retry", Some('r'), |_| {})]);
        assert_eq!(p.key, "outbox:7");
        assert_eq!(p.announce.as_deref(), Some("send failed: no route"));
        assert_eq!(p.verbs.len(), 1);
        assert_eq!(p.verbs[0].id, "mail.retry");
        assert!(format!("{p:?}").contains("outbox:7"));
    }
}
