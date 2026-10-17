//! Native desktop UI (egui) modelled on the provided mockup:
//! frameless dark window, configuration card on the left, drop zone + queue on
//! the right, and a collapsible terminal log along the bottom.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;

use eframe::egui::{
    self, pos2, vec2, Align, Align2, Color32, CornerRadius, FontId, Frame, Layout, Margin, Painter,
    Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Ui,
};

use crate::converter::{self, Config, Event, Level};

// ---------------------------------------------------------------------------
// Palette
// ---------------------------------------------------------------------------

const BG: Color32 = Color32::from_rgb(0x0F, 0x1A, 0x20);
const PANEL: Color32 = Color32::from_rgb(0x15, 0x24, 0x2B);
const PANEL_DEEP: Color32 = Color32::from_rgb(0x0C, 0x16, 0x1B);
const EDGE: Color32 = Color32::from_rgb(0x25, 0x39, 0x42);
const CHIP: Color32 = Color32::from_rgb(0x22, 0x34, 0x3C);
const ACCENT: Color32 = Color32::from_rgb(0x4D, 0xF0, 0xD4);
const ACCENT_FILL: Color32 = Color32::from_rgb(0x14, 0x3B, 0x3C);
const ACCENT_ROW: Color32 = Color32::from_rgb(0x15, 0x3A, 0x3B);
const ON_ACCENT: Color32 = Color32::from_rgb(0x06, 0x2B, 0x2B);
const TEXT: Color32 = Color32::from_rgb(0xE4, 0xED, 0xF0);
const MUTED: Color32 = Color32::from_rgb(0x8C, 0xA1, 0xAA);
const GREEN: Color32 = Color32::from_rgb(0x4D, 0xE0, 0xA0);
const YELLOW: Color32 = Color32::from_rgb(0xE8, 0xC4, 0x68);
const RED: Color32 = Color32::from_rgb(0xF2, 0x6D, 0x7D);

const MAX_LOG_LINES: usize = 2000;

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

const APP_NAME: &str = "WebP Forge";
const APP_CREDIT: &str = "by Araz Code";
const APP_TITLE: &str = "WebP Forge by Araz Code";
const DEVELOPER: &str = "Developed and designed by Araz Code";
const COMPANY: &str = "Araz Code is a brand of Novin Hushe Azerakhsh-e Araz LLC.";
const LINKS: [(&str, &str); 2] = [
    ("arazcode.com", "https://arazcode.com"),
    ("nhazerakhsh.ir", "https://nhazerakhsh.ir"),
];
const EMAILS: [(&str, &str); 2] = [
    ("info@arazcode.com", "mailto:info@arazcode.com"),
    ("info@nhazerakhsh.ir", "mailto:info@nhazerakhsh.ir"),
];

const WIN_BUTTON_SIZE: egui::Vec2 = egui::Vec2::new(36.0, 30.0);
const WIN_BUTTON_GAP: f32 = 4.0;
const TAB_WIDTH: f32 = 84.0;
const TAB_HEIGHT: f32 = 28.0;

// ---------------------------------------------------------------------------
// Brand assets (from ./logos)
// ---------------------------------------------------------------------------

const ICON_PNG: &[u8] = include_bytes!("../logos/webp-forge-icon-64.png");
const LOCKUP_PNG: &[u8] = include_bytes!("../logos/webp-forge-lockup-dark.png");
const WINDOW_ICON_PNG: &[u8] = include_bytes!("../logos/webp-forge-icon-256.png");

/// Decodes an embedded PNG into unmultiplied RGBA.
fn decode_png(bytes: &[u8]) -> (Vec<u8>, u32, u32) {
    let image = image::load_from_memory(bytes).expect("embedded logo is not a valid PNG").to_rgba8();
    let (width, height) = image.dimensions();
    (image.into_raw(), width, height)
}

/// The OS/window-manager icon (taskbar, alt-tab).
fn window_icon() -> egui::IconData {
    let (rgba, width, height) = decode_png(WINDOW_ICON_PNG);
    egui::IconData { rgba, width, height }
}

/// Uploads an embedded PNG as a GPU texture.
fn texture(ctx: &egui::Context, name: &str, bytes: &[u8]) -> egui::TextureHandle {
    let (rgba, width, height) = decode_png(bytes);
    let image = egui::ColorImage::from_rgba_unmultiplied([width as usize, height as usize], &rgba);
    ctx.load_texture(name, image, egui::TextureOptions::LINEAR)
}

/// Draws a texture scaled to fit `rect` (keeping its aspect ratio), centered.
fn paint_image(p: &Painter, texture: &egui::TextureHandle, rect: Rect) {
    let size = texture.size_vec2();
    let scale = (rect.width() / size.x).min(rect.height() / size.y);
    let target = Rect::from_center_size(rect.center(), size * scale);
    p.image(texture.id(), target, UV_RECT, Color32::WHITE);
}

const UV_RECT: Rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

enum Status {
    Waiting,
    Converting { width: u64, quality: u8, fraction: f32 },
    Done { size: u64, quality: u8, width: u64 },
    Failed(String),
}

struct Item {
    path: PathBuf,
    tag: &'static str,
    bytes: u64,
    status: Status,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tab {
    Convert,
    Info,
}

struct App {
    tab: Tab,
    icon: egui::TextureHandle,
    lockup: egui::TextureHandle,
    items: Vec<Item>,
    output_dir: String,
    max_kb: u64,
    jobs: usize,
    log: Vec<(Level, String)>,
    log_open: bool,
    running: bool,
    rx: Option<Receiver<Event>>,
    cancel: Arc<AtomicBool>,
}

impl App {
    fn new(ctx: &egui::Context) -> Self {
        let cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        Self {
            tab: Tab::Convert,
            icon: texture(ctx, "app-icon", ICON_PNG),
            lockup: texture(ctx, "app-lockup", LOCKUP_PNG),
            items: Vec::new(),
            output_dir: "./output".into(),
            max_kb: 300,
            jobs: 4.min(cpus).max(1),
            log: Vec::new(),
            log_open: true,
            running: false,
            rx: None,
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    fn push_log(&mut self, level: Level, text: impl Into<String>) {
        self.log.push((level, text.into()));
        if self.log.len() > MAX_LOG_LINES {
            self.log.drain(..self.log.len() - MAX_LOG_LINES);
        }
    }

    // ----- files ----------------------------------------------------------

    fn add_paths(&mut self, paths: Vec<PathBuf>) {
        if self.running {
            return;
        }
        let mut candidates = Vec::new();
        for path in paths {
            if path.is_dir() {
                if let Ok(entries) = std::fs::read_dir(&path) {
                    let mut inside: Vec<PathBuf> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
                    inside.sort();
                    candidates.extend(inside.into_iter().filter(|p| p.is_file()));
                }
            } else {
                candidates.push(path);
            }
        }
        for path in candidates {
            if !converter::is_supported(&path) {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                self.push_log(Level::Warn, format!("Skipped unsupported file: {name}"));
                continue;
            }
            if self.items.iter().any(|i| i.path == path) {
                continue;
            }
            self.items.push(Item {
                tag: format_tag(&path),
                bytes: std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
                path,
                status: Status::Waiting,
            });
        }
    }

    fn browse_files(&mut self) {
        // rfd turns each extension into a `*.ext` glob and file choosers match
        // those globs case-sensitively on Linux, which would hide `PHOTO.JPG`.
        // `filter_extensions` registers the upper-case spellings too.
        let patterns = converter::filter_extensions();
        let patterns: Vec<&str> = patterns.iter().map(String::as_str).collect();
        if let Some(files) = rfd::FileDialog::new()
            .set_title("Choose images")
            .add_filter("Images", &patterns)
            .pick_files()
        {
            self.add_paths(files);
        }
    }

    // ----- conversion -----------------------------------------------------

    fn start(&mut self, ctx: &egui::Context) {
        if self.running || self.items.is_empty() {
            return;
        }
        for item in &mut self.items {
            item.status = Status::Waiting;
        }
        let cfg = Config {
            max_kb: self.max_kb.max(1),
            jobs: self.jobs.max(1),
            output_dir: PathBuf::from(self.output_dir.trim()),
        };
        let files: Vec<PathBuf> = self.items.iter().map(|i| i.path.clone()).collect();
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = cancel.clone();
        self.rx = Some(rx);
        self.running = true;
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let emit = move |event: Event| {
                let _ = tx.send(event);
                ctx.request_repaint();
            };
            converter::run(&files, &cfg, &emit, &cancel);
        });
    }

    fn pump_events(&mut self) {
        let Some(rx) = self.rx.take() else { return };
        let mut finished = false;
        while let Ok(event) = rx.try_recv() {
            match event {
                Event::Log { level, text } => {
                    if level != Level::Try {
                        self.push_log(level, text);
                    }
                }
                Event::Progress { index, width, quality, fraction } => {
                    if let Some(item) = self.items.get_mut(index) {
                        item.status = Status::Converting { width, quality, fraction };
                    }
                }
                Event::Done { index, size, quality, width, .. } => {
                    if let Some(item) = self.items.get_mut(index) {
                        item.status = Status::Done { size, quality, width };
                    }
                }
                Event::Failed { index, reason } => {
                    if let Some(item) = self.items.get_mut(index) {
                        item.status = Status::Failed(reason);
                    }
                }
                Event::Finished { converted, failed } => {
                    let level = if failed == 0 { Level::Ok } else { Level::Warn };
                    self.push_log(level, format!("Finished: {converted} converted, {failed} failed"));
                    finished = true;
                }
            }
        }
        if finished {
            self.running = false;
        } else {
            self.rx = Some(rx);
        }
    }

    fn overall_progress(&self) -> f32 {
        if self.items.is_empty() {
            return 0.0;
        }
        let sum: f32 = self
            .items
            .iter()
            .map(|i| match &i.status {
                Status::Waiting => 0.0,
                Status::Converting { fraction, .. } => *fraction,
                Status::Done { .. } | Status::Failed(_) => 1.0,
            })
            .sum();
        sum / self.items.len() as f32
    }
}

fn format_tag(path: &Path) -> &'static str {
    match path.extension().map(|e| e.to_string_lossy().to_lowercase()).as_deref() {
        Some("heic" | "heif") => "HEIF",
        Some("jpg" | "jpeg") => "JPEG",
        Some("png") => "PNG",
        Some("tif" | "tiff") => "TIFF",
        Some("webp") => "WEBP",
        _ => "FILE",
    }
}

fn human_size(bytes: u64) -> String {
    let mb = bytes as f64 / 1_048_576.0;
    if mb >= 1.0 {
        format!("{mb:.0} MB")
    } else {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    }
}

fn clip(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        let head: String = text.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{head}…")
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub fn run() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(APP_TITLE)
            .with_inner_size([1000.0, 640.0])
            .with_min_inner_size([820.0, 540.0])
            .with_decorations(false)
            .with_transparent(true)
            .with_drag_and_drop(true)
            .with_icon(window_icon()),
        ..Default::default()
    };
    eframe::run_native(
        APP_TITLE,
        options,
        Box::new(|cc| {
            apply_theme(&cc.egui_ctx);
            Ok(Box::new(App::new(&cc.egui_ctx)))
        }),
    )
}

fn apply_theme(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = PANEL;
    v.extreme_bg_color = PANEL_DEEP;
    v.faint_bg_color = PANEL;
    v.selection.bg_fill = ACCENT_FILL;
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.hyperlink_color = ACCENT;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, EDGE);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    for w in [
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.noninteractive,
    ] {
        w.corner_radius = CornerRadius::same(7);
    }
    v.widgets.inactive.bg_fill = CHIP;
    v.widgets.inactive.weak_bg_fill = CHIP;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, EDGE);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x2A, 0x41, 0x4A);
    ctx.set_visuals(v);
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

impl eframe::App for App {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.pump_events();

        let dropped: Vec<PathBuf> =
            ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        if !dropped.is_empty() {
            self.add_paths(dropped);
        }

        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        let window = ui.max_rect();
        let radius = if maximized { 0 } else { 12 };
        ui.painter().rect(window, CornerRadius::same(radius), BG, Stroke::new(1.0, EDGE), StrokeKind::Inside);
        if !maximized {
            resize_handles(&ctx, window);
        }

        egui::Panel::top("title_bar")
            .exact_size(64.0)
            .frame(Frame::NONE.inner_margin(Margin::symmetric(20, 10)))
            .show(ui, |ui| title_bar(ui, maximized, &mut self.tab, &self.icon));

        let convert = matches!(self.tab, Tab::Convert);

        if convert {
            let log_height = if self.log_open { 176.0 } else { 46.0 };
            egui::Panel::bottom("terminal")
                .exact_size(log_height)
                .frame(Frame::NONE.inner_margin(Margin { left: 14, right: 14, top: 0, bottom: 14 }))
                .show(ui, |ui| self.terminal(ui));

            let left_width = (ui.available_width() * 0.31).clamp(250.0, 340.0);
            egui::Panel::left("config")
                .exact_size(left_width)
                .resizable(false)
                .frame(Frame::NONE.inner_margin(Margin { left: 14, right: 7, top: 0, bottom: 10 }))
                .show(ui, |ui| self.config_card(ui));
        }

        egui::CentralPanel::no_frame().show(ui, |ui| {
            let margin = if convert {
                Margin { left: 7, right: 14, top: 0, bottom: 10 }
            } else {
                Margin { left: 14, right: 14, top: 0, bottom: 14 }
            };
            Frame::NONE.inner_margin(margin).show(ui, |ui| {
                if convert {
                    self.queue_card(ui);
                } else {
                    self.info_card(ui);
                }
            });
        });
    }
}

// ---------------------------------------------------------------------------
// Window chrome
// ---------------------------------------------------------------------------

fn resize_handles(ctx: &egui::Context, rect: Rect) {
    use egui::viewport::ResizeDirection as D;
    use egui::CursorIcon as C;
    let Some(p) = ctx.input(|i| i.pointer.hover_pos()) else { return };
    let m = 6.0;
    let (l, r, t, b) = (
        p.x - rect.left() < m,
        rect.right() - p.x < m,
        p.y - rect.top() < m,
        rect.bottom() - p.y < m,
    );
    let (dir, cursor) = match (l, r, t, b) {
        (true, _, true, _) => (D::NorthWest, C::ResizeNwSe),
        (_, true, true, _) => (D::NorthEast, C::ResizeNeSw),
        (true, _, _, true) => (D::SouthWest, C::ResizeNeSw),
        (_, true, _, true) => (D::SouthEast, C::ResizeNwSe),
        (true, ..) => (D::West, C::ResizeHorizontal),
        (_, true, ..) => (D::East, C::ResizeHorizontal),
        (_, _, true, _) => (D::North, C::ResizeVertical),
        (_, _, _, true) => (D::South, C::ResizeVertical),
        _ => return,
    };
    ctx.set_cursor_icon(cursor);
    if ctx.input(|i| i.pointer.primary_pressed()) {
        ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(dir));
    }
}

fn title_bar(ui: &mut Ui, maximized: bool, tab: &mut Tab, icon: &egui::TextureHandle) {
    let ctx = ui.ctx().clone();
    let bar = ui.max_rect();

    let drag = ui.interact(bar, ui.scope_id().with("drag"), Sense::click_and_drag());
    if drag.drag_started_by(egui::PointerButton::Primary) {
        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    if drag.double_clicked() {
        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
    }

    let painter = ui.painter();
    // The PNG mark has transparent padding baked in, so it gets the full bar height.
    let logo = Rect::from_min_size(pos2(bar.left(), bar.center().y - 22.0), vec2(44.0, 44.0));
    paint_image(painter, icon, logo);

    let mut title = egui::text::LayoutJob::default();
    title.append(APP_NAME, 0.0, egui::TextFormat::simple(FontId::proportional(17.0), TEXT));
    title.append("  ", 0.0, egui::TextFormat::simple(FontId::proportional(17.0), TEXT));
    title.append(APP_CREDIT, 0.0, egui::TextFormat::simple(FontId::proportional(12.5), ACCENT));
    let title = painter.layout_job(title);
    painter.galley(
        pos2(logo.right() + 14.0, bar.center().y - 8.0 - title.size().y / 2.0),
        title,
        TEXT,
    );
    painter.text(
        pos2(logo.right() + 14.0, bar.center().y + 11.0),
        Align2::LEFT_CENTER,
        env!("CARGO_PKG_VERSION"),
        FontId::proportional(12.0),
        MUTED,
    );

    // Window buttons (right to left).
    let mut x = bar.right();
    for kind in [WinButton::Close, WinButton::Maximize, WinButton::Minimize] {
        let rect = Rect::from_min_size(pos2(x - WIN_BUTTON_SIZE.x, bar.center().y - WIN_BUTTON_SIZE.y / 2.0), WIN_BUTTON_SIZE);
        x -= WIN_BUTTON_SIZE.x + WIN_BUTTON_GAP;
        let resp = ui.interact(rect, ui.scope_id().with(kind as u8), Sense::click());
        win_button(ui.painter(), rect, kind, resp.hovered(), maximized);
        if resp.clicked() {
            match kind {
                WinButton::Close => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                WinButton::Maximize => ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized)),
                WinButton::Minimize => ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
            }
        }
    }

    for (target, rect) in tab_rects(bar) {
        let label = match target {
            Tab::Convert => "CONVERT",
            Tab::Info => "INFO",
        };
        let active = *tab == target;
        let resp = ui.interact(rect, ui.scope_id().with(("tab", label)), Sense::click());
        let p = ui.painter();
        let fill = if active { ACCENT } else if resp.hovered() { Color32::from_rgb(0x2A, 0x41, 0x4A) } else { CHIP };
        let edge = if active || resp.hovered() { ACCENT } else { EDGE };
        p.rect(rect, CornerRadius::same(9), fill, Stroke::new(1.0, edge), StrokeKind::Inside);
        let galley = p.layout_no_wrap(
            label.to_owned(),
            FontId::proportional(12.5),
            if active { ON_ACCENT } else { MUTED },
        );
        p.galley(rect.center() - galley.size() / 2.0, galley, TEXT);
        if resp.clicked() {
            *tab = target;
        }
    }
}

/// Rects of the two tabs, in left-to-right order, for a given title-bar rect.
/// They sit to the left of the window buttons.
fn tab_rects(bar: Rect) -> [(Tab, Rect); 2] {
    let buttons = 3.0 * WIN_BUTTON_SIZE.x + 2.0 * WIN_BUTTON_GAP;
    let right = bar.right() - buttons - 12.0;
    let size = vec2(TAB_WIDTH, TAB_HEIGHT);
    let top = bar.center().y - TAB_HEIGHT / 2.0;
    let info = Rect::from_min_size(pos2(right - TAB_WIDTH, top), size);
    let convert = Rect::from_min_size(pos2(info.left() - 6.0 - TAB_WIDTH, top), size);
    [(Tab::Convert, convert), (Tab::Info, info)]
}

#[derive(Clone, Copy)]
enum WinButton {
    Close,
    Maximize,
    Minimize,
}

fn win_button(p: &Painter, rect: Rect, kind: WinButton, hovered: bool, maximized: bool) {
    if hovered {
        let fill = if matches!(kind, WinButton::Close) { Color32::from_rgb(0xC4, 0x3B, 0x4B) } else { CHIP };
        p.rect_filled(rect, CornerRadius::same(7), fill);
    }
    let c = rect.center();
    let s = Stroke::new(1.4, if hovered { Color32::WHITE } else { MUTED });
    match kind {
        WinButton::Minimize => {
            p.line_segment([c + vec2(-6.0, 0.0), c + vec2(6.0, 0.0)], s);
        }
        WinButton::Maximize => {
            let r = Rect::from_center_size(c, vec2(11.0, 11.0));
            p.rect_stroke(r, CornerRadius::same(2), s, StrokeKind::Middle);
            if maximized {
                p.line_segment([r.left_top() + vec2(3.0, -3.0), r.right_top() + vec2(3.0, -3.0)], s);
                p.line_segment([r.right_top() + vec2(3.0, -3.0), r.right_bottom() + vec2(3.0, -3.0)], s);
            }
        }
        WinButton::Close => {
            p.line_segment([c + vec2(-5.5, -5.5), c + vec2(5.5, 5.5)], s);
            p.line_segment([c + vec2(-5.5, 5.5), c + vec2(5.5, -5.5)], s);
        }
    }
}

// ---------------------------------------------------------------------------
// Cards
// ---------------------------------------------------------------------------

fn card(ui: &mut Ui, fill: Color32, add: impl FnOnce(&mut Ui)) {
    Frame::new()
        .fill(fill)
        .stroke(Stroke::new(1.0, EDGE))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            add(ui);
        });
}

fn heading(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).size(12.5).strong().color(MUTED));
}

/// A small label followed by clickable chips that open in the browser or mail client.
fn link_row(ui: &mut Ui, label: &str, items: &[(&str, &str)]) {
    heading(ui, label);
    ui.add_space(6.0);
    ui.horizontal_wrapped(|ui| {
        for &(text, url) in items {
            let resp = ui.add(
                egui::Button::new(RichText::new(text).size(13.0).color(ACCENT))
                    .fill(CHIP)
                    .stroke(Stroke::new(1.0, EDGE))
                    .corner_radius(CornerRadius::same(9)),
            );
            if resp.clicked() {
                ui.ctx().open_url(egui::OpenUrl::new_tab(url));
            }
        }
    });
}

fn field_label(ui: &mut Ui, icon: Icon, text: &str) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(18.0, 16.0), Sense::hover());
        paint_icon(ui.painter(), rect, icon);
        ui.label(RichText::new(text).size(14.5).strong().color(TEXT));
    });
}

#[derive(Clone, Copy)]
enum Icon {
    Folder,
    Scale,
    Threads,
}

fn paint_icon(p: &Painter, r: Rect, icon: Icon) {
    let c = r.center();
    let s = Stroke::new(1.3, ACCENT);
    match icon {
        Icon::Folder => {
            p.rect_stroke(
                Rect::from_center_size(c + vec2(0.0, 1.5), vec2(15.0, 10.0)),
                CornerRadius::same(2),
                s,
                StrokeKind::Middle,
            );
            p.line_segment([c + vec2(-7.5, -3.5), c + vec2(-3.0, -3.5)], s);
        }
        Icon::Scale => {
            p.line_segment([c + vec2(0.0, -6.0), c + vec2(0.0, 6.0)], s);
            p.line_segment([c + vec2(-6.5, -3.0), c + vec2(6.5, -3.0)], s);
            p.line_segment([c + vec2(-4.0, 6.0), c + vec2(4.0, 6.0)], s);
            p.circle_stroke(c + vec2(-6.5, 1.0), 2.5, s);
            p.circle_stroke(c + vec2(6.5, 1.0), 2.5, s);
        }
        Icon::Threads => {
            for dy in [-5.0, 0.0, 5.0] {
                p.line_segment([c + vec2(-7.0, dy), c + vec2(7.0, dy)], Stroke::new(2.0, ACCENT));
            }
        }
    }
}

impl App {
    fn config_card(&mut self, ui: &mut Ui) {
        let running = self.running;
        card(ui, PANEL, |ui| {
            heading(ui, "CONFIGURATION");
            ui.add_space(14.0);

            field_label(ui, Icon::Folder, "Output Directory");
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let w = ui.available_width() - 40.0;
                ui.add_enabled(
                    !running,
                    egui::TextEdit::singleline(&mut self.output_dir)
                        .desired_width(w)
                        .margin(Margin::symmetric(10, 8)),
                );
                if ui.add_enabled(!running, egui::Button::new("…").min_size(vec2(30.0, 32.0))).clicked() {
                    if let Some(dir) = rfd::FileDialog::new().set_title("Output directory").pick_folder() {
                        self.output_dir = dir.display().to_string();
                    }
                }
            });
            ui.add_space(16.0);

            field_label(ui, Icon::Scale, "Max Size per Image");
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.add_enabled(
                    !running,
                    egui::DragValue::new(&mut self.max_kb).range(1..=1_000_000).speed(5.0),
                );
                ui.label(RichText::new("KiB").color(MUTED));
                for kb in [100u64, 300, 500] {
                    let selected = self.max_kb == kb;
                    let label = RichText::new(format!("{kb} KB")).size(12.0).color(if selected { ACCENT } else { MUTED });
                    let button = egui::Button::new(label)
                        .corner_radius(CornerRadius::same(9))
                        .fill(if selected { ACCENT_FILL } else { CHIP })
                        .stroke(Stroke::new(1.0, if selected { ACCENT } else { EDGE }));
                    if ui.add_enabled(!running, button).clicked() {
                        self.max_kb = kb;
                    }
                }
            });
            ui.add_space(16.0);

            field_label(ui, Icon::Threads, "Parallel Workers");
            ui.add_space(4.0);
            ui.add_enabled(!running, egui::DragValue::new(&mut self.jobs).range(1..=64).speed(0.1));

            ui.with_layout(Layout::bottom_up(Align::Center), |ui| {
                let can_start = !self.items.is_empty();
                let width = ui.available_width();
                if self.running {
                    let cancel = egui::Button::new(RichText::new("CANCEL").strong().color(Color32::WHITE))
                        .fill(Color32::from_rgb(0xB0, 0x3A, 0x4A))
                        .corner_radius(CornerRadius::same(8))
                        .min_size(vec2(width, 42.0));
                    if ui.add(cancel).clicked() {
                        self.cancel.store(true, Ordering::Relaxed);
                    }
                } else {
                    let start = egui::Button::new(RichText::new("START CONVERSION").strong().size(15.0).color(ON_ACCENT))
                        .fill(ACCENT)
                        .corner_radius(CornerRadius::same(8))
                        .min_size(vec2(width, 42.0));
                    let resp = ui.add_enabled(can_start, start);
                    // Play triangle to the left of the label.
                    let c = resp.rect.center() + vec2(-98.0, 0.0);
                    ui.painter().add(egui::Shape::convex_polygon(
                        vec![c + vec2(-4.0, -5.5), c + vec2(-4.0, 5.5), c + vec2(5.0, 0.0)],
                        ON_ACCENT,
                        Stroke::NONE,
                    ));
                    if resp.clicked() {
                        let ctx = ui.ctx().clone();
                        self.start(&ctx);
                    }
                }
            });
        });
    }

    fn queue_card(&mut self, ui: &mut Ui) {
        card(ui, PANEL, |ui| {
            ui.horizontal(|ui| {
                heading(ui, "DROP ZONE & ACTIVE QUEUE");
                if self.running {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!("{:.0}%", self.overall_progress() * 100.0))
                                .strong()
                                .color(ACCENT),
                        );
                    });
                }
            });
            ui.add_space(10.0);
            self.drop_zone(ui);
            ui.add_space(10.0);

            let total: u64 = self.items.iter().map(|i| i.bytes).sum();
            ui.horizontal(|ui| {
                let text = format!("{} files  |  Total: {}", self.items.len(), human_size(total));
                Frame::new()
                    .fill(CHIP)
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(Margin::symmetric(10, 4))
                    .show(ui, |ui| ui.label(RichText::new(text).size(12.5).color(MUTED)));
                if !self.running && !self.items.is_empty() {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.link(RichText::new("Clear all").size(12.5)).clicked() {
                            self.items.clear();
                        }
                    });
                }
            });
            ui.add_space(6.0);
            self.queue_list(ui);
        });
    }

    fn info_card(&mut self, ui: &mut Ui) {
        card(ui, PANEL, |ui| {
            heading(ui, "ABOUT");
            ui.add_space(12.0);

            // The dark lockup ships with an opaque background that is exactly our
            // window colour, so it sits on a matching plate.
            let height = 44.0;
            let width = self.lockup.aspect_ratio() * height;
            Frame::new()
                .fill(BG)
                .corner_radius(CornerRadius::same(10))
                .inner_margin(Margin::symmetric(14, 12))
                .show(ui, |ui| {
                    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
                    paint_image(ui.painter(), &self.lockup, rect);
                });

            ui.add_space(10.0);
            ui.label(
                RichText::new(format!("Version {}", env!("CARGO_PKG_VERSION")))
                    .size(13.0)
                    .color(MUTED),
            );

            ui.add_space(14.0);
            ui.label(
                RichText::new(
                    format!(
                        "Shrinks PNG, JPEG, TIFF, WEBP{} images into WebP files, walking a \
                         ladder of widths and qualities until every result fits your size limit.",
                        if cfg!(feature = "heic") { " and HEIC/HEIF" } else { "" }
                    )
                )
                .size(14.0)
                .color(TEXT.gamma_multiply(0.9)),
            );

            ui.add_space(18.0);
            ui.separator();
            ui.add_space(14.0);

            ui.label(RichText::new(DEVELOPER).size(15.0).strong().color(ACCENT));
            ui.add_space(6.0);
            ui.label(RichText::new(COMPANY).size(13.5).color(TEXT.gamma_multiply(0.9)));

            ui.add_space(16.0);
            link_row(ui, "WEBSITES", &LINKS);
            ui.add_space(14.0);
            link_row(ui, "EMAIL", &EMAILS);
        });
    }

    fn drop_zone(&mut self, ui: &mut Ui) {
        let hovering = ui.ctx().input(|i| !i.raw.hovered_files.is_empty());
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 84.0), Sense::click());
        let active = hovering || (resp.hovered() && !self.running);
        let p = ui.painter();
        p.rect_filled(rect, CornerRadius::same(10), if active { Color32::from_rgb(0x1A, 0x4B, 0x4B) } else { ACCENT_FILL });
        let corners = [
            rect.shrink(1.0).left_top(),
            rect.shrink(1.0).right_top(),
            rect.shrink(1.0).right_bottom(),
            rect.shrink(1.0).left_bottom(),
            rect.shrink(1.0).left_top(),
        ];
        let stroke = Stroke::new(if active { 2.0 } else { 1.5 }, if active { ACCENT } else { ACCENT.gamma_multiply(0.75) });
        p.extend(egui::Shape::dashed_line(&corners, stroke, 7.0, 5.0));

        let c = rect.center();
        p.text(c + vec2(0.0, -12.0), Align2::CENTER_CENTER, "Drag & Drop Images Here", FontId::proportional(16.0), ACCENT);
        p.text(
            c + vec2(0.0, 14.0),
            Align2::CENTER_CENTER,
            "or  [ Browse Files ]",
            FontId::proportional(14.0),
            if active { ACCENT } else { MUTED },
        );
        if resp.clicked() && !self.running {
            self.browse_files();
        }
    }

    fn queue_list(&mut self, ui: &mut Ui) {
        if self.items.is_empty() {
            ui.add_space(24.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("No images yet — drop some above to get started.").color(MUTED));
                ui.label(RichText::new(format!("PNG · JPEG · TIFF · WEBP{}", if cfg!(feature = "heic") { " · HEIC/HEIF" } else { "" })).size(12.0).color(MUTED.gamma_multiply(0.7)));
            });
            return;
        }

        let mut remove: Option<usize> = None;
        let running = self.running;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for (index, item) in self.items.iter().enumerate() {
                if queue_row(ui, index, item, running) {
                    remove = Some(index);
                }
            }
        });
        if let Some(index) = remove {
            self.items.remove(index);
        }
    }

    fn terminal(&mut self, ui: &mut Ui) {
        let open = self.log_open;
        card(ui, PANEL_DEEP, |ui| {
            ui.horizontal(|ui| {
                let (rect, resp) = ui.allocate_exact_size(vec2(130.0, 18.0), Sense::click());
                let c = pos2(rect.left() + 6.0, rect.center().y);
                let s = Stroke::new(1.5, MUTED);
                if open {
                    ui.painter().line_segment([c + vec2(-4.0, -2.0), c + vec2(0.0, 2.0)], s);
                    ui.painter().line_segment([c + vec2(0.0, 2.0), c + vec2(4.0, -2.0)], s);
                } else {
                    ui.painter().line_segment([c + vec2(-2.0, -4.0), c + vec2(2.0, 0.0)], s);
                    ui.painter().line_segment([c + vec2(2.0, 0.0), c + vec2(-2.0, 4.0)], s);
                }
                ui.painter().text(
                    pos2(rect.left() + 20.0, rect.center().y),
                    Align2::LEFT_CENTER,
                    "TERMINAL LOG",
                    FontId::proportional(12.5),
                    if resp.hovered() { TEXT } else { MUTED },
                );
                if resp.clicked() {
                    self.log_open = !self.log_open;
                }
                if open {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.link(RichText::new("Clear").size(12.0)).clicked() {
                            self.log.clear();
                        }
                    });
                }
            });
            if !open {
                return;
            }
            ui.add_space(4.0);
            let row_h = 16.0;
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .show_rows(ui, row_h, self.log.len(), |ui, range| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for (level, text) in &self.log[range] {
                        let color = match level {
                            Level::Info | Level::Try => ACCENT,
                            Level::SizeOk | Level::Ok => GREEN,
                            Level::SizeBig | Level::Warn => YELLOW,
                            Level::Error => RED,
                        };
                        let mut job = egui::text::LayoutJob::default();
                        let mono = FontId::monospace(12.5);
                        job.append(&format!("[{}] ", level.tag()), 0.0, egui::TextFormat::simple(mono.clone(), color));
                        job.append(text, 0.0, egui::TextFormat::simple(mono, TEXT.gamma_multiply(0.85)));
                        ui.add(egui::Label::new(job).truncate());
                    }
                });
        });
    }
}

/// Draws one queue row. Returns true if its remove button was clicked.
fn queue_row(ui: &mut Ui, index: usize, item: &Item, running: bool) -> bool {
    let converting = matches!(item.status, Status::Converting { .. });
    let has_sub = !matches!(item.status, Status::Waiting);
    let height = match item.status {
        Status::Converting { .. } => 58.0,
        Status::Done { .. } | Status::Failed(_) => 52.0,
        Status::Waiting => 42.0,
    };
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    let p = ui.painter_at(rect);

    if converting {
        p.rect_filled(rect.shrink2(vec2(0.0, 1.0)), CornerRadius::same(8), ACCENT_ROW);
    } else {
        p.line_segment([rect.left_bottom(), rect.right_bottom()], Stroke::new(1.0, EDGE));
    }

    // File icon.
    let icon = Rect::from_center_size(pos2(rect.left() + 20.0, rect.center().y), vec2(14.0, 16.0));
    let icon_color = if converting { ACCENT } else { MUTED };
    p.rect_stroke(icon, CornerRadius::same(3), Stroke::new(1.2, icon_color), StrokeKind::Middle);
    p.line_segment([icon.left_bottom() + vec2(2.0, -4.0), icon.center() + vec2(0.0, 1.0)], Stroke::new(1.2, icon_color));
    p.line_segment([icon.center() + vec2(0.0, 1.0), icon.right_bottom() + vec2(-2.0, -4.0)], Stroke::new(1.2, icon_color));

    // Name (+ sub line).
    let name = item.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let name_chars = ((rect.width() * 0.45 - 50.0) / 7.4).max(10.0) as usize;
    let name_y = if has_sub { rect.center().y - 9.0 } else { rect.center().y };
    p.text(
        pos2(rect.left() + 38.0, name_y),
        Align2::LEFT_CENTER,
        clip(&name, name_chars),
        FontId::proportional(14.5),
        TEXT,
    );

    let sub = match &item.status {
        Status::Converting { width, quality, .. } => Some((format!("Width: {width}px, Q: {quality}"), MUTED)),
        Status::Done { width, quality, .. } => Some((format!("Width: {width}px, Q: {quality}"), MUTED)),
        Status::Failed(reason) => Some((clip(reason, name_chars + 8), RED)),
        Status::Waiting => None,
    };
    if let Some((text, color)) = sub {
        let y = rect.center().y + 10.0;
        p.text(pos2(rect.left() + 38.0, y), Align2::LEFT_CENTER, text, FontId::proportional(12.0), color);
    }

    // Format tag.
    pill(&p, pos2(rect.left() + rect.width() * 0.5, name_y), Align2::LEFT_CENTER, item.tag, TEXT.gamma_multiply(0.9), CHIP, EDGE);

    // Status on the right.
    let right = rect.right() - 12.0;
    let mut remove = false;
    match &item.status {
        Status::Waiting => {
            pill(&p, pos2(right, name_y), Align2::RIGHT_CENTER, "Waiting...", MUTED, CHIP, EDGE);
        }
        Status::Converting { fraction, .. } => {
            egui::Spinner::new().size(14.0).color(ACCENT).paint_at(
                ui,
                Rect::from_center_size(pos2(right - 100.0, name_y), vec2(14.0, 14.0)),
            );
            pill(&p, pos2(right, name_y), Align2::RIGHT_CENTER, "Converting...", ACCENT, ACCENT_FILL, ACCENT.gamma_multiply(0.5));
            let bar = Rect::from_min_size(pos2(right - 150.0, rect.center().y + 11.0), vec2(110.0, 5.0));
            p.rect_filled(bar, CornerRadius::same(3), PANEL_DEEP);
            p.rect_filled(
                Rect::from_min_size(bar.min, vec2(bar.width() * fraction.clamp(0.0, 1.0), bar.height())),
                CornerRadius::same(3),
                ACCENT,
            );
            p.text(
                pos2(right, bar.center().y),
                Align2::RIGHT_CENTER,
                format!("{:.0}%", fraction * 100.0),
                FontId::proportional(12.0),
                MUTED,
            );
        }
        Status::Done { size, .. } => {
            pill(
                &p,
                pos2(right, name_y),
                Align2::RIGHT_CENTER,
                &format!("Done · {} KiB", converter::kib(*size)),
                GREEN,
                Color32::from_rgb(0x13, 0x33, 0x2E),
                GREEN.gamma_multiply(0.4),
            );
        }
        Status::Failed(_) => {
            pill(&p, pos2(right, name_y), Align2::RIGHT_CENTER, "Failed", RED, Color32::from_rgb(0x3A, 0x1B, 0x24), RED.gamma_multiply(0.5));
        }
    }

    // Remove button (hover, idle only).
    if !running && resp.hovered() {
        let x = Rect::from_center_size(pos2(rect.right() - 120.0, name_y), vec2(20.0, 20.0));
        let r = ui.interact(x, ui.scope_id().with(("remove", index)), Sense::click());
        let c = x.center();
        let s = Stroke::new(1.3, if r.hovered() { RED } else { MUTED });
        p.line_segment([c + vec2(-4.0, -4.0), c + vec2(4.0, 4.0)], s);
        p.line_segment([c + vec2(-4.0, 4.0), c + vec2(4.0, -4.0)], s);
        remove = r.clicked();
    }
    remove
}

fn pill(p: &Painter, anchor: Pos2, align: Align2, text: &str, fg: Color32, bg: Color32, edge: Color32) {
    let galley = p.layout_no_wrap(text.to_string(), FontId::proportional(12.0), fg);
    let size = galley.size() + vec2(18.0, 8.0);
    let rect = align.anchor_size(anchor, size);
    p.rect(rect, CornerRadius::same(10), bg, Stroke::new(1.0, edge), StrokeKind::Inside);
    p.galley(rect.min + vec2(9.0, 4.0), galley, fg);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every string the UI painted, including nested shapes.
    fn painted_text(output: &egui::FullOutput) -> Vec<String> {
        fn walk(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(text) => out.push(text.galley.text().to_owned()),
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        walk(shape, out);
                    }
                }
                _ => {}
            }
        }
        let mut out = Vec::new();
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut out);
        }
        out
    }

    /// Texture ids referenced by painted meshes (i.e. images).
    fn painted_textures(output: &egui::FullOutput) -> Vec<egui::TextureId> {
        fn walk(shape: &egui::Shape, out: &mut Vec<egui::TextureId>) {
            match shape {
                egui::Shape::Mesh(mesh) => out.push(mesh.texture_id),
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        walk(shape, out);
                    }
                }
                _ => {}
            }
        }
        let mut out = Vec::new();
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut out);
        }
        out
    }

    fn test_input(events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 640.0))),
            events,
            ..Default::default()
        }
    }

    /// Mirrors the real frame: title bar panel on top, then the active tab's
    /// content. Returns the `INFO` tab rect so tests can click it.
    fn app_frame(app: &mut App, ui: &mut Ui) -> Rect {
        let mut info = Rect::NOTHING;
        egui::Panel::top("title_bar")
            .exact_size(64.0)
            .frame(Frame::NONE.inner_margin(Margin::symmetric(20, 10)))
            .show(ui, |ui| {
                info = tab_rects(ui.max_rect())[1].1;
                let mut tab = app.tab;
                title_bar(ui, false, &mut tab, &app.icon);
                app.tab = tab;
            });
        match app.tab {
            Tab::Convert => {
                app.config_card(ui);
                app.queue_card(ui);
                app.terminal(ui);
            }
            Tab::Info => app.info_card(ui),
        }
        info
    }

    fn frame(app: &mut App, ctx: &egui::Context, events: Vec<egui::Event>) -> (Vec<String>, Rect) {
        let mut info = Rect::NOTHING;
        let output = ctx.run_ui(test_input(events), |ui| info = app_frame(app, ui));
        (painted_text(&output), info)
    }

    /// Texture ids painted by one frame of the current tab.
    fn frame_textures(app: &mut App, ctx: &egui::Context) -> Vec<egui::TextureId> {
        let output = ctx.run_ui(test_input(Vec::new()), |ui| {
            app_frame(app, ui);
        });
        painted_textures(&output)
    }

    fn new_app() -> (egui::Context, App) {
        let ctx = egui::Context::default();
        apply_theme(&ctx);
        let app = App::new(&ctx);
        (ctx, app)
    }

    fn click(pos: Pos2) -> Vec<egui::Event> {
        let button = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        vec![egui::Event::PointerMoved(pos), button(true), button(false)]
    }

    #[test]
    fn both_tabs_render() {
        for tab in [Tab::Convert, Tab::Info] {
            let (ctx, mut app) = new_app();
            app.tab = tab;
            let (text, _) = frame(&mut app, &ctx, Vec::new());
            assert!(!text.is_empty(), "{tab:?} painted nothing");
        }
    }

    #[test]
    fn title_bar_shows_name_and_credit() {
        let (ctx, mut app) = new_app();
        let (text, _) = frame(&mut app, &ctx, Vec::new());
        let text = text.join("\n");
        assert!(text.contains(APP_NAME), "missing app name");
        assert!(text.contains(APP_CREDIT), "missing Araz Code credit");
        assert!(text.contains("CONVERT") && text.contains("INFO"), "missing tabs");
    }

    #[test]
    fn info_tab_shows_araz_code_credits() {
        let (ctx, mut app) = new_app();
        app.tab = Tab::Info;
        let (text, _) = frame(&mut app, &ctx, Vec::new());
        let text = text.join("\n");
        assert!(text.contains(DEVELOPER), "missing developer line");
        assert!(text.contains(COMPANY), "missing company line");
        for (label, _) in LINKS {
            assert!(text.contains(label), "missing link {label}");
        }
        for (label, _) in EMAILS {
            assert!(text.contains(label), "missing email {label}");
        }
    }

    #[test]
    fn logos_load_from_assets() {
        let (_, app) = new_app();
        assert_eq!(app.icon.size(), [64, 64]);
        assert_eq!(app.lockup.size(), [974, 240]);

        let icon = window_icon();
        assert_eq!((icon.width, icon.height), (256, 256));
        assert_eq!(icon.rgba.len(), 256 * 256 * 4);
    }

    #[test]
    fn brand_images_are_painted() {
        let (ctx, mut app) = new_app();
        let icon = app.icon.id();
        let lockup = app.lockup.id();

        let convert = frame_textures(&mut app, &ctx);
        assert!(convert.contains(&icon), "title-bar icon not painted");

        app.tab = Tab::Info;
        let info = frame_textures(&mut app, &ctx);
        assert!(info.contains(&icon), "title-bar icon missing on Info tab");
        assert!(info.contains(&lockup), "lockup not painted on Info tab");
    }

    #[test]
    fn clicking_tabs_switches_between_convert_and_info() {
        let (ctx, mut app) = new_app();
        let (_, info) = frame(&mut app, &ctx, Vec::new());
        assert_eq!(app.tab, Tab::Convert);

        frame(&mut app, &ctx, click(info.center()));
        assert_eq!(app.tab, Tab::Info, "clicking INFO should switch tabs");
        let (text, _) = frame(&mut app, &ctx, Vec::new());
        assert!(text.join("\n").contains(DEVELOPER), "info content not shown");

        let convert = tab_rects(Rect::from_min_size(pos2(20.0, 10.0), vec2(960.0, 44.0)))[0].1.center();
        frame(&mut app, &ctx, click(convert));
        assert_eq!(app.tab, Tab::Convert, "clicking CONVERT should switch back");
    }

    #[test]
    fn tab_rects_stay_clear_of_window_buttons() {
        let bar = Rect::from_min_size(pos2(20.0, 10.0), vec2(960.0, 44.0));
        let [(_, convert), (_, info)] = tab_rects(bar);
        assert!(convert.left() > bar.left(), "tabs overlap the title text");
        assert!(convert.right() < info.left(), "tabs overlap each other");
        let buttons_left = bar.right() - (3.0 * WIN_BUTTON_SIZE.x + 2.0 * WIN_BUTTON_GAP);
        assert!(info.right() < buttons_left, "tabs overlap the window buttons");
        assert_eq!(convert.width(), TAB_WIDTH);
        assert_eq!(info.width(), TAB_WIDTH);
    }
}
