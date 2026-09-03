//! Test-only support code. `tests/support/mod.rs` is a Cargo-recognized
//! filename that is never itself compiled as a separate test binary --
//! nothing under `tests/` is reachable from `src/`, matching the house rule
//! that mocks stay off any production path.

// Each `tests/*.rs` file is compiled as its own independent test binary
// that includes this whole `support` module, but which items go unused
// varies per binary (e.g. `token_refresh.rs` happens to exercise all of
// `mock_idp`'s surface, so dead_code wouldn't fire there, while it does in
// `fail_closed.rs`). That's why this is `allow`, not `expect`: `expect`
// only fits a suppression that's uniformly needed everywhere it's placed,
// and this one's necessity is genuinely binary-dependent, not a fact that
// could go stale and get caught by "unfulfilled expectation".
#[allow(dead_code)]
pub mod collector_policy;
#[allow(dead_code)]
pub mod copilot;
#[allow(dead_code)]
pub mod harness;
#[allow(dead_code)]
pub mod interrupt;
#[allow(dead_code)]
pub mod mock_collector;
#[allow(dead_code)]
pub mod mock_idp;
#[allow(dead_code)]
pub mod serve_otel;
