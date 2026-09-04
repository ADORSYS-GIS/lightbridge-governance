//! Mints a fresh bearer through the same `oauth` path `token` uses (A3).
//!
//! Reuses [`crate::oauth::current_session`] + [`crate::oauth::emit_token`] —
//! the refresh-or-fail contract — so there is **no second credential path** in
//! this binary and no way for the daemon's minting to drift from `token`'s.
//! The daemon is effectively a longer-lived `token`-loop: it re-mints per
//! forward so a 300 s access token does not die mid-session.

use anyhow::{Context, Result};

use crate::{config::OauthConfig, freshness::Freshness, oauth, redacted::Redacted};

/// A minted credential: the bearer for the outbound `Authorization` header,
/// plus the raw access token so identity attributes can be stamped
/// ([`super::normalize`]) without re-reading the session.
pub struct MintedToken {
    pub bearer: Redacted<String>,
    pub access_token: Redacted<String>,
}

/// Resolves a fresh session and emits the bearer for outbound auth.
///
/// Fails closed by construction: an absent or unrefreshable session returns
/// `Err`, and nothing is forwarded unauthenticated (A4). The caller retains
/// the payload on `Err`.
///
/// Uses [`Freshness::Skew`] (not `OutlivesCache`) because the daemon uses the
/// bearer itself right now — it never hands it to a tool that will cache it.
pub async fn mint(http: &reqwest::Client, config: &OauthConfig) -> Result<MintedToken> {
    let session = oauth::current_session(http, config, Freshness::Skew).await.context(
        "refusing to forward without a valid session — nothing was forwarded and the payload was \
         retained",
    )?;
    let access_token = session.access_token.clone();
    let bearer = oauth::emit_token(http, config, session).await.context(
        "refusing to forward without a valid bearer — nothing was forwarded and the payload was \
         retained",
    )?;
    Ok(MintedToken {
        bearer,
        access_token,
    })
}
