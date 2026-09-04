//! Mail's accounts, and what can be done to one.
//!
//! Settings are not a shell panel: what a person configures belongs to the
//! app it configures. This is mail's — the accounts it syncs, their hosts,
//! what the last pass said, and the link to the form that adds one.
//!
//! The panel owns nothing. The rows are a cached query on every draw, so an
//! account a worker has just synced changes its own line.

use std::any::Any;
use std::rc::Rc;

use kernel::history::Intent;
use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::session::{Action, Session};
use kernel::store::Store;

use super::super::accounts::{self, Account};
use super::super::effects::{AccountAdded, AccountRemoved};
use super::AddAccount;

/// The letter the *add account* link wears. The `d` of "add", because `a` is
/// what every list's *archive* wears and `s` is *sync*.
pub const ACCEL_ADD_ACCOUNT: char = 'd';

/// The accounts panel.
pub struct Settings {
    id: PanelId,
    store: Rc<Store>,
    slot: SlotId,
}

impl Settings {
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

    /// Removes one account and everything it brought.
    ///
    /// Undo cannot bring the mail back and says so ([`AccountRemoved`] is
    /// blocked): the row goes, its folders and letters go with it, and the
    /// worker that was syncing them retires on the kick this action ends
    /// with. Called by the widget, which knows which row was pressed.
    pub fn remove(&mut self, s: &mut Session, id: i64) {
        let Some(a) = self.accounts().iter().find(|a| a.id == id).cloned() else {
            return;
        };
        let now = s.now();
        let email = a.email.clone();
        let done = s.act(
            Action::writing("account", format!("remove account {email}"), move |tx| {
                accounts::remove_account_tx(tx, id, now)
            })
            .about(super::super::effects::account_entity(id))
            .claiming(vec![Box::new(AccountRemoved {
                email: a.email.clone(),
            }) as Box<dyn Intent>]),
        );
        if done.is_some() {
            s.notify(format!("{email} removed"), false);
        }
    }

    /// Files an account row as an undoable action and claims the intent. The
    /// new row's id comes back from `act`, so the claim needs no shared cell.
    ///
    /// Both doors come through here — the password form and the end of a
    /// Gmail sign-in — because what differs between them is one word.
    /// Answers the new row's id, or zero when the write was refused.
    pub fn add(s: &mut Session, email: &str, imap: &str, smtp: &str, auth: &str) -> i64 {
        let (e, i, sm, au) = (
            email.to_string(),
            imap.to_string(),
            smtp.to_string(),
            auth.to_string(),
        );
        let id = s
            .act(Action::writing(
                "account",
                format!("add account {email}"),
                move |tx| accounts::add_account_tx(tx, &e, &i, &sm, &au),
            ))
            .unwrap_or(0);
        if id == 0 {
            return 0;
        }
        // The claim needs the row id the write answered with, so it is added
        // to the head node after the fact.
        s.claim(Box::new(AccountAdded {
            id,
            email: email.to_string(),
            imap: imap.to_string(),
            smtp: smtp.to_string(),
            auth: auth.to_string(),
        }));
        // A new account is a new sync pass; the kernel re-asks the apps for
        // the set after every action, so this only has to have happened
        // inside one.
        id
    }
}

impl Panel for Settings {
    fn id(&self) -> &PanelId {
        &self.id
    }

    fn title(&self) -> String {
        "settings".into()
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
            "mail.add_account",
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
        })
    }
}
