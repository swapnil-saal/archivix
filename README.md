# archivix

[![CI](https://github.com/swapnil-saal/archivix/actions/workflows/ci.yml/badge.svg)](https://github.com/swapnil-saal/archivix/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/swapnil-saal/archivix)](https://github.com/swapnil-saal/archivix/releases)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#license)
[![MSRV 1.93](https://img.shields.io/badge/rustc-1.93+-orange)](Cargo.toml)

A parallel archive engine in Rust. One binary (and library) that creates,
extracts, lists, reads and searches **zip, tar, tar.gz, tar.zst, tar.xz,
tar.bz2, tar.lz4, tar.br, tar.lzma, 7z and rar**, plus single compressed
streams (`.gz .zst .xz .bz2 .lz4 .br .lzma`), using every core.

It also lets AI agents and people **look inside archives without extracting
them**: `list`, `cat`, `grep` and an MCP server.

```
archivix c  out.tar.zst  src/ docs/        # create (format from extension)
archivix x  out.tar.zst  -C dest/          # extract (parallel, path-safe)
archivix ls -l out.zip 'src/**/*.rs'       # list without extracting
archivix cat out.tgz  src/main.rs          # read one member
archivix grep 'TODO' out.7z '*.py'         # search inside an archive
archivix info out.tar.gz                   # stats + capabilities
archivix t  out.zip                        # verify CRCs
archivix z  big.log -c zstd                # single-file compress
archivix d  big.log.zst                    # single-file decompress
archivix mcp                               # MCP server for AI agents
```

Every read command accepts `--json`.

## Install

**Prebuilt binaries** for Linux (x86_64, arm64), macOS (Intel, Apple Silicon) and
Windows are attached to each [GitHub release](https://github.com/swapnil-saal/archivix/releases).
Download, extract, and put `archivix` on your `PATH`; each archive has a `.sha256` next to it.

**From source** (Rust 1.93+ and a C/C++ compiler):

```
cargo install --git https://github.com/swapnil-saal/archivix
```

## Why it is fast

No new entropy coder is invented here. zstd, deflate and LZMA are already at
the speed/ratio frontier, and a new one would not beat a decade of tuning. The
speed comes from engine design around those codecs:

| Technique | Effect |
|---|---|
| **Chunk-parallel compression** for gzip, zstd, lz4 and bzip2: independent members/frames/streams, compressed on all cores and written in order with bounded memory | 7–9× faster `.tar.gz` creation than `tar`/`gzip`; output is still a normal stream for `gunzip`, `zstd -d`, `lz4 -d`, `bunzip2` |
| **libdeflate** for deflate (gzip members, zip entries) | Faster *and* smaller than zlib at the same level |
| **Parallel ZIP writer**: entries are compressed concurrently and written in completion order (the central directory holds the offsets); large files are chunked and joined with sync flushes, primed with the previous 32 KiB like pigz | 8× faster than Info-ZIP `zip` on mixed data |
| **Smart store**: extension check plus an entropy/LZ4 probe skips recompressing media, archives and random data | No CPU wasted on JPEG/MP4/zip members |
| **AXIX side index** (see below) | Instant listing, random access and parallel decompression for `.tar.gz` / `.tar.zst` / `.tar.lz4` |
| **Parallel decode** wherever chunk boundaries can be found: the AXIX index, multi-frame zstd, multi-stream bzip2 (pbzip2/lbzip2 output), multi-block xz (`xz -T0`) and 7z blocks | Decoding is not limited to one core |
| **Whole-buffer libdeflate inflate** for standard single-member `.gz` | ~2× zlib-class inflate on archives from other tools |
| **Parallel extraction**: zip entries on all cores (largest first); tar members are written by a worker pool with a memory budget | Many-small-file archives are not limited by per-file syscall latency |
| mmap input, thread-local codec contexts, LTO + `codegen-units=1` | Less copying and allocation |

## The AXIX index: instant listing for tar.gz / tar.zst

A `.tar.gz` normally has to be decompressed end to end just to list it.
archivix writes gz/zst/lz4 streams as independently decodable chunks and
appends a small compressed index: each chunk's sizes plus every tar entry's
name, size, mode, mtime, CRC32 and offset. The index hides in containers that
every standard decoder skips:

* **zstd / lz4:** skippable frames (magic `0x184D2A5A`, footer `0x184D2A5B`)
* **gzip:** empty gzip members carrying the payload in the FEXTRA field

So `tar -xzf`, `gunzip`, `zstd -d` and `lz4 -d` read archivix output
unchanged, while archivix gets:

* `list` in ~10 ms regardless of archive size
* `cat` / selective `extract` decode only the chunks a member spans
* full extraction decodes chunks in parallel

Disable it with `--no-index`.

## Formats

| Format | Create | Extract | List | Decode |
|---|---|---|---|---|
| zip (jar, apk, docx, whl, …) | parallel; deflate/zstd/store/bzip2/xz; ZIP64 | parallel; deflate, deflate64, bzip2, zstd, lzma, xz; ZipCrypto + AES | instant (central directory) | parallel per entry |
| tar | yes | parallel writes | instant (seeks over data) | — |
| tar.zst / .zst | chunk-parallel + index | yes | instant with index | parallel (multi-frame) |
| tar.gz / .tgz / .gz | chunk-parallel + index | yes | instant with index | parallel with index; libdeflate otherwise |
| tar.lz4 / .lz4 | chunk-parallel + index | yes | instant with index | parallel with index |
| tar.bz2 / .bz2 | chunk-parallel (multi-stream) | yes | stream | parallel for multi-stream |
| tar.xz / .xz | multithreaded liblzma (multi-block) | yes | stream | multithreaded for multi-block |
| tar.br, tar.lzma | yes | yes | stream | sequential |
| 7z | multithreaded LZMA2, solid | yes, incl. AES | instant (header) | parallel per block |
| rar v1.5–v5 | **no**: the encoder is proprietary | yes (official unrar code) | instant (headers) | sequential |

Formats are detected by **content** (magic bytes), not extension. Self-
extracting zips and mislabelled files work.

## Safety

* Member paths are sanitised: absolute paths, `..` and drive letters can never
  escape the destination (zip-slip / tar traversal).
* Files are opened with `O_NOFOLLOW`, and archive symlinks/hardlinks are created
  only after all regular files, so an archive cannot plant a symlink and then
  write through it.
* setuid/setgid bits are stripped on extract.
* `test` verifies CRC32 (zip, 7z, rar, AXIX-indexed tar) and stream checksums
  (gz, zst, xz, bz2).

## For AI agents

`archivix mcp` speaks the Model Context Protocol over stdio. Tools:

| Tool | Purpose |
|---|---|
| `archive_list` | members with size/kind/mtime; glob filter; pagination |
| `archive_info` | format, counts, sizes, ratio, capabilities |
| `archive_read` | one member's content (text, or a hex preview for binary), paged by `offset`/`max_bytes` |
| `archive_grep` | regex search across text members |
| `archive_extract` | extract all or selected members |
| `archive_create` | create from paths |

Register with Claude Code:

```
claude mcp add archivix -- /path/to/archivix mcp
```

Without MCP, every CLI read command has `--json`.

## Build

```
cargo build --release                         # includes RAR extraction
cargo build --release --no-default-features   # without the unrar C++ code
cargo test --release
```

RAR support compiles the official UnRAR source, which is freeware under the
unRAR license (not OSI open source). It is optional for that reason; see
[THIRD_PARTY.md](THIRD_PARTY.md).

## Library

```rust
let stats = archivix::create("out.tar.zst".as_ref(), &["src".into()], &Default::default())?;
let (format, entries) = archivix::list("out.tar.zst".as_ref(), None)?;
archivix::cat("out.tar.zst".as_ref(), "src/lib.rs", None, &mut std::io::stdout())?;
```

## Benchmarks

Apple M4 (10 cores) against the installed reference tools. Full table, method and caveats
in [BENCHMARKS.md](BENCHMARKS.md). archivix is faster in 34 of 37 scenarios.

| Task (609 MB mixed / 230 MB source tree) | Reference | archivix | |
|---|---:|---:|---:|
| create .zip (mixed) | Info-ZIP 11.67 s | 1.52 s | 7.7× |
| create .tar.gz (mixed) | tar \| gzip 13.34 s | 1.73 s | 7.7×, 1.3% smaller |
| extract .tar.zst (src) | zstd -dc \| tar 5.19 s | 0.85 s | 6.1× |
| extract .tar.bz2 (src) | tar -xjf 7.16 s | 1.17 s | 6.1× |
| list .tar.gz (mixed) | tar -tzf 523 ms | 12 ms | 45× |
| read 1 file from .tar.gz | tar -xOzf 195 ms | 12 ms | 16× |
| gzip a 497 MB file | gzip -6 7.62 s | 0.61 s | 12× |

Ties: lz4 and xz compression (same engines, already memory- or liblzma-bound), and
single-member zip reads (both under 10 ms).

## Contributing

Issues and pull requests are welcome. Please read [CONTRIBUTING.md](CONTRIBUTING.md)
and the [Code of Conduct](CODE_OF_CONDUCT.md). Report security issues privately as
described in [SECURITY.md](SECURITY.md).

## License

Licensed under either of

* Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
* MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option. The optional RAR feature compiles third-party code under the
unRAR license; see [THIRD_PARTY.md](THIRD_PARTY.md).

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in archivix by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
