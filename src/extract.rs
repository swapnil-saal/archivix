//! Safe, parallel extraction to disk.
//!
//! * Every member path is sanitised (no absolute paths, no `..`, no drive
//!   letters) — zip-slip / tar path traversal is impossible.
//! * Files are opened with O_NOFOLLOW, and archive symlinks/hardlinks are only
//!   created after all regular files are written, so an archive cannot plant a
//!   symlink and then write through it.
//! * Small files are written on the rayon pool (bounded by a memory budget), so
//!   archives with many small files are not limited by per-file syscall latency.

use crate::entry::{Entry, EntryKind};
use anyhow::{Result, anyhow};
use globset::{Glob, GlobSet, GlobSetBuilder};
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

#[derive(Debug, Clone)]
pub struct ExtractOptions {
    pub dest: PathBuf,
    pub patterns: Vec<String>,
    pub strip_components: usize,
    pub keep_existing: bool,
    pub password: Option<String>,
    pub preserve_mtime: bool,
    pub preserve_perms: bool,
    /// Treat `patterns` as exact member names (and directory prefixes), never
    /// as globs, so names containing `[`, `*` or `?` match themselves.
    pub literal: bool,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        ExtractOptions {
            dest: PathBuf::from("."),
            patterns: Vec::new(),
            strip_components: 0,
            keep_existing: false,
            password: None,
            preserve_mtime: true,
            preserve_perms: true,
            literal: false,
        }
    }
}

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct ExtractStats {
    pub files: u64,
    pub dirs: u64,
    pub links: u64,
    pub bytes: u64,
    pub skipped: u64,
}

/// Matches member names against user patterns: exact names, directory
/// prefixes (`docs` matches `docs/a.txt`) and globs (`*.rs`, `src/**`).
#[derive(Clone)]
pub struct Matcher {
    exact: HashSet<String>,
    globs: Option<GlobSet>,
    empty: bool,
}

impl Matcher {
    pub fn new(patterns: &[String]) -> Result<Matcher> {
        let mut exact = HashSet::new();
        let mut b = GlobSetBuilder::new();
        let mut any_glob = false;
        for p in patterns {
            let p = p.trim_start_matches("./").trim_end_matches('/');
            if p.contains(['*', '?', '[', '{']) {
                b.add(Glob::new(p)?);
                // Allow a basename glob like "*.rs" to match at any depth.
                if !p.contains('/') {
                    b.add(Glob::new(&format!("**/{p}"))?);
                }
                any_glob = true;
            } else {
                exact.insert(p.to_string());
            }
        }
        Ok(Matcher { empty: patterns.is_empty(), exact, globs: if any_glob { Some(b.build()?) } else { None } })
    }

    /// Exact names and directory prefixes only; no glob interpretation.
    pub fn literal(patterns: &[String]) -> Matcher {
        let exact = patterns.iter().map(|p| p.trim_start_matches("./").trim_end_matches('/').to_string()).collect();
        Matcher { empty: patterns.is_empty(), exact, globs: None }
    }

    pub fn is_empty(&self) -> bool {
        self.empty
    }

    pub fn matches(&self, path: &str) -> bool {
        if self.empty {
            return true;
        }
        let p = path.trim_start_matches("./").trim_end_matches('/');
        if self.exact.contains(p) {
            return true;
        }
        // directory prefix match
        let mut cur = p;
        while let Some((parent, _)) = cur.rsplit_once('/') {
            if self.exact.contains(parent) {
                return true;
            }
            cur = parent;
        }
        self.globs.as_ref().is_some_and(|g| g.is_match(p))
    }
}

/// Turn an archive member name into a safe relative path, or None if it would
/// escape the destination (or is empty after stripping).
pub fn sanitize(name: &str, strip: usize) -> Option<PathBuf> {
    let name = name.replace('\\', "/");
    let mut parts: Vec<&str> = Vec::new();
    for comp in Path::new(&name).components() {
        match comp {
            Component::Normal(s) => {
                let s = s.to_str()?;
                // Reject Windows drive prefixes smuggled as normal components ("C:").
                if s.len() == 2 && s.ends_with(':') {
                    continue;
                }
                parts.push(s);
            }
            Component::ParentDir => return None,
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
        }
    }
    if parts.len() <= strip {
        return None;
    }
    Some(parts[strip..].iter().collect())
}

/// Directory whose mtime/mode are restored after its contents are written.
type DirFixup = (PathBuf, Option<i64>, Option<u32>);

struct Budget {
    state: Mutex<(usize, u64)>, // (tasks in flight, bytes in flight)
    cv: Condvar,
}

pub struct Extractor {
    root: PathBuf,
    opts: ExtractOptions,
    matcher: Matcher,
    made_dirs: Mutex<HashSet<PathBuf>>,
    symlinks: Mutex<Vec<(PathBuf, String)>>,
    hardlinks: Mutex<Vec<(PathBuf, PathBuf)>>,
    dirs: Mutex<Vec<DirFixup>>,
    errors: Mutex<Vec<String>>,
    budget: Budget,
    files: AtomicU64,
    bytes: AtomicU64,
    skipped: AtomicU64,
}

const MAX_INFLIGHT_BYTES: u64 = 512 << 20;
/// Entries up to this size are buffered and written asynchronously.
pub const ASYNC_LIMIT: u64 = 4 << 20;

impl Extractor {
    pub fn new(opts: ExtractOptions) -> Result<Arc<Extractor>> {
        fs::create_dir_all(&opts.dest)?;
        let root = fs::canonicalize(&opts.dest)?;
        let matcher = if opts.literal { Matcher::literal(&opts.patterns) } else { Matcher::new(&opts.patterns)? };
        Ok(Arc::new(Extractor {
            root,
            opts,
            matcher,
            made_dirs: Mutex::new(HashSet::new()),
            symlinks: Mutex::new(Vec::new()),
            hardlinks: Mutex::new(Vec::new()),
            dirs: Mutex::new(Vec::new()),
            errors: Mutex::new(Vec::new()),
            budget: Budget { state: Mutex::new((0, 0)), cv: Condvar::new() },
            files: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            skipped: AtomicU64::new(0),
        }))
    }

    pub fn options(&self) -> &ExtractOptions {
        &self.opts
    }

    pub fn wants(&self, entry_path: &str) -> bool {
        self.matcher.matches(entry_path)
    }

    /// Destination path for a member, or None if it must be skipped.
    pub fn target(&self, name: &str) -> Option<PathBuf> {
        match sanitize(name, self.opts.strip_components) {
            Some(rel) => Some(self.root.join(rel)),
            None => {
                if name.contains("..") {
                    self.warn(format!("skipping unsafe path: {name}"));
                }
                self.skipped.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }

    pub fn warn(&self, msg: String) {
        eprintln!("archivix: warning: {msg}");
    }

    fn ensure_dir(&self, dir: &Path) -> io::Result<()> {
        {
            let made = self.made_dirs.lock().unwrap();
            if made.contains(dir) {
                return Ok(());
            }
        }
        fs::create_dir_all(dir)?;
        self.made_dirs.lock().unwrap().insert(dir.to_path_buf());
        Ok(())
    }

    fn ensure_parent(&self, path: &Path) -> io::Result<()> {
        match path.parent() {
            Some(p) => self.ensure_dir(p),
            None => Ok(()),
        }
    }

    fn mode_for(&self, mode: Option<u32>, is_dir: bool) -> u32 {
        let default = if is_dir { 0o755 } else { 0o644 };
        if !self.opts.preserve_perms {
            return default;
        }
        // Strip setuid/setgid; ensure owner can read/write (dirs: traverse).
        let m = mode.map(|m| m & 0o1777).unwrap_or(default);
        if is_dir { m | 0o700 } else { m | 0o600 }
    }

    fn open_new(&self, path: &Path, mode: Option<u32>) -> io::Result<Option<File>> {
        self.ensure_parent(path)?;
        let mut oo = OpenOptions::new();
        oo.write(true).create(true);
        if self.opts.keep_existing {
            oo.create_new(true);
        } else {
            oo.truncate(true);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            oo.custom_flags(libc::O_NOFOLLOW).mode(self.mode_for(mode, false));
        }
        let _ = mode;
        match oo.open(path) {
            Ok(f) => Ok(Some(f)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && self.opts.keep_existing => {
                self.skipped.fetch_add(1, Ordering::Relaxed);
                Ok(None)
            }
            Err(e) => {
                // A symlink (ELOOP) or directory sits where the file goes: replace it.
                if let Ok(md) = fs::symlink_metadata(path) {
                    if self.opts.keep_existing {
                        self.skipped.fetch_add(1, Ordering::Relaxed);
                        return Ok(None);
                    }
                    if md.file_type().is_symlink() || md.is_file() {
                        fs::remove_file(path)?;
                    } else if md.is_dir() {
                        fs::remove_dir_all(path)?;
                    }
                    return oo.open(path).map(Some);
                }
                Err(e)
            }
        }
    }

    fn finish_file(&self, f: &File, mtime: Option<i64>, n: u64) {
        if self.opts.preserve_mtime
            && let Some(t) = mtime
        {
            let _ = filetime::set_file_handle_times(f, None, Some(filetime::FileTime::from_unix_time(t, 0)));
        }
        self.files.fetch_add(1, Ordering::Relaxed);
        self.bytes.fetch_add(n, Ordering::Relaxed);
        crate::progress().add_entry();
    }

    /// Stream a (large) member to disk on the calling thread.
    pub fn write_stream(&self, path: &Path, r: &mut dyn Read, mode: Option<u32>, mtime: Option<i64>) -> Result<u64> {
        let Some(f) = self.open_new(path, mode)? else { return Ok(0) };
        let mut w = io::BufWriter::with_capacity(1 << 20, f);
        let n = io::copy(r, &mut w)?;
        let f = w.into_inner().map_err(|e| e.into_error())?;
        self.finish_file(&f, mtime, n);
        Ok(n)
    }

    /// Write a buffered member, synchronously.
    pub fn write_bytes(&self, path: &Path, data: &[u8], mode: Option<u32>, mtime: Option<i64>) -> Result<()> {
        let Some(mut f) = self.open_new(path, mode)? else { return Ok(()) };
        f.write_all(data)?;
        self.finish_file(&f, mtime, data.len() as u64);
        Ok(())
    }

    /// Write a buffered member on the thread pool. Blocks if too much data is in flight.
    pub fn write_async(self: &Arc<Self>, path: PathBuf, data: Vec<u8>, mode: Option<u32>, mtime: Option<i64>) {
        let size = data.len() as u64;
        {
            let mut st = self.budget.state.lock().unwrap();
            while st.1 > 0 && st.1 + size > MAX_INFLIGHT_BYTES {
                st = self.budget.cv.wait(st).unwrap();
            }
            st.0 += 1;
            st.1 += size;
        }
        let me = self.clone();
        rayon::spawn(move || {
            if let Err(e) = me.write_bytes(&path, &data, mode, mtime) {
                me.errors.lock().unwrap().push(format!("{}: {e}", path.display()));
            }
            let mut st = me.budget.state.lock().unwrap();
            st.0 -= 1;
            st.1 -= size;
            me.budget.cv.notify_all();
        });
    }

    pub fn record_error(&self, msg: String) {
        self.errors.lock().unwrap().push(msg);
    }

    pub fn make_dir(&self, path: &Path, mtime: Option<i64>, mode: Option<u32>) -> Result<()> {
        self.ensure_dir(path)?;
        self.dirs.lock().unwrap().push((path.to_path_buf(), mtime, mode));
        Ok(())
    }

    pub fn defer_symlink(&self, path: PathBuf, target: String) {
        self.symlinks.lock().unwrap().push((path, target));
    }

    /// `link_name` is the archive name of the existing member this hardlink points to.
    pub fn defer_hardlink(&self, path: PathBuf, link_name: &str) {
        match self.target(link_name) {
            Some(src) => self.hardlinks.lock().unwrap().push((path, src)),
            None => self.warn(format!("skipping hardlink to unsafe path: {link_name}")),
        }
    }

    /// Handle one member generically given its metadata and a reader for its content.
    pub fn extract_entry(self: &Arc<Self>, e: &Entry, r: &mut dyn Read) -> Result<()> {
        if !self.wants(&e.path) {
            return Ok(());
        }
        let Some(path) = self.target(&e.path) else { return Ok(()) };
        match e.kind {
            EntryKind::Dir => self.make_dir(&path, e.mtime, e.mode)?,
            EntryKind::Symlink => {
                let target = match &e.link_target {
                    Some(t) => t.clone(),
                    None => {
                        let mut s = String::new();
                        r.read_to_string(&mut s)?;
                        s
                    }
                };
                self.defer_symlink(path, target);
            }
            EntryKind::Hardlink => {
                if let Some(t) = &e.link_target {
                    self.defer_hardlink(path, t);
                }
            }
            EntryKind::File | EntryKind::Other => {
                // Inside a rayon worker we are already parallel: write in place
                // (blocking on the budget from a worker could starve the pool).
                if e.size <= ASYNC_LIMIT {
                    let mut buf = Vec::with_capacity(e.size as usize);
                    r.read_to_end(&mut buf)?;
                    // macOS tar stores xattrs as AppleDouble "._name" members; like
                    // bsdtar on macOS, don't materialize them as files.
                    if cfg!(target_os = "macos")
                        && e.path.rsplit('/').next().is_some_and(|b| b.starts_with("._"))
                        && buf.starts_with(&[0x00, 0x05, 0x16, 0x07])
                    {
                        self.skipped.fetch_add(1, Ordering::Relaxed);
                        return Ok(());
                    }
                    if rayon::current_thread_index().is_some() {
                        self.write_bytes(&path, &buf, e.mode, e.mtime)?;
                    } else {
                        self.write_async(path, buf, e.mode, e.mtime);
                    }
                } else {
                    self.write_stream(&path, r, e.mode, e.mtime)?;
                }
            }
        }
        Ok(())
    }

    fn wait_idle(&self) {
        let mut st = self.budget.state.lock().unwrap();
        while st.0 > 0 {
            st = self.budget.cv.wait(st).unwrap();
        }
    }

    /// Wait for writes, then create links and fix directory metadata.
    pub fn finish(&self) -> Result<ExtractStats> {
        self.wait_idle();
        let mut links = 0u64;
        for (path, src) in self.hardlinks.lock().unwrap().drain(..) {
            self.ensure_parent(&path)?;
            let _ = fs::remove_file(&path);
            if fs::hard_link(&src, &path).is_err() {
                fs::copy(&src, &path).map_err(|e| anyhow!("hardlink {}: {e}", path.display()))?;
            }
            links += 1;
        }
        for (path, target) in self.symlinks.lock().unwrap().drain(..) {
            self.ensure_parent(&path)?;
            if fs::symlink_metadata(&path).is_ok() {
                if self.opts.keep_existing {
                    continue;
                }
                let _ = fs::remove_file(&path);
            }
            #[cfg(unix)]
            std::os::unix::fs::symlink(&target, &path).map_err(|e| anyhow!("symlink {}: {e}", path.display()))?;
            #[cfg(windows)]
            {
                let _ = std::os::windows::fs::symlink_file(&target, &path);
            }
            links += 1;
        }
        let mut dirs = std::mem::take(&mut *self.dirs.lock().unwrap());
        let ndirs = dirs.len() as u64;
        // Deepest first so setting a parent's mtime is not undone by a child.
        dirs.sort_by_key(|d| std::cmp::Reverse(d.0.components().count()));
        for (path, mtime, mode) in dirs {
            #[cfg(unix)]
            if self.opts.preserve_perms
                && let Some(m) = mode
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&path, fs::Permissions::from_mode(self.mode_for(Some(m), true)));
            }
            let _ = mode;
            if self.opts.preserve_mtime
                && let Some(t) = mtime
            {
                let _ = filetime::set_file_mtime(&path, filetime::FileTime::from_unix_time(t, 0));
            }
        }
        let errors = std::mem::take(&mut *self.errors.lock().unwrap());
        if !errors.is_empty() {
            let n = errors.len();
            let first: Vec<_> = errors.into_iter().take(5).collect();
            return Err(anyhow!("{n} entries failed to extract:\n  {}", first.join("\n  ")));
        }
        Ok(ExtractStats {
            files: self.files.load(Ordering::Relaxed),
            dirs: ndirs,
            links,
            bytes: self.bytes.load(Ordering::Relaxed),
            skipped: self.skipped.load(Ordering::Relaxed),
        })
    }
}
