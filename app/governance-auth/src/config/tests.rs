//! Tests for [`super`]. Split into its own file (issue #175) rather than
//! raising the already-grandfathered LoC ceiling -- the same move
//! `otel/tests.rs` made, applied to the whole `mod tests` block.

use super::*;

#[test]
fn rejects_non_loopback_http_issuer() {
    let error = parse_issuer("http://auth.example.com/realms/platform")
        .expect_err("plaintext non-loopback issuer must be rejected");
    assert!(
        error.contains("HTTPS"),
        "error should explain the HTTPS requirement, got: {error}"
    );
}

#[test]
fn accepts_https_issuer() {
    assert!(parse_issuer("https://auth.example.com/realms/platform").is_ok());
}

#[test]
fn accepts_loopback_http_issuer() {
    assert!(parse_issuer("http://127.0.0.1:4181/realms/platform").is_ok());
}

/// `--exchange-token-endpoint` is explicitly not an issuer -- the error
/// message for an unparseable value must say "endpoint", not "issuer",
/// so an operator who typo'd this flag isn't told the wrong flag is
/// wrong. Regression test for the message `parse_issuer` used to
/// produce here (it was reused verbatim, hardcoding "issuer" for both).
#[test]
fn exchange_token_endpoint_parse_error_names_endpoint_not_issuer() {
    let error = parse_exchange_token_endpoint("not a url")
        .expect_err("an unparseable exchange-token-endpoint value must be rejected");
    assert!(
        error.contains("endpoint"),
        "error should say 'endpoint', not 'issuer' -- this flag is a token endpoint, not an \
         issuer, got: {error}"
    );
    assert!(
        !error.contains("issuer"),
        "error should not call this value an issuer, got: {error}"
    );
}

/// ADR-0012 Decision 2's five layers, proved pairwise: flag beats env,
/// env beats per-user file, per-user file beats machine-wide file,
/// machine-wide file beats the compiled default.
///
/// Every test below drives [`OauthConfigArgs::resolve_with_paths`]
/// directly with temp-file paths for the two file layers, rather than
/// going through `resolve()`'s real `/etc/governance-auth/config.toml`
/// and `$HOME`-derived per-user path -- that's what "the paths are
/// injectable" buys: these tests never touch the real filesystem
/// locations, so they're hermetic and safe to run in parallel with
/// every other test in this crate, including ones that touch a real
/// `$HOME` through the subprocess harness in `tests/`.
mod precedence {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    /// Minimal scratch dir, removed on drop -- same hand-rolled pattern
    /// used in `otel.rs`'s and `config_file.rs`'s own test modules.
    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn tempdir() -> TempDir {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "governance-auth-config-precedence-{}-{unique}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp dir");
        TempDir(path)
    }

    /// A path in a freshly-made temp dir that nothing has written to --
    /// `config_file::load` treats a missing file as "this layer has
    /// nothing to say", not an error, so this stands in for "this layer
    /// is absent" throughout.
    fn absent_path(dir: &TempDir) -> std::path::PathBuf {
        dir.path().join("absent.toml")
    }

    fn write_config(dir: &TempDir, name: &str, contents: &str) -> std::path::PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, contents).expect("write test config file");
        path
    }

    /// The minimal always-present layer: issuer/client-id as if parsed
    /// from a flag or env var (clap has already merged those two by the
    /// time `OauthConfigArgs` exists, so this crate has no way to tell
    /// them apart downstream -- see `tests/config_precedence.rs` for the
    /// one layer that's actually clap's job).
    fn base_args() -> OauthConfigArgs {
        OauthConfigArgs {
            issuer: Some("https://issuer.example/realms/platform".to_owned()),
            client_id: Some("cli".to_owned()),
            scopes: None,
            audience: None,
            otel_endpoint: None,
            otel_token: None,
            gateway_url: None,
            profile: None,
            copilot_spool_path: None,
            otel_headers_debounce_ms: None,
            open_browser: None,
            token_exchange: None,
            exchange_issuer: None,
            exchange_token_endpoint: None,
            exchange_client_id: None,
            exchange_scopes: None,
        }
    }

    /// Layer 1 vs layer 2 (clap flag vs its own env var) is proved in
    /// `tests/config_precedence.rs`, end-to-end through a real
    /// subprocess with `GOVERNANCE_AUTH_SCOPES` set on the *child's*
    /// environment via `Command::env`, not here: that precedence step
    /// happens INSIDE clap, before `OauthConfigArgs` is ever
    /// constructed by hand, and proving it would otherwise require
    /// mutating this test binary's own process environment -- which
    /// `std::env::set_var` can do, but only as `unsafe` since Rust 2024,
    /// and this workspace denies `unsafe_code` outright (see root
    /// `Cargo.toml`). A subprocess's environment is safe, ordinary
    /// `Command::env`, so that's where this one lives.
    ///
    /// This is also the exact case the `default_value`/`default_value_t`
    /// trap made impossible to observe: with a clap default in place,
    /// `scopes` was never `None`, so there was nothing for a config file
    /// (or, transitively, an env-var-vs-file test) to ever win against.
    /// Layer 2 vs layer 3: an env-var-sourced value (indistinguishable,
    /// by the time `OauthConfigArgs` exists, from a flag-sourced one --
    /// see the test above) must win over a per-user config file.
    #[test]
    fn env_or_flag_value_wins_over_per_user_file_for_scopes() {
        let dir = tempdir();
        let per_user = write_config(&dir, "per-user.toml", "scopes = \"per-user-scope\"\n");
        let machine = write_config(&dir, "machine.toml", "scopes = \"machine-scope\"\n");

        let args = OauthConfigArgs {
            scopes: Some("cli-or-env-scope".to_owned()),
            ..base_args()
        };
        let resolved = args
            .resolve_with_paths(&per_user, &machine)
            .expect("resolve");
        assert_eq!(resolved.scopes, "cli-or-env-scope");
    }

    /// Layer 3 vs layer 4: with no flag/env value, the per-user file
    /// must win over the machine-wide file.
    #[test]
    fn per_user_file_wins_over_machine_file_for_scopes() {
        let dir = tempdir();
        let per_user = write_config(&dir, "per-user.toml", "scopes = \"per-user-scope\"\n");
        let machine = write_config(&dir, "machine.toml", "scopes = \"machine-scope\"\n");

        let resolved = base_args()
            .resolve_with_paths(&per_user, &machine)
            .expect("resolve");
        assert_eq!(resolved.scopes, "per-user-scope");
    }

    /// Layer 4 vs layer 5: with no flag/env value and no per-user file,
    /// the machine-wide file must win over the compiled default.
    #[test]
    fn machine_file_wins_over_compiled_default_for_scopes() {
        let dir = tempdir();
        let per_user = absent_path(&dir);
        let machine = write_config(&dir, "machine.toml", "scopes = \"machine-scope\"\n");

        let resolved = base_args()
            .resolve_with_paths(&per_user, &machine)
            .expect("resolve");
        assert_eq!(resolved.scopes, "machine-scope");
    }

    /// `profile`'s own five-layer round trip, collapsed into one test
    /// per pair rather than the four separate `scopes` tests above --
    /// same layering call, different field.
    #[test]
    fn profile_per_user_file_wins_over_machine_file() {
        let dir = tempdir();
        let per_user = write_config(&dir, "per-user.toml", "profile = \"manual\"\n");
        let machine = write_config(&dir, "machine.toml", "profile = \"daemon\"\n");

        let resolved = base_args()
            .resolve_with_paths(&per_user, &machine)
            .expect("resolve");
        assert_eq!(resolved.profile, crate::profile::Profile::Manual);
    }

    #[test]
    fn profile_machine_file_wins_over_compiled_default() {
        let dir = tempdir();
        let per_user = absent_path(&dir);
        let machine = write_config(&dir, "machine.toml", "profile = \"manual\"\n");

        let resolved = base_args()
            .resolve_with_paths(&per_user, &machine)
            .expect("resolve");
        assert_eq!(resolved.profile, crate::profile::Profile::Manual);
    }

    /// `Profile::default()` is `Manual` until #268/#272 land (see that
    /// module's doc) -- pinned here the same way
    /// `open_browser_compiled_default_is_false` pins its own field below,
    /// so a change to `Profile::default()` fails a config test, not just
    /// `profile.rs`'s own unit test.
    #[test]
    fn profile_compiled_default_is_manual_when_nothing_else_is_configured() {
        let dir = tempdir();
        let per_user = absent_path(&dir);
        let machine = absent_path(&dir);

        let resolved = base_args()
            .resolve_with_paths(&per_user, &machine)
            .expect("resolve");
        assert_eq!(resolved.profile, crate::profile::Profile::Manual);
    }

    /// A config-file value bypasses clap's `value_parser` entirely, so
    /// this is the layer that actually rejects a bad one -- falsified by
    /// first checking the message names the culprit, mirroring
    /// `profile::tests::an_unrecognised_value_is_rejected_by_name`.
    #[test]
    fn profile_an_invalid_config_file_value_is_rejected_by_name() {
        let dir = tempdir();
        let per_user = absent_path(&dir);
        let machine = write_config(&dir, "machine.toml", "profile = \"sometimes\"\n");

        let error = base_args()
            .resolve_with_paths(&per_user, &machine)
            .expect_err("an unrecognised profile must not silently resolve");
        assert!(format!("{error:#}").contains("sometimes"));
    }

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

    /// `issuer`/`client_id` go through the exact same layering, not a
    /// separate code path -- pinned here so a future refactor that
    /// special-cases them can't quietly drop the file layers for the
    /// two fields that matter most (nothing else works without them).
    #[test]
    fn issuer_and_client_id_also_fall_through_to_config_files() {
        let dir = tempdir();
        let per_user = absent_path(&dir);
        let machine = write_config(
            &dir,
            "machine.toml",
            "issuer = \"https://from-machine-config.example\"\nclient_id = \"machine-client\"\n",
        );

        let args = OauthConfigArgs {
            issuer: None,
            client_id: None,
            ..base_args()
        };
        let resolved = args
            .resolve_with_paths(&per_user, &machine)
            .expect("resolve");
        assert_eq!(resolved.issuer, "https://from-machine-config.example");
        assert_eq!(resolved.client_id, "machine-client");
    }

    /// A config-file-sourced `issuer` gets the same HTTPS-or-loopback
    /// validation a CLI/env one already gets at clap-parse time --
    /// config files bypass clap entirely, so without this a plaintext
    /// typo in `/etc/governance-auth/config.toml` would reach the
    /// network unchecked.
    #[test]
    fn a_config_file_issuer_is_still_validated_for_transport_security() {
        let dir = tempdir();
        let per_user = absent_path(&dir);
        let machine = write_config(
            &dir,
            "machine.toml",
            "issuer = \"http://not-loopback.example\"\n",
        );

        let args = OauthConfigArgs {
            issuer: None,
            ..base_args()
        };
        let error = args
            .resolve_with_paths(&per_user, &machine)
            .expect_err("a plaintext non-loopback issuer from a config file must be rejected");
        assert!(format!("{error:#}").contains("HTTPS"));
    }

    /// Still required when absent from every layer -- config files
    /// don't relax the "issuer/client-id must be present" rule, they
    /// just add two more places it can come from.
    #[test]
    fn missing_issuer_everywhere_is_still_an_error() {
        let dir = tempdir();
        let per_user = absent_path(&dir);
        let machine = absent_path(&dir);

        let args = OauthConfigArgs {
            issuer: None,
            ..base_args()
        };
        let error = args
            .resolve_with_paths(&per_user, &machine)
            .expect_err("no issuer anywhere must be an error");
        assert!(format!("{error:#}").contains("--issuer"));
    }

    /// A malformed config file must fail loudly rather than being
    /// treated as absent -- silently falling through to the next,
    /// weaker layer would hide a real typo in a file an operator
    /// believes is in effect.
    #[test]
    fn a_malformed_per_user_file_is_a_loud_error_not_a_silent_fallthrough() {
        let dir = tempdir();
        let per_user = write_config(&dir, "per-user.toml", "not = [valid toml");
        let machine = absent_path(&dir);

        let error = base_args()
            .resolve_with_paths(&per_user, &machine)
            .expect_err("malformed TOML must be an error, not silently skipped");
        assert!(format!("{error:#}").contains(&per_user.display().to_string()));
    }

    /// `open_browser` (issue #141): with nothing configured anywhere,
    /// the compiled default is `false` -- pinned so the browser default
    /// stays off without needing a clap `default_value` (see this
    /// module's doc and `clap_default_value_would_defeat_the_config_file_layer`
    /// for why that specific mechanism must not be used here either).
    #[test]
    fn open_browser_compiled_default_is_false() {
        let dir = tempdir();
        let per_user = absent_path(&dir);
        let machine = absent_path(&dir);

        let resolved = base_args()
            .resolve_with_paths(&per_user, &machine)
            .expect("resolve");
        assert!(!resolved.open_browser);
    }

    /// A machine-wide file turning `open_browser` on must be honoured --
    /// the field-specific version of the "machine file beats compiled
    /// default" proof every other option already has.
    #[test]
    fn machine_file_wins_over_compiled_default_for_open_browser() {
        let dir = tempdir();
        let per_user = absent_path(&dir);
        let machine = write_config(&dir, "machine.toml", "open_browser = true\n");

        let resolved = base_args()
            .resolve_with_paths(&per_user, &machine)
            .expect("resolve");
        assert!(resolved.open_browser);
    }

    /// `last_no_vscode` (and its three siblings) are memory for
    /// `update::reapply`, not a live setting -- no flag or env var
    /// resolves them, only the file layers, which this pins the same
    /// way `open_browser`'s own compiled-default test does.
    #[test]
    fn last_opt_out_fields_compiled_default_to_false() {
        let dir = tempdir();
        let per_user = absent_path(&dir);
        let machine = absent_path(&dir);

        let resolved = base_args()
            .resolve_with_paths(&per_user, &machine)
            .expect("resolve");
        assert!(!resolved.last_no_claude);
        assert!(!resolved.last_no_codex);
        assert!(!resolved.last_no_vscode);
        assert!(!resolved.last_codex_telemetry_only);
    }

    /// A per-user file recording `no_vscode = true` (what
    /// `config_persist::remember` writes after a `--no-vscode` run) must
    /// resolve back to `true` -- the read half of the round trip
    /// `config_persist::tests::the_opt_out_choice_round_trips_per_field`
    /// covers the write half of.
    #[test]
    fn per_user_file_resolves_last_no_vscode() {
        let dir = tempdir();
        let per_user = write_config(&dir, "per-user.toml", "no_vscode = true\n");
        let machine = absent_path(&dir);

        let resolved = base_args()
            .resolve_with_paths(&per_user, &machine)
            .expect("resolve");
        assert!(resolved.last_no_vscode);
        assert!(
            !resolved.last_no_claude,
            "an unrelated field must not also flip"
        );
    }

    /// A per-user file must win over a machine-wide file for
    /// `open_browser` too.
    #[test]
    fn per_user_file_wins_over_machine_file_for_open_browser() {
        let dir = tempdir();
        let per_user = write_config(&dir, "per-user.toml", "open_browser = true\n");
        let machine = write_config(&dir, "machine.toml", "open_browser = false\n");

        let resolved = base_args()
            .resolve_with_paths(&per_user, &machine)
            .expect("resolve");
        assert!(resolved.open_browser);
    }

    /// A flag/env value must win over both file layers for
    /// `open_browser` too.
    #[test]
    fn flag_or_env_value_wins_over_both_files_for_open_browser() {
        let dir = tempdir();
        let per_user = write_config(&dir, "per-user.toml", "open_browser = true\n");
        let machine = write_config(&dir, "machine.toml", "open_browser = true\n");

        let args = OauthConfigArgs {
            open_browser: Some(false),
            ..base_args()
        };
        let resolved = args
            .resolve_with_paths(&per_user, &machine)
            .expect("resolve");
        assert!(
            !resolved.open_browser,
            "an explicit false from a flag/env must win over both files' true"
        );
    }

    /// `--open-browser` used bare (no `=true`) must resolve to `true`
    /// through REAL clap parsing -- the tri-state `Option<bool>`
    /// (`num_args = 0..=1` + `default_missing_value`) equivalent of
    /// `clap_default_value_would_defeat_the_config_file_layer`: this is
    /// the one test in this module that would catch a regression to a
    /// plain `ArgAction::SetTrue` bool flag, which would compile fine
    /// but bake in `false` the instant the flag is absent -- the same
    /// trap `default_value` sets for a string/int field.
    #[test]
    fn open_browser_flag_used_bare_resolves_to_true_through_real_clap_parsing() {
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
            "--open-browser",
        ])
        .expect("parse with a bare --open-browser flag");

        let dir = tempdir();
        let per_user = absent_path(&dir);
        let machine = absent_path(&dir);
        let resolved = cli
            .oauth
            .resolve_with_paths(&per_user, &machine)
            .expect("resolve");
        assert!(
            resolved.open_browser,
            "a bare --open-browser must resolve to true"
        );
    }

    /// Same real-clap-parsing proof for the OTHER direction: with
    /// `--open-browser` never mentioned at all, a real parse must leave
    /// it `None` internally so the machine-wide file still gets a
    /// chance -- catches a regression to a plain bool field with an
    /// implicit `false` default just as surely as the bare-flag test
    /// above catches the opposite mistake.
    #[test]
    fn open_browser_absent_from_a_real_clap_parse_still_falls_through_to_the_machine_file() {
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
        .expect("parse with no --open-browser flag");

        let dir = tempdir();
        let per_user = absent_path(&dir);
        let machine = write_config(&dir, "machine.toml", "open_browser = true\n");
        let resolved = cli
            .oauth
            .resolve_with_paths(&per_user, &machine)
            .expect("resolve");
        assert!(
            resolved.open_browser,
            "with --open-browser absent from the real CLI parse, the machine-wide file's \
             `open_browser = true` must still take effect"
        );
    }

    /// Token exchange (issue #140) is OFF by default: with nothing
    /// configured anywhere, `resolved.token_exchange` must be `None`,
    /// the ONLY representation of "disabled" (see `ExchangeConfig`'s
    /// doc).
    #[test]
    fn token_exchange_is_off_by_default() {
        let dir = tempdir();
        let per_user = absent_path(&dir);
        let machine = absent_path(&dir);

        let resolved = base_args()
            .resolve_with_paths(&per_user, &machine)
            .expect("resolve");
        assert!(resolved.token_exchange.is_none());
    }

    /// Enabling exchange without an exchange client id must be a loud
    /// error naming the missing flag, not a silently-disabled exchange
    /// or a panic reaching for a value that was never there.
    #[test]
    fn token_exchange_enabled_without_a_client_id_is_an_error() {
        let dir = tempdir();
        let per_user = absent_path(&dir);
        let machine = absent_path(&dir);

        let args = OauthConfigArgs {
            token_exchange: Some(true),
            exchange_issuer: Some("https://exchange.example".to_owned()),
            ..base_args()
        };
        let error = args
            .resolve_with_paths(&per_user, &machine)
            .expect_err("token exchange without a client id must be rejected");
        assert!(format!("{error:#}").contains("--exchange-client-id"));
    }

    /// Enabling exchange without EITHER an exchange issuer or an
    /// explicit exchange token endpoint must also be a loud error --
    /// there is nowhere to send the exchange request otherwise.
    #[test]
    fn token_exchange_enabled_without_an_issuer_or_token_endpoint_is_an_error() {
        let dir = tempdir();
        let per_user = absent_path(&dir);
        let machine = absent_path(&dir);

        let args = OauthConfigArgs {
            token_exchange: Some(true),
            exchange_client_id: Some("exchange-cli".to_owned()),
            ..base_args()
        };
        let error = args
            .resolve_with_paths(&per_user, &machine)
            .expect_err("token exchange without an issuer/token-endpoint must be rejected");
        assert!(format!("{error:#}").contains("--exchange-token-endpoint"));
        assert!(format!("{error:#}").contains("--exchange-issuer"));
    }

    /// A fully-specified exchange config (issuer form) resolves to
    /// `Some(ExchangeConfig)` with every field carried through, proving
    /// the happy path end-to-end through `resolve_with_paths` rather
    /// than just its error branches.
    #[test]
    fn token_exchange_fully_specified_via_issuer_resolves() {
        let dir = tempdir();
        let per_user = absent_path(&dir);
        let machine = absent_path(&dir);

        let args = OauthConfigArgs {
            token_exchange: Some(true),
            exchange_issuer: Some("https://exchange.example".to_owned()),
            exchange_client_id: Some("exchange-cli".to_owned()),
            exchange_scopes: Some("openid profile".to_owned()),
            ..base_args()
        };
        let resolved = args
            .resolve_with_paths(&per_user, &machine)
            .expect("resolve");
        let exchange = resolved
            .token_exchange
            .expect("token exchange must be Some when fully specified");
        assert_eq!(exchange.client_id, "exchange-cli");
        assert_eq!(exchange.scopes.as_deref(), Some("openid profile"));
        assert!(matches!(
            exchange.token_endpoint,
            ExchangeTokenEndpoint::Issuer(ref issuer) if issuer == "https://exchange.example"
        ));
    }

    /// An explicit `--exchange-token-endpoint` takes precedence over
    /// `--exchange-issuer` when both are set -- documented behaviour
    /// (that flag's `--help` and `docs/governance-auth/configuration.md`),
    /// pinned here so a refactor can't silently flip which one wins.
    #[test]
    fn token_exchange_explicit_token_endpoint_wins_over_issuer() {
        let dir = tempdir();
        let per_user = absent_path(&dir);
        let machine = absent_path(&dir);

        let args = OauthConfigArgs {
            token_exchange: Some(true),
            exchange_issuer: Some("https://exchange.example".to_owned()),
            exchange_token_endpoint: Some("https://exchange.example/oauth2/token".to_owned()),
            exchange_client_id: Some("exchange-cli".to_owned()),
            ..base_args()
        };
        let resolved = args
            .resolve_with_paths(&per_user, &machine)
            .expect("resolve");
        let exchange = resolved.token_exchange.expect("token exchange resolved");
        assert!(matches!(
            exchange.token_endpoint,
            ExchangeTokenEndpoint::Explicit(ref endpoint)
                if endpoint == "https://exchange.example/oauth2/token"
        ));
    }

    /// Every exchange field falls through the same file layering as
    /// `issuer`/`client_id` -- a machine-wide file alone can fully
    /// configure token exchange, with no flag/env involved at all.
    #[test]
    fn token_exchange_config_falls_through_to_the_machine_wide_file() {
        let dir = tempdir();
        let per_user = absent_path(&dir);
        let machine = write_config(
            &dir,
            "machine.toml",
            "token_exchange = true\n\
             exchange_token_endpoint = \"https://exchange.example/oauth2/token\"\n\
             exchange_client_id = \"machine-exchange-cli\"\n",
        );

        let resolved = base_args()
            .resolve_with_paths(&per_user, &machine)
            .expect("resolve");
        let exchange = resolved.token_exchange.expect("token exchange resolved");
        assert_eq!(exchange.client_id, "machine-exchange-cli");
    }

    /// A per-user file's `token_exchange = false` must win over a
    /// machine-wide file's `token_exchange = true` -- same precedence
    /// every other field has, proved specifically for the boolean gate
    /// rather than just the fields underneath it.
    #[test]
    fn per_user_file_wins_over_machine_file_for_token_exchange_enabled() {
        let dir = tempdir();
        let per_user = write_config(&dir, "per-user.toml", "token_exchange = false\n");
        let machine = write_config(
            &dir,
            "machine.toml",
            "token_exchange = true\n\
             exchange_token_endpoint = \"https://exchange.example/oauth2/token\"\n\
             exchange_client_id = \"machine-exchange-cli\"\n",
        );

        let resolved = base_args()
            .resolve_with_paths(&per_user, &machine)
            .expect("resolve");
        assert!(
            resolved.token_exchange.is_none(),
            "per-user token_exchange = false must win over the machine file's true"
        );
    }

    /// The `token_exchange` counterpart to
    /// `open_browser_absent_from_a_real_clap_parse_still_falls_through_to_the_machine_file`
    /// -- and, like that test, the only one in this module that can
    /// catch a `default_value`/`default_value_t` regression reintroduced
    /// on the real `#[arg(...)]` for `token_exchange`, because it's the
    /// only one that goes through REAL clap parsing
    /// (`TestCli::try_parse_from`) rather than hand-constructing
    /// `OauthConfigArgs { token_exchange: None, .. }`.
    ///
    /// Every OTHER `token_exchange` test above builds `OauthConfigArgs`
    /// by hand, which proves the layering logic in
    /// `resolve_token_exchange` is correct but can never observe a
    /// mistake in the `#[arg(...)]` attribute itself: clap fills a
    /// `default_value` in BEFORE `OauthConfigArgs` exists as a value a
    /// test could construct differently, so a hand-built
    /// `token_exchange: None` stays `None` even if the real CLI would
    /// never produce it. Confirmed by sabotaging with
    /// `default_value = "false"` on the `token_exchange` arg: with that
    /// in place, `--token-exchange` never mentioned still parses to
    /// `Some(false)`, so `resolve_token_exchange` sees `enabled = false`
    /// and never even reads the machine-wide file's `token_exchange =
    /// true` -- this test failed with `resolved.token_exchange` being
    /// `None` instead of `Some`, exactly the trap this module's doc
    /// warns about.
    #[test]
    fn token_exchange_absent_from_a_real_clap_parse_still_falls_through_to_the_machine_file() {
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
        .expect("parse with no --token-exchange flag");

        let dir = tempdir();
        let per_user = absent_path(&dir);
        let machine = write_config(
            &dir,
            "machine.toml",
            "token_exchange = true\n\
             exchange_token_endpoint = \"https://exchange.example/oauth2/token\"\n\
             exchange_client_id = \"machine-exchange-cli\"\n",
        );

        let resolved = cli
            .oauth
            .resolve_with_paths(&per_user, &machine)
            .expect("resolve");
        assert!(
            resolved.token_exchange.is_some(),
            "with --token-exchange absent from the real CLI parse, the machine-wide file's \
             `token_exchange = true` must still take effect -- if this fails with `None` \
             instead, `default_value` has been reintroduced on the `token_exchange` arg"
        );
    }
}
