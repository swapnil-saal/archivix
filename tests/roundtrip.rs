use archivix::{CreateOptions, EntryKind, Format, extract::ExtractOptions};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// A small tree with the awkward cases: unicode, long names, empty files and
/// dirs, symlinks, an incompressible file and a multi-chunk compressible file.
fn make_tree(root: &Path) -> PathBuf {
    let t = root.join("tree");
    fs::create_dir_all(t.join("ünï/日本")).unwrap();
    fs::create_dir_all(t.join("empty_dir")).unwrap();
    fs::write(t.join("ünï/日本/f.txt"), "hello wörld\n").unwrap();
    fs::write(t.join("empty"), "").unwrap();
    fs::write(t.join(format!("{}.txt", "x".repeat(150))), "long name").unwrap();
    let mut rnd = Vec::with_capacity(3 << 20);
    let mut x: u64 = 0x9E3779B97F4A7C15;
    while rnd.len() < 3 << 20 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        rnd.extend_from_slice(&x.to_le_bytes());
    }
    fs::write(t.join("random.bin"), &rnd).unwrap();
    let mut text = fs::File::create(t.join("big.txt")).unwrap();
    for i in 0..400_000 {
        writeln!(text, "line {i} of a fairly compressible text file").unwrap();
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink("big.txt", t.join("link")).unwrap();
    t
}

fn assert_same(a: &Path, b: &Path) {
    for e in walkdir::WalkDir::new(a) {
        let e = e.unwrap();
        let rel = e.path().strip_prefix(a).unwrap();
        let other = b.join(rel);
        let ft = e.file_type();
        let md = fs::symlink_metadata(&other).unwrap_or_else(|_| panic!("missing {}", other.display()));
        if ft.is_symlink() {
            assert!(md.file_type().is_symlink(), "{} not a symlink", other.display());
            assert_eq!(fs::read_link(e.path()).unwrap(), fs::read_link(&other).unwrap());
        } else if ft.is_dir() {
            assert!(md.is_dir());
        } else {
            assert_eq!(fs::read(e.path()).unwrap(), fs::read(&other).unwrap(), "content differs: {}", rel.display());
        }
    }
}

fn roundtrip(ext: &str) {
    let tmp = tempfile::tempdir().unwrap();
    let tree = make_tree(tmp.path());
    let archive = tmp.path().join(format!("a.{ext}"));
    let opts = CreateOptions { base_dir: Some(tmp.path().to_path_buf()), ..Default::default() };
    let st = archivix::create(&archive, &[PathBuf::from("tree")], &opts).unwrap();
    assert!(st.entries >= 8, "{ext}: {st:?}");

    let (_, entries) = archivix::list(&archive, None).unwrap();
    assert!(entries.iter().any(|e| e.path == "tree/ünï/日本/f.txt" && e.kind == EntryKind::File), "{ext}: listing");

    let rep = archivix::test(&archive, None).unwrap();
    assert!(rep.errors.is_empty(), "{ext}: {:?}", rep.errors);

    let mut buf = Vec::new();
    archivix::cat(&archive, "tree/ünï/日本/f.txt", None, &mut buf).unwrap();
    assert_eq!(buf, b"hello w\xc3\xb6rld\n", "{ext}: cat");

    let out = tmp.path().join("out");
    archivix::extract(&archive, &ExtractOptions { dest: out.clone(), ..Default::default() }).unwrap();
    assert_same(&tree, &out.join("tree"));
}

#[test]
fn zip() {
    roundtrip("zip")
}
#[test]
fn tar() {
    roundtrip("tar")
}
#[test]
fn tar_gz() {
    roundtrip("tar.gz")
}
#[test]
fn tar_zst() {
    roundtrip("tar.zst")
}
#[test]
fn tar_xz() {
    roundtrip("tar.xz")
}
#[test]
fn tar_bz2() {
    roundtrip("tar.bz2")
}
#[test]
fn tar_lz4() {
    roundtrip("tar.lz4")
}
#[test]
fn tar_br() {
    roundtrip("tar.br")
}
#[test]
fn sevenz() {
    roundtrip("7z")
}

#[test]
fn indexed_archives_are_plain_streams() {
    // The hidden index must not disturb standard decoders.
    let tmp = tempfile::tempdir().unwrap();
    let tree = make_tree(tmp.path());
    for ext in ["tar.gz", "tar.zst", "tar.lz4"] {
        let a = tmp.path().join(format!("i.{ext}"));
        archivix::create(&a, std::slice::from_ref(&tree), &CreateOptions::default()).unwrap();
        let info = archivix::info(&a, None).unwrap();
        assert!(info.instant_listing && info.random_access && info.parallel_decode, "{ext}: {info:?}");
        let codec = match archivix::format::detect(&a).unwrap() {
            Format::Tar(c) => c,
            f => panic!("{f:?}"),
        };
        let mut r = archivix::codec::decoder(codec, fs::File::open(&a).unwrap()).unwrap();
        let mut ar = tar::Archive::new(&mut r);
        let n = ar.entries().unwrap().count();
        assert_eq!(n, archivix::list(&a, None).unwrap().1.len(), "{ext}");
    }
}

#[test]
fn rejects_path_traversal() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("evil.tar");
    {
        let mut b = tar::Builder::new(fs::File::create(&a).unwrap());
        let data = b"pwned";
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        // Write the raw name to bypass the tar crate's own path checks.
        let name = b"../escaped.txt";
        h.as_old_mut().name[..name.len()].copy_from_slice(name);
        h.set_cksum();
        b.append(&h, &data[..]).unwrap();
        b.finish().unwrap();
    }
    let out = tmp.path().join("dest");
    archivix::extract(&a, &ExtractOptions { dest: out.clone(), ..Default::default() }).unwrap();
    assert!(!tmp.path().join("escaped.txt").exists(), "path traversal escaped the destination");
}

#[test]
fn sanitize_paths() {
    use archivix::extract::sanitize;
    assert_eq!(sanitize("/etc/passwd", 0), Some(PathBuf::from("etc/passwd")));
    assert_eq!(sanitize("a/../../b", 0), None);
    assert_eq!(sanitize("C:\\win\\x", 0), Some(PathBuf::from("win/x")));
    assert_eq!(sanitize("a/b/c", 1), Some(PathBuf::from("b/c")));
    assert_eq!(sanitize("a", 1), None);
}

#[test]
fn single_stream_compress() {
    let tmp = tempfile::tempdir().unwrap();
    let tree = make_tree(tmp.path());
    let input = tree.join("big.txt");
    for codec in [
        archivix::Codec::Zstd,
        archivix::Codec::Gzip,
        archivix::Codec::Lz4,
        archivix::Codec::Bzip2,
        archivix::Codec::Xz,
        archivix::Codec::Brotli,
    ] {
        let out = tmp.path().join(format!("big.txt.{}", codec.extension()));
        archivix::compress_file(&input, &out, codec, None, Some(256 << 10), true).unwrap();
        let mut dec = Vec::new();
        archivix::decompress_to(&out, &mut dec).unwrap();
        assert_eq!(dec, fs::read(&input).unwrap(), "{codec:?}");
    }
}

#[test]
fn literal_patterns_match_bracketed_names() {
    use archivix::extract::Matcher;
    let glob = Matcher::new(&["photo [1].jpg".to_string()]).unwrap();
    assert!(!glob.matches("photo [1].jpg"), "as a glob, [1] is a character class");
    let lit = Matcher::literal(&["photo [1].jpg".to_string(), "dir".to_string()]);
    assert!(lit.matches("photo [1].jpg"));
    assert!(lit.matches("dir/inner.txt"));
    assert!(!lit.matches("photo 1.jpg"));
}
