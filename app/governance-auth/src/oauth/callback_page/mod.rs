//! The page the browser lands on after the loopback redirect.
//!
//! The markup is **built in another repository** and vendored here as one
//! self-contained file: `ADORSYS-GIS/converse-frontends`, `apps/governance-auth`
//! (React + Vite). This is the only surface of `governance-auth` a developer
//! ever looks at, and it used to be hand-written HTML in a Jinja template that
//! resembled nothing else in the product. Now it composes the same design
//! primitives as the console and the auth plane, and it keeps doing so without
//! anyone re-typing markup into a Rust string literal.
//!
//! ## Why a vendored file and not a fetch
//!
//! `include_str!` runs at COMPILE time. The alternative -- pulling the artifact
//! in a `build.rs` -- would put the network on the path of every `cargo build`,
//! break offline and air-gapped builds, and make the binary's contents depend
//! on when it was compiled rather than on what is committed. So the artifact is
//! committed, and the *pull* is an explicit, tooling-driven refresh:
//! `scripts/vendor-callback-page.sh`. See [`SOURCE`].
//!
//! ## How the outcome reaches the page
//!
//! The built HTML carries [`STATUS_PLACEHOLDER`] in a `data-callback-status`
//! attribute on `<html>`. This module substitutes exactly one of two
//! compile-time constants for it. There is no templating and no user-controlled
//! value anywhere in the substitution, which is why an HTML-escaping template
//! engine is no longer needed for this page.
//!
//! ⚠️ The page **fails closed**: it renders the *failure* state for anything
//! that is not the literal `success` -- including an unsubstituted placeholder,
//! which is what a botched vendoring would leave behind. The terminal is the
//! source of truth for the real outcome either way, so pointing the developer
//! there is never wrong; claiming a sign-in worked when it did not would be.
//!
//! **`window.close()` is best-effort.** Browsers honour it only for windows
//! opened by script. This tab was reached by a redirect the user followed, so
//! Chrome and Firefox refuse it -- *"Scripts may close only the windows that
//! were opened by them."* The page tries, and when that fails (the common
//! case) says plainly that the tab can be closed. It never claims to have.

mod csp;

/// The vendored artifact. Rebuilt and refreshed by
/// `scripts/vendor-callback-page.sh`, never edited by hand -- `the_vendored_page_matches_its_recorded_digest`
/// fails if it is, which is the point.
const PAGE: &str = include_str!("callback.html");

/// Provenance for [`PAGE`]: which commit of which repository built it, and the
/// digest of what was vendored. Committed beside the artifact so "is this
/// current?" is answerable from this repository alone, without a network call.
#[cfg(test)]
const SOURCE: &str = include_str!("callback.source.json");

/// The token the build leaves in `data-callback-status` for this module to
/// replace. Kept in sync with `apps/governance-auth/index.html` by
/// `the_placeholder_is_present_exactly_once`.
const STATUS_PLACEHOLDER: &str = "__GOVERNANCE_AUTH_CALLBACK_STATUS__";

/// The only two values the page understands. `callback-status.ts` treats
/// anything that is not `success` as a failure, so these are a contract with
/// it rather than free strings.
const STATUS_SUCCESS: &str = "success";
const STATUS_ERROR: &str = "error";

/// Renders the full HTTP response, headers included.
///
/// `success` reflects the **real outcome**: the `state` check and the token
/// exchange have already run by the time this is called. See `authcode::run`
/// for why it is not decided earlier.
pub fn http_response(success: bool) -> String {
    let body = document(success);
    format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Content-Security-Policy: {}\r\n\
         Cache-Control: no-store\r\n\
         Referrer-Policy: no-referrer\r\n\
         X-Content-Type-Options: nosniff\r\n\
         Connection: close\r\n\
         \r\n\
         {}",
        body.len(),
        csp::header_value(&body),
        body
    )
}

/// Substitutes the outcome into the vendored page.
///
/// Infallible by construction, which is a change from the template era: there
/// is no render step left to fail, so the fallback that used to exist for a
/// broken template has nothing to catch. A page that cannot be produced would
/// now be a compile error, not a runtime one.
fn document(success: bool) -> String {
    let status = if success {
        STATUS_SUCCESS
    } else {
        STATUS_ERROR
    };
    PAGE.replace(STATUS_PLACEHOLDER, status)
}

#[cfg(test)]
mod tests;
