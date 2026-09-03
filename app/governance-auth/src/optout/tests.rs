//! What the `--no-…` flags promise, and how each promise reads to whoever ran
//! the command.
//!
//! Everything here is the easy half: a file that was not written, an outcome
//! that says which. The half that destroys configuration when it is wrong --
//! a skipped client's keys being *retracted* on the next run -- is
//! [`retraction`], and it is the test to read first.

mod retraction;

use std::{collections::BTreeMap, fs, path::Path};

use super::ClientOptOut;
use crate::{
    managed::testutil::{TempDir, tempdir},
    otel::{OtelSettings, Outcome, configure_all},
    redacted::Redacted,
    vscode,
};

const NO_CODEX: ClientOptOut = ClientOptOut {
    claude: false,
    codex: true,
    vscode: false,
};
pub(super) const NO_VSCODE: ClientOptOut = ClientOptOut {
    claude: false,
    codex: false,
    vscode: true,
};
const NONE_OF_THEM: ClientOptOut = ClientOptOut {
    claude: true,
    codex: true,
    vscode: true,
};

/// Telemetry *and* gateway, so every writer in the tree has something to do
/// and an opt-out has something real to decline.
pub(super) fn settings(home: &Path) -> OtelSettings {
    OtelSettings {
        issuer: "https://auth.example".to_owned(),
        client_id: "cli".to_owned(),
        endpoint: Some("https://otel.example.com".to_owned()),
        copilot_spool: home.join("spool").join("copilot-otel.jsonl"),
        copilot_drain_available: true,
        copilot_otlp_direct: false,
        token: Some(Redacted::new("ingest-token".to_owned())),
        headers_helper: None,
        headers_helper_debounce_ms: 240_000,
        resource_attributes: BTreeMap::new(),
        token_command: "/abs/path/governance-auth token".to_owned(),
        gateway_url: Some("https://api.example.com".to_owned()),
    }
}

/// A machine with all three clients installed, so nothing below is skipped for
/// being absent and every outcome is a real decision.
pub(super) fn installed() -> TempDir {
    let home = tempdir();
    fs::create_dir_all(home.path().join(".claude")).expect("claude dir");
    fs::create_dir_all(home.path().join(".codex")).expect("codex dir");
    fs::create_dir_all(vscode::user_dir(home.path(), "Code")).expect("vscode dir");
    home
}

#[test]
fn a_declined_client_is_left_byte_for_byte_alone() {
    let home = installed();
    let codex = home.path().join(".codex").join("config.toml");
    fs::write(&codex, "# mine\n").expect("seed a hand-written codex config");

    configure_all(home.path(), &settings(home.path()), NO_CODEX).expect("configure");

    assert_eq!(
        fs::read_to_string(&codex).expect("read back"),
        "# mine\n",
        "--no-codex must not touch the file at all"
    );
    assert!(
        home.path().join(".claude").join("settings.json").is_file(),
        "and the clients that were not opted out are still configured"
    );
}

/// `Skipped: ~/.codex not present` is a tool the developer could install.
/// `Left alone (--no-codex)` is a choice they made. One line meaning both is a
/// line nobody can act on.
#[test]
fn declining_is_reported_as_a_choice_not_as_an_absent_tool() {
    let home = installed();
    let outcomes = configure_all(home.path(), &settings(home.path()), NO_CODEX).expect("configure");

    assert!(
        outcomes.iter().any(|outcome| matches!(
            outcome,
            Outcome::Declined {
                flag: "--no-codex",
                ..
            }
        )),
        "got: {outcomes:?}"
    );
    assert!(
        !outcomes
            .iter()
            .any(|outcome| matches!(outcome, Outcome::Skipped(_))),
        "every client is installed here, so nothing may report as absent: {outcomes:?}"
    );
}

#[test]
fn declining_a_client_that_is_not_installed_is_a_no_op_not_an_error() {
    let home = tempdir();

    let outcomes = configure_all(home.path(), &settings(home.path()), NO_CODEX)
        .expect("declining an absent client must not be an error");

    assert!(
        outcomes
            .iter()
            .any(|outcome| matches!(outcome, Outcome::Declined { .. })),
        "got: {outcomes:?}"
    );
    assert!(!home.path().join(".codex").exists(), "nothing was created");
}

/// Accepted, not refused -- unlike neither-`--otel-endpoint`-nor-`--gateway-url`,
/// which is a hard error because there is no configuration to compute at all.
/// Here there is: the shell env file is this binary's own, not a client's.
#[test]
fn all_three_at_once_is_accepted_and_still_writes_the_shell() {
    let home = installed();

    let outcomes = configure_all(home.path(), &settings(home.path()), NONE_OF_THEM)
        .expect("all three at once must be accepted");

    for absent in [
        home.path().join(".claude").join("settings.json"),
        home.path().join(".codex").join("config.toml"),
        vscode::user_dir(home.path(), "Code").join("settings.json"),
    ] {
        assert!(!absent.exists(), "{} was written anyway", absent.display());
    }
    assert!(
        home.path()
            .join(".config")
            .join("governance-auth")
            .join("otel.env")
            .is_file(),
        "the shell env file is not a client's, so it is still written"
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, Outcome::Declined { .. }))
            .count(),
        3
    );
}
