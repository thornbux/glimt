//! Glimt: a fast, memory-frugal image viewer.
//!
//! Images are decoded in background threads to the size of the image area; full resolution
//! is decoded only when the user zooms past what that covers. Only a small window around
//! the current image is kept in memory.

use eframe::egui;
use image::ImageDecoder;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Instant;

mod exif;
mod help;
#[cfg(feature = "marks")]
mod marks;
mod raw;
mod remote;
mod search;
mod thumbs;
#[cfg(feature = "video")]
mod video;

const EXTS: &[&str] = &["jpg", "jpeg", "png", "webp", "gif", "tif", "tiff"];
/// HEIC/HEIF photos (iPhone), decoded by ffmpeg with the heic feature.
const HEIC_EXTS: &[&str] = if cfg!(feature = "heic") {
    &["heic", "heif", "hif"]
} else {
    &[]
};
/// AVIF photos, decoded by ffmpeg (dav1d) with the avif feature.
const AVIF_EXTS: &[&str] = if cfg!(feature = "avif") {
    &["avif"]
} else {
    &[]
};

/// JPEG XL photos, with the jxl feature.
const JXL_EXTS: &[&str] = if cfg!(feature = "jxl") { &["jxl"] } else { &[] };

/// SVG images, with the svg feature.
const SVG_EXTS: &[&str] = if cfg!(feature = "svg") {
    &["svg", "svgz"]
} else {
    &[]
};

/// HEIC and AVIF: both HEIF files, decoded the same way.
fn is_heif(path: &Path) -> bool {
    has_ext(path, HEIC_EXTS) || has_ext(path, AVIF_EXTS)
}
/// Colour management on (Preferences); read by the decode and thumbnail threads.
static COLOR_MANAGEMENT: AtomicBool = AtomicBool::new(true);
/// Default number of images kept decoded on each side of the shown one.
const DEFAULT_AHEAD: usize = 2;
/// Default seconds between images in a slideshow.
const DEFAULT_DELAY: u64 = 3;
const MAX_DELAY: u64 = 60;
/// Thumbnail box in the file list, in points (Geeqie's default 96×72).
const THUMBS: [egui::Vec2; 3] = [
    egui::vec2(96.0, 72.0),
    egui::vec2(128.0, 96.0),
    egui::vec2(192.0, 144.0),
];
/// A cell of the thumbnail grid: the thumbnail with its name below.
const CELL_EXTRA: egui::Vec2 = egui::vec2(8.0, 26.0);
/// Big thumbnails (Preferences) come from the freedesktop "large" cache; read by the workers.
static LARGE_THUMBS: AtomicBool = AtomicBool::new(false);
/// Thumbnail jobs further than this from the visible rows are skipped.
const THUMB_MARGIN: usize = 20;
/// Width of the size and date columns in the file list (points).
const DATE_WIDTH: f32 = 140.0;
const DETAILS_WIDTH: f32 = DATE_WIDTH + 90.0;
/// Thumbnails kept in memory (≈ 64 kB each) before those far from view are dropped.
const MAX_THUMBS: usize = 600;
/// Decoded frames kept of one animation (bytes); longer ones loop the first part.
const MAX_ANIMATION: usize = 128 << 20;

/// Everything the user can do from a menu or a shortcut.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Action {
    OpenFile,
    OpenFolder,
    NewFolder,
    Rename,
    Copy,
    Move,
    OpenWith,
    Play,
    Delete,
    DeletePermanently,
    CloseWindow,
    Quit,
    First,
    Prev,
    Next,
    Last,
    Back,
    Forward,
    Home,
    Up,
    SelectAll,
    SelectNone,
    InvertSelection,
    CopyPath,
    CopyPathUnquoted,
    CopyImage,
    CopyFiles,
    Preferences,
    ExifRotate,
    RotateCw,
    RotateCcw,
    Rotate180,
    Mirror,
    Flip,
    SaveOrientation,
    ZoomIn,
    ZoomOut,
    Zoom1To1,
    ZoomFit,
    FullScreen,
    HideTools,
    Overlay,
    Histogram,
    HistogramLog,
    Compare,
    Animate,
    Subfolders,
    NextPage,
    PrevPage,
    // Marks 1–6 (keys 1–6), selecting by mark (Ctrl+1–6), 0–5 stars (Alt+0–5).
    #[cfg(feature = "marks")]
    Mark1,
    #[cfg(feature = "marks")]
    Mark2,
    #[cfg(feature = "marks")]
    Mark3,
    #[cfg(feature = "marks")]
    Mark4,
    #[cfg(feature = "marks")]
    Mark5,
    #[cfg(feature = "marks")]
    Mark6,
    #[cfg(feature = "marks")]
    SelectMark1,
    #[cfg(feature = "marks")]
    SelectMark2,
    #[cfg(feature = "marks")]
    SelectMark3,
    #[cfg(feature = "marks")]
    SelectMark4,
    #[cfg(feature = "marks")]
    SelectMark5,
    #[cfg(feature = "marks")]
    SelectMark6,
    #[cfg(feature = "marks")]
    Rate0,
    #[cfg(feature = "marks")]
    Rate1,
    #[cfg(feature = "marks")]
    Rate2,
    #[cfg(feature = "marks")]
    Rate3,
    #[cfg(feature = "marks")]
    Rate4,
    #[cfg(feature = "marks")]
    Rate5,
    Icons,
    Toolbar,
    PathBar,
    Thumbnails,
    ExifPanel,
    ExifWindow,
    FolderTree,
    Refresh,
    Slideshow,
    Pause,
    RandomOrder,
    Faster,
    Slower,
    Escape,
    NewWindow,
    Help,
    Shortcuts,
    KeyboardMap,
    About,
}

impl Action {
    /// The name shown in menus and in the help window.
    /// For the marks actions: 'm' (toggle mark), 's' (select by mark) or 'r' (stars), and the
    /// number.
    #[cfg(feature = "marks")]
    fn mark(self) -> Option<(char, u8)> {
        use Action::*;
        let all = [
            Mark1,
            Mark2,
            Mark3,
            Mark4,
            Mark5,
            Mark6,
            SelectMark1,
            SelectMark2,
            SelectMark3,
            SelectMark4,
            SelectMark5,
            SelectMark6,
            Rate0,
            Rate1,
            Rate2,
            Rate3,
            Rate4,
            Rate5,
        ];
        let i = all.iter().position(|&a| a == self)? as u8;
        Some(match i {
            0..6 => ('m', i + 1),
            6..12 => ('s', i - 5),
            _ => ('r', i - 12),
        })
    }

    fn label(self) -> &'static str {
        use Action::*;
        match self {
            OpenFile => "Open File…",
            OpenFolder => "Open Folder…",
            NewFolder => "New Folder…",
            Rename => "Rename…",
            Copy => "Copy…",
            Move => "Move…",
            OpenWith => "Open With Default App",
            Play => "Play Video",
            Delete => "Delete…",
            DeletePermanently => "Delete Permanently…",
            CloseWindow => "Close Window",
            Quit => "Quit",
            First => "First Image",
            Prev => "Previous Image",
            Next => "Next Image",
            Last => "Last Image",
            Back => "Back",
            Forward => "Forward",
            Home => "Home",
            Up => "Up",
            SelectAll => "Select All",
            SelectNone => "Select None",
            InvertSelection => "Invert Selection",
            CopyPath => "Copy Path to Clipboard",
            CopyPathUnquoted => "Copy Path (Unquoted)",
            CopyImage => "Copy Image to Clipboard",
            CopyFiles => "Copy Files to Clipboard",
            Preferences => "Preferences…",
            ExifRotate => "Auto Rotate Using EXIF",
            RotateCw => "Rotate Clockwise",
            RotateCcw => "Rotate Counterclockwise",
            Rotate180 => "Rotate 180°",
            Mirror => "Mirror",
            Flip => "Flip",
            SaveOrientation => "Save Orientation to File",
            ZoomIn => "Zoom In",
            ZoomOut => "Zoom Out",
            Zoom1To1 => "Zoom 1:1",
            ZoomFit => "Zoom to Fit",
            FullScreen => "Full Screen",
            HideTools => "Hide File List",
            Overlay => "Show Image Info in Full Screen",
            Histogram => "Histogram: Off, RGB, Brightness",
            HistogramLog => "Logarithmic Histogram",
            Compare => "Compare Side by Side",
            Animate => "Play Animations",
            Subfolders => "Include Subfolders",
            NextPage => "Next Page",
            PrevPage => "Previous Page",
            #[cfg(feature = "marks")]
            Mark1 => "Mark 1",
            #[cfg(feature = "marks")]
            Mark2 => "Mark 2",
            #[cfg(feature = "marks")]
            Mark3 => "Mark 3",
            #[cfg(feature = "marks")]
            Mark4 => "Mark 4",
            #[cfg(feature = "marks")]
            Mark5 => "Mark 5",
            #[cfg(feature = "marks")]
            Mark6 => "Mark 6",
            #[cfg(feature = "marks")]
            SelectMark1 => "Select Mark 1",
            #[cfg(feature = "marks")]
            SelectMark2 => "Select Mark 2",
            #[cfg(feature = "marks")]
            SelectMark3 => "Select Mark 3",
            #[cfg(feature = "marks")]
            SelectMark4 => "Select Mark 4",
            #[cfg(feature = "marks")]
            SelectMark5 => "Select Mark 5",
            #[cfg(feature = "marks")]
            SelectMark6 => "Select Mark 6",
            #[cfg(feature = "marks")]
            Rate0 => "No Stars",
            #[cfg(feature = "marks")]
            Rate1 => "★",
            #[cfg(feature = "marks")]
            Rate2 => "★★",
            #[cfg(feature = "marks")]
            Rate3 => "★★★",
            #[cfg(feature = "marks")]
            Rate4 => "★★★★",
            #[cfg(feature = "marks")]
            Rate5 => "★★★★★",
            Icons => "View Images as Icons",
            Toolbar => "Show Toolbar",
            PathBar => "Show Folder Path",
            Thumbnails => "Show Thumbnails",
            ExifPanel => "Show EXIF Panel",
            ExifWindow => "EXIF Window",
            FolderTree => "Show Folder Tree",
            Refresh => "Refresh",
            Slideshow => "Start/Stop Slideshow",
            Pause => "Pause Slideshow",
            RandomOrder => "Random Order",
            Faster => "Faster",
            Slower => "Slower",
            Escape => "Stop Slideshow, Leave Full Screen",
            NewWindow => "New Window",
            Help => "Help Manual",
            Shortcuts => "Keyboard Shortcuts",
            KeyboardMap => "Keyboard Map",
            About => "About Glimt",
        }
    }
}

const NONE: egui::Modifiers = egui::Modifiers::NONE;
const SHIFT: egui::Modifiers = egui::Modifiers::SHIFT;
const CTRL: egui::Modifiers = egui::Modifiers::COMMAND;
const ALT: egui::Modifiers = egui::Modifiers::ALT;
const CTRL_SHIFT: egui::Modifiers = egui::Modifiers {
    shift: true,
    ..egui::Modifiers::COMMAND
};

/// Geeqie's default shortcuts, except R (random order; Geeqie: refresh) and F5 (refresh).
/// The first one for an action is shown in the menu.
/// egui ignores an extra Shift or Alt when matching, so those combinations come before the plain key.
const SHORTCUTS: &[(Action, egui::Modifiers, egui::Key)] = {
    use Action::*;
    use egui::Key;
    &[
        (ExifRotate, ALT, Key::X),
        // Before R (random order) and F (full screen), which would also match with Shift.
        (Rotate180, SHIFT, Key::R),
        (Mirror, SHIFT, Key::M),
        (Flip, SHIFT, Key::F),
        (RotateCw, NONE, Key::CloseBracket),
        (RotateCcw, NONE, Key::OpenBracket),
        (DeletePermanently, SHIFT, Key::Delete),
        (Delete, NONE, Key::Delete),
        (Delete, CTRL, Key::D),
        (NewFolder, CTRL, Key::F),
        (Rename, CTRL, Key::R),
        // Before Ctrl+C (copy to a folder), which would also match with Shift.
        (CopyImage, CTRL_SHIFT, Key::C),
        (Copy, CTRL, Key::C),
        (Move, CTRL, Key::M),
        (CloseWindow, CTRL, Key::W),
        (Quit, CTRL, Key::Q),
        (First, NONE, Key::Home),
        (Prev, NONE, Key::Backspace),
        (Prev, NONE, Key::PageUp),
        (Prev, NONE, Key::ArrowLeft),
        (Prev, NONE, Key::ArrowUp),
        (Next, NONE, Key::Space),
        (Next, NONE, Key::PageDown),
        (Next, NONE, Key::ArrowRight),
        (Next, NONE, Key::ArrowDown),
        (Last, NONE, Key::End),
        (SelectNone, CTRL_SHIFT, Key::A),
        (InvertSelection, CTRL_SHIFT, Key::I),
        (Icons, CTRL, Key::I),
        (SelectAll, CTRL, Key::A),
        (Preferences, CTRL, Key::O),
        (ZoomIn, NONE, Key::Equals),
        (ZoomIn, NONE, Key::Plus),
        (ZoomOut, NONE, Key::Minus),
        (Zoom1To1, NONE, Key::Z),
        (ZoomFit, NONE, Key::X),
        (FullScreen, NONE, Key::F),
        (FullScreen, NONE, Key::V),
        (FullScreen, NONE, Key::F11),
        (HideTools, CTRL, Key::H),
        (Thumbnails, NONE, Key::T),
        (Overlay, NONE, Key::I),
        (Histogram, NONE, Key::K),
        (HistogramLog, NONE, Key::J),
        (Compare, NONE, Key::E),
        (Animate, NONE, Key::A),
        (NextPage, CTRL, Key::PageDown),
        (PrevPage, CTRL, Key::PageUp),
        // Alt+digit before the plain digit, which would also match with Alt.
        #[cfg(feature = "marks")]
        (Rate0, ALT, Key::Num0),
        #[cfg(feature = "marks")]
        (Rate1, ALT, Key::Num1),
        #[cfg(feature = "marks")]
        (Rate2, ALT, Key::Num2),
        #[cfg(feature = "marks")]
        (Rate3, ALT, Key::Num3),
        #[cfg(feature = "marks")]
        (Rate4, ALT, Key::Num4),
        #[cfg(feature = "marks")]
        (Rate5, ALT, Key::Num5),
        #[cfg(feature = "marks")]
        (SelectMark1, CTRL, Key::Num1),
        #[cfg(feature = "marks")]
        (SelectMark2, CTRL, Key::Num2),
        #[cfg(feature = "marks")]
        (SelectMark3, CTRL, Key::Num3),
        #[cfg(feature = "marks")]
        (SelectMark4, CTRL, Key::Num4),
        #[cfg(feature = "marks")]
        (SelectMark5, CTRL, Key::Num5),
        #[cfg(feature = "marks")]
        (SelectMark6, CTRL, Key::Num6),
        #[cfg(feature = "marks")]
        (Mark1, NONE, Key::Num1),
        #[cfg(feature = "marks")]
        (Mark2, NONE, Key::Num2),
        #[cfg(feature = "marks")]
        (Mark3, NONE, Key::Num3),
        #[cfg(feature = "marks")]
        (Mark4, NONE, Key::Num4),
        #[cfg(feature = "marks")]
        (Mark5, NONE, Key::Num5),
        #[cfg(feature = "marks")]
        (Mark6, NONE, Key::Num6),
        (ExifPanel, CTRL, Key::K),
        (ExifWindow, CTRL, Key::E),
        (FolderTree, CTRL, Key::T),
        (Refresh, NONE, Key::F5),
        (Slideshow, NONE, Key::S),
        (Pause, NONE, Key::P),
        (RandomOrder, NONE, Key::R),
        (Faster, CTRL, Key::Equals),
        (Slower, CTRL, Key::Minus),
        (Play, NONE, Key::Enter),
        (Escape, NONE, Key::Escape),
        (Escape, NONE, Key::Q),
        (NewWindow, CTRL, Key::N),
        (Help, NONE, Key::F1),
    ]
};

/// What a zenity file dialog was opened for.
enum Pick {
    Open,
    CopyTo(Vec<PathBuf>),
    MoveTo(Vec<PathBuf>),
}

/// What the one-line text dialog is asking for.
#[derive(Clone, Copy)]
enum Prompt {
    NewFolder,
    /// Rename the shown image.
    Rename,
    /// Rename the selected files with a pattern: * = old name, # = number.
    RenameMany,
}

/// A menu entry showing the action's shortcut; sets `act` when clicked.
fn menu_item(ui: &mut egui::Ui, act: &mut Option<Action>, action: Action) {
    menu_button(ui, act, egui::Button::new(action.label()), action);
}

/// Mouse use, for the help window.
const MOUSE: &[(&str, &str)] = &[
    ("Mouse wheel", "Zoom, or next/previous image (Preferences)"),
    ("Ctrl+mouse wheel", "Zoom"),
    ("Drag", "Move a zoomed image"),
    ("Double-click", "Full Screen"),
    ("Ctrl+click in the list", "Select or unselect a file"),
];

/// Where Glimt differs from Geeqie: what, Glimt, Geeqie. Shown in the Help Manual (F1).
const DIFFERENCES: &[(&str, &str, &str)] = &[
    (
        "R",
        "Toggles random order in the slideshow, also while it runs",
        "Refresh",
    ),
    ("Refresh", "F5", "R"),
    (
        "Random order",
        "R, the View menu and Preferences",
        "Only in Preferences",
    ),
    (
        "Arrow keys",
        "←/↑ previous, →/↓ next image; they move a zoomed image that is larger than the window",
        "Scroll a zoomed image, move in the file list",
    ),
    (
        "Mouse wheel",
        "Zooms by default; a setting makes it go to the next or previous image",
        "Next or previous image",
    ),
    (
        "Esc / Q",
        "Stops the slideshow and leaves full screen; never quits",
        "Leaves full screen",
    ),
    (
        "Slideshow delay",
        "Whole seconds 1–60, Ctrl+= / Ctrl+- change it by 1 s",
        "Tenths of a second, Faster/Slower change it by 0.5 s",
    ),
    (
        "Browsing in a slideshow",
        "The new image gets the full delay",
        "",
    ),
    (
        "Small images",
        "\"Enlarge small images to fit the window\", on by default",
        "\"Allow enlargement\"",
    ),
    (
        "Close Window (Ctrl+W)",
        "Same as Quit (one window per process)",
        "Closes one of several windows",
    ),
    (
        "Keypad /, *, +, -",
        "Not supported",
        "Zoom 1:1, fit, in, out",
    ),
    (
        "Marks and stars (marks feature)",
        "1–6 toggle, Ctrl+1–6 select, Alt+0–5 stars (main keyboard); kept in ~/.local/state/glimt/marks; mark: and stars: in the search field",
        "Alt+keypad for stars, kept in XMP; marks filter buttons",
    ),
    (
        "Copy, Move (Ctrl+C, Ctrl+M)",
        "Target folder from a file dialog; never overwrites",
        "Own dialog",
    ),
    (
        "New Window (Ctrl+N)",
        "A new process in the same folder",
        "A new window in the same process, saved layouts",
    ),
    (
        "Images as icons (Ctrl+I)",
        "Ctrl+I switches between list and icons",
        "Ctrl+I icons, Ctrl+L list",
    ),
    (
        "Compare (E)",
        "E shows the next image (or the other of two selected) beside the shown one, at the same zoom and offset",
        "E, U and Y split into panes with an image each; Y back to one",
    ),
    (
        "Colour management",
        "Always on: embedded ICC profiles are converted to sRGB; the screen is taken to be sRGB",
        "Optional, with a chosen screen profile",
    ),
    (
        "Files on the clipboard",
        "Select → Copy Files to Clipboard copies them for a file manager; no Cut (Ctrl+X)",
        "Ctrl+C copies, Ctrl+X cuts",
    ),
    (
        "Histogram (K, J)",
        "In the image's corner: K cycles off, RGB and brightness, J logarithmic",
        "In the overlay: K cycles channels, J modes",
    ),
    (
        "Image info in full screen (I)",
        "On or off",
        "I cycles through several overlay layouts",
    ),
    (
        "Thumbnail cache",
        "The shared ~/.cache/thumbnails; new ones are saved only if the setting is on",
        "Own cache, or the shared one",
    ),
    (
        "Refresh on change",
        "Setting, on by default; checks the folder once a second",
        "Setting",
    ),
];

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

/// The first shortcut of an action, as shown in menus ("Ctrl+K").
/// The user's own keys (Preferences), which replace the defaults of their actions.
static OWN_KEYS: std::sync::RwLock<Vec<(Action, egui::Modifiers, egui::Key)>> =
    std::sync::RwLock::new(Vec::new());

/// The shortcuts in use: the user's own, and the defaults of the other actions. Those with
/// more modifiers come first, as egui ignores an extra Shift or Alt when matching.
fn shortcuts() -> Vec<(Action, egui::Modifiers, egui::Key)> {
    let own = OWN_KEYS.read().map(|o| o.clone()).unwrap_or_default();
    let mut all = own.clone();
    all.extend(SHORTCUTS.iter().filter(|s| !own.iter().any(|o| o.0 == s.0)));
    let weight = |m: &egui::Modifiers| m.shift as u8 + m.alt as u8 + m.command as u8;
    all.sort_by_key(|s| std::cmp::Reverse(weight(&s.1)));
    all
}

/// Own keys in the config: "Next=Ctrl+N;ZoomIn=Plus", by action and egui's key names.
#[derive(Clone, Debug, Default, PartialEq)]
struct Keys(Vec<(Action, egui::Modifiers, egui::Key)>);

impl std::str::FromStr for Keys {
    type Err = ();
    fn from_str(text: &str) -> Result<Self, ()> {
        let one = |item: &str| {
            let (name, combo) = item.split_once('=')?;
            let action = SHORTCUTS
                .iter()
                .map(|s| s.0)
                .find(|a| format!("{a:?}") == name)?;
            let mut mods = egui::Modifiers::NONE;
            let mut parts: Vec<&str> = combo.split('+').collect();
            let key = egui::Key::from_name(parts.pop()?)?;
            for part in parts {
                match part {
                    "Ctrl" => mods.command = true,
                    "Shift" => mods.shift = true,
                    "Alt" => mods.alt = true,
                    _ => return None,
                }
            }
            mods.ctrl = mods.command;
            Some((action, mods, key))
        };
        Ok(Keys(text.split(';').filter_map(one).collect()))
    }
}

impl std::fmt::Display for Keys {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let items: Vec<String> = (self.0.iter())
            .map(|(a, m, k)| {
                let mut combo = String::new();
                for (on, name) in [(m.command, "Ctrl+"), (m.shift, "Shift+"), (m.alt, "Alt+")] {
                    if on {
                        combo += name;
                    }
                }
                format!("{a:?}={combo}{}", k.name())
            })
            .collect();
        write!(f, "{}", items.join(";"))
    }
}

fn shortcut_text(ctx: &egui::Context, action: Action) -> Option<String> {
    let (_, mods, key) = shortcuts().into_iter().find(|s| s.0 == action)?;
    Some(ctx.format_shortcut(&egui::KeyboardShortcut::new(mods, key)))
}

/// A toolbar button with the action's name and shortcut as tooltip.
fn tool_button(ui: &mut egui::Ui, act: &mut Option<Action>, icon: &str, action: Action, on: bool) {
    let tip = match shortcut_text(ui.ctx(), action) {
        Some(keys) => format!("{} ({keys})", action.label()),
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

struct Decoded {
    idx: usize,
    full: bool,
    /// The file the job was for; rejects results from a folder that has since been replaced.
    path: PathBuf,
    /// Decoded with EXIF rotation; rejected if the setting changed meanwhile.
    rotated: bool,
    /// The page of a multi-page TIFF; 0 for everything else.
    page: usize,
    /// `None` = skipped because the user has already moved past it.
    result: Option<Result<DecodedImage, String>>,
}

/// The frames, the original size after EXIF rotation, the histogram and the number of pages.
type DecodedImage = (Frames, [usize; 2], Box<Hist>, usize);

/// A file's EXIF from the reader thread: the panel's rows and every field.
type ExifRead = (
    PathBuf,
    Vec<(&'static str, String)>,
    Vec<(&'static str, String, String)>,
);

/// Pixel counts per level of red, green, blue and brightness.
type Hist = [[u32; 256]; 4];

fn histogram(img: &egui::ColorImage) -> Box<Hist> {
    let mut h = Box::new([[0; 256]; 4]);
    for p in &img.pixels {
        let [r, g, b, _] = p.to_array();
        h[0][r as usize] += 1;
        h[1][g as usize] += 1;
        h[2][b as usize] += 1;
        // Rec. 709 luma.
        h[3][((54 * r as u32 + 183 * g as u32 + 19 * b as u32) >> 8) as usize] += 1;
    }
    h
}

/// The decoded frames with their delays in ms; one frame for a still image.
type Frames = Vec<(egui::ColorImage, u32)>;

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
        let total: u32 = self.anim.iter().map(|f| f.1).sum();
        let mut at = (t.as_millis() % total.max(1) as u128) as u32;
        for (tex, ms) in &self.anim {
            if at < *ms {
                ctx.request_repaint_after(std::time::Duration::from_millis((ms - at) as u64));
                return tex;
            }
            at -= ms;
        }
        &self.tex
    }
}

struct App {
    dir: PathBuf,
    subdirs: Vec<PathBuf>,
    /// The images in `dir`, file name only (the full path is built on demand by `path`): saves
    /// memory in large folders, where the path would otherwise be repeated for every file.
    files: Vec<OsString>,
    cur: usize,
    /// The images to keep decoded, the shown one first: its neighbours in list order.
    /// Shared with the workers, which skip jobs outside it.
    keep: Arc<Mutex<Vec<usize>>>,
    /// How many images at the start of `keep` may be decoded at full resolution: the shown one,
    /// and the compared one beside it.
    full_ok: Arc<AtomicUsize>,
    /// Number of images preloaded on each side (Preferences).
    ahead: Arc<AtomicUsize>,
    /// Workers apply EXIF orientation (Preferences, Alt+X).
    rotate: Arc<AtomicBool>,
    show_settings: bool,
    help: Option<help::Page>,
    /// The delete confirmation is open: `Some(true)` = move to trash, `Some(false)` = permanently.
    confirm: Option<bool>,
    /// Open text dialog and the text typed so far.
    prompt: Option<(Prompt, String)>,
    /// Scale from the previous frame (physical pixels per image pixel), for the zoom percentage in the status bar.
    last_scale: f32,
    /// Mouse wheel notches not yet turned into an image step (wheel_zoom off).
    wheel: f32,
    /// Last drawn image; kept on screen until the next one is decoded, so browsing never shows a black screen.
    last_drawn: Option<(egui::TextureHandle, [usize; 2])>,
    /// Selected files (Ctrl/Shift+click in the file list) that Delete acts on.
    selected: std::collections::BTreeSet<usize>,
    config: Config,
    /// Max decode size in physical pixels: [image area in the window, max texture side].
    limits: Arc<Mutex<[[u32; 2]; 2]>>,
    /// (image index, full resolution, file)
    jobs: mpsc::Sender<(usize, bool, PathBuf, usize)>,
    /// The page of a multi-page TIFF shown (Ctrl+Page Down/Up), from 0.
    page: usize,
    done: mpsc::Receiver<Decoded>,
    slots: HashMap<usize, Slot>,
    pending: HashSet<(usize, bool)>,
    errors: HashMap<usize, String>,
    /// Physical screen pixels per original pixel; `None` = fit to window.
    zoom: Option<f32>,
    pan: egui::Vec2,
    /// Set at startup; taken when the first image is shown.
    started: Option<Instant>,
    /// Answer from an open file dialog (zenity).
    dialog: Option<(Pick, mpsc::Receiver<Option<PathBuf>>)>,
    /// Generation of the folder watcher; opening another folder ends the old thread.
    watching: Arc<AtomicUsize>,
    /// The watcher saw the folder change.
    changed_tx: mpsc::Sender<()>,
    changed_rx: mpsc::Receiver<()>,
    /// The folder changed while a listing was running; list again when it is done.
    stale: bool,
    /// View → Hide File List: only the image, as in full screen but in a window.
    hide_tools: bool,
    /// Go → Back / Forward: earlier locations (file shown, or folder if it had no images).
    back: Vec<PathBuf>,
    forward: Vec<PathBuf>,
    /// Error messages from background tasks (deleting), and the one shown now.
    notices_tx: mpsc::Sender<String>,
    /// A network location (smb:// …) being mounted: its local path, or why it failed.
    mounting: Option<mpsc::Receiver<(Mounted, Login)>>,
    /// The login dialog for a network share that needs one.
    login: Option<Login>,
    /// "Open With" applications by file extension, found once per extension.
    apps: HashMap<String, Vec<(String, PathBuf)>>,
    /// Commands from `glimt --remote`.
    remote: mpsc::Receiver<String>,
    /// The window had the focus last frame; getting it makes this Glimt the one `--remote`
    /// talks to.
    has_focus: bool,
    /// Our own clipboard for file lists, kept so it can serve them (X11).
    clipboard: Option<arboard::Clipboard>,
    /// An image decoded for the clipboard; egui takes it only during a frame.
    clip: Option<mpsc::Receiver<egui::ColorImage>>,
    /// Subfolders by folder for the folder tree; `None` while being read.
    tree: HashMap<PathBuf, Option<Vec<PathBuf>>>,
    tree_tx: mpsc::Sender<(PathBuf, Vec<PathBuf>)>,
    tree_rx: mpsc::Receiver<(PathBuf, Vec<PathBuf>)>,
    /// The folder the tree was last opened up to; it opens again when the folder changes.
    tree_shown: PathBuf,
    #[cfg(feature = "marks")]
    marks: marks::Marks,
    /// The list holds the images of the subfolders too (slideshow with subfolders).
    recursive: bool,
    /// The next listing is shuffled as a whole instead of merged into the current order.
    whole_shuffle: bool,
    /// The action whose key is being chosen in Preferences: the next key press goes to it.
    capturing: Option<Action>,
    /// Two images side by side (E): the shown one and `compare_other`.
    compare: bool,
    /// Columns of the icon grid in the last frame; 1 when the list is rows or hidden.
    grid_cols: usize,
    /// How far Previous/Next move this frame: a row of the icon grid for ↑ and ↓, else 1.
    row_step: isize,
    notices: mpsc::Receiver<String>,
    notice: Option<String>,
    /// Total size of the images in the folder, summed in the background after listing.
    /// Only sums are sent (no per-file table), so memory use does not grow with the folder.
    total_size: u64,
    /// Partial sums from the background; `None` once all have arrived.
    size_rx: Option<mpsc::Receiver<u64>>,
    /// Bytes freed by deletions in the background.
    freed_tx: mpsc::Sender<u64>,
    freed_rx: mpsc::Receiver<u64>,
    /// Size of the selected files; computed in the background when the selection changes.
    selected_size: Option<u64>,
    selected_rx: Option<mpsc::Receiver<u64>>,
    /// Folder listing in the background: (subfolders, images).
    listing: Option<mpsc::Receiver<(Vec<PathBuf>, Vec<OsString>)>>,
    /// Scroll the file list to the current image on the next frame.
    scroll_to_cur: bool,
    /// Slideshow: when the previous image was shown; `None` = off.
    slideshow: Option<Instant>,
    /// Slideshow paused (P): stays on the image until resumed.
    paused: bool,
    /// The file list is shuffled for a random slideshow.
    random: bool,
    /// File list thumbnails by file name; `None` = could not be made.
    thumbs: HashMap<OsString, Option<egui::TextureHandle>>,
    thumbs_pending: HashSet<OsString>,
    /// (image index, file)
    thumb_jobs: mpsc::Sender<(usize, PathBuf)>,
    /// (file, thumbnail); the outer `None` = skipped because it scrolled out of view.
    thumbs_done: mpsc::Receiver<(PathBuf, Option<Option<egui::ColorImage>>)>,
    /// First and last visible row of the file list, for skipping stale thumbnail jobs.
    thumb_view: Arc<[AtomicUsize; 2]>,
    /// Workers write new thumbnails to the shared cache (Preferences).
    save_thumbs: Arc<AtomicBool>,
    /// EXIF panel: the file asked for last, and the rows read for a file.
    exif_jobs: mpsc::Sender<PathBuf>,
    exif_done: mpsc::Receiver<ExifRead>,
    /// Every EXIF field of the shown image, for the EXIF window (Ctrl+E).
    exif_all: Vec<(&'static str, String, String)>,
    exif_window: bool,
    exif_asked: Option<PathBuf>,
    exif: Option<(PathBuf, Vec<(&'static str, String)>)>,
    /// Size in bytes of the shown file, for the status bar.
    cur_size: Option<(PathBuf, u64)>,
    /// The folder path in the toolbar, as typed.
    path_text: String,
    /// Window geometry seen in the last frame, saved at exit.
    window: WindowState,
    /// The screen saver inhibitor running while in full screen.
    inhibitor: Option<std::process::Child>,
    /// File list size and date columns: (bytes, mtime) by file name.
    details: HashMap<OsString, (u64, i64)>,
    /// Names being deleted in the background; kept out of refreshed listings until done.
    deleting: HashSet<OsString>,
    deleted_tx: mpsc::Sender<Vec<PathBuf>>,
    deleted_rx: mpsc::Receiver<Vec<PathBuf>>,
    /// Background deletes, copies and moves still running, for the busy pointer.
    busy: Arc<AtomicUsize>,
    /// File name search above the file list, and the matching files best first while it has
    /// at least three letters.
    search: String,
    matches: Option<Vec<usize>>,
    /// Total size of the search matches, summed in the background like `total_size`.
    match_size: u64,
    match_rx: Option<mpsc::Receiver<u64>>,
    /// EXIF text of each file in the folder, lowercase, for "exif:" searches; built on the
    /// first one, dropped with the folder.
    exif_index: Option<HashMap<OsString, String>>,
    exif_rx: Option<mpsc::Receiver<HashMap<OsString, String>>>,
    /// Where a Shift selection started (file index).
    anchor: Option<usize>,
    /// Orientation of the shown image, for viewing only: quarter turns clockwise, applied
    /// after mirroring when the flag is set. Reset for each image, as in Geeqie.
    orient: (u8, bool),
    /// When the shown image was opened: the clock of an animation.
    opened: Instant,
    /// How far the shown image can be panned each way (points); zero where it fits.
    max_pan: egui::Vec2,
}

fn has_ext(path: &Path, exts: &[&str]) -> bool {
    let ext = path.extension().and_then(|e| e.to_str());
    ext.is_some_and(|e| exts.contains(&e.to_ascii_lowercase().as_str()))
}

/// A frame `at` seconds in, decoded by the `ffmpeg` program.
#[cfg(any(feature = "video", feature = "heic", feature = "avif"))]
fn ffmpeg_frame(path: &Path, at: &str) -> Result<image::DynamicImage, String> {
    ffmpeg_ppm(path, &["-ss", at], &[])
}

/// A HEIC or AVIF photo. Phones store it as a grid of tiles, which ffmpeg reads as separate streams
/// with their places in a "Tile Grid" group: they are put together with xstack, cut to the
/// image size and turned by the stored rotation.
#[cfg(any(feature = "heic", feature = "avif"))]
fn heif(path: &Path) -> Result<image::DynamicImage, String> {
    let probe = command("ffprobe")
        .args(["-v", "error", "-show_stream_groups", "-of", "compact"])
        .arg(path)
        .output()
        .map_err(|e| format!("HEIC and AVIF need ffprobe (ffmpeg): {e}"))?;
    let text = String::from_utf8_lossy(&probe.stdout);
    let Some(grid) = text.lines().find(|l| l.contains("type=Tile Grid")) else {
        return ffmpeg_frame(path, "0");
    };
    let fields: Vec<(&str, &str)> = grid.split('|').filter_map(|f| f.split_once('=')).collect();
    let get = |key: &str| {
        fields
            .iter()
            .find(|f| f.0.ends_with(key))
            .map_or("0", |f| f.1)
    };
    let (mut inputs, mut layout) = (String::new(), Vec::<String>::new());
    for (key, value) in &fields {
        if key.ends_with(":stream_index") {
            inputs += &format!("[0:{value}]");
        } else if key.ends_with(":tile_horizontal_offset") {
            layout.push(value.to_string());
        } else if key.ends_with(":tile_vertical_offset")
            && let Some(x) = layout.last_mut()
        {
            *x += &format!("_{value}");
        }
    }
    if layout.len() < 2 {
        return ffmpeg_frame(path, "0");
    }
    let turn = match get("rotation") {
        "-90" | "270" => ",transpose=clock",
        "90" | "-270" => ",transpose=cclock",
        "180" | "-180" => ",hflip,vflip",
        _ => "",
    };
    let filter = format!(
        "{inputs}xstack=inputs={}:layout={},crop={}:{}:{}:{}{turn}[out]",
        layout.len(),
        layout.join("|"),
        get("tile_grid:width"),
        get("tile_grid:height"),
        get("tile_grid:horizontal_offset"),
        get("tile_grid:vertical_offset"),
    );
    ffmpeg_ppm(path, &[], &["-filter_complex", &filter, "-map", "[out]"])
}

/// One frame of `path` as decoded by ffmpeg, with options before and after the input.
#[cfg(any(feature = "video", feature = "heic", feature = "avif"))]
fn ffmpeg_ppm(path: &Path, before: &[&str], after: &[&str]) -> Result<image::DynamicImage, String> {
    let out = command("ffmpeg")
        .args(["-v", "error"])
        .args(before)
        .arg("-i")
        .arg(path)
        .args(after)
        .args(["-frames:v", "1", "-f", "image2pipe", "-vcodec", "ppm", "-"])
        .output()
        .map_err(|e| format!("video, HEIC and AVIF need ffmpeg: {e}"))?;
    if !out.status.success() || out.stdout.is_empty() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_owned();
        return Err(if err.is_empty() {
            "no frame".into()
        } else {
            err
        });
    }
    image::load_from_memory_with_format(&out.stdout, image::ImageFormat::Pnm)
        .map_err(|e| e.to_string())
}

#[cfg(feature = "video")]
fn is_video(path: &Path) -> bool {
    has_ext(path, video::EXTS)
}

#[cfg(not(feature = "video"))]
fn is_video(_: &Path) -> bool {
    false
}

/// A mounted share's local path; `Err(None)` = a login is needed, `Err(Some)` = why it failed.
type Mounted = Result<PathBuf, Option<String>>;

/// User name, domain and password for a network share.
#[derive(Clone, Default)]
struct Login {
    uri: String,
    user: String,
    domain: String,
    password: String,
}

/// The local path of a URI (smb://, sftp:// …), mounting it with gio first if needed: GVfs
/// puts mounted shares under /run/user/<uid>/gvfs. gio asks for what the share needs (user,
/// domain, password) one prompt at a time; they are answered from `login`, or, without one,
/// `Err(None)` says that a login is needed. A password saved in the keyring (from Files) is
/// used without asking.
fn gio_mount(uri: &str, login: Option<&Login>) -> Mounted {
    use std::io::{Read, Write};
    let local = || {
        let out = command("gio")
            .args(["info", "-a", "standard::name", uri])
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .find_map(|l| l.strip_prefix("local path: "))
            .map(PathBuf::from)
    };
    if let Some(path) = local() {
        return Ok(path);
    }
    let mut child = command("gio")
        .args(["mount", uri])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| Some(format!("Could not run gio: {e}")))?;
    let (mut out, mut input) = (child.stdout.take().unwrap(), child.stdin.take().unwrap());
    // Prompts end in ": " without a newline, the next one printed after the answer.
    let (mut text, mut answered, mut buf) = (String::new(), 0, [0; 512]);
    while let Ok(n @ 1..) = out.read(&mut buf) {
        text.push_str(&String::from_utf8_lossy(&buf[..n]));
        let prompt = &text[answered..];
        if !prompt.ends_with(": ") {
            continue;
        }
        let Some(login) = login else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(None);
        };
        let prompt = prompt.rsplit('\n').next().unwrap_or(prompt).to_lowercase();
        let answer = if prompt.contains("password") {
            &login.password
        } else if prompt.contains("domain") {
            &login.domain
        } else if prompt.contains("user") {
            &login.user
        } else {
            // "Login anonymously?" and anything else: the default.
            ""
        };
        let _ = writeln!(input, "{answer}");
        answered = text.len();
    }
    let out = child.wait_with_output().map_err(|e| Some(e.to_string()))?;
    local().ok_or_else(|| {
        let err = String::from_utf8_lossy(&out.stderr);
        // "gio: smb://server/share/: Failed to mount …": the reason only.
        let err = err.lines().last().map_or("", str::trim);
        let err = err
            .strip_prefix("gio: ")
            .and_then(|e| e.split_once(": "))
            .map_or(err, |e| e.1);
        Some(format!("Could not mount {uri}: {err}"))
    })
}

/// Applications registered for the type of `path` (gio mime), as (name, desktop file), Glimt
/// itself left out.
fn apps_for(path: &Path) -> Vec<(String, PathBuf)> {
    let gio = |args: &[&std::ffi::OsStr]| {
        command("gio")
            .args(args)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    };
    let info = gio(&[
        "info".as_ref(),
        "-a".as_ref(),
        "standard::content-type".as_ref(),
        path.as_os_str(),
    ]);
    let Some(mime) = info
        .lines()
        .find_map(|l| l.trim().strip_prefix("standard::content-type: "))
    else {
        return Vec::new();
    };
    let apps = gio(&["mime".as_ref(), mime.as_ref()]);
    let ids = apps
        .lines()
        .skip_while(|l| !l.starts_with("Registered applications"))
        .skip(1)
        .take_while(|l| l.starts_with('\t'))
        .map(str::trim)
        .filter(|id| *id != "glimt.desktop");
    // Desktop files live in the "applications" folder of the XDG data folders.
    let home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")));
    let dirs = std::env::var("XDG_DATA_DIRS").unwrap_or("/usr/local/share:/usr/share".into());
    let dirs: Vec<PathBuf> = home
        .into_iter()
        .chain(dirs.split(':').map(PathBuf::from))
        .map(|d| d.join("applications"))
        .collect();
    ids.filter_map(|id| {
        let file = dirs.iter().map(|d| d.join(id)).find(|f| f.is_file())?;
        let text = std::fs::read_to_string(&file).ok()?;
        let name = text.lines().find_map(|l| l.strip_prefix("Name="))?;
        Some((name.to_owned(), file))
    })
    .collect()
}

/// The EXIF fields of every file as one lowercase line, read on all cores.
fn exif_index(dir: &Path, files: &[OsString]) -> HashMap<OsString, String> {
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    let chunk = files.len().div_ceil(threads).max(1);
    std::thread::scope(|s| {
        let parts: Vec<_> = files
            .chunks(chunk)
            .map(|part| {
                s.spawn(move || {
                    part.iter()
                        .map(|name| {
                            let rows = exif::read(&dir.join(name));
                            let text: Vec<&str> = rows.iter().map(|r| r.1.as_str()).collect();
                            (name.clone(), text.join(" ").to_lowercase())
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        parts
            .into_iter()
            .flat_map(|p| p.join().unwrap_or_default())
            .collect()
    })
}

/// A new file name from `pattern`: * is the old name without extension, a run of # the number
/// `n` with that many digits; the old extension is kept.
fn pattern_name(pattern: &str, old: &Path, n: usize) -> String {
    let stem = old
        .file_stem()
        .map_or(String::new(), |s| s.to_string_lossy().into_owned());
    let (mut out, mut chars) = (String::new(), pattern.chars().peekable());
    while let Some(c) = chars.next() {
        match c {
            '*' => out += &stem,
            '#' => {
                let mut width = 1;
                while chars.next_if_eq(&'#').is_some() {
                    width += 1;
                }
                out += &format!("{n:0width$}");
            }
            c => out.push(c),
        }
    }
    match old.extension() {
        Some(ext) => format!("{out}.{}", ext.to_string_lossy()),
        None => out,
    }
}

/// Open `url` in the default browser, in the background.
fn open_url(url: &str) {
    let url = url.to_owned();
    std::thread::spawn(move || {
        let run = |cmd: &str, args: &[&str]| command(cmd).args(args).arg(&url).status();
        if !run("gio", &["open"]).is_ok_and(|s| s.success()) {
            let _ = run("xdg-open", &[]);
        }
    });
}

/// Shown in the file list: an image, or a video with the video feature and `videos` on.
fn is_listed(path: &Path, videos: bool) -> bool {
    has_ext(path, EXTS)
        || has_ext(path, raw::EXTS)
        || is_heif(path)
        || has_ext(path, JXL_EXTS)
        || has_ext(path, SVG_EXTS)
        || (videos && is_video(path))
}

/// One pass over the folder: (subfolders, image file names), both sorted by name.
/// The file type comes from the directory entry, so there is no `stat` per file (only symlinks are resolved).
/// The images in `dir` and every folder below it, as paths relative to `dir`. A folder reached
/// again through a link is read once.
fn list_tree(dir: &Path, videos: bool) -> Vec<OsString> {
    let (mut seen, mut out, mut todo) = (HashSet::new(), Vec::new(), vec![PathBuf::new()]);
    while let Some(rel) = todo.pop() {
        let full = dir.join(&rel);
        if !seen.insert(full.canonicalize().unwrap_or_else(|_| full.clone())) {
            continue;
        }
        let (dirs, files) = list_folder(&full, videos);
        out.extend(files.into_iter().map(|f| rel.join(f).into_os_string()));
        todo.extend(
            dirs.iter()
                .filter_map(|d| d.file_name())
                .map(|n| rel.join(n)),
        );
    }
    // In name order, as list_folder gives a single folder; sort_files builds on that.
    out.sort_unstable();
    out
}

fn list_folder(dir: &Path, videos: bool) -> (Vec<PathBuf>, Vec<OsString>) {
    let (mut dirs, mut images) = (Vec::new(), Vec::new());
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if is_listed(&path, videos) {
            images.push(entry.file_name());
        } else if !entry.file_name().as_encoded_bytes().starts_with(b".")
            && entry
                .file_type()
                .is_ok_and(|t| t.is_dir() || (t.is_symlink() && path.is_dir()))
        {
            dirs.push(path);
        }
    }
    // Byte-wise comparison of the whole path gives name order for paths in one folder, and is
    // ~9× faster than Path::cmp, which re-parses the path in every comparison.
    dirs.sort_unstable_by(|a, b| a.as_os_str().cmp(b.as_os_str()));
    images.sort_unstable(); // byte-wise on Unix
    (dirs, images)
}

/// File list order, as in Geeqie's sort menu.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Sort {
    Name,
    /// Numbers in names compared by value: "img2" before "img10".
    Number,
    /// Modification time.
    Date,
    Created,
    ExifOriginal,
    ExifDigitized,
    Size,
    /// File type: "jpg" before "mp4" before "png"; then by name.
    Extension,
}

/// Each order with its config value and menu label.
const SORTS: [(Sort, &str, &str); 8] = [
    (Sort::Name, "name", "Sort by name"),
    (Sort::Number, "number", "Sort by number"),
    (Sort::Date, "date", "Sort by date"),
    (Sort::Created, "created", "Sort by file creation date"),
    (
        Sort::ExifOriginal,
        "exif_original",
        "Sort by EXIF date original",
    ),
    (
        Sort::ExifDigitized,
        "exif_digitized",
        "Sort by EXIF date digitized",
    ),
    (Sort::Size, "size", "Sort by size"),
    (Sort::Extension, "extension", "Sort by extension"),
];

impl Sort {
    fn entry(self) -> (Sort, &'static str, &'static str) {
        SORTS.into_iter().find(|s| s.0 == self).unwrap_or(SORTS[0])
    }
}

/// Sort file names in `dir`; `case`: case-sensitive names ("B" before "a"). Everything but name
/// and number needs a stat (or EXIF) per file, so this runs in the listing thread; ties fall
/// back to the name.
fn sort_files(dir: &Path, files: &mut [OsString], by: Sort, ascending: bool, case: bool) {
    let name = |f: &OsString| {
        let s = f.to_string_lossy();
        if case {
            s.into_owned()
        } else {
            s.to_lowercase()
        }
    };
    let meta = |f: &OsString| std::fs::metadata(dir.join(f)).ok();
    let secs = |t: std::io::Result<std::time::SystemTime>| {
        t.ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos())
    };
    match by {
        // Files are listed byte-wise, which is case-sensitive name order.
        Sort::Name if case => {}
        Sort::Name => files.sort_by_cached_key(name),
        Sort::Number => {
            files.sort_by_cached_key(name);
            files.sort_by(|a, b| natural_cmp(&name(a), &name(b)));
        }
        Sort::Date => files.sort_by_cached_key(|f| (meta(f).map(|m| secs(m.modified())), name(f))),
        Sort::Created => files.sort_by_cached_key(|f| {
            // Not every file system records creation time; then the modification time.
            let m = meta(f);
            let t = m
                .as_ref()
                .map(|m| secs(m.created().or_else(|_| m.modified())));
            (t, name(f))
        }),
        Sort::Size => files.sort_by_cached_key(|f| (meta(f).map(|m| m.len()), name(f))),
        Sort::Extension => files.sort_by_cached_key(|f| {
            let n = name(f);
            let ext = n.rsplit_once('.').map_or("", |e| e.1).to_owned();
            (ext, n)
        }),
        Sort::ExifOriginal | Sort::ExifDigitized => {
            let digitized = by == Sort::ExifDigitized;
            // Files without the date last.
            files.sort_by_cached_key(|f| {
                let date = exif::date(&dir.join(f), digitized);
                (date.is_none(), date, name(f))
            })
        }
    }
    if !ascending {
        files.reverse();
    }
}

/// Compare with runs of digits by value, so "img2" comes before "img10".
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut a, mut b) = (a, b);
    loop {
        let (Some(ca), Some(cb)) = (a.chars().next(), b.chars().next()) else {
            return a.len().cmp(&b.len());
        };
        if ca.is_ascii_digit() && cb.is_ascii_digit() {
            let end_a = a.find(|c: char| !c.is_ascii_digit()).unwrap_or(a.len());
            let end_b = b.find(|c: char| !c.is_ascii_digit()).unwrap_or(b.len());
            let (na, nb) = (
                a[..end_a].trim_start_matches('0'),
                b[..end_b].trim_start_matches('0'),
            );
            let ord = na.len().cmp(&nb.len()).then(na.cmp(nb));
            if ord.is_ne() {
                return ord;
            }
            (a, b) = (&a[end_a..], &b[end_b..]);
        } else {
            if ca != cb {
                return ca.cmp(&cb);
            }
            (a, b) = (&a[ca.len_utf8()..], &b[cb.len_utf8()..]);
        }
    }
}

/// `$XDG_CONFIG_HOME/glimt/config`, otherwise `~/.config/glimt/config`.
fn config_path() -> Option<PathBuf> {
    xdg_file("XDG_CONFIG_HOME", ".config", "config")
}

/// `$XDG_STATE_HOME/glimt/state`, otherwise `~/.local/state/glimt/state`.
/// State (such as the last folder) is not configuration, and belongs here according to XDG.
fn state_path() -> Option<PathBuf> {
    xdg_file("XDG_STATE_HOME", ".local/state", "state")
}

fn xdg_file(var: &str, home_fallback: &str, name: &str) -> Option<PathBuf> {
    Some(xdg_dir(var, home_fallback, "glimt")?.join(name))
}

/// An external program: ffmpeg, gio, zenity, a player. Inside a Flatpak it runs on the host,
/// which has them, through `flatpak-spawn --host`.
fn command(program: &str) -> std::process::Command {
    if std::env::var_os("FLATPAK_ID").is_none() {
        return std::process::Command::new(program);
    }
    let mut cmd = std::process::Command::new("flatpak-spawn");
    cmd.args(["--host", program]);
    cmd
}

fn xdg_dir(var: &str, home_fallback: &str, app: &str) -> Option<PathBuf> {
    let base = std::env::var_os(var)
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(home_fallback)))?;
    Some(base.join(app))
}

fn parse_last_dir(state: &str) -> Option<PathBuf> {
    state_value(state, "last_dir").map(PathBuf::from)
}

fn state_value<'a>(state: &'a str, key: &str) -> Option<&'a str> {
    state.lines().find_map(|l| {
        l.strip_prefix(key)?
            .strip_prefix('=')
            .filter(|v| !v.is_empty())
    })
}

/// Window geometry saved at exit (points): size, position (not on Wayland, where apps cannot
/// see or set it) and maximized.
#[derive(Debug, Default, PartialEq)]
struct WindowState {
    size: Option<egui::Vec2>,
    pos: Option<egui::Pos2>,
    maximized: bool,
}

impl WindowState {
    fn parse(state: &str) -> Self {
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

    fn to_text(&self) -> String {
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
struct Rgb([u8; 3]);

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

impl std::str::FromStr for Sort {
    type Err = ();
    fn from_str(text: &str) -> Result<Self, ()> {
        SORTS
            .into_iter()
            .find(|s| s.1 == text)
            .map(|s| s.0)
            .ok_or(())
    }
}

impl std::fmt::Display for Sort {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(self.entry().1)
    }
}

/// The config struct, its defaults, and reading and writing it, from one list; the field
/// name is the key in the file.
macro_rules! config {
    ($($(#[$doc:meta])* $name:ident: $ty:ty = $default:expr,)*) => {
        /// Preferences, stored as `key=value` lines. Unknown or invalid lines are ignored.
        #[derive(Debug, PartialEq)]
        struct Config {
            $($(#[$doc])* $name: $ty,)*
        }

        impl Default for Config {
            fn default() -> Self {
                Config { $($name: $default,)* }
            }
        }

        impl Config {
            // String's parse cannot fail, so its `if let` is irrefutable.
            #[allow(irrefutable_let_patterns)]
            fn set(&mut self, key: &str, value: &str) {
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

            fn to_text(&self) -> String {
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
    /// Animated GIF and WebP play (A); off, the first frame is shown.
    animate: bool = true,
    /// Embedded ICC profiles are converted to sRGB.
    color_management: bool = true,
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
    /// Folders as a tree from / (Ctrl+T) instead of the subfolders of the current one.
    folder_tree: bool = false,
    /// Toolbar under the menu, as in Geeqie.
    toolbar: bool = true,
    /// One malloc arena for all threads: much less memory, decoding up to ~8 % slower.
    /// Read at startup only, since glibc must be set before threads start.
    low_memory: bool = true,
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
    /// Re-read the folder when files are added, removed or renamed.
    auto_refresh: bool = true,
    /// Rotate images by their EXIF orientation (Geeqie: "Auto rotate image using Exif information").
    exif_rotate: bool = true,
    /// Video feature: list video files.
    videos: bool = true,
    /// Video feature: command line that plays a video; empty = the default application.
    video_player: String = String::new(),
}

impl Config {
    fn parse(text: &str) -> Self {
        let mut c = Config::default();
        for (key, value) in text.lines().filter_map(|l| l.split_once('=')) {
            c.set(key.trim(), value.trim());
        }
        c.preload = c.preload.min(20);
        c.histogram = c.histogram.min(2);
        c.thumb_size = c.thumb_size.min(2);
        c.webp_quality = c.webp_quality.min(100);
        c.delay = c.delay.clamp(1, MAX_DELAY);
        c.toolbar_icons = c.toolbar_icons.min(2);
        c
    }

    fn load() -> Self {
        config_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map_or_else(Config::default, |text| Config::parse(&text))
    }

    fn save(&self) {
        if let Some(p) = config_path() {
            write_atomic(&p, &self.to_text());
        }
    }
}

/// Write to a temporary file and rename it, so the file is never half-written after a crash.
/// The process ID is in the name, so concurrent sessions never write to the same temporary file.
fn write_atomic(path: &Path, text: &str) {
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    let result = std::fs::create_dir_all(path.parent().unwrap())
        .and_then(|_| std::fs::write(&tmp, text))
        .and_then(|_| std::fs::rename(&tmp, path));
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        eprintln!("could not save {}: {e}", path.display());
    }
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
    bytes.map_or("calculating…".to_owned(), human_size)
}

/// Sum of the file sizes (one stat per file); unreadable files are not counted.
fn total_size(paths: &[PathBuf]) -> u64 {
    paths
        .iter()
        .filter_map(|p| p.metadata().ok())
        .map(|m| m.len())
        .sum()
}

/// Size as in the file manager: "850.0 kB", "2.3 MB", "1.2 GB" (decimal units).
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

/// The shuffled `old` order without files that are gone, followed by new files in random order.
fn merge_shuffled(old: &[OsString], new: Vec<OsString>) -> Vec<OsString> {
    let old_set: HashSet<&OsString> = old.iter().collect();
    let new_set: HashSet<&OsString> = new.iter().collect();
    let mut added: Vec<OsString> = new
        .iter()
        .filter(|f| !old_set.contains(f))
        .cloned()
        .collect();
    shuffle(&mut added);
    let mut out: Vec<OsString> = old
        .iter()
        .filter(|f| new_set.contains(f))
        .cloned()
        .collect();
    out.extend(added);
    out
}

/// Fisher–Yates with xorshift seeded from std's random hash key (no rand dependency).
fn shuffle<T>(v: &mut [T]) {
    use std::hash::{BuildHasher, Hasher};
    let mut x = std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish()
        | 1;
    for i in (1..v.len()).rev() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        v.swap(i, (x % (i as u64 + 1)) as usize);
    }
}

/// Image `path` scaled to fit `max`, its original size and its number of pages; `page` picks a
/// page of a multi-page TIFF.
fn decode(
    path: &Path,
    max: [u32; 2],
    rotate: bool,
    page: usize,
) -> Result<(Frames, [usize; 2], usize), String> {
    if let Some((frames, orig)) = animation(path, max) {
        return Ok((frames, orig, 1));
    }
    let pages = if has_ext(path, &["tif", "tiff"]) {
        tiff_pages(path)
    } else {
        1
    };
    let (img, icc) = if page > 0 {
        (tiff_page(path, page)?, None)
    } else {
        load_icc(path, rotate)?
    };
    let orig = [img.width() as usize, img.height() as usize];
    let img = to_srgb(shrink(img, max)?, icc.as_deref());
    Ok((vec![(color_image(img), 0)], orig, pages))
}

/// Save the view's rotation in a PNG or WebP, which have no orientation field to set: the
/// pixels are turned and the file written again with its ICC profile. PNG and lossless WebP
/// lose nothing; a lossy WebP is encoded again by ffmpeg at `webp_quality`, in sRGB as ffmpeg
/// drops the profile. The new file replaces the old in one rename.
fn save_rotated(
    path: &Path,
    rotate: bool,
    turns: u8,
    mirrored: bool,
    webp_quality: u8,
) -> Result<(), String> {
    use image::ImageEncoder;
    let png = has_ext(path, &["png"]);
    if !png && !has_ext(path, &["webp"]) {
        return Err("only JPEG, PNG and WebP can store the rotation".into());
    }
    if !png {
        let file = std::io::BufReader::new(std::fs::File::open(path).map_err(|e| e.to_string())?);
        let dec = image::codecs::webp::WebPDecoder::new(file).map_err(|e| e.to_string())?;
        if dec.has_animation() {
            return Err("animated WebP files are not turned".into());
        }
    }
    let (mut img, icc) = load_icc(path, rotate)?;
    if mirrored {
        img = img.fliph();
    }
    img = match turns % 4 {
        1 => img.rotate90(),
        2 => img.rotate180(),
        3 => img.rotate270(),
        _ => img,
    };
    let tmp = path.with_extension("glimt-tmp");
    if !png && webp_lossless(path) == Some(false) {
        let img = to_srgb(img, icc.as_deref());
        lossy_webp(&img, &tmp, webp_quality)?;
        if let Ok(meta) = std::fs::metadata(path) {
            let _ = std::fs::set_permissions(&tmp, meta.permissions());
        }
        return std::fs::rename(&tmp, path).map_err(|e| e.to_string());
    }
    let out = std::io::BufWriter::new(std::fs::File::create(&tmp).map_err(|e| e.to_string())?);
    let written = if png {
        let mut enc = image::codecs::png::PngEncoder::new(out);
        if let Some(icc) = icc {
            let _ = enc.set_icc_profile(icc);
        }
        img.write_with_encoder(enc)
    } else {
        let mut enc = image::codecs::webp::WebPEncoder::new_lossless(out);
        if let Some(icc) = icc {
            let _ = enc.set_icc_profile(icc);
        }
        // The WebP encoder takes 8-bit RGB or RGBA.
        let img = if img.color().has_alpha() {
            image::DynamicImage::from(img.into_rgba8())
        } else {
            image::DynamicImage::from(img.into_rgb8())
        };
        img.write_with_encoder(enc)
    };
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.to_string());
    }
    if let Ok(meta) = std::fs::metadata(path) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

/// Whether a WebP file is lossless (a VP8L image) or lossy (VP8); `None` if neither is found.
fn webp_lossless(path: &Path) -> Option<bool> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let mut head = [0; 12];
    file.read_exact(&mut head).ok()?;
    if &head[..4] != b"RIFF" || &head[8..] != b"WEBP" {
        return None;
    }
    // The chunks after the header: an extended file has VP8X, ICCP, ALPH … before the image.
    let mut chunk = [0; 8];
    while file.read_exact(&mut chunk).is_ok() {
        match &chunk[..4] {
            b"VP8L" => return Some(true),
            b"VP8 " => return Some(false),
            _ => {
                let size = u32::from_le_bytes(chunk[4..].try_into().ok()?) as i64;
                file.seek(SeekFrom::Current(size + size % 2)).ok()?;
            }
        }
    }
    None
}

/// Write `img` as a lossy WebP at `quality` (0–100) through ffmpeg, which has libwebp.
fn lossy_webp(img: &image::DynamicImage, to: &Path, quality: u8) -> Result<(), String> {
    use std::io::Write;
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    let mut child = command("ffmpeg")
        .args([
            "-v", "error", "-y", "-f", "png_pipe", "-i", "-", "-c:v", "libwebp",
        ])
        .args([
            "-quality",
            &quality.to_string(),
            "-frames:v",
            "1",
            "-f",
            "webp",
        ])
        .arg(to)
        .stdin(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("a lossy WebP needs ffmpeg to be saved: {e}"))?;
    let _ = child.stdin.take().map(|mut i| i.write_all(&png));
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        let _ = std::fs::remove_file(to);
        Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
    }
}

/// The number of pages (images) in a TIFF file; reads only their directories.
fn tiff_pages(path: &Path) -> usize {
    let Ok(file) = std::fs::File::open(path) else {
        return 1;
    };
    let Ok(mut dec) = tiff::decoder::Decoder::new(std::io::BufReader::new(file)) else {
        return 1;
    };
    let mut pages = 1;
    while dec.more_images() && dec.next_image().is_ok() {
        pages += 1;
    }
    pages
}

/// Page `page` of a TIFF. The image crate reads only the first, so the tiff crate (which it
/// uses) decodes the others; 8- and 16-bit grey and colour, which covers scanned documents.
fn tiff_page(path: &Path, page: usize) -> Result<image::DynamicImage, String> {
    use image::DynamicImage as D;
    use tiff::ColorType as C;
    use tiff::decoder::DecodingResult as R;
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut dec =
        tiff::decoder::Decoder::new(std::io::BufReader::new(file)).map_err(|e| e.to_string())?;
    dec.seek_to_image(page).map_err(|e| e.to_string())?;
    let (w, h) = dec.dimensions().map_err(|e| e.to_string())?;
    let color = dec.colortype().map_err(|e| e.to_string())?;
    let img = match (color, dec.read_image().map_err(|e| e.to_string())?) {
        (C::Gray(8), R::U8(v)) => image::GrayImage::from_raw(w, h, v).map(D::ImageLuma8),
        (C::GrayA(8), R::U8(v)) => image::GrayAlphaImage::from_raw(w, h, v).map(D::ImageLumaA8),
        (C::RGB(8), R::U8(v)) => image::RgbImage::from_raw(w, h, v).map(D::ImageRgb8),
        (C::RGBA(8), R::U8(v)) => image::RgbaImage::from_raw(w, h, v).map(D::ImageRgba8),
        (C::Gray(16), R::U16(v)) => image::ImageBuffer::from_raw(w, h, v).map(D::ImageLuma16),
        (C::RGB(16), R::U16(v)) => image::ImageBuffer::from_raw(w, h, v).map(D::ImageRgb16),
        (C::RGBA(16), R::U16(v)) => image::ImageBuffer::from_raw(w, h, v).map(D::ImageRgba16),
        _ => None,
    };
    img.ok_or_else(|| format!("page {} is in a colour format Glimt cannot show", page + 1))
}

/// The whole image in sRGB.
fn load(path: &Path, rotate: bool) -> Result<image::DynamicImage, String> {
    let (img, icc) = load_icc(path, rotate)?;
    Ok(to_srgb(img, icc.as_deref()))
}

/// Colours from an embedded ICC profile (Adobe RGB, Display P3 …) converted to sRGB, which is
/// what the screen is taken to show; without it such photos look dull. Done after scaling down,
/// where it costs a few milliseconds. Profiles that do not fit (CMYK, grey) leave it as it is.
fn to_srgb(img: image::DynamicImage, icc: Option<&[u8]>) -> image::DynamicImage {
    use moxcms::{ColorProfile, Layout, TransformOptions};
    if !COLOR_MANAGEMENT.load(Ordering::Relaxed) {
        return img;
    }
    let Some(from) = icc.and_then(|b| ColorProfile::new_from_slice(b).ok()) else {
        return img;
    };
    let srgb = ColorProfile::new_srgb();
    let Ok(t) = from.create_transform_8bit(
        Layout::Rgba,
        &srgb,
        Layout::Rgba,
        TransformOptions::default(),
    ) else {
        return img;
    };
    let mut rgba = img.into_rgba8();
    let mut out = vec![0; rgba.len()];
    if t.transform(&rgba, &mut out).is_ok() {
        rgba.copy_from_slice(&out);
    }
    rgba.into()
}

/// The frames of an animated GIF or WebP; `None` for other files, which `load` decodes.
fn animation(path: &Path, max: [u32; 2]) -> Option<(Frames, [usize; 2])> {
    use image::AnimationDecoder;
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let file = std::io::BufReader::new(std::fs::File::open(path).ok()?);
    let frames = match ext.as_str() {
        "gif" => image::codecs::gif::GifDecoder::new(file)
            .ok()?
            .into_frames(),
        "webp" => {
            let dec = image::codecs::webp::WebPDecoder::new(file).ok()?;
            if !dec.has_animation() {
                return None;
            }
            dec.into_frames()
        }
        _ => return None,
    };
    let (mut out, mut orig, mut bytes) = (Vec::new(), [0, 0], 0);
    for frame in frames {
        let frame = frame.ok()?;
        let (n, d) = frame.delay().numer_denom_ms();
        // Browsers show delays under 20 ms as 100 ms; many GIFs rely on it.
        let ms = Some(n / d.max(1)).filter(|&ms| ms >= 20).unwrap_or(100);
        let img = frame.into_buffer();
        orig = [img.width() as usize, img.height() as usize];
        let img = color_image(shrink(img.into(), max).ok()?);
        bytes += img.pixels.len() * 4;
        out.push((img, ms));
        if bytes > MAX_ANIMATION {
            break;
        }
    }
    (!out.is_empty()).then_some((out, orig))
}

/// The whole image (a preview frame for videos), turned upright by EXIF if `rotate`.
/// The whole image, and its ICC profile if it has one.
fn load_icc(path: &Path, rotate: bool) -> Result<(image::DynamicImage, Option<Vec<u8>>), String> {
    #[cfg(feature = "video")]
    if is_video(path) {
        return Ok((video::preview(path)?, None));
    }
    if has_ext(path, raw::EXTS) {
        return Ok((raw::load(path, rotate)?, None));
    }
    #[cfg(any(feature = "heic", feature = "avif"))]
    if is_heif(path) {
        return Ok((heif(path)?, None));
    }
    #[cfg(feature = "svg")]
    if has_ext(path, SVG_EXTS) {
        return Ok((svg(path)?, None));
    }
    #[cfg(feature = "jxl")]
    if has_ext(path, JXL_EXTS) {
        let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
        let mut dec = jxl_oxide::integration::JxlDecoder::new(std::io::BufReader::new(file))
            .map_err(|e| e.to_string())?;
        check_size(&dec)?;
        // jxl-oxide already renders the image upright.
        let icc = dec.icc_profile().ok().flatten();
        let img = image::DynamicImage::from_decoder(dec).map_err(|e| e.to_string())?;
        return Ok((img, icc));
    }
    let mut dec = image::ImageReader::open(path)
        .and_then(|r| r.with_guessed_format())
        .map_err(|e| e.to_string())?
        .into_decoder()
        .map_err(|e| e.to_string())?;
    let orientation = dec
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let icc = dec.icc_profile().ok().flatten();
    let mut img = image::DynamicImage::from_decoder(dec).map_err(|e| e.to_string())?;
    if rotate {
        img.apply_orientation(orientation);
    }
    Ok((img, icc))
}

/// The size limit `ImageReader` applies, for decoders made directly: a header that claims a
/// huge image must not make Glimt allocate gigabytes (which aborts the process).
fn check_size(dec: &impl ImageDecoder) -> Result<(), String> {
    match dec.total_bytes() {
        n if n > MAX_DECODE => Err(format!("image too large ({})", human_size(n))),
        _ => Ok(()),
    }
}

/// image's default limit (`Limits::max_alloc`).
const MAX_DECODE: u64 = 512 << 20;

/// An SVG drawn by resvg, at its own size but at least 2048 pixels on the longer side, so it
/// stays sharp when fitted to the window.
#[cfg(feature = "svg")]
fn svg(path: &Path) -> Result<image::DynamicImage, String> {
    use resvg::{tiny_skia, usvg};
    // The system fonts, for text in SVGs, are read once: it takes tens of milliseconds.
    static FONTS: std::sync::OnceLock<Arc<usvg::fontdb::Database>> = std::sync::OnceLock::new();
    let fonts = FONTS.get_or_init(|| {
        let mut db = usvg::fontdb::Database::new();
        db.load_system_fonts();
        Arc::new(db)
    });
    let options = usvg::Options {
        resources_dir: path.parent().map(Path::to_path_buf),
        fontdb: fonts.clone(),
        ..usvg::Options::default()
    };
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    let tree = usvg::Tree::from_data(&data, &options).map_err(|e| e.to_string())?;
    let size = tree.size();
    // At most 8192 pixels on the longer side: an SVG may claim any size, and the pixmap is
    // 4 bytes per pixel (60000×60000 would be 14 GB).
    let longer = size.width().max(size.height());
    let scale = (2048.0 / longer).max(1.0).min(8192.0 / longer);
    let (w, h) = (
        (size.width() * scale).ceil() as u32,
        (size.height() * scale).ceil() as u32,
    );
    let mut pixmap = tiny_skia::Pixmap::new(w.max(1), h.max(1)).ok_or("SVG too large")?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    // tiny-skia keeps premultiplied alpha; image wants it straight.
    let rgba: Vec<u8> = (pixmap.pixels().iter())
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect();
    image::RgbaImage::from_raw(w.max(1), h.max(1), rgba)
        .map(image::DynamicImage::from)
        .ok_or_else(|| "SVG size mismatch".into())
}

/// Scale down to fit within `max`.
fn shrink(img: image::DynamicImage, max: [u32; 2]) -> Result<image::DynamicImage, String> {
    if img.width() > max[0] || img.height() > max[1] {
        let scale = (max[0] as f64 / img.width() as f64).min(max[1] as f64 / img.height() as f64);
        let w = ((img.width() as f64 * scale).round() as u32).max(1);
        let h = ((img.height() as f64 * scale).round() as u32).max(1);
        let mut small = image::DynamicImage::new(w, h, img.color());
        // SIMD downscaling; image's thumbnail took ~150–330 ms on 12 MP.
        let opts = fast_image_resize::ResizeOptions::new().resize_alg(
            fast_image_resize::ResizeAlg::Convolution(fast_image_resize::FilterType::Bilinear),
        );
        fast_image_resize::Resizer::new()
            .resize(&img, &mut small, &opts)
            .map_err(|e| e.to_string())?;
        return Ok(small);
    }
    Ok(img)
}

fn color_image(img: image::DynamicImage) -> egui::ColorImage {
    let rgba = img.into_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw())
}

impl App {
    fn new(cc: &eframe::CreationContext, path: Option<PathBuf>) -> Self {
        let keep = Arc::new(Mutex::new(vec![0]));
        let full_ok = Arc::new(AtomicUsize::new(1));
        let config = Config::load();
        COLOR_MANAGEMENT.store(config.color_management, Ordering::Relaxed);
        if let Ok(mut own) = OWN_KEYS.write() {
            own.clone_from(&config.keys.0);
        }
        LARGE_THUMBS.store(config.thumb_size == 2, Ordering::Relaxed);
        let path = path
            .or_else(|| {
                let state = std::fs::read_to_string(state_path()?).ok()?;
                parse_last_dir(&state).filter(|d| config.remember_dir && d.is_dir())
            })
            .unwrap_or_else(|| ".".into());
        let ahead = Arc::new(AtomicUsize::new(config.preload));
        let rotate = Arc::new(AtomicBool::new(config.exif_rotate));
        let limits = Arc::new(Mutex::new([[1920, 1080], [8192, 8192]]));
        let (jobs, job_rx) = mpsc::channel::<(usize, bool, PathBuf, usize)>();
        let (done_tx, done) = mpsc::channel();
        let job_rx = Arc::new(Mutex::new(job_rx));
        let workers = std::thread::available_parallelism().map_or(2, |n| n.get().min(4));
        for _ in 0..workers {
            let (keep, full_ok, rotate, limits, job_rx, done_tx, ctx) = (
                keep.clone(),
                full_ok.clone(),
                rotate.clone(),
                limits.clone(),
                job_rx.clone(),
                done_tx.clone(),
                cc.egui_ctx.clone(),
            );
            std::thread::spawn(move || {
                loop {
                    let Ok((idx, full, path, page)) = job_rx.lock().unwrap().recv() else {
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
                            idx,
                            full,
                            path,
                            page,
                            result: None,
                        });
                        continue;
                    }
                    let max = limits.lock().unwrap()[full as usize];
                    let rotated = rotate.load(Ordering::Relaxed);
                    // The histogram here too, so the UI thread never walks the pixels.
                    let result = Some(decode(&path, max, rotated, page).map(
                        |(frames, orig, pages)| {
                            let hist = histogram(&frames[0].0);
                            (frames, orig, hist, pages)
                        },
                    ));
                    let _ = done_tx.send(Decoded {
                        rotated,
                        idx,
                        full,
                        path,
                        page,
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
        // Thumbnails have their own workers, so scrolling the list never delays the shown image.
        let (thumb_jobs, thumb_rx) = mpsc::channel::<(usize, PathBuf)>();
        let (thumbs_tx, thumbs_done) = mpsc::channel();
        let thumb_rx = Arc::new(Mutex::new(thumb_rx));
        let thumb_view = Arc::new([AtomicUsize::new(0), AtomicUsize::new(0)]);
        let save_thumbs = Arc::new(AtomicBool::new(config.save_thumbnails));
        for _ in 0..2 {
            let (rx, tx, view, rotate, save, ctx) = (
                thumb_rx.clone(),
                thumbs_tx.clone(),
                thumb_view.clone(),
                rotate.clone(),
                save_thumbs.clone(),
                cc.egui_ctx.clone(),
            );
            std::thread::spawn(move || {
                loop {
                    let Ok((idx, path)) = rx.lock().unwrap().recv() else {
                        return;
                    };
                    let [first, last] = [0, 1].map(|i| view[i].load(Ordering::Relaxed));
                    let result =
                        (idx + THUMB_MARGIN >= first && idx <= last + THUMB_MARGIN).then(|| {
                            let (rotate, save) =
                                (rotate.load(Ordering::Relaxed), save.load(Ordering::Relaxed));
                            let large = LARGE_THUMBS.load(Ordering::Relaxed);
                            thumbs::thumbnail(&path, rotate, save, large)
                                .ok()
                                .map(|img| color_image(img.into()))
                        });
                    let _ = tx.send((path, result));
                    // SAFETY: as for the image workers above.
                    unsafe { malloc_trim(0) };
                    ctx.request_repaint();
                }
            });
        }
        // One EXIF reader that skips to the newest request, so fast browsing never queues up.
        let (exif_jobs, exif_rx) = mpsc::channel::<PathBuf>();
        let (exif_tx, exif_done) = mpsc::channel();
        let ctx = cc.egui_ctx.clone();
        std::thread::spawn(move || {
            while let Ok(mut path) = exif_rx.recv() {
                path = exif_rx.try_iter().last().unwrap_or(path);
                let size = std::fs::metadata(&path).map_or(0, |m| m.len());
                let mut rows = vec![("File size", human_size(size))];
                rows.extend(exif::read(&path));
                let all = exif::all(&path);
                if exif_tx.send((path, rows, all)).is_err() {
                    return;
                }
                ctx.request_repaint();
            }
        });
        let (notices_tx, notices) = mpsc::channel();
        let (tree_tx, tree_rx) = mpsc::channel();
        let (remote_tx, remote_rx) = mpsc::channel();
        if config.remote_control {
            remote::listen(remote_tx, cc.egui_ctx.clone());
        }
        let (freed_tx, freed_rx) = mpsc::channel();
        let (deleted_tx, deleted_rx) = mpsc::channel();
        let (changed_tx, changed_rx) = mpsc::channel();
        let mut app = App {
            notices_tx,
            mounting: None,
            login: None,
            grid_cols: 1,
            compare: false,
            capturing: None,
            recursive: false,
            whole_shuffle: false,
            #[cfg(feature = "marks")]
            marks: marks::Marks::load(),
            tree: HashMap::new(),
            tree_tx,
            tree_rx,
            tree_shown: PathBuf::new(),
            clip: None,
            clipboard: None,
            remote: remote_rx,
            has_focus: true,
            apps: HashMap::new(),
            row_step: 1,
            notices,
            notice: None,
            dir: PathBuf::new(),
            subdirs: Vec::new(),
            files: Vec::new(),
            cur: 0,
            keep,
            full_ok,
            ahead,
            rotate,
            show_settings: false,
            help: None,
            confirm: None,
            prompt: None,
            selected: Default::default(),
            last_drawn: None,
            last_scale: 1.0,
            wheel: 0.0,
            config,
            slideshow: None,
            random: false,
            limits,
            jobs,
            page: 0,
            done,
            slots: HashMap::new(),
            pending: HashSet::new(),
            errors: HashMap::new(),
            zoom: None,
            pan: egui::Vec2::ZERO,
            started: Some(Instant::now()),
            dialog: None,
            hide_tools: false,
            watching: Arc::new(AtomicUsize::new(0)),
            changed_tx,
            changed_rx,
            stale: false,
            paused: false,
            back: Vec::new(),
            forward: Vec::new(),
            listing: None,
            total_size: 0,
            size_rx: None,
            freed_tx,
            freed_rx,
            selected_size: None,
            selected_rx: None,
            scroll_to_cur: true,
            thumbs: HashMap::new(),
            thumbs_pending: HashSet::new(),
            thumb_jobs,
            thumbs_done,
            thumb_view,
            save_thumbs,
            exif_jobs,
            exif_done,
            exif_all: Vec::new(),
            exif_window: false,
            exif_asked: None,
            exif: None,
            cur_size: None,
            path_text: String::new(),
            window: WindowState::default(),
            inhibitor: None,
            details: HashMap::new(),
            deleting: HashSet::new(),
            deleted_tx,
            deleted_rx,
            busy: Arc::new(AtomicUsize::new(0)),
            search: String::new(),
            matches: None,
            match_size: 0,
            match_rx: None,
            exif_index: None,
            exif_rx: None,
            anchor: None,
            orient: (0, false),
            opened: Instant::now(),
            max_pan: egui::Vec2::ZERO,
        };
        // A URI on the command line: start where Glimt would otherwise, then go there once mounted.
        match path.to_str().filter(|p| p.contains("://")) {
            Some(uri) => {
                app.open(&cc.egui_ctx, ".".into());
                app.mount(&cc.egui_ctx, uri.to_owned());
            }
            None => app.open(&cc.egui_ctx, path),
        }
        // Small folders are listed before the first frame; large ones hold startup back at most 50 ms.
        app.receive_listing(&cc.egui_ctx, std::time::Duration::from_millis(50));
        app
    }

    /// Open a folder, or the folder of a file with that file shown.
    fn open(&mut self, ctx: &egui::Context, path: PathBuf) {
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
        self.subdirs.clear();
        self.deleting.clear();
        self.random = false;
        self.recursive = self.config.show_subfolders;
        self.thumbs.clear();
        self.thumbs_pending.clear();
        self.exif_index = None;
        self.exif_rx = None;
        self.dir = dir.clone();
        self.replace_files(ctx, shown.iter().cloned().collect(), shown);
        self.list(ctx);
        self.watch(ctx);
        self.title(ctx);
    }

    /// Read the folder (and then the sizes) in the background; `receive_listing` merges it in.
    fn list(&mut self, ctx: &egui::Context) {
        let (tx, rx) = mpsc::channel();
        let (size_tx, size_rx) = mpsc::channel();
        let (list_dir, ctx2, videos) = (self.dir.clone(), ctx.clone(), self.config.videos);
        let c = &self.config;
        let (by, ascending, case) = (c.sort, c.sort_ascending, c.sort_case);
        let (recursive, shuffled) = (self.recursive, self.whole_shuffle);
        std::thread::spawn(move || {
            let (dirs, mut files) = list_folder(&list_dir, videos);
            if recursive {
                files = list_tree(&list_dir, videos);
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
            if recursive {
                let tree = list_tree(&list_dir, videos);
                let sizes = tree
                    .iter()
                    .filter_map(|f| Some(list_dir.join(f).metadata().ok()?.len()));
                return send_sizes(sizes, size_tx, &ctx2);
            }
            let images = std::fs::read_dir(&list_dir)
                .into_iter()
                .flatten()
                .flatten()
                .filter(|e| is_listed(&e.path(), videos));
            let sizes = images.filter_map(|e| Some(e.metadata().ok()?.len()));
            send_sizes(sizes, size_tx, &ctx2);
        });
        self.listing = Some(rx);
        self.size_rx = Some(size_rx);
        self.total_size = 0;
        self.stale = false;
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

    /// The file shown, or the folder when it has no images.
    fn location(&self) -> PathBuf {
        match self.files.get(self.cur) {
            Some(name) => self.dir.join(name),
            None => self.dir.clone(),
        }
    }

    /// Open `path` as a new step in the Back/Forward history.
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
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = gio_mount(&login.uri, answer.then_some(&login));
            let _ = tx.send((result, login));
            ctx.request_repaint();
        });
        self.mounting = Some(rx);
    }

    fn visit(&mut self, ctx: &egui::Context, path: PathBuf) {
        self.back.push(self.location());
        self.forward.clear();
        self.open(ctx, path);
    }

    /// Take the folder listing if it is ready, waiting up to `wait`.
    fn receive_listing(&mut self, ctx: &egui::Context, wait: std::time::Duration) {
        let Some(Ok((dirs, files))) = self.listing.as_ref().map(|rx| rx.recv_timeout(wait)) else {
            return;
        };
        self.listing = None;
        self.tree.insert(self.dir.clone(), Some(dirs.clone()));
        self.subdirs = dirs;
        // Files still being deleted in the background must not come back with a refresh.
        let mut files = files;
        if !self.deleting.is_empty() {
            files.retain(|f| !self.deleting.contains(f));
        }
        // A refresh during random order keeps the order, so Previous still goes back
        // through the images that were shown; a list shuffled whole in the thread stays as it is.
        let files = if self.random && !std::mem::take(&mut self.whole_shuffle) {
            merge_shuffled(&self.files, files)
        } else {
            files
        };
        let shown = self.files.get(self.cur).cloned();
        self.replace_files(ctx, files, shown);
    }

    /// To the trash at once, permanently after confirmation.
    fn request_delete(&mut self, ctx: &egui::Context, trash: bool) {
        if self.files.is_empty() {
            return;
        }
        if if trash {
            self.config.confirm_trash
        } else {
            self.config.confirm_delete
        } {
            self.confirm = Some(trash);
        } else {
            self.delete_targets_now(ctx, trash);
        }
    }

    /// File operations act on the selected files, otherwise on the shown image. Sorted.
    fn targets(&self) -> Vec<usize> {
        if self.selected.is_empty() {
            vec![self.cur]
        } else {
            self.selected.iter().copied().collect()
        }
    }

    fn delete_targets_now(&mut self, ctx: &egui::Context, trash: bool) {
        let targets = self.targets();
        let paths: Vec<PathBuf> = targets.iter().map(|&i| self.path(i)).collect();
        self.deleting
            .extend(targets.iter().map(|&i| self.files[i].clone()));
        self.remove_indices(ctx, &targets);
        let (tx, ctx) = (self.notices_tx.clone(), ctx.clone());
        let (freed_tx, deleted_tx) = (self.freed_tx.clone(), self.deleted_tx.clone());
        let busy = self.busy.clone();
        busy.fetch_add(1, Ordering::Relaxed);
        std::thread::spawn(move || {
            // Read the size before deleting, so the status bar can subtract it.
            let _ = freed_tx.send(total_size(&paths));
            ctx.request_repaint();
            let mut errors = Vec::new();
            if trash {
                // Many files per gio call, so thousands of files do not become thousands of processes.
                for chunk in paths.chunks(500) {
                    match command("gio").arg("trash").arg("--").args(chunk).output() {
                        Ok(o) if o.status.success() => {}
                        Ok(o) => errors.push(String::from_utf8_lossy(&o.stderr).trim().to_owned()),
                        Err(e) => {
                            errors.push(format!("gio trash (from glib): {e}"));
                            break;
                        }
                    }
                }
            } else {
                for path in &paths {
                    if let Err(e) = std::fs::remove_file(path) {
                        errors.push(format!("{}: {e}", path.display()));
                    }
                }
            }
            if let Some(first) = errors.first() {
                let _ = tx.send(format!("Delete failed ({} errors): {first}", errors.len()));
            }
            let _ = deleted_tx.send(paths);
            busy.fetch_sub(1, Ordering::Relaxed);
            ctx.request_repaint();
        });
    }

    /// Remove the (sorted) indices from the list in one pass and shift index-based state
    /// accordingly. Neighbours' textures are kept, so the next image shows without decoding again.
    fn remove_indices(&mut self, ctx: &egui::Context, removed: &[usize]) {
        let mut idx = 0;
        self.files.retain(|_| {
            idx += 1;
            removed.binary_search(&(idx - 1)).is_err()
        });
        self.slots = without_indices(std::mem::take(&mut self.slots), removed);
        self.errors = without_indices(std::mem::take(&mut self.errors), removed);
        self.selected.clear();
        self.anchor = None;
        self.update_search(ctx);
        // Jobs in flight have old indices and are rejected by path; refill requests them again.
        self.pending.clear();
        // The next remaining file after the shown one (or the last).
        let cur = self.cur - removed.partition_point(|&r| r < self.cur);
        if self.files.is_empty() {
            self.cur = 0;
            self.title(ctx);
        } else {
            self.go(ctx, cur.min(self.files.len() - 1));
        }
    }

    fn receive_deleted(&mut self) {
        for paths in self.deleted_rx.try_iter() {
            for p in paths.iter().filter(|p| p.parent() == Some(&self.dir)) {
                self.deleting.remove(p.file_name().unwrap_or_default());
            }
        }
    }

    fn receive_sizes(&mut self, ctx: &egui::Context) {
        while let Ok(freed) = self.freed_rx.try_recv() {
            // ponytail: deleting while the sum is still being computed can make it slightly off.
            self.total_size = self.total_size.saturating_sub(freed);
        }
        if let Some(Ok(size)) = self.selected_rx.as_ref().map(mpsc::Receiver::try_recv) {
            self.selected_size = Some(size);
            self.selected_rx = None;
        }
        drain(&mut self.match_rx, &mut self.match_size);
        if let Some(index) = self.exif_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.exif_rx = None;
            self.exif_index = Some(index);
            self.update_search(ctx);
        }
        drain(&mut self.size_rx, &mut self.total_size);
    }

    /// Called when the selection changes: update the title and compute the size in the background.
    fn selection_changed(&mut self, ctx: &egui::Context) {
        self.title(ctx);
        if self.compare {
            self.refill();
        }
        self.selected_size = None;
        self.selected_rx = None;
        if self.selected.is_empty() {
            return;
        }
        let paths: Vec<PathBuf> = self.selected.iter().map(|&i| self.path(i)).collect();
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            // The receiver is gone if the selection changed again; then the answer is dropped.
            if tx.send(total_size(&paths)).is_ok() {
                ctx.request_repaint();
            }
        });
        self.selected_rx = Some(rx);
    }

    /// Replace the file list and go to `shown` (otherwise the first image). The texture shown now
    /// is kept if it was made from the same file. All index-based state is reset; jobs in flight
    /// are rejected by path when they arrive.
    fn replace_files(
        &mut self,
        ctx: &egui::Context,
        files: Vec<OsString>,
        shown: Option<OsString>,
    ) {
        let slot = self.slots.remove(&self.cur);
        self.files = files;
        self.details.clear();
        self.anchor = None;
        self.selected.clear();
        self.slots.clear();
        self.pending.clear();
        self.errors.clear();
        let cur = shown
            .and_then(|p| self.files.iter().position(|f| *f == p))
            .unwrap_or(0);
        if let Some(slot) = slot.filter(|s| self.is_file(cur, &s.path)) {
            self.slots.insert(cur, slot);
        }
        self.update_search(ctx);
        self.go(ctx, cur);
    }

    /// Run zenity's (or kdialog's) file dialog in its own thread so the window does not freeze.
    fn pick(&mut self, ctx: &egui::Context, folder: bool, purpose: Pick) {
        use std::os::unix::ffi::OsStringExt;
        let (tx, rx) = mpsc::channel();
        let (ctx, start) = (ctx.clone(), self.dir.join(""));
        let title = match purpose {
            Pick::Open if folder => "Open Folder",
            Pick::Open => "Open File",
            Pick::CopyTo(_) => "Copy to Folder",
            Pick::MoveTo(_) => "Move to Folder",
        };
        let notices = self.notices_tx.clone();
        std::thread::spawn(move || {
            let zenity = || {
                let mut cmd = command("zenity");
                cmd.arg("--file-selection").arg("--title").arg(title);
                cmd.arg("--filename").arg(&start);
                if folder {
                    cmd.arg("--directory");
                }
                cmd.output()
            };
            // KDE desktops often have kdialog but not zenity.
            let kdialog = || {
                let mode = if folder {
                    "--getexistingdirectory"
                } else {
                    "--getopenfilename"
                };
                let mut cmd = command("kdialog");
                cmd.arg("--title").arg(title).arg(mode).arg(&start);
                cmd.output()
            };
            let output = match zenity() {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => kdialog(),
                other => other,
            };
            let picked = match output {
                Err(e) => {
                    let _ =
                        notices.send(format!("No file dialog (install zenity or kdialog): {e}"));
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
        self.dialog = Some((purpose, rx));
    }

    /// Copy or move `paths` into `dest` in the background. Existing files are never
    /// overwritten; a move across file systems falls back to copy + delete.
    fn transfer(&mut self, ctx: &egui::Context, paths: Vec<PathBuf>, dest: PathBuf, keep: bool) {
        if !keep {
            // Moved out of the shown folder: out of the list at once, as when deleting.
            let names: HashSet<&std::ffi::OsStr> = paths
                .iter()
                .filter(|p| p.parent() == Some(self.dir.as_path()))
                .filter_map(|p| p.file_name())
                .collect();
            let gone: Vec<usize> = (0..self.files.len())
                .filter(|&i| names.contains(self.files[i].as_os_str()))
                .collect();
            if dest != self.dir {
                self.remove_indices(ctx, &gone);
            }
        }
        let (tx, freed_tx, ctx) = (self.notices_tx.clone(), self.freed_tx.clone(), ctx.clone());
        let same_dir = paths.first().and_then(|p| p.parent()) == Some(self.dir.as_path());
        let busy = self.busy.clone();
        busy.fetch_add(1, Ordering::Relaxed);
        std::thread::spawn(move || {
            if !keep && same_dir {
                let _ = freed_tx.send(total_size(&paths));
            }
            let mut errors = Vec::new();
            for path in &paths {
                let target = dest.join(path.file_name().unwrap_or_default());
                let result = if target.exists() {
                    Err(std::io::ErrorKind::AlreadyExists.into())
                } else if keep {
                    std::fs::copy(path, &target).map(drop)
                } else {
                    std::fs::rename(path, &target).or_else(|e| {
                        if e.kind() != std::io::ErrorKind::CrossesDevices {
                            return Err(e);
                        }
                        std::fs::copy(path, &target).and_then(|_| std::fs::remove_file(path))
                    })
                };
                if let Err(e) = result {
                    errors.push(format!("{}: {e}", target.display()));
                }
            }
            if let Some(first) = errors.first() {
                let verb = if keep { "Copy" } else { "Move" };
                let _ = tx.send(format!("{verb} failed ({} errors): {first}", errors.len()));
            }
            busy.fetch_sub(1, Ordering::Relaxed);
            ctx.request_repaint();
        });
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

    fn request(&mut self, idx: usize, full: bool) {
        let have = self.slots.get(&idx).is_some_and(|s| s.full || !full);
        if !have && !self.errors.contains_key(&idx) && self.pending.insert((idx, full)) {
            // The neighbours are preloaded at their first page.
            let page = if idx == self.cur { self.page } else { 0 };
            let _ = self.jobs.send((idx, full, self.path(idx), page));
        }
    }

    fn go(&mut self, ctx: &egui::Context, idx: usize) {
        if self.files.is_empty() {
            return;
        }
        // A TIFF shown at another page than the first: its decoded page must not stay for when
        // it is shown again from the first page.
        if self.page > 0 {
            self.slots.remove(&self.cur);
            self.page = 0;
        }
        // Saving the old image's rotation may leave a notice, which must outlive the move.
        let saved = self.config.save_orientation && self.orient != (0, false);
        if saved {
            self.save_orientation();
        }
        let notice = if saved { self.notice.take() } else { None };
        self.cur = idx.min(self.files.len() - 1);
        self.notice = notice;
        // Geeqie's "keep zoom": the next image at the same zoom and place, to compare a series.
        if !self.config.keep_zoom {
            self.zoom = None;
            self.pan = egui::Vec2::ZERO;
        }
        self.orient = (0, false);
        self.opened = Instant::now();
        self.scroll_to_cur = true;
        // Every image gets the full delay, also when the user moves back or forward by hand.
        if self.slideshow.is_some() {
            self.slideshow = Some(Instant::now());
        }
        self.refill();
        self.title(ctx);
    }

    /// Shuffle the file list (current image first) or sort it back, keeping the shown image.
    fn set_random(&mut self, ctx: &egui::Context, on: bool) {
        if self.files.is_empty() {
            return;
        }
        self.random = on;
        if !on {
            // Back to the chosen sort order; listing keeps the shown image.
            self.list(ctx);
            return;
        }
        let mut files = std::mem::take(&mut self.files);
        let shown = files[self.cur].clone();
        files.swap(0, self.cur);
        shuffle(&mut files[1..]);
        self.replace_files(ctx, files, Some(shown));
    }

    fn start_slideshow(&mut self, ctx: &egui::Context) {
        if self.config.slideshow_subfolders {
            // The images of the subfolders too, listed in the background; random order
            // shuffles them all together.
            self.recursive = true;
            self.random = self.config.random;
            self.whole_shuffle = self.random;
            self.list(ctx);
        } else if self.config.random != self.random {
            self.set_random(ctx, self.config.random);
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

    /// Open the shown file in `player` (a command line), or in the default application when
    /// `player` is empty. Runs in its own thread, which waits for the program to exit.
    fn launch(&self, player: &str) {
        if self.files.is_empty() {
            return;
        }
        let (path, tx) = (self.path(self.cur), self.notices_tx.clone());
        let mut words: Vec<String> = player.split_whitespace().map(String::from).collect();
        std::thread::spawn(move || {
            let run = |cmd: &str, args: &[String]| {
                let out = command(cmd).args(args).arg(&path).output();
                out.map_err(|e| format!("{cmd}: {e}"))
            };
            let out = if words.is_empty() {
                // gio is part of glib; xdg-open covers systems without it.
                run("gio", &["open".into()]).or_else(|_| run("xdg-open", &[]))
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
        ui.menu_button("Open With", |ui| {
            let apps = self.apps.entry(ext).or_insert_with(|| apps_for(&path));
            if apps.is_empty() {
                ui.label("No applications found");
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
                        Err(e) => drop(tx.send(format!("gio: {e}"))),
                    }
                });
            }
        });
    }

    /// Preferences or a modal dialog is open: keys go there.
    fn dialog_open(&self) -> bool {
        self.show_settings
            || self.confirm.is_some()
            || self.prompt.is_some()
            || self.login.is_some()
    }

    /// The text dialog was answered with `text`.
    fn answer_prompt(&mut self, ctx: &egui::Context, prompt: Prompt, text: &str) {
        if text.is_empty() || text.contains('/') || text == "." || text == ".." {
            self.notice = Some(format!("Not a valid name: \"{text}\""));
            return;
        }
        let target = self.dir.join(text);
        let result = match prompt {
            Prompt::NewFolder => {
                self.tree.remove(&self.dir);
                std::fs::create_dir(&target)
            }
            Prompt::Rename if self.files.get(self.cur) == Some(&OsString::from(text)) => return,
            // rename() would silently replace an existing file.
            Prompt::Rename if target.exists() => Err(std::io::ErrorKind::AlreadyExists.into()),
            Prompt::Rename if self.files.is_empty() => return,
            Prompt::RenameMany => return self.rename_many(ctx, text),
            Prompt::Rename => std::fs::rename(self.path(self.cur), &target),
        };
        match (result, prompt) {
            (Ok(()), Prompt::NewFolder) => self.list(ctx),
            (Ok(()), _) => self.open(ctx, target),
            (Err(e), _) => self.notice = Some(format!("{}: {e}", target.display())),
        }
    }

    /// Write the view's rotation into the shown JPEG as its EXIF orientation and show the file
    /// as it is now. ponytail: in the UI thread; a JPEG is read and written in milliseconds.
    fn save_orientation(&mut self) {
        let (turns, mirrored) = self.orient;
        if self.files.is_empty() || (turns, mirrored) == (0, false) {
            return;
        }
        let path = self.path(self.cur);
        // JPEG keeps its pixels and gets an orientation field; PNG and WebP are turned.
        let jpeg = has_ext(&path, &["jpg", "jpeg"]);
        let saved = if jpeg {
            exif::write_orientation(&path, turns, mirrored)
        } else {
            let quality = self.config.webp_quality;
            save_rotated(&path, self.config.exif_rotate, turns, mirrored, quality)
        };
        if let Err(e) = saved {
            self.notice = Some(format!("Could not save the orientation: {e}"));
            return;
        }
        if jpeg && !self.config.exif_rotate {
            self.notice = Some("Saved; it shows when Auto Rotate Using EXIF is on".into());
        }
        // Decode again with the new orientation, and make a new thumbnail.
        self.orient = (0, false);
        self.slots.remove(&self.cur);
        self.pending.retain(|p| p.0 != self.cur);
        self.thumbs.remove(&self.files[self.cur]);
        self.thumbs_pending.remove(&self.files[self.cur]);
        self.exif = None;
        self.refill();
    }

    /// The thumbnail box in the file list, by the size chosen in Preferences.
    fn thumb(&self) -> egui::Vec2 {
        THUMBS[self.config.thumb_size.min(2) as usize]
    }

    /// A cell of the icon grid: the thumbnail with its name below.
    fn cell(&self) -> egui::Vec2 {
        self.thumb() + CELL_EXTRA
    }

    fn set_thumb_size(&mut self, size: u8) {
        self.config.thumb_size = size;
        self.config.save();
        LARGE_THUMBS.store(size == 2, Ordering::Relaxed);
        self.thumbs.clear();
        self.thumbs_pending.clear();
        self.scroll_to_cur = true;
    }

    /// Rename the selected files by `pattern`, numbered in list order. Nothing is overwritten:
    /// if a new name is taken by a file outside the selection, nothing is renamed. When new
    /// names swap with old ones, the files go through temporary names.
    fn rename_many(&mut self, ctx: &egui::Context, pattern: &str) {
        let from: Vec<PathBuf> = self.targets().iter().map(|&i| self.path(i)).collect();
        let to: Vec<PathBuf> = (from.iter().enumerate())
            .map(|(k, p)| p.with_file_name(pattern_name(pattern, p, k + 1)))
            .collect();
        let unique: HashSet<&PathBuf> = to.iter().collect();
        if unique.len() < to.len() {
            self.notice =
                Some("The pattern gives several files the same name; add # for a number".into());
            return;
        }
        let sources: HashSet<&PathBuf> = from.iter().collect();
        if let Some(taken) = to.iter().find(|t| t.exists() && !sources.contains(t)) {
            self.notice = Some(format!(
                "{} exists already; nothing renamed",
                taken.display()
            ));
            return;
        }
        let swap = to.iter().any(|t| sources.contains(t));
        let tmp = |k: usize| {
            self.dir
                .join(format!(".glimt-rename-{}-{k}", std::process::id()))
        };
        let mut moves: Vec<(PathBuf, PathBuf)> = Vec::new();
        if swap {
            moves.extend(from.iter().enumerate().map(|(k, f)| (f.clone(), tmp(k))));
            moves.extend(to.iter().enumerate().map(|(k, t)| (tmp(k), t.clone())));
        } else {
            moves.extend(from.iter().cloned().zip(to.iter().cloned()));
        }
        for (a, b) in &moves {
            if let Err(e) = std::fs::rename(a, b) {
                self.notice = Some(format!("{}: {e}", a.display()));
                break;
            }
        }
        // Show the shown image under its new name.
        let shown = from.iter().position(|f| *f == self.path(self.cur));
        self.selected.clear();
        self.selection_changed(ctx);
        match shown {
            Some(k) => self.open(ctx, to[k].clone()),
            None => self.list(ctx),
        }
    }

    /// Decode everything again with or without EXIF rotation.
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
        self.list(ctx);
    }

    /// Preferences → Keyboard shortcuts: every action with a key, which can be changed (the next
    /// key press) or reset to Geeqie's.
    fn key_settings(&mut self, ui: &mut egui::Ui) {
        if let Some(action) = self.capturing {
            let pressed = ui.input(|i| {
                use egui::Key as K;
                i.events.iter().find_map(|e| match e {
                    // A modifier on its own is not the key; wait for the one pressed with it.
                    egui::Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } if !matches!(
                        key,
                        K::ShiftLeft
                            | K::ShiftRight
                            | K::ControlLeft
                            | K::ControlRight
                            | K::AltLeft
                            | K::AltRight
                            | K::SuperLeft
                            | K::SuperRight
                    ) =>
                    {
                        Some((*key, *modifiers))
                    }
                    _ => None,
                })
            });
            if let Some((key, mods)) = pressed {
                self.capturing = None;
                if key != egui::Key::Escape {
                    let mods = egui::Modifiers {
                        mac_cmd: false,
                        ..mods
                    };
                    self.config.keys.0.retain(|k| k.0 != action);
                    self.config.keys.0.push((action, mods, key));
                    self.config.save();
                    if let Ok(mut own) = OWN_KEYS.write() {
                        own.clone_from(&self.config.keys.0);
                    }
                }
            }
        }
        let all = shortcuts();
        let mut actions: Vec<Action> = Vec::new();
        for s in SHORTCUTS {
            if !actions.contains(&s.0) && (s.0 != Action::Play || cfg!(feature = "video")) {
                actions.push(s.0);
            }
        }
        egui::Grid::new("keys").striped(true).show(ui, |ui| {
            for action in actions {
                ui.label(action.label());
                let keys: Vec<String> = (all.iter().filter(|s| s.0 == action))
                    .map(|s| {
                        ui.ctx()
                            .format_shortcut(&egui::KeyboardShortcut::new(s.1, s.2))
                    })
                    .collect();
                if self.capturing == Some(action) {
                    ui.strong("Press a key (Esc cancels)");
                } else {
                    ui.label(keys.join(", "));
                }
                ui.horizontal(|ui| {
                    if ui.small_button("Change").clicked() {
                        self.capturing = Some(action);
                    }
                    let own = self.config.keys.0.iter().any(|k| k.0 == action);
                    if own && ui.small_button("Reset").clicked() {
                        self.config.keys.0.retain(|k| k.0 != action);
                        self.config.save();
                        if let Ok(mut o) = OWN_KEYS.write() {
                            o.clone_from(&self.config.keys.0);
                        }
                    }
                });
                ui.end_row();
            }
        });
    }

    /// Decode everything again with or without colour management.
    fn set_color_management(&mut self, on: bool) {
        self.config.color_management = on;
        self.config.save();
        COLOR_MANAGEMENT.store(on, Ordering::Relaxed);
        self.slots.clear();
        self.pending.clear();
        self.thumbs.clear();
        self.thumbs_pending.clear();
        self.last_drawn = None;
        self.refill();
    }

    fn set_exif_rotate(&mut self, on: bool) {
        self.config.exif_rotate = on;
        self.config.save();
        self.rotate.store(on, Ordering::Relaxed);
        self.slots.clear();
        self.pending.clear();
        self.errors.clear();
        self.last_drawn = None;
        self.refill();
    }

    /// Random order changed in the menu or preferences: reshuffle now if a slideshow runs.
    fn set_random_order(&mut self, ctx: &egui::Context, on: bool) {
        self.config.random = on;
        self.config.save();
        if self.slideshow.is_some() {
            self.set_random(ctx, on);
            self.title(ctx);
        }
    }

    /// A random order ends with the slideshow: the list goes back to the chosen sort order,
    /// keeping the shown image.
    fn stop_slideshow(&mut self, ctx: &egui::Context) {
        self.slideshow = None;
        self.paused = false;
        if self.recursive != self.config.show_subfolders {
            self.recursive = self.config.show_subfolders;
            self.random = false;
            self.list(ctx);
        } else if self.random {
            self.set_random(ctx, false);
        }
        self.title(ctx);
    }

    /// "image name - Glimt" (or "Glimt - image name", Preferences); the folder when it has no images.
    fn title(&self, ctx: &egui::Context) {
        let name = match self.files.get(self.cur) {
            Some(file) => file.to_string_lossy().into_owned(),
            None => self.dir.display().to_string(),
        };
        let title = if self.config.app_name_first {
            format!("Glimt - {name}")
        } else {
            format!("{name} - Glimt")
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
    }

    /// Switch image when the time is up. Waits for a preloaded image that is still
    /// decoding, so the slideshow never shows a black screen.
    fn slideshow_tick(&mut self, ctx: &egui::Context) {
        let Some(last) = self.slideshow else { return };
        if self.files.is_empty() || self.paused {
            return;
        }
        let Some(next) = self.step(self.cur, 1, true) else {
            return;
        };
        let first = self.step(self.cur, -isize::MAX / 2, false);
        let delay = std::time::Duration::from_secs(self.config.delay);
        let left = delay.saturating_sub(last.elapsed());
        if !left.is_zero() {
            ctx.request_repaint_after(left);
        } else if Some(next) == first && !self.config.slideshow_repeat {
            self.stop_slideshow(ctx);
        } else if self.random && Some(next) == first && self.files.len() > 2 {
            // New round: shuffle again instead of repeating the same order.
            self.set_random(ctx, true);
        } else if !self.pending.contains(&(next, false)) {
            self.go(ctx, next);
            self.slideshow = Some(Instant::now());
        }
    }

    /// Drop textures outside the preload window, and full-resolution versions of images other
    /// than the shown one; request the missing ones.
    /// The image shown next to the current one when comparing: the other of two selected
    /// files, else the next image.
    fn compare_other(&self) -> Option<usize> {
        if !self.compare {
            return None;
        }
        match self.selected.iter().collect::<Vec<_>>()[..] {
            [&a, &b] => Some(if a == self.cur { b } else { a }),
            _ => self.step(self.cur, 1, false).filter(|&i| i != self.cur),
        }
    }

    fn refill(&mut self) {
        if self.files.is_empty() {
            return;
        }
        // The shown image first, then its neighbours in the order the list shows.
        let (cur, ahead) = (self.cur, self.ahead.load(Ordering::Relaxed) as isize);
        let mut window = vec![cur];
        // The compared image second: it may be loaded at full resolution too.
        let other = self.compare_other().filter(|&i| i != cur);
        window.extend(other);
        self.full_ok.store(window.len(), Ordering::Relaxed);
        for d in (1..=ahead).flat_map(|d| [d, -d]) {
            if let Some(i) = self.step(cur, d, false).filter(|i| !window.contains(i)) {
                window.push(i);
            }
        }
        self.slots
            .retain(|i, s| window.contains(i) && (!s.full || *i == cur || Some(*i) == other));
        *self.keep.lock().unwrap() = window.clone();
        for i in window {
            self.request(i, false);
        }
    }

    fn receive(&mut self, ctx: &egui::Context) {
        while let Ok(d) = self.done.try_recv() {
            self.pending.remove(&(d.idx, d.full));
            let wanted = self.is_file(d.idx, &d.path)
                && d.rotated == self.config.exif_rotate
                && d.page == if d.idx == self.cur { self.page } else { 0 }
                && self.keep.lock().unwrap().contains(&d.idx)
                && (!d.full || d.idx == self.cur || Some(d.idx) == self.compare_other());
            match d.result {
                Some(Ok((frames, orig, hist, pages))) if wanted => {
                    // A screen-size version must not replace a full-resolution one already loaded.
                    if !d.full && self.slots.get(&d.idx).is_some_and(|s| s.full) {
                        continue;
                    }
                    let mut anim: Vec<_> = (frames.into_iter().enumerate())
                        .map(|(n, (img, ms))| {
                            let name = format!("img{} {n}", d.idx);
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
                        d.idx,
                        Slot {
                            tex,
                            path: d.path,
                            full: d.full,
                            orig,
                            anim,
                            hist,
                            pages,
                        },
                    );
                    if d.idx == self.cur
                        && let Some(t) = self.started.take()
                    {
                        eprintln!("first image: {} ms", t.elapsed().as_millis());
                    }
                }
                Some(Err(e)) if wanted => {
                    self.errors.insert(d.idx, e);
                }
                _ => {}
            }
        }
    }

    /// Geeqie's sort menu in the status bar.
    fn sort_menu(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        let mut changed = false;
        ui.menu_button(format!("{} ⏷", self.config.sort.entry().2), |ui| {
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
                .checkbox(&mut self.config.sort_ascending, "Ascending")
                .changed();
            changed |= ui.checkbox(&mut self.config.sort_case, "Case").changed();
        });
        if changed {
            self.config.save();
            // A chosen order replaces a shuffled one (kept after a random slideshow).
            self.random = false;
            self.list(ctx);
        }
    }

    /// Keep the screen saver off while `on`: gnome-session-inhibit (GNOME), else systemd-inhibit.
    /// The inhibitor runs `cat` on a pipe from Glimt, so it ends with Glimt even after a crash.
    fn inhibit(&mut self, on: bool) {
        if on == self.inhibitor.is_some() {
            return;
        }
        if let Some(mut child) = self.inhibitor.take() {
            let _ = child.kill();
            std::thread::spawn(move || child.wait());
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
        self.inhibitor = commands.iter().find_map(|c| {
            command(c[0])
                .args(&c[1..])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .ok()
        });
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
                self.notice = Some(format!("No such folder: {}", path.display()));
            }
        }
        if !response.has_focus() {
            self.path_text = self.dir.display().to_string();
        }
    }

    /// Geeqie's toolbar: navigation, zoom, thumbnails, slideshow and preferences.
    fn toolbar(&mut self, ui: &mut egui::Ui, act: &mut Option<Action>) {
        let size = [14.0, 20.0, 28.0][self.config.toolbar_icons.min(2)];
        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.style_mut().override_font_id = Some(egui::FontId::proportional(size));
            ui.horizontal(|ui| {
                tool_button(ui, act, "⬅", Action::Back, false);
                tool_button(ui, act, "➡", Action::Forward, false);
                tool_button(ui, act, "⬆", Action::Up, false);
                tool_button(ui, act, "🏠", Action::Home, false);
                tool_button(ui, act, "🔄", Action::Refresh, false);
                ui.separator();
                tool_button(ui, act, "➕", Action::ZoomIn, false);
                tool_button(ui, act, "➖", Action::ZoomOut, false);
                tool_button(ui, act, "1:1", Action::Zoom1To1, false);
                tool_button(ui, act, "⛶", Action::ZoomFit, self.zoom.is_none());
                ui.separator();
                tool_button(ui, act, "🖼", Action::Thumbnails, self.config.thumbnails);
                let show = if self.slideshow.is_some() {
                    "⏹"
                } else {
                    "▶"
                };
                tool_button(ui, act, show, Action::Slideshow, self.slideshow.is_some());
                tool_button(ui, act, "⚙", Action::Preferences, false);
            });
        });
    }

    /// Full screen info in the top left corner: name and place, then size and EXIF.
    fn overlay(&mut self, ui: &mut egui::Ui, area: egui::Rect) {
        self.want_exif();
        let mut info = Vec::new();
        if let Some(s) = self.slots.get(&self.cur) {
            info.push(format!("{} × {}", s.orig[0], s.orig[1]));
        }
        let path = self.path(self.cur);
        if let Some((_, rows)) = self.exif.as_ref().filter(|e| e.0 == path) {
            let wanted = [
                "File size",
                "Taken",
                "Exposure",
                "Aperture",
                "ISO",
                "Focal length",
            ];
            for (label, value) in rows.iter().filter(|r| wanted.contains(&r.0)) {
                info.push(if *label == "ISO" {
                    format!("ISO {value}")
                } else {
                    value.clone()
                });
            }
        }
        let marks = self.mark_text(self.cur);
        if !marks.is_empty() {
            info.push(marks);
        }
        let name = self.files[self.cur].to_string_lossy();
        let text = format!("{name}  {}\n{}", self.place(), info.join(" · "));
        let p = ui.painter();
        let galley = p.layout_no_wrap(text, egui::FontId::proportional(15.0), egui::Color32::WHITE);
        let at = egui::Rect::from_min_size(
            area.min + egui::vec2(12.0, 12.0),
            galley.size() + egui::vec2(20.0, 12.0),
        );
        p.rect_filled(at, 6.0, egui::Color32::from_black_alpha(150));
        p.galley(at.min + egui::vec2(10.0, 6.0), galley, egui::Color32::WHITE);
    }

    /// Ask the EXIF reader for the shown image (once per image) and take what has arrived.
    fn want_exif(&mut self) {
        let path = self.path(self.cur);
        if self.exif_asked.as_ref() != Some(&path) {
            let _ = self.exif_jobs.send(path.clone());
            self.exif_asked = Some(path);
        }
        for (path, rows, all) in self.exif_done.try_iter() {
            self.exif = Some((path, rows));
            self.exif_all = all;
        }
    }

    /// The shown image's place in the list as shown: "[3/16]", "[–/12]" if a search hides it.
    fn place(&self) -> String {
        match &self.matches {
            Some(m) => match m.iter().position(|&i| i == self.cur) {
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

    /// File and EXIF details of the shown image, read in the background (Ctrl+K).
    fn exif_panel(&mut self, ui: &mut egui::Ui) {
        self.want_exif();
        egui::Panel::right("exif")
            .resizable(true)
            .default_size(240.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    egui::Grid::new("exif rows").num_columns(2).show(ui, |ui| {
                        ui.weak("File");
                        ui.label(self.files[self.cur].to_string_lossy());
                        ui.end_row();
                        if let Some(s) = self.slots.get(&self.cur) {
                            ui.weak("Dimensions");
                            ui.label(format!("{} × {}", s.orig[0], s.orig[1]));
                            ui.end_row();
                        }
                        // The old rows stay until the new ones arrive, so the panel does not flicker.
                        for (label, value) in self.exif.iter().flat_map(|e| &e.1) {
                            ui.weak(*label);
                            // The place opens on a map in the browser.
                            if *label == "GPS" {
                                let link = ui.link(value).on_hover_text("Show on OpenStreetMap");
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
                });
            });
    }

    fn receive_thumbs(&mut self, ctx: &egui::Context) {
        while let Ok((path, result)) = self.thumbs_done.try_recv() {
            // The name as the list has it: with its folders in a slideshow with subfolders.
            let Ok(name) = path.strip_prefix(&self.dir).map(Path::as_os_str) else {
                continue;
            };
            self.thumbs_pending.remove(name);
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
        ui.painter().with_clip_rect(name_rect).text(
            egui::pos2(name_rect.min.x, rect.center().y),
            egui::Align2::LEFT_CENTER,
            self.files[i].to_string_lossy(),
            font,
            text_color,
        );
        response
    }

    /// Thumbnail `i` fitted into `frame`, or a request for it.
    fn paint_thumb(&mut self, ui: &egui::Ui, i: usize, frame: egui::Rect) {
        let name = &self.files[i];
        match self.thumbs.get(name) {
            Some(Some(tex)) => {
                // Fit the box, but never enlarge.
                let px = tex.size_vec2() / ui.ctx().pixels_per_point();
                let scale = (self.thumb().x / px.x).min(self.thumb().y / px.y).min(1.0);
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
        let name = self.files[i].to_string_lossy().into_owned();
        let mut job = egui::text::LayoutJob::simple_singleline(name, font, color);
        job.wrap = egui::text::TextWrapping::truncate_at_width(self.cell().x - 6.0);
        let galley = ui.painter().layout_job(job);
        let at = egui::pos2(
            rect.center().x - galley.size().x / 2.0,
            frame.bottom() + 4.0,
        );
        ui.painter().galley(at, galley, color);
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
        response.on_hover_text_at_pointer(self.files[i].to_string_lossy())
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
                    std::thread::spawn(move || {
                        let subdirs = list_folder(&dir, false).0;
                        let _ = tx.send((dir, subdirs));
                        ctx.request_repaint();
                    });
                }
            });
    }

    /// The compared image in `rect`: at the same zoom and offset as the shown one when zoomed,
    /// else fitted, with its name above.
    fn paint_other(&mut self, ui: &egui::Ui, rect: egui::Rect, zoom: Option<f32>) {
        let p = ui.painter();
        p.vline(
            rect.left() - 1.0,
            rect.y_range(),
            egui::Stroke::new(2.0, egui::Color32::from_gray(60)),
        );
        let Some(other) = self.compare_other() else {
            return;
        };
        let ppp = ui.ctx().pixels_per_point();
        // Zoomed in further than the screen-size version holds: load the full resolution.
        let sharper = self.slots.get(&other).is_some_and(|s| {
            !s.full
                && zoom
                    .is_some_and(|z| s.tex.size()[0] as f32 + 1.0 < s.orig[0] as f32 * z.min(1.0))
        });
        if sharper {
            self.request(other, true);
        }
        if let Some(slot) = self.slots.get(&other) {
            let size = egui::vec2(slot.orig[0] as f32, slot.orig[1] as f32);
            let scale = zoom.unwrap_or_else(|| {
                let fit = (rect.width() * ppp / size.x).min(rect.height() * ppp / size.y);
                if self.config.enlarge {
                    fit
                } else {
                    fit.min(1.0)
                }
            });
            let at = egui::Rect::from_center_size(rect.center() + self.pan, size * scale / ppp);
            let uv = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
            p.with_clip_rect(rect)
                .image(slot.tex.id(), at, uv, egui::Color32::WHITE);
        }
        let name = self.files[other].to_string_lossy();
        let font = egui::TextStyle::Body.resolve(ui.style());
        let galley = p.layout_no_wrap(name.into_owned(), font, egui::Color32::WHITE);
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

    /// Fuzzy search box above the file list.
    fn search_field(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if !self.search.is_empty() && ui.small_button("✖").clicked() {
                self.search.clear();
                self.update_search(ui.ctx());
                self.scroll_to_cur = true;
            }
            // A fixed ID: the ✖ button appearing in front of it must not make it a new
            // widget, which would lose the focus after the first letter.
            let edit = egui::TextEdit::singleline(&mut self.search)
                .id(egui::Id::new("file search"))
                .hint_text("Search file names (3+ letters), or exif: camera, date …")
                .desired_width(f32::INFINITY);
            if ui.add(edit).changed() {
                self.update_search(ui.ctx());
            }
        });
    }

    /// The files the list shows: the search matches, or all.
    fn shown_files(&self) -> Vec<usize> {
        (self.matches.clone()).unwrap_or_else(|| (0..self.files.len()).collect())
    }

    /// The file `delta` rows from file `from`, in the order the list shows (the search matches
    /// while searching); clamped at the ends, or going round with `wrap`. A file the search
    /// hides counts as being before the first row.
    fn step(&self, from: usize, delta: isize, wrap: bool) -> Option<usize> {
        let m = self.matches.as_deref();
        let n = m.map_or(self.files.len(), <[usize]>::len) as isize;
        if n == 0 {
            return None;
        }
        let row = match m {
            Some(m) => m.iter().position(|&x| x == from).map_or(-1, |r| r as isize),
            None => from as isize,
        };
        let r = if wrap {
            (row + delta).rem_euclid(n)
        } else {
            (row + delta).clamp(0, n - 1)
        } as usize;
        Some(m.map_or(r, |m| m[r]))
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

    /// Run the search again; file indices change when the list does.
    fn update_search(&mut self, ctx: &egui::Context) {
        let query = self.search.trim();
        self.matches = match query.strip_prefix("exif:") {
            Some(words) => Some(self.exif_matches(ctx, &words.to_lowercase())),
            None => self.mark_matches(query).or_else(|| {
                (query.chars().count() >= 3).then(|| search::matches(&self.files, query))
            }),
        };
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

    /// Files whose EXIF contains every word, in list order. The first such search in a folder
    /// reads the EXIF of all files in the background (the busy pointer shows meanwhile) and
    /// finds nothing until that is done.
    /// ponytail: files added after the index was built do not match until the folder is reopened.
    fn exif_matches(&mut self, ctx: &egui::Context, words: &str) -> Vec<usize> {
        let Some(index) = &self.exif_index else {
            if self.exif_rx.is_none() {
                let (tx, rx) = mpsc::channel();
                let (dir, files, ctx) = (self.dir.clone(), self.files.clone(), ctx.clone());
                let busy = self.busy.clone();
                busy.fetch_add(1, Ordering::Relaxed);
                std::thread::spawn(move || {
                    let _ = tx.send(exif_index(&dir, &files));
                    busy.fetch_sub(1, Ordering::Relaxed);
                    ctx.request_repaint();
                });
                self.exif_rx = Some(rx);
            }
            return Vec::new();
        };
        let words: Vec<&str> = words.split_whitespace().collect();
        (0..self.files.len())
            .filter(|&i| {
                index
                    .get(&self.files[i])
                    .is_some_and(|text| words.iter().all(|w| text.contains(w)))
            })
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
        Some(
            (0..self.files.len())
                .filter(|&i| hit(self.marks.get(&self.path(i))))
                .collect(),
        )
    }

    #[cfg(not(feature = "marks"))]
    fn mark_matches(&self, _: &str) -> Option<Vec<usize>> {
        None
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

    /// Size and modification time of file `i`, from the cache or one stat.
    /// ponytail: stat in the UI thread, once per visible file; move to a thread if network
    /// folders make scrolling stutter.
    fn file_details(&mut self, i: usize) -> (u64, i64) {
        if let Some(&d) = self.details.get(&self.files[i]) {
            return d;
        }
        let d = std::fs::metadata(self.path(i)).map_or((0, 0), |m| {
            use std::os::unix::fs::MetadataExt;
            (m.len(), m.mtime())
        });
        self.details.insert(self.files[i].clone(), d);
        d
    }

    /// Drop thumbnails far from view once there are many, so memory stays flat in huge folders.
    fn trim_thumbs(&mut self, first: usize, last: usize) {
        // Big thumbnails take four times the memory: keep a quarter as many.
        let max = if self.config.thumb_size == 2 {
            MAX_THUMBS / 4
        } else {
            MAX_THUMBS
        };
        if self.thumbs.len() <= max {
            return;
        }
        let keep = first.saturating_sub(max / 3)..(last + max / 3).min(self.files.len());
        let keep: HashSet<&OsString> = self.files[keep].iter().collect();
        self.thumbs.retain(|name, _| keep.contains(name));
    }

    /// The action for a shortcut pressed this frame, if any.
    /// `delete_key` false: the Delete key does nothing (Ctrl+D still deletes), as in Geeqie.
    fn shortcut(ctx: &egui::Context, delete_key: bool) -> Option<Action> {
        ctx.input_mut(|i| {
            // egui-winit turns Ctrl+C into a Copy event instead of a key press.
            let all = shortcuts();
            let copies = i.events.len();
            i.events.retain(|e| !matches!(e, egui::Event::Copy));
            if i.events.len() < copies {
                // Whatever has Ctrl+C: Copy, unless the user gave the key to another action.
                return (all.iter())
                    .find(|s| s.1 == CTRL && s.2 == egui::Key::C)
                    .map(|s| s.0);
            }
            all.iter()
                .filter(|s| delete_key || s.2 != egui::Key::Delete)
                .find(|&&(_, mods, key)| {
                    i.consume_shortcut(&egui::KeyboardShortcut::new(mods, key))
                })
                .map(|s| s.0)
        })
    }

    fn zoom_by(&mut self, factor: f32) {
        self.zoom = Some((self.zoom.unwrap_or(self.last_scale) * factor).clamp(0.02, 32.0));
    }

    fn run(&mut self, ctx: &egui::Context, action: Action) {
        let fullscreen = ctx.input(|i| i.viewport().fullscreen).unwrap_or(false);
        match action {
            Action::OpenFile => self.pick(ctx, false, Pick::Open),
            Action::OpenFolder => self.pick(ctx, true, Pick::Open),
            Action::Copy | Action::Move if !self.files.is_empty() => {
                let paths = self.targets().iter().map(|&i| self.path(i)).collect();
                let purpose = match action {
                    Action::Copy => Pick::CopyTo(paths),
                    _ => Pick::MoveTo(paths),
                };
                self.pick(ctx, true, purpose);
            }
            Action::Copy | Action::Move => {}
            // ponytail: the default application only; an app chooser (`gio mime`) if wanted.
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
                let Some(to) = self.step(self.cur, delta, false) else {
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
                let here = self.location();
                let (from, to) = if action == Action::Back {
                    (&mut self.back, &mut self.forward)
                } else {
                    (&mut self.forward, &mut self.back)
                };
                if let Some(path) = from.pop() {
                    to.push(here);
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
            Action::CopyImage if !self.files.is_empty() => {
                let (path, rotate) = (self.path(self.cur), self.config.exif_rotate);
                let (busy, tx, ctx) = (self.busy.clone(), self.notices_tx.clone(), ctx.clone());
                let (clip_tx, clip_rx) = mpsc::channel();
                self.clip = Some(clip_rx);
                busy.fetch_add(1, Ordering::Relaxed);
                std::thread::spawn(move || {
                    match load(&path, rotate) {
                        Ok(img) => drop(clip_tx.send(color_image(img))),
                        Err(e) => drop(tx.send(format!("Could not copy the image: {e}"))),
                    }
                    busy.fetch_sub(1, Ordering::Relaxed);
                    ctx.request_repaint();
                });
            }
            Action::CopyImage => {}
            // As a list of files, which a file manager pastes as a copy.
            Action::CopyFiles if !self.files.is_empty() => {
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
                    self.notice = Some(format!("Could not copy to the clipboard: {e}"));
                }
            }
            Action::CopyFiles => {}
            Action::Preferences => self.show_settings = true,
            Action::Help => self.help = Some(help::Page::Manual),
            Action::Shortcuts => self.help = Some(help::Page::Shortcuts),
            Action::KeyboardMap => self.help = Some(help::Page::Keymap),
            Action::About => self.help = Some(help::Page::About),
            Action::Thumbnails => self.set_thumbnails(!self.config.thumbnails),
            Action::SaveOrientation => self.save_orientation(),
            Action::Subfolders => self.set_subfolders(ctx, !self.config.show_subfolders),
            Action::NextPage | Action::PrevPage => {
                let pages = self.slots.get(&self.cur).map_or(1, |s| s.pages);
                let page = match action {
                    Action::NextPage => (self.page + 1).min(pages - 1),
                    _ => self.page.saturating_sub(1),
                };
                if page != self.page {
                    self.page = page;
                    self.slots.remove(&self.cur);
                    self.pending.retain(|p| p.0 != self.cur);
                    self.refill();
                }
            }
            Action::ExifWindow => self.exif_window = !self.exif_window,
            Action::Compare => {
                self.compare = !self.compare;
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
            | Action::Rate5 => {
                let Some((kind, n)) = action.mark() else {
                    return;
                };
                if kind == 's' {
                    let bit = 1 << (n - 1);
                    self.selected = (self.shown_files().into_iter())
                        .filter(|&i| self.marks.get(&self.path(i)).0 & bit != 0)
                        .collect();
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
                    if self.config.xmp_stars {
                        let failed = paths.iter().find_map(|p| marks::write_xmp(p, n).err());
                        if let Some(e) = failed {
                            self.notice = Some(format!("Could not write the XMP sidecar: {e}"));
                        }
                    }
                }
                // A "mark:" or "stars:" search shows the new state.
                if self.search.starts_with("mark:") || self.search.starts_with("stars:") {
                    self.update_search(ctx);
                }
            }
            Action::Histogram => {
                self.config.histogram = (self.config.histogram + 1) % 3;
                self.config.save();
            }
            Action::PathBar
            | Action::Toolbar
            | Action::ExifPanel
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
                    Action::Animate => &mut c.animate,
                    _ => &mut c.exif_panel,
                };
                *flag = !*flag;
                c.save();
            }
            Action::NewFolder => self.prompt = Some((Prompt::NewFolder, String::new())),
            Action::Rename if self.targets().len() > 1 => {
                self.prompt = Some((Prompt::RenameMany, "*_###".into()));
            }
            Action::Rename => {
                if let Some(name) = self.files.get(self.cur) {
                    self.prompt = Some((Prompt::Rename, name.to_string_lossy().into_owned()));
                }
            }
            // A separate process: windows share nothing but the config file.
            Action::NewWindow => {
                // After an update (or a rebuild) Linux reports the running binary as
                // "/path/glimt (deleted)"; the new one is at the same path.
                let started = std::env::current_exe().and_then(|exe| {
                    let exe = exe.to_string_lossy();
                    let exe = exe.strip_suffix(" (deleted)").unwrap_or(&exe);
                    std::process::Command::new(exe).arg(self.location()).spawn()
                });
                match started {
                    // Reap the child when it exits, so it does not linger as a zombie.
                    Ok(mut child) => drop(std::thread::spawn(move || child.wait())),
                    Err(e) => self.notice = Some(format!("Could not open a new window: {e}")),
                }
            }
            Action::ExifRotate => self.set_exif_rotate(!self.config.exif_rotate),
            // Mirroring or flipping what is on screen after a turn: mirror the image first and
            // turn the other way (a flip is a mirror and half a turn).
            Action::RotateCw
            | Action::RotateCcw
            | Action::Rotate180
            | Action::Mirror
            | Action::Flip => {
                let (q, m) = self.orient;
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
            Action::ZoomFit => {
                self.zoom = None;
                self.pan = egui::Vec2::ZERO;
            }
            Action::FullScreen => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fullscreen))
            }
            Action::HideTools => self.hide_tools = !self.hide_tools,
            Action::Refresh => {
                self.tree.clear();
                self.slots.clear();
                self.pending.clear();
                self.errors.clear();
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
}

impl eframe::App for App {
    /// The last folder is written once on exit, not on every folder change.
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        remote::stop();
        // ponytail: the path is stored as UTF-8 (lossless for ordinary paths); switch to OsString bytes if needed.
        let mut state = String::new();
        if self.config.remember_dir {
            state += &format!("last_dir={}\n", self.dir.display());
        }
        if self.config.remember_window {
            state += &self.window.to_text();
        }
        if let Some(p) = state_path().filter(|_| !state.is_empty()) {
            write_atomic(&p, &state);
        }
        self.inhibit(false);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = &ui.ctx().clone();
        let ppp = ctx.pixels_per_point();
        let max_side = ctx.input(|i| i.max_texture_side) as u32;
        self.limits.lock().unwrap()[1] = [max_side, max_side];
        self.receive(ctx);
        self.receive_thumbs(ctx);
        self.receive_listing(ctx, std::time::Duration::ZERO);
        self.receive_sizes(ctx);
        self.receive_deleted();
        let focus = ctx.input(|i| i.viewport().focused.unwrap_or(false));
        if focus && !self.has_focus && self.config.remote_control {
            remote::focused();
        }
        self.has_focus = focus;
        while let Ok(command) = self.remote.try_recv() {
            let action = match command.trim() {
                "next" => Some(Action::Next),
                "prev" => Some(Action::Prev),
                "first" => Some(Action::First),
                "last" => Some(Action::Last),
                "slideshow" => Some(Action::Slideshow),
                "fullscreen" => Some(Action::FullScreen),
                "quit" => Some(Action::Quit),
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
        if let Some(img) = self.clip.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.clip = None;
            ctx.copy_image(img);
        }
        // A file or folder dropped on the window opens there, as File → Open would.
        let dropped = ctx.input(|i| {
            let mut paths = i.raw.dropped_files.iter().map(|f| f.path().to_path_buf());
            paths.find(|p| !p.as_os_str().is_empty())
        });
        if let Some(path) = dropped {
            self.visit(ctx, path);
        }
        // Busy pointer while the folder is read, files are deleted, copied or moved, or the
        // shown image is still being decoded.
        let decoding = !self.files.is_empty()
            && !self.slots.contains_key(&self.cur)
            && !self.errors.contains_key(&self.cur);
        if let Some(done) = self.mounting.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.mounting = None;
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
        if self.listing.is_some()
            || decoding
            || self.mounting.is_some()
            || self.busy.load(Ordering::Relaxed) > 0
        {
            ctx.set_cursor_icon(egui::CursorIcon::Progress);
        }
        if self.changed_rx.try_iter().count() > 0 && self.config.auto_refresh {
            self.stale = true;
        }
        if self.stale && self.listing.is_none() {
            self.list(ctx);
        }
        while let Ok(msg) = self.notices.try_recv() {
            // Deleting failed, so the file probably still exists: re-read the folder so the list is right.
            self.open(ctx, self.location());
            self.notice = Some(msg);
        }
        self.slideshow_tick(ctx);
        if let Some(Ok(picked)) = self.dialog.as_ref().map(|(_, rx)| rx.try_recv()) {
            let (purpose, _) = self.dialog.take().unwrap();
            match (purpose, picked) {
                (_, None) => {}
                (Pick::Open, Some(path)) => self.visit(ctx, path),
                (Pick::CopyTo(paths), Some(dest)) => self.transfer(ctx, paths, dest, true),
                (Pick::MoveTo(paths), Some(dest)) => self.transfer(ctx, paths, dest, false),
            }
        }

        // Shortcuts go to the dialogs while one is open.
        // ... and to the text field while one has focus (the toolbar's folder path).
        let typing = ctx.egui_wants_keyboard_input();
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
                        self.pan = (self.pan + d).clamp(-max, max);
                    }
                }
            });
        }
        // In the icon grid ↑ and ↓ move a row; read before the shortcut takes the key.
        let vertical =
            ctx.input(|i| i.key_pressed(egui::Key::ArrowUp) || i.key_pressed(egui::Key::ArrowDown));
        self.row_step = if vertical { self.grid_cols as isize } else { 1 };
        let mut act = (!self.dialog_open() && !typing)
            .then(|| Self::shortcut(ctx, self.config.delete_key))
            .flatten();
        // Full screen and Hide File List as in Geeqie: only the image, no menu or panels.
        let fullscreen = ctx.input(|i| i.viewport().fullscreen).unwrap_or(false);
        let bare = fullscreen || self.hide_tools;
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
        self.window.maximized = maximized;
        if !fullscreen && !maximized {
            self.window.size = Some(size);
            // Wayland reports no position.
            self.window.pos = outer.map(|r| r.min).or(self.window.pos);
        }
        if !bare {
            egui::Panel::top("menu").show(ui, |ui| {
                egui::MenuBar::new().ui(ui, |ui| {
                    let act = &mut act;
                    ui.menu_button("File", |ui| {
                        menu_item(ui, act, Action::OpenFile);
                        menu_item(ui, act, Action::OpenFolder);
                        menu_item(ui, act, Action::NewFolder);
                        menu_item(ui, act, Action::Copy);
                        menu_item(ui, act, Action::Move);
                        menu_item(ui, act, Action::Rename);
                        menu_item(ui, act, Action::OpenWith);
                        self.open_with_menu(ui);
                        #[cfg(feature = "video")]
                        menu_item(ui, act, Action::Play);
                        ui.separator();
                        menu_item(ui, act, Action::Delete);
                        menu_item(ui, act, Action::DeletePermanently);
                        ui.separator();
                        menu_item(ui, act, Action::CloseWindow);
                        menu_item(ui, act, Action::Quit);
                    });
                    ui.menu_button("Go", |ui| {
                        menu_item(ui, act, Action::First);
                        menu_item(ui, act, Action::Prev);
                        menu_item(ui, act, Action::Next);
                        menu_item(ui, act, Action::Last);
                        ui.separator();
                        menu_item(ui, act, Action::Back);
                        menu_item(ui, act, Action::Forward);
                        menu_item(ui, act, Action::Home);
                        menu_item(ui, act, Action::Up);
                    });
                    ui.menu_button("Select", |ui| {
                        menu_item(ui, act, Action::SelectAll);
                        menu_item(ui, act, Action::SelectNone);
                        menu_item(ui, act, Action::InvertSelection);
                        ui.separator();
                        menu_item(ui, act, Action::CopyPath);
                        menu_item(ui, act, Action::CopyPathUnquoted);
                        menu_item(ui, act, Action::CopyImage);
                        menu_item(ui, act, Action::CopyFiles);
                    });
                    ui.menu_button("Edit", |ui| {
                        #[cfg(feature = "marks")]
                        ui.menu_button("Marks and Stars", |ui| {
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
                        ui.menu_button("Orientation", |ui| {
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
                        ui.separator();
                        menu_item(ui, act, Action::Preferences);
                    });
                    ui.menu_button("View", |ui| {
                        ui.menu_button("Zoom", |ui| {
                            menu_item(ui, act, Action::ZoomIn);
                            menu_item(ui, act, Action::ZoomOut);
                            menu_item(ui, act, Action::Zoom1To1);
                            menu_item(ui, act, Action::ZoomFit);
                        });
                        menu_item(ui, act, Action::FullScreen);
                        menu_item(ui, act, Action::HideTools);
                        let c = &self.config;
                        menu_toggle(ui, act, c.toolbar, Action::Toolbar);
                        menu_toggle(ui, act, c.path_bar, Action::PathBar);
                        menu_toggle(ui, act, c.folder_tree, Action::FolderTree);
                        menu_toggle(ui, act, c.show_subfolders, Action::Subfolders);
                        menu_toggle(ui, act, c.thumbnails, Action::Thumbnails);
                        menu_toggle(ui, act, c.icons, Action::Icons);
                        menu_toggle(ui, act, c.exif_panel, Action::ExifPanel);
                        menu_toggle(ui, act, self.exif_window, Action::ExifWindow);
                        menu_toggle(ui, act, c.osd, Action::Overlay);
                        menu_item(ui, act, Action::Histogram);
                        menu_toggle(ui, act, c.histogram_log, Action::HistogramLog);
                        menu_item(ui, act, Action::Refresh);
                        ui.separator();
                        let label = if self.slideshow.is_some() {
                            "Stop Slideshow"
                        } else {
                            "Start Slideshow"
                        };
                        menu_button(ui, act, egui::Button::new(label), Action::Slideshow);
                        menu_toggle(ui, act, self.paused, Action::Pause);
                        menu_item(ui, act, Action::Faster);
                        menu_item(ui, act, Action::Slower);
                        menu_toggle(ui, act, self.config.random, Action::RandomOrder);
                    });
                    ui.menu_button("Window", |ui| {
                        menu_item(ui, act, Action::NewWindow);
                        menu_item(ui, act, Action::CloseWindow);
                    });
                    ui.menu_button("Help", |ui| {
                        menu_item(ui, act, Action::Help);
                        menu_item(ui, act, Action::Shortcuts);
                        menu_item(ui, act, Action::KeyboardMap);
                        ui.separator();
                        menu_item(ui, act, Action::About);
                    });
                });
            });
            if self.config.toolbar {
                self.toolbar(ui, &mut act);
            }
        }
        if let Some(action) = act {
            self.run(ctx, action);
        }
        self.grid_cols = 1;

        let mut open = self.show_settings;
        // Esc closes Preferences (shortcuts are off while it is open), unless a dialog on top
        // of it is the one Esc is meant for.
        if open
            && self.prompt.is_none()
            && self.confirm.is_none()
            && self.capturing.is_none()
            && ctx.input(|i| i.key_pressed(egui::Key::Escape))
        {
            open = false;
        }
        egui::Window::new("Preferences")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .show(ctx, |ui| {
                // Scrolls when the window is smaller than the settings; a solid scroll bar
                // takes its own space instead of covering the text.
                ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    let mut n = self.ahead.load(Ordering::Relaxed);
                    let slider = egui::Slider::new(&mut n, 0..=20).text("images on each side");
                    ui.label("Preload next and previous images:");
                    if ui.add(slider).changed() {
                        self.ahead.store(n, Ordering::Relaxed);
                        self.refill();
                        self.config.preload = n;
                        self.config.save();
                    }
                    let [w, h] = self.limits.lock().unwrap()[0];
                    let mb = (2 * n as u64 + 1) * w as u64 * h as u64 * 4 / 1_000_000;
                    ui.weak(format!("Up to ≈ {mb} MB at window size {w}×{h}"));
                    ui.separator();
                    ui.label("Slideshow (S, R = random, Ctrl+= faster, Ctrl+- slower):");
                    // Plain settings set `changed`; the config is saved once at the end.
                    let mut changed = false;
                    let delay = egui::Slider::new(&mut self.config.delay, 1..=MAX_DELAY)
                        .text("seconds per image");
                    if ui.add(delay).changed() {
                        changed = true;
                        self.title(ctx);
                    }
                    let mut random = self.config.random;
                    if ui.checkbox(&mut random, "Random order").changed() {
                        self.set_random_order(ctx, random);
                    }
                    changed |= ui
                        .checkbox(
                            &mut self.config.slideshow_subfolders,
                            "Include the images in subfolders",
                        )
                        .changed();
                    changed |= ui
                        .checkbox(
                            &mut self.config.slideshow_repeat,
                            "Repeat: start again after the last image",
                        )
                        .changed();
                    ui.separator();
                    let mut rotate = self.config.exif_rotate;
                    if ui
                        .checkbox(&mut rotate, "Auto rotate images using EXIF (Alt+X)")
                        .changed()
                    {
                        self.set_exif_rotate(rotate);
                    }
                    changed |= ui
                        .checkbox(
                            &mut self.config.save_orientation,
                            "Save rotation to the file when changing image (JPEG; PNG and WebP are saved again)",
                        )
                        .changed();
                    let quality = egui::Slider::new(&mut self.config.webp_quality, 50..=100)
                        .text("quality when a lossy WebP is saved again");
                    changed |= ui.add(quality).changed();
                    ui.horizontal(|ui| {
                        changed |= ui
                            .color_edit_button_srgb(&mut self.config.background.0)
                            .changed();
                        ui.label("Background behind the image");
                        if self.config.background.0 != [0, 0, 0]
                            && ui.small_button("Black").clicked()
                        {
                            self.config.background.0 = [0, 0, 0];
                            changed = true;
                        }
                    });
                    let c = &mut self.config;
                    changed |= ui
                        .checkbox(&mut c.enlarge, "Enlarge small images to fit the window")
                        .changed();
                    changed |= ui
                        .checkbox(
                            &mut c.keep_zoom,
                            "Keep zoom and position when changing image",
                        )
                        .changed();
                    changed |= ui
                        .checkbox(
                            &mut c.wheel_zoom,
                            "Mouse wheel zooms (off: next/previous image, Ctrl+wheel zooms)",
                        )
                        .changed();
                    ui.separator();
                    let mut show = self.config.thumbnails;
                    if ui
                        .checkbox(&mut show, "Show thumbnails in the file list (T)")
                        .changed()
                    {
                        self.set_thumbnails(show);
                    }
                    ui.horizontal(|ui| {
                        ui.label("Thumbnail size:");
                        let mut size = self.config.thumb_size;
                        for (n, name) in [(0, "Small"), (1, "Medium"), (2, "Large")] {
                            if ui.radio_value(&mut size, n, name).changed() {
                                self.set_thumb_size(size);
                            }
                        }
                    });
                    let c = &mut self.config;
                    changed |= ui
                        .checkbox(&mut c.icons, "Show the file list as icons (Ctrl+I)")
                        .changed();
                    changed |= ui
                        .checkbox(&mut c.folder_tree, "Show folders as a tree (Ctrl+T)")
                        .changed();
                    let mut sub = c.show_subfolders;
                    if ui
                        .checkbox(
                            &mut sub,
                            "Include the images in subfolders in the file list",
                        )
                        .changed()
                    {
                        self.set_subfolders(ctx, sub);
                    }
                    let c = &mut self.config;
                    #[cfg(feature = "marks")]
                    {
                        changed |= ui
                            .checkbox(&mut c.show_marks, "Show marks and stars in the file list")
                            .changed();
                        changed |= ui
                            .checkbox(
                                &mut c.xmp_stars,
                                "Also write stars to XMP sidecars (photo.jpg.xmp), for darktable and digiKam",
                            )
                            .changed();
                    }
                    ui.horizontal(|ui| {
                        ui.label("Histogram (K):");
                        for (n, name) in [(0, "Off"), (1, "RGB"), (2, "Brightness")] {
                            changed |= ui.radio_value(&mut c.histogram, n, name).changed();
                        }
                        changed |= ui
                            .checkbox(&mut c.histogram_log, "Logarithmic (J)")
                            .changed();
                    });
                    changed |= ui
                        .checkbox(&mut c.animate, "Play animated GIF and WebP (A)")
                        .changed();
                    let mut cm = c.color_management;
                    if ui
                        .checkbox(
                            &mut cm,
                            "Colour management: show ICC profiles (Adobe RGB, P3) as sRGB",
                        )
                        .changed()
                    {
                        self.set_color_management(cm);
                    }
                    let c = &mut self.config;
                    changed |= ui
                        .checkbox(
                            &mut c.file_details,
                            "Show file size and date in the file list",
                        )
                        .changed();
                    let save =
                        "Save new thumbnails to ~/.cache/thumbnails (shared with other programs)";
                    if ui.checkbox(&mut c.save_thumbnails, save).changed() {
                        self.save_thumbs.store(c.save_thumbnails, Ordering::Relaxed);
                        changed = true;
                    }
                    // Geeqie's cache maintenance: thumbnails of files that are gone.
                    if ui
                        .button("Remove thumbnails of deleted files")
                        .on_hover_text(
                            "In ~/.cache/thumbnails; kept for disks that are not plugged in",
                        )
                        .clicked()
                    {
                        let (tx, busy, ctx) =
                            (self.notices_tx.clone(), self.busy.clone(), ctx.clone());
                        busy.fetch_add(1, Ordering::Relaxed);
                        std::thread::spawn(move || {
                            let (removed, seen) = thumbs::clean();
                            let _ = tx.send(format!(
                                "Removed {} of {} thumbnails",
                                thousands(removed),
                                thousands(seen)
                            ));
                            busy.fetch_sub(1, Ordering::Relaxed);
                            ctx.request_repaint();
                        });
                    }
                    ui.separator();
                    let low = "Use less memory (decoding up to ~8 % slower; after restart)";
                    changed |= ui.checkbox(&mut c.low_memory, low).changed();
                    ui.separator();
                    changed |= ui.checkbox(&mut c.toolbar, "Show the toolbar").changed();
                    changed |= ui
                        .checkbox(&mut c.path_bar, "Show the folder path")
                        .changed();
                    if ui
                        .checkbox(&mut c.search_bar, "Show the file name search")
                        .changed()
                    {
                        changed = true;
                        if !c.search_bar {
                            self.search.clear();
                            self.update_search(ctx);
                        }
                    }
                    ui.horizontal(|ui| {
                        ui.label("Toolbar icons:");
                        for (n, label) in ["Small", "Medium", "Large"].into_iter().enumerate() {
                            changed |= ui
                                .radio_value(&mut self.config.toolbar_icons, n, label)
                                .changed();
                        }
                    });
                    let c = &mut self.config;
                    let exif = "Show the EXIF panel (Ctrl+K; reads a little from each image shown)";
                    changed |= ui.checkbox(&mut c.exif_panel, exif).changed();
                    let osd = "Show image info in full screen (I)";
                    changed |= ui.checkbox(&mut c.osd, osd).changed();
                    ui.separator();
                    changed |= ui
                        .checkbox(&mut c.auto_refresh, "Refresh the folder when files change")
                        .changed();
                    changed |= ui
                        .checkbox(&mut c.remember_dir, "Start in the last used folder")
                        .changed();
                    let window = "Remember the window size and position";
                    changed |= ui.checkbox(&mut c.remember_window, window).changed();
                    let remote = "Listen for glimt --remote commands (after restart)";
                    changed |= ui.checkbox(&mut c.remote_control, remote).changed();
                    let inhibit = "Keep the screen saver off in full screen";
                    changed |= ui.checkbox(&mut c.inhibit_screensaver, inhibit).changed();
                    let order = "Window title \"Glimt - image name\" (off: \"image name - Glimt\")";
                    if ui.checkbox(&mut c.app_name_first, order).changed() {
                        changed = true;
                        self.title(ctx);
                    }
                    ui.separator();
                    egui::CollapsingHeader::new("Keyboard shortcuts").show(ui, |ui| {
                        self.key_settings(ui);
                    });
                    ui.separator();
                    ui.label("Delete behaviour:");
                    let c = &mut self.config;
                    changed |= ui
                        .checkbox(&mut c.confirm_delete, "Confirm permanent file delete")
                        .changed();
                    changed |= ui
                        .checkbox(&mut c.confirm_trash, "Confirm move file to Trash")
                        .changed();
                    changed |= ui
                        .checkbox(&mut c.delete_key, "Enable Delete key")
                        .changed();
                    let mut no_trash = !c.trash;
                    let none = "Use no trash at all (Delete deletes permanently)";
                    if ui.checkbox(&mut no_trash, none).changed() {
                        c.trash = !no_trash;
                        changed = true;
                    }
                    #[cfg(feature = "video")]
                    {
                        ui.separator();
                        ui.label("Video (needs ffmpeg for previews):");
                        if ui
                            .checkbox(&mut c.videos, "Show videos in the file list")
                            .changed()
                        {
                            changed = true;
                            self.list(ctx);
                        }
                        ui.horizontal(|ui| {
                            ui.label("Player (Enter):");
                            let player = egui::TextEdit::singleline(&mut self.config.video_player)
                                .hint_text("default application");
                            changed |= ui.add(player).changed();
                        });
                    }
                    if changed {
                        self.config.save();
                    }
                });
            });

        if let Some((prompt, mut text)) = self.prompt.take() {
            let title = match prompt {
                Prompt::NewFolder => "New folder name:".to_owned(),
                Prompt::Rename => "New file name:".to_owned(),
                Prompt::RenameMany => format!(
                    "New names for {} files (* = old name, # = number, ### = 001):",
                    self.targets().len()
                ),
            };
            // The first new name, so the pattern can be checked before renaming.
            let first = self.targets().first().map(|&i| self.path(i));
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
                    ui.weak(format!("First: {}", pattern_name(text.trim(), first, 1)));
                }
                ui.horizontal(|ui| {
                    if ui.button("OK (Enter)").clicked() || enter {
                        answer = Some(text.clone());
                    }
                    if ui.button("Cancel (Esc)").clicked() {
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

        if self.exif_window && !self.files.is_empty() {
            self.want_exif();
            let mut open = true;
            let title = format!("EXIF: {}", self.files[self.cur].to_string_lossy());
            egui::Window::new(title)
                .id(egui::Id::new("exif window"))
                .open(&mut open)
                .default_size([560.0, 600.0])
                .vscroll(true)
                .show(ctx, |ui| {
                    if self.exif_all.is_empty() {
                        ui.weak("No EXIF data");
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

        if let Some(mut login) = self.login.take() {
            let mut connect = false;
            let modal = egui::Modal::new(egui::Id::new("login")).show(ctx, |ui| {
                ui.label(format!("Log in to {}", login.uri));
                egui::Grid::new("login fields").show(ui, |ui| {
                    let field = |text| egui::TextEdit::singleline(text).desired_width(220.0);
                    ui.label("User:");
                    ui.add(field(&mut login.user));
                    ui.end_row();
                    ui.label("Domain:");
                    ui.add(field(&mut login.domain));
                    ui.end_row();
                    ui.label("Password:");
                    let edit = ui.add(field(&mut login.password).password(true));
                    // The password field first, as the rest has defaults.
                    if login.password.is_empty() && ui.memory(|m| m.focused().is_none()) {
                        edit.request_focus();
                    }
                    connect = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    ui.end_row();
                });
                ui.horizontal(|ui| {
                    connect |= ui.button("Connect (Enter)").clicked();
                    if ui.button("Cancel (Esc)").clicked() {
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

        if let Some(trash) = self.confirm {
            let what = match self.targets().as_slice() {
                [i] => format!("\"{}\"", self.files[*i].to_string_lossy()),
                many => format!("{} files", many.len()),
            };
            let mut delete = false;
            let modal = egui::Modal::new(egui::Id::new("confirm-delete")).show(ctx, |ui| {
                if trash {
                    ui.label(format!("Move {what} to the trash?"));
                } else {
                    ui.label(format!("Permanently delete {what}?"));
                    ui.weak("This cannot be undone.");
                }
                ui.horizontal(|ui| {
                    delete = ui.button("Delete (Enter)").clicked()
                        || ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.button("Cancel (Esc)").clicked() {
                        ui.close();
                    }
                });
            });
            if delete {
                self.confirm = None;
                self.delete_targets_now(ctx, trash);
            } else if modal.should_close() {
                self.confirm = None;
            }
        }
        self.show_settings = open;
        help::window(ctx, &mut self.help);

        // Status bar as in Geeqie: image and selection counts on the left, current image on the right.
        if !bare {
            egui::Panel::bottom("status").show(ui, |ui| {
                ui.horizontal(|ui| {
                    let n = self.files.len();
                    let size = size_text(self.size_rx.is_none().then_some(self.total_size));
                    let mut left = match &self.matches {
                        _ if self.listing.is_some() => "Reading folder…".to_owned(),
                        // A search shows the matches: count and size of what the list shows.
                        Some(found) => {
                            let size =
                                size_text(self.match_rx.is_none().then_some(self.match_size));
                            format!(
                                "{} of {} images ({size})",
                                thousands(found.len()),
                                thousands(n)
                            )
                        }
                        None => format!("{} images ({size})", thousands(n)),
                    };
                    if !self.selected.is_empty() {
                        let m = thousands(self.selected.len());
                        let size = size_text(self.selected_size);
                        left += &format!(", {m} selected ({size})");
                    }
                    self.sort_menu(ctx, ui);
                    ui.separator();
                    ui.label(left);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if n == 0 {
                            return;
                        }
                        // As in Geeqie: "( 4000 x 3000 ) 2,345,678 bytes [3/16]", then the zoom.
                        let path = self.path(self.cur);
                        if self.cur_size.as_ref().is_none_or(|c| c.0 != path) {
                            // ponytail: one stat in the UI thread per image change; move it to
                            // a thread if slow network folders make browsing stutter.
                            let bytes = std::fs::metadata(&path).map_or(0, |m| m.len());
                            self.cur_size = Some((path, bytes));
                        }
                        let bytes = self.cur_size.as_ref().map_or(0, |c| c.1);
                        let place = self.place();
                        let bytes = format!("{} bytes", thousands(bytes as usize));
                        match self.slots.get(&self.cur) {
                            Some(slot) => {
                                let [w, h] = slot.orig;
                                ui.label(format!("{} %", (self.last_scale * 100.0).round()));
                                ui.separator();
                                let page = if slot.pages > 1 {
                                    format!(" page {}/{}", self.page + 1, slot.pages)
                                } else {
                                    String::new()
                                };
                                ui.label(format!("( {w} x {h} ) {bytes} {place}{page}"));
                            }
                            None => {
                                ui.label(format!("{bytes} {place}"));
                            }
                        }
                    });
                });
            });
        }

        // Left side as in Geeqie: one-level folder tree at the top, file names below.
        let (mut open_dir, mut go_to, mut list_act) = (None, None, None);
        if !bare {
            // Room for a name next to the size and date columns.
            let min = if self.config.file_details {
                DETAILS_WIDTH + 120.0
            } else {
                80.0
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
                                return;
                            }
                            // Row 0 is ⬆ .. (when there is a parent folder), the rest are subfolders.
                            let up = self.dir.parent().map(Path::to_path_buf);
                            let rows = up.is_some() as usize + self.subdirs.len();
                            let row_h = ui.spacing().interact_size.y;
                            egui::ScrollArea::vertical()
                                .id_salt("folders")
                                .auto_shrink(false)
                                .show_rows(ui, row_h, rows, |ui, range| {
                                    for row in range {
                                        let (label, target) = match (row, &up) {
                                            (0, Some(parent)) => ("⬆ ..".to_owned(), parent),
                                            _ => {
                                                let d = &self.subdirs[row - up.is_some() as usize];
                                                let name = d.file_name().unwrap_or_default();
                                                (format!("📁 {}", name.to_string_lossy()), d)
                                            }
                                        };
                                        if ui.selectable_label(false, label).clicked() {
                                            open_dir = Some(target.clone());
                                        }
                                    }
                                });
                        });
                    if self.config.search_bar {
                        self.search_field(ui);
                    }
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
                        let off =
                            egui::scroll_area::State::load(ctx, ui.make_persistent_id("files"))
                                .map_or(0.0, |s| s.offset.y);
                        if top < off || top + pitch > off + view {
                            area = area.vertical_scroll_offset((top - view / 2.0).max(0.0));
                        }
                    }
                    area.show_rows(ui, row_h, count.div_ceil(cols), |ui, rows| {
                        let rows = rows.start * cols..(rows.end * cols).min(count);
                        let shown: Vec<usize> = match &self.matches {
                            Some(m) => m[rows].to_vec(),
                            None => rows.collect(),
                        };

                        if thumbs
                            && let (Some(&first), Some(&last)) =
                                (shown.iter().min(), shown.iter().max())
                        {
                            self.thumb_view[0].store(first, Ordering::Relaxed);
                            self.thumb_view[1].store(last, Ordering::Relaxed);
                            self.trim_thumbs(first, last);
                        }
                        let mut line = egui::Rect::NOTHING;
                        for (k, i) in shown.into_iter().enumerate() {
                            let marked = i == self.cur || self.selected.contains(&i);
                            let row = if icons {
                                if k % cols == 0 {
                                    let size = egui::vec2(ui.available_width(), self.cell().y);
                                    line = ui.allocate_exact_size(size, egui::Sense::hover()).0;
                                }
                                let x = (k % cols) as f32 * (self.cell().x + gap);
                                let cell = egui::Rect::from_min_size(
                                    line.min + egui::vec2(x, 0.0),
                                    self.cell(),
                                );
                                self.file_cell(ui, i, cell, marked)
                            } else if thumbs || self.config.file_details {
                                self.file_row(ui, i, row_h, marked)
                            } else {
                                let name = self.files[i].to_string_lossy();
                                let marks = self.mark_text(i);
                                if marks.is_empty() {
                                    ui.selectable_label(marked, name)
                                } else {
                                    ui.selectable_label(marked, format!("{name}   {marks}"))
                                }
                            };
                            if row.clicked() {
                                let m = ui.input(|i| i.modifiers);
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
                            row.context_menu(|ui| {
                                for action in [
                                    Action::Copy,
                                    Action::Move,
                                    Action::CopyPath,
                                    Action::CopyImage,
                                    Action::CopyFiles,
                                ] {
                                    menu_item(ui, &mut list_act, action);
                                }
                                self.open_with_menu(ui);
                                ui.separator();
                                menu_item(ui, &mut list_act, Action::Delete);
                                menu_item(ui, &mut list_act, Action::DeletePermanently);
                            });
                        }
                    });
                });
        }
        self.scroll_to_cur = false;
        if let Some(d) = open_dir {
            self.visit(ctx, d);
        } else if let Some(i) = go_to {
            self.go(ctx, i);
        }
        if let Some(action) = list_act {
            self.run(ctx, action);
        }

        if !bare && self.config.exif_panel && !self.files.is_empty() {
            self.exif_panel(ui);
        }

        // Actions from the image area, run after drawing it.
        let mut later = None;
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill({
                let [r, g, b] = self.config.background.0;
                egui::Color32::from_rgb(r, g, b)
            }))
            .show(ui, |ui| {
                // Comparing: the shown image on the left, the other on the right.
                let full = ui.max_rect();
                let (area, right) = if self.compare {
                    let (l, r) = full.split_left_right_at_fraction(0.5);
                    (
                        l.shrink2(egui::vec2(1.0, 0.0)),
                        Some(r.shrink2(egui::vec2(1.0, 0.0))),
                    )
                } else {
                    (full, None)
                };
                // Decode to the image area, not the whole screen: a small window on a 4K screen
                // needs a fraction of the memory (3840×2160 is 33 MB per image).
                let view = [(area.width() * ppp) as u32, (area.height() * ppp) as u32];
                self.limits.lock().unwrap()[0] = view;
                let response = ui.allocate_rect(area, egui::Sense::click_and_drag());
                let max_fit = if self.config.enlarge {
                    f32::INFINITY
                } else {
                    1.0
                };
                // Size on screen after a quarter turn.
                let quarter = self.orient.0 % 2 == 1;
                let turned = |[w, h]: [usize; 2]| if quarter { [h, w] } else { [w, h] };
                let fit = self.slots.get(&self.cur).map_or(1.0, |s| {
                    let [w, h] = turned(s.orig);
                    (area.width() * ppp / w as f32)
                        .min(area.height() * ppp / h as f32)
                        .min(max_fit)
                });
                if response.hovered() && !self.dialog_open() {
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
                            later = Some(if up { Action::Prev } else { Action::Next });
                            self.wheel = 0.0;
                        }
                    }
                    if factor != 1.0 {
                        let old = self.zoom.unwrap_or(fit);
                        let new = (old * factor).clamp(0.02, 32.0);
                        self.zoom = Some(new);
                        // Keep the point under the pointer in place, as in other viewers.
                        if let Some(p) = response.hover_pos() {
                            let from_centre = p - area.center();
                            self.pan = from_centre - (from_centre - self.pan) * (new / old);
                        }
                    }
                }
                if response.double_clicked() {
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
                        self.pan += response.drag_delta();
                    }
                    let size = egui::vec2(w as f32, h as f32) * scale / ppp;
                    let max = ((size - area.size()) / 2.0).max(egui::Vec2::ZERO);
                    self.pan = self.pan.clamp(-max, max);
                    self.max_pan = max;
                }

                // Texture too small: in fit mode the window has grown, so decode again at window
                // size (the previous texture is shown meanwhile); when zoomed, load full resolution.
                let too_small = self.slots.get(&self.cur).is_some_and(|s| {
                    let needed = (s.orig[0] as f32 * scale).min(s.orig[0] as f32);
                    !s.full && (s.tex.size()[0] as f32) + 1.0 < needed
                });
                if too_small && self.zoom.is_none() {
                    self.slots.remove(&self.cur);
                    self.request(self.cur, false);
                } else if too_small {
                    self.request(self.cur, true);
                }
                if let Some(slot) = self.slots.get(&self.cur) {
                    let size = egui::vec2(slot.orig[0] as f32, slot.orig[1] as f32) * scale / ppp;
                    let rect = egui::Rect::from_center_size(area.center() + self.pan, size);
                    let (turns, mirrored) = self.orient;
                    let uv = if mirrored {
                        egui::Rect::from_min_max(egui::pos2(1.0, 0.0), egui::pos2(0.0, 1.0))
                    } else {
                        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0))
                    };
                    let angle = turns as f32 * std::f32::consts::FRAC_PI_2;
                    let tex = if self.config.animate {
                        slot.frame(self.opened.elapsed(), ctx)
                    } else {
                        &slot.tex
                    };
                    egui::Image::new(tex)
                        .uv(uv)
                        .rotate(angle, egui::Vec2::splat(0.5))
                        .paint_at(ui, rect);
                    self.last_drawn = Some((slot.tex.clone(), slot.orig));
                } else if let Some((tex, orig)) = &self.last_drawn
                    && !self.files.is_empty()
                    && !self.errors.contains_key(&self.cur)
                {
                    let size = egui::vec2(orig[0] as f32, orig[1] as f32);
                    let fit = (area.width() * ppp / size.x)
                        .min(area.height() * ppp / size.y)
                        .min(max_fit);
                    let rect = egui::Rect::from_center_size(area.center(), size * fit / ppp);
                    egui::Image::new(tex).paint_at(ui, rect);
                } else {
                    let text = if self.files.is_empty() {
                        if self.listing.is_some() {
                            "Reading folder…"
                        } else {
                            "No images in folder"
                        }
                        .to_owned()
                    } else if let Some(e) = self.errors.get(&self.cur) {
                        format!("Could not open {}:\n{e}", self.path(self.cur).display())
                    } else {
                        String::new()
                    };
                    ui.painter().text(
                        area.center(),
                        egui::Align2::CENTER_CENTER,
                        text,
                        egui::FontId::proportional(16.0),
                        egui::Color32::GRAY,
                    );
                }
                // A video: a play button over the preview frame (click or Enter), readable on
                // any frame, also in the slideshow and full screen.
                #[cfg(feature = "video")]
                if self
                    .files
                    .get(self.cur)
                    .is_some_and(|f| is_video(Path::new(f)))
                {
                    let shade = egui::Color32::from_black_alpha(150);
                    let button =
                        egui::Rect::from_center_size(area.center(), egui::vec2(96.0, 96.0));
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
                        "Video: Enter or click to play".into(),
                        egui::FontId::proportional(16.0),
                        egui::Color32::WHITE,
                    );
                    let at = egui::Rect::from_center_size(
                        button.center_bottom() + egui::vec2(0.0, 28.0),
                        label.size() + egui::vec2(20.0, 10.0),
                    );
                    p.rect_filled(at, 6.0, shade);
                    p.galley(at.min + egui::vec2(10.0, 5.0), label, egui::Color32::WHITE);
                    if play.clicked() {
                        later = Some(Action::Play);
                    }
                }
                if bare && self.config.osd && !self.files.is_empty() {
                    self.overlay(ui, area);
                }
                if self.config.histogram > 0 {
                    self.paint_histogram(ui, area);
                }
                if let Some(right) = right {
                    self.paint_other(ui, right, self.zoom);
                }
                if let Some(notice) = &self.notice {
                    ui.painter().text(
                        area.center_bottom() - egui::vec2(0.0, 16.0),
                        egui::Align2::CENTER_BOTTOM,
                        notice,
                        egui::FontId::proportional(16.0),
                        egui::Color32::LIGHT_RED,
                    );
                }
            });
        if let Some(action) = later {
            self.run(ctx, action);
        }
    }
}

fn main() -> eframe::Result {
    // One malloc arena for all threads. With one per thread, freed image buffers stayed in
    // each decode thread's arena: 91 instead of 29 MB of own memory in a folder of
    // screenshots, while decoding is at most a few percent slower.
    if Config::load().low_memory {
        // SAFETY: called before any other thread exists.
        unsafe { mallopt(M_ARENA_MAX, 1) };
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|a| a == "--help" || a == "-h") {
        println!(
            "Usage: glimt [FILE_OR_FOLDER]\n       glimt --remote COMMAND      to the Glimt used last\n       glimt --remote-all COMMAND  to every running Glimt\n\nCommands:"
        );
        for (command, does) in remote::COMMANDS {
            println!("  {command:<12} {does}");
        }
        return Ok(());
    }
    let mut arg = std::env::args_os().nth(1).map(PathBuf::from);
    let to_all = args.first().is_some_and(|a| a == "--remote-all");
    if args.first().is_some_and(|a| a == "--remote") || to_all {
        let mut command = args[1..].join(" ");
        // The running Glimt has another working folder: send the full path.
        if let Some(path) = command.strip_prefix("open ") {
            let path = std::path::absolute(path).unwrap_or_else(|_| path.into());
            command = format!("open {}", path.display());
        }
        let sent = if to_all {
            remote::send_all(&command) > 0
        } else {
            remote::send(&command)
        };
        if sent {
            return Ok(());
        }
        match command.strip_prefix("open ") {
            Some(path) => arg = Some(path.into()),
            None => {
                eprintln!("Glimt is not running");
                std::process::exit(1);
            }
        }
    }
    // The app ID matches glimt.desktop, so the desktop shows its icon for the window.
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Glimt")
        .with_app_id("glimt")
        .with_inner_size([1280.0, 800.0]);
    if let Ok(icon) = image::load_from_memory(include_bytes!("../assets/glimt-256.png")) {
        let icon = icon.into_rgba8();
        viewport = viewport.with_icon(egui::IconData {
            width: icon.width(),
            height: icon.height(),
            rgba: icon.into_raw(),
        });
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
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "glimt",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, arg)))),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn lists_folder_and_decodes_to_screen_size() {
        let dir = std::env::temp_dir().join("glimt-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbImage::new(400, 200)
            .save(dir.join("a.png"))
            .unwrap();
        std::fs::write(dir.join("notes.txt"), "").unwrap();
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::create_dir_all(dir.join(".hidden")).unwrap();
        let (dirs, images) = super::list_folder(&dir, true);
        assert_eq!(images, vec![std::ffi::OsString::from("a.png")]);
        assert_eq!(dirs, vec![dir.join("sub")]);
        let (frames, orig, _) = super::decode(&dir.join("a.png"), [100, 100], true, 0).unwrap();
        assert_eq!(
            (frames.len(), frames[0].0.size, orig),
            (1, [100, 50], [400, 200])
        );
        // An animated GIF gives every frame with its delay (5 ms counts as 100 ms).
        let gif = std::fs::File::create(dir.join("b.gif")).unwrap();
        let frame = |ms| {
            let delay = image::Delay::from_numer_denom_ms(ms, 1);
            image::Frame::from_parts(image::RgbaImage::new(40, 20), 0, 0, delay)
        };
        image::codecs::gif::GifEncoder::new(gif)
            .encode_frames([frame(50), frame(5)])
            .unwrap();
        let (frames, orig, _) = super::decode(&dir.join("b.gif"), [100, 100], true, 0).unwrap();
        let delays: Vec<u32> = frames.iter().map(|f| f.1).collect();
        assert_eq!((delays, orig), (vec![50, 100], [40, 20]));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// An Adobe RGB photo is converted to sRGB: the same numbers mean a more saturated colour,
    /// so in sRGB they come out different. Needs colord's profile; skipped without it.
    #[test]
    fn converts_icc_to_srgb() {
        let Ok(adobe) = std::fs::read("/usr/share/color/icc/colord/AdobeRGB1998.icc") else {
            return;
        };
        let img = image::DynamicImage::from(image::RgbaImage::from_pixel(
            2,
            2,
            image::Rgba([60, 160, 60, 255]),
        ));
        let out = super::to_srgb(img.clone(), Some(&adobe)).into_rgba8();
        assert_ne!(out.get_pixel(0, 0), img.into_rgba8().get_pixel(0, 0));
        // Not a usable profile: unchanged.
        let img = image::DynamicImage::from(image::RgbaImage::new(1, 1));
        assert_eq!(super::to_srgb(img.clone(), Some(b"junk")), img);
    }

    /// image writes lossless WebP (VP8L); anything else is not taken for one.
    #[test]
    fn tells_lossless_webp() {
        let file = std::env::temp_dir().join(format!("glimt-{}.webp", std::process::id()));
        image::RgbImage::new(4, 4).save(&file).unwrap();
        assert_eq!(super::webp_lossless(&file), Some(true));
        std::fs::write(&file, b"RIFF\x04\0\0\0WEBP").unwrap();
        assert_eq!(super::webp_lossless(&file), None);
        std::fs::remove_file(file).unwrap();
    }

    /// Own keys survive the config file, and replace the defaults of their action.
    #[test]
    fn own_keys() {
        use super::{Action, Keys};
        let keys: Keys = "Next=Ctrl+Shift+N;ZoomIn=Plus;Bogus=X;Prev=Hyper+P"
            .parse()
            .unwrap();
        assert_eq!(keys.to_string(), "Next=Ctrl+Shift+N;ZoomIn=Plus");
        assert_eq!(keys.to_string().parse::<Keys>().unwrap(), keys);
        *super::OWN_KEYS.write().unwrap() = keys.0.clone();
        let all = super::shortcuts();
        let next: Vec<_> = all.iter().filter(|s| s.0 == Action::Next).collect();
        assert_eq!(next.len(), 1);
        // More modifiers first, so Shift+R (rotate) is matched before R.
        let pos = |a| all.iter().position(|s| s.0 == a).unwrap();
        assert!(pos(Action::Rotate180) < pos(Action::RandomOrder));
        super::OWN_KEYS.write().unwrap().clear();
    }

    #[test]
    fn pattern_names() {
        let old = std::path::Path::new("/p/IMG_4711.JPG");
        assert_eq!(super::pattern_name("trip_###", old, 7), "trip_007.JPG");
        assert_eq!(super::pattern_name("*-#", old, 12), "IMG_4711-12.JPG");
        assert_eq!(
            super::pattern_name("x", std::path::Path::new("/p/noext"), 1),
            "x"
        );
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
    }

    #[test]
    fn sorts_numbers_by_value() {
        use std::cmp::Ordering::*;
        assert_eq!(super::natural_cmp("img2.jpg", "img10.jpg"), Less);
        assert_eq!(super::natural_cmp("img010", "img10"), Equal);
        assert_eq!(super::natural_cmp("a", "a1"), Less);
        let mut v: Vec<std::ffi::OsString> =
            ["b10", "B2", "a1", "b9"].iter().map(Into::into).collect();
        super::sort_files(
            std::path::Path::new("."),
            &mut v,
            super::Sort::Number,
            false,
            false,
        );
        assert_eq!(v, ["b10", "b9", "B2", "a1"]);
        let mut v: Vec<std::ffi::OsString> = ["b.png", "c.JPG", "a.mp4", "d.jpg", "e"]
            .iter()
            .map(Into::into)
            .collect();
        let dot = std::path::Path::new(".");
        super::sort_files(dot, &mut v, super::Sort::Extension, true, false);
        assert_eq!(v, ["e", "c.JPG", "d.jpg", "a.mp4", "b.png"]);
    }

    #[test]
    fn formats_local_time() {
        let t = super::local_time(1_700_000_000);
        assert_eq!(t.len(), "2023-11-14 22:13:20".len(), "{t}");
        assert!(t.starts_with("2023-11-1"), "{t}");
    }

    #[test]
    fn merge_keeps_shuffled_order() {
        let names = |v: &[&str]| v.iter().map(std::ffi::OsString::from).collect::<Vec<_>>();
        let merged = super::merge_shuffled(&names(&["c", "a", "b"]), names(&["a", "b", "d"]));
        assert_eq!(merged, names(&["a", "b", "d"]));
    }

    #[test]
    fn shuffle_keeps_all_elements() {
        let mut v: Vec<u32> = (0..100).collect();
        super::shuffle(&mut v);
        assert_ne!(v, (0..100).collect::<Vec<_>>());
        v.sort();
        assert_eq!(v, (0..100).collect::<Vec<_>>());
    }

    #[test]
    fn writes_atomically() {
        let dir = std::env::temp_dir().join("glimt-test-atomic");
        let path = dir.join("config");
        super::write_atomic(&path, "a=1\n");
        super::write_atomic(&path, "a=2\n");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a=2\n");
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            1,
            "no .tmp files left"
        );
    }

    #[test]
    fn parses_config() {
        use super::Config;
        assert_eq!(Config::parse("# comment\npreload= 5\n").preload, 5);
        assert_eq!(Config::parse("preload=99").preload, 20);
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
            (super::Sort::ExifDigitized, false, false)
        );
        assert_eq!(Config::parse("sort=nonsense").sort, super::Sort::Name);
        assert_eq!(Config::default().toolbar_icons, 0);
        assert!(Config::default().search_bar && !Config::parse("search_bar=false").search_bar);
        assert_eq!(Config::parse("toolbar_icons=1").toolbar_icons, 1);
        assert_eq!(Config::parse("toolbar_icons=9").toolbar_icons, 2);
        assert!(Config::parse("app_name_first=true").app_name_first);
        let win = super::WindowState {
            size: Some(super::egui::vec2(1000.0, 700.5)),
            pos: Some(super::egui::pos2(10.0, -20.0)),
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
    }
}
