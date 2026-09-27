use super::*;

fn session() -> CachedSession {
    CachedSession {
        issuer: "https://issuer.example.com".to_owned(),
        client_id: "client".to_owned(),
        access_token: Redacted::new("access-token".to_owned()),
        refresh_token: None,
        expires_at: 0,
        lifetime_secs: None,
    }
}

fn config() -> OauthConfig {
    OauthConfig {
        issuer: "https://issuer.example.com".to_owned(),
        client_id: "client".to_owned(),
        scopes: "openid".to_owned(),
        audience: None,
        otel_endpoint: None,
        otel_token: None,
        gateway_url: None,
        // Matches the shipped compiled default (#280 review, P2-3): this
        // fixture does not exercise profile-dependent behaviour either
        // way (the assertion below is about the "neither flag set"
        // error, not about `daemon` vs. `manual`), so there is no reason
        // for it to disagree with what a real `configure` actually
        // defaults to.
        profile: crate::profile::Profile::Manual,
        profile_explicit: Some(crate::profile::Profile::Manual),
        copilot_spool_path: None,
        otel_headers_debounce_ms: 240_000,
        open_browser: false,
        callback_port: None,
        callback_bind: std::net::IpAddr::from([127, 0, 0, 1]),
        token_exchange: None,
        last_no_claude: false,
        last_no_codex: false,
        last_no_vscode: false,
        last_codex_telemetry_only: false,
    }
}

/// THE regression test for the bug this module fixes. Neither flag set
/// used to be a silent no-op (`Ok(())`, nothing written, nothing
/// returned) -- exactly what a developer who explicitly ran `configure`
/// and got total silence hit in production. It must now be a loud,
/// non-zero-exit error that names both flags, so `configure` propagates
/// it (this function's caller) while `login` still only warns (see the
/// comment on `login`'s call site).
#[test]
fn configure_fails_loudly_when_neither_otel_endpoint_nor_gateway_url_is_set() {
    let error = apply_telemetry(&config(), &session(), ClientOptOut::default(), true)
        .expect_err("neither flag set must be a hard error, not a silent no-op");
    let rendered = format!("{error:#}");
    assert!(
        rendered.contains("--otel-endpoint"),
        "must name the OTEL flag so the developer knows what to supply, got: {rendered}"
    );
    assert!(
        rendered.contains("--gateway-url"),
        "must name the gateway flag so the developer knows what to supply, got: {rendered}"
    );
}

/// #280 review round 2: the `daemon`-profile refusal must happen before
/// ANY of `apply_telemetry`'s three daemon-profile side effects, not just
/// at the last one (`schedule::daemon::apply`, which used to catch this
/// too late and have its `Err` downgraded to a warning by its caller --
/// see the removed call site's old comment). `serve_otel_supported` is
/// passed as `false` explicitly (not read from `cli::serve_otel_is_supported()`
/// -- see `apply_telemetry`'s parameter doc): this test asserts the
/// *unsupported* branch, which must keep working once #268 ships and this
/// build's own answer flips to `true`, not just until then.
///
/// Hermetic by construction, not just by assertion: the chokepoint bails
/// before `apply_telemetry` ever reads `$HOME`, so this needs no
/// filesystem fixture -- if the reorder ever regressed and let the
/// function reach the `$HOME` lookup first, this test would fail on a
/// missing/unwritable home directory rather than on the assertion below,
/// which is itself a second, independent tripwire for the same bug.
#[test]
fn configure_refuses_the_daemon_profile_atomically_when_serve_otel_is_unsupported() {
    let config = OauthConfig {
        otel_endpoint: Some("https://otel.example".to_owned()),
        profile: crate::profile::Profile::Daemon,
        ..config()
    };
    let error = apply_telemetry(&config, &session(), ClientOptOut::default(), false)
        .expect_err("daemon profile on a build with no serve verb must refuse, not warn");
    let rendered = format!("{error:#}");
    assert!(
        rendered.contains("serve --otel"),
        "must name the missing capability, got: {rendered}"
    );
    assert!(
        rendered.contains("--profile manual"),
        "must name the escape hatch, got: {rendered}"
    );
}
