//! Newtypes for values that must never be logged or shipped over the wire.
//!
//! ADR and AGENTS.md rules: never log a token, a signed URL, or a
//! request/response body. These newtypes make that structural rather than a
//! habit -- `Debug`/`Display` print `<redacted>` so a stray `{:?}` cannot leak.

use std::fmt;

/// A value whose `Debug`/`Display` render as `<redacted>`.
///
/// Used for the GitHub App private key and installation tokens. Signed
/// download URLs get no newtype because they only ever exist transiently
/// inside a function and are never printed (`download_host` is derived before
/// the URL is dropped).
#[derive(Clone, PartialEq, Eq)]
pub struct RawSecret(pub String);

impl fmt::Debug for RawSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

impl fmt::Display for RawSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

impl AsRef<str> for RawSecret {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::ops::Deref for RawSecret {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}
