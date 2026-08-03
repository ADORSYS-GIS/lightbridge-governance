//! Typed errors for the governance domain (`thiserror` at the library edge;
//! binaries use `anyhow`).

/// Errors raised by the governance domain.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The requested registry entity does not exist.
    #[error("not found: {0}")]
    NotFound(String),

    /// The caller is authenticated but lacks the required permission.
    #[error("forbidden: {0}")]
    Forbidden(String),

    /// A caller-supplied value failed validation before any persistence
    /// happened. Distinct from `Forbidden`: this is "malformed input", not
    /// "you may not do this".
    #[error("invalid input: {0}")]
    Validation(String),

    /// A persistence or transport operation failed inside cratestack.
    ///
    /// Deliberately opaque: the inner error can carry query text and parameter
    /// values, which for this service can include a credential hash. Log it at
    /// the boundary with the fields you actually want; never let `Display`
    /// surface it to a caller.
    #[error("storage error")]
    Storage(#[from] cratestack_core::CoolError),
}

/// Convenience alias for fallible governance operations.
pub type Result<T> = std::result::Result<T, Error>;
