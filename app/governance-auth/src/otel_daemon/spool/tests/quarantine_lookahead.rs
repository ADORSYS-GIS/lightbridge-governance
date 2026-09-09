//! `discard_confirmed`'s multi-record `lost` accounting. Split from
//! [`super::quarantine`] purely for the 200-LoC gate.

use super::{super::commit::MIN_SEPARATION_SECONDS, FORMAT, Signal, TempDir};

/// An arbitrary anchor timestamp -- these tests never touch real wall-clock
/// time, only offsets from this.
const NOW: u64 = 1_800_000_000;

/// `discard_confirmed`'s `lost` count is what makes a multi-record skip (two
/// or more consecutive permanently-refused records, walked by
/// `drain::quarantine::handle`'s bounded lookahead -- `peek_next` itself only
/// ever looks one record ahead per call) show up correctly in
/// `discarded_total`: it must count every record given up on, not just
/// `stuck`. Exercised here at the boundary this module owns (a single commit
/// discarding more than one record); the lookahead loop itself is exercised
/// end to end in `tests/serve_otel_quarantine_lookahead.rs` against a real
/// mock collector, since it is what decides *whether* to call this with
/// `lost` > 1 in the first place.
#[test]
fn discard_confirmed_charges_every_record_it_skips_to_discarded_total() {
    let dir = TempDir::new("quarantine-multi-lost");
    let mut spool = dir.spool("a");
    spool
        .retain(Signal::Logs, b"stuck".to_vec(), FORMAT)
        .expect("retain stuck");
    spool
        .retain(Signal::Logs, b"also-stuck".to_vec(), FORMAT)
        .expect("retain also-stuck");
    spool
        .retain(Signal::Logs, b"proof".to_vec(), FORMAT)
        .expect("retain proof");

    let stuck = spool.next().expect("next").expect("stuck pending");
    assert!(!spool.record_refusal(&stuck, NOW).expect("refusal 1"));
    let stuck_again = spool.next().expect("next").expect("still stuck");
    assert!(
        spool
            .record_refusal(&stuck_again, NOW + MIN_SEPARATION_SECONDS)
            .expect("refusal 2"),
        "eligible now"
    );

    // The lookahead's own job (walking past `also-stuck` to `proof`) is not
    // this module's concern -- this test starts from where that walk would
    // have already landed: `proof`, two records past `stuck`.
    let also_stuck = spool
        .peek_next(&stuck_again)
        .expect("peek")
        .expect("also-stuck is available to peek");
    let proof = spool
        .peek_next(&also_stuck)
        .expect("peek")
        .expect("proof is available to peek");

    spool
        .discard_confirmed(&stuck_again, 2, &proof)
        .expect("discard both stuck records, delivering proof in the same commit");

    assert_eq!(
        spool.checkpoint.discarded_total, 2,
        "both `stuck` and `also-stuck` were given up on, not just `stuck` -- a `lost` of 1 here \
         would silently undercount every multi-record skip"
    );
    assert!(
        spool.next().expect("next").is_none(),
        "all three records are now resolved: two discarded, one delivered"
    );
}
