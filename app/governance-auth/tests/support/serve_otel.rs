//! The `serve otel` daemon (issue #268) driven for real: spawn it as a
//! subprocess, poke its fixed loopback port, and tear it down.
//!
//! ## One fixed port, therefore one process at a time
//!
//! The daemon binds a **fixed** loopback port (17457, [`otel_port`]'s
//! `OTEL_PORT`) and runs until it is killed -- it never exits on its own, and
//! it refuses to fall back to an ephemeral port on purpose. So every test that
//! starts a daemon serializes on a [`OnceLock`] mutex, and a `Daemon` holds
//! the guard until it is stopped. A test that forgot to stop one would deadlock
//! the next, which is the loud failure we want (the fixed port being taken is
//! itself the symptom that matters).
//!
//! ## Kill is SIGKILL, and that is fine here
//!
//! The spool is in-memory and lost on process exit -- accepted for #268. So
//! these tests assert against the mock collector's state and the HTTP status
//! the client got, never against a graceful shutdown, and [`Child::kill`]
//! (the only kill reachable without `libc`/`unsafe`, which the repo denies) is
//! all `stop` needs.
//!
//! ## Why `until` and never the clock
//!
//! "Sleep 300 ms, then POST" is a race against an already-loaded CI runner. The
//! readiness wait below polls the one fact that must be true before a POST can
//! be meaningful -- that the daemon has bound the port -- and the drain test
//! polls the collector's request count, so the assertion lands at the same
//! point on an idle laptop and a busy runner.

use std::{
    net::TcpStream,
    process::{Child, Command, Stdio},
    sync::{Mutex, MutexGuard, OnceLock},
};

use anyhow::{Context, Result};
use serde_json::Value;

use super::{harness::Harness, interrupt};

/// The fixed loopback endpoint the daemon binds, mirroring [`otel_port`]'s
/// `OTEL_LOOPBACK_ENDPOINT` (re-derived because `tests/` cannot reach `src/`;
/// a drift shows up as every test here failing to connect, which is the point).
pub const DAEMON_ENDPOINT: &str = "http://127.0.0.1:17457";

fn port_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// A running `serve otel` daemon. Holds the fixed-port lock until `stop`.
pub struct Daemon {
    child: Child,
    _port_guard: MutexGuard<'static, ()>,
}

impl Daemon {
    /// Spawns `serve otel --otel-endpoint <collector>` and returns without
    /// waiting -- the daemon runs until killed. `extra` carries any additional
    /// flags a test needs.
    pub async fn start(harness: &Harness, collector: &str, extra: &[&str]) -> Result<Self> {
        let guard = port_lock()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let mut command = Command::new(env!("CARGO_BIN_EXE_governance-auth"));
        command
            .env("HOME", harness.home())
            .env_remove("XDG_CACHE_HOME")
            .env_remove("XDG_STATE_HOME")
            .env_remove("XDG_CONFIG_HOME") // see `harness::Harness`'s doc for why these are removed
            .arg("--issuer")
            .arg(harness.issuer())
            .arg("--client-id")
            .arg(harness.client_id())
            .arg("serve")
            .arg("otel")
            .arg("--otel-endpoint")
            .arg(collector)
            .args(extra)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = command.spawn().context("spawning the serve otel daemon")?;
        let daemon = Self {
            child,
            _port_guard: guard,
        };
        daemon.until_ready().await?;
        Ok(daemon)
    }

    /// Blocks until the daemon has bound the loopback port, so callers can POST
    /// without racing the spawn.
    pub async fn until_ready(&self) -> Result<()> {
        interrupt::until("the serve otel daemon to bind the loopback port", || {
            Ok(TcpStream::connect(("127.0.0.1", 17457)).is_ok())
        })
        .await
    }

    /// POSTs an OTLP JSON body to the daemon's loopback endpoint and returns the
    /// status it answered with. `path` exercises the "any path" contract.
    pub async fn post(&self, path: &str, body: &Value) -> Result<reqwest::StatusCode> {
        let response = reqwest::Client::new()
            .post(format!("{DAEMON_ENDPOINT}{path}"))
            .header("content-type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .with_context(|| format!("POSTing to {DAEMON_ENDPOINT}{path}"))?;
        Ok(response.status())
    }

    /// POSTs an arbitrary (e.g. OTLP protobuf) body and its content-type, for
    /// the "a non-JSON body is forwarded, not withheld" case.
    pub async fn post_bytes(
        &self,
        path: &str,
        content_type: &str,
        body: Vec<u8>,
    ) -> Result<reqwest::StatusCode> {
        let response = reqwest::Client::new()
            .post(format!("{DAEMON_ENDPOINT}{path}"))
            .header("content-type", content_type)
            .body(body)
            .send()
            .await
            .with_context(|| format!("POSTing bytes to {DAEMON_ENDPOINT}{path}"))?;
        Ok(response.status())
    }

    /// SIGKILLs the daemon and reaps it, releasing the fixed-port lock. Errors if
    /// it already exited: a test meant to exercise a *running* daemon and instead
    /// watched it die has proved nothing.
    pub fn stop(mut self) -> Result<()> {
        if let Some(status) = self
            .child
            .try_wait()
            .context("checking whether the daemon is still running")?
        {
            anyhow::bail!(
                "the daemon had already exited ({status}) before the test could stop it; nothing \
                 about a live daemon was exercised"
            );
        }
        self.child.kill().context("killing the daemon")?;
        self.child.wait().context("reaping the killed daemon")?;
        Ok(())
    }
}
