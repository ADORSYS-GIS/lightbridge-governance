//! The property the single-file design exists for: served from loopback with
//! `governance-auth` about to exit, there is nothing to fetch FROM.
//!
//! These replaced three bare substring probes that were correct against a
//! hand-written template and became false positives against a real React
//! bundle -- see [`super::ALLOWED_URLS`] for which, and why.

use super::*;

#[test]
fn no_tag_fetches_anything_external() {
    // The property the whole single-file design exists for: served from
    // loopback, with `governance-auth` about to exit, there is nothing to
    // fetch FROM. A page that reaches out renders broken exactly when the
    // developer is mid-login.
    for success in [true, false] {
        let page = markup_only(&document(success));
        for tag in tags(&page) {
            for attr in FETCHING_ATTRS {
                let Some(at) = tag.find(attr) else {
                    continue;
                };
                let value = tag.get(at + attr.len()..).unwrap_or_default();
                let value = value.trim_start_matches(['"', '\'']);
                assert!(
                    value.starts_with("data:") || value.starts_with('#') || value.is_empty(),
                    "tag fetches something external: <{tag}> (success={success})"
                );
            }
        }
    }
}

#[test]
fn the_only_absolute_urls_are_identifiers_never_requested() {
    for success in [true, false] {
        let page = document(success);
        for marker in ["http://", "https://"] {
            let mut rest = page.as_str();
            while let Some(at) = rest.find(marker) {
                let tail = rest.get(at..).unwrap_or_default();
                assert!(
                    ALLOWED_URLS.iter().any(|url| tail.starts_with(url)),
                    "unexpected absolute URL: {:?}",
                    tail.get(..60).unwrap_or(tail)
                );
                rest = rest.get(at + marker.len()..).unwrap_or_default();
            }
        }
    }
}

#[test]
fn nothing_links_or_imports_or_uses_a_protocol_relative_url() {
    // These three stayed valid across the rewrite: none of them has a
    // legitimate occurrence in a bundle that inlines everything.
    for success in [true, false] {
        let page = document(success);
        for probe in ["<link", "@import", "//cdn"] {
            assert!(
                !page.contains(probe),
                "page reaches outside for {probe:?} (success={success})"
            );
        }
    }
}
