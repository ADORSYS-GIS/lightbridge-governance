//! `stale`: would `configure` write this scheduler file differently now?
//!
//! Rendered-vs-written, never a search for the retired spelling -- see
//! [`super::super::staleness`]'s module doc. These write the *current*
//! rendering to disk and then perturb one thing at a time, so a future rename
//! needs no edit here: the "an older version wrote it" case is modelled by
//! substituting the old argv into the body, exactly as an older binary would
//! have produced it.

use std::path::Path;

use super::{super::staleness::stale, Invocation, config};

/// The files this platform would write, for the given argv.
fn render(home: &Path, invocation: &Invocation) -> Vec<(std::path::PathBuf, String)> {
    if super::super::macos() {
        vec![super::super::launchd::plist(home, invocation).expect("render")]
    } else {
        super::super::systemd::units(home, invocation).expect("render")
    }
}

fn invocation() -> Invocation {
    Invocation::resolve(&config())
        .expect("resolve")
        .expect("a collector is configured")
}

/// What this version writes.
fn rendered(home: &Path) -> Vec<(std::path::PathBuf, String)> {
    render(home, &invocation())
}

fn write(bodies: &[(std::path::PathBuf, String)]) {
    for (path, body) in bodies {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, body).expect("write");
    }
}

#[test]
fn nothing_installed_is_unknown_rather_than_current() {
    let home = crate::managed::testutil::tempdir();
    assert_eq!(
        stale(home.path(), &config()),
        None,
        "a missing unit is the drain row's `not scheduled` branch, not this one"
    );
}

#[test]
fn what_this_version_writes_is_not_stale() {
    let home = crate::managed::testutil::tempdir();
    write(&rendered(home.path()));
    assert_eq!(stale(home.path(), &config()), Some(false));
}

/// The regression this whole check exists for: a unit left behind by the
/// release before the rename. It is installed, the scheduler reports it
/// active, and it fails on every one of its five-minute wakes.
#[test]
fn a_unit_written_before_the_rename_is_stale() {
    let home = crate::managed::testutil::tempdir();
    // Produced by the SAME renderer, from the argv the previous release
    // built -- not by string-editing the current output, which would only
    // ever model one platform's quoting.
    let mut aged = invocation();
    aged.args
        .truncate(aged.args.len() - crate::cli::COPILOT_PUSH.len());
    aged.args.push("copilot-push".to_owned());
    write(&render(home.path(), &aged));
    assert_eq!(
        stale(home.path(), &config()),
        Some(true),
        "a unit invoking a command this binary no longer has must be reported"
    );
}
