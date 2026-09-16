use std::io::Cursor;

use serde_json::json;

use super::*;

fn records(cwd: &str) -> String {
    [json!({"type":"session_meta","payload":{"id":"session","cli_version":"0.154.0-alpha.6.2",
        "cwd":"/repo","git":{"repository_url":"https://token:secret@example.org/team/repo.git?secret=hidden"}}}),
     json!({"type":"turn_context","payload":{"turn_id":"turn","cwd":cwd}}),
     json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":"turn",
        "started_at":100,"completed_at":110,"duration_ms":10000,"last_agent_message":"private-content"}})]
        .iter().map(ToString::to_string).collect::<Vec<_>>().join("\n")
}

#[test]
fn terminal_metadata_is_allowlisted_and_repository_credentials_are_removed() {
    let facts = parse(Cursor::new(records("/repo"))).unwrap();
    let fact = facts.first().unwrap();
    assert_eq!(fact.repository.as_deref(), Some("example.org/team/repo"));
    assert_eq!(fact.duration_ms, 10000);
    let envelope = super::super::wire::envelope(&[fact]).unwrap();
    assert_ne!(
        envelope["resourceLogs"][0]["scopeLogs"][0]["logRecords"][0]["timeUnixNano"],
        "110000000000"
    );
    let output = envelope.to_string();
    for sensitive in [
        "private-content",
        "secret",
        "\"stringValue\":\"/repo\"",
        "last_agent_message",
    ] {
        assert!(!output.contains(sensitive));
    }
}

#[test]
fn changed_working_directory_does_not_inherit_launch_repository() {
    let facts = parse(Cursor::new(records("/other"))).unwrap();
    assert!(facts.first().unwrap().repository.is_none());
}

#[test]
fn unsupported_version_and_invalid_interval_fail_closed() {
    let input = records("/repo");
    assert!(parse(Cursor::new(input.replace("0.154.0-alpha.6.2", "future"))).is_err());
    assert!(
        parse(Cursor::new(
            input.replace("\"completed_at\":110", "\"completed_at\":90")
        ))
        .is_err()
    );
}

#[test]
fn incomplete_record_errors_do_not_expose_content() {
    let error = parse(Cursor::new(format!(
        "{}\n{{\"private-secret",
        records("/repo")
    )))
    .unwrap_err();
    assert!(!error.to_string().contains("private-secret"));
}

#[test]
fn repository_handles_ssh_and_rejects_local_paths() {
    assert_eq!(
        canonical_repository("git@example.org:team/repo.git"),
        Some("example.org/team/repo".into())
    );
    assert_eq!(canonical_repository("file:///home/user/repo"), None);
    assert_eq!(canonical_repository("/home/user/repo"), None);
}

#[test]
fn interrupted_turns_are_measured_but_unfinished_turns_are_not() {
    let input = records("/repo").replace("task_complete", "turn_aborted");
    assert_eq!(
        parse(Cursor::new(input)).unwrap().first().unwrap().status,
        "turn_aborted"
    );
    let incomplete = records("/repo").replace("task_complete", "task_started");
    assert!(parse(Cursor::new(incomplete)).unwrap().is_empty());
}
