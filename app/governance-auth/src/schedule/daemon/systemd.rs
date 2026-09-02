//! Linux: the daemon as a `Type=simple`, `Restart=on-failure` user service.
//!
//! No `.timer`: unlike the drain this must stay running, not wake
//! periodically, so `enable --now` is issued directly against the
//! `.service` unit.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};

use super::{DAEMON_UNIT, Invocation};
use crate::schedule::{
    Schedule, run,
    systemd::{classify, write},
};

/// Same directory the drain's units live in -- one systemd user tree, two
/// unrelated unit stems.
fn unit_dir(home: &Path) -> PathBuf {
    home.join(".config").join("systemd").join("user")
}

fn service_path(home: &Path) -> PathBuf {
    unit_dir(home).join(format!("{DAEMON_UNIT}.service"))
}

fn service_unit() -> String {
    format!("{DAEMON_UNIT}.service")
}

/// The unit, rendered but not written -- same split as
/// [`crate::schedule::systemd`]'s own `units`, for the same reason: the
/// `ExecStart=` quoting is testable, the `systemctl` round trip is not.
fn unit(home: &Path, invocation: &Invocation) -> Result<(PathBuf, String)> {
    let mut argv = vec![invocation.program.clone()];
    argv.extend(invocation.args.iter().cloned());
    let body = crate::templates::daemon::systemd_service(&argv)
        .context("rendering the daemon's systemd service unit")?;
    Ok((service_path(home), body))
}

pub fn install(home: &Path, invocation: &Invocation) -> Result<()> {
    let dir = unit_dir(home);
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let (path, body) = unit(home, invocation)?;
    write(&path, &body)?;
    eprintln!("Configured: {}", path.display());

    // ⚠️ Same two-step as the drain's install, and neither is optional here
    // either: without the reload systemd runs the unit it parsed at login,
    // so a changed endpoint is written to disk and ignored; without
    // `enable --now` the unit exists and never starts.
    run("systemctl", &["--user", "daemon-reload"])?;
    run("systemctl", &["--user", "enable", "--now", &service_unit()])?;
    eprintln!("Daemon installed: forwarding via {}.", service_unit());
    Ok(())
}

pub fn remove(home: &Path) -> Result<()> {
    let path = service_path(home);
    if !path.is_file() {
        return Ok(());
    }
    // Before the file goes: `disable` needs the unit to still exist to
    // resolve its `[Install]` section.
    let stopped = run(
        "systemctl",
        &["--user", "disable", "--now", &service_unit()],
    );
    fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    eprintln!(
        "Removed (manual profile, or no collector configured): {}",
        path.display()
    );
    let _ = run("systemctl", &["--user", "daemon-reload"]);
    stopped
}

pub fn survey(home: &Path) -> Schedule {
    let path = service_path(home);
    let installed = path.is_file();
    // Reuses `systemd::classify` -- the same three-valued read `status`
    // (#271) already needs, not a second copy of it. See this module's
    // parent's doc.
    let active = std::process::Command::new("systemctl")
        .args(["--user", "is-active", &service_unit()])
        .output()
        .ok()
        .and_then(|output| classify(&String::from_utf8_lossy(&output.stdout)));
    Schedule {
        path,
        installed,
        active,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_unit_names_a_fixed_daemon_path_distinct_from_the_drain() {
        let home = Path::new("/home/dev");
        assert_eq!(
            service_path(home),
            home.join(".config/systemd/user/governance-auth-serve-otel.service")
        );
        assert_ne!(
            service_path(home),
            crate::schedule::systemd::timer_path(home),
            "the daemon's unit and the drain's timer must never collide"
        );
    }

    #[test]
    fn the_rendered_unit_carries_every_argv_word() {
        let (_, body) = unit(
            Path::new("/home/dev"),
            &Invocation {
                program: "/usr/local/bin/governance-auth".to_owned(),
                args: vec!["--issuer".to_owned(), "https://auth.example".to_owned()],
            },
        )
        .expect("render");
        assert!(body.contains("\"/usr/local/bin/governance-auth\""));
        assert!(body.contains("\"--issuer\""));
        assert!(body.contains("\"https://auth.example\""));
    }
}
