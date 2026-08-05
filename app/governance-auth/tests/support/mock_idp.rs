//! A minimal mock OIDC server: discovery + token endpoints only. No
//! `/authorize` route exists -- tests act as "the browser" by hitting the
//! client's loopback redirect URI directly, so the authorization endpoint
//! is never actually called.
//!
//! Like `harness.rs`, this stays panic-free: `MockIdp::start` returns
//! `anyhow::Result` for the setup steps a `#[tokio::test]` caller can
//! propagate with `?`. The two spots that structurally can't return a
//! `Result` anywhere -- the detached server task and axum's handler
//! signatures -- degrade to an HTTP 500 / a swallowed error instead of
//! `.unwrap()`/`.expect()`, which would show up as an untraceable panic in
//! a background task rather than a clean test failure anyway.

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use axum::{
    Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, post},
};
use serde_json::json;

/// What the mock token endpoint hands back on every request it receives,
/// for the lifetime of one `MockIdp` instance. Good enough for these tests:
/// each scenario (fresh login, refresh success, refresh failure, ...) spins
/// up its own mock server with the behavior it needs, rather than one mock
/// server whose behavior mutates mid-test.
#[derive(Clone)]
pub enum TokenBehavior {
    Succeed {
        access_token: String,
        refresh_token: Option<String>,
        expires_in: u64,
    },
    Fail {
        status: u16,
        error: &'static str,
    },
}

struct Inner {
    base_url: String,
    behavior: TokenBehavior,
    token_calls: u32,
}

#[derive(Clone)]
pub struct MockIdp {
    pub base_url: String,
    state: Arc<Mutex<Inner>>,
}

impl MockIdp {
    pub async fn start(behavior: TokenBehavior) -> Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .context("binding mock idp listener")?;
        let addr = listener
            .local_addr()
            .context("reading mock idp local address")?;
        let base_url = format!("http://{addr}");

        let state = Arc::new(Mutex::new(Inner {
            base_url: base_url.clone(),
            behavior,
            token_calls: 0,
        }));

        let router = Router::new()
            .route("/.well-known/openid-configuration", get(discovery))
            .route("/token", post(token))
            .with_state(state.clone());

        tokio::spawn(async move {
            // Detached background task: nothing awaits its result, so a
            // failure here can only be surfaced by swallowing it and
            // letting the client-side HTTP call fail with a connection
            // error, not by propagating a `Result` anywhere.
            let _ = axum::serve(listener, router).await;
        });

        Ok(Self { base_url, state })
    }

    pub fn token_call_count(&self) -> Result<u32> {
        Ok(lock(&self.state)?.token_calls)
    }
}

fn lock(state: &Arc<Mutex<Inner>>) -> Result<std::sync::MutexGuard<'_, Inner>> {
    state
        .lock()
        .map_err(|_| anyhow::anyhow!("mock idp state lock poisoned"))
}

async fn discovery(State(state): State<Arc<Mutex<Inner>>>) -> impl IntoResponse {
    let Ok(guard) = lock(&state) else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "mock idp lock poisoned").into_response();
    };
    let base_url = guard.base_url.clone();
    Json(json!({
        "authorization_endpoint": format!("{base_url}/authorize"),
        "token_endpoint": format!("{base_url}/token"),
        "device_authorization_endpoint": format!("{base_url}/device"),
    }))
    .into_response()
}

async fn token(State(state): State<Arc<Mutex<Inner>>>) -> impl IntoResponse {
    let Ok(mut guard) = lock(&state) else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "mock idp lock poisoned").into_response();
    };
    guard.token_calls += 1;

    match &guard.behavior {
        TokenBehavior::Succeed {
            access_token,
            refresh_token,
            expires_in,
        } => (
            StatusCode::OK,
            Json(json!({
                "access_token": access_token,
                "refresh_token": refresh_token,
                "expires_in": expires_in,
            })),
        )
            .into_response(),
        TokenBehavior::Fail { status, error } => {
            let status = StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_REQUEST);
            (
                status,
                Json(json!({
                    "error": error,
                    "error_description": "mock idp configured failure",
                })),
            )
                .into_response()
        }
    }
}
