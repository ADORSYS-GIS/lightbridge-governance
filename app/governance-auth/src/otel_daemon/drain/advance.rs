//! One mint -> stamp -> forward attempt against whatever the durable spool
//! has at the front. Split from [`super`] purely for the LoC gate.

use super::{Outcome, quarantine, with_spool};
use crate::{
    copilot::Verdict,
    otel_daemon::{DaemonState, forward, mint, normalize, spool},
};

/// One mint -> stamp -> forward, against whatever the spool has at the front.
pub(super) async fn advance_one(state: &DaemonState) -> Outcome {
    let pending = match with_spool(state, spool::DurableSpool::next).await {
        Ok(Some(pending)) => pending,
        Ok(None) => return Outcome::Empty,
        Err(error) => {
            tracing::error!(error = %error, "could not read the durable spool; stopping this pass");
            return Outcome::Stopped;
        }
    };

    let minted = match mint::mint(&state.http, &state.config).await {
        Ok(minted) => minted,
        Err(error) => {
            tracing::warn!(error = %error, "no session; leaving the retained payload pending");
            return Outcome::Stopped;
        }
    };
    // Parsed once, threaded through `stamp` and `forward::post` -- each used
    // to re-parse the same bytes independently, repeating on every retry.
    let parsed: Option<serde_json::Value> = serde_json::from_slice(&pending.payload).ok();
    let is_json = parsed.is_some();
    // `Some(&pending.key)`: a retry has the stable key ingest can dedupe on.
    let stamped = match normalize::stamp(
        parsed,
        &pending.payload,
        &minted.access_token,
        Some(&pending.key),
    ) {
        Ok(stamped) => stamped,
        Err(error) => {
            tracing::warn!(error = %error, "could not re-stamp a retained payload; leaving it pending");
            return Outcome::Stopped;
        }
    };

    match forward::post(
        &state.http,
        &state.config,
        &minted.bearer,
        pending.signal,
        &stamped,
        is_json,
    )
    .await
    {
        Ok(Verdict::Accepted) => {
            let advanced = with_spool(state, {
                let pending = pending.clone();
                move |spool| spool.advance(&pending)
            })
            .await;
            match advanced {
                Ok(()) => Outcome::Advanced,
                Err(error) => {
                    // Not a loss: the collector already has this record --
                    // see `spool`'s own module doc, "at-least-once". `Stopped`,
                    // not `Advanced`: the offset did not move, so `Advanced`
                    // would spin the caller's loop against whatever is
                    // failing the write instead of waiting for `pump`.
                    tracing::error!(
                        error = %error,
                        "delivered a retained payload but could not durably advance past it -- \
                         it will be re-delivered next attempt, a duplicate export rather than a \
                         loss"
                    );
                    Outcome::Stopped
                }
            }
        }
        Ok(Verdict::Refused(status)) => quarantine::handle(state, pending, status).await,
        Err(error) => {
            tracing::warn!(
                error = %error,
                "collector unreachable while draining the durable spool; leaving it pending"
            );
            Outcome::Stopped
        }
    }
}
