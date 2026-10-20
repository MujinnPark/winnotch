// Hide the console window in release builds on Windows.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod features;
mod lyrics;
mod media;
mod store;
mod system;

use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Datelike, Local, Timelike, Weekday};
use eframe::egui::{
    self,
    load::SizedTexture,
    text::{Galley, LayoutJob},
    Align2, Color32, ColorImage, FontId, Painter, Pos2, Rect, Rounding, Sense, Stroke,
    TextureHandle, TextureOptions, Vec2, ViewportCommand,
};
use features::{fmt_mmss, parse_task_input, FocusTimer, Phase, Task, Water};
use media::{LyricsStatus, MediaCmd, MediaHandle, NowPlaying};
use system::SysEvent;

const COMPACT: Vec2 = Vec2::new(220.0, 34.0);
const EXPANDED: Vec2 = Vec2::new(600.0, 188.0);
const TOAST: Vec2 = Vec2::new(330.0, 46.0);
const ORANGE: Color32 = Color32::from_rgb(255, 159, 10);
const GREEN: Color32 = Color32::from_rgb(48, 209, 88);
const COMPACT_RADIUS: f32 = 14.0;
const EXPANDED_RADIUS: f32 = 32.0;
const BLUE: Color32 = Color32::from_rgb(10, 132, 255);
const RED: Color32 = Color32::from_rgb(255, 69, 58);
const TILES_PER_PAGE: usize = 5;

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Nook,
    Tray,
    Focus,
    Water,
    Todo,
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Folder,
    Image,
    File,
}

struct TrayItem {
    path: PathBuf,
    name: String,
    ext: String,
    info: String,
    kind: Kind,
    tex: Option<TextureHandle>,
}

#[derive(Clone, Copy, PartialEq)]
enum ToastKind {
    Volume,
    Muted,
    Brightness,
    Caps,
    Lang,
    Charger,
    Wifi,
    Audio,
    Focus,
    Water,
    Todo,
}

struct Toast {
    kind: ToastKind,
    title: String,
    sub: String,
    level: Option<f32>,
    until: Instant,
}

struct Notch {
    /// 0.0 = compact, 1.0 = expanded
    t: f32,
    tab: Tab,
    show_lyrics: bool,
    /// Latest lookup result for the current track (kept while the panel fades out).
    lyrics_status: LyricsStatus,
    /// Smoothed (fractional) index of the line being sung; drives the scrolling.
    lyric_pos: f32,
    /// Identity of the lyrics `lyric_pos` belongs to (a new song snaps instead of scrolling).
    lyric_ptr: usize,
    /// Shift applied to the playback position for lyrics; positive = lyrics appear earlier.
    lyric_offset_ms: i64,
    /// 0 = calendar fully shown, 1 = lyrics fully shown (cross-fade).
    lyrics_vis: f32,
    focus: FocusTimer,
    water: Water,
    tasks: Vec<Task>,
    todo_input: String,
    sys_rx: Receiver<SysEvent>,
    toast: Option<Toast>,
    /// 0 = plain compact notch, 1 = toast pill fully shown
    toast_t: f32,
    tray: Vec<TrayItem>,
    tray_page: usize,
    media: MediaHandle,
    art_tex: Option<TextureHandle>,
    art_id: u64,
    /// Files chosen through the "+ Add files" dialog (filled from a helper thread).
    picked: Arc<Mutex<Vec<PathBuf>>>,
    /// Decoded image thumbnails waiting to become textures (filled from helper threads).
    thumbs: Arc<Mutex<Vec<(PathBuf, ColorImage)>>>,
}

impl Notch {
    fn new() -> Self {
        Self {
            t: 0.0,
            tab: Tab::Nook,
            show_lyrics: false,
            lyrics_status: LyricsStatus::Idle,
            lyric_pos: 0.0,
            lyric_ptr: 0,
            lyric_offset_ms: load_offset(),
            lyrics_vis: 0.0,
            focus: FocusTimer::new(),
            water: store::load_water(&Local::now().format("%Y-%m-%d").to_string()),
            tasks: store::load_tasks(),
            todo_input: String::new(),
            sys_rx: system::spawn(),
            toast: None,
            toast_t: 0.0,
            tray: Vec::new(),
            tray_page: 0,
            media: MediaHandle::spawn(),
            art_tex: None,
            art_id: u64::MAX,
            picked: Default::default(),
            thumbs: Default::default(),
        }
    }

    fn add_path(&mut self, ctx: &egui::Context, path: PathBuf) {
        if self.tray.iter().any(|i| i.path == path) {
            return;
        }
        let meta = std::fs::metadata(&path).ok();
        let is_dir = meta.as_ref().map_or(false, |m| m.is_dir());
        let size = meta.as_ref().map_or(0, |m| m.len());
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string();
        let is_img = !is_dir && matches!(ext.as_str(), "png" | "jpg" | "jpeg");
        let kind = if is_dir {
            Kind::Folder
        } else if is_img {
            Kind::Image
        } else {
            Kind::File
        };
        let info = if is_dir {
            "Folder".to_string()
        } else {
            human_size(size)
        };

        if is_img {
            let thumbs = self.thumbs.clone();
            let p2 = path.clone();
            let ctx2 = ctx.clone();
            std::thread::spawn(move || {
                if let Ok(img) = image::open(&p2) {
                    let t = img.thumbnail(160, 160).to_rgba8();
                    let ci = ColorImage::from_rgba_unmultiplied(
                        [t.width() as usize, t.height() as usize],
                        t.as_raw(),
                    );
                    if let Ok(mut v) = thumbs.lock() {
                        v.push((p2, ci));
                    }
                    ctx2.request_repaint();
                }
            });
        }
        self.tray.push(TrayItem {
            path,
            name,
            ext,
            info,
            kind,
            tex: None,
        });
    }

    fn open_picker(&self, ctx: &egui::Context) {
        #[cfg(windows)]
        {
            let picked = self.picked.clone();
            let ctx2 = ctx.clone();
            std::thread::spawn(move || {
                if let Some(files) = rfd::FileDialog::new().pick_files() {
                    if let Ok(mut v) = picked.lock() {
                        v.extend(files);
                    }
                }
                ctx2.request_repaint();
            });
        }
        #[cfg(not(windows))]
        {
            let _ = ctx;
        }
    }
}

// ---------------------------------------------------------------------------
// small helpers
// ---------------------------------------------------------------------------

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

const DEFAULT_OFFSET_MS: i64 = 500;

fn offset_file() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .or_else(|| std::env::var_os("HOME"))?;
    Some(
        PathBuf::from(base)
            .join("winnotch")
            .join("lyrics_offset_ms.txt"),
    )
}

fn load_offset() -> i64 {
    offset_file()
        .and_then(|f| std::fs::read_to_string(f).ok())
        .and_then(|s| s.trim().parse::<i64>().ok())
        .map(|v| v.clamp(-3000, 3000))
        .unwrap_or(DEFAULT_OFFSET_MS)
}

fn save_offset(ms: i64) {
    if let Some(f) = offset_file() {
        if let Some(dir) = f.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(f, ms.to_string());
    }
}

fn human_size(b: u64) -> String {
    const KB: f64 = 1024.0;
    let f = b as f64;
    if f < KB {
        format!("{} B", b)
    } else if f < KB * KB {
        format!("{:.0} KB", f / KB)
    } else if f < KB * KB * KB {
        format!("{:.1} MB", f / (KB * KB))
    } else {
        format!("{:.1} GB", f / (KB * KB * KB))
    }
}

fn fmt_time(ms: u64) -> String {
    let s = ms / 1000;
    format!("{}:{:02}", s / 60, s % 60)
}

fn open_path(p: &Path) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("explorer").arg(p).spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("xdg-open").arg(p).spawn();
    }
}

/// Lays out text wrapped to `w` and truncated to `rows` rows with an ellipsis.
fn layout(p: &Painter, text: &str, size: f32, color: Color32, w: f32, rows: usize) -> Arc<Galley> {
    let mut job = LayoutJob::simple(text.to_string(), FontId::proportional(size), color, w);
    job.wrap.max_rows = rows;
    job.wrap.overflow_character = Some('…');
    p.layout_job(job)
}

/// Draws wrapped/truncated text at `pos`, returns its height.
fn text_block(
    p: &Painter,
    pos: Pos2,
    text: &str,
    size: f32,
    color: Color32,
    w: f32,
    rows: usize,
) -> f32 {
    let g = layout(p, text, size, color, w, rows);
    let h = g.size().y;
    p.galley(pos, g, color);
    h
}

/// One line of text centered horizontally on `cx`, truncated to `w`.
fn text_centered(p: &Painter, cx: f32, y: f32, text: &str, size: f32, color: Color32, w: f32) {
    let g = layout(p, text, size, color, w, 1);
    let x = cx - g.size().x / 2.0;
    p.galley(Pos2::new(x, y), g, color);
}

fn polyline(p: &Painter, pts: &[Pos2], stroke: Stroke) {
    for w in pts.windows(2) {
        p.line_segment([w[0], w[1]], stroke);
    }
}

/// Dashed line along a polyline.
fn dashed_path(p: &Painter, pts: &[Pos2], stroke: Stroke, dash: f32, gap: f32) {
    let mut draw = true;
    let mut remain = dash;
    for w in pts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let seg = b - a;
        let len = seg.length();
        if len <= 0.0 {
            continue;
        }
        let dir = seg / len;
        let mut pos = 0.0;
        while pos < len {
            let step = remain.min(len - pos);
            if draw {
                p.line_segment([a + dir * pos, a + dir * (pos + step)], stroke);
            }
            pos += step;
            remain -= step;
            if remain <= 0.0 {
                draw = !draw;
                remain = if draw { dash } else { gap };
            }
        }
    }
}

/// Closed outline of a rounded rectangle (clockwise from the top edge).
fn rounded_path(r: Rect, rad: f32) -> Vec<Pos2> {
    let corners = [
        (Pos2::new(r.right() - rad, r.top() + rad), -90.0_f32),
        (Pos2::new(r.right() - rad, r.bottom() - rad), 0.0),
        (Pos2::new(r.left() + rad, r.bottom() - rad), 90.0),
        (Pos2::new(r.left() + rad, r.top() + rad), 180.0),
    ];
    let mut pts = Vec::new();
    for (c, start) in corners {
        for i in 0..=8 {
            let a = (start + 90.0 * i as f32 / 8.0).to_radians();
            pts.push(Pos2::new(c.x + rad * a.cos(), c.y + rad * a.sin()));
        }
    }
    let first = pts[0];
    pts.push(first);
    pts
}

/// Clickable glyph button; returns true if clicked.
fn icon_btn(ui: &egui::Ui, id: &str, center: Pos2, glyph: &str, size: f32, color: Color32) -> bool {
    let r = Rect::from_center_size(center, Vec2::splat(size + 14.0));
    let resp = ui.interact(r, egui::Id::new(id), Sense::click());
    if resp.hovered() {
        ui.painter()
            .rect_filled(r, Rounding::same(8.0), Color32::from_white_alpha(18));
    }
    ui.painter().text(
        center,
        Align2::CENTER_CENTER,
        glyph,
        FontId::proportional(size),
        color,
    );
    resp.clicked()
}

/// Points on a circle arc from angle `a0` to `a1` (radians, 0 = right, clockwise on screen).
fn arc_points(c: Pos2, r: f32, a0: f32, a1: f32) -> Vec<Pos2> {
    let steps = (((a1 - a0).abs() / std::f32::consts::TAU) * 72.0)
        .ceil()
        .max(2.0) as usize;
    (0..=steps)
        .map(|i| {
            let a = a0 + (a1 - a0) * i as f32 / steps as f32;
            Pos2::new(c.x + r * a.cos(), c.y + r * a.sin())
        })
        .collect()
}

/// Small line icon for a toast, centered on `c` (about 20 x 20 px).
fn draw_icon(p: &Painter, kind: ToastKind, c: Pos2, col: Color32, on: bool) {
    let s = Stroke::new(1.8_f32, col);
    let at = |x: f32, y: f32| Pos2::new(c.x + x, c.y + y);
    match kind {
        ToastKind::Volume | ToastKind::Muted => {
            polyline(
                p,
                &[
                    at(-8.0, -3.0),
                    at(-4.0, -3.0),
                    at(1.0, -8.0),
                    at(1.0, 8.0),
                    at(-4.0, 3.0),
                    at(-8.0, 3.0),
                    at(-8.0, -3.0),
                ],
                s,
            );
            if kind == ToastKind::Muted {
                p.line_segment([at(4.0, -4.0), at(10.0, 4.0)], s);
                p.line_segment([at(10.0, -4.0), at(4.0, 4.0)], s);
            } else {
                polyline(p, &arc_points(at(1.0, 0.0), 6.0, -0.9, 0.9), s);
                polyline(p, &arc_points(at(1.0, 0.0), 10.0, -0.9, 0.9), s);
            }
        }
        ToastKind::Brightness => {
            p.circle_stroke(c, 4.0, s);
            for i in 0..8 {
                let a = i as f32 * std::f32::consts::TAU / 8.0;
                let (ca, sa) = (a.cos(), a.sin());
                p.line_segment([at(ca * 7.0, sa * 7.0), at(ca * 10.0, sa * 10.0)], s);
            }
        }
        ToastKind::Caps => {
            polyline(p, &[at(-7.0, 0.0), at(0.0, -8.0), at(7.0, 0.0)], s);
            p.line_segment([at(0.0, -7.0), at(0.0, 5.0)], s);
            p.line_segment([at(-6.0, 9.0), at(6.0, 9.0)], s);
            if !on {
                p.line_segment([at(-9.0, 9.0), at(9.0, -9.0)], s);
            }
        }
        ToastKind::Lang => {
            p.circle_stroke(c, 9.0, s);
            p.line_segment([at(-9.0, 0.0), at(9.0, 0.0)], s);
            p.line_segment([at(0.0, -9.0), at(0.0, 9.0)], s);
        }
        ToastKind::Charger => {
            polyline(
                p,
                &[
                    at(2.0, -9.0),
                    at(-4.0, 1.0),
                    at(0.0, 1.0),
                    at(-2.0, 9.0),
                    at(5.0, -2.0),
                    at(1.0, -2.0),
                    at(2.0, -9.0),
                ],
                s,
            );
        }
        ToastKind::Wifi => {
            for r in [10.0_f32, 6.5, 3.0] {
                polyline(p, &arc_points(at(0.0, 6.0), r, -2.2, -0.94), s);
            }
            p.circle_filled(at(0.0, 7.0), 1.4, col);
            if !on {
                p.line_segment([at(-9.0, -7.0), at(9.0, 9.0)], s);
            }
        }
        ToastKind::Audio => {
            // headphones
            polyline(
                p,
                &arc_points(
                    at(0.0, 2.0),
                    8.0,
                    std::f32::consts::PI,
                    std::f32::consts::TAU,
                ),
                s,
            );
            p.rect_stroke(
                Rect::from_min_max(at(-10.0, 2.0), at(-5.0, 9.0)),
                Rounding::same(2.0),
                s,
            );
            p.rect_stroke(
                Rect::from_min_max(at(5.0, 2.0), at(10.0, 9.0)),
                Rounding::same(2.0),
                s,
            );
        }
        ToastKind::Focus => {
            p.circle_stroke(at(0.0, 1.0), 8.0, s);
            p.line_segment([at(0.0, 1.0), at(0.0, -4.0)], s);
            p.line_segment([at(-3.0, -9.0), at(3.0, -9.0)], s);
        }
        ToastKind::Water => {
            let mut pts = vec![at(0.0, -9.0)];
            pts.extend(arc_points(
                at(0.0, 3.0),
                6.5,
                -0.35,
                std::f32::consts::PI + 0.35,
            ));
            pts.push(at(0.0, -9.0));
            polyline(p, &pts, s);
        }
        ToastKind::Todo => {
            p.circle_stroke(c, 9.0, s);
            polyline(p, &[at(-4.0, 0.0), at(-1.0, 3.5), at(5.0, -3.5)], s);
        }
    }
}

/// "label  [−] value [+]" row; returns -1 / 0 / +1 for a click on − / +.
fn stepper(
    ui: &egui::Ui,
    p: &Painter,
    look: &Look,
    id: &str,
    x: f32,
    y: f32,
    label: &str,
    value: &str,
) -> i32 {
    p.text(
        Pos2::new(x, y + 10.0),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(11.0),
        look.dim,
    );
    let minus = Rect::from_min_size(Pos2::new(x + 44.0, y), Vec2::new(20.0, 20.0));
    let plus = Rect::from_min_size(Pos2::new(x + 118.0, y), Vec2::new(20.0, 20.0));
    let mut d = 0;
    if pill(ui, p, look, &format!("{id}_m"), minus, "−", false) {
        d = -1;
    }
    if pill(ui, p, look, &format!("{id}_p"), plus, "+", false) {
        d = 1;
    }
    p.text(
        Pos2::new(x + 91.0, y + 10.0),
        Align2::CENTER_CENTER,
        value,
        FontId::proportional(12.0),
        look.white,
    );
    d
}

/// Short alert sound (Windows only).
fn chime() {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::System::Diagnostics::Debug::MessageBeep;
        use windows::Win32::UI::WindowsAndMessaging::MB_ICONASTERISK;
        let _ = MessageBeep(MB_ICONASTERISK);
    }
}

/// Colors that fade in as the notch expands, plus the content origin.
struct Look {
    af: f32,
    white: Color32,
    grey: Color32,
    dim: Color32,
    ox: f32,
    top: f32,
}

impl Look {
    fn new(rect: Rect, t: f32) -> Self {
        let af = ((t - 0.5) * 2.0).clamp(0.0, 1.0);
        let f = |c: Color32| c.gamma_multiply(af);
        Self {
            af,
            white: f(Color32::WHITE),
            grey: f(Color32::from_gray(165)),
            dim: f(Color32::from_gray(115)),
            // Anchor content to the final size so it doesn't slide while animating.
            ox: rect.center().x - EXPANDED.x / 2.0,
            top: rect.top(),
        }
    }

    fn fade(&self, c: Color32) -> Color32 {
        c.gamma_multiply(self.af)
    }

    /// Same origin, but everything additionally faded by `k` (for cross-fades).
    fn scaled(&self, k: f32) -> Look {
        Look {
            af: self.af * k,
            white: self.white.gamma_multiply(k),
            grey: self.grey.gamma_multiply(k),
            dim: self.dim.gamma_multiply(k),
            ox: self.ox,
            top: self.top,
        }
    }
}

/// Small rounded button in the header row; returns true if clicked.
fn pill(
    ui: &egui::Ui,
    p: &Painter,
    look: &Look,
    id: &str,
    r: Rect,
    label: &str,
    active: bool,
) -> bool {
    let resp = ui.interact(r, egui::Id::new(id), Sense::click());
    let base: u8 = if active { 72 } else { 44 };
    let g = if resp.hovered() { base + 16 } else { base };
    p.rect_filled(r, Rounding::same(14.0), look.fade(Color32::from_gray(g)));
    p.text(
        r.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(13.0),
        if active { look.white } else { look.grey },
    );
    resp.clicked()
}

fn ext_color(ext: &str) -> Color32 {
    match ext {
        "pdf" => Color32::from_rgb(235, 80, 70),
        "doc" | "docx" | "rtf" | "txt" | "odt" => Color32::from_rgb(70, 140, 245),
        "xls" | "xlsx" | "csv" => Color32::from_rgb(60, 180, 100),
        "ppt" | "pptx" => Color32::from_rgb(240, 120, 60),
        "zip" | "rar" | "7z" | "tar" | "gz" => Color32::from_rgb(235, 190, 60),
        "mp3" | "wav" | "flac" | "m4a" | "ogg" => Color32::from_rgb(170, 110, 240),
        "mp4" | "mkv" | "avi" | "mov" | "webm" => Color32::from_rgb(240, 110, 150),
        _ => Color32::from_rgb(140, 160, 190),
    }
}

/// Draws a file / folder / image icon centered at `c` (about 56 x 46 px).
fn draw_file_icon(p: &Painter, look: &Look, item: &TrayItem, c: Pos2) {
    match (item.kind, &item.tex) {
        (Kind::Folder, _) => {
            let body = Rect::from_center_size(Pos2::new(c.x, c.y + 3.0), Vec2::new(48.0, 33.0));
            let tab = Rect::from_min_size(
                Pos2::new(body.left(), body.top() - 6.0),
                Vec2::new(20.0, 10.0),
            );
            p.rect_filled(
                tab,
                Rounding::same(3.0),
                look.fade(Color32::from_rgb(222, 160, 40)),
            );
            p.rect_filled(
                body,
                Rounding::same(5.0),
                look.fade(Color32::from_rgb(250, 200, 70)),
            );
        }
        (Kind::Image, Some(tex)) => {
            let sz = tex.size_vec2();
            let s = (56.0 / sz.x).min(46.0 / sz.y);
            let r = Rect::from_center_size(c, sz * s);
            p.image(
                tex.id(),
                r,
                Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)),
                Color32::from_white_alpha((look.af * 255.0) as u8),
            );
        }
        _ => {
            let page = Rect::from_center_size(c, Vec2::new(36.0, 46.0));
            p.rect_filled(page, Rounding::same(5.0), look.fade(Color32::from_gray(58)));
            p.rect_stroke(
                page,
                Rounding::same(5.0),
                Stroke::new(1.0_f32, look.fade(Color32::from_gray(105))),
            );
            let label = if item.kind == Kind::Image {
                "IMG".to_string()
            } else if item.ext.is_empty() {
                "FILE".to_string()
            } else {
                item.ext.to_uppercase().chars().take(4).collect()
            };
            p.text(
                Pos2::new(c.x, c.y + 4.0),
                Align2::CENTER_CENTER,
                label,
                FontId::proportional(11.0),
                look.fade(ext_color(&item.ext)),
            );
        }
    }
}

/// True while the left mouse button is held (e.g. dragging a file) and the cursor
/// is near the top-center of the screen. Lets the notch open *before* the drag
/// reaches the tiny collapsed window, so there is a drop target to hit.
#[cfg(windows)]
fn drag_in_hotzone(ctx: &egui::Context) -> bool {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    unsafe {
        if GetAsyncKeyState(VK_LBUTTON.0 as i32) >= 0 {
            return false;
        }
        let mut pt = POINT::default();
        if GetCursorPos(&mut pt).is_err() {
            return false;
        }
        let ppp = ctx.pixels_per_point();
        let mw = ctx
            .input(|i| i.viewport().monitor_size)
            .map(|m| m.x)
            .unwrap_or(1920.0)
            * ppp;
        (pt.y as f32) < 70.0 * ppp && ((pt.x as f32) - mw / 2.0).abs() < 300.0 * ppp
    }
}

#[cfg(not(windows))]
fn drag_in_hotzone(_ctx: &egui::Context) -> bool {
    false
}

// ---------------------------------------------------------------------------
// drawing
// ---------------------------------------------------------------------------

impl Notch {
    fn draw_compact(
        &self,
        ui: &egui::Ui,
        p: &Painter,
        rect: Rect,
        np: &NowPlaying,
        now: &DateTime<Local>,
        time: f32,
    ) {
        let a = ((0.5 - self.t) * 2.0 * 255.0) as u8;
        let cy = rect.center().y;
        if let Some(tex) = &self.art_tex {
            let s = 22.0;
            egui::Image::from_texture(SizedTexture::new(tex.id(), Vec2::splat(s)))
                .rounding(5.0)
                .tint(Color32::from_white_alpha(a))
                .paint_at(
                    ui,
                    Rect::from_min_size(
                        Pos2::new(rect.left() + 14.0, cy - s / 2.0),
                        Vec2::splat(s),
                    ),
                );
        }
        if self.focus.running {
            let col = if self.focus.phase == Phase::Focus {
                ORANGE
            } else {
                GREEN
            };
            p.text(
                Pos2::new(rect.right() - 16.0, cy),
                Align2::RIGHT_CENTER,
                fmt_mmss(self.focus.remaining()),
                FontId::proportional(13.0),
                col.gamma_multiply(a as f32 / 255.0),
            );
        } else if np.playing {
            // little equalizer bars
            for i in 0..4 {
                let h = 4.0
                    + 12.0 * (0.5 + 0.5 * (time * (6.0 + i as f32 * 1.7) + i as f32 * 1.3).sin());
                let x = rect.right() - 20.0 - (3 - i) as f32 * 6.0;
                p.rect_filled(
                    Rect::from_center_size(Pos2::new(x, cy), Vec2::new(3.0, h)),
                    Rounding::same(1.5),
                    Color32::from_white_alpha(a),
                );
            }
        } else {
            p.text(
                Pos2::new(rect.right() - 16.0, cy),
                Align2::RIGHT_CENTER,
                now.format("%-I:%M %p").to_string(),
                FontId::proportional(13.0),
                Color32::from_white_alpha(a),
            );
        }
    }

    /// Tabs on the left, date/time in the middle, per-tab buttons on the right.
    fn draw_header(
        &mut self,
        ctx: &egui::Context,
        ui: &egui::Ui,
        p: &Painter,
        look: &Look,
        now: &DateTime<Local>,
    ) {
        let (ox, top) = (look.ox, look.top);

        for (label, tab, x, w) in [
            ("Nook", Tab::Nook, 14.0, 56.0),
            ("Tray", Tab::Tray, 74.0, 52.0),
            ("Focus", Tab::Focus, 130.0, 58.0),
            ("Water", Tab::Water, 192.0, 58.0),
            ("To-do", Tab::Todo, 254.0, 58.0),
        ] {
            let tr = Rect::from_min_size(Pos2::new(ox + x, top + 10.0), Vec2::new(w, 28.0));
            let resp = ui.interact(tr, egui::Id::new(("tab", label)), Sense::click());
            if resp.clicked() {
                self.tab = tab;
            }
            let sel = self.tab == tab;
            if sel {
                p.rect_filled(tr, Rounding::same(14.0), look.fade(Color32::from_gray(48)));
            }
            p.text(
                tr.center(),
                Align2::CENTER_CENTER,
                label,
                FontId::proportional(13.0),
                if sel { look.white } else { look.grey },
            );
        }

        if self.tab == Tab::Nook && !self.show_lyrics {
            p.text(
                Pos2::new(ox + 400.0, top + 24.0),
                Align2::CENTER_CENTER,
                now.format("%a %-d %b  %-I:%M %p").to_string(),
                FontId::proportional(13.0),
                look.grey,
            );
        }

        match self.tab {
            Tab::Nook => {
                let r = Rect::from_min_size(
                    Pos2::new(ox + EXPANDED.x - 22.0 - 84.0, top + 10.0),
                    Vec2::new(84.0, 28.0),
                );
                if pill(ui, p, look, "lyrics_btn", r, "Lyrics", self.show_lyrics) {
                    self.show_lyrics = !self.show_lyrics;
                    if self.show_lyrics {
                        self.media.retry_lyrics();
                    }
                }
                if self.show_lyrics {
                    // Sync tuning: "+" shows lyrics earlier, "−" later.
                    let minus = Rect::from_min_size(
                        Pos2::new(ox + 372.0, top + 10.0),
                        Vec2::new(22.0, 28.0),
                    );
                    let plus = Rect::from_min_size(
                        Pos2::new(ox + 458.0, top + 10.0),
                        Vec2::new(22.0, 28.0),
                    );
                    let mut off = self.lyric_offset_ms;
                    if pill(ui, p, look, "sync_minus", minus, "−", false) {
                        off -= 250;
                    }
                    if pill(ui, p, look, "sync_plus", plus, "+", false) {
                        off += 250;
                    }
                    p.text(
                        Pos2::new(ox + 426.0, top + 24.0),
                        Align2::CENTER_CENTER,
                        format!("sync {:+.2}s", self.lyric_offset_ms as f32 / 1000.0),
                        FontId::proportional(11.0),
                        look.dim,
                    );
                    let off = off.clamp(-3000, 3000);
                    if off != self.lyric_offset_ms {
                        self.lyric_offset_ms = off;
                        save_offset(off);
                    }
                }
            }
            Tab::Focus | Tab::Water => {}
            Tab::Todo => {
                if self.tasks.iter().any(|t| t.done) {
                    let r = Rect::from_min_size(
                        Pos2::new(ox + EXPANDED.x - 22.0 - 96.0, top + 10.0),
                        Vec2::new(96.0, 28.0),
                    );
                    if pill(ui, p, look, "clear_done", r, "Clear done", false) {
                        self.tasks.retain(|t| !t.done);
                        store::save_tasks(&self.tasks);
                    }
                }
            }
            Tab::Tray => {
                let add_r = Rect::from_min_size(
                    Pos2::new(ox + EXPANDED.x - 22.0 - 100.0, top + 10.0),
                    Vec2::new(100.0, 28.0),
                );
                if pill(ui, p, look, "add_files", add_r, "+ Add files", false) {
                    self.open_picker(ctx);
                }
                if !self.tray.is_empty() {
                    let clr_r = Rect::from_min_size(
                        Pos2::new(add_r.left() - 8.0 - 62.0, top + 10.0),
                        Vec2::new(62.0, 28.0),
                    );
                    if pill(ui, p, look, "clear_tray", clr_r, "Clear", false) {
                        self.tray.clear();
                        self.tray_page = 0;
                    }
                }
            }
        }
    }

    fn draw_nook(
        &mut self,
        ui: &egui::Ui,
        p: &Painter,
        look: &Look,
        np: &NowPlaying,
        now: &DateTime<Local>,
    ) {
        let (ox, top) = (look.ox, look.top);

        // ---- album art ----
        let art_rect = Rect::from_min_size(Pos2::new(ox + 22.0, top + 50.0), Vec2::splat(84.0));
        if let Some(tex) = &self.art_tex {
            egui::Image::from_texture(SizedTexture::new(tex.id(), Vec2::splat(84.0)))
                .rounding(12.0)
                .tint(Color32::from_white_alpha((look.af * 255.0) as u8))
                .paint_at(ui, art_rect);
        } else {
            p.rect_filled(
                art_rect,
                Rounding::same(12.0),
                look.fade(Color32::from_gray(34)),
            );
            p.text(
                art_rect.center(),
                Align2::CENTER_CENTER,
                "♪",
                FontId::proportional(34.0),
                look.grey,
            );
        }

        // ---- track text ----
        let tx = ox + 120.0;
        let tw = 170.0;
        let mut y = top + 48.0;
        let title = if np.title.is_empty() {
            "Nothing playing"
        } else {
            np.title.as_str()
        };
        y += text_block(p, Pos2::new(tx, y), title, 17.0, look.white, tw, 2) + 2.0;
        if !np.artist.is_empty() {
            y += text_block(p, Pos2::new(tx, y), &np.artist, 14.0, look.grey, tw, 1) + 1.0;
        }
        if !np.album.is_empty() {
            text_block(p, Pos2::new(tx, y), &np.album, 14.0, look.dim, tw, 1);
        }

        // ---- progress bar:  1:23 ━━━━━━──── -2:10 ----
        let pos = np.live_pos_ms();
        let by = top + 146.0;
        let (x0, x1) = (ox + 22.0, ox + 284.0);
        if np.dur_ms > 0 {
            p.text(
                Pos2::new(x0, by),
                Align2::LEFT_CENTER,
                fmt_time(pos),
                FontId::proportional(11.0),
                look.grey,
            );
            p.text(
                Pos2::new(x1, by),
                Align2::RIGHT_CENTER,
                format!("-{}", fmt_time(np.dur_ms.saturating_sub(pos))),
                FontId::proportional(11.0),
                look.grey,
            );
        }
        let track = Rect::from_min_max(
            Pos2::new(x0 + 38.0, by - 2.0),
            Pos2::new(x1 - 42.0, by + 2.0),
        );
        p.rect_filled(
            track,
            Rounding::same(2.0),
            look.fade(Color32::from_gray(58)),
        );
        if np.dur_ms > 0 {
            let hit = track.expand2(Vec2::new(4.0, 8.0));
            let resp = ui.interact(hit, egui::Id::new("seek_bar"), Sense::click());
            if resp.clicked() {
                if let Some(pt) = resp.interact_pointer_pos() {
                    let f = ((pt.x - track.min.x) / track.width()).clamp(0.0, 1.0);
                    self.media
                        .send(MediaCmd::Seek((f * np.dur_ms as f32) as u64));
                }
            }
            let frac = (pos as f32 / np.dur_ms as f32).clamp(0.0, 1.0);
            let fill = Rect::from_min_max(
                track.min,
                Pos2::new(track.min.x + track.width() * frac, track.max.y),
            );
            p.rect_filled(fill, Rounding::same(2.0), look.white);
        }

        // ---- controls ----
        let cy = top + 168.0;
        if icon_btn(ui, "prev", Pos2::new(ox + 109.0, cy), "⏮", 18.0, look.white) {
            self.media.send(MediaCmd::Prev);
        }
        if icon_btn(
            ui,
            "play",
            Pos2::new(ox + 153.0, cy),
            if np.playing { "⏸" } else { "▶" },
            22.0,
            look.white,
        ) {
            self.media.send(MediaCmd::PlayPause);
        }
        if icon_btn(ui, "next", Pos2::new(ox + 197.0, cy), "⏭", 18.0, look.white) {
            self.media.send(MediaCmd::Next);
        }

        // ---- divider ----
        p.line_segment(
            [
                Pos2::new(ox + 302.0, top + 52.0),
                Pos2::new(ox + 302.0, top + 172.0),
            ],
            Stroke::new(1.0_f32, look.fade(Color32::from_gray(40))),
        );

        // ---- right half: lyrics or calendar ----
        let v = self.lyrics_vis;
        if v < 0.999 {
            self.draw_calendar(p, &look.scaled(1.0 - v), now);
        }
        if v > 0.001 {
            self.draw_lyrics(ui, p, &look.scaled(v), np);
        }
    }

    fn draw_calendar(&self, p: &Painter, look: &Look, now: &DateTime<Local>) {
        let (ox, top) = (look.ox, look.top);
        let today = now.date_naive();
        p.text(
            Pos2::new(ox + 320.0, top + 82.0),
            Align2::LEFT_CENTER,
            today.format("%b").to_string(),
            FontId::proportional(32.0),
            look.white,
        );
        for (i, off) in (-2..=2i64).enumerate() {
            let d = today + chrono::Duration::days(off);
            let cx = ox + 414.0 + i as f32 * 38.0;
            let is_today = off == 0;
            let col = if is_today {
                look.fade(BLUE)
            } else if d.weekday() == Weekday::Sun {
                look.fade(RED).gamma_multiply(0.75)
            } else {
                look.grey
            };
            p.text(
                Pos2::new(cx, top + 64.0),
                Align2::CENTER_CENTER,
                d.format("%a").to_string().to_uppercase(),
                FontId::proportional(10.0),
                if is_today { col } else { look.dim },
            );
            p.text(
                Pos2::new(cx, top + 86.0),
                Align2::CENTER_CENTER,
                d.format("%d").to_string(),
                FontId::proportional(if is_today { 22.0 } else { 18.0 }),
                col,
            );
        }

        // agenda placeholder
        let icon_c = Pos2::new(ox + 450.0, top + 128.0);
        p.rect_stroke(
            Rect::from_center_size(icon_c, Vec2::new(20.0, 16.0)),
            Rounding::same(4.0),
            Stroke::new(1.5_f32, look.dim),
        );
        p.text(
            Pos2::new(ox + 450.0, top + 154.0),
            Align2::CENTER_CENTER,
            "Nothing for today",
            FontId::proportional(14.0),
            look.grey,
        );
    }

    fn draw_lyrics(&self, ui: &egui::Ui, p: &Painter, look: &Look, np: &NowPlaying) {
        let (ox, top) = (look.ox, look.top);
        let region = Rect::from_min_max(
            Pos2::new(ox + 320.0, top + 50.0),
            Pos2::new(ox + EXPANDED.x - 24.0, top + 176.0),
        );
        let pc = p.with_clip_rect(region);
        let message = |m: &str| {
            pc.text(
                region.center(),
                Align2::CENTER_CENTER,
                m,
                FontId::proportional(14.0),
                look.grey,
            );
        };

        let l = match &self.lyrics_status {
            LyricsStatus::Idle => return message("Nothing playing"),
            LyricsStatus::Loading => return message("Searching lyrics…"),
            LyricsStatus::NotFound => return message("No lyrics found for this song"),
            LyricsStatus::Failed => return message("Couldn't reach the lyrics service"),
            LyricsStatus::Found(l) => l,
        };
        if l.instrumental || l.lines.is_empty() {
            return message("Instrumental ♪");
        }

        // Synced lyrics get a virtual "♪" line (index -1) for the intro.
        let first: isize = if l.synced { -1 } else { 0 };
        let last = l.lines.len() as isize - 1;
        let text_for = |i: isize| -> String {
            if i == -1 {
                "♪".to_string()
            } else {
                l.text_at(i).unwrap_or("").to_string()
            }
        };

        let pos = self.lyric_pos.clamp(first as f32, last as f32);
        let base = pos.floor() as isize;
        let frac = pos - base as f32;
        let lo = (base - 4).max(first);
        let hi = (base + 5).min(last);

        const GAP: f32 = 10.0;
        let w = region.width();
        let mut galleys: Vec<(isize, Arc<Galley>)> = Vec::new();
        for i in lo..=hi {
            galleys.push((
                i,
                layout(&pc, &text_for(i), 16.0, Color32::PLACEHOLDER, w, 2),
            ));
        }
        let height = |i: isize| -> f32 {
            galleys
                .iter()
                .find(|(j, _)| *j == i)
                .map_or(0.0, |(_, g)| g.size().y)
        };
        // Vertical centre of line `i`, measured from the centre of line `base`.
        let center = |i: isize| -> f32 {
            let mut c = 0.0;
            if i > base {
                for j in base..i {
                    c += height(j) / 2.0 + GAP + height(j + 1) / 2.0;
                }
            } else {
                for j in i..base {
                    c -= height(j) / 2.0 + GAP + height(j + 1) / 2.0;
                }
            }
            c
        };
        let focus = if base < last {
            lerp(0.0, center(base + 1), frac)
        } else {
            0.0
        };

        let smooth = |x: f32| x * x * (3.0 - 2.0 * x);
        let can_seek = l.synced && np.dur_ms > 0;
        for (i, g) in galleys.iter() {
            let i = *i;
            let yc = region.center().y + center(i) - focus;
            let d = (i as f32 - pos).abs();
            let mut b = if d < 1.0 {
                0.34 + 0.66 * smooth(1.0 - d)
            } else {
                0.34 * (1.0 - 0.65 * ((d - 1.0) / 2.0).clamp(0.0, 1.0))
            };
            // fade out towards the top / bottom edge of the panel
            let edge =
                ((region.height() / 2.0 - (yc - region.center().y).abs()) / 26.0).clamp(0.0, 1.0);
            b *= edge;
            if b <= 0.01 {
                continue;
            }
            // click a line to jump there
            let h = g.size().y;
            let line_rect =
                Rect::from_min_size(Pos2::new(region.left(), yc - h / 2.0), Vec2::new(w, h))
                    .intersect(region);
            if can_seek && i >= 0 && line_rect.width() > 1.0 && line_rect.height() > 1.0 {
                let resp = ui.interact(line_rect, egui::Id::new(("lyric_line", i)), Sense::click());
                if resp.hovered() {
                    b = (b + 0.25).min(1.0);
                }
                if resp.clicked() {
                    if let Some((t, _)) = l.lines.get(i as usize) {
                        self.media.send(MediaCmd::Seek(*t));
                    }
                }
            }
            let col = look.fade(Color32::WHITE.gamma_multiply(b));
            pc.galley(
                Pos2::new(region.left(), yc - g.size().y / 2.0),
                g.clone(),
                col,
            );
        }

        if !l.synced {
            pc.text(
                Pos2::new(region.right(), region.bottom() - 2.0),
                Align2::RIGHT_BOTTOM,
                "not time-synced",
                FontId::proportional(10.0),
                look.dim,
            );
        }
    }

    fn show_toast(
        &mut self,
        kind: ToastKind,
        title: &str,
        sub: &str,
        level: Option<f32>,
        secs: f32,
    ) {
        self.toast = Some(Toast {
            kind,
            title: title.to_string(),
            sub: sub.to_string(),
            level,
            until: Instant::now() + Duration::from_secs_f32(secs),
        });
    }

    fn on_sys_event(&mut self, e: SysEvent) {
        match e {
            SysEvent::Volume { level, muted } => {
                let kind = if muted || level <= 0.001 {
                    ToastKind::Muted
                } else {
                    ToastKind::Volume
                };
                let title = if muted { "Muted" } else { "Volume" };
                let lv = if muted { 0.0 } else { level };
                self.show_toast(kind, title, "", Some(lv), 1.6);
            }
            SysEvent::Brightness(v) => {
                self.show_toast(ToastKind::Brightness, "Brightness", "", Some(v), 1.6)
            }
            SysEvent::CapsLock(on) => {
                let t = if on { "Caps Lock On" } else { "Caps Lock Off" };
                self.show_toast(ToastKind::Caps, t, "", None, 1.8);
            }
            SysEvent::Lang(name) => {
                self.show_toast(ToastKind::Lang, &name, "Keyboard language", None, 2.0)
            }
            SysEvent::Power { plugged, percent } => {
                let t = if plugged { "Charging" } else { "On battery" };
                self.show_toast(
                    ToastKind::Charger,
                    t,
                    &format!("{percent}%"),
                    Some(percent as f32 / 100.0),
                    3.0,
                );
            }
            SysEvent::Wifi { connected, name } => {
                if connected {
                    let sub = if name.is_empty() {
                        "Online"
                    } else {
                        name.as_str()
                    };
                    self.show_toast(ToastKind::Wifi, "Connected", sub, None, 3.0);
                } else {
                    self.show_toast(ToastKind::Wifi, "No internet", "Connection lost", None, 3.0);
                }
            }
            SysEvent::Audio(name) => {
                self.show_toast(ToastKind::Audio, "Audio output", &name, None, 3.0)
            }
        }
    }

    fn draw_toast(&self, p: &Painter, rect: Rect, alpha: f32) {
        let Some(t) = &self.toast else { return };
        let a = (alpha.clamp(0.0, 1.0) * 255.0) as u8;
        let white = Color32::from_white_alpha(a);
        let grey = Color32::from_gray(165).gamma_multiply(alpha);
        let cy = rect.center().y;
        let accent = match t.kind {
            ToastKind::Focus => ORANGE,
            ToastKind::Water => BLUE,
            ToastKind::Charger => GREEN,
            ToastKind::Todo => GREEN,
            _ => Color32::WHITE,
        }
        .gamma_multiply(alpha);
        let on = match t.kind {
            ToastKind::Caps => t.title.ends_with("On"),
            ToastKind::Wifi => t.title == "Connected",
            _ => true,
        };
        draw_icon(p, t.kind, Pos2::new(rect.left() + 28.0, cy), accent, on);
        let tx = rect.left() + 52.0;
        let bar_w = if t.level.is_some() { 110.0 } else { 0.0 };
        let tw = rect.width() - 52.0 - 20.0 - bar_w - if bar_w > 0.0 { 10.0 } else { 0.0 };
        if t.sub.is_empty() {
            let g = layout(p, &t.title, 14.0, white, tw, 1);
            p.galley(Pos2::new(tx, cy - g.size().y / 2.0), g, white);
        } else {
            let g = layout(p, &t.title, 14.0, white, tw, 1);
            p.galley(Pos2::new(tx, cy - 16.0), g, white);
            let g = layout(p, &t.sub, 11.5, grey, tw, 1);
            p.galley(Pos2::new(tx, cy + 2.0), g, grey);
        }
        if let Some(lv) = t.level {
            let track = Rect::from_min_max(
                Pos2::new(rect.right() - 20.0 - bar_w, cy - 2.5),
                Pos2::new(rect.right() - 20.0, cy + 2.5),
            );
            p.rect_filled(
                track,
                Rounding::same(2.5),
                Color32::from_gray(62).gamma_multiply(alpha),
            );
            let fill = Rect::from_min_max(
                track.min,
                Pos2::new(
                    track.min.x + track.width() * lv.clamp(0.0, 1.0),
                    track.max.y,
                ),
            );
            p.rect_filled(fill, Rounding::same(2.5), white);
        }
    }

    fn draw_focus(&mut self, ui: &egui::Ui, p: &Painter, look: &Look) {
        let (ox, top) = (look.ox, look.top);
        let accent = if self.focus.phase == Phase::Focus {
            ORANGE
        } else {
            GREEN
        };
        let c = Pos2::new(ox + 110.0, top + 114.0);
        let r = 52.0;
        polyline(
            p,
            &arc_points(c, r, 0.0, std::f32::consts::TAU),
            Stroke::new(6.0_f32, look.fade(Color32::from_gray(40))),
        );
        let prog = self.focus.progress();
        if prog > 0.002 {
            let a0 = -std::f32::consts::FRAC_PI_2;
            polyline(
                p,
                &arc_points(c, r, a0, a0 + prog * std::f32::consts::TAU),
                Stroke::new(6.0_f32, look.fade(accent)),
            );
        }
        p.text(
            Pos2::new(c.x, c.y - 4.0),
            Align2::CENTER_CENTER,
            fmt_mmss(self.focus.remaining()),
            FontId::proportional(28.0),
            look.white,
        );
        p.text(
            Pos2::new(c.x, c.y + 20.0),
            Align2::CENTER_CENTER,
            if self.focus.phase == Phase::Focus {
                "FOCUS"
            } else {
                "BREAK"
            },
            FontId::proportional(10.0),
            look.fade(accent),
        );

        let x = ox + 230.0;
        let r1 = Rect::from_min_size(Pos2::new(x, top + 56.0), Vec2::new(84.0, 28.0));
        let r2 = Rect::from_min_size(Pos2::new(x + 92.0, top + 56.0), Vec2::new(84.0, 28.0));
        if pill(
            ui,
            p,
            look,
            "ph_focus",
            r1,
            "Focus",
            self.focus.phase == Phase::Focus,
        ) {
            self.focus.set_phase(Phase::Focus);
        }
        if pill(
            ui,
            p,
            look,
            "ph_break",
            r2,
            "Break",
            self.focus.phase == Phase::Break,
        ) {
            self.focus.set_phase(Phase::Break);
        }
        let mins = if self.focus.phase == Phase::Focus {
            self.focus.focus_min
        } else {
            self.focus.break_min
        };
        let d = stepper(
            ui,
            p,
            look,
            "fmin",
            x,
            top + 96.0,
            "Length",
            &format!("{mins} min"),
        );
        if d != 0 {
            self.focus
                .adjust_minutes(d * if mins >= 10 { 5 } else { 1 });
        }
        let go = Rect::from_min_size(Pos2::new(x, top + 132.0), Vec2::new(120.0, 30.0));
        let label = if self.focus.running { "Pause" } else { "Start" };
        if pill(ui, p, look, "focus_go", go, label, !self.focus.running) {
            self.focus.start_pause();
        }
        let rs = Rect::from_min_size(Pos2::new(x + 128.0, top + 132.0), Vec2::new(72.0, 30.0));
        if pill(ui, p, look, "focus_reset", rs, "Reset", false) {
            self.focus.reset();
        }
        p.text(
            Pos2::new(ox + 560.0, top + 114.0),
            Align2::RIGHT_CENTER,
            format!("{} done", self.focus.sessions),
            FontId::proportional(14.0),
            look.grey,
        );
    }

    fn draw_water(&mut self, ui: &egui::Ui, p: &Painter, look: &Look) {
        let (ox, top) = (look.ox, look.top);
        let c = Pos2::new(ox + 110.0, top + 114.0);
        let r = 52.0;
        polyline(
            p,
            &arc_points(c, r, 0.0, std::f32::consts::TAU),
            Stroke::new(6.0_f32, look.fade(Color32::from_gray(40))),
        );
        let f = self.water.fraction();
        if f > 0.002 {
            let a0 = -std::f32::consts::FRAC_PI_2;
            polyline(
                p,
                &arc_points(c, r, a0, a0 + f * std::f32::consts::TAU),
                Stroke::new(6.0_f32, look.fade(BLUE)),
            );
        }
        p.text(
            Pos2::new(c.x, c.y - 6.0),
            Align2::CENTER_CENTER,
            format!("{}", self.water.drank_ml),
            FontId::proportional(26.0),
            look.white,
        );
        p.text(
            Pos2::new(c.x, c.y + 16.0),
            Align2::CENTER_CENTER,
            format!("of {} ml", self.water.goal_ml),
            FontId::proportional(11.0),
            look.grey,
        );

        let x = ox + 230.0;
        let mut changed = false;
        let drink = Rect::from_min_size(Pos2::new(x, top + 56.0), Vec2::new(130.0, 28.0));
        if pill(
            ui,
            p,
            look,
            "water_drink",
            drink,
            &format!("+ {} ml", self.water.bottle_ml),
            true,
        ) {
            self.water.drink();
            changed = true;
        }
        let undo = Rect::from_min_size(Pos2::new(x + 138.0, top + 56.0), Vec2::new(64.0, 28.0));
        if pill(ui, p, look, "water_undo", undo, "Undo", false) {
            self.water.undo();
            changed = true;
        }

        let y1 = top + 100.0;
        let y2 = top + 126.0;
        let y3 = top + 152.0;
        let d = stepper(
            ui,
            p,
            look,
            "w_goal",
            x,
            y1,
            "Goal",
            &format!("{} ml", self.water.goal_ml),
        );
        if d != 0 {
            self.water.goal_ml = (self.water.goal_ml as i32 + d * 250).clamp(500, 6000) as u32;
            changed = true;
        }
        let d = stepper(
            ui,
            p,
            look,
            "w_bottle",
            x,
            y2,
            "Bottle",
            &format!("{} ml", self.water.bottle_ml),
        );
        if d != 0 {
            self.water.bottle_ml = (self.water.bottle_ml as i32 + d * 50).clamp(50, 2000) as u32;
            changed = true;
        }
        let d = stepper(
            ui,
            p,
            look,
            "w_every",
            x,
            y3,
            "Every",
            &format!("{} min", self.water.interval_min),
        );
        if d != 0 {
            self.water.interval_min =
                (self.water.interval_min as i32 + d * 15).clamp(15, 240) as u32;
            changed = true;
        }
        let x2 = x + 190.0;
        let d = stepper(
            ui,
            p,
            look,
            "w_from",
            x2,
            y1,
            "From",
            &format!("{}:00", self.water.from_hour),
        );
        if d != 0 {
            self.water.from_hour =
                (self.water.from_hour as i32 + d).clamp(0, self.water.to_hour as i32 - 1) as u32;
            changed = true;
        }
        let d = stepper(
            ui,
            p,
            look,
            "w_to",
            x2,
            y2,
            "Until",
            &format!("{}:00", self.water.to_hour),
        );
        if d != 0 {
            self.water.to_hour =
                (self.water.to_hour as i32 + d).clamp(self.water.from_hour as i32 + 1, 24) as u32;
            changed = true;
        }
        if changed {
            store::save_water(&self.water);
        }
    }

    fn add_task(&mut self, now: &DateTime<Local>) {
        let (text, hm) = parse_task_input(&self.todo_input);
        if text.is_empty() {
            return;
        }
        let due = hm.and_then(|(h, m)| {
            let mut dt = now.date_naive().and_hms_opt(h, m, 0)?;
            if dt <= now.naive_local() {
                dt += chrono::Duration::days(1);
            }
            Some(dt.format("%Y-%m-%dT%H:%M").to_string())
        });
        self.tasks.push(Task {
            text,
            due,
            done: false,
            notified: false,
        });
        self.todo_input.clear();
        store::save_tasks(&self.tasks);
    }

    fn draw_todo(&mut self, ui: &mut egui::Ui, p: &Painter, look: &Look, now: &DateTime<Local>) {
        let (ox, top) = (look.ox, look.top);
        let now_s = now.format("%Y-%m-%dT%H:%M").to_string();

        // open tasks first, then finished ones
        let mut order: Vec<usize> = (0..self.tasks.len())
            .filter(|&i| !self.tasks[i].done)
            .collect();
        order.extend((0..self.tasks.len()).filter(|&i| self.tasks[i].done));

        const ROWS: usize = 3;
        let mut toggle: Option<usize> = None;
        let mut remove: Option<usize> = None;
        if order.is_empty() {
            p.text(
                Pos2::new(ox + 300.0, top + 90.0),
                Align2::CENTER_CENTER,
                "No tasks yet. Add one below, e.g. \"call mom 14:30\"",
                FontId::proportional(13.0),
                look.dim,
            );
        }
        for (row, &i) in order.iter().take(ROWS).enumerate() {
            let t = &self.tasks[i];
            let cy = top + 64.0 + row as f32 * 28.0;
            let chk = Rect::from_center_size(Pos2::new(ox + 34.0, cy), Vec2::splat(22.0));
            let resp = ui.interact(chk, egui::Id::new(("todo_chk", i)), Sense::click());
            let ring = if t.done { look.fade(GREEN) } else { look.grey };
            p.circle_stroke(chk.center(), 8.0, Stroke::new(1.6_f32, ring));
            if t.done {
                p.circle_filled(chk.center(), 5.0, look.fade(GREEN));
            }
            if resp.clicked() {
                toggle = Some(i);
            }
            let col = if t.done { look.dim } else { look.white };
            let g = layout(p, &t.text, 14.0, col, 400.0, 1);
            let w = g.size().x;
            p.galley(Pos2::new(ox + 54.0, cy - g.size().y / 2.0), g, col);
            if t.done {
                p.line_segment(
                    [Pos2::new(ox + 54.0, cy), Pos2::new(ox + 54.0 + w, cy)],
                    Stroke::new(1.2_f32, look.dim),
                );
            }
            if let Some(due) = &t.due {
                let overdue = !t.done && due.as_str() < now_s.as_str();
                let col = if overdue {
                    look.fade(RED)
                } else {
                    look.fade(BLUE)
                };
                let hm = due.get(11..16).unwrap_or("");
                let day = if due.get(0..10) == Some(&now_s[0..10]) {
                    ""
                } else {
                    " tmrw"
                };
                p.text(
                    Pos2::new(ox + 534.0, cy),
                    Align2::RIGHT_CENTER,
                    format!("{hm}{day}"),
                    FontId::proportional(12.0),
                    col,
                );
            }
            let xr = Rect::from_center_size(Pos2::new(ox + 566.0, cy), Vec2::splat(22.0));
            let resp = ui.interact(xr, egui::Id::new(("todo_del", i)), Sense::click());
            p.text(
                xr.center(),
                Align2::CENTER_CENTER,
                "×",
                FontId::proportional(16.0),
                if resp.hovered() { look.white } else { look.dim },
            );
            if resp.clicked() {
                remove = Some(i);
            }
        }
        if order.len() > ROWS {
            p.text(
                Pos2::new(ox + 54.0, top + 64.0 + ROWS as f32 * 28.0 - 4.0),
                Align2::LEFT_CENTER,
                format!("+{} more", order.len() - ROWS),
                FontId::proportional(11.0),
                look.dim,
            );
        }
        if let Some(i) = toggle {
            self.tasks[i].done = !self.tasks[i].done;
            store::save_tasks(&self.tasks);
        }
        if let Some(i) = remove {
            self.tasks.remove(i);
            store::save_tasks(&self.tasks);
        }

        // ---- input row ----
        let bg = Rect::from_min_max(
            Pos2::new(ox + 22.0, top + 148.0),
            Pos2::new(ox + 500.0, top + 176.0),
        );
        p.rect_filled(bg, Rounding::same(14.0), look.fade(Color32::from_gray(30)));
        let field = bg.shrink2(Vec2::new(12.0, 4.0));
        let resp = ui.put(
            field,
            egui::TextEdit::singleline(&mut self.todo_input)
                .frame(false)
                .hint_text("New task…  add a time at the end, like 14:30 or 9pm")
                .text_color(look.white)
                .font(FontId::proportional(14.0))
                .desired_width(field.width()),
        );
        let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let add = Rect::from_min_max(
            Pos2::new(ox + 508.0, top + 148.0),
            Pos2::new(ox + 578.0, top + 176.0),
        );
        let clicked = pill(ui, p, look, "todo_add", add, "Add", true);
        if enter || clicked {
            self.add_task(now);
            resp.request_focus();
        }
    }

    fn draw_tray(&mut self, ui: &egui::Ui, p: &Painter, look: &Look, dragging: bool) {
        let (ox, top) = (look.ox, look.top);
        let area = Rect::from_min_max(
            Pos2::new(ox + 14.0, top + 48.0),
            Pos2::new(ox + EXPANDED.x - 14.0, top + 176.0),
        );

        // Empty, or a file is being dragged over: show the dashed drop zone.
        if dragging || self.tray.is_empty() {
            let zone = area.shrink2(Vec2::new(8.0, 2.0));
            let accent = if dragging {
                look.fade(Color32::from_rgb(130, 185, 255))
            } else {
                look.dim
            };
            if dragging {
                p.rect_filled(
                    zone,
                    Rounding::same(18.0),
                    look.fade(Color32::from_rgb(18, 34, 60)),
                );
            }
            dashed_path(
                p,
                &rounded_path(zone, 18.0),
                Stroke::new(1.5_f32, accent),
                6.0,
                5.0,
            );

            // inbox icon: arrow pointing into a tray
            let c = Pos2::new(zone.center().x, zone.center().y - 14.0);
            let s = Stroke::new(1.8_f32, accent);
            p.line_segment([Pos2::new(c.x, c.y - 16.0), Pos2::new(c.x, c.y - 2.0)], s);
            polyline(
                p,
                &[
                    Pos2::new(c.x - 5.0, c.y - 7.0),
                    Pos2::new(c.x, c.y - 2.0),
                    Pos2::new(c.x + 5.0, c.y - 7.0),
                ],
                s,
            );
            polyline(
                p,
                &[
                    Pos2::new(c.x - 15.0, c.y + 2.0),
                    Pos2::new(c.x - 15.0, c.y + 12.0),
                    Pos2::new(c.x + 15.0, c.y + 12.0),
                    Pos2::new(c.x + 15.0, c.y + 2.0),
                    Pos2::new(c.x + 7.0, c.y + 2.0),
                    Pos2::new(c.x + 5.0, c.y + 6.0),
                    Pos2::new(c.x - 5.0, c.y + 6.0),
                    Pos2::new(c.x - 7.0, c.y + 2.0),
                    Pos2::new(c.x - 15.0, c.y + 2.0),
                ],
                s,
            );
            p.text(
                Pos2::new(zone.center().x, zone.center().y + 26.0),
                Align2::CENTER_CENTER,
                if dragging {
                    "Release to add"
                } else {
                    "Drop files here, or tap “+ Add files”"
                },
                FontId::proportional(14.0),
                if dragging { look.white } else { look.grey },
            );
            return;
        }

        // ---- tiles ----
        let n = self.tray.len();
        let pages = (n + TILES_PER_PAGE - 1) / TILES_PER_PAGE;
        if self.tray_page >= pages {
            self.tray_page = pages - 1;
        }
        let start = self.tray_page * TILES_PER_PAGE;
        let end = (start + TILES_PER_PAGE).min(n);

        let mut remove: Option<usize> = None;
        let mut open: Option<usize> = None;
        for (slot, idx) in (start..end).enumerate() {
            let x = ox + 48.0 + slot as f32 * (92.0 + 11.0);
            let tile = Rect::from_min_size(Pos2::new(x, top + 54.0), Vec2::new(92.0, 108.0));
            let hov = ui.rect_contains_pointer(tile);
            let resp = ui.interact(tile, egui::Id::new(("tile", idx)), Sense::click());
            if hov {
                p.rect_filled(
                    tile,
                    Rounding::same(12.0),
                    look.fade(Color32::from_gray(34)),
                );
            }
            let item = &self.tray[idx];
            draw_file_icon(p, look, item, Pos2::new(tile.center().x, tile.top() + 34.0));
            text_centered(
                p,
                tile.center().x,
                tile.top() + 66.0,
                &item.name,
                13.0,
                look.white,
                84.0,
            );
            text_centered(
                p,
                tile.center().x,
                tile.top() + 84.0,
                &item.info,
                11.0,
                look.dim,
                84.0,
            );

            // remove (×) button, drawn only while hovering the tile
            let xr = Rect::from_center_size(
                Pos2::new(tile.right() - 10.0, tile.top() + 10.0),
                Vec2::splat(18.0),
            );
            let xresp = ui.interact(xr, egui::Id::new(("tile_x", idx)), Sense::click());
            if hov {
                let c = xr.center();
                p.circle_filled(
                    c,
                    8.0,
                    look.fade(Color32::from_gray(if xresp.hovered() { 96 } else { 66 })),
                );
                let s = Stroke::new(1.5_f32, look.white);
                p.line_segment(
                    [
                        Pos2::new(c.x - 3.0, c.y - 3.0),
                        Pos2::new(c.x + 3.0, c.y + 3.0),
                    ],
                    s,
                );
                p.line_segment(
                    [
                        Pos2::new(c.x - 3.0, c.y + 3.0),
                        Pos2::new(c.x + 3.0, c.y - 3.0),
                    ],
                    s,
                );
            }

            if xresp.clicked() || resp.secondary_clicked() {
                remove = Some(idx);
            } else if resp.clicked() {
                open = Some(idx);
            }
        }
        if let Some(i) = open {
            open_path(&self.tray[i].path);
        }
        if let Some(i) = remove {
            self.tray.remove(i);
        }

        // ---- paging arrows + dots ----
        if pages > 1 {
            let mut new_page = self.tray_page;
            for (dir, cx) in [(-1i32, ox + 26.0), (1i32, ox + EXPANDED.x - 26.0)] {
                let c = Pos2::new(cx, top + 108.0);
                let enabled =
                    (dir < 0 && self.tray_page > 0) || (dir > 0 && self.tray_page + 1 < pages);
                let r = Rect::from_center_size(c, Vec2::splat(26.0));
                let resp = ui.interact(r, egui::Id::new(("page", dir)), Sense::click());
                let g = if resp.hovered() && enabled { 72 } else { 48 };
                p.circle_filled(c, 11.0, look.fade(Color32::from_gray(g)));
                let col = if enabled { look.white } else { look.dim };
                let d = dir as f32;
                polyline(
                    p,
                    &[
                        Pos2::new(c.x - 2.0 * d, c.y - 5.0),
                        Pos2::new(c.x + 2.0 * d, c.y),
                        Pos2::new(c.x - 2.0 * d, c.y + 5.0),
                    ],
                    Stroke::new(1.8_f32, col),
                );
                if resp.clicked() && enabled {
                    new_page = (self.tray_page as i32 + dir) as usize;
                }
            }
            self.tray_page = new_page;

            let shown = pages.min(8);
            let total_w = (shown as f32 - 1.0) * 10.0;
            for i in 0..shown {
                let dx = ox + EXPANDED.x / 2.0 - total_w / 2.0 + i as f32 * 10.0;
                let dy = top + 172.0;
                if i == self.tray_page {
                    p.rect_filled(
                        Rect::from_center_size(Pos2::new(dx, dy), Vec2::new(12.0, 5.0)),
                        Rounding::same(2.5),
                        look.white,
                    );
                } else {
                    p.circle_filled(Pos2::new(dx, dy), 2.5, look.dim);
                }
            }
        }
    }
}

impl eframe::App for Notch {
    // Fully transparent window background; we paint the island ourselves.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // --- files chosen via the "+ Add files" dialog ---------------------
        let picked: Vec<PathBuf> = self
            .picked
            .lock()
            .map(|mut v| v.drain(..).collect::<Vec<PathBuf>>())
            .unwrap_or_default();
        for p in picked {
            self.add_path(ctx, p);
        }

        // --- files dropped onto the notch go to the Tray -----------------
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        if !dropped.is_empty() {
            self.tab = Tab::Tray;
            for p in dropped {
                self.add_path(ctx, p);
            }
            self.tray_page = self.tray.len().saturating_sub(1) / TILES_PER_PAGE;
        }

        // --- finished image thumbnails -> textures -----------------------
        let ready: Vec<(PathBuf, ColorImage)> = self
            .thumbs
            .lock()
            .map(|mut v| v.drain(..).collect::<Vec<(PathBuf, ColorImage)>>())
            .unwrap_or_default();
        for (path, img) in ready {
            if let Some(item) = self.tray.iter_mut().find(|i| i.path == path) {
                item.tex = Some(ctx.load_texture(
                    format!("thumb-{}", path.display()),
                    img,
                    TextureOptions::LINEAR,
                ));
            }
        }

        // --- hover -> target state -------------------------------------
        let dragging_files = ctx.input(|i| !i.raw.hovered_files.is_empty());
        if dragging_files {
            self.tab = Tab::Tray;
        }
        let hovered = ctx.input(|i| i.pointer.hover_pos().is_some())
            || dragging_files
            || drag_in_hotzone(ctx);
        let target = if hovered { 1.0 } else { 0.0 };
        let dt = ctx.input(|i| i.stable_dt).min(0.05);
        let k = 1.0 - (-16.0 * dt).exp();
        self.t += (target - self.t) * k;
        if (self.t - target).abs() < 0.002 {
            self.t = target;
        }
        let animating = self.t != target;

        // --- media state + album-art texture ---------------------------
        let np = self.media.snapshot();
        if np.art_id != self.art_id {
            self.art_id = np.art_id;
            self.art_tex = np.art.as_ref().map(|a| {
                let img = ColorImage::from_rgba_unmultiplied([a.w, a.h], &a.rgba);
                ctx.load_texture("album_art", img, TextureOptions::LINEAR)
            });
        }

        // --- lyrics: prefetch, cross-fade and smooth scrolling ----------
        if self.show_lyrics {
            self.lyrics_status = self.media.lyrics_for(&np);
        }
        let mut lyr_moving = false;
        let lv_target = if self.show_lyrics { 1.0 } else { 0.0 };
        self.lyrics_vis += (lv_target - self.lyrics_vis) * (1.0 - (-14.0 * dt).exp());
        if (self.lyrics_vis - lv_target).abs() < 0.003 {
            self.lyrics_vis = lv_target;
        }
        lyr_moving |= self.lyrics_vis != lv_target;
        if let LyricsStatus::Found(l) = &self.lyrics_status {
            let ptr = Arc::as_ptr(l) as usize;
            let pos = (np.live_pos_ms() as i64 + self.lyric_offset_ms).max(0) as u64;
            let target = l
                .current_index(pos, np.dur_ms)
                .max(if l.synced { -1 } else { 0 }) as f32;
            if ptr != self.lyric_ptr || (target - self.lyric_pos).abs() > 6.0 {
                self.lyric_ptr = ptr;
                self.lyric_pos = target;
            } else {
                self.lyric_pos += (target - self.lyric_pos) * (1.0 - (-11.0 * dt).exp());
                if (target - self.lyric_pos).abs() < 0.002 {
                    self.lyric_pos = target;
                }
            }
            lyr_moving |= self.show_lyrics && self.lyric_pos != target;
        }

        // --- system events, timers, reminders -> toasts ---------------------
        while let Ok(e) = self.sys_rx.try_recv() {
            self.on_sys_event(e);
        }
        let now = Local::now();
        if let Some(done) = self.focus.tick() {
            chime();
            if done == Phase::Focus {
                self.show_toast(
                    ToastKind::Focus,
                    "Focus complete",
                    "Time for a break",
                    None,
                    8.0,
                );
            } else {
                self.show_toast(
                    ToastKind::Focus,
                    "Break over",
                    "Ready for another session?",
                    None,
                    8.0,
                );
            }
        }
        if self.water.roll_over(&now.format("%Y-%m-%d").to_string()) {
            store::save_water(&self.water);
        }
        if self.water.reminder_due(now.hour()) {
            let sub = format!("{} / {} ml today", self.water.drank_ml, self.water.goal_ml);
            self.show_toast(ToastKind::Water, "Time to drink water", &sub, None, 8.0);
        }
        let now_s = now.format("%Y-%m-%dT%H:%M").to_string();
        let mut due_now: Vec<String> = Vec::new();
        for t in self.tasks.iter_mut() {
            if !t.done && !t.notified && t.due.as_deref().map_or(false, |d| d <= now_s.as_str()) {
                t.notified = true;
                due_now.push(t.text.clone());
            }
        }
        if !due_now.is_empty() {
            store::save_tasks(&self.tasks);
            chime();
            self.show_toast(ToastKind::Todo, &due_now[0], "Task is due", None, 8.0);
        }
        let toast_live = self
            .toast
            .as_ref()
            .map_or(false, |t| Instant::now() < t.until);
        let tt = if toast_live { 1.0 } else { 0.0 };
        self.toast_t += (tt - self.toast_t) * (1.0 - (-14.0 * dt).exp());
        if (self.toast_t - tt).abs() < 0.004 {
            self.toast_t = tt;
        }
        if !toast_live && self.toast_t == 0.0 {
            self.toast = None;
        }

        // --- resize + recenter the OS window to follow the animation ----
        let base = Vec2::new(
            lerp(COMPACT.x, TOAST.x, self.toast_t),
            lerp(COMPACT.y, TOAST.y, self.toast_t),
        );
        let size = Vec2::new(
            lerp(base.x, EXPANDED.x, self.t),
            lerp(base.y, EXPANDED.y, self.t),
        );
        let monitor_w = ctx
            .input(|i| i.viewport().monitor_size)
            .map(|m| m.x)
            .unwrap_or(1920.0);
        ctx.send_viewport_cmd(ViewportCommand::InnerSize(size));
        ctx.send_viewport_cmd(ViewportCommand::OuterPosition(Pos2::new(
            (monitor_w - size.x) / 2.0,
            0.0,
        )));

        // --- draw ------------------------------------------------------
        let r = lerp(
            lerp(COMPACT_RADIUS, 20.0, self.toast_t),
            EXPANDED_RADIUS,
            self.t,
        );
        let time = ctx.input(|i| i.time) as f32;
        let frame = egui::Frame::none().fill(Color32::TRANSPARENT);
        egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
            let rect = ui.max_rect();
            let p = ui.painter().clone();
            // Flat top (flush with the screen edge), round bottom corners.
            p.rect_filled(
                rect,
                Rounding {
                    nw: 0.0,
                    ne: 0.0,
                    sw: r,
                    se: r,
                },
                Color32::BLACK,
            );

            if self.t < 0.5 {
                if self.toast_t > 0.02 {
                    let a = self.toast_t * ((0.5 - self.t) * 2.0);
                    self.draw_toast(&p, rect, a);
                } else {
                    self.draw_compact(ui, &p, rect, &np, &now, time);
                }
                return;
            }

            let look = Look::new(rect, self.t);
            self.draw_header(ctx, ui, &p, &look, &now);
            match self.tab {
                Tab::Nook => self.draw_nook(ui, &p, &look, &np, &now),
                Tab::Tray => self.draw_tray(ui, &p, &look, dragging_files),
                Tab::Focus => self.draw_focus(ui, &p, &look),
                Tab::Water => self.draw_water(ui, &p, &look),
                Tab::Todo => self.draw_todo(ui, &p, &look, &now),
            }
        });

        // Keep ticking: every frame while animating/hovered, ~30fps when the
        // equalizer bars are moving, otherwise ~10x/sec.
        if animating || hovered || lyr_moving || self.toast.is_some() {
            ctx.request_repaint();
        } else if np.playing {
            ctx.request_repaint_after(std::time::Duration::from_millis(33));
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(COMPACT)
            .with_position(Pos2::new(0.0, 0.0))
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_taskbar(false)
            .with_resizable(false),
        ..Default::default()
    };
    eframe::run_native(
        "winnotch",
        options,
        Box::new(|_cc| Ok(Box::new(Notch::new()))),
    )
}
