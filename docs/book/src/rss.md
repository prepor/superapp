# RSS

The launcher offers **rss**, the reading queue, and **feeds**, the
subscriptions. Feeds accepts an RSS or Atom URL through **add feed**;
**remove** unsubscribes the selected or marked feeds. Adding and removing
subscriptions are undoable. Removing keeps the cache and read state, so
undo or subscribing to the same URL restores them.

**feeds → import OPML** accepts a local OPML file path (`~/Downloads/feeds.opml`
works). Import reads feeds from nested folders, uses their exported titles,
and reports how many were imported, already subscribed, or skipped. It skips
duplicate and invalid URLs, preserves existing subscriptions and read state,
and restores removed subscriptions from their cache. The whole batch is one
undoable action; importing the same file again adds nothing. Folders are
flattened into the feed list. The importer accepts UTF-8 files up to two MiB
and tolerates unescaped title text and query strings in older exports.

Articles use the shared [rich table](./richtable.md), ordered from oldest
to newest, with `@unseen` in the filter by default. Clear that filter to see
the archive. The table supports free text over titles, authors and feed
names, plus `@feed`, `@feed_id`, `@author`, `@date`, `@seen` and `@unseen`.
Feed and author values autocomplete. The subscriptions table offers
`@failed` to find feeds whose last refresh failed.

Opening or previewing an article marks it seen on the same undoable action
as the panel opening. A selected article stays under the cursor until the
cursor moves, even after it no longer matches `@unseen`. Use `@seen` and
`@unseen` in the filter to choose which articles appear. The article filter
survives session restore.

Each article visit has its own undo step, even when moving quickly through
the queue. `cmd+z` returns to the previous article and restores the read state
changed by the visit; the list cursor follows the restored reader.
Panel focus returns to where it was before the visit as well. Switching back
or forward in the same reader keeps the panel geometry and camera position
while that focus remains at least partly on-screen. A fully off-screen focus
is brought into view.
`cmd+shift+z` moves forward again. Entering or clicking the current preview
adds no extra undo step.

The reader uses the same HTML cleanup, proportional typography, heading
scale, selectable text, code, links and image loader as
[mail](./mail.md#html-and-pictures). These components live in
`app/src/reader/`; mail supplies its own adapter for inline MIME files.
Relative article links and images resolve against their source URL.
Article links retain fragments, including when a permalink supplies a
missing entry ID; subscription URLs ignore fragments for deduplication.
Full feed content takes precedence over the publisher's summary; feeds
that only publish a summary show that summary. **open original** opens the
publisher's page in the browser.

A `<video>` in an article is a clip in the reading — a box in the column
with the [player](./media.md)'s strip beneath it — and an `<audio>` is the
strip alone. The clip streams from the publisher's address; a poster stands
in the box until it plays, and a clip published as a silent moving picture
(`autoplay muted`) runs on sight. What this platform cannot play is a link
to the source instead.
The article bar also offers **show original** (`cmd+o`), available from the
article list while that article is previewed.

Each article retains the publisher's content before HTML cleanup, its
content type, and its effective base URL. A shared sanitizer version change
rebuilds every cached reading on the next open, including articles that
have left the publisher's feed and removed subscriptions. Older caches
without source are cleaned again from their saved HTML, and their feeds
request a full refresh to recover source where it is still available.

Feeds refresh every fifteen minutes, one request at a time. **refresh**
queues all subscriptions for refresh. HTTP follows up to five redirects,
uses conditional requests when validators are available, and bounds each
request to thirty seconds and eight MiB of feed content. RSS, Atom and JSON
Feed share one parser. Requests run outside the UI thread and database
transactions, and appear as `rss.fetch` in the effect log. The feeds list
shows each refresh's status or error.

Entries are unique by `(feed, guid)`. A repeated entry updates its reading
while retaining seen state and its place in the queue. Missing dates use
the first retrieval time. Entries remain cached when a publisher drops
them from its rolling feed. A response arriving after a feed was removed
cannot resubscribe it or add articles. A subscription
[replicates](./device-sync.md#what-replicates), and so does an article's
read mark, as `rss_seen`; the cached readings are each device's own,
fetched from the feed. Real stores start with no subscriptions; scripted
runs use local fixtures and make no feed requests to the web.
