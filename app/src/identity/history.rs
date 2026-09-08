//! Account history belongs to the shared identity, independent of its services.
use super::accounts;
use kernel::{effect::World, history::Intent};

type ServiceSnapshot = (bool, bool, String, Option<String>);

/// Adding an account. Reversible while it is still empty, which it is at the
/// moment it is added.
pub struct AccountAdded {
    pub id: i64,
    pub email: String,
    pub imap: String,
    pub smtp: String,
    /// The `account.auth` word, so a redo restores a Gmail account as a
    /// Gmail account rather than as one asking for a password.
    pub auth: String,
    pub services: std::sync::Mutex<Option<ServiceSnapshot>>,
}

impl Intent for AccountAdded {
    fn describe(&self) -> String {
        format!("account:{} added", self.id)
    }
    fn reverse(&self, w: &World) -> Result<(), String> {
        let (id, now) = (self.id, w.now());
        crate::identity::pending(w.store().conn(), id)?;
        let snapshot = w
            .store()
            .conn()
            .query_row(
                "SELECT mail_enabled,calendar_enabled,scopes,google_sub FROM account WHERE id=?",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .map_err(|e| e.to_string())?;
        *self
            .services
            .lock()
            .map_err(|_| "account history is unavailable")? = Some(snapshot);
        w.store()
            .write(move |c| accounts::remove_account_tx(c, id, now))
            .map_err(|e| e.to_string())
    }
    fn reapply(&self, w: &World) -> Result<(), String> {
        let (id, email, imap, smtp, auth) = (
            self.id,
            self.email.clone(),
            self.imap.clone(),
            self.smtp.clone(),
            self.auth.clone(),
        );
        let services = self
            .services
            .lock()
            .map_err(|_| "account history is unavailable")?
            .clone();
        w.store()
            .write(move |c| {
                c.execute(
                    "INSERT INTO account(id, label, email, imap_host, smtp_host, auth)
                     VALUES(?1, ?2, ?2, ?3, ?4, ?5)",
                    rusqlite::params![id, email, imap, smtp, auth],
                )?;
                if let Some((mail,calendar,scopes,subject))=services{
                    c.execute("UPDATE account SET mail_enabled=?1,calendar_enabled=?2,scopes=?3,google_sub=?4 WHERE id=?5",rusqlite::params![mail,calendar,scopes,subject,id])?;
                }
                Ok(())
            })
            .map_err(|e| e.to_string())
    }
}

/// Removing an account takes its mail with it, and no snapshot brings that
/// back. Stated honestly rather than half-restored: the node goes expired and
/// the walk steps past it.
pub struct AccountRemoved {
    pub email: String,
}

impl Intent for AccountRemoved {
    fn describe(&self) -> String {
        format!("account {} removed", self.email)
    }
    fn blocked(&self, _w: &World) -> Option<String> {
        Some("an account's cached mail and calendar data cannot be restored".into())
    }
    fn reverse(&self, _w: &World) -> Result<(), String> {
        Ok(())
    }
    fn reapply(&self, _w: &World) -> Result<(), String> {
        Ok(())
    }
}
