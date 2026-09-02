//! macOS: a `StartInterval` LaunchAgent in the user's GUI domain.
//!
//! `gui/<uid>`, not `user/<uid>`: the job spawns `governance-auth`, which
//! refreshes a session out of the user's keychain-adjacent state directory and
//! is meant to run while they are logged in. A job bootstrapped into the
//! background `user` domain runs before login and outlives logout, which is a
//! larger claim than this needs and a worse place for a credential.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};

use super::{INTERVAL_SECONDS, Invocation, LABEL, Schedule, run};

fn plist_path(home: &Path) -> PathBuf {
    home.join("Library")
        .join("LaunchAgents")
        .join(format!("{LABEL}.plist"))
}

/// launchd has no journal, so the job's stderr has to land somewhere a human
/// can find. `~/Library/Logs` is where Console.app already looks.
fn log_path(home: &Path) -> PathBuf {
    home.join("Library")
        .join("Logs")
        .join("governance-auth-copilot-push.log")
}

/// The agent, rendered but not written. Split from [`install`] for the same
/// reason as [`super::systemd::units`]: the XML escaping is testable, the
/// `launchctl` round trip is not.
pub(super) fn plist(home: &Path, invocation: &Invocation) -> Result<(PathBuf, String)> {
    let mut argv = vec![invocation.program.clone()];
    argv.extend(invocation.args.iter().cloned());
    let body = crate::templates::launchd_plist(
        LABEL,
        &argv,
        INTERVAL_SECONDS,
        &log_path(home).to_string_lossy(),
    )
    .context("rendering the launchd agent")?;
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

    // ⚠️ `bootstrap` alone is not idempotent: on an already-loaded label it
    // fails with `Bootstrap failed: 5: Input/output error` and leaves the OLD
    // argv running, so a changed endpoint would be written to disk and never
    // used. Unload first, ignoring the failure that means "it was not loaded".
    let domain = domain(&path)?;
    let _ = run("launchctl", &["bootout", &format!("{domain}/{LABEL}")]);
    run(
        "launchctl",
        &["bootstrap", &domain, &path.to_string_lossy()],
    )?;
    eprintln!("Copilot drain scheduled: every {INTERVAL_SECONDS}s via {LABEL}.");
    Ok(())
}

pub fn remove(home: &Path) -> Result<()> {
    let path = plist_path(home);
    if !path.exists() {
        return Ok(());
    }
    let domain = domain(&path)?;
    let booted = run("launchctl", &["bootout", &format!("{domain}/{LABEL}")]);
    fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    eprintln!("Removed (no collector configured): {}", path.display());
    booted
}

pub fn survey(home: &Path) -> Schedule {
    let path = plist_path(home);
    let installed = path.is_file();
    // `launchctl print` exits 0 only for a label the domain actually holds.
    // A failure to resolve the domain at all is `None` -- "could not ask",
    // not "not loaded". See `Schedule::active`.
    let active = domain(&path).ok().and_then(|domain| {
        std::process::Command::new("launchctl")
            .args(["print", &format!("{domain}/{LABEL}")])
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

/// `gui/<uid>`, taken from the owner of a path inside the user's own tree.
///
/// Reading the uid off the filesystem rather than shelling out to `id -u`:
/// this binary denies `unsafe_code`, so `getuid(2)` is unreachable without a
/// libc dependency, and a subprocess to learn our own identity is a subprocess
/// that can fail. The plist's parent is `~/Library/LaunchAgents`, which is
/// owned by the user by construction -- we just created it.
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

/// tmp-then-rename: launchd refuses to bootstrap a plist it cannot parse, and
/// a half-written file is exactly that.
fn write(path: &Path, body: &str) -> Result<()> {
    let tmp = path.with_extension("governance-auth-tmp");
    fs::write(&tmp, body).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} to {}", tmp.display(), path.display()))
}
