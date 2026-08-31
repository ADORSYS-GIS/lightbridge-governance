//! Which local port the browser redirect comes back on.
//!
//! Exists as its own module because it is a **temporary workaround with a
//! deletion condition**, and burying it inside the flow makes it easy to
//! forget. RFC 8252 §7.3 requires the authorization server to accept *any*
//! port for a loopback redirect; ours does not, so we pin a block of ports
//! and register every one of them. See [`CALLBACK_PORTS`].

use std::net::TcpListener;

use anyhow::{Result, bail};

/// Loopback callback ports, in the order they are tried.
///
/// ⚠️ These are **contract, not preference**. Every value here must also be a
/// `redirect_uris` entry on the `governance-auth-cli` client, because
/// `authkestra-op`'s `allows_redirect_uri` is a plain `==` -- no
/// normalisation, no port exemption. Adding a port here without the matching
/// registration yields `400 invalid redirect_uri`; dropping one that a
/// released binary still tries yields the same. Change both together, and
/// land the registration first.
///
/// Why a fixed block at all: RFC 8252 §7.3 says an authorization server
/// **MUST** allow any port for loopback redirects, exactly so a native app can
/// take an ephemeral one from the OS. Ours does not
/// (<https://github.com/marcjazz/authkestra/issues/291>), so an ephemeral port
/// can never match a registration and the browser flow fails every time.
/// **Delete this module once that is fixed** and go back to
/// `TcpListener::bind(("127.0.0.1", 0))`.
///
/// Why *these* ports:
///
/// - **Below 32768.** The OS draws ephemeral ports from 32768-60999 on Linux
///   and 49152-65535 (IANA Dynamic) on macOS. A "fixed" port inside either
///   window can be handed to an unrelated process at any time, so login would
///   fail intermittently and unreproducibly -- the worst failure mode for a
///   credential helper, and one that would look like a server bug.
/// - **Above 1024.** Lower ports need root; this runs as a developer.
/// - **A quiet block.** Unassigned in `/etc/services`, and clear of the
///   well-trodden dev ports (3000, 5000, 8000, 8080, 9000, ...) most likely to
///   be held by something else on a developer's machine.
///
/// Past those constraints the specific number is arbitrary and deliberately
/// meaningless -- nothing is encoded in it, so nobody should preserve it for
/// its own sake. The *window* is what is load-bearing.
///
/// Why five and not one: a single fixed port reintroduces precisely the
/// failure §7.3 exists to prevent -- one unrelated process holding it locks
/// the developer out with no recourse. Five consecutive ports make that
/// vanishingly unlikely while remaining compatible with exact-match
/// registration, because all five are registered.
pub const CALLBACK_PORTS: [u16; 5] = [17452, 17453, 17454, 17455, 17456];

/// Binds the first free port in [`CALLBACK_PORTS`].
///
/// Fails loudly, naming every port tried, rather than falling back to an
/// ephemeral one. A fallback would bind successfully and then fail later at
/// `/authorize` with `invalid redirect_uri`, moving the error away from its
/// cause and into the authorization server's response, where it looks like a
/// server or registration problem instead of a local port collision.
pub fn bind() -> Result<TcpListener> {
    let mut last_error = None;
    for port in CALLBACK_PORTS {
        match TcpListener::bind(("127.0.0.1", port)) {
            Ok(listener) => return Ok(listener),
            Err(error) => last_error = Some((port, error)),
        }
    }

    let ports = CALLBACK_PORTS
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    let detail = last_error
        .map(|(port, error)| format!(" (binding {port} failed: {error})"))
        .unwrap_or_default();
    bail!(
        "every loopback callback port is already in use: {ports}{detail}. These specific ports \
         are required because the authorization server matches redirect URIs exactly and only \
         these are registered, so this cannot fall back to another port. Free one of them, or \
         use `--device-code`, which needs no local listener at all."
    )
}

#[cfg(test)]
mod tests {
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

        let listener = bind().expect("a later port in the block should still be free");
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

        let error = bind().expect_err("must refuse when every registered port is taken");
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
}
