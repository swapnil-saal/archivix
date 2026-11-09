//! ZIP reading: instant listing from the central directory, parallel extraction.

use crate::Visitor;
use crate::entry::{Entry, EntryKind};
use crate::extract::{ExtractOptions, ExtractStats, Extractor};
use anyhow::{Context, Result, anyhow};
use rayon::prelude::*;
use std::fs::File;
use std::io::{Cursor, Read};
use std::path::Path;
use zip::ZipArchive;
use zip::read::ZipFile;

fn map(path: &Path) -> Result<memmap2::Mmap> {
    let f = File::open(path).with_context(|| format!("open {}", path.display()))?;
    Ok(unsafe { memmap2::Mmap::map(&f)? })
}

fn entry_from(name: &str, f: &zip::read::ZipFileEntry<'_>) -> Entry {
    let kind = if f.is_dir() {
        EntryKind::Dir
    } else if f.is_symlink() {
        EntryKind::Symlink
    } else {
        EntryKind::File
    };
    let mut e = Entry::new(name.trim_end_matches('/').to_string(), kind, f.size());
    e.compressed_size = Some(f.compressed_size());
    e.crc32 = Some(f.crc32());
    e.method = Some(format!("{}", f.compression()).to_lowercase());
    e.encrypted = f.encrypted();
    e.mode = f.unix_mode().map(|m| m & 0o7777);
    let mut mtime = None;
    {
        if let Some(dt) = f.last_modified() {
            mtime = Some(crate::util::unix_from_local(
                dt.year() as i32,
                dt.month() as u32,
                dt.day() as u32,
                dt.hour() as u32,
                dt.minute() as u32,
                dt.second() as u32,
            ));
        }
    }
    e.mtime = mtime;
    e
}

fn file_entry<R: Read + ?Sized>(f: &ZipFile<'_, R>) -> Entry {
    let name = f.name().map(|s| s.into_owned()).unwrap_or_else(|_| String::from_utf8_lossy(f.name_raw()).into_owned());
    entry_from(&name, &f.entry())
}

pub fn list(path: &Path) -> Result<Vec<Entry>> {
    let m = map(path)?;
    let ar = ZipArchive::new(Cursor::new(&m[..]))?;
    let mut out = Vec::with_capacity(ar.len());
    for i in 0..ar.len() {
        let f = ar.by_index_data(i)?;
        let name = f.name().map(|s| s.into_owned()).unwrap_or_else(|_| String::from_utf8_lossy(f.name_raw()).into_owned());
        out.push(entry_from(&name, &f));
    }
    Ok(out)
}

fn open_entry<'a, 'b>(ar: &'a mut ZipArchive<Cursor<&'b [u8]>>, i: usize, pw: Option<&str>) -> Result<ZipFile<'a, Cursor<&'b [u8]>>> {
    Ok(match pw {
        Some(p) => ar.by_index_decrypt(i, p.as_bytes())?,
        None => ar.by_index(i).map_err(|e| match e {
            zip::result::ZipError::UnsupportedArchive(m) if m.contains("assword") => anyhow!("entry is encrypted: pass --password"),
            e => e.into(),
        })?,
    })
}

pub fn extract(path: &Path, opts: &ExtractOptions) -> Result<ExtractStats> {
    let m = map(path)?;
    let ar = ZipArchive::new(Cursor::new(&m[..]))?;
    let ex = Extractor::new(opts.clone())?;
    let mut idx: Vec<(usize, u64)> = Vec::with_capacity(ar.len());
    for i in 0..ar.len() {
        let f = ar.by_index_data(i)?;
        let name = f.name().map(|s| s.into_owned()).unwrap_or_default();
        if ex.wants(name.trim_end_matches('/')) {
            idx.push((i, f.size()));
        }
    }
    // Largest first: big entries start early and don't straggle at the end.
    idx.sort_unstable_by(|a, b| b.1.cmp(&a.1));
    let pw = opts.password.as_deref();
    idx.par_iter().try_for_each_init(
        || ar.clone(),
        |ar, &(i, _)| -> Result<()> {
            let mut f = open_entry(ar, i, pw)?;
            let e = file_entry(&f);
            ex.extract_entry(&e, &mut f).with_context(|| format!("extract {}", e.path))
        },
    )?;
    ex.finish()
}

pub fn visit(path: &Path, password: Option<&str>, f: &mut Visitor<'_>) -> Result<()> {
    let m = map(path)?;
    let mut ar = ZipArchive::new(Cursor::new(&m[..]))?;
    for i in 0..ar.len() {
        let e = {
            let d = ar.by_index_data(i)?;
            let name = d.name().map(|s| s.into_owned()).unwrap_or_default();
            entry_from(&name, &d)
        };
        if !(f.filter)(&e) {
            continue;
        }
        let mut zf = open_entry(&mut ar, i, password)?;
        if !(f.each)(&e, &mut zf)? {
            break;
        }
    }
    Ok(())
}

/// Direct lookup of one member by name (no scan of the central directory).
pub fn cat(path: &Path, name: &str, password: Option<&str>, out: &mut dyn std::io::Write) -> Result<Option<u64>> {
    let m = map(path)?;
    let mut ar = ZipArchive::new(Cursor::new(&m[..]))?;
    let Some(i) = ar.index_for_name(name) else { return Ok(None) };
    let mut f = open_entry(&mut ar, i, password)?;
    Ok(Some(std::io::copy(&mut f, out)?))
}
