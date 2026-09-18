//! Orchestrates a single day's ingestion: authenticate -> fetch each report ->
//! archive raw to S3 -> parse -> upsert -> record an ingest manifest.
//!
//! The S3 archive is injected (`archive`) so this module is unit-testable
//! without object storage and step 4 (the real S3 writer) plugs in behind the
//! same signature. The raw bytes are archived BEFORE parsing, per RFC-0001: a
//! parsing bug is replayed from the archive, never refetched.
//!
//! Split into `day` (the per-day report loop) and `seats` (the once-per-run
//! seat snapshot) submodules; this file holds the shared types and helpers.

mod day;
mod seats;

pub use day::sync_day;
pub use seats::sync_seats;

pub use crate::replay::replay_report;
use crate::{
    error::{CopilotError, Result},
    model::{OrgDaily, RepoDaily, SeatSnapshot, UserDaily, UserTeam},
    parse::{parse_org_daily, parse_repo_daily, parse_seats, parse_user_daily, parse_user_team},
};

/// The normalized rows a report's raw bytes parse into, tagged by report kind.
///
/// Exposed so the collector CLI can emit the rows as OTLP log records (the
/// ADR-0014 sink) without re-parsing or reaching into the connector's
/// internals. The parse itself is unchanged -- this is the same
/// `parse_*` code path `replay_report` uses, just surfaced.
#[derive(Debug, Clone)]
pub enum ParsedRows {
    Org(Vec<OrgDaily>),
    User(Vec<UserDaily>),
    Repo(Vec<RepoDaily>),
    UserTeam(Vec<UserTeam>),
    Seat(Vec<SeatSnapshot>),
}

impl ParsedRows {
    /// The number of normalized rows, across every report kind. Used by the
    /// collector to report a record count when the direct-Postgres write path
    /// is frozen (ADR-0014 cutover) -- the parse still happens, the upsert
    /// does not, and the count is what the OTLP emit and the count-assertion
    /// harness compare against.
    pub fn len(&self) -> usize {
        match self {
            ParsedRows::Org(v) => v.len(),
            ParsedRows::User(v) => v.len(),
            ParsedRows::Repo(v) => v.len(),
            ParsedRows::UserTeam(v) => v.len(),
            ParsedRows::Seat(v) => v.len(),
        }
    }

    /// Whether the report parsed to zero rows. Mirrors the `Vec::is_empty`
    /// contract so callers can distinguish "no rows" from "not parsed".
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Parse raw report bytes into the normalized rows for `report`, without
/// persisting anything. `day` is `report_day` for the four day-based reports
/// and `snapshot_day` for `billing-seats`.
pub fn parse_report_rows(report: &str, bytes: &[u8], day: &str) -> Result<ParsedRows> {
    match report {
        "organization-1-day" => Ok(ParsedRows::Org(parse_org_daily(bytes, report, day)?)),
        "users-1-day" => Ok(ParsedRows::User(parse_user_daily(bytes, report, day)?)),
        "repos-1-day" => Ok(ParsedRows::Repo(parse_repo_daily(bytes, report, day)?)),
        "user-teams-1-day" => Ok(ParsedRows::UserTeam(parse_user_team(bytes, report, day)?)),
        crate::SEATS_REPORT_TYPE => Ok(ParsedRows::Seat(parse_seats(bytes, report, day)?)),
        other => Err(CopilotError::github(
            "sync",
            0,
            format!("unknown report type {other} in REPORTS"),
        )),
    }
}

/// Key under which a report's raw NDJSON is archived, relative to the sink's
/// own prefix (`copilot-governance/raw/` on S3, `RAW_DIR` locally; RFC-0001).
/// Must stay in lockstep with `Archive::list_day`/`read` — replay depends on
/// it. Printable only; never a URL.
pub fn archive_key(org: &str, report: &str, day: &str) -> String {
    format!("org={org}/day={day}/{report}.ndjson")
}

/// Key under which the archived seat listing lives, relative to the sink's
/// own prefix. Deterministic per (org, day): a same-day re-run overwrites
/// this one file regardless of how many pages the current seat count spans
/// (`FetchedSeats::to_archive_bytes` always produces exactly one JSON
/// document). `.json`, not `.ndjson`, on purpose: this is a single JSON
/// array document, not one row of NDJSON per line like every `archive_key`
/// above.
pub fn seats_archive_key(org: &str, snapshot_day: &str) -> String {
    format!(
        "org={org}/day={snapshot_day}/{}.json",
        crate::SEATS_REPORT_TYPE
    )
}

/// Outcome of ingesting one report for one day.
#[derive(Debug, Clone)]
pub struct ReportOutcome {
    pub report: String,
    pub day: String,
    pub status: String,
    pub record_count: usize,
    pub host: Option<String>,
}
