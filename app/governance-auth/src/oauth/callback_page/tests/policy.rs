//! The Content-Security-Policy the response carries.
//!
//! ⚠️ These assert the policy's SHAPE. They cannot tell you the hashes are
//! right -- a wrong hash is a blank tab, not a failing assertion. That is
//! verified by rendering the real response in a real browser; see the PR.

use super::*;

#[test]
fn the_response_carries_a_locked_down_policy() {
    let response = http_response(true);
    for directive in [
        "default-src 'none'",
        "base-uri 'none'",
        "form-action 'none'",
        "frame-ancestors 'none'",
        "img-src data:",
        "font-src data:",
    ] {
        assert!(
            response.contains(directive),
            "the callback response must carry {directive:?}"
        );
    }
    assert!(
        !response.contains("'unsafe-inline'"),
        "'unsafe-inline' would permit anything injected, on the one page that receives an auth code"
    );
}

#[test]
fn every_inline_block_is_covered_by_a_hash() {
    // A block the policy does not name is a block the browser will refuse, and
    // the symptom is a blank tab mid-login rather than an error anyone sees.
    let page = document(true);
    let policy = csp::header_value(&page);
    let inline_scripts = page.matches("</script>").count();
    let inline_styles = page.matches("</style>").count();
    assert_eq!(
        policy.matches("'sha256-").count(),
        inline_scripts + inline_styles,
        "policy: {policy}"
    );
}
