# SPDX-License-Identifier: MIT
# Copyright (c) 2026 pith-hash
"""pith-flac SDK: FLAC decoding through ctypes.

Decodes a native FLAC stream (STREAMINFO, constant/verbatim subframes,
fixed predictors of orders 0-4, both Rice methods) through the Rust
cdylib, handing back the canonical byte stream the ``reference.json``
vectors are defined over: a 64-byte big-endian header followed by the
decoded PCM, interleaved ``i32`` little-endian.

The cdylib is located through the suite's discovery chain:

1. ``PITH_CDYLIB`` — an explicit cdylib *file* path;
2. ``PITH_CDYLIB_DIR`` — a *directory* scanned for the cdylib names
   (the CD pipeline points this at ``target/release``);
3. the package directory itself (the built wheel ships the cdylib as
   package data);
4. ``<repo root>/target/release`` — the repository working-tree layout,
   so a source checkout runs against a local cargo build with no
   configuration.
"""

from __future__ import annotations

import ctypes
import os
from dataclasses import dataclass
from pathlib import Path

__all__ = [
    "Canonical",
    "FfiError",
    "LibraryNotFoundError",
    "find_cdylib",
    "decode_canonical",
    "parse_canonical",
    "STATUS_OK",
    "STATUS_INVALID",
    "STATUS_REJECTED",
    "ERR_BAD_VALUE",
    "ERR_INVALID_MAGIC",
    "ERR_TOO_LARGE",
    "ERR_TRUNCATED",
    "ERR_UNSUPPORTED",
    "HEADER_LEN",
]

#: Status: success.
STATUS_OK = 0
#: Status: a caller argument is invalid (a null pointer).
STATUS_INVALID = -1
#: Status: the core decoder refused the input (malformed FLAC).
STATUS_REJECTED = -2

#: Refusal kind ``"bad-value"`` (``Error::BadValue``).
ERR_BAD_VALUE = 1
#: Refusal kind ``"invalid-magic"`` (``Error::InvalidMagic``).
ERR_INVALID_MAGIC = 2
#: Refusal kind ``"too-large"`` (``Error::TooLarge``).
ERR_TOO_LARGE = 3
#: Refusal kind ``"truncated"`` (``Error::Truncated``).
ERR_TRUNCATED = 4
#: Refusal kind ``"unsupported"`` (``Error::Unsupported``).
ERR_UNSUPPORTED = 5

#: The canonical stream's header length in bytes.
HEADER_LEN = 64

#: Every cdylib file name cargo may drop into the build directory, per
#: platform (windows / linux / macOS).
CDYLIB_NAMES = ("pith_flac.dll", "libpith_flac.so", "libpith_flac.dylib")

#: Refusal-kind names by code — the same stable names the
#: ``error_vectors`` of ``reference.json`` record as strings.
ERR_KIND_NAMES = {
    ERR_BAD_VALUE: "bad-value",
    ERR_INVALID_MAGIC: "invalid-magic",
    ERR_TOO_LARGE: "too-large",
    ERR_TRUNCATED: "truncated",
    ERR_UNSUPPORTED: "unsupported",
}


@dataclass(frozen=True)
class Canonical:
    """A decoded FLAC, re-expressed from the canonical byte stream.

    ``pcm`` is the interleaved ``i32`` little-endian sample buffer —
    exactly the bytes the ``pcm_i32_le_sha256`` digest covers.
    """

    #: STREAMINFO: smallest block size (samples per channel).
    si_min_block_size: int
    #: STREAMINFO: largest block size (samples per channel).
    si_max_block_size: int
    #: STREAMINFO: sample rate in Hz (0 when the frames carry it).
    si_sample_rate: int
    #: STREAMINFO: channel count, 1-8.
    si_channels: int
    #: STREAMINFO: bits per sample, 4-32.
    si_bits_per_sample: int
    #: STREAMINFO: declared total samples per channel (0 = unknown).
    si_total_samples: int
    #: Decoded sample rate in Hz (resolved from frames when STREAMINFO
    #: said 0).
    sample_rate: int
    #: Decoded channel count, 1-8.
    channels: int
    #: Decoded bits per sample.
    bits_per_sample: int
    #: Declared total samples per channel, 0 when unknown.
    total_samples: int
    #: Decoded sample count, interleaved across channels.
    decoded_samples: int
    #: The canonical byte stream the digest is computed over.
    raw: bytes

    @property
    def pcm(self) -> bytes:
        """The interleaved ``i32`` little-endian PCM (everything after
        the 64-byte header)."""
        return self.raw[HEADER_LEN:]

    @property
    def samples_i32(self) -> list[int]:
        """The PCM as Python ints, in stream order."""
        return list(
            int.from_bytes(self.raw[HEADER_LEN + i : HEADER_LEN + i + 4], "little", signed=True)
            for i in range(0, len(self.raw) - HEADER_LEN, 4)
        )


class LibraryNotFoundError(OSError):
    """No cdylib was found through the discovery chain."""


class FfiError(Exception):
    """A non-zero status code came back from the cdylib."""

    def __init__(self, op: str, status: int, kind: int = 0) -> None:
        detail = {
            STATUS_INVALID: "invalid argument",
            STATUS_REJECTED: "input rejected",
        }.get(status, "unknown failure")
        if kind:
            detail = f"{detail} ({ERR_KIND_NAMES.get(kind, f'kind {kind}')})"
        super().__init__(f"{op} failed: {detail} (status {status})")
        #: The raw status code the FFI returned.
        self.status = status
        #: The refusal kind code (0 unless the status is
        #: :data:`STATUS_REJECTED`; one of the ``ERR_*`` constants).
        self.kind = kind


def find_cdylib() -> Path:
    """Locates the cdylib through the suite's discovery chain."""
    explicit = os.environ.get("PITH_CDYLIB")
    if explicit:
        p = Path(explicit)
        if p.is_file():
            return p
    env_dir = os.environ.get("PITH_CDYLIB_DIR")
    candidates: list[Path] = []
    if env_dir:
        env_dir_path = Path(env_dir)
        candidates.append(env_dir_path)
        if not env_dir_path.is_absolute():
            # CD and local runs invoke tools from the repository root or
            # from sdk/<lang>; resolve the env value against both.
            candidates.append(Path.cwd() / env_dir_path)
            candidates.append(Path(__file__).resolve().parents[3] / env_dir_path)
    candidates.append(Path(__file__).resolve().parent)  # packaged wheel
    candidates.append(Path(__file__).resolve().parents[3] / "target" / "release")
    for directory in candidates:
        for name in CDYLIB_NAMES:
            p = directory / name
            if p.is_file():
                return p
    raise LibraryNotFoundError(
        "no pith-flac cdylib found (searched PITH_CDYLIB, PITH_CDYLIB_DIR, "
        "the package directory and <repo>/target/release); "
        "run `cargo build --release` first"
    )


_lib: ctypes.CDLL | None = None


def _load() -> ctypes.CDLL:
    global _lib
    if _lib is None:
        lib = ctypes.CDLL(str(find_cdylib()))
        lib.pith_flac_decode.argtypes = [
            ctypes.c_void_p,  # data
            ctypes.c_size_t,  # len
            ctypes.POINTER(ctypes.c_void_p),  # out buffer
            ctypes.POINTER(ctypes.c_size_t),  # out length
            ctypes.POINTER(ctypes.c_int32),  # out refusal kind
        ]
        lib.pith_flac_decode.restype = ctypes.c_int32
        lib.pith_flac_free.argtypes = [ctypes.c_void_p, ctypes.c_size_t]
        lib.pith_flac_free.restype = None
        _lib = lib
    return _lib


def decode_canonical(data: bytes) -> bytes:
    """Decodes a complete FLAC stream into the canonical byte stream
    the ``reference.json`` vectors are defined over.

    Raises :class:`FfiError` with ``status == STATUS_REJECTED`` for any
    malformed input; ``err.kind`` then carries one of the ``ERR_*``
    kind codes the reference.json error vectors pin. The decoder never
    panics through this boundary.
    """
    out = ctypes.c_void_p()
    out_len = ctypes.c_size_t()
    err = ctypes.c_int32()
    status = _load().pith_flac_decode(
        data, len(data), ctypes.byref(out), ctypes.byref(out_len), ctypes.byref(err)
    )
    if status != STATUS_OK:
        raise FfiError("pith_flac_decode", status, err.value)
    try:
        return ctypes.string_at(out, out_len.value)
    finally:
        _load().pith_flac_free(out, out_len.value)


def parse_canonical(raw: bytes) -> Canonical:
    """Re-expresses the canonical byte stream as a :class:`Canonical`."""
    if len(raw) < HEADER_LEN:
        raise ValueError("canonical stream is shorter than the 64-byte header")

    def u32(offset: int) -> int:
        return int.from_bytes(raw[offset : offset + 4], "big")

    def u64(offset: int) -> int:
        return int.from_bytes(raw[offset : offset + 8], "big")

    return Canonical(
        si_min_block_size=u32(0),
        si_max_block_size=u32(4),
        si_sample_rate=u32(8),
        si_channels=u32(12),
        si_bits_per_sample=u32(16),
        si_total_samples=u64(20),
        sample_rate=u32(28),
        channels=u32(32),
        bits_per_sample=u32(36),
        total_samples=u64(40),
        decoded_samples=u64(48),
        raw=raw,
    )
