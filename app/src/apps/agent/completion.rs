//! The *add panel* field's completion: the panels that are open, by title.

use kernel::richtable::{Completion, Suggestion, MAX_SUGGESTIONS};
use kernel::store::Store;

/// The open panels as a completion — the whole field is one value, so the
/// context is what has been typed and a pick replaces all of it.
///
/// The offer is carried rather than read: a title comes off a panel
/// instance, which is the session's, and a completion is handed a store. So
/// the widget takes the list on the draw it offers it, which is also the
/// only moment it could be right.
pub struct PanelPick {
    /// Every pickable panel: the title it reads as, and where it stands —
    /// *ws 2* — in the order the workspaces hold them.
    pub open: Vec<(String, String)>,
}

impl Completion for PanelPick {
    /// What has been typed, folded for matching. The whole field: a panel
    /// is named by its title, and a title has spaces in it.
    type Ctx = String;

    fn context(&self, text: &str, cursor: usize) -> Option<String> {
        let mut cursor = cursor.min(text.len());
        while !text.is_char_boundary(cursor) {
            cursor -= 1;
        }
        Some(text[..cursor].trim_start().to_lowercase())
    }

    /// The titles that begin with what was typed, then the ones that merely
    /// have it in them: *inb* is the inbox, and *box* still finds it.
    /// Nothing typed offers everything, which is the list itself.
    fn offer(&self, _store: &Store, ctx: &String) -> Vec<Suggestion> {
        let mut out: Vec<Suggestion> = Vec::new();
        for lead in [true, false] {
            for (title, at) in &self.open {
                let folded = title.to_lowercase();
                let hit = if lead {
                    folded.starts_with(ctx.as_str())
                } else {
                    !folded.starts_with(ctx.as_str()) && folded.contains(ctx.as_str())
                };
                if !hit || out.iter().any(|s| s.value == *title) {
                    continue;
                }
                out.push(Suggestion {
                    value: title.clone(),
                    label: title.clone(),
                    describe: at.clone(),
                });
            }
        }
        out.truncate(MAX_SUGGESTIONS);
        out
    }

    /// A pick is the whole line: there is one value in this field.
    fn splice(
        &self,
        _text: &str,
        _cursor: usize,
        _ctx: &String,
        pick: &Suggestion,
    ) -> (String, usize) {
        (pick.value.clone(), pick.value.len())
    }
}
