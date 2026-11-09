//! RAR (v1.5–v5) reading via the official unrar library. Creating RAR archives
//! is not possible: the format's encoder is proprietary.

use crate::Visitor;
use crate::entry::{Entry, EntryKind};
use crate::extract::{ExtractOptions, ExtractStats, Extractor};
use anyhow::{Result, anyhow};
use std::path::Path;

fn to_entry(h: &unrar::FileHeader) -> Entry {
    let name = h.filename.to_string_lossy().replace('\\', "/");
    let kind = if h.is_directory() { EntryKind::Dir } else { EntryKind::File };
    let mut e = Entry::new(name.trim_end_matches('/').to_string(), kind, h.unpacked_size);
    e.crc32 = Some(h.file_crc);
    e.mtime = Some(crate::util::unix_from_dos(h.file_time));
    if h.file_attr & 0o170000 != 0 {
        e.mode = Some(h.file_attr & 0o7777);
        if h.file_attr & 0o170000 == 0o120000 {
            e.kind = EntryKind::Symlink;
        }
    }
    e.encrypted = h.is_encrypted();
    e.method = Some(format!("rar-m{}", h.method.saturating_sub(0x30)));
    e
}

macro_rules! archive {
    ($path:expr, $pw:expr) => {
        match $pw {
            Some(p) => unrar::Archive::with_password($path, p),
            None => unrar::Archive::new($path),
        }
    };
}

pub fn list(path: &Path, pw: Option<&str>) -> Result<Vec<Entry>> {
    let mut out = Vec::new();
    for h in archive!(path, pw).open_for_listing().map_err(|e| anyhow!("rar: {e}"))? {
        let h = h.map_err(|e| anyhow!("rar: {e}"))?;
        out.push(to_entry(&h));
    }
    Ok(out)
}

pub fn extract(path: &Path, opts: &ExtractOptions) -> Result<ExtractStats> {
    let ex = Extractor::new(opts.clone())?;
    let mut ar = archive!(path, opts.password.as_deref()).open_for_processing().map_err(|e| anyhow!("rar: {e}"))?;
    while let Some(h) = ar.read_header().map_err(|e| anyhow!("rar: {e}"))? {
        let e = to_entry(h.entry());
        let target = if ex.wants(&e.path) { ex.target(&e.path) } else { None };
        ar = match (target, e.kind) {
            (Some(t), EntryKind::Dir) => {
                ex.make_dir(&t, e.mtime, e.mode)?;
                h.skip()
            }
            (Some(t), EntryKind::Symlink) => {
                let (data, next) = h.read().map_err(|x| anyhow!("rar: {x}"))?;
                ex.defer_symlink(t, String::from_utf8_lossy(&data).into_owned());
                Ok(next)
            }
            (Some(t), _) if e.size > crate::extract::ASYNC_LIMIT => {
                if let Some(p) = t.parent() {
                    std::fs::create_dir_all(p)?;
                }
                h.extract_to(&t)
            }
            (Some(t), _) => {
                let (data, next) = h.read().map_err(|x| anyhow!("rar {}: {x}", e.path))?;
                ex.write_bytes(&t, &data, e.mode, e.mtime)?;
                Ok(next)
            }
            (None, _) => h.skip(),
        }
        .map_err(|x| anyhow!("rar: {x}"))?;
    }
    ex.finish()
}

pub fn visit(path: &Path, pw: Option<&str>, v: &mut Visitor<'_>) -> Result<()> {
    let mut ar = archive!(path, pw).open_for_processing().map_err(|e| anyhow!("rar: {e}"))?;
    while let Some(h) = ar.read_header().map_err(|e| anyhow!("rar: {e}"))? {
        let e = to_entry(h.entry());
        if e.kind == EntryKind::File && (v.filter)(&e) {
            let (data, next) = h.read().map_err(|x| anyhow!("rar: {x}"))?;
            ar = next;
            if !(v.each)(&e, &mut &data[..])? {
                break;
            }
        } else {
            ar = h.skip().map_err(|x| anyhow!("rar: {x}"))?;
        }
    }
    Ok(())
}
