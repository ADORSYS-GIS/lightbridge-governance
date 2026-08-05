//! Cross-platform "open a URL in the system browser". Best-effort: callers
//! print the URL first, so a failure here just means the user copies it
//! manually instead of the tab opening for them.

use std::process::Command;

use anyhow::{Context, Result, bail};

pub fn open(url: &str) -> Result<()> {
    let (program, args): (&str, Vec<&str>) = if cfg!(target_os = "macos") {
        ("open", vec![url])
    } else if cfg!(target_os = "linux") {
        ("xdg-open", vec![url])
    } else {
        bail!("no known browser-opening command for this platform");
    };

    let status = Command::new(program)
        .args(&args)
        .status()
        .with_context(|| format!("spawning `{program}` to open the browser"))?;

    if !status.success() {
        bail!("`{program}` exited with {status}");
    }
    Ok(())
}
