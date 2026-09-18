//! The `copilot-sync` run path: read config from env, then call the connector
//! pipeline for the requested days. `sync` (backfill) always re-fetches a
//! trailing lookback window (default the last 3 days, RFC-0001) and also
//! fills any gap after the high-water mark, bounded so a cold start cannot
//! walk back forever; `sync-day` ingests one explicit day. `replay`/
//! `verify`/`status` are the archive-facing operators (S3 phase of #12).
//!
//! Split out of a single 1178-line `sync.rs` (#178) into focused submodules:
//! `config` (env parsing), `backfill` (window + run orchestration), and
//! `operators` (sync-day/replay/verify/status). The public surface below is
//! what `main.rs` and `metrics.rs` consume; it is unchanged by the split.

mod backfill;
mod config;
mod cutover;
mod operators;
#[cfg(test)]
mod operators_tests;
#[cfg(test)]
mod test_util;

pub use backfill::run_backfill;
pub use config::Config;
pub use cutover::{decommission, export_counts, verify_archive_counts};
pub use operators::{SyncStatus, run_replay, run_status, run_sync_day, run_verify};
