//! Writes back what you just used, so you only pass it once.
//!
//! `login` resolves its options through ADR-0012's five layers and then throws
//! the answer away, so every later `token`/`status`/`logout` had to be handed
//! `--issuer`/`--client-id` again or it failed with *"--issuer … is required"*.
//! That is the same value, re-typed, on a machine that already proved it works.
//! After a successful `login` the resolved settings are written to the per-user
//! config file (layer 3), and the next command finds them there.
//!
//! ## Why `toml_edit` and not `serde`
//!
//! Serialising a fresh `ConfigFile` would rewrite the whole file, which means
//! **destroying the developer's comments and formatting** and -- worse --
//! round-tripping `otel_token` through this process. `toml_edit` mutates only
//! the keys named below and leaves every other byte alone, so a hand-written
//! file survives, and a credential this module never reads cannot be moved,
//! reformatted, or accidentally logged by it.
//!
//! ## What is deliberately NOT written
//!
//! `otel_token` / `otel_token_file`. The OTLP ingest bearer is a real
//! credential; it already has a home at `0600` (`otel.rs`'s env file, or the
//! `*_FILE` indirection for MDM-managed material). Persisting it here would
//! silently copy a secret into a second location the developer did not choose,
//! and `config_file.rs` would then refuse to load its own output if the file
//! were ever group-readable. Passing `--otel-token` still works; it just is not
//! remembered.

use std::{fs, path::Path};

use anyhow::{Context, Result};
use toml_edit::{DocumentMut, Item, value};

use crate::config::{ExchangeTokenEndpoint, OauthConfig};

/// Merges `config`'s durable settings into the TOML document at `path`,
/// creating it if absent. Returns the path on success.
///
/// Idempotent: running `login` twice with the same options rewrites the same
/// values and leaves the file byte-identical.
pub fn remember(config: &OauthConfig, path: &Path) -> Result<()> {
    let existing = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };

    let mut doc: DocumentMut = existing
        .parse()
        .with_context(|| format!("parsing {} before updating it", path.display()))?;

    if doc.as_table().is_empty() {
        // Only on a file we are creating: never prepend to someone else's.
        doc.decor_mut().set_prefix(
            crate::templates::config_header().context("rendering the config-file header")?,
        );
    }

    set(&mut doc, "issuer", value(&config.issuer));
    set(&mut doc, "client_id", value(&config.client_id));
    set(&mut doc, "scopes", value(&config.scopes));
    set(
        &mut doc,
        "otel_headers_debounce_ms",
        value(i64::try_from(config.otel_headers_debounce_ms)?),
    );
    set(&mut doc, "open_browser", value(config.open_browser));

    set_or_clear(&mut doc, "audience", config.audience.as_deref());
    set_or_clear(&mut doc, "otel_endpoint", config.otel_endpoint.as_deref());
    set_or_clear(&mut doc, "gateway_url", config.gateway_url.as_deref());
    // Persisted like any other durable path. `None` clears it, which is what
    // returns `copilot-push` to the state-directory default rather than
    // leaving a path the developer stopped passing silently in force.
    set_or_clear(
        &mut doc,
        "copilot_spool_path",
        config.copilot_spool_path.as_deref(),
    );

    // Token exchange is a block, and `None` is the only representation of
    // "off" (see `OauthConfig::token_exchange`). Writing `token_exchange =
    // false` and leaving the four sibling keys behind would persist a shape
    // that resolve() cannot produce, so the whole block clears together.
    match &config.token_exchange {
        Some(exchange) => {
            set(&mut doc, "token_exchange", value(true));
            set(&mut doc, "exchange_client_id", value(&exchange.client_id));
            set_or_clear(&mut doc, "exchange_scopes", exchange.scopes.as_deref());
            // The endpoint is one-or-the-other by construction, so the key not
            // chosen is removed rather than left stale -- persisting both would
            // reproduce a shape `resolve()` refuses.
            let (chosen, unused) = match &exchange.token_endpoint {
                ExchangeTokenEndpoint::Explicit(url) => {
                    (("exchange_token_endpoint", url), "exchange_issuer")
                }
                ExchangeTokenEndpoint::Issuer(url) => {
                    (("exchange_issuer", url), "exchange_token_endpoint")
                }
            };
            set(&mut doc, chosen.0, value(chosen.1));
            clear(&mut doc, unused);
        }
        None => {
            for key in [
                "token_exchange",
                "exchange_issuer",
                "exchange_token_endpoint",
                "exchange_client_id",
                "exchange_scopes",
            ] {
                clear(&mut doc, key);
            }
        }
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    write_private(path, doc.to_string().as_bytes())
}

/// `Some` writes the key, `None` removes it. Leaving a stale value behind
/// would mean an option dropped from the command line silently kept applying
/// from the file -- the failure mode this whole module could otherwise create.
fn set_or_clear(doc: &mut DocumentMut, key: &str, new: Option<&str>) {
    match new {
        Some(text) => set(doc, key, value(text)),
        None => clear(doc, key),
    }
}

/// Insert-or-replace, **preserving the key's decor**.
///
/// Two traps stacked here. `doc[key] = …` reads best but is `Index`, which
/// `clippy::indexing_slicing` denies workspace-wide. The obvious replacement,
/// `Table::insert`, silently replaces the key *and its decor* -- and in
/// `toml_edit` a comment written above `issuer = …` is that key's leading
/// decor, so `insert` deletes the developer's comments. Assigning through
/// `get_mut` replaces only the value and leaves the key, its comment and the
/// surrounding whitespace untouched.
fn set(doc: &mut DocumentMut, key: &str, item: Item) {
    let table = doc.as_table_mut();
    match table.get_mut(key) {
        Some(slot) => *slot = item,
        None => {
            table.insert(key, item);
        }
    }
}

fn clear(doc: &mut DocumentMut, key: &str) {
    doc.as_table_mut().remove(key);
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};

    // 0600 even though nothing secret is written: `config_file.rs` REFUSES to
    // load a file that inlines `otel_token` and is group/other-readable, so a
    // developer who later adds that key by hand must not be blocked by
    // permissions this binary chose.
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("opening {} for writing", path.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests;
