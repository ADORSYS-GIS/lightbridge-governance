//! The page the browser lands on after the loopback redirect.
//!
//! The markup lives in [`templates/callback.html.jinja`], not in this file:
//! HTML, CSS and JS embedded as a Rust string literal get no syntax
//! highlighting, no formatter and a diff that reads as one changed line. The
//! template is `include_str!`d, so the binary still ships as a single file with
//! no template directory to find at run time -- and none to be swapped for
//! another, which matters for a page rendered right after an OAuth callback.
//!
//! Autoescaping is on: the template is registered under a `.html` name, which
//! is what turns it on in minijinja. Nothing here is user-controlled today --
//! every value is a compile-time constant selected by a bool -- but the icon is
//! passed as a list of SVG path `d` attributes rather than raw markup so that
//! stays true if it ever stops being.
//!
//! **`window.close()` is best-effort.** Browsers honour it only for windows
//! opened by script. This tab was reached by a redirect the user followed, so
//! Chrome and Firefox refuse it -- *"Scripts may close only the windows that
//! were opened by them."* The page tries, and when that fails (the common
//! case) says plainly that the tab can be closed. It never claims to have.

use minijinja::{Environment, context};

/// Registered under a `.html` name on purpose -- minijinja keys autoescaping
/// off the extension, so naming it `callback` or `callback.jinja` would render
/// the same bytes with escaping silently OFF.
const TEMPLATE_NAME: &str = "callback.html";
const TEMPLATE: &str = include_str!("templates/callback.html.jinja");

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

/// Renders the page body.
///
/// A template that fails to render must not take the login with it: the
/// session is already cached and valid by this point, so a broken page falls
/// back to plain text rather than turning a successful sign-in into an error.
/// `render_is_infallible_from_the_callers_view` covers it.
fn document(success: bool) -> String {
    let (accent, icon_paths, heading, detail) = if success {
        (
            "#059669",
            // Inline SVG rather than an emoji: emoji render differently per
            // platform and some browsers drop them entirely.
            vec!["M20 6 9 17l-5-5"],
            "You're signed in",
            "Your terminal has the session. This tab is finished with.",
        )
    } else {
        (
            "#dc2626",
            vec!["M18 6 6 18", "m6 6 12 12"],
            "Sign-in failed",
            "Nothing was saved. Your terminal has the reason — check there.",
        )
    };

    render(accent, &icon_paths, heading, detail).unwrap_or_else(|_| {
        format!("{heading}. You can close this tab and return to your terminal.")
    })
}

fn render(
    accent: &str,
    icon_paths: &[&str],
    heading: &str,
    detail: &str,
) -> Result<String, minijinja::Error> {
    let mut env = Environment::new();
    env.add_template(TEMPLATE_NAME, TEMPLATE)?;
    env.get_template(TEMPLATE_NAME)?.render(context! {
        accent,
        icon_paths,
        heading,
        detail,
    })
}

#[cfg(test)]
mod tests;
