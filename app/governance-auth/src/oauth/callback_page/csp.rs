//! The Content-Security-Policy for the callback page, derived **from the page
//! itself**.
//!
//! ## Why derive rather than hardcode
//!
//! The page is built in another repository and vendored (see [`super`]). A
//! hand-written policy listing hashes would go stale the first time that
//! artifact is refreshed, and the failure is the worst kind: the browser
//! silently blocks the script and the developer gets a blank tab at the exact
//! moment they are trying to sign in. Hashing whatever is actually embedded
//! means a re-vendor cannot desynchronise the two.
//!
//! ## Why this page can afford `default-src 'none'`
//!
//! Almost nothing can. This page can, because the build gate in
//! `apps/governance-auth` refuses to emit anything that reaches the network --
//! no `<link>`, no `@import`, every `url()` a `data:` URI, fonts inlined. So
//! the policy is not a compromise between safety and function: it is the
//! narrowest thing that still renders, and it turns "self-contained" from a
//! property a test asserts in another repository into one this response
//! *enforces* in the browser.
//!
//! `frame-ancestors 'none'` and `form-action 'none'` matter more here than the
//! usual boilerplate: this is the redirect target of an OAuth2 authorization
//! code flow, so the URL that lands in the browser carries a `code` in its
//! query string. Framing it or letting it submit a form anywhere are precisely
//! the ways that value leaves the machine.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256};

/// Builds the header value for `page`.
///
/// Every inline `<script>` and `<style>` is hashed, because CSP has no other
/// way to permit inline content without `'unsafe-inline'` -- which would
/// permit *any* inline content, including anything injected, and is the thing
/// worth avoiding on this page of all pages.
pub fn header_value(page: &str) -> String {
    let scripts = hashes(page, "script");
    let styles = hashes(page, "style");
    format!(
        "default-src 'none'; script-src {}; style-src {}; img-src data:; font-src data:; \
         base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
        join(&scripts),
        join(&styles),
    )
}

/// `'none'` when there are no blocks of that kind: an empty source list is a
/// syntax error that browsers treat as blocking everything, which would be the
/// right outcome by accident rather than by statement.
fn join(hashes: &[String]) -> String {
    if hashes.is_empty() {
        return "'none'".to_owned();
    }
    hashes.join(" ")
}

/// `'sha256-<base64>'` for each inline `<tag>…</tag>` body in `page`.
///
/// ⚠️ CSP hashes the bytes **between** the tags, exactly: not trimmed, not
/// normalised. One stray byte and the browser blocks the block. That is why
/// `the_policy_admits_the_page_it_was_built_from` renders the real artifact
/// through a real browser rather than trusting this function to be right.
///
/// A `<script src=…>` has no body to hash, and the build gate guarantees there
/// are none -- but an empty body would hash harmlessly anyway, so this needs no
/// special case.
fn hashes(page: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut out = Vec::new();
    let mut rest = page;

    while let Some(start) = rest.find(&open) {
        let Some(after_open) = rest
            .get(start..)
            .and_then(|s| s.find('>'))
            .map(|i| start + i + 1)
        else {
            break;
        };
        let Some(body) = rest.get(after_open..) else {
            break;
        };
        let Some(end) = body.find(&close) else {
            break;
        };
        let Some(content) = body.get(..end) else {
            break;
        };
        out.push(format!(
            "'sha256-{}'",
            STANDARD.encode(Sha256::digest(content.as_bytes()))
        ));
        let Some(next) = rest.get(after_open + end + close.len()..) else {
            break;
        };
        rest = next;
    }
    out
}
