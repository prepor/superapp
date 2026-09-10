//! Transport failures suspend execution; corrupt or conflicting history needs
//! diagnosis. Keeping these categories distinct prevents a replay error from
//! masquerading as an unreachable bucket.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncError {
    Transport(String),
    Protocol(String),
    Changed,
}
impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(s) | Self::Protocol(s) => f.write_str(s),
            Self::Changed => f.write_str("ownership changed; checking again"),
        }
    }
}
impl std::error::Error for SyncError {}
impl From<String> for SyncError {
    fn from(s: String) -> Self {
        Self::Protocol(s)
    }
}
impl From<&str> for SyncError {
    fn from(s: &str) -> Self {
        Self::Protocol(s.into())
    }
}
impl From<rusqlite::Error> for SyncError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Protocol(e.to_string())
    }
}

impl From<SyncError> for String {
    fn from(error: SyncError) -> Self {
        error.to_string()
    }
}
