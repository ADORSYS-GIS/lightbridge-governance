//! `--callback-port`/`--callback-bind` (issue #364): [`validate`] and
//! [`bind_with`], the two pieces `config::OauthConfigArgs` and
//! `oauth::authcode` build the new flags on.

use super::*;

#[test]
fn validate_accepts_every_registered_port() {
    for port in CALLBACK_PORTS {
        assert!(validate(port).is_ok(), "{port} is registered");
    }
}

#[test]
fn validate_rejects_a_port_outside_the_block_and_names_it() {
    let error = validate(9999).expect_err("9999 is not in CALLBACK_PORTS");
    assert!(error.contains("9999"), "should name the port: {error}");
    for port in CALLBACK_PORTS {
        assert!(
            error.contains(&port.to_string()),
            "should name the whole registered block; {port} missing from: {error}"
        );
    }
}

/// The pure function `validate` never touches the network -- calling it
/// costs nothing, which is what lets `config::OauthConfigArgs::resolve`
/// reject a bad `--callback-port` before OIDC discovery ever runs.
#[test]
fn validate_never_binds_a_socket() {
    // Not a mock: genuinely nothing here can reach the network, because
    // `validate` takes a bare `u16` and returns a `Result<(), String>` --
    // there is no `TcpListener`, `reqwest::Client` or `Url` anywhere in its
    // signature for a hidden network call to hide behind.
    assert!(validate(CALLBACK_PORTS[0]).is_ok());
}

#[test]
fn bind_with_an_explicit_port_binds_exactly_that_one() {
    let listener =
        bind_with(DEFAULT_BIND, Some(CALLBACK_PORTS[2])).expect("the port should be free");
    let bound = listener.local_addr().expect("bound address").port();
    assert_eq!(bound, CALLBACK_PORTS[2]);
}

/// The behaviour that makes `--callback-port` different from the unset
/// default: a busy chosen port is refused BY NAME, never silently retried on
/// a sibling port from the block -- unlike `bind_with(addr, None)`, this
/// port is what the caller explicitly asked for.
#[test]
fn bind_with_a_busy_explicit_port_refuses_without_trying_another() {
    let port = CALLBACK_PORTS[1];
    let Ok(held) = TcpListener::bind((DEFAULT_BIND, port)) else {
        eprintln!("skipped: port {port} already unavailable");
        return;
    };

    let error = bind_with(DEFAULT_BIND, Some(port))
        .expect_err("must refuse a port this process already holds");
    let message = error.to_string();
    assert!(message.contains(&port.to_string()), "got: {message}");
    assert!(
        !CALLBACK_PORTS
            .iter()
            .any(|other| *other != port && message.contains(&other.to_string())),
        "must not mention a different port -- there was no fallback attempt: {message}"
    );
    drop(held);
}

#[test]
fn bind_with_an_explicit_address_binds_there_not_on_loopback() {
    let unspecified = IpAddr::V4(Ipv4Addr::UNSPECIFIED);
    let listener = bind_with(unspecified, Some(CALLBACK_PORTS[3]))
        .expect("0.0.0.0 should be bindable in this sandbox");
    let bound = listener.local_addr().expect("bound address");
    assert_eq!(
        bound.ip(),
        unspecified,
        "must bind the requested address, not fall back to loopback"
    );
}

#[test]
fn bind_with_no_port_falls_through_the_block_same_as_bind() {
    let listener = bind_with(DEFAULT_BIND, None).expect("at least one port should be free");
    let bound = listener.local_addr().expect("bound address").port();
    assert!(CALLBACK_PORTS.contains(&bound));
}
