//! A page, drawn: the title, the muted line, the body through the shared
//! `Html` widget, then under rules the links, the backlinks and the files.

use kernel::nav::Nav;
use kernel::session::Session;
use makepad_widgets::*;

use crate::reader::{self, pictures, HtmlContent};
use crate::shell::dsl::SLinkWidgetRefExt;
use crate::shell::hosted::PanelProps;
use crate::shell::keys::Letters;

use super::super::markdown::LinkKind;
use super::super::model::{Link, Target};
use super::super::panels::{File, Page};
use super::{file_pictures, follow, text_hit, with};

/// One row of the reading's list.
enum Block {
    Body,
    Head(&'static str),
    Link { text: String, nav: Nav, dotted: bool },
    Muted(String),
}

#[derive(Script, ScriptHook, Widget)]
pub struct PagePanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[rust]
    body: HtmlContent,
    #[rust]
    viewport: Option<Rect>,
    /// Whether the rename field has been given the title, once per raise.
    #[rust]
    primed: bool,
}

impl Widget for PagePanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return;
        };
        let rename = self.view.text_input(cx, ids!(rename_row.rename_input));
        match event {
            Event::Actions(actions) => {
                if pictures::landed(cx, actions) || reader::html_landed(actions) {
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
                        let props = props.clone();
                        follow(cx, scope, &url, |s, url| {
                            with::<Page, _>(&props, |p| p.follow(s, url)).unwrap_or(false)
                        });
                    }
                }
                if rename.returned(actions).is_some() {
                    let text = rename.text();
                    if let Some(s) = scope.data.get_mut::<Session>() {
                        with::<Page, _>(&props, |p| p.rename(s, &text));
                    }
                    self.primed = false;
                    self.view.redraw(cx);
                }
                if rename.escaped(actions) {
                    with::<Page, _>(&props, |p| p.set_renaming(None));
                    self.primed = false;
                    cx.set_key_focus(self.view.area());
                    self.view.redraw(cx);
                }
            }
            Event::NetworkResponses(responses) if pictures::arrived(cx, responses) => self.view.redraw(cx),
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let Some((page, slot, renaming, status)) = with::<Page, _>(&props, |p| {
            (p.page(), p.slot(), p.renaming().map(str::to_string), p.status().map(str::to_string))
        }) else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let title = self.view.label(cx, ids!(title_lbl));
        let meta = self.view.label(cx, ids!(meta_lbl));
        let Some(p) = page else {
            title.set_text(cx, "no such page");
            meta.set_text(cx, "it may have been deleted — undo brings it back");
            self.view.widget(cx, ids!(rename_row)).set_visible(cx, false);
            return self.view.draw_walk(cx, scope, walk);
        };
        let (meta_line, html, links, backlinks, pictures) = with::<Page, _>(&props, |pg| {
            (pg.meta(&p), pg.html(&p), pg.links(&p), pg.backlinks(&p), pg.pictures(&p))
        })
        .unwrap_or_default();
        file_pictures(cx, pictures);
        title.set_text(cx, &p.title);
        meta.set_text(cx, &meta_line);

        // The rename field stands where the title is drawn: exactly one of
        // the two is up.
        let rename = self.view.text_input(cx, ids!(rename_row.rename_input));
        let up = renaming.is_some();
        self.view.widget(cx, ids!(rename_row)).set_visible(cx, up);
        title.set_visible(cx, !up);
        if let Some(text) = &renaming {
            if !self.primed {
                rename.set_text(cx, text);
                rename.set_key_focus(cx);
                self.primed = true;
            }
            props.keyboard.keep(&self.view.widget(cx, ids!(rename_row.rename_input)), Letters::ALL);
        }
        let status_lbl = self.view.label(cx, ids!(status_lbl));
        status_lbl.set_text(cx, status.as_deref().unwrap_or(""));
        status_lbl.set_visible(cx, status.is_some());

        let mut blocks = vec![Block::Body];
        let open = |id| Nav::Open { from: slot, id, fresh: false };
        let named: Vec<&Link> = links.iter().filter(|l| l.kind != LinkKind::Image).collect();
        if !named.is_empty() {
            blocks.push(Block::Head("LINKS"));
            for l in &named {
                match &l.resolved {
                    Target::Page { slug, title, .. } => blocks.push(Block::Link { text: title.clone(), nav: open(Page::id(slug)), dotted: false }),
                    Target::File { path, .. } => blocks.push(Block::Link { text: path.clone(), nav: open(File::id(path)), dotted: false }),
                    Target::Dangling(t) => blocks.push(Block::Muted(format!("{t} — no such page"))),
                }
            }
        }
        if !backlinks.is_empty() {
            blocks.push(Block::Head("LINKED FROM"));
            for (slug, title) in backlinks.iter() {
                blocks.push(Block::Link { text: title.clone(), nav: open(Page::id(slug)), dotted: false });
            }
        }
        let files: Vec<(String, String)> = links
            .iter()
            .filter_map(|l| match &l.resolved {
                Target::File { path, .. } => Some((path.clone(), path.clone())),
                _ => None,
            })
            .collect();
        if !files.is_empty() {
            blocks.push(Block::Head("FILES"));
            let mut seen = Vec::new();
            for (path, text) in files {
                if seen.contains(&path) {
                    continue;
                }
                seen.push(path.clone());
                blocks.push(Block::Link { text, nav: open(File::id(&path)), dotted: false });
            }
        }

        pictures::set_viewport(cx, self.viewport);
        let mut drawn: Vec<(usize, WidgetRef)> = Vec::new();
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            let list_ref = item.as_portal_list();
            let Some(mut list) = list_ref.borrow_mut() else {
                continue;
            };
            list.set_item_range(cx, 0, blocks.len());
            while let Some(i) = list.next_visible_item(cx) {
                let Some(b) = blocks.get(i) else { continue };
                let row = match b {
                    Block::Body => {
                        let row = list.item(cx, i, live_id!(body));
                        let view = row.html(cx, ids!(body_html));
                        self.body.set(cx, view, &html);
                        row
                    }
                    Block::Head(cap) => {
                        let row = list.item(cx, i, live_id!(head));
                        row.label(cx, ids!(cap_lbl)).set_text(cx, cap);
                        row
                    }
                    Block::Link { text, nav, dotted } => {
                        let row = list.item(cx, i, live_id!(link));
                        row.widget(cx, ids!(link)).as_slink().set(cx, text, nav.clone(), *dotted, None);
                        row
                    }
                    Block::Muted(text) => {
                        let row = list.item(cx, i, live_id!(muted));
                        row.label(cx, ids!(muted_lbl)).set_text(cx, text);
                        row
                    }
                };
                row.draw_all(cx, scope);
                drawn.push((i, row));
            }
        }
        let pics = pictures::link_rects(cx);
        let clip = self.view.widget(cx, ids!(list)).area().rect(cx);
        text_hit(cx, &props, &title, None);
        text_hit(cx, &props, &meta, None);
        if status.is_some() {
            text_hit(cx, &props, &status_lbl, None);
        }
        if up {
            let r = rename.area().rect(cx);
            if r.size.x > 0.0 {
                props.hits.add("new title", r, MouseCursor::Text, props.slot);
            }
        }
        for (i, row) in drawn {
            match blocks.get(i) {
                Some(Block::Body) => {
                    let widget = row.widget(cx, ids!(body_html));
                    if widget.area().is_valid(cx) {
                        let area = widget.area().rect(cx);
                        props.hits.add_clipped("page body", area, clip, MouseCursor::Text, props.slot);
                        for rect in reader::link_runs(cx, &row, ids!(body_html), area, &pics) {
                            props.hits.add_clipped("link", rect, clip, MouseCursor::Hand, props.slot);
                        }
                    }
                }
                Some(Block::Head(_)) => {
                    let l = row.label(cx, ids!(cap_lbl));
                    text_hit(cx, &props, &l, Some(clip));
                }
                Some(Block::Muted(_)) => {
                    let l = row.label(cx, ids!(muted_lbl));
                    text_hit(cx, &props, &l, Some(clip));
                }
                _ => {}
            }
        }
        self.viewport = Some(clip).filter(|r| r.size.y > 0.0);
        pictures::set_viewport(cx, None);
        DrawStep::done()
    }
}
