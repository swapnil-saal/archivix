//! tar and compressed tar (gz, zst, xz, bz2, lz4, br, lzma).

use crate::codec::{self, Codec};
use crate::entry::{Entry, EntryKind};
use crate::extract::{ExtractOptions, ExtractStats, Extractor};
use crate::index::{self, Index, IndexEntry};
use crate::parallel::{self, ParallelReader, ParallelWriter, Span};
use crate::walk::Input;
use crate::{CreateStats, Visitor};
use anyhow::{Context, Result};
use rayon::prelude::*;
use std::fs::File;
use std::io::{self, BufWriter, Cursor, Read, Write};
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct TarOptions {
    pub codec: Codec,
    pub level: i32,
    pub chunk: Option<usize>,
    /// Append the AXIX index (gz/zst/lz4 only).
    pub index: bool,
}

// ---------------------------------------------------------------- writing

pub(crate) enum Sink {
    Plain(BufWriter<File>),
    Par(ParallelWriter<BufWriter<File>>),
    Xz(liblzma::write::XzEncoder<BufWriter<File>>),
    Br(Box<brotli::CompressorWriter<BufWriter<File>>>),
}

impl Write for Sink {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        match self {
            Sink::Plain(w) => w.write(b),
            Sink::Par(w) => w.write(b),
            Sink::Xz(w) => w.write(b),
            Sink::Br(w) => w.write(b),
        }
    }
    fn write_all(&mut self, b: &[u8]) -> io::Result<()> {
        match self {
            Sink::Plain(w) => w.write_all(b),
            Sink::Par(w) => w.write_all(b),
            Sink::Xz(w) => w.write_all(b),
            Sink::Br(w) => w.write_all(b),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Sink::Plain(w) => w.flush(),
            Sink::Par(w) => w.flush(),
            // Never flush encoders mid-stream (it would cost ratio); finished explicitly.
            Sink::Xz(_) | Sink::Br(_) => Ok(()),
        }
    }
}

struct Counter<W: Write> {
    w: W,
    pos: u64,
}
impl<W: Write> Write for Counter<W> {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        let n = self.w.write(b)?;
        self.pos += n as u64;
        Ok(n)
    }
    fn write_all(&mut self, b: &[u8]) -> io::Result<()> {
        self.w.write_all(b)?;
        self.pos += b.len() as u64;
        Ok(())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.w.flush()
    }
}

struct CrcReader<R> {
    r: R,
    h: crc32fast::Hasher,
}
impl<R: Read> Read for CrcReader<R> {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        crate::progress().check()?;
        let n = self.r.read(b)?;
        self.h.update(&b[..n]);
        crate::progress().add(n as u64);
        Ok(n)
    }
}

pub(crate) fn open_sink(output: &Path, opts: &TarOptions) -> Result<Sink> {
    let f = File::create(output).with_context(|| format!("create {}", output.display()))?;
    let w = BufWriter::with_capacity(4 << 20, f);
    let level = opts.codec.clamp_level(opts.level);
    Ok(match opts.codec {
        Codec::None => Sink::Plain(w),
        c if c.parallel_chunks() => Sink::Par(ParallelWriter::new(w, c, level, opts.chunk)),
        Codec::Xz => {
            let mut b = liblzma::stream::MtStreamBuilder::new();
            b.preset(level as u32).check(liblzma::stream::Check::Crc64).threads(parallel::threads() as u32);
            if let Some(c) = opts.chunk {
                b.block_size(c as u64);
            }
            Sink::Xz(liblzma::write::XzEncoder::new_stream(w, b.encoder()?))
        }
        Codec::Lzma => {
            let o = liblzma::stream::LzmaOptions::new_preset(level as u32)?;
            Sink::Xz(liblzma::write::XzEncoder::new_stream(w, liblzma::stream::Stream::new_lzma_encoder(&o)?))
        }
        Codec::Brotli => Sink::Br(Box::new(brotli::CompressorWriter::new(w, 1 << 20, level as u32, 24))),
        _ => unreachable!(),
    })
}

/// Files up to this size are prefetched in parallel batches.
const PREFETCH_MAX: u64 = 1 << 20;
const BATCH: usize = 256;

pub fn create(output: &Path, inputs: &[Input], opts: &TarOptions) -> Result<CreateStats> {
    let sink = open_sink(output, opts)?;
    let mut counter = Counter { w: sink, pos: 0 };
    let mut idx_entries: Vec<IndexEntry> = Vec::with_capacity(inputs.len());
    let mut stats = CreateStats::default();
    {
        let mut b = tar::Builder::new(&mut counter);
        b.mode(tar::HeaderMode::Complete);
        for batch in inputs.chunks(BATCH) {
            // Read small files of the batch concurrently; tar order is preserved.
            let pre: Vec<Option<io::Result<Vec<u8>>>> =
                batch.par_iter().map(|i| (i.kind == EntryKind::File && i.size <= PREFETCH_MAX).then(|| std::fs::read(&i.src))).collect();
            for (inp, data) in batch.iter().zip(pre) {
                let mut h = tar::Header::new_gnu();
                h.set_mtime(inp.mtime.max(0) as u64);
                h.set_mode(inp.mode);
                h.set_uid(0);
                h.set_gid(0);
                let (crc, size, link) = match inp.kind {
                    EntryKind::Dir => {
                        h.set_entry_type(tar::EntryType::Directory);
                        h.set_size(0);
                        b.append_data(&mut h, format!("{}/", inp.name), io::empty())?;
                        (0, 0, None)
                    }
                    EntryKind::Symlink => {
                        let t = inp.link_target.clone().unwrap_or_default();
                        h.set_entry_type(tar::EntryType::Symlink);
                        h.set_size(0);
                        b.append_link(&mut h, &inp.name, &t)?;
                        (0, 0, Some(t))
                    }
                    _ => {
                        h.set_entry_type(tar::EntryType::Regular);
                        match data {
                            Some(d) => {
                                let d = d.with_context(|| format!("read {}", inp.src.display()))?;
                                h.set_size(d.len() as u64);
                                b.append_data(&mut h, &inp.name, &d[..])?;
                                crate::progress().add(d.len() as u64);
                                (crc32fast::hash(&d), d.len() as u64, None)
                            }
                            None => {
                                let f = File::open(&inp.src).with_context(|| format!("open {}", inp.src.display()))?;
                                let len = f.metadata()?.len();
                                h.set_size(len);
                                let mut r = CrcReader { r: f.take(len), h: crc32fast::Hasher::new() };
                                b.append_data(&mut h, &inp.name, &mut r)?;
                                (r.h.finalize(), len, None)
                            }
                        }
                    }
                };
                stats.entries += 1;
                stats.input_bytes += size;
                crate::progress().add_entry();
                crate::progress().check()?;
                let pos = b.get_ref().pos;
                let data_off = pos - size.div_ceil(512) * 512;
                idx_entries.push(IndexEntry {
                    path: inp.name.clone(),
                    kind: inp.kind,
                    size,
                    mtime: inp.mtime,
                    mode: inp.mode,
                    link,
                    data_off,
                    crc32: crc,
                });
            }
        }
        b.finish()?;
    }
    let Counter { w: sink, .. } = counter;
    finish_sink(sink, opts.index, opts.index.then_some(idx_entries))?;
    stats.output_bytes = std::fs::metadata(output)?.len();
    Ok(stats)
}

pub(crate) fn finish_sink(sink: Sink, index: bool, entries: Option<Vec<IndexEntry>>) -> Result<()> {
    match sink {
        Sink::Plain(mut w) => w.flush()?,
        Sink::Par(p) => {
            p.finish(index, entries)?;
        }
        Sink::Xz(e) => e.finish()?.flush()?,
        Sink::Br(e) => e.into_inner().flush()?,
    }
    Ok(())
}

// ---------------------------------------------------------------- reading

/// Owning byte buffer over a shared mmap (so readers can be 'static).
#[derive(Clone)]
pub struct Shared(pub Arc<memmap2::Mmap>, pub usize);
impl AsRef<[u8]> for Shared {
    fn as_ref(&self) -> &[u8] {
        &self.0[..self.1]
    }
}

pub fn map_file(path: &Path) -> Result<Arc<memmap2::Mmap>> {
    let f = File::open(path).with_context(|| format!("open {}", path.display()))?;
    Ok(Arc::new(unsafe { memmap2::Mmap::map(&f)? }))
}

/// Everything known about a compressed stream before decoding it.
pub struct Opened {
    pub map: Arc<memmap2::Mmap>,
    pub index: Option<Index>,
    pub spans: Option<Vec<Span>>,
    pub data_end: usize,
}

pub fn open_stream(path: &Path, codec: Codec) -> Result<Opened> {
    let map = map_file(path)?;
    let mut data_end = map.len();
    let mut index = None;
    let mut spans = None;
    match codec {
        Codec::Gzip | Codec::Zstd | Codec::Lz4 => {
            if let Some((idx, end)) = index::read_trailer(codec, &map) {
                data_end = end;
                spans = Some(parallel::spans_from_index(&idx));
                index = Some(idx);
            } else if codec == Codec::Zstd {
                spans = parallel::scan_zstd_frames(&map);
            }
        }
        Codec::Bzip2 => {
            let s = parallel::scan_bzip2_streams(&map);
            if s.len() > 1 {
                spans = Some(s);
            }
        }
        _ => {}
    }
    Ok(Opened { map, index, spans, data_end })
}

impl Opened {
    /// A decoded byte stream: parallel when chunk boundaries are known.
    pub fn reader(&self, codec: Codec) -> Result<Box<dyn Read + Send>> {
        use crate::progress::{Count, Counted};
        // Progress: fraction = compressed bytes consumed; payload = decoded bytes.
        let p = crate::progress();
        p.set_total(self.data_end as u64);
        if let Some(entries) = self.indexed_entries() {
            p.set_total_entries(entries.iter().filter(|e| e.kind == EntryKind::File).count() as u64);
        }
        let shared = Shared(self.map.clone(), self.data_end);
        if let Some(spans) = &self.spans
            && spans.len() > 1
        {
            return Ok(Box::new(Counted::new(ParallelReader::new(Arc::new(shared), codec, spans.clone()), Count::Bytes)));
        }
        if let Some(v) = self.whole(codec) {
            // Decoded in one fast step: track progress through the decoded bytes.
            p.set_total(v.len() as u64);
            return Ok(Box::new(Counted::new(Cursor::new(v), Count::Both)));
        }
        let decoded = self.sequential(codec, Counted::new(Cursor::new(shared), Count::Done))?;
        Ok(Box::new(Counted::new(decoded, Count::Bytes)))
    }

    fn sequential(&self, codec: Codec, src: crate::progress::Counted<Cursor<Shared>>) -> Result<Box<dyn Read + Send>> {
        let shared = src;
        Ok(match codec {
            Codec::None => Box::new(shared),
            Codec::Gzip => Box::new(flate2::read::MultiGzDecoder::new(shared)),
            Codec::Bzip2 => Box::new(bzip2::read::MultiBzDecoder::new(shared)),
            Codec::Xz => {
                Box::new(liblzma::bufread::XzDecoder::new_stream(io::BufReader::with_capacity(1 << 20, shared), codec::xz_mt_decoder()?))
            }
            Codec::Lzma => {
                let s = liblzma::stream::Stream::new_lzma_decoder(u64::MAX)?;
                Box::new(liblzma::read::XzDecoder::new_stream(shared, s))
            }
            Codec::Zstd => {
                let mut d = zstd::stream::read::Decoder::new(shared)?;
                d.window_log_max(31)?;
                Box::new(d)
            }
            Codec::Lz4 => Box::new(codec::Lz4Multi::new(io::BufReader::with_capacity(1 << 20, shared))),
            Codec::Brotli => Box::new(brotli::Decompressor::new(shared, 1 << 20)),
        })
    }

    /// Fully decoded stream in memory, when a faster whole-buffer path applies.
    pub fn whole(&self, codec: Codec) -> Option<Vec<u8>> {
        if codec == Codec::Gzip && self.spans.is_none() {
            return parallel::gzip_whole(&self.map[..self.data_end]);
        }
        None
    }

    pub fn indexed_entries(&self) -> Option<&Vec<IndexEntry>> {
        self.index.as_ref().and_then(|i| i.entries.as_ref())
    }
}

fn tar_entry<R: Read>(e: &tar::Entry<'_, R>) -> Result<Entry> {
    let h = e.header();
    let path = e.path()?.to_string_lossy().trim_end_matches('/').to_string();
    let link = e.link_name()?.map(|l| l.to_string_lossy().into_owned());
    let kind = match h.entry_type() {
        tar::EntryType::Regular | tar::EntryType::Continuous | tar::EntryType::GNUSparse => EntryKind::File,
        tar::EntryType::Directory => EntryKind::Dir,
        tar::EntryType::Symlink => EntryKind::Symlink,
        tar::EntryType::Link => EntryKind::Hardlink,
        _ => EntryKind::Other,
    };
    let mut out = Entry::new(path, kind, e.size());
    out.mtime = h.mtime().ok().map(|t| t as i64);
    out.mode = h.mode().ok().map(|m| m & 0o7777);
    out.link_target = link;
    Ok(out)
}

fn is_metadata_only(kind: EntryKind) -> bool {
    !matches!(kind, EntryKind::File | EntryKind::Other)
}

/// macOS tar stores xattrs as an AppleDouble `._name` member right before
/// `name`; bsdtar hides those in listings on macOS, so do we.
fn hide_appledouble(entries: Vec<Entry>) -> Vec<Entry> {
    if !cfg!(target_os = "macos") {
        return entries;
    }
    let companion = |e: &Entry| -> Option<String> {
        let (dir, base) = match e.path.rsplit_once('/') {
            Some((d, b)) => (Some(d), b),
            None => (None, e.path.as_str()),
        };
        let real = base.strip_prefix("._")?;
        Some(match dir {
            Some(d) => format!("{d}/{real}"),
            None => real.to_string(),
        })
    };
    let mut out = Vec::with_capacity(entries.len());
    for i in 0..entries.len() {
        if let Some(c) = companion(&entries[i])
            && entries.get(i + 1).is_some_and(|n| n.path == c)
        {
            continue;
        }
        out.push(entries[i].clone());
    }
    out
}

pub fn list(path: &Path, codec: Codec) -> Result<Vec<Entry>> {
    list_raw(path, codec).map(hide_appledouble)
}

fn list_raw(path: &Path, codec: Codec) -> Result<Vec<Entry>> {
    if codec == Codec::None {
        // Plain tar: seek over member data instead of reading it.
        let mut ar = tar::Archive::new(io::BufReader::new(File::open(path)?));
        let mut out = Vec::new();
        for e in ar.entries_with_seek()? {
            out.push(tar_entry(&e?)?);
        }
        return Ok(out);
    }
    let op = open_stream(path, codec)?;
    if let Some(entries) = op.indexed_entries() {
        return Ok(entries.iter().map(|e| e.to_entry()).collect());
    }
    if let Some(v) = op.whole(codec) {
        // In memory: seek over member data instead of copying it.
        let mut ar = tar::Archive::new(Cursor::new(v));
        let mut out = Vec::new();
        for e in ar.entries_with_seek()? {
            out.push(tar_entry(&e?)?);
        }
        return Ok(out);
    }
    let mut ar = tar::Archive::new(op.reader(codec)?);
    let mut out = Vec::new();
    for e in ar.entries()? {
        out.push(tar_entry(&e?)?);
    }
    Ok(out)
}

pub fn extract(path: &Path, codec: Codec, opts: &ExtractOptions) -> Result<ExtractStats> {
    let ex = Extractor::new(opts.clone())?;
    let op = open_stream(path, codec)?;
    // Selective extraction from an indexed archive: decode only what is needed.
    if !opts.patterns.is_empty()
        && let (Some(entries), Some(spans)) = (op.indexed_entries(), op.spans.as_ref())
    {
        let data = &op.map[..op.data_end];
        let wanted: Vec<&IndexEntry> = entries.iter().filter(|e| ex.wants(&e.path)).collect();
        crate::progress().set_total(wanted.iter().map(|e| e.size).sum());
        crate::progress().set_total_entries(wanted.iter().filter(|e| e.kind == EntryKind::File).count() as u64);
        wanted.par_iter().try_for_each(|ie| -> Result<()> {
            let e = ie.to_entry();
            if is_metadata_only(e.kind) {
                return ex.extract_entry(&e, &mut io::empty());
            }
            let mut buf = Vec::with_capacity(ie.size as usize);
            parallel::read_range(data, codec, spans, ie.data_off, ie.size, &mut buf)?;
            crate::progress().add(ie.size);
            ex.extract_entry(&e, &mut &buf[..])
        })?;
        return ex.finish();
    }
    let mut ar = tar::Archive::new(op.reader(codec)?);
    for e in ar.entries()? {
        let mut e = e?;
        let meta = tar_entry(&e)?;
        ex.extract_entry(&meta, &mut e).with_context(|| format!("extract {}", meta.path))?;
    }
    ex.finish()
}

pub fn visit(path: &Path, codec: Codec, v: &mut Visitor<'_>) -> Result<()> {
    let op = open_stream(path, codec)?;
    if let (Some(entries), Some(spans)) = (op.indexed_entries(), op.spans.as_ref()) {
        let data = &op.map[..op.data_end];
        let selected: Vec<&IndexEntry> = entries.iter().filter(|e| (v.filter)(&e.to_entry())).collect();
        // Random access pays off when only part of the archive is wanted.
        if selected.len() * 2 < entries.len() {
            for ie in selected {
                let e = ie.to_entry();
                let mut buf = Vec::with_capacity(ie.size as usize);
                if !is_metadata_only(e.kind) {
                    parallel::read_range(data, codec, spans, ie.data_off, ie.size, &mut buf)?;
                }
                if !(v.each)(&e, &mut &buf[..])? {
                    break;
                }
            }
            return Ok(());
        }
    }
    let mut ar = tar::Archive::new(op.reader(codec)?);
    for e in ar.entries()? {
        let mut e = e?;
        let meta = tar_entry(&e)?;
        if !(v.filter)(&meta) {
            continue;
        }
        if !(v.each)(&meta, &mut e)? {
            break;
        }
    }
    Ok(())
}

pub fn decoder_for(codec: Codec, r: File) -> io::Result<Box<dyn Read>> {
    codec::decoder(codec, r)
}
