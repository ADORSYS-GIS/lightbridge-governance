//! Drives the actual compiled `governance-auth` binary as a subprocess --
//! the same way `apiKeyHelper`/`auth.command` invoke it -- against an
//! isolated `$HOME` so tests never touch a developer's real session cache.
//!
//! Every fallible step here returns `anyhow::Result` rather than
//! unwrapping: `clippy.toml` calls out that free functions under
//! `tests/support/` are NOT covered by the `allow-*-in-tests` carve-out
//! (only `#[test]`/`#[tokio::test]` functions are), so this module is held
//! to the same no-`unwrap`/`expect`/`panic` bar as `src/`. Callers are
//! `#[tokio::test]` functions, which propagate failures with `?`.

use std::{
    io::{BufRead, Read, Write},
    net::TcpStream,
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A per-test scratch `$HOME`, removed on drop. Hand-rolled instead of
/// pulling in `tempfile`: one call site, and the cleanup is a single
/// `remove_dir_all`.
struct TempHome {
    path: PathBuf,
}

impl TempHome {
    fn new() -> Result<Self> {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .context("system clock before unix epoch")?
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "governance-auth-test-{}-{unique}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path)
            .with_context(|| format!("creating temp $HOME at {}", path.display()))?;
        Ok(Self { path })
    }
}

impl Drop for TempHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

pub struct Harness {
    home: TempHome,
    issuer: String,
    client_id: String,
}

impl Harness {
    pub fn new(issuer: &str) -> Result<Self> {
        Ok(Self {
            home: TempHome::new()?,
            issuer: issuer.to_owned(),
            client_id: "test-client".to_owned(),
        })
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_governance-auth"));
        command
            .env("HOME", &self.home.path)
            .env_remove("XDG_CACHE_HOME")
            .env_remove("XDG_STATE_HOME")
            .arg("--issuer")
            .arg(&self.issuer)
            .arg("--client-id")
            .arg(&self.client_id)
            .args(args);
        command
    }

    /// Runs a subcommand that doesn't need an interactive browser step
    /// (`token`, `status`, `logout`) and waits for it to exit. Routed
    /// through `spawn_blocking` like [`Self::login_with_browser_action`]:
    /// `token` on a near-expiry cache calls out to the mock IdP, which runs
    /// as a task on this same (by default single-threaded) test runtime --
    /// a plain synchronous `.output()` here would block that thread and
    /// deadlock against the mock server never getting polled.
    pub async fn run(&self, args: &[&str]) -> Result<Output> {
        let mut command = self.command(args);
        tokio::task::spawn_blocking(move || {
            command
                .output()
                .context("running governance-auth subprocess")
        })
        .await
        .context("run harness task panicked")?
    }

    /// Like [`Self::run`], but the caller supplies the *entire* argument
    /// list -- no automatic `--issuer <issuer> --client-id <client_id>`
    /// prefix. Exists to test argument-position independence itself (e.g.
    /// `token --issuer ... --client-id ...`, flags *after* the subcommand --
    /// the exact shape a single `apiKeyHelper`/`auth.command` string
    /// composes), which `run`'s fixed flags-first prefix can't exercise.
    pub async fn run_raw(&self, args: &[&str]) -> Result<Output> {
        let mut command = Command::new(env!("CARGO_BIN_EXE_governance-auth"));
        command
            .env("HOME", &self.home.path)
            .env_remove("XDG_CACHE_HOME")
            .env_remove("XDG_STATE_HOME")
            .args(args);
        tokio::task::spawn_blocking(move || {
            command
                .output()
                .context("running governance-auth subprocess")
        })
        .await
        .context("run_raw harness task panicked")?
    }

    /// Like [`Self::run`], but with extra environment variables set on the
    /// child process only -- e.g. `GOVERNANCE_AUTH_SCOPES`, to prove a CLI
    /// flag wins over its env var (ADR-0012 Decision 2's layer 1 vs layer 2)
    /// against the real process environment, not a simulated one. Setting
    /// this on the child rather than the test binary's own process is what
    /// keeps this test-safe under Rust 2024's `unsafe` requirement on
    /// `std::env::set_var` -- a per-child env is ordinary, safe
    /// `Command::env`, and this repo denies `unsafe_code` outright.
    pub async fn run_with_env(&self, args: &[&str], envs: &[(&str, &str)]) -> Result<Output> {
        let mut command = self.command(args);
        for (key, value) in envs {
            command.env(key, value);
        }
        tokio::task::spawn_blocking(move || {
            command
                .output()
                .context("running governance-auth subprocess")
        })
        .await
        .context("run_with_env harness task panicked")?
    }

    pub fn issuer(&self) -> &str {
        &self.issuer
    }

    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    /// Runs `login` (the loopback browser flow), acting as "the browser" by
    /// hitting the printed redirect URI directly with a synthetic
    /// authorization code -- no real browser or `xdg-open`/`open` needed.
    pub async fn login_via_browser(&self) -> Result<Output> {
        self.login_with_browser_action(correct_state_action).await
    }

    /// Same as [`Self::login_via_browser`], but lets the caller control
    /// what "the browser" submits back to the loopback callback -- used to
    /// exercise the client-side `state` check with a tampered value.
    ///
    /// Blocking child-process I/O runs on a dedicated thread so it never
    /// starves the async test's own runtime, which the mock IdP server
    /// depends on to keep answering the child's discovery/token requests.
    pub async fn login_with_browser_action(
        &self,
        act: impl FnOnce(&str) -> Result<()> + Send + 'static,
    ) -> Result<Output> {
        let mut command = self.command(&["login"]);
        command.stdout(Stdio::piped()).stderr(Stdio::piped());

        tokio::task::spawn_blocking(move || -> Result<Output> {
            let mut child = command.spawn().context("spawn governance-auth login")?;
            let stderr = child.stderr.take().context("child stderr pipe")?;
            let mut reader = std::io::BufReader::new(stderr);

            let mut seen_stderr = String::new();
            let mut authorize_url = None;
            let mut line = String::new();
            loop {
                line.clear();
                let bytes = reader
                    .read_line(&mut line)
                    .context("reading child stderr")?;
                if bytes == 0 {
                    break;
                }
                seen_stderr.push_str(&line);
                let trimmed = line.trim();
                if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
                    authorize_url = Some(trimmed.to_owned());
                    break;
                }
            }

            if let Some(url) = authorize_url {
                act(&url)?;
            }

            let mut remaining_stderr = String::new();
            reader
                .read_to_string(&mut remaining_stderr)
                .context("draining remaining child stderr")?;
            seen_stderr.push_str(&remaining_stderr);

            let mut stdout = String::new();
            if let Some(mut out) = child.stdout.take() {
                out.read_to_string(&mut stdout)
                    .context("reading child stdout")?;
            }

            let status = child.wait().context("waiting for governance-auth login")?;

            Ok(Output {
                status,
                stdout: stdout.into_bytes(),
                stderr: seen_stderr.into_bytes(),
            })
        })
        .await
        .context("login harness task panicked")?
    }

    /// The LEGACY session location, pre state/cache split. Kept so the
    /// migration test can seed a session where an older build left one.
    pub fn legacy_cache_dir(&self) -> PathBuf {
        let base = if cfg!(target_os = "macos") {
            self.home.path.join("Library").join("Caches")
        } else {
            self.home.path.join(".cache")
        };
        base.join("governance-auth")
    }

    /// Where the session lives now. Mirrors `cache::state_dir`: a refresh
    /// token is STATE, not cache -- see that module's doc for why the
    /// distinction is load-bearing rather than cosmetic.
    fn state_dir(&self) -> PathBuf {
        let base = if cfg!(target_os = "macos") {
            self.home.path.join("Library").join("Application Support")
        } else {
            self.home.path.join(".local").join("state")
        };
        base.join("governance-auth")
    }

    fn session_file_name(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.issuer.as_bytes());
        hasher.update(b"\0");
        hasher.update(self.client_id.as_bytes());
        format!("{}.json", hex::encode(hasher.finalize()))
    }

    pub fn legacy_session_path(&self) -> PathBuf {
        self.legacy_cache_dir().join(self.session_file_name())
    }

    /// Mirrors `cache::cache_key`/`cache::session_path` (private to `src/`,
    /// so re-derived here) so tests can inspect the session file the binary
    /// itself would read and write.
    pub fn session_path(&self) -> PathBuf {
        self.state_dir().join(self.session_file_name())
    }

    pub fn seed_session(&self, session: &serde_json::Value) -> Result<()> {
        let dir = self.state_dir();
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("creating state dir {}", dir.display()))?;
        std::fs::write(self.session_path(), session.to_string()).context("writing seeded session")
    }

    /// Seeds a session at the OLD path, as an older build would have left it.
    pub fn seed_legacy_session(&self, session: &serde_json::Value) -> Result<()> {
        let dir = self.legacy_cache_dir();
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("creating legacy cache dir {}", dir.display()))?;
        std::fs::write(self.legacy_session_path(), session.to_string())
            .context("writing seeded legacy session")
    }
}

/// The well-behaved browser: submits the `state` the authorize URL actually
/// carried.
pub fn correct_state_action(authorize_url: &str) -> Result<()> {
    hit_callback(authorize_url, None)
}

/// A tampered browser: submits a `state` that doesn't match the one the
/// authorize URL carried, simulating a forged/replayed callback. The client
/// must reject this rather than accepting whatever code comes back.
pub fn wrong_state_action(authorize_url: &str) -> Result<()> {
    hit_callback(authorize_url, Some("attacker-supplied-state"))
}

fn hit_callback(authorize_url: &str, state_override: Option<&str>) -> Result<()> {
    let parsed = url::Url::parse(authorize_url).context("parsing authorize url")?;
    let mut redirect_uri = None;
    let mut state = None;
    for (key, value) in parsed.query_pairs() {
        match key.as_ref() {
            "redirect_uri" => redirect_uri = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            _ => {}
        }
    }
    let redirect_uri = redirect_uri.context("authorize url missing redirect_uri")?;
    let state = match state_override {
        Some(overridden) => overridden.to_owned(),
        None => state.context("authorize url missing state")?,
    };

    let mut callback_url = url::Url::parse(&redirect_uri).context("parsing redirect_uri")?;
    callback_url
        .query_pairs_mut()
        .append_pair("code", "test-authorization-code")
        .append_pair("state", &state);

    let host = callback_url
        .host_str()
        .context("callback url has no host")?;
    let port = callback_url.port().context("callback url has no port")?;
    let path = match callback_url.query() {
        Some(query) => format!("{}?{query}", callback_url.path()),
        None => callback_url.path().to_owned(),
    };

    let mut stream = TcpStream::connect((host, port)).context("connecting to loopback callback")?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .context("writing callback request")?;
    let mut response = String::new();
    // Best-effort: the callback server closes the connection after writing
    // its response, so a read error/EOF here isn't itself a test failure.
    let _ = stream.read_to_string(&mut response);
    Ok(())
}
