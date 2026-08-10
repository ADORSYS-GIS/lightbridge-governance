//! `--issuer`/`--client-id` must parse regardless of whether they appear
//! before or after the subcommand name.
//!
//! This matters specifically because both vendors' credential-helper hooks
//! (`apiKeyHelper`, `auth.command`) are configured as a single command-line
//! string, and both this repo's runbook and the vendors' own examples write
//! the subcommand first (`"governance-auth token"`). Composing that with
//! explicit flags -- the only reliable option, since a helper subprocess
//! isn't guaranteed to inherit `GOVERNANCE_AUTH_ISSUER`/`_CLIENT_ID` from a
//! shell profile -- naturally reads as `governance-auth token --issuer ...`.
//! Found by actually wiring this into a real Claude Code `apiKeyHelper` and
//! watching it fail with `error: unexpected argument '--issuer' found`, not
//! by inspection.

mod support;

use anyhow::Result;
use support::harness::Harness;

#[tokio::test]
async fn issuer_and_client_id_parse_after_the_subcommand() -> Result<()> {
    let harness = Harness::new("https://issuer.invalid/realms/test")?;

    let output = harness
        .run_raw(&[
            "token",
            "--issuer",
            harness.issuer(),
            "--client-id",
            harness.client_id(),
        ])
        .await?;

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_ne!(
        output.status.code(),
        Some(2),
        "flags after the subcommand must not be a clap parse error \
         (`unexpected argument`); got: {stderr}"
    );
    assert!(
        stderr.contains("no cached session"),
        "expected the real `token` logic to run (and correctly fail closed \
         on an empty cache), got: {stderr}"
    );
    Ok(())
}

#[tokio::test]
async fn issuer_and_client_id_still_parse_before_the_subcommand() -> Result<()> {
    // The pre-existing order (this is what `Harness::run` already exercises
    // via every other test file) -- pinned here explicitly so a future
    // change can't silently flip which order works.
    let harness = Harness::new("https://issuer.invalid/realms/test")?;

    let output = harness
        .run_raw(&[
            "--issuer",
            harness.issuer(),
            "--client-id",
            harness.client_id(),
            "token",
        ])
        .await?;

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_ne!(output.status.code(), Some(2), "got: {stderr}");
    assert!(stderr.contains("no cached session"), "got: {stderr}");
    Ok(())
}
