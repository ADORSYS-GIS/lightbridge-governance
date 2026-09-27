//! THE regression guard for the `default_value`/`default_value_t` trap this
//! crate's module doc exists to avoid, for `scopes` and `otel_headers_debounce_ms`.

use super::*;

/// THE regression guard for the `default_value`/`default_value_t`
/// trap this whole module exists to avoid -- and the only test in
/// this file that can catch it, because it's the only one that goes
/// through real clap parsing (`OauthConfigArgs::try_parse_from`)
/// instead of hand-constructing the struct.
///
/// Every other precedence test here builds `OauthConfigArgs` by hand
/// (`OauthConfigArgs { scopes: None, .. }`), which proves the
/// *layering logic* in `resolve_with_paths` is correct but can never
/// observe a mistake in the `#[arg(...)]` attribute itself: clap
/// fills a `default_value` in BEFORE `OauthConfigArgs` exists as a
/// value a test could construct differently, so a hand-built
/// `scopes: None` stays `None` even if the real CLI would never
/// produce it. This test parses `--issuer`/`--client-id` alone (no
/// `--scopes`, and nothing sets `GOVERNANCE_AUTH_SCOPES` in this
/// process) through the ACTUAL `OauthConfigArgs`, then feeds the
/// result into `resolve_with_paths` against a machine-wide file that
/// sets `scopes` -- so a `default_value` reintroduced on the real
/// `#[arg(...)]` would make `scopes` `Some("openid profile
/// offline_access")` right out of clap, and this test would fail
/// with the machine file's value never taking effect.
#[test]
fn clap_default_value_would_defeat_the_config_file_layer() {
    use clap::Parser as _;

    #[derive(Debug, clap::Parser)]
    struct TestCli {
        #[command(flatten)]
        oauth: OauthConfigArgs,
    }

    let cli = TestCli::try_parse_from([
        "governance-auth",
        "--issuer",
        "https://issuer.example",
        "--client-id",
        "cli",
    ])
    .expect("parse with no --scopes flag");

    let dir = tempdir();
    let per_user = absent_path(&dir);
    let machine = write_config(&dir, "machine.toml", "scopes = \"machine-scope\"\n");

    let resolved = cli
        .oauth
        .resolve_with_paths(&per_user, &machine)
        .expect("resolve");
    assert_eq!(
        resolved.scopes, "machine-scope",
        "a real clap parse with no --scopes flag must still fall through to the \
         machine-wide config file -- if this fails with the compiled default instead, \
         `default_value` has been reintroduced on the `scopes` arg"
    );
}

/// The `otel_headers_debounce_ms` counterpart to the test above.
///
/// Note the name: the ORIGINAL trap used `default_value_t = 240_000`
/// (this field was a bare `u64`), but that specific form no longer
/// even compiles once the field is `Option<u64>` --
/// `default_value_t` requires the field's own type to implement
/// `Display`, and `Option<u64>` doesn't. That's a free compile-time
/// guard against literally reintroducing the old attribute verbatim.
/// It does NOT guard against the string form, `default_value =
/// "240000"`, which compiles fine on `Option<u64>` (clap parses the
/// string at runtime) and reintroduces the exact same bug silently
/// -- confirmed by sabotaging with that form specifically, not
/// `default_value_t`, when this test was written.
#[test]
fn clap_default_value_t_would_defeat_the_config_file_layer_for_debounce_ms() {
    use clap::Parser as _;

    #[derive(Debug, clap::Parser)]
    struct TestCli {
        #[command(flatten)]
        oauth: OauthConfigArgs,
    }

    let cli = TestCli::try_parse_from([
        "governance-auth",
        "--issuer",
        "https://issuer.example",
        "--client-id",
        "cli",
    ])
    .expect("parse with no --otel-headers-debounce-ms flag");

    let dir = tempdir();
    let per_user = absent_path(&dir);
    let machine = write_config(&dir, "machine.toml", "otel_headers_debounce_ms = 12345\n");

    let resolved = cli
        .oauth
        .resolve_with_paths(&per_user, &machine)
        .expect("resolve");
    assert_eq!(
        resolved.otel_headers_debounce_ms, 12_345,
        "a real clap parse with no --otel-headers-debounce-ms flag must still fall \
         through to the machine-wide config file -- if this fails with the compiled \
         default instead, `default_value_t` has been reintroduced on that arg"
    );
}
