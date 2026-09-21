//! Tests for the incremental SSE response path (issue #180 split).

use super::{
    super::*, engine, extract_body, metrics, response_state_with_content_type,
    response_state_with_content_type_and_encoding,
};

/// Reproduces the production incident directly (2026-08-03): an SSE
/// delta containing an em dash (3-byte UTF-8) split across chunk
/// boundaries at every possible cut point must still deliver the
/// content, not fail the request.
#[test]
fn split_codepoint_across_response_chunks_does_not_fail_the_request() {
    let e = engine();
    let m = metrics();
    // Content-Type is what routes this to the SSE path under test; a
    // response with no headers at all would take the safe Buffered
    // default instead (see the module doc), which is the wrong path for
    // this specific incident.
    let mut state = response_state_with_content_type(64, "text/event-stream");

    let frame =
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\u{2014}there\"}}]}\n\n";
    let bytes = frame.as_bytes();
    let dash_at = frame.find('\u{2014}').expect("frame contains the dash");
    assert_eq!("\u{2014}".len(), 3, "em dash must be 3 bytes for this test");

    // Cut so chunk 1 ends after the dash's first byte, chunk 2 is just
    // its second byte, chunk 3 carries the third byte plus the rest.
    let (chunk1, remainder) = bytes.split_at(dash_at + 1);
    let (chunk2, chunk3) = remainder.split_at(1);

    let mut collected = Vec::new();
    for (chunk, last) in [(chunk1, false), (chunk2, false), (chunk3, true)] {
        let resp = handle_response_chunk(&e, &m, &mut state, chunk, last);
        assert!(
            !matches!(&resp.response, Some(Resp::ImmediateResponse(_))),
            "request was rejected on a mere chunk-boundary split: {resp:?}"
        );
        if let Some(bytes) = extract_body(&resp) {
            collected.extend_from_slice(&bytes);
        }
    }

    let out = String::from_utf8(collected).expect("output is valid utf8");
    assert!(
        out.contains("hi\u{2014}there"),
        "content lost or mangled: {out}"
    );
}

/// The narrower failure mode this fix must still catch: bytes that are
/// not a chunk-boundary artifact but genuinely invalid, with no more
/// input ever coming to complete them.
#[test]
fn genuinely_malformed_final_chunk_still_fails_closed() {
    let e = engine();
    let m = metrics();
    // Routed via the SSE path specifically -- this is the UTF-8 carry
    // integration under test, not the Buffered path's independent
    // "not valid JSON" refusal (which would also fail closed here, but
    // for a different reason than the one this test names).
    let mut state = response_state_with_content_type(64, "text/event-stream");
    // opengrep-ignore: test vector, not a token
    let resp = handle_response_chunk(&e, &m, &mut state, &[0xFF, 0xFE], true);
    assert!(
        matches!(&resp.response, Some(Resp::ImmediateResponse(_))),
        "genuinely malformed UTF-8 must still fail closed: {resp:?}"
    );
}

/// The gzip-SSE branch buffers compressed chunks, decompresses at
/// end_of_stream, scans through the SSE holdback, and re-compresses the
/// emitted output so the delivered body matches the still-present
/// `Content-Encoding: gzip` header. Without the re-compress step a
/// gzip-decoding client would receive identity bytes stamped as gzip.
#[test]
fn gzip_sse_is_decompressed_scanned_and_recompressed() {
    let e = engine();
    let m = metrics();
    let mut state = response_state_with_content_type_and_encoding(64, "text/event-stream", "gzip");

    // Assert the gzip-SSE mode actually engaged (gzip_buf armed).
    assert!(
        state.gzip_buf.is_some(),
        "SSE+gzip headers must arm the gzip buffer"
    );

    // Build gzip-compressed SSE: "data: hello from upstream\n\n".
    use std::io::Write;

    use flate2::{Compression, write::GzEncoder};
    let plaintext = b"data: hello from upstream\n\n";
    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    enc.write_all(plaintext).expect("write");
    let compressed = enc.finish().expect("finish");

    let resp = handle_response_chunk(&e, &m, &mut state, &compressed, true);
    let body = extract_body(&resp).expect("SSE gzip must emit a scanned body");

    // The emitted body must still be gzip-encoded (magic bytes) to match the
    // Content-Encoding: gzip header, NOT plaintext.
    assert!(
        body.starts_with(&[0x1f, 0x8b]),
        "gzip-SSE output must be re-compressed, got plaintext: {:?}",
        String::from_utf8_lossy(&body)
    );

    // Decompress it back and confirm the SSE line survived the round-trip.
    use std::io::Read;

    use flate2::read::GzDecoder;
    let mut dec = GzDecoder::new(&body[..]);
    let mut roundtrip = String::new();
    dec.read_to_string(&mut roundtrip).expect("decompress");
    assert_eq!(roundtrip, "data: hello from upstream\n\n");
}

/// A gzip decompression bomb (small compressed input that expands to far
/// more than `MAX_BUFFERED_RESPONSE_BYTES`) must be refused while
/// decompressing, not allocate gigabytes and only fail after the fact.
#[test]
fn gzip_decompression_bomb_is_refused_rather_than_oom() {
    use std::io::Write;

    use flate2::{Compression, write::GzEncoder};

    // Highly compressible input: >MAX bytes of 'x' compresses to far less
    // than MAX, but decompresses to a size that exceeds MAX_BUFFERED_RESPONSE_BYTES.
    const BOMB_SRC: usize = MAX_BUFFERED_RESPONSE_BYTES + (16 * 1024 * 1024);
    let mut enc = GzEncoder::new(Vec::new(), Compression::new(9));
    enc.write_all(&vec![b'x'; BOMB_SRC]).expect("write");
    let bomb = enc.finish().expect("finish");
    // Sanity: the compressed bomb must be much smaller than its intent
    // (otherwise the test isn't exercising the decompression cap).
    assert!(
        bomb.len() < MAX_BUFFERED_RESPONSE_BYTES,
        "bomb did not compress enough: {}",
        bomb.len()
    );

    let mut buf = bomb;
    let err = decompress_gzip(&mut buf).expect_err("decompression bomb must be refused");
    assert!(
        err.contains("exceeds"),
        "expected an 'exceeds' refusal, got: {err}"
    );
}
