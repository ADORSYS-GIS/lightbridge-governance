//! macOS: a `KeepAlive` LaunchAgent in the user's GUI domain.
//!
//! `gui/<uid>`, not `user/<uid>` -- same reasoning as
//! [`crate::schedule::launchd`]'s own module doc: the job spawns
//! `governance-auth`, which is meant to run while the developer is logged
//! in, and a job bootstrapped into the background `user` domain outlives
//! logout.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};

use super::{DAEMON_LABEL, Invocation};
use crate::schedule::{Schedule, run};

fn plist_path(home: &Path) -> PathBuf {
    home.join("Library")
        .join("LaunchAgents")
        .join(format!("{DAEMON_LABEL}.plist"))
}

/// Same file [`crate::schedule::launchd`]'s own `log_path` uses -- one
/// rotating, `O_APPEND` log for every job this binary installs, per that
/// module's doc on why sharing it is safe.
fn log_path(home: &Path) -> PathBuf {
    crate::logging::path_in(home)
}

/// The agent, rendered but not written -- same split as
/// [`crate::schedule::launchd`]'s own `plist`.
fn plist(home: &Path, invocation: &Invocation) -> Result<(PathBuf, String)> {
    let mut argv = vec![invocation.program.clone()];
    argv.extend(invocation.args.iter().cloned());
    let body = crate::templates::daemon::launchd_plist(
        DAEMON_LABEL,
        &argv,
        &log_path(home).to_string_lossy(),
    )
    .context("rendering the daemon's launchd agent")?;
    Ok((plist_path(home), body))
}

pub fn install(home: &Path, invocation: &Invocation) -> Result<()> {
    let (path, body) = plist(home, invocation)?;
    for dir in [path.parent(), log_path(home).parent()]
        .into_iter()
        .flatten()
    {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    write(&path, &body)?;
    eprintln!("Configured: {}", path.display());

    // ⚠️ Same non-idempotent `bootstrap` trap as the drain's install: on an
    // already-loaded label it fails and leaves the OLD argv running, so
    // unload first, ignoring the failure that means "it was not loaded".
    let domain = domain(&path)?;
    let _ = run(
        "launchctl",
        &["bootout", &format!("{domain}/{DAEMON_LABEL}")],
    );
    run(
        "launchctl",
        &["bootstrap", &domain, &path.to_string_lossy()],
    )?;
    eprintln!("Daemon installed: forwarding via {DAEMON_LABEL}.");
    Ok(())
}

pub fn remove(home: &Path) -> Result<()> {
    let path = plist_path(home);
    if !path.exists() {
        return Ok(());
    }
    let domain = domain(&path)?;
    let booted = run(
        "launchctl",
        &["bootout", &format!("{domain}/{DAEMON_LABEL}")],
    );
    fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    eprintln!(
        "Removed (manual profile, or no collector configured): {}",
        path.display()
    );
    booted
}

#[expect(
    dead_code,
    reason = "wired in by #271 (status); no caller yet on this branch"
)]
pub fn survey(home: &Path) -> Schedule {
    let path = plist_path(home);
    let installed = path.is_file();
    // `launchctl print` exits 0 only for a label the domain actually holds;
    // a failure to resolve the domain at all is `None` -- "could not ask",
    // never rendered as "stopped". Same rule as the drain's own survey.
    let active = domain(&path).ok().and_then(|domain| {
        std::process::Command::new("launchctl")
            .args(["print", &format!("{domain}/{DAEMON_LABEL}")])
            .output()
            .ok()
            .map(|output| output.status.success())
    });
    Schedule {
        path,
        installed,
        active,
    }
}

/// `gui/<uid>`, read off the filesystem rather than `getuid(2)` -- same
/// reasoning as [`crate::schedule::launchd`]'s own `domain`. Duplicated
/// rather than imported: that one is private to a sibling module, not a
/// descendant of this one, so it is not reachable from here (unlike
/// `systemd::classify`, which this tree's `daemon::systemd` reuses because
/// it IS a descendant of the module that defines it).
#[cfg(unix)]
fn domain(path: &Path) -> Result<String> {
    use std::os::unix::fs::MetadataExt;

    let dir = path.parent().unwrap_or(path);
    let uid = fs::metadata(dir)
        .with_context(|| format!("reading the owner of {}", dir.display()))?
        .uid();
    Ok(format!("gui/{uid}"))
}

#[cfg(not(unix))]
fn domain(_path: &Path) -> Result<String> {
    anyhow::bail!("launchd is only reachable on unix")
}

/// tmp-then-rename: launchd refuses to bootstrap a plist it cannot parse.
fn write(path: &Path, body: &str) -> Result<()> {
    let tmp = path.with_extension("governance-auth-tmp");
    fs::write(&tmp, body).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} to {}", tmp.display(), path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plist_names_a_fixed_daemon_label_distinct_from_the_drain() {
        assert_ne!(
            DAEMON_LABEL,
            crate::schedule::LABEL,
            "the daemon's plist and the drain's must never collide"
        );
        let home = Path::new("/home/dev");
        assert_eq!(
            plist_path(home),
            home.join("Library/LaunchAgents")
                .join(format!("{DAEMON_LABEL}.plist"))
        );
    }
}
