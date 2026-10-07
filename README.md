<p align="center">
  <img src="https://pith-flac.n24q02m.com/logo.svg" alt="pith-flac" width="120">
</p>

<h1 align="center">pith-flac</h1>

<p align="center">
  <strong>FLAC subset decoding: STREAMINFO, constant/verbatim/fixed subframes, both Rice methods</strong>
</p>

<p align="center">
  <a href="https://github.com/pith-hash/pith-flac/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/pith-hash/pith-flac/actions/workflows/ci.yml/badge.svg"></a>
  <a href="https://github.com/pith-hash/pith-flac/actions/workflows/cd.yml"><img alt="CD" src="https://github.com/pith-hash/pith-flac/actions/workflows/cd.yml/badge.svg"></a>
  <a href="https://github.com/pith-hash/pith-flac/releases/latest"><img alt="Latest release" src="https://img.shields.io/github/v/release/pith-hash/pith-flac?display_name=tag&sort=semver"></a>
  <a href="https://github.com/n24q02m/better-semantic-release"><img alt="semantic-release" src="https://img.shields.io/badge/semantic--release-e10079?logo=semantic-release&logoColor=white"></a>
  <a href="LICENSE"><img alt="License: MIT" src="https://img.shields.io/badge/License-MIT-blue.svg"></a>
</p>

<p align="center">
  <a href="#install">Install</a> ·
  <a href="#quick-start">Quick start</a> ·
  <a href="#the-pith-suite-contract">Suite contract</a>
</p>

<!-- BEGIN: AUTO-GENERATED-CROSS-PROMO -->
<!-- END: AUTO-GENERATED-CROSS-PROMO -->

## The pith suite contract

pith-flac is part of the **pith** suite (pith-hash). Every suite repository
follows the same rules; CI enforces them mechanically:

- **Naming**: a library is always `pith-<domain>` (`pith-image`, `pith-audio`,
  `pith-zip`, ...). The curator/repository of repositories is the bare
  `pith-hash`. Never invent a second naming scheme inside the suite.
- **Version pinning**: cross-library dependencies pin `~0.1` (e.g.
  `pith-image = { version = "~0.1", path = "../pith-image" }`). The whole suite
  moves together inside 0.1.x; breaking changes require a suite-wide version
  bump, never a silent minor drift.
- **Zero third-party dependencies**: every crate depends only on other
  `pith-*` crates plus `std`. `scripts/check-zero-deps.py` (run in CI) fails
  the build on any other crate, for normal, build and dev dependencies alike.
- **No unsafe**: every crate root carries `#![forbid(unsafe_code)]`.
- **Hex-exact vectors**: `reference.json` at the repo root is the
  cross-language source of truth. The `gen-reference` binary regenerates it;
  CI verifies the committed copy is current (`gen-reference verify`), and CD
  ships the regenerated file with every SDK artifact. Python, Node and Go SDKs
  MUST test against the same bytes.

## Repository layout

```
crates/            one published crate per suite lib (pith-<domain>)
tools/gen-reference  the vector generator binary (bin name: gen-reference)
sdk/python         ctypes wheel; build backend reads PITH_CDYLIB_DIR
sdk/node           koffi-based package; prebuilds/<os-arch>/ carry the cdylib
sdk/go             cgo binding; go.mod carries the module's cgo flags
fuzz/corpus        fuzz inputs, replayed by tests/fuzz_corpus.rs (parser crates)
reference.json     hex-exact cross-SDK test vectors
```

## Install

Rust (the core library):

```bash
cargo add pith-flac
```

Python / Node / Go SDKs are published from the same cdylib on every release;
see the release assets or the package registries for the matching version.

## Quick start

Rust:

```rust
use pith_flac::{Limits, decode, decode_streaminfo};

// Header only: decide whether the stream is worth decoding.
let info = decode_streaminfo(flac_bytes).unwrap();
assert_eq!(info.sample_rate, 44_100);

// Whole stream to interleaved PCM (`samples[i * channels + c]`).
let pcm = decode(flac_bytes, &Limits::default()).unwrap();
assert_eq!(pcm.channels(), 1);
assert_eq!(pcm.bits_per_sample(), 16);
assert_eq!(pcm.frames(), pcm.samples().len() / usize::from(pcm.channels()));
```

The crate is `no_std` apart from the `alloc` `Vec` its API returns, and
deliberately refuses what it does not implement: LPC subframes surface as
`Error::Unsupported`, never a guess. The committed
[`reference.json`](reference.json) carries the hex-exact decode vectors
(the `tone.flac` conformance fixture plus synthetic constant, verbatim,
fixed-predictor, Rice-partitioned and stereo side-channel streams);
regenerate with `cargo run --bin gen-reference -- gen` and verify with
`cargo run --bin gen-reference -- verify`.

Python / Node / Go SDKs are published from the same cdylib on every release;
see the release assets or the package registries for the matching version.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## Security

See [SECURITY.md](SECURITY.md).

## License

[MIT](LICENSE) © pith-hash
