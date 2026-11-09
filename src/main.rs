use anyhow::{Context, Result, bail};
use archivix::extract::{ExtractOptions, Matcher};
use archivix::util::{format_time, human, mode_string};
use archivix::{Codec, CreateOptions, EntryKind, Format};
use clap::{Parser, Subcommand};
use std::io::IsTerminal;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(
    name = "archivix",
    version,
    about = "Parallel archive engine: zip, tar.{gz,zst,xz,bz2,lz4,br}, 7z, rar — create, extract, list, read and search without extracting."
)]
struct Cli {
    /// Worker threads (default: all logical cores)
    #[arg(short = 'j', long, global = true)]
    threads: Option<usize>,
    /// Suppress the summary line
    #[arg(short, long, global = true)]
    quiet: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create an archive; format follows the extension (.zip .tar.zst .tgz .tar.xz .7z ...)
    #[command(visible_alias = "c")]
    Create {
        output: PathBuf,
        #[arg(required = true)]
        inputs: Vec<PathBuf>,
        /// Compression level (codec-native: zstd -7..22, gzip/zip 0..9, xz 0..9, bz2 1..9, br 0..11)
        #[arg(short, long, allow_hyphen_values = true)]
        level: Option<i32>,
        /// Override format: zip, 7z, tar, tar.gz, tar.zst, tar.xz, tar.bz2, tar.lz4, tar.br
        #[arg(short = 'F', long)]
        format: Option<String>,
        /// zip method: deflate (default), zstd, store, bzip2, xz
        #[arg(short, long)]
        method: Option<String>,
        /// Exclude glob (repeatable), e.g. --exclude 'node_modules' --exclude '*.o'
        #[arg(short = 'x', long)]
        exclude: Vec<String>,
        /// Follow symlinks instead of storing them
        #[arg(short = 'L', long)]
        follow: bool,
        /// Change to this directory before adding inputs (like tar -C)
        #[arg(short = 'C', long = "cd")]
        base_dir: Option<PathBuf>,
        /// Chunk size for parallel compression, e.g. 4M
        #[arg(long, value_parser = parse_size)]
        chunk: Option<usize>,
        /// Don't append the archivix index (instant listing / random access) to gz/zst/lz4
        #[arg(long)]
        no_index: bool,
        /// Compress everything, even data detected as incompressible (zip)
        #[arg(long)]
        no_smart_store: bool,
    },
    /// Extract an archive (all members, or the given names/dirs/globs)
    #[command(visible_alias = "x")]
    Extract {
        archive: PathBuf,
        members: Vec<String>,
        /// Destination directory
        #[arg(short = 'C', short_alias = 'o', long, default_value = ".")]
        dest: PathBuf,
        #[arg(long, default_value_t = 0)]
        strip_components: usize,
        /// Don't overwrite existing files
        #[arg(short, long)]
        keep: bool,
        #[arg(short, long)]
        password: Option<String>,
        /// Don't restore permissions
        #[arg(long)]
        no_perms: bool,
        /// Emit a JSON summary
        #[arg(long)]
        json: bool,
    },
    /// List members without extracting
    #[command(visible_aliases = ["ls", "l"])]
    List {
        archive: PathBuf,
        /// Only members matching these names/dirs/globs
        members: Vec<String>,
        /// Detailed listing (mode, size, date)
        #[arg(short, long)]
        long: bool,
        /// Machine-readable output (one JSON document)
        #[arg(long)]
        json: bool,
        #[arg(short, long)]
        password: Option<String>,
    },
    /// Print members' content to stdout without extracting
    Cat {
        archive: PathBuf,
        #[arg(required = true)]
        members: Vec<String>,
        #[arg(short, long)]
        password: Option<String>,
    },
    /// Search text members for a regex without extracting
    Grep {
        pattern: String,
        archive: PathBuf,
        /// Only search members matching these names/globs
        members: Vec<String>,
        #[arg(short, long)]
        ignore_case: bool,
        #[arg(short, long, default_value_t = 1000)]
        max: usize,
        #[arg(long)]
        json: bool,
        #[arg(short, long)]
        password: Option<String>,
    },
    /// Archive summary and capabilities
    Info {
        archive: PathBuf,
        #[arg(long)]
        json: bool,
        #[arg(short, long)]
        password: Option<String>,
    },
    /// Verify integrity by decoding every member (CRC / stream checksums)
    #[command(visible_alias = "t")]
    Test {
        archive: PathBuf,
        #[arg(long)]
        json: bool,
        #[arg(short, long)]
        password: Option<String>,
    },
    /// Compress a single file to a stream (file.zst, file.gz, ...)
    #[command(visible_alias = "z")]
    Compress {
        input: PathBuf,
        /// Output path (default: input + codec extension)
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Codec: zstd (default), gzip, xz, bzip2, lz4, brotli, lzma
        #[arg(short, long, default_value = "zstd")]
        codec: String,
        #[arg(short, long, allow_hyphen_values = true)]
        level: Option<i32>,
        #[arg(long, value_parser = parse_size)]
        chunk: Option<usize>,
        #[arg(long)]
        no_index: bool,
        /// Remove the input after success
        #[arg(long)]
        rm: bool,
    },
    /// Decompress a single stream (auto-detected codec)
    #[command(visible_alias = "d")]
    Decompress {
        input: PathBuf,
        /// Output path ('-' for stdout; default: input without extension)
        #[arg(short, long)]
        output: Option<PathBuf>,
        #[arg(long)]
        rm: bool,
    },
    /// Run the MCP (Model Context Protocol) server on stdio for AI agents
    Mcp,
    /// Show supported formats and capabilities
    Formats {
        #[arg(long)]
        json: bool,
    },
}

fn parse_size(s: &str) -> Result<usize, String> {
    let s = s.trim();
    let (num, mul) = match s.chars().last().map(|c| c.to_ascii_uppercase()) {
        Some('K') => (&s[..s.len() - 1], 1usize << 10),
        Some('M') => (&s[..s.len() - 1], 1 << 20),
        Some('G') => (&s[..s.len() - 1], 1 << 30),
        _ => (s, 1),
    };
    num.parse::<usize>().map(|n| n * mul).map_err(|e| e.to_string())
}

fn parse_format(s: &str) -> Result<Format> {
    let s = s.trim_start_matches('.');
    Format::from_name(&format!("x.{s}")).with_context(|| format!("unknown format '{s}'"))
}

fn rate(bytes: u64, secs: f64) -> String {
    if secs <= 0.0 {
        return "-".into();
    }
    format!("{}/s", human((bytes as f64 / secs) as u64))
}

fn main() {
    let cli = Cli::parse();
    archivix::init_threads(cli.threads);
    if let Err(e) = run(cli) {
        eprintln!("archivix: error: {e:#}");
        std::process::exit(1);
    }
}

/// Live progress line on stderr, drawn by a background thread from the
/// library's global progress counters. Cleared on `stop()` or drop.
struct Bar {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Bar {
    fn start(phase: Option<&str>, enabled: bool) -> Bar {
        let stop = Arc::new(AtomicBool::new(false));
        let Some(phase) = phase.filter(|_| enabled) else { return Bar { stop, handle: None } };
        archivix::progress().reset(phase);
        let flag = stop.clone();
        let handle = std::thread::spawn(move || {
            let t0 = Instant::now();
            let mut drawn = false;
            let mut tick = 0usize;
            while !flag.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(80));
                // Quick commands finish without ever showing a bar.
                if t0.elapsed() < Duration::from_millis(200) || flag.load(Ordering::Relaxed) {
                    continue;
                }
                let line = render(&archivix::progress().snapshot(), t0.elapsed(), tick);
                tick += 1;
                eprint!("\r\x1b[2K{line}");
                let _ = std::io::stderr().flush();
                drawn = true;
            }
            if drawn {
                eprint!("\r\x1b[2K");
                let _ = std::io::stderr().flush();
            }
        });
        Bar { stop, handle: Some(handle) }
    }

    fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for Bar {
    fn drop(&mut self) {
        self.stop();
    }
}

fn term_width() -> usize {
    #[cfg(unix)]
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(libc::STDERR_FILENO, libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_col > 0 {
            return ws.ws_col as usize;
        }
    }
    std::env::var("COLUMNS").ok().and_then(|c| c.parse().ok()).unwrap_or(100)
}

fn fmt_dur(d: f64) -> String {
    let s = d.round() as u64;
    if s >= 3600 {
        format!("{}h{:02}m", s / 3600, s % 3600 / 60)
    } else if s >= 60 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}

fn render(p: &archivix::progress::Snapshot, elapsed: Duration, tick: usize) -> String {
    const SPIN: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    let secs = elapsed.as_secs_f64();
    let rate = if secs > 0.0 { format!("{}/s", human((p.bytes as f64 / secs) as u64)) } else { String::new() };
    let entries = match (p.entries, p.total_entries) {
        (0, _) => String::new(),
        (e, t) if t > 0 => format!("  {e}/{t} files"),
        (e, _) => format!("  {e} files"),
    };
    let width = term_width();
    let line = match p.fraction() {
        Some(f) => {
            let eta = if f > 0.01 && f < 1.0 { format!("  ETA {}", fmt_dur(secs * (1.0 - f) / f)) } else { String::new() };
            let tail = format!(" {:>3}%  {}  {rate}{entries}{eta}", (f * 100.0) as u32, human(p.bytes));
            let head = format!("{} {:<13} ", SPIN[tick % SPIN.len()], p.phase);
            let bar_w = width.saturating_sub(head.chars().count() + tail.chars().count() + 3).clamp(10, 40);
            let filled = f * bar_w as f64;
            let full = filled as usize;
            const PART: [char; 8] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉'];
            let mut bar: String = "█".repeat(full);
            if full < bar_w {
                bar.push(PART[((filled - full as f64) * 8.0) as usize]);
                bar.push_str(&"░".repeat(bar_w - full - 1));
            }
            format!("{head}▕{bar}▏{tail}")
        }
        None => format!("{} {:<13} {}  {rate}{entries}  {}", SPIN[tick % SPIN.len()], p.phase, human(p.bytes), fmt_dur(secs)),
    };
    line.chars().take(width.saturating_sub(1)).collect()
}

fn phase_of(cmd: &Cmd) -> Option<&'static str> {
    Some(match cmd {
        Cmd::Create { .. } | Cmd::Compress { .. } => "Compressing",
        Cmd::Extract { .. } => "Extracting",
        Cmd::List { .. } | Cmd::Info { .. } => "Reading",
        Cmd::Grep { .. } => "Searching",
        Cmd::Test { .. } => "Testing",
        Cmd::Decompress { .. } => "Decompressing",
        Cmd::Cat { .. } | Cmd::Mcp | Cmd::Formats { .. } => return None,
    })
}

fn run(cli: Cli) -> Result<()> {
    let quiet = cli.quiet;
    let t0 = Instant::now();
    let mut bar = Bar::start(phase_of(&cli.cmd), !quiet && std::io::stderr().is_terminal());
    match cli.cmd {
        Cmd::Create { output, inputs, level, format, method, exclude, follow, base_dir, chunk, no_index, no_smart_store } => {
            let opts = CreateOptions {
                format: format.as_deref().map(parse_format).transpose()?,
                level,
                method,
                chunk,
                index: !no_index,
                smart_store: !no_smart_store,
                follow_symlinks: follow,
                excludes: exclude,
                base_dir,
            };
            let st = archivix::create(&output, &inputs, &opts)?;
            bar.stop();
            if !quiet {
                let secs = t0.elapsed().as_secs_f64();
                let pct = if st.input_bytes > 0 { st.output_bytes as f64 * 100.0 / st.input_bytes as f64 } else { 0.0 };
                eprintln!(
                    "{}: {} entries, {} -> {} ({pct:.1}%) in {secs:.2}s ({})",
                    output.display(),
                    st.entries,
                    human(st.input_bytes),
                    human(st.output_bytes),
                    rate(st.input_bytes, secs)
                );
            }
        }
        Cmd::Extract { archive, members, dest, strip_components, keep, password, no_perms, json } => {
            let opts = ExtractOptions {
                dest,
                patterns: members,
                strip_components,
                keep_existing: keep,
                password,
                preserve_perms: !no_perms,
                ..Default::default()
            };
            let st = archivix::extract(&archive, &opts)?;
            bar.stop();
            let secs = t0.elapsed().as_secs_f64();
            if json {
                println!("{}", serde_json::to_string(&serde_json::json!({"stats": st, "seconds": secs}))?);
            } else if !quiet {
                eprintln!(
                    "{}: {} files, {} dirs, {} links, {} in {secs:.2}s ({})",
                    archive.display(),
                    st.files,
                    st.dirs,
                    st.links,
                    human(st.bytes),
                    rate(st.bytes, secs)
                );
            }
        }
        Cmd::List { archive, members, long, json, password } => {
            let (format, entries) = archivix::list(&archive, password.as_deref())?;
            bar.stop();
            let m = Matcher::new(&members)?;
            let entries: Vec<_> = entries.into_iter().filter(|e| m.matches(&e.path)).collect();
            let out = std::io::stdout();
            let mut out = std::io::BufWriter::new(out.lock());
            if json {
                serde_json::to_writer(&mut out, &serde_json::json!({"format": format.name(), "count": entries.len(), "entries": entries}))?;
                writeln!(out)?;
            } else if long {
                for e in &entries {
                    let name = if e.kind == EntryKind::Dir { format!("{}/", e.path) } else { e.path.clone() };
                    let link = e.link_target.as_ref().map(|l| format!(" -> {l}")).unwrap_or_default();
                    let c = e.compressed_size.map(|c| format!("{c:>12}")).unwrap_or_else(|| format!("{:>12}", ""));
                    writeln!(out, "{} {:>12} {c} {} {name}{link}", mode_string(e.kind, e.mode), e.size, format_time(e.mtime))?;
                }
                let total: u64 = entries.iter().map(|e| e.size).sum();
                writeln!(out, "{} entries, {} ({})", entries.len(), human(total), format.name())?;
            } else {
                for e in &entries {
                    if e.kind == EntryKind::Dir {
                        writeln!(out, "{}/", e.path)?;
                    } else {
                        writeln!(out, "{}", e.path)?;
                    }
                }
            }
            out.flush()?;
        }
        Cmd::Cat { archive, members, password } => {
            let out = std::io::stdout();
            let mut out = std::io::BufWriter::with_capacity(1 << 20, out.lock());
            for m in members {
                archivix::cat(&archive, &m, password.as_deref(), &mut out)?;
            }
            out.flush()?;
        }
        Cmd::Grep { pattern, archive, members, ignore_case, max, json, password } => {
            let re = regex::bytes::RegexBuilder::new(&pattern).case_insensitive(ignore_case).build()?;
            let m = Matcher::new(&members)?;
            let hits = archivix::grep(&archive, &re, &m, max, password.as_deref())?;
            bar.stop();
            if json {
                println!("{}", serde_json::to_string(&serde_json::json!({"matches": hits.len(), "results": hits}))?);
            } else {
                for h in &hits {
                    println!("{}:{}:{}", h.path, h.line, h.text);
                }
            }
            if hits.is_empty() {
                std::process::exit(1);
            }
        }
        Cmd::Info { archive, json, password } => {
            let i = archivix::info(&archive, password.as_deref())?;
            bar.stop();
            if json {
                println!("{}", serde_json::to_string_pretty(&i)?);
            } else {
                println!("archive:          {}", i.path);
                println!("format:           {}", i.format);
                println!("entries:          {} ({} files, {} dirs, {} links)", i.entries, i.files, i.dirs, i.links);
                println!("uncompressed:     {} ({} bytes)", human(i.total_size), i.total_size);
                println!("archive size:     {} ({:.1}%)", human(i.archive_size), i.ratio * 100.0);
                println!("instant listing:  {}", i.instant_listing);
                println!("random access:    {}", i.random_access);
                println!("parallel decode:  {}", i.parallel_decode);
                if i.encrypted_entries > 0 {
                    println!("encrypted:        {} entries", i.encrypted_entries);
                }
            }
        }
        Cmd::Test { archive, json, password } => {
            let r = archivix::test(&archive, password.as_deref())?;
            bar.stop();
            let secs = t0.elapsed().as_secs_f64();
            if json {
                println!("{}", serde_json::to_string(&serde_json::json!({"ok": r.errors.is_empty(), "report": r, "seconds": secs}))?);
            } else {
                for e in &r.errors {
                    eprintln!("FAIL {e}");
                }
                if !quiet {
                    eprintln!(
                        "{}: {} entries, {} checked in {secs:.2}s ({}) — {}",
                        archive.display(),
                        r.entries,
                        human(r.bytes),
                        rate(r.bytes, secs),
                        if r.errors.is_empty() { "OK" } else { "ERRORS" }
                    );
                }
            }
            if !r.errors.is_empty() {
                std::process::exit(2);
            }
        }
        Cmd::Compress { input, output, codec, level, chunk, no_index, rm } => {
            let c = Codec::parse(&codec).with_context(|| format!("unknown codec '{codec}'"))?;
            if c == Codec::None {
                bail!("choose a codec");
            }
            let output = output.unwrap_or_else(|| PathBuf::from(format!("{}.{}", input.display(), c.extension())));
            let st = archivix::compress_file(&input, &output, c, level, chunk, !no_index)?;
            bar.stop();
            if rm {
                std::fs::remove_file(&input)?;
            }
            if !quiet {
                let secs = t0.elapsed().as_secs_f64();
                eprintln!(
                    "{}: {} -> {} ({:.1}%) in {secs:.2}s ({})",
                    output.display(),
                    human(st.input_bytes),
                    human(st.output_bytes),
                    st.output_bytes as f64 * 100.0 / st.input_bytes.max(1) as f64,
                    rate(st.input_bytes, secs)
                );
            }
        }
        Cmd::Decompress { input, output, rm } => {
            let to_stdout = output.as_ref().is_some_and(|o| o.as_os_str() == "-");
            if to_stdout {
                let out = std::io::stdout();
                let mut w = std::io::BufWriter::with_capacity(1 << 20, out.lock());
                archivix::decompress_to(&input, &mut w)?;
                bar.stop();
                w.flush()?;
            } else {
                let out = match output {
                    Some(o) => o,
                    None => {
                        let stem = input.file_stem().context("input has no extension to strip; pass -o")?;
                        let p = input.with_file_name(stem);
                        if p == input {
                            bail!("cannot derive output name; pass -o");
                        }
                        p
                    }
                };
                let mut w = std::io::BufWriter::with_capacity(1 << 20, std::fs::File::create(&out)?);
                let n = archivix::decompress_to(&input, &mut w)?;
                bar.stop();
                w.flush()?;
                if !quiet {
                    let secs = t0.elapsed().as_secs_f64();
                    eprintln!("{}: {} in {secs:.2}s ({})", out.display(), human(n), rate(n, secs));
                }
            }
            if rm {
                std::fs::remove_file(&input)?;
            }
        }
        Cmd::Mcp => archivix::mcp::serve()?,
        Cmd::Formats { json } => {
            let rows = [
                ("zip / jar / apk / docx ...", "yes", "yes", "instant (central directory)", "parallel per entry"),
                ("tar", "yes", "yes", "instant (seek over data)", "parallel file writes"),
                ("tar.zst / .zst", "parallel", "yes", "instant with archivix index", "parallel (multi-frame)"),
                ("tar.gz / tgz / .gz", "parallel", "yes", "instant with archivix index", "parallel with index"),
                ("tar.lz4 / .lz4", "parallel", "yes", "instant with archivix index", "parallel with index"),
                ("tar.bz2 / .bz2", "parallel", "yes", "stream decode", "parallel (multi-stream)"),
                ("tar.xz / .xz", "multithreaded", "yes", "stream decode", "multithreaded (multi-block)"),
                ("tar.br / .br", "yes", "yes", "stream decode", "sequential"),
                ("tar.lzma / .lzma", "yes", "yes", "stream decode", "sequential"),
                ("7z", "multithreaded LZMA2", "yes", "instant (header)", "parallel per block"),
                (
                    "rar (v1.5-v5)",
                    "no (proprietary)",
                    if cfg!(feature = "rar") { "yes" } else { "not compiled" },
                    "instant (headers)",
                    "sequential",
                ),
            ];
            if json {
                let v: Vec<_> = rows
                    .iter()
                    .map(|r| serde_json::json!({"format": r.0, "create": r.1, "extract": r.2, "list": r.3, "decode": r.4}))
                    .collect();
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                println!("{:<28} {:<22} {:<13} {:<30} DECODE", "FORMAT", "CREATE", "EXTRACT", "LIST");
                for r in rows {
                    println!("{:<28} {:<22} {:<13} {:<30} {}", r.0, r.1, r.2, r.3, r.4);
                }
            }
        }
    }
    Ok(())
}
