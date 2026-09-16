//! In-memory row-state builders shared by every dashboard test file --
//! `survey_support` is its file-writing counterpart (real settings.json/
//! config.toml on disk); these build the `Surveys` structs and their
//! `unsurveyed_*` defaults directly, as literals.

use super::super::{Daemon, Drain, OtelSpool, Session, Spool, Surveys, Target, Telemetry, render};

/// [`render`] with the two Copilot rows fixed at "nothing surveyed", so tests
/// predating them assert exactly what they did before and never touch `$HOME`
/// (never running `systemctl` for the drain row). Covered in `spool`/`drain`.
pub(super) fn table(
    issuer: &str,
    client_id: &str,
    session: &Session,
    telemetry: &Telemetry,
    targets: &[Target],
) -> String {
    render(
        issuer,
        client_id,
        session,
        &Surveys {
            telemetry,
            daemon: &unsurveyed_daemon(),
            otel_spool: &unsurveyed_otel_spool(),
            spool: &Spool {
                inner: None,
                last_push_age: None,
                last_discard_age: None,
                held_age: None,
                profile: crate::profile::Profile::Manual,
            },
            drain: &unsurveyed_drain(),
        },
        targets,
    )
}

/// `home` unresolvable, so `Drain::row` takes its "unknown" branch without
/// asking the platform's scheduler anything.
pub(super) fn unsurveyed_drain() -> Drain {
    Drain {
        schedule: None,
        collector: false,
        stale: None,
        profile: crate::profile::Profile::Manual,
    }
}

/// `Daemon::row`'s "unknown" branch, for the same reason as
/// [`unsurveyed_drain`] above.
pub(super) fn unsurveyed_daemon() -> Daemon {
    Daemon {
        schedule: None,
        profile: crate::profile::Profile::Daemon,
        collector: true,
    }
}

/// `Profile::Manual` -> `OtelSpool::row`'s quiet "not applicable" branch,
/// for the same reason [`unsurveyed_drain`] picks `Manual`: a test that is not
/// specifically about this row should not have to reason about a daemon spool
/// that was never surveyed.
pub(super) fn unsurveyed_otel_spool() -> OtelSpool {
    OtelSpool {
        inner: None,
        worst_quarantined_age: None,
        last_discard_age: None,
        profile: crate::profile::Profile::Manual,
    }
}

pub(super) fn target(path: &str, managed: usize, edited: usize) -> Target {
    Target {
        path: path.to_owned(),
        managed,
        edited,
    }
}

pub(super) fn otel(endpoint: Option<&str>, has_static_token: bool) -> Telemetry {
    Telemetry {
        endpoint: endpoint.map(ToOwned::to_owned),
        applied: endpoint.is_some(),
        has_static_token,
        stale: false,
        // `manual`: every existing caller of this helper is asserting on
        // `has_static_token` meaning something, which is only true under
        // `manual` (`Telemetry::row`'s doc) -- a `daemon` fixture belongs in
        // `dashboard/tests/telemetry.rs`'s own daemon-specific test instead.
        profile: crate::profile::Profile::Manual,
    }
}

pub(super) fn session(cached: bool, fresh: bool) -> Session {
    expiring(cached, fresh, 900)
}

pub(super) fn expiring(cached: bool, fresh: bool, expires_in: i64) -> Session {
    Session {
        cached,
        fresh,
        expires_in,
    }
}
