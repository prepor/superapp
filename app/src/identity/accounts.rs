//! Shared identity metadata. The historic table/key names are preserved.
use kernel::store::{Store, Q};
use std::rc::Rc;
/// One account row, as settings shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct Account {
    pub id: i64,
    pub label: String,
    pub email: String,
    pub imap_host: Option<String>,
    pub smtp_host: Option<String>,
    pub status: Option<String>,
    pub synced: Option<f64>,
    /// How it authenticates: `NULL`/`password` for an app password,
    /// `google` for an OAuth grant. See [`oauth`](super::oauth).
    pub auth: Option<String>,
}

impl Account {
    /// Whether it signs in with a Google grant rather than a password.
    #[must_use]
    pub fn oauth(&self) -> bool {
        self.auth.as_deref() == Some(super::oauth::GOOGLE.name)
    }

    /// What the settings row shows beside the address: the host it syncs
    /// from, or the honest word for an account with none.
    #[must_use]
    pub fn host_line(&self) -> String {
        match self.imap_host.as_deref().filter(|h| !h.is_empty()) {
            Some(h) => h.to_string(),
            None => "local demo".into(),
        }
    }

    /// The line under it: what the last pass said, or that there has not
    /// been one. `true` marks it as a failure.
    #[must_use]
    pub fn status_line(&self) -> (String, bool) {
        let s = self.status.clone().unwrap_or_else(|| "never synced".into());
        let err = s.starts_with("error");
        (s, err)
    }
}

static Q_ACCOUNTS: Q = Q {
    id: "accounts",
    sql: "SELECT id, label, email, imap_host, smtp_host, status, synced, auth
          FROM account ORDER BY id",
    describe: "every account with its connection config, auth and sync status",
};

fn account_row(r: &rusqlite::Row) -> rusqlite::Result<Account> {
    Ok(Account {
        id: r.get(0)?,
        label: r.get(1)?,
        email: r.get(2)?,
        imap_host: r.get(3)?,
        smtp_host: r.get(4)?,
        status: r.get(5)?,
        synced: r.get(6)?,
        auth: r.get(7)?,
    })
}

/// Every account.
#[must_use]
pub fn accounts(store: &Store) -> Rc<Vec<Account>> {
    store.rows(&Q_ACCOUNTS, &[], account_row)
}

/// The account already holding this address, if any. Two rows for one mailbox
/// would mean two workers fetching the same mail into the same store — and
/// for a Gmail sign-in an existing row is not an error but the ordinary case:
/// signing in again is how a grant is renewed.
#[must_use]
pub fn account_for(store: &Store, email: &str) -> Option<Account> {
    accounts(store)
        .iter()
        .find(|a| a.email.eq_ignore_ascii_case(email))
        .cloned()
}

/// Creates an account (the add-account form's action, and the end of a Gmail
/// sign-in). Folders arrive with the first sync; the secret — a password or a
/// refresh token — goes to the keychain, never here.
///
/// # Errors
///
/// If the store refuses the write.
pub fn add_account_tx(
    c: &rusqlite::Connection,
    email: &str,
    imap_host: &str,
    smtp_host: &str,
    auth: &str,
) -> rusqlite::Result<i64> {
    c.execute(
        "INSERT INTO account(label, email, imap_host, smtp_host, auth) VALUES(?1,?1,?2,?3,?4)",
        rusqlite::params![email, imap_host, smtp_host, auth],
    )?;
    Ok(c.last_insert_rowid())
}

/// Removes an account and everything it brought, `now` being the world's
/// clock (the queue it retires is timestamped with it).
///
/// # Errors
///
/// If the store refuses the write.
pub fn remove_account_tx(c: &rusqlite::Connection, id: i64, now: f64) -> rusqlite::Result<()> {
    // Its queued work goes with it. Only this account's worker may claim
    // these, and that worker retires with the row — so a job left behind
    // would wait for a thread that is never coming back. Obsolete rather
    // than deleted: the log keeps what was asked for.
    c.execute(
        "UPDATE effect SET status='obsolete', updated=?2
         WHERE entity=?1 AND status IN ('pending','processing')",
        rusqlite::params![format!("account:{id}"), now],
    )?;
    // Historical Mail rows share these IDs; Mail may be absent from a build.
    if c.prepare("SELECT 1 FROM sqlite_master WHERE type='table' AND name='message'")?
        .exists([])?
    {
        c.execute(
            "DELETE FROM reference WHERE message IN (SELECT id FROM message WHERE account=?1)",
            [id],
        )?;
        c.execute(
            "DELETE FROM server_msg WHERE message IN (SELECT id FROM message WHERE account=?1)",
            [id],
        )?;
        c.execute("DELETE FROM message WHERE account=?1", [id])?;
        c.execute("DELETE FROM folder WHERE account=?1", [id])?;
    }
    if c.prepare("SELECT 1 FROM sqlite_master WHERE type='table' AND name='calendar_source'")?
        .exists([])?
    {
        c.execute("DELETE FROM calendar_event WHERE source IN (SELECT id FROM calendar_source WHERE account=?)",[id])?;
        c.execute("DELETE FROM calendar_draft WHERE source IN (SELECT id FROM calendar_source WHERE account=?)",[id])?;
        c.execute("DELETE FROM calendar_change WHERE source IN (SELECT id FROM calendar_source WHERE account=?)",[id])?;
        c.execute("DELETE FROM calendar_availability WHERE account=?", [id])?;
        c.execute("DELETE FROM calendar_source WHERE account=?", [id])?;
    }
    c.execute("DELETE FROM account WHERE id=?1", [id])?;
    Ok(())
}
