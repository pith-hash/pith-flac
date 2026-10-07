// SPDX-License-Identifier: MIT
// Copyright (c) 2026 pith-hash

//go:build !windows && !cgo

package pithflac

import "fmt"

// ffiDecode is unavailable without cgo on unix: there is no pure-Go
// dlopen in the standard library. Build with CGO_ENABLED=1 (the CD
// pipeline always does).
func ffiDecode(string, *byte, int, **byte, *uintptr, *int32) (int32, error) {
	return 0, fmt.Errorf("pithflac: cgo is required to load the cdylib on this platform (build with CGO_ENABLED=1)")
}

// ffiFree mirrors the unavailable decode.
func ffiFree(string, *byte, uintptr) {}
