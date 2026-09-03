//! The shape of the tree, pinned.
//!
//! These assert on `clap`'s own model rather than on parsed output, because
//! the properties that matter here are structural: which names exist, that no
//! alias for a retired name crept back in, and that the configuration flags
//! still reach a command written *after* the subcommand -- the ordering both
//! vendors' helper hooks compose (`tests/cli_arg_order.rs` proves the same
//! thing end to end, through the real binary).

mod help;

use clap::CommandFactory;

use super::*;

/// Whether `path` walks to a real command in this tree.
///
/// Test-only, and the reason [`invoke`] can be trusted: it turns "the string
/// we write into someone else's config file names a command we still have"
/// from a claim into an assertion.
pub(super) fn accepts(path: &[&str]) -> bool {
    let mut command = Cli::command();
    for word in path {
        let Some(found) = command.find_subcommand(word).cloned() else {
            return false;
        };
        command = found;
    }
    true
}

#[test]
fn the_tree_is_the_documented_one() {
    for path in [
        &["login"][..],
        &["token"],
        &["refresh"],
        &["status"],
        &["configure"],
        &["logout"],
        &["serve", "otel"],
        &["otel", "headers"],
        &["copilot", "push"],
        &["self", "update"],
    ] {
        assert!(accepts(path), "`{}` is missing", path.join(" "));
    }
}

/// The house rule is a hard cutover: no aliases for the retired names, hidden
/// or otherwise. A "just to be safe" alias is how two spellings end up in two
/// developers' config files and the rename never actually finishes.
#[test]
fn no_retired_name_still_resolves() {
    for retired in ["copilot-push", "otel-headers", "self-update"] {
        assert!(
            !accepts(&[retired]),
            "`{retired}` still resolves -- the cutover is supposed to be hard"
        );
    }
}

/// `--check` became `--dry-run` on `self update`, so that one word means
/// "report, change nothing" everywhere in this CLI.
#[test]
fn dry_run_is_the_only_spelling_of_report_and_change_nothing() {
    for path in [&["copilot", "push"][..], &["self", "update"]] {
        let ids = arg_ids(path);
        assert!(
            ids.iter().any(|id| id == "dry_run"),
            "`{}` has no --dry-run; got {ids:?}",
            path.join(" ")
        );
        assert!(
            !ids.iter().any(|id| id == "check"),
            "`{}` still has --check",
            path.join(" ")
        );
    }
}

/// Every argument id declared directly on the command at `path`. Empty when
/// the path does not resolve, which the caller's `--dry-run` assertion then
/// reports as a missing flag -- there is no path here that needs to panic.
fn arg_ids(path: &[&str]) -> Vec<String> {
    let mut command = Cli::command();
    for word in path {
        let Some(found) = command.find_subcommand(word).cloned() else {
            return Vec::new();
        };
        command = found;
    }
    command
        .get_arguments()
        .map(|arg| arg.get_id().to_string())
        .collect()
}

/// The configuration flags are `global`, so they must reach every leaf of the
/// tree -- including one two levels down, which is new. Without this a
/// scheduler unit written as `--issuer … copilot push` and a helper written as
/// `… copilot push --issuer …` would disagree about which one parses.
#[test]
fn configuration_flags_reach_a_nested_leaf_from_either_side() {
    for argv in [
        vec![
            "governance-auth",
            "--issuer",
            "https://issuer.example",
            "--client-id",
            "c",
            "copilot",
            "push",
        ],
        vec![
            "governance-auth",
            "copilot",
            "push",
            "--issuer",
            "https://issuer.example",
            "--client-id",
            "c",
        ],
    ] {
        let rendered = argv.join(" ");
        assert!(
            Cli::try_parse_from(argv).is_ok(),
            "`{rendered}` must parse; global flags have to reach a nested leaf"
        );
    }
}
