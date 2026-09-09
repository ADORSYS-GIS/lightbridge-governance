//! Codex model-provider config shape and argv boundaries.
use super::{
    tests::{settings, settings_with_gateway, tempdir},
    *,
};

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
fn codex_auth_uses_an_absolute_command_and_a_separate_argument_array() {
    // Codex passes `auth.command` directly to the OS. Putting flags in that
    // string makes them part of the executable filename and fails with os
    // error 2. Its `args` property is the argv boundary.
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
    assert!(
        !command.contains(" --issuer"),
        "auth.command must contain only the executable path, got: {command}"
    );
    let args = document["model_providers"][CODEX_PROVIDER_ID]["auth"]["args"]
        .as_array()
        .expect("auth.args must be an array");
    let args: Vec<&str> = args
        .iter()
        .map(|value| value.as_str().expect("every argument must be a string"))
        .collect();
    assert_eq!(
        args,
        [
            "--issuer",
            "https://auth.example",
            "--client-id",
            "cli",
            "token"
        ]
    );
    assert_eq!(
        document["model_providers"][CODEX_PROVIDER_ID]["base_url"].as_str(),
        Some("https://api.example.com/v1"),
    );
    // Codex rejects the former `chat` value while loading its config.
    assert_eq!(
        document["model_providers"][CODEX_PROVIDER_ID]["wire_api"].as_str(),
        Some("responses"),
    );
}

#[test]
fn codex_auth_replaces_the_old_single_command_shape() {
    let home = tempdir();
    let dir = home.path().join(".codex");
    fs::create_dir_all(&dir).expect("codex dir");
    fs::write(
        dir.join("config.toml"),
        "[model_providers.governance.auth]\ncommand = \"/old/governance-auth --issuer \
         https://old.example --client-id old token\"\n",
    )
    .expect("seed old config");

    configure_codex(home.path(), &settings_with_gateway()).expect("configure");
    let text = fs::read_to_string(dir.join("config.toml")).expect("read");
    let document: toml_edit::DocumentMut = text.parse().expect("valid TOML");
    let auth = &document["model_providers"][CODEX_PROVIDER_ID]["auth"];
    assert_eq!(auth["command"].as_str(), Some(binary_path().as_str()));
    assert!(auth["args"].is_array(), "new argv missing:\n{text}");
    assert!(
        !text.contains("https://old.example"),
        "old argv survived:\n{text}"
    );
}

#[test]
fn codex_daemon_config_matches_the_working_shape_and_preserves_user_settings() {
    let home = tempdir();
    let dir = home.path().join(".codex");
    fs::create_dir_all(&dir).expect("codex dir");
    fs::write(
        dir.join("config.toml"),
        r#"model = "gpt-5.6-sol"
model_reasoning_effort = "low"

[projects."/home/developer/.local"]
trust_level = "trusted"
"#,
    )
    .expect("seed existing Codex settings");
    let configured = OtelSettings {
        issuer: "https://auth.ai.camer.digital".to_owned(),
        client_id: "governance-auth-cli".to_owned(),
        endpoint: Some("http://127.0.0.1:17457".to_owned()),
        gateway_url: Some("https://api.ai.camer.digital".to_owned()),
        ..settings()
    };

    configure_codex(home.path(), &configured).expect("configure");
    let text = fs::read_to_string(dir.join("config.toml")).expect("read");
    let document: toml_edit::DocumentMut = text.parse().expect("valid TOML");

    assert_eq!(document["model_provider"].as_str(), Some("governance"));
    assert_eq!(document["model"].as_str(), Some("gpt-5.6-sol"));
    assert_eq!(document["model_reasoning_effort"].as_str(), Some("low"));
    assert_eq!(
        document["otel"]["exporter"]["otlp-http"]["endpoint"].as_str(),
        Some("http://127.0.0.1:17457")
    );
    assert_eq!(
        document["otel"]["metrics_exporter"]["otlp-http"]["endpoint"].as_str(),
        Some("http://127.0.0.1:17457")
    );
    assert_eq!(
        document["model_providers"][CODEX_PROVIDER_ID]["base_url"].as_str(),
        Some("https://api.ai.camer.digital/v1")
    );
    let auth = &document["model_providers"][CODEX_PROVIDER_ID]["auth"];
    assert_eq!(auth["command"].as_str(), Some(binary_path().as_str()));
    let args: Vec<&str> = auth["args"]
        .as_array()
        .expect("auth args")
        .iter()
        .map(|value| value.as_str().expect("string argument"))
        .collect();
    assert_eq!(
        args,
        [
            "--issuer",
            "https://auth.ai.camer.digital",
            "--client-id",
            "governance-auth-cli",
            "token"
        ]
    );
    assert_eq!(
        document["projects"]["/home/developer/.local"]["trust_level"].as_str(),
        Some("trusted"),
        "existing project trust must survive:\n{text}"
    );
}
