//! What is testable here is the *rendering* and the *argv*. `systemctl` and
//! `launchctl` are not invoked from any test: a unit test that reloads the
//! developer's own systemd tree is a test with side effects on the machine
//! running it, and CI has no user session to reload. So [`systemd::units`] and
//! [`launchd::plist`] exist as pure functions and the activation round trip is
//! verified live, on a real machine, in the PR that ships it.
//!
//! The property that matters most is the last test: the path Copilot is told
//! to write and the path the timer is told to drain come from one resolution,
//! so they cannot disagree. Before this module they were two separate
//! copy-pastes in a runbook, which is exactly how they got out of step.

use std::path::Path;

use super::*;

fn config() -> OauthConfig {
    OauthConfig {
        issuer: "https://issuer.example.com".to_owned(),
        client_id: "cli".to_owned(),
        scopes: "openid".to_owned(),
        audience: None,
        otel_endpoint: Some("https://otel.example.com".to_owned()),
        otel_token: None,
        gateway_url: None,
        // `manual` -- not the fixture's `daemon` value elsewhere -- because
        // this whole module's `Invocation` is `manual`-only now (#270 AC5).
        profile: crate::profile::Profile::Manual,
        copilot_spool_path: Some("/state/copilot-otel.jsonl".to_owned()),
        otel_headers_debounce_ms: 240_000,
        open_browser: false,
        token_exchange: None,
    }
}

#[test]
fn no_collector_means_no_schedule_rather_than_one_pointing_nowhere() {
    let mut config = config();
    config.otel_endpoint = None;
    let resolved = Invocation::resolve(&config).expect("resolve");
    assert!(
        resolved.is_none(),
        "a timer with no collector would wake every five minutes to fail"
    );
}

/// #270 AC5: switching to `daemon` removes this timer even with a collector
/// still configured -- the daemon (once #272 rewires Copilot) is what
/// forwards the spool under that profile, so a `manual`-only timer draining
/// the same file would double-export.
#[test]
fn daemon_profile_means_no_schedule_even_with_a_collector_configured() {
    let mut config = config();
    config.profile = crate::profile::Profile::Daemon;
    assert!(Invocation::resolve(&config).expect("resolve").is_none());
}

#[test]
fn the_invocation_carries_every_flag_in_claps_order() {
    let invocation = Invocation::resolve(&config())
        .expect("resolve")
        .expect("a collector is configured");
    assert_eq!(
        invocation.args,
        vec![
            "--issuer",
            "https://issuer.example.com",
            "--client-id",
            "cli",
            "--otel-endpoint",
            "https://otel.example.com",
            "--copilot-spool-path",
            "/state/copilot-otel.jsonl",
            "copilot",
            "push",
        ],
        "globals must precede the subcommand -- see tests/cli_arg_order.rs"
    );
}

#[test]
fn exec_start_quotes_each_word_and_escapes_systemds_percent() {
    // A `%` in a path is expanded by systemd even inside quotes, so an
    // unescaped one silently rewrites the command. `%%` is the only escape.
    let mut config = config();
    config.copilot_spool_path = Some("/state/100%/spool.jsonl".to_owned());
    let invocation = Invocation::resolve(&config)
        .expect("resolve")
        .expect("some");
    let units = systemd::units(Path::new("/home/dev"), &invocation).expect("render");
    let (path, service) = units.first().expect("the service unit");

    assert!(path.ends_with("governance-auth-copilot-push.service"));
    assert!(
        service.contains(r#""--copilot-spool-path" "/state/100%%/spool.jsonl""#),
        "every word quoted and `%` doubled; got:\n{service}"
    );
    assert!(
        service.contains("ExecStart=\""),
        "argv[0] is quoted too, so a $HOME with a space still resolves"
    );
    assert!(
        service.contains("TimeoutStartSec=240"),
        "a Type=oneshot unit defaults to an INFINITE timeout"
    );
}

#[test]
fn the_timer_fires_on_the_interval_the_drain_was_designed_for() {
    let invocation = Invocation::resolve(&config())
        .expect("resolve")
        .expect("some");
    let units = systemd::units(Path::new("/home/dev"), &invocation).expect("render");
    let (path, timer) = units.get(1).expect("the timer unit");

    assert!(path.ends_with("governance-auth-copilot-push.timer"));
    assert!(timer.contains("OnUnitActiveSec=300s"), "got:\n{timer}");
    assert!(
        timer.contains("WantedBy=timers.target"),
        "without [Install] `systemctl --user enable` has nothing to link"
    );
    assert!(
        // Not `contains`: the template's own comment explains why the key is
        // absent, and a substring check would match that explanation.
        !timer
            .lines()
            .any(|line| line.trim_start().starts_with("Persistent=")),
        "systemd.timer(5) defines Persistent= only for calendar timers, so on a monotonic one \
         it is silently ignored; got:\n{timer}"
    );
}

#[test]
fn the_plist_escapes_xml_rather_than_emitting_a_broken_agent() {
    // launchd refuses to bootstrap a plist it cannot parse, and a bare `&` in
    // a query string is exactly that -- the job would then never run, silently.
    let mut config = config();
    config.otel_endpoint = Some("https://otel.example.com/?a=1&b=2".to_owned());
    let invocation = Invocation::resolve(&config)
        .expect("resolve")
        .expect("some");
    let (path, plist) = launchd::plist(Path::new("/Users/dev"), &invocation).expect("render");

    assert!(path.ends_with("digital.camer.ai.governance-auth.copilot-push.plist"));
    assert!(
        plist.contains("<string>https://otel.example.com/?a=1&amp;b=2</string>"),
        "got:\n{plist}"
    );
    assert!(
        plist.contains("<string>--copilot-spool-path</string>"),
        "each argv word is its own <string>, not one shell line"
    );
    assert!(plist.contains("<integer>300</integer>"));
    assert!(
        plist.contains("/Users/dev/Library/Logs/governance-auth/governance-auth.log"),
        "launchd has no journal, so stderr must land where Console.app looks \
         -- and in the SAME rotated file `crate::logging` writes, not a \
         second, unbounded one beside it"
    );
}

#[test]
fn the_timer_drains_exactly_the_file_copilot_is_told_to_write() {
    // The conservation property of this whole feature. `otel::configure_vscode`
    // writes `outfile` from `OtelSettings::copilot_spool`, and both come from
    // `copilot::resolve_spool_path` on the same config -- so a developer who
    // sets `--copilot-spool-path` cannot end up with Copilot writing one file
    // and the timer draining another. Break `Invocation::resolve` to read the
    // raw `config.copilot_spool_path` instead and this fails on the default
    // (`None`), which is the case a runbook copy-paste always got wrong.
    let mut config = config();
    config.copilot_spool_path = None;
    let expected = crate::copilot::resolve_spool_path(&config).expect("the compiled default");
    let invocation = Invocation::resolve(&config)
        .expect("resolve")
        .expect("some");

    assert!(
        invocation
            .args
            .contains(&expected.to_string_lossy().into_owned()),
        "the timer must drain {}, got {:?}",
        expected.display(),
        invocation.args
    );
}

/// The classification the *row* depends on and no other test reaches. Written
/// after an exit-code-only implementation of `survey` passed the entire suite:
/// `systemctl --user is-active` exits non-zero for a stopped timer AND for a
/// machine with no user manager, so the code alone cannot tell them apart.
///
/// Falsification: replace the body of `systemd::classify` with
/// `Some(!stdout.trim().is_empty())` and the "could not be asked" case fails --
/// checked, and it is the only test in the crate that does.
mod staleness;
mod survey;
