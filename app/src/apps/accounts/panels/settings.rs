//! Shared account settings and service controls.
//!
//! Settings are not a shell panel: what a person configures belongs to the
//! app it configures. Accounts owns shared identities, their hosts,
//! what the last pass said, and the link to the form that adds one.
//!
//! The panel owns nothing. The rows are a cached query on every draw, so an
//! account a worker has just synced changes its own line.

use std::any::Any;
use std::rc::Rc;

use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::session::{Edit, Session};
use kernel::store::Store;

use super::AddAccount;
use crate::identity::accounts::{self, Account};
use crate::identity::history::AccountRemoved;

/// The letter the *add account* link wears. The `d` of "add", because `a` is
/// what every list's *archive* wears and `s` is *sync*.
pub const ACCEL_ADD_ACCOUNT: char = 'd';

/// The accounts panel.
pub struct Settings {
    id: PanelId,
    store: Rc<Store>,
    slot: SlotId,
    pub confirming: Option<i64>,
}

enum ServiceChange {
    Consent,
    Changed {
        before: (bool, bool),
        after: (bool, bool),
    },
}

impl Settings {
    pub fn shared() -> PanelId {
        PanelId::bare(Tag("accounts"))
    }

    pub const TAG: Tag = Tag("settings");

    /// The identity of the one settings panel.
    #[must_use]
    pub fn id() -> PanelId {
        PanelId::bare(Self::TAG)
    }

    /// The rows, as the panel draws them.
    #[must_use]
    pub fn accounts(&self) -> Rc<Vec<Account>> {
        accounts::accounts(&self.store)
    }

    /// Validates current grants and pending writes in the committing transaction.
    pub fn service(&mut self, s: &mut Session, id: i64, calendar: bool) {
        let slot = self.slot;
        s.act_async(
            Edit::writing("accounts.service", "change account services", move |tx| {
                let (mail, cal, scopes, auth): (bool, bool, String, Option<String>) = tx
                    .query_row(
                        "SELECT mail_enabled,calendar_enabled,scopes,auth FROM account WHERE id=?",
                        [id],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                    )?;
                let enabled = if calendar { cal } else { mail };
                let google = auth.as_deref() == Some(crate::identity::oauth::GOOGLE.name);
                let needed = crate::identity::required_scopes(!calendar, calendar);
                if !enabled
                    && (calendar || google)
                    && needed
                        .split_whitespace()
                        .filter(|scope| scope.starts_with("https://"))
                        .any(|scope| !scopes.split_whitespace().any(|held| held == scope))
                {
                    return Ok(Ok(ServiceChange::Consent));
                }
                if let Err(error) = crate::identity::pending(tx, id) {
                    return Ok(Err(error));
                }
                let after = (
                    if calendar { mail } else { !mail },
                    if calendar { !cal } else { cal },
                );
                crate::identity::set_services(tx, id, after.0, after.1)?;
                Ok(Ok(ServiceChange::Changed {
                    before: (mail, cal),
                    after,
                }))
            })
            .record_if(|done| matches!(done, Ok(ServiceChange::Changed { .. }))),
            move |s, done| match done {
                Some(Ok(ServiceChange::Consent)) => s.nav_within(Nav::Open {
                    from: slot,
                    id: AddAccount::for_service(id, calendar),
                    fresh: false,
                }),
                Some(Ok(ServiceChange::Changed { before, after })) => {
                    s.claim(Box::new(crate::identity::ServicesChanged {
                        account: id,
                        before,
                        after,
                    }))
                }
                Some(Err(error)) => s.notify(error, true),
                None => {}
            },
        );
    }
    pub fn reconnect(&mut self, s: &mut Session, id: i64) {
        s.nav_within(Nav::Open {
            from: self.slot,
            id: AddAccount::for_account(id),
            fresh: false,
        });
    }
    pub fn ask_remove(&mut self, s: &mut Session, id: i64) {
        if self.confirming == Some(id) {
            self.confirming = None;
            self.remove(s, id);
        } else {
            self.confirming = Some(id);
            s.redraw();
        }
    }
    /// Removes the account only after pending protocol writes have settled.
    pub fn remove(&mut self, s: &mut Session, id: i64) {
        let Some(account) = self
            .accounts()
            .iter()
            .find(|account| account.id == id)
            .cloned()
        else {
            return;
        };
        let now = s.now();
        s.act_async(
            Edit::writing(
                "account",
                format!("remove account {}", account.email),
                move |tx| {
                    if let Err(error) = crate::identity::pending(tx, id) {
                        return Ok(Err(error));
                    }
                    let email: String =
                        tx.query_row("SELECT email FROM account WHERE id=?", [id], |r| r.get(0))?;
                    accounts::remove_account_tx(tx, id, now)?;
                    Ok(Ok(email))
                },
            )
            .about(format!("account:{id}"))
            .record_if(Result::is_ok),
            move |s, done| match done {
                Some(Ok(email)) => {
                    s.claim(Box::new(AccountRemoved {
                        email: email.clone(),
                    }));
                    s.notify(format!("{email} removed"), false);
                }
                Some(Err(error)) => s.notify(error, true),
                None => {}
            },
        );
    }
}

impl Panel for Settings {
    fn id(&self) -> &PanelId {
        &self.id
    }

    fn title(&self) -> String {
        if self.id.tag == Self::TAG {
            "settings"
        } else {
            "accounts"
        }
        .into()
    }

    /// The accounts, and the one thing that is deliberately not here.
    fn about(&self) -> String {
        "Shared Mail and Google Calendar accounts: one row per row of `account`, with the address, the \
         IMAP host and what the last sync pass wrote on it, each of the three \
         selectable so an error can be carried somewhere else, plus a *remove* \
         per row. It takes no arguments and owns nothing — the rows are a \
         cached query, so an account a worker has just synced changes its own \
         line. No secret is here or in the store: an app password lives in the \
         keychain under the address and a Google refresh token under a key of \
         its own. What a person does here is read a failing account, remove \
         one, or go on to the form that adds one."
            .into()
    }

    fn wish(&self, _cols: usize) -> (u32, u32) {
        (4, 4)
    }

    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }

    /// One link: the form that adds an account, joined to the right. The
    /// *remove* buttons belong to their rows, not to the bar — a bar's
    /// letters are unique within one bar, and there are as many of those as
    /// there are accounts.
    fn verbs(&self) -> Vec<Verb> {
        vec![Verb::go(
            "accounts.add_account",
            "add account",
            Some(ACCEL_ADD_ACCOUNT),
            Nav::Open {
                from: self.slot,
                id: AddAccount::id(),
                fresh: false,
            },
        )]
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

/// Its factory.
pub struct SettingsKind;

impl PanelKind for SettingsKind {
    fn tag(&self) -> Tag {
        Settings::TAG
    }

    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        Box::new(Settings {
            id: id.clone(),
            store: cx.session().store().clone(),
            slot: 0,
            confirming: None,
        })
    }
}
