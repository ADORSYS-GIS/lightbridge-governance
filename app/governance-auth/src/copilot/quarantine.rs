//! Records the collector refused on their own, and how many separate wakes it
//! did so on.
//!
//! ## Why one wake's evidence is not enough to discard a record
//!
//! [`super::export`] isolates a permanently-refused record by halving the
//! batch, and its old rule was "a record its own siblings survived is bad".
//! That assumes an HTTP 400 is a deterministic function of the payload. It is
//! not, once anything sits in front of the collector: a WAF, a proxy, an
//! upstream hiccup, a rate limiter answering the wrong status. Measured
//! against a gateway returning 400 for roughly half of all requests for
//! reasons unrelated to the payload, one round in twelve permanently
//! discarded four **valid** records and exited 0.
//!
//! So a refusal is now evidence, not a verdict. A record is given up on only
//! once it has been refused on its own across
//! [`REFUSALS_BEFORE_DISCARD`] **separate wakes** -- which a deterministically
//! bad payload manages every time and a flaky transport manages only by
//! coincidence. The cost is one extra wake per bad record; the alternative is
//! deleting good telemetry because a proxy sneezed.
//!
//! This table answers only "how many separate wakes refused this record". The
//! *other* half of the discard rule -- has the collector been shown to accept
//! anything at all -- is answered live, in [`super::export::isolate`], because
//! a stale answer to it is the difference between one bad record and an
//! emptied spool.
//!
//! ## Why the key is a digest
//!
//! Not the byte offset: a rotation renumbers every record, and the point of an
//! entry is to survive between wakes. Not the record either -- `AGENTS.md`
//! bans writing a payload anywhere, and this one is prompt-adjacent telemetry.
//! A truncated SHA-256 of the line is stable, is not the content, and is short
//! enough that a full table is a few kilobytes.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Separate wakes that must each refuse a record before it is discarded. Two
/// is the smallest number that satisfies "not on a single wake's evidence";
/// every additional round multiplies the drain latency of a genuinely bad
/// record, which is the common case, so this is deliberately not higher.
pub const REFUSALS_BEFORE_DISCARD: u32 = 2;

/// How long an entry outlives its last refusal. A record refused once and then
/// accepted (the benign-flake case) leaves an entry nothing will ever clear,
/// so entries expire rather than accumulating.
const TTL_SECONDS: u64 = 7 * 24 * 60 * 60;

/// Most entries kept. A drain that is quarantining more than this at once is
/// not looking at bad records, it is looking at a broken collector -- and the
/// checkpoint is not the place to grow an unbounded table either way.
const MAX_ENTRIES: usize = 64;

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Quarantine {
    entries: BTreeMap<String, Entry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    /// Separate wakes that refused this record on its own.
    refusals: u32,
    #[serde(default)]
    last_seen_unix: u64,
}

impl Quarantine {
    /// A stable, content-derived name for a spool line. Truncated to 32 hex
    /// characters: 128 bits, so a collision between two records in one
    /// checkpoint is not a thing that happens.
    pub fn key(text: &str) -> String {
        let digest = Sha256::digest(text.as_bytes());
        hex::encode(digest).chars().take(32).collect()
    }

    /// Records one wake's refusal of `key` and answers whether the record has
    /// now been refused on enough separate wakes to be given up on.
    ///
    /// ⚠️ That is *necessary*, not sufficient. The caller must also have shown
    /// that the collector accepts anything at all -- see
    /// [`super::export::isolate`] -- or a collector misconfigured to refuse
    /// everything would be answered by discarding the spool one record per
    /// wake.
    pub fn refused(&mut self, key: &str, now: u64) -> bool {
        let entry = self.entries.entry(key.to_owned()).or_insert(Entry {
            refusals: 0,
            last_seen_unix: now,
        });
        entry.refusals = entry.refusals.saturating_add(1);
        entry.last_seen_unix = now;
        entry.refusals >= REFUSALS_BEFORE_DISCARD
    }

    /// Drops the entry for a record that has been given up on: it will never be
    /// offered again, so keeping it would only crowd the table.
    pub fn forget(&mut self, key: &str) {
        self.entries.remove(key);
    }

    /// Drops expired entries, then the oldest ones if the table is still over
    /// [`MAX_ENTRIES`]. Called once per wake, before anything is offered.
    pub fn prune(&mut self, now: u64) {
        self.entries
            .retain(|_, entry| now.saturating_sub(entry.last_seen_unix) <= TTL_SECONDS);
        while self.entries.len() > MAX_ENTRIES {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_seen_unix)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            self.entries.remove(&oldest);
        }
    }
}
