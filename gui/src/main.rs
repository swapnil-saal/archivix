//! archivix desktop app: open, browse, preview, search, extract and create
//! archives. A thin UI over the `archivix` library; all heavy work runs on a
//! background thread and reports through `archivix::progress()`.

#![cfg_attr(windows, windows_subsystem = "windows")]

mod icon;

use archivix::extract::ExtractOptions;
use archivix::util::{format_time, human};
use archivix::{CreateOptions, Entry, EntryKind, Info};
use eframe::egui::{self, Color32, RichText, Vec2};
use egui_extras::{Column, TableBuilder};
use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

const ACCENT: Color32 = Color32::from_rgb(0x4F, 0x5B, 0xE8);
const OK: Color32 = Color32::from_rgb(0x1F, 0xA8, 0x6B);
const ERR: Color32 = Color32::from_rgb(0xE0, 0x4F, 0x4F);
const PREVIEW_MAX: u64 = 256 * 1024;

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--write-iconset") {
        let dir = args.get(1).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("archivix.iconset"));
        if let Err(e) = icon::write_iconset(&dir) {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("--write-png") {
        let path = args.get(1).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("archivix.png"));
        let size = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(512);
        if let Err(e) = icon::write_png(&path, size) {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
        return Ok(());
    }
    let initial: Vec<PathBuf> = args.iter().filter(|a| !a.starts_with("--")).map(PathBuf::from).collect();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("archivix")
            .with_inner_size([980.0, 640.0])
            .with_min_inner_size([640.0, 420.0])
            .with_drag_and_drop(true)
            .with_icon(icon::icon_data(256)),
        ..Default::default()
    };
    eframe::run_native("archivix", options, Box::new(move |cc| Ok(Box::new(App::new(cc, initial)))))
}

// ------------------------------------------------------------------ state

#[derive(PartialEq, Clone, Copy)]
enum Mode {
    Home,
    Browse,
    Create,
}

#[derive(PartialEq, Clone, Copy)]
enum SortCol {
    Name,
    Size,
    Packed,
    Modified,
}

struct Browse {
    path: PathBuf,
    info: Info,
    entries: Vec<Entry>,
    filter: String,
    view: Vec<usize>,
    sort: SortCol,
    ascending: bool,
    selected: BTreeSet<usize>,
    anchor: Option<usize>,
    preview: Option<Preview>,
    preview_for: Option<usize>,
}

impl Browse {
    fn rebuild_view(&mut self) {
        let f = self.filter.to_lowercase();
        self.view = (0..self.entries.len()).filter(|&i| f.is_empty() || self.entries[i].path.to_lowercase().contains(&f)).collect();
        let e = &self.entries;
        match self.sort {
            SortCol::Name => self.view.sort_by(|&a, &b| e[a].path.cmp(&e[b].path)),
            SortCol::Size => self.view.sort_by_key(|&i| e[i].size),
            SortCol::Packed => self.view.sort_by_key(|&i| e[i].compressed_size.unwrap_or(0)),
            SortCol::Modified => self.view.sort_by_key(|&i| e[i].mtime.unwrap_or(0)),
        }
        if !self.ascending {
            self.view.reverse();
        }
    }

    fn selected_paths(&self) -> Vec<String> {
        self.selected.iter().map(|&i| self.entries[i].path.clone()).collect()
    }
}

struct Preview {
    path: String,
    size: u64,
    text: Option<String>,
    hex: String,
    truncated: bool,
}

#[derive(Clone, Copy, PartialEq)]
struct FormatChoice {
    ext: &'static str,
    label: &'static str,
    hint: &'static str,
    levels: (i32, i32, i32), // min, max, default
}

const FORMATS: &[FormatChoice] = &[
    FormatChoice { ext: "tar.zst", label: "tar.zst", hint: "Fastest overall, instant listing (recommended)", levels: (1, 19, 3) },
    FormatChoice { ext: "zip", label: "zip", hint: "Opens everywhere, including Windows Explorer and Finder", levels: (1, 9, 6) },
    FormatChoice { ext: "tar.gz", label: "tar.gz", hint: "Universal on Linux and macOS", levels: (1, 9, 6) },
    FormatChoice { ext: "tar.xz", label: "tar.xz", hint: "Smallest files, slower to create", levels: (0, 9, 6) },
    FormatChoice { ext: "7z", label: "7z", hint: "Small files, opens in 7-Zip", levels: (0, 9, 6) },
    FormatChoice { ext: "tar.bz2", label: "tar.bz2", hint: "Legacy compatibility", levels: (1, 9, 9) },
    FormatChoice { ext: "tar.lz4", label: "tar.lz4", hint: "Fastest compression, larger files", levels: (1, 1, 1) },
];

struct CreateState {
    inputs: Vec<PathBuf>,
    format: usize,
    level: i32,
    output: Option<PathBuf>,
    skip_system_files: bool,
}

impl CreateState {
    fn fmt(&self) -> FormatChoice {
        FORMATS[self.format]
    }
    fn default_output(&self) -> Option<PathBuf> {
        let first = self.inputs.first()?;
        let dir = first.parent().map(Path::to_path_buf).unwrap_or_default();
        let stem = if self.inputs.len() == 1 {
            first.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "archive".into())
        } else {
            "Archive".into()
        };
        Some(dir.join(format!("{stem}.{}", self.fmt().ext)))
    }
    fn output_path(&self) -> Option<PathBuf> {
        self.output.clone().or_else(|| self.default_output())
    }
}

enum Msg {
    Opened(Result<Box<Browse>, String>),
    Extracted(Result<(archivix::ExtractStats, PathBuf), String>, f64),
    Created(Result<(archivix::CreateStats, PathBuf), String>, f64),
    Tested(Result<archivix::TestReport, String>, f64),
    Preview(usize, Result<Preview, String>),
}

struct Job {
    label: String,
    started: Instant,
}

#[derive(Clone)]
enum StatusKind {
    Ok,
    Err,
    Info,
}

struct Status {
    text: String,
    kind: StatusKind,
    reveal: Option<PathBuf>,
}

struct App {
    mode: Mode,
    browse: Option<Browse>,
    create: CreateState,
    job: Option<Job>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    status: Option<Status>,
    logo: Option<egui::TextureHandle>,
    shot: Option<(PathBuf, u32)>,
    frame: u64,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, initial: Vec<PathBuf>) -> App {
        style(&cc.egui_ctx);
        let (tx, rx) = channel();
        let mut app = App {
            mode: Mode::Home,
            browse: None,
            create: CreateState { inputs: Vec::new(), format: 0, level: FORMATS[0].levels.2, output: None, skip_system_files: true },
            job: None,
            tx,
            rx,
            status: None,
            logo: None,
            // Test hook: render, save a screenshot and quit.
            shot: std::env::var_os("ARCHIVIX_GUI_SCREENSHOT").map(|p| (PathBuf::from(p), 0)),
            frame: 0,
        };
        if let Ok(m) = std::env::var("ARCHIVIX_GUI_MODE") {
            app.mode = if m == "create" { Mode::Create } else { app.mode };
        }
        app.accept_paths(&cc.egui_ctx, initial);
        app
    }

    fn busy(&self) -> bool {
        self.job.is_some()
    }

    fn spawn(&mut self, ctx: &egui::Context, label: &str, work: impl FnOnce() -> Msg + Send + 'static) {
        archivix::progress().reset(label);
        self.job = Some(Job { label: label.to_string(), started: Instant::now() });
        self.status = None;
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let msg = work();
            let _ = tx.send(msg);
            ctx.request_repaint();
        });
    }

    /// Dropped or command-line paths: one archive opens; anything else is staged for compression.
    fn accept_paths(&mut self, ctx: &egui::Context, paths: Vec<PathBuf>) {
        if paths.is_empty() || self.busy() {
            return;
        }
        if self.mode != Mode::Create && paths.len() == 1 && paths[0].is_file() && archivix::format::detect(&paths[0]).is_ok() {
            self.open_archive(ctx, paths[0].clone());
            return;
        }
        for p in paths {
            if !self.create.inputs.contains(&p) {
                self.create.inputs.push(p);
            }
        }
        self.mode = Mode::Create;
    }

    fn open_archive(&mut self, ctx: &egui::Context, path: PathBuf) {
        self.spawn(ctx, "Reading", move || {
            let r = (|| -> anyhow::Result<Browse> {
                let (format, entries) = archivix::list(&path, None)?;
                let info = archivix::info_from_entries(&path, format, &entries)?;
                let mut b = Browse {
                    path,
                    info,
                    entries,
                    filter: String::new(),
                    view: Vec::new(),
                    sort: SortCol::Name,
                    ascending: true,
                    selected: BTreeSet::new(),
                    anchor: None,
                    preview: None,
                    preview_for: None,
                };
                // Keep archive order by default (that is how the archive is laid out).
                b.view = (0..b.entries.len()).collect();
                Ok(b)
            })();
            Msg::Opened(r.map(Box::new).map_err(|e| format!("{e:#}")))
        });
    }

    fn extract(&mut self, ctx: &egui::Context, only_selected: bool) {
        let Some(b) = &self.browse else { return };
        let start = b.path.parent().map(Path::to_path_buf).unwrap_or_default();
        let Some(dir) = rfd::FileDialog::new().set_title("Extract to…").set_directory(&start).pick_folder() else { return };
        let stem = archive_stem(&b.path);
        // Extract into a folder named after the archive unless the user picked one already named so.
        let dest = if dir.file_name().is_some_and(|n| n.to_string_lossy() == stem) { dir } else { dir.join(&stem) };
        let opts = ExtractOptions {
            dest: dest.clone(),
            patterns: if only_selected { b.selected_paths() } else { Vec::new() },
            literal: true,
            ..Default::default()
        };
        let path = b.path.clone();
        self.spawn(ctx, "Extracting", move || {
            let t = Instant::now();
            let r = archivix::extract(&path, &opts).map(|s| (s, dest)).map_err(|e| format!("{e:#}"));
            Msg::Extracted(r, t.elapsed().as_secs_f64())
        });
    }

    fn test(&mut self, ctx: &egui::Context) {
        let Some(b) = &self.browse else { return };
        let path = b.path.clone();
        self.spawn(ctx, "Testing", move || {
            let t = Instant::now();
            let r = archivix::test(&path, None).map_err(|e| format!("{e:#}"));
            Msg::Tested(r, t.elapsed().as_secs_f64())
        });
    }

    fn create(&mut self, ctx: &egui::Context) {
        let Some(output) = self.create.output_path() else { return };
        let inputs = self.create.inputs.clone();
        // Store names relative to the inputs' common parent (not absolute paths).
        let base = common_parent(&inputs);
        let rel: Vec<PathBuf> = inputs
            .iter()
            .map(|p| base.as_ref().and_then(|b| p.strip_prefix(b).ok()).map(Path::to_path_buf).unwrap_or_else(|| p.clone()))
            .collect();
        let excludes = if self.create.skip_system_files {
            [".DS_Store", "Thumbs.db", "desktop.ini", "__MACOSX"].map(String::from).to_vec()
        } else {
            Vec::new()
        };
        let opts = CreateOptions { level: Some(self.create.level), base_dir: base, excludes, ..Default::default() };
        self.spawn(ctx, "Compressing", move || {
            let t = Instant::now();
            let r = archivix::create(&output, &rel, &opts).map(|s| (s, output)).map_err(|e| format!("{e:#}"));
            Msg::Created(r, t.elapsed().as_secs_f64())
        });
    }

    fn request_preview(&mut self, ctx: &egui::Context) {
        let Some(b) = &mut self.browse else { return };
        let single = (b.selected.len() == 1).then(|| *b.selected.iter().next().unwrap());
        if single == b.preview_for {
            return;
        }
        b.preview_for = single;
        b.preview = None;
        let Some(idx) = single else { return };
        let e = &b.entries[idx];
        if e.kind != EntryKind::File || self.job.is_some() {
            return;
        }
        let (path, member, size) = (b.path.clone(), e.path.clone(), e.size);
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let r = load_preview(&path, &member, size).map_err(|e| format!("{e:#}"));
            let _ = tx.send(Msg::Preview(idx, r));
            ctx.request_repaint();
        });
    }

    fn handle_messages(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Preview(idx, r) => {
                    if let Some(b) = &mut self.browse
                        && b.preview_for == Some(idx)
                    {
                        b.preview = r.ok();
                    }
                    continue;
                }
                Msg::Opened(r) => match r {
                    Ok(mut b) => {
                        // Test hook: preselect an entry (exercises the preview pane in screenshots).
                        if let Ok(sel) = std::env::var("ARCHIVIX_GUI_SELECT")
                            && let Some(i) = b.entries.iter().position(|e| e.path == sel)
                        {
                            b.selected.insert(i);
                            b.anchor = Some(i);
                        }
                        self.browse = Some(*b);
                        self.mode = Mode::Browse;
                    }
                    Err(e) => self.fail("Could not open archive", e),
                },
                Msg::Extracted(r, secs) => match r {
                    Ok((s, dest)) => {
                        self.status = Some(Status {
                            text: format!("Extracted {} files ({}) in {secs:.2}s", s.files, human(s.bytes)),
                            kind: StatusKind::Ok,
                            reveal: Some(dest),
                        })
                    }
                    Err(e) => self.fail("Extraction failed", e),
                },
                Msg::Created(r, secs) => match r {
                    Ok((s, out)) => {
                        let pct = if s.input_bytes > 0 { s.output_bytes as f64 * 100.0 / s.input_bytes as f64 } else { 0.0 };
                        self.status = Some(Status {
                            text: format!(
                                "Created {} — {} entries, {} → {} ({pct:.1}%) in {secs:.2}s",
                                out.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                                s.entries,
                                human(s.input_bytes),
                                human(s.output_bytes)
                            ),
                            kind: StatusKind::Ok,
                            reveal: Some(out),
                        });
                    }
                    Err(e) => self.fail("Could not create archive", e),
                },
                Msg::Tested(r, secs) => match r {
                    Ok(rep) if rep.errors.is_empty() => {
                        self.status = Some(Status {
                            text: format!("Integrity OK: {} entries, {} verified in {secs:.2}s", rep.entries, human(rep.bytes)),
                            kind: StatusKind::Ok,
                            reveal: None,
                        })
                    }
                    Ok(rep) => {
                        let first = rep.errors.first().cloned().unwrap_or_default();
                        self.fail(&format!("{} problems found", rep.errors.len()), first)
                    }
                    Err(e) => self.fail("Test failed", e),
                },
            }
            self.job = None;
        }
    }

    fn fail(&mut self, what: &str, err: String) {
        let text = if err.contains("cancelled") { "Cancelled".to_string() } else { format!("{what}: {err}") };
        let kind = if err.contains("cancelled") { StatusKind::Info } else { StatusKind::Err };
        self.status = Some(Status { text, kind, reveal: None });
    }

    fn logo(&mut self, ctx: &egui::Context) -> egui::TextureId {
        self.logo
            .get_or_insert_with(|| {
                let img = egui::ColorImage::from_rgba_unmultiplied([64, 64], &icon::rgba(64));
                ctx.load_texture("logo", img, egui::TextureOptions::LINEAR)
            })
            .id()
    }
}

// ------------------------------------------------------------------ UI

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_messages();
        if self.busy() {
            ctx.request_repaint_after(Duration::from_millis(80));
        }
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        if !dropped.is_empty() {
            self.accept_paths(ctx, dropped);
        }
        self.screenshot_hook(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.frame += 1;
        self.top_bar(ui, &ctx);
        self.status_bar(ui, &ctx);
        match self.mode {
            Mode::Home => self.home(ui, &ctx),
            Mode::Browse => self.browse_view(ui, &ctx),
            Mode::Create => self.create_view(ui, &ctx),
        }
        self.drop_overlay(&ctx);
        self.request_preview(&ctx);
    }
}

impl App {
    fn top_bar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let logo = self.logo(ctx);
        egui::Panel::top("top").frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(14, 10)).fill(ui.visuals().panel_fill)).show(
            ui,
            |ui| {
                ui.horizontal(|ui| {
                    ui.add(egui::Image::new((logo, Vec2::splat(22.0))));
                    ui.label(RichText::new("archivix").strong().size(17.0));
                    ui.add_space(12.0);
                    let enabled = !self.busy();
                    if ui.add_enabled(enabled, egui::Button::new("Open Archive…")).clicked()
                        && let Some(p) = rfd::FileDialog::new().set_title("Open archive").pick_file()
                    {
                        self.open_archive(ctx, p);
                    }
                    if ui.add_enabled(enabled, egui::Button::new("New Archive")).clicked() {
                        self.mode = Mode::Create;
                    }
                    if self.browse.is_some()
                        && self.mode != Mode::Browse
                        && ui.add_enabled(enabled, egui::Button::new("Back to Archive")).clicked()
                    {
                        self.mode = Mode::Browse;
                    }
                });
            },
        );
    }

    fn status_bar(&mut self, ui: &mut egui::Ui, _ctx: &egui::Context) {
        egui::Panel::bottom("status")
            .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(14, 8)).fill(ui.visuals().panel_fill))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if let Some(job) = &self.job {
                        let snap = archivix::progress().snapshot();
                        let secs = job.started.elapsed().as_secs_f64();
                        let rate = if secs > 0.2 { format!("  ·  {}/s", human((snap.bytes as f64 / secs) as u64)) } else { String::new() };
                        let files = if snap.entries > 0 {
                            if snap.total_entries > 0 {
                                format!("  ·  {}/{} files", snap.entries, snap.total_entries)
                            } else {
                                format!("  ·  {} files", snap.entries)
                            }
                        } else {
                            String::new()
                        };
                        let pct = snap.fraction().map(|f| format!("{:.0}%  ·  ", f * 100.0)).unwrap_or_default();
                        let text = format!("{}  {pct}{}{rate}{files}", job.label, human(snap.bytes));
                        let bar = match snap.fraction() {
                            Some(f) => egui::ProgressBar::new(f as f32),
                            None => egui::ProgressBar::new(0.0).animate(true),
                        };
                        ui.add(bar.text(text).desired_width((ui.available_width() - 90.0).max(120.0)).fill(ACCENT));
                        if ui.button("Cancel").clicked() {
                            archivix::progress().cancel();
                        }
                    } else if let Some(st) = &self.status {
                        let color = match st.kind {
                            StatusKind::Ok => OK,
                            StatusKind::Err => ERR,
                            StatusKind::Info => ui.visuals().weak_text_color(),
                        };
                        ui.label(
                            RichText::new(match st.kind {
                                StatusKind::Ok => "✔",
                                StatusKind::Err => "✖",
                                StatusKind::Info => "ℹ",
                            })
                            .color(color),
                        );
                        ui.label(RichText::new(&st.text).color(color));
                        if let Some(p) = st.reveal.clone() {
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button(reveal_label()).clicked() {
                                    reveal(&p);
                                }
                            });
                        }
                    } else {
                        ui.label(RichText::new("Ready").color(ui.visuals().weak_text_color()));
                    }
                });
            });
    }

    fn home(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let logo = self.logo(ctx);
        egui::CentralPanel::default().show(ui, |ui| {
            let avail = ui.available_size();
            let card_w = 460.0f32.min(avail.x - 40.0);
            ui.add_space(((avail.y - 330.0) / 2.0).max(10.0));
            ui.vertical_centered(|ui| {
                egui::Frame::new()
                    .stroke(egui::Stroke::new(1.5, ui.visuals().widgets.noninteractive.bg_stroke.color))
                    .corner_radius(16)
                    .inner_margin(egui::Margin::symmetric(28, 30))
                    .show(ui, |ui| {
                        ui.set_width(card_w);
                        ui.vertical_centered(|ui| {
                            ui.add(egui::Image::new((logo, Vec2::splat(72.0))));
                            ui.add_space(12.0);
                            ui.label(RichText::new("Drop an archive to open it").size(20.0).strong());
                            ui.label(RichText::new("or drop files and folders to compress them").color(ui.visuals().weak_text_color()));
                            ui.add_space(18.0);
                            ui.horizontal(|ui| {
                                let w = 150.0;
                                ui.add_space((card_w - 2.0 * w - ui.spacing().item_spacing.x) / 2.0);
                                if ui
                                    .add_sized(
                                        [w, 32.0],
                                        egui::Button::new(RichText::new("Open Archive…").color(Color32::WHITE)).fill(ACCENT),
                                    )
                                    .clicked()
                                    && let Some(p) = rfd::FileDialog::new().set_title("Open archive").pick_file()
                                {
                                    self.open_archive(ctx, p);
                                }
                                if ui.add_sized([w, 32.0], egui::Button::new("New Archive")).clicked() {
                                    self.mode = Mode::Create;
                                }
                            });
                            ui.add_space(18.0);
                            ui.label(
                                RichText::new("zip · tar.gz · tar.zst · tar.xz · tar.bz2 · tar.lz4 · 7z · rar")
                                    .small()
                                    .color(ui.visuals().weak_text_color()),
                            );
                        });
                    });
            });
        });
    }

    fn browse_view(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let busy = self.busy();
        let Some(b) = &mut self.browse else {
            self.mode = Mode::Home;
            return;
        };
        let mut action: Option<&'static str> = None;

        // Summary header + toolbar.
        egui::Panel::top("archive_header")
            .frame(egui::Frame::new().fill(ui.visuals().panel_fill).inner_margin(egui::Margin { left: 14, right: 14, top: 4, bottom: 10 }))
            .show(ui, |ui| {
                let i = &b.info;
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(b.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()).size(18.0).strong(),
                    );
                    badge(ui, &i.format);
                    if i.encrypted_entries > 0 {
                        badge(ui, "encrypted");
                    }
                });
                ui.label(
                    RichText::new(format!(
                        "{} files, {} folders  ·  {} packed into {} ({:.1}%)",
                        i.files,
                        i.dirs,
                        human(i.total_size),
                        human(i.archive_size),
                        i.ratio * 100.0
                    ))
                    .color(ui.visuals().weak_text_color()),
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let r = ui.add(egui::TextEdit::singleline(&mut b.filter).hint_text("🔍  Filter by name").desired_width(260.0));
                    if r.changed() {
                        b.rebuild_view();
                    }
                    ui.label(RichText::new(format!("{} shown", b.view.len())).small().color(ui.visuals().weak_text_color()));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(!busy, egui::Button::new(RichText::new("Extract All…").color(Color32::WHITE)).fill(ACCENT))
                            .clicked()
                        {
                            action = Some("all");
                        }
                        let n = b.selected.len();
                        if ui.add_enabled(!busy && n > 0, egui::Button::new(format!("Extract Selected ({n})…"))).clicked() {
                            action = Some("selected");
                        }
                        if ui.add_enabled(!busy, egui::Button::new("Test")).on_hover_text("Verify checksums of every entry").clicked() {
                            action = Some("test");
                        }
                    });
                });
            });

        // Preview pane.
        if b.selected.len() == 1 {
            egui::Panel::right("preview").resizable(true).default_size(320.0).show(ui, |ui| {
                preview_ui(ui, b);
            });
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(ui.visuals().panel_fill).inner_margin(egui::Margin { left: 8, right: 8, top: 0, bottom: 0 }))
            .show(ui, |ui| {
                entries_table(ui, b);
            });

        match action {
            Some("all") => self.extract(ctx, false),
            Some("selected") => self.extract(ctx, true),
            Some("test") => self.test(ctx),
            _ => {}
        }
    }

    fn create_view(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let busy = self.busy();
        let mut do_create = false;
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(ui.visuals().panel_fill).inner_margin(egui::Margin::symmetric(18, 8)))
            .show(ui, |ui| {
                let c = &mut self.create;
                ui.label(RichText::new("New archive").size(18.0).strong());
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.add_enabled(!busy, egui::Button::new("Add Files…")).clicked()
                        && let Some(files) = rfd::FileDialog::new().pick_files()
                    {
                        c.inputs.extend(files.into_iter().filter(|f| !c.inputs.contains(f)).collect::<Vec<_>>());
                    }
                    if ui.add_enabled(!busy, egui::Button::new("Add Folder…")).clicked()
                        && let Some(dirs) = rfd::FileDialog::new().pick_folders()
                    {
                        c.inputs.extend(dirs.into_iter().filter(|d| !c.inputs.contains(d)).collect::<Vec<_>>());
                    }
                    if !c.inputs.is_empty() && ui.add_enabled(!busy, egui::Button::new("Clear")).clicked() {
                        c.inputs.clear();
                        c.output = None;
                    }
                });
                ui.add_space(6.0);
                egui::Frame::new()
                    .stroke(egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color))
                    .corner_radius(8)
                    .inner_margin(8)
                    .show(ui, |ui| {
                        ui.set_min_height(140.0);
                        ui.set_width(ui.available_width());
                        if c.inputs.is_empty() {
                            ui.centered_and_justified(|ui| {
                                ui.label(RichText::new("Drop files or folders here").color(ui.visuals().weak_text_color()));
                            });
                        } else {
                            let mut remove = None;
                            egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                                for (i, p) in c.inputs.iter().enumerate() {
                                    ui.horizontal(|ui| {
                                        ui.label(if p.is_dir() { "📁" } else { "📄" });
                                        let name = p
                                            .file_name()
                                            .map(|n| n.to_string_lossy().into_owned())
                                            .unwrap_or_else(|| p.display().to_string());
                                        ui.label(RichText::new(name).strong());
                                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                            if ui.add_enabled(!busy, egui::Button::new("✕").frame(false)).on_hover_text("Remove").clicked()
                                            {
                                                remove = Some(i);
                                            }
                                            let dir = p.parent().map(|d| d.display().to_string()).unwrap_or_default();
                                            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                                                ui.add(
                                                    egui::Label::new(RichText::new(dir).color(ui.visuals().weak_text_color())).truncate(),
                                                );
                                            });
                                        });
                                    })
                                    .response
                                    .on_hover_text(p.display().to_string());
                                }
                            });
                            if let Some(i) = remove {
                                c.inputs.remove(i);
                            }
                        }
                    });
                ui.add_space(12.0);
                egui::Grid::new("create_opts").num_columns(2).spacing([16.0, 10.0]).show(ui, |ui| {
                    ui.label("Format");
                    ui.horizontal(|ui| {
                        let before = c.format;
                        egui::ComboBox::from_id_salt("format").width(120.0).selected_text(c.fmt().label).show_ui(ui, |ui| {
                            for (i, f) in FORMATS.iter().enumerate() {
                                ui.selectable_value(&mut c.format, i, f.label).on_hover_text(f.hint);
                            }
                        });
                        if before != c.format {
                            c.level = c.fmt().levels.2;
                            // Keep a user-chosen name but switch its extension.
                            if let Some(out) = &c.output {
                                let name = out.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                                let stem = strip_archive_ext(&name);
                                c.output = Some(out.with_file_name(format!("{stem}.{}", c.fmt().ext)));
                            }
                        }
                        ui.label(RichText::new(c.fmt().hint).color(ui.visuals().weak_text_color()));
                    });
                    ui.end_row();

                    let (lo, hi, _) = c.fmt().levels;
                    ui.label("Level");
                    ui.horizontal(|ui| {
                        ui.add_enabled(lo != hi, egui::Slider::new(&mut c.level, lo..=hi));
                        ui.label(RichText::new("higher = smaller, slower").color(ui.visuals().weak_text_color()));
                    });
                    ui.end_row();

                    ui.label("Save as");
                    ui.horizontal(|ui| {
                        let shown = c.output_path().map(|p| p.display().to_string()).unwrap_or_else(|| "-".into());
                        let change = ui.add_enabled(!busy && !c.inputs.is_empty(), egui::Button::new("Change…"));
                        ui.add(egui::Label::new(RichText::new(&shown).monospace()).truncate()).on_hover_text(&shown);
                        if change.clicked() {
                            let def = c.output_path();
                            let mut d = rfd::FileDialog::new().set_title("Save archive as");
                            if let Some(def) = &def {
                                if let Some(dir) = def.parent() {
                                    d = d.set_directory(dir);
                                }
                                if let Some(n) = def.file_name() {
                                    d = d.set_file_name(n.to_string_lossy());
                                }
                            }
                            if let Some(p) = d.save_file() {
                                c.output = Some(p);
                            }
                        }
                    });
                    ui.end_row();

                    ui.label("");
                    ui.checkbox(&mut c.skip_system_files, "Skip system files (.DS_Store, Thumbs.db, __MACOSX)");
                    ui.end_row();
                });
                ui.add_space(14.0);
                let ready = !busy && !c.inputs.is_empty();
                if ui
                    .add_enabled(
                        ready,
                        egui::Button::new(RichText::new("Create Archive").color(Color32::WHITE).strong())
                            .fill(ACCENT)
                            .min_size(Vec2::new(160.0, 34.0)),
                    )
                    .clicked()
                {
                    do_create = true;
                }
            });
        if do_create {
            self.create(ctx);
        }
    }

    fn drop_overlay(&self, ctx: &egui::Context) {
        let hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());
        if !hovering {
            return;
        }
        let text = if self.mode == Mode::Create { "Drop to add to the archive" } else { "Drop to open or compress" };
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("drop")));
        let rect = ctx.content_rect();
        painter.rect_filled(rect, 0.0, Color32::from_black_alpha(150));
        painter.rect_stroke(rect.shrink(16.0), 14.0, egui::Stroke::new(2.0, ACCENT), egui::StrokeKind::Inside);
        painter.text(rect.center(), egui::Align2::CENTER_CENTER, text, egui::FontId::proportional(24.0), Color32::WHITE);
    }

    /// `ARCHIVIX_GUI_SCREENSHOT=out.png archivix-gui [path]`: once idle, save the window and quit.
    fn screenshot_hook(&mut self, ctx: &egui::Context) {
        if self.shot.is_none() {
            return;
        }
        // Test hook: start an action automatically.
        if std::env::var("ARCHIVIX_GUI_AUTORUN").as_deref() == Ok("create")
            && self.mode == Mode::Create
            && self.job.is_none()
            && self.status.is_none()
        {
            self.create(ctx);
        }
        let Some((path, frames)) = &mut self.shot else { return };
        let shot = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(img) = shot {
            let _ = save_color_image(path, &img);
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        // Test hook: capture while a job runs.
        let shoot_busy = std::env::var_os("ARCHIVIX_GUI_SHOT_BUSY").is_some();
        if shoot_busy && self.job.as_ref().is_some_and(|j| j.started.elapsed() > Duration::from_millis(600)) && *frames % 30 == 0 {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
        if shoot_busy {
            *frames += 1;
            ctx.request_repaint();
            return;
        }
        let preview_pending = self.browse.as_ref().is_some_and(|b| {
            b.selected.len() == 1 && b.preview.is_none() && b.entries[*b.selected.iter().next().unwrap()].kind == EntryKind::File
        });
        if self.job.is_none() && !preview_pending {
            *frames += 1;
            // Re-request until it arrives: the first request can be dropped if
            // the window surface is not ready yet.
            if *frames >= 15 && *frames % 30 == 15 {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            }
        }
        ctx.request_repaint();
    }
}

fn entries_table(ui: &mut egui::Ui, b: &mut Browse) {
    let row_h = 22.0;
    let weak = ui.visuals().weak_text_color();
    let mut clicked: Option<(usize, egui::Modifiers)> = None;
    let mut sort_click: Option<SortCol> = None;
    let header = |ui: &mut egui::Ui, label: &str, col: SortCol, b: &Browse, sort_click: &mut Option<SortCol>| {
        let r = ui.add(egui::Button::new(RichText::new(label).strong()).frame(false));
        if r.clicked() {
            *sort_click = Some(col);
        }
        if b.sort == col {
            // Sort direction as a small painted triangle (no font dependency).
            let (rect, _) = ui.allocate_exact_size(Vec2::new(10.0, 10.0), egui::Sense::hover());
            let c = rect.center();
            let pts = if b.ascending {
                vec![c + Vec2::new(-4.0, 2.5), c + Vec2::new(4.0, 2.5), c + Vec2::new(0.0, -3.0)]
            } else {
                vec![c + Vec2::new(-4.0, -2.5), c + Vec2::new(4.0, -2.5), c + Vec2::new(0.0, 3.0)]
            };
            ui.painter().add(egui::Shape::convex_polygon(pts, ui.visuals().text_color(), egui::Stroke::NONE));
        }
    };
    TableBuilder::new(ui)
        .striped(true)
        .sense(egui::Sense::click())
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
        .column(Column::remainder().at_least(220.0).clip(true))
        .column(Column::exact(90.0))
        .column(Column::exact(90.0))
        .column(Column::exact(130.0))
        .header(24.0, |mut h| {
            h.col(|ui| header(ui, "Name", SortCol::Name, b, &mut sort_click));
            h.col(|ui| header(ui, "Size", SortCol::Size, b, &mut sort_click));
            h.col(|ui| header(ui, "Packed", SortCol::Packed, b, &mut sort_click));
            h.col(|ui| header(ui, "Modified", SortCol::Modified, b, &mut sort_click));
        })
        .body(|body| {
            body.rows(row_h, b.view.len(), |mut row| {
                let idx = b.view[row.index()];
                let e = &b.entries[idx];
                row.set_selected(b.selected.contains(&idx));
                row.col(|ui| {
                    let glyph = match e.kind {
                        EntryKind::Dir => "📁",
                        EntryKind::Symlink | EntryKind::Hardlink => "🔗",
                        _ => "📄",
                    };
                    ui.label(glyph);
                    if e.kind == EntryKind::Dir {
                        ui.label(RichText::new(format!("{}/", e.path)).strong()).on_hover_text(&e.path);
                    } else {
                        ui.label(&e.path).on_hover_text(&e.path);
                    }
                });
                row.col(|ui| {
                    if e.kind != EntryKind::Dir {
                        ui.label(human(e.size));
                    }
                });
                row.col(|ui| {
                    if let Some(c) = e.compressed_size.filter(|_| e.kind != EntryKind::Dir) {
                        ui.label(RichText::new(human(c)).color(weak));
                    }
                });
                row.col(|ui| {
                    ui.label(RichText::new(format_time(e.mtime)).color(weak));
                });
                if row.response().clicked() {
                    clicked = Some((row.index(), row.response().ctx.input(|i| i.modifiers)));
                }
            });
        });
    if let Some(col) = sort_click {
        if b.sort == col {
            b.ascending = !b.ascending;
        } else {
            b.sort = col;
            b.ascending = col == SortCol::Name;
        }
        b.rebuild_view();
    }
    if let Some((pos, m)) = clicked {
        let idx = b.view[pos];
        if m.shift
            && let Some(a) = b.anchor.and_then(|a| b.view.iter().position(|&v| v == a))
        {
            let (lo, hi) = if a <= pos { (a, pos) } else { (pos, a) };
            if !m.command {
                b.selected.clear();
            }
            b.selected.extend(b.view[lo..=hi].iter().copied());
        } else if m.command {
            if !b.selected.remove(&idx) {
                b.selected.insert(idx);
            }
            b.anchor = Some(idx);
        } else {
            b.selected.clear();
            b.selected.insert(idx);
            b.anchor = Some(idx);
        }
    }
}

fn preview_ui(ui: &mut egui::Ui, b: &Browse) {
    let Some(&idx) = b.selected.iter().next() else { return };
    let e = &b.entries[idx];
    ui.add_space(6.0);
    ui.label(RichText::new(e.path.rsplit('/').next().unwrap_or(&e.path)).strong().size(15.0));
    ui.label(RichText::new(&e.path).small().color(ui.visuals().weak_text_color()));
    ui.add_space(4.0);
    egui::Grid::new("meta").num_columns(2).spacing([10.0, 3.0]).show(ui, |ui| {
        let row = |ui: &mut egui::Ui, k: &str, v: String| {
            ui.label(RichText::new(k).color(ui.visuals().weak_text_color()));
            ui.label(v);
            ui.end_row();
        };
        row(ui, "Size", format!("{} ({} bytes)", human(e.size), e.size));
        if let Some(c) = e.compressed_size {
            row(ui, "Packed", human(c));
        }
        if let Some(m) = &e.method {
            row(ui, "Method", m.clone());
        }
        row(ui, "Modified", format_time(e.mtime).trim().to_string());
        if let Some(m) = e.mode {
            row(ui, "Mode", archivix::util::mode_string(e.kind, Some(m)));
        }
        if let Some(c) = e.crc32 {
            row(ui, "CRC-32", format!("{c:08x}"));
        }
        if let Some(t) = &e.link_target {
            row(ui, "Target", t.clone());
        }
    });
    ui.separator();
    match (&b.preview, e.kind) {
        (_, EntryKind::Dir) => {
            ui.label(RichText::new("Folder").color(ui.visuals().weak_text_color()));
        }
        (Some(p), _) if p.path == e.path => {
            egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
                let body = p.text.as_deref().unwrap_or(&p.hex);
                ui.add(egui::Label::new(RichText::new(body).monospace().size(11.5)).extend());
                if p.truncated {
                    ui.label(
                        RichText::new(format!("… showing the first {} of {}", human(PREVIEW_MAX), human(p.size)))
                            .small()
                            .color(ui.visuals().weak_text_color()),
                    );
                }
            });
        }
        _ => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Loading preview…");
            });
        }
    }
}

fn load_preview(archive: &Path, member: &str, size: u64) -> anyhow::Result<Preview> {
    let mut buf = Vec::new();
    let want = member.to_string();
    let filter = |e: &Entry| e.path == want;
    let mut each = |_: &Entry, r: &mut dyn Read| -> anyhow::Result<bool> {
        r.take(PREVIEW_MAX).read_to_end(&mut buf)?;
        Ok(false)
    };
    archivix::visit(archive, None, &mut archivix::Visitor { filter: &filter, each: &mut each })?;
    let binary = buf[..buf.len().min(8192)].contains(&0);
    let text = if binary {
        None
    } else {
        match std::str::from_utf8(&buf) {
            Ok(s) => Some(s.to_string()),
            // Cut mid-character at the preview limit: keep the valid prefix.
            Err(e) if e.error_len().is_none() => Some(String::from_utf8_lossy(&buf[..e.valid_up_to()]).into_owned()),
            Err(_) => None,
        }
    };
    let hex = if text.is_none() { hexdump(&buf[..buf.len().min(4096)]) } else { String::new() };
    Ok(Preview { path: member.to_string(), size, text, hex, truncated: size > buf.len() as u64 })
}

fn hexdump(data: &[u8]) -> String {
    let mut out = String::new();
    for (i, row) in data.chunks(16).enumerate() {
        out.push_str(&format!("{:08x}  ", i * 16));
        for b in row {
            out.push_str(&format!("{b:02x} "));
        }
        for _ in row.len()..16 {
            out.push_str("   ");
        }
        out.push(' ');
        out.extend(row.iter().map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '.' }));
        out.push('\n');
    }
    out
}

fn badge(ui: &mut egui::Ui, text: &str) {
    egui::Frame::new().fill(ACCENT.linear_multiply(0.18)).corner_radius(6).inner_margin(egui::Margin::symmetric(7, 2)).show(ui, |ui| {
        ui.label(RichText::new(text).small().strong().color(ACCENT));
    });
}

/// egui's bundled fonts cover Latin only. Add the OS's own fonts as low-priority
/// fallbacks so CJK file names, arrows and symbols render instead of boxes.
fn install_fallback_fonts(ctx: &egui::Context) {
    let groups: &[&[&str]] = &[
        // Broad Unicode / CJK
        &[
            "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
            "/System/Library/Fonts/Hiragino Sans GB.ttc",
            "C:\\Windows\\Fonts\\msyh.ttc",
            "C:\\Windows\\Fonts\\YuGothM.ttc",
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
        ],
        // Korean (when the first group lacks Hangul)
        &["C:\\Windows\\Fonts\\malgun.ttf", "/usr/share/fonts/truetype/nanum/NanumGothic.ttf"],
        // Symbols
        &[
            "/System/Library/Fonts/Apple Symbols.ttf",
            "C:\\Windows\\Fonts\\seguisym.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/TTF/DejaVuSans.ttf",
        ],
    ];
    for (gi, group) in groups.iter().enumerate() {
        let Some(bytes) = group.iter().find_map(|p| std::fs::read(p).ok().filter(|b| b.len() > 1024)) else { continue };
        let families = [egui::FontFamily::Proportional, egui::FontFamily::Monospace]
            .into_iter()
            .map(|family| egui::epaint::text::InsertFontFamily { family, priority: egui::epaint::text::FontPriority::Lowest })
            .collect();
        ctx.add_font(egui::epaint::text::FontInsert::new(&format!("system-fallback-{gi}"), egui::FontData::from_owned(bytes), families));
    }
}

fn style(ctx: &egui::Context) {
    install_fallback_fonts(ctx);
    ctx.all_styles_mut(|s| {
        use egui::{FontId, TextStyle};
        s.text_styles.insert(TextStyle::Body, FontId::proportional(14.0));
        s.text_styles.insert(TextStyle::Button, FontId::proportional(14.0));
        s.text_styles.insert(TextStyle::Small, FontId::proportional(11.5));
        s.text_styles.insert(TextStyle::Monospace, FontId::monospace(12.5));
        s.text_styles.insert(TextStyle::Heading, FontId::proportional(20.0));
        s.spacing.item_spacing = Vec2::new(8.0, 6.0);
        s.spacing.button_padding = Vec2::new(12.0, 5.0);
        s.spacing.interact_size.y = 26.0;
        s.visuals.selection.bg_fill = ACCENT.linear_multiply(0.55);
        s.visuals.hyperlink_color = ACCENT;
        for w in
            [&mut s.visuals.widgets.inactive, &mut s.visuals.widgets.hovered, &mut s.visuals.widgets.active, &mut s.visuals.widgets.open]
        {
            w.corner_radius = 7.into();
        }
    });
}

fn save_color_image(path: &Path, img: &egui::ColorImage) -> anyhow::Result<()> {
    let [w, h] = img.size;
    let mut rgba = Vec::with_capacity(w * h * 4);
    for p in &img.pixels {
        rgba.extend_from_slice(&p.to_srgba_unmultiplied());
    }
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut enc = png::Encoder::new(file, w as u32, h as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(&rgba)?;
    Ok(())
}

fn archive_stem(p: &Path) -> String {
    strip_archive_ext(&p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "archive".into()))
}

fn strip_archive_ext(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    for ext in [
        ".tar.gz",
        ".tar.zst",
        ".tar.xz",
        ".tar.bz2",
        ".tar.lz4",
        ".tar.br",
        ".tar.lzma",
        ".tgz",
        ".tbz2",
        ".txz",
        ".tzst",
        ".tar",
        ".zip",
        ".7z",
        ".rar",
        ".gz",
        ".zst",
        ".xz",
        ".bz2",
        ".lz4",
    ] {
        if lower.ends_with(ext) && lower.len() > ext.len() {
            return name[..name.len() - ext.len()].to_string();
        }
    }
    name.to_string()
}

fn common_parent(paths: &[PathBuf]) -> Option<PathBuf> {
    let mut parents = paths.iter().map(|p| p.parent().map(Path::to_path_buf));
    let mut common = parents.next()??;
    for p in parents {
        let p = p?;
        while !p.starts_with(&common) {
            common = common.parent()?.to_path_buf();
        }
    }
    Some(common)
}

fn reveal_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "Show in Finder"
    } else if cfg!(windows) {
        "Show in Explorer"
    } else {
        "Open Folder"
    }
}

fn reveal(p: &Path) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg("-R").arg(p).spawn();
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer").arg(format!("/select,{}", p.display())).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(if p.is_dir() { p } else { p.parent().unwrap_or(p) }).spawn();
}
