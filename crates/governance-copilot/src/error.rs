//! Typed errors for the GitHub Copilot connector (`thiserror` at the library
//! edge; binaries use `anyhow`).

/// Errors raised by the Copilot connector.
#[derive(Debug, thiserror::Error)]
pub enum CopilotError {
    /// GitHub returned a non-success status for an auth or report call.
    #[error("github {kind} failed with status {status}: {detail}")]
    Github {
        /// The GitHub API surface that failed (e.g. `app/installations`).
        kind: &'static str,
        /// The HTTP status code.
        status: u16,
        /// Redacted reason. Never echoes a token or a signed URL.
        detail: String,
    },

    /// We minted an invalid or unverifiable installation token.
    #[error("github token minting failed: {0}")]
    Token(String),

    /// A report payload could not be parsed into the normalized model.
    ///
    /// The raw bytes are archived to S3 before this is surfaced, so a fix is a
    /// `replay`, not a re-fetch (RFC-0001).
    #[error("report {report} for {day} failed to parse: {source}")]
    Parse {
        /// The report type (`organization-1-day`, ...).
        report: String,
        /// The report day.
        day: String,
        /// The underlying parse error.
        source: serde_json::Error,
    },

    /// An I/O operation failed.
    #[error("copilot connector io: {0}")]
    Io(#[from] std::io::Error),

    /// A reqwest transport error.
    #[error("copilot connector transport: {0}")]
    Transport(#[from] reqwest::Error),

    /// A failure writing to or reading from the raw archive (S3 or local).
    /// Kept opaque so the library never depends on a storage SDK; the archive
    /// sink is injected by the caller (RFC-0001).
    #[error("copilot archive: {0}")]
    Archive(String),

    /// A Postgres persistence error during a bulk upsert or manifest write.
    #[error("copilot persistence: {0}")]
    Storage(#[from] cratestack::sqlx::Error),
}

impl CopilotError {
    /// Build a `Github` error from a response status without echoing a body.
    pub fn github(kind: &'static str, status: u16, detail: impl Into<String>) -> Self {
        Self::Github {
            kind,
            status,
            detail: detail.into(),
        }
    }
}

/// Convenience alias for Copilot connector fallible operations.
pub type Result<T> = std::result::Result<T, CopilotError>;

// The secret newtype's redaction is load-bearing for the never-log-a-secret
// rule; keep a test pinning it so a future refactor cannot silently unredact.
#[cfg(test)]
mod tests {
    use crate::RawSecret;

    #[test]
    fn raw_secret_debug_and_display_are_redacted() {
        let secret = RawSecret("supersecret".to_owned());
        assert_eq!(format!("{secret:?}"), "<redacted>");
        assert_eq!(format!("{secret}"), "<redacted>");
    }
}
