use super::panels::{AddFeed, Article, Articles, Feeds, ImportFeeds};
use super::widgets::{
    RssAddFeedPanel, RssArticlePanel, RssArticlesPanel, RssFeedsPanel, RssImportPanel,
};
use crate::shell::app_ui::{AppUi, Setup};
use crate::shell::catalog::{panel, workspace_on};
use kernel::panel::Tag;
use kernel::scene::Scene;
use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.RssFeedBody = View {
        width: Fill, height: Fit, flow: Down, spacing: 4
        View {
            width: Fill, height: Fit, flow: Right
            View { width: Fill, height: Fit, flow: Down
                title_lbl := mod.widgets.SLabel { width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis }
            }
            meta_lbl := mod.widgets.SLabel { width: Fit, draw_text +: { color: #909090 } }
        }
        detail_lbl := mod.widgets.SLabel { width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, draw_text +: { color: #909090 } }
        status_lbl := mod.widgets.SLabel { width: Fill, max_lines: 2, draw_text +: { color: #909090 } }
    }
    mod.widgets.RssFeedRow = mod.widgets.TblRow {
        line := mod.widgets.TblLine { body := mod.widgets.RssFeedBody {} }
        line_sel := mod.widgets.TblLineSel { body := mod.widgets.RssFeedBody {} }
        line_mark := mod.widgets.TblLineMark { body := mod.widgets.RssFeedBody {} }
        line_mark_sel := mod.widgets.TblLineMarkSel { body := mod.widgets.RssFeedBody {} }
        mod.widgets.TblHairline {}
    }
    mod.widgets.RssArticleBody = View {
        width: Fill, height: Fit, flow: Down, spacing: 4
        title_lbl := mod.widgets.SLabel { width: Fill, max_lines: 2 }
        unseen_lbl := mod.widgets.SLabel { width: Fill, max_lines: 2, draw_text +: { text_style: mod.widgets.SMonoBoldStyle{} } }
        View {
            width: Fill, height: Fit, flow: Right
            View { width: Fill, height: Fit, flow: Down
                detail_lbl := mod.widgets.SLabel { width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, draw_text +: { color: #909090 } }
            }
            meta_lbl := mod.widgets.SLabel { width: Fit, draw_text +: { color: #909090 } }
        }
    }
    mod.widgets.RssArticleRow = mod.widgets.TblRow {
        line := mod.widgets.TblLine { body := mod.widgets.RssArticleBody {} }
        line_sel := mod.widgets.TblLineSel { body := mod.widgets.RssArticleBody {} }
        line_mark := mod.widgets.TblLineMark { body := mod.widgets.RssArticleBody {} }
        line_mark_sel := mod.widgets.TblLineMarkSel { body := mod.widgets.RssArticleBody {} }
        mod.widgets.TblHairline {}
    }

    mod.widgets.RssFeedsPanel = set_type_default() do #(RssFeedsPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill, flow: Down
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        filter_input := mod.widgets.TblFilter {}
        filter_err_lbl := mod.widgets.TblErr {}
        View { width: Fill, height: 8 }
        mod.widgets.SSection { text: "SUBSCRIPTIONS" }
        mod.widgets.TblHeadRule {}
        empty_lbl := mod.widgets.TblEmpty {}
        list := mod.widgets.SList {
            width: Fill, height: Fill, flow: Down, reuse_items: true
            row := mod.widgets.RssFeedRow {}
            caption := mod.widgets.TblCaption {}
            band_rule := mod.widgets.TblBandRule {}
        }
        suggest: mod.widgets.TblSuggest {}
    }
    mod.widgets.RssArticlesPanel = set_type_default() do #(RssArticlesPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill, flow: Down
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        filter_input := mod.widgets.TblFilter {}
        filter_err_lbl := mod.widgets.TblErr {}
        View { width: Fill, height: 8 }
        View { width: Fill, height: Fit, flow: Right
            View { width: Fill, height: Fit
                mod.widgets.SSection { text: "ARTICLES" }
            }
            mod.widgets.SSection { width: Fit, text: "OLDEST FIRST" }
        }
        mod.widgets.TblHeadRule {}
        empty_lbl := mod.widgets.TblEmpty {}
        list := mod.widgets.SList {
            width: Fill, height: Fill, flow: Down, reuse_items: true
            row := mod.widgets.RssArticleRow {}
            caption := mod.widgets.TblCaption {}
            band_rule := mod.widgets.TblBandRule {}
        }
        suggest: mod.widgets.TblSuggest {}
    }
    mod.widgets.RssAddFeedPanel = set_type_default() do #(RssAddFeedPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill, flow: Down, spacing: 10
        padding: Inset{left: 16, right: 16, top: 16, bottom: 16}
        mod.widgets.SSection { text: "FEED URL" }
        url_input := mod.widgets.SField { width: Fill, empty_text: "https://example.com/feed.xml" }
        mod.widgets.SLabel { width: Fill, text: "Paste an RSS or Atom feed URL, then subscribe.", draw_text +: { color: #909090 } }
        error_lbl := mod.widgets.SLabel { width: Fill, text: "", draw_text +: { color: #a01500 } }
    }
    mod.widgets.RssImportPanel = set_type_default() do #(RssImportPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill, flow: Down, spacing: 10
        padding: Inset{left: 16, right: 16, top: 16, bottom: 16}
        mod.widgets.SSection { text: "OPML FILE" }
        path_input := mod.widgets.SField { width: Fill, empty_text: "~/Downloads/feeds.opml" }
        mod.widgets.SLabel { width: Fill, text: "Paste an OPML file path, then import. Existing subscriptions are skipped. Undo removes this import.", draw_text +: { color: #909090 } }
        error_lbl := mod.widgets.SLabel { width: Fill, text: "", draw_text +: { color: #a01500 } }
        status_lbl := mod.widgets.SLabel { width: Fill, text: "" }
    }
    mod.widgets.RssArticlePanel = set_type_default() do #(RssArticlePanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill, flow: Down
        padding: Inset{left: 20, right: 20, top: 16, bottom: 16}
        list := mod.widgets.SList {
            width: Fill, height: Fill, flow: Down, reuse_items: true
            article := View {
                width: Fill, height: Fit, flow: Down, spacing: 12
                title_lbl := mod.widgets.SLabel { width: Fill, draw_text +: { text_style: mod.widgets.SProseBoldStyle{font_size: 16.5} } }
                meta_lbl := mod.widgets.SLabel { width: Fill, draw_text +: { color: #909090 } }
                mod.widgets.TblHeadRule {}
                body_html := mod.widgets.ReaderHtml {}
                original_html := mod.widgets.ReaderHtml {}
                View { width: Fill, height: 16 }
            }
        }
    }
}

pub struct Ui;
pub static UI: Ui = Ui;
impl AppUi for Ui {
    fn script_mod(&self, vm: &mut ScriptVm) -> ScriptValue {
        self::script_mod(vm)
    }
    fn template(&self, tag: Tag) -> Option<LiveId> {
        match tag {
            Feeds::TAG => Some(live_id!(rss_feeds_tpl)),
            Articles::TAG => Some(live_id!(rss_articles_tpl)),
            Article::TAG => Some(live_id!(rss_article_tpl)),
            AddFeed::TAG => Some(live_id!(rss_add_feed_tpl)),
            ImportFeeds::TAG => Some(live_id!(rss_import_tpl)),
            _ => None,
        }
    }
    fn scenes(&self) -> Vec<Scene<Setup>> {
        vec![Scene::new("rss", (600.0, 700.0))
            .node("unseen", panel(|_| Articles::id(), ""))
            .node("feeds", panel(|_| Feeds::id(), ""))
            .node("subscribe", panel(|_| AddFeed::id(), ""))
            .node("import", panel(|_| ImportFeeds::id(), ""))
            .node(
                "reading",
                workspace_on(|_| Articles::id(), "key down\nwait 600"),
            )
            .sized((1200.0, 700.0))
            .node(
                "clip",
                workspace_on(|_| Articles::id(), "key down 4\nwait 600"),
            )
            .sized((1200.0, 700.0))
            .about("a <video> the feed kept: the kit's surface in the column, the strip beneath, the caption after")
            .node(
                "sound",
                workspace_on(|_| Articles::id(), "key down 5\nwait 600"),
            )
            .sized((1200.0, 700.0))
            .about("an <audio>: the strip alone, in the flow of the prose")]
    }
}
