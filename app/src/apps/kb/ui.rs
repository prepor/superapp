//! The KB's Makepad half: its templates, and the tag each of them draws.
//!
//! A page is a reading and is set in the shell's prose face through the
//! reader's `Html`; everything that is chrome stays in the mono face. No
//! colour but the shell's greys and the one red.

use kernel::panel::Tag;
use kernel::scene::Scene;
use makepad_widgets::*;

use crate::shell::app_ui::{AppUi, Setup};

use super::panels::{Catalogue, Edit, File, History, Import, Page, Revision};
use super::widgets::{CataloguePanel, Dangling, EditPanel, FilePanel, HistoryPanel, ImportPanel, PagePanel, RevisionPanel};

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    /** A muted line of chrome. */
    mod.widgets.KbMuted = mod.widgets.SLabel {
        width: Fill, draw_text +: { color: #909090 }
    }

    // ---- the catalogue -----------------------------------------------------

    /** One page's row: the title over its summary, the slug and the date
        muted at the right. */
    mod.widgets.KbRowBody = View {
        width: Fill, height: Fit, flow: Right, spacing: 12
        View {
            width: Fill, height: Fit, flow: Down, spacing: 2
            title_lbl := mod.widgets.SLabel { width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis }
            summary_lbl := mod.widgets.KbMuted { max_lines: 2, text_overflow: TextOverflow.Ellipsis }
        }
        /* The right column is capped, so a long slug clips before it
           crowds the title and the summary out of a narrow row. */
        View {
            width: 132, height: Fit, flow: Down, spacing: 2, align: Align{x: 1.0}
            slug_lbl := mod.widgets.SLabel {
                width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis
                align: Align{x: 1.0}
                draw_text +: { color: #909090 }
            }
            date_lbl := mod.widgets.SLabel { width: Fit, draw_text +: { color: #909090 } }
        }
    }
    mod.widgets.KbRow = mod.widgets.TblRow {
        line := mod.widgets.TblLine { body := mod.widgets.KbRowBody {} }
        line_sel := mod.widgets.TblLineSel { body := mod.widgets.KbRowBody {} }
        line_mark := mod.widgets.TblLineMark { body := mod.widgets.KbRowBody {} }
        line_mark_sel := mod.widgets.TblLineMarkSel { body := mod.widgets.KbRowBody {} }
        mod.widgets.TblHairline {}
    }
    mod.widgets.KbCataloguePanel = set_type_default() do #(CataloguePanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill, flow: Down
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        filter_input := mod.widgets.TblFilter {}
        filter_err_lbl := mod.widgets.TblErr {}
        View { width: Fill, height: 4 }
        empty_lbl := mod.widgets.TblEmpty {}
        list := mod.widgets.SList {
            width: Fill, height: Fill, flow: Down, reuse_items: true
            row := mod.widgets.KbRow {}
            caption := mod.widgets.TblCaption {}
            band_rule := mod.widgets.TblBandRule {}
        }
        suggest: mod.widgets.TblSuggest {}
    }

    // ---- a reading -----------------------------------------------------------

    /** A link to nothing: the words in the muted grey, no underline — an
        underline is the promise that something opens — and no click. */
    mod.widgets.KbDangling = set_type_default() do #(Dangling::register_widget(vm)) {}
    /** The reader's letter, plus the one element a wiki page adds. */
    mod.widgets.KbHtml = mod.widgets.ReaderHtml {
        dangling := mod.widgets.KbDangling {}
    }
    mod.widgets.KbPageBody = View {
        width: Fill, height: Fit, flow: Down, padding: Inset{bottom: 8}
        body_html := mod.widgets.KbHtml {}
    }
    mod.widgets.KbHead = View {
        width: Fill, height: Fit, flow: Down, padding: Inset{top: 10, bottom: 4}
        cap_lbl := mod.widgets.SSection { text: "" }
        mod.widgets.TblHeadRule {}
    }
    mod.widgets.KbLinkRow = View {
        width: Fill, height: Fit, flow: Right, padding: Inset{top: 3, bottom: 3}
        link := mod.widgets.SLink {}
    }
    mod.widgets.KbMutedRow = View {
        width: Fill, height: Fit, flow: Right, padding: Inset{top: 3, bottom: 3}
        muted_lbl := mod.widgets.KbMuted { text: "" }
    }
    mod.widgets.KbPagePanel = set_type_default() do #(PagePanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill, flow: Down, spacing: 6
        padding: Inset{left: 16, right: 16, top: 14, bottom: 10}
        title_lbl := mod.widgets.SLabel {
            width: Fill, max_lines: 2
            draw_text +: { text_style: mod.widgets.SProseBoldStyle{font_size: 16.5} }
        }
        /* The name, edited where it is drawn: rename raises this in the
           title's place. */
        rename_row := View {
            visible: false, width: Fill, height: Fit, flow: Right, align: Align{y: 0.5}
            rename_input := mod.widgets.SField {
                empty_text: "title"
                return_key_type: ReturnKeyType.Done
            }
        }
        meta_lbl := mod.widgets.KbMuted { text: "" }
        status_lbl := mod.widgets.SLabel { visible: false, width: Fill, text: "", draw_text +: { color: #a01500 } }
        mod.widgets.TblHeadRule {}
        list := mod.widgets.SList {
            width: Fill, height: Fill, flow: Down, reuse_items: true
            body := mod.widgets.KbPageBody {}
            head := mod.widgets.KbHead {}
            link := mod.widgets.KbLinkRow {}
            muted := mod.widgets.KbMutedRow {}
        }
    }
    mod.widgets.KbRevisionPanel = set_type_default() do #(RevisionPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill, flow: Down, spacing: 6
        padding: Inset{left: 16, right: 16, top: 14, bottom: 10}
        title_lbl := mod.widgets.SLabel {
            width: Fill, max_lines: 2
            draw_text +: { text_style: mod.widgets.SProseBoldStyle{font_size: 16.5} }
        }
        meta_lbl := mod.widgets.KbMuted { text: "" }
        mod.widgets.TblHeadRule {}
        list := mod.widgets.SList {
            width: Fill, height: Fill, flow: Down, reuse_items: true
            body := mod.widgets.KbPageBody {}
        }
    }

    // ---- the editor ----------------------------------------------------------

    mod.widgets.KbEditPanel = set_type_default() do #(EditPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill, flow: Down
        body_input := mod.widgets.SourceInput {}
        error_lbl := mod.widgets.SLabel {
            width: Fill, margin: Inset{left: 12, right: 12, top: 6, bottom: 6}
            draw_text +: { color: #a01500 }
        }
        status_lbl := mod.widgets.SSection { margin: Inset{left: 12, right: 12, top: 6, bottom: 10} }
    }

    // ---- the history ---------------------------------------------------------

    /** One revision: the line that opens it, the chat it came from where
        one did, and the message. */
    mod.widgets.KbRevisionRow = View {
        width: Fill, height: Fit, flow: Down, spacing: 3
        padding: Inset{top: 7, bottom: 7}
        line_link := mod.widgets.SLink {}
        chat_row := View {
            visible: false, width: Fill, height: Fit, flow: Right
            chat_link := mod.widgets.SLink {}
        }
        msg_lbl := mod.widgets.KbMuted { text: "" }
        View { width: Fill, height: 4 }
        mod.widgets.TblHairline {}
    }
    mod.widgets.KbHistoryPanel = set_type_default() do #(HistoryPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill, flow: Down
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        mod.widgets.SSection { text: "REVISIONS" }
        mod.widgets.TblHeadRule {}
        empty_lbl := mod.widgets.KbMuted { visible: false, margin: Inset{top: 6}, text: "no revisions yet" }
        list := mod.widgets.SList {
            width: Fill, height: Fill, flow: Down, reuse_items: true
            row := mod.widgets.KbRevisionRow {}
        }
    }

    // ---- the file card -------------------------------------------------------

    /** The shell's card, with the line that says where the bytes are and
        the pages that name the file between the name and the viewer. */
    mod.widgets.KbFilePanel = set_type_default() do #(FilePanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill, flow: Down, spacing: 6
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        name_lbl := mod.widgets.SBoldLabel {
            width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
            draw_text +: { text_style: mod.widgets.SMonoBoldStyle{font_size: 13.0} }
        }
        kind_lbl := mod.widgets.SLabel { text: "", draw_text +: { color: #5a5a5a } }
        where_lbl := mod.widgets.KbMuted { text: "" }
        when_lbl := mod.widgets.SLabel { text: "", draw_text +: { color: #909090 } }
        detail_txt := mod.widgets.SText { text: "" }
        pages_row := View {
            width: Fill, height: Fit, flow: Right, spacing: 14
            mod.widgets.SSection { width: Fit, text: "NAMED BY" }
            p0 := mod.widgets.SLink { visible: false }
            p1 := mod.widgets.SLink { visible: false }
            p2 := mod.widgets.SLink { visible: false }
            p3 := mod.widgets.SLink { visible: false }
        }
        mod.widgets.SRule {}
        viewer := mod.widgets.FileViewer {}
    }

    // ---- the import form -----------------------------------------------------

    mod.widgets.KbImportPanel = set_type_default() do #(ImportPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill, flow: Down, spacing: 10
        padding: Inset{left: 16, right: 16, top: 16, bottom: 16}
        mod.widgets.SSection { text: "FOLDER" }
        path_input := mod.widgets.SField { width: Fill, empty_text: "~/cloud/KB" }
        mod.widgets.KbMuted { text: "Reads the folder's pages and files into the knowledge base; what is already here is left alone." }
        status_lbl := mod.widgets.SLabel { width: Fill, max_lines: 3, text: "" }
    }
}

/// The KB's Makepad half.
pub struct Ui;
pub static UI: Ui = Ui;

impl AppUi for Ui {
    fn script_mod(&self, vm: &mut ScriptVm) -> ScriptValue {
        self::script_mod(vm)
    }
    fn template(&self, tag: Tag) -> Option<LiveId> {
        match tag {
            Catalogue::TAG => Some(live_id!(kb_catalogue_tpl)),
            Page::TAG => Some(live_id!(kb_page_tpl)),
            Edit::TAG => Some(live_id!(kb_edit_tpl)),
            History::TAG => Some(live_id!(kb_history_tpl)),
            Revision::TAG => Some(live_id!(kb_revision_tpl)),
            File::TAG => Some(live_id!(kb_file_tpl)),
            Import::TAG => Some(live_id!(kb_import_tpl)),
            _ => None,
        }
    }
    fn scenes(&self) -> Vec<Scene<Setup>> {
        super::scenes::scenes()
    }
}
