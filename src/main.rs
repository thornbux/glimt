//! Glimt: a fast, memory-frugal image viewer.
//!
//! Images are decoded in background threads to the size of the image area; full resolution
//! is decoded only when the user zooms past what that covers. Only a small window around
//! the current image is kept in memory.

use crate::actions::OWN_KEYS;
use crate::check::Check;
use crate::config::{Config, FADES, WindowState, parse_last_dir, state_path, write_atomic};
use crate::decode::{Hist, MAX_ANIMATION, decode};
use crate::duplicates::Dups;
use crate::file_list::RubberBand;
use crate::format::{count, thousands};
use crate::pan::Pan;
use crate::print::PrintDialog;
use crate::threads::{ONE_ARENA, Receiving, Working, cores, set_cpu_use, thread_cpu_us};
use crate::view::View;
use crate::view::{LAYOUTS, frame_at};
use eframe::egui;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Instant;
#[cfg(feature = "accessibility")]
mod accessibility;

mod actions;

mod check;

mod collections;

mod colour;

mod config;

mod crop;

mod decode;

mod decoder_plugins;

mod dialogs;

mod duplicates;

mod exif;

mod external;

mod file_list;

mod file_ops;

mod folder;

mod format;

mod help;

mod info_panel;

mod journal;

mod lang;

mod lightroom;

mod listing;

#[cfg(feature = "marks")]
mod marks;

mod menus;

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

mod xmp;

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

/// The window's state. Each part is a struct in the module whose code uses it; a new part's
/// state goes there too, not in a field of its own here (`source_rules::app_stays_small`).
struct App {
    /// The window's context, for the one caller that gets none: on_exit.
    ctx: egui::Context,
    config: Config,
    /// What the desktop says: its dark style and accent, and its screen profile.
    desktop: theme::FromDesktop,

    // ---- What is open and its files ----
    /// What is open (a folder, a collection or several folders), its listing in the
    /// background, the way back and forward, and the folders sorted into.
    folder: folder::Folder,
    /// The file list with everything indexed into it.
    list: List,
    /// File operations in the background: the jobs, the files they touch, what they report,
    /// and Undo.
    work: file_ops::Work,
    /// Work still running in the background, for the busy pointer: deletes, copies and moves,
    /// printing, Check Images, Rename Many, the Lightroom import, plugins and every `background`.
    busy: Arc<AtomicUsize>,

    // ---- The side panel ----
    /// The file list in the side panel: hidden or not, the icon grid's shape, and whether it
    /// scrolls to the shown image.
    panel: file_list::Panel,
    /// The folder tree: the subfolders read so far, the folders opened in it and the one shown.
    tree: file_list::Tree,
    /// Thumbnails for the file list: those made, those being made, and the workers making them.
    thumbs: file_list::Thumbs,
    /// The search field: its query, the matches, and the EXIF index that exif: terms look in.
    search: search::Search,
    /// The status bar's numbers: the shown image's size and place, the folder's total, and the
    /// selection's size.
    status: file_list::StatusBar,
    /// Files Check Images found broken, marked in the list until another list is shown.
    broken_files: HashSet<OsString>,
    #[cfg(feature = "marks")]
    marks: marks::Marks,

    // ---- The image and the window ----
    /// Decoding the shown image and its neighbours: the workers, which images they keep, and
    /// the size they decode to.
    decoding: view::Decoding,
    /// What belongs to the image shown, made anew when it is left.
    viewing: Viewing,
    /// The zoom, and where the zoomed image is in the view.
    zoom: view::Zoom,
    /// Two images side by side (E): the shown one and `compare_other`.
    compare: bool,
    /// What the image area showed in the last frame, for the projector to show the same.
    last_view: Option<View>,
    /// The image area, which keeps the keyboard focus for a screen reader: not a widget that
    /// takes the keys from the shortcuts.
    image_id: Option<egui::Id>,
    /// The focus last moved by Tab, not by the mouse: its ring shows, as in GNOME.
    focus_by_keys: bool,
    /// The slideshow: whether it runs, its order and the pane it changes next.
    slideshow: slideshow::Slideshow,
    /// The window: its state remembered, the kiosk, the projector, the screen saver, the
    /// pointer and the title.
    window: window::Window,
    /// The info panel and the EXIF window: what is read of the shown image, and what is being
    /// edited.
    info: info_panel::Info,
    /// The message shown over the image, until the next image.
    notice: Option<String>,
    /// Messages from background tasks (deleting, copying), to become the notice.
    notices_tx: mpsc::Sender<String>,
    notices_rx: mpsc::Receiver<String>,

    // ---- Menus, dialogs and windows ----
    /// The small dialogs: a name asked for, a delete or a folder's trashing confirmed, and the
    /// file chooser.
    dialogs: dialogs::Dialogs,
    /// The Preferences window: open or not, its search, and the action whose key is being
    /// chosen.
    prefs: preferences::Prefs,
    /// Open With's programs for each file type, read in the background.
    open_with: menus::OpenWith,
    /// Edit → Plugins, read at start.
    plugins: Vec<plugins::Plugin>,
    /// The plugins running, for Stop in their menu.
    plugins_running: plugins::Running,
    /// The help page open (F1), if any.
    help: Option<help::Page>,
    /// File → Print's dialog, while open.
    print_window: Option<PrintDialog>,
    /// A Lightroom catalog being read, then imported with the folders where its root folders
    /// are now.
    lightroom_rx: Receiving<Result<lightroom::Catalog, String>>,
    lightroom: Option<lightroom::Import>,
    /// The pan view's window (Ctrl+J), while it is open.
    pan_window: Option<Pan>,
    /// File → Save As: the dialog while open, and what it asked last.
    save_as: Option<save_as::SaveAs>,
    save_as_last: save_as::Choice,
    /// The Find Duplicates window, while open.
    dups: Option<Dups>,
    /// The Check Images window: `None` closed, `Some(None)` open and not started.
    check: Option<Option<Check>>,
    /// Commands from --remote, and selects held back until the list is read.
    remote: remote::Listener,
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
        let screen_error = colour::set_up(&config);
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
        MAX_ANIMATION.store(config.animation_mb << 20, Ordering::Relaxed);
        set_cpu_use(config.cpu_use);
        theme::fonts(ctx);
        theme::apply(ctx, &theme::load(&config.theme), None, None);
        // The size comes from Preferences and the desktop; egui's own Ctrl+0 would undo it.
        ctx.options_mut(|o| o.zoom_with_keyboard = false);
        ctx.set_zoom_factor(config.ui_scale as f32 / 100.0);
        let workers = cores().min(config.decode_threads);
        let decoding = view::Decoding::new(ctx, &config, workers);
        let thumbs = file_list::Thumbs::new(ctx, workers, &decoding.rotate, config.save_thumbnails);
        // A name is only of use with remote control, so it listens anyway; a kiosk too, as
        // --remote quit is its way out.
        let listen = config.remote_control || remote::given_name().is_some() || start.kiosk;
        let (notices_tx, notices_rx) = mpsc::channel();
        let mut app = App {
            ctx: ctx.clone(),
            desktop: theme::FromDesktop::new(ctx, config.desktop_profile),
            folder: folder::Folder::new(&config),
            list: List::default(),
            work: file_ops::Work::new(),
            busy: Arc::default(),
            panel: file_list::Panel::default(),
            tree: file_list::Tree::default(),
            thumbs,
            search: search::Search::default(),
            status: file_list::StatusBar::default(),
            broken_files: HashSet::new(),
            #[cfg(feature = "marks")]
            marks: marks::Marks::load(),
            decoding,
            viewing: Viewing::default(),
            zoom: view::Zoom::default(),
            compare: false,
            last_view: None,
            image_id: None,
            focus_by_keys: false,
            slideshow: slideshow::Slideshow {
                at_start: start.slideshow,
                ..Default::default()
            },
            window: window::Window::new(start.kiosk),
            info: info_panel::Info::new(ctx),
            notice: screen_error.or_else(|| config_notice(problems)),
            notices_tx,
            notices_rx,
            dialogs: dialogs::Dialogs::default(),
            prefs: preferences::Prefs::default(),
            open_with: menus::OpenWith::default(),
            plugins: plugins::find(),
            plugins_running: plugins::Running::default(),
            help: None,
            print_window: None,
            lightroom_rx: Receiving::none(),
            lightroom: None,
            pan_window: None,
            save_as: None,
            save_as_last: Default::default(),
            dups: None,
            check: None,
            remote: remote::Listener::new(ctx, listen),
            config,
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
        let bare = fullscreen || self.panel.hidden || self.window.kiosk;
        if !bare {
            self.menu_bar(ui, &mut act);
        }
        if let Some(action) = act {
            self.run(ctx, action);
        }
        // The icon grid sets it again in the file list; the next frame's ↑ and ↓ read it.
        self.panel.grid_cols = 1;
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
            self.panel.scroll_to_cur = false;
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
        if self.window.projector {
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
            let last = self.folder.collection.as_ref().unwrap_or(&self.folder.dir);
            state += &format!("last_dir={}\n", last.display());
        }
        if self.config.remember_window {
            state += &self.window.state.to_text();
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
        if self.window.closing < 2 {
            config::flush();
        }
        let at = |share| frame_at(&self.decoding.frame_ms, share);
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
        if self.decoding.started.is_none() {
            let ms = thread_cpu_us().saturating_sub(start) / 1000;
            self.decoding.frame_ms[(ms as usize).min(255)] += 1;
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
            std::fs::write(crate::xmp::sidecar(&png), file).unwrap();
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
