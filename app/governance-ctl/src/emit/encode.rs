//! Pure, transport-agnostic encoding of normalized rows into [`LogRecordData`]
//! (RFC-0001 contract). Split from emission so it is unit-testable without a
//! network. Each report family lives in its own submodule; this file holds the
//! shared types, the common-attribute helper, and the re-export surface.

mod daily;
mod seats;
mod teams;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_teams_seats;

use anyhow::{Result, anyhow};
pub use daily::{encode_org_daily, encode_repo_daily, encode_user_daily};
pub use seats::encode_seat;
pub use teams::encode_user_team;

/// The trusted-source stamp carried on every record (ADR-0013 invariant 2 /
/// RFC-0001 contract). The usage-side normalizer keys on this.
pub const SOURCE: &str = "github-copilot";

/// Convert a `u64` count to the contract's `i64` "int" attribute type,
/// refusing (rather than silently wrapping) if it ever exceeds `i64::MAX`.
///
/// Counts from GitHub are `u64`; the OTLP contract pins them as `i64`, and a
/// bare `as i64` cast would wrap a value past `i64::MAX` into a negative --
/// emitting a corrupt count into a load-bearing contract. Realistic values are
/// far below the bound, so this only fires on a genuinely impossible input,
/// and when it does it fails loudly instead of corrupting the record.
fn checked_i64(v: u64) -> Result<i64> {
    i64::try_from(v)
        .map_err(|_| anyhow!("count {v} exceeds i64 range; refusing to emit a wrapped value"))
}

/// A pure, transport-agnostic log record: a human-readable body plus typed
/// attributes. Encoding produces these; [`crate::emit::emit`] turns them into
/// OTLP log records. Kept separate so the encoding is testable without any
/// network or OTLP SDK machinery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRecordData {
    pub body: String,
    pub attributes: Vec<(String, AttributeValue)>,
}

/// A typed attribute value. Only the two types the contract pins appear:
/// strings and integers (money and counts are integers; never floats).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttributeValue {
    Str(String),
    Int(i64),
}

/// The seven common attributes present on every record (RFC-0001 contract).
fn common(
    tenant_id: &str,
    org: &str,
    report: &str,
    day: &str,
    subject_kind: &str,
    subject_id: &str,
) -> Vec<(String, AttributeValue)> {
    vec![
        ("source".to_owned(), AttributeValue::Str(SOURCE.to_owned())),
        (
            "tenant_id".to_owned(),
            AttributeValue::Str(tenant_id.to_owned()),
        ),
        ("org".to_owned(), AttributeValue::Str(org.to_owned())),
        ("report".to_owned(), AttributeValue::Str(report.to_owned())),
        ("day".to_owned(), AttributeValue::Str(day.to_owned())),
        (
            "subject_kind".to_owned(),
            AttributeValue::Str(subject_kind.to_owned()),
        ),
        (
            "subject_id".to_owned(),
            AttributeValue::Str(subject_id.to_owned()),
        ),
    ]
}

/// Test helpers shared by the per-report test modules.
#[cfg(test)]
pub(crate) mod test_util {
    use super::*;

    pub fn attr<'a>(attrs: &'a [(String, AttributeValue)], key: &str) -> &'a AttributeValue {
        attrs
            .iter()
            .find(|(k, _)| k == key)
            .map_or_else(|| panic!("attribute {key} missing"), |(_, v)| v)
    }

    pub fn int(attrs: &[(String, AttributeValue)], key: &str) -> i64 {
        match attr(attrs, key) {
            AttributeValue::Int(i) => *i,
            other => panic!("attribute {key} expected Int, got {other:?}"),
        }
    }

    pub fn str_attr<'a>(attrs: &'a [(String, AttributeValue)], key: &str) -> &'a str {
        match attr(attrs, key) {
            AttributeValue::Str(s) => s,
            other => panic!("attribute {key} expected Str, got {other:?}"),
        }
    }
}
