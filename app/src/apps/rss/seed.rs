use super::{model, parse};
use kernel::app::Mode;
use kernel::store::Store;
use kernel::time::virtual_epoch;
use rusqlite::params;

pub const NOTES: &str = "https://example.com/field-notes.xml";
pub const LAB: &str = "https://example.com/workshop.atom";
pub const EXTRA: &str = "https://example.com/new-feed.xml";

pub fn seed(store: &Store, mode: Mode) -> rusqlite::Result<()> {
    if mode != Mode::Fake {
        return Ok(());
    }
    store.write(|c| {
        if c.query_row("SELECT COUNT(*) FROM rss_feed", [], |r| r.get::<_, i64>(0))? > 0 {
            return Ok(());
        }
        for url in [NOTES, LAB] {
            c.execute("INSERT INTO rss_feed(url,title) VALUES(?1,?1)", [url])?;
            let id = c.last_insert_rowid();
            let feed =
                parse::parse(fixture(url).unwrap().as_bytes(), url).expect("valid demo feed");
            model::ingest(c, id, &feed, virtual_epoch())?;
            c.execute(
                "UPDATE rss_feed SET checked=?1,completed=requested WHERE id=?2",
                params![virtual_epoch(), id],
            )?;
        }
        c.execute("UPDATE rss_article SET seen=1 WHERE guid='notes-3'", [])?;
        Ok(())
    })
}

pub fn fixture(url: &str) -> Option<&'static str> {
    match url {
        NOTES => Some(NOTES_XML),
        LAB => Some(LAB_XML),
        EXTRA => Some(EXTRA_XML),
        _ => None,
    }
}

const NOTES_XML: &str = r#"<?xml version="1.0"?>
<rss version="2.0" xmlns:content="http://purl.org/rss/1.0/modules/content/">
<channel><title>Field notes</title><link>https://example.com/notes/</link><description>Observations on everyday work</description>
<item><guid>notes-2</guid><title>A quieter morning</title><link>https://example.com/notes/morning</link><author>Alex Morgan</author><pubDate>Sun, 30 Aug 2026 09:00:00 GMT</pubDate>
<description><![CDATA[<p>A little room to think before the day gets busy.</p>]]></description></item>
<item><guid>notes-1</guid><title>Make room for reading</title><link>https://example.com/notes/reading</link><author>Alex Morgan</author><pubDate>Fri, 28 Aug 2026 08:00:00 GMT</pubDate>
<content:encoded><![CDATA[<p>Good reading starts with a little space. A quiet column, a useful typeface, and something worth your attention.</p><h2>One article at a time</h2><p>Let the oldest story lead. New stories can wait their turn while you finish the thought in front of you.</p><blockquote>Attention is a practice, built one page at a time.</blockquote><h2>A small ritual</h2><ol><li>Find a few voices you trust.</li><li>Read slowly enough to follow their ideas.</li><li>Keep the pieces you want to return to.</li></ol><p>There is no finish line. Just a better way to spend a morning.</p><p><a href="https://example.com/notes/">More field notes</a></p>]]></content:encoded></item>
<item><guid>notes-3</guid><title>Already on the bookshelf</title><link>https://example.com/notes/bookshelf</link><pubDate>Mon, 31 Aug 2026 10:00:00 GMT</pubDate><description>An article you have already read. Clear @unseen to find it again.</description></item>
</channel></rss>"#;

const LAB_XML: &str = r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom"><id>workshop</id><title>The workshop</title><updated>2026-08-29T10:00:00Z</updated><author><name>Sam Chen</name></author>
<entry><id>lab-1</id><title>Small tools, lasting habits</title><link href="https://example.com/workshop/tools"/><published>2026-08-29T10:00:00Z</published><content type="html"><![CDATA[<p>The best tools leave room for the work itself.</p><h2>A readable loop</h2><p>Give each idea a name, then let the code say what happens.</p><pre><code>for article in feed.unseen() {
    read(article);
}</code></pre><p>A tool can be small and still deserve care: clear feedback, reliable undo, and sensible defaults.</p><hr><p>Built to be used every day.</p>]]></content></entry>
</feed>"#;

const EXTRA_XML: &str = r#"<rss version="2.0"><channel><title>A new subscription</title><link>https://example.com/</link><description>Demo feed</description><item><guid>extra-1</guid><title>Your first new article</title><pubDate>Tue, 01 Sep 2026 08:00:00 GMT</pubDate><description>The new feed is ready to read.</description></item></channel></rss>"#;
