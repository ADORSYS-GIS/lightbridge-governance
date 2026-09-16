//! Parsing of the identity directory file handed to `identity-sync`.
//!
//! The file is a single JSON array of `{provider_user_id, internal_user_id}`
//! objects, or one such array (or object) per line. Blank lines are ignored.

use anyhow::Result;

/// Parses the directory file: a single JSON array of `{provider_user_id,
/// internal_user_id}` objects, or one such array (or object) per line.
/// Blank lines are ignored.
pub fn read_directory(file: &str) -> Result<Vec<governance_core::identity::DirectoryEntry>> {
    let raw = std::fs::read_to_string(file)?;
    parse_directory(&raw)
}

/// Parses directory content. Accepts either a single top-level JSON array of
/// entries, or one JSON array (or single object) per line -- so an operator
/// can hand over a pretty-printed array or an NDJSON-style stream. Blank
/// lines are skipped. A line that is neither a valid array nor a valid
/// object fails the whole parse (fail-closed, not best-effort).
fn parse_directory(raw: &str) -> Result<Vec<governance_core::identity::DirectoryEntry>> {
    if let Ok(entries) = serde_json::from_str::<Vec<DirectoryEntryJson>>(raw) {
        return Ok(to_directory_entries(entries));
    }

    let mut out = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(entries) = serde_json::from_str::<Vec<DirectoryEntryJson>>(line) {
            out.extend(to_directory_entries(entries));
        } else {
            let entry: DirectoryEntryJson = serde_json::from_str(line)?;
            out.push(governance_core::identity::DirectoryEntry {
                provider_user_id: entry.provider_user_id,
                internal_user_id: entry.internal_user_id,
            });
        }
    }
    Ok(out)
}

fn to_directory_entries(
    entries: Vec<DirectoryEntryJson>,
) -> Vec<governance_core::identity::DirectoryEntry> {
    entries
        .into_iter()
        .map(|e| governance_core::identity::DirectoryEntry {
            provider_user_id: e.provider_user_id,
            internal_user_id: e.internal_user_id,
        })
        .collect()
}

#[derive(serde::Deserialize)]
struct DirectoryEntryJson {
    provider_user_id: String,
    internal_user_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_single_array() {
        let parsed =
            parse_directory(r#"[{"provider_user_id":"a@x.com","internal_user_id":"sub-a"}]"#)
                .unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].provider_user_id, "a@x.com");
        assert_eq!(parsed[0].internal_user_id, "sub-a");
    }

    #[test]
    fn parses_a_pretty_printed_single_array() {
        let parsed = parse_directory(
            "[\n  {\"provider_user_id\":\"a@x.com\",\"internal_user_id\":\"sub-a\"},\n  \
             {\"provider_user_id\":\"b@x.com\",\"internal_user_id\":\"sub-b\"}\n]\n",
        )
        .unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[1].provider_user_id, "b@x.com");
    }

    #[test]
    fn parses_one_array_per_line() {
        let parsed = parse_directory(
            "[{\"provider_user_id\":\"a@x.com\",\"internal_user_id\":\"sub-a\"}]\n\
             [{\"provider_user_id\":\"b@x.com\",\"internal_user_id\":\"sub-b\"}]\n",
        )
        .unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[1].provider_user_id, "b@x.com");
    }

    #[test]
    fn parses_one_object_per_line() {
        let parsed = parse_directory(
            "{\"provider_user_id\":\"a@x.com\",\"internal_user_id\":\"sub-a\"}\n\
             {\"provider_user_id\":\"b@x.com\",\"internal_user_id\":\"sub-b\"}\n",
        )
        .unwrap();
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn ignores_blank_lines() {
        let parsed = parse_directory(
            "\n\n{\"provider_user_id\":\"a@x.com\",\"internal_user_id\":\"sub-a\"}\n\n",
        )
        .unwrap();
        assert_eq!(parsed.len(), 1);
    }

    #[test]
    fn rejects_a_malformed_line() {
        let result = parse_directory("{\"provider_user_id\":\"a@x.com\"\n");
        assert!(result.is_err());
    }
}
