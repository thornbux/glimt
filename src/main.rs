//! Glimt is a fast image viewer that uses little memory.
//!
//! Images are decoded in background threads to the size of the image area. Full resolution
//! is decoded only when the user zooms in further than that size covers. Only a small window
//! of images around the current one is kept in memory.

// Tests make and read their files directly. The rules in clippy.toml are for Glimt's own code,
// which `cargo clippy --release` checks without the tests.
#![cfg_attr(test, allow(clippy::disallowed_methods))]

use crate::cli::Start;
use crate::features::check::Check;
use crate::features::duplicates::Dups;
use crate::features::print::PrintDialog;
use crate::files::config::{Config, WindowState, parse_last_dir, state_path};
use crate::files::file_io::write_atomic;
use crate::images::decode::MAX_ANIMATION;
use crate::model::list::List;
use crate::support::threads::{ONE_ARENA, Receiving, cores, set_cpu_use, thread_cpu_us};
use crate::ui::actions::OWN_KEYS;
use crate::viewer::pan::Pan;
use crate::viewer::view::View;
use crate::viewer::view::frame_at;
use eframe::egui;
use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};

mod cli;
mod features;
mod files;
mod images;
mod metadata;
mod model;
mod support;
mod ui;
mod viewer;

/// glibc's M_ARENA_MAX.
const M_ARENA_MAX: i32 = -8;

/// Glimt's logo, 256 pixels square. It is the window's icon and appears on the About page.
const LOGO: &[u8] = include_bytes!("../assets/glimt-256.png");

/// The window's state. Each part is a struct in the module whose code uses it. A new part's
/// state goes there too, not in a field of its own here (`source_rules::app_stays_small`).
struct App {
    /// The window's context. It is kept for on_exit, the one caller that is not given one.
    ctx: egui::Context,
    config: Config,
    /// What the desktop says. It holds the desktop's dark style, its accent colour and its
    /// screen profile.
    desktop: crate::ui::theme::FromDesktop,

    // ---- What is open and its files ----
    /// What is open (a folder, a collection or several folders), its listing in the
    /// background, the way back and forward, and the folders sorted into.
    folder: crate::model::folder::Folder,
    /// The file list with everything indexed into it.
    list: List,
    /// File operations in the background. It holds the jobs, the files they touch, what they
    /// report, and Undo.
    work: crate::files::file_ops::Work,
    /// The count of work still running in the background, for the busy pointer. Deletes,
    /// copies and moves, printing, Check Images, Rename Many, the Lightroom import, plugins and
    /// every `background` count.
    busy: Arc<AtomicUsize>,

    // ---- The side panel ----
    /// The file list in the side panel. It knows whether it is hidden, the shape of the icon
    /// grid, and whether it scrolls to the shown image.
    panel: crate::ui::file_list::Panel,
    /// The folder tree. It holds the subfolders read so far, the folders opened in it and the
    /// one shown.
    tree: crate::ui::file_list::Tree,
    /// The thumbnails for the file list. It holds those made, those being made, and the
    /// workers that make them.
    thumbs: crate::ui::file_list::Thumbs,
    /// The search field. It holds the query, the matches, and the EXIF index that exif: terms
    /// look in.
    search: crate::model::search::Search,
    /// The status bar's numbers. It holds the shown image's size and place, the folder's total,
    /// and the size of the selection.
    status: crate::ui::file_list::StatusBar,
    /// Files Check Images found broken, marked in the list until another list is shown.
    broken_files: HashSet<OsString>,
    #[cfg(feature = "marks")]
    marks: crate::metadata::marks::Marks,

    // ---- The image and the window ----
    /// The decoding of the shown image and its neighbours. It holds the workers, the images
    /// they keep, and the size they decode to.
    decoding: crate::viewer::view::Decoding,
    /// What belongs to the image shown, made anew when it is left.
    viewing: crate::viewer::view::Viewing,
    /// The zoom, and where the zoomed image is in the view.
    zoom: crate::viewer::view::Zoom,
    /// Whether two images are shown side by side (E), the shown one and `compare_other`.
    compare: bool,
    /// What the image area showed in the last frame, for the projector to show the same.
    last_view: Option<View>,
    /// The image area. It keeps the keyboard focus for a screen reader, because a widget with
    /// the focus would take the keys from the shortcuts.
    image_id: Option<egui::Id>,
    /// Whether the focus was last moved by Tab rather than by the mouse. Its ring shows only
    /// then, as in GNOME.
    focus_by_keys: bool,
    /// The slideshow. It knows whether it runs, its order, and the pane it changes next.
    slideshow: crate::viewer::slideshow::Slideshow,
    /// The window. It holds the remembered window state, the kiosk, the projector, the screen
    /// saver, the pointer and the title.
    window: crate::ui::window::Window,
    /// The info panel and the EXIF window. They hold what was read of the shown image and
    /// what is being edited.
    info: crate::ui::info_panel::Info,
    /// The message shown over the image, until the next image.
    notice: Option<String>,
    /// Messages from background tasks (deleting, copying), to become the notice.
    notices_tx: crate::support::threads::Notices,
    notices_rx: mpsc::Receiver<String>,

    // ---- Menus, dialogs and windows ----
    /// The small dialogs. They ask for a name, confirm a delete or the trashing of a folder,
    /// and open the file chooser.
    dialogs: crate::ui::dialogs::Dialogs,
    /// The Preferences window. It knows whether it is open, its search, and the action whose
    /// key is being chosen.
    prefs: crate::ui::preferences::Prefs,
    /// Open With's programs for each file type, read in the background.
    open_with: crate::ui::menus::OpenWith,
    /// Edit → Plugins, read at start.
    plugins: Vec<crate::features::plugins::Plugin>,
    /// The plugins running, for Stop in their menu.
    plugins_running: crate::features::plugins::Running,
    /// The help page open (F1), if any.
    help: Option<crate::ui::help::Page>,
    /// File → Print's dialog, while open.
    print_window: Option<PrintDialog>,
    /// A Lightroom catalog being read, then imported with the folders where its root folders
    /// are now.
    lightroom_rx: Receiving<Result<crate::metadata::lightroom::Catalog, String>>,
    lightroom: Option<crate::metadata::lightroom::Import>,
    /// The pan view's window (Ctrl+J), while it is open.
    pan_window: Option<Pan>,
    /// File → Save As. It holds the dialog while it is open, and the answers given last.
    save_as: Option<crate::viewer::save_as::SaveAs>,
    save_as_last: crate::viewer::save_as::Choice,
    /// The Find Duplicates window, while open.
    dups: Option<Dups>,
    /// The Check Images window. `None` means it is closed, and `Some(None)` means it is open
    /// but not started.
    check: Option<Option<Check>>,
    /// Commands from --remote, and selects held back until the list is read.
    remote: crate::features::remote::Listener,
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
        crate::support::lang::set(&config.language);
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
        let screen_error = crate::images::colour::set_up(&config);
        if let Ok(mut own) = OWN_KEYS.write() {
            own.clone_from(&config.keys.0);
        }
        let path = path
            .or_else(|| {
                let state = crate::images::decode::read_text(state_path()?).ok()?;
                parse_last_dir(&state).filter(|d| {
                    config.remember_dir
                        && (d.is_dir() || crate::model::collections::is_collection(d))
                })
            })
            .unwrap_or_else(|| ".".into());
        MAX_ANIMATION.store(config.animation_mb << 20, Ordering::Relaxed);
        set_cpu_use(config.cpu_use);
        crate::ui::theme::fonts(ctx);
        crate::ui::theme::apply(ctx, &crate::ui::theme::load(&config.theme), None, None);
        // The size comes from Preferences and the desktop, so egui's own Ctrl+0 would undo it.
        ctx.options_mut(|o| o.zoom_with_keyboard = false);
        ctx.set_zoom_factor(config.ui_scale as f32 / 100.0);
        let workers = cores().min(config.decode_threads);
        let decoding = crate::viewer::view::Decoding::new(ctx, &config, workers);
        let thumbs = crate::ui::file_list::Thumbs::new(
            ctx,
            workers,
            decoding.rotation(),
            config.save_thumbnails,
        );
        // A name is only of use with remote control, so a named Glimt listens in any case. A
        // kiosk listens too, because --remote quit is its way out.
        let listen =
            config.remote_control || crate::features::remote::given_name().is_some() || start.kiosk;
        let (notices_tx, notices_rx) = crate::support::threads::Notices::new(ctx);
        let mut app = App {
            ctx: ctx.clone(),
            desktop: crate::ui::theme::FromDesktop::new(ctx, config.desktop_profile),
            folder: crate::model::folder::Folder::new(&config),
            list: List::default(),
            work: crate::files::file_ops::Work::new(),
            busy: Arc::default(),
            panel: crate::ui::file_list::Panel::default(),
            tree: crate::ui::file_list::Tree::default(),
            thumbs,
            search: crate::model::search::Search::default(),
            status: crate::ui::file_list::StatusBar::default(),
            broken_files: HashSet::new(),
            #[cfg(feature = "marks")]
            marks: crate::metadata::marks::Marks::load(),
            decoding,
            viewing: crate::viewer::view::Viewing::default(),
            zoom: crate::viewer::view::Zoom::default(),
            compare: false,
            last_view: None,
            image_id: None,
            focus_by_keys: false,
            slideshow: crate::viewer::slideshow::Slideshow::new(start.slideshow),
            window: crate::ui::window::Window::new(start.kiosk),
            info: crate::ui::info_panel::Info::new(ctx),
            notice: screen_error.or_else(|| config_notice(problems)),
            notices_tx,
            notices_rx,
            dialogs: crate::ui::dialogs::Dialogs::default(),
            prefs: crate::ui::preferences::Prefs::default(),
            open_with: crate::ui::menus::OpenWith::default(),
            plugins: crate::features::plugins::find(),
            plugins_running: crate::features::plugins::Running::default(),
            help: None,
            print_window: None,
            lightroom_rx: Receiving::none(),
            lightroom: None,
            pan_window: None,
            save_as: None,
            save_as_last: Default::default(),
            dups: None,
            check: None,
            remote: crate::features::remote::Listener::new(ctx, listen),
            config,
        };
        // When a URI is on the command line, Glimt starts where it would otherwise and goes to
        // the URI once it is mounted.
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
        // Small folders are listed before the first frame. A large one holds startup back for
        // at most 50 ms.
        app.receive_listing(ctx, std::time::Duration::from_millis(50));
        app
    }

    fn frame_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = &ui.ctx().clone();
        #[cfg(feature = "test-hooks")]
        crate::support::test_hooks::begin_frame();
        self.sync_thumb_source(ctx.pixels_per_point());
        self.poll_background(ctx);
        let mut act = self.read_keys(ctx);
        // Full screen and Hide File List work as in Geeqie, showing only the image without the
        // menu or the panels.
        let fullscreen = self.track_window(ctx);
        self.hide_still_pointer(ctx, fullscreen);
        let bare = fullscreen || self.panel.is_hidden() || self.window.is_kiosk();
        if !bare {
            self.menu_bar(ui, &mut act);
        }
        if let Some(action) = act {
            self.run(ctx, action);
        }
        // The icon grid sets the row step again in the file list, and the next frame's ↑ and ↓
        // read it.
        self.panel.start_frame();
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
        self.confirm_collection_dialog(ctx);
        self.duplicates_window(ctx);
        self.check_window(ctx);
        crate::ui::help::window(ctx, &mut self.help);
        self.focus_ring(ctx);
        if !bare {
            self.status_bar(ctx, ui);
        }
        if bare {
            // A scroll to the shown image asked for while the list is hidden is dropped.
            self.panel.drop_scroll();
        } else {
            self.side_panel(ctx, ui);
        }
        if !bare && self.config.sort_manager {
            self.sort_manager(ui);
        }
        if !bare && self.config.exif_panel && !self.list.files().is_empty() {
            self.exif_panel(ui);
        } else {
            // The panel was hidden while a field had the focus, so the field never lost it.
            self.save_meta(ui.ctx());
        }
        self.image_area(ctx, ui, fullscreen, bare);
        if self.window.has_projector() {
            self.projector_window(ctx);
        }
    }
}

// The parts of a frame, in the order `ui` draws them, and what reaches the frame from the
// threads in the background. The windows of one feature (pan view, Find Duplicates, Check
// Images, Preferences, the projector) are kept with that feature, above or in its own module.
impl eframe::App for App {
    /// The last folder is written once on exit, not on every folder change.
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // The listener closes to new clients first, because a client coming after the answers
        // would get none.
        crate::features::remote::close();
        self.answer_held_selects();
        crate::features::remote::stop(|| self.answer_held_selects());
        let ctx = self.ctx.clone();
        self.save_meta(&ctx);
        // shortcut: the path is stored as UTF-8, lossless for ordinary paths; store its bytes
        // if a folder name that is not UTF-8 must be remembered.
        let mut state = String::new();
        if self.config.remember_dir {
            let last = self.folder.collection().unwrap_or(self.folder.dir());
            state += &format!("last_dir={}\n", last.display());
        }
        if self.config.remember_window {
            state += &self.window.state_text();
        }
        // When there is nothing to keep, the old folder and window go too, so a later start
        // does not use them.
        match state_path() {
            Some(p) if state.is_empty() => drop(std::fs::remove_file(p)),
            Some(p) => write_atomic(&p, &state),
            None => {}
        }
        self.inhibit(false);
        // This waits a few seconds at most for the state above, the settings, and the keywords
        // queued at the first close. After a third close it does not wait, and Glimt ends at
        // once whatever still runs.
        if self.window.closes_asked() < 2 {
            crate::files::file_io::flush();
        }
        let at = |share| frame_at(self.decoding.frame_times(), share);
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
        if self.decoding.counts_frames() {
            let ms = thread_cpu_us().saturating_sub(start) / 1000;
            self.decoding.count_frame(ms);
        }
    }
}

/// The windows' app ID, which is the desktop file's name, so the desktop shows Glimt's icon for
/// them. It is glimt.desktop, or io.github.thornbux.Glimt.desktop (FLATPAK_ID) in a Flatpak.
fn app_id() -> String {
    std::env::var("FLATPAK_ID").unwrap_or_else(|_| "glimt".into())
}

unsafe extern "C" {
    /// Returns freed heap memory to the operating system (glibc).
    fn malloc_trim(pad: usize) -> i32;
    /// Sets a malloc parameter (glibc).
    fn mallopt(param: i32, value: i32) -> i32;
}

/// The program that New Window starts. From an AppImage it is the AppImage, because the binary
/// is in the AppImage's mount, which goes away when this window closes. Otherwise it is the
/// binary. Every program started from an AppImage inherits its APPIMAGE variable, so the
/// variable counts only when the binary is inside APPDIR.
fn own_program(exe: PathBuf, appdir: Option<OsString>, appimage: Option<OsString>) -> PathBuf {
    match (appdir, appimage) {
        (Some(dir), Some(image)) if exe.starts_with(&dir) => PathBuf::from(image),
        _ => exe,
    }
}

/// What is wrong in the settings file, as the window tells it. It gives the first problem and
/// where to see them all. A setting this Glimt does not know is left out, because a newer Glimt
/// beside it (a Flatpak and a .deb) wrote it. This Glimt keeps the setting, so the notice would
/// otherwise come at every start.
fn config_notice(problems: &[String]) -> Option<String> {
    let problems: Vec<&String> = (problems.iter())
        .filter(|p| !p.contains(": there is no setting "))
        .collect();
    let first = problems.first()?;
    let more = match problems.len() - 1 {
        0 => String::new(),
        n => tf!("; and {n} more (glimt --check-config)", n),
    };
    Some(tf!("Settings file: {first}{more}", first, more))
}

fn main() -> eframe::Result {
    // There is one malloc arena for all threads. With one arena per thread, freed image
    // buffers stayed in each decode thread's arena, and a folder of screenshots used 91 MB of
    // own memory instead of 29 MB. Decoding is at most a few percent slower.
    if Config::load().low_memory {
        // SAFETY: This is called before any other thread exists.
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
    if cli::answered(&args_os) {
        return Ok(());
    }
    let args: Vec<String> = (args_os.iter())
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let mut arg = args_os.first().map(PathBuf::from);
    let mut start = start;
    // With glimt DIR DIR …, the images of every folder are shown in one list.
    if !args.first().is_some_and(|a| a.starts_with("--remote")) {
        start.more = args_os.iter().skip(1).map(PathBuf::from).collect();
    }
    let to_all = args.first().is_some_and(|a| a == "--remote-all");
    if args.first().is_some_and(|a| a == "--remote") || to_all {
        let paths = args_os.get(2..).unwrap_or_default();
        match crate::features::remote::client(&args[1..], paths, start.name.as_deref(), to_all) {
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
    // A kiosk changes no settings either, because a visitor's changes would last until the
    // next boot.
    if start.lock_settings || start.kiosk || Path::new(crate::files::config::LOCK_FILE).exists() {
        crate::files::config::lock();
    }
    if Config::load().remember_window
        && let Some(state) = state_path().and_then(|p| crate::images::decode::read_text(p).ok())
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
        crate::features::remote::set_name(name);
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
    use crate::support::test_support::app_on;

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
        assert_eq!(app.list.files(), ["a.png", "b.png"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A setting this Glimt does not know gives no notice at start, but a wrong value does.
    #[test]
    fn tells_settings_problems_but_unknown_ones() {
        let unknown = "line 3: there is no setting projector_screen".to_string();
        let wrong = "line 5: delay=fast is not a number; 3 is used".to_string();
        assert_eq!(super::config_notice(std::slice::from_ref(&unknown)), None);
        assert_eq!(
            super::config_notice(&[unknown, wrong.clone()]),
            Some(format!("Settings file: {wrong}"))
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
}
