use super::reload;

#[test]
fn unchanged_daemon_restarts_without_unregistering() {
    let mut commands = Vec::new();
    reload("gui/502", "/agent.plist", true, |args| {
        commands.push(args.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        Ok(())
    })
    .unwrap();
    assert_eq!(
        commands,
        vec![vec![
            "kickstart",
            "-k",
            "gui/502/digital.camer.ai.governance-auth.serve-otel"
        ]]
    );
}

#[test]
fn absent_daemon_is_registered_after_kickstart_fails() {
    let mut commands = Vec::new();
    reload("gui/502", "/agent.plist", true, |args| {
        commands.push(args[0].to_string());
        if args[0] == "kickstart" || args[0] == "bootout" {
            anyhow::bail!("service absent");
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(commands, ["kickstart", "bootout", "bootstrap"]);
}

#[test]
fn changed_configuration_is_registered_and_failure_surfaces() {
    let mut commands = Vec::new();
    let result = reload("gui/502", "/agent.plist", false, |args| {
        commands.push(args[0].to_string());
        if args[0] == "bootstrap" {
            anyhow::bail!("registration failed");
        }
        Ok(())
    });
    assert!(result.is_err());
    assert_eq!(commands, ["bootout", "bootstrap"]);
}
