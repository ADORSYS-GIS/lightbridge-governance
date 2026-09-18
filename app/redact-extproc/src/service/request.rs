//! Request-body handling for the `ExternalProcessor` service.
//!
//! Split out of `service.rs` (issue #180) along the "request body" seam.
//! The request arrives whole under `Buffered` mode, so it is walked the same
//! way `redact-gateway`'s request path did.

use envoy_types::pb::envoy::{service::ext_proc::v3::ProcessingResponse, r#type::v3::StatusCode};
use governance_redact::{Engine, scan_request};

use super::{
    Direction, body_response_with_original_length, immediate_response, record, refuse_or_block,
};
use crate::metrics::Metrics;

/// Scans the whole (buffered) request body and decides what to tell Envoy.
pub(crate) fn handle_request_body(
    engine: &Engine,
    metrics: &Metrics,
    raw: &[u8],
) -> ProcessingResponse {
    let original_length = raw.len();
    let Ok(mut json) = serde_json::from_slice::<serde_json::Value>(raw) else {
        return refuse_or_block(
            Direction::Request,
            engine,
            metrics,
            "request body is not JSON",
        );
    };

    match scan_request(engine, &mut json) {
        Ok(report) => {
            record(metrics, &report);
            if report.is_blocked() {
                metrics.blocked_total.inc();
                tracing::warn!(entities = ?report.blocked, "blocked request: prohibited content");
                return immediate_response(
                    Direction::Request,
                    StatusCode::UnprocessableEntity,
                    "content_blocked",
                    &format!(
                        "request blocked: content matched a prohibited category ({})",
                        report.blocked.join(", ")
                    ),
                );
            }
            if report.scanned_fields == 0 {
                metrics.uninspected_total.inc();
                tracing::warn!(
                    "request body had no recognised text fields; forwarding uninspected"
                );
            }
            // Pass original_length so body_response can update Content-Length
            // if the body was redacted (length changed).
            body_response_with_original_length(
                Direction::Request,
                serde_json::to_vec(&json).unwrap_or_else(|_| raw.to_vec()),
                Some(original_length),
            )
        }
        Err(e) => refuse_or_block(
            Direction::Request,
            engine,
            metrics,
            &format!("request scan failed: {e}"),
        ),
    }
}
