use serde_json::json;

use super::*;

fn request_with_metric(metric_name: &str) -> ExportMetricsServiceRequest {
    serde_json::from_value(json!({"resourceMetrics":[{"resource":{"attributes":[
        {"key":"service.name","value":{"stringValue":"whatever-the-client-claims"}}
    ]},
    "scopeMetrics":[{"metrics":[{"name":metric_name,"sum":{"dataPoints":[],"aggregationTemporality":1,"isMonotonic":true}}]}]}]}))
    .unwrap()
}

fn resource_attr<'a>(request: &'a ExportMetricsServiceRequest, key: &str) -> Option<&'a str> {
    let attrs = &request.resource_metrics[0].resource.as_ref()?.attributes;
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
fn copilot_chat_metrics_are_stamped_github_copilot() {
    for metric in [
        "copilot_chat.time_to_first_token",
        "copilot_chat.session.count",
        "copilot_chat.tool.call.count",
        "copilot_chat.agent.turn.count",
    ] {
        let request = request_with_metric(metric);
        let output = enrich_metrics(&serde_json::to_vec(&request).unwrap(), WireFormat::Json);
        let decoded: ExportMetricsServiceRequest = serde_json::from_slice(&output).unwrap();
        assert_eq!(
            resource_attr(&decoded, SOURCE_ATTRIBUTE),
            Some("github-copilot"),
            "metric {metric} should be attributed to github-copilot"
        );
    }
}

#[test]
fn a_shared_gen_ai_metric_name_alone_leaves_source_unset() {
    // `gen_ai.client.*` is a generic OTel semantic-convention namespace, not
    // Copilot-specific -- it must never trigger this taxonomy by itself. In
    // a real batch it always arrives alongside a `copilot_chat.*` metric
    // (see the module doc); this test asserts the narrower, safer behavior
    // rather than assuming that co-occurrence always holds.
    let request = request_with_metric("gen_ai.client.token.usage");
    let input = serde_json::to_vec(&request).unwrap();
    let output = enrich_metrics(&input, WireFormat::Json);
    assert_eq!(
        output, input,
        "a bare gen_ai.client.* metric must not change the payload at all"
    );
}

#[test]
fn an_unrecognised_metric_name_leaves_source_unset_rather_than_guessing() {
    let request = request_with_metric("some_future_tool.metric");
    let input = serde_json::to_vec(&request).unwrap();
    let output = enrich_metrics(&input, WireFormat::Json);
    assert_eq!(
        output, input,
        "an unrecognised metric must not change the payload at all"
    );
}

#[test]
fn a_client_supplied_governance_source_is_replaced_not_layered_on() {
    let mut request = request_with_metric("copilot_chat.time_to_first_token");
    request.resource_metrics[0]
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
    let output = enrich_metrics(&serde_json::to_vec(&request).unwrap(), WireFormat::Json);
    let decoded: ExportMetricsServiceRequest = serde_json::from_slice(&output).unwrap();
    let attrs = &decoded.resource_metrics[0]
        .resource
        .as_ref()
        .unwrap()
        .attributes;
    let matching = attrs.iter().filter(|a| a.key == SOURCE_ATTRIBUTE).count();
    assert_eq!(
        matching, 1,
        "the forged value must be replaced, not left alongside the real one"
    );
    assert_eq!(
        resource_attr(&decoded, SOURCE_ATTRIBUTE),
        Some("github-copilot")
    );
}

#[test]
fn protobuf_and_json_stamp_identically_and_reenrichment_is_idempotent() {
    let request = request_with_metric("copilot_chat.session.count");
    let binary = enrich_metrics(&request.encode_to_vec(), WireFormat::Protobuf);
    let binary_decoded = ExportMetricsServiceRequest::decode(binary.as_slice()).unwrap();
    let json = enrich_metrics(&serde_json::to_vec(&request).unwrap(), WireFormat::Json);
    let json_decoded: ExportMetricsServiceRequest = serde_json::from_slice(&json).unwrap();
    assert_eq!(binary_decoded, json_decoded);
    assert_eq!(
        resource_attr(&json_decoded, SOURCE_ATTRIBUTE),
        Some("github-copilot")
    );
    assert_eq!(enrich_metrics(&binary, WireFormat::Protobuf), binary);
    assert_eq!(enrich_metrics(&json, WireFormat::Json), json);
}

#[test]
fn a_multi_resource_batch_stamps_each_resource_from_its_own_metrics() {
    let combined: ExportMetricsServiceRequest = serde_json::from_value(json!({"resourceMetrics":[
        {"resource":{"attributes":[]},"scopeMetrics":[{"metrics":[
            {"name":"copilot_chat.time_to_first_token","sum":{"dataPoints":[],"aggregationTemporality":1,"isMonotonic":true}}
        ]}]},
        {"resource":{"attributes":[]},"scopeMetrics":[{"metrics":[
            {"name":"gen_ai.client.token.usage","sum":{"dataPoints":[],"aggregationTemporality":1,"isMonotonic":true}}
        ]}]}
    ]}))
    .unwrap();
    let output = enrich_metrics(&serde_json::to_vec(&combined).unwrap(), WireFormat::Json);
    let decoded: ExportMetricsServiceRequest = serde_json::from_slice(&output).unwrap();
    assert_eq!(
        resource_attr(&decoded, SOURCE_ATTRIBUTE),
        Some("github-copilot")
    );
    assert_eq!(
        decoded.resource_metrics[1]
            .resource
            .as_ref()
            .unwrap()
            .attributes
            .iter()
            .find(|a| a.key == SOURCE_ATTRIBUTE),
        None,
        "the second resource has no copilot_chat.* metric of its own and must stay unstamped"
    );
}
