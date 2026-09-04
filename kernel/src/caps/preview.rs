//! What a file *is*, and how much of it a card shows.
//!
//! Beside [`FileKind`](super::FileKind), because two apps ask the same
//! questions of the same bytes: the files app about a path on a disk, mail
//! about a part of a letter. The media type a name claims, whether a picture
//! is worth decoding and how big it is, how much to read, and how big a thing
//! may be before another app refuses to carry it — all of it is about bytes
//! and names, so none of it is either app's.
//!
//! Nothing here reaches a disk: [`preview_of`] is handed a reader and decides
//! only *whether* to call it.

use super::FileKind;

/// How much of a text file the card reads.
pub const TEXT_PREVIEW_MAX: usize = 64 * 1024;

/// How much of an image the card decodes. Past this the card is its lines
/// alone: a picture nobody can see is not worth the pause it costs to read.
pub const IMAGE_PREVIEW_MAX: usize = 20 * 1024 * 1024;

/// How big a file another app may carry out as a part. Past this the attach
/// refuses on the panel's status line rather than building something no
/// server will take.
pub const ATTACH_MAX: u64 = 25 * 1024 * 1024;

/// `1.2 MB`, `84 KB`, `640 B`.
#[must_use]
pub fn fmt_size(bytes: u64) -> String {
    let b = bytes as f64;
    let unit = |v: f64, u: &str| {
        if v < 10.0 {
            format!("{v:.1} {u}")
        } else {
            format!("{v:.0} {u}")
        }
    };
    if b < 1024.0 {
        format!("{bytes} B")
    } else if b < 1024.0 * 1024.0 {
        unit(b / 1024.0, "KB")
    } else if b < 1024.0 * 1024.0 * 1024.0 {
        unit(b / (1024.0 * 1024.0), "MB")
    } else {
        unit(b / (1024.0 * 1024.0 * 1024.0), "GB")
    }
}

/// The media type a name claims. What a part another app carries out is
/// labelled with, and what a card shows where the kind word says little — a
/// short table over the kinds these apps actually meet, and
/// `application/octet-stream` for everything else, which is the honest
/// answer rather than a guess.
#[must_use]
pub fn mime_of(name: &str) -> &'static str {
    match ext_of(name).as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("heic") => "image/heic",
        Some("svg") => "image/svg+xml",
        Some("pdf") => "application/pdf",
        Some("zip") => "application/zip",
        Some("gz" | "tgz") => "application/gzip",
        Some("tar") => "application/x-tar",
        Some("txt" | "log") => "text/plain",
        Some("md") => "text/markdown",
        Some("csv") => "text/csv",
        Some("html") => "text/html",
        Some("json") => "application/json",
        Some("xml") => "application/xml",
        Some("yaml" | "yml") => "application/yaml",
        Some("rs" | "toml" | "tla" | "cfg" | "sh") => "text/plain",
        _ => "application/octet-stream",
    }
}

/// A name's extension, folded — the one place the two tables here read it.
fn ext_of(name: &str) -> Option<String> {
    name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase())
}

/// The two picture formats a card decodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    Jpeg,
}

/// Which decoder a picture *probably* wants, off its name — enough to
/// decide whether to read the file at all. What actually decodes it is the
/// bytes' own account of themselves: a name lies often enough (a PNG saved
/// as `.jpg`) that trusting it would leave a picture unshown.
#[must_use]
pub fn image_format(name: &str) -> Option<ImageFormat> {
    match ext_of(name).as_deref() {
        Some("png") => Some(ImageFormat::Png),
        Some("jpg" | "jpeg") => Some(ImageFormat::Jpeg),
        _ => None,
    }
}

/// A PNG's signature. The card widget knows it too — picking a decoder is
/// the drawing half's business — and this half needs it to read a size.
const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

/// A picture's `(width, height)` in pixels off its header alone — the PNG's
/// IHDR, a JPEG's first frame marker — so the card can wish its rows before
/// anything is decoded. `None` for what is not one.
#[must_use]
pub fn image_size(bytes: &[u8]) -> Option<(u32, u32)> {
    // PNG: an 8-byte signature, then the IHDR chunk: length, "IHDR", width,
    // height — big-endian.
    if bytes.len() >= 24 && bytes.starts_with(PNG_MAGIC) && &bytes[12..16] == b"IHDR" {
        let w = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
        let h = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
        return (w > 0 && h > 0).then_some((w, h));
    }
    // JPEG: segments of `FF xx len…`; the size sits in the first SOF
    // segment (C0–CF, not the tables at C4, C8, CC).
    if bytes.len() >= 4 && bytes[0] == 0xFF && bytes[1] == 0xD8 {
        let mut i = 2;
        while i + 4 <= bytes.len() {
            if bytes[i] != 0xFF {
                i += 1;
                continue;
            }
            let marker = bytes[i + 1];
            if marker == 0xFF {
                i += 1;
                continue;
            }
            if (0xD0..=0xD9).contains(&marker) || marker == 0x01 {
                i += 2;
                continue;
            }
            let len = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
            let sof = (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC);
            if sof {
                if i + 9 > bytes.len() {
                    return None;
                }
                let h = u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]);
                let w = u16::from_be_bytes([bytes[i + 7], bytes[i + 8]]);
                return (w > 0 && h > 0).then_some((u32::from(w), u32::from(h)));
            }
            i += 2 + len.max(2);
        }
    }
    None
}

/// A file's contents, as far as a card shows them: a text file's reading, a
/// picture's bytes, or nothing at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Preview {
    Text(String),
    Image(Vec<u8>),
    None,
}

/// The preview a card of this kind wants, read through `read` — which is
/// handed the ceiling and answers `None` when the bytes cannot be had.
///
/// The kind decides *whether* to read at all: a 38 MB disk image is never
/// pulled into a panel to be told it is not a picture, and a picture past
/// [`IMAGE_PREVIEW_MAX`] is its lines alone rather than a pause. `size` is
/// what the disk (or the part's row) says it is; a source that cannot say
/// passes 0.
pub fn preview_of(
    kind: FileKind,
    name: &str,
    size: u64,
    read: impl FnOnce(usize) -> Option<Vec<u8>>,
) -> Preview {
    match kind {
        FileKind::Text => match read(TEXT_PREVIEW_MAX) {
            Some(b) => Preview::Text(String::from_utf8_lossy(&b).into_owned()),
            None => Preview::None,
        },
        // The name says whether to read it; the bytes say how to decode it.
        FileKind::Image if image_format(name).is_some() && size <= IMAGE_PREVIEW_MAX as u64 => {
            match read(IMAGE_PREVIEW_MAX) {
                Some(b) => Preview::Image(b),
                None => Preview::None,
            }
        }
        _ => Preview::None,
    }
}
