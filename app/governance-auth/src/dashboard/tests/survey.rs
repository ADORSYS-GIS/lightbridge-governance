//! `Telemetry::survey` -- what it concludes from a manifest on disk.

use std::collections::BTreeMap;

use crate::{
    dashboard::Telemetry,
    managed::{Manifest, digest, manifest_path, save},
};

fn seed(home: &std::path::Path, keys: &[&str]) {
    let path = manifest_path(home);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    let mut target = BTreeMap::new();
    for key in keys {
        target.insert((*key).to_owned(), digest("whatever we wrote"));
    }
    let mut targets = BTreeMap::new();
    targets.insert(
        home.join(".codex/config.toml").display().to_string(),
        target,
    );
    save(
        &path,
        &Manifest {
            version: 1,
            targets,
        },
    )
    .expect("save manifest");
}

fn survey(home: &std::path::Path) -> Telemetry {
    Telemetry::survey(Some(home), Some("https://otel.example".to_owned()))
}

#[test]
fn no_manifest_means_nothing_was_applied() {
    let home = crate::managed::testutil::tempdir();
    let t = survey(home.path());
    assert!(!t.applied, "an endpoint alone is not an applied endpoint");
    assert!(!t.has_static_token);
}

/// The regression this module exists for. `otel_token` is never persisted, so
/// the old `config.otel_token.is_some()` test reported "no OTLP token" on every
/// run after the login that wrote one. The manifest still knows.
#[test]
fn a_written_codex_header_counts_as_a_static_token() {
    let home = crate::managed::testutil::tempdir();
    seed(
        home.path(),
        &[
            "otel.environment",
            "otel.exporter.otlp-http.endpoint",
            "otel.exporter.otlp-http.headers.Authorization",
        ],
    );
    let t = survey(home.path());
    assert!(t.applied);
    assert!(t.has_static_token, "the header is in Codex's config");
}

#[test]
fn claude_codes_env_form_of_the_header_counts_too() {
    let home = crate::managed::testutil::tempdir();
    seed(home.path(), &["env.OTEL_EXPORTER_OTLP_HEADERS"]);
    assert!(survey(home.path()).has_static_token);
}

/// Telemetry applied, but no credential written -- the case Codex silently
/// fails on.
#[test]
fn telemetry_without_a_header_is_applied_but_untokened() {
    let home = crate::managed::testutil::tempdir();
    seed(
        home.path(),
        &["otel.environment", "otel.exporter.otlp-http.endpoint"],
    );
    let t = survey(home.path());
    assert!(t.applied);
    assert!(!t.has_static_token);
}

/// Inference wiring is not telemetry. A gateway-only `configure` must not make
/// the row claim OTLP is set up.
#[test]
fn inference_keys_alone_are_not_telemetry() {
    let home = crate::managed::testutil::tempdir();
    seed(
        home.path(),
        &[
            "model_provider",
            "model_providers.lightbridge.base_url",
            "model_providers.lightbridge.auth.command",
        ],
    );
    let t = survey(home.path());
    assert!(!t.applied, "inference keys are not OTLP keys");
    assert!(!t.has_static_token);
}

#[test]
fn without_a_home_nothing_is_claimed() {
    let t = Telemetry::survey(None, Some("https://otel.example".to_owned()));
    assert!(!t.applied);
    assert!(!t.has_static_token);
}
