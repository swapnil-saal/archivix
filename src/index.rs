//! The archivix side index ("AXIX").
//!
//! Every gzip / zstd / lz4 stream archivix writes is made of independently
//! decodable chunks (gzip members, zstd frames, lz4 frames). After the data we
//! append a small, compressed index describing those chunks and — for tar
//! archives — every entry and its offset in the uncompressed tar stream.
//!
//! The index is hidden in containers that every standard decoder ignores:
//!   * zstd / lz4: skippable frames (magic 0x184D2A5A / 0x184D2A5B)
//!   * gzip:       empty gzip members whose FEXTRA field carries the payload
//!
//! So `tar -xzf`, `zstd -d`, `gunzip`, `lz4 -d` read archivix output unchanged,
//! while archivix gets instant listing, random access and parallel decoding.

use crate::codec::Codec;
use crate::entry::{Entry, EntryKind};
use anyhow::{Result, bail};
use std::io::{self, Write};

const BLOB_MAGIC: &[u8; 4] = b"AXIX";
const BLOB_VERSION: u8 = 1;
const FOOT_MAGIC: &[u8; 8] = b"AXIXFOOT";
const SKIP_BLOB: u32 = 0x184D_2A5A;
const SKIP_FOOT: u32 = 0x184D_2A5B;
const GZ_FOOTER_LEN: usize = 42;
const GZ_PIECE: usize = 65_531;

#[derive(Debug, Clone, Copy, Default)]
pub struct FrameRef {
    pub c_size: u64,
    pub u_size: u64,
}

#[derive(Debug, Clone)]
pub struct IndexEntry {
    pub path: String,
    pub kind: EntryKind,
    pub size: u64,
    pub mtime: i64,
    pub mode: u32,
    pub link: Option<String>,
    /// Offset of the entry's data in the uncompressed (tar) stream.
    pub data_off: u64,
    pub crc32: u32,
}

impl IndexEntry {
    pub fn to_entry(&self) -> Entry {
        let mut e = Entry::new(self.path.clone(), self.kind, self.size);
        e.mtime = Some(self.mtime);
        e.mode = Some(self.mode);
        if self.kind == EntryKind::File {
            e.crc32 = Some(self.crc32);
        }
        e.link_target = self.link.clone();
        e
    }
}

#[derive(Debug, Clone, Default)]
pub struct Index {
    pub frames: Vec<FrameRef>,
    pub entries: Option<Vec<IndexEntry>>,
}

impl Index {
    pub fn total_uncompressed(&self) -> u64 {
        self.frames.iter().map(|f| f.u_size).sum()
    }
    pub fn total_compressed(&self) -> u64 {
        self.frames.iter().map(|f| f.c_size).sum()
    }

    /// Serialize and zstd-compress.
    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(64 + self.frames.len() * 6);
        b.extend_from_slice(BLOB_MAGIC);
        b.push(BLOB_VERSION);
        b.push(self.entries.is_some() as u8);
        put_var(&mut b, self.frames.len() as u64);
        for f in &self.frames {
            put_var(&mut b, f.c_size);
            put_var(&mut b, f.u_size);
        }
        if let Some(entries) = &self.entries {
            put_var(&mut b, entries.len() as u64);
            let mut prev_off = 0u64;
            for e in entries {
                b.push(e.kind.to_u8());
                put_bytes(&mut b, e.path.as_bytes());
                put_var(&mut b, e.size);
                put_var(&mut b, zigzag(e.mtime));
                put_var(&mut b, e.mode as u64);
                // Offsets are monotonic: delta-encode.
                put_var(&mut b, e.data_off.wrapping_sub(prev_off));
                prev_off = e.data_off;
                b.extend_from_slice(&e.crc32.to_le_bytes());
                match &e.link {
                    Some(l) => {
                        b.push(1);
                        put_bytes(&mut b, l.as_bytes());
                    }
                    None => b.push(0),
                }
            }
        }
        zstd::bulk::compress(&b, 3).expect("zstd compress index")
    }

    pub fn decode(compressed: &[u8]) -> Result<Index> {
        let raw = zstd::stream::decode_all(compressed)?;
        let mut r = Cur { b: &raw, p: 0 };
        if r.take(4)? != BLOB_MAGIC {
            bail!("bad index magic");
        }
        let ver = r.u8()?;
        if ver != BLOB_VERSION {
            bail!("unsupported index version {ver}");
        }
        let has_entries = r.u8()? != 0;
        let n = r.var()? as usize;
        let mut frames = Vec::with_capacity(n.min(1 << 24));
        for _ in 0..n {
            frames.push(FrameRef { c_size: r.var()?, u_size: r.var()? });
        }
        let entries = if has_entries {
            let n = r.var()? as usize;
            let mut v = Vec::with_capacity(n.min(1 << 24));
            let mut prev = 0u64;
            for _ in 0..n {
                let kind = EntryKind::from_u8(r.u8()?);
                let path = String::from_utf8_lossy(r.bytes()?).into_owned();
                let size = r.var()?;
                let mtime = unzigzag(r.var()?);
                let mode = r.var()? as u32;
                let data_off = prev.wrapping_add(r.var()?);
                prev = data_off;
                let c = r.take(4)?;
                let crc32 = u32::from_le_bytes([c[0], c[1], c[2], c[3]]);
                let link = if r.u8()? == 1 { Some(String::from_utf8_lossy(r.bytes()?).into_owned()) } else { None };
                v.push(IndexEntry { path, kind, size, mtime, mode, link, data_off, crc32 });
            }
            Some(v)
        } else {
            None
        };
        Ok(Index { frames, entries })
    }
}

/// Append the encoded index to a stream of the given codec.
pub fn write_trailer<W: Write>(codec: Codec, w: &mut W, index: &Index) -> io::Result<u64> {
    let blob = index.encode();
    match codec {
        Codec::Zstd | Codec::Lz4 => {
            w.write_all(&SKIP_BLOB.to_le_bytes())?;
            w.write_all(&(blob.len() as u32).to_le_bytes())?;
            w.write_all(&blob)?;
            let region = 8 + blob.len() as u64;
            w.write_all(&SKIP_FOOT.to_le_bytes())?;
            w.write_all(&16u32.to_le_bytes())?;
            w.write_all(&region.to_le_bytes())?;
            w.write_all(FOOT_MAGIC)?;
            Ok(region + 24)
        }
        Codec::Gzip => {
            let mut region = 0u64;
            for piece in blob.chunks(GZ_PIECE) {
                region += write_gz_extra_member(w, b'A', b'X', piece)? as u64;
            }
            let mut foot = [0u8; 16];
            foot[..8].copy_from_slice(&region.to_le_bytes());
            foot[8..].copy_from_slice(FOOT_MAGIC);
            let n = write_gz_extra_member(w, b'A', b'F', &foot)?;
            debug_assert_eq!(n, GZ_FOOTER_LEN);
            Ok(region + n as u64)
        }
        _ => Ok(0),
    }
}

fn write_gz_extra_member<W: Write>(w: &mut W, si1: u8, si2: u8, payload: &[u8]) -> io::Result<usize> {
    let xlen = 4 + payload.len();
    let mut h = Vec::with_capacity(12 + xlen + 10);
    h.extend_from_slice(&[0x1f, 0x8b, 0x08, 0x04, 0, 0, 0, 0, 0, 0x03]);
    h.extend_from_slice(&(xlen as u16).to_le_bytes());
    h.extend_from_slice(&[si1, si2]);
    h.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    h.extend_from_slice(payload);
    h.extend_from_slice(&[0x03, 0x00, 0, 0, 0, 0, 0, 0, 0, 0]);
    w.write_all(&h)?;
    Ok(h.len())
}

/// Locate and decode a trailing index. Returns the index and the length of the
/// data region (everything before the trailer).
pub fn read_trailer(codec: Codec, data: &[u8]) -> Option<(Index, usize)> {
    match codec {
        Codec::Zstd | Codec::Lz4 => {
            if data.len() < 24 {
                return None;
            }
            let f = &data[data.len() - 24..];
            if u32le(&f[0..4]) != SKIP_FOOT || u32le(&f[4..8]) != 16 || &f[16..24] != FOOT_MAGIC {
                return None;
            }
            let region = u64::from_le_bytes(f[8..16].try_into().ok()?) as usize;
            let start = data.len().checked_sub(24 + region)?;
            let b = &data[start..data.len() - 24];
            if b.len() < 8 || u32le(&b[0..4]) != SKIP_BLOB || u32le(&b[4..8]) as usize != b.len() - 8 {
                return None;
            }
            let idx = Index::decode(&b[8..]).ok()?;
            Some((idx, start))
        }
        Codec::Gzip => {
            if data.len() < GZ_FOOTER_LEN {
                return None;
            }
            let f = &data[data.len() - GZ_FOOTER_LEN..];
            if f[..4] != [0x1f, 0x8b, 0x08, 0x04] || f[12] != b'A' || f[13] != b'F' || &f[24..32] != FOOT_MAGIC {
                return None;
            }
            let region = u64::from_le_bytes(f[16..24].try_into().ok()?) as usize;
            let start = data.len().checked_sub(GZ_FOOTER_LEN + region)?;
            let mut p = start;
            let end = data.len() - GZ_FOOTER_LEN;
            let mut blob = Vec::new();
            while p < end {
                let m = data.get(p..p + 16)?;
                if m[..4] != [0x1f, 0x8b, 0x08, 0x04] || m[12] != b'A' || m[13] != b'X' {
                    return None;
                }
                let len = u16::from_le_bytes([m[14], m[15]]) as usize;
                blob.extend_from_slice(data.get(p + 16..p + 16 + len)?);
                p += 16 + len + 10;
            }
            let idx = Index::decode(&blob).ok()?;
            Some((idx, start))
        }
        _ => None,
    }
}

fn u32le(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}
fn unzigzag(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

fn put_var(b: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        b.push((v as u8) | 0x80);
        v >>= 7;
    }
    b.push(v as u8);
}
fn put_bytes(b: &mut Vec<u8>, s: &[u8]) {
    put_var(b, s.len() as u64);
    b.extend_from_slice(s);
}

struct Cur<'a> {
    b: &'a [u8],
    p: usize,
}
impl<'a> Cur<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.p + n > self.b.len() {
            bail!("truncated index");
        }
        let s = &self.b[self.p..self.p + n];
        self.p += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn var(&mut self) -> Result<u64> {
        let mut v = 0u64;
        let mut shift = 0;
        loop {
            let byte = self.u8()?;
            if shift >= 64 {
                bail!("varint overflow");
            }
            v |= ((byte & 0x7f) as u64) << shift;
            if byte & 0x80 == 0 {
                return Ok(v);
            }
            shift += 7;
        }
    }
    fn bytes(&mut self) -> Result<&'a [u8]> {
        let n = self.var()? as usize;
        self.take(n)
    }
}
