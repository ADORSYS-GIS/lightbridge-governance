//! Shared test helpers for the `sync` submodules (`backfill` and `operators`).
//!
//! These were extracted from the single `sync.rs` test module when the file was
//! split (#178). They are compiled only into test binaries (`#[cfg(test)]`),
//! so they are unreachable from any production build (AGENTS.md: "mocks must
//! be unreachable from a production path").

#![cfg(test)]

use governance_copilot::RawSecret;

use super::config::Config;
use crate::{archive::Archive, test_support::TEST_APP_PRIVATE_KEY_PEM};

/// Parse a `YYYY-MM-DD` string into a `NaiveDate` for the pure window tests.
pub fn date(s: &str) -> chrono::NaiveDate {
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
}

/// `DATABASE_URL`-gated, matching `crates/governance-copilot/tests/
/// store.rs`'s convention: skip (with an explicit message, not a silent
/// no-op) when no database is configured, otherwise migrate and hand
/// back a live pool. `run_backfill_at`/`run_status` had NO test coverage
/// before this fix -- exactly where BLOCKERs 1-3 lived -- so these tests
/// exercise the real async functions against real Postgres (and, for
/// backfill, a real HTTP round trip to a local mock GitHub), not just
/// the pure helpers extracted above.
///
/// No `#[expect(clippy::expect_used)]` here, unlike the equivalent
/// helper in `governance-copilot`'s `tests/store.rs`: this function
/// lives inside a `#[cfg(test)]` module, which clippy.toml's
/// `allow-expect-in-tests` already covers (see that file's comment) --
/// adding the attribute here would be an unfulfilled expectation, not a
/// necessary one.
pub async fn db_pool() -> Option<cratestack::sqlx::PgPool> {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(v) => v,
        Err(_) => {
            eprintln!("skipping: DATABASE_URL not set (governance-ctl sync integration)");
            return None;
        }
    };
    let pool = cratestack::sqlx::PgPool::connect(&database_url)
        .await
        .expect("connect");
    governance_core::migrate::run(&pool).await.expect("migrate");
    Some(pool)
}

pub fn tmp_archive_dir(label: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("lb-ctl-sync-test-{label}-{}", std::process::id()))
}

/// `lookback_days: 1, max_backfill_days: 1` collapses `backfill_window`
/// (with no high-water mark) to exactly `[today - 1, today - 1]` -- the
/// single most-recent day GitHub can actually answer for (D-1, never
/// D-0/"today" -- same-day latency, RFC-0001 §Scheduling) -- so these
/// tests stay to a single round trip to the mock server per report type
/// instead of a full multi-day backfill. `0` would collapse the window
/// to `[today, today]` instead, which is no longer a valid window: `end`
/// is always `today - 1`, so a `0`-sized lookback/max-backfill would put
/// `start` (`today`) after `end` (`today - 1`).
pub fn test_config(tenant_id: String, org: String, archive_dir: std::path::PathBuf) -> Config {
    Config {
        tenant_id,
        org,
        app_id: "123456".to_owned(),
        private_key: RawSecret::new(TEST_APP_PRIVATE_KEY_PEM.to_owned()),
        archive: Archive::Local { dir: archive_dir },
        lookback_days: 1,
        max_backfill_days: 1,
    }
}
