//! `daemon`'s Copilot path (issue #272 AC3): its own `otlp-http` exporter,
//! pointed at loopback, instead of the file exporter `manual` uses.

use std::fs;

use super::*;

/// The mirror of `vscode_settings_are_merged_into_an_existing_user_config`
/// for the other profile.
#[test]
fn daemon_profile_points_copilots_own_otlp_exporter_at_loopback() {
    let home = tempdir();
    let user = user_dir(home.path(), "Code");
    fs::create_dir_all(&user).expect("create VS Code User dir");

    let daemon = OtelSettings {
        endpoint: Some("http://127.0.0.1:17457".to_owned()),
        copilot_drain_available: false,
        copilot_otlp_direct: true,
        ..settings()
    };
    let outcomes = configure(home.path(), &daemon).expect("configure vscode");
    assert!(matches!(outcomes.as_slice(), [Outcome::Written(_)]));

    let value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(user.join("settings.json")).expect("read"))
            .expect("valid JSON out");
    assert_eq!(value["github.copilot.chat.otel.enabled"], true);
    assert_eq!(value["github.copilot.chat.otel.exporterType"], "otlp-http");
    assert_eq!(
        value["github.copilot.chat.otel.otlpEndpoint"],
        "http://127.0.0.1:17457"
    );
    assert_eq!(value["github.copilot.chat.otel.captureContent"], false);
    assert!(
        value.get("github.copilot.chat.otel.outfile").is_none(),
        "daemon's path writes no outfile -- nothing drains one"
    );
    // No `headers` key of any kind: the whole point of pointing Copilot's own
    // exporter at the loopback daemon is that it needs no credential.
    assert!(
        !fs::read_to_string(user.join("settings.json"))
            .expect("read")
            .to_lowercase()
            .contains("header"),
        "the daemon path must carry no credential"
    );
}
