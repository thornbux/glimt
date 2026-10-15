//! Preferences: the config file (the config! macro: one line per setting), the window state, and
//! the XDG paths they live in.

use crate::actions::Keys;
use crate::files::Sort;
use crate::{CPU_LEVELS, DEFAULT_AHEAD, DEFAULT_DELAY, FADES, MAX_DELAY, OSD_TEMPLATE, print};
use eframe::egui;
use std::path::{Path, PathBuf};

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
            // String's parse cannot fail, so its `if let` is irrefutable.
            #[allow(irrefutable_let_patterns)]
            pub fn set(&mut self, key: &str, value: &str) {
                match key {
                    // An invalid value keeps the default.
                    $(stringify!($name) => {
                        if let Ok(v) = value.parse() {
                            self.$name = v;
                        }
                    })*
                    _ => {}
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
    preload: usize = DEFAULT_AHEAD,
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
    /// Marks and stars shown in the file list and overlay (marks feature).
    show_marks: bool = true,
    /// The user's own keys, replacing the defaults of those actions (Preferences).
    keys: Keys = Keys::default(),
    /// Listen for `glimt --remote` commands.
    remote_control: bool = true,
    /// Stars are also written as xmp:Rating to `<file>.xmp` sidecars, for darktable and digiKam.
    xmp_stars: bool = false,
    /// The file list holds the images of all subfolders too, with their folders in the name.
    show_subfolders: bool = false,
    /// A slideshow also shows the images in the subfolders.
    slideshow_subfolders: bool = false,
    /// Write the view's rotation into the JPEG when going to another image.
    save_orientation: bool = false,
    /// Quality (0–100) when a lossy WebP is saved again after turning it.
    webp_quality: u8 = 90,
    /// Keep zoom and position when going to another image, instead of fitting it.
    keep_zoom: bool = false,
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
    print_caption: bool = true,
    /// Images on each page: 1, or a contact sheet of 2 to 20 (print::PER_PAGE).
    print_per_page: usize = 1,
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
        if !print::PER_PAGE.iter().any(|p| p.0 == c.print_per_page) {
            c.print_per_page = 1;
        }
        c.animation_mb = c.animation_mb.clamp(8, 1024);
        c.histogram = c.histogram.min(2);
        c.thumb_size = c.thumb_size.min(2);
        c.webp_quality = c.webp_quality.min(100);
        c.delay = c.delay.clamp(1, MAX_DELAY);
        c.toolbar_icons = c.toolbar_icons.min(2);
        c
    }

    pub fn load() -> Self {
        let config = config_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map_or_else(Config::default, |text| Config::parse(&text));
        *SAVED.lock().unwrap_or_else(|e| e.into_inner()) = config.to_text();
        config
    }

    /// Only the settings this window changed go over the file; the others keep what other
    /// windows wrote meanwhile.
    pub fn save(&self) {
        if let Some(p) = config_path() {
            let ours = self.to_text();
            let mut saved = SAVED.lock().unwrap_or_else(|e| e.into_inner());
            let on_disk = std::fs::read_to_string(&p).unwrap_or_default();
            write_atomic(&p, &merged(&ours, &saved, &on_disk));
            *saved = ours;
        }
    }
}

/// The settings as this window last read or wrote them.
static SAVED: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

/// `ours` (key=value lines), but where a value is as this window `saved` it, the one
/// `on_disk`: another window may have changed that setting since.
fn merged(ours: &str, saved: &str, on_disk: &str) -> String {
    let pairs = |text: &str| -> std::collections::HashMap<String, String> {
        (text.lines().filter_map(|l| l.split_once('=')))
            .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
            .collect()
    };
    let (saved, theirs) = (pairs(saved), pairs(on_disk));
    let mut text = String::new();
    for line in ours.lines() {
        let theirs = line
            .split_once('=')
            .filter(|(k, v)| saved.get(*k).is_some_and(|s| s == v))
            .and_then(|(k, _)| Some((k, theirs.get(k)?)));
        match theirs {
            Some((k, v)) => text += &format!("{k}={v}\n"),
            None => text += &format!("{line}\n"),
        }
    }
    // Settings this Glimt does not know, from a newer one (a Flatpak beside a .deb), stay.
    let known: std::collections::HashSet<&str> = (ours
        .lines()
        .filter_map(|l| Some(l.split_once('=')?.0.trim())))
    .collect();
    for line in on_disk.lines() {
        if line
            .split_once('=')
            .is_some_and(|(k, _)| !known.contains(k.trim()))
        {
            text += &format!("{line}\n");
        }
    }
    text
}

/// Write to a temporary file and rename it, so the file is never half-written after a crash.
/// This Glimt's name is in it (in a Flatpak more than the process ID, which windows can share),
/// so concurrent sessions never write to the same temporary file.
pub fn replace_file(path: &Path, bytes: impl AsRef<[u8]>) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension(format!("{}.tmp", crate::remote::instance_id()));
    // On disk before the rename: after a crash the old file or the new one, never an empty one.
    let result = std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))
        .and_then(|_| std::fs::File::create(&tmp))
        .and_then(|mut f| f.write_all(bytes.as_ref()).and_then(|_| f.sync_all()))
        .and_then(|_| std::fs::rename(&tmp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// `replace_file`, with a failure on stderr: for files Glimt keeps for itself.
pub fn write_atomic(path: &Path, text: &str) {
    if let Err(e) = replace_file(path, text) {
        eprintln!("could not save {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    /// This window changed the sort order, another turned thumbnails off: both stay.
    #[test]
    fn keeps_what_other_windows_changed() {
        let saved = "thumbnails=true\nsort=name\n";
        let ours = "thumbnails=true\nsort=date\n";
        let on_disk = "thumbnails = false\nsort=name\nfrom_a_newer_glimt=1\n";
        let text = super::merged(ours, saved, on_disk);
        assert_eq!(text, "thumbnails=false\nsort=date\nfrom_a_newer_glimt=1\n");
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

    #[test]
    fn parses_config() {
        use crate::Config;
        assert_eq!(Config::parse("# comment\npreload= 5\n").preload, 5);
        assert_eq!(Config::parse("preload=99").preload, 20);
        let c = Config::parse("decode_threads=0\nanimation_mb=5000");
        assert_eq!((c.decode_threads, c.animation_mb), (1, 1024));
        // The defaults are the Medium level.
        let d = Config::default();
        let medium = crate::MEMORY_LEVELS
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
