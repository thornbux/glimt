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
pub(crate) const DEFAULT_DELAY: u64 = 3;

pub(crate) const MAX_DELAY: u64 = 60;

/// The full screen info, as Geeqie writes its overlay template. Fields are in percent signs,
/// `|` stands between the parts of a line, and `\n` starts a new line. A part whose fields are
/// all empty is left out. The default shows what Glimt always showed.
pub(crate) const OSD_TEMPLATE: &str = "%name%  %position%\\n%dimensions%|%File size%|%Taken%|%Exposure%|%Aperture%|ISO %ISO%|\
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
        hover: "Least memory: one image decoded at a time, one preloaded on each side. Slow \
                storage (SMB, NFS, USB) makes browsing wait for each file.",
    },
    MemoryLevel {
        name: "Medium",
        preload: DEFAULT_PRELOAD,
        decode_threads: 4,
        animation_mb: 128,
        one_arena: true,
        hover: "The default: four images decoded at once, two preloaded on each side.",
    },
    MemoryLevel {
        name: "High",
        preload: 5,
        decode_threads: 8,
        animation_mb: 256,
        one_arena: false,
        hover: "Fastest browsing: eight images decoded at once, five preloaded on each side; \
                hides the wait of slow storage (SMB, NFS) best.",
    },
];

/// How many Open Recent keeps.
pub const RECENT: usize = 10;

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

/// The administrator's lock. While this file is there, every Glimt starts with --lock-settings.
pub const LOCK_FILE: &str = "/etc/glimt/lock-settings";

/// Whether --lock-settings is on. Then there are no Preferences, and nothing is written to the
/// settings files.
static LOCKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The settings as this window last read or wrote them.
static SAVED: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

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
    pub(crate) hover: &'static str,
}

/// The window geometry saved at exit, in points. It holds the size, the position and whether
/// the window is maximized. The position is not kept on Wayland, where apps cannot see or set
/// it.
#[derive(Debug, Default, PartialEq)]
pub struct WindowState {
    pub size: Option<egui::Vec2>,
    pub pos: Option<egui::Pos2>,
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
            pos: pair("window_pos").map(|(x, y)| egui::pos2(x, y)),
            maximized: state_value(state, "maximized") == Some("true"),
        }
    }

    pub fn to_text(&self) -> String {
        let mut text = format!("maximized={}\n", self.maximized);
        if let Some(s) = self.size {
            text += &format!("window_size={},{}\n", s.x, s.y);
        }
        if let Some(p) = self.pos {
            text += &format!("window_pos={},{}\n", p.x, p.y);
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
    delay: u64 = DEFAULT_DELAY,
    /// Start in the last used folder when no folder is given on the command line.
    remember_dir: bool = false,
    /// Keep File → Open Recent's list. When this is off, no list is written and the one kept
    /// is deleted.
    keep_recent: bool = true,
    /// Deleting moves files to the trash (the default). Otherwise a file is deleted for good.
    trash: bool = true,
    /// Ask before deleting permanently.
    confirm_delete: bool = true,
    /// Ask before moving to the trash.
    confirm_trash: bool = true,
    /// The Delete key deletes (Ctrl+D always does).
    delete_key: bool = true,
    /// Slideshow in random order.
    random: bool = false,
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
    layout_turns: String = String::new(),
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
    own_profile: bool = true,
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
    /// The EXIF panel on the right (Ctrl+K). It is off by default, because reading EXIF costs a
    /// little for each image.
    exif_panel: bool = false,
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
    /// Window title "Glimt - image name" instead of "image name - Glimt".
    app_name_first: bool = false,
    /// The window title, from a template of %name%, %path%, %folder% and %app%. When it is
    /// empty, the title has the order above.
    window_title: String = String::new(),
    /// File list with size and date columns, as in Geeqie.
    file_details: bool = false,
    sort: Sort = Sort::Name,
    sort_ascending: bool = true,
    sort_case: bool = true,
    /// The toolbar's icon size, small, medium or large, as an index.
    toolbar_icons: usize = 0,
    /// View → Filter: none, greyscale, inverted or contrast, as an index into colour::FILTERS.
    view_filter: usize = 0,
    /// File name search above the file list.
    search_bar: bool = true,
    /// Name, position and a few EXIF fields over the image in full screen (I), as Geeqie's
    /// image overlay.
    osd: bool = true,
    /// What the full screen info shows (OSD_TEMPLATE).
    osd_template: String = OSD_TEMPLATE.to_owned(),
    /// The histogram in the corner of the image (K): off, rgb or brightness, as an index.
    histogram: u8 = 0,
    /// Logarithmic histogram (J), which shows the small counts better.
    histogram_log: bool = false,
    /// The thumbnail size: small (96×72), medium (128×96) or large (192×144, from the
    /// freedesktop "large" cache), as an index.
    thumb_size: u8 = 0,
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
    search_case: bool = false,
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
    videos: bool = true,
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
            c.set(key.trim(), value.trim());
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
        c.delay = c.delay.clamp(1, MAX_DELAY);
        c.toolbar_icons = c.toolbar_icons.min(2);
        c.view_filter = c.view_filter.min(3);
        c
    }

    /// The settings in `text`, and what is wrong in it, line by line. A problem is a line that
    /// is not key=value, a setting that does not exist or is set twice, or a value that cannot
    /// be read or is out of range. Empty lines and lines starting with # are left alone.
    pub fn check(text: &str) -> (Self, Vec<String>) {
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
            let has_key =
                |&(_, l): &(usize, &str)| l.split_once('=').map(|(o, _)| o.trim()) == Some(key);
            let again = lines[..k].iter().any(has_key);
            let later = lines[k + 1..].iter().any(has_key);
            let mut alone = Config::default();
            match alone.set(key, value) {
                None => problems.push(format!("line {n}: there is no setting {key}")),
                Some(false) => problems.push(format!(
                    "line {n}: {key}={value} is not a value it takes; {} is used",
                    used(key)
                )),
                Some(true) if again => {
                    problems.push(format!("line {n}: {key} is set again; the last is used"));
                }
                // When the setting is set again later, that later line is the one checked.
                Some(true) if later => {}
                Some(true) => {
                    let wanted = value_in(&alone.to_text(), key);
                    if wanted != used(key) {
                        problems.push(format!(
                            "line {n}: {key}={value} is out of range; {} is used",
                            used(key)
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
            Some(Ok(text)) => Self::check(&text),
            Some(Err(e)) if e.kind() != std::io::ErrorKind::NotFound => (
                Config::default(),
                vec![format!("cannot be read ({e}); the defaults are used")],
            ),
            _ => (Config::default(), Vec::new()),
        };
        *SAVED.lock().unwrap_or_else(|e| e.into_inner()) = config.to_text();
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
                &mut *SAVED.lock().unwrap_or_else(|e| e.into_inner()),
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
                    save_failed(format!("Could not save {}: {e}", p.display()));
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
        let bytes = d.as_os_str().as_encoded_bytes();
        if !bytes.contains(&b'\n') {
            text.extend(bytes);
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

/// The paths of a recent file's text, read as bytes, because a name need not be UTF-8.
pub fn parse_recent(text: &[u8]) -> Vec<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    (text.split(|&b| b == b'\n'))
        .filter(|l| !l.is_empty())
        .map(|l| std::ffi::OsStr::from_bytes(l).into())
        .take(RECENT)
        .collect()
}

/// `recent` with `path` first, once, and no more than RECENT paths. A name with a line break in
/// it cannot be kept on a line, so it is left out.
pub fn add_recent(recent: &[PathBuf], path: &Path) -> Vec<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let fits = !path.as_os_str().as_bytes().contains(&b'\n');
    (fits.then(|| path.to_path_buf()).into_iter())
        .chain(recent.iter().filter(|p| *p != path).cloned())
        .take(RECENT)
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
    let flatpak = std::env::var_os("FLATPAK_ID").is_some();
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
/// plugins and decoders. Returns `None` when neither gives an absolute folder.
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

pub fn parse_last_dir(state: &str) -> Option<PathBuf> {
    state_value(state, "last_dir").map(PathBuf::from)
}

pub fn state_value<'a>(state: &'a str, key: &str) -> Option<&'a str> {
    state.lines().find_map(|l| {
        l.strip_prefix(key)?
            .strip_prefix('=')
            .filter(|v| !v.is_empty())
    })
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
    LOCKED.store(true, std::sync::atomic::Ordering::Relaxed);
}

pub fn locked() -> bool {
    LOCKED.load(std::sync::atomic::Ordering::Relaxed)
}

/// `ours` (key=value lines), except that where a value is as this window `saved` it, the one
/// `on_disk` is kept, because another window may have changed that setting since. The result
/// keeps the file's own order, its comments and blank lines, with the settings it lacks after
/// them. Settings this Glimt does not know, from a newer one (a Flatpak beside a .deb), stay.
fn merged(ours: &str, saved: &str, on_disk: &str) -> String {
    let pairs = |text: &str| -> std::collections::HashMap<String, String> {
        (text.lines().filter_map(|l| l.split_once('=')))
            .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
            .collect()
    };
    let (saved, theirs) = (pairs(saved), pairs(on_disk));
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
        let (_, none) = super::Config::check(&super::Config::default().to_text());
        assert_eq!(none, Vec::<String>::new());
        let text = "# note\nthumbnails=ja\nthumbnail=true\nui_scale=500\ndelay=4\ndelay=5\nwords\n\nfit_mode=2\n";
        let (config, problems) = super::Config::check(text);
        assert_eq!(
            problems,
            [
                "line 2: thumbnails=ja is not a value it takes; true is used",
                "line 3: there is no setting thumbnail",
                "line 4: ui_scale=500 is out of range; 300 is used",
                "line 6: delay is set again; the last is used",
                "line 7: “words” is not setting=value",
            ]
        );
        assert_eq!(
            (config.delay, config.fit_mode, config.ui_scale),
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
        assert!(Config::parse("app_name_first=true").app_name_first);
        let win = super::WindowState {
            size: Some(eframe::egui::vec2(1000.0, 700.5)),
            pos: Some(eframe::egui::pos2(10.0, -20.0)),
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
                .any(|e| e.file_name().to_string_lossy().ends_with(".glimt-tmp"))
        );
        // A name near the limit of 255 bytes is written again, and its sidecar too, because the
        // temporary name made from it was too long.
        let long = dir.join(format!("{}.jpg", "a".repeat(240)));
        crate::files::file_io::create_file(&long, "x").unwrap();
        crate::files::file_io::replace_file(&long, "y").unwrap();
        crate::files::file_io::replace_file(&crate::metadata::xmp::sidecar(&long), "<x/>").unwrap();
        assert_eq!(std::fs::read_to_string(&long).unwrap(), "y");
        let file = dir.join("config");
        std::fs::write(&file, b"delay=5\nvideo_player=mpv \xff\n").unwrap();
        let text = super::read_lossy(&file).unwrap();
        assert_eq!(Config::parse(&text).delay, 5);
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
        let (_, problems) = Config::check("fade=9\nfit_mode=wide");
        assert!(problems[0].contains("long is used") && problems[1].contains("not a value"));
        for (k, (name, _, _)) in super::FADES.iter().enumerate() {
            assert_eq!(super::NAMED[0].1[k], *name);
        }
    }

    /// Open Recent has the newest first, each once and at most RECENT, and it comes back from
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
        assert_eq!(recent.len(), super::RECENT);
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
}
