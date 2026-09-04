//! The agent's Makepad half.
//!
//! A placeholder, and deliberately so: this phase is the engine — the rows,
//! the runs, the worker and the model behind the gateway — and nothing of
//! it draws. What is here is the least the shell asks for, which is a
//! template per registered tag: the boot check refuses a panel nothing can
//! draw, and a kind with no widget behind it would stop the process rather
//! than come up blank.
//!
//! The two templates are declared in [`root`](crate::root), as every app's
//! are, as bare views. The transcript, the composer and the list go in
//! their place.

use kernel::panel::Tag;
use makepad_widgets::*;

use crate::shell::app_ui::AppUi;

use super::panels::{Agents, Chat};

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*
}

/// The agent's Makepad half.
pub struct Ui;

/// The one in this build.
pub static UI: Ui = Ui;

impl AppUi for Ui {
    fn script_mod(&self, vm: &mut ScriptVm) -> ScriptValue {
        self::script_mod(vm)
    }

    /// Two tags, two templates: a conversation, and the list of them.
    fn template(&self, tag: Tag) -> Option<LiveId> {
        match tag {
            Chat::TAG => Some(live_id!(agent_chat_tpl)),
            Agents::TAG => Some(live_id!(agent_agents_tpl)),
            _ => None,
        }
    }
}
