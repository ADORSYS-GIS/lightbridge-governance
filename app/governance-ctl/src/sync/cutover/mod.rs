//! The ADR-0014 cutover operators: `export-counts`, `verify-counts` and
//! `decommission` (lightbridge-authz#588 governance half).
//!
//! The cutover mandate is migrate, verify by counts, decommission. These three
//! operators are the governance-side machinery for that:
//!
//! - [`export_counts`] reads the expected per-`(day, report)` counts from
//!   `ingest_manifests` (plus per-table counts for the execution/model-call/
//!   tool-call telemetry) and emits them as JSON -- the export the authz-side
//!   `verify-counts` CLI consumes.
//! - [`verify_archive_counts`] is the governance-side no-loss bar: it parses
//!   the S3 raw archive back and compares each `(day, report)`'s record count
//!   against what `ingest_manifests` recorded. A mismatch means the archive is
//!   not trustworthy for replay and the cutover must block loudly.
//! - [`decommission`] drops the governance telemetry tables, gated on
//!   [`verify_archive_counts`] passing and an explicit `--confirm`. It never
//!   drops while counts are unverified -- no dormant tables, no silent loss.

mod decommission;
#[cfg(test)]
mod decommission_shared_tests;
#[cfg(test)]
mod decommission_tests;
mod export;
#[cfg(test)]
mod export_tests;
mod verify;
#[cfg(test)]
mod verify_seats_tests;
#[cfg(test)]
mod verify_tests;

use anyhow::Result;
pub use decommission::decommission;
pub use export::export_counts;
use serde::Serialize;
pub use verify::verify_archive_counts;

use super::config::Config;

/// The Copilot day-fact telemetry tables decommissioned at the cutover. These
/// are migrated by replaying the S3 archive through the day-grain ingest API,
/// and their no-loss bar is the governance-side [`verify_archive_counts`].
/// `ingest_manifests` is deliberately NOT in this list: it is the metadata
/// record the count assertions read from and is not telemetry.
///
/// `copilot_user_teams` is deliberately NOT in this list either: `user-teams-1-day`
/// is not cut over (the authz receiver refuses it, RFC-0001 known-issue #1), so
/// its rows have no usage-store equivalent and dropping the table would
/// permanently destroy data that was never migrated. It is left in place until
/// the natural-key fix lands and the report is cut over.
const COPILOT_DAY_TABLES: &[&str] = &[
    "copilot_org_dailys",
    "copilot_user_dailys",
    "copilot_repo_dailys",
    "copilot_seat_snapshots",
];

/// The shared normalized telemetry tables written by EVERY push connector
/// (Copilot, Foundry, redact), not just Copilot. Their no-loss bar is the
/// authz-side `verify-counts` CLI (which consumes `export-counts`), which this
/// binary cannot call -- so dropping them is gated on an explicit
/// `--include-shared-tables` flag, never a default (see `decommission`).
///
/// Children before parents: `model_calls`/`tool_calls` hold a foreign key to
/// `executions`, so they must be dropped first or Postgres refuses.
const SHARED_TABLES: &[&str] = &["model_calls", "tool_calls", "executions"];

/// One expected `(day, report)` count, as recorded in `ingest_manifests`.
#[derive(Debug, Clone, Serialize)]
pub struct ExpectedCount {
    pub day: String,
    pub report: String,
    pub expected: i64,
}

/// One per-day expected seat-snapshot count (`billing-seats`), as recorded in
/// `ingest_manifests`.
#[derive(Debug, Clone, Serialize)]
pub struct SeatExpectation {
    pub day: String,
    pub expected: i64,
}

/// One `(day, report)` whose archived record count disagrees with the manifest.
///
/// `actual` carries two sentinel values that are load-bearing for any consumer
/// that branches on it (the authz-side `verify-counts` CLI, a monitoring
/// script):
///
/// - `actual == 0` means the archive was **missing** (or parsed to zero rows):
///   the file could not be read at all, so nothing was verified.
/// - `actual == -1` means the archive was **present but unparseable**: the file
///   existed but could not be parsed back into rows.
///
/// A non-negative `actual` is a real parsed row count that disagreed with
/// `expected`. Consumers must not treat `-1` as a count.
#[derive(Debug, Clone)]
pub struct CountMismatch {
    pub day: String,
    pub report: String,
    pub expected: i64,
    pub actual: i64,
}

/// The full count export, matching the authz-side `verify-counts` CLI's
/// `VerifyManifest` shape (lightbridge-authz#751). This is the JSON the
/// authz-side count-assertion harness consumes to assert the usage store
/// matches before the governance telemetry tables are dropped.
///
/// - `day_facts` carries the three day-fact reports (`organization-1-day`,
///   `users-1-day`, `repos-1-day`) that land in `usage_day_facts`.
///   `user-teams-1-day` is deliberately excluded: the authz receiver refuses it
///   (RFC-0001 known-issue #1), so it is not present in the usage store and
///   asserting it would always mismatch.
/// - `seat_snapshots` carries `billing-seats` per-day counts, which land in
///   `usage_seat_snapshots`.
/// - `executions`/`model_calls`/`tool_calls` are the governance telemetry table
///   counts migrated table-to-table into the usage store.
#[derive(Debug, Clone, Serialize)]
pub struct CountExport {
    pub day_facts: Vec<ExpectedCount>,
    pub seat_snapshots: Vec<SeatExpectation>,
    pub executions: i64,
    pub model_calls: i64,
    pub tool_calls: i64,
}

/// Refuse the count-verification operators while the cutover freeze is on.
///
/// `export-counts`/`verify-counts`/`decommission` all read `ingest_manifests`
/// as the source of truth for the no-loss bar. Under `CUTOVER_FREEZE_WRITES`
/// the direct-Postgres write path (and therefore `ingest_manifests`) is
/// frozen, so those counts are stale or empty and verification would report a
/// false green -- the exact silent-loss failure the no-loss bar exists to
/// prevent. These operators are pre-freeze machinery; refuse loudly rather
/// than let a freeze run masquerade as a verified cutover.
fn refuse_during_freeze(cfg: &Config) -> Result<()> {
    anyhow::ensure!(
        !cfg.freeze_writes,
        "CUTOVER_FREEZE_WRITES is set: the direct-Postgres write path (and ingest_manifests) \
         is frozen, so count verification against ingest_manifests is meaningless and would \
         report a false green. Run export-counts/verify-counts/decommission BEFORE enabling \
         the freeze."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::test_util::{test_config, tmp_archive_dir};

    /// The count-verification operators must refuse while the cutover freeze is
    /// on: `ingest_manifests` is frozen (not written), so verification against
    /// it would report a false green. This is the F1 guard -- it fails before
    /// any database access, so it is testable without a pool.
    #[test]
    fn refuse_during_freeze_blocks_count_operators_when_freeze_is_on() {
        let mut cfg = test_config(
            "t".to_owned(),
            "o".to_owned(),
            tmp_archive_dir("freeze-guard"),
        );
        cfg.freeze_writes = true;
        let err = refuse_during_freeze(&cfg).unwrap_err();
        assert!(
            format!("{err:#}").contains("CUTOVER_FREEZE_WRITES"),
            "a freeze must refuse count verification: {err:#}"
        );

        cfg.freeze_writes = false;
        assert!(
            refuse_during_freeze(&cfg).is_ok(),
            "without a freeze the guard must pass"
        );
    }
}
