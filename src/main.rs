//! Glimt: a fast, memory-frugal image viewer.
//!
//! Images are decoded in background threads to the size of the image area; full resolution
//! is decoded only when the user zooms past what that covers. Only a small window around
//! the current image is kept in memory.

use crate::file_list::RubberBand;
use crate::info_panel::ExifRead;
use crate::info_panel::MetaEdit;
use crate::info_panel::spawn_exif_reader;
use crate::selection::round_ends;
use crate::view::DecodeJob;
use crate::view::Shown;
use crate::view::View;
use crate::view::layout_grid;
use crate::view::spawn_decoders;
use actions::{Action, CTRL, CTRL_SHIFT, OWN_KEYS, SHORTCUTS, shortcut_text, shortcuts};
use check::Check;
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
use decode::{Decoded, Hist, MAX_ANIMATION, color_image, decode, histogram, survive};
use duplicates::Dups;
use eframe::egui;
use external::{Login, Mounted, apps_for, command, gio_mount, is_video, open_url};
use file_ops::RenameJob;
use files::{
    SORTS, Types, has_ext, is_listed, list_folder, list_tree, merge_shuffled, pattern_name,
    shuffle, sort_files, walk,
};
use format::{count, fill_template, human_size, local_time, middle_cut, size_text, thousands};
use lang::tr;
use meta::Meta;
use pan::Pan;
use print::{PrintDialog, PrintJob, PrintTo};
use save::save_rotated;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Instant;
use threads::{
    CPU_CHANGED, ONE_ARENA, Receiving, Took, Working, background_threads, cores, in_parts,
    set_cpu_use, small_alloc_threads, thread_cpu_us, thumb_threads,
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
mod theme;
mod threads;
mod thumbs;
#[cfg(feature = "video")]
mod video;
mod view;

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

/// Whether a window, menu or popup was on the screen in this frame or the last; the panels
/// of the main window do not count.
fn window_shown(ctx: &egui::Context) -> bool {
    use egui::Order::{Foreground, Middle};
    ctx.memory(|m| {
        (m.areas().visible_layer_ids().iter()).any(|l| matches!(l.order, Middle | Foreground))
    })
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

/// What a delete, move, copy or plugin reports, in one message: the files it was asked to
/// delete or move, those gone now (found out in its thread, not one `stat` each in the
/// window's), whether to read the folder again, and the files moved or copied `(from, to)`,
/// whose marks follow before the gone ones are forgotten; the Undo it makes possible, the
/// marks an Undo puts back, and files changed or written over. The one report of what happened
/// to files on disk, and `receive_deleted` the one place that brings everything that follows a
/// file along: marks, the collection, a turn not saved, decoded images, thumbnails, details and
/// the `exif:` index. One message, so nothing depends on the order two would arrive in.
#[derive(Default)]
struct Deleted {
    paths: Vec<PathBuf>,
    gone: Vec<PathBuf>,
    failed: bool,
    moved: Vec<(PathBuf, PathBuf)>,
    copied: bool,
    /// The folder is to be read again: a plugin edited files in it, a repair renamed them.
    relist: bool,
    /// What Undo puts back, when this was a move to the trash or a move.
    undo: Option<Undo>,
    /// Marks and stars to put back: of files Undo brought back from the trash.
    restored_marks: Vec<(PathBuf, (u8, u8))>,
    /// Files a plugin changed in place: what is shown of them is read again. (A turn saved
    /// comes through `saved_rx`, which also ends its guard, into the same `files_changed`.)
    changed: Vec<PathBuf>,
    /// Files written over by another image (Save As, Save This Frame onto a file there): read
    /// again, and their marks and stars go, as their sidecar did.
    overwritten: Vec<PathBuf>,
    /// A folder renamed (old, new), which the window follows.
    folder_renamed: Option<(PathBuf, PathBuf)>,
    /// A folder made (New Folder): the tree reads its parent again, the list its folder.
    folder_made: Option<PathBuf>,
}

/// What Edit → Undo puts back: the last files or folder moved to the trash, the last files
/// moved `(from, to)`, or the last renamed (old names, new names).
enum Undo {
    Trash(file_ops::Thrown),
    Move(Vec<(PathBuf, PathBuf)>),
    Rename(Vec<PathBuf>, Vec<PathBuf>),
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

/// How long the mouse is still in full screen before the pointer hides.
const POINTER_STILL: std::time::Duration = std::time::Duration::from_secs(2);

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
    /// Save As: where to save the copy .1 of the file .0, starting at the name .2.
    SaveAs(PathBuf, save::CopyAs, PathBuf),
    /// The folder for every frame (or page) of the file, as PNG files.
    ExportFrames(PathBuf),
}

/// What ends a screen saver inhibitor: its process killed, or the portal's request closed.
type Ender = Box<dyn FnOnce() + Send>;

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
    /// This window's own title, a template as in Preferences.
    WindowTitle,
}

/// A menu that scrolls when it is taller than the window, instead of running off it.
fn scroll_menu<R>(
    ui: &mut egui::Ui,
    title: impl Into<egui::WidgetText>,
    add: impl FnOnce(&mut egui::Ui) -> R,
) {
    let title: egui::WidgetText = title.into();
    let name = title.text().to_owned();
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
        noted(&ui.menu_button(title, content).response, &name);
        return;
    }
    // A menu of the menu bar. While one of them is open, pointing at another opens that one,
    // as in GNOME and KDE.
    let response = egui::containers::menu::MenuButton::new(title)
        .ui(ui, content)
        .0;
    noted(&response, &name);
    let ctx = ui.ctx().clone();
    let id = egui::Popup::default_response_id(&response);
    let others = ctx.data_mut(|d| {
        let ids = d.get_temp_mut_or_default::<Vec<egui::Id>>(egui::Id::unique(BAR_MENUS));
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
    let ids: Vec<egui::Id> =
        ctx.data(|d| d.get_temp(egui::Id::unique(BAR_MENUS)).unwrap_or_default());
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

/// `text` with the Phosphor `icon` before it, as every row of the menus shows: the icon in the
/// weak colour, the text in the widget's own (Color32::PLACEHOLDER), so a selected row keeps
/// its colours. Phosphor's glyphs are all one em wide, so the texts line up.
fn iconed(ui: &egui::Ui, icon: &str, text: &str) -> egui::text::LayoutJob {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let mut job = egui::text::LayoutJob::default();
    let format = |color| egui::TextFormat {
        font_id: font.clone(),
        color,
        ..Default::default()
    };
    job.append(icon, 0.0, format(ui.visuals().weak_text_color()));
    job.append(text, 8.0, format(egui::Color32::PLACEHOLDER));
    job
}

/// A menu entry showing the action's shortcut; sets `act` when clicked.
fn menu_item(ui: &mut egui::Ui, act: &mut Option<Action>, action: Action) {
    let text = iconed(ui, action.icon(), action.label());
    menu_button(ui, act, egui::Button::new(text), action);
}

/// A menu entry with a check mark when `on`.
fn menu_toggle(ui: &mut egui::Ui, act: &mut Option<Action>, on: bool, action: Action) {
    let mut text = iconed(ui, action.icon(), action.label());
    // On the accent colour of a row that is on, the weak grey would hardly show.
    if on {
        text.sections[0].format.color = egui::Color32::PLACEHOLDER;
    }
    menu_button(ui, act, egui::Button::selectable(on, text), action);
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
    if noted(&ui.add(button), action.label()).clicked() {
        *act = Some(action);
    }
}

/// A toolbar button with the action's name and shortcut as tooltip.
fn tool_button(ui: &mut egui::Ui, act: &mut Option<Action>, icon: &str, action: Action, on: bool) {
    let tip = match shortcut_text(ui.ctx(), action) {
        Some(keys) => format!("{label} ({keys})", label = action.label()),
        None => action.label().to_owned(),
    };
    let response = ui
        .add(egui::Button::selectable(on, icon))
        .on_hover_text(tip);
    if noted(&response, action.label()).clicked() {
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

/// Glimt's logo, 256 pixels square: the window's icon, and on the About page.
const LOGO: &[u8] = include_bytes!("../assets/glimt-256.png");

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
                    thumbs::thumbnail(&path, rotate, save, large)
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
            next_pane: 0,
            capturing: None,
            recursive: false,
            collection: None,
            folders: Vec::new(),
            together: None,
            sort_collection: false,
            dups: None,
            check: None,
            broken_files: HashSet::new(),
            whole_shuffle: false,
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
            slideshow: None,
            slideshow_at_start: start.slideshow,
            shuffled: false,
            view_box: [1920, 1080],
            view_area: [1920, 1080],
            max_side: 8192,
            decode_jobs: jobs,
            decoded_rx: done,
            decodes_sent: 0,
            stale_thumbs: HashSet::new(),
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
            paused: false,
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
            thumbs: HashMap::new(),
            thumbs_pending: HashSet::new(),
            thumb_jobs,
            thumbs_rx,
            thumb_view,
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
        // Small folders are listed before the first frame; large ones hold startup back at most 50 ms.
        app.receive_listing(ctx, std::time::Duration::from_millis(50));
        app
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
            let icon = match name.as_str() {
                "Dark" => theme::icon::MOON,
                "Light" => theme::icon::SUN,
                "Desktop" => theme::icon::MONITOR,
                "Nord" => theme::icon::SNOWFLAKE,
                "High Contrast" => theme::icon::HALF,
                "Contrast" => theme::icon::HALF_TILT,
                _ => theme::icon::BRUSH,
            };
            // Chosen again too, so a theme file just edited shows.
            let radio = ui.radio(self.config.theme == name, iconed(ui, icon, &name));
            if noted(&radio, &name).clicked() {
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
            self.desktop_screen_rx = Receiving::of(read_desktop_screen(ctx));
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

    // ---- Keys and actions ----

    /// Preferences or a modal dialog is open: keys go there.
    /// A question about the selection on screen (Delete, Rename): what it acts on must stay as
    /// it was when asked. Preferences and the like do not count: a script would wait on them.
    fn asking_about_selection(&self) -> bool {
        self.confirm_delete.is_some() || self.prompt.is_some()
    }

    /// Busy pointer while the folder is read (also sorted anew), a share mounted, work done in
    /// the background, or the shown image still decoded.
    fn working(&self) -> bool {
        let decoding = !self.list.files.is_empty()
            && !self.list.slots.contains_key(&self.list.cur)
            && !self.list.decode_errors.contains_key(&self.list.cur);
        decoding || self.working_besides_image()
    }

    /// `working` but for the shown image's decode, which a full-screen slideshow waits on at
    /// every image not preloaded.
    fn working_besides_image(&self) -> bool {
        self.listing_rx.is_some()
            || self.mounting_rx.is_some()
            || self.busy.load(Ordering::Relaxed) > 0
    }

    /// In full screen, with the setting on, the pointer hidden once the mouse has been still for
    /// `POINTER_STILL`; never while work goes on (the busy pointer shows it; the shown image
    /// decoded does not count, or it would flash at each image of a slideshow), nor while a window,
    /// menu or popup shows (Help, the EXIF window, pan view, a dialog): one reads it, and wants
    /// the pointer there at once.
    fn hide_still_pointer(&mut self, ctx: &egui::Context, fullscreen: bool) {
        let moved = ctx.input(|i| {
            i.pointer.delta() != egui::Vec2::ZERO
                || i.pointer.any_down()
                || i.smooth_scroll_delta != egui::Vec2::ZERO
        });
        if moved || !fullscreen {
            self.pointer_moved = std::time::Instant::now();
        }
        if fullscreen && self.config.hide_pointer && self.working_besides_image() {
            // A job may end without a repaint; look again, for the pointer to hide then.
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }
        if !self.pointer_hides(fullscreen) || window_shown(ctx) {
            return;
        }
        match POINTER_STILL.checked_sub(self.pointer_moved.elapsed()) {
            Some(left) if !left.is_zero() => ctx.request_repaint_after(left),
            _ => ctx.set_cursor_icon(egui::CursorIcon::None),
        }
    }

    /// Whether the pointer may be hidden while still: full screen, the setting on, no work going
    /// on but the shown image's decode, and no dialog open.
    fn pointer_hides(&self, fullscreen: bool) -> bool {
        fullscreen
            && self.config.hide_pointer
            && !self.working_besides_image()
            && !self.dialog_open()
    }

    fn dialog_open(&self) -> bool {
        self.preferences_open
            || self.confirm_delete.is_some()
            || self.confirm_folder
            || self.prompt.is_some()
            || self.login.is_some()
            || self.print_window.is_some()
            || self.lightroom.is_some()
            || self.save_as.is_some()
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
        // Without remote control (no $XDG_RUNTIME_DIR, the socket not made) --remote quit
        // cannot come to a kiosk, so Ctrl+Q is its way out then, rather than none.
        if self.kiosk && action == Action::Quit && !remote::listening() {
            self.kiosk_quit = true;
        }
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
            Action::NewCollection if !self.list.files.is_empty() => {
                self.prompt = Some((Prompt::NewCollection, String::new()));
            }
            Action::NewCollection => {
                self.notice = Some(tr("No images for a collection here").into())
            }
            Action::OpenCollection => self.pick(ctx, false, Pick::Collection),
            Action::ImportLightroom => self.pick(ctx, false, Pick::Lightroom),
            Action::RemoveFromCollection
                if self.collection.is_some() && !self.list.files.is_empty() =>
            {
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
                let order = collections::moved(self.list.files.len(), &picked, action);
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
            Action::Copy | Action::Move if !self.list.files.is_empty() => {
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
            Action::SetWallpaper if !self.list.files.is_empty() => {
                let (path, tx) = (self.path(self.list.cur), self.notices_tx.clone());
                std::thread::spawn(move || {
                    if let Err(e) = external::set_wallpaper(&path) {
                        let _ = tx.send(tf!("Could not set the wallpaper: {e}", e));
                    }
                });
            }
            Action::SetWallpaper => {}
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
            Action::Undo => self.undo(ctx),
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
                let shift = ctx.input(|i| i.modifiers.shift);
                if shift {
                    let anchor = *self.list.anchor.get_or_insert(self.list.cur);
                    if !self.move_range_end(anchor, self.list.cur, to) {
                        self.list.selected.clear();
                        self.select_range(anchor, to);
                    }
                    self.selection_changed(ctx);
                } else {
                    self.list.anchor = None;
                    if !self.list.selected.is_empty() {
                        self.list.selected.clear();
                        self.selection_changed(ctx);
                    }
                }
                self.go(ctx, to);
                if shift {
                    self.list.range_end = Some(to);
                }
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
                self.list.selected = self.shown_files().into_iter().collect();
                self.selection_changed(ctx);
            }
            Action::SelectNone => {
                self.list.selected.clear();
                self.selection_changed(ctx);
            }
            Action::InvertSelection => {
                let shown = self.shown_files().into_iter();
                self.list.selected = shown.filter(|i| !self.list.selected.contains(i)).collect();
                self.selection_changed(ctx);
            }
            Action::CopyPath | Action::CopyPathUnquoted if !self.list.files.is_empty() => {
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
            Action::CopyImage if !self.list.files.is_empty() => self.copy_image(ctx),
            Action::CopyImage => {}
            // As a list of files, which a file manager pastes as a copy.
            Action::CopyFiles if !self.list.files.is_empty() => self.copy_files(),
            Action::CopyFiles => {}
            Action::Preferences if config::locked() => {
                self.notice = Some(tr("The settings are locked (--lock-settings)").into());
            }
            Action::Preferences => {
                // A search from last time would hide settings without saying why.
                self.preferences_open = true;
                self.preferences_query.clear();
                self.focus_preferences_search = true;
            }
            Action::Help => self.help = Some(help::Page::Manual),
            Action::Shortcuts => self.help = Some(help::Page::Shortcuts),
            Action::KeyboardMap => self.help = Some(help::Page::Keymap),
            Action::About => self.help = Some(help::Page::About),
            Action::Thumbnails => self.set_thumbnails(!self.config.thumbnails),
            Action::SaveOrientation => self.save_orientation(ctx),
            Action::Crop => self.start_crop(),
            Action::SaveAs => self.start_save_as(),
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
                if self
                    .list
                    .slots
                    .get(&self.list.cur)
                    .is_some_and(|s| s.anim.len() > 1) =>
            {
                let n = self
                    .list
                    .slots
                    .get(&self.list.cur)
                    .map_or(1, |s| s.anim.len());
                let at = self.viewing.frame.unwrap_or_else(|| self.playing_frame());
                self.viewing.frame = Some(match action {
                    Action::NextPage => (at + 1) % n,
                    _ => (at + n - 1) % n,
                });
            }
            Action::NextPage | Action::PrevPage => {
                let pages = self.list.slots.get(&self.list.cur).map_or(1, |s| s.pages);
                let page = match action {
                    Action::NextPage => (self.viewing.page + 1).min(pages - 1),
                    _ => self.viewing.page.saturating_sub(1),
                };
                if page != self.viewing.page {
                    self.viewing.page = page;
                    self.list.slots.remove(&self.list.cur);
                    self.list
                        .decodes_pending
                        .retain(|p, _| p.0 != self.list.cur);
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
                    .then(|| self.list.panes.get(step as usize - 1))
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
                        if self.viewing.frame.take().is_some() {
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
            Action::Print if !self.list.files.is_empty() => self.open_print(ctx),
            Action::SaveFrame if self.frame_count() > 1 => {
                let k = self.shown_frame();
                self.pick(ctx, false, Pick::SaveFrame(self.path(self.list.cur), k));
            }
            Action::ExportFrames if self.frame_count() > 1 => {
                self.pick(ctx, true, Pick::ExportFrames(self.path(self.list.cur)));
            }
            Action::SaveFrame | Action::ExportFrames => {}
            Action::Print => {}
            Action::Rename if self.targets().len() > 1 => {
                self.ask_about_targets();
                self.prompt = Some((Prompt::RenameMany, "*_###".into()));
            }
            Action::Rename => {
                // The one file selected, as Delete and Move take it (right-clicked in the list,
                // say), not the image shown beside it.
                if let Some(&i) = self.list.selected.first().filter(|&&i| i != self.list.cur) {
                    self.go(ctx, i);
                }
                // The name only: in the list with subfolders the entry has its folders too.
                if let Some(name) = self
                    .list
                    .files
                    .get(self.list.cur)
                    .and_then(|f| Path::new(f).file_name())
                {
                    self.prompt = Some((Prompt::Rename, name.to_string_lossy().into_owned()));
                    self.ask_about_targets();
                }
            }
            // A separate process: windows share nothing but the config file.
            Action::NewWindow => self.new_window(),
            Action::WindowTitle => {
                self.prompt = Some((Prompt::WindowTitle, self.window_title.clone()));
            }
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
                if self.list.files.is_empty() {
                    return;
                }
                // Until it is saved the old picture shows: turned again, it would be saved twice,
                // into the same temporary file.
                if self.saving.contains(&self.path(self.list.cur)) {
                    self.notice = Some(tr("Still saving the last rotation of this file").into());
                    return;
                }
                let (q, m) = self.viewing.orient;
                self.viewing.turned = self.path(self.list.cur);
                self.viewing.orient = match action {
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
                // Asked for: sorted anew, also in an EXIF date order whose names stayed.
                self.listed_as = None;
                self.tree.clear();
                self.list.slots.clear();
                self.list.decodes_pending.clear();
                self.list.decode_errors.clear();
                // Decoded again now: a folder read again unchanged keeps its list as it is and
                // asks for nothing. Thumbnails and the size too, of a file edited elsewhere.
                self.refill();
                self.thumbs_changed();
                self.cur_size = None;
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
                .filter(|&i| names.contains(self.list.files[i].as_os_str()))
                .collect();
            self.list.selected = picked;
            self.list.anchor = None;
            self.selection_changed(ctx);
            return;
        }
        if self.list.files.is_empty() {
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
        #[allow(clippy::disallowed_methods)]
        // Glimt itself, in the sandbox where it runs, not on the host
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
            #[allow(clippy::disallowed_methods)] // the new window, reaped in a thread of its own
            Ok(mut child) => drop(std::thread::spawn(move || child.wait())),
            Err(e) => self.notice = Some(tf!("Could not open a new window: {e}", e)),
        }
    }

    /// Select → Copy Image: the shown image's pixels to the clipboard, decoded in the background.
    fn copy_image(&mut self, ctx: &egui::Context) {
        let (path, how) = (self.path(self.list.cur), self.shown_copy());
        let (tx, ctx) = (self.notices_tx.clone(), ctx.clone());
        let (clip_tx, clip_rx) = mpsc::channel();
        self.clip_rx = Receiving::of(clip_rx);
        let pointer = Working::new(&self.busy);
        std::thread::spawn(move || {
            // A decoder that panics on a broken file must not end the thread: the busy pointer
            // would stay for good.
            match survive(|| save::shown_image(&path, &how)) {
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
        if let Took::Answer(look) = self.desktop_rx.take() {
            self.desktop_look = look;
            self.apply_theme(ctx);
        }
        if let Took::Answer(path) = self.desktop_screen_rx.take() {
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
        self.receive_deleted(ctx);
        self.selection_size(ctx);
        self.match_size(ctx);
        let sidecars = |jobs: &[Arc<Job>]| jobs.iter().any(|j| j.sidecars);
        let were_writing = sidecars(&self.file_jobs);
        self.file_jobs.retain(|job| Arc::strong_count(job) > 1);
        // Keywords written: an `exif:` search reads what it held back for them.
        let held = !self.exif_stale.is_empty() && self.search.contains("exif:");
        if were_writing && !sidecars(&self.file_jobs) && held {
            self.update_search(ctx);
        }
        self.touching.retain(|t| Arc::strong_count(t) > 1);
        if self.work_left() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        } else if self.closing > 0 {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        while let Ok((path, result)) = self.saved_rx.try_recv() {
            self.saving.remove(&path);
            match result {
                Ok(()) => self.files_changed(&[path]),
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
        if let Took::Answer(img) = self.clip_rx.take() {
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
        if self.working() {
            ctx.set_cursor_icon(egui::CursorIcon::Progress);
        }
        if self.changed_rx.try_iter().count() > 0 && self.config.auto_refresh {
            self.relist = true;
        }
        if self.relist && self.listing_rx.is_none() {
            self.list(ctx);
        }
        match self.lightroom_rx.take() {
            Took::Nothing => {}
            Took::Gone => self.stopped(tr("Reading the Lightroom catalog")),
            Took::Answer(Ok(catalog)) => {
                let now = lightroom::roots_now(&catalog);
                self.lightroom = Some(lightroom::Import {
                    catalog,
                    now,
                    plan: None,
                });
            }
            Took::Answer(Err(e)) => self.notice = Some(e),
        }
        match self.renaming_rx.take() {
            Took::Answer(result) => {
                if let Some(job) = self.renaming.take() {
                    self.renamed(ctx, job, result);
                }
            }
            // Gone without a word: not waited for at closing, Rename Many not refused for ever.
            Took::Gone => {
                self.renaming = None;
                self.stopped(tr("Rename Many"));
            }
            Took::Nothing => {}
        }
        match self.cut_short_rx.take() {
            Took::Answer(found) => self.cut_short = found,
            Took::Gone => self.stopped(tr("Looking for renames cut short")),
            Took::Nothing => {}
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

    /// A background thread ended without its answer (it panicked on something): told, so the
    /// user knows why `what` did not come, instead of a pointer or a dialog waiting for ever.
    fn stopped(&mut self, what: &str) {
        self.notice = Some(tf!("{what} stopped unexpectedly", what));
    }

    /// A share mounted, or why not: its folder opens, or the error or a login is asked for.
    fn receive_mount(&mut self, ctx: &egui::Context) {
        let done = match self.mounting_rx.take() {
            Took::Nothing => return,
            Took::Gone => return self.stopped(tr("Connecting")),
            Took::Answer(done) => done,
        };
        {
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
        let picked = match self.file_dialog_rx.take() {
            Took::Answer(picked) => picked,
            // A dialog whose thread ended without an answer is over too: the next may open.
            Took::Gone => return self.file_dialog = None,
            Took::Nothing => return,
        };
        {
            let Some(purpose) = self.file_dialog.take() else {
                return;
            };
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
                (Pick::SaveAs(path, how, _), Some(to)) => self.save_as_to(ctx, path, how, to),
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
                (self.dir).join(save::stem_and(path, &format!("-frame-{}.png", k + 1)))
            }
            Pick::SaveAs(_, _, start) => start.clone(),
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
        let save = matches!(
            purpose,
            Pick::SavePdf(_) | Pick::SaveFrame(..) | Pick::SaveAs(..)
        );
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
            Pick::SaveAs(..) => tr("Save As"),
        };
        let notices = self.notices_tx.clone();
        std::thread::spawn(move || {
            #[allow(clippy::disallowed_methods)] // waits for the user, in its own thread
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
            #[allow(clippy::disallowed_methods)] // waits for the user, in its own thread
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
            let output = match (external::has("zenity"), external::has("kdialog")) {
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
        self.file_dialog = Some(purpose);
        self.file_dialog_rx = Receiving::of(rx);
    }

    /// The action of the key pressed this frame, if any; the arrow keys move a zoomed image first.
    fn read_keys(&mut self, ctx: &egui::Context) -> Option<Action> {
        // Choosing a part to crop: Enter saves it, Escape stops; read first, as for the pan view.
        if self.viewing.crop.is_some() && !self.dialog_open() {
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
                self.viewing.crop = None;
            } else if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)) {
                self.save_crop(ctx);
            }
        }
        // Escape closes the pan view: read first, before a text field with the focus or the
        // shortcuts could take the key, and nothing else is done with it then.
        if self.pan_window.is_some()
            && !self.dialog_open()
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            self.pan_window = None;
        }
        // And the Check Images window: a check still running is stopped with it.
        if self.check.is_some()
            && !self.dialog_open()
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
            && let Some(Some(check)) = self.check.take()
        {
            check.stop.store(true, Ordering::Relaxed);
        }
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
            let layer = egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("focus ring"));
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
            self.save_meta(ctx);
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
    /// The inhibitor runs `cat` on a pipe from Glimt, so it ends with Glimt even after a crash. In
    /// the Flatpak, which has neither, the inhibit portal, held by a connection of Glimt's.
    fn inhibit(&mut self, on: bool) {
        if on == self.inhibit_on {
            return;
        }
        self.inhibit_on = on;
        let mut held = self.inhibitor.lock().unwrap_or_else(|e| e.into_inner());
        held.0 += 1;
        let turn = held.0;
        let old = held.1.take();
        drop(held);
        if let Some(end) = old {
            end();
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
        // The first that is there, on the host in a Flatpak: asked of the shell, through
        // flatpak-spawn there, so in a thread, not each time full screen starts on the UI thread.
        let inhibitor = self.inhibitor.clone();
        std::thread::spawn(move || {
            #[cfg(feature = "portals")]
            let portal = (crate::portal::in_flatpak())
                .then(|| crate::portal::inhibit_idle("Glimt full screen"))
                .and_then(|held| held.map_err(|e| eprintln!("inhibit portal: {e}")).ok());
            #[cfg(not(feature = "portals"))]
            let portal: Option<std::sync::mpsc::Sender<()>> = None;
            let end: Ender = if let Some(held) = portal {
                Box::new(move || drop(held))
            } else {
                let found = commands.iter().find(|c| external::host_has(c[0]));
                let Some(mut child) = found.and_then(|c| {
                    external::host_command(c[0])
                        .args(&c[1..])
                        .stdin(std::process::Stdio::piped())
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .spawn()
                        .ok()
                }) else {
                    return;
                };
                Box::new(move || {
                    let _ = child.kill();
                    #[allow(clippy::disallowed_methods)]
                    // killed just now, reaped in its own thread
                    std::thread::spawn(move || child.wait());
                })
            };
            let mut held = inhibitor.lock().unwrap_or_else(|e| e.into_inner());
            // Full screen ended or started again meanwhile: this one is not wanted.
            if held.0 == turn {
                held.1 = Some(end);
            } else {
                drop(held);
                end();
            }
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
                    self.recent_menu(ui);
                    menu_item(ui, act, Action::NewFolder);
                    menu_item(ui, act, Action::Copy);
                    menu_item(ui, act, Action::Move);
                    menu_item(ui, act, Action::Rename);
                    menu_item(ui, act, Action::OpenWith);
                    self.open_with_menu(ui);
                    menu_item(ui, act, Action::SetWallpaper);
                    #[cfg(feature = "video")]
                    menu_item(ui, act, Action::Play);
                    menu_item(ui, act, Action::SaveAs);
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
                    menu_item(ui, act, Action::Undo);
                    ui.separator();
                    #[cfg(feature = "marks")]
                    scroll_menu(
                        ui,
                        iconed(ui, theme::icon::STAR, tr("Marks and Stars")),
                        |ui| {
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
                        },
                    );
                    scroll_menu(
                        ui,
                        iconed(ui, theme::icon::ORIENTATION, tr("Orientation")),
                        |ui| {
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
                        },
                    );
                    menu_item(ui, act, Action::Crop);
                    menu_item(ui, act, Action::Keywords);
                    ui.separator();
                    self.plugins_menu(ui);
                    ui.separator();
                    menu_item(ui, act, Action::Preferences);
                });
                scroll_menu(ui, tr("View"), |ui| {
                    self.layout_menu(ui, act);
                    scroll_menu(ui, iconed(ui, theme::icon::PALETTE, tr("Theme")), |ui| {
                        self.theme_menu(ui)
                    });
                    scroll_menu(
                        ui,
                        iconed(ui, theme::icon::COLOUR, tr("Colour Management")),
                        |ui| self.colour_settings(ui),
                    );
                    scroll_menu(ui, iconed(ui, theme::icon::FILTER, tr("Filter")), |ui| {
                        use theme::icon::{CIRCLE, HALF, HALF_TILT, SUN_DIM};
                        let icons = [CIRCLE, HALF, HALF_TILT, SUN_DIM];
                        for (f, label) in FILTERS.iter().enumerate() {
                            let text = iconed(ui, icons[f.min(3)], tr(label));
                            let radio = ui.radio(self.config.view_filter == f, text);
                            if noted(&radio, tr(label)).clicked() {
                                self.set_view_filter(f);
                            }
                        }
                        ui.separator();
                        menu_item(ui, act, Action::ViewFilter);
                    });
                    scroll_menu(ui, iconed(ui, theme::icon::ZOOM, tr("Zoom")), |ui| {
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
                    let (icon, label) = if self.slideshow.is_some() {
                        (theme::icon::STOP, tr("Stop Slideshow"))
                    } else {
                        (theme::icon::PLAY, tr("Start Slideshow"))
                    };
                    let text = iconed(ui, icon, label);
                    menu_button(ui, act, egui::Button::new(text), Action::Slideshow);
                    menu_toggle(ui, act, self.paused, Action::Pause);
                    menu_item(ui, act, Action::Faster);
                    menu_item(ui, act, Action::Slower);
                    menu_toggle(ui, act, self.config.random, Action::RandomOrder);
                });
                scroll_menu(ui, tr("Window"), |ui| {
                    menu_item(ui, act, Action::NewWindow);
                    menu_item(ui, act, Action::CloseWindow);
                    menu_item(ui, act, Action::WindowTitle);
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
                Prompt::WindowTitle => tr(
                    "Title of this window (%name%, %path%, %folder%, %app%; empty: as in \
                     Preferences):",
                )
                .to_owned(),
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
            let first = (i < self.list.files.len()).then(|| self.path(i));
            let mut answer = None;
            let modal = egui::Modal::new(egui::Id::unique("prompt")).show(ctx, |ui| {
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
        if let Prompt::WindowTitle = prompt {
            return self.set_window_title(ctx, text);
        }
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
                // Whole or not at all: a full disk left half a theme, whose name was then taken.
                let made = config::create_file(&f, theme::Theme::file_from(ctx));
                made.map(|_| f).map_err(|e| match e.kind() {
                    std::io::ErrorKind::AlreadyExists => {
                        tf!("{text} exists already: choose another name", text)
                    }
                    _ => e.to_string(),
                })
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
                Prompt::Rename if found[0] != self.list.cur => self.go(ctx, found[0]),
                Prompt::Rename => {}
                _ => self.list.selected = found.into_iter().collect(),
            }
        }
        if matches!(prompt, Prompt::Rename)
            && !self.list.files.is_empty()
            && !self.none_saving(&[self.path(self.list.cur)])
        {
            return;
        }
        // A renamed file stays in its own folder, also a subfolder in the list with subfolders.
        let target = match prompt {
            Prompt::Rename if !self.list.files.is_empty() => {
                self.path(self.list.cur).with_file_name(text)
            }
            _ => self.dir.join(text),
        };
        match prompt {
            // In the background, as a share that stopped answering would hold the window until
            // create_dir returns; `folder_made` reads the folder again.
            Prompt::NewFolder => {
                let deleted_tx = self.deleted_tx.clone();
                self.background(ctx, Vec::new(), move || {
                    if let Err(e) = std::fs::create_dir(&target) {
                        return Some(format!("{display}: {e}", display = target.display()));
                    }
                    let _ = deleted_tx.send(Deleted {
                        folder_made: Some(target),
                        ..Default::default()
                    });
                    None
                });
            }
            Prompt::Rename if self.list.files.is_empty() => {}
            Prompt::Rename if target == self.path(self.list.cur) => {}
            Prompt::RenameMany => self.rename_many(ctx, text),
            Prompt::NewCollection
            | Prompt::RenameCollection
            | Prompt::SaveTheme
            | Prompt::RenameFolder
            | Prompt::WindowTitle => {}
            // As Rename Many: in its thread, never over a file or another file's sidecar
            // there; `renamed` puts the list right.
            Prompt::Rename => {
                self.rename_in_background(ctx, vec![self.path(self.list.cur)], vec![target]);
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
        egui::Modal::new(egui::Id::unique("cut short")).show(ctx, |ui| {
            ui.strong(tr("Rename Many was cut short"));
            ui.label(format!(
                "A crash or a power cut stopped it in {folder}. Some files may have their new \
                 names, some their old ones, and some hidden temporary names ({} to put right).",
                count(files, "step")
            ));
            ui.horizontal(|ui| {
                if noted(
                    &ui.button(tr("Put the old names back")),
                    tr("Put the old names back"),
                )
                .clicked()
                {
                    choice = Some(true);
                }
                if noted(
                    &ui.button(tr("Finish the renaming")),
                    tr("Finish the renaming"),
                )
                .clicked()
                {
                    choice = Some(false);
                }
                if noted(&ui.button(tr("Later")), tr("Later")).clicked() {
                    choice = None;
                    self.cut_short.clear();
                }
            });
        });
        let Some(undo) = choice else { return };
        // Asked before the plans are taken: one held back is asked about again.
        let touches: Vec<PathBuf> = (self.cut_short.iter())
            .flat_map(|(_, moves, _)| moves.iter().flat_map(|(a, b)| [a.clone(), b.clone()]))
            .collect();
        if !self.none_saving(&touches) {
            return;
        }
        let plans = std::mem::take(&mut self.cut_short);
        let deleted = self.deleted_tx.clone();
        self.background(ctx, touches, move || {
            let (mut renamed, mut left) = (0, 0);
            let mut moved = Vec::new();
            for (plan, moves, done) in &plans {
                // Put right by another window: nothing done here, nothing to tell.
                let Some((r, l)) = journal::settle(plan, moves, *done, undo) else {
                    continue;
                };
                (renamed, left) = (renamed + r, left + l);
                // Finished: marks and collections follow the files, as after a Rename Many.
                if !undo && l == 0 {
                    moved.extend(journal::pairs(moves));
                }
            }
            // One report when the steps are done, which also reads the folder again: read
            // before, the list would show the names of the middle.
            let _ = deleted.send(Deleted {
                relist: true,
                moved,
                ..Default::default()
            });
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
    }

    /// The user name, domain and password for a network folder that asks for them.
    fn login_dialog(&mut self, ctx: &egui::Context) {
        if let Some(mut login) = self.login.take() {
            let mut connect = false;
            let modal = egui::Modal::new(egui::Id::unique("login")).show(ctx, |ui| {
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
            let modal = egui::Modal::new(egui::Id::unique("confirm-delete")).show(ctx, |ui| {
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
            let modal = egui::Modal::new(egui::Id::unique("confirm-folder")).show(ctx, |ui| {
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
        scroll_menu(
            ui,
            iconed(ui, theme::icon::STACK_PLUS, tr("Add to Collection")),
            |ui| {
                for (name, file) in collections::list() {
                    if noted(&ui.button(iconed(ui, theme::icon::STACK, &name)), &name).clicked() {
                        self.add_to_collection(&file);
                    }
                }
                menu_item(ui, list_act, Action::NewCollection);
            },
        );
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
        scroll_menu(ui, iconed(ui, theme::icon::PLUGIN, tr("Plugins")), |ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
            let dir = xdg_dir("XDG_DATA_HOME", ".local/share", "glimt").map(|d| d.join("plugins"));
            if self.plugins.is_empty() {
                ui.weak(tr("No plugins"));
            }
            let mut chosen = None;
            for (k, plugin) in self.plugins.iter().enumerate() {
                let button = ui.button(iconed(ui, theme::icon::PLUGIN, &plugin.name));
                if noted(&button, &plugin.name).clicked() {
                    chosen = Some(k);
                }
            }
            // A plugin that is still running, an editor left open or a script that hangs: ended
            // here, with what it started, in the background (it is given a second).
            let running = self.plugins_running.lock().unwrap().clone();
            if !running.is_empty() {
                ui.separator();
            }
            for (name, pid) in running {
                let text = tf!("Stop {name}", name);
                if noted(&ui.button(iconed(ui, theme::icon::STOP, &text)), &text).clicked() {
                    let mut running = self.plugins_running.lock().unwrap();
                    let was = running.len();
                    running.retain(|r| r.1 != pid);
                    // Not when it ended meanwhile: its number may be another's by now.
                    if running.len() < was {
                        std::thread::spawn(move || external::stop(pid));
                    }
                }
            }
            ui.separator();
            let folder = iconed(ui, theme::icon::FOLDER_OPEN, tr("Open the Plugins Folder"));
            if noted(&ui.button(folder), tr("Open the Plugins Folder"))
                .on_hover_text(tr(
                    "Desktop files there, as Geeqie's, show in this menu after a restart",
                ))
                .clicked()
                && let Some(dir) = dir
            {
                let _ = std::fs::create_dir_all(&dir);
                open_url(&dir.to_string_lossy());
            }
            let Some(k) = chosen.filter(|_| !self.list.files.is_empty()) else {
                return;
            };
            let files: Vec<PathBuf> = self.targets().iter().map(|&i| self.path(i)).collect();
            // A turned image still being saved would be written over the plugin's edit.
            if !self.none_saving(&files) {
                return;
            }
            let plugin = self.plugins[k].clone();
            let (dir, running) = (self.dir.clone(), self.plugins_running.clone());
            let deleted = self.deleted_tx.clone();
            let (tx, ctx) = (self.notices_tx.clone(), ui.ctx().clone());
            // A plugin may be an editor that stays open for hours: not waited for at closing,
            // and no busy pointer meanwhile, as for Open With.
            std::thread::spawn(move || {
                let result = plugin.run(&files, &dir, &running);
                let _ = deleted.send(Deleted {
                    relist: true,
                    changed: files,
                    ..Default::default()
                });
                if let Err(e) = result {
                    let _ = tx.send(format!("{}: {e}", plugin.name));
                }
                ctx.request_repaint();
            });
        });
    }

    /// "Open With" submenu: the applications registered for the shown file's type; the chosen
    /// one opens the selected files (or the shown one).
    fn open_with_menu(&mut self, ui: &mut egui::Ui) {
        let Some(name) = self.list.files.get(self.list.cur) else {
            return;
        };
        let ext = Path::new(name)
            .extension()
            .map_or(String::new(), |e| e.to_string_lossy().to_lowercase());
        let path = self.path(self.list.cur);
        scroll_menu(ui, iconed(ui, theme::icon::APP, tr("Open With")), |ui| {
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
            #[cfg(feature = "portals")]
            if apps.is_empty() && crate::portal::in_flatpak() {
                // The Flatpak, without the host's applications: the desktop's own chooser.
                if ui
                    .button(iconed(ui, theme::icon::APP, tr("Choose Application…")))
                    .clicked()
                {
                    // Each selected file (or the shown one), the chooser asking for each.
                    let files: Vec<PathBuf> =
                        self.targets().iter().map(|&i| self.path(i)).collect();
                    let tx = self.notices_tx.clone();
                    std::thread::spawn(move || {
                        // Cancel ends them all, not only the one asked about.
                        for path in files {
                            match crate::portal::open_with(&path) {
                                Ok(true) => {}
                                Ok(false) => break,
                                Err(e) => drop(tx.send(e)),
                            }
                        }
                    });
                }
                return;
            }
            if apps.is_empty() {
                ui.label(tr("No applications found"));
            }
            let mut chosen = None;
            for (name, desktop) in apps.iter() {
                if noted(&ui.button(iconed(ui, theme::icon::APP, name)), name).clicked() {
                    chosen = Some(desktop.clone());
                }
            }
            if let Some(desktop) = chosen {
                let files: Vec<PathBuf> = self.targets().iter().map(|&i| self.path(i)).collect();
                let tx = self.notices_tx.clone();
                std::thread::spawn(move || {
                    #[allow(clippy::disallowed_methods)]
                    // the chosen application may stay open: its own thread
                    let out = external::host_command("gio")
                        .arg("launch")
                        .arg(&desktop)
                        .args(&files)
                        .stdout(std::process::Stdio::null())
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

    /// Open the shown file in `player` (a command line), or in the default application when
    /// `player` is empty. Runs in its own thread, which waits for the program to exit.
    fn launch(&self, player: &str) {
        if self.list.files.is_empty() {
            return;
        }
        let (path, tx) = (self.path(self.list.cur), self.notices_tx.clone());
        let mut words = plugins::words(player);
        std::thread::spawn(move || {
            let run = |name: &str, mut cmd: std::process::Command| {
                // Only its errors are read: a program that prints for hours is not kept.
                #[allow(clippy::disallowed_methods)]
                // the player stays open as long as it plays: its own thread
                let out = cmd.arg(&path).stdout(std::process::Stdio::null()).output();
                out.map_err(|e| format!("{name}: {e}"))
            };
            let out = if words.is_empty() {
                // gio is part of glib; xdg-open covers systems without it. In a Flatpak both
                // ask the portal for the default application.
                match external::has("gio") {
                    true => run("gio", {
                        let mut gio = command("gio");
                        gio.arg("open");
                        gio
                    }),
                    false => run("xdg-open", command("xdg-open")),
                }
            } else {
                // The user's player, on the host in a Flatpak.
                let name = words.remove(0);
                let mut player = external::host_command(&name);
                player.args(&words);
                run(&name, player)
            };
            match out {
                Ok(o) if o.status.success() => {}
                Ok(o) => drop(tx.send(String::from_utf8_lossy(&o.stderr).trim().into())),
                Err(e) => drop(tx.send(e)),
            }
        });
    }
}

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

impl App {
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

    /// Moving the end of a Shift range changes the selection as selecting the range anew does,
    /// in the list and among search matches, also across the anchor.
    #[test]
    fn a_range_end_moved_is_the_range_anew() {
        let dir = std::env::temp_dir().join(format!("glimt-test-range-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = app_on(&dir);
        app.list.files = (0..10).map(|i| format!("{i}.png").into()).collect();
        for matches in [None, Some(vec![1, 3, 4, 6, 8, 9])] {
            app.list.matches = matches.clone();
            let shown = app.shown_files();
            let (anchor, mut end) = (shown[2], shown[2]);
            app.list.selected = [anchor].into();
            app.list.range_end = Some(end);
            for &to in [shown[4], shown[5], shown[0], shown[3], shown[5]].iter() {
                assert!(app.move_range_end(anchor, end, to));
                let moved = std::mem::take(&mut app.list.selected);
                app.select_range(anchor, to);
                assert_eq!(moved, app.list.selected, "{matches:?} to {to}");
                end = to;
                app.list.range_end = Some(end);
            }
        }
        // Ctrl-clicks since (on 9, then 2, the image shown 0): a selection of three that looks
        // like the range 2..=0 but holds 9; it is made anew, not moved.
        app.list.matches = None;
        app.list.selected = [0, 2, 9].into();
        app.selection_changed(&ctx);
        assert!(!app.move_range_end(2, 0, 1));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The window title from the template in Preferences, and the default order when it is
    /// empty or all its fields are.
    #[test]
    fn window_title_from_the_template() {
        let dir = std::env::temp_dir().join(format!("glimt-test-title-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbImage::new(4, 4).save(dir.join("a.png")).unwrap();
        let (mut app, ctx) = app_on(&dir);
        assert_eq!(app.title_text(), "a.png - Glimt");
        app.config.window_title = "%app%: %name% in %folder%".into();
        assert_eq!(
            app.title_text(),
            format!("Glimt: a.png in {}", dir.display())
        );
        app.config.window_title = "%path%".into();
        assert_eq!(app.title_text(), dir.join("a.png").display().to_string());
        app.config.window_title = "%nothing%".into();
        assert_eq!(app.title_text(), "a.png - Glimt");
        // This window's own title before the one in Preferences, until it is emptied.
        app.config.window_title = "%app%".into();
        app.set_window_title(&ctx, "Left: %name%");
        assert_eq!(app.title_text(), "Left: a.png");
        app.set_window_title(&ctx, "");
        assert_eq!(app.title_text(), "Glimt");
        // In a collection, listed as "/": the image's own folder; parts joined without spaces
        // doubled.
        let gqv = dir.join("c.gqv");
        let text = format!("#GQview collection\n\"{}\"\n", dir.join("a.png").display());
        std::fs::write(&gqv, text).unwrap();
        let (mut col, _) = app_on(&gqv);
        col.config.window_title = "%folder% | %name%".into();
        assert_eq!(col.title_text(), format!("{} · a.png", dir.display()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Work on a file holds every other operation on it back, also work started outside a job
    /// (Undo, Save As, Crop): Undo could run in the middle of a Rename Many, and a delete in the
    /// middle of an Undo. A refused Undo stays to be done again.
    #[test]
    fn work_on_a_file_holds_others_back() {
        let dir = std::env::temp_dir().join(format!("glimt-test-guard-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a.png", "b.png"] {
            image::RgbImage::new(4, 4).save(dir.join(name)).unwrap();
        }
        let a = dir.join("a.png");
        let (mut app, ctx) = app_on(&dir);
        // Work that touches none of the user's files is never held back, also while a
        // keywords job holds back everything else (a Lightroom import's collections).
        let held = app
            .start_job_of("Writing", 1, true, vec![dir.join("b.png")])
            .unwrap();
        assert!(app.background(&ctx, Vec::new(), || None));
        drop(held);
        let (go, wait) = std::sync::mpsc::channel::<()>();
        assert!(app.background(&ctx, vec![a.clone()], move || {
            let _ = wait.recv();
            None
        }));
        app.delete_now(&ctx, false, vec![0]);
        assert!(a.exists() && app.notice.is_some());
        assert_eq!(app.list.files.len(), 2);
        app.undo = Some(super::Undo::Move(vec![(a.clone(), dir.join("c.png"))]));
        app.undo(&ctx);
        assert!(app.undo.is_some());
        // Once that work is over, the delete goes ahead.
        go.send(()).unwrap();
        let wait_for = |done: &dyn Fn() -> bool| {
            (0..250).any(|_| {
                let now = done();
                if !now {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                now
            })
        };
        let free = (0..250).any(|_| {
            let now = app.none_saving(std::slice::from_ref(&a));
            if !now {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            now
        });
        assert!(free);
        app.undo = None;
        app.delete_now(&ctx, false, vec![0]);
        assert!(wait_for(&|| !a.exists()));
        // Thousands against thousands are answered at once, not path against path (10^8
        // comparisons froze the window for seconds).
        let many = |top: &str| -> Vec<std::path::PathBuf> {
            (0..50_000)
                .map(|k| dir.join(top).join(format!("{k}.jpg")))
                .collect()
        };
        let held = std::sync::Arc::new(many("busy"));
        app.touching.push(held.clone());
        let asked = many("free");
        let start = std::time::Instant::now();
        assert!(app.none_saving(&asked));
        assert!(!app.none_saving(&[dir.join("busy")]));
        assert!(!app.none_saving(&[dir.join("busy/7.jpg")]));
        assert!(start.elapsed() < std::time::Duration::from_secs(1));
        drop(held);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A folder for the contract: a.png (red) and b.png (blue), each with a sidecar saying whose
    /// it is, a folder sub, and a collection naming both images.
    fn contract_folder(name: &str) -> std::path::PathBuf {
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
    fn settle(
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

    /// The stars of `path` in the window's marks; none without marks built in.
    fn stars(app: &super::App, path: &std::path::Path) -> u8 {
        #[cfg(feature = "marks")]
        return app.marks.get(path).1;
        #[cfg(not(feature = "marks"))]
        {
            let _ = (app, path);
            0
        }
    }

    /// The sidecar's text of `path`, or "" when it has none.
    fn side(path: &std::path::Path) -> String {
        std::fs::read_to_string(crate::meta::sidecar(path)).unwrap_or_default()
    }

    /// Every file operation brings along what follows a file (its sidecar, its stars, its entry
    /// in the collection shown) and waits while other work changes the file: the classes that
    /// came back release after release, held for every operation at once. A new operation
    /// joins here; `every_file_operation_is_in_the_contract` fails until it does.
    #[test]
    fn file_operations_keep_their_contract() {
        use std::path::Path;
        type Ctx = eframe::egui::Context;
        type Op = fn(&mut super::App, &Ctx, &Path);
        type Check = fn(&super::App, &Path) -> bool;
        // Each operation, run on a.png (and b.png), and what must hold after. Held back while
        // other work changes its files, except those that change none: a copy only reads them,
        // and keywords never wait (an edit would be lost; their job holds back the rest).
        let ops: [(&str, Op, Check); 10] = [
            (
                "move",
                |app, ctx, d| app.transfer(ctx, vec![d.join("a.png")], d.join("sub"), false),
                |app, d| {
                    let to = d.join("sub/a.png");
                    to.exists()
                        && !d.join("a.png").exists()
                        && side(&to) == "a"
                        && !crate::meta::sidecar(&d.join("a.png")).exists()
                        && (!cfg!(feature = "marks")
                            || (stars(app, &to) == 3 && stars(app, &d.join("a.png")) == 0))
                },
            ),
            (
                "copy",
                |app, ctx, d| app.transfer(ctx, vec![d.join("a.png")], d.join("sub"), true),
                |app, d| {
                    let to = d.join("sub/a.png");
                    to.exists()
                        && d.join("a.png").exists()
                        && side(&to) == "a"
                        && side(&d.join("a.png")) == "a"
                        && (!cfg!(feature = "marks")
                            || (stars(app, &to) == 3 && stars(app, &d.join("a.png")) == 3))
                },
            ),
            (
                "rename",
                |app, ctx, d| {
                    app.rename_in_background(ctx, vec![d.join("a.png")], vec![d.join("c.png")])
                },
                |app, d| {
                    let to = d.join("c.png");
                    to.exists()
                        && !d.join("a.png").exists()
                        && side(&to) == "a"
                        && app.renaming.is_none()
                        && (!cfg!(feature = "marks") || stars(app, &to) == 3)
                },
            ),
            (
                "rename many",
                |app, ctx, _| {
                    app.list.selected = [0, 1].into_iter().collect();
                    app.rename_many(ctx, "x#");
                },
                |app, d| {
                    side(&d.join("x1.png")) == "a"
                        && side(&d.join("x2.png")) == "b"
                        && app.renaming.is_none()
                        && (!cfg!(feature = "marks")
                            || (stars(app, &d.join("x1.png")) == 3
                                && stars(app, &d.join("x2.png")) == 2))
                },
            ),
            (
                "delete",
                |app, ctx, _| app.delete_now(ctx, false, vec![0]),
                |app, d| {
                    !d.join("a.png").exists()
                        && !crate::meta::sidecar(&d.join("a.png")).exists()
                        && (!cfg!(feature = "marks") || stars(app, &d.join("a.png")) == 0)
                },
            ),
            (
                "save as over b",
                |app, ctx, d| {
                    let how = app.shown_copy();
                    app.save_as_to(ctx, d.join("a.png"), how, d.join("b.png"));
                },
                |app, d| {
                    side(&d.join("b.png")) == "a"
                        && (!cfg!(feature = "marks") || stars(app, &d.join("b.png")) == 0)
                },
            ),
            (
                "save frame over b",
                |app, ctx, d| app.export_frames(ctx, d.join("g.gif"), Some(0), d.join("b.png")),
                |app, d| {
                    !crate::meta::sidecar(&d.join("b.png")).exists()
                        && (!cfg!(feature = "marks") || stars(app, &d.join("b.png")) == 0)
                },
            ),
            (
                "undo a move",
                |app, ctx, d| {
                    app.transfer(ctx, vec![d.join("a.png")], d.join("sub"), false);
                    // Only when it started: held back, it says so, and there is nothing to wait for.
                    if app.notice.is_none() {
                        settle(app, ctx, |app| app.undo.is_some());
                    }
                    app.undo(ctx);
                },
                |app, d| {
                    let a = d.join("a.png");
                    a.exists()
                        && !d.join("sub/a.png").exists()
                        && side(&a) == "a"
                        && (!cfg!(feature = "marks") || stars(app, &a) == 3)
                },
            ),
            (
                "undo a rename",
                |app, ctx, d| {
                    app.rename_in_background(ctx, vec![d.join("a.png")], vec![d.join("c.png")]);
                    // Only when it started: held back, it says so, and there is nothing to wait for.
                    if app.notice.is_none() {
                        settle(app, ctx, |app| app.undo.is_some());
                    }
                    app.undo(ctx);
                },
                |app, d| {
                    let a = d.join("a.png");
                    a.exists()
                        && !d.join("c.png").exists()
                        && side(&a) == "a"
                        && app.renaming.is_none()
                        && (!cfg!(feature = "marks") || stars(app, &a) == 3)
                },
            ),
            (
                "keywords",
                |app, ctx, d| {
                    // The fixture's sidecar says only whose it is: no XMP Glimt writes over.
                    let _ = std::fs::remove_file(crate::meta::sidecar(&d.join("a.png")));
                    app.write_sidecars(
                        ctx,
                        "Saving keywords",
                        vec![d.join("a.png")],
                        |p| crate::meta::add_tags(p, &["Tag".into()]).is_ok(),
                        |_, _| None,
                    );
                },
                |_, d| side(&d.join("a.png")).contains("Tag"),
            ),
        ];
        for (name, op, check) in ops {
            let d = contract_folder(&name.replace(' ', "-"));
            let gif = d.join("g.gif");
            let mut enc = image::codecs::gif::GifEncoder::new(std::fs::File::create(&gif).unwrap());
            for c in [10u8, 200] {
                let frame = image::Frame::new(image::RgbaImage::from_pixel(
                    4,
                    4,
                    image::Rgba([c, c, c, 255]),
                ));
                enc.encode_frame(frame).unwrap();
            }
            drop(enc);
            let (mut app, ctx) = app_on(&d);
            #[cfg(feature = "marks")]
            {
                app.marks.rate(&[d.join("a.png")], 3);
                app.marks.rate(&[d.join("b.png")], 2);
            }
            // Held back while other work changes the same files: nothing moves.
            let (go, wait) = std::sync::mpsc::channel::<()>();
            let held = vec![d.join("a.png"), d.join("b.png")];
            assert!(app.background(&ctx, held, move || {
                let _ = wait.recv();
                None
            }));
            if !["copy", "keywords"].contains(&name) {
                app.notice = None;
                op(&mut app, &ctx, &d);
                std::thread::sleep(std::time::Duration::from_millis(100));
                app.poll_background(&ctx);
                let before = ["a.png", "b.png"].iter().all(|f| d.join(f).exists())
                    && side(&d.join("a.png")) == "a"
                    && side(&d.join("b.png")) == "b";
                assert!(before && app.notice.is_some(), "{name}: not held back");
            }
            go.send(()).unwrap();
            assert!(settle(&mut app, &ctx, |app| app
                .touching
                .iter()
                .all(|t| std::sync::Arc::strong_count(t) == 1)));
            // Then it runs, and what follows the file comes along.
            app.notice = None;
            op(&mut app, &ctx, &d);
            assert!(
                settle(&mut app, &ctx, |app| check(app, &d)),
                "{name}: what follows the file"
            );
            std::fs::remove_dir_all(d).unwrap();
        }
    }

    /// A refresh of a collection that started before a rename landed (auto refresh saw the
    /// folder change first) loses neither the image in the list nor its entry in the file.
    #[test]
    fn a_collection_refreshed_before_a_rename_landed_keeps_it() {
        let d = contract_folder("collection-race");
        let gqv = d.join("col.gqv");
        let (mut app, ctx) = app_on(&gqv);
        let (a, c) = (d.join("a.png"), d.join("c.png"));
        // Renamed on disk, the refresh reads, and only then the rename is reported.
        std::fs::rename(&a, &c).unwrap();
        app.list(&ctx);
        assert!(settle(&mut app, &ctx, |app| app.listing_rx.is_none()));
        app.paths_moved(vec![(a, c)], false);
        let has_c = |app: &super::App| {
            (app.list.files.iter()).any(|f| std::path::Path::new(f).ends_with("c.png"))
        };
        assert!(settle(&mut app, &ctx, |app| app.listing_rx.is_none() && has_c(app)));
        assert!(std::fs::read_to_string(&gqv).unwrap().contains("c.png"));
        std::fs::remove_dir_all(d).unwrap();
    }

    /// A folder in EXIF date order read again keeps its order, but Refresh sorts it anew: a date
    /// changed by another program under the same name takes its place.
    #[test]
    fn refresh_sorts_an_exif_date_order_anew() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-exif-order-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a.png", "b.png"] {
            image::RgbImage::new(2, 2).save(dir.join(name)).unwrap();
        }
        let (mut app, ctx) = app_on(&dir);
        app.config.sort = crate::files::Sort::ExifOriginal;
        app.list(&ctx);
        assert!(settle(&mut app, &ctx, |app| app.listing_rx.is_none()));
        assert!(app.listed_as.is_some());
        app.run(&ctx, crate::actions::Action::Refresh);
        assert!(app.listed_as.is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A random slideshow with subfolders, without Repeat, started on the last image: it does
    /// not end before its list is read, and stopped before that, the list comes back in sort
    /// order with no image moved.
    #[test]
    fn a_random_slideshow_waits_for_its_list() {
        for stop in [false, true] {
            let d = std::env::temp_dir()
                .join(format!("glimt-test-slides-{stop}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(d.join("sub")).unwrap();
            for i in 0..5 {
                image::RgbImage::new(2, 2)
                    .save(d.join(format!("{i}.png")))
                    .unwrap();
                image::RgbImage::new(2, 2)
                    .save(d.join("sub").join(format!("{i}.png")))
                    .unwrap();
            }
            let (mut app, ctx) = app_on(&d);
            let sorted = app.list.files.clone();
            app.go(&ctx, 4);
            app.config.slideshow_subfolders = true;
            app.config.random = true;
            app.config.slideshow_repeat = false;
            app.start_slideshow(&ctx);
            app.slideshow_tick(&ctx);
            assert!(app.slideshow.is_some(), "ended before its list came");
            if stop {
                app.stop_slideshow(&ctx);
            } else {
                // Random off and on again while the list is read: it still comes shuffled.
                app.set_random_order(&ctx, false);
                app.set_random_order(&ctx, true);
            }
            assert!(settle(&mut app, &ctx, |a| a.listing_rx.is_none()));
            if stop {
                assert_eq!(app.list.files, sorted);
            } else {
                // Shuffled, its round starting at the image shown (the first slide may have
                // come since).
                assert_eq!(
                    (app.list.files.len(), app.list.files[0].to_str()),
                    (10, Some("4.png"))
                );
                // Not the sort order with the shown image put first.
                let mut sorted = app.list.files.clone();
                sorted.sort();
                let at = sorted.iter().position(|f| f == "4.png").unwrap();
                sorted.swap(0, at);
                assert_ne!(app.list.files, sorted, "shuffled");
            }
            std::fs::remove_dir_all(&d).unwrap();
        }
    }

    /// A playlist back in its own order when random is turned off while it is read again: the
    /// listing copies the list, which was still shuffled.
    #[test]
    fn a_playlist_random_off_during_a_refresh_is_in_its_order() {
        let d = std::env::temp_dir().join(format!("glimt-test-m3u-off-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let mut m3u = String::new();
        for i in 0..30 {
            let f = d.join(format!("{i:02}.png"));
            image::RgbImage::new(2, 2).save(&f).unwrap();
            m3u.push_str(&format!("{}\n", f.display()));
        }
        let file = d.join("c.m3u");
        std::fs::write(&file, m3u).unwrap();
        let (mut app, ctx) = app_on(&file);
        let order = app.list.files.clone();
        app.config.random = true;
        app.start_slideshow(&ctx);
        assert!(app.shuffled && app.list.files != order);
        // A refresh on its way.
        app.list(&ctx);
        app.set_random_order(&ctx, false);
        assert!(settle(&mut app, &ctx, |a| a.listing_rx.is_none()
            && a.size_rx.is_none()));
        assert_eq!(app.list.files, order);
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// Sort by name with Case on (the default) puts a collection in name order: it kept the
    /// collection's own.
    #[test]
    fn sort_by_name_orders_a_collection() {
        let dir = std::env::temp_dir().join(format!("glimt-test-col-sort-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut gqv = String::from("#GQview collection\n");
        for name in ["b.png", "a.png"] {
            image::RgbImage::new(2, 2).save(dir.join(name)).unwrap();
            gqv.push_str(&format!("\"{}\"\n", dir.join(name).display()));
        }
        let file = dir.join("c.gqv");
        std::fs::write(&file, gqv).unwrap();
        let (mut app, ctx) = app_on(&file);
        app.config.sort = crate::files::Sort::Name;
        app.config.sort_case = true;
        app.sort_collection = true;
        app.list(&ctx);
        assert!(settle(&mut app, &ctx, |a| a.listing_rx.is_none()));
        let names: Vec<String> = (app.list.files.iter())
            .map(|f| {
                std::path::Path::new(f)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(names, ["a.png", "b.png"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A turn is not saved while other work changes the file (Rename Many, a move): it could
    /// be saved over the photo that takes its name.
    #[test]
    fn a_turn_waits_for_other_work_on_its_file() {
        let dir = std::env::temp_dir().join(format!("glimt-test-turn-held-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.png");
        image::RgbImage::new(4, 4).save(&a).unwrap();
        let (mut app, ctx) = app_on(&dir);
        app.viewing.orient = (1, false);
        app.viewing.turned = a.clone();
        let held = app
            .start_job_of("Moving", 1, false, vec![a.clone()])
            .unwrap();
        app.notice = None;
        app.save_orientation(&ctx);
        assert!(
            app.saving.is_empty() && app.notice.is_some(),
            "saved while held"
        );
        drop(held);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Refresh in a folder that has not changed decodes the image shown again (an editor may
    /// have saved it under its own name): it asked for nothing, and the busy pointer stayed.
    #[test]
    fn refresh_decodes_the_shown_image_again() {
        let dir = std::env::temp_dir().join(format!("glimt-test-refresh-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbImage::new(4, 4).save(dir.join("a.png")).unwrap();
        let (mut app, ctx) = app_on(&dir);
        let cur = app.list.cur;
        assert!(settle(&mut app, &ctx, |a| a.list.slots.contains_key(&cur)));
        app.run(&ctx, crate::actions::Action::Refresh);
        assert!(settle(&mut app, &ctx, |a| a.listing_rx.is_none()));
        let asked = |a: &super::App| {
            a.list.slots.contains_key(&cur) || a.list.decodes_pending.keys().any(|k| k.0 == cur)
        };
        assert!(asked(&app), "the image shown decoded again");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A random slideshow of a collection with its subfolders plays in random order, also when
    /// the list was shuffled (R) before: the collection is read again from its file for it.
    #[test]
    fn a_random_slideshow_of_a_collection_is_shuffled() {
        for shuffled_first in [false, true] {
            let d = std::env::temp_dir().join(format!(
                "glimt-test-shuffle-{shuffled_first}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            let mut gqv = String::from("#GQview collection\n");
            for i in 0..30 {
                let f = d.join(format!("{i:02}.png"));
                image::RgbImage::new(2, 2).save(&f).unwrap();
                gqv.push_str(&format!("\"{}\"\n", f.display()));
            }
            let file = d.join("c.gqv");
            std::fs::write(&file, gqv).unwrap();
            let (mut app, ctx) = app_on(&file);
            let order = app.list.files.clone();
            app.config.slideshow_subfolders = true;
            app.config.random = true;
            if shuffled_first {
                app.shuffle_list(&ctx, true);
            }
            let shown = app.list.files[app.list.cur].clone();
            app.start_slideshow(&ctx);
            assert!(settle(&mut app, &ctx, |a| a.listing_rx.is_none()));
            assert!(
                app.shuffled && app.list.files != order,
                "R first: {shuffled_first}"
            );
            // The round starts at the image shown (the first slide may have come since): without
            // Repeat it shows every image.
            assert_eq!(app.list.files[0], shown);
            // Again, and random off before the list comes: the saved order, and its size.
            app.stop_slideshow(&ctx);
            app.start_slideshow(&ctx);
            app.set_random_order(&ctx, false);
            assert!(settle(&mut app, &ctx, |a| a.listing_rx.is_none()
                && a.size_rx.is_none()));
            assert_eq!(app.list.files, order);
            assert!(app.total_size > 0, "size");
            std::fs::remove_dir_all(&d).unwrap();
        }
    }

    /// The same in a collection: its file follows a rename, a move, a folder renamed and a
    /// delete.
    #[test]
    fn a_collection_follows_its_files() {
        let d = contract_folder("collection");
        let gqv = d.join("col.gqv");
        let (mut app, ctx) = app_on(&gqv);
        let entries = |gqv: &std::path::Path| std::fs::read_to_string(gqv).unwrap();
        // Each step's work ended too: the next would be held back while it still runs.
        let idle =
            |app: &super::App| (app.file_jobs.iter()).all(|j| std::sync::Arc::strong_count(j) == 1);
        app.rename_in_background(&ctx, vec![d.join("a.png")], vec![d.join("c.png")]);
        assert!(settle(&mut app, &ctx, |app| idle(app)
            && entries(&gqv).contains("c.png")
            && !entries(&gqv).contains("a.png")));
        app.transfer(&ctx, vec![d.join("c.png")], d.join("sub"), false);
        assert!(settle(&mut app, &ctx, |app| idle(app)
            && entries(&gqv).contains("sub/c.png")));
        // A folder renamed, in the background: its images in the collection follow.
        app.folder_target = Some(d.join("sub"));
        app.rename_folder(&ctx, "moved");
        assert!(settle(&mut app, &ctx, |app| idle(app)
            && entries(&gqv).contains("moved/c.png")));
        assert!(d.join("moved/c.png").exists());
        let b = app
            .list
            .files
            .iter()
            .position(|f| std::path::Path::new(f).ends_with("b.png"))
            .unwrap();
        app.delete_now(&ctx, false, vec![b]);
        assert!(settle(&mut app, &ctx, |_| !entries(&gqv).contains("b.png")));
        std::fs::remove_dir_all(d).unwrap();
    }

    /// A move, a rename and a delete wait for a sidecar write of their files that holds the
    /// sidecar's lock, so the write never lands on a name the photo has left.
    #[test]
    fn file_operations_wait_for_a_sidecar_write() {
        let d = contract_folder("held");
        let (mut app, ctx) = app_on(&d);
        let idle =
            |app: &super::App| (app.file_jobs.iter()).all(|j| std::sync::Arc::strong_count(j) == 1);
        let waits =
            |app: &mut super::App, file: &std::path::Path, start: &dyn Fn(&mut super::App)| {
                let held = crate::meta::held(file);
                start(app);
                std::thread::sleep(std::time::Duration::from_millis(300));
                assert!(file.exists(), "{} gone while held", file.display());
                drop(held);
                assert!(settle(app, &ctx, |app| idle(app) && !file.exists()));
            };
        let (a, b) = (d.join("a.png"), d.join("b.png"));
        waits(&mut app, &a, &|app| {
            app.transfer(&ctx, vec![a.clone()], d.join("sub"), false)
        });
        assert!(d.join("sub/a.png").exists() && d.join("sub/a.png.xmp").exists());
        let c = d.join("c.png");
        waits(&mut app, &b, &|app| {
            app.rename_in_background(&ctx, vec![b.clone()], vec![c.clone()])
        });
        assert!(c.exists() && d.join("c.png.xmp").exists());
        assert!(settle(&mut app, &ctx, |app| app.listing_rx.is_none()));
        waits(&mut app, &c, &|app| {
            let at = (app.list.files.iter()).position(|f| f == "c.png").unwrap();
            app.delete_now(&ctx, false, vec![at]);
        });
        assert!(!d.join("c.png.xmp").exists());
        // Many files: held a chunk (100) at a time, so one held up in a sidecar write holds up
        // only its own chunk, and those before it go meanwhile. Deleted for good: a chunk of
        // 100 files of other lock slots, then two more, of which the second is held; the first
        // of them waits too, as it is in its chunk (not one gio call and one taking of the locks
        // per file).
        let slot = |n: &str| crate::meta::lock_slot(&crate::meta::sidecar(&d.join(n)));
        let names: Vec<String> = (0..400).map(|k| format!("d{k:03}.png")).collect();
        let y = &names[0];
        let first: Vec<&String> = names
            .iter()
            .filter(|n| slot(n) != slot(y))
            .take(100)
            .collect();
        assert_eq!(first.len(), 100);
        let z = (names.iter())
            .find(|n| slot(n) != slot(y) && !first.contains(n))
            .unwrap();
        for n in first.iter().copied().chain([y, z]) {
            std::fs::write(d.join(n), "").unwrap();
        }
        for n in [first[0], y] {
            std::fs::write(crate::meta::sidecar(&d.join(n)), n).unwrap();
        }
        app.list(&ctx);
        assert!(settle(&mut app, &ctx, |app| app.listing_rx.is_none()));
        let at = |app: &super::App, n: &str| (app.list.files.iter()).position(|f| f == n).unwrap();
        let mut all: Vec<usize> = first.iter().map(|n| at(&app, n)).collect();
        all.extend([at(&app, z), at(&app, y)]);
        let (x, y, z) = (d.join(first[0]), d.join(y), d.join(z));
        let held = crate::meta::held(&y);
        app.delete_now(&ctx, false, all);
        assert!(
            settle(&mut app, &ctx, |_| first
                .iter()
                .all(|n| !d.join(n).exists())),
            "the first chunk waits not for the second"
        );
        assert!(y.exists() && crate::meta::sidecar(&y).exists());
        assert!(z.exists(), "the second chunk is one");
        drop(held);
        assert!(settle(&mut app, &ctx, |app| idle(app)
            && !y.exists()
            && !z.exists()));
        assert!(!crate::meta::sidecar(&x).exists() && !crate::meta::sidecar(&y).exists());
        std::fs::remove_dir_all(d).unwrap();
    }

    /// New Folder makes the folder in the background, and the list and the tree show it.
    #[test]
    fn makes_a_new_folder_in_the_background() {
        let d = contract_folder("new-folder");
        let (mut app, ctx) = app_on(&d);
        assert!(settle(&mut app, &ctx, |a| a.listing_rx.is_none()));
        app.answer_prompt(&ctx, super::Prompt::NewFolder, "trip");
        assert!(settle(&mut app, &ctx, |a| {
            a.listing_rx.is_none() && a.subdirs.iter().any(|s| s.ends_with("trip"))
        }));
        assert!(d.join("trip").is_dir());
        // There already: said, and nothing else made.
        app.answer_prompt(&ctx, super::Prompt::NewFolder, "trip");
        assert!(settle(&mut app, &ctx, |a| a.notice.is_some()));
        std::fs::remove_dir_all(d).unwrap();
    }

    /// Every operation that starts a job or background work is in the contract above, or says
    /// here why it is not: a new one cannot leave out what follows its files unseen.
    #[test]
    fn every_file_operation_is_in_the_contract() {
        const IN_CONTRACT: &[&str] = &[
            "delete_now",
            "transfer",
            "rename_in_background",
            "undo",
            "save_as_to",
            "export_frames",
            "write_sidecars",
            "rename_folder",
        ];
        const OUTSIDE: &[(&str, &str)] = &[
            ("start_job", "the start itself"),
            (
                "save_crop",
                "a new file beside the image: save::land's tests; it needs a decoded image",
            ),
            ("trash_folder", "gio trash; Undo's restore has its own test"),
            (
                "answer_prompt",
                "New Folder: an empty folder, nothing to follow; makes_a_new_folder_in_the_background",
            ),
            ("trash_collection", "gio trash of the collection's own file"),
            ("back_up", "Glimt's own folders, no file of the user's"),
            (
                "import_lightroom",
                "needs sqlite3; a release check imports a catalog",
            ),
            (
                "cut_short_dialog",
                "journal's tests put every cut-short plan right",
            ),
        ];
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut found = std::collections::BTreeSet::new();
        for entry in std::fs::read_dir(src).unwrap() {
            let path = entry.unwrap().path();
            let text = std::fs::read_to_string(&path).unwrap();
            let code = text.split("\n#[cfg(test)]\nmod tests").next().unwrap();
            let mut function = String::new();
            for line in code.lines() {
                if let Some(at) = line.find("fn ") {
                    let rest = &line[at + 3..];
                    if let Some(end) = rest.find(|c: char| !(c.is_alphanumeric() || c == '_')) {
                        if rest[end..].starts_with('(') || rest[end..].starts_with('<') {
                            function = rest[..end].to_owned();
                        }
                    }
                }
                let starts = ["self.start_job(", "self.start_job_of(", "self.background("];
                if starts.iter().any(|s| line.contains(s)) && !line.trim_start().starts_with("//") {
                    found.insert(function.clone());
                }
            }
        }
        let known: Vec<&str> = IN_CONTRACT
            .iter()
            .copied()
            .chain(OUTSIDE.iter().map(|o| o.0))
            .collect();
        let unknown: Vec<&String> = found
            .iter()
            .filter(|f| !known.contains(&f.as_str()))
            .collect();
        assert!(
            unknown.is_empty(),
            "operations outside the contract: {unknown:?}"
        );
        let gone: Vec<&&str> = known.iter().filter(|k| !found.contains(**k)).collect();
        assert!(gone.is_empty(), "named here but no longer there: {gone:?}");
    }

    /// What follows a file comes along through the one report of what happened on disk: an
    /// `exif:` search was blind to a renamed file, as the index never read its new name; and a
    /// file written over by Save As kept its old marks and stars, though it lost its sidecar.
    #[test]
    fn what_follows_a_file_comes_along() {
        let dir = std::env::temp_dir().join(format!("glimt-test-follow-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a.png", "b.png"] {
            image::RgbImage::new(4, 4).save(dir.join(name)).unwrap();
        }
        let (mut app, ctx) = app_on(&dir);
        // Keywords written while the first index is still being read are read again after it.
        app.write_sidecars(
            &ctx,
            "Saving keywords",
            vec![dir.join("a.png")],
            |_| true,
            |_, _| None,
        );
        assert!(app.exif_stale.contains(std::ffi::OsStr::new("a.png")));
        let names = ["a.png", "b.png"].map(|n| (std::ffi::OsString::from(n), String::new()));
        app.exif_index = Some(names.into_iter().collect());
        // A search while keywords are still being written keeps the marks for after: the file
        // would be read before its sidecar landed. Once written, it is read again.
        let writing = app
            .start_job_of("Saving keywords", 1, true, vec![dir.join("a.png")])
            .unwrap();
        app.search = "exif:x".into();
        app.update_search(&ctx);
        assert!(app.exif_stale.contains(std::ffi::OsStr::new("a.png")));
        assert!(app.exif_index_rx.is_none());
        drop(writing);
        // The first keywords job above is over too, once the guard lets everything through.
        let over = (0..250).any(|_| {
            let now = app.none_saving(&[]);
            if !now {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            now
        });
        assert!(over);
        // The frame that sees the jobs end searches again, and the held marks are read.
        app.poll_background(&ctx);
        assert!(app.exif_stale.is_empty() && app.exif_index_rx.is_some());
        app.exif_index_rx = super::Receiving::none();
        app.search.clear();
        // Renamed by another program, the folder read again: the new name is read too.
        std::fs::rename(dir.join("a.png"), dir.join("c.png")).unwrap();
        app.replace_files(&ctx, vec!["b.png".into(), "c.png".into()], None);
        assert!(app.exif_stale.contains(std::ffi::OsStr::new("c.png")));
        // Written over by another image: read again, and its stars gone.
        let b = dir.join("b.png");
        #[cfg(feature = "marks")]
        app.marks.rate(std::slice::from_ref(&b), 3);
        let over = super::Deleted {
            overwritten: vec![b.clone()],
            ..Default::default()
        };
        app.deleted_tx.send(over).unwrap();
        app.receive_deleted(&ctx);
        assert!(app.exif_stale.contains(std::ffi::OsStr::new("b.png")));
        #[cfg(feature = "marks")]
        assert_eq!(app.marks.get(&b), (0, 0));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Going to another image leaves nothing of the one before: its turn, page, frame and crop
    /// are one `Viewing`, made anew, so a field added later cannot be left out of the resets
    /// (a crop once outlived its image).
    #[test]
    fn another_image_starts_afresh() {
        let dir = std::env::temp_dir().join(format!("glimt-test-viewing-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a.png", "b.png"] {
            image::RgbImage::new(4, 4).save(dir.join(name)).unwrap();
        }
        let (mut app, ctx) = app_on(&dir);
        app.config.save_orientation = false;
        app.viewing.orient = (1, true);
        app.viewing.page = 2;
        app.viewing.frame = Some(3);
        app.viewing.turned = dir.join("a.png");
        app.go(&ctx, 1);
        let v = &app.viewing;
        assert_eq!((v.orient, v.page, v.frame), ((0, false), 0, None));
        assert!(v.turned.as_os_str().is_empty() && v.crop.is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A list opened by a relative name is the collection by its absolute path, as Open Recent
    /// and the last folder keep it: a window started elsewhere opened another list or none.
    #[test]
    fn a_list_opened_by_a_relative_name_is_kept_absolute() {
        let dir = std::path::PathBuf::from(format!("glimt-test-app-list-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbImage::new(4, 4).save(dir.join("a.png")).unwrap();
        std::fs::write(dir.join("show.m3u"), "a.png\n").unwrap();
        let (app, _ctx) = app_on(&dir.join("show.m3u"));
        let here = std::env::current_dir().unwrap().join(&dir);
        assert_eq!(app.collection, Some(here.join("show.m3u")));
        assert_eq!(app.list.files.len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Two `--remote select` held back while the folder is read each get their answer: the
    /// second took the first one's place, and the first script got no answer, which it read as
    /// every file found.
    #[test]
    fn selects_held_back_each_get_their_answer() {
        let dir = std::env::temp_dir().join(format!("glimt-test-selects-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbImage::new(4, 4).save(dir.join("a.png")).unwrap();
        let (mut app, ctx) = app_on(&dir);
        let (tx, rx) = std::sync::mpsc::channel();
        app.remote_rx = rx;
        // A folder still being read.
        let (_reading, listing) = std::sync::mpsc::channel();
        app.listing_rx = super::Receiving::of(listing);
        let mut answers = Vec::new();
        for name in ["a.png", "none.png"] {
            let (reply, answer) = std::sync::mpsc::channel();
            let command = format!("select {}\0", dir.join(name).display());
            tx.send((command, Some(reply))).unwrap();
            answers.push(answer);
        }
        app.receive_remote(&ctx);
        app.listing_rx = super::Receiving::none();
        app.receive_remote(&ctx);
        let got: Vec<Vec<u8>> = answers.iter().map(|a| a.try_recv().unwrap()).collect();
        assert!(got[0].is_empty(), "a.png is there");
        assert!(String::from_utf8_lossy(&got[1]).contains("none.png"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A command after a select held back waits its turn: another script's `open` does not run
    /// first and leave the select answered for the other folder. More than 64 waiting: told to
    /// try again.
    #[test]
    fn commands_behind_a_held_select_wait_their_turn() {
        let dir = std::env::temp_dir().join(format!("glimt-test-turn-{}", std::process::id()));
        let other = dir.join("other");
        std::fs::create_dir_all(&other).unwrap();
        image::RgbImage::new(4, 4).save(dir.join("a.png")).unwrap();
        image::RgbImage::new(4, 4)
            .save(other.join("b.png"))
            .unwrap();
        let (mut app, ctx) = app_on(&dir);
        let (tx, rx) = std::sync::mpsc::channel();
        app.remote_rx = rx;
        let (_reading, listing) = std::sync::mpsc::channel();
        app.listing_rx = super::Receiving::of(listing);
        let (reply, answer) = std::sync::mpsc::channel();
        let select = format!("select {}\0", dir.join("a.png").display());
        tx.send((select.clone(), Some(reply))).unwrap();
        tx.send((format!("open {}", other.display()), None))
            .unwrap();
        // 64 waiting at most: one more is dropped.
        let mut more = Vec::new();
        for _ in 0..63 {
            let (reply, answer) = std::sync::mpsc::channel();
            tx.send((select.clone(), Some(reply))).unwrap();
            more.push(answer);
        }
        // A query is answered at once, also behind a held select.
        let (reply, current) = std::sync::mpsc::channel();
        tx.send(("current".to_owned(), Some(reply))).unwrap();
        app.receive_remote(&ctx);
        assert!(current.try_recv().is_ok(), "current answered at once");
        assert_eq!(app.dir, dir, "open waits behind the select");
        assert!(matches!(
            more[62].try_recv(),
            Err(std::sync::mpsc::TryRecvError::Disconnected)
        ));
        app.listing_rx = super::Receiving::none();
        app.receive_remote(&ctx);
        assert!(
            answer.try_recv().unwrap().is_empty(),
            "a.png found in its own folder"
        );
        assert_eq!(app.dir, other);
        // Closing with selects still held, and one sent just before: each told that none of its
        // paths was selected.
        let (reply, closing) = std::sync::mpsc::channel();
        app.waiting_select.push((select.clone(), Some(reply)));
        let (reply, late) = std::sync::mpsc::channel();
        tx.send((select.clone(), Some(reply))).unwrap();
        app.answer_held_selects();
        assert!(String::from_utf8_lossy(&closing.try_recv().unwrap()).contains("a.png"));
        assert!(String::from_utf8_lossy(&late.try_recv().unwrap()).contains("a.png"));
        std::fs::remove_dir_all(dir).unwrap();
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
        let fits = [0, 1, 2, 3].map(|m| crate::view::fit_scale(view, [400.0, 300.0], m));
        assert_eq!(fits, [1.0 / 3.0, 0.5, 1.0 / 3.0, 0.5]);
    }

    /// Images of a collection moved: each one place, never past another moved one, and to the
    /// start or the end in their own order.
    /// A photo shown larger after the window grew is decoded again at the size shown, whatever
    /// the fit mode; only one shown larger than a texture may be needs full resolution.
    #[test]
    fn decodes_again_at_the_size_shown() {
        // 6000×4000 shown at 0.5 in a texture of at most 16384: 3000×2000 fits.
        assert!(crate::view::fits_texture([6000, 4000], 0.5, 16384));
        // A panorama shown 20,000 pixels wide does not.
        assert!(!crate::view::fits_texture([40000, 2000], 0.5, 16384));
    }

    /// In full screen the pointer hides while the mouse is still, but not with the setting off,
    /// out of full screen, while work goes on (the list sorted anew after a slideshow, a file
    /// job), or with a dialog open.
    #[test]
    fn hides_the_pointer_only_when_nothing_goes_on() {
        let dir = contract_folder("pointer");
        let (mut app, ctx) = app_on(&dir);
        assert!(settle(&mut app, &ctx, |a| !a.working()));
        app.config.hide_pointer = true;
        assert!(app.pointer_hides(true));
        assert!(!app.pointer_hides(false));
        let job = super::Working::new(&app.busy);
        assert!(!app.pointer_hides(true));
        drop(job);
        app.list(&ctx);
        assert!(app.listing_rx.is_some() && !app.pointer_hides(true));
        assert!(settle(&mut app, &ctx, |a| a.pointer_hides(true)));
        app.preferences_open = true;
        assert!(!app.pointer_hides(true));
        app.preferences_open = false;
        app.config.hide_pointer = false;
        assert!(!app.pointer_hides(true));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A window, a menu or a popup on the screen keeps the pointer; the panels do not.
    #[test]
    fn a_window_on_the_screen_keeps_the_pointer() {
        let ctx = eframe::egui::Context::default();
        let pass = |window: bool| {
            let _ = ctx.run_ui(Default::default(), |ui| {
                eframe::egui::CentralPanel::default().show(ui, |ui| ui.label("image"));
                if window {
                    eframe::egui::Window::new("Help").show(ui.ctx(), |ui| ui.label("text"));
                }
            });
            super::window_shown(&ctx)
        };
        assert!(!pass(false) && !pass(false));
        assert!(pass(true));
        // The frame after it closed still counts it (shown last frame), the next not.
        let _ = pass(false);
        assert!(!pass(false));
    }

    /// In full screen the pointer hides once still for `POINTER_STILL`, and comes back when the
    /// mouse moves, outside full screen, or while a dialog is open.
    #[test]
    fn a_still_pointer_hides_after_the_timer() {
        use eframe::egui::{CursorIcon, Event, RawInput, pos2};
        let dir = std::env::temp_dir().join(format!("glimt-test-pointer-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = app_on(&dir);
        app.config.hide_pointer = true;
        let pass = |app: &mut super::App, events: Vec<Event>, fullscreen: bool| {
            let input = RawInput {
                events,
                ..Default::default()
            };
            let out = ctx.run_ui(input, |ui| app.hide_still_pointer(ui.ctx(), fullscreen));
            out.platform_output.cursor_icon == CursorIcon::None
        };
        let long_ago = || std::time::Instant::now() - super::POINTER_STILL * 2;
        // Just moved: shown, until the timer has run.
        app.pointer_moved = std::time::Instant::now();
        assert!(!pass(&mut app, vec![], true));
        app.pointer_moved = long_ago();
        assert!(pass(&mut app, vec![], true));
        // The mouse moves: shown again, and the timer starts anew.
        let moved = |x| vec![Event::PointerMoved(pos2(x, 10.0))];
        let _ = pass(&mut app, moved(10.0), true);
        assert!(!pass(&mut app, moved(20.0), true));
        assert!(app.pointer_moved.elapsed() < super::POINTER_STILL);
        // Outside full screen it never hides, and the timer starts when full screen begins.
        app.pointer_moved = long_ago();
        assert!(!pass(&mut app, vec![], false));
        assert!(!pass(&mut app, vec![], true));
        // A dialog keeps it.
        app.pointer_moved = long_ago();
        app.preferences_open = true;
        assert!(!pass(&mut app, vec![], true));
        app.preferences_open = false;
        assert!(pass(&mut app, vec![], true));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Esc in Preferences clears a search first and closes the window the next time; with no
    /// search, or one of only spaces, it closes at once.
    #[test]
    fn escape_clears_the_preferences_search_then_closes() {
        use eframe::egui::{Event, Key, RawInput};
        let dir = std::env::temp_dir().join(format!("glimt-test-esc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = app_on(&dir);
        let escape = |app: &mut super::App| {
            let input = RawInput {
                events: vec![Event::Key {
                    key: Key::Escape,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Default::default(),
                }],
                ..Default::default()
            };
            let _ = ctx.run_ui(input, |ui| app.preferences_window(ui.ctx()));
        };
        app.preferences_open = true;
        app.preferences_query = "zoom".into();
        escape(&mut app);
        assert!(app.preferences_open && app.preferences_query.is_empty());
        escape(&mut app);
        assert!(!app.preferences_open);
        app.preferences_open = true;
        app.preferences_query = "  ".into();
        escape(&mut app);
        assert!(!app.preferences_open && app.preferences_query.is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The shown image still decoded keeps the busy pointer, but not the pointer from hiding in
    /// full screen: a slideshow would flash it at each image not preloaded.
    #[test]
    fn the_shown_image_decoded_lets_the_pointer_hide() {
        let dir = std::env::temp_dir().join(format!("glimt-test-decode-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, _ctx) = app_on(&dir);
        app.config.hide_pointer = true;
        app.list.files = vec!["a.png".into()];
        assert!(app.working() && app.pointer_hides(true));
        app.busy.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        assert!(!app.pointer_hides(true));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Work in full screen keeps the pointer, and the window looks again soon, so the pointer
    /// hides when the work ends even if nothing else repaints.
    #[test]
    fn the_pointer_hides_after_work_without_a_repaint() {
        let dir = std::env::temp_dir().join(format!("glimt-test-work-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = app_on(&dir);
        app.config.hide_pointer = true;
        app.pointer_moved = std::time::Instant::now() - super::POINTER_STILL * 2;
        let pass = |app: &mut super::App| {
            let out = ctx.run_ui(Default::default(), |ui| {
                app.hide_still_pointer(ui.ctx(), true)
            });
            let delay = out.viewport_output[&eframe::egui::ViewportId::ROOT].repaint_delay;
            (
                out.platform_output.cursor_icon == eframe::egui::CursorIcon::None,
                delay,
            )
        };
        // egui repaints at once for a few passes after a change (the first, the cursor).
        app.busy.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        for _ in 0..3 {
            let _ = pass(&mut app);
        }
        let (hidden, delay) = pass(&mut app);
        assert!(
            !hidden && delay <= std::time::Duration::from_secs(1),
            "{delay:?}"
        );
        app.busy.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        let (hidden, delay) = pass(&mut app);
        assert!(
            hidden && delay > std::time::Duration::from_secs(1),
            "{delay:?}"
        );
        // The shown image decoded does not keep the pointer, so it needs no look again either.
        app.list.files = vec!["a.png".into()];
        assert!(app.working());
        let (hidden, delay) = pass(&mut app);
        assert!(
            hidden && delay > std::time::Duration::from_secs(1),
            "{delay:?}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Fitted, an image is decoded again only for a larger box than its texture was made for:
    /// the same box gives the same texture, whatever the decoders round (a JPEG of 13484×2499
    /// in Fit Height came out 6014 wide where 6021 were shown, again and again), and a texture
    /// of the whole image needs no larger one.
    #[test]
    fn decodes_again_only_for_a_larger_box() {
        assert!(!crate::view::grown(
            [16384, 1115],
            [16384, 1115],
            [6014, 1115],
            [13484, 2499]
        ));
        assert!(crate::view::grown(
            [16384, 1116],
            [16384, 1115],
            [6014, 1115],
            [13484, 2499]
        ));
        // A strip of 10000×5 fitted 1920 wide, the window then 3840 wide.
        assert!(crate::view::grown(
            [3840, 1000],
            [1920, 1000],
            [1920, 1],
            [10000, 5]
        ));
        // Smaller, or a small image whole already: not again.
        assert!(!crate::view::grown(
            [1000, 600],
            [1920, 1080],
            [960, 540],
            [4000, 2250]
        ));
        assert!(!crate::view::grown(
            [3840, 2160],
            [1920, 1080],
            [640, 480],
            [640, 480]
        ));
    }

    /// A photo fitted into a window whose height is not whole pixels is not decoded again and
    /// again: its width comes out two pixels short of the scale shown, its height within one.
    /// A grown window, 1:1, and a strip whose window grew only along it are decoded again.
    #[test]
    fn a_texture_short_on_one_side_is_the_size_shown() {
        let view = [2614.0, 1328.9];
        let fit = 1328.9 / 2657.0;
        assert!(!crate::view::too_small(
            [1998, 1328],
            [4000, 2657],
            fit,
            view
        ));
        assert!(crate::view::too_small(
            [1998, 1328],
            [4000, 2657],
            0.75,
            [4000.0, 2000.0]
        ));
        assert!(crate::view::too_small(
            [1998, 1328],
            [4000, 2657],
            1.0,
            view
        ));
        assert!(!crate::view::too_small(
            [4000, 2657],
            [4000, 2657],
            1.0,
            view
        ));
        // Fit Width: the width fills, the height is worked out from it.
        assert!(!crate::view::too_small(
            [2000, 1328],
            [4000, 2657],
            2000.6 / 4000.0,
            [2000.6, 8192.0]
        ));
        // A strip of 10000×5: fitted 1920 wide, then the window 3840 wide.
        assert!(!crate::view::too_small(
            [1920, 1],
            [10000, 5],
            0.192,
            [1920.0, 1000.0]
        ));
        assert!(crate::view::too_small(
            [1920, 1],
            [10000, 5],
            0.384,
            [3840.0, 1000.0]
        ));
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
            crate::view::decode_box([1920, 1080], [1920, 1080], fill, shown, quarter, [0, 0])
        };
        assert_eq!(b(true, false, false), [1920, 1080]);
        assert_eq!(b(true, true, false), [1080, 1920]);
        assert_eq!(b(false, true, false), [1920, 1080]);
        assert_eq!(b(false, false, true), [1920, 1920]);
        assert_eq!(b(true, true, true), [1920, 1080]);
        let with_projector =
            crate::view::decode_box([800, 600], [800, 600], false, true, true, [1920, 1080]);
        assert_eq!(with_projector, [1080, 1920]);
        let neighbour =
            crate::view::decode_box([800, 600], [800, 600], false, false, true, [1920, 1080]);
        assert_eq!(neighbour, [1920, 1080]);
    }

    /// A slideshow goes on, tick after tick, whatever the layout, order and search: the image
    /// shown is another one after every step, a round of a random order shuffles once and goes
    /// on, a search that hides the image or shows one image never holds it, and without repeat
    /// it stops at the end of the round and not before.
    #[test]
    fn slideshow_advances_on_every_tick() {
        use super::{Place, Slide, next_slide};
        use crate::selection::next_row;
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
        assert!(!crate::selection::round_ends(Some(6), 3, 10));
        assert!(crate::selection::round_ends(Some(9), 3, 10));
        assert!(crate::selection::round_ends(Some(8), 3, 10));
        assert!(crate::selection::round_ends(Some(0), 1, 1));
        assert!(crate::selection::round_ends(None, 1, 5));
    }

    /// Cells 100 high every 110: a band from 50 to 230 touches rows 0, 1 and 2; one in the gap
    /// from 101 to 109 none; one above the grid none.
    #[test]
    fn finds_the_cells_a_rectangle_touches() {
        use crate::file_list::touched;
        assert_eq!(touched(50.0, 230.0, 110.0, 100.0), 0..3);
        assert_eq!(touched(101.0, 109.0, 110.0, 100.0), 1..1);
        assert_eq!(touched(-80.0, -10.0, 110.0, 100.0), 0..0);
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
        let keep = crate::file_list::thumbs_to_keep(&files, Some(&matches), 0..2, 6);
        let mut names: Vec<_> = keep.into_iter().map(|n| n.to_str().unwrap()).collect();
        names.sort();
        assert_eq!(names, ["12", "50000", "7", "99000"]);
        assert_eq!(
            crate::file_list::thumbs_to_keep(&files, None, 500..510, 600).len(),
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

    #[test]
    fn without_indices_shifts_down() {
        let map = std::collections::HashMap::from([(1, 'a'), (2, 'b'), (4, 'c'), (6, 'd')]);
        let expected = std::collections::HashMap::from([(1, 'a'), (3, 'd')]);
        assert_eq!(crate::folder::without_indices(map, &[2, 4, 5]), expected);
    }
}
