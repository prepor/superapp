//! Shared form input policy: Tab/Shift-Tab walk visible fields, including
//! fields whose controls live in a scrolling form rather than a panel root.
use makepad_widgets::*;
pub fn tab(cx: &mut Cx, event: &Event, inputs: &[TextInputRef]) -> Option<usize> {
    let Event::KeyDown(k) = event else {
        return None;
    };
    if k.key_code != KeyCode::Tab || inputs.is_empty() {
        return None;
    }
    let focused = inputs.iter().position(|t| t.key_focus(cx))?;
    let n = inputs.len();
    let next = if k.modifiers.shift {
        (focused + n - 1) % n
    } else {
        (focused + 1) % n
    };
    inputs[next].set_key_focus(cx);
    if let Some(mut field) = inputs[next].borrow_mut() {
        field.select_all(cx);
    }
    Some(next)
}

/// Keep the focused field visible in a form stored in one portal-list item.
pub fn reveal(cx: &mut Cx, list: &PortalListRef, input: &TextInputRef) {
    let viewport = list.area().rect(cx);
    let field = input.area().rect(cx);
    let delta = if field.pos.y < viewport.pos.y {
        viewport.pos.y - field.pos.y + 8.0
    } else if field.pos.y + field.size.y > viewport.pos.y + viewport.size.y {
        viewport.pos.y + viewport.size.y - field.pos.y - field.size.y - 8.0
    } else {
        return;
    };
    if let Some(mut list) = list.borrow_mut() {
        let scroll = list.first_scroll() + delta;
        list.set_first_id_and_scroll(0, scroll);
        list.redraw(cx);
    }
}

/// Keep a click's focus across a redraw between press and release. An older
/// field's outside-release handler must not clear the field just selected.
#[derive(Default)]
pub struct ClickFocus {
    pressed: Option<usize>,
}
impl ClickFocus {
    pub fn handle(
        &mut self,
        cx: &mut Cx,
        event: &Event,
        props: &crate::shell::hosted::PanelProps,
        inputs: &[TextInputRef],
        labels: &[&str],
    ) {
        let position = match event {
            Event::MouseDown(e) => e.abs,
            Event::MouseUp(e) => e.abs,
            _ => return,
        };
        let target = props
            .hits
            .at(position)
            .filter(|h| h.slot == Some(props.slot))
            .and_then(|h| labels.iter().position(|label| *label == h.label));
        if matches!(event, Event::MouseDown(_)) {
            self.pressed = target;
            if let Some(i) = target {
                inputs[i].set_key_focus(cx);
            }
        } else if self.pressed.take() == target {
            if let Some(i) = target {
                inputs[i].set_key_focus(cx);
            }
        }
    }
}
