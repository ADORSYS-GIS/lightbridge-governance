//! A minimal mock GitHub API + signed-download server, for the retry tests in
//! `tests/retry.rs`. Test-only: this lives under `tests/`, never under
//! `src/`, so it is unreachable from any production path (AGENTS.md).
//!
//! Modeled on `app/governance-auth/tests/support/mock_idp.rs`: axum is
//! already a pinned workspace dependency (governance-ctl uses it
//! transitively via cratestack-axum), so reusing it here as a dev-dependency
//! adds no new crate to the supply chain.
//!
//! Each route can be told to fail with a given status a fixed number of
//! times before it starts succeeding, so a test can assert both "a
//! transient failure is retried and eventually succeeds" and "a
//! deterministic failure is never retried" against the same server shape.

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use axum::{
    Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json},
    routing::{get, post},
};
use serde_json::json;

/// How a route behaves: fail with `status` (and an optional `Retry-After`
/// header) for the first `fail_times` calls, then always succeed with
/// `body`/`bytes`.
#[derive(Clone)]
pub struct RouteBehavior {
    pub fail_times: u32,
    pub fail_status: u16,
    pub retry_after: Option<String>,
}

impl RouteBehavior {
    /// Always succeeds -- no failures to simulate.
    pub fn always_succeeds() -> Self {
        Self {
            fail_times: 0,
            fail_status: 200,
            retry_after: None,
        }
    }

    /// Fails with `status` on every call -- for asserting a deterministic
    /// failure (401/403/404) is never retried.
    pub fn always_fails(status: u16) -> Self {
        Self {
            fail_times: u32::MAX,
            fail_status: status,
            retry_after: None,
        }
    }

    /// Fails with `status` for the first `n` calls, then succeeds.
    pub fn fails_then_succeeds(n: u32, status: u16) -> Self {
        Self {
            fail_times: n,
            fail_status: status,
            retry_after: None,
        }
    }

    /// As `fails_then_succeeds`, plus a `Retry-After` header on the failing
    /// responses.
    pub fn fails_with_retry_after(n: u32, status: u16, retry_after_secs: u64) -> Self {
        Self {
            fail_times: n,
            fail_status: status,
            retry_after: Some(retry_after_secs.to_string()),
        }
    }
}

/// How the `/orgs/{org}/copilot/billing/seats` route behaves. A separate
/// enum from `RouteBehavior` (rather than reusing it) because seats
/// pagination has shapes `RouteBehavior`'s "fail N times then succeed"
/// model does not: a fixed number of real pages, a server that never stops
/// claiming a next page, and a first page whose `Link` header does not
/// parse at all.
#[derive(Clone)]
pub enum SeatsBehavior {
    /// `total` seats spread across pages of `page_size`, following normal
    /// `Link: rel="next"` pagination until exhausted.
    Paginated { total: u32, page_size: u32 },
    /// Every page's `Link` header advertises another "next" page forever,
    /// regardless of how many seats have already been returned -- proves
    /// the client's page cap (not the data) is what stops the fetch.
    LoopsForever { page_size: u32 },
    /// The first page returns a `Link` header that does not parse as a
    /// `rel="next"` entry at all (not even `<...>`-bracketed) -- proves a
    /// genuinely garbage header stops pagination immediately.
    MalformedLinkHeader,
    /// The first page returns a syntactically well-formed, bracketed `Link`
    /// header whose only relation is `rel="last"` (never `rel="next"`),
    /// with `total_seats` claiming more data exists than one page holds --
    /// proves the `rel="next"` check itself gates continuation. A client
    /// whose relation check were broken (treating any bracketed URL as
    /// "next") would incorrectly fetch a second page here; only a client
    /// that actually enforces the relation stops at page 1.
    LinkHeaderWithNoNextRelation { page_size: u32 },
    /// Every call fails with `status`.
    AlwaysFails(u16),
}

impl Default for SeatsBehavior {
    fn default() -> Self {
        Self::Paginated {
            total: 1,
            page_size: 100,
        }
    }
}

struct Inner {
    base_url: String,
    report: RouteBehavior,
    report_calls: u32,
    download: RouteBehavior,
    download_calls: u32,
    download_body: Vec<u8>,
    installations: RouteBehavior,
    installations_calls: u32,
    /// The org login the mock's `/app/installations` list advertises.
    installed_org: String,
    access_token: RouteBehavior,
    access_token_calls: u32,
    seats: SeatsBehavior,
    seats_calls: u32,
}

#[derive(Clone)]
pub struct MockGithub {
    pub base_url: String,
    state: Arc<Mutex<Inner>>,
}

/// Everything a test needs to configure before starting the mock; every
/// field has a working default so a test only sets what it cares about.
pub struct MockGithubConfig {
    pub report: RouteBehavior,
    pub download: RouteBehavior,
    pub download_body: Vec<u8>,
    pub installations: RouteBehavior,
    pub installed_org: String,
    pub access_token: RouteBehavior,
    pub seats: SeatsBehavior,
}

impl Default for MockGithubConfig {
    fn default() -> Self {
        Self {
            report: RouteBehavior::always_succeeds(),
            download: RouteBehavior::always_succeeds(),
            download_body: b"{\"day\":\"2026-08-01\"}\n".to_vec(),
            installations: RouteBehavior::always_succeeds(),
            installed_org: "test-org".to_owned(),
            access_token: RouteBehavior::always_succeeds(),
            seats: SeatsBehavior::default(),
        }
    }
}

impl MockGithub {
    pub async fn start(config: MockGithubConfig) -> Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .context("binding mock github listener")?;
        let addr = listener
            .local_addr()
            .context("reading mock github local address")?;
        let base_url = format!("http://{addr}");

        let state = Arc::new(Mutex::new(Inner {
            base_url: base_url.clone(),
            report: config.report,
            report_calls: 0,
            download: config.download,
            download_calls: 0,
            download_body: config.download_body,
            installations: config.installations,
            installations_calls: 0,
            installed_org: config.installed_org,
            access_token: config.access_token,
            access_token_calls: 0,
            seats: config.seats,
            seats_calls: 0,
        }));

        let router = Router::new()
            .route(
                "/orgs/{org}/copilot/metrics/reports/{report}",
                get(report_route),
            )
            .route("/download/{report}", get(download_route))
            .route("/orgs/{org}/copilot/billing/seats", get(seats_route))
            .route("/app/installations", get(installations_route))
            .route(
                "/app/installations/{id}/access_tokens",
                post(access_token_route),
            )
            .with_state(state.clone());

        tokio::spawn(async move {
            // Detached background task: nothing awaits its result, so a
            // failure here can only be surfaced by swallowing it and letting
            // the client-side HTTP call fail with a connection error -- same
            // tradeoff as mock_idp.rs.
            let _ = axum::serve(listener, router).await;
        });

        Ok(Self { base_url, state })
    }

    pub fn report_call_count(&self) -> Result<u32> {
        Ok(lock(&self.state)?.report_calls)
    }

    pub fn download_call_count(&self) -> Result<u32> {
        Ok(lock(&self.state)?.download_calls)
    }

    pub fn installations_call_count(&self) -> Result<u32> {
        Ok(lock(&self.state)?.installations_calls)
    }

    pub fn access_token_call_count(&self) -> Result<u32> {
        Ok(lock(&self.state)?.access_token_calls)
    }

    pub fn seats_call_count(&self) -> Result<u32> {
        Ok(lock(&self.state)?.seats_calls)
    }
}

fn lock(state: &Arc<Mutex<Inner>>) -> Result<std::sync::MutexGuard<'_, Inner>> {
    state
        .lock()
        .map_err(|_| anyhow::anyhow!("mock github state lock poisoned"))
}

/// Build the response for one call against `behavior`, given the call
/// number about to be recorded (1-based). `on_success` builds the success
/// response body only when this call is the one that succeeds -- kept lazy
/// so failing calls never touch it.
fn respond(
    behavior: &RouteBehavior,
    call_no: u32,
    on_success: impl FnOnce() -> axum::response::Response,
) -> axum::response::Response {
    if call_no <= behavior.fail_times {
        let status = StatusCode::from_u16(behavior.fail_status).unwrap_or(StatusCode::BAD_GATEWAY);
        let mut headers = HeaderMap::new();
        if let Some(retry_after) = &behavior.retry_after
            && let Ok(v) = retry_after.parse()
        {
            headers.insert(axum::http::header::RETRY_AFTER, v);
        }
        return (
            status,
            headers,
            Json(json!({"message": "mock github configured failure"})),
        )
            .into_response();
    }
    on_success()
}

async fn report_route(
    State(state): State<Arc<Mutex<Inner>>>,
    Path((_org, report)): Path<(String, String)>,
) -> impl IntoResponse {
    let Ok(mut guard) = lock(&state) else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "lock poisoned").into_response();
    };
    guard.report_calls += 1;
    let call_no = guard.report_calls;
    let base_url = guard.base_url.clone();
    respond(&guard.report.clone(), call_no, || {
        Json(json!({
            "download_links": [format!("{base_url}/download/{report}")],
            "report_day": "2026-08-01",
        }))
        .into_response()
    })
}

async fn download_route(
    State(state): State<Arc<Mutex<Inner>>>,
    Path(_report): Path<String>,
) -> impl IntoResponse {
    let Ok(mut guard) = lock(&state) else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "lock poisoned").into_response();
    };
    guard.download_calls += 1;
    let call_no = guard.download_calls;
    let body = guard.download_body.clone();
    respond(&guard.download.clone(), call_no, || body.into_response())
}

async fn installations_route(State(state): State<Arc<Mutex<Inner>>>) -> impl IntoResponse {
    let Ok(mut guard) = lock(&state) else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "lock poisoned").into_response();
    };
    guard.installations_calls += 1;
    let call_no = guard.installations_calls;
    let org = guard.installed_org.clone();
    respond(&guard.installations.clone(), call_no, move || {
        Json(json!([{"id": 42, "account": {"login": org}}])).into_response()
    })
}

async fn access_token_route(State(state): State<Arc<Mutex<Inner>>>) -> impl IntoResponse {
    let Ok(mut guard) = lock(&state) else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "lock poisoned").into_response();
    };
    guard.access_token_calls += 1;
    let call_no = guard.access_token_calls;
    respond(&guard.access_token.clone(), call_no, || {
        (
            StatusCode::CREATED,
            Json(json!({"token": "mock-installation-token"})),
        )
            .into_response()
    })
}

#[derive(serde::Deserialize)]
struct SeatsQuery {
    #[serde(default)]
    page: Option<u32>,
}

/// Fabricated seat rows `start..start+count`, 1-indexed ids/logins so a
/// test can assert on exactly which seats came back.
fn seat_objects(start: u32, count: u32) -> Vec<serde_json::Value> {
    (start..start + count)
        .map(|i| {
            json!({
                "created_at": "2026-01-01T00:00:00Z",
                "last_activity_at": "2026-08-01T00:00:00Z",
                "last_activity_editor": "vscode/1.0.0",
                "pending_cancellation_date": null,
                "assignee": {"id": i + 1, "login": format!("user{}", i + 1)},
            })
        })
        .collect()
}

async fn seats_route(
    State(state): State<Arc<Mutex<Inner>>>,
    Path(org): Path<String>,
    Query(query): Query<SeatsQuery>,
) -> impl IntoResponse {
    let Ok(mut guard) = lock(&state) else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "lock poisoned").into_response();
    };
    guard.seats_calls += 1;
    let page = query.page.unwrap_or(1);
    let base_url = guard.base_url.clone();
    let behavior = guard.seats.clone();
    drop(guard);

    match behavior {
        SeatsBehavior::AlwaysFails(status) => {
            let status = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
            (
                status,
                Json(json!({"message": "mock github configured failure"})),
            )
                .into_response()
        }
        SeatsBehavior::MalformedLinkHeader => {
            let mut headers = HeaderMap::new();
            if let Ok(v) = "not-a-valid-link-header".parse() {
                headers.insert(axum::http::header::LINK, v);
            }
            let seats = seat_objects(0, 1);
            (headers, Json(json!({"total_seats": 1, "seats": seats}))).into_response()
        }
        SeatsBehavior::LinkHeaderWithNoNextRelation { page_size } => {
            let seats = seat_objects(0, page_size);
            let mut headers = HeaderMap::new();
            // Well-formed and bracketed, but `rel="last"` -- never "next".
            let last =
                format!("{base_url}/orgs/{org}/copilot/billing/seats?per_page={page_size}&page=1");
            if let Ok(v) = format!("<{last}>; rel=\"last\"").parse() {
                headers.insert(axum::http::header::LINK, v);
            }
            // Claims far more seats exist than this one page holds, so a
            // client that (bug) kept going would have somewhere to go.
            let total = page_size.saturating_mul(10);
            (headers, Json(json!({"total_seats": total, "seats": seats}))).into_response()
        }
        SeatsBehavior::LoopsForever { page_size } => {
            let start = page.saturating_sub(1) * page_size;
            let seats = seat_objects(start, page_size);
            let mut headers = HeaderMap::new();
            let next = format!(
                "{base_url}/orgs/{org}/copilot/billing/seats?per_page={page_size}&page={}",
                page + 1
            );
            if let Ok(v) = format!("<{next}>; rel=\"next\"").parse() {
                headers.insert(axum::http::header::LINK, v);
            }
            (
                headers,
                Json(json!({"total_seats": u32::MAX, "seats": seats})),
            )
                .into_response()
        }
        SeatsBehavior::Paginated { total, page_size } => {
            let start = page.saturating_sub(1) * page_size;
            let remaining = total.saturating_sub(start);
            let count = remaining.min(page_size);
            let seats = seat_objects(start, count);
            let mut headers = HeaderMap::new();
            if start + count < total {
                let next = format!(
                    "{base_url}/orgs/{org}/copilot/billing/seats?per_page={page_size}&page={}",
                    page + 1
                );
                if let Ok(v) = format!("<{next}>; rel=\"next\"").parse() {
                    headers.insert(axum::http::header::LINK, v);
                }
            }
            (headers, Json(json!({"total_seats": total, "seats": seats}))).into_response()
        }
    }
}
