//! Regenerates and verifies `reference.json`, the hex-exact cross-SDK
//! test vectors for `pith-flac`.
//!
//! Every vector is computed through the crate's public decode API from
//! inputs that are either the committed conformance fixture
//! (`tests/fixtures/tone.flac`) or synthetic streams built bit-by-bit in
//! [`builder`] — no RNG, no time, no platform-dependent bytes — so the
//! output is byte-stable everywhere.
//!
//! Usage:
//! - `gen-reference gen` — recompute every vector and write
//!   `reference.json` at the repository root.
//! - `gen-reference verify` — recompute and compare byte-for-byte
//!   against the committed copy; exit 1 on drift. This is the CI gate.

mod builder;

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use pith_digest::{fnv1a64, sha256};
use pith_flac::{Limits, decode, decode_streaminfo};

use builder::{FrameSpec, Part, Rice, Sub, frame, stream, streaminfo_block};

/// Where the committed copy lives, relative to the repository root.
const REFERENCE_PATH: &str = "reference.json";
/// The decode-path conformance fixture, relative to the repository root.
const FIXTURE_PATH: &str = "tests/fixtures/tone.flac";

/// One decoded-stream measurement: the fields reference.json records for
/// every success vector.
struct Decoded {
    sample_rate: u32,
    channels: u16,
    bits_per_sample: u16,
    total_samples: u64,
    decoded_samples: usize,
    /// The decoded samples, little-endian `i32`, for small vectors.
    samples: Vec<i32>,
    pcm_i32_le_sha256: String,
    pcm_i32_le_fnv1a64: String,
}

/// Measures one stream through the public decode API.
fn measure(input: &[u8]) -> Decoded {
    let out = decode(input, &Limits::default()).expect("vector input must decode");
    Decoded {
        sample_rate: out.sample_rate(),
        channels: out.channels(),
        bits_per_sample: out.bits_per_sample(),
        total_samples: out.total_samples(),
        decoded_samples: out.samples().len(),
        samples: out.samples().to_vec(),
        pcm_i32_le_sha256: hex(sha256(&pcm_le(out.samples()))
            .expect("sha256 of pcm")
            .as_bytes()),
        pcm_i32_le_fnv1a64: format!("{:016x}", fnv1a64(&pcm_le(out.samples()))),
    }
}

fn pcm_le(samples: &[i32]) -> Vec<u8> {
    let mut v = Vec::with_capacity(samples.len() * 4);
    for &s in samples {
        v.extend_from_slice(&s.to_le_bytes());
    }
    v
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// The stable kind name an [`pith_digest::Error`] variant records as.
fn error_kind(e: &pith_digest::Error) -> &'static str {
    match e {
        pith_digest::Error::Truncated { .. } => "truncated",
        pith_digest::Error::InvalidMagic { .. } => "invalid-magic",
        pith_digest::Error::BadValue(_) => "bad-value",
        pith_digest::Error::Unsupported(_) => "unsupported",
        pith_digest::Error::TooLarge { .. } => "too-large",
    }
}

// ------------------------------------------------------------------
// The vector inputs
// ------------------------------------------------------------------

/// 8 samples of one repeated value, mono 16-bit 44100 Hz.
fn constant_mono_stream() -> Vec<u8> {
    let si = streaminfo_block(44_100, 1, 16, 8, true);
    let f = frame(&FrameSpec::mono(
        8,
        16,
        Sub::Constant {
            value: 0x1234,
            wasted: 0,
        },
    ));
    stream(&si, &f)
}

/// Small signed pattern, mono 16-bit, verbatim subframe.
fn verbatim_mono_stream() -> Vec<u8> {
    let si = streaminfo_block(44_100, 1, 16, 8, true);
    let f = frame(&FrameSpec::mono(
        8,
        16,
        Sub::Verbatim {
            samples: vec![1, -2, 3, -4, 5, -6, 7, -8],
            wasted: 0,
        },
    ));
    stream(&si, &f)
}

/// Fixed predictor of order 1, Rice 4-bit parameters, two partitions
/// with different parameters so the partition read order is pinned.
fn fixed1_rice4_stream() -> Vec<u8> {
    let si = streaminfo_block(44_100, 1, 16, 8, true);
    let f = frame(&FrameSpec::mono(
        8,
        16,
        Sub::Fixed {
            order: 1,
            warmup: vec![500],
            residual: Rice {
                method: 0,
                porder: 1,
                parts: vec![Part::Rice(4), Part::Rice(0)],
                data: vec![-1000, 800, 20, 20, 20, 20, 20],
            },
        },
    ));
    stream(&si, &f)
}

/// Fixed predictor of order 0 with an escaped partition carrying raw
/// 5-bit residuals, exercising both partition kinds in one block.
fn fixed0_escape_stream() -> Vec<u8> {
    let si = streaminfo_block(44_100, 1, 16, 8, true);
    let f = frame(&FrameSpec::mono(
        4,
        16,
        Sub::Fixed {
            order: 0,
            warmup: vec![],
            residual: Rice {
                method: 0,
                porder: 1,
                parts: vec![Part::Escape(5), Part::Rice(0)],
                data: vec![-15, 15, 0, 1],
            },
        },
    ));
    stream(&si, &f)
}

/// Stereo left/side decorrelation: channel 0 is left at 16 bits,
/// channel 1 is the side signal (left - right) at 17 bits.
fn stereo_left_side_stream() -> Vec<u8> {
    let left = [100i64, -100, 100, -100];
    let right = [40i64, 40, -40, -40];
    let side: Vec<i64> = left.iter().zip(right.iter()).map(|(l, r)| l - r).collect();
    let si = streaminfo_block(44_100, 2, 16, 4, true);
    let f = frame(&FrameSpec::stereo_left_side(
        4,
        16,
        Sub::Verbatim {
            samples: left.to_vec(),
            wasted: 0,
        },
        Sub::Verbatim {
            samples: side,
            wasted: 0,
        },
    ));
    stream(&si, &f)
}

// ------------------------------------------------------------------
// JSON output (hand-rolled: the suite has no third-party crates)
// ------------------------------------------------------------------

/// Serializes one success vector. `name`/`input` fields differ per kind,
/// so the caller passes them pre-formed.
fn json_vector(name: &str, input_fields: &str, d: &Decoded, with_sample_array: bool) -> String {
    let mut s = String::new();
    s.push_str("    {\n");
    s.push_str(&format!("      \"name\": \"{name}\",\n"));
    s.push_str(input_fields);
    s.push_str(&format!("      \"sample_rate\": {},\n", d.sample_rate));
    s.push_str(&format!("      \"channels\": {},\n", d.channels));
    s.push_str(&format!(
        "      \"bits_per_sample\": {},\n",
        d.bits_per_sample
    ));
    s.push_str(&format!("      \"total_samples\": {},\n", d.total_samples));
    s.push_str(&format!(
        "      \"decoded_samples\": {},\n",
        d.decoded_samples
    ));
    if with_sample_array {
        let list: Vec<String> = d.samples.iter().map(|v| v.to_string()).collect();
        s.push_str(&format!("      \"samples_i32\": [{}],\n", list.join(", ")));
    }
    s.push_str(&format!(
        "      \"pcm_i32_le_sha256\": \"{}\",\n",
        d.pcm_i32_le_sha256
    ));
    s.push_str(&format!(
        "      \"pcm_i32_le_fnv1a64\": \"{}\"\n",
        d.pcm_i32_le_fnv1a64
    ));
    s.push_str("    }");
    s
}

/// Builds the whole reference.json text.
fn reference_json(fixture_bytes: &[u8]) -> String {
    let si = decode_streaminfo(fixture_bytes).expect("fixture must parse");
    let fixture = measure(fixture_bytes);
    let fixture_fields = format!(
        "      \"input_kind\": \"fixture-file\",\n      \"input_path\": \"{FIXTURE_PATH}\",\n      \"input_sha256\": \"{}\",\n      \"streaminfo_min_block_size\": {},\n      \"streaminfo_max_block_size\": {},\n      \"streaminfo_sample_rate\": {},\n      \"streaminfo_channels\": {},\n      \"streaminfo_bits_per_sample\": {},\n      \"streaminfo_total_samples\": {},\n",
        hex(sha256(fixture_bytes).expect("sha256 of fixture").as_bytes()),
        si.min_block_size,
        si.max_block_size,
        si.sample_rate,
        si.channels,
        si.bits_per_sample,
        si.total_samples
    );
    let vectors = [
        ("fixture-tone", fixture_fields, &fixture, false),
        (
            "constant-mono-16bit",
            format!(
                "      \"input_kind\": \"inline-hex\",\n      \"input_hex\": \"{}\",\n",
                hex(&constant_mono_stream())
            ),
            &measure(&constant_mono_stream()),
            true,
        ),
        (
            "verbatim-mono-signs",
            format!(
                "      \"input_kind\": \"inline-hex\",\n      \"input_hex\": \"{}\",\n",
                hex(&verbatim_mono_stream())
            ),
            &measure(&verbatim_mono_stream()),
            true,
        ),
        (
            "fixed1-rice4-partitioned",
            format!(
                "      \"input_kind\": \"inline-hex\",\n      \"input_hex\": \"{}\",\n",
                hex(&fixed1_rice4_stream())
            ),
            &measure(&fixed1_rice4_stream()),
            true,
        ),
        (
            "fixed0-rice-escape",
            format!(
                "      \"input_kind\": \"inline-hex\",\n      \"input_hex\": \"{}\",\n",
                hex(&fixed0_escape_stream())
            ),
            &measure(&fixed0_escape_stream()),
            true,
        ),
        (
            "stereo-left-side",
            format!(
                "      \"input_kind\": \"inline-hex\",\n      \"input_hex\": \"{}\",\n",
                hex(&stereo_left_side_stream())
            ),
            &measure(&stereo_left_side_stream()),
            true,
        ),
    ];

    // Error behavior is part of the cross-SDK contract: the kind name is
    // stable, the message text is not depended on. The kinds below are
    // computed here so a decoder change that shifts a kind fails loudly
    // instead of silently drifting the file.
    let magic_err = decode(b"nope", &Limits::default()).expect_err("bad magic must error");
    let short_err = decode(b"fLaC", &Limits::default()).expect_err("headerless stream must error");
    let errors = [
        ("not-a-flac-stream", "6e6f7065", error_kind(&magic_err)),
        ("headerless-stream", "664c6143", error_kind(&short_err)),
    ];

    let mut s = String::new();
    s.push_str("{\n");
    s.push_str("  \"suite\": \"pith\",\n");
    s.push_str("  \"crate\": \"pith-flac\",\n");
    s.push_str("  \"format_version\": 1,\n");
    s.push_str("  \"generator\": \"cargo run --bin gen-reference -- gen\",\n");
    s.push_str("  \"vectors\": [\n");
    for (i, (name, input, d, arr)) in vectors.iter().enumerate() {
        s.push_str(&json_vector(name, input, d, *arr));
        s.push_str(if i + 1 == vectors.len() { "\n" } else { ",\n" });
    }
    s.push_str("  ],\n");
    s.push_str("  \"error_vectors\": [\n");
    for (i, (name, input_hex, kind)) in errors.iter().enumerate() {
        s.push_str("    {\n");
        s.push_str(&format!("      \"name\": \"{name}\",\n"));
        s.push_str("      \"input_kind\": \"inline-hex\",\n");
        s.push_str(&format!("      \"input_hex\": \"{input_hex}\",\n"));
        s.push_str(&format!("      \"error\": \"{kind}\"\n"));
        s.push_str(if i + 1 == errors.len() {
            "    }\n"
        } else {
            "    },\n"
        });
    }
    s.push_str("  ]\n");
    s.push_str("}\n");
    s
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture_bytes() -> Vec<u8> {
    fs::read(repo_root().join(FIXTURE_PATH))
        .unwrap_or_else(|e| panic!("cannot read {FIXTURE_PATH}: {e}"))
}

fn main() -> ExitCode {
    run(std::env::args().nth(1).unwrap_or_default().as_str())
}

/// One CLI invocation, split out of [`main`] so the mode dispatch is
/// unit-testable.
fn run(mode: &str) -> ExitCode {
    match mode {
        "gen" => {
            let json = reference_json(&fixture_bytes());
            let path = repo_root().join(REFERENCE_PATH);
            fs::write(&path, &json).unwrap_or_else(|e| panic!("cannot write {path:?}: {e}"));
            println!("wrote {} ({} bytes)", path.display(), json.len());
            ExitCode::SUCCESS
        }
        "verify" => {
            let json = reference_json(&fixture_bytes());
            let path = repo_root().join(REFERENCE_PATH);
            let committed = match fs::read(&path) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("FAIL: cannot read {path:?}: {e}");
                    return ExitCode::FAILURE;
                }
            };
            if committed == json.as_bytes() {
                println!("reference.json is current");
                ExitCode::SUCCESS
            } else {
                let off = committed
                    .iter()
                    .zip(json.as_bytes())
                    .position(|(a, b)| a != b)
                    .unwrap_or(committed.len().min(json.len()));
                eprintln!(
                    "FAIL: reference.json is stale: committed {} bytes, computed {} bytes, first difference at byte {off}",
                    committed.len(),
                    json.len()
                );
                ExitCode::FAILURE
            }
        }
        _ => {
            eprintln!("usage: gen-reference <gen|verify> (got {mode:?})");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixture's ground-truth values, measured against the upstream
    /// decoder and pinned here so a port regression cannot quietly
    /// rewrite them.
    const FIXTURE_SHA256: &str = "f954b800b5476f06529b365ed429da96fbf9961067073e955c1966d5fa098d9e";
    const FIXTURE_PCM_SHA256: &str =
        "2514eff527051953533d999b57bc3745386432e2a37dcc32ccab77fb67faee6f";
    const FIXTURE_PCM_FNV1A64: u64 = 0x3a11_60e7_7c61_f2b9;

    #[test]
    fn fixture_measures_reference_values() {
        let d = measure(&fixture_bytes());
        assert_eq!(d.sample_rate, 44_100);
        assert_eq!(d.channels, 1);
        assert_eq!(d.bits_per_sample, 16);
        assert_eq!(d.total_samples, 44_100);
        assert_eq!(d.decoded_samples, 44_100);
        assert_eq!(d.pcm_i32_le_sha256, FIXTURE_PCM_SHA256);
        assert_eq!(d.pcm_i32_le_fnv1a64, format!("{FIXTURE_PCM_FNV1A64:016x}"));
    }

    #[test]
    fn json_carries_fixture_identity_and_kinds() {
        let json = reference_json(&fixture_bytes());
        assert!(json.contains("\"suite\": \"pith\""));
        assert!(json.contains(FIXTURE_SHA256));
        assert!(json.contains(FIXTURE_PCM_SHA256));
        // Every error kind the generator computes is one the mapping
        // knows; the two inputs cover invalid-magic and truncated.
        assert!(json.contains("\"error\": \"invalid-magic\""));
        assert!(json.contains("\"error\": \"truncated\""));
    }

    #[test]
    fn constant_vector_decodes_its_value() {
        let d = measure(&constant_mono_stream());
        assert_eq!(d.samples, vec![0x1234; 8]);
        assert_eq!(d.decoded_samples, 8);
    }

    #[test]
    fn hex_encodes_lowercase_bytes() {
        assert_eq!(hex(&[0x00, 0x0f, 0xf0, 0xff]), "000ff0ff");
    }

    #[test]
    fn error_kinds_map_one_to_one() {
        assert_eq!(
            error_kind(&pith_digest::Error::InvalidMagic { what: "x" }),
            "invalid-magic"
        );
        assert_eq!(
            error_kind(&pith_digest::Error::Truncated {
                what: "x",
                needed: 1,
                found: 0
            }),
            "truncated"
        );
        assert_eq!(error_kind(&pith_digest::Error::BadValue("x")), "bad-value");
        assert_eq!(
            error_kind(&pith_digest::Error::Unsupported("x")),
            "unsupported"
        );
        assert_eq!(
            error_kind(&pith_digest::Error::TooLarge {
                what: "x",
                limit: 1
            }),
            "too-large"
        );
    }

    #[test]
    fn gen_rewrites_the_committed_file() {
        assert_eq!(run("gen"), ExitCode::SUCCESS);
        // Determinism: the regenerated copy must equal what gen produced
        // a moment before, so verify mode stays green.
        assert_eq!(run("verify"), ExitCode::SUCCESS);
    }

    #[test]
    fn unknown_mode_exits_two() {
        assert_eq!(run("bogus"), ExitCode::from(2));
        assert_eq!(run(""), ExitCode::from(2));
    }
}
