//! Identity changes must return a record, not spin while holding the spool lock.

use std::{sync::mpsc, thread, time::Duration};

use super::{DurableSpool, FORMAT, TempDir};
use crate::otel_daemon::signal::Signal;

#[test]
fn a_changed_device_identity_recovers_and_persists_only_after_advance() {
    let worker = thread::spawn(|| {
        let dir = TempDir::new("device-restart");
        let mut spool = dir.spool("a");
        spool
            .retain(Signal::Logs, b"first".to_vec(), FORMAT)
            .expect("retain");
        let first = spool.next().expect("next").expect("first");
        spool.advance(&first).expect("advance");
        spool
            .retain(Signal::Logs, b"second".to_vec(), FORMAT)
            .expect("retain");

        // Preserve the actual inode and content digest; only the recorded
        // device differs, reproducing the workstation's stale checkpoint.
        let mut stale = serde_json::to_value(&spool.checkpoint).expect("serialize");
        stale["spool"]["device"] = serde_json::json!(u64::MAX);
        spool.checkpoint = serde_json::from_value(stale).expect("deserialize");
        super::super::checkpoint::store(&spool.checkpoint_path, &spool.checkpoint)
            .expect("store stale checkpoint");
        let before = std::fs::read(&spool.checkpoint_path).expect("read checkpoint");
        let mut reopened =
            DurableSpool::at(spool.spool_path.clone(), spool.checkpoint_path.clone())
                .expect("reopen");
        assert!(!reopened.is_empty().expect("check identity"));
        let replay = reopened.next().expect("recover").expect("first replayed");
        assert_eq!(replay.payload, b"first");
        assert_eq!(
            std::fs::read(&spool.checkpoint_path).expect("read checkpoint"),
            before,
            "observing a replacement must not persist delivery progress"
        );
        reopened.advance(&replay).expect("commit new identity");
        let mut reopened =
            DurableSpool::at(spool.spool_path.clone(), spool.checkpoint_path.clone())
                .expect("reopen after commit");
        let second = reopened.next().expect("next").expect("second");
        assert_eq!(second.payload, b"second");
        reopened.advance(&second).expect("advance second");
        assert!(reopened.is_empty().expect("caught up"));
        assert_eq!(reopened.checkpoint.discarded_total, 0);
    });
    // A synchronous infinite loop cannot be cancelled with a Tokio timeout.
    // Join from another thread so the test fails promptly on the old code.
    let (send, receive) = mpsc::channel();
    thread::spawn(move || send.send(worker.join()).expect("report worker result"));
    receive
        .recv_timeout(Duration::from_secs(5))
        .expect("identity recovery must finish instead of spinning")
        .expect("recovery assertions");
}
