// SPDX-License-Identifier: MIT
// Copyright (c) 2026 pith-hash

package pithflac

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
)

// repoRoot resolves the repository root relative to this package
// (sdk/go -> sdk -> repo root), the anchor for reference.json and the
// committed fixtures.
func repoRoot(t *testing.T) string {
	t.Helper()
	root, err := filepath.Abs(filepath.Join("..", ".."))
	if err != nil {
		t.Fatal(err)
	}
	if st, err := os.Stat(filepath.Join(root, "reference.json")); err != nil || st.IsDir() {
		t.Fatalf("reference.json not found at %s", root)
	}
	return root
}

// flacVector mirrors one success vector of reference.json.
type flacVector struct {
	Name            string  `json:"name"`
	InputKind       string  `json:"input_kind"`
	InputPath       string  `json:"input_path"`
	InputHex        string  `json:"input_hex"`
	SampleRate      uint32  `json:"sample_rate"`
	Channels        int     `json:"channels"`
	BitsPerSample   int     `json:"bits_per_sample"`
	TotalSamples    uint64  `json:"total_samples"`
	DecodedSamples  uint64  `json:"decoded_samples"`
	SamplesI32      []int32 `json:"samples_i32"`
	PcmSha256       string  `json:"pcm_i32_le_sha256"`
	SIMinBlock      *uint32 `json:"streaminfo_min_block_size"`
	SIMaxBlock      *uint32 `json:"streaminfo_max_block_size"`
	SISampleRate    *uint32 `json:"streaminfo_sample_rate"`
	SIChannels      *uint32 `json:"streaminfo_channels"`
	SIBitsPerSample *uint32 `json:"streaminfo_bits_per_sample"`
	SITotalSamples  *uint64 `json:"streaminfo_total_samples"`
}

// flacErrorVector mirrors one error vector of reference.json.
type flacErrorVector struct {
	Name     string `json:"name"`
	InputHex string `json:"input_hex"`
	Error    string `json:"error"`
}

// reference parses the committed reference.json.
func reference(t *testing.T) ([]flacVector, []flacErrorVector) {
	t.Helper()
	raw, err := os.ReadFile(filepath.Join(repoRoot(t), "reference.json"))
	if err != nil {
		t.Fatal(err)
	}
	var parsed struct {
		Vectors      []flacVector      `json:"vectors"`
		ErrorVectors []flacErrorVector `json:"error_vectors"`
	}
	if err := json.Unmarshal(raw, &parsed); err != nil {
		t.Fatal(err)
	}
	return parsed.Vectors, parsed.ErrorVectors
}

// vectorInput reads the raw FLAC bytes a vector was computed over:
// the committed fixture file, or the inline hex.
func vectorInput(t *testing.T, v flacVector) []byte {
	t.Helper()
	if v.InputKind == "fixture-file" {
		data, err := os.ReadFile(filepath.Join(repoRoot(t), filepath.FromSlash(v.InputPath)))
		if err != nil {
			t.Fatal(err)
		}
		return data
	}
	data, err := hex.DecodeString(v.InputHex)
	if err != nil {
		t.Fatal(err)
	}
	return data
}

// TestReferenceVectorsHexExact replays every committed reference.json
// vector through the cdylib and compares byte-exact: the PCM section's
// SHA-256 against pcm_i32_le_sha256, the decoded samples_i32 array and
// every recorded fact — the same vectors the Rust gen-reference verify
// gate and the Python/Node SDKs check.
func TestReferenceVectorsHexExact(t *testing.T) {
	vectors, _ := reference(t)
	for _, want := range vectors {
		t.Run(want.Name, func(t *testing.T) {
			raw, err := DecodeCanonical(vectorInput(t, want))
			if err != nil {
				t.Fatalf("DecodeCanonical(%s): %v", want.Name, err)
			}
			canonical, err := ParseCanonical(raw)
			if err != nil {
				t.Fatal(err)
			}
			digest := sha256.Sum256(canonical.PCM())
			if got := hex.EncodeToString(digest[:]); got != want.PcmSha256 {
				t.Errorf("%s: digest %s, want %s", want.Name, got, want.PcmSha256)
			}
			if int(canonical.SampleRate) != int(want.SampleRate) ||
				int(canonical.Channels) != want.Channels ||
				int(canonical.BitsPerSample) != want.BitsPerSample ||
				canonical.TotalSamples != want.TotalSamples ||
				canonical.DecodedSamples != want.DecodedSamples {
				t.Errorf("%s: facts %+v, want rate=%d ch=%d bits=%d total=%d decoded=%d",
					want.Name, canonical, want.SampleRate, want.Channels, want.BitsPerSample,
					want.TotalSamples, want.DecodedSamples)
			}
			if want.SamplesI32 != nil {
				got := canonical.SamplesI32()
				if len(got) != len(want.SamplesI32) {
					t.Fatalf("%s: %d samples, want %d", want.Name, len(got), len(want.SamplesI32))
				}
				for i := range got {
					if got[i] != want.SamplesI32[i] {
						t.Fatalf("%s: sample %d = %d, want %d", want.Name, i, got[i], want.SamplesI32[i])
					}
				}
			}
			if want.SIMinBlock != nil {
				if canonical.SIMinBlockSize != *want.SIMinBlock ||
					canonical.SIMaxBlockSize != *want.SIMaxBlock ||
					canonical.SISampleRate != *want.SISampleRate ||
					canonical.SIChannels != *want.SIChannels ||
					canonical.SIBitsPerSample != *want.SIBitsPerSample ||
					canonical.SITotalSamples != *want.SITotalSamples {
					t.Errorf("%s: streaminfo %+v, want %d/%d/%d/%d/%d/%d", want.Name, canonical,
						*want.SIMinBlock, *want.SIMaxBlock, *want.SISampleRate, *want.SIChannels,
						*want.SIBitsPerSample, *want.SITotalSamples)
				}
			}
		})
	}
}

// TestErrorVectorsRefusedWithRecordedKind checks that every committed
// error vector is refused with its recorded kind — a status code,
// never a crash.
func TestErrorVectorsRefusedWithRecordedKind(t *testing.T) {
	_, errorVectors := reference(t)
	for _, want := range errorVectors {
		t.Run(want.Name, func(t *testing.T) {
			data, err := hex.DecodeString(want.InputHex)
			if err != nil {
				t.Fatal(err)
			}
			_, err = DecodeCanonical(data)
			var ffi *FfiError
			if e, ok := err.(*FfiError); ok {
				ffi = e
			} else {
				t.Fatalf("want FfiError, got %v", err)
			}
			if ffi.Status != StatusRejected {
				t.Errorf("%s: want StatusRejected, got %d", want.Name, ffi.Status)
			}
			wantKind := ErrTruncated
			if want.Error == "invalid-magic" {
				wantKind = ErrInvalidMagic
			}
			if ffi.Kind != wantKind {
				t.Errorf("%s: kind %d, want %d", want.Name, ffi.Kind, wantKind)
			}
		})
	}
}

// TestFixturePinnedDigest pins one digest the Rust unit tests
// re-derive, so the binding fails loudly even if reference.json were
// regenerated wrongly.
func TestFixturePinnedDigest(t *testing.T) {
	data, err := os.ReadFile(filepath.Join(repoRoot(t), "tests", "fixtures", "tone.flac"))
	if err != nil {
		t.Fatal(err)
	}
	raw, err := DecodeCanonical(data)
	if err != nil {
		t.Fatal(err)
	}
	canonical, err := ParseCanonical(raw)
	if err != nil {
		t.Fatal(err)
	}
	digest := sha256.Sum256(canonical.PCM())
	const want = "2514eff527051953533d999b57bc3745386432e2a37dcc32ccab77fb67faee6f"
	if got := hex.EncodeToString(digest[:]); got != want {
		t.Errorf("fixture-tone: digest %s, want %s", got, want)
	}
	wantHeader := []byte{0, 0, 0, 16, 0, 0, 255, 255}
	for i, b := range wantHeader {
		if raw[i] != b {
			t.Fatalf("fixture-tone: header byte %d = %d, want %d", i, raw[i], b)
		}
	}
}

// TestMalformedInputIsRefused checks the decoder's refusal path: a
// status code, never a crash.
func TestMalformedInputIsRefused(t *testing.T) {
	_, err := DecodeCanonical([]byte("not a flac stream at all"))
	var ffi *FfiError
	if e, ok := err.(*FfiError); ok {
		ffi = e
	} else {
		t.Fatalf("want FfiError, got %v", err)
	}
	if ffi.Status != StatusRejected {
		t.Errorf("want StatusRejected, got %d", ffi.Status)
	}
}
