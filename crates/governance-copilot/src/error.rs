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

    /// A report download exceeded the size cap and was rejected rather than
    /// buffered unbounded in memory.
    #[error("report {report} for {day} download was {size} bytes, over the {max} byte cap")]
    ReportTooLarge {
        /// The report type (`users-1-day`, ...).
        report: String,
        /// The report day.
        day: String,
        /// The size that was being accumulated when the cap tripped.
        size: u64,
        /// The configured cap.
        max: usize,
    },

    /// An I/O operation failed.
    #[error("copilot connector io: {0}")]
    Io(#[from] std::io::Error),

    /// A reqwest transport error. Always constructed via
    /// [`CopilotError::transport`], never the tuple variant directly (no
    /// `#[from]` here on purpose) -- see that constructor's doc comment.
    #[error("copilot connector transport: {0}")]
    Transport(reqwest::Error),

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

    /// Wrap a transport error, stripping any URL it carries first.
    ///
    /// `reqwest::Error` attaches the request URL to itself by default (it
    /// shows up in `Display` and in `Debug`) -- for `report.rs`'s second
    /// call, that URL is the short-lived SIGNED download URL RFC-0001
    /// describes, i.e. exactly the secret AGENTS.md says must never be
    /// logged. Every `CopilotError::Transport` in this crate goes through
    /// this constructor (the tuple variant has no `#[from]`, so nothing can
    /// bypass it via `?`), which is what makes every downstream `%e`/
    /// `{}`/`{:?}` on a `CopilotError` safe by construction -- retry
    /// logging (`client.rs`) included -- rather than something every new
    /// call site has to remember to redact itself.
    pub(crate) fn transport(e: reqwest::Error) -> Self {
        Self::Transport(e.without_url())
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
        let secret = RawSecret::new("supersecret".to_owned());
        assert_eq!(format!("{secret:?}"), "<redacted>");
        assert_eq!(format!("{secret}"), "<redacted>");
    }

    /// `reqwest::Error` attaches the request URL to itself by default; for
    /// the signed report-download call that URL IS the secret. Prove
    /// `CopilotError::transport` strips it before it can reach a `Display`
    /// (and therefore a log line) anywhere downstream.
    #[tokio::test]
    async fn transport_error_display_never_leaks_the_request_url() {
        // A connection refused on a loopback port carries the request URL
        // on the `reqwest::Error` by default -- this stands in for the real
        // signed download URL, which carries a token in its query string.
        let secret_looking_url = "http://127.0.0.1:1/download?token=super-secret-signed-value";
        let reqwest_err = reqwest::Client::new()
            .get(secret_looking_url)
            .send()
            .await
            .expect_err("connecting to port 1 must fail");
        assert!(
            reqwest_err.url().is_some(),
            "test precondition: reqwest must attach a url to this error, or this test is not \
             exercising the redaction at all"
        );

        let err = crate::CopilotError::transport(reqwest_err);
        let rendered = format!("{err}");
        assert!(
            !rendered.contains("super-secret-signed-value") && !rendered.contains("127.0.0.1:1"),
            "transport error Display must never include the request url: {rendered:?}"
        );
    }
}
