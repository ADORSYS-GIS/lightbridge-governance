use super::*;

fn config() -> OauthConfig {
    OauthConfig {
        issuer: "https://issuer.example.com".to_owned(),
        client_id: "client".to_owned(),
        scopes: "openid".to_owned(),
        audience: None,
        otel_endpoint: None,
        otel_token: None,
        gateway_url: None,
        profile: crate::profile::Profile::Daemon,
        copilot_spool_path: None,
        otel_headers_debounce_ms: 240_000,
        open_browser: false,
        token_exchange: None,
    }
}

/// ADR-0016 / #270 AC1: the endpoint substitution and the credential
/// suppression, both in one assertion -- the two are meant to change
/// together (a loopback endpoint with a static bearer attached would be
/// the daemon's whole point undone).
#[test]
fn daemon_profile_points_at_loopback_and_carries_no_credential() {
    let config = OauthConfig {
        otel_endpoint: Some("https://otel.example".to_owned()),
        otel_token: Some("static-secret".to_owned()),
        profile: crate::profile::Profile::Daemon,
        ..config()
    };
    let wiring = TelemetryWiring::resolve(&config);
    assert_eq!(
        wiring.endpoint.as_deref(),
        Some(otel_port::OTEL_LOOPBACK_ENDPOINT)
    );
    assert!(
        wiring.token.is_none(),
        "a static credential must never reach a client under `daemon`"
    );
    assert!(!wiring.wants_headers_helper);
}

/// ADR-0016 / #270 AC2: `manual` must reproduce today's behaviour
/// EXACTLY -- falsified by asserting the real endpoint and the real
/// token survive unchanged, not just that they are "present".
#[test]
fn manual_profile_reproduces_the_real_endpoint_and_token() {
    let config = OauthConfig {
        otel_endpoint: Some("https://otel.example".to_owned()),
        otel_token: Some("static-secret".to_owned()),
        profile: crate::profile::Profile::Manual,
        ..config()
    };
    let wiring = TelemetryWiring::resolve(&config);
    assert_eq!(wiring.endpoint.as_deref(), Some("https://otel.example"));
    assert_eq!(
        wiring.token.map(|token| token.expose().clone()),
        Some("static-secret".to_owned())
    );
    assert!(wiring.wants_headers_helper);
}

/// "No endpoint configured" must stay "no telemetry wiring at all"
/// under EITHER profile -- `vscode::configure`'s own
/// `settings.endpoint.is_none()` check depends on this holding, not
/// just on `daemon`'s substitution not accidentally manufacturing one.
#[test]
fn no_endpoint_configured_resolves_to_no_endpoint_under_either_profile() {
    for profile in [
        crate::profile::Profile::Daemon,
        crate::profile::Profile::Manual,
    ] {
        let config = OauthConfig {
            otel_endpoint: None,
            profile,
            ..config()
        };
        assert!(
            TelemetryWiring::resolve(&config).endpoint.is_none(),
            "{profile} must not manufacture an endpoint nothing configured"
        );
    }
}

/// Confirmed live (pre-#272): without `copilot_drain_available` gated on
/// the profile, `daemon`'s loopback `endpoint` substitution alone made
/// `vscode::configure`'s old `endpoint.is_none()` gate read as "turn the
/// file exporter on", and Copilot's spool grew forever with the drain
/// that used to empty it removed. `endpoint` itself must still carry the
/// loopback value -- Claude Code and Codex still need it -- so the fix is
/// a second signal, not touching `endpoint`. #272 gave that signal a
/// destination (`copilot_otlp_direct`) instead of leaving it unused.
#[test]
fn daemon_profile_has_an_endpoint_with_direct_otlp_not_the_file_drain() {
    let config = OauthConfig {
        otel_endpoint: Some("https://otel.example".to_owned()),
        profile: crate::profile::Profile::Daemon,
        ..config()
    };
    let wiring = TelemetryWiring::resolve(&config);
    assert!(
        wiring.endpoint.is_some(),
        "Claude Code and Codex still need the loopback endpoint"
    );
    assert!(
        !wiring.copilot_drain_available,
        "the file+drain path is `manual`'s, not `daemon`'s"
    );
    assert!(
        wiring.copilot_otlp_direct,
        "`daemon`'s Copilot path is its own otlp-http exporter at loopback"
    );
}

#[test]
fn manual_profile_has_a_copilot_drain_exactly_when_an_endpoint_is_configured() {
    for (endpoint, expected) in [(Some("https://otel.example"), true), (None, false)] {
        let config = OauthConfig {
            otel_endpoint: endpoint.map(str::to_owned),
            profile: crate::profile::Profile::Manual,
            ..config()
        };
        let wiring = TelemetryWiring::resolve(&config);
        assert_eq!(
            wiring.copilot_drain_available, expected,
            "endpoint = {endpoint:?}"
        );
        assert!(
            !wiring.copilot_otlp_direct,
            "`manual` never uses the direct otlp-http path"
        );
    }
}

#[test]
fn only_one_copilot_path_is_ever_active_at_once() {
    for profile in [
        crate::profile::Profile::Daemon,
        crate::profile::Profile::Manual,
    ] {
        let config = OauthConfig {
            otel_endpoint: Some("https://otel.example".to_owned()),
            profile,
            ..config()
        };
        let wiring = TelemetryWiring::resolve(&config);
        assert_ne!(
            wiring.copilot_drain_available, wiring.copilot_otlp_direct,
            "{profile} must pick exactly one Copilot path, not both or neither"
        );
    }
}
