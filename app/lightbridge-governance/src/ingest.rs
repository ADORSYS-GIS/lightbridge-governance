//! `/internal/v1/ingest` for OTLP telemetry (#30).
//!
//! Receives **raw OTLP JSON** from the OpenTelemetry Collector (the collector
//! exports OTLP, not our normalized model), dispatches to the correct provider
//! normalizer, and persists through `governance_core::ingest::ingest_telemetry`
//! -- the single persistence path. Like `/internal/v1/resolve`, this is a
//! hand-written JSON route, deliberately outside the cratestack-generated
//! router: the collector cannot speak CBOR (ADR-0009).
//!
//! Authentication: Authorino validates the collector's bearer token and stamps
//! trusted headers (`governance.tenant.id`, `governance.integration.id`,
//! `governance.source`). The shared `X-Internal-Token` (constant-time
//! compared, matching resolve) authenticates the collector process itself, so
//! a mis-stamped or absent header is rejected before the body is parsed. The
//! endpoint never reads tenant/integration from the request body (RFC-0002's
//! trust boundary).
//!
//! Fail-closed: every rejection path -- bad shared secret, malformed OTLP,
//! unsupported provider, rate limited, storage error -- returns a generic
//! error body. The distinguishing reason lives only in the `tracing` log at
//! the point of rejection, never in the response.

use std::sync::Arc;

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use subtle::ConstantTimeEq;

use crate::{metrics::Metrics, rate_limit::RateLimiter};

/// Deliberate cap on an OTLP export batch (main.rs installs it as the route's
/// `DefaultBodyLimit`). Larger than axum's implicit 2 MiB so real agent
/// telemetry batches fit; smaller than the collector could produce, so a
/// runaway batch fails fast at the edge instead of tying up the DB.
pub const MAX_OTLP_BODY_BYTES: usize = 10 * 1024 * 1024;

#[derive(Clone)]
pub struct IngestState {
    pub pool: cratestack::sqlx::PgPool,
    /// Shared secret the OpenTelemetry Collector presents as
    /// `X-Internal-Token`, matching `/internal/v1/resolve`'s Authorino shared
    /// secret. Never logged -- only presence/absence, via the outcome.
    pub internal_token: Arc<str>,
    /// Per-integration throttle so one noisy collector can't melt the write
    /// path.
    pub rate_limiter: Arc<RateLimiter>,
    pub metrics: Arc<Metrics>,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: &'static str,
}

/// Every distinguishable-in-logs, opaque-to-the-caller rejection reason.
/// Never rendered into the HTTP response -- only into a `tracing` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RejectReason {
    BadSharedSecret,
    MissingTenantHeader,
    MissingIntegrationHeader,
    MissingProviderHeader,
    UnsupportedProvider,
    RateLimited,
    MalformedOtlp,
    IngestFailed,
}

impl std::fmt::Display for RejectReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::BadSharedSecret => "bad_shared_secret",
            Self::MissingTenantHeader => "missing_tenant_header",
            Self::MissingIntegrationHeader => "missing_integration_header",
            Self::MissingProviderHeader => "missing_provider_header",
            Self::UnsupportedProvider => "unsupported_provider",
            Self::RateLimited => "rate_limited",
            Self::MalformedOtlp => "malformed_otlp",
            Self::IngestFailed => "ingest_failed",
        })
    }
}

fn shared_secret_is_valid(headers: &HeaderMap, expected: &str) -> bool {
    let Some(presented) = headers
        .get("x-internal-token")
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    // Constant-time: this header authenticates the collector process itself,
    // so a timing side-channel here is the same class of bug resolve already
    // guards against.
    bool::from(presented.as_bytes().ct_eq(expected.as_bytes()))
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

async fn handle(
    state: &IngestState,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<governance_core::ingest::IngestResult, RejectReason> {
    if !shared_secret_is_valid(headers, &state.internal_token) {
        return Err(RejectReason::BadSharedSecret);
    }

    let tenant_id =
        header(headers, "governance.tenant.id").ok_or(RejectReason::MissingTenantHeader)?;
    let integration_id = header(headers, "governance.integration.id")
        .ok_or(RejectReason::MissingIntegrationHeader)?;
    // The provider is data on the registered integration, stamped here by
    // Authorino as the integration row's provider string. The ingest path
    // dispatches on that string and never enumerates the provider list (story
    // #31 AC1/AC4) -- an unknown string is a normalizer lookup miss, not a
    // compile-time branch.
    let provider =
        header(headers, "governance.source").ok_or(RejectReason::MissingProviderHeader)?;

    if !state.rate_limiter.allow(integration_id) {
        return Err(RejectReason::RateLimited);
    }

    // Raw OTLP JSON -- the collector's export shape, parsed by the normalizer.
    let otlp: serde_json::Value =
        serde_json::from_slice(body).map_err(|_| RejectReason::MalformedOtlp)?;

    let normalizer = governance_foundry::normalizer::dispatch_normalizer(provider)
        .map_err(|_| RejectReason::UnsupportedProvider)?;
    let payload = normalizer
        .normalize(&otlp)
        .map_err(|_| RejectReason::MalformedOtlp)?;

    if payload.executions.is_empty() {
        return Err(RejectReason::MalformedOtlp);
    }

    governance_core::ingest::ingest_telemetry(
        &state.pool,
        tenant_id,
        integration_id,
        provider,
        &payload.executions,
    )
    .await
    .map_err(|error| {
        // Log the error's *kind* only, never its `Display`: per
        // `governance_core::error`, the inner error can carry query text and
        // bind values (possibly a credential hash). The kind is enough to tell
        // "dead DB" from "constraint violation" in operations.
        tracing::warn!(
            reason = "ingest_failed",
            error_kind = storage_error_kind(&error),
            "ingest: persistence failed"
        );
        RejectReason::IngestFailed
    })
}

/// A short, safe-to-log classification of a [`governance_core::Error`]. This
/// is the only thing the ingest path ever records about a storage failure --
/// deliberately not the error's `Display` (PII / bind values).
fn storage_error_kind(error: &governance_core::Error) -> &'static str {
    match error {
        governance_core::Error::NotFound(_) => "not_found",
        governance_core::Error::Forbidden(_) => "forbidden",
        governance_core::Error::Validation(_) => "validation",
        governance_core::Error::Storage(_) => "storage",
    }
}

pub async fn ingest(
    State(state): State<IngestState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let outcome = handle(&state, &headers, &body).await;

    match outcome {
        Ok(result) => {
            state
                .metrics
                .ingest_requests_total
                .with_label_values(&["success"])
                .inc();
            state
                .metrics
                .ingest_executions_total
                .inc_by(result.executions_upserted as u64);
            state
                .metrics
                .ingest_model_calls_total
                .inc_by(result.model_calls_upserted as u64);
            state
                .metrics
                .ingest_tool_calls_total
                .inc_by(result.tool_calls_upserted as u64);

            // Increment metric if identity mismatch detection failed (best-effort).
            if result.identity_mismatch_detection_failed {
                state
                    .metrics
                    .ingest_identity_mismatch_failures_total
                    .inc();
            }

            tracing::info!(
                executions = result.executions_upserted,
                model_calls = result.model_calls_upserted,
                tool_calls = result.tool_calls_upserted,
                "ingest: accepted"
            );

            #[derive(Serialize)]
            struct IngestOk {
                executions_upserted: i64,
                model_calls_upserted: i64,
                tool_calls_upserted: i64,
            }

            (
                StatusCode::OK,
                Json(IngestOk {
                    executions_upserted: result.executions_upserted,
                    model_calls_upserted: result.model_calls_upserted,
                    tool_calls_upserted: result.tool_calls_upserted,
                }),
            )
                .into_response()
        }
        Err(reason) => {
            state
                .metrics
                .ingest_requests_total
                .with_label_values(&["error"])
                .inc();
            tracing::warn!(%reason, "ingest: rejected");

            let (status, body) = match reason {
                RejectReason::BadSharedSecret
                | RejectReason::MissingTenantHeader
                | RejectReason::MissingIntegrationHeader
                | RejectReason::MissingProviderHeader => (
                    StatusCode::UNAUTHORIZED,
                    ErrorBody {
                        error: "unauthorized",
                    },
                ),
                RejectReason::UnsupportedProvider | RejectReason::MalformedOtlp => (
                    StatusCode::BAD_REQUEST,
                    ErrorBody {
                        error: "bad request",
                    },
                ),
                // A storage failure is transient: the OTLP exporter retries on
                // 5xx/429, but a 4xx means permanent drop. Cost accounting
                // cannot silently lose a payload, so this must be retryable.
                RejectReason::IngestFailed => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    ErrorBody {
                        error: "service unavailable",
                    },
                ),
                RejectReason::RateLimited => (
                    StatusCode::TOO_MANY_REQUESTS,
                    ErrorBody {
                        error: "rate limited",
                    },
                ),
            };

            (status, Json(body)).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn headers_with_token(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("x-internal-token", HeaderValue::from_str(token).unwrap());
        headers
    }

    #[test]
    fn shared_secret_check_accepts_the_correct_token() {
        assert!(shared_secret_is_valid(
            &headers_with_token("correct-horse-battery-staple"),
            "correct-horse-battery-staple"
        ));
    }

    #[test]
    fn shared_secret_check_rejects_a_wrong_token() {
        assert!(!shared_secret_is_valid(
            &headers_with_token("wrong"),
            "correct-horse-battery-staple"
        ));
    }

    #[test]
    fn shared_secret_check_rejects_a_missing_header() {
        assert!(!shared_secret_is_valid(
            &HeaderMap::new(),
            "correct-horse-battery-staple"
        ));
    }

    /// A state whose pool is never actually queried: every rejection reason in
    /// this test module happens before `ingest_telemetry` touches the DB, so a
    /// lazy pool that cannot connect proves we never reached the DB (fail
    /// before the unavailable branch can become permissive).
    ///
    /// `acquire_timeout` is set short (not sqlx's 30s default) so the
    /// ordering proof stays deterministic on any host: whether port 1 refuses
    /// the connection or silently drops the SYN, the acquire fails in
    /// milliseconds, keeping the rate-limit test inside its 60s window.
    fn unreachable_state() -> IngestState {
        IngestState {
            pool: cratestack::sqlx::postgres::PgPoolOptions::new()
                .acquire_timeout(std::time::Duration::from_millis(200))
                .connect_lazy("postgres://x:x@127.0.0.1:1/does-not-matter")
                .expect("lazy pool construction never actually connects"),
            internal_token: Arc::from("the-real-token"),
            rate_limiter: Arc::new(RateLimiter::new(1_000, 60)),
            metrics: Arc::new(Metrics::new()),
        }
    }

    fn full_headers(token: &str) -> HeaderMap {
        let mut headers = headers_with_token(token);
        headers.insert(
            "governance.tenant.id",
            HeaderValue::from_str("tenant-1").unwrap(),
        );
        headers.insert(
            "governance.integration.id",
            HeaderValue::from_str("integration-1").unwrap(),
        );
        headers.insert(
            "governance.source",
            HeaderValue::from_str("claude_code").unwrap(),
        );
        headers
    }

    fn valid_otlp() -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "resourceSpans": [{
                "resource": {
                    "attributes": [
                        { "key": "user.email", "value": { "stringValue": "user@example.com" } }
                    ]
                },
                "scopeSpans": [{
                    "spans": [{
                        "traceId": "trace-123",
                        "spanId": "span-456",
                        "startTimeUnixNano": "1700000000000000000",
                        "endTimeUnixNano": "1700000005000000000",
                        "attributes": [
                            { "key": "session.id", "value": { "stringValue": "session-789" } },
                            { "key": "model.name", "value": { "stringValue": "claude-3-sonnet" } },
                            { "key": "tokens.input", "value": { "intValue": "1000" } },
                            { "key": "tokens.output", "value": { "intValue": "500" } }
                        ]
                    }]
                }]
            }]
        }))
        .expect("valid otlp json")
    }

    #[tokio::test]
    async fn rejects_a_wrong_shared_secret_before_touching_the_body_or_db() {
        let state = unreachable_state();
        let result = handle(&state, &full_headers("wrong-token"), b"not even json").await;
        assert_eq!(result, Err(RejectReason::BadSharedSecret));
    }

    #[tokio::test]
    async fn rejects_a_missing_tenant_header() {
        let state = unreachable_state();
        let mut headers = headers_with_token("the-real-token");
        headers.remove("governance.tenant.id");
        let result = handle(&state, &headers, &valid_otlp()).await;
        assert_eq!(result, Err(RejectReason::MissingTenantHeader));
    }

    #[tokio::test]
    async fn rejects_an_unsupported_provider() {
        let state = unreachable_state();
        let mut headers = headers_with_token("the-real-token");
        headers.insert(
            "governance.tenant.id",
            HeaderValue::from_str("tenant-1").unwrap(),
        );
        headers.insert(
            "governance.integration.id",
            HeaderValue::from_str("integration-1").unwrap(),
        );
        headers.insert(
            "governance.source",
            HeaderValue::from_str("github_copilot").unwrap(),
        );
        let result = handle(&state, &headers, &valid_otlp()).await;
        assert_eq!(result, Err(RejectReason::UnsupportedProvider));
    }

    #[tokio::test]
    async fn rejects_malformed_otlp() {
        let state = unreachable_state();
        let result = handle(&state, &full_headers("the-real-token"), b"not json").await;
        assert_eq!(result, Err(RejectReason::MalformedOtlp));
    }

    #[tokio::test]
    async fn rejects_an_empty_payload() {
        let state = unreachable_state();
        let result = handle(
            &state,
            &full_headers("the-real-token"),
            br#"{"resourceSpans":[]}"#,
        )
        .await;
        assert_eq!(result, Err(RejectReason::MalformedOtlp));
    }

    #[tokio::test]
    async fn rate_limits_after_the_window_is_exhausted() {
        let state = IngestState {
            rate_limiter: Arc::new(RateLimiter::new(1, 60)),
            ..unreachable_state()
        };
        // First call passes the limiter, then fails at the DB (which is
        // unreachable) -- proving the limiter ran before persistence.
        let first = handle(&state, &full_headers("the-real-token"), &valid_otlp()).await;
        assert_eq!(first, Err(RejectReason::IngestFailed));
        // Second call is throttled before the DB is touched.
        let second = handle(&state, &full_headers("the-real-token"), &valid_otlp()).await;
        assert_eq!(second, Err(RejectReason::RateLimited));
    }
}
