//! The proxy handler.
//!
//! One path in, one path out:
//!
//! ```text
//! client -> scan request -> upstream provider -> scan response -> client
//! ```
//!
//! # Fail-closed
//!
//! Every branch that cannot reach a confident "this content is safe to
//! forward" rejects the request. That includes the boring ones — a body that
//! will not parse as JSON, a scan that errors, an upstream response we cannot
//! read. On a `fail_closed` profile an outage of the detector is an outage of
//! the gateway, deliberately: the alternative is forwarding content nobody
//! inspected, which is the failure this service exists to prevent.
//!
//! `observe-only` is the one profile that does not fail closed, and it makes no
//! promises in exchange.

use std::sync::Arc;

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use governance_redact::{Engine, ScanReport, scan_request, scan_response, scan_sse};
use serde_json::Value;

use crate::metrics::Metrics;

/// Shared handler state.
pub struct AppState {
    pub engine: Engine,
    pub client: reqwest::Client,
    pub provider_base_url: String,
    pub metrics: Metrics,
}

/// Why a request was refused.
enum Refusal {
    /// Content must not leave. Carries entity TYPES only, never values.
    Blocked(Vec<String>),
    /// We could not determine whether the content was safe.
    Undetermined(String),
}

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            Self::Blocked(entities) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "content_blocked",
                format!(
                    "request blocked: content matched a prohibited category ({})",
                    entities.join(", ")
                ),
            ),
            Self::Undetermined(why) => (
                StatusCode::BAD_GATEWAY,
                "redaction_unavailable",
                format!("request refused: redaction could not be completed ({why})"),
            ),
        };

        // OpenAI-shaped error body, so a client's existing error handling works.
        let body = serde_json::json!({
            "error": { "message": message, "type": code, "code": code }
        });
        (status, axum::Json(body)).into_response()
    }
}

/// Proxies one OpenAI-compatible request.
pub async fn handle(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    path: axum::extract::OriginalUri,
    body: Bytes,
) -> Response {
    let fail_closed = state.engine.profile().fail_closed;
    state.metrics.requests_total.inc();

    // ── Request side ────────────────────────────────────────────────────────
    let mut json: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            // A body we cannot parse is a body we cannot inspect.
            return refuse(
                &state,
                fail_closed,
                Refusal::Undetermined(format!("request body is not JSON: {e}")),
            );
        }
    };

    let streaming = json.get("stream").and_then(Value::as_bool).unwrap_or(false);

    match scan_request(&state.engine, &mut json) {
        Ok(report) => {
            state.metrics.record(&report);
            if report.is_blocked() {
                state.metrics.blocked_total.inc();
                tracing::warn!(
                    entities = ?report.blocked,
                    "blocked request: prohibited content"
                );
                return Refusal::Blocked(report.blocked).into_response();
            }
            if report.scanned_fields == 0 {
                // Not an error, but it means the request went through
                // uninspected — the shape was one we do not know.
                state.metrics.uninspected_total.inc();
                tracing::warn!("request body had no recognised text fields; forwarded uninspected");
            }
        }
        Err(e) => {
            return refuse(
                &state,
                fail_closed,
                Refusal::Undetermined(format!("request scan failed: {e}")),
            );
        }
    }

    // ── Upstream ────────────────────────────────────────────────────────────
    let url = format!("{}{}", state.provider_base_url, path.0.path());
    let upstream = state
        .client
        .post(&url)
        .headers(forwarded_headers(&headers))
        .json(&json)
        .send()
        .await;

    let upstream = match upstream {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "upstream request failed");
            return Refusal::Undetermined(format!("upstream unavailable: {e}")).into_response();
        }
    };

    let status = upstream.status();
    let content_type = upstream
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();

    let upstream_body = match upstream.text().await {
        Ok(b) => b,
        Err(e) => {
            return refuse(
                &state,
                fail_closed,
                Refusal::Undetermined(format!("could not read upstream response: {e}")),
            );
        }
    };

    // A non-2xx upstream response is an error object, not model output. Pass it
    // through unmodified so the client sees the provider's own error.
    if !status.is_success() {
        return (status, upstream_body).into_response();
    }

    // ── Response side ───────────────────────────────────────────────────────
    if streaming || content_type.starts_with("text/event-stream") {
        return match scan_sse(&state.engine, &upstream_body) {
            Ok(outcome) => {
                state.metrics.record(&outcome.report);
                if outcome.report.is_blocked() {
                    state.metrics.blocked_total.inc();
                    tracing::warn!(
                        entities = ?outcome.report.blocked,
                        "blocked response stream: prohibited content"
                    );
                    return Refusal::Blocked(outcome.report.blocked).into_response();
                }
                (
                    status,
                    [(header::CONTENT_TYPE, "text/event-stream")],
                    outcome.body,
                )
                    .into_response()
            }
            Err(e) => refuse(
                &state,
                fail_closed,
                Refusal::Undetermined(format!("response stream scan failed: {e}")),
            ),
        };
    }

    let mut response_json: Value = match serde_json::from_str(&upstream_body) {
        Ok(v) => v,
        Err(e) => {
            return refuse(
                &state,
                fail_closed,
                Refusal::Undetermined(format!("upstream response is not JSON: {e}")),
            );
        }
    };

    match scan_response(&state.engine, &mut response_json) {
        Ok(report) => {
            state.metrics.record(&report);
            if report.is_blocked() {
                state.metrics.blocked_total.inc();
                tracing::warn!(
                    entities = ?report.blocked,
                    "blocked response: prohibited content"
                );
                return Refusal::Blocked(report.blocked).into_response();
            }
        }
        Err(e) => {
            return refuse(
                &state,
                fail_closed,
                Refusal::Undetermined(format!("response scan failed: {e}")),
            );
        }
    }

    (status, axum::Json(response_json)).into_response()
}

/// Applies a refusal, honouring the profile's fail-closed setting.
///
/// On an `observe-only` profile an undetermined result is logged and the
/// request proceeds — that profile promises nothing. On every other profile it
/// is refused.
fn refuse(state: &Arc<AppState>, fail_closed: bool, refusal: Refusal) -> Response {
    if fail_closed {
        state.metrics.refused_total.inc();
        if let Refusal::Undetermined(ref why) = refusal {
            tracing::error!(reason = %why, "failing closed");
        }
        return refusal.into_response();
    }

    state.metrics.fail_open_total.inc();
    tracing::warn!("redaction indeterminate on a non-fail-closed profile; continuing");
    (StatusCode::BAD_GATEWAY, "redaction unavailable").into_response()
}

/// The subset of client headers forwarded upstream.
///
/// `authorization` is forwarded so the upstream gateway performs its own
/// authentication exactly as it would without this proxy in the path — this
/// service authenticates nobody and deliberately holds no credential of its
/// own. Hop-by-hop headers and anything that would confuse the upstream about
/// body framing are dropped.
fn forwarded_headers(headers: &HeaderMap) -> HeaderMap {
    let mut out = HeaderMap::new();
    for name in [header::AUTHORIZATION, header::ACCEPT, header::USER_AGENT] {
        if let Some(v) = headers.get(&name) {
            out.insert(name, v.clone());
        }
    }
    // Propagate the caller's trace context so the upstream span joins the same
    // trace rather than starting a new one.
    for name in ["traceparent", "tracestate", "x-request-id"] {
        if let (Some(v), Ok(hn)) = (headers.get(name), header::HeaderName::try_from(name)) {
            out.insert(hn, v.clone());
        }
    }
    out
}

/// Records a report's counts. Kept here so both request and response paths
/// report identically.
impl Metrics {
    pub fn record(&self, report: &ScanReport) {
        if report.redactions > 0 {
            self.redactions_total.inc_by(report.redactions as u64);
        }
        self.scanned_fields_total
            .inc_by(report.scanned_fields as u64);
    }
}
