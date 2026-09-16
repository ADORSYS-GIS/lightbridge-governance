//! Versioned, content-free Codex session metadata export. Never tails implicitly.
//! Explicit input files are read only after authentication; output uses the
//! existing daemon's durable admission. Replays carry the same natural keys.
mod parse;
pub(crate) mod scan;
mod wire;

use std::{collections::BTreeMap, path::PathBuf, time::Duration};

use anyhow::{Context, Result, ensure};

use crate::{config::OauthConfig, freshness::Freshness, oauth, otel_port};

pub async fn run(
    http: &reqwest::Client,
    config: &OauthConfig,
    paths: Vec<PathBuf>,
    dry_run: bool,
) -> Result<()> {
    // Same fail-closed read ordering as `copilot push`, including dry runs.
    let _session = oauth::current_session(http, config, Freshness::Skew).await?;
    let facts = tokio::task::spawn_blocking(move || {
        let mut facts = BTreeMap::new();
        for path in paths {
            for fact in parse::read(&path)? {
                let key = (fact.session.clone(), fact.turn.clone());
                if let Some(previous) = facts.get(&key) {
                    ensure!(
                        previous == &fact,
                        "conflicting Codex terminal turn metadata"
                    );
                }
                facts.insert(key, fact);
            }
        }
        Ok::<_, anyhow::Error>(facts)
    })
    .await
    .context("joining Codex metadata reader")??;
    if !dry_run {
        // Local custody only. Do not use proxy environment variables or follow
        // a redirect that could send metadata away from the loopback daemon.
        let local = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(15))
            .build()?;
        for chunk in facts.values().collect::<Vec<_>>().chunks(100) {
            let response = local
                .post(format!("{}/v1/logs", otel_port::OTEL_LOOPBACK_ENDPOINT))
                .json(&wire::envelope(chunk)?)
                .send()
                .await
                .context("sending Codex measurements to the local daemon")?;
            ensure!(
                response.status() == reqwest::StatusCode::OK,
                "local daemon refused Codex measurements (status {})",
                response.status()
            );
        }
    }
    eprintln!(
        "Codex terminal turns: {}; {}",
        facts.len(),
        if dry_run {
            "validated; nothing exported"
        } else {
            "durably admitted by local daemon"
        }
    );
    Ok(())
}
