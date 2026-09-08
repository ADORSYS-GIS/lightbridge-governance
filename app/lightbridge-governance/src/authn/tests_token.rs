//! Tests for the pod's own outgoing TokenReview credential being re-read on
//! every call (not cached at startup). Kept in its own file to stay under the
//! repo's 200-LoC ceiling (see `.github/actions/loc-gate`).

use std::collections::HashSet;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::TokenReviewVerifier;

/// Starts a TCP stub that answers each TokenReview POST with a canned JSON
/// body and sends the request's `Authorization` header over a channel.
/// Returns the base URL and a receiver for the captured headers.
async fn token_review_stub_capturing(
    body: &'static str,
) -> (String, tokio::sync::mpsc::UnboundedReceiver<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub listener");
    let addr = listener.local_addr().expect("local addr");
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = match listener.accept().await {
                Ok(x) => x,
                Err(_) => break,
            };
            // Read until the header terminator so the whole request is seen.
            let mut buf = Vec::new();
            let mut tmp = [0u8; 1024];
            loop {
                let n = socket.read(&mut tmp).await.expect("read request");
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let request = String::from_utf8_lossy(&buf).to_string();
            // Header names are case-insensitive; reqwest sends `authorization:`.
            let auth = request
                .lines()
                .find_map(|l| {
                    let (name, value) = l.split_once(':')?;
                    name.trim()
                        .eq_ignore_ascii_case("authorization")
                        .then(|| value.trim().to_owned())
                })
                .unwrap_or_default();
            let _ = tx.send(auth);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    (format!("http://{addr}"), rx)
}

/// The pod's own outgoing TokenReview credential must be re-read on every
/// call, not cached at startup: the projected-token volume rewrites the file
/// before its ~1h expiry, so a cached token would go stale and fail closed
/// for every caller (see #301 review).
#[tokio::test]
async fn own_token_is_re_read_on_every_call() {
    let dir = std::env::temp_dir().join(format!("lb-token-{}", cuid::cuid2()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let token_path = dir.join("token");
    std::fs::write(&token_path, "token-v1").expect("write token");

    let body = r#"{"status":{"authenticated":true,"audiences":["api"],"user":{"username":"system:serviceaccount:default/allowed"}}}"#;
    let (base, mut rx) = token_review_stub_capturing(body).await;
    let verifier = TokenReviewVerifier {
        client: reqwest::Client::new(),
        review_url: format!("{base}/apis/authentication.k8s.io/v1/tokenreviews"),
        token_path,
        audiences: vec!["api".to_owned()],
        allowed_accounts: HashSet::from(["default/allowed".to_owned()]),
    };

    verifier.verify("caller-token").await.expect("first verify");
    assert_eq!(rx.recv().await.as_deref(), Some("Bearer token-v1"));

    std::fs::write(&verifier.token_path, "token-v2").expect("rewrite token");
    verifier
        .verify("caller-token")
        .await
        .expect("second verify");
    assert_eq!(rx.recv().await.as_deref(), Some("Bearer token-v2"));

    let _ = std::fs::remove_dir_all(&dir);
}
