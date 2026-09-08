//! Writes the OpenTelemetry export configuration into Claude Code's
//! `settings.json` and Codex's `config.toml`, so pointing either tool at this
//! org's gateway also points its telemetry at this org's collector. Exporting
//! telemetry is the condition for using the endpoints, so this is wired by
//! `login` automatically rather than left as an opt-in step someone can skip.
//!
//! Both files are **merged, never rewritten**: a developer's `settings.json`
//! carries their theme/permissions and `config.toml` carries their project
//! trust levels and hand-written comments. Only the keys this module owns are
//! touched, and writing is tmp-then-rename so a crash mid-write can't leave
//! either tool with an unparseable config (Codex in particular refuses to
//! start on a malformed `config.toml` -- it doesn't degrade, it exits).
//!
//! ## Why the auth header is not the access token
//!
//! Neither tool re-reads its config mid-session, and neither has a
//! credential-helper hook for OTLP headers the way `apiKeyHelper`/
//! `auth.command` exist for the inference call -- `OTEL_EXPORTER_OTLP_HEADERS`
//! and Codex's `otel.exporter.*.headers` are static strings read once at
//! process start. A 300s Keycloak access token written here would export
//! telemetry for five minutes and then 401 silently for the rest of a session.
//! So the OTLP credential has to be long-lived with server-side revocation --
//! the same conclusion RFC-0002 reached for Foundry, for a different reason
//! (there, changing an agent's env vars requires publishing a new agent
//! version). It is supplied out of band via `--otel-token`, not minted here.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};

// The OTEL contract (fixed loopback port + client URL shape) lives in
// `otel_port` and is re-exported here so the daemon (#268) and the
// configure/managed/status consumers (#270/#271) reach it from `crate::otel`
// without a second copy — see issue #276 AC3. No in-crate consumer exists yet
// (those land with the consumers), hence the `unused_imports` expectation.
#[expect(
    unused_imports,
    reason = "shipped contract for #268's daemon and #270/#271; remove once a consumer exists"
)]
pub use crate::otel_port::{OTEL_LOOPBACK_ENDPOINT, OTEL_PORT};
use crate::redacted::Redacted;

/// Resolved OTLP export settings, shared by both writers so the two tools
/// can't drift to different endpoints or protocols.
#[derive(Debug, Clone)]
pub struct OtelSettings {
    /// The resolved issuer and client id, exported into the developer's shell
    /// so `governance-auth` itself works from any terminal without flags, and
    /// so a helper subprocess that does not inherit them can still resolve.
    pub issuer: String,
    pub client_id: String,
    /// Collector base URL, e.g. `https://otel.ai.camer.digital`. Signal
    /// suffixes (`/v1/metrics`, `/v1/logs`, `/v1/traces`) are appended by the
    /// SDKs themselves from this base -- do not include one here.
    ///
    /// `None` when the caller has no `--otel-endpoint` -- telemetry wiring is
    /// independent of inference/gateway wiring (see `gateway_url` below), so
    /// this can't be a bare `String` without forcing every caller to invent a
    /// value when only the gateway was configured. Every writer in this
    /// module treats `None` as "skip telemetry entirely for this tool", never
    /// as an empty-string endpoint.
    pub endpoint: Option<String>,
    /// Absolute path VS Code Copilot Chat's *file* exporter is told to write,
    /// and the path `copilot push` drains. Resolved ONCE by the caller through
    /// ADR-0012's five layers, so `settings.json`'s `outfile` and the drain's
    /// default cannot disagree -- which they silently would if each side
    /// computed its own. See `crate::copilot::resolve_spool_path`.
    pub copilot_spool: PathBuf,
    /// Whether Copilot's *file* exporter should be turned on at all --
    /// distinct from `endpoint.is_some()`, which under the `daemon` profile
    /// is true (it holds the loopback substitute) even though `daemon` uses
    /// [`Self::copilot_otlp_direct`] for Copilot instead of this path.
    /// `vscode::configure`'s own doc already refuses to turn the exporter on
    /// with nowhere to push -- this is that same rule, reached by profile
    /// instead of by a missing endpoint. `false` here must retract, not just
    /// skip writing, any exporter config a prior `manual` run left behind;
    /// see `managed::plan`'s own use of this field.
    pub copilot_drain_available: bool,
    /// Whether Copilot's OWN `otlp-http` exporter should point directly at
    /// `endpoint` (issue #272 AC3) -- the `daemon` profile's Copilot path,
    /// and mutually exclusive with [`Self::copilot_drain_available`] by
    /// construction (`TelemetryWiring::resolve` never sets both). `false`
    /// here must retract this path's keys for the same reason
    /// `copilot_drain_available = false` must retract the file exporter's.
    pub copilot_otlp_direct: bool,
    /// Long-lived OTLP ingest credential, rendered into the header value both
    /// tools send verbatim. `None` writes the endpoint but no header, which
    /// is only useful against a collector that doesn't authenticate.
    pub token: Option<Redacted<String>>,
    /// Stamped onto every exported signal. Carries who this developer is, so
    /// telemetry arriving at the collector is attributable without the
    /// collector having to resolve the OTLP credential back to a person.
    pub resource_attributes: BTreeMap<String, String>,
    /// Command Claude Code re-invokes for fresh OTLP headers
    /// (`otelHeadersHelper`). When set, telemetry auth is self-renewing and
    /// the static `OTEL_EXPORTER_OTLP_HEADERS` is not written for that
    /// client -- the two would fight, and a stale static value silently
    /// winning is exactly the failure this replaces.
    pub headers_helper: Option<String>,
    /// How often Claude Code re-runs the helper. Its own default is 29
    /// MINUTES, which is far longer than a Keycloak access token lives
    /// (300s) -- leaving it alone would mean exporting with an expired token
    /// for most of every half-hour, silently. This must stay below the
    /// token lifetime.
    pub headers_helper_debounce_ms: u64,
    /// The `governance-auth … token` command Claude Code spawns through
    /// `apiKeyHelper` for a fresh inference credential. Codex receives the
    /// same logical invocation as a separate executable and argument array.
    ///
    /// Built from [`binary_path`] so both clients name the same installed
    /// binary. Codex requires that absolute path in `auth.command` and every
    /// flag in `auth.args`; combining them makes the entire string an
    /// executable filename and fails with OS error 2.
    pub token_command: String,
    /// Gateway base URL. `Some` turns on inference wiring in both writers;
    /// `None` leaves every inference key untouched, so a telemetry-only
    /// `configure` can't clobber a hand-tuned provider block.
    pub gateway_url: Option<String>,
}

impl OtelSettings {
    /// `key=value,key=value`, the W3C-ish encoding both
    /// `OTEL_RESOURCE_ATTRIBUTES` and Codex expect. `BTreeMap` (not a plain
    /// map) so the rendered string is deterministic -- an unstable ordering
    /// would make every `login` rewrite the config file with a spurious diff.
    fn resource_attributes_value(&self) -> String {
        self.resource_attributes
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join(",")
    }

    fn headers_value(&self) -> Option<String> {
        self.token
            .as_ref()
            .map(|token| format!("Authorization=Bearer {}", token.expose()))
    }

    /// `<gateway>/anthropic` -- Claude Code appends `/v1/messages` itself.
    fn anthropic_base_url(&self) -> Option<String> {
        self.gateway_url
            .as_ref()
            .map(|base| format!("{}/anthropic", base.trim_end_matches('/')))
    }

    /// `<gateway>/v1` -- the OpenAI-compatible base Codex appends to.
    fn openai_base_url(&self) -> Option<String> {
        self.gateway_url
            .as_ref()
            .map(|base| format!("{}/v1", base.trim_end_matches('/')))
    }
}

/// Absolute path to the running binary, for any command string written into
/// another tool's config. Falls back to the bare name only when the path is
/// genuinely unavailable -- see [`OtelSettings::token_command`] for what a
/// bare name costs on Codex.
pub fn binary_path() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.to_str().map(str::to_owned))
        .unwrap_or_else(|| "governance-auth".to_owned())
}

/// Pulls `sub`/`email` out of a JWT access token's payload for use as OTLP
/// resource attributes, so exported telemetry is attributable to a person.
///
/// **Deliberately does not verify the signature**, and must not be used for
/// any authorization decision. This token came from the token endpoint over
/// TLS moments ago and is only being read to label this machine's own
/// outgoing telemetry; the collector re-derives trusted identity itself and
/// never trusts these attributes (RFC-0002's trust boundary: tenant context
/// comes from the authenticated credential, never from the payload body).
/// Returns whatever it can parse -- a token shaped differently, or one that
/// isn't a JWT at all, yields no attributes rather than an error, because
/// failing `login` over a cosmetic label would be the wrong trade.
pub fn identity_attributes(access_token: &str) -> BTreeMap<String, String> {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

    let mut attributes = BTreeMap::new();
    let Some(payload) = access_token.split('.').nth(1) else {
        return attributes;
    };
    let Ok(decoded) = URL_SAFE_NO_PAD.decode(payload) else {
        return attributes;
    };
    let Ok(claims) = serde_json::from_slice::<serde_json::Value>(&decoded) else {
        return attributes;
    };

    for (claim, attribute) in [
        ("sub", "user.id"),
        ("email", "user.email"),
        ("preferred_username", "user.name"),
    ] {
        if let Some(value) = claims.get(claim).and_then(serde_json::Value::as_str)
            && !value.is_empty()
        {
            attributes.insert(attribute.to_owned(), value.to_owned());
        }
    }
    attributes
}

/// Where a tool's config lives, and whether it was actually updated. Returned
/// (rather than logged in place) so `login` can tell the developer exactly
/// which files it touched -- silently editing someone's dotfiles is worse
/// than not editing them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Written(PathBuf),
    /// The tool isn't installed here (its config directory doesn't exist).
    /// Not an error: most developers have one of the two, not both.
    Skipped(PathBuf),
    /// The developer passed this client's `--no-…` flag. Distinct from
    /// `Skipped` because the two are different facts about their machine, and
    /// one line of output that conflates them is a line nobody can act on:
    /// "not present" is a tool to install, "left alone" is a choice they made.
    Declined {
        path: PathBuf,
        flag: &'static str,
    },
}

impl Outcome {
    /// Prints one line per outcome, and reports whether Codex's `config.toml`
    /// was among the files written.
    ///
    /// Every outcome gets a line: silently editing someone's dotfiles is worse
    /// than not editing them. The three read differently on purpose --
    /// `Configured:` is a file that changed, `Skipped:` is a tool they could
    /// install, `Left alone:` is a choice they made and nothing to act on.
    ///
    /// The return value is that narrow on purpose. Codex is the ONLY client
    /// without a dynamic-headers hook: Claude Code refreshes through
    /// `otelHeadersHelper`, and VS Code Copilot no longer exports for itself at
    /// all -- it writes a file that `copilot push` ships with a bearer it
    /// refreshes. So the missing-credential warning its caller prints is about
    /// exactly one file, and naming the others would be crying wolf.
    pub fn report(outcomes: &[Self]) -> bool {
        let mut wrote_codex_config = false;
        for outcome in outcomes {
            match outcome {
                Self::Written(path) => {
                    eprintln!("Configured: {}", path.display());
                    wrote_codex_config |=
                        path.file_name().is_some_and(|name| name == "config.toml");
                }
                Self::Skipped(dir) => eprintln!("Skipped: {} not present.", dir.display()),
                Self::Declined { path, flag } => {
                    eprintln!("Left alone ({flag}): {}", path.display());
                }
            }
        }
        wrote_codex_config
    }
}

/// Configures every supported tool found on this machine, except those
/// [`crate::optout`] names. A tool whose config directory is absent is skipped,
/// not created -- creating `~/.codex` for someone who doesn't use Codex would
/// be surprising, and an empty config directory changes how some tools behave
/// on first run.
pub fn configure_all(
    home: &Path,
    settings: &OtelSettings,
    optout: crate::optout::ClientOptOut,
) -> Result<Vec<Outcome>> {
    let previous = crate::managed::load(&crate::managed::manifest_path(home));
    let codex_settings = if optout.codex_telemetry_only {
        OtelSettings {
            gateway_url: None,
            ..settings.clone()
        }
    } else {
        settings.clone()
    };

    let mut outcomes = vec![
        if optout.claude {
            Outcome::Declined {
                path: home.join(".claude"),
                flag: "--no-claude",
            }
        } else {
            configure_claude_code(home, settings)?
        },
        if optout.codex {
            Outcome::Declined {
                path: home.join(".codex"),
                flag: "--no-codex",
            }
        } else {
            configure_codex(home, &codex_settings)?
        },
    ];
    if optout.vscode {
        outcomes.push(Outcome::Declined {
            path: crate::vscode::user_dir(home, "Code"),
            flag: "--no-vscode",
        });
    } else {
        outcomes.extend(crate::vscode::configure(home, settings)?);
    }
    outcomes.extend(configure_shell_env(home, settings)?);

    // Retract anything we wrote last time and did not write now, then record
    // what we own for next time. Non-fatal by design: a failure here leaves a
    // stale key, which is what happens today anyway -- it must never undo a
    // successful configure. See `managed`.
    let now = crate::managed::plan(home, settings, optout, &previous);
    match crate::managed::retract_stale(&previous, &now) {
        Ok(removed) => {
            for entry in removed {
                eprintln!("Removed (no longer managed): {entry}");
            }
        }
        Err(error) => eprintln!("warning: could not retract stale config keys: {error:#}"),
    }
    let manifest = crate::managed::Manifest {
        version: 1,
        targets: now,
    };
    if let Err(error) = crate::managed::save(&crate::managed::manifest_path(home), &manifest) {
        eprintln!("warning: could not record managed keys: {error:#}");
    }

    Ok(outcomes)
}

/// Marker pair delimiting the block this binary owns in a shell rc file.
/// Everything between them is replaced wholesale on each run; everything
/// outside is never touched. Without markers the only idempotent options are
/// "append every time" (the block accumulates forever) or "rewrite the file"
/// (destroys the developer's own config).
const BLOCK_BEGIN: &str = "# >>> governance-auth otel (managed) >>>";
const BLOCK_END: &str = "# <<< governance-auth otel (managed) <<<";

/// POSIX rc files, then fish (different syntax, different path).
const POSIX_RC_FILES: [&str; 4] = [".bashrc", ".zshrc", ".profile", ".bash_profile"];

/// Every variable placed in the developer's shell, in a stable order.
///
/// ⚠️ **Nothing OTLP goes here, deliberately — see [`configure_shell_env`].**
/// Every client this binary configures has its own file for telemetry, and the
/// generic `OTEL_*` variables are machine-global: one shared value where each
/// client needs a different one.
///
/// What is left is genuinely global to this machine.
/// `GOVERNANCE_AUTH_ISSUER`/`_CLIENT_ID` are here so the binary itself works
/// from any terminal with no flags, which is the other half of what `login`
/// persisting its settings buys (see `config_persist`): the file covers this
/// binary, the environment covers everything that shells out to it.
/// `ANTHROPIC_BASE_URL` names the gateway, of which there is exactly one per
/// org — it is inference wiring, not telemetry, and Claude Code reads it from
/// `settings.json` anyway (see [`claude_code_env`]).
fn shell_exports(settings: &OtelSettings) -> Vec<(&'static str, String)> {
    let mut exports = vec![
        ("GOVERNANCE_AUTH_ISSUER", settings.issuer.clone()),
        ("GOVERNANCE_AUTH_CLIENT_ID", settings.client_id.clone()),
    ];
    if let Some(base_url) = settings.anthropic_base_url() {
        exports.push(("ANTHROPIC_BASE_URL", base_url));
    }
    exports
}

/// Places this binary's own settings, and the gateway URL, in the developer's
/// shell so any terminal (and any subprocess that does not inherit them) can
/// resolve them without flags.
///
/// ## Why no OTLP configuration is written here
///
/// **One collector per audience, so the endpoint is per-CLIENT.** Each
/// collector's OIDC gate accepts exactly one `aud`: `otel.ai.camer.digital`
/// takes `governance-auth-cli`, `otel-opencode.ai.camer.digital` takes
/// `opencode-cli`. `OTEL_EXPORTER_OTLP_ENDPOINT` (and `_PROTOCOL`,
/// `_HEADERS`, `OTEL_METRICS_EXPORTER`, `OTEL_LOGS_EXPORTER`,
/// `OTEL_RESOURCE_ATTRIBUTES`) are **generic OpenTelemetry variables**: once
/// sourced from an rc file they apply to every OTLP exporter started on that
/// machine, and SDKs read the environment *ahead of* their own configured
/// default. So exporting one client's endpoint machine-wide makes every other
/// client's correct default unreachable — measured 2026-09-02, when OpenCode
/// (`@vymalo/opencode-otel`, `env.OTEL_EXPORTER_OTLP_ENDPOINT ||
/// opts.endpoint`) silently exported to the Claude Code collector and 401'd on
/// every span. There is no machine-wide correct value, so there is no
/// machine-wide variable.
///
/// Every client is reached through its own file instead, and none of them
/// needs the environment: Claude Code via `~/.claude/settings.json`
/// ([`claude_code_env`], which also covers a desktop-icon launch with no shell
/// to inherit from), Codex via `[otel]` in `~/.codex/config.toml`
/// ([`configure_codex`] writes `endpoint`, `protocol` and `headers` — its
/// `OtelExporterKind::OtlpHttp` takes `endpoint` as a required field, so the
/// file is authoritative), and VS Code Copilot via its `file` exporter
/// ([`crate::vscode`]) drained out of band.
///
/// ## Why the file is still 0600 and still indirected
///
/// It no longer carries a credential — that removal is the point — but `.bashrc`
/// is routinely mode 0644 and routinely committed to a dotfiles repo, and the
/// rc block being a one-line `source` of `~/.config/governance-auth/otel.env`
/// is what keeps it that way for whatever lands here next.
pub fn configure_shell_env(home: &Path, settings: &OtelSettings) -> Result<Vec<Outcome>> {
    let exports = shell_exports(settings);
    if exports.is_empty() {
        // Nothing to place, and an rc block that exports nothing is just noise
        // in someone's shell startup.
        return Ok(Vec::new());
    }

    let env_dir = home.join(".config").join("governance-auth");
    fs::create_dir_all(&env_dir).with_context(|| format!("creating {}", env_dir.display()))?;

    let posix_env = env_dir.join("otel.env");
    let posix =
        crate::templates::shell_env_sh(&exports).context("rendering the POSIX shell env file")?;
    write_atomically(&posix_env, posix.as_bytes())?;

    let fish_env = env_dir.join("otel.fish");
    let fish =
        crate::templates::shell_env_fish(&exports).context("rendering the fish shell env file")?;
    write_atomically(&fish_env, fish.as_bytes())?;

    let mut outcomes = vec![
        Outcome::Written(posix_env.clone()),
        Outcome::Written(fish_env.clone()),
    ];

    for rc in POSIX_RC_FILES {
        let path = home.join(rc);
        // Only existing rc files are edited. Creating a `.zshrc` for someone
        // who doesn't run zsh changes which startup path their shell takes.
        if !path.is_file() {
            continue;
        }
        let line = format!(
            "[ -f \"{}\" ] && . \"{}\"",
            display_with_home(&posix_env),
            display_with_home(&posix_env)
        );
        upsert_block(&path, &line)?;
        outcomes.push(Outcome::Written(path));
    }

    let fish_rc = home.join(".config").join("fish").join("config.fish");
    if fish_rc.is_file() {
        let line = format!(
            "test -f \"{}\"; and source \"{}\"",
            display_with_home(&fish_env),
            display_with_home(&fish_env)
        );
        upsert_block(&fish_rc, &line)?;
        outcomes.push(Outcome::Written(fish_rc));
    }

    Ok(outcomes)
}

/// Renders an absolute path under the home directory as `$HOME/...` so the
/// line written into an rc file stays correct if that file is shared between
/// machines with different usernames -- a real pattern for dotfiles repos.
fn display_with_home(path: &Path) -> String {
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && path.starts_with(&home) => {
            path.strip_prefix(&home).map_or_else(
                |_| path.display().to_string(),
                |rest| format!("$HOME/{}", rest.display()),
            )
        }
        _ => path.display().to_string(),
    }
}

/// Replaces the managed block in `path`, or appends one if absent. Everything
/// outside the markers is preserved byte-for-byte.
fn upsert_block(path: &Path, body: &str) -> Result<()> {
    let existing = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };

    let block = format!("{BLOCK_BEGIN}\n{body}\n{BLOCK_END}");

    let updated = match (existing.find(BLOCK_BEGIN), existing.find(BLOCK_END)) {
        (Some(start), Some(end)) if end > start => {
            let mut out = String::with_capacity(existing.len() + block.len());
            out.push_str(existing.get(..start).unwrap_or_default());
            out.push_str(&block);
            out.push_str(
                existing
                    .get(end.saturating_add(BLOCK_END.len())..)
                    .unwrap_or_default(),
            );
            out
        }
        // A damaged block -- one marker only, or END before BEGIN (both
        // reachable by hand-editing) -- is left alone rather than guessed at.
        // Appending would give the file two BEGINs and make every later run
        // ambiguous; rewriting could swallow the developer's own lines.
        (Some(_), None) | (None, Some(_)) | (Some(_), Some(_)) => {
            anyhow::bail!(
                "{} contains only one of the governance-auth markers, or they are out of order; \
                 refusing to guess where the managed block ends. Remove the stray marker and \
                 re-run.",
                path.display()
            )
        }
        (None, None) => {
            let mut out = existing;
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push('\n');
            out.push_str(&block);
            out.push('\n');
            out
        }
    };

    // Not `write_atomically`: an rc file's existing mode is the developer's
    // business (and 0600 on a `.profile` would be a surprising side effect).
    // This file carries no secret -- only a `source` line -- precisely so it
    // doesn't need locking down.
    let tmp = path.with_extension("governance-auth-tmp");
    fs::write(&tmp, updated.as_bytes()).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} to {}", tmp.display(), path.display()))?;
    Ok(())
}

/// Claude Code: `~/.claude/settings.json`, `env` block. Key names are taken
/// verbatim from the "Administrator Configuration" section of
/// <https://code.claude.com/docs/en/monitoring-usage>.
///
/// `http/protobuf`, not `grpc`: the collector is reached through a public
/// HTTPS ingress here, and the generic `OTEL_EXPORTER_OTLP_ENDPOINT` with an
/// HTTP protocol is the combination that works through one without per-signal
/// port juggling.
pub fn configure_claude_code(home: &Path, settings: &OtelSettings) -> Result<Outcome> {
    let dir = home.join(".claude");
    if !dir.is_dir() {
        return Ok(Outcome::Skipped(dir));
    }
    let path = dir.join("settings.json");

    let mut root: serde_json::Value = match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing existing {}", path.display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            serde_json::Value::Object(serde_json::Map::new())
        }
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };

    let object = root
        .as_object_mut()
        .with_context(|| format!("{} is not a JSON object", path.display()))?;

    // `otelHeadersHelper` -- Claude Code re-invokes this on an interval and
    // uses whatever JSON headers it prints, so telemetry auth refreshes
    // itself instead of depending on anyone rotating a long-lived key by
    // hand. This is the one client that can do it; see `headers_value`'s
    // callers for the others.
    if let Some(helper) = &settings.headers_helper {
        object.insert(
            "otelHeadersHelper".to_owned(),
            serde_json::Value::String(helper.clone()),
        );
    }

    // `apiKeyHelper` -- the INFERENCE credential, distinct from the telemetry
    // one above. Only written alongside `ANTHROPIC_BASE_URL`: pointing Claude
    // Code's API key at this gateway's tokens while it still talks to
    // api.anthropic.com would send a Keycloak token to Anthropic, so the two
    // keys move together or not at all.
    if let Some(base_url) = settings.anthropic_base_url() {
        object.insert(
            "apiKeyHelper".to_owned(),
            serde_json::Value::String(settings.token_command.clone()),
        );
        let env = object
            .entry("env")
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()))
            .as_object_mut()
            .with_context(|| format!("`env` in {} is not a JSON object", path.display()))?;
        env.insert(
            "ANTHROPIC_BASE_URL".to_owned(),
            serde_json::Value::String(base_url),
        );
    }

    let env = object
        .entry("env")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()))
        .as_object_mut()
        .with_context(|| format!("`env` in {} is not a JSON object", path.display()))?;

    // Remove the static header FIRST when a helper is in play. Only adding
    // keys would leave a stale `OTEL_EXPORTER_OTLP_HEADERS` from an earlier
    // run sitting next to the refreshing helper -- the exact silent failure
    // the helper exists to remove, and one that survives every subsequent
    // `configure`. Observed on a real machine before this line existed.
    if settings.headers_helper.is_some() {
        env.remove("OTEL_EXPORTER_OTLP_HEADERS");
    }

    for (key, value) in claude_code_env(settings) {
        env.insert(key.to_owned(), serde_json::Value::String(value));
    }

    let mut bytes = serde_json::to_vec_pretty(&root).context("serializing settings.json")?;
    bytes.push(b'\n');
    write_atomically(&path, &bytes)?;
    Ok(Outcome::Written(path))
}

/// The exact `env` entries this module owns in `settings.json` -- so "which
/// keys do we touch" has one answer, and the test can assert the full set.
pub(crate) fn claude_code_env(settings: &OtelSettings) -> Vec<(&'static str, String)> {
    let mut entries = vec![
        // `apiKeyHelper` output is cached for FIVE MINUTES by default -- the
        // exact lifetime of a Keycloak access token here, so the cache can
        // hand Claude Code a token that expired moments ago and the request
        // 401s. Claude Code re-runs the helper on a 401, so this self-heals,
        // but only after a failed request; keeping the TTL under the token
        // lifetime avoids the failure instead of recovering from it.
        //
        // Unconditional (not gated on `gateway_url`) to match this key's
        // pre-existing behaviour: harmless when `apiKeyHelper` itself isn't
        // written, and not part of the bug this module fixes (that bug was
        // `apiKeyHelper` never being reached at all when only the OTEL
        // endpoint was unset -- see `oauth::apply_telemetry`).
        (
            "CLAUDE_CODE_API_KEY_HELPER_TTL_MS",
            settings.headers_helper_debounce_ms.to_string(),
        ),
        // This gateway serves model names Claude Code doesn't ship in its
        // built-in list (adorsys-coder, minimax-m3, ...), so without
        // discovery they never appear in the `/model` picker at all.
        //
        // It does NOT silence the "not a model this version recognizes"
        // warning -- checked live, the warning still prints with discovery
        // on, because that one is about the assumed 200k context window and
        // is only fixed by `modelOverrides` or CLAUDE_CODE_MAX_CONTEXT_TOKENS.
        // Setting either would mean hard-coding each gateway model's real
        // window here, which this binary has no way to know and which would
        // silently rot as models change. Left to the values repo, where the
        // model list already lives.
        ("CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY", "1".to_owned()),
    ];

    // Everything below is genuinely telemetry-only: without an OTEL endpoint
    // there is no collector to export to, so none of these keys should be
    // written -- the other half of the bug this module fixes (the first half
    // was `apply_telemetry` bailing out before reaching here; this half is
    // `settings.endpoint` no longer silently being any `String` when absent).
    let Some(endpoint) = &settings.endpoint else {
        return entries;
    };

    entries.push(("CLAUDE_CODE_ENABLE_TELEMETRY", "1".to_owned()));
    entries.push(("OTEL_METRICS_EXPORTER", "otlp".to_owned()));
    entries.push(("OTEL_LOGS_EXPORTER", "otlp".to_owned()));
    entries.push(("OTEL_EXPORTER_OTLP_PROTOCOL", "http/protobuf".to_owned()));
    entries.push(("OTEL_EXPORTER_OTLP_ENDPOINT", endpoint.clone()));
    entries.push((
        "OTEL_RESOURCE_ATTRIBUTES",
        settings.resource_attributes_value(),
    ));
    // Off by default in Claude Code -- see files.md's "Resource attributes".
    entries.push(("OTEL_METRICS_INCLUDE_ENTRYPOINT", "1".to_owned()));

    match (&settings.headers_helper, settings.headers_value()) {
        // The helper wins outright when present: a stale static header
        // sitting alongside a refreshing one is the exact silent-failure
        // mode this whole mechanism exists to remove.
        (Some(_), _) => {
            entries.push((
                "CLAUDE_CODE_OTEL_HEADERS_HELPER_DEBOUNCE_MS",
                settings.headers_helper_debounce_ms.to_string(),
            ));
        }
        (None, Some(headers)) => entries.push(("OTEL_EXPORTER_OTLP_HEADERS", headers)),
        (None, None) => {}
    }
    entries
}

/// Codex: `~/.codex/config.toml`, `[otel]` table. Key names from
/// <https://learn.chatgpt.com/docs/config-file/config-reference>.
///
/// Edited through `toml_edit` rather than parse-and-reserialize so the
/// developer's existing comments and key order survive -- this file is
/// hand-maintained.
pub fn configure_codex(home: &Path, settings: &OtelSettings) -> Result<Outcome> {
    let dir = home.join(".codex");
    if !dir.is_dir() {
        return Ok(Outcome::Skipped(dir));
    }
    let path = dir.join("config.toml");

    let existing = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };

    let mut document = existing
        .parse::<toml_edit::DocumentMut>()
        .with_context(|| format!("parsing existing {}", path.display()))?;

    // `[otel]` is genuinely telemetry-only: without an OTEL endpoint there is
    // no collector to point it at, and the `model_providers` block below
    // (inference) must not depend on it -- that's the bug this branch fixes.
    if let Some(endpoint) = &settings.endpoint {
        let otel = table_entry(document.as_table_mut(), "otel")?;
        otel.insert("environment", toml_edit::value("prod"));
        // Content capture stays off. The collector's own redaction is the
        // authoritative control (RFC-0002 treats that as a release blocker,
        // not an enhancement), but a client that never sends raw prompts in
        // the first place is one fewer place for them to leak.
        otel.insert("log_user_prompt", toml_edit::value(false));

        // `otel.exporter` is a TAGGED ENUM, not a string: the exporter kind is
        // the table NAME and its settings are that table's contents. Writing
        // `exporter = "otlp-http"` with the settings in a sibling table parses
        // as TOML but Codex rejects it at load time with `invalid type: unit
        // variant, expected struct variant in otel.exporter` -- and Codex
        // refuses to start at all on a config it can't load, so getting this
        // wrong bricks the tool rather than just disabling telemetry. The
        // shape below was confirmed by loading it in codex-cli 0.146.1, not
        // inferred from the reference docs (which describe it as
        // `otel.exporter.<id>.endpoint`).
        for kind in ["exporter", "metrics_exporter"] {
            let exporter = table_entry(otel, kind)?;
            let otlp = table_entry(exporter, "otlp-http")?;
            otlp.insert("endpoint", toml_edit::value(endpoint));
            otlp.insert("protocol", toml_edit::value("binary"));
            if let Some(token) = &settings.token {
                let headers = table_entry(otlp, "headers")?;
                headers.insert(
                    "Authorization",
                    toml_edit::value(format!("Bearer {}", token.expose())),
                );
            }
        }
    }

    if let Some(base_url) = settings.openai_base_url() {
        // Take over the default. Writing the provider block alone leaves Codex
        // pointed at whatever it used before, so the wiring existed and did
        // nothing -- this key is what selects it. Set here only because
        // `model_providers` is borrowed below; placement in the output is
        // `toml_edit`'s job, see `set_root_scalar`.
        //
        // Deliberately authoritative: it overwrites an existing value rather
        // than deferring to it. Someone who wants another provider for a
        // session has `--config model_provider=...`; someone still talking to
        // api.openai.com while believing they are on the gateway gets no
        // signal at all, and that is the failure this prevents.
        set_root_scalar(
            document.as_table_mut(),
            "model_provider",
            toml_edit::value(CODEX_PROVIDER_ID),
        );

        let providers = table_entry(document.as_table_mut(), "model_providers")?;
        let provider = table_entry(providers, CODEX_PROVIDER_ID)?;
        provider.insert("name", toml_edit::value(CODEX_PROVIDER_ID));
        provider.insert("base_url", toml_edit::value(&base_url));
        // The ONLY value codex-cli 0.146.1 accepts: `wire_api = "chat"` is
        // rejected outright at config load ("no longer supported"), so there
        // is no shape of this block that reaches a chat-completions gateway.
        provider.insert("wire_api", toml_edit::value("responses"));
        provider.decor_mut().set_prefix(
            crate::templates::codex_provider_banner().context("rendering the Codex banner")?,
        );

        let auth = table_entry(provider, "auth")?;
        // Codex passes `command` directly to the OS. Arguments belong in its
        // separate array; putting the whole command line here asks the OS to
        // find one executable whose filename contains every flag and value.
        auth.insert("command", toml_edit::value(binary_path()));
        let args: toml_edit::Array = crate::cli::token_args(&settings.issuer, &settings.client_id)
            .into_iter()
            .collect();
        auth.insert("args", toml_edit::value(args));
        auth.insert(
            "refresh_interval_ms",
            toml_edit::value(i64::try_from(settings.headers_helper_debounce_ms).unwrap_or(240_000)),
        );
    }

    let mut bytes = document.to_string().into_bytes();
    if !bytes.ends_with(b"\n") {
        bytes.push(b'\n');
    }
    write_atomically(&path, &bytes)?;
    Ok(Outcome::Written(path))
}

/// Provider id `governance-auth` owns in `config.toml`. A stable constant so
/// re-running `configure` updates the same block instead of accumulating one
/// per run; any differently-named provider a developer wrote by hand is left
/// strictly alone.
/// Sets a top-level scalar, replacing in place so an existing key keeps its
/// comment (see `config_persist::set` for why `Table::insert` alone loses it).
///
/// A bare TOML key must precede the first table header or it belongs to that
/// table instead -- but `toml_edit` handles this for us: it emits root scalars
/// ahead of tables no matter when they were inserted. Checked by moving this
/// call after `[model_providers]` was built and confirming the output was still
/// a root key, so the call site's ordering is a borrow-checker constraint, not
/// a correctness one. `codex_default_provider_is_a_root_key` pins the result
/// regardless, because it is what Codex actually reads.
fn set_root_scalar(table: &mut toml_edit::Table, key: &str, item: toml_edit::Item) {
    match table.get_mut(key) {
        Some(slot) => *slot = item,
        None => {
            table.insert(key, item);
        }
    }
}

pub(crate) const CODEX_PROVIDER_ID: &str = "governance";

/// `table[key]` on a `toml_edit` table panics when the key exists but holds a
/// non-table (a developer who wrote `otel = "something"` by hand), and
/// `indexing_slicing` is denied in this workspace for exactly that reason.
/// This is the non-panicking equivalent: auto-vivify a table, or report which
/// key is the wrong shape rather than taking the process down.
fn table_entry<'a>(table: &'a mut toml_edit::Table, key: &str) -> Result<&'a mut toml_edit::Table> {
    table
        .entry(key)
        .or_insert(toml_edit::table())
        .as_table_mut()
        .with_context(|| format!("`{key}` already exists in config.toml but is not a table"))
}

/// tmp-then-rename at mode 0600. Claude Code's and Codex's files can carry the
/// OTLP bearer token, so they get the same treatment as the session cache --
/// and an interrupted write must never leave a half-file behind, since Codex
/// refuses to start on a malformed config rather than degrading. VS Code's
/// (`crate::vscode`) carries no credential since the file-exporter cutover and
/// is written through here anyway: one writer, one set of guarantees.
pub(crate) fn write_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("governance-auth-tmp");
    write_private_file(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} to {}", tmp.display(), path.display()))?;
    Ok(())
}

#[cfg(unix)]
fn write_private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};

    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    fs::write(path, bytes)?;
    Ok(())
}

#[cfg(test)]
mod client_scope_tests;
#[cfg(test)]
mod tests;
