//! Which local port the browser redirect comes back on.
//!
//! Exists as its own module because it is a **temporary workaround with a
//! deletion condition**, and burying it inside the flow makes it easy to
//! forget. RFC 8252 §7.3 requires the authorization server to accept *any*
//! port for a loopback redirect; ours does not, so we pin a block of ports
//! and register every one of them. See [`CALLBACK_PORTS`].

use std::net::{IpAddr, Ipv4Addr, TcpListener};

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

/// The default listen address: loopback, matching every build before
/// `--callback-bind` existed.
pub const DEFAULT_BIND: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

/// Rejects a port outside [`CALLBACK_PORTS`], naming the whole block. Shared
/// by `config::OauthConfigArgs`'s `--callback-port` clap parser and its
/// config-file re-validation (a file value never passes through clap), so
/// both reject exactly the same set of ports with exactly the same message.
pub fn validate(port: u16) -> Result<(), String> {
    if CALLBACK_PORTS.contains(&port) {
        return Ok(());
    }
    let ports = CALLBACK_PORTS
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    Err(format!(
        "--callback-port {port} is not one of the registered loopback callback ports ({ports}); \
         the authorization server only accepts a redirect to one of those, so any other port \
         would bind but then fail at `/authorize`"
    ))
}

/// Binds the loopback callback listener. `oauth::authcode` is the one
/// caller; every build before `--callback-port`/`--callback-bind` existed
/// bound `DEFAULT_BIND` with `port: None`, which is still exactly what an
/// unconfigured `login` does today.
///
/// `Some(port)` binds exactly that port and nothing else: a busy port is
/// refused, by name, never silently retried on a different one -- unlike the
/// `None` case below, picking a specific port is something the caller asked
/// for, and falling back would bind a port they did not ask for and still
/// fail later at `/authorize` on a mismatched `redirect_uri`.
///
/// `None` tries every port of [`CALLBACK_PORTS`] on `addr` in order and
/// fails loudly (naming every port tried) rather than falling back to an
/// ephemeral one -- a fallback would bind successfully and then fail later
/// at `/authorize` with `invalid redirect_uri`, moving the error away from
/// its cause and into the authorization server's response.
pub fn bind_with(addr: IpAddr, port: Option<u16>) -> Result<TcpListener> {
    let Some(port) = port else {
        return bind_any(addr);
    };

    TcpListener::bind((addr, port)).map_err(|error| {
        anyhow::anyhow!(
            "--callback-port {port} is already in use ({error}); refusing rather than trying a \
             different port from the block, since that is not the one you asked for. Free it, \
             drop --callback-port to try the whole block, or use `--device-code`."
        )
    })
}

fn bind_any(addr: IpAddr) -> Result<TcpListener> {
    let mut last_error = None;
    for port in CALLBACK_PORTS {
        match TcpListener::bind((addr, port)) {
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
mod tests;
#[cfg(test)]
mod tests_bind_with;
