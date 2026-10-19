//! Glimt's own files: the settings (the config! macro, one line per setting, with their defaults
//! and ranges), the window state and the XDG paths they live in. Also how they are written: one
//! writer thread that the window waits for at exit (`write_later`, `flush`), a lock beside each
//! file for windows writing at once (`file_lock`), files replaced whole (`write_atomic`) and a
//! failed write told once; `--lock-settings`; and the backup of Glimt's folders.

use crate::actions::Keys;
use crate::files::Sort;
use crate::{count, local_time, print};
use eframe::egui;
use std::path::{Path, PathBuf};

// ---- Defaults and ranges of settings ----

/// Default number of images kept decoded on each side of the shown one.
pub(crate) const DEFAULT_PRELOAD: usize = 2;
/// Default seconds between images in a slideshow.
pub(crate) const DEFAULT_DELAY: u64 = 3;
pub(crate) const MAX_DELAY: u64 = 60;
/// The full screen info as Geeqie writes its overlay template: fields in percent signs, `|`
/// between parts of a line (a part whose fields are all empty is left out) and `\n` for a new
/// line. The default shows what Glimt always showed.
pub(crate) const OSD_TEMPLATE: &str = "%name%  %position%\\n%dimensions%|%File size%|%Taken%|%Exposure%|%Aperture%|ISO %ISO%|\
     %Focal length%|%marks%";
/// Preferences → CPU use: (name, hover text). Power saving makes thumbnails one at a time and
/// runs searches, the EXIF index, Find Duplicates and pan view on two cores; Balanced makes
/// two thumbnails at once and runs those on every core; Full makes thumbnails on every core.
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

/// Slideshow → Fade: (name for the config and --fade, menu label, milliseconds).
pub(crate) const FADES: [(&str, &str, u64); 3] = [
    ("off", "Off", 0),
    ("short", "Short", 400),
    ("long", "Long", 1000),
];

/// A memory use level in Preferences. The config holds the values; the level shown is the one
/// they match, or Custom.
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

/// `$XDG_CONFIG_HOME/glimt/config`, otherwise `~/.config/glimt/config`.
pub fn config_path() -> Option<PathBuf> {
    xdg_file("XDG_CONFIG_HOME", ".config", "config")
}

/// `$XDG_CONFIG_HOME/glimt/keywords`: the keyword tree, a path such as "Places|Norway|Oslo"
/// on each line.
pub fn keyword_tree_path() -> Option<PathBuf> {
    xdg_file("XDG_CONFIG_HOME", ".config", "keywords")
}

/// `$XDG_CONFIG_HOME/glimt/sort-folders`: the sort manager's folders, one on each line.
pub fn sort_folders_path() -> Option<PathBuf> {
    xdg_file("XDG_CONFIG_HOME", ".config", "sort-folders")
}

/// The folders of the sort-folders file, as bytes: one that is not UTF-8 is kept.
pub fn sort_folders_from(text: &[u8]) -> Vec<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    (text.split(|&b| b == b'\n'))
        .filter(|l| !l.is_empty())
        .map(|l| PathBuf::from(std::ffi::OsStr::from_bytes(l)))
        .collect()
}

/// The sort-folders file's text for `dirs`; a name with a line break cannot be kept in it.
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

/// `on_disk` with `add` added and `remove` taken out: a change, applied to the file as it is
/// now, so folders another window added meanwhile stay.
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

pub fn xdg_file(var: &str, home_fallback: &str, name: &str) -> Option<PathBuf> {
    Some(xdg_dir(var, home_fallback, "glimt")?.join(name))
}

pub fn xdg_dir(var: &str, home_fallback: &str, app: &str) -> Option<PathBuf> {
    let base = std::env::var_os(var)
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(home_fallback)))?;
    Some(base.join(app))
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

/// Window geometry saved at exit (points): size, position (not on Wayland, where apps cannot
/// see or set it) and maximized.
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

/// The config struct, its defaults, and reading and writing it, from one list; the field
/// name is the key in the file.
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
            /// Set `key` to `value`: None for a key that is no setting, false for a value that
            /// cannot be read, which keeps the one before.
            // String's parse cannot fail, so its `if let` is irrefutable.
            #[allow(irrefutable_let_patterns)]
            pub fn set(&mut self, key: &str, value: &str) -> Option<bool> {
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
                $(text += &format!("{}={}\n", stringify!($name), self.$name);)*
                text
            }
        }
    };
}

config! {
    preload: usize = DEFAULT_PRELOAD,
    /// Seconds between images in a slideshow.
    delay: u64 = DEFAULT_DELAY,
    /// Start in the last used folder when no folder is given on the command line.
    remember_dir: bool = false,
    /// Deleting moves to the trash (default); otherwise the file is deleted permanently.
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
    /// A slideshow starts again from the first image after the last; off, it stops there.
    slideshow_repeat: bool = true,
    /// The slideshow fades from one image to the next: 0 off, 1 short, 2 long (FADES).
    fade: usize = 1,
    /// Several images at once (View → Layout): "1", "2x1", "1x2" or "2x2" (columns × rows).
    layout: String = String::from("1"),
    /// The layouts whose slideshow changes one image at a time, round the panes, separated by
    /// commas; the others change them all together.
    layout_turns: String = String::new(),
    /// Layouts with several panes show each image's file name in its pane.
    layout_names: bool = false,
    /// Animated GIF and WebP play (A); off, the first frame is shown.
    animate: bool = true,
    /// Embedded ICC profiles are converted to sRGB, or to the screen profile.
    color_management: bool = true,
    /// The screen's ICC profile file, chosen in Preferences; empty: the screen is taken to be sRGB.
    screen_profile: String = String::new(),
    /// The desktop's screen profile, from colord, goes before `screen_profile` when there is one
    /// (Geeqie's "Use system screen profile if available").
    desktop_profile: bool = true,
    /// The profile of images without one: srgb, adobe, p3 or an ICC file.
    input_profile: String = "srgb".to_owned(),
    /// An image's own ICC profile (or Adobe RGB from its EXIF) counts; off, the input profile
    /// is used for every image.
    own_profile: bool = true,
    /// How colours outside the target are brought in: perceptual, relative, saturation or
    /// absolute. Only profiles with tables for it (LUT profiles) tell them apart.
    rendering_intent: String = "perceptual".to_owned(),
    /// The interface's language: a translation's code (no_nb) or "en"; empty, the system's.
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
    /// Off at every start: a large tree is read whole before its folders show.
    show_subfolders: bool = false,
    /// A slideshow also shows the images in the subfolders.
    slideshow_subfolders: bool = false,
    /// Write the view's rotation into the JPEG when going to another image.
    save_orientation: bool = false,
    /// Quality (0–100) when a lossy WebP is saved again after turning it.
    webp_quality: u8 = 90,
    /// Keep zoom and position when going to another image, instead of fitting it.
    keep_zoom: bool = false,
    /// How an image that is not zoomed fills the view: 0 whole, 1 the width, 2 the height,
    /// 3 the whole view (cut at the sides). FITS.
    fit_mode: u8 = 0,
    /// The mouse wheel zooms; off, it goes to the next or previous image as in Geeqie.
    wheel_zoom: bool = true,
    /// Thumbnails in the file list (T).
    thumbnails: bool = true,
    /// Write new thumbnails to the shared cache (~/.cache/thumbnails), as file managers do.
    save_thumbnails: bool = true,
    /// EXIF panel on the right (Ctrl+K); off by default, as reading EXIF costs a little per image.
    exif_panel: bool = false,
    /// Sort manager on the right (Shift+S): folders to copy or move the selected files to.
    sort_manager: bool = false,
    /// The sort manager moves the files; off, it copies them.
    sort_move: bool = false,
    /// Folders as a tree from / (Ctrl+T) instead of the subfolders of the current one.
    folder_tree: bool = false,
    /// Toolbar under the menu, as in Geeqie.
    toolbar: bool = true,
    /// One malloc arena for all threads: much less memory, decoding up to ~8 % slower.
    /// Read at startup only, since glibc must be set before threads start.
    low_memory: bool = true,
    /// Images decoded at once, at most the number of cores; read at startup.
    decode_threads: usize = 4,
    /// CPU use: 0 power saving, 1 balanced, 2 full (CPU_LEVELS).
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
    // Import Lightroom Catalog: each image's keywords into its XMP sidecar.
    lightroom_keywords: bool = true,
    /// Keep the screen saver off while in full screen.
    inhibit_screensaver: bool = true,
    /// Window title "Glimt - image name" instead of "image name - Glimt".
    app_name_first: bool = false,
    /// File list with size and date columns, as in Geeqie.
    file_details: bool = false,
    sort: Sort = Sort::Name,
    sort_ascending: bool = true,
    sort_case: bool = true,
    /// Toolbar icon size: 0 small, 1 medium, 2 large.
    toolbar_icons: usize = 0,
    /// View → Filter: an index into colour::FILTERS (greyscale, inverted, more contrast).
    view_filter: usize = 0,
    /// File name search above the file list.
    search_bar: bool = true,
    /// Name, position and a few EXIF fields over the image in full screen (I), as Geeqie's
    /// image overlay.
    osd: bool = true,
    /// What the full screen info shows (OSD_TEMPLATE).
    osd_template: String = OSD_TEMPLATE.to_owned(),
    /// Histogram in the corner of the image (K): 0 off, 1 RGB, 2 brightness.
    histogram: u8 = 0,
    /// Logarithmic histogram (J), which shows the small counts better.
    histogram_log: bool = false,
    /// Thumbnail size: 0 small (96×72), 1 medium (128×96), 2 large (192×144, from the
    /// freedesktop "large" cache).
    thumb_size: u8 = 0,
    /// The file list as a grid of thumbnails (Ctrl+I) instead of rows.
    icons: bool = false,
    /// Colour behind the image (sRGB).
    background: Rgb = Rgb([0, 0, 0]),
    /// Window → Projector: a ring on the projector where the pointer is over the image.
    projector_pointer: bool = true,
    /// The theme's name: built in ("Dark", the default for an image viewer; "Desktop" follows
    /// the desktop's light or dark style), or a file in `$XDG_CONFIG_HOME/glimt/themes`.
    theme: String = String::from("Dark"),
    /// The search field finds names fuzzily (like fzf); off, by regular expression, as Geeqie's
    /// file filter.
    fuzzy_search: bool = true,
    /// Without fuzzy search: the regular expression tells upper and lower case apart.
    search_case: bool = false,
    /// Re-read the folder when files are added, removed or renamed.
    auto_refresh: bool = true,
    /// Rotate images by their EXIF orientation (Geeqie: "Auto rotate image using Exif information").
    exif_rotate: bool = true,
    /// List images (Types menu in the status bar).
    list_images: bool = true,
    /// List camera RAW files.
    list_raw: bool = true,
    /// List files and folders whose names start with a dot.
    show_hidden: bool = false,
    /// Video feature: list video files.
    videos: bool = true,
    /// Video feature: command line that plays a video; empty = the default application.
    video_player: String = String::new(),
    /// Print dialog: the printer chosen last (empty: a PDF file), the paper and file names.
    print_printer: String = String::new(),
    print_paper: String = String::from("A4"),
    /// Which way the paper is turned: "auto" (a wide image alone on its page goes landscape),
    /// "portrait" or "landscape".
    print_orientation: String = String::from("auto"),
    print_caption: bool = true,
    /// The text under each printed image, as the full screen info's template (%name%, EXIF
    /// fields, %keywords% …).
    print_template: String = String::from("%name%"),
    /// Images on each page: 1, or a contact sheet of 2 to 20 (print::PER_PAGE).
    print_per_page: usize = 1,
    /// Around the printed page, in millimetres (0 to 50).
    print_margin: u32 = 10,
    /// One image a page: its size in per cent of the room it has (10 to 100).
    print_scale: u32 = 100,
    /// Each image fills its box, cut to it, instead of fitting in it.
    print_fill: bool = false,
    /// Text at the top and the bottom of every printed page; %page% and %pages% number it.
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
        c.delay = c.delay.clamp(1, MAX_DELAY);
        c.toolbar_icons = c.toolbar_icons.min(2);
        c.view_filter = c.view_filter.min(3);
        c
    }

    /// The settings in `text`, and what is wrong in it, line by line: a line that is not
    /// key=value, a setting that does not exist or is set twice, a value that cannot be read or
    /// is out of range. Empty lines and lines starting with # are left alone.
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
                // Set again later: that one is checked.
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

    /// The settings, and what is wrong in their file: its lines, or that it cannot be read.
    pub fn load_checked() -> (Self, Vec<String>) {
        let read = config_path().map(std::fs::read);
        let (config, problems) = match read {
            // A byte that is not UTF-8 (a path in another encoding) does not cost the rest.
            Some(Ok(bytes)) => Self::check(&String::from_utf8_lossy(&bytes)),
            Some(Err(e)) if e.kind() != std::io::ErrorKind::NotFound => (
                Config::default(),
                vec![format!("cannot be read ({e}); the defaults are used")],
            ),
            _ => (Config::default(), Vec::new()),
        };
        *SAVED.lock().unwrap_or_else(|e| e.into_inner()) = config.to_text();
        (config, problems)
    }

    /// Only the settings this window changed go over the file; the others keep what other
    /// windows wrote meanwhile. Locked settings are never written.
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
            // Read, merged and written in the background, as each toggle in a menu saves: the
            // file's sync must not hold the window.
            write_later(move || {
                let _held = file_lock(&p);
                // One that is there but cannot be read is not written over: every setting the
                // user had would be the defaults then.
                let on_disk = match std::fs::read_to_string(&p) {
                    Ok(text) => text,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
                    Err(e) => {
                        return save_failed(format!(
                            "Settings not saved: {} cannot be read: {e}",
                            p.display()
                        ));
                    }
                };
                write_atomic(&p, merged(&ours, &before, &on_disk));
            });
        }
    }
}

/// The value of `key` in key=value lines, as `to_text` writes them.
fn value_in(text: &str, key: &str) -> String {
    let line = text
        .lines()
        .find_map(|l| l.strip_prefix(key)?.strip_prefix('='));
    line.unwrap_or_default().to_owned()
}

/// A file Glimt could not save, for the window to tell: from any thread.
static SAVE_ERROR: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// The last file Glimt could not save, once.
pub fn take_save_error() -> Option<String> {
    SAVE_ERROR.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// The administrator's lock: while this file is there, every Glimt starts with --lock-settings.
pub const LOCK_FILE: &str = "/etc/glimt/lock-settings";

/// --lock-settings: no Preferences, and nothing written to the settings files.
static LOCKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn lock() {
    LOCKED.store(true, std::sync::atomic::Ordering::Relaxed);
}

pub fn locked() -> bool {
    LOCKED.load(std::sync::atomic::Ordering::Relaxed)
}

/// The settings as this window last read or wrote them.
static SAVED: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

/// Writes of Glimt's own files waiting in their thread.
static WRITES_WAITING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// `work` in the one thread that writes Glimt's own files, in the order given.
pub fn write_later(work: impl FnOnce() + Send + 'static) {
    use std::sync::atomic::Ordering::Relaxed;
    static WRITER: std::sync::OnceLock<std::sync::mpsc::Sender<Work>> = std::sync::OnceLock::new();
    WRITES_WAITING.fetch_add(1, Relaxed);
    let counted = move || {
        // Counted down also when it panics: Glimt would wait for it at exit.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work));
        WRITES_WAITING.fetch_sub(1, Relaxed);
    };
    if WRITER.get_or_init(serial).send(Box::new(counted)).is_err() {
        WRITES_WAITING.fetch_sub(1, Relaxed);
    }
}

/// Work for a `serial` thread.
pub type Work = Box<dyn FnOnce() + Send>;

/// A thread that does the work sent to it one after another, in that order. One that panics
/// does not end it, or every later work sent would be lost.
pub fn serial() -> std::sync::mpsc::Sender<Work> {
    let (works, rx) = std::sync::mpsc::channel::<Work>();
    std::thread::spawn(move || {
        for work in rx {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work));
        }
    });
    works
}

/// Wait for the writes asked for, a few seconds at most, before Glimt ends: the last change of
/// a setting must not be lost with the thread.
pub fn flush() {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while WRITES_WAITING.load(std::sync::atomic::Ordering::Relaxed) > 0
        && std::time::Instant::now() < until
    {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// The lock beside `path`, held while it lives: another Glimt window's reading and writing of
/// the same file does not fall between ours, which would lose its change. The lock is a file of
/// its own, hidden beside it, as `path` is renamed over.
pub fn file_lock(path: &Path) -> Option<std::fs::File> {
    let name = path.file_name()?.to_string_lossy();
    // Never truncated: a lock file that is a link planted in a shared folder must not empty
    // the file it points to.
    let lock = (std::fs::OpenOptions::new().create(true).append(true))
        .open(path.with_file_name(format!(".{name}.lock")))
        .ok()?;
    lock.lock().ok()?;
    Some(lock)
}

/// `ours` (key=value lines), but where a value is as this window `saved` it, the one
/// `on_disk`: another window may have changed that setting since. In the file's own order, with
/// its comments and blank lines, and the settings it lacks after them; settings this Glimt does
/// not know, from a newer one (a Flatpak beside a .deb), stay.
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

/// Write to a temporary file and rename it, so the file is never half-written after a crash.
/// This Glimt's name is in it (in a Flatpak more than the process ID, which windows can share),
/// so concurrent sessions never write to the same temporary file.
pub fn replace_file(path: &Path, bytes: impl AsRef<[u8]>) -> std::io::Result<()> {
    use std::io::Write;
    // Through a link (dotfiles kept in a repository), the file it points to: the link stays.
    let path = &std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    let tmp = tmp_path(path);
    // On disk before the rename: after a crash the old file or the new one, never an empty one.
    let result = std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))
        .and_then(|_| new_beside(&tmp, path))
        .and_then(|mut f| f.write_all(bytes.as_ref()).and_then(|_| f.sync_all()))
        .and_then(|_| std::fs::rename(&tmp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Where a new version of `path` is written before it is renamed over it: its whole name, this
/// Glimt's instance ID and ".glimt-tmp", so that "a.png" and "a.webp", saved at the same time,
/// or one file saved by two windows at once, never share one.
pub fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_owned();
    name.push(format!(".{}.glimt-tmp", crate::remote::instance_id()));
    path.with_file_name(name)
}

/// The temporary file `tmp` for writing `path` again, with the permissions of `path` (a sidecar
/// or a list kept private, 0600, stays so). Made private first when `path` is there, and given
/// those before a byte is written: no one else can open it meanwhile. A new file as usual.
pub fn new_beside(tmp: &Path, path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    // Made new, never opened: a link left or planted at `tmp` would be followed and the file
    // it points to emptied.
    let _ = std::fs::remove_file(tmp);
    let new = || std::fs::OpenOptions::new().write(true).create_new(true).clone();
    let Ok(old) = std::fs::metadata(path) else {
        return new().open(tmp);
    };
    let file = new().mode(0o600).open(tmp)?;
    file.set_permissions(old.permissions())?;
    Ok(file)
}

/// `replace_file`, with a failure on stderr: for files Glimt keeps for itself.
pub fn write_atomic(path: &Path, bytes: impl AsRef<[u8]>) {
    if let Err(e) = replace_file(path, bytes) {
        save_failed(format!("Could not save {}: {e}", path.display()));
    }
}

/// A file that could not be saved, told on the terminal and, once, in the window.
pub fn save_failed(error: String) {
    eprintln!("{error}");
    *SAVE_ERROR.lock().unwrap_or_else(|e| e.into_inner()) = Some(error);
}

/// Glimt's own folders, to back up, with their names in a backup: its settings, keyword tree,
/// sort folders and themes (config), marks and stars (state), and collections, plugins and
/// decoders (data). Thumbnails are not: they are made again.
pub fn own_folders() -> Vec<(&'static str, PathBuf)> {
    [
        ("config", xdg_dir("XDG_CONFIG_HOME", ".config", "glimt")),
        ("state", xdg_dir("XDG_STATE_HOME", ".local/state", "glimt")),
        ("data", xdg_dir("XDG_DATA_HOME", ".local/share", "glimt")),
    ]
    .into_iter()
    .filter_map(|(name, dir)| Some((name, dir?)))
    .filter(|(_, dir)| dir.is_dir())
    .collect()
}

/// Copy the folder `from` with everything in it to `to`, made; how many files. Links are
/// copied as the files they point to; one that leads nowhere is left out.
pub fn copy_tree(from: &Path, to: &Path) -> std::io::Result<usize> {
    std::fs::create_dir_all(to)?;
    let mut files = 0;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let (source, target) = (entry.path(), to.join(entry.file_name()));
        if source.is_dir() {
            files += copy_tree(&source, &target)?;
        } else if source.is_file() {
            // Named, as the files after it are not copied.
            std::fs::copy(&source, &target)
                .map_err(|e| std::io::Error::new(e.kind(), format!("{}: {e}", source.display())))?;
            files += 1;
        }
    }
    Ok(files)
}

/// Glimt's own folders copied to a new folder glimt-backup-DATE in `dir`: what was done, or
/// what went wrong.
pub(crate) fn back_up_to(dir: &Path) -> Result<String, String> {
    // A mistyped or unmounted folder is not made: the backup would land where no one looks.
    if !dir.is_dir() {
        return Err(format!("{} is not a folder", dir.display()));
    }
    let own = own_folders();
    if own.is_empty() {
        return Err("Nothing to back up: Glimt has no settings, marks or collections here".into());
    }
    // Inside one of the folders it copies, the backup would copy itself until paths are too long.
    let real = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_owned());
    if let Some((_, inside)) = own.iter().find(|(_, d)| real(dir).starts_with(real(d))) {
        return Err(format!(
            "A backup cannot go inside {}, which it copies",
            inside.display()
        ));
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
    let date: String = local_time(now.map_or(0, |d| d.as_secs() as i64))
        .chars()
        .take(10)
        .collect();
    // A second backup the same day goes beside the first, never over it.
    let to = (1..)
        .map(|n| match n {
            1 => dir.join(format!("glimt-backup-{date}")),
            n => dir.join(format!("glimt-backup-{date}-{n}")),
        })
        .find(|p| !p.exists())
        .unwrap_or_default();
    let copied: std::io::Result<usize> = (own.into_iter())
        .map(|(name, from)| copy_tree(&from, &to.join(name)))
        .sum();
    // Not the plan of a Rename Many running now: restored later, it would rename files again.
    let _ = std::fs::remove_dir_all(to.join("state/renames"));
    match copied {
        Ok(n) => Ok(format!(
            "Glimt's data backed up to {}: {}",
            to.display(),
            count(n, "file")
        )),
        Err(e) => Err(format!("The backup to {} is not whole: {e}", to.display())),
    }
}

#[cfg(test)]
mod tests {
    /// A write that panics leaves the writer running: the next one is done, and flush waits.
    #[test]
    fn writer_survives_a_panic() {
        use std::sync::atomic::{AtomicBool, Ordering};
        static DONE: AtomicBool = AtomicBool::new(false);
        super::write_later(|| panic!("a broken write"));
        super::write_later(|| DONE.store(true, Ordering::Relaxed));
        super::flush();
        assert!(DONE.load(Ordering::Relaxed));
    }

    /// The file Glimt writes has nothing wrong; each kind of mistake is told with its line.
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

    /// A backup copy: every file of the tree, at the same place below the new folder.
    #[test]
    fn copies_a_tree() {
        let dir = std::env::temp_dir().join(format!("glimt-test-tree-{}", std::process::id()));
        let from = dir.join("from");
        std::fs::create_dir_all(from.join("collections/sub")).unwrap();
        std::fs::write(from.join("config"), "theme=Dark").unwrap();
        std::fs::write(from.join("collections/a.gqv"), "#GQview").unwrap();
        std::fs::write(from.join("collections/sub/b.gqv"), "").unwrap();
        assert_eq!(super::copy_tree(&from, &dir.join("to")).unwrap(), 3);
        let text = |p: &str| std::fs::read_to_string(dir.join("to").join(p)).unwrap();
        assert_eq!(
            (text("config"), text("collections/a.gqv")),
            ("theme=Dark".into(), "#GQview".into())
        );
        assert!(dir.join("to/collections/sub/b.gqv").is_file());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A file reached through a link is written where the link points; the link stays.
    #[test]
    fn writes_through_links() {
        let dir = std::env::temp_dir().join(format!("glimt-test-link-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (real, link) = (dir.join("real"), dir.join("config"));
        std::fs::write(&real, "old").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        super::replace_file(&link, "new").unwrap();
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "new");
        // A file kept private stays private when written again.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
        super::replace_file(&real, "newer").unwrap();
        let mode = std::fs::metadata(&real).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        std::fs::remove_dir_all(dir).unwrap();
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

    /// This window changed the sort order, another turned thumbnails off: both stay.
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
    fn writes_atomically() {
        let dir = std::env::temp_dir().join(format!("glimt-test-atomic-{}", std::process::id()));
        let path = dir.join("config");
        super::write_atomic(&path, "a=1\n");
        super::write_atomic(&path, "a=2\n");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a=2\n");
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            1,
            "no .tmp files left"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Links planted where the lock and the temporary file go empty nothing they point to.
    #[test]
    fn planted_links_empty_nothing() {
        use std::os::unix::fs::symlink;
        let dir = std::env::temp_dir().join(format!("glimt-test-planted-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (victim, photo) = (dir.join("victim"), dir.join("trip.gqv"));
        std::fs::write(&victim, "keep").unwrap();
        std::fs::write(&photo, "old").unwrap();
        symlink(&victim, dir.join(".trip.gqv.lock")).unwrap();
        drop(super::file_lock(&photo));
        let tmp = super::tmp_path(&photo);
        symlink(&victim, &tmp).unwrap();
        drop(super::new_beside(&tmp, &photo).unwrap());
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn parses_config() {
        use crate::Config;
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
            (crate::files::Sort::ExifDigitized, false, false)
        );
        assert_eq!(
            Config::parse("sort=nonsense").sort,
            crate::files::Sort::Name
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
        assert_eq!(crate::WindowState::parse(&text), win);
        assert_eq!(crate::WindowState::parse("window_size=5,5\n").size, None);
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
    }

    /// Config and window state files broken in thousands of ways: read without a panic.
    #[test]
    fn survives_broken_files() {
        let seed = super::Config::default().to_text();
        for bytes in crate::broken(seed.as_bytes()) {
            super::Config::parse(&String::from_utf8_lossy(&bytes));
        }
        for bytes in crate::broken(b"maximized=true\nsize=1000x700.5\npos=10,-20\n") {
            super::WindowState::parse(&String::from_utf8_lossy(&bytes));
        }
    }
}
