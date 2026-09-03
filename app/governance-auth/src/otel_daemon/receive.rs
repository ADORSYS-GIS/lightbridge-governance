//! Parses an incoming OTLP request: method, path, body.
//!
//! ## "Any path" is the point (A2)
//!
//! Codex posts OTLP to its configured endpoint **verbatim**, appending no
//! signal path (`POST /` with a `resourceLogs` body, measured). So this
//! receiver must not reject a path it does not recognise — the path is carried
//! along, not branch-decisioned here. Admission for forwarding never depends
//! on the URL path; the body discriminates the signal ([`super::classify`]).

use anyhow::{Context, Result};

/// The maximum request body the receiver will read. Mirrors [`super::spool`]'s
/// [`super::spool::CAPACITY`]; a larger request is refused with 413 rather than
/// buffered into memory and then refused at the spool.
pub const MAX_BODY_SIZE: usize = super::spool::CAPACITY;

/// What the receiver parsed out of the raw request.
pub struct Incoming {
    pub method: String,
    pub path: String,
    pub body: Vec<u8>,
}

impl Incoming {
    fn new(method: &str, path: &str, body: Vec<u8>) -> Self {
        Self {
            method: method.to_owned(),
            path: path.to_owned(),
            body,
        }
    }
}

/// Extracts method + path + body from an axum request.
///
/// The path is carried along, never validated for admission — see the module
/// doc. A body over [`MAX_BODY_SIZE`] is an `Err` so the caller can answer 413.
pub async fn build(request: axum::extract::Request) -> Result<Incoming> {
    let method = request.method().to_string();
    let path = request.uri().path().to_string();
    let bytes = axum::body::to_bytes(request.into_body(), MAX_BODY_SIZE)
        .await
        .context("reading the OTLP request body")?;
    Ok(Incoming::new(&method, &path, bytes.to_vec()))
}

#[cfg(test)]
mod tests {
    use axum::{
        body::Body,
        http::{Method, Request as AxumRequest},
    };

    use super::*;

    #[tokio::test]
    async fn any_path_is_carried_not_rejected() {
        for (method, path) in [
            (Method::POST, "/"),
            (Method::POST, "/garbage"),
            (Method::POST, "/v1/logs"),
            (Method::POST, "/v1/metrics"),
            (Method::POST, "/anything"),
        ] {
            let request = AxumRequest::builder()
                .method(method.clone())
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(r#"{"resourceLogs":[{}]}"#))
                .expect("build request");
            let incoming = build(request).await.expect("any path is accepted");
            assert_eq!(incoming.path, path, "path must be carried, not rejected");
            assert_eq!(incoming.method, method.as_str());
            assert!(!incoming.body.is_empty());
        }
    }

    #[tokio::test]
    async fn the_body_is_captured_verbatim() {
        let body = r#"{"resourceLogs":[{"resource":{"attributes":[{"key":"a","value":{"stringValue":"b"}}]}}]}"#;
        let request = AxumRequest::builder()
            .method(Method::POST)
            .uri("/")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .expect("build request");
        let incoming = build(request).await.expect("accept");
        assert_eq!(String::from_utf8_lossy(&incoming.body), body);
    }

    #[tokio::test]
    async fn a_body_over_the_cap_is_an_error() {
        // A request larger than MAX_BODY_SIZE must fail to buffer, so the
        // caller can answer 413 rather than OOM or overflowing the spool.
        let too_big = vec![b' '; MAX_BODY_SIZE + 1];
        let request = AxumRequest::builder()
            .method(Method::POST)
            .uri("/")
            .header("content-type", "application/json")
            .body(Body::from(too_big))
            .expect("build request");
        assert!(build(request).await.is_err(), "over-cap body must error");
    }
}
