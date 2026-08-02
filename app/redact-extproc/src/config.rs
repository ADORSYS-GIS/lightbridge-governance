//! Startup configuration.
//!
//! Mirrors `redact-gateway`'s `config.rs` deliberately — same validation
//! stance (reject an unknown profile rather than default to a weaker one,
//! never let the salt reach `Debug`), because both binaries wrap the same
//! `governance-redact` engine and neither gets to be looser about it.

use anyhow::{Result, bail};
use clap::Parser;
use governance_redact::{DEFAULT_WINDOW, Profile};

/// Command-line and environment surface.
#[derive(Debug, Parser)]
#[command(name = "redact-extproc", version, about)]
pub struct Args {
    /// Address the `ext_proc` gRPC server binds to.
    ///
    /// Localhost-only in production: this runs as a sidecar in the gateway
    /// pod (ADR-0116), reached over loopback by the Envoy `ext_proc` filter,
    /// not over the pod network.
    #[arg(long, env = "LISTEN_ADDR", default_value = "127.0.0.1:9500")]
    pub listen_addr: String,

    /// Address the Prometheus `/metrics` endpoint binds to.
    #[arg(long, env = "METRICS_LISTEN_ADDR", default_value = "0.0.0.0:9501")]
    pub metrics_listen_addr: String,

    /// Redaction profile name.
    #[arg(long, env = "REDACT_PROFILE", default_value = "coding-assistant")]
    pub redact_profile: String,

    /// Salt for the hash action. Must be stable and secret; see
    /// `governance_redact::Engine::new`.
    #[arg(long, env = "REDACT_HASH_SALT")]
    pub redact_hash_salt: String,

    /// Bytes of a streamed response held back before release.
    ///
    /// Must exceed the longest entity that must be caught whole, or that
    /// entity can straddle the release boundary forever. See
    /// `governance_redact::holdback` for the full rationale.
    #[arg(long, env = "RESPONSE_HOLD_BACK_BYTES", default_value_t = DEFAULT_WINDOW)]
    pub response_hold_back_bytes: usize,
}

/// Validated configuration.
///
/// ⚠️ [`Debug`] is implemented manually, matching `redact-gateway::Config` —
/// see that type for why a derive here would be a live leak, not a
/// hypothetical one.
pub struct Config {
    pub listen_addr: std::net::SocketAddr,
    pub metrics_listen_addr: std::net::SocketAddr,
    pub redact_profile: String,
    pub redact_hash_salt: String,
    pub response_hold_back_bytes: usize,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("listen_addr", &self.listen_addr)
            .field("metrics_listen_addr", &self.metrics_listen_addr)
            .field("redact_profile", &self.redact_profile)
            .field("redact_hash_salt", &"<redacted>")
            .field("response_hold_back_bytes", &self.response_hold_back_bytes)
            .finish()
    }
}

impl Args {
    /// Delegates to [`clap::Parser::parse`], then validates.
    ///
    /// # Panics
    ///
    /// Exits the process on invalid CLI/env input, matching every other
    /// binary in this workspace (`clap`'s own behaviour) and on a rejected
    /// [`Config::validate`] result.
    #[must_use]
    pub fn parse() -> Config {
        let args = <Self as Parser>::parse();
        Config::validate(args).unwrap_or_else(|e| {
            eprintln!("redact-extproc: {e:#}");
            std::process::exit(2);
        })
    }
}

impl Config {
    /// Validates arguments into a usable configuration.
    ///
    /// # Errors
    ///
    /// Returns an error for an unparsable listen address, an empty profile
    /// name (the profile itself is resolved by the caller — see
    /// [`Profile::by_name`] — so an unknown name is rejected there, not
    /// here), an empty salt, or a hold-back window of zero.
    pub fn validate(args: Args) -> Result<Self> {
        if args.redact_hash_salt.trim().is_empty() {
            bail!("hash salt must not be empty");
        }
        if args.response_hold_back_bytes == 0 {
            bail!(
                "response hold-back window must be nonzero, or a streamed entity can never be caught whole"
            );
        }

        let listen_addr = args
            .listen_addr
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid LISTEN_ADDR {:?}: {e}", args.listen_addr))?;
        let metrics_listen_addr = args.metrics_listen_addr.parse().map_err(|e| {
            anyhow::anyhow!(
                "invalid METRICS_LISTEN_ADDR {:?}: {e}",
                args.metrics_listen_addr
            )
        })?;

        Ok(Self {
            listen_addr,
            metrics_listen_addr,
            redact_profile: args.redact_profile,
            redact_hash_salt: args.redact_hash_salt,
            response_hold_back_bytes: args.response_hold_back_bytes,
        })
    }
}

/// Resolves the configured profile name, rejecting an unknown one outright.
///
/// A silently-weaker fallback is exactly the failure this service exists to
/// prevent — matches `redact-gateway::Config`'s stance.
///
/// # Errors
///
/// Returns an error naming the unrecognised profile.
pub fn resolve_profile(name: &str) -> Result<Profile> {
    Profile::by_name(name).ok_or_else(|| {
        anyhow::anyhow!(
            "unknown REDACT_PROFILE {name:?} (known: coding-assistant, secrets-only, observe-only)"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::{Args, Config};

    fn args() -> Args {
        Args {
            listen_addr: "127.0.0.1:9500".into(),
            metrics_listen_addr: "0.0.0.0:9501".into(),
            redact_profile: "coding-assistant".into(),
            redact_hash_salt: "s".into(),
            response_hold_back_bytes: 1024,
        }
    }

    #[test]
    fn empty_salt_is_rejected() {
        let mut a = args();
        a.redact_hash_salt = "   ".into();
        let err = Config::validate(a).unwrap_err();
        assert!(err.to_string().contains("salt must not be empty"));
    }

    #[test]
    fn zero_window_is_rejected() {
        let mut a = args();
        a.response_hold_back_bytes = 0;
        let err = Config::validate(a).unwrap_err();
        assert!(err.to_string().contains("nonzero"));
    }

    #[test]
    fn invalid_listen_addr_is_rejected() {
        let mut a = args();
        a.listen_addr = "not-an-addr".into();
        assert!(Config::validate(a).is_err());
    }

    #[test]
    fn debug_never_prints_the_salt() {
        let mut a = args();
        a.redact_hash_salt = "super-secret-salt".into();
        let c = Config::validate(a).expect("config");
        let rendered = format!("{c:?}");
        assert!(
            !rendered.contains("super-secret-salt"),
            "salt leaked into Debug output: {rendered}"
        );
        assert!(rendered.contains("<redacted>"), "{rendered}");
    }

    #[test]
    fn unknown_profile_is_rejected() {
        let err = super::resolve_profile("nope").unwrap_err();
        assert!(err.to_string().contains("unknown REDACT_PROFILE"));
    }
}
