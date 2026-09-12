//! Bounded discovery of recently modified, regular Codex session files.
//! Only path/mtime/length signatures persist in memory, never transcript bytes.
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use anyhow::{Context, Result, ensure};

use super::{parse, wire};

pub(crate) type Signature = (u64, SystemTime);
pub(crate) type Seen = BTreeMap<PathBuf, Signature>;

pub(crate) struct Batch {
    pub path: PathBuf,
    pub signature: Signature,
    pub payloads: Vec<Vec<u8>>,
}

pub(crate) fn root() -> Option<PathBuf> {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".codex")))
        .map(|p| p.join("sessions"))
}

pub(crate) fn collect(root: &Path, seen: &Seen) -> Result<Vec<Batch>> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut candidates = Vec::new();
    let mut budget = 20_000;
    discover(root, 0, &mut budget, &mut candidates)?;
    candidates.sort_by_key(|(_, (_, modified))| std::cmp::Reverse(*modified));
    let mut batches = Vec::new();
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)?
        .as_secs();
    for (path, signature) in candidates
        .into_iter()
        .filter(|(p, s)| seen.get(p) != Some(s))
        .take(8)
    {
        let facts = match parse::read(&path) {
            Ok(facts) => facts,
            Err(error) => {
                tracing::warn!(error = %error, "Codex metadata file was not exported");
                // Remember the attempted signature so eight unsupported files
                // cannot starve every supported session. A writer flush changes
                // the signature; daemon restart also retries unchanged failures.
                batches.push(Batch {
                    path,
                    signature,
                    payloads: Vec::new(),
                });
                continue;
            }
        };
        let mut unique = BTreeMap::new();
        for fact in &facts {
            // A resumed transcript can contain years of history. Do not inject
            // those records into Loki or let one old timestamp reject a batch.
            if fact.ended < now.saturating_sub(86400) || fact.ended > now {
                continue;
            }
            let key = (&fact.session, &fact.turn);
            if let Some(previous) = unique.insert(key, fact) {
                ensure!(previous == fact, "conflicting Codex terminal turn metadata");
            }
        }
        let mut payloads = Vec::new();
        for chunk in unique.values().copied().collect::<Vec<_>>().chunks(100) {
            payloads.push(serde_json::to_vec(&wire::envelope(chunk)?)?);
        }
        batches.push(Batch {
            path,
            signature,
            payloads,
        });
    }
    Ok(batches)
}

fn discover(
    root: &Path,
    depth: usize,
    budget: &mut usize,
    out: &mut Vec<(PathBuf, Signature)>,
) -> Result<()> {
    if depth > 3 {
        return Ok(());
    }
    for entry in fs::read_dir(root).context("listing Codex session metadata directory")? {
        ensure!(
            *budget > 0,
            "Codex discovery exceeded 20000 directory entries"
        );
        *budget -= 1;
        let entry = entry.context("reading Codex session directory entry")?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            discover(&entry.path(), depth + 1, budget, out)?;
        } else if kind.is_file() && entry.path().extension().is_some_and(|e| e == "jsonl") {
            let metadata = entry.metadata()?;
            let modified = metadata.modified()?;
            if modified
                .elapsed()
                .is_ok_and(|age| age <= Duration::from_secs(86400))
            {
                out.push((entry.path(), (metadata.len(), modified)));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
