//! Every file this binary generates whole, rendered from a template instead of
//! assembled from string literals.
//!
//! ## What belongs here, and what must never
//!
//! Only files `governance-auth` **owns outright** -- the shell env files, and
//! the comment banners it injects into TOML it is already editing.
//!
//! Claude Code's `settings.json`, Codex's `config.toml` and VS Code's
//! `settings.json` are **merged into, never replaced** (`otel.rs`, and
//! `config_file.rs`'s "owned entirely by governance-auth" note). Rendering a
//! whole-file template over any of those would delete the developer's own
//! configuration. The templates below produce *fragments* those writers then
//! merge; the merge itself stays structural, in `toml_edit`/`serde_json`.
//!
//! ## Autoescaping is OFF here, deliberately
//!
//! minijinja keys autoescaping off the template name, and nothing here ends in
//! `.html`. That is required, not incidental: HTML-escaping a shell file would
//! turn `&` in a URL into `&amp;` and an apostrophe into `&#x27;`, corrupting
//! the value silently. `shell_env_is_not_html_escaped` pins it.
//!
//! Shell quoting is handled by the `sh_quote`/`fish_quote` filters instead --
//! see their doc comments for why a bare `'{value}'` was not safe.

use minijinja::{Environment, Value, context};

const SHELL_ENV_SH: &str = include_str!("shell_env.sh.jinja");
const SHELL_ENV_FISH: &str = include_str!("shell_env.fish.jinja");
const CODEX_BANNER: &str = include_str!("codex_provider_banner.toml.jinja");
const CONFIG_HEADER: &str = include_str!("config_header.toml.jinja");

/// One environment, built per render.
///
/// Cheap relative to the work around it (these render at most once per
/// `configure`), and a fresh environment means no shared mutable state between
/// callers -- which matters more than the microseconds.
fn environment() -> Environment<'static> {
    let mut env = Environment::new();
    // Off by default, which silently eats the final newline of every template
    // -- so `config_header.toml.jinja`'s trailing blank line, the one that
    // separates the comment from the first key, disappeared. Templates should
    // render what they say.
    env.set_keep_trailing_newline(true);
    env.add_filter("sh_quote", sh_quote);
    env.add_filter("fish_quote", fish_quote);
    env
}

/// POSIX single-quoting. Inside `'...'` every byte is literal except `'`
/// itself, which cannot be escaped -- the only way out is to close the quote,
/// emit an escaped quote, and reopen: `'\''`.
///
/// Not theoretical here. `OTEL_RESOURCE_ATTRIBUTES` carries identity
/// attributes taken from the access token, so a user named `O'Brien` produced
/// `export OTEL_RESOURCE_ATTRIBUTES='...O'Brien...'` -- which terminates the
/// string early and leaves the shell parsing the remainder as code. Every rc
/// file sourcing it would then emit an error on every new shell.
fn sh_quote(value: String) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// fish's single quotes are not POSIX's: inside `'...'` fish honours `\\` and
/// `\'` escapes, so the POSIX `'\''` dance is both unnecessary and wrong here.
/// Backslash must be escaped first or it would double-escape the quotes added
/// after it.
fn fish_quote(value: String) -> String {
    format!("'{}'", value.replace('\\', r"\\").replace('\'', r"\'"))
}

fn render(template_name: &str, source: &str, ctx: Value) -> Result<String, minijinja::Error> {
    let mut env = environment();
    env.add_template(template_name, source)?;
    env.get_template(template_name)?.render(ctx)
}

/// The `0600` file a shell rc sources. `exports` is `(name, value)` in the
/// order they should appear.
pub fn shell_env_sh(exports: &[(&str, String)]) -> Result<String, minijinja::Error> {
    render("shell_env.sh", SHELL_ENV_SH, context! { exports })
}

/// The fish equivalent. Separate template rather than a conditional: the two
/// syntaxes share no line, so a branch inside one file would be harder to read
/// than two short files.
pub fn shell_env_fish(exports: &[(&str, String)]) -> Result<String, minijinja::Error> {
    render("shell_env.fish", SHELL_ENV_FISH, context! { exports })
}

/// TOML comment block introducing the provider entry in Codex's config.
pub fn codex_provider_banner() -> Result<String, minijinja::Error> {
    render("codex_banner.toml", CODEX_BANNER, context! {})
}

/// TOML comment header for the per-user config file, written only when this
/// binary is creating it.
pub fn config_header() -> Result<String, minijinja::Error> {
    render("config_header.toml", CONFIG_HEADER, context! {})
}

#[cfg(test)]
mod tests;
