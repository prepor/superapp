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

struct Owner {
    letters: Letters,
    editable: bool,
}

impl Keyboard {
    pub fn keep(&self, widget: &WidgetRef, letters: Letters) {
        let mut policies = self.policies.borrow_mut();
        policies.retain(|_, (widget, _)| widget.upgrade().is_some());
        policies.insert(widget.widget_uid(), (widget.downgrade(), letters.plus(Letters::TEXT)));
    }

    /// No geometry, and no draw needed between a focus change and this read.
    pub fn kept(&self, cx: &Cx, root: &WidgetRef) -> Letters {
        self.find(cx, root).map_or(Letters::NONE, |owner| owner.letters)
    }

    /// Text undo belongs to an editable input regardless of its letter
    /// policy or whether it has any edits left to undo.
    pub fn editing(&self, cx: &Cx, root: &WidgetRef) -> bool {
        self.find(cx, root).is_some_and(|owner| owner.editable)
    }

    fn find(&self, cx: &Cx, widget: &WidgetRef) -> Option<Owner> {
        if cx.key_focus() == Area::Empty || !widget.visible() {
            return None;
        }
        if widget.key_focus(cx) {
            let input = widget.borrow::<TextInput>();
            let editable = input.as_ref().is_some_and(|input| !input.is_read_only());
            if let Some((_, letters)) = self.policies.borrow().get(&widget.widget_uid()) {
                return Some(Owner { letters: *letters, editable });
            }
            if input.is_some() {
                return Some(Owner {
                    letters: if editable { Letters::ALL } else { Letters::TEXT },
                    editable,
                });
            }
            let selection = widget.borrow::<TextFlow>().map(|text| text.has_selection())
                .or_else(|| widget.borrow::<Html>().map(|text| text.has_selection()))
                .or_else(|| widget.borrow::<Markdown>().map(|text| text.has_selection()));
            if let Some(selected) = selection {
                // A collapsed selection leaves copy available to the panel.
                return Some(Owner {
                    letters: if selected { Letters::TEXT } else { Letters::TEXT.minus(Letters::of(&['c'])) },
                    editable: false,
                });
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
