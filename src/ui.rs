//! The shared content vocabulary: text styles and per-kind accelerators.
//!
//! This is the seed of the high-level component library (stelaxis's
//! philosophy: name the *meaning* — style, accelerator — never the pixels).
//! Panels are retained widget trees now (CR-002), so the drawing lives in
//! [`crate::panels`]; what stays here is the vocabulary both the shell's own
//! chrome and those widgets have to agree on.

use crate::core::Kind;
use crate::theme;

// ---------------------------------------------------------------------------
// Text styles
// ---------------------------------------------------------------------------

/// Text styles the content grammar needs. Everything monochrome except `Err`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Style {
    /// Body text.
    N,
    /// Fake-bold body text (unread rows).
    Bold,
    /// A bigger fake-bold heading (the contact name).
    Big,
    /// Secondary.
    T2,
    /// Muted.
    Muted,
    /// Uppercase small tracked label.
    Label,
    /// The one colour: errors.
    Err,
}

impl Style {
    pub fn color(self) -> theme::Rgba {
        match self {
            Style::N | Style::Bold | Style::Big => theme::INK,
            Style::T2 => theme::TEXT2,
            Style::Muted => theme::MUTED,
            Style::Label => theme::TEXT2,
            Style::Err => theme::ERR,
        }
    }
    pub fn size(self) -> f64 {
        match self {
            Style::Label => theme::LABEL_SIZE,
            Style::Big => theme::FONT_SIZE * 1.25,
            _ => theme::FONT_SIZE,
        }
    }
}

/// Side-effect buttons (never navigation).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BtnAct {
    Refresh,
    Archive,
    Send,
    Discard,
    TryIt,
}

// ---------------------------------------------------------------------------
// Accelerators (CR-003): a control carries its own key, drawn into its label
// ---------------------------------------------------------------------------

/// Cmd chords the workspace owns on every panel. An accelerator may never
/// claim one — these mean the same thing everywhere, forever.
pub const RESERVED: &[char] = &['w', 'z', 'u', 'i', 't', 'q'];

/// Cmd chords an editable field owns: copy, cut, paste, select-all. A kind
/// that edits text yields them, so `cmd+a` still selects all in a compose
/// body — see [`accels`].
pub const TEXT_CHORDS: &[char] = &['c', 'v', 'x', 'a'];

/// The side-effect buttons a kind's chrome carries, right to left from the
/// close button. Shared by the header draw and the accelerator table so the
/// two can never disagree.
pub fn head_btns(kind: &Kind) -> &'static [(&'static str, BtnAct)] {
    match kind {
        Kind::Inbox { .. } => &[("refresh", BtnAct::Refresh)],
        Kind::Message { .. } => &[("archive", BtnAct::Archive)],
        Kind::Compose { .. } => &[("send", BtnAct::Send), ("discard", BtnAct::Discard)],
        _ => &[],
    }
}

/// The key a side-effect button carries.
pub fn btn_accel(act: BtnAct) -> Option<char> {
    match act {
        BtnAct::Refresh => Some('r'),
        BtnAct::Archive => Some('a'),
        BtnAct::Send => Some('s'),
        BtnAct::Discard => Some('d'),
        // The help panel's demo button fires nothing worth a chord.
        BtnAct::TryIt => None,
    }
}

/// The message panel's link keys — the walk that used to be `j`/`k`, and
/// reply, now visible on the links themselves.
pub const ACCEL_NEWER: char = 'n';
pub const ACCEL_OLDER: char = 'o';
pub const ACCEL_REPLY: char = 'r';

/// Settings' link to the add-account form. The `d` of "add" rather than the
/// `a`: the account rows are selectable text, so `cmd+a` belongs to them —
/// the same courtesy an editable field gets, for the same reason.
pub const ACCEL_ADD_ACCOUNT: char = 'd';

/// Every accelerator a kind declares — chrome buttons *and* links — as
/// `(key, what it fires)`. One table, so [`tests`] can hold the whole
/// design to its rules rather than trusting discipline.
pub fn accels(kind: &Kind) -> Vec<(char, &'static str)> {
    let mut v: Vec<(char, &'static str)> = head_btns(kind)
        .iter()
        .filter_map(|(label, act)| btn_accel(*act).map(|c| (c, *label)))
        .collect();
    if matches!(kind, Kind::Message { .. }) {
        v.push((ACCEL_NEWER, "newer"));
        v.push((ACCEL_OLDER, "older"));
        v.push((ACCEL_REPLY, "reply"));
    }
    if matches!(kind, Kind::Settings) {
        v.push((ACCEL_ADD_ACCOUNT, "add account"));
    }
    v
}

/// Where `accel` sits in `label` — the index the bold mark draws at. `None`
/// when the label does not contain its own key, which the tests forbid.
pub fn accel_idx(label: &str, accel: char) -> Option<usize> {
    label.chars().position(|c| c.eq_ignore_ascii_case(&accel))
}

// ---------------------------------------------------------------------------
// Text helpers
// ---------------------------------------------------------------------------

/// `s` cut to `max` chars, with an ellipsis when it did not fit.
pub fn trunc(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

// ---------------------------------------------------------------------------
// Which kinds edit text
// ---------------------------------------------------------------------------

/// Text fields a kind's form carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldId {
    Filter,
    To,
    Subject,
    Body,
    /// Settings form: address, app password, IMAP host, SMTP host.
    SetEmail,
    SetPass,
    SetImap,
    SetSmtp,
}

/// The fields a kind's form carries, in walk order. The widgets own the walk
/// itself now; this stays as the "does this kind edit text?" oracle the
/// accelerator rules are held to (see [`tests`]).
#[must_use]
pub fn field_order(kind: &Kind) -> &'static [FieldId] {
    match kind {
        Kind::AddAccount => &[
            FieldId::SetEmail,
            FieldId::SetPass,
            FieldId::SetImap,
            FieldId::SetSmtp,
        ],
        Kind::Compose { .. } => &[FieldId::To, FieldId::Subject, FieldId::Body],
        Kind::Inbox { .. } => &[FieldId::Filter],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every kind an accelerator could live on.
    fn every_kind() -> Vec<Kind> {
        vec![
            Kind::Settings,
            Kind::AddAccount,
            Kind::Help,
            Kind::About,
            Kind::Inbox { filter: None },
            Kind::Message { id: 1 },
            Kind::Compose { re: 0 },
            Kind::Contact {
                email: "a@b.c".into(),
            },
        ]
    }

    // The four rules of CR-003, held by test rather than by discipline.

    #[test]
    fn accelerators_never_claim_a_reserved_chord() {
        for k in every_kind() {
            for (c, what) in accels(&k) {
                assert!(
                    !RESERVED.contains(&c),
                    "{k:?} claims cmd+{c} for {what}, but the workspace owns it"
                );
            }
        }
    }

    #[test]
    fn accelerators_are_unique_within_a_panel() {
        for k in every_kind() {
            let mut seen = Vec::new();
            for (c, what) in accels(&k) {
                assert!(
                    !seen.contains(&c),
                    "{k:?} claims cmd+{c} twice (second: {what})"
                );
                seen.push(c);
            }
        }
    }

    #[test]
    fn kinds_that_edit_text_yield_the_text_chords() {
        for k in every_kind() {
            if field_order(&k).is_empty() {
                continue;
            }
            for (c, what) in accels(&k) {
                assert!(
                    !TEXT_CHORDS.contains(&c),
                    "{k:?} edits text, so cmd+{c} ({what}) must stay copy/cut/paste/select-all"
                );
            }
        }
    }

    #[test]
    fn every_accelerator_is_drawable_in_its_label() {
        // The mark is a bold char *inside* the label; a key the label does
        // not contain could never be shown, which defeats the point.
        for k in every_kind() {
            for (label, act) in head_btns(&k) {
                let Some(c) = btn_accel(*act) else { continue };
                assert!(
                    accel_idx(label, c).is_some(),
                    "{k:?}: “{label}” cannot show its key {c}"
                );
            }
        }
        for (label, c) in [
            ("newer", ACCEL_NEWER),
            ("older", ACCEL_OLDER),
            ("reply", ACCEL_REPLY),
            ("add account", ACCEL_ADD_ACCOUNT),
        ] {
            assert!(accel_idx(label, c).is_some(), "“{label}” cannot show {c}");
        }
    }
}
