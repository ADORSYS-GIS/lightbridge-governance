//! PII detection and redaction for the AI request path.
//!
//! This crate owns the *policy* — which entities matter, what happens to them,
//! and what happens when the detector fails — and delegates only "where are the
//! entities in this string" to the [`pii`] crate.
//!
//! # Why we own this
//!
//! The platform previously deployed an off-the-shelf redaction *gateway*
//! (`censgate/redact`) as a front proxy. That put a third party's binary in the
//! fail-closed path of every AI request, and its published release could not
//! execute at all — a glibc mismatch between its build and runtime images meant
//! no container ever started. A binary in that position has to be one we build,
//! test and can fix.
//!
//! Depending on [`pii`] as a *library* is a different proposition: it is a
//! bounded, pure-function dependency (text in, spans out) with no network, no
//! process supervision and no release engineering of ours riding on it. If it
//! stalls we can vendor or replace it without touching the request path.
//!
//! # What this crate does not do
//!
//! - **It does not detect personal names.** See
//!   [`profile::Profile::detects_names`] — the `pii` crate's NER entities need
//!   a model that its `candle-ner` feature does not ship.
//! - **It does not stream.** [`Engine::scan`] takes a complete string. The SSE
//!   hold-back logic belongs to the proxy, which is the only layer that knows
//!   about chunk boundaries.
//!
//! # Example
//!
//! ```
//! use governance_redact::{Engine, Profile, Verdict};
//!
//! let engine = Engine::new(Profile::coding_assistant(), "deployment-salt")?;
//!
//! // Ordinary code passes through untouched.
//! assert_eq!(engine.scan("let x = 1;")?, Verdict::Clean);
//!
//! // A credential stops the request outright.
//! let verdict = engine.scan("ghp_abcdefghijklmnopqrstuvwxyz0123456789")?;
//! assert!(verdict.is_blocked());
//! # Ok::<(), governance_redact::Error>(())
//! ```

pub mod engine;
pub mod error;
pub mod profile;
pub mod secrets;

pub use engine::{Engine, Verdict};
pub use error::{Error, Result};
pub use profile::{Action, Profile};
