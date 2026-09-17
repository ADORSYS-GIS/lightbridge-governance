//! Deadlock retry policy for the ingest transaction.
//!
//! The whole batch runs in one transaction; a deadlock (SQLSTATE 40P01) is
//! transient, so the caller retries with exponential backoff a bounded number
//! of times before surfacing the error.

pub(crate) const MAX_DEADLOCK_RETRIES: u32 = 3;
pub(crate) const DEADLOCK_RETRY_BASE_MS: u64 = 10;

pub(crate) fn is_deadlock(e: &cratestack_core::CratestackError) -> bool {
    matches!(e, cratestack_core::CratestackError::DatabaseTyped(info) if info.sqlstate.as_deref() == Some("40P01"))
}
