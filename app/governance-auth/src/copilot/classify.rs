//! Which of [`super::record`]'s shapes a spool line carries.
//!
//! Split out of that module only to keep both halves under the 200-LoC gate --
//! same reason as `dashboard::style` -- so it is re-exported there and every
//! caller still says `record::classify`.
//!
//! Dispatched on *key presence* rather than a `#[serde(untagged)]` enum:
//! untagged tries each variant and reports only "data did not match any
//! variant", and both shapes deserialise successfully from `{}` (every field
//! is optional), so untagged would silently classify empty and unknown records
//! as metrics. The live spool contains 22 literal `{}` lines out of 98, so
//! that is not a hypothetical.

use serde_json::Value;

/// ⚠️ [`Kind::Empty`] is separate from [`Kind::Unknown`] on purpose, and the
/// distinction is load-bearing rather than tidy. An unrecognised record is
/// **lost data** and `status` colours it accordingly; a `{}` record carries
/// nothing to lose. Folding the two together would put 22 of every 98 records
/// into the loss counter on a perfectly healthy install, and a row that is
/// always red is a row nobody reads -- which is exactly how a real parser
/// regression stays invisible.
///
/// ⚠️⚠️ A log line is recognised by `_body` **alone**, and it used to be
/// recognised by `_body` *or* `hrTime`. That `or` opened a third outcome
/// between "delivered" and "recorded as lost": **delivered empty**. A Copilot
/// release renaming only `_body` -- strictly smaller, and so likelier, than
/// the two-field rename -- still matched on `hrTime`, still parsed (every
/// field of [`super::record::LogRecord`] is optional), and still exported: a
/// timestamp, some attributes, and no body. Nothing counted it and `status`
/// stayed green while the content of every log line went missing. `_body` is
/// the field that carries the record, so a line without it is a shape this
/// build does not recognise -- which is what [`Kind::Unknown`] means, and it
/// routes to the loss counter.
pub fn classify(line: &Value) -> Kind {
    let Some(object) = line.as_object() else {
        return Kind::Unknown;
    };
    if object.is_empty() {
        return Kind::Empty;
    }
    if object.contains_key("scopeMetrics") {
        return Kind::Metrics;
    }
    if object.contains_key("_body") {
        return Kind::Log;
    }
    Kind::Unknown
}

#[derive(Debug, PartialEq, Eq)]
pub enum Kind {
    Metrics,
    Log,
    /// A literal `{}`: known-benign, carries nothing, counted apart from loss.
    Empty,
    Unknown,
}
