//! Parallel ZIP writer.
//!
//! * Small files are read and compressed concurrently on every core and written
//!   in completion order (ZIP allows any order: the central directory holds
//!   the offsets), so there is no head-of-line blocking.
//! * Large files are split into chunks compressed in parallel. Deflate chunks
//!   are primed with the previous 32 KiB (pigz-style) and joined with sync
//!   flushes into one standard deflate stream; CRCs are computed per chunk and
//!   combined. Any unzip can read the result.
//! * Incompressible data (media, archives, random bytes) is detected by
//!   extension and an entropy probe and stored instead of burning CPU.
//! * ZIP64 is emitted automatically when sizes, offsets or counts need it.

use crate::CreateStats;
use crate::entry::EntryKind;
use crate::parallel::{self, threads};
use crate::walk::{self, Input};
use anyhow::{Context, Result};
use rayon::prelude::*;
use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZipMethod {
    Store,
    Deflate,
    Zstd,
    Bzip2,
    Xz,
}

impl ZipMethod {
    pub fn parse(s: &str) -> Option<ZipMethod> {
        Some(match s.to_ascii_lowercase().as_str() {
            "store" | "stored" | "none" => ZipMethod::Store,
            "deflate" | "gzip" | "gz" => ZipMethod::Deflate,
            "zstd" | "zst" => ZipMethod::Zstd,
            "bzip2" | "bz2" => ZipMethod::Bzip2,
            "xz" | "lzma" => ZipMethod::Xz,
            _ => return None,
        })
    }
    fn id(self) -> u16 {
        match self {
            ZipMethod::Store => 0,
            ZipMethod::Deflate => 8,
            ZipMethod::Bzip2 => 12,
            ZipMethod::Zstd => 93,
            ZipMethod::Xz => 95,
        }
    }
    fn version_needed(self) -> u16 {
        match self {
            ZipMethod::Store | ZipMethod::Deflate => 20,
            ZipMethod::Bzip2 => 46,
            ZipMethod::Zstd | ZipMethod::Xz => 63,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ZipOptions {
    pub method: ZipMethod,
    pub level: i32,
    /// Store data that would not compress.
    pub smart_store: bool,
}

const LARGE: u64 = 8 << 20;
const CHUNK: usize = 2 << 20;

struct CdRecord {
    name: Vec<u8>,
    method: u16,
    version_needed: u16,
    crc: u32,
    csize: u64,
    usize: u64,
    offset: u64,
    mtime: i64,
    ext_attr: u32,
}

struct Prepared {
    idx: usize,
    method: ZipMethod,
    crc: u32,
    usize: u64,
    data: Vec<u8>,
}

struct ZipOut {
    w: BufWriter<File>,
    pos: u64,
    cd: Vec<CdRecord>,
}

fn dos_datetime(t: i64) -> (u16, u16) {
    #[cfg(unix)]
    let tm = unsafe {
        let tt: libc::time_t = t as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&tt, &mut tm);
        (tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday, tm.tm_hour, tm.tm_min, tm.tm_sec)
    };
    #[cfg(not(unix))]
    let tm = crate::util::civil_from_unix(t);
    let (y, mo, d, h, mi, s) = tm;
    if y < 1980 {
        return (0, (1 << 5) | 1);
    }
    let y = y.min(2107);
    let time = ((h as u16) << 11) | ((mi as u16) << 5) | ((s as u16) / 2);
    let date = (((y - 1980) as u16) << 9) | ((mo as u16) << 5) | d as u16;
    (time, date)
}

fn ext_attr(kind: EntryKind, mode: u32) -> u32 {
    let (ty, dosbit) = match kind {
        EntryKind::Dir => (0o040000, 0x10),
        EntryKind::Symlink => (0o120000, 0),
        _ => (0o100000, 0),
    };
    ((ty | (mode & 0o7777)) << 16) | dosbit
}

impl ZipOut {
    // One argument per on-disk header field; a struct would only rename them.
    #[allow(clippy::too_many_arguments)]
    fn local_header(&mut self, name: &[u8], method: ZipMethod, crc: u32, csize: u64, usize: u64, mtime: i64, zip64: bool) -> Result<u64> {
        let offset = self.pos;
        let (t, d) = dos_datetime(mtime);
        let mut h = Vec::with_capacity(30 + name.len() + 29);
        h.extend_from_slice(&0x04034b50u32.to_le_bytes());
        let vn = if zip64 { method.version_needed().max(45) } else { method.version_needed() };
        h.extend_from_slice(&vn.to_le_bytes());
        h.extend_from_slice(&0x0800u16.to_le_bytes()); // UTF-8 names
        h.extend_from_slice(&method.id().to_le_bytes());
        h.extend_from_slice(&t.to_le_bytes());
        h.extend_from_slice(&d.to_le_bytes());
        h.extend_from_slice(&crc.to_le_bytes());
        if zip64 {
            h.extend_from_slice(&u32::MAX.to_le_bytes());
            h.extend_from_slice(&u32::MAX.to_le_bytes());
        } else {
            h.extend_from_slice(&(csize as u32).to_le_bytes());
            h.extend_from_slice(&(usize as u32).to_le_bytes());
        }
        h.extend_from_slice(&(name.len() as u16).to_le_bytes());
        let extra_len: u16 = 9 + if zip64 { 20 } else { 0 };
        h.extend_from_slice(&extra_len.to_le_bytes());
        h.extend_from_slice(name);
        // Extended timestamp (exact UTC mtime)
        h.extend_from_slice(&0x5455u16.to_le_bytes());
        h.extend_from_slice(&5u16.to_le_bytes());
        h.push(1);
        h.extend_from_slice(&(mtime.clamp(0, u32::MAX as i64) as u32).to_le_bytes());
        if zip64 {
            h.extend_from_slice(&0x0001u16.to_le_bytes());
            h.extend_from_slice(&16u16.to_le_bytes());
            h.extend_from_slice(&usize.to_le_bytes());
            h.extend_from_slice(&csize.to_le_bytes());
        }
        self.w.write_all(&h)?;
        self.pos += h.len() as u64;
        Ok(offset)
    }

    fn write_whole(&mut self, inp: &Input, method: ZipMethod, crc: u32, usize: u64, data: &[u8]) -> Result<()> {
        let mut name = inp.name.clone();
        if inp.kind == EntryKind::Dir {
            name.push('/');
        }
        let csize = data.len() as u64;
        let zip64 = usize >= u32::MAX as u64 || csize >= u32::MAX as u64;
        let offset = self.local_header(name.as_bytes(), method, crc, csize, usize, inp.mtime, zip64)?;
        self.w.write_all(data)?;
        self.pos += csize;
        self.cd.push(CdRecord {
            name: name.into_bytes(),
            method: method.id(),
            version_needed: if zip64 { method.version_needed().max(45) } else { method.version_needed() },
            crc,
            csize,
            usize,
            offset,
            mtime: inp.mtime,
            ext_attr: ext_attr(inp.kind, inp.mode),
        });
        Ok(())
    }

    fn finish(mut self) -> Result<u64> {
        let cd_start = self.pos;
        let count = self.cd.len() as u64;
        let mut buf = Vec::with_capacity(self.cd.len() * 80);
        for r in &self.cd {
            let need_u = r.usize >= u32::MAX as u64;
            let need_c = r.csize >= u32::MAX as u64;
            let need_o = r.offset >= u32::MAX as u64;
            let z64_len = 8 * (need_u as u16 + need_c as u16 + need_o as u16);
            let (t, d) = dos_datetime(r.mtime);
            buf.extend_from_slice(&0x02014b50u32.to_le_bytes());
            buf.extend_from_slice(&((3u16 << 8) | 63).to_le_bytes()); // made by: Unix, spec 6.3
            let vn = if z64_len > 0 { r.version_needed.max(45) } else { r.version_needed };
            buf.extend_from_slice(&vn.to_le_bytes());
            buf.extend_from_slice(&0x0800u16.to_le_bytes());
            buf.extend_from_slice(&r.method.to_le_bytes());
            buf.extend_from_slice(&t.to_le_bytes());
            buf.extend_from_slice(&d.to_le_bytes());
            buf.extend_from_slice(&r.crc.to_le_bytes());
            buf.extend_from_slice(&(if need_c { u32::MAX } else { r.csize as u32 }).to_le_bytes());
            buf.extend_from_slice(&(if need_u { u32::MAX } else { r.usize as u32 }).to_le_bytes());
            buf.extend_from_slice(&(r.name.len() as u16).to_le_bytes());
            let extra_len = 9 + if z64_len > 0 { 4 + z64_len } else { 0 };
            buf.extend_from_slice(&extra_len.to_le_bytes());
            buf.extend_from_slice(&0u16.to_le_bytes()); // comment
            buf.extend_from_slice(&0u16.to_le_bytes()); // disk
            buf.extend_from_slice(&0u16.to_le_bytes()); // internal attr
            buf.extend_from_slice(&r.ext_attr.to_le_bytes());
            buf.extend_from_slice(&(if need_o { u32::MAX } else { r.offset as u32 }).to_le_bytes());
            buf.extend_from_slice(&r.name);
            buf.extend_from_slice(&0x5455u16.to_le_bytes());
            buf.extend_from_slice(&5u16.to_le_bytes());
            buf.push(1);
            buf.extend_from_slice(&(r.mtime.clamp(0, u32::MAX as i64) as u32).to_le_bytes());
            if z64_len > 0 {
                buf.extend_from_slice(&0x0001u16.to_le_bytes());
                buf.extend_from_slice(&z64_len.to_le_bytes());
                if need_u {
                    buf.extend_from_slice(&r.usize.to_le_bytes());
                }
                if need_c {
                    buf.extend_from_slice(&r.csize.to_le_bytes());
                }
                if need_o {
                    buf.extend_from_slice(&r.offset.to_le_bytes());
                }
            }
        }
        self.w.write_all(&buf)?;
        let cd_size = buf.len() as u64;
        let mut end = Vec::with_capacity(98);
        let need64 = count >= 0xFFFF || cd_size >= u32::MAX as u64 || cd_start >= u32::MAX as u64;
        if need64 {
            let z64_eocd = cd_start + cd_size;
            end.extend_from_slice(&0x06064b50u32.to_le_bytes());
            end.extend_from_slice(&44u64.to_le_bytes());
            end.extend_from_slice(&((3u16 << 8) | 63).to_le_bytes());
            end.extend_from_slice(&45u16.to_le_bytes());
            end.extend_from_slice(&0u32.to_le_bytes());
            end.extend_from_slice(&0u32.to_le_bytes());
            end.extend_from_slice(&count.to_le_bytes());
            end.extend_from_slice(&count.to_le_bytes());
            end.extend_from_slice(&cd_size.to_le_bytes());
            end.extend_from_slice(&cd_start.to_le_bytes());
            end.extend_from_slice(&0x07064b50u32.to_le_bytes());
            end.extend_from_slice(&0u32.to_le_bytes());
            end.extend_from_slice(&z64_eocd.to_le_bytes());
            end.extend_from_slice(&1u32.to_le_bytes());
        }
        end.extend_from_slice(&0x06054b50u32.to_le_bytes());
        end.extend_from_slice(&0u16.to_le_bytes());
        end.extend_from_slice(&0u16.to_le_bytes());
        let c16 = if need64 { 0xFFFF } else { count as u16 };
        end.extend_from_slice(&c16.to_le_bytes());
        end.extend_from_slice(&c16.to_le_bytes());
        end.extend_from_slice(&(if need64 { u32::MAX } else { cd_size as u32 }).to_le_bytes());
        end.extend_from_slice(&(if need64 { u32::MAX } else { cd_start as u32 }).to_le_bytes());
        end.extend_from_slice(&0u16.to_le_bytes());
        self.w.write_all(&end)?;
        self.w.flush()?;
        Ok(cd_start + cd_size + end.len() as u64)
    }
}

fn compress_whole(method: ZipMethod, level: i32, data: &[u8]) -> Result<Vec<u8>> {
    Ok(match method {
        ZipMethod::Store => data.to_vec(),
        ZipMethod::Deflate => parallel::with_deflater(level, |c| -> Result<Vec<u8>> {
            let mut out = vec![0u8; c.deflate_compress_bound(data.len())];
            let n = c.deflate_compress(data, &mut out).map_err(|e| anyhow::anyhow!("{e:?}"))?;
            out.truncate(n);
            Ok(out)
        })?,
        ZipMethod::Zstd => parallel::compress_chunk(crate::codec::Codec::Zstd, level, data)?,
        ZipMethod::Bzip2 => parallel::compress_chunk(crate::codec::Codec::Bzip2, level, data)?,
        ZipMethod::Xz => {
            let mut e = liblzma::write::XzEncoder::new(Vec::new(), level.clamp(0, 9) as u32);
            e.write_all(data)?;
            e.finish()?
        }
    })
}

fn prepare_small(idx: usize, inp: &Input, opts: &ZipOptions) -> Result<Prepared> {
    crate::progress().check()?;
    let data = std::fs::read(&inp.src).with_context(|| format!("read {}", inp.src.display()))?;
    let crc = crc32fast::hash(&data);
    let usize = data.len() as u64;
    let mut method = opts.method;
    if data.is_empty() || (opts.smart_store && (walk::is_precompressed_ext(&inp.name) || walk::looks_incompressible(&data))) {
        method = ZipMethod::Store;
    }
    if method == ZipMethod::Store {
        return Ok(Prepared { idx, method, crc, usize, data });
    }
    let c = compress_whole(method, opts.level, &data)?;
    if c.len() as u64 >= usize {
        return Ok(Prepared { idx, method: ZipMethod::Store, crc, usize, data });
    }
    Ok(Prepared { idx, method, crc, usize, data: c })
}

/// Deflate one chunk primed with `dict`; non-final chunks end with a sync flush
/// so the chunks concatenate into one valid deflate stream.
fn deflate_chunk(level: i32, dict: &[u8], data: &[u8], last: bool) -> std::io::Result<Vec<u8>> {
    use flate2::{Compress, Compression, FlushCompress, Status};
    let mut c = Compress::new(Compression::new(level.clamp(0, 9) as u32), false);
    if !dict.is_empty() {
        c.set_dictionary(dict).map_err(std::io::Error::other)?;
    }
    let mut out: Vec<u8> = Vec::with_capacity(data.len() / 2 + 4096);
    let flush = if last { FlushCompress::Finish } else { FlushCompress::Sync };
    let mut input = data;
    loop {
        if out.capacity() - out.len() < 32 * 1024 {
            out.reserve(out.capacity().max(64 * 1024));
        }
        let before = c.total_in();
        let st = c.compress_vec(input, &mut out, flush).map_err(std::io::Error::other)?;
        input = &input[(c.total_in() - before) as usize..];
        if st == Status::StreamEnd {
            break;
        }
        if !last && input.is_empty() && out.len() < out.capacity() {
            break;
        }
    }
    Ok(out)
}

fn write_large(out: &mut ZipOut, inp: &Input, opts: &ZipOptions) -> Result<()> {
    let file = File::open(&inp.src).with_context(|| format!("open {}", inp.src.display()))?;
    let map = unsafe { memmap2::Mmap::map(&file)? };
    let data: &[u8] = &map;
    let usize = data.len() as u64;
    let mut method = opts.method;
    if opts.smart_store {
        let probe = |at: usize| walk::looks_incompressible(&data[at..(at + 65536).min(data.len())]);
        if walk::is_precompressed_ext(&inp.name) || (probe(0) && probe(data.len() / 2) && probe(data.len().saturating_sub(65536))) {
            method = ZipMethod::Store;
        }
    }
    // bzip2/xz in zip cannot be chunked safely for all readers: compress whole.
    if matches!(method, ZipMethod::Bzip2 | ZipMethod::Xz) {
        let c = compress_whole(method, opts.level, data)?;
        return out.write_whole(inp, method, crc32fast::hash(data), usize, &c);
    }
    let zip64 = usize > 0xFFFF_0000;
    let name = inp.name.as_bytes().to_vec();
    let offset = out.local_header(&name, method, 0, 0, usize, inp.mtime, zip64)?;
    let header_len = out.pos - offset;
    let chunks: Vec<(usize, usize)> = (0..data.len()).step_by(CHUNK).map(|s| (s, (s + CHUNK).min(data.len()))).collect();
    let nchunks = chunks.len();
    let mut hasher = crc32fast::Hasher::new();
    let mut csize = 0u64;
    let level = opts.level;
    for (wi, window) in chunks.chunks(threads() * 2).enumerate() {
        let base = wi * threads() * 2;
        let results: Vec<std::io::Result<(Vec<u8>, u32)>> = window
            .par_iter()
            .enumerate()
            .map(|(k, &(s, e))| {
                let chunk = &data[s..e];
                let crc = crc32fast::hash(chunk);
                let last = base + k + 1 == nchunks;
                let c = match method {
                    ZipMethod::Store => Vec::new(),
                    ZipMethod::Deflate => deflate_chunk(level, &data[s.saturating_sub(32768)..s], chunk, last)?,
                    _ => parallel::compress_chunk(crate::codec::Codec::Zstd, level, chunk)?,
                };
                Ok((c, crc))
            })
            .collect();
        for ((s, e), r) in window.iter().zip(results) {
            let (c, crc) = r?;
            hasher.combine(&crc32fast::Hasher::new_with_initial_len(crc, (e - s) as u64));
            let bytes: &[u8] = if method == ZipMethod::Store { &data[*s..*e] } else { &c };
            out.w.write_all(bytes)?;
            csize += bytes.len() as u64;
            crate::progress().add((e - s) as u64);
            crate::progress().check()?;
        }
    }
    if nchunks == 0 && method == ZipMethod::Deflate {
        let c = deflate_chunk(level, &[], &[], true)?;
        out.w.write_all(&c)?;
        csize += c.len() as u64;
    }
    out.pos += csize;
    let crc = hasher.finalize();
    // Patch the local header with the real CRC and sizes.
    out.w.flush()?;
    let f = out.w.get_mut();
    f.seek(SeekFrom::Start(offset + 14))?;
    f.write_all(&crc.to_le_bytes())?;
    if zip64 {
        f.seek(SeekFrom::Start(offset + header_len - 16))?;
        f.write_all(&usize.to_le_bytes())?;
        f.write_all(&csize.to_le_bytes())?;
    } else {
        f.write_all(&(csize as u32).to_le_bytes())?;
        f.write_all(&(usize as u32).to_le_bytes())?;
    }
    f.seek(SeekFrom::Start(out.pos))?;
    out.cd.push(CdRecord {
        name,
        method: method.id(),
        version_needed: if zip64 { method.version_needed().max(45) } else { method.version_needed() },
        crc,
        csize,
        usize,
        offset,
        mtime: inp.mtime,
        ext_attr: ext_attr(inp.kind, inp.mode),
    });
    Ok(())
}

pub fn create(output: &Path, inputs: &[Input], opts: &ZipOptions) -> Result<CreateStats> {
    let file = File::create(output).with_context(|| format!("create {}", output.display()))?;
    let mut out = ZipOut { w: BufWriter::with_capacity(4 << 20, file), pos: 0, cd: Vec::with_capacity(inputs.len()) };
    let mut stats = CreateStats::default();

    // Directories and symlinks first (no compression work).
    for inp in inputs {
        match inp.kind {
            EntryKind::Dir => out.write_whole(inp, ZipMethod::Store, 0, 0, &[])?,
            EntryKind::Symlink => {
                let t = inp.link_target.clone().unwrap_or_default().into_bytes();
                out.write_whole(inp, ZipMethod::Store, crc32fast::hash(&t), t.len() as u64, &t)?;
            }
            _ => continue,
        }
        stats.entries += 1;
        crate::progress().add_entry();
    }

    let small: Vec<usize> = (0..inputs.len()).filter(|&i| inputs[i].kind == EntryKind::File && inputs[i].size < LARGE).collect();
    let large: Vec<usize> = (0..inputs.len()).filter(|&i| inputs[i].kind == EntryKind::File && inputs[i].size >= LARGE).collect();

    // Small files: parallel read+compress, written in completion order.
    std::thread::scope(|s| -> Result<()> {
        let (tx, rx) = crossbeam_channel::bounded::<Result<Prepared>>(threads() * 4);
        let small = &small;
        s.spawn(move || {
            small.par_iter().for_each_with(tx, |tx, &i| {
                let _ = tx.send(prepare_small(i, &inputs[i], opts));
            });
        });
        for p in rx {
            let p = p?;
            let inp = &inputs[p.idx];
            stats.input_bytes += p.usize;
            out.write_whole(inp, p.method, p.crc, p.usize, &p.data)?;
            stats.entries += 1;
            crate::progress().add(p.usize);
            crate::progress().add_entry();
        }
        Ok(())
    })?;

    // Large files: one at a time, each chunk-parallel across all cores.
    for &i in &large {
        write_large(&mut out, &inputs[i], opts)?;
        stats.input_bytes += inputs[i].size;
        stats.entries += 1;
        crate::progress().add_entry();
    }
    stats.output_bytes = out.finish()?;
    Ok(stats)
}

#[allow(dead_code)]
fn _assert_read(_: &mut dyn Read) {}
