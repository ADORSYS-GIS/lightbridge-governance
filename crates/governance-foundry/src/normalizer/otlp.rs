//! Shared helpers for parsing real OTLP JSON (the proto3 JSON mapping).
//!
//! The collector's export wire format is the OTLP protobuf JSON mapping, not a
//! hand-flattened shape: every `attributes` field is an *array* of
//! `{ "key": ..., "value": { "stringValue": ... | "intValue": ... } }`
//! objects, and `int64`/`uint64` fields (`startTimeUnixNano`,
//! `endTimeUnixNano`, `intValue`) are serialized as **decimal strings** per
//! the proto3 spec. These helpers know that shape so the per-provider
//! normalizers don't each re-parse it (and don't silently break on a real
//! collector export).

use serde_json::Value;

use super::NormalizerError;

/// Returns an OTLP object's `attributes` array. Missing attributes behave as
/// an empty array (a span with no attributes is valid OTLP); a present but
/// non-array `attributes` is malformed and rejects.
fn attributes<'a>(obj: &'a Value, context: &str) -> Result<&'a [Value], NormalizerError> {
    match obj.get("attributes") {
        None | Some(Value::Null) => Ok(&[]),
        Some(Value::Array(attrs)) => Ok(attrs),
        Some(other) => Err(NormalizerError::InvalidFieldType {
            field: format!("{context}.attributes"),
            expected: "array".to_owned(),
            actual: other.to_string(),
        }),
    }
}

/// The `value` object for the attribute with the given key, if present.
fn attribute_value<'a>(
    attrs: &'a [Value],
    key: &str,
    context: &str,
) -> Result<Option<&'a Value>, NormalizerError> {
    for attr in attrs {
        let attr_key = match attr.get("key") {
            Some(Value::String(k)) => k.as_str(),
            Some(other) => {
                return Err(NormalizerError::InvalidFieldType {
                    field: format!("{context}.attributes[].key"),
                    expected: "string".to_owned(),
                    actual: other.to_string(),
                });
            }
            None => continue,
        };
        if attr_key == key {
            return Ok(Some(attr));
        }
    }
    Ok(None)
}

fn value_of<'a>(attr: &'a Value, key: &str, context: &str) -> Result<&'a Value, NormalizerError> {
    let value = attr.get("value").ok_or_else(|| {
        NormalizerError::MissingField(format!("{context}.attributes[{key}].value"))
    })?;
    match value {
        Value::Object(_) => Ok(value),
        other => Err(NormalizerError::InvalidFieldType {
            field: format!("{context}.attributes[{key}].value"),
            expected: "object".to_owned(),
            actual: other.to_string(),
        }),
    }
}

/// Extracts an optional string attribute (`value.stringValue`).
pub fn attr_string(
    obj: &Value,
    key: &str,
    context: &str,
) -> Result<Option<String>, NormalizerError> {
    let attrs = attributes(obj, context)?;
    let Some(attr) = attribute_value(attrs, key, context)? else {
        return Ok(None);
    };
    let value = value_of(attr, key, context)?;
    match value.get("stringValue") {
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(Value::Null) | None => Ok(None),
        Some(other) => Err(NormalizerError::InvalidFieldType {
            field: format!("{context}.attributes[{key}].stringValue"),
            expected: "string".to_owned(),
            actual: other.to_string(),
        }),
    }
}

/// Extracts an optional integer attribute. `intValue` is a decimal string in
/// proto3 JSON, but accepts a JSON number too for tolerant fixtures.
pub fn attr_i64(obj: &Value, key: &str, context: &str) -> Result<Option<i64>, NormalizerError> {
    let attrs = attributes(obj, context)?;
    let Some(attr) = attribute_value(attrs, key, context)? else {
        return Ok(None);
    };
    let value = value_of(attr, key, context)?;
    match value.get("intValue") {
        Some(Value::String(s)) => {
            s.parse::<i64>()
                .map(Some)
                .map_err(|_| NormalizerError::InvalidFieldType {
                    field: format!("{context}.attributes[{key}].intValue"),
                    expected: "int64 string".to_owned(),
                    actual: s.clone(),
                })
        }
        Some(Value::Number(n)) => {
            n.as_i64()
                .map(Some)
                .ok_or_else(|| NormalizerError::InvalidFieldType {
                    field: format!("{context}.attributes[{key}].intValue"),
                    expected: "int64".to_owned(),
                    actual: n.to_string(),
                })
        }
        Some(Value::Null) | None => Ok(None),
        Some(other) => Err(NormalizerError::InvalidFieldType {
            field: format!("{context}.attributes[{key}].intValue"),
            expected: "int64".to_owned(),
            actual: other.to_string(),
        }),
    }
}

/// Extracts a top-level int64 span field (`startTimeUnixNano` etc.), which is
/// a decimal string in proto3 JSON (also tolerant of a JSON number).
pub fn span_i64(span: &Value, key: &str) -> Result<i64, NormalizerError> {
    match span.get(key) {
        Some(Value::String(s)) => s
            .parse::<i64>()
            .map_err(|_| NormalizerError::InvalidFieldType {
                field: key.to_owned(),
                expected: "int64 string".to_owned(),
                actual: s.clone(),
            }),
        Some(Value::Number(n)) => n.as_i64().ok_or_else(|| NormalizerError::InvalidFieldType {
            field: key.to_owned(),
            expected: "int64".to_owned(),
            actual: n.to_string(),
        }),
        None => Err(NormalizerError::MissingField(key.to_owned())),
        Some(other) => Err(NormalizerError::InvalidFieldType {
            field: key.to_owned(),
            expected: "int64".to_owned(),
            actual: other.to_string(),
        }),
    }
}

/// Extracts an optional top-level string span field (`traceId`, `spanId`).
pub fn span_string(span: &Value, key: &str) -> Result<Option<String>, NormalizerError> {
    match span.get(key) {
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(Value::Null) | None => Ok(None),
        Some(other) => Err(NormalizerError::InvalidFieldType {
            field: key.to_owned(),
            expected: "string".to_owned(),
            actual: other.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn attr_string_reads_real_otlp_shape() {
        let span = json!({
            "attributes": [{
                "key": "model.name",
                "value": { "stringValue": "claude-3-sonnet" }
            }]
        });
        assert_eq!(
            attr_string(&span, "model.name", "span").expect("parse"),
            Some("claude-3-sonnet".to_owned())
        );
    }

    #[test]
    fn attr_i64_reads_string_encoded_int() {
        let span = json!({
            "attributes": [{
                "key": "tokens.input",
                "value": { "intValue": "1000" }
            }]
        });
        assert_eq!(
            attr_i64(&span, "tokens.input", "span").expect("parse"),
            Some(1000)
        );
    }

    #[test]
    fn missing_attribute_is_none_not_an_error() {
        let span = json!({});
        assert_eq!(attr_string(&span, "missing", "span").expect("parse"), None);
        assert_eq!(attr_i64(&span, "missing", "span").expect("parse"), None);
    }

    #[test]
    fn span_i64_accepts_string_and_number() {
        assert_eq!(
            span_i64(
                &json!({"startTimeUnixNano": "1700000000000000000"}),
                "startTimeUnixNano"
            )
            .expect("parse"),
            1_700_000_000_000_000_000
        );
        assert_eq!(
            span_i64(
                &json!({"startTimeUnixNano": 1700000000000000000_i64}),
                "startTimeUnixNano"
            )
            .expect("parse"),
            1_700_000_000_000_000_000
        );
    }

    #[test]
    fn malformed_attribute_rejects() {
        // A `value` that is not an object at all is structurally invalid OTLP
        // and must reject, not be silently treated as an absent attribute.
        let span = json!({
            "attributes": [{
                "key": "model.name",
                "value": "not-an-object"
            }]
        });
        assert!(matches!(
            attr_string(&span, "model.name", "span"),
            Err(NormalizerError::InvalidFieldType { .. })
        ));
    }

    #[test]
    fn wrong_value_kind_is_none_not_an_error() {
        // An attribute whose value is an int is simply not a string attribute;
        // the normalizer turns the resulting None into a MissingField error
        // for required fields. Parsing itself must not reject.
        let span = json!({
            "attributes": [{
                "key": "model.name",
                "value": { "intValue": "42" }
            }]
        });
        assert_eq!(
            attr_string(&span, "model.name", "span").expect("parse"),
            None
        );
    }
}
