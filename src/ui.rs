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
    Delete,
    Send,
    Discard,
    TryIt,
    /// A file card's `open`: hand the path to the OS (CR-008).
    Open,
    /// A files panel's `new dir`: the one-line field above the rows.
    NewDir,
    /// A files panel's `go to`: the crumbs become a path field, with
    /// completion; enter goes there.
    GoTo,
    /// `copy` / `move`: **hold** the object the panel shows; nothing
    /// touches the disk until a `… here`.
    CopyHold,
    MoveHold,
    /// `copy here` / `move here`: perform the held item into the
    /// directory this files panel shows.
    Here,
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
        // "sync" rather than "refresh": the button kicks the IMAP workers, it
        // does not reload a view. The truer word also frees `r` for the mail
        // the inbox is previewing to lend back its `reply` (CR-005).
        Kind::Inbox { .. } => &[("sync", BtnAct::Refresh)],
        // Drawn right to left from the close button, so this reads
        // "delete archive ×" — the destructive one furthest from the corner,
        // the same order compose puts discard and send in.
        Kind::Message { .. } => &[("archive", BtnAct::Archive), ("delete", BtnAct::Delete)],
        Kind::Compose { .. } => &[("send", BtnAct::Send), ("discard", BtnAct::Discard)],
        // Every verb acts on the thing the panel shows — a card's on its
        // file, a files panel's on its directory (CR-008). Reads
        // "delete move copy new dir ×": the destructive one furthest from
        // the corner, as on a message. A files panel wears the three
        // object verbs only while it is joined under someone's cursor —
        // see [`head_btns_of`]; this is the full set.
        Kind::Files { .. } => &[
            ("new dir", BtnAct::NewDir),
            ("go to", BtnAct::GoTo),
            ("copy", BtnAct::CopyHold),
            ("move", BtnAct::MoveHold),
            ("delete", BtnAct::Delete),
        ],
        Kind::File { .. } => &[
            ("open", BtnAct::Open),
            ("copy", BtnAct::CopyHold),
            ("move", BtnAct::MoveHold),
            ("delete", BtnAct::Delete),
        ],
        _ => &[],
    }
}

/// The button a files panel gains while something is held (CR-008):
/// `copy here` or `move here`, naming what will happen. Drawn leftmost —
/// past `delete` — so the contextual verb is the first thing read.
#[must_use]
pub fn hold_btn(op: crate::files::HoldOp) -> (&'static str, BtnAct) {
    (op.here_label(), BtnAct::Here)
}

/// What one panel's header wears now: its kind's buttons, the held item's
/// if one is held and the kind takes it, and — for a files panel — the
/// object verbs only while it is the **end of a chain**: joined under a
/// parent and driving nothing (CR-008).
///
/// The end of a chain is the thing under the cursor. A row previews the
/// directory's own panel beside the list, and *that* panel wears `copy`,
/// `move`, `delete` for the directory it shows, which the list borrows.
/// A root, an un-joined files panel, or a list that is itself driving a
/// preview is nobody's object right now, so it wears only `new dir`:
/// `~` cannot be deleted, and a chord in a list never hits the directory
/// the list itself shows — it hits what the cursor is on.
///
/// The shell asks this, never [`head_btns`] alone, so the width, the
/// chords and the drawing agree.
#[must_use]
pub fn head_btns_of(
    kind: &Kind,
    hold: Option<crate::files::HoldOp>,
    object: bool,
) -> Vec<(&'static str, BtnAct)> {
    let mut v: Vec<(&'static str, BtnAct)> = match kind {
        Kind::Files { .. } if !object => {
            vec![("new dir", BtnAct::NewDir), ("go to", BtnAct::GoTo)]
        }
        _ => head_btns(kind).to_vec(),
    };
    if let (Kind::Files { .. }, Some(op)) = (kind, hold) {
        v.push(hold_btn(op));
    }
    v
}

/// The key a side-effect button carries.
pub fn btn_accel(act: BtnAct) -> Option<char> {
    match act {
        BtnAct::Refresh => Some('s'),
        BtnAct::Archive => Some('a'),
        BtnAct::Delete => Some('d'),
        BtnAct::Send => Some('s'),
        BtnAct::Discard => Some('d'),
        // The help panel's demo button fires nothing worth a chord.
        BtnAct::TryIt => None,
        BtnAct::Open => Some('o'),
        BtnAct::NewDir => Some('n'),
        BtnAct::GoTo => Some('g'),
        // The `p` of "copy", not the `c`: a card's path is selectable, so
        // cmd+c copies the path — the file clipboard is not the text
        // clipboard (CR-008).
        BtnAct::CopyHold => Some('p'),
        BtnAct::MoveHold => Some('m'),
        BtnAct::Here => Some('h'),
    }
}

/// The message panel's link key: reply, visible on the link itself.
pub const ACCEL_REPLY: char = 'r';

/// The message panel's other link: forward, on its own first letter.
pub const ACCEL_FORWARD: char = 'f';

/// Settings' link to the add-account form. The `d` of "add" rather than the
/// `a`: the account rows are selectable text, so `cmd+a` belongs to them —
/// the same courtesy an editable field gets, for the same reason.
pub const ACCEL_ADD_ACCOUNT: char = 'd';

/// Settings' link to the device-sync form (CR-005). The `y` of "sync": `d`
/// is next door on the same panel, and the letters before it are all either
/// reserved or taken.
pub const ACCEL_DEVICE_SYNC: char = 'y';

/// The verbs of the marks bar (CR-009): what a list offers on its marked
/// set. `archive` and `delete` are the row's own verbs, on the set; `all`
/// marks every row under the filter; `clear` empties the set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkVerb {
    Archive,
    Delete,
    All,
    Clear,
}

/// The bar's order, left to right.
pub const MARK_VERBS: &[MarkVerb] = &[
    MarkVerb::Archive,
    MarkVerb::Delete,
    MarkVerb::All,
    MarkVerb::Clear,
];

/// The row's left edge that toggles its mark, in points — the mark's own
/// place, wider than the bar it draws.
pub const MARK_GUTTER: f64 = 12.0;

impl MarkVerb {
    /// The button's text.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            MarkVerb::Archive => "archive",
            MarkVerb::Delete => "delete",
            MarkVerb::All => "all",
            MarkVerb::Clear => "clear",
        }
    }

    /// The key it wears: the letter the single-row verb wears, so a batch
    /// is nothing new to learn. `clear` wears none — `esc` is its key, and
    /// esc cannot be drawn into a label.
    #[must_use]
    pub fn accel(self) -> Option<char> {
        match self {
            MarkVerb::Archive => Some('a'),
            MarkVerb::Delete => Some('d'),
            MarkVerb::All => Some('l'),
            MarkVerb::Clear => None,
        }
    }

    /// The verb a chord fires, while the bar is up.
    #[must_use]
    pub fn from_accel(c: char) -> Option<MarkVerb> {
        MARK_VERBS.iter().copied().find(|v| v.accel() == Some(c))
    }

    /// What the harness addresses the button by — apart from the message
    /// panel's own `archive`, one column over.
    #[must_use]
    pub fn hit_label(self) -> &'static str {
        match self {
            MarkVerb::Archive => "archive marked",
            MarkVerb::Delete => "delete marked",
            MarkVerb::All => "mark all",
            MarkVerb::Clear => "clear marks",
        }
    }
}

/// The kind a panel **previews** into its joined child: a master/detail list
/// whose cursor walk re-targets the child instead of opening a new panel, and
/// which keeps focus while doing it (CR-005).
///
/// Such a panel also **borrows** its preview's accelerators — the fifth
/// letter rule. The borrowed mark is never drawn on the borrower: it stays on
/// the previewed panel's own chrome, one column over and in plain sight. The
/// id here is a placeholder; only the variant is ever read.
#[must_use]
pub fn preview_kind(kind: &Kind) -> Option<Kind> {
    match kind {
        Kind::Inbox { .. } => Some(Kind::Message { id: 0 }),
        Kind::Effects => Some(Kind::Job { id: 0 }),
        // A files list previews into the kind its row names — a directory
        // as a column, a file as a card (CR-008). The placeholder is the
        // card; the union test covers both children.
        Kind::Files { .. } => Some(Kind::File {
            path: String::new(),
        }),
        _ => None,
    }
}

/// Every accelerator a kind declares — chrome buttons *and* links — as
/// `(key, what it fires)`. One table, so [`tests`] can hold the whole
/// design to its rules rather than trusting discipline. `marks` is the
/// one piece of state the table depends on: a list with marked rows wears
/// its bar's verbs too (CR-009), and its borrowed chords stand down.
pub fn accels(kind: &Kind, marks: bool) -> Vec<(char, &'static str)> {
    let mut v: Vec<(char, &'static str)> = head_btns(kind)
        .iter()
        .filter_map(|(label, act)| btn_accel(*act).map(|c| (c, *label)))
        .collect();
    if matches!(kind, Kind::Message { .. }) {
        v.push((ACCEL_REPLY, "reply"));
        v.push((ACCEL_FORWARD, "forward"));
    }
    if matches!(kind, Kind::Settings) {
        v.push((ACCEL_ADD_ACCOUNT, "add account"));
        v.push((ACCEL_DEVICE_SYNC, "device sync"));
    }
    if marks && matches!(kind, Kind::Inbox { .. }) {
        for verb in MARK_VERBS {
            if let Some(c) = verb.accel() {
                v.push((c, verb.label()));
            }
        }
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
    /// A files panel's `new dir` field (CR-008).
    NewDir,
    /// A files panel's `go to` path field (CR-008).
    Path,
    SetBucketUrl,
    SetBucketKey,
    SetBucketSecret,
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
        Kind::Bucket => &[
            FieldId::SetBucketUrl,
            FieldId::SetBucketKey,
            FieldId::SetBucketSecret,
        ],
        Kind::Compose { .. } => &[FieldId::To, FieldId::Subject, FieldId::Body],
        Kind::Inbox { .. } | Kind::Effects => &[FieldId::Filter],
        Kind::Files { .. } => &[FieldId::Filter, FieldId::NewDir, FieldId::Path],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Seed;

    /// Every kind in every state its accelerators depend on: with and
    /// without marks (only a list has them, but the table has to say so).
    fn every_state() -> Vec<(Kind, bool)> {
        every_kind()
            .into_iter()
            .flat_map(|k| [(k.clone(), false), (k, true)])
            .collect()
    }

    /// Every kind an accelerator could live on.
    fn every_kind() -> Vec<Kind> {
        vec![
            Kind::Settings,
            Kind::AddAccount,
            Kind::Problems,
            Kind::Bucket,
            Kind::Help,
            Kind::About,
            Kind::Inbox { filter: None },
            Kind::Message { id: 1 },
            Kind::Compose { seed: Seed::Blank },
            Kind::Compose {
                seed: Seed::Reply(1),
            },
            Kind::Compose {
                seed: Seed::Forward(1),
            },
            Kind::Contact {
                email: "a@b.c".into(),
            },
            Kind::Effects,
            Kind::Job { id: 1 },
            Kind::Files { dir: "~".into() },
            Kind::File {
                path: "~/notes.md".into(),
            },
        ]
    }

    /// A files panel holding something shows one more button; the union
    /// has to obey the rules like any header.
    #[test]
    fn the_held_button_collides_with_nothing() {
        for op in [crate::files::HoldOp::Copy, crate::files::HoldOp::Move] {
            let k = Kind::Files { dir: "~".into() };
            let btns = head_btns_of(&k, Some(op), true);
            assert_eq!(btns.len(), head_btns(&k).len() + 1);
            let mut seen = Vec::new();
            for (label, act) in &btns {
                let c = btn_accel(*act).expect("every files button has a chord");
                assert!(!RESERVED.contains(&c) && !TEXT_CHORDS.contains(&c), "{label}: cmd+{c}");
                assert!(!seen.contains(&c), "{label}: cmd+{c} twice");
                assert!(accel_idx(label, c).is_some(), "“{label}” cannot show its key {c}");
                seen.push(c);
            }
            // A card never grows the button: it is not a destination.
            let card = Kind::File { path: "~/a".into() };
            assert_eq!(head_btns_of(&card, Some(op), true).len(), head_btns(&card).len());
        }
    }

    // The four rules of CR-003, held by test rather than by discipline.

    #[test]
    fn accelerators_never_claim_a_reserved_chord() {
        for (k, marks) in every_state() {
            for (c, what) in accels(&k, marks) {
                assert!(
                    !RESERVED.contains(&c),
                    "{k:?} claims cmd+{c} for {what}, but the workspace owns it"
                );
            }
        }
    }

    #[test]
    fn accelerators_are_unique_within_a_panel() {
        for (k, marks) in every_state() {
            let mut seen = Vec::new();
            for (c, what) in accels(&k, marks) {
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
        // Without marks: the bar's `a` is the one exception, held to the
        // same guard as a borrowed chord — see the marks test below.
        for k in every_kind() {
            if field_order(&k).is_empty() {
                continue;
            }
            for (c, what) in accels(&k, false) {
                assert!(
                    !TEXT_CHORDS.contains(&c),
                    "{k:?} edits text, so cmd+{c} ({what}) must stay copy/cut/paste/select-all"
                );
            }
        }
    }

    /// The fifth rule (CR-005): a panel that previews **borrows** its
    /// preview's accelerators, so while a preview is up the user faces the
    /// union of the two sets. That union has to obey the same rules a single
    /// panel does — otherwise a borrowed chord could shadow a reserved one,
    /// or two visible controls could answer to the same key.
    ///
    /// One allowance (CR-008): a driver and its preview may share a key
    /// for the **same verb** — a files list and the directory it previews
    /// both wear `copy`. The driver's wins, and the shell draws the
    /// preview's mark plain while it is shadowed, so no visible bold
    /// letter lies. Two *different* verbs on one key stay forbidden.
    #[test]
    fn a_preview_lends_without_colliding_with_its_driver() {
        for k in every_kind() {
            let Some(child) = preview_kind(&k) else {
                continue;
            };
            // A files list previews into a card or another list.
            let children: Vec<Kind> = match k {
                Kind::Files { .. } => vec![child, Kind::Files { dir: "~/x".into() }],
                _ => vec![child],
            };
            for child in children {
                let mut seen: Vec<(char, &'static str)> = accels(&k, false);
                for (c, what) in accels(&child, false) {
                    assert!(
                        !RESERVED.contains(&c),
                        "{k:?} would borrow cmd+{c} for {what}, but the workspace owns it"
                    );
                    if let Some((_, mine)) = seen.iter().find(|(s, _)| *s == c) {
                        assert_eq!(
                            *mine, what,
                            "{k:?} claims cmd+{c} for {mine}, so its preview cannot lend {what}"
                        );
                        continue;
                    }
                    seen.push((c, what));
                }
            }
        }
    }

    /// A files panel wears the object verbs only at the end of a chain —
    /// under a parent's cursor, driving nothing; a root, or a list that
    /// is driving, wears `new dir` alone (CR-008).
    #[test]
    fn a_files_panel_wears_its_object_verbs_only_at_the_end_of_a_chain() {
        let k = Kind::Files { dir: "~".into() };
        let root: Vec<&str> = head_btns_of(&k, None, false).iter().map(|(l, _)| *l).collect();
        assert_eq!(root, ["new dir", "go to"]);
        let joined: Vec<&str> = head_btns_of(&k, None, true).iter().map(|(l, _)| *l).collect();
        assert_eq!(joined, ["new dir", "go to", "copy", "move", "delete"]);
        let held = head_btns_of(&k, Some(crate::files::HoldOp::Move), false);
        assert_eq!(held.last().map(|(l, _)| *l), Some("move here"));
        let card = Kind::File { path: "~/a".into() };
        assert_eq!(head_btns_of(&card, None, false).len(), head_btns(&card).len());
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
            ("reply", ACCEL_REPLY),
            ("forward", ACCEL_FORWARD),
            ("add account", ACCEL_ADD_ACCOUNT),
            ("device sync", ACCEL_DEVICE_SYNC),
        ] {
            assert!(accel_idx(label, c).is_some(), "“{label}” cannot show {c}");
        }
        for verb in MARK_VERBS {
            if let Some(c) = verb.accel() {
                assert!(
                    accel_idx(verb.label(), c).is_some(),
                    "“{}” cannot show its key {c}",
                    verb.label()
                );
            }
        }
    }

    /// The marks bar's verbs (CR-009): a list with marks wears `a`, `d` and
    /// `l` — the first two the very letters its preview lends it, which is
    /// the point (a batch is the row's verb on a set) and the reason the
    /// borrowed chords stand down while the bar is up. Nothing else on the
    /// list collides with them.
    #[test]
    fn a_marked_list_takes_its_verbs_and_the_preview_stands_down() {
        let inbox = Kind::Inbox { filter: None };
        let bare = accels(&inbox, false);
        let marked = accels(&inbox, true);
        assert!(bare.iter().all(|(c, _)| !"adl".contains(*c)));
        for verb in MARK_VERBS {
            let Some(c) = verb.accel() else { continue };
            assert!(marked.contains(&(c, verb.label())), "the bar wears {c}");
            assert!(MarkVerb::from_accel(c) == Some(*verb));
        }
        assert_eq!(MarkVerb::from_accel('z'), None);
        // The bar's `a` is a text chord on a kind with a field. Like the
        // borrowed `a` it stands down while the filter holds the keyboard
        // (the shell's guard), so select-all in a live filter never
        // archives; no other text chord is claimed.
        for (c, what) in &marked {
            assert!(!TEXT_CHORDS.contains(c) || *c == 'a', "cmd+{c} ({what}) is a text chord");
        }
        // The preview's own a and d are exactly the bar's: lent while the
        // set is empty, stood down while it is not.
        let lent = accels(&preview_kind(&inbox).unwrap(), false);
        for (c, _) in &marked {
            let shared = lent.iter().any(|(l, _)| l == c);
            assert!(shared || *c == 'l' || *c == 's', "cmd+{c} is neither lent nor the bar's own");
        }
        // Only a list has marks: the flag changes nothing else.
        for k in every_kind() {
            if !matches!(k, Kind::Inbox { .. }) {
                assert_eq!(accels(&k, true), accels(&k, false));
            }
        }
    }
}
