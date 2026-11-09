//! Parallel chunked compression and decompression.
//!
//! `ParallelWriter` splits a byte stream into chunks, compresses each chunk as
//! an independent unit (gzip member / zstd frame / lz4 frame / bzip2 stream) on
//! the rayon pool, and writes results strictly in order with a bounded number
//! of chunks in flight. The concatenation is a valid stream for every standard
//! decoder.
//!
//! `ParallelReader` does the reverse when chunk boundaries are known (from the
//! archivix index, zstd frame headers, or bzip2 stream magic).

use crate::codec::Codec;
use crate::index::{self, FrameRef, Index, IndexEntry};
use crossbeam_channel::{Receiver, bounded};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::Arc;

pub fn threads() -> usize {
    rayon::current_num_threads().max(1)
}

/// Default chunk size per codec: large enough that ratio loss from chunking is
/// negligible, small enough to keep every core busy.
pub fn default_chunk(codec: Codec, level: i32) -> usize {
    match codec {
        Codec::Gzip => 1 << 20,
        Codec::Zstd if level >= 17 => 32 << 20,
        Codec::Zstd if level >= 10 => 16 << 20,
        Codec::Zstd => 4 << 20,
        Codec::Lz4 => 4 << 20,
        Codec::Bzip2 => 900_000,
        _ => 4 << 20,
    }
}

thread_local! {
    static ZSTD_C: RefCell<Option<(i32, zstd::bulk::Compressor<'static>)>> = const { RefCell::new(None) };
    static ZSTD_D: RefCell<Option<zstd::bulk::Decompressor<'static>>> = const { RefCell::new(None) };
    static DEFL_C: RefCell<Option<(i32, libdeflater::Compressor)>> = const { RefCell::new(None) };
    static DEFL_D: RefCell<Option<libdeflater::Decompressor>> = const { RefCell::new(None) };
}

/// Run `f` with a thread-local libdeflate compressor at `level` (1..=12).
pub fn with_deflater<T>(level: i32, f: impl FnOnce(&mut libdeflater::Compressor) -> T) -> T {
    let level = level.clamp(1, 12);
    DEFL_C.with(|c| {
        let mut c = c.borrow_mut();
        if c.as_ref().map(|(l, _)| *l) != Some(level) {
            let lvl = libdeflater::CompressionLvl::new(level).unwrap_or_default();
            *c = Some((level, libdeflater::Compressor::new(lvl)));
        }
        f(&mut c.as_mut().unwrap().1)
    })
}

/// Compress one chunk into an independently decodable unit.
pub fn compress_chunk(codec: Codec, level: i32, data: &[u8]) -> io::Result<Vec<u8>> {
    match codec {
        Codec::Gzip => {
            // Map zlib levels 1..9 onto libdeflate 1..12 (libdeflate 6 ≈ zlib 6 but faster/smaller).
            let lvl = if level <= 0 { 1 } else { level };
            with_deflater(lvl, |c| {
                let mut out = vec![0u8; c.gzip_compress_bound(data.len())];
                let n = c.gzip_compress(data, &mut out).map_err(|e| io::Error::other(format!("{e:?}")))?;
                out.truncate(n);
                Ok(out)
            })
        }
        Codec::Zstd => ZSTD_C.with(|c| {
            let mut c = c.borrow_mut();
            if c.as_ref().map(|(l, _)| *l) != Some(level) {
                let mut comp = zstd::bulk::Compressor::new(level)?;
                comp.include_checksum(true)?;
                comp.include_contentsize(true)?;
                *c = Some((level, comp));
            }
            c.as_mut().unwrap().1.compress(data)
        }),
        Codec::Lz4 => {
            let info =
                lz4_flex::frame::FrameInfo::new().block_size(lz4_flex::frame::BlockSize::Max4MB).content_size(Some(data.len() as u64));
            let mut enc = lz4_flex::frame::FrameEncoder::with_frame_info(info, Vec::with_capacity(data.len() / 2 + 64));
            enc.write_all(data)?;
            enc.finish().map_err(io::Error::other)
        }
        Codec::Bzip2 => {
            let mut enc =
                bzip2::write::BzEncoder::new(Vec::with_capacity(data.len() / 3), bzip2::Compression::new(level.clamp(1, 9) as u32));
            enc.write_all(data)?;
            enc.finish()
        }
        other => Err(io::Error::other(format!("{} has no chunked encoder", other.name()))),
    }
}

/// Decode one independently decodable unit whose decompressed size is known.
pub fn decode_chunk(codec: Codec, src: &[u8], u_size: usize) -> io::Result<Vec<u8>> {
    match codec {
        Codec::Zstd => ZSTD_D.with(|d| {
            let mut d = d.borrow_mut();
            if d.is_none() {
                let mut dec = zstd::bulk::Decompressor::new()?;
                dec.window_log_max(31)?;
                *d = Some(dec);
            }
            d.as_mut().unwrap().decompress(src, u_size)
        }),
        Codec::Gzip => DEFL_D.with(|d| {
            let mut d = d.borrow_mut();
            let d = d.get_or_insert_with(libdeflater::Decompressor::new);
            let mut out = vec![0u8; u_size];
            match d.gzip_decompress(src, &mut out) {
                Ok(n) if n == u_size => Ok(out),
                _ => {
                    // Fall back to the streaming decoder (handles unusual headers).
                    let mut out = Vec::with_capacity(u_size);
                    flate2::read::MultiGzDecoder::new(src).read_to_end(&mut out)?;
                    Ok(out)
                }
            }
        }),
        Codec::Lz4 => {
            let mut out = Vec::with_capacity(u_size);
            lz4_flex::frame::FrameDecoder::new(src).read_to_end(&mut out)?;
            Ok(out)
        }
        Codec::Bzip2 => {
            let mut out = Vec::with_capacity(u_size.max(1 << 20));
            bzip2::read::BzDecoder::new(src).read_to_end(&mut out)?;
            Ok(out)
        }
        other => Err(io::Error::other(format!("{} has no chunked decoder", other.name()))),
    }
}

type ChunkResult = io::Result<(Vec<u8>, usize)>;

pub struct ParallelWriter<W: Write> {
    inner: W,
    codec: Codec,
    level: i32,
    chunk: usize,
    buf: Vec<u8>,
    pending: VecDeque<Receiver<ChunkResult>>,
    max_inflight: usize,
    frames: Vec<FrameRef>,
    uncompressed: u64,
}

impl<W: Write> ParallelWriter<W> {
    pub fn new(inner: W, codec: Codec, level: i32, chunk: Option<usize>) -> Self {
        let chunk = chunk.unwrap_or_else(|| default_chunk(codec, level)).max(64 * 1024);
        ParallelWriter {
            inner,
            codec,
            level,
            chunk,
            buf: Vec::with_capacity(chunk),
            pending: VecDeque::new(),
            max_inflight: threads() * 2 + 2,
            frames: Vec::new(),
            uncompressed: 0,
        }
    }

    /// Uncompressed bytes accepted so far.
    pub fn position(&self) -> u64 {
        self.uncompressed
    }

    fn submit(&mut self, data: Vec<u8>) -> io::Result<()> {
        crate::progress().check()?;
        while self.pending.len() >= self.max_inflight {
            self.drain_one()?;
        }
        let (tx, rx) = bounded(1);
        let (codec, level) = (self.codec, self.level);
        rayon::spawn(move || {
            let r = compress_chunk(codec, level, &data).map(|c| (c, data.len()));
            let _ = tx.send(r);
        });
        self.pending.push_back(rx);
        Ok(())
    }

    fn drain_one(&mut self) -> io::Result<()> {
        if let Some(rx) = self.pending.pop_front() {
            let (c, u) = rx.recv().map_err(|_| io::Error::other("compression worker died"))??;
            self.inner.write_all(&c)?;
            self.frames.push(FrameRef { c_size: c.len() as u64, u_size: u as u64 });
        }
        Ok(())
    }

    /// Flush all chunks, append the index (if requested) and return the sink.
    pub fn finish(mut self, write_index: bool, entries: Option<Vec<IndexEntry>>) -> io::Result<(W, Index)> {
        if !self.buf.is_empty() || (self.frames.is_empty() && self.pending.is_empty()) {
            let data = std::mem::take(&mut self.buf);
            self.submit(data)?;
        }
        while !self.pending.is_empty() {
            self.drain_one()?;
        }
        let index = Index { frames: std::mem::take(&mut self.frames), entries };
        if write_index {
            index::write_trailer(self.codec, &mut self.inner, &index)?;
        }
        self.inner.flush()?;
        Ok((self.inner, index))
    }
}

impl<W: Write> Write for ParallelWriter<W> {
    fn write(&mut self, mut data: &[u8]) -> io::Result<usize> {
        let total = data.len();
        while !data.is_empty() {
            let room = self.chunk - self.buf.len();
            let n = room.min(data.len());
            self.buf.extend_from_slice(&data[..n]);
            data = &data[n..];
            if self.buf.len() == self.chunk {
                let full = std::mem::replace(&mut self.buf, Vec::with_capacity(self.chunk));
                self.submit(full)?;
            }
        }
        self.uncompressed += total as u64;
        Ok(total)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Whole-buffer inflate of a standard single-member gzip with libdeflate
/// (about 2x faster than zlib-class streaming inflate). Returns None — and the
/// caller falls back to streaming — when the stream is multi-member, padded,
/// larger than the memory cap, or its ISIZE trailer is unreliable (>4 GiB).
pub fn gzip_whole(data: &[u8]) -> Option<Vec<u8>> {
    const MAX: usize = 2 << 30;
    if data.len() < 18 || data[0] != 0x1f || data[1] != 0x8b {
        return None;
    }
    let isize = u32::from_le_bytes(data[data.len() - 4..].try_into().ok()?) as usize;
    // Deflate cannot exceed ~1032:1; a smaller ISIZE means it wrapped past 4 GiB.
    if isize == 0 || isize > MAX || (isize as u64) < data.len() as u64 / 1100 {
        return None;
    }
    let mut out: Vec<u8> = Vec::with_capacity(isize);
    unsafe {
        let d = libdeflate_sys::libdeflate_alloc_decompressor();
        if d.is_null() {
            return None;
        }
        let (mut ain, mut aout) = (0usize, 0usize);
        let r = libdeflate_sys::libdeflate_gzip_decompress_ex(
            d,
            data.as_ptr() as _,
            data.len(),
            out.as_mut_ptr() as _,
            isize,
            &mut ain,
            &mut aout,
        );
        libdeflate_sys::libdeflate_free_decompressor(d);
        // Success only if the single member spans the whole file (proves it is
        // not multi-member) and produced exactly ISIZE bytes.
        if r != libdeflate_sys::libdeflate_result_LIBDEFLATE_SUCCESS || ain != data.len() || aout != isize {
            return None;
        }
        out.set_len(aout);
    }
    Some(out)
}

/// A chunk of a memory-mapped stream: offset, compressed length, decompressed length.
#[derive(Clone, Copy, Debug)]
pub struct Span {
    pub off: usize,
    pub len: usize,
    pub u_size: usize,
}

pub fn spans_from_index(index: &Index) -> Vec<Span> {
    let mut off = 0usize;
    index
        .frames
        .iter()
        .map(|f| {
            let s = Span { off, len: f.c_size as usize, u_size: f.u_size as usize };
            off += f.c_size as usize;
            s
        })
        .collect()
}

/// Find zstd frame boundaries by walking frame/block headers (no decompression).
/// Returns None when frames lack a content size (we then decode sequentially).
pub fn scan_zstd_frames(data: &[u8]) -> Option<Vec<Span>> {
    let mut spans = Vec::new();
    let mut off = 0usize;
    while off < data.len() {
        let rest = &data[off..];
        let len = zstd::zstd_safe::find_frame_compressed_size(rest).ok()?;
        if len == 0 {
            return None;
        }
        let magic = u32::from_le_bytes(rest.get(..4)?.try_into().ok()?);
        if magic & 0xFFFF_FFF0 != 0x184D_2A50 {
            let size = zstd::zstd_safe::get_frame_content_size(&rest[..len]).ok()??;
            spans.push(Span { off, len, u_size: size as usize });
        }
        off += len;
    }
    Some(spans)
}

/// Find bzip2 stream boundaries in multi-stream files (pbzip2/lbzip2/archivix).
/// Decompressed sizes are unknown (0); the decoder grows its buffer.
pub fn scan_bzip2_streams(data: &[u8]) -> Vec<Span> {
    let mut starts: Vec<usize> = Vec::new();
    // "BZh" + level + block magic 0x314159265359
    for pos in memchr::memmem::find_iter(data, b"BZh") {
        if let Some(h) = data.get(pos..pos + 10)
            && (b'1'..=b'9').contains(&h[3])
            && h[4..10] == [0x31, 0x41, 0x59, 0x26, 0x53, 0x59]
        {
            starts.push(pos);
        }
    }
    if starts.first() != Some(&0) {
        return Vec::new();
    }
    starts
        .iter()
        .enumerate()
        .map(|(i, &s)| {
            let e = starts.get(i + 1).copied().unwrap_or(data.len());
            Span { off: s, len: e - s, u_size: 0 }
        })
        .collect()
}

/// Ordered, pipelined parallel decoder over a shared byte buffer (usually an mmap).
pub struct ParallelReader<B: AsRef<[u8]> + Send + Sync + 'static> {
    data: Arc<B>,
    codec: Codec,
    spans: Vec<Span>,
    next: usize,
    /// In-flight decodes and the compressed size of each (for progress).
    pending: VecDeque<(Receiver<io::Result<Vec<u8>>>, usize)>,
    max_inflight: usize,
    cur: Vec<u8>,
    pos: usize,
}

impl<B: AsRef<[u8]> + Send + Sync + 'static> ParallelReader<B> {
    pub fn new(data: Arc<B>, codec: Codec, spans: Vec<Span>) -> Self {
        ParallelReader { data, codec, spans, next: 0, pending: VecDeque::new(), max_inflight: threads() * 2 + 2, cur: Vec::new(), pos: 0 }
    }

    fn fill(&mut self) {
        while self.pending.len() < self.max_inflight && self.next < self.spans.len() {
            let s = self.spans[self.next];
            self.next += 1;
            let (tx, rx) = bounded(1);
            let data = self.data.clone();
            let codec = self.codec;
            rayon::spawn(move || {
                let src = &(*data).as_ref()[s.off..s.off + s.len];
                let _ = tx.send(decode_chunk(codec, src, s.u_size));
            });
            self.pending.push_back((rx, s.len));
        }
    }
}

impl<B: AsRef<[u8]> + Send + Sync + 'static> Read for ParallelReader<B> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.pos >= self.cur.len() {
            self.fill();
            crate::progress().check()?;
            match self.pending.pop_front() {
                None => return Ok(0),
                Some((rx, clen)) => {
                    self.cur = rx.recv().map_err(|_| io::Error::other("decode worker died"))??;
                    self.pos = 0;
                    crate::progress().add_done(clen as u64);
                }
            }
        }
        let n = out.len().min(self.cur.len() - self.pos);
        out[..n].copy_from_slice(&self.cur[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// Decode a byte range [start, start+len) of the uncompressed stream, touching
/// only the chunks that overlap it (random access through the index).
pub fn read_range(data: &[u8], codec: Codec, spans: &[Span], start: u64, len: u64, out: &mut dyn Write) -> io::Result<()> {
    if len == 0 {
        return Ok(());
    }
    let end = start + len;
    let mut u_off = 0u64;
    let mut needed = Vec::new();
    for s in spans {
        let u_end = u_off + s.u_size as u64;
        if u_end > start && u_off < end {
            needed.push((*s, u_off));
        }
        if u_off >= end {
            break;
        }
        u_off = u_end;
    }
    use rayon::prelude::*;
    // Decode in parallel windows, write in order.
    for window in needed.chunks(threads() * 2) {
        let decoded: Vec<io::Result<Vec<u8>>> =
            window.par_iter().map(|(s, _)| decode_chunk(codec, &data[s.off..s.off + s.len], s.u_size)).collect();
        for ((_, u_start), buf) in window.iter().zip(decoded) {
            let buf = buf?;
            let lo = start.saturating_sub(*u_start) as usize;
            let hi = ((end - u_start) as usize).min(buf.len());
            if lo < hi {
                out.write_all(&buf[lo..hi])?;
            }
        }
    }
    Ok(())
}
