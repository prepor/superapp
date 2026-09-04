//! `system`: the shell's own app.
//!
//! Help and about, and the card a slot gets when no app in this build owns
//! its tag. It is an app like any other — it registers tags, roots and
//! widget templates through the same interfaces mail and files will — which
//! is how the shell proves its extension points on itself.
//!
//! It is listed last, so the launcher's roots keep their order: an app's
//! own panels lead, help and about close.

use std::any::Any;

use kernel::app::{App, Root};
use kernel::panel::{PanelKind, Tag};
use makepad_widgets::*;

use crate::shell::app_ui::AppUi;

mod about;
mod bucket;
mod effects;
mod help;
mod job;
mod missing;
mod problems;
mod scenes;
mod search;

pub use about::{About, AboutPanel};
pub use bucket::{Bucket, BucketPanel};
pub use effects::{Effects, EffectsPanel};
pub use help::{Help, HelpPanel};
pub use job::{Job, JobPanel};
pub use missing::MissingPanel;
pub use problems::{Problems, ProblemsPanel};
pub use search::{Search, SearchPanel};

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    /** The manual, and the design language's own showcase: every part of
        the grammar it describes is drawn with the widget that implements
        it — the links really open and replace, and the bar at the foot
        really carries this panel's verbs. */
    mod.widgets.SysHelpPanel = set_type_default() do #(HelpPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        scroll_bars: ScrollBars{ show_scroll_x: false }

        mod.widgets.SSection { text: "LEGEND" }
        mod.widgets.SRule {}
        mod.widgets.SRow {
            solid_link := mod.widgets.SLink {}
            mod.widgets.SLabel { text: " — opens a panel to the right, joined" }
        }
        mod.widgets.SRow {
            dotted_link := mod.widgets.SLink {}
            mod.widgets.SLabel { text: " — replaces this panel in place" }
        }
        mod.widgets.SRow {
            mod.widgets.SLabel { width: Fill, text: "the bar at the foot carries what this panel can do: buttons act, links go. its bold letter is its chord." }
        }
        mod.widgets.SRow {
            mod.widgets.SKbd { text: "cmd" }
            mod.widgets.SLabel { text: "+click — always a fresh, un-joined panel" }
        }
        mod.widgets.SRow {
            mod.widgets.SLabel { width: Fill, text: "a ═ bridge marks a joined pair: the next solid link in the parent replaces the joined panel; replacing a panel closes its joined chain" }
        }
        mod.widgets.SRow {
            mod.widgets.SLabel { text: "colour is reserved for errors: " }
            mod.widgets.SLabel { text: "like this", draw_text +: { color: #a01500 } }
        }

        View { width: Fill, height: 10 }
        mod.widgets.SSection { text: "KEYS" }
        mod.widgets.SRule {}
        mod.widgets.SRow {
            mod.widgets.SKbd { text: "cmd" }
            mod.widgets.SLabel { text: "+arrows — focus panels" }
        }
        mod.widgets.SRow {
            mod.widgets.SKbd { text: "cmd" }
            mod.widgets.SKbd { text: "shift" }
            mod.widgets.SLabel { text: "+same — move the panel" }
        }
        mod.widgets.SRow {
            mod.widgets.SKbd { text: "cmd" }
            mod.widgets.SKbd { text: "1" }
            mod.widgets.SLabel { text: "…9 — go to a workspace; with " }
            mod.widgets.SKbd { text: "shift" }
            mod.widgets.SLabel { text: " send the panel there" }
        }
        mod.widgets.SRow {
            mod.widgets.SKbd { text: "cmd" }
            mod.widgets.SKbd { text: "w" }
            mod.widgets.SLabel { text: " — close the focused panel" }
        }
        mod.widgets.SRow {
            mod.widgets.SKbd { text: "cmd" }
            mod.widgets.SKbd { text: "z" }
            mod.widgets.SLabel { text: " — undo; with " }
            mod.widgets.SKbd { text: "shift" }
            mod.widgets.SLabel { text: " redo" }
        }
        mod.widgets.SRow {
            mod.widgets.SKbd { text: "cmd" }
            mod.widgets.SKbd { text: "u" }
            mod.widgets.SLabel { text: " — history: the whole tree, walkable" }
        }
        mod.widgets.SRow {
            mod.widgets.SKbd { text: "cmd" }
            mod.widgets.SKbd { text: "[" }
            mod.widgets.SKbd { text: "]" }
            mod.widgets.SKbd { text: "," }
            mod.widgets.SKbd { text: "." }
            mod.widgets.SKbd { text: "t" }
            mod.widgets.SLabel { text: " — columns: consume, expel, pull, push, tabs" }
        }
        mod.widgets.SRow {
            mod.widgets.SKbd { text: "cmd" }
            mod.widgets.SKbd { text: "shift" }
            mod.widgets.SKbd { text: "s" }
            mod.widgets.SLabel { text: " — search: one question, every app's own sources" }
        }
        mod.widgets.SRow {
            mod.widgets.SKbd { text: "cmd" }
            mod.widgets.SKbd { text: "cmd" }
            mod.widgets.SLabel { text: " — the launcher: everything open, and every root" }
        }
    }

    /** The colophon: three lines and the way back. */
    mod.widgets.SysAboutPanel = set_type_default() do #(AboutPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}

        mod.widgets.SSection { text: "SUPERAPP" }
        mod.widgets.SRule {}
        mod.widgets.SRow {
            mod.widgets.SLabel { width: Fill, text: "a kernel that does not draw, a shell that does, and apps on top of both." }
        }
        mod.widgets.SRow {
            mod.widgets.SLabel { width: Fill, text: "panels on scrolling tiled workspaces. monochrome, monospaced, keyboard first." }
        }
        View { width: Fill, height: 8 }
        mod.widgets.SRow {
            help_link := mod.widgets.SLink {}
        }
    }

    // ---- the effect log ----------------------------------------------------

    /** One job as the log lists it: the verb and whose it was on the first
        line, the effect's own sentence under it, and — only when there is
        one — what went wrong, in the colour errors get.

        The body is declared once and hung in each of the row's four twins,
        so the cursor wash and the mark bar stay the shell's. */
    mod.widgets.SysEffectBody = View {
        width: Fill, height: Fit
        flow: Down
        View {
            width: Fill, height: Fit
            align: Align{y: 0.5}
            kind_lbl := mod.widgets.SLabel { padding: 0, width: Fit, text: "" }
            View { width: 8, height: 1 }
            // The entity rides a Fill View whose flow is Down: a Fill label
            // on a Right flow's main axis defer-walks.
            View {
                width: Fill, height: Fit
                flow: Down
                entity_lbl := mod.widgets.SLabel {
                    padding: 0
                    width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
                    draw_text +: { color: #909090 }
                }
            }
            View { width: 10, height: 1 }
            status_lbl := mod.widgets.SLabel {
                padding: 0, width: Fit, text: "", draw_text +: { color: #5a5a5a }
            }
            View { width: 10, height: 1 }
            date_lbl := mod.widgets.SLabel {
                padding: 0, width: Fit, text: "", draw_text +: { color: #909090 }
            }
        }
        what_lbl := mod.widgets.SLabel {
            padding: 0
            width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
        }
        err_lbl := mod.widgets.SLabel {
            visible: false
            padding: 0
            width: Fill, max_lines: 2, text_overflow: TextOverflow.Ellipsis, text: ""
            draw_text +: { color: #a01500 }
        }
    }

    /** A row of the log: the four twins, and the hairline under them. */
    mod.widgets.SysEffectRow = mod.widgets.TblRow {
        line          := mod.widgets.TblLine        { body := mod.widgets.SysEffectBody {} }
        line_sel      := mod.widgets.TblLineSel     { body := mod.widgets.SysEffectBody {} }
        line_mark     := mod.widgets.TblLineMark    { body := mod.widgets.SysEffectBody {} }
        line_mark_sel := mod.widgets.TblLineMarkSel { body := mod.widgets.SysEffectBody {} }
        mod.widgets.TblHairline {}
    }

    /** The effect log: the filter over the header over the virtualized
        list — a rich table over `effect::LOG`, which is the queue and the
        in-memory ring joined in SQL, so one list holds everything that left
        the process. */
    mod.widgets.SysEffectsPanel = set_type_default() do #(EffectsPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        // No flow spacing: the gaps are explicit, so a rule sits the same
        // 3 pt under the header as under every row.
        spacing: 0

        filter_input := mod.widgets.TblFilter {}
        filter_err_lbl := mod.widgets.TblErr {}
        View { width: Fill, height: 6 }
        // Header cells for the columns the head line has; the sentence
        // under it owns no column and so gets no header.
        View {
            width: Fill, height: Fit
            padding: Inset{left: 8, right: 8, top: 0, bottom: 3}
            View {
                width: Fill, height: Fit
                mod.widgets.SSection { padding: 0, text: "EFFECT" }
            }
            mod.widgets.SSection { padding: 0, width: Fit, text: "STATUS" }
        }
        mod.widgets.TblHeadRule {}
        empty_lbl := mod.widgets.TblEmpty {}
        list := mod.widgets.SList {
            width: Fill, height: Fill
            flow: Down
            // A row that scrolls out is kept for the next one that scrolls
            // in, rather than built again.
            reuse_items: true
            row := mod.widgets.SysEffectRow {}
            caption := mod.widgets.TblCaption {}
            band_rule := mod.widgets.TblBandRule {}
        }
        suggest: mod.widgets.TblSuggest {}
    }

    /** One effect of the log, in full — what the log previews into.
        Everything below the subject is a selectable run; a payload is
        something one copies into a report. */
    mod.widgets.SysJobPanel = set_type_default() do #(JobPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        spacing: 0
        scroll_bars: ScrollBars{ show_scroll_x: false }

        View {
            width: Fill, height: Fit, align: Align{y: 0.5}
            kind_lbl := mod.widgets.SLabel { padding: 0, width: Fit, text: "" }
            View { width: 8, height: 1 }
            View {
                width: Fill, height: Fit
                flow: Down
                entity_lbl := mod.widgets.SLabel {
                    padding: 0
                    width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
                    draw_text +: { color: #909090 }
                }
            }
            View { width: 10, height: 1 }
            status_lbl := mod.widgets.SLabel {
                padding: 0, width: Fit, text: "", draw_text +: { color: #5a5a5a }
            }
        }
        // Every run here is `is_multiline`: without it a TextInput lays out
        // on one row and neither wraps nor honours a newline — and this
        // panel is nothing but long text.
        what_txt := mod.widgets.SText { is_multiline: true, margin: Inset{top: 2} }
        // A run that comes and goes hangs on a View: `visible` is the
        // View's property, and a TextInput neither takes it in the DSL nor
        // honours `set_visible`.
        err_row := View {
            visible: false
            width: Fill, height: Fit
            flow: Down
            err_txt := mod.widgets.SText {
                is_multiline: true
                margin: Inset{top: 4}
                draw_text +: {
                    color: #a01500
                    color_hover: #a01500
                    color_focus: #a01500
                    color_down: #a01500
                    color_empty: #a01500
                }
            }
        }

        View { width: Fill, height: 10 }
        mod.widgets.SSection { text: "JOB" }
        mod.widgets.SRule {}
        meta_txt := mod.widgets.SText { is_multiline: true }

        payload_block := View {
            width: Fill, height: Fit
            flow: Down
            View { width: Fill, height: 10 }
            mod.widgets.SSection { text: "PAYLOAD" }
            mod.widgets.SRule {}
            payload_txt := mod.widgets.SText { is_multiline: true }
        }

        reply_block := View {
            visible: false
            width: Fill, height: Fit
            flow: Down
            View { width: Fill, height: 10 }
            mod.widgets.SSection { text: "REPLY" }
            mod.widgets.SRule {}
            reply_txt := mod.widgets.SText { is_multiline: true }
        }
    }

    // ---- search ------------------------------------------------------------

    /** One row of an answer: what was found and which source found it on
        the first line, the source's own second thought under it.

        The body is declared once and hung in each of the row's four twins,
        so the cursor wash and the mark bar stay the shell's. */
    mod.widgets.SysHitBody = View {
        width: Fill, height: Fit
        flow: Down
        View {
            width: Fill, height: Fit
            align: Align{y: 0.5}
            // The label rides a Fill View whose flow is Down: a Fill label
            // on a Right flow's main axis defer-walks.
            View {
                width: Fill, height: Fit
                flow: Down
                label_lbl := mod.widgets.SLabel {
                    padding: 0
                    width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
                }
            }
            View { width: 10, height: 1 }
            source_lbl := mod.widgets.SLabel {
                padding: 0, width: Fit, text: "", draw_text +: { color: #909090 }
            }
        }
        detail_lbl := mod.widgets.SLabel {
            visible: false
            padding: 0
            width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
            draw_text +: { color: #909090 }
        }
    }

    /** A row of an answer: the four twins, and the hairline under them. */
    mod.widgets.SysHitRow = mod.widgets.TblRow {
        line          := mod.widgets.TblLine        { body := mod.widgets.SysHitBody {} }
        line_sel      := mod.widgets.TblLineSel     { body := mod.widgets.SysHitBody {} }
        line_mark     := mod.widgets.TblLineMark    { body := mod.widgets.SysHitBody {} }
        line_mark_sel := mod.widgets.TblLineMarkSel { body := mod.widgets.SysHitBody {} }
        mod.widgets.TblHairline {}
    }

    /** The search panel: the same filter over the same header over the same
        virtualized list every rich table has — but the words in that field
        are the question every source is asked, and only its `@` tags narrow
        what comes back. */
    mod.widgets.SysSearchPanel = set_type_default() do #(SearchPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        spacing: 0

        filter_input := mod.widgets.TblFilter {
            empty_text: "search…  ( / )   @app: for a source"
        }
        filter_err_lbl := mod.widgets.TblErr {}
        View { width: Fill, height: 6 }
        View {
            width: Fill, height: Fit
            padding: Inset{left: 8, right: 8, top: 0, bottom: 3}
            View {
                width: Fill, height: Fit
                mod.widgets.SSection { padding: 0, text: "FOUND" }
            }
            mod.widgets.SSection { padding: 0, width: Fit, text: "SOURCE" }
        }
        mod.widgets.TblHeadRule {}
        empty_lbl := mod.widgets.TblEmpty {}
        list := mod.widgets.SList {
            width: Fill, height: Fill
            flow: Down
            reuse_items: true
            row := mod.widgets.SysHitRow {}
            caption := mod.widgets.TblCaption {}
            band_rule := mod.widgets.TblBandRule {}
        }
        suggest: mod.widgets.TblSuggest {}
    }

    // ---- problems ----------------------------------------------------------

    /** A small bordered button a row wears. Rows give no chords — a panel
        with a control per row would have to invent a letter per row — so it
        carries no letter either. */
    mod.widgets.SysRowBtn = View {
        width: Fit, height: Fit
        flow: Right
        align: Align{y: 0.5}
        margin: Inset{left: 8}
        padding: Inset{left: 10, right: 10, top: 4, bottom: 4}
        cursor: MouseCursor.Hand
        show_bg: true
        draw_bg +: {
            color: #ffffff
            // The 1 pt ink border, shader-drawn: a View's bg has no border
            // of its own, and a distinct shader earns the correctly ordered
            // draw call anyway.
            pixel: fn() {
                let p = self.pos * self.rect_size
                let d = min(min(p.x, p.y), min(self.rect_size.x - p.x, self.rect_size.y - p.y))
                if d < 1.0 {
                    return vec4(0.078, 0.078, 0.078, 1.0)
                }
                return vec4(self.color.xyz * self.color.w, self.color.w)
            }
        }
        lbl := mod.widgets.SLabel {
            padding: 0, text: ""
            draw_text +: { text_style: mod.widgets.SMonoStyle{font_size: 8.25} }
        }
    }

    /** One standing problem: what it concerns and its buttons, what is
        wrong under them, then the muted detail and its links.

        All three lines are selectable runs and not labels. What is wrong is
        the sentence a person carries somewhere else — into a search, into a
        bug report — and a row nobody can copy out of makes them retype an
        error by hand. */
    mod.widgets.SysProblemRow = View {
        width: Fill, height: Fit
        flow: Down
        padding: Inset{top: 6, bottom: 6}
        head := View {
            width: Fill, height: Fit
            align: Align{y: 0.5}
            label_lbl := mod.widgets.SText {
                width: Fit, is_multiline: false, text: ""
                draw_text +: { text_style: mod.widgets.SMonoBoldStyle{} }
            }
            View { width: Fill, height: 1 }
            b0 := mod.widgets.SysRowBtn {}
            b1 := mod.widgets.SysRowBtn {}
        }
        line_lbl := mod.widgets.SText {
            width: Fill, is_multiline: true
            margin: Inset{top: 6}
            text: ""
            draw_text +: {
                color: #a01500
                color_hover: #a01500
                color_focus: #a01500
                color_down: #a01500
                color_empty: #a01500
            }
        }
        foot := View {
            width: Fill, height: Fit
            flow: Right
            align: Align{y: 0.5}
            margin: Inset{top: 5}
            // Fill, so a long detail wraps rather than running under a link.
            detail_lbl := mod.widgets.SText {
                width: Fill, is_multiline: true, text: ""
                draw_text +: {
                    color: #909090
                    color_hover: #909090
                    color_focus: #909090
                    color_down: #909090
                    color_empty: #909090
                }
            }
            l0 := mod.widgets.SLink { margin: Inset{left: 12} }
            l1 := mod.widgets.SLink { margin: Inset{left: 12} }
        }
        View { width: Fill, height: 8 }
        mod.widgets.TblHairline {}
    }

    /** Every standing problem as a row, or one muted line saying nothing
        is wrong. */
    mod.widgets.SysProblemsPanel = set_type_default() do #(ProblemsPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        spacing: 0

        mod.widgets.SSection { text: "PROBLEMS" }
        mod.widgets.SRule {}
        none_lbl := mod.widgets.SLabel {
            margin: Inset{top: 6}
            text: "nothing is wrong", draw_text +: { color: #909090 }
        }
        list := mod.widgets.SList {
            width: Fill, height: Fill
            flow: Down
            problem_row := mod.widgets.SysProblemRow {}
        }
    }

    // ---- device sync -------------------------------------------------------

    /** The device-sync form: where the bucket is, and the key that opens
        it. Three fields and nothing else — the *connect* that acts on them
        is on the bar at the foot, where every button that acts on what a
        panel shows belongs. */
    mod.widgets.SysBucketPanel = set_type_default() do #(BucketPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        spacing: 0

        View {
            width: Fill, height: Fit, align: Align{y: 0.5}
            mod.widgets.SSection { width: 82, text: "BUCKET" }
            url_input := mod.widgets.SField {
                empty_text: "https://<account>.r2.cloudflarestorage.com/<bucket>"
                autocapitalize: AutoCapitalize.None
                autocorrect: AutoCorrect.Disabled
            }
        }
        View { width: Fill, height: 7 }
        View {
            width: Fill, height: Fit, align: Align{y: 0.5}
            mod.widgets.SSection { width: 82, text: "KEY ID" }
            key_input := mod.widgets.SField {
                empty_text: "access key id"
                autocapitalize: AutoCapitalize.None
                autocorrect: AutoCorrect.Disabled
            }
        }
        View { width: Fill, height: 7 }
        View {
            width: Fill, height: Fit, align: Align{y: 0.5}
            mod.widgets.SSection { width: 82, text: "SECRET" }
            secret_input := mod.widgets.SField {
                empty_text: "cloudflare api token — its value, stored, never shown"
                autocapitalize: AutoCapitalize.None
                autocorrect: AutoCorrect.Disabled
            }
        }
        View { width: Fill, height: 8 }
        mod.widgets.SRow {
            mod.widgets.SLabel {
                width: Fill
                text: "the token goes to this machine's keychain, never to the store: it is the one thing that must not replicate."
                draw_text +: { color: #909090 }
            }
        }
    }

    /** A slot whose tag no app in this build owns. It is kept, not
        dropped: another build has the app, and the session is shared. */
    mod.widgets.SysMissingPanel = set_type_default() do #(MissingPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}

        tag_lbl := mod.widgets.SBoldLabel { text: "" }
        mod.widgets.SRow {
            mod.widgets.SLabel {
                width: Fill
                text: "no app for this panel in this build"
                draw_text +: { color: #909090 }
            }
        }
    }
}

/// The app.
pub struct System;

/// The one in this build.
pub static SYSTEM: System = System;

static HELP_KIND: help::HelpKind = help::HelpKind;
static ABOUT_KIND: about::AboutKind = about::AboutKind;
static EFFECTS_KIND: effects::EffectsKind = effects::EffectsKind;
static JOB_KIND: job::JobKind = job::JobKind;
static PROBLEMS_KIND: problems::ProblemsKind = problems::ProblemsKind;
static SEARCH_KIND: search::SearchKind = search::SearchKind;
static BUCKET_KIND: bucket::BucketKind = bucket::BucketKind;
static KINDS: &[&dyn PanelKind] = &[
    &HELP_KIND,
    &ABOUT_KIND,
    &EFFECTS_KIND,
    &JOB_KIND,
    &PROBLEMS_KIND,
    &SEARCH_KIND,
    &BUCKET_KIND,
];

impl App for System {
    fn id(&self) -> &'static str {
        "system"
    }

    fn kinds(&self) -> &'static [&'static dyn PanelKind] {
        KINDS
    }

    /// Help leads, so an empty store comes up on the manual. A `job` is
    /// not a root: one is reached from the log that lists it.
    fn roots(&self) -> Vec<Root> {
        vec![
            Root::new(Help::id(), "help", "manual keys legend grammar"),
            Root::new(About::id(), "about", "colophon version"),
            Root::new(Effects::id(), "effects", "log queue jobs ring"),
            Root::new(Problems::id(), "problems", "wrong failing standing"),
            Root::new(Search::id(), "search", "find query sources everything"),
            Root::new(Bucket::id(), "device sync", "bucket lease r2 replicate"),
        ]
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Its Makepad half.
pub struct Ui;

/// The one in this build.
pub static UI: Ui = Ui;

impl AppUi for Ui {
    fn script_mod(&self, vm: &mut ScriptVm) -> ScriptValue {
        self::script_mod(vm)
    }

    fn template(&self, tag: Tag) -> Option<LiveId> {
        match tag {
            Help::TAG => Some(live_id!(sys_help_tpl)),
            About::TAG => Some(live_id!(sys_about_tpl)),
            Effects::TAG => Some(live_id!(sys_effects_tpl)),
            Job::TAG => Some(live_id!(sys_job_tpl)),
            Problems::TAG => Some(live_id!(sys_problems_tpl)),
            Search::TAG => Some(live_id!(sys_search_tpl)),
            Bucket::TAG => Some(live_id!(sys_bucket_tpl)),
            _ => None,
        }
    }

    /// The shell's own panels, on the canvas: the manual, the colophon, and
    /// the two lists it keeps about itself.
    fn scenes(&self) -> Vec<kernel::scene::Scene<crate::shell::app_ui::Setup>> {
        scenes::scenes()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use kernel::panel::PanelId;
    use kernel::session::{Action, Session};

    use super::*;

    static APPS: &[&dyn App] = &[&SYSTEM];

    /// Every kind the shell's own app owns says what it is about in its own
    /// words — not the default, which is the title and the identity and
    /// tells an agent nothing it could not read off the tag.
    ///
    /// A new kind here fails this until it has both a sample identity and a
    /// paragraph: the two go together, since a paragraph is written about
    /// the arguments a real one carries.
    #[test]
    fn every_system_panel_says_what_it_is_about() {
        let ids = [
            Help::id(),
            About::id(),
            Effects::id(),
            Job::id(7),
            Problems::id(),
            Search::id(),
            Bucket::id(),
        ];
        let covered: HashSet<Tag> = ids.iter().map(|id| id.tag).collect();
        for kind in SYSTEM.kinds() {
            assert!(
                covered.contains(&kind.tag()),
                "no sample panel for the tag {}",
                kind.tag()
            );
        }

        let mut s = Session::fake(APPS);
        for id in ids {
            let slot = open(&mut s, id.clone());
            let (title, about) = {
                let inst = s.panel(slot).expect("the panel");
                let b = inst.borrow();
                (b.title(), b.about())
            };
            assert!(!about.is_empty(), "{id} says nothing");
            assert_ne!(about, format!("{title} — {id}"), "{id} is on the default");
            assert!(about.len() > 120, "{id}: “{about}”");
            assert!(
                !about.contains("cmd+"),
                "{id} names a chord; keys go on the control"
            );
        }
    }

    /// A panel opened as the launcher would.
    fn open(s: &mut Session, id: PanelId) -> kernel::layout::SlotId {
        let show = id.clone();
        s.act(Action::new("open", format!("open “{id}”")).moving(move |wm| {
            wm.open(show, None, false);
        }));
        s.settle();
        s.focus().expect("the new slot has focus")
    }
}
