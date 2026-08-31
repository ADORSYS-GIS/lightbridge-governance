//! A mock OTLP/HTTP collector: `/v1/metrics` and `/v1/logs`, nothing else.
//!
//! Exists so `copilot-push` can be tested end to end without a network. The
//! assertion these tests actually depend on is **negative** -- "the collector
//! received nothing" -- which needs a real listener that can prove zero
//! requests arrived, not a stubbed client that could only prove no call was
//! made to a function.
//!
//! Panic-free for the same reason `mock_idp.rs` is: free functions under
//! `tests/support/` are outside clippy's `allow-*-in-tests` carve-out.

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use axum::{
    Router,
    extract::State,
    http::{HeaderMap, StatusCode, Uri},
    routing::post,
};
use serde_json::Value;

/// What the collector answers with, for the lifetime of one instance.
#[derive(Clone, Copy)]
pub enum Behavior {
    Accept,
    /// Reject everything -- used to prove the checkpoint does not advance
    /// past a batch the collector never took.
    Reject(u16),
}

#[derive(Default)]
struct Inner {
    /// `(path, had_bearer, payload)` for every request received, in order.
    requests: Vec<(String, bool, Value)>,
}

#[derive(Clone)]
pub struct MockCollector {
    pub base_url: String,
    behavior: Behavior,
    state: Arc<Mutex<Inner>>,
}

impl MockCollector {
    pub async fn start(behavior: Behavior) -> Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .context("binding mock collector listener")?;
        let addr = listener
            .local_addr()
            .context("reading mock collector local address")?;

        let this = Self {
            base_url: format!("http://{addr}"),
            behavior,
            state: Arc::new(Mutex::new(Inner::default())),
        };

        let router = Router::new()
            .route("/v1/metrics", post(receive))
            .route("/v1/logs", post(receive))
            .with_state(this.clone());

        let served = this.clone();
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
            drop(served);
        });

        Ok(this)
    }

    /// Total requests received. `0` is the assertion the fail-closed test
    /// turns on.
    pub fn request_count(&self) -> Result<usize> {
        Ok(lock(&self.state)?.requests.len())
    }

    pub fn paths(&self) -> Result<Vec<String>> {
        Ok(lock(&self.state)?
            .requests
            .iter()
            .map(|(path, ..)| path.clone())
            .collect())
    }

    /// Whether every request carried an `Authorization` header. Vacuously
    /// true with no requests, so callers assert a count as well.
    pub fn every_request_authenticated(&self) -> Result<bool> {
        Ok(lock(&self.state)?
            .requests
            .iter()
            .all(|(_, had_bearer, _)| *had_bearer))
    }

    /// `(path, payload)` for every request, so a test can assert *what* was
    /// exported and not merely that something was.
    pub fn payloads(&self) -> Result<Vec<(String, Value)>> {
        Ok(lock(&self.state)?
            .requests
            .iter()
            .map(|(path, _, payload)| (path.clone(), payload.clone()))
            .collect())
    }
}

async fn receive(
    State(collector): State<MockCollector>,
    uri: Uri,
    headers: HeaderMap,
    body: String,
) -> StatusCode {
    let path = uri.path().to_owned();
    let had_bearer = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("Bearer "));
    let payload = serde_json::from_str(&body).unwrap_or(Value::Null);

    if let Ok(mut inner) = collector.state.lock() {
        inner.requests.push((path, had_bearer, payload));
    }

    match collector.behavior {
        Behavior::Accept => StatusCode::OK,
        Behavior::Reject(status) => {
            StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

fn lock(state: &Arc<Mutex<Inner>>) -> Result<std::sync::MutexGuard<'_, Inner>> {
    state
        .lock()
        .map_err(|_| anyhow::anyhow!("the mock collector's state mutex was poisoned"))
}
