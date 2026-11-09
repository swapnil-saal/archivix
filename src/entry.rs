//! Format-neutral description of an archive member.

use serde::Serialize;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    Hardlink,
    Other,
}

impl EntryKind {
    pub fn as_char(self) -> char {
        match self {
            EntryKind::File => '-',
            EntryKind::Dir => 'd',
            EntryKind::Symlink => 'l',
            EntryKind::Hardlink => 'h',
            EntryKind::Other => '?',
        }
    }
    pub(crate) fn to_u8(self) -> u8 {
        match self {
            EntryKind::File => 0,
            EntryKind::Dir => 1,
            EntryKind::Symlink => 2,
            EntryKind::Hardlink => 3,
            EntryKind::Other => 4,
        }
    }
    pub(crate) fn from_u8(v: u8) -> Self {
        match v {
            0 => EntryKind::File,
            1 => EntryKind::Dir,
            2 => EntryKind::Symlink,
            3 => EntryKind::Hardlink,
            _ => EntryKind::Other,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub path: String,
    pub kind: EntryKind,
    /// Uncompressed size in bytes.
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compressed_size: Option<u64>,
    /// Modification time, seconds since the Unix epoch (UTC).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mtime: Option<i64>,
    /// Unix permission bits (without the file-type bits).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crc32: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub encrypted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link_target: Option<String>,
}

impl Entry {
    pub fn new(path: String, kind: EntryKind, size: u64) -> Self {
        Entry {
            path,
            kind,
            size,
            compressed_size: None,
            mtime: None,
            mode: None,
            crc32: None,
            method: None,
            encrypted: false,
            link_target: None,
        }
    }
}
