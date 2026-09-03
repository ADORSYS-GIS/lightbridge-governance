//! `Telemetry::survey`'s two #272 AC4 derivations. Split out of `telemetry.rs`
//! (which pins `row()`'s rendering of the same fields via struct literals)
//! purely to stay under the LoC ceiling.

use super::*;

/// Minimal config for the tests below -- `profile` is the axis under test,
/// everything else is filler.
fn config(profile: crate::profile::Profile) -> crate::config::OauthConfig {
    crate::config::OauthConfig {
        issuer: "https://issuer.example".to_owned(),
        client_id: "client".to_owned(),
        scopes: "openid".to_owned(),
        audience: None,
        otel_endpoint: Some("https://otel.example".to_owned()),
        otel_token: None,
        gateway_url: None,
        profile,
        copilot_spool_path: None,
        otel_headers_debounce_ms: 240_000,
        open_browser: false,
        token_exchange: None,
    }
}

/// #272 AC4: `token_required` tracks the profile, not any manifest content --
/// a static token is only ever expected under `manual`.
#[test]
fn token_required_tracks_the_profile() {
    let home = crate::managed::testutil::tempdir();
    for (profile, expected) in [
        (crate::profile::Profile::Daemon, false),
        (crate::profile::Profile::Manual, true),
    ] {
        let telemetry = Telemetry::survey(Some(home.path()), &config(profile));
        assert_eq!(telemetry.token_required, expected, "profile = {profile}");
    }
}

/// #272 AC4's other derivation: `codex_installed` reads the filesystem, not
/// the config.
#[test]
fn codex_installed_reflects_whether_the_directory_exists() {
    let home = crate::managed::testutil::tempdir();
    let config = config(crate::profile::Profile::Manual);
    assert!(
        !Telemetry::survey(Some(home.path()), &config).codex_installed,
        "nothing created yet"
    );
    std::fs::create_dir_all(home.path().join(".codex")).expect("mkdir .codex");
    assert!(Telemetry::survey(Some(home.path()), &config).codex_installed);
}
