//! Errors from the redaction path.
//!
//! Typed rather than `anyhow`, because callers must distinguish "this text is
//! dirty" from "the detector broke". On a `fail_closed` profile both reject the
//! request, but only the second is an incident.

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// A failure in the redaction path.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A first-party credential recognizer failed to compile at startup.
    ///
    /// Fatal by design: the service must not run with a silently-missing
    /// credential pattern.
    #[error("credential recognizers failed to build: expected {expected}, built {built}")]
    RecognizerBuild {
        /// How many patterns the pack defines.
        expected: usize,
        /// How many actually compiled.
        built: usize,
    },

    /// Detection failed.
    #[error("detection failed: {0}")]
    Analyze(String),

    /// The transform stage failed after detection succeeded.
    #[error("anonymization failed: {0}")]
    Anonymize(String),

    /// A caller asked for a profile that does not exist.
    ///
    /// Never resolved by falling back to a default — see
    /// [`crate::profile::Profile::by_name`].
    #[error("unknown redaction profile: {0}")]
    UnknownProfile(String),
}
