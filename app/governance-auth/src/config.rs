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

use std::path::Path;

use anyhow::{Context, Result, bail};
use clap::Args;
use url::Url;

use crate::{config_file, security};

/// Compiled fallback for `scopes` -- the lowest of the five layers. Used to
/// live as clap's `default_value`, which is exactly the bug this whole
/// module exists to not have: `default_value` fires the instant flag and
/// env are both absent, before either config file layer is ever consulted.
const DEFAULT_SCOPES: &str = "openid profile offline_access";

/// Compiled fallback for `otel_headers_debounce_ms` -- same trap, same fix.
/// See [`DEFAULT_SCOPES`].
const DEFAULT_OTEL_HEADERS_DEBOUNCE_MS: u64 = 240_000;

/// What `clap` actually parses. Fields are `Option`, not `String`/required,
/// because clap rejects a `global = true` arg that's also `required`
/// (`Command governance-auth: Global arguments cannot be required`) --
/// [`Self::resolve`] is where "must actually be present" gets enforced, with
/// a message naming the flag, not a generic clap usage dump.
///
/// `global = true` on all four fields: without it, clap only accepts them
/// *before* the subcommand name (`governance-auth --issuer ... token`, not
/// `governance-auth token --issuer ...`), because this is flattened onto the
/// top-level `Cli` rather than duplicated per subcommand. That ordering
/// requirement is a footgun specifically for this binary's main use case: a
/// single command-line string embedded in `apiKeyHelper`/`auth.command`,
/// which both vendors' own docs and this repo's runbook show with the
/// subcommand written first (`"governance-auth token"`) -- composing that
/// pattern with explicit `--issuer`/`--client-id` (rather than relying on
/// `GOVERNANCE_AUTH_ISSUER`/`GOVERNANCE_AUTH_CLIENT_ID` env vars, which a
/// helper subprocess isn't guaranteed to inherit) used to fail with `error:
/// unexpected argument '--issuer' found` and no hint that reordering the
/// string would fix it. Verified against a real `apiKeyHelper` invocation,
/// not just a unit test.
#[derive(Debug, Clone, Args)]
pub struct OauthConfigArgs {
    /// Base URL of the issuing OIDC provider, e.g.
    /// `https://auth.ai.camer.digital` -- no realm path: `authz-idp` is the
    /// issuer itself, and a `/realms/...` suffix 404s at discovery.
    /// Must be `https://`, unless it's a loopback address
    /// (`127.0.0.1`/`::1`/`localhost`) -- see [`crate::security`]. Validated
    /// here, at parse time, rather than left to fail at first network use:
    /// this is a credential helper, and an operator's typo shouldn't be
    /// discovered only when a token request silently goes out in plaintext.
    #[arg(long, env = "GOVERNANCE_AUTH_ISSUER", value_parser = parse_issuer, global = true)]
    issuer: Option<String>,

    /// Public OAuth2 client id registered for this binary. Must be a public
    /// client (no client secret ships in a binary distributed to laptops).
    #[arg(long, env = "GOVERNANCE_AUTH_CLIENT_ID", global = true)]
    client_id: Option<String>,

    /// Space-separated OAuth2 scopes to request. `Option`, deliberately not
    /// a clap `default_value`: see this module's doc for why a compiled
    /// default has to be applied in [`Self::resolve`] instead, after the
    /// config-file layers get a chance to supply it.
    #[arg(long, env = "GOVERNANCE_AUTH_SCOPES", global = true)]
    scopes: Option<String>,

    /// Optional `resource`/`audience` parameter, if the authorization server
    /// needs one to scope the issued token to the gateway.
    #[arg(long, env = "GOVERNANCE_AUTH_AUDIENCE", global = true)]
    audience: Option<String>,

    /// OTLP collector base URL written into Claude Code's and Codex's config
    /// on `login`. Signal suffixes (`/v1/metrics`, ...) are appended by those
    /// tools' own SDKs -- pass the base, not a per-signal path. Same
    /// HTTPS-or-loopback rule as `--issuer`: telemetry carries prompts and
    /// tool detail, so it must not go out in plaintext by typo.
    #[arg(long, env = "GOVERNANCE_AUTH_OTEL_ENDPOINT", value_parser = parse_issuer, global = true)]
    otel_endpoint: Option<String>,

    /// Long-lived OTLP ingest credential. Written verbatim into both tools'
    /// config as an `Authorization: Bearer` header.
    ///
    /// Deliberately NOT the Keycloak access token: neither tool re-reads its
    /// config mid-session and neither has a credential-helper hook for OTLP
    /// headers, so a 300s token would export for five minutes and then fail
    /// silently. See `crate::otel`'s module doc.
    #[arg(long, env = "GOVERNANCE_AUTH_OTEL_TOKEN", global = true)]
    otel_token: Option<String>,

    /// Base URL of the AI gateway, e.g. `https://api.ai.camer.digital`. When
    /// given, `configure` also writes the INFERENCE wiring (Claude Code's
    /// `ANTHROPIC_BASE_URL` + `apiKeyHelper`, Codex's provider block) rather
    /// than telemetry alone -- the "no bash script required" half of ADR-0010.
    /// Left unset, `configure` touches telemetry only, exactly as before.
    ///
    /// The per-client paths underneath it are this gateway's (Envoy AI
    /// Gateway) layout, both verified live: `<gateway>/anthropic/v1/messages`
    /// and `<gateway>/v1/chat/completions` each return 200.
    #[arg(long, env = "GOVERNANCE_AUTH_GATEWAY_URL", value_parser = parse_issuer, global = true)]
    gateway_url: Option<String>,

    /// Where VS Code Copilot Chat's file exporter writes its OTel records,
    /// which `copilot-push` drains. Defaults to `copilot-otel.jsonl` in the
    /// state directory -- resolved in `crate::copilot::resolve_spool_path`,
    /// NOT as a clap `default_value`, because the default depends on `$HOME`
    /// and a `default_value` fires before either config-file layer is read
    /// (this module's doc).
    ///
    /// Deliberately NOT validated as a path that exists: Copilot creates the
    /// file on its first export, so a developer configuring this before
    /// restarting VS Code would otherwise be told their correct path is wrong.
    #[arg(long, env = "GOVERNANCE_AUTH_COPILOT_SPOOL_PATH", global = true)]
    copilot_spool_path: Option<String>,

    /// How often Claude Code re-runs `otel-headers` for fresh OTLP headers.
    /// Default 240s, deliberately under Keycloak's 300s access-token
    /// lifetime -- Claude Code's own default is 29 MINUTES, which would mean
    /// exporting with an expired token for most of every half hour, and
    /// failing silently while doing it. `Option`, not `default_value_t`: see
    /// this module's doc.
    #[arg(long, env = "GOVERNANCE_AUTH_OTEL_HEADERS_DEBOUNCE_MS", global = true)]
    otel_headers_debounce_ms: Option<u64>,

    /// Whether `login` launches the system browser automatically. OFF by
    /// default (issue #141): SSH sessions, containers, CI and VM-based
    /// testing all inherit a `DISPLAY`/`xdg-open` that either fails or
    /// hijacks an unrelated desktop, and the authorize URL is printed either
    /// way -- so auto-opening is wrong more often than it's right. Only
    /// `login`'s loopback (non-`--device-code`) path reads this.
    ///
    /// `Option<bool>`, not a plain clap flag (`ArgAction::SetTrue`): a bare
    /// `SetTrue` bakes in "false" the instant the flag is absent, before any
    /// config-file layer is consulted -- exactly the `default_value` trap
    /// this module's doc warns about, just for a bool instead of a string.
    /// `num_args = 0..=1` + `default_missing_value` keeps `--open-browser`
    /// usable bare (no `=true` needed) while still leaving the field `None`,
    /// not `Some(false)`, when the flag is never mentioned at all.
    #[arg(
        long,
        env = "GOVERNANCE_AUTH_OPEN_BROWSER",
        num_args = 0..=1,
        default_missing_value = "true",
        global = true
    )]
    open_browser: Option<bool>,

    /// Whether `token`/`otel-headers` exchange the cached upstream access
    /// token for a downstream one before printing it (RFC 8693). OFF by
    /// default -- opt-in only. See [`ExchangeTokenEndpoint`] and
    /// [`ExchangeConfig`] for what else must be configured once this is on,
    /// and `oauth::exchange`'s module doc for the fail-closed contract: a
    /// misconfigured or failing exchange must never fall back to emitting
    /// the un-exchanged upstream token. Same `Option<bool>` reasoning as
    /// `open_browser`, above.
    #[arg(
        long,
        env = "GOVERNANCE_AUTH_TOKEN_EXCHANGE",
        num_args = 0..=1,
        default_missing_value = "true",
        global = true
    )]
    token_exchange: Option<bool>,

    /// The token-exchange issuer, discovered via OIDC discovery the same way
    /// the primary `--issuer` is -- only consulted when
    /// `--exchange-token-endpoint` is absent (discovery costs a round trip
    /// the explicit form skips). Deliberately a SEPARATE field from
    /// `--issuer`: "authenticate at A, present credentials minted by B" is
    /// the whole point of token exchange, so the two issuers must be able to
    /// differ.
    #[arg(long, env = "GOVERNANCE_AUTH_EXCHANGE_ISSUER", value_parser = parse_issuer, global = true)]
    exchange_issuer: Option<String>,

    /// The token-exchange endpoint, given directly rather than discovered.
    /// Takes precedence over `--exchange-issuer` when both are set (skips a
    /// discovery round trip for the more specific value; not an error the
    /// way `otel_token`/`otel_token_file` both-set is, because these two
    /// aren't independently meaningful ways to say the same secret -- one is
    /// just a shortcut past the other's discovery step).
    #[arg(
        long,
        env = "GOVERNANCE_AUTH_EXCHANGE_TOKEN_ENDPOINT",
        value_parser = parse_exchange_token_endpoint,
        global = true
    )]
    exchange_token_endpoint: Option<String>,

    /// The `client_id` presented on the exchange request. Required when
    /// `--token-exchange` is on; see lightbridge-authz's
    /// `docs/token-exchange-integration.md` for why this specific client id
    /// (not the primary `--client-id`) must appear in the subject token's
    /// `aud` claim for the exchange to succeed at all.
    #[arg(long, env = "GOVERNANCE_AUTH_EXCHANGE_CLIENT_ID", global = true)]
    exchange_client_id: Option<String>,

    /// Space-separated scopes requested on the exchange, if any. Optional --
    /// omitting it grants the exchange server's allow-list minus
    /// `offline_access` (see the integration guide's "Scope semantics").
    #[arg(long, env = "GOVERNANCE_AUTH_EXCHANGE_SCOPES", global = true)]
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

        let token_exchange = resolve_token_exchange(
            self.token_exchange,
            self.exchange_issuer.clone(),
            self.exchange_token_endpoint.clone(),
            self.exchange_client_id.clone(),
            self.exchange_scopes,
            per_user.as_ref(),
            machine.as_ref(),
        )?;

        Ok(OauthConfig {
            issuer,
            client_id,
            scopes,
            audience,
            otel_endpoint,
            otel_token,
            gateway_url,
            copilot_spool_path,
            otel_headers_debounce_ms,
            open_browser,
            token_exchange,
        })
    }
}

/// The token-exchange sub-block of [`OauthConfigArgs::resolve_with_paths`],
/// split out as a free function (taking the five raw fields by value, rather
/// than `&self`) because it's a five-field group with its own internal
/// validation (client id required, exactly one of issuer/token-endpoint
/// required) -- inlining it would make the parent function's field-by-field
/// shape harder to scan. A `&self`-taking method would not compile here:
/// by the point this is called, several *other* fields of `self` have
/// already been individually moved out via `self.field.or_else(...)`
/// (`Option<String>` isn't `Copy`), and Rust does not allow borrowing a
/// struct as a whole once any one of its fields has been partially moved,
/// even to read a field that was never touched.
fn resolve_token_exchange(
    token_exchange: Option<bool>,
    exchange_issuer: Option<String>,
    exchange_token_endpoint: Option<String>,
    exchange_client_id: Option<String>,
    exchange_scopes: Option<String>,
    per_user: Option<&config_file::ConfigFile>,
    machine: Option<&config_file::ConfigFile>,
) -> Result<Option<ExchangeConfig>> {
    let enabled = token_exchange
        .or_else(|| per_user.and_then(|file| file.token_exchange))
        .or_else(|| machine.and_then(|file| file.token_exchange))
        .unwrap_or(false);

    if !enabled {
        return Ok(None);
    }

    let exchange_issuer = exchange_issuer
        .or_else(|| per_user.and_then(|file| file.exchange_issuer.clone()))
        .or_else(|| machine.and_then(|file| file.exchange_issuer.clone()))
        .map(|value| parse_issuer(&value).map_err(|error| anyhow::anyhow!(error)))
        .transpose()?;

    let exchange_token_endpoint = exchange_token_endpoint
        .or_else(|| per_user.and_then(|file| file.exchange_token_endpoint.clone()))
        .or_else(|| machine.and_then(|file| file.exchange_token_endpoint.clone()))
        .map(|value| parse_exchange_token_endpoint(&value).map_err(|error| anyhow::anyhow!(error)))
        .transpose()?;

    let client_id = exchange_client_id
        .or_else(|| per_user.and_then(|file| file.exchange_client_id.clone()))
        .or_else(|| machine.and_then(|file| file.exchange_client_id.clone()))
        .context(
            "--exchange-client-id (or GOVERNANCE_AUTH_EXCHANGE_CLIENT_ID, or \
             `exchange_client_id` in a config file) is required when token exchange \
             (--token-exchange) is enabled",
        )?;

    let scopes = exchange_scopes
        .or_else(|| per_user.and_then(|file| file.exchange_scopes.clone()))
        .or_else(|| machine.and_then(|file| file.exchange_scopes.clone()));

    let token_endpoint = match (exchange_token_endpoint, exchange_issuer) {
        (Some(endpoint), _) => ExchangeTokenEndpoint::Explicit(endpoint),
        (None, Some(issuer)) => ExchangeTokenEndpoint::Issuer(issuer),
        (None, None) => bail!(
            "token exchange (--token-exchange) is enabled but neither \
             --exchange-token-endpoint (GOVERNANCE_AUTH_EXCHANGE_TOKEN_ENDPOINT) nor \
             --exchange-issuer (GOVERNANCE_AUTH_EXCHANGE_ISSUER) is set, in a flag, env var, or \
             config file"
        ),
    };

    Ok(Some(ExchangeConfig {
        token_endpoint,
        client_id,
        scopes,
    }))
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
    /// `None` means "use the compiled default under the state directory" --
    /// see `crate::copilot::resolve_spool_path`, which is where the fifth
    /// layer is applied.
    pub copilot_spool_path: Option<String>,
    pub otel_headers_debounce_ms: u64,
    /// Whether `login`'s loopback flow launches the system browser
    /// automatically. See `OauthConfigArgs::open_browser`'s doc for why this
    /// defaults to `false` (issue #141).
    pub open_browser: bool,
    /// Present only when token exchange (RFC 8693) is enabled -- `None` is
    /// the ONLY representation of "off", so there is no separate bool that
    /// could drift out of sync with these fields. See `oauth::exchange`.
    pub token_exchange: Option<ExchangeConfig>,
}

/// Resolved RFC 8693 token-exchange configuration, built by
/// [`resolve_token_exchange`] only when `--token-exchange` (or its env
/// var/config-file equivalent) is on. See `oauth::exchange`'s module doc for
/// the request this drives and its fail-closed contract, and
/// lightbridge-authz's `docs/token-exchange-integration.md` for the wire
/// contract itself.
#[derive(Debug, Clone)]
pub struct ExchangeConfig {
    pub token_endpoint: ExchangeTokenEndpoint,
    pub client_id: String,
    pub scopes: Option<String>,
}

/// Where the token-exchange request goes. An explicit
/// `--exchange-token-endpoint` is used as-is; `--exchange-issuer` costs one
/// OIDC discovery round trip (cached, same as the primary `--issuer` --
/// see `oauth::discovery`) to find it.
#[derive(Debug, Clone)]
pub enum ExchangeTokenEndpoint {
    Explicit(String),
    Issuer(String),
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
/// flag is explicitly NOT an issuer (see its doc comment on
/// [`OauthConfigArgs::exchange_token_endpoint`]), it's the resolved token
/// endpoint itself, given directly to skip a discovery round trip.
fn parse_exchange_token_endpoint(raw: &str) -> Result<String, String> {
    parse_url("endpoint", raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_loopback_http_issuer() {
        let error = parse_issuer("http://auth.example.com/realms/platform")
            .expect_err("plaintext non-loopback issuer must be rejected");
        assert!(
            error.contains("HTTPS"),
            "error should explain the HTTPS requirement, got: {error}"
        );
    }

    #[test]
    fn accepts_https_issuer() {
        assert!(parse_issuer("https://auth.example.com/realms/platform").is_ok());
    }

    #[test]
    fn accepts_loopback_http_issuer() {
        assert!(parse_issuer("http://127.0.0.1:4181/realms/platform").is_ok());
    }

    /// `--exchange-token-endpoint` is explicitly not an issuer -- the error
    /// message for an unparseable value must say "endpoint", not "issuer",
    /// so an operator who typo'd this flag isn't told the wrong flag is
    /// wrong. Regression test for the message `parse_issuer` used to
    /// produce here (it was reused verbatim, hardcoding "issuer" for both).
    #[test]
    fn exchange_token_endpoint_parse_error_names_endpoint_not_issuer() {
        let error = parse_exchange_token_endpoint("not a url")
            .expect_err("an unparseable exchange-token-endpoint value must be rejected");
        assert!(
            error.contains("endpoint"),
            "error should say 'endpoint', not 'issuer' -- this flag is a token endpoint, not an \
             issuer, got: {error}"
        );
        assert!(
            !error.contains("issuer"),
            "error should not call this value an issuer, got: {error}"
        );
    }

    /// ADR-0012 Decision 2's five layers, proved pairwise: flag beats env,
    /// env beats per-user file, per-user file beats machine-wide file,
    /// machine-wide file beats the compiled default.
    ///
    /// Every test below drives [`OauthConfigArgs::resolve_with_paths`]
    /// directly with temp-file paths for the two file layers, rather than
    /// going through `resolve()`'s real `/etc/governance-auth/config.toml`
    /// and `$HOME`-derived per-user path -- that's what "the paths are
    /// injectable" buys: these tests never touch the real filesystem
    /// locations, so they're hermetic and safe to run in parallel with
    /// every other test in this crate, including ones that touch a real
    /// `$HOME` through the subprocess harness in `tests/`.
    mod precedence {
        use std::sync::atomic::{AtomicU64, Ordering};

        use super::*;

        /// Minimal scratch dir, removed on drop -- same hand-rolled pattern
        /// used in `otel.rs`'s and `config_file.rs`'s own test modules.
        struct TempDir(std::path::PathBuf);

        impl TempDir {
            fn path(&self) -> &Path {
                &self.0
            }
        }

        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        fn tempdir() -> TempDir {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "governance-auth-config-precedence-{}-{unique}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("create temp dir");
            TempDir(path)
        }

        /// A path in a freshly-made temp dir that nothing has written to --
        /// `config_file::load` treats a missing file as "this layer has
        /// nothing to say", not an error, so this stands in for "this layer
        /// is absent" throughout.
        fn absent_path(dir: &TempDir) -> std::path::PathBuf {
            dir.path().join("absent.toml")
        }

        fn write_config(dir: &TempDir, name: &str, contents: &str) -> std::path::PathBuf {
            let path = dir.path().join(name);
            std::fs::write(&path, contents).expect("write test config file");
            path
        }

        /// The minimal always-present layer: issuer/client-id as if parsed
        /// from a flag or env var (clap has already merged those two by the
        /// time `OauthConfigArgs` exists, so this crate has no way to tell
        /// them apart downstream -- see `tests/config_precedence.rs` for the
        /// one layer that's actually clap's job).
        fn base_args() -> OauthConfigArgs {
            OauthConfigArgs {
                issuer: Some("https://issuer.example/realms/platform".to_owned()),
                client_id: Some("cli".to_owned()),
                scopes: None,
                audience: None,
                otel_endpoint: None,
                otel_token: None,
                gateway_url: None,
                copilot_spool_path: None,
                otel_headers_debounce_ms: None,
                open_browser: None,
                token_exchange: None,
                exchange_issuer: None,
                exchange_token_endpoint: None,
                exchange_client_id: None,
                exchange_scopes: None,
            }
        }

        /// Layer 1 vs layer 2 (clap flag vs its own env var) is proved in
        /// `tests/config_precedence.rs`, end-to-end through a real
        /// subprocess with `GOVERNANCE_AUTH_SCOPES` set on the *child's*
        /// environment via `Command::env`, not here: that precedence step
        /// happens INSIDE clap, before `OauthConfigArgs` is ever
        /// constructed by hand, and proving it would otherwise require
        /// mutating this test binary's own process environment -- which
        /// `std::env::set_var` can do, but only as `unsafe` since Rust 2024,
        /// and this workspace denies `unsafe_code` outright (see root
        /// `Cargo.toml`). A subprocess's environment is safe, ordinary
        /// `Command::env`, so that's where this one lives.
        ///
        /// This is also the exact case the `default_value`/`default_value_t`
        /// trap made impossible to observe: with a clap default in place,
        /// `scopes` was never `None`, so there was nothing for a config file
        /// (or, transitively, an env-var-vs-file test) to ever win against.
        /// Layer 2 vs layer 3: an env-var-sourced value (indistinguishable,
        /// by the time `OauthConfigArgs` exists, from a flag-sourced one --
        /// see the test above) must win over a per-user config file.
        #[test]
        fn env_or_flag_value_wins_over_per_user_file_for_scopes() {
            let dir = tempdir();
            let per_user = write_config(&dir, "per-user.toml", "scopes = \"per-user-scope\"\n");
            let machine = write_config(&dir, "machine.toml", "scopes = \"machine-scope\"\n");

            let args = OauthConfigArgs {
                scopes: Some("cli-or-env-scope".to_owned()),
                ..base_args()
            };
            let resolved = args
                .resolve_with_paths(&per_user, &machine)
                .expect("resolve");
            assert_eq!(resolved.scopes, "cli-or-env-scope");
        }

        /// Layer 3 vs layer 4: with no flag/env value, the per-user file
        /// must win over the machine-wide file.
        #[test]
        fn per_user_file_wins_over_machine_file_for_scopes() {
            let dir = tempdir();
            let per_user = write_config(&dir, "per-user.toml", "scopes = \"per-user-scope\"\n");
            let machine = write_config(&dir, "machine.toml", "scopes = \"machine-scope\"\n");

            let resolved = base_args()
                .resolve_with_paths(&per_user, &machine)
                .expect("resolve");
            assert_eq!(resolved.scopes, "per-user-scope");
        }

        /// Layer 4 vs layer 5: with no flag/env value and no per-user file,
        /// the machine-wide file must win over the compiled default.
        #[test]
        fn machine_file_wins_over_compiled_default_for_scopes() {
            let dir = tempdir();
            let per_user = absent_path(&dir);
            let machine = write_config(&dir, "machine.toml", "scopes = \"machine-scope\"\n");

            let resolved = base_args()
                .resolve_with_paths(&per_user, &machine)
                .expect("resolve");
            assert_eq!(resolved.scopes, "machine-scope");
        }

        /// THE regression guard for the `default_value`/`default_value_t`
        /// trap this whole module exists to avoid -- and the only test in
        /// this file that can catch it, because it's the only one that goes
        /// through real clap parsing (`OauthConfigArgs::try_parse_from`)
        /// instead of hand-constructing the struct.
        ///
        /// Every other precedence test here builds `OauthConfigArgs` by hand
        /// (`OauthConfigArgs { scopes: None, .. }`), which proves the
        /// *layering logic* in `resolve_with_paths` is correct but can never
        /// observe a mistake in the `#[arg(...)]` attribute itself: clap
        /// fills a `default_value` in BEFORE `OauthConfigArgs` exists as a
        /// value a test could construct differently, so a hand-built
        /// `scopes: None` stays `None` even if the real CLI would never
        /// produce it. This test parses `--issuer`/`--client-id` alone (no
        /// `--scopes`, and nothing sets `GOVERNANCE_AUTH_SCOPES` in this
        /// process) through the ACTUAL `OauthConfigArgs`, then feeds the
        /// result into `resolve_with_paths` against a machine-wide file that
        /// sets `scopes` -- so a `default_value` reintroduced on the real
        /// `#[arg(...)]` would make `scopes` `Some("openid profile
        /// offline_access")` right out of clap, and this test would fail
        /// with the machine file's value never taking effect.
        #[test]
        fn clap_default_value_would_defeat_the_config_file_layer() {
            use clap::Parser as _;

            #[derive(Debug, clap::Parser)]
            struct TestCli {
                #[command(flatten)]
                oauth: OauthConfigArgs,
            }

            let cli = TestCli::try_parse_from([
                "governance-auth",
                "--issuer",
                "https://issuer.example",
                "--client-id",
                "cli",
            ])
            .expect("parse with no --scopes flag");

            let dir = tempdir();
            let per_user = absent_path(&dir);
            let machine = write_config(&dir, "machine.toml", "scopes = \"machine-scope\"\n");

            let resolved = cli
                .oauth
                .resolve_with_paths(&per_user, &machine)
                .expect("resolve");
            assert_eq!(
                resolved.scopes, "machine-scope",
                "a real clap parse with no --scopes flag must still fall through to the \
                 machine-wide config file -- if this fails with the compiled default instead, \
                 `default_value` has been reintroduced on the `scopes` arg"
            );
        }

        /// The `otel_headers_debounce_ms` counterpart to the test above.
        ///
        /// Note the name: the ORIGINAL trap used `default_value_t = 240_000`
        /// (this field was a bare `u64`), but that specific form no longer
        /// even compiles once the field is `Option<u64>` --
        /// `default_value_t` requires the field's own type to implement
        /// `Display`, and `Option<u64>` doesn't. That's a free compile-time
        /// guard against literally reintroducing the old attribute verbatim.
        /// It does NOT guard against the string form, `default_value =
        /// "240000"`, which compiles fine on `Option<u64>` (clap parses the
        /// string at runtime) and reintroduces the exact same bug silently
        /// -- confirmed by sabotaging with that form specifically, not
        /// `default_value_t`, when this test was written.
        #[test]
        fn clap_default_value_t_would_defeat_the_config_file_layer_for_debounce_ms() {
            use clap::Parser as _;

            #[derive(Debug, clap::Parser)]
            struct TestCli {
                #[command(flatten)]
                oauth: OauthConfigArgs,
            }

            let cli = TestCli::try_parse_from([
                "governance-auth",
                "--issuer",
                "https://issuer.example",
                "--client-id",
                "cli",
            ])
            .expect("parse with no --otel-headers-debounce-ms flag");

            let dir = tempdir();
            let per_user = absent_path(&dir);
            let machine = write_config(&dir, "machine.toml", "otel_headers_debounce_ms = 12345\n");

            let resolved = cli
                .oauth
                .resolve_with_paths(&per_user, &machine)
                .expect("resolve");
            assert_eq!(
                resolved.otel_headers_debounce_ms, 12_345,
                "a real clap parse with no --otel-headers-debounce-ms flag must still fall \
                 through to the machine-wide config file -- if this fails with the compiled \
                 default instead, `default_value_t` has been reintroduced on that arg"
            );
        }

        /// Layer 5: with nothing configured anywhere, the compiled default
        /// applies. NOTE: this test alone would still pass if `default_value`
        /// were reintroduced on `scopes` -- it hand-constructs
        /// `OauthConfigArgs { scopes: None, .. }` directly, bypassing clap
        /// entirely, so a clap-level attribute regression is invisible to
        /// it. `clap_default_value_would_defeat_the_config_file_layer`,
        /// below, is the one that actually goes through clap and catches
        /// that specific regression -- see its doc for why.
        #[test]
        fn compiled_default_applies_when_nothing_else_is_configured_for_scopes() {
            let dir = tempdir();
            let per_user = absent_path(&dir);
            let machine = absent_path(&dir);

            let resolved = base_args()
                .resolve_with_paths(&per_user, &machine)
                .expect("resolve");
            assert_eq!(resolved.scopes, DEFAULT_SCOPES);
        }

        /// The same compiled-default fallback, for the *other* field the
        /// ADR calls out by name (`otel_headers_debounce_ms`) -- a single
        /// green test on `scopes` wouldn't catch a `default_value_t` left in
        /// place on this one specifically.
        #[test]
        fn compiled_default_applies_when_nothing_else_is_configured_for_debounce_ms() {
            let dir = tempdir();
            let per_user = absent_path(&dir);
            let machine = absent_path(&dir);

            let resolved = base_args()
                .resolve_with_paths(&per_user, &machine)
                .expect("resolve");
            assert_eq!(
                resolved.otel_headers_debounce_ms,
                DEFAULT_OTEL_HEADERS_DEBOUNCE_MS
            );
        }

        /// A machine-wide file supplying `otel_headers_debounce_ms` must be
        /// consulted at all -- the field-specific version of the
        /// machine-vs-default test above.
        #[test]
        fn machine_file_wins_over_compiled_default_for_debounce_ms() {
            let dir = tempdir();
            let per_user = absent_path(&dir);
            let machine = write_config(&dir, "machine.toml", "otel_headers_debounce_ms = 12345\n");

            let resolved = base_args()
                .resolve_with_paths(&per_user, &machine)
                .expect("resolve");
            assert_eq!(resolved.otel_headers_debounce_ms, 12_345);
        }

        /// A per-user file must win over a machine-wide file for
        /// `otel_headers_debounce_ms` too.
        #[test]
        fn per_user_file_wins_over_machine_file_for_debounce_ms() {
            let dir = tempdir();
            let per_user = write_config(&dir, "per-user.toml", "otel_headers_debounce_ms = 111\n");
            let machine = write_config(&dir, "machine.toml", "otel_headers_debounce_ms = 222\n");

            let resolved = base_args()
                .resolve_with_paths(&per_user, &machine)
                .expect("resolve");
            assert_eq!(resolved.otel_headers_debounce_ms, 111);
        }

        /// A flag/env value must win over both file layers for
        /// `otel_headers_debounce_ms` too.
        #[test]
        fn flag_or_env_value_wins_over_both_files_for_debounce_ms() {
            let dir = tempdir();
            let per_user = write_config(&dir, "per-user.toml", "otel_headers_debounce_ms = 111\n");
            let machine = write_config(&dir, "machine.toml", "otel_headers_debounce_ms = 222\n");

            let args = OauthConfigArgs {
                otel_headers_debounce_ms: Some(999),
                ..base_args()
            };
            let resolved = args
                .resolve_with_paths(&per_user, &machine)
                .expect("resolve");
            assert_eq!(resolved.otel_headers_debounce_ms, 999);
        }

        /// `issuer`/`client_id` go through the exact same layering, not a
        /// separate code path -- pinned here so a future refactor that
        /// special-cases them can't quietly drop the file layers for the
        /// two fields that matter most (nothing else works without them).
        #[test]
        fn issuer_and_client_id_also_fall_through_to_config_files() {
            let dir = tempdir();
            let per_user = absent_path(&dir);
            let machine = write_config(
                &dir,
                "machine.toml",
                "issuer = \"https://from-machine-config.example\"\nclient_id = \"machine-client\"\n",
            );

            let args = OauthConfigArgs {
                issuer: None,
                client_id: None,
                ..base_args()
            };
            let resolved = args
                .resolve_with_paths(&per_user, &machine)
                .expect("resolve");
            assert_eq!(resolved.issuer, "https://from-machine-config.example");
            assert_eq!(resolved.client_id, "machine-client");
        }

        /// A config-file-sourced `issuer` gets the same HTTPS-or-loopback
        /// validation a CLI/env one already gets at clap-parse time --
        /// config files bypass clap entirely, so without this a plaintext
        /// typo in `/etc/governance-auth/config.toml` would reach the
        /// network unchecked.
        #[test]
        fn a_config_file_issuer_is_still_validated_for_transport_security() {
            let dir = tempdir();
            let per_user = absent_path(&dir);
            let machine = write_config(
                &dir,
                "machine.toml",
                "issuer = \"http://not-loopback.example\"\n",
            );

            let args = OauthConfigArgs {
                issuer: None,
                ..base_args()
            };
            let error = args
                .resolve_with_paths(&per_user, &machine)
                .expect_err("a plaintext non-loopback issuer from a config file must be rejected");
            assert!(format!("{error:#}").contains("HTTPS"));
        }

        /// Still required when absent from every layer -- config files
        /// don't relax the "issuer/client-id must be present" rule, they
        /// just add two more places it can come from.
        #[test]
        fn missing_issuer_everywhere_is_still_an_error() {
            let dir = tempdir();
            let per_user = absent_path(&dir);
            let machine = absent_path(&dir);

            let args = OauthConfigArgs {
                issuer: None,
                ..base_args()
            };
            let error = args
                .resolve_with_paths(&per_user, &machine)
                .expect_err("no issuer anywhere must be an error");
            assert!(format!("{error:#}").contains("--issuer"));
        }

        /// A malformed config file must fail loudly rather than being
        /// treated as absent -- silently falling through to the next,
        /// weaker layer would hide a real typo in a file an operator
        /// believes is in effect.
        #[test]
        fn a_malformed_per_user_file_is_a_loud_error_not_a_silent_fallthrough() {
            let dir = tempdir();
            let per_user = write_config(&dir, "per-user.toml", "not = [valid toml");
            let machine = absent_path(&dir);

            let error = base_args()
                .resolve_with_paths(&per_user, &machine)
                .expect_err("malformed TOML must be an error, not silently skipped");
            assert!(format!("{error:#}").contains(&per_user.display().to_string()));
        }

        /// `open_browser` (issue #141): with nothing configured anywhere,
        /// the compiled default is `false` -- pinned so the browser default
        /// stays off without needing a clap `default_value` (see this
        /// module's doc and `clap_default_value_would_defeat_the_config_file_layer`
        /// for why that specific mechanism must not be used here either).
        #[test]
        fn open_browser_compiled_default_is_false() {
            let dir = tempdir();
            let per_user = absent_path(&dir);
            let machine = absent_path(&dir);

            let resolved = base_args()
                .resolve_with_paths(&per_user, &machine)
                .expect("resolve");
            assert!(!resolved.open_browser);
        }

        /// A machine-wide file turning `open_browser` on must be honoured --
        /// the field-specific version of the "machine file beats compiled
        /// default" proof every other option already has.
        #[test]
        fn machine_file_wins_over_compiled_default_for_open_browser() {
            let dir = tempdir();
            let per_user = absent_path(&dir);
            let machine = write_config(&dir, "machine.toml", "open_browser = true\n");

            let resolved = base_args()
                .resolve_with_paths(&per_user, &machine)
                .expect("resolve");
            assert!(resolved.open_browser);
        }

        /// A per-user file must win over a machine-wide file for
        /// `open_browser` too.
        #[test]
        fn per_user_file_wins_over_machine_file_for_open_browser() {
            let dir = tempdir();
            let per_user = write_config(&dir, "per-user.toml", "open_browser = true\n");
            let machine = write_config(&dir, "machine.toml", "open_browser = false\n");

            let resolved = base_args()
                .resolve_with_paths(&per_user, &machine)
                .expect("resolve");
            assert!(resolved.open_browser);
        }

        /// A flag/env value must win over both file layers for
        /// `open_browser` too.
        #[test]
        fn flag_or_env_value_wins_over_both_files_for_open_browser() {
            let dir = tempdir();
            let per_user = write_config(&dir, "per-user.toml", "open_browser = true\n");
            let machine = write_config(&dir, "machine.toml", "open_browser = true\n");

            let args = OauthConfigArgs {
                open_browser: Some(false),
                ..base_args()
            };
            let resolved = args
                .resolve_with_paths(&per_user, &machine)
                .expect("resolve");
            assert!(
                !resolved.open_browser,
                "an explicit false from a flag/env must win over both files' true"
            );
        }

        /// `--open-browser` used bare (no `=true`) must resolve to `true`
        /// through REAL clap parsing -- the tri-state `Option<bool>`
        /// (`num_args = 0..=1` + `default_missing_value`) equivalent of
        /// `clap_default_value_would_defeat_the_config_file_layer`: this is
        /// the one test in this module that would catch a regression to a
        /// plain `ArgAction::SetTrue` bool flag, which would compile fine
        /// but bake in `false` the instant the flag is absent -- the same
        /// trap `default_value` sets for a string/int field.
        #[test]
        fn open_browser_flag_used_bare_resolves_to_true_through_real_clap_parsing() {
            use clap::Parser as _;

            #[derive(Debug, clap::Parser)]
            struct TestCli {
                #[command(flatten)]
                oauth: OauthConfigArgs,
            }

            let cli = TestCli::try_parse_from([
                "governance-auth",
                "--issuer",
                "https://issuer.example",
                "--client-id",
                "cli",
                "--open-browser",
            ])
            .expect("parse with a bare --open-browser flag");

            let dir = tempdir();
            let per_user = absent_path(&dir);
            let machine = absent_path(&dir);
            let resolved = cli
                .oauth
                .resolve_with_paths(&per_user, &machine)
                .expect("resolve");
            assert!(
                resolved.open_browser,
                "a bare --open-browser must resolve to true"
            );
        }

        /// Same real-clap-parsing proof for the OTHER direction: with
        /// `--open-browser` never mentioned at all, a real parse must leave
        /// it `None` internally so the machine-wide file still gets a
        /// chance -- catches a regression to a plain bool field with an
        /// implicit `false` default just as surely as the bare-flag test
        /// above catches the opposite mistake.
        #[test]
        fn open_browser_absent_from_a_real_clap_parse_still_falls_through_to_the_machine_file() {
            use clap::Parser as _;

            #[derive(Debug, clap::Parser)]
            struct TestCli {
                #[command(flatten)]
                oauth: OauthConfigArgs,
            }

            let cli = TestCli::try_parse_from([
                "governance-auth",
                "--issuer",
                "https://issuer.example",
                "--client-id",
                "cli",
            ])
            .expect("parse with no --open-browser flag");

            let dir = tempdir();
            let per_user = absent_path(&dir);
            let machine = write_config(&dir, "machine.toml", "open_browser = true\n");
            let resolved = cli
                .oauth
                .resolve_with_paths(&per_user, &machine)
                .expect("resolve");
            assert!(
                resolved.open_browser,
                "with --open-browser absent from the real CLI parse, the machine-wide file's \
                 `open_browser = true` must still take effect"
            );
        }

        /// Token exchange (issue #140) is OFF by default: with nothing
        /// configured anywhere, `resolved.token_exchange` must be `None`,
        /// the ONLY representation of "disabled" (see `ExchangeConfig`'s
        /// doc).
        #[test]
        fn token_exchange_is_off_by_default() {
            let dir = tempdir();
            let per_user = absent_path(&dir);
            let machine = absent_path(&dir);

            let resolved = base_args()
                .resolve_with_paths(&per_user, &machine)
                .expect("resolve");
            assert!(resolved.token_exchange.is_none());
        }

        /// Enabling exchange without an exchange client id must be a loud
        /// error naming the missing flag, not a silently-disabled exchange
        /// or a panic reaching for a value that was never there.
        #[test]
        fn token_exchange_enabled_without_a_client_id_is_an_error() {
            let dir = tempdir();
            let per_user = absent_path(&dir);
            let machine = absent_path(&dir);

            let args = OauthConfigArgs {
                token_exchange: Some(true),
                exchange_issuer: Some("https://exchange.example".to_owned()),
                ..base_args()
            };
            let error = args
                .resolve_with_paths(&per_user, &machine)
                .expect_err("token exchange without a client id must be rejected");
            assert!(format!("{error:#}").contains("--exchange-client-id"));
        }

        /// Enabling exchange without EITHER an exchange issuer or an
        /// explicit exchange token endpoint must also be a loud error --
        /// there is nowhere to send the exchange request otherwise.
        #[test]
        fn token_exchange_enabled_without_an_issuer_or_token_endpoint_is_an_error() {
            let dir = tempdir();
            let per_user = absent_path(&dir);
            let machine = absent_path(&dir);

            let args = OauthConfigArgs {
                token_exchange: Some(true),
                exchange_client_id: Some("exchange-cli".to_owned()),
                ..base_args()
            };
            let error = args
                .resolve_with_paths(&per_user, &machine)
                .expect_err("token exchange without an issuer/token-endpoint must be rejected");
            assert!(format!("{error:#}").contains("--exchange-token-endpoint"));
            assert!(format!("{error:#}").contains("--exchange-issuer"));
        }

        /// A fully-specified exchange config (issuer form) resolves to
        /// `Some(ExchangeConfig)` with every field carried through, proving
        /// the happy path end-to-end through `resolve_with_paths` rather
        /// than just its error branches.
        #[test]
        fn token_exchange_fully_specified_via_issuer_resolves() {
            let dir = tempdir();
            let per_user = absent_path(&dir);
            let machine = absent_path(&dir);

            let args = OauthConfigArgs {
                token_exchange: Some(true),
                exchange_issuer: Some("https://exchange.example".to_owned()),
                exchange_client_id: Some("exchange-cli".to_owned()),
                exchange_scopes: Some("openid profile".to_owned()),
                ..base_args()
            };
            let resolved = args
                .resolve_with_paths(&per_user, &machine)
                .expect("resolve");
            let exchange = resolved
                .token_exchange
                .expect("token exchange must be Some when fully specified");
            assert_eq!(exchange.client_id, "exchange-cli");
            assert_eq!(exchange.scopes.as_deref(), Some("openid profile"));
            assert!(matches!(
                exchange.token_endpoint,
                ExchangeTokenEndpoint::Issuer(ref issuer) if issuer == "https://exchange.example"
            ));
        }

        /// An explicit `--exchange-token-endpoint` takes precedence over
        /// `--exchange-issuer` when both are set -- documented behaviour in
        /// `OauthConfigArgs::exchange_token_endpoint`'s doc, pinned here so
        /// a refactor can't silently flip which one wins.
        #[test]
        fn token_exchange_explicit_token_endpoint_wins_over_issuer() {
            let dir = tempdir();
            let per_user = absent_path(&dir);
            let machine = absent_path(&dir);

            let args = OauthConfigArgs {
                token_exchange: Some(true),
                exchange_issuer: Some("https://exchange.example".to_owned()),
                exchange_token_endpoint: Some("https://exchange.example/oauth2/token".to_owned()),
                exchange_client_id: Some("exchange-cli".to_owned()),
                ..base_args()
            };
            let resolved = args
                .resolve_with_paths(&per_user, &machine)
                .expect("resolve");
            let exchange = resolved.token_exchange.expect("token exchange resolved");
            assert!(matches!(
                exchange.token_endpoint,
                ExchangeTokenEndpoint::Explicit(ref endpoint)
                    if endpoint == "https://exchange.example/oauth2/token"
            ));
        }

        /// Every exchange field falls through the same file layering as
        /// `issuer`/`client_id` -- a machine-wide file alone can fully
        /// configure token exchange, with no flag/env involved at all.
        #[test]
        fn token_exchange_config_falls_through_to_the_machine_wide_file() {
            let dir = tempdir();
            let per_user = absent_path(&dir);
            let machine = write_config(
                &dir,
                "machine.toml",
                "token_exchange = true\n\
                 exchange_token_endpoint = \"https://exchange.example/oauth2/token\"\n\
                 exchange_client_id = \"machine-exchange-cli\"\n",
            );

            let resolved = base_args()
                .resolve_with_paths(&per_user, &machine)
                .expect("resolve");
            let exchange = resolved.token_exchange.expect("token exchange resolved");
            assert_eq!(exchange.client_id, "machine-exchange-cli");
        }

        /// A per-user file's `token_exchange = false` must win over a
        /// machine-wide file's `token_exchange = true` -- same precedence
        /// every other field has, proved specifically for the boolean gate
        /// rather than just the fields underneath it.
        #[test]
        fn per_user_file_wins_over_machine_file_for_token_exchange_enabled() {
            let dir = tempdir();
            let per_user = write_config(&dir, "per-user.toml", "token_exchange = false\n");
            let machine = write_config(
                &dir,
                "machine.toml",
                "token_exchange = true\n\
                 exchange_token_endpoint = \"https://exchange.example/oauth2/token\"\n\
                 exchange_client_id = \"machine-exchange-cli\"\n",
            );

            let resolved = base_args()
                .resolve_with_paths(&per_user, &machine)
                .expect("resolve");
            assert!(
                resolved.token_exchange.is_none(),
                "per-user token_exchange = false must win over the machine file's true"
            );
        }

        /// The `token_exchange` counterpart to
        /// `open_browser_absent_from_a_real_clap_parse_still_falls_through_to_the_machine_file`
        /// -- and, like that test, the only one in this module that can
        /// catch a `default_value`/`default_value_t` regression reintroduced
        /// on the real `#[arg(...)]` for `token_exchange`, because it's the
        /// only one that goes through REAL clap parsing
        /// (`TestCli::try_parse_from`) rather than hand-constructing
        /// `OauthConfigArgs { token_exchange: None, .. }`.
        ///
        /// Every OTHER `token_exchange` test above builds `OauthConfigArgs`
        /// by hand, which proves the layering logic in
        /// `resolve_token_exchange` is correct but can never observe a
        /// mistake in the `#[arg(...)]` attribute itself: clap fills a
        /// `default_value` in BEFORE `OauthConfigArgs` exists as a value a
        /// test could construct differently, so a hand-built
        /// `token_exchange: None` stays `None` even if the real CLI would
        /// never produce it. Confirmed by sabotaging with
        /// `default_value = "false"` on the `token_exchange` arg: with that
        /// in place, `--token-exchange` never mentioned still parses to
        /// `Some(false)`, so `resolve_token_exchange` sees `enabled = false`
        /// and never even reads the machine-wide file's `token_exchange =
        /// true` -- this test failed with `resolved.token_exchange` being
        /// `None` instead of `Some`, exactly the trap this module's doc
        /// warns about.
        #[test]
        fn token_exchange_absent_from_a_real_clap_parse_still_falls_through_to_the_machine_file() {
            use clap::Parser as _;

            #[derive(Debug, clap::Parser)]
            struct TestCli {
                #[command(flatten)]
                oauth: OauthConfigArgs,
            }

            let cli = TestCli::try_parse_from([
                "governance-auth",
                "--issuer",
                "https://issuer.example",
                "--client-id",
                "cli",
            ])
            .expect("parse with no --token-exchange flag");

            let dir = tempdir();
            let per_user = absent_path(&dir);
            let machine = write_config(
                &dir,
                "machine.toml",
                "token_exchange = true\n\
                 exchange_token_endpoint = \"https://exchange.example/oauth2/token\"\n\
                 exchange_client_id = \"machine-exchange-cli\"\n",
            );

            let resolved = cli
                .oauth
                .resolve_with_paths(&per_user, &machine)
                .expect("resolve");
            assert!(
                resolved.token_exchange.is_some(),
                "with --token-exchange absent from the real CLI parse, the machine-wide file's \
                 `token_exchange = true` must still take effect -- if this fails with `None` \
                 instead, `default_value` has been reintroduced on the `token_exchange` arg"
            );
        }
    }
}
