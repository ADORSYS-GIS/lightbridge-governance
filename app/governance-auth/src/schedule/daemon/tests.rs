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
    let invocation = Invocation::resolve(&config(Profile::Daemon, Some("https://otel.example")))
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
    assert!(Invocation::resolve(&config(Profile::Manual, Some("https://otel.example"))).is_none());
}

/// The same "nothing to point at" rule the drain's own `Invocation::resolve`
/// uses -- a daemon profile with no collector installs a service that
/// forwards nowhere, which `schedule::apply`'s doc already treats as worse
/// than not installing one.
#[test]
fn daemon_profile_with_no_collector_resolves_to_nothing() {
    assert!(Invocation::resolve(&config(Profile::Daemon, None)).is_none());
}

/// Every word `token`/`otel headers` need to mint a fresh bearer (#268 AC3)
/// is present, in order -- falsified by checking each flag has its value
/// immediately after it, not just that both strings appear somewhere in the
/// argv.
#[test]
fn the_invocation_carries_issuer_and_client_id_for_token_minting() {
    let invocation = Invocation::resolve(&config(Profile::Daemon, Some("https://otel.example")))
        .expect("resolve");
    let position = |flag: &str| invocation.args.iter().position(|word| word == flag);
    let issuer_at = position("--issuer").expect("--issuer present");
    let client_id_at = position("--client-id").expect("--client-id present");
    assert_eq!(invocation.args[issuer_at + 1], "https://issuer.example");
    assert_eq!(invocation.args[client_id_at + 1], "cli");
}

#[test]
fn output_within_returns_promptly_for_a_command_that_finishes() {
    let output = output_within("true", &[]).expect("a real, fast command must succeed");
    assert!(output.status.success());
}

/// Confirmed live, not just plausible: `status` making a SECOND unbounded
/// shell-out (the first is the Copilot drain's own survey) meant a hung
/// `systemctl`/`launchctl` -- seen in practice when the session bus is gone
/// -- hung `status` outright rather than showing a stale row. `sleep 30`
/// stands in for that hang; the child is left running (this binary denies
/// `unsafe_code`, so there is no bounded way to kill it), but this function
/// must not wait for it.
#[test]
fn output_within_gives_up_on_a_command_that_outlives_the_timeout() {
    let start = std::time::Instant::now();
    let result = output_within("sleep", &["30"]);
    assert!(
        result.is_none(),
        "a command that outlives the timeout must report `None`, not a stale result"
    );
    assert!(
        start.elapsed() < std::time::Duration::from_secs(5),
        "must give up at ASK_TIMEOUT (2s), not wait out the full 30s command: took {:?}",
        start.elapsed()
    );
}
