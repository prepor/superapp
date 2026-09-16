# CR-021 · Telegram: the comments under a post

**Status:** **implemented** (Andrey, 2026-09-15: "telegram: support of
channel comments"; then, on the plan: "ok, implement with your
suggestions"). Written as the book should read once the change has landed,
with the phases and the open decisions at the end; what landed, and where it
went differently, is in *Progress* at the foot.

## Why

A channel post already says how many comments it has, and there is no way to
read one. `updates::message` takes `interaction_info.reply_info.reply_count`
into the row (`updates.rs:69`), the transcript and the line card print it
under the reactions in grey — `8 comments` (`widgets/chat.rs:1491`,
`widgets/line.rs:299`) — and that is the whole of it. `message_topic` reads
`messageTopicForum` and drops every other kind on purpose: *"Ordinary chats,
channel comments and direct-message topics stay distinct"* (`updates.rs:96`).

So a channel is the one conversation this app draws with no composer and no
way in, and for a reader whose Telegram is mostly channels the comments are
half of what is written there — the post is the headline and the comments
are the paper.

Nothing here is new machinery. A post's comments are a part of a chat, as a
forum topic is: their own history walk, their own draft, their own read
cursor, their own unread line, sent into with the same `sendMessage` under a
different `topic_id`. What is new is that the part lives in *another chat* —
the channel's discussion group — one pair of columns and one table to say
so, one way in from the post, and the rule about joining a group before
writing in it.

## The reference

The two official clients were read for what the flow *does*. Pointers are
into `overtake/TelegramSwift` at `579cebbf` (the Mac, the same pin the book
already cites) and `telegramdesktop/tdesktop` at `272f6f5c`, with the phone
(`DrKLO/Telegram`) checked where the two differ.

- **A channel keeps its comments in a group.** A broadcast channel may have
  a **discussion group** linked to it. Every post is copied into that group
  as an automatic forward, and a comment is an ordinary message in the group
  answering that copy. The copy is the thread's **root**; the post itself
  stays in the channel.
- **The button under a post.** `ChatRowItem.channelHasCommentButton`
  (`Telegram-Mac/ChatRowItem.swift:1458`) shows it when the peer is a
  broadcast with `hasDiscussionGroup` *and* the message carries a reply-thread
  attribute whose `commentsPeerId` is the discussion group the client knows.
  `commentsBubbleData` (same file, 1546) writes the words: **Leave a comment**
  at zero, else **N Comments** with thousands separated, the avatars of the
  latest repliers beside it, and an unread dot when
  `maxReadMessageId < maxMessageId`. A post still sending shows the button
  only once the discussion group is known. The phone draws the same strip —
  `ChatMessageCell.drawCommentLayout`.
- **What opens.** A chat controller in thread mode — `.comments(origin:)`
  for a channel post, `.replies(origin:)` for a group's own reply thread
  (`Telegram-Mac/AccountContext.swift`, `InAppLinks.swift`). It shows the
  post at the top, then a service caption — `chatCommentsHeaderEmpty` /
  `chatCommentsHeaderFull`, *no comments yet* / *comments*
  (`ChatCommentsHeaderItem.swift:42`) — then the comments, then a composer.
- **Joining to write.** In thread mode, where the group's participation
  status is *left* and the group has the `joinToSend` flag, the composer
  gives way to a **Join** action
  (`ChatPresentationInterfaceState.swift:947`). tdesktop draws the same
  button under the same condition —
  `BottomControls::isJoinGroup()` is `!channel->amIn() && !CanSendAnything`
  in `Replies` mode (`history_view_bottom_controls.cpp:552`). This is not a
  formality: TDLib's own schema says `join_to_send_messages` "may be false
  only for discussion supergroups", which is precisely the case where a
  stranger may comment without joining.
- **Reading.** The thread has its own read cursor — `messageReplyInfo`
  carries `last_read_inbox_message_id` beside `last_message_id` — and
  reading comments does not read the group.

## The words

- A **discussion group** is the supergroup a channel's comments live in.
  `supergroup.has_linked_chat` says a channel has one;
  `supergroupFullInfo.linked_chat_id` names it.
- The **root** is the post's own copy in that group, which every comment
  answers. Its id is the thread's id.
- A **thread** is one post's comments: the pair *(group, root)*, reached
  from the pair *(channel, post)*.
- A **comment** is a message in the discussion group belonging to a thread.
  It is an ordinary message: it can be replied to, reacted to, forwarded,
  edited, deleted and searched like any other.

## Where in a chat a panel stands

Today a panel says where it stands with a chat and an integer: `(peer,
topic)`, zero meaning the whole chat. That pair runs through the model's
queries, the transcript's loader key, the history pages, the attach and
place panels' ids, the requests and the runtime's loading flags — about a
hundred lines mention it, most of them tests.

The pair becomes a chat and a **scope**:

```rust
/// Where in a chat a panel stands: the whole of it, one forum topic of it,
/// or one post's comments — a thread, whose root is the post's own copy in
/// the discussion group every comment answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Scope {
    #[default]
    Whole,
    Topic(i64),
    Thread(MsgId),
}
```

`history_in(store, chat, scope)`, `first_unread_in`, `loading_in`,
`newest_ordinary_line_in`, `trim_topic` (which becomes `trim_scope`),
`Attach::in_scope`, `Place::in_scope`, `transcript::Key` and `Page` all take
it in place of the integer. Two SQL parameters rather than one, exactly one
of them ever nonzero:

```sql
WHERE m.chat = ?1 AND (?2 = 0 OR m.topic = ?2) AND (?3 = 0 OR m.thread = ?3)
```

A forum topic and a comment thread are both named by a message id in the
same chat, and they never collide — no message is both a topic's root and a
post's copy — but they are not the same thing to the wire, and one column
holding either would leave every request guessing which. Two columns, and
the question never arises. A discussion group that is *also* a forum keeps
both: its messages carry a topic and a thread.

## What the projection holds

One column and one table.

```sql
-- On tg_message, beside `topic`: the root of the thread this line is a
-- comment in, which is a message id in this same chat. 0 for everything
-- else, including the root itself when nobody has commented yet.
ALTER TABLE tg_message ADD COLUMN thread INTEGER NOT NULL DEFAULT 0;
CREATE INDEX tg_message_thread ON tg_message(chat, thread, date DESC, id DESC);

-- One post's comments. Keyed by the post, because the post is what a
-- person points at and what the panel is named after; the group and the
-- root are what the wire answers when asked.
CREATE TABLE tg_thread(
  chat      INTEGER NOT NULL REFERENCES tg_peer(id),  -- the channel
  post      INTEGER NOT NULL,                         -- the post in it
  -- Where the comments are. NULL until getMessageThread has answered.
  group_id  INTEGER,
  root      INTEGER,
  -- How many, as reply_info says; the number the post's foot draws.
  count     INTEGER NOT NULL DEFAULT 0,
  -- The newest comment, and how far I have read. Forward-only: see below.
  last      INTEGER,
  last_read INTEGER,
  draft     TEXT,
  PRIMARY KEY(chat, post)
);
CREATE INDEX tg_thread_root ON tg_thread(group_id, root);
```

`tg_message.comments` stays where it is and goes on being the drawn count;
`tg_thread` is what the comments panel and the *new* mark are read from.

**The rung.** This is a new step on the append-only ladder, and it must sit
**before** the `telegram:column-order` derived rung, whose canonical is
built from `SCHEMA.steps[..19]` (`schema.rs:137`) — that slice grows by one,
as `v19_live_updated` already demonstrates for a column added late
(`schema.rs:72`). A rung added after it would leave every store a column
wider than the canonical and the repair would refuse them all.

**Retention.** The newest 10,000 lines are kept per part of a chat
(`project::trim_topic`, `project.rs:501`), so a thread keeps its own ten
thousand and a busy discussion group cannot evict the comments under a post
somebody is reading.

**Who else is told.** A discussion group I am not a member of still gets a
`tg_chat` row, with `in_main = 0` — the column that already keeps channels
never joined out of the list (`schema.rs:795`) — so its comments have a chat
to belong to and the list stays what it was.

## The way in: a post's foot

The foot already draws the count. It becomes the way in, in the shell's own
link grammar — the words underlined, a click opens them:

| What the post has | What the foot says |
|---|---|
| no comments, and a discussion group | `leave a comment` |
| comments | `8 comments` |
| comments past my read cursor | `8 comments · new` |

*new* rather than a number: the count of unread comments is known only for a
thread we have asked the wire about (`messageThreadInfo.unread_message_count`),
and a number that is sometimes a guess is worse than a word that is always
true. Inside the panel the unread line does the counting, as it does in every
other conversation.

`comments` (`m`) stands on the chat's bar over a post that has a discussion
group, and on that post's `line` card. `m` is free on both bars — *comments*
spells it and nothing else on either bar wears it.

The same foot, and the same verb, appear on the **root** in the discussion
group, for the group I am a member of and reading directly: it is the same
thread, and it opens the same panel. The way back from the root to the post
is the root's own forward origin — `messageForwardInfo.origin`, a
`messageOriginChannel` carrying the channel and the post id. Where that
origin is missing, the way in is not offered from the group's side.

## The comments panel

It is the chat panel, standing in a thread. Everything a conversation has
comes with it: the cursor and the marks, reply, react, copy, forward,
delete, the attach panel, the media viewer, the line card, the reading
position, the unread line.

- **Its identity** is the post: `telegram-chat <channel> comments <post>`,
  beside the topic's existing `telegram-chat <chat> topic <id>`
  (`panels/chat.rs:140`). A restart restores it from the post, which is
  what the person pressed and what a `t.me` comment link names. The id
  therefore names the *channel* while the panel reads and writes the
  *group*: `Chat::of` goes on answering what the id says, and the chat the
  transcript is of becomes the panel's own question — one accessor, checked
  everywhere `self.peer` is read today.
- **Its title** is `comments · Rust Weekly`, spelled the way a topic's row
  is spelled `topic · group`.
- **Its transcript** is the group's messages in that thread: the root
  first — the post itself, drawn as the post — then a caption row,
  `comments` or `no comments yet`, then the comments. One new `Row`
  (`panels/chat.rs:47`), drawn like the day's caption.
- **Before the wire has answered**, which is the first opening of a thread
  we have never resolved, the panel draws the post from the channel's own
  cached row and says `loading comments…` where the caption will be. A
  refusal says so and offers `retry`, like every other failed load.
- **`post` (`p`)** on its bar opens the channel post's `line` card, which is
  the way back to the channel from a panel restored on its own.
- **`about` (`a`)** opens the *group's* card — the conversation the comments
  are in, with its join, leave, mute and search.
- **A channel's card** gains a line naming its discussion group, `discussion ·
  Rust Weekly chat`, which opens it. `linked_chat_id` is already in the
  `supergroupFullInfo` the projection reads for member counts
  (`updates.rs:1123`); this is one more field off the same object.

## Sending, and joining to send

The composer is the group's, in the thread. A comment is
`sendMessage` to the *group* with `topic_id: messageTopicThread`, and
everything the composer can already make goes the same way — text, an
album, a file, a voice note, a place, a forward — because they all go
through the one funnel `requests::in_topic` (`requests.rs:868`), which
becomes `requests::in_scope` and writes the thread's topic instead of the
forum's.

A draft belongs to the thread: kept in `tg_thread.draft`, sent to Telegram
as `setChatDraftMessage` with the same `topic_id` when the panel is left,
under the same 300 ms rule as every other draft.

Whether the composer is there at all follows the clients' rule, not ours:

| The discussion group | The foot of the panel |
|---|---|
| I am in it, and may write | the composer |
| I am not in it, and it takes a stranger's message | the composer |
| I am not in it, and it wants members (`join_to_send_messages`) | `join group` (`j`) |
| I am in it and restricted, or it is closed to me | the reason, in the wire's words |

`join group` is the peer card's own join (`panels/peer.rs:315`,
`requests::join_chat`) and writes nothing locally: the wire says I am in a
moment later, as it does today, and the composer appears with the answer.

Two fields are needed for that row and neither is projected yet:
`supergroup.join_to_send_messages` and the membership the group's own status
already carries — `may_post` reads that status for channels today
(`updates.rs:1082`) and learns one more answer here. `PeerCard::can_post`
(`model.rs:851`) grows the group's side of the question it already answers
for channels.

## Reading, and what is new

Comments are read by being looked at, as every other line is: the viewport's
receipts, with `messageSourceMessageThreadHistory` as the source rather than
the forum's — the same swap `in_topic` already performs for a topic
(`requests.rs:873`). The group's own unread count follows Telegram's answer;
reading a thread does not read the group.

`tg_thread.last_read` moves **forward only**, and a server snapshot is
rebased onto ours rather than replacing it — the rule the chat and topic
cursors already live by, and the one that cost a round when busy forums kept
rewinding local reads (PR #136). A thread whose `last` is past `last_read`
is what puts *new* on the post's foot.

`reply_info` arrives on the post through `updateMessageInteractionInfo`,
which the worker already projects into `views`/`comments`
(`sync.rs:1437`); it gains the two ids and the thread row beside them.

## The wire

Every call is in the installed TDLib (checked against
`/opt/homebrew/opt/tdlib/include/td/telegram/td_api.h`; none of this needs a
newer one):

| What for | The call |
|---|---|
| where a post's comments are | `getMessageThread(chat, message)` → `messageThreadInfo{chat_id, message_thread_id, reply_info, unread_message_count, messages, draft_message}` |
| whether a post has any | `messageProperties.can_get_message_thread`, and `reply_info` on the post |
| a page of comments | `getMessageThreadHistory(group, root, from, offset, limit)` |
| which thread a line is in | `message.topic_id` as `messageTopicThread{message_thread_id}` |
| sending one | `sendMessage(group, topic_id: messageTopicThread, …)` |
| its draft | `setChatDraftMessage(group, topic_id: messageTopicThread, …)` |
| reading | `viewMessages(group, ids, source: messageSourceMessageThreadHistory)` |
| the discussion group | `supergroup.has_linked_chat`, `supergroupFullInfo.linked_chat_id` |
| joining to write | `supergroup.join_to_send_messages`, `joinChat` |

The history walk is the existing one: `Page` (`sync.rs:167`) carries a scope
instead of a topic and `get_history_in` picks the third call
(`requests.rs:899`), so a thread inherits one page in flight per account,
the one-second gap, the thirty-second deadline, Telegram's retry waits, the
cancellation when the panel goes away, and the fill-before-extend rule. A
comments panel keeps its discussion group *open* (`openChat`) while it is
visible, which is what makes the wire send new comments for a group I am not
a member of — the same subscription the chat panel already takes for its own
chat.

## The library and the suites

- `seed.rs` grows **Rust Weekly chat**, the discussion group behind the
  existing `RUST_WEEKLY` channel (`seed.rs:30`): the root copies of two
  posts, eight comments under one of them with two of them unread, and one
  post with none. The group is `in_main = 0` — a group I read the comments
  of without having joined — so the fixtures carry the join case too.
- `scenes.rs` grows a `comments` scene beside `chat`: the post with its
  foot, the panel with the root and the caption, the panel with `join
  group` in place of the composer, and the empty thread's *leave a comment*.
- `e2e/telegram/comments.txt`: walk a channel, press `comments` on a post,
  see the root and the comments, type a comment and send it, come back and
  see the foot say the new count; then the post with none, and the group
  that wants joining.

## Phases

Each phase leaves the tree green — `cargo clippy --workspace --all-targets
--locked --no-default-features -- -D warnings`, `cargo clippy -p superapp
--all-targets --locked -- -D warnings`, `cargo test --workspace --locked
--no-default-features`, `MAKEPAD=headless cargo build -p superapp
--no-default-features` then `./e2e/run-all.sh`, and `mdbook build
docs/book` — and is validated before the next begins.

1. **The projection.** The `thread` column, `tg_thread`, the ladder rung
   before the column-order rung and its slice, retention per thread.
   `message_topic` learns `messageTopicThread`; `reply_info` yields its two
   ids; `supergroupFullInfo.linked_chat_id` and
   `supergroup.join_to_send_messages` are projected. Decoder and migration
   tests, including a store at the ladder's current height climbing the new
   rung with its messages, drafts and topic selections intact. No UI.
2. **The scope.** `(chat, topic)` becomes `(chat, Scope)` through the model,
   the transcript loader, the runtime's loading flags, the pages, the attach
   and place panel ids and the requests; `in_scope` writes the thread's
   `topic_id` and the thread's read source; `get_history_in` gains
   `getMessageThreadHistory`; the worker resolves a post with
   `getMessageThread`, caches the answer in `tg_thread` and walks the
   thread's history on the existing pacing. Protocol tests against the
   installed schema's names; no behaviour change for chats or topics.
3. **The panel.** The foot as a way in, `comments` on both bars, the panel's
   identity and title, the root and caption rows, the loading and failed
   states, the thread's draft, reading and *new*, the composer and `join
   group`, `post` and `about`, the channel card's discussion line. Fixtures
   and scenes.
4. **The words and the proof.** `telegram.md` gains *The comments under a
   post*; `vocabulary.md` gains *discussion group*, *thread* and *comment*;
   the agents' notes (`tools.rs:47`, where the panel's arguments and the
   projection's rules are written) explain `tg_thread`, the `thread` column
   and how to find a post's comments with `sql.query`. The e2e suite, and the
   book's *Current limits* trimmed of what is no longer true.

## To decide in review

- **The panel's name.** Proposed: the post — `telegram-chat <channel>
  comments <post>` — because that is what a person points at, what survives
  a restart with nothing else open, and what a comment link names. The
  alternative is the resolved pair *(group, root)*, which is what the
  transcript actually reads but is unknown at the moment of the press.
- **The word on the foot.** `8 comments · new`, or the count
  (`· 3 new`) only for threads we have asked about, or the clients' bare
  dot, which this app has no vocabulary for.
- **`m` for comments**, and `p` for the post behind them.
- **Group reply threads.** The same panel serves a reply thread in an
  ordinary group — the Mac's `.replies(origin:)` — and the wire is
  identical. Proposed: leave the way in off the bar for now, and keep the
  machinery kind-agnostic so adding it later is one verb.
- **Whether a thread can be pinned into the chat list**, the way a forum
  topic can be selected into it. Proposed: no. Neither client does, a post's
  comments are a place one visits rather than a correspondence one keeps,
  and the list's row key would have to grow a third field.
- **Whether the discussion group should appear in the chat list** once its
  comments have been read. Proposed: no — `in_main = 0`, as for a channel
  never joined.

## Not done, on purpose

- **Writing a comment from an agent.** `telegram.draft` and `telegram.send`
  keep taking a chat and a topic; a thread destination is a bigger change to
  their shape than this change earns. Reading comments needs nothing new:
  they are rows in `tg_message` and `sql.query` already finds them.
- **Slow mode.** Discussion groups often have one. A refusal is reported in
  the status strip in the wire's own words; there is no countdown on the
  composer.
- **A thread's pinned message, its own participants, and its mute.** All
  three exist on the wire and none is part of reading comments.
- **Comment notifications as their own view.** A reply to my comment already
  arrives as an unread mention in the group and shows up in *replies &
  mentions*.
- **Opening a search hit inside its thread.** A hit in a comment opens the
  group at that message, as it does today.
- **Discussion groups that are forums.** Their messages carry both a topic
  and a thread; the columns hold both, and which one a panel stands in is
  the panel's own. Nothing is drawn differently for them, and no attempt is
  made to show a thread inside its topic.

## Progress — 2026-09-16

All four phases have landed on `prepor/telegram-channel-comments`, and the
book says what they do (`telegram.md`, *The comments under a post*;
`vocabulary.md`, *discussion group* and *thread*). What is here is what the
plan said, with the deviations below.

- **The projection.** `v23_comment_threads` adds `tg_message.thread` and its
  index, `tg_peer.linked` and `tg_peer.join_to_send`, and `tg_thread`. It
  sits before the column-order rung, whose canonical slice is `[..21]` now —
  `[..20]` when this was written, and one more after the rebase below.
  `updates::message_thread` reads `messageTopicThread`; `comment_count` makes
  a post's `comments` the *presence* of `reply_info` rather than a count
  above zero, so `Some(0)` is a discussion with nobody in it and `None` is a
  line with no comments to open; `updates::thread` decodes
  `messageThreadInfo` and `origin_post` the forward origin.
- **The scope.** `(chat, topic)` is `(chat, Scope)` through the model, the
  transcript loader, the runtime's viewports and loading flags, the pages,
  the attach and place panel ids, the mention search and the requests.
  `requests::in_scope` writes `messageTopicThread` and the thread's read
  source; `get_history_in` picks `getMessageThreadHistory`.
- **The panel.** `telegram-chat <channel> comments <post>` is the chat panel
  standing in a thread: the root, the caption row, the comments, the
  thread's own draft and reading, `comments` (`m`) on both bars and on the
  post's foot, `post` (`p`), `join group` (`j`), `retry` (`y`), and
  `discussion` (`d`) on a channel's card. A thread has an unread line of its
  own, drawn above the comments past its cursor, and opening one reads them
  — in the thread, never in the group.
- **The words and the proof.** 23 new tests over the panel, the wire and the
  rung, a suite at `e2e/telegram/comments.txt`, two scenes, the agents'
  notes, and the fixture below.

### Deviations from the sketch

- **The panel is named by the line a person pressed**, not always by the
  channel's post: `threads::way_in` turns a root in the discussion group
  into the channel's post where the store knows the pair, so both ways in
  land on one panel — and where it does not (a thread this device only knows
  from the group's side), the group's root names it and everything works the
  same. One thread can therefore own two `tg_thread` rows, one keyed each
  way; both are written together and the draft and read cursors address them
  by `(group_id, root)`, so they never disagree.
- **A thread reached from the group's side never waits.** A post's copy
  there carries `messageOriginChannel`, so projecting it writes the
  resolution — group and root — without asking `getMessageThread` at all.
  The ask is only for a post looked at from inside the channel.
- **`tg_thread` has no foreign key into `tg_peer`.** The copy in a group
  names a channel this device may never have been told about, and a write
  that failed on that would lose the whole batch.
- **No `getSupergroupFullInfo` of our own.** `linked` rides the
  `updateSupergroupFullInfo` the projection already reads for member counts,
  which TDLib sends for an opened chat; a request of our own would be a
  second way to learn one thing.
- **The letters** are `m` for *comments*, `p` for *post*, `j` for *join
  group*, `y` for *retry* and `d` for *discussion*. Not `t` or `i`: the
  workspace keeps `w z u t i l`.
- **The fixture** is *Rust Weekly chat*, a group the account is not in
  (`in_main = 0`) whose comments are read all the same. Three of the
  channel's four posts have a thread — eight comments with two past the
  reading, three read to the end, and one nobody has written in — and the
  newest post predates the discussion group, so it has no foot at all.

### What could not be verified here

Nothing was run against a real Telegram account: there is no session on this
machine. What is proved is the JSON of every request against the names the
installed TDLib (`HEAD-d1085f9`) knows, the projection and its rung, the
panel's behaviour over the fixture, and the suite's walk. A live thread —
the wire's own `messageThreadInfo`, a comment actually sent, a group that
demands joining — is Andrey's to see.

Verified: `cargo clippy --workspace --all-targets --locked
--no-default-features -- -D warnings` clean; `cargo clippy -p superapp
--all-targets --locked -- -D warnings` clean (with `tdlib`); `cargo test
--workspace --locked --no-default-features` — 1507 + 420 + 2 passed, 0
failed; `MAKEPAD=headless cargo build -p superapp --no-default-features`
then `./e2e/run-all.sh` — 119 suites, no failures; `mdbook build docs/book`
clean.

Three runs of the suite on a loaded machine each failed one test somewhere
else in the tree — `mail::selection::native_filing_carries_…`,
`mail::downloads::attachments_download_only_on_demand_…`,
`workshop::snapshots::edited_reviewed_file_reopens_whole_file_…` — a
different one each time, never the same twice, and never one of Telegram's
527. The first of them was run four times on `origin/main` with none of this
work in the tree and failed once there too, so they are the tree's own
flakes under load rather than this change's.

## Review fixes — 2026-09-16

Seven findings from a reading of the change, each with a test. Two of them
were wrong answers rather than missing ones, and those are the first two.

- **A bar wore one letter twice.** `join group` took `j`, which is *react*'s
  over the cursor's line — and both stand on a comments panel at once, so a
  debug build asserted its way out of the draw (`shell::bar::check`). It is
  `g` now. The bar test walked no fixture that had a group wanting members
  *and* a line under the cursor; one does now.
- **A forwarded post claimed somebody else's comments.** Any line with a
  reply info was taken for a thread's root, and a channel origin on it for
  the post those comments belong to — so a person forwarding channel X's
  post into the discussion group of channel Y wrote *X's post's comments are
  in Y's group*, and the way in from X led to a conversation about something
  else. Two rules now: a line is the root of its own thread only in a chat
  the store knows to be a **group** (never a channel, never a stub), and a
  post is joined to that thread only by the channel's *own* automatic copy —
  the one the channel itself is the sender of. Everything else waits for
  `getMessageThread`, which is the only source that actually knows.
- **A reply opened from search lost its composer.** `message_scope` learned
  threads, and the chat factory used it to place a panel opened *at* a line:
  a hit on a reply in an ordinary group opened a transcript filtered to that
  reply's thread, with the group's draft nowhere. Opening at a line stands
  in that line's forum topic, as it always did, and never in its thread.
- **A discussion group could not be marked read.** The newest line a chat
  may read through was `thread = 0`, so the comments — ordinary messages of
  the group — were skipped, and a group whose newest lines are comments
  stayed unread for good. A chat's cursor counts every line of it again; a
  thread's still counts only its own.
- **An archived topic disabled its group's composer.** `wants_joining` read
  `archived` off the card, which a topic's card overwrites with the topic's
  own archived preference — so a topic put away on this device read as a
  chat not joined. `PeerCard::joined` is the chat's own standing, computed
  in the query and never overwritten.
- **A thread's draft from another device was thrown away.**
  `messageThreadInfo` carries `draft_message` and nothing read it. It is
  projected now, and only a source that carries one may write it — a line
  arriving in the thread cannot blank what was typed elsewhere.
- **The window dropped the post the comments answer.** A thread's root is
  its oldest line, so a thread past ten thousand comments trimmed the post
  itself away and left them hanging under nothing. The root is never
  trimmed.

Verified: `cargo clippy --workspace --all-targets --locked
--no-default-features -- -D warnings` clean; `cargo clippy -p superapp
--all-targets --locked -- -D warnings` clean (with `tdlib`); `cargo test
--workspace --locked --no-default-features` — 1514 + 420 + 2 passed, 0
failed; `MAKEPAD=headless cargo build -p superapp --no-default-features`
then `./e2e/run-all.sh` — 119 suites, no failures.

## Review fixes — 2026-09-16, second round

Four more, all about what a thread is told and by whom.

- **A repost redirected the original's comments.** The copy in the group was
  read for its forward *origin*, which is the first author of a line and not
  the message it was forwarded from: a channel reposting one of its own old
  posts made a new thread that claimed to be the old post's, and the way in
  from the old post led to the new conversation. `forward_info.source` — the
  last message it came from, which is what TDLib fills in for the automatic
  copy — is read first, and the origin only where the wire gave no source.
- **A thread already found was never asked about.** A post's copy in the
  group says where its comments are by itself, so a panel opened from that
  side never sent `getMessageThread` — and that answer is the only thing
  that carries the draft another device left in the thread. The ask goes out
  once per post now, whether or not the destination is known.
- **And that answer could take back what was typed here.** It is a snapshot
  from before it was asked for, so a draft in it may not overwrite one this
  device holds; it lands where there is none. What is typed here goes to
  Telegram when the panel is left, as it always did.
- **The window would not close.** Keeping the root meant a thread's oldest
  held line was always older than any page the wire answered with, and its
  count was always one past full — so a walk down a thread of more than ten
  thousand comments never stopped asking for pages it then threw away. The
  root is outside the window it is the head of: neither counted nor trimmed.

Verified: `cargo clippy --workspace --all-targets --locked
--no-default-features -- -D warnings` clean; `cargo clippy -p superapp
--all-targets --locked -- -D warnings` clean (with `tdlib`); `cargo test -p
superapp --locked --no-default-features` — 1517 passed, 0 failed; `cargo
test -p superapp-kernel --locked` — 420 passed, 0 failed (the kernel's
process-lock tests take real file locks and fail when another suite on this
machine holds them, so the two are run apart); `MAKEPAD=headless cargo build
-p superapp --no-default-features` then `./e2e/run-all.sh` — 119 suites, no
failures.

## Review fixes — 2026-09-16, third round

Three, and the last two are one thing said twice: which field of a forward
means *this is that post's own copy*.

- **A draft was resurrected, and a fresh one refused.** The second round's
  rule — the wire's draft lands only where this device holds none — was
  wrong in both directions: a stale answer put back a draft just sent or
  cleared here, and a newer one made elsewhere could not replace a cached
  one. Both are an ordering, so both are answered by a date. `tg_thread`
  keeps `draft_date`; what is typed here is dated by the panel's clock, and
  what the wire answers with by `draftMessage.date` — or, where it says
  there is no draft at all and so carries no date, by the moment of the ask,
  which rides in the request's `@extra`. The newer of the two stands.
- **A hand's forward could still redirect a post's comments**, because the
  fallback to the forward's *origin* was still there for a copy the wire
  gave no source for. There is no fallback now.
- **A post of somebody else's words lost its comments**, because the origin
  was also being *required* to be a channel — and a channel can post what a
  person wrote, in which case the origin is that person while the copy in
  the group is still the post's own.

Both forward findings are the same correction, and TDLib's own schema is
what settles it: `messageForwardInfo.source` is filled in "for messages
forwarded to the chat with the current user, to the Replies bot chat, or to
the channel's discussion group… may be null for other forwards". So the
source, and nothing beside it, is what says a line is a post's copy —
neither necessary nor sufficient is the origin. With the two guards already
in place — the chat is a group, and the line's sender is the chat the source
names — Saved Messages and the Replies chat are out (neither is a group),
and a forward made by hand carries no source at all.

Verified: `cargo clippy --workspace --all-targets --locked
--no-default-features -- -D warnings` clean; `cargo clippy -p superapp
--all-targets --locked -- -D warnings` clean (with `tdlib`); `cargo test -p
superapp --locked --no-default-features` — 1518 passed, 0 failed; `cargo
test -p superapp-kernel --locked` — 420 passed, 0 failed; `MAKEPAD=headless
cargo build -p superapp --no-default-features` then `./e2e/run-all.sh` — 119
suites, no failures.

## Rebased onto main — 2026-09-16

`main` had moved, and *Telegram: name a forward's origin, and open it*
(#163) landed in the same places. Three things were merged rather than
taken:

- **The ladder.** That change added `v22_forward_origin` in the very slot
  this one wanted, so the comments rung is `v23_comment_threads`, after it
  and still before the column-order rung, whose canonical slice is `[..21]`.
- **The transcript's query and its row.** Both sides append columns; the
  forward's three (`fwd_peer`, `fwd_msg`, `fwd_sign`) keep the places they
  were given, and the thread's two follow them.
- **The trim.** It had gained a list of lines a panel is waiting for —
  `trim_topic(chat, topic, keep)` — and becomes
  `trim_scope(chat, scope, keep)`, sparing both those lines and a thread's
  root.
- **A press inside a line.** `came_from` and `comments` are both openings
  from within a row, and sit in one match; a `came_from` may answer nothing,
  so `comments` answers `Some`.

The fixture's root copies now carry `fwd_peer`/`fwd_msg` rather than a bare
`fwd_from` name, which is the shape a real copy arrives in after #163.

Verified after the rebase: `cargo clippy --workspace --all-targets --locked
--no-default-features -- -D warnings` clean; `cargo clippy -p superapp
--all-targets --locked -- -D warnings` clean (with `tdlib`); `cargo test -p
superapp --locked --no-default-features` — 1533 passed, 0 failed; `cargo
test -p superapp-kernel --locked` — 423 passed, 0 failed; `MAKEPAD=headless
cargo build -p superapp --no-default-features` then `./e2e/run-all.sh` — 121
suites, no failures.

## Review fixes — 2026-09-16, fourth round

Two more about the thread's draft, and between them they take the clock out
of it altogether.

- **The date on a local draft was the last draw's**, not the moment of the
  typing: a panel that has sat undrawn takes its stamp from minutes ago, so
  an ask sent in between looked newer and its answer could erase what had
  just been written.
- **And a lookup could answer with a null for a draft Telegram had never
  been told about** — the draft goes to the wire when the panel is left —
  which the ask's own clock then made authoritative.

Both are the same mistake: a clock cannot say who is right, because the two
sides are not racing. What decides it is whether Telegram has been *told*.
`tg_thread.draft_sent` says so — `NULL` where nothing has ever been written
here, 0 where something has and the wire has not heard it, 1 where it has —
and the wire's answer may write the draft except while that is 0. An answer
carrying no draft writes nothing at all: it is an ignorance, not a clear.
`draft_date`, `draftMessage.date` and the moment of the ask riding in the
request's `@extra` are all gone with the race they were trying to settle.

Verified: `cargo clippy --workspace --all-targets --locked
--no-default-features -- -D warnings` clean; `cargo clippy -p superapp
--all-targets --locked -- -D warnings` clean (with `tdlib`); `cargo test -p
superapp --locked --no-default-features` — 1534 passed, 0 failed; `cargo
test -p superapp-kernel --locked` — 423 passed, 0 failed; `MAKEPAD=headless
cargo build -p superapp --no-default-features` then `./e2e/run-all.sh` — 121
suites, no failures.
