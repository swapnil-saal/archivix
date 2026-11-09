# Third-party code and licenses

archivix is licensed under MIT OR Apache-2.0. It builds on the libraries
below. All are permissively licensed (MIT, Apache-2.0, BSD or zlib-style),
with **one exception: the optional unrar code**.

| Component | Used for | License |
|---|---|---|
| [zstd](https://github.com/facebook/zstd) via `zstd` crate | Zstandard | BSD-3-Clause |
| [libdeflate](https://github.com/ebiggers/libdeflate) via `libdeflater` | Fast deflate / gzip | MIT (C library), Apache-2.0 (Rust bindings) |
| [zlib-rs](https://github.com/trifectatechfoundation/zlib-rs) via `flate2` | Streaming deflate | Zlib |
| [liblzma (XZ Utils)](https://tukaani.org/xz/) via `liblzma` crate | xz / lzma | 0BSD (C library), MIT OR Apache-2.0 (bindings) |
| `bzip2` / `libbz2-rs-sys` | bzip2 | MIT OR Apache-2.0 / bzip2-1.0.6 (BSD-style) |
| `lz4_flex` | LZ4 | MIT |
| `brotli` | Brotli | BSD-3-Clause AND MIT |
| `zip` | ZIP reading | MIT |
| `tar` | tar | MIT OR Apache-2.0 |
| `sevenz-rust2`, `lzma-rust2` | 7z | Apache-2.0 |
| **[UnRAR](https://www.rarlab.com/rar_add.htm) via `unrar` / `unrar_sys`** | **RAR extraction (feature `rar`)** | **Rust bindings: MIT OR Apache-2.0. Vendored UnRAR C++ source: unRAR license (freeware, not OSI open source)** |

## About the unRAR license

RAR extraction compiles RARLAB's official UnRAR source. Its license allows
free use and redistribution of the source and of binaries built from it,
**with one restriction: the code may not be used to develop a RAR (WinRAR)
compatible archiver**, meaning one that *creates* RAR archives. archivix only
extracts RAR, which this license permits.

Because the unRAR license is not an OSI-approved open-source license, the
`rar` feature is **separable**:

```sh
cargo build --release --no-default-features    # pure MIT/Apache-2.0 + permissive deps
```

Distributors with strict licensing policies (e.g. Linux distributions) should
build with `--no-default-features`, or ship RAR support as a separate package.

No dependency is GPL-only. The full dependency graph uses MIT, Apache-2.0,
BSD-3-Clause, Zlib, BSL-1.0, Unicode-3.0, CC0/MIT-0, Unlicense and the
bzip2-1.0.6 license, plus unRAR for the optional feature.

For the authoritative license text of each dependency, see the package
sources (e.g. `cargo vendor`) or run `cargo about` / `cargo deny list`.
