//! The `copilot-sync` run path: read config from env, then call the connector
//! pipeline for the requested days. `sync` (backfill) always re-fetches a
//! trailing lookback window (default the last 3 days, RFC-0001) and also
//! fills any gap after the high-water mark, bounded so a cold start cannot
//! walk back forever; `sync-day` ingests one explicit day. `replay`/
//! `verify`/`status` are the archive-facing operators (S3 phase of #12).

use anyhow::{Context, Result};
use governance_copilot::{
    AppAuth, CopilotError, GithubClient, RawSecret, high_water_mark, manifest_schema_version,
    replay_report, sync_day, unmapped_user_count, verify_manifests,
};
use tracing::{info, warn};

use crate::archive::Archive;

/// RFC-0001: "Each run re-fetches D-1, D-2 and D-3 so a late-published
/// report is picked up with no operator action."
pub const DEFAULT_LOOKBACK_DAYS: i64 = 3;
/// How far a cold start (or a high-water mark stuck at `None`) is allowed to
/// walk back in one run. Bounds the request volume of a first-ever sync.
pub const DEFAULT_MAX_BACKFILL_DAYS: i64 = 28;

/// Connector configuration, read from the environment.
#[derive(Debug, Clone)]
pub struct Config {
    pub tenant_id: String,
    pub org: String,
    pub app_id: String,
    pub private_key: RawSecret,
    /// Where raw report NDJSON is archived (S3 in production, RAW_DIR locally).
    /// Required: archiving is a stated AC of #12, not an optional extra.
    pub archive: Archive,
    /// Trailing days always re-fetched on every `sync`, regardless of the
    /// high-water mark (`COPILOT_LOOKBACK_DAYS`, default
    /// [`DEFAULT_LOOKBACK_DAYS`]).
    pub lookback_days: i64,
    /// How far back a `sync` is allowed to walk when the high-water mark is
    /// stale or absent (`COPILOT_MAX_BACKFILL_DAYS`, default
    /// [`DEFAULT_MAX_BACKFILL_DAYS`]).
    pub max_backfill_days: i64,
}

impl Config {
    /// Build from env. The private key is read from `GH_APP_PRIVATE_KEY_FILE`
    /// (a PEM path), never from an env value, and wrapped in `RawSecret` so it
    /// cannot be logged (never-log-a-secret rule).
    pub async fn from_env() -> Result<Self> {
        let tenant_id = std::env::var("TENANT_ID")
            .context("TENANT_ID is required (single-tenant deployment, ADR-0001)")?;
        let org = std::env::var("GH_ORG").unwrap_or_else(|_| "adorsys-gis".to_owned());
        let app_id = std::env::var("GH_APP_ID").context("GH_APP_ID is required")?;
        let key_path = std::env::var("GH_APP_PRIVATE_KEY_FILE")
            .context("GH_APP_PRIVATE_KEY_FILE is required (path to the App PEM)")?;
        let pem =
            std::fs::read_to_string(&key_path).with_context(|| format!("reading {key_path}"))?;
        let archive = Archive::from_env().await?.context(
            "no archive sink configured: set AWS_ACCESS_KEY_ID + \
                 AWS_SECRET_ACCESS_KEY (S3) or RAW_DIR (local); a run that \
                 archives nothing is a silent failure (#12)",
        )?;
        Ok(Self {
            tenant_id,
            org,
            app_id,
            private_key: RawSecret::new(pem),
            archive,
            // Both windows are read leniently -- an operator typo here must
            // never crash the CronJob at startup (AGENTS.md: a required arg
            // with no default is how the API server got a CrashLoopBackOff).
            lookback_days: positive_env_i64("COPILOT_LOOKBACK_DAYS", DEFAULT_LOOKBACK_DAYS),
            max_backfill_days: positive_env_i64(
                "COPILOT_MAX_BACKFILL_DAYS",
                DEFAULT_MAX_BACKFILL_DAYS,
            ),
        })
    }
}

/// Read a positive `i64` from `key`, falling back to `default` -- and
/// warning, never erroring -- when the var is absent, not a number, or not
/// positive. Used only for the backfill window sizes: a malformed value here
/// must degrade to the safe default, not crash the CronJob at startup.
fn positive_env_i64(key: &str, default: i64) -> i64 {
    match std::env::var(key) {
        Err(_) => default,
        Ok(v) => match v.parse::<i64>() {
            Ok(n) if n > 0 => n,
            _ => {
                warn!(
                    key,
                    value = v.as_str(),
                    default,
                    "invalid or non-positive; using default"
                );
                default
            }
        },
    }
}

/// Ingest a single day, archiving raw NDJSON through the configured sink.
async fn ingest_day(
    client: &GithubClient,
    pool: &cratestack::sqlx::PgPool,
    cfg: &Config,
    day: &str,
) -> Result<Vec<governance_copilot::ReportOutcome>> {
    let auth = AppAuth::new(cfg.app_id.clone(), cfg.private_key.clone(), client);
    let archive = cfg.archive.clone();
    let archive_fn = async |key: &str, bytes: &[u8]| {
        archive
            .write(key, bytes)
            .await
            .map_err(|e| CopilotError::Archive(format!("{e:#}")))
    };

    let outcomes = sync_day(
        client,
        pool,
        &auth,
        &cfg.tenant_id,
        &cfg.org,
        day,
        archive_fn,
    )
    .await?;
    for o in &outcomes {
        let host = o.host.clone().unwrap_or_else(|| "-".to_owned());
        info!(
            report = o.report,
            day = o.day,
            status = o.status,
            count = o.record_count,
            host = host,
            "ingested report"
        );
    }
    Ok(outcomes)
}

/// The `[start, end]` (inclusive) window of report days a `sync` run should
/// (re-)ingest. Three RFC-0001 requirements, one expression:
///
/// - **Always re-fetch the trailing `lookback_days`**, regardless of where
///   the high-water mark sits. This is what makes a late-published report
///   self-heal with no operator action, and it is what stops the
///   high-water mark from permanently orphaning a day: a manifest row for a
///   *later* day (even an "empty" 204 one) can legitimately push the
///   high-water mark past an earlier day that never actually finished, but
///   that earlier day is still re-fetched here as long as it is within the
///   trailing window.
/// - **Still close any gap after the high-water mark** -- cold start, or
///   catching up after an outage that left it stale for a while.
/// - **Never walk back further than `max_backfill_days`**, so a cold start
///   (or a watermark that never advanced) cannot stampede the API.
///
/// `start = max(min(hwm + 1, today - lookback_days), today - max_backfill_days)`,
/// `end = today`. See the `backfill_window_*` tests below for the three
/// cases this is required to get right (recent/stale/absent high-water
/// mark) plus the exact re-orphaning scenario from the review.
pub fn backfill_window(
    hwm: Option<chrono::NaiveDate>,
    today: chrono::NaiveDate,
    lookback_days: i64,
    max_backfill_days: i64,
) -> (chrono::NaiveDate, chrono::NaiveDate) {
    let lookback_bound = today - chrono::Days::new(lookback_days.max(0) as u64);
    let max_backfill_bound = today - chrono::Days::new(max_backfill_days.max(0) as u64);
    let start = match hwm {
        Some(h) => {
            let resume = (h + chrono::Days::new(1)).min(today);
            resume.min(lookback_bound).max(max_backfill_bound)
        }
        // No manifest row exists at all: treat as a cold start, bounded by
        // max_backfill_days rather than only the (much shorter) trailing
        // lookback window.
        None => max_backfill_bound,
    };
    (start, today)
}

/// The outcome of one `sync` (backfill) run.
#[derive(Debug, Clone)]
pub struct BackfillOutcome {
    /// Per-report outcomes for every day that ingested without error.
    pub outcomes: Vec<governance_copilot::ReportOutcome>,
    /// Days in the window that ingested without error. An "empty" (204)
    /// report still counts as covered -- only a transport/auth/parse/storage
    /// error for the day counts as failed.
    pub covered: usize,
    /// Days in the window that errored: the day plus the error's rendered
    /// message (not the `anyhow::Error` itself -- `main` only needs to log
    /// and count these, and error types here are not `Clone`).
    pub failed: Vec<(String, String)>,
    /// Total days the computed window covered. Lets the caller distinguish
    /// "nothing to do" (window empty; fine) from "everything in a non-empty
    /// window failed" (must exit non-zero -- see `main`'s `Command::Sync`).
    pub window_days: usize,
}

impl BackfillOutcome {
    /// Whether a `Command::Sync` run should exit non-zero (BLOCKER 1): the
    /// window was non-empty and every day in it failed -- a totally broken
    /// run (dead credential, GitHub unreachable) that the CronJob's
    /// `backoffLimit`/alerting must see, not a process that quietly exits 0.
    ///
    /// Returns `Ok(())` for the two exit-0 cases: nothing to do (empty
    /// window), or a partial failure -- logged loudly by `run_backfill_at`
    /// already, and re-attempted by the next run's trailing window
    /// (BLOCKER 2), so failing the whole job here would only be noise.
    pub fn exit_result(&self) -> Result<()> {
        if self.window_days > 0 && self.covered == 0 {
            anyhow::bail!(
                "backfill covered 0 of {} day(s) in the window; every day failed \
                 (first failure: {:?})",
                self.window_days,
                self.failed.first()
            );
        }
        Ok(())
    }
}

/// Backfill: ingest the trailing lookback window plus any gap after the
/// high-water mark (see `backfill_window`), newest first so a late report
/// lands quickly. Does not decide the process exit code -- see `main`'s
/// `Command::Sync`, which uses `window_days`/`covered`/`failed` to do that.
pub async fn run_backfill(
    client: &GithubClient,
    pool: &cratestack::sqlx::PgPool,
    cfg: &Config,
) -> Result<BackfillOutcome> {
    let today = chrono::Utc::now().date_naive();
    run_backfill_at(client, pool, cfg, today).await
}

/// As `run_backfill`, but with `today` injected so tests can fix "now"
/// instead of racing the real clock.
pub async fn run_backfill_at(
    client: &GithubClient,
    pool: &cratestack::sqlx::PgPool,
    cfg: &Config,
    today: chrono::NaiveDate,
) -> Result<BackfillOutcome> {
    let hwm = high_water_mark(pool, &cfg.tenant_id, "github_copilot").await?;
    let (start, end) = backfill_window(hwm, today, cfg.lookback_days, cfg.max_backfill_days);

    let mut days: Vec<chrono::NaiveDate> = Vec::new();
    let mut d = start;
    while d <= end {
        days.push(d);
        d = d + chrono::Days::new(1);
    }
    days.reverse(); // newest first so a late report lands quickly
    let window_days = days.len();

    info!(start = %start, end = %end, n = window_days, "backfill window");
    let mut all = Vec::new();
    let mut covered = 0usize;
    let mut failed = Vec::new();
    for day in days {
        let ds = day.format("%Y-%m-%d").to_string();
        match ingest_day(client, pool, cfg, &ds).await {
            Ok(outcomes) => {
                covered += 1;
                all.extend(outcomes);
            }
            Err(e) => {
                warn!(day = ds, error = %e, "day failed; continuing backfill");
                failed.push((ds, format!("{e:#}")));
            }
        }
    }
    Ok(BackfillOutcome {
        outcomes: all,
        covered,
        failed,
        window_days,
    })
}

/// Ingest one explicit day. Idempotent.
pub async fn run_sync_day(
    client: &GithubClient,
    pool: &cratestack::sqlx::PgPool,
    cfg: &Config,
    day: &str,
) -> Result<Vec<governance_copilot::ReportOutcome>> {
    // Validate the day string early so a typo fails before any network call.
    chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d")
        .with_context(|| format!("invalid day {day:?}, want YYYY-MM-DD"))?;
    ingest_day(client, pool, cfg, day).await
}

/// Replay a day range from the raw archive, without calling GitHub at all.
///
/// This is the recovery path for a parse/upsert bug (RFC-0001): the archive
/// holds the exact bytes a re-fetch would return, so the replay exercises the
/// same `replay_report` code path as live ingestion.
pub async fn run_replay(
    pool: &cratestack::sqlx::PgPool,
    cfg: &Config,
    from: &str,
    to: &str,
) -> Result<()> {
    let from = chrono::NaiveDate::parse_from_str(from, "%Y-%m-%d")
        .with_context(|| format!("invalid day {from:?}, want YYYY-MM-DD"))?;
    let to = chrono::NaiveDate::parse_from_str(to, "%Y-%m-%d")
        .with_context(|| format!("invalid day {to:?}, want YYYY-MM-DD"))?;
    if to < from {
        anyhow::bail!("replay range is inverted: {from} > {to}");
    }

    let mut day = from;
    while day <= to {
        let ds = day.format("%Y-%m-%d").to_string();
        let keys = cfg.archive.list_day(&cfg.org, &ds).await?;
        if keys.is_empty() {
            info!(day = ds, "no archived reports for day; nothing to replay");
        }
        for key in keys {
            let report = key
                .rsplit('/')
                .next()
                .and_then(|f| f.strip_suffix(".ndjson"))
                .unwrap_or(&key)
                .to_owned();
            let bytes = cfg.archive.read(&key).await?;
            // A schema bump invalidates old archives; surface it rather than
            // silently replaying into the new shape (SCHEMA_VERSION).
            if let Some(version) = manifest_schema_version(
                pool,
                &cfg.tenant_id,
                "github_copilot",
                &cfg.org,
                &report,
                &ds,
            )
            .await?
                && version < governance_copilot::SCHEMA_VERSION
            {
                warn!(
                    report = report,
                    day = ds,
                    archived_schema = version,
                    current_schema = governance_copilot::SCHEMA_VERSION,
                    "replaying archive written under an older schema"
                );
            }
            let n = replay_report(pool, &cfg.tenant_id, &cfg.org, &ds, &report, &bytes).await?;
            info!(report = report, day = ds, count = n, "replayed report");
        }
        day = day + chrono::Days::new(1);
    }
    Ok(())
}

/// Reconcile stored row counts against the manifests and report drift.
/// Returns the number of mismatching manifest rows.
pub async fn run_verify(pool: &cratestack::sqlx::PgPool, cfg: &Config) -> Result<usize> {
    let drift = verify_manifests(pool, &cfg.tenant_id, "github_copilot", &cfg.org).await?;
    for d in &drift {
        warn!(
            day = d.day,
            report = d.report,
            status = d.status,
            expected = d.expected,
            actual = d.actual,
            "manifest/stored row-count drift"
        );
    }
    info!(mismatch = drift.len(), "verification complete");
    Ok(drift.len())
}

/// Whether a deployment has ever completed a Copilot sync, and if so how
/// stale the most recent success is.
///
/// This used to be a `(-1, -1)` sentinel pair of ints, and `push_status_
/// metrics` folded `-1` into `0` via `.max(0)` -- making "never synced"
/// metrically identical to "synced 0 days ago". An age-based alert
/// ("report older than 72h") could then never fire for a deployment that
/// had never run at all (BLOCKER 3). An enum makes "unknown" a case the
/// caller must handle, not a magic number that can be silently clamped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncStatus {
    /// No `ingest_manifests` row exists for this tenant/provider at all.
    NeverSynced,
    /// At least one manifest row exists.
    Synced {
        /// `today - MAX(report_day)`, in days.
        age_days: i64,
        unmapped_users: i64,
    },
}

/// Connector status: last success, report age, unmapped users.
pub async fn run_status(pool: &cratestack::sqlx::PgPool, cfg: &Config) -> Result<SyncStatus> {
    let Some(last) = high_water_mark(pool, &cfg.tenant_id, "github_copilot").await? else {
        info!("no manifests yet; nothing has been ingested");
        return Ok(SyncStatus::NeverSynced);
    };
    let today = chrono::Utc::now().date_naive();
    let age_days = today.signed_duration_since(last).num_days();
    let unmapped_users =
        unmapped_user_count(pool, &cfg.tenant_id, &cfg.org, &last.to_string()).await?;
    info!(
        last = %last,
        age_days,
        unmapped_users,
        "connector status"
    );
    Ok(SyncStatus::Synced {
        age_days,
        unmapped_users,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        archive::Archive,
        test_support::{MockGithub, RouteBehavior, TEST_APP_PRIVATE_KEY_PEM},
    };

    fn date(s: &str) -> chrono::NaiveDate {
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
    /// lives inside a `#[cfg(test)] mod`, which clippy.toml's
    /// `allow-expect-in-tests` already covers (see that file's comment) --
    /// adding the attribute here would be an unfulfilled expectation, not a
    /// necessary one.
    async fn db_pool() -> Option<cratestack::sqlx::PgPool> {
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

    fn tmp_archive_dir(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("lb-ctl-sync-test-{label}-{}", std::process::id()))
    }

    /// `lookback_days: 0, max_backfill_days: 0` collapses `backfill_window`
    /// (with no high-water mark) to exactly `[today, today]` -- a
    /// one-day window keeps these tests to a single round trip to the mock
    /// server per report type instead of a full multi-day backfill.
    fn test_config(tenant_id: String, org: String, archive_dir: std::path::PathBuf) -> Config {
        Config {
            tenant_id,
            org,
            app_id: "123456".to_owned(),
            private_key: RawSecret::new(TEST_APP_PRIVATE_KEY_PEM.to_owned()),
            archive: Archive::Local { dir: archive_dir },
            lookback_days: 0,
            max_backfill_days: 0,
        }
    }

    /// `run_status` end to end: `NeverSynced` for a tenant with no
    /// manifests, `Synced` once one exists -- the exact distinction BLOCKER
    /// 3 required and that the `(-1, -1)` sentinel erased.
    #[tokio::test]
    async fn run_status_distinguishes_never_synced_from_synced() {
        let Some(pool) = db_pool().await else {
            return;
        };
        let tenant_id = format!("it-ctl-status-{}", std::process::id());
        let cfg = test_config(
            tenant_id.clone(),
            "it-org".to_owned(),
            tmp_archive_dir("status"),
        );

        let status = run_status(&pool, &cfg).await.unwrap();
        assert_eq!(status, SyncStatus::NeverSynced);

        let today = chrono::Utc::now().date_naive();
        governance_copilot::upsert_manifest(
            &pool,
            &tenant_id,
            "github_copilot",
            &cfg.org,
            "organization-1-day",
            &today.format("%Y-%m-%d").to_string(),
            "ok",
            1,
        )
        .await
        .unwrap();

        let status = run_status(&pool, &cfg).await.unwrap();
        assert_eq!(
            status,
            SyncStatus::Synced {
                age_days: 0,
                unmapped_users: 0
            }
        );
    }

    /// `run_backfill_at` end to end against a mock GitHub that always
    /// succeeds: the day is covered, the window matches `backfill_window`,
    /// and (via `run_status`) the high-water mark actually advanced in
    /// Postgres.
    #[tokio::test]
    async fn run_backfill_at_covers_a_successful_day_and_advances_the_high_water_mark() {
        let Some(pool) = db_pool().await else {
            return;
        };
        let tenant_id = format!("it-ctl-backfill-ok-{}", std::process::id());
        let org = "it-org-ok";
        let mock = MockGithub::start(RouteBehavior::AlwaysSucceeds, org)
            .await
            .unwrap();
        let client = GithubClient::with_api_base(reqwest::Client::new(), mock.base_url.clone());
        let cfg = test_config(
            tenant_id.clone(),
            org.to_owned(),
            tmp_archive_dir("backfill-ok"),
        );
        let today = chrono::Utc::now().date_naive();

        let result = run_backfill_at(&client, &pool, &cfg, today).await.unwrap();

        assert_eq!(
            result.window_days, 1,
            "lookback=1, max_backfill=1 => just today"
        );
        assert_eq!(result.covered, 1);
        assert!(result.failed.is_empty());
        assert!(result.exit_result().is_ok());

        let status = run_status(&pool, &cfg).await.unwrap();
        assert_eq!(
            status,
            SyncStatus::Synced {
                age_days: 0,
                unmapped_users: 0
            }
        );
    }

    /// `run_backfill_at` end to end against a mock GitHub that always
    /// fails: the day is NOT covered, no manifest row is written (the
    /// high-water mark stays absent), and `exit_result` signals the
    /// non-zero exit BLOCKER 1 requires.
    #[tokio::test]
    async fn run_backfill_at_reports_a_totally_failed_day_and_does_not_advance_the_high_water_mark()
    {
        let Some(pool) = db_pool().await else {
            return;
        };
        let tenant_id = format!("it-ctl-backfill-fail-{}", std::process::id());
        let org = "it-org-fail";
        let mock = MockGithub::start(RouteBehavior::AlwaysFails(500), org)
            .await
            .unwrap();
        let client = GithubClient::with_api_base(reqwest::Client::new(), mock.base_url.clone());
        let cfg = test_config(
            tenant_id.clone(),
            org.to_owned(),
            tmp_archive_dir("backfill-fail"),
        );
        let today = chrono::Utc::now().date_naive();

        let result = run_backfill_at(&client, &pool, &cfg, today).await.unwrap();

        assert_eq!(result.window_days, 1);
        assert_eq!(result.covered, 0);
        assert_eq!(result.failed.len(), 1);
        assert!(
            result.exit_result().is_err(),
            "a non-empty window where every day failed must signal a non-zero exit (BLOCKER 1)"
        );

        let status = run_status(&pool, &cfg).await.unwrap();
        assert_eq!(
            status,
            SyncStatus::NeverSynced,
            "a fully failed day must not write a manifest row that would advance the \
             high-water mark"
        );
    }

    /// A recent high-water mark (yesterday) must not shrink the window down
    /// to "just the gap" -- the trailing lookback always applies on top of
    /// it, per RFC-0001 ("each run re-fetches D-1, D-2 and D-3").
    #[test]
    fn backfill_window_with_recent_hwm_still_covers_the_trailing_lookback() {
        let today = date("2026-08-10");
        let hwm = Some(date("2026-08-09")); // yesterday
        let (start, end) = backfill_window(hwm, today, 3, 28);
        assert_eq!(start, date("2026-08-07")); // today - 3
        assert_eq!(end, today);
    }

    /// A stale high-water mark (older than max_backfill_days) must be
    /// bounded, not walked back forever.
    #[test]
    fn backfill_window_with_stale_hwm_is_bounded_by_max_backfill_days() {
        let today = date("2026-08-10");
        let hwm = Some(date("2026-07-01")); // 40 days ago
        let (start, end) = backfill_window(hwm, today, 3, 28);
        assert_eq!(start, date("2026-07-13")); // today - 28
        assert_eq!(end, today);
    }

    /// No high-water mark at all (first-ever run) is a cold start: bounded
    /// by max_backfill_days, not just the trailing lookback.
    #[test]
    fn backfill_window_with_no_hwm_is_a_cold_start_bounded_by_max_backfill_days() {
        let today = date("2026-08-10");
        let (start, end) = backfill_window(None, today, 3, 28);
        assert_eq!(start, date("2026-07-13")); // today - 28
        assert_eq!(end, today);
    }

    /// A moderately stale high-water mark (inside max_backfill_days but
    /// outside the trailing lookback) must still close the whole gap, not
    /// just the trailing window.
    #[test]
    fn backfill_window_fills_a_gap_wider_than_the_trailing_lookback() {
        let today = date("2026-08-10");
        let hwm = Some(date("2026-07-31")); // 10 days ago
        let (start, end) = backfill_window(hwm, today, 3, 28);
        assert_eq!(start, date("2026-08-01")); // hwm + 1, not just today - 3
        assert_eq!(end, today);
    }

    /// The exact review scenario (BLOCKER 2): a day D fails (or gets an
    /// "empty" 204 that later turns out to have been premature), a LATER
    /// day still gets a manifest row and pushes the high-water mark past D.
    /// Under the old "walk forward from hwm+1" logic, D was never
    /// re-attempted again. Under the trailing-window fix, D is still
    /// in-window as long as it is within `lookback_days` of "today" --
    /// regardless of where the high-water mark now sits.
    #[test]
    fn backfill_window_still_covers_a_day_the_hwm_has_already_advanced_past() {
        let today = date("2026-08-10");
        let failed_day = date("2026-08-09"); // "D": failed/empty on a prior run
        // A later day (today itself) already has a manifest row, so the
        // high-water mark is now AFTER `failed_day`.
        let hwm = Some(today);
        let (start, end) = backfill_window(hwm, today, 3, 28);
        assert!(
            start <= failed_day && failed_day <= end,
            "expected the window [{start}, {end}] to still include {failed_day}"
        );
    }

    /// `run_status` must read "never synced" as a distinct case, not "0 days
    /// old" -- proved directly against the type rather than a metrics push,
    /// which `metrics.rs`'s own tests cover.
    #[test]
    fn sync_status_never_synced_is_not_synced_zero_days_ago() {
        assert_ne!(
            SyncStatus::NeverSynced,
            SyncStatus::Synced {
                age_days: 0,
                unmapped_users: 0
            }
        );
    }

    fn outcome(covered: usize, window_days: usize, failed: &[&str]) -> BackfillOutcome {
        BackfillOutcome {
            outcomes: Vec::new(),
            covered,
            failed: failed
                .iter()
                .map(|d| (d.to_string(), "boom".to_owned()))
                .collect(),
            window_days,
        }
    }

    /// BLOCKER 1: a non-empty window where every day failed must produce an
    /// `Err`, so `main` propagates it and the process exits non-zero.
    #[test]
    fn exit_result_errors_when_every_day_in_a_nonempty_window_failed() {
        let result = outcome(
            0,
            4,
            &["2026-08-07", "2026-08-08", "2026-08-09", "2026-08-10"],
        );
        assert!(result.exit_result().is_err());
    }

    /// A partial failure (some days ok) must stay exit 0 -- it is logged
    /// loudly by `run_backfill_at` already, and the failed day is
    /// re-attempted by the next run's trailing window (BLOCKER 2), so
    /// failing the whole job here would only be noise.
    #[test]
    fn exit_result_is_ok_on_a_partial_failure() {
        let result = outcome(3, 4, &["2026-08-07"]);
        assert!(result.exit_result().is_ok());
    }

    /// An empty window (already caught up; nothing to do) must stay exit 0
    /// -- `covered == 0` here means "there was nothing to cover", not "every
    /// day failed".
    #[test]
    fn exit_result_is_ok_on_an_empty_window() {
        let result = outcome(0, 0, &[]);
        assert!(result.exit_result().is_ok());
    }

    /// A fully successful window must stay exit 0.
    #[test]
    fn exit_result_is_ok_when_every_day_succeeded() {
        let result = outcome(4, 4, &[]);
        assert!(result.exit_result().is_ok());
    }
}
