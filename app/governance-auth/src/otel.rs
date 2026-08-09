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
    /// Collector base URL, e.g. `https://otel.ai.camer.digital`. Signal
    /// suffixes (`/v1/metrics`, `/v1/logs`, `/v1/traces`) are appended by the
    /// SDKs themselves from this base -- do not include one here.
    pub endpoint: String,
    /// Long-lived OTLP ingest credential, rendered into the header value both
    /// tools send verbatim. `None` writes the endpoint but no header, which
    /// is only useful against a collector that doesn't authenticate.
    pub token: Option<Redacted<String>>,
    /// Stamped onto every exported signal. Carries who this developer is, so
    /// telemetry arriving at the collector is attributable without the
    /// collector having to resolve the OTLP credential back to a person.
    pub resource_attributes: BTreeMap<String, String>,
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
    Ok(outcomes)
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
    let mut outcomes = Vec::new();
    for flavour in VSCODE_FLAVOURS {
        let dir = vscode_user_dir(home, flavour);
        if !dir.is_dir() {
            continue;
        }
        outcomes.push(configure_vscode_flavour(&dir, settings)?);
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

fn configure_vscode_flavour(user_dir: &Path, settings: &OtelSettings) -> Result<Outcome> {
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
                vscode_settings_hint(settings)
            )
        })?
    };

    let object = root
        .as_object_mut()
        .with_context(|| format!("{} is not a JSON object", path.display()))?;

    for (key, value) in vscode_settings(settings) {
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
fn vscode_settings(settings: &OtelSettings) -> Vec<(&'static str, serde_json::Value)> {
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
            serde_json::Value::String(settings.endpoint.clone()),
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
fn vscode_settings_hint(settings: &OtelSettings) -> String {
    vscode_settings(settings)
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

    let env = object
        .entry("env")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()))
        .as_object_mut()
        .with_context(|| format!("`env` in {} is not a JSON object", path.display()))?;

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
        ("CLAUDE_CODE_ENABLE_TELEMETRY", "1".to_owned()),
        ("OTEL_METRICS_EXPORTER", "otlp".to_owned()),
        ("OTEL_LOGS_EXPORTER", "otlp".to_owned()),
        ("OTEL_EXPORTER_OTLP_PROTOCOL", "http/protobuf".to_owned()),
        ("OTEL_EXPORTER_OTLP_ENDPOINT", settings.endpoint.clone()),
        (
            "OTEL_RESOURCE_ATTRIBUTES",
            settings.resource_attributes_value(),
        ),
    ];
    if let Some(headers) = settings.headers_value() {
        entries.push(("OTEL_EXPORTER_OTLP_HEADERS", headers));
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

    let otel = table_entry(document.as_table_mut(), "otel")?;
    otel.insert("environment", toml_edit::value("prod"));
    // Content capture stays off. The collector's own redaction is the
    // authoritative control (RFC-0002 treats that as a release blocker, not
    // an enhancement), but a client that never sends raw prompts in the first
    // place is one fewer place for them to leak.
    otel.insert("log_user_prompt", toml_edit::value(false));

    // `otel.exporter` is a TAGGED ENUM, not a string: the exporter kind is
    // the table NAME and its settings are that table's contents. Writing
    // `exporter = "otlp-http"` with the settings in a sibling table parses as
    // TOML but Codex rejects it at load time with `invalid type: unit
    // variant, expected struct variant in otel.exporter` -- and Codex refuses
    // to start at all on a config it can't load, so getting this wrong
    // bricks the tool rather than just disabling telemetry. The shape below
    // was confirmed by loading it in codex-cli 0.146.1, not inferred from the
    // reference docs (which describe it as `otel.exporter.<id>.endpoint`).
    for kind in ["exporter", "metrics_exporter"] {
        let exporter = table_entry(otel, kind)?;
        let otlp = table_entry(exporter, "otlp-http")?;
        otlp.insert("endpoint", toml_edit::value(&settings.endpoint));
        otlp.insert("protocol", toml_edit::value("binary"));
        if let Some(token) = &settings.token {
            let headers = table_entry(otlp, "headers")?;
            headers.insert(
                "Authorization",
                toml_edit::value(format!("Bearer {}", token.expose())),
            );
        }
    }

    let mut bytes = document.to_string().into_bytes();
    if !bytes.ends_with(b"\n") {
        bytes.push(b'\n');
    }
    write_atomically(&path, &bytes)?;
    Ok(Outcome::Written(path))
}

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
            endpoint: "https://otel.example.com".to_owned(),
            token: Some(Redacted::new("ingest-token".to_owned())),
            resource_attributes: BTreeMap::from([
                ("user.id".to_owned(), "abc-123".to_owned()),
                ("service.name".to_owned(), "claude-code".to_owned()),
            ]),
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
}
