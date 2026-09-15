//! What a picture is, read off its first bytes — and what it becomes before
//! it is put in front of a model.
//!
//! A name lies: `receipt.jpg` is a PNG about as often as a person renames a
//! file. So the kind and the size of a picture come from the bytes, the way
//! [`document::read`](super::document::read) already tells a PDF by `%PDF-`
//! rather than by `.pdf`. Four kinds, and they are the four every provider
//! takes and makepad's decoder draws on every platform.
//!
//! Nothing here reaches a disk or a network, and [`of`] reads only a header
//! — a few dozen bytes — so a card or a tool can ask what something is
//! without paying for the whole file.

/// How big a picture may be before this reader declines to describe it at
/// all. Above this, an honest sentence beats a description nothing can use.
pub const MAX_IMAGE: usize = 20 * 1024 * 1024;

/// The longest edge a picture is reduced to before it goes out. Big enough
/// that a receipt, a screenshot or a page of handwriting is still legible;
/// small enough that a round of a chat is not a dozen megabytes of base64.
pub const LONG_EDGE: u32 = 1600;

/// What a picture is: the media type its bytes really claim, and how big it
/// is in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Picture {
    pub mime: &'static str,
    pub width: u32,
    pub height: u32,
}

/// The picture these bytes are, or `None` for anything else — including a
/// header too short or too damaged to say how big it is. A truncated PNG is
/// not a picture; it is a file that will disappoint whoever opens it, and
/// saying so is [`document::read`](super::document::read)'s job.
#[must_use]
pub fn of(bytes: &[u8]) -> Option<Picture> {
    png(bytes)
        .or_else(|| jpeg(bytes))
        .or_else(|| gif(bytes))
        .or_else(|| webp(bytes))
}

/// Whether these first bytes are the start of a picture, from the signature
/// alone — twelve bytes are enough for all four. What [`of`] needs the
/// header for is how *big* it is, which for a JPEG can sit past an entire
/// EXIF block; this is the cheap question a reader asks before deciding how
/// much of a file to pull off a disk.
#[must_use]
pub fn looks_like(b: &[u8]) -> bool {
    b.starts_with(b"\x89PNG\r\n\x1a\n")
        || b.starts_with(&[0xff, 0xd8, 0xff])
        || b.starts_with(b"GIF87a")
        || b.starts_with(b"GIF89a")
        || (b.starts_with(b"RIFF") && b.get(8..12) == Some(b"WEBP"))
}

fn be16(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(u16::from_be_bytes(
        b.get(at..at + 2)?.try_into().ok()?,
    )))
}

fn le16(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(u16::from_le_bytes(
        b.get(at..at + 2)?.try_into().ok()?,
    )))
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// The eight-byte signature, then the first chunk, which the format
/// *requires* to be `IHDR` with the two dimensions at its head.
fn png(b: &[u8]) -> Option<Picture> {
    if !b.starts_with(b"\x89PNG\r\n\x1a\n") || b.get(12..16)? != b"IHDR" {
        return None;
    }
    let (width, height) = (be32(b, 16)?, be32(b, 20)?);
    (width > 0 && height > 0).then_some(Picture {
        mime: "image/png",
        width,
        height,
    })
}

/// SOI, then a walk over the markers to the start-of-frame that carries the
/// dimensions. Everything before it — the JFIF and EXIF blocks a camera
/// writes, a thumbnail, a colour profile — is skipped by its own length.
fn jpeg(b: &[u8]) -> Option<Picture> {
    if !b.starts_with(&[0xff, 0xd8, 0xff]) {
        return None;
    }
    let mut at = 2;
    loop {
        // Padding: any number of 0xff bytes may sit between two markers.
        while *b.get(at)? == 0xff && *b.get(at + 1)? == 0xff {
            at += 1;
        }
        if *b.get(at)? != 0xff {
            return None;
        }
        let marker = *b.get(at + 1)?;
        match marker {
            // Standalone markers: no length follows them.
            0x01 | 0xd0..=0xd9 => at += 2,
            // The start of the entropy-coded scan. Past here there is no
            // frame header left to find.
            0xda => return None,
            // SOF0-SOF15, minus the four that are not frame headers.
            0xc0..=0xcf if !matches!(marker, 0xc4 | 0xc8 | 0xcc) => {
                let (height, width) = (be16(b, at + 5)?, be16(b, at + 7)?);
                return (width > 0 && height > 0).then_some(Picture {
                    mime: "image/jpeg",
                    width,
                    height,
                });
            }
            _ => at += 2 + be16(b, at + 2)? as usize,
        }
    }
}

/// The signature, then the logical screen descriptor's two little-endian
/// shorts.
fn gif(b: &[u8]) -> Option<Picture> {
    if !b.starts_with(b"GIF87a") && !b.starts_with(b"GIF89a") {
        return None;
    }
    let (width, height) = (le16(b, 6)?, le16(b, 8)?);
    (width > 0 && height > 0).then_some(Picture {
        mime: "image/gif",
        width,
        height,
    })
}

/// A RIFF container whose form is `WEBP`, in its three shapes: lossy,
/// lossless, and the extended header an animation or an alpha channel adds.
fn webp(b: &[u8]) -> Option<Picture> {
    if !b.starts_with(b"RIFF") || b.get(8..12)? != b"WEBP" {
        return None;
    }
    let (width, height) = match b.get(12..16)? {
        // The bitstream's three-byte sync code, then two 14-bit sizes.
        b"VP8 " => {
            if b.get(23..26)? != [0x9d, 0x01, 0x2a] {
                return None;
            }
            (le16(b, 26)? & 0x3fff, le16(b, 28)? & 0x3fff)
        }
        // A one-byte signature, then 14 bits of width-1 and 14 of height-1,
        // packed little-endian across four bytes.
        b"VP8L" => {
            if *b.get(20)? != 0x2f {
                return None;
            }
            let bits = u32::from_le_bytes(b.get(21..25)?.try_into().ok()?);
            ((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1)
        }
        // Three bytes of width-1 and three of height-1, little-endian.
        b"VP8X" => {
            let three = |at: usize| -> Option<u32> {
                let b = b.get(at..at + 3)?;
                Some(u32::from(b[0]) | u32::from(b[1]) << 8 | u32::from(b[2]) << 16)
            };
            (three(24)? + 1, three(27)? + 1)
        }
        _ => return None,
    };
    (width > 0 && height > 0).then_some(Picture {
        mime: "image/webp",
        width,
        height,
    })
}

/// This picture, reduced to something worth sending: the same bytes when it
/// is already small enough, and a PNG of its long edge at [`LONG_EDGE`] when
/// it is not.
///
/// Decoding is makepad's, which reads all four kinds on every platform and
/// needs no `Cx` — so this runs on a worker like everything else that reads
/// a document. The reduction is a box average, which is what a photograph
/// wants and costs one pass.
///
/// # Errors
///
/// If the bytes do not decode, or the reduced picture cannot be encoded.
pub fn shrink(bytes: &[u8], picture: Picture) -> Result<(Vec<u8>, &'static str), String> {
    // Decoded whether or not it needs reducing. [`of`] reads a header, and a
    // header is a claim: a truncated or corrupt file whose first bytes are a
    // valid IHDR would otherwise go out as a data URL and come back as a
    // failed round, instead of as a sentence said here. The original bytes
    // still go out where nothing needs changing — re-encoding a photograph
    // as a PNG would multiply it.
    let image = makepad_widgets::image_cache::decode_image_from_data(bytes)
        .map_err(|error| format!("this picture could not be decoded: {error:?}"))?;
    if picture.width.max(picture.height) <= LONG_EDGE {
        return Ok((bytes.to_vec(), picture.mime));
    }
    let (from_w, from_h) = (image.width.max(1), image.height.max(1));
    let scale = f64::from(LONG_EDGE) / from_w.max(from_h) as f64;
    let width = ((from_w as f64 * scale).round() as usize).clamp(1, from_w);
    let height = ((from_h as f64 * scale).round() as usize).clamp(1, from_h);
    let mut rgba = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        let (y0, y1) = (
            y * from_h / height,
            ((y + 1) * from_h / height).max(y * from_h / height + 1),
        );
        for x in 0..width {
            let (x0, x1) = (
                x * from_w / width,
                ((x + 1) * from_w / width).max(x * from_w / width + 1),
            );
            let (mut r, mut g, mut b, mut a, mut n) = (0u32, 0u32, 0u32, 0u32, 0u32);
            for sy in y0..y1.min(from_h) {
                for sx in x0..x1.min(from_w) {
                    let p = image.data.get(sy * from_w + sx).copied().unwrap_or(0);
                    a += (p >> 24) & 0xff;
                    r += (p >> 16) & 0xff;
                    g += (p >> 8) & 0xff;
                    b += p & 0xff;
                    n += 1;
                }
            }
            let n = n.max(1);
            rgba.extend([(r / n) as u8, (g / n) as u8, (b / n) as u8, (a / n) as u8]);
        }
    }
    Ok((encode(width as u32, height as u32, &rgba)?, "image/png"))
}

/// Straight RGBA bytes as a PNG. The one encoder in this tree: a reduced
/// photograph and a rasterised PDF page take the same road out.
///
/// # Errors
///
/// If the encoder refuses the size or the data.
pub fn encode(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|error| format!("this picture could not be encoded: {error}"))?;
        writer
            .write_image_data(rgba)
            .map_err(|error| format!("this picture could not be encoded: {error}"))?;
    }
    Ok(out)
}

/// The same, from the `0xAARRGGBB` words a decoder and a rasteriser both
/// hand back.
///
/// # Errors
///
/// As [`encode`].
pub fn encode_words(width: u32, height: u32, words: &[u32]) -> Result<Vec<u8>, String> {
    let mut rgba = Vec::with_capacity(words.len() * 4);
    for p in words {
        rgba.extend([(p >> 16) as u8, (p >> 8) as u8, *p as u8, (p >> 24) as u8]);
    }
    encode(width, height, &rgba)
}

/// The prefixes a blob key may wear for the request builder to be willing to
/// send what it names. The cache is one namespace every app writes to, and a
/// picture goes out only under a key one of these three minted.
pub const KEYS: &[&str] = &["tg:", "mail:", "agent:"];

/// Where the model will find this picture again, as a tool's `look` names
/// it: `[{"blob": "…"}]`.
///
/// A picture the app already keeps — a Telegram photo, a mail part whose
/// download cached it — keeps its own key and costs nothing twice. Anything
/// else is filed under `agent:<sha-256 of the bytes>`, so what the request
/// later puts in front of the model is fixed at the bytes the tool saw and
/// not at whatever the path holds by then.
///
/// # Errors
///
/// If there is no blob cache in this world, or it could not be written.
pub async fn keep(
    world: &kernel::effect::World,
    key: Option<&str>,
    bytes: &[u8],
) -> Result<serde_json::Value, String> {
    use kernel::caps::Blobs;
    if let Some(key) = key.filter(|k| KEYS.iter().any(|p| k.starts_with(p))) {
        let held = {
            let key = key.to_string();
            match world.with_cap::<dyn Blobs, _>(|cache| cache.background())? {
                Some(mut cache) => kernel::runtime::spawn_blocking(move || cache.contains(&key))
                    .await
                    .map_err(|error| error.to_string())?,
                None => world.with_cap::<dyn Blobs, _>(|cache| cache.contains(&key))?,
            }
        };
        if held {
            return Ok(serde_json::json!([{"blob": key}]));
        }
    }
    let mut hex = String::with_capacity(70);
    hex.push_str("agent:");
    for byte in ring::digest::digest(&ring::digest::SHA256, bytes).as_ref() {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    let bytes = bytes.to_vec();
    let key = hex.clone();
    match world.with_cap::<dyn Blobs, _>(|cache| cache.background())? {
        Some(mut cache) => kernel::runtime::spawn_blocking(move || {
            if !cache.contains(&key) {
                cache.put(&key, &bytes)?;
            }
            Ok::<_, String>(())
        })
        .await
        .map_err(|error| error.to_string())??,
        None => world.with_cap::<dyn Blobs, _>(|cache| {
            if cache.contains(&key) {
                return Ok(());
            }
            cache.put(&key, &bytes).map(|_| ())
        })??,
    }
    Ok(serde_json::json!([{"blob": hex}]))
}

/// What a tool adds to [`document::read`](super::document::read)'s answer
/// once it knows where the bytes came from: the key a picture is named by,
/// or the pages a scanned PDF was rasterised into.
///
/// The reader is handed bytes and nothing else, so it can describe a picture
/// but never say where one is. This is the other half, and it is the same
/// half for all three reading tools.
///
/// # Errors
///
/// If a picture could not be filed in the blob cache. A PDF whose pages will
/// not rasterise is not an error — the answer keeps its note and says so.
pub async fn looked(
    world: &kernel::effect::World,
    out: &mut serde_json::Value,
    key: Option<&str>,
    bytes: &[u8],
) -> Result<(), String> {
    use serde_json::{Value, json};
    match out["format"].as_str() {
        Some("image") => out["look"] = keep(world, key, bytes).await?,
        Some("scanned") => {
            let owned = bytes.to_vec();
            // A page index, not a byte one: `document::read` echoed what was
            // asked for, and a long scan is read a few pages at a time the
            // way a long text is read a window at a time.
            let from = out["offset"]
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .unwrap_or(0);
            let pages = kernel::runtime::spawn_blocking(move || super::pdf::pages(owned, from))
                .await
                .map_err(|error| error.to_string())?;
            match pages {
                // Nothing rendered from a document that has pages: the
                // `offset` asked for is past the end, which is the caller's
                // mistake and reads as one — the text window says the same
                // thing when its offset runs off the end.
                Ok((pages, total)) if pages.is_empty() && total > 0 => {
                    return Err(format!(
                        "`offset` is past this document: it has {total} page{}",
                        if total == 1 { "" } else { "s" }
                    ));
                }
                Ok((pages, total)) => {
                    let mut look = Vec::with_capacity(pages.len());
                    for page in &pages {
                        look.push(keep(world, None, page).await?[0].take());
                    }
                    let end = from + look.len();
                    out["pages"] = json!(look.len());
                    out["total_pages"] = json!(total);
                    out["truncated"] = json!(end < total);
                    if end < total {
                        out["next_offset"] = json!(end);
                    }
                    out["look"] = Value::Array(look);
                }
                Err(error) => {
                    out["note"] = json!(format!(
                        "This PDF has no text layer, and its pages could not be turned into pictures: {error}"
                    ));
                }
            }
        }
        _ => {}
    }
    Ok(())
}

/// One picture made ready to go out: its bytes and their media type, or
/// `None` where there are none to send.
pub type Looked = Option<(Vec<u8>, &'static str)>;

/// What each round's pictures came to, newest round first, inside one
/// request's budget.
///
/// A round is resolved whole or not at all: a receipt and its back are one
/// question, and half an answer is worse than a line saying where the other
/// half went. A picture whose blob has been evicted, or that will not decode,
/// comes back `None` and is named in words instead.
///
/// `get` marks a key most-recently-used, which is the intended reading of
/// recently used: a chat still looking at its eight pictures keeps them
/// against the cache's budget for as long as it is looking.
pub fn resolve(
    cache: &mut dyn kernel::caps::Blobs,
    rounds: &[Vec<String>],
    max: usize,
    max_bytes: usize,
) -> Vec<Vec<Looked>> {
    let mut out = vec![Vec::new(); rounds.len()];
    let (mut count, mut bytes) = (0usize, 0usize);
    for (at, round) in rounds.iter().enumerate() {
        if count + round.len() > max {
            out[at] = round.iter().map(|_| None).collect();
            continue;
        }
        let mut answers = Vec::with_capacity(round.len());
        let mut weight = 0usize;
        for key in round {
            answers.push(one(cache, key).inspect(|(bytes, _)| weight += bytes.len()));
        }
        if bytes + weight > max_bytes {
            out[at] = round.iter().map(|_| None).collect();
            continue;
        }
        count += answers.iter().filter(|a| a.is_some()).count();
        bytes += weight;
        out[at] = answers;
    }
    out
}

/// One key, as far as it can be taken: cached, still a picture, small enough,
/// and reduced if it is not.
fn one(cache: &mut dyn kernel::caps::Blobs, key: &str) -> Looked {
    if !KEYS.iter().any(|p| key.starts_with(p)) {
        return None;
    }
    let bytes = std::fs::read(cache.get(key)?).ok()?;
    // `put` and `ingest` overwrite a key, so what the tool checked may not
    // be what is there now: sniff and weigh it again before it goes out.
    let picture = of(&bytes).filter(|_| bytes.len() <= MAX_IMAGE)?;
    shrink(&bytes, picture).ok()
}

#[cfg(test)]
pub(crate) fn test_png(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    bytes.extend(13u32.to_be_bytes());
    bytes.extend(b"IHDR");
    bytes.extend(width.to_be_bytes());
    bytes.extend(height.to_be_bytes());
    // Bit depth 8, colour type 2 (truecolour), the three zeroes that follow.
    bytes.extend([8, 2, 0, 0, 0]);
    bytes.extend([0, 0, 0, 0]);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_kind_is_read_off_its_header_and_not_off_its_name() {
        assert_eq!(
            of(&test_png(1280, 960)),
            Some(Picture {
                mime: "image/png",
                width: 1280,
                height: 960
            })
        );

        // SOI, an APP0 block that must be skipped by its length, then SOF0.
        let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe0, 0x00, 0x04, 0x00, 0x00];
        jpeg.extend([0xff, 0xc0, 0x00, 0x11, 0x08]);
        jpeg.extend(480u16.to_be_bytes());
        jpeg.extend(640u16.to_be_bytes());
        assert_eq!(
            of(&jpeg),
            Some(Picture {
                mime: "image/jpeg",
                width: 640,
                height: 480
            })
        );

        let mut gif = b"GIF89a".to_vec();
        gif.extend(300u16.to_le_bytes());
        gif.extend(200u16.to_le_bytes());
        assert_eq!(
            of(&gif),
            Some(Picture {
                mime: "image/gif",
                width: 300,
                height: 200
            })
        );

        let mut webp = b"RIFF\0\0\0\0WEBPVP8X".to_vec();
        webp.extend([0, 0, 0, 0, 0, 0, 0, 0]);
        webp.extend([0x7f, 0x00, 0x00]); // width - 1 = 127
        webp.extend([0x3f, 0x00, 0x00]); // height - 1 = 63
        assert_eq!(
            of(&webp),
            Some(Picture {
                mime: "image/webp",
                width: 128,
                height: 64
            })
        );
    }

    /// A real PNG of `width` x `height`, encoded the way [`shrink`] encodes:
    /// the header fixtures above have no pixels and cannot be decoded.
    fn real_png(width: u32, height: u32) -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer
            .write_image_data(&vec![0x40; (width * height * 4) as usize])
            .unwrap();
        drop(writer);
        out
    }

    #[test]
    fn a_picture_within_the_long_edge_goes_out_as_it_came() {
        let small = real_png(64, 32);
        let (bytes, mime) = shrink(&small, of(&small).unwrap()).unwrap();
        assert_eq!(bytes, small, "nothing is re-encoded that need not be");
        assert_eq!(mime, "image/png");
    }

    #[test]
    fn a_header_that_lies_about_its_body_is_caught_here_and_not_by_a_provider() {
        // A valid signature and IHDR with nothing behind them: `of` reads
        // only the header and says 8x8, and a reader that trusted it would
        // base64 the wreck into a request and lose the round to a gateway
        // error instead of a sentence.
        let truncated = test_png(8, 8);
        let claimed = of(&truncated).expect("the header still parses");
        assert_eq!((claimed.width, claimed.height), (8, 8));
        let error = shrink(&truncated, claimed).unwrap_err();
        assert!(error.contains("could not be decoded"), "{error}");

        // The same for a picture past the long edge, which was already
        // decoded on its way through the reduction.
        let big = test_png(LONG_EDGE * 2, 8);
        assert!(shrink(&big, of(&big).unwrap()).is_err());
    }

    #[test]
    fn a_picture_past_the_long_edge_is_reduced_to_it_and_keeps_its_shape() {
        let big = real_png(LONG_EDGE * 2, LONG_EDGE / 2);
        let (bytes, mime) = shrink(&big, of(&big).unwrap()).unwrap();
        assert_eq!(mime, "image/png");
        let reduced = of(&bytes).expect("the reduction is still a picture");
        assert_eq!(reduced.width, LONG_EDGE);
        assert_eq!(reduced.height, LONG_EDGE / 4, "the aspect ratio is kept");
        assert!(bytes.len() < big.len());
    }

    #[test]
    fn the_budget_spends_itself_on_the_newest_rounds_and_drops_older_ones_whole() {
        use kernel::caps::{BlobCache, Blobs};
        let dir = std::env::temp_dir().join(format!("superapp-look-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut cache = BlobCache::at(dir.clone(), 64 * 1024 * 1024);
        let png = real_png(8, 8);
        for key in ["agent:a", "agent:b", "agent:c"] {
            cache.put(key, &png).unwrap();
        }
        cache.put("agent:gone", &png).unwrap();
        cache.remove("agent:gone");

        // Newest round first. The count stops at the third here, and a round
        // is dropped whole rather than half-answered.
        let rounds = vec![
            vec!["agent:a".to_string(), "agent:b".to_string()],
            vec!["agent:c".to_string()],
        ];
        let out = resolve(&mut cache, &rounds, 2, usize::MAX);
        assert_eq!(out[0].iter().filter(|a| a.is_some()).count(), 2);
        assert!(out[1][0].is_none(), "the older round did not fit");

        // The byte budget binds the same way.
        let out = resolve(&mut cache, &rounds, 8, png.len());
        assert_eq!(
            out[0].iter().filter(|a| a.is_some()).count(),
            0,
            "a round that would cross the byte budget is dropped whole"
        );
        assert!(
            out[1][0].is_some(),
            "and the one that fits is still carried"
        );

        // What the cache no longer holds, and what no app of this build
        // minted, are both a `None` the caller says in words.
        let missing = resolve(
            &mut cache,
            &[vec!["agent:gone".into()], vec!["file:///etc/passwd".into()]],
            8,
            usize::MAX,
        );
        assert!(missing[0][0].is_none() && missing[1][0].is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_header_that_cannot_say_how_big_it_is_is_not_a_picture() {
        // The signature alone, with no IHDR behind it: what
        // `document::read` must still refuse rather than describe.
        assert_eq!(of(b"\x89PNG\0\r\n"), None);
        assert_eq!(of(b"\x89PNG\r\n\x1a\n"), None);
        assert_eq!(of(&test_png(0, 10)), None);
        // A JPEG that runs into its scan without ever carrying a frame.
        assert_eq!(of(&[0xff, 0xd8, 0xff, 0xda, 0x00, 0x02]), None);
        assert_eq!(of(b"GIF89a"), None);
        assert_eq!(of(b"RIFF\0\0\0\0WEBPXXXX"), None);
        assert_eq!(of(b"%PDF-1.4"), None);
        assert_eq!(of(b"plain text"), None);
        assert_eq!(of(b""), None);
    }
}
