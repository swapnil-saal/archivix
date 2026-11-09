# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.0] - 2026-11-09

First public release.

### Added

- Create, extract, list, `cat`, `grep`, `info` and `test` for zip, tar,
  tar.gz, tar.zst, tar.xz, tar.bz2, tar.lz4, tar.br, tar.lzma and 7z; RAR
  extraction (optional `rar` feature, on by default); single-stream
  `compress` / `decompress` for gz, zst, xz, bz2, lz4, br and lzma.
- Chunk-parallel compression for gzip, zstd, lz4 and bzip2 that stays readable
  by the standard tools.
- Parallel ZIP writer with smart store for incompressible data and automatic
  ZIP64.
- AXIX side index for gz/zst/lz4: instant listing, random access and parallel
  decompression, invisible to standard decoders.
- Parallel decompression for multi-frame zstd, multi-stream bzip2, multi-block
  xz and 7z blocks; whole-buffer libdeflate inflate for standard gzip.
- Parallel, path-traversal-safe extraction (O_NOFOLLOW, deferred links).
- MCP server (`archivix mcp`) and `--json` output for AI agents.
- Desktop app (`archivix-gui`, egui) for macOS, Windows and Linux: browse,
  preview, extract, test and create archives with drag and drop; universal
  macOS `.dmg`.
- Live progress bar in the CLI and a progress/cancel API in the library
  (`archivix::progress()`).
- `ExtractOptions::literal` for exact member names (names containing `[`,
  `*` or `?` are not treated as globs).
- Content-based format detection; ZipCrypto/AES zip and AES 7z decryption.

[Unreleased]: https://github.com/swapnil-saal/archivix/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/swapnil-saal/archivix/releases/tag/v0.1.0
