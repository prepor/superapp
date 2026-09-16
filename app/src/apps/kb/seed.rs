//! A fictional wiki, so the library and the suites have rows to draw:
//! eight pages across the seven kinds, three files, links among them, one
//! that dangles and one page nothing names. A sailing trip, a lamp build
//! and a harbour town; no row of anybody's.
//!
//! Only a `Fake` or a `Deny` world gets it — a real store starts empty,
//! for the import and the agent to fill.
//!
//! The files' bytes are drawn here too, by the same code every time: a
//! picture of the harbour, a one-page PDF of the marina's fees, a parts
//! list. In the plan they live in the blob cache and the bucket; the
//! prototype has neither and hands the card what the cache would.

use kernel::app::Mode;
use kernel::store::Store;
use kernel::time::ts;
use rusqlite::{params, Connection};

use super::model::{realias, relink};

/// A hash that looks like one: sixty-four hex characters, the same for the
/// same name every time. Content addressing is phase 3's; this is a name.
#[must_use]
pub fn hash_of(name: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut out = String::with_capacity(64);
    for round in 0..4u64 {
        for b in name.bytes().chain(round.to_le_bytes()) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        out.push_str(&format!("{h:016x}"));
    }
    out
}

/// A page's uid, from its slug: the seed's rows have to be the same on
/// every store so a scene can name one.
#[must_use]
pub fn uid_of(slug: &str) -> String {
    hash_of(&format!("uid:{slug}"))[..32].to_string()
}

pub const PICTURE: &str = "attachments/porto-lume-harbour.png";
pub const PDF: &str = "sources/mooring-fees-2026.pdf";
pub const TEXT: &str = "sources/copper-lamp-parts.txt";

/// The revision the library's *restore* node opens: the lamp page as the
/// editor left it before the agent added the invoice.
#[must_use]
pub fn restorable_revision() -> String {
    hash_of("rev:lamp-build:2")[..32].to_string()
}

struct Seed {
    slug: &'static str,
    kind: &'static str,
    title: &'static str,
    summary: &'static str,
    aliases: &'static [&'static str],
    tags: &'static [&'static str],
    extra: &'static str,
    body: &'static str,
    path: &'static str,
    created: When,
    updated: When,
}

/// A civil instant the seed spells: year, month, day, hour, minute.
type When = (i64, u32, u32, u32, u32);

fn at(w: When) -> f64 {
    ts(w.0, w.1, w.2, w.3, w.4)
}

const LAMP_BODY: &str = "A desk lamp from 15 mm copper pipe on a turned oak foot, the switch in the \
cable. The bulb ran warm at first because of [[joule-heating]] in the thin cable \
that came with the holder, which is why the cable was changed for a 0.75 mm² one.

## Parts

The list as bought is in [the parts file](sources/copper-lamp-parts.txt); the \
shop's invoice came to 48.20 € and is filed under [[mooring-fees-2026|the same folder as the marina's fees]].

| part | count | note |
|---|---|---|
| copper pipe 15 mm | 1.2 m | cut to 40 + 40 + 25 |
| elbow 90° | 2 | soldered |
| E27 holder, brass | 1 | with a cord grip |
| oak blank | 1 | turned on the lathe |

## Left to do

- [x] solder the elbows
- [ ] oil the oak foot
- [ ] find a fabric cable, 2 m
";

const LAMP_BODY_BEFORE: &str = "A desk lamp from 15 mm copper pipe on a turned oak foot, the switch in the \
cable. The bulb ran warm at first because of [[joule-heating]] in the thin cable \
that came with the holder, which is why the cable was changed for a 0.75 mm² one.

## Parts

The list as bought is in [the parts file](sources/copper-lamp-parts.txt).

## Left to do

- [x] solder the elbows
- [ ] oil the oak foot
";

const SAILING_BODY: &str = "Ten days on a chartered Bavaria 34 out of [[porto-lume]], the second week of \
September, two of us. The plan is the islands to the south and back by the \
mainland shore.

## What is known

- the marina's prices are in [[mooring-fees-2026]] — a 34-footer is 62 € a night in September
- the harbour office answers channel 17 from 07:00
- the [[tide-tables]] for the strait are still to be found; the pilot book says the current runs to two knots at springs

## Route

1. Porto Lume → Cape Vesna, 14 M
2. Cape Vesna → the anchorage under Sveti Ilar, 22 M
3. back along the shore over three days
";

const PORTO_BODY: &str = "A harbour town of some four thousand people on the north shore of the gulf, \
with a marina of two hundred berths, a fuel pontoon and a chandlery that opens \
at eight. Home port for the [[sailing-trip-2026]].

![The harbour at Porto Lume, from the mole](attachments/porto-lume-harbour.png)

## Facts

- marina: 200 berths, water and power on every pontoon, showers by the office
- fees: [[mooring-fees-2026]]
- harbour office: channel 17, 07:00–21:00 in season
- the chandlery is behind the fish market; the good bakery is the one on the square

## Getting there

By road two hours from the airport; the last bus leaves at 19:40.
";

const JOULE_BODY: &str = "The heat a conductor gives off when a current runs through it: *P = I²R*. \
A thin cable has more resistance a metre than a thick one, so the same lamp \
warms a 0.5 mm² cable more than a 0.75 mm² one — which is what the copper \
lamp's first cable did ([[lamp-build]]).

For a lamp of 40 W on 230 V the current is under 0.2 A, and the warmth is \
harmless; it was the cheap holder's cable, rated for less, that mattered.
";

const FEES_BODY: &str = "The marina's price list for 2026, as [the PDF](sources/mooring-fees-2026.pdf) \
the office sent in August. Prices are per night, water and power included, \
and the season runs June to September.

| length | June | July–August | September |
|---|---|---|---|
| up to 10 m | 44 € | 58 € | 46 € |
| up to 12 m | 58 € | 76 € | 62 € |
| up to 14 m | 74 € | 96 € | 78 € |

A week paid in advance is six nights. The town's tourist tax, 1.50 € a \
person a night, comes on top. See [[porto-lume]] and the [[sailing-trip-2026]]; \
filed the way [[filing-a-source]] says.
";

const SKILL_BODY: &str = "When a source arrives — a letter, a PDF, a screenshot, a saved page — file it \
so the fact can be found again.

1. Put the raw file under `sources/` with `kb.attach`, named by what it is and \
   when: `sources/mooring-fees-2026.pdf`.
2. Write one page of kind `source` with `kb.write`: the facts a later question \
   would need, dated, in plain words. Link the file. Prefer a link to a retelling.
3. Link the page from the pages whose facts it changes, and only those.
4. Never fill a gap with a likely answer. Say what the source says, and what \
   is still open.

The summary line is what the catalogue shows and what every chat reads first: \
one sentence, the fact that matters. What the person said to keep is on \
[[remembered]]; read it before asking twice.
";

const MEMORY_BODY: &str = "- 2026-08-12 — prefers prices dated; a number without a date is a question
- 2026-08-20 — the lamp project is the workshop's current one; ask about it before starting another
- 2026-08-30 — the marina office at Porto Lume closes at 16:00 on Sundays
";

const INBOX_BODY: &str = "Channel 17 is the harbour office. Channel 72 is what the charter base uses \
between its boats. The coastguard listens on 16 and works on 10. Nobody has \
said which one the fuel pontoon answers — ask on arrival.
";

const PAGES: &[Seed] = &[
    Seed {
        slug: "lamp-build",
        kind: "project",
        title: "The copper desk lamp",
        summary: "A desk lamp from copper pipe on an oak foot: the parts, where they came from, what is left to do.",
        aliases: &["copper lamp"],
        tags: &["workshop", "lamp"],
        extra: "{}",
        body: LAMP_BODY,
        path: "lamp-build.md",
        created: (2026, 7, 14, 19, 20),
        updated: (2026, 8, 30, 18, 5),
    },
    Seed {
        slug: "sailing-trip-2026",
        kind: "project",
        title: "Sailing the gulf, September 2026",
        summary: "Ten days on a chartered 34-footer out of Porto Lume, the second week of September.",
        aliases: &[],
        tags: &["sailing", "travel"],
        extra: "{}",
        body: SAILING_BODY,
        path: "sailing-trip-2026.md",
        created: (2026, 8, 2, 10, 0),
        updated: (2026, 8, 29, 21, 15),
    },
    Seed {
        slug: "joule-heating",
        kind: "concept",
        title: "Joule heating",
        summary: "Why a thin cable runs warm: the heat a current makes in a conductor, P = I²R.",
        aliases: &["resistive heating"],
        tags: &["electricity"],
        extra: "{}",
        body: JOULE_BODY,
        path: "joule-heating.md",
        created: (2026, 5, 3, 9, 30),
        updated: (2026, 5, 3, 9, 30),
    },
    Seed {
        slug: "porto-lume",
        kind: "entity",
        title: "Porto Lume",
        summary: "The harbour town the sailing trip starts from: the marina, the office, the chandlery.",
        aliases: &["Lume"],
        tags: &["city", "harbour", "travel"],
        extra: "{}",
        body: PORTO_BODY,
        path: "porto-lume.md",
        created: (2026, 8, 2, 10, 30),
        updated: (2026, 8, 28, 8, 45),
    },
    Seed {
        slug: "mooring-fees-2026",
        kind: "source",
        title: "Porto Lume marina — mooring fees 2026",
        summary: "The marina's 2026 price list: per night by length and month, tourist tax on top.",
        aliases: &[],
        tags: &["sailing", "prices"],
        extra: "{\"source\":\"the marina office, by mail\"}",
        body: FEES_BODY,
        path: "mooring-fees-2026.md",
        created: (2026, 8, 11, 17, 2),
        updated: (2026, 8, 11, 17, 2),
    },
    Seed {
        slug: "filing-a-source",
        kind: "skill",
        title: "How to file a source",
        summary: "The four steps that turn a letter or a PDF into a page a later question can find.",
        aliases: &[],
        tags: &["wiki"],
        extra: "{}",
        body: SKILL_BODY,
        path: "",
        created: (2026, 7, 1, 12, 0),
        updated: (2026, 8, 12, 9, 10),
    },
    Seed {
        slug: "remembered",
        kind: "memory",
        title: "What the agent keeps",
        summary: "One dated line per thing worth keeping between chats.",
        aliases: &[],
        tags: &[],
        extra: "{}",
        body: MEMORY_BODY,
        path: "",
        created: (2026, 8, 12, 9, 12),
        updated: (2026, 8, 30, 20, 40),
    },
    Seed {
        slug: "harbour-radio-channels",
        kind: "inbox",
        title: "Harbour radio channels",
        summary: "Which VHF channel is whose at Porto Lume — not yet filed anywhere.",
        aliases: &[],
        tags: &[],
        extra: "{\"captured\":\"2026-08-30\",\"source\":\"the charter base's welcome mail\"}",
        body: INBOX_BODY,
        path: "inbox/harbour-radio-channels.md",
        created: (2026, 8, 30, 11, 5),
        updated: (2026, 8, 30, 11, 5),
    },
];

/// The seed, once, on a store that has no pages.
pub fn seed(store: &Store, mode: Mode) -> rusqlite::Result<()> {
    if mode == Mode::Real {
        return Ok(());
    }
    store.write(|c| {
        if c.query_row("SELECT COUNT(*) FROM kb_page", [], |r| r.get::<_, i64>(0))? > 0 {
            return Ok(());
        }
        pages(c)?;
        files(c)?;
        revisions(c)?;
        Ok(())
    })
}

fn pages(c: &Connection) -> rusqlite::Result<()> {
    for p in PAGES {
        let uid = uid_of(p.slug);
        let aliases: Vec<String> = p.aliases.iter().map(|s| (*s).to_string()).collect();
        c.execute(
            "INSERT INTO kb_page(uid, slug, kind, title, summary, aliases, tags, extra, body, path, created, updated)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                uid,
                p.slug,
                p.kind,
                p.title,
                p.summary,
                serde_json::to_string(&aliases).unwrap_or_default(),
                serde_json::to_string(p.tags).unwrap_or_default(),
                p.extra,
                p.body,
                p.path,
                at(p.created),
                at(p.updated)
            ],
        )?;
        realias(c, &uid, &aliases)?;
        relink(c, &uid, p.body, at(p.updated))?;
    }
    Ok(())
}

fn files(c: &Connection) -> rusqlite::Result<()> {
    type Row<'a> = (&'a str, &'a str, Vec<u8>, &'a str, &'a str, f64);
    let rows: [Row<'_>; 3] = [
        (PICTURE, "image/png", picture_png(), "", "cached", ts(2026, 8, 28, 8, 40)),
        (PDF, "application/pdf", pdf_bytes(), "", "cached", ts(2026, 8, 11, 16, 58)),
        (TEXT, "text/plain", parts_text().into_bytes(), &parts_text(), "outbox", ts(2026, 8, 30, 18, 0)),
    ];
    for (path, mime, bytes, text, state, at) in rows {
        c.execute(
            "INSERT INTO kb_file(path, hash, mime, size, text, created, updated) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?6)",
            params![path, hash_of(path), mime, bytes.len() as i64, text, at],
        )?;
        c.execute("INSERT INTO kb_cache(hash, state) VALUES(?1, ?2)", params![hash_of(path), state])?;
    }
    Ok(())
}

/// One revision per page, author `import`, and the lamp page's four: the
/// import, an edit, the agent's addition of the invoice in *file the tax
/// letter*, and one more edit. The memory page's last line was
/// `kb.remember`'s, in a chat of its own.
fn revisions(c: &Connection) -> rusqlite::Result<()> {
    let doc = |p: &Seed, body: &str| {
        let front = super::markdown::Front {
            kind: p.kind.into(),
            title: p.title.into(),
            summary: p.summary.into(),
            slug: String::new(),
            aliases: p.aliases.iter().map(|s| (*s).to_string()).collect(),
            tags: p.tags.iter().map(|s| (*s).to_string()).collect(),
            extra: p.extra.into(),
        };
        super::markdown::document(&front, body)
    };
    let put = |uid: &str, page: &str, at: f64, device: &str, author: &str, chat_title: &str, message: &str, body: &str| {
        c.execute(
            "INSERT INTO kb_revision(uid, page, at, device, author, chat_title, message, body) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![uid, page, at, device, author, chat_title, message, body],
        )
    };
    for p in PAGES {
        let page = uid_of(p.slug);
        let imported = "imported from ~/cloud/KB at 3f9c2e1".to_string();
        match p.slug {
            "lamp-build" => {
                put(&hash_of("rev:lamp-build:1")[..32], &page, ts(2026, 7, 14, 19, 20), "Andrey's Mac", "import", "", &imported, &doc(p, LAMP_BODY_BEFORE))?;
                put(&hash_of("rev:lamp-build:2")[..32], &page, ts(2026, 8, 20, 22, 10), "Andrey's Mac", "editor", "", "edited", &doc(p, LAMP_BODY_BEFORE))?;
                put(&hash_of("rev:lamp-build:3")[..32], &page, ts(2026, 8, 30, 17, 50), "Andrey's Mac", "chat:2", "file the tax letter", "added the invoice and its total to the parts section", &doc(p, LAMP_BODY))?;
                put(&hash_of("rev:lamp-build:4")[..32], &page, at(p.updated), "the Fold", "editor", "", "edited", &doc(p, LAMP_BODY))?;
            }
            "remembered" => {
                put(&hash_of("rev:remembered:1")[..32], &page, at(p.created), "Andrey's Mac", "chat:1", "what does my kb say about porto lume", "remembered one line", &doc(p, "- 2026-08-12 — prefers prices dated; a number without a date is a question\n"))?;
                put(&hash_of("rev:remembered:2")[..32], &page, at(p.updated), "Andrey's Mac", "chat:3", "remember that the marina office closes at 16:00 on Sundays", "remembered one line", &doc(p, p.body))?;
            }
            _ => {
                let author = if p.path.is_empty() { "editor" } else { "import" };
                let message = if p.path.is_empty() { "edited" } else { imported.as_str() };
                put(&hash_of(&format!("rev:{}:1", p.slug))[..32], &page, at(p.created), "Andrey's Mac", author, "", message, &doc(p, p.body))?;
            }
        }
    }
    Ok(())
}

// -- the files' bytes ------------------------------------------------------------

/// The bytes the cache would hand a card, by the file's hash. `None` for a
/// hash the seed did not make.
#[must_use]
pub fn bytes_of(hash: &str) -> Option<Vec<u8>> {
    if hash == hash_of(PICTURE) {
        Some(picture_png())
    } else if hash == hash_of(PDF) {
        Some(pdf_bytes())
    } else if hash == hash_of(TEXT) {
        Some(parts_text().into_bytes())
    } else {
        None
    }
}

fn parts_text() -> String {
    "copper lamp — parts as bought, 14 Jul 2026\n\n\
     1.2 m   copper pipe 15 mm            9.80\n\
     2       elbow 90°, solder            3.40\n\
     1       E27 holder, brass, cord grip 14.90\n\
     1       oak blank 90 x 90 x 40       7.50\n\
     2 m     cable 0.75 mm², black        6.20\n\
     1       inline switch                 6.40\n\
     \n\
     total                                48.20\n"
        .to_string()
}

/// The harbour, drawn: a sky, a sea, a mole with a light on it, and the
/// sun low over the water. 360 by 220, encoded as a PNG.
#[must_use]
pub fn picture_png() -> Vec<u8> {
    let (w, h) = (360u32, 220u32);
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    let horizon = 128i64;
    for y in 0..h as i64 {
        for x in 0..w as i64 {
            let (mut r, mut g, mut b) = if y < horizon {
                // Sky: pale at the horizon, deeper above.
                let t = y as f64 / horizon as f64;
                (lerp(150.0, 214.0, t), lerp(176.0, 222.0, t), lerp(205.0, 228.0, t))
            } else {
                // Sea: darker down the picture, a ripple every few rows.
                let t = (y - horizon) as f64 / (h as i64 - horizon) as f64;
                let ripple = if (x / 7 + y) % 9 == 0 { 12.0 } else { 0.0 };
                (lerp(96.0, 58.0, t) + ripple, lerp(140.0, 96.0, t) + ripple, lerp(168.0, 126.0, t) + ripple)
            };
            // The sun, low on the right, and its path on the water.
            let (sx, sy) = (270i64, 96i64);
            let d2 = (x - sx) * (x - sx) + (y - sy) * (y - sy);
            if d2 < 19 * 19 {
                (r, g, b) = (250.0, 236.0, 196.0);
            } else if y >= horizon && (x - sx).abs() < 14 && (x + y) % 5 == 0 {
                (r, g, b) = (r + 60.0, g + 50.0, b + 30.0);
            }
            // The mole: a dark bar from the left, and the light at its end.
            if (120..=horizon + 8).contains(&y) && x < 200 && y >= horizon - 8 {
                (r, g, b) = (72.0, 70.0, 66.0);
            }
            if (190..198).contains(&x) && (horizon - 34..horizon - 8).contains(&y) {
                (r, g, b) = (232.0, 228.0, 220.0);
            }
            if (188..200).contains(&x) && (horizon - 40..horizon - 34).contains(&y) {
                (r, g, b) = (60.0, 58.0, 56.0);
            }
            rgba.extend([r.clamp(0.0, 255.0) as u8, g.clamp(0.0, 255.0) as u8, b.clamp(0.0, 255.0) as u8, 255]);
        }
    }
    crate::reader::picture::encode(w, h, &rgba).unwrap_or_default()
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

/// One A4 page of the marina's fees, as an uncompressed PDF the shared
/// viewer rasterizes and reads the text of.
#[must_use]
pub fn pdf_bytes() -> Vec<u8> {
    let lines: &[(&str, f64, i32)] = &[
        ("Porto Lume Marina", 18.0, 780),
        ("Mooring fees 2026 - per night, water and power included", 11.0, 756),
        ("length            June      July-August   September", 11.0, 712),
        ("up to 10 m        44 EUR    58 EUR        46 EUR", 11.0, 694),
        ("up to 12 m        58 EUR    76 EUR        62 EUR", 11.0, 676),
        ("up to 14 m        74 EUR    96 EUR        78 EUR", 11.0, 658),
        ("A week paid in advance is six nights.", 11.0, 620),
        ("Tourist tax 1.50 EUR a person a night, collected by the office.", 11.0, 602),
        ("Harbour office: VHF 17, 07:00 - 21:00 in season.", 11.0, 584),
    ];
    let mut content = String::from("BT\n");
    for (text, size, y) in lines {
        let font = if *size > 12.0 { "F2" } else { "F1" };
        content.push_str(&format!("/{font} {size} Tf 1 0 0 1 56 {y} Tm ({}) Tj\n", text.replace('(', "\\(").replace(')', "\\)")));
    }
    content.push_str("ET\n");
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Contents 4 0 R /Resources << /Font << /F1 5 0 R /F2 6 0 R >> >> >>".to_string(),
        format!("<< /Length {} >>\nstream\n{content}endstream", content.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold >>".to_string(),
    ];
    let mut out = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (i, obj) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.push_str(&format!("{} 0 obj\n{obj}\nendobj\n", i + 1));
    }
    let xref = out.len();
    out.push_str(&format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1));
    for o in offsets {
        out.push_str(&format!("{o:010} 00000 n \n"));
    }
    out.push_str(&format!(
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        objects.len() + 1
    ));
    out.into_bytes()
}
