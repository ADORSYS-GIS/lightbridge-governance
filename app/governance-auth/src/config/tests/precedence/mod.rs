//! ADR-0012 Decision 2's five layers, proved pairwise: flag beats env,
//! env beats per-user file, per-user file beats machine-wide file,
//! machine-wide file beats the compiled default.
//!
//! Every test drives [`super::OauthConfigArgs::resolve_with_paths`]
//! directly with temp-file paths for the two file layers, rather than
//! going through `resolve()`'s real `/etc/governance-auth/config.toml`
//! and `$HOME`-derived per-user path -- that's what "the paths are
//! injectable" buys: these tests never touch the real filesystem
//! locations, so they're hermetic and safe to run in parallel with
//! every other test in this crate, including ones that touch a real
//! `$HOME` through the subprocess harness in `tests/`.
//!
//! Split into one file per option (issue #364's LoC-gate fallout) --
//! `support` holds the shared scratch-dir/config-file helpers every
//! sibling module here uses.

use super::*;

mod support;
pub(super) use support::{absent_path, base_args, tempdir, write_config};

mod clap_default_trap;
mod debounce;
mod issuer_client;
mod open_browser;
mod open_browser_clap;
mod profile;
mod scopes;
mod token_exchange;
mod token_exchange_endpoint;
