//! Parses an incoming OTLP request: method, path, body — after two admission
//! checks that run *before* the body is ever read.
//!
//! ## "Any path" is the point (A2)
//!
//! Codex posts OTLP to its configured endpoint **verbatim**, appending no
//! signal path (`POST /` with a `resourceLogs` body, measured). So this
//! receiver must not reject a path it does not recognise — the path is carried
//! along, not branch-decisioned here. Admission for forwarding never depends
//! on the URL path; the body discriminates the signal ([`super::classify`]).
//!
//! ## Why `Host` and `Content-Type` ARE admission gates, unlike the path
//!
//! Reviewed finding (#268/#290): `Router::new().fallback(any(handle_request))`
//! with no `Host`/`Origin` check meant **any web page the developer's browser
//! visited could POST here**. A `POST` with `content-type: text/plain` is a
//! CORS *simple request* — no preflight, `fetch(url, {mode:'no-cors'})` is
//! enough — so the browser's same-origin policy never engaged at all.
//! ADR-0016's "why no local authentication" argument is about local
//! *processes* ("a secret readable by the client is readable by anything
//! running as the developer"); a remote web origin reachable through the
//! developer's own browser is not a process on the machine, and a
//! `Host`-header rebind is not covered by filesystem permissions either. This
//! was outside the risk ADR-0016 accepted, not inside it.
//!
//! [`host_is_trusted`] rejects unless `Host` is exactly this daemon's own
//! loopback endpoint (the standard DNS-rebinding defence: an attacker-owned
//! domain cannot ever resolve to that literal string). [`content_type_is_otlp`]
//! rejects unless the body claims to be one of the two wire formats this
//! daemon actually forwards, which are both CORS *non-simple* — a browser
//! sending either must preflight, and the preflight has no `Origin` this
//! daemon replies to (no CORS headers are ever set), so it is refused before
//! the real request is even attempted. Both checks run before the body is
//! read, so an untrusted request costs nothing.

use anyhow::{Context, Result};
use axum::http::{HeaderMap, header};

use crate::otel_port::TRUSTED_HOSTS;

/// The maximum request body the receiver will read. Mirrors
/// [`super::spool::MAX_RETAINABLE_PAYLOAD`], **not** [`super::spool::CAPACITY`]
/// directly (#269/#291 review, P2-5): a body sized to `CAPACITY` itself
/// base64-encodes larger than `CAPACITY` once it reaches the spool, and
/// larger still than `copilot::spool::MAX_READ` -- either way it could never
/// actually be retained, on any spool, empty or not. A larger request is
/// refused with 413 rather than buffered into memory and then refused at the
/// spool regardless.
pub const MAX_BODY_SIZE: usize = super::spool::MAX_RETAINABLE_PAYLOAD;

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

/// Why a request was refused, so the caller can answer with the status that
/// actually explains it rather than folding every failure into 413 (P3-7 in
/// the #290 review).
pub enum ReceiveError {
    /// `Host` did not name this daemon's own loopback endpoint.
    UntrustedHost,
    /// `Content-Type` was not one of the two OTLP wire formats this daemon
    /// forwards.
    UnsupportedContentType,
    /// The body could not be read, including exceeding [`MAX_BODY_SIZE`].
    Body(anyhow::Error),
}

/// Extracts method + path + body from an axum request, after the `Host` and
/// `Content-Type` admission checks in the module doc. Both run before the
/// body is touched.
pub async fn build(request: axum::extract::Request) -> Result<Incoming, ReceiveError> {
    if !host_is_trusted(request.headers()) {
        return Err(ReceiveError::UntrustedHost);
    }
    if !content_type_is_otlp(request.headers()) {
        return Err(ReceiveError::UnsupportedContentType);
    }

    let method = request.method().to_string();
    let path = request.uri().path().to_string();
    let bytes = axum::body::to_bytes(request.into_body(), MAX_BODY_SIZE)
        .await
        .context("reading the OTLP request body")
        .map_err(ReceiveError::Body)?;
    Ok(Incoming::new(&method, &path, bytes.to_vec()))
}

/// `Host` must be exactly this daemon's own loopback endpoint (`127.0.0.1
/// :17457`, or `localhost:17457` for a client that resolves that name to
/// loopback itself) — never absent, never anything a DNS record could ever be
/// made to answer for. Case-insensitive: the hostname half of `Host` is
/// case-insensitive per RFC 9110 §7.2, and a client is free to send either
/// case for `localhost`.
fn host_is_trusted(headers: &HeaderMap) -> bool {
    let Some(host) = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let host = host.trim().to_ascii_lowercase();
    TRUSTED_HOSTS.iter().any(|trusted| host == *trusted)
}

/// `Content-Type` must be one of the two OTLP wire formats this daemon
/// forwards. Both are CORS *non-simple* — the module doc explains why that
/// matters. The three CORS-simple content types
/// (`application/x-www-form-urlencoded`, `multipart/form-data`,
/// `text/plain`) were never valid OTLP anyway, so this is a real validation
/// as well as a defence.
fn content_type_is_otlp(headers: &HeaderMap) -> bool {
    let Some(content_type) = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    // Strip a `; charset=...` (or any other) parameter before comparing --
    // `application/json; charset=utf-8` is the same media type as
    // `application/json`.
    let essence = content_type
        .split(';')
        .next()
        .unwrap_or(content_type)
        .trim()
        .to_ascii_lowercase();
    essence == "application/json" || essence == "application/x-protobuf"
}

#[cfg(test)]
mod tests;
