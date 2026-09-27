//! CLI-configurable OAuth2 client identity. No issuer/client id is baked in:
//! the OIDC issuer and client this binary talks to are registered
//! per-deployment (see the ai-helm coordination note in
//! `docs/adr/0010-governance-auth-keycloak-oauth2-credential-helper.md`).
//! Keycloak is what's deployed today, but nothing here assumes it: `--issuer`
//! is resolved purely through OIDC discovery (`oauth::discovery`), and the
//! optional token-exchange config below (`ExchangeConfig`) makes "authenticate
//! at one issuer, present credentials minted by a second one" a first-class,
//! separately-configured pair rather than an assumption baked into a single
//! `issuer` field.
//!
//! [`OauthConfigArgs::resolve`] implements ADR-0012 Decision 2's five-layer
//! precedence: CLI flag -> env var -> per-user config file -> machine-wide
//! config file -> compiled default. The first two layers are clap's job
//! (every field below carries `env = "GOVERNANCE_AUTH_*"`, and clap prefers
//! an explicit flag over the env var when both are present); the file
//! layers are [`crate::config_file`]'s job; only the final "nothing was
//! configured at all" fallback lives here, in `resolve`.
//!
//! ## clap prints the field docs below, verbatim
//!
//! A `///` on a field of [`OauthConfigArgs`] IS that flag's `--help` text, so
//! it is user documentation and nothing else: one line, no Rust paths, no
//! rationale, and a `long_help` only where a wrong value costs an afternoon.
//! `--help` once opened with `see [`crate::security`]` and three paragraphs on
//! clap's `Option` handling because that rule did not exist. Maintainer notes
//! belong in a plain `//` comment, which neither clap nor rustdoc renders, or
//! in `docs/governance-auth/configuration.md` -- which is also where the two
//! mechanics every field depends on are argued: the `default_value` trap, and
//! why all seventeen are `global = true` (`tests/cli_arg_order.rs` pins it).

use std::{net::IpAddr, path::Path};

use anyhow::{Context, Result, bail};
use clap::Args;
use url::Url;

use crate::{config_file, security};

mod token_exchange;
pub use token_exchange::{ExchangeConfig, ExchangeTokenEndpoint};

/// Compiled fallback for `scopes` -- the lowest of the five layers. Used to
/// live as clap's `default_value`, which is exactly the bug this whole
/// module exists to not have: `default_value` fires the instant flag and
/// env are both absent, before either config file layer is ever consulted.
const DEFAULT_SCOPES: &str = "openid profile offline_access";

/// Compiled fallback for `otel_headers_debounce_ms` -- same trap, same fix.
/// See [`DEFAULT_SCOPES`].
const DEFAULT_OTEL_HEADERS_DEBOUNCE_MS: u64 = 240_000;

/// What `clap` actually parses -- read this module's doc before touching a
/// doc comment below, because clap prints them.
//
// Every field is `Option` and `global = true`. Both are load-bearing and both
// are written up in `docs/governance-auth/configuration.md`. `global` also
// forbids `required` (`Global arguments cannot be required`), which is why
// `resolve` -- not clap -- enforces that issuer and client id are present, with
// a message naming the flag instead of a clap usage dump.
#[derive(Debug, Clone, Args)]
#[command(next_help_heading = "Configuration (accepted before or after the command)")]
pub struct OauthConfigArgs {
    /// OIDC issuer base URL. Required once; `login` saves it.
    #[arg(
        long,
        env = "GOVERNANCE_AUTH_ISSUER",
        value_parser = parse_issuer,
        global = true,
        long_help = "OIDC issuer base URL, e.g. `https://auth.example.com`. Required once; \
                     `login` saves it to your config file so later runs need no flags.\n\n\
                     Pass the issuer itself, with NO realm path: a `/realms/...` suffix 404s at \
                     discovery. Must be `https://` unless it is loopback (`127.0.0.1`, `::1`, \
                     `localhost`); a plaintext typo is rejected here rather than at first \
                     network use."
    )]
    issuer: Option<String>,

    /// Public OAuth2 client id for this binary. Required once; `login` saves
    /// it.
    #[arg(long, env = "GOVERNANCE_AUTH_CLIENT_ID", global = true)]
    client_id: Option<String>,

    /// Space-separated OAuth2 scopes to request. Default:
    /// `openid profile offline_access`.
    #[arg(long, env = "GOVERNANCE_AUTH_SCOPES", global = true)]
    scopes: Option<String>,

    /// Optional `resource`/`audience` parameter, when the authorization
    /// server needs one to scope the token to the gateway.
    #[arg(long, env = "GOVERNANCE_AUTH_AUDIENCE", global = true)]
    audience: Option<String>,

    /// OTLP collector base URL. Its presence is what turns telemetry wiring
    /// on.
    #[arg(
        long,
        env = "GOVERNANCE_AUTH_OTEL_ENDPOINT",
        value_parser = parse_issuer,
        global = true,
        long_help = "OTLP collector base URL, written into Claude Code's and Codex's config by \
                     `configure`. Its presence is what turns telemetry wiring on.\n\n\
                     Pass the BASE URL, not a per-signal path: those tools' own SDKs append \
                     `/v1/metrics`, `/v1/traces` and `/v1/logs` themselves. Same \
                     HTTPS-or-loopback rule as `--issuer` -- telemetry carries prompts and tool \
                     detail, so it must not go out in plaintext by typo."
    )]
    otel_endpoint: Option<String>,

    /// Long-lived OTLP ingest credential, written as an
    /// `Authorization: Bearer` header.
    #[arg(
        long,
        env = "GOVERNANCE_AUTH_OTEL_TOKEN",
        global = true,
        long_help = "Long-lived OTLP ingest credential, written verbatim into both tools' \
                     config as an `Authorization: Bearer` header.\n\n\
                     This is NOT your access token, and passing one here does not work: neither \
                     tool re-reads its config mid-session, so a 300-second token would export \
                     for five minutes and then fail silently. Use a credential minted for \
                     ingest, or leave this unset and let `otel headers` refresh the header on \
                     every call."
    )]
    otel_token: Option<String>,

    /// AI gateway base URL. Its presence is what turns inference wiring on.
    #[arg(long, env = "GOVERNANCE_AUTH_GATEWAY_URL", value_parser = parse_issuer, global = true)]
    gateway_url: Option<String>,

    /// Telemetry wiring profile: `daemon` or `manual` (currently the
    /// default). See ADR-0016.
    #[arg(
        long,
        env = "GOVERNANCE_AUTH_PROFILE",
        value_parser = parse_profile,
        global = true,
        long_help = "Which telemetry wiring `configure` writes.\n\n\
                     `daemon` points every installed client at the loopback collector daemon \
                     and installs it as a service; no client ever holds a long-lived OTLP \
                     credential. `manual` (currently the default) reproduces today's behaviour \
                     exactly: direct exporters, the `copilot-push` timer, and a static \
                     `--otel-token` where a client needs \
                     one -- also the correct choice on a locked-down build agent, in a \
                     container, or anywhere a long-running user service is unwanted. Switching \
                     either way retracts the other profile's keys; a value you edited by hand \
                     is left alone. See ADR-0016."
    )]
    profile: Option<String>,

    /// Where VS Code Copilot Chat writes its OTel spool, for `copilot push`
    /// to drain. Default: `copilot-otel.jsonl` under the state directory.
    #[arg(
        long,
        env = "GOVERNANCE_AUTH_COPILOT_SPOOL_PATH",
        global = true,
        long_help = "Where VS Code Copilot Chat's file exporter writes its OTel records, which \
                     `copilot push` drains. Default: `copilot-otel.jsonl` under the state \
                     directory. `configure` writes the resolved value into both VS Code's \
                     settings and the drain's schedule, so the two cannot disagree.\n\n\
                     Not checked for existence: Copilot creates the file on its first export, \
                     so a correct path set before restarting VS Code is not an error."
    )]
    copilot_spool_path: Option<String>,

    /// How often Claude Code re-runs `otel headers`. Default: 240000, which
    /// must stay under the access-token lifetime.
    #[arg(long, env = "GOVERNANCE_AUTH_OTEL_HEADERS_DEBOUNCE_MS", global = true)]
    otel_headers_debounce_ms: Option<u64>,

    /// Let `login`'s loopback flow open the system browser. Off by default;
    /// the authorize URL is printed either way.
    #[arg(
        long,
        env = "GOVERNANCE_AUTH_OPEN_BROWSER",
        num_args = 0..=1,
        default_missing_value = "true",
        global = true
    )]
    open_browser: Option<bool>,

    /// Which port of the registered loopback block `login`'s browser flow
    /// binds. Unset tries the block in order, as before this flag existed.
    #[arg(
        long,
        env = "GOVERNANCE_AUTH_CALLBACK_PORT",
        value_parser = parse_callback_port_flag,
        global = true,
        long_help = "Which port of `oauth::callback_port::CALLBACK_PORTS` `login`'s browser flow \
                     binds. This only SELECTS a port within that registered block -- a value \
                     outside it is refused before any network call, because the authorization \
                     server only has `redirect_uris` for those exact ports. A busy chosen port is \
                     refused too, by name, never silently retried on another one."
    )]
    callback_port: Option<u16>,

    /// The loopback callback listener's bind address. Default: 127.0.0.1.
    #[arg(
        long,
        env = "GOVERNANCE_AUTH_CALLBACK_BIND",
        value_parser = parse_callback_bind_flag,
        global = true,
        long_help = "The loopback callback listener's LISTEN address. Default: 127.0.0.1.\n\n\
                     Opt-in, for a container: bind 0.0.0.0 inside it and publish \
                     `127.0.0.1:<port>:<port>` on the host. The authorize URL's `redirect_uri` \
                     host stays `127.0.0.1` regardless -- that is what the authorization server \
                     has registered, not where this process happens to listen."
    )]
    callback_bind: Option<String>,

    /// Exchange the access token for a downstream one (RFC 8693) before
    /// `token`/`otel headers` print it. Off by default.
    #[arg(
        long,
        env = "GOVERNANCE_AUTH_TOKEN_EXCHANGE",
        num_args = 0..=1,
        default_missing_value = "true",
        global = true,
        help_heading = "Token exchange (RFC 8693 -- off unless --token-exchange)",
        hide_short_help = true
    )]
    token_exchange: Option<bool>,

    /// Issuer of the exchange server, resolved by discovery. Needed unless
    /// `--exchange-token-endpoint` is given.
    #[arg(
        long,
        env = "GOVERNANCE_AUTH_EXCHANGE_ISSUER",
        value_parser = parse_issuer,
        global = true,
        help_heading = "Token exchange (RFC 8693 -- off unless --token-exchange)",
        hide_short_help = true
    )]
    exchange_issuer: Option<String>,

    /// Exchange token endpoint given directly, skipping discovery. Wins over
    /// `--exchange-issuer`.
    #[arg(
        long,
        env = "GOVERNANCE_AUTH_EXCHANGE_TOKEN_ENDPOINT",
        value_parser = parse_exchange_token_endpoint,
        global = true,
        help_heading = "Token exchange (RFC 8693 -- off unless --token-exchange)",
        hide_short_help = true
    )]
    exchange_token_endpoint: Option<String>,

    /// Client id presented on the exchange request. Required once
    /// `--token-exchange` is on, and not the same client as `--client-id`.
    #[arg(
        long,
        env = "GOVERNANCE_AUTH_EXCHANGE_CLIENT_ID",
        global = true,
        help_heading = "Token exchange (RFC 8693 -- off unless --token-exchange)",
        hide_short_help = true
    )]
    exchange_client_id: Option<String>,

    /// Scopes requested on the exchange. Omit to take the exchange server's
    /// own allow-list.
    #[arg(
        long,
        env = "GOVERNANCE_AUTH_EXCHANGE_SCOPES",
        global = true,
        help_heading = "Token exchange (RFC 8693 -- off unless --token-exchange)",
        hide_short_help = true
    )]
    exchange_scopes: Option<String>,
}

impl OauthConfigArgs {
    /// Turns the as-parsed (possibly incomplete) args into the
    /// [`OauthConfig`] every command actually needs, consulting the
    /// per-user and machine-wide config files for anything a flag/env var
    /// didn't supply, or a message naming exactly which flag/env var/config
    /// key is missing -- clap can't enforce presence itself once
    /// `issuer`/`client_id` are `global` (see the struct doc).
    pub fn resolve(self) -> Result<OauthConfig> {
        let per_user_path = config_file::per_user_config_path()?;
        self.resolve_with_paths(&per_user_path, Path::new(config_file::MACHINE_CONFIG_PATH))
    }

    /// [`Self::resolve`], with the two file-layer paths taken as parameters
    /// instead of resolved internally -- so tests can prove each precedence
    /// layer against a temp file, hermetically and in parallel, without
    /// touching a real `$HOME` or (for the machine-wide layer, which has no
    /// per-process override) `/etc` at all.
    fn resolve_with_paths(self, per_user_path: &Path, machine_path: &Path) -> Result<OauthConfig> {
        let per_user = config_file::load(per_user_path)
            .with_context(|| format!("loading per-user config file {}", per_user_path.display()))?;
        let machine = config_file::load(machine_path).with_context(|| {
            format!(
                "loading machine-wide config file {}",
                machine_path.display()
            )
        })?;

        // Both required values are resolved BEFORE either is reported missing.
        // Failing on `--issuer` alone sent a first-time user round the loop
        // twice: fix the issuer, run again, discover `--client-id`. Naming
        // both, with a command they can paste, ends it in one go.
        let issuer_value = self
            .issuer
            .or_else(|| per_user.as_ref().and_then(|file| file.issuer.clone()))
            .or_else(|| machine.as_ref().and_then(|file| file.issuer.clone()));
        let client_id_value = self
            .client_id
            .or_else(|| per_user.as_ref().and_then(|file| file.client_id.clone()))
            .or_else(|| machine.as_ref().and_then(|file| file.client_id.clone()));

        // Destructured together so "both present" is proved by the match, not
        // asserted with `expect` -- which this workspace denies, correctly.
        let (issuer, client_id) = match (issuer_value, client_id_value) {
            (Some(issuer), Some(client_id)) => (issuer, client_id),
            (issuer, client_id) => {
                let mut missing = Vec::new();
                if issuer.is_none() {
                    missing
                        .push("--issuer (or GOVERNANCE_AUTH_ISSUER, or `issuer` in a config file)");
                }
                if client_id.is_none() {
                    missing.push(
                        "--client-id (or GOVERNANCE_AUTH_CLIENT_ID, or `client_id` in a config \
                         file)",
                    );
                }
                bail!(
                    "{} required.\n\nFirst time here? This writes them to your config file so \
                     you only pass them once:\n\n  governance-auth login --device-code \\\n    \
                     --issuer <your-issuer-url> \\\n    --client-id <your-client-id>",
                    missing.join(" and ")
                );
            }
        };

        // ⚠️ RESTORED after a refactor dropped it, caught by
        // `a_config_file_issuer_is_still_validated_for_transport_security`.
        //
        // Re-validated here even though `--issuer` already goes through
        // `parse_issuer` at CLI-parse time: a value sourced from a config file
        // never passes through clap at all, so without this an operator's
        // plaintext-HTTP typo in `/etc/governance-auth/config.toml` would reach
        // the network unchecked -- the exact hole `security`'s module doc says
        // this predicate exists to close everywhere.
        let issuer = parse_issuer(&issuer).map_err(|error| anyhow::anyhow!(error))?;

        let scopes = self
            .scopes
            .or_else(|| per_user.as_ref().and_then(|file| file.scopes.clone()))
            .or_else(|| machine.as_ref().and_then(|file| file.scopes.clone()))
            .unwrap_or_else(|| DEFAULT_SCOPES.to_owned());

        let audience = self
            .audience
            .or_else(|| per_user.as_ref().and_then(|file| file.audience.clone()))
            .or_else(|| machine.as_ref().and_then(|file| file.audience.clone()));

        let otel_endpoint = self
            .otel_endpoint
            .or_else(|| {
                per_user
                    .as_ref()
                    .and_then(|file| file.otel_endpoint.clone())
            })
            .or_else(|| machine.as_ref().and_then(|file| file.otel_endpoint.clone()))
            .map(|value| parse_issuer(&value).map_err(|error| anyhow::anyhow!(error)))
            .transpose()?;

        let per_user_token = per_user
            .as_ref()
            .map(|file| file.otel_token(per_user_path))
            .transpose()?
            .flatten();
        let machine_token = machine
            .as_ref()
            .map(|file| file.otel_token(machine_path))
            .transpose()?
            .flatten();
        // `Redacted` is unwrapped here, at the same CLI/config boundary
        // `OauthConfig` already keeps every other field at -- the CLI/env
        // value arrives as a plain `String` too (clap has no concept of
        // `Redacted`), and every existing call site re-wraps it at the point
        // it's actually used (`oauth::mod::apply_telemetry`). Not printed,
        // not logged, in between.
        let otel_token = self
            .otel_token
            .or_else(|| per_user_token.map(|token| token.expose().clone()))
            .or_else(|| machine_token.map(|token| token.expose().clone()));

        let gateway_url = self
            .gateway_url
            .or_else(|| per_user.as_ref().and_then(|file| file.gateway_url.clone()))
            .or_else(|| machine.as_ref().and_then(|file| file.gateway_url.clone()))
            .map(|value| parse_issuer(&value).map_err(|error| anyhow::anyhow!(error)))
            .transpose()?;

        // Layered like every other field; the compiled default lives on
        // `Profile` itself (ADR-0016), not here, so this arm is the same
        // shape as `scopes`/`otel_headers_debounce_ms` above rather than a
        // one-off. Re-parsed here (not trusted from clap) because a
        // config-file value never passes through `parse_profile` at all --
        // the exact reason `otel_endpoint` re-runs `parse_issuer` below.
        let profile_explicit = self
            .profile
            .or_else(|| per_user.as_ref().and_then(|file| file.profile.clone()))
            .or_else(|| machine.as_ref().and_then(|file| file.profile.clone()))
            .map(|value| value.parse::<crate::profile::Profile>())
            .transpose()?;
        let profile = profile_explicit.unwrap_or_default();

        let copilot_spool_path = self
            .copilot_spool_path
            .or_else(|| {
                per_user
                    .as_ref()
                    .and_then(|file| file.copilot_spool_path.clone())
            })
            .or_else(|| {
                machine
                    .as_ref()
                    .and_then(|file| file.copilot_spool_path.clone())
            });

        let otel_headers_debounce_ms = self
            .otel_headers_debounce_ms
            .or_else(|| {
                per_user
                    .as_ref()
                    .and_then(|file| file.otel_headers_debounce_ms)
            })
            .or_else(|| {
                machine
                    .as_ref()
                    .and_then(|file| file.otel_headers_debounce_ms)
            })
            .unwrap_or(DEFAULT_OTEL_HEADERS_DEBOUNCE_MS);

        let open_browser = self
            .open_browser
            .or_else(|| per_user.as_ref().and_then(|file| file.open_browser))
            .or_else(|| machine.as_ref().and_then(|file| file.open_browser))
            .unwrap_or(false);

        // File layer only, deliberately: there is no flag or env var for any
        // of these four, unlike every field resolved above. See `ConfigFile`
        // and `OauthConfig::last_no_claude`'s own docs for why.
        let last_no_claude = per_user
            .as_ref()
            .and_then(|file| file.no_claude)
            .or_else(|| machine.as_ref().and_then(|file| file.no_claude))
            .unwrap_or(false);
        let last_no_codex = per_user
            .as_ref()
            .and_then(|file| file.no_codex)
            .or_else(|| machine.as_ref().and_then(|file| file.no_codex))
            .unwrap_or(false);
        let last_no_vscode = per_user
            .as_ref()
            .and_then(|file| file.no_vscode)
            .or_else(|| machine.as_ref().and_then(|file| file.no_vscode))
            .unwrap_or(false);
        let last_codex_telemetry_only = per_user
            .as_ref()
            .and_then(|file| file.codex_telemetry_only)
            .or_else(|| machine.as_ref().and_then(|file| file.codex_telemetry_only))
            .unwrap_or(false);

        let token_exchange = token_exchange::resolve(
            self.token_exchange,
            self.exchange_issuer.clone(),
            self.exchange_token_endpoint.clone(),
            self.exchange_client_id.clone(),
            self.exchange_scopes,
            per_user.as_ref(),
            machine.as_ref(),
        )?;

        // Config-file value re-validated (bypasses clap), same as `otel_endpoint`/`profile`.
        let callback_port = self
            .callback_port
            .or_else(|| per_user.as_ref().and_then(|file| file.callback_port))
            .or_else(|| machine.as_ref().and_then(|file| file.callback_port));
        if let Some(port) = callback_port {
            crate::oauth::callback_port::validate(port).map_err(|error| anyhow::anyhow!(error))?;
        }

        // Same reasoning as `callback_port` above.
        let callback_bind = self
            .callback_bind
            .or_else(|| {
                per_user
                    .as_ref()
                    .and_then(|file| file.callback_bind.clone())
            })
            .or_else(|| machine.as_ref().and_then(|file| file.callback_bind.clone()))
            .map(|value| parse_callback_bind(&value).map_err(|error| anyhow::anyhow!(error)))
            .transpose()?
            .unwrap_or(crate::oauth::callback_port::DEFAULT_BIND);

        Ok(OauthConfig {
            issuer,
            client_id,
            scopes,
            audience,
            otel_endpoint,
            otel_token,
            gateway_url,
            profile,
            profile_explicit,
            copilot_spool_path,
            otel_headers_debounce_ms,
            open_browser,
            callback_port,
            callback_bind,
            token_exchange,
            last_no_claude,
            last_no_codex,
            last_no_vscode,
            last_codex_telemetry_only,
        })
    }
}

/// The resolved, always-present OAuth2 client identity every command
/// operates on -- what `OauthConfigArgs::resolve` produces. Kept as a
/// separate (non-`Option`) type so the 13+ call sites across `oauth/*.rs`
/// that read `config.issuer`/`config.client_id` as plain `&str` don't each
/// need to handle absence individually; that's handled once, at the CLI
/// boundary.
#[derive(Debug, Clone)]
pub struct OauthConfig {
    pub issuer: String,
    pub client_id: String,
    pub scopes: String,
    pub audience: Option<String>,
    pub otel_endpoint: Option<String>,
    pub otel_token: Option<String>,
    pub gateway_url: Option<String>,
    /// `daemon` (ADR-0016's compiled default) or `manual`. Always present --
    /// `crate::profile::Profile::default()` is the fifth layer, so callers
    /// never match on absence the way they do for `otel_endpoint`.
    pub profile: crate::profile::Profile,
    /// The same five-layer resolution as [`Self::profile`], but stopped
    /// *before* the fifth layer's `unwrap_or_default()` -- `None` means
    /// nothing (flag, env var, either config file) ever named a profile, as
    /// opposed to `Some` naming one that happens to equal the compiled
    /// default. Exists purely for [`crate::config_persist::remember`] (#280
    /// review): persisting [`Self::profile`] unconditionally would bake
    /// today's compiled default into every developer's config file the
    /// first time they ever ran `login`/`configure`, permanently pinning
    /// them to it even after a future build changes what the default is --
    /// see that function's own doc for the mechanism. No other consumer
    /// should read this field; every behavioural decision belongs on
    /// [`Self::profile`].
    pub profile_explicit: Option<crate::profile::Profile>,
    /// `None` means "use the compiled default under the state directory" --
    /// see `crate::copilot::resolve_spool_path`, which is where the fifth
    /// layer is applied.
    pub copilot_spool_path: Option<String>,
    pub otel_headers_debounce_ms: u64,
    /// Whether `login`'s loopback flow launches the system browser
    /// automatically. Defaults to `false`; the reasoning (issue #141) is in
    /// `docs/governance-auth/configuration.md`.
    pub open_browser: bool,
    /// Which port of [`crate::oauth::CALLBACK_PORTS`] `login`'s loopback flow
    /// binds. `None` means "try the block in order" (`oauth::callback_port::bind`).
    /// Already validated as a block member -- see `resolve_with_paths`.
    pub callback_port: Option<u16>,
    /// The loopback listener's bind address. Defaults to `127.0.0.1`; the
    /// authorize URL's `redirect_uri` host does not follow it, on purpose.
    pub callback_bind: IpAddr,
    /// Present only when token exchange (RFC 8693) is enabled -- `None` is
    /// the ONLY representation of "off", so there is no separate bool that
    /// could drift out of sync with these fields. See `oauth::exchange`.
    pub token_exchange: Option<ExchangeConfig>,
    /// The `ClientOptOut` `configure`/`login` were last run with, file-layer
    /// only (no CLI flag or env var resolves these -- seeing them here would
    /// suggest they behave like every other field above, when they exist
    /// purely as memory for `update::reapply` to read back). See
    /// `ConfigFile`'s own doc on the same four fields.
    pub last_no_claude: bool,
    pub last_no_codex: bool,
    pub last_no_vscode: bool,
    pub last_codex_telemetry_only: bool,
}

/// Shared validation behind [`parse_issuer`] and
/// [`parse_exchange_token_endpoint`]: rejects an unparseable URL or one that
/// fails [`security::require_secure`] before this binary ever tries to use
/// it. The raw string is kept (not the re-serialized `Url`) so downstream
/// trailing-slash handling (`oauth::discovery::discover`) sees exactly what
/// the operator passed.
///
/// `label` exists only to keep the error message accurate for both callers:
/// `--issuer`/`--exchange-issuer` really are issuers, but
/// `--exchange-token-endpoint` is explicitly a full endpoint URL, not one --
/// reusing a single hardcoded "invalid issuer URL: ..." message for both
/// (the previous shape) told an operator who typo'd
/// `--exchange-token-endpoint` that their *issuer* was wrong, which isn't
/// even the flag they set.
fn parse_url(label: &str, raw: &str) -> Result<String, String> {
    let url = Url::parse(raw).map_err(|error| format!("invalid {label} URL: {error}"))?;
    security::require_secure(&url).map_err(|error| error.to_string())?;
    Ok(raw.to_owned())
}

/// `clap` value parser for `--issuer`/`GOVERNANCE_AUTH_ISSUER` and
/// `--exchange-issuer`/`GOVERNANCE_AUTH_EXCHANGE_ISSUER` -- both name an
/// actual OIDC issuer. See [`parse_url`] for what's validated.
fn parse_issuer(raw: &str) -> Result<String, String> {
    parse_url("issuer", raw)
}

/// `clap` value parser for `--exchange-token-endpoint`/
/// `GOVERNANCE_AUTH_EXCHANGE_TOKEN_ENDPOINT`: the same two checks as
/// [`parse_issuer`], but labelled "endpoint" rather than "issuer" -- this
/// flag is explicitly NOT an issuer, it's the resolved token endpoint itself,
/// given directly to skip a discovery round trip.
fn parse_exchange_token_endpoint(raw: &str) -> Result<String, String> {
    parse_url("endpoint", raw)
}

/// `clap` value parser for `--profile`/`GOVERNANCE_AUTH_PROFILE`. Delegates
/// to [`crate::profile::Profile::from_str`] so clap and a config-file value
/// (re-parsed in `resolve_with_paths`, which never sees clap) reject exactly
/// the same set of strings.
fn parse_profile(raw: &str) -> Result<String, String> {
    raw.parse::<crate::profile::Profile>()
        .map(|profile| profile.to_string())
        .map_err(|error| error.to_string())
}

/// `clap` value parser for `--callback-port`/`GOVERNANCE_AUTH_CALLBACK_PORT`.
/// Delegates the block-membership check to
/// [`crate::oauth::callback_port::validate`] (rejecting a port outside
/// [`crate::oauth::CALLBACK_PORTS`] here, before `login` ever performs OIDC discovery) so
/// the CLI/env layer and a config-file value, re-checked in
/// `resolve_with_paths`, share one rule.
fn parse_callback_port_flag(raw: &str) -> Result<u16, String> {
    let port: u16 = raw
        .parse()
        .map_err(|error| format!("invalid --callback-port value {raw:?}: {error}"))?;
    crate::oauth::callback_port::validate(port)?;
    Ok(port)
}

/// `clap` value parser for `--callback-bind`. Kept a `String` here, like
/// `--issuer`/[`parse_issuer`], so a config-file value (which never passes
/// through clap) goes through the same [`IpAddr`] check in
/// `resolve_with_paths` rather than a second, separately-written one.
fn parse_callback_bind_flag(raw: &str) -> Result<String, String> {
    parse_callback_bind(raw)?;
    Ok(raw.to_owned())
}

/// Must be a bare IP address, not a hostname or a `host:port` pair -- the
/// port is chosen separately, by `--callback-port`/[`crate::oauth::CALLBACK_PORTS`].
fn parse_callback_bind(raw: &str) -> Result<IpAddr, String> {
    raw.parse()
        .map_err(|error| format!("invalid --callback-bind value {raw:?}: {error}"))
}

#[cfg(test)]
mod tests;
