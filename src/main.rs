//! Glimt: a fast, memory-frugal image viewer.
//!
//! Images are decoded in background threads to the size of the image area; full resolution
//! is decoded only when the user zooms past what that covers. Only a small window around
//! the current image is kept in memory.

use eframe::egui;
use image::ImageDecoder;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Instant;

mod exif;
mod thumbs;
#[cfg(feature = "video")]
mod video;

const EXTS: &[&str] = &["jpg", "jpeg", "png", "webp", "gif", "tif", "tiff"];
/// Default number of images kept decoded on each side of the shown one.
const DEFAULT_AHEAD: usize = 2;
/// Default seconds between images in a slideshow.
const DEFAULT_DELAY: u64 = 3;
const MAX_DELAY: u64 = 60;
/// Thumbnail box in the file list, in points (Geeqie's default 96×72).
const THUMB: egui::Vec2 = egui::vec2(96.0, 72.0);
/// Thumbnail jobs further than this from the visible rows are skipped.
const THUMB_MARGIN: usize = 20;
/// Thumbnails kept in memory (≈ 64 kB each) before those far from view are dropped.
const MAX_THUMBS: usize = 600;

/// Everything the user can do from a menu or a shortcut.
#[derive(Clone, Copy, PartialEq)]
enum Action {
    OpenFile,
    OpenFolder,
    NewFolder,
    Rename,
    Copy,
    Move,
    OpenWith,
    Play,
    Delete,
    DeletePermanently,
    CloseWindow,
    Quit,
    First,
    Prev,
    Next,
    Last,
    Back,
    Forward,
    Home,
    Up,
    SelectAll,
    SelectNone,
    InvertSelection,
    CopyPath,
    CopyPathUnquoted,
    Preferences,
    ExifRotate,
    ZoomIn,
    ZoomOut,
    Zoom1To1,
    ZoomFit,
    FullScreen,
    HideTools,
    Toolbar,
    Thumbnails,
    ExifPanel,
    Refresh,
    Slideshow,
    Pause,
    RandomOrder,
    Faster,
    Slower,
    Escape,
    NewWindow,
    Help,
}

impl Action {
    /// The name shown in menus and in the help window.
    fn label(self) -> &'static str {
        use Action::*;
        match self {
            OpenFile => "Open File…",
            OpenFolder => "Open Folder…",
            NewFolder => "New Folder…",
            Rename => "Rename…",
            Copy => "Copy…",
            Move => "Move…",
            OpenWith => "Open With Default App",
            Play => "Play Video",
            Delete => "Delete…",
            DeletePermanently => "Delete Permanently…",
            CloseWindow => "Close Window",
            Quit => "Quit",
            First => "First Image",
            Prev => "Previous Image",
            Next => "Next Image",
            Last => "Last Image",
            Back => "Back",
            Forward => "Forward",
            Home => "Home",
            Up => "Up",
            SelectAll => "Select All",
            SelectNone => "Select None",
            InvertSelection => "Invert Selection",
            CopyPath => "Copy Path to Clipboard",
            CopyPathUnquoted => "Copy Path (Unquoted)",
            Preferences => "Preferences…",
            ExifRotate => "Auto Rotate Using EXIF",
            ZoomIn => "Zoom In",
            ZoomOut => "Zoom Out",
            Zoom1To1 => "Zoom 1:1",
            ZoomFit => "Zoom to Fit",
            FullScreen => "Full Screen",
            HideTools => "Hide File List",
            Toolbar => "Show Toolbar",
            Thumbnails => "Show Thumbnails",
            ExifPanel => "Show EXIF Panel",
            Refresh => "Refresh",
            Slideshow => "Start/Stop Slideshow",
            Pause => "Pause Slideshow",
            RandomOrder => "Random Order",
            Faster => "Faster",
            Slower => "Slower",
            Escape => "Stop Slideshow, Leave Full Screen",
            NewWindow => "New Window",
            Help => "Help",
        }
    }
}

const NONE: egui::Modifiers = egui::Modifiers::NONE;
const SHIFT: egui::Modifiers = egui::Modifiers::SHIFT;
const CTRL: egui::Modifiers = egui::Modifiers::COMMAND;
const ALT: egui::Modifiers = egui::Modifiers::ALT;
const CTRL_SHIFT: egui::Modifiers = egui::Modifiers {
    shift: true,
    ..egui::Modifiers::COMMAND
};

/// Geeqie's default shortcuts, except R (random order; Geeqie: refresh) and F5 (refresh).
/// The first one for an action is shown in the menu.
/// egui ignores an extra Shift or Alt when matching, so those combinations come before the plain key.
const SHORTCUTS: &[(Action, egui::Modifiers, egui::Key)] = {
    use Action::*;
    use egui::Key;
    &[
        (ExifRotate, ALT, Key::X),
        (DeletePermanently, SHIFT, Key::Delete),
        (Delete, NONE, Key::Delete),
        (Delete, CTRL, Key::D),
        (NewFolder, CTRL, Key::F),
        (Rename, CTRL, Key::R),
        (Copy, CTRL, Key::C),
        (Move, CTRL, Key::M),
        (CloseWindow, CTRL, Key::W),
        (Quit, CTRL, Key::Q),
        (First, NONE, Key::Home),
        (Prev, NONE, Key::Backspace),
        (Prev, NONE, Key::PageUp),
        (Prev, NONE, Key::ArrowLeft),
        (Prev, NONE, Key::ArrowUp),
        (Next, NONE, Key::Space),
        (Next, NONE, Key::PageDown),
        (Next, NONE, Key::ArrowRight),
        (Next, NONE, Key::ArrowDown),
        (Last, NONE, Key::End),
        (SelectNone, CTRL_SHIFT, Key::A),
        (InvertSelection, CTRL_SHIFT, Key::I),
        (SelectAll, CTRL, Key::A),
        (Preferences, CTRL, Key::O),
        (ZoomIn, NONE, Key::Equals),
        (ZoomIn, NONE, Key::Plus),
        (ZoomOut, NONE, Key::Minus),
        (Zoom1To1, NONE, Key::Z),
        (ZoomFit, NONE, Key::X),
        (FullScreen, NONE, Key::F),
        (FullScreen, NONE, Key::V),
        (FullScreen, NONE, Key::F11),
        (HideTools, CTRL, Key::H),
        (Thumbnails, NONE, Key::T),
        (ExifPanel, CTRL, Key::K),
        (Refresh, NONE, Key::F5),
        (Slideshow, NONE, Key::S),
        (Pause, NONE, Key::P),
        (RandomOrder, NONE, Key::R),
        (Faster, CTRL, Key::Equals),
        (Slower, CTRL, Key::Minus),
        (Play, NONE, Key::Enter),
        (Escape, NONE, Key::Escape),
        (Escape, NONE, Key::Q),
        (NewWindow, CTRL, Key::N),
        (Help, NONE, Key::F1),
    ]
};

/// What a zenity file dialog was opened for.
enum Pick {
    Open,
    CopyTo(Vec<PathBuf>),
    MoveTo(Vec<PathBuf>),
}

/// What the one-line text dialog is asking for.
#[derive(Clone, Copy)]
enum Prompt {
    NewFolder,
    /// Rename the shown image.
    Rename,
}

/// A menu entry showing the action's shortcut; sets `act` when clicked.
fn menu_item(ui: &mut egui::Ui, act: &mut Option<Action>, action: Action) {
    menu_button(ui, act, egui::Button::new(action.label()), action);
}

/// Mouse use, for the help window.
const MOUSE: &[(&str, &str)] = &[
    ("Mouse wheel", "Zoom, or next/previous image (Preferences)"),
    ("Ctrl+mouse wheel", "Zoom"),
    ("Drag", "Move a zoomed image"),
    ("Double-click", "Full Screen"),
    ("Ctrl+click in the list", "Select or unselect a file"),
];

/// Where Glimt differs from Geeqie: what, Glimt, Geeqie. Shown in the Help Manual (F1).
const DIFFERENCES: &[(&str, &str, &str)] = &[
    (
        "R",
        "Toggles random order in the slideshow, also while it runs",
        "Refresh",
    ),
    ("Refresh", "F5", "R"),
    (
        "Random order",
        "R, the View menu and Preferences",
        "Only in Preferences",
    ),
    (
        "Arrow keys",
        "←/↑ previous, →/↓ next image",
        "Scroll a zoomed image, move in the file list",
    ),
    (
        "Mouse wheel",
        "Zooms by default; a setting makes it go to the next or previous image",
        "Next or previous image",
    ),
    (
        "Esc / Q",
        "Stops the slideshow and leaves full screen; never quits",
        "Leaves full screen",
    ),
    (
        "Slideshow delay",
        "Whole seconds 1–60, Ctrl+= / Ctrl+- change it by 1 s",
        "Tenths of a second, Faster/Slower change it by 0.5 s",
    ),
    (
        "Browsing in a slideshow",
        "The new image gets the full delay",
        "",
    ),
    (
        "Stopping a random slideshow",
        "The order is kept, so Previous goes back through the shown images",
        "",
    ),
    (
        "Small images",
        "\"Enlarge small images to fit the window\", on by default",
        "\"Allow enlargement\"",
    ),
    (
        "Close Window (Ctrl+W)",
        "Same as Quit (one window per process)",
        "Closes one of several windows",
    ),
    (
        "Keypad /, *, +, -",
        "Not supported",
        "Zoom 1:1, fit, in, out",
    ),
    ("1–6, Ctrl+1–6", "Not used (no marks yet)", "Marks"),
    (
        "Rename (Ctrl+R)",
        "Only the shown image",
        "Selected files, with a pattern for several",
    ),
    (
        "Open With",
        "The default application",
        "Choose an application",
    ),
    (
        "Copy, Move (Ctrl+C, Ctrl+M)",
        "Target folder from a file dialog; never overwrites",
        "Own dialog",
    ),
    (
        "New Window (Ctrl+N)",
        "A new process in the same folder",
        "A new window in the same process, saved layouts",
    ),
    (
        "EXIF",
        "Panel on the right (Ctrl+K), off by default; selected fields",
        "Info sidebar (Ctrl+K) and an EXIF window (Ctrl+E) with all fields",
    ),
    (
        "Thumbnail cache",
        "The shared ~/.cache/thumbnails; new ones are saved only if the setting is on",
        "Own cache, or the shared one",
    ),
    (
        "Refresh on change",
        "Setting, on by default; checks the folder once a second",
        "Setting",
    ),
];

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

/// The first shortcut of an action, as shown in menus ("Ctrl+K").
fn shortcut_text(ctx: &egui::Context, action: Action) -> Option<String> {
    let &(_, mods, key) = SHORTCUTS.iter().find(|s| s.0 == action)?;
    Some(ctx.format_shortcut(&egui::KeyboardShortcut::new(mods, key)))
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

struct Decoded {
    idx: usize,
    full: bool,
    /// The file the job was for; rejects results from a folder that has since been replaced.
    path: PathBuf,
    /// Decoded with EXIF rotation; rejected if the setting changed meanwhile.
    rotated: bool,
    /// `None` = skipped because the user has already moved past it.
    result: Option<Result<(egui::ColorImage, [usize; 2]), String>>,
}

struct Slot {
    tex: egui::TextureHandle,
    /// The file the texture was made from; decides whether it can be kept when the file list changes.
    path: PathBuf,
    full: bool,
    /// Original size after EXIF rotation.
    orig: [usize; 2],
}

struct App {
    dir: PathBuf,
    subdirs: Vec<PathBuf>,
    /// The images in `dir`, file name only (the full path is built on demand by `path`): saves
    /// memory in large folders, where the path would otherwise be repeated for every file.
    files: Vec<OsString>,
    cur: usize,
    current: Arc<AtomicUsize>,
    /// Number of images preloaded on each side (Preferences).
    ahead: Arc<AtomicUsize>,
    /// Workers apply EXIF orientation (Preferences, Alt+X).
    rotate: Arc<AtomicBool>,
    show_settings: bool,
    show_help: bool,
    /// The confirmation dialog for permanent deletion is open.
    confirm: bool,
    /// Open text dialog and the text typed so far.
    prompt: Option<(Prompt, String)>,
    /// Scale from the previous frame (physical pixels per image pixel), for the zoom percentage in the status bar.
    last_scale: f32,
    /// Mouse wheel notches not yet turned into an image step (wheel_zoom off).
    wheel: f32,
    /// Last drawn image; kept on screen until the next one is decoded, so browsing never shows a black screen.
    last_drawn: Option<(egui::TextureHandle, [usize; 2])>,
    /// Selected files (Ctrl/Shift+click in the file list) that Delete acts on.
    selected: std::collections::BTreeSet<usize>,
    config: Config,
    /// Max decode size in physical pixels: [image area in the window, max texture side].
    limits: Arc<Mutex<[[u32; 2]; 2]>>,
    /// (image index, full resolution, file)
    jobs: mpsc::Sender<(usize, bool, PathBuf)>,
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
    /// First and last visible row of the file list, for skipping stale thumbnail jobs.
    thumb_view: Arc<[AtomicUsize; 2]>,
    /// Workers write new thumbnails to the shared cache (Preferences).
    save_thumbs: Arc<AtomicBool>,
    /// EXIF panel: the file asked for last, and the rows read for a file.
    exif_jobs: mpsc::Sender<PathBuf>,
    exif_done: mpsc::Receiver<(PathBuf, Vec<(&'static str, String)>)>,
    exif_asked: Option<PathBuf>,
    exif: Option<(PathBuf, Vec<(&'static str, String)>)>,
    /// Size in bytes of the shown file, for the status bar.
    cur_size: Option<(PathBuf, u64)>,
    /// The folder path in the toolbar, as typed.
    path_text: String,
}

fn has_ext(path: &Path, exts: &[&str]) -> bool {
    let ext = path.extension().and_then(|e| e.to_str());
    ext.is_some_and(|e| exts.contains(&e.to_ascii_lowercase().as_str()))
}

#[cfg(feature = "video")]
fn is_video(path: &Path) -> bool {
    has_ext(path, video::EXTS)
}

#[cfg(not(feature = "video"))]
fn is_video(_: &Path) -> bool {
    false
}

/// Shown in the file list: an image, or a video with the video feature and `videos` on.
fn is_listed(path: &Path, videos: bool) -> bool {
    has_ext(path, EXTS) || (videos && is_video(path))
}

/// One pass over the folder: (subfolders, image file names), both sorted by name.
/// The file type comes from the directory entry, so there is no `stat` per file (only symlinks are resolved).
fn list_folder(dir: &Path, videos: bool) -> (Vec<PathBuf>, Vec<OsString>) {
    let (mut dirs, mut images) = (Vec::new(), Vec::new());
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if is_listed(&path, videos) {
            images.push(entry.file_name());
        } else if !entry.file_name().as_encoded_bytes().starts_with(b".")
            && entry
                .file_type()
                .is_ok_and(|t| t.is_dir() || (t.is_symlink() && path.is_dir()))
        {
            dirs.push(path);
        }
    }
    sort_by_name(&mut dirs);
    images.sort_unstable(); // byte-wise on Unix
    (dirs, images)
}

/// Paths in the same folder: byte-wise comparison of the whole path gives name order, and is
/// ~9× faster than Path::cmp/file_name(), which re-parses the path in every comparison.
fn sort_by_name(paths: &mut [PathBuf]) {
    paths.sort_unstable_by(|a, b| a.as_os_str().cmp(b.as_os_str()));
}

/// `$XDG_CONFIG_HOME/glimt/config`, otherwise `~/.config/glimt/config`.
fn config_path() -> Option<PathBuf> {
    xdg_file("XDG_CONFIG_HOME", ".config", "config")
}

/// `$XDG_STATE_HOME/glimt/state`, otherwise `~/.local/state/glimt/state`.
/// State (such as the last folder) is not configuration, and belongs here according to XDG.
fn state_path() -> Option<PathBuf> {
    xdg_file("XDG_STATE_HOME", ".local/state", "state")
}

fn xdg_file(var: &str, home_fallback: &str, name: &str) -> Option<PathBuf> {
    Some(xdg_dir(var, home_fallback, "glimt")?.join(name))
}

fn xdg_dir(var: &str, home_fallback: &str, app: &str) -> Option<PathBuf> {
    let base = std::env::var_os(var)
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(home_fallback)))?;
    Some(base.join(app))
}

fn parse_last_dir(state: &str) -> Option<PathBuf> {
    state.lines().find_map(|l| {
        l.strip_prefix("last_dir=")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    })
}

/// Preferences, stored as `key=value` lines. Unknown or invalid lines are ignored.
#[derive(Debug, PartialEq)]
struct Config {
    preload: usize,
    /// Seconds between images in a slideshow.
    delay: u64,
    /// Start in the last used folder when no folder is given on the command line.
    remember_dir: bool,
    /// Deleting moves to the trash (default); otherwise the file is deleted permanently.
    trash: bool,
    /// Ask before deleting permanently.
    confirm_delete: bool,
    /// Slideshow in random order.
    random: bool,
    /// Rotate images by their EXIF orientation (Geeqie: "Auto rotate image using Exif information").
    exif_rotate: bool,
    /// Video feature: list video files.
    videos: bool,
    /// Video feature: command line that plays a video; empty = the default application.
    video_player: String,
    /// Re-read the folder when files are added, removed or renamed.
    auto_refresh: bool,
    /// Zoom to fit also enlarges images smaller than the window (Geeqie: "Allow enlargement").
    enlarge: bool,
    /// The mouse wheel zooms; off, it goes to the next or previous image as in Geeqie.
    wheel_zoom: bool,
    /// Thumbnails in the file list (T).
    thumbnails: bool,
    /// Write new thumbnails to the shared cache (~/.cache/thumbnails).
    save_thumbnails: bool,
    /// EXIF panel on the right (Ctrl+K); off by default, as reading EXIF costs a little per image.
    exif_panel: bool,
    /// Toolbar under the menu, as in Geeqie.
    toolbar: bool,
    /// One malloc arena for all threads: much less memory, decoding up to ~8 % slower.
    /// Read at startup only, since glibc must be set before threads start.
    low_memory: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            preload: DEFAULT_AHEAD,
            delay: DEFAULT_DELAY,
            remember_dir: false,
            trash: true,
            confirm_delete: true,
            random: false,
            enlarge: true,
            wheel_zoom: true,
            thumbnails: true,
            // Off while Glimt is in development, so testing leaves no files behind; on for release.
            save_thumbnails: false,
            exif_panel: false,
            toolbar: true,
            low_memory: true,
            auto_refresh: true,
            exif_rotate: true,
            videos: true,
            video_player: String::new(),
        }
    }
}

impl Config {
    fn parse(text: &str) -> Self {
        let mut c = Config::default();
        for (key, value) in text.lines().filter_map(|l| l.split_once('=')) {
            let value = value.trim();
            match key.trim() {
                "preload" => c.preload = value.parse().map_or(c.preload, |n: usize| n.min(20)),
                "delay" => {
                    c.delay = value
                        .parse()
                        .map_or(c.delay, |n: u64| n.clamp(1, MAX_DELAY))
                }
                "remember_dir" => c.remember_dir = value == "true",
                "trash" => c.trash = value == "true",
                "confirm_delete" => c.confirm_delete = value == "true",
                "random" => c.random = value == "true",
                "enlarge" => c.enlarge = value == "true",
                "wheel_zoom" => c.wheel_zoom = value == "true",
                "thumbnails" => c.thumbnails = value == "true",
                "save_thumbnails" => c.save_thumbnails = value == "true",
                "exif_panel" => c.exif_panel = value == "true",
                "toolbar" => c.toolbar = value == "true",
                "low_memory" => c.low_memory = value == "true",
                "auto_refresh" => c.auto_refresh = value == "true",
                "exif_rotate" => c.exif_rotate = value == "true",
                "videos" => c.videos = value == "true",
                "video_player" => c.video_player = value.to_owned(),
                _ => {}
            }
        }
        c
    }

    fn load() -> Self {
        config_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map_or_else(Config::default, |text| Config::parse(&text))
    }

    fn save(&self) {
        let Some(p) = config_path() else { return };
        let text = format!(
            "preload={}\ndelay={}\nremember_dir={}\ntrash={}\nconfirm_delete={}\nrandom={}\nenlarge={}\nwheel_zoom={}\nthumbnails={}\nsave_thumbnails={}\nexif_panel={}\ntoolbar={}\nlow_memory={}\nauto_refresh={}\nexif_rotate={}\nvideos={}\nvideo_player={}\n",
            self.preload,
            self.delay,
            self.remember_dir,
            self.trash,
            self.confirm_delete,
            self.random,
            self.enlarge,
            self.wheel_zoom,
            self.thumbnails,
            self.save_thumbnails,
            self.exif_panel,
            self.toolbar,
            self.low_memory,
            self.auto_refresh,
            self.exif_rotate,
            self.videos,
            self.video_player
        );
        write_atomic(&p, &text);
    }
}

/// Write to a temporary file and rename it, so the file is never half-written after a crash.
/// The process ID is in the name, so concurrent sessions never write to the same temporary file.
fn write_atomic(path: &Path, text: &str) {
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    let result = std::fs::create_dir_all(path.parent().unwrap())
        .and_then(|_| std::fs::write(&tmp, text))
        .and_then(|_| std::fs::rename(&tmp, path));
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        eprintln!("could not save {}: {e}", path.display());
    }
}

/// Trash for systems without gio: the freedesktop.org home trash,
/// `$XDG_DATA_HOME/Trash`, otherwise `~/.local/share/Trash`.
fn trash_without_gio(paths: &[PathBuf]) -> Vec<String> {
    let trash = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .map(|d| d.join("Trash"));
    // The spec wants local time; `date` saves a time zone dependency.
    let date = std::process::Command::new("date")
        .arg("+%Y-%m-%dT%H:%M:%S")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_default();
    match trash {
        Some(trash) => trash_into(&trash, paths, &date),
        None => vec!["no trash folder: HOME is not set".into()],
    }
}

/// Move `paths` into the trash folder `trash`; returns the errors.
/// ponytail: home trash only; files on other file systems fail (rename across devices) instead
/// of going to `$topdir/.Trash-$uid`.
fn trash_into(trash: &Path, paths: &[PathBuf], date: &str) -> Vec<String> {
    let (files, info) = (trash.join("files"), trash.join("info"));
    if let Err(e) = std::fs::create_dir_all(&files).and_then(|_| std::fs::create_dir_all(&info)) {
        return vec![format!("{}: {e}", trash.display())];
    }
    paths
        .iter()
        .filter_map(|path| {
            let e = trash_one(&files, &info, path, date).err()?;
            Some(format!("{}: {e}", path.display()))
        })
        .collect()
}

fn trash_one(files: &Path, info: &Path, path: &Path, date: &str) -> std::io::Result<()> {
    use std::io::Write;
    let path = std::path::absolute(path)?;
    let name = path.file_name().ok_or(std::io::ErrorKind::InvalidInput)?;
    for n in 1.. {
        let mut entry = name.to_os_string();
        if n > 1 {
            entry.push(format!(".{n}"));
        }
        let mut info_name = entry.clone();
        info_name.push(".trashinfo");
        let info_path = info.join(info_name);
        // Creating the .trashinfo exclusively reserves the name, as the spec says.
        let open = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&info_path);
        let mut f = match open {
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            other => other?,
        };
        if files.join(&entry).exists() {
            let _ = std::fs::remove_file(&info_path);
            continue;
        }
        let path_url = percent_encode(&path);
        let result = write!(f, "[Trash Info]\nPath={path_url}\nDeletionDate={date}\n")
            .and_then(|_| std::fs::rename(&path, files.join(&entry)));
        if result.is_err() {
            let _ = std::fs::remove_file(&info_path);
        }
        return result;
    }
    unreachable!()
}

/// Percent-encode a path for `.trashinfo` (RFC 2396), keeping `/` and unreserved bytes.
fn percent_encode(path: &Path) -> String {
    let bytes = path.as_os_str().as_encoded_bytes();
    bytes
        .iter()
        .map(|&b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// New index for `i` when the (sorted) indices in `removed` are taken out of the list; `None` if `i` itself is removed.
fn shifted(i: usize, removed: &[usize]) -> Option<usize> {
    removed.binary_search(&i).err().map(|before| i - before)
}

/// Drop the entries for `removed` (sorted) and shift the rest down accordingly.
fn without_indices<T>(map: HashMap<usize, T>, removed: &[usize]) -> HashMap<usize, T> {
    map.into_iter()
        .filter_map(|(i, v)| Some((shifted(i, removed)?, v)))
        .collect()
}

/// Sum of the file sizes (one stat per file); unreadable files are not counted.
fn total_size(paths: &[PathBuf]) -> u64 {
    paths
        .iter()
        .filter_map(|p| p.metadata().ok())
        .map(|m| m.len())
        .sum()
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

/// The shuffled `old` order without files that are gone, followed by new files in random order.
fn merge_shuffled(old: &[OsString], new: Vec<OsString>) -> Vec<OsString> {
    let old_set: HashSet<&OsString> = old.iter().collect();
    let new_set: HashSet<&OsString> = new.iter().collect();
    let mut added: Vec<OsString> = new
        .iter()
        .filter(|f| !old_set.contains(f))
        .cloned()
        .collect();
    shuffle(&mut added);
    let mut out: Vec<OsString> = old
        .iter()
        .filter(|f| new_set.contains(f))
        .cloned()
        .collect();
    out.extend(added);
    out
}

/// Fisher–Yates with xorshift seeded from std's random hash key (no rand dependency).
fn shuffle<T>(v: &mut [T]) {
    use std::hash::{BuildHasher, Hasher};
    let mut x = std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish()
        | 1;
    for i in (1..v.len()).rev() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        v.swap(i, (x % (i as u64 + 1)) as usize);
    }
}

fn decode(
    path: &Path,
    max: [u32; 2],
    rotate: bool,
) -> Result<(egui::ColorImage, [usize; 2]), String> {
    let img = load(path, rotate)?;
    let orig = [img.width() as usize, img.height() as usize];
    Ok((color_image(shrink(img, max)?), orig))
}

/// The whole image (a preview frame for videos), turned upright by EXIF if `rotate`.
fn load(path: &Path, rotate: bool) -> Result<image::DynamicImage, String> {
    #[cfg(feature = "video")]
    if is_video(path) {
        return video::preview(path);
    }
    let mut dec = image::ImageReader::open(path)
        .and_then(|r| r.with_guessed_format())
        .map_err(|e| e.to_string())?
        .into_decoder()
        .map_err(|e| e.to_string())?;
    let orientation = dec
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = image::DynamicImage::from_decoder(dec).map_err(|e| e.to_string())?;
    if rotate {
        img.apply_orientation(orientation);
    }
    Ok(img)
}

/// Scale down to fit within `max`.
fn shrink(img: image::DynamicImage, max: [u32; 2]) -> Result<image::DynamicImage, String> {
    if img.width() > max[0] || img.height() > max[1] {
        let scale = (max[0] as f64 / img.width() as f64).min(max[1] as f64 / img.height() as f64);
        let w = ((img.width() as f64 * scale).round() as u32).max(1);
        let h = ((img.height() as f64 * scale).round() as u32).max(1);
        let mut small = image::DynamicImage::new(w, h, img.color());
        // SIMD downscaling; image's thumbnail took ~150–330 ms on 12 MP.
        let opts = fast_image_resize::ResizeOptions::new().resize_alg(
            fast_image_resize::ResizeAlg::Convolution(fast_image_resize::FilterType::Bilinear),
        );
        fast_image_resize::Resizer::new()
            .resize(&img, &mut small, &opts)
            .map_err(|e| e.to_string())?;
        return Ok(small);
    }
    Ok(img)
}

fn color_image(img: image::DynamicImage) -> egui::ColorImage {
    let rgba = img.into_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw())
}

impl App {
    fn new(cc: &eframe::CreationContext, path: Option<PathBuf>) -> Self {
        let current = Arc::new(AtomicUsize::new(0));
        let config = Config::load();
        let path = path
            .or_else(|| {
                let state = std::fs::read_to_string(state_path()?).ok()?;
                parse_last_dir(&state).filter(|d| config.remember_dir && d.is_dir())
            })
            .unwrap_or_else(|| ".".into());
        let ahead = Arc::new(AtomicUsize::new(config.preload));
        let rotate = Arc::new(AtomicBool::new(config.exif_rotate));
        let limits = Arc::new(Mutex::new([[1920, 1080], [8192, 8192]]));
        let (jobs, job_rx) = mpsc::channel::<(usize, bool, PathBuf)>();
        let (done_tx, done) = mpsc::channel();
        let job_rx = Arc::new(Mutex::new(job_rx));
        let workers = std::thread::available_parallelism().map_or(2, |n| n.get().min(4));
        for _ in 0..workers {
            let (current, ahead, rotate, limits, job_rx, done_tx, ctx) = (
                current.clone(),
                ahead.clone(),
                rotate.clone(),
                limits.clone(),
                job_rx.clone(),
                done_tx.clone(),
                cc.egui_ctx.clone(),
            );
            std::thread::spawn(move || {
                loop {
                    let Ok((idx, full, path)) = job_rx.lock().unwrap().recv() else {
                        return;
                    };
                    // Cancellation: skip images the user has already moved past.
                    let cur = current.load(Ordering::Relaxed);
                    if idx.abs_diff(cur) > ahead.load(Ordering::Relaxed) || (full && idx != cur) {
                        let _ = done_tx.send(Decoded {
                            rotated: false,
                            idx,
                            full,
                            path,
                            result: None,
                        });
                        continue;
                    }
                    let max = limits.lock().unwrap()[full as usize];
                    let rotated = rotate.load(Ordering::Relaxed);
                    let result = Some(decode(&path, max, rotated));
                    let _ = done_tx.send(Decoded {
                        rotated,
                        idx,
                        full,
                        path,
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
        let thumb_view = Arc::new([AtomicUsize::new(0), AtomicUsize::new(0)]);
        let save_thumbs = Arc::new(AtomicBool::new(config.save_thumbnails));
        for _ in 0..2 {
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
                    let Ok((idx, path)) = rx.lock().unwrap().recv() else {
                        return;
                    };
                    let [first, last] = [0, 1].map(|i| view[i].load(Ordering::Relaxed));
                    let result =
                        (idx + THUMB_MARGIN >= first && idx <= last + THUMB_MARGIN).then(|| {
                            let (rotate, save) =
                                (rotate.load(Ordering::Relaxed), save.load(Ordering::Relaxed));
                            thumbs::thumbnail(&path, rotate, save)
                                .ok()
                                .map(|img| color_image(img.into()))
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
        let ctx = cc.egui_ctx.clone();
        std::thread::spawn(move || {
            while let Ok(mut path) = exif_rx.recv() {
                path = exif_rx.try_iter().last().unwrap_or(path);
                let size = std::fs::metadata(&path).map_or(0, |m| m.len());
                let mut rows = vec![("File size", human_size(size))];
                rows.extend(exif::read(&path));
                if exif_tx.send((path, rows)).is_err() {
                    return;
                }
                ctx.request_repaint();
            }
        });
        let (notices_tx, notices) = mpsc::channel();
        let (freed_tx, freed_rx) = mpsc::channel();
        let (changed_tx, changed_rx) = mpsc::channel();
        let mut app = App {
            notices_tx,
            notices,
            notice: None,
            dir: PathBuf::new(),
            subdirs: Vec::new(),
            files: Vec::new(),
            cur: 0,
            current,
            ahead,
            rotate,
            show_settings: false,
            show_help: false,
            confirm: false,
            prompt: None,
            selected: Default::default(),
            last_drawn: None,
            last_scale: 1.0,
            wheel: 0.0,
            config,
            slideshow: None,
            random: false,
            limits,
            jobs,
            done,
            slots: HashMap::new(),
            pending: HashSet::new(),
            errors: HashMap::new(),
            zoom: None,
            pan: egui::Vec2::ZERO,
            started: Some(Instant::now()),
            dialog: None,
            hide_tools: false,
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
            save_thumbs,
            exif_jobs,
            exif_done,
            exif_asked: None,
            exif: None,
            cur_size: None,
            path_text: String::new(),
        };
        app.open(&cc.egui_ctx, path);
        // Small folders are listed before the first frame; large ones hold startup back at most 50 ms.
        app.receive_listing(&cc.egui_ctx, std::time::Duration::from_millis(50));
        app
    }

    /// Open a folder, or the folder of a file with that file shown.
    fn open(&mut self, ctx: &egui::Context, path: PathBuf) {
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
        self.subdirs.clear();
        self.random = false;
        self.thumbs.clear();
        self.thumbs_pending.clear();
        self.dir = dir.clone();
        self.replace_files(ctx, shown.iter().cloned().collect(), shown);
        self.list(ctx);
        self.watch(ctx);
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!(
            "{} — Glimt",
            dir.display()
        )));
    }

    /// Read the folder (and then the sizes) in the background; `receive_listing` merges it in.
    fn list(&mut self, ctx: &egui::Context) {
        let (tx, rx) = mpsc::channel();
        let (size_tx, size_rx) = mpsc::channel();
        let (list_dir, ctx2, videos) = (self.dir.clone(), ctx.clone(), self.config.videos);
        std::thread::spawn(move || {
            // The receiver is gone if another folder was opened meanwhile; then we stop.
            if tx.send(list_folder(&list_dir, videos)).is_err() {
                return;
            }
            ctx2.request_repaint();
            // Sizes afterwards: read the folder once more and sum from the directory entries in
            // chunks, so the status bar fills in gradually without a copy of the whole list in memory.
            let images = std::fs::read_dir(&list_dir)
                .into_iter()
                .flatten()
                .flatten()
                .filter(|e| is_listed(&e.path(), videos));
            let mut sizes = images
                .filter_map(|e| Some(e.metadata().ok()?.len()))
                .peekable();
            while sizes.peek().is_some() {
                if size_tx.send(sizes.by_ref().take(10_000).sum()).is_err() {
                    return;
                }
                ctx2.request_repaint();
            }
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
        self.subdirs = dirs;
        // A refresh during random order keeps the order, so Previous still goes back
        // through the images that were shown.
        let files = if self.random {
            merge_shuffled(&self.files, files)
        } else {
            files
        };
        let shown = self.files.get(self.cur).cloned();
        self.replace_files(ctx, files, shown);
    }

    /// To the trash at once, permanently after confirmation.
    fn request_delete(&mut self, ctx: &egui::Context, trash: bool) {
        if self.files.is_empty() {
            return;
        }
        if !trash && self.config.confirm_delete {
            self.confirm = true;
        } else {
            self.delete_targets_now(ctx, trash);
        }
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
        self.remove_indices(ctx, &targets);
        let (tx, ctx) = (self.notices_tx.clone(), ctx.clone());
        let freed_tx = self.freed_tx.clone();
        std::thread::spawn(move || {
            // Read the size before deleting, so the status bar can subtract it.
            let _ = freed_tx.send(total_size(&paths));
            ctx.request_repaint();
            let mut errors = Vec::new();
            if trash {
                // Many files per gio call, so thousands of files do not become thousands of processes.
                for chunk in paths.chunks(500) {
                    match std::process::Command::new("gio")
                        .arg("trash")
                        .arg("--")
                        .args(chunk)
                        .output()
                    {
                        Ok(o) if o.status.success() => {}
                        Ok(o) => errors.push(String::from_utf8_lossy(&o.stderr).trim().to_owned()),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                            errors.extend(trash_without_gio(chunk));
                        }
                        Err(e) => errors.push(e.to_string()),
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
                ctx.request_repaint();
            }
        });
    }

    /// Remove the (sorted) indices from the list in one pass and shift index-based state
    /// accordingly. Neighbours' textures are kept, so the next image shows without decoding again.
    fn remove_indices(&mut self, ctx: &egui::Context, removed: &[usize]) {
        let mut idx = 0;
        self.files.retain(|_| {
            idx += 1;
            removed.binary_search(&(idx - 1)).is_err()
        });
        self.slots = without_indices(std::mem::take(&mut self.slots), removed);
        self.errors = without_indices(std::mem::take(&mut self.errors), removed);
        self.selected.clear();
        // Jobs in flight have old indices and are rejected by path; refill requests them again.
        self.pending.clear();
        // The next remaining file after the shown one (or the last).
        let cur = self.cur - removed.partition_point(|&r| r < self.cur);
        if self.files.is_empty() {
            self.cur = 0;
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!(
                "{} — Glimt",
                self.dir.display()
            )));
        } else {
            self.go(ctx, cur.min(self.files.len() - 1));
        }
    }

    fn receive_sizes(&mut self) {
        while let Ok(freed) = self.freed_rx.try_recv() {
            // ponytail: deleting while the sum is still being computed can make it slightly off.
            self.total_size = self.total_size.saturating_sub(freed);
        }
        if let Some(Ok(size)) = self.selected_rx.as_ref().map(mpsc::Receiver::try_recv) {
            self.selected_size = Some(size);
            self.selected_rx = None;
        }
        let Some(rx) = &self.size_rx else { return };
        loop {
            match rx.try_recv() {
                Ok(sum) => self.total_size += sum,
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => break,
            }
        }
        self.size_rx = None;
    }

    /// Called when the selection changes: update the title and compute the size in the background.
    fn selection_changed(&mut self, ctx: &egui::Context) {
        self.title(ctx);
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
        let slot = self.slots.remove(&self.cur);
        self.files = files;
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
        self.go(ctx, cur);
    }

    /// Run zenity's (or kdialog's) file dialog in its own thread so the window does not freeze.
    fn pick(&mut self, ctx: &egui::Context, folder: bool, purpose: Pick) {
        use std::os::unix::ffi::OsStringExt;
        let (tx, rx) = mpsc::channel();
        let (ctx, start) = (ctx.clone(), self.dir.join(""));
        let title = match purpose {
            Pick::Open if folder => "Open Folder",
            Pick::Open => "Open File",
            Pick::CopyTo(_) => "Copy to Folder",
            Pick::MoveTo(_) => "Move to Folder",
        };
        let notices = self.notices_tx.clone();
        std::thread::spawn(move || {
            let zenity = || {
                let mut cmd = std::process::Command::new("zenity");
                cmd.arg("--file-selection").arg("--title").arg(title);
                cmd.arg("--filename").arg(&start);
                if folder {
                    cmd.arg("--directory");
                }
                cmd.output()
            };
            // KDE desktops often have kdialog but not zenity.
            let kdialog = || {
                let mode = if folder {
                    "--getexistingdirectory"
                } else {
                    "--getopenfilename"
                };
                let mut cmd = std::process::Command::new("kdialog");
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
        if !keep {
            // Moved out of the shown folder: out of the list at once, as when deleting.
            let names: HashSet<&std::ffi::OsStr> = paths
                .iter()
                .filter(|p| p.parent() == Some(self.dir.as_path()))
                .filter_map(|p| p.file_name())
                .collect();
            let gone: Vec<usize> = (0..self.files.len())
                .filter(|&i| names.contains(self.files[i].as_os_str()))
                .collect();
            if dest != self.dir {
                self.remove_indices(ctx, &gone);
            }
        }
        let (tx, freed_tx, ctx) = (self.notices_tx.clone(), self.freed_tx.clone(), ctx.clone());
        let same_dir = paths.first().and_then(|p| p.parent()) == Some(self.dir.as_path());
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
                if let Err(e) = result {
                    errors.push(format!("{}: {e}", target.display()));
                }
            }
            if let Some(first) = errors.first() {
                let verb = if keep { "Copy" } else { "Move" };
                let _ = tx.send(format!("{verb} failed ({} errors): {first}", errors.len()));
            }
            ctx.request_repaint();
        });
    }

    /// Full path of image `i`.
    fn path(&self, i: usize) -> PathBuf {
        self.dir.join(&self.files[i])
    }

    /// Is `path` image number `i` in the list? (Without building the path.)
    fn is_file(&self, i: usize, path: &Path) -> bool {
        self.files.get(i).is_some_and(|name| {
            path.parent() == Some(self.dir.as_path()) && path.file_name() == Some(name.as_os_str())
        })
    }

    fn request(&mut self, idx: usize, full: bool) {
        let have = self.slots.get(&idx).is_some_and(|s| s.full || !full);
        if !have && !self.errors.contains_key(&idx) && self.pending.insert((idx, full)) {
            let _ = self.jobs.send((idx, full, self.path(idx)));
        }
    }

    fn go(&mut self, ctx: &egui::Context, idx: usize) {
        if self.files.is_empty() {
            return;
        }
        self.cur = idx.min(self.files.len() - 1);
        self.current.store(self.cur, Ordering::Relaxed);
        self.notice = None;
        self.zoom = None;
        self.pan = egui::Vec2::ZERO;
        self.scroll_to_cur = true;
        // Every image gets the full delay, also when the user moves back or forward by hand.
        if self.slideshow.is_some() {
            self.slideshow = Some(Instant::now());
        }
        self.refill();
        self.title(ctx);
    }

    /// Shuffle the file list (current image first) or sort it back, keeping the shown image.
    fn set_random(&mut self, ctx: &egui::Context, on: bool) {
        if self.files.is_empty() {
            return;
        }
        let mut files = std::mem::take(&mut self.files);
        let shown = files[self.cur].clone();
        if on {
            files.swap(0, self.cur);
            shuffle(&mut files[1..]);
        } else {
            files.sort_unstable();
        }
        self.random = on;
        self.replace_files(ctx, files, Some(shown));
    }

    fn start_slideshow(&mut self, ctx: &egui::Context) {
        if self.config.random != self.random {
            self.set_random(ctx, self.config.random);
        }
        self.slideshow = Some(Instant::now());
        self.title(ctx);
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
                let out = std::process::Command::new(cmd)
                    .args(args)
                    .arg(&path)
                    .output();
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

    /// Preferences or a modal dialog is open: keys go there.
    fn dialog_open(&self) -> bool {
        self.show_settings || self.confirm || self.prompt.is_some()
    }

    /// The text dialog was answered with `text`.
    fn answer_prompt(&mut self, ctx: &egui::Context, prompt: Prompt, text: &str) {
        if text.is_empty() || text.contains('/') || text == "." || text == ".." {
            self.notice = Some(format!("Not a valid name: \"{text}\""));
            return;
        }
        let target = self.dir.join(text);
        let result = match prompt {
            Prompt::NewFolder => std::fs::create_dir(&target),
            Prompt::Rename if self.files.get(self.cur) == Some(&OsString::from(text)) => return,
            // rename() would silently replace an existing file.
            Prompt::Rename if target.exists() => Err(std::io::ErrorKind::AlreadyExists.into()),
            Prompt::Rename if self.files.is_empty() => return,
            Prompt::Rename => std::fs::rename(self.path(self.cur), &target),
        };
        match (result, prompt) {
            (Ok(()), Prompt::NewFolder) => self.list(ctx),
            (Ok(()), Prompt::Rename) => self.open(ctx, target),
            (Err(e), _) => self.notice = Some(format!("{}: {e}", target.display())),
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

    /// A random order is kept after stopping, so Previous still goes back through the images
    /// that were shown; reopening the folder sorts it again.
    fn stop_slideshow(&mut self, ctx: &egui::Context) {
        self.slideshow = None;
        self.paused = false;
        self.title(ctx);
    }

    fn title(&self, ctx: &egui::Context) {
        let Some(file) = self.files.get(self.cur) else {
            return;
        };
        let name = file.to_string_lossy();
        let show = match (self.slideshow.is_some(), self.random) {
            (true, _) if self.paused => "⏸ ".to_owned(),
            (true, true) => format!("🔀 {} s · ", self.config.delay),
            (true, false) => format!("▶ {} s · ", self.config.delay),
            _ => String::new(),
        };
        let (n, total) = (self.cur + 1, self.files.len());
        let marked = match self.selected.len() {
            0 => String::new(),
            m => format!(" · {m} selected"),
        };
        let title = format!("{show}{name} ({n}/{total}){marked} — Glimt");
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
    }

    /// Switch image when the time is up. Waits for a preloaded image that is still
    /// decoding, so the slideshow never shows a black screen.
    fn slideshow_tick(&mut self, ctx: &egui::Context) {
        let Some(last) = self.slideshow else { return };
        if self.files.is_empty() || self.paused {
            return;
        }
        let next = (self.cur + 1) % self.files.len();
        let delay = std::time::Duration::from_secs(self.config.delay);
        let left = delay.saturating_sub(last.elapsed());
        if !left.is_zero() {
            ctx.request_repaint_after(left);
        } else if self.random && next == 0 && self.files.len() > 2 {
            // New round: shuffle again instead of repeating the same order.
            self.set_random(ctx, true);
        } else if !self.pending.contains(&(next, false)) {
            self.go(ctx, next);
            self.slideshow = Some(Instant::now());
        }
    }

    /// Drop textures outside the preload window, and full-resolution versions of images other
    /// than the shown one; request the missing ones.
    fn refill(&mut self) {
        if self.files.is_empty() {
            return;
        }
        let (cur, ahead) = (self.cur, self.ahead.load(Ordering::Relaxed));
        self.slots
            .retain(|&i, s| i.abs_diff(cur) <= ahead && (!s.full || i == cur));
        self.request(cur, false);
        for d in 1..=ahead {
            if cur + d < self.files.len() {
                self.request(cur + d, false);
            }
            if let Some(i) = cur.checked_sub(d) {
                self.request(i, false);
            }
        }
    }

    fn receive(&mut self, ctx: &egui::Context) {
        while let Ok(d) = self.done.try_recv() {
            self.pending.remove(&(d.idx, d.full));
            let wanted = self.is_file(d.idx, &d.path)
                && d.rotated == self.config.exif_rotate
                && d.idx.abs_diff(self.cur) <= self.ahead.load(Ordering::Relaxed)
                && (!d.full || d.idx == self.cur);
            match d.result {
                Some(Ok((img, orig))) if wanted => {
                    // A screen-size version must not replace a full-resolution one already loaded.
                    if !d.full && self.slots.get(&d.idx).is_some_and(|s| s.full) {
                        continue;
                    }
                    let tex = ctx.load_texture(
                        format!("img{}", d.idx),
                        img,
                        egui::TextureOptions::LINEAR,
                    );
                    self.slots.insert(
                        d.idx,
                        Slot {
                            tex,
                            path: d.path,
                            full: d.full,
                            orig,
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

    /// Geeqie's toolbar: navigation, zoom, thumbnails, slideshow, preferences and the folder path.
    fn toolbar(&mut self, ui: &mut egui::Ui, act: &mut Option<Action>) {
        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.horizontal(|ui| {
                tool_button(ui, act, "⬅", Action::Back, false);
                tool_button(ui, act, "➡", Action::Forward, false);
                tool_button(ui, act, "⬆", Action::Up, false);
                tool_button(ui, act, "🏠", Action::Home, false);
                tool_button(ui, act, "🔄", Action::Refresh, false);
                ui.separator();
                tool_button(ui, act, "➕", Action::ZoomIn, false);
                tool_button(ui, act, "➖", Action::ZoomOut, false);
                tool_button(ui, act, "1:1", Action::Zoom1To1, false);
                tool_button(ui, act, "⛶", Action::ZoomFit, self.zoom.is_none());
                ui.separator();
                tool_button(ui, act, "🖼", Action::Thumbnails, self.config.thumbnails);
                let show = if self.slideshow.is_some() {
                    "⏹"
                } else {
                    "▶"
                };
                tool_button(ui, act, show, Action::Slideshow, self.slideshow.is_some());
                tool_button(ui, act, "⚙", Action::Preferences, false);
                ui.separator();
                // The folder path; Enter opens what was typed.
                let edit =
                    egui::TextEdit::singleline(&mut self.path_text).desired_width(f32::INFINITY);
                let response = ui.add(edit);
                if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    let path = PathBuf::from(self.path_text.trim());
                    if path.exists() {
                        self.visit(ui.ctx(), path);
                    } else {
                        self.notice = Some(format!("No such folder: {}", path.display()));
                    }
                }
                if !response.has_focus() {
                    self.path_text = self.dir.display().to_string();
                }
            });
        });
    }

    /// File and EXIF details of the shown image, read in the background (Ctrl+K).
    fn exif_panel(&mut self, ui: &mut egui::Ui) {
        let path = self.path(self.cur);
        if self.exif_asked.as_ref() != Some(&path) {
            let _ = self.exif_jobs.send(path.clone());
            self.exif_asked = Some(path.clone());
        }
        for done in self.exif_done.try_iter() {
            self.exif = Some(done);
        }
        egui::Panel::right("exif")
            .resizable(true)
            .default_size(240.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    egui::Grid::new("exif rows").num_columns(2).show(ui, |ui| {
                        ui.weak("File");
                        ui.label(self.files[self.cur].to_string_lossy());
                        ui.end_row();
                        if let Some(s) = self.slots.get(&self.cur) {
                            ui.weak("Dimensions");
                            ui.label(format!("{} × {}", s.orig[0], s.orig[1]));
                            ui.end_row();
                        }
                        // The old rows stay until the new ones arrive, so the panel does not flicker.
                        for (label, value) in self.exif.iter().flat_map(|e| &e.1) {
                            ui.weak(*label);
                            ui.label(value);
                            ui.end_row();
                        }
                    });
                });
            });
    }

    fn receive_thumbs(&mut self, ctx: &egui::Context) {
        while let Ok((path, result)) = self.thumbs_done.try_recv() {
            let (Some(name), true) = (path.file_name(), path.parent() == Some(&self.dir)) else {
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
    fn thumb_row(&mut self, ui: &mut egui::Ui, i: usize, height: f32, marked: bool) -> bool {
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
        let name = &self.files[i];
        let frame = egui::Rect::from_min_size(rect.min + egui::vec2(2.0, 2.0), THUMB);
        match self.thumbs.get(name) {
            Some(Some(tex)) => {
                // Fit the box, but never enlarge.
                let px = tex.size_vec2() / ui.ctx().pixels_per_point();
                let scale = (THUMB.x / px.x).min(THUMB.y / px.y).min(1.0);
                let uv = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
                let at = egui::Rect::from_center_size(frame.center(), px * scale);
                ui.painter().image(tex.id(), at, uv, egui::Color32::WHITE);
            }
            Some(None) => {}
            None => {
                if self.thumbs_pending.insert(name.clone()) {
                    let _ = self.thumb_jobs.send((i, self.path(i)));
                }
            }
        }
        ui.painter().text(
            egui::pos2(frame.right() + 8.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            self.files[i].to_string_lossy(),
            egui::TextStyle::Body.resolve(ui.style()),
            text_color,
        );
        response.clicked()
    }

    /// Drop thumbnails far from view once there are many, so memory stays flat in huge folders.
    fn trim_thumbs(&mut self, first: usize, last: usize) {
        if self.thumbs.len() <= MAX_THUMBS {
            return;
        }
        let keep =
            first.saturating_sub(MAX_THUMBS / 3)..(last + MAX_THUMBS / 3).min(self.files.len());
        let keep: HashSet<&OsString> = self.files[keep].iter().collect();
        self.thumbs.retain(|name, _| keep.contains(name));
    }

    /// The action for a shortcut pressed this frame, if any.
    fn shortcut(ctx: &egui::Context) -> Option<Action> {
        ctx.input_mut(|i| {
            // egui-winit turns Ctrl+C into a Copy event instead of a key press.
            let copies = i.events.len();
            i.events.retain(|e| !matches!(e, egui::Event::Copy));
            if i.events.len() < copies {
                return Some(Action::Copy);
            }
            SHORTCUTS
                .iter()
                .find(|&&(_, mods, key)| {
                    i.consume_shortcut(&egui::KeyboardShortcut::new(mods, key))
                })
                .map(|s| s.0)
        })
    }

    /// Shortcuts, mouse use and the differences from Geeqie (F1).
    fn help_window(&mut self, ctx: &egui::Context) {
        let mut open = self.show_help;
        egui::Window::new("Help")
            .open(&mut open)
            .collapsible(false)
            .default_height(500.0)
            .show(ctx, |ui| {
                ui.heading(format!("Glimt {}", env!("CARGO_PKG_VERSION")));
                ui.label("A fast image viewer inspired by Geeqie.");
                ui.weak("MIT License, © 2026 Hans-Henry Jakobsen");
                egui::ScrollArea::vertical().show(ui, |ui| {
                    egui::CollapsingHeader::new("Keyboard and mouse")
                        .default_open(true)
                        .show(ui, |ui| {
                            // Every shortcut of an action on one row, in menu order.
                            let mut rows: Vec<(Action, Vec<String>)> = Vec::new();
                            for &(action, mods, key) in SHORTCUTS {
                                if action == Action::Play && !cfg!(feature = "video") {
                                    continue;
                                }
                                let text =
                                    ctx.format_shortcut(&egui::KeyboardShortcut::new(mods, key));
                                match rows.iter_mut().find(|r| r.0 == action) {
                                    Some(row) => row.1.push(text),
                                    None => rows.push((action, vec![text])),
                                }
                            }
                            rows.sort_by_key(|r| r.0 as usize);
                            egui::Grid::new("shortcuts").striped(true).show(ui, |ui| {
                                for (action, keys) in rows {
                                    ui.label(keys.join(", "));
                                    ui.label(action.label());
                                    ui.end_row();
                                }
                                for (what, does) in MOUSE {
                                    ui.label(*what);
                                    ui.label(*does);
                                    ui.end_row();
                                }
                            });
                        });
                    egui::CollapsingHeader::new("Differences from Geeqie").show(ui, |ui| {
                        egui::Grid::new("differences").striped(true).show(ui, |ui| {
                            ui.strong("");
                            ui.strong("Glimt");
                            ui.strong("Geeqie");
                            ui.end_row();
                            for (what, glimt, geeqie) in DIFFERENCES {
                                ui.label(*what);
                                ui.label(*glimt);
                                ui.label(*geeqie);
                                ui.end_row();
                            }
                        });
                    });
                });
            });
        self.show_help = open;
    }

    fn zoom_by(&mut self, factor: f32) {
        self.zoom = Some((self.zoom.unwrap_or(self.last_scale) * factor).clamp(0.02, 32.0));
    }

    fn run(&mut self, ctx: &egui::Context, action: Action) {
        let last = self.files.len().saturating_sub(1);
        let fullscreen = ctx.input(|i| i.viewport().fullscreen).unwrap_or(false);
        match action {
            Action::OpenFile => self.pick(ctx, false, Pick::Open),
            Action::OpenFolder => self.pick(ctx, true, Pick::Open),
            Action::Copy | Action::Move if !self.files.is_empty() => {
                let paths = self.targets().iter().map(|&i| self.path(i)).collect();
                let purpose = match action {
                    Action::Copy => Pick::CopyTo(paths),
                    _ => Pick::MoveTo(paths),
                };
                self.pick(ctx, true, purpose);
            }
            Action::Copy | Action::Move => {}
            // ponytail: the default application only; an app chooser (`gio mime`) if wanted.
            Action::OpenWith => self.launch(""),
            // Enter: videos play in a player (images have nothing to play).
            Action::Play =>
            {
                #[cfg(feature = "video")]
                if is_video(&self.location()) {
                    self.launch(&self.config.video_player);
                }
            }
            Action::Delete => self.request_delete(ctx, self.config.trash),
            Action::DeletePermanently => self.request_delete(ctx, false),
            Action::CloseWindow | Action::Quit => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close)
            }
            Action::First => self.go(ctx, 0),
            Action::Prev => self.go(ctx, self.cur.saturating_sub(1)),
            Action::Next => self.go(ctx, (self.cur + 1).min(last)),
            Action::Last => self.go(ctx, last),
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
            Action::SelectAll => {
                self.selected = (0..self.files.len()).collect();
                self.selection_changed(ctx);
            }
            Action::SelectNone => {
                self.selected.clear();
                self.selection_changed(ctx);
            }
            Action::InvertSelection => {
                let n = self.files.len();
                self.selected = (0..n).filter(|i| !self.selected.contains(i)).collect();
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
            Action::Preferences => self.show_settings = true,
            Action::Help => self.show_help = !self.show_help,
            Action::Thumbnails => self.set_thumbnails(!self.config.thumbnails),
            Action::Toolbar => {
                self.config.toolbar = !self.config.toolbar;
                self.config.save();
            }
            Action::ExifPanel => {
                self.config.exif_panel = !self.config.exif_panel;
                self.config.save();
            }
            Action::NewFolder => self.prompt = Some((Prompt::NewFolder, String::new())),
            Action::Rename => {
                if let Some(name) = self.files.get(self.cur) {
                    self.prompt = Some((Prompt::Rename, name.to_string_lossy().into_owned()));
                }
            }
            // A separate process: windows share nothing but the config file.
            Action::NewWindow => {
                let started = std::env::current_exe()
                    .and_then(|exe| std::process::Command::new(exe).arg(self.location()).spawn());
                match started {
                    // Reap the child when it exits, so it does not linger as a zombie.
                    Ok(mut child) => drop(std::thread::spawn(move || child.wait())),
                    Err(e) => self.notice = Some(format!("Could not open a new window: {e}")),
                }
            }
            Action::ExifRotate => self.set_exif_rotate(!self.config.exif_rotate),
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
                self.show_help = false;
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

impl eframe::App for App {
    /// The last folder is written once on exit, not on every folder change.
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // ponytail: the path is stored as UTF-8 (lossless for ordinary paths); switch to OsString bytes if needed.
        if self.config.remember_dir
            && let Some(p) = state_path()
        {
            write_atomic(&p, &format!("last_dir={}\n", self.dir.display()));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = &ui.ctx().clone();
        let ppp = ctx.pixels_per_point();
        let max_side = ctx.input(|i| i.max_texture_side) as u32;
        self.limits.lock().unwrap()[1] = [max_side, max_side];
        self.receive(ctx);
        self.receive_thumbs(ctx);
        self.receive_listing(ctx, std::time::Duration::ZERO);
        self.receive_sizes();
        if self.changed_rx.try_iter().count() > 0 && self.config.auto_refresh {
            self.stale = true;
        }
        if self.stale && self.listing.is_none() {
            self.list(ctx);
        }
        while let Ok(msg) = self.notices.try_recv() {
            // Deleting failed, so the file probably still exists: re-read the folder so the list is right.
            self.open(ctx, self.location());
            self.notice = Some(msg);
        }
        self.slideshow_tick(ctx);
        if let Some(Ok(picked)) = self.dialog.as_ref().map(|(_, rx)| rx.try_recv()) {
            let (purpose, _) = self.dialog.take().unwrap();
            match (purpose, picked) {
                (_, None) => {}
                (Pick::Open, Some(path)) => self.visit(ctx, path),
                (Pick::CopyTo(paths), Some(dest)) => self.transfer(ctx, paths, dest, true),
                (Pick::MoveTo(paths), Some(dest)) => self.transfer(ctx, paths, dest, false),
            }
        }

        // Shortcuts go to the dialogs while one is open.
        // ... and to the text field while one has focus (the toolbar's folder path).
        let typing = ctx.egui_wants_keyboard_input();
        let mut act = (!self.dialog_open() && !typing)
            .then(|| Self::shortcut(ctx))
            .flatten();
        // Full screen and Hide File List as in Geeqie: only the image, no menu or panels.
        let fullscreen = ctx.input(|i| i.viewport().fullscreen).unwrap_or(false);
        let bare = fullscreen || self.hide_tools;
        if !bare {
            egui::Panel::top("menu").show(ui, |ui| {
                egui::MenuBar::new().ui(ui, |ui| {
                    let act = &mut act;
                    ui.menu_button("File", |ui| {
                        menu_item(ui, act, Action::OpenFile);
                        menu_item(ui, act, Action::OpenFolder);
                        menu_item(ui, act, Action::NewFolder);
                        menu_item(ui, act, Action::Copy);
                        menu_item(ui, act, Action::Move);
                        menu_item(ui, act, Action::Rename);
                        menu_item(ui, act, Action::OpenWith);
                        #[cfg(feature = "video")]
                        menu_item(ui, act, Action::Play);
                        ui.separator();
                        menu_item(ui, act, Action::Delete);
                        menu_item(ui, act, Action::DeletePermanently);
                        ui.separator();
                        menu_item(ui, act, Action::CloseWindow);
                        menu_item(ui, act, Action::Quit);
                    });
                    ui.menu_button("Go", |ui| {
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
                    ui.menu_button("Select", |ui| {
                        menu_item(ui, act, Action::SelectAll);
                        menu_item(ui, act, Action::SelectNone);
                        menu_item(ui, act, Action::InvertSelection);
                        ui.separator();
                        menu_item(ui, act, Action::CopyPath);
                        menu_item(ui, act, Action::CopyPathUnquoted);
                    });
                    ui.menu_button("Edit", |ui| {
                        let rotate = egui::Button::selectable(
                            self.config.exif_rotate,
                            "Auto Rotate Using EXIF",
                        );
                        menu_button(ui, act, rotate, Action::ExifRotate);
                        ui.separator();
                        menu_item(ui, act, Action::Preferences);
                    });
                    ui.menu_button("View", |ui| {
                        ui.menu_button("Zoom", |ui| {
                            menu_item(ui, act, Action::ZoomIn);
                            menu_item(ui, act, Action::ZoomOut);
                            menu_item(ui, act, Action::Zoom1To1);
                            menu_item(ui, act, Action::ZoomFit);
                        });
                        menu_item(ui, act, Action::FullScreen);
                        menu_item(ui, act, Action::HideTools);
                        let toolbar =
                            egui::Button::selectable(self.config.toolbar, Action::Toolbar.label());
                        menu_button(ui, act, toolbar, Action::Toolbar);
                        let thumbs = egui::Button::selectable(
                            self.config.thumbnails,
                            Action::Thumbnails.label(),
                        );
                        menu_button(ui, act, thumbs, Action::Thumbnails);
                        let exif = egui::Button::selectable(
                            self.config.exif_panel,
                            Action::ExifPanel.label(),
                        );
                        menu_button(ui, act, exif, Action::ExifPanel);
                        menu_item(ui, act, Action::Refresh);
                        ui.separator();
                        let label = if self.slideshow.is_some() {
                            "Stop Slideshow"
                        } else {
                            "Start Slideshow"
                        };
                        menu_button(ui, act, egui::Button::new(label), Action::Slideshow);
                        let pause = egui::Button::selectable(self.paused, "Pause Slideshow");
                        menu_button(ui, act, pause, Action::Pause);
                        menu_item(ui, act, Action::Faster);
                        menu_item(ui, act, Action::Slower);
                        let random = egui::Button::selectable(self.config.random, "Random Order");
                        menu_button(ui, act, random, Action::RandomOrder);
                    });
                    ui.menu_button("Window", |ui| {
                        menu_item(ui, act, Action::NewWindow);
                        menu_item(ui, act, Action::CloseWindow);
                    });
                    ui.menu_button("Help", |ui| menu_item(ui, act, Action::Help));
                });
            });
            if self.config.toolbar {
                self.toolbar(ui, &mut act);
            }
        }
        if let Some(action) = act {
            self.run(ctx, action);
        }

        let mut open = self.show_settings;
        egui::Window::new("Preferences")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .show(ctx, |ui| {
                let mut n = self.ahead.load(Ordering::Relaxed);
                let slider = egui::Slider::new(&mut n, 0..=20).text("images on each side");
                ui.label("Preload next and previous images:");
                if ui.add(slider).changed() {
                    self.ahead.store(n, Ordering::Relaxed);
                    self.refill();
                    self.config.preload = n;
                    self.config.save();
                }
                let [w, h] = self.limits.lock().unwrap()[0];
                let mb = (2 * n as u64 + 1) * w as u64 * h as u64 * 4 / 1_000_000;
                ui.weak(format!("Up to ≈ {mb} MB at window size {w}×{h}"));
                ui.separator();
                ui.label("Slideshow (S, R = random, Ctrl+= faster, Ctrl+- slower):");
                let delay = egui::Slider::new(&mut self.config.delay, 1..=MAX_DELAY)
                    .text("seconds per image");
                if ui.add(delay).changed() {
                    self.config.save();
                    self.title(ctx);
                }
                let mut random = self.config.random;
                if ui.checkbox(&mut random, "Random order").changed() {
                    self.set_random_order(ctx, random);
                }
                ui.separator();
                let mut rotate = self.config.exif_rotate;
                if ui
                    .checkbox(&mut rotate, "Auto rotate images using EXIF (Alt+X)")
                    .changed()
                {
                    self.set_exif_rotate(rotate);
                }
                let enlarge = egui::Checkbox::new(
                    &mut self.config.enlarge,
                    "Enlarge small images to fit the window",
                );
                if ui.add(enlarge).changed() {
                    self.config.save();
                }
                let wheel = egui::Checkbox::new(
                    &mut self.config.wheel_zoom,
                    "Mouse wheel zooms (off: next/previous image, Ctrl+wheel zooms)",
                );
                if ui.add(wheel).changed() {
                    self.config.save();
                }
                ui.separator();
                let mut show = self.config.thumbnails;
                if ui
                    .checkbox(&mut show, "Show thumbnails in the file list (T)")
                    .changed()
                {
                    self.set_thumbnails(show);
                }
                let save = egui::Checkbox::new(
                    &mut self.config.save_thumbnails,
                    "Save new thumbnails to ~/.cache/thumbnails (shared with other programs)",
                );
                if ui.add(save).changed() {
                    self.save_thumbs
                        .store(self.config.save_thumbnails, Ordering::Relaxed);
                    self.config.save();
                }
                ui.separator();
                let low = egui::Checkbox::new(
                    &mut self.config.low_memory,
                    "Use less memory (decoding up to ~8 % slower; after restart)",
                );
                if ui.add(low).changed() {
                    self.config.save();
                }
                ui.separator();
                let exif = egui::Checkbox::new(
                    &mut self.config.exif_panel,
                    "Show the EXIF panel (Ctrl+K; reads a little from each image shown)",
                );
                if ui.add(exif).changed() {
                    self.config.save();
                }
                ui.separator();
                let auto = egui::Checkbox::new(
                    &mut self.config.auto_refresh,
                    "Refresh the folder when files change",
                );
                if ui.add(auto).changed() {
                    self.config.save();
                }
                let remember = egui::Checkbox::new(
                    &mut self.config.remember_dir,
                    "Start in the last used folder",
                );
                if ui.add(remember).changed() {
                    self.config.save();
                }
                ui.separator();
                ui.label("Deleting:");
                let trash = egui::Checkbox::new(&mut self.config.trash, "Delete moves to trash");
                let confirm = egui::Checkbox::new(
                    &mut self.config.confirm_delete,
                    "Confirm permanent deletion",
                );
                if ui.add(trash).changed() | ui.add(confirm).changed() {
                    self.config.save();
                }
                #[cfg(feature = "video")]
                {
                    ui.separator();
                    ui.label("Video (needs ffmpeg for previews):");
                    let videos = egui::Checkbox::new(
                        &mut self.config.videos,
                        "Show videos in the file list",
                    );
                    if ui.add(videos).changed() {
                        self.config.save();
                        self.list(ctx);
                    }
                    ui.horizontal(|ui| {
                        ui.label("Player (Enter):");
                        let player = egui::TextEdit::singleline(&mut self.config.video_player)
                            .hint_text("default application");
                        if ui.add(player).changed() {
                            self.config.save();
                        }
                    });
                }
            });

        if let Some((prompt, mut text)) = self.prompt.take() {
            let title = match prompt {
                Prompt::NewFolder => "New folder name:",
                Prompt::Rename => "New file name:",
            };
            let mut answer = None;
            let modal = egui::Modal::new(egui::Id::new("prompt")).show(ctx, |ui| {
                ui.label(title);
                let edit = ui.text_edit_singleline(&mut text);
                edit.request_focus();
                let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
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

        if self.confirm {
            let what = match self.targets().as_slice() {
                [i] => format!("\"{}\"", self.files[*i].to_string_lossy()),
                many => format!("{} files", many.len()),
            };
            let mut delete = false;
            let modal = egui::Modal::new(egui::Id::new("confirm-delete")).show(ctx, |ui| {
                ui.label(format!("Permanently delete {what}?"));
                ui.weak("This cannot be undone.");
                ui.horizontal(|ui| {
                    delete = ui.button("Delete (Enter)").clicked()
                        || ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.button("Cancel (Esc)").clicked() {
                        ui.close();
                    }
                });
            });
            if delete {
                self.confirm = false;
                self.delete_targets_now(ctx, false);
            } else if modal.should_close() {
                self.confirm = false;
            }
        }
        self.show_settings = open;
        self.help_window(ctx);

        // Status bar as in Geeqie: image and selection counts on the left, current image on the right.
        if !bare {
            egui::Panel::bottom("status").show(ui, |ui| {
                ui.horizontal(|ui| {
                    let n = self.files.len();
                    let size = if self.size_rx.is_some() {
                        "calculating…".to_owned()
                    } else {
                        human_size(self.total_size)
                    };
                    let mut left = if self.listing.is_some() {
                        "Reading folder…".to_owned()
                    } else {
                        format!("{} images ({size})", thousands(n))
                    };
                    if !self.selected.is_empty() {
                        let m = thousands(self.selected.len());
                        let size = self
                            .selected_size
                            .map_or("calculating…".to_owned(), human_size);
                        left += &format!(", {m} selected ({size})");
                    }
                    ui.label(left);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if n == 0 {
                            return;
                        }
                        // As in Geeqie: "( 4000 x 3000 ) 2,345,678 bytes [3/16]", then the zoom.
                        let path = self.path(self.cur);
                        if self.cur_size.as_ref().is_none_or(|c| c.0 != path) {
                            // ponytail: one stat in the UI thread per image change; move it to
                            // a thread if slow network folders make browsing stutter.
                            let bytes = std::fs::metadata(&path).map_or(0, |m| m.len());
                            self.cur_size = Some((path, bytes));
                        }
                        let bytes = self.cur_size.as_ref().map_or(0, |c| c.1);
                        let place = format!("[{}/{}]", thousands(self.cur + 1), thousands(n));
                        let bytes = format!("{} bytes", thousands(bytes as usize));
                        match self.slots.get(&self.cur) {
                            Some(slot) => {
                                let [w, h] = slot.orig;
                                ui.label(format!("{} %", (self.last_scale * 100.0).round()));
                                ui.separator();
                                ui.label(format!("( {w} x {h} ) {bytes} {place}"));
                            }
                            None => {
                                ui.label(format!("{bytes} {place}"));
                            }
                        }
                    });
                });
            });
        }

        // Left side as in Geeqie: one-level folder tree at the top, file names below.
        let (mut open_dir, mut go_to) = (None, None);
        if !bare {
            egui::Panel::left("left")
                .resizable(true)
                .default_size(240.0)
                .show(ui, |ui| {
                    egui::Panel::top("folders")
                        .resizable(true)
                        .default_size(200.0)
                        .show(ui, |ui| {
                            // Row 0 is ⬆ .. (when there is a parent folder), the rest are subfolders.
                            let up = self.dir.parent().map(Path::to_path_buf);
                            let rows = up.is_some() as usize + self.subdirs.len();
                            let row_h = ui.spacing().interact_size.y;
                            egui::ScrollArea::vertical()
                                .id_salt("folders")
                                .auto_shrink(false)
                                .show_rows(ui, row_h, rows, |ui, range| {
                                    for row in range {
                                        let (label, target) = match (row, &up) {
                                            (0, Some(parent)) => ("⬆ ..".to_owned(), parent),
                                            _ => {
                                                let d = &self.subdirs[row - up.is_some() as usize];
                                                let name = d.file_name().unwrap_or_default();
                                                (format!("📁 {}", name.to_string_lossy()), d)
                                            }
                                        };
                                        if ui.selectable_label(false, label).clicked() {
                                            open_dir = Some(target.clone());
                                        }
                                    }
                                });
                        });
                    // Only visible rows are drawn, so the list is as fast with 10 files as with 300,000.
                    let thumbs = self.config.thumbnails;
                    let row_h = if thumbs {
                        THUMB.y + 4.0
                    } else {
                        ui.spacing().interact_size.y
                    };
                    let pitch = row_h + ui.spacing().item_spacing.y;
                    let mut area = egui::ScrollArea::vertical()
                        .id_salt("files")
                        .auto_shrink(false);
                    if self.scroll_to_cur {
                        // Scroll only when the current image is out of view, and centre it.
                        let (top, view) = (self.cur as f32 * pitch, ui.available_height());
                        let off =
                            egui::scroll_area::State::load(ctx, ui.make_persistent_id("files"))
                                .map_or(0.0, |s| s.offset.y);
                        if top < off || top + pitch > off + view {
                            area = area.vertical_scroll_offset((top - view / 2.0).max(0.0));
                        }
                    }
                    area.show_rows(ui, row_h, self.files.len(), |ui, rows| {
                        if thumbs {
                            let last = rows.end.saturating_sub(1);
                            self.thumb_view[0].store(rows.start, Ordering::Relaxed);
                            self.thumb_view[1].store(last, Ordering::Relaxed);
                            self.trim_thumbs(rows.start, last);
                        }
                        for i in rows {
                            let marked = i == self.cur || self.selected.contains(&i);
                            let clicked = if thumbs {
                                self.thumb_row(ui, i, row_h, marked)
                            } else {
                                let name = self.files[i].to_string_lossy();
                                ui.selectable_label(marked, name).clicked()
                            };
                            if clicked {
                                let m = ui.input(|i| i.modifiers);
                                if m.command {
                                    // Ctrl+click: toggle selection (the shown image is included the first time).
                                    if self.selected.is_empty() {
                                        self.selected.insert(self.cur);
                                    }
                                    if !self.selected.remove(&i) {
                                        self.selected.insert(i);
                                    }
                                    self.selection_changed(ctx);
                                } else if m.shift {
                                    // Shift+click: select the range from the shown image.
                                    self.selected.extend(i.min(self.cur)..=i.max(self.cur));
                                    self.selection_changed(ctx);
                                } else {
                                    self.selected.clear();
                                    self.selected_size = None;
                                    self.selected_rx = None;
                                    go_to = Some(i);
                                }
                            }
                        }
                    });
                });
        }
        self.scroll_to_cur = false;
        if let Some(d) = open_dir {
            self.visit(ctx, d);
        } else if let Some(i) = go_to {
            self.go(ctx, i);
        }

        if !bare && self.config.exif_panel && !self.files.is_empty() {
            self.exif_panel(ui);
        }

        let mut wheel_step = None;
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(egui::Color32::BLACK))
            .show(ui, |ui| {
                let area = ui.max_rect();
                // Decode to the image area, not the whole screen: a small window on a 4K screen
                // needs a fraction of the memory (3840×2160 is 33 MB per image).
                let view = [(area.width() * ppp) as u32, (area.height() * ppp) as u32];
                self.limits.lock().unwrap()[0] = view;
                let response = ui.allocate_rect(area, egui::Sense::click_and_drag());
                let max_fit = if self.config.enlarge {
                    f32::INFINITY
                } else {
                    1.0
                };
                let fit = self.slots.get(&self.cur).map_or(1.0, |s| {
                    (area.width() * ppp / s.orig[0] as f32)
                        .min(area.height() * ppp / s.orig[1] as f32)
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
                            wheel_step = Some(if up { Action::Prev } else { Action::Next });
                            self.wheel = 0.0;
                        }
                    }
                    if factor != 1.0 {
                        self.zoom = Some((self.zoom.unwrap_or(fit) * factor).clamp(0.02, 32.0));
                    }
                }
                if response.dragged() {
                    self.pan += response.drag_delta();
                }
                if response.double_clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fullscreen));
                }
                let scale = self.zoom.unwrap_or(fit);
                self.last_scale = scale;

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
                if let Some(slot) = self.slots.get(&self.cur) {
                    let size = egui::vec2(slot.orig[0] as f32, slot.orig[1] as f32) * scale / ppp;
                    let rect = egui::Rect::from_center_size(area.center() + self.pan, size);
                    egui::Image::new(&slot.tex).paint_at(ui, rect);
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
                #[cfg(feature = "video")]
                if self
                    .files
                    .get(self.cur)
                    .is_some_and(|f| is_video(Path::new(f)))
                {
                    ui.painter().text(
                        area.center_top() + egui::vec2(0.0, 16.0),
                        egui::Align2::CENTER_TOP,
                        "▶ Video: press Enter to play",
                        egui::FontId::proportional(16.0),
                        egui::Color32::LIGHT_GRAY,
                    );
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
        if let Some(action) = wheel_step {
            self.run(ctx, action);
        }
    }
}

fn main() -> eframe::Result {
    // One malloc arena for all threads. With one per thread, freed image buffers stayed in
    // each decode thread's arena: 91 instead of 29 MB of own memory in a folder of
    // screenshots, while decoding is at most a few percent slower.
    if Config::load().low_memory {
        // SAFETY: called before any other thread exists.
        unsafe { mallopt(M_ARENA_MAX, 1) };
    }
    let arg = std::env::args_os().nth(1).map(PathBuf::from);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Glimt")
            .with_inner_size([1280.0, 800.0]),
        ..Default::default()
    };
    eframe::run_native(
        "glimt",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, arg)))),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn lists_folder_and_decodes_to_screen_size() {
        let dir = std::env::temp_dir().join("glimt-test");
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbImage::new(400, 200)
            .save(dir.join("a.png"))
            .unwrap();
        std::fs::write(dir.join("notes.txt"), "").unwrap();
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::create_dir_all(dir.join(".hidden")).unwrap();
        let (dirs, images) = super::list_folder(&dir, true);
        assert_eq!(images, vec![std::ffi::OsString::from("a.png")]);
        assert_eq!(dirs, vec![dir.join("sub")]);
        let (img, orig) = super::decode(&dir.join("a.png"), [100, 100], true).unwrap();
        assert_eq!((img.size, orig), ([100, 50], [400, 200]));
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
    fn trashes_without_gio() {
        let base = std::env::temp_dir().join("glimt-test-trash");
        let _ = std::fs::remove_dir_all(&base);
        let (a, b, trash) = (base.join("a"), base.join("b"), base.join("Trash"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let paths = [a.join("x y.png"), b.join("x y.png")];
        for p in &paths {
            std::fs::write(p, "").unwrap();
        }
        let errors = super::trash_into(&trash, &paths, "2026-10-10T12:00:00");
        assert!(errors.is_empty(), "{errors:?}");
        assert!(!paths[0].exists() && !paths[1].exists());
        assert!(trash.join("files/x y.png").exists() && trash.join("files/x y.png.2").exists());
        let info = std::fs::read_to_string(trash.join("info/x y.png.2.trashinfo")).unwrap();
        let expected = format!(
            "[Trash Info]\nPath={}/b/x%20y.png\nDeletionDate=2026-10-10T12:00:00\n",
            base.display()
        );
        assert_eq!(info, expected);
    }

    #[test]
    fn merge_keeps_shuffled_order() {
        let names = |v: &[&str]| v.iter().map(std::ffi::OsString::from).collect::<Vec<_>>();
        let merged = super::merge_shuffled(&names(&["c", "a", "b"]), names(&["a", "b", "d"]));
        assert_eq!(merged, names(&["a", "b", "d"]));
    }

    #[test]
    fn shuffle_keeps_all_elements() {
        let mut v: Vec<u32> = (0..100).collect();
        super::shuffle(&mut v);
        assert_ne!(v, (0..100).collect::<Vec<_>>());
        v.sort();
        assert_eq!(v, (0..100).collect::<Vec<_>>());
    }

    #[test]
    fn writes_atomically() {
        let dir = std::env::temp_dir().join("glimt-test-atomic");
        let path = dir.join("config");
        super::write_atomic(&path, "a=1\n");
        super::write_atomic(&path, "a=2\n");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a=2\n");
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            1,
            "no .tmp files left"
        );
    }

    #[test]
    fn parses_config() {
        use super::Config;
        assert_eq!(Config::parse("# comment\npreload= 5\n").preload, 5);
        assert_eq!(Config::parse("preload=99").preload, 20);
        assert_eq!(Config::parse("delay=0").delay, 1);
        assert_eq!(Config::parse("delay=7").delay, 7);
        assert!(Config::default().trash && !Config::parse("trash=false").trash);
        assert!(Config::parse("remember_dir=true").remember_dir);
        assert!(!Config::default().random && Config::parse("random=true").random);
        assert!(Config::default().enlarge && !Config::parse("enlarge=false").enlarge);
        assert!(Config::default().wheel_zoom && !Config::parse("wheel_zoom=false").wheel_zoom);
        assert!(Config::default().thumbnails && !Config::parse("thumbnails=false").thumbnails);
        assert!(!Config::default().save_thumbnails);
        assert!(!Config::default().exif_panel && Config::parse("exif_panel=true").exif_panel);
        assert!(Config::default().toolbar && !Config::parse("toolbar=false").toolbar);
        assert!(Config::default().low_memory && !Config::parse("low_memory=false").low_memory);
        assert!(Config::parse("save_thumbnails=true").save_thumbnails);
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
            super::parse_last_dir("last_dir=/tmp/a b\n"),
            Some("/tmp/a b".into())
        );
        assert_eq!(super::parse_last_dir("last_dir=\n"), None);
        assert_eq!(Config::parse("preload=x\nunknown=1"), Config::default());
    }
}
