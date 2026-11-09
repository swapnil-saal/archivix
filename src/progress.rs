//! Process-wide progress reporting and cancellation.
//!
//! Every operation (create, extract, list, test, compress, decompress) updates
//! one global set of atomic counters. A front end (the CLI's progress bar, the
//! GUI) calls [`Progress::reset`] before starting an operation and polls
//! [`Progress::snapshot`] from another thread. Updates are relaxed atomic adds
//! per buffer or entry, so the overhead is negligible.
//!
//! The counters are global, so run one reported operation at a time.
//!
//! * `done / total` is the completion fraction. Its unit depends on the
//!   operation: input bytes for create, uncompressed bytes for zip/7z/rar
//!   extraction, compressed bytes consumed for streamed tar/gz/zst/xz/... data.
//!   `total == 0` means "unknown" (show a spinner).
//! * `bytes` counts payload bytes processed, for throughput display.

use std::io::{self, Read};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

pub struct Progress {
    total: AtomicU64,
    done: AtomicU64,
    bytes: AtomicU64,
    entries: AtomicU64,
    total_entries: AtomicU64,
    cancel: AtomicBool,
    phase: Mutex<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub phase: String,
    pub total: u64,
    pub done: u64,
    pub bytes: u64,
    pub entries: u64,
    pub total_entries: u64,
}

impl Snapshot {
    /// Completion in 0.0..=1.0, or None when the total is unknown.
    pub fn fraction(&self) -> Option<f64> {
        (self.total > 0).then(|| (self.done as f64 / self.total as f64).clamp(0.0, 1.0))
    }
}

static GLOBAL: Progress = Progress {
    total: AtomicU64::new(0),
    done: AtomicU64::new(0),
    bytes: AtomicU64::new(0),
    entries: AtomicU64::new(0),
    total_entries: AtomicU64::new(0),
    cancel: AtomicBool::new(false),
    phase: Mutex::new(String::new()),
};

/// The process-wide progress tracker.
pub fn progress() -> &'static Progress {
    &GLOBAL
}

impl Progress {
    /// Start a new operation: zero all counters and clear cancellation.
    pub fn reset(&self, phase: &str) {
        for c in [&self.total, &self.done, &self.bytes, &self.entries, &self.total_entries] {
            c.store(0, Relaxed);
        }
        self.cancel.store(false, Relaxed);
        self.set_phase(phase);
    }
    pub fn set_phase(&self, phase: &str) {
        if let Ok(mut p) = self.phase.lock() {
            p.clear();
            p.push_str(phase);
        }
    }
    pub fn set_total(&self, total: u64) {
        self.total.store(total, Relaxed);
    }
    pub fn set_total_entries(&self, n: u64) {
        self.total_entries.store(n, Relaxed);
    }
    pub fn add_done(&self, n: u64) {
        self.done.fetch_add(n, Relaxed);
    }
    pub fn add_bytes(&self, n: u64) {
        self.bytes.fetch_add(n, Relaxed);
    }
    /// Count `n` bytes as both completed work and processed payload.
    pub fn add(&self, n: u64) {
        self.add_done(n);
        self.add_bytes(n);
    }
    pub fn add_entry(&self) {
        self.entries.fetch_add(1, Relaxed);
    }
    /// Mark the work as complete (e.g. after a whole-buffer decode).
    pub fn finish_done(&self) {
        self.done.store(self.total.load(Relaxed), Relaxed);
    }

    /// Ask the running operation to stop at the next checkpoint.
    pub fn cancel(&self) {
        self.cancel.store(true, Relaxed);
    }
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Relaxed)
    }
    /// `Err(cancelled)` if cancellation was requested.
    pub fn check(&self) -> io::Result<()> {
        if self.is_cancelled() { Err(cancelled()) } else { Ok(()) }
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            phase: self.phase.lock().map(|p| p.clone()).unwrap_or_default(),
            total: self.total.load(Relaxed),
            done: self.done.load(Relaxed),
            bytes: self.bytes.load(Relaxed),
            entries: self.entries.load(Relaxed),
            total_entries: self.total_entries.load(Relaxed),
        }
    }
}

pub fn cancelled() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "cancelled")
}

/// Is this error a user cancellation?
pub fn is_cancel_error(e: &anyhow::Error) -> bool {
    e.chain()
        .any(|c| c.downcast_ref::<io::Error>().is_some_and(|io| io.kind() == io::ErrorKind::Interrupted && io.to_string() == "cancelled"))
}

/// What a [`Counted`] reader adds its byte count to.
#[derive(Clone, Copy)]
pub enum Count {
    /// Completed work and processed payload.
    Both,
    /// Completed work only (e.g. compressed bytes consumed).
    Done,
    /// Processed payload only (e.g. decompressed output).
    Bytes,
}

/// Reader wrapper that reports bytes read to the global progress and stops
/// with a "cancelled" error once cancellation is requested.
pub struct Counted<R> {
    inner: R,
    count: Count,
}

impl<R> Counted<R> {
    pub fn new(inner: R, count: Count) -> Self {
        Counted { inner, count }
    }
}

impl<R: Read> Read for Counted<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let p = progress();
        p.check()?;
        let n = self.inner.read(buf)?;
        match self.count {
            Count::Both => p.add(n as u64),
            Count::Done => p.add_done(n as u64),
            Count::Bytes => p.add_bytes(n as u64),
        }
        Ok(n)
    }
}
