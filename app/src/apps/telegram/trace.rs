//! Device-local diagnostic lines, separate from runtime coordination.

pub fn note(store_dir: Option<&std::path::Path>, line: &str) {
    use std::io::Write as _;
    let Some(dir) = store_dir else { return };
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("tg-debug.log"))
    {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(f, "{secs} {line}");
    }
}
