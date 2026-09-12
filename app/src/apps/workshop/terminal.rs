//! Workspace presentation owns a mapping, not a PTY. Moving a terminal changes
//! that mapping and opens the same terminal-app session in an ordinary panel.
use crate::apps::terminal::{SessionHandle, TerminalService};
use kernel::effect::World;
use kernel::{layout::SlotId, nav::Nav, session::Session, store::Store};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Mutex,
};

#[derive(Default)]
struct Embedded {
    sessions: Mutex<HashMap<i64, String>>,
}

fn service(world: &World) -> Result<TerminalService, String> {
    world
        .with_cap::<TerminalService, _>(|service| *service)
        .map_err(|error| format!("Terminal is unavailable: {error}"))
}

pub fn embedded(world: &World, workspace_id: i64) -> Result<SessionHandle, String> {
    let service = service(world)?;
    let store = world.store();
    let workspace = super::model::workspace(store, workspace_id).ok_or("Workspace not found")?;
    if workspace.archived {
        return Err("Restore this workspace before opening its terminal".into());
    }
    let registry = store.local::<Embedded>();
    let mut entries = registry.sessions.lock().unwrap();
    if let Some(handle) = entries
        .get(&workspace_id)
        .and_then(|key| service.get(store, key))
    {
        return Ok(handle);
    }
    if workspace.status != "ready" {
        return Err("The workspace is still being prepared".into());
    }
    let handle = service.create(store, Path::new(&workspace.path))?;
    entries.insert(workspace_id, handle.id.clone());
    record(store, workspace_id, &handle);
    Ok(handle)
}

fn record(store: &Store, workspace: i64, handle: &SessionHandle) {
    let db = store.db();
    let key = handle.id.clone();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    kernel::runtime::spawn_local(move || async move {
        let Ok(store) = Store::with_db(db) else {
            return;
        };
        if let Err(error)=store.write_async(move|c|{
            c.execute("UPDATE workshop_terminal SET embedded=0 WHERE workspace_id=?",[workspace])?;
            c.execute("INSERT INTO workshop_terminal(workspace_id,session_key,embedded,created) VALUES(?1,?2,1,?3)",rusqlite::params![workspace,key,now])?;
            Ok(())
        }).await{eprintln!("Workshop terminal metadata: {error}");}
        makepad_widgets::SignalToUI::set_ui_signal();
    });
}

pub fn promote(s: &mut Session, from: SlotId, workspace_id: i64) {
    if let Err(error) = try_promote(s, from, workspace_id) {
        s.notify(error, true);
    }
}

pub fn try_promote(s: &mut Session, from: SlotId, workspace_id: i64) -> Result<(), String> {
    let service = service(s.world())?;
    let handle = embedded(s.world(), workspace_id)?;
    // Allocate first, so a failed replacement cannot orphan the live session.
    let fresh = service.create(s.store(), &handle.cwd)?;
    s.store()
        .local::<Embedded>()
        .sessions
        .lock()
        .unwrap()
        .insert(workspace_id, fresh.id.clone());
    record(s.store(), workspace_id, &fresh);
    s.nav(Nav::Open {
        from,
        id: service.panel_id(&handle),
        fresh: true,
    });
    s.redraw();
    Ok(())
}

pub fn login(s: &mut Session, from: SlotId, provider: &str) {
    if let Err(error) = try_login(s, from, provider) {
        s.notify(error, true);
    }
}

pub fn try_login(s: &mut Session, from: SlotId, provider: &str) -> Result<(), String> {
    let service = service(s.world())?;
    let provider = super::harness::Provider::parse(provider)?;
    let executable = if service.is_real() {
        match super::harness::find_executable(provider) {
            Some(path) => path,
            None => {
                return Err(format!(
                    "Install the {} CLI, then sign in with your subscription.",
                    provider.as_str()
                ))
            }
        }
    } else {
        PathBuf::from(provider.as_str())
    };
    let command = super::harness::login_command(provider, &executable);
    let cwd = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"));
    let handle = service.create(s.store(), &cwd)?;
    if let Err(error) = handle.input(&format!("{command}\r")) {
        service.close(s.store(), &handle.id);
        return Err(error);
    }
    s.nav(Nav::Open {
        from,
        id: service.panel_id(&handle),
        fresh: true,
    });
    Ok(())
}

pub fn list(world: &World, workspace_id: i64) -> Vec<serde_json::Value> {
    let Ok(service) = service(world) else {
        return vec![];
    };
    let store = world.store();
    let current = store
        .local::<Embedded>()
        .sessions
        .lock()
        .unwrap()
        .get(&workspace_id)
        .cloned();
    let mut result = Vec::new();
    if let Ok(mut statement) = store.conn().prepare(
        "SELECT id,session_key,embedded FROM workshop_terminal WHERE workspace_id=? ORDER BY id",
    ) {
        if let Ok(rows) = statement.query_map([workspace_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, bool>(2)?,
            ))
        }) {
            for (id, key, embedded) in rows.flatten() {
                let handle = service.get(store, &key);
                result.push(serde_json::json!({"id":id,"session_key":key,"embedded":current.as_ref().map_or(embedded,|current|current==&key),"live":handle.as_ref().is_some_and(|h|!h.finished()),"finished":handle.as_ref().is_none_or(SessionHandle::finished),"title":handle.as_ref().map(SessionHandle::title),"status":handle.as_ref().and_then(SessionHandle::status)}));
            }
        }
    }
    // The writer completes asynchronously; new live handles remain addressable
    // immediately, including a tool's open-panel reply in the same UI event.
    if let Some(workspace) = super::model::workspace(store, workspace_id) {
        for handle in service
            .list(store)
            .into_iter()
            .filter(|h| h.cwd == Path::new(&workspace.path))
        {
            if !result
                .iter()
                .any(|r| r["session_key"].as_str() == Some(handle.id.as_str()))
            {
                result.push(serde_json::json!({"id":null,"session_key":handle.id,"embedded":current.as_ref()==Some(&handle.id),"live":!handle.finished(),"finished":handle.finished(),"title":handle.title(),"status":handle.status()}));
            }
        }
    }
    result
}

pub fn input(world: &World, session_key: &str, text: &str) -> Result<(), String> {
    service(world)?
        .get(world.store(), session_key)
        .ok_or("Terminal session is not running")?
        .input(text)
}
pub async fn read(world: &World, session_key: &str) -> Result<String, String> {
    service(world)?
        .get(world.store(), session_key)
        .ok_or("Terminal session is not running")?
        .read()
        .await
}
pub fn close_embedded(world: &World, workspace_id: i64) {
    let key = world
        .store()
        .local::<Embedded>()
        .sessions
        .lock()
        .unwrap()
        .remove(&workspace_id);
    if let Some(key) = key {
        close(world, &key);
    }
}
pub fn close(world: &World, session_key: &str) -> bool {
    service(world).is_ok_and(|service| service.close(world.store(), session_key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel::{
        app::App,
        panel::{PanelId, Tag},
    };
    #[test]
    fn promotion_replaces_only_the_embedded_mapping_and_workspace_close_keeps_both_shells() {
        static APPS: &[&dyn App] = &[&crate::apps::terminal::TERMINAL, &super::super::WORKSHOP];
        let mut session = Session::fake(APPS);
        session.nav(Nav::Open {
            from: 0,
            id: PanelId::new(Tag("workshop_workspace"), ["1"]),
            fresh: true,
        });
        session.settle();
        let workspace_slot = session.focus().unwrap();
        let original = embedded(session.world(), 1).unwrap();
        original.input("echo retained input\r").unwrap();
        try_promote(&mut session, workspace_slot, 1).unwrap();
        session.settle();
        let terminal_slot = session.focus().unwrap();
        let fresh = embedded(session.world(), 1).unwrap();
        assert_ne!(original.id, fresh.id);
        assert_eq!(
            session.panel(terminal_slot).unwrap().borrow().id().arg(1),
            Some(original.id.as_str())
        );
        assert!(kernel::runtime::block_on(original.read())
            .unwrap()
            .contains("retained input"));
        assert!(!kernel::runtime::block_on(fresh.read())
            .unwrap()
            .contains("retained input"));
        session.nav(Nav::Close {
            slot: workspace_slot,
            label: None,
        });
        session.settle();
        assert!(service(session.world())
            .unwrap()
            .get(session.store(), &original.id)
            .is_some());
        assert!(service(session.world())
            .unwrap()
            .get(session.store(), &fresh.id)
            .is_some());
        let reopened = embedded(session.world(), 1).unwrap();
        assert_eq!(fresh.id, reopened.id);
        service(session.world())
            .unwrap()
            .close(session.store(), &original.id);
        service(session.world())
            .unwrap()
            .close(session.store(), &fresh.id);
    }
    #[test]
    fn a_build_without_terminal_has_no_terminal_capability_or_accidental_demo_shell() {
        static APPS: &[&dyn App] = &[&super::super::WORKSHOP];
        let mut session = Session::fake(APPS);
        assert!(embedded(session.world(), 1)
            .err()
            .unwrap()
            .contains("unavailable"));
        assert!(list(session.world(), 1).is_empty());
        assert!(try_promote(&mut session, 0, 1)
            .unwrap_err()
            .contains("unavailable"));
        assert!(try_login(&mut session, 0, "codex")
            .unwrap_err()
            .contains("unavailable"));
    }
    #[test]
    fn tool_close_replaces_the_embedded_session_and_reports_the_old_one_closed() {
        static APPS: &[&dyn App] = &[&crate::apps::terminal::TERMINAL, &super::super::WORKSHOP];
        let session = Session::fake(APPS);
        let old = embedded(session.world(), 1).unwrap();
        assert!(close(session.world(), &old.id));
        let fresh = embedded(session.world(), 1).unwrap();
        assert_ne!(fresh.id, old.id);
        let listed = list(session.world(), 1);
        assert!(listed
            .iter()
            .any(|r| r["session_key"] == fresh.id && r["embedded"] == true));
        assert!(!listed
            .iter()
            .any(|r| r["session_key"] == old.id && r["live"] == true));
        close(session.world(), &fresh.id);
    }
}
