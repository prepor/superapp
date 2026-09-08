# Accounts

Accounts is the shared list of identities used by Mail and Google Calendar.
Open **accounts** from the launcher, or from Calendar's sources panel. Each row
shows its address, connection status, Mail and Calendar service controls, and
Google reconnect when applicable. Password accounts continue to support IMAP
and SMTP.

**add account** offers Google sign-in with Mail and Calendar service choices,
plus the existing password/IMAP form. Enabling a Google service whose scope is
missing opens consent for that account. Reconnecting requests the union of
existing and selected scopes. A partial grant cannot replace access another
service still needs, and a different signed-in identity cannot overwrite the
selected account.

Google's installed-application flow does not support incremental authorization,
so the browser receives the complete requested scope set. The loopback listener,
PKCE verifier and state are created before opening the browser. The token
endpoint's verified email and Google subject identify the account. Reconnecting
preserves its local ID, caches and keychain naming; an email change with the
same verified subject updates the existing identity.

## Storage and compatibility

The historical `account` table remains in place, preserving account IDs and all
Mail metadata. It gains `google_sub`, `scopes`, `mail_enabled` and
`calendar_enabled`. Existing accounts default to Mail enabled and Calendar
disabled. Enabling Calendar therefore requires deliberate consent, while old
Mail grants keep working.

The shared `identity` module owns account queries, lifecycle/history, migration,
OAuth and token refresh. Accounts has its own schema ladder. Mail retains an
idempotent migration bridge so existing stores and Mail-only test builds keep
working; it no longer owns the account settings UI. The historical `settings`
and `add_account` tags are registered by Accounts, preserving restored panels.

| Value | Storage |
|---|---|
| Email, Google subject, granted scope names and enabled services | SQLite |
| IMAP/SMTP password and Google refresh grant | platform secret store |
| Access tokens | process memory |
| OAuth client registration | existing `google-oauth.json` beside the store |

Mail and Calendar share the refresh implementation and key naming. Replacing a
refresh grant invalidates the associated cached access token. Neither secrets
nor token-bearing request data are exposed through app descriptions or tools.
Google consent is unavailable in tests, scripted runs and panel-library mounts.

Disabling a service stops its workers/queries while preserving cached data.
Removing an account requires a second click and removes its local dependent
caches and drafts. Pending outgoing work must finish first. Remote Google events
are not deleted by disconnecting an account. Removal cannot reconstruct erased
caches through workspace undo. Service switches are undoable.

## Google registration

Use the existing desktop client registration documented in
[Mail](./mail.md#gmail-sign-in), enable the Calendar API in that Google Cloud
project, and configure the consent screen for the scopes used:

- `openid` and `email` for the identity;
- `https://mail.google.com/` when Mail is selected;
- `calendar.calendarlist.readonly`, `calendar.events`, and
  `calendar.events.freebusy` under `https://www.googleapis.com/auth/` when
  Calendar is selected.

Scopes and account permissions are separate: a granted API scope cannot make
a read-only calendar writable or reveal unshared participant availability.
Google Workspace administrators may impose additional restrictions. See
[Google Calendar authorization](https://developers.google.com/workspace/calendar/api/auth)
and [desktop OAuth](https://developers.google.com/identity/protocols/oauth2/native-app).

## Agents

`accounts.list` reads account IDs, addresses, providers, enabled services,
granted scope names, and Mail status. It exposes no credentials. The Accounts
app and panels describe the shared identity model so agents can choose the
correct account and direct a person to browser consent when access is missing.
