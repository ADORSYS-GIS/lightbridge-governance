use serde_json::json;

use super::*;

fn request_with_event(event_name: &str) -> ExportLogsServiceRequest {
    serde_json::from_value(json!({"resourceLogs":[{"resource":{"attributes":[
        {"key":"service.name","value":{"stringValue":"whatever-the-client-claims"}}
    ]},
    "scopeLogs":[{"logRecords":[{"timeUnixNano":"1234567890","attributes":[
        {"key":"event.name","value":{"stringValue":event_name}}
    ]}]}]}]}))
    .unwrap()
}

fn resource_attr<'a>(request: &'a ExportLogsServiceRequest, key: &str) -> Option<&'a str> {
    let attrs = &request.resource_logs[0].resource.as_ref()?.attributes;
    let mut matches = attrs.iter().filter(|a| a.key == key);
    let value = matches.next()?.value.as_ref()?.value.as_ref()?;
    if matches.next().is_some() {
        return None;
    }
    match value {
        Value::StringValue(v) => Some(v),
        _ => None,
    }
}

#[test]
fn codex_namespaced_events_are_stamped_codex() {
    for event in ["codex.api_request", "codex.sse_event", "codex.tool_result"] {
        let request = request_with_event(event);
        let output = enrich(&serde_json::to_vec(&request).unwrap(), WireFormat::Json);
        let decoded: ExportLogsServiceRequest = serde_json::from_slice(&output).unwrap();
        assert_eq!(resource_attr(&decoded, SOURCE_ATTRIBUTE), Some("codex"));
    }
}

#[test]
fn claude_code_bare_events_are_stamped_claude_code() {
    for event in [
        "api_request",
        "user_prompt",
        "tool_result",
        "auth",
        "plugin_installed",
    ] {
        let request = request_with_event(event);
        let output = enrich(&serde_json::to_vec(&request).unwrap(), WireFormat::Json);
        let decoded: ExportLogsServiceRequest = serde_json::from_slice(&output).unwrap();
        assert_eq!(
            resource_attr(&decoded, SOURCE_ATTRIBUTE),
            Some("claude-code"),
            "event {event} should be attributed to claude-code"
        );
    }
}

#[test]
fn an_unrecognised_event_name_leaves_source_unset_rather_than_guessing() {
    let request = request_with_event("some_future_tool_event");
    let input = serde_json::to_vec(&request).unwrap();
    let output = enrich(&input, WireFormat::Json);
    assert_eq!(
        output, input,
        "an unrecognised event must not change the payload at all"
    );
}

#[test]
fn a_client_supplied_governance_source_is_replaced_not_layered_on() {
    let mut request = request_with_event("codex.api_request");
    request.resource_logs[0]
        .resource
        .as_mut()
        .unwrap()
        .attributes
        .push(KeyValue {
            key: SOURCE_ATTRIBUTE.to_owned(),
            value: Some(AnyValue {
                value: Some(Value::StringValue("claude-code".to_owned())),
            }),
            ..Default::default()
        });
    let output = enrich(&serde_json::to_vec(&request).unwrap(), WireFormat::Json);
    let decoded: ExportLogsServiceRequest = serde_json::from_slice(&output).unwrap();
    let attrs = &decoded.resource_logs[0]
        .resource
        .as_ref()
        .unwrap()
        .attributes;
    let matching = attrs.iter().filter(|a| a.key == SOURCE_ATTRIBUTE).count();
    assert_eq!(
        matching, 1,
        "the forged value must be replaced, not left alongside the real one"
    );
    assert_eq!(resource_attr(&decoded, SOURCE_ATTRIBUTE), Some("codex"));
}

#[test]
fn protobuf_and_json_stamp_identically_and_reenrichment_is_idempotent() {
    let request = request_with_event("codex.sse_event");
    let binary = enrich(&request.encode_to_vec(), WireFormat::Protobuf);
    let binary_decoded = ExportLogsServiceRequest::decode(binary.as_slice()).unwrap();
    let json = enrich(&serde_json::to_vec(&request).unwrap(), WireFormat::Json);
    let json_decoded: ExportLogsServiceRequest = serde_json::from_slice(&json).unwrap();
    assert_eq!(binary_decoded, json_decoded);
    assert_eq!(
        resource_attr(&json_decoded, SOURCE_ATTRIBUTE),
        Some("codex")
    );
    assert_eq!(enrich(&binary, WireFormat::Protobuf), binary);
    assert_eq!(enrich(&json, WireFormat::Json), json);
}

#[test]
fn a_multi_resource_batch_stamps_each_resource_from_its_own_events() {
    let combined: ExportLogsServiceRequest = serde_json::from_value(json!({"resourceLogs":[
        {"resource":{"attributes":[]},"scopeLogs":[{"logRecords":[
            {"timeUnixNano":"1","attributes":[{"key":"event.name","value":{"stringValue":"codex.api_request"}}]}
        ]}]},
        {"resource":{"attributes":[]},"scopeLogs":[{"logRecords":[
            {"timeUnixNano":"2","attributes":[{"key":"event.name","value":{"stringValue":"api_request"}}]}
        ]}]}
    ]}))
    .unwrap();
    let output = enrich(&serde_json::to_vec(&combined).unwrap(), WireFormat::Json);
    let decoded: ExportLogsServiceRequest = serde_json::from_slice(&output).unwrap();
    assert_eq!(resource_attr(&decoded, SOURCE_ATTRIBUTE), Some("codex"));
    assert_eq!(
        decoded.resource_logs[1]
            .resource
            .as_ref()
            .unwrap()
            .attributes
            .iter()
            .find(|a| a.key == SOURCE_ATTRIBUTE)
            .and_then(|a| a.value.as_ref())
            .and_then(|v| v.value.as_ref()),
        Some(&Value::StringValue("claude-code".to_owned()))
    );
}
