# SPDX-License-Identifier: MIT
# Copyright (c) 2026 pith-hash
"""Hex-exact conformance: the committed reference vectors through ctypes.

Every vector in the repository-root ``reference.json`` is replayed
through the cdylib and compared byte-exact — the PCM section's SHA-256
against ``pcm_i32_le_sha256``, the decoded ``samples_i32`` array, the
STREAMINFO facts (fixture vector) and the recorded refusal kinds. The
same vectors the Rust ``gen-reference verify`` gate and the Node/Go
SDKs check.
"""

from __future__ import annotations

import hashlib
import json
from pathlib import Path

import pytest

from pith_flac import (
    ERR_INVALID_MAGIC,
    ERR_TRUNCATED,
    FfiError,
    decode_canonical,
    find_cdylib,
    parse_canonical,
)

REPO_ROOT = Path(__file__).resolve().parents[3]
REFERENCE = json.loads((REPO_ROOT / "reference.json").read_text(encoding="utf-8"))
VECTORS = REFERENCE["vectors"]
ERROR_VECTORS = REFERENCE["error_vectors"]


def vector_input(vector: dict) -> bytes:
    """The raw FLAC bytes a vector was computed over: the committed
    fixture file, or the inline hex."""
    if vector["input_kind"] == "fixture-file":
        return (REPO_ROOT / vector["input_path"]).read_bytes()
    return bytes.fromhex(vector["input_hex"])


def test_cdylib_is_discoverable() -> None:
    path = find_cdylib()
    assert path.is_file(), path


@pytest.mark.parametrize("vector", VECTORS, ids=lambda v: v["name"])
def test_reference_vector_is_reproduced_hex_exact(vector: dict) -> None:
    raw = decode_canonical(vector_input(vector))
    canonical = parse_canonical(raw)

    # The PCM section digests to the recorded sha256, byte-exact.
    assert hashlib.sha256(canonical.pcm).hexdigest() == vector["pcm_i32_le_sha256"], vector["name"]

    # Every decoded fact the vector records.
    assert canonical.sample_rate == vector["sample_rate"], vector["name"]
    assert canonical.channels == vector["channels"], vector["name"]
    assert canonical.bits_per_sample == vector["bits_per_sample"], vector["name"]
    assert canonical.total_samples == vector["total_samples"], vector["name"]
    assert canonical.decoded_samples == vector["decoded_samples"], vector["name"]

    # The recorded sample array, value-exact (the small vectors carry it).
    if "samples_i32" in vector:
        assert canonical.samples_i32 == vector["samples_i32"], vector["name"]

    # The fixture vector additionally records the STREAMINFO facts.
    if "streaminfo_min_block_size" in vector:
        assert canonical.si_min_block_size == vector["streaminfo_min_block_size"], vector["name"]
        assert canonical.si_max_block_size == vector["streaminfo_max_block_size"], vector["name"]
        assert canonical.si_sample_rate == vector["streaminfo_sample_rate"], vector["name"]
        assert canonical.si_channels == vector["streaminfo_channels"], vector["name"]
        assert canonical.si_bits_per_sample == vector["streaminfo_bits_per_sample"], vector["name"]
        assert canonical.si_total_samples == vector["streaminfo_total_samples"], vector["name"]


@pytest.mark.parametrize("vector", ERROR_VECTORS, ids=lambda v: v["name"])
def test_error_vector_is_refused_with_recorded_kind(vector: dict) -> None:
    with pytest.raises(FfiError) as err:
        decode_canonical(bytes.fromhex(vector["input_hex"]))
    assert err.value.status == -2, vector["name"]
    kind = ERR_INVALID_MAGIC if vector["error"] == "invalid-magic" else ERR_TRUNCATED
    assert err.value.kind == kind, vector["name"]


def test_malformed_input_is_refused_not_crashing() -> None:
    with pytest.raises(FfiError) as err:
        decode_canonical(b"not a flac stream at all")
    assert err.value.status == -2


def test_empty_input_is_refused() -> None:
    with pytest.raises(FfiError):
        decode_canonical(b"")


def test_fixture_pcm_matches_a_rust_pinned_value() -> None:
    # fixture-tone's digest, pinned in the committed reference.json and
    # re-derived by the Rust unit tests; this test fails loudly even if
    # reference.json were regenerated wrongly.
    data = (REPO_ROOT / "tests" / "fixtures" / "tone.flac").read_bytes()
    raw = decode_canonical(data)
    assert hashlib.sha256(parse_canonical(raw).pcm).hexdigest() == (
        "2514eff527051953533d999b57bc3745386432e2a37dcc32ccab77fb67faee6f"
    )
    # Header prologue: STREAMINFO min/max block 16/65535, 44100 Hz,
    # mono, 16-bit.
    assert raw[:8] == bytes([0, 0, 0, 16, 0, 0, 255, 255])
