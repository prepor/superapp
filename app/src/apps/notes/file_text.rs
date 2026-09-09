//! The widget edits LF text; the file keeps its BOM and individual endings.

use similar::{capture_diff_slices, Algorithm};

pub fn editable(text: &str) -> String {
    text.strip_prefix('\u{feff}')
        .unwrap_or(text)
        .replace("\r\n", "\n")
}

pub fn encode(original: &str, text: &str) -> String {
    let (bom, original) = match original.strip_prefix('\u{feff}') {
        Some(original) => ("\u{feff}", original),
        None => ("", original),
    };
    let mut endings = original.split_inclusive('\n').filter_map(ending);
    let default = endings.next().unwrap_or("\n");
    // Ordinary LF and CRLF documents need no diff or per-line allocation.
    if endings.all(|ending| ending == default) {
        return format!("{bom}{}", text.replace('\n', default));
    }

    let old: Vec<_> = original.split_inclusive('\n').collect();
    let new: Vec<_> = text.split_inclusive('\n').collect();
    let old_content: Vec<_> = old.iter().map(|line| content(line)).collect();
    let new_content: Vec<_> = new.iter().map(|line| content(line)).collect();
    let mut bytes = String::with_capacity(bom.len() + text.len());
    bytes.push_str(bom);
    // Align line contents, so insertions and deletions do not shift the
    // endings of unchanged lines. Replaced lines reuse their old endings;
    // additional new lines follow the surrounding file's style.
    // https://docs.rs/similar/3.2.0/similar/fn.capture_diff_slices.html
    for op in capture_diff_slices(Algorithm::Patience, &old_content, &new_content) {
        let old_range = op.old_range();
        for (offset, line) in new[op.new_range()].iter().enumerate() {
            bytes.push_str(content(line));
            if line.ends_with('\n') {
                let index = old_range.start + offset.min(old_range.len().saturating_sub(1));
                let eol = old
                    .get(index)
                    .and_then(|line| ending(line))
                    .or_else(|| {
                        old.get(index.saturating_sub(1))
                            .and_then(|line| ending(line))
                    })
                    .unwrap_or(default);
                bytes.push_str(eol);
            }
        }
    }
    bytes
}

fn ending(line: &str) -> Option<&'static str> {
    if line.ends_with("\r\n") {
        Some("\r\n")
    } else if line.ends_with('\n') {
        Some("\n")
    } else {
        None
    }
}

fn content(line: &str) -> &str {
    line.strip_suffix("\r\n")
        .or_else(|| line.strip_suffix('\n'))
        .unwrap_or(line)
}
