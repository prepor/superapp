//! The two repairs stored HTML still needs, and the base64 a `data:` image
//! carries its bytes in.
//!
//! [`guard`] is the last thing between a stored reading and the widget: the
//! narrowing runs at ingest, so a row written by an older build may still
//! carry a character reference the widget's parser unwraps into a panic. The
//! guarantee has to hold at the point of use as well as at the point of
//! writing.

/// Decodes base64 — standard or URL-safe alphabet, whitespace and padding
/// tolerated — which is how a `data:` image carries its bytes. `None` on
/// any other byte.
#[must_use]
pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            c if c.is_ascii_whitespace() => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// The inverse: standard base64, wrapped at 76 columns the way a MIME body
/// is. Used where this app *writes* a letter rather than reads one — the
/// fake transport's copy of an outgoing mail, the demo seed's parts.
#[must_use]
pub fn base64_encode(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    let mut col = 0;
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(A[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
        col += 4;
        if col >= 76 {
            out.push_str("\r\n");
            col = 0;
        }
    }
    out
}

/// One numeric character reference at `at`: its value and the index past
/// the `;`. `None` when this is not one — including digits the widget's own
/// parser would also reject, which it does cleanly.
fn num_ref(src: &str, at: usize) -> Option<(i64, usize)> {
    let b = src.as_bytes();
    if at + 3 >= b.len() || b[at] != b'&' || b[at + 1] != b'#' {
        return None;
    }
    let hex = b[at + 2] | 0x20 == b'x';
    let start = if hex { at + 3 } else { at + 2 };
    let mut j = start;
    while j < b.len() && b[j] != b';' {
        j += 1;
    }
    if j >= b.len() || j == start {
        return None;
    }
    let digits = &src[start..j];
    let n = if hex {
        i64::from_str_radix(digits, 16)
    } else {
        digits.parse::<i64>()
    };
    n.ok().map(|n| (n, j + 1))
}

/// Whether `src` carries a numeric reference the widget's parser would die
/// on. A pure scan with no allocation, so [`guard`] costs nothing on the
/// letters that are fine — which is all of them but the odd one.
fn needs_repair(src: &str) -> bool {
    let b = src.as_bytes();
    let mut i = 0usize;
    while i < b.len() {
        let Some(rel) = b[i..].iter().position(|&c| c == b'&') else {
            return false;
        };
        let at = i + rel;
        match num_ref(src, at) {
            Some((n, end)) => {
                if u32::try_from(n).ok().and_then(char::from_u32).is_none() {
                    return true;
                }
                i = end;
            }
            None => i = at + 1,
        }
    }
    false
}

/// Repairs numeric character references the widget's parser cannot decode.
///
/// It parses `&#N;` into a `u32` and then calls `char::from_u32(..).unwrap()`,
/// so a value that is not a Unicode scalar takes the process down. Real mail
/// is full of them: composers routinely emit an emoji as the two halves of
/// its UTF-16 surrogate pair, and `&#55357;&#56538;` is an ordinary 📚.
///
/// A pair is put back together into the character it meant; anything else out
/// of range becomes U+FFFD. References the parser *can* read are copied
/// through byte for byte.
fn fix_numeric_entities(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut i = 0usize;
    while i < src.len() {
        if let Some((n, end)) = num_ref(src, i) {
            // Readable as it stands: leave it exactly alone.
            if u32::try_from(n).ok().and_then(char::from_u32).is_some() {
                out.push_str(&src[i..end]);
                i = end;
                continue;
            }
            // A high surrogate whose low half follows it is half of a real
            // character, not a broken one.
            if (0xD800..=0xDBFF).contains(&n) {
                if let Some((lo, end2)) = num_ref(src, end) {
                    if (0xDC00..=0xDFFF).contains(&lo) {
                        let c = 0x1_0000 + ((n - 0xD800) << 10) + (lo - 0xDC00);
                        out.push(
                            u32::try_from(c)
                                .ok()
                                .and_then(char::from_u32)
                                .unwrap_or('\u{FFFD}'),
                        );
                        i = end2;
                        continue;
                    }
                }
            }
            out.push('\u{FFFD}');
            i = end;
            continue;
        }
        let ch = src[i..].chars().next().unwrap_or('\u{FFFD}');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// The last thing between stored HTML and the widget.
///
/// [`sanitize`] runs at **ingest**, so what the store holds was narrowed by
/// whichever version was current when the mail arrived. Rows written by a
/// build that passed character references through untouched may still carry
/// one the widget's parser unwraps into a panic — and a mail that crashes
/// the parser crashes it on every frame that draws it, which means the app
/// cannot be opened rather than that one letter looks wrong. So the
/// guarantee has to hold at the point of use and not only at the point of
/// writing. [`sanitize`] uses the same repair on its way in, since the
/// browser-grade parser would otherwise read each half of a surrogate pair
/// as U+FFFD.
///
/// Borrows unless there is something to repair, so a letter that is fine
/// costs one scan and no allocation.
#[must_use]
pub fn guard(src: &str) -> std::borrow::Cow<'_, str> {
    if needs_repair(src) {
        std::borrow::Cow::Owned(fix_numeric_entities(src))
    } else {
        std::borrow::Cow::Borrowed(src)
    }
}
