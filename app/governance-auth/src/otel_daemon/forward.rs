//! The authenticated outbound OTLP/HTTP POST (A3/A4).
//!
//! Reuses [`crate::copilot`]'s signal/verdict taxonomy and endpoint builder so
//! the daemon cannot drift from `copilot push` on what an endpoint looks like
//! or which statuses are permanent. The `Redacted` discipline is structural:
//! **never log the bearer or a body.**

use anyhow::{Context, Result, bail};

pub use crate::copilot::{Signal, Verdict};
use crate::{config::OauthConfig, copilot, redacted::Redacted};

/// Posts one payload to the governed collector for the given signal.
///
/// `is_json` names the wire format rather than this function re-parsing
/// `payload` to sniff it (#290 review round 2 -- this was the third of three
/// redundant `serde_json::from_slice` calls over the same body in one
/// request). It is sound because [`super::normalize::stamp`] never changes a
/// body's format: a JSON payload (possibly identity-stamped) goes out as
/// JSON; anything else (OTLP protobuf passthrough) goes out as protobuf, so
/// "was the body JSON before stamping" and "is the stamped payload JSON"
/// always agree.
///
/// A retryable failure is an `Err`; a permanent refusal is a
/// [`Verdict::Refused`]. Both tell the caller to retain the payload; neither
/// ever forwards unauthenticated or logs the bearer/body.
pub async fn post(
    http: &reqwest::Client,
    config: &OauthConfig,
    bearer: &Redacted<String>,
    signal: Signal,
    payload: &[u8],
    is_json: bool,
) -> Result<Verdict> {
    let base = config.otel_endpoint.as_deref().context(
        "no collector configured: supply --otel-endpoint / GOVERNANCE_AUTH_OTEL_ENDPOINT before \
         running `serve --otel`",
    )?;
    let url = copilot::endpoint(base, signal);
    let content_type = if is_json {
        "application/json"
    } else {
        "application/x-protobuf"
    };
    let response = http
        .post(&url)
        .header("content-type", content_type)
        .header("authorization", format!("Bearer {}", bearer.expose()))
        .body(payload.to_vec())
        .send()
        .await
        .with_context(|| format!("posting {signal} to {url}"))?;

    let status = response.status();
    if status.is_success() {
        return Ok(Verdict::Accepted);
    }
    if copilot::is_permanent(status) {
        return Ok(Verdict::Refused(status));
    }
    // Body deliberately omitted — see copilot::push's module doc (a collector's
    // 4xx body can echo the submitted payload, which is prompt-adjacent).
    bail!("the collector rejected the {signal} export at {url} with HTTP {status}");
}
