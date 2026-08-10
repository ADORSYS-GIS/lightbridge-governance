//! Test-only support code. `tests/support/mod.rs` is a Cargo-recognized
//! filename that is never itself compiled as a separate test binary --
//! nothing under `tests/` is reachable from `src/`, matching the house rule
//! that mocks stay off any production path (AGENTS.md).

// Each `tests/*.rs` file is compiled as its own independent test binary that
// includes this whole `support` module, but which items go unused varies per
// binary. That's why this is `allow`, not `expect`, mirroring
// app/governance-auth/tests/support/mod.rs: `expect` only fits a suppression
// that's uniformly needed everywhere it's placed, and this one's necessity
// is genuinely binary-dependent.
#[allow(dead_code)]
pub mod mock_github;
#[allow(dead_code)]
pub mod test_app_key;
