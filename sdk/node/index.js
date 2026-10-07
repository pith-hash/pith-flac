// SPDX-License-Identifier: MIT
// Copyright (c) 2026 pith-hash
"use strict";

/**
 * pith-flac SDK: FLAC decoding through koffi.
 *
 * Decodes a native FLAC stream (STREAMINFO, constant/verbatim
 * subframes, fixed predictors of orders 0-4, both Rice methods) into
 * the canonical byte stream the `reference.json` vectors are defined
 * over: a 64-byte big-endian header followed by the decoded PCM,
 * interleaved `i32` little-endian.
 *
 * The cdylib is located through the suite's discovery chain:
 *
 *  1. `PITH_CDYLIB` — an explicit cdylib *file* path;
 *  2. `PITH_CDYLIB_DIR` — a *directory* scanned for the cdylib names
 *     (the CD pipeline points this at `target/release`);
 *  3. `prebuilds/` — the packaged npm layout the CD publish job
 *     assembles, flat and per `<os-arch>` (e.g. `linux-x64`);
 *  4. `<repo root>/target/release` — the repository working-tree
 *     layout, so a source checkout runs against a local cargo build
 *     with no configuration.
 *
 * The FFI surface is one decode operation plus one free:
 * `pith_flac_decode` decodes a whole FLAC stream into the canonical
 * byte stream, and `pith_flac_free` releases the handed-out buffer.
 */

const koffi = require("koffi");
const fs = require("node:fs");
const path = require("node:path");

const STATUS_OK = 0;
const STATUS_INVALID = -1;
const STATUS_REJECTED = -2;

/** Refusal kind "bad-value" (Error::BadValue). */
const ERR_BAD_VALUE = 1;
/** Refusal kind "invalid-magic" (Error::InvalidMagic). */
const ERR_INVALID_MAGIC = 2;
/** Refusal kind "too-large" (Error::TooLarge). */
const ERR_TOO_LARGE = 3;
/** Refusal kind "truncated" (Error::Truncated). */
const ERR_TRUNCATED = 4;
/** Refusal kind "unsupported" (Error::Unsupported). */
const ERR_UNSUPPORTED = 5;

/** The canonical stream's header length in bytes. */
const HEADER_LEN = 64;

/** Refusal-kind names by code — the stable names reference.json
 * `error_vectors` record as strings. */
const ERR_KIND_NAMES = Object.freeze({
  [ERR_BAD_VALUE]: "bad-value",
  [ERR_INVALID_MAGIC]: "invalid-magic",
  [ERR_TOO_LARGE]: "too-large",
  [ERR_TRUNCATED]: "truncated",
  [ERR_UNSUPPORTED]: "unsupported",
});

/** Every cdylib file name cargo may drop into the build directory, per platform. */
const CDYLIB_NAMES = ["pith_flac.dll", "libpith_flac.so", "libpith_flac.dylib"];

const PKG_ROOT = path.join(__dirname);
const REPO_ROOT = path.resolve(__dirname, "..", "..");

/** FfiError: a non-zero status code came back from the cdylib. */
class FfiError extends Error {
  /**
   * @param {string} op the FFI operation name
   * @param {number} status the raw status code
   * @param {number} kind the refusal kind code (0 unless rejected)
   */
  constructor(op, status, kind = 0) {
    let kinded = { [STATUS_INVALID]: "invalid argument", [STATUS_REJECTED]: "input rejected" }[status] ?? "unknown failure";
    if (kind) kinded += ` (${ERR_KIND_NAMES[kind] ?? `kind ${kind}`})`;
    super(`${op} failed: ${kinded} (status ${status})`);
    this.name = "FfiError";
    /** The raw status code the FFI returned. */
    this.status = status;
    /** The refusal kind code (0 unless status is STATUS_REJECTED). */
    this.kind = kind;
  }
}

/**
 * Locates the cdylib through the suite's discovery chain.
 * @returns {string} an absolute path to the cdylib file
 * @throws {Error} when nothing is found
 */
function findCdylib() {
  const explicit = process.env.PITH_CDYLIB;
  if (explicit && fs.statSync(explicit, { throwIfNoEntry: false })?.isFile()) {
    return path.resolve(explicit);
  }
  /** @type {string[]} */
  const dirs = [];
  const envDir = process.env.PITH_CDYLIB_DIR;
  if (envDir) {
    dirs.push(envDir);
    if (!path.isAbsolute(envDir)) {
      dirs.push(path.join(REPO_ROOT, envDir));
    }
  }
  const osArch = `${process.platform}-${process.arch}`;
  dirs.push(path.join(PKG_ROOT, "prebuilds", osArch));
  dirs.push(path.join(PKG_ROOT, "prebuilds"));
  dirs.push(path.join(REPO_ROOT, "target", "release"));
  for (const dir of dirs) {
    for (const name of CDYLIB_NAMES) {
      const p = path.join(dir, name);
      if (fs.statSync(p, { throwIfNoEntry: false })?.isFile()) return p;
    }
  }
  throw new Error(
    "no pith-flac cdylib found (searched PITH_CDYLIB, PITH_CDYLIB_DIR, prebuilds/ and <repo>/target/release); " +
      "run `cargo build --release` first",
  );
}

let cached = undefined;

/**
 * Loads the cdylib and binds the exported symbols (lazily, once).
 * @returns {{decode: Function, free: Function}}
 */
function loadLibrary() {
  if (cached) return cached;
  const lib = koffi.load(findCdylib());
  const decode = lib.func("pith_flac_decode", "int32_t", [
    "const uint8_t *",
    "size_t",
    koffi.out(koffi.pointer("void *")),
    koffi.out(koffi.pointer("size_t")),
    koffi.out(koffi.pointer("int32_t")),
  ]);
  const free = lib.func("void pith_flac_free(void *ptr, size_t len)");
  cached = { decode, free };
  return cached;
}

/**
 * Decodes a complete FLAC stream into the canonical byte stream the
 * `reference.json` vectors are defined over. The handed-out cdylib
 * buffer is copied into a JS Buffer and released before returning.
 *
 * @param {Buffer} data the complete FLAC file bytes
 * @returns {Buffer} the canonical stream (64-byte header + PCM)
 * @throws {FfiError} with `status === -2` for any malformed input
 */
function decodeCanonical(data) {
  if (!Buffer.isBuffer(data)) {
    throw new TypeError("data must be a Buffer");
  }
  const { decode, free } = loadLibrary();
  const out = [null];
  const outLen = [0];
  const err = [0];
  const status = decode(data, data.length, out, outLen, err);
  if (status !== STATUS_OK) {
    throw new FfiError("pith_flac_decode", status, err[0]);
  }
  try {
    // koffi.decode hands back a Uint8Array view over the external
    // buffer; copy it into a Buffer before the cdylib buffer is freed.
    return Buffer.from(koffi.decode(out[0], "uint8_t", Number(outLen[0])));
  } finally {
    free(out[0], Number(outLen[0]));
  }
}

/**
 * Re-expresses the canonical byte stream as a plain object.
 *
 * @param {Buffer} raw the canonical stream
 * @returns {{siMinBlockSize: number, siMaxBlockSize: number, siSampleRate: number,
 *   siChannels: number, siBitsPerSample: number, siTotalSamples: number,
 *   sampleRate: number, channels: number, bitsPerSample: number,
 *   totalSamples: number, decodedSamples: number, pcm: Buffer, raw: Buffer}}
 */
function parseCanonical(raw) {
  if (!Buffer.isBuffer(raw) || raw.length < HEADER_LEN) {
    throw new TypeError("canonical stream is shorter than the 64-byte header");
  }
  return {
    siMinBlockSize: raw.readUInt32BE(0),
    siMaxBlockSize: raw.readUInt32BE(4),
    siSampleRate: raw.readUInt32BE(8),
    siChannels: raw.readUInt32BE(12),
    siBitsPerSample: raw.readUInt32BE(16),
    siTotalSamples: Number(raw.readBigUInt64BE(20)),
    sampleRate: raw.readUInt32BE(28),
    channels: raw.readUInt32BE(32),
    bitsPerSample: raw.readUInt32BE(36),
    totalSamples: Number(raw.readBigUInt64BE(40)),
    decodedSamples: Number(raw.readBigUInt64BE(48)),
    pcm: raw.subarray(HEADER_LEN),
    raw,
  };
}

module.exports = {
  STATUS_OK,
  STATUS_INVALID,
  STATUS_REJECTED,
  ERR_BAD_VALUE,
  ERR_INVALID_MAGIC,
  ERR_TOO_LARGE,
  ERR_TRUNCATED,
  ERR_UNSUPPORTED,
  HEADER_LEN,
  ERR_KIND_NAMES,
  CDYLIB_NAMES,
  FfiError,
  findCdylib,
  decodeCanonical,
  parseCanonical,
};
