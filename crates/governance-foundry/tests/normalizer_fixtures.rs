//! Fixture replay harness for RFC-0002's Verification requirement: "The
//! golden-dataset fixture replays through the real collector config in CI on
//! every change to collector config, normalization, pricing or policy
//! logic."
//!
//! This harness covers the **normalization** half of that sentence only --
//! payload in, [`TelemetryPayload`] out -- not the collector config, pricing,
//! or policy halves. See `fixtures/README.md` for exactly what is and is not
//! retired by this file.
//!
//! For every `<case>.json` under `fixtures/synthetic/<provider>/` and
//! `fixtures/captured/<provider>/`, this runs the file through the
//! normalizer registered for `<provider>` and compares the result against a
//! committed `<case>.expected.json` snapshot. A future change to
//! normalization that alters output for any case fails this test and the
//! diff names the field that moved -- that is the whole value proposition:
//! it locks in *today's* behavior so a change is caught in review, not in
//! production. It cannot and does not prove today's behavior matches a real
//! provider's wire format (see fixtures/README.md's honesty note).
//!
//! Both `synthetic/` and `captured/` are walked identically and by directory
//! structure alone: dropping a real capture under `fixtures/captured/<provider>/`
//! plus its `.expected.json` snapshot is picked up with no change to this
//! file.
//!
//! Snapshots are committed, hand-rolled JSON (no snapshot-testing crate is in
//! the workspace, and RFC-0002's fixture requirement does not need one). To
//! (re)generate them after a deliberate normalizer change, run:
//!
//! ```text
//! GOVERNANCE_FOUNDRY_BLESS_FIXTURES=1 cargo test -p governance-foundry --test normalizer_fixtures
//! ```
//!
//! and review the resulting diff by hand before committing -- blessing is a
//! way to avoid hand-typing `chrono`'s RFC3339 rendering of a test
//! timestamp, not a way to skip reviewing what changed.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use governance_foundry::normalizer::{
    Normalizer, TelemetryPayload, claude_code::ClaudeCodeNormalizer, codex::CodexNormalizer,
    foundry::FoundryNormalizer,
};
use serde::Serialize;
use serde_json::Value;

/// Mirrors `Result<TelemetryPayload, NormalizerError>` as JSON so both the
/// success and the rejection cases share one snapshot shape. Errors are
/// captured by `Display` text -- stable, human-readable, and the crate's
/// actual public contract (`Debug` is not) -- not by matching on a variant,
/// so a change to *which* variant fires, or to its message, both show up as
/// a snapshot diff.
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    Ok(TelemetryPayload),
    Err(String),
}

/// The one seam between a fixture directory name and the normalizer it
/// exercises. Mirrors the crate's own `NORMALIZERS` dispatch table
/// (`src/normalizer.rs`) except keyed by the directory name used under
/// `fixtures/`, which is the module name (`foundry`) rather than the wire
/// provider string (`microsoft_foundry`) the two intentionally differ by.
fn normalizer_for(provider: &str) -> Option<&'static dyn Normalizer> {
    match provider {
        "claude_code" => Some(&ClaudeCodeNormalizer),
        "codex" => Some(&CodexNormalizer),
        "foundry" => Some(&FoundryNormalizer),
        _ => None,
    }
}

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

/// One fixture case: which root it came from (for error messages), which
/// provider directory, the case's base name, and its two files.
struct Case {
    root_kind: &'static str,
    provider: String,
    name: String,
    input_path: PathBuf,
    expected_path: PathBuf,
}

/// Walks `fixtures/{synthetic,captured}/<provider>/*.json` (excluding
/// `*.expected.json`, which is the snapshot half of the pair, not a case).
/// Returns `io::Error` rather than panicking/unwrapping: this is a free
/// function, not itself a `#[test]` or `#[cfg(test)]` item, so clippy's
/// test carve-out in `clippy.toml` does not apply to it (see the same
/// pattern in `crates/governance-copilot/tests/store.rs`'s `pool()`/`count()`).
/// Its caller is the `#[test]` function below, which the carve-out *does*
/// cover, so it unwraps the `io::Result` there instead.
fn discover_cases() -> io::Result<Vec<Case>> {
    let mut cases = Vec::new();

    for root_kind in ["synthetic", "captured"] {
        let root = fixtures_root().join(root_kind);
        if !root.is_dir() {
            continue;
        }

        for provider_entry in fs::read_dir(&root)? {
            let provider_entry = provider_entry?;
            if !provider_entry.file_type()?.is_dir() {
                continue;
            }
            let provider_dir = provider_entry.path();
            let provider = provider_dir
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| {
                    io::Error::other(format!("non-UTF-8 directory name under {root:?}"))
                })?
                .to_owned();

            for case_entry in fs::read_dir(&provider_dir)? {
                let case_entry = case_entry?;
                let path = case_entry.path();
                let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                if !name.ends_with(".json") || name.ends_with(".expected.json") {
                    continue;
                }
                let case_name = name
                    .strip_suffix(".json")
                    .ok_or_else(|| io::Error::other("checked suffix vanished"))?
                    .to_owned();
                let expected_path = provider_dir.join(format!("{case_name}.expected.json"));
                cases.push(Case {
                    root_kind,
                    provider: provider.clone(),
                    name: case_name,
                    input_path: path,
                    expected_path,
                });
            }
        }
    }

    Ok(cases)
}

#[test]
fn normalizer_output_matches_committed_snapshots() {
    let bless = std::env::var_os("GOVERNANCE_FOUNDRY_BLESS_FIXTURES").is_some();

    let cases = discover_cases().expect("discover fixtures under fixtures/");
    assert!(
        !cases.is_empty(),
        "fixture discovery under {:?} found zero cases -- fixtures/ is missing or misconfigured, \
         which would make this whole test a silent no-op",
        fixtures_root()
    );

    let mut ran = 0usize;
    let mut failures = Vec::new();

    for case in cases {
        let Some(normalizer) = normalizer_for(&case.provider) else {
            failures.push(format!(
                "{}/{}/{}: no normalizer registered for directory name '{}' -- fix the \
                 directory name or add an arm to normalizer_for()",
                case.root_kind, case.provider, case.name, case.provider
            ));
            continue;
        };

        let raw = fs::read_to_string(&case.input_path)
            .unwrap_or_else(|e| panic!("read {:?}: {e}", case.input_path));
        let input: Value = serde_json::from_str(&raw)
            .unwrap_or_else(|e| panic!("parse {:?} as JSON: {e}", case.input_path));

        let outcome = match normalizer.normalize(&input) {
            Ok(payload) => Outcome::Ok(payload),
            Err(e) => Outcome::Err(e.to_string()),
        };
        let mut actual = serde_json::to_string_pretty(&outcome).unwrap_or_else(|e| {
            panic!("serialize outcome for {}/{}: {e}", case.provider, case.name)
        });
        actual.push('\n');

        if bless {
            fs::write(&case.expected_path, &actual)
                .unwrap_or_else(|e| panic!("write {:?}: {e}", case.expected_path));
            ran += 1;
            continue;
        }

        let expected = fs::read_to_string(&case.expected_path).unwrap_or_else(|e| {
            panic!(
                "read {:?} (run with GOVERNANCE_FOUNDRY_BLESS_FIXTURES=1 to create it): {e}",
                case.expected_path
            )
        });

        if actual != expected {
            failures.push(format!(
                "{}/{}/{}: normalizer output no longer matches the committed snapshot at {:?}\n\
                 --- expected ---\n{expected}--- actual ---\n{actual}",
                case.root_kind, case.provider, case.name, case.expected_path
            ));
        }
        ran += 1;
    }

    // Green-does-not-mean-tested (AGENTS.md): a harness that silently skips
    // every case would report as passed. Assert it actually exercised
    // something.
    assert!(
        ran > 0,
        "fixture harness discovered cases but ran zero of them"
    );

    assert!(
        failures.is_empty(),
        "{} fixture snapshot(s) mismatched:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}
