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

The reader uses the same HTML cleanup, proportional typography, heading
scale, selectable text, code, links and image loader as
[mail](./mail.md#html-and-pictures). These components live in
`app/src/reader/`; mail supplies its own adapter for inline MIME files.
Relative article links and images resolve against their source URL.
Full feed content takes precedence over the publisher's summary; feeds
that only publish a summary show that summary. **open original** opens the
publisher's page in the browser.

Each subscription has a worker that refreshes every fifteen minutes.
**refresh** requests an immediate pass. HTTP follows up to five redirects,
uses conditional requests when validators are available, and bounds each
request to thirty seconds and eight MiB of feed content. RSS, Atom and JSON
Feed share one parser. Requests run outside the UI thread and database
transactions, and appear as `rss.fetch` in the effect log. The feeds list
shows each refresh's status or error.

Entries are unique by `(feed, guid)`. A repeated entry updates its reading
while retaining seen state and its place in the queue. Missing dates use
the first retrieval time. Entries remain cached when a publisher drops
them from its rolling feed. A response arriving after a feed was removed
cannot resubscribe it or add articles. Subscriptions and cached readings
live in the replicated store. Real stores start with no subscriptions;
scripted runs use local fixtures and make no feed requests to the web.
