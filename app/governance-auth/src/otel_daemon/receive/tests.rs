//! Tests for the receive-path admission checks: `Host`/`Content-Type`
//! (#290 review, P1-2), and the body-size cap. Split out of `mod.rs` purely
//! for the LoC ceiling.

use axum::{
    body::Body,
    http::{Method, Request as AxumRequest},
};

use super::*;

/// A request builder pre-loaded with the two headers a real client
/// always sends: the trusted `Host` and a supported `Content-Type`. Every
/// test below that is not itself about one of those two checks starts
/// from this, so it exercises exactly the property under test.
fn trusted(method: Method, path: &str, body: impl Into<Body>) -> axum::extract::Request {
    AxumRequest::builder()
        .method(method)
        .uri(path)
        .header("host", TRUSTED_HOSTS[0])
        .header("content-type", "application/json")
        .body(body.into())
        .expect("build request")
}

#[tokio::test]
async fn any_path_is_carried_not_rejected() {
    for (method, path) in [
        (Method::POST, "/"),
        (Method::POST, "/garbage"),
        (Method::POST, "/v1/logs"),
        (Method::POST, "/v1/metrics"),
        (Method::POST, "/anything"),
    ] {
        let request = trusted(method.clone(), path, r#"{"resourceLogs":[{}]}"#);
        let incoming = build(request).await.ok().expect("any path is accepted");
        assert_eq!(incoming.path, path, "path must be carried, not rejected");
        assert_eq!(incoming.method, method.as_str());
        assert!(!incoming.body.is_empty());
    }
}

#[tokio::test]
async fn the_body_is_captured_verbatim() {
    let body = r#"{"resourceLogs":[{"resource":{"attributes":[{"key":"a","value":{"stringValue":"b"}}]}}]}"#;
    let incoming = build(trusted(Method::POST, "/", body))
        .await
        .ok()
        .expect("accept");
    assert_eq!(String::from_utf8_lossy(&incoming.body), body);
}

#[tokio::test]
async fn a_body_over_the_cap_is_an_error() {
    // A request larger than MAX_BODY_SIZE must fail to buffer, so the
    // caller can answer 413 rather than OOM or overflowing the spool.
    let too_big = vec![b' '; MAX_BODY_SIZE + 1];
    assert!(
        matches!(
            build(trusted(Method::POST, "/", too_big)).await,
            Err(ReceiveError::Body(_))
        ),
        "over-cap body must error as Body, not another variant"
    );
}

/// The #290 review finding this module exists to close: no `Host` at all
/// (the shape a same-origin-policy-blind script would send) must be
/// refused, not defaulted to trusted.
#[tokio::test]
async fn a_request_with_no_host_header_is_refused() {
    let request = AxumRequest::builder()
        .method(Method::POST)
        .uri("/")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"resourceLogs":[{}]}"#))
        .expect("build request");
    assert!(
        matches!(build(request).await, Err(ReceiveError::UntrustedHost)),
        "no Host header must be untrusted, not accepted"
    );
}

/// The exact reproduction from the review: an attacker-controlled domain
/// in `Host` (what a DNS-rebinding attack would present after the rebind)
/// must be refused.
#[tokio::test]
async fn a_rebound_host_is_refused() {
    let mut request = trusted(Method::POST, "/", r#"{"resourceLogs":[{}]}"#);
    request.headers_mut().insert(
        header::HOST,
        "attacker.rebound.example:17457".parse().unwrap(),
    );
    assert!(
        matches!(build(request).await, Err(ReceiveError::UntrustedHost)),
        "a Host naming any domain but this daemon's own must be refused"
    );
}

#[tokio::test]
async fn localhost_is_trusted_as_well_as_the_literal_loopback_address() {
    let mut request = trusted(Method::POST, "/", r#"{"resourceLogs":[{}]}"#);
    request
        .headers_mut()
        .insert(header::HOST, TRUSTED_HOSTS[1].parse().unwrap());
    assert!(build(request).await.is_ok());
}

/// The other half of the review finding: a CORS *simple* content type
/// (what a cross-origin `fetch(..., {mode:'no-cors'})` is limited to, so
/// no preflight and no `Origin` check ever engages) must be refused, not
/// silently accepted as if it were OTLP JSON.
#[tokio::test]
async fn a_cors_simple_content_type_is_refused() {
    for simple in [
        "text/plain",
        "multipart/form-data",
        "application/x-www-form-urlencoded",
    ] {
        let mut request = trusted(Method::POST, "/", r#"{"resourceLogs":[{}]}"#);
        request
            .headers_mut()
            .insert(header::CONTENT_TYPE, simple.parse().unwrap());
        assert!(
            matches!(
                build(request).await,
                Err(ReceiveError::UnsupportedContentType)
            ),
            "{simple} is CORS-simple and must be refused"
        );
    }
}

#[tokio::test]
async fn a_charset_parameter_does_not_defeat_the_content_type_check() {
    let mut request = trusted(Method::POST, "/", r#"{"resourceLogs":[{}]}"#);
    request.headers_mut().insert(
        header::CONTENT_TYPE,
        "application/json; charset=utf-8".parse().unwrap(),
    );
    assert!(build(request).await.is_ok());
}

#[tokio::test]
async fn protobuf_content_type_is_accepted() {
    let mut request = trusted(Method::POST, "/", vec![0x0a, 0x03]);
    request.headers_mut().insert(
        header::CONTENT_TYPE,
        "application/x-protobuf".parse().unwrap(),
    );
    assert!(build(request).await.is_ok());
}
