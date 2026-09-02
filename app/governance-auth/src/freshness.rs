//! When a cached access token still counts as usable -- the one place that
//! decides it, for every command.
//!
//! Two different questions hide behind "is this token still good?", and
//! answering the first when the second was asked is what caused the
//! production defect this module exists to prevent:
//!
//! - **I am about to use it myself, now.** Then it only has to survive the
//!   round trip plus the clock skew between here and the authorization
//!   server. That is [`Freshness::Skew`] -- `copilot push`, `status`.
//! - **I am about to hand it to a tool that will CACHE it.** Then it has to
//!   survive that tool's whole cache window as well. That is
//!   [`Freshness::OutlivesCache`] -- `token` and `otel headers`, whose
//!   output Claude Code caches for `CLAUDE_CODE_API_KEY_HELPER_TTL_MS` and
//!   `CLAUDE_CODE_OTEL_HEADERS_HELPER_DEBOUNCE_MS` respectively. Both of
//!   those are written by THIS binary from `otel_headers_debounce_ms`, so
//!   the window is not a guess -- it is a setting we control.
//!
//! Measured in production on 2026-09-02: with the skew rule alone, `otel
//! headers` handed over a token with 31s left, Claude Code cached it for
//! 240s, and the collector logged ~30 `oidc: token is expired` rejections
//! per 15-minute token while the session refresh landed three seconds
//! *after* the expiry it was meant to precede.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

use crate::cache::CachedSession;

/// How far ahead of the real expiry a cached token is treated as unusable.
/// Matches the margin the org's own `opencode-oauth2` plugin uses
/// (`tokenExpirySkewMs`).
///
/// It is a FLOOR, not the whole rule. The rule is: a token must stay valid
/// for the entire window its recipient will cache it, **plus** this skew.
/// For a caller that uses the token immediately (Codex's
/// `refresh_interval_ms`, `copilot push`) that window is zero and the skew
/// is the whole requirement; for `token`/`otel headers` it is
/// `otel_headers_debounce_ms`. See [`Freshness`].
pub const SKEW_SECONDS: u64 = 30;

/// How much life a cached token must have left before this process is
/// willing to hand it out.
#[derive(Debug, Clone, Copy)]
pub enum Freshness {
    /// The token is used immediately by the caller that asked for it.
    Skew,
    /// The token is handed to a tool that will keep using it for
    /// `window_secs` before asking again.
    OutlivesCache { window_secs: u64 },
}

impl Freshness {
    /// The window for the two credential-helper commands, from the debounce
    /// this binary itself writes into Claude Code's settings. Taking
    /// milliseconds (rather than seconds) is deliberate: it is the unit
    /// every layer of config carries, so the one conversion lives here
    /// instead of at each call site.
    pub fn for_helper(debounce_ms: u64) -> Self {
        Self::OutlivesCache {
            window_secs: debounce_ms / 1_000,
        }
    }

    /// The minimum remaining lifetime `session` must have to be handed out
    /// under this policy.
    ///
    /// Warns on **stderr** (never stdout -- stdout is the credential) and
    /// caps when the demand cannot be met by construction: a cache window at
    /// least as long as the token lifetime would make every freshly minted
    /// token "stale" the instant it arrives, so every invocation would
    /// refresh, burning one single-use refresh token per call and inviting
    /// the authorization server's reuse-detection cascade. Capping at half
    /// the last observed lifetime keeps the helper working -- degraded, and
    /// saying so -- rather than turning a misconfiguration into an outage.
    ///
    /// The cap never drops below [`SKEW_SECONDS`]: handing out a token with
    /// less margin than that is never right, and an authorization server
    /// minting sub-60s access tokens is broken in a way this helper cannot
    /// paper over.
    pub fn required_remaining_secs(self, session: &CachedSession) -> u64 {
        let Self::OutlivesCache { window_secs } = self else {
            return SKEW_SECONDS;
        };
        let required = window_secs.saturating_add(SKEW_SECONDS);

        // No recorded lifetime: a session written by an older build. The cap
        // is simply unavailable for this one call; the refresh it may cause
        // records a lifetime, so the next call has one.
        let Some(lifetime) = session.lifetime_secs else {
            return required;
        };
        let cap = (lifetime / 2).max(SKEW_SECONDS);
        if required <= cap {
            return required;
        }

        eprintln!(
            "warning: the caller caches this token for {window_secs}s, which with the \
             {SKEW_SECONDS}s clock skew is more than half the {lifetime}s lifetime this \
             authorization server issues. Requiring only {cap}s of remaining life instead, so \
             the helper does not refresh on every single invocation -- tokens may still expire \
             inside the caller's cache. Lower --otel-headers-debounce-ms (currently \
             {}ms) below {}ms, or raise the access-token lifetime.",
            window_secs.saturating_mul(1_000),
            cap.saturating_sub(SKEW_SECONDS).saturating_mul(1_000),
        );
        cap
    }
}

impl CachedSession {
    /// Usable by a caller that will use the token right away.
    pub fn is_fresh(&self) -> Result<bool> {
        self.is_fresh_for(SKEW_SECONDS)
    }

    /// Usable by a caller that needs the token to stay valid for at least
    /// `min_remaining_secs` more.
    pub fn is_fresh_for(&self, min_remaining_secs: u64) -> Result<bool> {
        Ok(self.expires_at > now_unix()?.saturating_add(min_remaining_secs))
    }

    pub fn seconds_until_expiry(&self) -> Result<i64> {
        Ok(i64::try_from(self.expires_at)
            .unwrap_or(i64::MAX)
            .saturating_sub(i64::try_from(now_unix()?).unwrap_or(i64::MAX)))
    }
}

fn now_unix() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("reading system clock")?
        .as_secs())
}

#[cfg(test)]
mod tests;
