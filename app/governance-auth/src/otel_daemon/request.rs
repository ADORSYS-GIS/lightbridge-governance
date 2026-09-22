//! Per-request handling for the local collector daemon, split out of `mod.rs` (LoC gate):
//! receive -> classify -> enrich -> durable admission. Daemon lifecycle/startup stays in `mod.rs`.

use axum::{
    extract::State,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};

use super::{DaemonState, classify, codex_cost, drain, receive, signal, source_stamp};

/// Handles one OTLP request: receive -> classify -> durable admission.
///
/// Forwarding belongs exclusively to the background drain. Keeping the
/// network out of this handler makes the acknowledgement precise: `200`
/// means this daemon has durably accepted custody, independent of the online
/// collector's latency or current verdict. OTLP defines `200`, rather than
/// HTTP's asynchronous `202`, as its full-success response.
pub(super) async fn handle_request(
    State(state): State<DaemonState>,
    request: axum::extract::Request,
) -> Response {
    // Admission FIRST: `receive::build`'s `Host`/`Content-Type` checks make
    // an untrusted request free because no disk or credentialed work runs
    // before them.
    let incoming = match receive::build(request).await {
        Ok(incoming) => incoming,
        Err(receive::ReceiveError::UntrustedHost) => {
            tracing::warn!("refusing a request with an untrusted Host header");
            return StatusCode::FORBIDDEN.into_response();
        }
        Err(receive::ReceiveError::UnsupportedContentType) => {
            return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
        }
        Err(receive::ReceiveError::Body(error)) => {
            tracing::warn!(error = %error, "could not read the request body");
            return StatusCode::PAYLOAD_TOO_LARGE.into_response();
        }
    };

    // Path is diagnostic metadata and the explicit OTLP signal discriminator.
    tracing::trace!(method = %incoming.method, path = %incoming.path, "received OTLP");
    // Classification is the only inspection needed at admission. Identity
    // stamping happens when the drain forwards the retained bytes.
    let Some(signal) = classify::signal(&incoming.body, incoming.format, &incoming.path) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let body = if signal == signal::Signal::Logs {
        // Source stamping needs no bearer, so it runs here rather than
        // waiting for drain/forward -- see source_stamp's module doc for why
        // this admission point (not the public collector) is where it is
        // safe to derive `governance.source` from `event.name`.
        codex_cost::enrich(
            &source_stamp::enrich(&incoming.body, incoming.format),
            incoming.format,
        )
    } else {
        incoming.body
    };
    retained_response(&state, signal, body, incoming.format).await
}

/// Retains `payload` and answers what actually happened: an OTLP full-success
/// response when it is durably queued, `503` when the spool could not retain
/// it. The success body is the empty ExportLogsServiceResponse /
/// ExportMetricsServiceResponse encoding: `{}` for JSON, zero bytes for
/// protobuf, with the same content type the sender used as OTLP requires.
async fn retained_response(
    state: &DaemonState,
    signal: signal::Signal,
    payload: Vec<u8>,
    format: receive::WireFormat,
) -> Response {
    if drain::retain(state, signal, payload, format).await {
        let content_type = [(header::CONTENT_TYPE, format.content_type())];
        match format {
            receive::WireFormat::Json => (StatusCode::OK, content_type, "{}").into_response(),
            receive::WireFormat::Protobuf => {
                (StatusCode::OK, content_type, Vec::<u8>::new()).into_response()
            }
        }
    } else {
        // Spool capacity is backpressure, not a permanent payload verdict.
        // Give an exporter a concrete floor for retry instead of inviting a
        // tight loop while the drain is already stalled.
        (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::RETRY_AFTER, "5")],
        )
            .into_response()
    }
}
