//! The daemon's two unit files -- `serve --otel`'s systemd service and
//! launchd agent (ADR-0016, #260/#270).
//!
//! Split out from [`super`] rather than added to it: that module sits at
//! 181/200 lines already, and a `KeepAlive`/`Type=simple` persistent service
//! is a different shape from the `StartInterval`/`Type=oneshot` pair the
//! Copilot drain renders there, not a parameterisation of it -- see
//! `crate::schedule::daemon`'s module doc for why the *installer* logic is
//! likewise a sibling module rather than a branch inside `schedule::systemd`/
//! `schedule::launchd`.

use minijinja::context;

const SYSTEMD_DAEMON_SERVICE: &str = include_str!("systemd_daemon_service.jinja");
const LAUNCHD_DAEMON_PLIST: &str = include_str!("launchd_daemon_plist.jinja");

/// The daemon's systemd unit: `Type=simple`, `Restart=on-failure`, no
/// `.timer` -- see the template's own comments for why this must stay
/// running rather than wake periodically. `argv` is quoted per word by the
/// template, never pre-joined -- see [`super::systemd_quote`].
pub fn systemd_service(argv: &[String]) -> Result<String, minijinja::Error> {
    super::render(
        "systemd_daemon_service",
        SYSTEMD_DAEMON_SERVICE,
        context! { argv },
    )
}

/// The launchd equivalent: `KeepAlive` in place of `StartInterval`.
pub fn launchd_plist(
    label: &str,
    argv: &[String],
    log_path: &str,
) -> Result<String, minijinja::Error> {
    super::render(
        "launchd_daemon_plist",
        LAUNCHD_DAEMON_PLIST,
        context! { label, argv, log_path },
    )
}

#[cfg(test)]
mod tests;
