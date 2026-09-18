//! Tests for [`super`]. Split into per-area files (issue #180) to keep every
//! file under the 200-LoC ceiling, mirroring the `metrics` split. This file
//! holds the shared helpers; the tests themselves live in the submodules.

mod buffered;
mod builders;
mod dispatch;
mod sse;
mod utf8;

use envoy_types::pb::envoy::config::core::v3::HeaderMap;
use governance_redact::Profile;

use super::*;

fn engine() -> Engine {
    Engine::new(Profile::coding_assistant(), "test-salt").expect("engine")
}

fn metrics() -> Metrics {
    Metrics::new().expect("metrics")
}

/// Synthesizes the `ResponseHeaders` message Envoy sends before any
/// `ResponseBody` chunk, carrying one `Content-Type` value, and applies
/// it the way `process()`'s message loop does — so tests exercise the
/// real routing decision (`ResponseState::set_mode_from_headers`)
/// instead of relying on `ResponseState::new`'s default.
fn response_state_with_content_type(window: usize, content_type: &str) -> ResponseState {
    let mut state = ResponseState::new(window);
    state.set_mode_from_headers(&HttpHeaders {
        headers: Some(HeaderMap {
            headers: vec![HeaderValue {
                key: "content-type".to_string(),
                value: content_type.to_string(),
                raw_value: Vec::new(),
            }],
        }),
        attributes: std::collections::HashMap::new(),
        end_of_stream: false,
    });
    state
}

/// Like `response_state_with_content_type`, but also sets Content-Encoding
/// so the `is_sse && is_gzip` branch of `set_mode_from_headers` engages.
fn response_state_with_content_type_and_encoding(
    window: usize,
    content_type: &str,
    content_encoding: &str,
) -> ResponseState {
    let mut state = ResponseState::new(window);
    state.set_mode_from_headers(&HttpHeaders {
        headers: Some(HeaderMap {
            headers: vec![
                HeaderValue {
                    key: "content-type".to_string(),
                    value: content_type.to_string(),
                    raw_value: Vec::new(),
                },
                HeaderValue {
                    key: "content-encoding".to_string(),
                    value: content_encoding.to_string(),
                    raw_value: Vec::new(),
                },
            ],
        }),
        attributes: std::collections::HashMap::new(),
        end_of_stream: false,
    });
    state
}

fn extract_body(resp: &ProcessingResponse) -> Option<Vec<u8>> {
    let Some(Resp::ResponseBody(BodyResponse {
        response:
            Some(CommonResponse {
                body_mutation:
                    Some(BodyMutation {
                        mutation: Some(body_mutation::Mutation::Body(bytes)),
                    }),
                ..
            }),
    })) = &resp.response
    else {
        return None;
    };
    Some(bytes.clone())
}
