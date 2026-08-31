//! Identity resolution for telemetry attribution (#35).
//!
//! Identity comes from the ingest token, not from the payload. Both Claude Code
//! and Codex stamp `user.email` in their telemetry, but it is self-asserted by
//! the client and may be absent (API-key auth, metric-only records). The
//! credential is the thing we issued and can revoke; the email is a claim to
//! check against it.
//!
//! The resolution flow:
//! 1. The ingest token identifies an integration (via `/internal/v1/resolve`).
//! 2. The integration's bound `internal_user_id` (Keycloak sub) is the authoritative
//!    identity for attribution.
//! 3. If the payload's `user.email` disagrees with the token's bound identity,
//!    an alert fires but the token-derived identity wins (the mismatch is a
//!    signal worth seeing, not something to resolve silently).
//! 4. If the payload carries no identity, the token-derived identity is used
//!    and this is not an error.
//!
//! ## Schema Assumption
//!
//! The `identity_maps` table stores provider-specific user identifiers in
//! `provider_user_id`. For Claude Code and Codex, this is the user's email
//! address. The table is populated by the identity mapping process (e.g., when
//! a user authenticates via GitHub OAuth, their GitHub email is mapped to their
//! Keycloak `internal_user_id`). This table must be populated by the identity
//! provider integration, not by the telemetry ingest pipeline.

use std::collections::{HashMap, HashSet};

use cratestack::{cratestack_error_from_sqlx, sqlx};
use sqlx::{PgPool, Postgres, Transaction};

use crate::Error;

/// The result of resolving identity for a telemetry record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityResolution {
    /// The Keycloak sub (internal user ID) derived from the ingest token.
    /// This is the authoritative identity for attribution.
    pub internal_user_id: Option<String>,
    /// Whether there was a mismatch between the token-derived identity and the
    /// payload's email. This is a signal worth alerting on.
    pub mismatch: bool,
}

/// Looks up the integration's bound identity (token-derived).
///
/// This is the authoritative identity for attribution. Called once per batch
/// to avoid redundant queries.
pub async fn get_integration_identity(
    pool: &PgPool,
    tenant_id: &str,
    integration_id: &str,
) -> Result<Option<String>, Error> {
    let row: Option<(Option<String>,)> = sqlx::query_as(
        "SELECT internal_user_id FROM integrations \
         WHERE id = $1 AND tenant_id = $2",
    )
    .bind(integration_id)
    .bind(tenant_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| Error::Storage(cratestack_error_from_sqlx(e)))?;

    Ok(row.and_then(|r| r.0))
}

/// Checks if payload emails mismatch the token-derived identity.
///
/// Returns a tuple of (token_user_id, mismatch_flags, query_failed) where:
/// - mismatch_flags[i] indicates whether payload_emails[i] mismatches the token identity
/// - query_failed indicates whether the identity_maps query failed (best-effort detection)
///
/// The mismatch check queries `identity_maps` for all emails in the batch at
/// once to avoid N+1 queries.
///
/// This is best-effort: if the query fails, all mismatch flags are false and
/// query_failed is true. The caller should increment a metric and log the error.
/// Mismatch detection should not block telemetry ingest.
pub async fn check_email_mismatches(
    pool: &PgPool,
    tenant_id: &str,
    provider: &str,
    token_user_id: Option<&str>,
    payload_emails: &[Option<&str>],
) -> (Option<String>, Vec<bool>, bool) {
    // Collect unique non-None emails for batch lookup.
    let unique_emails: Vec<&str> = payload_emails
        .iter()
        .filter_map(|e| *e)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    // Batch query identity_maps for all emails at once.
    let (email_mappings, query_failed) = if unique_emails.is_empty() {
        (HashMap::new(), false)
    } else {
        match sqlx::query_as::<_, (String, String)>(
            "SELECT provider_user_id, internal_user_id FROM identity_maps \
             WHERE tenant_id = $1 AND provider = $2 AND provider_user_id = ANY($3) \
             AND (valid_to IS NULL OR valid_to > now()) \
             ORDER BY valid_from DESC",
        )
        .bind(tenant_id)
        .bind(provider)
        .bind(&unique_emails)
        .fetch_all(pool)
        .await
        {
            Ok(rows) => {
                // Keep only the most recent mapping per email (first occurrence after ORDER BY).
                let mut map = HashMap::new();
                for (email, user_id) in rows {
                    map.entry(email).or_insert(user_id);
                }
                (map, false)
            }
            Err(e) => {
                // Best-effort: log error and continue without mismatch detection.
                tracing::error!(
                    tenant_id = %tenant_id,
                    provider = %provider,
                    error = %cratestack_error_from_sqlx(e),
                    "failed to query identity_maps for mismatch detection; continuing without mismatch alerts"
                );
                (HashMap::new(), true)
            }
        }
    };

    // Build mismatch flags for each email.
    let mismatches = payload_emails
        .iter()
        .map(|email_opt| {
            if let (Some(token_id), Some(email)) = (token_user_id, email_opt) {
                email_mappings
                    .get(*email)
                    .is_some_and(|mapped_id| mapped_id != token_id)
            } else {
                false
            }
        })
        .collect();

    (
        token_user_id.map(|s| s.to_string()),
        mismatches,
        query_failed,
    )
}

/// One entry from the identity directory (Keycloak): a provider principal and
/// the internal user (Keycloak sub) it belongs to.
///
/// `provider_user_id` is provider-specific -- for GitHub Copilot it is the
/// user's email; for Microsoft Foundry it is the user's directory object id.
/// `internal_user_id` is always the Keycloak sub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryEntry {
    pub provider_user_id: String,
    pub internal_user_id: String,
}

/// The outcome of a directory sync, per entry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IdentitySyncReport {
    /// Entries that had no active mapping and got one.
    pub inserted: usize,
    /// Entries whose active mapping pointed at a different user and was
    /// re-pointed (old row closed, new row opened).
    pub repointed: usize,
    /// Entries already mapped to the right user -- no change.
    pub unchanged: usize,
}

/// Syncs the identity directory into `identity_maps` (ADR-0001, RFC-0001).
///
/// Idempotent by construction: re-running the same directory changes no row
/// counts. For each entry the *active* mapping (the one with `valid_to IS
/// NULL`) is the source of truth:
///
/// - no active mapping -> insert one (`valid_from = now()`, `valid_to = NULL`);
/// - active mapping already points at the right `internal_user_id` -> no-op;
/// - active mapping points at a *different* user -> close it (`valid_to =
///   now()`) and open a new one, so history is preserved and a record is
///   attributed by the mapping that was valid at its own time, not today's.
///
/// When several rows are simultaneously active for the same
/// `provider_user_id` (an OAuth-flow row plus a directory row), the one with
/// the latest `valid_from` wins -- the same rule `check_email_mismatches` and
/// `verify_attribution` apply. Active mappings are loaded in one batched
/// query, not per entry.
///
/// `mapping_source` is stamped `key_directory` so directory-derived rows are
/// distinguishable from OAuth-flow rows.
///
/// Runs in a single transaction: a partial failure rolls back the whole sync.
pub async fn sync_identity_directory(
    pool: &PgPool,
    tenant_id: &str,
    provider: &str,
    entries: &[DirectoryEntry],
) -> crate::Result<IdentitySyncReport> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| Error::Storage(cratestack_error_from_sqlx(e)))?;

    // One query for the whole directory instead of one per entry.
    // `ORDER BY valid_from DESC` then first-wins keeps the latest active
    // mapping per provider_user_id, mirroring the live mismatch lookup.
    let active_rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT provider_user_id, internal_user_id FROM identity_maps \
         WHERE tenant_id = $1 AND provider = $2 AND valid_to IS NULL \
         ORDER BY valid_from DESC",
    )
    .bind(tenant_id)
    .bind(provider)
    .fetch_all(&mut *tx)
    .await
    .map_err(|e| Error::Storage(cratestack_error_from_sqlx(e)))?;

    let mut active_by_provider_user_id: HashMap<String, String> = HashMap::new();
    for (provider_user_id, internal_user_id) in active_rows {
        active_by_provider_user_id
            .entry(provider_user_id)
            .or_insert(internal_user_id);
    }

    let mut report = IdentitySyncReport::default();
    for entry in entries {
        match active_by_provider_user_id.get(&entry.provider_user_id) {
            None => {
                insert_identity_map(&mut tx, tenant_id, provider, entry).await?;
                report.inserted += 1;
            }
            Some(current) if *current == entry.internal_user_id => {
                report.unchanged += 1;
            }
            Some(_) => {
                close_active_identity_map(&mut tx, tenant_id, provider, &entry.provider_user_id)
                    .await?;
                insert_identity_map(&mut tx, tenant_id, provider, entry).await?;
                report.repointed += 1;
            }
        }
    }

    tx.commit()
        .await
        .map_err(|e| Error::Storage(cratestack_error_from_sqlx(e)))?;

    Ok(report)
}

async fn insert_identity_map(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: &str,
    provider: &str,
    entry: &DirectoryEntry,
) -> crate::Result<()> {
    sqlx::query(
        "INSERT INTO identity_maps \
         (id, tenant_id, provider, provider_user_id, internal_user_id, mapping_source) \
         VALUES ($1, $2, $3, $4, $5, 'key_directory')",
    )
    .bind(format!("idmap-{}", cuid::cuid2()))
    .bind(tenant_id)
    .bind(provider)
    .bind(&entry.provider_user_id)
    .bind(&entry.internal_user_id)
    .execute(&mut **tx)
    .await
    .map_err(|e| Error::Storage(cratestack_error_from_sqlx(e)))?;
    Ok(())
}

async fn close_active_identity_map(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: &str,
    provider: &str,
    provider_user_id: &str,
) -> crate::Result<()> {
    sqlx::query(
        "UPDATE identity_maps SET valid_to = now() \
         WHERE tenant_id = $1 AND provider = $2 AND provider_user_id = $3 AND valid_to IS NULL",
    )
    .bind(tenant_id)
    .bind(provider)
    .bind(provider_user_id)
    .execute(&mut **tx)
    .await
    .map_err(|e| Error::Storage(cratestack_error_from_sqlx(e)))?;
    Ok(())
}

/// Per-provider attribution counts for `verify` (RFC-0001).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderAttribution {
    pub provider: String,
    /// Executions attributed to an internal user (token-derived identity).
    ///
    /// `attributed` and `mismatched` are deliberately **not** mutually
    /// exclusive: a mismatched execution was still attributed to *a* user
    /// (the token's), it just contradicts the payload email. So a fully
    /// consistent source shows `mismatched = 0`, and `attributed =
    /// total - unattributed`.
    pub attributed: i64,
    /// Executions with no internal user -- telemetry that could not be
    /// attributed to anyone.
    pub unattributed: i64,
    /// Executions whose payload email contradicted the token-derived identity.
    pub mismatched: i64,
}

/// The attribution report `verify` prints and gates on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttributionReport {
    pub providers: Vec<ProviderAttribution>,
}

impl AttributionReport {
    /// Any provider with unattributed executions. `verify` fails when this is
    /// non-empty for a fully-deployed source (the caller decides what
    /// "fully deployed" means for its own deployment).
    pub fn has_unattributed(&self) -> bool {
        self.providers.iter().any(|p| p.unattributed > 0)
    }
}

/// Computes per-provider attribution counts from stored executions.
///
/// `attributed`/`unattributed` come straight off the `executions` row.
/// `mismatched` re-derives the ingest-time mismatch check from stored data.
///
/// The mismatch rule here is the **same** rule `check_email_mismatches`
/// applies live, evaluated against history: an execution is mismatched when
/// the payload email's *most recent* mapping valid at the execution's own
/// `started_at` resolves to a different internal user than the token-derived
/// identity stored on the row. When several mappings overlap in time, the one
/// with the latest `valid_from` wins -- exactly like the live lookup, which
/// keeps only the latest active mapping per email. An email with no mapping at
/// that time is **not** a mismatch (same as live).
pub async fn verify_attribution(
    pool: &PgPool,
    tenant_id: &str,
) -> crate::Result<AttributionReport> {
    let rows: Vec<(String, i64, i64, i64)> = sqlx::query_as(
        "SELECT e.provider,
                COUNT(*) FILTER (WHERE e.internal_user_id IS NOT NULL) AS attributed,
                COUNT(*) FILTER (WHERE e.internal_user_id IS NULL) AS unattributed,
                COUNT(*) FILTER (
                    WHERE e.user_email IS NOT NULL
                      AND e.internal_user_id IS NOT NULL
                      AND e.internal_user_id <> (
                        SELECT m.internal_user_id FROM identity_maps m
                        WHERE m.tenant_id = e.tenant_id
                          AND m.provider = e.provider
                          AND m.provider_user_id = e.user_email
                          AND m.valid_from <= e.started_at
                          AND (m.valid_to IS NULL OR m.valid_to > e.started_at)
                        ORDER BY m.valid_from DESC
                        LIMIT 1
                      )
                ) AS mismatched
         FROM executions e
         WHERE e.tenant_id = $1
         GROUP BY e.provider
         ORDER BY e.provider",
    )
    .bind(tenant_id)
    .fetch_all(pool)
    .await
    .map_err(|e| Error::Storage(cratestack_error_from_sqlx(e)))?;

    Ok(AttributionReport {
        providers: rows
            .into_iter()
            .map(
                |(provider, attributed, unattributed, mismatched)| ProviderAttribution {
                    provider,
                    attributed,
                    unattributed,
                    mismatched,
                },
            )
            .collect(),
    })
}
