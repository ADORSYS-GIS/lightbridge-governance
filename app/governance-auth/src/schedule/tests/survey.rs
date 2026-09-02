//! The two questions `status` asks the platform, and the one place a wrong
//! answer is invisible.
//!
//! `an_unaskable_systemd_is_unknown_not_stopped` is the test that earns this
//! file. Reading `systemctl --user is-active`'s **exit code** instead of its
//! stdout conflates "the timer is stopped" with "there is no user manager here
//! to ask" -- and that implementation passes every other test in the crate,
//! which is how it survived until it was deliberately reintroduced.

use std::path::Path;

use super::{super::systemd, Invocation, config};

#[test]
fn an_unaskable_systemd_is_unknown_not_stopped() {
    assert_eq!(systemd::classify("active\n"), Some(true));
    assert_eq!(systemd::classify("inactive\n"), Some(false));
    assert_eq!(systemd::classify("failed\n"), Some(false));
    assert_eq!(
        systemd::classify(""),
        None,
        "no user manager to ask is not the same as a stopped timer"
    );
}

#[test]
fn the_two_units_do_not_share_a_temp_file() {
    // `with_extension` REPLACES the extension, so `…push.service` and
    // `…push.timer` both reduce to `…push.governance-auth-tmp`. Written
    // sequentially that still produces correct files, which is why nothing
    // else here catches it -- but two concurrent `configure` runs can land the
    // timer's body in the service file.
    let home = Path::new("/home/dev");
    let invocation = Invocation::resolve(&config())
        .expect("resolve")
        .expect("a collector is configured");
    let temps: Vec<_> = systemd::units(home, &invocation)
        .expect("render")
        .into_iter()
        .map(|(path, _)| systemd::tmp_path(&path))
        .collect();
    assert_eq!(temps.len(), 2);
    assert_ne!(
        temps.first(),
        temps.get(1),
        "the service and timer must not write through the same temp path"
    );
}
