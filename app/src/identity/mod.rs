//! Accounts and Google grants shared by Mail and Calendar. The original
//! `account` IDs and keychain keys remain valid across the extraction.
pub mod accounts;
pub mod history;
pub mod oauth;

pub static SCHEMA: kernel::app::Schema = kernel::app::Schema {
    app: "accounts",
    steps: &[kernel::app::Step::Run(upgrade)],
};

use kernel::app::Env;
use kernel::caps::{ClockSource, MemSecrets, Secrets, SecretsFactory};
use rusqlite::{params, Connection};
use std::{collections::HashMap, path::PathBuf};

pub fn upgrade(c: &Connection) -> rusqlite::Result<()> {
    // The historical Mail ladder may already own this table. Keep every ID
    // and column while allowing Accounts to run without the Mail app.
    c.execute_batch(
        "CREATE TABLE IF NOT EXISTS account(
        id INTEGER PRIMARY KEY, label TEXT NOT NULL, email TEXT NOT NULL,
        imap_host TEXT, smtp_host TEXT, status TEXT, synced REAL, auth TEXT
    )",
    )?;
    for (name, definition) in [
        ("google_sub", "TEXT"),
        ("scopes", "TEXT NOT NULL DEFAULT ''"),
        ("mail_enabled", "INTEGER NOT NULL DEFAULT 1"),
        ("calendar_enabled", "INTEGER NOT NULL DEFAULT 0"),
    ] {
        if !c
            .prepare("SELECT 1 FROM pragma_table_info('account') WHERE name=?")?
            .exists([name])?
        {
            c.execute_batch(&format!(
                "ALTER TABLE account ADD COLUMN {name} {definition}"
            ))?;
        }
    }
    c.execute_batch("CREATE UNIQUE INDEX IF NOT EXISTS account_google_identity ON account(google_sub) WHERE google_sub IS NOT NULL")?;
    Ok(())
}

pub fn services(c: &Connection, id: i64) -> (bool, bool, String) {
    c.query_row(
        "SELECT mail_enabled,calendar_enabled,scopes FROM account WHERE id=?",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
    .unwrap_or_default()
}

/// Mail-only grants keep working until Calendar is explicitly connected.
/// Scope upgrades always request the union; installed-app OAuth has no
/// incremental authorization. Never replace a usable grant with fewer scopes.
pub fn required_scopes(mail: bool, calendar: bool) -> String {
    let mut scopes = vec!["openid", "email"];
    if mail {
        scopes.push(oauth::GOOGLE.mail_scope);
    }
    if calendar {
        scopes.extend(oauth::CALENDAR_SCOPES);
    }
    scopes.join(" ")
}

pub fn set_services(c: &Connection, id: i64, mail: bool, calendar: bool) -> rusqlite::Result<()> {
    c.execute(
        "UPDATE account SET mail_enabled=?1,calendar_enabled=?2 WHERE id=?3",
        params![mail, calendar, id],
    )?;
    if c.prepare("SELECT 1 FROM sqlite_master WHERE name='calendar_sync'")?
        .exists([])?
    {
        c.execute(
            "UPDATE calendar_sync SET requested=requested+1 WHERE id=1",
            [],
        )?;
    }
    Ok(())
}

/// Every protocol uses the same secret lookup and refresh implementation.
/// Reauthorization changes the refresh token, immediately invalidating a
/// cached access token even when the previous one has not expired yet.
pub struct Tokens {
    dir: Option<PathBuf>,
    backend: Option<SecretsFactory>,
    memory: MemSecrets,
    clock: ClockSource,
    cache: HashMap<String, (String, String, f64)>,
}
impl Tokens {
    pub fn new(env: &Env) -> Self {
        Self {
            dir: env.db_dir.clone(),
            backend: env.secrets_backend.clone(),
            memory: env.secrets.clone(),
            clock: env.clock.clone(),
            cache: HashMap::new(),
        }
    }
    #[cfg(test)]
    pub(crate) fn grant(&self, email: &str) -> Option<String> {
        lookup_refresh(self.backend.clone(), self.memory.clone(), email)
    }
    pub async fn access(&mut self, email: &str) -> Result<String, String> {
        let (backend, memory, address) = (self.backend.clone(), self.memory.clone(), email.to_string());
        let grant = kernel::runtime::spawn_blocking(move || lookup_refresh(backend, memory, &address))
            .await.map_err(|error| error.to_string())?
            .ok_or("Google account needs to be reconnected in Accounts")?;
        let now = self.clock.read();
        if let Some((held, token, until)) = self.cache.get(email) {
            if held == &grant && now + 120.0 < *until {
                return Ok(token.clone());
            }
        }
        let dir = self.dir.clone().ok_or("no store directory for Google registration")?;
        let client = kernel::runtime::spawn_blocking(move || oauth::Client::load(&dir))
            .await.map_err(|error| error.to_string())??;
        let (token, until) = oauth::refresh(&client, oauth::GOOGLE, &grant, now).await?;
        self.cache
            .insert(email.into(), (grant, token.clone(), until));
        Ok(token)
    }
}

fn lookup_refresh(backend: Option<SecretsFactory>, memory: MemSecrets, email: &str) -> Option<String> {
    let mut secrets: Box<dyn Secrets> = backend.map_or_else(
        || Box::new(memory) as Box<dyn Secrets>, |factory| factory.make());
    secrets.get(&oauth::refresh_key(email))
}

/// Account/service removal must not silently abandon outgoing work.
pub fn pending(c: &Connection, id: i64) -> Result<(), String> {
    let mail = c
        .query_row(
            "SELECT COUNT(*) FROM outbox WHERE account=? AND status='pending'",
            [id],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0);
    let calendar=c.query_row("SELECT COUNT(*) FROM calendar_change ch JOIN calendar_source cs ON cs.id=ch.source WHERE cs.account=? AND ch.state IN ('pending','processing')",[id],|r|r.get::<_,i64>(0)).unwrap_or(0);
    if mail + calendar > 0 {
        Err(format!("finish the {mail} Mail and {calendar} Calendar operations before disconnecting this account"))
    } else {
        Ok(())
    }
}

pub struct ServicesChanged {
    pub account: i64,
    pub before: (bool, bool),
    pub after: (bool, bool),
}
impl kernel::history::Intent for ServicesChanged {
    fn describe(&self) -> String {
        "account service settings".into()
    }
    fn reverse(&self, w: &kernel::effect::World) -> Result<(), String> {
        self.put(w, self.before)
    }
    fn reapply(&self, w: &kernel::effect::World) -> Result<(), String> {
        self.put(w, self.after)
    }
}
impl ServicesChanged {
    fn put(&self, w: &kernel::effect::World, services: (bool, bool)) -> Result<(), String> {
        let id = self.account;
        pending(w.store().conn(), id)?;
        w.store()
            .write(move |c| set_services(c, id, services.0, services.1))
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(flavor = "current_thread")]
    async fn a_slow_native_grant_lookup_does_not_block_the_service_executor() {
        use std::sync::{Arc, Mutex};
        struct SlowSecrets {
            entered: Arc<Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
            released: Arc<Mutex<std::sync::mpsc::Receiver<()>>>,
        }
        impl Secrets for SlowSecrets {
            fn get(&mut self, _: &str) -> Option<String> {
                self.entered.lock().unwrap().take().unwrap().send(()).unwrap();
                self.released.lock().unwrap().recv_timeout(std::time::Duration::from_secs(2)).expect("service executor can release the native lookup");
                None
            }
            fn set(&mut self, _: &str, _: &str) -> bool { false }
        }
        let (entered, waiting) = tokio::sync::oneshot::channel();
        let (release, released) = std::sync::mpsc::channel();
        let entered = Arc::new(Mutex::new(Some(entered)));
        let released = Arc::new(Mutex::new(released));
        let env = Env { secrets_backend: Some(SecretsFactory::new(move || Box::new(SlowSecrets {
            entered: entered.clone(), released: released.clone(),
        }))), ..Env::default() };
        let mut tokens = Tokens::new(&env);
        let access = tokio::spawn(async move { tokens.access("grant-test@example.test").await });
        waiting.await.unwrap();
        tokio::task::yield_now().await;
        release.send(()).unwrap();
        assert!(access.await.unwrap().unwrap_err().contains("reconnected"));
    }
    #[test]
    fn migration_preserves_legacy_mail_ids_and_defaults_calendar_off() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE account(id INTEGER PRIMARY KEY,label TEXT NOT NULL,email TEXT NOT NULL,imap_host TEXT,smtp_host TEXT,status TEXT,synced REAL,auth TEXT); INSERT INTO account VALUES(42,'Work','me@example.com','imap.gmail.com','smtp.gmail.com','synced',123,'google');").unwrap();
        upgrade(&c).unwrap();
        upgrade(&c).unwrap();
        assert_eq!(services(&c, 42), (true, false, String::new()));
        assert_eq!(
            c.query_row("SELECT email FROM account WHERE id=42", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "me@example.com"
        );
        assert_eq!(
            c.query_row("SELECT COUNT(*) FROM account", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
    #[test]
    fn accounts_schema_works_with_and_without_the_historical_mail_ladder() {
        use kernel::app::App;
        let mail = crate::apps::mail::MAIL.schema().unwrap();
        for schemas in [vec![&SCHEMA], vec![&SCHEMA, mail], vec![mail, &SCHEMA]] {
            let store = kernel::store::Store::open(None, &schemas).unwrap();
            store
                .write(|c| accounts::add_account_tx(c, "me@example.com", "", "", "google"))
                .unwrap();
            assert_eq!(services(store.conn(), 1), (true, false, String::new()));
        }
    }
}
