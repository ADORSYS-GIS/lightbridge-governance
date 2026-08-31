//! Tests for [`super`]. In their own file so the module and its template stay
//! easy to read; they are ordinary child-module unit tests.

use super::*;

/// The template's loudest comment, enforced. A CDN link would break the page
/// offline and leak a referrer from a URL that just carried an authorization
/// code, so this asserts on the rendered output rather than trusting the
/// template to stay clean.
#[test]
fn makes_no_external_requests() {
    for success in [true, false] {
        let page = document(success);
        for probe in ["http://", "https://", "//cdn", "<link", "@import", "src="] {
            assert!(
                !page.contains(probe),
                "page reaches outside for {probe:?} (success={success})"
            );
        }
    }
}

#[test]
fn says_what_actually_happened() {
    // Apostrophe-free substrings: the heading "You're signed in" is escaped to
    // "You&#x27;re signed in" (see `apostrophes_are_escaped_not_mangled`), so a
    // literal match on the source text would be checking the wrong thing.
    assert!(document(true).contains("signed in"));
    assert!(!document(true).contains("Sign-in failed"));
    assert!(document(false).contains("Sign-in failed"));
    assert!(!document(false).contains("signed in"));
}

/// Moving from `format!` to a template turned autoescaping ON, which rewrites
/// the apostrophe in "You're" to `&#39;`. That is correct -- a browser renders
/// it identically -- but it surprised this test suite, so pin it rather than
/// let the next person rediscover it as a bug. Note `&#x27;`, not `&#39;`:
/// the exact entity was measured from the output, not assumed.
#[test]
fn apostrophes_are_escaped_not_mangled() {
    let page = document(true);
    assert!(page.contains("You&#x27;re signed in"), "got:\n{page}");
    assert!(!page.contains("You're signed in"), "escaping regressed");
}

/// Never assert a close that browsers will refuse: the fallback copy must
/// exist so a tab that stays open still tells the user what to do.
#[test]
fn keeps_a_fallback_for_when_close_is_refused() {
    for success in [true, false] {
        let page = document(success);
        assert!(page.contains("window.close()"), "should attempt to close");
        assert!(page.contains("You can close this tab"), "must degrade");
    }
}

/// `Content-Length` is bytes. `.len()` on a `String` is correct;
/// `.chars().count()` would truncate the body in the browser. Pin it.
#[test]
fn content_length_is_bytes_not_chars() {
    let response = http_response(true);
    let (head, body) = response.split_once("\r\n\r\n").expect("headers then body");
    let declared: usize = head
        .lines()
        .find_map(|l| l.strip_prefix("Content-Length: "))
        .expect("Content-Length present")
        .trim()
        .parse()
        .expect("numeric");
    assert_eq!(
        declared,
        body.len(),
        "declared length must equal body bytes"
    );
}

#[test]
fn response_carries_no_referrer_and_no_store() {
    let response = http_response(false);
    assert!(response.contains("Referrer-Policy: no-referrer"));
    assert!(response.contains("Cache-Control: no-store"));
    assert!(response.contains("Content-Type: text/html; charset=utf-8"));
}

/// The template must actually be interpolated. Without this a typo'd variable
/// name renders an empty string and every other assertion above still passes,
/// because they check for text that is hard-coded in the template.
#[test]
fn the_template_is_rendered_not_emitted_raw() {
    let page = document(true);
    assert!(
        !page.contains("{{") && !page.contains("{%"),
        "unrendered template syntax survived into the output:\n{page}"
    );
    // Values that can ONLY be present via interpolation.
    assert!(page.contains("#059669"), "accent not interpolated:\n{page}");
    assert!(
        page.contains(r#"<path d="M20 6 9 17l-5-5"/>"#),
        "icon path not interpolated:\n{page}"
    );
    // The failure icon has two paths; the loop must emit both.
    let failed = document(false);
    assert_eq!(failed.matches("<path d=").count(), 2, "loop dropped a path");
}

/// Autoescaping is keyed off the `.html` name. If someone renames the template
/// registration, escaping silently turns off -- which matters the moment any
/// value stops being a compile-time constant.
#[test]
fn autoescaping_is_on() {
    let mut env = Environment::new();
    env.add_template(TEMPLATE_NAME, "{{ v }}")
        .expect("template");
    let out = env
        .get_template(TEMPLATE_NAME)
        .expect("get")
        .render(context! { v => "<script>x</script>" })
        .expect("render");
    // minijinja also escapes `/` as `&#x2f;`; asserting the exact output
    // rather than a substring is what makes this test able to fail loudly if
    // escaping is ever turned off.
    assert_eq!(
        out, "&lt;script&gt;x&lt;&#x2f;script&gt;",
        "autoescaping is off -- is TEMPLATE_NAME still a .html name?"
    );
}

/// A broken template must not fail the login: the session is cached and valid
/// before this page is written.
#[test]
fn render_is_infallible_from_the_callers_view() {
    // `render` itself can fail; `document` must absorb it.
    assert!(render("#000", &[], "H", "D").is_ok(), "baseline renders");
    let page = document(true);
    assert!(!page.is_empty(), "document must always produce something");
}
