//! Stamps identity attributes onto forwarded OTLP payloads (A6).
//!
//! Uses the **same** [`crate::otel::identity_attributes`] that `login` /
//! `configure` / `copilot push` use, so attribution cannot regress or drift
//! between the daemon and the drain. The access token is read (its JWT payload
//! claims) purely to *label* outgoing telemetry.
//!
//! ## The trust boundary this used to assert, and why it was false
//!
//! This module doc used to say the collector "re-derives trusted identity
//! from the authenticated credential, never from these attributes." Checked
//! against the deployed ingest handler
//! (`lightbridge-authz-usage/src/handlers/ingest.rs`), that is false: the row
//! is built by reading `user.id` / `account_id` / `api_key_id` straight out
//! of the payload's resource attributes, with no credential-derived override
//! anywhere in the handler. Combined with [`stamp_resource`]'s old rule
//! ("never overwrite an attribute the client already set"), any poster could
//! pre-set `user.id`/`account_id`/`api_key_id` to someone else's and have it
//! forwarded verbatim under *this* developer's bearer — identity and billing
//! forgery, reachable from any process that can reach the loopback port.
//!
//! So [`stamp_resource`] now removes every key in [`FORGEABLE_IDENTITY_KEYS`]
//! from the client-supplied attributes **unconditionally**, before this
//! module's own (trustworthy, JWT-derived) values are inserted. This runs
//! even when we have no identity of our own to add (an opaque token) -- the
//! strip is not conditional on a replacement existing.
//!
//! ⚠️ **Scoped to JSON.** A protobuf-encoded payload passes through
//! [`stamp`] unchanged (see its doc) and this stripping never runs on it. If
//! the governed collector decodes protobuf into the same attribute model
//! before persisting, the forgery this module closes for JSON remains open
//! for protobuf. Closing that needs a protobuf OTLP decoder, which is out of
//! scope here; flagged rather than silently accepted.
//!
//! ## The idempotency key (#269/#291 review, P1-4)
//!
//! `otel_daemon::drain::advance_one` durably advances the spool's checkpoint
//! *after* the collector has already accepted a retried record -- a kill in
//! that narrow window re-offers the same bytes next attempt, a duplicate
//! export rather than a loss (see `spool`'s own module doc, "at-least-once,
//! not exactly-once"). This daemon cannot itself deduplicate that -- it does
//! not own the ingest table -- but it can carry the key that lets the ingest
//! side do so: [`stamp`]'s `idempotency_key` parameter, when `Some` (only the
//! drain's retry path has one; a live pass-through's first attempt does not,
//! since nothing has a stable key until it has been read back out of the
//! spool at least once), is stamped as [`RETRY_KEY_ATTRIBUTE`] alongside the
//! identity attributes, through the exact same strip-then-set path -- a
//! client-supplied value under that key is stripped unconditionally too, for
//! the same reason a forged identity is: an attacker naming a real record's
//! key would let them collide a dedup key on purpose, griefing a delivery
//! that was never actually a duplicate.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde_json::Value;

use crate::{otel, redacted::Redacted};

/// Attribute keys the governed collector's ingest handler reads directly, with
/// no credential-derived override -- see the module doc. `user.id`/
/// `user.email`/`user.name` are the three this module itself writes (from
/// [`crate::otel::identity_attributes`]); `account_id`/`api_key_id`/`azp` are
/// never written by this binary but are still forgery vectors the same ingest
/// handler trusts, so they are stripped even though nothing here replaces
/// them.
const FORGEABLE_IDENTITY_KEYS: [&str; 6] = [
    "user.id",
    "user.email",
    "user.name",
    "account_id",
    "api_key_id",
    "azp",
];

/// The resource attribute [`stamp`]'s `idempotency_key` parameter is written
/// under. Stripped from client-supplied attributes unconditionally, exactly
/// like [`FORGEABLE_IDENTITY_KEYS`] -- see the module doc's "idempotency
/// key" section for why a forged one is a griefing vector, not a harmless
/// no-op.
const RETRY_KEY_ATTRIBUTE: &str = "governance.retry_key";

/// Stamps identity attributes into an OTLP JSON payload, returning the
/// re-serialized bytes. Client-supplied values for [`FORGEABLE_IDENTITY_KEYS`]
/// are stripped first, unconditionally -- see the module doc.
///
/// `parsed` is the body already parsed once by the caller (`None` when it is
/// not JSON) -- taken rather than re-parsing `body` here, because this used
/// to be the second of three `serde_json::from_slice` calls over the same
/// bytes in one request (`classify`, here, `forward`'s content-type sniff),
/// repeating on every drain retry too (#290 review round 2). `body` is kept
/// alongside it purely as the fallback to return unchanged.
///
/// On no extractable identity (an opaque or non-JWT token), no attributes of
/// ours are added, but a forged one already present is still stripped.
///
/// On a body that is **not JSON** (e.g. OTLP protobuf, `parsed` is `None`),
/// the original bytes are passed through **unchanged rather than an error**,
/// so a real client's default wire format is still forwarded. Identity
/// stamping is a best-effort label, not an admission gate: withholding a
/// valid payload merely because we cannot parse it to add attributes turns an
/// unhandled format into data loss, and it is **not** an authentication
/// refusal — the bearer was already minted before this runs (A4). See the
/// module doc's ⚠️ for what this means for protobuf forgery.
///
/// `idempotency_key`: `Some` only on a drain retry, where
/// [`crate::otel_daemon::spool::Pending::key`] already exists -- see the
/// module doc's "idempotency key" section. Stamped as
/// [`RETRY_KEY_ATTRIBUTE`], through the same strip-then-set path as identity.
pub fn stamp(
    parsed: Option<Value>,
    body: &[u8],
    access_token: &Redacted<String>,
    idempotency_key: Option<&str>,
) -> Result<Vec<u8>> {
    let mut attributes = otel::identity_attributes(access_token.expose());
    if let Some(key) = idempotency_key {
        attributes.insert(RETRY_KEY_ATTRIBUTE.to_owned(), key.to_owned());
    }

    let Some(mut value) = parsed else {
        // Not JSON (e.g. OTLP protobuf): forward the original unchanged, unstamped.
        return Ok(body.to_vec());
    };

    let mut changed = false;
    for key in ["resourceMetrics", "resourceLogs"] {
        if let Some(resources) = value.get_mut(key).and_then(Value::as_array_mut) {
            for resource in resources {
                changed |= stamp_resource(resource, &attributes);
            }
        }
    }

    if !changed {
        // No recognisable resource list in this body, and nothing to strip;
        // forward it unchanged rather than inventing structure.
        return Ok(body.to_vec());
    }

    serde_json::to_vec(&value).context("serializing stamped OTLP payload")
}

/// Strips [`FORGEABLE_IDENTITY_KEYS`] from one `resourceMetrics[]`/
/// `resourceLogs[]` entry's attributes, then inserts this module's own
/// values (if any). Returns whether anything changed.
fn stamp_resource(resource: &mut Value, attributes: &BTreeMap<String, String>) -> bool {
    let Some(resource_obj) = resource.get_mut("resource").and_then(Value::as_object_mut) else {
        return false;
    };
    let Some(attrs) = resource_obj
        .entry("attributes")
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
    else {
        return false;
    };

    let before = attrs.len();
    attrs.retain(|attribute| {
        let key = attribute
            .as_object()
            .and_then(|object| object.get("key"))
            .and_then(Value::as_str);
        !key.is_some_and(|key| FORGEABLE_IDENTITY_KEYS.contains(&key) || key == RETRY_KEY_ATTRIBUTE)
    });
    let mut changed = attrs.len() != before;

    for (key, value) in attributes {
        // Safe to insert unconditionally now: the strip above already
        // removed any client-supplied value under this key, so there is
        // nothing left to collide with.
        attrs.push(Value::Object(serde_json::Map::from_iter([
            ("key".to_owned(), Value::String(key.clone())),
            (
                "value".to_owned(),
                Value::Object(serde_json::Map::from_iter([(
                    "stringValue".to_owned(),
                    Value::String(value.clone()),
                )])),
            ),
        ])));
        changed = true;
    }
    changed
}

#[cfg(test)]
mod tests;
