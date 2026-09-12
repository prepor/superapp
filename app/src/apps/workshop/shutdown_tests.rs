//! Real service retirement with an isolated local subprocess, never a provider.
use super::*;
use kernel::app::{world_for, App, Apps, Capabilities, Env, Schema, Workers};
use kernel::{panel::PanelKind, sync::Device};
use std::{any::Any, os::unix::fs::PermissionsExt, rc::Rc, time::Instant};

struct ChatOnly;
impl App for ChatOnly {
    fn id(&self) -> &'static str {
        "workshop-shutdown-test"
    }
    fn kinds(&self) -> &'static [&'static dyn PanelKind] {
        &[]
    }
    fn schema(&self) -> Option<&'static Schema> {
        Some(&super::super::schema::SCHEMA)
    }
    fn outside(&self, _: Mode, _: &Env, caps: &mut Capabilities) {
        caps.insert(Box::new(RuntimeMode(Mode::Real)));
    }
    fn workers(&self, store: &Store) -> Vec<Box<dyn Worker>> {
        workers(store)
            .into_iter()
            .filter(|worker| worker.name().starts_with("workshop-chat-"))
            .collect()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
static CHAT_ONLY: ChatOnly = ChatOnly;
static APPS: &[&dyn App] = &[&CHAT_ONLY];

struct Fixture {
    dir: PathBuf,
    repo: PathBuf,
    session: Session,
    capture: Option<tokio::sync::OwnedMutexGuard<()>>,
}
impl Fixture {
    fn new(hold_capture: bool) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "workshop-shutdown-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let repo = dir.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(&repo)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-b", "main"]);
        std::fs::write(repo.join("file.txt"), "before\n").unwrap();
        git(&["add", "file.txt"]);
        git(&[
            "-c",
            "user.name=Workshop test",
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-m",
            "base",
        ]);
        let executable = dir.join("fake-provider");
        std::fs::write(&executable, r#"#!/bin/sh
cat >/dev/null
printf 'changed by provider\n' > file.txt
sleep 300 &
printf '%s %s\n' "$$" "$!" > .git/provider-pids
printf '%s\n' '{"type":"thread.started","thread_id":"retirement-session"}'
printf '%s\n' '{"type":"item.completed","item":{"id":"reply","type":"agent_message","text":"Waiting for your approval"}}'
wait
"#).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let apps = Apps::new(APPS);
        let store = Store::open(None, &apps.schemas(), Device::fake()).unwrap();
        let path = repo.to_string_lossy().into_owned();
        store.write(move |c| {
            c.execute("INSERT INTO workshop_project(id,name,path,base_ref,status) VALUES(1,'repo',?,'main','ready')", [&path])?;
            c.execute("INSERT INTO workshop_workspace(id,project_id,label,path,branch,base_ref,status,activity) VALUES(1,1,'basel',?,'main','main','ready',1)", [&path])?;
            c.execute("INSERT INTO workshop_chat(id,workspace_id,ordinal,last_used) VALUES(1,1,1,1)", [])?;
            model::send_tx(c, 1, "Make a change, then ask for approval.", "work", 1.0)?;
            Ok(())
        }).unwrap();
        *store.local::<TestHarness>().executable.lock().unwrap() = Some(executable);
        let capture = if hold_capture {
            let lock = Arc::new(tokio::sync::Mutex::new(()));
            store
                .local::<Live>()
                .captures
                .lock()
                .unwrap()
                .insert(1, lock.clone());
            Some(kernel::runtime::block_on(lock.lock_owned()))
        } else {
            None
        };
        let world = Rc::new(world_for(APPS, store, Mode::Real, &Env::default()));
        let workers = Workers::async_io(
            APPS,
            world.store().clone(),
            Mode::Real,
            Env::default(),
            || {},
        );
        workers.kick_all();
        let session = Session::new(apps, world, workers);
        Self {
            dir,
            repo,
            session,
            capture,
        }
    }
    fn wait(&mut self, mut condition: impl FnMut(&mut Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(8);
        while !condition(self) {
            assert!(
                Instant::now() < deadline,
                "Workshop worker did not reach the expected state"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn status(&self) -> String {
        self.session
            .store()
            .conn()
            .query_row("SELECT status FROM workshop_run WHERE id=1", [], |r| {
                r.get(0)
            })
            .unwrap()
    }
    fn connection(&self) -> Option<super::super::bridge::RunConnection> {
        self.session
            .store()
            .local::<TestHarness>()
            .connection
            .lock()
            .unwrap()
            .clone()
    }
    fn pids(&self) -> Vec<u32> {
        std::fs::read_to_string(self.repo.join(".git/provider-pids"))
            .unwrap_or_default()
            .split_whitespace()
            .map(|pid| pid.parse().unwrap())
            .collect()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        // Also stop subprocesses if an assertion fails before the regression's
        // shutdown path, so a failed test cannot leave a provider fixture live.
        for token in self
            .session
            .store()
            .local::<Live>()
            .cancels
            .lock()
            .unwrap()
            .values()
        {
            token.cancel();
        }
        self.capture.take();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn request(
    connection: &super::super::bridge::RunConnection,
    body: Value,
) -> tokio::task::JoinHandle<(u16, Value)> {
    let connection = connection.clone();
    kernel::runtime::spawn(async move {
        let response = reqwest::Client::new()
            .post(connection.url)
            .bearer_auth(connection.bearer_token)
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .unwrap();
        let status = response.status().as_u16();
        let body = response.bytes().await.unwrap();
        (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
    })
}
fn running(pid: u32) -> bool {
    let output = std::process::Command::new("/bin/ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    let state = String::from_utf8_lossy(&output.stdout);
    output.status.success() && !state.trim().is_empty() && !state.trim().starts_with('Z')
}

#[test]
fn session_shutdown_cancels_a_live_provider_waiting_for_app_tool_approval() {
    let mut fixture = Fixture::new(false);
    fixture.wait(|f| {
        f.connection().is_some()
            && f.pids().len() == 2
            && model::messages(f.session.store(), 1)
                .iter()
                .any(|m| m.body == "Waiting for your approval")
    });
    let pids = fixture.pids();
    assert!(pids.iter().all(|pid| running(*pid)));
    let connection = fixture.connection().unwrap();
    let approval = request(
        &connection,
        json!({
            "jsonrpc":"2.0", "id":1, "method":"tools/call", "params":{
                "name":"sql.write", "arguments":{"sql":"INSERT INTO meta(key,value) VALUES('unapproved-retirement-write',1)"}
            }
        }),
    );
    fixture.wait(|f| {
        super::super::bridge::poll(&mut f.session);
        f.session.settle();
        f.session
            .store()
            .conn()
            .query_row(
                "SELECT count(*) FROM workshop_tool_call WHERE status='pending'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
            == 1
    });
    assert!(!approval.is_finished());
    fixture.session.begin_shutdown();
    // A closing session never needs a user to approve (or another UI poll to
    // service) the outstanding tool request before its worker can finish.
    fixture.wait(|f| f.session.poll_shutdown());
    assert_eq!(fixture.status(), "stopped");
    fixture.wait(|_| pids.iter().all(|pid| !running(*pid)));
    let (status, answer) = kernel::runtime::block_on(approval).unwrap();
    assert_eq!(status, 200);
    assert_eq!(answer["result"]["isError"], true);
    let conn = fixture.session.store().conn();
    assert_eq!(
        conn.query_row("SELECT status FROM workshop_tool_call", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "interrupted"
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM meta WHERE key='unapproved-retirement-write'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        conn.query_row("SELECT session_id FROM workshop_chat WHERE id=1", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "retirement-session"
    );
    assert!(model::messages(fixture.session.store(), 1)
        .iter()
        .any(|m| m.body == "Waiting for your approval" && m.step_id.is_some()));
    assert_eq!(
        conn.query_row("SELECT status FROM workshop_step", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "stopped"
    );
    assert!(conn.query_row("SELECT EXISTS(SELECT 1 FROM workshop_change c JOIN workshop_step s ON s.diff_id=c.snapshot_id WHERE c.path='file.txt' AND c.patch LIKE '%changed by provider%')", [], |r| r.get::<_, bool>(0)).unwrap());
    assert!(fixture
        .session
        .store()
        .local::<Live>()
        .cancels
        .lock()
        .unwrap()
        .is_empty());
    let (status, _) = kernel::runtime::block_on(request(
        &connection,
        json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{}}),
    ))
    .unwrap();
    assert_eq!(status, 401, "retired run authentication must be revoked");
}

#[test]
fn retirement_during_snapshot_setup_cannot_start_a_provider_or_register_late_auth() {
    let mut fixture = Fixture::new(true);
    fixture.wait(|f| {
        f.status() == "running"
            && f.session
                .store()
                .local::<Live>()
                .cancels
                .lock()
                .unwrap()
                .contains_key(&1)
    });
    assert!(fixture.connection().is_none());
    fixture.session.begin_shutdown();
    fixture.wait(|f| {
        f.session.poll_shutdown();
        f.status() == "stopped"
    });
    fixture.capture.take();
    fixture.wait(|f| f.session.poll_shutdown());
    assert_eq!(fixture.status(), "stopped");
    assert!(
        fixture.pids().is_empty(),
        "retirement must win even before the provider starts"
    );
    assert!(
        fixture.connection().is_none(),
        "cancelled setup must not register run authentication"
    );
    assert_eq!(
        model::messages(fixture.session.store(), 1).len(),
        1,
        "the accepted user prompt remains in history"
    );
    assert!(fixture
        .session
        .store()
        .local::<Live>()
        .cancels
        .lock()
        .unwrap()
        .is_empty());
}
