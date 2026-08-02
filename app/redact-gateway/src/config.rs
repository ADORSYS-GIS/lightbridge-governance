//! Startup configuration.

use anyhow::{Context, Result, bail};
use clap::Parser;
use governance_redact::Profile;

/// Command-line and environment surface.
#[derive(Debug, Parser)]
#[command(name = "redact-gateway", version, about)]
pub struct Args {
    /// Address to bind the HTTP listener to.
    #[arg(long, env = "LISTEN_ADDR", default_value = "0.0.0.0:8080")]
    pub listen_addr: String,

    /// Base URL of the upstream provider every request is forwarded to.
    #[arg(long, env = "PROVIDER_BASE_URL")]
    pub provider_base_url: String,

    /// Redaction profile name.
    #[arg(long, env = "REDACT_PROFILE", default_value = "coding-assistant")]
    pub profile: String,

    /// Salt for the hash action.
    ///
    /// Must be stable for the deployment's lifetime (so a value hashes
    /// consistently) and secret — a digest of an email address is otherwise
    /// trivially brute-forced from a known address list.
    #[arg(long, env = "REDACT_HASH_SALT")]
    pub hash_salt: String,

    /// PEM file holding a CA to trust for the upstream connection.
    ///
    /// ⚠️ Needed because the upstream (`core-gateway-internal`) presents a cert
    /// from the cluster's internal CA. This is an explicit flag rather than
    /// `SSL_CERT_FILE` on purpose: this binary's HTTP client is **rustls**, and
    /// `SSL_CERT_FILE` is an **OpenSSL** convention that rustls does not read.
    /// The predecessor service used OpenSSL and got trust that way; carrying
    /// that assumption over would fail at connect time with a certificate
    /// error, in production, on the first request.
    #[arg(long, env = "PROVIDER_CA_FILE")]
    pub provider_ca_file: Option<String>,

    /// Upstream request timeout, seconds.
    #[arg(long, env = "PROVIDER_TIMEOUT_SECS", default_value_t = 600)]
    pub provider_timeout_secs: u64,

    /// Maximum request body accepted, bytes.
    #[arg(long, env = "MAX_BODY_BYTES", default_value_t = 33_554_432)]
    pub max_body_bytes: usize,
}

/// Validated configuration.
///
/// ⚠️ [`Debug`] is implemented **manually** rather than derived, so
/// `hash_salt` cannot reach a log line. A derive here would print the salt
/// every time anything formatted a `Config` — including `Result::unwrap_err`
/// in a test, or a `tracing` field added later without thinking about it.
pub struct Config {
    pub listen_addr: String,
    pub provider_base_url: String,
    pub profile: Profile,
    pub hash_salt: String,
    pub provider_ca_pem: Option<Vec<u8>>,
    pub provider_timeout_secs: u64,
    pub max_body_bytes: usize,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("listen_addr", &self.listen_addr)
            .field("provider_base_url", &self.provider_base_url)
            .field("profile", &self.profile.name)
            .field("hash_salt", &"<redacted>")
            .field("provider_ca_pem", &self.provider_ca_pem.is_some())
            .field("provider_timeout_secs", &self.provider_timeout_secs)
            .field("max_body_bytes", &self.max_body_bytes)
            .finish()
    }
}

impl Config {
    /// Validates arguments into a usable configuration.
    ///
    /// Rejects rather than defaults on every questionable input — an unknown
    /// profile name silently becoming a weaker profile is exactly the failure
    /// this service exists to prevent.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown profile, a non-HTTP provider URL, an
    /// empty salt, or an unreadable CA file.
    pub fn from_args(args: Args) -> Result<Self> {
        let profile = Profile::by_name(&args.profile).ok_or_else(|| {
            anyhow::anyhow!(
                "unknown redaction profile {:?} (known: coding-assistant, secrets-only, observe-only)",
                args.profile
            )
        })?;

        if !args.provider_base_url.starts_with("http://")
            && !args.provider_base_url.starts_with("https://")
        {
            bail!(
                "provider base URL must start with http:// or https://, got {:?}",
                args.provider_base_url
            );
        }

        if args.hash_salt.trim().is_empty() {
            bail!("hash salt must not be empty");
        }

        let provider_ca_pem = match &args.provider_ca_file {
            Some(path) => Some(
                std::fs::read(path)
                    .with_context(|| format!("reading provider CA file {path:?}"))?,
            ),
            None => None,
        };

        Ok(Self {
            listen_addr: args.listen_addr,
            provider_base_url: args.provider_base_url.trim_end_matches('/').to_string(),
            profile,
            hash_salt: args.hash_salt,
            provider_ca_pem,
            provider_timeout_secs: args.provider_timeout_secs,
            max_body_bytes: args.max_body_bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Args, Config};

    fn args(profile: &str, url: &str, salt: &str) -> Args {
        Args {
            listen_addr: "0.0.0.0:8080".into(),
            provider_base_url: url.into(),
            profile: profile.into(),
            hash_salt: salt.into(),
            provider_ca_file: None,
            provider_timeout_secs: 600,
            max_body_bytes: 1024,
        }
    }

    #[test]
    fn unknown_profile_is_rejected() {
        let err = Config::from_args(args("nope", "https://x", "s")).unwrap_err();
        assert!(err.to_string().contains("unknown redaction profile"));
    }

    #[test]
    fn non_http_provider_url_is_rejected() {
        let err = Config::from_args(args("coding-assistant", "ftp://x", "s")).unwrap_err();
        assert!(err.to_string().contains("must start with http"));
    }

    #[test]
    fn empty_salt_is_rejected() {
        let err = Config::from_args(args("coding-assistant", "https://x", "   ")).unwrap_err();
        assert!(err.to_string().contains("salt must not be empty"));
    }

    #[test]
    fn trailing_slash_is_normalised() {
        let c = Config::from_args(args("coding-assistant", "https://x/", "s")).expect("config");
        assert_eq!(c.provider_base_url, "https://x");
    }

    #[test]
    fn debug_never_prints_the_salt() {
        // A derived Debug would leak the salt into any log line or test
        // failure that formatted a Config.
        let c = Config::from_args(args("coding-assistant", "https://x", "super-secret-salt"))
            .expect("config");
        let rendered = format!("{c:?}");
        assert!(
            !rendered.contains("super-secret-salt"),
            "salt leaked into Debug output: {rendered}"
        );
        assert!(rendered.contains("<redacted>"), "{rendered}");
    }

    #[test]
    fn missing_ca_file_is_fatal_not_ignored() {
        // Silently continuing without the CA would surface as a TLS error on
        // the first real request instead of at startup.
        let mut a = args("coding-assistant", "https://x", "s");
        a.provider_ca_file = Some("/nonexistent/ca.pem".into());
        assert!(Config::from_args(a).is_err());
    }
}
