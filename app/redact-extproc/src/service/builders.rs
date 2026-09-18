//! Response/refusal builders shared by the request and response handlers.
//!
//! Split out of `service.rs` (issue #180) along the "build the Envoy reply"
//! seam: the small helpers that assemble a `ProcessingResponse` — a
//! `Continue`, an `ImmediateResponse` refusal, or a body mutation — plus the
//! metric-recording and fail-closed decision helpers the handlers call.

use envoy_types::pb::envoy::{
    config::core::v3::{HeaderValue, HeaderValueOption},
    service::ext_proc::v3::{
        BodyMutation, BodyResponse, CommonResponse, HeaderMutation, ImmediateResponse,
        ProcessingResponse, body_mutation, common_response::ResponseStatus,
        processing_response::Response as Resp,
    },
    r#type::v3::{HttpStatus, StatusCode},
};
use governance_redact::{Engine, ScanReport};

use super::{Direction, continue_headers};
use crate::metrics::Metrics;

pub(crate) fn record(metrics: &Metrics, report: &ScanReport) {
    if report.redactions > 0 {
        metrics.redactions_total.inc_by(report.redactions as u64);
    }
    metrics
        .scanned_fields_total
        .inc_by(report.scanned_fields as u64);
}

/// Applies the profile's `fail_closed` rule to an indeterminate result.
pub(crate) fn refuse_or_block(
    dir: Direction,
    engine: &Engine,
    metrics: &Metrics,
    reason: &str,
) -> ProcessingResponse {
    if engine.profile().fail_closed {
        metrics.refused_total.inc();
        tracing::error!(reason, "failing closed");
        return immediate_response(
            dir,
            StatusCode::BadGateway,
            "redaction_unavailable",
            &format!("request refused: redaction could not be completed ({reason})"),
        );
    }
    metrics.fail_open_total.inc();
    tracing::warn!(
        reason,
        "redaction indeterminate on a non-fail-closed profile; continuing"
    );
    continue_headers(dir)
}

pub(crate) fn immediate_response(
    _dir: Direction,
    status: StatusCode,
    code: &str,
    message: &str,
) -> ProcessingResponse {
    // `ImmediateResponse` is direction-agnostic in the proto (it aborts and
    // replaces the whole HTTP exchange regardless of which body direction
    // triggered it), so `_dir` is accepted for symmetry with the other
    // helpers but unused here.
    let body = serde_json::json!({
        "error": { "message": message, "type": code, "code": code }
    })
    .to_string();

    ProcessingResponse {
        response: Some(Resp::ImmediateResponse(ImmediateResponse {
            status: Some(HttpStatus {
                code: status as i32,
            }),
            headers: Some(HeaderMutation {
                set_headers: vec![HeaderValueOption {
                    header: Some(HeaderValue {
                        key: "content-type".into(),
                        value: "application/json".into(),
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            body: body.into_bytes(),
            grpc_status: None,
            details: message.to_string(),
        })),
        ..Default::default()
    }
}

pub(crate) fn body_response(dir: Direction, bytes: Vec<u8>) -> ProcessingResponse {
    body_response_with_original_length(dir, bytes, None)
}

/// Builds a body response with optional Content-Length header mutation.
///
/// When `original_length` is `Some(old_len)` and `bytes.len()` (the new length)
/// differs from `old_len`, this includes a HeaderMutation that **removes** the
/// Content-Length header. Envoy v1.32 strips Content-Length to an empty value
/// when a body mutation changes the length, and an empty Content-Length causes
/// HTTP/2 upstreams to RST_STREAM with PROTOCOL_ERROR and HTTP/1.1 upstreams to
/// misframe the body. Removing the header entirely lets Envoy frame the
/// mutated body correctly: chunked transfer encoding over HTTP/1.1, DATA frames
/// over HTTP/2 — neither of which needs Content-Length.
///
/// Setting Content-Length to the new value (via `OverwriteIfExistsOrAdd`) was
/// the first attempt, but Envoy v1.32's ext_proc filter empties it *after*
/// applying the header mutation, so the overwrite never reaches the wire. The
/// `allow_content_length_header` config field that would prevent this was
/// added in Envoy v1.33+ and does not exist in v1.32.
pub(crate) fn body_response_with_original_length(
    dir: Direction,
    bytes: Vec<u8>,
    original_length: Option<usize>,
) -> ProcessingResponse {
    let header_mutation = original_length.and_then(|old_len| {
        if bytes.len() == old_len {
            return None;
        }
        Some(HeaderMutation {
            set_headers: Vec::new(),
            remove_headers: vec!["content-length".into()],
        })
    });

    let common = CommonResponse {
        status: ResponseStatus::Continue as i32,
        header_mutation,
        body_mutation: Some(BodyMutation {
            mutation: Some(body_mutation::Mutation::Body(bytes)),
        }),
        ..Default::default()
    };
    let response = match dir {
        Direction::Request => Resp::RequestBody(BodyResponse {
            response: Some(common),
        }),
        Direction::Response => Resp::ResponseBody(BodyResponse {
            response: Some(common),
        }),
    };
    ProcessingResponse {
        response: Some(response),
        ..Default::default()
    }
}
