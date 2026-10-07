// SPDX-License-Identifier: MIT
// Copyright (c) 2026 pith-hash

// Package pithflac provides Go bindings for the pith-flac Rust
// cdylib: FLAC decoding into the canonical vector stream.
//
// The single Rust core (built by `cargo build --release`) is loaded at
// runtime; the package carries zero module dependencies. On unix the
// cdylib is opened with dlopen through cgo, on Windows with
// LoadLibrary through the standard syscall package — both resolve the
// library through the same discovery chain, so `go build ./... &&
// go test ./...` works unchanged on every OS the CD matrix builds.
//
// Discovery order (the suite's cdylib convention):
//
//  1. PITH_CDYLIB — an explicit cdylib file path;
//  2. PITH_CDYLIB_DIR — a directory scanned for the cdylib names (the
//     CD pipeline points this at target/release);
//  3. <repo root>/target/release — the repository working-tree layout,
//     anchored at this package's source directory, so a source
//     checkout runs against a local cargo build unconfigured.
//
// The FFI surface is one decode operation plus one free:
// pith_flac_decode decodes a whole FLAC stream into the canonical byte
// stream the reference.json vectors are defined over (a 64-byte
// big-endian header of STREAMINFO and decoded facts, then the decoded
// PCM as interleaved i32 little-endian), and pith_flac_free releases
// the handed-out buffer.
package pithflac

import (
	"encoding/binary"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"sync"
	"unsafe"
)

// Status codes returned by the cdylib's C ABI.
const (
	// StatusOK: success.
	StatusOK int32 = 0
	// StatusInvalid: a caller argument is invalid (a null pointer).
	StatusInvalid int32 = -1
	// StatusRejected: the core decoder refused the input (malformed
	// FLAC: bad magic, bad metadata block, CRC mismatch, truncated
	// stream).
	StatusRejected int32 = -2
)

// Refusal kinds reported through FfiError.Kind on StatusRejected — the
// stable names reference.json's error_vectors record as strings.
const (
	// ErrBadValue: Error::BadValue ("bad-value").
	ErrBadValue int32 = 1
	// ErrInvalidMagic: Error::InvalidMagic ("invalid-magic").
	ErrInvalidMagic int32 = 2
	// ErrTooLarge: Error::TooLarge ("too-large").
	ErrTooLarge int32 = 3
	// ErrTruncated: Error::Truncated ("truncated").
	ErrTruncated int32 = 4
	// ErrUnsupported: Error::Unsupported ("unsupported").
	ErrUnsupported int32 = 5
)

// HeaderLen is the canonical stream's header length in bytes.
const HeaderLen = 64

// ErrKindNames maps a refusal kind code to the stable name
// reference.json records.
var ErrKindNames = map[int32]string{
	ErrBadValue:     "bad-value",
	ErrInvalidMagic: "invalid-magic",
	ErrTooLarge:     "too-large",
	ErrTruncated:    "truncated",
	ErrUnsupported:  "unsupported",
}

// cdylibNames are the file names cargo may drop into the build
// directory, per platform (windows / linux / macOS).
var cdylibNames = []string{"pith_flac.dll", "libpith_flac.so", "libpith_flac.dylib"}

// FfiError reports a non-zero status code from the cdylib.
type FfiError struct {
	// Op is the FFI operation name.
	Op string
	// Status is the raw status code the FFI returned.
	Status int32
	// Kind is the refusal kind code (0 unless Status is
	// StatusRejected); one of the Err* constants.
	Kind int32
}

func (e *FfiError) Error() string {
	kind := "unknown failure"
	switch e.Status {
	case StatusInvalid:
		kind = "invalid argument"
	case StatusRejected:
		kind = "input rejected"
	}
	if e.Kind != 0 {
		if name, ok := ErrKindNames[e.Kind]; ok {
			kind = fmt.Sprintf("%s (%s)", kind, name)
		} else {
			kind = fmt.Sprintf("%s (kind %d)", kind, e.Kind)
		}
	}
	return fmt.Sprintf("%s failed: %s (status %d)", e.Op, kind, e.Status)
}

// FindCdylib locates the cdylib through the suite's discovery chain.
func FindCdylib() (string, error) {
	if p := os.Getenv("PITH_CDYLIB"); p != "" {
		if st, err := os.Stat(p); err == nil && st.Mode().IsRegular() {
			return filepath.Abs(p)
		}
	}
	_, thisFile, _, ok := runtime.Caller(0)
	if !ok {
		return "", fmt.Errorf("pithflac: cannot locate the package source directory")
	}
	pkgDir := filepath.Dir(thisFile)
	repoRoot := filepath.Dir(filepath.Dir(pkgDir)) // sdk/go -> sdk -> repo root

	var dirs []string
	if env := os.Getenv("PITH_CDYLIB_DIR"); env != "" {
		dirs = append(dirs, env)
		if !filepath.IsAbs(env) {
			dirs = append(dirs, filepath.Join(repoRoot, env))
		}
	}
	dirs = append(dirs, filepath.Join(repoRoot, "target", "release"))
	for _, dir := range dirs {
		for _, name := range cdylibNames {
			p := filepath.Join(dir, name)
			if st, err := os.Stat(p); err == nil && st.Mode().IsRegular() {
				return p, nil
			}
		}
	}
	return "", fmt.Errorf(
		"pithflac: no cdylib found (searched PITH_CDYLIB, PITH_CDYLIB_DIR and <repo>/target/release); run `cargo build --release` first",
	)
}

// locate resolves the cdylib path once per process.
var locate = sync.OnceValues(FindCdylib)

// Canonical is a decoded FLAC, re-expressed from the canonical byte
// stream: the STREAMINFO facts, the decoded facts and the PCM.
type Canonical struct {
	// SIMinBlockSize is STREAMINFO's smallest block size (samples per
	// channel).
	SIMinBlockSize uint32
	// SIMaxBlockSize is STREAMINFO's largest block size.
	SIMaxBlockSize uint32
	// SISampleRate is STREAMINFO's sample rate in Hz (0 when the
	// frames carry it).
	SISampleRate uint32
	// SIChannels is STREAMINFO's channel count, 1-8.
	SIChannels uint32
	// SIBitsPerSample is STREAMINFO's bits per sample, 4-32.
	SIBitsPerSample uint32
	// SITotalSamples is STREAMINFO's declared total samples per
	// channel (0 = unknown).
	SITotalSamples uint64
	// SampleRate is the decoded sample rate in Hz (resolved from the
	// frames when STREAMINFO said 0).
	SampleRate uint32
	// Channels is the decoded channel count, 1-8.
	Channels uint32
	// BitsPerSample is the decoded bits per sample.
	BitsPerSample uint32
	// TotalSamples is the declared total samples per channel, 0 when
	// unknown.
	TotalSamples uint64
	// DecodedSamples is the decoded sample count, interleaved across
	// channels.
	DecodedSamples uint64
	// Raw is the canonical byte stream the digest is computed over.
	Raw []byte
}

// PCM returns the interleaved i32 little-endian sample buffer —
// exactly the bytes the pcm_i32_le_sha256 digest covers.
func (c *Canonical) PCM() []byte {
	return c.Raw[HeaderLen:]
}

// SamplesI32 decodes the PCM as Go ints, in stream order.
func (c *Canonical) SamplesI32() []int32 {
	pcm := c.PCM()
	out := make([]int32, len(pcm)/4)
	for i := range out {
		out[i] = int32(binary.LittleEndian.Uint32(pcm[i*4:]))
	}
	return out
}

// DecodeCanonical decodes a complete FLAC stream into the canonical
// byte stream the reference.json vectors are defined over. The
// returned slice is a Go copy; the handed-out cdylib buffer is
// released before returning.
func DecodeCanonical(data []byte) ([]byte, error) {
	libPath, err := locate()
	if err != nil {
		return nil, err
	}
	var out *byte
	var outLen uintptr
	var kind int32
	var dataPtr *byte
	if len(data) > 0 {
		dataPtr = &data[0]
	}
	status, err := ffiDecode(libPath, dataPtr, len(data), &out, &outLen, &kind)
	if err != nil {
		return nil, err
	}
	if status != StatusOK {
		return nil, &FfiError{Op: "pith_flac_decode", Status: status, Kind: kind}
	}
	buf := make([]byte, outLen)
	copy(buf, unsafe.Slice(out, outLen))
	ffiFree(libPath, out, outLen)
	return buf, nil
}

// ParseCanonical re-expresses the canonical byte stream as a
// Canonical.
func ParseCanonical(raw []byte) (*Canonical, error) {
	if len(raw) < HeaderLen {
		return nil, fmt.Errorf("pithflac: canonical stream is shorter than the 64-byte header")
	}
	be32 := func(off int) uint32 {
		return binary.BigEndian.Uint32(raw[off:])
	}
	be64 := func(off int) uint64 {
		return binary.BigEndian.Uint64(raw[off:])
	}
	return &Canonical{
		SIMinBlockSize:  be32(0),
		SIMaxBlockSize:  be32(4),
		SISampleRate:    be32(8),
		SIChannels:      be32(12),
		SIBitsPerSample: be32(16),
		SITotalSamples:  be64(20),
		SampleRate:      be32(28),
		Channels:        be32(32),
		BitsPerSample:   be32(36),
		TotalSamples:    be64(40),
		DecodedSamples:  be64(48),
		Raw:             raw,
	}, nil
}
