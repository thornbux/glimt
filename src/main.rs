//! Glimt is a fast image viewer that uses little memory.
//!
//! Images are decoded in background threads to the size of the image area. Full resolution
//! is decoded only when the user zooms in further than that size covers. Only a small window
//! of images around the current one is kept in memory.

// Tests make and read their files directly. The rules in clippy.toml are for Glimt's own code,
// which `cargo clippy --release` checks without the tests.
#![cfg_attr(test, allow(clippy::disallowed_methods))]

use crate::cli::Start;
use crate::features::check::ImageCheck;
use crate::features::duplicates::FindDuplicates;
use crate::features::print::PrintDialog;
use crate::files::config::{Config, WindowState, parse_last_dir, state_path};
use crate::files::file_io::write_atomic;
use crate::model::list::List;
use crate::support::threads::{HAS_ONE_MALLOC_ARENA, Receiving, cores, set_cpu_use, thread_cpu_us};
use crate::viewer::pan::PanView;
use crate::viewer::view::ImageArea;
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
    desktop_settings: crate::ui::theme::DesktopSettings,

    // ---- What is open and its files ----
    /// What is open (a folder, a collection or several folders), its listing in the
    /// background, the way back and forward, and the folders sorted into.
    folder: crate::model::folder::Folder,
    /// The file list with everything indexed into it.
    list: List,
    /// File operations in the background. It holds the jobs, the files they touch, what they
    /// report, and Undo.
    file_operations: crate::files::file_ops::FileOperations,
    /// The count of work still running in the background, for the busy pointer. Deletes,
    /// copies and moves, printing, Check Images, Rename Many, the Lightroom import, tools and
    /// every `run_in_background` count.
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
    status_bar: crate::ui::file_list::StatusBar,
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
    /// Whether two images are shown side by side (E), the shown one and `compared_image`.
    is_comparing: bool,
    /// What the image area showed in the last frame, for the projector to show the same.
    last_image_area: Option<ImageArea>,
    /// The image area. It keeps the keyboard focus for a screen reader, because a widget with
    /// the focus would take the keys from the shortcuts.
    image_area_id: Option<egui::Id>,
    /// Whether the focus was last moved by Tab rather than by the mouse. Its ring shows only
    /// then, as in GNOME.
    is_focus_from_keys: bool,
    /// The slideshow. It knows whether it runs, its order, and the pane it changes next.
    slideshow: crate::viewer::slideshow::Slideshow,
    /// The window. It holds the remembered window state, the kiosk, the projector, the screen
    /// saver, the pointer and the title.
    window: crate::ui::window::Window,
    /// The info panel and the EXIF window. They hold what was read of the shown image and
    /// what is being edited.
    info_panel: crate::ui::info_panel::InfoPanel,
    /// The message shown over the image, until the next image.
    notice: Option<String>,
    /// Messages from background tasks (deleting, copying), to become the notice.
    notice_sender: crate::support::threads::NoticeSender,
    notices: mpsc::Receiver<String>,

    // ---- Menus, dialogs and windows ----
    /// The small dialogs. They ask for a name, confirm a delete or the trashing of a folder,
    /// and open the file chooser.
    dialogs: crate::ui::dialogs::Dialogs,
    /// The Preferences window. It knows whether it is open, its search, and the action whose
    /// key is being chosen.
    preferences: crate::ui::preferences::Preferences,
    /// Open With's programs for each file type, read in the background.
    open_with: crate::ui::menus::OpenWith,
    /// Edit → Tools, read at start.
    tools: Vec<crate::features::tools::Tool>,
    /// The tools running, for Stop in their menu.
    tools_running: crate::features::tools::RunningTools,
    /// The help page open (F1), if any.
    help: Option<crate::ui::help::Page>,
    /// File → Print's dialog, while open.
    print_dialog: Option<PrintDialog>,
    /// A Lightroom catalog being read, then imported with the folders where its root folders
    /// are now.
    lightroom_catalog_answer: Receiving<Result<crate::metadata::lightroom::Catalog, String>>,
    lightroom: Option<crate::metadata::lightroom::Import>,
    /// The pan view's window (Ctrl+J), while it is open.
    pan_view: Option<PanView>,
    /// File → Save As. It holds the dialog while it is open, and the answers given last.
    save_as: Option<crate::viewer::save_as::SaveAs>,
    save_as_choices: crate::viewer::save_as::SaveAsChoices,
    /// The Find Duplicates window, while open.
    find_duplicates: Option<FindDuplicates>,
    /// The Check Images window. `None` means it is closed, and `Some(None)` means it is open
    /// but not started.
    check_images: Option<Option<ImageCheck>>,
    /// Commands from --remote, and the selects that wait until the list is read.
    remote: crate::features::remote::Listener,
}

impl App {
    // ---- Starting up ----

    fn new(cc: &eframe::CreationContext, path: Option<PathBuf>, start: Start) -> Self {
        let (config, problems) = Config::load_checked();
        Self::with_config(&cc.egui_ctx, config, &problems, path, start)
    }

    /// The window's state from the settings `new` reads, or a test gives, so a test makes an
    /// `App` on `egui::Context::default()` with settings of its own.
    fn with_config(
        ctx: &egui::Context,
        mut config: Config,
        problems: &[String],
        path: Option<PathBuf>,
        start: Start,
    ) -> Self {
        crate::support::lang::set_language(&config.language);
        // Start options are kept, as choosing them in the View menu would.
        if let Some(layout) = start.layout {
            config.layout = layout.into();
        }
        config.slideshow_random |= start.random;
        if let Some(fade) = start.fade {
            config.fade = fade;
        }
        if start.layout.is_some() || start.random || start.fade.is_some() {
            config.save();
        }
        let screen_error = crate::images::colour::set_up_colour(&config);
        crate::ui::actions::use_user_keys(&config.keys);
        let path = path
            .or_else(|| {
                let state = crate::images::decode::read_text(state_path()?).ok()?;
                parse_last_dir(&state).filter(|d| {
                    config.remember_dir
                        && (d.is_dir() || crate::model::collections::is_collection(d))
                })
            })
            .unwrap_or_else(|| ".".into());
        crate::images::decode::set_animation_limit(config.animation_mb);
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
            decoding.auto_rotate(),
            config.save_thumbnails,
        );
        // A name is only of use with remote control, so a named Glimt listens in any case. A
        // kiosk listens too, because --remote quit is its way out.
        let listen =
            config.remote_control || crate::features::remote::given_name().is_some() || start.kiosk;
        let (notice_sender, notices) = crate::support::threads::NoticeSender::new(ctx);
        let mut app = App {
            ctx: ctx.clone(),
            desktop_settings: crate::ui::theme::DesktopSettings::new(ctx, config.desktop_profile),
            folder: crate::model::folder::Folder::new(&config),
            list: List::default(),
            file_operations: crate::files::file_ops::FileOperations::new(),
            busy: Arc::default(),
            panel: crate::ui::file_list::Panel::default(),
            tree: crate::ui::file_list::Tree::default(),
            thumbs,
            search: crate::model::search::Search::default(),
            status_bar: crate::ui::file_list::StatusBar::default(),
            broken_files: HashSet::new(),
            #[cfg(feature = "marks")]
            marks: crate::metadata::marks::Marks::load(),
            decoding,
            viewing: crate::viewer::view::Viewing::default(),
            zoom: crate::viewer::view::Zoom::default(),
            is_comparing: false,
            last_image_area: None,
            image_area_id: None,
            is_focus_from_keys: false,
            slideshow: crate::viewer::slideshow::Slideshow::new(start.slideshow),
            window: crate::ui::window::Window::new(start.kiosk),
            info_panel: crate::ui::info_panel::InfoPanel::new(ctx),
            notice: screen_error.or_else(|| config_notice(problems)),
            notice_sender,
            notices,
            dialogs: crate::ui::dialogs::Dialogs::default(),
            preferences: crate::ui::preferences::Preferences::default(),
            open_with: crate::ui::menus::OpenWith::default(),
            tools: crate::features::tools::find(),
            tools_running: crate::features::tools::RunningTools::default(),
            help: None,
            print_dialog: None,
            lightroom_catalog_answer: Receiving::none(),
            lightroom: None,
            pan_view: None,
            save_as: None,
            save_as_choices: Default::default(),
            find_duplicates: None,
            check_images: None,
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
            None if !start.more_paths.is_empty() => {
                let all = std::iter::once(path).chain(start.more_paths.iter().cloned());
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
        let mut chosen = self.read_keys(ctx);
        // Full screen and Hide File List work as in Geeqie, showing only the image without the
        // menu or the panels.
        let fullscreen = self.track_window(ctx);
        self.hide_still_pointer(ctx, fullscreen);
        let bare = fullscreen || self.panel.is_hidden() || self.window.is_kiosk();
        if !bare {
            self.menu_bar(ui, &mut chosen);
        }
        if let Some(action) = chosen {
            self.run(ctx, action);
        }
        // The icon grid sets the row step again in the file list, and the next frame's ↑ and ↓
        // read it. The scroll to the shown image waits while no list is drawn.
        self.panel
            .start_frame(!bare || self.panel.is_area_grid_shown());
        // A drag's drop target is set again by the row under the pointer, in whichever list,
        // so a drag let go elsewhere ends where it is.
        if let Some(to) = self.list.drag_target_mut() {
            *to = None;
        }
        self.preferences_window(ctx);
        self.prompt_dialog(ctx);
        self.print_dialog(ctx);
        self.lightroom_dialog(ctx);
        self.save_as_dialog(ctx);
        self.sharing_dialog(ctx);
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
        if !bare {
            self.side_panel(ctx, ui);
        }
        if !bare && self.config.sort_manager {
            self.sort_manager(ui);
        }
        if !bare && self.config.info_panel && !self.list.files().is_empty() {
            self.info_panel(ui);
        } else {
            // The panel was hidden while a field had the focus, so the field never lost it.
            self.save_metadata_edit(ui.ctx());
        }
        if self.panel.is_area_grid_shown() {
            self.area_grid(ctx, ui);
        } else {
            self.image_area(ctx, ui, fullscreen, bare);
        }
        if self.window.has_projector() {
            self.projector_window(ctx);
        }
        self.note_focus(ctx);
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
        self.answer_waiting_selects();
        crate::features::remote::stop(|| self.answer_waiting_selects());
        let ctx = self.ctx.clone();
        self.save_metadata_edit(&ctx);
        // shortcut: the path is stored as UTF-8, lossless for ordinary paths; store its bytes
        // if a folder name that is not UTF-8 must be remembered.
        let mut state = String::new();
        if self.config.remember_dir {
            let last = self.folder.collection().unwrap_or(self.folder.dir());
            state += &crate::files::config::last_dir_line(last);
        }
        if self.config.remember_window {
            state += &self.window.state_text();
        }
        // When there is nothing to keep, the old folder and window go too, so a later start
        // does not use them.
        match state_path() {
            #[allow(clippy::disallowed_methods)] // Glimt's own state file
            Some(p) if state.is_empty() => drop(std::fs::remove_file(p)),
            Some(p) => write_atomic(&p, &state),
            None => {}
        }
        self.inhibit(false);
        // This waits a few seconds at most for the state above, the settings, and the keywords
        // queued at the first close. After a third close it does not wait, and Glimt ends at
        // once whatever still runs.
        if self.window.close_requests() < 2 {
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
fn new_window_program(
    exe: PathBuf,
    appdir: Option<OsString>,
    appimage: Option<OsString>,
) -> PathBuf {
    match (appdir, appimage) {
        (Some(dir), Some(image)) if exe.starts_with(&dir) => PathBuf::from(image),
        _ => exe,
    }
}

/// What is wrong in the settings file, as the window tells it. It gives the first problem and
/// where to see them all. Two problems are left out, because a Glimt of another version beside
/// this one (a Flatpak and a .deb) writes them at each save, so the notice would otherwise come
/// at every start. A setting this Glimt does not know was written by a newer Glimt. An old key
/// beside its new key was written by an older Glimt, which writes every key it knows.
/// `glimt --check-config` still tells both.
fn config_notice(problems: &[String]) -> Option<String> {
    let problems: Vec<&String> = (problems.iter())
        .filter(|p| !p.contains(": there is no setting ") && !p.contains(" is the old name of "))
        .collect();
    let first = problems.first()?;
    let more = match problems.len() - 1 {
        0 => String::new(),
        n => tf!("; and {n} more (glimt --check-config)", n),
    };
    Some(tf!("Settings file: {first}{more}", first, more))
}

fn main() -> eframe::Result {
    // A Glimt started by another to draw an SVG draws it and ends, before anything else.
    #[cfg(feature = "svg")]
    if let Some(path) = std::env::var_os(crate::images::svg::DRAW_SVG) {
        std::process::exit(crate::images::svg::draw_for_parent(&path));
    }
    // There is one malloc arena for all threads. With one arena per thread, freed image
    // buffers stayed in each decode thread's arena, and a folder of screenshots used 91 MB of
    // own memory instead of 29 MB. Decoding is at most a few percent slower.
    if Config::load().low_memory {
        // SAFETY: This is called before any other thread exists.
        unsafe { mallopt(M_ARENA_MAX, 1) };
        HAS_ONE_MALLOC_ARENA.store(true, Ordering::Relaxed);
    }
    let (start, args_os) = match Start::parse(std::env::args_os().skip(1).collect()) {
        Ok(parsed) => parsed,
        Err(e) => {
            eprintln!("glimt: {e}\nTry glimt --help");
            std::process::exit(2);
        }
    };
    if cli::answer_without_window(&start, &args_os) {
        return Ok(());
    }
    let args: Vec<String> = (args_os.iter())
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let mut arg = args_os.first().map(PathBuf::from);
    let mut start = start;
    // After `--` the first argument is a file, also when it is named --remote.
    let command = args.first().filter(|_| !start.files_only);
    // With glimt DIR DIR …, the images of every folder are shown in one list.
    let remote = [
        crate::features::remote::REMOTE,
        crate::features::remote::REMOTE_ALL,
    ];
    if !command.is_some_and(|a| remote.contains(&a.as_str())) {
        start.more_paths = args_os.iter().skip(1).map(PathBuf::from).collect();
    }
    let to_all = command.is_some_and(|a| a == remote[1]);
    if command.is_some_and(|a| a == remote[0]) || to_all {
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
    if let Ok(icon) = crate::images::decode::decode_glimts_own_image(LOGO) {
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
        if let Some(pos) = win.position {
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

    /// A setting this Glimt does not know, and an old key beside its new key, give no notice at
    /// start, but a wrong value does.
    #[test]
    fn tells_settings_problems_but_unknown_ones() {
        let unknown = "line 3: there is no setting projector_screen".to_string();
        let wrong = "line 5: slideshow_delay=fast is not a number; 3 is used".to_string();
        assert_eq!(super::config_notice(std::slice::from_ref(&unknown)), None);
        assert_eq!(
            super::config_notice(&[unknown, wrong.clone()]),
            Some(format!("Settings file: {wrong}"))
        );
        // The problems as parse_checked words them for a file that a 0.73 Glimt saved last.
        let (_, problems) =
            crate::files::config::Config::parse_checked("slideshow_delay=4\ndelay=5\n");
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert_eq!(super::config_notice(&problems), None);
    }

    /// New Window starts the AppImage only from inside it, not when Glimt was merely started
    /// from another AppImage (a terminal), whose APPIMAGE it inherits.
    #[test]
    fn new_window_program() {
        use std::path::PathBuf;
        let os = |s: &str| Some(std::ffi::OsString::from(s));
        let inside = PathBuf::from("/tmp/.mount_ab12/usr/bin/glimt");
        let got =
            super::new_window_program(inside, os("/tmp/.mount_ab12"), os("/home/u/glimt.AppImage"));
        assert_eq!(got, PathBuf::from("/home/u/glimt.AppImage"));
        let installed = PathBuf::from("/usr/bin/glimt");
        let got = super::new_window_program(
            installed.clone(),
            os("/tmp/.mount_cd34"),
            os("/home/u/Term.AppImage"),
        );
        assert_eq!(got, installed);
        assert_eq!(
            super::new_window_program(installed.clone(), None, None),
            installed
        );
    }
}
