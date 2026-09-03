//! Text styles, button actions, and keyboard shortcuts shared by the shell and
//! panel widgets.

use crate::core::{Kind, Role};
use crate::theme;

// ---------------------------------------------------------------------------
// Text styles
// ---------------------------------------------------------------------------

/// Text styles the content grammar needs. Everything monochrome except `Err`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Style {
    N,
    Bold,
    Big,
    T2,
    Muted,
    Label,
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
    /// A file card's `open`: hand the path to the OS.
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
    /// A compose panel's `attach`: the held files become parts of
    /// the draft. The destination half of the hold grammar, exactly as
    /// `… here` is — a compose *is* a destination for a file.
    Attach,
}

// ---------------------------------------------------------------------------
// Accelerators: a control carries its own key, drawn into its label
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
        // the list is previewing to lend back its `reply`.
        Kind::Mailbox { .. } => &[("sync", BtnAct::Refresh)],
        // Drawn right to left from the close button, so this reads
        // "delete archive ×" — the destructive one furthest from the corner,
        // the same order compose puts discard and send in.
        Kind::Message { .. } => &[("archive", BtnAct::Archive), ("delete", BtnAct::Delete)],
        Kind::Compose { .. } => &[("send", BtnAct::Send), ("discard", BtnAct::Discard)],
        // Every verb acts on the thing the panel shows — a card's on its
        // file, a files panel's on its directory. Reads
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
        // A mail part has no path. `open` writes it out and hands it to the
        // OS; the
        // three verbs that act on a place on the disk have no place to act.
        Kind::Attachment { .. } => &[("open", BtnAct::Open)],
        _ => &[],
    }
}

/// The button a files panel gains while something is held:
/// `copy here` or `move here`, naming what will happen. Drawn leftmost —
/// past `delete` — so the contextual verb is the first thing read.
#[must_use]
pub fn hold_btn(op: crate::files::HoldOp) -> (&'static str, BtnAct) {
    (op.here_label(), BtnAct::Here)
}

/// What one panel's header wears now: its kind's buttons, the held item's
/// if one is held and the kind takes it, and — for a files panel — the
/// object verbs only while it is the **end of a chain**: joined under a
/// parent and driving nothing, and no row of it is marked.
///
/// The end of a chain is the thing under the cursor. A row previews the
/// directory's own panel beside the list, and *that* panel wears `copy`,
/// `move`, `delete` for the directory it shows, which the list borrows.
/// A root, an un-joined files panel, or a list that is itself driving a
/// preview is nobody's object right now, so it wears only `new dir`:
/// `~` cannot be deleted, and a chord in a list never hits the directory
/// the list itself shows — it hits what the cursor is on.
///
/// Marked rows take the verbs the same way: two visible controls may not
/// answer to one chord, and with rows marked the verb meant is the set's
/// ([`mark_verbs`]), so the header falls back to its non-object set.
///
/// The shell asks this, never [`head_btns`] alone, so the width, the
/// chords and the drawing agree.
#[must_use]
pub fn head_btns_of(
    kind: &Kind,
    hold: Option<crate::files::HoldOp>,
    object: bool,
    marked: bool,
) -> Vec<(&'static str, BtnAct)> {
    let mut v: Vec<(&'static str, BtnAct)> = match kind {
        Kind::Files { .. } if !object || marked => {
            vec![("new dir", BtnAct::NewDir), ("go to", BtnAct::GoTo)]
        }
        _ => head_btns(kind).to_vec(),
    };
    if let (Kind::Files { .. }, Some(op)) = (kind, hold) {
        v.push(hold_btn(op));
    }
    // The other destination for a held file. A compose wears
    // `attach` only while something is held, exactly as a files panel wears
    // `… here` — the verb names what will happen to what you are carrying,
    // and with empty hands there is nothing to name. Either hold does: you
    // are attaching a copy of the file either way, so a `move` is read as
    // the `copy` it can only be.
    if matches!(kind, Kind::Compose { .. }) && hold.is_some() {
        v.push(("attach", BtnAct::Attach));
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
        // clipboard.
        BtnAct::CopyHold => Some('p'),
        BtnAct::MoveHold => Some('m'),
        BtnAct::Here => Some('h'),
        // The `h` of "attach", not the `a`: a compose edits text, so `cmd+a`
        // is select-all — the same courtesy `copy` gets its `p` for. It is
        // also the letter `… here` wears, which is the point: both are the
        // hold's destination verb.
        BtnAct::Attach => Some('h'),
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

/// Settings' link to the device-sync form. The `y` of "sync": `d`
/// is next door on the same panel, and the letters before it are all either
/// reserved or taken.
pub const ACCEL_DEVICE_SYNC: char = 'y';

/// The verbs of the marks bar: what a list offers on its marked
/// set. The first ones are the row's own verbs, on the set — the inbox
/// files, a files panel copies, moves and deletes; then `all`, which marks
/// every row under the filter, and `clear`, which empties it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkVerb {
    Archive,
    Copy,
    Move,
    Delete,
    All,
    Clear,
}

/// Every verb a bar can wear — the order the buttons take, and what the
/// rules are held against. A list wears the ones [`mark_verbs`] gives it.
pub const MARK_VERBS: &[MarkVerb] = &[
    MarkVerb::Archive,
    MarkVerb::Copy,
    MarkVerb::Move,
    MarkVerb::Delete,
    MarkVerb::All,
    MarkVerb::Clear,
];

/// The bar a list's marks raise, left to right: the row's own verbs on the
/// set, then `all` and `clear`. A batch verb is the single verb on a wider
/// set, so it wears the same letter — which is why the borrowed chords
/// stand down while the bar is up, and why a files panel's own object
/// verbs do (see [`head_btns_of`]).
#[must_use]
pub fn mark_verbs(kind: &Kind) -> &'static [MarkVerb] {
    match kind {
        // Only the inbox archives: everywhere else the mail is already out
        // of it, and a bar may not wear a verb that would do nothing (or,
        // from Sent, something nobody asked for). Delete is the one move
        // every mailbox has — the trash is where mail goes from anywhere.
        Kind::Mailbox { role: Role::Inbox, .. } => &[
            MarkVerb::Archive,
            MarkVerb::Delete,
            MarkVerb::All,
            MarkVerb::Clear,
        ],
        Kind::Mailbox { .. } => &[MarkVerb::Delete, MarkVerb::All, MarkVerb::Clear],
        Kind::Files { .. } => &[
            MarkVerb::Copy,
            MarkVerb::Move,
            MarkVerb::Delete,
            MarkVerb::All,
            MarkVerb::Clear,
        ],
        _ => &[],
    }
}

impl MarkVerb {
    /// The button's text.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            MarkVerb::Archive => "archive",
            MarkVerb::Copy => "copy",
            MarkVerb::Move => "move",
            MarkVerb::Delete => "delete",
            MarkVerb::All => "all",
            MarkVerb::Clear => "clear",
        }
    }

    /// The key it wears: the letter the single-row verb wears, so a batch
    /// is nothing new to learn (`copy` is `p`, not `c`, for the reason a
    /// files panel's own is — see [`btn_accel`]). `clear` wears none —
    /// `esc` is its key, and esc cannot be drawn into a label.
    #[must_use]
    pub fn accel(self) -> Option<char> {
        match self {
            MarkVerb::Archive => Some('a'),
            MarkVerb::Copy => Some('p'),
            MarkVerb::Move => Some('m'),
            MarkVerb::Delete => Some('d'),
            MarkVerb::All => Some('l'),
            MarkVerb::Clear => None,
        }
    }

    /// The verb a chord fires among the ones this bar wears.
    #[must_use]
    pub fn from_accel(verbs: &[MarkVerb], c: char) -> Option<MarkVerb> {
        verbs.iter().copied().find(|v| v.accel() == Some(c))
    }

    /// What the harness addresses the button by — apart from the panel's
    /// own verb of the same name, one column over.
    #[must_use]
    pub fn hit_label(self) -> &'static str {
        match self {
            MarkVerb::Archive => "archive marked",
            MarkVerb::Copy => "copy marked",
            MarkVerb::Move => "move marked",
            MarkVerb::Delete => "delete marked",
            MarkVerb::All => "mark all",
            MarkVerb::Clear => "clear marks",
        }
    }
}

/// The kind a panel **previews** into its joined child: a master/detail list
/// whose cursor walk re-targets the child instead of opening a new panel, and
/// which keeps focus while doing it.
///
/// Such a panel also **borrows** its preview's accelerators — the fifth
/// letter rule. The borrowed mark is never drawn on the borrower: it stays on
/// the previewed panel's own chrome, one column over and in plain sight. The
/// id here is a placeholder; only the variant is ever read.
#[must_use]
pub fn preview_kind(kind: &Kind) -> Option<Kind> {
    match kind {
        Kind::Mailbox { .. } => Some(Kind::Message { id: 0 }),
        Kind::Effects => Some(Kind::Job { id: 0 }),
        // A files list previews into the kind its row names — a directory
        // as a column, a file as a card. The placeholder is the
        // card; the union test covers both children.
        Kind::Files { .. } => Some(Kind::File {
            path: String::new(),
        }),
        _ => None,
    }
}

/// The bar verb a chrome button is the single-row twin of, if any. `archive`
/// on a message and `archive` on a marked set are one verb over one row and
/// over many — which is why they wear the same letter, and why a list may
/// not offer one without the other.
#[must_use]
fn mark_verb_of(act: BtnAct) -> Option<MarkVerb> {
    match act {
        BtnAct::Archive => Some(MarkVerb::Archive),
        BtnAct::Delete => Some(MarkVerb::Delete),
        BtnAct::CopyHold => Some(MarkVerb::Copy),
        BtnAct::MoveHold => Some(MarkVerb::Move),
        _ => None,
    }
}

/// Whether a list driving a preview lends that button's shortcut. A verb the
/// driver's own marks bar does not show is not
/// lent: `archive` borrowed from a sent list's preview would mean, one row
/// at a time, exactly what its bar refuses to do to a set — and one surface
/// per answer is the whole point of the bar and the borrow wearing the same
/// letter. Anything the bar has no opinion about — a message's `reply`, a
/// card's `open` — is lent as before, and a driver with no bar of its own
/// lends everything.
///
/// A withheld chord is withheld from the *drawing* too: the preview draws
/// that letter plain (see the shadow in `draw_panel_full`), so no bold mark
/// promises a chord nobody would answer. The button itself is untouched —
/// it is the mail's, and pressing it still archives.
#[must_use]
pub fn lends(driver: &Kind, act: BtnAct) -> bool {
    let verbs = mark_verbs(driver);
    match mark_verb_of(act) {
        Some(v) if !verbs.is_empty() => verbs.contains(&v),
        _ => true,
    }
}

/// Every accelerator a kind declares — chrome buttons *and* links — as
/// `(key, what it fires)`. One table, so [`tests`] can hold the whole
/// design to its rules rather than trusting discipline. `marks` is the
/// one piece of state the table depends on: a list with marked rows wears
/// its bar's verbs too, and its borrowed chords stand down.
pub fn accels(kind: &Kind, marks: bool) -> Vec<(char, &'static str)> {
    // As an object — the state that wears the most — and with the marks
    // standing its object verbs down where they do.
    let mut v: Vec<(char, &'static str)> = head_btns_of(kind, None, true, marks)
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
    if marks {
        for verb in mark_verbs(kind) {
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

/// A label split around its accelerator: what comes before the letter, the
/// letter, and what follows. A control draws the three as its own labels,
/// the middle one bold — the one place the design spends bold on a key, so
/// the split is one function rather than one per control. A label that does
/// not carry its key is all `pre`, and so draws no mark.
#[must_use]
pub fn split_accel(label: &str, accel: Option<char>) -> (String, String, String) {
    let Some(i) = accel.and_then(|c| accel_idx(label, c)) else {
        return (label.to_string(), String::new(), String::new());
    };
    let mut it = label.chars();
    let pre: String = it.by_ref().take(i).collect();
    let key: String = it.next().into_iter().collect();
    (pre, key, it.collect())
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
    /// A files panel's `new dir` field.
    NewDir,
    /// A files panel's `go to` path field.
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
        Kind::Mailbox { .. } | Kind::Effects => &[FieldId::Filter],
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

    /// Every kind an accelerator could live on — every mailbox role
    /// included: four lists share one chrome, and the rules hold on each.
    fn every_kind() -> Vec<Kind> {
        let mut v = vec![
            Kind::Settings,
            Kind::AddAccount,
            Kind::Problems,
            Kind::Bucket,
            Kind::Help,
            Kind::About,
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
            Kind::Attachment { mail: 1, at: 2 },
        ];
        v.extend(crate::core::ROLES.map(|role| Kind::Mailbox { role, filter: None }));
        v
    }

    /// A files panel holding something shows one more button; the union
    /// has to obey the rules like any header.
    #[test]
    fn the_held_button_collides_with_nothing() {
        for op in [crate::files::HoldOp::Copy, crate::files::HoldOp::Move] {
            let k = Kind::Files { dir: "~".into() };
            let btns = head_btns_of(&k, Some(op), true, false);
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
            assert_eq!(head_btns_of(&card, Some(op), true, false).len(), head_btns(&card).len());
            // Marked rows stand the object verbs down, but never the
            // destination: `… here` is what a hold is for.
            let marked = head_btns_of(&k, Some(op), true, true);
            let labels: Vec<&str> = marked.iter().map(|(l, _)| *l).collect();
            assert_eq!(labels, ["new dir", "go to", op.here_label()]);
        }
    }

    /// The hold's other destination: a compose wears `attach` only
    /// while something is held, and the union still obeys every rule — in
    /// particular it must not claim a text chord, because a compose is
    /// nothing but fields.
    #[test]
    fn a_held_file_gives_compose_its_attach() {
        let k = Kind::Compose { seed: Seed::Blank };
        let bare: Vec<&str> = head_btns_of(&k, None, false, false).iter().map(|(l, _)| *l).collect();
        assert_eq!(bare, ["send", "discard"], "empty hands, nothing to attach");
        for op in [crate::files::HoldOp::Copy, crate::files::HoldOp::Move] {
            let held = head_btns_of(&k, Some(op), false, false);
            let labels: Vec<&str> = held.iter().map(|(l, _)| *l).collect();
            assert_eq!(labels, ["send", "discard", "attach"]);
            let mut seen = Vec::new();
            for (label, act) in &held {
                let c = btn_accel(*act).expect("every compose button has a chord");
                assert!(!RESERVED.contains(&c), "{label}: cmd+{c} is the workspace's");
                assert!(!TEXT_CHORDS.contains(&c), "{label}: cmd+{c} belongs to the fields");
                assert!(!seen.contains(&c), "{label}: cmd+{c} twice");
                assert!(accel_idx(label, c).is_some(), "“{label}” cannot show its key {c}");
                seen.push(c);
            }
        }
        // Nothing else grows a verb from a hold: a card is not a destination.
        for k in every_kind() {
            if matches!(k, Kind::Files { .. } | Kind::Compose { .. }) {
                continue;
            }
            assert_eq!(
                head_btns_of(&k, Some(crate::files::HoldOp::Copy), true, false).len(),
                head_btns(&k).len(),
                "{k:?} grew a button from a hold"
            );
        }
    }

    // Accelerator rules are enforced by tests.

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

    /// The fifth rule: a panel that previews **borrows** its
    /// preview's accelerators, so while a preview is up the user faces the
    /// union of the two sets. That union has to obey the same rules a single
    /// panel does — otherwise a borrowed chord could shadow a reserved one,
    /// or two visible controls could answer to the same key.
    ///
    /// One allowance: a driver and its preview may share a key
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
    /// is driving, wears `new dir` alone. Marked rows take them
    /// too: the verb meant is then the set's.
    #[test]
    fn a_files_panel_wears_its_object_verbs_only_at_the_end_of_a_chain() {
        let k = Kind::Files { dir: "~".into() };
        let worn = |object, marked| -> Vec<&'static str> {
            head_btns_of(&k, None, object, marked).iter().map(|(l, _)| *l).collect()
        };
        assert_eq!(worn(false, false), ["new dir", "go to"]);
        assert_eq!(worn(true, false), ["new dir", "go to", "copy", "move", "delete"]);
        assert_eq!(worn(true, true), ["new dir", "go to"], "the marks have them");
        assert_eq!(worn(false, true), ["new dir", "go to"]);
        let held = head_btns_of(&k, Some(crate::files::HoldOp::Move), false, false);
        assert_eq!(held.last().map(|(l, _)| *l), Some("move here"));
        let card = Kind::File { path: "~/a".into() };
        assert_eq!(head_btns_of(&card, None, false, false).len(), head_btns(&card).len());
    }

    #[test]
    fn a_label_splits_around_its_key() {
        assert_eq!(
            split_accel("archive", Some('a')),
            (String::new(), "a".into(), "rchive".into())
        );
        assert_eq!(
            split_accel("add account", Some('d')),
            ("a".into(), "d".into(), "d account".into())
        );
        // Case-insensitive, like `accel_idx`; a key the label does not
        // carry leaves the label whole.
        assert_eq!(split_accel("Reply", Some('r')).1, "R");
        assert_eq!(
            split_accel("clear", None),
            ("clear".into(), String::new(), String::new())
        );
        assert_eq!(split_accel("clear", Some('z')).0, "clear");
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

    /// Every verb a bar wears is one of the ones the rules above cover,
    /// and no bar wears one twice.
    #[test]
    fn a_bar_wears_verbs_the_table_knows() {
        for k in every_kind() {
            let verbs = mark_verbs(&k);
            let list = matches!(k, Kind::Mailbox { .. } | Kind::Files { .. });
            assert_eq!(!verbs.is_empty(), list, "{k:?}: only a list has a bar");
            let mut seen = Vec::new();
            for v in verbs {
                assert!(MARK_VERBS.contains(v), "{v:?} is not in the table");
                assert!(!seen.contains(v), "{k:?}'s bar wears {v:?} twice");
                seen.push(*v);
            }
            if list {
                assert_eq!(
                    &verbs[verbs.len() - 2..],
                    [MarkVerb::All, MarkVerb::Clear],
                    "all and clear close every bar"
                );
            }
        }
    }

    /// A driver lends the same actions its marks bar supports: the inbox
    /// lends `archive` and
    /// `delete`, every other mailbox lends `delete` alone — so `cmd+a` from
    /// a sent list does not quietly file the conversation the bar and the
    /// swipe both refuse to. What the bar has no opinion about is lent by
    /// everyone.
    #[test]
    fn a_list_lends_only_what_its_bar_would_do() {
        let inbox = Kind::Mailbox { role: Role::Inbox, filter: None };
        assert!(lends(&inbox, BtnAct::Archive));
        assert!(lends(&inbox, BtnAct::Delete));
        for role in [Role::Archive, Role::Sent, Role::Spam] {
            let list = Kind::Mailbox { role, filter: None };
            assert!(!lends(&list, BtnAct::Archive), "{role:?} lends no archive");
            assert!(lends(&list, BtnAct::Delete));
        }
        // A files list's own three are all on its bar, and a verb no bar
        // knows is nobody's to withhold.
        let files = Kind::Files { dir: "~".into() };
        for act in [BtnAct::CopyHold, BtnAct::MoveHold, BtnAct::Delete] {
            assert!(lends(&files, act));
        }
        for list in [&inbox, &files] {
            assert!(lends(list, BtnAct::Open), "a card's open is no bar's verb");
            assert!(lends(list, BtnAct::Refresh));
        }
        // A driver with no bar of its own lends everything it previews.
        assert!(lends(&Kind::Effects, BtnAct::Delete));

        // Every button a mailbox's preview wears is either lent or one the
        // bar deliberately dropped — nothing falls between the two.
        for role in crate::core::ROLES {
            let list = Kind::Mailbox { role, filter: None };
            let child = preview_kind(&list).unwrap();
            for (label, act) in head_btns(&child) {
                let bar = mark_verbs(&list);
                let dropped = mark_verb_of(*act).is_some_and(|v| !bar.contains(&v));
                assert_eq!(!lends(&list, *act), dropped, "{role:?}: “{label}”");
            }
        }
    }

    /// The marks bar's verbs: a list with marks wears the letters
    /// its own rows' verbs wear — the inbox `a`, `d`, `l`, a files panel
    /// `p`, `m`, `d`, `l` — which is the point (a batch is the row's verb
    /// on a set) and the reason the chords it borrows, or wears on its own
    /// object, stand down while the bar is up. Nothing else on the list
    /// collides with them.
    #[test]
    fn a_marked_list_takes_its_verbs_and_the_preview_stands_down() {
        for role in crate::core::ROLES {
            let list = Kind::Mailbox { role, filter: None };
            let bare = accels(&list, false);
            let marked = accels(&list, true);
            assert!(bare.iter().all(|(c, _)| !"adl".contains(*c)));
            for verb in mark_verbs(&list) {
                let Some(c) = verb.accel() else { continue };
                assert!(marked.contains(&(c, verb.label())), "the bar wears {c}");
                assert!(MarkVerb::from_accel(mark_verbs(&list), c) == Some(*verb));
            }
            assert_eq!(MarkVerb::from_accel(mark_verbs(&list), 'z'), None);
            assert_eq!(
                MarkVerb::from_accel(mark_verbs(&list), 'p'),
                None,
                "no copy on a mail list"
            );
            // The bar's `a` is a text chord on a kind with a field. Like the
            // borrowed `a` it stands down while the filter holds the keyboard
            // (the shell's guard), so select-all in a live filter never
            // archives; no other text chord is claimed.
            for (c, what) in &marked {
                assert!(!TEXT_CHORDS.contains(c) || *c == 'a', "cmd+{c} ({what}) is a text chord");
            }
            // The preview's own a and d are exactly the bar's: lent while the
            // set is empty, stood down while it is not.
            let lent = accels(&preview_kind(&list).unwrap(), false);
            for (c, _) in &marked {
                let shared = lent.iter().any(|(l, _)| l == c);
                assert!(
                    shared || *c == 'l' || *c == 's',
                    "cmd+{c} is neither lent nor the bar's own"
                );
            }
        }
        // Only the inbox offers the move out of it; the other three offer
        // the one move they all have.
        let inbox = Kind::Mailbox { role: Role::Inbox, filter: None };
        assert!(mark_verbs(&inbox).contains(&MarkVerb::Archive));
        for role in [Role::Archive, Role::Sent, Role::Spam] {
            let k = Kind::Mailbox { role, filter: None };
            assert!(!mark_verbs(&k).contains(&MarkVerb::Archive), "{role:?} does not archive");
            assert!(mark_verbs(&k).contains(&MarkVerb::Delete));
        }
        // A files panel's are its own object verbs, one column over: the
        // set takes p, m and d, and the header hands them over rather than
        // answering to the same chord twice.
        let files = Kind::Files { dir: "~".into() };
        let object = accels(&files, false);
        let marked = accels(&files, true);
        for verb in mark_verbs(&files) {
            let Some(c) = verb.accel() else { continue };
            assert!(marked.contains(&(c, verb.label())), "the bar wears {c}");
            assert!(MarkVerb::from_accel(mark_verbs(&files), c) == Some(*verb));
        }
        for (c, _) in &marked {
            let own = object.iter().any(|(o, _)| o == c);
            assert!(own || *c == 'l', "cmd+{c} is neither the panel's own nor the bar's");
        }
        assert!(marked.iter().all(|(c, _)| !TEXT_CHORDS.contains(c)));
        // Only a list has marks: the flag changes nothing else.
        for k in every_kind() {
            if !matches!(k, Kind::Mailbox { .. } | Kind::Files { .. }) {
                assert_eq!(accels(&k, true), accels(&k, false));
            }
        }
    }
}
