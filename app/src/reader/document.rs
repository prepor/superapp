//! Bounded, provider-independent text reads of downloaded documents.
//! These run on an agent worker, never in a panel's draw or event handler.

use serde_json::{json, Value};

use super::picture;

pub const MAX_FILE: usize = 32 * 1024 * 1024;
pub const MAX_TEXT: usize = 64 * 1024;

pub fn offset(input: &Value) -> Result<usize, String> {
    match input.get("offset") {
        None => Ok(0),
        Some(value) => value
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(|| {
                "`offset` must be a nonnegative byte offset returned by this tool".into()
            }),
    }
}

pub fn check_size(size: u64) -> Result<(), String> {
    if size > MAX_FILE as u64 {
        Err("This file exceeds the 32 MiB document reading limit".into())
    } else {
        Ok(())
    }
}

/// A PDF's text layer, a picture described, or a text file followed by an
/// exact continuation offset. Binary payloads never become
/// replacement-character gibberish.
///
/// A picture is the one answer with no `text` on it. It is not read — it is
/// *named*, so that whatever called this can put the picture itself in front
/// of a model that can see one; the caller adds where the bytes are, because
/// this function knows only the bytes it was handed.
pub fn read(bytes: &[u8], name: &str, mime: &str, offset: usize) -> Result<Value, String> {
    check_size(bytes.len() as u64)?;
    if let Some(picture) = picture::of(bytes) {
        if bytes.len() > picture::MAX_IMAGE {
            return Err(format!(
                "{name} is a {} of {}, past the {} limit for looking at a picture",
                picture.mime,
                kernel::caps::fmt_size(bytes.len() as u64),
                kernel::caps::fmt_size(picture::MAX_IMAGE as u64)
            ));
        }
        return Ok(json!({
            "format": "image",
            "mime": picture.mime,
            "width": picture.width,
            "height": picture.height,
            "size": bytes.len(),
        }));
    }
    let pdf = bytes[..bytes.len().min(1024)]
        .windows(5)
        .any(|w| w == b"%PDF-")
        || mime.eq_ignore_ascii_case("application/pdf")
        || name.to_ascii_lowercase().ends_with(".pdf");
    let text = if pdf {
        // Malformed third-party PDFs must fail this call, not retire the
        // worker with an unanswered call still holding the agent run.
        let text = std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem(bytes))
            .map_err(|_| "The PDF could not be parsed".to_string())?
            .map_err(|error| format!("The PDF could not be read: {error}"))?;
        if text.trim().is_empty() {
            // Not a failure: a scanned page is a picture, and this says so
            // so the caller can rasterise it for a model that can look at
            // one. A model that cannot reads the note and stops guessing.
            //
            // `offset` counts pages here rather than bytes — there is no
            // text to index into — and it is echoed so that whoever
            // rasterises knows where to start.
            return Ok(json!({
                "format": "scanned",
                "offset": offset,
                "note": "This PDF has no text layer; its pages are pictures of a page, not text.",
            }));
        }
        text
    } else {
        let text = if bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff]) {
            if !bytes.len().is_multiple_of(2) {
                return Err("The UTF-16 text file has an incomplete character".into());
            }
            let little = bytes[0] == 0xff;
            let units: Vec<u16> = bytes[2..]
                .as_chunks::<2>().0.iter()
                .map(|pair| {
                    if little {
                        u16::from_le_bytes(*pair)
                    } else {
                        u16::from_be_bytes(*pair)
                    }
                })
                .collect();
            String::from_utf16(&units)
                .map_err(|_| "The file contains invalid UTF-16 text".to_string())?
        } else {
            std::str::from_utf8(bytes)
                .map_err(|_| unsupported(name))?
                .trim_start_matches('\u{feff}')
                .to_string()
        };
        if text
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t' | '\u{c}'))
        {
            return Err(unsupported(name));
        }
        text
    };
    if offset > text.len() || !text.is_char_boundary(offset) {
        return Err("`offset` is outside the extracted text or splits a UTF-8 character; use the returned next_offset".into());
    }
    let mut end = offset.saturating_add(MAX_TEXT).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let truncated = end < text.len();
    Ok(json!({
        "format": if pdf { "pdf" } else { "text" },
        "text": &text[offset..end],
        "offset": offset,
        "next_offset": truncated.then_some(end),
        "truncated": truncated,
        "total_text_bytes": text.len(),
    }))
}

fn unsupported(name: &str) -> String {
    format!("Cannot extract text from {name}: this reader supports PDF text layers, PNG/JPEG/WebP/GIF pictures and UTF-8/UTF-16 text files, not audio, video or other binary formats")
}

/// A valid PDF of `pages` pages with nothing written on any of them: what a
/// scan is, as far as a text layer is concerned.
#[cfg(test)]
pub(crate) fn test_scan(pages: usize) -> Vec<u8> {
    use std::fmt::Write as _;
    let kids: Vec<String> = (0..pages).map(|n| format!("{} 0 R", 3 + n)).collect();
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        format!(
            "<< /Type /Pages /Kids [{}] /Count {pages} >>",
            kids.join(" ")
        ),
    ];
    // Each page is a different width, so a test can tell one rendering from
    // another without looking at pixels.
    for n in 0..pages {
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} 200] >>",
            100 + n * 10
        ));
    }
    let mut pdf = "%PDF-1.4\n".to_string();
    let mut offsets = Vec::new();
    for (i, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        let _ = write!(pdf, "{} 0 obj\n{object}\nendobj\n", i + 1);
    }
    let xref = pdf.len();
    let size = objects.len() + 1;
    let _ = writeln!(pdf, "xref\n0 {size}\n0000000000 65535 f ");
    for offset in offsets {
        let _ = writeln!(pdf, "{offset:010} 00000 n ");
    }
    let _ = write!(
        pdf,
        "trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n"
    );
    pdf.into_bytes()
}

/// A small, valid PDF shared by the mail, Telegram and agent integration tests.
#[cfg(test)]
pub(crate) fn test_pdf(text: &str) -> Vec<u8> {
    let text = text
        .replace('\\', "\\\\")
        .replace('(', "\\(")
        .replace(')', "\\)");
    let stream = format!("BT /F1 12 Tf 20 100 Td ({text}) Tj ET");
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".to_string(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_string(),
        format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()),
    ];
    let mut pdf = "%PDF-1.4\n".to_string();
    let mut offsets = Vec::new();
    for (i, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.push_str(&format!("{} 0 obj\n{object}\nendobj\n", i + 1));
    }
    let xref = pdf.len();
    pdf.push_str("xref\n0 6\n0000000000 65535 f \n");
    for offset in offsets {
        pdf.push_str(&format!("{offset:010} 00000 n \n"));
    }
    pdf.push_str(&format!(
        "trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n"
    ));
    pdf.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pdf_text_is_extracted_even_when_the_filename_and_mime_are_generic() {
        let pdf = test_pdf("Bonjour depuis le PDF.");
        let result = read(&pdf, "download.bin", "application/octet-stream", 0).unwrap();
        assert_eq!(result["format"], "pdf");
        assert!(result["text"]
            .as_str()
            .unwrap()
            .contains("Bonjour depuis le PDF."));
        // A page with no text on it is not a failure any more: it is a
        // picture of a page, and the caller rasterises it for a model that
        // can look at one.
        let scanned = read(&test_pdf(""), "scan.pdf", "", 0).unwrap();
        assert_eq!(scanned["format"], "scanned");
        assert!(scanned["note"].as_str().unwrap().contains("no text layer"));
        assert!(scanned.get("text").is_none());
        assert!(scanned.get("look").is_none(), "the caller names the pages");
    }

    #[test]
    fn long_unicode_text_can_be_read_without_missing_or_replacing_characters() {
        let text = "Привет 🌍\n".repeat(10_000);
        let mut offset = 0;
        let mut recovered = String::new();
        loop {
            let result = read(text.as_bytes(), "letter.txt", "text/plain", offset).unwrap();
            let chunk = result["text"].as_str().unwrap();
            assert!(chunk.len() <= MAX_TEXT);
            recovered.push_str(chunk);
            let Some(next) = result["next_offset"].as_u64() else {
                break;
            };
            assert!(next as usize > offset);
            offset = next as usize;
        }
        assert_eq!(recovered, text);
        assert!(read(text.as_bytes(), "letter.txt", "", 1).is_err());
    }

    #[test]
    fn a_picture_is_described_rather_than_read_or_refused() {
        let png = super::super::picture::test_png(1280, 960);
        // The name and the media type both lie; the bytes do not.
        let out = read(&png, "receipt.jpg", "application/octet-stream", 0).unwrap();
        assert_eq!(out["format"], "image");
        assert_eq!(out["mime"], "image/png");
        assert_eq!(out["width"], 1280);
        assert_eq!(out["height"], 960);
        assert_eq!(out["size"], png.len());
        // There is nothing to page through, so it says nothing about text.
        assert!(out.get("text").is_none());
        assert!(out.get("next_offset").is_none());
        // The caller names where the bytes are; this reader never can.
        assert!(out.get("look").is_none());

        let huge = [png.clone(), vec![0; super::super::picture::MAX_IMAGE]].concat();
        let error = read(&huge, "huge.png", "", 0).unwrap_err();
        assert!(error.contains("huge.png") && error.contains("20 MB"), "{error}");
    }

    #[test]
    fn binary_corrupt_and_oversized_files_fail_honestly() {
        assert!(read(b"\x89PNG\0\r\n", "image.png", "image/png", 0).is_err());
        assert!(read(b"%PDF-broken", "file.pdf", "", 0)
            .unwrap_err()
            .contains("PDF"));
        assert!(check_size(MAX_FILE as u64 + 1).is_err());
        assert!(offset(&json!({"offset": -1})).is_err());
        assert!(read(b"abc", "a.txt", "", 4).is_err());
    }

    #[test]
    fn unicode_text_with_a_byte_order_mark_is_decoded() {
        let mut bytes = vec![0xff, 0xfe];
        bytes.extend("Привет".encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(read(&bytes, "a.txt", "", 0).unwrap()["text"], "Привет");
    }
}
