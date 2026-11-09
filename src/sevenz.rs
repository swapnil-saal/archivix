//! 7z: header-only listing, block-parallel extraction, multithreaded LZMA2 creation.

use crate::entry::{Entry, EntryKind};
use crate::extract::{ExtractOptions, ExtractStats, Extractor};
use crate::parallel::threads;
use crate::walk::Input;
use crate::{CreateStats, Visitor};
use anyhow::{Context, Result, anyhow};
use rayon::prelude::*;
use sevenz_rust2::{Archive, ArchiveEntry, ArchiveReader, ArchiveWriter, BlockDecoder, Password};
use std::collections::HashSet;
use std::fs::File;
use std::io::{self, Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

fn password(p: Option<&str>) -> Password {
    p.map(Password::from).unwrap_or_else(Password::empty)
}

fn to_entry(a: &Archive, i: usize) -> Entry {
    let f = &a.files[i];
    let attrs = f.windows_attributes;
    let unix = (f.has_windows_attributes && attrs & 0x8000 != 0).then_some(attrs >> 16);
    let kind = if f.is_directory || attrs & 0x10 != 0 && f.has_windows_attributes {
        EntryKind::Dir
    } else if unix.is_some_and(|m| m & 0o170000 == 0o120000) {
        EntryKind::Symlink
    } else {
        EntryKind::File
    };
    let mut e = Entry::new(f.name.trim_end_matches('/').to_string(), kind, f.size);
    if f.has_last_modified_date {
        let st: std::time::SystemTime = f.last_modified_date.into();
        e.mtime = st.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs() as i64);
    }
    e.mode = unix.map(|m| m & 0o7777);
    if f.has_crc {
        e.crc32 = Some(f.crc as u32);
    }
    e.encrypted = a.is_file_encrypted(i);
    e
}

fn open(path: &Path, pw: &Password) -> Result<(memmap2::Mmap, Archive)> {
    let f = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let map = unsafe { memmap2::Mmap::map(&f)? };
    let a = Archive::read(&mut Cursor::new(&map[..]), pw).map_err(|e| anyhow!("7z: {e}"))?;
    Ok((map, a))
}

pub fn list(path: &Path, pw: Option<&str>) -> Result<Vec<Entry>> {
    let (_, a) = open(path, &password(pw))?;
    Ok((0..a.files.len()).map(|i| to_entry(&a, i)).collect())
}

pub fn extract(path: &Path, opts: &ExtractOptions) -> Result<ExtractStats> {
    let pw = password(opts.password.as_deref());
    let (map, a) = open(path, &pw)?;
    let ex = Extractor::new(opts.clone())?;
    let by_name: std::collections::HashMap<&str, usize> = a.files.iter().enumerate().map(|(i, f)| (f.name.as_str(), i)).collect();
    // Entries without data (directories, empty files) need no decoding.
    for i in 0..a.files.len() {
        if !a.files[i].has_stream {
            let e = to_entry(&a, i);
            ex.extract_entry(&e, &mut io::empty())?;
        }
    }
    let blocks: HashSet<usize> = (0..a.files.len())
        .filter(|&i| a.files[i].has_stream && ex.wants(a.files[i].name.trim_end_matches('/')))
        .filter_map(|i| a.stream_map.file_block_index[i])
        .collect();
    let mut blocks: Vec<usize> = blocks.into_iter().collect();
    blocks.sort_unstable();
    let wanted = (0..a.files.len()).filter(|&i| a.files[i].has_stream && ex.wants(a.files[i].name.trim_end_matches('/')));
    let (bytes, n) = wanted.fold((0u64, 0u64), |(b, n), i| (b + a.files[i].size, n + 1));
    crate::progress().set_total(bytes);
    crate::progress().set_total_entries(n);
    // Independent blocks decode in parallel; spare threads go to each block's codec.
    let per_block = (threads() / blocks.len().max(1)).max(1) as u32;
    let err: Mutex<Option<anyhow::Error>> = Mutex::new(None);
    blocks.par_iter().try_for_each(|&bi| -> Result<()> {
        let mut src = Cursor::new(&map[..]);
        let dec = BlockDecoder::new(per_block, bi, &a, &pw, &mut src);
        dec.for_each_entries(&mut |entry: &ArchiveEntry, r: &mut dyn Read| {
            let Some(&i) = by_name.get(entry.name.as_str()) else { return Ok(true) };
            let e = to_entry(&a, i);
            let mut r = crate::progress::Counted::new(r, crate::progress::Count::Both);
            if let Err(x) = ex.extract_entry(&e, &mut r) {
                *err.lock().unwrap() = Some(x);
                return Ok(false);
            }
            Ok(true)
        })
        .map_err(|e| anyhow!("7z block {bi}: {e}"))?;
        Ok(())
    })?;
    if let Some(e) = err.into_inner().unwrap() {
        return Err(e);
    }
    ex.finish()
}

pub fn visit(path: &Path, pw: Option<&str>, v: &mut Visitor<'_>) -> Result<()> {
    let pwd = password(pw);
    let (map, a) = open(path, &pwd)?;
    let by_name: std::collections::HashMap<String, usize> = a.files.iter().enumerate().map(|(i, f)| (f.name.clone(), i)).collect();
    for i in 0..a.files.len() {
        if !a.files[i].has_stream {
            let e = to_entry(&a, i);
            if (v.filter)(&e) && !(v.each)(&e, &mut io::empty())? {
                return Ok(());
            }
        }
    }
    let mut reader = ArchiveReader::from_archive(a.clone(), Cursor::new(&map[..]), pwd);
    let mut err = None;
    let mut stop = false;
    reader
        .for_each_entries(|entry, r| {
            if stop {
                return Ok(false);
            }
            let Some(&i) = by_name.get(&entry.name) else { return Ok(true) };
            let e = to_entry(&a, i);
            if !(v.filter)(&e) {
                return Ok(true);
            }
            let mut r = crate::progress::Counted::new(r, crate::progress::Count::Both);
            match (v.each)(&e, &mut r) {
                Ok(true) => Ok(true),
                Ok(false) => {
                    stop = true;
                    Ok(false)
                }
                Err(x) => {
                    err = Some(x);
                    stop = true;
                    Ok(false)
                }
            }
        })
        .map_err(|e| anyhow!("7z: {e}"))?;
    if let Some(e) = err {
        return Err(e);
    }
    Ok(())
}

/// Opens the file only when first read and closes it at EOF, so an archive of
/// 1M files never holds more than one descriptor.
struct LazyFile {
    path: PathBuf,
    f: Option<File>,
    done: bool,
}
impl Read for LazyFile {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        if self.done {
            return Ok(0);
        }
        if self.f.is_none() {
            self.f = Some(File::open(&self.path)?);
        }
        crate::progress().check()?;
        let n = self.f.as_mut().unwrap().read(b)?;
        crate::progress().add(n as u64);
        if n == 0 {
            self.f = None;
            self.done = true;
            crate::progress().add_entry();
        }
        Ok(n)
    }
}

pub fn create(output: &Path, inputs: &[Input], level: i32) -> Result<CreateStats> {
    let mut w = ArchiveWriter::create(output).map_err(|e| anyhow!("7z: {e}"))?;
    let level = level.clamp(0, 9) as u32;
    let lz = sevenz_rust2::encoder_options::Lzma2Options::from_level_mt(level, threads() as u32, 16 << 20);
    w.set_content_methods(vec![sevenz_rust2::EncoderConfiguration::new(sevenz_rust2::EncoderMethod::LZMA2).with_options(lz.into())]);
    let mut stats = CreateStats::default();
    let mut solid_entries = Vec::new();
    let mut readers = Vec::new();
    for inp in inputs {
        let mut e = ArchiveEntry::new_file(&inp.name);
        e.is_directory = inp.kind == EntryKind::Dir;
        e.has_last_modified_date = true;
        e.last_modified_date =
            sevenz_rust2::NtTime::try_from(std::time::UNIX_EPOCH + std::time::Duration::from_secs(inp.mtime.max(0) as u64))
                .unwrap_or_default();
        let ty = match inp.kind {
            EntryKind::Dir => 0o040000,
            EntryKind::Symlink => 0o120000,
            _ => 0o100000,
        };
        e.has_windows_attributes = true;
        e.windows_attributes = 0x8000 | ((ty | inp.mode) << 16) | if e.is_directory { 0x10 } else { 0x20 };
        stats.entries += 1;
        match inp.kind {
            EntryKind::Dir => {
                e.has_stream = false;
                w.push_archive_entry::<&[u8]>(e, None).map_err(|x| anyhow!("7z: {x}"))?;
                crate::progress().add_entry();
            }
            EntryKind::Symlink => {
                let t = inp.link_target.clone().unwrap_or_default().into_bytes();
                e.has_stream = !t.is_empty();
                stats.input_bytes += t.len() as u64;
                w.push_archive_entry(e, Some(&t[..])).map_err(|x| anyhow!("7z: {x}"))?;
                crate::progress().add_entry();
            }
            _ if inp.size == 0 => {
                e.has_stream = false;
                w.push_archive_entry::<&[u8]>(e, None).map_err(|x| anyhow!("7z: {x}"))?;
                crate::progress().add_entry();
            }
            _ => {
                e.has_stream = true;
                stats.input_bytes += inp.size;
                solid_entries.push(e);
                readers.push(sevenz_rust2::SourceReader::new(LazyFile { path: inp.src.clone(), f: None, done: false }));
            }
        }
    }
    if !solid_entries.is_empty() {
        w.push_archive_entries(solid_entries, readers).map_err(|x| anyhow!("7z: {x}"))?;
    }
    w.finish().map_err(|x| anyhow!("7z: {x}"))?;
    stats.output_bytes = std::fs::metadata(output)?.len();
    Ok(stats)
}
