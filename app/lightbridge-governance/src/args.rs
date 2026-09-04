//! The CLI surface for the API server, kept out of `main.rs` so that file
//! stays under the repo's 200-LoC ceiling (see `.github/actions/loc-gate`).

use std::collections::HashSet;

use clap::Parser;

/// Command-line surface for the API server.
#[derive(Debug, Parser)]
#[command(name = "lightbridge-governance", version, about)]
pub struct Args {
    /// Address to bind the HTTP listener to.
    #[arg(long, env = "LISTEN_ADDR", default_value = "0.0.0.0:8080")]
    pub listen_addr: String,

    /// Postgres connection string.
    #[arg(long, env = "DATABASE_URL")]
    pub database_url: String,

    /// Base URL of the kube-apiserver, used for TokenReview-based caller
    /// authentication on `/internal/v1/resolve` (ADR-0017). The
    /// `/apis/authentication.k8s.io/v1/tokenreviews` path is appended
    /// internally. Typically `https://kubernetes.default.svc` in-cluster.
    #[arg(long, env = "KUBE_APISERVER_URL")]
    pub kube_apiserver_url: String,

    /// Audience the projected ServiceAccount token must carry to pass
    /// TokenReview (ADR-0017). Must match the `audience` in the projected
    /// volume's `serviceAccountToken` source.
    #[arg(long, env = "TOKEN_REVIEW_AUDIENCE", default_value = "api")]
    pub token_review_audience: String,

    /// Comma-separated list of permitted ServiceAccount identities that may
    /// call `/internal/v1/resolve`, in `namespace/name` format (ADR-0017).
    /// Authorino's own SA must be in this list. No default: an empty
    /// allowlist rejects every caller, which is the safe startup failure
    /// (same pattern as `TENANT_ID`).
    #[arg(long, env = "ALLOWED_SERVICE_ACCOUNTS", value_parser = parse_allowed_accounts)]
    pub allowed_service_accounts: HashSet<String>,

    /// Shared secret the OpenTelemetry Collector presents as
    /// `X-Internal-Token` on `/internal/v1/ingest` (#30). Never logged --
    /// only its presence/absence is, via the request outcome.
    #[arg(long, env = "INTERNAL_INGEST_TOKEN")]
    pub internal_ingest_token: String,

    /// Upper bound on `/internal/v1/resolve`'s credential lookup, in
    /// milliseconds. Deliberately far below sqlx's own 30s pool default --
    /// this is Authorino's ext_authz hot path, and a dependency's own
    /// timeout must be shorter than the caller's (ADR-0006).
    #[arg(long, env = "RESOLVE_TIMEOUT_MS", default_value_t = 500)]
    pub resolve_timeout_ms: u64,

    /// TTL for the in-process `/internal/v1/resolve` cache (ADR-0006,
    /// ADR-0007). This *is* the revocation SLA the runbook documents --
    /// `docs/runbooks/revoke-an-integration-token.md` -- so it must match
    /// `config/default.yaml`'s `resolveCache.ttlSeconds` default. Has a
    /// `default_value_t` deliberately: an env var with no default is how the
    /// `INTERNAL_INGEST_TOKEN` chart gap became a CrashLoopBackOff (see
    /// AGENTS.md); this one must never repeat that.
    #[arg(long, env = "RESOLVE_CACHE_TTL_SECS", default_value_t = 60)]
    pub resolve_cache_ttl_secs: u64,

    /// Max entries the `/internal/v1/resolve` cache holds before moka starts
    /// evicting. Must match `config/default.yaml`'s
    /// `resolveCache.maxCapacity` default. See `resolve_cache_ttl_secs` for
    /// why this also carries a `default_value_t`.
    #[arg(long, env = "RESOLVE_CACHE_MAX_CAPACITY", default_value_t = 10_000)]
    pub resolve_cache_max_capacity: u64,

    /// Max `/internal/v1/ingest` requests per integration per
    /// `INGEST_RATE_WINDOW_SECS`. A throttle, not a billing meter.
    #[arg(long, env = "INGEST_RATE_MAX_PER_WINDOW", default_value_t = 600)]
    pub ingest_rate_max_per_window: u64,

    /// Fixed window length for the `/internal/v1/ingest` rate limiter.
    #[arg(long, env = "INGEST_RATE_WINDOW_SECS", default_value_t = 60)]
    pub ingest_rate_window_secs: u64,

    /// Single-tenant deployment (ADR-0001). Scopes the `governance_connector_*`
    /// freshness query `/metrics` derives from `ingest_manifests` (ADR-0007) --
    /// `tenant_id` belongs in the WHERE clause of every query, even in a
    /// deployment that only ever has the one. No default: an empty tenant_id
    /// has no safe meaning here, matching `governance-ctl`'s own `TENANT_ID`
    /// (`app/governance-ctl/src/sync.rs`).
    ///
    /// `value_parser` rather than a bare `String`: clap treats a required arg
    /// as satisfied by an EMPTY value, and the chart renders `TENANT_ID: ""`
    /// by default, so `required` alone never caught the case that actually
    /// happened in production -- a deployment running with no tenant identity
    /// at all, scoping `governance_connector_*` to a tenant that owns no rows
    /// and therefore reporting has_synced=0 forever.
    #[arg(long, env = "TENANT_ID", value_parser = parse_tenant_id)]
    pub tenant_id: String,

    /// Upper bound on the `governance_connector_*` freshness query `/metrics`
    /// runs against `ingest_manifests` on every scrape (ADR-0007).
    /// Deliberately far below the ServiceMonitor's 30s scrape interval
    /// (`charts/lightbridge-governance/values.yaml`'s `serviceMonitor.interval`),
    /// same reasoning as `resolve_timeout_ms`: a dependency's own timeout must
    /// be shorter than the caller's, not left at sqlx's 30s pool
    /// `acquire_timeout` default.
    #[arg(long, env = "CONNECTOR_METRICS_TIMEOUT_MS", default_value_t = 3_000)]
    pub connector_metrics_timeout_ms: u64,

    /// Upper bound (per query -- usage and seats are queried independently,
    /// see `Metrics::refresh_org_kpis`) on the `governance_org_*` KPI
    /// queries `/metrics` runs against `copilot_org_dailys`/
    /// `copilot_seat_snapshots` on every scrape. Same reasoning as
    /// `connector_metrics_timeout_ms`.
    #[arg(long, env = "ORG_KPI_TIMEOUT_MS", default_value_t = 3_000)]
    pub org_kpi_timeout_ms: u64,
}

/// Rejects an empty or whitespace-only tenant id at argument-parse time, so a
/// misconfigured deployment fails immediately and loudly instead of running
/// with no tenant identity.
///
/// This is a real failure that reached production, not a hypothetical: the
/// chart's structural default is `TENANT_ID: ""`, clap counts a required arg
/// as satisfied by an empty value, and `std::env::var` returns `Ok("")` for a
/// set-but-empty variable -- so all three layers said "present" and the
/// deployment ran with `tenant_id = ''`.
fn parse_tenant_id(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(
            "TENANT_ID is set but empty. It is this deployment's tenant identity \
             (ADR-0001) and scopes the governance_connector_* freshness query \
             /metrics derives from ingest_manifests (ADR-0007), so an empty value \
             reports has_synced=0 forever regardless of how healthy the connector \
             is. Set `copilot.tenantId` in the deployed values (ai-helm-values)."
                .to_owned(),
        );
    }
    Ok(trimmed.to_owned())
}

/// Parses a comma-separated list of `<namespace>/<name>` ServiceAccount
/// identities. Rejects empty or whitespace-only entries, and entries that
/// don't contain a `/` separator (the namespace/name boundary).
fn parse_allowed_accounts(raw: &str) -> Result<HashSet<String>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("ALLOWED_SERVICE_ACCOUNTS is set but empty. It lists the \
             ServiceAccounts permitted to call /internal/v1/resolve \
             (ADR-0017). An empty list rejects every caller. Set it to \
             at least Authorino's SA (e.g. \"authorino/authorino\")."
            .to_owned());
    }
    let mut accounts = HashSet::new();
    for entry in trimmed.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        if !entry.contains('/') {
            return Err(format!(
                "ALLOWED_SERVICE_ACCOUNTS entry \"{entry}\" is not in \
                 namespace/name format (e.g. \"authorino/authorino\"). \
                 Every entry must contain a `/` separator."
            ));
        }
        accounts.insert(entry.to_owned());
    }
    if accounts.is_empty() {
        return Err("ALLOWED_SERVICE_ACCOUNTS contains no valid entries after \
             trimming. Every entry must be in namespace/name format."
            .to_owned());
    }
    Ok(accounts)
}
