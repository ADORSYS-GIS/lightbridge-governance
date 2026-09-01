//! Starting a `copilot-push` wake and killing it part way through.
//!
//! [`super::harness::Harness::run`] waits for the child, which is precisely
//! what a test about a wake that never finishes cannot do. So this builds its
//! own [`Command`] -- with the same environment isolation every harness spawn
//! site uses, see that module's doc for why the three `XDG_*` variables are
//! *removed* rather than overridden.
//!
//! ## Why SIGKILL and not SIGTERM
//!
//! The property under test is that durability does **not** depend on catching
//! a signal: a handler covers SIGTERM and covers neither SIGKILL, nor an OOM
//! kill, nor the machine losing power. A test that sent SIGTERM would pass
//! against a fix that only installs a handler, which is the fix this is
//! supposed to rule out. `Child::kill` is SIGKILL, and it is also the only
//! kill reachable without `libc` and an `unsafe` block, which this repo denies
//! outright.
//!
//! ## Why the waits are on a condition and never on the clock
//!
//! "Kill it 1.2 seconds in" makes the test a race against whatever else the
//! machine is doing. Every wait here polls a fact the mock collector can
//! answer -- how many requests it has seen, how many records it has taken --
//! so the kill lands at the same point in the drain on a loaded CI runner as
//! on an idle laptop.

use std::{
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};

use super::harness::Harness;

/// How long a condition may take before the test gives up. Generous: it only
/// bounds a hang, and every wait below normally settles in well under a
/// second.
const PATIENCE: Duration = Duration::from_secs(30);

/// How often a condition is re-checked. Short enough that the kill lands
/// within one collector round trip of where the test asked for it.
const POLL: Duration = Duration::from_millis(5);

/// One `copilot-push` process that the test intends to interrupt.
pub struct Wake {
    child: Child,
}

impl Wake {
    /// Starts a wake against `collector` and `spool` and returns immediately.
    ///
    /// The argument list is deliberately the same shape as
    /// [`super::copilot::push`]'s, so an interrupted wake and a completed one
    /// differ in nothing but the interruption.
    pub fn start(harness: &Harness, collector: &str, spool: &Path) -> Result<Self> {
        let mut command = Command::new(env!("CARGO_BIN_EXE_governance-auth"));
        command
            .env("HOME", harness.home())
            .env_remove("XDG_CACHE_HOME")
            .env_remove("XDG_STATE_HOME")
            .env_remove("XDG_CONFIG_HOME")
            .arg("--issuer")
            .arg(harness.issuer())
            .arg("--client-id")
            .arg(harness.client_id())
            .arg("copilot-push")
            .arg("--otel-endpoint")
            .arg(collector)
            .arg("--copilot-spool-path")
            .arg(spool)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = command.spawn().context("spawning a copilot-push wake")?;
        Ok(Self { child })
    }

    /// SIGKILLs the wake and reaps it. Errors when it had already exited: a
    /// test that meant to interrupt a running drain and instead watched it
    /// finish has proved nothing, and must say so rather than pass.
    pub fn kill(mut self) -> Result<()> {
        if let Some(status) = self
            .child
            .try_wait()
            .context("checking whether the wake is still running")?
        {
            bail!(
                "the wake had already finished ({status}) before the test could interrupt it; \
                 nothing about a killed drain was exercised"
            );
        }
        self.child.kill().context("killing the wake")?;
        self.child.wait().context("reaping the killed wake")?;
        Ok(())
    }
}

/// Blocks until `ready` answers `true`, re-checking every [`POLL`].
///
/// `label` is what the failure says it was waiting for, because a timeout here
/// is nearly always a fixture that stopped producing the shape the test
/// assumed rather than a genuine hang.
pub async fn until(label: &str, mut ready: impl FnMut() -> Result<bool>) -> Result<()> {
    let deadline = Instant::now() + PATIENCE;
    while Instant::now() < deadline {
        if ready()? {
            return Ok(());
        }
        tokio::time::sleep(POLL).await;
    }
    bail!("timed out after {PATIENCE:?} waiting for {label}")
}
