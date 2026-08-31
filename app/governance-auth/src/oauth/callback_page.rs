//! The page the browser lands on after the loopback redirect.
//!
//! Two deliberate constraints shape it:
//!
//! **No external requests.** No CDN stylesheet, webfont or analytics. This is
//! served by a loopback listener on a developer's machine, possibly offline,
//! and renders right after an OAuth callback -- the worst moment to hand a
//! third party a referrer containing a `code`. Everything is inline, so it
//! works offline and leaks nothing. That rules out Tailwind's CDN build; the
//! styles below are hand-written to the same utility-ish look.
//!
//! **`window.close()` is best-effort.** Browsers honour it only for windows
//! opened by script. This tab was reached by a redirect the user followed, so
//! Chrome and Firefox refuse it -- *"Scripts may close only the windows that
//! were opened by them."* The page tries, and when that fails (the common
//! case) says plainly that the tab can be closed. It never claims to have.

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
         Cache-Control: no-store\r\n\
         Referrer-Policy: no-referrer\r\n\
         X-Content-Type-Options: nosniff\r\n\
         Connection: close\r\n\
         \r\n\
         {}",
        body.len(),
        body
    )
}

fn document(success: bool) -> String {
    let (accent, icon, heading, detail) = if success {
        (
            "#059669",
            // Inline SVG rather than an emoji: emoji render differently per
            // platform and some terminals-turned-browsers drop them entirely.
            r#"<path d="M20 6 9 17l-5-5"/>"#,
            "You're signed in",
            "Your terminal has the session. This tab is finished with.",
        )
    } else {
        (
            "#dc2626",
            r#"<path d="M18 6 6 18"/><path d="m6 6 12 12"/>"#,
            "Sign-in failed",
            "Nothing was saved. Your terminal has the reason — check there.",
        )
    };

    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<meta name="referrer" content="no-referrer">
<title>{heading} &middot; governance-auth</title>
<style>
  :root {{ color-scheme: light dark; }}
  * {{ box-sizing: border-box; }}
  body {{
    margin: 0; min-height: 100vh; display: flex; align-items: center;
    justify-content: center; padding: 1.5rem;
    font: 15px/1.55 ui-sans-serif, -apple-system, "Segoe UI", Roboto,
          "Helvetica Neue", Arial, sans-serif;
    background: #f8fafc; color: #0f172a;
  }}
  .card {{
    width: 100%; max-width: 27rem; background: #fff; border: 1px solid #e2e8f0;
    border-radius: 14px; padding: 2rem; text-align: center;
    box-shadow: 0 1px 2px rgba(15,23,42,.04), 0 8px 24px rgba(15,23,42,.06);
  }}
  .icon {{
    width: 3rem; height: 3rem; margin: 0 auto 1.15rem; border-radius: 999px;
    display: flex; align-items: center; justify-content: center;
    background: {accent}14; color: {accent};
  }}
  .icon svg {{ width: 1.6rem; height: 1.6rem; }}
  h1 {{ margin: 0 0 .5rem; font-size: 1.2rem; font-weight: 620; letter-spacing: -.01em; }}
  p {{ margin: 0; color: #475569; }}
  .hint {{
    margin-top: 1.5rem; padding-top: 1.15rem; border-top: 1px solid #e2e8f0;
    font-size: .8125rem; color: #64748b;
  }}
  code {{
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: .95em; background: #f1f5f9; padding: .1rem .3rem; border-radius: 4px;
  }}
  @media (prefers-color-scheme: dark) {{
    body {{ background: #0b1120; color: #e2e8f0; }}
    .card {{ background: #111827; border-color: #1f2937; box-shadow: none; }}
    p {{ color: #94a3b8; }}
    .hint {{ border-top-color: #1f2937; color: #64748b; }}
    code {{ background: #1f2937; }}
  }}
</style>
</head>
<body>
  <main class="card">
    <div class="icon" aria-hidden="true">
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor"
           stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round">{icon}</svg>
    </div>
    <h1>{heading}</h1>
    <p>{detail}</p>
    <p class="hint" id="hint">Closing this tab&hellip;</p>
  </main>
<script>
  // Best-effort. Browsers only allow close() on script-opened windows, so this
  // is expected to fail for a tab the user navigated to -- hence the fallback
  // text rather than a promise we cannot keep. The delay lets the message be
  // read in the case where it does work.
  setTimeout(function () {{
    try {{ window.close(); }} catch (e) {{}}
    // Reached whenever close() was refused, which is the common case.
    document.getElementById('hint').textContent =
      'You can close this tab and return to your terminal.';
  }}, 1200);
</script>
</body>
</html>"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of the module doc's first constraint. A CDN link here
    /// would break the page offline and leak a referrer from a URL that just
    /// carried an authorization code.
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
        assert!(document(true).contains("You're signed in"));
        assert!(!document(true).contains("Sign-in failed"));
        assert!(document(false).contains("Sign-in failed"));
        assert!(!document(false).contains("You're signed in"));
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
}
