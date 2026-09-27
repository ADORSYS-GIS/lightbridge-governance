//! Split out of `config_persist/tests.rs` (issue #364) purely to keep that
//! file under its grandfathered LoC ceiling after adding the callback-port/
//! callback-bind fields to `OauthConfig::base()`'s test fixture -- these two
//! tests' own subject (the `otel_token` secret never being written, and the
//! file's permissions) is unrelated to that change.

use super::{base, tempdir};
use crate::{config_persist::remember, optout::ClientOptOut};

/// A secret in a second place the developer never chose.
#[test]
fn never_writes_the_otel_token() {
    let dir = tempdir();
    let path = dir.path().join("config.toml");
    remember(&base(), ClientOptOut::default(), &path).expect("write");
    let text = std::fs::read_to_string(&path).expect("read");
    assert!(
        !text.contains("SECRET-DO-NOT-PERSIST"),
        "token persisted: {text}"
    );
    assert!(!text.contains("otel_token ="), "token key written: {text}");
}

#[cfg(unix)]
#[test]
fn is_written_private() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempdir();
    let path = dir.path().join("config.toml");
    remember(&base(), ClientOptOut::default(), &path).expect("write");
    let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "config file must not be group/other readable");
}
