//! Authorization Code + PKCE via a localhost loopback redirect (RFC 8252,
//! the native-app pattern). Binds one of a small block of **fixed**,
//! pre-registered ports (see [`crate::oauth::callback_port`]), prints the
//! authorize URL,
//! and blocks for exactly one HTTP request -- the redirect back from the
//! authorization server. Launching the system browser automatically is opt-in
//! (`config.open_browser`, issue #141) -- see that field's doc in
//! `crate::config` for why it isn't the default.
//!
//! ⚠️ The fixed ports are a **workaround for a server-side spec violation**,
//! not a design preference. RFC 8252 §7.3 says the authorization server MUST
//! allow any port for a loopback redirect, precisely so a native app can take
//! an ephemeral one from the OS. `authkestra-op` matches redirect URIs with a
//! plain `==`, so an ephemeral port can never match a registration and the
//! flow fails 100% of the time. Filed upstream as
//! <https://github.com/marcjazz/authkestra/issues/291>; revert to
//! `bind(("127.0.0.1", 0))` and delete that module once it is fixed.
//!
//! ⚠️ PKCE (`code_challenge`/`code_challenge_method=S256`, below) is
//! unconditional, not a flag. RFC 8252 / OAuth 2.1 require it for public
//! clients (this binary ships with no client secret), and there is
//! deliberately no way to turn it off -- see `tests/pkce_authcode.rs` for
//! the regression guard.

use std::{
    io::{BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
};

use anyhow::{Context, Result, bail};
use url::Url;

use super::{OidcMetadata, callback_port, token_endpoint};
use crate::{browser, cache::CachedSession, config::OauthConfig, oauth::pkce};

pub async fn run(
    http: &reqwest::Client,
    config: &OauthConfig,
    metadata: &OidcMetadata,
) -> Result<CachedSession> {
    let listener = callback_port::bind()?;
    let port = listener
        .local_addr()
        .context("reading loopback listener address")?
        .port();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");

    let pkce = pkce::generate()?;
    let state = pkce::random_state()?;

    let authorize_url = build_authorize_url(metadata, config, &redirect_uri, &pkce, &state)?;

    // OFF by default (issue #141): auto-opening a browser is wrong more
    // often than right over SSH, in containers, in CI, and in VM-based
    // testing, and the URL below works exactly the same whether the tab was
    // launched by this binary or pasted by a human -- so the default costs
    // nothing. `--open-browser`/`GOVERNANCE_AUTH_OPEN_BROWSER`/the config
    // key restore the old behaviour.
    if config.open_browser {
        eprintln!("Opening your browser to log in. If it doesn't open, visit:\n{authorize_url}");
        if let Err(error) = browser::open(authorize_url.as_str()) {
            eprintln!(
                "Could not open a browser automatically ({error}); visit the URL above manually."
            );
        }
    } else {
        eprintln!("To log in, visit:\n{authorize_url}");
    }

    let (code, returned_state) = tokio::task::spawn_blocking(move || await_callback(listener))
        .await
        .context("waiting for the OAuth callback")??;

    if returned_state != state {
        bail!("OAuth `state` mismatch on callback; aborting rather than trusting it");
    }

    let mut params = vec![
        ("grant_type", "authorization_code"),
        ("code", code.as_str()),
        ("redirect_uri", redirect_uri.as_str()),
        ("client_id", config.client_id.as_str()),
        ("code_verifier", pkce.verifier.as_str()),
    ];
    if let Some(audience) = &config.audience {
        params.push(("audience", audience.as_str()));
    }

    let response = token_endpoint::request(http, &metadata.token_endpoint, &params).await?;
    token_endpoint::into_session(config, response)
}

fn build_authorize_url(
    metadata: &OidcMetadata,
    config: &OauthConfig,
    redirect_uri: &str,
    pkce: &pkce::Pkce,
    state: &str,
) -> Result<Url> {
    // The ONE flow that needs this field, so "absent" is reported here rather
    // than by making it required at deserialize time -- doing that broke
    // `--exchange-issuer` against a server that legitimately serves no
    // authorization endpoint (issue #145). A caller reaching this genuinely
    // cannot log in interactively against that issuer, and the message says
    // so instead of surfacing a serde `missing field` error.
    let authorization_endpoint = metadata.authorization_endpoint.as_deref().context(
        "this issuer advertises no `authorization_endpoint`, so the browser login flow cannot be \
         used against it; use `--device-code`, or point `--issuer` at an authorization server \
         that serves one",
    )?;
    let mut url =
        Url::parse(authorization_endpoint).context("parsing authorization endpoint URL")?;
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("response_type", "code")
            .append_pair("client_id", &config.client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("scope", &config.scopes)
            .append_pair("state", state)
            .append_pair("code_challenge", &pkce.challenge)
            .append_pair("code_challenge_method", "S256");
        if let Some(audience) = &config.audience {
            query.append_pair("audience", audience);
        }
    }
    Ok(url)
}

/// Blocks the calling (blocking-pool) thread for exactly one HTTP request.
fn await_callback(listener: TcpListener) -> Result<(String, String)> {
    let (stream, _) = listener.accept().context("accepting loopback callback")?;
    parse_callback_request(stream)
}

fn parse_callback_request(mut stream: TcpStream) -> Result<(String, String)> {
    let mut reader = BufReader::new(stream.try_clone().context("cloning callback stream")?);
    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .context("reading callback HTTP request line")?;

    let path = request_line
        .split_whitespace()
        .nth(1)
        .context("malformed HTTP request line on the loopback callback")?;

    let url =
        Url::parse(&format!("http://127.0.0.1{path}")).context("parsing callback request path")?;

    let mut code = None;
    let mut state = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            "error" => {
                write_callback_response(&mut stream, false)?;
                bail!("authorization server returned an error: {value}");
            }
            _ => {}
        }
    }

    write_callback_response(&mut stream, code.is_some())?;

    let code = code.context("callback request had no `code` parameter")?;
    let state = state.context("callback request had no `state` parameter")?;
    Ok((code, state))
}

fn write_callback_response(stream: &mut TcpStream, success: bool) -> Result<()> {
    let body = if success {
        "You're signed in. You can close this tab and return to your terminal."
    } else {
        "Sign-in failed. You can close this tab and return to your terminal."
    };
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream
        .write_all(response.as_bytes())
        .context("writing callback HTTP response")?;
    Ok(())
}
