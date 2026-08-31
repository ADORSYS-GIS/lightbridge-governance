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
    /// The `governance-auth … token` command clients spawn for a fresh
    /// INFERENCE credential (Claude Code's `apiKeyHelper`, Codex's
    /// `[model_providers.*.auth] command`).
    ///
    /// ⚠️ MUST be an absolute path. Codex spawns this itself rather than
    /// through a shell, so it does NOT get the login shell's `PATH` -- a bare
    /// `governance-auth` fails with `No such file or directory (os error 2)`
    /// and the provider silently falls back to unauthenticated. Measured live
    /// against codex-cli 0.146.1 with the binary in `~/.local/bin`. Claude
    /// Code happens to resolve a bare name (it goes through a shell), so this
    /// trap only shows up on one of the two clients -- which is exactly why
    /// both are built from [`binary_path`] rather than a literal.
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
}

/// Configures every supported tool found on this machine. A tool whose config
/// directory is absent is skipped, not created -- creating `~/.codex` for
/// someone who doesn't use Codex would be surprising, and an empty config
/// directory changes how some tools behave on first run.
pub fn configure_all(home: &Path, settings: &OtelSettings) -> Result<Vec<Outcome>> {
    let mut outcomes = vec![
        configure_claude_code(home, settings)?,
        configure_codex(home, settings)?,
    ];
    outcomes.extend(configure_vscode(home, settings)?);
    outcomes.extend(configure_shell_env(home, settings)?);
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
/// ⚠️ This is the ONLY channel some clients have. VS Code Copilot has no
/// settings key for OTLP headers, and Codex reads OTEL configuration from the
/// environment rather than from its own config file -- so a value that exists
/// only in Claude Code's `settings.json` reaches exactly one of the three
/// tools. Claude Code gets these through `settings.json` too (see
/// [`claude_code_env`]); the duplication is deliberate, because a developer who
/// launches Claude Code from a desktop icon has no shell to inherit from.
///
/// `GOVERNANCE_AUTH_ISSUER`/`_CLIENT_ID` are here so the binary itself works
/// from any terminal with no flags, which is the other half of what `login`
/// persisting its settings buys (see `config_persist`): the file covers this
/// binary, the environment covers everything that shells out to it.
fn shell_exports(settings: &OtelSettings) -> Vec<(&'static str, String)> {
    let mut exports = vec![
        ("GOVERNANCE_AUTH_ISSUER", settings.issuer.clone()),
        ("GOVERNANCE_AUTH_CLIENT_ID", settings.client_id.clone()),
    ];
    if let Some(base_url) = settings.anthropic_base_url() {
        exports.push(("ANTHROPIC_BASE_URL", base_url));
    }
    if let Some(endpoint) = &settings.endpoint {
        exports.push(("OTEL_EXPORTER_OTLP_ENDPOINT", endpoint.clone()));
        exports.push(("OTEL_EXPORTER_OTLP_PROTOCOL", "http/protobuf".to_owned()));
        exports.push(("OTEL_METRICS_EXPORTER", "otlp".to_owned()));
        exports.push(("OTEL_LOGS_EXPORTER", "otlp".to_owned()));
        exports.push((
            "OTEL_RESOURCE_ATTRIBUTES",
            settings.resource_attributes_value(),
        ));
    }
    // Last, and only with a collector to send it to: an exported Authorization
    // header with no endpoint authenticates to nothing.
    if settings.endpoint.is_some()
        && let Some(headers) = settings.headers_value()
    {
        exports.push(("OTEL_EXPORTER_OTLP_HEADERS", headers));
    }
    exports
}

/// Exports the OTLP credential into the developer's shell, which is the only
/// way VS Code Copilot can be authenticated -- it has no settings key for
/// OTLP headers (see [`configure_vscode`]).
///
/// The token does **not** go into `.bashrc`. Those files are routinely mode
/// 0644 and routinely committed to a dotfiles repo, so writing a bearer token
/// there is how a credential ends up in someone's public GitHub. Instead the
/// secret lives in `~/.config/governance-auth/otel.env` at 0600 and the rc
/// block is a one-line `source` of it -- so a committed `.bashrc` leaks
/// nothing.
///
/// ⚠️ `OTEL_EXPORTER_OTLP_HEADERS` is a **global** OpenTelemetry variable, not
/// a Copilot-scoped one. Once exported, every OTLP exporter started from that
/// shell attaches this Authorization header to whatever collector it targets.
/// VS Code offers no scoped alternative (its `COPILOT_OTEL_*` variables cover
/// endpoint/protocol/capture, but not headers), so this is inherent to
/// authenticating Copilot at all, not a choice made here. Callers should say
/// so out loud.
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

/// Every VS Code flavour whose user-settings directory this understands.
/// Insiders and VSCodium keep entirely separate settings trees, so a
/// developer running one of those gets nothing if only stable `Code` is
/// considered -- and the failure is silent, which is the worst kind.
const VSCODE_FLAVOURS: [&str; 3] = ["Code", "Code - Insiders", "VSCodium"];

/// GitHub Copilot in VS Code, via each flavour's `User/settings.json`.
/// Setting names are verbatim from
/// <https://code.visualstudio.com/docs/agents/guides/monitoring-agents>.
///
/// ⚠️ **There is no VS Code setting for OTLP headers.** The documented
/// surface exposes the endpoint, protocol and content-capture as settings,
/// but authentication *only* through the `OTEL_EXPORTER_OTLP_HEADERS`
/// environment variable, which has to be present in the environment VS Code
/// itself was launched from -- a `settings.json` key can't supply it, and
/// neither can this binary. Against an authenticating collector that means
/// Copilot telemetry is rejected until that variable is exported. The caller
/// surfaces this rather than writing a config that looks complete and
/// silently drops every span.
pub fn configure_vscode(home: &Path, settings: &OtelSettings) -> Result<Vec<Outcome>> {
    // VS Code Copilot's OTEL surface is telemetry-only -- there is no
    // gateway/inference setting this writer could touch instead (see the
    // module doc's "no setting for OTLP headers" note). With no endpoint
    // there is nothing meaningful to write, so this is a quiet no-op rather
    // than a `Skipped` about a tool that may not even be installed here.
    let Some(endpoint) = settings.endpoint.as_deref() else {
        return Ok(Vec::new());
    };

    let mut outcomes = Vec::new();
    for flavour in VSCODE_FLAVOURS {
        let dir = vscode_user_dir(home, flavour);
        if !dir.is_dir() {
            continue;
        }
        outcomes.push(configure_vscode_flavour(&dir, endpoint)?);
    }
    if outcomes.is_empty() {
        outcomes.push(Outcome::Skipped(vscode_user_dir(home, VSCODE_FLAVOURS[0])));
    }
    Ok(outcomes)
}

/// `~/.config/<flavour>/User` on Linux, `~/Library/Application
/// Support/<flavour>/User` on macOS -- VS Code does not follow
/// `XDG_CONFIG_HOME` on macOS.
fn vscode_user_dir(home: &Path, flavour: &str) -> PathBuf {
    let base = if cfg!(target_os = "macos") {
        home.join("Library").join("Application Support")
    } else {
        home.join(".config")
    };
    base.join(flavour).join("User")
}

fn configure_vscode_flavour(user_dir: &Path, endpoint: &str) -> Result<Outcome> {
    let path = user_dir.join("settings.json");

    let existing = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };

    // VS Code's settings.json is JSONC -- comments and trailing commas are
    // legal there and developers really do use them. `serde_json` can't parse
    // that, and the tempting fixes are both destructive: stripping comments
    // to parse then writing plain JSON back deletes them permanently. So a
    // file this can't parse losslessly is REFUSED, with the exact settings
    // printed for the developer to paste. Declining to edit beats silently
    // eating someone's annotated config.
    let mut root: serde_json::Value = if existing.trim().is_empty() {
        serde_json::Value::Object(serde_json::Map::new())
    } else {
        serde_json::from_str(&existing).with_context(|| {
            format!(
                "{} is not plain JSON (VS Code allows JSONC comments/trailing commas, which \
                 cannot be rewritten without discarding them). Leaving it untouched -- add \
                 these settings by hand:\n{}",
                path.display(),
                vscode_settings_hint(endpoint)
            )
        })?
    };

    let object = root
        .as_object_mut()
        .with_context(|| format!("{} is not a JSON object", path.display()))?;

    for (key, value) in vscode_settings(endpoint) {
        object.insert(key.to_owned(), value);
    }

    let mut bytes = serde_json::to_vec_pretty(&root).context("serializing VS Code settings")?;
    bytes.push(b'\n');
    write_atomically(&path, &bytes)?;
    Ok(Outcome::Written(path))
}

/// The exact `settings.json` entries this module owns, in one place so the
/// "what do we touch" question and the paste-this-by-hand fallback can't
/// drift apart.
///
/// `captureContent` is pinned to `false`: the collector's redaction is the
/// authoritative control, but a client that never sends prompts is one fewer
/// place they can leak -- matching the `log_user_prompt = false` choice on
/// the Codex side.
fn vscode_settings(endpoint: &str) -> Vec<(&'static str, serde_json::Value)> {
    vec![
        (
            "github.copilot.chat.otel.enabled",
            serde_json::Value::Bool(true),
        ),
        (
            "github.copilot.chat.otel.exporterType",
            serde_json::Value::String("otlp-http".to_owned()),
        ),
        (
            "github.copilot.chat.otel.otlpEndpoint",
            serde_json::Value::String(endpoint.to_owned()),
        ),
        (
            "github.copilot.chat.otel.captureContent",
            serde_json::Value::Bool(false),
        ),
    ]
}

/// Rendered into the error when a JSONC `settings.json` can't be rewritten
/// losslessly, so declining to edit still leaves the developer with
/// everything they need to do it themselves.
fn vscode_settings_hint(endpoint: &str) -> String {
    vscode_settings(endpoint)
        .into_iter()
        .map(|(key, value)| format!("  \"{key}\": {value},"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether Copilot telemetry will actually be accepted, given VS Code has no
/// settings key for OTLP headers. Returns the env var the developer has to
/// export themselves when a credential is in play -- there is no file this
/// binary can write that supplies it.
pub fn vscode_manual_env(settings: &OtelSettings) -> Option<String> {
    settings
        .headers_value()
        .map(|headers| format!("OTEL_EXPORTER_OTLP_HEADERS={headers}"))
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

/// The exact `env` entries this module owns in `settings.json`. Split out so
/// the test can assert the full set without re-deriving it, and so the
/// "which keys do we touch" question has one answer.
fn claude_code_env(settings: &OtelSettings) -> Vec<(&'static str, String)> {
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
    // written -- that's the other half of the bug this module fixes (the
    // first half was `apply_telemetry` bailing out before even reaching
    // here; this half is `settings.endpoint` no longer being a `String` that
    // could silently be anything when the caller has none).
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
        // Absolute path, deliberately -- see OtelSettings::token_command.
        // Codex spawns this without a shell, so a bare name cannot resolve.
        auth.insert("command", toml_edit::value(&settings.token_command));
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

const CODEX_PROVIDER_ID: &str = "governance";

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

/// tmp-then-rename at mode 0600. Both files can carry the OTLP bearer token,
/// so they get the same treatment as the session cache -- and an interrupted
/// write must never leave a half-file behind, since Codex refuses to start on
/// a malformed config rather than degrading.
fn write_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
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
mod tests {
    use super::*;

    fn settings() -> OtelSettings {
        OtelSettings {
            issuer: "https://auth.example".to_owned(),
            client_id: "cli".to_owned(),
            endpoint: Some("https://otel.example.com".to_owned()),
            token: Some(Redacted::new("ingest-token".to_owned())),
            headers_helper: None,
            headers_helper_debounce_ms: 240_000,
            resource_attributes: BTreeMap::from([
                ("user.id".to_owned(), "abc-123".to_owned()),
                ("service.name".to_owned(), "claude-code".to_owned()),
            ]),
            token_command: "/abs/path/governance-auth token".to_owned(),
            // Telemetry-only by default: the inference keys are opt-in, so
            // every pre-existing test keeps asserting the same surface.
            gateway_url: None,
        }
    }

    /// `settings()` plus the gateway, i.e. what `--gateway-url` turns on.
    fn settings_with_gateway() -> OtelSettings {
        OtelSettings {
            gateway_url: Some("https://api.example.com".to_owned()),
            ..settings()
        }
    }

    #[test]
    fn resource_attributes_render_deterministically() {
        // Unstable ordering here would make every `login` rewrite the config
        // with a spurious diff, which is how a "did it change?" check stops
        // meaning anything.
        let rendered = settings().resource_attributes_value();
        assert_eq!(rendered, "service.name=claude-code,user.id=abc-123");
    }

    #[test]
    fn claude_code_env_carries_every_key_the_docs_require() {
        let env: BTreeMap<_, _> = claude_code_env(&settings()).into_iter().collect();
        assert_eq!(
            env.get("CLAUDE_CODE_ENABLE_TELEMETRY"),
            Some(&"1".to_owned())
        );
        assert_eq!(env.get("OTEL_METRICS_EXPORTER"), Some(&"otlp".to_owned()));
        assert_eq!(env.get("OTEL_LOGS_EXPORTER"), Some(&"otlp".to_owned()));
        assert_eq!(
            env.get("OTEL_EXPORTER_OTLP_ENDPOINT"),
            Some(&"https://otel.example.com".to_owned())
        );
        assert_eq!(
            env.get("OTEL_EXPORTER_OTLP_HEADERS"),
            Some(&"Authorization=Bearer ingest-token".to_owned())
        );
    }

    #[test]
    fn identity_attributes_are_extracted_from_a_jwt_payload() {
        use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
        let payload = URL_SAFE_NO_PAD
            .encode(br#"{"sub":"user-uuid","email":"dev@example.com","preferred_username":"dev"}"#);
        let token = format!("header.{payload}.signature");

        let attributes = identity_attributes(&token);
        assert_eq!(attributes.get("user.id"), Some(&"user-uuid".to_owned()));
        assert_eq!(
            attributes.get("user.email"),
            Some(&"dev@example.com".to_owned())
        );
        assert_eq!(attributes.get("user.name"), Some(&"dev".to_owned()));
    }

    #[test]
    fn a_non_jwt_token_yields_no_attributes_rather_than_failing() {
        // An opaque token is a legitimate thing for an IdP to issue. Losing
        // the `user.id` label is acceptable; failing the developer's `login`
        // over it is not.
        assert!(identity_attributes("not-a-jwt").is_empty());
        assert!(identity_attributes("still.not.ajwt").is_empty());
    }

    #[test]
    fn binary_path_resolves_to_an_absolute_path() {
        // The test above pins the WRITER (it passes its fixture through), so
        // on its own it would still pass if this function regressed to the
        // bare-name fallback -- which is the actual defect that broke Codex.
        // This is the guard for the source of that string.
        let path = binary_path();
        assert!(
            path.starts_with('/'),
            "binary_path must be absolute so Codex can spawn it without a shell, got: {path}"
        );
        assert_ne!(
            path, "governance-auth",
            "the bare-name fallback means Codex gets `No such file or directory`"
        );
    }

    /// Codex reads `model_provider` from the document root; the same text
    /// nested under `[model_providers]` is valid TOML with the wrong meaning
    /// and would silently leave the old default in place. Parse rather than
    /// grep, because both forms contain the same substring.
    #[test]
    fn codex_default_provider_is_a_root_key() {
        let home = tempdir();
        fs::create_dir_all(home.path().join(".codex")).expect("codex dir");
        configure_codex(home.path(), &settings_with_gateway()).expect("configure");

        let text = fs::read_to_string(home.path().join(".codex/config.toml")).expect("read");
        let doc: toml_edit::DocumentMut = text.parse().expect("valid TOML");

        assert_eq!(
            doc.as_table()
                .get("model_provider")
                .and_then(|item| item.as_str()),
            Some(CODEX_PROVIDER_ID),
            "default provider must be a ROOT key, got:\n{text}"
        );
        assert!(
            doc["model_providers"].get("model_provider").is_none(),
            "key nested under [model_providers] -- Codex would ignore it:\n{text}"
        );
    }

    /// Authoritative means authoritative: an existing choice is replaced.
    #[test]
    fn codex_default_provider_overwrites_an_existing_choice() {
        let home = tempdir();
        fs::create_dir_all(home.path().join(".codex")).expect("codex dir");
        fs::write(
            home.path().join(".codex/config.toml"),
            "model_provider = \"openai\"\n",
        )
        .expect("seed");

        configure_codex(home.path(), &settings_with_gateway()).expect("configure");
        let text = fs::read_to_string(home.path().join(".codex/config.toml")).expect("read");
        let doc: toml_edit::DocumentMut = text.parse().expect("valid TOML");
        assert_eq!(
            doc.as_table()
                .get("model_provider")
                .and_then(|item| item.as_str()),
            Some(CODEX_PROVIDER_ID),
            "must take over, got:\n{text}"
        );
    }

    #[test]
    fn codex_auth_command_is_an_absolute_path_not_a_bare_name() {
        // THE regression guard for this module. Codex spawns `auth.command`
        // itself, NOT through a shell, so it never sees the login shell's
        // PATH. A bare `governance-auth` fails with `No such file or
        // directory (os error 2)` -- measured live against codex-cli 0.146.1
        // with the binary in ~/.local/bin, where it silently degraded the
        // provider to unauthenticated rather than erroring usefully. Claude
        // Code resolves a bare name fine (it uses a shell), so this trap is
        // invisible if you only ever test that client.
        let home = tempdir();
        fs::create_dir_all(home.path().join(".codex"))
            .expect("create .codex in the test's own temp dir");

        configure_codex(home.path(), &settings_with_gateway()).expect("write codex config");

        let text = fs::read_to_string(home.path().join(".codex/config.toml"))
            .expect("read back the config just written");
        let document = text
            .parse::<toml_edit::DocumentMut>()
            .expect("output must be valid TOML");

        let command = document["model_providers"][CODEX_PROVIDER_ID]["auth"]["command"]
            .as_str()
            .expect("auth.command must be written when a gateway URL is set");
        assert!(
            command.starts_with('/'),
            "auth.command must be absolute or Codex cannot spawn it, got: {command}"
        );
        assert_eq!(
            document["model_providers"][CODEX_PROVIDER_ID]["base_url"].as_str(),
            Some("https://api.example.com/v1"),
        );
        // Not "chat": codex-cli 0.146.1 refuses to load a config containing
        // it, which would brick the tool rather than disable the provider.
        assert_eq!(
            document["model_providers"][CODEX_PROVIDER_ID]["wire_api"].as_str(),
            Some("responses"),
        );
    }

    #[test]
    fn codex_provider_block_is_absent_without_a_gateway_url() {
        // Inference wiring is opt-in. A telemetry-only `configure` must not
        // invent a provider block -- doing so would point Codex at a gateway
        // the caller never named, and (given /v1/responses 404s today) at a
        // broken one.
        let home = tempdir();
        fs::create_dir_all(home.path().join(".codex"))
            .expect("create .codex in the test's own temp dir");

        configure_codex(home.path(), &settings()).expect("write codex config");

        let text = fs::read_to_string(home.path().join(".codex/config.toml"))
            .expect("read back the config just written");
        assert!(
            !text.contains("model_providers"),
            "telemetry-only configure must not write a provider block, got:\n{text}"
        );
    }

    #[test]
    fn claude_code_inference_keys_move_together() {
        // `apiKeyHelper` and `ANTHROPIC_BASE_URL` are written as a pair or
        // not at all: an apiKeyHelper pointed at this gateway's tokens while
        // the base URL still points at api.anthropic.com would ship a
        // Keycloak token to Anthropic on every request.
        let home = tempdir();
        fs::create_dir_all(home.path().join(".claude"))
            .expect("create .claude in the test's own temp dir");

        configure_claude_code(home.path(), &settings()).expect("telemetry-only write");
        let text = fs::read_to_string(home.path().join(".claude/settings.json"))
            .expect("read back settings.json");
        let value: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
        assert!(
            value.get("apiKeyHelper").is_none(),
            "no gateway => no helper"
        );
        assert!(
            value["env"].get("ANTHROPIC_BASE_URL").is_none(),
            "no gateway => no base URL"
        );

        configure_claude_code(home.path(), &settings_with_gateway()).expect("with-gateway write");
        let text = fs::read_to_string(home.path().join(".claude/settings.json"))
            .expect("read back settings.json");
        let value: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
        assert_eq!(
            value["apiKeyHelper"].as_str(),
            Some("/abs/path/governance-auth token"),
        );
        assert_eq!(
            value["env"]["ANTHROPIC_BASE_URL"].as_str(),
            Some("https://api.example.com/anthropic"),
        );
    }

    #[test]
    fn codex_exporter_is_a_struct_variant_not_a_bare_string() {
        // Regression guard for the shape Codex actually accepts. Writing
        // `exporter = "otlp-http"` parses fine as TOML and reads correctly
        // against the published reference, but codex-cli 0.146.1 rejects it
        // (`invalid type: unit variant, expected struct variant`) and then
        // REFUSES TO START -- so this mistake doesn't disable telemetry, it
        // bricks the developer's Codex until someone edits the file by hand.
        let home = tempdir();
        fs::create_dir_all(home.path().join(".codex"))
            .expect("create .codex in the test's own temp dir");

        configure_codex(home.path(), &settings()).expect("write codex config");

        let text = fs::read_to_string(home.path().join(".codex/config.toml"))
            .expect("read back the config just written");
        let document = text
            .parse::<toml_edit::DocumentMut>()
            .expect("output must be valid TOML");

        for kind in ["exporter", "metrics_exporter"] {
            let otlp = document["otel"][kind]["otlp-http"]
                .as_table()
                .unwrap_or_else(|| panic!("otel.{kind}.otlp-http must be a table, not a string"));
            assert_eq!(
                otlp["endpoint"].as_str(),
                Some("https://otel.example.com"),
                "otel.{kind}.otlp-http.endpoint"
            );
            assert_eq!(
                otlp["headers"]["Authorization"].as_str(),
                Some("Bearer ingest-token"),
                "otel.{kind}.otlp-http.headers.Authorization"
            );
        }
        assert!(
            document["otel"]["exporter"].as_str().is_none(),
            "otel.exporter must not be a bare string -- Codex rejects that and won't start"
        );
    }

    #[test]
    fn existing_codex_config_keeps_its_comments_and_other_tables() {
        // This file is hand-maintained: it carries project trust levels and
        // explanatory comments. A parse-and-reserialize round trip through a
        // plain `toml::Value` would silently delete every comment, which is
        // why this goes through `toml_edit`.
        let home = tempdir();
        let dir = home.path().join(".codex");
        fs::create_dir_all(&dir).expect("create .codex");
        fs::write(
            dir.join("config.toml"),
            "# a comment worth keeping\n[projects.\"/home/dev\"]\ntrust_level = \"trusted\"\n",
        )
        .expect("seed an existing config");

        configure_codex(home.path(), &settings()).expect("write codex config");

        let text = fs::read_to_string(dir.join("config.toml")).expect("read back");
        assert!(
            text.contains("# a comment worth keeping"),
            "comments must survive; got:\n{text}"
        );
        assert!(
            text.contains("trust_level = \"trusted\""),
            "unrelated tables must survive; got:\n{text}"
        );
        assert!(text.contains("[otel]"), "and the otel block must be added");
    }

    #[test]
    fn existing_claude_settings_are_merged_not_clobbered() {
        let home = tempdir();
        let dir = home.path().join(".claude");
        fs::create_dir_all(&dir).expect("create .claude");
        fs::write(
            dir.join("settings.json"),
            r#"{"theme":"dark","apiKeyHelper":"governance-auth token","env":{"EXISTING":"kept"}}"#,
        )
        .expect("seed existing settings");

        configure_claude_code(home.path(), &settings()).expect("write claude settings");

        let text = fs::read_to_string(dir.join("settings.json")).expect("read back");
        let value: serde_json::Value = serde_json::from_str(&text).expect("valid JSON out");
        assert_eq!(value["theme"], "dark", "unrelated top-level keys survive");
        assert_eq!(
            value["apiKeyHelper"], "governance-auth token",
            "the credential-helper wiring must not be disturbed by telemetry setup"
        );
        assert_eq!(
            value["env"]["EXISTING"], "kept",
            "pre-existing env entries survive"
        );
        assert_eq!(value["env"]["CLAUDE_CODE_ENABLE_TELEMETRY"], "1");
        assert_eq!(
            value["env"]["OTEL_EXPORTER_OTLP_ENDPOINT"],
            "https://otel.example.com"
        );
    }

    #[test]
    fn vscode_settings_are_merged_into_an_existing_user_config() {
        let home = tempdir();
        let user = vscode_user_dir(home.path(), "Code");
        fs::create_dir_all(&user).expect("create VS Code User dir");
        fs::write(
            user.join("settings.json"),
            r#"{"editor.fontSize":14,"github.copilot.enable":{"*":true}}"#,
        )
        .expect("seed existing VS Code settings");

        let outcomes = configure_vscode(home.path(), &settings()).expect("configure vscode");
        assert!(matches!(outcomes.as_slice(), [Outcome::Written(_)]));

        let value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(user.join("settings.json")).expect("read"))
                .expect("valid JSON out");
        assert_eq!(value["editor.fontSize"], 14, "unrelated settings survive");
        assert_eq!(value["github.copilot.enable"]["*"], true);
        assert_eq!(value["github.copilot.chat.otel.enabled"], true);
        assert_eq!(value["github.copilot.chat.otel.exporterType"], "otlp-http");
        assert_eq!(
            value["github.copilot.chat.otel.otlpEndpoint"],
            "https://otel.example.com"
        );
        assert_eq!(
            value["github.copilot.chat.otel.captureContent"], false,
            "content capture must stay off unless deliberately enabled"
        );
    }

    #[test]
    fn a_jsonc_vscode_config_is_refused_rather_than_stripped_of_its_comments() {
        // VS Code's settings.json legitimately allows comments. Parsing them
        // out and writing plain JSON back would delete a developer's
        // annotations permanently, so this must decline and tell them what to
        // add -- the file has to come back untouched.
        let home = tempdir();
        let user = vscode_user_dir(home.path(), "Code");
        fs::create_dir_all(&user).expect("create VS Code User dir");
        let original = "{\n  // my carefully explained setting\n  \"editor.fontSize\": 14\n}\n";
        fs::write(user.join("settings.json"), original).expect("seed JSONC settings");

        let error = configure_vscode(home.path(), &settings())
            .expect_err("a JSONC config must be refused, not silently rewritten");
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("github.copilot.chat.otel.enabled"),
            "the error must tell the developer exactly what to add; got: {rendered}"
        );

        assert_eq!(
            fs::read_to_string(user.join("settings.json")).expect("read back"),
            original,
            "the file must be left byte-for-byte untouched"
        );
    }

    #[test]
    fn vscode_insiders_and_vscodium_are_configured_too() {
        // A developer on Insiders or VSCodium would otherwise get nothing,
        // silently, because those keep entirely separate settings trees.
        let home = tempdir();
        for flavour in ["Code - Insiders", "VSCodium"] {
            fs::create_dir_all(vscode_user_dir(home.path(), flavour)).expect("create user dir");
        }

        let outcomes = configure_vscode(home.path(), &settings()).expect("configure vscode");
        assert_eq!(outcomes.len(), 2, "both flavours present must be written");
        for flavour in ["Code - Insiders", "VSCodium"] {
            let path = vscode_user_dir(home.path(), flavour).join("settings.json");
            assert!(path.exists(), "{flavour} settings.json should exist");
        }
    }

    #[test]
    fn vscode_auth_needs_an_env_var_because_no_setting_can_carry_it() {
        // Pins the documented gap: if VS Code ever gains a headers setting
        // this test is the thing that should be revisited.
        assert_eq!(
            vscode_manual_env(&settings()).as_deref(),
            Some("OTEL_EXPORTER_OTLP_HEADERS=Authorization=Bearer ingest-token")
        );
        let mut without = settings();
        without.token = None;
        assert_eq!(vscode_manual_env(&without), None);
    }

    #[test]
    fn the_credential_goes_in_a_0600_file_not_into_bashrc() {
        // The whole point of the sourced-file indirection: a developer's
        // .bashrc is routinely 0644 and routinely committed to a dotfiles
        // repo. A bearer token written there is a credential in git.
        let home = tempdir();
        fs::write(home.path().join(".bashrc"), "export EDITOR=vim\n").expect("seed bashrc");

        configure_shell_env(home.path(), &settings()).expect("configure shell env");

        let bashrc = fs::read_to_string(home.path().join(".bashrc")).expect("read bashrc");
        assert!(
            !bashrc.contains("ingest-token"),
            "the token must NEVER be written into an rc file; got:\n{bashrc}"
        );
        assert!(
            bashrc.contains("export EDITOR=vim"),
            "existing rc content must survive"
        );
        assert!(bashrc.contains(BLOCK_BEGIN) && bashrc.contains(BLOCK_END));

        let env_file = home.path().join(".config/governance-auth/otel.env");
        let contents = fs::read_to_string(&env_file).expect("read env file");
        assert!(contents.contains("ingest-token"), "secret lives here");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&env_file)
                .expect("stat env file")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "the credential file must be 0600");
        }
    }

    #[test]
    fn rerunning_replaces_the_block_rather_than_stacking_copies() {
        // Without marker-delimited replacement, every `login` would append
        // another block and the rc file would grow without bound.
        let home = tempdir();
        fs::write(home.path().join(".zshrc"), "# mine\n").expect("seed zshrc");

        for _ in 0..3 {
            configure_shell_env(home.path(), &settings()).expect("configure");
        }

        let zshrc = fs::read_to_string(home.path().join(".zshrc")).expect("read zshrc");
        assert_eq!(
            zshrc.matches(BLOCK_BEGIN).count(),
            1,
            "exactly one managed block after repeated runs; got:\n{zshrc}"
        );
        assert!(
            zshrc.contains("# mine"),
            "the developer's own lines survive"
        );
    }

    #[test]
    fn a_half_present_marker_pair_is_refused_rather_than_guessed_at() {
        let home = tempdir();
        let rc = home.path().join(".bashrc");
        let original = format!("# mine\n{BLOCK_BEGIN}\nsomething hand-edited\n");
        fs::write(&rc, &original).expect("seed a damaged block");

        let error = configure_shell_env(home.path(), &settings())
            .expect_err("a half-present block must not be silently appended to");
        assert!(format!("{error:#}").contains("only one of the governance-auth markers"));
        assert_eq!(
            fs::read_to_string(&rc).expect("read back"),
            original,
            "the damaged file must be left untouched"
        );
    }

    #[test]
    fn fish_gets_its_own_syntax_not_posix_export() {
        // `export VAR=value` is a syntax error in fish; a shared file would
        // break every new shell the developer opens.
        let home = tempdir();
        let fish_dir = home.path().join(".config/fish");
        fs::create_dir_all(&fish_dir).expect("create fish config dir");
        fs::write(fish_dir.join("config.fish"), "# fish\n").expect("seed config.fish");

        configure_shell_env(home.path(), &settings()).expect("configure");

        let fish_env = fs::read_to_string(home.path().join(".config/governance-auth/otel.fish"))
            .expect("read fish env file");
        assert!(fish_env.contains("set -gx OTEL_EXPORTER_OTLP_HEADERS"));
        assert!(
            !fish_env.contains("export "),
            "fish must not get POSIX export"
        );

        let config = fs::read_to_string(fish_dir.join("config.fish")).expect("read config.fish");
        assert!(config.contains("and source"), "fish sources, not dots");
    }

    #[test]
    fn no_token_exports_everything_except_the_header() {
        let home = tempdir();
        fs::write(home.path().join(".bashrc"), "# mine\n").expect("seed");
        let mut without = settings();
        without.token = None;

        let outcomes = configure_shell_env(home.path(), &without).expect("configure");
        assert!(
            !outcomes.is_empty(),
            "issuer/client-id still belong in the shell so the binary needs no flags"
        );

        let env = fs::read_to_string(home.path().join(".config/governance-auth/otel.env"))
            .expect("env file");
        assert!(env.contains("GOVERNANCE_AUTH_ISSUER"), "{env}");
        assert!(env.contains("OTEL_EXPORTER_OTLP_ENDPOINT"), "{env}");
        // The point that survived the behaviour change: no credential exists,
        // so no Authorization header may be exported.
        assert!(
            !env.contains("OTEL_EXPORTER_OTLP_HEADERS"),
            "exported a header with no token: {env}"
        );
    }

    #[test]
    fn an_absent_tool_is_skipped_rather_than_created() {
        // Creating `~/.codex` for someone who doesn't use Codex would be
        // surprising, and an empty config dir changes some tools' first-run
        // behavior.
        let home = tempdir();
        let outcome = configure_codex(home.path(), &settings()).expect("skip cleanly");
        assert!(matches!(outcome, Outcome::Skipped(_)));
        assert!(!home.path().join(".codex").exists());
    }

    /// Minimal scratch dir, removed on drop -- same reason `tests/support`
    /// hand-rolls one rather than pulling in `tempfile` for a couple of uses.
    struct TempDir(PathBuf);

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
            "governance-auth-otel-{}-{unique}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create temp dir");
        TempDir(path)
    }

    #[test]
    fn no_token_writes_no_header_rather_than_an_empty_one() {
        // An `Authorization=Bearer ` with nothing after it is worse than no
        // header at all: it looks configured and fails at the collector.
        let mut without = settings();
        without.token = None;
        let env: BTreeMap<_, _> = claude_code_env(&without).into_iter().collect();
        assert!(!env.contains_key("OTEL_EXPORTER_OTLP_HEADERS"));
    }

    /// `settings_with_gateway()` minus the OTEL endpoint, i.e. exactly what
    /// `--gateway-url` alone (no `--otel-endpoint`) produces. This is the
    /// regression fixture for the bug this module fixes: inference wiring
    /// used to be unreachable whenever telemetry wasn't configured, because
    /// `oauth::apply_telemetry` bailed out before ever building an
    /// `OtelSettings`, let alone calling into these writers.
    fn settings_gateway_only() -> OtelSettings {
        OtelSettings {
            issuer: "https://auth.example".to_owned(),
            client_id: "cli".to_owned(),
            endpoint: None,
            headers_helper: None,
            ..settings_with_gateway()
        }
    }

    #[test]
    fn gateway_only_writes_claude_code_inference_keys_with_no_telemetry_keys() {
        let home = tempdir();
        fs::create_dir_all(home.path().join(".claude")).expect("create .claude");

        configure_claude_code(home.path(), &settings_gateway_only()).expect("write claude config");

        let text =
            fs::read_to_string(home.path().join(".claude/settings.json")).expect("read back");
        let value: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");

        // The bug: this used to never run at all when no OTEL endpoint was
        // supplied, because `apply_telemetry` returned before reaching here.
        assert_eq!(
            value["apiKeyHelper"].as_str(),
            Some("/abs/path/governance-auth token"),
            "gateway-only configure must still write apiKeyHelper"
        );
        assert_eq!(
            value["env"]["ANTHROPIC_BASE_URL"].as_str(),
            Some("https://api.example.com/anthropic"),
            "gateway-only configure must still write ANTHROPIC_BASE_URL"
        );

        // The other half: without an OTEL endpoint there is nothing to point
        // a telemetry key at, so none of these should appear.
        assert!(value.get("otelHeadersHelper").is_none());
        for key in [
            "CLAUDE_CODE_ENABLE_TELEMETRY",
            "OTEL_METRICS_EXPORTER",
            "OTEL_LOGS_EXPORTER",
            "OTEL_EXPORTER_OTLP_PROTOCOL",
            "OTEL_EXPORTER_OTLP_ENDPOINT",
            "OTEL_RESOURCE_ATTRIBUTES",
            "OTEL_EXPORTER_OTLP_HEADERS",
        ] {
            assert!(
                value["env"].get(key).is_none(),
                "gateway-only configure must not write telemetry key {key}, got:\n{text}"
            );
        }
    }

    #[test]
    fn gateway_only_writes_codex_provider_block_with_no_otel_table() {
        let home = tempdir();
        fs::create_dir_all(home.path().join(".codex")).expect("create .codex");

        configure_codex(home.path(), &settings_gateway_only()).expect("write codex config");

        let text = fs::read_to_string(home.path().join(".codex/config.toml")).expect("read back");
        assert!(
            text.contains("model_providers"),
            "gateway-only configure must still write the provider block, got:\n{text}"
        );
        assert!(
            !text.contains("[otel]"),
            "gateway-only configure must not write an otel table with no endpoint, got:\n{text}"
        );
    }

    #[test]
    fn vscode_is_untouched_without_an_otel_endpoint() {
        // VS Code Copilot's OTEL surface is telemetry-only; a gateway-only
        // configure has nothing for this writer to do, and must not create or
        // touch its settings.json.
        let home = tempdir();
        let user = vscode_user_dir(home.path(), "Code");
        fs::create_dir_all(&user).expect("create VS Code User dir");
        fs::write(user.join("settings.json"), r#"{"editor.fontSize":14}"#).expect("seed settings");

        let outcomes = configure_vscode(home.path(), &settings_gateway_only()).expect("configure");
        assert!(outcomes.is_empty(), "nothing to write, so nothing reported");

        let text = fs::read_to_string(user.join("settings.json")).expect("read back");
        assert_eq!(
            text, r#"{"editor.fontSize":14}"#,
            "file must be left untouched"
        );
    }

    #[test]
    fn no_otel_endpoint_exports_no_otel_variables() {
        let home = tempdir();
        fs::write(home.path().join(".bashrc"), "# mine\n").expect("seed bashrc");

        let outcomes =
            configure_shell_env(home.path(), &settings_gateway_only()).expect("configure");
        assert!(
            !outcomes.is_empty(),
            "gateway + identity still get exported"
        );

        let env = fs::read_to_string(home.path().join(".config/governance-auth/otel.env"))
            .expect("env file");
        assert!(env.contains("ANTHROPIC_BASE_URL"), "{env}");
        assert!(env.contains("GOVERNANCE_AUTH_CLIENT_ID"), "{env}");
        // No collector means every OTEL_* variable would point at nothing.
        assert!(
            !env.contains("OTEL_"),
            "exported OTEL config with no endpoint: {env}"
        );

        // The rc file is still only ever a `source` line inside the markers.
        let bashrc = fs::read_to_string(home.path().join(".bashrc")).expect("read bashrc");
        assert!(bashrc.starts_with("# mine\n"), "clobbered the user's file");
        assert!(
            !bashrc.contains("ANTHROPIC_BASE_URL"),
            "secret-adjacent value inlined into rc"
        );
    }

    #[test]
    fn endpoint_and_gateway_together_write_both_telemetry_and_inference() {
        let home = tempdir();
        fs::create_dir_all(home.path().join(".claude")).expect("create .claude");
        fs::create_dir_all(home.path().join(".codex")).expect("create .codex");

        let both = settings_with_gateway();
        configure_claude_code(home.path(), &both).expect("write claude config");
        configure_codex(home.path(), &both).expect("write codex config");

        let claude = fs::read_to_string(home.path().join(".claude/settings.json")).expect("read");
        let value: serde_json::Value = serde_json::from_str(&claude).expect("valid JSON");
        assert_eq!(
            value["apiKeyHelper"].as_str(),
            Some("/abs/path/governance-auth token")
        );
        assert_eq!(
            value["env"]["OTEL_EXPORTER_OTLP_ENDPOINT"].as_str(),
            Some("https://otel.example.com")
        );

        let codex = fs::read_to_string(home.path().join(".codex/config.toml")).expect("read");
        assert!(codex.contains("model_providers"), "got:\n{codex}");
        assert!(codex.contains("[otel]"), "got:\n{codex}");
    }
}
