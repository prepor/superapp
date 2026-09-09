use crate::shell::app_ui::{AppUi, Setup};
use crate::shell::catalog::{panel, workspace_on};
use kernel::panel::{PanelId, Tag};
use kernel::scene::Scene;
use makepad_widgets::*;

use super::widget::TerminalView;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.TerminalPanel = set_type_default() do #(TerminalView::register_widget(vm)) {
        width: Fill, height: Fill
        draw_fill +: { color: #181b20 }
        draw_text +: { text_style: mod.widgets.SMonoStyle{font_size: 10.5}, color: #dce0e6 }
        draw_bold +: { text_style: mod.widgets.SMonoBoldStyle{font_size: 10.5}, color: #dce0e6 }
        draw_italic +: { text_style: mod.widgets.SMonoItalicStyle{font_size: 10.5}, color: #dce0e6 }
        draw_status +: { text_style: mod.widgets.SMonoStyle{font_size: 8.25}, color: #8a919e }
    }
}

pub struct Ui;
pub static UI: Ui = Ui;
impl AppUi for Ui {
    fn script_mod(&self, vm: &mut ScriptVm) -> ScriptValue {
        self::script_mod(vm)
    }
    fn template(&self, tag: Tag) -> Option<LiveId> {
        (tag == super::TAG).then_some(live_id!(terminal_tpl))
    }
    fn scenes(&self) -> Vec<Scene<Setup>> {
        vec![Scene::new("terminal", (720.0, 780.0))
            .node("shell", panel(|_| PanelId::bare(super::TAG), ""))
            .node("full width", workspace_on(|_| PanelId::bare(super::TAG),
                "click \"full width\"\nwait 600\ntype \"echo hello from superapp\"\nkey enter\nwait 300"))
            .sized((1440.0, 850.0))]
    }
}
