//! Reuses the same keyboard and pointer completion policy as Mail and filters.
use super::completion::Field;
use crate::shell::{hosted::PanelProps, widgets::suggest::Suggest};
use kernel::session::Session;
use makepad_widgets::*;

#[derive(Default)]
pub struct Offers {
    ac: Suggest<Field>,
    current: Option<usize>,
    picks: Vec<Rect>,
    picking: bool,
}
impl Offers {
    /// Some means the offer consumed this event; true means the field changed.
    pub fn handle(
        &mut self,
        cx: &mut Cx,
        event: &Event,
        props: &PanelProps,
        fields: &[(Field, TextInputRef)],
    ) -> Option<bool> {
        let focused = fields.iter().position(|(_, t)| t.key_focus(cx));
        if focused.is_some() && focused != self.current {
            self.current = focused;
            self.ac = Suggest::default();
            self.picks.clear();
        }
        let i = self.current?;
        let (kind, input) = &fields[i];
        self.ac.track(cx, input);
        if let Event::MouseDown(e) = event {
            if props
                .hits
                .at(e.abs)
                .is_some_and(|h| h.slot == Some(props.slot))
            {
                if let Some(i) = self.picks.iter().position(|r| r.contains(e.abs)) {
                    self.ac.pick(cx, kind, input, i);
                    if *kind != Field::Guests {
                        self.ac.dismiss(kind, input);
                    }
                    self.picking = true;
                    return Some(true);
                }
            }
        }
        if matches!(event, Event::MouseUp(_)) && self.picking {
            self.picking = false;
            return Some(false);
        }
        if let Event::KeyDown(k) = event {
            let before = input.text();
            if self.ac.key(cx, kind, input, k) {
                if *kind != Field::Guests
                    && matches!(
                        k.key_code,
                        KeyCode::ReturnKey | KeyCode::NumpadEnter | KeyCode::Tab
                    )
                {
                    self.ac.dismiss(kind, input);
                }
                return Some(before != input.text());
            }
        }
        None
    }
    pub fn draw(
        &mut self,
        cx: &mut Cx2d,
        scope: &mut Scope,
        props: &PanelProps,
        fields: &[(Field, TextInputRef)],
        suggest: &mut View,
        clip: Rect,
    ) {
        self.picks.clear();
        let Some(store) = scope.data.get::<Session>().map(|s| s.store().clone()) else {
            return;
        };
        let Some(i) = self.current else { return };
        let (kind, field) = &fields[i];
        let Some(r) = crate::shell::widgets::form::drawn_rect(cx, field.area()) else {
            return;
        };
        if r.size.x <= 0.0 || !clip.contains(r.pos + r.size * 0.5) {
            return;
        }
        self.ac.track(cx, field);
        self.ac.draw(cx, scope, &store, kind, field, suggest);
        for (label, r) in self.ac.hits(cx, suggest) {
            if let Some(r) = props
                .hits
                .add_clipped(label, r, clip, MouseCursor::Hand, props.slot)
            {
                self.picks.push(r);
            }
        }
    }
}
