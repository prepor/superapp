//! Username completion in the composer: cached chat participants immediately,
//! then Telegram's mention search for members outside the loaded history.

use kernel::richtable::{Completion, Suggestion, MAX_SUGGESTIONS};
use kernel::store::Store;

use super::model::PeerId;
use super::panel_read::Read;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Context {
    pub start: usize,
    pub end: usize,
    pub partial: String,
}

fn username_char(ch: char) -> bool { ch.is_alphanumeric() || ch == '_' }

pub fn context(text: &str, cursor: usize) -> Option<Context> {
    let before = text.get(..cursor)?;
    let start = before.rfind('@')?;
    if before[..start].chars().next_back().is_some_and(|ch|
        !ch.is_whitespace() && !matches!(ch, '(' | '[' | '{' | '"' | '\''))
    { return None; }
    let partial = &before[start + 1..];
    if !partial.chars().all(username_char) { return None; }
    let end = cursor + text[cursor..].find(|ch: char| !username_char(ch))
        .unwrap_or(text.len() - cursor);
    Some(Context { start, end, partial: partial.to_lowercase() })
}

#[derive(Default)]
pub struct Mentions {
    chat: PeerId,
    topic: i64,
    query: Option<String>,
    ready_at: f64,
    sent: bool,
    read: Option<Read>,
    members: Vec<PeerId>,
}

impl Mentions {
    /// The request is debounced and owned by this exact chat and query.
    pub fn track(&mut self, store: &Store, chat: PeerId, topic: i64, ctx: Option<&Context>, now: f64) -> bool {
        let query = ctx.map(|ctx| ctx.partial.clone());
        let changed = (self.chat, self.topic) != (chat, topic) || self.query != query;
        if changed {
            self.chat = chat;
            self.topic = topic;
            self.query = query;
            self.ready_at = now + 0.2;
            self.sent = false;
            self.read = None;
            self.members.clear();
        }
        if let Some(result) = self.read.as_ref().and_then(|read| read.poll(now)) {
            self.read = None;
            if let Ok(value) = result {
                self.members = value["members"].as_array().into_iter().flatten()
                    .filter_map(|member| member["member_id"]["user_id"].as_i64()).collect();
            }
            return true;
        }
        if !self.sent && now >= self.ready_at && super::panels::live(store) {
            if let Some(query) = &self.query {
                self.sent = true;
                self.read = Some(Read::start(store,
                    &super::requests::search_mention_members(chat, topic, query), now));
            }
        }
        changed
    }
}

impl Completion for Mentions {
    type Ctx = Context;

    fn context(&self, text: &str, cursor: usize) -> Option<Context> { context(text, cursor) }

    fn offer(&self, store: &Store, ctx: &Context) -> Vec<Suggestion> {
        let members = serde_json::to_string(&self.members).expect("member ids");
        let people = store.snapshot_rows_sql("telegram.mention_candidates", "mentionable chat participants",
            "SELECT p.name, p.username FROM tg_peer p
             WHERE p.kind = 'person' AND p.username IS NOT NULL AND p.username != ''
             AND (p.id = ?1 OR p.id IN (SELECT peer FROM tg_member WHERE chat = ?1)
                 OR p.id IN (SELECT sender FROM tg_message WHERE chat = ?1)
                 OR p.id IN (SELECT value FROM json_each(?2)))
             ORDER BY p.name COLLATE NOCASE, p.id", &[self.chat.into(), members.into()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)));
        people.iter().filter(|(name, username)|
            name.to_lowercase().contains(&ctx.partial) || username.to_lowercase().contains(&ctx.partial))
            .take(MAX_SUGGESTIONS)
            .map(|(name, username)| Suggestion::labeled(name, format!("@{username}"))).collect()
    }

    fn splice(&self, text: &str, _cursor: usize, ctx: &Context, pick: &Suggestion) -> (String, usize) {
        // Replace the whole token even when the caret is in its middle.
        let after = &text[ctx.end..];
        let space = if after.starts_with(char::is_whitespace) { "" } else { " " };
        let value = format!("{}{space}", pick.value);
        (format!("{}{}{}", &text[..ctx.start], value, after), ctx.start + value.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::telegram::{runtime, seed, TELEGRAM};
    use kernel::session::Session;
    use serde_json::{json, Value};

    static APPS: &[&dyn kernel::app::App] = &[&TELEGRAM];

    #[test]
    fn mentions_start_at_word_boundaries_and_replace_the_entire_token() {
        for text in ["@", "hi @ve", "👋 (@ve", "a\n@ve", "@Ива"] {
            assert!(context(text, text.len()).is_some(), "{text}");
        }
        for text in ["", "mail@vera", "https://t.me/@ve", "@@ve", "@ve ", "@ve!", "hey@"] {
            assert!(context(text, text.len()).is_none(), "{text}");
        }
        assert!(context("👋 @ve", 1).is_none());
        let text = "👋 @verax tomorrow";
        let cursor = "👋 @ve".len();
        let ctx = context(text, cursor).unwrap();
        let (after, caret) = Mentions::default().splice(text, cursor, &ctx, &Suggestion::value("@vera"));
        assert_eq!(after, "👋 @vera tomorrow");
        assert_eq!(&after[..caret], "👋 @vera");
        let ctx = context("@ve", 3).unwrap();
        assert_eq!(Mentions::default().splice("@ve", 3, &ctx, &Suggestion::value("@vera")),
            ("@vera ".into(), 6));
    }

    #[test]
    fn offers_match_names_and_handles_and_stay_in_the_chat() {
        let session = Session::fake(APPS);
        let mut mentions = Mentions::default();
        let ctx = context("@pet", 4).unwrap();
        mentions.track(session.store(), seed::STELAXIS, 0, Some(&ctx), 0.0);
        let offer = mentions.offer(session.store(), &ctx);
        assert!(offer.iter().any(|s| s.value == "@ivanp"), "search a participant's surname");
        mentions.track(session.store(), seed::VERA, 0, Some(&ctx), 0.0);
        assert!(mentions.offer(session.store(), &ctx).iter().all(|s| s.value != "@ivanp"));
        let ctx = context("@ve", 3).unwrap();
        mentions.track(session.store(), seed::VERA, 0, Some(&ctx), 0.0);
        assert!(mentions.offer(session.store(), &ctx).iter().any(|s| s.value == "@vera"));
    }

    #[test]
    fn member_search_is_debounced_scoped_to_topics_and_discards_old_replies() {
        let session = Session::fake(APPS);
        let store = session.store();
        let rt = runtime::of(store);
        let inbox = rt.connect();
        let mut mentions = Mentions::default();
        let ctx = context("@iv", 3).unwrap();
        mentions.track(store, seed::STELAXIS, 2, Some(&ctx), 0.0);
        assert!(inbox.try_recv().is_err());
        mentions.track(store, seed::STELAXIS, 2, Some(&ctx), 0.3);
        let request: Value = serde_json::from_str(&inbox.try_recv().unwrap()).unwrap();
        assert_eq!(request["filter"]["topic_id"]["forum_topic_id"], 2);
        assert_eq!(request["query"], "iv");
        let first = super::super::panel_read::id(request["@extra"]["context"].as_str().unwrap()).unwrap();
        let next = context("@ve", 3).unwrap();
        mentions.track(store, seed::STELAXIS, 2, Some(&next), 0.4);
        assert!(!rt.reads.lock().unwrap().alive(first));
        rt.reads.lock().unwrap().finish(first, Ok(json!({"members": [{"member_id": {"user_id": 42}}]})));
        mentions.track(store, seed::STELAXIS, 2, Some(&next), 0.7);
        let request: Value = serde_json::from_str(&inbox.try_recv().unwrap()).unwrap();
        assert_eq!(request["query"], "ve");
        let id = super::super::panel_read::id(request["@extra"]["context"].as_str().unwrap()).unwrap();
        rt.reads.lock().unwrap().finish(id, Ok(json!({"members": [{"member_id": {"user_id": 73}}]})));
        mentions.track(store, seed::STELAXIS, 2, Some(&next), 0.8);
        assert_eq!(mentions.members, [73]);
        mentions.track(store, seed::STELAXIS, 2, None, 0.9);
        assert!(mentions.members.is_empty());
    }
}
