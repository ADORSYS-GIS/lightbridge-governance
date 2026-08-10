//! Idempotent persistence of Copilot report rows and ingest manifests.
//!
//! These are bulk writes from the collector, not user-facing CRUD, so they go
//! through raw `sqlx` `INSERT ... ON CONFLICT DO UPDATE`, keyed on the natural
//! keys we hand-added `UNIQUE INDEX`es for (cratestack#262). This matches the
//! sanctioned escape-hatch precedent in `governance-core/src/credential.rs`
//! and the `ingest_manifests` upsert test (ADR-0009): reprocessing a day
//! replaces rows in place rather than duplicating (RFC-0001 idempotency).
//!
//! `ingest_manifests` is also where ADR-0007 derives the connector's
//! operational metrics from, so every run writes one row per (day, report).

use cratestack::{sqlx, sqlx::PgPool};

use crate::{
    error::{CopilotError, Result},
    model::{OrgDaily, RepoDaily, SeatSnapshot, UserDaily, UserTeam},
};

/// Upsert one day's org-aggregate report rows.
pub async fn upsert_org_daily(pool: &PgPool, tenant_id: &str, rows: &[OrgDaily]) -> Result<usize> {
    let mut tx = pool.begin().await.map_err(CopilotError::Storage)?;
    for r in rows {
        let org = &r.organization_id;
        let day = &r.report_day;
        sqlx::query(
            "INSERT INTO copilot_org_dailys \
             (id, tenant_id, organization_id, report_day, active_users, engaged_users, \
              total_interactions, code_generations, code_acceptances, loc_suggested, \
              loc_added, loc_deleted, ai_credits, net_cost_micro_usd) \
             VALUES ($1, $2, $3, CAST($4 AS date), $5, $6, $7, $8, $9, $10, $11, $12, $13, $14) \
             ON CONFLICT (tenant_id, organization_id, report_day) DO UPDATE SET \
               active_users = EXCLUDED.active_users, \
               engaged_users = EXCLUDED.engaged_users, \
               total_interactions = EXCLUDED.total_interactions, \
               code_generations = EXCLUDED.code_generations, \
               code_acceptances = EXCLUDED.code_acceptances, \
               loc_suggested = EXCLUDED.loc_suggested, \
               loc_added = EXCLUDED.loc_added, \
               loc_deleted = EXCLUDED.loc_deleted, \
               ai_credits = EXCLUDED.ai_credits, \
               net_cost_micro_usd = EXCLUDED.net_cost_micro_usd",
        )
        .bind(row_id("org", tenant_id, org, day))
        .bind(tenant_id)
        .bind(org)
        .bind(day)
        .bind(r.active_users as i64)
        .bind(r.engaged_users as i64)
        .bind(r.total_interactions as i64)
        .bind(0i64) // code_generations aggregate not split into a stored column yet
        .bind(0i64) // code_acceptances
        .bind(0i64) // loc_suggested
        .bind(0i64) // loc_added
        .bind(0i64) // loc_deleted
        .bind(r.ai_credits as i64)
        .bind(r.net_cost_micro_usd.0)
        .execute(&mut *tx)
        .await
        .map_err(CopilotError::Storage)?;
    }
    let n = rows.len();
    tx.commit().await.map_err(CopilotError::Storage)?;
    Ok(n)
}

/// Upsert one day's user-level report rows for `organization_id`.
pub async fn upsert_user_daily(
    pool: &PgPool,
    tenant_id: &str,
    organization_id: &str,
    rows: &[UserDaily],
) -> Result<usize> {
    let mut tx = pool.begin().await.map_err(CopilotError::Storage)?;
    for r in rows {
        let day = &r.report_day;
        sqlx::query(
            "INSERT INTO copilot_user_dailys \
             (id, tenant_id, organization_id, report_day, provider_user_id, user_login, \
              total_interactions, code_generations, code_acceptances, ai_credits, \
              net_cost_micro_usd) \
             VALUES ($1, $2, $3, CAST($4 AS date), $5, $6, $7, $8, $9, $10, $11) \
             ON CONFLICT (tenant_id, organization_id, report_day, provider_user_id) \
             DO UPDATE SET \
               user_login = EXCLUDED.user_login, \
               total_interactions = EXCLUDED.total_interactions, \
               code_generations = EXCLUDED.code_generations, \
               code_acceptances = EXCLUDED.code_acceptances, \
               ai_credits = EXCLUDED.ai_credits, \
               net_cost_micro_usd = EXCLUDED.net_cost_micro_usd",
        )
        .bind(row_id("user", tenant_id, &r.provider_user_id, day))
        .bind(tenant_id)
        .bind(organization_id)
        .bind(day)
        .bind(&r.provider_user_id)
        .bind(&r.user_login)
        .bind(r.total_interactions as i64)
        .bind(r.total_completions as i64)
        .bind(0i64) // code_acceptances
        .bind(r.ai_credits as i64)
        .bind(r.net_cost_micro_usd.0)
        .execute(&mut *tx)
        .await
        .map_err(CopilotError::Storage)?;
    }
    let n = rows.len();
    tx.commit().await.map_err(CopilotError::Storage)?;
    Ok(n)
}

/// Upsert one day's per-repo report rows for `organization_id`.
pub async fn upsert_repo_daily(
    pool: &PgPool,
    tenant_id: &str,
    organization_id: &str,
    rows: &[RepoDaily],
) -> Result<usize> {
    let mut tx = pool.begin().await.map_err(CopilotError::Storage)?;
    for r in rows {
        let day = &r.report_day;
        sqlx::query(
            "INSERT INTO copilot_repo_dailys \
             (id, tenant_id, organization_id, report_day, repository_id, coding_agent_activity, \
              code_review_activity, pull_request_activity) \
             VALUES ($1, $2, $3, CAST($4 AS date), $5, $6, $7, $8) \
             ON CONFLICT (tenant_id, organization_id, report_day, repository_id) DO UPDATE SET \
               coding_agent_activity = EXCLUDED.coding_agent_activity, \
               code_review_activity = EXCLUDED.code_review_activity, \
               pull_request_activity = EXCLUDED.pull_request_activity",
        )
        .bind(row_id("repo", tenant_id, &r.repository_id, day))
        .bind(tenant_id)
        .bind(organization_id)
        .bind(day)
        .bind(&r.repository_id)
        .bind(r.coding_agent_activity as i64)
        .bind(r.code_review_activity as i64)
        .bind(r.pull_request_activity as i64)
        .execute(&mut *tx)
        .await
        .map_err(CopilotError::Storage)?;
    }
    let n = rows.len();
    tx.commit().await.map_err(CopilotError::Storage)?;
    Ok(n)
}

/// Upsert user->team membership rows for `organization_id`.
pub async fn upsert_user_team(
    pool: &PgPool,
    tenant_id: &str,
    organization_id: &str,
    rows: &[UserTeam],
) -> Result<usize> {
    let mut tx = pool.begin().await.map_err(CopilotError::Storage)?;
    for r in rows {
        let day = &r.report_day;
        sqlx::query(
            "INSERT INTO copilot_user_teams \
             (id, tenant_id, organization_id, report_day, user_id, team_id, team_slug) \
             VALUES ($1, $2, $3, CAST($4 AS date), $5, $6, $7) \
             ON CONFLICT (tenant_id, organization_id, report_day, user_id, team_id) \
             DO UPDATE SET team_slug = EXCLUDED.team_slug",
        )
        .bind(row_id("team", tenant_id, &r.user_id, day))
        .bind(tenant_id)
        .bind(organization_id)
        .bind(day)
        .bind(&r.user_id)
        .bind(&r.team_id)
        .bind(&r.team_slug)
        .execute(&mut *tx)
        .await
        .map_err(CopilotError::Storage)?;
    }
    let n = rows.len();
    tx.commit().await.map_err(CopilotError::Storage)?;
    Ok(n)
}

/// Upsert one snapshot's seat rows for `organization_id`. Unlike the daily
/// reports, `seat_assigned_at`/`last_activity_at` bind as real
/// `TIMESTAMPTZ` values (see `SeatSnapshot`'s doc comment), not a `CAST`'d
/// date string -- `sqlx`'s `chrono` support round-trips `Option<DateTime<
/// Utc>>` directly, including `NULL` for "unknown", never a fabricated
/// zero time.
pub async fn upsert_seat_snapshot(
    pool: &PgPool,
    tenant_id: &str,
    organization_id: &str,
    rows: &[SeatSnapshot],
) -> Result<usize> {
    let mut tx = pool.begin().await.map_err(CopilotError::Storage)?;
    for r in rows {
        let day = &r.snapshot_day;
        sqlx::query(
            "INSERT INTO copilot_seat_snapshots \
             (id, tenant_id, organization_id, snapshot_day, provider_user_id, user_login, \
              seat_assigned_at, last_activity_at, last_activity_editor, seat_state) \
             VALUES ($1, $2, $3, CAST($4 AS date), $5, $6, $7, $8, $9, $10) \
             ON CONFLICT (tenant_id, organization_id, snapshot_day, provider_user_id) \
             DO UPDATE SET \
               user_login = EXCLUDED.user_login, \
               seat_assigned_at = EXCLUDED.seat_assigned_at, \
               last_activity_at = EXCLUDED.last_activity_at, \
               last_activity_editor = EXCLUDED.last_activity_editor, \
               seat_state = EXCLUDED.seat_state",
        )
        .bind(row_id("seat", tenant_id, &r.provider_user_id, day))
        .bind(tenant_id)
        .bind(organization_id)
        .bind(day)
        .bind(&r.provider_user_id)
        .bind(&r.user_login)
        .bind(r.seat_assigned_at)
        .bind(r.last_activity_at)
        .bind(&r.last_activity_editor)
        .bind(&r.seat_state)
        .execute(&mut *tx)
        .await
        .map_err(CopilotError::Storage)?;
    }
    let n = rows.len();
    tx.commit().await.map_err(CopilotError::Storage)?;
    Ok(n)
}

/// Record the outcome of ingesting one (report, day) for ADR-0007 metrics and
/// the high-water-mark backfill.
#[expect(
    clippy::too_many_arguments,
    reason = "an ingest manifest carries (tenant, provider, scope, day, report_type, status, count) by nature; grouping would hide the natural-key identity it is keyed on"
)]
pub async fn upsert_manifest(
    pool: &PgPool,
    tenant_id: &str,
    provider: &str,
    scope_id: &str,
    report_type: &str,
    day: &str,
    status: &str,
    record_count: usize,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO ingest_manifests \
         (id, tenant_id, provider, scope_id, report_day, report_type, status, record_count, \
          schema_version, started_at, completed_at) \
         VALUES ($1, $2, $3, $4, CAST($5 AS date), $6, $7, $8, $9, now(), now()) \
         ON CONFLICT (tenant_id, provider, scope_id, report_day, report_type) DO UPDATE SET \
           status = EXCLUDED.status, \
           record_count = EXCLUDED.record_count, \
           schema_version = EXCLUDED.schema_version, \
           completed_at = now()",
    )
    // The manifest id must disambiguate two reports for the same (tenant, org,
    // day): fold the report type into the natural-key-derived id.
    .bind(format!(
        "manifest:{tenant_id}:{scope_id}:{report_type}:{day}"
    ))
    .bind(tenant_id)
    .bind(provider)
    .bind(scope_id)
    .bind(day)
    .bind(report_type)
    .bind(status)
    .bind(record_count as i64)
    .bind(crate::SCHEMA_VERSION)
    .execute(pool)
    .await
    .map_err(CopilotError::Storage)?;
    Ok(())
}

/// The most recent report day already ingested (for the high-water mark).
///
/// Deliberately excludes `SEATS_REPORT_TYPE`: the seat snapshot succeeds on
/// every run and always writes `report_day = today` (see
/// `crate::SEATS_REPORT_TYPE`'s doc comment), so including it here would
/// advance the *daily reports'* high-water mark to "today" even while every
/// daily report has been failing for a week -- silently disabling the
/// gap-filling half of `app/governance-ctl/src/sync.rs`'s
/// `backfill_window`. This query answers "how current are the day-based
/// reports", not "did the connector do anything recently".
pub async fn high_water_mark(
    pool: &PgPool,
    tenant_id: &str,
    provider: &str,
) -> Result<Option<chrono::NaiveDate>> {
    let row: Option<(Option<chrono::NaiveDate>,)> = sqlx::query_as(
        "SELECT MAX(report_day::date) FROM ingest_manifests \
         WHERE tenant_id = $1 AND provider = $2 AND report_type <> $3",
    )
    .bind(tenant_id)
    .bind(provider)
    .bind(crate::SEATS_REPORT_TYPE)
    .fetch_optional(pool)
    .await
    .map_err(CopilotError::Storage)?;
    // MAX() over an empty table yields a single NULL row, not no row.
    Ok(row.and_then(|(d,)| d))
}

/// Deterministic id for a row, derived from the natural key. Callers must pass
/// enough of the natural key to disambiguate rows of the same day: the manifest
/// caller passes the report type as `scope`, because two reports for the same
/// (tenant, org, day) would otherwise collide on the primary key.
fn row_id(kind: &str, tenant: &str, scope: &str, day: &str) -> String {
    format!("{kind}:{tenant}:{scope}:{day}")
}

/// One manifest row whose stored count disagrees with what the table holds.
/// `verify_manifests` yields one of these per mismatch; a clean run yields none.
#[derive(Debug, Clone)]
pub struct ManifestDrift {
    pub day: String,
    pub report: String,
    pub status: String,
    pub expected: i64,
    pub actual: i64,
}

/// Reconcile every manifest for (tenant, provider, scope) against the rows
/// actually stored, per (report, day). This is how gaps and half-ingested days
/// become visible instead of silent (RFC-0001 verification).
pub async fn verify_manifests(
    pool: &PgPool,
    tenant_id: &str,
    provider: &str,
    scope_id: &str,
) -> Result<Vec<ManifestDrift>> {
    let manifests: Vec<(chrono::NaiveDate, String, String, i64)> = sqlx::query_as(
        "SELECT report_day::date, report_type, status, record_count \
         FROM ingest_manifests WHERE tenant_id = $1 AND provider = $2 AND scope_id = $3 \
         ORDER BY report_day DESC",
    )
    .bind(tenant_id)
    .bind(provider)
    .bind(scope_id)
    .fetch_all(pool)
    .await
    .map_err(CopilotError::Storage)?;

    let mut drift = Vec::new();
    for (day, report, status, expected) in manifests {
        let actual = count_rows(pool, tenant_id, scope_id, &report, &day).await?;
        if actual != expected {
            drift.push(ManifestDrift {
                day: day.to_string(),
                report,
                status,
                expected,
                actual,
            });
        }
    }
    Ok(drift)
}

/// Count the stored rows a manifest row claims to cover.
async fn count_rows(
    pool: &PgPool,
    tenant_id: &str,
    scope_id: &str,
    report: &str,
    day: &chrono::NaiveDate,
) -> Result<i64> {
    let day = day.to_string();
    let (n,): (i64,) = match report {
        // The org aggregate carries the payload's organization id, so it is
        // counted by tenant + day alone; user/repo/team rows carry the scope.
        "organization-1-day" => sqlx::query_as(
            "SELECT count(*) FROM copilot_org_dailys \
             WHERE tenant_id = $1 AND report_day = CAST($2 AS date)",
        )
        .bind(tenant_id)
        .bind(&day)
        .fetch_one(pool)
        .await
        .map_err(CopilotError::Storage)?,
        "users-1-day" => sqlx::query_as(
            "SELECT count(*) FROM copilot_user_dailys \
             WHERE tenant_id = $1 AND organization_id = $2 AND report_day = CAST($3 AS date)",
        )
        .bind(tenant_id)
        .bind(scope_id)
        .bind(&day)
        .fetch_one(pool)
        .await
        .map_err(CopilotError::Storage)?,
        "repos-1-day" => sqlx::query_as(
            "SELECT count(*) FROM copilot_repo_dailys \
             WHERE tenant_id = $1 AND organization_id = $2 AND report_day = CAST($3 AS date)",
        )
        .bind(tenant_id)
        .bind(scope_id)
        .bind(&day)
        .fetch_one(pool)
        .await
        .map_err(CopilotError::Storage)?,
        "user-teams-1-day" => sqlx::query_as(
            "SELECT count(*) FROM copilot_user_teams \
             WHERE tenant_id = $1 AND organization_id = $2 AND report_day = CAST($3 AS date)",
        )
        .bind(tenant_id)
        .bind(scope_id)
        .bind(&day)
        .fetch_one(pool)
        .await
        .map_err(CopilotError::Storage)?,
        // The seat snapshot's manifest row carries `snapshot_day`, not a
        // historical `report_day` -- same column position, different
        // table/name, so it still reconciles through the identical
        // `ManifestDrift` shape as the four day-based reports above.
        crate::SEATS_REPORT_TYPE => sqlx::query_as(
            "SELECT count(*) FROM copilot_seat_snapshots \
             WHERE tenant_id = $1 AND organization_id = $2 AND snapshot_day = CAST($3 AS date)",
        )
        .bind(tenant_id)
        .bind(scope_id)
        .bind(&day)
        .fetch_one(pool)
        .await
        .map_err(CopilotError::Storage)?,
        other => {
            return Err(CopilotError::github(
                "verify",
                0,
                format!("unknown report type {other}"),
            ));
        }
    };
    Ok(n)
}

/// Users with usage rows on `day` but no team membership row: attribution is
/// incomplete for them (teams with <5 seated users are omitted by GitHub).
pub async fn unmapped_user_count(
    pool: &PgPool,
    tenant_id: &str,
    org: &str,
    day: &str,
) -> Result<i64> {
    let (n,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM ( \
           SELECT provider_user_id FROM copilot_user_dailys \
             WHERE tenant_id = $1 AND organization_id = $2 AND report_day = CAST($3 AS date) \
           EXCEPT \
           SELECT user_id FROM copilot_user_teams \
             WHERE tenant_id = $1 AND organization_id = $2 AND report_day = CAST($3 AS date) \
         ) unmapped",
    )
    .bind(tenant_id)
    .bind(org)
    .bind(day)
    .fetch_one(pool)
    .await
    .map_err(CopilotError::Storage)?;
    Ok(n)
}

/// The `schema_version` a day's report was last ingested under, for `replay`
/// to warn when the archive predates the current normalized shape.
pub async fn manifest_schema_version(
    pool: &PgPool,
    tenant_id: &str,
    provider: &str,
    scope_id: &str,
    report: &str,
    day: &str,
) -> Result<Option<i64>> {
    let row: Option<(i64,)> = sqlx::query_as(
        "SELECT schema_version FROM ingest_manifests \
         WHERE tenant_id = $1 AND provider = $2 AND scope_id = $3 \
           AND report_type = $4 AND report_day = CAST($5 AS date)",
    )
    .bind(tenant_id)
    .bind(provider)
    .bind(scope_id)
    .bind(report)
    .bind(day)
    .fetch_optional(pool)
    .await
    .map_err(CopilotError::Storage)?;
    Ok(row.map(|(v,)| v))
}
