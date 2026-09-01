//! OTLP/HTTP **JSON** primitives: the pieces both signals share.
//!
//! JSON, not protobuf, is a first-class OTLP/HTTP encoding
//! (`Content-Type: application/json`, opentelemetry-specification
//! `protocol/otlp.md`), so this drain needs no `.proto` toolchain and no
//! generated code. The cost is the proto3 JSON mapping's one sharp edge:
//! **64-bit integers are strings**, not numbers. `timeUnixNano`,
//! `bucketCounts` and `count` are all emitted as strings for that reason --
//! a collector that parses them as `float64` would silently round timestamps
//! past millisecond precision.
//!
//! Nothing here inspects a value's meaning; it only re-shapes. Deciding what
//! is worth sending is [`super::metrics`]/[`super::logs`]' job.

use serde_json::{Map, Value, json};

use super::record::{HrTime, Resource, Scope};

/// One `AnyValue`. `null` becomes an AnyValue with no field set, which the
/// spec defines as the empty value -- dropping the key entirely would change
/// the attribute set, and inventing `""` would change its type.
pub fn any_value(value: &Value) -> Value {
    match value {
        Value::String(text) => json!({ "stringValue": text }),
        Value::Bool(flag) => json!({ "boolValue": flag }),
        Value::Number(number) => match number.as_i64() {
            Some(integer) => json!({ "intValue": integer.to_string() }),
            None => json!({ "doubleValue": number.as_f64().unwrap_or_default() }),
        },
        Value::Array(items) => {
            let values: Vec<Value> = items.iter().map(any_value).collect();
            json!({ "arrayValue": { "values": values } })
        }
        Value::Object(map) => json!({ "kvlistValue": { "values": attributes(map) } }),
        Value::Null => json!({}),
    }
}

/// A JS-SDK attribute map (`{"gen_ai.token.type": "input"}`) as OTLP's
/// `repeated KeyValue`.
pub fn attributes(map: &Map<String, Value>) -> Vec<Value> {
    map.iter()
        .map(|(key, value)| json!({ "key": key, "value": any_value(value) }))
        .collect()
}

/// A resource's `_rawAttributes` (`[[key, value], ...]`) as `repeated
/// KeyValue`. Entries that are not a two-or-more-element array with a string
/// first element are dropped individually -- see [`Resource`]'s doc for why
/// one malformed pair must not cost the whole record.
pub fn resource_attributes(raw: &[Value]) -> Vec<Value> {
    raw.iter()
        .filter_map(|entry| {
            let pair = entry.as_array()?;
            let key = pair.first()?.as_str()?;
            let value = pair.get(1).unwrap_or(&Value::Null);
            Some(json!({ "key": key, "value": any_value(value) }))
        })
        .collect()
}

pub fn resource(resource: &Resource) -> Value {
    json!({ "attributes": resource_attributes(&resource.raw_attributes) })
}

pub fn scope(scope: &Scope) -> Value {
    let mut object = Map::new();
    object.insert(
        "name".to_owned(),
        Value::String(scope.name.clone().unwrap_or_default()),
    );
    if let Some(version) = &scope.version {
        object.insert("version".to_owned(), Value::String(version.clone()));
    }
    Value::Object(object)
}

/// `[seconds, nanoseconds]` as OTLP's `timeUnixNano`.
///
/// `None` on absence, a short array, or arithmetic that would overflow --
/// every one of which means "we do not know when this happened", and OTLP's
/// own answer to that is to omit the field rather than send an epoch of 0.
pub fn time_unix_nano(time: Option<&HrTime>) -> Option<String> {
    let time = time?;
    let seconds = *time.first()?;
    let nanos = time.get(1).copied().unwrap_or_default();
    let total = seconds.checked_mul(1_000_000_000)?.checked_add(nanos)?;
    u64::try_from(total).ok().map(|nanos| nanos.to_string())
}

/// Inserts `key` only when the timestamp is known. See [`time_unix_nano`].
pub fn insert_time(object: &mut Map<String, Value>, key: &str, time: Option<&HrTime>) {
    if let Some(nanos) = time_unix_nano(time) {
        object.insert(key.to_owned(), Value::String(nanos));
    }
}

/// One record's contribution: the resource it was exported under, the scope
/// it came from, and the already-transformed OTLP items.
pub type Grouped = (Value, Value, Vec<Value>);

/// The accumulator [`group`] folds into: a resource, and the scopes seen under
/// it with their items. A named alias only because the inline form trips
/// `clippy::type_complexity`; it has no other reader.
type ByResource = Vec<(Value, Vec<(Value, Vec<Value>)>)>;

/// Folds per-record triples into OTLP's `resource -> scope -> items` nesting.
///
/// Every spool line repeats the same resource and scope (they are per-session
/// constants), so pushing one `resourceMetrics`/`resourceLogs` entry per line
/// would multiply the payload by the record count for no added information.
/// Linear search rather than a hash map: the key is a whole `Value` and the
/// group count is one or two in practice, so building a hashable key would
/// cost more than the scan it saves.
pub fn group(entries: Vec<Grouped>, scope_field: &str, item_field: &str) -> Vec<Value> {
    let mut grouped: ByResource = Vec::new();
    for (resource, scope, items) in entries {
        if items.is_empty() {
            continue;
        }
        let index = match grouped.iter().position(|(seen, _)| *seen == resource) {
            Some(index) => index,
            None => {
                grouped.push((resource, Vec::new()));
                grouped.len().saturating_sub(1)
            }
        };
        let Some((_, by_scope)) = grouped.get_mut(index) else {
            continue;
        };
        match by_scope.iter_mut().find(|(seen, _)| *seen == scope) {
            Some((_, existing)) => existing.extend(items),
            None => by_scope.push((scope, items)),
        }
    }

    grouped
        .into_iter()
        .map(|(resource, by_scope)| {
            let scopes: Vec<Value> = by_scope
                .into_iter()
                .map(|(scope, items)| {
                    let mut object = Map::new();
                    object.insert("scope".to_owned(), scope);
                    object.insert(item_field.to_owned(), Value::Array(items));
                    Value::Object(object)
                })
                .collect();
            let mut object = Map::new();
            object.insert("resource".to_owned(), resource);
            object.insert(scope_field.to_owned(), Value::Array(scopes));
            Value::Object(object)
        })
        .collect()
}
