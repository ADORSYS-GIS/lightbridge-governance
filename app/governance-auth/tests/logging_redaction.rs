//! A token must never reach the log file, and the log file must never reach
//! stdout.
//!
//! Why this has its own test file. Adding file logging changed the stakes of
//! a leak: a token on stderr is gone when the terminal scrolls, a token in
//! `~/Library/Logs/governance-auth/governance-auth.log` is a credential at
//! rest that outlives the session it belonged to, is world-discoverable by
//! path, and gets swept into whatever backs that directory up. So the guard
//! is not "we were careful" -- it runs the real binary at the loudest
//! logging any operator can ask for and greps the resulting file for a
//! sentinel.
//!
//! The other half is the parsed contract: `token`'s stdout carries the
//! access token and nothing else, `otel headers`' carries one JSON object.
//! Both layers of the subscriber are pinned off stdout, and turning logging
//! all the way up is exactly the configuration that would expose it if one
//! were not.

mod support;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use support::harness::Harness;

/// A value that cannot occur by accident, so a hit in the log file is
/// unambiguous rather than a coincidence.
const ACCESS: &str = "sentinel-access-token-must-never-be-logged";
const REFRESH: &str = "sentinel-refresh-token-must-never-be-logged";

/// The loudest configuration reachable: `trace` on the stderr layer AND on
/// the file layer. Anything the binary is capable of writing, it writes here.
const LOUDEST: [(&str, &str); 2] = [("RUST_LOG", "trace"), ("GOVERNANCE_AUTH_LOG", "trace")];

/// Mirrors `logging::path`, which is private to `src/`.
fn log_path(home: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library").join("Logs").join("governance-auth")
    } else {
        home.join(".local")
            .join("state")
            .join("governance-auth")
            .join("logs")
    }
    .join("governance-auth.log")
}

fn read_log(home: &Path) -> Result<String> {
    let path = log_path(home);
    std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))
}

fn now_unix() -> Result<u64> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("system clock before unix epoch")?
        .as_secs())
}

fn seed(harness: &Harness) -> Result<()> {
    // A session that is valid for another hour, so `token` prints it
    // straight from the cache without a network round trip -- the exact
    // path Claude Code's `apiKeyHelper` takes hundreds of times a day.
    harness.seed_session(&serde_json::json!({
        "issuer": harness.issuer(),
        "client_id": harness.client_id(),
        "access_token": ACCESS,
        "refresh_token": REFRESH,
        "expires_at": now_unix()? + 3600,
    }))
}

#[tokio::test]
async fn the_token_it_prints_never_lands_in_the_log_file() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    seed(&harness)?;

    let output = harness.run_with_env(&["token"], &LOUDEST).await?;

    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        ACCESS,
        "stdout must be the token and nothing else, even at trace"
    );

    let log = read_log(harness.home())?;

    // ⚠️ Read this assertion FIRST. Without it the two below are vacuous:
    // an empty (or absent) file trivially "contains no token", and a bug
    // that stopped logging altogether would make this test pass while
    // proving nothing.
    assert!(
        log.contains("invoked"),
        "the invocation must actually have been recorded, or the assertions \
         below prove nothing. got {log:?}"
    );

    assert!(
        !log.contains(ACCESS),
        "the access token reached the log file, which is a credential at \
         rest. got {log:?}"
    );
    assert!(
        !log.contains(REFRESH),
        "the refresh token reached the log file -- worse than the access \
         token, it does not expire in an hour. got {log:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_failing_run_records_why_without_recording_the_credential() -> Result<()> {
    // The 03:00 case this feature exists for: nobody is watching, stderr
    // goes nowhere, and the only question afterwards is *why*. The answer
    // has to be on disk, and it still must not carry the session.
    let harness = Harness::new("https://unreachable.invalid.example")?;
    harness.seed_session(&serde_json::json!({
        "issuer": "https://unreachable.invalid.example",
        "client_id": "test-client",
        "access_token": ACCESS,
        "refresh_token": null,
        "expires_at": now_unix()?.saturating_sub(3600),
    }))?;

    let output = harness.run_with_env(&["token"], &LOUDEST).await?;

    assert!(!output.status.success());
    assert!(output.stdout.is_empty(), "fail closed: nothing on stdout");

    let log = read_log(harness.home())?;
    assert!(
        log.contains("command failed"),
        "a failed run must leave the reason behind, got {log:?}"
    );
    assert!(
        !log.contains(ACCESS),
        "not even the expired token may be written down, got {log:?}"
    );
    Ok(())
}

#[tokio::test]
async fn otel_headers_stdout_stays_a_bare_json_object_at_trace() -> Result<()> {
    // `otelHeadersHelper` parses this. A single log line escaping onto
    // stdout breaks Claude Code's telemetry with no error anywhere.
    let harness = Harness::new("https://unreachable.invalid.example")?;
    seed(&harness)?;

    let output = harness.run_with_env(&["otel", "headers"], &LOUDEST).await?;

    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let headers: serde_json::Value =
        serde_json::from_str(stdout.trim()).with_context(|| format!("parsing {stdout:?}"))?;
    assert!(
        headers.is_object(),
        "stdout must be one JSON object, got {stdout:?}"
    );

    let log = read_log(harness.home())?;
    assert!(log.contains("invoked"), "the file layer must have run");
    assert!(
        !log.contains(ACCESS),
        "the bearer token reached the log file, got {log:?}"
    );
    Ok(())
}
