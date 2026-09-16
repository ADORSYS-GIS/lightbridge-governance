//! Metadata collection is independent of native OTLP receipt and respects opt-out.
use std::time::Duration;

use super::{DaemonState, drain, receive::WireFormat, signal::Signal};
use crate::{codex_measurements::scan, freshness::Freshness, oauth};

pub(super) async fn ticker(state: DaemonState) {
    if state.config.last_no_codex {
        return;
    }
    let Some(root) = scan::root() else {
        return;
    };
    let mut seen = scan::Seen::new();
    let mut timer = tokio::time::interval(Duration::from_secs(60));
    loop {
        timer.tick().await;
        // Do not read session files before establishing the local principal.
        if oauth::current_session(&state.http, &state.config, Freshness::Skew)
            .await
            .is_err()
        {
            tracing::warn!("Codex metadata export is waiting for a valid governance session");
            continue;
        }
        let task_root = root.clone();
        let task_seen = seen.clone();
        let batches = match tokio::task::spawn_blocking(move || {
            scan::collect(&task_root, &task_seen)
        })
        .await
        {
            Ok(Ok(batches)) => batches,
            _ => {
                tracing::warn!("Codex metadata scan failed; no checkpoints advanced");
                continue;
            }
        };
        for batch in batches {
            let mut admitted = true;
            for payload in batch.payloads {
                if !drain::retain(&state, Signal::Logs, payload, WireFormat::Json).await {
                    admitted = false;
                    break;
                }
            }
            if admitted {
                seen.insert(batch.path, batch.signature);
            }
        }
        // A restart or bounded-cache reset may replay source facts. Dashboard
        // queries deduplicate by principal/session/turn before summing durations.
        if seen.len() > 4096 {
            seen.clear();
        }
    }
}
