//! Layer-by-layer precedence for `otel_headers_debounce_ms`.

use super::*;

/// Layer 5: with nothing configured anywhere, the compiled default
/// applies. NOTE: this test alone would still pass if `default_value`
/// were reintroduced on `scopes` -- it hand-constructs
/// `OauthConfigArgs { scopes: None, .. }` directly, bypassing clap
/// entirely, so a clap-level attribute regression is invisible to
/// it. `clap_default_value_would_defeat_the_config_file_layer`,
/// below, is the one that actually goes through clap and catches
/// that specific regression -- see its doc for why.
#[test]
fn compiled_default_applies_when_nothing_else_is_configured_for_scopes() {
    let dir = tempdir();
    let per_user = absent_path(&dir);
    let machine = absent_path(&dir);

    let resolved = base_args()
        .resolve_with_paths(&per_user, &machine)
        .expect("resolve");
    assert_eq!(resolved.scopes, DEFAULT_SCOPES);
}

/// The same compiled-default fallback, for the *other* field the
/// ADR calls out by name (`otel_headers_debounce_ms`) -- a single
/// green test on `scopes` wouldn't catch a `default_value_t` left in
/// place on this one specifically.
#[test]
fn compiled_default_applies_when_nothing_else_is_configured_for_debounce_ms() {
    let dir = tempdir();
    let per_user = absent_path(&dir);
    let machine = absent_path(&dir);

    let resolved = base_args()
        .resolve_with_paths(&per_user, &machine)
        .expect("resolve");
    assert_eq!(
        resolved.otel_headers_debounce_ms,
        DEFAULT_OTEL_HEADERS_DEBOUNCE_MS
    );
}

/// A machine-wide file supplying `otel_headers_debounce_ms` must be
/// consulted at all -- the field-specific version of the
/// machine-vs-default test above.
#[test]
fn machine_file_wins_over_compiled_default_for_debounce_ms() {
    let dir = tempdir();
    let per_user = absent_path(&dir);
    let machine = write_config(&dir, "machine.toml", "otel_headers_debounce_ms = 12345\n");

    let resolved = base_args()
        .resolve_with_paths(&per_user, &machine)
        .expect("resolve");
    assert_eq!(resolved.otel_headers_debounce_ms, 12_345);
}

/// A per-user file must win over a machine-wide file for
/// `otel_headers_debounce_ms` too.
#[test]
fn per_user_file_wins_over_machine_file_for_debounce_ms() {
    let dir = tempdir();
    let per_user = write_config(&dir, "per-user.toml", "otel_headers_debounce_ms = 111\n");
    let machine = write_config(&dir, "machine.toml", "otel_headers_debounce_ms = 222\n");

    let resolved = base_args()
        .resolve_with_paths(&per_user, &machine)
        .expect("resolve");
    assert_eq!(resolved.otel_headers_debounce_ms, 111);
}

/// A flag/env value must win over both file layers for
/// `otel_headers_debounce_ms` too.
#[test]
fn flag_or_env_value_wins_over_both_files_for_debounce_ms() {
    let dir = tempdir();
    let per_user = write_config(&dir, "per-user.toml", "otel_headers_debounce_ms = 111\n");
    let machine = write_config(&dir, "machine.toml", "otel_headers_debounce_ms = 222\n");

    let args = OauthConfigArgs {
        otel_headers_debounce_ms: Some(999),
        ..base_args()
    };
    let resolved = args
        .resolve_with_paths(&per_user, &machine)
        .expect("resolve");
    assert_eq!(resolved.otel_headers_debounce_ms, 999);
}
