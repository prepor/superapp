//! A scalar choice with stable values, keyboard navigation and a scrolling menu.
//!
//! Options are prepared by the caller; the control never loads data. Like the
//! shared completion box, the menu draws after its enclosing form. Call
//! [`handle_open`] before forwarding form events and [`draw_open`] after drawing
//! its controls, so a menu owns both the pixels and the input it covers.

use std::sync::Arc;

use makepad_widgets::*;

use super::{form, reveal::Reveal};
use crate::shell::hosted::PanelProps;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectOption {
    pub value: String,
    pub label: String,
}

impl SelectOption {
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        Self { value: value.into(), label: label.into() }
    }
}

#[derive(Clone, Debug, Default)]
pub enum SelectAction {
    Changed(String),
    #[default]
    None,
}

/// Selection and the open menu's highlight are deliberately separate: browsing
/// choices, dismissing them, or refreshing the offer never changes the value.
#[derive(Default)]
struct Choices {
    options: Arc<[SelectOption]>,
    selected: String,
    highlighted: Option<String>,
    highlighted_index: Option<usize>,
    selected_index: Option<usize>,
    open: bool,
}

impl Choices {
    fn update(&mut self, options: Arc<[SelectOption]>, selected: &str) {
        if Arc::ptr_eq(&self.options, &options) && self.selected == selected { return; }
        self.options = options;
        if self.selected != selected { selected.clone_into(&mut self.selected); }
        self.selected_index = self.options.iter().position(|o| o.value == selected);
        let highlight = self.options.iter().position(|o| Some(&o.value) == self.highlighted.as_ref())
            .or(self.selected_index).or_else(|| (!self.options.is_empty()).then_some(0));
        self.highlighted_index = highlight;
        self.highlighted = highlight.map(|i| self.options[i].value.clone());
        if self.options.is_empty() { self.open = false; }
    }

    fn index(&self) -> Option<usize> {
        self.highlighted_index
    }

    fn open(&mut self) {
        self.highlighted_index = self.selected_index.or_else(|| (!self.options.is_empty()).then_some(0));
        self.highlighted = self.highlighted_index.map(|i| self.options[i].value.clone());
        self.open = !self.options.is_empty();
    }

    fn highlight(&mut self, index: usize) {
        self.highlighted = self.options.get(index).map(|o| o.value.clone());
        self.highlighted_index = (index < self.options.len()).then_some(index);
    }

    fn commit(&mut self) -> Option<String> {
        let value = self.options.get(self.index()?)?.value.clone();
        self.open = false;
        (value != self.selected).then(|| {
            self.selected = value.clone();
            self.selected_index = self.highlighted_index;
            value
        })
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct Select {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[live]
    menu: View,
    #[rust]
    choices: Choices,
    #[rust]
    label: String,
    #[rust]
    disabled: bool,
    #[rust]
    rows: Vec<(usize, Rect)>,
    #[rust]
    pressed: Option<String>,
    #[rust]
    release: bool,
    #[rust]
    reveal: Reveal<usize>,
}

impl Select {
    fn face(&self, cx: &Cx) -> ButtonRef { self.view.button(cx, ids!(field)) }

    fn close(&mut self, cx: &mut Cx) {
        self.choices.open = false;
        self.pressed = None;
        self.rows.clear();
        self.view.redraw(cx);
    }

    fn open(&mut self, cx: &mut Cx) {
        if self.disabled { return; }
        self.choices.open();
        self.face(cx).set_key_focus(cx);
        if let Some(index) = self.choices.index() { self.reveal.request(index); }
        self.view.redraw(cx);
    }

    fn commit(&mut self, cx: &mut Cx) {
        if let Some(value) = self.choices.commit() {
            cx.widget_action(self.widget_uid(), SelectAction::Changed(value));
        }
        self.close(cx);
        self.face(cx).set_key_focus(cx);
    }

    fn key(&mut self, cx: &mut Cx, event: &Event) -> bool {
        let Event::KeyDown(key) = event else { return false };
        if !self.key_focus(cx) || self.disabled || key.modifiers.logo || key.modifiers.control
            || key.modifiers.alt { return false; }
        match key.key_code {
            KeyCode::Space | KeyCode::ReturnKey | KeyCode::NumpadEnter => {
                if self.choices.open { self.commit(cx); } else { self.open(cx); }
            }
            KeyCode::ArrowDown | KeyCode::ArrowUp | KeyCode::Home | KeyCode::End => {
                if !self.choices.open {
                    self.open(cx);
                } else if let Some(index) = self.choices.index() {
                    let next = match key.key_code {
                        KeyCode::ArrowDown => (index + 1).min(self.choices.options.len() - 1),
                        KeyCode::ArrowUp => index.saturating_sub(1),
                        KeyCode::Home => 0,
                        _ => self.choices.options.len() - 1,
                    };
                    self.choices.highlight(next);
                    self.reveal.request(next);
                    self.view.redraw(cx);
                }
            }
            KeyCode::Escape if self.choices.open => self.close(cx),
            KeyCode::Tab => {
                self.close(cx);
                return false; // The enclosing form owns the next focus stop.
            }
            _ => return false,
        }
        true
    }

    fn popup_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) -> bool {
        if self.release && matches!(event, Event::MouseUp(e) if e.button == MouseButton::PRIMARY) {
            self.release = false;
            // Another panel may have taken focus on the outside press.
            return true;
        }
        if !self.choices.open { return false; }
        if self.disabled {
            self.close(cx);
            return false;
        }
        if self.key(cx, event) { return true; }
        if matches!(event, Event::WindowLostFocus(_))
            || matches!(event, Event::KeyFocus(_)) && !self.key_focus(cx)
        {
            self.close(cx);
            return false;
        }
        match event {
            Event::MouseDown(e) if e.button == MouseButton::PRIMARY => {
                let inside = form::drawn_rect(cx, self.menu.area()).is_some_and(|r| r.contains(e.abs));
                if inside {
                    self.menu.handle_event(cx, event, scope);
                    let owner = e.handled.get();
                    if owner != Area::Empty && owner != self.menu.portal_list(cx, ids!(list)).area() {
                        // A scrollbar captured this press. It owns the whole
                        // drag; the row underneath it is not a pending choice.
                        self.pressed = None;
                        self.reveal.cancel();
                        return true;
                    }
                }
                let index = self.rows.iter().find(|(_, r)| r.contains(e.abs)).map(|(i, _)| *i);
                self.pressed = index.map(|i| self.choices.options[i].value.clone());
                if let Some(index) = index {
                    self.choices.highlight(index);
                    self.view.redraw(cx);
                } else if !inside {
                    self.close(cx);
                    self.release = true;
                }
                true
            }
            Event::MouseUp(e) if e.button == MouseButton::PRIMARY => {
                self.menu.handle_event(cx, event, scope);
                let row = self.rows.iter().find(|(_, r)| r.contains(e.abs)).map(|(i, _)| *i);
                let value = row.map(|i| &self.choices.options[i].value);
                if self.pressed.take().is_some_and(|pressed| value == Some(&pressed)) {
                    self.choices.highlight(row.unwrap());
                    self.commit(cx);
                }
                self.face(cx).set_key_focus(cx);
                true
            }
            Event::MouseMove(e) => {
                self.menu.handle_event(cx, event, scope);
                if let Some(index) = self.rows.iter().find(|(_, r)| r.contains(e.abs)).map(|(i, _)| *i) {
                    if self.choices.index() != Some(index) {
                        self.choices.highlight(index);
                        self.view.redraw(cx);
                    }
                    cx.set_cursor(MouseCursor::Hand);
                }
                true
            }
            Event::Scroll(e) => {
                if form::drawn_rect(cx, self.menu.area()).is_some_and(|r| r.contains(e.abs)) {
                    self.menu.handle_event(cx, event, scope);
                    self.reveal.cancel();
                    self.view.redraw(cx);
                    true
                } else {
                    self.close(cx);
                    false
                }
            }
            _ => false,
        }
    }

    fn draw_menu(&mut self, cx: &mut Cx2d, scope: &mut Scope, bounds: Rect, props: &PanelProps) {
        self.rows.clear();
        if !self.choices.open || self.disabled { return; }
        if !self.key_focus(cx) {
            self.close(cx);
            return;
        }
        let Some(anchor) = form::drawn_rect(cx, self.view.area()) else {
            self.close(cx);
            return;
        };
        let Some(anchor) = crate::shell::hits::visible(anchor, bounds) else {
            self.close(cx);
            return;
        };
        let height = (self.choices.options.len() as f64 * 32.0 + 4.0).min(292.0).min(bounds.size.y);
        let width = anchor.size.x.max(180.0).min(bounds.size.x);
        let x = anchor.pos.x.min(bounds.pos.x + bounds.size.x - width).max(bounds.pos.x);
        let below = anchor.pos.y + anchor.size.y + 3.0;
        let y = if below + height <= bounds.pos.y + bounds.size.y { below }
            else { (anchor.pos.y - height - 3.0).max(bounds.pos.y) };
        let menu_rect = Rect { pos: dvec2(x, y), size: dvec2(width, height) };
        let highlighted = self.choices.index();
        let mut drawn = Vec::new();
        while let Some(item) = self.menu.draw_walk(cx, scope, Walk::abs_rect(menu_rect)).step() {
            let list_ref = item.as_portal_list();
            let Some(mut list) = list_ref.borrow_mut() else { continue };
            list.set_item_range(cx, 0, self.choices.options.len());
            while let Some(index) = list.next_visible_item(cx) {
                let row = list.item(cx, index, live_id!(option));
                let Some(option) = self.choices.options.get(index) else { continue };
                let active = highlighted == Some(index);
                for (path, on) in [(ids!(normal), !active), (ids!(highlighted), active)] {
                    let line = row.view(cx, path);
                    line.set_visible(cx, on);
                    line.label(cx, ids!(label)).set_text(cx, &option.label);
                    line.view(cx, ids!(mark.check)).set_visible(cx, option.value == self.choices.selected);
                }
                row.draw_all(cx, scope);
                drawn.push((index, row));
            }
        }
        // PortalList positions backward-filled rows when its draw finishes.
        // Read final geometry so those rows cannot overlap visible choices.
        let list = self.menu.portal_list(cx, ids!(list));
        let Some(viewport) = form::drawn_rect(cx, list.area()) else { return };
        let mut highlighted_rect = None;
        props.hits.add_clipped(format!("{} choices", self.label), menu_rect, bounds, MouseCursor::Default, props.slot);
        for (index, row) in drawn {
            let Some(rect) = form::drawn_rect(cx, row.area()) else { continue };
            if highlighted == Some(index) { highlighted_rect = Some(rect); }
            if let Some(rect) = props.hits.add_clipped(format!("{}: {}", self.label, self.choices.options[index].label),
                rect, viewport, MouseCursor::Hand, props.slot) {
                self.rows.push((index, rect));
            }
        }
        self.reveal.apply(cx, &list, highlighted, highlighted_rect);
    }
}

impl Widget for Select {
    fn set_disabled(&mut self, cx: &mut Cx, disabled: bool) {
        self.disabled = disabled;
        self.view.widget(cx, ids!(field)).set_disabled(cx, disabled);
        if disabled && self.choices.open { self.close(cx); }
    }

    fn disabled(&self, _: &Cx) -> bool { self.disabled }
    fn set_key_focus(&self, cx: &mut Cx) { self.face(cx).set_key_focus(cx); }
    fn key_focus(&self, cx: &Cx) -> bool { self.face(cx).key_focus(cx) }
    fn text(&self) -> String {
        self.choices.selected_index.and_then(|i| self.choices.options.get(i))
            .map(|o| o.label.clone()).unwrap_or_default()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if self.choices.open && matches!(event, Event::NextFrame(_)) {
            self.menu.handle_event(cx, event, scope);
        }
        if self.popup_event(cx, event, scope) || self.key(cx, event) { return; }
        if let Event::MouseDown(e) = event {
            if e.button == MouseButton::PRIMARY && !self.disabled {
                let face = self.face(cx);
                if matches!(event.hits(cx, face.area()), Hit::FingerDown(_)) {
                    self.open(cx);
                    return;
                }
            }
        }
        self.view.handle_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

impl SelectRef {
    /// A missing value keeps the supplied placeholder rather than silently
    /// selecting the first option. Refreshes preserve highlighted identity.
    pub fn set_options(&self, cx: &mut Cx, label: &str, options: Arc<[SelectOption]>, selected: &str, placeholder: &str) {
        if let Some(mut this) = self.borrow_mut() {
            if !Arc::ptr_eq(&this.choices.options, &options) {
                // Until the next draw, old rectangles still depict the old
                // array. They must not address a reordered or removed option.
                this.rows.clear();
            }
            this.choices.update(options, selected);
            if this.label != label { label.clone_into(&mut this.label); }
            let text = this.choices.selected_index.and_then(|i| this.choices.options.get(i))
                .map(|o| o.label.as_str()).unwrap_or(placeholder);
            if this.face(cx).text() != text { this.face(cx).set_text(cx, text); }
        }
    }

    pub fn changed(&self, actions: &Actions) -> Option<String> {
        match actions.find_widget_action(self.widget_uid()).cast() {
            SelectAction::Changed(value) => Some(value),
            SelectAction::None => None,
        }
    }
}

/// Open menus get first refusal, including the release after dismissing one.
pub fn handle_open(cx: &mut Cx, event: &Event, scope: &mut Scope, controls: &[SelectRef]) -> bool {
    controls.iter().any(|control| control.borrow_mut().is_some_and(|mut c| c.popup_event(cx, event, scope)))
}

pub fn draw_open(cx: &mut Cx2d, scope: &mut Scope, bounds: Rect, props: &PanelProps, controls: &[SelectRef]) {
    for control in controls {
        if let Some(mut control) = control.borrow_mut() { control.draw_menu(cx, scope, bounds, props); }
    }
}

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.SSelectLine = View {
        width: Fill, height: 32, flow: Right, align: Align{y: 0.5}, spacing: 6
        padding: Inset{left: 7, right: 7}
        mark := View { width: 12, height: 12
            check := View { width: Fill, height: Fill, visible: false, show_bg: true
                draw_bg +: {
                    pixel: fn() {
                        let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                        sdf.move_to(1.5, 6.0)
                        sdf.line_to(4.5, 9.0)
                        sdf.line_to(10.5, 2.5)
                        sdf.stroke(#141414, 1.5)
                        return sdf.result
                    }
                }
            }
        }
        label := mod.widgets.SLabel { width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis }
    }
    mod.widgets.SSelect = set_type_default() do #(Select::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fit
        field := mod.widgets.SBtn {
            width: Fill, align: Align{x: 0.0, y: 0.5}
            padding: Inset{left: 7, right: 25, top: 5, bottom: 5}
            text: "choose…"
            draw_bg +: {
                border_color: #dcdcdc, border_color_hover: #909090, border_color_focus: #141414
                pixel: fn() {
                    let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                    let fill = self.color.mix(self.color_hover, self.hover).mix(self.color_down, self.down)
                        .mix(self.color_disabled, self.disabled)
                    let border = self.border_color.mix(self.border_color_hover, self.hover)
                        .mix(self.border_color_focus, self.focus).mix(self.border_color_disabled, self.disabled)
                    sdf.box(0.5, 0.5, self.rect_size.x - 1.0, self.rect_size.y - 1.0, 1.0)
                    sdf.fill_keep(fill)
                    sdf.stroke(border, 1.0)
                    let c = vec2(self.rect_size.x - 12.0, self.rect_size.y * 0.5)
                    sdf.move_to(c.x - 3.0, c.y - 1.5)
                    sdf.line_to(c.x, c.y + 1.5)
                    sdf.line_to(c.x + 3.0, c.y - 1.5)
                    sdf.stroke(#5a5a5a, 1.0)
                    return sdf.result
                }
            }
        }
        menu: View {
            width: Fill, height: Fill, flow: Down, padding: 2
            show_bg: true
            draw_bg +: { color: #ffffff
                pixel: fn() {
                    let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                    sdf.box(0.5, 0.5, self.rect_size.x - 1.0, self.rect_size.y - 1.0, 1.0)
                    sdf.fill_keep(#ffffff)
                    sdf.stroke(#909090, 1.0)
                    return sdf.result
                }
            }
            list := mod.widgets.SList {
                width: Fill, height: Fill, flow: Down, reuse_items: true, grab_key_focus: false
                option := View {
                    width: Fill, height: Fit, flow: Down
                    normal := mod.widgets.SSelectLine {}
                    highlighted := mod.widgets.SSelectLine {
                        visible: false, show_bg: true
                        draw_bg +: { color: #e7e7e7
                            pixel: fn() { return vec4(self.color.xyz * self.color.w, self.color.w) }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "select_tests.rs"]
mod widget_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn options(values: &[&str]) -> Arc<[SelectOption]> {
        values.iter().map(|value| SelectOption::new(*value, format!("Label {value}"))).collect()
    }

    #[test]
    fn browsing_and_refreshing_preserve_value_and_highlight_identity() {
        let mut choices = Choices::default();
        choices.update(options(&["work", "personal", "team"]), "personal");
        choices.open();
        assert_eq!(choices.index(), Some(1));
        choices.highlight(2);
        choices.update(options(&["team", "work", "personal"]), "personal");
        assert!(choices.open);
        assert_eq!(choices.index(), Some(0));
        assert_eq!(choices.selected, "personal");
        assert_eq!(choices.commit().as_deref(), Some("team"));
        assert!(!choices.open);
    }

    #[test]
    fn removed_choices_and_missing_values_never_commit_a_stale_index() {
        let mut choices = Choices::default();
        choices.update(options(&["work", "personal"]), "work");
        choices.open();
        choices.highlight(1);
        choices.update(options(&["work"]), "work");
        assert_eq!(choices.commit(), None);
        choices.update(options(&[]), "disconnected");
        choices.open();
        assert!(!choices.open);
        assert_eq!(choices.selected, "disconnected");
        assert_eq!(choices.commit(), None);
    }
}
