use serde_json::json;

use super::*;

struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn bounded_scan_deduplicates_skips_old_turns_and_does_not_starve_supported_files() {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap();
    let root = Directory(std::env::temp_dir().join(format!(
        "codex-scan-{}-{}",
        std::process::id(),
        now.as_nanos()
    )));
    fs::create_dir(&root.0).unwrap();
    let nested = root.0.join("2026/09/12");
    fs::create_dir_all(&nested).unwrap();
    let header =
        json!({"type":"session_meta","payload":{"id":"session","cli_version":"0.154.0-alpha.6.2"}});
    let terminal = |id, ended| {
        json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":id,
        "started_at":ended-10,"completed_at":ended,"duration_ms":10000}})
    };
    let recent = terminal("recent", now.as_secs());
    let old = terminal("old", now.as_secs() - 172800);
    fs::write(
        nested.join("supported.jsonl"),
        format!("{header}\n{recent}\n{recent}\n{old}\n"),
    )
    .unwrap();
    for n in 0..10 {
        fs::write(
            nested.join(format!("unsupported-{n}.jsonl")),
            "{\"incomplete",
        )
        .unwrap();
    }
    let mut seen = Seen::new();
    let mut payloads = Vec::new();
    for _ in 0..3 {
        let batches = collect(&root.0, &seen).unwrap();
        assert!(batches.len() <= 8);
        for batch in batches {
            seen.insert(batch.path, batch.signature);
            payloads.extend(batch.payloads);
        }
    }
    assert_eq!(payloads.len(), 1);
    let decoded: serde_json::Value = serde_json::from_slice(&payloads[0]).unwrap();
    assert_eq!(
        decoded["resourceLogs"][0]["scopeLogs"][0]["logRecords"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(collect(&root.0, &seen).unwrap().is_empty());
    assert!(
        !String::from_utf8(payloads[0].clone())
            .unwrap()
            .contains("\"old\"")
    );
    // A flush changes the failed file's signature and makes it eligible again.
    fs::write(
        nested.join("unsupported-0.jsonl"),
        format!("{header}\n{recent}\n"),
    )
    .unwrap();
    assert_eq!(collect(&root.0, &seen).unwrap().len(), 1);
}
