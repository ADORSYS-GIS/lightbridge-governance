//! A minimal raw-bytes OTLP collector for the `serve --otel` tests.
//!
//! Accepts **any** body and records `(path, content-type, bytes)`. Exists
//! because the shared [`super::mock_collector`] only accepts JSON, and the
//! "protobuf is forwarded verbatim, not withheld" case needs a destination that
//! can prove the *bytes* arrived. Kept here (not inline in a test) so every
//! test file stays under the repo's 200-LoC gate.

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};

type RawRequest = (String, String, Vec<u8>);
type RawState = Mutex<Vec<RawRequest>>;

pub struct RawCollector {
    pub base_url: String,
    state: Arc<RawState>,
}

impl RawCollector {
    pub async fn start() -> Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .context("binding the raw collector listener")?;
        let addr = listener
            .local_addr()
            .context("reading the raw collector address")?;
        let state: Arc<RawState> = Arc::default();
        let this = Self {
            base_url: format!("http://{addr}"),
            state: state.clone(),
        };

        let router = axum::Router::new().fallback(raw_receive).with_state(state);
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });

        Ok(this)
    }

    pub fn requests(&self) -> Result<Vec<RawRequest>> {
        Ok(self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("raw collector mutex poisoned"))?
            .clone())
    }
}

async fn raw_receive(
    axum::extract::State(state): axum::extract::State<Arc<RawState>>,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::http::StatusCode {
    let content_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    if let Ok(mut inner) = state.lock() {
        inner.push((uri.path().to_owned(), content_type, body.to_vec()));
    }
    axum::http::StatusCode::OK
}
