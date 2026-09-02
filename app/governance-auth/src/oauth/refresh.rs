//! `refresh`: mint a new access token now, whether or not the cached one is
//! still fresh.
//!
//! [`super::token`] refreshes only inside the expiry skew, which is exactly
//! right for a helper Claude Code and Codex spawn every few minutes and
//! useless when the reason you want a new token is something that changed at
//! the *server* -- a role added, a client's scopes edited, an audience
//! remapped. Before this command the only ways to get a fresh one were to wait
//! out the skew or to `logout` and log in again, and the second throws away a
//! working refresh token to fix a problem it was never part of.
//!
//! ## Why this cannot become a way around fail-closed
//!
//! Four properties, each one a thing this command deliberately does not have:
//!
//! 1. **It mints nothing itself.** The only new session it can produce comes
//!    from [`super::refresh_or_fail`], the same authorization-server round
//!    trip `token` uses inside the skew. There is no second code path to the
//!    cache.
//! 2. **It never logs in.** No cached session, or one with no refresh token,
//!    is a hard error naming `login` -- not a browser launch. An unattended
//!    caller that could be talked into an interactive flow is how a stuck
//!    timer becomes a hijacked desktop.
//! 3. **A failure leaves the cache exactly as it was.** `cache::store` runs
//!    only on the success path, so a server that refuses is a non-zero exit
//!    over an untouched session: the developer is no worse off than before
//!    they asked, and in particular is not logged out by a network blip.
//! 4. **It prints no credential.** stdout stays empty; the token goes to the
//!    cache and `token` is still the only command that emits one. Wiring this
//!    into `apiKeyHelper` would therefore break loudly rather than
//!    half-working, which is the point -- it must not become the credential
//!    path, because a forced round trip per request is a denial of service
//!    against the authorization server.
//!
//! Token exchange (RFC 8693) is untouched here on purpose: the exchange
//! happens when a token is *emitted*, not when the session is stored, so a
//! forced refresh renews the upstream session and the next `token` exchanges
//! it under exactly the rules it always did.

use anyhow::{Result, bail};

use super::refresh_or_fail;
use crate::{
    cache::{self, FileLock},
    config::OauthConfig,
};

pub async fn run(http: &reqwest::Client, config: &OauthConfig) -> Result<()> {
    // Same lock as every other session-touching command: two `refresh` runs,
    // or a `refresh` racing the `token` a helper just spawned, must not both
    // spend the refresh token -- a rotating authorization server invalidates
    // the loser and the developer is logged out by a refresh they asked for.
    let _lock = FileLock::acquire(&config.issuer, &config.client_id)?;

    let Some(session) = cache::load(&config.issuer, &config.client_id)? else {
        bail!("no cached session for this issuer/client; run `governance-auth login` first");
    };

    let refreshed = refresh_or_fail(http, config, &session).await?;
    let expires_in = refreshed.seconds_until_expiry()?;
    // Only here, and only on success. See property 3 above.
    cache::store(&refreshed)?;
    // stderr, like every other diagnostic in this binary. See property 4.
    eprintln!("Refreshed; session cached, expires in {expires_in}s.");
    Ok(())
}
