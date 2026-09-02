//! Reclaiming the spool, and the one thing it may never do.
//!
//! The rule these pin is [`crate::copilot`]'s: a byte only ever leaves the
//! spool having been delivered or counted. A truncate does not advance over
//! bytes, it destroys them, so the whole of its correctness is the
//! precondition -- `size == offset`, exactly. Every test here is either that
//! precondition holding or that precondition refusing.

use super::{
    super::{
        checkpoint::{self, Checkpoint},
        spool::{
            self,
            reclaim::{self, RECLAIM_ABOVE},
        },
    },
    TempDir, log_line,
};

/// Enough records to clear [`RECLAIM_ABOVE`], plus one, so a test that means
/// "over the threshold" cannot be quietly turned into "at" it.
fn oversized() -> String {
    let one = format!("{}\n", log_line());
    let count = (RECLAIM_ABOVE as usize / one.len()).saturating_add(2);
    one.repeat(count)
}

/// The state a wake leaves behind when it delivered everything it read --
/// including the identity of the file it read, because that is what
/// `Journal::commit` records and it is what a reclaim then has to replace.
fn caught_up(spool: &std::path::Path, offset: u64) -> Checkpoint {
    let identity = spool::drain(spool, offset, None)
        .expect("reading the spool")
        .identity;
    Checkpoint {
        offset,
        metrics_offset: Some(offset),
        logs_offset: Some(offset),
        spool: identity,
        ..Checkpoint::default()
    }
}

fn size(path: &std::path::Path) -> u64 {
    std::fs::metadata(path).expect("sizing the spool").len()
}

/// The ordinary case, and the whole point of the change: an oversized spool
/// every byte of which has been delivered goes back to zero, and the
/// checkpoint that described it goes with it.
#[test]
fn a_caught_up_spool_over_the_threshold_is_truncated() {
    let dir = TempDir::new("reclaim-caught-up");
    let spool = dir.file("spool.jsonl");
    let state_path = dir.file("copilot-push.json");
    std::fs::write(&spool, oversized()).expect("writing the spool");
    let delivered = size(&spool);

    let before = caught_up(&spool, delivered);
    let reclaimed =
        reclaim::maybe(&spool, &state_path, &before).expect("the reclaim must not fail");

    assert_eq!(reclaimed, Some(delivered), "it reports what it destroyed");
    assert_eq!(size(&spool), 0, "the file is the thing being reclaimed");
    let state = checkpoint::load(&state_path).expect("the checkpoint must be rewritten");
    assert_eq!(state.offset, 0, "a byte count into bytes that are gone");
    assert_eq!(state.metrics_offset, Some(0));
    assert_eq!(state.logs_offset, Some(0));
    assert!(
        state.spool.is_some() && state.spool != before.spool,
        "and it must name the file as it is NOW: an identity digested over 4 KiB the truncate \
         just deleted makes every later wake report a rotation"
    );
}

/// ⚠️ THE conservation test. Copilot appends between the drain's read and the
/// reclaim -- the ordinary case on a machine anybody is using -- and those
/// bytes have been delivered to nobody and counted nowhere. Truncating here
/// would destroy them silently, which is the one outcome the invariant has no
/// room for.
///
/// Falsified by deleting the `size != delivered` guard in
/// `spool::reclaim::maybe`: this fails on all three assertions, the last one
/// naming the record that vanished.
#[test]
fn a_record_appended_after_the_drain_is_never_destroyed() {
    let dir = TempDir::new("reclaim-raced");
    let spool = dir.file("spool.jsonl");
    let state_path = dir.file("copilot-push.json");
    let drained = oversized();
    let delivered = drained.len() as u64;
    let undelivered = format!(
        "{}\n",
        log_line().replace("manage_todo_list", "raced-in-record")
    );
    std::fs::write(&spool, format!("{drained}{undelivered}")).expect("writing the spool");

    let reclaimed = reclaim::maybe(&spool, &state_path, &caught_up(&spool, delivered))
        .expect("the reclaim must not fail");

    assert_eq!(
        reclaimed, None,
        "a spool with bytes past the checkpoint is not caught up, whatever its size"
    );
    assert!(
        !state_path.exists(),
        "and a reclaim that did not happen must not rewrite the checkpoint either"
    );
    let after = std::fs::read_to_string(&spool).expect("re-reading the spool");
    assert!(
        after.contains("raced-in-record"),
        "the record appended after the drain was destroyed: undelivered, uncounted, and \
         unrecoverable -- {} bytes left of {}",
        after.len(),
        delivered.saturating_add(undelivered.len() as u64)
    );
}

/// The same refusal, in the shape it actually takes: Copilot's append is half
/// written when the drain reads, so the offset stops at the last newline and
/// the file is longer than it by a fragment.
#[test]
fn a_trailing_partial_line_blocks_the_reclaim() {
    let dir = TempDir::new("reclaim-partial");
    let spool = dir.file("spool.jsonl");
    let state_path = dir.file("copilot-push.json");
    let drained = oversized();
    let delivered = drained.len() as u64;
    std::fs::write(&spool, format!("{drained}{{\"hrTime\":[178")).expect("writing the spool");

    assert_eq!(
        reclaim::maybe(&spool, &state_path, &caught_up(&spool, delivered)).expect("no error"),
        None,
        "the fragment is a record nobody has seen the end of, let alone delivered"
    );
    assert!(size(&spool) > delivered, "and it is still there");
}

/// A spool nobody needs reclaiming is one the race above is not worth running
/// for. Under the threshold nothing is touched at all -- not the file, not the
/// checkpoint.
#[test]
fn a_spool_under_the_threshold_is_left_alone() {
    let dir = TempDir::new("reclaim-small");
    let spool = dir.file("spool.jsonl");
    let state_path = dir.file("copilot-push.json");
    std::fs::write(&spool, format!("{}\n", log_line())).expect("writing the spool");
    let delivered = size(&spool);
    assert!(delivered < RECLAIM_ABOVE, "the fixture must be small");

    assert_eq!(
        reclaim::maybe(&spool, &state_path, &caught_up(&spool, delivered)).expect("no error"),
        None
    );
    assert_eq!(size(&spool), delivered);
    assert!(!state_path.exists());
}

/// The reclaim has to leave the *next* wake correct, and the trap is the
/// identity: an offset of 0 beside an identity describing the file as it was
/// before the truncate reads as a rotation on every later wake, and a
/// rotation re-reports itself for ever.
#[test]
fn the_wake_after_a_reclaim_reads_from_zero_and_reports_no_rotation() {
    let dir = TempDir::new("reclaim-next-wake");
    let spool = dir.file("spool.jsonl");
    let state_path = dir.file("copilot-push.json");
    std::fs::write(&spool, oversized()).expect("writing the spool");
    let delivered = size(&spool);
    reclaim::maybe(&spool, &state_path, &caught_up(&spool, delivered)).expect("the reclaim");
    let state = checkpoint::load(&state_path).expect("the checkpoint");

    // Copilot carries on into the truncated file, which its O_APPEND handles
    // resume at byte 0.
    std::fs::write(&spool, format!("{}\n{}\n", log_line(), log_line())).expect("appending");

    let next = spool::drain(&spool, state.offset, state.spool.as_ref()).expect("the next drain");
    assert_eq!(
        next.restarted, None,
        "the checkpoint already describes the truncated file, so nothing was rotated under it"
    );
    assert_eq!(next.lines.len(), 2, "and both new records are read");
}
