//! `compact_if_stuck` -- the gap `try_reclaim`'s exact-catch-up truncate
//! cannot close. Split from [`super`] for the LoC gate, same as
//! [`super::reclaim`].

use super::{super::commit::RECLAIM_ABOVE, DurableSpool, FORMAT, TempDir};
use crate::otel_daemon::signal::Signal;

/// The property that matters most: compaction never loses, duplicates, or
/// reorders a still-pending record, even while the spool is deliberately
/// NEVER presented with an exact "fully caught up" instant -- the one
/// condition `try_reclaim` requires and this exists because continuous
/// traffic can withhold indefinitely.
#[test]
fn compaction_preserves_every_still_pending_record_in_order() {
    let dir = TempDir::new("compact-preserve");
    let mut spool = dir.spool("a");

    // Enough delivered bytes to cross RECLAIM_ABOVE, so `checkpoint.offset`
    // (the dead prefix) is worth compacting away.
    let delivered = vec![b'd'; usize::try_from(RECLAIM_ABOVE).unwrap_or(usize::MAX) + 1024];
    spool
        .retain(Signal::Logs, delivered, FORMAT)
        .expect("retain the big delivered record");
    let big = spool.next().expect("next").expect("the big record");

    // Two more records land BEFORE the big one is advanced past -- so when
    // `advance` below runs its own internal `try_reclaim`, `size != offset`
    // (the two pending records are already on disk past the big one) and it
    // correctly declines. That is the whole scenario this test exists for:
    // continuous traffic that never lets the spool present an exact
    // "fully caught up" instant, which is exactly the case `try_reclaim`
    // cannot touch and `compact_if_stuck` has to.
    spool
        .retain(Signal::Metrics, b"still-pending-one".to_vec(), FORMAT)
        .expect("retain first pending");
    spool
        .retain(Signal::Logs, b"still-pending-two".to_vec(), FORMAT)
        .expect("retain second pending");

    spool.advance(&big).expect("advance past the big record");

    let before_offset = spool.checkpoint.offset;
    assert!(
        before_offset > RECLAIM_ABOVE,
        "the test setup must actually cross the threshold, got {before_offset}"
    );

    let compacted = spool.compact_if_stuck().expect("compact");
    assert!(compacted, "a dead prefix over the threshold must compact");
    assert_eq!(
        spool.checkpoint.offset, 0,
        "the offset must reset against the newly compacted file"
    );

    // Both still-pending records must survive, in order, with their exact
    // payload and signal -- nothing lost, nothing duplicated.
    let first = spool
        .next()
        .expect("next")
        .expect("first still-pending record must survive compaction");
    assert_eq!(first.payload, b"still-pending-one");
    assert_eq!(first.signal, Signal::Metrics);
    spool.advance(&first).expect("advance");

    let second = spool
        .next()
        .expect("next")
        .expect("second still-pending record must survive compaction");
    assert_eq!(second.payload, b"still-pending-two");
    assert_eq!(second.signal, Signal::Logs);
    spool.advance(&second).expect("advance");

    assert!(
        spool.next().expect("next").is_none(),
        "nothing must be conjured that was never retained"
    );
}

#[test]
fn compaction_is_a_no_op_below_the_threshold() {
    let dir = TempDir::new("compact-below-threshold");
    let mut spool = dir.spool("a");
    spool
        .retain(Signal::Logs, b"small".to_vec(), FORMAT)
        .expect("retain");
    let pending = spool.next().expect("next").expect("pending");
    spool.advance(&pending).expect("advance");

    assert!(
        spool.checkpoint.offset <= RECLAIM_ABOVE,
        "test setup must stay under the threshold"
    );
    assert!(
        !spool.compact_if_stuck().expect("compact"),
        "a dead prefix under the threshold must not be rewritten"
    );
}

/// Mirrors `reclaim`'s own
/// `a_stale_offset_after_an_unfinished_reclaim_does_not_wedge_the_drain`:
/// a crash between the rename and the checkpoint reset leaves
/// `{checkpoint: offset=N (stale), file: already just the compacted tail}`.
/// The file is shorter than the stale offset (and a different inode), which
/// `read`'s restart detection already treats as self-healing -- nothing new
/// is required here, this pins that it actually holds for compaction's own
/// crash window too.
#[test]
fn a_stale_offset_after_an_unfinished_compaction_does_not_wedge_the_drain() {
    let dir = TempDir::new("compact-post-crash");
    let spool_path = dir.0.join("spool.crash");
    let checkpoint_path = dir.0.join("checkpoint.crash.json");

    // The file exactly as a completed rename would have just left it:
    // present, holding only what was the pending tail.
    std::fs::write(&spool_path, b"").expect("seed the post-rename (empty-tail) spool file");
    // The checkpoint exactly as it was BEFORE compact_if_stuck's second
    // store -- still carrying the large, pre-compaction offset.
    let stale = super::super::checkpoint::Checkpoint {
        offset: RECLAIM_ABOVE + 1024,
        ..Default::default()
    };
    super::super::checkpoint::store(&checkpoint_path, &stale).expect("seed the stale checkpoint");

    let mut spool =
        DurableSpool::at(spool_path, checkpoint_path).expect("open against the post-crash state");
    assert!(
        !spool.is_empty().expect("check"),
        "a stale offset far larger than the compacted file's size must not read as caught up"
    );

    spool
        .retain(Signal::Logs, b"after-the-crash".to_vec(), FORMAT)
        .expect("retain");
    let pending = spool
        .next()
        .expect("next must detect the restart and recover, not stay wedged")
        .expect("the newly retained record must be reachable");
    assert_eq!(pending.payload, b"after-the-crash");
}
