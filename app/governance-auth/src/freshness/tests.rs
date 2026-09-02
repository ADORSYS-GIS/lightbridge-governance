use super::*;
use crate::redacted::Redacted;

fn session(expires_at: u64, lifetime_secs: Option<u64>) -> CachedSession {
    CachedSession {
        issuer: "https://issuer.example.com".to_owned(),
        client_id: "client".to_owned(),
        access_token: Redacted::new("access-token".to_owned()),
        refresh_token: None,
        expires_at,
        lifetime_secs,
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or_default()
}

/// (c) The 30s rule is unchanged for callers that use the token themselves
/// (`copilot push`, `status`). Widening it for them would refresh -- and so
/// rotate a single-use refresh token -- far more often than they need.
#[test]
fn is_fresh_still_means_exactly_the_thirty_second_skew() -> Result<()> {
    assert!(
        session(now() + 31, Some(900)).is_fresh()?,
        "31s of life is outside the 30s skew and must stay usable"
    );
    assert!(
        !session(now() + 29, Some(900)).is_fresh()?,
        "29s of life is inside the 30s skew and must stay unusable"
    );
    Ok(())
}

#[test]
fn skew_freshness_requires_only_the_skew() {
    assert_eq!(
        Freshness::Skew.required_remaining_secs(&session(now() + 600, Some(900))),
        SKEW_SECONDS
    );
}

/// The defect in one assertion: a 240 000ms debounce means a token must have
/// 270s of life left, not 30s.
#[test]
fn a_cached_caller_requires_its_whole_window_plus_the_skew() {
    assert_eq!(
        Freshness::for_helper(240_000).required_remaining_secs(&session(now() + 600, Some(900))),
        270
    );
}

#[test]
fn a_session_with_no_recorded_lifetime_is_not_capped() {
    assert_eq!(
        Freshness::for_helper(600_000).required_remaining_secs(&session(now() + 200, None)),
        630
    );
}

/// (d) A window that no token this server issues could ever satisfy is
/// capped at half the observed lifetime, so the helper does not refresh on
/// every invocation.
#[test]
fn a_window_longer_than_the_token_lifetime_is_capped_at_half_the_lifetime() {
    let capped =
        Freshness::for_helper(600_000).required_remaining_secs(&session(now() + 200, Some(300)));
    assert_eq!(capped, 150);
    assert!(
        session(now() + 300, Some(300)).is_fresh_for(capped).is_ok(),
        "a freshly minted token must clear the capped requirement, or the cap loops"
    );
}

/// The cap must never make the helper LESS careful than the plain skew.
#[test]
fn the_cap_never_drops_below_the_skew() {
    assert_eq!(
        Freshness::for_helper(600_000).required_remaining_secs(&session(now() + 10, Some(20))),
        SKEW_SECONDS
    );
}
