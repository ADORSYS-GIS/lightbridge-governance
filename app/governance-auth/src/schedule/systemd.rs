//! Linux: a `Type=oneshot` service driven by a monotonic user timer.
//!
//! A **user** timer, never a system one. The drain reads a spool under the
//! developer's `$HOME` and refreshes their session, so it has to run as them;
//! a system unit would need the credential in a machine-wide location, which
//! is the one place a refresh token must not be.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};

use super::{INTERVAL_SECONDS, Invocation, Schedule, UNIT, run};

/// systemd's own search path for per-user units. Not `$XDG_CONFIG_HOME`:
/// systemd reads `~/.config/systemd/user` regardless of that variable, so
/// honouring it here would put the units somewhere systemd never looks.
fn unit_dir(home: &Path) -> PathBuf {
    home.join(".config").join("systemd").join("user")
}

fn service_path(home: &Path) -> PathBuf {
    unit_dir(home).join(format!("{UNIT}.service"))
}

pub fn timer_path(home: &Path) -> PathBuf {
    unit_dir(home).join(format!("{UNIT}.timer"))
}

/// `TimeoutStartSec=`. Shorter than the interval so a stuck wake is killed
/// before the next one is due and they cannot pile up behind the drain's lock.
const TIMEOUT_SECONDS: u64 = 240;

/// The two unit files, rendered but not written.
///
/// Split from [`install`] so the rendering -- which is where the `ExecStart=`
/// quoting bugs live -- is reachable from a test that does not touch the
/// machine's real systemd.
pub(super) fn units(home: &Path, invocation: &Invocation) -> Result<Vec<(PathBuf, String)>> {
    let mut argv = vec![invocation.program.clone()];
    argv.extend(invocation.args.iter().cloned());
    Ok(vec![
        (
            service_path(home),
            crate::templates::systemd_service(&argv, TIMEOUT_SECONDS)
                .context("rendering the systemd service unit")?,
        ),
        (
            timer_path(home),
            crate::templates::systemd_timer(INTERVAL_SECONDS)
                .context("rendering the systemd timer unit")?,
        ),
    ])
}

pub fn install(home: &Path, invocation: &Invocation) -> Result<()> {
    let dir = unit_dir(home);
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    for (path, body) in units(home, invocation)? {
        write(&path, &body)?;
        eprintln!("Configured: {}", path.display());
    }

    // ⚠️ Both, in this order, and neither is optional. Without the reload
    // systemd runs the units it parsed at login, so a changed endpoint is
    // written to disk and ignored; without `enable --now` the timer exists and
    // never fires, which is the exact "looks installed, exports nothing" state
    // this module was written to remove.
    run("systemctl", &["--user", "daemon-reload"])?;
    run("systemctl", &["--user", "enable", "--now", &timer_unit()])?;
    eprintln!(
        "Copilot drain scheduled: every {INTERVAL_SECONDS}s via {}.",
        timer_unit()
    );
    Ok(())
}

pub fn remove(home: &Path) -> Result<()> {
    let paths = [service_path(home), timer_path(home)];
    if !paths.iter().any(|path| path.exists()) {
        return Ok(());
    }
    // Before the files go: `disable` needs the unit to still exist to resolve
    // its `[Install]` section, and a timer stopped after its unit file is gone
    // stays loaded until the next reboot.
    let stopped = run("systemctl", &["--user", "disable", "--now", &timer_unit()]);
    for path in paths {
        fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
        // Not "(no collector configured)": #270 gave `Invocation::resolve`
        // a second reason to return `None` (the `daemon` profile), and a
        // developer switching profiles with a real `--otel-endpoint` set
        // would read that claim as flatly wrong. Neutral wording covers
        // both without guessing which one applies here.
        eprintln!("Removed (nothing to schedule): {}", path.display());
    }
    let _ = run("systemctl", &["--user", "daemon-reload"]);
    stopped
}

pub fn survey(home: &Path) -> Schedule {
    let path = timer_path(home);
    let installed = path.is_file();
    // `is-active` exits non-zero for an inactive timer AND for every reason
    // the question could not be asked, so the exit code alone cannot tell
    // "stopped" from "no user manager here". The stdout word can: systemd
    // prints a state (`inactive`, `failed`, ...) in the first case and nothing
    // useful in the second.
    let active = std::process::Command::new("systemctl")
        .args(["--user", "is-active", &timer_unit()])
        .output()
        .ok()
        .and_then(|output| classify(&String::from_utf8_lossy(&output.stdout)));
    Schedule {
        path,
        installed,
        active,
    }
}

/// `systemctl --user is-active`'s stdout, turned into the three-valued answer
/// [`Schedule::active`] promises.
///
/// A free function so it is reachable from a test without a systemd to ask --
/// which matters, because the wrong implementation passes every other test in
/// this crate. Reading the *exit code* instead conflates "the timer is stopped"
/// with "there is no user manager here to answer", and the second is most of a
/// container's population.
pub(super) fn classify(stdout: &str) -> Option<bool> {
    match stdout.trim() {
        "active" => Some(true),
        // Measured against a live systemd, not inferred: with no user manager
        // reachable, `systemctl --user is-active` prints **nothing** on stdout,
        // exits 1, and puts "Failed to connect to user scope bus ...
        // $DBUS_SESSION_BUS_ADDRESS and $XDG_RUNTIME_DIR not defined" on
        // stderr. A stopped timer prints `inactive` and exits 3. Same non-zero
        // code, different stdout -- which is why this reads the word.
        "" => None,
        _ => Some(false),
    }
}

fn timer_unit() -> String {
    format!("{UNIT}.timer")
}

/// tmp-then-rename, like every other file this binary writes: systemd may be
/// reading the directory at any moment, and a half-written unit is a parse
/// error that disables the timer rather than a transient one.
///
/// Mode is left to the umask, unlike `otel::write_atomically`'s 0600 -- these
/// files carry no credential, only flags, and a 0600 unit is a surprising
/// thing to find in a systemd tree.
///
/// `pub(super)`, like [`tmp_path`] below it: [`super::daemon::systemd`] reuses
/// this rather than keeping its own copy (#280 review round 2) -- the copy it
/// used to keep had regressed the naked-`with_extension` fix [`tmp_path`]'s
/// own doc explains.
pub(super) fn write(path: &Path, body: &str) -> Result<()> {
    let tmp = tmp_path(path);
    fs::write(&tmp, body).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} to {}", tmp.display(), path.display()))
}

/// ⚠️ APPENDS the marker, never `with_extension`. Both units live in one
/// directory and differ only by extension, so `with_extension` -- which
/// REPLACES it -- hands `…push.service` and `…push.timer` the identical temp
/// name. Sequentially that is merely ugly; two `configure` runs at once can
/// interleave write-tmp/write-tmp/rename/rename and land the timer's body in
/// the service file. systemd ignores a file with no unit suffix, so the temp
/// name itself is safe to leave in that directory for the instant it exists.
pub(super) fn tmp_path(path: &Path) -> PathBuf {
    let name = path.file_name().map_or_else(
        || UNIT.to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    path.with_file_name(format!("{name}.governance-auth-tmp"))
}
