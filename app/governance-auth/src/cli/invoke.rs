//! How this binary spells its own commands when it writes them into somebody
//! else's config file.
//!
//! Three files hold a `governance-auth …` command line that this binary
//! generated: Claude Code's `settings.json` (`apiKeyHelper`,
//! `otelHeadersHelper`), Codex's `config.toml`
//! (`model_providers.*.auth.command`) and the drain's systemd unit / launchd
//! plist. Before this module each of those strings was built at its own call
//! site, so renaming a subcommand meant finding every one of them by grep --
//! and a miss is invisible: the file still parses, the tool still starts, and
//! the only symptom is telemetry that stops arriving.
//!
//! So the spelling lives here, once, and [`crate::dashboard`] compares what is
//! on disk against what these functions produce *now* to tell a developer who
//! upgraded that their wiring is stale. That comparison is only meaningful
//! because there is exactly one generator.
//!
//! ## Absolute paths, always
//!
//! Codex spawns `auth.command` **without a shell** and a scheduler inherits no
//! login `PATH`, so a bare `governance-auth` cannot resolve in either. See
//! [`crate::otel::OtelSettings::token_command`].

/// The drain's subcommand, as argv. A scheduler unit stores argv, not a
/// string, so this is the array form; the two credential helpers below take
/// the string form because that is what their host files hold.
pub const COPILOT_PUSH: [&str; 2] = ["copilot", "push"];

/// The daemon's subcommand, as argv -- the shape ADR-0016 and #268 name
/// (`governance-auth serve --otel`), spelled here for the same reason
/// [`COPILOT_PUSH`] is.
///
/// ⚠️ Unlike `COPILOT_PUSH`, this is a contract on a command that does not
/// exist on this branch yet: `serve --otel` is #268's own deliverable ("the
/// daemon's internals" -- #270 is explicitly out of scope for it), still
/// unmerged as of this constant's addition.
/// `every_generated_command_is_a_command_this_binary_has` deliberately does
/// NOT check this one for exactly that reason -- add it to that test's list
/// once #268 lands and this stops being aspirational.
pub const SERVE_OTEL: [&str; 2] = ["serve", "--otel"];

/// The subcommand each generated command line ends with, leading space
/// included. Exposed separately from the builders below so
/// [`crate::dashboard`] can ask "does the string in this config file still end
/// with a command we have?" without also having to agree about the binary's
/// path, the issuer or the client id -- none of which being different means
/// the wiring is broken, and all of which vary innocently between a
/// `configure` run and a later `status`.
pub const TOKEN_TAIL: &str = " token";

/// See [`TOKEN_TAIL`].
pub const OTEL_HEADERS_TAIL: &str = " otel headers";

/// What Claude Code's `apiKeyHelper` and Codex's `auth.command` run.
///
/// `--issuer`/`--client-id` are written explicitly rather than left to
/// `GOVERNANCE_AUTH_*`: a helper subprocess is not guaranteed to inherit the
/// shell profile that sets them, and a helper that resolves only inside an
/// interactive terminal is a helper that fails exactly where nobody is
/// watching.
pub fn token_command(issuer: &str, client_id: &str) -> String {
    format!(
        "{} --issuer {issuer} --client-id {client_id}{TOKEN_TAIL}",
        crate::otel::binary_path()
    )
}

/// What Claude Code's `otelHeadersHelper` runs. Same argument-passing
/// reasoning as [`token_command`].
pub fn otel_headers_command(issuer: &str, client_id: &str) -> String {
    format!(
        "{} --issuer {issuer} --client-id {client_id}{OTEL_HEADERS_TAIL}",
        crate::otel::binary_path()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two helper lines and the drain's argv must name commands that
    /// exist, spelled the way the tree spells them. A rename that updates the
    /// enum and not this module writes a config file whose command fails on
    /// every invocation -- silently, because nothing reads a credential
    /// helper's stderr.
    #[test]
    fn every_generated_command_is_a_command_this_binary_has() {
        for rendered in [
            token_command("https://issuer.example", "client"),
            otel_headers_command("https://issuer.example", "client"),
        ] {
            // `<path> --issuer <url> --client-id <id>` is five words; the
            // subcommand path is whatever follows.
            let words: Vec<&str> = rendered.split_whitespace().collect();
            let tail = words.get(5..).unwrap_or_default();
            assert!(
                !tail.is_empty() && crate::cli::tests::accepts(tail),
                "`{}` is not a command this binary has",
                tail.join(" ")
            );
        }
        assert!(
            crate::cli::tests::accepts(&COPILOT_PUSH),
            "the drain's argv is not a command this binary has"
        );
    }
}
