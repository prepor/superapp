//! Keyboard ownership is read from live widgets, never inferred from their
//! rectangles or remembered from an earlier draw. Both shortcut dispatch and
//! the bar's accelerator marks read this same policy.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use makepad_widgets::*;
use makepad_widgets::widget::WidgetWeakRef;

use super::keys::Letters;

/// Ordinary inputs keep all letter chords; selectable text keeps the text
/// chords. A composer can declare a narrower policy for its input once,
/// without reporting focus or intercepting shortcuts on every event.
#[derive(Clone, Default)]
pub struct Keyboard {
    policies: Rc<RefCell<HashMap<WidgetUid, (WidgetWeakRef, Letters)>>>,
}

impl Keyboard {
    pub fn keep(&self, widget: &WidgetRef, letters: Letters) {
        let mut policies = self.policies.borrow_mut();
        policies.retain(|_, (widget, _)| widget.upgrade().is_some());
        policies.insert(widget.widget_uid(), (widget.downgrade(), letters.plus(Letters::TEXT)));
    }

    /// No geometry, and no draw needed between a focus change and this read.
    pub fn kept(&self, cx: &Cx, root: &WidgetRef) -> Letters {
        if cx.key_focus() == Area::Empty {
            return Letters::NONE;
        }
        self.find(cx, root).unwrap_or(Letters::NONE)
    }

    fn find(&self, cx: &Cx, widget: &WidgetRef) -> Option<Letters> {
        if !widget.visible() {
            return None;
        }
        if widget.key_focus(cx) {
            if let Some((_, letters)) = self.policies.borrow().get(&widget.widget_uid()) {
                return Some(*letters);
            }
            if let Some(input) = widget.borrow::<TextInput>() {
                return Some(if input.is_read_only() { Letters::TEXT } else { Letters::ALL });
            }
            if widget.borrow::<TextFlow>().is_some()
                || widget.borrow::<Html>().is_some()
                || widget.borrow::<Markdown>().is_some()
            {
                return Some(Letters::TEXT);
            }
        }
        let mut found = None;
        widget.children(&mut |_, child| {
            if found.is_none() {
                found = self.find(cx, &child);
            }
        });
        found
    }
}
