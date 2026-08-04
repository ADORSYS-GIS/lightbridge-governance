//! Integration credential issuance, revocation and resolution (#10, ADR-0006).
//!
//! Credentials are 256-bit CSPRNG-generated values, not human-chosen
//! passwords -- hashed with SHA-256, not argon2id/bcrypt. A slow/memory-hard
//! hash exists to blunt brute-forcing *low-entropy* secrets; it buys nothing
//! against a uniformly random 256-bit value and would only add real latency to
//! every `/internal/v1/resolve` cache miss (ADR-0006's gateway hot path).
//! SHA-256 also allows a direct `WHERE credential_hash = sha256(presented)`
//! lookup, since (unlike argon2id/bcrypt) it has no per-hash random salt.
//! Matches `lightbridge-authz`'s `ApiKey`/`createApiKey` precedent exactly.
//!
//! `Integration` has no `@@allow("create"/"update", ...)` policy, so issuance
//! and revocation go through raw SQL against the pool here (the sanctioned
//! escape hatch, ADR-0009) rather than the generated CRUD -- then re-read the
//! canonical record through the generated (policy-checked) `find_unique` path
//! for the response, rather than hand-maintaining a `RETURNING` column list.

use std::fmt;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use cratestack::{CoolContext, CoolError, cool_error_from_sqlx, sqlx};
use sha2::{Digest, Sha256};

use crate::schema::cratestack_schema::{
    Cratestack,
    types::{
        IntegrationCredential, IssueIntegrationCredentialInput, RevokeIntegrationCredentialInput,
    },
};

const SECRET_PREFIX: &str = "gov_";
const DISPLAY_PREFIX_LEN: usize = 12;
const DEFAULT_CONTENT_CAPTURE: &str = "metadata_only";

/// A credential's plaintext value. `Debug`/`Display` both print `<redacted>`
/// -- structural, not a habit (house rule): this can never end up in a log
/// line, error message, or trace by accident, only by explicitly calling
/// [`CredentialSecret::expose`].
pub struct CredentialSecret(String);

impl CredentialSecret {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for CredentialSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

impl fmt::Display for CredentialSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

fn generate_secret() -> Result<CredentialSecret, CoolError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|error| CoolError::Internal(format!("csprng failure: {error}")))?;
    Ok(CredentialSecret(format!(
        "{SECRET_PREFIX}{}",
        URL_SAFE_NO_PAD.encode(bytes)
    )))
}

fn hash_secret(secret: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(secret.as_bytes());
    hex::encode(hasher.finalize())
}

fn display_prefix(secret: &str) -> String {
    secret.chars().take(DISPLAY_PREFIX_LEN).collect()
}

/// Issues a new credential for an integration under `args.applicationId`.
/// `tenantId` is never taken from the caller -- derived from the application
/// record (which itself is read through the generated, policy-checked path),
/// matching `createApiKey`'s handling of `projectId` in `lightbridge-authz`.
pub async fn issue(
    db: &Cratestack,
    ctx: &CoolContext,
    args: IssueIntegrationCredentialInput,
) -> Result<IntegrationCredential, CoolError> {
    let application = db
        .bind_context(ctx.clone())
        .application()
        .find_unique(args.applicationId.clone())
        .run()
        .await?
        .ok_or_else(|| CoolError::Validation("application not found".to_owned()))?;

    // `environmentId` must be a real, registered `Environment` under *this*
    // application -- not just any environment, or a caller could mint a
    // credential attributed to another application's environment while
    // `application_id` still points at the one they asked for. The FK on
    // `integrations.environment_id` (see the migration comment) only
    // guarantees the environment exists somewhere, not that it belongs here.
    let environment = db
        .bind_context(ctx.clone())
        .environment()
        .find_unique(args.environmentId.clone())
        .run()
        .await?
        .ok_or_else(|| CoolError::Validation("environment not found".to_owned()))?;
    if environment.applicationId != args.applicationId {
        return Err(CoolError::Validation(
            "environment does not belong to the given application".to_owned(),
        ));
    }

    let id = cuid::cuid2();
    let secret = generate_secret()?;
    let prefix = display_prefix(secret.expose());
    let hash = hash_secret(secret.expose());
    let content_capture = args
        .contentCapture
        .unwrap_or_else(|| DEFAULT_CONTENT_CAPTURE.to_owned());

    sqlx::query(
        "INSERT INTO integrations \
         (id, tenant_id, application_id, environment_id, provider, credential_prefix, \
          credential_hash, status, content_capture, internal_user_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, 'active', $8, $9)",
    )
    .bind(&id)
    .bind(&application.tenantId)
    .bind(&args.applicationId)
    .bind(&args.environmentId)
    .bind(&args.provider)
    .bind(&prefix)
    .bind(&hash)
    .bind(&content_capture)
    .bind(&args.internalUserId)
    .execute(db.pool())
    .await
    .map_err(cool_error_from_sqlx)?;

    let integration = db
        .bind_context(ctx.clone())
        .integration()
        .find_unique(id)
        .run()
        .await?
        .ok_or_else(|| CoolError::Internal("just-inserted integration not found".to_owned()))?;

    Ok(IntegrationCredential {
        integration,
        secret: secret.0,
    })
}

/// Revokes an integration's credential. Idempotent: revoking an
/// already-revoked integration is a no-op (the `WHERE status = 'active'`
/// guard means a concurrent double-revoke never races on `revokedAt`), not an
/// error -- only a genuinely unknown id is `NotFound`.
pub async fn revoke(
    db: &Cratestack,
    ctx: &CoolContext,
    args: RevokeIntegrationCredentialInput,
) -> Result<crate::schema::cratestack_schema::models::Integration, CoolError> {
    sqlx::query(
        "UPDATE integrations SET status = 'revoked', revoked_at = now() \
         WHERE id = $1 AND status = 'active'",
    )
    .bind(&args.integrationId)
    .execute(db.pool())
    .await
    .map_err(cool_error_from_sqlx)?;

    db.bind_context(ctx.clone())
        .integration()
        .find_unique(args.integrationId)
        .run()
        .await?
        .ok_or_else(|| CoolError::NotFound("integration not found".to_owned()))
}

/// The identity a valid credential resolves to (#11's own AC: "returns the
/// tenant, application, environment and integration").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedIdentity {
    pub tenant_id: String,
    pub application_id: String,
    pub environment: String,
    pub integration_id: String,
}

/// Deliberately one variant: callers get a single opaque rejection regardless
/// of cause (unknown vs. revoked vs. malformed), matching the AC that this
/// must not be a credential-enumeration oracle. The *reason* is only ever
/// visible in the `tracing` call at the point of rejection, never here.
#[derive(Debug, thiserror::Error)]
#[error("credential rejected")]
pub struct CredentialRejected;

/// Resolves a presented credential to its tenant/application/integration.
/// Fail-closed: a database error is treated exactly like an unknown
/// credential, never like a valid one.
///
/// Deliberately does NOT touch `lastUsedAt` -- this sits underneath
/// `/internal/v1/resolve`, which is in Authorino's ext_authz hot path
/// (ADR-0006); adding a write here would repeat the exact mistake the
/// Keycloak-introspection incident (`docs/adr/0006-...md`) already paid for.
/// `lastUsedAt` tracking, if wanted, is a #11-or-later decision, not this
/// function's.
pub async fn resolve(
    pool: &sqlx::PgPool,
    presented: &str,
) -> Result<ResolvedIdentity, CredentialRejected> {
    let hash = hash_secret(presented);
    // `environment` here is the registered Environment's *name* (e.g. "prod"),
    // matching the shape this returned before Environment existed as its own
    // model (#10's original design) -- joined, not a bare column, now that
    // `integrations.environment_id` is a real FK (see the migration comment).
    let row: Option<(String, String, String, String, String)> = sqlx::query_as(
        "SELECT i.tenant_id, i.application_id, e.name, i.id, i.status \
         FROM integrations i JOIN environments e ON e.id = i.environment_id \
         WHERE i.credential_hash = $1",
    )
    .bind(&hash)
    .fetch_optional(pool)
    .await
    .map_err(|error| {
        tracing::warn!(error = %error, "credential resolve: database error, failing closed");
        CredentialRejected
    })?;

    match row {
        Some((tenant_id, application_id, environment, integration_id, status))
            if status == "active" =>
        {
            Ok(ResolvedIdentity {
                tenant_id,
                application_id,
                environment,
                integration_id,
            })
        }
        Some(_) => {
            tracing::info!("credential resolve: rejected (revoked)");
            Err(CredentialRejected)
        }
        None => {
            tracing::info!("credential resolve: rejected (not found)");
            Err(CredentialRejected)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_secret_debug_and_display_are_both_redacted() {
        let secret = CredentialSecret("gov_super-secret-value".to_owned());
        assert_eq!(format!("{secret:?}"), "<redacted>");
        assert_eq!(format!("{secret}"), "<redacted>");
        assert_eq!(secret.expose(), "gov_super-secret-value");
    }

    #[test]
    fn generated_secrets_are_unique_and_carry_the_expected_prefix() {
        let a = generate_secret().expect("csprng must succeed");
        let b = generate_secret().expect("csprng must succeed");
        assert_ne!(
            a.expose(),
            b.expose(),
            "two generated secrets must not collide"
        );
        assert!(a.expose().starts_with(SECRET_PREFIX));
    }

    #[test]
    fn hash_is_deterministic_and_display_prefix_is_a_real_prefix() {
        let secret = "gov_abcdefghijklmnopqrstuvwxyz";
        assert_eq!(hash_secret(secret), hash_secret(secret));
        assert_ne!(hash_secret(secret), hash_secret("gov_something-else"));
        assert!(secret.starts_with(&display_prefix(secret)));
        assert_eq!(display_prefix(secret).len(), DISPLAY_PREFIX_LEN);
    }
}
