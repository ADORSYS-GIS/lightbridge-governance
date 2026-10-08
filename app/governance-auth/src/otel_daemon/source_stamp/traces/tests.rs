use serde_json::{Value as Json, json};

use super::*;

fn kv(key: &str, value: &str) -> Json {
    json!({"key": key, "value": {"stringValue": value}})
}

fn span(name: &str, attributes: Json) -> Json {
    json!({"traceId": "000000000000000000000000000000c1", "spanId": "00000000000000a1",
           "name": name, "startTimeUnixNano": "1", "endTimeUnixNano": "2", "attributes": attributes})
}

fn request(service_name: &str, spans: Json) -> ExportTraceServiceRequest {
    serde_json::from_value(json!({"resourceSpans": [{
        "resource": {"attributes": [kv("service.name", service_name)]},
        "scopeSpans": [{"spans": spans}]
    }]}))
    .unwrap()
}

/// One Copilot Chat agent turn. SHAPE from a real capture (Copilot Chat 0.68.0, 2026-10-08): span
/// names and attribute KEYS as captured; every value synthetic, identifying keys omitted.
fn copilot_turn(service_name: &str) -> ExportTraceServiceRequest {
    request(
        service_name,
        json!([
            span(
                "invoke_agent GitHub Copilot Chat",
                json!([
                    kv("gen_ai.operation.name", "invoke_agent"),
                    kv("copilot_chat.turn_count", "1")
                ])
            ),
            span(
                "execute_tool list_dir",
                json!([
                    kv("gen_ai.tool.name", "list_dir"),
                    kv("copilot_chat.session_id", "s")
                ])
            ),
            span(
                "embeddings text-embedding-x",
                json!([kv("gen_ai.request.model", "x")])
            ),
            span(
                "vscode.chat.user_perceived_time_to_first_progress",
                json!([])
            )
        ]),
    )
}

fn stamped(request: &ExportTraceServiceRequest, format: WireFormat) -> Option<String> {
    let input = match format {
        WireFormat::Json => serde_json::to_vec(request).unwrap(),
        WireFormat::Protobuf => request.encode_to_vec(),
    };
    let output = enrich_traces(&input, format);
    let decoded: ExportTraceServiceRequest = match format {
        WireFormat::Json => serde_json::from_slice(&output).unwrap(),
        WireFormat::Protobuf => ExportTraceServiceRequest::decode(output.as_slice()).unwrap(),
    };
    let attrs = &decoded.resource_spans[0].resource.as_ref()?.attributes;
    let mut matches = attrs.iter().filter(|a| a.key == SOURCE_ATTRIBUTE);
    let value = matches.next()?.value.as_ref()?.value.as_ref()?;
    assert!(matches.next().is_none(), "exactly one governance.source");
    match value {
        Value::StringValue(v) => Some(v.clone()),
        _ => None,
    }
}

#[test]
fn a_copilot_chat_turn_is_stamped_github_copilot_in_both_wire_formats() {
    for format in [WireFormat::Json, WireFormat::Protobuf] {
        assert_eq!(
            stamped(&copilot_turn("copilot-chat"), format).as_deref(),
            Some("github-copilot"),
            "{format:?}"
        );
    }
}

/// The reason `service.name` is the primary signal: these two spans carry no `copilot_chat.*`
/// attribute, so a batch of only them would otherwise go unstamped -- back to `claude-code`.
#[test]
fn a_batch_without_any_copilot_chat_attribute_is_still_stamped_by_service_name() {
    let request = request(
        "copilot-chat",
        json!([
            span(
                "embeddings text-embedding-x",
                json!([kv("gen_ai.request.model", "x")])
            ),
            span(
                "vscode.chat.user_perceived_time_to_first_progress",
                json!([])
            )
        ]),
    );
    assert_eq!(
        stamped(&request, WireFormat::Json).as_deref(),
        Some("github-copilot")
    );
}

/// The second signal: a renamed service is still recognised by Copilot's attribute namespace.
#[test]
fn copilot_chat_attributes_identify_the_source_when_the_service_name_differs() {
    assert_eq!(
        stamped(&copilot_turn("some-renamed-service"), WireFormat::Json).as_deref(),
        Some("github-copilot")
    );
}

#[test]
fn an_unrecognised_trace_is_left_byte_for_byte_unchanged() {
    let request = request(
        "some-other-tool",
        json!([span("agent.run", json!([kv("gen_ai.request.model", "x")]))]),
    );
    let input = serde_json::to_vec(&request).unwrap();
    assert_eq!(enrich_traces(&input, WireFormat::Json), input);
}

#[test]
fn a_client_supplied_governance_source_is_replaced_not_layered_on() {
    let mut request = copilot_turn("copilot-chat");
    request.resource_spans[0]
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
    assert_eq!(
        stamped(&request, WireFormat::Json).as_deref(),
        Some("github-copilot")
    );
}

#[test]
fn a_duplicated_service_name_is_ambiguous_and_does_not_stamp_by_itself() {
    let mut request = request("copilot-chat", json!([span("agent.run", json!([]))]));
    let resource = request.resource_spans[0].resource.as_mut().unwrap();
    resource.attributes.push(KeyValue {
        key: "service.name".to_owned(),
        value: Some(AnyValue {
            value: Some(Value::StringValue("copilot-chat".to_owned())),
        }),
        ..Default::default()
    });
    let input = serde_json::to_vec(&request).unwrap();
    assert_eq!(enrich_traces(&input, WireFormat::Json), input);
}

#[test]
fn an_unparseable_body_is_forwarded_untouched() {
    let garbage = b"not an otlp payload".to_vec();
    assert_eq!(enrich_traces(&garbage, WireFormat::Json), garbage);
}
