//! Pure helpers for the OTLP sink: per-report encoding dispatch, org-level
//! cost aggregation, and the natural-key collision guard. Kept separate from
//! `Sink` so they are unit-testable without a provider or network.

use std::collections::HashMap;

use anyhow::{Result, anyhow};
use governance_copilot::ParsedRows;
use governance_core::MicroUsd;

use super::encode::{
    AttributeValue, LogRecordData, encode_org_daily, encode_repo_daily, encode_seat,
    encode_user_daily, encode_user_team,
};

/// Encode one report kind into its `(report name, records)`. The org encoder
/// receives the aggregated org-level cost/credits.
pub(super) fn encode_report(
    tenant_id: &str,
    org: &str,
    parsed: &ParsedRows,
    org_credits: u64,
    org_cost: MicroUsd,
) -> Result<(String, Vec<LogRecordData>)> {
    match parsed {
        ParsedRows::Org(rs) => Ok((
            "organization-1-day".to_owned(),
            rs.iter()
                .map(|r| encode_org_daily(tenant_id, org, r, org_credits, org_cost))
                .collect::<Result<Vec<_>>>()?,
        )),
        ParsedRows::User(rs) => Ok((
            "users-1-day".to_owned(),
            rs.iter()
                .map(|r| encode_user_daily(tenant_id, org, r))
                .collect::<Result<Vec<_>>>()?,
        )),
        ParsedRows::Repo(rs) => Ok((
            "repos-1-day".to_owned(),
            rs.iter()
                .map(|r| encode_repo_daily(tenant_id, org, r))
                .collect::<Result<Vec<_>>>()?,
        )),
        ParsedRows::UserTeam(rs) => Ok((
            "user-teams-1-day".to_owned(),
            rs.iter()
                .map(|r| encode_user_team(tenant_id, org, r))
                .collect(),
        )),
        ParsedRows::Seat(rs) => Ok((
            "billing-seats".to_owned(),
            rs.iter().map(|r| encode_seat(tenant_id, org, r)).collect(),
        )),
    }
}

/// Aggregate the org-level AI credits and cost from a day's user rows. GitHub's
/// org report carries no cost (credits/cost are user-level only), so the org
/// record's spend is the sum of its users' spend. Uses saturating arithmetic so
/// an overflow cannot wrap a monetary value (ADR-0008).
pub(super) fn aggregate_org_cost(rows: &[ParsedRows]) -> (u64, MicroUsd) {
    let mut credits = 0u64;
    let mut cost = 0i64;
    for parsed in rows {
        if let ParsedRows::User(rs) = parsed {
            for r in rs {
                credits = credits.saturating_add(r.ai_credits);
                cost = cost.saturating_add(r.net_cost_micro_usd.0);
            }
        }
    }
    (credits, MicroUsd(cost))
}

/// Read a string attribute value from a record's attributes.
fn attr_str<'a>(attrs: &'a [(String, AttributeValue)], key: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| match v {
            AttributeValue::Str(s) => Some(s.as_str()),
            _ => None,
        })
}

/// Detect natural-key collisions within one emit batch (RFC-0001 known-issues
/// #1 and #5).
///
/// The usage-side upsert is keyed on `(source, day, subject_kind, subject_id)`
/// for day facts, and on `(source, snapshot_day, subject_kind, subject_id,
/// provider_user_id)` for seat snapshots (lightbridge-authz#751). A collision
/// means two records in this batch would merge into one row on the pinned key
/// -- one record silently lost.
///
/// `user-teams-1-day` is skipped entirely before this runs (the receiver
/// refuses it), so the live case this guard protects is a duplicate seat
/// holder (`provider_user_id`) within one snapshot -- a genuine bug that would
/// otherwise collapse two seats into one row.
///
/// Under the cutover freeze (`strict`) a collision fails the emit loudly; in
/// shadow mode it is a warning, because Postgres remains authoritative and
/// failing the whole run would be a regression.
pub(super) fn detect_collisions(records: &[LogRecordData], strict: bool) -> Result<()> {
    let mut seen: HashMap<(String, String, String), usize> = HashMap::new();
    for rec in records {
        let report = attr_str(&rec.attributes, "report").unwrap_or("").to_owned();
        let day = attr_str(&rec.attributes, "day").unwrap_or("").to_owned();
        // Seats key on the seat holder (the table's PK), not the org subject.
        let subject = if report == "billing-seats" {
            attr_str(&rec.attributes, "provider_user_id").unwrap_or("")
        } else {
            attr_str(&rec.attributes, "subject_id").unwrap_or("")
        };
        *seen.entry((report, day, subject.to_owned())).or_insert(0) += 1;
    }

    let dupes: Vec<String> = seen
        .iter()
        .filter(|(_, c)| **c > 1)
        .map(|((report, day, subject), &c)| format!("{report}/{day}/{subject} x{c}"))
        .collect();
    if dupes.is_empty() {
        return Ok(());
    }

    let msg = format!(
        "{} natural-key collision(s) in one emit batch: {}",
        dupes.len(),
        dupes.join(", ")
    );
    if strict {
        Err(anyhow!(
            "{msg}; refusing to emit colliding records under the cutover freeze \
             (RFC-0001 known-issues #1/#5 -- do not cut over until resolved)"
        ))
    } else {
        tracing::warn!("{msg}; emitting anyway in shadow mode (Postgres remains authoritative)");
        Ok(())
    }
}
