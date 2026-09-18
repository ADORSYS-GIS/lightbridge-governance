//! Tests for [`super`]. Split into its own file (issue #175) rather than
//! raising the already-grandfathered LoC ceiling -- the same move
//! `otel/tests.rs` made, applied to the whole `mod tests` block.

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
