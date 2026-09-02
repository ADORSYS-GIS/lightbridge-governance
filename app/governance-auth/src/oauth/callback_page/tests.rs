//! What the callback page must be true of, now that it is built elsewhere.
//!
//! The tests that matter are the ones guarding the seam. The markup comes from
//! another repository's build, so this file's job is to catch the ways a
//! re-vendor can go wrong: a stale digest, a placeholder that stopped being
//! substituted, or an artifact that quietly started reaching the network.

use super::*;

/// Attributes a browser will fetch. `data`/`poster` are here because they are
/// fetchable too and cost nothing to check.
const FETCHING_ATTRS: [&str; 5] = ["src=", "href=", "srcset=", "poster=", "data="];

/// The only absolute URLs the artifact is allowed to contain, none of which is
/// ever fetched.
///
/// ⚠️ This allowlist replaced three bare substring probes (`http://`,
/// `https://`, `src=`) that were correct against a hand-written template and
/// became FALSE POSITIVES against a real React bundle:
///
/// - `http://www.w3.org/...` — XML namespace constants React uses to create
///   SVG and MathML elements. Identifiers, never requested.
/// - `https://react.dev/errors/` — the text of a minified-error message React
///   throws. A string in a `throw`, not a resource.
/// - `src=` — appears inside `@font-face { src: url(data:...) format("woff2") }`
///   and in React's own DOM-property tables.
///
/// Keeping the bare probes would have meant either a permanently red test or
/// deleting the check. Asserting on *tags* keeps the property and drops the
/// noise.
const ALLOWED_URLS: [&str; 5] = [
    "http://www.w3.org/2000/svg",
    "http://www.w3.org/1998/Math/MathML",
    "http://www.w3.org/1999/xlink",
    "http://www.w3.org/XML/1998/namespace",
    "https://react.dev/errors/",
];

/// `page` with the BODY of every `<script>` and `<style>` removed, opening
/// tags kept.
///
/// ⚠️ Required, not tidiness. React's minified bundle uses `<` as a comparison
/// operator constantly (`e<t`, `25<=u`, `8>`), so scanning raw bytes for
/// `<`…`>` swallows an entire function body as one enormous "tag" and then
/// finds `href=` inside a string literal in it. That is a false positive, and
/// it is the same class of mistake the three retired probes made.
///
/// The browser does not parse tags in these elements either -- their content
/// is raw text until the closing tag -- so removing the bodies is what makes
/// this scan agree with the parser it stands in for. Opening tags are kept, so
/// a `<script src=…>` would still be caught.
fn markup_only(page: &str) -> String {
    let mut out = String::with_capacity(page.len());
    let mut rest = page;
    'outer: loop {
        let mut next = None;
        for tag in ["script", "style"] {
            if let Some(at) = rest.find(&format!("<{tag}"))
                && next.is_none_or(|(best, _)| at < best)
            {
                next = Some((at, tag));
            }
        }
        let Some((at, tag)) = next else {
            break;
        };
        let Some(head) = rest.get(..at) else {
            break 'outer;
        };
        out.push_str(head);
        let Some(tail) = rest.get(at..) else {
            break;
        };
        // Keep the opening tag itself, drop everything to the closing tag.
        let Some(open_end) = tail.find('>') else {
            break;
        };
        if let Some(open) = tail.get(..=open_end) {
            out.push_str(open);
        }
        let close = format!("</{tag}>");
        let Some(body) = tail.get(open_end + 1..) else {
            break;
        };
        let Some(close_at) = body.find(&close) else {
            break;
        };
        let Some(next_rest) = body.get(close_at..) else {
            break;
        };
        rest = next_rest;
    }
    out.push_str(rest);
    out
}

/// Every `<tag …>` in `page`, as the text between `<` and the matching `>`.
fn tags(page: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = page;
    while let Some(start) = rest.find('<') {
        let Some(tail) = rest.get(start + 1..) else {
            break;
        };
        let Some(end) = tail.find('>') else {
            break;
        };
        if let Some(tag) = tail.get(..end) {
            out.push(tag);
        }
        let Some(next) = tail.get(end + 1..) else {
            break;
        };
        rest = next;
    }
    out
}

mod external;
mod policy;
mod vendoring;
