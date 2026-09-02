//! A mock OTLP/HTTP collector: `/v1/metrics` and `/v1/logs`, nothing else.
//!
//! Exists so `copilot push` can be tested end to end without a network. The
//! assertion these tests actually depend on is **negative** -- "the collector
//! received nothing", "it never saw this record twice" -- which needs a real
//! listener that can prove zero requests arrived, not a stubbed client that
//! could only prove no call was made to a function.
//!
//! Every request is recorded with the status it was answered with, because
//! "delivered" is what duplicate-delivery assertions are about and a request
//! the collector refused delivered nothing.
//!
//! Panic-free for the same reason `mock_idp.rs` is: free functions under
//! `tests/support/` are outside clippy's `allow-*-in-tests` carve-out.

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use axum::{
    Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode, Uri},
    routing::post,
};
use serde_json::Value;

pub use super::collector_policy::Behavior;

/// One request, and what it was answered with.
struct Received {
    path: String,
    had_bearer: bool,
    payload: Value,
    accepted: bool,
}

#[derive(Default)]
struct Inner {
    requests: Vec<Received>,
}

#[derive(Clone)]
pub struct MockCollector {
    pub base_url: String,
    behavior: Arc<Mutex<Behavior>>,
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
            behavior: Arc::new(Mutex::new(behavior)),
            state: Arc::new(Mutex::new(Inner::default())),
        };

        let router = Router::new()
            .route("/v1/metrics", post(receive))
            .route("/v1/logs", post(receive))
            // axum's default 2 MiB body cap would 413 one sweep's 8 MiB post.
            .layer(DefaultBodyLimit::disable())
            .with_state(this.clone());

        let served = this.clone();
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
            drop(served);
        });

        Ok(this)
    }

    /// Changes what the collector answers with from here on. Tests use this
    /// between two `copilot push` runs to model a transport that refused a
    /// record once and takes it next time -- flaky, not poisonous.
    pub fn set_behavior(&self, behavior: Behavior) -> Result<()> {
        *self
            .behavior
            .lock()
            .map_err(|_| anyhow::anyhow!("the mock collector's behavior mutex was poisoned"))? =
            behavior;
        Ok(())
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
            .map(|received| received.path.clone())
            .collect())
    }

    /// Whether every request carried an `Authorization` header. Vacuously
    /// true with no requests, so callers assert a count as well.
    pub fn every_request_authenticated(&self) -> Result<bool> {
        Ok(lock(&self.state)?
            .requests
            .iter()
            .all(|received| received.had_bearer))
    }

    /// `(path, payload)` for every request, so a test can assert *what* was
    /// exported and not merely that something was.
    pub fn payloads(&self) -> Result<Vec<(String, Value)>> {
        Ok(lock(&self.state)?
            .requests
            .iter()
            .map(|received| (received.path.clone(), received.payload.clone()))
            .collect())
    }

    /// The log record bodies the collector actually **took**, in order and
    /// with repeats kept. Refused requests are excluded: they delivered
    /// nothing, so counting them would hide the very duplication these tests
    /// exist to catch.
    pub fn accepted_log_bodies(&self) -> Result<Vec<String>> {
        let array = |value: &Value, key: &str| match value.get(key) {
            Some(Value::Array(items)) => items.clone(),
            _ => Vec::new(),
        };
        let mut bodies = Vec::new();
        for received in lock(&self.state)?.requests.iter() {
            if !received.accepted || received.path != "/v1/logs" {
                continue;
            }
            for resource in array(&received.payload, "resourceLogs") {
                for scope in array(&resource, "scopeLogs") {
                    for record in array(&scope, "logRecords") {
                        if let Some(body) =
                            record.pointer("/body/stringValue").and_then(Value::as_str)
                        {
                            bodies.push(body.to_owned());
                        }
                    }
                }
            }
        }
        Ok(bodies)
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

    let behavior = match collector.behavior.lock() {
        Ok(behavior) => *behavior,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR,
    };
    let rejection = behavior.verdict(&path, &body);
    let status = match rejection {
        None => StatusCode::OK,
        Some(status) => StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
    };

    if let Ok(mut inner) = collector.state.lock() {
        inner.requests.push(Received {
            path,
            had_bearer,
            payload,
            accepted: status.is_success(),
        });
    }

    let delay = behavior.delay_millis();
    if delay > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
    }
    status
}

fn lock(state: &Arc<Mutex<Inner>>) -> Result<std::sync::MutexGuard<'_, Inner>> {
    state
        .lock()
        .map_err(|_| anyhow::anyhow!("the mock collector's state mutex was poisoned"))
}
