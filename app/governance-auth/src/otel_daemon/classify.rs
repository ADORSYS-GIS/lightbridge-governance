//! Preserve the signal rather than defaulting unknown binary exports to logs.

use serde_json::Value;

use super::{protobuf, receive::WireFormat, signal::Signal};

/// JSON declares its signal in its envelope. Binary exports use explicit
/// signal paths, or (for older clients posting to `/`) schema inference.
/// Unknown/ambiguous root exports are refused before durable admission.
pub fn signal(body: &[u8], format: WireFormat, path: &str) -> Option<Signal> {
    let declared = Signal::from_path(path);
    match format {
        WireFormat::Json => {
            let value: Value = serde_json::from_slice(body).ok()?;
            let mut signals = [Signal::Logs, Signal::Metrics, Signal::Traces]
                .into_iter()
                .filter(|s| value.get(s.json_key()).is_some());
            let found = signals.next()?;
            if signals.next().is_some() || declared.is_some_and(|s| s != found) {
                return None;
            }
            Some(found)
        }
        WireFormat::Protobuf => {
            let inferred = protobuf::signal(body);
            if declared.zip(inferred).is_some_and(|(a, b)| a != b) {
                return None;
            }
            declared.or(inferred)
        }
    }
}

#[cfg(test)]
mod tests;
