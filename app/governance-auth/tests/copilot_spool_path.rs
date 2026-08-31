//! ADR-0012 Decision 2's precedence, for the one key `copilot-push` adds.
//!
//! Layer 5 (the compiled default) is the interesting one: it depends on
//! `$HOME`, so it cannot be a clap `default_value` -- a `default_value` fires
//! the instant flag and env are both absent, *before* either config-file layer
//! is consulted, which would make layers 3 and 4 unreachable. That is the trap
//! `config.rs`'s module doc warns about, and these tests are what pin it shut
//! for this key: the file layer must actually be reached, and the flag must
//! still beat it.

mod support;

use std::path::PathBuf;

use anyhow::{Context, Result};
use support::{
    copilot as fixture,
    harness::Harness,
    mock_collector::{Behavior, MockCollector},
};

/// A scratch `XDG_CONFIG_HOME` inside the harness's throwaway `$HOME`, so it
/// is cleaned up with it. Set per-child via `run_with_env`, which overrides
/// the harness's own `env_remove` because `Command` applies env changes in
/// order and the later one wins.
fn config_home(harness: &Harness) -> Result<PathBuf> {
    let root = harness.state_dir().join("scratch-xdg-config");
    let dir = root.join("governance-auth");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(root)
}

fn write_per_user_config(root: &std::path::Path, body: &str) -> Result<()> {
    let path = root.join("governance-auth").join("config.toml");
    std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))
}

#[tokio::test]
async fn the_config_file_layer_supplies_the_spool_path() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    let collector = MockCollector::start(Behavior::Accept).await?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;

    // Somewhere the compiled default would never look.
    let custom = harness.state_dir().join("elsewhere.jsonl");
    fixture::write_spool(&custom, &[fixture::log_line()])?;

    let root = config_home(&harness)?;
    write_per_user_config(
        &root,
        &format!("copilot_spool_path = {:?}\n", custom.display().to_string()),
    )?;

    // No `--copilot-spool-path`: the file layer is the only thing that can
    // point this run at the spool above.
    let output = harness
        .run_with_env(
            &["copilot-push", "--otel-endpoint", &collector.base_url],
            &[("XDG_CONFIG_HOME", &root.display().to_string())],
        )
        .await?;

    assert!(
        output.status.success(),
        "run failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        collector.paths()?,
        vec!["/v1/logs".to_owned()],
        "the record at the config-file path must have been drained"
    );
    Ok(())
}

#[tokio::test]
async fn an_explicit_flag_beats_the_config_file() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    let collector = MockCollector::start(Behavior::Accept).await?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;

    // Distinguishable by signal: the file's spool would produce /v1/logs, the
    // flag's produces /v1/metrics, so "which one was read" is unambiguous.
    let from_file = harness.state_dir().join("from-file.jsonl");
    fixture::write_spool(&from_file, &[fixture::log_line(), fixture::log_line()])?;
    let from_flag = harness.state_dir().join("from-flag.jsonl");
    fixture::write_spool(&from_flag, &[fixture::metrics_line()])?;

    let root = config_home(&harness)?;
    write_per_user_config(
        &root,
        &format!(
            "copilot_spool_path = {:?}\n",
            from_file.display().to_string()
        ),
    )?;

    let output = harness
        .run_with_env(
            &[
                "copilot-push",
                "--otel-endpoint",
                &collector.base_url,
                "--copilot-spool-path",
                &from_flag.display().to_string(),
            ],
            &[("XDG_CONFIG_HOME", &root.display().to_string())],
        )
        .await?;

    assert!(
        output.status.success(),
        "run failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        collector.paths()?,
        vec!["/v1/metrics".to_owned()],
        "only the flag's spool (metrics) may have been read, never the file's (logs)"
    );
    Ok(())
}

/// With nothing configured anywhere, the default is `copilot-otel.jsonl` in
/// the state directory -- the same string the docs tell a developer to paste
/// into `github.copilot.chat.otel.outfile`.
#[tokio::test]
async fn the_compiled_default_is_the_state_directory_spool() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    let collector = MockCollector::start(Behavior::Accept).await?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;
    // `seed_spool` writes to exactly the default location.
    fixture::seed_spool(&harness)?;

    let output = harness
        .run(&["copilot-push", "--otel-endpoint", &collector.base_url])
        .await?;

    assert!(
        output.status.success(),
        "run failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        collector.paths()?,
        vec!["/v1/metrics".to_owned(), "/v1/logs".to_owned()],
        "no flag, no env, no config file: the state-directory default must be found"
    );
    Ok(())
}
