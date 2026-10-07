//! Decode-path conformance against the committed `tone.flac` fixture.
//!
//! The fixture is byte-identical to the `tone.flac` in the upstream
//! `modhash` test corpus (sha256 `f954b800b5476f06...`, a hand-built
//! stream of verbatim subframes per RFC 9639 — no encoder library
//! involved), so these pins travel with the format, not with this
//! port. The expected values are the ones recorded in
//! `reference.json` by `tools/gen-reference`.

use pith_digest::{fnv1a64, sha256};
use pith_flac::{Limits, decode, decode_streaminfo};

const TONE_FLAC: &[u8] = include_bytes!("fixtures/tone.flac");

fn pcm_le(samples: &[i32]) -> Vec<u8> {
    let mut v = Vec::with_capacity(samples.len() * 4);
    for &s in samples {
        v.extend_from_slice(&s.to_le_bytes());
    }
    v
}

#[test]
fn tone_fixture_streaminfo_fields() {
    let si = decode_streaminfo(TONE_FLAC).unwrap();
    assert_eq!(si.sample_rate, 44_100);
    assert_eq!(si.channels, 1);
    assert_eq!(si.bits_per_sample, 16);
    assert_eq!(si.total_samples, 44_100);
    assert_eq!(si.min_block_size, 16);
    assert_eq!(si.max_block_size, 65_535);
}

#[test]
fn tone_fixture_decodes_to_reference_pcm() {
    let out = decode(TONE_FLAC, &Limits::default()).unwrap();
    assert_eq!(out.sample_rate(), 44_100);
    assert_eq!(out.channels(), 1);
    assert_eq!(out.bits_per_sample(), 16);
    assert_eq!(out.total_samples(), 44_100);
    assert_eq!(out.frames(), 44_100);
    assert_eq!(out.samples().len(), 44_100);

    let pcm = pcm_le(out.samples());
    let sha = sha256(&pcm).unwrap();
    assert_eq!(
        crate::to_hex(sha.as_bytes()),
        "2514eff527051953533d999b57bc3745386432e2a37dcc32ccab77fb67faee6f"
    );
    assert_eq!(fnv1a64(&pcm), 0x3a11_60e7_7c61_f2b9);
}

/// Lowercase hex without allocating through format machinery per byte.
fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}
