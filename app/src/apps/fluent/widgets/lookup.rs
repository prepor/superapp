//! One word looked up, drawn: the word, what it means, the grammar under
//! it, and the line that says which of the three answered.

use kernel::session::Session;
use makepad_widgets::*;

use crate::shell::hosted::PanelProps;

use super::super::panels::{Found, Lookup};
use super::{dots, run_hit, with};

#[derive(Script, ScriptHook, Widget)]
pub struct LookupPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    /// The frame asked for while the word is still unresolved: the panel
    /// resolves itself on an event, and a still screen sends none.
    #[rust]
    next_frame: NextFrame,
}

impl Widget for LookupPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return;
        };
        // The deck and the cache answer here; a word neither knows goes to
        // the tutor and comes back on a later event.
        if let Some(s) = scope.data.get_mut::<Session>() {
            with::<Lookup, _>(&props, |p| p.poll(s));
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let Some((term, found)) = with::<Lookup, _>(&props, |p| (p.shown_term(), p.found())) else {
            return self.view.draw_walk(cx, scope, walk);
        };

        self.view.widget(cx, ids!(term_txt)).set_text(cx, &term);
        let entry = found.entry().cloned().unwrap_or_default();
        let back = self.view.widget(cx, ids!(back_txt));
        back.set_text(cx, &entry.translation);
        self.view
            .widget(cx, ids!(back_wrap))
            .set_visible(cx, !entry.translation.trim().is_empty());
        let grammar = dots(&[entry.pos.clone(), entry.note.clone()]);
        let gram = self.view.label(cx, ids!(gram_lbl));
        gram.set_visible(cx, !grammar.is_empty());
        gram.set_text(cx, &grammar);

        let source = self.view.label(cx, ids!(source_lbl));
        source.set_visible(cx, !found.source().is_empty());
        source.set_text(cx, found.source());
        let why = match &found {
            Found::Failed(why) => why.clone(),
            _ => String::new(),
        };
        let error = self.view.label(cx, ids!(error_lbl));
        error.set_visible(cx, !why.is_empty());
        error.set_text(cx, &why);

        let step = self.view.draw_walk(cx, scope, walk);
        let clip = self.view.area().rect(cx);
        for path in [ids!(term_txt) as &[LiveId], ids!(back_txt)] {
            let w = self.view.widget(cx, path);
            if w.text().trim().is_empty() {
                continue;
            }
            run_hit(cx, &props, &w, Some(clip));
        }
        for path in [ids!(gram_lbl) as &[LiveId], ids!(source_lbl), ids!(error_lbl)] {
            let l = self.view.label(cx, path);
            if l.visible() {
                super::text_hit(cx, &props, &l, Some(clip));
            }
        }
        // Nothing has been asked yet: keep the events coming until the
        // panel's own poll has run, so a word resolves on a still screen.
        if found == Found::Fresh {
            self.next_frame = cx.new_next_frame();
        }
        step
    }
}
