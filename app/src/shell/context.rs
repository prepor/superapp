//! `cmd+i`: the focused panel's context, to the clipboard.
//!
//! What a panel is, what it was asked for, and every query its last draw ran
//! — provenance by construction, since the trace is opened and closed around
//! that draw by [`draw`](super::draw) rather than declared by hand. The
//! agent handoff this feeds is future work; the surface is ready.
//!
//! The copy is an effect, so a world that may not touch a human's clipboard
//! refuses it out loud instead of quietly doing it.

use kernel::caps::Clip;

use super::stage::{Shell, Stage};

impl Stage {
    /// Serializes the focused panel's context and delivers it.
    pub(super) fn copy_panel_context(&mut self, sh: &mut Shell) {
        let Some(slot) = sh.session.focus() else {
            sh.session.notify("no focused panel", false);
            return;
        };
        let Some(inst) = sh.session.panel(slot) else {
            return;
        };
        let (id, title) = {
            let p = inst.borrow();
            (p.id().clone(), p.title())
        };
        let ws = sh.session.ws().ws_of(slot).map_or(0, |k| k + 1);
        let entries = sh.session.store().trace_of(slot);

        let mut md = String::new();
        md.push_str("# superapp panel context\n\n");
        md.push_str(&format!("panel: “{title}” — workspace {ws}\n"));
        md.push_str(&format!("kind: {}\n", id.tag.as_str()));
        if !id.args.is_empty() {
            md.push_str(&format!("params: {}\n", id.args.join(", ")));
        }
        md.push_str(&format!(
            "\n## queries (last draw — {} of them)\n",
            entries.len()
        ));
        for e in &entries {
            md.push_str(&format!("\n### {} — {}\n", e.id, e.describe));
            if !e.params.is_empty() {
                md.push_str(&format!("params: {}\n", e.params));
            }
            md.push_str(&format!("rows: {}\n", e.rows));
            let sql: String = e.sql.split_whitespace().collect::<Vec<_>>().join(" ");
            md.push_str(&format!("```sql\n{sql}\n```\n"));
        }

        sh.session.world().try_run(&Clip {
            text: &md,
            what: "panel context",
        });
        let n = entries.len();
        sh.session
            .notify(format!("panel context copied — {n} queries"), false);
    }
}
