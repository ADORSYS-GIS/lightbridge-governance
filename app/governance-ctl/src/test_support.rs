//! Test-only support for `sync.rs`'s integration tests: a minimal mock
//! GitHub API server, plus a throwaway RSA test key for `AppAuth`.
//!
//! Declared `#[cfg(test)]` in `main.rs`, so this whole module is compiled
//! only into test binaries and does not exist in a production build --
//! unreachable from any production path (AGENTS.md), the same guarantee
//! `governance-copilot`'s `tests/support/` gets from living under `tests/`.
//! This crate is `[[bin]]`-only (no `[lib]`), so an integration test under
//! `tests/*.rs` cannot link against it at all; `#[cfg(test)]` is the
//! equivalent boundary available here.
//!
//! Modeled on `governance-copilot`'s `tests/support/mock_github.rs` (itself
//! modeled on `app/governance-auth/tests/support/mock_idp.rs`), trimmed to
//! just what `sync.rs`'s tests exercise: the report envelope + signed
//! download, and the App-installation-token exchange.

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use axum::{
    Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, post},
};
use serde_json::json;

/// A throwaway 2048-bit RSA private key for tests that need `AppAuth` to
/// mint a well-formed JWT. Belongs to no real GitHub App; the mock server
/// here never verifies the JWT's signature. Generated once with
/// `openssl genrsa -traditional 2048`.
pub const TEST_APP_PRIVATE_KEY_PEM: &str = "-----BEGIN RSA PRIVATE KEY-----
MIIEowIBAAKCAQEAtOjy/gO/A5Sc9pxxdUxY3gpjCwwcPmAeCSUQRXxUpVQoo17I
Ph+2S018GH+wn2kLbUJJk9tYCtKAwApRso/rb/8IKpWm3Ft+ecBnF+H1pU+AFRWk
DrTD7AUCcXPqR3stLCvE95jq7NFHRnMfv0OXaB/obUBCgT2FySPrX8Rd2fKLazrK
ZFTXRR10pl8mVf3cOqB529fDxyHthP1J1s7iu4j+HDkO89xxm6Fwfbp+71Yz7XH5
yApsFMrXbrMv98B9z00d6muh+EcszmYu/JBrf1kcdc+Rt12svqs1uGTqsgWic7ef
rka3dzecCP180B/uIEQJyjE8vlSyw6PNI+nmLwIDAQABAoIBAEFt0NhS1YY/fQdq
LFSukKN5oTmRHzPmAmbvSzO+VETZK7tuX8CsKnuQohWgNOpqjPHum/rIRU7gtCUA
dmy8xXtjgvoX1tnyk0sIbaDDHds0Zg/6HDQfZ46Yfzo2IKDKqVtE1z9vRGPzCrKt
l2lO0lcb1y2QJJ1meVj2Tz37ILBefnnhDvYZvWOiIzRC7ZvsNApdQ2VVDN9Cj9b2
Cj0SXZ78gU9BeVW3KpiukHHLaja07rUwapo7EyCYntfyNwPGas5Rxmh4FDwGANRl
y3Onv/PByswblYIGyVw2Vk+PeYf4hdusYuNZ0rReMxttlbYIDW4v2Ok1BDPCmryC
CBPtQnUCgYEA7E6C5k5oWOlvzR1PQ04nZSAr/4pysqupZS0bUtKgexfhHIC4MmO6
QOpKTAXt36mG/ZpMnBLy60qNfABd6wDwygmWFYHH9S6VocDnV3oDWMfqD+hKWbKL
HMV8JXwPpXwjn4e9U22s8+Efh7HOvxx0dXSduwJqniUFfb0dMigcmCMCgYEAw/yT
VTQWaoCvph3AdKJpbb/UZ8yj82hoxKMZV7I8g4A6hyZn+ePOLh+0Peef0kbncxZd
Ug6yV144gMHIa3v/wMSqnBkkXd9fgPiymuc56GDZOogl/TgubBrR7LVYQXaZYo2Z
kqiXaK0P/XgRdC5mJJ+aoZv0Exn08m2XsOHVdIUCgYAVZlrGXo1ml+VPDvtxne9F
Yi952eDfO1qA1h/mVTrBSv1Q5ntH3O4uGMmXruXG3oRiDQopDDJBiqPbefEHajNk
KJAV7IXeN1THrD+HFX6eGKSiwieRjfC5L005281S8DYNqW5E0ubZwyZm1HxjpEEL
rf7mw6ZCIhooM+sj8qv8PwKBgQCGJAHTd2tASgPu9r4bFm6Cp6GByhcNKpFKxTc7
RssUVle42RiheMJN33VGSZqiGdWgd9Y3q8d09RBHUFsU9jH+hp0fajXx6kk7xPy5
+TkxS9hir30Q67saUuEL2rMlWz9wrOpH7wxyoMEpA10u3/MZbgQwSMWtrT5yD4Cb
mHa44QKBgD2KpxJP98epnAuqhFI9zah/2VJ58WehMMDsytCMFFWA5vSATNnHS2cT
MEKIEkpXT26LMGEoMh7Dj46eiENfzTNe8TwZOVazGfUvPP1d4EXEAG5uA1Apobzr
CSgbK75NG/wz2eYiJQfyZ6sTqURh5dxu9kB9GUOXYVqDAt2JIn6l
-----END RSA PRIVATE KEY-----
";

/// A route either always succeeds, or fails with `status` for every call.
/// Only these two shapes are needed here: `sync.rs`'s tests care about
/// "this day's fetch works" vs. "this day's fetch is completely broken",
/// not about retry timing (that is `governance-copilot`'s `tests/retry.rs`).
#[derive(Clone, Copy)]
pub enum RouteBehavior {
    AlwaysSucceeds,
    AlwaysFails(u16),
}

struct Inner {
    base_url: String,
    report: RouteBehavior,
    installed_org: String,
}

/// A handle to the running mock server. `base_url` is the only field a test
/// needs; the router holds its own `Arc<Mutex<Inner>>` clone, so this handle
/// does not need to hold one back for the server to keep working.
#[derive(Clone)]
pub struct MockGithub {
    pub base_url: String,
}

impl MockGithub {
    /// Start a mock server whose report endpoint behaves as `report`, for
    /// an App installed on `installed_org`.
    pub async fn start(report: RouteBehavior, installed_org: &str) -> Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .context("binding mock github listener")?;
        let addr = listener
            .local_addr()
            .context("reading mock github local address")?;
        let base_url = format!("http://{addr}");

        let state = Arc::new(Mutex::new(Inner {
            base_url: base_url.clone(),
            report,
            installed_org: installed_org.to_owned(),
        }));

        let router = Router::new()
            .route(
                "/orgs/{org}/copilot/metrics/reports/{report}",
                get(report_route),
            )
            .route("/download/{report}", get(download_route))
            .route("/app/installations", get(installations_route))
            .route(
                "/app/installations/{id}/access_tokens",
                post(access_token_route),
            )
            .with_state(state);

        tokio::spawn(async move {
            // Detached background task: nothing awaits its result, so a
            // failure here can only surface as a connection error on the
            // client side, same tradeoff as mock_idp.rs/mock_github.rs.
            let _ = axum::serve(listener, router).await;
        });

        Ok(Self { base_url })
    }
}

fn lock(state: &Arc<Mutex<Inner>>) -> Result<std::sync::MutexGuard<'_, Inner>> {
    state
        .lock()
        .map_err(|_| anyhow::anyhow!("mock github state lock poisoned"))
}

async fn report_route(
    State(state): State<Arc<Mutex<Inner>>>,
    Path((_org, report)): Path<(String, String)>,
) -> impl IntoResponse {
    let Ok(guard) = lock(&state) else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "lock poisoned").into_response();
    };
    match guard.report {
        RouteBehavior::AlwaysFails(status) => {
            let status = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
            (status, Json(json!({"message": "mock configured failure"}))).into_response()
        }
        RouteBehavior::AlwaysSucceeds => {
            let base_url = guard.base_url.clone();
            Json(json!({
                "download_links": [format!("{base_url}/download/{report}")],
                "report_day": "2026-08-01",
            }))
            .into_response()
        }
    }
}

/// Always succeeds with an empty body -- `report.rs` reads an empty download
/// as `DownloadedReport.empty = true`, which `sync.rs`'s `ingest_one` turns
/// into a valid "empty" manifest row (no rows to parse), which is enough for
/// these tests: they assert manifest-row/high-water-mark/day-coverage
/// behavior, not parsed-row content (that is `parse.rs`'s own tests).
async fn download_route() -> impl IntoResponse {
    Vec::<u8>::new()
}

async fn installations_route(State(state): State<Arc<Mutex<Inner>>>) -> impl IntoResponse {
    let Ok(guard) = lock(&state) else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "lock poisoned").into_response();
    };
    Json(json!([{"id": 42, "account": {"login": guard.installed_org}}])).into_response()
}

async fn access_token_route() -> impl IntoResponse {
    (
        StatusCode::CREATED,
        Json(json!({"token": "mock-installation-token"})),
    )
}
