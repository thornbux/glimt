//! Glimt: a fast, memory-frugal image viewer.
//!
//! Images are decoded in background threads to the size of the image area; full resolution
//! is decoded only when the user zooms past what that covers. Only a small window around
//! the current image is kept in memory.

use crate::dialogs::{Pick, Prompt};
use crate::file_list::{RubberBand, ThumbView};
use crate::file_ops::{Deleted, Undo};
use crate::info_panel::{ExifRead, MetaEdit, spawn_exif_reader};
use crate::view::{DecodeJob, Shown, View, spawn_decoders};
use crate::view::{LAYOUTS, frame_at};
use crate::window::Ender;
use crate::workers::spawn_thumbnailers;
use actions::{Action, OWN_KEYS};
use check::Check;
use colour::{
    COLOR_MANAGEMENT, SCREEN_WAIT, SCREEN_WAIT_MS, VIEW_FILTER, read_desktop_screen, set_colour,
    set_input, set_intent, set_screen,
};
use config::{
    Config, FADES, WindowState, keyword_tree_path, parse_last_dir, sort_folders_from,
    sort_folders_path, state_path, write_atomic,
};
use decode::{Decoded, Hist, MAX_ANIMATION, decode};
use duplicates::Dups;
use eframe::egui;
use external::{Login, Mounted};
use file_ops::RenameJob;
use files::{Types, has_ext};
use format::{count, human_size, local_time, thousands};
use pan::Pan;
use print::PrintDialog;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Instant;
use threads::{
    ONE_ARENA, Receiving, Working, cores, set_cpu_use, small_alloc_threads, thread_cpu_us,
};
#[cfg(feature = "accessibility")]
mod accessibility;

mod actions;

mod check;

mod collections;

mod colour;

mod config;

mod crop;

mod decode;

mod decoders;

mod dialogs;

mod duplicates;

mod exif;

mod external;

mod file_list;

mod file_ops;

mod files;

mod folder;

mod format;

mod help;

mod info_panel;

mod journal;

mod lang;

mod lightroom;

#[cfg(feature = "marks")]
mod marks;

mod menus;

mod meta;

mod pan;

mod plugins;

#[cfg(feature = "portals")]
mod portal;

mod preferences;

mod print;

mod raw;

mod remote;

mod save;

mod save_as;

mod search;

mod selection;

mod slideshow;
#[cfg(test)]
mod source_rules;

mod theme;

mod threads;

mod thumbs;

#[cfg(feature = "video")]
mod video;

mod view;

mod window;

mod workers;

/// The release tests' hook (`test-hooks`): `glimt --remote where LABEL` tells where the menu
/// item, button or choice with that text was drawn last frame, so the tests click by text and
/// need no screen coordinates that every change to the look would move.
#[cfg(feature = "test-hooks")]
mod hooks {
    use eframe::egui;
    use std::sync::Mutex;
    static LABELS: Mutex<Vec<(String, egui::Rect)>> = Mutex::new(Vec::new());
    static LAST: Mutex<Vec<(String, egui::Rect)>> = Mutex::new(Vec::new());

    /// A text drawn this frame, where it was.
    pub fn note(label: &str, rect: egui::Rect) {
        LABELS.lock().unwrap().push((label.to_owned(), rect));
    }

    /// At the start of a frame: last frame's notes are the ones asked about.
    pub fn begin_frame() {
        *LAST.lock().unwrap() = std::mem::take(&mut *LABELS.lock().unwrap());
    }

    /// The answer to `where LABEL`: "x y" of the text's middle, in the window's pixels (the
    /// tests' window is at the top left, unscaled), or nothing when it was not on the screen.
    pub fn position(label: &str) -> Vec<u8> {
        let last = LAST.lock().unwrap();
        match last.iter().rev().find(|(l, _)| *l == label) {
            Some((_, rect)) => {
                format!("{} {}\n", rect.center().x as i32, rect.center().y as i32).into()
            }
            None => Vec::new(),
        }
    }
}

/// Options on the command line before the file or folder, for --help.
const START_OPTIONS: &[(&str, &str)] = &[
    (
        "--slideshow",
        "Start the slideshow; of a list (.gqv, .txt, .m3u), in its order",
    ),
    (
        "--random",
        "Random order in the slideshow (kept, as in the View menu)",
    ),
    ("--fullscreen", "Start in full screen"),
    (
        "--kiosk",
        "Full screen, only the image, nothing changes files; ends by --remote quit",
    ),
    (
        "--lock-settings",
        "No Preferences, and changes to settings last only until Glimt closes",
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
    (
        "--check-config [FILE]",
        "Tell what is wrong in the settings file (or FILE), line by line",
    ),
    (
        "--back-up FOLDER",
        "Copy Glimt's data to a new folder glimt-backup-DATE in FOLDER",
    ),
    ("-V, --version", "Print the version"),
    ("-h, --help", "Print this help"),
];

/// glibc's M_ARENA_MAX.
const M_ARENA_MAX: i32 = -8;

/// Glimt's logo, 256 pixels square: the window's icon, and on the About page.
const LOGO: &[u8] = include_bytes!("../assets/glimt-256.png");

/// What belongs to the image shown, beside its place in the list (`List.cur`): the page of a
/// TIFF, the frame of an animation stepped to, the turn not saved yet and the file it belongs
/// to, the part being cropped. Made anew whole when the image is left (`leave_image`), as
/// `List` is when the list is read again, so nothing of one image stays for the next, also a
/// field added later; the zoom, which Keep Zoom carries over, is not here.
#[derive(Default)]
struct Viewing {
    /// The page of a multi-page TIFF shown (Ctrl+Page Down/Up), from 0.
    page: usize,
    /// The frame of an animation stepped to with Ctrl+Page Up/Down; `None` while it plays.
    frame: Option<usize>,
    /// Orientation of the shown image, for viewing only: quarter turns clockwise, applied
    /// after mirroring when the flag is set. Reset for each image, as in Geeqie.
    orient: (u8, bool),
    /// The file `orient` belongs to, set when it is turned: saved into that file even when the
    /// list changed meanwhile and another file is at `cur`.
    turned: PathBuf,
    /// Edit → Crop: the part of the shown image being chosen.
    crop: Option<crop::Crop>,
}

/// A file operation running in the background (copy, move, delete, keywords, a Lightroom
/// import), for the status bar: how far it is, and a Stop that takes effect between files.
struct Job {
    /// "Copying", "Deleting" …, before "120 of 3,000".
    doing: &'static str,
    total: usize,
    done: AtomicUsize,
    stop: std::sync::atomic::AtomicBool,
    started: Instant,
    /// Every job writes files: closing waits for it, as long as its thread holds the job.
    _writing: Working,
    /// Writes the sidecars of its files: they must not be moved, renamed or deleted meanwhile.
    sidecars: bool,
    /// The files and folders it changes: no other operation starts on them meanwhile.
    touches: Vec<PathBuf>,
}

impl Job {
    /// A job on `total` files, holding the writing count until it is dropped.
    fn new(doing: &'static str, total: usize, sidecars: bool, touches: Vec<PathBuf>) -> Job {
        Job {
            doing,
            total,
            done: AtomicUsize::new(0),
            stop: Default::default(),
            started: Instant::now(),
            _writing: Working::new(&config::WRITING),
            sidecars,
            touches,
        }
    }

    /// Whether Stop was pressed.
    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    fn did(&self, n: usize) {
        self.done.fetch_add(n, Ordering::Relaxed);
    }

    /// How far a stopped one got.
    fn stopped_text(&self) -> String {
        let done = self.done.load(Ordering::Relaxed);
        tf!(
            "Stopped: {done} of {total}",
            done = thousands(done),
            total = count(self.total, "file")
        )
    }

    /// What to tell when it ends: how far a stopped one got; `finished` for one that took long
    /// enough to be lost sight of; nothing for a quick one, which the list already shows.
    fn report(&self, finished: impl FnOnce() -> String) -> Option<String> {
        if self.stopped() {
            Some(self.stopped_text())
        } else if self.started.elapsed() > std::time::Duration::from_secs(2) {
            Some(finished())
        } else {
            None
        }
    }
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
    lock_settings: bool,
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
                "--lock-settings" => start.lock_settings = true,
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
                            "--name takes up to 64 letters, digits, - and _ (not only digits)"
                                .into(),
                        );
                    }
                    if !remote::name_fits(name) {
                        return Err(format!("--name {name} is too long for remote control here"));
                    }
                    start.name = Some(name.to_owned());
                    args.remove(0);
                }
                "--help" | "-h" | "--version" | "-V" | "--remote" | "--remote-all"
                | "--check-config" | "--back-up" => break,
                a if a.starts_with("--") => return Err(format!("unknown option {a}")),
                _ => break,
            }
            args.remove(0);
        }
        Ok((start, args))
    }
}

/// An image decoded for the view: the shown one or a neighbour read ahead, at the size shown or
/// (`full`) at full resolution.
struct Slot {
    tex: egui::TextureHandle,
    /// The file the texture was made from; decides whether it can be kept when the file list
    /// changes.
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
    /// The box it was decoded into (`decode_box`): decoded again only for a larger one.
    decoded_for: [u32; 2],
}

impl Slot {
    /// Whether any pixel of the (first) frame is less than opaque.
    fn transparent(&self) -> bool {
        self.hist[4][..255].iter().any(|&n| n > 0)
    }

    /// The frame to show `t` after the image was opened; asks for a repaint when the next is due.
    fn frame(&self, t: std::time::Duration, ctx: &egui::Context) -> &egui::TextureHandle {
        let (k, left) = self.frame_at(t);
        if let Some(left) = left {
            ctx.request_repaint_after(std::time::Duration::from_millis(left as u64));
        }
        self.anim.get(k).map_or(&self.tex, |f| &f.0)
    }

    /// The frame shown `t` after the start, and the milliseconds it still has.
    fn frame_at(&self, t: std::time::Duration) -> (usize, Option<u32>) {
        let total: u32 = self.anim.iter().map(|f| f.1).sum();
        let mut at = (t.as_millis() % total.max(1) as u128) as u32;
        for (k, (_, ms)) in self.anim.iter().enumerate() {
            if at < *ms {
                return (k, Some(ms - at));
            }
            at -= ms;
        }
        (0, None)
    }
}

/// The file list and everything that holds a place in it: made anew when the list is another
/// (`replace_files`), so nothing indexed into one list can outlive it, whatever field comes
/// next. What follows a file from one list to the next (the shown image, the selection, the
/// panes, the decoded slots) is carried over by name there, in the one place.
#[derive(Default)]
struct List {
    /// The images in `dir`, file name only (the full path is built on demand by `path`): saves
    /// memory in large folders, where the path would otherwise be repeated for every file.
    files: Vec<OsString>,
    cur: usize,
    /// Selected files (Ctrl/Shift+click in the file list) that Delete acts on.
    selected: std::collections::BTreeSet<usize>,
    /// Where a Shift selection started (file index).
    anchor: Option<usize>,
    /// Where the Shift selection made by the keys last ended (file index), while nothing else
    /// changed the selection or the search since: the selection is then that range.
    range_end: Option<usize>,
    /// The images of a layout's panes after the first, which shows `cur`.
    panes: Vec<usize>,
    slots: HashMap<usize, Slot>,
    decode_errors: HashMap<usize, String>,
    /// Decodes asked for, with the number of the job: only its result is taken, so one asked
    /// for before the list, the page, the look or the file changed is dropped; and the box it
    /// was asked for in.
    decodes_pending: HashMap<(usize, bool), (u64, PathBuf, [u32; 2])>,
    matches: Option<Vec<usize>>,
    /// Total size of the search matches, summed in the background like `total_size`.
    match_size: u64,
    /// A rectangle being dragged in the icon grid to select.
    rubber_band: Option<RubberBand>,
    /// Images of a collection being dragged in the file list, and the place they would go.
    dragging: Option<(HashSet<usize>, Option<usize>)>,
    last_slide: usize,
    /// The files whose cells pan view draws this frame: their thumbnails are made and kept,
    /// also while the file list shows others.
    pan_thumbs: Vec<usize>,
    /// File list size and date columns: (bytes, mtime) by file name.
    details: HashMap<OsString, (u64, i64)>,
    /// Goes up whenever the file list changes, so what was worked out from its indices can tell
    /// that it is old, also when the list kept its length (another order).
    version: usize,
}

struct App {
    /// The window's context, for the one caller that gets none: on_exit.
    ctx: egui::Context,
    /// The file list with everything indexed into it.
    list: List,
    // ---- Settings ----
    /// The desktop's dark style and accent colour, once read from the portal.
    desktop_look: theme::Desktop,
    desktop_rx: Receiving<theme::Desktop>,
    /// The desktop's screen profile file, once read from colord.
    desktop_screen: Option<String>,
    desktop_screen_rx: Receiving<Option<String>>,
    /// The action whose key is being chosen in Preferences: the next key press goes to it.
    capturing: Option<Action>,
    config: Config,

    // ---- The list: the folder (or collection, or several folders) and its files ----
    /// A network location (smb:// …) being mounted: its local path, or why it failed.
    mounting_rx: Receiving<(Mounted, Login)>,
    /// The login dialog for a network share that needs one.
    login: Option<Login>,
    /// The folder path in the toolbar, as typed.
    path_text: String,
    dir: PathBuf,
    subdirs: Vec<PathBuf>,
    /// The type of the file opened directly (not the folder), listed even when the Types menu
    /// leaves it out.
    opened_type: Types,
    /// The list holds the images of the subfolders too (slideshow with subfolders).
    recursive: bool,
    /// The open collection's file. The list is then its images, with `dir` "/" and the paths
    /// without their leading "/" as names, so `path()` works unchanged.
    collection: Option<PathBuf>,
    /// The collection's next listing is sorted (Sort menu), and saved in that order.
    sort_collection: bool,
    /// Several folders shown as one list (glimt DIR DIR …, File → Open Folders…): the names
    /// are relative to /, as in a collection, and the folders are read again on refresh.
    folders: Vec<PathBuf>,
    /// Folders to show together, chosen in a folder's menu; opened after the frame.
    together: Option<Vec<PathBuf>>,
    /// Folder listing in the background: (subfolders, images).
    listing_rx: Receiving<(Vec<PathBuf>, Vec<OsString>)>,
    /// The subfolders of a folder listed with its tree, sent at once: the tree may take long.
    subdirs_rx: Receiving<Vec<PathBuf>>,
    /// The images of the tree found so far, as it is read, a folder of it at a time.
    part_rx: Receiving<Vec<OsString>>,
    /// The next listing shows a tree's images as they are found (a folder opened).
    list_parts: bool,
    /// What the list shown was read and sorted as: folder, order, ascending, case, subfolders.
    listed_as: Option<(PathBuf, files::Sort, bool, bool, bool)>,
    /// The image the first part of a tree went to by itself: the whole list goes to its first.
    went_to: Option<OsString>,
    /// Set to stop the listing of the tree before, when another one starts.
    listing_stop: Arc<AtomicBool>,
    /// The folder changed while a listing was running; list again when it is done.
    relist: bool,
    /// Generation of the folder watcher; opening another folder ends the old thread.
    watching: Arc<AtomicUsize>,
    /// The watcher saw the folder change.
    changed_tx: mpsc::Sender<()>,
    changed_rx: mpsc::Receiver<()>,
    /// Names being deleted in the background; kept out of refreshed listings until done.
    deleting: HashSet<OsString>,
    /// What Edit → Undo puts back: the last move to the trash, move or rename.
    undo: Option<Undo>,
    /// File → Open Recent: the folders and collections opened last, newest first.
    recent: Vec<PathBuf>,
    /// Go → Back / Forward: earlier locations (file shown, or folder if it had no images).
    back: Vec<PathBuf>,
    forward: Vec<PathBuf>,
    /// Scroll the file list to the current image on the next frame.
    scroll_to_cur: bool,
    /// Subfolders by folder for the folder tree; `None` while being read.
    tree: HashMap<PathBuf, Option<Vec<PathBuf>>>,
    tree_tx: mpsc::Sender<(PathBuf, Vec<PathBuf>)>,
    tree_rx: mpsc::Receiver<(PathBuf, Vec<PathBuf>)>,
    /// The folder the tree was last opened up to; it opens again when the folder changes.
    tree_shown: PathBuf,
    /// The folders open in the tree.
    tree_open: HashSet<PathBuf>,

    // ---- The selection ----
    /// Size of the selected files; computed in the background when the selection changes.
    selected_size: Option<u64>,
    selected_rx: Receiving<u64>,
    /// When to work out that size: once the selection has stayed the same a moment.
    selected_size_due: Option<Instant>,

    // ---- Decoding: the shown image and its neighbours ----
    /// The images to keep decoded, the shown one first: its neighbours in list order.
    /// Shared with the workers, which skip jobs outside it.
    keep: Arc<Mutex<Vec<usize>>>,
    /// How many images at the start of `keep` may be decoded at full resolution: the shown one,
    /// and the compared one beside it.
    full_ok: Arc<AtomicUsize>,
    /// Number of images preloaded on each side (Preferences).
    preload: Arc<AtomicUsize>,
    /// Workers apply EXIF orientation (Preferences, Alt+X).
    rotate: Arc<AtomicBool>,
    /// The view's box for the shown image, unturned, as `decode_size` works it out each frame
    /// (per fit mode; in Fill from the image's own shape), and the view's size: what the decoders
    /// shrink into, through `decode_box` for each job.
    view_box: [u32; 2],
    view_area: [u32; 2],
    /// The longest side a texture may have.
    max_side: u32,
    /// To the decode workers.
    decode_jobs: mpsc::Sender<DecodeJob>,
    decoded_rx: mpsc::Receiver<Decoded>,
    decodes_sent: u64,
    /// Last drawn image; kept on screen until the next one is decoded, so browsing never shows a
    /// black screen.
    last_drawn: Option<(egui::TextureHandle, [usize; 2])>,
    /// A slideshow's change of image fading in: when it began, and what each pane (the first,
    /// then the others) showed before, kept until it has faded out.
    fade: Option<(Instant, Vec<Option<Shown>>)>,
    /// When the shown image was opened: the clock of an animation.
    opened: Instant,
    /// Set at startup; taken when the first image is shown, for the "first image: N ms" line on
    /// standard error that the release tests read.
    started: Option<Instant>,
    /// How many frames after the first image took each number of milliseconds of the window
    /// thread's processor time (the last: that
    /// many or more), for the "frames:" line at exit that the release
    /// tests read: work that grows with the list makes every frame slow, while the slowest alone
    /// varies with one-off work such as Select All.
    frame_ms: [u32; 256],

    // ---- The view: zoom, panes, the window ----
    /// The image area, which keeps the keyboard focus for a screen reader: not a widget that
    /// takes the keys from the shortcuts.
    image_id: Option<egui::Id>,
    /// The focus last moved by Tab, not by the mouse: its ring shows, as in GNOME.
    focus_by_keys: bool,
    /// Physical screen pixels per original pixel; `None` = fit to window.
    zoom: Option<f32>,
    pan_offset: egui::Vec2,
    /// How far the shown image can be panned each way (points); zero where it fits.
    max_pan: egui::Vec2,
    /// Scale from the previous frame (physical pixels per image pixel), for the zoom percentage in
    /// the status bar.
    last_scale: f32,
    /// Mouse wheel notches not yet turned into an image step (wheel_zoom off).
    wheel: f32,
    /// What belongs to the image shown, made anew when it is left.
    viewing: Viewing,
    /// Two images side by side (E): the shown one and `compare_other`.
    compare: bool,
    /// Columns of the icon grid in the last frame; 1 when the list is rows or hidden.
    grid_cols: usize,
    /// How far Previous/Next move this frame: a row of the icon grid for ↑ and ↓, else 1.
    row_step: isize,
    /// View → Hide File List: only the image, as in full screen but in a window.
    hide_file_list: bool,
    /// Window geometry seen in the last frame, saved at exit.
    window_state: WindowState,
    /// The screen saver inhibitor running while in full screen, and how many times full screen
    /// was turned on or off: the inhibitor is looked for and started in a thread, and one that
    /// comes after full screen ended again is ended at once.
    inhibitor: Arc<Mutex<(u64, Option<Ender>)>>,
    /// Whether it was asked for: tried once, not on every frame where none could be started.
    inhibit_on: bool,
    /// Since when the pointer has been away from an open menu of the menu bar.
    menu_away: Option<Instant>,
    /// Window → Projector: a second window with only the image, following this one.
    projector: bool,
    /// The projector's image area in pixels, which images are decoded to cover too.
    projector_size: [u32; 2],
    /// What the image area showed in the last frame, for the projector to show the same.
    last_view: Option<View>,
    /// --kiosk: full screen, only the image, only actions that show and browse.
    kiosk: bool,
    /// `--remote quit` came: a kiosk may close.
    kiosk_quit: bool,
    /// The window had the focus last frame; getting it makes this Glimt the one `--remote`
    /// talks to.
    has_focus: bool,

    // ---- The slideshow ----
    /// The slideshow: whether it runs, its order and the pane it changes next.
    slideshow: slideshow::Slideshow,

    // ---- Thumbnails ----
    /// Thumbnails for the file list: those made, those being made, and the workers making them.
    thumbs: file_list::Thumbs,

    // ---- EXIF, keywords, comment and marks ----
    /// To the EXIF reader thread, and its answers.
    exif_jobs: mpsc::Sender<PathBuf>,
    exif_rx: mpsc::Receiver<ExifRead>,
    /// The file the EXIF panel asked for last, and the panel's rows as read for a file.
    exif_asked: Option<PathBuf>,
    exif: Option<(PathBuf, Vec<(&'static str, String)>)>,
    /// Every EXIF field of the shown image, for the EXIF window (Ctrl+E).
    exif_all: Vec<(&'static str, String, String)>,
    /// View → EXIF Window is open.
    exif_window: bool,
    /// Keywords, comment, alt text and extended description in the EXIF panel; saved when a field
    /// loses the focus, or another image is shown.
    meta: Option<MetaEdit>,
    /// Alt+K: put the focus in the keywords field when the panel shows next.
    focus_keywords: bool,
    /// The keyword tree (Geeqie's), as paths "Places|Norway|Oslo", sorted; in the EXIF panel.
    keyword_tree: Vec<String>,
    /// A path being typed to add to the keyword tree.
    new_keyword: String,
    /// EXIF text of each file in the folder, lowercase, for "exif:" searches; built on the
    /// first one, dropped with the folder.
    exif_index: Option<HashMap<OsString, String>>,
    /// Files whose keywords or comment changed since they were indexed: read again, alone,
    /// at the next "exif:" search.
    exif_stale: HashSet<OsString>,
    exif_index_rx: Receiving<HashMap<OsString, String>>,
    /// Set when the index being read is no longer wanted (another folder): the thread stops.
    exif_index_stop: Arc<AtomicBool>,
    #[cfg(feature = "marks")]
    marks: marks::Marks,

    // ---- The search ----
    /// The search field takes the focus when it is next drawn (/).
    focus_search: bool,
    /// File name search above the file list, and the matching files best first while it has
    /// at least three letters.
    search: String,
    /// The rows among the matches of the shown file and of the one a slideshow of several panes
    /// steps from, as (file, row): found once per image and search, not in every frame, as the
    /// matches can be 300,000 long.
    match_rows: std::cell::Cell<[Option<FileRow>; 2]>,
    /// Why a re: search pattern is not one, shown under the search field.
    search_error: Option<String>,
    match_rx: Receiving<u64>,
    /// When to sum the matches' size: once the search has stayed the same a moment.
    match_size_due: Option<Instant>,

    // ---- The status bar ----
    /// Size in bytes of the shown file, for the status bar.
    cur_size: Option<(PathBuf, u64)>,
    /// Image and scale in each pane of the last frame, the pane the status bar tells about (the
    /// one under the pointer, else the first) and the panes as [columns, rows].
    pane_info: Vec<(usize, Option<f32>)>,
    status_pane: usize,
    status_grid: [usize; 2],
    /// The place "[n/m]" of an image in another pane than the first, by image and list length.
    status_place: Option<(usize, usize, String)>,
    /// Total size of the images in the folder, summed in the background after listing.
    /// Only sums are sent (no per-file table), so memory use does not grow with the folder.
    total_size: u64,
    /// Partial sums from the background; `None` once all have arrived.
    size_rx: Receiving<u64>,
    /// Bytes freed by deletions in the background.
    freed_tx: mpsc::Sender<u64>,
    freed_rx: mpsc::Receiver<u64>,

    // ---- File operations in the background ----
    /// A Rename Many running in the background.
    renaming: Option<RenameJob>,
    renaming_rx: Receiving<Result<(), String>>,
    /// Renames found cut short at the start (a crash or a power cut during Rename Many), to put
    /// right; read in the background.
    cut_short_rx: Receiving<Vec<journal::CutShort>>,
    cut_short: Vec<journal::CutShort>,
    /// Work still running in the background, for the busy pointer: deletes, copies and moves,
    /// printing, Check Images, Rename Many, the Lightroom import, plugins and every `background`.
    busy: Arc<AtomicUsize>,
    /// When the mouse last moved, or full screen began: the pointer hides after `POINTER_STILL`.
    pointer_moved: std::time::Instant,
    /// File operations running, for the status bar; one is over when its thread let go of it.
    file_jobs: Vec<Arc<Job>>,
    /// What `background` work changes, held by its thread as long as it runs.
    touching: Vec<Arc<Vec<PathBuf>>>,
    /// Close asked for while files were being written (`work_left`): 1, it waits for them; 2,
    /// they are stopped after the file at hand and it waits for that; a third close ends at once.
    closing: u8,
    /// What deletes, moves and copies report (`Deleted`). A plugin and Move Folder to Trash
    /// send it too, only for the list to be read again.
    deleted_tx: mpsc::Sender<Deleted>,
    deleted_rx: mpsc::Receiver<Deleted>,
    /// Those still being saved.
    saving: HashSet<PathBuf>,
    /// PNG and WebP files saved turned in the background; a plugin sends its files too, as it may
    /// have changed them, so they are decoded again.
    saved_tx: mpsc::Sender<(PathBuf, Result<(), String>)>,
    saved_rx: mpsc::Receiver<(PathBuf, Result<(), String>)>,
    /// The files Rename or a delete asked about, by their names in the list, and where the list
    /// was (`ask_about_targets`): the answer acts on those files, wherever a refresh put them.
    asked_for: (u64, Vec<OsString>),
    /// The sort manager's folders, kept in `$XDG_CONFIG_HOME/glimt/sort-folders`.
    sort_folders: Vec<PathBuf>,
    /// Our own clipboard for file lists, kept so it can serve them (X11).
    clipboard: Option<arboard::Clipboard>,
    /// An image decoded for the clipboard; egui takes it only during a frame.
    clip_rx: Receiving<egui::ColorImage>,

    // ---- Dialogs, windows and messages ----
    /// "Open With" applications by file extension, found once per extension.
    /// `None` while `apps_for` runs in the background.
    apps: HashMap<String, Option<Vec<(String, PathBuf)>>>,
    apps_tx: mpsc::Sender<(String, Vec<(String, PathBuf)>)>,
    apps_rx: mpsc::Receiver<(String, Vec<(String, PathBuf)>)>,
    /// Edit → Plugins, read at start.
    plugins: Vec<plugins::Plugin>,
    /// The plugins running, for Stop in their menu.
    plugins_running: plugins::Running,
    /// Edit → Preferences is open.
    preferences_open: bool,
    /// Preferences changed while the pointer is down: saved when it is let go.
    preferences_unsaved: bool,
    /// The search field of Preferences: only the sections with these words show.
    preferences_query: String,
    /// Preferences was just opened: its search field takes the keys.
    focus_preferences_search: bool,
    /// The help page open (F1), if any.
    help: Option<help::Page>,
    /// Open text dialog and the text typed so far.
    prompt: Option<(Prompt, String)>,
    /// Window → Set Window Title: this window's title template; empty, the one in Preferences.
    window_title: String,
    /// The delete confirmation is open: `Some(true)` = move to trash, `Some(false)` = permanently.
    confirm_delete: Option<bool>,
    /// The folder a folder's context menu acts on: to rename, or, with `confirm_folder`, to trash.
    folder_target: Option<PathBuf>,
    confirm_folder: bool,
    print_window: Option<PrintDialog>,
    /// A Lightroom catalog being read, then imported with the folders where its root folders
    /// are now.
    lightroom_rx: Receiving<Result<lightroom::Catalog, String>>,
    lightroom: Option<lightroom::Import>,
    /// Answer from an open file dialog (zenity or kdialog).
    file_dialog: Option<Pick>,
    file_dialog_rx: Receiving<Option<PathBuf>>,
    /// The pan view's window (Ctrl+J), while it is open.
    pan_window: Option<Pan>,
    /// File → Save As: the dialog while open, and what it asked last.
    save_as: Option<save_as::SaveAs>,
    save_as_last: save_as::Choice,
    /// The Find Duplicates window, while open.
    dups: Option<Dups>,
    /// The Check Images window: `None` closed, `Some(None)` open and not started.
    check: Option<Option<Check>>,
    /// Files Check Images found broken, marked in the list until another list is shown.
    broken_files: HashSet<OsString>,
    /// The message shown over the image, until the next image.
    notice: Option<String>,
    /// Messages from background tasks (deleting, copying), to become the notice.
    notices_tx: mpsc::Sender<String>,
    notices_rx: mpsc::Receiver<String>,
    /// Commands from `glimt --remote`.
    remote_rx: mpsc::Receiver<(String, Option<remote::Reply>)>,
    /// A `--remote select` that came while the folder was being read or Delete or Rename asked,
    /// and every command after it: done in the order they came once neither is.
    waiting_select: Vec<(String, Option<remote::Reply>)>,
}

/// A file of the list and its row among the search matches (`None`: not among them).
type FileRow = (usize, Option<usize>);

impl App {
    // ---- Starting up ----

    fn new(cc: &eframe::CreationContext, path: Option<PathBuf>, start: Start) -> Self {
        let (config, problems) = Config::load_checked();
        Self::with(&cc.egui_ctx, config, &problems, path, start)
    }

    /// The window's state from the settings `new` reads, or a test gives, so a test makes an
    /// `App` on `egui::Context::default()` with settings of its own.
    fn with(
        ctx: &egui::Context,
        mut config: Config,
        problems: &[String],
        path: Option<PathBuf>,
        start: Start,
    ) -> Self {
        let keep = Arc::new(Mutex::new(vec![0]));
        let full_ok = Arc::new(AtomicUsize::new(1));
        lang::set(&config.language);
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
        VIEW_FILTER.store(config.view_filter, Ordering::Relaxed);
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
        let path = path
            .or_else(|| {
                let state = crate::decode::read_text(state_path()?).ok()?;
                parse_last_dir(&state).filter(|d| {
                    config.remember_dir && (d.is_dir() || collections::is_collection(d))
                })
            })
            .unwrap_or_else(|| ".".into());
        // No list kept: one of before goes too, also when the setting was changed in the file.
        let recent = match config::recent_path() {
            Some(file) if !config.keep_recent => {
                config::write_later(move || config::remove_shared(&file));
                Vec::new()
            }
            Some(file) => crate::decode::read_limited(file)
                .map_or_else(|_| Vec::new(), |t| config::parse_recent(&t)),
            None => Vec::new(),
        };
        let preload = Arc::new(AtomicUsize::new(config.preload));
        let rotate = Arc::new(AtomicBool::new(config.exif_rotate));
        MAX_ANIMATION.store(config.animation_mb << 20, Ordering::Relaxed);
        set_cpu_use(config.cpu_use);
        let workers = cores().min(config.decode_threads);
        let (jobs, done) = spawn_decoders(ctx, workers, &keep, &full_ok, &rotate);
        // Thumbnails have their own workers, so scrolling the list never delays the shown image.
        let thumb_view: ThumbView = Arc::default();
        let save_thumbs = Arc::new(AtomicBool::new(config.save_thumbnails));
        let (thumb_jobs, thumbs_rx) =
            spawn_thumbnailers(ctx, workers, &rotate, &thumb_view, &save_thumbs);
        let (exif_jobs, exif_rx) = spawn_exif_reader(ctx);
        theme::fonts(ctx);
        theme::apply(ctx, &theme::load(&config.theme), None, None);
        // The size comes from Preferences and the desktop; egui's own Ctrl+0 would undo it.
        ctx.options_mut(|o| o.zoom_with_keyboard = false);
        ctx.set_zoom_factor(config.ui_scale as f32 / 100.0);
        // gdbus takes a moment; the window shows meanwhile and takes the desktop's look after.
        let (desktop_tx, desktop_rx) = mpsc::channel();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            let _ = desktop_tx.send(theme::desktop());
            repaint.request_repaint();
        });
        let (notices_tx, notices) = mpsc::channel();
        let (tree_tx, tree_rx) = mpsc::channel();
        let (apps_tx, apps_rx) = mpsc::channel();
        let (remote_tx, remote_rx) = mpsc::channel();
        // A name is only of use with remote control, so it listens anyway; a kiosk too, as
        // --remote quit is its way out.
        if config.remote_control || remote::given_name().is_some() || start.kiosk {
            remote::listen(remote_tx, ctx.clone());
        }
        let (freed_tx, freed_rx) = mpsc::channel();
        let (deleted_tx, deleted_rx) = mpsc::channel();
        let (saved_tx, saved_rx) = mpsc::channel();
        let (changed_tx, changed_rx) = mpsc::channel();
        let mut app = App {
            notices_tx,
            mounting_rx: Receiving::none(),
            login: None,
            print_window: None,
            lightroom_rx: Receiving::none(),
            lightroom: None,
            cut_short_rx: Receiving::of({
                let (tx, rx) = mpsc::channel();
                std::thread::spawn(move || {
                    let _ =
                        tx.send(journal::dir().map_or_else(Vec::new, |d| journal::interrupted(&d)));
                });
                rx
            }),
            cut_short: Vec::new(),
            renaming: None,
            renaming_rx: Receiving::none(),
            keyword_tree: (keyword_tree_path().and_then(|p| crate::decode::read_text(p).ok()))
                .map_or_else(Vec::new, |t| meta::keyword_paths(t.lines())),
            new_keyword: String::new(),
            sort_folders: (sort_folders_path().and_then(|p| crate::decode::read_limited(p).ok()))
                .map_or_else(Vec::new, |t| sort_folders_from(&t)),
            pan_window: None,
            viewing: Viewing::default(),
            save_as: None,
            save_as_last: Default::default(),
            asked_for: (0, Vec::new()),
            grid_cols: 1,
            compare: false,
            capturing: None,
            recursive: false,
            collection: None,
            folders: Vec::new(),
            together: None,
            sort_collection: false,
            dups: None,
            check: None,
            broken_files: HashSet::new(),
            #[cfg(feature = "marks")]
            marks: marks::Marks::load(),
            tree: HashMap::new(),
            tree_tx,
            tree_rx,
            tree_shown: PathBuf::new(),
            tree_open: HashSet::new(),
            clip_rx: Receiving::none(),
            clipboard: None,
            remote_rx,
            ctx: ctx.clone(),
            list: List::default(),
            has_focus: true,
            apps: HashMap::new(),
            apps_tx,
            desktop_look: Default::default(),
            desktop_rx: Receiving::of(desktop_rx),
            desktop_screen: None,
            desktop_screen_rx: (config.desktop_profile)
                .then(|| read_desktop_screen(ctx))
                .map_or_else(Receiving::none, Receiving::of),
            plugins: plugins::find(),
            plugins_running: plugins::Running::default(),
            apps_rx,
            row_step: 1,
            notices_rx: notices,
            notice: screen_error.or_else(|| config_notice(problems)),
            dir: PathBuf::new(),
            subdirs: Vec::new(),
            keep,
            full_ok,
            preload,
            rotate,
            preferences_open: false,
            preferences_unsaved: false,
            preferences_query: String::new(),
            focus_preferences_search: false,
            help: None,
            confirm_delete: None,
            folder_target: None,
            confirm_folder: false,
            opened_type: Types::NONE,
            prompt: None,
            window_title: String::new(),
            last_drawn: None,
            fade: None,
            last_scale: 1.0,
            wheel: 0.0,
            config,
            view_box: [1920, 1080],
            view_area: [1920, 1080],
            max_side: 8192,
            decode_jobs: jobs,
            decoded_rx: done,
            decodes_sent: 0,
            zoom: None,
            pan_offset: egui::Vec2::ZERO,
            started: Some(Instant::now()),
            frame_ms: [0; 256],
            file_dialog: None,
            file_dialog_rx: Receiving::none(),
            hide_file_list: false,
            focus_search: false,
            kiosk: start.kiosk,
            menu_away: None,
            projector: false,
            projector_size: [0, 0],
            last_view: None,
            kiosk_quit: false,
            watching: Arc::new(AtomicUsize::new(0)),
            changed_tx,
            changed_rx,
            relist: false,
            back: Vec::new(),
            forward: Vec::new(),
            subdirs_rx: Receiving::none(),
            part_rx: Receiving::none(),
            list_parts: false,
            listed_as: None,
            went_to: None,
            listing_stop: Arc::default(),
            listing_rx: Receiving::none(),
            image_id: None,
            focus_by_keys: false,
            waiting_select: Vec::new(),
            total_size: 0,
            size_rx: Receiving::none(),
            freed_tx,
            freed_rx,
            selected_size: None,
            selected_rx: Receiving::none(),
            selected_size_due: None,
            scroll_to_cur: true,
            exif_jobs,
            exif_rx,
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
            window_state: WindowState::default(),
            inhibitor: Arc::default(),
            inhibit_on: false,
            deleting: HashSet::new(),
            undo: None,
            recent,
            deleted_tx,
            deleted_rx,
            saved_tx,
            saved_rx,
            saving: HashSet::new(),
            busy: Arc::new(AtomicUsize::new(0)),
            pointer_moved: std::time::Instant::now(),
            file_jobs: Vec::new(),
            touching: Vec::new(),
            closing: 0,
            search: String::new(),
            match_rows: Default::default(),
            search_error: None,
            match_rx: Receiving::none(),
            match_size_due: None,
            exif_index: None,
            exif_stale: HashSet::new(),
            exif_index_rx: Receiving::none(),
            exif_index_stop: Arc::default(),
            opened: Instant::now(),
            max_pan: egui::Vec2::ZERO,
            slideshow: slideshow::Slideshow {
                shown_at: None,
                shuffled: false,
                whole_shuffle: false,
                paused: false,
                at_start: start.slideshow,
                next_pane: 0,
            },
            thumbs: file_list::Thumbs {
                stale: HashSet::new(),
                textures: HashMap::new(),
                pending: HashSet::new(),
                jobs: thumb_jobs,
                rx: thumbs_rx,
                view: thumb_view,
                list_rows: 0..0,
                save: save_thumbs,
            },
        };
        // A URI on the command line: start where Glimt would otherwise, then go there once mounted.
        match path.to_str().filter(|p| p.contains("://")) {
            Some(uri) => {
                app.open(ctx, ".".into());
                app.mount(ctx, uri.to_owned());
            }
            None if !start.more.is_empty() => {
                let all = std::iter::once(path).chain(start.more.iter().cloned());
                app.open_folders(ctx, all.collect());
            }
            None => app.open(ctx, path),
        }
        // Small folders are listed before the first frame; large ones hold startup back at most 50
        // ms.
        app.receive_listing(ctx, std::time::Duration::from_millis(50));
        app
    }

    fn frame_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = &ui.ctx().clone();
        #[cfg(feature = "test-hooks")]
        hooks::begin_frame();
        self.sync_thumb_source(ctx.pixels_per_point());
        self.poll_background(ctx);
        let mut act = self.read_keys(ctx);
        // Full screen and Hide File List as in Geeqie: only the image, no menu or panels.
        let fullscreen = self.track_window(ctx);
        self.hide_still_pointer(ctx, fullscreen);
        let bare = fullscreen || self.hide_file_list || self.kiosk;
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
        self.lightroom_dialog(ctx);
        self.save_as_dialog(ctx);
        self.cut_short_dialog(ctx);
        self.pan_view(ctx);
        self.exif_fields_window(ctx);
        self.login_dialog(ctx);
        self.confirm_delete_dialog(ctx);
        self.confirm_folder_dialog(ctx);
        self.duplicates_window(ctx);
        self.check_window(ctx);
        help::window(ctx, &mut self.help);
        self.focus_ring(ctx);
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
        if !bare && self.config.exif_panel && !self.list.files.is_empty() {
            self.exif_panel(ui);
        } else {
            // Hidden while a field had the focus: it never lost it.
            self.save_meta(ui.ctx());
        }
        self.image_area(ctx, ui, fullscreen, bare);
        if self.projector {
            self.projector_window(ctx);
        }
    }
}

// The parts of a frame, in the order `ui` draws them, and what reaches it from the threads in the
// background. The windows of one feature (pan view, Find Duplicates, Check Images, Preferences,
// the projector) are with that feature: above, or in its own module.
impl eframe::App for App {
    /// The last folder is written once on exit, not on every folder change.
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // No new client first: one coming after the answers would have none.
        remote::close();
        self.answer_held_selects();
        remote::stop(|| self.answer_held_selects());
        let ctx = self.ctx.clone();
        self.save_meta(&ctx);
        // shortcut: the path is stored as UTF-8, lossless for ordinary paths; store its bytes
        // if a folder name that is not UTF-8 must be remembered.
        let mut state = String::new();
        if self.config.remember_dir {
            let last = self.collection.as_ref().unwrap_or(&self.dir);
            state += &format!("last_dir={}\n", last.display());
        }
        if self.config.remember_window {
            state += &self.window_state.to_text();
        }
        // Nothing to keep: the folder and window of before go too, not left for a later start.
        match state_path() {
            Some(p) if state.is_empty() => drop(std::fs::remove_file(p)),
            Some(p) => write_atomic(&p, &state),
            None => {}
        }
        self.inhibit(false);
        // The state above, the settings, and the keywords queued at the first close: waited for,
        // a few seconds at most. Not after a third close, which ends at once whatever runs.
        if self.closing < 2 {
            config::flush();
        }
        let at = |share| frame_at(&self.frame_ms, share);
        eprintln!(
            "frames: median {} ms, 90% within {} ms, slowest {} ms",
            at(0.5),
            at(0.9),
            at(1.0)
        );
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let start = thread_cpu_us();
        self.frame_ui(ui);
        if self.started.is_none() {
            let ms = thread_cpu_us().saturating_sub(start) / 1000;
            self.frame_ms[(ms as usize).min(255)] += 1;
        }
    }
}

/// The text of a widget as the release tests name it: the label without the icon before it.
fn noted(response: &egui::Response, text: &str) -> egui::Response {
    #[cfg(feature = "test-hooks")]
    hooks::note(
        text.trim_start_matches(|c: char| ('\u{e000}'..='\u{f8ff}').contains(&c) || c == ' '),
        response.rect,
    );
    #[cfg(not(feature = "test-hooks"))]
    let _ = text;
    response.clone()
}

/// The windows' app ID, the desktop file's name, so the desktop shows its icon for them:
/// glimt.desktop, or in a Flatpak io.github.thornbux.Glimt.desktop (FLATPAK_ID).
fn app_id() -> String {
    std::env::var("FLATPAK_ID").unwrap_or_else(|_| "glimt".into())
}

unsafe extern "C" {
    /// glibc: return freed heap memory to the operating system.
    fn malloc_trim(pad: usize) -> i32;
    /// glibc: set a malloc parameter.
    fn mallopt(param: i32, value: i32) -> i32;
}

/// Broken forms of `seed`, as a damaged or crafted file has them, for the parsers' tests: bits
/// flipped, bytes changed, cut short, a piece doubled. Always the same ones for a seed, so a
/// failure comes again; GLIMT_FUZZ=N for N of them instead of 2,000, GLIMT_FUZZ_SEED=S for
/// other ones.
#[cfg(test)]
fn broken(seed: &[u8]) -> impl Iterator<Item = Vec<u8>> + '_ {
    let n = std::env::var("GLIMT_FUZZ")
        .ok()
        .and_then(|n| n.parse().ok());
    // Another seed (GLIMT_FUZZ_SEED) gives other mutations; never 0, which xorshift keeps.
    let seed_value = (std::env::var("GLIMT_FUZZ_SEED").ok())
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|&s| s != 0);
    let mut x: u64 = seed_value.unwrap_or(0x9e37_79b9_7f4a_7c15);
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x as usize
    };
    (0..n.unwrap_or(2000)).map(move |_| {
        let mut v = seed.to_vec();
        for _ in 0..1 + next() % 4 {
            if v.is_empty() {
                break;
            }
            let at = next() % v.len();
            match next() % 4 {
                0 => v[at] ^= 1 << (next() % 8),
                1 => v[at] = next() as u8,
                2 => v.truncate(at),
                _ => {
                    let piece = v[at..(at + next() % 16).min(v.len())].to_vec();
                    v.splice(at..at, piece);
                }
            }
        }
        v
    })
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

/// What is wrong in the settings file, for the window: the first, and where to see all. Not a
/// setting this Glimt does not know: a newer one beside it (a Flatpak and a .deb) wrote it, and
/// this one keeps it there, so the notice would come at every start.
fn config_notice(problems: &[String]) -> Option<String> {
    let problems: Vec<&String> = (problems.iter())
        .filter(|p| !p.contains(": there is no setting "))
        .collect();
    let first = problems.first()?;
    let more = match problems.len() - 1 {
        0 => String::new(),
        n => tf!("; and {n} more (glimt --check-config)", n),
    };
    Some(format!("Settings file: {first}{more}"))
}

/// glimt --check-config [FILE]: what is wrong in the settings file, or FILE; exit 1 if anything.
fn check_config(file: Option<PathBuf>) -> ! {
    let Some(path) = file.or_else(config::config_path) else {
        eprintln!("glimt: no home folder, so no settings file");
        std::process::exit(1);
    };
    let problems = match crate::decode::read_text(&path) {
        Ok(text) => Config::check(&text).1,
        Err(e) => vec![format!("cannot be read ({e})")],
    };
    for p in &problems {
        eprintln!("{}: {p}", path.display());
    }
    if problems.is_empty() {
        println!("{}: OK", path.display());
    }
    std::process::exit(!problems.is_empty() as i32)
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
    if args.first().is_some_and(|a| a == "--check-config") {
        check_config(args_os.get(1).map(PathBuf::from));
    }
    if args.first().is_some_and(|a| a == "--back-up") {
        let Some(dir) = args_os.get(1) else {
            eprintln!("glimt: --back-up takes the folder to put the backup in");
            std::process::exit(2);
        };
        match config::back_up_to(Path::new(dir)) {
            Ok(done) => println!("{done}"),
            Err(e) => {
                eprintln!("glimt: {e}");
                std::process::exit(1);
            }
        }
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
        let paths = args_os.get(2..).unwrap_or_default();
        match remote::client(&args[1..], paths, start.name.as_deref(), to_all) {
            Some(path) => arg = Some(path),
            None => return Ok(()),
        }
    }
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Glimt")
        .with_app_id(app_id())
        .with_inner_size([1280.0, 800.0]);
    if let Ok(icon) = image::load_from_memory(LOGO) {
        let icon = icon.into_rgba8();
        viewport = viewport.with_icon(egui::IconData {
            width: icon.width(),
            height: icon.height(),
            rgba: icon.into_raw(),
        });
    }
    // A kiosk changes no settings either: a visitor's keys would last until the next boot.
    if start.lock_settings || start.kiosk || Path::new(config::LOCK_FILE).exists() {
        config::lock();
    }
    if Config::load().remember_window
        && let Some(state) = state_path().and_then(|p| crate::decode::read_text(p).ok())
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
    /// An `App` for a test, on folder `dir`, listed: its own settings, no remote control,
    /// nothing kept in the user's files (no recent list, thumbnails or last folder; marks are
    /// not written in tests). The window's logic without a window.
    pub(crate) fn app_on(dir: &std::path::Path) -> (super::App, eframe::egui::Context) {
        let ctx = eframe::egui::Context::default();
        let config = crate::config::Config {
            remote_control: false,
            keep_recent: false,
            save_thumbnails: false,
            desktop_profile: false,
            remember_dir: false,
            ..Default::default()
        };
        let start = super::Start::default();
        // A new window sets the colour settings, which also empties the transforms a colour
        // test running beside it was looking at.
        let held = crate::colour::COLOUR_TEST
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut app = super::App::with(&ctx, config, &[], Some(dir.into()), start);
        drop(held);
        app.receive_listing(&ctx, std::time::Duration::from_secs(5));
        (app, ctx)
    }

    /// A folder for the contract: a.png (red) and b.png (blue), each with a sidecar saying whose
    /// it is, a folder sub, and a collection naming both images.
    pub(crate) fn contract_folder(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-contract-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        for (file, colour) in [("a", [255, 0, 0]), ("b", [0, 0, 255])] {
            let png = dir.join(format!("{file}.png"));
            image::RgbImage::from_pixel(8, 8, image::Rgb(colour))
                .save(&png)
                .unwrap();
            std::fs::write(crate::meta::sidecar(&png), file).unwrap();
        }
        let gqv = format!(
            "#GQview collection\n\"{}\"\n\"{}\"\n",
            dir.join("a.png").display(),
            dir.join("b.png").display()
        );
        std::fs::write(dir.join("col.gqv"), gqv).unwrap();
        dir
    }

    /// Run the window's background work until `done` holds, at most five seconds.
    pub(crate) fn settle(
        app: &mut super::App,
        ctx: &eframe::egui::Context,
        done: impl Fn(&super::App) -> bool,
    ) -> bool {
        (0..250).any(|_| {
            app.poll_background(ctx);
            let now = done(app);
            if !now {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            now
        })
    }

    /// The window's state is made and lists a folder without a window, so its logic can be
    /// tested in milliseconds rather than through the release tests' virtual display.
    #[test]
    fn an_app_lists_its_folder_without_a_window() {
        let dir = std::env::temp_dir().join(format!("glimt-test-app-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["b.png", "a.png"] {
            image::RgbImage::new(4, 4).save(dir.join(name)).unwrap();
        }
        let (app, _ctx) = app_on(&dir);
        assert_eq!(app.list.files, ["a.png", "b.png"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A setting this Glimt does not know is no notice at start; a wrong value is.
    #[test]
    fn tells_settings_problems_but_unknown_ones() {
        let unknown = "line 3: there is no setting projector_screen".to_string();
        let wrong = "line 5: delay=fast is not a number; 3 is used".to_string();
        assert_eq!(super::config_notice(&[unknown.clone()]), None);
        assert_eq!(
            super::config_notice(&[unknown, wrong.clone()]),
            Some(format!("Settings file: {wrong}"))
        );
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
        assert!(
            Start::parse(args(&["--lock-settings"]))
                .unwrap()
                .0
                .lock_settings
        );
        assert_eq!(
            Start::parse(args(&["--fade", "long"])).unwrap().0.fade,
            Some(2)
        );
        assert!(Start::parse(args(&["--fade", "slow"])).is_err());
        let (_, rest) = Start::parse(args(&["--version"])).unwrap();
        assert_eq!(rest, args(&["--version"]));
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
}
