//! Collect filesystem inputs into archive entries.

use crate::entry::EntryKind;
use anyhow::{Context, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Input {
    pub src: PathBuf,
    /// Archive member name, '/'-separated, no trailing slash.
    pub name: String,
    pub kind: EntryKind,
    pub size: u64,
    pub mode: u32,
    pub mtime: i64,
    pub link_target: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct WalkOptions {
    pub follow_symlinks: bool,
    pub excludes: Vec<String>,
    /// Never include this file (the archive being written).
    pub skip: Option<PathBuf>,
    /// Resolve inputs relative to this directory (like `tar -C`); member names
    /// stay relative to it.
    pub base: Option<PathBuf>,
}

fn base_name(p: &Path) -> String {
    let mut parts = Vec::new();
    for c in p.components() {
        match c {
            Component::Normal(s) => parts.push(s.to_string_lossy().into_owned()),
            Component::ParentDir => {
                parts.clear();
            }
            _ => {}
        }
    }
    parts.join("/")
}

fn build_excludes(pats: &[String]) -> Result<Option<GlobSet>> {
    if pats.is_empty() {
        return Ok(None);
    }
    let mut b = GlobSetBuilder::new();
    for p in pats {
        b.add(Glob::new(p)?);
        if !p.contains('/') {
            b.add(Glob::new(&format!("**/{p}"))?);
            b.add(Glob::new(&format!("**/{p}/**"))?);
        }
    }
    Ok(Some(b.build()?))
}

pub fn collect(paths: &[PathBuf], opts: &WalkOptions) -> Result<Vec<Input>> {
    let excl = build_excludes(&opts.excludes)?;
    let skip = opts.skip.as_ref().and_then(|p| {
        let name = p.file_name()?.to_owned();
        let dir = p.parent().map(|d| if d.as_os_str().is_empty() { Path::new(".") } else { d })?;
        Some((name, std::fs::canonicalize(dir).ok()?))
    });
    let mut out = Vec::new();
    for root in paths {
        let base = base_name(root);
        let fs_root = match &opts.base {
            Some(b) => b.join(root),
            None => root.clone(),
        };
        let root = &fs_root;
        let walker = walkdir::WalkDir::new(root).follow_links(opts.follow_symlinks).sort_by_file_name();
        for item in walker {
            let item = item.with_context(|| format!("walking {}", root.display()))?;
            let rel = item.path().strip_prefix(root).unwrap_or(item.path());
            let rel = rel.to_string_lossy().replace('\\', "/");
            let name = match (base.is_empty(), rel.is_empty()) {
                (true, true) => continue, // "." itself
                (true, false) => rel,
                (false, true) => base.clone(),
                (false, false) => format!("{base}/{rel}"),
            };
            if let Some(ex) = &excl
                && ex.is_match(&name)
            {
                continue;
            }
            if let Some((sname, sdir)) = &skip
                && item.file_name() == sname.as_os_str()
                && let Some(parent) = item.path().parent()
            {
                let parent = if parent.as_os_str().is_empty() { Path::new(".") } else { parent };
                if std::fs::canonicalize(parent).ok().as_ref() == Some(sdir) {
                    continue;
                }
            }
            let md = item.metadata().with_context(|| format!("stat {}", item.path().display()))?;
            let ft = md.file_type();
            let (kind, link_target) = if ft.is_symlink() {
                let t = std::fs::read_link(item.path())?;
                (EntryKind::Symlink, Some(t.to_string_lossy().into_owned()))
            } else if ft.is_dir() {
                (EntryKind::Dir, None)
            } else if ft.is_file() {
                (EntryKind::File, None)
            } else {
                continue; // sockets, fifos, devices: skipped
            };
            #[cfg(unix)]
            let mode = {
                use std::os::unix::fs::PermissionsExt;
                md.permissions().mode() & 0o7777
            };
            #[cfg(not(unix))]
            let mode = if kind == EntryKind::Dir { 0o755 } else { 0o644 };
            let mtime =
                md.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64).unwrap_or(0);
            out.push(Input {
                src: item.path().to_path_buf(),
                name,
                kind,
                size: if kind == EntryKind::File { md.len() } else { 0 },
                mode,
                mtime,
                link_target,
            });
        }
    }
    Ok(out)
}

/// File extensions whose content is already compressed: storing beats
/// recompressing (faster, and usually smaller).
pub fn is_precompressed_ext(name: &str) -> bool {
    let Some((_, ext)) = name.rsplit_once('.') else { return false };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "jpg"
            | "jpeg"
            | "png"
            | "gif"
            | "webp"
            | "heic"
            | "heif"
            | "avif"
            | "jxl"
            | "mp4"
            | "m4v"
            | "mkv"
            | "mov"
            | "avi"
            | "webm"
            | "wmv"
            | "flv"
            | "mp3"
            | "aac"
            | "ogg"
            | "opus"
            | "flac"
            | "m4a"
            | "wma"
            | "zip"
            | "gz"
            | "tgz"
            | "bz2"
            | "xz"
            | "txz"
            | "zst"
            | "lz4"
            | "7z"
            | "rar"
            | "br"
            | "lzma"
            | "jar"
            | "apk"
            | "aab"
            | "ipa"
            | "whl"
            | "docx"
            | "xlsx"
            | "pptx"
            | "odt"
            | "epub"
            | "woff"
            | "woff2"
            | "dmg"
            | "pkg"
            | "deb"
            | "rpm"
            | "cab"
            | "msi"
    )
}

/// Quick entropy probe: compress a sample with LZ4 (≈ 5 GB/s). If it barely
/// shrinks, the data is effectively random and should be stored.
pub fn looks_incompressible(sample: &[u8]) -> bool {
    if sample.len() < 512 {
        return false;
    }
    let s = &sample[..sample.len().min(64 * 1024)];
    // Order-0 entropy: base64/hex text defeats LZ4 but Huffman still wins ~25%.
    let mut hist = [0u32; 256];
    for &b in s {
        hist[b as usize] += 1;
    }
    let n = s.len() as f64;
    let entropy: f64 = hist
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = c as f64 / n;
            -p * p.log2()
        })
        .sum();
    if entropy < 7.6 {
        return false;
    }
    let c = lz4_flex::block::compress(s);
    c.len() as f64 > s.len() as f64 * 0.97
}
