//! Only deserialize metadata fields. Serde discards message/tool content.
use std::{
    collections::BTreeMap,
    fs::File,
    io::{BufRead, BufReader, Read},
    path::Path,
};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Fact {
    pub session: String,
    pub turn: String,
    pub started: u64,
    pub ended: u64,
    pub duration_ms: u64,
    pub repository: Option<String>,
    pub status: String,
}

#[derive(Default, Deserialize)]
struct Payload {
    #[serde(default, rename = "type")]
    kind: String,
    id: Option<String>,
    cli_version: Option<String>,
    turn_id: Option<String>,
    cwd: Option<String>,
    git: Option<Git>,
    started_at: Option<u64>,
    completed_at: Option<u64>,
    duration_ms: Option<u64>,
}
#[derive(Deserialize)]
struct Git {
    repository_url: Option<String>,
}
#[derive(Deserialize)]
struct Record {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    payload: Payload,
}

pub(super) fn read(path: &Path) -> Result<Vec<Fact>> {
    let file = File::open(path).context("opening explicit Codex transcript")?;
    ensure!(
        file.metadata()?.len() <= 64 * 1024 * 1024,
        "Codex transcript exceeds 64 MiB inspection limit"
    );
    parse(BufReader::new(file.take(64 * 1024 * 1024)))
}

fn parse(reader: impl BufRead) -> Result<Vec<Fact>> {
    let mut session = None;
    let mut launch_cwd = None;
    let mut repository = None;
    let mut contexts = BTreeMap::new();
    let mut facts = Vec::new();
    for line in reader.split(b'\n') {
        let line = line.context("reading Codex metadata record")?;
        if line.is_empty() {
            continue;
        }
        // Errors never include source bytes or serde's unexpected value text.
        let record: Record = serde_json::from_slice(&line).map_err(|_| {
            anyhow::anyhow!("invalid or incomplete Codex metadata record; retry after writer flush")
        })?;
        let p = record.payload;
        match record.kind.as_str() {
            "session_meta" => {
                ensure!(
                    p.cli_version.as_deref() == Some("0.154.0-alpha.6.2"),
                    "unsupported Codex transcript version; native OTLP remains unaffected"
                );
                ensure!(
                    session.is_none(),
                    "multiple session headers in Codex transcript"
                );
                session = p.id;
                launch_cwd = p.cwd;
                repository = p
                    .git
                    .and_then(|g| g.repository_url)
                    .and_then(|r| canonical_repository(&r));
            }
            "turn_context" => {
                if let Some(turn) = p.turn_id {
                    contexts.insert(turn, p.cwd);
                }
            }
            "event_msg" if matches!(p.kind.as_str(), "task_complete" | "turn_aborted") => {
                let session = session
                    .as_ref()
                    .context("Codex terminal turn lacks session header")?;
                let turn = p.turn_id.context("Codex terminal turn lacks turn ID")?;
                let started = p.started_at.context("Codex terminal turn lacks start")?;
                let ended = p.completed_at.context("Codex terminal turn lacks end")?;
                let duration_ms = p
                    .duration_ms
                    .context("Codex terminal turn lacks duration")?;
                ensure!(
                    ended >= started && ended <= u64::MAX / 1_000_000_000,
                    "invalid Codex turn interval"
                );
                ensure!(
                    duration_ms.abs_diff((ended - started).saturating_mul(1000)) <= 2000,
                    "Codex turn duration disagrees with interval"
                );
                let matching_cwd = launch_cwd.is_some() && contexts.get(&turn) == Some(&launch_cwd);
                facts.push(Fact {
                    session: session.clone(),
                    turn,
                    started,
                    ended,
                    duration_ms,
                    repository: matching_cwd.then(|| repository.clone()).flatten(),
                    status: p.kind,
                });
            }
            _ => {}
        }
    }
    ensure!(session.is_some(), "Codex transcript lacks session metadata");
    Ok(facts)
}

fn canonical_repository(remote: &str) -> Option<String> {
    let expanded;
    let candidate = if !remote.contains("://") {
        let (host, path) = remote.split_once(':')?;
        let host = host.rsplit('@').next()?;
        expanded = format!("ssh://{host}/{path}");
        &expanded
    } else {
        remote
    };
    let parsed = url::Url::parse(candidate).ok()?;
    if !matches!(parsed.scheme(), "ssh" | "https" | "http" | "git") {
        return None;
    }
    let host = parsed.host_str()?;
    let path = parsed.path().trim_matches('/').trim_end_matches(".git");
    if path.is_empty() {
        return None;
    }
    Some(format!("{host}/{path}"))
}

#[cfg(test)]
mod tests;
