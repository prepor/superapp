//! The `go to` field's completion.

use std::rc::Rc;

use kernel::effect::World;
use kernel::richtable::{Completion, Suggestion, MAX_SUGGESTIONS};
use kernel::store::Store;

use super::model::{fmt_size, list_in, normalize, HOME, ROOT};

/// The `go to` field as a completion: the segment under the caret,
/// matched as a prefix against the entries of the directory the segments
/// before it name — a shell's tab, in the rich table's box. A picked
/// directory lands with its slash, so the next offer opens at once; a
/// root is offered when nothing is typed yet. The listing comes through
/// the world's disk, like the panel's own.
pub struct PathCompletion {
    pub world: Rc<World>,
}

/// What the caret is in the middle of typing in a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathCtx {
    /// Where the segment starts: after the last `/` before the caret.
    pub start: usize,
    /// The directory the segments before it name; `None` before the
    /// first slash, where a root is what completes.
    pub dir: Option<String>,
    /// The segment as typed up to the caret.
    pub prefix: String,
}

impl Completion for PathCompletion {
    type Ctx = PathCtx;

    fn context(&self, text: &str, cursor: usize) -> Option<PathCtx> {
        let mut cursor = cursor.min(text.len());
        while !text.is_char_boundary(cursor) {
            cursor -= 1;
        }
        let before = &text[..cursor];
        match before.rfind('/') {
            Some(i) => {
                let dir = normalize(&before[..=i])?;
                Some(PathCtx {
                    start: i + 1,
                    dir: Some(dir),
                    prefix: before[i + 1..].to_string(),
                })
            }
            None => Some(PathCtx {
                start: 0,
                dir: None,
                prefix: before.to_string(),
            }),
        }
    }

    fn offer(&self, _store: &Store, ctx: &PathCtx) -> Vec<Suggestion> {
        let Some(dir) = &ctx.dir else {
            // Before a slash: the two roots, as far as they match.
            return [(HOME, "~/"), (ROOT, ROOT)]
                .iter()
                .filter(|(r, _)| ctx.prefix.is_empty() || r.starts_with(ctx.prefix.as_str()))
                .map(|(_, v)| Suggestion::value(*v))
                .collect();
        };
        let prefix = ctx.prefix.to_lowercase();
        let hidden = prefix.starts_with('.');
        let mut out: Vec<Suggestion> = list_in(&self.world, dir)
            .unwrap_or_default()
            .into_iter()
            .filter(|e| hidden || !e.hidden())
            .filter(|e| e.name.to_lowercase().starts_with(&prefix))
            .map(|e| {
                let label = e.label();
                let describe = if e.is_dir {
                    String::new()
                } else {
                    fmt_size(e.size)
                };
                Suggestion {
                    value: label.clone(),
                    label,
                    describe,
                }
            })
            .collect();
        out.truncate(MAX_SUGGESTIONS);
        out
    }

    fn splice(
        &self,
        text: &str,
        cursor: usize,
        ctx: &PathCtx,
        pick: &Suggestion,
    ) -> (String, usize) {
        let cursor = cursor.min(text.len()).max(ctx.start);
        let out = format!("{}{}{}", &text[..ctx.start], pick.value, &text[cursor..]);
        (out, ctx.start + pick.value.len())
    }
}
