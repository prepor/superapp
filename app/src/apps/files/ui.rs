//! Files' Makepad half: its templates, and the tag each of them draws.
//!
//! One `script_mod!` block, its stage ids all prefixed `files_`, which is
//! what keeps two apps apart in one script virtual machine. The chassis is
//! the shell's — the table's filter and row twins, the completion box, the
//! file card, the field, the link, the rule — so an app declares what is its
//! own and nothing else: a listing's three columns, the crumb line, and the
//! two fields that stand in it.

use kernel::panel::Tag;
use kernel::scene::Scene;
use makepad_widgets::*;

use crate::shell::app_ui::{AppUi, Setup};

use super::widgets::{CardPanel, DirPanel};
use super::{Card, Dir};

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    // ---- the listing --------------------------------------------------------

    /** One entry of a directory: the name (a directory wears its slash),
        then the size and the date at the right, on the columns the header
        above them draws.

        The body is declared once and hung in each of the row's four twins,
        so the cursor wash and the mark bar stay the shell's. */
    mod.widgets.FilesDirBody = View {
        width: Fill, height: Fit
        flow: Right
        align: Align{y: 0.5}
        // The name rides a Fill View whose flow is Down: a Fill label on a
        // Right flow's main axis defer-walks.
        View {
            width: Fill, height: Fit
            flow: Down
            name_lbl := mod.widgets.SLabel {
                padding: 0
                width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
            }
        }
        View { width: 10, height: 1 }
        View {
            width: 60, height: Fit
            align: Align{x: 1.0}
            size_lbl := mod.widgets.SLabel {
                padding: 0
                width: Fit, text: "", draw_text +: { color: #5a5a5a }
            }
        }
        View { width: 12, height: 1 }
        date_lbl := mod.widgets.SLabel {
            padding: 0
            width: Fit, text: "", draw_text +: { color: #909090 }
        }
    }

    /** A row of a listing: the four twins, and the hairline under them. */
    mod.widgets.FilesDirRow = mod.widgets.TblRow {
        line          := mod.widgets.TblLine        { body := mod.widgets.FilesDirBody {} }
        line_sel      := mod.widgets.TblLineSel     { body := mod.widgets.FilesDirBody {} }
        line_mark     := mod.widgets.TblLineMark    { body := mod.widgets.FilesDirBody {} }
        line_mark_sel := mod.widgets.TblLineMarkSel { body := mod.widgets.FilesDirBody {} }
        mod.widgets.TblHairline {}
    }

    /** A directory as a column: where the panel stands as crumbs, the
        filter, the two fields that stand in for the crumb line while they
        are up, the header over the rows, the status line under them.

        A rich table like any other — what files adds is the row body above,
        the four functions beside it, and the chrome around the list. */
    mod.widgets.FilesDirPanel = set_type_default() do #(DirPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        // No flow spacing: the gaps are explicit, so a rule sits the same
        // 3 pt under the header as under every row.
        spacing: 0

        // Every ancestor a dotted link — it replaces the panel with that
        // directory, in place — and the directory itself plain, last.
        crumbs := View {
            width: Fill, height: Fit
            flow: Right
            align: Align{y: 0.5}
            padding: Inset{left: 8, right: 8, bottom: 8}
            c0 := mod.widgets.SLink {}
            s0 := mod.widgets.SLabel { padding: 0, text: " / ", draw_text +: { color: #909090 } }
            c1 := mod.widgets.SLink {}
            s1 := mod.widgets.SLabel { padding: 0, text: " / ", draw_text +: { color: #909090 } }
            c2 := mod.widgets.SLink {}
            s2 := mod.widgets.SLabel { padding: 0, text: " / ", draw_text +: { color: #909090 } }
            c3 := mod.widgets.SLink {}
            s3 := mod.widgets.SLabel { padding: 0, text: " / ", draw_text +: { color: #909090 } }
            here_lbl := mod.widgets.SLabel { padding: 0, text: "" }
        }
        /* `go to`: the crumbs as a field — the path, completed segment by
           segment; enter goes there, esc puts the crumbs back. */
        path_row := View {
            visible: false
            width: Fill, height: Fit
            flow: Right
            align: Align{y: 0.5}
            margin: Inset{bottom: 6}
            mod.widgets.SSection { width: 82, text: "GO TO" }
            path_input := mod.widgets.SField {
                empty_text: "~/ or /"
                return_key_type: ReturnKeyType.Go
                autocapitalize: AutoCapitalize.None
                autocorrect: AutoCorrect.Disabled
            }
        }
        filter_input := mod.widgets.TblFilter {}
        filter_err_lbl := mod.widgets.TblErr {}
        /* `new dir`: up while the verb asked for it; enter creates, esc
           puts it away. */
        newdir_row := View {
            visible: false
            width: Fill, height: Fit
            flow: Right
            align: Align{y: 0.5}
            margin: Inset{top: 6}
            mod.widgets.SSection { width: 82, text: "NEW DIR" }
            newdir_input := mod.widgets.SField {
                empty_text: "name"
                return_key_type: ReturnKeyType.Done
                autocapitalize: AutoCapitalize.None
                autocorrect: AutoCorrect.Disabled
            }
        }
        View { width: Fill, height: 6 }
        // Header cells for the three columns a row draws.
        View {
            width: Fill, height: Fit
            padding: Inset{left: 8, right: 8, top: 0, bottom: 3}
            View {
                width: Fill, height: Fit
                mod.widgets.SSection { padding: 0, text: "NAME" }
            }
            View { width: 10, height: 1 }
            View {
                width: 60, height: Fit
                align: Align{x: 1.0}
                mod.widgets.SSection { padding: 0, width: Fit, text: "SIZE" }
            }
            View { width: 12, height: 1 }
            mod.widgets.SSection { padding: 0, width: Fit, text: "MODIFIED" }
        }
        mod.widgets.TblHeadRule {}
        empty_lbl := mod.widgets.TblEmpty {}
        list := PortalList {
            width: Fill, height: Fill
            flow: Down
            // A row that scrolls out is kept for the next one that scrolls
            // in, rather than built again.
            reuse_items: true
            row := mod.widgets.FilesDirRow {}
            caption := mod.widgets.TblCaption {}
            band_rule := mod.widgets.TblBandRule {}
        }
        // A refused verb, a directory that is gone: the one colour errors
        // get.
        status_lbl := mod.widgets.SLabel {
            visible: false
            padding: 0
            margin: Inset{left: 8, top: 6}
            text: "", draw_text +: { color: #a01500 }
        }
        suggest: mod.widgets.TblSuggest {}
        // The path field's own box, hung under that field.
        suggest_path: mod.widgets.TblSuggest {}
    }

    // ---- the card -----------------------------------------------------------

    /** One file, shown: the shell's own card, and under it the line a
        refused verb leaves.

        Everything the card draws — the name, what it is, when it changed,
        the path, the preview, the picture — is filled from the instance
        through `card::fill`, which is what lets one card draw a file on a
        disk and a part of a letter without knowing there is a difference. */
    mod.widgets.FilesCardPanel = set_type_default() do #(CardPanel::register_widget(vm)) {
        ..mod.widgets.CardFile
        status_lbl := mod.widgets.SLabel {
            visible: false
            padding: 0
            margin: Inset{top: 6}
            text: "", draw_text +: { color: #a01500 }
        }
    }
}

/// Files' Makepad half.
pub struct Ui;

/// The one in this build.
pub static UI: Ui = Ui;

impl AppUi for Ui {
    fn script_mod(&self, vm: &mut ScriptVm) -> ScriptValue {
        self::script_mod(vm)
    }

    /// Two tags, two templates: a directory is a list, a file is a card.
    fn template(&self, tag: Tag) -> Option<LiveId> {
        match tag {
            Dir::TAG => Some(live_id!(files_dir_tpl)),
            Card::TAG => Some(live_id!(files_card_tpl)),
            _ => None,
        }
    }

    /// Files' own entries on the panels library's canvas.
    fn scenes(&self) -> Vec<Scene<Setup>> {
        super::scenes::scenes()
    }
}
