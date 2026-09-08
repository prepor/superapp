//! Mail-specific credential and contact lookups over shared identities.
use kernel::caps::Secrets;
use kernel::store::{Q,Store,Val};
use super::caps::Creds;
pub use crate::identity::accounts::{accounts,account_for};
static Q_CONTACT: Q = Q {
    id: "contact",
    sql: "SELECT from_name, COUNT(*) FROM message WHERE from_email = ?1",
    describe: "a sender's display name and how many mails they sent",
};

/// A sender's `(name, mail count)`; the name falls back to the address.
#[must_use]
pub fn contact(store: &Store, email: &str) -> (String, i64) {
    store
        .rows(&Q_CONTACT, &[Val::S(email.to_string())], |r| {
            Ok((r.get::<_, Option<String>>(0)?, r.get::<_, i64>(1)?))
        })
        .first()
        .map(|(name, n)| {
            (
                name.clone()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| email.to_string()),
                *n,
            )
        })
        .unwrap_or_else(|| (email.to_string(), 0))
}

/// The credentials an account with a password signs in with.
///
/// The account row's `auth` picks the mechanism, and the two secrets live in
/// different places for different lengths of time: an app password is read
/// straight out of the keychain — this — while a Gmail account's bearer token
/// is minted (or recalled from the process cache) by
/// [`OAuth`](super::caps::OAuth) and wrapped in
/// [`Creds::bearer`](super::caps::Creds::bearer). Two doors because the two
/// backends are two capabilities and a bag is borrowed one at a time; the
/// *choice* between them is made in exactly two places, and both read the
/// same column ([`sync::creds`](super::sync::creds) for a session,
/// [`Submit`](super::effects::Submit) for a submission).
///
/// # Errors
///
/// If the keychain has no password for the address.
pub fn creds_for(secrets: &mut dyn Secrets, email: &str, host: &str) -> Result<Creds, String> {
    let pass = secrets.get(email).ok_or("no password in the keychain")?;
    Ok(Creds::password(host, email, pass))
}
