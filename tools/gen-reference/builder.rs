//! Deterministic FLAC stream construction for the reference vectors.
//!
//! The suite has no FLAC encoder, so the vectors' synthetic inputs are
//! assembled here bit-by-bit exactly as RFC 9639 describes — the same
//! construction the integration tests use — and the decoder is then
//! measured on those bytes. Every function is pure and every stream is
//! byte-stable across runs and platforms: no RNG, no time, no floats.

use std::vec::Vec;

/// MSB-first bit writer: bit order is bit 7 of each byte first, matching
/// the stream order FLAC uses for every field.
pub struct BitW {
    bytes: Vec<u8>,
    bit: u32,
}

impl BitW {
    /// A writer at bit 0 of an empty buffer.
    pub fn new() -> Self {
        BitW {
            bytes: Vec::new(),
            bit: 0,
        }
    }

    /// Writes the low `n` bits of `v`, MSB first.
    pub fn bits(&mut self, v: u64, n: u32) {
        assert!(n <= 64);
        assert!(
            n == 64 || v < (1u64 << n),
            "value {v} does not fit in {n} bits"
        );
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

    /// Signed two's-complement field of `n` bits.
    pub fn signed(&mut self, v: i64, n: u32) {
        if n == 0 {
            assert_eq!(v, 0, "a 0-bit field can only encode 0");
            return;
        }
        assert!(
            v >= -(1i64 << (n - 1)) && v < (1i64 << (n - 1)),
            "signed value {v} does not fit in {n} bits"
        );
        let masked = (v as u64) & ((1u64 << n) - 1);
        self.bits(masked, n);
    }

    /// FLAC unary: `q` zero bits then a one bit.
    pub fn unary(&mut self, q: u32) {
        for _ in 0..q {
            self.bit(false);
        }
        self.bit(true);
    }

    /// Pads with zero bits to the next byte boundary.
    pub fn pad(&mut self) {
        while self.bit != 0 {
            self.bit(false);
        }
    }

    /// Whole bytes written so far; only valid when byte-aligned.
    pub fn bytes(&self) -> Vec<u8> {
        assert_eq!(self.bit, 0, "byte-aligned read of BitW");
        self.bytes.clone()
    }
}

/// CRC-8 as FLAC uses it (poly 0x07, init 0, MSB-first).
pub fn crc8(data: &[u8]) -> u8 {
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

/// CRC-16 as FLAC uses it (poly 0x8005, init 0, MSB-first).
pub fn crc16(data: &[u8]) -> u16 {
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

/// A STREAMINFO block. `last` is the last-metadata-block flag.
pub fn streaminfo_block(rate: u32, channels: u8, bps: u8, total: u64, last: bool) -> Vec<u8> {
    let mut w = BitW::new();
    // Block header: last flag, type 0 (STREAMINFO), length 34.
    w.bits(u64::from(last), 1);
    w.bits(0, 7);
    w.bits(34, 24);
    // Body (RFC 9639 section 8.2).
    w.bits(192, 16); // min block size
    w.bits(4608, 16); // max block size
    w.bits(0, 24); // min frame size: unknown
    w.bits(0, 24); // max frame size: unknown
    w.bits(u64::from(rate), 20);
    w.bits(u64::from(channels - 1), 3);
    w.bits(u64::from(bps - 1), 5);
    w.bits(total, 36);
    for _ in 0..16 {
        w.bits(0, 8); // md5
    }
    w.bytes()
}

/// The UTF-8-style coded frame/sample number, RFC 9639 section 9.2.1.
fn coded(w: &mut BitW, v: u64) {
    if v < 0x80 {
        w.bits(v, 8);
        return;
    }
    let nbytes = (2..=7u32)
        .find(|&nb| v < (1u64 << (7 - nb + 6 * (nb - 1))))
        .expect("coded number fits in 7 bytes");
    let lead_payload = 7 - nbytes;
    let lead = (0xFFu8 << (8 - nbytes))
        | (((v >> (6 * (nbytes - 1))) as u8) & ((1u8 << lead_payload) - 1));
    w.bits(u64::from(lead), 8);
    for i in (0..nbytes - 1).rev() {
        w.bits(0x80 | ((v >> (6 * i)) & 0x3F), 8);
    }
}

/// One subframe of a frame being built.
pub enum Sub {
    /// Type 000000: one signed value repeated `block_size` times.
    Constant { value: i64, wasted: u32 },
    /// Type 000001: every sample verbatim.
    Verbatim { samples: Vec<i64>, wasted: u32 },
    /// Type 001ooo: warmup samples plus a Rice-coded residual.
    Fixed {
        order: u32,
        warmup: Vec<i64>,
        residual: Rice,
    },
}

/// A Rice residual block with per-partition parameters chosen explicitly,
/// so the byte layout is pinned rather than encoder-chosen.
pub struct Rice {
    /// 0 for 4-bit parameters, 1 for 5-bit.
    pub method: u32,
    /// Partition order.
    pub porder: u32,
    /// One entry per partition (`1 << porder` entries).
    pub parts: Vec<Part>,
    /// The residuals, flat across partitions in stream order.
    pub data: Vec<i64>,
}

/// One Rice partition.
pub enum Part {
    /// Normal Rice coding at the given parameter.
    Rice(u32),
    /// Escape: raw `width`-bit signed residuals.
    Escape(u32),
}

/// Zig-zag encode: the FLAC residual map (even = non-negative).
fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

fn write_subframe(w: &mut BitW, sub: &Sub, block_size: u32, eff_bps: u32) {
    w.bit(false); // pad bit
    match sub {
        Sub::Constant { .. } => w.bits(0, 6),
        Sub::Verbatim { .. } => w.bits(1, 6),
        Sub::Fixed { order, .. } => w.bits(u64::from(8 + order), 6),
    }
    let wasted = match sub {
        Sub::Constant { wasted, .. } | Sub::Verbatim { wasted, .. } => *wasted,
        Sub::Fixed { .. } => 0,
    };
    w.bit(wasted > 0);
    if wasted > 0 {
        w.unary(wasted - 1);
    }
    let width = eff_bps - wasted;
    match sub {
        Sub::Constant { value, .. } => w.signed(value >> wasted, width),
        Sub::Verbatim { samples, .. } => {
            assert_eq!(samples.len() as u32, block_size);
            for s in samples {
                w.signed(s >> wasted, width);
            }
        }
        Sub::Fixed {
            order,
            warmup,
            residual,
        } => {
            assert_eq!(warmup.len(), *order as usize);
            for &s in warmup {
                w.signed(s, eff_bps);
            }
            write_rice(w, residual, block_size, *order);
        }
    }
}

fn write_rice(w: &mut BitW, r: &Rice, block_size: u32, order: u32) {
    let parts = 1u32 << r.porder;
    assert_eq!(r.parts.len(), parts as usize);
    assert_eq!(block_size % parts, 0, "partitions must divide the block");
    w.bits(u64::from(r.method), 2);
    w.bits(u64::from(r.porder), 4);
    let pwidth = if r.method == 0 { 4 } else { 5 };
    let part_len = block_size >> r.porder;
    let mut pos = 0usize;
    for (p, part) in r.parts.iter().enumerate() {
        // The decoder's split: partition 0 codes `order` fewer residuals.
        let count = (if p == 0 { part_len - order } else { part_len }) as usize;
        let chunk = &r.data[pos..pos + count];
        pos += count;
        match *part {
            Part::Rice(param) => {
                w.bits(u64::from(param), pwidth);
                for &v in chunk {
                    let u = zigzag(v);
                    w.unary((u >> param) as u32);
                    w.bits(u & ((1u64 << param) - 1), param);
                }
            }
            Part::Escape(width) => {
                w.bits(if r.method == 0 { 15 } else { 31 }, pwidth);
                w.bits(u64::from(width), 5);
                for &v in chunk {
                    w.signed(v, width);
                }
            }
        }
    }
    assert_eq!(pos, r.data.len(), "residual data must be fully consumed");
}

/// Everything a frame needs; the fields map straight onto the FLAC
/// header fields (RFC 9639 section 9.2).
pub struct FrameSpec {
    /// 4-bit block size code.
    pub block_code: u32,
    /// Value for the 8/16-bit block-size extra field (codes 6/7).
    pub block_extra: u32,
    /// 4-bit sample rate code.
    pub rate_code: u32,
    /// 4-bit channel assignment.
    pub chan_code: u32,
    /// 3-bit coded sample size; 0 defers to STREAMINFO.
    pub bps_code: u32,
    /// Coded frame/sample number.
    pub number: u64,
    /// Resolved block size in samples per channel.
    pub block_size: u32,
    /// Bits per sample the subframes are coded at, before the side boost.
    pub bps: u32,
    /// One subframe per channel the map implies.
    pub subs: Vec<Sub>,
}

impl FrameSpec {
    /// A small block via code 6 (8-bit extra), rate and bps deferred to
    /// STREAMINFO, one independent mono channel.
    pub fn mono(block_size: u32, bps: u32, sub: Sub) -> Self {
        FrameSpec {
            block_code: 6,
            block_extra: block_size - 1,
            rate_code: 0,
            chan_code: 0,
            bps_code: 0,
            number: 0,
            block_size,
            bps,
            subs: vec![sub],
        }
    }

    /// A stereo left/side block via channel code 8: channel 0 is left,
    /// channel 1 is the side signal, coded one bit wider.
    pub fn stereo_left_side(block_size: u32, bps: u32, left: Sub, side: Sub) -> Self {
        FrameSpec {
            block_code: 6,
            block_extra: block_size - 1,
            rate_code: 0,
            chan_code: 8,
            bps_code: 0,
            number: 0,
            block_size,
            bps,
            subs: vec![left, side],
        }
    }
}

/// Writes one whole frame including both CRCs.
pub fn frame(spec: &FrameSpec) -> Vec<u8> {
    let mut h = BitW::new();
    h.bits(0x3FFE, 14); // sync
    h.bit(false); // reserved
    h.bit(false); // fixed blocking strategy
    h.bits(u64::from(spec.block_code), 4);
    h.bits(u64::from(spec.rate_code), 4);
    h.bits(u64::from(spec.chan_code), 4);
    h.bits(u64::from(spec.bps_code), 3);
    h.bit(false); // reserved
    coded(&mut h, spec.number);
    match spec.block_code {
        6 => h.bits(u64::from(spec.block_extra), 8),
        7 => h.bits(u64::from(spec.block_extra), 16),
        _ => {}
    }
    let mut bytes = h.bytes();
    bytes.push(crc8(&bytes));

    // Subframes: the side channel, if any, is coded at bps + 1.
    let side = match spec.chan_code {
        8 | 10 => Some(1usize),
        9 => Some(0usize),
        _ => None,
    };
    let mut s = BitW::new();
    for (i, sub) in spec.subs.iter().enumerate() {
        let boost = u32::from(side == Some(i));
        write_subframe(&mut s, sub, spec.block_size, spec.bps + boost);
    }
    s.pad();
    bytes.extend(s.bytes());
    let crc = crc16(&bytes);
    bytes.push((crc >> 8) as u8);
    bytes.push(crc as u8);
    bytes
}

/// A complete stream: signature, STREAMINFO, then frames.
pub fn stream(si: &[u8], frames: &[u8]) -> Vec<u8> {
    let mut v = b"fLaC".to_vec();
    v.extend_from_slice(si);
    v.extend_from_slice(frames);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The standard known-answer values for the FLAC CRC parameters
    /// (CRC-8/SMBUS and CRC-16/BUYPASS).
    #[test]
    fn crc_known_answers() {
        assert_eq!(crc8(b"123456789"), 0xF4);
        assert_eq!(crc8(&[]), 0);
        assert_eq!(crc16(b"123456789"), 0xFEE8);
        assert_eq!(crc16(&[]), 0);
    }

    #[test]
    fn bitw_packs_msb_first_across_bytes() {
        let mut w = BitW::new();
        w.bits(0b101, 3);
        w.bits(0b11_1111_1111, 10);
        w.pad();
        // 3 + 10 bits = 13 bits: 101 1111111111, then zero pad to byte 2.
        assert_eq!(w.bytes(), vec![0b1011_1111, 0b1111_1000]);
    }

    #[test]
    fn bitw_signed_two_complement() {
        let mut w = BitW::new();
        w.signed(-1, 8);
        w.pad();
        assert_eq!(w.bytes(), vec![0xFF]);
        let mut w = BitW::new();
        w.signed(-2, 3);
        w.pad();
        assert_eq!(w.bytes(), vec![0b110_00000]);
    }

    #[test]
    fn streaminfo_block_is_38_bytes() {
        let si = streaminfo_block(44_100, 2, 24, 123_456_789, true);
        assert_eq!(si.len(), 4 + 34);
        // Header: last flag + type 0 + length 34.
        assert_eq!(&si[..4], &[0x80, 0x00, 0x00, 0x22]);
    }

    #[test]
    fn coded_number_byte_counts() {
        // An n-byte coding carries 6n-1 significant bits, so the
        // thresholds sit at 2^7, 2^11, 2^17, 2^23, 2^29, 2^35.
        for (v, expect) in [
            (0x7Fu64, 1usize),
            (0x80, 2),
            (2_048, 3),
            (1 << 17, 4),
            (1 << 23, 5),
            (1 << 29, 6),
            (1 << 35, 7),
        ] {
            let mut w = BitW::new();
            coded(&mut w, v);
            w.pad();
            assert_eq!(w.bytes().len(), expect, "value {v}");
        }
    }

    #[test]
    fn frame_carries_valid_crcs_and_sync() {
        let f = frame(&FrameSpec::mono(
            4,
            16,
            Sub::Constant {
                value: 1,
                wasted: 0,
            },
        ));
        assert_eq!(&f[..2], &[0xFF, 0xF8]); // sync 0x3FFE in the top 14 bits
        let body = &f[..f.len() - 2];
        assert_eq!(&f[f.len() - 2..], crc16(body).to_be_bytes());
        // Header: 32 packed bits + 1 coded number byte + 1 block-size
        // extra byte, then the CRC-8.
        assert_eq!(f[6], crc8(&body[..6]));
    }

    #[test]
    fn stereo_left_side_sets_channel_code_8() {
        let f = frame(&FrameSpec::stereo_left_side(
            2,
            16,
            Sub::Verbatim {
                samples: vec![0, 0],
                wasted: 0,
            },
            Sub::Verbatim {
                samples: vec![0, 0],
                wasted: 0,
            },
        ));
        // Header bits 24-27 are the 4-bit channel assignment; code 8
        // lands in the high nibble of byte 3.
        assert_eq!(f[3] & 0xF0, 0x80);
    }
}
