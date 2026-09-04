//! [`Invocation::resolve`]'s gating -- the one piece of logic unique to this
//! module rather than delegated to `systemd`/`launchd`. Everything else
//! (unit rendering, quoting, the three-valued survey) is covered in those
//! submodules and in `templates::daemon`.

use super::*;

fn config(profile: Profile, otel_endpoint: Option<&str>) -> OauthConfig {
    OauthConfig {
        issuer: "https://issuer.example".to_owned(),
        client_id: "cli".to_owned(),
        scopes: "openid".to_owned(),
        audience: None,
        otel_endpoint: otel_endpoint.map(str::to_owned),
        otel_token: None,
        gateway_url: None,
        profile,
        copilot_spool_path: None,
        otel_headers_debounce_ms: 240_000,
        open_browser: false,
        token_exchange: None,
    }
}

#[test]
fn daemon_profile_with_a_collector_resolves_to_an_invocation() {
    let invocation =
        Invocation::resolve(&config(Profile::Daemon, Some("https://otel.example")), true)
            .expect("supported, so this must not refuse")
            .expect("daemon profile with a collector must resolve");
    assert!(invocation.args.contains(&"--otel-endpoint".to_owned()));
    assert!(invocation.args.contains(&"https://otel.example".to_owned()));
    assert!(
        invocation
            .args
            .ends_with(&["serve".to_owned(), "--otel".to_owned()])
    );
}

/// AC5's "switching to manual removes the daemon service" -- falsified by
/// checking `resolve` itself, since [`apply`] does nothing but branch on
/// this being `None`.
#[test]
fn manual_profile_resolves_to_nothing_even_with_a_collector_configured() {
    let resolved =
        Invocation::resolve(&config(Profile::Manual, Some("https://otel.example")), true)
            .expect("manual never refuses");
    assert!(resolved.is_none());
}

/// The same "nothing to point at" rule the drain's own `Invocation::resolve`
/// uses -- a daemon profile with no collector installs a service that
/// forwards nowhere, which `schedule::apply`'s doc already treats as worse
/// than not installing one.
#[test]
fn daemon_profile_with_no_collector_resolves_to_nothing() {
    let resolved = Invocation::resolve(&config(Profile::Daemon, None), true)
        .expect("nothing to point at never refuses -- there is nothing to install either way");
    assert!(resolved.is_none());
}

/// #280 review, P1-1: the exact bug this check exists to prevent -- a
/// `daemon` profile with a collector configured, on a build that does not
/// have `serve --otel` yet, must refuse rather than install a service that
/// can only crash-loop.
#[test]
fn daemon_profile_refuses_when_serve_otel_is_not_supported() {
    let error = Invocation::resolve(
        &config(Profile::Daemon, Some("https://otel.example")),
        false,
    )
    .expect_err("must refuse, not install a crash-looping service");
    let message = format!("{error:#}");
    assert!(message.contains("#268"), "names the reason: {message}");
}

/// The refusal is specific to `daemon` needing to install something --
/// `manual` never resolves to an invocation regardless, so there is nothing
/// for the missing command to block.
#[test]
fn manual_profile_never_refuses_even_when_serve_otel_is_not_supported() {
    let resolved = Invocation::resolve(
        &config(Profile::Manual, Some("https://otel.example")),
        false,
    )
    .expect("manual installs nothing, so an unsupported serve --otel is irrelevant to it");
    assert!(resolved.is_none());
}

/// Every word `token`/`otel headers` need to mint a fresh bearer (#268 AC3)
/// is present, in order -- falsified by checking each flag has its value
/// immediately after it, not just that both strings appear somewhere in the
/// argv.
#[test]
fn the_invocation_carries_issuer_and_client_id_for_token_minting() {
    let invocation =
        Invocation::resolve(&config(Profile::Daemon, Some("https://otel.example")), true)
            .expect("supported")
            .expect("resolve");
    let position = |flag: &str| invocation.args.iter().position(|word| word == flag);
    let issuer_at = position("--issuer").expect("--issuer present");
    let client_id_at = position("--client-id").expect("--client-id present");
    assert_eq!(invocation.args[issuer_at + 1], "https://issuer.example");
    assert_eq!(invocation.args[client_id_at + 1], "cli");
}
