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

impl PathCompletion {
    /// The offer can decide whether Enter names an already listed directory.
    /// An unknown or stale entry goes through the panel's async lookup.
    pub fn known_dir(&self, path: &str) -> Option<bool> {
        match self.world.with_cap::<DirectoryCache, _>(|cache| {
            let known = if !cache.loaded || cache.pending.is_some() {
                None
            } else if path == cache.dir {
                Some(true)
            } else if super::model::parent(path) != Some(cache.dir.as_str()) {
                None
            } else {
                Some(
                    cache
                        .entries
                        .iter()
                        .any(|entry| entry.name == super::model::basename(path) && entry.is_dir),
                )
            };
            (cache.dir.clone(), cache.seen, known)
        }) {
            Ok((dir, seen, known)) if seen == super::FILES.seen(&self.world, &dir) => known,
            Ok(_) => None,
            Err(_) => Some(super::model::is_dir_in(&self.world, path)),
        }
    }
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
    revision: u64,
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
                let seen = super::FILES.seen(&self.world, &dir);
                let revision = self
                    .world
                    .with_cap::<DirectoryCache, _>(|cache| cache.observe(&dir, seen))
                    .unwrap_or(0);
                Some(PathCtx {
                    revision,
                    start: i + 1,
                    dir: Some(dir),
                    prefix: before[i + 1..].to_string(),
                })
            }
            None => Some(PathCtx {
                revision: 0,
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
        let entries = self
            .world
            .with_cap::<DirectoryCache, _>(|cache| cache.entries.clone())
            .unwrap_or_else(|_| list_in(&self.world, dir).unwrap_or_default());
        let mut out: Vec<Suggestion> = entries
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

/// The focused path field shares one directory snapshot across keystrokes.
/// A changed path replaces an obsolete read; its result cannot overwrite
/// the listing for the newly typed directory.
pub(super) struct DirectoryCache {
    disk: kernel::caps::DiskFactory,
    dir: String,
    seen: super::Seen,
    revision: u64,
    entries: Vec<super::model::Entry>,
    loaded: bool,
    pending: Option<tokio::sync::oneshot::Receiver<Result<Vec<super::model::Entry>, String>>>,
}

impl DirectoryCache {
    pub(super) fn new(disk: kernel::caps::DiskFactory) -> Self {
        Self {
            disk,
            dir: String::new(),
            seen: super::Seen::default(),
            revision: 0,
            entries: Vec::new(),
            loaded: false,
            pending: None,
        }
    }

    fn observe(&mut self, dir: &str, seen: super::Seen) -> u64 {
        if self.dir != dir || self.seen != seen {
            self.dir = dir.to_owned();
            self.seen = seen;
            self.entries.clear();
            self.loaded = false;
            self.revision += 1;
            let path = super::model::real_path(dir);
            self.pending = Some(super::model::read_background(
                self.disk.clone(),
                move |disk| disk.list_dir(&path),
            ));
        }
        if let Some(rx) = &mut self.pending {
            match rx.try_recv() {
                Ok(result) => {
                    self.loaded = result.is_ok();
                    self.entries = result.unwrap_or_default();
                    self.pending = None;
                    self.revision += 1;
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {}
                Err(_) => {
                    self.pending = None;
                    self.revision += 1;
                }
            }
        }
        self.revision
    }
}
