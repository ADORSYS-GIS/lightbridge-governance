//! Connector configuration, read from the environment.
//!
//! Split out of `sync.rs` (which had grown past the 200-LoC ratchet, #178) so
//! the run orchestration modules stay focused on the pipeline, not on env
//! parsing. Nothing here touches the network or the database.

use anyhow::{Context, Result};
use governance_copilot::RawSecret;
use tracing::warn;

use crate::archive::Archive;

/// RFC-0001: "Each run re-fetches D-1, D-2 and D-3 so a late-published
/// report is picked up with no operator action."
pub const DEFAULT_LOOKBACK_DAYS: i64 = 3;
/// How far a cold start (or a high-water mark stuck at `None`) is allowed to
/// walk back in one run. Bounds the request volume of a first-ever sync.
pub const DEFAULT_MAX_BACKFILL_DAYS: i64 = 28;

/// Days behind real calendar `today` before GitHub's Copilot metrics API will
/// answer for a day at all -- confirmed live: a request for day D-0 returns
/// `400 "Date must be within the last year and not in the future"`.
///
/// Deliberately NOT an env var like [`DEFAULT_LOOKBACK_DAYS`]/
/// [`DEFAULT_MAX_BACKFILL_DAYS`]. Those two encode a real per-deployment
/// policy choice (how much re-checking versus API call volume, how far a
/// cold start may walk back) and different operators could reasonably want
/// different values. This encodes a fact about GitHub's data pipeline --
/// every deployment talks to the same API with the same latency, so there is
/// no deployment where a value other than `1` is correct. An env var here
/// would only add a way to reintroduce the exact bug this constant fixes (set
/// it to `0` while debugging and the D-0 400s are back), for no compensating
/// flexibility. If GitHub's latency ever changes, that is a fact to update
/// here -- and in RFC-0001's own "D-1, D-2 and D-3" wording, so the spec and
/// the implementation can't drift apart -- not something to tune per
/// deployment.
pub(crate) const COPILOT_DATA_LAG_DAYS: u64 = 1;

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
    /// ADR-0014 cutover switch (`CUTOVER_FREEZE_WRITES`, default `false`).
    /// When `true`, the direct-Postgres write path is frozen: `sync`/`replay`
    /// emit day-grain facts and seat snapshots as OTLP log records through the
    /// usage ingest sink as the ONLY write path, and never write to the
    /// governance telemetry tables. Requires an OTLP sink
    /// (`OTEL_EXPORTER_OTLP_LOGS_ENDPOINT`, falling back to
    /// `OTEL_EXPORTER_OTLP_ENDPOINT`); a freeze with no sink fails loudly
    /// rather than silently writing nothing.
    pub freeze_writes: bool,
}

impl Config {
    /// Build from env. The private key is read from `GH_APP_PRIVATE_KEY_FILE`
    /// (a PEM path), never from an env value, and wrapped in `RawSecret` so it
    /// cannot be logged (never-log-a-secret rule).
    pub async fn from_env() -> Result<Self> {
        // `env::var` returns Ok("") for a variable that is SET BUT EMPTY, so
        // the `.context(...)` below only catches a genuinely absent variable.
        // The chart renders `TENANT_ID: ""` by default, which sailed straight
        // through this check -- the deployed collector ingested a week of real
        // Copilot data stamped `tenant_id = ''` before anyone noticed.
        //
        // An empty tenant is not a lenient default, it is a wrong identity:
        // every table carries tenant_id and every query filters on it
        // (ADR-0001), so rows written under '' are invisible to any correctly
        // scoped query and have to be re-ingested under the real tenant later.
        // Refuse it here rather than write data that looks fine and is
        // addressed to nobody.
        let tenant_id = std::env::var("TENANT_ID")
            .context("TENANT_ID is required (single-tenant deployment, ADR-0001)")?;
        let tenant_id = tenant_id.trim().to_owned();
        anyhow::ensure!(
            !tenant_id.is_empty(),
            "TENANT_ID is set but empty. It is this deployment's tenant identity \
             (ADR-0001) and lands in every row and every WHERE clause, so an empty \
             value silently writes data no scoped query can find. Set \
             `copilot.tenantId` in the deployed values (ai-helm-values) to a stable \
             identifier for this deployment -- changing it later re-backfills from \
             scratch, since the high-water mark is itself tenant-scoped."
        );
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
            freeze_writes: env_bool("CUTOVER_FREEZE_WRITES"),
        })
    }
}

/// Read a boolean env var, defaulting to `false` when absent or unparseable.
/// Used for the ADR-0014 cutover switch: a malformed value must degrade to the
/// safe (pre-cutover) default, never crash the CronJob at startup.
fn env_bool(key: &str) -> bool {
    match std::env::var(key) {
        Err(_) => false,
        Ok(v) => matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
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
