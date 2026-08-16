//! `login` must NOT launch a browser by default (issue #141) -- and
//! `--open-browser` must still launch one. Proved by putting a FAKE
//! `xdg-open` on `PATH` (Linux only -- `src/browser.rs` shells out to
//! `xdg-open` on `target_os = "linux"`, `open` on macOS) that records
//! whether it was ever invoked, rather than by inspecting source: a real
//! `xdg-open`/`open` invocation is exactly what this test needs to catch,
//! not a proxy for it.

mod support;

use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

use anyhow::{Context, Result};
use support::{
    harness::{Harness, correct_state_action},
    mock_idp::{MockIdp, TokenBehavior},
};

/// A directory on `PATH` holding a fake `xdg-open` that, instead of opening
/// anything, writes a marker file and exits 0 -- so `browser::open`'s
/// `Command::new("xdg-open").status()` succeeds without ever touching a
/// real browser, and this test can assert on the marker file's existence
/// rather than trying to observe a real desktop action.
struct FakeXdgOpen {
    dir: PathBuf,
    marker: PathBuf,
}

impl FakeXdgOpen {
    fn new() -> Result<Self> {
        let unique = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .context("system clock before unix epoch")?
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("governance-auth-fake-xdg-open-{unique}-{nanos}"));
        fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;

        let marker = dir.join("invoked");
        let script_path = dir.join("xdg-open");
        // `$1` is the URL `browser::open` passes -- unused here, but taking
        // it (rather than requiring zero args) keeps this a faithful stand-in
        // for the real `xdg-open` invocation shape.
        fs::write(
            &script_path,
            format!("#!/bin/sh\ntouch \"{}\"\nexit 0\n", marker.display()),
        )
        .with_context(|| format!("writing {}", script_path.display()))?;
        fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755))
            .with_context(|| format!("chmod +x {}", script_path.display()))?;

        Ok(Self { dir, marker })
    }

    /// `PATH` with this fake binary's directory prepended -- so it's found
    /// first, ahead of any real `xdg-open` that might also be on `PATH`.
    /// `std::env::var("PATH")` failing (unset/non-Unicode) can't itself be an
    /// error worth propagating -- an empty fallback still leaves the fake
    /// binary's directory first, which is all this test needs.
    fn path_env(&self) -> String {
        let existing = std::env::var("PATH").unwrap_or_default();
        format!("{}:{existing}", self.dir.display())
    }

    fn was_invoked(&self) -> bool {
        self.marker.exists()
    }
}

impl Drop for FakeXdgOpen {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[tokio::test]
async fn login_does_not_launch_a_browser_by_default() -> Result<()> {
    let idp = MockIdp::start(TokenBehavior::Succeed {
        access_token: "issued-access-token".to_owned(),
        refresh_token: Some("issued-refresh-token".to_owned()),
        expires_in: 300,
    })
    .await?;
    let harness = Harness::new(&idp.base_url)?;
    let fake_browser = FakeXdgOpen::new()?;

    let output = harness
        .login_with_env_and_browser_action(
            &[],
            &[("PATH", &fake_browser.path_env())],
            correct_state_action,
        )
        .await?;

    assert!(
        output.status.success(),
        "login failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        !fake_browser.was_invoked(),
        "login with no flags must not launch a browser opener by default"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("Opening your browser"),
        "the default-off message must not claim it's opening a browser, got: {stderr}"
    );
    Ok(())
}

#[tokio::test]
async fn open_browser_flag_launches_the_configured_browser_opener() -> Result<()> {
    let idp = MockIdp::start(TokenBehavior::Succeed {
        access_token: "issued-access-token".to_owned(),
        refresh_token: Some("issued-refresh-token".to_owned()),
        expires_in: 300,
    })
    .await?;
    let harness = Harness::new(&idp.base_url)?;
    let fake_browser = FakeXdgOpen::new()?;

    let output = harness
        .login_with_env_and_browser_action(
            &["--open-browser"],
            &[("PATH", &fake_browser.path_env())],
            correct_state_action,
        )
        .await?;

    assert!(
        output.status.success(),
        "login failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        fake_browser.was_invoked(),
        "--open-browser must launch the platform browser opener"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Opening your browser"), "got: {stderr}");
    Ok(())
}

#[tokio::test]
async fn open_browser_env_var_also_launches_the_configured_browser_opener() -> Result<()> {
    let idp = MockIdp::start(TokenBehavior::Succeed {
        access_token: "issued-access-token".to_owned(),
        refresh_token: Some("issued-refresh-token".to_owned()),
        expires_in: 300,
    })
    .await?;
    let harness = Harness::new(&idp.base_url)?;
    let fake_browser = FakeXdgOpen::new()?;

    let output = harness
        .login_with_env_and_browser_action(
            &[],
            &[
                ("PATH", &fake_browser.path_env()),
                ("GOVERNANCE_AUTH_OPEN_BROWSER", "true"),
            ],
            correct_state_action,
        )
        .await?;

    assert!(output.status.success());
    assert!(
        fake_browser.was_invoked(),
        "GOVERNANCE_AUTH_OPEN_BROWSER=true must launch the browser opener just like the flag"
    );
    Ok(())
}
