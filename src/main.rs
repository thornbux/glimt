//! Glimt: a fast, memory-frugal image viewer.
//!
//! Images are decoded in background threads to the size of the image area; full resolution
//! is decoded only when the user zooms past what that covers. Only a small window around
//! the current image is kept in memory.

use actions::{Action, CTRL, CTRL_SHIFT, OWN_KEYS, SHORTCUTS, shortcut_text, shortcuts};
use colour::{
    COLOR_MANAGEMENT, FILTERS, INPUT_PROFILES, INTENTS, SCREEN_WAIT, SCREEN_WAIT_MS, VIEW_FILTER,
    colour_source, read_desktop_screen, set_colour, set_input, set_intent, set_screen,
    srgb_to_display,
};
use config::{
    CPU_LEVELS, Config, FADES, MAX_DELAY, MEMORY_LEVELS, OSD_TEMPLATE, WindowState,
    keyword_tree_path, parse_last_dir, sort_folders_from, sort_folders_path, state_path,
    write_atomic, xdg_dir,
};
use decode::{Decoded, Hist, MAX_ANIMATION, color_image, decode, histogram, load, shrink, survive};
use eframe::egui;
use external::{Login, Mounted, apps_for, command, gio_mount, is_video, open_url};
use files::{
    SORTS, Types, has_ext, is_listed, list_folder, list_tree, merge_shuffled, pattern_name,
    shuffle, sort_files, walk,
};
use lang::tr;
use meta::Meta;
use save::save_rotated;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Instant;

#[cfg(feature = "accessibility")]
mod accessibility;
mod actions;
mod collections;
mod colour;
mod config;
mod decode;
mod decoders;
mod duplicates;
mod exif;
mod external;
mod file_ops;
mod files;
mod help;
mod journal;
mod lang;
mod lightroom;
#[cfg(feature = "marks")]
mod marks;
mod meta;
mod pan;
mod plugins;
mod preferences;
mod print;
mod raw;
mod remote;
mod save;
mod search;
mod theme;
mod thumbs;
#[cfg(feature = "video")]
mod video;

/// Thumbnail box in the file list, in points (Geeqie's default 96×72).
const THUMBS: [egui::Vec2; 3] = [
    egui::vec2(96.0, 72.0),
    egui::vec2(128.0, 96.0),
    egui::vec2(192.0, 144.0),
];
/// A cell of the thumbnail grid: the thumbnail with its name below.
const CELL_EXTRA: egui::Vec2 = egui::vec2(8.0, 26.0);
/// Thumbnails come from the freedesktop "large" cache, for big ones or a scaled screen; read by
/// the workers.
static LARGE_THUMBS: AtomicBool = AtomicBool::new(false);
/// Thumbnail jobs further than this from the visible rows are skipped.
const THUMB_MARGIN: usize = 20;
/// The files whose thumbnails are on screen, for the thumbnail workers to make first: `[0]` in the
/// file list, `[1]` in pan view.
type ThumbView = Arc<Mutex<[Vec<usize>; 2]>>;
/// Width of the size and date columns in the file list (points).
const DATE_WIDTH: f32 = 140.0;
const DETAILS_WIDTH: f32 = DATE_WIDTH + 90.0;
/// Thumbnails kept in memory (≈ 64 kB each) before those far from view are dropped.
const MAX_THUMBS: usize = 600;

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

/// `work` on `items` cut into one part for each of `threads`, run at once; the results in the
/// order of the parts. `work` gets a part and the place in `items` where it starts.
fn in_parts<T: Sync, R: Send>(
    items: &[T],
    threads: usize,
    work: impl Fn(&[T], usize) -> Vec<R> + Sync,
) -> Vec<R> {
    let chunk = items.len().div_ceil(threads.max(1)).max(1);
    std::thread::scope(|s| {
        let parts: Vec<_> = (items.chunks(chunk).enumerate())
            .map(|(c, part)| {
                let work = &work;
                s.spawn(move || work(part, c * chunk))
            })
            .collect();
        (parts.into_iter())
            .flat_map(|p| p.join().unwrap_or_default())
            .collect()
    })
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

/// The pan view's window (Ctrl+J).
#[derive(Default)]
struct Pan {
    /// Days from the EXIF dates instead of the file dates.
    exif: bool,
    /// The days, and the version of the file list they are for (`App::files_gen`).
    days: Option<(Vec<pan::Day>, usize)>,
    days_rx: Option<mpsc::Receiver<(Vec<pan::Day>, usize)>>,
    /// The rows for this many columns, and their height.
    rows: Vec<(f32, pan::Row)>,
    columns: usize,
    height: f32,
    /// The calendar instead of the timeline, its months, and a day picked there for the
    /// timeline to scroll to.
    calendar: bool,
    months: Vec<pan::Month>,
    jump: Option<usize>,
}

/// What a delete, move, copy or plugin reports, in one message: the files it was asked to
/// delete or move, those gone now (found out in its thread, not one `stat` each in the
/// window's), whether to read the folder again, and the files moved or copied `(from, to)`,
/// whose marks follow before the gone ones are forgotten. One message, so nothing depends on
/// the order two would arrive in.
#[derive(Default)]
struct Deleted {
    paths: Vec<PathBuf>,
    gone: Vec<PathBuf>,
    failed: bool,
    moved: Vec<(PathBuf, PathBuf)>,
    copied: bool,
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
}

impl Job {
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

/// The Find Duplicates window: what to compare by, the search running and its results.
struct Dups {
    by: duplicates::By,
    progress: Arc<duplicates::Progress>,
    groups_rx: Option<mpsc::Receiver<Vec<Vec<PathBuf>>>>,
    groups: Vec<Vec<PathBuf>>,
    /// (group, file) of each row: the file `usize::MAX` for the group's heading.
    rows: Vec<(usize, usize)>,
    checked: HashSet<PathBuf>,
    /// Images searched, for the line above the results.
    searched: usize,
    /// File sizes, read once when a row is first shown, not on every frame.
    sizes: HashMap<PathBuf, u64>,
}

impl Dups {
    fn new() -> Self {
        Dups {
            by: duplicates::By::Checksum,
            progress: Arc::default(),
            groups_rx: None,
            groups: Vec::new(),
            sizes: HashMap::new(),
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

/// Check Images: every image of the list opened in the background, as the viewer would; those
/// that cannot be are listed, and marked in the file list.
struct Check {
    /// Files checked, and the threads still at work.
    done: Arc<AtomicUsize>,
    working: Arc<AtomicUsize>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    total: usize,
    found_rx: mpsc::Receiver<(OsString, String)>,
    /// The broken ones found: (name in the list, why).
    broken: Vec<(OsString, String)>,
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
    Action::FitWidth,
    Action::FitHeight,
    Action::FitFill,
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

/// Where an image of `orig` is drawn in `rect`, and its scale: at the `zoom` and pan given, else
/// fitted in fit `mode`, centred, and larger than its size only with `enlarge`.
fn placed(
    rect: egui::Rect,
    orig: [usize; 2],
    ppp: f32,
    (enlarge, mode): (bool, u8),
    zoom: Option<(f32, egui::Vec2)>,
) -> (egui::Rect, f32) {
    let size = egui::vec2(orig[0] as f32, orig[1] as f32);
    let (scale, pan) = zoom.unwrap_or_else(|| {
        let fit = fit_scale(rect.size() * ppp, size.into(), mode);
        (if enlarge { fit } else { fit.min(1.0) }, egui::Vec2::ZERO)
    });
    let at = egui::Rect::from_center_size(rect.center() + pan, size * scale / ppp);
    (at, scale)
}

/// A texture shown and its image's size, which a fade goes from.
type Shown = (egui::TextureHandle, [usize; 2]);

/// `tex` drawn in `rect` (its size before turning), turned by quarter `turns` and mirrored as
/// the view is, tinted.
fn paint_turned(
    ui: &egui::Ui,
    tex: &egui::TextureHandle,
    rect: egui::Rect,
    (turns, mirrored): (u8, bool),
    tint: egui::Color32,
) {
    let uv = match mirrored {
        true => egui::Rect::from_min_max(egui::pos2(1.0, 0.0), egui::pos2(0.0, 1.0)),
        false => egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
    };
    let angle = turns as f32 * std::f32::consts::FRAC_PI_2;
    egui::Image::new(tex)
        .uv(uv)
        .rotate(angle, egui::Vec2::splat(0.5))
        .tint(tint)
        .paint_at(ui, rect);
}

/// Faded to `alpha` (0 to 1), as a tint. A fade draws the image before at 1 - t⁴ under the new
/// one at t: the sum stays near full brightness, where 1 - t would darken the middle.
fn alpha(a: f32) -> egui::Color32 {
    egui::Color32::from_white_alpha((a.clamp(0.0, 1.0) * 255.0) as u8)
}

/// The image area of the last frame, which the projector follows: where it was (points), the
/// screen's pixels per point, the scale (screen pixels per image pixel), the offset of a zoomed
/// image, whether it was zoomed, and the pointer over it, from the image's centre.
#[derive(Clone, Copy)]
struct View {
    area: egui::Rect,
    ppp: f32,
    scale: f32,
    pan: egui::Vec2,
    zoomed: bool,
    pointer: Option<egui::Vec2>,
}

/// The scale (screen pixels per image pixel) at which an image of `size` fits `view`, both in
/// pixels, in fit `mode`: 0 the whole image, 1 its width, 2 its height, 3 filling the view.
fn fit_scale(view: egui::Vec2, size: [f32; 2], mode: u8) -> f32 {
    let (x, y) = (view.x / size[0], view.y / size[1]);
    match mode {
        1 => x,
        2 => y,
        3 => x.max(y),
        _ => x.min(y),
    }
}

/// Whether an image of `orig` shown at `scale` fits in a texture whose sides are at most `long`:
/// then it is decoded again at that size, not at full resolution. Not the decoders' limit, whose
/// free side is `long` itself in Fit Width and Fit Height.
fn fits_texture(orig: [usize; 2], scale: f32, long: u32) -> bool {
    orig.iter().all(|&side| side as f32 * scale < long as f32)
}

/// The row `delta` on from `row` of `n` rows, wrapping round or stopping at the ends; from
/// before the first row (`None`, the shown file hidden by a search) the step counts from there.
fn next_row(row: Option<usize>, delta: isize, n: usize, wrap: bool) -> Option<usize> {
    if n == 0 {
        return None;
    }
    let (row, n) = (row.map_or(-1, |r| r as isize), n as isize);
    let r = match wrap {
        true => (row + delta).rem_euclid(n),
        false => (row + delta).clamp(0, n - 1),
    };
    Some(r as usize)
}

/// What a slideshow does once its delay is up.
#[derive(Debug, PartialEq)]
enum Slide {
    /// The round is over and it does not repeat.
    Stop,
    /// A new round of a random order: the list shuffled again.
    Shuffle,
    /// The next image, or the next set.
    Step(usize),
    /// The next image is still being decoded: looked at again soon.
    Hold,
}

/// Where a slideshow is in its order: at `row` of the `shown` rows (`None` when a search hides
/// the image), stepping `delta` (the panes changed at once), in a list of `len` files.
#[derive(Clone, Copy)]
struct Place {
    row: Option<usize>,
    delta: usize,
    shown: usize,
    len: usize,
}

/// The slideshow's move from `at`, to `next` if there is one; repeating or not, in a `shuffled`
/// order, with `next` `pending` (still being decoded) or not.
fn next_slide(
    at: Place,
    next: Option<usize>,
    repeat: bool,
    shuffled: bool,
    pending: bool,
) -> Slide {
    let Place {
        row,
        delta,
        shown,
        len,
    } = at;
    let round_over = round_ends(row, delta, shown);
    let Some(next) = next else { return Slide::Hold };
    if round_over && !repeat {
        return Slide::Stop;
    }
    // A new round shuffles again instead of repeating the same order. Not when a search hides
    // the shown image, nor when it shows no more images than one step: the shuffled list starts
    // with the shown image, so the round would be over again, and shuffled on every frame.
    if shuffled && round_over && row.is_some() && len > 2 && !round_ends(Some(0), delta, shown) {
        return Slide::Shuffle;
    }
    match pending {
        true => Slide::Hold,
        false => Slide::Step(next),
    }
}

/// Whether a slideshow stepping `delta` on from `row` of a list of `len` starts again at the top:
/// also when the shown file is not in the list (hidden by a search). Several panes step by
/// their number, so the round may end past the last row, not on the first.
fn round_ends(row: Option<usize>, delta: usize, len: usize) -> bool {
    row.is_none_or(|r| r + delta >= len)
}

/// The box an image is decoded into: the `view` box for the shown image, turned with it when
/// it is turned a quarter by hand (`quarter`; Fill's view has the turn in it already); for a
/// neighbour the same box, unturned, or in Fill the longer side's square, as its shape is not
/// known before it is decoded (shown larger, it is decoded again); and at least the projector's
/// size, turned with the shown image too.
fn decode_box(
    view: [u32; 2],
    area: [u32; 2],
    fill: bool,
    shown: bool,
    quarter: bool,
    projector: [u32; 2],
) -> [u32; 2] {
    let [w, h] = match (fill, shown) {
        (true, false) => [area[0].max(area[1]); 2],
        _ => view,
    };
    let turned = quarter && shown;
    let [w, h] = if turned && !fill { [h, w] } else { [w, h] };
    let [pw, ph] = if turned {
        [projector[1], projector[0]]
    } else {
        projector
    };
    [w.max(pw), h.max(ph)]
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
    response.on_hover_text(tr(
        "The pane these details are of: the one under the pointer",
    ));
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

fn layout_grid(layout: &str) -> [usize; 2] {
    LAYOUTS
        .iter()
        .find(|l| l.0 == layout)
        .map_or([1, 1], |l| l.2)
}

/// A Rename Many running in the background: the list as it was (`App::files_gen`), the files
/// renamed with their new names, and which of them was shown.
struct RenameJob {
    files_gen: usize,
    /// The old names, for the marks and a collection's entries to follow.
    from: Vec<PathBuf>,
    to: Vec<PathBuf>,
    shown: Option<usize>,
}

/// What a file dialog (zenity or kdialog) was opened for.
enum Pick {
    Open,
    /// Several folders to show as one list.
    Folders,
    /// A folder with the images of every folder below it.
    Recursive,
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
    /// A Lightroom Classic catalog to import.
    Lightroom,
    /// The folder to put a backup of Glimt's own files in.
    BackUp,
    /// Where to save frame (or page) number .1 of the file .0, as PNG.
    SaveFrame(PathBuf, usize),
    /// The folder for every frame (or page) of the file, as PNG files.
    ExportFrames(PathBuf),
}

/// Images to print, and how.
struct PrintJob {
    paths: Vec<PathBuf>,
    layout: print::Layout,
    /// The template of the text under each image, if there is one.
    caption: Option<String>,
    /// On both sides of the paper (a printer, not a PDF).
    two_sided: bool,
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
    /// The first page's images, small, for the preview, as they come from the background, with
    /// their sizes upright as printed (for %dimensions%); `decoded` once all have come.
    previews: Vec<Option<egui::TextureHandle>>,
    sizes: Vec<Option<[usize; 2]>>,
    preview_rx: mpsc::Receiver<(usize, [usize; 2], egui::ColorImage)>,
    decoded: bool,
    /// Their texts from the template they were made of; being made again after it changed.
    captions: Option<(String, Vec<String>)>,
    captions_rx: Option<mpsc::Receiver<(String, Vec<String>)>>,
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
    /// A new name for the open collection.
    RenameCollection,
    /// Save the look shown as a theme file of this name.
    SaveTheme,
}

/// A menu that scrolls when it is taller than the window, instead of running off it.
fn scroll_menu<R>(
    ui: &mut egui::Ui,
    title: impl Into<egui::WidgetText>,
    add: impl FnOnce(&mut egui::Ui) -> R,
) {
    let content = |ui: &mut egui::Ui| {
        // From where it opens to the bottom of the window; submenus open further down. egui
        // limits a menu to its size the frame before, which a scroll area could only keep or
        // shrink: the limit is set here instead.
        let margin = ui.spacing().menu_margin.bottom as f32 + ui.spacing().item_spacing.y;
        let max = ui.ctx().content_rect().bottom() - ui.next_widget_position().y - margin;
        ui.set_max_height(max);
        egui::ScrollArea::vertical().max_height(max).show(ui, add);
    };
    if egui::containers::menu::is_in_menu(ui) {
        ui.menu_button(title, content);
        return;
    }
    // A menu of the menu bar. While one of them is open, pointing at another opens that one,
    // as in GNOME and KDE.
    let response = egui::containers::menu::MenuButton::new(title.into())
        .ui(ui, content)
        .0;
    let ctx = ui.ctx().clone();
    let id = egui::Popup::default_response_id(&response);
    let others = ctx.data_mut(|d| {
        let ids = d.get_temp_mut_or_default::<Vec<egui::Id>>(egui::Id::new(BAR_MENUS));
        if !ids.contains(&id) {
            ids.push(id);
        }
        ids.clone()
    });
    let other_open = others
        .iter()
        .any(|&o| o != id && egui::Popup::is_id_open(&ctx, o));
    if response.hovered() && other_open {
        egui::Popup::open_id(&ctx, id);
        ctx.request_repaint();
    }
}

/// The windows' app ID, the desktop file's name, so the desktop shows its icon for them:
/// glimt.desktop, or in a Flatpak io.github.thornbux.Glimt.desktop (FLATPAK_ID).
fn app_id() -> String {
    std::env::var("FLATPAK_ID").unwrap_or_else(|_| "glimt".into())
}

/// Where the popup IDs of the menu bar's menus are kept, for `scroll_menu` and `close_menus`.
const BAR_MENUS: &str = "menu bar menus";

/// Close an open menu of the menu bar once the pointer has moved away from the bar and every
/// open menu for a moment (`away` holds since when). A menu opened from the keyboard stays
/// while the mouse is still.
fn close_menus(ctx: &egui::Context, bar: egui::Rect, away: &mut Option<Instant>) {
    let ids: Vec<egui::Id> = ctx.data(|d| d.get_temp(egui::Id::new(BAR_MENUS)).unwrap_or_default());
    if !ids.iter().any(|&id| egui::Popup::is_id_open(ctx, id)) {
        *away = None;
        return;
    }
    let pointer = ctx.pointer_hover_pos();
    // Over the bar, or over a menu: a layer of its own above the panels.
    let over = pointer.is_some_and(|p| {
        bar.contains(p)
            || ctx
                .layer_id_at(p)
                .is_some_and(|l| l.order != egui::Order::Background)
    });
    if over {
        *away = None;
    } else if away.is_none()
        && ctx.input(|i| i.pointer.delta() != egui::Vec2::ZERO || pointer.is_none())
    {
        *away = Some(Instant::now());
    }
    if let Some(since) = *away {
        let wait = std::time::Duration::from_millis(400);
        match wait.checked_sub(since.elapsed()) {
            Some(left) => ctx.request_repaint_after(left),
            None => {
                for &id in &ids {
                    egui::Popup::close_id(ctx, id);
                }
                *away = None;
                ctx.request_repaint();
            }
        }
    }
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
        Some(keys) => format!("{label} ({keys})", label = action.label()),
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

/// A file's EXIF from the reader thread.
struct ExifRead {
    path: PathBuf,
    /// The EXIF panel's rows, and every field for the EXIF window.
    rows: Vec<(&'static str, String)>,
    all: Vec<(&'static str, String, String)>,
    /// Keywords and comment, their keyword tree paths, and the alt text and description.
    meta: meta::Meta,
    paths: Vec<String>,
    access: meta::Access,
}

/// A rectangle being dragged in the icon grid to select.
struct RubberBand {
    /// Where it started, from the top left of the grid's content, so it scrolls along.
    start: egui::Vec2,
    /// The selection before it, kept with Ctrl or Shift.
    before: std::collections::BTreeSet<usize>,
    /// Whether the pointer has moved far enough to be a rectangle and not a click.
    moved: bool,
    /// The rows and columns it touched when the selection was last made from it.
    touched: Option<[usize; 4]>,
}

/// Keywords, comment, alt text and extended description of the shown image as being edited in
/// the EXIF panel, and as last read or saved, to tell whether they changed.
struct MetaEdit {
    path: PathBuf,
    keywords: String,
    comment: String,
    saved: meta::Meta,
    /// Its keyword tree paths, which tick the tree's boxes with `saved`'s keywords.
    paths: Vec<String>,
    /// The IPTC alt text and extended description as edited, and as last read or saved.
    alt: String,
    extended: String,
    access: meta::Access,
}

/// An image decoded for the view: the shown one or a neighbour read ahead, at the size shown or
/// (`full`) at full resolution.
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

/// Glimt's logo, 256 pixels square: the window's icon, and on the About page.
const LOGO: &[u8] = include_bytes!("../assets/glimt-256.png");

/// One piece of background work, for the busy pointer (`busy`) or for closing
/// (`config::WRITING`): counted while held, and no longer when dropped, also as its thread
/// panics, which a count by hand would leave on for good.
struct Working(Arc<AtomicUsize>);

impl Working {
    fn new(count: &Arc<AtomicUsize>) -> Self {
        count.fetch_add(1, Ordering::Relaxed);
        Working(count.clone())
    }
}

impl Drop for Working {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

struct App {
    // ---- Settings ----
    /// The desktop's dark style and accent colour, once read from the portal.
    desktop_look: theme::Desktop,
    desktop_rx: Option<mpsc::Receiver<theme::Desktop>>,
    /// The desktop's screen profile file, once read from colord.
    desktop_screen: Option<String>,
    desktop_screen_rx: Option<mpsc::Receiver<Option<String>>>,
    /// The action whose key is being chosen in Preferences: the next key press goes to it.
    capturing: Option<Action>,
    config: Config,

    // ---- The list: the folder (or collection, or several folders) and its files ----
    /// A network location (smb:// …) being mounted: its local path, or why it failed.
    mounting_rx: Option<mpsc::Receiver<(Mounted, Login)>>,
    /// The login dialog for a network share that needs one.
    login: Option<Login>,
    /// The folder path in the toolbar, as typed.
    path_text: String,
    dir: PathBuf,
    subdirs: Vec<PathBuf>,
    /// The images in `dir`, file name only (the full path is built on demand by `path`): saves
    /// memory in large folders, where the path would otherwise be repeated for every file.
    files: Vec<OsString>,
    /// Goes up whenever the file list changes, so what was worked out from its indices can tell
    /// that it is old, also when the list kept its length (another order).
    files_gen: usize,
    cur: usize,
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
    listing_rx: Option<mpsc::Receiver<(Vec<PathBuf>, Vec<OsString>)>>,
    /// The subfolders of a folder listed with its tree, sent at once: the tree may take long.
    subdirs_rx: Option<mpsc::Receiver<Vec<PathBuf>>>,
    /// The images of the tree found so far, as it is read, a folder of it at a time.
    part_rx: Option<mpsc::Receiver<Vec<OsString>>>,
    /// The next listing shows a tree's images as they are found (a folder opened).
    list_parts: bool,
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
    /// File list size and date columns: (bytes, mtime) by file name.
    details: HashMap<OsString, (u64, i64)>,
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

    // ---- The selection ----
    /// Images of a collection being dragged in the file list, and the place they would go.
    dragging: Option<(HashSet<usize>, Option<usize>)>,
    /// Selected files (Ctrl/Shift+click in the file list) that Delete acts on.
    selected: std::collections::BTreeSet<usize>,
    /// A rectangle being dragged in the icon grid to select.
    rubber_band: Option<RubberBand>,
    /// Where a Shift selection started (file index).
    anchor: Option<usize>,
    /// Size of the selected files; computed in the background when the selection changes.
    selected_size: Option<u64>,
    selected_rx: Option<mpsc::Receiver<u64>>,
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
    slots: HashMap<usize, Slot>,
    /// Decodes asked for, with the number of the job: only its result is taken, so one asked
    /// for before the list, the page, the look or the file changed is dropped.
    decodes_pending: HashMap<(usize, bool), (u64, PathBuf)>,
    decodes_sent: u64,
    decode_errors: HashMap<usize, String>,
    /// The page of a multi-page TIFF shown (Ctrl+Page Down/Up), from 0.
    page: usize,
    /// The frame of an animation stepped to with Ctrl+Page Up/Down; `None` while it plays.
    frame: Option<usize>,
    /// Last drawn image; kept on screen until the next one is decoded, so browsing never shows a black screen.
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
    /// Scale from the previous frame (physical pixels per image pixel), for the zoom percentage in the status bar.
    last_scale: f32,
    /// Mouse wheel notches not yet turned into an image step (wheel_zoom off).
    wheel: f32,
    /// Orientation of the shown image, for viewing only: quarter turns clockwise, applied
    /// after mirroring when the flag is set. Reset for each image, as in Geeqie.
    orient: (u8, bool),
    /// The file `orient` belongs to, set when it is turned: saved into that file even when the
    /// list changed meanwhile and another file is at `cur`.
    turned: PathBuf,
    /// Two images side by side (E): the shown one and `compare_other`.
    compare: bool,
    /// The images of a layout's panes after the first, which shows `cur`.
    panes: Vec<usize>,
    /// Columns of the icon grid in the last frame; 1 when the list is rows or hidden.
    grid_cols: usize,
    /// How far Previous/Next move this frame: a row of the icon grid for ↑ and ↓, else 1.
    row_step: isize,
    /// View → Hide File List: only the image, as in full screen but in a window.
    hide_file_list: bool,
    /// Window geometry seen in the last frame, saved at exit.
    window_state: WindowState,
    /// The screen saver inhibitor running while in full screen.
    inhibitor: Option<std::process::Child>,
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
    /// The file list is shuffled for a random slideshow.
    shuffled: bool,
    /// The next listing is shuffled as a whole instead of merged into the current order.
    whole_shuffle: bool,
    /// Slideshow: when the previous image was shown; `None` = off.
    slideshow: Option<Instant>,
    /// Slideshow paused (P): stays on the image until resumed.
    paused: bool,
    /// --slideshow: start it once the folder is read.
    slideshow_at_start: bool,
    /// A slideshow changing one pane at a time: the pane next (0 is the first), and the image
    /// shown last.
    next_pane: usize,
    last_slide: usize,

    // ---- Thumbnails ----
    /// Thumbnails still being made when a setting changed how they look: their results are
    /// dropped once, as they come back in the old look.
    stale_thumbs: HashSet<OsString>,
    /// File list thumbnails by file name; `None` = could not be made.
    thumbs: HashMap<OsString, Option<egui::TextureHandle>>,
    thumbs_pending: HashSet<OsString>,
    /// (image index, file)
    thumb_jobs: mpsc::Sender<(usize, PathBuf)>,
    /// (file, thumbnail); the outer `None` = skipped because it scrolled out of view.
    thumbs_rx: mpsc::Receiver<(PathBuf, Option<Option<egui::ColorImage>>)>,
    /// The files the file list (or film strip) shows, with some rows around them, and those pan
    /// view shows, both sorted: a thumbnail job for another file is skipped. Files, not a range:
    /// the matches of a search, or pan view's days, are spread over the whole folder.
    thumb_view: ThumbView,
    /// The files whose cells pan view draws this frame: their thumbnails are made and kept,
    /// also while the file list shows others.
    pan_thumbs: Vec<usize>,
    /// The rows the file list showed last, in the list as shown; thumbnails near them are kept.
    list_rows: std::ops::Range<usize>,
    /// Workers write new thumbnails to the shared cache (Preferences).
    save_thumbs: Arc<AtomicBool>,

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
    exif_index_rx: Option<mpsc::Receiver<HashMap<OsString, String>>>,
    #[cfg(feature = "marks")]
    marks: marks::Marks,

    // ---- The search ----
    /// The search field takes the focus when it is next drawn (/).
    focus_search: bool,
    /// File name search above the file list, and the matching files best first while it has
    /// at least three letters.
    search: String,
    matches: Option<Vec<usize>>,
    /// The rows among the matches of the shown file and of the one a slideshow of several panes
    /// steps from, as (file, row): found once per image and search, not in every frame, as the
    /// matches can be 300,000 long.
    match_rows: std::cell::Cell<[Option<FileRow>; 2]>,
    /// Total size of the search matches, summed in the background like `total_size`.
    match_size: u64,
    /// Why a re: search pattern is not one, shown under the search field.
    search_error: Option<String>,
    match_rx: Option<mpsc::Receiver<u64>>,

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
    size_rx: Option<mpsc::Receiver<u64>>,
    /// Bytes freed by deletions in the background.
    freed_tx: mpsc::Sender<u64>,
    freed_rx: mpsc::Receiver<u64>,

    // ---- File operations in the background ----
    /// A Rename Many running in the background.
    renaming: Option<(mpsc::Receiver<Result<(), String>>, RenameJob)>,
    /// Renames found cut short at the start (a crash or a power cut during Rename Many), to put
    /// right; read in the background.
    cut_short_rx: Option<mpsc::Receiver<Vec<journal::CutShort>>>,
    cut_short: Vec<journal::CutShort>,
    /// Work still running in the background, for the busy pointer: deletes, copies and moves,
    /// printing, Check Images, Rename Many, the Lightroom import, plugins and every `background`.
    busy: Arc<AtomicUsize>,
    /// File operations running, for the status bar; one is over when its thread let go of it.
    file_jobs: Vec<Arc<Job>>,
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
    /// Files a plugin may have edited: shown again. Not `saved_tx`, whose report ends the
    /// guard of a turned image being saved.
    edited_tx: mpsc::Sender<PathBuf>,
    edited_rx: mpsc::Receiver<PathBuf>,
    /// The files Rename or a delete asked about, by their names in the list, and where the list
    /// was (`ask_about_targets`): the answer acts on those files, wherever a refresh put them.
    asked_for: (u64, Vec<OsString>),
    /// The sort manager's folders, kept in `$XDG_CONFIG_HOME/glimt/sort-folders`.
    sort_folders: Vec<PathBuf>,
    /// Our own clipboard for file lists, kept so it can serve them (X11).
    clipboard: Option<arboard::Clipboard>,
    /// An image decoded for the clipboard; egui takes it only during a frame.
    clip_rx: Option<mpsc::Receiver<egui::ColorImage>>,

    // ---- Dialogs, windows and messages ----
    /// "Open With" applications by file extension, found once per extension.
    /// `None` while `apps_for` runs in the background.
    apps: HashMap<String, Option<Vec<(String, PathBuf)>>>,
    apps_tx: mpsc::Sender<(String, Vec<(String, PathBuf)>)>,
    apps_rx: mpsc::Receiver<(String, Vec<(String, PathBuf)>)>,
    /// Edit → Plugins, read at start.
    plugins: Vec<plugins::Plugin>,
    /// Edit → Preferences is open.
    preferences_open: bool,
    /// Preferences changed while the pointer is down: saved when it is let go.
    preferences_unsaved: bool,
    /// The help page open (F1), if any.
    help: Option<help::Page>,
    /// Open text dialog and the text typed so far.
    prompt: Option<(Prompt, String)>,
    /// The delete confirmation is open: `Some(true)` = move to trash, `Some(false)` = permanently.
    confirm_delete: Option<bool>,
    /// The folder a folder's context menu acts on: to rename, or, with `confirm_folder`, to trash.
    folder_target: Option<PathBuf>,
    confirm_folder: bool,
    print_window: Option<PrintDialog>,
    /// A Lightroom catalog being read, then imported with the folders where its root folders
    /// are now.
    lightroom_rx: Option<mpsc::Receiver<Result<lightroom::Catalog, String>>>,
    lightroom: Option<lightroom::Import>,
    /// Answer from an open file dialog (zenity or kdialog).
    file_dialog: Option<(Pick, mpsc::Receiver<Option<PathBuf>>)>,
    /// The pan view's window (Ctrl+J), while it is open.
    pan_window: Option<Pan>,
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
    /// A `--remote select` that came while the folder was being read or Delete or Rename asked:
    /// done once neither is.
    waiting_select: Option<(String, remote::Reply)>,
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
    bytes.map_or(tr("calculating…").to_owned(), human_size)
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

/// `text` as it is if it `fits`, else its start and end around "…", as long as fits; found by
/// halving, so a row lays out a few texts, not one for each letter.
fn middle_cut(text: &str, fits: impl Fn(&str) -> bool) -> String {
    if fits(text) {
        return text.to_owned();
    }
    let chars: Vec<char> = text.chars().collect();
    let cut = |keep: usize| -> String {
        let tail = keep / 2;
        let head: String = chars[..keep - tail].iter().collect();
        let end: String = chars[chars.len() - tail..].iter().collect();
        format!("{head}…{end}")
    };
    let (mut lo, mut hi) = (0, chars.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if fits(&cut(mid)) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    cut(lo)
}

/// `n` and `noun` in the singular or plural, with thousands separated: "1 image", "1,200 images".
fn count(n: usize, noun: &'static str) -> String {
    format!("{} {}", thousands(n), lang::plural(noun, n))
}

/// A file of the list and its row among the search matches (`None`: not among them).
type FileRow = (usize, Option<usize>);

/// (index, full resolution, path, page, the job's number in `pending`).
/// Index, full resolution, path, page, job number, and the box to shrink into (sent with the
/// job, so a decode is at the size asked for when it was asked, whatever the view did since).
type DecodeJob = (usize, bool, PathBuf, usize, u64, [u32; 2]);

/// The decode workers: `workers` threads that take jobs and send back what they decoded,
/// skipping images the list has moved past (`keep`).
fn spawn_decoders(
    ctx: &egui::Context,
    workers: usize,
    keep: &Arc<Mutex<Vec<usize>>>,
    full_ok: &Arc<AtomicUsize>,
    rotate: &Arc<AtomicBool>,
) -> (mpsc::Sender<DecodeJob>, mpsc::Receiver<Decoded>) {
    let (jobs, job_rx) = mpsc::channel::<DecodeJob>();
    let (done_tx, done) = mpsc::channel();
    let job_rx = Arc::new(Mutex::new(job_rx));
    for _ in 0..workers {
        let (keep, full_ok, rotate, job_rx, done_tx, ctx) = (
            keep.clone(),
            full_ok.clone(),
            rotate.clone(),
            job_rx.clone(),
            done_tx.clone(),
            ctx.clone(),
        );
        std::thread::spawn(move || {
            loop {
                let Ok((idx, full, path, page, job, max)) = job_rx.lock().unwrap().recv() else {
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
                        path,
                        page,
                        job,
                        result: None,
                    });
                    continue;
                }
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
                    path,
                    page,
                    job,
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
    (jobs, done)
}

/// The thumbnail workers, one for each core of which CPU use lets `thumb_threads(workers)`
/// take jobs: so scrolling the list never delays the shown image.
#[allow(clippy::type_complexity)]
fn spawn_thumbnailers(
    ctx: &egui::Context,
    workers: usize,
    rotate: &Arc<AtomicBool>,
    view: &ThumbView,
    save: &Arc<AtomicBool>,
) -> (
    mpsc::Sender<(usize, PathBuf)>,
    mpsc::Receiver<(PathBuf, Option<Option<egui::ColorImage>>)>,
) {
    let (thumb_jobs, thumb_rx) = mpsc::channel::<(usize, PathBuf)>();
    let (thumbs_tx, thumbs_done) = mpsc::channel();
    let thumb_rx = Arc::new(Mutex::new(thumb_rx));
    // As many as there are cores; CPU use (thumb_threads) says how many take jobs, so it
    // changes without a restart.
    for k in 0..cores() {
        let (rx, tx, view, rotate, save, ctx) = (
            thumb_rx.clone(),
            thumbs_tx.clone(),
            view.clone(),
            rotate.clone(),
            save.clone(),
            ctx.clone(),
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
                let wanted = (view.lock().unwrap().iter()).any(|v| v.binary_search(&idx).is_ok());
                let result = wanted.then(|| {
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
    (thumb_jobs, thumbs_done)
}

/// The EXIF reader: one thread that skips to the newest request, so fast browsing never
/// queues up; it sends the panel's rows, every field, and the keywords and comment.
fn spawn_exif_reader(ctx: &egui::Context) -> (mpsc::Sender<PathBuf>, mpsc::Receiver<ExifRead>) {
    let (exif_jobs, paths_rx) = mpsc::channel::<PathBuf>();
    let (exif_tx, exif_rx) = mpsc::channel();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        while let Ok(mut path) = paths_rx.recv() {
            path = paths_rx.try_iter().last().unwrap_or(path);
            let size = std::fs::metadata(&path).map_or(0, |m| m.len());
            let mut rows = vec![("File size", human_size(size))];
            let (mut all, mut meta, mut paths, mut access) = Default::default();
            // Not opened unless it is a file: a pipe would block this thread for good. A decoder
            // that panics on the file leaves the panel without EXIF for it, not for good.
            if decode::is_file(&path) {
                let read = std::panic::catch_unwind(|| {
                    let mut rows = vec![("Colour profile", colour_source(&path))];
                    let (exif_rows, all) = exif::read_and_all(&path);
                    rows.extend(exif_rows);
                    let (meta, paths, access) = meta::read_all(&path);
                    (rows, all, meta, paths, access)
                });
                if let Ok((more, a, m, p, x)) = read {
                    rows.extend(more);
                    (all, meta, paths, access) = (a, m, p, x);
                }
            }
            let read = ExifRead {
                path,
                rows,
                all,
                meta,
                paths,
                access,
            };
            if exif_tx.send(read).is_err() {
                return;
            }
            ctx.request_repaint();
        }
    });
    (exif_jobs, exif_rx)
}

impl App {
    // ---- Starting up ----

    fn new(cc: &eframe::CreationContext, path: Option<PathBuf>, start: Start) -> Self {
        let keep = Arc::new(Mutex::new(vec![0]));
        let full_ok = Arc::new(AtomicUsize::new(1));
        let (mut config, problems) = Config::load_checked();
        lang::set(&config.language);
        // Include Subfolders starts off each time: kept on, every start in a large tree read all
        // of it before its folders showed.
        config.show_subfolders = false;
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
                let state = std::fs::read_to_string(state_path()?).ok()?;
                parse_last_dir(&state).filter(|d| {
                    config.remember_dir && (d.is_dir() || collections::is_collection(d))
                })
            })
            .unwrap_or_else(|| ".".into());
        let preload = Arc::new(AtomicUsize::new(config.preload));
        let rotate = Arc::new(AtomicBool::new(config.exif_rotate));
        MAX_ANIMATION.store(config.animation_mb << 20, Ordering::Relaxed);
        set_cpu_use(config.cpu_use);
        let workers = cores().min(config.decode_threads);
        let (jobs, done) = spawn_decoders(&cc.egui_ctx, workers, &keep, &full_ok, &rotate);
        // Thumbnails have their own workers, so scrolling the list never delays the shown image.
        let thumb_view: ThumbView = Arc::default();
        let save_thumbs = Arc::new(AtomicBool::new(config.save_thumbnails));
        let (thumb_jobs, thumbs_rx) =
            spawn_thumbnailers(&cc.egui_ctx, workers, &rotate, &thumb_view, &save_thumbs);
        let (exif_jobs, exif_rx) = spawn_exif_reader(&cc.egui_ctx);
        theme::fonts(&cc.egui_ctx);
        theme::apply(&cc.egui_ctx, &theme::load(&config.theme), None, None);
        // The size comes from Preferences and the desktop; egui's own Ctrl+0 would undo it.
        cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);
        cc.egui_ctx.set_zoom_factor(config.ui_scale as f32 / 100.0);
        // gdbus takes a moment; the window shows meanwhile and takes the desktop's look after.
        let (desktop_tx, desktop_rx) = mpsc::channel();
        let ctx = cc.egui_ctx.clone();
        std::thread::spawn(move || {
            let _ = desktop_tx.send(theme::desktop());
            ctx.request_repaint();
        });
        let (notices_tx, notices) = mpsc::channel();
        let (tree_tx, tree_rx) = mpsc::channel();
        let (apps_tx, apps_rx) = mpsc::channel();
        let (remote_tx, remote_rx) = mpsc::channel();
        // A name is only of use with remote control, so it listens anyway.
        if config.remote_control || remote::given_name().is_some() {
            remote::listen(remote_tx, cc.egui_ctx.clone());
        }
        let (freed_tx, freed_rx) = mpsc::channel();
        let (deleted_tx, deleted_rx) = mpsc::channel();
        let (saved_tx, saved_rx) = mpsc::channel();
        let (changed_tx, changed_rx) = mpsc::channel();
        let (edited_tx, edited_rx) = mpsc::channel();
        let mut app = App {
            notices_tx,
            mounting_rx: None,
            login: None,
            print_window: None,
            lightroom_rx: None,
            lightroom: None,
            cut_short_rx: Some({
                let (tx, rx) = mpsc::channel();
                std::thread::spawn(move || {
                    let _ =
                        tx.send(journal::dir().map_or_else(Vec::new, |d| journal::interrupted(&d)));
                });
                rx
            }),
            cut_short: Vec::new(),
            renaming: None,
            keyword_tree: (keyword_tree_path().and_then(|p| std::fs::read_to_string(p).ok()))
                .map_or_else(Vec::new, |t| meta::keyword_paths(t.lines())),
            new_keyword: String::new(),
            sort_folders: (sort_folders_path().and_then(|p| std::fs::read(p).ok()))
                .map_or_else(Vec::new, |t| sort_folders_from(&t)),
            pan_window: None,
            files_gen: 0,
            asked_for: (0, Vec::new()),
            grid_cols: 1,
            compare: false,
            panes: Vec::new(),
            next_pane: 0,
            last_slide: 0,
            capturing: None,
            recursive: false,
            collection: None,
            folders: Vec::new(),
            together: None,
            sort_collection: false,
            dups: None,
            check: None,
            broken_files: HashSet::new(),
            dragging: None,
            whole_shuffle: false,
            #[cfg(feature = "marks")]
            marks: marks::Marks::load(),
            tree: HashMap::new(),
            tree_tx,
            tree_rx,
            tree_shown: PathBuf::new(),
            clip_rx: None,
            clipboard: None,
            remote_rx,
            has_focus: true,
            apps: HashMap::new(),
            apps_tx,
            desktop_look: Default::default(),
            desktop_rx: Some(desktop_rx),
            desktop_screen: None,
            desktop_screen_rx: (config.desktop_profile).then(|| read_desktop_screen(&cc.egui_ctx)),
            plugins: plugins::find(),
            apps_rx,
            row_step: 1,
            notices_rx: notices,
            notice: screen_error.or_else(|| config_notice(&problems)),
            dir: PathBuf::new(),
            subdirs: Vec::new(),
            files: Vec::new(),
            cur: 0,
            keep,
            full_ok,
            preload,
            rotate,
            preferences_open: false,
            preferences_unsaved: false,
            help: None,
            confirm_delete: None,
            folder_target: None,
            confirm_folder: false,
            opened_type: Types::NONE,
            prompt: None,
            selected: Default::default(),
            rubber_band: None,
            last_drawn: None,
            fade: None,
            last_scale: 1.0,
            wheel: 0.0,
            config,
            slideshow: None,
            slideshow_at_start: start.slideshow,
            shuffled: false,
            view_box: [1920, 1080],
            view_area: [1920, 1080],
            max_side: 8192,
            decode_jobs: jobs,
            page: 0,
            frame: None,
            decoded_rx: done,
            slots: HashMap::new(),
            decodes_pending: HashMap::new(),
            decodes_sent: 0,
            stale_thumbs: HashSet::new(),
            decode_errors: HashMap::new(),
            zoom: None,
            pan_offset: egui::Vec2::ZERO,
            started: Some(Instant::now()),
            frame_ms: [0; 256],
            file_dialog: None,
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
            paused: false,
            back: Vec::new(),
            forward: Vec::new(),
            subdirs_rx: None,
            part_rx: None,
            list_parts: false,
            went_to: None,
            listing_stop: Arc::default(),
            listing_rx: None,
            image_id: None,
            focus_by_keys: false,
            waiting_select: None,
            total_size: 0,
            size_rx: None,
            freed_tx,
            freed_rx,
            selected_size: None,
            selected_rx: None,
            selected_size_due: None,
            scroll_to_cur: true,
            thumbs: HashMap::new(),
            thumbs_pending: HashSet::new(),
            thumb_jobs,
            thumbs_rx,
            thumb_view,
            pan_thumbs: Vec::new(),
            list_rows: 0..0,
            save_thumbs,
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
            inhibitor: None,
            inhibit_on: false,
            details: HashMap::new(),
            deleting: HashSet::new(),
            deleted_tx,
            deleted_rx,
            saved_tx,
            saved_rx,
            edited_tx,
            edited_rx,
            saving: HashSet::new(),
            busy: Arc::new(AtomicUsize::new(0)),
            file_jobs: Vec::new(),
            closing: 0,
            search: String::new(),
            matches: None,
            match_rows: Default::default(),
            match_size: 0,
            search_error: None,
            match_rx: None,
            exif_index: None,
            exif_stale: HashSet::new(),
            exif_index_rx: None,
            anchor: None,
            orient: (0, false),
            turned: PathBuf::new(),
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

    // ---- Opening a folder, a collection or several folders, and listing it ----

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
        self.forget_old_list();
        // Another folder: its file of the same name is another image, which opens as one, not
        // at the zoom and place of the image before.
        if dir != self.dir {
            self.files.clear();
        }
        self.recursive = self.config.show_subfolders;
        self.dir = dir.clone();
        self.replace_files(ctx, shown.iter().cloned().collect(), shown);
        self.list_parts = true;
        self.list(ctx);
        self.watch(ctx);
        self.title(ctx);
    }

    /// What belongs to the list shown before, which a new folder, collection or set of folders
    /// starts without.
    fn forget_old_list(&mut self) {
        // A check still running finds names of the old list: stopped, its window closed.
        if let Some(Some(check)) = self.check.take() {
            check.stop.store(true, Ordering::Relaxed);
        }
        self.broken_files.clear();
        self.subdirs.clear();
        self.deleting.clear();
        self.shuffled = false;
        self.thumbs.clear();
        self.thumbs_pending.clear();
        self.exif_index = None;
        self.exif_stale.clear();
        self.exif_index_rx = None;
    }

    /// Show the collection in `file` as the list.
    fn open_collection(&mut self, ctx: &egui::Context, file: PathBuf) {
        let paths = match collections::load(&file) {
            Ok(paths) => paths,
            Err(e) => {
                return self.notice = Some(format!("{display}: {e}", display = file.display()));
            }
        };
        self.leave_image(ctx);
        let root = Path::new("/");
        let files: Vec<OsString> = (paths.iter())
            .filter_map(|p| Some(p.strip_prefix(root).ok()?.as_os_str().to_owned()))
            .collect();
        // The same collection read again (F5): a delete still running keeps its names, so a
        // failed one brings its images back.
        let deleting = if self.collection.as_ref() == Some(&file) {
            std::mem::take(&mut self.deleting)
        } else {
            HashSet::new()
        };
        self.forget_old_list();
        self.deleting = deleting;
        self.recursive = false;
        // No folder to watch: stop the watcher of the last one.
        self.watching.fetch_add(1, Ordering::Relaxed);
        self.dir = root.to_path_buf();
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
        // As a folder opened: with the subfolders when the menu says so, whatever was shown.
        self.recursive = self.config.show_subfolders;
        self.forget_old_list();
        // No one folder to watch: stop the watcher of the last one.
        self.watching.fetch_add(1, Ordering::Relaxed);
        self.dir = PathBuf::from("/");
        self.folders = folders;
        self.replace_files(ctx, Vec::new(), None);
        self.list(ctx);
        self.title(ctx);
    }

    /// Where Back comes back to: the collection shown, else the file shown or the folder.
    fn back_step(&self) -> PathBuf {
        self.collection.clone().unwrap_or_else(|| self.location())
    }

    /// Open `path` as a new step in the Back/Forward history.
    fn visit(&mut self, ctx: &egui::Context, path: PathBuf) {
        // A message about the folder before (a share that could not be mounted) goes with it.
        self.notice = None;
        self.back.push(self.back_step());
        // Not without end: a slideshow of folders, or a script's --remote open, visits many.
        if self.back.len() > 200 {
            self.back.remove(0);
        }
        self.forward.clear();
        self.open(ctx, path);
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
        // Says which address is tried; replaces what an attempt before left.
        self.notice = Some(tf!("Connecting to {uri}…", uri = login.uri));
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = gio_mount(&login.uri, answer.then_some(&login));
            let _ = tx.send((result, login));
            ctx.request_repaint();
        });
        self.mounting_rx = Some(rx);
    }

    /// Read the folder (and then the sizes) in the background; `receive_listing` merges it in.
    /// A collection is checked for files that are gone instead.
    fn list(&mut self, ctx: &egui::Context) {
        let (tx, rx) = mpsc::channel();
        let (size_tx, size_rx) = mpsc::channel();
        let (list_dir, ctx2, types) = (self.dir.clone(), ctx.clone(), self.types());
        let c = &self.config;
        let (by, ascending, case) = (c.sort, c.sort_ascending, c.sort_case);
        let (recursive, shuffled) = (self.recursive, self.shuffled && self.whole_shuffle);
        let collection = self.collection.is_some().then(|| self.files.clone());
        // A collection keeps its own order unless the Sort menu was used.
        let sort = collection.is_none() || self.sort_collection;
        let folders = self.folders.clone();
        // The listing before stops, if it is still reading a tree.
        self.listing_stop.store(true, Ordering::Relaxed);
        self.listing_stop = Arc::default();
        let stop = self.listing_stop.clone();
        let (subdirs_tx, subdirs_rx) = mpsc::channel();
        self.subdirs_rx = Some(subdirs_rx);
        // Room for one: a window not drawn (minimized, on another workspace) takes none, and a
        // copy of the whole list so far each time would pile up.
        let (part_tx, part_rx) = mpsc::sync_channel(1);
        self.part_rx = Some(part_rx);
        // Only for a folder opened, or its tree asked for: a folder read again (a refresh) keeps
        // its whole list on screen until the new one is read, instead of shrinking and growing.
        // Nor in random order: a list in name order would take the shuffled one's place.
        let parts = std::mem::take(&mut self.list_parts) && !self.shuffled;
        self.went_to = None;
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
                        true => rel(d, list_tree(d, types, &stop, |_| {}, |_| {}).1),
                        false => rel(d, list_folder(d, types).1),
                    })
                    .collect();
                // A folder inside another one given has its images listed once.
                let mut seen = HashSet::new();
                files.retain(|f| seen.insert(f.clone()));
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
            // The folder read once, also with the folders below it. Its own subfolders show at
            // once: a large tree takes long to read whole.
            // Its images too, a folder of it at a time, in name order (the sort asked for comes
            // with the whole tree): at most four times a second, as each is a copy of the list.
            // The first images at once; after them, at most four times a second.
            let mut sent: Option<(Instant, std::time::Duration)> = None;
            let (dirs, mut files) = match recursive {
                true => list_tree(
                    &list_dir,
                    types,
                    &stop,
                    |dirs| {
                        if subdirs_tx.send(dirs.to_vec()).is_ok() {
                            ctx2.request_repaint();
                        }
                    },
                    |so_far| {
                        // The next one after four times as long as this one took, a quarter second
                        // at least: copying a long list must not slow the reading down.
                        let soon = sent.is_some_and(|(t, wait)| t.elapsed() < wait);
                        if !parts || shuffled || soon {
                            return;
                        }
                        let began = Instant::now();
                        let mut part = so_far.to_vec();
                        part.sort_unstable();
                        // Full: the one before is not taken yet; this one is left out.
                        if part_tx.try_send(part).is_ok() {
                            ctx2.request_repaint();
                        }
                        let wait = (began.elapsed() * 4).max(std::time::Duration::from_millis(250));
                        sent = Some((Instant::now(), wait));
                    },
                ),
                false => list_folder(&list_dir, types),
            };
            // Stopped: another folder is listed now.
            if stop.load(Ordering::Relaxed) {
                return;
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
                let sizes = walk(&list_dir, types).flat_map(|(rel, _, files)| {
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
                .filter(|e| files::kind(e).0 && is_listed(&e.path(), types));
            // Through a link, the size of the file it points to, as the list shows that.
            let sizes = images.filter_map(|e| Some(std::fs::metadata(e.path()).ok()?.len()));
            send_sizes(sizes, size_tx, &ctx2);
        });
        self.listing_rx = Some(rx);
        self.size_rx = Some(size_rx);
        self.total_size = 0;
        self.relist = false;
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

    /// Take the folder listing if it is ready, waiting up to `wait`.
    fn receive_listing(&mut self, ctx: &egui::Context, wait: std::time::Duration) {
        if let Some(Ok(dirs)) = self.subdirs_rx.as_ref().map(mpsc::Receiver::try_recv) {
            self.subdirs = dirs;
            self.subdirs_rx = None;
        }
        // The tree's images so far: only the last that came, one list a frame at most.
        if let Some(part) = self.part_rx.as_ref().and_then(|rx| rx.try_iter().last()) {
            self.take_files(ctx, part, false);
        }
        let Some(Ok((dirs, files))) = self.listing_rx.as_ref().map(|rx| rx.recv_timeout(wait))
        else {
            return;
        };
        self.subdirs_rx = None;
        self.part_rx = None;
        self.listing_rx = None;
        // Several folders shown together are listed as "/" with no subfolders: not the tree's.
        if self.collection.is_none() && self.folders.is_empty() {
            self.tree.insert(self.dir.clone(), Some(dirs.clone()));
        }
        self.subdirs = dirs;
        self.take_files(ctx, files, true);
    }

    /// The images of the folder read in the background as the list: all of them when `whole`,
    /// else those of a tree found so far. The shown image and the selection stay.
    fn take_files(&mut self, ctx: &egui::Context, files: Vec<OsString>, whole: bool) {
        // Files still being deleted in the background must not come back with a refresh.
        let mut files = files;
        if !self.deleting.is_empty() {
            files.retain(|f| !self.deleting.contains(f));
        }
        // A refresh during random order keeps the order, so Previous still goes back
        // through the images that were shown; a list shuffled whole in the thread stays as it is.
        // The flag is for this listing, taken whether or not the order is still random: left,
        // it would shuffle every later listing.
        let fresh = whole && std::mem::take(&mut self.whole_shuffle);
        let files = if whole && self.shuffled && !fresh {
            merge_shuffled(&self.files, files)
        } else {
            files
        };
        // Nothing changed (most refreshes, after Glimt's own writes beside the files): the list,
        // the selection and the search stay as they are.
        if files == self.files && !self.sort_collection {
            // A file saved over under its own name (an editor writes and renames): its size
            // and date read again, for the rows on screen only.
            self.details.clear();
            return;
        }
        // The image Glimt went to itself, in the name order of a tree's first images: the whole
        // list, in the order asked for, starts at its first image instead, unless another one
        // was chosen meanwhile.
        let mut shown = self.files.get(self.cur).cloned();
        if whole && shown.is_some() && std::mem::take(&mut self.went_to) == shown {
            shown = None;
        }
        let first = self.files.is_empty() && !whole;
        // A refresh keeps the selection: Delete or Move after it still acts on what was picked.
        let picked: HashSet<OsString> = (self.selected.iter())
            .filter_map(|&i| self.files.get(i).cloned())
            .collect();
        self.replace_files(ctx, files, shown);
        if first {
            self.went_to = self.files.get(self.cur).cloned();
        }
        if !picked.is_empty() {
            self.selected = (self.files.iter().enumerate())
                .filter(|(_, f)| picked.contains(*f))
                .map(|(i, _)| i)
                .collect();
            self.selection_changed(ctx);
        }
        if whole && std::mem::take(&mut self.sort_collection) {
            self.save_collection_order();
        }
    }

    /// Replace the file list and go to `shown` (otherwise the first image). Textures are kept
    /// for the files they were made from, at their new places. All other index-based state is
    /// reset; jobs in flight are rejected by path when they arrive. The same file still shown (a
    /// refresh, the list put in another order) keeps its zoom, turn and place.
    fn replace_files(
        &mut self,
        ctx: &egui::Context,
        files: Vec<OsString>,
        shown: Option<OsString>,
    ) {
        // Shown now and in the new list too: not when another program removed or renamed it,
        // whose turn not saved would else go to the image shown in its place.
        let found = (shown.as_ref()).and_then(|p| files.iter().position(|f| f == p));
        let still = found.is_some() && self.files.get(self.cur) == shown.as_ref();
        // The shown file's decodes still being made: kept, so it is not decoded twice. Not
        // those of a later page, as the file is shown again from its first.
        let jobs: Vec<_> = ([false, true].into_iter())
            .filter(|_| self.page == 0)
            .filter_map(|full| Some((full, self.decodes_pending.remove(&(self.cur, full))?)))
            .collect();
        if !still {
            self.leave_image(ctx);
        }
        // By name: the neighbours read ahead stay, after a refresh as after a delete.
        let mut slots: HashMap<OsString, Slot> = (self.slots.drain())
            .filter_map(|(i, s)| Some((self.files.get(i)?.clone(), s)))
            .collect();
        self.files = files;
        self.files_gen += 1;
        self.details.clear();
        self.anchor = None;
        self.selected.clear();
        self.selection_changed(ctx);
        // A rectangle or a collection drag holds places in the old list, now other files'.
        self.rubber_band = None;
        self.dragging = None;
        self.decodes_pending.clear();
        self.decode_errors.clear();
        let cur = found.unwrap_or(0);
        // One pass over the list for a handful of textures.
        let mut kept = Vec::new();
        for (i, name) in self.files.iter().enumerate() {
            if slots.is_empty() {
                break;
            }
            if let Some(slot) = slots.remove(name) {
                kept.push((i, slot));
            }
        }
        for (i, slot) in kept {
            if self.is_file(i, &slot.path) {
                self.slots.insert(i, slot);
            }
        }
        for (full, job) in jobs {
            if self.is_file(cur, &job.1) {
                self.decodes_pending.insert((cur, full), job);
            }
        }
        self.update_search(ctx);
        if still {
            self.cur = cur;
            self.refill();
            self.title(ctx);
            return;
        }
        // go() clears the notice for a new image; a new list is not one, and a notice is often
        // why the list was read again.
        let notice = self.notice.take();
        self.go(ctx, cur);
        self.notice = notice;
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
        self.decode_errors = without_indices(std::mem::take(&mut self.decode_errors), removed);
        // The panes' places are other files' now: filled again from the list as it is.
        self.panes.clear();
        self.selected.clear();
        self.anchor = None;
        self.rubber_band = None;
        self.dragging = None;
        self.update_search(ctx);
        // Jobs in flight have old indices and are rejected by path; refill requests them again.
        self.decodes_pending.clear();
        // The next remaining file after the shown one (or the last).
        let cur = self.cur - removed.partition_point(|&r| r < self.cur);
        if self.files.is_empty() {
            self.cur = 0;
            self.title(ctx);
        } else {
            self.go(ctx, cur.min(self.files.len() - 1));
        }
    }

    /// "image name - Glimt" (or "Glimt - image name", Preferences); the folder when it has no images.
    fn title(&self, ctx: &egui::Context) {
        let name = match (self.files.get(self.cur), &self.collection) {
            (Some(_), _) => self.name(self.cur).into_owned(),
            (None, Some(file)) => file.display().to_string(),
            (None, None) => self.dir.display().to_string(),
        };
        // A named Glimt says which it is.
        let app = match remote::given_name() {
            Some(n) => tf!("Glimt ({n})", n),
            None => "Glimt".to_owned(),
        };
        let title = if self.config.app_name_first {
            format!("{app} - {name}")
        } else {
            format!("{name} - {app}")
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
    }

    // ---- Collections ----

    /// Make `change` to the open collection's file as it is now, with what other windows changed.
    fn edit_collection(&mut self, change: impl FnOnce(&mut Vec<PathBuf>)) {
        let Some(file) = &self.collection else { return };
        // A plain list is the user's own: shown, never written.
        if collections::is_list(file) {
            return;
        }
        // shortcut: read and written in the UI thread, small text files; move to
        // config::write_later if collections of hundreds of thousands are edited.
        match collections::edit(file, false, change) {
            Ok(()) => {}
            // Renamed or moved to the trash in another window since it was opened here: not
            // made again under its old name.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.notice = Some(tf!(
                    "The collection {file} is gone (renamed or removed in another window): not saved",
                    file = file.display()
                ))
            }
            Err(e) => self.notice = Some(tf!("Could not save the collection: {e}", e)),
        }
    }

    /// The collection's file put in the order of the list, unless shuffled for a slideshow.
    fn save_collection_order(&mut self) {
        if self.shuffled {
            return;
        }
        let ours: Vec<PathBuf> = (0..self.files.len()).map(|i| self.path(i)).collect();
        self.edit_collection(|all| collections::reorder(all, &ours));
    }

    /// Move the selected images (or the shown one) of the open collection one place earlier or
    /// later, or to its start or end, and save its new order: a slideshow plays it so. Several
    /// selected keep their order among themselves; the shown image and the selection follow.
    // shortcut: the whole list put together again for each move, some milliseconds for 100,000
    // images; move the indices alone if collections that large are moved image by image.
    fn move_in_collection(
        &mut self,
        ctx: &egui::Context,
        picked: HashSet<usize>,
        order: Vec<usize>,
    ) {
        if self.collection.is_none() {
            self.notice = Some(tr("Only the images of a collection can be moved").into());
            return;
        }
        // An empty collection: nothing to move, and no shown image to keep.
        if self.files.is_empty() {
            return;
        }
        if self.shuffled {
            self.notice = Some(tr("Random order is on: R turns it off to move images").into());
            return;
        }
        let shown = self.files.get(self.cur).cloned();
        let names: HashSet<OsString> = picked.iter().map(|&i| self.files[i].clone()).collect();
        let files: Vec<OsString> = order.iter().map(|&i| self.files[i].clone()).collect();
        let selected = !self.selected.is_empty();
        self.replace_files(ctx, files, shown);
        if selected {
            self.selected = (self.files.iter().enumerate())
                .filter(|(_, f)| names.contains(*f))
                .map(|(i, _)| i)
                .collect();
            self.selection_changed(ctx);
        }
        self.save_collection_order();
    }

    /// The open collection's file to the trash, through gio in the background; the images stay,
    /// and the window shows the shown image's folder.
    fn trash_collection(&mut self, ctx: &egui::Context) {
        let Some(file) = self.collection.clone() else {
            return;
        };
        self.background(ctx, true, move || {
            let name = file
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            Some(
                match command("gio").arg("trash").arg("--").arg(&file).output() {
                    Ok(out) if out.status.success() => {
                        tf!("Collection {name} moved to the trash", name)
                    }
                    Ok(out) => tf!(
                        "Could not trash {name}: {err}",
                        name,
                        err = String::from_utf8_lossy(&out.stderr).trim()
                    ),
                    Err(e) => tf!("Could not trash {name} (gio is needed): {e}", name, e),
                },
            )
        });
        let folder = (!self.files.is_empty())
            .then(|| self.path(self.cur))
            .and_then(|p| p.parent().map(Path::to_path_buf));
        if let Some(folder) = folder.or_else(std::env::home_dir) {
            self.visit(ctx, folder);
        }
    }

    /// Add the selected files, or the shown image, to the collection in `file`.
    fn add_to_collection(&mut self, file: &Path) {
        let paths: Vec<PathBuf> = self.targets().iter().map(|&i| self.path(i)).collect();
        let name = file.file_stem().unwrap_or_default().to_string_lossy();
        self.notice = Some(match collections::append(file, &paths) {
            Ok(n) => tf!("{n} added to the collection \"{name}\"", n, name),
            Err(e) => tf!("Could not save the collection: {e}", e),
        });
    }

    /// File menu entries for collections; the ones listing collections read the folder only
    /// while open.
    fn collections_menu(&mut self, ui: &mut egui::Ui, act: &mut Option<Action>) {
        menu_item(ui, act, Action::NewCollection);
        menu_item(ui, act, Action::OpenCollection);
        menu_item(ui, act, Action::ImportLightroom);
        let mut open = None;
        scroll_menu(ui, tr("Collections"), |ui| {
            let all = collections::list();
            if all.is_empty() {
                ui.label(tr("No collections yet"));
            }
            for (name, file) in all {
                if ui.button(name).clicked() {
                    open = Some(file);
                }
            }
        });
        if !self.files.is_empty() {
            scroll_menu(ui, tr("Add to Collection"), |ui| {
                let all = collections::list();
                if all.is_empty() {
                    ui.label(tr("No collections yet"));
                }
                for (name, file) in all {
                    if ui.button(name).clicked() {
                        self.add_to_collection(&file);
                    }
                }
            });
        }
        if let Some(file) = &self.collection {
            menu_item(ui, act, Action::RemoveFromCollection);
            // A plain list is the user's own file: Glimt never writes it.
            if !collections::is_list(file) {
                menu_item(ui, act, Action::RenameCollection);
                menu_item(ui, act, Action::TrashCollection);
            }
            ui.menu_button(tr("Move in Collection"), |ui| {
                for action in [
                    Action::MoveEarlier,
                    Action::MoveLater,
                    Action::MoveFirst,
                    Action::MoveLast,
                ] {
                    menu_item(ui, act, action);
                }
            });
        }
        if let Some(file) = open {
            self.visit(ui.ctx(), file);
        }
    }

    // ---- Moving through the list, and the selection ----

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
            return format!("{folder}/{last}", last = last(path)).into();
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

    /// Show file `idx`: the image before is left (its turn saved or dropped), the zoom reset
    /// unless kept, and the images around the new one decoded.
    fn go(&mut self, ctx: &egui::Context, idx: usize) {
        if self.files.is_empty() {
            return;
        }
        // Browsing by hand shows the image at once; the slideshow sets a fade after this.
        self.fade = None;
        // A new image plays from its start.
        self.frame = None;
        // Saving the old image's rotation may leave a notice, which must outlive the move.
        let saved = self.config.save_orientation && self.orient != (0, false);
        self.leave_image(ctx);
        let notice = if saved { self.notice.take() } else { None };
        self.cur = idx.min(self.files.len() - 1);
        self.notice = notice;
        // A failure of before (a share away for a moment) is tried again now that the image is
        // shown again, not kept for the rest of the session.
        self.decode_errors.remove(&self.cur);
        // Geeqie's "keep zoom": the next image at the same zoom and place, to compare a series.
        if !self.config.keep_zoom {
            self.zoom = None;
            self.pan_offset = egui::Vec2::ZERO;
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

    /// The file `delta` rows from the shown one, in the order the list shows (the search matches
    /// while searching); clamped at the ends, or going round with `wrap`. A file the search
    /// hides counts as being before the first row.
    fn step(&self, delta: isize, wrap: bool) -> Option<usize> {
        self.step_from(self.cur, delta, wrap)
    }

    /// As `step`, from file `from`.
    fn step_from(&self, from: usize, delta: isize, wrap: bool) -> Option<usize> {
        let m = self.matches.as_deref();
        let n = m.map_or(self.files.len(), <[usize]>::len);
        let row = match m {
            Some(m) => self.row_of(m, from),
            None => Some(from),
        };
        let r = next_row(row, delta, n, wrap)?;
        Some(m.map_or(r, |m| m[r]))
    }

    /// The row of the shown file among the search matches `m`; `None` if the search hides it.
    fn cur_row(&self, m: &[usize]) -> Option<usize> {
        self.row_of(m, self.cur)
    }

    /// The row of `file` among the search matches `m`, kept for the shown file and one other.
    fn row_of(&self, m: &[usize], file: usize) -> Option<usize> {
        let mut rows = self.match_rows.get();
        if let Some((_, row)) = rows.iter().flatten().find(|r| r.0 == file) {
            return *row;
        }
        let row = m.iter().position(|&i| i == file);
        rows[(file != self.cur) as usize] = Some((file, row));
        self.match_rows.set(rows);
        row
    }

    /// The shown image's place in the list as shown: "[3/16]", "[–/12]" if a search hides it.
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

    /// The files the list shows: the search matches, or all.
    fn shown_files(&self) -> Vec<usize> {
        (self.matches.clone()).unwrap_or_else(|| (0..self.files.len()).collect())
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

    /// Called when the selection changes: update the title and compute the size in the background.
    fn selection_changed(&mut self, ctx: &egui::Context) {
        self.title(ctx);
        if self.compare {
            self.refill();
        }
        self.selected_size = None;
        self.selected_rx = None;
        // Not at once: Shift+↓ held in a selection of 500,000 changes it every frame, and each
        // time a path for every file would be made here.
        let wait = std::time::Duration::from_millis(200);
        self.selected_size_due = (!self.selected.is_empty()).then(|| Instant::now() + wait);
        ctx.request_repaint_after(wait);
    }

    /// The size of the selection, in the background, once it has stayed the same a moment.
    fn selection_size(&mut self, ctx: &egui::Context) {
        if self
            .selected_size_due
            .is_none_or(|due| Instant::now() < due)
        {
            return;
        }
        self.selected_size_due = None;
        if self.selected.is_empty() {
            return;
        }
        // All of them (Select All in a folder of 500,000): the folder's size, summed already.
        if self.selected.len() == self.files.len() && self.size_rx.is_none() {
            self.selected_size = Some(self.total_size);
            self.selected_rx = None;
            return;
        }
        // The paths made in the thread: only the names are copied here.
        let names: Vec<OsString> = self
            .selected
            .iter()
            .map(|&i| self.files[i].clone())
            .collect();
        let (dir, (tx, rx), ctx) = (self.dir.clone(), mpsc::channel(), ctx.clone());
        std::thread::spawn(move || {
            let paths: Vec<PathBuf> = names.iter().map(|n| dir.join(n)).collect();
            // The receiver is gone if the selection changed again; then the answer is dropped.
            if tx.send(total_size(&paths)).is_ok() {
                ctx.request_repaint();
            }
        });
        self.selected_rx = Some(rx);
    }

    fn receive_sizes(&mut self) {
        while let Ok(freed) = self.freed_rx.try_recv() {
            // shortcut: a delete while the sum is still being computed can make it slightly off;
            // fine for a status bar figure.
            self.total_size = self.total_size.saturating_sub(freed);
        }
        if let Some(Ok(size)) = self.selected_rx.as_ref().map(mpsc::Receiver::try_recv) {
            self.selected_size = Some(size);
            self.selected_rx = None;
        }
        drain(&mut self.match_rx, &mut self.match_size);
        drain(&mut self.size_rx, &mut self.total_size);
    }

    /// File operations act on the selected files, otherwise on the shown image. Sorted.
    fn targets(&self) -> Vec<usize> {
        if self.selected.is_empty() {
            vec![self.cur]
        } else {
            self.selected.iter().copied().collect()
        }
    }

    /// How many `targets` there are and the first, without listing them: for a dialog drawn in
    /// every frame, with 500,000 selected.
    fn first_target(&self) -> (usize, usize) {
        let first = self.selected.first().copied().unwrap_or(self.cur);
        (self.selected.len().max(1), first)
    }

    // ---- Decoding and painting the image ----

    /// Ask the decoders for file `idx` at the size shown, or at full resolution, unless it is
    /// there, failed or asked for already.
    fn request(&mut self, idx: usize, full: bool) {
        let have = self.slots.get(&idx).is_some_and(|s| s.full || !full);
        if !have
            && !self.decode_errors.contains_key(&idx)
            && !self.decodes_pending.contains_key(&(idx, full))
        {
            self.decodes_sent += 1;
            let path = self.path(idx);
            self.decodes_pending
                .insert((idx, full), (self.decodes_sent, path.clone()));
            // The neighbours are preloaded at their first page.
            let page = if idx == self.cur { self.page } else { 0 };
            let max = match full {
                true => [self.max_side; 2],
                false => self.decode_box(idx),
            };
            let _ = (self.decode_jobs).send((idx, full, path, page, self.decodes_sent, max));
        }
    }

    /// Drop textures outside the preload window, and full-resolution versions of images other
    /// than the shown one; request the missing ones.
    fn refill(&mut self) {
        if self.files.is_empty() {
            return;
        }
        self.fill_panes();
        // The shown image first, then its neighbours in the order the list shows.
        let (cur, ahead) = (self.cur, self.preload.load(Ordering::Relaxed) as isize);
        let mut window = vec![cur];
        // The compared image second: it may be loaded at full resolution too.
        let other = self.compare_other().filter(|&i| i != cur);
        window.extend(other);
        // The other panes, which may be zoomed too, and the images a slideshow shows in them next.
        window.extend(self.panes.iter().filter(|&&i| i != cur && Some(i) != other));
        let full = window.len();
        self.full_ok.store(full, Ordering::Relaxed);
        let n = self.panes.len() + 1;
        let upcoming = (1..=n as isize).filter_map(|d| self.step_from(self.last_slide, d, true));
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

    /// Decoded images into textures, those still wanted; why for those that could not be.
    fn receive_decoded(&mut self, ctx: &egui::Context) {
        while let Ok(d) = self.decoded_rx.try_recv() {
            // Not a job still asked for: asked for again, in another list, page or look. Its
            // place in the list is the job's now, as the list may have been read again since.
            let Some(&(idx, full)) = (self.decodes_pending.iter())
                .find(|(_, j)| j.0 == d.job)
                .map(|(k, _)| k)
            else {
                continue;
            };
            self.decodes_pending.remove(&(idx, full));
            let wanted = self.is_file(idx, &d.path)
                && d.rotated == self.config.exif_rotate
                && d.page == if idx == self.cur { self.page } else { 0 }
                && self.keep.lock().unwrap().contains(&idx)
                // Full resolution for each image on screen: the shown one, the compared one and
                // the panes of a layout, zoomed together.
                && (!full
                    || idx == self.cur
                    || Some(idx) == self.compare_other()
                    || self.panes.contains(&idx));
            match d.result {
                Some(Ok((frames, orig, hist, pages))) if wanted => {
                    // A screen-size version must not replace a full-resolution one already loaded.
                    if !full && self.slots.get(&idx).is_some_and(|s| s.full) {
                        continue;
                    }
                    let mut anim: Vec<_> = (frames.into_iter().enumerate())
                        .map(|(n, (img, ms))| {
                            let name = format!("img{} {n}", idx);
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
                        idx,
                        Slot {
                            tex,
                            path: d.path,
                            full,
                            orig,
                            anim,
                            hist,
                            pages,
                        },
                    );
                    // A full-resolution decode keeps fewer frames of a large animation (its
                    // memory budget): the frame stepped to stays among them.
                    let frames = self.slots.get(&idx).map_or(0, |s| s.anim.len());
                    if idx == self.cur && frames > 0 && self.frame.is_some_and(|k| k >= frames) {
                        self.frame = Some(frames - 1);
                    }
                    if idx == self.cur
                        && let Some(t) = self.started.take()
                    {
                        eprintln!("first image: {} ms", t.elapsed().as_millis());
                    }
                }
                Some(Err(e)) if wanted => {
                    self.decode_errors.insert(idx, e);
                }
                // Skipped by a worker that went by the place the job had when sent; the list
                // was read again meanwhile and moved the file to `idx`, still wanted there.
                // Asked for again, else nothing would: the previous image stayed on screen.
                None if !full
                    && self.is_file(idx, &d.path)
                    && !self.slots.contains_key(&idx)
                    && self.keep.lock().unwrap().contains(&idx) =>
                {
                    self.request(idx, false);
                }
                _ => {}
            }
        }
    }

    /// Decode the images and thumbnails again, to be shown in other colours.
    fn redecode(&mut self) {
        self.slots.clear();
        self.decodes_pending.clear();
        self.decode_errors.clear();
        self.thumbs.clear();
        self.stale_thumbs.extend(self.thumbs_pending.drain());
        self.last_drawn = None;
        self.refill();
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

    /// What to show while the shown image is decoded, and whether it is its thumbnail: that
    /// thumbnail when there is one, else the image shown before, so browsing does not flash
    /// black. Not the image before for a video, whose frame takes ffmpeg a while: another photo
    /// stood under its play button.
    fn placeholder(&self) -> Option<(egui::TextureHandle, [usize; 2], bool)> {
        let name = self.files.get(self.cur)?;
        if let Some(Some(t)) = self.thumbs.get(name) {
            return Some((t.clone(), t.size(), true));
        }
        if is_video(Path::new(name)) {
            return None;
        }
        self.last_drawn
            .as_ref()
            .map(|(t, orig)| (t.clone(), *orig, false))
    }

    /// The image shown next to the current one when comparing: the other of two selected
    /// files, else the next image.
    fn compare_other(&self) -> Option<usize> {
        if !self.compare {
            return None;
        }
        // Two selected: the other of them. Not collected first: Select All in a large folder
        // would copy the whole selection every frame.
        let mut two = self.selected.iter().copied();
        match (self.selected.len(), two.next(), two.next()) {
            (2, Some(a), Some(b)) => Some(if a == self.cur { b } else { a }),
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
        self.last_slide = self.panes.last().copied().unwrap_or(self.cur);
        self.next_pane = 0;
    }

    fn set_layout(&mut self, layout: &str) {
        self.config.layout = layout.to_owned();
        self.config.save();
        // Comparing is a layout of its own.
        self.compare = false;
        self.refill();
    }

    /// View → Layout: one image, two or four, and how a slideshow changes them.
    fn layout_menu(&mut self, ui: &mut egui::Ui, act: &mut Option<Action>) {
        scroll_menu(ui, tr("Layout"), |ui| {
            menu_item(ui, act, Action::NextLayout);
            ui.separator();
            for (key, label, _) in LAYOUTS {
                if ui.radio(self.config.layout == key, tr(label)).clicked() {
                    self.set_layout(key);
                }
            }
            if self.config.layout == "1" {
                return;
            }
            ui.separator();
            if ui
                .checkbox(&mut self.config.layout_names, tr("Show File Names"))
                .changed()
            {
                self.config.save();
            }
            ui.weak(tr("Slideshow in this layout:"));
            let turns = self.one_at_a_time();
            let mut chosen = turns;
            ui.radio_value(&mut chosen, false, tr("All Images Change Together"));
            ui.radio_value(&mut chosen, true, tr("One Image at a Time"));
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

    fn zoom_by(&mut self, factor: f32) {
        self.zoom = Some((self.zoom.unwrap_or(self.last_scale) * factor).clamp(0.02, 32.0));
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
        let look = (self.config.enlarge, self.config.fit_mode);
        let zoom = zoom.map(|z| (z, self.pan_offset));
        let (at, scale) = placed(rect, slot.orig, ppp, look, zoom);
        let uv = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
        let p = ui.painter().with_clip_rect(rect);
        let shown = match &fade {
            Some((t, old, orig)) => {
                let from = placed(rect, *orig, ppp, look, zoom).0;
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

    /// Full screen info in the top left corner, from the template in Preferences.
    fn overlay(&mut self, ui: &mut egui::Ui, area: egui::Rect) {
        self.want_exif();
        let path = self.path(self.cur);
        let meta = self
            .meta
            .as_ref()
            .filter(|m| m.path == path)
            .map(|m| &m.saved);
        let text = fill_template(&self.config.osd_template, |field| match field {
            "name" => self.name(self.cur).to_string(),
            "path" => path.display().to_string(),
            "position" => self.place(),
            "dimensions" => (self.slots.get(&self.cur))
                .map(|s| format!("{} × {}", s.orig[0], s.orig[1]))
                .unwrap_or_default(),
            "marks" => self.mark_text(self.cur),
            "keywords" => meta.map(|m| m.keywords.join(", ")).unwrap_or_default(),
            "comment" => meta.map(|m| m.comment.clone()).unwrap_or_default(),
            label => (self.exif.as_ref().filter(|e| e.0 == path))
                .and_then(|(_, rows)| rows.iter().find(|r| r.0 == label))
                .map(|r| r.1.clone())
                .unwrap_or_default(),
        });
        if text.is_empty() {
            return;
        }
        let p = ui.painter();
        let galley = p.layout_no_wrap(text, egui::FontId::proportional(15.0), egui::Color32::WHITE);
        let at = egui::Rect::from_min_size(
            area.min + egui::vec2(12.0, 12.0),
            galley.size() + egui::vec2(20.0, 12.0),
        );
        p.rect_filled(at, 6.0, egui::Color32::from_black_alpha(150));
        p.galley(at.min + egui::vec2(10.0, 6.0), galley, egui::Color32::WHITE);
    }

    /// Window → Projector: a second window, for another screen or a projector, with only the
    /// shown image, as the main window shows it: fitted, or when zoomed the same part of it,
    /// scaled to this window. Same process, so the textures are shared and nothing is decoded
    /// twice. Wayland lets no program put a window on a chosen screen: it is dragged there, and
    /// F or a double-click makes it full screen. Its keys browse the main window.
    fn projector_window(&mut self, ctx: &egui::Context) {
        let id = egui::ViewportId::from_hash_of("glimt projector");
        let builder = egui::ViewportBuilder::default()
            .with_title("Glimt Projector")
            .with_app_id(app_id())
            .with_inner_size([960.0, 600.0]);
        let mut act = None;
        let closed = ctx.show_viewport_immediate(id, builder, |ui, _| {
            let pctx = ui.ctx().clone();
            let full = pctx.input(|i| i.viewport().fullscreen).unwrap_or(false);
            let (f, esc) = pctx.input(|i| {
                let f = i.key_pressed(egui::Key::F) || i.key_pressed(egui::Key::F11);
                (f && i.modifiers.is_none(), i.key_pressed(egui::Key::Escape))
            });
            let key = Self::shortcut(&pctx, self.config.delete_key, self.collection.is_some())
                .filter(|a| KIOSK_ACTIONS.contains(a));
            act = key;
            let [r, g, b] = self.config.background.0;
            let frame = egui::Frame::NONE.fill(egui::Color32::from_rgb(r, g, b));
            egui::CentralPanel::default().frame(frame).show(ui, |ui| {
                let area = ui.max_rect();
                // A projector larger than the main view (full screen on a big screen): the
                // images decoded again to its size, which they were too small for.
                let ppp = pctx.pixels_per_point();
                let size = [(area.width() * ppp) as u32, (area.height() * ppp) as u32];
                if size != self.projector_size {
                    let old = std::mem::replace(&mut self.projector_size, size);
                    let had = [self.view_box[0].max(old[0]), self.view_box[1].max(old[1])];
                    let grown = size[0] > had[0] || size[1] > had[1];
                    // Only then: a projector smaller than the view shows the images it has, and
                    // dragging its edge decodes nothing again.
                    if grown {
                        self.slots.retain(|_, s| s.full);
                        self.decodes_pending.retain(|&(_, full), _| full);
                        self.refill();
                    }
                }
                let response = ui.allocate_rect(area, egui::Sense::click());
                if f || response.double_clicked() || (esc && full) {
                    pctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!full));
                }
                self.paint_projector(ui, area);
            });
            pctx.input(|i| i.viewport().close_requested())
        });
        if closed {
            self.projector = false;
            self.projector_size = [0, 0];
        }
        if let Some(action) = act {
            self.run(ctx, action);
        }
    }

    /// The shown image in the projector's `area`, as `view` showed it, and the pointer's ring.
    fn paint_projector(&self, ui: &egui::Ui, area: egui::Rect) {
        let Some(slot) = self.slots.get(&self.cur) else {
            return;
        };
        let ppp = ui.ctx().pixels_per_point();
        let turns = self.orient.0;
        let [w, h] = slot.orig.map(|n| n as f32);
        let (uw, uh) = if turns % 2 == 1 { (h, w) } else { (w, h) };
        let fit = fit_scale(area.size() * ppp, [uw, uh], self.config.fit_mode);
        let fit = if self.config.enlarge {
            fit
        } else {
            fit.min(1.0)
        };
        // Zoomed: the same part of the image, as large as this window allows.
        let (scale, pan) = match self.last_view {
            Some(v) if v.zoomed => {
                let k = (area.width() * ppp / (v.area.width() * v.ppp))
                    .min(area.height() * ppp / (v.area.height() * v.ppp));
                let scale = v.scale * k;
                (scale, v.pan * (scale / ppp) / (v.scale / v.ppp))
            }
            _ => (fit, egui::Vec2::ZERO),
        };
        let rect =
            egui::Rect::from_center_size(area.center() + pan, egui::vec2(w, h) * scale / ppp);
        let tex = self.texture(slot, ui.ctx());
        paint_turned(ui, tex, rect, self.orient, egui::Color32::WHITE);
        // Where the pointer is over the main window's image, to point things out.
        if self.config.projector_pointer
            && let Some(v) = self.last_view
            && let Some(at) = v.pointer
        {
            let at = area.center() + pan + at * (scale / ppp) / (v.scale / v.ppp);
            let p = ui.painter();
            p.circle_stroke(
                at,
                22.0,
                egui::Stroke::new(5.0, egui::Color32::from_black_alpha(160)),
            );
            p.circle_stroke(
                at,
                22.0,
                egui::Stroke::new(3.0, egui::Color32::from_rgb(255, 210, 0)),
            );
        }
    }

    /// The frame of the shown animation playing now.
    fn playing_frame(&self) -> usize {
        let slot = self.slots.get(&self.cur);
        match self.config.animate {
            true => slot.map_or(0, |s| s.frame_at(self.opened.elapsed()).0),
            false => 0,
        }
    }

    /// What `slot` shows now, in the main window and on the projector alike: the frame stepped
    /// to, the frame playing, or the image.
    fn texture<'a>(&self, slot: &'a Slot, ctx: &egui::Context) -> &'a egui::TextureHandle {
        match self.frame {
            Some(k) => slot.anim.get(k).map_or(&slot.tex, |f| &f.0),
            None if self.config.animate => slot.frame(self.opened.elapsed(), ctx),
            None => &slot.tex,
        }
    }

    /// The frame (or page) shown now: the one stepped to, else the page.
    fn shown_frame(&self) -> usize {
        let animated = self.slots.get(&self.cur).is_some_and(|s| s.anim.len() > 1);
        match self.frame {
            Some(k) => k,
            None if animated => self.playing_frame(),
            None => self.page,
        }
    }

    /// Frames, or pages, of the shown file: for the status bar and the File menu's entries.
    fn frame_count(&self) -> usize {
        (self.slots.get(&self.cur)).map_or(1, |s| s.anim.len().max(s.pages))
    }

    // ---- The slideshow ----

    /// Shuffle the file list (current image first) or sort it back, keeping the shown image.
    fn shuffle_list(&mut self, ctx: &egui::Context, on: bool) {
        if self.files.is_empty() {
            return;
        }
        self.leave_image(ctx);
        self.shuffled = on;
        if !on {
            // A collection back in its own order, which shuffling left only in its file.
            // shortcut: the file read in the UI thread; collections are small text files.
            if let Some(file) = self.collection.as_ref().filter(|_| !self.sort_collection)
                && let Ok(saved) = collections::load(file)
            {
                let shown = self.files.get(self.cur).cloned();
                let files: Vec<PathBuf> = (0..self.files.len()).map(|i| self.path(i)).collect();
                let files: Vec<OsString> = (collections::in_saved_order(&saved, files).iter())
                    .filter_map(|p| Some(p.strip_prefix(&self.dir).ok()?.as_os_str().to_owned()))
                    .collect();
                self.replace_files(ctx, files, shown);
                return;
            }
            // Back to the chosen sort order; listing keeps the shown image.
            self.list(ctx);
            return;
        }
        // A copy, not taken: the textures and the shown image's zoom are kept by the names in
        // the list as it is.
        let mut files = self.files.clone();
        let shown = files[self.cur].clone();
        files.swap(0, self.cur);
        shuffle(&mut files[1..]);
        // New panes from the new order: kept, a slideshow one image at a time would still stand
        // at the end of the round and shuffle again on every frame.
        self.panes.clear();
        self.replace_files(ctx, files, Some(shown));
    }

    /// Start the slideshow: with the subfolders when set, shuffled for random order.
    fn start_slideshow(&mut self, ctx: &egui::Context) {
        if self.config.slideshow_subfolders {
            // The images of the subfolders too, listed in the background; random order
            // shuffles them all together.
            self.recursive = true;
            self.shuffled = self.config.random;
            self.whole_shuffle = self.shuffled;
            self.list(ctx);
        } else if self.config.random != self.shuffled {
            self.shuffle_list(ctx, self.config.random);
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

    /// A random order ends with the slideshow: the list goes back to the chosen sort order,
    /// keeping the shown image.
    fn stop_slideshow(&mut self, ctx: &egui::Context) {
        self.slideshow = None;
        self.paused = false;
        if self.recursive != self.config.show_subfolders {
            self.recursive = self.config.show_subfolders;
            self.shuffled = false;
            self.list(ctx);
        } else if self.shuffled {
            self.shuffle_list(ctx, false);
        }
        self.title(ctx);
    }

    /// Switch image when the time is up. Waits for a preloaded image that is still
    /// decoding, so the slideshow never shows a black screen.
    fn slideshow_tick(&mut self, ctx: &egui::Context) {
        let Some(last) = self.slideshow else { return };
        // Waits while Rename or a delete asks about the shown image.
        if self.files.is_empty()
            || self.paused
            || self.prompt.is_some()
            || self.confirm_delete.is_some()
        {
            return;
        }
        // Several panes: the next set together, or the next image into one pane at a time.
        let n = self.panes.len() + 1;
        let turns = n > 1 && self.one_at_a_time();
        let (from, delta) = match n {
            1 => (self.cur, 1),
            _ if turns => (self.last_slide, 1),
            _ => (self.cur, n),
        };
        let delay = std::time::Duration::from_secs(self.config.delay);
        let left = delay.saturating_sub(last.elapsed());
        // The slideshow owns its clock: the next frame is asked for here, whatever this one
        // does, so that no move below can leave it standing still; held for a decode, it looks
        // again soon.
        ctx.request_repaint_after(left.max(std::time::Duration::from_millis(50)));
        if !left.is_zero() {
            return;
        }
        let next = self.step_from(from, delta as isize, true);
        let m = self.matches.as_deref();
        let row = m.map_or(Some(from), |m| self.row_of(m, from));
        let shown = m.map_or(self.files.len(), <[usize]>::len);
        let pending = next.is_some_and(|next| self.decodes_pending.contains_key(&(next, false)));
        let at = Place {
            row,
            delta,
            shown,
            len: self.files.len(),
        };
        match next_slide(
            at,
            next,
            self.config.slideshow_repeat,
            self.shuffled,
            pending,
        ) {
            Slide::Stop => self.stop_slideshow(ctx),
            Slide::Shuffle => self.shuffle_list(ctx, true),
            Slide::Hold => {}
            Slide::Step(next) => {
                let before = self.shown_textures();
                if turns && self.next_pane > 0 {
                    self.panes[self.next_pane - 1] = next;
                    self.refill();
                } else {
                    self.go(ctx, next);
                }
                self.fade = before.map(|b| (Instant::now(), b));
                if turns {
                    (self.last_slide, self.next_pane) = (next, (self.next_pane + 1) % n);
                }
                self.slideshow = Some(Instant::now());
            }
        }
    }

    /// The fade's length: none in power saving, where every frame counts.
    fn fade_ms(&self) -> u64 {
        match self.config.cpu_use {
            0 => 0,
            _ => FADES[self.config.fade].2,
        }
    }

    /// Random order changed in the menu or preferences: reshuffle now if a slideshow runs.
    fn set_random_order(&mut self, ctx: &egui::Context, on: bool) {
        self.config.random = on;
        self.config.save();
        if self.slideshow.is_some() {
            self.shuffle_list(ctx, on);
            self.title(ctx);
        }
    }

    // ---- The file list, folders and thumbnails ----

    /// The thumbnail box in the file list, by the size chosen in Preferences.
    fn thumb(&self) -> egui::Vec2 {
        THUMBS[self.config.thumb_size.min(2) as usize]
    }

    /// Thumbnails from the freedesktop "large" cache when the box is wider than a "normal" one
    /// on the screen: at 150 % scaling even the smallest box is 144 pixels wide. Read again when
    /// the scale or the size changes.
    fn sync_thumb_source(&mut self, ppp: f32) {
        let large = self.thumb().x * ppp > 128.0;
        if LARGE_THUMBS.swap(large, Ordering::Relaxed) != large {
            self.thumbs.clear();
            self.stale_thumbs.extend(self.thumbs_pending.drain());
        }
    }

    /// A cell of the icon grid: the thumbnail with its name below.
    fn cell(&self) -> egui::Vec2 {
        self.thumb() + CELL_EXTRA
    }

    fn set_thumb_size(&mut self, size: u8) {
        self.config.thumb_size = size;
        self.config.save();
        self.thumbs.clear();
        self.stale_thumbs.extend(self.thumbs_pending.drain());
        self.scroll_to_cur = true;
    }

    /// Thumbnails in the file list, or none.
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
        self.list_parts = on;
        self.list(ctx);
    }

    fn receive_thumbs(&mut self, ctx: &egui::Context) {
        while let Ok((path, result)) = self.thumbs_rx.try_recv() {
            // The name as the list has it: with its folders in a slideshow with subfolders.
            let Ok(name) = path.strip_prefix(&self.dir).map(Path::as_os_str) else {
                continue;
            };
            self.thumbs_pending.remove(name);
            // Made before a setting changed its look: asked for again, in the new one.
            if self.stale_thumbs.remove(name) {
                continue;
            }
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

    /// Drop thumbnails far from view once there are many, so memory stays flat in huge folders.
    /// Over the budget, drop the thumbnails of all but the rows `rows` of the list as shown (the
    /// search matches, or all files) and those near them.
    fn trim_thumbs(&mut self, rows: std::ops::Range<usize>) {
        // Big thumbnails take four times the memory: keep a quarter as many.
        let max = if LARGE_THUMBS.load(Ordering::Relaxed) {
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
        // Found broken by Check Images: a warning sign at the right.
        if self.broken_files.contains(&self.files[i]) {
            let c = egui::pos2(name_rect.max.x - 12.0, rect.center().y);
            broken_mark(ui.painter(), c, font.size);
            name_rect.max.x = c.x - 14.0;
        }
        // Cut in the middle, so the end of the name and its extension stay.
        let width = name_rect.width() - 4.0;
        let p = ui.painter();
        let fits = |s: &str| {
            p.layout_no_wrap(s.to_owned(), font.clone(), text_color)
                .size()
                .x
                <= width
        };
        let name = middle_cut(&self.name(i), fits);
        p.with_clip_rect(name_rect).text(
            egui::pos2(name_rect.min.x, rect.center().y),
            egui::Align2::LEFT_CENTER,
            name,
            font,
            text_color,
        );
        response
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
        // Found broken by Check Images: the warning sign on the thumbnail's top left.
        if self.broken_files.contains(&self.files[i]) {
            let size = egui::TextStyle::Body.resolve(ui.style()).size;
            broken_mark(
                ui.painter(),
                frame.left_top() + egui::vec2(10.0, 10.0),
                size,
            );
        }
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

    /// Thumbnail `i` fitted into `frame`, or a request for it.
    fn paint_thumb(&mut self, ui: &egui::Ui, i: usize, frame: egui::Rect) {
        let name = &self.files[i];
        match self.thumbs.get(name) {
            Some(Some(tex)) => {
                // Fit the box, but never enlarge a small image. One the cache cut down to its
                // size (256 pixels) does fill a box larger on a scaled screen.
                let px = tex.size_vec2() / ui.ctx().pixels_per_point();
                let cut = tex.size().into_iter().max() >= Some(256);
                // Fit the frame given: a narrow calendar day has less room than a list row.
                let fit = (frame.width() / px.x).min(frame.height() / px.y);
                let scale = if cut { fit } else { fit.min(1.0) };
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
        if !shown.is_empty() {
            let mut view = shown.clone();
            view.sort_unstable();
            self.thumb_view.lock().unwrap()[0] = view;
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

    /// Size and modification time of file `i`, from the cache or one stat.
    /// shortcut: stat in the UI thread, once per visible file; move to a thread if network
    /// folders make scrolling stutter.
    fn file_details(&mut self, i: usize) -> (u64, i64) {
        if let Some(&d) = self.details.get(&self.files[i]) {
            return d;
        }
        let d = std::fs::metadata(self.path(i)).map_or((0, 0), |m| {
            use std::os::unix::fs::MetadataExt;
            (m.len(), m.mtime())
        });
        // Bounded: scrolling through a folder of 500,000 would keep some 50 MB. Only the rows on
        // screen are read again.
        if self.details.len() >= 20_000 {
            self.details.clear();
        }
        self.details.insert(self.files[i].clone(), d);
        d
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

    /// Right-click on a folder in the list or the tree: rename it, or move it to the trash.
    fn folder_menu(&mut self, response: &egui::Response, dir: &Path) {
        response.context_menu(|ui| {
            // Its images in the list together with those shown now.
            let shown = self.folders.contains(&dir.to_path_buf()) || dir == self.dir;
            if !shown
                && self.collection.is_none()
                && ui.button(tr("Show Together with These Images")).clicked()
            {
                let mut all = match self.folders.is_empty() {
                    true => vec![self.dir.clone()],
                    false => self.folders.clone(),
                };
                all.push(dir.to_path_buf());
                self.together = Some(all);
            }
            if ui.button(tr("Rename Folder…")).clicked() {
                let name = dir.file_name().unwrap_or_default().to_string_lossy();
                self.prompt = Some((Prompt::RenameFolder, name.into_owned()));
                self.folder_target = Some(dir.to_path_buf());
            }
            if ui.button(tr("Move Folder to Trash…")).clicked() {
                self.folder_target = Some(dir.to_path_buf());
                self.confirm_folder = true;
            }
        });
    }

    /// The sort manager (Shift+S), as in Geeqie: a click on a folder copies or moves the selected
    /// files, or the shown one, there.
    fn sort_manager(&mut self, ui: &mut egui::Ui) {
        let mut remove = None;
        egui::Panel::right("sort manager")
            .resizable(true)
            .default_size(200.0)
            .show(ui, |ui| {
                ui.strong(tr("Sort Manager"));
                ui.horizontal(|ui| {
                    let c = &mut self.config;
                    let changed = ui
                        .radio_value(&mut c.sort_move, false, tr("Copy"))
                        .changed()
                        | ui.radio_value(&mut c.sort_move, true, tr("Move")).changed();
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
                                .on_hover_text(tr("Remove"))
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
                        if ui.button(tr("Add This Folder")).clicked() {
                            self.add_sort_folder(self.dir.clone());
                        }
                        if ui.button(tr("Add…")).clicked() {
                            self.pick(ui.ctx(), true, Pick::SortFolder);
                        }
                    });
                });
            });
        if let Some(k) = remove {
            let dir = self.sort_folders[k].clone();
            self.change_sort_folders(None, Some(&dir));
        }
    }

    fn add_sort_folder(&mut self, dir: PathBuf) {
        if !self.sort_folders.contains(&dir) {
            self.change_sort_folders(Some(&dir), None);
        }
    }

    /// Add a folder to the sort manager or take one out: the change goes to the file as it is
    /// now, so another window's folders stay, and is written whole or not at all.
    fn change_sort_folders(&mut self, add: Option<&Path>, remove: Option<&Path>) {
        if config::locked() {
            return self.notice = Some(tr("The settings are locked (--lock-settings)").into());
        }
        let Some(path) = sort_folders_path() else {
            return self.notice =
                Some(tr("Could not save the sort manager's folders: no home folder").into());
        };
        // Another window's read and write must not fall between ours, or its folder is lost.
        let _held = config::file_lock(&path);
        // One that is there but cannot be read is not written over: its folders would be lost.
        let on_disk = match std::fs::read(&path) {
            Ok(text) => sort_folders_from(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                return self.notice =
                    Some(tf!("Could not read the sort manager's folders: {e}", e));
            }
        };
        let dirs = config::sort_folders_changed(on_disk, add, remove);
        let _ = std::fs::create_dir_all(path.parent().unwrap_or(&path));
        match config::replace_file(&path, config::sort_folders_text(&dirs)) {
            Ok(()) => self.sort_folders = dirs,
            Err(e) => self.notice = Some(tf!("Could not save the sort manager's folders: {e}", e)),
        }
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
        let mut all = vec![(tr("Images"), &mut c.list_images), ("RAW", &mut c.list_raw)];
        if cfg!(feature = "video") {
            all.push((tr("Videos"), &mut c.videos));
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
        scroll_menu(ui, format!("{} ⏷", self.config.sort.row().2), |ui| {
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
                .checkbox(&mut self.config.sort_ascending, tr("Ascending"))
                .changed();
            changed |= ui
                .checkbox(&mut self.config.sort_case, tr("Case"))
                .changed();
        });
        if changed {
            self.config.save();
            // A chosen order replaces a shuffled one (kept after a random slideshow).
            self.shuffled = false;
            self.sort_collection = self.collection.is_some();
            self.list(ctx);
        }
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
                self.notice = Some(tf!("No such folder: {folder}", folder = path.display()));
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

    // ---- EXIF, keywords, comment and marks ----

    /// Ask the EXIF reader for the shown image (once per image) and take what has arrived.
    fn want_exif(&mut self) {
        let path = self.path(self.cur);
        if self.exif_asked.as_ref() != Some(&path) {
            let _ = self.exif_jobs.send(path.clone());
            self.exif_asked = Some(path);
        }
        let done: Vec<ExifRead> = self.exif_rx.try_iter().collect();
        for ExifRead {
            path,
            rows,
            all,
            meta,
            paths,
            access,
        } in done
        {
            // Kept while being edited: the reader may answer for the image again.
            if self.meta.as_ref().is_none_or(|m| m.path != path) {
                self.save_meta();
                self.meta = Some(MetaEdit {
                    path: path.clone(),
                    keywords: meta.keywords_text(),
                    comment: meta.comment.clone(),
                    saved: meta,
                    paths,
                    alt: access.alt.clone(),
                    extended: access.extended.clone(),
                    access,
                });
            }
            self.exif = Some((path, rows));
            self.exif_all = all;
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
                        ui.weak(tr("File"));
                        ui.label(self.name(self.cur));
                        ui.end_row();
                        if let Some(s) = self.slots.get(&self.cur) {
                            ui.weak(tr("Dimensions"));
                            ui.label(format!("{} × {}", s.orig[0], s.orig[1]));
                            ui.end_row();
                        }
                        // The old rows stay until the new ones arrive, so the panel does not flicker.
                        for (label, value) in self.exif.iter().flat_map(|e| &e.1) {
                            ui.weak(*label);
                            // The place opens on a map in the browser.
                            if *label == "GPS" {
                                let link = ui.link(value).on_hover_text(tr("Show on OpenStreetMap"));
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
                    self.keyword_tree(ui);
                });
            });
    }

    /// Keywords and comment of the shown image, as in Geeqie's info sidebar, with its alt text and
    /// extended description.
    fn meta_fields(&mut self, ui: &mut egui::Ui) {
        let path = self.path(self.cur);
        let Some(edit) = self.meta.as_mut().filter(|m| m.path == path) else {
            return;
        };
        ui.weak(tr("Keywords"));
        let keywords = ui.add(
            egui::TextEdit::singleline(&mut edit.keywords)
                .hint_text(tr("Separated by commas"))
                .desired_width(f32::INFINITY),
        );
        if std::mem::take(&mut self.focus_keywords) {
            keywords.request_focus();
        }
        ui.weak(tr("Comment"));
        let comment = ui.add(
            egui::TextEdit::multiline(&mut edit.comment)
                .desired_rows(3)
                .desired_width(f32::INFINITY),
        );
        // IPTC's accessibility fields, which screen readers and web pages read.
        ui.weak(tr("Alt text"));
        let alt = ui.add(
            egui::TextEdit::multiline(&mut edit.alt)
                .hint_text(tr("What the picture shows, for someone who cannot see it"))
                .desired_rows(2)
                .desired_width(f32::INFINITY),
        );
        ui.weak(tr("Extended description"));
        let extended = ui.add(
            egui::TextEdit::multiline(&mut edit.extended)
                .desired_rows(2)
                .desired_width(f32::INFINITY),
        );
        let add = Meta::parse_keywords(&edit.keywords);
        let n = self.selected.len();
        if n > 1
            && !add.is_empty()
            && ui
                .button(tf!(
                    "Add the keywords to the {n} selected",
                    n = thousands(n)
                ))
                .on_hover_text(tr("Each file keeps its own keywords and gets these too"))
                .clicked()
        {
            self.add_keywords(ui.ctx(), add);
        }
        if [keywords, comment, alt, extended]
            .iter()
            .any(egui::Response::lost_focus)
        {
            self.save_meta();
        }
    }

    /// The keyword tree as in Geeqie's sidebar: a box for each keyword, ticked when the shown
    /// image has it; a click sets it on the selected images (or the shown one), or takes it off.
    fn keyword_tree(&mut self, ui: &mut egui::Ui) {
        let path = self.path(self.cur);
        let Some((have, paths)) = (self.meta.as_ref())
            .filter(|m| m.path == path)
            .map(|m| (m.saved.keywords.clone(), m.paths.clone()))
        else {
            return;
        };
        ui.separator();
        ui.weak(tr("Keyword tree"));
        let mut root = KeywordNode::default();
        for p in &self.keyword_tree {
            root.insert(p);
        }
        let (mut tick, mut drop) = (None, None);
        root.show(ui, "", (&have, &paths), &mut tick, &mut drop);
        // The button first, from the right, so the field takes exactly what is left: a field
        // too wide makes the panel grow a little on every frame.
        let row = egui::vec2(ui.available_width(), ui.spacing().interact_size.y);
        let layout = egui::Layout::right_to_left(egui::Align::Center);
        ui.allocate_ui_with_layout(row, layout, |ui| {
            let add = ui.button(tr("Add")).clicked();
            let field = egui::TextEdit::singleline(&mut self.new_keyword)
                .hint_text(tr("Places|Norway|Oslo"))
                .desired_width(ui.available_width());
            let enter = ui.add(field).lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (add || enter) && !self.new_keyword.trim().is_empty() {
                let new = std::mem::take(&mut self.new_keyword);
                self.change_keyword_tree(vec![new], None);
            }
        });
        // The image's own keywords that the tree does not have yet, at its top.
        let names: HashSet<String> = (self.keyword_tree.iter())
            .map(|p| p.rsplit('|').next().unwrap_or(p).to_lowercase())
            .collect();
        let missing: Vec<String> = (have.iter())
            .filter(|k| !names.contains(&k.to_lowercase()))
            .cloned()
            .collect();
        if !missing.is_empty()
            && ui
                .small_button(tr("Add this image's keywords to the tree"))
                .clicked()
        {
            self.change_keyword_tree(missing, None);
        }
        if let Some(p) = drop {
            self.change_keyword_tree(Vec::new(), Some(p));
        }
        if let Some((tag, on)) = tick {
            self.tag_files(ui.ctx(), tag, on);
        }
    }

    /// Add `add` to the keyword tree in `$XDG_CONFIG_HOME/glimt/keywords`, or take `remove` and
    /// the paths below it out: this change alone, on the file as it is now, so what another
    /// window changed meanwhile stays.
    fn change_keyword_tree(&mut self, add: Vec<String>, remove: Option<String>) {
        let Some(p) = keyword_tree_path() else {
            self.keyword_tree = meta::keyword_tree_changed(&self.keyword_tree, add, remove);
            return;
        };
        let _held = config::file_lock(&p);
        // One that is there but cannot be read is not written over: its keywords would be lost.
        let on_disk = match std::fs::read_to_string(&p) {
            Ok(text) => text.lines().map(str::to_owned).collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return self.notice = Some(tf!("Could not read the keyword tree: {e}", e)),
        };
        self.keyword_tree = meta::keyword_tree_changed(&on_disk, add, remove);
        write_atomic(&p, self.keyword_tree.join("\n") + "\n");
    }

    /// Set the keyword `tag` (a tree path) on the selected images, or the shown one, or take it
    /// off, in the background; the shown image's keywords show it at once.
    fn tag_files(&mut self, ctx: &egui::Context, tag: String, on: bool) {
        self.save_meta();
        let targets = self.targets();
        if targets.contains(&self.cur)
            && let Some(edit) = &mut self.meta
        {
            meta::set_tag(&mut edit.saved.keywords, &mut edit.paths, &tag, on);
            edit.keywords = edit.saved.keywords_text();
        }
        // "exif:" searches find keywords too: only these files are read again.
        self.exif_stale
            .extend(targets.iter().filter_map(|&i| self.files.get(i).cloned()));
        let paths: Vec<PathBuf> = targets.iter().map(|&i| self.path(i)).collect();
        let (doing, verb) = match on {
            true => (tr("Setting keywords"), tr("set on")),
            false => (tr("Taking keywords off"), tr("taken off")),
        };
        let (n, written) = (paths.len(), tag.clone());
        self.write_sidecars(
            ctx,
            doing,
            paths,
            move |path| meta::tag_file(path, &written, on).is_ok(),
            move |failed, job| match () {
                _ if failed > 0 => Some(tf!(
                    "The keyword could not be saved for {count}",
                    count = count(failed, "file")
                )),
                _ if job.stopped() => Some(job.stopped_text()),
                _ if n > 1 => Some(format!("{tag} {verb} {count}", count = count(n, "file"))),
                _ => None,
            },
        );
    }

    /// Save the keywords, comment, alt text and extended description being edited, if they
    /// changed: in the thread that writes this window's sidecars, which closing waits for, not
    /// in the window's, as a sidecar on a slow share would hold the window up for its read, write and
    /// sync. The fields show what was saved at once; a failure is told.
    fn save_meta(&mut self) {
        let Some(edit) = &mut self.meta else { return };
        let access = meta::Access {
            alt: edit.alt.trim().to_owned(),
            extended: edit.extended.trim().to_owned(),
        };
        // Saved only when edited, as the keywords: browsing writes nothing.
        if (edit.alt != edit.access.alt || edit.extended != edit.access.extended)
            && access != edit.access
        {
            (edit.alt, edit.extended) = (access.alt.clone(), access.extended.clone());
            edit.access = access.clone();
            let (path, tx) = (edit.path.clone(), self.notices_tx.clone());
            file_ops::write_sidecar(move || {
                if let Err(e) = meta::write_access(&path, &access) {
                    let _ = tx.send(tf!("Could not save the alt text: {e}", e));
                }
            });
        }
        // Fields as they were shown are no change, also when splitting them again at commas
        // would read them differently ("Smith, John" as one keyword, a comment's last newline).
        if edit.keywords == edit.saved.keywords_text() && edit.comment == edit.saved.comment {
            return;
        }
        // Only what was edited: keywords set on this file in the background meanwhile stay.
        let keywords = (edit.keywords != edit.saved.keywords_text())
            .then(|| Meta::parse_keywords(&edit.keywords));
        let comment = (edit.comment != edit.saved.comment).then(|| edit.comment.trim().to_owned());
        if let Some(k) = &keywords {
            edit.saved.keywords = k.clone();
        }
        if let Some(c) = &comment {
            edit.saved.comment = c.clone();
        }
        edit.keywords = edit.saved.keywords_text();
        edit.comment = edit.saved.comment.clone();
        let (path, tx) = (edit.path.clone(), self.notices_tx.clone());
        let edited = path.clone();
        file_ops::write_sidecar(move || {
            if let Err(e) = meta::write_changed(&path, keywords, comment) {
                let _ = tx.send(tf!("Could not save the keywords: {e}", e));
            }
        });
        // "exif:" searches find keywords and comments too: the edited file, which may not be
        // the shown one any more, is read again when next used.
        if let Ok(name) = edited.strip_prefix(&self.dir) {
            self.exif_stale.insert(name.as_os_str().to_owned());
        }
    }

    /// Add `keywords` to every selected file, in the background.
    fn add_keywords(&mut self, ctx: &egui::Context, keywords: Vec<String>) {
        // The shown image's own edit first; it is read again afterwards, with the added ones.
        self.save_meta();
        self.meta = None;
        self.exif_asked = None;
        let targets = self.targets();
        self.exif_stale
            .extend(targets.iter().filter_map(|&i| self.files.get(i).cloned()));
        let paths: Vec<PathBuf> = self.targets().iter().map(|&i| self.path(i)).collect();
        let n = paths.len();
        // Through the sidecar writer, with keywords ticked and stars: in the order asked.
        self.write_sidecars(
            ctx,
            tr("Adding keywords"),
            paths,
            move |path| meta::add_keywords(path, &keywords).is_ok(),
            move |failed, job| match () {
                _ if failed > 0 => Some(tf!(
                    "Keywords could not be saved for {count}",
                    count = count(failed, "file")
                )),
                _ if job.stopped() => Some(job.stopped_text()),
                _ => Some(tf!("Keywords added to {count}", count = count(n, "file"))),
            },
        );
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

    // ---- The search ----

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
            let field = ui.add(edit.desired_width(width));
            if std::mem::take(&mut self.focus_search) {
                field.request_focus();
            }
            let mut changed = field.changed();
            if geeqie
                && ui
                    .checkbox(&mut self.config.search_case, tr("Case"))
                    .on_hover_text(tr("Tell upper and lower case apart"))
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
            ui.colored_label(ui.visuals().error_fg_color, tf!("Not a pattern: {e}", e));
        }
    }

    /// Run the search again; file indices change when the list does.
    fn update_search(&mut self, ctx: &egui::Context) {
        let query = self.search.trim().to_owned();
        let query = query.as_str();
        self.match_rows.set(Default::default());
        // A place "[n/m]" told for another pane: another search may have as many matches.
        self.status_place = None;
        self.search_error = None;
        self.matches = self.search_matches(ctx, query);
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

    /// The exif: search index once it is read, and the search again with it.
    fn receive_exif_index(&mut self, ctx: &egui::Context) {
        if let Some(index) = self
            .exif_index_rx
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
        {
            self.exif_index_rx = None;
            // The files read again go into the index there is; else this is the whole index.
            match &mut self.exif_index {
                Some(old) => old.extend(index),
                None => self.exif_index = Some(index),
            }
            self.update_search(ctx);
        }
    }

    /// The files a search finds: one term as it always was (fuzzy names best first), or terms
    /// combined (`sunset ( exif:canon | exif:nikon ) -re:png$`, see `search::parse`) in list
    /// order; `None` for no search.
    fn search_matches(&mut self, ctx: &egui::Context, query: &str) -> Option<Vec<usize>> {
        match search::parse(query)? {
            search::Query::Term(term) => self.term_matches(ctx, &term, 3),
            tree => Some(self.query_matches(ctx, &tree)),
        }
    }

    /// The files a part of a combined search finds, in order: an and keeps the order of its
    /// first part that is no "not", an or the order of its parts one after the other.
    fn query_matches(&mut self, ctx: &egui::Context, query: &search::Query) -> Vec<usize> {
        use search::Query;
        let n = self.files.len();
        let mask = |hits: &[usize]| {
            let mut mask = vec![false; n];
            hits.iter().for_each(|&i| mask[i] = true);
            mask
        };
        match query {
            Query::Term(term) => self.term_matches(ctx, term, 1).unwrap_or_default(),
            Query::Not(inner) => {
                let out = mask(&self.query_matches(ctx, inner));
                (0..n).filter(|&i| !out[i]).collect()
            }
            Query::Or(parts) => {
                let mut seen = vec![false; n];
                let mut found = Vec::new();
                for part in parts {
                    for i in self.query_matches(ctx, part) {
                        if !std::mem::replace(&mut seen[i], true) {
                            found.push(i);
                        }
                    }
                }
                found
            }
            Query::And(parts) => {
                let lead = parts.iter().position(|p| !matches!(p, Query::Not(_)));
                let mut found: Vec<usize> = match lead {
                    Some(at) => self.query_matches(ctx, &parts[at]),
                    None => (0..n).collect(),
                };
                for (at, part) in parts.iter().enumerate() {
                    if Some(at) == lead {
                        continue;
                    }
                    // A "not" left out directly; the others kept where both find.
                    let (inner, keep) = match part {
                        Query::Not(inner) => (inner.as_ref(), false),
                        part => (part, true),
                    };
                    let hit = mask(&self.query_matches(ctx, inner));
                    found.retain(|&i| hit[i] == keep);
                }
                found
            }
        }
    }

    /// The files one term of a search finds: `exif:`, `re:`, `mark:`, `stars:`, or the names
    /// (fuzzy from `min` letters, or a regular expression with fuzzy search off).
    fn term_matches(&mut self, ctx: &egui::Context, term: &str, min: usize) -> Option<Vec<usize>> {
        // Geeqie's Case checkbox, shown beside the field in its mode.
        let case = self.config.search_case && !self.config.fuzzy_search;
        let regex = |this: &mut Self, pattern: &str| match search::regex(pattern, case) {
            Ok(re) => search::regex_matches(&this.files, &re),
            Err(e) => {
                this.search_error = Some(e);
                Vec::new()
            }
        };
        match (term.strip_prefix("exif:"), term.strip_prefix("re:")) {
            (Some(words), _) => Some(self.exif_matches(ctx, words)),
            (_, Some(pattern)) if !pattern.is_empty() => Some(regex(self, pattern)),
            _ => {
                self.mark_matches(term)
                    .or_else(|| match self.config.fuzzy_search {
                        true => (term.chars().count() >= min)
                            .then(|| search::matches(&self.files, term)),
                        // As Geeqie's file filter: a regular expression, from the first letter.
                        false => (!term.is_empty()).then(|| regex(self, term)),
                    })
            }
        }
    }

    /// Files whose EXIF contains every word, or that a regular expression after "re:" finds, in
    /// list order. The first such search in a folder reads the EXIF of all files in the
    /// background (the busy pointer shows meanwhile) and finds nothing until that is done.
    /// shortcut: files added after the index was built do not match until the folder is
    /// reopened; add them to the index as the list grows if that is missed.
    fn exif_matches(&mut self, ctx: &egui::Context, words: &str) -> Vec<usize> {
        let re = match words.strip_prefix("re:").map(|p| search::regex(p, false)) {
            Some(Err(e)) => {
                self.search_error = Some(e);
                return Vec::new();
            }
            Some(Ok(re)) => Some(re),
            None => None,
        };
        // Files edited since: read again alone, while the search goes on with the index there is.
        if self.exif_index.is_some() && !self.exif_stale.is_empty() && self.exif_index_rx.is_none()
        {
            let (tx, rx) = mpsc::channel();
            let names: Vec<OsString> = self.exif_stale.drain().collect();
            let (dir, ctx) = (self.dir.clone(), ctx.clone());
            let pointer = Working::new(&self.busy);
            std::thread::spawn(move || {
                let _ = tx.send(search::exif_index(&dir, &names));
                drop(pointer);
                ctx.request_repaint();
            });
            self.exif_index_rx = Some(rx);
        }
        let Some(index) = &self.exif_index else {
            if self.exif_index_rx.is_none() {
                self.exif_stale.clear();
                let (tx, rx) = mpsc::channel();
                let (dir, files, ctx) = (self.dir.clone(), self.files.clone(), ctx.clone());
                let pointer = Working::new(&self.busy);
                std::thread::spawn(move || {
                    let _ = tx.send(search::exif_index(&dir, &files));
                    drop(pointer);
                    ctx.request_repaint();
                });
                self.exif_index_rx = Some(rx);
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
        let names = self.marked_names(hit);
        Some(
            (0..self.files.len())
                .filter(|&i| names.contains(self.files[i].as_os_str()))
                .collect(),
        )
    }

    /// The names in the list of the files whose (marks, stars) `hit` takes: from the marked
    /// files, a few thousand at most, not a path made for each of 500,000 at every key press.
    #[cfg(feature = "marks")]
    fn marked_names(&self, hit: impl Fn((u8, u8)) -> bool) -> HashSet<&std::ffi::OsStr> {
        (self.marks.marked())
            .filter(|&(_, v)| hit(v))
            .filter_map(|(p, _)| Some(p.strip_prefix(&self.dir).ok()?.as_os_str()))
            .collect()
    }

    #[cfg(not(feature = "marks"))]
    fn mark_matches(&self, _: &str) -> Option<Vec<usize>> {
        None
    }

    // ---- Windows: Check Images ----

    /// The Check Images window: starts the check of every image in the list, shows how far it
    /// is, and lists the broken ones; a click shows one, Select Them selects them in the list.
    fn check_window(&mut self, ctx: &egui::Context) {
        let Some(mut check) = self.check.take() else {
            return;
        };
        if let Some(c) = &mut check {
            for (name, why) in c.found_rx.try_iter() {
                self.broken_files.insert(name.clone());
                c.broken.push((name, why));
            }
        }
        let (mut open, mut start, mut show, mut select) = (true, false, None, false);
        egui::Window::new(tr("Check Images"))
            .open(&mut open)
            .default_size([560.0, 400.0])
            .show(ctx, |ui| {
                let Some(c) = &check else {
                    ui.label(format!(
                        "Opens each of the {} in the list, as the viewer would, and lists those \
                         that cannot be: broken files, files cut off in copying, and files that \
                         are no image. They are marked in the list too.",
                        count(self.files.len(), "image")
                    ));
                    start = ui.button(tr("Check")).clicked();
                    return;
                };
                let (done, working) = (
                    c.done.load(Ordering::Relaxed),
                    c.working.load(Ordering::Relaxed),
                );
                ui.horizontal(|ui| {
                    let progress = done as f32 / c.total.max(1) as f32;
                    let line = format!("{} of {}", thousands(done), thousands(c.total));
                    ui.add(
                        egui::ProgressBar::new(progress)
                            .text(line)
                            .desired_width(260.0),
                    );
                    if working > 0 {
                        if ui.button(tr("Stop")).clicked() {
                            c.stop.store(true, Ordering::Relaxed);
                        }
                        ctx.request_repaint_after(std::time::Duration::from_millis(200));
                    }
                });
                ui.label(match (c.broken.len(), working) {
                    (0, 0) => "None broken.".to_owned(),
                    (n, _) => tf!("{n} broken:", n = thousands(n)),
                });
                if !c.broken.is_empty() && ui.button(tr("Select Them")).clicked() {
                    select = true;
                }
                let row = ui.text_style_height(&egui::TextStyle::Body) + 4.0;
                egui::ScrollArea::vertical().auto_shrink(false).show_rows(
                    ui,
                    row,
                    c.broken.len(),
                    |ui, rows| {
                        for (name, why) in &c.broken[rows] {
                            let text = format!("{}: {why}", name.to_string_lossy());
                            if ui.selectable_label(false, text).clicked() {
                                show = Some(name.clone());
                            }
                        }
                    },
                );
            });
        if start {
            check = Some(self.start_check(ctx));
        }
        if let Some(name) = show
            && let Some(i) = self.files.iter().position(|f| *f == name)
        {
            self.go(ctx, i);
        }
        if select {
            self.selected = (self.files.iter().enumerate())
                .filter(|(_, f)| self.broken_files.contains(*f))
                .map(|(i, _)| i)
                .collect();
            self.selection_changed(ctx);
        }
        if open {
            self.check = Some(check);
        } else if let Some(c) = check {
            // Closed while it runs: the threads stop at their next file.
            c.stop.store(true, Ordering::Relaxed);
        }
    }

    /// Check every image of the list in background threads, as many as the CPU setting allows,
    /// each taking the next file until all are done or Stop is pressed.
    fn start_check(&mut self, ctx: &egui::Context) -> Check {
        self.broken_files.clear();
        let files: Arc<Vec<(OsString, PathBuf)>> = Arc::new(
            (0..self.files.len())
                .map(|i| (self.files[i].clone(), self.path(i)))
                .collect(),
        );
        let threads = background_threads().min(files.len().max(1));
        let (tx, found) = mpsc::channel();
        let next = Arc::new(AtomicUsize::new(0));
        let check = Check {
            done: Arc::default(),
            working: Arc::new(AtomicUsize::new(threads)),
            stop: Arc::default(),
            total: files.len(),
            found_rx: found,
            broken: Vec::new(),
        };
        for _ in 0..threads {
            let (files, tx, next, ctx) = (files.clone(), tx.clone(), next.clone(), ctx.clone());
            let (done, working, stop) = (
                check.done.clone(),
                check.working.clone(),
                check.stop.clone(),
            );
            let pointer = Working::new(&self.busy);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let Some((name, path)) = files.get(next.fetch_add(1, Ordering::Relaxed)) else {
                        break;
                    };
                    if let Err(why) = decode::check(path) {
                        let _ = tx.send((name.clone(), why));
                    }
                    done.fetch_add(1, Ordering::Relaxed);
                }
                working.fetch_sub(1, Ordering::Relaxed);
                drop(pointer);
                ctx.request_repaint();
            });
        }
        check
    }

    // ---- Settings and the menus that change them ----

    /// The chosen theme, read from its file again, with the desktop's look where it sets none;
    /// High Contrast while the desktop asks for it, unless that is turned off.
    fn apply_theme(&self, ctx: &egui::Context) {
        let look = self.desktop_look;
        let name = match look.high_contrast && self.config.follow_contrast {
            true => theme::HIGH_CONTRAST,
            false => &self.config.theme,
        };
        theme::apply(ctx, &theme::load(name), look.dark, look.accent);
        let desktop = look.text_scale.filter(|_| self.config.follow_text_scale);
        ctx.set_zoom_factor(self.config.ui_scale as f32 / 100.0 * desktop.unwrap_or(1.0));
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
        // Locked: no theme written, and no file manager or browser opened from here.
        if !config::locked() {
            ui.separator();
            self.theme_buttons(ui);
        }
    }

    /// Save as Theme…, Open Themes Folder and Get More Themes…, in View → Theme and Preferences.
    fn theme_buttons(&mut self, ui: &mut egui::Ui) {
        if ui.button(tr("Save as Theme…")).clicked() {
            self.prompt = Some((Prompt::SaveTheme, String::new()));
        }
        if ui.button(tr("Open Themes Folder")).clicked()
            && let Some(dir) = theme::dir()
        {
            let _ = std::fs::create_dir_all(&dir);
            open_url(&dir.to_string_lossy());
        }
        if ui.button(tr("Get More Themes…")).clicked() {
            open_url("https://github.com/thornbux/glimt/tree/main/themes");
        }
    }

    /// Preload `n` images on each side.
    fn set_preload(&mut self, n: usize) {
        self.preload.store(n, Ordering::Relaxed);
        self.refill();
        self.config.preload = n;
    }

    /// Decode everything again with or without EXIF rotation.
    fn set_exif_rotate(&mut self, on: bool) {
        self.config.exif_rotate = on;
        self.config.save();
        self.rotate.store(on, Ordering::Relaxed);
        // The thumbnails too, which are turned by it as well.
        self.redecode();
    }

    /// Decode everything again through another view filter.
    fn set_view_filter(&mut self, f: usize) {
        self.config.view_filter = f;
        self.config.save();
        VIEW_FILTER.store(f, Ordering::Relaxed);
        self.redecode();
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

    // ---- Remote control ----

    /// `glimt --remote` commands and queries; a `select` waits while the folder is read or a
    /// dialog asks about the selection.
    fn receive_remote(&mut self, ctx: &egui::Context) {
        if self.listing_rx.is_none()
            && !self.asking_about_selection()
            && let Some((command, reply)) = self.waiting_select.take()
        {
            let _ = reply.send(self.remote_select(ctx, &command));
        }
        while let Ok((command, reply)) = self.remote_rx.try_recv() {
            match reply {
                // After `open`, the folder is still being read: its files are not all listed. A
                // dialog asking about the selection (Delete, Rename) is answered for the one it
                // asked about, not one a script put in its place meanwhile.
                Some(reply)
                    if command.starts_with("select ")
                        && (self.listing_rx.is_some() || self.asking_about_selection()) =>
                {
                    self.waiting_select = Some((command, reply));
                }
                Some(reply) if command.starts_with("select ") => {
                    let _ = reply.send(self.remote_select(ctx, &command));
                }
                Some(reply) => drop(reply.send(self.remote_query(&command))),
                None => self.remote_command(ctx, &command),
            }
        }
    }

    /// A command from `glimt --remote`: next, prev, first, last, slideshow, random, fullscreen,
    /// subfolders on|off, quit, or open PATH.
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
            "subfolders on" => return self.set_subfolders(ctx, true),
            "subfolders off" => return self.set_subfolders(ctx, false),
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

    /// The answer to `glimt --remote current` or `selection`: the paths, each ending in a NUL
    /// byte. Nothing else is told, and nothing changes.
    // shortcut: the paths of a selection made in the window's thread, some 50 ms for 500,000
    // selected; in a thread of its own if scripts ask for large selections often.
    fn remote_query(&self, command: &str) -> Vec<u8> {
        use std::os::unix::ffi::OsStringExt;
        let files = match command.trim() {
            "current" => vec![self.cur],
            _ => self.targets(),
        };
        (files.into_iter().filter(|&i| i < self.files.len()))
            .flat_map(|i| {
                let path = self.path(i).into_os_string().into_vec();
                path.into_iter().chain([0])
            })
            .collect()
    }

    /// `--remote select PATH…` (the paths after a NUL each): select those of the list and show
    /// the first. The answer: the paths that are not in the list, each ending in a NUL byte.
    /// Only the selection changes; deleting or moving it is left to the user.
    fn remote_select(&mut self, ctx: &egui::Context, command: &str) -> Vec<u8> {
        let paths = command.strip_prefix("select ").unwrap_or_default();
        let paths: Vec<&str> = paths.split('\0').filter(|p| !p.is_empty()).collect();
        // The list's name of `p`: relative to the folder shown.
        fn name<'a>(dir: &Path, p: &'a str) -> Option<&'a std::ffi::OsStr> {
            Some(Path::new(p).strip_prefix(dir).ok()?.as_os_str())
        }
        let dir = self.dir.clone();
        let names: HashSet<&std::ffi::OsStr> = paths.iter().filter_map(|p| name(&dir, p)).collect();
        let found: std::collections::BTreeSet<usize> = (self.files.iter().enumerate())
            .filter(|(_, f)| names.contains(f.as_os_str()))
            .map(|(i, _)| i)
            .collect();
        let listed: HashSet<&std::ffi::OsStr> =
            found.iter().map(|&i| self.files[i].as_os_str()).collect();
        let missing: Vec<u8> = (paths.iter())
            .filter(|p| !name(&dir, p).is_some_and(|n| listed.contains(n)))
            .flat_map(|p| p.bytes().chain([0]))
            .collect();
        if let Some(&first) = found.first() {
            self.go(ctx, first);
            self.selected = found;
            self.anchor = None;
            self.selection_changed(ctx);
        }
        missing
    }

    // ---- Keys and actions ----

    /// Preferences or a modal dialog is open: keys go there.
    /// A question about the selection on screen (Delete, Rename): what it acts on must stay as
    /// it was when asked. Preferences and the like do not count: a script would wait on them.
    fn asking_about_selection(&self) -> bool {
        self.confirm_delete.is_some() || self.prompt.is_some()
    }

    fn dialog_open(&self) -> bool {
        self.preferences_open
            || self.confirm_delete.is_some()
            || self.confirm_folder
            || self.prompt.is_some()
            || self.login.is_some()
            || self.print_window.is_some()
            || self.lightroom.is_some()
            || !self.cut_short.is_empty()
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
                // Whatever has Ctrl+C: Copy, unless the user gave the key to another action. Ctrl+
                // Shift+C comes as the same event: Copy Image to Clipboard.
                let held = if i.modifiers.shift { CTRL_SHIFT } else { CTRL };
                return (all.iter())
                    .find(|s| actions::same_keys(s, &(s.0, held, egui::Key::C)))
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

    /// Do `action`, from a menu, a key, a toolbar button or `--remote`: one arm each.
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
            Action::OpenRecursive => self.pick(ctx, true, Pick::Recursive),
            Action::NewCollection if !self.files.is_empty() => {
                self.prompt = Some((Prompt::NewCollection, String::new()));
            }
            Action::NewCollection => {
                self.notice = Some(tr("No images for a collection here").into())
            }
            Action::OpenCollection => self.pick(ctx, false, Pick::Collection),
            Action::ImportLightroom => self.pick(ctx, false, Pick::Lightroom),
            Action::RemoveFromCollection if self.collection.is_some() && !self.files.is_empty() => {
                let targets = self.targets();
                // Out of the collection's file here, at once: the files stay where they are.
                let out: HashSet<PathBuf> = targets.iter().map(|&i| self.path(i)).collect();
                self.edit_collection(|all| all.retain(|p| !out.contains(p)));
                self.remove_indices(ctx, &targets);
            }
            Action::RemoveFromCollection => {}
            Action::RenameCollection | Action::TrashCollection
                if self.collection.as_deref().is_some_and(collections::is_list) =>
            {
                self.notice = Some(tr("A plain list is not renamed or trashed by Glimt").into());
            }
            Action::RenameCollection => {
                if let Some(name) = self.collection.as_ref().and_then(|f| f.file_stem()) {
                    let name = name.to_string_lossy().into_owned();
                    self.prompt = Some((Prompt::RenameCollection, name));
                }
            }
            Action::TrashCollection => self.trash_collection(ctx),
            Action::MoveEarlier | Action::MoveLater | Action::MoveFirst | Action::MoveLast => {
                let picked: HashSet<usize> = self.targets().into_iter().collect();
                let order = collections::moved(self.files.len(), &picked, action);
                self.move_in_collection(ctx, picked, order)
            }
            Action::FindDuplicates => drop(self.dups.get_or_insert_with(Dups::new)),
            Action::CheckImages => drop(self.check.get_or_insert(None)),
            Action::BackUp => self.pick(ctx, true, Pick::BackUp),
            // A kiosk shows only the image.
            Action::FocusSearch if self.kiosk => {}
            Action::FocusSearch => {
                // The field shows if it was hidden, with the file list (out of full screen),
                // and the / itself is not typed into it.
                if !self.config.search_bar {
                    self.config.search_bar = true;
                    self.config.save();
                }
                self.hide_file_list = false;
                if fullscreen {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                }
                ctx.input_mut(|i| (i.events).retain(|e| *e != egui::Event::Text("/".into())));
                // Given when the field is drawn: egui drops focus on a widget not drawn yet.
                self.focus_search = true;
            }
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
                let here = self.back_step();
                let (from, to) = if action == Action::Back {
                    (&mut self.back, &mut self.forward)
                } else {
                    (&mut self.forward, &mut self.back)
                };
                if let Some(path) = from.pop() {
                    to.push(here);
                    self.notice = None;
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
            Action::CopyImage if !self.files.is_empty() => self.copy_image(ctx),
            Action::CopyImage => {}
            // As a list of files, which a file manager pastes as a copy.
            Action::CopyFiles if !self.files.is_empty() => self.copy_files(),
            Action::CopyFiles => {}
            Action::Preferences if config::locked() => {
                self.notice = Some(tr("The settings are locked (--lock-settings)").into());
            }
            Action::Preferences => self.preferences_open = true,
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
            // An animation: stopped at the frame before or after the one shown, which A plays on.
            Action::NextPage | Action::PrevPage
                if self.slots.get(&self.cur).is_some_and(|s| s.anim.len() > 1) =>
            {
                let n = self.slots.get(&self.cur).map_or(1, |s| s.anim.len());
                let at = self.frame.unwrap_or_else(|| self.playing_frame());
                self.frame = Some(match action {
                    Action::NextPage => (at + 1) % n,
                    _ => (at + n - 1) % n,
                });
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
                    self.decodes_pending.retain(|p, _| p.0 != self.cur);
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
            | Action::Rate5 => self.apply_mark(ctx, action),
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
                    Action::Animate => {
                        // Stepped to a frame: A plays on from there, without turning it off.
                        if self.frame.take().is_some() {
                            return;
                        }
                        &mut c.animate
                    }
                    Action::SortManager => &mut c.sort_manager,
                    _ => &mut c.exif_panel,
                };
                *flag = !*flag;
                c.save();
            }
            // A collection, or several folders shown together, is no folder to make one in: it
            // would be made in /.
            Action::NewFolder if self.collection.is_some() || !self.folders.is_empty() => {
                self.notice =
                    Some(tr("New Folder makes a folder in the folder shown: open one").into());
            }
            Action::NewFolder => self.prompt = Some((Prompt::NewFolder, String::new())),
            Action::Print if !self.files.is_empty() => self.open_print(ctx),
            Action::SaveFrame if self.frame_count() > 1 => {
                let k = self.shown_frame();
                self.pick(ctx, false, Pick::SaveFrame(self.path(self.cur), k));
            }
            Action::ExportFrames if self.frame_count() > 1 => {
                self.pick(ctx, true, Pick::ExportFrames(self.path(self.cur)));
            }
            Action::SaveFrame | Action::ExportFrames => {}
            Action::Print => {}
            // One Rename Many at a time: its result is awaited, and its plan kept for a crash.
            Action::Rename if self.renaming.is_some() => {
                self.notice = Some(tr("Rename Many is still renaming").into());
            }
            Action::Rename if self.targets().len() > 1 => {
                self.ask_about_targets();
                self.prompt = Some((Prompt::RenameMany, "*_###".into()));
            }
            Action::Rename => {
                // The one file selected, as Delete and Move take it (right-clicked in the list,
                // say), not the image shown beside it.
                if let Some(&i) = self.selected.first().filter(|&&i| i != self.cur) {
                    self.go(ctx, i);
                }
                // The name only: in the list with subfolders the entry has its folders too.
                if let Some(name) = self
                    .files
                    .get(self.cur)
                    .and_then(|f| Path::new(f).file_name())
                {
                    self.prompt = Some((Prompt::Rename, name.to_string_lossy().into_owned()));
                    self.ask_about_targets();
                }
            }
            // A separate process: windows share nothing but the config file.
            Action::NewWindow => self.new_window(),
            Action::Projector => {
                self.projector = !self.projector;
                self.projector_size = [0, 0];
            }
            Action::ProjectorPointer => {
                self.config.projector_pointer = !self.config.projector_pointer;
                self.config.save();
            }
            Action::ExifRotate => self.set_exif_rotate(!self.config.exif_rotate),
            Action::ViewFilter => {
                self.set_view_filter((self.config.view_filter + 1) % FILTERS.len())
            }
            // Mirroring or flipping what is on screen after a turn: mirror the image first and
            // turn the other way (a flip is a mirror and half a turn).
            Action::RotateCw
            | Action::RotateCcw
            | Action::Rotate180
            | Action::Mirror
            | Action::Flip => {
                // Nothing shown: nothing to turn.
                if self.files.is_empty() {
                    return;
                }
                // Until it is saved the old picture shows: turned again, it would be saved twice,
                // into the same temporary file.
                if self.saving.contains(&self.path(self.cur)) {
                    self.notice = Some(tr("Still saving the last rotation of this file").into());
                    return;
                }
                let (q, m) = self.orient;
                self.turned = self.path(self.cur);
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
            // Modes that stay for the next images, as ImageGlass's; X goes back to the whole.
            Action::ZoomFit | Action::FitWidth | Action::FitHeight | Action::FitFill => {
                self.zoom = None;
                self.pan_offset = egui::Vec2::ZERO;
                self.config.fit_mode = match action {
                    Action::FitWidth => 1,
                    Action::FitHeight => 2,
                    Action::FitFill => 3,
                    _ => 0,
                };
                self.config.save();
            }
            Action::FullScreen => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fullscreen))
            }
            Action::HideTools => self.hide_file_list = !self.hide_file_list,
            Action::Refresh => {
                self.tree.clear();
                self.slots.clear();
                self.decodes_pending.clear();
                self.decode_errors.clear();
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

    /// A mark, a star rating or selecting by mark, from Edit → Marks and Stars or its keys.
    #[cfg(feature = "marks")]
    fn apply_mark(&mut self, ctx: &egui::Context, action: Action) {
        let Some((kind, n)) = action.mark() else {
            return;
        };
        // What another window marked meanwhile counts too.
        self.marks.refresh();
        if kind == 's' {
            let bit = 1 << (n - 1);
            let names = self.marked_names(|(bits, _)| bits & bit != 0);
            let picked = (self.shown_files().into_iter())
                .filter(|&i| names.contains(self.files[i].as_os_str()))
                .collect();
            self.selected = picked;
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
            // In the background: a thousand photos on a network share would hold the window.
            if self.config.xmp_stars {
                self.write_sidecars(
                    ctx,
                    tr("Writing stars"),
                    paths,
                    move |path| marks::write_xmp(path, n).is_ok(),
                    |failed, job| match () {
                        _ if failed > 0 => Some(tf!(
                            "The stars could not be written to the XMP sidecar of {count}",
                            count = count(failed, "file")
                        )),
                        _ if job.stopped() => Some(job.stopped_text()),
                        _ => None,
                    },
                );
            }
        }
        // A "mark:" or "stars:" search shows the new state.
        if self.search.starts_with("mark:") || self.search.starts_with("stars:") {
            self.update_search(ctx);
        }
    }

    /// File → New Window (Ctrl+N): another Glimt at this place.
    fn new_window(&mut self) {
        let exe = std::env::current_exe().map(|exe| {
            let var = std::env::var_os;
            own_program(exe, var("APPDIR"), var("APPIMAGE"))
        });
        // After an update (or a rebuild) Linux reports the running binary as
        // "/path/glimt (deleted)"; the new one is at the same path.
        let started = exe.and_then(|exe| {
            let exe = exe.to_string_lossy();
            let exe = exe.strip_suffix(" (deleted)").unwrap_or(&exe);
            // Locked settings stay locked in the new window, which is a Glimt of its own.
            (std::process::Command::new(exe))
                .args(config::locked().then_some("--lock-settings"))
                .arg(self.location())
                .spawn()
        });
        match started {
            // Reap the child when it exits, so it does not linger as a zombie.
            Ok(mut child) => drop(std::thread::spawn(move || child.wait())),
            Err(e) => self.notice = Some(tf!("Could not open a new window: {e}", e)),
        }
    }

    /// Select → Copy Image: the shown image's pixels to the clipboard, decoded in the background.
    fn copy_image(&mut self, ctx: &egui::Context) {
        let (path, rotate) = (self.path(self.cur), self.config.exif_rotate);
        let (tx, ctx) = (self.notices_tx.clone(), ctx.clone());
        let (clip_tx, clip_rx) = mpsc::channel();
        self.clip_rx = Some(clip_rx);
        let pointer = Working::new(&self.busy);
        std::thread::spawn(move || {
            // A decoder that panics on a broken file must not end the thread: the busy pointer
            // would stay for good.
            match survive(|| load(&path, rotate)) {
                Ok(img) => drop(clip_tx.send(color_image(img))),
                Err(e) => drop(tx.send(tf!("Could not copy the image: {e}", e))),
            }
            drop(pointer);
            ctx.request_repaint();
        });
    }

    /// Select → Copy Files: the selected files (or the shown one) to the clipboard, as a file
    /// manager copies them.
    fn copy_files(&mut self) {
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
            self.notice = Some(tf!("Could not copy to the clipboard: {e}", e));
        }
    }
}

// The parts of a frame, in the order `ui` draws them, and what reaches it from the threads in the
// background. The windows of one feature (pan view, Find Duplicates, Check Images, Preferences,
// the projector) are with that feature: above, or in its own module.
impl App {
    /// What the background threads and the rest of the desktop sent since the last frame: decoded
    /// images and thumbnails, the folder listing, saves, remote commands, dropped files, mounts,
    /// notices and the file dialog's answer.
    fn poll_background(&mut self, ctx: &egui::Context) {
        if let Some(error) = config::take_save_error() {
            self.notice = Some(error);
        }
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
        self.max_side = ctx.input(|i| i.max_texture_side) as u32;
        self.receive_decoded(ctx);
        self.receive_thumbs(ctx);
        self.receive_listing(ctx, std::time::Duration::ZERO);
        self.receive_sizes();
        self.receive_exif_index(ctx);
        self.receive_deleted();
        self.selection_size(ctx);
        self.file_jobs.retain(|job| Arc::strong_count(job) > 1);
        if self.work_left() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        } else if self.closing > 0 {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        while let Ok(path) = self.edited_rx.try_recv() {
            self.saved(&path);
        }
        while let Ok((path, result)) = self.saved_rx.try_recv() {
            self.saving.remove(&path);
            match result {
                Ok(()) => self.saved(&path),
                Err(e) => self.notice = Some(tf!("Could not save the orientation: {e}", e)),
            }
        }
        // --slideshow: once the folder is read, so random order shuffles all of it.
        if self.slideshow_at_start && self.listing_rx.is_none() {
            self.slideshow_at_start = false;
            self.start_slideshow(ctx);
        }
        let focus = ctx.input(|i| i.viewport().focused.unwrap_or(false));
        if focus && !self.has_focus && self.config.remote_control {
            remote::mark_focused();
        }
        // Back from another window, which may have marked files meanwhile.
        #[cfg(feature = "marks")]
        if focus && !self.has_focus {
            self.marks.refresh();
        }
        self.has_focus = focus;
        self.receive_remote(ctx);
        if let Some(img) = self.clip_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.clip_rx = None;
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
        self.receive_mount(ctx);
        // Busy pointer while the folder is read, a share mounted, work done in the background,
        // or the shown image still decoded.
        let decoding = !self.files.is_empty()
            && !self.slots.contains_key(&self.cur)
            && !self.decode_errors.contains_key(&self.cur);
        if self.listing_rx.is_some()
            || decoding
            || self.mounting_rx.is_some()
            || self.busy.load(Ordering::Relaxed) > 0
        {
            ctx.set_cursor_icon(egui::CursorIcon::Progress);
        }
        if self.changed_rx.try_iter().count() > 0 && self.config.auto_refresh {
            self.relist = true;
        }
        if self.relist && self.listing_rx.is_none() {
            self.list(ctx);
        }
        if let Some(Ok(read)) = self.lightroom_rx.as_ref().map(|rx| rx.try_recv()) {
            self.lightroom_rx = None;
            match read {
                Ok(catalog) => {
                    let now = lightroom::roots_now(&catalog);
                    self.lightroom = Some(lightroom::Import {
                        catalog,
                        now,
                        plan: None,
                    });
                }
                Err(e) => self.notice = Some(e),
            }
        }
        if let Some(Ok(result)) = self.renaming.as_ref().map(|r| r.0.try_recv()) {
            let (_, job) = self.renaming.take().unwrap();
            self.renamed(ctx, job, result);
        }
        if let Some(Ok(found)) = self.cut_short_rx.as_ref().map(|rx| rx.try_recv()) {
            (self.cut_short, self.cut_short_rx) = (found, None);
        }
        while let Ok(msg) = self.notices_rx.try_recv() {
            self.notice = Some(msg);
            ctx.request_repaint();
        }
        self.slideshow_tick(ctx);
        if let Some(all) = self.together.take() {
            self.open_folders(ctx, all);
        }
        self.receive_picked(ctx);
    }

    /// A share mounted, or why not: its folder opens, or the error or a login is asked for.
    fn receive_mount(&mut self, ctx: &egui::Context) {
        if let Some(done) = self.mounting_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.mounting_rx = None;
            // "Connecting to …" goes: the folder, the error or the login takes its place.
            self.notice = None;
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
    }

    /// The file dialog's answer, for what it was opened for.
    fn receive_picked(&mut self, ctx: &egui::Context) {
        let answer = self.file_dialog.as_ref().map(|(_, rx)| rx.try_recv());
        // A dialog whose thread ended without an answer is over too: the next may open.
        if let Some(Err(mpsc::TryRecvError::Disconnected)) = answer {
            self.file_dialog = None;
        }
        if let Some(Ok(picked)) = answer {
            let (purpose, _) = self.file_dialog.take().unwrap();
            match (purpose, picked) {
                (_, None) => {}
                (Pick::Open | Pick::Collection, Some(path)) => self.visit(ctx, path),
                // As View → Include Subfolders, which turns it off again.
                (Pick::Recursive, Some(path)) => {
                    self.config.show_subfolders = true;
                    self.config.save();
                    self.visit(ctx, path);
                }
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
                (Pick::Lightroom, Some(file)) => self.read_lightroom(ctx, file),
                (Pick::BackUp, Some(dir)) => self.back_up(ctx, dir),
                (Pick::SaveFrame(path, k), Some(file)) => {
                    self.export_frames(ctx, path, Some(k), file)
                }
                (Pick::ExportFrames(path), Some(dir)) => self.export_frames(ctx, path, None, dir),
            }
        }
    }

    /// Run zenity's (or kdialog's) file dialog in its own thread so the window does not freeze.
    fn pick(&mut self, ctx: &egui::Context, folder: bool, purpose: Pick) {
        use std::os::unix::ffi::OsStringExt;
        // One at a time: only one answer is waited for, and a second dialog's would replace the
        // first's, whose copy or move would then never happen.
        if self.file_dialog.is_some() {
            self.notice = Some(tr("A file dialog is open already").into());
            return;
        }
        let (tx, rx) = mpsc::channel();
        let start = match &purpose {
            // A name for the file to save: the first image's, as a PDF.
            Pick::SavePdf(job) => self.dir.join(
                (job.paths.first())
                    .map_or("print".into(), |p| p.with_extension("pdf"))
                    .file_name()
                    .unwrap_or_default(),
            ),
            // "name-frame-3.png" beside the file.
            Pick::SaveFrame(path, k) => {
                let stem = path.file_stem().unwrap_or_default().to_string_lossy();
                self.dir.join(format!("{stem}-frame-{}.png", k + 1))
            }
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
        let save = matches!(purpose, Pick::SavePdf(_) | Pick::SaveFrame(..));
        let several = matches!(purpose, Pick::Folders);
        let ctx = ctx.clone();
        let title = match purpose {
            Pick::Open if folder => tr("Open Folder"),
            Pick::Open => tr("Open File"),
            Pick::Folders => tr("Open Folders (Ctrl+click for several)"),
            Pick::Recursive => tr("Open Folder with Its Subfolders"),
            Pick::Collection => tr("Open Collection"),
            Pick::CopyTo(_) => tr("Copy to Folder"),
            Pick::MoveTo(_) => tr("Move to Folder"),
            Pick::SavePdf(_) => tr("Save as PDF"),
            Pick::SortFolder => tr("Add to the Sort Manager"),
            Pick::ScreenProfile => tr("Screen Profile"),
            Pick::InputProfile => tr("Profile of Images Without One"),
            Pick::Lightroom => tr("Import Lightroom Catalog"),
            Pick::BackUp => tr("Back Up Glimt's Data To"),
            Pick::SaveFrame(..) => tr("Save This Frame as PNG"),
            Pick::ExportFrames(_) => tr("Export Every Frame To"),
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
            let output = match (external::host_has("zenity"), external::host_has("kdialog")) {
                (true, _) => zenity(),
                (false, true) => kdialog(),
                (false, false) => Err(std::io::ErrorKind::NotFound.into()),
            };
            let picked = match output {
                Err(e) => {
                    let _ = notices.send(tf!("No file dialog (install zenity or kdialog): {e}", e));
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
        self.file_dialog = Some((purpose, rx));
    }

    /// The action of the key pressed this frame, if any; the arrow keys move a zoomed image first.
    fn read_keys(&mut self, ctx: &egui::Context) -> Option<Action> {
        // Shortcuts go to the dialogs while one is open.
        // ... and to the text field while one has focus (the toolbar's folder path).
        let typing = ctx
            .memory(|m| m.focused())
            .is_some_and(|f| Some(f) != self.image_id);
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
                        self.pan_offset = (self.pan_offset + d).clamp(-max, max);
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

    /// A ring in the accent colour around the widget with the keyboard focus, over everything,
    /// once Tab has moved it there; a click makes it go. egui draws none in menus and on flat
    /// buttons, so without it Tab moved an invisible focus. Text fields show their own.
    fn focus_ring(&mut self, ctx: &egui::Context) {
        let pressed = ctx.input(|i| {
            if i.key_pressed(egui::Key::Tab) {
                self.focus_by_keys = true;
            }
            i.pointer.any_pressed()
        });
        if pressed {
            self.focus_by_keys = false;
        }
        let Some(id) = ctx.memory(|m| m.focused()) else {
            return;
        };
        if Some(id) == self.image_id || ctx.text_edit_focused() {
            return;
        }
        // A click puts the focus Tab gave away, also on the widget clicked, where egui keeps it:
        // the keys would go there, not to the shortcuts, with no ring to show it.
        if pressed {
            ctx.memory_mut(|m| m.surrender_focus(id));
            return;
        }
        if !self.focus_by_keys {
            return;
        }
        if let Some(widget) = ctx.read_response(id) {
            let layer = egui::LayerId::new(egui::Order::Foreground, egui::Id::new("focus ring"));
            let accent = ctx.global_style().visuals.selection.bg_fill;
            ctx.layer_painter(layer).rect_stroke(
                widget.rect.expand(1.0),
                4.0,
                egui::Stroke::new(2.0, accent),
                egui::StrokeKind::Outside,
            );
        }
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
        // Keywords typed and not yet saved: written now, so closing waits for them.
        if ctx.input(|i| i.viewport().close_requested()) {
            self.save_meta();
        }
        // Files still being copied, moved, renamed or saved: closed when they are done, not
        // halfway; a second close stops them after the file at hand, a third does not wait.
        if ctx.input(|i| i.viewport().close_requested()) && self.work_left() && self.closing < 2 {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.closing += 1;
            if self.closing == 2 {
                (self.file_jobs.iter()).for_each(|job| job.stop.store(true, Ordering::Relaxed));
            }
            self.notice = Some(
                match self.closing {
                    1 => "Glimt closes when the file operations are done; close again to stop them",
                    _ => "Stopping after the files at hand; close once more to end at once",
                }
                .into(),
            );
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
        self.window_state.maximized = maximized;
        if !fullscreen && !maximized {
            // In the desktop's units, as the window opens with them: egui's shrink with the
            // size chosen in Preferences, and a window kept at 200 % opened half as large.
            let zoom = ctx.zoom_factor();
            self.window_state.size = Some(size * zoom);
            // Wayland reports no position.
            self.window_state.pos = outer
                .map(|r| (r.min.to_vec2() * zoom).to_pos2())
                .or(self.window_state.pos);
        }
        fullscreen
    }

    /// Keep the screen saver off while `on`: gnome-session-inhibit (GNOME), else systemd-inhibit.
    /// The inhibitor runs `cat` on a pipe from Glimt, so it ends with Glimt even after a crash.
    fn inhibit(&mut self, on: bool) {
        if on == self.inhibit_on {
            return;
        }
        self.inhibit_on = on;
        if let Some(mut child) = self.inhibitor.take() {
            let _ = child.kill();
            std::thread::spawn(move || child.wait());
        }
        if !on {
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
        // The first that is there, on the host in a Flatpak.
        let found = commands.iter().find(|c| external::host_has(c[0]));
        self.inhibitor = found.and_then(|c| {
            command(c[0])
                .args(&c[1..])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .ok()
        });
    }

    /// The menu bar, as Geeqie's, and the toolbar under it.
    fn menu_bar(&mut self, ui: &mut egui::Ui, act: &mut Option<Action>) {
        let bar = egui::Panel::top("menu").show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                scroll_menu(ui, tr("File"), |ui| {
                    menu_item(ui, act, Action::OpenFile);
                    menu_item(ui, act, Action::OpenFolder);
                    menu_item(ui, act, Action::OpenRecursive);
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
                    // Only for a file of several frames or pages: the menu stays as it was
                    // for every other one.
                    if self.frame_count() > 1 {
                        menu_item(ui, act, Action::SaveFrame);
                        menu_item(ui, act, Action::ExportFrames);
                    }
                    ui.separator();
                    menu_item(ui, act, Action::Delete);
                    menu_item(ui, act, Action::DeletePermanently);
                    ui.separator();
                    self.collections_menu(ui, act);
                    menu_item(ui, act, Action::FindDuplicates);
                    menu_item(ui, act, Action::CheckImages);
                    menu_item(ui, act, Action::BackUp);
                    ui.separator();
                    menu_item(ui, act, Action::CloseWindow);
                    menu_item(ui, act, Action::Quit);
                });
                scroll_menu(ui, tr("Go"), |ui| {
                    menu_item(ui, act, Action::First);
                    menu_item(ui, act, Action::Prev);
                    menu_item(ui, act, Action::Next);
                    menu_item(ui, act, Action::Last);
                    ui.separator();
                    menu_item(ui, act, Action::Back);
                    menu_item(ui, act, Action::Forward);
                    menu_item(ui, act, Action::Home);
                    menu_item(ui, act, Action::Up);
                    ui.separator();
                    menu_item(ui, act, Action::FocusSearch);
                });
                scroll_menu(ui, tr("Select"), |ui| {
                    menu_item(ui, act, Action::SelectAll);
                    menu_item(ui, act, Action::SelectNone);
                    menu_item(ui, act, Action::InvertSelection);
                    ui.separator();
                    menu_item(ui, act, Action::CopyPath);
                    menu_item(ui, act, Action::CopyPathUnquoted);
                    menu_item(ui, act, Action::CopyImage);
                    menu_item(ui, act, Action::CopyFiles);
                });
                scroll_menu(ui, tr("Edit"), |ui| {
                    #[cfg(feature = "marks")]
                    scroll_menu(ui, tr("Marks and Stars"), |ui| {
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
                    scroll_menu(ui, tr("Orientation"), |ui| {
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
                scroll_menu(ui, tr("View"), |ui| {
                    self.layout_menu(ui, act);
                    scroll_menu(ui, tr("Theme"), |ui| self.theme_menu(ui));
                    scroll_menu(ui, tr("Colour Management"), |ui| self.colour_settings(ui));
                    scroll_menu(ui, tr("Filter"), |ui| {
                        for (f, label) in FILTERS.iter().enumerate() {
                            if ui.radio(self.config.view_filter == f, *label).clicked() {
                                self.set_view_filter(f);
                            }
                        }
                        ui.separator();
                        menu_item(ui, act, Action::ViewFilter);
                    });
                    scroll_menu(ui, tr("Zoom"), |ui| {
                        menu_item(ui, act, Action::ZoomIn);
                        menu_item(ui, act, Action::ZoomOut);
                        menu_item(ui, act, Action::Zoom1To1);
                        let mode = self.config.fit_mode;
                        menu_toggle(ui, act, mode == 0, Action::ZoomFit);
                        menu_toggle(ui, act, mode == 1, Action::FitWidth);
                        menu_toggle(ui, act, mode == 2, Action::FitHeight);
                        menu_toggle(ui, act, mode == 3, Action::FitFill);
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
                        tr("Stop Slideshow")
                    } else {
                        tr("Start Slideshow")
                    };
                    menu_button(ui, act, egui::Button::new(label), Action::Slideshow);
                    menu_toggle(ui, act, self.paused, Action::Pause);
                    menu_item(ui, act, Action::Faster);
                    menu_item(ui, act, Action::Slower);
                    menu_toggle(ui, act, self.config.random, Action::RandomOrder);
                });
                scroll_menu(ui, tr("Window"), |ui| {
                    menu_item(ui, act, Action::NewWindow);
                    menu_item(ui, act, Action::CloseWindow);
                    ui.separator();
                    menu_toggle(ui, act, self.projector, Action::Projector);
                    let pointer = self.config.projector_pointer;
                    menu_toggle(ui, act, pointer, Action::ProjectorPointer);
                });
                scroll_menu(ui, tr("Help"), |ui| {
                    menu_item(ui, act, Action::Help);
                    menu_item(ui, act, Action::Shortcuts);
                    menu_item(ui, act, Action::KeyboardMap);
                    ui.separator();
                    menu_item(ui, act, Action::About);
                });
            });
        });
        close_menus(ui.ctx(), bar.response.rect, &mut self.menu_away);
        if self.config.toolbar {
            self.toolbar(ui, act);
        }
    }

    /// Asks for a name: a new folder, a new name for a folder or file, Rename Many's pattern, a new
    /// collection, a theme to save.
    fn prompt_dialog(&mut self, ctx: &egui::Context) {
        if let Some((prompt, mut text)) = self.prompt.take() {
            let title = match prompt {
                Prompt::NewFolder => "New folder name:".to_owned(),
                Prompt::RenameFolder => "New folder name:".to_owned(),
                Prompt::SaveTheme => "Name of the new theme:".to_owned(),
                Prompt::RenameCollection => "New name of the collection:".to_owned(),
                Prompt::Rename => "New file name:".to_owned(),
                Prompt::NewCollection => tf!(
                    "Name of a new collection with {count}:",
                    count = count(self.first_target().0, "image")
                ),
                Prompt::RenameMany => tf!(
                    "New names for {count} (* = old name, # = number, ### = 001):",
                    count = count(self.first_target().0, "file")
                ),
            };
            // The first new name, so the pattern can be checked before renaming.
            let i = self.first_target().1;
            let first = (i < self.files.len()).then(|| self.path(i));
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
                    let name = pattern_name(text.trim(), first, 1);
                    ui.weak(tf!("First: {name}", name = name.to_string_lossy()));
                }
                ui.horizontal(|ui| {
                    if ui.button(tr("OK (Enter)")).clicked() || enter {
                        answer = Some(text.clone());
                    }
                    if ui.button(tr("Cancel (Esc)")).clicked() {
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

    /// The text dialog was answered with `text`.
    fn answer_prompt(&mut self, ctx: &egui::Context, prompt: Prompt, text: &str) {
        if let Prompt::NewCollection = prompt {
            return match collections::new_file(text) {
                Ok(file) => self.add_to_collection(&file),
                Err(e) => self.notice = Some(e),
            };
        }
        if let Prompt::RenameCollection = prompt {
            let Some(old) = self.collection.clone() else {
                return;
            };
            match collections::rename(&old, text) {
                Ok(new) => {
                    self.collection = Some(new);
                    self.title(ctx);
                }
                Err(e) => self.notice = Some(e),
            }
            return;
        }
        if text.is_empty() || text.contains('/') || text == "." || text == ".." {
            self.notice = Some(tf!("Not a valid name: \"{text}\"", text));
            return;
        }
        if let Prompt::SaveTheme = prompt {
            let file = theme::dir().map(|d| d.join(format!("{text}.theme")));
            let saved = file.ok_or("no home folder".to_owned()).and_then(|f| {
                // Never over a theme there, hand-edited perhaps, nor in the place of a built-in
                // one, which a file of its name would hide for good.
                if theme::built_in(text) {
                    return Err(tf!("{text} is a built-in theme: choose another name", text));
                }
                let _ = std::fs::create_dir_all(f.parent().unwrap_or(&f));
                let new = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&f);
                let mut file = new.map_err(|e| match e.kind() {
                    std::io::ErrorKind::AlreadyExists => {
                        tf!("{text} exists already: choose another name", text)
                    }
                    _ => e.to_string(),
                })?;
                std::io::Write::write_all(&mut file, theme::Theme::file_from(ctx).as_bytes())
                    .map(|_| f)
                    .map_err(|e| e.to_string())
            });
            self.notice = Some(match saved {
                Ok(f) => {
                    self.config.theme = text.to_owned();
                    self.config.save();
                    tf!(
                        "Saved as {file}: edit it, then choose it again",
                        file = f.display()
                    )
                }
                Err(e) => tf!("Could not save the theme: {e}", e),
            });
            return;
        }
        if let Prompt::RenameFolder = prompt {
            return self.rename_folder(ctx, text);
        }
        if matches!(prompt, Prompt::Rename | Prompt::RenameMany) {
            let Some(found) = self.asked_targets() else {
                return;
            };
            match prompt {
                Prompt::Rename if found[0] != self.cur => self.go(ctx, found[0]),
                Prompt::Rename => {}
                _ => self.selected = found.into_iter().collect(),
            }
        }
        if matches!(prompt, Prompt::Rename)
            && !self.files.is_empty()
            && !self.none_saving(&[self.path(self.cur)])
        {
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
            Prompt::RenameMany => return self.rename_many(ctx, text),
            Prompt::NewCollection
            | Prompt::RenameCollection
            | Prompt::SaveTheme
            | Prompt::RenameFolder => return,
            // As Rename Many: in its thread, never over a file or another file's sidecar
            // there; `renamed` puts the list right.
            Prompt::Rename => {
                self.rename_in_background(ctx, vec![self.path(self.cur)], vec![target]);
                return;
            }
        };
        match (result, prompt) {
            (Ok(()), Prompt::NewFolder) => self.list(ctx),
            // The list keeps its folder and mode; the shown entry takes the new name.
            (Ok(()), _) => {
                if let Ok(rel) = target.strip_prefix(&self.dir) {
                    self.files[self.cur] = rel.as_os_str().to_owned();
                }
                self.title(ctx);
                self.list(ctx);
            }
            (Err(e), _) => {
                self.notice = Some(format!("{display}: {e}", display = target.display()))
            }
        }
    }

    /// Renames cut short by a crash or a power cut: put the old names back, or finish them, in
    /// the background; Later asks again at the next start.
    fn cut_short_dialog(&mut self, ctx: &egui::Context) {
        if self.cut_short.is_empty() {
            return;
        }
        let files: usize = self.cut_short.iter().map(|c| c.1.len()).sum();
        let folder = (self.cut_short.iter())
            .find_map(|c| c.1.first())
            .and_then(|m| m.0.parent())
            .map_or(String::new(), |p| p.display().to_string());
        let mut choice = None;
        egui::Modal::new(egui::Id::new("cut short")).show(ctx, |ui| {
            ui.strong(tr("Rename Many was cut short"));
            ui.label(format!(
                "A crash or a power cut stopped it in {folder}. Some files may have their new \
                 names, some their old ones, and some hidden temporary names ({} to put right).",
                count(files, "step")
            ));
            ui.horizontal(|ui| {
                if ui.button(tr("Put the old names back")).clicked() {
                    choice = Some(true);
                }
                if ui.button(tr("Finish the renaming")).clicked() {
                    choice = Some(false);
                }
                if ui.button(tr("Later")).clicked() {
                    choice = None;
                    self.cut_short.clear();
                }
            });
        });
        let Some(undo) = choice else { return };
        let plans = std::mem::take(&mut self.cut_short);
        self.background(ctx, true, move || {
            let (mut renamed, mut left) = (0, 0);
            for (plan, moves, done) in &plans {
                let (r, l) = journal::settle(plan, moves, *done, undo);
                (renamed, left) = (renamed + r, left + l);
            }
            let files = count(renamed, "file");
            let mut msg = match undo {
                true => tf!("{files} given their old names back", files),
                false => tf!("{files} renamed", files),
            };
            if left > 0 {
                let files = count(left, "file");
                msg += &tf!(
                    "; {files} could not be, asked again at the next start",
                    files
                );
            }
            Some(msg)
        });
        self.relist = true;
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
                        ui.weak(tr("No EXIF data"));
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
                ui.label(tf!("Log in to {uri}", uri = login.uri));
                egui::Grid::new("login fields").show(ui, |ui| {
                    // Sized outright: in the grid of a modal, desired_width left them a few
                    // letters wide.
                    let size = egui::vec2(220.0, ui.spacing().interact_size.y);
                    let field = |text| egui::TextEdit::singleline(text);
                    ui.label(tr("User:"));
                    ui.add_sized(size, field(&mut login.user));
                    ui.end_row();
                    ui.label(tr("Domain:"));
                    ui.add_sized(size, field(&mut login.domain));
                    ui.end_row();
                    ui.label(tr("Password:"));
                    let edit = ui.add_sized(size, field(&mut login.password).password(true));
                    // The password field first, as the rest has defaults.
                    if login.password.is_empty() && ui.memory(|m| m.focused().is_none()) {
                        edit.request_focus();
                    }
                    connect = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    ui.end_row();
                });
                ui.horizontal(|ui| {
                    connect |= ui.button(tr("Connect (Enter)")).clicked();
                    if ui.button(tr("Cancel (Esc)")).clicked() {
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
        if let Some(trash) = self.confirm_delete {
            let what = match self.first_target() {
                (1, i) => format!("\"{}\"", self.name(i)),
                (many, _) => count(many, "file"),
            };
            let mut delete = false;
            let modal = egui::Modal::new(egui::Id::new("confirm-delete")).show(ctx, |ui| {
                if trash {
                    ui.label(tf!("Move {what} to the trash?", what));
                } else {
                    ui.label(tf!("Permanently delete {what}?", what));
                    ui.weak(tr("This cannot be undone."));
                }
                ui.horizontal(|ui| {
                    delete = ui.button(tr("Delete (Enter)")).clicked()
                        || ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.button(tr("Cancel (Esc)")).clicked() {
                        ui.close();
                    }
                });
            });
            if delete {
                self.confirm_delete = None;
                if let Some(targets) = self.asked_targets() {
                    self.delete_now(ctx, trash, targets);
                }
            } else if modal.should_close() {
                self.confirm_delete = None;
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
                ui.label(tf!(
                    "Move the folder \"{name}\" and all in it to the trash?",
                    name
                ));
                ui.horizontal(|ui| {
                    trash = ui.button(tr("Move to Trash (Enter)")).clicked()
                        || ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.button(tr("Cancel (Esc)")).clicked() {
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
                    _ if self.listing_rx.is_some() => tr("Reading folder…").to_owned(),
                    // A search shows the matches: count and size of what the list shows.
                    Some(found) => {
                        let size = size_text(self.match_rx.is_none().then_some(self.match_size));
                        tf!(
                            "{found} of {images} ({size})",
                            found = thousands(found.len()),
                            images = count(n, "image"),
                            size
                        )
                    }
                    None => format!("{count} ({size})", count = count(n, "image")),
                };
                if !self.selected.is_empty() {
                    let m = thousands(self.selected.len());
                    let size = size_text(self.selected_size);
                    left += &tf!(", {m} selected ({size})", m, size);
                }
                self.sort_menu(ctx, ui);
                self.types_menu(ctx, ui);
                ui.separator();
                ui.label(left);
                if let Some(job) = self.file_jobs.first() {
                    ui.separator();
                    let more = match self.file_jobs.len() - 1 {
                        0 => String::new(),
                        n => tf!(", and {n} more", n),
                    };
                    ui.label(tf!(
                        "{doing} {done} of {total}…{more}",
                        doing = tr(job.doing),
                        done = thousands(job.done.load(Ordering::Relaxed)),
                        total = thousands(job.total),
                        more
                    ));
                    if ui.small_button(tr("Stop")).clicked() {
                        self.file_jobs
                            .iter()
                            .for_each(|j| j.stop.store(true, Ordering::Relaxed));
                    }
                }
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
                        // shortcut: one stat in the UI thread per image change; move it to
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
                    let bytes = tf!("{n} bytes", n = thousands(bytes as usize));
                    match self.slots.get(&idx) {
                        Some(slot) => {
                            let [w, h] = slot.orig;
                            if let Some(scale) = scale {
                                ui.label(format!("{} %", (scale * 100.0).round()));
                                ui.separator();
                            }
                            let page = if slot.pages > 1 && idx == self.cur {
                                tf!(" page {n}/{pages}", n = self.page + 1, pages = slot.pages)
                            } else if let Some(k) = self.frame.filter(|_| idx == self.cur) {
                                tf!(" frame {n}/{frames}", n = k + 1, frames = slot.anim.len())
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
        // Room for a name next to the size and date columns, and the thumbnail before it.
        let thumb = if self.config.thumbnails {
            self.thumb().x + 10.0
        } else {
            0.0
        };
        let min = if self.config.file_details {
            DETAILS_WIDTH + 140.0 + thumb
        } else {
            80.0 + thumb
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
            let first_row = rows.start;
            let rows = rows.start * cols..(rows.end * cols).min(count);
            let shown: Vec<usize> = match &self.matches {
                Some(m) => m[rows.clone()].to_vec(),
                None => rows.clone().collect(),
            };

            if thumbs && !shown.is_empty() {
                // The rows shown and some around them, as files: kept being made while near.
                let near =
                    rows.start.saturating_sub(THUMB_MARGIN)..(rows.end + THUMB_MARGIN).min(count);
                let mut view: Vec<usize> = match &self.matches {
                    Some(m) => m[near].to_vec(),
                    None => near.collect(),
                };
                view.sort_unstable();
                self.thumb_view.lock().unwrap()[0] = view;
                self.list_rows = rows.clone();
                self.trim_thumbs(rows);
            }
            let mut line = egui::Rect::NOTHING;
            let pointer = ctx.input(|i| i.pointer.interact_pos());
            if let Some((_, to)) = &mut self.dragging {
                *to = None;
                // Near the top or bottom edge, the list scrolls on.
                let clip = ui.clip_rect();
                match pointer {
                    Some(p) if p.y < clip.top() + row_h / 2.0 => {
                        ui.scroll_with_delta(egui::vec2(0.0, 10.0))
                    }
                    Some(p) if p.y > clip.bottom() - row_h / 2.0 => {
                        ui.scroll_with_delta(egui::vec2(0.0, -10.0))
                    }
                    _ => {}
                }
                ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
                ctx.request_repaint();
            }
            let mut cells = Vec::new();
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
                    cells.push((i, cell));
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
                if self.collection.is_some() {
                    self.drag_row(ui, &row, i, icons, pointer);
                }
            }
            if icons {
                self.drag_rubber_band(ui, &cells, row_h, first_row);
            }
            // Dropped: the dragged images before the row the line was at.
            if ctx.input(|i| i.pointer.any_released())
                && let Some((picked, Some(to))) = self.dragging.take()
            {
                let order = collections::dropped(self.files.len(), &picked, to);
                self.move_in_collection(ctx, picked, order);
            } else if !ctx.input(|i| i.pointer.any_down()) {
                self.dragging = None;
            }
        });
    }

    /// Selecting in the icon grid by dragging a rectangle, as in a file manager: every image
    /// it touches, added to the selection with Ctrl or Shift. It starts anywhere but in a
    /// collection, where dragging an image moves it, from the space between them; it counts once
    /// the pointer has moved a little, so a click stays a click. `cells` are the shown ones.
    fn drag_rubber_band(
        &mut self,
        ui: &egui::Ui,
        cells: &[(usize, egui::Rect)],
        row_h: f32,
        first_row: usize,
    ) {
        let ctx = ui.ctx().clone();
        // The grid's top, above the rows drawn: the rectangle is kept in the grid, so it moves
        // with the images as they scroll.
        let pitch = row_h + ui.spacing().item_spacing.y;
        let origin = ui.max_rect().min - egui::vec2(0.0, first_row as f32 * pitch);
        let (cell, gap) = (self.cell(), ui.spacing().item_spacing.x);
        let cols = self.grid_cols.max(1);
        let count = self.matches.as_ref().map_or(self.files.len(), Vec::len);
        let clip = ui.clip_rect();
        let (pressed, down, pos, add) = ctx.input(|i| {
            let m = i.modifiers;
            (
                i.pointer.primary_pressed(),
                i.pointer.primary_down(),
                i.pointer.interact_pos(),
                m.command || m.shift,
            )
        });
        if pressed
            && self.rubber_band.is_none()
            && self.dragging.is_none()
            && !self.dialog_open()
            // Not on the scroll bar, which is dragged too.
            && let Some(p) = pos.filter(|p| clip.contains(*p))
            && p.x < clip.right() - ui.spacing().scroll.allocated_width()
            && (self.collection.is_none() || !cells.iter().any(|c| c.1.contains(p)))
        {
            let before = if add {
                self.selected.clone()
            } else {
                Default::default()
            };
            self.rubber_band = Some(RubberBand {
                start: p - origin,
                before,
                moved: false,
                touched: None,
            });
        }
        let Some(RubberBand {
            start,
            before,
            moved,
            touched: last,
        }) = &mut self.rubber_band
        else {
            return;
        };
        let start = origin + *start;
        let Some(p) = pos.filter(|_| down) else {
            // Released: the size of what is selected, once.
            if std::mem::take(moved) {
                self.selection_changed(&ctx);
            }
            self.rubber_band = None;
            return;
        };
        // Something else is being dragged (the panel's edge, a scroll bar): not a rectangle.
        if !*moved && ctx.dragged_id().is_some() {
            self.rubber_band = None;
            return;
        }
        *moved |= p.distance(start) > 6.0;
        if !*moved {
            return;
        }
        let band = egui::Rect::from_two_pos(start, p);
        // Every cell under it, also those scrolled out of view, from where they are in the grid.
        let rel = band.translate(-origin.to_vec2());
        let rows = touched(rel.top(), rel.bottom(), pitch, row_h);
        let columns = touched(rel.left(), rel.right(), cell.x + gap, cell.x);
        // Made again only when it touches other cells: the selection before it may be large
        // (Ctrl+A in a big folder, then Ctrl and a rectangle), and frames come many a second.
        let touches = [rows.start, rows.end, columns.start, columns.end];
        if *last != Some(touches) {
            *last = Some(touches);
            let mut selected = before.clone();
            for r in rows {
                for c in columns.start..columns.end.min(cols) {
                    let k = r * cols + c;
                    if k < count {
                        selected.insert(self.matches.as_ref().map_or(k, |m| m[k]));
                    }
                }
            }
            self.selected = selected;
        }
        let v = ui.visuals();
        let painter = ui.painter().with_clip_rect(clip);
        painter.rect_filled(band, 2.0, v.selection.bg_fill.gamma_multiply(0.25));
        let stroke = egui::Stroke::new(1.0, v.selection.bg_fill);
        painter.rect_stroke(band, 2.0, stroke, egui::StrokeKind::Inside);
        // Near the top or bottom edge, the grid scrolls on.
        if p.y < clip.top() + row_h / 2.0 {
            ui.scroll_with_delta(egui::vec2(0.0, 10.0));
        } else if p.y > clip.bottom() - row_h / 2.0 {
            ui.scroll_with_delta(egui::vec2(0.0, -10.0));
        }
        ctx.request_repaint();
    }

    /// Row `i` of a collection dragged to another place: the selection when it is in it, else
    /// the row; a line in the accent colour shows where the images go.
    fn drag_row(
        &mut self,
        ui: &egui::Ui,
        row: &egui::Response,
        i: usize,
        icons: bool,
        pointer: Option<egui::Pos2>,
    ) {
        let row = row.interact(egui::Sense::drag());
        if row.drag_started() {
            let picked = if self.selected.contains(&i) {
                self.selected.iter().copied().collect()
            } else {
                HashSet::from([i])
            };
            self.dragging = Some((picked, None));
        }
        let Some((_, to)) = &mut self.dragging else {
            return;
        };
        let r = row.rect.expand(ui.spacing().item_spacing.y / 2.0);
        let Some(p) = pointer.filter(|p| r.contains(*p)) else {
            return;
        };
        let accent = egui::Stroke::new(2.0, ui.visuals().selection.bg_fill);
        let before = if icons {
            p.x < r.center().x
        } else {
            p.y < r.center().y
        };
        *to = Some(i + !before as usize);
        let p = ui.painter();
        let _ = match (icons, before) {
            (true, true) => p.vline(r.left(), r.y_range(), accent),
            (true, false) => p.vline(r.right(), r.y_range(), accent),
            (false, true) => p.hline(r.x_range(), r.top(), accent),
            (false, false) => p.hline(r.x_range(), r.bottom(), accent),
        };
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
        scroll_menu(ui, tr("Add to Collection"), |ui| {
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

    /// Edit → Plugins: run one on the selected files, or the shown one, in the background;
    /// they are read again afterwards, as plugins change them.
    fn plugins_menu(&mut self, ui: &mut egui::Ui) {
        scroll_menu(ui, tr("Plugins"), |ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
            let dir = xdg_dir("XDG_DATA_HOME", ".local/share", "glimt").map(|d| d.join("plugins"));
            if self.plugins.is_empty() {
                ui.weak(tr("No plugins"));
            }
            let mut chosen = None;
            for (k, plugin) in self.plugins.iter().enumerate() {
                if ui.button(&plugin.name).clicked() {
                    chosen = Some(k);
                }
            }
            ui.separator();
            if ui
                .button(tr("Open the Plugins Folder"))
                .on_hover_text(tr(tr(
                    "Desktop files there, as Geeqie's, show in this menu after a restart",
                )))
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
            // A turned image still being saved would be written over the plugin's edit.
            if !self.none_saving(&files) {
                return;
            }
            let plugin = self.plugins[k].clone();
            let dir = self.dir.clone();
            let (edited, deleted) = (self.edited_tx.clone(), self.deleted_tx.clone());
            // Not waited for at closing: a plugin may be an editor that stays open for long.
            self.background(ui.ctx(), false, move || {
                let result = plugin.run(&files, &dir);
                for f in files {
                    let _ = edited.send(f);
                }
                let _ = deleted.send(Deleted {
                    failed: true,
                    ..Default::default()
                });
                result.err().map(|e| format!("{}: {e}", plugin.name))
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
        scroll_menu(ui, tr("Open With"), |ui| {
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
                ui.label(tr("No applications found"));
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
                        Err(e) => drop(tx.send(tf!("gio: {e}", e))),
                    }
                });
            }
        });
    }

    /// Open the shown file in `player` (a command line), or in the default application when
    /// `player` is empty. Runs in its own thread, which waits for the program to exit.
    fn launch(&self, player: &str) {
        if self.files.is_empty() {
            return;
        }
        let (path, tx) = (self.path(self.cur), self.notices_tx.clone());
        let mut words = plugins::words(player);
        std::thread::spawn(move || {
            let run = |cmd: &str, args: &[String]| {
                let out = command(cmd).args(args).arg(&path).output();
                out.map_err(|e| format!("{cmd}: {e}"))
            };
            let out = if words.is_empty() {
                // gio is part of glib; xdg-open covers systems without it.
                match external::host_has("gio") {
                    true => run("gio", &["open".into()]),
                    false => run("xdg-open", &[]),
                }
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
                // The wheel and dragging work over every pane, and the compared half.
                let several = panes.len() > 1 || right.is_some();
                let input = if several { ui.max_rect() } else { area };
                let response = ui.allocate_rect(input, egui::Sense::click_and_drag());
                #[cfg(feature = "accessibility")]
                self.speak_image(ui, &response);
                let max_fit = if self.config.enlarge {
                    f32::INFINITY
                } else {
                    1.0
                };
                // Size on screen after a quarter turn.
                let quarter = self.orient.0 % 2 == 1;
                let turned = |[w, h]: [usize; 2]| if quarter { [h, w] } else { [w, h] };
                let mode = self.config.fit_mode;
                let fit = self.slots.get(&self.cur).map_or(1.0, |s| {
                    let [w, h] = turned(s.orig);
                    fit_scale(area.size() * ppp, [w as f32, h as f32], mode).min(max_fit)
                });
                let long = self.decode_size(area, ppp, max_fit);
                if response.hovered()
                    && !self.dialog_open()
                    && let Some(step) = self.wheel_input(ctx, &response, fit, area, &panes, right)
                {
                    later = Some(step);
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
                        self.pan_offset += response.drag_delta();
                    }
                    let size = egui::vec2(w as f32, h as f32) * scale / ppp;
                    let max = ((size - area.size()) / 2.0).max(egui::Vec2::ZERO);
                    self.pan_offset = self.pan_offset.clamp(-max, max);
                    self.max_pan = max;
                }

                // Texture too small: in fit mode the window has grown, so decode again at window
                // size (the previous texture is shown meanwhile); when zoomed, load full resolution.
                let slot = self.slots.get(&self.cur);
                let too_small = slot.is_some_and(|s| {
                    let needed = (s.orig[0] as f32 * scale).min(s.orig[0] as f32);
                    !s.full && (s.tex.size()[0] as f32) + 1.0 < needed
                });
                // Not zoomed: decoded again to the size shown, which the decoders' limit now
                // holds; full resolution only where that is larger than a texture may be.
                let fits = slot.is_some_and(|s| fits_texture(s.orig, scale, long));
                if too_small && self.zoom.is_none() && fits {
                    self.slots.remove(&self.cur);
                    self.request(self.cur, false);
                } else if too_small {
                    self.request(self.cur, true);
                }
                self.paint_shown(ctx, ui, area, scale, max_fit);
                // A video: a play button over the preview frame (click or Enter), readable on
                // any frame, also in the slideshow and full screen.
                #[cfg(feature = "video")]
                if (self.files.get(self.cur)).is_some_and(|f| is_video(Path::new(f)))
                    && play_button(ui, area)
                {
                    later = Some(Action::Play);
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
                self.paint_notice(ui, area);
            });
        if let Some(action) = later {
            self.run(ctx, action);
        }
        if let Some(i) = clicked_pane {
            self.go(ctx, i);
        }
    }

    /// The view's box the shown image is decoded into (`view_box`), from the fit mode and, in
    /// Fill, the image's shape; returns the longest side a texture may have.
    fn decode_size(&mut self, area: egui::Rect, ppp: f32, max_fit: f32) -> u32 {
        // The image area, not the whole screen: a small window on a 4K screen needs a fraction
        // of the memory (3840×2160 is 33 MB per image).
        let [w, h] = [(area.width() * ppp) as u32, (area.height() * ppp) as u32];
        let quarter = self.orient.0 % 2 == 1;
        let turned = |[w, h]: [usize; 2]| if quarter { [h, w] } else { [w, h] };
        let mode = self.config.fit_mode;
        // Decoded to the size shown: the view for a whole image. Fit Width and Fit Height
        // fill one side and may need up to the screen on the other, whatever the image,
        // so the neighbours read ahead are sharp too. Fill depends on the image's shape:
        // its size at its scale once that is known (also while it is decoded again, from
        // the image drawn), else the longer side.
        let long = self.max_side;
        let orig =
            (self.slots.get(&self.cur).map(|s| s.orig)).or(self.last_drawn.as_ref().map(|d| d.1));
        let view = match (mode, orig) {
            (0, _) => [w, h],
            (3, Some([ow, oh])) => {
                let [tw, th] = turned([ow, oh]);
                let s = fit_scale(area.size() * ppp, [tw as f32, th as f32], mode);
                let size = |side: usize| (side as f32 * s.min(max_fit)).ceil() as u32;
                [size(ow).clamp(1, long), size(oh).clamp(1, long)]
            }
            (1, _) => [w, long],
            (2, _) => [long, h],
            _ => [w.max(h); 2],
        };
        (self.view_box, self.view_area) = (view, [w, h]);
        long
    }

    /// The box file `idx` is shrunk into for the view and the projector, as `decode_box`.
    fn decode_box(&self, idx: usize) -> [u32; 2] {
        decode_box(
            self.view_box,
            self.view_area,
            self.config.fit_mode == 3,
            idx == self.cur,
            self.orient.0 % 2 == 1,
            self.projector_size,
        )
    }

    /// The shown image in `area` at `scale`, turned as the view is, fading from the one before
    /// in a slideshow; while it is decoded its thumbnail or the image before, else why not.
    fn paint_shown(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        area: egui::Rect,
        scale: f32,
        max_fit: f32,
    ) {
        let ppp = ctx.pixels_per_point();
        let mode = self.config.fit_mode;
        let now = self.slots.get(&self.cur).map(|s| s.tex.id());
        let fade = now.and_then(|id| self.fading(ctx, 0, id));
        if let Some(slot) = self.slots.get(&self.cur) {
            let size = egui::vec2(slot.orig[0] as f32, slot.orig[1] as f32) * scale / ppp;
            let rect = egui::Rect::from_center_size(area.center() + self.pan_offset, size);
            let tex = self.texture(slot, ctx);
            // Within its pane or half, which zoom makes it overflow.
            let clip = ui.clip_rect();
            ui.set_clip_rect(area.intersect(clip));
            // A slideshow fading: the image before goes as this one comes.
            let shown = match &fade {
                Some((t, old, orig)) => {
                    let look = (self.config.enlarge, mode);
                    let zoom = self.zoom.map(|z| (z, self.pan_offset));
                    let at = placed(area, *orig, ppp, look, zoom).0;
                    let uv1 = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
                    ui.painter()
                        .image(old.id(), at, uv1, alpha(1.0 - t.powi(4)));
                    *t
                }
                None => 1.0,
            };
            paint_turned(ui, tex, rect, self.orient, alpha(shown));
            ui.set_clip_rect(clip);
            self.last_drawn = Some((slot.tex.clone(), slot.orig));
            // For the projector: this view, and the pointer from the image's centre.
            let centre = area.center() + self.pan_offset;
            let pointer = (ctx.pointer_hover_pos())
                .filter(|p| area.contains(*p))
                .map(|p| p - centre);
            self.last_view = Some(View {
                area,
                ppp,
                scale,
                pan: self.pan_offset,
                zoomed: self.zoom.is_some(),
                pointer,
            });
        } else if let Some((tex, orig, thumb)) = self.placeholder()
            && !self.decode_errors.contains_key(&self.cur)
        {
            // A thumbnail fills the area, as the image will; the image before keeps
            // its size.
            let size = egui::vec2(orig[0] as f32, orig[1] as f32);
            let fit = (area.width() * ppp / size.x).min(area.height() * ppp / size.y);
            let fit = if thumb { fit } else { fit.min(max_fit) };
            let rect = egui::Rect::from_center_size(area.center(), size * fit / ppp);
            egui::Image::new(&tex).paint_at(ui, rect);
        } else {
            let text = if self.files.is_empty() {
                if self.listing_rx.is_some() {
                    tr("Reading folder…")
                } else {
                    tr("No images in folder")
                }
                .to_owned()
            } else if let Some(e) = self.decode_errors.get(&self.cur) {
                format!("Could not open {}:\n{e}", self.path(self.cur).display())
            } else {
                String::new()
            };
            // Broken into lines that fit, as an error can be long: a decoder's message.
            let (font, grey) = (egui::FontId::proportional(16.0), egui::Color32::GRAY);
            let width = (area.width() - 32.0).max(80.0);
            let galley = ui.painter().layout(text, font, grey, width);
            let at = area.center() - galley.size() / 2.0;
            ui.painter().galley(at, galley, grey);
        }
    }

    /// The notice at the foot of `area`, wrapped to it, on a dark band so it reads over any image.
    fn paint_notice(&self, ui: &egui::Ui, area: egui::Rect) {
        if let Some(notice) = &self.notice {
            let (font, red) = (egui::FontId::proportional(16.0), egui::Color32::LIGHT_RED);
            let width = (area.width() - 48.0).max(80.0);
            let galley = ui.painter().layout(notice.clone(), font, red, width);
            let at =
                area.center_bottom() - egui::vec2(galley.size().x / 2.0, 16.0 + galley.size().y);
            let band = egui::Rect::from_min_size(at, galley.size()).expand(8.0);
            ui.painter()
                .rect_filled(band, 6.0, egui::Color32::from_black_alpha(200));
            ui.painter().galley(at, galley, red);
        }
    }

    /// The wheel over the image: zoom about the point under the pointer (Ctrl+wheel, or every
    /// wheel with wheel_zoom), else a notch is the next or previous image, which it returns.
    /// `fit` is the zoom that fits, `area` the first pane (or compared half), `right` the
    /// other half when comparing.
    fn wheel_input(
        &mut self,
        ctx: &egui::Context,
        response: &egui::Response,
        fit: f32,
        area: egui::Rect,
        panes: &[egui::Rect],
        right: Option<egui::Rect>,
    ) -> Option<Action> {
        let mut step = None;
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
                step = Some(if up { Action::Prev } else { Action::Next });
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
                let under = (right.iter().chain(panes))
                    .find(|r| r.contains(p))
                    .map_or(area, |r| if *r == panes[0] { area } else { *r });
                let from_centre = p - under.center();
                self.pan_offset = from_centre - (from_centre - self.pan_offset) * (new / old);
            }
        }
        step
    }
}

/// A play button over a video's preview frame in `area`, readable on any frame; whether it
/// was clicked.
#[cfg(feature = "video")]
fn play_button(ui: &egui::Ui, area: egui::Rect) -> bool {
    let shade = egui::Color32::from_black_alpha(150);
    let button = egui::Rect::from_center_size(area.center(), egui::vec2(96.0, 96.0));
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
        tr("Video: Enter or click to play").into(),
        egui::FontId::proportional(16.0),
        egui::Color32::WHITE,
    );
    let at = egui::Rect::from_center_size(
        button.center_bottom() + egui::vec2(0.0, 28.0),
        label.size() + egui::vec2(20.0, 10.0),
    );
    p.rect_filled(at, 6.0, shade);
    p.galley(at.min + egui::vec2(10.0, 5.0), label, egui::Color32::WHITE);
    play.clicked()
}

impl eframe::App for App {
    /// The last folder is written once on exit, not on every folder change.
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        remote::stop();
        self.save_meta();
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

impl App {
    fn frame_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = &ui.ctx().clone();
        self.sync_thumb_source(ctx.pixels_per_point());
        self.poll_background(ctx);
        let mut act = self.read_keys(ctx);
        // Full screen and Hide File List as in Geeqie: only the image, no menu or panels.
        let fullscreen = self.track_window(ctx);
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
        if !bare && self.config.exif_panel && !self.files.is_empty() {
            self.exif_panel(ui);
        } else {
            // Hidden while a field had the focus: it never lost it.
            self.save_meta();
        }
        self.image_area(ctx, ui, fullscreen, bare);
        if self.projector {
            self.projector_window(ctx);
        }
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

/// A level of the keyword tree: its children by name.
#[derive(Default)]
struct KeywordNode(std::collections::BTreeMap<String, KeywordNode>);

impl KeywordNode {
    fn insert(&mut self, path: &str) {
        let mut node = self;
        for level in path.split('|') {
            node = node.0.entry(level.to_owned()).or_default();
        }
    }

    /// The boxes of this level's keywords under `prefix`, ticked by `have` (the image's keywords
    /// and keyword tree paths); a box clicked
    /// gives (its path, on) in `tick`, and Remove in its menu gives the path in `drop`.
    fn show(
        &self,
        ui: &mut egui::Ui,
        prefix: &str,
        have: (&[String], &[String]),
        tick: &mut Option<(String, bool)>,
        drop: &mut Option<String>,
    ) {
        for (name, child) in &self.0 {
            let path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}|{name}")
            };
            let mut box_of = |ui: &mut egui::Ui| {
                let mut on = meta::has_tag(have.0, have.1, &path);
                let response = ui.checkbox(&mut on, name);
                if response.changed() {
                    *tick = Some((path.clone(), on));
                }
                response.context_menu(|ui| {
                    if ui.button(tr("Remove from the tree")).clicked() {
                        *drop = Some(path.clone());
                    }
                });
            };
            if child.0.is_empty() {
                box_of(ui);
            } else {
                let id = ui.make_persistent_id(("keyword", &path));
                egui::collapsing_header::CollapsingState::load_with_default_open(
                    ui.ctx(),
                    id,
                    false,
                )
                .show_header(ui, box_of)
                .body(|ui| child.show(ui, &path, have, tick, drop));
            }
        }
    }
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
    let problems = match std::fs::read_to_string(&path) {
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

/// The sign of a file Check Images found broken: an orange disc with "!", at `centre`.
fn broken_mark(p: &egui::Painter, centre: egui::Pos2, text_size: f32) {
    p.circle_filled(centre, 7.0, egui::Color32::from_rgb(240, 140, 0));
    let bold = egui::FontId::proportional(text_size * 0.85);
    p.text(
        centre,
        egui::Align2::CENTER_CENTER,
        "!",
        bold,
        egui::Color32::BLACK,
    );
}

/// The places along a row or column of items `size` long, one every `step`, that the span
/// `lo..hi` (from the first item's start) overlaps.
fn touched(lo: f32, hi: f32, step: f32, size: f32) -> std::ops::Range<usize> {
    let first = ((lo - size) / step).floor() + 1.0;
    first.max(0.0) as usize..(hi / step).ceil().max(0.0) as usize
}

/// `template` (see OSD_TEMPLATE) with each `%field%` replaced by `value(field)`.
fn fill_template(template: &str, value: impl Fn(&str) -> String) -> String {
    let lines = template.split("\\n").filter_map(|line| {
        let parts: Vec<String> = (line.split('|'))
            .filter_map(|part| {
                let (mut out, mut rest, mut fields, mut filled) = (String::new(), part, 0, 0);
                while let Some(a) = rest.find('%')
                    && let Some(b) = rest[a + 1..].find('%')
                {
                    let v = value(&rest[a + 1..a + 1 + b]);
                    fields += 1;
                    filled += !v.is_empty() as usize;
                    out += &rest[..a];
                    out += &v;
                    rest = &rest[a + b + 2..];
                }
                out += rest;
                (fields == 0 || filled > 0).then_some(out)
            })
            .filter(|p| !p.trim().is_empty())
            .collect();
        (!parts.is_empty()).then(|| parts.join(" · "))
    });
    lines.collect::<Vec<_>>().join("\n")
}

#[repr(C)]
struct Timespec {
    sec: i64,
    nsec: i64,
}

unsafe extern "C" {
    fn clock_gettime(clock: i32, time: *mut Timespec) -> i32;
}

/// The processor time this thread has used, in microseconds: a frame's own work, which other
/// threads busy at the same moment (summing the sizes of 500,000 files) do not add to.
fn thread_cpu_us() -> u64 {
    const CLOCK_THREAD_CPUTIME_ID: i32 = 3;
    let mut t = Timespec { sec: 0, nsec: 0 };
    // SAFETY: `t` is a valid timespec for the call to fill.
    unsafe { clock_gettime(CLOCK_THREAD_CPUTIME_ID, &mut t) };
    t.sec as u64 * 1_000_000 + t.nsec as u64 / 1000
}

/// The milliseconds that `share` of the frames counted by milliseconds took at most; 0 for none.
fn frame_at(frame_ms: &[u32], share: f64) -> usize {
    let total: u32 = frame_ms.iter().sum();
    let mut seen = 0;
    (frame_ms.iter().position(|&n| {
        seen += n;
        total > 0 && seen as f64 >= share * total as f64
    }))
    .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    /// The median, 90 % and the slowest frame from the counts by milliseconds.
    #[test]
    fn frame_shares() {
        let mut ms = [0; 256];
        assert_eq!(super::frame_at(&ms, 0.5), 0);
        ms[2] = 5;
        ms[3] = 4;
        ms[90] = 1;
        let at = |share| super::frame_at(&ms, share);
        assert_eq!((at(0.5), at(0.9), at(1.0)), (2, 3, 90));
    }

    /// The busy pointer goes when its work ends, also by a panic.
    #[test]
    fn busy_ends_with_a_panic() {
        use std::sync::{Arc, atomic::AtomicUsize, atomic::Ordering};
        let count = Arc::new(AtomicUsize::new(0));
        let pointer = super::Working::new(&count);
        assert_eq!(count.load(Ordering::Relaxed), 1);
        let _ = std::thread::spawn(move || {
            let _pointer = pointer;
            panic!("a broken file");
        })
        .join();
        assert_eq!(count.load(Ordering::Relaxed), 0);
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

    /// The fit modes: a 4:3 image in a 2:1 view, the whole, the width, the height, filled.
    #[test]
    fn fits_in_each_mode() {
        let view = eframe::egui::vec2(200.0, 100.0);
        let fits = [0, 1, 2, 3].map(|m| super::fit_scale(view, [400.0, 300.0], m));
        assert_eq!(fits, [1.0 / 3.0, 0.5, 1.0 / 3.0, 0.5]);
    }

    /// Images of a collection moved: each one place, never past another moved one, and to the
    /// start or the end in their own order.
    /// A photo shown larger after the window grew is decoded again at the size shown, whatever
    /// the fit mode; only one shown larger than a texture may be needs full resolution.
    #[test]
    fn decodes_again_at_the_size_shown() {
        // 6000×4000 shown at 0.5 in a texture of at most 16384: 3000×2000 fits.
        assert!(super::fits_texture([6000, 4000], 0.5, 16384));
        // A panorama shown 20,000 pixels wide does not.
        assert!(!super::fits_texture([40000, 2000], 0.5, 16384));
    }

    /// egui-winit sends Ctrl+C, with or without Shift, as a Copy event: Copy to a folder, or
    /// with Shift the image to the clipboard.
    #[test]
    fn copy_event_with_shift_copies_the_image() {
        use eframe::egui;
        let pressed = |shift| {
            let ctx = egui::Context::default();
            let modifiers = egui::Modifiers {
                shift,
                ..super::CTRL
            };
            ctx.begin_pass(egui::RawInput {
                events: vec![egui::Event::ModifiersChanged(modifiers), egui::Event::Copy],
                ..Default::default()
            });
            super::App::shortcut(&ctx, true, false)
        };
        assert_eq!(pressed(false), Some(super::Action::Copy));
        assert_eq!(pressed(true), Some(super::Action::CopyImage));
    }

    /// The box turns with the shown image only, Fill has the turn in its view already, a Fill
    /// neighbour gets the longer side's square, and the projector counts, turned with the image.
    #[test]
    fn decode_box_turns_with_the_shown_image_only() {
        let b = |shown, quarter, fill| {
            super::decode_box([1920, 1080], [1920, 1080], fill, shown, quarter, [0, 0])
        };
        assert_eq!(b(true, false, false), [1920, 1080]);
        assert_eq!(b(true, true, false), [1080, 1920]);
        assert_eq!(b(false, true, false), [1920, 1080]);
        assert_eq!(b(false, false, true), [1920, 1920]);
        assert_eq!(b(true, true, true), [1920, 1080]);
        let with_projector =
            super::decode_box([800, 600], [800, 600], false, true, true, [1920, 1080]);
        assert_eq!(with_projector, [1080, 1920]);
        let neighbour = super::decode_box([800, 600], [800, 600], false, false, true, [1920, 1080]);
        assert_eq!(neighbour, [1920, 1080]);
    }

    /// A slideshow goes on, tick after tick, whatever the layout, order and search: the image
    /// shown is another one after every step, a round of a random order shuffles once and goes
    /// on, a search that hides the image or shows one image never holds it, and without repeat
    /// it stops at the end of the round and not before.
    #[test]
    fn slideshow_advances_on_every_tick() {
        use super::{Place, Slide, next_row, next_slide};
        // (files, rows shown by a search or all, step: the panes changed at once)
        let cases: Vec<(usize, Option<Vec<usize>>, usize)> = vec![
            (3, None, 1),
            (7, None, 1),
            (7, None, 4),
            (3, None, 4),
            (7, Some(vec![1, 3, 5]), 1),
            (7, Some(vec![1, 3, 5]), 4),
            (7, Some(vec![6]), 1),
            (7, Some(vec![]), 1),
        ];
        for (len, matches, delta) in cases {
            for (repeat, shuffled) in [(true, false), (true, true), (false, false)] {
                let m = matches.as_deref();
                let shown = m.map_or(len, <[usize]>::len);
                let (mut from, mut steps, mut shuffles, mut stopped) = (0usize, 0, 0, false);
                for _ in 0..30 {
                    let row = m.map_or(Some(from), |m| m.iter().position(|&i| i == from));
                    let next =
                        next_row(row, delta as isize, shown, true).map(|r| m.map_or(r, |m| m[r]));
                    let case = format!(
                        "{len} files, {m:?} shown, step {delta}, repeat {repeat}, shuffled {shuffled}, from {from}"
                    );
                    let at = Place {
                        row,
                        delta,
                        shown,
                        len,
                    };
                    match next_slide(at, next, repeat, shuffled, false) {
                        Slide::Hold => assert_eq!(shown, 0, "held: {case}"),
                        Slide::Stop => {
                            assert!(!repeat, "stopped: {case}");
                            stopped = true;
                            break;
                        }
                        Slide::Shuffle => {
                            shuffles += 1;
                            assert!(shuffles == 1 || steps > 0, "shuffled again at once: {case}");
                            // As shuffle_list: the shown image first in the new order.
                            (from, steps) = (0, 0);
                        }
                        Slide::Step(next) => {
                            assert!(
                                next < len && (next != from || shown == 1),
                                "no advance: {case}"
                            );
                            from = next;
                            steps += 1;
                        }
                    }
                }
                if !repeat && shown > 0 {
                    assert!(stopped, "never stopped: {len} files, {m:?}, step {delta}");
                }
            }
        }
        // Held only while the next image is still being decoded.
        let at = Place {
            row: Some(0),
            delta: 1,
            shown: 3,
            len: 3,
        };
        assert_eq!(next_slide(at, Some(1), true, false, true), Slide::Hold);
        assert_eq!(next_row(None, 1, 3, true), Some(0));
        assert_eq!(next_row(Some(2), 1, 3, false), Some(2));
    }

    #[test]
    fn slideshow_round_ends_past_the_last() {
        // Ten images in three panes: 0, 3, 6, 9, then a new round.
        assert!(!super::round_ends(Some(6), 3, 10));
        assert!(super::round_ends(Some(9), 3, 10));
        assert!(super::round_ends(Some(8), 3, 10));
        assert!(super::round_ends(Some(0), 1, 1));
        assert!(super::round_ends(None, 1, 5));
    }

    /// Cells 100 high every 110: a band from 50 to 230 touches rows 0, 1 and 2; one in the gap
    /// from 101 to 109 none; one above the grid none.
    #[test]
    fn finds_the_cells_a_rectangle_touches() {
        use super::touched;
        assert_eq!(touched(50.0, 230.0, 110.0, 100.0), 0..3);
        assert_eq!(touched(101.0, 109.0, 110.0, 100.0), 1..1);
        assert_eq!(touched(-80.0, -10.0, 110.0, 100.0), 0..0);
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

    /// The overlay template: empty fields take their part along, an empty line goes.
    #[test]
    fn fills_templates() {
        let value = |f: &str| match f {
            "name" => "a.jpg".to_owned(),
            "ISO" => "200".to_owned(),
            _ => String::new(),
        };
        let fill = |t| super::fill_template(t, value);
        assert_eq!(fill(super::OSD_TEMPLATE), "a.jpg  \nISO 200");
        assert_eq!(
            fill("%Taken%|x %name%|%comment%\\n%keywords%\\n50%"),
            "x a.jpg\n50%"
        );
        assert_eq!(fill(""), "");
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
        assert_eq!(super::count(1, "image"), "1 image");
        // One letter is one unit wide here.
        let fits = |n: usize| move |s: &str| s.chars().count() <= n;
        assert_eq!(
            super::middle_cut("fjord_sunrise.jpg", fits(20)),
            "fjord_sunrise.jpg"
        );
        assert_eq!(
            super::middle_cut("fjord_sunrise.jpg", fits(11)),
            "fjord…e.jpg"
        );
        assert_eq!(super::middle_cut("fjørd_sunrise.jpg", fits(5)), "fj…pg");
        assert_eq!(super::middle_cut("abc", fits(0)), "…");
        assert_eq!(super::count(0, "image"), "0 images");
        assert_eq!(super::count(1200, "file"), "1,200 files");
    }

    #[test]
    fn formats_local_time() {
        let t = super::local_time(1_700_000_000);
        assert_eq!(t.len(), "2023-11-14 22:13:20".len(), "{t}");
        assert!(t.starts_with("2023-11-1"), "{t}");
    }
}
