//! `report`'s pass/fail aggregation -- the pure part of `doctor`. Everything
//! else in this module (`mint`, `gateway_check`, `run` itself) mints real
//! credentials and makes real HTTP requests, covered by manual verification
//! against a real config (see this feature's own commit message for the
//! specifics), not mocked here -- the same boundary `update.rs`'s own tests
//! draw around `install`/`download`.

use super::*;

#[test]
fn all_passing_checks_is_ok() {
    let checks = vec![Check::pass("a", "fine"), Check::pass("b", "also fine")];
    assert!(report(&checks).is_ok());
}

#[test]
fn one_failure_among_passes_is_an_error_naming_the_count() {
    let checks = vec![
        Check::pass("a", "fine"),
        Check::fail("b", "broken"),
        Check::pass("c", "fine"),
    ];
    let error = report(&checks).expect_err("one failed check must fail the whole report");
    let message = format!("{error:#}");
    assert!(message.contains('1'), "{message}");
    assert!(message.contains('3'), "{message}");
}

#[test]
fn every_check_failing_is_still_one_error_not_a_panic() {
    let checks = vec![Check::fail("a", "broken"), Check::fail("b", "also broken")];
    assert!(report(&checks).is_err());
}

#[test]
fn an_empty_report_is_vacuously_ok() {
    // Never actually reachable from `run` (it always pushes at least the
    // credential check), but `report` itself must not divide by, or
    // otherwise choke on, zero.
    assert!(report(&[]).is_ok());
}
