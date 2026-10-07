// SPDX-License-Identifier: MIT
// Copyright (c) 2026 pith-hash
"use strict";

// Hex-exact conformance: the committed reference vectors through koffi.
// Every vector in the repository-root reference.json is replayed through
// the cdylib and compared byte-exact — the PCM section's SHA-256 against
// pcm_i32_le_sha256, the decoded samples_i32 array, the STREAMINFO facts
// and the recorded refusal kinds. The same vectors the Rust
// gen-reference verify gate and the Python/Go SDKs check.

const test = require("node:test");
const assert = require("node:assert/strict");
const crypto = require("node:crypto");
const fs = require("node:fs");
const path = require("node:path");

const {
  ERR_INVALID_MAGIC,
  ERR_TRUNCATED,
  FfiError,
  decodeCanonical,
  findCdylib,
  parseCanonical,
} = require("../index.js");

const REPO_ROOT = path.resolve(__dirname, "..", "..", "..");

const REFERENCE = JSON.parse(fs.readFileSync(path.join(REPO_ROOT, "reference.json"), "utf8"));
const VECTORS = REFERENCE.vectors;
const ERROR_VECTORS = REFERENCE.error_vectors;

/** The raw FLAC bytes a vector was computed over: the committed
 * fixture file, or the inline hex. */
function vectorInput(vector) {
  if (vector.input_kind === "fixture-file") {
    return fs.readFileSync(path.join(REPO_ROOT, vector.input_path));
  }
  return Buffer.from(vector.input_hex, "hex");
}

test("cdylib is discoverable", () => {
  assert.ok(fs.statSync(findCdylib()).isFile());
});

for (const vector of VECTORS) {
  test(`reference vector ${vector.name} is reproduced hex-exact`, () => {
    const raw = decodeCanonical(vectorInput(vector));
    const canonical = parseCanonical(raw);

    assert.equal(
      crypto.createHash("sha256").update(canonical.pcm).digest("hex"),
      vector.pcm_i32_le_sha256,
      vector.name,
    );

    assert.equal(canonical.sampleRate, vector.sample_rate, vector.name);
    assert.equal(canonical.channels, vector.channels, vector.name);
    assert.equal(canonical.bitsPerSample, vector.bits_per_sample, vector.name);
    assert.equal(canonical.totalSamples, vector.total_samples, vector.name);
    assert.equal(canonical.decodedSamples, vector.decoded_samples, vector.name);

    if ("samples_i32" in vector) {
      const samples = [];
      for (let i = 0; i < canonical.pcm.length; i += 4) {
        samples.push(canonical.pcm.readInt32LE(i));
      }
      assert.deepEqual(samples, vector.samples_i32, vector.name);
    }

    if ("streaminfo_min_block_size" in vector) {
      assert.equal(canonical.siMinBlockSize, vector.streaminfo_min_block_size, vector.name);
      assert.equal(canonical.siMaxBlockSize, vector.streaminfo_max_block_size, vector.name);
      assert.equal(canonical.siSampleRate, vector.streaminfo_sample_rate, vector.name);
      assert.equal(canonical.siChannels, vector.streaminfo_channels, vector.name);
      assert.equal(canonical.siBitsPerSample, vector.streaminfo_bits_per_sample, vector.name);
      assert.equal(canonical.siTotalSamples, vector.streaminfo_total_samples, vector.name);
    }
  });
}

for (const vector of ERROR_VECTORS) {
  test(`error vector ${vector.name} is refused with the recorded kind`, () => {
    assert.throws(() => decodeCanonical(Buffer.from(vector.input_hex, "hex")), (err) => {
      assert.ok(err instanceof FfiError);
      assert.equal(err.status, -2, vector.name);
      assert.equal(err.kind, vector.error === "invalid-magic" ? ERR_INVALID_MAGIC : ERR_TRUNCATED, vector.name);
      return true;
    });
  });
}

test("malformed input is refused, not crashing", () => {
  assert.throws(() => decodeCanonical(Buffer.from("not a flac stream at all")), (err) => {
    assert.ok(err instanceof FfiError);
    assert.equal(err.status, -2);
    return true;
  });
});

test("empty input is refused", () => {
  assert.throws(() => decodeCanonical(Buffer.alloc(0)), FfiError);
});

test("fixture pcm matches a rust-pinned value", () => {
  // fixture-tone's digest, pinned in the committed reference.json and
  // re-derived by the Rust unit tests; this test fails loudly even if
  // reference.json were regenerated wrongly.
  const data = fs.readFileSync(path.join(REPO_ROOT, "tests", "fixtures", "tone.flac"));
  const raw = decodeCanonical(data);
  const canonical = parseCanonical(raw);
  assert.equal(
    crypto.createHash("sha256").update(canonical.pcm).digest("hex"),
    "2514eff527051953533d999b57bc3745386432e2a37dcc32ccab77fb67faee6f",
  );
  // Header prologue: STREAMINFO min/max block 16/65535, 44100 Hz, mono,
  // 16-bit.
  assert.deepEqual([...raw.subarray(0, 8)], [0, 0, 0, 16, 0, 0, 255, 255]);
});
