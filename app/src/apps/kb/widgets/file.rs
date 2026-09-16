//! A file's card, drawn: the shell's card over what the row says, the
//! muted line saying where the bytes are, and the pages that name it.

use kernel::nav::Nav;
use kernel::session::Session;
use makepad_widgets::*;

use crate::shell::dsl::SLinkWidgetRefExt;
use crate::shell::hosted::PanelProps;
use crate::shell::widgets::card::{self, CardData};

use super::super::model::Where;
use super::super::panels::{File, Page};
use super::{text_hit, with};

/// The link slots the card has for the pages that name it.
const PAGE_LINKS: [&[LiveId]; 4] = [ids!(pages_row.p0), ids!(pages_row.p1), ids!(pages_row.p2), ids!(pages_row.p3)];

#[derive(Script, ScriptHook, Widget)]
pub struct FilePanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    /// Which file, in which state, the card was last filled for: the
    /// viewer decodes once per fill.
    #[rust]
    shown: Option<(String, Where)>,
}

impl Widget for FilePanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if let Some(props) = scope.props.get::<PanelProps>().cloned() {
            if let Some(s) = scope.data.get_mut::<Session>() {
                if with::<File, _>(&props, |f| f.poll(s)).unwrap_or(false) {
                    self.view.redraw(cx);
                }
            }
        }
        self.view.handle_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        if let Some(s) = scope.data.get_mut::<Session>() {
            let _ = with::<File, _>(&props, |f| f.poll(s));
        }
        let Some((path, slot, where_is, kind, when, named, control)) = with::<File, _>(&props, |f| {
            (f.path().to_string(), f.slot(), f.where_is(), f.kind_line(), f.when(), f.named_by(), f.viewer())
        }) else {
            return self.view.draw_walk(cx, scope, walk);
        };
        card::bind(cx, &self.view, control);
        let key = (path.clone(), where_is);
        if self.shown.as_ref() != Some(&key) {
            let (name, preview) = with::<File, _>(&props, |f| (f.name(), f.preview())).unwrap_or_default();
            card::fill(
                cx,
                &self.view,
                &CardData { name, kind_word: kind.0, size: kind.1, modified: when, detail: path.clone(), preview },
            );
            self.shown = Some(key);
        }
        let where_lbl = self.view.label(cx, ids!(where_lbl));
        where_lbl.set_text(cx, where_is.line());
        let pages = self.view.widget(cx, ids!(pages_row));
        pages.set_visible(cx, !named.is_empty());
        for (i, link) in PAGE_LINKS.iter().enumerate() {
            let w = self.view.widget(cx, link);
            match named.get(i) {
                Some((slug, title)) => {
                    w.set_visible(cx, true);
                    w.as_slink().set(cx, title, Nav::Open { from: slot, id: Page::id(slug), fresh: false }, false, None);
                }
                None => w.set_visible(cx, false),
            }
        }
        let step = self.view.draw_walk(cx, scope, walk);
        text_hit(cx, &props, &where_lbl, None);
        let r = self.view.widget(cx, ids!(detail_txt)).area().rect(cx);
        if r.size.x > 0.0 && r.size.y > 0.0 {
            props.hits.add(path, r, MouseCursor::Text, props.slot);
        }
        step
    }
}
