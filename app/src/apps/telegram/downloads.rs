//! Copies explicitly requested attachments out of the evictable media cache.
//! The account worker owns the transfer, so closing a panel does not cancel it.

use std::path::{Path, PathBuf};

use kernel::caps::{self, CopyPath, Disk, MakeDir};
use kernel::effect::World;
use kernel::panel::Verb;
use kernel::session::Session;

use super::{model::Msg, panels, requests, runtime};

pub fn reference(m: &Msg) -> Option<&str> {
    let media = m.media.as_ref()?;
    let reference = match media.kind.as_str() {
        "video" | "circle" | "animation" => media.clip.as_deref(),
        "file" | "photo" | "audio" | "voice" => media.reference.as_deref(),
        _ => None,
    }?;
    reference.starts_with("tg:").then_some(reference)
}

pub fn verb(m: &Msg) -> Option<Verb> {
    reference(m).map(|_| Verb::run("telegram.download", "download", None))
}

pub fn request(s: &mut Session, m: &Msg) {
    if reference(m).is_none() {
        return;
    }
    let context = requests::save_context(m.chat, m.id);
    let rt = runtime::of(s.store());
    if rt.operations.pending_context(&context) {
        return;
    }
    if panels::wire(s.store(), &requests::save_file(m.chat, m.id)) {
        s.notify("downloading to ~/Downloads", false);
    } else {
        s.notify(
            "Telegram is not connected; try again after reconnecting",
            true,
        );
    }
    s.redraw();
}

/// Documents already retain their original name in the projection. Other
/// cached media gets a stable name when the source metadata is unavailable.
pub fn name(m: &Msg) -> String {
    let media = m.media.as_ref();
    if let Some(name) = media
        .filter(|m| m.kind == "file")
        .and_then(|m| m.label.as_deref())
    {
        return safe_name(name);
    }
    let extension = match media.map(|m| m.kind.as_str()) {
        Some("photo") => "jpg",
        Some("video" | "circle" | "animation") => "mp4",
        Some("voice") => "ogg",
        _ => "bin",
    };
    format!("telegram-{}-{}.{}", m.chat, m.id, extension)
}

/// A sender's filename is a basename, never a path on this device.
pub fn safe_name(name: &str) -> String {
    let name = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let name: String = name
        .chars()
        .map(|c| if c.is_control() || c == ':' { '_' } else { c })
        .collect();
    let name = name.trim().trim_start_matches('.');
    if name.is_empty() {
        return "telegram-file".into();
    }
    // Leave room for a collision suffix on filesystems with 255-byte names,
    // retaining ordinary extensions and never splitting a UTF-8 character.
    let extension = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .filter(|e| e.len() <= 32)
        .map(|e| format!(".{e}"))
        .unwrap_or_default();
    let stem = &name[..name.len() - extension.len()];
    let mut end = stem.len().min(240 - extension.len());
    while !stem.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{extension}", &stem[..end])
}

/// Runs on the account worker. Disk's copy claims the destination exclusively
/// and removes partial copies on failure. No large file is read into memory.
pub fn save(w: &World, source: &Path, name: &str) -> Result<PathBuf, String> {
    let dir = caps::real_path("~/Downloads");
    if w.with_cap::<dyn Disk, _>(|d| d.stat(&dir))??.is_none() {
        w.run(&MakeDir { path: &dir })?;
    }
    let name = safe_name(name);
    let path = Path::new(&name);
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let extension = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    for n in 0..10_000 {
        let to = dir.join(if n == 0 {
            name.clone()
        } else {
            format!("{stem} ({n}){extension}")
        });
        if w.with_cap::<dyn Disk, _>(|d| d.stat(&to))??.is_some() {
            continue;
        }
        match w.run(&CopyPath {
            from: source,
            to: &to,
        }) {
            Ok(()) => return Ok(to),
            // Another writer may have claimed the name after our stat.
            Err(_) if w.with_cap::<dyn Disk, _>(|d| d.stat(&to))??.is_some() => continue,
            Err(error) => return Err(error),
        }
    }
    Err("Too many files with this name in ~/Downloads".into())
}
