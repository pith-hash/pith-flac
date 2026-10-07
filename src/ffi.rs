//! The C ABI surface of `pith-flac`: the entry points the Python
//! (ctypes), Node (koffi) and Go (cgo) SDKs bind through.
//!
//! The suite's FFI convention, defined by the pilot cdylibs and
//! mirrored by every `pith-*` cdylib:
//!
//! * one flat set of `#[unsafe(no_mangle)] pub unsafe extern "C"`
//!   functions — raw pointers plus lengths, no structs across the
//!   boundary;
//! * every function returns a status code (see the constants below),
//!   never a `Result`, never a panic: a `panic = "abort"` cdylib must
//!   not be reachable from a foreign caller;
//! * an operation either hands ownership to the caller (and ships a
//!   matching `_free` — [`pith_flac_free`] here) or writes into
//!   caller-provided out-parameters;
//! * the `unsafe` allowance is confined to this module; every core
//!   module stays unsafe-free behind the crate-root `#![deny]`.
//!
//! Decoding uses the crate's conservative default [`Limits`] — a
//! hashing pipeline never wants an unbounded decode, and the FFI
//! surface is no exception.
//!
//! # Canonical wire format
//!
//! [`pith_flac_decode`] hands the caller the canonical decode-output
//! stream the SDKs consume: a 64-byte big-endian header followed by
//! the decoded PCM, interleaved `i32` little-endian — exactly the byte
//! string `pcm_i32_le_sha256` in `reference.json` covers:
//!
//! | offset | width | field |
//! |-------:|------:|-------|
//! | 0  | u32 | STREAMINFO min block size |
//! | 4  | u32 | STREAMINFO max block size |
//! | 8  | u32 | STREAMINFO sample rate |
//! | 12 | u32 | STREAMINFO channel count |
//! | 16 | u32 | STREAMINFO bits per sample |
//! | 20 | u64 | STREAMINFO total samples |
//! | 28 | u32 | decoded sample rate |
//! | 32 | u32 | decoded channel count |
//! | 36 | u32 | decoded bits per sample |
//! | 40 | u64 | declared total samples |
//! | 48 | u64 | decoded sample count (interleaved) |
//! | 56 | u64 | PCM byte length |
//! | 64 | …   | PCM, interleaved `i32` little-endian |
//!
//! On a refusal the `err` out-parameter carries one of the
//! `PITH_ERR_*` kind codes — the stable names `reference.json`
//! `error_vectors` record ("bad-value", "invalid-magic", ...) as
//! numbers, so an SDK test can assert the exact refusal kind.

#![allow(unsafe_code)]

use alloc::vec::Vec;

use crate::{Limits, decode, decode_streaminfo};
use pith_digest::Error;

/// Status: success.
pub const PITH_OK: i32 = 0;
/// Status: a caller argument is invalid — a null pointer.
pub const PITH_E_INVALID: i32 = -1;
/// Status: the core decoder refused the input (malformed FLAC: bad
/// magic, bad metadata block, CRC mismatch, or a truncated stream).
pub const PITH_E_REJECTED: i32 = -2;

/// Refusal kind: `Error::BadValue` (`"bad-value"` in reference.json).
pub const PITH_ERR_BAD_VALUE: i32 = 1;
/// Refusal kind: `Error::InvalidMagic` (`"invalid-magic"`).
pub const PITH_ERR_INVALID_MAGIC: i32 = 2;
/// Refusal kind: `Error::TooLarge` (`"too-large"`).
pub const PITH_ERR_TOO_LARGE: i32 = 3;
/// Refusal kind: `Error::Truncated` (`"truncated"`).
pub const PITH_ERR_TRUNCATED: i32 = 4;
/// Refusal kind: `Error::Unsupported` (`"unsupported"`).
pub const PITH_ERR_UNSUPPORTED: i32 = 5;

/// The canonical stream's header length in bytes.
pub const PITH_HEADER_LEN: usize = 64;

/// Appends one big-endian `u32` field to the canonical header.
fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_be_bytes());
}

/// Appends one big-endian `u64` field to the canonical header.
fn push_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_be_bytes());
}

/// Maps a decoder error to its stable kind code (alphabetical over the
/// kebab-case names reference.json records).
fn err_kind(e: &Error) -> i32 {
    match e {
        Error::BadValue(_) => PITH_ERR_BAD_VALUE,
        Error::InvalidMagic { .. } => PITH_ERR_INVALID_MAGIC,
        Error::TooLarge { .. } => PITH_ERR_TOO_LARGE,
        Error::Truncated { .. } => PITH_ERR_TRUNCATED,
        Error::Unsupported(_) => PITH_ERR_UNSUPPORTED,
    }
}

/// Hands a serialized canonical stream to the caller: the exact-length
/// buffer goes out as an owned boxed slice; [`pith_flac_free`]
/// reconstructs it from the same length to release it.
unsafe fn hand_out(canonical: Vec<u8>, out: *mut *mut u8, out_len: *mut usize) {
    let len = canonical.len();
    let ptr = alloc::boxed::Box::into_raw(canonical.into_boxed_slice());
    unsafe {
        *out = ptr.cast::<u8>();
        *out_len = len;
    }
}

/// Writes the refusal shape through the caller's out-parameters: a
/// null buffer, zero length and the stable kind code.
unsafe fn refuse(out: *mut *mut u8, out_len: *mut usize, err: *mut i32, kind: i32) {
    unsafe {
        *out = core::ptr::null_mut();
        *out_len = 0;
        *err = kind;
    }
}

/// Decodes a FLAC stream into the canonical byte stream the SDK
/// vectors are defined over.
///
/// `data` points at `len` bytes of the complete FLAC file. On success
/// the function allocates a buffer, writes its address through `out`,
/// its length through `out_len`, a zero kind through `err`, and
/// returns [`PITH_OK`]; the caller owns the buffer and must release it
/// with [`pith_flac_free`], passing back the same pointer *and*
/// length. The buffer layout is the canonical wire format documented
/// on this module.
///
/// On a refusal the function returns [`PITH_E_REJECTED`], writes zero
/// through `out`, and stores one `PITH_ERR_*` kind code through `err`.
///
/// # Safety
///
/// `data` must point to `len` readable bytes; `out` and `out_len` to
/// one writable pointer/`usize` each; `err` to one writable `i32`. All
/// must stay valid for the duration of the call; the function retains
/// nothing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pith_flac_decode(
    data: *const u8,
    len: usize,
    out: *mut *mut u8,
    out_len: *mut usize,
    err: *mut i32,
) -> i32 {
    if data.is_null() || out.is_null() || out_len.is_null() || err.is_null() {
        return PITH_E_INVALID;
    }
    let bytes = unsafe { core::slice::from_raw_parts(data, len) };
    match decode_and_serialize(bytes) {
        Ok(canonical) => {
            unsafe {
                hand_out(canonical, out, out_len);
                *err = PITH_OK;
            }
            PITH_OK
        }
        Err((status, kind)) => {
            unsafe { refuse(out, out_len, err, kind) };
            status
        }
    }
}

/// Releases a buffer handed out by [`pith_flac_decode`].
///
/// # Safety
///
/// `ptr` must be a pointer returned by [`pith_flac_decode`] with the
/// `out_len` value that came back with it, and must not have been
/// released (or otherwise freed) before. Null is accepted and
/// ignored, so callers can free unconditionally on the error path.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pith_flac_free(ptr: *mut u8, len: usize) {
    if ptr.is_null() {
        return;
    }
    let slice = unsafe { core::slice::from_raw_parts_mut(ptr, len) };
    drop(unsafe { alloc::boxed::Box::from_raw(slice) });
}

/// The safe core of [`pith_flac_decode`]: decode, read STREAMINFO,
/// then serialize canonically. Decoding failures map to
/// `(PITH_E_REJECTED, kind)`; the kind is the stable refusal name the
/// reference.json error vectors pin.
fn decode_and_serialize(bytes: &[u8]) -> Result<Vec<u8>, (i32, i32)> {
    let flac = decode(bytes, &Limits::default()).map_err(|e| (PITH_E_REJECTED, err_kind(&e)))?;
    let si = decode_streaminfo(bytes).map_err(|e| (PITH_E_REJECTED, err_kind(&e)))?;
    let samples = flac.samples();
    let mut out = Vec::with_capacity(PITH_HEADER_LEN + samples.len() * 4);
    // STREAMINFO facts (as parsed).
    push_u32(&mut out, u32::from(si.min_block_size));
    push_u32(&mut out, u32::from(si.max_block_size));
    push_u32(&mut out, si.sample_rate);
    push_u32(&mut out, u32::from(si.channels));
    push_u32(&mut out, u32::from(si.bits_per_sample));
    push_u64(&mut out, si.total_samples);
    // Decoded facts (resolved from the frames).
    push_u32(&mut out, flac.sample_rate());
    push_u32(&mut out, u32::from(flac.channels()));
    push_u32(&mut out, u32::from(flac.bits_per_sample()));
    push_u64(&mut out, flac.total_samples());
    push_u64(&mut out, samples.len() as u64);
    push_u64(&mut out, samples.len() as u64 * 4);
    debug_assert_eq!(out.len(), PITH_HEADER_LEN);
    // The PCM section: interleaved i32 little-endian, the exact bytes
    // pcm_i32_le_sha256 is computed over.
    for &s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{
        PITH_E_INVALID, PITH_E_REJECTED, PITH_ERR_INVALID_MAGIC, PITH_ERR_TRUNCATED,
        PITH_HEADER_LEN, PITH_OK, decode_and_serialize, pith_flac_decode, pith_flac_free,
    };
    use pith_digest::sha256;

    /// An inline reference vector's stream, hex-decoded: the
    /// constant-mono-16bit vector input from reference.json.
    const CONSTANT_MONO_HEX: &str = "664c61438000002200c012000000000000000ac440f00000000800000000000000000000000000000000fff860000007ff001234fd6f";
    /// The recorded `pcm_i32_le_sha256` of that vector.
    const CONSTANT_MONO_SHA: &str =
        "8949c8b20154a8ad94f90be09da09819eb3f6e4327b25df4ff0b28b0d8e9295c";

    fn hex_decode(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
            .collect()
    }

    /// The committed conformance fixture, decoded end-to-end through
    /// the raw FFI: status OK, the 64-byte header carries the recorded
    /// STREAMINFO and decoded facts, the PCM section digests to the
    /// recorded sha256, and the buffer round-trips through
    /// `pith_flac_free`.
    #[test]
    fn ffi_decode_reproduces_the_canonical_stream() {
        let path = format!("{}/tests/fixtures/tone.flac", env!("CARGO_MANIFEST_DIR"));
        let flac = std::fs::read(&path).expect("fixture");
        let expected = decode_and_serialize(&flac).expect("decode");

        let mut out: *mut u8 = core::ptr::null_mut();
        let mut out_len: usize = 0;
        let mut err: i32 = -99;
        let status = unsafe {
            pith_flac_decode(flac.as_ptr(), flac.len(), &mut out, &mut out_len, &mut err)
        };
        assert_eq!(status, PITH_OK);
        assert_eq!(err, PITH_OK);
        assert_eq!(out_len, expected.len());
        let handed_back = unsafe { core::slice::from_raw_parts(out, out_len) };
        assert_eq!(handed_back, expected.as_slice());
        // The header: 16/65535 block sizes, 44100 Hz mono 16-bit,
        // 44100 declared and decoded samples, 176400 PCM bytes.
        assert_eq!(
            u32::from_be_bytes(handed_back[0..4].try_into().unwrap()),
            16
        );
        assert_eq!(
            u32::from_be_bytes(handed_back[4..8].try_into().unwrap()),
            65_535
        );
        assert_eq!(
            u32::from_be_bytes(handed_back[8..12].try_into().unwrap()),
            44_100
        );
        assert_eq!(
            u32::from_be_bytes(handed_back[12..16].try_into().unwrap()),
            1
        );
        assert_eq!(
            u32::from_be_bytes(handed_back[16..20].try_into().unwrap()),
            16
        );
        assert_eq!(
            u64::from_be_bytes(handed_back[20..28].try_into().unwrap()),
            44_100
        );
        assert_eq!(
            u64::from_be_bytes(handed_back[48..56].try_into().unwrap()),
            44_100
        );
        assert_eq!(
            u64::from_be_bytes(handed_back[56..64].try_into().unwrap()),
            44_100 * 4
        );
        // The PCM section digests to the vector's recorded sha256.
        let digest = sha256(&handed_back[PITH_HEADER_LEN..]).expect("sha256");
        assert_eq!(
            digest.as_bytes(),
            &hex_decode("2514eff527051953533d999b57bc3745386432e2a37dcc32ccab77fb67faee6f")[..]
        );
        unsafe { pith_flac_free(out, out_len) };
    }

    /// An inline vector replays through the raw FFI and the PCM
    /// section digests to the recorded value.
    #[test]
    fn ffi_inline_vector_matches_the_recorded_digest() {
        let stream = hex_decode(CONSTANT_MONO_HEX);
        let mut out: *mut u8 = core::ptr::null_mut();
        let mut out_len: usize = 0;
        let mut err: i32 = -99;
        let status = unsafe {
            pith_flac_decode(
                stream.as_ptr(),
                stream.len(),
                &mut out,
                &mut out_len,
                &mut err,
            )
        };
        assert_eq!(status, PITH_OK);
        let canonical = unsafe { core::slice::from_raw_parts(out, out_len) };
        let digest = sha256(&canonical[PITH_HEADER_LEN..]).expect("sha256");
        assert_eq!(digest.as_bytes(), &hex_decode(CONSTANT_MONO_SHA)[..]);
        unsafe { pith_flac_free(out, out_len) };
    }

    /// Null pointers are [`PITH_E_INVALID`]; the two reference.json
    /// error vectors are [`PITH_E_REJECTED`] with their recorded kind;
    /// a null buffer is a legal free.
    #[test]
    fn ffi_refusals() {
        let mut out: *mut u8 = core::ptr::null_mut();
        let mut out_len: usize = 0;
        let mut err: i32 = -99;
        let null_data =
            unsafe { pith_flac_decode(core::ptr::null(), 0, &mut out, &mut out_len, &mut err) };
        assert_eq!(null_data, PITH_E_INVALID);

        let stream = [0u8; 16];
        let null_err = unsafe {
            pith_flac_decode(
                stream.as_ptr(),
                stream.len(),
                &mut out,
                &mut out_len,
                core::ptr::null_mut(),
            )
        };
        assert_eq!(null_err, PITH_E_INVALID);

        // not-a-flac-stream: recorded kind "invalid-magic".
        let bad_magic =
            unsafe { pith_flac_decode(b"nope".as_ptr(), 4, &mut out, &mut out_len, &mut err) };
        assert_eq!(bad_magic, PITH_E_REJECTED);
        assert_eq!(err, PITH_ERR_INVALID_MAGIC);
        assert!(out.is_null() && out_len == 0);

        // headerless-stream: recorded kind "truncated".
        let short =
            unsafe { pith_flac_decode(b"fLaC".as_ptr(), 4, &mut out, &mut out_len, &mut err) };
        assert_eq!(short, PITH_E_REJECTED);
        assert_eq!(err, PITH_ERR_TRUNCATED);

        unsafe { pith_flac_free(core::ptr::null_mut(), 0) };
    }

    /// Every decoder error maps to its stable kind code.
    #[test]
    fn err_kinds_map_one_to_one() {
        use super::{
            PITH_ERR_BAD_VALUE, PITH_ERR_INVALID_MAGIC, PITH_ERR_TOO_LARGE, PITH_ERR_TRUNCATED,
            PITH_ERR_UNSUPPORTED, err_kind,
        };
        use pith_digest::Error;
        assert_eq!(err_kind(&Error::BadValue("x")), PITH_ERR_BAD_VALUE);
        assert_eq!(
            err_kind(&Error::InvalidMagic { what: "x" }),
            PITH_ERR_INVALID_MAGIC
        );
        assert_eq!(err_kind(&Error::too_large("x", 0)), PITH_ERR_TOO_LARGE);
        assert_eq!(err_kind(&Error::truncated("x", 1, 0)), PITH_ERR_TRUNCATED);
        assert_eq!(err_kind(&Error::Unsupported("x")), PITH_ERR_UNSUPPORTED);
    }

    /// The safe core rejects malformed input instead of panicking, and
    /// the canonical stream is header + 4 bytes per sample.
    #[test]
    fn safe_core_shape() {
        assert_eq!(
            decode_and_serialize(b"nope"),
            Err((PITH_E_REJECTED, PITH_ERR_INVALID_MAGIC))
        );
        let stream = hex_decode(CONSTANT_MONO_HEX);
        let canonical = decode_and_serialize(&stream).expect("decode");
        assert_eq!(canonical.len(), PITH_HEADER_LEN + 8 * 4);
    }
}
