//! archivix — a parallel, format-agnostic archive engine.
//!
//! One API over zip, tar (+ gz / zst / xz / bz2 / lz4 / br / lzma), 7z, rar and
//! single compressed streams. See `README.md` for the design.

pub mod codec;
pub mod entry;
pub mod extract;
pub mod format;
pub mod index;
pub mod mcp;
pub mod parallel;
pub mod progress;
#[cfg(feature = "rar")]
pub mod rar;
pub mod sevenz;
pub mod tar_fmt;
pub mod util;
pub mod walk;
pub mod zip_read;
pub mod zip_write;

pub use codec::Codec;
pub use entry::{Entry, EntryKind};
pub use extract::{ExtractOptions, ExtractStats};
pub use format::Format;
pub use progress::progress;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Clone, Serialize)]
pub struct CreateStats {
    pub entries: u64,
    pub input_bytes: u64,
    pub output_bytes: u64,
}

/// Streaming visitor over archive members: `filter` decides which members are
/// opened, `each` receives the member and a reader over its content and
/// returns `false` to stop early.
pub struct Visitor<'a> {
    pub filter: &'a dyn Fn(&Entry) -> bool,
    pub each: &'a mut dyn FnMut(&Entry, &mut dyn Read) -> Result<bool>,
}

#[derive(Debug, Clone)]
pub struct CreateOptions {
    /// Output format; inferred from the output name when None.
    pub format: Option<Format>,
    pub level: Option<i32>,
    /// zip only: store | deflate | zstd | bzip2 | xz
    pub method: Option<String>,
    pub chunk: Option<usize>,
    /// Append the AXIX index to gz/zst/lz4 output (on by default).
    pub index: bool,
    /// Store incompressible files instead of compressing them (zip).
    pub smart_store: bool,
    pub follow_symlinks: bool,
    pub excludes: Vec<String>,
    /// Resolve inputs relative to this directory (like `tar -C`).
    pub base_dir: Option<PathBuf>,
}

impl Default for CreateOptions {
    fn default() -> Self {
        CreateOptions {
            format: None,
            level: None,
            method: None,
            chunk: None,
            index: true,
            smart_store: true,
            follow_symlinks: false,
            excludes: Vec::new(),
            base_dir: None,
        }
    }
}

/// Create an archive at `output` from files and directories.
pub fn create(output: &Path, inputs: &[PathBuf], opts: &CreateOptions) -> Result<CreateStats> {
    let format = match opts.format {
        Some(f) => f,
        None => Format::from_name(&output.to_string_lossy())
            .with_context(|| format!("cannot infer archive format from '{}'; use --format", output.display()))?,
    };
    let wo = walk::WalkOptions {
        follow_symlinks: opts.follow_symlinks,
        excludes: opts.excludes.clone(),
        skip: Some(output.to_path_buf()),
        base: opts.base_dir.clone(),
    };
    let files = walk::collect(inputs, &wo)?;
    let p = progress();
    p.set_total(files.iter().map(|f| f.size).sum());
    p.set_total_entries(files.len() as u64);
    match format {
        Format::Zip => {
            let method = match &opts.method {
                Some(m) => zip_write::ZipMethod::parse(m).with_context(|| format!("unknown zip method '{m}'"))?,
                None => zip_write::ZipMethod::Deflate,
            };
            let default_level = if method == zip_write::ZipMethod::Zstd { 3 } else { 6 };
            let zo = zip_write::ZipOptions { method, level: opts.level.unwrap_or(default_level), smart_store: opts.smart_store };
            zip_write::create(output, &files, &zo)
        }
        Format::Tar(codec) => {
            let to =
                tar_fmt::TarOptions { codec, level: opts.level.unwrap_or(codec.default_level()), chunk: opts.chunk, index: opts.index };
            tar_fmt::create(output, &files, &to)
        }
        Format::SevenZ => sevenz::create(output, &files, opts.level.unwrap_or(6)),
        Format::Rar => bail!("creating RAR archives is not possible (proprietary encoder); use .7z, .zip or .tar.zst"),
        Format::Raw(_) => bail!(
            "'{}' is a single-stream format; use `archivix compress` for single files, or name the output .tar.{}",
            output.display(),
            output.extension().and_then(|e| e.to_str()).unwrap_or("zst")
        ),
    }
}

/// Compress a single file into a stream (`file.zst`, `file.gz`, ...).
pub fn compress_file(
    input: &Path,
    output: &Path,
    codec: Codec,
    level: Option<i32>,
    chunk: Option<usize>,
    index: bool,
) -> Result<CreateStats> {
    let level = level.unwrap_or(codec.default_level());
    let to = tar_fmt::TarOptions { codec, level, chunk, index };
    let mut sink = tar_fmt::open_sink(output, &to)?;
    let f = std::fs::File::open(input).with_context(|| format!("open {}", input.display()))?;
    progress().set_total(f.metadata()?.len());
    let mut f = progress::Counted::new(f, progress::Count::Both);
    let n = copy_big(&mut f, &mut sink)?;
    tar_fmt::finish_sink(sink, index, None)?;
    Ok(CreateStats { entries: 1, input_bytes: n, output_bytes: std::fs::metadata(output)?.len() })
}

/// Decompress a single stream (auto-detected codec) into `out`.
pub fn decompress_to(input: &Path, out: &mut dyn Write) -> Result<u64> {
    let codec = match format::detect(input)? {
        Format::Raw(c) | Format::Tar(c) => c,
        f => bail!("{} is a {} archive, not a compressed stream; use `archivix extract`", input.display(), f.name()),
    };
    let op = tar_fmt::open_stream(input, codec)?;
    let mut r = op.reader(codec)?;
    Ok(copy_big(&mut r, out)?)
}

fn copy_big(r: &mut dyn Read, w: &mut dyn Write) -> io::Result<u64> {
    let mut buf = vec![0u8; 1 << 20];
    let mut total = 0u64;
    loop {
        let n = match r.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        w.write_all(&buf[..n])?;
        total += n as u64;
    }
    Ok(total)
}

fn raw_entry(path: &Path, codec: Codec) -> Result<Entry> {
    let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "data".into());
    let op = tar_fmt::open_stream(path, codec)?;
    let size = match (&op.index, &op.spans) {
        (Some(i), _) => i.total_uncompressed(),
        (None, Some(s)) if codec == Codec::Zstd => s.iter().map(|s| s.u_size as u64).sum(),
        _ => 0,
    };
    let mut e = Entry::new(name, EntryKind::File, size);
    e.compressed_size = Some(op.data_end as u64);
    e.method = Some(codec.name().into());
    Ok(e)
}

/// List members without extracting anything to disk.
pub fn list(path: &Path, password: Option<&str>) -> Result<(Format, Vec<Entry>)> {
    let f = format::detect(path)?;
    let entries = match f {
        Format::Zip => zip_read::list(path)?,
        Format::Tar(c) => tar_fmt::list(path, c)?,
        Format::SevenZ => sevenz::list(path, password)?,
        #[cfg(feature = "rar")]
        Format::Rar => rar::list(path, password)?,
        #[cfg(not(feature = "rar"))]
        Format::Rar => bail!("RAR support not compiled in (enable feature 'rar')"),
        Format::Raw(c) => vec![raw_entry(path, c)?],
    };
    Ok((f, entries))
}

pub fn extract(path: &Path, opts: &ExtractOptions) -> Result<ExtractStats> {
    match format::detect(path)? {
        Format::Zip => zip_read::extract(path, opts),
        Format::Tar(c) => tar_fmt::extract(path, c, opts),
        Format::SevenZ => sevenz::extract(path, opts),
        #[cfg(feature = "rar")]
        Format::Rar => rar::extract(path, opts),
        #[cfg(not(feature = "rar"))]
        Format::Rar => bail!("RAR support not compiled in (enable feature 'rar')"),
        Format::Raw(_) => {
            let name = path.file_stem().map(|s| s.to_os_string()).unwrap_or_else(|| "data".into());
            std::fs::create_dir_all(&opts.dest)?;
            let target = opts.dest.join(name);
            let mut w = io::BufWriter::with_capacity(1 << 20, std::fs::File::create(&target)?);
            let bytes = decompress_to(path, &mut w)?;
            w.flush()?;
            Ok(ExtractStats { files: 1, bytes, ..Default::default() })
        }
    }
}

/// Stream over members' content without touching the disk.
pub fn visit(path: &Path, password: Option<&str>, v: &mut Visitor<'_>) -> Result<()> {
    match format::detect(path)? {
        Format::Zip => zip_read::visit(path, password, v),
        Format::Tar(c) => tar_fmt::visit(path, c, v),
        Format::SevenZ => sevenz::visit(path, password, v),
        #[cfg(feature = "rar")]
        Format::Rar => rar::visit(path, password, v),
        #[cfg(not(feature = "rar"))]
        Format::Rar => bail!("RAR support not compiled in (enable feature 'rar')"),
        Format::Raw(c) => {
            let e = raw_entry(path, c)?;
            if (v.filter)(&e) {
                let op = tar_fmt::open_stream(path, c)?;
                let mut r = op.reader(c)?;
                (v.each)(&e, &mut r)?;
            }
            Ok(())
        }
    }
}

/// Write one member's content to `out`. Returns bytes written.
pub fn cat(path: &Path, member: &str, password: Option<&str>, out: &mut dyn Write) -> Result<u64> {
    let want = member.trim_start_matches("./").trim_end_matches('/').to_string();
    let fmt = format::detect(path)?;
    if fmt == Format::Zip
        && let Some(n) = zip_read::cat(path, &want, password, out)?
    {
        return Ok(n);
    }
    let mut found = None;
    let is_raw = matches!(fmt, Format::Raw(_));
    let filter = |e: &Entry| is_raw || e.path.trim_start_matches("./") == want;
    let mut each = |e: &Entry, r: &mut dyn Read| -> Result<bool> {
        found = Some(copy_big(r, out)?);
        let _ = e;
        Ok(false)
    };
    visit(path, password, &mut Visitor { filter: &filter, each: &mut each })?;
    found.with_context(|| format!("'{member}' not found in {}", path.display()))
}

#[derive(Debug, Clone, Serialize)]
pub struct Info {
    pub path: String,
    pub format: String,
    pub entries: usize,
    pub files: usize,
    pub dirs: usize,
    pub links: usize,
    pub total_size: u64,
    pub archive_size: u64,
    pub ratio: f64,
    /// Listing came from an index/central directory, without decompression.
    pub instant_listing: bool,
    /// Single members can be read without decoding the whole archive.
    pub random_access: bool,
    /// Decompression can use all cores.
    pub parallel_decode: bool,
    pub encrypted_entries: usize,
}

pub fn info(path: &Path, password: Option<&str>) -> Result<Info> {
    let (format, entries) = list(path, password)?;
    info_from_entries(path, format, &entries)
}

/// Summarise an archive from a listing you already have (avoids listing twice).
pub fn info_from_entries(path: &Path, format: Format, entries: &[Entry]) -> Result<Info> {
    let archive_size = std::fs::metadata(path)?.len();
    let total: u64 = entries.iter().map(|e| e.size).sum();
    let (instant, random, par) = match format {
        Format::Zip => (true, true, true),
        Format::SevenZ => (true, false, true),
        Format::Rar => (true, false, false),
        Format::Tar(Codec::None) => (true, true, true),
        Format::Tar(c) | Format::Raw(c) => {
            let op = tar_fmt::open_stream(path, c)?;
            let indexed = op.indexed_entries().is_some();
            let multi = op.spans.as_ref().is_some_and(|s| s.len() > 1);
            (indexed, indexed, multi || c == Codec::Xz)
        }
    };
    Ok(Info {
        path: path.display().to_string(),
        format: format.name(),
        entries: entries.len(),
        files: entries.iter().filter(|e| e.kind == EntryKind::File).count(),
        dirs: entries.iter().filter(|e| e.kind == EntryKind::Dir).count(),
        links: entries.iter().filter(|e| matches!(e.kind, EntryKind::Symlink | EntryKind::Hardlink)).count(),
        total_size: total,
        archive_size,
        ratio: if total > 0 { archive_size as f64 / total as f64 } else { 0.0 },
        instant_listing: instant,
        random_access: random,
        parallel_decode: par,
        encrypted_entries: entries.iter().filter(|e| e.encrypted).count(),
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct TestReport {
    pub entries: u64,
    pub bytes: u64,
    pub errors: Vec<String>,
}

/// Decode every member and verify checksums (CRC32 for zip/7z/rar/indexed tar,
/// stream checksums for gz/zst/xz/bz2).
pub fn test(path: &Path, password: Option<&str>) -> Result<TestReport> {
    let mut rep = TestReport { entries: 0, bytes: 0, errors: Vec::new() };
    let filter = |_: &Entry| true;
    let mut each = |e: &Entry, r: &mut dyn Read| -> Result<bool> {
        let mut h = crc32fast::Hasher::new();
        let mut buf = vec![0u8; 1 << 20];
        let mut n = 0u64;
        loop {
            match r.read(&mut buf) {
                Ok(0) => break,
                Ok(k) => {
                    h.update(&buf[..k]);
                    n += k as u64;
                }
                Err(x) => {
                    rep.errors.push(format!("{}: {x}", e.path));
                    return Ok(true);
                }
            }
        }
        if e.kind == EntryKind::File
            && let Some(c) = e.crc32
            && h.finalize() != c
            && !e.encrypted
        {
            rep.errors.push(format!("{}: CRC mismatch", e.path));
        }
        rep.entries += 1;
        rep.bytes += n;
        Ok(true)
    };
    visit(path, password, &mut Visitor { filter: &filter, each: &mut each })?;
    Ok(rep)
}

/// Search member contents for a regex, without extracting.
#[derive(Debug, Clone, Serialize)]
pub struct GrepMatch {
    pub path: String,
    pub line: u64,
    pub text: String,
}

pub fn grep(
    path: &Path,
    pattern: &regex::bytes::Regex,
    members: &extract::Matcher,
    max: usize,
    password: Option<&str>,
) -> Result<Vec<GrepMatch>> {
    let mut out = Vec::new();
    let filter = |e: &Entry| e.kind == EntryKind::File && members.matches(&e.path);
    let mut each = |e: &Entry, r: &mut dyn Read| -> Result<bool> {
        let mut data = Vec::with_capacity(e.size.min(64 << 20) as usize);
        r.take(256 << 20).read_to_end(&mut data)?;
        // Skip binary content (NUL in the first 8 KiB).
        if memchr::memchr(0, &data[..data.len().min(8192)]).is_some() {
            return Ok(true);
        }
        for (i, line) in data.split(|&b| b == b'\n').enumerate() {
            if pattern.is_match(line) {
                let text = String::from_utf8_lossy(&line[..line.len().min(500)]).trim_end().to_string();
                out.push(GrepMatch { path: e.path.clone(), line: i as u64 + 1, text });
                if out.len() >= max {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    };
    visit(path, password, &mut Visitor { filter: &filter, each: &mut each })?;
    Ok(out)
}

/// Configure the global worker pool (call once, before any work).
/// Without an explicit count the pool is created lazily on first parallel use,
/// so quick commands (list, cat) never pay for spawning threads.
pub fn init_threads(n: Option<usize>) {
    if let Some(n) = n {
        let _ = rayon::ThreadPoolBuilder::new().num_threads(n.max(1)).build_global();
    }
}
