//! The pure window math for a backfill run: which `[start, end]` (inclusive)
//! range of report days to (re-)ingest. Split out of `backfill.rs` (#178) so
//! the logic is unit-testable in isolation (`window_tests.rs`).

use crate::sync::config::COPILOT_DATA_LAG_DAYS;

/// The `[start, end]` (inclusive) window of report days a `sync` run should
/// (re-)ingest. Three RFC-0001 requirements, one expression:
///
/// - **The upper bound is D-1, never D-0 ("today").** GitHub's Copilot
///   metrics have same-day latency: a request for `today` always fails with
///   HTTP 400 ("Date must be within the last year and not in the future").
///   RFC-0001 §Scheduling is explicit that a run "re-fetches D-1, D-2 and
///   D-3", not D-0 through D-2 -- confirmed live in production, where every
///   6h run was burning four guaranteed-to-fail requests (one per report
///   type) on `today` and logging a WARN for each, forever. `today` is still
///   the parameter's meaning (the real calendar day the run executes on);
///   every bound below is computed relative to it, but the window itself
///   never extends past `today - 1`.
/// - **Always re-fetch the trailing `lookback_days`**, regardless of where
///   the high-water mark sits. This is what makes a late-published report
///   self-heal with no operator action, and it is what stops the
///   high-water mark from permanently orphaning a day: a manifest row for a
///   *later* day (even an "empty" 204 one) can legitimately push the
///   high-water mark past an earlier day that never actually finished, but
///   that earlier day is still re-fetched here as long as it is within the
///   trailing window.
/// - **Still close any gap after the high-water mark** -- cold start, or
///   catching up after an outage that left it stale for a while.
/// - **Never walk back further than `max_backfill_days`**, so a cold start
///   (or a watermark that never advanced) cannot stampede the API.
///
/// `end = today - COPILOT_DATA_LAG_DAYS` (currently 1 day),
/// `start = max(min(hwm + 1, end, today - lookback_days), today - max_backfill_days)`.
/// See the `backfill_window_*` tests in `window_tests.rs` for the three cases
/// this is required to get right (recent/stale/absent high-water mark) plus
/// the exact re-orphaning scenario from the review and the D-1-upper-bound
/// regression.
pub fn backfill_window(
    hwm: Option<chrono::NaiveDate>,
    today: chrono::NaiveDate,
    lookback_days: i64,
    max_backfill_days: i64,
) -> (chrono::NaiveDate, chrono::NaiveDate) {
    // The most recent day GitHub can ever answer for. This is the ONLY place
    // that decides that -- every caller (`run_backfill`/`run_backfill_at`)
    // passes real calendar `today` straight through; `sync-day` bypasses
    // this function entirely and is unaffected (an explicit operator
    // request for "today" should still be tried and get GitHub's real
    // error, not be silently blocked here).
    let end = today - chrono::Days::new(COPILOT_DATA_LAG_DAYS);
    let lookback_bound = today - chrono::Days::new(lookback_days.max(0) as u64);
    let max_backfill_bound = today - chrono::Days::new(max_backfill_days.max(0) as u64);
    let start = match hwm {
        Some(h) => {
            // Resume the day after the high-water mark, but never past the
            // last queryable day -- a stale-in-the-other-direction hwm (or a
            // 0-day lookback in a test config) must not push `start` beyond
            // `end`.
            let resume = (h + chrono::Days::new(1)).min(end);
            resume.min(lookback_bound).max(max_backfill_bound)
        }
        // No manifest row exists at all: treat as a cold start, bounded by
        // max_backfill_days rather than only the (much shorter) trailing
        // lookback window.
        None => max_backfill_bound,
    };
    (start, end)
}
