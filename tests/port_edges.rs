//! Edge-path tests for `pith-flac`: header variants, coded-number
//! widths, limit enforcement and streaminfo validation that the main
//! conformance suite (`flac.rs`) does not build streams for.
//!
//! Same philosophy as `flac.rs`: the bytes are assembled here, by hand,
//! with a test-local writer, so the decoder is measured against the
//! format definition rather than against anything it produced.

use pith_digest::Error;
use pith_flac::{Limits, decode, decode_streaminfo};

// ------------------------------------------------------------------
// Test-local writer (deliberately not shared with `flac.rs`: each
// harness computes its own bytes and its own CRCs)
// ------------------------------------------------------------------

struct BitW {
    bytes: Vec<u8>,
    bit: u32,
}

impl BitW {
    fn new() -> Self {
        BitW {
            bytes: Vec::new(),
            bit: 0,
        }
    }

    fn bits(&mut self, v: u64, n: u32) {
        for i in (0..n).rev() {
            self.bit(((v >> i) & 1) != 0);
        }
    }

    fn bit(&mut self, b: bool) {
        if self.bit == 0 {
            self.bytes.push(0);
        }
        if b {
            let last = self.bytes.len() - 1;
            self.bytes[last] |= 0x80 >> self.bit;
        }
        self.bit = (self.bit + 1) % 8;
    }

    fn signed(&mut self, v: i64, n: u32) {
        if n == 0 {
            return;
        }
        let masked = (v as u64) & ((1u64 << n) - 1);
        self.bits(masked, n);
    }

    fn pad(&mut self) {
        while self.bit != 0 {
            self.bit(false);
        }
    }

    fn bytes(&self) -> Vec<u8> {
        assert_eq!(self.bit, 0);
        self.bytes.clone()
    }
}

fn crc8(data: &[u8]) -> u8 {
    let mut crc = 0u8;
    for &b in data {
        crc ^= b;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 {
                (crc << 1) ^ 0x07
            } else {
                crc << 1
            };
        }
    }
    crc
}

fn crc16(data: &[u8]) -> u16 {
    let mut crc = 0u16;
    for &b in data {
        crc ^= u16::from(b) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x8005
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// A STREAMINFO body.
fn streaminfo_block(rate: u32, channels: u8, bps: u8, total: u64, last: bool) -> Vec<u8> {
    let mut w = BitW::new();
    w.bits(u64::from(last), 1);
    w.bits(0, 7);
    w.bits(34, 24);
    w.bits(192, 16);
    w.bits(4608, 16);
    w.bits(0, 24);
    w.bits(0, 24);
    w.bits(u64::from(rate), 20);
    w.bits(u64::from(channels - 1), 3);
    w.bits(u64::from(bps - 1), 5);
    w.bits(total, 36);
    for _ in 0..16 {
        w.bits(0, 8);
    }
    w.bytes()
}

fn stream(si: &[u8], frames: &[u8]) -> Vec<u8> {
    let mut v = b"fLaC".to_vec();
    v.extend_from_slice(si);
    v.extend_from_slice(frames);
    v
}

/// Frame header fields this harness needs; everything else is the
/// common case (block size via code 6, bps/rate deferred or explicit).
struct Header {
    block_code: u32,
    block_extra: u32,
    rate_code: u32,
    rate_extra: u32,
    chan_code: u32,
    bps_code: u32,
    number: u64,
    /// Raw coded-number bytes to splice in place of the real coding.
    raw_number: Option<Vec<u8>>,
}

/// The encoded number, RFC 9639 section 9.2.1.
fn coded(w: &mut BitW, v: u64) {
    if v < 0x80 {
        w.bits(v, 8);
        return;
    }
    let nbytes = (2..=7u32)
        .find(|&nb| v < (1u64 << (7 - nb + 6 * (nb - 1))))
        .expect("coded number fits");
    let lead_payload = 7 - nbytes;
    let lead = (0xFFu8 << (8 - nbytes))
        | (((v >> (6 * (nbytes - 1))) as u8) & ((1u8 << lead_payload) - 1));
    w.bits(u64::from(lead), 8);
    for i in (0..nbytes - 1).rev() {
        w.bits(0x80 | ((v >> (6 * i)) & 0x3F), 8);
    }
}

/// Assembles one frame: header + CRC-8, one constant or verbatim
/// subframe per the closure, byte pad, CRC-16.
fn frame_bytes(h: &Header, sub: impl FnOnce(&mut BitW)) -> Vec<u8> {
    let mut head = BitW::new();
    head.bits(0x3FFE, 14);
    head.bit(false);
    head.bit(false);
    head.bits(u64::from(h.block_code), 4);
    head.bits(u64::from(h.rate_code), 4);
    head.bits(u64::from(h.chan_code), 4);
    head.bits(u64::from(h.bps_code), 3);
    head.bit(false);
    match h.raw_number {
        Some(ref raw) => {
            for &b in raw {
                head.bits(u64::from(b), 8);
            }
        }
        None => coded(&mut head, h.number),
    }
    match h.block_code {
        6 => head.bits(u64::from(h.block_extra), 8),
        7 => head.bits(u64::from(h.block_extra), 16),
        _ => {}
    }
    match h.rate_code {
        12 => head.bits(u64::from(h.rate_extra), 8),
        13 | 14 => head.bits(u64::from(h.rate_extra), 16),
        _ => {}
    }
    let mut bytes = head.bytes();
    bytes.push(crc8(&bytes));
    let mut s = BitW::new();
    sub(&mut s);
    s.pad();
    bytes.extend(s.bytes());
    let crc = crc16(&bytes);
    bytes.push((crc >> 8) as u8);
    bytes.push(crc as u8);
    bytes
}

/// Constant subframe of `block_size` × `value`.
fn constant_sub(w: &mut BitW, value: i64, width: u32) {
    w.bit(false);
    w.bits(0, 6);
    w.bit(false);
    w.signed(value, width);
}

/// Verbatim subframe.
fn verbatim_sub(w: &mut BitW, samples: &[i64], width: u32) {
    w.bit(false);
    w.bits(1, 6);
    w.bit(false);
    for &s in samples {
        w.signed(s, width);
    }
}

fn dec(input: &[u8]) -> Result<pith_flac::Flac, Error> {
    decode(input, &Limits::default())
}

// ------------------------------------------------------------------
// Block size and rate header codes
// ------------------------------------------------------------------

#[test]
fn block_code_1_is_192() {
    let si = streaminfo_block(44_100, 1, 16, 192, true);
    let f = frame_bytes(
        &Header {
            block_code: 1,
            block_extra: 0,
            rate_code: 0,
            rate_extra: 0,
            chan_code: 0,
            bps_code: 0,
            number: 0,
            raw_number: None,
        },
        |w| constant_sub(w, 0x1234, 16),
    );
    let out = dec(&stream(&si, &f)).unwrap();
    assert_eq!(out.frames(), 192);
    assert!(out.samples().iter().all(|&v| v == 0x1234));
}

#[test]
fn rate_code_12_is_khz() {
    let si = streaminfo_block(0, 1, 16, 8, true);
    let f = frame_bytes(
        &Header {
            block_code: 6,
            block_extra: 7,
            rate_code: 12,
            rate_extra: 8,
            chan_code: 0,
            bps_code: 0,
            number: 0,
            raw_number: None,
        },
        |w| constant_sub(w, 0, 16),
    );
    assert_eq!(dec(&stream(&si, &f)).unwrap().sample_rate(), 8_000);
}

#[test]
fn rate_code_13_is_hz() {
    let si = streaminfo_block(0, 1, 16, 8, true);
    let f = frame_bytes(
        &Header {
            block_code: 6,
            block_extra: 7,
            rate_code: 13,
            rate_extra: 22_050,
            chan_code: 0,
            bps_code: 0,
            number: 0,
            raw_number: None,
        },
        |w| constant_sub(w, 0, 16),
    );
    assert_eq!(dec(&stream(&si, &f)).unwrap().sample_rate(), 22_050);
}

#[test]
fn rate_code_14_is_dahz() {
    let si = streaminfo_block(0, 1, 16, 8, true);
    let f = frame_bytes(
        &Header {
            block_code: 6,
            block_extra: 7,
            rate_code: 14,
            rate_extra: 441,
            chan_code: 0,
            bps_code: 0,
            number: 0,
            raw_number: None,
        },
        |w| constant_sub(w, 0, 16),
    );
    assert_eq!(dec(&stream(&si, &f)).unwrap().sample_rate(), 4_410);
}

#[test]
fn rate_code_15_is_rejected() {
    let si = streaminfo_block(0, 1, 16, 8, true);
    let f = frame_bytes(
        &Header {
            block_code: 6,
            block_extra: 7,
            rate_code: 15,
            rate_extra: 0,
            chan_code: 0,
            bps_code: 0,
            number: 0,
            raw_number: None,
        },
        |w| constant_sub(w, 0, 16),
    );
    assert_eq!(
        dec(&stream(&si, &f)),
        Err(Error::BadValue("FLAC invalid sample rate code"))
    );
}

#[test]
fn reserved_channel_assignment_is_rejected() {
    let si = streaminfo_block(44_100, 1, 16, 8, true);
    let f = frame_bytes(
        &Header {
            block_code: 6,
            block_extra: 7,
            rate_code: 0,
            rate_extra: 0,
            chan_code: 15,
            bps_code: 0,
            number: 0,
            raw_number: None,
        },
        |w| constant_sub(w, 0, 16),
    );
    assert_eq!(
        dec(&stream(&si, &f)),
        Err(Error::BadValue("FLAC reserved channel assignment"))
    );
}

// ------------------------------------------------------------------
// Bits-per-sample codes
// ------------------------------------------------------------------

#[test]
fn bps_code_3_is_rejected() {
    let si = streaminfo_block(44_100, 1, 16, 8, true);
    let f = frame_bytes(
        &Header {
            block_code: 6,
            block_extra: 7,
            rate_code: 0,
            rate_extra: 0,
            chan_code: 0,
            bps_code: 3,
            number: 0,
            raw_number: None,
        },
        |w| constant_sub(w, 0, 16),
    );
    assert_eq!(
        dec(&stream(&si, &f)),
        Err(Error::BadValue("FLAC reserved sample size code"))
    );
}

#[test]
fn bps_code_5_is_20bit() {
    let si = streaminfo_block(44_100, 1, 20, 4, true);
    let f = frame_bytes(
        &Header {
            block_code: 6,
            block_extra: 3,
            rate_code: 0,
            rate_extra: 0,
            chan_code: 0,
            bps_code: 5,
            number: 0,
            raw_number: None,
        },
        |w| verbatim_sub(w, &[524_287, -524_288, 0, 12_345], 20),
    );
    let out = dec(&stream(&si, &f)).unwrap();
    assert_eq!(out.bits_per_sample(), 20);
    assert_eq!(out.samples(), &[524_287, -524_288, 0, 12_345]);
}

#[test]
fn bps_code_7_is_32bit() {
    let si = streaminfo_block(44_100, 1, 32, 3, true);
    let f = frame_bytes(
        &Header {
            block_code: 6,
            block_extra: 2,
            rate_code: 0,
            rate_extra: 0,
            chan_code: 0,
            bps_code: 7,
            number: 0,
            raw_number: None,
        },
        |w| verbatim_sub(w, &[2_147_483_647, -2_147_483_648, 7], 32),
    );
    let out = dec(&stream(&si, &f)).unwrap();
    assert_eq!(out.bits_per_sample(), 32);
    assert_eq!(out.samples(), &[2_147_483_647, -2_147_483_648, 7]);
}

// ------------------------------------------------------------------
// Coded number widths
// ------------------------------------------------------------------

#[test]
fn coded_number_widths_two_through_six_decode() {
    // 2048 needs a 2-byte lead, 2^17 a 3-byte, 2^23 a 4-byte, 2^29 a
    // 5-byte, 2^35 the 0xFE 6-byte form.
    for number in [2_048u64, 1 << 17, 1 << 23, 1 << 29, 1 << 35] {
        let si = streaminfo_block(44_100, 1, 16, 8, true);
        let f = frame_bytes(
            &Header {
                block_code: 6,
                block_extra: 7,
                rate_code: 0,
                rate_extra: 0,
                chan_code: 0,
                bps_code: 0,
                number,
                raw_number: None,
            },
            |w| verbatim_sub(w, &[1, -2, 3, -4, 5, -6, 7, -8], 16),
        );
        let out = dec(&stream(&si, &f)).unwrap();
        assert_eq!(
            out.samples(),
            &[1, -2, 3, -4, 5, -6, 7, -8],
            "number {number}"
        );
    }
}

#[test]
fn coded_number_bad_continuation_is_rejected() {
    let si = streaminfo_block(44_100, 1, 16, 8, true);
    let f = frame_bytes(
        &Header {
            block_code: 6,
            block_extra: 7,
            rate_code: 0,
            rate_extra: 0,
            chan_code: 0,
            bps_code: 0,
            number: 0,
            // 0b1110_0000 lead announces two continuations; 0b1100_0000
            // is not a continuation byte.
            raw_number: Some(vec![0xE0, 0xC0, 0x00]),
        },
        |w| constant_sub(w, 0, 16),
    );
    assert_eq!(
        dec(&stream(&si, &f)),
        Err(Error::BadValue("FLAC coded number continuation"))
    );
}

// ------------------------------------------------------------------
// Predictor and partition guards
// ------------------------------------------------------------------

#[test]
fn predictor_order_exceeding_block_is_rejected() {
    let si = streaminfo_block(44_100, 1, 16, 2, true);
    // Fixed subframe, order 4 (a legal type code), on a 2-sample block:
    // the guard fires before any warmup is read.
    let f = frame_bytes(
        &Header {
            block_code: 6,
            block_extra: 1,
            rate_code: 0,
            rate_extra: 0,
            chan_code: 0,
            bps_code: 0,
            number: 0,
            raw_number: None,
        },
        |w| {
            w.bit(false);
            w.bits(8 + 4, 6);
            w.bit(false);
        },
    );
    assert_eq!(
        dec(&stream(&si, &f)),
        Err(Error::BadValue("FLAC predictor order exceeds block size"))
    );
}

#[test]
fn partition_smaller_than_order_is_rejected() {
    let si = streaminfo_block(44_100, 1, 16, 8, true);
    // Fixed order 4 on an 8-sample block with partition order 1: each
    // partition holds 4 samples, which is not greater than the order.
    let f = frame_bytes(
        &Header {
            block_code: 6,
            block_extra: 7,
            rate_code: 0,
            rate_extra: 0,
            chan_code: 0,
            bps_code: 0,
            number: 0,
            raw_number: None,
        },
        |w| {
            w.bit(false);
            w.bits(8 + 4, 6);
            w.bit(false);
            for _ in 0..4 {
                w.signed(0, 16);
            }
            w.bits(0, 2); // method: 4-bit parameters
            w.bits(1, 4); // partition order 1
        },
    );
    assert_eq!(
        dec(&stream(&si, &f)),
        Err(Error::BadValue("FLAC Rice partition smaller than order"))
    );
}

#[test]
fn partition_not_dividing_block_is_rejected() {
    let si = streaminfo_block(44_100, 1, 16, 4, true);
    // Fixed order 0 on a 4-sample block with partition order 3: eight
    // partitions cannot divide four samples.
    let f = frame_bytes(
        &Header {
            block_code: 6,
            block_extra: 3,
            rate_code: 0,
            rate_extra: 0,
            chan_code: 0,
            bps_code: 0,
            number: 0,
            raw_number: None,
        },
        |w| {
            w.bit(false);
            w.bits(8, 6);
            w.bit(false);
            w.bits(0, 2);
            w.bits(3, 4);
        },
    );
    assert_eq!(
        dec(&stream(&si, &f)),
        Err(Error::BadValue("FLAC Rice partition does not divide block"))
    );
}

// ------------------------------------------------------------------
// Limits and streaminfo validation
// ------------------------------------------------------------------

#[test]
fn decode_streaminfo_rejects_bad_magic() {
    assert_eq!(
        decode_streaminfo(b"nope"),
        Err(Error::InvalidMagic {
            what: "FLAC signature"
        })
    );
}

#[test]
fn decode_streaminfo_rejects_non_streaminfo_first_block() {
    let mut v = b"fLaC".to_vec();
    v.extend_from_slice(&[0b1000_0100, 0, 0, 4, 1, 2, 3, 4]); // type 4, len 4
    assert_eq!(
        decode_streaminfo(&v),
        Err(Error::BadValue(
            "FLAC STREAMINFO must be the first metadata block"
        ))
    );
}

#[test]
fn decode_streaminfo_rejects_wrong_length() {
    let mut v = b"fLaC".to_vec();
    v.extend_from_slice(&[0b1000_0000, 0, 0, 33]); // last flag, type 0, len 33
    v.extend_from_slice(&[0u8; 40]);
    assert_eq!(
        decode_streaminfo(&v),
        Err(Error::BadValue("FLAC STREAMINFO length"))
    );
}

#[test]
fn decode_enforces_max_input() {
    let si = streaminfo_block(44_100, 1, 16, 8, true);
    let limits = Limits {
        max_input: 4,
        ..Limits::default()
    };
    assert_eq!(
        decode(&stream(&si, &[]), &limits),
        Err(Error::TooLarge {
            what: "FLAC input",
            limit: 4
        })
    );
}

#[test]
fn decode_enforces_max_output() {
    let si = streaminfo_block(44_100, 1, 16, 8, true);
    let f = frame_bytes(
        &Header {
            block_code: 6,
            block_extra: 7,
            rate_code: 0,
            rate_extra: 0,
            chan_code: 0,
            bps_code: 0,
            number: 0,
            raw_number: None,
        },
        |bw| constant_sub(bw, 7, 16),
    );
    let limits = Limits {
        max_input: Limits::default().max_input,
        max_output: 16, // caps the buffer at 4 samples; the frame yields 8
    };
    assert_eq!(
        decode(&stream(&si, &f), &limits),
        Err(Error::TooLarge {
            what: "FLAC decoded PCM",
            limit: 16
        })
    );
}

#[test]
fn decode_rejects_stream_exceeding_declared_total() {
    // STREAMINFO declares 4 samples; two 4-sample frames follow.
    let si = streaminfo_block(44_100, 1, 16, 4, true);
    let mk = |number: u64| {
        frame_bytes(
            &Header {
                block_code: 6,
                block_extra: 3,
                rate_code: 0,
                rate_extra: 0,
                chan_code: 0,
                bps_code: 0,
                number,
                raw_number: None,
            },
            |w| constant_sub(w, 1, 16),
        )
    };
    let mut frames = mk(0);
    frames.extend(mk(1));
    assert_eq!(
        dec(&stream(&si, &frames)),
        Err(Error::BadValue(
            "FLAC stream exceeds declared total samples"
        ))
    );
}

#[test]
fn decode_reports_truncated_metadata_block() {
    // STREAMINFO (last=0) followed by a block header announcing 10 body
    // bytes of which only 4 are present: the body must span offsets
    // 46..56 but the input ends at 50.
    let mut si = streaminfo_block(44_100, 1, 16, 8, false);
    si.extend_from_slice(&[0b0000_0010, 0, 0, 10, 1, 2, 3, 4]);
    assert_eq!(
        dec(&stream(&si, &[])).map(|_| ()),
        Err(Error::Truncated {
            what: "FLAC metadata block",
            needed: 56,
            found: 50
        })
    );
}
