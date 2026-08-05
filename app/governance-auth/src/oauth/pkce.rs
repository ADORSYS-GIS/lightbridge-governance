//! PKCE (RFC 7636) and OAuth `state` generation. Same 256-bit-CSPRNG +
//! base64url-no-pad shape as `governance-core::credential`'s secret
//! generation -- `getrandom::fill` is the workspace's one sanctioned
//! randomness source (see that module's comment for why SHA-256/base64url
//! over anything password-hash-shaped: this is high-entropy machine-
//! generated output, not a human secret).

use anyhow::{Result, anyhow};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};

pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

pub fn generate() -> Result<Pkce> {
    let mut bytes = [0u8; 32];
    // `getrandom::Error` doesn't implement `std::error::Error`, so it can't
    // flow through `anyhow::Context` -- format it explicitly instead, same
    // as `governance-core::credential::generate_secret`.
    getrandom::fill(&mut bytes)
        .map_err(|error| anyhow!("generating PKCE code verifier: {error}"))?;
    let verifier = URL_SAFE_NO_PAD.encode(bytes);

    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    let challenge = URL_SAFE_NO_PAD.encode(hasher.finalize());

    Ok(Pkce {
        verifier,
        challenge,
    })
}

pub fn random_state() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|error| anyhow!("generating OAuth state parameter: {error}"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}
