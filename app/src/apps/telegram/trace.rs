//! Device-local diagnostic lines, separate from runtime coordination.

pub fn error(store_dir: Option<&std::path::Path>, line: &str) {
    eprintln!("telegram: {line}");
    note(store_dir, &format!("!! {line}"));
}

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

#[cfg(test)]
mod tests {
    #[test]
    fn errors_are_written_to_the_device_log() {
        let dir = std::env::temp_dir().join(format!(
            "superapp-telegram-error-log-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        super::error(
            Some(&dir),
            "request 42 sendMessage chat=7: FILE_INVALID (400)",
        );
        let log = std::fs::read_to_string(dir.join("tg-debug.log")).unwrap();
        assert!(log.contains("!! request 42 sendMessage chat=7: FILE_INVALID (400)"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
