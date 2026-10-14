//! Glimt: a fast, memory-frugal image viewer.
//!
//! Images are decoded in background threads to the size of the image area; full resolution
//! is decoded only when the user zooms past what that covers. Only a small window around
//! the current image is kept in memory.

use actions::{Action, CTRL, OWN_KEYS, SHORTCUTS, shortcut_text, shortcuts};
use colour::{
    COLOR_MANAGEMENT, INPUT_PROFILES, INTENTS, SCREEN_WAIT, SCREEN_WAIT_MS, colour_source,
    read_desktop_screen, set_colour, set_input, set_intent, set_screen, srgb_to_display,
};
use config::{
    Config, WindowState, parse_last_dir, sort_folders_from, sort_folders_path, state_path,
    write_atomic, xdg_dir,
};
use decode::{Decoded, Hist, MAX_ANIMATION, color_image, decode, histogram, load, shrink, survive};
use eframe::egui;
use external::{Login, Mounted, apps_for, command, gio_mount, is_video, open_url};
use files::{
    SORTS, Types, exif_index, has_ext, is_listed, list_folder, list_tree, merge_shuffled,
    pattern_name, shuffle, sort_files, walk,
};
use meta::Meta;
use save::save_rotated;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Instant;

mod actions;
mod collections;
mod colour;
mod config;
mod decode;
mod duplicates;
mod exif;
mod external;
mod files;
mod help;
#[cfg(feature = "marks")]
mod marks;
mod meta;
mod pan;
mod plugins;
mod print;
mod raw;
mod remote;
mod save;
mod search;
mod theme;
mod thumbs;
#[cfg(feature = "video")]
mod video;

/// Default number of images kept decoded on each side of the shown one.
const DEFAULT_AHEAD: usize = 2;
/// Default seconds between images in a slideshow.
const DEFAULT_DELAY: u64 = 3;
const MAX_DELAY: u64 = 60;
/// Thumbnail box in the file list, in points (Geeqie's default 96×72).
const THUMBS: [egui::Vec2; 3] = [
    egui::vec2(96.0, 72.0),
    egui::vec2(128.0, 96.0),
    egui::vec2(192.0, 144.0),
];
/// A cell of the thumbnail grid: the thumbnail with its name below.
const CELL_EXTRA: egui::Vec2 = egui::vec2(8.0, 26.0);
/// Big thumbnails (Preferences) come from the freedesktop "large" cache; read by the workers.
static LARGE_THUMBS: AtomicBool = AtomicBool::new(false);
/// Thumbnail jobs further than this from the visible rows are skipped.
const THUMB_MARGIN: usize = 20;
/// Width of the size and date columns in the file list (points).
const DATE_WIDTH: f32 = 140.0;
const DETAILS_WIDTH: f32 = DATE_WIDTH + 90.0;
/// Thumbnails kept in memory (≈ 64 kB each) before those far from view are dropped.
const MAX_THUMBS: usize = 600;

/// Preferences → CPU use: (name, hover text). Power saving makes thumbnails one at a time and
/// runs searches, the EXIF index, Find Duplicates and pan view on two cores; Balanced makes
/// two thumbnails at once and runs those on every core; Full makes thumbnails on every core.
const CPU_LEVELS: [(&str, &str); 3] = [
    (
        "Power saving",
        "Fewer cores at once: thumbnails one at a time, searches and Find Duplicates on two \
         cores. Slower in large folders, kinder to a laptop battery.",
    ),
    (
        "Balanced",
        "Two thumbnails at once; searches and Find Duplicates on every core.",
    ),
    (
        "Full",
        "Every core: thumbnails and searches as fast as the machine allows.",
    ),
];

/// Slideshow → Fade: (name for the config and --fade, menu label, milliseconds).
const FADES: [(&str, &str, u64); 3] = [
    ("off", "Off", 0),
    ("short", "Short", 400),
    ("long", "Long", 1000),
];

/// The CPU use level (index in CPU_LEVELS), for the worker threads, and the signal that wakes
/// the thumbnail workers it leaves idle when it changes.
static CPU_USE: AtomicUsize = AtomicUsize::new(1);
static CPU_CHANGED: (Mutex<()>, std::sync::Condvar) = (Mutex::new(()), std::sync::Condvar::new());

fn set_cpu_use(level: usize) {
    let _lock = CPU_CHANGED.0.lock().unwrap();
    CPU_USE.store(level, Ordering::Relaxed);
    CPU_CHANGED.1.notify_all();
}

fn cores() -> usize {
    std::thread::available_parallelism().map_or(2, |n| n.get())
}

/// Glimt runs with one malloc arena (`low_memory`): set at startup.
static ONE_ARENA: AtomicBool = AtomicBool::new(false);

/// Threads for work that allocates a little for every file, such as reading EXIF for the
/// exif: index or pan view. With one malloc arena they wait for each other in the allocator:
/// 2,000 files took 206 ms on one thread, 128 ms on two and 551 ms on four (20 cores); with an
/// arena each, eight threads took 68 ms.
fn small_alloc_threads() -> usize {
    match ONE_ARENA.load(Ordering::Relaxed) {
        true => background_threads().min(2),
        false => background_threads(),
    }
}

/// Threads for parallel work in the background: searches, the EXIF index, Find Duplicates and
/// pan view.
fn background_threads() -> usize {
    match CPU_USE.load(Ordering::Relaxed) {
        0 => cores().min(2),
        _ => cores(),
    }
}

/// Thumbnails made at once; Balanced keeps one with the low memory level (`decoders` 1), so
/// two full-size images are not decoded at once.
fn thumb_threads(decoders: usize) -> usize {
    match CPU_USE.load(Ordering::Relaxed) {
        0 => 1,
        1 => decoders.min(2),
        _ => cores(),
    }
}

/// Memory use in Preferences: (name, preload, decoding threads, animation limit in MB, one malloc
/// arena, hover text). The config holds the values; the level shown is the one they match, or
/// Custom.
const MEMORY_LEVELS: [(&str, usize, usize, usize, bool, &str); 3] = [
    (
        "Low",
        1,
        1,
        32,
        true,
        "Least memory: one image decoded at a time, one preloaded on each side. Slow storage \
         (SMB, NFS, USB) makes browsing wait for each file.",
    ),
    (
        "Medium",
        DEFAULT_AHEAD,
        4,
        128,
        true,
        "The default: four images decoded at once, two preloaded on each side.",
    ),
    (
        "High",
        5,
        8,
        256,
        false,
        "Fastest browsing: eight images decoded at once, five preloaded on each side; hides \
         the wait of slow storage (SMB, NFS) best.",
    ),
];

/// The pan view's window (Ctrl+J).
#[derive(Default)]
struct Pan {
    /// Days from the EXIF dates instead of the file dates.
    exif: bool,
    /// The days, and the version of the file list they are for (`App::files_gen`).
    days: Option<(Vec<pan::Day>, usize)>,
    rx: Option<mpsc::Receiver<(Vec<pan::Day>, usize)>>,
    /// The rows for this many columns, and their height.
    rows: Vec<(f32, pan::Row)>,
    columns: usize,
    height: f32,
}

/// The Find Duplicates window: what to compare by, the search running and its results.
struct Dups {
    by: duplicates::By,
    progress: Arc<duplicates::Progress>,
    running: Option<mpsc::Receiver<Vec<Vec<PathBuf>>>>,
    groups: Vec<Vec<PathBuf>>,
    /// (group, file) of each row: the file `usize::MAX` for the group's heading.
    rows: Vec<(usize, usize)>,
    checked: HashSet<PathBuf>,
    /// Images searched, for the line above the results.
    searched: usize,
}

impl Dups {
    fn new() -> Self {
        Dups {
            by: duplicates::By::Checksum,
            progress: Arc::default(),
            running: None,
            groups: Vec::new(),
            rows: Vec::new(),
            checked: HashSet::new(),
            searched: 0,
        }
    }

    /// Take out files that are gone; groups of one are no groups.
    fn remove(&mut self, gone: &HashSet<&Path>) {
        for g in &mut self.groups {
            g.retain(|p| !gone.contains(p.as_path()));
        }
        self.groups.retain(|g| g.len() > 1);
        self.checked.retain(|p| !gone.contains(p.as_path()));
        self.rows = (self.groups.iter().enumerate())
            .flat_map(|(g, files)| {
                std::iter::once((g, usize::MAX)).chain((0..files.len()).map(move |f| (g, f)))
            })
            .collect();
    }
}

/// View → Layout: the config value, the menu label and the panes as [columns, rows].
const LAYOUTS: [(&str, &str, [usize; 2]); 4] = [
    ("1", "One Image", [1, 1]),
    ("2x1", "Two Side by Side", [2, 1]),
    ("1x2", "Two Above Each Other", [1, 2]),
    ("2x2", "Four (2 × 2)", [2, 2]),
];

/// Options on the command line before the file or folder, for --help.
const START_OPTIONS: &[(&str, &str)] = &[
    ("--slideshow", "Start the slideshow"),
    (
        "--random",
        "Random order in the slideshow (kept, as in the View menu)",
    ),
    ("--fullscreen", "Start in full screen"),
    (
        "--kiosk",
        "Full screen, only the image, nothing changes files; ends by --remote quit",
    ),
    ("--layout 1|2x1|1x2|2x2", "Panes as in View → Layout (kept)"),
    (
        "--fade off|short|long",
        "Fade between slideshow images (kept)",
    ),
    (
        "--name NAME",
        "Name this Glimt; glimt --name NAME --remote COMMAND reaches only it",
    ),
    ("-V, --version", "Print the version"),
    ("-h, --help", "Print this help"),
];

/// The actions a kiosk allows: showing and browsing, not changing files, settings or the
/// window.
const KIOSK_ACTIONS: &[Action] = &[
    Action::Next,
    Action::Prev,
    Action::First,
    Action::Last,
    Action::Slideshow,
    Action::Pause,
    Action::Faster,
    Action::Slower,
    Action::RandomOrder,
    Action::ZoomIn,
    Action::ZoomOut,
    Action::Zoom1To1,
    Action::ZoomFit,
    Action::Overlay,
    Action::Histogram,
    Action::HistogramLog,
    Action::Compare,
    Action::Animate,
    Action::OneImage,
    Action::AboveEachOther,
    Action::NextLayout,
    Action::PaneLeft,
    Action::PaneRight,
    Action::PaneUp,
    Action::PaneDown,
    Action::NextPage,
    Action::PrevPage,
];

/// `orig` fitted into `rect`, centred; larger than its size only with `enlarge`.
fn fitted(rect: egui::Rect, orig: [usize; 2], ppp: f32, enlarge: bool) -> egui::Rect {
    let size = egui::vec2(orig[0] as f32, orig[1] as f32);
    let fit = (rect.width() * ppp / size.x).min(rect.height() * ppp / size.y);
    let fit = if enlarge { fit } else { fit.min(1.0) };
    egui::Rect::from_center_size(rect.center(), size * fit / ppp)
}

/// A texture shown and its image's size, which a fade goes from.
type Shown = (egui::TextureHandle, [usize; 2]);

/// Faded to `alpha` (0 to 1), as a tint. A fade draws the image before at 1 - t⁴ under the new
/// one at t: the sum stays near full brightness, where 1 - t would darken the middle.
fn alpha(a: f32) -> egui::Color32 {
    egui::Color32::from_white_alpha((a.clamp(0.0, 1.0) * 255.0) as u8)
}

/// A small map of the panes in the status bar, with the one it tells about filled in.
fn pane_map(ui: &mut egui::Ui, [cols, rows]: [usize; 2], on: usize) {
    let cell = egui::vec2(9.0, 7.0);
    let size = egui::vec2(cols as f32, rows as f32) * (cell + egui::vec2(2.0, 2.0));
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
    let v = ui.visuals();
    for k in 0..cols * rows {
        let at = egui::vec2((k % cols) as f32, (k / cols) as f32) * (cell + egui::vec2(2.0, 2.0));
        let r = egui::Rect::from_min_size(rect.min + at + egui::vec2(1.0, 1.0), cell);
        if k == on {
            ui.painter().rect_filled(r, 1.0, v.selection.bg_fill);
        } else {
            let stroke = egui::Stroke::new(1.0, v.weak_text_color());
            ui.painter()
                .rect_stroke(r, 1.0, stroke, egui::StrokeKind::Inside);
        }
    }
    response.on_hover_text("The pane these details are of: the one under the pointer");
}

/// What the start options ask for.
#[derive(Clone, Default)]
struct Start {
    slideshow: bool,
    random: bool,
    fullscreen: bool,
    layout: Option<&'static str>,
    name: Option<String>,
    kiosk: bool,
    /// --fade: an index in FADES.
    fade: Option<usize>,
    /// More folders after the first, to show together.
    more: Vec<PathBuf>,
}

impl Start {
    /// The options in front of `args`, and the arguments after them.
    fn parse(mut args: Vec<OsString>) -> Result<(Start, Vec<OsString>), String> {
        let mut start = Start::default();
        while let Some(arg) = args.first().and_then(|a| a.to_str()).map(str::to_owned) {
            match arg.as_str() {
                "--slideshow" => start.slideshow = true,
                "--random" => start.random = true,
                "--fullscreen" => start.fullscreen = true,
                "--kiosk" => start.kiosk = true,
                "--fade" => {
                    let value = args.get(1).and_then(|a| a.to_str()).unwrap_or_default();
                    let k = FADES.iter().position(|f| f.0 == value);
                    start.fade = Some(k.ok_or("--fade takes off, short or long")?);
                    args.remove(0);
                }
                "--layout" => {
                    let value = args.get(1).and_then(|a| a.to_str()).unwrap_or_default();
                    let layout = LAYOUTS.iter().find(|l| l.0 == value).ok_or_else(|| {
                        let all: Vec<&str> = LAYOUTS.iter().map(|l| l.0).collect();
                        format!("--layout takes one of {}", all.join(", "))
                    })?;
                    start.layout = Some(layout.0);
                    args.remove(0);
                }
                "--name" => {
                    let name = args.get(1).and_then(|a| a.to_str()).unwrap_or_default();
                    if !remote::valid_name(name) || name.is_empty() {
                        return Err(
                            "--name takes letters, digits, - and _ (not only digits)".into()
                        );
                    }
                    start.name = Some(name.to_owned());
                    args.remove(0);
                }
                "--help" | "-h" | "--version" | "-V" | "--remote" | "--remote-all" => break,
                a if a.starts_with("--") => return Err(format!("unknown option {a}")),
                _ => break,
            }
            args.remove(0);
        }
        Ok((start, args))
    }
}

fn layout_grid(layout: &str) -> [usize; 2] {
    LAYOUTS
        .iter()
        .find(|l| l.0 == layout)
        .map_or([1, 1], |l| l.2)
}

/// What a zenity file dialog was opened for.
enum Pick {
    Open,
    /// Several folders to show as one list.
    Folders,
    /// Open a collection: the dialog starts in the collections folder.
    Collection,
    CopyTo(Vec<PathBuf>),
    MoveTo(Vec<PathBuf>),
    /// Where to save the PDF of these images.
    SavePdf(PrintJob),
    /// A folder for the sort manager.
    SortFolder,
    /// An ICC profile for the screen.
    ScreenProfile,
    /// An ICC profile for images without one.
    InputProfile,
}

/// Images to print, and how.
struct PrintJob {
    paths: Vec<PathBuf>,
    paper: [f32; 2],
    /// Images on each page (print::PER_PAGE).
    per_page: usize,
    caption: bool,
    rotate: bool,
    copies: u32,
}

/// Where a print job goes.
enum PrintTo {
    Printer(String),
    File(PathBuf),
}

/// The Print dialog (Ctrl+P).
struct PrintDialog {
    paths: Vec<PathBuf>,
    /// The printers and the default, from lpstat in the background; `None` until known.
    printers: Option<Vec<String>>,
    printers_rx: mpsc::Receiver<(Vec<String>, Option<String>)>,
    copies: u32,
}

/// What the one-line text dialog is asking for.
#[derive(Clone, Copy)]
enum Prompt {
    NewFolder,
    /// Rename the folder in `App::folder_target`.
    RenameFolder,
    /// Rename the shown image.
    Rename,
    /// Rename the selected files with a pattern: * = old name, # = number.
    RenameMany,
    /// A new collection with the selected files.
    NewCollection,
    /// Save the look shown as a theme file of this name.
    SaveTheme,
}

/// A menu that scrolls when it is taller than the window, instead of running off it.
fn scroll_menu<R>(
    ui: &mut egui::Ui,
    title: impl Into<egui::WidgetText>,
    add: impl FnOnce(&mut egui::Ui) -> R,
) {
    ui.menu_button(title, |ui| {
        // From where it opens to the bottom of the window; submenus open further down. egui
        // limits a menu to its size the frame before, which a scroll area could only keep or
        // shrink: the limit is set here instead.
        let margin = ui.spacing().menu_margin.bottom as f32 + ui.spacing().item_spacing.y;
        let max = ui.ctx().content_rect().bottom() - ui.next_widget_position().y - margin;
        ui.set_max_height(max);
        egui::ScrollArea::vertical().max_height(max).show(ui, add);
    });
}

/// A menu entry showing the action's shortcut; sets `act` when clicked.
fn menu_item(ui: &mut egui::Ui, act: &mut Option<Action>, action: Action) {
    menu_button(ui, act, egui::Button::new(action.label()), action);
}

/// A menu entry with a check mark when `on`.
fn menu_toggle(ui: &mut egui::Ui, act: &mut Option<Action>, on: bool, action: Action) {
    menu_button(
        ui,
        act,
        egui::Button::selectable(on, action.label()),
        action,
    );
}

fn menu_button(
    ui: &mut egui::Ui,
    act: &mut Option<Action>,
    mut button: egui::Button,
    action: Action,
) {
    if let Some(text) = shortcut_text(ui.ctx(), action) {
        button = button.shortcut_text(text);
    }
    if ui.add(button).clicked() {
        *act = Some(action);
    }
}

/// A toolbar button with the action's name and shortcut as tooltip.
fn tool_button(ui: &mut egui::Ui, act: &mut Option<Action>, icon: &str, action: Action, on: bool) {
    let tip = match shortcut_text(ui.ctx(), action) {
        Some(keys) => format!("{} ({keys})", action.label()),
        None => action.label().to_owned(),
    };
    if ui
        .add(egui::Button::selectable(on, icon))
        .on_hover_text(tip)
        .clicked()
    {
        *act = Some(action);
    }
}

unsafe extern "C" {
    /// glibc: return freed heap memory to the operating system.
    fn malloc_trim(pad: usize) -> i32;
    /// glibc: set a malloc parameter.
    fn mallopt(param: i32, value: i32) -> i32;
}

/// glibc's M_ARENA_MAX.
const M_ARENA_MAX: i32 = -8;

/// A file's EXIF from the reader thread: the panel's rows, every field, keywords and comment.
type ExifRead = (
    PathBuf,
    Vec<(&'static str, String)>,
    Vec<(&'static str, String, String)>,
    meta::Meta,
);

/// Keywords and comment of the shown image as being edited in the EXIF panel, and as last read
/// or saved, to tell whether they changed.
struct MetaEdit {
    path: PathBuf,
    keywords: String,
    comment: String,
    saved: meta::Meta,
}

struct Slot {
    tex: egui::TextureHandle,
    /// The file the texture was made from; decides whether it can be kept when the file list changes.
    path: PathBuf,
    full: bool,
    /// Original size after EXIF rotation.
    orig: [usize; 2],
    /// Frames of an animated GIF or WebP with their delays in ms; empty for a still image.
    anim: Vec<(egui::TextureHandle, u32)>,
    /// Of the decoded image (the first frame of an animation).
    hist: Box<Hist>,
    /// Pages of a multi-page TIFF; 1 for other files.
    pages: usize,
}

impl Slot {
    /// The frame to show `t` after the image was opened; asks for a repaint when the next is due.
    fn frame(&self, t: std::time::Duration, ctx: &egui::Context) -> &egui::TextureHandle {
        let total: u32 = self.anim.iter().map(|f| f.1).sum();
        let mut at = (t.as_millis() % total.max(1) as u128) as u32;
        for (tex, ms) in &self.anim {
            if at < *ms {
                ctx.request_repaint_after(std::time::Duration::from_millis((ms - at) as u64));
                return tex;
            }
            at -= ms;
        }
        &self.tex
    }
}

struct App {
    dir: PathBuf,
    subdirs: Vec<PathBuf>,
    /// The images in `dir`, file name only (the full path is built on demand by `path`): saves
    /// memory in large folders, where the path would otherwise be repeated for every file.
    files: Vec<OsString>,
    cur: usize,
    /// The images to keep decoded, the shown one first: its neighbours in list order.
    /// Shared with the workers, which skip jobs outside it.
    keep: Arc<Mutex<Vec<usize>>>,
    /// How many images at the start of `keep` may be decoded at full resolution: the shown one,
    /// and the compared one beside it.
    full_ok: Arc<AtomicUsize>,
    /// Number of images preloaded on each side (Preferences).
    ahead: Arc<AtomicUsize>,
    /// Workers apply EXIF orientation (Preferences, Alt+X).
    rotate: Arc<AtomicBool>,
    show_settings: bool,
    help: Option<help::Page>,
    /// The delete confirmation is open: `Some(true)` = move to trash, `Some(false)` = permanently.
    confirm: Option<bool>,
    /// The folder a folder's context menu acts on: to rename, or, with `confirm_folder`, to trash.
    folder_target: Option<PathBuf>,
    confirm_folder: bool,
    /// The type of the file opened directly (not the folder), listed even when the Types menu
    /// leaves it out.
    opened_type: Types,
    /// Open text dialog and the text typed so far.
    prompt: Option<(Prompt, String)>,
    /// Scale from the previous frame (physical pixels per image pixel), for the zoom percentage in the status bar.
    last_scale: f32,
    /// Mouse wheel notches not yet turned into an image step (wheel_zoom off).
    wheel: f32,
    /// Last drawn image; kept on screen until the next one is decoded, so browsing never shows a black screen.
    last_drawn: Option<(egui::TextureHandle, [usize; 2])>,
    /// A slideshow's change of image fading in: when it began, and what each pane (the first,
    /// then the others) showed before, kept until it has faded out.
    fade: Option<(Instant, Vec<Option<Shown>>)>,
    /// Selected files (Ctrl/Shift+click in the file list) that Delete acts on.
    selected: std::collections::BTreeSet<usize>,
    config: Config,
    /// Max decode size in physical pixels: [image area in the window, max texture side].
    limits: Arc<Mutex<[[u32; 2]; 2]>>,
    /// (image index, full resolution, file, page of a multi-page TIFF)
    jobs: mpsc::Sender<(usize, bool, PathBuf, usize)>,
    /// The page of a multi-page TIFF shown (Ctrl+Page Down/Up), from 0.
    page: usize,
    done: mpsc::Receiver<Decoded>,
    slots: HashMap<usize, Slot>,
    pending: HashSet<(usize, bool)>,
    errors: HashMap<usize, String>,
    /// Physical screen pixels per original pixel; `None` = fit to window.
    zoom: Option<f32>,
    pan: egui::Vec2,
    /// Set at startup; taken when the first image is shown.
    started: Option<Instant>,
    /// Answer from an open file dialog (zenity).
    dialog: Option<(Pick, mpsc::Receiver<Option<PathBuf>>)>,
    /// Generation of the folder watcher; opening another folder ends the old thread.
    watching: Arc<AtomicUsize>,
    /// The watcher saw the folder change.
    changed_tx: mpsc::Sender<()>,
    changed_rx: mpsc::Receiver<()>,
    /// The folder changed while a listing was running; list again when it is done.
    stale: bool,
    /// View → Hide File List: only the image, as in full screen but in a window.
    hide_tools: bool,
    /// Go → Back / Forward: earlier locations (file shown, or folder if it had no images).
    back: Vec<PathBuf>,
    forward: Vec<PathBuf>,
    /// Error messages from background tasks (deleting), and the one shown now.
    notices_tx: mpsc::Sender<String>,
    /// A network location (smb:// …) being mounted: its local path, or why it failed.
    mounting: Option<mpsc::Receiver<(Mounted, Login)>>,
    /// The login dialog for a network share that needs one.
    login: Option<Login>,
    print: Option<PrintDialog>,
    /// The sort manager's folders, kept in `$XDG_CONFIG_HOME/glimt/sort-folders`.
    sort_folders: Vec<PathBuf>,
    pan_window: Option<Pan>,
    /// Goes up whenever the file list changes, so what was worked out from its indices can tell
    /// that it is old, also when the list kept its length (another order).
    files_gen: usize,
    /// `files_gen` and `cur` when Rename or a delete was asked: the answer acts on those files
    /// only if the list and the shown image are still the same.
    asked_for: (usize, usize),
    /// "Open With" applications by file extension, found once per extension.
    /// `None` while `apps_for` runs in the background.
    apps: HashMap<String, Option<Vec<(String, PathBuf)>>>,
    apps_tx: mpsc::Sender<(String, Vec<(String, PathBuf)>)>,
    /// The desktop's dark style and accent colour, once read from the portal.
    desktop_look: (Option<bool>, Option<egui::Color32>),
    desktop_rx: Option<mpsc::Receiver<(Option<bool>, Option<egui::Color32>)>>,
    /// The desktop's screen profile file, once read from colord.
    desktop_screen: Option<String>,
    desktop_screen_rx: Option<mpsc::Receiver<Option<String>>>,
    /// Edit → Plugins, read at start.
    plugins: Vec<plugins::Plugin>,
    apps_rx: mpsc::Receiver<(String, Vec<(String, PathBuf)>)>,
    /// Commands from `glimt --remote`.
    remote: mpsc::Receiver<String>,
    /// The window had the focus last frame; getting it makes this Glimt the one `--remote`
    /// talks to.
    has_focus: bool,
    /// Our own clipboard for file lists, kept so it can serve them (X11).
    clipboard: Option<arboard::Clipboard>,
    /// An image decoded for the clipboard; egui takes it only during a frame.
    clip: Option<mpsc::Receiver<egui::ColorImage>>,
    /// Subfolders by folder for the folder tree; `None` while being read.
    tree: HashMap<PathBuf, Option<Vec<PathBuf>>>,
    tree_tx: mpsc::Sender<(PathBuf, Vec<PathBuf>)>,
    tree_rx: mpsc::Receiver<(PathBuf, Vec<PathBuf>)>,
    /// The folder the tree was last opened up to; it opens again when the folder changes.
    tree_shown: PathBuf,
    #[cfg(feature = "marks")]
    marks: marks::Marks,
    /// The list holds the images of the subfolders too (slideshow with subfolders).
    recursive: bool,
    /// The open collection's file. The list is then its images, with `dir` "/" and the paths
    /// without their leading "/" as names, so `path()` works unchanged.
    collection: Option<PathBuf>,
    /// The collection's paths as last read or written here; what is on disk beyond them was
    /// added elsewhere and is kept when this window saves.
    collection_read: HashSet<PathBuf>,
    /// Several folders shown as one list (glimt DIR DIR …, File → Open Folders…): the names
    /// are relative to /, as in a collection, and the folders are read again on refresh.
    folders: Vec<PathBuf>,
    /// Folders to show together, chosen in a folder's menu; opened after the frame.
    together: Option<Vec<PathBuf>>,
    /// The collection's next listing is sorted (Sort menu), and saved in that order.
    sort_collection: bool,
    /// The Find Duplicates window, while open.
    dups: Option<Dups>,
    /// The next listing is shuffled as a whole instead of merged into the current order.
    whole_shuffle: bool,
    /// The action whose key is being chosen in Preferences: the next key press goes to it.
    capturing: Option<Action>,
    /// --slideshow: start it once the folder is read.
    start_slideshow: bool,
    /// --kiosk: full screen, only the image, only actions that show and browse.
    kiosk: bool,
    /// `--remote quit` came: a kiosk may close.
    kiosk_quit: bool,
    /// Two images side by side (E): the shown one and `compare_other`.
    compare: bool,
    /// The images of a layout's panes after the first, which shows `cur`.
    panes: Vec<usize>,
    /// A slideshow changing one pane at a time: the pane next (0 is the first), and the image
    /// shown last.
    turn: usize,
    latest: usize,
    /// Columns of the icon grid in the last frame; 1 when the list is rows or hidden.
    grid_cols: usize,
    /// How far Previous/Next move this frame: a row of the icon grid for ↑ and ↓, else 1.
    row_step: isize,
    notices: mpsc::Receiver<String>,
    notice: Option<String>,
    /// Total size of the images in the folder, summed in the background after listing.
    /// Only sums are sent (no per-file table), so memory use does not grow with the folder.
    total_size: u64,
    /// Partial sums from the background; `None` once all have arrived.
    size_rx: Option<mpsc::Receiver<u64>>,
    /// Bytes freed by deletions in the background.
    freed_tx: mpsc::Sender<u64>,
    freed_rx: mpsc::Receiver<u64>,
    /// Size of the selected files; computed in the background when the selection changes.
    selected_size: Option<u64>,
    selected_rx: Option<mpsc::Receiver<u64>>,
    /// Folder listing in the background: (subfolders, images).
    listing: Option<mpsc::Receiver<(Vec<PathBuf>, Vec<OsString>)>>,
    /// Scroll the file list to the current image on the next frame.
    scroll_to_cur: bool,
    /// Slideshow: when the previous image was shown; `None` = off.
    slideshow: Option<Instant>,
    /// Slideshow paused (P): stays on the image until resumed.
    paused: bool,
    /// The file list is shuffled for a random slideshow.
    random: bool,
    /// File list thumbnails by file name; `None` = could not be made.
    thumbs: HashMap<OsString, Option<egui::TextureHandle>>,
    thumbs_pending: HashSet<OsString>,
    /// (image index, file)
    thumb_jobs: mpsc::Sender<(usize, PathBuf)>,
    /// (file, thumbnail); the outer `None` = skipped because it scrolled out of view.
    thumbs_done: mpsc::Receiver<(PathBuf, Option<Option<egui::ColorImage>>)>,
    /// First and last file shown by the file list (or film strip), then by pan view (none: MAX,
    /// 0), for skipping stale thumbnail jobs.
    thumb_view: Arc<[AtomicUsize; 4]>,
    /// The files whose cells pan view draws this frame: their thumbnails are made and kept,
    /// also while the file list shows others.
    pan_thumbs: Vec<usize>,
    /// Workers write new thumbnails to the shared cache (Preferences).
    save_thumbs: Arc<AtomicBool>,
    /// EXIF panel: the file asked for last, and the rows read for a file.
    exif_jobs: mpsc::Sender<PathBuf>,
    exif_done: mpsc::Receiver<ExifRead>,
    /// Every EXIF field of the shown image, for the EXIF window (Ctrl+E).
    exif_all: Vec<(&'static str, String, String)>,
    exif_window: bool,
    exif_asked: Option<PathBuf>,
    /// Keywords and comment in the EXIF panel; saved when a field loses the focus, or another
    /// image is shown.
    meta: Option<MetaEdit>,
    /// Alt+K: put the focus in the keywords field when the panel shows next.
    focus_keywords: bool,
    exif: Option<(PathBuf, Vec<(&'static str, String)>)>,
    /// Size in bytes of the shown file, for the status bar.
    cur_size: Option<(PathBuf, u64)>,
    /// Image and scale in each pane of the last frame, the pane the status bar tells about (the
    /// one under the pointer, else the first) and the panes as [columns, rows].
    pane_info: Vec<(usize, Option<f32>)>,
    status_pane: usize,
    status_grid: [usize; 2],
    /// The place "[n/m]" of an image in another pane than the first, by image and list length.
    status_place: Option<(usize, usize, String)>,
    /// The folder path in the toolbar, as typed.
    path_text: String,
    /// Window geometry seen in the last frame, saved at exit.
    window: WindowState,
    /// The screen saver inhibitor running while in full screen.
    inhibitor: Option<std::process::Child>,
    /// File list size and date columns: (bytes, mtime) by file name.
    details: HashMap<OsString, (u64, i64)>,
    /// Names being deleted in the background; kept out of refreshed listings until done.
    deleting: HashSet<OsString>,
    /// Deletes and moves done, and whether to read the folder again: one failed, so the files
    /// may still be there, or files were moved or copied into the list.
    deleted_tx: mpsc::Sender<(Vec<PathBuf>, bool)>,
    deleted_rx: mpsc::Receiver<(Vec<PathBuf>, bool)>,
    /// PNG and WebP files saved turned in the background.
    saved_tx: mpsc::Sender<(PathBuf, Result<(), String>)>,
    saved_rx: mpsc::Receiver<(PathBuf, Result<(), String>)>,
    /// Those still being saved.
    saving: HashSet<PathBuf>,
    /// Background deletes, copies and moves still running, for the busy pointer.
    busy: Arc<AtomicUsize>,
    /// File name search above the file list, and the matching files best first while it has
    /// at least three letters.
    search: String,
    matches: Option<Vec<usize>>,
    /// The shown file's row among the matches, as (file, row): found once per image and search,
    /// not in every frame, as the matches can be 300,000 long.
    cur_row: std::cell::Cell<Option<(usize, Option<usize>)>>,
    /// Total size of the search matches, summed in the background like `total_size`.
    match_size: u64,
    /// Why a re: search pattern is not one, shown under the search field.
    search_error: Option<String>,
    match_rx: Option<mpsc::Receiver<u64>>,
    /// EXIF text of each file in the folder, lowercase, for "exif:" searches; built on the
    /// first one, dropped with the folder.
    exif_index: Option<HashMap<OsString, String>>,
    exif_rx: Option<mpsc::Receiver<HashMap<OsString, String>>>,
    /// Where a Shift selection started (file index).
    anchor: Option<usize>,
    /// Orientation of the shown image, for viewing only: quarter turns clockwise, applied
    /// after mirroring when the flag is set. Reset for each image, as in Geeqie.
    orient: (u8, bool),
    /// When the shown image was opened: the clock of an animation.
    opened: Instant,
    /// How far the shown image can be panned each way (points); zero where it fits.
    max_pan: egui::Vec2,
}

/// Drop the entries for `removed` (sorted) and shift the rest down by the number removed
/// before them.
fn without_indices<T>(map: HashMap<usize, T>, removed: &[usize]) -> HashMap<usize, T> {
    map.into_iter()
        .filter_map(|(i, v)| Some((i - removed.binary_search(&i).err()?, v)))
        .collect()
}

/// Send sizes as partial sums of 10,000 files, so the status bar fills in gradually; stops
/// when the receiver is gone. The closed channel tells the UI the sum is complete.
fn send_sizes(sizes: impl Iterator<Item = u64>, tx: mpsc::Sender<u64>, ctx: &egui::Context) {
    let mut sizes = sizes.peekable();
    while sizes.peek().is_some() {
        if tx.send(sizes.by_ref().take(10_000).sum()).is_err() {
            return;
        }
        ctx.request_repaint();
    }
    drop(tx);
    ctx.request_repaint();
}

/// Add the partial sums from `send_sizes` that have arrived; `rx` becomes `None` when done.
fn drain(rx: &mut Option<mpsc::Receiver<u64>>, total: &mut u64) {
    loop {
        match rx.as_ref().map(mpsc::Receiver::try_recv) {
            Some(Ok(sum)) => *total += sum,
            Some(Err(mpsc::TryRecvError::Disconnected)) => *rx = None,
            _ => return,
        }
    }
}

/// A size for the status bar, or "calculating…" while it is summed (`None`).
fn size_text(bytes: Option<u64>) -> String {
    bytes.map_or("calculating…".to_owned(), human_size)
}

/// Sum of the file sizes (one stat per file); unreadable files are not counted.
fn total_size(paths: &[PathBuf]) -> u64 {
    paths
        .iter()
        .filter_map(|p| p.metadata().ok())
        .map(|m| m.len())
        .sum()
}

#[repr(C)]
struct Tm {
    sec: i32,
    min: i32,
    hour: i32,
    mday: i32,
    mon: i32,
    year: i32,
    wday: i32,
    yday: i32,
    isdst: i32,
    gmtoff: i64,
    zone: *const std::ffi::c_char,
}

unsafe extern "C" {
    fn localtime_r(time: *const i64, tm: *mut Tm) -> *mut Tm;
}

/// Seconds since 1970 as local "2026-10-11 00:34:12".
fn local_time(secs: i64) -> String {
    // SAFETY: Tm matches glibc's struct tm on 64-bit Linux, and both pointers are valid.
    let tm = unsafe {
        let mut tm: Tm = std::mem::zeroed();
        if localtime_r(&secs, &mut tm).is_null() {
            return String::new();
        }
        tm
    };
    format!(
        "{}-{:02}-{:02} {:02}:{:02}:{:02}",
        tm.year + 1900,
        tm.mon + 1,
        tm.mday,
        tm.hour,
        tm.min,
        tm.sec
    )
}

/// Size as in the file manager: "850.0 kB", "2.3 MB", "1.2 GB" (decimal units).
fn human_size(bytes: u64) -> String {
    let (value, unit) = match bytes {
        b if b >= 1_000_000_000 => (b as f64 / 1e9, "GB"),
        b if b >= 1_000_000 => (b as f64 / 1e6, "MB"),
        b if b >= 1_000 => (b as f64 / 1e3, "kB"),
        b => return format!("{b} B"),
    };
    format!("{value:.1} {unit}")
}

/// Integer with comma thousands separators: 300000 → "300,000".
fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

impl App {
    fn new(cc: &eframe::CreationContext, path: Option<PathBuf>, start: Start) -> Self {
        let keep = Arc::new(Mutex::new(vec![0]));
        let full_ok = Arc::new(AtomicUsize::new(1));
        let mut config = Config::load();
        // Start options are kept, as choosing them in the View menu would.
        if let Some(layout) = start.layout {
            config.layout = layout.into();
        }
        config.random |= start.random;
        if let Some(fade) = start.fade {
            config.fade = fade;
        }
        if start.layout.is_some() || start.random || start.fade.is_some() {
            config.save();
        }
        COLOR_MANAGEMENT.store(config.color_management, Ordering::Relaxed);
        let screen_error = (set_screen(&config.screen_profile))
            .and_then(|_| set_input(&config.input_profile))
            .err();
        set_colour(|c| c.own = config.own_profile);
        set_intent(&config.rendering_intent);
        if config.desktop_profile && config.color_management {
            let deadline = Instant::now() + std::time::Duration::from_millis(SCREEN_WAIT_MS);
            *SCREEN_WAIT.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(deadline);
        }
        if let Ok(mut own) = OWN_KEYS.write() {
            own.clone_from(&config.keys.0);
        }
        LARGE_THUMBS.store(config.thumb_size == 2, Ordering::Relaxed);
        let path = path
            .or_else(|| {
                let state = std::fs::read_to_string(state_path()?).ok()?;
                parse_last_dir(&state).filter(|d| {
                    config.remember_dir && (d.is_dir() || collections::is_collection(d))
                })
            })
            .unwrap_or_else(|| ".".into());
        let ahead = Arc::new(AtomicUsize::new(config.preload));
        let rotate = Arc::new(AtomicBool::new(config.exif_rotate));
        let limits = Arc::new(Mutex::new([[1920, 1080], [8192, 8192]]));
        let (jobs, job_rx) = mpsc::channel::<(usize, bool, PathBuf, usize)>();
        let (done_tx, done) = mpsc::channel();
        let job_rx = Arc::new(Mutex::new(job_rx));
        MAX_ANIMATION.store(config.animation_mb << 20, Ordering::Relaxed);
        set_cpu_use(config.cpu_use);
        let workers = cores().min(config.decode_threads);
        for _ in 0..workers {
            let (keep, full_ok, rotate, limits, job_rx, done_tx, ctx) = (
                keep.clone(),
                full_ok.clone(),
                rotate.clone(),
                limits.clone(),
                job_rx.clone(),
                done_tx.clone(),
                cc.egui_ctx.clone(),
            );
            std::thread::spawn(move || {
                loop {
                    let Ok((idx, full, path, page)) = job_rx.lock().unwrap().recv() else {
                        return;
                    };
                    // Cancellation: skip images the user has already moved past.
                    let wanted = {
                        let w = keep.lock().unwrap();
                        let n = full_ok.load(Ordering::Relaxed);
                        w.contains(&idx) && (!full || w.iter().take(n).any(|&i| i == idx))
                    };
                    if !wanted {
                        let _ = done_tx.send(Decoded {
                            rotated: false,
                            idx,
                            full,
                            path,
                            page,
                            result: None,
                        });
                        continue;
                    }
                    let max = limits.lock().unwrap()[full as usize];
                    let rotated = rotate.load(Ordering::Relaxed);
                    // The histogram here too, so the UI thread never walks the pixels.
                    let result = Some(survive(|| decode(&path, max, rotated, page)).map(
                        |(frames, orig, pages)| {
                            let hist = histogram(&frames[0].0);
                            (frames, orig, hist, pages)
                        },
                    ));
                    let _ = done_tx.send(Decoded {
                        rotated,
                        idx,
                        full,
                        path,
                        page,
                        result,
                    });
                    // glibc does not return freed memory from the threads' malloc arenas on its own;
                    // decoding leaves several image-sized buffers per thread.
                    // SAFETY: malloc_trim has no preconditions beyond a working glibc malloc.
                    unsafe { malloc_trim(0) };
                    ctx.request_repaint();
                }
            });
        }
        // Thumbnails have their own workers, so scrolling the list never delays the shown image.
        let (thumb_jobs, thumb_rx) = mpsc::channel::<(usize, PathBuf)>();
        let (thumbs_tx, thumbs_done) = mpsc::channel();
        let thumb_rx = Arc::new(Mutex::new(thumb_rx));
        let thumb_view = Arc::new([0, 0, usize::MAX, 0].map(AtomicUsize::new));
        let save_thumbs = Arc::new(AtomicBool::new(config.save_thumbnails));
        // As many as there are cores; CPU use (thumb_threads) says how many take jobs, so it
        // changes without a restart.
        for k in 0..cores() {
            let (rx, tx, view, rotate, save, ctx) = (
                thumb_rx.clone(),
                thumbs_tx.clone(),
                thumb_view.clone(),
                rotate.clone(),
                save_thumbs.clone(),
                cc.egui_ctx.clone(),
            );
            std::thread::spawn(move || {
                loop {
                    let mut idle = CPU_CHANGED.0.lock().unwrap();
                    while k >= thumb_threads(workers) {
                        idle = CPU_CHANGED.1.wait(idle).unwrap();
                    }
                    drop(idle);
                    let Ok((idx, path)) = rx.lock().unwrap().recv() else {
                        return;
                    };
                    let near = |first: usize, last: usize| {
                        let [first, last] = [first, last].map(|i| view[i].load(Ordering::Relaxed));
                        idx + THUMB_MARGIN >= first && idx <= last + THUMB_MARGIN
                    };
                    let result = (near(0, 1) || near(2, 3)).then(|| {
                        let (rotate, save) =
                            (rotate.load(Ordering::Relaxed), save.load(Ordering::Relaxed));
                        let large = LARGE_THUMBS.load(Ordering::Relaxed);
                        survive(|| thumbs::thumbnail(&path, rotate, save, large))
                            .ok()
                            // sRGB in the shared cache; to the screen only to be shown.
                            .map(|img| color_image(srgb_to_display(img.into())))
                    });
                    let _ = tx.send((path, result));
                    // SAFETY: as for the image workers above.
                    unsafe { malloc_trim(0) };
                    ctx.request_repaint();
                }
            });
        }
        // One EXIF reader that skips to the newest request, so fast browsing never queues up.
        let (exif_jobs, exif_rx) = mpsc::channel::<PathBuf>();
        let (exif_tx, exif_done) = mpsc::channel();
        theme::fonts(&cc.egui_ctx);
        theme::apply(&cc.egui_ctx, &theme::load(&config.theme), None, None);
        // gdbus takes a moment; the window shows meanwhile and takes the desktop's look after.
        let (desktop_tx, desktop_rx) = mpsc::channel();
        let ctx = cc.egui_ctx.clone();
        std::thread::spawn(move || {
            let _ = desktop_tx.send(theme::desktop());
            ctx.request_repaint();
        });
        let ctx = cc.egui_ctx.clone();
        std::thread::spawn(move || {
            while let Ok(mut path) = exif_rx.recv() {
                path = exif_rx.try_iter().last().unwrap_or(path);
                let size = std::fs::metadata(&path).map_or(0, |m| m.len());
                let mut rows = vec![
                    ("File size", human_size(size)),
                    ("Colour profile", colour_source(&path)),
                ];
                rows.extend(exif::read(&path));
                let all = exif::all(&path);
                let meta = meta::read(&path);
                if exif_tx.send((path, rows, all, meta)).is_err() {
                    return;
                }
                ctx.request_repaint();
            }
        });
        let (notices_tx, notices) = mpsc::channel();
        let (tree_tx, tree_rx) = mpsc::channel();
        let (apps_tx, apps_rx) = mpsc::channel();
        let (remote_tx, remote_rx) = mpsc::channel();
        // A name is only of use with remote control, so it listens anyway.
        if config.remote_control || remote::name().is_some() {
            remote::listen(remote_tx, cc.egui_ctx.clone());
        }
        let (freed_tx, freed_rx) = mpsc::channel();
        let (deleted_tx, deleted_rx) = mpsc::channel();
        let (saved_tx, saved_rx) = mpsc::channel();
        let (changed_tx, changed_rx) = mpsc::channel();
        let mut app = App {
            notices_tx,
            mounting: None,
            login: None,
            print: None,
            sort_folders: (sort_folders_path().and_then(|p| std::fs::read(p).ok()))
                .map_or_else(Vec::new, |t| sort_folders_from(&t)),
            pan_window: None,
            files_gen: 0,
            asked_for: (0, 0),
            grid_cols: 1,
            compare: false,
            panes: Vec::new(),
            turn: 0,
            latest: 0,
            capturing: None,
            recursive: false,
            collection: None,
            collection_read: HashSet::new(),
            folders: Vec::new(),
            together: None,
            sort_collection: false,
            dups: None,
            whole_shuffle: false,
            #[cfg(feature = "marks")]
            marks: marks::Marks::load(),
            tree: HashMap::new(),
            tree_tx,
            tree_rx,
            tree_shown: PathBuf::new(),
            clip: None,
            clipboard: None,
            remote: remote_rx,
            has_focus: true,
            apps: HashMap::new(),
            apps_tx,
            desktop_look: (None, None),
            desktop_rx: Some(desktop_rx),
            desktop_screen: None,
            desktop_screen_rx: (config.desktop_profile).then(|| read_desktop_screen(&cc.egui_ctx)),
            plugins: plugins::find(),
            apps_rx,
            row_step: 1,
            notices,
            notice: screen_error,
            dir: PathBuf::new(),
            subdirs: Vec::new(),
            files: Vec::new(),
            cur: 0,
            keep,
            full_ok,
            ahead,
            rotate,
            show_settings: false,
            help: None,
            confirm: None,
            folder_target: None,
            confirm_folder: false,
            opened_type: Types::NONE,
            prompt: None,
            selected: Default::default(),
            last_drawn: None,
            fade: None,
            last_scale: 1.0,
            wheel: 0.0,
            config,
            slideshow: None,
            start_slideshow: start.slideshow,
            random: false,
            limits,
            jobs,
            page: 0,
            done,
            slots: HashMap::new(),
            pending: HashSet::new(),
            errors: HashMap::new(),
            zoom: None,
            pan: egui::Vec2::ZERO,
            started: Some(Instant::now()),
            dialog: None,
            hide_tools: false,
            kiosk: start.kiosk,
            kiosk_quit: false,
            watching: Arc::new(AtomicUsize::new(0)),
            changed_tx,
            changed_rx,
            stale: false,
            paused: false,
            back: Vec::new(),
            forward: Vec::new(),
            listing: None,
            total_size: 0,
            size_rx: None,
            freed_tx,
            freed_rx,
            selected_size: None,
            selected_rx: None,
            scroll_to_cur: true,
            thumbs: HashMap::new(),
            thumbs_pending: HashSet::new(),
            thumb_jobs,
            thumbs_done,
            thumb_view,
            pan_thumbs: Vec::new(),
            save_thumbs,
            exif_jobs,
            exif_done,
            exif_all: Vec::new(),
            exif_window: false,
            exif_asked: None,
            meta: None,
            focus_keywords: false,
            exif: None,
            cur_size: None,
            pane_info: Vec::new(),
            status_pane: 0,
            status_grid: [1, 1],
            status_place: None,
            path_text: String::new(),
            window: WindowState::default(),
            inhibitor: None,
            details: HashMap::new(),
            deleting: HashSet::new(),
            deleted_tx,
            deleted_rx,
            saved_tx,
            saved_rx,
            saving: HashSet::new(),
            busy: Arc::new(AtomicUsize::new(0)),
            search: String::new(),
            matches: None,
            cur_row: Default::default(),
            match_size: 0,
            search_error: None,
            match_rx: None,
            exif_index: None,
            exif_rx: None,
            anchor: None,
            orient: (0, false),
            opened: Instant::now(),
            max_pan: egui::Vec2::ZERO,
        };
        // A URI on the command line: start where Glimt would otherwise, then go there once mounted.
        match path.to_str().filter(|p| p.contains("://")) {
            Some(uri) => {
                app.open(&cc.egui_ctx, ".".into());
                app.mount(&cc.egui_ctx, uri.to_owned());
            }
            None if !start.more.is_empty() => {
                let all = std::iter::once(path).chain(start.more.iter().cloned());
                app.open_folders(&cc.egui_ctx, all.collect());
            }
            None => app.open(&cc.egui_ctx, path),
        }
        // Small folders are listed before the first frame; large ones hold startup back at most 50 ms.
        app.receive_listing(&cc.egui_ctx, std::time::Duration::from_millis(50));
        app
    }

    /// Open a folder, or the folder of a file with that file shown.
    fn open(&mut self, ctx: &egui::Context, path: PathBuf) {
        if collections::is_collection(&path) {
            return self.open_collection(ctx, path);
        }
        self.leave_image(ctx);
        self.collection = None;
        self.folders.clear();
        let (dir, file) = if path.is_dir() {
            (path, None)
        } else {
            (
                path.parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or(Path::new("."))
                    .to_path_buf(),
                Some(path),
            )
        };
        // Canonical path once here, so ⬆ .. and "remember folder" need no system calls later.
        let dir = dir.canonicalize().unwrap_or(dir);
        // Show a given file at once; the folder is listed in the background and the list merged in afterwards.
        let shown = file
            .as_deref()
            .and_then(Path::file_name)
            .map(OsString::from);
        self.opened_type = file.as_deref().map_or(Types::NONE, Types::of);
        self.subdirs.clear();
        self.deleting.clear();
        self.random = false;
        self.recursive = self.config.show_subfolders;
        self.thumbs.clear();
        self.thumbs_pending.clear();
        self.exif_index = None;
        self.exif_rx = None;
        self.dir = dir.clone();
        self.replace_files(ctx, shown.iter().cloned().collect(), shown);
        self.list(ctx);
        self.watch(ctx);
        self.title(ctx);
    }

    /// Show the collection in `file` as the list.
    fn open_collection(&mut self, ctx: &egui::Context, file: PathBuf) {
        let paths = match collections::load(&file) {
            Ok(paths) => paths,
            Err(e) => return self.notice = Some(format!("{}: {e}", file.display())),
        };
        self.leave_image(ctx);
        let root = Path::new("/");
        let files: Vec<OsString> = (paths.iter())
            .filter_map(|p| Some(p.strip_prefix(root).ok()?.as_os_str().to_owned()))
            .collect();
        self.subdirs.clear();
        self.deleting.clear();
        self.random = false;
        self.recursive = false;
        self.thumbs.clear();
        self.thumbs_pending.clear();
        self.exif_index = None;
        self.exif_rx = None;
        // No folder to watch: stop the watcher of the last one.
        self.watching.fetch_add(1, Ordering::Relaxed);
        self.dir = root.to_path_buf();
        self.collection_read = paths.into_iter().collect();
        self.collection = Some(file);
        self.folders.clear();
        let first = files.first().cloned();
        self.replace_files(ctx, files, first);
        self.list(ctx);
        self.title(ctx);
    }

    /// Show the images of several folders as one list, for a slideshow across them; one folder
    /// is opened as usual. A file stands for its folder.
    fn open_folders(&mut self, ctx: &egui::Context, paths: Vec<PathBuf>) {
        let first = paths.first().cloned();
        let mut folders: Vec<PathBuf> = Vec::new();
        for p in paths {
            let p = std::path::absolute(&p).unwrap_or(p);
            let dir = if p.is_dir() {
                p
            } else {
                p.parent().map_or(p.clone(), Path::to_path_buf)
            };
            if !folders.contains(&dir) {
                folders.push(dir);
            }
        }
        // One folder (several files of it, from a file manager): opened at the first file.
        if folders.len() < 2 {
            if let Some(first) = first {
                self.open(ctx, first);
            }
            return;
        }
        self.leave_image(ctx);
        self.collection = None;
        self.subdirs.clear();
        self.deleting.clear();
        self.random = false;
        self.thumbs.clear();
        self.thumbs_pending.clear();
        self.exif_index = None;
        self.exif_rx = None;
        // No one folder to watch: stop the watcher of the last one.
        self.watching.fetch_add(1, Ordering::Relaxed);
        self.dir = PathBuf::from("/");
        self.folders = folders;
        self.replace_files(ctx, Vec::new(), None);
        self.list(ctx);
        self.title(ctx);
    }

    /// Write the open collection with the list as it is now.
    fn save_collection(&mut self) {
        let Some(file) = &self.collection else { return };
        let ours: Vec<PathBuf> = (0..self.files.len()).map(|i| self.path(i)).collect();
        let on_disk = collections::load(file).unwrap_or_default();
        let paths = collections::merged(ours, &self.collection_read, on_disk);
        match collections::save(file, &paths) {
            Ok(()) => self.collection_read = paths.into_iter().collect(),
            Err(e) => self.notice = Some(format!("Could not save the collection: {e}")),
        }
    }

    /// Add the selected files, or the shown image, to the collection in `file`.
    fn add_to_collection(&mut self, file: &Path) {
        let paths: Vec<PathBuf> = self.targets().iter().map(|&i| self.path(i)).collect();
        let name = file.file_stem().unwrap_or_default().to_string_lossy();
        self.notice = Some(match collections::append(file, &paths) {
            Ok(n) => format!("{n} added to the collection \"{name}\""),
            Err(e) => format!("Could not save the collection: {e}"),
        });
    }

    /// File menu entries for collections; the ones listing collections read the folder only
    /// while open.
    fn collections_menu(&mut self, ui: &mut egui::Ui, act: &mut Option<Action>) {
        menu_item(ui, act, Action::NewCollection);
        menu_item(ui, act, Action::OpenCollection);
        let mut open = None;
        scroll_menu(ui, "Collections", |ui| {
            let all = collections::list();
            if all.is_empty() {
                ui.label("No collections yet");
            }
            for (name, file) in all {
                if ui.button(name).clicked() {
                    open = Some(file);
                }
            }
        });
        if !self.files.is_empty() {
            scroll_menu(ui, "Add to Collection", |ui| {
                let all = collections::list();
                if all.is_empty() {
                    ui.label("No collections yet");
                }
                for (name, file) in all {
                    if ui.button(name).clicked() {
                        self.add_to_collection(&file);
                    }
                }
            });
        }
        if self.collection.is_some() {
            menu_item(ui, act, Action::RemoveFromCollection);
        }
        if let Some(file) = open {
            self.visit(ui.ctx(), file);
        }
    }

    /// Read the folder (and then the sizes) in the background; `receive_listing` merges it in.
    /// A collection is checked for files that are gone instead.
    fn list(&mut self, ctx: &egui::Context) {
        let (tx, rx) = mpsc::channel();
        let (size_tx, size_rx) = mpsc::channel();
        let (list_dir, ctx2, types) = (self.dir.clone(), ctx.clone(), self.types());
        let c = &self.config;
        let (by, ascending, case) = (c.sort, c.sort_ascending, c.sort_case);
        let (recursive, shuffled) = (self.recursive, self.whole_shuffle);
        let collection = self.collection.is_some().then(|| self.files.clone());
        // A collection keeps its own order unless the Sort menu was used.
        let sort = collection.is_none() || self.sort_collection;
        let folders = self.folders.clone();
        std::thread::spawn(move || {
            // Several folders: their images, each folder (with its subfolders) after the other.
            if !folders.is_empty() {
                let rel = |dir: &Path, names: Vec<OsString>| -> Vec<OsString> {
                    let at = dir.strip_prefix("/").unwrap_or(dir).to_path_buf();
                    (names.into_iter())
                        .map(|n| at.join(n).into_os_string())
                        .collect()
                };
                let mut files: Vec<OsString> = (folders.iter())
                    .flat_map(|d| match recursive {
                        true => rel(d, list_tree(d, types)),
                        false => rel(d, list_folder(d, types).1),
                    })
                    .collect();
                sort_files(&list_dir, &mut files, by, ascending, case);
                if shuffled {
                    shuffle(&mut files);
                }
                // Sizes after, as for a folder: the list shows first.
                let paths: Vec<PathBuf> = files.iter().map(|f| list_dir.join(f)).collect();
                if tx.send((Vec::new(), files)).is_ok() {
                    ctx2.request_repaint();
                    let sizes = paths
                        .into_iter()
                        .filter_map(|p| Some(p.metadata().ok()?.len()));
                    send_sizes(sizes, size_tx, &ctx2);
                }
                return;
            }
            if let Some(mut files) = collection {
                files.retain(|f| list_dir.join(f).exists());
                if sort {
                    sort_files(&list_dir, &mut files, by, ascending, case);
                }
                let sizes: Vec<u64> = (files.iter())
                    .filter_map(|f| Some(list_dir.join(f).metadata().ok()?.len()))
                    .collect();
                if tx.send((Vec::new(), files)).is_ok() {
                    ctx2.request_repaint();
                    send_sizes(sizes.into_iter(), size_tx, &ctx2);
                }
                return;
            }
            let (dirs, mut files) = list_folder(&list_dir, types);
            if recursive {
                files = list_tree(&list_dir, types);
            }
            sort_files(&list_dir, &mut files, by, ascending, case);
            if shuffled {
                shuffle(&mut files);
            }
            // The receiver is gone if another folder was opened meanwhile; then we stop.
            if tx.send((dirs, files)).is_err() {
                return;
            }
            ctx2.request_repaint();
            // Sizes afterwards: read the folder once more and sum from the directory entries in
            // chunks, so the status bar fills in gradually without a copy of the whole list in memory.
            // A tree a folder at a time, unsorted.
            if recursive {
                let sizes = walk(&list_dir, types).flat_map(|(rel, files)| {
                    let folder = list_dir.join(rel);
                    (files.into_iter())
                        .filter_map(move |f| Some(folder.join(f).metadata().ok()?.len()))
                });
                return send_sizes(sizes, size_tx, &ctx2);
            }
            let images = std::fs::read_dir(&list_dir)
                .into_iter()
                .flatten()
                .flatten()
                .filter(|e| is_listed(&e.path(), types));
            let sizes = images.filter_map(|e| Some(e.metadata().ok()?.len()));
            send_sizes(sizes, size_tx, &ctx2);
        });
        self.listing = Some(rx);
        self.size_rx = Some(size_rx);
        self.total_size = 0;
        self.stale = false;
    }

    /// Poll the folder's modification time once a second in the background (one `stat`, no
    /// repaint unless it changed). Adding, removing or renaming a file changes it.
    /// The thread ends when another folder is opened.
    fn watch(&mut self, ctx: &egui::Context) {
        let generation = self.watching.fetch_add(1, Ordering::Relaxed) + 1;
        let (watching, tx, ctx, dir) = (
            self.watching.clone(),
            self.changed_tx.clone(),
            ctx.clone(),
            self.dir.clone(),
        );
        std::thread::spawn(move || {
            let mtime = || std::fs::metadata(&dir).and_then(|m| m.modified()).ok();
            let mut last = mtime();
            while watching.load(Ordering::Relaxed) == generation {
                std::thread::sleep(std::time::Duration::from_secs(1));
                let now = mtime();
                if now != last && watching.load(Ordering::Relaxed) == generation {
                    last = now;
                    let _ = tx.send(());
                    ctx.request_repaint();
                }
            }
        });
    }

    /// The file shown, or the folder when it has no images.
    fn location(&self) -> PathBuf {
        match self.files.get(self.cur) {
            Some(name) => self.dir.join(name),
            None => self.dir.clone(),
        }
    }

    /// Open a network location such as smb://server/share/folder, mounting it in the background.
    fn mount(&mut self, ctx: &egui::Context, uri: String) {
        let login = Login {
            uri,
            ..Login::default()
        };
        self.mount_as(ctx, login, false);
    }

    /// Mount `login.uri`, answering gio's questions from `login` if `answer`.
    fn mount_as(&mut self, ctx: &egui::Context, login: Login, answer: bool) {
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = gio_mount(&login.uri, answer.then_some(&login));
            let _ = tx.send((result, login));
            ctx.request_repaint();
        });
        self.mounting = Some(rx);
    }

    /// Open `path` as a new step in the Back/Forward history.
    fn visit(&mut self, ctx: &egui::Context, path: PathBuf) {
        self.back.push(self.location());
        self.forward.clear();
        self.open(ctx, path);
    }

    /// Take the folder listing if it is ready, waiting up to `wait`.
    fn receive_listing(&mut self, ctx: &egui::Context, wait: std::time::Duration) {
        let Some(Ok((dirs, files))) = self.listing.as_ref().map(|rx| rx.recv_timeout(wait)) else {
            return;
        };
        self.listing = None;
        if self.collection.is_none() {
            self.tree.insert(self.dir.clone(), Some(dirs.clone()));
        }
        self.subdirs = dirs;
        // Files still being deleted in the background must not come back with a refresh.
        let mut files = files;
        if !self.deleting.is_empty() {
            files.retain(|f| !self.deleting.contains(f));
        }
        // A refresh during random order keeps the order, so Previous still goes back
        // through the images that were shown; a list shuffled whole in the thread stays as it is.
        let files = if self.random && !std::mem::take(&mut self.whole_shuffle) {
            merge_shuffled(&self.files, files)
        } else {
            files
        };
        let shown = self.files.get(self.cur).cloned();
        self.replace_files(ctx, files, shown);
        if std::mem::take(&mut self.sort_collection) {
            self.save_collection();
        }
    }

    /// To the trash at once, permanently after confirmation.
    fn request_delete(&mut self, ctx: &egui::Context, trash: bool) {
        if self.files.is_empty() {
            return;
        }
        if if trash {
            self.config.confirm_trash
        } else {
            self.config.confirm_delete
        } {
            self.asked_for = (self.files_gen, self.cur);
            self.confirm = Some(trash);
        } else {
            self.delete_targets_now(ctx, trash);
        }
    }

    /// The list and the shown image are still those a dialog asked about; else a notice.
    fn still_asked(&mut self) -> bool {
        let same = self.asked_for == (self.files_gen, self.cur);
        if !same {
            self.notice =
                Some("The list changed while the dialog was open: nothing was done".into());
        }
        same
    }

    /// File operations act on the selected files, otherwise on the shown image. Sorted.
    fn targets(&self) -> Vec<usize> {
        if self.selected.is_empty() {
            vec![self.cur]
        } else {
            self.selected.iter().copied().collect()
        }
    }

    fn delete_targets_now(&mut self, ctx: &egui::Context, trash: bool) {
        let targets = self.targets();
        let paths: Vec<PathBuf> = targets.iter().map(|&i| self.path(i)).collect();
        self.deleting
            .extend(targets.iter().map(|&i| self.files[i].clone()));
        self.remove_indices(ctx, &targets);
        let (tx, ctx) = (self.notices_tx.clone(), ctx.clone());
        let (freed_tx, deleted_tx) = (self.freed_tx.clone(), self.deleted_tx.clone());
        let busy = self.busy.clone();
        busy.fetch_add(1, Ordering::Relaxed);
        std::thread::spawn(move || {
            // Read the size before deleting, so the status bar can subtract it.
            let _ = freed_tx.send(total_size(&paths));
            ctx.request_repaint();
            let mut errors = Vec::new();
            if trash {
                // Many files per gio call, so thousands of files do not become thousands of processes.
                for chunk in paths.chunks(500) {
                    match command("gio").arg("trash").arg("--").args(chunk).output() {
                        Ok(o) if o.status.success() => {}
                        Ok(o) => errors.push(String::from_utf8_lossy(&o.stderr).trim().to_owned()),
                        Err(e) => {
                            errors.push(format!("gio trash (from glib): {e}"));
                            break;
                        }
                    }
                }
            } else {
                for path in &paths {
                    if let Err(e) = std::fs::remove_file(path) {
                        errors.push(format!("{}: {e}", path.display()));
                    }
                }
            }
            if let Some(first) = errors.first() {
                let _ = tx.send(format!("Delete failed ({} errors): {first}", errors.len()));
            }
            // Their sidecars go the same way; a new file of the same name must not get their
            // keywords.
            let sides: Vec<PathBuf> = (paths.iter().filter(|p| !p.exists()))
                .map(|p| meta::sidecar(p))
                .filter(|s| s.exists())
                .collect();
            for chunk in sides.chunks(500) {
                if trash {
                    let _ = command("gio").arg("trash").arg("--").args(chunk).output();
                } else {
                    chunk.iter().for_each(|s| drop(std::fs::remove_file(s)));
                }
            }
            let _ = deleted_tx.send((paths, !errors.is_empty()));
            busy.fetch_sub(1, Ordering::Relaxed);
            ctx.request_repaint();
        });
    }

    /// Remove the (sorted) indices from the list in one pass and shift index-based state
    /// accordingly. Neighbours' textures are kept, so the next image shows without decoding again.
    fn remove_indices(&mut self, ctx: &egui::Context, removed: &[usize]) {
        if removed.binary_search(&self.cur).is_ok() {
            // The shown image is gone, and its rotation with it.
            self.orient = (0, false);
            self.page = 0;
        } else {
            self.leave_image(ctx);
        }
        let mut idx = 0;
        self.files.retain(|_| {
            idx += 1;
            removed.binary_search(&(idx - 1)).is_err()
        });
        self.files_gen += 1;
        self.slots = without_indices(std::mem::take(&mut self.slots), removed);
        self.errors = without_indices(std::mem::take(&mut self.errors), removed);
        self.selected.clear();
        self.anchor = None;
        self.update_search(ctx);
        // Jobs in flight have old indices and are rejected by path; refill requests them again.
        self.pending.clear();
        self.save_collection();
        // The next remaining file after the shown one (or the last).
        let cur = self.cur - removed.partition_point(|&r| r < self.cur);
        if self.files.is_empty() {
            self.cur = 0;
            self.title(ctx);
        } else {
            self.go(ctx, cur.min(self.files.len() - 1));
        }
    }

    fn receive_deleted(&mut self) {
        for (paths, failed) in self.deleted_rx.try_iter() {
            if let Some(dups) = &mut self.dups {
                // Those that failed are still there.
                dups.remove(
                    &paths
                        .iter()
                        .filter(|p| !p.exists())
                        .map(PathBuf::as_path)
                        .collect(),
                );
            }
            // By the name the list has, with its folders in the list with subfolders and in a
            // collection.
            for rel in paths.iter().filter_map(|p| p.strip_prefix(&self.dir).ok()) {
                self.deleting.remove(rel.as_os_str());
            }
            // The files that are still there come back into the list. A listing that started
            // before the files went still has them: the folder is read again after it.
            self.stale |= failed || self.listing.is_some();
        }
    }

    fn receive_sizes(&mut self, ctx: &egui::Context) {
        while let Ok(freed) = self.freed_rx.try_recv() {
            // ponytail: deleting while the sum is still being computed can make it slightly off.
            self.total_size = self.total_size.saturating_sub(freed);
        }
        if let Some(Ok(size)) = self.selected_rx.as_ref().map(mpsc::Receiver::try_recv) {
            self.selected_size = Some(size);
            self.selected_rx = None;
        }
        drain(&mut self.match_rx, &mut self.match_size);
        if let Some(index) = self.exif_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.exif_rx = None;
            self.exif_index = Some(index);
            self.update_search(ctx);
        }
        drain(&mut self.size_rx, &mut self.total_size);
    }

    /// Called when the selection changes: update the title and compute the size in the background.
    fn selection_changed(&mut self, ctx: &egui::Context) {
        self.title(ctx);
        if self.compare {
            self.refill();
        }
        self.selected_size = None;
        self.selected_rx = None;
        if self.selected.is_empty() {
            return;
        }
        let paths: Vec<PathBuf> = self.selected.iter().map(|&i| self.path(i)).collect();
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            // The receiver is gone if the selection changed again; then the answer is dropped.
            if tx.send(total_size(&paths)).is_ok() {
                ctx.request_repaint();
            }
        });
        self.selected_rx = Some(rx);
    }

    /// Replace the file list and go to `shown` (otherwise the first image). The texture shown now
    /// is kept if it was made from the same file. All index-based state is reset; jobs in flight
    /// are rejected by path when they arrive.
    fn replace_files(
        &mut self,
        ctx: &egui::Context,
        files: Vec<OsString>,
        shown: Option<OsString>,
    ) {
        self.leave_image(ctx);
        let slot = self.slots.remove(&self.cur);
        self.files = files;
        self.files_gen += 1;
        self.details.clear();
        self.anchor = None;
        self.selected.clear();
        self.slots.clear();
        self.pending.clear();
        self.errors.clear();
        let cur = shown
            .and_then(|p| self.files.iter().position(|f| *f == p))
            .unwrap_or(0);
        if let Some(slot) = slot.filter(|s| self.is_file(cur, &s.path)) {
            self.slots.insert(cur, slot);
        }
        self.update_search(ctx);
        // go() clears the notice for a new image; a new list is not one, and a notice is often
        // why the list was read again.
        let notice = self.notice.take();
        self.go(ctx, cur);
        self.notice = notice;
    }

    /// Run zenity's (or kdialog's) file dialog in its own thread so the window does not freeze.
    fn pick(&mut self, ctx: &egui::Context, folder: bool, purpose: Pick) {
        use std::os::unix::ffi::OsStringExt;
        let (tx, rx) = mpsc::channel();
        let start = match &purpose {
            // A name for the file to save: the first image's, as a PDF.
            Pick::SavePdf(job) => self.dir.join(
                (job.paths.first())
                    .map_or("print".into(), |p| p.with_extension("pdf"))
                    .file_name()
                    .unwrap_or_default(),
            ),
            // Made first: the file dialog cannot start in a folder that is not there.
            Pick::Collection => (collections::dir())
                .inspect(|d| drop(std::fs::create_dir_all(d)))
                .unwrap_or_default()
                .join(""),
            // Where colord and the user keep them.
            Pick::ScreenProfile | Pick::InputProfile => {
                (xdg_dir("XDG_DATA_HOME", ".local/share", "icc"))
                    .filter(|d| d.is_dir())
                    .unwrap_or_else(|| "/usr/share/color/icc".into())
                    .join("")
            }
            _ => self.dir.join(""),
        };
        let save = matches!(purpose, Pick::SavePdf(_));
        let several = matches!(purpose, Pick::Folders);
        let ctx = ctx.clone();
        let title = match purpose {
            Pick::Open if folder => "Open Folder",
            Pick::Open => "Open File",
            Pick::Folders => "Open Folders (Ctrl+click for several)",
            Pick::Collection => "Open Collection",
            Pick::CopyTo(_) => "Copy to Folder",
            Pick::MoveTo(_) => "Move to Folder",
            Pick::SavePdf(_) => "Save as PDF",
            Pick::SortFolder => "Add to the Sort Manager",
            Pick::ScreenProfile => "Screen Profile",
            Pick::InputProfile => "Profile of Images Without One",
        };
        let notices = self.notices_tx.clone();
        std::thread::spawn(move || {
            let zenity = || {
                let mut cmd = command("zenity");
                cmd.arg("--file-selection").arg("--title").arg(title);
                cmd.arg("--filename").arg(&start);
                if save {
                    cmd.arg("--save").arg("--confirm-overwrite");
                }
                if folder {
                    cmd.arg("--directory");
                }
                if several {
                    cmd.arg("--multiple").arg("--separator=\n");
                }
                cmd.output()
            };
            // KDE desktops often have kdialog but not zenity.
            let kdialog = || {
                let mode = if save {
                    "--getsavefilename"
                } else if folder {
                    "--getexistingdirectory"
                } else {
                    "--getopenfilename"
                };
                let mut cmd = command("kdialog");
                cmd.arg("--title").arg(title).arg(mode).arg(&start);
                cmd.output()
            };
            let output = match zenity() {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => kdialog(),
                other => other,
            };
            let picked = match output {
                Err(e) => {
                    let _ =
                        notices.send(format!("No file dialog (install zenity or kdialog): {e}"));
                    None
                }
                // Cancel exits with status 1 in both.
                Ok(o) if !o.status.success() => None,
                Ok(mut o) => {
                    o.stdout.pop_if(|b| *b == b'\n');
                    Some(PathBuf::from(std::ffi::OsString::from_vec(o.stdout)))
                }
            };
            let _ = tx.send(picked);
            ctx.request_repaint();
        });
        self.dialog = Some((purpose, rx));
    }

    /// Copy or move `paths` into `dest` in the background. Existing files are never
    /// overwritten; a move across file systems falls back to copy + delete.
    fn transfer(&mut self, ctx: &egui::Context, paths: Vec<PathBuf>, dest: PathBuf, keep: bool) {
        if !keep && self.collection.is_some() {
            // The collection follows the files to their new place.
            // ponytail: before the move is done; a file that fails to move drops out of the
            // collection at the next refresh.
            let moving: HashSet<&PathBuf> = paths.iter().collect();
            for i in 0..self.files.len() {
                let path = self.path(i);
                let to = dest.join(path.file_name().unwrap_or_default());
                // A file there already stays, and this one is not moved: its entry stays too.
                if moving.contains(&path)
                    && !to.exists()
                    && let Ok(rel) = to.strip_prefix(&self.dir)
                {
                    self.files[i] = rel.as_os_str().to_owned();
                }
            }
            self.save_collection();
        } else if !keep {
            // Moved out of its folder: out of the list at once, as when deleting. Names in the
            // list are relative to the folder, also those in subfolders.
            let names: HashSet<&std::ffi::OsStr> = paths
                .iter()
                .filter(|p| p.parent() != Some(dest.as_path()))
                .filter_map(|p| p.strip_prefix(&self.dir).ok())
                .map(Path::as_os_str)
                .collect();
            // Kept out of a listing that is running, as files being deleted are.
            self.deleting.extend(names.iter().map(|n| n.to_os_string()));
            let gone: Vec<usize> = (0..self.files.len())
                .filter(|&i| names.contains(self.files[i].as_os_str()))
                .collect();
            self.remove_indices(ctx, &gone);
        }
        let (tx, freed_tx, ctx) = (self.notices_tx.clone(), self.freed_tx.clone(), ctx.clone());
        let deleted_tx = self.deleted_tx.clone();
        let same_dir = paths.first().and_then(|p| p.parent()) == Some(self.dir.as_path());
        // Into the folder, or one below it in the list with subfolders: the files join the list.
        let listed = dest == self.dir || (self.recursive && dest.starts_with(&self.dir));
        let busy = self.busy.clone();
        busy.fetch_add(1, Ordering::Relaxed);
        std::thread::spawn(move || {
            if !keep && same_dir {
                let _ = freed_tx.send(total_size(&paths));
            }
            let mut errors = Vec::new();
            for path in &paths {
                let target = dest.join(path.file_name().unwrap_or_default());
                let result = if target.exists() {
                    Err(std::io::ErrorKind::AlreadyExists.into())
                } else if keep {
                    std::fs::copy(path, &target).map(drop)
                } else {
                    std::fs::rename(path, &target).or_else(|e| {
                        if e.kind() != std::io::ErrorKind::CrossesDevices {
                            return Err(e);
                        }
                        std::fs::copy(path, &target).and_then(|_| std::fs::remove_file(path))
                    })
                };
                match result {
                    Ok(()) => meta::follow(path, &target, keep),
                    Err(e) => errors.push(format!("{}: {e}", target.display())),
                }
            }
            if let Some(first) = errors.first() {
                let verb = if keep { "Copy" } else { "Move" };
                let _ = tx.send(format!("{verb} failed ({} errors): {first}", errors.len()));
            }
            // Moved: out of `deleting`; read the folder again for files that joined it, or that
            // failed to go.
            if listed || !keep {
                let relist = listed || !errors.is_empty();
                let _ = deleted_tx.send((if keep { Vec::new() } else { paths }, relist));
            }
            busy.fetch_sub(1, Ordering::Relaxed);
            ctx.request_repaint();
        });
    }

    /// Print `job` in the background.
    fn print(&mut self, ctx: &egui::Context, job: PrintJob, to: PrintTo) {
        let (tx, busy, ctx) = (self.notices_tx.clone(), self.busy.clone(), ctx.clone());
        busy.fetch_add(1, Ordering::Relaxed);
        std::thread::spawn(move || {
            let max = print::max_pixels(job.paper, job.per_page);
            let failed = std::cell::Cell::new(0);
            let pages = job.paths.iter().filter_map(|path| {
                let img = load(path, job.rotate).and_then(|img| shrink(img, [max, max]));
                failed.set(failed.get() + img.is_err() as usize);
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                Some((img.ok()?, job.caption.then(|| name.into_owned())))
            });
            // Written as it is made, so only one page is in memory, not the whole PDF.
            let title = format!("Glimt: {} images", job.paths.len());
            let done = match &to {
                PrintTo::Printer(printer) => print::send(printer, job.copies, &title, |w| {
                    print::pdf(pages, job.paper, job.per_page, w)
                }),
                PrintTo::File(file) => std::fs::File::create(file)
                    .and_then(|f| {
                        let out = std::io::BufWriter::new(f);
                        print::pdf(pages, job.paper, job.per_page, out)
                    })
                    .map_err(|e| e.to_string()),
            };
            let n = job.paths.len() - failed.get();
            let done = done.map(|()| match &to {
                PrintTo::Printer(printer) => format!("{n} pages sent to {printer}"),
                PrintTo::File(file) => format!("{n} pages saved in {}", file.display()),
            });
            let _ = tx.send(match (done, failed.get()) {
                (Ok(msg), 0) => msg,
                (Ok(msg), f) => format!("{msg}; {f} could not be read"),
                (Err(e), _) => format!("Printing failed: {e}"),
            });
            busy.fetch_sub(1, Ordering::Relaxed);
            ctx.request_repaint();
        });
    }

    /// Image `i`'s name as shown: relative to the folder, or in a collection the file name.
    fn name(&self, i: usize) -> std::borrow::Cow<'_, str> {
        let name = self.files[i].as_os_str();
        // Several folders: the folder and the file, "Holiday/IMG_0012.jpg".
        if !self.folders.is_empty() {
            let path = Path::new(name);
            let last = |p: &Path| {
                p.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            };
            let folder = path.parent().map(last).unwrap_or_default();
            return format!("{folder}/{}", last(path)).into();
        }
        match self.collection {
            Some(_) => Path::new(name)
                .file_name()
                .unwrap_or(name)
                .to_string_lossy(),
            None => name.to_string_lossy(),
        }
    }

    /// Full path of image `i`.
    fn path(&self, i: usize) -> PathBuf {
        self.dir.join(&self.files[i])
    }

    /// Is `path` image number `i` in the list? (Without building the path.)
    fn is_file(&self, i: usize, path: &Path) -> bool {
        // Names are relative to the folder; in a slideshow with subfolders they have folders too.
        self.files.get(i).is_some_and(|name| {
            path.strip_prefix(&self.dir)
                .is_ok_and(|rest| rest.as_os_str() == name)
        })
    }

    fn request(&mut self, idx: usize, full: bool) {
        let have = self.slots.get(&idx).is_some_and(|s| s.full || !full);
        if !have && !self.errors.contains_key(&idx) && self.pending.insert((idx, full)) {
            // The neighbours are preloaded at their first page.
            let page = if idx == self.cur { self.page } else { 0 };
            let _ = self.jobs.send((idx, full, self.path(idx), page));
        }
    }

    fn go(&mut self, ctx: &egui::Context, idx: usize) {
        if self.files.is_empty() {
            return;
        }
        // Browsing by hand shows the image at once; the slideshow sets a fade after this.
        self.fade = None;
        // Saving the old image's rotation may leave a notice, which must outlive the move.
        let saved = self.config.save_orientation && self.orient != (0, false);
        self.leave_image(ctx);
        let notice = if saved { self.notice.take() } else { None };
        self.cur = idx.min(self.files.len() - 1);
        self.notice = notice;
        // Geeqie's "keep zoom": the next image at the same zoom and place, to compare a series.
        if !self.config.keep_zoom {
            self.zoom = None;
            self.pan = egui::Vec2::ZERO;
        }
        self.opened = Instant::now();
        self.scroll_to_cur = true;
        // Every image gets the full delay, also when the user moves back or forward by hand.
        if self.slideshow.is_some() {
            self.slideshow = Some(Instant::now());
        }
        self.refill();
        self.title(ctx);
    }

    /// The shown image is left: its rotation saved (with Save Orientation on) or dropped, and a
    /// TIFF page other than the first let go. Called while `dir` and `cur` still name the image,
    /// so before the folder or the list changes.
    fn leave_image(&mut self, ctx: &egui::Context) {
        // A TIFF shown at another page than the first: its decoded page must not stay for when
        // it is shown again from the first page.
        if self.page > 0 {
            self.slots.remove(&self.cur);
            self.page = 0;
        }
        if self.config.save_orientation && self.orient != (0, false) {
            self.save_orientation(ctx);
        }
        self.orient = (0, false);
    }

    /// Shuffle the file list (current image first) or sort it back, keeping the shown image.
    fn set_random(&mut self, ctx: &egui::Context, on: bool) {
        if self.files.is_empty() {
            return;
        }
        self.leave_image(ctx);
        self.random = on;
        if !on {
            // Back to the chosen sort order; listing keeps the shown image.
            self.list(ctx);
            return;
        }
        let mut files = std::mem::take(&mut self.files);
        let shown = files[self.cur].clone();
        files.swap(0, self.cur);
        shuffle(&mut files[1..]);
        self.replace_files(ctx, files, Some(shown));
    }

    fn start_slideshow(&mut self, ctx: &egui::Context) {
        if self.config.slideshow_subfolders {
            // The images of the subfolders too, listed in the background; random order
            // shuffles them all together.
            self.recursive = true;
            self.random = self.config.random;
            self.whole_shuffle = self.random;
            self.list(ctx);
        } else if self.config.random != self.random {
            self.set_random(ctx, self.config.random);
        }
        // The first slide comes at once: start as if the delay has already run out.
        let delay = std::time::Duration::from_secs(self.config.delay);
        self.slideshow = Some(
            Instant::now()
                .checked_sub(delay)
                .unwrap_or_else(Instant::now),
        );
        self.title(ctx);
        ctx.request_repaint();
    }

    /// Open the shown file in `player` (a command line), or in the default application when
    /// `player` is empty. Runs in its own thread, which waits for the program to exit.
    fn launch(&self, player: &str) {
        if self.files.is_empty() {
            return;
        }
        let (path, tx) = (self.path(self.cur), self.notices_tx.clone());
        let mut words: Vec<String> = player.split_whitespace().map(String::from).collect();
        std::thread::spawn(move || {
            let run = |cmd: &str, args: &[String]| {
                let out = command(cmd).args(args).arg(&path).output();
                out.map_err(|e| format!("{cmd}: {e}"))
            };
            let out = if words.is_empty() {
                // gio is part of glib; xdg-open covers systems without it.
                run("gio", &["open".into()]).or_else(|_| run("xdg-open", &[]))
            } else {
                let cmd = words.remove(0);
                run(&cmd, &words)
            };
            match out {
                Ok(o) if o.status.success() => {}
                Ok(o) => drop(tx.send(String::from_utf8_lossy(&o.stderr).trim().into())),
                Err(e) => drop(tx.send(e)),
            }
        });
    }

    /// Edit → Plugins: run one on the selected files, or the shown one, in the background;
    /// they are read again afterwards, as plugins change them.
    fn plugins_menu(&mut self, ui: &mut egui::Ui) {
        scroll_menu(ui, "Plugins", |ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
            let dir = xdg_dir("XDG_DATA_HOME", ".local/share", "glimt").map(|d| d.join("plugins"));
            if self.plugins.is_empty() {
                ui.weak("No plugins");
            }
            let mut chosen = None;
            for (k, plugin) in self.plugins.iter().enumerate() {
                if ui.button(&plugin.name).clicked() {
                    chosen = Some(k);
                }
            }
            ui.separator();
            if ui
                .button("Open the Plugins Folder")
                .on_hover_text(
                    "Desktop files there, as Geeqie's, show in this menu after a restart",
                )
                .clicked()
                && let Some(dir) = dir
            {
                let _ = std::fs::create_dir_all(&dir);
                open_url(&dir.to_string_lossy());
            }
            let Some(k) = chosen.filter(|_| !self.files.is_empty()) else {
                return;
            };
            let files: Vec<PathBuf> = self.targets().iter().map(|&i| self.path(i)).collect();
            let plugin = self.plugins[k].clone();
            let (dir, ctx) = (self.dir.clone(), ui.ctx().clone());
            let (tx, saved, deleted) = (
                self.notices_tx.clone(),
                self.saved_tx.clone(),
                self.deleted_tx.clone(),
            );
            let busy = self.busy.clone();
            busy.fetch_add(1, Ordering::Relaxed);
            std::thread::spawn(move || {
                if let Err(e) = plugin.run(&files, &dir) {
                    let _ = tx.send(format!("{}: {e}", plugin.name));
                }
                for f in files {
                    let _ = saved.send((f, Ok(())));
                }
                let _ = deleted.send((Vec::new(), true));
                busy.fetch_sub(1, Ordering::Relaxed);
                ctx.request_repaint();
            });
        });
    }

    /// "Open With" submenu: the applications registered for the shown file's type; the chosen
    /// one opens the selected files (or the shown one).
    fn open_with_menu(&mut self, ui: &mut egui::Ui) {
        let Some(name) = self.files.get(self.cur) else {
            return;
        };
        let ext = Path::new(name)
            .extension()
            .map_or(String::new(), |e| e.to_string_lossy().to_lowercase());
        let path = self.path(self.cur);
        scroll_menu(ui, "Open With", |ui| {
            // The menu keeps the width of its first frame, "…": names must not wrap to it.
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
            for (ext, apps) in self.apps_rx.try_iter() {
                self.apps.insert(ext, Some(apps));
            }
            // apps_for runs gio twice: in the background, so the menu never stalls.
            let apps = self.apps.entry(ext.clone()).or_insert_with(|| {
                let (tx, ctx) = (self.apps_tx.clone(), ui.ctx().clone());
                std::thread::spawn(move || {
                    let _ = tx.send((ext, apps_for(&path)));
                    ctx.request_repaint();
                });
                None
            });
            let Some(apps) = apps else {
                ui.label("…");
                return;
            };
            if apps.is_empty() {
                ui.label("No applications found");
            }
            let mut chosen = None;
            for (name, desktop) in apps.iter() {
                if ui.button(name).clicked() {
                    chosen = Some(desktop.clone());
                }
            }
            if let Some(desktop) = chosen {
                let files: Vec<PathBuf> = self.targets().iter().map(|&i| self.path(i)).collect();
                let tx = self.notices_tx.clone();
                std::thread::spawn(move || {
                    let out = command("gio")
                        .arg("launch")
                        .arg(&desktop)
                        .args(&files)
                        .output();
                    match out {
                        Ok(o) if o.status.success() => {}
                        Ok(o) => drop(tx.send(String::from_utf8_lossy(&o.stderr).trim().into())),
                        Err(e) => drop(tx.send(format!("gio: {e}"))),
                    }
                });
            }
        });
    }

    /// The chosen theme, read from its file again, with the desktop's look where it sets none.
    fn apply_theme(&self, ctx: &egui::Context) {
        let (dark, accent) = self.desktop_look;
        theme::apply(ctx, &theme::load(&self.config.theme), dark, accent);
    }

    /// View → Theme: the themes to choose from, then where to make and find more.
    fn theme_menu(&mut self, ui: &mut egui::Ui) {
        for name in theme::names() {
            // Chosen again too, so a theme file just edited shows.
            if ui.radio(self.config.theme == name, &name).clicked() {
                self.config.theme = name;
                self.config.save();
                self.apply_theme(ui.ctx());
            }
        }
        ui.separator();
        self.theme_buttons(ui);
    }

    /// Save as Theme…, Open Themes Folder and Get More Themes…, in View → Theme and Preferences.
    fn theme_buttons(&mut self, ui: &mut egui::Ui) {
        if ui.button("Save as Theme…").clicked() {
            self.prompt = Some((Prompt::SaveTheme, String::new()));
        }
        if ui.button("Open Themes Folder").clicked()
            && let Some(dir) = theme::dir()
        {
            let _ = std::fs::create_dir_all(&dir);
            open_url(&dir.to_string_lossy());
        }
        if ui.button("Get More Themes…").clicked() {
            open_url("https://github.com/thornbux/glimt/tree/main/themes");
        }
    }

    /// Preferences or a modal dialog is open: keys go there.
    fn dialog_open(&self) -> bool {
        self.show_settings
            || self.confirm.is_some()
            || self.confirm_folder
            || self.prompt.is_some()
            || self.login.is_some()
            || self.print.is_some()
    }

    /// The text dialog was answered with `text`.
    fn answer_prompt(&mut self, ctx: &egui::Context, prompt: Prompt, text: &str) {
        if let Prompt::NewCollection = prompt {
            return match collections::new_file(text) {
                Ok(file) => self.add_to_collection(&file),
                Err(e) => self.notice = Some(e),
            };
        }
        if text.is_empty() || text.contains('/') || text == "." || text == ".." {
            self.notice = Some(format!("Not a valid name: \"{text}\""));
            return;
        }
        if let Prompt::SaveTheme = prompt {
            let file = theme::dir().map(|d| d.join(format!("{text}.theme")));
            let saved = file.ok_or("no home folder".to_owned()).and_then(|f| {
                let _ = std::fs::create_dir_all(f.parent().unwrap_or(&f));
                std::fs::write(&f, theme::Theme::file_from(ctx))
                    .map(|_| f)
                    .map_err(|e| e.to_string())
            });
            self.notice = Some(match saved {
                Ok(f) => {
                    self.config.theme = text.to_owned();
                    self.config.save();
                    format!("Saved as {}: edit it, then choose it again", f.display())
                }
                Err(e) => format!("Could not save the theme: {e}"),
            });
            return;
        }
        if let Prompt::RenameFolder = prompt {
            return self.rename_folder(ctx, text);
        }
        if matches!(prompt, Prompt::Rename | Prompt::RenameMany) && !self.still_asked() {
            return;
        }
        // A renamed file stays in its own folder, also a subfolder in the list with subfolders.
        let target = match prompt {
            Prompt::Rename if !self.files.is_empty() => self.path(self.cur).with_file_name(text),
            _ => self.dir.join(text),
        };
        let result = match prompt {
            Prompt::NewFolder => {
                self.tree.remove(&self.dir);
                std::fs::create_dir(&target)
            }
            Prompt::Rename if self.files.is_empty() => return,
            Prompt::Rename if target == self.path(self.cur) => return,
            // rename() would silently replace an existing file.
            Prompt::Rename if target.exists() => Err(std::io::ErrorKind::AlreadyExists.into()),
            Prompt::RenameMany => return self.rename_many(ctx, text),
            Prompt::NewCollection | Prompt::SaveTheme | Prompt::RenameFolder => return,
            Prompt::Rename => std::fs::rename(self.path(self.cur), &target)
                .inspect(|_| meta::follow(&self.path(self.cur), &target, false)),
        };
        match (result, prompt) {
            (Ok(()), Prompt::NewFolder) => self.list(ctx),
            // The list keeps its folder and mode; the shown entry takes the new name.
            (Ok(()), _) => {
                if let Ok(rel) = target.strip_prefix(&self.dir) {
                    self.files[self.cur] = rel.as_os_str().to_owned();
                }
                self.save_collection();
                self.title(ctx);
                self.list(ctx);
            }
            (Err(e), _) => self.notice = Some(format!("{}: {e}", target.display())),
        }
    }

    /// Write the view's rotation into the shown file and show it as it is now.
    /// ponytail: a JPEG in the UI thread, as it is read and written in milliseconds.
    fn save_orientation(&mut self, ctx: &egui::Context) {
        let (turns, mirrored) = self.orient;
        if self.files.is_empty() || (turns, mirrored) == (0, false) {
            return;
        }
        let path = self.path(self.cur);
        // Being saved already, with this rotation: the rotate keys wait for the save.
        if self.saving.contains(&path) {
            return;
        }
        // JPEG keeps its pixels and gets an orientation field; PNG and WebP are turned, which
        // takes seconds for a large one, so in the background.
        if !has_ext(&path, &["jpg", "jpeg"]) {
            let (rotate, quality) = (self.config.exif_rotate, self.config.webp_quality);
            let (saved, busy, ctx) = (self.saved_tx.clone(), self.busy.clone(), ctx.clone());
            self.saving.insert(path.clone());
            busy.fetch_add(1, Ordering::Relaxed);
            std::thread::spawn(move || {
                let result = survive(|| save_rotated(&path, rotate, turns, mirrored, quality));
                let _ = saved.send((path, result));
                busy.fetch_sub(1, Ordering::Relaxed);
                ctx.request_repaint();
            });
            return;
        }
        if let Err(e) = exif::write_orientation(&path, turns, mirrored) {
            self.notice = Some(format!("Could not save the orientation: {e}"));
            return;
        }
        if !self.config.exif_rotate {
            self.notice = Some("Saved; it shows when Auto Rotate Using EXIF is on".into());
        }
        self.saved(&path);
    }

    /// `path` was saved turned: decode it again and make a new thumbnail.
    fn saved(&mut self, path: &Path) {
        if self.is_file(self.cur, path) {
            self.orient = (0, false);
            self.pending.retain(|p| p.0 != self.cur);
            self.exif = None;
        }
        self.slots.retain(|_, s| s.path != path);
        // Its size changed: the status bar and the list read it again.
        self.cur_size = None;
        if let Ok(name) = path.strip_prefix(&self.dir) {
            self.thumbs.remove(name.as_os_str());
            self.thumbs_pending.remove(name.as_os_str());
            self.details.remove(name.as_os_str());
        }
        self.refill();
    }

    /// The thumbnail box in the file list, by the size chosen in Preferences.
    fn thumb(&self) -> egui::Vec2 {
        THUMBS[self.config.thumb_size.min(2) as usize]
    }

    /// A cell of the icon grid: the thumbnail with its name below.
    fn cell(&self) -> egui::Vec2 {
        self.thumb() + CELL_EXTRA
    }

    fn set_thumb_size(&mut self, size: u8) {
        self.config.thumb_size = size;
        self.config.save();
        LARGE_THUMBS.store(size == 2, Ordering::Relaxed);
        self.thumbs.clear();
        self.thumbs_pending.clear();
        self.scroll_to_cur = true;
    }

    /// Rename the selected files by `pattern`, numbered in list order. Nothing is overwritten:
    /// if a new name is taken by a file outside the selection, nothing is renamed. When new
    /// names swap with old ones, the files go through temporary names.
    fn rename_many(&mut self, ctx: &egui::Context, pattern: &str) {
        let from: Vec<PathBuf> = self.targets().iter().map(|&i| self.path(i)).collect();
        let to: Vec<PathBuf> = (from.iter().enumerate())
            .map(|(k, p)| p.with_file_name(pattern_name(pattern, p, k + 1)))
            .collect();
        let unique: HashSet<&PathBuf> = to.iter().collect();
        if unique.len() < to.len() {
            self.notice =
                Some("The pattern gives several files the same name; add # for a number".into());
            return;
        }
        let sources: HashSet<&PathBuf> = from.iter().collect();
        if let Some(taken) = to.iter().find(|t| t.exists() && !sources.contains(t)) {
            self.notice = Some(format!(
                "{} exists already; nothing renamed",
                taken.display()
            ));
            return;
        }
        let swap = to.iter().any(|t| sources.contains(t));
        // Sidecars go with their files, through the same steps.
        let (mut from, mut to) = (from, to);
        let sides: Vec<(PathBuf, PathBuf)> = (from.iter().zip(&to))
            .map(|(f, t)| (meta::sidecar(f), meta::sidecar(t)))
            .filter(|(f, _)| f.exists())
            .collect();
        for (f, t) in sides {
            from.push(f);
            to.push(t);
        }
        // Next to the file: in a collection the files are in different folders.
        let tmp = |f: &Path, k: usize| {
            f.with_file_name(format!(".glimt-rename-{}-{k}", std::process::id()))
        };
        let mut moves: Vec<(PathBuf, PathBuf)> = Vec::new();
        if swap {
            moves.extend(from.iter().enumerate().map(|(k, f)| (f.clone(), tmp(f, k))));
            moves.extend(to.iter().enumerate().map(|(k, t)| (tmp(t, k), t.clone())));
        } else {
            moves.extend(from.iter().cloned().zip(to.iter().cloned()));
        }
        let mut failed = false;
        for (k, (a, b)) in moves.iter().enumerate() {
            if let Err(e) = std::fs::rename(a, b) {
                // Named by the file and its new name, not a temporary one.
                let j = k % from.len();
                let name = |p: &PathBuf| p.file_name().unwrap_or_default().display().to_string();
                self.notice = Some(format!(
                    "Could not rename {} to {}: {e}; nothing renamed",
                    name(&from[j]),
                    name(&to[j])
                ));
                // Undone, so that no file is left under a hidden temporary name.
                for (a, b) in moves[..k].iter().rev() {
                    let _ = std::fs::rename(b, a);
                }
                failed = true;
                break;
            }
        }
        // Show the shown image under its new name.
        let shown = from.iter().position(|f| *f == self.path(self.cur));
        self.selected.clear();
        self.selection_changed(ctx);
        if self.collection.is_some() {
            // The collection keeps the files, under their new names.
            for (f, t) in from.iter().zip(&to) {
                let i = (0..self.files.len()).find(|&i| self.is_file(i, f));
                if let (Some(i), Ok(rel)) = (i, t.strip_prefix(&self.dir)) {
                    self.files[i] = rel.as_os_str().to_owned();
                }
            }
            self.save_collection();
            return self.list(ctx);
        }
        match shown {
            Some(k) if !failed => self.open(ctx, to[k].clone()),
            _ => self.list(ctx),
        }
    }

    /// Thumbnails in the file list, or none.
    /// The Find Duplicates window: compares the images of the list in the background, and
    /// lists the groups found; checked files can go to the trash.
    fn duplicates_window(&mut self, ctx: &egui::Context) {
        let Some(mut dups) = self.dups.take() else {
            return;
        };
        if let Some(Ok(groups)) = dups.running.as_ref().map(|rx| rx.try_recv()) {
            dups.running = None;
            dups.groups = groups;
            dups.checked.clear();
            dups.remove(&HashSet::new());
        }
        let (mut open, mut start, mut trash, mut show) = (true, false, false, None);
        egui::Window::new("Find Duplicates")
            .open(&mut open)
            .default_size([620.0, 480.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(format!(
                        "Among the {} images in the list, compare by",
                        self.files.len()
                    ));
                    let label = duplicates::ALL
                        .iter()
                        .find(|a| a.0 == dups.by)
                        .map_or("", |a| a.1);
                    egui::ComboBox::from_id_salt("compare by")
                        .selected_text(label)
                        .show_ui(ui, |ui| {
                            for (by, label) in duplicates::ALL {
                                ui.selectable_value(&mut dups.by, by, label);
                            }
                        });
                    if dups.running.is_some() {
                        if ui.button("Stop").clicked() {
                            dups.progress.cancel.store(true, Ordering::Relaxed);
                        }
                    } else {
                        start = ui
                            .add_enabled(self.files.len() > 1, egui::Button::new("Find"))
                            .clicked();
                    }
                });
                if dups.running.is_some() {
                    let (done, total) = (
                        dups.progress.done.load(Ordering::Relaxed),
                        dups.progress.total.load(Ordering::Relaxed),
                    );
                    ui.label(format!(
                        "Comparing… {} of {}",
                        thousands(done),
                        thousands(total)
                    ));
                    ctx.request_repaint_after(std::time::Duration::from_millis(100));
                    return;
                }
                if dups.searched == 0 {
                    if !self.recursive {
                        ui.weak(
                            "Include Subfolders in the View menu searches the folders below too.",
                        );
                    }
                    return;
                }
                let files: usize = dups.groups.iter().map(Vec::len).sum();
                ui.label(format!(
                    "{} groups, {} files, among {} images",
                    thousands(dups.groups.len()),
                    thousands(files),
                    thousands(dups.searched)
                ));
                ui.horizontal(|ui| {
                    if ui.button("Check all but the first of each group").clicked() {
                        dups.checked = dups
                            .groups
                            .iter()
                            .flat_map(|g| g[1..].iter().cloned())
                            .collect();
                    }
                    if ui.button("Uncheck all").clicked() {
                        dups.checked.clear();
                    }
                    let n = dups.checked.len();
                    trash = ui
                        .add_enabled(
                            n > 0,
                            egui::Button::new(format!("Move {n} checked to trash…")),
                        )
                        .clicked();
                });
                ui.separator();
                let row_h = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
                egui::ScrollArea::vertical().auto_shrink(false).show_rows(
                    ui,
                    row_h,
                    dups.rows.len(),
                    |ui, range| {
                        for &(g, f) in &dups.rows[range] {
                            let group = &dups.groups[g];
                            if f == usize::MAX {
                                ui.strong(format!(
                                    "Group {} ({} files)",
                                    g + 1,
                                    thousands(group.len())
                                ));
                                continue;
                            }
                            let path = &group[f];
                            ui.horizontal(|ui| {
                                let mut on = dups.checked.contains(path);
                                if ui.checkbox(&mut on, "").changed() {
                                    match on {
                                        true => dups.checked.insert(path.clone()),
                                        false => dups.checked.remove(path),
                                    };
                                }
                                let name = path.file_name().unwrap_or_default().to_string_lossy();
                                if ui.link(name).clicked() {
                                    show = Some(path.clone());
                                }
                                let size = path.metadata().map_or(0, |m| m.len());
                                ui.weak(format!(
                                    "{}  {}",
                                    human_size(size),
                                    path.parent().unwrap_or(path).display()
                                ));
                            });
                        }
                    },
                );
            });
        if start {
            let paths: Vec<PathBuf> = (0..self.files.len()).map(|i| self.path(i)).collect();
            let (tx, ctx) = (mpsc::channel(), ctx.clone());
            let (by, progress) = (dups.by, Arc::new(duplicates::Progress::default()));
            let (rotate, save) = (self.config.exif_rotate, self.config.save_thumbnails);
            dups.searched = paths.len();
            dups.progress = progress.clone();
            dups.running = Some(tx.1);
            let sender = tx.0;
            std::thread::spawn(move || {
                let _ = sender.send(duplicates::find(paths, by, rotate, save, &progress));
                ctx.request_repaint();
            });
        }
        if let Some(path) = show {
            match (0..self.files.len()).find(|&i| self.is_file(i, &path)) {
                Some(i) => self.go(ctx, i),
                None => self.visit(ctx, path),
            }
        }
        if trash {
            // The checked files of the list, through the usual delete with its confirmation.
            self.selected = (0..self.files.len())
                .filter(|&i| dups.checked.contains(&self.path(i)))
                .collect();
            self.selection_changed(ctx);
            // With none selected the delete takes the shown image: only when the list still has
            // every checked file, else it would be another one, or too few.
            if self.selected.len() == dups.checked.len() {
                self.request_delete(ctx, true);
            } else {
                self.notice = Some("The checked files are no longer all in the list".into());
            }
        }
        if open {
            self.dups = Some(dups);
        } else {
            dups.progress.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Decode everything again with or without EXIF rotation.
    fn set_thumbnails(&mut self, on: bool) {
        self.config.thumbnails = on;
        self.config.save();
        if !on {
            self.thumbs.clear();
            self.thumbs_pending.clear();
        }
        self.scroll_to_cur = true;
    }

    /// List the images of all subfolders too, or only the folder's own.
    fn set_subfolders(&mut self, ctx: &egui::Context, on: bool) {
        self.config.show_subfolders = on;
        self.config.save();
        self.recursive = on;
        self.list(ctx);
    }

    /// Preferences → Keyboard shortcuts: every action with a key, which can be changed (the next
    /// key press) or reset to Geeqie's.
    fn key_settings(&mut self, ui: &mut egui::Ui) {
        if let Some(action) = self.capturing {
            let pressed = ui.input(|i| {
                use egui::Key as K;
                i.events.iter().find_map(|e| match e {
                    // A modifier on its own is not the key; wait for the one pressed with it.
                    egui::Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } if !matches!(
                        key,
                        K::ShiftLeft
                            | K::ShiftRight
                            | K::ControlLeft
                            | K::ControlRight
                            | K::AltLeft
                            | K::AltRight
                            | K::SuperLeft
                            | K::SuperRight
                    ) =>
                    {
                        Some((*key, *modifiers))
                    }
                    _ => None,
                })
            });
            if let Some((key, mods)) = pressed {
                self.capturing = None;
                if key != egui::Key::Escape {
                    let mods = egui::Modifiers {
                        mac_cmd: false,
                        ..mods
                    };
                    self.config.keys.0.retain(|k| k.0 != action);
                    self.config.keys.0.push((action, mods, key));
                    self.config.save();
                    if let Ok(mut own) = OWN_KEYS.write() {
                        own.clone_from(&self.config.keys.0);
                    }
                }
            }
        }
        let all = shortcuts();
        let mut actions: Vec<Action> = Vec::new();
        for s in SHORTCUTS {
            if !actions.contains(&s.0) && (s.0 != Action::Play || cfg!(feature = "video")) {
                actions.push(s.0);
            }
        }
        egui::Grid::new("keys").striped(true).show(ui, |ui| {
            for action in actions {
                ui.label(action.label());
                let keys: Vec<String> = (all.iter().filter(|s| s.0 == action))
                    .map(|s| {
                        ui.ctx()
                            .format_shortcut(&egui::KeyboardShortcut::new(s.1, s.2))
                    })
                    .collect();
                if self.capturing == Some(action) {
                    ui.strong("Press a key (Esc cancels)");
                } else {
                    ui.label(keys.join(", "));
                }
                ui.horizontal(|ui| {
                    if ui.small_button("Change").clicked() {
                        self.capturing = Some(action);
                    }
                    let own = self.config.keys.0.iter().any(|k| k.0 == action);
                    if own && ui.small_button("Reset").clicked() {
                        self.config.keys.0.retain(|k| k.0 != action);
                        self.config.save();
                        if let Ok(mut o) = OWN_KEYS.write() {
                            o.clone_from(&self.config.keys.0);
                        }
                    }
                });
                ui.end_row();
            }
        });
    }

    /// Decode everything again with or without colour management.
    fn set_color_management(&mut self, on: bool) {
        self.config.color_management = on;
        self.config.save();
        COLOR_MANAGEMENT.store(on, Ordering::Relaxed);
        self.redecode();
    }

    /// Show everything through the screen profile in `path` (empty: none, the screen is sRGB).
    fn set_screen_profile(&mut self, path: String) {
        if let Err(e) = set_screen(&path) {
            self.notice = Some(e);
            return;
        }
        self.config.screen_profile = path;
        self.config.save();
        self.use_screen();
    }

    /// Whether the desktop's screen profile goes before the chosen one; read from colord when on.
    fn set_desktop_profile(&mut self, on: bool, ctx: &egui::Context) {
        self.config.desktop_profile = on;
        self.config.save();
        if on && self.desktop_screen.is_none() {
            self.desktop_screen_rx = Some(read_desktop_screen(ctx));
        }
        self.use_screen();
    }

    /// Show through the desktop's screen profile when it counts and colord has one, else through
    /// the chosen one.
    fn use_screen(&mut self) {
        let path = match &self.desktop_screen {
            Some(p) if self.config.desktop_profile => p.clone(),
            _ => self.config.screen_profile.clone(),
        };
        match set_screen(&path) {
            Ok(false) => {}
            Ok(true) => self.redecode(),
            Err(e) => self.notice = Some(e),
        }
    }

    /// The profile for images without one: srgb, adobe, p3 or an ICC file.
    fn set_input_profile(&mut self, name: String) {
        if let Err(e) = set_input(&name) {
            self.notice = Some(e);
            return;
        }
        self.config.input_profile = name;
        self.config.save();
        self.redecode();
    }

    /// Whether an image's own profile counts, or the input profile is used for all.
    fn set_own_profile(&mut self, on: bool) {
        set_colour(|c| c.own = on);
        self.config.own_profile = on;
        self.config.save();
        self.redecode();
    }

    /// The rendering intent, from `INTENTS`.
    fn set_rendering_intent(&mut self, name: String) {
        set_intent(&name);
        self.config.rendering_intent = name;
        self.config.save();
        self.redecode();
    }

    /// Colour management, as in Geeqie's View → Color Management and in Preferences: on or off,
    /// the profile of images without one, whether an image's own counts, and the screen profile.
    fn colour_settings(&mut self, ui: &mut egui::Ui) {
        let mut cm = self.config.color_management;
        if ui.checkbox(&mut cm, "Colour management").changed() {
            self.set_color_management(cm);
        }
        ui.add_enabled_ui(cm, |ui| {
            let mut own = self.config.own_profile;
            if ui
                .checkbox(&mut own, "Use the image's own profile")
                .changed()
            {
                self.set_own_profile(own);
            }
            ui.label("Images without a profile:");
            let current = self.config.input_profile.clone();
            for (value, label) in INPUT_PROFILES {
                if ui.radio(current == value, label).clicked() && current != value {
                    self.set_input_profile(value.to_owned());
                }
            }
            let own_file = !INPUT_PROFILES.iter().any(|p| p.0 == current);
            let file = Path::new(&current).file_name().filter(|_| own_file);
            let label = file.map_or("Profile file…".into(), |n| {
                format!("{}…", n.to_string_lossy())
            });
            if ui.radio(own_file, label).clicked() {
                self.pick(&ui.ctx().clone(), false, Pick::InputProfile);
            }
            let mut desktop = self.config.desktop_profile;
            if ui
                .checkbox(&mut desktop, "Use the desktop's screen profile")
                .on_hover_text("From colord, which GNOME and KDE set")
                .changed()
            {
                self.set_desktop_profile(desktop, ui.ctx());
            }
            let from_desktop =
                (self.desktop_screen.clone()).filter(|_| self.config.desktop_profile);
            ui.horizontal(|ui| {
                ui.label("Screen profile:");
                if let Some(path) = from_desktop {
                    let name = Path::new(&path).file_name().unwrap_or_default();
                    let name = format!("{}, from the desktop", name.to_string_lossy());
                    ui.label(name).on_hover_text(path);
                    return;
                }
                let file = Path::new(&self.config.screen_profile).file_name();
                let name = file.map_or("none, sRGB".into(), |n| n.to_string_lossy());
                ui.label(name).on_hover_text(&self.config.screen_profile);
                if ui.button("Choose…").clicked() {
                    self.pick(&ui.ctx().clone(), false, Pick::ScreenProfile);
                }
                if !self.config.screen_profile.is_empty() && ui.button("None").clicked() {
                    self.set_screen_profile(String::new());
                }
            });
            ui.label("Rendering intent:");
            let current = self.config.rendering_intent.clone();
            for (value, label, _) in INTENTS {
                if ui.radio(current == value, label).clicked() && current != value {
                    self.set_rendering_intent(value.to_owned());
                }
            }
        });
    }

    /// Decode the images and thumbnails again, to be shown in other colours.
    fn redecode(&mut self) {
        self.slots.clear();
        self.pending.clear();
        self.thumbs.clear();
        self.thumbs_pending.clear();
        self.last_drawn = None;
        self.refill();
    }

    /// Decode everything again with or without EXIF rotation.
    fn set_exif_rotate(&mut self, on: bool) {
        self.config.exif_rotate = on;
        self.config.save();
        self.rotate.store(on, Ordering::Relaxed);
        self.slots.clear();
        self.pending.clear();
        self.errors.clear();
        self.last_drawn = None;
        self.refill();
    }

    /// Random order changed in the menu or preferences: reshuffle now if a slideshow runs.
    fn set_random_order(&mut self, ctx: &egui::Context, on: bool) {
        self.config.random = on;
        self.config.save();
        if self.slideshow.is_some() {
            self.set_random(ctx, on);
            self.title(ctx);
        }
    }

    /// A random order ends with the slideshow: the list goes back to the chosen sort order,
    /// keeping the shown image.
    fn stop_slideshow(&mut self, ctx: &egui::Context) {
        self.slideshow = None;
        self.paused = false;
        if self.recursive != self.config.show_subfolders {
            self.recursive = self.config.show_subfolders;
            self.random = false;
            self.list(ctx);
        } else if self.random {
            self.set_random(ctx, false);
        }
        self.title(ctx);
    }

    /// "image name - Glimt" (or "Glimt - image name", Preferences); the folder when it has no images.
    fn title(&self, ctx: &egui::Context) {
        let name = match (self.files.get(self.cur), &self.collection) {
            (Some(_), _) => self.name(self.cur).into_owned(),
            (None, Some(file)) => file.display().to_string(),
            (None, None) => self.dir.display().to_string(),
        };
        // A named Glimt says which it is.
        let app = match remote::name() {
            Some(n) => format!("Glimt ({n})"),
            None => "Glimt".to_owned(),
        };
        let title = if self.config.app_name_first {
            format!("{app} - {name}")
        } else {
            format!("{name} - {app}")
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
    }

    /// Switch image when the time is up. Waits for a preloaded image that is still
    /// decoding, so the slideshow never shows a black screen.
    fn slideshow_tick(&mut self, ctx: &egui::Context) {
        let Some(last) = self.slideshow else { return };
        // Waits while Rename or a delete asks about the shown image.
        if self.files.is_empty() || self.paused || self.prompt.is_some() || self.confirm.is_some() {
            return;
        }
        // Several panes: the next set together, or the next image into one pane at a time.
        let n = self.panes.len() + 1;
        let turns = n > 1 && self.one_at_a_time();
        let next = match n {
            1 => self.step(1, true),
            _ if turns => self.step_from(self.latest, 1, true),
            _ => self.step(n as isize, true),
        };
        let Some(next) = next else {
            return;
        };
        let first = self.step(-isize::MAX / 2, false);
        let delay = std::time::Duration::from_secs(self.config.delay);
        let left = delay.saturating_sub(last.elapsed());
        if !left.is_zero() {
            ctx.request_repaint_after(left);
        } else if Some(next) == first && !self.config.slideshow_repeat {
            self.stop_slideshow(ctx);
        } else if self.random && Some(next) == first && self.files.len() > 2 {
            // New round: shuffle again instead of repeating the same order.
            self.set_random(ctx, true);
        } else if !self.pending.contains(&(next, false)) {
            let before = self.shown_textures();
            if turns && self.turn > 0 {
                self.panes[self.turn - 1] = next;
                self.refill();
            } else {
                self.go(ctx, next);
            }
            self.fade = before.map(|b| (Instant::now(), b));
            if turns {
                (self.latest, self.turn) = (next, (self.turn + 1) % n);
            }
            self.slideshow = Some(Instant::now());
        }
    }

    /// The fade's length: none in power saving, where every frame counts.
    fn fade_ms(&self) -> u64 {
        match self.config.cpu_use {
            0 => 0,
            _ => FADES[self.config.fade].2,
        }
    }

    /// The textures each pane shows now, for a fade to start from; `None` with fading off.
    fn shown_textures(&self) -> Option<Vec<Option<Shown>>> {
        (self.fade_ms() > 0).then(|| {
            (std::iter::once(self.cur).chain(self.panes.iter().copied()))
                .map(|i| self.slots.get(&i).map(|s| (s.tex.clone(), s.orig)))
                .collect()
        })
    }

    /// Pane `k` fading from another image to `now`: how far (0 to 1, eased), and the image it
    /// fades from. Ends the fade when it is done; asks for the next frame while it runs.
    fn fading(
        &mut self,
        ctx: &egui::Context,
        k: usize,
        now: egui::TextureId,
    ) -> Option<(f32, egui::TextureHandle, [usize; 2])> {
        let (start, from) = self.fade.as_ref()?;
        let t = start.elapsed().as_millis() as f32 / self.fade_ms().max(1) as f32;
        if t >= 1.0 {
            self.fade = None;
            return None;
        }
        ctx.request_repaint();
        let (tex, orig) = from.get(k)?.as_ref().filter(|f| f.0.id() != now)?;
        Some((t * t * (3.0 - 2.0 * t), tex.clone(), *orig))
    }

    /// The image shown next to the current one when comparing: the other of two selected
    /// files, else the next image.
    fn compare_other(&self) -> Option<usize> {
        if !self.compare {
            return None;
        }
        match self.selected.iter().collect::<Vec<_>>()[..] {
            [&a, &b] => Some(if a == self.cur { b } else { a }),
            _ => self.step(1, false).filter(|&i| i != self.cur),
        }
    }

    /// The layout's slideshow changes one pane at a time.
    fn one_at_a_time(&self) -> bool {
        let layout = &self.config.layout;
        self.config.layout_turns.split(',').any(|l| l == layout)
    }

    /// The panes after the first show the images that follow the shown one, except while a
    /// slideshow changes them one at a time.
    fn fill_panes(&mut self) {
        let [c, r] = layout_grid(&self.config.layout);
        let n = c * r;
        let kept = self.slideshow.is_some()
            && self.one_at_a_time()
            && self.panes.len() + 1 == n
            && self.panes.iter().all(|&i| i < self.files.len());
        if kept {
            return;
        }
        self.panes = (1..n as isize).filter_map(|k| self.step(k, true)).collect();
        self.latest = self.panes.last().copied().unwrap_or(self.cur);
        self.turn = 0;
    }

    fn set_layout(&mut self, layout: &str) {
        self.config.layout = layout.to_owned();
        self.config.save();
        // Comparing is a layout of its own.
        self.compare = false;
        self.refill();
    }

    /// Drop textures outside the preload window, and full-resolution versions of images other
    /// than the shown one; request the missing ones.
    fn refill(&mut self) {
        if self.files.is_empty() {
            return;
        }
        self.fill_panes();
        // The shown image first, then its neighbours in the order the list shows.
        let (cur, ahead) = (self.cur, self.ahead.load(Ordering::Relaxed) as isize);
        let mut window = vec![cur];
        // The compared image second: it may be loaded at full resolution too.
        let other = self.compare_other().filter(|&i| i != cur);
        window.extend(other);
        // The other panes, which may be zoomed too, and the images a slideshow shows in them next.
        window.extend(self.panes.iter().filter(|&&i| i != cur && Some(i) != other));
        let full = window.len();
        self.full_ok.store(full, Ordering::Relaxed);
        let n = self.panes.len() + 1;
        let upcoming = (1..=n as isize).filter_map(|d| self.step_from(self.latest, d, true));
        for i in self
            .panes
            .iter()
            .copied()
            .chain(upcoming.collect::<Vec<_>>())
        {
            if !window.contains(&i) {
                window.push(i);
            }
        }
        for d in (1..=ahead).flat_map(|d| [d, -d]) {
            if let Some(i) = self.step(d, false).filter(|i| !window.contains(i)) {
                window.push(i);
            }
        }
        self.slots
            .retain(|i, s| window.contains(i) && (!s.full || window[..full].contains(i)));
        *self.keep.lock().unwrap() = window.clone();
        for i in window {
            self.request(i, false);
        }
    }

    fn receive(&mut self, ctx: &egui::Context) {
        while let Ok(d) = self.done.try_recv() {
            self.pending.remove(&(d.idx, d.full));
            let wanted = self.is_file(d.idx, &d.path)
                && d.rotated == self.config.exif_rotate
                && d.page == if d.idx == self.cur { self.page } else { 0 }
                && self.keep.lock().unwrap().contains(&d.idx)
                && (!d.full || d.idx == self.cur || Some(d.idx) == self.compare_other());
            match d.result {
                Some(Ok((frames, orig, hist, pages))) if wanted => {
                    // A screen-size version must not replace a full-resolution one already loaded.
                    if !d.full && self.slots.get(&d.idx).is_some_and(|s| s.full) {
                        continue;
                    }
                    let mut anim: Vec<_> = (frames.into_iter().enumerate())
                        .map(|(n, (img, ms))| {
                            let name = format!("img{} {n}", d.idx);
                            (
                                ctx.load_texture(name, img, egui::TextureOptions::LINEAR),
                                ms,
                            )
                        })
                        .collect();
                    let tex = anim[0].0.clone();
                    if anim.len() == 1 {
                        anim.clear();
                    }
                    self.slots.insert(
                        d.idx,
                        Slot {
                            tex,
                            path: d.path,
                            full: d.full,
                            orig,
                            anim,
                            hist,
                            pages,
                        },
                    );
                    if d.idx == self.cur
                        && let Some(t) = self.started.take()
                    {
                        eprintln!("first image: {} ms", t.elapsed().as_millis());
                    }
                }
                Some(Err(e)) if wanted => {
                    self.errors.insert(d.idx, e);
                }
                _ => {}
            }
        }
    }

    /// Preferences → CPU use: power saving, balanced or full.
    fn cpu_settings(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("CPU use:");
            for (k, (name, hover)) in CPU_LEVELS.iter().enumerate() {
                if ui
                    .radio(self.config.cpu_use == k, *name)
                    .on_hover_text(*hover)
                    .clicked()
                {
                    self.config.cpu_use = k;
                    set_cpu_use(k);
                    self.config.save();
                }
            }
        });
    }

    /// Preferences → Memory use: Low, Medium or High set the settings below it at once; other
    /// values are Custom.
    fn memory_settings(&mut self, ui: &mut egui::Ui) {
        let c = &self.config;
        let now = (c.preload, c.decode_threads, c.animation_mb, c.low_memory);
        let level = MEMORY_LEVELS.iter().find(|l| (l.1, l.2, l.3, l.4) == now);
        let (mut chosen, mut custom) = (None, false);
        ui.horizontal(|ui| {
            ui.label("Memory use:");
            for l in &MEMORY_LEVELS {
                let on = level.is_some_and(|m| m.0 == l.0);
                if ui.radio(on, l.0).on_hover_text(l.5).clicked() {
                    chosen = Some(*l);
                }
            }
            // Chosen by changing the details, which it opens.
            custom = ui.radio(level.is_none(), "Custom").clicked();
        });
        if let Some((_, preload, threads, animation, low, _)) = chosen {
            self.set_preload(preload);
            (self.config.decode_threads, self.config.animation_mb) = (threads, animation);
            self.config.low_memory = low;
            MAX_ANIMATION.store(animation << 20, Ordering::Relaxed);
            self.config.save();
        }
        let restart = "Decoding threads and one malloc arena take effect after a restart.";
        egui::CollapsingHeader::new("Details")
            .open((level.is_none() || custom).then_some(true))
            .show(ui, |ui| {
                let mut n = self.config.preload;
                ui.label("Preload next and previous images:");
                let slider = egui::Slider::new(&mut n, 0..=20).text("images on each side");
                if ui.add(slider).changed() {
                    self.set_preload(n);
                    self.config.save();
                }
                let [w, h] = self.limits.lock().unwrap()[0];
                let mb = (2 * n as u64 + 1) * w as u64 * h as u64 * 4 / 1_000_000;
                ui.weak(format!("Up to ≈ {mb} MB at window size {w}×{h}"));
                let c = &mut self.config;
                let mut changed = ui
                    .add(
                        egui::Slider::new(&mut c.decode_threads, 1..=8)
                            .text("images decoded at once"),
                    )
                    .changed();
                let animation = egui::Slider::new(&mut c.animation_mb, 8..=1024)
                    .logarithmic(true)
                    .text("MB at most for an animation's frames");
                if ui.add(animation).changed() {
                    MAX_ANIMATION.store(c.animation_mb << 20, Ordering::Relaxed);
                    changed = true;
                }
                let low = "One malloc arena: much less memory, decoding up to ~8 % slower";
                changed |= ui.checkbox(&mut c.low_memory, low).changed();
                ui.weak(restart);
                if changed {
                    c.save();
                }
            });
    }

    /// Preload `n` images on each side.
    fn set_preload(&mut self, n: usize) {
        self.ahead.store(n, Ordering::Relaxed);
        self.refill();
        self.config.preload = n;
    }

    /// The file types the list shows, from the Types menu.
    /// The type of a file opened directly is listed too, in its folder.
    fn types(&self) -> Types {
        let o = self.opened_type;
        Types {
            images: self.config.list_images || o.images,
            raw: self.config.list_raw || o.raw,
            videos: self.config.videos || o.videos,
            hidden: self.config.show_hidden || o.hidden,
        }
    }

    /// The file types to list, as the file classes of Geeqie's file filter; beside the sort menu.
    fn types_menu(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        let c = &mut self.config;
        let mut all = vec![("Images", &mut c.list_images), ("RAW", &mut c.list_raw)];
        if cfg!(feature = "video") {
            all.push(("Videos", &mut c.videos));
        }
        let on: Vec<&str> = all.iter().filter(|t| *t.1).map(|t| t.0).collect();
        let title = match on.len() {
            n if n == all.len() => "All types".to_owned(),
            0 => "No types".to_owned(),
            _ => on.join(", "),
        };
        let mut changed = false;
        ui.menu_button(format!("{title} ⏷"), |ui| {
            for (label, on) in all {
                changed |= ui.checkbox(on, label).changed();
            }
        });
        if changed {
            self.config.save();
            self.list(ctx);
        }
    }

    /// Geeqie's sort menu in the status bar.
    fn sort_menu(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        let mut changed = false;
        scroll_menu(ui, format!("{} ⏷", self.config.sort.entry().2), |ui| {
            for (sort, _, label) in SORTS {
                if ui
                    .selectable_label(self.config.sort == sort, label)
                    .clicked()
                {
                    self.config.sort = sort;
                    changed = true;
                }
            }
            ui.separator();
            changed |= ui
                .checkbox(&mut self.config.sort_ascending, "Ascending")
                .changed();
            changed |= ui.checkbox(&mut self.config.sort_case, "Case").changed();
        });
        if changed {
            self.config.save();
            // A chosen order replaces a shuffled one (kept after a random slideshow).
            self.random = false;
            self.sort_collection = self.collection.is_some();
            self.list(ctx);
        }
    }

    /// Keep the screen saver off while `on`: gnome-session-inhibit (GNOME), else systemd-inhibit.
    /// The inhibitor runs `cat` on a pipe from Glimt, so it ends with Glimt even after a crash.
    fn inhibit(&mut self, on: bool) {
        if on == self.inhibitor.is_some() {
            return;
        }
        if let Some(mut child) = self.inhibitor.take() {
            let _ = child.kill();
            std::thread::spawn(move || child.wait());
            return;
        }
        let commands: [&[&str]; 2] = [
            &[
                "gnome-session-inhibit",
                "--inhibit",
                "idle",
                "--reason",
                "Glimt full screen",
                "cat",
            ],
            &[
                "systemd-inhibit",
                "--what=idle",
                "--who=Glimt",
                "--why=Full screen",
                "cat",
            ],
        ];
        self.inhibitor = commands.iter().find_map(|c| {
            command(c[0])
                .args(&c[1..])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .ok()
        });
    }

    /// The folder path above the folder list, as in Geeqie; Enter opens what was typed.
    fn path_field(&mut self, ui: &mut egui::Ui) {
        let edit = egui::TextEdit::singleline(&mut self.path_text).desired_width(f32::INFINITY);
        let response = ui.add(edit);
        if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            let text = self.path_text.trim();
            let path = match (text.strip_prefix('~'), std::env::var_os("HOME")) {
                (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') => {
                    PathBuf::from(home).join(rest.trim_start_matches('/'))
                }
                _ => PathBuf::from(text),
            };
            if text.contains("://") {
                self.mount(ui.ctx(), text.to_owned());
            } else if path.exists() {
                self.visit(ui.ctx(), path);
            } else {
                self.notice = Some(format!("No such folder: {}", path.display()));
            }
        }
        if !response.has_focus() {
            let shown = self.collection.as_ref().unwrap_or(&self.dir);
            self.path_text = match self.folders.is_empty() {
                true => shown.display().to_string(),
                // Several folders: each of them.
                false => (self.folders.iter())
                    .map(|f| f.display().to_string())
                    .collect::<Vec<_>>()
                    .join(" + "),
            };
        }
    }

    /// Geeqie's toolbar: navigation, zoom, thumbnails, slideshow and preferences.
    fn toolbar(&mut self, ui: &mut egui::Ui, act: &mut Option<Action>) {
        let size = [14.0, 20.0, 28.0][self.config.toolbar_icons.min(2)];
        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.style_mut().override_font_id = Some(egui::FontId::proportional(size));
            ui.horizontal(|ui| {
                tool_button(ui, act, theme::icon::BACK, Action::Back, false);
                tool_button(ui, act, theme::icon::FORWARD, Action::Forward, false);
                tool_button(ui, act, theme::icon::UP, Action::Up, false);
                tool_button(ui, act, theme::icon::HOME, Action::Home, false);
                tool_button(ui, act, theme::icon::REFRESH, Action::Refresh, false);
                ui.separator();
                tool_button(ui, act, theme::icon::ZOOM_IN, Action::ZoomIn, false);
                tool_button(ui, act, theme::icon::ZOOM_OUT, Action::ZoomOut, false);
                tool_button(ui, act, "1:1", Action::Zoom1To1, false);
                tool_button(
                    ui,
                    act,
                    theme::icon::FIT,
                    Action::ZoomFit,
                    self.zoom.is_none(),
                );
                ui.separator();
                tool_button(
                    ui,
                    act,
                    theme::icon::THUMBNAILS,
                    Action::Thumbnails,
                    self.config.thumbnails,
                );
                let show = if self.slideshow.is_some() {
                    theme::icon::STOP
                } else {
                    theme::icon::PLAY
                };
                tool_button(ui, act, show, Action::Slideshow, self.slideshow.is_some());
                tool_button(ui, act, theme::icon::SETTINGS, Action::Preferences, false);
            });
        });
    }

    /// Full screen info in the top left corner: name and place, then size and EXIF.
    fn overlay(&mut self, ui: &mut egui::Ui, area: egui::Rect) {
        self.want_exif();
        let mut info = Vec::new();
        if let Some(s) = self.slots.get(&self.cur) {
            info.push(format!("{} × {}", s.orig[0], s.orig[1]));
        }
        let path = self.path(self.cur);
        if let Some((_, rows)) = self.exif.as_ref().filter(|e| e.0 == path) {
            let wanted = [
                "File size",
                "Taken",
                "Exposure",
                "Aperture",
                "ISO",
                "Focal length",
            ];
            for (label, value) in rows.iter().filter(|r| wanted.contains(&r.0)) {
                info.push(if *label == "ISO" {
                    format!("ISO {value}")
                } else {
                    value.clone()
                });
            }
        }
        let marks = self.mark_text(self.cur);
        if !marks.is_empty() {
            info.push(marks);
        }
        let name = self.name(self.cur);
        let text = format!("{name}  {}\n{}", self.place(), info.join(" · "));
        let p = ui.painter();
        let galley = p.layout_no_wrap(text, egui::FontId::proportional(15.0), egui::Color32::WHITE);
        let at = egui::Rect::from_min_size(
            area.min + egui::vec2(12.0, 12.0),
            galley.size() + egui::vec2(20.0, 12.0),
        );
        p.rect_filled(at, 6.0, egui::Color32::from_black_alpha(150));
        p.galley(at.min + egui::vec2(10.0, 6.0), galley, egui::Color32::WHITE);
    }

    /// Ask the EXIF reader for the shown image (once per image) and take what has arrived.
    fn want_exif(&mut self) {
        let path = self.path(self.cur);
        if self.exif_asked.as_ref() != Some(&path) {
            let _ = self.exif_jobs.send(path.clone());
            self.exif_asked = Some(path);
        }
        let done: Vec<ExifRead> = self.exif_done.try_iter().collect();
        for (path, rows, all, meta) in done {
            // Kept while being edited: the reader may answer for the image again.
            if self.meta.as_ref().is_none_or(|m| m.path != path) {
                self.save_meta();
                self.meta = Some(MetaEdit {
                    path: path.clone(),
                    keywords: meta.keywords_text(),
                    comment: meta.comment.clone(),
                    saved: meta,
                });
            }
            self.exif = Some((path, rows));
            self.exif_all = all;
        }
    }

    /// The shown image's place in the list as shown: "[3/16]", "[–/12]" if a search hides it.
    /// `place` of image `idx` in another pane; the search matches are looked through once per
    /// image, not every frame.
    fn place_of(&mut self, idx: usize) -> String {
        let total = self.matches.as_ref().map_or(self.files.len(), Vec::len);
        if let Some((i, t, text)) = &self.status_place
            && (*i, *t) == (idx, total)
        {
            return text.clone();
        }
        let row = match &self.matches {
            Some(m) => m.iter().position(|&x| x == idx),
            None => Some(idx),
        };
        let row = row.map_or("–".into(), |r| thousands(r + 1));
        let text = format!("[{row}/{}]", thousands(total));
        self.status_place = Some((idx, total, text.clone()));
        text
    }

    fn place(&self) -> String {
        match &self.matches {
            Some(m) => match self.cur_row(m) {
                Some(row) => format!("[{}/{}]", thousands(row + 1), thousands(m.len())),
                None => format!("[–/{}]", thousands(m.len())),
            },
            None => format!(
                "[{}/{}]",
                thousands(self.cur + 1),
                thousands(self.files.len())
            ),
        }
    }

    /// The sort manager (Shift+S), as in Geeqie: a click on a folder copies or moves the selected
    /// files, or the shown one, there.
    fn sort_manager(&mut self, ui: &mut egui::Ui) {
        let mut remove = None;
        egui::Panel::right("sort manager")
            .resizable(true)
            .default_size(200.0)
            .show(ui, |ui| {
                ui.strong("Sort Manager");
                ui.horizontal(|ui| {
                    let c = &mut self.config;
                    let changed = ui.radio_value(&mut c.sort_move, false, "Copy").changed()
                        | ui.radio_value(&mut c.sort_move, true, "Move").changed();
                    if changed {
                        c.save();
                    }
                });
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (k, dir) in self.sort_folders.clone().into_iter().enumerate() {
                        ui.horizontal(|ui| {
                            if ui
                                .small_button(theme::icon::CLOSE)
                                .on_hover_text("Remove")
                                .clicked()
                            {
                                remove = Some(k);
                            }
                            let name = dir.file_name().unwrap_or(dir.as_os_str());
                            let here = dir == self.dir;
                            let button = ui.add_enabled(
                                !here && !self.files.is_empty(),
                                egui::Button::new(name.to_string_lossy()),
                            );
                            if button.on_hover_text(dir.display().to_string()).clicked() {
                                let paths = self.targets().iter().map(|&i| self.path(i)).collect();
                                self.transfer(ui.ctx(), paths, dir, !self.config.sort_move);
                            }
                        });
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Add This Folder").clicked() {
                            self.add_sort_folder(self.dir.clone());
                        }
                        if ui.button("Add…").clicked() {
                            self.pick(ui.ctx(), true, Pick::SortFolder);
                        }
                    });
                });
            });
        if let Some(k) = remove {
            self.sort_folders.remove(k);
            self.save_sort_folders();
        }
    }

    fn add_sort_folder(&mut self, dir: PathBuf) {
        if !self.sort_folders.contains(&dir) {
            self.sort_folders.push(dir);
            self.save_sort_folders();
        }
    }

    fn save_sort_folders(&mut self) {
        // As bytes: a name that is not UTF-8 (an old Windows share) must come back the same.
        let mut text = Vec::new();
        for d in &self.sort_folders {
            let bytes = d.as_os_str().as_encoded_bytes();
            if !bytes.contains(&b'\n') {
                text.extend(bytes);
                text.push(b'\n');
            }
        }
        let saved = sort_folders_path()
            .ok_or("no home folder".to_owned())
            .and_then(|p| {
                let _ = std::fs::create_dir_all(p.parent().unwrap_or(&p));
                std::fs::write(&p, text).map_err(|e| e.to_string())
            });
        if let Err(e) = saved {
            self.notice = Some(format!("Could not save the sort manager's folders: {e}"));
        }
    }

    /// The pan view (Ctrl+J): the images of the list by day. A click shows one, a double click
    /// also closes the view.
    fn pan_view(&mut self, ctx: &egui::Context) {
        // The thumbnail workers make the cells drawn last frame (a frame late is soon enough);
        // this frame's are noted as they are drawn.
        let first = self.pan_thumbs.iter().min().copied().unwrap_or(usize::MAX);
        let last = self.pan_thumbs.iter().max().copied().unwrap_or(0);
        self.thumb_view[2].store(first, Ordering::Relaxed);
        self.thumb_view[3].store(last, Ordering::Relaxed);
        self.pan_thumbs.clear();
        let Some(mut pan) = self.pan_window.take() else {
            return;
        };
        // The days again when the list changed, also only its order; only the visible rows are
        // drawn.
        let stale = (pan.days.as_ref()).is_none_or(|d| d.1 != self.files_gen);
        if stale && pan.rx.is_none() {
            let (tx, rx) = mpsc::channel();
            let (dir, files, exif) = (self.dir.clone(), self.files.clone(), pan.exif);
            let (busy, ctx, made) = (self.busy.clone(), ctx.clone(), self.files_gen);
            busy.fetch_add(1, Ordering::Relaxed);
            std::thread::spawn(move || {
                let _ = tx.send((pan::timeline(&dir, &files, exif), made));
                busy.fetch_sub(1, Ordering::Relaxed);
                ctx.request_repaint();
            });
            pan.rx = Some(rx);
        }
        // Days for a list that changed meanwhile are taken as old, and made again.
        if let Some(days) = pan.rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            pan.days = Some(days);
            pan.rx = None;
            pan.columns = 0;
        }
        let (mut open, mut show, mut close) = (true, None, false);
        egui::Window::new("Pan View")
            .open(&mut open)
            .default_size([900.0, 640.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui.checkbox(&mut pan.exif, "EXIF date").changed() {
                        pan.days = None;
                        pan.rx = None;
                    }
                    // Days of an older list are not shown while new ones are made: they point
                    // at other images now.
                    match pan.days.as_ref().filter(|d| d.1 == self.files_gen) {
                        Some((days, _)) => {
                            let n = days.iter().filter(|d| d.0 != "No date").count();
                            let total: usize = days.iter().map(|d| d.1.len()).sum();
                            ui.weak(format!(
                                "{} {} on {} {}",
                                thousands(total),
                                if total == 1 { "image" } else { "images" },
                                thousands(n),
                                if n == 1 { "day" } else { "days" }
                            ))
                        }
                        None => ui.spinner(),
                    };
                });
                ui.separator();
                let Some((days, _)) = pan.days.as_ref().filter(|d| d.1 == self.files_gen) else {
                    return;
                };
                let cell = self.cell();
                let heading = 26.0;
                let columns = ((ui.available_width() / cell.x) as usize).max(1);
                if columns != pan.columns {
                    (pan.rows, pan.height) = pan::layout(days, columns, heading, cell.y);
                    pan.columns = columns;
                }
                egui::ScrollArea::vertical()
                    .auto_shrink(false)
                    .show_viewport(ui, |ui, view| {
                        ui.set_height(pan.height);
                        let top = ui.min_rect().min;
                        let first = pan.rows.partition_point(|r| r.0 + cell.y < view.min.y);
                        for &(y, row) in &pan.rows[first..] {
                            if y > view.max.y {
                                break;
                            }
                            match row {
                                pan::Row::Heading(d) => {
                                    let (day, list) = &days[d];
                                    ui.painter().text(
                                        top + egui::vec2(4.0, y + heading - 6.0),
                                        egui::Align2::LEFT_BOTTOM,
                                        format!(
                                            "{day}   {} {}",
                                            thousands(list.len()),
                                            if list.len() == 1 { "image" } else { "images" }
                                        ),
                                        egui::TextStyle::Body.resolve(ui.style()),
                                        ui.visuals().strong_text_color(),
                                    );
                                }
                                pan::Row::Cells(d, start) => {
                                    let list = &days[d].1;
                                    let end = (start + columns).min(list.len());
                                    for (j, &i) in list[start..end].iter().enumerate() {
                                        if i >= self.files.len() {
                                            continue;
                                        }
                                        let at = top + egui::vec2(j as f32 * cell.x, y);
                                        let rect = egui::Rect::from_min_size(at, cell);
                                        self.pan_thumbs.push(i);
                                        let r = self.file_cell(ui, i, rect, i == self.cur);
                                        if r.clicked() {
                                            show = Some(i);
                                        }
                                        if r.double_clicked() {
                                            (show, close) = (Some(i), true);
                                        }
                                    }
                                }
                            }
                        }
                    });
            });
        if let Some(i) = show {
            self.go(ctx, i);
        }
        if open && !close {
            self.pan_window = Some(pan);
        }
    }

    /// File and EXIF details of the shown image, read in the background (Ctrl+K).
    fn exif_panel(&mut self, ui: &mut egui::Ui) {
        self.want_exif();
        egui::Panel::right("exif")
            .resizable(true)
            .default_size(240.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    egui::Grid::new("exif rows").num_columns(2).show(ui, |ui| {
                        ui.weak("File");
                        ui.label(self.name(self.cur));
                        ui.end_row();
                        if let Some(s) = self.slots.get(&self.cur) {
                            ui.weak("Dimensions");
                            ui.label(format!("{} × {}", s.orig[0], s.orig[1]));
                            ui.end_row();
                        }
                        // The old rows stay until the new ones arrive, so the panel does not flicker.
                        for (label, value) in self.exif.iter().flat_map(|e| &e.1) {
                            ui.weak(*label);
                            // The place opens on a map in the browser.
                            if *label == "GPS" {
                                let link = ui.link(value).on_hover_text("Show on OpenStreetMap");
                                if link.clicked()
                                    && let Some((lat, lon)) = value.split_once(", ")
                                {
                                    open_url(&format!(
                                        "https://www.openstreetmap.org/?mlat={lat}&mlon={lon}#map=15/{lat}/{lon}"
                                    ));
                                }
                            } else {
                                ui.label(value);
                            }
                            ui.end_row();
                        }
                    });
                    ui.separator();
                    self.meta_fields(ui);
                });
            });
    }

    /// Keywords and comment of the shown image, as in Geeqie's info sidebar.
    fn meta_fields(&mut self, ui: &mut egui::Ui) {
        let path = self.path(self.cur);
        let Some(edit) = self.meta.as_mut().filter(|m| m.path == path) else {
            return;
        };
        ui.weak("Keywords");
        let keywords = ui.add(
            egui::TextEdit::singleline(&mut edit.keywords)
                .hint_text("Separated by commas")
                .desired_width(f32::INFINITY),
        );
        if std::mem::take(&mut self.focus_keywords) {
            keywords.request_focus();
        }
        ui.weak("Comment");
        let comment = ui.add(
            egui::TextEdit::multiline(&mut edit.comment)
                .desired_rows(3)
                .desired_width(f32::INFINITY),
        );
        let add = Meta::parse_keywords(&edit.keywords);
        let n = self.selected.len();
        if n > 1
            && !add.is_empty()
            && ui
                .button(format!("Add the keywords to the {} selected", thousands(n)))
                .on_hover_text("Each file keeps its own keywords and gets these too")
                .clicked()
        {
            self.add_keywords(ui.ctx(), add);
        }
        if keywords.lost_focus() || comment.lost_focus() {
            self.save_meta();
        }
    }

    /// Save the keywords and comment being edited, if they changed.
    fn save_meta(&mut self) {
        let Some(edit) = &mut self.meta else { return };
        let meta = Meta {
            keywords: Meta::parse_keywords(&edit.keywords),
            comment: edit.comment.trim().to_owned(),
        };
        if meta == edit.saved {
            return;
        }
        match meta::write(&edit.path, &meta) {
            Ok(()) => {
                edit.keywords = meta.keywords_text();
                edit.saved = meta;
                // "exif:" searches find keywords and comments too.
                self.exif_index = None;
            }
            Err(e) => self.notice = Some(format!("Could not save the keywords: {e}")),
        }
    }

    /// Add `keywords` to every selected file, in the background.
    fn add_keywords(&mut self, ctx: &egui::Context, keywords: Vec<String>) {
        // The shown image's own edit first; it is read again afterwards, with the added ones.
        self.save_meta();
        self.meta = None;
        self.exif_asked = None;
        self.exif_index = None;
        let paths: Vec<PathBuf> = self.targets().iter().map(|&i| self.path(i)).collect();
        let (tx, busy, ctx) = (self.notices_tx.clone(), self.busy.clone(), ctx.clone());
        busy.fetch_add(1, Ordering::Relaxed);
        std::thread::spawn(move || {
            let mut errors = 0;
            for path in &paths {
                errors += meta::add(path, &keywords).is_err() as usize;
            }
            let _ = tx.send(match errors {
                0 => format!("Keywords added to {} files", thousands(paths.len())),
                n => format!("Keywords could not be saved for {} files", thousands(n)),
            });
            busy.fetch_sub(1, Ordering::Relaxed);
            ctx.request_repaint();
        });
    }

    fn receive_thumbs(&mut self, ctx: &egui::Context) {
        while let Ok((path, result)) = self.thumbs_done.try_recv() {
            // The name as the list has it: with its folders in a slideshow with subfolders.
            let Ok(name) = path.strip_prefix(&self.dir).map(Path::as_os_str) else {
                continue;
            };
            self.thumbs_pending.remove(name);
            if let Some(img) = result {
                let tex = img.map(|img| {
                    ctx.load_texture(
                        format!("thumb {}", path.display()),
                        img,
                        egui::TextureOptions::LINEAR,
                    )
                });
                self.thumbs.insert(name.to_owned(), tex);
            }
        }
    }

    /// A file list row with a thumbnail; true when clicked.
    fn file_row(
        &mut self,
        ui: &mut egui::Ui,
        i: usize,
        height: f32,
        marked: bool,
    ) -> egui::Response {
        let size = egui::vec2(ui.available_width(), height);
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
        let visuals = ui.visuals();
        if marked || response.hovered() {
            let fill = if marked {
                visuals.selection.bg_fill
            } else {
                visuals.widgets.hovered.weak_bg_fill
            };
            ui.painter().rect_filled(rect, 2.0, fill);
        }
        let text_color = if marked {
            visuals.selection.stroke.color
        } else {
            visuals.text_color()
        };
        let thumbs = self.config.thumbnails;
        let frame = egui::Rect::from_min_size(
            rect.min + egui::vec2(2.0, 2.0),
            if thumbs {
                self.thumb()
            } else {
                egui::vec2(0.0, height)
            },
        );
        if thumbs {
            self.paint_thumb(ui, i, frame);
        }
        let font = egui::TextStyle::Body.resolve(ui.style());
        let gap = if thumbs { 8.0 } else { 2.0 };
        let mut name_rect = rect.with_min_x(frame.right() + gap);
        if self.config.file_details {
            // Size and date columns at the right, as in Geeqie; the name is cut off before them.
            name_rect.max.x -= DETAILS_WIDTH;
            let (bytes, mtime) = self.file_details(i);
            let y = rect.center().y;
            let size_right = name_rect.max.x + DETAILS_WIDTH - DATE_WIDTH - 8.0;
            let p = ui.painter();
            p.text(
                egui::pos2(size_right, y),
                egui::Align2::RIGHT_CENTER,
                thousands(bytes as usize),
                font.clone(),
                text_color,
            );
            p.text(
                egui::pos2(rect.max.x - DATE_WIDTH, y),
                egui::Align2::LEFT_CENTER,
                local_time(mtime),
                font.clone(),
                text_color,
            );
        }
        // Stars and marks at the right of the name, which is cut off before them.
        let marks = self.mark_text(i);
        if !marks.is_empty() {
            let gold = egui::Color32::from_rgb(255, 190, 40);
            let at = egui::pos2(name_rect.max.x - 6.0, rect.center().y);
            let r = ui
                .painter()
                .text(at, egui::Align2::RIGHT_CENTER, marks, font.clone(), gold);
            name_rect.max.x = r.left() - 8.0;
        }
        ui.painter().with_clip_rect(name_rect).text(
            egui::pos2(name_rect.min.x, rect.center().y),
            egui::Align2::LEFT_CENTER,
            self.name(i),
            font,
            text_color,
        );
        response
    }

    /// The film strip along the bottom of `area` in full screen, shown while the pointer is
    /// near the bottom: thumbnails of the images before and after the shown one, which is
    /// outlined. Returns the image clicked.
    fn filmstrip(&mut self, ui: &egui::Ui, area: egui::Rect) -> Option<usize> {
        let cell = self.thumb() + egui::vec2(8.0, 8.0);
        let height = cell.y + 16.0;
        let strip =
            egui::Rect::from_min_max(egui::pos2(area.left(), area.bottom() - height), area.max);
        if !ui.rect_contains_pointer(strip) {
            return None;
        }
        let k = ((area.width() / cell.x) as isize - 1).max(0) / 2;
        let mut shown: Vec<usize> = (-k..=k).filter_map(|d| self.step(d, false)).collect();
        shown.dedup();
        // Thumbnails for these, as for the rows of the file list.
        if let (Some(&first), Some(&last)) = (shown.iter().min(), shown.iter().max()) {
            self.thumb_view[0].store(first, Ordering::Relaxed);
            self.thumb_view[1].store(last, Ordering::Relaxed);
            let row = match self.matches.as_deref() {
                Some(m) => self.cur_row(m).unwrap_or(0),
                None => self.cur,
            };
            let k = k as usize;
            self.trim_thumbs(row.saturating_sub(k)..row + k + 1);
        }
        ui.painter()
            .rect_filled(strip, 0.0, egui::Color32::from_black_alpha(190));
        // The shown image in the middle, where it is when there are images on both sides.
        let at = shown.iter().position(|&i| i == self.cur).unwrap_or(0) as f32;
        let x0 = strip.center().x - (at + 0.5) * cell.x;
        let mut clicked = None;
        for (n, &i) in shown.iter().enumerate() {
            let min = egui::pos2(x0 + n as f32 * cell.x, strip.top() + 8.0);
            let frame = egui::Rect::from_min_size(min, cell).shrink(4.0);
            self.paint_thumb(ui, i, frame);
            if i == self.cur {
                let stroke = egui::Stroke::new(2.0, ui.visuals().selection.bg_fill);
                ui.painter()
                    .rect_stroke(frame.expand(2.0), 3.0, stroke, egui::StrokeKind::Outside);
            }
            let id = ui.id().with(("strip", i));
            if ui.interact(frame, id, egui::Sense::click()).clicked() {
                clicked = Some(i);
            }
        }
        clicked
    }

    /// Thumbnail `i` fitted into `frame`, or a request for it.
    fn paint_thumb(&mut self, ui: &egui::Ui, i: usize, frame: egui::Rect) {
        let name = &self.files[i];
        match self.thumbs.get(name) {
            Some(Some(tex)) => {
                // Fit the box, but never enlarge.
                let px = tex.size_vec2() / ui.ctx().pixels_per_point();
                let scale = (self.thumb().x / px.x).min(self.thumb().y / px.y).min(1.0);
                let uv = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
                let at = egui::Rect::from_center_size(frame.center(), px * scale);
                ui.painter().image(tex.id(), at, uv, egui::Color32::WHITE);
                // Videos get a small play badge in the corner.
                if is_video(Path::new(name)) {
                    let c = at.left_bottom() + egui::vec2(13.0, -13.0);
                    let p = ui.painter();
                    p.circle_filled(c, 10.0, egui::Color32::from_black_alpha(170));
                    let triangle = vec![
                        c + egui::vec2(-3.0, -5.5),
                        c + egui::vec2(6.0, 0.0),
                        c + egui::vec2(-3.0, 5.5),
                    ];
                    p.add(egui::Shape::convex_polygon(
                        triangle,
                        egui::Color32::WHITE,
                        egui::Stroke::NONE,
                    ));
                }
            }
            Some(None) => {}
            None => {
                if self.thumbs_pending.insert(name.clone()) {
                    let _ = self.thumb_jobs.send((i, self.path(i)));
                }
            }
        }
    }

    /// A cell of the thumbnail grid: the thumbnail with the name below, cut off to fit.
    fn file_cell(
        &mut self,
        ui: &mut egui::Ui,
        i: usize,
        rect: egui::Rect,
        marked: bool,
    ) -> egui::Response {
        let response = ui.interact(rect, ui.id().with(("cell", i)), egui::Sense::click());
        let visuals = ui.visuals();
        if marked || response.hovered() {
            let fill = if marked {
                visuals.selection.bg_fill
            } else {
                visuals.widgets.hovered.weak_bg_fill
            };
            ui.painter().rect_filled(rect, 2.0, fill);
        }
        let color = if marked {
            visuals.selection.stroke.color
        } else {
            visuals.text_color()
        };
        let frame = egui::Rect::from_min_size(rect.min + egui::vec2(4.0, 4.0), self.thumb());
        self.paint_thumb(ui, i, frame);
        let font = egui::TextStyle::Small.resolve(ui.style());
        let name = self.name(i).into_owned();
        let mut job = egui::text::LayoutJob::simple_singleline(name, font, color);
        job.wrap = egui::text::TextWrapping::truncate_at_width(self.cell().x - 6.0);
        let galley = ui.painter().layout_job(job);
        let at = egui::pos2(
            rect.center().x - galley.size().x / 2.0,
            frame.bottom() + 4.0,
        );
        ui.painter().galley(at, galley, color);
        let marks = self.mark_text(i);
        if !marks.is_empty() {
            let p = ui.painter();
            let gold = egui::Color32::from_rgb(255, 190, 40);
            let galley = p.layout_no_wrap(marks, egui::TextStyle::Small.resolve(ui.style()), gold);
            let at = egui::Rect::from_min_size(
                frame.right_top() + egui::vec2(-galley.size().x - 4.0, 3.0),
                galley.size(),
            );
            p.rect_filled(at.expand(2.0), 3.0, egui::Color32::from_black_alpha(170));
            p.galley(at.min, galley, gold);
        }
        response.on_hover_text_at_pointer(match self.collection {
            Some(_) => self.path(i).display().to_string(),
            None => self.name(i).into_owned(),
        })
    }

    /// Folders as a tree from /, opened up to the current folder. Subfolders are read in the
    /// background when a folder is opened in the tree.
    fn folder_tree(&mut self, ui: &mut egui::Ui, open_dir: &mut Option<PathBuf>) {
        while let Ok((dir, subdirs)) = self.tree_rx.try_recv() {
            self.tree.insert(dir, Some(subdirs));
        }
        egui::ScrollArea::both()
            .id_salt("tree")
            .auto_shrink(false)
            .show(ui, |ui| self.tree_node(ui, Path::new("/"), open_dir));
    }

    /// Right-click on a folder in the list or the tree: rename it, or move it to the trash.
    fn folder_menu(&mut self, response: &egui::Response, dir: &Path) {
        response.context_menu(|ui| {
            // Its images in the list together with those shown now.
            let shown = self.folders.contains(&dir.to_path_buf()) || dir == self.dir;
            if !shown
                && self.collection.is_none()
                && ui.button("Show Together with These Images").clicked()
            {
                let mut all = match self.folders.is_empty() {
                    true => vec![self.dir.clone()],
                    false => self.folders.clone(),
                };
                all.push(dir.to_path_buf());
                self.together = Some(all);
            }
            if ui.button("Rename Folder…").clicked() {
                let name = dir.file_name().unwrap_or_default().to_string_lossy();
                self.prompt = Some((Prompt::RenameFolder, name.into_owned()));
                self.folder_target = Some(dir.to_path_buf());
            }
            if ui.button("Move Folder to Trash…").clicked() {
                self.folder_target = Some(dir.to_path_buf());
                self.confirm_folder = true;
            }
        });
    }

    /// Rename `folder_target` within its parent; never over another. The open folder, or one
    /// inside it, is followed to the new name.
    fn rename_folder(&mut self, ctx: &egui::Context, name: &str) {
        let Some(old) = self.folder_target.take() else {
            return;
        };
        let new = old.with_file_name(name);
        if new == old {
            return;
        }
        // rename() would replace an empty folder of that name.
        let result = match new.exists() {
            true => Err(std::io::ErrorKind::AlreadyExists.into()),
            false => std::fs::rename(&old, &new),
        };
        if let Err(e) = result {
            self.notice = Some(format!("{}: {e}", new.display()));
            return;
        }
        self.forget_folder(&old);
        match self.dir.strip_prefix(&old) {
            Ok(rest) => self.visit(ctx, new.join(rest)),
            Err(_) => self.list(ctx),
        }
    }

    /// Move `folder_target` and all in it to the trash, in the background; the open folder, or
    /// one inside it, is left for the folder above first.
    fn trash_folder(&mut self, ctx: &egui::Context) {
        let Some(dir) = self.folder_target.take() else {
            return;
        };
        self.forget_folder(&dir);
        self.subdirs.retain(|d| *d != dir);
        if self.dir.starts_with(&dir)
            && let Some(parent) = dir.parent()
        {
            self.visit(ctx, parent.to_path_buf());
        }
        let (tx, deleted_tx, ctx) = (
            self.notices_tx.clone(),
            self.deleted_tx.clone(),
            ctx.clone(),
        );
        let busy = self.busy.clone();
        busy.fetch_add(1, Ordering::Relaxed);
        std::thread::spawn(move || {
            let error = match command("gio").arg("trash").arg("--").arg(&dir).output() {
                Ok(o) if o.status.success() => None,
                Ok(o) => Some(String::from_utf8_lossy(&o.stderr).trim().to_owned()),
                Err(e) => Some(format!("gio trash (from glib): {e}")),
            };
            if let Some(e) = &error {
                let _ = tx.send(format!("Could not move the folder to the trash: {e}"));
            }
            // Failed: listed again, as a failed delete of files is.
            let _ = deleted_tx.send((vec![dir], error.is_some()));
            busy.fetch_sub(1, Ordering::Relaxed);
            ctx.request_repaint();
        });
    }

    /// Forget what the folder tree read of `dir` and its parent, so it is read again.
    fn forget_folder(&mut self, dir: &Path) {
        self.tree.retain(|d, _| !d.starts_with(dir));
        if let Some(parent) = dir.parent() {
            self.tree.remove(parent);
        }
    }

    fn tree_node(&mut self, ui: &mut egui::Ui, path: &Path, open_dir: &mut Option<PathBuf>) {
        let id = ui.make_persistent_id(("tree", path));
        let on_way = self.dir.starts_with(path);
        let mut state =
            egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, on_way);
        let follow = on_way && self.tree_shown != self.dir;
        if follow && path != self.dir {
            state.set_open(true);
        }
        let name = path.file_name().map_or("/".into(), |n| n.to_string_lossy());
        state
            .show_header(ui, |ui| {
                let label = ui.selectable_label(path == self.dir, name);
                // Shown once the folders above it are read.
                if follow && path == self.dir {
                    label.scroll_to_me(Some(egui::Align::Center));
                    self.tree_shown = self.dir.clone();
                }
                if label.clicked() {
                    *open_dir = Some(path.to_path_buf());
                }
                if path.parent().is_some() {
                    self.folder_menu(&label, path);
                }
            })
            .body(|ui| match self.tree.get(path) {
                Some(Some(subdirs)) => {
                    for dir in subdirs.clone() {
                        self.tree_node(ui, &dir, open_dir);
                    }
                }
                Some(None) => drop(ui.label("…")),
                None => {
                    self.tree.insert(path.to_path_buf(), None);
                    let (dir, tx, ctx) =
                        (path.to_path_buf(), self.tree_tx.clone(), ui.ctx().clone());
                    let folders = Types {
                        hidden: self.config.show_hidden,
                        ..Types::NONE
                    };
                    std::thread::spawn(move || {
                        let subdirs = list_folder(&dir, folders).0;
                        let _ = tx.send((dir, subdirs));
                        ctx.request_repaint();
                    });
                }
            });
    }

    /// View → Layout: one image, two or four, and how a slideshow changes them.
    fn layout_menu(&mut self, ui: &mut egui::Ui, act: &mut Option<Action>) {
        scroll_menu(ui, "Layout", |ui| {
            menu_item(ui, act, Action::NextLayout);
            ui.separator();
            for (key, label, _) in LAYOUTS {
                if ui.radio(self.config.layout == key, label).clicked() {
                    self.set_layout(key);
                }
            }
            if self.config.layout == "1" {
                return;
            }
            ui.separator();
            if ui
                .checkbox(&mut self.config.layout_names, "Show File Names")
                .changed()
            {
                self.config.save();
            }
            ui.weak("Slideshow in this layout:");
            let turns = self.one_at_a_time();
            let mut chosen = turns;
            ui.radio_value(&mut chosen, false, "All Images Change Together");
            ui.radio_value(&mut chosen, true, "One Image at a Time");
            if chosen != turns {
                let layout = self.config.layout.clone();
                let mut list: Vec<&str> = (self.config.layout_turns.split(','))
                    .filter(|l| !l.is_empty() && *l != layout)
                    .collect();
                if chosen {
                    list.push(&layout);
                }
                self.config.layout_turns = list.join(",");
                self.config.save();
                self.refill();
            }
        });
    }

    /// Image `i` in `rect`, one of a layout's panes after the first or the compared image: at
    /// the same zoom and offset as the shown one when zoomed, else fitted.
    /// Returns the scale it is drawn at, for the status bar; `None` while it is decoded.
    /// `k` is the pane's number, for a slideshow fading it; `None` for the compared image.
    fn paint_pane(
        &mut self,
        ui: &egui::Ui,
        rect: egui::Rect,
        i: usize,
        zoom: Option<f32>,
        k: Option<usize>,
    ) -> Option<f32> {
        // Zoomed in further than the screen-size version holds: load the full resolution.
        let sharper = self.slots.get(&i).is_some_and(|s| {
            !s.full
                && zoom
                    .is_some_and(|z| s.tex.size()[0] as f32 + 1.0 < s.orig[0] as f32 * z.min(1.0))
        });
        if sharper {
            self.request(i, true);
        }
        let now = self.slots.get(&i)?.tex.id();
        let fade = k.and_then(|k| self.fading(ui.ctx(), k, now));
        let slot = self.slots.get(&i)?;
        let ppp = ui.ctx().pixels_per_point();
        let size = egui::vec2(slot.orig[0] as f32, slot.orig[1] as f32);
        let (scale, pan) = match zoom {
            Some(z) => (z, self.pan),
            None => {
                let fit = (rect.width() * ppp / size.x).min(rect.height() * ppp / size.y);
                let fit = if self.config.enlarge {
                    fit
                } else {
                    fit.min(1.0)
                };
                (fit, egui::Vec2::ZERO)
            }
        };
        let at = egui::Rect::from_center_size(rect.center() + pan, size * scale / ppp);
        let uv = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
        let p = ui.painter().with_clip_rect(rect);
        let shown = match &fade {
            Some((t, old, orig)) => {
                let from = fitted(rect, *orig, ppp, self.config.enlarge);
                p.image(old.id(), from, uv, alpha(1.0 - t.powi(4)));
                *t
            }
            None => 1.0,
        };
        p.image(slot.tex.id(), at, uv, alpha(shown));
        Some(scale)
    }

    /// The compared image in `rect`, with its name above; which it is and its scale.
    fn paint_other(
        &mut self,
        ui: &egui::Ui,
        rect: egui::Rect,
        zoom: Option<f32>,
    ) -> Option<(usize, Option<f32>)> {
        ui.painter().vline(
            rect.left() - 1.0,
            rect.y_range(),
            egui::Stroke::new(2.0, egui::Color32::from_gray(60)),
        );
        let other = self.compare_other()?;
        let scale = self.paint_pane(ui, rect, other, zoom, None);
        self.paint_name(ui, rect, other);
        Some((other, scale))
    }

    /// The name of image `i` in the top left corner of `rect`.
    fn paint_name(&self, ui: &egui::Ui, rect: egui::Rect, i: usize) {
        let p = ui.painter();
        let font = egui::TextStyle::Body.resolve(ui.style());
        let galley = p.layout_no_wrap(self.name(i).into_owned(), font, egui::Color32::WHITE);
        let text = egui::Rect::from_min_size(rect.left_top() + egui::vec2(8.0, 8.0), galley.size());
        p.rect_filled(text.expand(4.0), 3.0, egui::Color32::from_black_alpha(160));
        p.galley(text.min, galley, egui::Color32::WHITE);
    }

    /// The shown image's histogram in the bottom right corner of `area`.
    fn paint_histogram(&self, ui: &egui::Ui, area: egui::Rect) {
        let Some(slot) = self.slots.get(&self.cur) else {
            return;
        };
        let size = egui::vec2(256.0, 100.0);
        let rect =
            egui::Rect::from_min_size(area.right_bottom() - size - egui::vec2(16.0, 16.0), size);
        let p = ui.painter();
        p.rect_filled(rect.expand(6.0), 4.0, egui::Color32::from_black_alpha(170));
        use egui::Color32 as C;
        let channels: &[(usize, C)] = if self.config.histogram == 1 {
            &[
                (0, C::from_rgb(255, 80, 80)),
                (1, C::from_rgb(80, 220, 80)),
                (2, C::from_rgb(90, 140, 255)),
            ]
        } else {
            &[(3, C::from_gray(230))]
        };
        let log = self.config.histogram_log;
        let height = |v: u32| if log { (v as f32).ln_1p() } else { v as f32 };
        let max = channels
            .iter()
            .flat_map(|c| slot.hist[c.0].iter())
            .map(|&v| height(v))
            .fold(1.0, f32::max);
        for &(c, color) in channels {
            let points = (0..256)
                .map(|i| {
                    let x = rect.left() + i as f32 * rect.width() / 255.0;
                    egui::pos2(
                        x,
                        rect.bottom() - height(slot.hist[c][i]) / max * rect.height(),
                    )
                })
                .collect();
            p.add(egui::Shape::line(points, egui::Stroke::new(1.5, color)));
        }
    }

    /// Fuzzy search box above the file list.
    fn search_field(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if !self.search.is_empty() && ui.small_button(theme::icon::CLOSE).clicked() {
                self.search.clear();
                self.update_search(ui.ctx());
                self.scroll_to_cur = true;
            }
            // A fixed ID: the ✖ button appearing in front of it must not make it a new
            // widget, which would lose the focus after the first letter.
            let hint = match self.config.fuzzy_search {
                true => "Search file names (3+ letters), re: pattern, or exif: camera, date …",
                false => "Filter file names by pattern, or exif: camera, date …",
            };
            let edit = egui::TextEdit::singleline(&mut self.search)
                .id(egui::Id::new("file search"))
                .hint_text(hint);
            // Geeqie's file filter has its Case checkbox to the right.
            let geeqie = !self.config.fuzzy_search;
            let width = ui.available_width() - if geeqie { 64.0 } else { 0.0 };
            let mut changed = ui.add(edit.desired_width(width)).changed();
            if geeqie
                && ui
                    .checkbox(&mut self.config.search_case, "Case")
                    .on_hover_text("Tell upper and lower case apart")
                    .changed()
            {
                self.config.save();
                changed = true;
            }
            if changed {
                self.update_search(ui.ctx());
            }
        });
        if let Some(e) = &self.search_error {
            ui.colored_label(ui.visuals().error_fg_color, format!("Not a pattern: {e}"));
        }
    }

    /// The files the list shows: the search matches, or all.
    fn shown_files(&self) -> Vec<usize> {
        (self.matches.clone()).unwrap_or_else(|| (0..self.files.len()).collect())
    }

    /// The row of the shown file among the search matches `m`; `None` if the search hides it.
    fn cur_row(&self, m: &[usize]) -> Option<usize> {
        match self.cur_row.get() {
            Some((cur, row)) if cur == self.cur => row,
            _ => {
                let row = m.iter().position(|&i| i == self.cur);
                self.cur_row.set(Some((self.cur, row)));
                row
            }
        }
    }

    /// The file `delta` rows from the shown one, in the order the list shows (the search matches
    /// while searching); clamped at the ends, or going round with `wrap`. A file the search
    /// hides counts as being before the first row.
    fn step(&self, delta: isize, wrap: bool) -> Option<usize> {
        self.step_from(self.cur, delta, wrap)
    }

    /// As `step`, from file `from`.
    fn step_from(&self, from: usize, delta: isize, wrap: bool) -> Option<usize> {
        let m = self.matches.as_deref();
        let n = m.map_or(self.files.len(), <[usize]>::len) as isize;
        if n == 0 {
            return None;
        }
        let row = match m {
            Some(m) if from == self.cur => self.cur_row(m),
            Some(m) => m.iter().position(|&i| i == from),
            None => Some(from),
        }
        .map_or(-1, |r| r as isize);
        let r = if wrap {
            (row + delta).rem_euclid(n)
        } else {
            (row + delta).clamp(0, n - 1)
        } as usize;
        Some(m.map_or(r, |m| m[r]))
    }

    /// Select the list rows from file `a` to file `b`, in the order the list shows them (the
    /// search matches while searching).
    fn select_range(&mut self, a: usize, b: usize) {
        let m = self.matches.as_deref();
        let row = |i| m.map_or(Some(i), |m| m.iter().position(|&x| x == i));
        match (row(a), row(b), m) {
            (Some(ra), Some(rb), Some(m)) => self.selected.extend(&m[ra.min(rb)..=ra.max(rb)]),
            (Some(ra), Some(rb), None) => self.selected.extend(ra.min(rb)..=ra.max(rb)),
            _ => {
                self.selected.insert(b);
            }
        }
    }

    /// Run the search again; file indices change when the list does.
    fn update_search(&mut self, ctx: &egui::Context) {
        let query = self.search.trim().to_owned();
        let query = query.as_str();
        self.cur_row.set(None);
        self.search_error = None;
        // Geeqie's Case checkbox, shown beside the field in its mode.
        let case = self.config.search_case && !self.config.fuzzy_search;
        self.matches = match (query.strip_prefix("exif:"), query.strip_prefix("re:")) {
            (Some(words), _) => Some(self.exif_matches(ctx, words)),
            (_, Some(pattern)) if !pattern.is_empty() => Some(match search::regex(pattern, case) {
                Ok(re) => search::regex_matches(&self.files, &re),
                Err(e) => {
                    self.search_error = Some(e);
                    Vec::new()
                }
            }),
            _ => {
                self.mark_matches(query)
                    .or_else(|| match self.config.fuzzy_search {
                        true => (query.chars().count() >= 3)
                            .then(|| search::matches(&self.files, query)),
                        // As Geeqie's file filter: a regular expression, from the first letter.
                        false => (!query.is_empty()).then(|| match search::regex(query, case) {
                            Ok(re) => search::regex_matches(&self.files, &re),
                            Err(e) => {
                                self.search_error = Some(e);
                                Vec::new()
                            }
                        }),
                    })
            }
        };
        // Size of the matches in the background; a new search drops the receiver, which
        // stops the old sum.
        self.match_size = 0;
        self.match_rx = None;
        let Some(m) = &self.matches else { return };
        let names: Vec<OsString> = m.iter().map(|&i| self.files[i].clone()).collect();
        let (tx, rx) = mpsc::channel();
        let (dir, ctx) = (self.dir.clone(), ctx.clone());
        std::thread::spawn(move || {
            let sizes = names
                .iter()
                .filter_map(|n| Some(dir.join(n).metadata().ok()?.len()));
            send_sizes(sizes, tx, &ctx);
        });
        self.match_rx = Some(rx);
    }

    /// Files whose EXIF contains every word, or that a regular expression after "re:" finds, in
    /// list order. The first such search in a folder reads the EXIF of all files in the
    /// background (the busy pointer shows meanwhile) and finds nothing until that is done.
    /// ponytail: files added after the index was built do not match until the folder is reopened.
    fn exif_matches(&mut self, ctx: &egui::Context, words: &str) -> Vec<usize> {
        let re = match words.strip_prefix("re:").map(|p| search::regex(p, false)) {
            Some(Err(e)) => {
                self.search_error = Some(e);
                return Vec::new();
            }
            Some(Ok(re)) => Some(re),
            None => None,
        };
        let Some(index) = &self.exif_index else {
            if self.exif_rx.is_none() {
                let (tx, rx) = mpsc::channel();
                let (dir, files, ctx) = (self.dir.clone(), self.files.clone(), ctx.clone());
                let busy = self.busy.clone();
                busy.fetch_add(1, Ordering::Relaxed);
                std::thread::spawn(move || {
                    let _ = tx.send(exif_index(&dir, &files));
                    busy.fetch_sub(1, Ordering::Relaxed);
                    ctx.request_repaint();
                });
                self.exif_rx = Some(rx);
            }
            return Vec::new();
        };
        // The index is lowercase, as the words are made; a regular expression ignores case.
        let words = words.to_lowercase();
        let words: Vec<&str> = words.split_whitespace().collect();
        let found = |text: &String| match &re {
            Some(re) => re.is_match(text.as_bytes()),
            None => words.iter().all(|w| text.contains(w)),
        };
        (0..self.files.len())
            .filter(|&i| index.get(&self.files[i]).is_some_and(found))
            .collect()
    }

    /// "mark:2" shows the files with mark 2, "stars:3" those with three stars or more.
    #[cfg(feature = "marks")]
    fn mark_matches(&self, query: &str) -> Option<Vec<usize>> {
        let (mark, n) = match (query.strip_prefix("mark:"), query.strip_prefix("stars:")) {
            (Some(n), _) => (
                true,
                n.trim()
                    .parse::<u8>()
                    .ok()
                    .filter(|n| (1..=6).contains(n))?,
            ),
            (_, Some(n)) => (false, n.trim().parse::<u8>().ok()?),
            _ => return None,
        };
        let hit = |(bits, stars): (u8, u8)| {
            if mark {
                bits & (1 << (n - 1)) != 0
            } else {
                stars >= n
            }
        };
        Some(
            (0..self.files.len())
                .filter(|&i| hit(self.marks.get(&self.path(i))))
                .collect(),
        )
    }

    #[cfg(not(feature = "marks"))]
    fn mark_matches(&self, _: &str) -> Option<Vec<usize>> {
        None
    }

    /// Stars and marks of file `i` as text ("★★★  1 4"); empty without the marks feature.
    #[cfg(feature = "marks")]
    fn mark_text(&self, i: usize) -> String {
        if !self.config.show_marks {
            return String::new();
        }
        marks::text(self.marks.get(&self.path(i)))
    }

    #[cfg(not(feature = "marks"))]
    fn mark_text(&self, _: usize) -> String {
        String::new()
    }

    /// Size and modification time of file `i`, from the cache or one stat.
    /// ponytail: stat in the UI thread, once per visible file; move to a thread if network
    /// folders make scrolling stutter.
    fn file_details(&mut self, i: usize) -> (u64, i64) {
        if let Some(&d) = self.details.get(&self.files[i]) {
            return d;
        }
        let d = std::fs::metadata(self.path(i)).map_or((0, 0), |m| {
            use std::os::unix::fs::MetadataExt;
            (m.len(), m.mtime())
        });
        self.details.insert(self.files[i].clone(), d);
        d
    }

    /// Drop thumbnails far from view once there are many, so memory stays flat in huge folders.
    /// Over the budget, drop the thumbnails of all but the rows `rows` of the list as shown (the
    /// search matches, or all files) and those near them.
    fn trim_thumbs(&mut self, rows: std::ops::Range<usize>) {
        // Big thumbnails take four times the memory: keep a quarter as many.
        let max = if self.config.thumb_size == 2 {
            MAX_THUMBS / 4
        } else {
            MAX_THUMBS
        };
        if self.thumbs.len() <= max {
            return;
        }
        let mut keep = thumbs_to_keep(&self.files, self.matches.as_deref(), rows, max);
        keep.extend(self.pan_thumbs.iter().filter_map(|&i| self.files.get(i)));
        self.thumbs.retain(|name, _| keep.contains(name));
    }

    /// The action for a shortcut pressed this frame, if any.
    /// `delete_key` false: the Delete key does nothing (Ctrl+D still deletes), as in Geeqie.
    fn shortcut(ctx: &egui::Context, delete_key: bool, collection: bool) -> Option<Action> {
        ctx.input_mut(|i| {
            // egui-winit turns Ctrl+C into a Copy event instead of a key press.
            let all = shortcuts();
            let copies = i.events.len();
            i.events.retain(|e| !matches!(e, egui::Event::Copy));
            if i.events.len() < copies {
                // Whatever has Ctrl+C: Copy, unless the user gave the key to another action.
                return (all.iter())
                    .find(|s| s.1 == CTRL && s.2 == egui::Key::C)
                    .map(|s| s.0);
            }
            all.iter()
                .filter(|s| collection || s.0 != Action::RemoveFromCollection)
                .filter(|s| {
                    delete_key || s.2 != egui::Key::Delete || s.0 == Action::RemoveFromCollection
                })
                .find(|&&(_, mods, key)| {
                    i.consume_shortcut(&egui::KeyboardShortcut::new(mods, key))
                })
                .map(|s| s.0)
        })
    }

    fn zoom_by(&mut self, factor: f32) {
        self.zoom = Some((self.zoom.unwrap_or(self.last_scale) * factor).clamp(0.02, 32.0));
    }

    fn run(&mut self, ctx: &egui::Context, action: Action) {
        // A kiosk shows and browses; it does not change files, settings or the window.
        if self.kiosk && !KIOSK_ACTIONS.contains(&action) && !self.kiosk_quit {
            return;
        }
        let fullscreen = ctx.input(|i| i.viewport().fullscreen).unwrap_or(false);
        match action {
            Action::OpenFile => self.pick(ctx, false, Pick::Open),
            Action::OpenFolder => self.pick(ctx, true, Pick::Open),
            Action::OpenFolders => self.pick(ctx, true, Pick::Folders),
            Action::NewCollection if !self.files.is_empty() => {
                self.prompt = Some((Prompt::NewCollection, String::new()));
            }
            Action::NewCollection => self.notice = Some("No images for a collection here".into()),
            Action::OpenCollection => self.pick(ctx, false, Pick::Collection),
            Action::RemoveFromCollection if self.collection.is_some() && !self.files.is_empty() => {
                let targets = self.targets();
                self.remove_indices(ctx, &targets);
            }
            Action::RemoveFromCollection => {}
            Action::FindDuplicates => drop(self.dups.get_or_insert_with(Dups::new)),
            Action::Copy | Action::Move if !self.files.is_empty() => {
                let paths = self.targets().iter().map(|&i| self.path(i)).collect();
                let purpose = match action {
                    Action::Copy => Pick::CopyTo(paths),
                    _ => Pick::MoveTo(paths),
                };
                self.pick(ctx, true, purpose);
            }
            Action::Copy | Action::Move => {}
            // The default application; the Open With submenu lists the others.
            Action::OpenWith => self.launch(""),
            // Enter: videos play in a player (images have nothing to play).
            Action::Play => {
                #[cfg(feature = "video")]
                if is_video(&self.location()) {
                    self.launch(&self.config.video_player);
                    // The slideshow waits while the video plays; P resumes it.
                    if self.slideshow.is_some() {
                        self.paused = true;
                        self.title(ctx);
                    }
                }
            }
            Action::Delete => self.request_delete(ctx, self.config.trash),
            Action::DeletePermanently => self.request_delete(ctx, false),
            Action::CloseWindow | Action::Quit => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close)
            }
            Action::First | Action::Prev | Action::Next | Action::Last => {
                // Along the list as shown: the search matches while searching.
                let row = self.row_step;
                let delta = match action {
                    Action::First => -isize::MAX / 2,
                    Action::Prev => -row,
                    Action::Next => row,
                    _ => isize::MAX / 2,
                };
                let Some(to) = self.step(delta, false) else {
                    return;
                };
                // With Shift the selection runs from where it started to the new image, as in
                // a file manager; moving without Shift clears it.
                if ctx.input(|i| i.modifiers.shift) {
                    let anchor = *self.anchor.get_or_insert(self.cur);
                    self.selected.clear();
                    self.select_range(anchor, to);
                    self.selection_changed(ctx);
                } else {
                    self.anchor = None;
                    if !self.selected.is_empty() {
                        self.selected.clear();
                        self.selection_changed(ctx);
                    }
                }
                self.go(ctx, to);
            }
            Action::Back | Action::Forward => {
                let here = self.location();
                let (from, to) = if action == Action::Back {
                    (&mut self.back, &mut self.forward)
                } else {
                    (&mut self.forward, &mut self.back)
                };
                if let Some(path) = from.pop() {
                    to.push(here);
                    self.open(ctx, path);
                }
            }
            Action::Home => {
                if let Some(home) = std::env::var_os("HOME") {
                    self.visit(ctx, home.into());
                }
            }
            Action::Up => {
                if let Some(parent) = self.dir.parent() {
                    self.visit(ctx, parent.to_path_buf());
                }
            }
            // Select All and Invert act on the files the list shows: the matches while searching.
            Action::SelectAll => {
                self.selected = self.shown_files().into_iter().collect();
                self.selection_changed(ctx);
            }
            Action::SelectNone => {
                self.selected.clear();
                self.selection_changed(ctx);
            }
            Action::InvertSelection => {
                let shown = self.shown_files().into_iter();
                self.selected = shown.filter(|i| !self.selected.contains(i)).collect();
                self.selection_changed(ctx);
            }
            Action::CopyPath | Action::CopyPathUnquoted if !self.files.is_empty() => {
                let quote = action == Action::CopyPath;
                let paths: Vec<String> = self
                    .targets()
                    .iter()
                    .map(|&i| self.path(i).to_string_lossy().into_owned())
                    .map(|p| if quote { format!("\"{p}\"") } else { p })
                    .collect();
                ctx.copy_text(paths.join(" "));
            }
            Action::CopyPath | Action::CopyPathUnquoted => {}
            // Decoded at full size in the background; egui hands it to the clipboard.
            Action::CopyImage if !self.files.is_empty() => {
                let (path, rotate) = (self.path(self.cur), self.config.exif_rotate);
                let (busy, tx, ctx) = (self.busy.clone(), self.notices_tx.clone(), ctx.clone());
                let (clip_tx, clip_rx) = mpsc::channel();
                self.clip = Some(clip_rx);
                busy.fetch_add(1, Ordering::Relaxed);
                std::thread::spawn(move || {
                    match load(&path, rotate) {
                        Ok(img) => drop(clip_tx.send(color_image(img))),
                        Err(e) => drop(tx.send(format!("Could not copy the image: {e}"))),
                    }
                    busy.fetch_sub(1, Ordering::Relaxed);
                    ctx.request_repaint();
                });
            }
            Action::CopyImage => {}
            // As a list of files, which a file manager pastes as a copy.
            Action::CopyFiles if !self.files.is_empty() => {
                let paths: Vec<PathBuf> = self.targets().iter().map(|&i| self.path(i)).collect();
                let clipboard = match self.clipboard.take() {
                    Some(c) => Ok(c),
                    None => arboard::Clipboard::new(),
                };
                let result = clipboard.and_then(|mut c| {
                    let set = c.set().file_list(&paths);
                    // On X11 the clipboard is served from this object: keep it.
                    self.clipboard = Some(c);
                    set
                });
                if let Err(e) = result {
                    self.notice = Some(format!("Could not copy to the clipboard: {e}"));
                }
            }
            Action::CopyFiles => {}
            Action::Preferences => self.show_settings = true,
            Action::Help => self.help = Some(help::Page::Manual),
            Action::Shortcuts => self.help = Some(help::Page::Shortcuts),
            Action::KeyboardMap => self.help = Some(help::Page::Keymap),
            Action::About => self.help = Some(help::Page::About),
            Action::Thumbnails => self.set_thumbnails(!self.config.thumbnails),
            Action::SaveOrientation => self.save_orientation(ctx),
            Action::Subfolders => self.set_subfolders(ctx, !self.config.show_subfolders),
            Action::HiddenFiles => {
                self.config.show_hidden = !self.config.show_hidden;
                self.config.save();
                // Read again, and scrolled back to the open folder.
                self.tree.clear();
                self.tree_shown = PathBuf::new();
                self.list(ctx);
            }
            Action::NextPage | Action::PrevPage => {
                let pages = self.slots.get(&self.cur).map_or(1, |s| s.pages);
                let page = match action {
                    Action::NextPage => (self.page + 1).min(pages - 1),
                    _ => self.page.saturating_sub(1),
                };
                if page != self.page {
                    self.page = page;
                    self.slots.remove(&self.cur);
                    self.pending.retain(|p| p.0 != self.cur);
                    self.refill();
                }
            }
            Action::ExifWindow => self.exif_window = !self.exif_window,
            Action::Keywords => {
                self.config.exif_panel = true;
                self.config.save();
                self.focus_keywords = true;
            }
            Action::PanView => {
                self.pan_window = match self.pan_window {
                    Some(_) => None,
                    None => Some(Pan::default()),
                }
            }
            Action::OneImage => self.set_layout("1"),
            Action::AboveEachOther => self.set_layout("1x2"),
            // As a click on that pane: its image becomes the shown one, in the first pane.
            Action::PaneLeft | Action::PaneRight | Action::PaneUp | Action::PaneDown => {
                let cols = layout_grid(&self.config.layout)[0] as isize;
                let step = match action {
                    Action::PaneLeft => -1,
                    Action::PaneRight => 1,
                    Action::PaneUp => -cols,
                    _ => cols,
                };
                // Forward, the panes say which image is there (a slideshow may change them
                // one at a time); back, the list does.
                let pane = (step > 0)
                    .then(|| self.panes.get(step as usize - 1))
                    .flatten();
                if let Some(i) = pane.copied().or_else(|| self.step(step, false)) {
                    self.go(ctx, i);
                }
            }
            Action::NextLayout => {
                let at = LAYOUTS.iter().position(|l| l.0 == self.config.layout);
                let next = LAYOUTS[at.map_or(0, |k| (k + 1) % LAYOUTS.len())].0;
                self.set_layout(next);
            }
            Action::Compare => {
                self.compare = !self.compare;
                if self.compare && self.config.layout != "1" {
                    self.config.layout = "1".into();
                    self.config.save();
                }
                self.refill();
            }
            #[cfg(feature = "marks")]
            Action::Mark1
            | Action::Mark2
            | Action::Mark3
            | Action::Mark4
            | Action::Mark5
            | Action::Mark6
            | Action::SelectMark1
            | Action::SelectMark2
            | Action::SelectMark3
            | Action::SelectMark4
            | Action::SelectMark5
            | Action::SelectMark6
            | Action::Rate0
            | Action::Rate1
            | Action::Rate2
            | Action::Rate3
            | Action::Rate4
            | Action::Rate5 => {
                let Some((kind, n)) = action.mark() else {
                    return;
                };
                if kind == 's' {
                    let bit = 1 << (n - 1);
                    self.selected = (self.shown_files().into_iter())
                        .filter(|&i| self.marks.get(&self.path(i)).0 & bit != 0)
                        .collect();
                    self.anchor = None;
                    self.selection_changed(ctx);
                    return;
                }
                if self.files.is_empty() {
                    return;
                }
                let paths: Vec<PathBuf> = self.targets().iter().map(|&i| self.path(i)).collect();
                if kind == 'm' {
                    self.marks.toggle(&paths, n);
                } else {
                    self.marks.rate(&paths, n);
                    if self.config.xmp_stars {
                        let failed = paths.iter().find_map(|p| marks::write_xmp(p, n).err());
                        if let Some(e) = failed {
                            self.notice = Some(format!("Could not write the XMP sidecar: {e}"));
                        }
                    }
                }
                // A "mark:" or "stars:" search shows the new state.
                if self.search.starts_with("mark:") || self.search.starts_with("stars:") {
                    self.update_search(ctx);
                }
            }
            Action::Histogram => {
                self.config.histogram = (self.config.histogram + 1) % 3;
                self.config.save();
            }
            Action::PathBar
            | Action::Toolbar
            | Action::ExifPanel
            | Action::SortManager
            | Action::Overlay
            | Action::Icons
            | Action::FolderTree
            | Action::HistogramLog
            | Action::Animate => {
                self.scroll_to_cur = true;
                let c = &mut self.config;
                let flag = match action {
                    Action::PathBar => &mut c.path_bar,
                    Action::Toolbar => &mut c.toolbar,
                    Action::Overlay => &mut c.osd,
                    Action::Icons => &mut c.icons,
                    Action::FolderTree => &mut c.folder_tree,
                    Action::HistogramLog => &mut c.histogram_log,
                    Action::Animate => &mut c.animate,
                    Action::SortManager => &mut c.sort_manager,
                    _ => &mut c.exif_panel,
                };
                *flag = !*flag;
                c.save();
            }
            Action::NewFolder => self.prompt = Some((Prompt::NewFolder, String::new())),
            Action::Print if !self.files.is_empty() => {
                let (tx, rx) = mpsc::channel();
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(print::printers());
                    ctx.request_repaint();
                });
                self.print = Some(PrintDialog {
                    paths: self.targets().iter().map(|&i| self.path(i)).collect(),
                    printers: None,
                    printers_rx: rx,
                    copies: 1,
                });
            }
            Action::Print => {}
            Action::Rename if self.targets().len() > 1 => {
                self.asked_for = (self.files_gen, self.cur);
                self.prompt = Some((Prompt::RenameMany, "*_###".into()));
            }
            Action::Rename => {
                // The name only: in the list with subfolders the entry has its folders too.
                if let Some(name) = self
                    .files
                    .get(self.cur)
                    .and_then(|f| Path::new(f).file_name())
                {
                    self.asked_for = (self.files_gen, self.cur);
                    self.prompt = Some((Prompt::Rename, name.to_string_lossy().into_owned()));
                }
            }
            // A separate process: windows share nothing but the config file.
            Action::NewWindow => {
                let exe = std::env::current_exe().map(|exe| {
                    let var = std::env::var_os;
                    own_program(exe, var("APPDIR"), var("APPIMAGE"))
                });
                // After an update (or a rebuild) Linux reports the running binary as
                // "/path/glimt (deleted)"; the new one is at the same path.
                let started = exe.and_then(|exe| {
                    let exe = exe.to_string_lossy();
                    let exe = exe.strip_suffix(" (deleted)").unwrap_or(&exe);
                    std::process::Command::new(exe).arg(self.location()).spawn()
                });
                match started {
                    // Reap the child when it exits, so it does not linger as a zombie.
                    Ok(mut child) => drop(std::thread::spawn(move || child.wait())),
                    Err(e) => self.notice = Some(format!("Could not open a new window: {e}")),
                }
            }
            Action::ExifRotate => self.set_exif_rotate(!self.config.exif_rotate),
            // Mirroring or flipping what is on screen after a turn: mirror the image first and
            // turn the other way (a flip is a mirror and half a turn).
            Action::RotateCw
            | Action::RotateCcw
            | Action::Rotate180
            | Action::Mirror
            | Action::Flip => {
                // Until it is saved the old picture shows: turned again, it would be saved twice,
                // into the same temporary file.
                if !self.files.is_empty() && self.saving.contains(&self.path(self.cur)) {
                    self.notice = Some("Still saving the last rotation of this file".into());
                    return;
                }
                let (q, m) = self.orient;
                self.orient = match action {
                    Action::RotateCw => ((q + 1) % 4, m),
                    Action::RotateCcw => ((q + 3) % 4, m),
                    Action::Rotate180 => ((q + 2) % 4, m),
                    Action::Mirror => ((4 - q) % 4, !m),
                    _ => ((6 - q) % 4, !m),
                };
            }
            Action::ZoomIn => self.zoom_by(1.25),
            Action::ZoomOut => self.zoom_by(0.8),
            Action::Zoom1To1 => self.zoom = Some(1.0),
            Action::ZoomFit => {
                self.zoom = None;
                self.pan = egui::Vec2::ZERO;
            }
            Action::FullScreen => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fullscreen))
            }
            Action::HideTools => self.hide_tools = !self.hide_tools,
            Action::Refresh => {
                self.tree.clear();
                self.slots.clear();
                self.pending.clear();
                self.errors.clear();
                self.list(ctx);
            }
            Action::Slideshow if self.slideshow.is_some() => self.stop_slideshow(ctx),
            Action::Slideshow => self.start_slideshow(ctx),
            // Resuming gives the shown image the full delay again.
            Action::Pause if self.slideshow.is_some() => {
                self.paused = !self.paused;
                self.slideshow = Some(Instant::now());
                self.title(ctx);
            }
            Action::Pause => {}
            Action::RandomOrder => self.set_random_order(ctx, !self.config.random),
            Action::Faster | Action::Slower => {
                let step = if action == Action::Faster { -1 } else { 1 };
                self.config.delay = self
                    .config
                    .delay
                    .saturating_add_signed(step)
                    .clamp(1, MAX_DELAY);
                self.config.save();
                self.title(ctx);
            }
            // As in Geeqie, Escape never quits; it stops the slideshow and leaves full screen.
            Action::Escape => {
                self.help = None;
                if self.slideshow.is_some() {
                    self.stop_slideshow(ctx);
                }
                if fullscreen {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                }
            }
        }
    }
}

// The parts of a frame, in the order `ui` draws them.
impl App {
    /// What the background threads and the rest of the desktop sent since the last frame: decoded
    /// images and thumbnails, the folder listing, saves, remote commands, dropped files, mounts,
    /// notices and the file dialog's answer.
    fn poll_background(&mut self, ctx: &egui::Context) {
        if let Some(look) = self.desktop_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.desktop_look = look;
            self.desktop_rx = None;
            self.apply_theme(ctx);
        }
        if let Some(path) = (self.desktop_screen_rx.as_ref()).and_then(|rx| rx.try_recv().ok()) {
            self.desktop_screen_rx = None;
            self.desktop_screen = path;
            if self.desktop_screen.is_some() {
                self.use_screen();
            }
        }
        let max_side = ctx.input(|i| i.max_texture_side) as u32;
        self.limits.lock().unwrap()[1] = [max_side, max_side];
        self.receive(ctx);
        self.receive_thumbs(ctx);
        self.receive_listing(ctx, std::time::Duration::ZERO);
        self.receive_sizes(ctx);
        self.receive_deleted();
        while let Ok((path, result)) = self.saved_rx.try_recv() {
            self.saving.remove(&path);
            match result {
                Ok(()) => self.saved(&path),
                Err(e) => self.notice = Some(format!("Could not save the orientation: {e}")),
            }
        }
        // --slideshow: once the folder is read, so random order shuffles all of it.
        if self.start_slideshow && self.listing.is_none() {
            self.start_slideshow = false;
            self.start_slideshow(ctx);
        }
        let focus = ctx.input(|i| i.viewport().focused.unwrap_or(false));
        if focus && !self.has_focus && self.config.remote_control {
            remote::focused();
        }
        self.has_focus = focus;
        while let Ok(command) = self.remote.try_recv() {
            self.remote_command(ctx, &command);
        }
        if let Some(img) = self.clip.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.clip = None;
            ctx.copy_image(img);
        }
        // A file or folder dropped on the window opens there, as File → Open would.
        let dropped = ctx.input(|i| {
            let mut paths = i.raw.dropped_files.iter().map(|f| f.path().to_path_buf());
            paths.find(|p| !p.as_os_str().is_empty())
        });
        if let Some(path) = dropped.filter(|_| !self.kiosk) {
            self.visit(ctx, path);
        }
        // Busy pointer while the folder is read, files are deleted, copied or moved, or the
        // shown image is still being decoded.
        let decoding = !self.files.is_empty()
            && !self.slots.contains_key(&self.cur)
            && !self.errors.contains_key(&self.cur);
        if let Some(done) = self.mounting.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.mounting = None;
            match done {
                (Ok(path), _) => self.visit(ctx, path),
                (Err(Some(e)), _) => self.notice = Some(e),
                // Ask for the login; the user name defaults to this one, as gio's does.
                (Err(None), login) => {
                    let user = std::env::var("USER").unwrap_or_default();
                    self.login = Some(Login {
                        user,
                        domain: "WORKGROUP".into(),
                        ..login
                    });
                }
            }
        }
        if self.listing.is_some()
            || decoding
            || self.mounting.is_some()
            || self.busy.load(Ordering::Relaxed) > 0
        {
            ctx.set_cursor_icon(egui::CursorIcon::Progress);
        }
        if self.changed_rx.try_iter().count() > 0 && self.config.auto_refresh {
            self.stale = true;
        }
        if self.stale && self.listing.is_none() {
            self.list(ctx);
        }
        while let Ok(msg) = self.notices.try_recv() {
            self.notice = Some(msg);
            ctx.request_repaint();
        }
        self.slideshow_tick(ctx);
        if let Some(all) = self.together.take() {
            self.open_folders(ctx, all);
        }
        if let Some(Ok(picked)) = self.dialog.as_ref().map(|(_, rx)| rx.try_recv()) {
            let (purpose, _) = self.dialog.take().unwrap();
            match (purpose, picked) {
                (_, None) => {}
                (Pick::Open | Pick::Collection, Some(path)) => self.visit(ctx, path),
                // One per line, from zenity's --multiple.
                (Pick::Folders, Some(paths)) => {
                    use std::os::unix::ffi::OsStringExt;
                    let bytes = paths.into_os_string().into_encoded_bytes();
                    let all = (bytes.split(|&b| b == b'\n'))
                        .filter(|p| !p.is_empty())
                        .map(|p| PathBuf::from(OsString::from_vec(p.to_vec())))
                        .collect();
                    self.open_folders(ctx, all);
                }
                (Pick::CopyTo(paths), Some(dest)) => self.transfer(ctx, paths, dest, true),
                (Pick::MoveTo(paths), Some(dest)) => self.transfer(ctx, paths, dest, false),
                (Pick::SavePdf(job), Some(file)) => self.print(ctx, job, PrintTo::File(file)),
                (Pick::SortFolder, Some(dir)) => self.add_sort_folder(dir),
                (Pick::ScreenProfile, Some(file)) => {
                    self.set_screen_profile(file.to_string_lossy().into_owned())
                }
                (Pick::InputProfile, Some(file)) => {
                    self.set_input_profile(file.to_string_lossy().into_owned())
                }
            }
        }
    }

    /// A command from `glimt --remote`: next, prev, first, last, slideshow, random, fullscreen,
    /// quit, or open PATH.
    fn remote_command(&mut self, ctx: &egui::Context, command: &str) {
        let action = match command.trim() {
            "next" => Some(Action::Next),
            "prev" => Some(Action::Prev),
            "first" => Some(Action::First),
            "last" => Some(Action::Last),
            "slideshow" => Some(Action::Slideshow),
            "random" => Some(Action::RandomOrder),
            "fullscreen" => Some(Action::FullScreen),
            "quit" => {
                // The only way out of a kiosk.
                self.kiosk_quit = true;
                Some(Action::Quit)
            }
            other => {
                if let Some(path) = other.strip_prefix("open ") {
                    self.visit(ctx, PathBuf::from(path));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                None
            }
        };
        if let Some(action) = action {
            self.run(ctx, action);
        }
    }

    /// The action of the key pressed this frame, if any; the arrow keys move a zoomed image first.
    fn read_keys(&mut self, ctx: &egui::Context) -> Option<Action> {
        // Shortcuts go to the dialogs while one is open.
        // ... and to the text field while one has focus (the toolbar's folder path).
        let typing = ctx.egui_wants_keyboard_input();
        // A zoomed image larger than the window: the arrow keys move it, as in Geeqie; along a
        // side where it fits they still browse.
        if !self.dialog_open() && !typing && self.zoom.is_some() {
            let step = 100.0;
            let max = self.max_pan;
            ctx.input_mut(|i| {
                for (key, d) in [
                    (egui::Key::ArrowLeft, egui::vec2(step, 0.0)),
                    (egui::Key::ArrowRight, egui::vec2(-step, 0.0)),
                    (egui::Key::ArrowUp, egui::vec2(0.0, step)),
                    (egui::Key::ArrowDown, egui::vec2(0.0, -step)),
                ] {
                    let along = if d.x != 0.0 { max.x } else { max.y };
                    if along > 0.0 && i.consume_key(egui::Modifiers::NONE, key) {
                        self.pan = (self.pan + d).clamp(-max, max);
                    }
                }
            });
        }
        // In the icon grid ↑ and ↓ move a row; read before the shortcut takes the key.
        let vertical =
            ctx.input(|i| i.key_pressed(egui::Key::ArrowUp) || i.key_pressed(egui::Key::ArrowDown));
        self.row_step = if vertical { self.grid_cols as isize } else { 1 };
        (!self.dialog_open() && !typing)
            .then(|| Self::shortcut(ctx, self.config.delete_key, self.collection.is_some()))
            .flatten()
    }

    /// Keeps a kiosk in full screen and the screensaver off in full screen, and remembers the
    /// window's size and place; whether the window is full screen.
    fn track_window(&mut self, ctx: &egui::Context) -> bool {
        let fullscreen = ctx.input(|i| i.viewport().fullscreen).unwrap_or(false);
        if self.kiosk && !self.kiosk_quit {
            // Closing the window is refused, and full screen comes back if the desktop ends it.
            if ctx.input(|i| i.viewport().close_requested()) {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            }
            if ctx.input(|i| i.viewport().fullscreen) == Some(false) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
            }
        }
        self.inhibit(fullscreen && self.config.inhibit_screensaver);
        // Geometry of the normal window only, so leaving full screen or maximized restores it.
        // The size from egui's own area: the viewport's inner_rect needs the window position,
        // which Wayland does not give, so it stayed empty there and the size was never saved.
        let (size, outer, maximized) = ctx.input(|i| {
            let v = i.viewport();
            (
                i.viewport_rect().size(),
                v.outer_rect,
                v.maximized.unwrap_or(false),
            )
        });
        self.window.maximized = maximized;
        if !fullscreen && !maximized {
            self.window.size = Some(size);
            // Wayland reports no position.
            self.window.pos = outer.map(|r| r.min).or(self.window.pos);
        }
        fullscreen
    }

    /// The menu bar, as Geeqie's, and the toolbar under it.
    fn menu_bar(&mut self, ui: &mut egui::Ui, act: &mut Option<Action>) {
        egui::Panel::top("menu").show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                scroll_menu(ui, "File", |ui| {
                    menu_item(ui, act, Action::OpenFile);
                    menu_item(ui, act, Action::OpenFolder);
                    menu_item(ui, act, Action::OpenFolders);
                    menu_item(ui, act, Action::NewFolder);
                    menu_item(ui, act, Action::Copy);
                    menu_item(ui, act, Action::Move);
                    menu_item(ui, act, Action::Rename);
                    menu_item(ui, act, Action::OpenWith);
                    self.open_with_menu(ui);
                    #[cfg(feature = "video")]
                    menu_item(ui, act, Action::Play);
                    menu_item(ui, act, Action::Print);
                    ui.separator();
                    menu_item(ui, act, Action::Delete);
                    menu_item(ui, act, Action::DeletePermanently);
                    ui.separator();
                    self.collections_menu(ui, act);
                    menu_item(ui, act, Action::FindDuplicates);
                    ui.separator();
                    menu_item(ui, act, Action::CloseWindow);
                    menu_item(ui, act, Action::Quit);
                });
                scroll_menu(ui, "Go", |ui| {
                    menu_item(ui, act, Action::First);
                    menu_item(ui, act, Action::Prev);
                    menu_item(ui, act, Action::Next);
                    menu_item(ui, act, Action::Last);
                    ui.separator();
                    menu_item(ui, act, Action::Back);
                    menu_item(ui, act, Action::Forward);
                    menu_item(ui, act, Action::Home);
                    menu_item(ui, act, Action::Up);
                });
                scroll_menu(ui, "Select", |ui| {
                    menu_item(ui, act, Action::SelectAll);
                    menu_item(ui, act, Action::SelectNone);
                    menu_item(ui, act, Action::InvertSelection);
                    ui.separator();
                    menu_item(ui, act, Action::CopyPath);
                    menu_item(ui, act, Action::CopyPathUnquoted);
                    menu_item(ui, act, Action::CopyImage);
                    menu_item(ui, act, Action::CopyFiles);
                });
                scroll_menu(ui, "Edit", |ui| {
                    #[cfg(feature = "marks")]
                    scroll_menu(ui, "Marks and Stars", |ui| {
                        for action in [
                            Action::Mark1,
                            Action::Mark2,
                            Action::Mark3,
                            Action::Mark4,
                            Action::Mark5,
                            Action::Mark6,
                        ] {
                            menu_item(ui, act, action);
                        }
                        ui.separator();
                        for action in [
                            Action::SelectMark1,
                            Action::SelectMark2,
                            Action::SelectMark3,
                            Action::SelectMark4,
                            Action::SelectMark5,
                            Action::SelectMark6,
                        ] {
                            menu_item(ui, act, action);
                        }
                        ui.separator();
                        for action in [
                            Action::Rate0,
                            Action::Rate1,
                            Action::Rate2,
                            Action::Rate3,
                            Action::Rate4,
                            Action::Rate5,
                        ] {
                            menu_item(ui, act, action);
                        }
                    });
                    scroll_menu(ui, "Orientation", |ui| {
                        for action in [
                            Action::RotateCw,
                            Action::RotateCcw,
                            Action::Rotate180,
                            Action::Mirror,
                            Action::Flip,
                        ] {
                            menu_item(ui, act, action);
                        }
                        ui.separator();
                        menu_item(ui, act, Action::SaveOrientation);
                    });
                    menu_item(ui, act, Action::Keywords);
                    ui.separator();
                    self.plugins_menu(ui);
                    ui.separator();
                    menu_item(ui, act, Action::Preferences);
                });
                scroll_menu(ui, "View", |ui| {
                    self.layout_menu(ui, act);
                    scroll_menu(ui, "Theme", |ui| self.theme_menu(ui));
                    scroll_menu(ui, "Colour Management", |ui| self.colour_settings(ui));
                    scroll_menu(ui, "Zoom", |ui| {
                        menu_item(ui, act, Action::ZoomIn);
                        menu_item(ui, act, Action::ZoomOut);
                        menu_item(ui, act, Action::Zoom1To1);
                        menu_item(ui, act, Action::ZoomFit);
                    });
                    menu_item(ui, act, Action::FullScreen);
                    menu_item(ui, act, Action::HideTools);
                    let c = &self.config;
                    menu_toggle(ui, act, c.toolbar, Action::Toolbar);
                    menu_toggle(ui, act, c.path_bar, Action::PathBar);
                    menu_toggle(ui, act, c.folder_tree, Action::FolderTree);
                    menu_toggle(ui, act, c.show_subfolders, Action::Subfolders);
                    menu_toggle(ui, act, c.show_hidden, Action::HiddenFiles);
                    menu_toggle(ui, act, c.thumbnails, Action::Thumbnails);
                    menu_toggle(ui, act, c.icons, Action::Icons);
                    menu_toggle(ui, act, c.exif_panel, Action::ExifPanel);
                    menu_toggle(ui, act, c.sort_manager, Action::SortManager);
                    menu_toggle(ui, act, self.exif_window, Action::ExifWindow);
                    menu_toggle(ui, act, self.pan_window.is_some(), Action::PanView);
                    menu_toggle(ui, act, c.osd, Action::Overlay);
                    menu_item(ui, act, Action::Histogram);
                    menu_toggle(ui, act, c.histogram_log, Action::HistogramLog);
                    menu_item(ui, act, Action::Refresh);
                    ui.separator();
                    let label = if self.slideshow.is_some() {
                        "Stop Slideshow"
                    } else {
                        "Start Slideshow"
                    };
                    menu_button(ui, act, egui::Button::new(label), Action::Slideshow);
                    menu_toggle(ui, act, self.paused, Action::Pause);
                    menu_item(ui, act, Action::Faster);
                    menu_item(ui, act, Action::Slower);
                    menu_toggle(ui, act, self.config.random, Action::RandomOrder);
                });
                scroll_menu(ui, "Window", |ui| {
                    menu_item(ui, act, Action::NewWindow);
                    menu_item(ui, act, Action::CloseWindow);
                });
                scroll_menu(ui, "Help", |ui| {
                    menu_item(ui, act, Action::Help);
                    menu_item(ui, act, Action::Shortcuts);
                    menu_item(ui, act, Action::KeyboardMap);
                    ui.separator();
                    menu_item(ui, act, Action::About);
                });
            });
        });
        if self.config.toolbar {
            self.toolbar(ui, act);
        }
    }

    /// Edit → Preferences: every setting, saved when one changes; Esc closes it.
    fn preferences_window(&mut self, ctx: &egui::Context) {
        let mut open = self.show_settings;
        // Esc closes Preferences (shortcuts are off while it is open), unless a dialog on top
        // of it is the one Esc is meant for.
        if open
            && self.prompt.is_none()
            && self.confirm.is_none()
            && self.capturing.is_none()
            && ctx.input(|i| i.key_pressed(egui::Key::Escape))
        {
            open = false;
        }
        egui::Window::new("Preferences")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .show(ctx, |ui| {
                // Scrolls when the window is smaller than the settings; a solid scroll bar
                // takes its own space instead of covering the text.
                ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    // Plain settings set `changed`; the config is saved once at the end.
                    let mut changed = false;
                    // The look first, where it is found (also under View → Theme).
                    ui.horizontal(|ui| {
                        let mut chosen = false;
                        egui::ComboBox::from_id_salt("theme")
                            .selected_text(self.config.theme.clone())
                            .show_ui(ui, |ui| {
                                for name in theme::names() {
                                    let c = &mut self.config.theme;
                                    chosen |= ui.selectable_value(c, name.clone(), name).clicked();
                                }
                            });
                        ui.label("Theme");
                        // Chosen again too, so a theme file just edited shows.
                        if chosen {
                            changed = true;
                            self.apply_theme(ui.ctx());
                        }
                    });
                    ui.horizontal(|ui| self.theme_buttons(ui));
                    ui.separator();
                    self.memory_settings(ui);
                    self.cpu_settings(ui);
                    ui.separator();
                    ui.label("Slideshow (S, R = random, Ctrl+= faster, Ctrl+- slower):");
                    let delay = egui::Slider::new(&mut self.config.delay, 1..=MAX_DELAY)
                        .text("seconds per image");
                    if ui.add(delay).changed() {
                        changed = true;
                        self.title(ctx);
                    }
                    let mut random = self.config.random;
                    if ui.checkbox(&mut random, "Random order").changed() {
                        self.set_random_order(ctx, random);
                    }
                    changed |= ui
                        .checkbox(
                            &mut self.config.slideshow_subfolders,
                            "Include the images in subfolders",
                        )
                        .changed();
                    changed |= ui
                        .checkbox(
                            &mut self.config.slideshow_repeat,
                            "Repeat: start again after the last image",
                        )
                        .changed();
                    ui.horizontal(|ui| {
                        ui.label("Fade to the next image:");
                        for (k, (_, label, _)) in FADES.iter().enumerate() {
                            changed |= ui
                                .radio_value(&mut self.config.fade, k, *label)
                                .on_hover_text("Not with CPU use Power saving")
                                .changed();
                        }
                    });
                    ui.separator();
                    let mut rotate = self.config.exif_rotate;
                    if ui
                        .checkbox(&mut rotate, "Auto rotate images using EXIF (Alt+X)")
                        .changed()
                    {
                        self.set_exif_rotate(rotate);
                    }
                    changed |= ui
                        .checkbox(
                            &mut self.config.save_orientation,
                            "Save rotation to the file when changing image (JPEG; PNG and WebP are saved again)",
                        )
                        .changed();
                    let quality = egui::Slider::new(&mut self.config.webp_quality, 50..=100)
                        .text("quality when a lossy WebP is saved again");
                    changed |= ui.add(quality).changed();
                    ui.horizontal(|ui| {
                        changed |= ui
                            .color_edit_button_srgb(&mut self.config.background.0)
                            .changed();
                        ui.label("Background behind the image");
                        if self.config.background.0 != [0, 0, 0]
                            && ui.small_button("Black").clicked()
                        {
                            self.config.background.0 = [0, 0, 0];
                            changed = true;
                        }
                    });
                    let c = &mut self.config;
                    changed |= ui
                        .checkbox(&mut c.enlarge, "Enlarge small images to fit the window")
                        .changed();
                    changed |= ui
                        .checkbox(
                            &mut c.keep_zoom,
                            "Keep zoom and position when changing image",
                        )
                        .changed();
                    changed |= ui
                        .checkbox(
                            &mut c.wheel_zoom,
                            "Mouse wheel zooms (off: next/previous image, Ctrl+wheel zooms)",
                        )
                        .changed();
                    ui.separator();
                    ui.label("Search field above the file list:");
                    let fuzzy = self.config.fuzzy_search;
                    for (on, label) in [
                        (true, "Fuzzy, best matches first (like fzf)"),
                        (false, "Regular expression, in list order (like Geeqie's file filter)"),
                    ] {
                        if ui.radio(fuzzy == on, label).clicked() && fuzzy != on {
                            self.config.fuzzy_search = on;
                            changed = true;
                            self.update_search(ctx);
                        }
                    }
                    ui.separator();
                    let mut show = self.config.thumbnails;
                    if ui
                        .checkbox(&mut show, "Show thumbnails in the file list (T)")
                        .changed()
                    {
                        self.set_thumbnails(show);
                    }
                    ui.horizontal(|ui| {
                        ui.label("Thumbnail size:");
                        let mut size = self.config.thumb_size;
                        for (n, name) in [(0, "Small"), (1, "Medium"), (2, "Large")] {
                            if ui.radio_value(&mut size, n, name).changed() {
                                self.set_thumb_size(size);
                            }
                        }
                    });
                    let c = &mut self.config;
                    changed |= ui
                        .checkbox(&mut c.icons, "Show the file list as icons (Ctrl+I)")
                        .changed();
                    changed |= ui
                        .checkbox(&mut c.folder_tree, "Show folders as a tree (Ctrl+T)")
                        .changed();
                    let mut sub = c.show_subfolders;
                    if ui
                        .checkbox(
                            &mut sub,
                            "Include the images in subfolders in the file list",
                        )
                        .changed()
                    {
                        self.set_subfolders(ctx, sub);
                    }
                    let c = &mut self.config;
                    #[cfg(feature = "marks")]
                    {
                        changed |= ui
                            .checkbox(&mut c.show_marks, "Show marks and stars in the file list")
                            .changed();
                        changed |= ui
                            .checkbox(
                                &mut c.xmp_stars,
                                "Also write stars to XMP sidecars (photo.jpg.xmp), for darktable and digiKam",
                            )
                            .changed();
                    }
                    ui.horizontal(|ui| {
                        ui.label("Histogram (K):");
                        for (n, name) in [(0, "Off"), (1, "RGB"), (2, "Brightness")] {
                            changed |= ui.radio_value(&mut c.histogram, n, name).changed();
                        }
                        changed |= ui
                            .checkbox(&mut c.histogram_log, "Logarithmic (J)")
                            .changed();
                    });
                    changed |= ui
                        .checkbox(&mut c.animate, "Play animated GIF and WebP (A)")
                        .changed();
                    ui.separator();
                    self.colour_settings(ui);
                    ui.separator();
                    let c = &mut self.config;
                    changed |= ui
                        .checkbox(
                            &mut c.file_details,
                            "Show file size and date in the file list",
                        )
                        .changed();
                    let save =
                        "Save new thumbnails to ~/.cache/thumbnails (shared with other programs)";
                    if ui.checkbox(&mut c.save_thumbnails, save).changed() {
                        self.save_thumbs.store(c.save_thumbnails, Ordering::Relaxed);
                        changed = true;
                    }
                    // Geeqie's cache maintenance: thumbnails of files that are gone.
                    if ui
                        .button("Remove thumbnails of deleted files")
                        .on_hover_text(
                            "In ~/.cache/thumbnails; kept for disks that are not plugged in",
                        )
                        .clicked()
                    {
                        let (tx, busy, ctx) =
                            (self.notices_tx.clone(), self.busy.clone(), ctx.clone());
                        busy.fetch_add(1, Ordering::Relaxed);
                        std::thread::spawn(move || {
                            let (removed, seen) = thumbs::clean();
                            let _ = tx.send(format!(
                                "Removed {} of {} thumbnails",
                                thousands(removed),
                                thousands(seen)
                            ));
                            busy.fetch_sub(1, Ordering::Relaxed);
                            ctx.request_repaint();
                        });
                    }
                    ui.separator();
                    changed |= ui.checkbox(&mut c.toolbar, "Show the toolbar").changed();
                    changed |= ui
                        .checkbox(&mut c.path_bar, "Show the folder path")
                        .changed();
                    if ui
                        .checkbox(&mut c.search_bar, "Show the file name search")
                        .changed()
                    {
                        changed = true;
                        if !c.search_bar {
                            self.search.clear();
                            self.update_search(ctx);
                        }
                    }
                    ui.horizontal(|ui| {
                        ui.label("Toolbar icons:");
                        for (n, label) in ["Small", "Medium", "Large"].into_iter().enumerate() {
                            changed |= ui
                                .radio_value(&mut self.config.toolbar_icons, n, label)
                                .changed();
                        }
                    });
                    let c = &mut self.config;
                    let exif = "Show the EXIF panel (Ctrl+K; reads a little from each image shown)";
                    changed |= ui.checkbox(&mut c.exif_panel, exif).changed();
                    let osd = "Show image info in full screen (I)";
                    changed |= ui.checkbox(&mut c.osd, osd).changed();
                    ui.separator();
                    changed |= ui
                        .checkbox(&mut c.auto_refresh, "Refresh the folder when files change")
                        .changed();
                    changed |= ui
                        .checkbox(&mut c.remember_dir, "Start in the last used folder")
                        .changed();
                    let window = "Remember the window size and position";
                    changed |= ui.checkbox(&mut c.remember_window, window).changed();
                    let remote = "Listen for glimt --remote commands (after restart)";
                    changed |= ui.checkbox(&mut c.remote_control, remote).changed();
                    let inhibit = "Keep the screen saver off in full screen";
                    changed |= ui.checkbox(&mut c.inhibit_screensaver, inhibit).changed();
                    let order = "Window title \"Glimt - image name\" (off: \"image name - Glimt\")";
                    if ui.checkbox(&mut c.app_name_first, order).changed() {
                        changed = true;
                        self.title(ctx);
                    }
                    ui.separator();
                    egui::CollapsingHeader::new("Keyboard shortcuts").show(ui, |ui| {
                        self.key_settings(ui);
                    });
                    ui.separator();
                    ui.label("Delete behaviour:");
                    let c = &mut self.config;
                    changed |= ui
                        .checkbox(&mut c.confirm_delete, "Confirm permanent file delete")
                        .changed();
                    changed |= ui
                        .checkbox(&mut c.confirm_trash, "Confirm move file to Trash")
                        .changed();
                    changed |= ui
                        .checkbox(&mut c.delete_key, "Enable Delete key")
                        .changed();
                    let mut no_trash = !c.trash;
                    let none = "Use no trash at all (Delete deletes permanently)";
                    if ui.checkbox(&mut no_trash, none).changed() {
                        c.trash = !no_trash;
                        changed = true;
                    }
                    #[cfg(feature = "video")]
                    {
                        ui.separator();
                        ui.label("Video (needs ffmpeg for previews):");
                        if ui
                            .checkbox(&mut c.videos, "Show videos in the file list")
                            .changed()
                        {
                            changed = true;
                            self.list(ctx);
                        }
                        ui.horizontal(|ui| {
                            ui.label("Player (Enter):");
                            let player = egui::TextEdit::singleline(&mut self.config.video_player)
                                .hint_text("default application");
                            changed |= ui.add(player).changed();
                        });
                    }
                    if changed {
                        self.config.save();
                    }
                });
            });
        self.show_settings = open;
    }

    /// Asks for a name: a new folder, a new name for a folder or file, Rename Many's pattern, a new
    /// collection, a theme to save.
    fn prompt_dialog(&mut self, ctx: &egui::Context) {
        if let Some((prompt, mut text)) = self.prompt.take() {
            let title = match prompt {
                Prompt::NewFolder => "New folder name:".to_owned(),
                Prompt::RenameFolder => "New folder name:".to_owned(),
                Prompt::SaveTheme => "Name of the new theme:".to_owned(),
                Prompt::Rename => "New file name:".to_owned(),
                Prompt::NewCollection => format!(
                    "Name of a new collection with {} images:",
                    self.targets().len()
                ),
                Prompt::RenameMany => format!(
                    "New names for {} files (* = old name, # = number, ### = 001):",
                    self.targets().len()
                ),
            };
            // The first new name, so the pattern can be checked before renaming.
            let first = self.targets().first().map(|&i| self.path(i));
            let mut answer = None;
            let modal = egui::Modal::new(egui::Id::new("prompt")).show(ctx, |ui| {
                ui.label(title);
                let edit = ui.text_edit_singleline(&mut text);
                let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                // Focus when the dialog opens (after reading Enter, which ends the focus).
                if !enter && ui.memory(|m| m.focused().is_none()) {
                    edit.request_focus();
                }
                if let (Prompt::RenameMany, Some(first)) = (prompt, &first) {
                    ui.weak(format!("First: {}", pattern_name(text.trim(), first, 1)));
                }
                ui.horizontal(|ui| {
                    if ui.button("OK (Enter)").clicked() || enter {
                        answer = Some(text.clone());
                    }
                    if ui.button("Cancel (Esc)").clicked() {
                        ui.close();
                    }
                });
            });
            if let Some(text) = answer {
                self.answer_prompt(ctx, prompt, text.trim());
            } else if !modal.should_close() {
                self.prompt = Some((prompt, text));
            }
        }
    }

    /// File → Print: the printer or a PDF, the paper, and images or contact sheets.
    fn print_dialog(&mut self, ctx: &egui::Context) {
        if let Some(mut dialog) = self.print.take() {
            if let Ok((names, default)) = dialog.printers_rx.try_recv() {
                // The printer used last, else CUPS's default; a PDF file if neither is there.
                let c = &mut self.config;
                if !names.contains(&c.print_printer) {
                    c.print_printer = default.filter(|d| names.contains(d)).unwrap_or_default();
                }
                dialog.printers = Some(names);
            }
            let mut go = false;
            let modal = egui::Modal::new(egui::Id::new("print")).show(ctx, |ui| {
                let n = dialog.paths.len();
                ui.strong(match n {
                    1 => "Print the image".to_owned(),
                    n => format!("Print {} images", thousands(n)),
                });
                let c = &mut self.config;
                egui::Grid::new("print fields").show(ui, |ui| {
                    ui.label("Printer:");
                    let shown = |p: &str| if p.is_empty() { "PDF file" } else { p }.to_owned();
                    egui::ComboBox::from_id_salt("printer")
                        .selected_text(shown(&c.print_printer))
                        .show_ui(ui, |ui| {
                            for p in dialog.printers.iter().flatten() {
                                ui.selectable_value(&mut c.print_printer, p.clone(), p);
                            }
                            ui.selectable_value(&mut c.print_printer, String::new(), "PDF file");
                        });
                    match &dialog.printers {
                        None => drop(ui.spinner()),
                        Some(p) if p.is_empty() => drop(ui.weak("No printers found")),
                        Some(_) => {}
                    }
                    ui.end_row();
                    ui.label("Paper:");
                    egui::ComboBox::from_id_salt("paper")
                        .selected_text(c.print_paper.clone())
                        .show_ui(ui, |ui| {
                            for (name, _) in print::PAPERS {
                                ui.selectable_value(&mut c.print_paper, name.to_owned(), name);
                            }
                        });
                    ui.end_row();
                    ui.label("Images per page:");
                    egui::ComboBox::from_id_salt("per page")
                        .selected_text(c.print_per_page.to_string())
                        .show_ui(ui, |ui| {
                            for (n, [cols, rows]) in print::PER_PAGE {
                                let label = match n {
                                    1 => "1".to_owned(),
                                    n => format!("{n} ({cols} × {rows})"),
                                };
                                ui.selectable_value(&mut c.print_per_page, n, label);
                            }
                        });
                    ui.end_row();
                    if !c.print_printer.is_empty() {
                        ui.label("Copies:");
                        ui.add(egui::DragValue::new(&mut dialog.copies).range(1..=99));
                        ui.end_row();
                    }
                });
                ui.checkbox(&mut c.print_caption, "File name under each image");
                ui.horizontal(|ui| {
                    let label = if c.print_printer.is_empty() {
                        "Save PDF… (Enter)"
                    } else {
                        "Print (Enter)"
                    };
                    go =
                        ui.button(label).clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.button("Cancel (Esc)").clicked() {
                        ui.close();
                    }
                });
            });
            if go {
                self.config.save();
                let c = &self.config;
                let paper = (print::PAPERS.iter())
                    .find(|p| p.0 == c.print_paper)
                    .unwrap_or(&print::PAPERS[0])
                    .1;
                let job = PrintJob {
                    paths: dialog.paths,
                    paper,
                    per_page: c.print_per_page,
                    caption: c.print_caption,
                    rotate: c.exif_rotate,
                    copies: dialog.copies,
                };
                match c.print_printer.clone() {
                    p if p.is_empty() => self.pick(ctx, false, Pick::SavePdf(job)),
                    p => self.print(ctx, job, PrintTo::Printer(p)),
                }
            } else if !modal.should_close() {
                self.print = Some(dialog);
            }
        }
    }

    /// View → EXIF Window (Ctrl+E): every EXIF field of the shown image.
    fn exif_fields_window(&mut self, ctx: &egui::Context) {
        if self.exif_window && !self.files.is_empty() {
            self.want_exif();
            let mut open = true;
            let title = format!("EXIF: {}", self.name(self.cur));
            egui::Window::new(title)
                .id(egui::Id::new("exif window"))
                .open(&mut open)
                .default_size([560.0, 600.0])
                .vscroll(true)
                .show(ctx, |ui| {
                    if self.exif_all.is_empty() {
                        ui.weak("No EXIF data");
                    }
                    egui::Grid::new("exif all")
                        .striped(true)
                        .num_columns(3)
                        .show(ui, |ui| {
                            for (group, name, value) in &self.exif_all {
                                ui.weak(*group);
                                ui.label(name);
                                ui.label(value);
                                ui.end_row();
                            }
                        });
                });
            self.exif_window = open;
        }
    }

    /// The user name, domain and password for a network folder that asks for them.
    fn login_dialog(&mut self, ctx: &egui::Context) {
        if let Some(mut login) = self.login.take() {
            let mut connect = false;
            let modal = egui::Modal::new(egui::Id::new("login")).show(ctx, |ui| {
                ui.label(format!("Log in to {}", login.uri));
                egui::Grid::new("login fields").show(ui, |ui| {
                    let field = |text| egui::TextEdit::singleline(text).desired_width(220.0);
                    ui.label("User:");
                    ui.add(field(&mut login.user));
                    ui.end_row();
                    ui.label("Domain:");
                    ui.add(field(&mut login.domain));
                    ui.end_row();
                    ui.label("Password:");
                    let edit = ui.add(field(&mut login.password).password(true));
                    // The password field first, as the rest has defaults.
                    if login.password.is_empty() && ui.memory(|m| m.focused().is_none()) {
                        edit.request_focus();
                    }
                    connect = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    ui.end_row();
                });
                ui.horizontal(|ui| {
                    connect |= ui.button("Connect (Enter)").clicked();
                    if ui.button("Cancel (Esc)").clicked() {
                        ui.close();
                    }
                });
            });
            if connect {
                self.mount_as(ctx, login, true);
            } else if !modal.should_close() {
                self.login = Some(login);
            }
        }
    }

    /// Asks before files go to the trash or are deleted for good.
    fn confirm_delete_dialog(&mut self, ctx: &egui::Context) {
        if let Some(trash) = self.confirm {
            let what = match self.targets().as_slice() {
                [i] => format!("\"{}\"", self.name(*i)),
                many => format!("{} files", many.len()),
            };
            let mut delete = false;
            let modal = egui::Modal::new(egui::Id::new("confirm-delete")).show(ctx, |ui| {
                if trash {
                    ui.label(format!("Move {what} to the trash?"));
                } else {
                    ui.label(format!("Permanently delete {what}?"));
                    ui.weak("This cannot be undone.");
                }
                ui.horizontal(|ui| {
                    delete = ui.button("Delete (Enter)").clicked()
                        || ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.button("Cancel (Esc)").clicked() {
                        ui.close();
                    }
                });
            });
            if delete {
                self.confirm = None;
                if self.still_asked() {
                    self.delete_targets_now(ctx, trash);
                }
            } else if modal.should_close() {
                self.confirm = None;
            }
        }
    }

    /// Asks before a folder goes to the trash.
    fn confirm_folder_dialog(&mut self, ctx: &egui::Context) {
        if self.confirm_folder {
            let name = (self.folder_target.as_deref())
                .and_then(Path::file_name)
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let mut trash = false;
            let modal = egui::Modal::new(egui::Id::new("confirm-folder")).show(ctx, |ui| {
                ui.label(format!(
                    "Move the folder \"{name}\" and all in it to the trash?"
                ));
                ui.horizontal(|ui| {
                    trash = ui.button("Move to Trash (Enter)").clicked()
                        || ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.button("Cancel (Esc)").clicked() {
                        ui.close();
                    }
                });
            });
            if trash {
                self.confirm_folder = false;
                self.trash_folder(ctx);
            } else if modal.should_close() {
                self.confirm_folder = false;
                self.folder_target = None;
            }
        }
    }

    /// The status bar as in Geeqie: the counts and sizes on the left, the shown image (or the pane
    /// under the pointer) on the right.
    fn status_bar(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                let n = self.files.len();
                let size = size_text(self.size_rx.is_none().then_some(self.total_size));
                let mut left = match &self.matches {
                    _ if self.listing.is_some() => "Reading folder…".to_owned(),
                    // A search shows the matches: count and size of what the list shows.
                    Some(found) => {
                        let size = size_text(self.match_rx.is_none().then_some(self.match_size));
                        format!(
                            "{} of {} images ({size})",
                            thousands(found.len()),
                            thousands(n)
                        )
                    }
                    None => format!("{} images ({size})", thousands(n)),
                };
                if !self.selected.is_empty() {
                    let m = thousands(self.selected.len());
                    let size = size_text(self.selected_size);
                    left += &format!(", {m} selected ({size})");
                }
                self.sort_menu(ctx, ui);
                self.types_menu(ctx, ui);
                ui.separator();
                ui.label(left);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if n == 0 {
                        return;
                    }
                    // As in Geeqie: "( 4000 x 3000 ) 2,345,678 bytes [3/16]", then the zoom;
                    // with several panes, of the one under the pointer.
                    let (idx, scale) = (self.pane_info.get(self.status_pane).copied())
                        .filter(|p| p.0 < n)
                        .unwrap_or((self.cur, Some(self.last_scale)));
                    let path = self.path(idx);
                    if self.cur_size.as_ref().is_none_or(|c| c.0 != path) {
                        // ponytail: one stat in the UI thread per image change; move it to
                        // a thread if slow network folders make browsing stutter.
                        let bytes = std::fs::metadata(&path).map_or(0, |m| m.len());
                        self.cur_size = Some((path, bytes));
                    }
                    let bytes = self.cur_size.as_ref().map_or(0, |c| c.1);
                    let place = if idx == self.cur {
                        self.place()
                    } else {
                        self.place_of(idx)
                    };
                    let bytes = format!("{} bytes", thousands(bytes as usize));
                    match self.slots.get(&idx) {
                        Some(slot) => {
                            let [w, h] = slot.orig;
                            if let Some(scale) = scale {
                                ui.label(format!("{} %", (scale * 100.0).round()));
                                ui.separator();
                            }
                            let page = if slot.pages > 1 && idx == self.cur {
                                format!(" page {}/{}", self.page + 1, slot.pages)
                            } else {
                                String::new()
                            };
                            ui.label(format!("( {w} x {h} ) {bytes} {place}{page}"));
                        }
                        None => {
                            ui.label(format!("{bytes} {place}"));
                        }
                    }
                    if self.status_grid != [1, 1] {
                        pane_map(ui, self.status_grid, self.status_pane);
                    }
                });
            });
        });
    }

    /// The left side as in Geeqie: the path, the folders at the top, the search and the files
    /// below; then what a click there asked for.
    fn side_panel(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        let (mut open_dir, mut go_to, mut list_act) = (None, None, None);
        // Room for a name next to the size and date columns.
        let min = if self.config.file_details {
            DETAILS_WIDTH + 120.0
        } else {
            80.0
        };
        egui::Panel::left("left")
            .resizable(true)
            .default_size(240.0)
            .min_size(min)
            .show(ui, |ui| {
                if self.config.path_bar {
                    self.path_field(ui);
                }
                egui::Panel::top("folders")
                    .resizable(true)
                    .default_size(200.0)
                    .show(ui, |ui| {
                        if self.config.folder_tree {
                            self.folder_tree(ui, &mut open_dir);
                        } else {
                            self.folder_list(ui, &mut open_dir);
                        }
                    });
                if self.config.search_bar {
                    self.search_field(ui);
                }
                self.file_list(ctx, ui, &mut go_to, &mut list_act);
            });
        self.scroll_to_cur = false;
        if let Some(d) = open_dir {
            self.visit(ctx, d);
        } else if let Some(i) = go_to {
            self.go(ctx, i);
        }
        if let Some(action) = list_act {
            self.run(ctx, action);
        }
    }

    /// The folder list: ⬆ .. for the parent folder, then the subfolders.
    fn folder_list(&mut self, ui: &mut egui::Ui, open_dir: &mut Option<PathBuf>) {
        // Row 0 is ⬆ .. (when there is a parent folder), the rest are subfolders.
        let up = self.dir.parent().map(Path::to_path_buf);
        let rows = up.is_some() as usize + self.subdirs.len();
        let row_h = ui.spacing().interact_size.y;
        egui::ScrollArea::vertical()
            .id_salt("folders")
            .auto_shrink(false)
            .show_rows(ui, row_h, rows, |ui, range| {
                for row in range {
                    let is_up = row == 0 && up.is_some();
                    let (label, target) = match (row, &up) {
                        (0, Some(parent)) => (format!("{}  ..", theme::icon::UP), parent),
                        _ => {
                            let d = &self.subdirs[row - up.is_some() as usize];
                            let name = d.file_name().unwrap_or_default();
                            (
                                format!("{}  {}", theme::icon::FOLDER, name.to_string_lossy()),
                                d,
                            )
                        }
                    };
                    let target = target.clone();
                    let response = ui.selectable_label(false, label);
                    if response.clicked() {
                        *open_dir = Some(target.clone());
                    }
                    if !is_up {
                        self.folder_menu(&response, &target);
                    }
                }
            });
    }

    /// The file list (names, details, thumbnails or icons), drawn only where it is visible.
    fn file_list(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        go_to: &mut Option<usize>,
        list_act: &mut Option<Action>,
    ) {
        // The rows shown: the search matches, or all files.
        let count = self.matches.as_ref().map_or(self.files.len(), Vec::len);
        let row_of = |i: usize, m: &Option<Vec<usize>>| match m {
            Some(m) => m.iter().position(|&x| x == i),
            None => Some(i),
        };
        // Only visible rows are drawn, so the list is as fast with 10 files as with 300,000.
        let icons = self.config.icons;
        let thumbs = self.config.thumbnails || icons;
        let row_h = if icons {
            self.cell().y
        } else if thumbs {
            self.thumb().y + 4.0
        } else {
            ui.spacing().interact_size.y
        };
        let pitch = row_h + ui.spacing().item_spacing.y;
        // As icons, a row holds as many cells as fit the width.
        let gap = ui.spacing().item_spacing.x;
        let cols = if icons {
            (((ui.available_width() + gap) / (self.cell().x + gap)) as usize).max(1)
        } else {
            1
        };
        self.grid_cols = cols;
        let mut area = egui::ScrollArea::vertical()
            .id_salt("files")
            .auto_shrink(false);
        if self.scroll_to_cur
            && let Some(row) = row_of(self.cur, &self.matches).map(|r| r / cols)
        {
            // Scroll only when the current image is out of view, and centre it.
            let (top, view) = (row as f32 * pitch, ui.available_height());
            let off = egui::scroll_area::State::load(ctx, ui.make_persistent_id("files"))
                .map_or(0.0, |s| s.offset.y);
            if top < off || top + pitch > off + view {
                area = area.vertical_scroll_offset((top - view / 2.0).max(0.0));
            }
        }
        area.show_rows(ui, row_h, count.div_ceil(cols), |ui, rows| {
            let rows = rows.start * cols..(rows.end * cols).min(count);
            let shown: Vec<usize> = match &self.matches {
                Some(m) => m[rows.clone()].to_vec(),
                None => rows.clone().collect(),
            };

            if thumbs && let (Some(&first), Some(&last)) = (shown.iter().min(), shown.iter().max())
            {
                self.thumb_view[0].store(first, Ordering::Relaxed);
                self.thumb_view[1].store(last, Ordering::Relaxed);
                self.trim_thumbs(rows);
            }
            let mut line = egui::Rect::NOTHING;
            for (k, i) in shown.into_iter().enumerate() {
                let marked = i == self.cur || self.selected.contains(&i);
                let row = if icons {
                    if k % cols == 0 {
                        let size = egui::vec2(ui.available_width(), self.cell().y);
                        line = ui.allocate_exact_size(size, egui::Sense::hover()).0;
                    }
                    let x = (k % cols) as f32 * (self.cell().x + gap);
                    let cell =
                        egui::Rect::from_min_size(line.min + egui::vec2(x, 0.0), self.cell());
                    self.file_cell(ui, i, cell, marked)
                } else if thumbs || self.config.file_details {
                    self.file_row(ui, i, row_h, marked)
                } else {
                    let name = self.name(i);
                    let marks = self.mark_text(i);
                    if marks.is_empty() {
                        ui.selectable_label(marked, name)
                    } else {
                        ui.selectable_label(marked, format!("{name}   {marks}"))
                    }
                };
                if let Some(i) = self.row_clicked(ctx, &row, i) {
                    *go_to = Some(i);
                }
                row.context_menu(|ui| self.file_menu(ui, list_act));
            }
        });
    }

    /// A click on row `i`: Ctrl and Shift change the selection, a plain click gives the image to
    /// show; a right-click outside the selection selects just that row.
    fn row_clicked(
        &mut self,
        ctx: &egui::Context,
        row: &egui::Response,
        i: usize,
    ) -> Option<usize> {
        let mut go_to = None;
        if row.clicked() {
            let m = row.ctx.input(|i| i.modifiers);
            if m.command {
                // Ctrl+click: toggle selection (the shown image is included the first time).
                if self.selected.is_empty() {
                    self.selected.insert(self.cur);
                }
                if !self.selected.remove(&i) {
                    self.selected.insert(i);
                }
                self.anchor = Some(i);
                self.selection_changed(ctx);
            } else if m.shift {
                // Shift+click: select the rows from where the selection started.
                let anchor = *self.anchor.get_or_insert(self.cur);
                self.selected.clear();
                self.select_range(anchor, i);
                self.selection_changed(ctx);
            } else {
                self.selected.clear();
                self.selected_size = None;
                self.selected_rx = None;
                self.anchor = None;
                go_to = Some(i);
            }
        }
        // Right-click on a file outside the selection selects just it, as in
        // a file manager; the menu acts on the selection.
        if row.secondary_clicked()
            && !self.selected.contains(&i)
            && (i != self.cur || !self.selected.is_empty())
        {
            self.selected.clear();
            self.selected.insert(i);
            self.anchor = Some(i);
            self.selection_changed(ctx);
        }
        go_to
    }

    /// A file's right-click menu; it acts on the selection.
    fn file_menu(&mut self, ui: &mut egui::Ui, list_act: &mut Option<Action>) {
        for action in [
            Action::Copy,
            Action::Move,
            Action::CopyPath,
            Action::CopyImage,
            Action::CopyFiles,
        ] {
            menu_item(ui, list_act, action);
        }
        self.open_with_menu(ui);
        scroll_menu(ui, "Add to Collection", |ui| {
            for (name, file) in collections::list() {
                if ui.button(name).clicked() {
                    self.add_to_collection(&file);
                }
            }
            menu_item(ui, list_act, Action::NewCollection);
        });
        if self.collection.is_some() {
            menu_item(ui, list_act, Action::RemoveFromCollection);
        }
        ui.separator();
        menu_item(ui, list_act, Action::Delete);
        menu_item(ui, list_act, Action::DeletePermanently);
    }

    /// The image area: the shown image in one or more panes, compare, zoom and drag, the overlay,
    /// the film strip in full screen; and what a click there asked for.
    fn image_area(&mut self, ctx: &egui::Context, ui: &mut egui::Ui, fullscreen: bool, bare: bool) {
        let ppp = ctx.pixels_per_point();
        // Actions from the image area, run after drawing it.
        let mut later = None;
        let mut clicked_pane = None;
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill({
                let [r, g, b] = self.config.background.0;
                egui::Color32::from_rgb(r, g, b)
            }))
            .show(ui, |ui| {
                // A layout: the shown image in the first pane, the following ones in the rest.
                let [cols, rows] = layout_grid(&self.config.layout);
                let cell = ui.max_rect().size() / egui::vec2(cols as f32, rows as f32);
                let panes: Vec<egui::Rect> = (0..rows * cols)
                    .map(|k| {
                        let at = egui::vec2((k % cols) as f32, (k / cols) as f32) * cell;
                        let rect = egui::Rect::from_min_size(ui.max_rect().min + at, cell);
                        if cols * rows > 1 {
                            rect.shrink(1.0)
                        } else {
                            rect
                        }
                    })
                    .collect();
                // Comparing: the shown image on the left, the other on the right.
                let full = panes[0];
                let (area, right) = if self.compare && panes.len() == 1 {
                    let (l, r) = full.split_left_right_at_fraction(0.5);
                    (
                        l.shrink2(egui::vec2(1.0, 0.0)),
                        Some(r.shrink2(egui::vec2(1.0, 0.0))),
                    )
                } else {
                    (full, None)
                };
                // Decode to the image area, not the whole screen: a small window on a 4K screen
                // needs a fraction of the memory (3840×2160 is 33 MB per image).
                let view = [(area.width() * ppp) as u32, (area.height() * ppp) as u32];
                self.limits.lock().unwrap()[0] = view;
                // The wheel and dragging work over every pane, and the compared half.
                let several = panes.len() > 1 || right.is_some();
                let input = if several { ui.max_rect() } else { area };
                let response = ui.allocate_rect(input, egui::Sense::click_and_drag());
                let max_fit = if self.config.enlarge {
                    f32::INFINITY
                } else {
                    1.0
                };
                // Size on screen after a quarter turn.
                let quarter = self.orient.0 % 2 == 1;
                let turned = |[w, h]: [usize; 2]| if quarter { [h, w] } else { [w, h] };
                let fit = self.slots.get(&self.cur).map_or(1.0, |s| {
                    let [w, h] = turned(s.orig);
                    (area.width() * ppp / w as f32)
                        .min(area.height() * ppp / h as f32)
                        .min(max_fit)
                });
                if response.hovered() && !self.dialog_open() {
                    // egui turns Ctrl+wheel into zoom_delta, so it zooms in both modes.
                    let (mut factor, scroll, notches) = ctx.input(|i| {
                        let notches: f32 = i
                            .events
                            .iter()
                            .filter_map(|e| match e {
                                egui::Event::MouseWheel {
                                    unit,
                                    delta,
                                    modifiers,
                                    ..
                                } if !modifiers.command => Some(match unit {
                                    egui::MouseWheelUnit::Point => delta.y / 50.0,
                                    _ => delta.y,
                                }),
                                _ => None,
                            })
                            .sum();
                        (i.zoom_delta(), i.smooth_scroll_delta.y, notches)
                    });
                    if self.config.wheel_zoom {
                        factor *= 1.0 + scroll / 400.0;
                    } else {
                        // One notch is one image; touchpad deltas add up to a notch.
                        self.wheel += notches;
                        if self.wheel.abs() >= 1.0 {
                            let up = self.wheel > 0.0;
                            later = Some(if up { Action::Prev } else { Action::Next });
                            self.wheel = 0.0;
                        }
                    }
                    if factor != 1.0 {
                        let old = self.zoom.unwrap_or(fit);
                        let new = (old * factor).clamp(0.02, 32.0);
                        self.zoom = Some(new);
                        // Keep the point under the pointer in place, as in other viewers.
                        if let Some(p) = response.hover_pos() {
                            // From the centre of the pane under the pointer: they share the offset.
                            let under = (right.iter().chain(&panes))
                                .find(|r| r.contains(p))
                                .map_or(area, |r| if *r == full { area } else { *r });
                            let from_centre = p - under.center();
                            self.pan = from_centre - (from_centre - self.pan) * (new / old);
                        }
                    }
                }
                if response.double_clicked() && !self.kiosk {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fullscreen));
                }
                let scale = self.zoom.unwrap_or(fit);
                self.last_scale = scale;
                // Panning only along a side where the image is larger than the view, and only up
                // to its edge, so an image that fits stays put.
                let orig = (self.slots.get(&self.cur).map(|s| s.orig))
                    .or(self.last_drawn.as_ref().map(|d| d.1));
                if let Some([w, h]) = orig.map(turned) {
                    if response.dragged() {
                        self.pan += response.drag_delta();
                    }
                    let size = egui::vec2(w as f32, h as f32) * scale / ppp;
                    let max = ((size - area.size()) / 2.0).max(egui::Vec2::ZERO);
                    self.pan = self.pan.clamp(-max, max);
                    self.max_pan = max;
                }

                // Texture too small: in fit mode the window has grown, so decode again at window
                // size (the previous texture is shown meanwhile); when zoomed, load full resolution.
                let too_small = self.slots.get(&self.cur).is_some_and(|s| {
                    let needed = (s.orig[0] as f32 * scale).min(s.orig[0] as f32);
                    !s.full && (s.tex.size()[0] as f32) + 1.0 < needed
                });
                if too_small && self.zoom.is_none() {
                    self.slots.remove(&self.cur);
                    self.request(self.cur, false);
                } else if too_small {
                    self.request(self.cur, true);
                }
                let now = self.slots.get(&self.cur).map(|s| s.tex.id());
                let fade = now.and_then(|id| self.fading(ctx, 0, id));
                if let Some(slot) = self.slots.get(&self.cur) {
                    let size = egui::vec2(slot.orig[0] as f32, slot.orig[1] as f32) * scale / ppp;
                    let rect = egui::Rect::from_center_size(area.center() + self.pan, size);
                    let (turns, mirrored) = self.orient;
                    let uv = if mirrored {
                        egui::Rect::from_min_max(egui::pos2(1.0, 0.0), egui::pos2(0.0, 1.0))
                    } else {
                        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0))
                    };
                    let angle = turns as f32 * std::f32::consts::FRAC_PI_2;
                    let tex = if self.config.animate {
                        slot.frame(self.opened.elapsed(), ctx)
                    } else {
                        &slot.tex
                    };
                    // Within its pane or half, which zoom makes it overflow.
                    let clip = ui.clip_rect();
                    ui.set_clip_rect(area.intersect(clip));
                    // A slideshow fading: the image before goes as this one comes.
                    let shown = match &fade {
                        Some((t, old, orig)) => {
                            let at = fitted(area, *orig, ppp, self.config.enlarge);
                            let uv1 =
                                egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
                            ui.painter()
                                .image(old.id(), at, uv1, alpha(1.0 - t.powi(4)));
                            *t
                        }
                        None => 1.0,
                    };
                    egui::Image::new(tex)
                        .uv(uv)
                        .rotate(angle, egui::Vec2::splat(0.5))
                        .tint(alpha(shown))
                        .paint_at(ui, rect);
                    ui.set_clip_rect(clip);
                    self.last_drawn = Some((slot.tex.clone(), slot.orig));
                } else if let Some((tex, orig)) = &self.last_drawn
                    && !self.files.is_empty()
                    && !self.errors.contains_key(&self.cur)
                {
                    let size = egui::vec2(orig[0] as f32, orig[1] as f32);
                    let fit = (area.width() * ppp / size.x)
                        .min(area.height() * ppp / size.y)
                        .min(max_fit);
                    let rect = egui::Rect::from_center_size(area.center(), size * fit / ppp);
                    egui::Image::new(tex).paint_at(ui, rect);
                } else {
                    let text = if self.files.is_empty() {
                        if self.listing.is_some() {
                            "Reading folder…"
                        } else {
                            "No images in folder"
                        }
                        .to_owned()
                    } else if let Some(e) = self.errors.get(&self.cur) {
                        format!("Could not open {}:\n{e}", self.path(self.cur).display())
                    } else {
                        String::new()
                    };
                    ui.painter().text(
                        area.center(),
                        egui::Align2::CENTER_CENTER,
                        text,
                        egui::FontId::proportional(16.0),
                        egui::Color32::GRAY,
                    );
                }
                // A video: a play button over the preview frame (click or Enter), readable on
                // any frame, also in the slideshow and full screen.
                #[cfg(feature = "video")]
                if self
                    .files
                    .get(self.cur)
                    .is_some_and(|f| is_video(Path::new(f)))
                {
                    let shade = egui::Color32::from_black_alpha(150);
                    let button =
                        egui::Rect::from_center_size(area.center(), egui::vec2(96.0, 96.0));
                    let play = ui.interact(button, ui.id().with("play"), egui::Sense::click());
                    let p = ui.painter();
                    p.circle_filled(button.center(), 48.0, shade);
                    let c = button.center();
                    let triangle = vec![
                        c + egui::vec2(-14.0, -22.0),
                        c + egui::vec2(24.0, 0.0),
                        c + egui::vec2(-14.0, 22.0),
                    ];
                    p.add(egui::Shape::convex_polygon(
                        triangle,
                        egui::Color32::WHITE,
                        egui::Stroke::NONE,
                    ));
                    let label = p.layout_no_wrap(
                        "Video: Enter or click to play".into(),
                        egui::FontId::proportional(16.0),
                        egui::Color32::WHITE,
                    );
                    let at = egui::Rect::from_center_size(
                        button.center_bottom() + egui::vec2(0.0, 28.0),
                        label.size() + egui::vec2(20.0, 10.0),
                    );
                    p.rect_filled(at, 6.0, shade);
                    p.galley(at.min + egui::vec2(10.0, 5.0), label, egui::Color32::WHITE);
                    if play.clicked() {
                        later = Some(Action::Play);
                    }
                }
                // Several panes: the name in each or in none (after the panes are drawn), not the
                // shown image's details in one.
                if panes.len() == 1 && bare && self.config.osd && !self.files.is_empty() {
                    self.overlay(ui, area);
                }
                if self.config.histogram > 0 {
                    self.paint_histogram(ui, area);
                }
                // What each pane shows, for the status bar: the one under the pointer.
                let mut shown = vec![(self.cur, Some(self.last_scale))];
                if let Some(right) = right {
                    shown.extend(self.paint_other(ui, right, self.zoom));
                }
                for (k, (rect, i)) in panes[1..].iter().zip(self.panes.clone()).enumerate() {
                    shown.push((i, self.paint_pane(ui, *rect, i, self.zoom, Some(k + 1))));
                    // A click makes it the shown image, in the first pane.
                    if ui
                        .interact(*rect, ui.id().with(("pane", k)), egui::Sense::click())
                        .clicked()
                    {
                        clicked_pane = Some(i);
                    }
                }
                let rects: Vec<egui::Rect> = match right {
                    Some(r) => vec![area, r],
                    None => panes.clone(),
                };
                let pointer = ui.ctx().pointer_hover_pos();
                let hovered = pointer.and_then(|p| rects.iter().position(|r| r.contains(p)));
                // The pane the status bar tells about gets a thin outline.
                if let Some(k) = hovered.filter(|_| rects.len() > 1 && !bare)
                    && k < shown.len()
                {
                    let stroke = egui::Stroke::new(1.5, ui.visuals().selection.bg_fill);
                    ui.painter()
                        .rect_stroke(rects[k], 0.0, stroke, egui::StrokeKind::Inside);
                }
                self.status_pane = hovered.filter(|&k| k < shown.len()).unwrap_or(0);
                self.status_grid = if right.is_some() {
                    [2, 1]
                } else {
                    [cols, rows]
                };
                self.pane_info = shown;
                if panes.len() > 1 && self.config.layout_names && !self.files.is_empty() {
                    let shown = std::iter::once(self.cur).chain(self.panes.iter().copied());
                    for (rect, i) in panes.iter().zip(shown) {
                        self.paint_name(ui, *rect, i);
                    }
                }
                // Full screen: a film strip of the images around this one, while the pointer
                // is near the bottom.
                if fullscreen && !self.kiosk && !self.files.is_empty() {
                    clicked_pane = self.filmstrip(ui, ui.max_rect()).or(clicked_pane);
                }
                if let Some(notice) = &self.notice {
                    ui.painter().text(
                        area.center_bottom() - egui::vec2(0.0, 16.0),
                        egui::Align2::CENTER_BOTTOM,
                        notice,
                        egui::FontId::proportional(16.0),
                        egui::Color32::LIGHT_RED,
                    );
                }
            });
        if let Some(action) = later {
            self.run(ctx, action);
        }
        if let Some(i) = clicked_pane {
            self.go(ctx, i);
        }
    }
}

impl eframe::App for App {
    /// The last folder is written once on exit, not on every folder change.
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        remote::stop();
        self.save_meta();
        // ponytail: the path is stored as UTF-8 (lossless for ordinary paths); switch to OsString bytes if needed.
        let mut state = String::new();
        if self.config.remember_dir {
            let last = self.collection.as_ref().unwrap_or(&self.dir);
            state += &format!("last_dir={}\n", last.display());
        }
        if self.config.remember_window {
            state += &self.window.to_text();
        }
        if let Some(p) = state_path().filter(|_| !state.is_empty()) {
            write_atomic(&p, &state);
        }
        self.inhibit(false);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = &ui.ctx().clone();
        self.poll_background(ctx);
        let mut act = self.read_keys(ctx);
        // Full screen and Hide File List as in Geeqie: only the image, no menu or panels.
        let fullscreen = self.track_window(ctx);
        let bare = fullscreen || self.hide_tools || self.kiosk;
        if !bare {
            self.menu_bar(ui, &mut act);
        }
        if let Some(action) = act {
            self.run(ctx, action);
        }
        // The icon grid sets it again in the file list; the next frame's ↑ and ↓ read it.
        self.grid_cols = 1;
        self.preferences_window(ctx);
        self.prompt_dialog(ctx);
        self.print_dialog(ctx);
        self.pan_view(ctx);
        self.exif_fields_window(ctx);
        self.login_dialog(ctx);
        self.confirm_delete_dialog(ctx);
        self.confirm_folder_dialog(ctx);
        self.duplicates_window(ctx);
        help::window(ctx, &mut self.help);
        if !bare {
            self.status_bar(ctx, ui);
        }
        if bare {
            // A scroll to the shown image asked for while the list is hidden is dropped.
            self.scroll_to_cur = false;
        } else {
            self.side_panel(ctx, ui);
        }
        if !bare && self.config.sort_manager {
            self.sort_manager(ui);
        }
        if !bare && self.config.exif_panel && !self.files.is_empty() {
            self.exif_panel(ui);
        } else {
            // Hidden while a field had the focus: it never lost it.
            self.save_meta();
        }
        self.image_area(ctx, ui, fullscreen, bare);
    }
}

/// The names whose thumbnails stay: rows `rows` of the list as shown (`matches`, or all of
/// `files`) and a third of `max` rows on each side. Rows, not file indices: search matches are
/// spread over the whole folder, so a range of indices would keep almost every file.
fn thumbs_to_keep<'a>(
    files: &'a [OsString],
    matches: Option<&[usize]>,
    rows: std::ops::Range<usize>,
    max: usize,
) -> HashSet<&'a OsString> {
    let count = matches.map_or(files.len(), <[usize]>::len);
    let around = rows.start.saturating_sub(max / 3)..(rows.end + max / 3).min(count);
    around
        .map(|row| &files[matches.map_or(row, |m| m[row])])
        .collect()
}

/// The program New Window starts: from an AppImage the AppImage, as the binary is in its mount,
/// which goes when this window closes; else the binary. Every program started from an AppImage
/// inherits its APPIMAGE, so it counts only when the binary is inside APPDIR.
fn own_program(exe: PathBuf, appdir: Option<OsString>, appimage: Option<OsString>) -> PathBuf {
    match (appdir, appimage) {
        (Some(dir), Some(image)) if exe.starts_with(&dir) => PathBuf::from(image),
        _ => exe,
    }
}

fn main() -> eframe::Result {
    // One malloc arena for all threads. With one per thread, freed image buffers stayed in
    // each decode thread's arena: 91 instead of 29 MB of own memory in a folder of
    // screenshots, while decoding is at most a few percent slower.
    if Config::load().low_memory {
        // SAFETY: called before any other thread exists.
        unsafe { mallopt(M_ARENA_MAX, 1) };
        ONE_ARENA.store(true, Ordering::Relaxed);
    }
    let (start, args_os) = match Start::parse(std::env::args_os().skip(1).collect()) {
        Ok(parsed) => parsed,
        Err(e) => {
            eprintln!("glimt: {e}\nTry glimt --help");
            std::process::exit(2);
        }
    };
    let args: Vec<String> = (args_os.iter())
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    if args.first().is_some_and(|a| a == "--version" || a == "-V") {
        println!("glimt {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.first().is_some_and(|a| a == "--help" || a == "-h") {
        println!(
            "Usage: glimt [OPTIONS] [FILE_OR_FOLDER …]   several folders show as one list\n       glimt --remote COMMAND      to the Glimt used last\n       glimt --remote-all COMMAND  to every running Glimt\n\nOptions:"
        );
        for (option, does) in START_OPTIONS {
            println!("  {option:<24} {does}");
        }
        println!("\nCommands:");
        for (command, does) in remote::COMMANDS {
            println!("  {command:<12} {does}");
        }
        return Ok(());
    }
    let mut arg = args_os.first().map(PathBuf::from);
    let mut start = start;
    // glimt DIR DIR …: the images of every folder in one list.
    if !args.first().is_some_and(|a| a.starts_with("--remote")) {
        start.more = args_os.iter().skip(1).map(PathBuf::from).collect();
    }
    let to_all = args.first().is_some_and(|a| a == "--remote-all");
    if args.first().is_some_and(|a| a == "--remote") || to_all {
        let mut command = args[1..].join(" ");
        // The running Glimt has another working folder: send the full path.
        if let Some(path) = command.strip_prefix("open ") {
            let path = std::path::absolute(path).unwrap_or_else(|_| path.into());
            command = format!("open {}", path.display());
        }
        let sent = match (&start.name, to_all) {
            (_, true) => remote::send_all(&command) > 0,
            (Some(name), false) => remote::send_to(name, &command),
            (None, false) => remote::send(&command),
        };
        if sent {
            return Ok(());
        }
        match command.strip_prefix("open ") {
            Some(path) => arg = Some(path.into()),
            None => {
                match &start.name {
                    Some(name) => eprintln!("No Glimt named {name} is running"),
                    None => eprintln!("Glimt is not running"),
                }
                std::process::exit(1);
            }
        }
    }
    // The app ID matches the desktop file, so the desktop shows its icon for the window:
    // glimt.desktop, or in a Flatpak io.github.thornbux.Glimt.desktop (FLATPAK_ID).
    let app_id = std::env::var("FLATPAK_ID").unwrap_or_else(|_| "glimt".into());
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Glimt")
        .with_app_id(app_id)
        .with_inner_size([1280.0, 800.0]);
    if let Ok(icon) = image::load_from_memory(include_bytes!("../assets/glimt-256.png")) {
        let icon = icon.into_rgba8();
        viewport = viewport.with_icon(egui::IconData {
            width: icon.width(),
            height: icon.height(),
            rgba: icon.into_raw(),
        });
    }
    if Config::load().remember_window
        && let Some(state) = state_path().and_then(|p| std::fs::read_to_string(p).ok())
    {
        let win = WindowState::parse(&state);
        if let Some(size) = win.size {
            viewport = viewport.with_inner_size(size);
        }
        if let Some(pos) = win.pos {
            viewport = viewport.with_position(pos);
        }
        viewport = viewport.with_maximized(win.maximized);
    }
    if start.fullscreen || start.kiosk {
        viewport = viewport.with_fullscreen(true);
    }
    if let Some(name) = start.name.clone() {
        remote::set_name(name);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "glimt",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, arg, start)))),
    )
}

#[cfg(test)]
mod tests {
    /// A JPEG far larger than the window is decoded scaled, turned upright by its EXIF, with
    /// its full upright size; one that needs more than half its pixels is left to image.
    #[test]
    fn decodes_jpeg_scaled_and_upright() {
        let dir = std::env::temp_dir().join(format!("glimt-scaled-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("wide.jpg");
        // Red on the left half, blue on the right.
        let img = image::RgbImage::from_fn(400, 200, |x, _| match x < 200 {
            true => image::Rgb([255, 0, 0]),
            false => image::Rgb([0, 0, 255]),
        });
        img.save(&file).unwrap();
        // A quarter turn clockwise (EXIF orientation 6): the left half ends up on top.
        super::exif::write_orientation(&file, 1, false).unwrap();
        let (img, _, orig) = super::decode::scaled_jpeg(&file, [60, 100], true).unwrap();
        assert_eq!(orig, [200, 400]);
        assert_eq!((img.width(), img.height()), (50, 100));
        let rgb = img.to_rgb8();
        let (top, bottom) = (rgb.get_pixel(25, 10), rgb.get_pixel(25, 90));
        assert!(top[0] > 200 && top[2] < 60, "{top:?}");
        assert!(bottom[2] > 200 && bottom[0] < 60, "{bottom:?}");
        // Not turned: the stored size; and no gain at more than half: None.
        let (_, _, orig) = super::decode::scaled_jpeg(&file, [100, 60], false).unwrap();
        assert_eq!(orig, [400, 200]);
        assert!(super::decode::scaled_jpeg(&file, [300, 300], true).is_none());
        assert!(super::decode::scaled_jpeg(&dir.join("none.jpg"), [10, 10], true).is_none());
        // A RAW file's preview, scaled the same way.
        let mut raw = b"FUJIFILMCCD-RAW ".to_vec();
        raw.extend(std::fs::read(&file).unwrap());
        std::fs::write(dir.join("a.raf"), raw).unwrap();
        let (img, icc, orig) =
            super::decode::scaled_jpeg(&dir.join("a.raf"), [60, 100], true).unwrap();
        assert_eq!((orig, img.width(), img.height()), ([200, 400], 50, 100));
        assert!(icc.is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// CPU use: thumbnails and background threads for each level.
    #[test]
    fn cpu_levels() {
        use super::{background_threads, cores, set_cpu_use, thumb_threads};
        set_cpu_use(0);
        assert_eq!(
            (thumb_threads(4), background_threads()),
            (1, cores().min(2))
        );
        set_cpu_use(2);
        assert_eq!((thumb_threads(4), background_threads()), (cores(), cores()));
        set_cpu_use(1);
        assert_eq!((thumb_threads(4), thumb_threads(1)), (2.min(4), 1));
        assert_eq!(background_threads(), cores());
    }

    /// Start options in front of the file or folder; the rest is left as it was.
    #[test]
    fn parses_start_options() {
        use super::Start;
        let args = |a: &[&str]| {
            a.iter()
                .map(Into::into)
                .collect::<Vec<std::ffi::OsString>>()
        };
        let (s, rest) = Start::parse(args(&[
            "--fullscreen",
            "--layout",
            "2x2",
            "--slideshow",
            "d",
        ]))
        .unwrap();
        assert!(s.fullscreen && s.slideshow && !s.random && s.layout == Some("2x2"));
        assert_eq!(rest, args(&["d"]));
        let (s, rest) = Start::parse(args(&["--random", "--remote", "next"])).unwrap();
        assert!(s.random && s.layout.is_none());
        assert_eq!(rest, args(&["--remote", "next"]));
        assert!(Start::parse(args(&["--layout", "3x3"])).is_err());
        assert!(Start::parse(args(&["--layout"])).is_err());
        assert!(Start::parse(args(&["--bogus"])).is_err());
        let (s, rest) = Start::parse(args(&["--name", "left", "--remote", "next"])).unwrap();
        assert_eq!(
            (s.name.as_deref(), rest),
            (Some("left"), args(&["--remote", "next"]))
        );
        assert!(Start::parse(args(&["--name", "123"])).is_err());
        assert!(Start::parse(args(&["--name", "a/b"])).is_err());
        assert!(Start::parse(args(&["--name"])).is_err());
        assert!(Start::parse(args(&["--kiosk", "d"])).unwrap().0.kiosk);
        assert_eq!(
            Start::parse(args(&["--fade", "long"])).unwrap().0.fade,
            Some(2)
        );
        assert!(Start::parse(args(&["--fade", "slow"])).is_err());
        let (_, rest) = Start::parse(args(&["--version"])).unwrap();
        assert_eq!(rest, args(&["--version"]));
    }

    /// The Types menu: images, RAW and videos each on their own; an opened file's type.
    #[test]
    fn lists_by_type() {
        use super::{Types, is_listed};
        use std::path::Path;
        let raw = Types {
            raw: true,
            ..Types::NONE
        };
        let images = Types {
            images: true,
            ..Types::NONE
        };
        assert!(is_listed(Path::new("a.CR2"), raw) && !is_listed(Path::new("a.cr2"), images));
        assert!(is_listed(Path::new("a.jpg"), images) && !is_listed(Path::new("a.jpg"), raw));
        assert!(!is_listed(
            Path::new("a.txt"),
            Types {
                videos: true,
                images: true,
                raw: true,
                hidden: false
            }
        ));
        let t = Types::of(Path::new("x/a.nef"));
        assert!(t.raw && !t.images && !t.videos && !t.hidden);
        assert!(Types::of(Path::new("x/.a.nef")).hidden);
        assert!(!Types::of(Path::new("a.txt")).images);
    }

    #[test]
    fn lists_folder_and_decodes_to_screen_size() {
        let dir = std::env::temp_dir().join(format!("glimt-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbImage::new(400, 200)
            .save(dir.join("a.png"))
            .unwrap();
        std::fs::write(dir.join("notes.txt"), "").unwrap();
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::create_dir_all(dir.join(".hidden")).unwrap();
        std::fs::write(dir.join(".b.png"), "").unwrap();
        let mut all = super::files::Types {
            images: true,
            raw: true,
            videos: true,
            hidden: false,
        };
        let (dirs, images) = super::files::list_folder(&dir, all);
        assert_eq!(images, vec![std::ffi::OsString::from("a.png")]);
        assert_eq!(dirs, vec![dir.join("sub")]);
        // Hidden files and folders, when shown.
        all.hidden = true;
        let (dirs, images) = super::files::list_folder(&dir, all);
        assert_eq!(images, [".b.png", "a.png"].map(std::ffi::OsString::from));
        assert_eq!(dirs, vec![dir.join(".hidden"), dir.join("sub")]);
        std::fs::remove_file(dir.join(".b.png")).unwrap();
        let (frames, orig, _) =
            super::decode::decode(&dir.join("a.png"), [100, 100], true, 0).unwrap();
        assert_eq!(
            (frames.len(), frames[0].0.size, orig),
            (1, [100, 50], [400, 200])
        );
        // An animated GIF gives every frame with its delay (5 ms counts as 100 ms).
        let gif = std::fs::File::create(dir.join("b.gif")).unwrap();
        let frame = |ms| {
            let delay = image::Delay::from_numer_denom_ms(ms, 1);
            image::Frame::from_parts(image::RgbaImage::new(40, 20), 0, 0, delay)
        };
        image::codecs::gif::GifEncoder::new(gif)
            .encode_frames([frame(50), frame(5)])
            .unwrap();
        let (frames, orig, _) =
            super::decode::decode(&dir.join("b.gif"), [100, 100], true, 0).unwrap();
        let delays: Vec<u32> = frames.iter().map(|f| f.1).collect();
        assert_eq!((delays, orig), (vec![50, 100], [40, 20]));
        // A GIF whose canvas claims 65535×65535 is refused before a frame is made: each frame
        // would be a canvas of 17 GB.
        let huge = dir.join("huge.gif");
        let mut gif = b"GIF89a\xff\xff\xff\xff\0\0\0".to_vec();
        gif.extend(b",\0\0\0\0\x01\0\x01\0\0\x02\x02\x44\x01\0;");
        std::fs::write(&huge, gif).unwrap();
        assert!(super::decode::animation(&huge, [100, 100]).is_none());
        assert!(super::decode::decode(&huge, [100, 100], true, 0).is_err());
        std::fs::remove_file(huge).unwrap();
        // The tree: a folder reached again through a link is read once.
        std::fs::copy(dir.join("a.png"), dir.join("sub/c.png")).unwrap();
        std::os::unix::fs::symlink(&dir, dir.join("sub/back")).unwrap();
        assert_eq!(
            super::files::list_tree(
                &dir,
                super::files::Types {
                    images: true,
                    raw: true,
                    videos: true,
                    hidden: false
                }
            ),
            ["a.png", "b.gif", "sub/c.png"]
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Through a Display P3 screen profile, sRGB red is less red in the screen's numbers. A file
    /// that is no profile leaves the one there; an sRGB screen profile counts as none.
    #[test]
    fn shows_through_a_screen_profile() {
        let _held = super::colour::COLOUR_TEST
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("glimt-screen-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let write = |name: &str, bytes: Vec<u8>| {
            let file = dir.join(name);
            std::fs::write(&file, bytes).unwrap();
            file.to_string_lossy().into_owned()
        };
        let p3 = write(
            "p3.icc",
            moxcms::ColorProfile::new_display_p3().encode().unwrap(),
        );
        let srgb = write(
            "srgb.icc",
            moxcms::ColorProfile::new_srgb().encode().unwrap(),
        );
        let junk = write("junk.icc", b"junk".to_vec());
        let red = || {
            image::DynamicImage::from(image::RgbaImage::from_pixel(
                2,
                2,
                image::Rgba([255, 0, 0, 255]),
            ))
        };
        let shown = || {
            *super::colour::to_display(red(), None)
                .into_rgba8()
                .get_pixel(0, 0)
        };
        super::colour::set_screen(&p3).unwrap();
        let on_p3 = shown();
        assert!(on_p3[0] < 250 && on_p3[1] > 20, "{on_p3:?}");
        assert!(super::colour::set_screen(&junk).is_err());
        assert_eq!(shown(), on_p3);
        super::colour::set_screen(&srgb).unwrap();
        assert!(super::colour::COLOUR.read().unwrap().screen.is_none());
        assert_eq!(super::colour::to_display(red(), None), red());
        // Images without a profile taken as Adobe RGB: in sRGB the same numbers mean another colour.
        super::colour::set_input("adobe").unwrap();
        let grey = || {
            image::DynamicImage::from(image::RgbaImage::from_pixel(
                1,
                1,
                image::Rgba([60, 160, 60, 255]),
            ))
        };
        assert_ne!(super::colour::to_srgb(grey(), None), grey());
        // A thumbnail is sRGB already: not taken as Adobe RGB a second time.
        assert_eq!(super::colour::srgb_to_display(grey()), grey());
        // The image's own profile left out: the input profile for all, here sRGB.
        super::colour::set_input("srgb").unwrap();
        super::colour::set_colour(|c| c.own = false);
        let adobe = moxcms::ColorProfile::new_adobe_rgb().encode().unwrap();
        assert_eq!(super::colour::to_srgb(grey(), Some(&adobe)), grey());
        super::colour::set_colour(|c| c.own = true);
        assert_ne!(super::colour::to_srgb(grey(), Some(&adobe)), grey());
        assert!(super::colour::set_input(&junk).is_err());
        // A profile that cannot be read: left as it is.
        assert_eq!(super::colour::to_srgb(grey(), Some(b"junk")), grey());
        super::colour::set_screen("").unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A profile with a table for perceptual and another for colorimetric: the intent picks one,
    /// and saturation, which has none, takes the perceptual one.
    #[test]
    fn rendering_intent_picks_its_table() {
        use moxcms::{LutDataType, LutStore, LutType, LutWarehouse, Matrix3d};
        let _held = super::colour::COLOUR_TEST
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // RGB to XYZ through a 2×2×2 grid, red and blue swapped in the colorimetric one.
        let table = |swap: bool| {
            let mut clut = Vec::new();
            for r in [0, 1] {
                for g in [0, 1] {
                    for b in [0, 1] {
                        let (x, z) = if swap { (b, r) } else { (r, b) };
                        clut.extend([x * 30000, g * 30000, z * 30000]);
                    }
                }
            }
            LutWarehouse::Lut(LutDataType {
                num_input_channels: 3,
                num_output_channels: 3,
                num_clut_grid_points: 2,
                matrix: Matrix3d::IDENTITY,
                num_input_table_entries: 2,
                num_output_table_entries: 2,
                input_table: LutStore::Store16([0, 65535].repeat(3)),
                clut_table: LutStore::Store16(clut),
                output_table: LutStore::Store16([0, 65535].repeat(3)),
                lut_type: LutType::Lut16,
            })
        };
        let mut profile = moxcms::ColorProfile::new_srgb();
        profile.lut_a_to_b_perceptual = Some(table(false));
        profile.lut_a_to_b_colorimetric = Some(table(true));
        super::colour::set_colour(|c| c.input = Some(profile));
        let red = || {
            image::DynamicImage::from(image::RgbaImage::from_pixel(
                1,
                1,
                image::Rgba([200, 40, 40, 255]),
            ))
        };
        let shown = |intent| {
            super::colour::set_intent(intent);
            *super::colour::to_srgb(red(), None)
                .into_rgba8()
                .get_pixel(0, 0)
        };
        let perceptual = shown("perceptual");
        assert_ne!(perceptual, red().into_rgba8()[(0, 0)]);
        assert_ne!(shown("relative"), perceptual);
        assert_eq!(shown("saturation"), perceptual);
        super::colour::set_input("srgb").unwrap();
        super::colour::set_intent("perceptual");
    }

    /// Images for the screen wait for colord at start until the deadline, and not at all after.
    #[test]
    fn waits_for_the_screen_profile_until_the_deadline() {
        let start = std::time::Instant::now();
        *super::colour::SCREEN_WAIT.0.lock().unwrap() =
            Some(start + std::time::Duration::from_millis(40));
        super::colour::wait_for_screen();
        let waited = start.elapsed().as_millis();
        assert!((35..1000).contains(&waited), "{waited} ms");
        let start = std::time::Instant::now();
        super::colour::wait_for_screen();
        assert!(start.elapsed().as_millis() < 5);
    }

    /// What gdbus prints for colord: the first object or file, and nothing for an empty list.
    #[test]
    fn reads_colord() {
        let devices = "([objectpath '/org/freedesktop/ColorManager/devices/a', '/b'],)\n";
        let first = super::colour::first_quoted(devices);
        assert_eq!(first, Some("/org/freedesktop/ColorManager/devices/a"));
        assert_eq!(
            super::colour::first_quoted("(<'/x/edid.icc'>,)"),
            Some("/x/edid.icc")
        );
        assert_eq!(super::colour::first_quoted("(@ao [],)\n"), None);
        assert_eq!(super::colour::first_quoted("(<''>,)"), None);
    }

    /// Thumbnails are kept by rows of the list as shown: with search matches spread over a large
    /// folder, a few hundred names, not the whole folder.
    #[test]
    fn keeps_thumbnails_near_the_rows() {
        use std::ffi::OsString;
        let files: Vec<OsString> = (0..100_000)
            .map(|i| OsString::from(i.to_string()))
            .collect();
        let matches = [12, 99_000, 50_000, 7, 80_000];
        let keep = super::thumbs_to_keep(&files, Some(&matches), 0..2, 6);
        let mut names: Vec<_> = keep.into_iter().map(|n| n.to_str().unwrap()).collect();
        names.sort();
        assert_eq!(names, ["12", "50000", "7", "99000"]);
        assert_eq!(
            super::thumbs_to_keep(&files, None, 500..510, 600).len(),
            410
        );
    }

    /// New Window starts the AppImage only from inside it, not when Glimt was merely started
    /// from another AppImage (a terminal), whose APPIMAGE it inherits.
    #[test]
    fn new_window_program() {
        use std::path::PathBuf;
        let os = |s: &str| Some(std::ffi::OsString::from(s));
        let inside = PathBuf::from("/tmp/.mount_ab12/usr/bin/glimt");
        let got = super::own_program(inside, os("/tmp/.mount_ab12"), os("/home/u/glimt.AppImage"));
        assert_eq!(got, PathBuf::from("/home/u/glimt.AppImage"));
        let installed = PathBuf::from("/usr/bin/glimt");
        let got = super::own_program(
            installed.clone(),
            os("/tmp/.mount_cd34"),
            os("/home/u/Term.AppImage"),
        );
        assert_eq!(got, installed);
        assert_eq!(super::own_program(installed.clone(), None, None), installed);
    }

    /// image writes lossless WebP (VP8L); anything else is not taken for one.
    #[test]
    fn tells_lossless_webp() {
        let file = std::env::temp_dir().join(format!("glimt-{}.webp", std::process::id()));
        image::RgbImage::new(4, 4).save(&file).unwrap();
        assert_eq!(super::save::webp_lossless(&file), Some(true));
        std::fs::write(&file, b"RIFF\x04\0\0\0WEBP").unwrap();
        assert_eq!(super::save::webp_lossless(&file), None);
        std::fs::remove_file(file).unwrap();
    }

    /// Own keys survive the config file, and replace the defaults of their action.
    #[test]
    fn own_keys() {
        use super::actions::{Action, Keys};
        let keys: Keys = "Next=Ctrl+Shift+N;ZoomIn=Plus;Bogus=X;Prev=Hyper+P"
            .parse()
            .unwrap();
        assert_eq!(keys.to_string(), "Next=Ctrl+Shift+N;ZoomIn=Plus");
        assert_eq!(keys.to_string().parse::<Keys>().unwrap(), keys);
        *super::actions::OWN_KEYS.write().unwrap() = keys.0.clone();
        let all = super::actions::shortcuts();
        let next: Vec<_> = all.iter().filter(|s| s.0 == Action::Next).collect();
        assert_eq!(next.len(), 1);
        // More modifiers first, so Shift+R (rotate) is matched before R.
        let pos = |a| all.iter().position(|s| s.0 == a).unwrap();
        assert!(pos(Action::Rotate180) < pos(Action::RandomOrder));
        super::actions::OWN_KEYS.write().unwrap().clear();
    }

    /// A PNG turned upright by its EXIF orientation and then a quarter turn keeps its EXIF,
    /// with an upright orientation.
    #[test]
    fn saves_rotated_png_with_exif() {
        use image::{ImageDecoder, ImageEncoder};
        let dir = std::env::temp_dir().join(format!("glimt-rotated-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.png");
        // IFD0 with Orientation 6 (turn 90° clockwise) and Make "Canon".
        let exif = b"MM\0*\0\0\0\x08\0\x02\x01\x0f\0\x02\0\0\0\x06\0\0\0\x26\
            \x01\x12\0\x03\0\0\0\x01\0\x06\0\0\0\0\0\0Canon\0";
        let mut enc = image::codecs::png::PngEncoder::new(std::fs::File::create(&file).unwrap());
        enc.set_exif_metadata(exif.to_vec()).unwrap();
        image::RgbImage::new(4, 2).write_with_encoder(enc).unwrap();
        super::save::save_rotated(&file, true, 1, false, 90).unwrap();
        let mut dec = image::ImageReader::open(&file)
            .unwrap()
            .into_decoder()
            .unwrap();
        assert_eq!(dec.dimensions(), (4, 2));
        assert_eq!(
            dec.orientation().unwrap(),
            image::metadata::Orientation::NoTransforms
        );
        let rows = super::exif::read(&file);
        assert!(rows.contains(&("Camera", "Canon".to_string())), "{rows:?}");
        // An animated PNG (an acTL chunk after IHDR) is refused and left as it was.
        let apng = dir.join("b.png");
        image::RgbImage::new(4, 2).save(&apng).unwrap();
        let mut bytes = std::fs::read(&apng).unwrap();
        let crc = |data: &[u8]| {
            let mut c = !0u32;
            for &b in data {
                c ^= u32::from(b);
                for _ in 0..8 {
                    c = if c & 1 == 1 {
                        0xedb8_8320 ^ (c >> 1)
                    } else {
                        c >> 1
                    };
                }
            }
            !c
        };
        let actl = [b"acTL".as_slice(), &[0, 0, 0, 1, 0, 0, 0, 0]].concat();
        let mut chunk = 8u32.to_be_bytes().to_vec();
        chunk.extend(&actl);
        chunk.extend(crc(&actl).to_be_bytes());
        bytes.splice(33..33, chunk);
        std::fs::write(&apng, &bytes).unwrap();
        assert!(super::save::save_rotated(&apng, false, 1, false, 90).is_err());
        assert_eq!(std::fs::read(&apng).unwrap(), bytes);
        // A JPEG with a .png name is turned, and saved as a PNG.
        let misnamed = dir.join("c.png");
        image::RgbImage::new(4, 2)
            .save_with_format(&misnamed, image::ImageFormat::Jpeg)
            .unwrap();
        super::save::save_rotated(&misnamed, false, 1, false, 90).unwrap();
        assert_eq!(image::image_dimensions(&misnamed).unwrap(), (2, 4));
        // Through a symlink: the file it points to is turned, and the link stays a link.
        let link = dir.join("link.png");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        super::save::save_rotated(&link, false, 1, false, 90).unwrap();
        assert!(link.is_symlink());
        assert_eq!(image::image_dimensions(&file).unwrap(), (2, 4));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Saved as the view showed it, also with a mirroring orientation: turned by it with Auto
    /// Rotate on, as it is without, and in both cases with an upright orientation after.
    #[test]
    fn saves_rotated_as_seen() {
        use image::{ImageDecoder, ImageEncoder};
        let dir = std::env::temp_dir().join(format!("glimt-seen-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.png");
        let (red, blue) = (image::Rgb([255, 0, 0]), image::Rgb([0, 0, 255]));
        // Orientation 2: mirrored left to right.
        let exif = b"MM\0*\0\0\0\x08\0\x01\x01\x12\0\x03\0\0\0\x01\0\x02\0\0\0\0\0\0";
        for (auto_rotate, top) in [(false, red), (true, blue)] {
            let mut enc =
                image::codecs::png::PngEncoder::new(std::fs::File::create(&file).unwrap());
            enc.set_exif_metadata(exif.to_vec()).unwrap();
            image::RgbImage::from_fn(2, 1, |x, _| if x == 0 { red } else { blue })
                .write_with_encoder(enc)
                .unwrap();
            // A quarter turn clockwise: the left pixel of what the view shows goes on top.
            super::save::save_rotated(&file, auto_rotate, 1, false, 90).unwrap();
            let mut dec = image::ImageReader::open(&file)
                .unwrap()
                .into_decoder()
                .unwrap();
            assert_eq!(
                dec.orientation().unwrap(),
                image::metadata::Orientation::NoTransforms
            );
            let img = image::DynamicImage::from_decoder(dec).unwrap().into_rgb8();
            assert_eq!((img.dimensions(), *img.get_pixel(0, 0)), ((1, 2), top));
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A simple WebP gets the extended header and an EXIF chunk that the decoder reads back.
    #[test]
    fn adds_exif_to_webp() {
        use image::{ImageDecoder, ImageEncoder};
        let mut webp = Vec::new();
        image::codecs::webp::WebPEncoder::new_lossless(&mut webp)
            .write_image(&[0; 5 * 3 * 3], 5, 3, image::ExtendedColorType::Rgb8)
            .unwrap();
        let exif = b"II*\0\x08\0\0\0\0\0\0\0\0";
        let with = super::save::webp_with_exif(&webp, exif, 5, 3).unwrap();
        let mut dec = image::codecs::webp::WebPDecoder::new(std::io::Cursor::new(with)).unwrap();
        assert_eq!(dec.dimensions(), (5, 3));
        assert_eq!(dec.exif_metadata().unwrap().as_deref(), Some(&exif[..]));
    }

    /// "a.png" and "a.webp" saved turned at the same time: each keeps its own pixels and format.
    #[test]
    fn saves_same_names_at_once() {
        let dir = std::env::temp_dir().join(format!("glimt-same-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (png, webp) = (dir.join("a.png"), dir.join("a.webp"));
        assert_ne!(super::save::tmp_path(&png), super::save::tmp_path(&webp));
        image::RgbImage::from_pixel(1200, 800, image::Rgb([255, 0, 0]))
            .save(&png)
            .unwrap();
        image::RgbImage::from_pixel(1200, 800, image::Rgb([0, 0, 255]))
            .save(&webp)
            .unwrap();
        let start = std::sync::Barrier::new(2);
        std::thread::scope(|s| {
            for path in [&png, &webp] {
                let start = &start;
                s.spawn(move || {
                    start.wait();
                    super::save::save_rotated(path, false, 1, false, 90).unwrap();
                });
            }
        });
        let pixel = |p: &std::path::Path| {
            let img = image::open(p).unwrap().into_rgb8();
            (img.dimensions(), *img.get_pixel(0, 0))
        };
        assert_eq!(pixel(&png), ((800, 1200), image::Rgb([255, 0, 0])));
        assert_eq!(pixel(&webp), ((800, 1200), image::Rgb([0, 0, 255])));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The sort manager's folders come back as they were, also a name that is not UTF-8.
    #[test]
    fn reads_sort_folders_as_bytes() {
        use std::os::unix::ffi::OsStrExt;
        let latin = std::ffi::OsStr::from_bytes(b"/caf\xe9");
        assert_eq!(
            super::config::sort_folders_from(b"/a b\n/caf\xe9\n\n"),
            [std::path::PathBuf::from("/a b"), latin.into()]
        );
    }

    #[test]
    fn survives_a_panic() {
        assert_eq!(super::decode::survive(|| Ok::<_, String>(1)), Ok(1));
        assert!(
            super::decode::survive(|| -> Result<(), String> { panic!("broken file") }).is_err()
        );
    }

    #[test]
    fn pattern_names() {
        let old = std::path::Path::new("/p/IMG_4711.JPG");
        assert_eq!(
            super::files::pattern_name("trip_###", old, 7),
            "trip_007.JPG"
        );
        assert_eq!(
            super::files::pattern_name("*-#", old, 12),
            "IMG_4711-12.JPG"
        );
        assert_eq!(
            super::files::pattern_name("x", std::path::Path::new("/p/noext"), 1),
            "x"
        );
    }

    #[test]
    fn without_indices_shifts_down() {
        let map = std::collections::HashMap::from([(1, 'a'), (2, 'b'), (4, 'c'), (6, 'd')]);
        let expected = std::collections::HashMap::from([(1, 'a'), (3, 'd')]);
        assert_eq!(super::without_indices(map, &[2, 4, 5]), expected);
    }

    #[test]
    fn formats_sizes() {
        assert_eq!(super::human_size(512), "512 B");
        assert_eq!(super::human_size(850_000), "850.0 kB");
        assert_eq!(super::human_size(2_345_678), "2.3 MB");
        assert_eq!(super::human_size(1_200_000_000), "1.2 GB");
    }

    #[test]
    fn formats_thousands() {
        assert_eq!(super::thousands(7), "7");
        assert_eq!(super::thousands(1000), "1,000");
        assert_eq!(super::thousands(300000), "300,000");
        assert_eq!(super::thousands(1234567), "1,234,567");
    }

    #[test]
    fn sorts_numbers_by_value() {
        use std::cmp::Ordering::*;
        assert_eq!(super::files::natural_cmp("img2.jpg", "img10.jpg"), Less);
        assert_eq!(super::files::natural_cmp("img010", "img10"), Equal);
        assert_eq!(super::files::natural_cmp("a", "a1"), Less);
        let mut v: Vec<std::ffi::OsString> =
            ["b10", "B2", "a1", "b9"].iter().map(Into::into).collect();
        super::files::sort_files(
            std::path::Path::new("."),
            &mut v,
            super::files::Sort::Number,
            false,
            false,
        );
        assert_eq!(v, ["b10", "b9", "B2", "a1"]);
        // Equal numbers in name order.
        let mut v: Vec<std::ffi::OsString> = ["a1", "a01"].iter().map(Into::into).collect();
        let dot = std::path::Path::new(".");
        super::files::sort_files(dot, &mut v, super::files::Sort::Number, true, false);
        assert_eq!(v, ["a01", "a1"]);
        let mut v: Vec<std::ffi::OsString> = ["b.png", "c.JPG", "a.mp4", "d.jpg", "e"]
            .iter()
            .map(Into::into)
            .collect();
        super::files::sort_files(dot, &mut v, super::files::Sort::Extension, true, false);
        assert_eq!(v, ["e", "c.JPG", "d.jpg", "a.mp4", "b.png"]);
    }

    #[test]
    fn formats_local_time() {
        let t = super::local_time(1_700_000_000);
        assert_eq!(t.len(), "2023-11-14 22:13:20".len(), "{t}");
        assert!(t.starts_with("2023-11-1"), "{t}");
    }

    #[test]
    fn merge_keeps_shuffled_order() {
        let names = |v: &[&str]| v.iter().map(std::ffi::OsString::from).collect::<Vec<_>>();
        let merged =
            super::files::merge_shuffled(&names(&["c", "a", "b"]), names(&["a", "b", "d"]));
        assert_eq!(merged, names(&["a", "b", "d"]));
    }

    #[test]
    fn shuffle_keeps_all_elements() {
        let mut v: Vec<u32> = (0..100).collect();
        super::files::shuffle(&mut v);
        assert_ne!(v, (0..100).collect::<Vec<_>>());
        v.sort();
        assert_eq!(v, (0..100).collect::<Vec<_>>());
    }

    #[test]
    fn writes_atomically() {
        let dir = std::env::temp_dir().join(format!("glimt-test-atomic-{}", std::process::id()));
        let path = dir.join("config");
        super::config::write_atomic(&path, "a=1\n");
        super::config::write_atomic(&path, "a=2\n");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a=2\n");
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            1,
            "no .tmp files left"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn parses_config() {
        use super::config::Config;
        assert_eq!(Config::parse("# comment\npreload= 5\n").preload, 5);
        assert_eq!(Config::parse("preload=99").preload, 20);
        let c = Config::parse("decode_threads=0\nanimation_mb=5000");
        assert_eq!((c.decode_threads, c.animation_mb), (1, 1024));
        // The defaults are the Medium level.
        let d = Config::default();
        let medium = super::MEMORY_LEVELS
            .iter()
            .find(|l| l.0 == "Medium")
            .unwrap();
        assert_eq!(
            (d.preload, d.decode_threads, d.animation_mb, d.low_memory),
            (medium.1, medium.2, medium.3, medium.4)
        );
        assert_eq!(Config::parse("delay=0").delay, 1);
        assert_eq!(Config::parse("delay=7").delay, 7);
        // Every setting survives a save and load.
        let mut c = Config::parse("trash=false\nsort=size\nbackground=#102030\ntoolbar_icons=2");
        c.video_player = "mpv --fs".into();
        assert_eq!(Config::parse(&c.to_text()), c);
        assert!(Config::default().trash && !Config::parse("trash=false").trash);
        assert!(
            Config::default().confirm_trash && !Config::parse("confirm_trash=false").confirm_trash
        );
        assert!(Config::default().delete_key && !Config::parse("delete_key=false").delete_key);
        assert!(Config::parse("remember_dir=true").remember_dir);
        assert!(!Config::default().random && Config::parse("random=true").random);
        assert!(Config::default().enlarge && !Config::parse("enlarge=false").enlarge);
        assert!(Config::default().wheel_zoom && !Config::parse("wheel_zoom=false").wheel_zoom);
        assert!(Config::default().thumbnails && !Config::parse("thumbnails=false").thumbnails);
        assert!(Config::default().save_thumbnails);
        assert!(!Config::default().exif_panel && Config::parse("exif_panel=true").exif_panel);
        assert!(Config::default().toolbar && !Config::parse("toolbar=false").toolbar);
        assert!(Config::default().low_memory && !Config::parse("low_memory=false").low_memory);
        assert!(Config::default().path_bar && !Config::parse("path_bar=false").path_bar);
        assert_eq!(Config::default().background.0, [0, 0, 0]);
        assert!(Config::default().remember_window && Config::default().inhibit_screensaver);
        assert!(!Config::parse("remember_window=false").remember_window);
        assert!(!Config::parse("inhibit_screensaver=false").inhibit_screensaver);
        assert!(!Config::default().app_name_first);
        assert!(!Config::default().file_details && Config::parse("file_details=true").file_details);
        let c = Config::parse("sort=exif_digitized\nsort_ascending=false\nsort_case=false");
        assert_eq!(
            (c.sort, c.sort_ascending, c.sort_case),
            (super::files::Sort::ExifDigitized, false, false)
        );
        assert_eq!(
            Config::parse("sort=nonsense").sort,
            super::files::Sort::Name
        );
        assert_eq!(Config::default().toolbar_icons, 0);
        assert!(Config::default().search_bar && !Config::parse("search_bar=false").search_bar);
        assert_eq!(Config::parse("toolbar_icons=1").toolbar_icons, 1);
        assert_eq!(Config::parse("toolbar_icons=9").toolbar_icons, 2);
        assert!(Config::parse("app_name_first=true").app_name_first);
        let win = super::config::WindowState {
            size: Some(super::egui::vec2(1000.0, 700.5)),
            pos: Some(super::egui::pos2(10.0, -20.0)),
            maximized: true,
        };
        let text = format!("last_dir=/x\n{}", win.to_text());
        assert_eq!(super::config::WindowState::parse(&text), win);
        assert_eq!(
            super::config::WindowState::parse("window_size=5,5\n").size,
            None
        );
        assert_eq!(
            Config::parse("background=#1a2B3c").background.0,
            [0x1a, 0x2b, 0x3c]
        );
        assert_eq!(Config::parse("background=#12").background.0, [0, 0, 0]);
        assert!(!Config::parse("save_thumbnails=false").save_thumbnails);
        assert!(Config::default().exif_rotate && !Config::parse("exif_rotate=false").exif_rotate);
        assert!(Config::default().videos && !Config::parse("videos=false").videos);
        assert_eq!(
            Config::parse("video_player= mpv --fs ").video_player,
            "mpv --fs"
        );
        assert!(
            Config::default().auto_refresh && !Config::parse("auto_refresh=false").auto_refresh
        );
        assert_eq!(
            super::config::parse_last_dir("last_dir=/tmp/a b\n"),
            Some("/tmp/a b".into())
        );
        assert_eq!(super::config::parse_last_dir("last_dir=\n"), None);
        assert_eq!(Config::parse("preload=x\nunknown=1"), Config::default());
    }

    /// Two tiles side by side, cut to 1000×500 and turned a quarter: the keys as the parser
    /// reads them from ffprobe's compact output.
    #[test]
    fn heif_grid_filter() {
        let probe = "stream_group|index=0|type=Tile Grid|c:tile_grid:width=1000|\
            c:tile_grid:height=500|c:tile_grid:horizontal_offset=0|\
            c:tile_grid:vertical_offset=0|c:rotation=-90|\
            t:stream_index=0|t:tile_horizontal_offset=0|t:tile_vertical_offset=0|\
            t:stream_index=1|t:tile_horizontal_offset=512|t:tile_vertical_offset=0\n";
        assert_eq!(
            super::external::heif_filter(probe).as_deref(),
            Some(
                "[0:0][0:1]xstack=inputs=2:layout=0_0|512_0,crop=1000:500:0:0,\
                 transpose=clock[out]"
            )
        );
        assert_eq!(super::external::heif_filter("stream|index=0\n"), None);
        // A value that is not a number is left out, and adds no filter of its own.
        let hostile = probe.replace("t:stream_index=1", "t:stream_index=1];movie=x[a");
        assert!(
            !super::external::heif_filter(&hostile)
                .unwrap()
                .contains("movie")
        );
    }
}
