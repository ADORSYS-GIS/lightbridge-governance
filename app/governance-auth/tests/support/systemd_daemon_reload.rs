use super::ensure_running;

/// The core parity fix: `enable --now` alone never restarts an
/// already-active unit, so this must issue a `restart` unconditionally --
/// not only when the unit was previously stopped -- or a `self update`
/// followed by `configure`/`login` would leave the old binary running.
#[test]
fn always_restarts_even_when_nothing_changed() {
    let mut commands = Vec::new();
    ensure_running("governance-auth-serve-otel.service", |args| {
        commands.push(args.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        Ok(())
    })
    .unwrap();
    assert_eq!(
        commands,
        vec![
            vec!["--user", "daemon-reload"],
            vec!["--user", "enable", "governance-auth-serve-otel.service"],
            vec!["--user", "restart", "governance-auth-serve-otel.service"],
        ]
    );
}

/// A failure at any step must surface, not be swallowed -- an install that
/// reports success while `restart` actually failed would hide exactly the
/// stale-binary condition this function exists to prevent.
#[test]
fn a_restart_failure_surfaces_rather_than_being_swallowed() {
    let mut commands = Vec::new();
    let result = ensure_running("governance-auth-serve-otel.service", |args| {
        commands.push(args[1].to_string());
        if args[1] == "restart" {
            anyhow::bail!("unit failed to restart");
        }
        Ok(())
    });
    assert!(result.is_err());
    assert_eq!(commands, ["daemon-reload", "enable", "restart"]);
}
