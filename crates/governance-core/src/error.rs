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

    /// A database operation failed.
    #[error("database error")]
    Database(#[from] sqlx::Error),
}

/// Convenience alias for fallible governance operations.
pub type Result<T> = std::result::Result<T, Error>;
