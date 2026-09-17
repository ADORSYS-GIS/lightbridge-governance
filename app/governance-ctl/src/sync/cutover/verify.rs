//! The `verify-counts` operator: the governance-side no-loss bar that parses
//! the S3 raw archive back and compares each `(day, report)`'s record count
//! against what `ingest_manifests` recorded.

use anyhow::{Context, Result};
use tracing::warn;

use super::{CountMismatch, refuse_during_freeze};
use crate::sync::config::Config;

/// The governance-side no-loss bar: verify the S3 raw archive is complete
/// against `ingest_manifests` by parsing each archived `(day, report)` back and
/// comparing its record count to what the manifest recorded.
///
/// Returns the list of mismatches (empty = the archive is trustworthy for
/// replay). A mismatch means the archive cannot be replayed through the
/// day-grain ingest API with asserted-equal counts, so the cutover must block.
pub async fn verify_archive_counts(
    pool: &cratestack::sqlx::PgPool,
    cfg: &Config,
) -> Result<Vec<CountMismatch>> {
    refuse_during_freeze(cfg)?;
    let manifests: Vec<(chrono::NaiveDate, String, i64)> = cratestack::sqlx::query_as(
        "SELECT report_day::date, report_type, record_count \
         FROM ingest_manifests \
         WHERE tenant_id = $1 AND provider = $2 AND scope_id = $3 \
         ORDER BY report_day ASC, report_type ASC",
    )
    .bind(&cfg.tenant_id)
    .bind("github_copilot")
    .bind(&cfg.org)
    .fetch_all(pool)
    .await
    .context("reading ingest_manifests for archive verification")?;

    let mut mismatches = Vec::new();
    for (day, report, expected) in manifests {
        let ds = day.to_string();

        // A zero-count manifest row is an empty day (GitHub HTTP 204): the
        // report had no rows, so `ingest_one` recorded a manifest but wrote no
        // archive. There is nothing to parse back, so there is nothing to
        // verify -- skipping avoids a spurious mismatch on every empty day.
        if expected == 0 {
            continue;
        }

        // `billing-seats` is archived as a single JSON document under
        // `seats_archive_key` (`.json`), not as NDJSON under `archive_key`
        // (`.ndjson`) like the four day reports. Read the right key or the
        // seats archive is always reported missing.
        let key = if report == governance_copilot::SEATS_REPORT_TYPE {
            governance_copilot::seats_archive_key(&cfg.org, &ds)
        } else {
            governance_copilot::archive_key(&cfg.org, &report, &ds)
        };
        let bytes = match cfg.archive.read(&key).await {
            Ok(b) => b,
            Err(e) => {
                warn!(day = ds, report = report, error = %e, "archived report missing");
                mismatches.push(CountMismatch {
                    day: ds,
                    report,
                    expected,
                    actual: 0,
                });
                continue;
            }
        };
        let actual = match governance_copilot::parse_report_rows(&report, &bytes, &ds) {
            Ok(rows) => rows.len() as i64,
            Err(e) => {
                warn!(day = ds, report = report, error = %e, "archived report unparseable");
                mismatches.push(CountMismatch {
                    day: ds,
                    report,
                    expected,
                    actual: -1,
                });
                continue;
            }
        };
        if actual != expected {
            mismatches.push(CountMismatch {
                day: ds,
                report,
                expected,
                actual,
            });
        }
    }
    Ok(mismatches)
}
