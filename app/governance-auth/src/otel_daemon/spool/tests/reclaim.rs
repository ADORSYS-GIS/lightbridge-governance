//! Reclaim-on-catch-up and `is_empty` -- split from [`super`] for the LoC
//! gate. The last test here is the regression for #269/#291 review round 2's
//! P1: a crash between `try_reclaim`'s truncate and its checkpoint reset must
//! not wedge the drain.

use super::{super::commit::RECLAIM_ABOVE, DurableSpool, TempDir};
use crate::copilot::Signal;

#[test]
fn a_fully_delivered_spool_over_the_reclaim_threshold_is_truncated() {
    let dir = TempDir::new("reclaim");
    let spool_path = dir.0.join("spool.reclaim");
    let checkpoint_path = dir.0.join("checkpoint.reclaim.json");
    let mut spool = DurableSpool::at(spool_path.clone(), checkpoint_path).expect("open");

    // One record safely over RECLAIM_ABOVE, so the very first advance already
    // meets the reclaim precondition (size == offset).
    let big = vec![b'x'; usize::try_from(RECLAIM_ABOVE).unwrap_or(usize::MAX) + 1024];
    spool.retain(Signal::Logs, big).expect("retain");
    let pending = spool.next().expect("next").expect("pending");
    spool.advance(&pending).expect("advance");

    let size = std::fs::metadata(&spool_path).expect("stat").len();
    assert_eq!(
        size, 0,
        "a fully-delivered spool over the threshold must be reclaimed"
    );
}

#[test]
fn is_empty_reflects_pending_bytes_not_file_existence() {
    let dir = TempDir::new("is-empty");
    let mut spool = dir.spool("a");
    assert!(spool.is_empty().expect("no file yet"), "no file at all");

    spool.retain(Signal::Logs, b"one".to_vec()).expect("retain");
    assert!(!spool.is_empty().expect("check"), "one record pending");

    let pending = spool.next().expect("next").expect("pending");
    spool.advance(&pending).expect("advance");
    assert!(
        spool.is_empty().expect("check"),
        "delivered, nothing left pending"
    );
}

/// #269/#291 review round 2, P1: a crash between `try_reclaim`'s
/// `set_len(0)` and its checkpoint-reset store leaves disk state
/// `{checkpoint: offset=N (stale, large), file: truncated to 0}`. `is_empty`
/// used to trust a raw `size <= offset` compare, which reads that state as
/// "caught up" forever -- even once new records are appended starting from
/// byte 0 -- wedging the drain permanently: the client keeps getting `202`,
/// but nothing is ever offered to the collector again. Seeded here by hand,
/// since actually killing the process mid-`try_reclaim` is what
/// `tests/serve_otel_durability.rs`'s SIGKILL tests exercise at a much
/// coarser grain; this pins the exact boundary condition.
#[test]
fn a_stale_offset_after_an_unfinished_reclaim_does_not_wedge_the_drain() {
    let dir = TempDir::new("post-crash-truncate");
    let spool_path = dir.0.join("spool.crash");
    let checkpoint_path = dir.0.join("checkpoint.crash.json");

    // The file exactly as `try_reclaim`'s `set_len(0)` would have just left
    // it: present, empty.
    std::fs::write(&spool_path, b"").expect("seed an empty spool file");
    // The checkpoint exactly as it was BEFORE `try_reclaim`'s second store
    // -- still carrying the large, pre-truncation offset, because the crash
    // landed before that reset ever reached disk.
    let stale = super::super::checkpoint::Checkpoint {
        offset: RECLAIM_ABOVE + 1024,
        ..Default::default()
    };
    super::super::checkpoint::store(&checkpoint_path, &stale).expect("seed the stale checkpoint");

    let mut spool =
        DurableSpool::at(spool_path, checkpoint_path).expect("open against the post-crash state");
    assert!(
        !spool.is_empty().expect("check"),
        "a stale offset far larger than the truncated file's size must not read as caught up \
         -- that is exactly the wedge this test guards against"
    );

    spool
        .retain(Signal::Logs, b"after-the-crash".to_vec())
        .expect("retain");
    let pending = spool
        .next()
        .expect("next must detect the restart and recover, not stay wedged")
        .expect("the newly retained record must be reachable");
    assert_eq!(pending.payload, b"after-the-crash");
}
