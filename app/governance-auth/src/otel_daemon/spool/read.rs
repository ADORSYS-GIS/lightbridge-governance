//! Tailing the durable spool for the next undelivered record.

use anyhow::{Context, Result};

use super::{DurableSpool, Pending, envelope};
use crate::copilot::{quarantine::Quarantine, spool as tail};

impl DurableSpool {
    /// The next undelivered record, decoding and skipping past anything that
    /// fails to parse (see [`super`]'s module doc's torn-write paragraph).
    /// `Ok(None)` once the spool is caught up.
    pub fn next(&mut self) -> Result<Option<Pending>> {
        let now = super::commit::now_unix()?;
        self.checkpoint.quarantine.prune(now);
        loop {
            let drained = tail::drain(
                &self.spool_path,
                self.checkpoint.offset,
                self.checkpoint.spool.as_ref(),
            )
            .context("tailing the daemon's durable spool")?;
            if let Some(restart) = drained.restarted {
                tracing::warn!(
                    reason = ?restart,
                    "the daemon's durable spool was rotated unexpectedly; restarting the tail \
                     at byte 0"
                );
                self.checkpoint.restart();
            }
            self.pending_identity = drained.identity;

            let Some(first) = drained.lines.first() else {
                return Ok(None);
            };
            // Not `drained.next_offset`, which spans every complete line that
            // read happened to return: this loop hands back one record at a
            // time, so it needs exactly where *that* record ends. A second
            // line's own recorded start is that boundary, computed from raw
            // byte lengths rather than the (possibly trimmed) text -- see
            // `copilot::spool::drain`. Only when there is no second line yet
            // does the read's own `next_offset` (computed the same way) apply.
            let boundary = drained
                .lines
                .get(1)
                .map_or(drained.next_offset, |second| second.offset);
            let key = Quarantine::key(&first.text);

            match envelope::decode(&first.text) {
                Ok((signal, payload)) => {
                    return Ok(Some(Pending {
                        signal,
                        payload,
                        key,
                        boundary,
                    }));
                }
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        "a durable spool record could not be decoded; discarding it"
                    );
                    self.commit_past(boundary, 1)?;
                }
            }
        }
    }
}
