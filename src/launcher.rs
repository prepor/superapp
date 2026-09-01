//! The launcher's search: one query over everything that can be a panel.
//!
//! Results come in two verbs. A hit that is already open somewhere is a
//! **go to** ([`Go::Focus`] — switch workspace, focus it); anything else is
//! an **open** ([`Go::Open`] — a fresh un-joined column on the active
//! workspace). The list is built from the open panels themselves, the root
//! panels, and the mail world (senders as contacts, mails by subject); when
//! real kinds arrive (telegram, rss, kb), each contributes its entries here
//! and this becomes the global search.

use crate::core::{Kind, PanelId, Wm, WS_N};
use crate::data;

/// What activating a hit does.
#[derive(Debug, Clone, PartialEq)]
pub enum Go {
    /// Switch to the workspace holding this panel and focus it.
    Focus(PanelId),
    /// Open a fresh un-joined panel on the active workspace.
    Open(Kind),
}

/// One launcher row.
#[derive(Debug, Clone)]
pub struct Hit {
    /// Primary text: the panel title / subject / person.
    pub label: String,
    /// Muted secondary text: sender, address, kind.
    pub detail: String,
    /// The workspace the panel lives on, when it is already open.
    pub ws: Option<usize>,
    /// What enter does.
    pub go: Go,
}

impl Hit {
    fn matches(&self, extra: &str, tokens: &[String]) -> bool {
        if tokens.is_empty() {
            return true;
        }
        let hay = format!("{} {} {}", self.label, self.detail, extra).to_lowercase();
        tokens.iter().all(|t| hay.contains(t.as_str()))
    }
}

/// A kind's one-word class, part of every haystack — "inbox" finds the inbox,
/// "draft" the composes.
fn kind_word(kind: &Kind) -> &'static str {
    match kind {
        Kind::Help => "help",
        Kind::About => "about",
        Kind::Inbox { .. } => "inbox",
        Kind::Message { .. } => "mail",
        Kind::Contact { .. } => "contact",
        Kind::Compose { .. } => "draft",
    }
}

/// The muted line under/next to a hit: what identifies it beyond the title.
fn kind_detail(kind: &Kind) -> String {
    match kind {
        Kind::Message { id } => data::mail(id)
            .map(|m| m.from_name.to_string())
            .unwrap_or_default(),
        Kind::Contact { email } => (*email).to_string(),
        _ => kind_word(kind).to_string(),
    }
}

/// Everything the query matches, in rank order: open panels (active
/// workspace first), root panels, contacts, mails. A candidate whose kind is
/// already open anywhere becomes a [`Go::Focus`] hit instead of a second
/// copy; an empty query lists just the open panels and the roots — the pure
/// switcher.
#[must_use]
pub fn search(wm: &Wm, query: &str) -> Vec<Hit> {
    let tokens: Vec<String> = query
        .split_whitespace()
        .map(str::to_lowercase)
        .collect();

    // Open panels, visual order, the active workspace's first.
    let mut open: Vec<(usize, PanelId, Kind)> = Vec::new();
    let mut order: Vec<usize> = (0..WS_N).collect();
    order.sort_by_key(|&k| (k != wm.active, k));
    for k in order {
        let w = &wm.wss[k];
        for col in &w.columns {
            for pid in &col.panels {
                if let Some(p) = w.panels.get(pid) {
                    open.push((k, *pid, p.kind.clone()));
                }
            }
        }
    }

    let mut hits: Vec<Hit> = Vec::new();
    let mut seen: Vec<PanelId> = Vec::new();
    let push = |hits: &mut Vec<Hit>, seen: &mut Vec<PanelId>, hit: Hit, extra: &str| {
        if let Go::Focus(pid) = hit.go {
            if seen.contains(&pid) {
                return;
            }
            if hit.matches(extra, &tokens) {
                seen.push(pid);
                hits.push(hit);
            }
        } else if hit.matches(extra, &tokens) {
            hits.push(hit);
        }
    };

    for (k, pid, kind) in &open {
        let extra = format!("{} {}", kind_word(kind), mail_extra(kind));
        push(
            &mut hits,
            &mut seen,
            Hit {
                label: kind.title(),
                detail: kind_detail(kind),
                ws: Some(*k),
                go: Go::Focus(*pid),
            },
            &extra,
        );
    }

    // A candidate that is already open resolves to its panel.
    let placed = |kind: &Kind| -> Option<(usize, PanelId)> {
        open.iter()
            .find(|(_, _, k)| k == kind)
            .map(|(w, pid, _)| (*w, *pid))
    };
    let candidate = |hits: &mut Vec<Hit>, seen: &mut Vec<PanelId>, kind: Kind, extra: &str| {
        let (ws, go) = match placed(&kind) {
            Some((w, pid)) => (Some(w), Go::Focus(pid)),
            None => (None, Go::Open(kind.clone())),
        };
        push(
            hits,
            seen,
            Hit {
                label: kind.title(),
                detail: kind_detail(&kind),
                ws,
                go,
            },
            extra,
        );
    };

    for kind in [Kind::Inbox { filter: None }, Kind::Help, Kind::About] {
        let extra = format!("{} panel", kind_word(&kind));
        candidate(&mut hits, &mut seen, kind, &extra);
    }

    // Contacts and mails only when there is a query — the empty launcher is
    // the switcher, not a directory dump.
    if !tokens.is_empty() {
        let mut seen_senders: Vec<&str> = Vec::new();
        for m in data::mails() {
            if seen_senders.contains(&m.from_email) {
                continue;
            }
            seen_senders.push(m.from_email);
            let extra = format!("contact {}", m.from_email);
            candidate(
                &mut hits,
                &mut seen,
                Kind::Contact {
                    email: m.from_email,
                },
                &extra,
            );
        }
        for m in data::mails() {
            let extra = format!("mail {} {}", m.from_email, m.date);
            candidate(&mut hits, &mut seen, Kind::Message { id: m.id }, &extra);
        }
    }

    hits
}

/// Sender words for an open mail panel's haystack, so "vera" finds the open
/// message the same way it finds the unopened one.
fn mail_extra(kind: &Kind) -> String {
    match kind {
        Kind::Message { id } => data::mail(id)
            .map(|m| format!("{} {}", m.from_email, m.date))
            .unwrap_or_default(),
        Kind::Contact { email } => (*email).to_string(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wm() -> Wm {
        let mut wm = Wm::new();
        wm.open(Kind::Help, None, false);
        let inbox = wm.open(Kind::Inbox { filter: None }, None, false);
        wm.focus = Some(inbox);
        wm
    }

    #[test]
    fn empty_query_is_the_switcher() {
        let wm = wm();
        let hits = search(&wm, "");
        // Open help + inbox, then the two unopened roots; no mails, no people.
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].label, "help");
        assert_eq!(hits[1].label, "inbox");
        assert!(matches!(hits[0].go, Go::Focus(_)));
        assert!(matches!(hits[1].go, Go::Focus(_)));
        assert_eq!(hits[2].label, "about");
        assert!(matches!(hits[2].go, Go::Open(Kind::About)));
    }

    #[test]
    fn mails_and_contacts_match_by_any_word() {
        let wm = wm();
        let hits = search(&wm, "vera");
        // The contact first, then her mail — neither is open.
        assert_eq!(hits[0].label, "Vera Kovac");
        assert!(matches!(
            hits[0].go,
            Go::Open(Kind::Contact { email: "vera@kovac.io" })
        ));
        assert!(hits[1..]
            .iter()
            .any(|h| matches!(h.go, Go::Open(Kind::Message { id: "m1" }))));
        // Every token must match: sender + a subject word.
        let hits = search(&wm, "vera q3");
        assert!(hits
            .iter()
            .all(|h| !matches!(h.go, Go::Open(Kind::Contact { .. }))));
        assert!(hits
            .iter()
            .any(|h| matches!(h.go, Go::Open(Kind::Message { id: "m1" }))));
    }

    #[test]
    fn open_panels_win_over_second_copies() {
        let mut wm = wm();
        // Open m1's message on workspace 3.
        wm.switch(2);
        let msg = wm.open(Kind::Message { id: "m1" }, None, false);
        wm.switch(0);
        let hits = search(&wm, "q3");
        // Exactly one hit for m1: a Focus at workspace 3, not an Open.
        let m1: Vec<&Hit> = hits
            .iter()
            .filter(|h| h.label.contains("Q3 infra"))
            .collect();
        assert_eq!(m1.len(), 1);
        assert_eq!(m1[0].ws, Some(2));
        assert_eq!(m1[0].go, Go::Focus(msg));
    }

    #[test]
    fn active_workspace_panels_lead() {
        let mut wm = wm();
        wm.switch(4);
        wm.open(Kind::About, None, false);
        let hits = search(&wm, "");
        // Workspace 5 is active: its panel sorts before workspace 1's.
        assert_eq!(hits[0].label, "about");
        assert_eq!(hits[0].ws, Some(4));
    }

    #[test]
    fn focus_panel_switches_and_focuses() {
        let mut wm = wm();
        let help = wm.columns[0].panels[0];
        wm.switch(2);
        let msg = wm.open(Kind::Message { id: "m2" }, None, false);
        assert_eq!(wm.focus_panel(help), Some(0));
        assert_eq!(wm.active, 0);
        assert_eq!(wm.focus, Some(help));
        assert_eq!(wm.focus_panel(msg), Some(2));
        assert_eq!(wm.active, 2);
        assert_eq!(wm.focus, Some(msg));
        assert_eq!(wm.focus_panel(0xdead_beef), None);
    }
}
