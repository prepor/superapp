//! Shared Google identities and per-service access. Owns the historic Mail
//! settings/add_account tags so restored workspaces still open the same data.
use kernel::{
    app::{App, Capabilities, Env, Mode, Root, Schema},
    panel::{Opening, Panel, PanelId, PanelKind, Tag},
    tool::Tool,
};
use serde_json::json;
use std::any::Any;
pub mod panels;
mod ui;
pub mod widgets;
pub use ui::UI;
pub struct Accounts;
pub static ACCOUNTS: Accounts = Accounts;
/// Only a real, unscripted window may start browser consent.
pub struct GoogleConsent;
struct SharedKind;
impl PanelKind for SharedKind {
    fn tag(&self) -> Tag {
        Tag("accounts")
    }
    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        panels::SettingsKind.open(id, cx)
    }
}
static KINDS: &[&dyn PanelKind] = &[&SharedKind, &panels::SettingsKind, &panels::AddAccountKind];
impl App for Accounts {
    fn id(&self) -> &'static str {
        "accounts"
    }
    fn kinds(&self) -> &'static [&'static dyn PanelKind] {
        KINDS
    }
    fn schema(&self) -> Option<&'static Schema> {
        Some(&crate::identity::SCHEMA)
    }
    fn outside(&self, mode: Mode, env: &Env, caps: &mut Capabilities) {
        if mode == Mode::Real && !env.scripted && !env.clock.is_virtual() {
            caps.insert::<GoogleConsent>(Box::new(GoogleConsent));
        }
    }
    fn roots(&self) -> Vec<Root> {
        vec![Root::new(
            panels::Settings::shared(),
            "accounts",
            "settings Google Mail Calendar IMAP SMTP sign in",
        )]
    }
    fn describe(&self) -> Option<&'static str> {
        Some("Shared accounts: account preserves existing IDs, email addresses and Mail configuration. google_sub is the verified Google identity, scopes records granted API permissions, mail_enabled/calendar_enabled choose services. Accounts is the place to add, reconnect, enable and remove accounts. Mail and Calendar use the same Google refresh grant; enabling Calendar requires fresh consent for the union of required scopes. Password/IMAP accounts support Mail only. A disabled service keeps its cache but stops synchronization. No passwords, refresh tokens, access tokens or OAuth client secrets are in SQL, panel context or tools. Removing an account removes dependent caches and must not abandon pending writes. Use accounts.list to inspect capabilities; Google consent happens in the browser.")
    }
    fn tools(&self) -> Vec<Tool> {
        vec![Tool::new("accounts.list","List shared accounts, enabled services, granted scopes and Mail status. Contains no credentials.",json!({"type":"object","properties":{},"additionalProperties":false}),false,|s,_|{let rows=s.store().rows_sql("shared accounts","shared identities and service access","SELECT id,label,email,auth,mail_enabled,calendar_enabled,scopes,status FROM account ORDER BY id",&[],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"label":r.get::<_,String>(1)?,"email":r.get::<_,String>(2)?,"provider":r.get::<_,Option<String>>(3)?,"mail":r.get::<_,bool>(4)?,"calendar":r.get::<_,bool>(5)?,"scopes":r.get::<_,String>(6)?,"mail_status":r.get::<_,Option<String>>(7)?})));Ok(json!(rows.as_ref()))})]
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
