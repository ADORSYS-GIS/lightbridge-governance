//! How a session lifetime is worded. Split from the parent test file to keep
//! both under the 200-LoC gate.

use super::{super::style::since, *};

/// Found on the VM, not in a test: `expires_in` goes negative once the token is
/// past its `exp` -- the normal state while waiting for a refresh -- and the
/// dashboard printed `needs refresh, -8338s`. Every fixture here used a
/// positive value, so nothing caught it.
#[test]
fn an_expired_session_does_not_print_negative_seconds() {
    let out = table(
        "i",
        "c",
        &expiring(true, false, -8338),
        &otel(None, false),
        &[],
    );
    // Assert on the session line only: the empty-state row contains
    // "governance-auth configure", whose hyphen made a whole-output check for
    // '-' fail against correct code.
    let line = out
        .lines()
        .find(|l| l.contains("session"))
        .expect("session row");
    assert!(!line.contains("-8338"), "raw negative seconds: {line}");
    assert!(line.contains("expired"), "{line}");
    assert!(line.contains("ago"), "{line}");
}

#[test]
fn durations_scale_with_magnitude() {
    assert_eq!(ago(45), "45s left");
    assert_eq!(ago(900), "15m left");
    assert_eq!(ago(7200), "2h left");
    assert_eq!(ago(-30), "expired 30s ago");
    assert_eq!(ago(-8338), "expired 3h ago");
    // Zero is neither past nor future; it must not read as expired.
    assert_eq!(ago(0), "0s left");
}

/// Elapsed time has its own renderer because `ago`'s two answers are both
/// wrong for it: a discard that just happened came out as "last 0s left"
/// (`ago` reads 0 as remaining lifetime), and anything older came out as
/// "last expired 60s ago" -- wording borrowed from session expiry, and nothing
/// about a discarded record expires.
#[test]
fn elapsed_time_reads_as_elapsed_not_as_a_remaining_lifetime() {
    assert_eq!(since(0), "just now");
    assert_eq!(since(45), "45s ago");
    assert_eq!(since(120), "2m ago");
    assert_eq!(since(7200), "2h ago");
    for rendered in [since(0), since(45), since(7200)] {
        assert!(!rendered.contains("expired"), "{rendered}");
        assert!(!rendered.contains("left"), "{rendered}");
    }
}
