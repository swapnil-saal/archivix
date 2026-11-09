# Contributing to archivix

Thanks for your interest! Bug reports, format-compatibility reports, benchmarks
on other hardware and pull requests are all welcome.

## Ground rules

* Be kind. This project follows the [Code of Conduct](CODE_OF_CONDUCT.md).
* **Compatibility is non-negotiable.** Every archive archivix writes must be
  readable by the reference tool for its format (`unzip`, `tar`, `gzip`,
  `zstd`, `xz`, `bzip2`, `lz4`, `brotli`, 7-Zip). A speedup that breaks this is
  a bug.
* **Performance claims need numbers.** If a PR is about speed, include before
  and after timings from `bench/bench.py` (or a focused reproduction) and the
  machine they ran on. Report regressions as plainly as wins.
* **Safety first on extraction.** Changes to `src/extract.rs` must keep
  path-traversal and symlink protections intact; add a test for any new case.

## Getting started

```sh
git clone https://github.com/swapnil-saal/archivix
cd archivix
cargo build --release
cargo test --release
```

Requirements: Rust **1.93+** and a C/C++ compiler (zstd, liblzma, libdeflate and
unrar are compiled from source). To build without the unrar C++ code:
`cargo build --no-default-features`.

## Before you open a PR

```sh
cargo fmt --all
cargo clippy --all-targets --release -- -D warnings
cargo test --release
bench/roundtrip.sh target/release/archivix <some-dir> <scratch-dir>   # optional, end-to-end
```

CI runs fmt, clippy and the tests on Linux, macOS and Windows, plus an MSRV
build.

## Project layout

| Path | What lives there |
|---|---|
| `src/parallel.rs` | Chunk-parallel compressor/decompressor (the engine core) |
| `src/index.rs` | The AXIX side index hidden in gzip/zstd/lz4 streams |
| `src/zip_write.rs` / `src/zip_read.rs` | Parallel ZIP writer, ZIP reading/extraction |
| `src/tar_fmt.rs` | tar + every stream codec, listing, random access |
| `src/sevenz.rs`, `src/rar.rs` | 7z and RAR |
| `src/extract.rs` | Path sanitising and parallel, safe writes to disk |
| `src/format.rs`, `src/codec.rs` | Format detection and codec plumbing |
| `src/mcp.rs` | MCP server for AI agents |
| `src/main.rs` | CLI |
| `tests/` | Integration tests (round trips, traversal, index compatibility) |
| `bench/` | Benchmark harness, round-trip script, report generator |

## Adding a format

1. Implement `list`, `extract` and `visit` (and `create` if the format can be
   written) in a new module, following `sevenz.rs` as a template.
2. Add detection by magic bytes in `src/format.rs`.
3. Wire it into the dispatchers in `src/lib.rs`.
4. Add a round-trip test in `tests/roundtrip.rs` and, if a reference tool
   exists, a compatibility check in `bench/roundtrip.sh`.

## Reporting bugs

Please include the archivix version (`archivix --version`), your OS, the exact
command, and, if you can share it, the archive (or the tool and command that
produced it). For security issues, see [SECURITY.md](SECURITY.md) instead.

## License

By contributing, you agree that your contributions are dual-licensed under
MIT OR Apache-2.0, as described in the README, without any additional terms or
conditions.
