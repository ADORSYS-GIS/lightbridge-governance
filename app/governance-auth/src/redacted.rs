//! A generic wrapper for secret-shaped values whose `Debug` is redacted
//! structurally rather than by habit -- mirrors
//! `governance-core::credential::CredentialSecret`'s newtype pattern
//! (AGENTS.md: "Wrap secrets in a newtype whose Debug/Display print
//! `<redacted>` so this is structural, not a habit"). No `Display` impl at
//! all: the one place that legitimately needs the plaintext (`token`'s
//! stdout) calls [`Redacted::expose`] explicitly, so every other call site
//! either redacts (`{:?}`) or fails to compile (`{}`).
//!
//! `#[serde(transparent)]` keeps the on-disk cache JSON and the vendor
//! token-response JSON exactly as if this were a plain `String` -- this
//! type is invisible on the wire, only in `Debug`.

use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Redacted<T>(T);

impl<T> Redacted<T> {
    pub fn new(value: T) -> Self {
        Self(value)
    }

    pub fn expose(&self) -> &T {
        &self.0
    }
}

impl<T> fmt::Debug for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}
