use super::{model, opml, panels, parse, seed, sync, RSS};
use kernel::app::{App, Worker};
use kernel::nav::Nav;
use kernel::panel::PanelId;
use kernel::richtable::{Datasource, ListState};
use kernel::session::{Action, Session};

static APPS: &[&dyn App] = &[&RSS];
fn session() -> Session {
    Session::fake(APPS)
}
fn open(s: &mut Session, id: PanelId) -> kernel::layout::SlotId {
    s.act(Action::new("open", "open RSS").moving(move |wm| {
        wm.open(id, None, false);
    }));
    s.settle();
    s.focus().unwrap()
}
fn id(s: &Session, guid: &str) -> i64 {
    s.store()
        .conn()
        .query_row("SELECT id FROM rss_article WHERE guid=?", [guid], |r| {
            r.get(0)
        })
        .unwrap()
}

#[test]
fn subscriptions_and_seen_state_are_undoable() {
    let mut s = session();
    let added = model::add(&mut s, seed::EXTRA).unwrap();
    assert_eq!(model::FEEDS.count(s.store(), None), Some(3));
    s.undo();
    assert_eq!(model::FEEDS.count(s.store(), None), Some(2));
    s.redo();
    assert_eq!(model::FEEDS.count(s.store(), None), Some(3));
    assert!(model::add(&mut s, seed::EXTRA)
        .unwrap_err()
        .contains("already subscribed"));
    assert!(model::change(
        &mut s,
        model::Flag::Subscribed,
        &[added],
        false
    ));
    s.undo();
    assert!(model::FEEDS.by_key(s.store(), &added).is_some());
    let article = id(&s, "notes-1");
    assert!(model::change(&mut s, model::Flag::Seen, &[article], true));
    assert!(model::article(s.store(), article).unwrap().seen);
    s.undo();
    assert!(!model::article(s.store(), article).unwrap().seen);
    s.redo();
    assert!(model::article(s.store(), article).unwrap().seen);
}

#[test]
fn oldest_first_and_unseen_is_an_editable_default() {
    let mut s = session();
    let slot = open(&mut s, panels::Articles::id());
    let panel = s.panel(slot).unwrap();
    let mut borrow = panel.borrow_mut();
    let p = borrow.as_any().downcast_mut::<panels::Articles>().unwrap();
    assert_eq!(p.list.table().filter(), "@unseen");
    assert_eq!(p.list.len(s.store()), 3);
    let titles = (0..3)
        .map(|i| p.list.row(s.store(), i).unwrap().title)
        .collect::<Vec<_>>();
    assert_eq!(
        titles,
        [
            "Make room for reading",
            "Small tools, lasting habits",
            "A quieter morning"
        ]
    );
    p.list.set_filter("");
    assert_eq!(p.list.len(s.store()), 4);
    p.list.set_filter("@feed:\"Field notes\" @seen");
    assert_eq!(p.list.len(s.store()), 1);
    p.list.set_filter("@author:Sam @date:29.08.2026");
    assert_eq!(p.list.len(s.store()), 1);
    p.list.set_filter("@author:Alex");
    assert_eq!(p.list.len(s.store()), 2);
    assert!(model::ARTICLES
        .suggest(s.store(), "feed", "field")
        .iter()
        .any(|s| s.value.contains("Field notes")));
}

#[test]
fn preview_marks_seen_and_the_selected_row_stays_until_cursor_moves() {
    let mut s = session();
    let slot = open(&mut s, panels::Articles::id());
    let panel = s.panel(slot).unwrap();
    let article = {
        let mut borrow = panel.borrow_mut();
        let p = borrow.as_any().downcast_mut::<panels::Articles>().unwrap();
        p.list.set_cursor(s.store(), 0).unwrap().id
    };
    s.nav(Nav::Preview {
        from: slot,
        id: panels::Article::id(article),
    });
    s.settle();
    assert!(model::article(s.store(), article).unwrap().seen);
    {
        let mut borrow = panel.borrow_mut();
        let p = borrow.as_any().downcast_mut::<panels::Articles>().unwrap();
        p.list.sync(s.store());
        assert_eq!(p.list.row(s.store(), 0).unwrap().id, article);
        assert_eq!(p.list.len(s.store()), 3);
        p.list.set_cursor(s.store(), 1);
        assert_eq!(p.list.len(s.store()), 2);
    }
    s.undo();
    assert!(!model::article(s.store(), article).unwrap().seen);
}

#[test]
fn refresh_updates_readings_without_duplicates_or_losing_read_state_and_order() {
    let mut s = session();
    let article = id(&s, "notes-1");
    model::change(&mut s, model::Flag::Seen, &[article], true);
    let before = model::article(s.store(), article).unwrap();
    let mut feed =
        parse::parse(seed::fixture(seed::NOTES).unwrap().as_bytes(), seed::NOTES).unwrap();
    feed.articles.retain(|a| a.guid == "notes-1");
    feed.articles[0].title = "An edited title".into();
    feed.articles[0].published = Some(s.now() + 3600.0);
    let feed_id = before.feed;
    s.store()
        .write(move |c| model::ingest(c, feed_id, &feed, 0.0))
        .unwrap();
    assert_eq!(
        model::ARTICLES.count(s.store(), None),
        Some(4),
        "older entries survive a rolling window"
    );
    let after = model::article(s.store(), article).unwrap();
    assert!(after.seen);
    assert_eq!(after.title, "An edited title");
    assert_eq!(after.published, before.published);
    model::change(&mut s, model::Flag::Subscribed, &[feed_id], false);
    assert!(model::article(s.store(), article).is_none());
    assert_eq!(model::add(&mut s, seed::NOTES).unwrap(), feed_id);
    assert!(model::article(s.store(), article).unwrap().seen);
}

#[test]
fn worker_fetches_new_feeds_and_reports_errors_without_losing_cached_articles() {
    let mut s = session();
    let feed = model::add(&mut s, seed::EXTRA).unwrap();
    let mut worker = sync::FeedWorker(feed);
    worker.pass(s.world());
    assert_eq!(
        model::FEEDS.by_key(s.store(), &feed).unwrap().title,
        "A new subscription"
    );
    assert_eq!(model::ARTICLES.count(s.store(), None), Some(5));
    worker.pass(s.world());
    assert_eq!(model::ARTICLES.count(s.store(), None), Some(5));
    let bad = model::add(&mut s, "https://example.com/missing.xml").unwrap();
    sync::FeedWorker(bad).pass(s.world());
    assert_eq!(
        model::FEEDS.by_key(s.store(), &bad).unwrap().error,
        "demo feed not found"
    );
    assert_eq!(model::ARTICLES.count(s.store(), None), Some(5));
}

#[test]
fn a_removed_feed_ignores_an_inflight_response() {
    let mut s = session();
    let feed_id = model::add(&mut s, "https://example.com/pending.xml").unwrap();
    model::change(&mut s, model::Flag::Subscribed, &[feed_id], false);
    let parsed = parse::parse(seed::fixture(seed::EXTRA).unwrap().as_bytes(), seed::EXTRA).unwrap();
    s.store()
        .write(move |c| model::ingest(c, feed_id, &parsed, 0.0))
        .unwrap();
    assert_eq!(
        s.store()
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM rss_article WHERE feed=?",
                [feed_id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}

#[test]
fn parser_prefers_full_content_and_resolves_safe_links_and_images() {
    let xml=br#"<rss version="2.0" xmlns:content="http://purl.org/rss/1.0/modules/content/"><channel><title>News &amp; ideas</title><item><guid>x</guid><title>Story</title><link>https://example.com/posts/one</link><description>Summary</description><content:encoded><![CDATA[<h1>Full article</h1><p><a href="../two">two</a><a href="javascript:alert(1)">bad</a></p><img src="/image.png" width="200" height="100"><script>secret()</script><pre><code>let x = 1;</code></pre>]]></content:encoded></item></channel></rss>"#;
    let feed = parse::parse(xml, "https://example.com/feed").unwrap();
    assert_eq!(feed.title, "News & ideas");
    let a = &feed.articles[0];
    assert!(a.html.contains("Full article"));
    assert!(!a.html.contains("Summary"));
    assert!(a.html.contains("https://example.com/two"));
    assert!(a.html.contains("https://example.com/image.png"));
    assert!(!a.html.contains("javascript:") && !a.html.contains("secret()"));
    assert!(
        a.html.contains("<pre>") && a.html.contains("let x = 1;"),
        "{}",
        a.html
    );
}

#[test]
fn atom_plain_text_is_literal_and_missing_identifiers_are_stable() {
    let xml=br#"<feed xmlns="http://www.w3.org/2005/Atom"><title>Example</title><entry><title>One</title><content type="text">&lt;b&gt;literal&lt;/b&gt; &amp; safe</content></entry></feed>"#;
    let a = parse::parse(xml, seed::LAB).unwrap();
    let b = parse::parse(xml, seed::LAB).unwrap();
    assert_eq!(a.articles[0].guid, b.articles[0].guid);
    assert!(a.articles[0].html.contains("&lt;b&gt;literal&lt;/b&gt;"));
    assert!(!a.articles[0].html.contains("<b>"));
}

#[test]
fn untitled_entries_without_ids_do_not_collapse_into_one_article() {
    let xml=br#"<rss version="2.0"><channel><title>Example</title><item><description>First story</description></item><item><description>Second story</description></item></channel></rss>"#;
    let a = parse::parse(xml, seed::NOTES).unwrap();
    let b = parse::parse(xml, seed::NOTES).unwrap();
    assert_ne!(a.articles[0].guid, a.articles[1].guid);
    assert_eq!(a.articles[0].guid, b.articles[0].guid);
}

#[test]
fn batch_actions_include_hidden_marks_and_undo_restores_them() {
    let mut s = session();
    let slot = open(&mut s, panels::Articles::id());
    let keys = vec![id(&s, "notes-1"), id(&s, "lab-1")];
    let panel = s.panel(slot).unwrap();
    {
        let mut borrow = panel.borrow_mut();
        let p = borrow.as_any().downcast_mut::<panels::Articles>().unwrap();
        p.list.marks_mut().extend(keys.iter().copied());
        p.list.set_filter("@feed:Field");
        p.list.sync(s.store());
    }
    panel.borrow_mut().run("rss.seen", &mut s);
    for key in &keys {
        assert!(model::article(s.store(), *key).unwrap().seen);
    }
    s.undo();
    for key in &keys {
        assert!(!model::article(s.store(), *key).unwrap().seen);
    }
    let mut borrow = panel.borrow_mut();
    let p = borrow.as_any().downcast_mut::<panels::Articles>().unwrap();
    for key in keys {
        assert!(p.list.marks().has(&key));
    }
}

#[test]
fn bad_urls_and_documents_are_rejected_and_empty_feeds_are_valid() {
    for bad in [
        "example.com/feed",
        "ftp://example.com/feed",
        "file:///etc/passwd",
        "https://user:pass@example.com/",
    ] {
        assert!(parse::web_url(bad).is_err(), "{bad}");
    }
    assert_eq!(
        parse::web_url(" HTTPS://EXAMPLE.COM/feed#top ").unwrap(),
        "https://example.com/feed"
    );
    assert!(parse::parse(b"<html><body>not a feed</body></html>", seed::NOTES).is_err());
    let empty = parse::parse(
        b"<rss version=\"2.0\"><channel><title>Empty</title></channel></rss>",
        seed::NOTES,
    )
    .unwrap();
    assert!(empty.articles.is_empty());
}

#[test]
fn feed_filter_uses_identity_even_when_two_titles_match() {
    let s = session();
    s.store()
        .write(|c| c.execute("UPDATE rss_feed SET title='Same title'", []))
        .unwrap();
    let feed = model::FEEDS.page(s.store(), None, 0, 50)[0].id;
    let mut list = ListState::new(&model::ARTICLES, 50);
    list.set_filter(&format!("@feed_id:{feed}"));
    assert!(list.len(s.store()) > 0);
    for i in 0..list.len(s.store()) {
        assert_eq!(list.row(s.store(), i).unwrap().feed, feed);
    }
}

#[test]
fn restoring_preserves_an_empty_filter_and_does_not_mark_articles_seen() {
    use kernel::app::{Apps, Mode, Workers};
    let mut s = session();
    let list_slot = open(&mut s, panels::Articles::id());
    let article = id(&s, "notes-1");
    open(&mut s, panels::Article::id(article));
    model::change(&mut s, model::Flag::Seen, &[article], false);
    s.panel(list_slot)
        .unwrap()
        .borrow_mut()
        .as_any()
        .downcast_mut::<panels::Articles>()
        .unwrap()
        .list
        .set_filter("");
    s.save();
    let mut restored = Session::new(
        Apps::new(APPS),
        s.world().clone(),
        Workers::none(s.store().clone()),
        Mode::Fake,
    );
    assert!(restored.restore());
    assert!(!model::article(restored.store(), article).unwrap().seen);
    let panel = restored.panel(list_slot).unwrap();
    let mut borrow = panel.borrow_mut();
    let p = borrow.as_any().downcast_mut::<panels::Articles>().unwrap();
    assert_eq!(p.list.table().filter(), "");
    assert_eq!(p.list.len(restored.store()), 4);
}

#[test]
#[ignore = "reads a live RSS feed; run explicitly"]
fn real_feed_can_be_fetched_and_parsed() {
    use sync::Fetch;
    let url = "https://blog.rust-lang.org/feed.xml";
    let reply = sync::Http::default()
        .get(&sync::Request {
            id: 0,
            url: url.into(),
            etag: String::new(),
            modified: String::new(),
        })
        .unwrap();
    let sync::Response::Updated { url, bytes, .. } = reply else {
        panic!("first request has content")
    };
    let parsed = parse::parse(&bytes, &url).unwrap();
    assert!(!parsed.articles.is_empty());
    assert!(parsed.articles.iter().any(|a| !a.html.is_empty()));
}

const OPML: &[u8] = include_bytes!("../../../../e2e/rss/fixtures/subscriptions.opml");

#[test]
fn opml_reads_nested_feeds_and_preserves_exported_titles_and_query_strings() {
    let doc = opml::parse(OPML).unwrap();
    assert_eq!(doc.feeds.len(), 3);
    assert_eq!(doc.skipped, 2); // one duplicate and one unusable URL
    assert_eq!(doc.feeds[0].url, seed::NOTES);
    assert_eq!(doc.feeds[1].title, "A new subscription");
    assert_eq!(doc.feeds[2].title, "Reader \"notes\" & <ideas>");
    assert_eq!(
        doc.feeds[2].url,
        "https://example.com/other.xml?one=1&two=2"
    );
    let clean = br#"<opml version="2.0"><head/><body><outline text="Folders">
        <outline title="" text="News &amp; ideas" xmlUrl="https://example.org/rss?a=1&amp;b=2"/>
        <outline title="" xmlUrl="https://untitled.example/rss"/>
        <outline title="A backslash\" xmlUrl="https://example.net/rss"/>
        <outline isComment="true"><outline xmlUrl="https://ignored.example/rss"/></outline>
        </outline></body></opml>"#;
    let doc = opml::parse(clean).unwrap();
    assert_eq!(doc.feeds.len(), 3);
    assert_eq!(doc.feeds[0].title, "News & ideas");
    assert_eq!(doc.feeds[0].url, "https://example.org/rss?a=1&b=2");
    assert_eq!(doc.feeds[1].title, "untitled.example");
    assert_eq!(doc.feeds[2].title, "A backslash\\");
}

#[test]
fn opml_refuses_incomplete_documents_and_external_entities() {
    for bad in [
        "<html><body><outline xmlUrl='https://example.com/rss'/></body></html>",
        "<opml><body><outline xmlUrl='https://example.com/rss'/>",
        "<opml><body><outline xmlUrl='https://example.com/rss'/></opml>",
        "<opml><body><outline xmlUrl='file:///tmp/feed'/></body></opml>",
        "<!DOCTYPE opml [<!ENTITY x SYSTEM 'file:///etc/passwd'>]><opml><body><outline xmlUrl='https://example.com/rss'/></body></opml>",
        "<opml><body/></opml><opml><body><outline xmlUrl='https://example.com/rss'/></body></opml>",
    ] {
        assert!(opml::parse(bad.as_bytes()).is_err(), "{bad}");
    }
    assert!(opml::parse(&vec![b' '; opml::MAX_OPML + 1])
        .unwrap_err()
        .contains("2 MiB"));
}

#[test]
fn opml_import_is_one_undoable_batch_and_reimport_is_a_noop() {
    let mut s = session();
    let read = id(&s, "notes-3");
    let result = model::import(&mut s, opml::parse(OPML).unwrap()).unwrap();
    assert_eq!(
        result,
        model::Imported {
            added: 2,
            existing: 1,
            skipped: 2
        }
    );
    assert_eq!(model::FEEDS.count(s.store(), None), Some(4));
    assert!(model::article(s.store(), read).unwrap().seen);
    assert_eq!(
        model::article(s.store(), read).unwrap().feed_title,
        "Field notes"
    );
    let again = model::import(&mut s, opml::parse(OPML).unwrap()).unwrap();
    assert_eq!(
        again,
        model::Imported {
            added: 0,
            existing: 3,
            skipped: 2
        }
    );
    // Re-import made no history entry: one undo reverses the original batch.
    s.undo();
    assert_eq!(model::FEEDS.count(s.store(), None), Some(2));
    assert!(model::article(s.store(), read).unwrap().seen);
    s.redo();
    assert_eq!(model::FEEDS.count(s.store(), None), Some(4));
}

#[test]
fn opml_restores_removed_subscriptions_without_resetting_the_cache() {
    let mut s = session();
    let read = id(&s, "notes-3");
    let feed = model::article(s.store(), read).unwrap().feed;
    model::change(&mut s, model::Flag::Subscribed, &[feed], false);
    let result = model::import(&mut s, opml::parse(OPML).unwrap()).unwrap();
    assert_eq!(result.added, 3);
    assert_eq!(model::article(s.store(), read).unwrap().feed, feed);
    assert!(model::article(s.store(), read).unwrap().seen);
    s.undo();
    assert_eq!(model::FEEDS.count(s.store(), None), Some(1));
    s.redo();
    assert!(model::article(s.store(), read).unwrap().seen);
}

#[test]
fn opml_import_rolls_back_the_whole_batch_on_a_write_failure() {
    let mut s = session();
    s.store().write(|c| c.execute_batch("CREATE TEMP TRIGGER reject_import BEFORE INSERT ON rss_feed WHEN NEW.url LIKE '%other.xml%' BEGIN SELECT RAISE(ABORT,'test failure'); END;")).unwrap();
    assert!(model::import(&mut s, opml::parse(OPML).unwrap()).is_err());
    assert_eq!(model::FEEDS.count(s.store(), None), Some(2));
    assert_eq!(
        s.store()
            .conn()
            .query_row(
                "SELECT count(*) FROM rss_feed WHERE url=?",
                [seed::EXTRA],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}

#[test]
fn opml_form_reads_through_the_disk_capability_and_reports_the_result() {
    use kernel::caps::{real_path, Disk};
    let mut s = session();
    s.world()
        .with_cap::<dyn Disk, _>(|d| d.write_file(&real_path("~/feeds.opml"), OPML))
        .unwrap()
        .unwrap();
    let slot = open(&mut s, panels::ImportFeeds::id());
    let panel = s.panel(slot).unwrap();
    let mut borrow = panel.borrow_mut();
    let p = borrow
        .as_any()
        .downcast_mut::<panels::ImportFeeds>()
        .unwrap();
    p.submit(&mut s);
    assert!(p.error.contains("enter the path"));
    p.path = "~/feeds.opml".into();
    p.submit(&mut s);
    assert_eq!(p.error, "");
    assert_eq!(
        p.status,
        "2 feeds imported · 1 already subscribed · 2 skipped"
    );
}

#[test]
#[ignore = "reads the OPML path supplied in RSS_OPML_TEST_FILE; run explicitly"]
fn supplied_opml_imports_and_reimports_without_duplicates() {
    let path = std::env::var("RSS_OPML_TEST_FILE").expect("RSS_OPML_TEST_FILE");
    let bytes = std::fs::read(path).unwrap();
    let doc = opml::parse(&bytes).unwrap();
    let count = doc.feeds.len();
    assert_eq!(doc.skipped, 0);
    let mut s = session();
    assert_eq!(model::import(&mut s, doc).unwrap().added, count);
    let again = model::import(&mut s, opml::parse(&bytes).unwrap()).unwrap();
    assert_eq!(again.added, 0);
    assert_eq!(again.existing, count);
    println!("imported {count} feeds; re-import added no duplicates");
}
