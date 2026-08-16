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

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result};
use axum::{
    Form, Router,
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

/// Lets a test override what the discovery document advertises for
/// `token_endpoint`, independent of where this server actually listens --
/// used to simulate exactly the shape `oauth::discovery::require_same_origin`
/// exists to reject: a discovery response whose `issuer` matches what was
/// requested (so the RFC 8414 §3.1.2 check passes) but whose `token_endpoint`
/// is at a different origin. `None` means "advertise this server's own
/// `/token`", the previous (and still default) behavior.
#[derive(Clone, Default)]
pub struct DiscoveryOverrides {
    pub token_endpoint: Option<String>,
}

struct Inner {
    base_url: String,
    behavior: TokenBehavior,
    token_calls: u32,
    discovery_overrides: DiscoveryOverrides,
    device_calls: u32,
    // What the client actually sent, captured verbatim rather than just a
    // presence bool -- a test asserting the device flow implements PKCE
    // correctly (not just "sends *a* param named code_challenge") needs the
    // real values to recompute S256(verifier) and compare.
    last_device_code_challenge: Option<String>,
    last_device_code_challenge_method: Option<String>,
    last_token_code_verifier: Option<String>,
    // What `scope` the client actually sent on the device-authorization
    // request -- used to prove ADR-0012 Decision 2's precedence (a `--scopes`
    // flag must win over `GOVERNANCE_AUTH_SCOPES`) against the real value the
    // client would put on the wire, not just an internal struct field.
    last_device_scope: Option<String>,
}

#[derive(Clone)]
pub struct MockIdp {
    pub base_url: String,
    state: Arc<Mutex<Inner>>,
}

impl MockIdp {
    pub async fn start(behavior: TokenBehavior) -> Result<Self> {
        Self::start_with_discovery_overrides(behavior, DiscoveryOverrides::default()).await
    }

    pub async fn start_with_discovery_overrides(
        behavior: TokenBehavior,
        discovery_overrides: DiscoveryOverrides,
    ) -> Result<Self> {
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
            discovery_overrides,
            device_calls: 0,
            last_device_code_challenge: None,
            last_device_code_challenge_method: None,
            last_token_code_verifier: None,
            last_device_scope: None,
        }));

        let router = Router::new()
            .route("/.well-known/openid-configuration", get(discovery))
            .route("/token", post(token))
            .route("/device", post(device_authorization))
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

    pub fn device_call_count(&self) -> Result<u32> {
        Ok(lock(&self.state)?.device_calls)
    }

    /// What the client sent as `code_challenge` on the device-authorization
    /// request, if anything -- `None` distinguishes "no PKCE params at all"
    /// from "sent an empty string", which a plain `String` default wouldn't.
    pub fn last_device_code_challenge(&self) -> Result<Option<String>> {
        Ok(lock(&self.state)?.last_device_code_challenge.clone())
    }

    pub fn last_device_code_challenge_method(&self) -> Result<Option<String>> {
        Ok(lock(&self.state)?.last_device_code_challenge_method.clone())
    }

    /// What the client sent as `code_verifier` on its most recent poll of
    /// the token endpoint.
    pub fn last_token_code_verifier(&self) -> Result<Option<String>> {
        Ok(lock(&self.state)?.last_token_code_verifier.clone())
    }

    /// What the client sent as `scope` on its most recent device-authorization
    /// request.
    pub fn last_device_scope(&self) -> Result<Option<String>> {
        Ok(lock(&self.state)?.last_device_scope.clone())
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
    let token_endpoint = guard
        .discovery_overrides
        .token_endpoint
        .clone()
        .unwrap_or_else(|| format!("{base_url}/token"));
    Json(json!({
        "issuer": base_url,
        "authorization_endpoint": format!("{base_url}/authorize"),
        "token_endpoint": token_endpoint,
        "device_authorization_endpoint": format!("{base_url}/device"),
    }))
    .into_response()
}

/// Real device-authorization endpoints (Keycloak's included) reject a
/// request with no `code_challenge_method` once PKCE is required on the
/// client -- this mock only needs to hand back a device/user code pair, but
/// it captures what the client sent so a test can assert the client
/// actually included PKCE, not just that it survived a mock that doesn't
/// enforce anything.
async fn device_authorization(
    State(state): State<Arc<Mutex<Inner>>>,
    Form(form): Form<HashMap<String, String>>,
) -> impl IntoResponse {
    let Ok(mut guard) = lock(&state) else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "mock idp lock poisoned").into_response();
    };
    guard.device_calls += 1;
    guard.last_device_code_challenge = form.get("code_challenge").cloned();
    guard.last_device_code_challenge_method = form.get("code_challenge_method").cloned();
    guard.last_device_scope = form.get("scope").cloned();

    let base_url = guard.base_url.clone();
    (
        StatusCode::OK,
        Json(json!({
            "device_code": "mock-device-code",
            "user_code": "MOCK-CODE",
            "verification_uri": format!("{base_url}/device/verify"),
            "verification_uri_complete": format!("{base_url}/device/verify?user_code=MOCK-CODE"),
            // 0 collapses to `interval.max(1)` == 1s in the client's poll
            // loop (device.rs) -- fast enough for a test, never zero-wait.
            "expires_in": 60,
            "interval": 0,
        })),
    )
        .into_response()
}

async fn token(
    State(state): State<Arc<Mutex<Inner>>>,
    Form(form): Form<HashMap<String, String>>,
) -> impl IntoResponse {
    let Ok(mut guard) = lock(&state) else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "mock idp lock poisoned").into_response();
    };
    guard.token_calls += 1;
    if form.get("grant_type").map(String::as_str)
        == Some("urn:ietf:params:oauth:grant-type:device_code")
    {
        guard.last_token_code_verifier = form.get("code_verifier").cloned();
    }

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
