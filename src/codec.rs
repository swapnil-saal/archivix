//! Stream codecs: identification, sequential decoders and default levels.

use std::io::{self, BufReader, Read};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Codec {
    None,
    Gzip,
    Bzip2,
    Xz,
    Lzma,
    Zstd,
    Lz4,
    Brotli,
}

impl Codec {
    pub fn name(self) -> &'static str {
        match self {
            Codec::None => "none",
            Codec::Gzip => "gzip",
            Codec::Bzip2 => "bzip2",
            Codec::Xz => "xz",
            Codec::Lzma => "lzma",
            Codec::Zstd => "zstd",
            Codec::Lz4 => "lz4",
            Codec::Brotli => "brotli",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Codec::None => "",
            Codec::Gzip => "gz",
            Codec::Bzip2 => "bz2",
            Codec::Xz => "xz",
            Codec::Lzma => "lzma",
            Codec::Zstd => "zst",
            Codec::Lz4 => "lz4",
            Codec::Brotli => "br",
        }
    }

    pub fn parse(s: &str) -> Option<Codec> {
        Some(match s.to_ascii_lowercase().as_str() {
            "none" | "store" | "raw" => Codec::None,
            "gz" | "gzip" | "deflate" => Codec::Gzip,
            "bz2" | "bzip2" | "bz" => Codec::Bzip2,
            "xz" => Codec::Xz,
            "lzma" => Codec::Lzma,
            "zst" | "zstd" | "zstandard" => Codec::Zstd,
            "lz4" => Codec::Lz4,
            "br" | "brotli" => Codec::Brotli,
            _ => return None,
        })
    }

    /// Codec from a single file extension (without dot).
    pub fn from_extension(ext: &str) -> Option<Codec> {
        Some(match ext.to_ascii_lowercase().as_str() {
            "gz" | "gzip" | "z" => Codec::Gzip,
            "bz2" | "bz" | "bzip2" => Codec::Bzip2,
            "xz" => Codec::Xz,
            "lzma" => Codec::Lzma,
            "zst" | "zstd" => Codec::Zstd,
            "lz4" => Codec::Lz4,
            "br" => Codec::Brotli,
            _ => return None,
        })
    }

    /// Identify a codec from the first bytes of a stream.
    pub fn sniff(head: &[u8]) -> Option<Codec> {
        if head.len() >= 2 && head[0] == 0x1f && head[1] == 0x8b {
            return Some(Codec::Gzip);
        }
        if head.len() >= 4 && &head[..3] == b"BZh" && (b'1'..=b'9').contains(&head[3]) {
            return Some(Codec::Bzip2);
        }
        if head.len() >= 6 && head[..6] == [0xFD, b'7', b'z', b'X', b'Z', 0x00] {
            return Some(Codec::Xz);
        }
        if head.len() >= 4 {
            let m = u32::from_le_bytes([head[0], head[1], head[2], head[3]]);
            if m == 0xFD2F_B528 {
                return Some(Codec::Zstd);
            }
            if m == 0x184D_2204 {
                return Some(Codec::Lz4);
            }
            // Skippable frame: shared by zstd and lz4. Assume zstd (lz4 rarely starts so).
            if m & 0xFFFF_FFF0 == 0x184D_2A50 {
                return Some(Codec::Zstd);
            }
        }
        // Legacy .lzma (lzma_alone): props byte 0x5D + dict size, commonly.
        if head.len() >= 13 && head[0] == 0x5D && head[1] == 0x00 && head[2] == 0x00 {
            return Some(Codec::Lzma);
        }
        None
    }

    pub fn default_level(self) -> i32 {
        match self {
            Codec::None => 0,
            Codec::Gzip => 6,
            Codec::Bzip2 => 9,
            Codec::Xz | Codec::Lzma => 6,
            Codec::Zstd => 3,
            Codec::Lz4 => 1,
            Codec::Brotli => 6,
        }
    }

    pub fn clamp_level(self, level: i32) -> i32 {
        match self {
            Codec::None => 0,
            Codec::Gzip => level.clamp(0, 9),
            Codec::Bzip2 => level.clamp(1, 9),
            Codec::Xz | Codec::Lzma => level.clamp(0, 9),
            Codec::Zstd => level.clamp(-7, 22),
            Codec::Lz4 => level.clamp(0, 12),
            Codec::Brotli => level.clamp(0, 11),
        }
    }

    /// Whether `archivix` can compress this codec in parallel chunks while staying
    /// readable by the reference tool for that format.
    pub fn parallel_chunks(self) -> bool {
        matches!(self, Codec::Gzip | Codec::Zstd | Codec::Lz4 | Codec::Bzip2)
    }
}

/// A sequential decoder for any supported codec. Handles concatenated
/// members/frames/streams, which is what our parallel encoders emit.
pub fn decoder<'a, R: Read + 'a>(codec: Codec, r: R) -> io::Result<Box<dyn Read + 'a>> {
    const BUF: usize = 256 * 1024;
    Ok(match codec {
        Codec::None => Box::new(r),
        Codec::Gzip => Box::new(flate2::read::MultiGzDecoder::new(BufReader::with_capacity(BUF, r))),
        Codec::Bzip2 => Box::new(bzip2::read::MultiBzDecoder::new(BufReader::with_capacity(BUF, r))),
        Codec::Xz => Box::new(liblzma::bufread::XzDecoder::new_stream(BufReader::with_capacity(BUF, r), xz_mt_decoder()?)),
        Codec::Lzma => {
            let stream = liblzma::stream::Stream::new_lzma_decoder(u64::MAX).map_err(io::Error::other)?;
            Box::new(liblzma::read::XzDecoder::new_stream(BufReader::with_capacity(BUF, r), stream))
        }
        Codec::Zstd => {
            let mut d = zstd::stream::read::Decoder::with_buffer(BufReader::with_capacity(BUF, r))?;
            d.window_log_max(31)?;
            Box::new(d)
        }
        Codec::Lz4 => Box::new(Lz4Multi::new(BufReader::with_capacity(BUF, r))),
        Codec::Brotli => Box::new(brotli::Decompressor::new(r, BUF)),
    })
}

/// Multithreaded xz decoder. liblzma silently decodes on ONE thread unless
/// `memlimit_threading` is raised (the liblzma crate's `new_parallel` leaves it
/// at the default), so set it explicitly: blocks then decode on every core.
pub fn xz_mt_decoder() -> io::Result<liblzma::stream::Stream> {
    let threads = rayon::current_num_threads().max(1) as u32;
    liblzma::stream::MtStreamBuilder::new()
        .threads(threads)
        .memlimit_threading(u64::MAX)
        .memlimit_stop(u64::MAX)
        .decoder()
        .map_err(io::Error::other)
}

/// Spec-compliant LZ4 frame stream reader: decodes concatenated frames and
/// skips skippable frames (lz4_flex's FrameDecoder stops after the first frame
/// and errors on skippable frames).
pub struct Lz4Multi<R: Read> {
    src: Option<R>,
    dec: Option<lz4_flex::frame::FrameDecoder<io::Chain<io::Cursor<[u8; 4]>, R>>>,
}

impl<R: Read> Lz4Multi<R> {
    pub fn new(r: R) -> Self {
        Lz4Multi { src: Some(r), dec: None }
    }
}

impl<R: Read> Read for Lz4Multi<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            if let Some(d) = self.dec.as_mut() {
                let n = d.read(buf)?;
                if n > 0 || buf.is_empty() {
                    return Ok(n);
                }
                let (_, r) = self.dec.take().unwrap().into_inner().into_inner();
                self.src = Some(r);
            }
            let mut r = self.src.take().ok_or_else(|| io::Error::other("lz4 reader poisoned"))?;
            let mut magic = [0u8; 4];
            let k = crate::format::read_full(&mut r, &mut magic)?;
            if k == 0 {
                self.src = Some(r);
                return Ok(0);
            }
            if k < 4 {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "truncated lz4 frame"));
            }
            let m = u32::from_le_bytes(magic);
            if m & 0xFFFF_FFF0 == 0x184D_2A50 {
                let mut sz = [0u8; 4];
                r.read_exact(&mut sz)?;
                let len = u32::from_le_bytes(sz) as u64;
                let skipped = io::copy(&mut (&mut r).take(len), &mut io::sink())?;
                if skipped != len {
                    return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "truncated skippable frame"));
                }
                self.src = Some(r);
                continue;
            }
            self.dec = Some(lz4_flex::frame::FrameDecoder::new(io::Cursor::new(magic).chain(r)));
        }
    }
}
