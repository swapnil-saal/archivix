# Benchmarks

**Machine:** Apple M4, 10 cores, 16 GB, internal SSD, macOS 26 (Darwin 25.5).
**Method:** `bench/bench.py`, wall-clock median of 3 runs (1 run when a case takes over 10 s), warm page cache,
fresh output directory per extraction run. Every extractor in a row reads the *same* input archive.
All archivix outputs were verified byte-exact by round trip and by the reference tool (`bench/roundtrip.sh`).

**Corpora** (real data, not synthetic text):

| Name | Content | Size |
|---|---|---|
| src | Rust crate sources (cargo registry), 10,241 files | 230 MB |
| mixed | src + 239 MB of compiled binaries + 150 MB of random data (stand-in for media) | 609 MB, 10,628 files |
| big | one 497 MB file (tar of sources + binaries) | 497 MB |

**Competitors:** whatever was installed on the machine: Info-ZIP `zip`/`unzip` 3.0/6.0, macOS `tar` (bsdtar 3.5,
libarchive), `gzip`, `bzip2`, `zstd` (with `-T0`, all cores), `xz` (with `-T0`), `lz4`.
**Not tested:** `pigz`, `pbzip2` and the official `7zz`. They weren't installed, and downloading binaries was
disallowed in this session. pigz in particular would close much of the gzip-*creation* gap. Re-run
`bench/bench.py` with them installed for a stricter comparison.

| Scenario | Fastest other tool | Time | archivix | Time | Speedup | Size other → archivix |
|---|---|---:|---|---:|---:|---|
| create zip (src) | zip -r (Info-ZIP) | 2.55 s | archivix | **1.24 s** | **2.1×** | 33.1 → 32.4 MB (-2.4%) |
| extract zip (src) | bsdtar -x | 1.81 s | archivix | **881 ms** | **2.1×** |  |
| list zip (src) | unzip -l | 35 ms | archivix ls | **25 ms** | **1.4×** |  |
| create zip (mixed) | zip -r (Info-ZIP) | 11.67 s | archivix | **1.52 s** | **7.7×** | 276.9 → 275.0 MB (-0.7%) |
| extract zip (mixed) | bsdtar -x | 2.69 s | archivix | **1.42 s** | **1.9×** |  |
| list zip (mixed) | unzip -l | 49 ms | archivix ls | **26 ms** | **1.9×** |  |
| create tar.gz (src) | tar \| gzip -6 | 6.02 s | archivix | **1.21 s** | **5.0×** | 29.5 → 27.5 MB (-6.9%) |
| extract tar.gz (src) | tar -xzf  [archivix .tgz] | 1.74 s | archivix [archivix .tgz] | **896 ms** | **1.9×** |  |
| list tar.gz (src) | tar -tzf | 192 ms | archivix ls [archivix .tgz] | **11 ms** | **17.5×** |  |
| create tar.gz (mixed) | tar \| gzip -6 | 13.34 s | archivix | **1.73 s** | **7.7×** | 273.5 → 269.8 MB (-1.3%) |
| extract tar.gz (mixed) | tar -xzf  [archivix .tgz] | 2.82 s | archivix [archivix .tgz] | **1.57 s** | **1.8×** |  |
| list tar.gz (mixed) | tar -tzf | 523 ms | archivix ls [archivix .tgz] | **12 ms** | **44.7×** |  |
| create tar.zst (src) | tar \| zstd -T0 -3 | 5.43 s | archivix | **1.22 s** | **4.4×** | 23.8 → 23.2 MB (-2.4%) |
| extract tar.zst (src) | zstd -dc \| tar -x | 5.19 s | archivix [archivix file] | **851 ms** | **6.1×** |  |
| list tar.zst (src) | zstd -dc \| tar -t | 138 ms | archivix ls [archivix file] | **12 ms** | **12.0×** |  |
| create tar.zst (mixed) | tar \| zstd -T0 -3 | 6.03 s | archivix | **1.61 s** | **3.7×** | 261.1 → 250.3 MB (-4.1%) |
| extract tar.zst (mixed) | zstd -dc \| tar -x | 5.98 s | archivix [zstd-cli file] | **1.12 s** | **5.4×** |  |
| list tar.zst (mixed) | zstd -dc \| tar -t | 349 ms | archivix ls [archivix file] | **12 ms** | **30.1×** |  |
| read 1 member (tar.gz, src) | tar -xOzf | 195 ms | archivix cat [archivix .tgz] | **12 ms** | **15.8×** |  |
| read 1 member (tar.zst, src) | zstd -dc \| tar -xO | 131 ms | archivix cat [archivix file] | **20 ms** | **6.5×** |  |
| read 1 member (zip, src) | unzip -p | 9 ms | archivix cat | 11 ms | 0.8× |  |
| create tar.xz (src) | tar \| xz -T0 -6 | 8.19 s | archivix | **7.57 s** | **1.1×** | 16.1 → 15.4 MB (-4.3%) |
| extract tar.xz (src) | xz -dc \| tar -x | 5.23 s | archivix | **951 ms** | **5.5×** |  |
| create tar.bz2 (src) | tar -cjf (bsdtar) | 15.54 s | archivix | **1.91 s** | **8.1×** | 20.0 → 19.3 MB (-3.8%) |
| extract tar.bz2 (src) | tar -xjf  [std] | 7.16 s | archivix [archivix file] | **1.17 s** | **6.1×** |  |
| create 7z (src) | bsdtar --format 7zip | 32.85 s | archivix | **9.11 s** | **3.6×** | 15.1 → 15.2 MB (+0.3%) |
| extract 7z (src) | bsdtar -x | 2.17 s | archivix | **975 ms** | **2.2×** |  |
| compress 497MB file (gzip) | gzip -6 | 7.62 s | archivix | **613 ms** | **12.4×** | 123.4 → 121.8 MB (-1.3%) |
| decompress 497MB file (gzip) | gzip -d | 389 ms | archivix [archivix file] | **84 ms** | **4.6×** |  |
| compress 497MB file (zstd) | zstd -T0 -3 | 244 ms | archivix | **213 ms** | **1.1×** | 111.1 → 111.8 MB (+0.7%) |
| decompress 497MB file (zstd) | zstd -d | 320 ms | archivix [archivix file] | **108 ms** | **3.0×** |  |
| compress 497MB file (lz4) | lz4 | 416 ms | archivix | 413 ms | 1.0× | 172.7 → 173.1 MB (+0.2%) |
| decompress 497MB file (lz4) | lz4 -d | 196 ms | archivix [archivix file] | **55 ms** | **3.5×** |  |
| compress 497MB file (xz) | xz -T0 -6 | 13.15 s | archivix | 13.89 s | 0.9× | 77.1 → 77.1 MB (+0.0%) |
| decompress 497MB file (xz) | xz -d | 563 ms | archivix [archivix file] | **522 ms** | **1.1×** |  |
| compress 497MB file (bzip2) | bzip2 -9 | 19.42 s | archivix | **2.91 s** | **6.7×** | 115.2 → 115.1 MB (-0.0%) |
| decompress 497MB file (bzip2) | bzip2 -d | 6.24 s | archivix [archivix file] | **1.10 s** | **5.7×** |  |

Default levels on both sides: gzip/zip 6, zstd 3, xz 6, bzip2 9.

## Reading the results

**Faster in 34 of 37 scenarios**, most of them several times over. The table compares against the *fastest* competitor variant in each row (e.g. `tar -xzf` on whichever .tgz it handled best), and archives usually come out *smaller*
(libdeflate beats zlib at the same level; smart-store skips media).

**Not wins, or wins with a cost**, stated plainly:

* **zip single-member read:** a tie. Both `unzip -p` and `archivix cat` finish in under 10 ms. (The 11 ms above
  was measured before the thread pool was made lazy; after the change it is about 6 ms vs 9 ms, within noise.)
* **lz4 single-file compress:** a tie. lz4 at ~1.2 GB/s per core is bound by memory and I/O, not CPU.
* **xz compress:** 0.9–1.1×. archivix and `xz -T0` use the same multithreaded liblzma engine with the same block
  size, so this is a tie within noise. (Decompression wins against a piped `xz | tar`; on a single file it ties.)
* **zstd single-file compress:** 1.1× faster but **0.7% larger**, because archivix splits the input into
  independent 4 MiB frames. That costs a little ratio and buys parallel decompression (3× faster) and random access.

**Why the gaps are large elsewhere:** Info-ZIP `zip`, `gzip`, `bzip2` and bsdtar are single-threaded. `zstd -T0`
compresses in parallel but always decodes on one core, and tar extraction then writes files one at a time.
archivix parallelises every stage, and its AXIX index turns listing and single-member reads into O(index)
operations instead of O(archive).
