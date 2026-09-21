//! Tests for [`super::super::decode_chunk_with_carry`] — the UTF-8 carry
//! logic that keeps a chunk boundary landing mid-codepoint from failing the
//! request (issue #180 split).

use super::super::*;

#[test]
fn decode_chunk_with_carry_reassembles_a_split_codepoint() {
    // "é" = 0xC3 0xA9. Split across two calls so a lone leading byte is
    // carried, exactly what a fixed-size upstream frame boundary does.
    let mut carry = Vec::new();

    let first = decode_chunk_with_carry(&mut carry, b"caf").expect("ascii prefix");
    assert_eq!(first, "caf");
    assert!(carry.is_empty());

    let split = decode_chunk_with_carry(&mut carry, &[0xC3]).expect("lone leading byte");
    assert_eq!(
        split, "",
        "nothing releasable yet -- codepoint is incomplete"
    );
    assert_eq!(carry, vec![0xC3]);

    let rest = decode_chunk_with_carry(&mut carry, &[0xA9, b'!']).expect("completes it");
    assert_eq!(rest, "é!");
    assert!(carry.is_empty());
}

#[test]
fn decode_chunk_with_carry_rejects_genuinely_malformed_bytes() {
    let mut carry = Vec::new();
    // 0xFF is never a valid UTF-8 leading byte -- no continuation byte
    // could ever complete it, unlike a mere split boundary.
    assert!(decode_chunk_with_carry(&mut carry, &[0xFF, 0xFE]).is_err());
}
