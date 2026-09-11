//! `doctor`: one command, one exit code, for "is this actually working".
//!
//! ## Why this exists
//!
//! `docs/governance-auth/default-flow.md`'s own "Verifying it actually
//! works" section already says the right thing: *"Not 'no errors' -- these
//! are the observable outcomes"* -- and then lists three separate commands
//! (`status`, `token`, `curl`) plus a per-tool manual check, each answering
//! a different question a reader has to already know to ask. That is the
//! correct list of checks; it is just three commands, three mental models,
//! and no single answer at the end. `doctor` runs the same checks in one
//! place and turns them into one process exit code: `0` means every check
//! passed, anything else means read the report above it for which one
//! didn't.
//!
//! ## What it does NOT replace
//!
//! `status`'s own rows still exist and still answer "what does this look
//! like right now" for a human at a terminal -- `doctor` reuses exactly
//! those rows ([`dashboard::survey_rows`]) rather than deriving a second,
//! possibly-disagreeing set of facts, and adds a verdict on top of them.
//! A per-tool check -- does Claude Code / Codex / VS Code actually get a
//! response -- is still a real request through that tool, which nothing
//! server-side can substitute for; see `default-flow.md`'s own per-tool
//! list for what to look at there.
//!
//! ## What counts as a failure
//!
//! A `red` row, or a live check (credential, gateway) that outright failed.
//! `yellow` rows print too, so nothing is hidden, but do not fail the exit
//! code -- the same reasoning `dashboard::style::Colour`'s own doc gives for
//! yellow throughout this binary: a stale-but-refreshable token, an
//! unaskable scheduler, are the normal steady state of *something*, not a
//! problem `doctor` should train a reader to treat as one.

use anyhow::{Result, bail};

use crate::{config::OauthConfig, dashboard, freshness::Freshness, oauth, redacted::Redacted};

/// One line of the report: a label, whether it passed, and what to tell the
/// reader either way.
struct Check {
    label: String,
    ok: bool,
    detail: String,
}

impl Check {
    fn pass(label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            ok: true,
            detail: detail.into(),
        }
    }

    fn fail(label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            ok: false,
            detail: detail.into(),
        }
    }
}

pub async fn run(http: &reqwest::Client, config: &OauthConfig) -> Result<()> {
    let mut checks = Vec::new();

    // Live check 1: can a credential actually be minted right now? Reuses
    // exactly `token`'s own path (`oauth::current_session` +
    // `oauth::emit_token`), so this can never pass while `token` itself
    // would fail, or vice versa. Kept (not just its success/failure) so the
    // gateway check below can reuse the same bearer instead of minting a
    // second one for the same run.
    let minted = mint(http, config).await;
    let bearer = minted.as_ref().ok().cloned();
    checks.push(match &minted {
        Ok(_) => Check::pass("credential", "a fresh access token can be minted"),
        Err(error) => Check::fail("credential", format!("{error:#}")),
    });

    // Live check 2: does the gateway actually answer, authenticated? Only
    // when one is configured at all -- `not configured` is the ordinary
    // state on a telemetry-only install, not a failure (mirrors every other
    // row in this binary that gates on whether a URL was ever set).
    match (&config.gateway_url, bearer) {
        (Some(gateway_url), Some(bearer)) => {
            checks.push(gateway_check(http, gateway_url, &bearer).await);
        }
        (Some(_), None) => checks.push(Check::fail(
            "gateway",
            "skipped: no credential to authenticate the request with (see `credential` above)",
        )),
        (None, _) => checks.push(Check::pass("gateway", "not configured")),
    }

    // Everything `status --json` already knows, reusing exactly its rows --
    // see this module's own doc for why this must never derive a second,
    // possibly-disagreeing set of facts.
    for (label, value, colour, note) in dashboard::survey_rows(config)? {
        let detail = if note.is_empty() {
            value
        } else {
            format!("{value} -- {note}")
        };
        checks.push(Check {
            label,
            ok: colour != "red",
            detail,
        });
    }

    report(&checks)
}

/// [`oauth::current_session`] then [`oauth::emit_token`] -- exactly `token`'s
/// own two-call path, so this can never disagree with what `token` itself
/// would do.
async fn mint(http: &reqwest::Client, config: &OauthConfig) -> Result<Redacted<String>> {
    let session = oauth::current_session(http, config, Freshness::Skew).await?;
    oauth::emit_token(http, config, session).await
}

async fn gateway_check(
    http: &reqwest::Client,
    gateway_url: &str,
    bearer: &Redacted<String>,
) -> Check {
    let url = format!("{}/v1/models/info", gateway_url.trim_end_matches('/'));
    match http.get(&url).bearer_auth(bearer.expose()).send().await {
        Ok(response) => {
            let status = response.status();
            if status.is_success() {
                Check::pass("gateway", format!("{url} -> {status}"))
            } else {
                Check::fail("gateway", format!("{url} -> {status}"))
            }
        }
        Err(error) => Check::fail("gateway", format!("{url}: {error:#}")),
    }
}

fn report(checks: &[Check]) -> Result<()> {
    let failed: usize = checks.iter().filter(|check| !check.ok).count();
    for check in checks {
        eprintln!(
            "  {} {:<12} {}",
            if check.ok { "OK  " } else { "FAIL" },
            check.label,
            check.detail
        );
    }
    if failed == 0 {
        eprintln!("\nAll checks passed.");
        Ok(())
    } else {
        bail!(
            "{failed} of {} check(s) failed -- see the report above",
            checks.len()
        )
    }
}

#[cfg(test)]
mod tests;
