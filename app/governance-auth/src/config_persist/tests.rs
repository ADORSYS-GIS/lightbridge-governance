//! Tests for [`super`]. Split into their own file purely to keep both
//! halves under the 200-LoC gate; they are ordinary child-module unit
//! tests and reach `super`'s private items exactly as before.

use super::*;

struct TempDir(std::path::PathBuf);
impl TempDir {
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn tempdir() -> TempDir {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "governance-auth-config-persist-{}-{unique}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).expect("create temp dir");
    TempDir(path)
}

fn base() -> OauthConfig {
    OauthConfig {
        issuer: "https://auth.example".to_owned(),
        client_id: "cli".to_owned(),
        scopes: "openid profile".to_owned(),
        audience: None,
        otel_endpoint: Some("https://otel.example".to_owned()),
        otel_token: Some("SECRET-DO-NOT-PERSIST".to_owned()),
        gateway_url: Some("https://api.example".to_owned()),
        profile: crate::profile::Profile::Manual,
        // `Some`: this fixture represents a developer who explicitly chose
        // `manual` (matches `profile` above). `profile_was_not_explicit`
        // below covers the `None` case this field exists for.
        profile_explicit: Some(crate::profile::Profile::Manual),
        copilot_spool_path: None,
        otel_headers_debounce_ms: 240_000,
        open_browser: false,
        token_exchange: None,
    }
}

/// The property the feature exists for: what `login` writes must be what a
/// later command reads back. Asserting the file's text would pass while
/// `config_file` rejected it.
#[test]
fn what_is_written_loads_back_identically() {
    let dir = tempdir();
    let path = dir.path().join("config.toml");
    remember(&base(), &path).expect("write");

    let loaded = crate::config_file::load(&path)
        .expect("the file this module wrote must be loadable")
        .expect("present");
    assert_eq!(loaded.issuer.as_deref(), Some("https://auth.example"));
    assert_eq!(loaded.client_id.as_deref(), Some("cli"));
    assert_eq!(loaded.scopes.as_deref(), Some("openid profile"));
    assert_eq!(loaded.gateway_url.as_deref(), Some("https://api.example"));
    assert_eq!(loaded.profile.as_deref(), Some("manual"));
    assert_eq!(loaded.otel_headers_debounce_ms, Some(240_000));
}

/// The entire reason for `toml_edit` over a serde rewrite.
#[test]
fn preserves_comments_and_keys_it_does_not_own() {
    let dir = tempdir();
    let path = dir.path().join("config.toml");
    fs::write(
        &path,
        "# my note\nissuer = \"https://old\"\notel_token_file = \"/etc/t\"\n",
    )
    .expect("seed");
    remember(&base(), &path).expect("write");

    let text = fs::read_to_string(&path).expect("read");
    assert!(text.contains("# my note"), "comment destroyed: {text}");
    assert!(
        text.contains("otel_token_file"),
        "foreign key dropped: {text}"
    );
    assert!(text.contains("https://auth.example"), "not updated: {text}");
    assert!(!text.contains("https://old"), "stale value kept: {text}");
}

/// A secret in a second place the developer never chose.
#[test]
fn never_writes_the_otel_token() {
    let dir = tempdir();
    let path = dir.path().join("config.toml");
    remember(&base(), &path).expect("write");
    let text = fs::read_to_string(&path).expect("read");
    assert!(
        !text.contains("SECRET-DO-NOT-PERSIST"),
        "token persisted: {text}"
    );
    assert!(!text.contains("otel_token ="), "token key written: {text}");
}

/// Logging in twice must not grow or churn the file.
#[test]
fn is_idempotent() {
    let dir = tempdir();
    let path = dir.path().join("config.toml");
    remember(&base(), &path).expect("first");
    let once = fs::read_to_string(&path).expect("read");
    remember(&base(), &path).expect("second");
    assert_eq!(once, fs::read_to_string(&path).expect("read"));
}

/// An option dropped from the command line must stop applying, not linger.
#[test]
fn clears_a_key_that_is_no_longer_set() {
    let dir = tempdir();
    let path = dir.path().join("config.toml");
    remember(&base(), &path).expect("with gateway");
    assert!(
        fs::read_to_string(&path)
            .expect("read")
            .contains("gateway_url")
    );

    let mut without = base();
    without.gateway_url = None;
    remember(&without, &path).expect("without gateway");
    let text = fs::read_to_string(&path).expect("read");
    assert!(!text.contains("gateway_url"), "stale key survived: {text}");
}

/// #280 review: nothing ever naming a profile must not bake today's compiled
/// default into the file. If it did, a developer who ran `login`/`configure`
/// today under `Profile::default() == Manual` would silently stay pinned to
/// `manual` forever, even after a future build's compiled default changes --
/// only a fresh install with no config file yet would pick up a new default.
#[test]
fn does_not_persist_a_profile_nothing_ever_named() {
    let dir = tempdir();
    let path = dir.path().join("config.toml");
    let mut unnamed = base();
    unnamed.profile_explicit = None;
    remember(&unnamed, &path).expect("write");

    // Not a raw-text `contains("profile")` check: `scopes = "openid
    // profile"` (this fixture's own scopes string) contains that substring
    // too, so it would false-positive on a byte that has nothing to do with
    // the `profile` key. The loaded value below is the unambiguous check.
    let loaded = crate::config_file::load(&path)
        .expect("the file this module wrote must be loadable")
        .expect("present");
    assert_eq!(
        loaded.profile, None,
        "a later resolve must still fall through to the live compiled default"
    );
}

/// The mirror of the test above: a profile something DID name (even from a
/// lower layer, not necessarily this run's own flag) is written, same as
/// every other durable field.
#[test]
fn persists_a_profile_something_named() {
    let dir = tempdir();
    let path = dir.path().join("config.toml");
    remember(&base(), &path).expect("write");

    let loaded = crate::config_file::load(&path)
        .expect("the file this module wrote must be loadable")
        .expect("present");
    assert_eq!(loaded.profile.as_deref(), Some("manual"));
}

#[cfg(unix)]
#[test]
fn is_written_private() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempdir();
    let path = dir.path().join("config.toml");
    remember(&base(), &path).expect("write");
    let mode = fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "config file must not be group/other readable");
}
