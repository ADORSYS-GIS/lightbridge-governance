//! Old envelopes must recover their signal without changing bytes or retry keys.

use super::{DurableSpool, Signal, TempDir};
use crate::{copilot::quarantine::Quarantine, otel_daemon::receive::WireFormat};

#[test]
fn a_legacy_log_envelope_carrying_metrics_is_delivered_as_metrics() {
    let dir = TempDir::new("misrouted-metrics");
    let mut spool = dir.spool("a");
    let body = b"\x0a\x09\x12\x07\x12\x05\x0a\x01m\x3a\x00";
    spool
        .retain(Signal::Logs, body.to_vec(), WireFormat::Protobuf)
        .expect("old retain");
    let before = std::fs::read_to_string(&spool.spool_path).expect("spool");
    let pending = spool.next().expect("read").expect("pending");
    assert_eq!(pending.signal, Signal::Metrics);
    assert_eq!(pending.payload, body);
    assert_eq!(pending.key, Quarantine::key(before.trim_end()));
    assert_eq!(
        std::fs::read_to_string(&spool.spool_path).expect("spool"),
        before
    );
    spool.advance(&pending).expect("advance");
    assert_eq!(spool.checkpoint.discarded_total, 0);
    let mut reopened = DurableSpool::at(spool.spool_path, spool.checkpoint_path).expect("reopen");
    assert!(reopened.next().expect("next").is_none());
}

#[test]
fn traces_survive_a_restart_and_keep_their_own_destination() {
    let dir = TempDir::new("trace-restart");
    let mut spool = dir.spool("a");
    let body = br#"{"resourceSpans":[]}"#;
    spool
        .retain(Signal::Traces, body.to_vec(), WireFormat::Json)
        .expect("retain");
    let mut reopened = DurableSpool::at(spool.spool_path, spool.checkpoint_path).expect("reopen");
    let pending = reopened.next().expect("next").expect("pending");
    assert_eq!(pending.signal.path(), "/v1/traces");
    assert_eq!(pending.payload, body);
    reopened.advance(&pending).expect("advance");
    assert!(reopened.is_empty().expect("caught up"));
}

#[test]
fn refusals_from_the_old_destination_do_not_count_against_recovered_metrics() {
    let dir = TempDir::new("refusal-destination");
    let mut spool = dir.spool("a");
    let body = b"\x0a\x09\x12\x07\x12\x05\x0a\x01m\x3a\x00";
    spool
        .retain(Signal::Logs, body.to_vec(), WireFormat::Protobuf)
        .expect("old retain");
    let pending = spool.next().expect("next").expect("pending");
    let now = 1_800_000_000;
    spool.checkpoint.quarantine.refused(&pending.key, now, 60);
    spool
        .checkpoint
        .quarantine
        .refused(&pending.key, now + 60, 60);
    assert!(
        !spool
            .record_refusal(&pending, now + 120)
            .expect("first metrics refusal"),
        "old logs refusals are not evidence against the metrics destination"
    );
    assert!(
        spool
            .record_refusal(&pending, now + 180)
            .expect("second metrics refusal")
    );
}
