//! Tests for [`super`]. Split into its own file (review, #302) rather than
//! raising `otel.rs`'s already-grandfathered LoC ceiling -- the same move
//! `telemetry_wiring.rs`/`vscode/tests.rs` made, applied to the whole
//! `mod tests` block since none of it lived in its own file yet.

use super::*;
use crate::optout::ClientOptOut;

pub(super) fn settings() -> OtelSettings {
    OtelSettings {
        issuer: "https://auth.example".to_owned(),
        client_id: "cli".to_owned(),
        endpoint: Some("https://otel.example.com".to_owned()),
        copilot_spool: PathBuf::from("/state/governance-auth/copilot-otel.jsonl"),
        copilot_drain_available: true,
        copilot_otlp_direct: false,
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
/// Every file, byte-identical after a second run.
///
/// Not a nicety: `configure` runs on every `login`, and anything that
/// churns here shows up as a spurious diff in a developer's dotfiles --
/// or, for the managed manifest, as a retraction that deletes and rewrites
/// the same key forever.
#[test]
fn configure_all_is_idempotent() {
    let home = tempdir();
    fs::create_dir_all(home.path().join(".claude")).expect("claude dir");
    fs::create_dir_all(home.path().join(".codex")).expect("codex dir");
    fs::create_dir_all(crate::vscode::user_dir(home.path(), "Code")).expect("vscode dir");
    fs::write(home.path().join(".bashrc"), "# mine\n").expect("seed bashrc");

    let settings = settings_with_gateway();
    configure_all(home.path(), &settings, ClientOptOut::default()).expect("first run");
    let first = snapshot(home.path());
    assert!(
        first.len() >= 5,
        "expected several files to be written, got {:?}",
        first.keys().collect::<Vec<_>>()
    );
    assert!(
        first.keys().any(|k| k.ends_with("managed.json")),
        "the manifest must be among them: {:?}",
        first.keys().collect::<Vec<_>>()
    );

    configure_all(home.path(), &settings, ClientOptOut::default()).expect("second run");
    let second = snapshot(home.path());

    for (path, before) in &first {
        let after = second
            .get(path)
            .expect("file disappeared on the second run");
        assert_eq!(before, after, "second run changed {path}");
    }
    assert_eq!(
        first.len(),
        second.len(),
        "second run added or removed a file"
    );
}

/// #270 AC4/AC6: switching profiles retracts the OTHER profile's keys
/// via the digest rule, and a key the developer hand-edited survives
/// the retraction that would otherwise have removed it. `manual` here
/// is `settings()` itself (its fixture already carries a token and no
/// `headers_helper` -- exactly what `TelemetryWiring::resolve` produces
/// under `manual`); `daemon` substitutes the loopback endpoint and
/// drops the token, mirroring that same resolution.
///
/// Falsification per the ticket's own Test Expectations: this test was
/// run against a build with the `!is_daemon` guard on
/// `TelemetryWiring::token` deleted, and it failed on the `manual ->
/// daemon` assertion below for the predicted reason (the header
/// survived) before being restored.
///
/// ⚠️ **Scope** (#280 review, P2-4): this proves retraction for a key
/// this binary *currently owns and has not been hand-edited* -- exactly
/// `managed`'s digest rule, by design. A key never written by this
/// binary, written by an older version before this record existed, or
/// edited by the developer is left alone on purpose (the test below
/// proves that half too). "Switching profiles retracts the other
/// profile's keys" holds for the keys this binary manages; it is not a
/// claim about every credential that could exist in a config file by
/// some other means.
#[test]
fn switching_profiles_retracts_the_other_profiles_keys_but_not_a_hand_edit() {
    let home = tempdir();
    fs::create_dir_all(home.path().join(".codex")).expect("codex dir");
    let codex_path = home.path().join(".codex").join("config.toml");

    let manual = settings();
    let daemon = OtelSettings {
        endpoint: Some(OTEL_LOOPBACK_ENDPOINT.to_owned()),
        token: None,
        headers_helper: None,
        copilot_drain_available: false,
        copilot_otlp_direct: false,
        ..settings()
    };

    // manual: writes a static Authorization header (this fixture's token).
    configure_all(home.path(), &manual, ClientOptOut::default()).expect("manual run");
    let after_manual = fs::read_to_string(&codex_path).expect("read codex config");
    assert!(
        after_manual.contains("Authorization = \"Bearer ingest-token\""),
        "manual must write the static header: {after_manual}"
    );

    // daemon: the header is no longer owned, so it must be retracted --
    // not merely left unwritten by the writer, which never removes a
    // key itself (`retract_stale`'s job, exercised here end to end).
    configure_all(home.path(), &daemon, ClientOptOut::default()).expect("daemon run");
    let after_daemon = fs::read_to_string(&codex_path).expect("read codex config");
    assert!(
        !after_daemon.contains("Authorization"),
        "daemon must retract the header manual wrote: {after_daemon}"
    );
    assert!(
        after_daemon.contains(OTEL_LOOPBACK_ENDPOINT),
        "daemon must point Codex at loopback: {after_daemon}"
    );

    // Re-run manual (writes the header fresh, with a freshly recorded
    // digest), then hand-edit that value directly on disk -- simulating
    // a developer who touched it -- before switching to daemon again.
    // THIS is where the digest rule is exercised: the header is once
    // more a retraction candidate, but its current value no longer
    // matches what this binary last recorded writing.
    configure_all(home.path(), &manual, ClientOptOut::default()).expect("manual run again");
    let hand_edited = fs::read_to_string(&codex_path)
        .expect("read codex config")
        .replace(
            "Authorization = \"Bearer ingest-token\"",
            "Authorization = \"Bearer developers-own-token\"",
        );
    assert_ne!(
        hand_edited,
        fs::read_to_string(&codex_path).expect("read codex config"),
        "the replace must have matched something, or this test proves nothing"
    );
    fs::write(&codex_path, &hand_edited).expect("hand-edit codex config");

    configure_all(home.path(), &daemon, ClientOptOut::default()).expect("daemon run again");
    let after_hand_edit = fs::read_to_string(&codex_path).expect("read codex config");
    assert!(
        after_hand_edit.contains("Authorization = \"Bearer developers-own-token\""),
        "a hand-edited value must survive retraction, not be deleted with the key: \
         {after_hand_edit}"
    );
}

/// Confirmed live, on a real machine (pre-#272): without
/// `copilot_drain_available` gating BOTH `vscode::configure`'s writer AND
/// `managed::plan`'s candidate list, switching to `daemon` only stopped
/// WRITING Copilot's file exporter -- it never RETRACTED a prior `manual`
/// run's, because `telemetry` (`endpoint.is_some()`) stays true under
/// `daemon` (the loopback substitute), so `plan()` kept reading the
/// untouched config back and recording it as still owned. Copilot kept
/// appending to a spool the drain that used to empty it no longer existed
/// to drain -- unbounded, not just lost.
///
/// #272 gave `daemon` a real Copilot path (`copilot_otlp_direct`,
/// otlp-http at loopback), so this now proves the FULL switch: `outfile`
/// gone (nothing should still be appending to it), `exporterType` changed
/// from `file` to `otlp-http` (not merely absent), and `otlpEndpoint`
/// present.
#[test]
fn switching_to_daemon_retracts_copilots_file_exporter_for_its_own_otlp_path() {
    let home = tempdir();
    fs::create_dir_all(crate::vscode::user_dir(home.path(), "Code")).expect("vscode dir");
    let vscode_path = crate::vscode::user_dir(home.path(), "Code").join("settings.json");

    let manual = settings();
    let daemon = OtelSettings {
        endpoint: Some(OTEL_LOOPBACK_ENDPOINT.to_owned()),
        token: None,
        headers_helper: None,
        copilot_drain_available: false,
        copilot_otlp_direct: true,
        ..settings()
    };

    configure_all(home.path(), &manual, ClientOptOut::default()).expect("manual run");
    let after_manual = fs::read_to_string(&vscode_path).expect("read vscode settings");
    assert!(
        after_manual.contains("\"file\""),
        "manual must enable Copilot's file exporter: {after_manual}"
    );

    configure_all(home.path(), &daemon, ClientOptOut::default()).expect("daemon run");
    let after_daemon = fs::read_to_string(&vscode_path).expect("read vscode settings");
    assert!(
        !after_daemon.contains("github.copilot.chat.otel.outfile"),
        "daemon must retract the outfile Copilot was writing to: {after_daemon}"
    );
    assert!(
        after_daemon.contains("\"otlp-http\""),
        "daemon must switch Copilot onto its own otlp-http exporter, not merely stop the \
         file one: {after_daemon}"
    );
    assert!(
        after_daemon.contains(OTEL_LOOPBACK_ENDPOINT),
        "daemon must point Copilot's otlp-http exporter at loopback: {after_daemon}"
    );
}

/// The other direction: a developer switching FROM `daemon` back TO
/// `manual` must have the otlp-http keys retracted, not left beside the
/// file exporter's -- the two paths are mutually exclusive by design
/// (`TelemetryWiring::resolve`), and Copilot only honours one
/// `exporterType` at a time regardless.
#[test]
fn switching_back_to_manual_retracts_daemons_otlp_exporter() {
    let home = tempdir();
    fs::create_dir_all(crate::vscode::user_dir(home.path(), "Code")).expect("vscode dir");
    let vscode_path = crate::vscode::user_dir(home.path(), "Code").join("settings.json");

    let manual = settings();
    let daemon = OtelSettings {
        endpoint: Some(OTEL_LOOPBACK_ENDPOINT.to_owned()),
        token: None,
        headers_helper: None,
        copilot_drain_available: false,
        copilot_otlp_direct: true,
        ..settings()
    };

    configure_all(home.path(), &daemon, ClientOptOut::default()).expect("daemon run");
    configure_all(home.path(), &manual, ClientOptOut::default()).expect("manual run");
    let after_manual = fs::read_to_string(&vscode_path).expect("read vscode settings");

    assert!(
        !after_manual.contains("github.copilot.chat.otel.otlpEndpoint"),
        "manual must retract daemon's otlpEndpoint: {after_manual}"
    );
    assert!(
        after_manual.contains("github.copilot.chat.otel.outfile"),
        "manual must write its own outfile: {after_manual}"
    );
}

/// path -> contents, for every file under `root`.
fn snapshot(root: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(text) = fs::read_to_string(&path) {
                out.insert(path.display().to_string(), text);
            }
        }
    }
    out
}

pub(super) fn settings_with_gateway() -> OtelSettings {
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
    let entrypoint = env.get("OTEL_METRICS_INCLUDE_ENTRYPOINT");
    assert_eq!(entrypoint, Some(&"1".to_owned()));
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
fn the_shell_file_stays_0600_and_the_rc_only_sources_it() {
    // The whole point of the sourced-file indirection: a developer's
    // .bashrc is routinely 0644 and routinely committed to a dotfiles
    // repo. A bearer token written there is a credential in git. Nothing
    // secret is placed here any more (see `configure_shell_env`), and the
    // posture is kept for whatever lands here next.
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
    assert!(
        contents.contains("GOVERNANCE_AUTH_ISSUER"),
        "this binary's own settings live here"
    );

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
    assert!(fish_env.contains("set -gx GOVERNANCE_AUTH_ISSUER"));
    assert!(
        !fish_env.contains("export "),
        "fish must not get POSIX export"
    );

    let config = fs::read_to_string(fish_dir.join("config.fish")).expect("read config.fish");
    assert!(config.contains("and source"), "fish sources, not dots");
}

#[test]
fn a_token_never_reaches_the_shell_whether_or_not_one_exists() {
    let home = tempdir();
    fs::write(home.path().join(".bashrc"), "# mine\n").expect("seed");

    for token in [Some(Redacted::new("ingest-token".to_owned())), None] {
        let mut variant = settings();
        variant.token = token;

        let outcomes = configure_shell_env(home.path(), &variant).expect("configure");
        assert!(
            !outcomes.is_empty(),
            "issuer/client-id still belong in the shell so the binary needs no flags"
        );

        let env = fs::read_to_string(home.path().join(".config/governance-auth/otel.env"))
            .expect("env file");
        assert!(env.contains("GOVERNANCE_AUTH_ISSUER"), "{env}");
        assert!(
            !env.contains("OTEL_EXPORTER_OTLP_HEADERS"),
            "exported an Authorization header into a machine-global file: {env}"
        );
    }
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
pub(super) struct TempDir(PathBuf);

impl TempDir {
    pub(super) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub(super) fn tempdir() -> TempDir {
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
        copilot_drain_available: false,
        copilot_otlp_direct: false,
        headers_helper: None,
        ..settings_with_gateway()
    }
}

#[test]
fn gateway_only_writes_claude_code_inference_keys_with_no_telemetry_keys() {
    let home = tempdir();
    fs::create_dir_all(home.path().join(".claude")).expect("create .claude");

    configure_claude_code(home.path(), &settings_gateway_only()).expect("write claude config");

    let text = fs::read_to_string(home.path().join(".claude/settings.json")).expect("read back");
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
        "OTEL_METRICS_INCLUDE_ENTRYPOINT",
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
fn no_otel_endpoint_exports_no_otel_variables() {
    let home = tempdir();
    fs::write(home.path().join(".bashrc"), "# mine\n").expect("seed bashrc");

    let outcomes = configure_shell_env(home.path(), &settings_gateway_only()).expect("configure");
    assert!(
        !outcomes.is_empty(),
        "gateway + identity still get exported"
    );

    let env =
        fs::read_to_string(home.path().join(".config/governance-auth/otel.env")).expect("env file");
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
