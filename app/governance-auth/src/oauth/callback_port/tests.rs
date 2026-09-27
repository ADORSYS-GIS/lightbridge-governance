//! Pre-existing behaviour: [`super::bind_with`] called with `DEFAULT_BIND`
//! and `None` (no `--callback-port`/`--callback-bind` involved) tries the
//! block in order on loopback, exactly as before those flags existed.

use super::*;

/// The constraint that makes these ports safe to pin. Inside an ephemeral
/// range the OS could hand one to an unrelated process, and login would
/// fail intermittently -- so this is the property to guard, not the
/// specific values.
#[test]
fn every_port_is_outside_both_ephemeral_ranges() {
    for port in CALLBACK_PORTS {
        assert!(
            port > 1024,
            "{port} needs root to bind; developers do not run this as root"
        );
        // Linux ip_local_port_range starts at 32768; macOS/IANA Dynamic at
        // 49152. Staying under the lower of the two covers both.
        assert!(
            port < 32768,
            "{port} is inside an OS ephemeral range, so it can be taken by another process"
        );
    }
}

#[test]
fn ports_are_unique() {
    let mut seen = CALLBACK_PORTS;
    seen.sort_unstable();
    let mut deduped = seen.to_vec();
    deduped.dedup();
    assert_eq!(deduped.len(), CALLBACK_PORTS.len(), "duplicate port listed");
}

/// A held port must be skipped, not fatal -- the whole reason there is a
/// block rather than a single port.
#[test]
fn falls_through_to_the_next_free_port() {
    let Ok(first) = TcpListener::bind(("127.0.0.1", CALLBACK_PORTS[0])) else {
        // Something else on this machine holds it; the property under test
        // cannot be set up, and asserting anything here would be a lie.
        eprintln!("skipped: port {} unavailable", CALLBACK_PORTS[0]);
        return;
    };

    let listener =
        bind_with(DEFAULT_BIND, None).expect("a later port in the block should still be free");
    let bound = listener
        .local_addr()
        .expect("bound listener has an address")
        .port();

    assert_ne!(bound, CALLBACK_PORTS[0], "should not return the held port");
    assert!(
        CALLBACK_PORTS.contains(&bound),
        "bound {bound}, which is outside the registered block -- the server would reject it"
    );
    drop(first);
}

/// The failure that must NOT be silent. If this ever falls back to an
/// ephemeral port, the flow proceeds and dies later at `/authorize` with a
/// misleading `invalid redirect_uri`.
#[test]
fn refuses_rather_than_falling_back_when_all_ports_are_held() {
    let mut held = Vec::new();
    for port in CALLBACK_PORTS {
        match TcpListener::bind(("127.0.0.1", port)) {
            Ok(listener) => held.push(listener),
            Err(_) => {
                eprintln!("skipped: port {port} already unavailable");
                return;
            }
        }
    }

    let error =
        bind_with(DEFAULT_BIND, None).expect_err("must refuse when every registered port is taken");
    let message = error.to_string();
    for port in CALLBACK_PORTS {
        assert!(
            message.contains(&port.to_string()),
            "error should name every port tried; {port} missing from: {message}"
        );
    }
    assert!(
        message.contains("--device-code"),
        "error should point at the flow that needs no listener: {message}"
    );
}
