//! A document's queued I/O survives its panel. Edits coalesce between writes;
//! explicit saves are ordering barriers and always retain a draft first.
use super::{file_text, markdown, model};
use kernel::app::WorldFactory;
use kernel::effect::{MemEffect, World};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use tokio::sync::{mpsc, watch};

#[derive(Clone)]
pub(super) enum Source {
    Note(i64),
    File(String),
    Invalid,
}

pub(super) struct Snapshot {
    pub version: u64,
    pub sequence: u64,
    pub text: String,
    pub original: String,
    pub baseline: String,
    pub spans: Vec<crate::shell::widgets::source_input::Span>,
    pub error: String,
    pub available: bool,
    pub save: SaveOutcome,
}

/// A watch receiver may skip intermediate presentations. Keep the latest
/// explicit save's completion and outcome in every subsequent snapshot.
#[derive(Clone, Default)]
pub(super) struct SaveOutcome {
    pub sequence: u64,
    pub error: String,
    pub written: bool,
}

enum Command {
    Barrier(tokio::sync::oneshot::Sender<()>),
    Edit {
        sequence: u64,
        text: String,
        now: f64,
    },
    Save {
        sequence: u64,
        text: String,
        now: f64,
    },
    Reload {
        sequence: u64,
    },
}

struct Service {
    db: Weak<kernel::store::Db>,
    commands: mpsc::WeakUnboundedSender<Command>,
    done: watch::Receiver<bool>,
}

fn services() -> &'static Mutex<Vec<Service>> {
    static SERVICES: OnceLock<Mutex<Vec<Service>>> = OnceLock::new();
    SERVICES.get_or_init(Mutex::default)
}

/// A barrier for every document on this store. The registry holds weak
/// senders, so closing the last panel still drains and retires its service.
pub(super) async fn flush(db: Arc<kernel::store::Db>) {
    let running = {
        let mut services = services().lock().expect("document services");
        services.retain(|service| service.db.strong_count() > 0 && !*service.done.borrow());
        services
            .iter()
            .filter(|service| {
                service
                    .db
                    .upgrade()
                    .is_some_and(|owner| Arc::ptr_eq(&owner, &db))
            })
            .map(|service| (service.commands.clone(), service.done.clone()))
            .collect::<Vec<_>>()
    };
    let mut barriers = Vec::new();
    for (commands, mut done) in running {
        let barrier = commands.upgrade().and_then(|commands| {
            let (acknowledge, received) = tokio::sync::oneshot::channel();
            commands
                .send(Command::Barrier(acknowledge))
                .ok()
                .map(|()| received)
        });
        barriers.push(async move {
            if let Some(barrier) = barrier {
                let _ = barrier.await;
            } else {
                while !*done.borrow_and_update() {
                    if done.changed().await.is_err() {
                        break;
                    }
                }
            }
        });
    }
    futures_util::future::join_all(barriers).await;
}

pub(super) struct Handle {
    commands: mpsc::UnboundedSender<Command>,
    snapshots: watch::Receiver<Option<Arc<Snapshot>>>,
    seen: u64,
}

impl Handle {
    pub fn start(factory: WorldFactory, db: Arc<kernel::store::Db>, source: Source) -> Self {
        let (commands, receiver) = mpsc::unbounded_channel();
        let (updates, snapshots) = watch::channel(None);
        let (completed, done) = watch::channel(false);
        services().lock().expect("document services").push(Service {
            db: Arc::downgrade(&db),
            commands: commands.downgrade(),
            done,
        });
        kernel::runtime::spawn_local(move || async move {
            run(factory, source, receiver, updates).await;
            completed.send_replace(true);
        });
        Self {
            commands,
            snapshots,
            seen: 0,
        }
    }

    pub fn edit(&self, sequence: u64, text: String, now: f64) {
        let _ = self.commands.send(Command::Edit {
            sequence,
            text,
            now,
        });
    }

    pub fn save(&self, sequence: u64, text: String, now: f64) {
        let _ = self.commands.send(Command::Save {
            sequence,
            text,
            now,
        });
    }

    pub fn reload(&self, sequence: u64) {
        let _ = self.commands.send(Command::Reload { sequence });
    }

    pub fn poll(&mut self) -> Option<Arc<Snapshot>> {
        let snapshot = self.snapshots.borrow_and_update().clone()?;
        if snapshot.version == self.seen {
            return None;
        }
        self.seen = snapshot.version;
        Some(snapshot)
    }
}

fn publish(updates: &watch::Sender<Option<Arc<Snapshot>>>, snapshot: Snapshot) {
    // Retain only the latest presentation. Accepted commands keep running
    // after the panel closes; failures reach the shared effect log.
    updates.send_replace(Some(Arc::new(snapshot)));
    makepad_widgets::SignalToUI::set_ui_signal();
}

async fn load(factory: WorldFactory, source: Source) -> Result<model::Draft, String> {
    kernel::runtime::spawn_blocking(move || {
        let world = factory.build().map_err(|e| e.to_string())?;
        match source {
            Source::Note(id) => model::body(world.store(), id)
                .map(|body| model::Draft {
                    original: body.clone(),
                    body,
                })
                .ok_or_else(|| "this note was deleted — undo restores it".into()),
            Source::File(path) => model::draft(world.store(), &path)
                .map(Ok)
                .unwrap_or_else(|| {
                    world.run(&model::ReadFile(path)).map(|body| model::Draft {
                        original: body.clone(),
                        body,
                    })
                }),
            Source::Invalid => Err("invalid editor address".into()),
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

async fn snapshot(
    version: u64,
    sequence: u64,
    source: Source,
    draft: model::Draft,
    error: String,
    available: bool,
    save: SaveOutcome,
) -> Snapshot {
    kernel::runtime::spawn_blocking(move || {
        let file = matches!(source, Source::File(_));
        let text = if file {
            file_text::editable(&draft.body)
        } else {
            draft.body
        };
        let baseline = if file {
            file_text::editable(&draft.original)
        } else {
            draft.original.clone()
        };
        let spans = markdown::spans(&text);
        Snapshot {
            version,
            sequence,
            text,
            original: draft.original,
            baseline,
            spans,
            error,
            available,
            save,
        }
    })
    .await
    .expect("document presentation")
}

fn report(world: &World, source: &Source, error: &str) {
    if error.is_empty() {
        return;
    }
    let entity = match source {
        Source::Note(id) => format!("note:{id}"),
        Source::File(path) => format!("file:{path}"),
        Source::Invalid => "editor".into(),
    };
    world.store().mem().record(MemEffect {
        seq: world.store().mem().next_seq(),
        kind: "notes.autosave",
        entity: Some(entity),
        writes: true,
        what: "save editor draft".into(),
        error: Some(error.into()),
        at: world.now(),
    });
}

async fn run(
    factory: WorldFactory,
    source: Source,
    mut commands: mpsc::UnboundedReceiver<Command>,
    updates: watch::Sender<Option<Arc<Snapshot>>>,
) {
    let world = match factory.build() {
        Ok(world) => world,
        Err(error) => {
            publish(
                &updates,
                snapshot(
                    1,
                    0,
                    source,
                    model::Draft {
                        original: String::new(),
                        body: String::new(),
                    },
                    error.to_string(),
                    false,
                    SaveOutcome::default(),
                )
                .await,
            );
            return;
        }
    };
    let loaded = load(factory.clone(), source.clone()).await;
    let available = loaded.is_ok();
    let error = loaded.as_ref().err().cloned().unwrap_or_default();
    let mut held = loaded.unwrap_or(model::Draft {
        original: String::new(),
        body: String::new(),
    });
    let mut version = 1;
    publish(
        &updates,
        snapshot(
            version,
            0,
            source.clone(),
            held.clone(),
            error,
            available,
            SaveOutcome::default(),
        )
        .await,
    );
    let mut pending = None;
    let mut save_outcome = SaveOutcome::default();
    while let Some(mut command) = match pending.take() {
        Some(command) => Some(command),
        None => commands.recv().await,
    } {
        if let Command::Barrier(acknowledge) = command {
            let _ = acknowledge.send(());
            continue;
        }
        if matches!(command, Command::Edit { .. }) {
            while let Ok(next) = commands.try_recv() {
                if matches!(next, Command::Edit { .. }) {
                    command = next;
                } else {
                    pending = Some(next);
                    break;
                }
            }
        }
        version += 1;
        if let Command::Reload { sequence } = command {
            let loaded = load(factory.clone(), source.clone()).await;
            let available = loaded.is_ok();
            let error = loaded.as_ref().err().cloned().unwrap_or_default();
            if let Ok(draft) = loaded {
                held = draft;
            }
            publish(
                &updates,
                snapshot(
                    version,
                    sequence,
                    source.clone(),
                    held.clone(),
                    error,
                    available,
                    save_outcome.clone(),
                )
                .await,
            );
            continue;
        }
        let save = matches!(command, Command::Save { .. });
        let (sequence, text, now) = match command {
            Command::Edit {
                sequence,
                text,
                now,
            }
            | Command::Save {
                sequence,
                text,
                now,
            } => (sequence, text, now),
            Command::Reload { .. } | Command::Barrier(_) => unreachable!(),
        };
        let original = held.original.clone();
        let file = matches!(source, Source::File(_));
        let body = kernel::runtime::spawn_blocking(move || {
            if file {
                file_text::encode(&original, &text)
            } else {
                text
            }
        })
        .await
        .expect("document encoding");
        held.body = body;
        let draft = held.clone();
        let target = source.clone();
        let result = world
            .store()
            .write_async(move |c| match target {
                Source::Note(id) => model::put_note(c, id, &model::NoteText::new(draft.body, now)),
                Source::File(path) => model::put_draft(
                    c,
                    &path,
                    (draft.body != draft.original).then_some(&draft),
                    now,
                ),
                Source::Invalid => Ok(()),
            })
            .await
            .map_err(|e| format!("could not autosave: {e}"));
        let mut error = result.err().unwrap_or_default();
        let mut saved_file = false;
        if save && error.is_empty() {
            if let Source::File(path) = &source {
                let request = model::SaveFile {
                    path: path.clone(),
                    original: held.original.clone(),
                    body: held.body.clone(),
                };
                let work = factory.clone();
                let result = kernel::runtime::spawn_blocking(move || {
                    work.build().map_err(|e| e.to_string())?.run(&request)
                })
                .await
                .map_err(|e| e.to_string())
                .and_then(|result| result);
                match result {
                    Err(why) => error = why,
                    Ok(()) => {
                        let previous_original = held.original.clone();
                        held.original = held.body.clone();
                        saved_file = true;
                        let path = path.clone();
                        let saved = held.body.clone();
                        // A different editor may have saved a newer draft while
                        // disk I/O ran. Clear only the exact draft we wrote.
                        let result = world.store().write_async(move |c| {
                            c.execute("DELETE FROM notes_draft WHERE path=?1 AND body=?2 AND original=?3", rusqlite::params![path, saved, previous_original])?;
                            Ok(())
                        }).await;
                        if let Err(why) = result {
                            error = format!("file saved but draft cleanup failed: {why}");
                        }
                    }
                }
            }
        }
        report(&world, &source, &error);
        if save {
            save_outcome = SaveOutcome {
                sequence,
                error: error.clone(),
                written: saved_file,
            };
        }
        let state = snapshot(
            version,
            sequence,
            source.clone(),
            held.clone(),
            error,
            true,
            save_outcome.clone(),
        )
        .await;
        publish(&updates, state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel::{
        app::{App, Env, Mode},
        caps::{real_path, DemoDisk, DiskFactory},
        store::Store,
    };
    static APPS: &[&dyn App] = &[&super::super::NOTES];

    fn world() -> World {
        let disk = DiskFactory::shared(DemoDisk::new(Default::default()));
        let env = Env {
            disk: Some(disk),
            ..Env::default()
        };
        kernel::app::world_for(
            APPS,
            Store::open(None, &[&super::super::SCHEMA], kernel::sync::Device::fake()).unwrap(),
            Mode::Fake,
            &env,
        )
    }

    async fn until(
        receiver: &mut watch::Receiver<Option<Arc<Snapshot>>>,
        sequence: u64,
    ) -> Arc<Snapshot> {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let snapshot = receiver.borrow_and_update().clone();
                if let Some(snapshot) = snapshot.filter(|s| s.sequence >= sequence) {
                    return snapshot;
                }
                receiver
                    .changed()
                    .await
                    .expect("document service is running");
            }
        })
        .await
        .expect("document I/O completed")
    }

    #[test]
    fn accepted_autosaves_survive_close_and_keep_the_latest_edit() {
        let world = world();
        world.store().write(|c| {
            c.execute("INSERT INTO notes_note(id,title,body,created,modified) VALUES(900,'test','',0,0)", [])?;
            Ok(())
        }).unwrap();
        let handle = Handle::start(
            world.factory().unwrap(),
            world.store().db(),
            Source::Note(900),
        );
        let mut snapshots = handle.snapshots.clone();
        kernel::runtime::block_on(until(&mut snapshots, 0));
        let (entered, held) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let _write = world
            .store()
            .submit_write(move |_| {
                entered.send(()).unwrap();
                released.recv().unwrap();
                Ok(())
            })
            .unwrap();
        held.recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        for sequence in 1..=100 {
            handle.edit(sequence, format!("edit {sequence}"), sequence as f64);
        }
        drop(handle);
        // Submission and closing both returned while the writer was blocked.
        release.send(()).unwrap();
        let final_state = kernel::runtime::block_on(until(&mut snapshots, 100));
        assert_eq!(final_state.text, "edit 100");
        assert!(final_state.error.is_empty());
        assert_eq!(model::body(world.store(), 900).as_deref(), Some("edit 100"));
    }

    #[test]
    fn shutdown_flushes_open_editors_and_drains_closed_editors() {
        let world = world();
        world.store().write(|c| {
            for id in [901, 902] {
                c.execute("INSERT INTO notes_note(id,title,body,created,modified) VALUES(?1,'test','',0,0)", [id])?;
            }
            Ok(())
        }).unwrap();
        let open = Handle::start(
            world.factory().unwrap(),
            world.store().db(),
            Source::Note(901),
        );
        let closed = Handle::start(
            world.factory().unwrap(),
            world.store().db(),
            Source::Note(902),
        );
        open.edit(1, "open editor final text".into(), 1.0);
        closed.edit(1, "closed editor final text".into(), 1.0);
        drop(closed);
        kernel::runtime::block_on(flush(world.store().db()));
        assert_eq!(
            model::body(world.store(), 901).as_deref(),
            Some("open editor final text")
        );
        assert_eq!(
            model::body(world.store(), 902).as_deref(),
            Some("closed editor final text")
        );
        open.edit(2, "still usable after a barrier".into(), 2.0);
        kernel::runtime::block_on(flush(world.store().db()));
        assert_eq!(
            model::body(world.store(), 901).as_deref(),
            Some("still usable after a barrier")
        );
    }

    #[test]
    fn save_is_a_barrier_and_new_typing_keeps_a_draft_against_the_saved_file() {
        let world = world();
        let path = "~/async-note.md";
        world
            .with_cap::<dyn kernel::caps::Disk, _>(|disk| {
                disk.write_file(&real_path(path), b"old\r\n")
            })
            .unwrap()
            .unwrap();
        let handle = Handle::start(
            world.factory().unwrap(),
            world.store().db(),
            Source::File(path.into()),
        );
        let mut snapshots = handle.snapshots.clone();
        kernel::runtime::block_on(until(&mut snapshots, 0));
        handle.edit(1, "first\n".into(), 1.0);
        handle.save(2, "first\n".into(), 2.0);
        handle.edit(3, "second\n".into(), 3.0);
        let final_state = kernel::runtime::block_on(until(&mut snapshots, 3));
        assert_eq!(final_state.original, "first\r\n");
        assert_eq!(final_state.text, "second\n");
        assert!(final_state.error.is_empty(), "{}", final_state.error);
        assert_eq!(final_state.save.sequence, 2);
        assert!(final_state.save.written && final_state.save.error.is_empty());
        let draft = model::draft(world.store(), path).unwrap();
        assert_eq!(draft.original, "first\r\n");
        assert_eq!(draft.body, "second\r\n");
        let bytes = world
            .with_cap::<dyn kernel::caps::Disk, _>(|disk| disk.read_file(&real_path(path), 100))
            .unwrap()
            .unwrap();
        assert_eq!(bytes, b"first\r\n");
    }

    #[test]
    fn a_conflicting_file_is_unchanged_and_the_failed_save_keeps_its_draft() {
        let world = world();
        let path = "~/async-conflict.md";
        world
            .with_cap::<dyn kernel::caps::Disk, _>(|disk| disk.write_file(&real_path(path), b"old"))
            .unwrap()
            .unwrap();
        let handle = Handle::start(
            world.factory().unwrap(),
            world.store().db(),
            Source::File(path.into()),
        );
        let mut snapshots = handle.snapshots.clone();
        kernel::runtime::block_on(until(&mut snapshots, 0));
        world
            .with_cap::<dyn kernel::caps::Disk, _>(|disk| {
                disk.write_file(&real_path(path), b"external")
            })
            .unwrap()
            .unwrap();
        handle.save(1, "my draft".into(), 1.0);
        drop(handle);
        let final_state = kernel::runtime::block_on(until(&mut snapshots, 1));
        assert!(!final_state.error.is_empty());
        assert_eq!(final_state.save.sequence, 1);
        assert_eq!(final_state.save.error, final_state.error);
        assert!(!final_state.save.written);
        assert_eq!(model::draft(world.store(), path).unwrap().body, "my draft");
        let bytes = world
            .with_cap::<dyn kernel::caps::Disk, _>(|disk| disk.read_file(&real_path(path), 100))
            .unwrap()
            .unwrap();
        assert_eq!(bytes, b"external");
        assert!(world.store().mem().json().contains("notes.autosave"));
    }
}
