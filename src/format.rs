//! Archive format detection (content first, extension as a hint).

use crate::codec::{self, Codec};
use anyhow::{Context, Result};
use std::fs::File;
use std::io::Read;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Zip,
    /// tar, optionally wrapped in a stream codec
    Tar(Codec),
    SevenZ,
    Rar,
    /// A single compressed stream that is not a tar archive (file.gz, file.zst, ...)
    Raw(Codec),
}

impl Format {
    pub fn name(&self) -> String {
        match self {
            Format::Zip => "zip".into(),
            Format::Tar(Codec::None) => "tar".into(),
            Format::Tar(c) => format!("tar.{}", c.extension()),
            Format::SevenZ => "7z".into(),
            Format::Rar => "rar".into(),
            Format::Raw(c) => c.name().into(),
        }
    }

    /// Pick an output format from a file name, e.g. `x.tar.zst`, `x.tgz`, `x.zip`.
    pub fn from_name(name: &str) -> Option<Format> {
        let n = name.to_ascii_lowercase();
        let zip_exts = [
            ".zip", ".jar", ".war", ".ear", ".apk", ".aar", ".whl", ".xpi", ".ipa", ".nupkg", ".cbz", ".epub", ".docx", ".xlsx", ".pptx",
            ".odt", ".ods", ".odp", ".vsix",
        ];
        if zip_exts.iter().any(|e| n.ends_with(e)) {
            return Some(Format::Zip);
        }
        if n.ends_with(".7z") {
            return Some(Format::SevenZ);
        }
        if n.ends_with(".rar") || n.ends_with(".cbr") {
            return Some(Format::Rar);
        }
        if n.ends_with(".tar") {
            return Some(Format::Tar(Codec::None));
        }
        let short = [
            (".tgz", Codec::Gzip),
            (".taz", Codec::Gzip),
            (".tbz", Codec::Bzip2),
            (".tbz2", Codec::Bzip2),
            (".tb2", Codec::Bzip2),
            (".txz", Codec::Xz),
            (".tlz", Codec::Lzma),
            (".tzst", Codec::Zstd),
            (".tzs", Codec::Zstd),
            (".tlz4", Codec::Lz4),
        ];
        for (e, c) in short {
            if n.ends_with(e) {
                return Some(Format::Tar(c));
            }
        }
        if let Some((stem, ext)) = n.rsplit_once('.')
            && let Some(c) = Codec::from_extension(ext)
        {
            return Some(if stem.ends_with(".tar") { Format::Tar(c) } else { Format::Raw(c) });
        }
        None
    }
}

fn is_tar_header(b: &[u8]) -> bool {
    if b.len() < 512 {
        return false;
    }
    if &b[257..262] == b"ustar" {
        return true;
    }
    // v7 tar: validate the header checksum.
    let stored = std::str::from_utf8(&b[148..156])
        .ok()
        .map(|s| s.trim_matches(|c: char| c == '\0' || c == ' '))
        .and_then(|s| u32::from_str_radix(s, 8).ok());
    let Some(stored) = stored else { return false };
    let sum: u32 = b[..512].iter().enumerate().map(|(i, &x)| if (148..156).contains(&i) { 32 } else { x as u32 }).sum();
    sum == stored && b[0] != 0
}

/// Detect the format of an existing archive by its content.
pub fn detect(path: &Path) -> Result<Format> {
    let mut f = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut head = vec![0u8; 1024];
    let n = read_full(&mut f, &mut head)?;
    head.truncate(n);
    let by_name = path.file_name().and_then(|s| s.to_str()).and_then(Format::from_name);

    if head.starts_with(b"PK\x03\x04") || head.starts_with(b"PK\x05\x06") || head.starts_with(b"PK\x07\x08") {
        return Ok(Format::Zip);
    }
    if head.starts_with(&[b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C]) {
        return Ok(Format::SevenZ);
    }
    if head.starts_with(b"Rar!\x1a\x07") {
        return Ok(Format::Rar);
    }
    if is_tar_header(&head) {
        return Ok(Format::Tar(Codec::None));
    }
    let codec = Codec::sniff(&head).or(match by_name {
        Some(Format::Tar(c)) | Some(Format::Raw(c)) if c != Codec::None => Some(c),
        _ => None,
    });
    if let Some(c) = codec {
        // Peek into the decompressed stream to see if it holds a tar archive.
        let f = File::open(path)?;
        let mut dec = codec::decoder(c, f)?;
        let mut inner = vec![0u8; 512];
        let n = read_full(&mut dec, &mut inner).unwrap_or(0);
        if n == 512 && is_tar_header(&inner) {
            return Ok(Format::Tar(c));
        }
        if n == 0 && matches!(by_name, Some(Format::Tar(_))) {
            return Ok(Format::Tar(c));
        }
        return Ok(Format::Raw(c));
    }
    // Self-extracting / prefixed zip: the central directory is at the end.
    if zip::ZipArchive::new(File::open(path)?).is_ok() {
        return Ok(Format::Zip);
    }
    if let Some(f) = by_name {
        return Ok(f);
    }
    anyhow::bail!("{}: unrecognized archive format", path.display())
}

pub fn read_full<R: Read + ?Sized>(r: &mut R, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}
