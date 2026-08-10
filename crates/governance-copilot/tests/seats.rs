//! Client-level tests for `GithubClient::fetch_seats` (`src/seats.rs`):
//! pagination across multiple pages, a single page, an empty org, and the
//! two bounded-termination guarantees the pre-implementation review
//! demanded -- a malformed `Link` header stops immediately, and a server
//! that always claims another page is stopped by the page cap rather than
//! hanging the run.
//!
//! Exercised against a local mock server (extends
//! `tests/support/mock_github.rs` rather than inventing a second mock),
//! through the real public `fetch_seats` entry point.

mod support;

use governance_copilot::{GithubClient, RawSecret};
use reqwest::Client as ReqwestClient;
use support::mock_github::{MockGithub, MockGithubConfig, SeatsBehavior};

fn client_for(mock: &MockGithub) -> GithubClient {
    GithubClient::with_api_base(ReqwestClient::new(), mock.base_url.clone())
}

#[tokio::test]
async fn fetch_seats_returns_every_row_on_a_single_page() {
    let mock = MockGithub::start(MockGithubConfig {
        seats: SeatsBehavior::Paginated {
            total: 3,
            page_size: 100,
        },
        ..Default::default()
    })
    .await
    .unwrap();
    let client = client_for(&mock);
    let token = RawSecret::new("test-token".to_owned());

    let fetched = client.fetch_seats("test-org", &token).await.unwrap();

    assert_eq!(fetched.pages.len(), 1, "3 seats fit on one page of 100");
    assert_eq!(mock.seats_call_count().unwrap(), 1);

    let rows =
        governance_copilot::parse_seats(&fetched.to_archive_bytes(), "billing-seats", "2026-08-07")
            .unwrap();
    assert_eq!(rows.len(), 3);
}

#[tokio::test]
async fn fetch_seats_follows_the_link_header_across_multiple_pages() {
    let mock = MockGithub::start(MockGithubConfig {
        seats: SeatsBehavior::Paginated {
            total: 5,
            page_size: 2,
        },
        ..Default::default()
    })
    .await
    .unwrap();
    let client = client_for(&mock);
    let token = RawSecret::new("test-token".to_owned());

    let fetched = client.fetch_seats("test-org", &token).await.unwrap();

    // 5 seats at 2/page: pages of 2, 2, 1.
    assert_eq!(fetched.pages.len(), 3);
    assert_eq!(mock.seats_call_count().unwrap(), 3);

    let rows =
        governance_copilot::parse_seats(&fetched.to_archive_bytes(), "billing-seats", "2026-08-07")
            .unwrap();
    assert_eq!(rows.len(), 5, "every page's rows must be combined");
    let ids: Vec<&str> = rows.iter().map(|r| r.provider_user_id.as_str()).collect();
    assert_eq!(ids, vec!["1", "2", "3", "4", "5"]);
}

#[tokio::test]
async fn fetch_seats_on_an_empty_org_returns_one_page_with_zero_rows() {
    let mock = MockGithub::start(MockGithubConfig {
        seats: SeatsBehavior::Paginated {
            total: 0,
            page_size: 100,
        },
        ..Default::default()
    })
    .await
    .unwrap();
    let client = client_for(&mock);
    let token = RawSecret::new("test-token".to_owned());

    let fetched = client.fetch_seats("test-org", &token).await.unwrap();

    assert_eq!(fetched.pages.len(), 1);
    assert_eq!(mock.seats_call_count().unwrap(), 1);

    let rows =
        governance_copilot::parse_seats(&fetched.to_archive_bytes(), "billing-seats", "2026-08-07")
            .unwrap();
    assert!(
        rows.is_empty(),
        "an empty org must parse to zero rows, not an error"
    );
}

/// A malformed `Link` header on the first page must stop pagination right
/// there -- the client must not, for example, guess "keep going" from the
/// presence of *a* header. Proven by asserting exactly one call was made
/// even though the mock's `total_seats: 1` on a `page_size: 1` config would
/// otherwise imply there could be more.
#[tokio::test]
async fn fetch_seats_stops_immediately_on_a_malformed_link_header() {
    let mock = MockGithub::start(MockGithubConfig {
        seats: SeatsBehavior::MalformedLinkHeader,
        ..Default::default()
    })
    .await
    .unwrap();
    let client = client_for(&mock);
    let token = RawSecret::new("test-token".to_owned());

    let fetched = client.fetch_seats("test-org", &token).await.unwrap();

    assert_eq!(
        fetched.pages.len(),
        1,
        "a malformed Link header must not be treated as \"keep going\""
    );
    assert_eq!(
        mock.seats_call_count().unwrap(),
        1,
        "pagination must stop after the first page, not retry or continue"
    );
}

/// A syntactically well-formed `Link` header whose only relation is
/// `rel="last"` must also stop pagination at one page -- proves the
/// `rel="next"` check itself is what gates continuation, not merely
/// "some Link header was present" (the malformed-header test above only
/// proves the latter, since its header has no brackets to parse at all).
#[tokio::test]
async fn fetch_seats_does_not_follow_a_link_header_with_no_next_relation() {
    let mock = MockGithub::start(MockGithubConfig {
        seats: SeatsBehavior::LinkHeaderWithNoNextRelation { page_size: 2 },
        ..Default::default()
    })
    .await
    .unwrap();
    let client = client_for(&mock);
    let token = RawSecret::new("test-token".to_owned());

    let fetched = client.fetch_seats("test-org", &token).await.unwrap();

    assert_eq!(
        fetched.pages.len(),
        1,
        "a rel=\"last\"-only Link header must not be followed as if it were rel=\"next\""
    );
    assert_eq!(mock.seats_call_count().unwrap(), 1);
}

/// A server that always claims another page (a broken or malicious host)
/// must not hang the run forever -- the client's page cap has to be what
/// stops it. This is the direct proof that `MAX_SEAT_PAGES` (seats.rs) is
/// actually wired into `fetch_seats`, not just a constant that exists.
///
/// Broken against a version of `fetch_seats` with no page cap: this test
/// would never terminate (or would terminate only when the test harness's
/// own timeout killed it) -- see the module's report for the exact
/// before/after call counts.
#[tokio::test]
async fn fetch_seats_terminates_against_a_link_header_that_always_claims_a_next_page() {
    let mock = MockGithub::start(MockGithubConfig {
        seats: SeatsBehavior::LoopsForever { page_size: 100 },
        ..Default::default()
    })
    .await
    .unwrap();
    let client = client_for(&mock);
    let token = RawSecret::new("test-token".to_owned());

    let err = client
        .fetch_seats("test-org", &token)
        .await
        .expect_err("a server that never stops paginating must surface as an error, not hang");

    assert!(
        err.to_string().contains("exceeded") || err.to_string().contains("pages"),
        "expected the page-cap error, got: {err}"
    );
    // The exact bound (MAX_SEAT_PAGES = 200 in seats.rs) is intentionally
    // not asserted here byte-for-byte (that would couple this test to an
    // internal constant); bounded-and-small is the property that matters.
    let calls = mock.seats_call_count().unwrap();
    assert!(
        (1..=1000).contains(&calls),
        "expected a small, bounded number of calls, got {calls}"
    );
}

#[tokio::test]
async fn fetch_seats_surfaces_a_deterministic_failure() {
    let mock = MockGithub::start(MockGithubConfig {
        seats: SeatsBehavior::AlwaysFails(403),
        ..Default::default()
    })
    .await
    .unwrap();
    let client = client_for(&mock);
    let token = RawSecret::new("test-token".to_owned());

    let err = client.fetch_seats("test-org", &token).await.unwrap_err();

    assert!(
        err.to_string().contains("403"),
        "expected a 403 error, got: {err}"
    );
}
