//! Glimt's own files. These are the settings (the config! macro, with one line per setting and
//! its default and range), the window state, the XDG paths they live in, and `--lock-settings`.
//! How the files are written is in file_io.rs, and the backup of Glimt's folders is in
//! backup.rs.

use crate::features::print;
use crate::files::file_io::{edit_shared, save_failed, write_later};
use crate::model::listing::Sort;
use crate::ui::actions::Keys;
use eframe::egui;
use std::path::{Path, PathBuf};

/// Makes the config struct, its defaults, and the code that reads and writes it, from one list.
/// The field name is the key in the file.
macro_rules! config {
    ($($(#[$doc:meta])* $name:ident: $ty:ty = $default:expr,)*) => {
        /// Preferences, stored as `key=value` lines. Unknown or invalid lines are ignored.
        #[derive(Debug, PartialEq)]
        pub struct Config {
            $($(#[$doc])* pub $name: $ty,)*
        }

        impl Default for Config {
            fn default() -> Self {
                Config { $($name: $default,)* }
            }
        }

        impl Config {
            /// Sets `key` to `value`. Returns `None` for a key that is no setting, and false for
            /// a value that cannot be read, which keeps the value from before.
            // String's parse cannot fail, so its `if let` is irrefutable.
            #[allow(irrefutable_let_patterns)]
            pub fn set(&mut self, key: &str, value: &str) -> Option<bool> {
                let value = by_number(key, value);
                let value = value.as_str();
                match key {
                    $(stringify!($name) => {
                        if let Ok(v) = value.parse() {
                            self.$name = v;
                            return Some(true);
                        }
                        Some(false)
                    })*
                    _ => None,
                }
            }

            pub fn to_text(&self) -> String {
                let mut text = String::new();
                $(text += &format!(
                    "{}={}\n",
                    stringify!($name),
                    by_name(stringify!($name), self.$name.to_string())
                );)*
                text
            }
        }
    };
}

// ---- Defaults and ranges of settings ----

/// Default number of images kept decoded on each side of the shown one.
pub(crate) const DEFAULT_PRELOAD: usize = 2;

/// Default seconds between images in a slideshow.
pub(crate) const DEFAULT_SLIDESHOW_DELAY: u64 = 3;

pub(crate) const MAX_SLIDESHOW_DELAY: u64 = 60;

/// The full screen info, as Geeqie writes its overlay template. Fields are in percent signs,
/// `|` stands between the parts of a line, and `\n` starts a new line. A part whose fields are
/// all empty is left out. The default shows what Glimt always showed.
pub(crate) const FULL_SCREEN_INFO_TEMPLATE: &str = "%name%  %position%\\n%dimensions%|%File size%|%Taken%|%Exposure%|%Aperture%|ISO %ISO%|\
     %Focal length%|%marks%";

/// Preferences → CPU use, as (name, hover text). Power saving makes thumbnails one at a time,
/// and runs searches, the EXIF index, Find Duplicates and pan view on two cores. Balanced makes
/// two thumbnails at once and runs the others on every core. Full makes thumbnails on every
/// core.
pub(crate) const CPU_LEVELS: [(&str, &str); 3] = [
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

/// Slideshow → Fade, as (the name for the config and --fade, the menu label, milliseconds).
pub(crate) const FADES: [(&str, &str, u64); 3] = [
    ("off", "Off", 0),
    ("short", "Short", 400),
    ("long", "Long", 1000),
];

pub(crate) const MEMORY_LEVELS: [MemoryLevel; 3] = [
    MemoryLevel {
        name: "Low",
        preload: 1,
        decode_threads: 1,
        animation_mb: 32,
        one_arena: true,
        tooltip: "Least memory: one image decoded at a time, one preloaded on each side. Slow \
                storage (SMB, NFS, USB) makes browsing wait for each file.",
    },
    MemoryLevel {
        name: "Medium",
        preload: DEFAULT_PRELOAD,
        decode_threads: 4,
        animation_mb: 128,
        one_arena: true,
        tooltip: "The default: four images decoded at once, two preloaded on each side.",
    },
    MemoryLevel {
        name: "High",
        preload: 5,
        decode_threads: 8,
        animation_mb: 256,
        one_arena: false,
        tooltip: "Fastest browsing: eight images decoded at once, five preloaded on each side; \
                hides the wait of slow storage (SMB, NFS) best.",
    },
];

/// How many Open Recent keeps.
pub const MAX_RECENT: usize = 10;

/// The settings that take one of a few values. Since 1.0 they are written by name
/// (fit_mode=width), and they are read by name or by the number used before (fit_mode=1). A
/// list grows only at its end, so the numbers keep their meaning.
const NAMED: [(&str, &[&str]); 7] = [
    ("fade", &["off", "short", "long"]),
    ("fit_mode", &["whole", "width", "height", "fill"]),
    ("histogram", &["off", "rgb", "brightness"]),
    ("thumb_size", &["small", "medium", "large"]),
    ("toolbar_icons", &["small", "medium", "large"]),
    (
        "view_filter",
        &["none", "greyscale", "inverted", "contrast"],
    ),
    ("cpu_use", &["saving", "balanced", "full"]),
];

/// The settings renamed in 0.74.0 and later, as (old key, new key). A settings file is read by
/// the old keys too, forever, so the users' files keep working, and only the new keys are
/// written. When a file holds both keys of a setting, the value of the new key is the one used.
const RENAMED_SETTINGS: [(&str, &str); 11] = [
    ("trash", "delete_to_trash"),
    ("delay", "slideshow_delay"),
    ("random", "slideshow_random"),
    ("own_profile", "use_image_profile"),
    ("videos", "list_videos"),
    ("sort_case", "sort_case_sensitive"),
    ("search_case", "search_case_sensitive"),
    ("exif_panel", "info_panel"),
    // 0.76.0: "OSD" is jargon that the menus and the action had already dropped.
    ("osd", "full_screen_info"),
    ("osd_template", "full_screen_info_template"),
    // 0.76.0: "turns" was a word of its own. The menu says One Image at a Time.
    ("layout_turns", "slideshow_one_image_at_a_time"),
];

/// The window state's keys renamed in 0.74.0, as (old key, new key). They are read as the
/// keys of `RENAMED_SETTINGS` are.
const RENAMED_STATE: [(&str, &str); 1] = [("window_pos", "window_position")];

/// The window title that the retired app_name_first=true stood for.
const APP_NAME_FIRST_TITLE: &str = "%app% - %name%";

/// The key of the last folder in the window state file, beside `WindowState`'s keys.
pub const LAST_DIR: &str = "last_dir";

/// The administrator's lock. While this file is there, every Glimt starts with --lock-settings.
pub const LOCK_FILE: &str = "/etc/glimt/lock-settings";

/// Whether --lock-settings is on. Then there are no Preferences, and nothing is written to the
/// settings files.
static SETTINGS_LOCKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The settings as this window last read or wrote them.
static LAST_SAVED_TEXT: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

/// A memory use level in Preferences. The config holds the values, and the level shown is the
/// one they match, or Custom.
#[derive(Clone, Copy)]
pub(crate) struct MemoryLevel {
    pub(crate) name: &'static str,
    /// Images preloaded on each side.
    pub(crate) preload: usize,
    pub(crate) decode_threads: usize,
    /// The most an animation's frames may take, in MB.
    pub(crate) animation_mb: usize,
    /// One malloc arena for all threads.
    pub(crate) one_arena: bool,
    pub(crate) tooltip: &'static str,
}

/// The window geometry saved at exit, in points. It holds the size, the position and whether
/// the window is maximized. The position is not kept on Wayland, where apps cannot see or set
/// it.
#[derive(Debug, Default, PartialEq)]
pub struct WindowState {
    pub size: Option<egui::Vec2>,
    pub position: Option<egui::Pos2>,
    pub maximized: bool,
}

impl WindowState {
    pub fn parse(state: &str) -> Self {
        let pair = |key| {
            let (a, b) = state_value(state, key)?.split_once(',')?;
            Some((a.parse::<f32>().ok()?, b.parse::<f32>().ok()?))
        };
        WindowState {
            size: pair("window_size")
                .filter(|&(w, h)| w >= 200.0 && h >= 150.0)
                .map(|(w, h)| egui::vec2(w, h)),
            position: pair("window_position").map(|(x, y)| egui::pos2(x, y)),
            maximized: state_value(state, "maximized") == Some("true"),
        }
    }

    pub fn to_text(&self) -> String {
        let mut text = format!("maximized={}\n", self.maximized);
        if let Some(s) = self.size {
            text += &format!("window_size={},{}\n", s.x, s.y);
        }
        if let Some(p) = self.position {
            text += &format!("window_position={},{}\n", p.x, p.y);
        }
        text
    }
}

/// A colour (sRGB), written as #rrggbb in the config.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgb(pub [u8; 3]);

impl std::str::FromStr for Rgb {
    type Err = ();
    fn from_str(text: &str) -> Result<Self, ()> {
        let hex = text.trim_start_matches('#');
        let [_, r, g, b] = u32::from_str_radix(hex, 16).map_err(drop)?.to_be_bytes();
        (hex.len() == 6).then_some(Rgb([r, g, b])).ok_or(())
    }
}

impl std::fmt::Display for Rgb {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let [r, g, b] = self.0;
        write!(f, "#{r:02x}{g:02x}{b:02x}")
    }
}

config! {
    preload: usize = DEFAULT_PRELOAD,
    /// Seconds between images in a slideshow.
    slideshow_delay: u64 = DEFAULT_SLIDESHOW_DELAY,
    /// Start in the last used folder when no folder is given on the command line.
    remember_dir: bool = false,
    /// Keep File → Open Recent's list. When this is off, no list is written and the one kept
    /// is deleted.
    keep_recent: bool = true,
    /// Deleting moves files to the trash (the default). Otherwise a file is deleted for good.
    delete_to_trash: bool = true,
    /// Ask before deleting permanently.
    confirm_delete: bool = true,
    /// Ask before moving to the trash.
    confirm_trash: bool = true,
    /// The Delete key deletes (Ctrl+D always does).
    delete_key: bool = true,
    /// Slideshow in random order.
    slideshow_random: bool = false,
    /// Zoom to fit also enlarges images smaller than the window (Geeqie: "Allow enlargement").
    enlarge: bool = true,
    /// A slideshow starts again from the first image after the last. When this is off, it stops
    /// there.
    slideshow_repeat: bool = true,
    /// The slideshow fades from one image to the next. It is off, short or long, as an index
    /// into FADES.
    fade: usize = 1,
    /// How many images are shown at once (View → Layout): "1", "2x1", "1x2" or "2x2" (columns ×
    /// rows).
    layout: String = String::from("1"),
    /// The layouts whose slideshow changes one image at a time, round the panes, separated by
    /// commas. The other layouts change all their images together.
    slideshow_one_image_at_a_time: String = String::new(),
    /// Layouts with several panes show each image's file name in its pane.
    layout_names: bool = false,
    /// Animated GIF and WebP files play (A). When this is off, the first frame is shown.
    animate: bool = true,
    /// Embedded ICC profiles are converted to sRGB, or to the screen profile.
    color_management: bool = true,
    /// The screen's ICC profile file, chosen in Preferences. When it is empty, the screen is
    /// taken to be sRGB.
    screen_profile: String = String::new(),
    /// The desktop's screen profile, from colord, goes before `screen_profile` when there is one
    /// (Geeqie's "Use system screen profile if available").
    desktop_profile: bool = true,
    /// The profile of images that have none: srgb, adobe, p3 or an ICC file.
    input_profile: String = "srgb".to_owned(),
    /// An image's own ICC profile (or Adobe RGB from its EXIF) counts. When this is off, the
    /// input profile is used for every image.
    use_image_profile: bool = true,
    /// How colours outside the target are brought in: perceptual, relative, saturation or
    /// absolute. Only profiles with tables for it (LUT profiles) tell them apart.
    rendering_intent: String = "perceptual".to_owned(),
    /// The interface's language, as a translation's code (no_nb) or "en". When it is empty,
    /// the system's language is used.
    language: String = String::new(),
    /// Marks and stars shown in the file list and overlay (marks feature).
    show_marks: bool = true,
    /// The user's own keys, replacing the defaults of those actions (Preferences).
    keys: Keys = Keys::default(),
    /// Listen for `glimt --remote` commands.
    remote_control: bool = true,
    /// Stars are also written as xmp:Rating to `<file>.xmp` sidecars, for darktable and digiKam.
    xmp_stars: bool = false,
    /// The file list holds the images of all subfolders too, with their folders in the name.
    /// It is kept across starts like the other View toggles, and the top folders show while
    /// the tree is read.
    show_subfolders: bool = false,
    /// A slideshow also shows the images in the subfolders.
    slideshow_subfolders: bool = false,
    /// Write the view's rotation into the JPEG when going to another image.
    save_orientation: bool = false,
    /// Quality (0–100) when a lossy WebP is saved again after turning it.
    webp_quality: u8 = 90,
    /// Quality (0–100) of a cropped JPEG.
    jpeg_quality: u8 = 92,
    /// Keep zoom and position when going to another image, instead of fitting it.
    keep_zoom: bool = false,
    /// How an image that is not zoomed fills the view: whole, width, height or fill. Fill
    /// covers the whole view and cuts the image at the sides. It is an index into FITS.
    fit_mode: u8 = 0,
    /// The mouse wheel zooms. When this is off, it goes to the next or previous image, as in
    /// Geeqie.
    wheel_zoom: bool = true,
    /// Thumbnails in the file list (T).
    thumbnails: bool = true,
    /// Write new thumbnails to the shared cache (~/.cache/thumbnails), as file managers do.
    save_thumbnails: bool = true,
    /// The info panel on the right (Ctrl+K). It is off by default, because reading EXIF costs a
    /// little for each image.
    info_panel: bool = false,
    /// The sort manager on the right (Shift+S), with folders to copy or move the selected files
    /// to.
    sort_manager: bool = false,
    /// The sort manager moves the files. When this is off, it copies them.
    sort_move: bool = false,
    /// Folders as a tree from / (Ctrl+T) instead of the subfolders of the current one.
    folder_tree: bool = false,
    /// Toolbar under the menu, as in Geeqie.
    toolbar: bool = true,
    /// One malloc arena for all threads. It uses much less memory, and decoding is up to about
    /// 8 % slower. It is read only at startup, because glibc must be set before threads start.
    low_memory: bool = true,
    /// How many images are decoded at once, at most the number of cores. It is read at startup.
    decode_threads: usize = 4,
    /// CPU use: saving, balanced or full, as an index into CPU_LEVELS.
    cpu_use: usize = 1,
    /// Frames of an animation kept, in MB.
    animation_mb: usize = 128,
    /// Folder path above the folder list.
    path_bar: bool = true,
    /// Restore the window's size, position and maximized state at startup.
    remember_window: bool = true,
    // The High Contrast theme while the desktop asks for high contrast (GNOME, KDE).
    follow_contrast: bool = true,
    // The size of the menus, lists and text, in percent, times the desktop's text size if
    // followed (GNOME's Large Text and text scaling).
    ui_scale: u32 = 100,
    follow_text_scale: bool = true,
    // Import Lightroom Catalog writes each image's keywords into its XMP sidecar.
    lightroom_keywords: bool = true,
    /// Keep the screen saver off while in full screen.
    inhibit_screensaver: bool = true,
    /// Retired in 0.76.0. It was the window title "Glimt - image name" instead of "image name -
    /// Glimt". Its true is read once into `window_title` (`move_app_name_first`), and it is
    /// written as false.
    app_name_first: bool = false,
    /// The window title, from a template of %name%, %path%, %folder% and %app%. When it is
    /// empty, the title is "image name - Glimt".
    window_title: String = String::new(),
    /// File list with size and date columns, as in Geeqie.
    file_details: bool = false,
    sort: Sort = Sort::Name,
    sort_ascending: bool = true,
    sort_case_sensitive: bool = true,
    /// The toolbar's icon size, small, medium or large, as an index.
    toolbar_icons: usize = 0,
    /// View → Filter: none, greyscale, inverted or contrast, as an index into colour::FILTERS.
    view_filter: usize = 0,
    /// File name search above the file list.
    search_bar: bool = true,
    /// Name, position and a few EXIF fields over the image in full screen (I), as Geeqie's
    /// image overlay.
    full_screen_info: bool = true,
    /// What the full screen info shows (FULL_SCREEN_INFO_TEMPLATE).
    full_screen_info_template: String = FULL_SCREEN_INFO_TEMPLATE.to_owned(),
    /// The histogram in the corner of the image (K): off, rgb or brightness, as an index.
    histogram: u8 = 0,
    /// Logarithmic histogram (J), which shows the small counts better.
    histogram_log: bool = false,
    /// The thumbnail size: small (96×72), medium (128×96) or large (192×144, from the
    /// freedesktop "large" cache), as an index.
    thumb_size: u8 = 0,
    /// The file name under each thumbnail of a grid: the icon grid, the grid in the image area
    /// and pan view. Without it the cells are shorter, so more rows fit.
    grid_names: bool = true,
    /// The file list as a grid of thumbnails (Ctrl+I) instead of rows.
    icons: bool = false,
    /// Colour behind the image (sRGB).
    background: Rgb = Rgb([0, 0, 0]),
    /// A checkerboard behind transparent images, where the background shows otherwise.
    checkerboard: bool = false,
    /// Window → Projector shows a ring on the projector where the pointer is over the image.
    projector_pointer: bool = true,
    /// In full screen the pointer is hidden while the mouse is still, unless work goes on.
    hide_pointer: bool = true,
    /// The theme's name. It is a built-in theme, or a file in `$XDG_CONFIG_HOME/glimt/themes`.
    /// "Dark" is the default for an image viewer, and "Desktop" follows the desktop's light or
    /// dark style.
    theme: String = String::from("Dark"),
    /// The search field finds names fuzzily, like fzf. When this is off, it finds them by
    /// regular expression, as Geeqie's file filter does.
    fuzzy_search: bool = true,
    /// Without fuzzy search, the regular expression tells upper and lower case apart.
    search_case_sensitive: bool = false,
    /// Re-read the folder when files are added, removed or renamed.
    auto_refresh: bool = true,
    /// Rotate images by their EXIF orientation (Geeqie: "Auto rotate image using Exif
    /// information").
    exif_rotate: bool = true,
    /// List images (Types menu in the status bar).
    list_images: bool = true,
    /// List camera RAW files.
    list_raw: bool = true,
    /// List files and folders whose names start with a dot.
    show_hidden: bool = false,
    /// The video feature lists video files.
    list_videos: bool = true,
    /// The video feature's command line that plays a video. When it is empty, the default
    /// application plays it.
    video_player: String = String::new(),
    /// The print dialog's choices: the printer chosen last (empty for a PDF file), the paper
    /// and the file names.
    print_printer: String = String::new(),
    print_paper: String = String::from("A4"),
    /// Which way the paper is turned: "auto", "portrait" or "landscape". With "auto", a wide
    /// image alone on its page goes landscape.
    print_orientation: String = String::from("auto"),
    print_caption: bool = true,
    /// The text under each printed image, as the full screen info's template (%name%, EXIF
    /// fields, %keywords% …).
    print_template: String = String::from("%name%"),
    /// The images on each page. It is 1, or a contact sheet of 2 to 20 (print::PER_PAGE).
    print_per_page: usize = 1,
    /// Around the printed page, in millimetres (0 to 50).
    print_margin: u32 = 10,
    /// With one image on a page, its size in per cent of the room it has (10 to 100).
    print_scale: u32 = 100,
    /// Each image fills its box, cut to it, instead of fitting in it.
    print_fill: bool = false,
    /// The text at the top and the bottom of every printed page. %page% and %pages% number the
    /// pages.
    print_header: String = String::new(),
    print_footer: String = String::new(),
    /// On both sides of the paper, turned on the long edge, where the printer can.
    print_two_sided: bool = false,
}

impl Config {
    pub fn parse(text: &str) -> Self {
        let mut c = Config::default();
        for (key, value) in text.lines().filter_map(|l| l.split_once('=')) {
            let key = key.trim();
            if !old_key_overridden(key, text) {
                c.set(setting_key_now(key), value.trim());
            }
        }
        c.preload = c.preload.min(20);
        c.decode_threads = c.decode_threads.clamp(1, 8);
        c.cpu_use = c.cpu_use.min(CPU_LEVELS.len() - 1);
        c.fade = c.fade.min(FADES.len() - 1);
        c.fit_mode = c.fit_mode.min(3);
        if !print::PER_PAGE.iter().any(|p| p.0 == c.print_per_page) {
            c.print_per_page = 1;
        }
        c.print_margin = c.print_margin.min(50);
        c.print_scale = c.print_scale.clamp(10, 100);
        c.animation_mb = c.animation_mb.clamp(8, 1024);
        c.ui_scale = c.ui_scale.clamp(50, 300);
        c.histogram = c.histogram.min(2);
        c.thumb_size = c.thumb_size.min(2);
        c.webp_quality = c.webp_quality.min(100);
        c.jpeg_quality = c.jpeg_quality.clamp(1, 100);
        c.slideshow_delay = c.slideshow_delay.clamp(1, MAX_SLIDESHOW_DELAY);
        c.toolbar_icons = c.toolbar_icons.min(2);
        c.view_filter = c.view_filter.min(3);
        move_app_name_first(&mut c.app_name_first, &mut c.window_title);
        c.theme = crate::ui::theme::theme_name_now(&c.theme).to_owned();
        c
    }

    /// The settings in `text`, and what is wrong in it, line by line. A problem is a line that
    /// is not key=value, a setting that does not exist or is set twice, or a value that cannot
    /// be read or is out of range. Empty lines and lines starting with # are left alone.
    pub fn parse_checked(text: &str) -> (Self, Vec<String>) {
        let config = Config::parse(text);
        let used = config.to_text();
        let used = |key: &str| value_in(&used, key);
        let lines: Vec<(usize, &str)> = (text.lines().enumerate())
            .map(|(n, l)| (n + 1, l.trim()))
            .filter(|(_, l)| !l.is_empty() && !l.starts_with('#'))
            .collect();
        let mut problems = Vec::new();
        for (k, &(n, line)) in lines.iter().enumerate() {
            let Some((key, value)) = line.split_once('=') else {
                problems.push(format!("line {n}: “{line}” is not setting=value"));
                continue;
            };
            let (key, value) = (key.trim(), value.trim());
            // The setting's key now, also for a line with its old key.
            let now = setting_key_now(key);
            // The line of an old key beside its new key is passed over, as `parse` does. It is
            // told, because a value changed there by hand would otherwise be lost unseen.
            if old_key_overridden(key, text) {
                problems.push(format!(
                    "line {n}: {key} is the old name of {now}, which this file also sets; \
                     this line is ignored"
                ));
                continue;
            }
            let has_key = |&(_, l): &(usize, &str)| {
                let key = l.split_once('=').map(|(o, _)| o.trim());
                key.filter(|o| !old_key_overridden(o, text))
                    .map(setting_key_now)
                    == Some(now)
            };
            let again = lines[..k].iter().any(has_key);
            let later = lines[k + 1..].iter().any(has_key);
            let mut alone = Config::default();
            match alone.set(now, value) {
                None => problems.push(format!("line {n}: there is no setting {key}")),
                Some(false) => problems.push(format!(
                    "line {n}: {key}={value} is not a value it takes; {} is used",
                    used(now)
                )),
                Some(true) if again => {
                    problems.push(format!("line {n}: {key} is set again; the last is used"));
                }
                // When the setting is set again later, that later line is the one checked.
                Some(true) if later => {}
                Some(true) => {
                    move_app_name_first(&mut alone.app_name_first, &mut alone.window_title);
                    let wanted = value_in(&alone.to_text(), now);
                    if wanted != used(now) {
                        problems.push(format!(
                            "line {n}: {key}={value} is out of range; {} is used",
                            used(now)
                        ));
                    }
                }
            }
        }
        (config, problems)
    }

    pub fn load() -> Self {
        Self::load_checked().0
    }

    /// The settings, and what is wrong in their file. That is its lines, or that it cannot be
    /// read.
    pub fn load_checked() -> (Self, Vec<String>) {
        let read = config_path().map(|p| read_lossy(&p));
        let (config, problems) = match read {
            // A byte that is not UTF-8 (a path in another encoding) does not cost the rest.
            Some(Ok(text)) => Self::parse_checked(&text),
            Some(Err(e)) if e.kind() != std::io::ErrorKind::NotFound => (
                Config::default(),
                vec![format!("cannot be read ({e}); the defaults are used")],
            ),
            _ => (Config::default(), Vec::new()),
        };
        *LAST_SAVED_TEXT.lock().unwrap_or_else(|e| e.into_inner()) = config.to_text();
        (config, problems)
    }

    /// Saves the settings. Only the settings this window changed go over the file, and the
    /// others keep what other windows wrote meanwhile. Locked settings are never written.
    pub fn save(&self) {
        if locked() {
            return;
        }
        if let Some(p) = config_path() {
            let ours = self.to_text();
            let before = std::mem::replace(
                &mut *LAST_SAVED_TEXT.lock().unwrap_or_else(|e| e.into_inner()),
                ours.clone(),
            );
            // The file is read, merged and written in the background, because each toggle in a
            // menu saves, and the file's sync must not hold the window.
            write_later(move || {
                let merge = |on_disk: &[u8]| {
                    let on_disk = String::from_utf8_lossy(on_disk);
                    (merged(&ours, &before, &on_disk).into_bytes(), ())
                };
                // A file that cannot be read is not written over, because every setting would
                // be lost.
                if let Err(e) = edit_shared(&p, merge) {
                    save_failed(crate::tf!(
                        "Could not save {name}: {e}",
                        name = p.display(),
                        e
                    ));
                }
            });
        }
    }
}

/// `$XDG_CONFIG_HOME/glimt/config`, otherwise `~/.config/glimt/config`.
pub fn config_path() -> Option<PathBuf> {
    xdg_file("XDG_CONFIG_HOME", ".config", "config")
}

/// `$XDG_CONFIG_HOME/glimt/keywords`, the keyword tree, with a path such as "Places|Norway|Oslo"
/// on each line.
pub fn keyword_tree_path() -> Option<PathBuf> {
    xdg_file("XDG_CONFIG_HOME", ".config", "keywords")
}

/// `$XDG_CONFIG_HOME/glimt/sort-folders`, the sort manager's folders, one on each line.
pub fn sort_folders_path() -> Option<PathBuf> {
    xdg_file("XDG_CONFIG_HOME", ".config", "sort-folders")
}

/// The folders of the sort-folders file, read as bytes, so a folder name that is not UTF-8 is
/// kept.
pub fn sort_folders_from(text: &[u8]) -> Vec<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    (text.split(|&b| b == b'\n'))
        .filter(|l| !l.is_empty())
        .map(|l| PathBuf::from(std::ffi::OsStr::from_bytes(l)))
        .collect()
}

/// The sort-folders file's text for `dirs`. A name with a line break cannot be kept in it.
pub fn sort_folders_text(dirs: &[PathBuf]) -> Vec<u8> {
    let mut text = Vec::new();
    for d in dirs {
        if fits_on_a_line(d) {
            text.extend(d.as_os_str().as_encoded_bytes());
            text.push(b'\n');
        }
    }
    text
}

/// `on_disk` with `add` added and `remove` taken out. The change is applied to the file as it is
/// now, so folders that another window added meanwhile stay.
pub fn sort_folders_changed(
    mut on_disk: Vec<PathBuf>,
    add: Option<&Path>,
    remove: Option<&Path>,
) -> Vec<PathBuf> {
    on_disk.retain(|d| Some(d.as_path()) != remove);
    if let Some(add) = add.filter(|a| !on_disk.iter().any(|d| d == a)) {
        on_disk.push(add.to_path_buf());
    }
    on_disk
}

/// `$XDG_STATE_HOME/glimt/state`, otherwise `~/.local/state/glimt/state`.
/// State (such as the last folder) is not configuration, and belongs here according to XDG.
pub fn state_path() -> Option<PathBuf> {
    xdg_file("XDG_STATE_HOME", ".local/state", "state")
}

/// `$XDG_STATE_HOME/glimt/recent`, File → Open Recent's folders and collections, newest first,
/// one on each line. The windows share it.
pub fn recent_path() -> Option<PathBuf> {
    xdg_file("XDG_STATE_HOME", ".local/state", "recent")
}

/// Returns `$XDG_STATE_HOME/glimt/sharing`, which holds what File → Copy for Sharing left out
/// last time (`shared_copy::LeftOut`), one word on each line. The windows share it.
pub fn sharing_path() -> Option<PathBuf> {
    xdg_file("XDG_STATE_HOME", ".local/state", "sharing")
}

/// The paths of a recent file's text, read as bytes, because a name need not be UTF-8.
pub fn parse_recent(text: &[u8]) -> Vec<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    (text.split(|&b| b == b'\n'))
        .filter(|l| !l.is_empty())
        .map(|l| std::ffi::OsStr::from_bytes(l).into())
        .take(MAX_RECENT)
        .collect()
}

/// `recent` with `path` first, once, and no more than MAX_RECENT paths. A name with a line break in
/// it cannot be kept on a line, so it is left out.
pub fn add_recent(recent: &[PathBuf], path: &Path) -> Vec<PathBuf> {
    (fits_on_a_line(path).then(|| path.to_path_buf()).into_iter())
        .chain(recent.iter().filter(|p| *p != path).cloned())
        .take(MAX_RECENT)
        .collect()
}

/// The text of a recent file.
pub fn recent_text(recent: &[PathBuf]) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    recent
        .iter()
        .flat_map(|p| [p.as_os_str().as_bytes(), b"\n"].concat())
        .collect()
}

pub fn xdg_file(var: &str, home_fallback: &str, name: &str) -> Option<PathBuf> {
    Some(xdg_dir(var, home_fallback, "glimt")?.join(name))
}

/// Returns the host's folder `app` under the XDG folder `var` (such as XDG_DATA_HOME), or under
/// `home_fallback` in the home. In a Flatpak the XDG folders point into the sandbox, so the
/// host's own come from HOST_XDG_*. The shared thumbnail cache and the trash are the host's.
pub fn host_xdg_dir(var: &str, home_fallback: &str, app: &str) -> Option<PathBuf> {
    let flatpak = crate::files::external::in_flatpak();
    xdg_dir(&host_var(var, flatpak), home_fallback, app)
}

/// Returns the variable that names the host's XDG folder `var`: HOST_ before it in a Flatpak.
fn host_var(var: &str, flatpak: bool) -> String {
    match flatpak {
        true => format!("HOST_{var}"),
        false => var.to_owned(),
    }
}

pub fn xdg_dir(var: &str, home_fallback: &str, app: &str) -> Option<PathBuf> {
    // Tests have a home of their own, because an App made in a test removed the user's recent
    // list.
    #[cfg(test)]
    return {
        let _ = var;
        Some(test_home().join(home_fallback).join(app))
    };
    #[cfg(not(test))]
    return xdg_base(
        std::env::var_os(var),
        std::env::var_os("HOME"),
        home_fallback,
    )
    .map(|base| base.join(app));
}

/// The base folder from an XDG variable's value `set`, or else `home` joined with
/// `home_fallback`. A relative folder is ignored, as the XDG spec says. An empty
/// $XDG_DATA_HOME would otherwise be the current folder, where a downloaded folder could hold
/// tools and added formats. Returns `None` when neither gives an absolute folder.
fn xdg_base(
    set: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
    home_fallback: &str,
) -> Option<PathBuf> {
    (set.map(PathBuf::from))
        .filter(|p| p.is_absolute())
        .or_else(|| home.map(|h| PathBuf::from(h).join(home_fallback)))
        .filter(|p| p.is_absolute())
}

/// The home folder of the tests, under the temporary folder, for every XDG folder. There is one
/// for every run, so what a test leaves there (Rename Many's empty plans folder) is never more
/// than one tree. The files in it have names of their own (plans by instance and time).
#[cfg(test)]
pub fn test_home() -> PathBuf {
    std::env::temp_dir().join("glimt-test-home")
}

/// Whether `path` can be kept on a line of Glimt's own files. A name with a line break would be
/// read back as two lines, the second of them perhaps taken for a key of its own.
pub fn fits_on_a_line(path: &Path) -> bool {
    !path.as_os_str().as_encoded_bytes().contains(&b'\n')
}

/// The window state's line that keeps `dir` as the last folder. It is empty when `dir` does
/// not fit on a line.
pub fn last_dir_line(dir: &Path) -> String {
    match fits_on_a_line(dir) {
        true => format!("{LAST_DIR}={}\n", dir.display()),
        false => String::new(),
    }
}

pub fn parse_last_dir(state: &str) -> Option<PathBuf> {
    state_value(state, LAST_DIR).map(PathBuf::from)
}

/// Returns the value of `key` in the window state's text. A key renamed in `RENAMED_STATE` is
/// read by its old name when the text does not hold the new one. Returns `None` when neither
/// is there with a value.
pub fn state_value<'a>(state: &'a str, key: &str) -> Option<&'a str> {
    let old_key = RENAMED_STATE.iter().find(|r| r.1 == key).map(|r| r.0);
    state_line_value(state, key).or_else(|| state_line_value(state, old_key?))
}

/// Returns the value of the first line of `key` in the window state's text, or `None` when no
/// line has that key with a value.
fn state_line_value<'a>(state: &'a str, key: &str) -> Option<&'a str> {
    state.lines().find_map(|l| {
        l.strip_prefix(key)?
            .strip_prefix('=')
            .filter(|v| !v.is_empty())
    })
}

/// Returns the key a setting has now: the new key of one renamed in `RENAMED_SETTINGS`, and
/// `key` itself for every other.
fn setting_key_now(key: &str) -> &str {
    (RENAMED_SETTINGS.iter())
        .find(|r| r.0 == key)
        .map_or(key, |r| r.1)
}

/// Whether the line of `key` in the settings file `text` is passed over. That is so when `key`
/// is the old key of a renamed setting and `text` also sets its new key, whose value counts.
fn old_key_overridden(key: &str, text: &str) -> bool {
    let key = key.trim();
    let now = setting_key_now(key);
    now != key && (text.lines()).any(|l| l.split_once('=').is_some_and(|(k, _)| k.trim() == now))
}

/// Moves the retired app_name_first=true into the window title it stood for, when the user has
/// no title of their own. The setting is false afterwards, so it is moved only once, and a
/// title the user empties later stays empty.
fn move_app_name_first(app_name_first: &mut bool, window_title: &mut String) {
    if std::mem::take(app_name_first) && window_title.is_empty() {
        *window_title = APP_NAME_FIRST_TITLE.to_owned();
    }
}

/// The `value` of `key` as the code holds it. A name becomes its place in its list, and other
/// values stay as given.
fn by_number(key: &str, value: &str) -> String {
    let names = NAMED.iter().find(|n| n.0 == key).map(|n| n.1);
    let place = names.and_then(|names| names.iter().position(|n| n.eq_ignore_ascii_case(value)));
    place.map_or_else(|| value.to_owned(), |k| k.to_string())
}

/// The `value` of `key` as the file holds it, which is the name of a numbered choice.
fn by_name(key: &str, value: String) -> String {
    let names = NAMED.iter().find(|n| n.0 == key).map(|n| n.1);
    match names.zip(value.parse::<usize>().ok()) {
        Some((names, k)) if k < names.len() => names[k].to_owned(),
        _ => value,
    }
}

/// A settings file's text. A byte that is not UTF-8 (a path in another encoding) becomes the
/// replacement character. The file is read this way when loaded, and when saved again, because
/// the save merges into it.
fn read_lossy(path: &Path) -> std::io::Result<String> {
    Ok(String::from_utf8_lossy(&crate::images::decode::read_limited(path)?).into_owned())
}

/// The value of `key` in key=value lines, as `to_text` writes them.
fn value_in(text: &str, key: &str) -> String {
    let line = text
        .lines()
        .find_map(|l| l.strip_prefix(key)?.strip_prefix('='));
    line.unwrap_or_default().to_owned()
}

pub fn lock() {
    SETTINGS_LOCKED.store(true, std::sync::atomic::Ordering::Relaxed);
}

pub fn locked() -> bool {
    SETTINGS_LOCKED.load(std::sync::atomic::Ordering::Relaxed)
}

/// `ours` (key=value lines), except that where a value is as this window `saved` it, the one
/// `on_disk` is kept, because another window may have changed that setting since. The result
/// keeps the file's own order, its comments and blank lines, with the settings it lacks after
/// them. Settings this Glimt does not know, from a newer one (a Flatpak beside a .deb), stay.
/// The line of a setting's old key in `RENAMED_SETTINGS` goes, and its new key takes its value.
fn merged(ours: &str, saved: &str, on_disk: &str) -> String {
    let pairs = |text: &str| -> std::collections::HashMap<String, String> {
        (text.lines().filter_map(|l| l.split_once('=')))
            .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
            .collect()
    };
    let (saved, mut theirs) = (pairs(saved), pairs(on_disk));
    // A value under a setting's old key on disk counts as the new key's when the file lacks the
    // new key, as `Config::parse` reads it. An older Glimt that shares the file writes it so.
    for (old, new) in RENAMED_SETTINGS {
        if let Some(value) = theirs.remove(old) {
            theirs.entry(new.to_owned()).or_insert(value);
        }
    }
    // The retired app_name_first=true on disk is moved into the window title there too, so a
    // save writes that title and app_name_first=false, and the move is not made again.
    if theirs.get("app_name_first").is_some_and(|v| v == "true") {
        let mut title = theirs.remove("window_title").unwrap_or_default();
        move_app_name_first(&mut true, &mut title);
        theirs.insert("window_title".to_owned(), title);
        theirs.insert("app_name_first".to_owned(), "false".to_owned());
    }
    let line = |l: &str| -> String {
        let value = l
            .split_once('=')
            .filter(|(k, v)| saved.get(*k).is_some_and(|s| s == v))
            .and_then(|(k, _)| Some((k, theirs.get(k)?)));
        match value {
            Some((k, v)) => format!("{k}={v}\n"),
            None => format!("{l}\n"),
        }
    };
    let key = |l: &str| Some(l.split_once('=')?.0.trim().to_owned());
    let mine: std::collections::HashMap<String, &str> =
        ours.lines().filter_map(|l| Some((key(l)?, l))).collect();
    let mut done = std::collections::HashSet::new();
    let mut text = String::new();
    for l in on_disk.lines() {
        // The line of an old key goes, because its new key is written instead. So after one
        // save the file holds only new keys, and no line in it is ignored without a word.
        let is_old_key = |k: &str| setting_key_now(k) != k && mine.contains_key(setting_key_now(k));
        if key(l).is_some_and(|k| is_old_key(&k)) {
            continue;
        }
        match key(l).filter(|k| mine.contains_key(k)) {
            // Ours, once (a setting set twice by hand comes out once).
            Some(k) => {
                if done.insert(k.clone()) {
                    text += &line(mine[&k]);
                }
            }
            None => text += &format!("{l}\n"),
        }
    }
    // Those the file lacks, in this Glimt's order.
    for l in ours
        .lines()
        .filter(|l| key(l).is_none_or(|k| !done.contains(&k)))
    {
        text += &line(l);
    }
    text
}

#[cfg(test)]
mod tests {
    /// In a Flatpak the host's XDG folder is named by HOST_ before the variable, because the
    /// variable itself points into the sandbox; outside one it is the variable itself.
    #[test]
    fn names_the_hosts_xdg_folder() {
        assert_eq!(super::host_var("XDG_DATA_HOME", true), "HOST_XDG_DATA_HOME");
        assert_eq!(super::host_var("XDG_DATA_HOME", false), "XDG_DATA_HOME");
    }

    /// An XDG variable that holds a relative folder, or is empty, is passed over for the home
    /// folder's default, as the XDG spec says. With no absolute folder at all there is none.
    #[test]
    fn xdg_folders_are_absolute() {
        let base = |set: Option<&str>, home: Option<&str>| {
            super::xdg_base(set.map(Into::into), home.map(Into::into), ".local/share")
        };
        let home = Some("/home/u");
        let default = Some(std::path::PathBuf::from("/home/u/.local/share"));
        assert_eq!(base(Some("/data"), home), Some("/data".into()));
        assert_eq!(base(Some(""), home), default);
        assert_eq!(base(Some("share"), home), default);
        assert_eq!(base(None, home), default);
        assert_eq!(base(Some(""), Some("relative")), None);
    }

    /// The file Glimt writes has nothing wrong, and each kind of mistake is told with its line.
    #[test]
    fn checks_the_settings_file() {
        let (_, none) = super::Config::parse_checked(&super::Config::default().to_text());
        assert_eq!(none, Vec::<String>::new());
        let text = "# note\nthumbnails=ja\nthumbnail=true\nui_scale=500\nslideshow_delay=4\nslideshow_delay=5\nwords\n\nfit_mode=2\n";
        let (config, problems) = super::Config::parse_checked(text);
        assert_eq!(
            problems,
            [
                "line 2: thumbnails=ja is not a value it takes; true is used",
                "line 3: there is no setting thumbnail",
                "line 4: ui_scale=500 is out of range; 300 is used",
                "line 6: slideshow_delay is set again; the last is used",
                "line 7: “words” is not setting=value",
            ]
        );
        assert_eq!(
            (config.slideshow_delay, config.fit_mode, config.ui_scale),
            (5, 2, 300)
        );
    }

    /// Another window's folder stays when this one adds or takes out one.
    #[test]
    fn changes_sort_folders_as_they_are() {
        let p = |s: &str| std::path::PathBuf::from(s);
        let on_disk = vec![p("/a"), p("/from-b")];
        let after = super::sort_folders_changed(on_disk.clone(), Some(&p("/c")), None);
        assert_eq!(after, [p("/a"), p("/from-b"), p("/c")]);
        assert_eq!(
            super::sort_folders_changed(on_disk, None, Some(&p("/a"))),
            [p("/from-b")]
        );
        let text = super::sort_folders_text(&[p("/x"), p("/bad\nname")]);
        assert_eq!(super::sort_folders_from(&text), [p("/x")]);
    }

    /// When this window changed the sort order and another turned thumbnails off, both changes
    /// stay.
    #[test]
    fn keeps_what_other_windows_changed() {
        let saved = "thumbnails=true\nsort=name\n";
        let ours = "thumbnails=true\nsort=date\n";
        let on_disk = "thumbnails = false\nsort=name\nfrom_a_newer_glimt=1\n";
        let text = super::merged(ours, saved, on_disk);
        assert_eq!(text, "thumbnails=false\nsort=date\nfrom_a_newer_glimt=1\n");
        // The user's comments and blank lines stay where they are.
        let on_disk = "# my viewer\nsort=name\n\n# thumbs off on the laptop\nthumbnails=false\n";
        let text = super::merged(ours, saved, on_disk);
        assert_eq!(
            text,
            "# my viewer\nsort=date\n\n# thumbs off on the laptop\nthumbnails=false\n"
        );
    }

    /// The sort manager's folders come back as they were, also a name that is not UTF-8.
    #[test]
    fn reads_sort_folders_as_bytes() {
        use std::os::unix::ffi::OsStrExt;
        let latin = std::ffi::OsStr::from_bytes(b"/caf\xe9");
        assert_eq!(
            super::sort_folders_from(b"/a b\n/caf\xe9\n\n"),
            [std::path::PathBuf::from("/a b"), latin.into()]
        );
    }

    /// File names in the grids are on by default, and off stays off through a save and a read.
    #[test]
    fn keeps_grid_names_off() {
        use super::Config;
        assert!(Config::default().grid_names);
        let off = Config::parse("grid_names=false\n");
        assert!(!off.grid_names);
        assert!(off.to_text().contains("\ngrid_names=false\n"));
        assert!(!Config::parse(&off.to_text()).grid_names);
    }

    #[test]
    fn parses_config() {
        use super::Config;
        assert_eq!(Config::parse("# comment\npreload= 5\n").preload, 5);
        assert_eq!(Config::parse("preload=99").preload, 20);
        let c = Config::parse("decode_threads=0\nanimation_mb=5000");
        assert_eq!((c.decode_threads, c.animation_mb), (1, 1024));
        // The defaults are the Medium level.
        let d = Config::default();
        let medium = super::MEMORY_LEVELS
            .iter()
            .find(|l| l.name == "Medium")
            .unwrap();
        assert_eq!(
            (d.preload, d.decode_threads, d.animation_mb, d.low_memory),
            (
                medium.preload,
                medium.decode_threads,
                medium.animation_mb,
                medium.one_arena
            )
        );
        assert_eq!(Config::parse("slideshow_delay=0").slideshow_delay, 1);
        assert_eq!(Config::parse("slideshow_delay=7").slideshow_delay, 7);
        // Every setting survives a save and load.
        let mut c =
            Config::parse("delete_to_trash=false\nsort=size\nbackground=#102030\ntoolbar_icons=2");
        c.video_player = "mpv --fs".into();
        assert_eq!(Config::parse(&c.to_text()), c);
        assert!(
            Config::default().delete_to_trash
                && !Config::parse("delete_to_trash=false").delete_to_trash
        );
        assert!(
            Config::default().confirm_trash && !Config::parse("confirm_trash=false").confirm_trash
        );
        assert!(Config::default().delete_key && !Config::parse("delete_key=false").delete_key);
        assert!(Config::parse("remember_dir=true").remember_dir);
        assert!(
            !Config::default().slideshow_random
                && Config::parse("slideshow_random=true").slideshow_random
        );
        assert!(Config::default().enlarge && !Config::parse("enlarge=false").enlarge);
        assert!(Config::default().wheel_zoom && !Config::parse("wheel_zoom=false").wheel_zoom);
        assert!(Config::default().thumbnails && !Config::parse("thumbnails=false").thumbnails);
        assert!(Config::default().save_thumbnails);
        assert!(!Config::default().info_panel && Config::parse("info_panel=true").info_panel);
        assert!(Config::default().toolbar && !Config::parse("toolbar=false").toolbar);
        assert!(Config::default().low_memory && !Config::parse("low_memory=false").low_memory);
        assert!(Config::default().path_bar && !Config::parse("path_bar=false").path_bar);
        assert_eq!(Config::default().background.0, [0, 0, 0]);
        assert!(Config::default().remember_window && Config::default().inhibit_screensaver);
        assert!(!Config::parse("remember_window=false").remember_window);
        assert!(!Config::parse("inhibit_screensaver=false").inhibit_screensaver);
        assert!(!Config::default().app_name_first);
        assert!(!Config::default().file_details && Config::parse("file_details=true").file_details);
        let c =
            Config::parse("sort=exif_digitized\nsort_ascending=false\nsort_case_sensitive=false");
        assert_eq!(
            (c.sort, c.sort_ascending, c.sort_case_sensitive),
            (crate::model::listing::Sort::ExifDigitized, false, false)
        );
        assert_eq!(
            Config::parse("sort=nonsense").sort,
            crate::model::listing::Sort::Name
        );
        assert_eq!(Config::default().toolbar_icons, 0);
        assert!(Config::default().search_bar && !Config::parse("search_bar=false").search_bar);
        assert_eq!(Config::parse("toolbar_icons=1").toolbar_icons, 1);
        assert_eq!(Config::parse("toolbar_icons=9").toolbar_icons, 2);
        assert!(!Config::parse("app_name_first=true").app_name_first);
        let win = super::WindowState {
            size: Some(eframe::egui::vec2(1000.0, 700.5)),
            position: Some(eframe::egui::pos2(10.0, -20.0)),
            maximized: true,
        };
        let text = format!("last_dir=/x\n{}", win.to_text());
        assert_eq!(super::WindowState::parse(&text), win);
        assert_eq!(super::WindowState::parse("window_size=5,5\n").size, None);
        assert_eq!(
            Config::parse("background=#1a2B3c").background.0,
            [0x1a, 0x2b, 0x3c]
        );
        assert_eq!(Config::parse("background=#12").background.0, [0, 0, 0]);
        assert!(!Config::parse("save_thumbnails=false").save_thumbnails);
        assert!(Config::default().exif_rotate && !Config::parse("exif_rotate=false").exif_rotate);
        assert!(Config::default().list_videos && !Config::parse("list_videos=false").list_videos);
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
        // A folder name with a line break is not kept, as its second line would be read as a
        // key of the window state.
        let line = super::last_dir_line(std::path::Path::new("/tmp/a b"));
        assert_eq!(super::parse_last_dir(&line), Some("/tmp/a b".into()));
        assert_eq!(
            super::last_dir_line(std::path::Path::new("/x\nwindow_x=99999")),
            ""
        );
        assert_eq!(Config::parse("preload=x\nunknown=1"), Config::default());
        // A byte that is not UTF-8 costs neither the load nor the next save.
        let dir = std::env::temp_dir().join(format!("glimt-lossy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A new file never lands over one made meanwhile.
        let file = dir.join("new.gqv");
        crate::files::file_io::create_file(&file, "first").unwrap();
        assert!(crate::files::file_io::create_file(&file, "second").is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "first");
        let left = std::fs::read_dir(&dir).unwrap().flatten();
        assert!(
            !left
                .into_iter()
                .any(|e| e.file_name().to_string_lossy().starts_with(".glimt-tmp-"))
        );
        // A name near the limit of 255 bytes is written again, and its sidecar too, because the
        // temporary name made from it was too long.
        let long = dir.join(format!("{}.jpg", "a".repeat(240)));
        crate::files::file_io::create_file(&long, "x").unwrap();
        crate::files::file_io::replace_file(&long, "y").unwrap();
        crate::files::file_io::replace_file(&crate::metadata::xmp::sidecar(&long), "<x/>").unwrap();
        assert_eq!(std::fs::read_to_string(&long).unwrap(), "y");
        let file = dir.join("config");
        std::fs::write(&file, b"slideshow_delay=5\nvideo_player=mpv \xff\n").unwrap();
        let text = super::read_lossy(&file).unwrap();
        assert_eq!(Config::parse(&text).slideshow_delay, 5);
        // A file that is not there gives none. One that is there but cannot be read (a folder)
        // gives an error and is never written over.
        assert_eq!(
            crate::files::file_io::read_or_none(&dir.join("none")).unwrap(),
            None
        );
        assert!(crate::files::file_io::read_or_none(&dir).is_err());
        assert!(
            crate::files::file_io::read_or_none(&file)
                .unwrap()
                .is_some()
        );
        assert!(text.contains('\u{fffd}'));
        std::fs::remove_dir_all(&dir).unwrap();
        // The choices are read by name, or by the number used before 1.0, and written by name.
        let c = Config::parse("fit_mode=width\nfade=2\nview_filter=Inverted\ncpu_use=9");
        assert_eq!((c.fit_mode, c.fade, c.view_filter, c.cpu_use), (1, 2, 2, 2));
        let text = c.to_text();
        for line in [
            "fit_mode=width\n",
            "fade=long\n",
            "view_filter=inverted\n",
            "cpu_use=full\n",
        ] {
            assert!(text.contains(line), "{text}");
        }
        // A built-in theme is read by its old name too, and written by its new one.
        let theme = Config::parse("theme=Contrast").to_text();
        assert!(theme.contains("\ntheme=High Contrast Light\n"), "{theme}");
        let (_, problems) = Config::parse_checked("fade=9\nfit_mode=wide");
        assert!(problems[0].contains("long is used") && problems[1].contains("not a value"));
        for (k, (name, _, _)) in super::FADES.iter().enumerate() {
            assert_eq!(super::NAMED[0].1[k], *name);
        }
    }

    /// Open Recent has the newest first, each once and at most MAX_RECENT, and it comes back from
    /// its file as it was written, also a name that is not UTF-8.
    #[test]
    fn keeps_recent_folders() {
        use std::os::unix::ffi::OsStrExt;
        let p = |s: &str| std::path::PathBuf::from(s);
        let latin: std::path::PathBuf = std::ffi::OsStr::from_bytes(b"/caf\xe9").into();
        let mut recent = Vec::new();
        for path in ["/a", "/b", "/a"] {
            recent = super::add_recent(&recent, &p(path));
        }
        assert_eq!(recent, [p("/a"), p("/b")]);
        recent = super::add_recent(&recent, &latin);
        recent = super::add_recent(&recent, &p("/line\nbreak"));
        assert_eq!(recent, [latin.clone(), p("/a"), p("/b")]);
        assert_eq!(super::parse_recent(&super::recent_text(&recent)), recent);
        for k in 0..20 {
            recent = super::add_recent(&recent, &p(&format!("/{k}")));
        }
        assert_eq!(recent.len(), super::MAX_RECENT);
        assert_eq!(recent[0], p("/19"));
    }

    /// Config and window state files broken in thousands of ways are read without a panic.
    #[test]
    fn survives_broken_files() {
        let seed = super::Config::default().to_text();
        for bytes in crate::support::test_support::broken(seed.as_bytes()) {
            super::Config::parse(&String::from_utf8_lossy(&bytes));
        }
        for bytes in
            crate::support::test_support::broken(b"maximized=true\nsize=1000x700.5\npos=10,-20\n")
        {
            super::WindowState::parse(&String::from_utf8_lossy(&bytes));
        }
    }

    /// The names that users' settings files and scripts hold are frozen in 1.x: the settings'
    /// keys and the values that name something, the window state's keys, the --remote commands
    /// with their words and exit codes, and the command-line options. src/frozen-1.0.txt pins
    /// them. Before, a rename passed every test, because the man page they were checked
    /// against was changed with it.
    #[test]
    fn frozen_names_are_kept() {
        let now = names_now();
        let frozen: Vec<&str> = (include_str!("../frozen-1.0.txt").lines())
            .filter(|l| !l.starts_with('#') && !l.is_empty())
            .collect();
        // An old name passes when its table of old names maps it to a name that is read now.
        let gone: Vec<&&str> = frozen
            .iter()
            .filter(|f| !now.contains(&frozen_name_now(f)))
            .collect();
        assert!(
            gone.is_empty(),
            "frozen in 1.x, so bring these back: {gone:?}"
        );
        let new: Vec<&String> = now
            .iter()
            .filter(|n| !frozen.contains(&n.as_str()))
            .collect();
        assert!(
            new.is_empty(),
            "add these at the end of src/frozen-1.0.txt: {new:?}"
        );
        // A named value's place is the number it was written as before 1.0, so a list grows
        // only at its end. The pinned values must therefore come in the list's own order.
        for (key, values) in super::NAMED {
            let prefix = format!("value {key} ");
            let pinned: Vec<&str> = (frozen.iter())
                .filter_map(|l| l.strip_prefix(prefix.as_str()))
                .collect();
            assert_eq!(pinned, values, "{key} gains values only at its end");
        }
    }

    /// The retired app_name_first=true becomes the window title it stood for, once: a title of
    /// the user's own stays, a save writes the title and app_name_first=false, and a title
    /// emptied afterwards stays empty. --check-config finds nothing wrong with the old line.
    #[test]
    fn app_name_first_is_read_once_into_the_window_title() {
        use super::Config;
        let old = "app_name_first=true\n";
        let c = Config::parse(old);
        assert_eq!(
            (c.app_name_first, c.window_title.as_str()),
            (false, "%app% - %name%")
        );
        let own = Config::parse("app_name_first=true\nwindow_title=%name%\n");
        assert_eq!(own.window_title, "%name%");
        assert_eq!(Config::parse_checked(old).1, Vec::<String>::new());
        // Saved by a window that changed another setting: the title is written, not the old
        // setting's true.
        let saved = c.to_text();
        let ours = saved.replace("thumbnails=true", "thumbnails=false");
        let written = super::merged(&ours, &saved, old);
        let again = Config::parse(&written);
        assert_eq!(
            (again.window_title.as_str(), again.thumbnails),
            ("%app% - %name%", false)
        );
        assert!(written.contains("app_name_first=false\n"), "{written}");
        // The user empties the title: it stays empty after a save and a load.
        let mut emptied = Config::parse(&written);
        emptied.window_title.clear();
        let written = super::merged(&emptied.to_text(), &saved, &written);
        assert_eq!(Config::parse(&written).window_title, "");
    }

    /// Each renamed setting is read by its old key too, and a file with both keys has the value
    /// of the new one, wherever it stands. Only the new key is written, also by a save into the
    /// file on disk, which takes the old key's line out. --check-config finds nothing wrong with
    /// an old key alone, and tells the line of one that its new key overrides. The window
    /// state's renamed keys are read the same way, and its file is written whole.
    #[test]
    fn old_keys_are_read_and_only_new_ones_written() {
        use super::Config;
        let defaults = Config::default().to_text();
        for (old, new) in super::RENAMED_SETTINGS {
            let value = super::value_in(&defaults, new);
            // Another value than the default: a bool turned, a number one larger, or a text with
            // a letter more.
            let other = match value.as_str() {
                "true" => "false".to_owned(),
                "false" => "true".to_owned(),
                text => text
                    .parse::<u64>()
                    .map_or_else(|_| format!("{text}x"), |n| (n + 1).to_string()),
            };
            let text = format!("{old}={other}\n");
            let written = Config::parse(&text).to_text();
            assert_eq!(super::value_in(&written, new), other, "{old} is not read");
            let old_line = format!("{old}=");
            assert!(
                !written.lines().any(|l| l.starts_with(&old_line)),
                "{old} written"
            );
            assert_eq!(Config::parse_checked(&text).1, Vec::<String>::new());
            for (both, line) in [
                (format!("{new}={value}\n{old}={other}\n"), 2),
                (format!("{old}={other}\n{new}={value}\n"), 1),
            ] {
                let written = Config::parse(&both).to_text();
                assert_eq!(super::value_in(&written, new), value, "{both}");
                let ignored = format!(
                    "line {line}: {old} is the old name of {new}, which this file also sets; \
                     this line is ignored"
                );
                assert_eq!(Config::parse_checked(&both).1, [ignored]);
                // A save takes the old line out and keeps the new key's value.
                let saved = super::merged(&defaults, &defaults, &both);
                assert!(!saved.lines().any(|l| l.starts_with(&old_line)), "{saved}");
                assert_eq!(super::value_in(&saved, new), value);
            }
            // A save of a file with only the old key writes its value under the new key.
            let saved = super::merged(&defaults, &defaults, &text);
            assert!(!saved.lines().any(|l| l.starts_with(&old_line)), "{saved}");
            assert_eq!(super::value_in(&saved, new), other);
        }
        let position = |text: &str| super::WindowState::parse(text).position;
        let at = |x, y| Some(eframe::egui::pos2(x, y));
        assert_eq!(position("window_pos=10,20\n"), at(10.0, 20.0));
        assert_eq!(
            position("window_pos=10,20\nwindow_position=1,2\n"),
            at(1.0, 2.0)
        );
        let written = super::WindowState::parse("window_pos=10,20\n").to_text();
        assert!(written.contains("window_position=10,20\n") && !written.contains("window_pos="));
    }

    /// No old name in a table of old names is a setting, a window state key or a --remote
    /// command now. One given an old name later would otherwise be read as the name it was
    /// renamed to, and never under its own.
    #[test]
    fn old_names_are_not_names_now() {
        use crate::features::remote::{COMMANDS, RENAMED_COMMANDS};
        let now = names_now();
        for (kind, table) in [
            ("setting", &super::RENAMED_SETTINGS[..]),
            ("state", &super::RENAMED_STATE[..]),
        ] {
            for (old, _) in table {
                assert!(
                    !now.contains(&format!("{kind} {old}")),
                    "{kind} {old} is an old name in a table of old names, so it is no name now"
                );
            }
        }
        for (old, _) in crate::ui::theme::RENAMED_THEMES {
            assert!(
                !now.contains(&format!("value theme {old}")),
                "{old} is an old name in RENAMED_THEMES, so it is no theme now"
            );
        }
        // The commands are read from `COMMANDS` itself, which also has the release tests' "where".
        for (old, _) in RENAMED_COMMANDS {
            assert!(
                !(COMMANDS.iter()).any(|(usage, _)| usage.split(' ').next() == Some(old)),
                "remote {old} is an old name in RENAMED_COMMANDS, so it is no command now"
            );
        }
    }

    /// Returns the line of src/frozen-1.0.txt as the code names it now. A setting, a window
    /// state key, a --remote command or an action renamed in its table of old names gets its
    /// new name, as does the setting of a value line, and every other line is returned as it is.
    fn frozen_name_now(line: &str) -> String {
        let (kind, name) = line.split_once(' ').unwrap_or((line, ""));
        // A remote command's line may have a word of its choice after the command's own, and a
        // value line has its value after the setting's key.
        let (name, rest) = match kind {
            "remote" | "value" => name.split_once(' ').map_or((name, ""), |(n, r)| (n, r)),
            _ => (name, ""),
        };
        let table: &[(&str, &str)] = match kind {
            "setting" | "value" => &super::RENAMED_SETTINGS,
            "state" => &super::RENAMED_STATE,
            "remote" => &crate::features::remote::RENAMED_COMMANDS,
            "script" => &crate::ui::actions::RENAMED_ACTIONS,
            _ => &[],
        };
        // A built-in theme renamed in RENAMED_THEMES is the value of the theme setting now.
        let rest = match (kind, name) {
            ("value", "theme") => crate::ui::theme::theme_name_now(rest),
            _ => rest,
        };
        match table.iter().find(|r| r.0 == name) {
            Some((_, new)) if rest.is_empty() => format!("{kind} {new}"),
            Some((_, new)) => format!("{kind} {new} {rest}"),
            None if kind == "value" => format!("{kind} {name} {rest}"),
            None => line.to_owned(),
        }
    }

    /// Returns every frozen name as the code holds it now, one line each as in
    /// src/frozen-1.0.txt.
    fn names_now() -> Vec<String> {
        let text = super::Config::default().to_text();
        let mut now: Vec<String> = (text.lines())
            .filter_map(|line| Some(format!("setting {}", line.split_once('=')?.0)))
            .collect();
        for (key, values) in super::NAMED {
            now.extend(values.iter().map(|v| format!("value {key} {v}")));
        }
        now.extend(named_values());
        let window = super::WindowState {
            size: Some(eframe::egui::vec2(1000.0, 700.0)),
            position: Some(eframe::egui::pos2(0.0, 0.0)),
            maximized: true,
        };
        let state = format!("{}=/x\n{}", super::LAST_DIR, window.to_text());
        let keys = state.lines().filter_map(|l| l.split_once('='));
        now.extend(keys.map(|(key, _)| format!("state {key}")));
        now.extend(remote_names());
        now.extend(template_fields());
        // The actions a script may run with --remote run. One taken out of the list would
        // break every script that runs it.
        let script = crate::ui::actions::SCRIPT_ACTIONS.iter();
        now.extend(script.map(|action| format!("script {action:?}")));
        // The marks feature's actions are pinned also in a build without it.
        #[cfg(not(feature = "marks"))]
        now.extend((1..=6).map(|n| format!("script SelectMark{n}")));
        let options = crate::cli::START_OPTIONS
            .iter()
            .flat_map(|o| o.0.split(", "));
        now.extend(options.filter_map(|o| Some(format!("option {}", o.split(' ').next()?))));
        let remote = [
            crate::features::remote::REMOTE,
            crate::features::remote::REMOTE_ALL,
        ];
        now.extend(remote.map(|o| format!("option {o}")));
        now
    }

    /// Returns a "field" line for each `%field%` that src/frozen-1.0.txt pins and that fills in
    /// the templates it belongs to: the full screen info and the print caption, the window
    /// title, or the page header and footer. They are filled for a PNG with every EXIF label
    /// that the default full screen info shows, and with keywords and a comment in its sidecar.
    /// A field renamed where it is read, or an EXIF label relabelled, so stops filling, and the
    /// default full screen info would lose it unseen.
    fn template_fields() -> Vec<String> {
        let pinned: Vec<&str> = (include_str!("../frozen-1.0.txt").lines())
            .filter_map(|l| l.strip_prefix("field "))
            .collect();
        // A folder for each call, as two tests ask for the names at once.
        static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let call = CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("glimt-test-fields-{}-{call}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.png");
        let out = std::fs::File::create(&file).unwrap();
        let mut encoder = image::codecs::png::PngEncoder::new(out);
        image::ImageEncoder::set_exif_metadata(&mut encoder, exif_of_every_label()).unwrap();
        image::RgbImage::new(4, 2)
            .write_with_encoder(encoder)
            .unwrap();
        use crate::metadata::xmp::{KeywordChange, write_changed};
        let keywords = Some(KeywordChange::between(&[], &["Oslo".to_owned()]));
        write_changed(&file, keywords, Some("Trip".into())).unwrap();
        let (mut app, ctx) = crate::support::test_support::app_on(&dir);
        #[cfg(feature = "marks")]
        app.marks.toggle(std::slice::from_ref(&file), 1);
        // Each field on a line of its own as "field:value"; a field left empty drops its line.
        let template: Vec<String> = pinned.iter().map(|f| format!("{f}:%{f}%")).collect();
        app.config.full_screen_info_template = template.join("\\n");
        let mut full = String::new();
        for _ in 0..250 {
            app.poll_background(&ctx);
            full = app.full_screen_info_text(&ctx);
            if ["dimensions:", "ISO:", "keywords:"]
                .iter()
                .all(|f| full.contains(f))
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let caption = crate::features::print::caption(&template.join("|"), &file, 0, 1, None);
        let fills = |text: &str, separator: &str, field: &str| {
            text.split(separator)
                .any(|line| line.starts_with(&format!("{field}:")))
        };
        let untitled = app.title_text();
        let mut in_title = |field: &str| {
            app.config.window_title = format!("%{field}%");
            app.title_text() != untitled
        };
        let filled = pinned.iter().copied().filter(|&field| {
            let (in_full, in_caption) = (fills(&full, "\n", field), fills(&caption, " · ", field));
            match field {
                "page" | "pages" => {
                    let template = format!("%{field}%");
                    crate::features::print::page_text(&template, 2, 3) != template
                }
                "folder" | "app" => in_title(field),
                // The marks are filled in a build with the marks feature.
                "marks" => in_full || !cfg!(feature = "marks"),
                "name" | "path" => in_full && in_caption && in_title(field),
                _ => in_full && in_caption,
            }
        });
        let filled = filled.map(|f| format!("field {f}")).collect();
        std::fs::remove_dir_all(dir).unwrap();
        filled
    }

    /// Returns an EXIF block (TIFF, little-endian) with the date taken, the exposure, the
    /// aperture, the ISO and the focal length: every EXIF label of the default full screen info.
    fn exif_of_every_label() -> Vec<u8> {
        // A directory of (tag, type, count, value or offset) entries, and no next one.
        let directory = |block: &mut Vec<u8>, entries: &[(u16, u16, u32, u32)]| {
            block.extend((entries.len() as u16).to_le_bytes());
            for (tag, kind, count, value) in entries {
                block.extend(tag.to_le_bytes());
                block.extend(kind.to_le_bytes());
                block.extend(count.to_le_bytes());
                block.extend(value.to_le_bytes());
            }
            block.extend(0u32.to_le_bytes());
        };
        let mut block = b"II*\0\x08\0\0\0".to_vec();
        // IFD0 at 8 has one entry, the Exif IFD at 26, which has five; its values start at 92.
        directory(&mut block, &[(0x8769, 4, 1, 26)]);
        let exif = [
            (0x829a, 5, 1, 112),
            (0x829d, 5, 1, 120),
            (0x8827, 3, 1, 400),
            (0x9003, 2, 20, 92),
            (0x920a, 5, 1, 128),
        ];
        directory(&mut block, &exif);
        block.extend(b"2020:01:02 03:04:05\0");
        // The exposure 1/250 s, the aperture f/2.8 and the focal length 50 mm.
        for value in [1u32, 250, 28, 10, 50, 1] {
            block.extend(value.to_le_bytes());
        }
        block
    }

    /// Returns the values of the settings that name something outside `NAMED`. They are
    /// written by name, so their order does not count.
    fn named_values() -> Vec<String> {
        use crate::features::print::{ORIENTATIONS, PAPERS};
        let layouts = crate::viewer::view::LAYOUTS.map(|l| l.0).to_vec();
        let lists: [(&str, Vec<&str>); 9] = [
            ("sort", crate::model::listing::SORTS.map(|s| s.1).to_vec()),
            ("layout", layouts.clone()),
            // This setting is a list of layouts, separated by commas.
            ("slideshow_one_image_at_a_time", layouts),
            (
                "rendering_intent",
                crate::images::colour::INTENTS.map(|i| i.0).to_vec(),
            ),
            ("print_paper", PAPERS.map(|p| p.0).to_vec()),
            ("print_orientation", ORIENTATIONS.map(|o| o.0).to_vec()),
            ("theme", crate::ui::theme::BUILT_IN.map(|t| t.0).to_vec()),
            (
                "language",
                crate::support::lang::LANGUAGES
                    .iter()
                    .map(|l| l.0)
                    .chain([crate::support::lang::ENGLISH])
                    .collect(),
            ),
            (
                "input_profile",
                crate::images::colour::INPUT_PROFILES.map(|p| p.0).to_vec(),
            ),
        ];
        (lists.iter())
            .flat_map(|(key, values)| values.iter().map(move |v| format!("value {key} {v}")))
            .collect()
    }

    /// Returns the --remote commands, the words that each takes from a choice ("on|off"), and
    /// the exit codes. The release tests' "where" is left out, because only their build has it.
    fn remote_names() -> Vec<String> {
        use crate::features::remote::{COMMANDS, NOT_DONE, NOT_SENT, NOT_TAKEN};
        let mut now = Vec::new();
        for (usage, _) in COMMANDS {
            let mut words = usage.split(' ');
            let command = words.next().unwrap_or_default();
            if command == "where" {
                continue;
            }
            now.push(format!("remote {command}"));
            let choice = words.next().filter(|w| w.contains('|')).unwrap_or_default();
            let choices = choice.trim_matches(['[', ']']).split('|');
            now.extend(
                (choices.filter(|w| !w.is_empty())).map(|w| format!("remote {command} {w}")),
            );
        }
        now.push(format!("exit not_sent {NOT_SENT}"));
        now.push(format!("exit not_taken {NOT_TAKEN}"));
        now.push(format!("exit not_done {NOT_DONE}"));
        now
    }

    /// The parser takes every start option that --help lists, and --remote and --remote-all.
    /// An option that the parser leaves in place must be one that is acted on after it: a
    /// --remote command, or a command that `answered` answers. Before, an option renamed in
    /// the parser alone passed, because only the help was pinned. Later, `answered` read the
    /// commands by names of its own, so renaming --check-config there passed too, and -h and
    /// -V could never fail.
    #[test]
    fn the_parser_takes_every_frozen_option() {
        let remote = [
            crate::features::remote::REMOTE,
            crate::features::remote::REMOTE_ALL,
        ];
        let usages = (crate::cli::START_OPTIONS.iter().map(|o| o.0)).chain(remote);
        for usage in usages {
            for option in usage.split(", ") {
                let (name, value) = option.split_once(' ').unwrap_or((option, ""));
                // The value given is the first choice, or the word that --help shows for it.
                let value = value.trim_matches(['[', ']']).split('|').next();
                let args = vec![name.into(), value.unwrap_or_default().into()];
                let (_, rest) = crate::cli::Start::parse(args).expect(name);
                let left = rest.first().is_some_and(|a| a == name);
                let acted_on = remote.contains(&name) || crate::cli::Command::named(name).is_some();
                assert!(!left || acted_on, "{name} is neither taken nor answered");
            }
        }
    }
}
