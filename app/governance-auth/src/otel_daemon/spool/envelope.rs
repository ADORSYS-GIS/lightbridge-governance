//! One line of the durable spool file: encode, append, decode.
//!
//! Kept apart from [`super`]'s orchestration because everything here is a
//! pure function of bytes in, bytes out -- nothing touches a [`super::
//! DurableSpool`]'s state, so it is exactly the part of this module worth
//! being able to read (and test) without the checkpoint in view.

use std::{fs::OpenOptions, io::Write, path::Path};

use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};

use crate::copilot::Signal;

#[derive(Serialize, Deserialize)]
struct Envelope {
    signal: Signal,
    body: String,
}

/// Serializes one record to its on-disk line (without the trailing newline
/// [`append_line`] adds).
pub(super) fn encode(signal: Signal, payload: &[u8]) -> Result<String> {
    serde_json::to_string(&Envelope {
        signal,
        body: STANDARD.encode(payload),
    })
    .context("serialising a retained OTLP payload for the durable spool")
}

/// The inverse of [`encode`]. Kept separate from deserializing `Envelope`
/// directly so a caller never has to name that type.
pub(super) fn decode(text: &str) -> Result<(Signal, Vec<u8>)> {
    let envelope: Envelope =
        serde_json::from_str(text).context("parsing a durable spool envelope")?;
    let body = STANDARD
        .decode(envelope.body)
        .context("base64-decoding a durable spool payload")?;
    Ok((envelope.signal, body))
}

/// Appends `line` plus a newline to `path`, creating the state directory and
/// the file (mode `0600`) as needed. `O_APPEND` on Unix, matching Copilot's
/// own outfile -- see the parent module doc's "torn write" paragraph for the
/// one thing this does not guarantee.
#[cfg(unix)]
pub(super) fn append_line(path: &Path, line: &str) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;

    if let Some(dir) = path.parent() {
        crate::copilot::private_file::create_dir(dir)?;
    }
    let mut file = OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("opening {} to append", path.display()))?;
    writeln!(file, "{line}").with_context(|| format!("appending to {}", path.display()))
}

#[cfg(not(unix))]
pub(super) fn append_line(path: &Path, line: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        crate::copilot::private_file::create_dir(dir)?;
    }
    let mut file = OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
        .with_context(|| format!("opening {} to append", path.display()))?;
    writeln!(file, "{line}").with_context(|| format!("appending to {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_protobuf_body_round_trips_through_the_envelope() {
        // The whole reason this is base64, not a raw line: a protobuf byte
        // can legitimately be `\n`.
        let payload = vec![0x0a, 0x03, b'\n', 0x12, 0x04, b'\r'];
        let line = encode(Signal::Logs, &payload).expect("encode");
        assert!(
            !line.as_bytes().contains(&b'\n'),
            "an encoded line must never itself contain a raw newline: {line:?}"
        );
        let (signal, decoded) = decode(&line).expect("decode");
        assert_eq!(signal, Signal::Logs);
        assert_eq!(decoded, payload);
    }

    #[test]
    fn a_metrics_signal_round_trips() {
        let line = encode(Signal::Metrics, b"hello").expect("encode");
        let (signal, decoded) = decode(&line).expect("decode");
        assert_eq!(signal, Signal::Metrics);
        assert_eq!(decoded, b"hello");
    }

    #[test]
    fn garbage_fails_to_decode_rather_than_panicking() {
        assert!(decode("not json").is_err());
        assert!(decode(r#"{"signal":"Logs","body":"not base64!!"}"#).is_err());
    }
}
