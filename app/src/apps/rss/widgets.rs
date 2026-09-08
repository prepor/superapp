use super::{
    model,
    panels::{AddFeed, Article, Articles, Feeds, ImportFeeds},
};
use crate::reader::{self, pictures};
use crate::shell::hosted::PanelProps;
use crate::shell::widgets::table::{self, RowSpec, TableView};
use kernel::panel::PanelId;
use kernel::richtable::{ListState, SqlSource};
use kernel::session::Session;
use kernel::time::fmt_date;
use makepad_widgets::*;

pub struct FeedRows;
impl RowSpec for FeedRows {
    type Src = &'static SqlSource<model::Feed, i64>;
    type Panel = Feeds;
    fn list(p: &mut Feeds) -> &mut ListState<Self::Src> {
        &mut p.list
    }
    fn row_tpl() -> LiveId {
        live_id!(row)
    }
    fn populate(
        cx: &mut Cx,
        row: &WidgetRef,
        r: &model::Feed,
        selected: bool,
        marked: bool,
        _: f64,
    ) {
        let line = table::line(cx, row, selected, marked);
        line.label(cx, ids!(body.title_lbl)).set_text(cx, &r.title);
        line.label(cx, ids!(body.meta_lbl))
            .set_text(cx, &format!("{} unseen", r.unseen));
        line.label(cx, ids!(body.detail_lbl)).set_text(cx, &r.url);
        let status = if !r.error.is_empty() {
            r.error.clone()
        } else {
            r.checked
                .map(|t| format!("updated {}", fmt_date(t)))
                .unwrap_or_else(|| "waiting for first refresh…".into())
        };
        line.label(cx, ids!(body.status_lbl)).set_text(cx, &status);
    }
    fn label(r: &model::Feed, _: f64) -> String {
        if r.error.is_empty() {
            format!("{} · {} unseen", r.title, r.unseen)
        } else {
            format!("{} · {} unseen · {}", r.title, r.unseen, r.error)
        }
    }
    fn target(r: &model::Feed) -> PanelId {
        Articles::for_feed(r.id)
    }
    fn empty_line(_: &Feeds, filter: &str) -> String {
        if filter.trim().is_empty() {
            "no feeds yet — add a feed to start reading"
        } else {
            "no feed under this filter"
        }
        .into()
    }
    fn swipe_verbs(_: &Feeds) -> [Option<&'static str>; 2] {
        [Some("rss.remove"), None]
    }
}

pub struct ArticleRows;
impl RowSpec for ArticleRows {
    type Src = &'static SqlSource<model::Article, i64>;
    type Panel = Articles;
    fn list(p: &mut Articles) -> &mut ListState<Self::Src> {
        &mut p.list
    }
    fn row_tpl() -> LiveId {
        live_id!(row)
    }
    fn seed_filter(p: &Articles) -> String {
        p.filter.clone()
    }
    fn populate(
        cx: &mut Cx,
        row: &WidgetRef,
        r: &model::Article,
        selected: bool,
        marked: bool,
        _: f64,
    ) {
        let line = table::line(cx, row, selected, marked);
        for (path, visible) in [
            (ids!(body.title_lbl), r.seen),
            (ids!(body.unseen_lbl), !r.seen),
        ] {
            let label = line.label(cx, path);
            label.set_text(cx, if visible { &r.title } else { "" });
            label.set_visible(cx, visible);
        }
        line.label(cx, ids!(body.meta_lbl))
            .set_text(cx, &fmt_date(r.published));
        line.label(cx, ids!(body.detail_lbl))
            .set_text(cx, &r.feed_title);
    }
    fn label(r: &model::Article, _: f64) -> String {
        r.title.clone()
    }
    fn target(r: &model::Article) -> PanelId {
        Article::id(r.id)
    }
    fn empty_line(_: &Articles, filter: &str) -> String {
        if filter.trim() == "@unseen" {
            "all caught up — manage subscriptions in feeds"
        } else {
            "no article under this filter"
        }
        .into()
    }
    fn swipe_verbs(_: &Articles) -> [Option<&'static str>; 2] {
        [Some("rss.seen"), Some("rss.unseen")]
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct RssFeedsPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[live]
    suggest: View,
    #[rust]
    table: TableView<FeedRows>,
}
impl Widget for RssFeedsPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.table.handle_event(cx, event, scope, &mut self.view);
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.table
            .draw(cx, scope, walk, &mut self.view, &mut self.suggest)
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct RssArticlesPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[live]
    suggest: View,
    #[rust]
    table: TableView<ArticleRows>,
}
impl Widget for RssArticlesPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.table.handle_event(cx, event, scope, &mut self.view);
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.table
            .draw(cx, scope, walk, &mut self.view, &mut self.suggest)
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct RssAddFeedPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
}
impl Widget for RssAddFeedPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return;
        };
        let field = self.view.text_input(cx, ids!(url_input));
        if let Event::Actions(actions) = event {
            if field.changed(actions).is_some() || field.returned(actions).is_some() {
                let mut borrow = props.panel.borrow_mut();
                let Some(p) = borrow.as_any().downcast_mut::<AddFeed>() else {
                    return;
                };
                p.url = field.text();
                p.error.clear();
                if field.returned(actions).is_some() {
                    if let Some(s) = scope.data.get_mut::<Session>() {
                        p.submit(s);
                    }
                }
                self.view.redraw(cx);
            }
        }
        if let Event::KeyDown(k) = event {
            if k.key_code == KeyCode::Tab {
                field.set_key_focus(cx);
            }
        }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        if let Some(p) = props.panel.borrow_mut().as_any().downcast_mut::<AddFeed>() {
            self.view.label(cx, ids!(error_lbl)).set_text(cx, &p.error);
        }
        let step = self.view.draw_walk(cx, scope, walk);
        props.hits.add(
            "feed URL",
            self.view.text_input(cx, ids!(url_input)).area().rect(cx),
            MouseCursor::Text,
            props.slot,
        );
        let error = self.view.label(cx, ids!(error_lbl));
        if !error.text().is_empty() {
            props.hits.add_clipped(
                error.text(),
                error.area().rect(cx),
                self.view.area().rect(cx),
                MouseCursor::Text,
                props.slot,
            );
        }
        step
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct RssImportPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
}
impl Widget for RssImportPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return;
        };
        let field = self.view.text_input(cx, ids!(path_input));
        if let Event::Actions(actions) = event {
            if field.changed(actions).is_some() || field.returned(actions).is_some() {
                let mut borrow = props.panel.borrow_mut();
                let Some(p) = borrow.as_any().downcast_mut::<ImportFeeds>() else {
                    return;
                };
                p.path = field.text();
                p.error.clear();
                p.status.clear();
                if field.returned(actions).is_some() {
                    if let Some(s) = scope.data.get_mut::<Session>() {
                        p.submit(s);
                    }
                }
                self.view.redraw(cx);
            }
        }
        if let Event::KeyDown(k) = event {
            if k.key_code == KeyCode::Tab {
                field.set_key_focus(cx);
            }
        }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        if let Some(p) = props
            .panel
            .borrow_mut()
            .as_any()
            .downcast_mut::<ImportFeeds>()
        {
            self.view.label(cx, ids!(error_lbl)).set_text(cx, &p.error);
            self.view
                .label(cx, ids!(status_lbl))
                .set_text(cx, &p.status);
        }
        let step = self.view.draw_walk(cx, scope, walk);
        props.hits.add(
            "OPML path",
            self.view.text_input(cx, ids!(path_input)).area().rect(cx),
            MouseCursor::Text,
            props.slot,
        );
        for path in [ids!(error_lbl), ids!(status_lbl)] {
            let label = self.view.label(cx, path);
            if !label.text().is_empty() {
                props.hits.add_clipped(
                    label.text(),
                    label.area().rect(cx),
                    self.view.area().rect(cx),
                    MouseCursor::Text,
                    props.slot,
                );
            }
        }
        step
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct RssArticlePanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
}
impl Widget for RssArticlePanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        match event {
            Event::Actions(actions) => {
                if pictures::landed(cx, actions) {
                    self.view.redraw(cx);
                }
                let mine = self.view.widget(cx, ids!(list)).widget_uid();
                for action in actions {
                    let Some(a) = action.as_widget_action() else {
                        continue;
                    };
                    if a.group.as_ref().map(|g| g.group_uid) != Some(mine) {
                        continue;
                    }
                    if let HtmlLinkAction::Clicked { url, .. } = a.cast() {
                        cx.open_url(&url, OpenUrlInPlace::No);
                    }
                }
            }
            Event::NetworkResponses(responses) if pictures::arrived(cx, responses) => {
                self.view.redraw(cx)
            }
            _ => {}
        }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let reading = props
            .panel
            .borrow_mut()
            .as_any()
            .downcast_mut::<Article>()
            .and_then(|p| p.reading());
        let mut drawn = None;
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            let list_ref = item.as_portal_list();
            let Some(mut list) = list_ref.borrow_mut() else {
                continue;
            };
            list.set_item_range(cx, 0, 1);
            while let Some(i) = list.next_visible_item(cx) {
                if i != 0 {
                    continue;
                }
                let row = list.item(cx, i, live_id!(article));
                let (title, meta, body, url) = match &reading {
                    Some((a, body)) => (
                        a.title.clone(),
                        format!(
                            "{} · {}{}",
                            a.feed_title,
                            fmt_date(a.published),
                            if a.author.is_empty() {
                                String::new()
                            } else {
                                format!(" · {}", a.author)
                            }
                        ),
                        body.as_str(),
                        a.url.as_str(),
                    ),
                    None => (
                        "article unavailable".into(),
                        "this feed may have been removed".into(),
                        "",
                        "",
                    ),
                };
                row.label(cx, ids!(title_lbl)).set_text(cx, &title);
                row.label(cx, ids!(meta_lbl)).set_text(cx, &meta);
                let body_view = row.html(cx, ids!(body_html));
                reader::set_html(
                    cx,
                    body_view,
                    if body.trim().is_empty() {
                        "<p>This feed has no article text. Follow the original link to read it on the web.</p>"
                    } else {
                        body
                    },
                );
                let link = if url.is_empty() {
                    String::new()
                } else {
                    reader::html::sanitize(&format!(
                        "<p><a href=\"{}\">open original ↗</a></p>",
                        url.replace('&', "&amp;").replace('"', "&quot;")
                    ))
                };
                let original = row.html(cx, ids!(original_html));
                reader::set_html(cx, original, &link);
                row.widget(cx, ids!(original_html))
                    .set_visible(cx, !url.is_empty());
                row.draw_all(cx, scope);
                drawn = Some(row);
            }
        }
        let pics = pictures::link_rects(cx);
        if let Some(row) = drawn {
            let clip = self.view.widget(cx, ids!(list)).area().rect(cx);
            let title = row.label(cx, ids!(title_lbl));
            props.hits.add_clipped(
                title.text(),
                title.area().rect(cx),
                clip,
                MouseCursor::Text,
                props.slot,
            );
            for (path, label) in [
                (ids!(body_html), "article body"),
                (ids!(original_html), "open original"),
            ] {
                let widget = row.widget(cx, path);
                if !widget.visible() || !widget.area().is_valid(cx) {
                    continue;
                }
                let area = widget.area().rect(cx);
                props
                    .hits
                    .add_clipped(label, area, clip, MouseCursor::Text, props.slot);
                for rect in reader::link_runs(cx, &row, path, area, &pics) {
                    props
                        .hits
                        .add_clipped("link", rect, clip, MouseCursor::Hand, props.slot);
                }
            }
        }
        DrawStep::done()
    }
}
