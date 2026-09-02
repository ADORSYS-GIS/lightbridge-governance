//! Layers 3 and 4 of ADR-0012 Decision 2's five-layer config precedence:
//! CLI flag -> env var -> per-user file -> machine-wide file -> compiled
//! default. This module is the "file" half -- both file layers are read
//! through the same [`ConfigFile`] shape and the same [`load`] function.
//! [`crate::config::OauthConfigArgs::resolve`] is what actually orders the
//! five layers; this module only knows how to find and parse one file.
//!
//! ## The trap this exists to not reintroduce
//!
//! `scopes` and `otel_headers_debounce_ms` used to be filled by clap's
//! `default_value`/`default_value_t`, which fires the instant a flag and its
//! env var are both absent -- before this module is ever consulted. Both
//! became `Option` in `config.rs` specifically so a config file gets a
//! chance to supply them; the compiled defaults now live in
//! `config::resolve_with_paths` instead. See `config.rs`'s
//! `tests::precedence` module -- in particular
//! `machine_file_wins_over_compiled_default_for_scopes` and its
//! `..._for_debounce_ms` counterpart -- for the regression guard: either
//! test fails if a clap default is reintroduced on the field it covers.
//!
//! ## Secrets
//!
//! `otel_token` is the one field this file can carry that's a genuine
//! credential (the long-lived OTLP ingest bearer, ADR-0012 §2 / `otel.rs`'s
//! module doc). Two rules follow, mirroring the posture `otel.rs` already
//! takes with its own `0600` env file:
//!
//! - A file that inlines `otel_token` and is readable by group or other is
//!   REFUSED, not silently loaded -- see [`refuse_if_group_or_other_readable`].
//! - `otel_token_file = "/path"` (the `*_FILE` convention) lets a
//!   machine-wide file -- which, like `/etc/gitconfig`, is reasonably
//!   world-readable -- point at MDM/ESO-managed material instead of inlining
//!   the secret. The file it points to carries the same hazard as an inlined
//!   token, so it gets the same permission check on read.

use std::{fs, path::Path};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::redacted::Redacted;

/// Where the machine-wide layer always lives. ADR-0012 §1 puts this at
/// `/etc/` on macOS too -- a deliberate divergence from the Claude Code
/// managed-settings convention, argued for in the ADR's Decision 1 table.
/// Unlike the per-user layer there is no XDG (or XDG-like) analogue for a
/// systemwide config location on either platform this binary targets, so
/// this is a plain constant rather than a resolver function.
pub const MACHINE_CONFIG_PATH: &str = "/etc/governance-auth/config.toml";

/// `$XDG_CONFIG_HOME/governance-auth/config.toml`, else
/// `~/.config/governance-auth/config.toml` on both Linux and macOS -- the
/// same rule `otel.rs` already uses for its own writes (no macOS branch,
/// deliberately: see that module's doc), so config reads and telemetry
/// writes agree on where "per-user config" lives.
pub fn per_user_config_path() -> Result<std::path::PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME")
        && !xdg.is_empty()
    {
        return Ok(std::path::PathBuf::from(xdg)
            .join("governance-auth")
            .join("config.toml"));
    }

    let home = std::env::var("HOME")
        .context("locating the per-user config file ($XDG_CONFIG_HOME and $HOME both unset)")?;
    Ok(std::path::PathBuf::from(home)
        .join(".config")
        .join("governance-auth")
        .join("config.toml"))
}

/// The recognised keys, snake_case, mirroring the CLI flags/env vars they
/// layer beneath (ADR-0012 §2). Every field is optional: a config file only
/// supplies what it supplies, and `OauthConfigArgs::resolve` is the one
/// place "must actually be present" gets enforced -- exactly the same split
/// `config.rs` already uses between clap's `OauthConfigArgs` and the
/// resolved `OauthConfig`.
///
/// `deny_unknown_fields`: this file is owned entirely by `governance-auth`
/// (unlike Codex's or Claude Code's config, which `otel.rs` merges into
/// rather than replaces), so an unrecognised key is a typo, not a
/// deliberately-preserved neighbour -- and a typo that failed loudly here
/// beats one field silently never taking effect.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    pub issuer: Option<String>,
    pub client_id: Option<String>,
    pub scopes: Option<String>,
    pub audience: Option<String>,
    pub otel_endpoint: Option<String>,
    otel_token: Option<Redacted<String>>,
    otel_token_file: Option<String>,
    pub gateway_url: Option<String>,
    /// `"daemon"` or `"manual"` (ADR-0016). Kept as a raw string here, like
    /// every other field in this file-only layer -- `config::resolve` is
    /// where it's parsed into [`crate::profile::Profile`], the same split
    /// `otel_endpoint`/`parse_issuer` already use.
    pub profile: Option<String>,
    /// See the option matrix in `docs/governance-auth/configuration.md`.
    pub copilot_spool_path: Option<String>,
    pub otel_headers_debounce_ms: Option<u64>,
    /// Off by default (issue #141) -- see the docs above.
    pub open_browser: Option<bool>,
    /// Off by default (issue #140); the four fields below it are part of the
    /// same opt-in block. See the docs above.
    pub token_exchange: Option<bool>,
    pub exchange_issuer: Option<String>,
    pub exchange_token_endpoint: Option<String>,
    pub exchange_client_id: Option<String>,
    pub exchange_scopes: Option<String>,
}

impl ConfigFile {
    /// The `otel_token` this file supplies, whether written inline or via
    /// the `otel_token_file` indirection. `source` is only used to name the
    /// file in an error message -- never to re-read it.
    ///
    /// Bails if both are set: silently preferring one would be a
    /// misconfiguration nobody would ever notice, exactly the kind of
    /// "malformed config, fail loudly" case this module exists to catch
    /// rather than paper over.
    pub fn otel_token(&self, source: &Path) -> Result<Option<Redacted<String>>> {
        match (&self.otel_token, &self.otel_token_file) {
            (Some(_), Some(_)) => bail!(
                "{} sets both `otel_token` and `otel_token_file`; keep only one",
                source.display()
            ),
            (Some(token), None) => Ok(Some(token.clone())),
            (None, Some(path)) => Ok(Some(Redacted::new(read_token_file(Path::new(path))?))),
            (None, None) => Ok(None),
        }
    }
}

/// Loads and parses `path` as a [`ConfigFile`]. A missing file is normal --
/// most machines have no machine-wide config, and a fresh developer has no
/// per-user one yet -- so that returns `Ok(None)`, not an error. A file that
/// exists but doesn't parse, or inlines a secret with the wrong permissions,
/// is an error: never silently fall through to the next, weaker layer just
/// because this one is broken.
pub fn load(path: &Path) -> Result<Option<ConfigFile>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };

    let file: ConfigFile = toml_edit::de::from_str(&text).with_context(|| {
        format!(
            "{} is not valid TOML for governance-auth's config schema",
            path.display()
        )
    })?;

    if file.otel_token.is_some() {
        refuse_if_group_or_other_readable(path)?;
    }

    Ok(Some(file))
}

/// Reads the secret a `otel_token_file = "/path"` entry points at, refusing
/// first if that file itself is readable by group or other -- it carries
/// exactly the same secret as an inlined `otel_token`, so it gets exactly
/// the same check.
///
/// Trailing newline is stripped: the common way to produce one of these
/// files (`echo "$TOKEN" > path`, an ESO `secretKeyRef` volume mount) always
/// leaves one, and a token with a literal trailing `\n` baked into every
/// `Authorization` header would fail at the collector in a way that's
/// miserable to debug.
fn read_token_file(path: &Path) -> Result<String> {
    refuse_if_group_or_other_readable(path)?;
    let contents = fs::read_to_string(path)
        .with_context(|| format!("reading otel_token_file at {}", path.display()))?;
    let token = contents.trim_end_matches(['\n', '\r']).to_owned();
    if token.is_empty() {
        bail!("otel_token_file at {} is empty", path.display());
    }
    Ok(token)
}

/// The SSH-precedent permission check: refuse to load a file that carries a
/// secret if its mode grants group or other any permission at all, and name
/// the exact fix rather than making the operator work it out. Mirrors the
/// posture `otel.rs` already takes when it *writes* `otel.env` at `0600`;
/// this is the read-side equivalent for a file this binary didn't write
/// itself and can't assume the permissions of.
#[cfg(unix)]
fn refuse_if_group_or_other_readable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mode = fs::metadata(path)
        .with_context(|| format!("stat-ing {}", path.display()))?
        .permissions()
        .mode();

    if mode & 0o077 != 0 {
        bail!(
            "{} carries an OTLP ingest token and is readable by group or other (mode {:o}); \
             refusing to load it. Fix with:\n\n  chmod 600 {}\n",
            path.display(),
            mode & 0o777,
            path.display(),
        );
    }
    Ok(())
}

/// Non-Unix targets have no POSIX mode bits to check. This binary only ships
/// for Linux and macOS (ADR-0012 §1), so this arm exists only so the crate
/// still compiles if that ever changes, not because it's expected to run.
#[cfg(not(unix))]
fn refuse_if_group_or_other_readable(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal scratch dir, removed on drop -- same hand-rolled pattern
    /// `otel.rs`'s own tests use, for the same reason (one or two call
    /// sites, not worth a `tempfile` dependency).
    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn tempdir() -> TempDir {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "governance-auth-config-file-{}-{unique}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create temp dir");
        TempDir(path)
    }

    #[cfg(unix)]
    fn chmod(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("chmod test fixture");
    }

    #[test]
    fn a_missing_file_is_none_not_an_error() {
        let dir = tempdir();
        let path = dir.path().join("does-not-exist.toml");
        let result = load(&path).expect("a missing file must not be an error");
        assert!(result.is_none());
    }

    #[test]
    fn a_malformed_file_is_a_loud_error_naming_the_path() {
        let dir = tempdir();
        let path = dir.path().join("config.toml");
        fs::write(&path, "issuer = [this is not valid toml").expect("seed a broken file");
        #[cfg(unix)]
        chmod(&path, 0o600);

        let error = load(&path).expect_err("malformed TOML must not silently fall through");
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains(&path.display().to_string()),
            "error must name the file, got: {rendered}"
        );
    }

    #[test]
    fn an_unknown_key_is_rejected_as_malformed() {
        // This file is owned entirely by governance-auth, so an unrecognised
        // key is a typo, not a neighbour to preserve -- unlike otel.rs's
        // merge-only writes into OTHER tools' configs.
        let dir = tempdir();
        let path = dir.path().join("config.toml");
        fs::write(&path, "issur = \"https://example.com\"\n").expect("seed a typo'd key");
        #[cfg(unix)]
        chmod(&path, 0o600);

        assert!(
            load(&path).is_err(),
            "an unrecognised key must be an error, not ignored"
        );
    }

    #[test]
    fn ordinary_keys_parse() {
        let dir = tempdir();
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            "issuer = \"https://issuer.example/realms/platform\"\n\
             client_id = \"cli\"\n\
             scopes = \"openid custom\"\n\
             audience = \"aud\"\n\
             otel_endpoint = \"https://otel.example\"\n\
             gateway_url = \"https://gw.example\"\n\
             profile = \"manual\"\n\
             otel_headers_debounce_ms = 60000\n\
             open_browser = true\n\
             token_exchange = true\n\
             exchange_issuer = \"https://exchange.example\"\n\
             exchange_token_endpoint = \"https://exchange.example/oauth2/token\"\n\
             exchange_client_id = \"exchange-cli\"\n\
             exchange_scopes = \"openid profile\"\n",
        )
        .expect("seed a full config file");
        #[cfg(unix)]
        chmod(&path, 0o600);

        let file = load(&path)
            .expect("well-formed file must load")
            .expect("file exists");
        assert_eq!(
            file.issuer.as_deref(),
            Some("https://issuer.example/realms/platform")
        );
        assert_eq!(file.client_id.as_deref(), Some("cli"));
        assert_eq!(file.scopes.as_deref(), Some("openid custom"));
        assert_eq!(file.audience.as_deref(), Some("aud"));
        assert_eq!(file.otel_endpoint.as_deref(), Some("https://otel.example"));
        assert_eq!(file.gateway_url.as_deref(), Some("https://gw.example"));
        assert_eq!(file.profile.as_deref(), Some("manual"));
        assert_eq!(file.otel_headers_debounce_ms, Some(60_000));
        assert_eq!(file.open_browser, Some(true));
        assert_eq!(file.token_exchange, Some(true));
        assert_eq!(
            file.exchange_issuer.as_deref(),
            Some("https://exchange.example")
        );
        assert_eq!(
            file.exchange_token_endpoint.as_deref(),
            Some("https://exchange.example/oauth2/token")
        );
        assert_eq!(file.exchange_client_id.as_deref(), Some("exchange-cli"));
        assert_eq!(file.exchange_scopes.as_deref(), Some("openid profile"));
    }

    #[cfg(unix)]
    #[test]
    fn a_group_readable_file_carrying_otel_token_is_refused() {
        let dir = tempdir();
        let path = dir.path().join("config.toml");
        fs::write(&path, "otel_token = \"super-secret\"\n").expect("seed a file with a token");
        chmod(&path, 0o640); // group-readable -- the exact case to refuse

        let error = load(&path).expect_err("a group-readable token file must be refused");
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains(&format!("chmod 600 {}", path.display())),
            "error must print the exact fix command, got: {rendered}"
        );
        assert!(
            !rendered.contains("super-secret"),
            "the token value must never appear in an error message, got: {rendered}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_0600_file_carrying_otel_token_loads_fine() {
        let dir = tempdir();
        let path = dir.path().join("config.toml");
        fs::write(&path, "otel_token = \"super-secret\"\n").expect("seed a file with a token");
        chmod(&path, 0o600);

        let file = load(&path)
            .expect("a 0600 file must load")
            .expect("file exists");
        assert_eq!(
            file.otel_token(&path)
                .expect("resolve otel_token")
                .map(|token| token.expose().clone()),
            Some("super-secret".to_owned())
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_world_readable_file_without_a_token_loads_fine() {
        // The permission check is scoped to files that actually inline a
        // secret. A machine-wide file with no `otel_token` is meant to be
        // as ordinary as `/etc/gitconfig` -- refusing it would make the
        // ADR's own worked example (`otel_token_file` on a world-readable
        // machine config) impossible.
        let dir = tempdir();
        let path = dir.path().join("config.toml");
        fs::write(&path, "issuer = \"https://issuer.example\"\n").expect("seed a plain file");
        chmod(&path, 0o644);

        assert!(load(&path).expect("must load").is_some());
    }

    #[test]
    fn setting_both_otel_token_and_otel_token_file_is_rejected() {
        let dir = tempdir();
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            "otel_token = \"inline\"\notel_token_file = \"/does/not/matter\"\n",
        )
        .expect("seed an ambiguous file");
        #[cfg(unix)]
        chmod(&path, 0o600);

        let file = load(&path)
            .expect("file itself is valid TOML")
            .expect("file exists");
        let error = file
            .otel_token(&path)
            .expect_err("both set at once must be rejected, not silently resolved");
        assert!(format!("{error:#}").contains("both"));
    }

    #[cfg(unix)]
    #[test]
    fn otel_token_file_is_read_and_trailing_newline_is_stripped() {
        let dir = tempdir();
        let token_path = dir.path().join("otel-token");
        fs::write(&token_path, "token-from-file\n").expect("seed token file");
        chmod(&token_path, 0o600);

        let config_path = dir.path().join("config.toml");
        fs::write(
            &config_path,
            format!("otel_token_file = \"{}\"\n", token_path.display()),
        )
        .expect("seed config referencing the token file");
        chmod(&config_path, 0o644); // the config itself carries no secret

        let file = load(&config_path)
            .expect("must load: no inline otel_token, so no permission check on this file")
            .expect("file exists");
        let token = file
            .otel_token(&config_path)
            .expect("resolve otel_token_file")
            .expect("a token was configured");
        assert_eq!(token.expose(), "token-from-file");
    }

    #[cfg(unix)]
    #[test]
    fn a_group_readable_otel_token_file_target_is_refused() {
        let dir = tempdir();
        let token_path = dir.path().join("otel-token");
        fs::write(&token_path, "token-from-file\n").expect("seed token file");
        chmod(&token_path, 0o644); // world-readable -- must be refused

        let config_path = dir.path().join("config.toml");
        fs::write(
            &config_path,
            format!("otel_token_file = \"{}\"\n", token_path.display()),
        )
        .expect("seed config referencing the token file");
        chmod(&config_path, 0o644);

        let file = load(&config_path).expect("must load").expect("file exists");
        let error = file
            .otel_token(&config_path)
            .expect_err("a world-readable token-file target must be refused");
        assert!(format!("{error:#}").contains(&format!("chmod 600 {}", token_path.display())));
    }
}
