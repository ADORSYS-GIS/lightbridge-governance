//! The JSONC refusal: VS Code's `settings.json` legitimately allows comments
//! and trailing commas, which `serde_json` cannot round-trip. A config we
//! can't rewrite losslessly is REFUSED (not silently stripped of its
//! comments), with the exact settings printed for the developer to add by
//! hand. Split out of [`super`] for the LoC ceiling.
//!
//! The third test is the `configure_all`-level consequence: the refusal is a
//! partial outcome, not a failed configure, so it must not abort the rest of
//! the run.

use std::fs;

use super::{settings, settings_gateway_only};
use crate::{
    managed::testutil::tempdir,
    optout::ClientOptOut,
    otel::configure_all,
    vscode::{configure, user_dir},
};

#[test]
fn a_jsonc_vscode_config_is_refused_rather_than_stripped_of_its_comments() {
    // VS Code's settings.json legitimately allows comments. Parsing them out
    // and writing plain JSON back would delete a developer's annotations
    // permanently, so this must decline and tell them what to add -- the file
    // has to come back untouched.
    let home = tempdir();
    let user = user_dir(home.path(), "Code");
    fs::create_dir_all(&user).expect("create VS Code User dir");
    let original = "{\n  // my carefully explained setting\n  \"editor.fontSize\": 14\n}\n";
    fs::write(user.join("settings.json"), original).expect("seed JSONC settings");

    let error = configure(home.path(), &settings())
        .expect_err("a JSONC config must be refused, not silently rewritten");
    let rendered = format!("{error:#}");
    assert!(
        rendered.contains("github.copilot.chat.otel.outfile"),
        "the error must tell the developer exactly what to add; got: {rendered}"
    );

    assert_eq!(
        fs::read_to_string(user.join("settings.json")).expect("read back"),
        original,
        "the file must be left byte-for-byte untouched"
    );
}

#[test]
fn a_gateway_only_jsonc_config_is_refused_rather_than_silently_rewritten() {
    // The JSONC refusal used to apply only to machines with a Copilot path;
    // since the `lightbridge.*` gate (issue #233) a gateway-only machine now
    // has real keys to write, so it reaches the same refuse-don't-clobber path
    // and must pin it: the error tells the developer exactly what to add
    // (including the inference keys), and the file comes back untouched.
    let home = tempdir();
    let user = user_dir(home.path(), "Code");
    fs::create_dir_all(&user).expect("create VS Code User dir");
    let original = "{\n  // my carefully explained setting\n  \"editor.fontSize\": 14\n}\n";
    fs::write(user.join("settings.json"), original).expect("seed JSONC settings");

    let error = configure(home.path(), &settings_gateway_only())
        .expect_err("a JSONC config must be refused, not silently rewritten");
    let rendered = format!("{error:#}");
    assert!(
        rendered.contains("lightbridge.gatewayUrl"),
        "the error must tell the developer the inference key to add; got: {rendered}"
    );

    assert_eq!(
        fs::read_to_string(user.join("settings.json")).expect("read back"),
        original,
        "the file must be left byte-for-byte untouched"
    );
}

#[test]
fn a_jsonc_vscode_config_does_not_abort_the_rest_of_configure() {
    // A refused JSONC settings.json used to be fatal to the whole run: the
    // `Err` from `vscode::configure` skipped `configure_shell_env` and the
    // managed manifest, so a developer with one annotated file silently lost
    // their shell exports and the ownership ledger described the previous
    // run. Refusing one file is a partial outcome -- Claude/Codex and the
    // shell env wrote fine -- so `configure_all` must not undo them.
    let home = tempdir();
    let user = user_dir(home.path(), "Code");
    fs::create_dir_all(&user).expect("create VS Code User dir");
    let original = "{\n  // my carefully explained setting\n  \"editor.fontSize\": 14\n}\n";
    fs::write(user.join("settings.json"), original).expect("seed JSONC settings");

    configure_all(
        home.path(),
        &settings_gateway_only(),
        ClientOptOut::default(),
    )
    .expect("configure_all must not fail because one file is JSONC");

    assert_eq!(
        fs::read_to_string(user.join("settings.json")).expect("read back"),
        original,
        "the annotated VS Code settings must still be left untouched"
    );
    assert!(
        home.path()
            .join(".config")
            .join("governance-auth")
            .join("otel.env")
            .is_file(),
        "the shell env must still be written despite the VS Code refusal"
    );
    assert!(
        home.path()
            .join(".config")
            .join("governance-auth")
            .join("managed.json")
            .is_file(),
        "the ownership manifest must still be saved despite the VS Code refusal"
    );
}
