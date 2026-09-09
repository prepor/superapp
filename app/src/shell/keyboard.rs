//! Text widgets own their pointer presses and editing chords across all apps.
//! Both shortcut dispatch and accelerator marks read their live selection.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use makepad_widgets::*;
use makepad_widgets::makepad_platform::event::TouchState;
use makepad_widgets::widget::WidgetWeakRef;

use super::keys::Letters;
use super::widgets::source_input;

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
            let editable = input.as_ref().is_some_and(|input| !input.is_read_only())
                || widget.borrow::<source_input::SourceInput>().is_some_and(|input| !input.is_read_only());
            if let Some((_, letters)) = self.policies.borrow().get(&widget.widget_uid()) {
                return Some(Owner { letters: *letters, editable });
            }
            if editable {
                return Some(Owner { letters: Letters::ALL, editable });
            }
            if let Some(selected) = selection(widget) {
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

/// `Some` identifies a native text widget, including an empty selection.
fn selection(widget: &WidgetRef) -> Option<bool> {
    widget.borrow::<TextInput>().map(|text| !text.selected_text().is_empty())
        .or_else(|| widget.borrow::<source_input::SourceInput>().map(|text| !text.selected_text().is_empty()))
        .or_else(|| widget.borrow::<TextFlow>().map(|text| text.has_selection()))
        .or_else(|| widget.borrow::<Html>().map(|text| text.has_selection()))
        .or_else(|| widget.borrow::<Markdown>().map(|text| text.has_selection()))
        .or_else(|| widget.borrow::<super::widgets::viewer::canvas::ViewerImage>().and_then(|text| text.text_selection()))
}

/// Walk only visible text. A hidden row variant cannot own the keyboard.
pub(super) fn find_text(root: &WidgetRef, matches: &impl Fn(&WidgetRef) -> bool) -> Option<WidgetRef> {
    if !root.visible() { return None; }
    if selection(root).is_some() && matches(root) { return Some(root.clone()); }
    let mut found = None;
    root.children(&mut |_, child| {
        if found.is_none() { found = find_text(&child, matches); }
    });
    found
}

/// Run after panel and shell handlers: the text that captured the gesture
/// keeps focus on both press and release. A previously focused input can
/// otherwise blur on release and overwrite the new text's focus request.
/// No hit rectangles or panel-specific registrations are involved.
pub(super) fn focus_captured(cx: &mut Cx, root: &WidgetRef, event: &Event) {
    match event {
        Event::MouseDown(e) if e.button == MouseButton::PRIMARY => {}
        Event::MouseUp(e) if e.button == MouseButton::PRIMARY => {}
        Event::TouchUpdate(e) if e.touches.iter().any(|t| matches!(t.state, TouchState::Start | TouchState::Stop)) => {}
        _ => return,
    }
    // Scroll containers can also capture the press after their child does.
    // The final handled area can therefore be the list, while the original
    // text capture still owns the selection drag.
    if let Some(text) = find_text(root, &|widget| cx.fingers.is_area_captured(widget.area())) {
        cx.set_key_focus(text.area());
    }
}

/// Row variants have matching child ids. Move their native text widgets
/// instead of recreating focus, selection and pointer capture in a new one.
pub(super) fn retain_text(cx: &mut Cx, from: &WidgetRef, to: &WidgetRef) {
    use makepad_widgets::widget_tree::CxWidgetExt;

    if from == to { return; }
    if let (Some(mut from), Some(mut to)) = (from.borrow_mut::<View>(), to.borrow_mut::<View>()) {
        let mut changed = false;
        for (id, source) in &mut from.children {
            let Some((_, target)) = to.children.iter_mut().find(|(next, _)| next == id) else { continue };
            if selection(source).is_some() && selection(target).is_some() {
                std::mem::swap(source, target);
                changed = true;
            } else {
                retain_text(cx, source, target);
            }
        }
        if changed {
            cx.widget_tree_mark_dirty(from.widget_uid());
            cx.widget_tree_mark_dirty(to.widget_uid());
        }
        return;
    }
    // Custom containers can expose nested Views through the widget interface.
    let mut targets = HashMap::new();
    to.children(&mut |id, child| { targets.insert(id, child); });
    from.children(&mut |id, child| {
        if let Some(target) = targets.get(&id) { retain_text(cx, &child, target); }
    });
}
