//! Everything the user can do from a menu or a key: the Action list with menu labels, Geeqie's
//! default keys (SHORTCUTS), and the user's own keys from Preferences.

use crate::features::duplicates::Dups;
use crate::images::colour::FILTERS;
use crate::images::decode::{color_image, survive};
#[cfg(feature = "marks")]
use crate::metadata::marks;
#[cfg(feature = "marks")]
use crate::support::format::count;
use crate::support::lang::tr;
use crate::support::threads::Working;
use crate::ui::dialogs::{Pick, Prompt};
use crate::viewer::pan::Pan;
use crate::viewer::view::LAYOUTS;
use crate::{
    features::remote, files::config, images::save, model::collections, own_program, tf, ui::help,
};
use eframe::egui;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc;

pub const NONE: egui::Modifiers = egui::Modifiers::NONE;

pub const SHIFT: egui::Modifiers = egui::Modifiers::SHIFT;

pub const CTRL: egui::Modifiers = egui::Modifiers::COMMAND;

pub const ALT: egui::Modifiers = egui::Modifiers::ALT;

pub const CTRL_SHIFT: egui::Modifiers = egui::Modifiers {
    shift: true,
    ..egui::Modifiers::COMMAND
};

/// Geeqie's default shortcuts, except R (random order; Geeqie: refresh) and F5 (refresh). The first
/// one for an action is shown in the menu. egui ignores an extra Shift or Alt when matching, so
/// those combinations come before the plain key.
pub const SHORTCUTS: &[(Action, egui::Modifiers, egui::Key)] = {
    use Action::*;
    use egui::Key;
    &[
        (ExifRotate, ALT, Key::X),
        // Before the plain arrows, Home and End, which would also match with Shift.
        (SaveAs, CTRL_SHIFT, Key::S),
        (MoveEarlier, CTRL_SHIFT, Key::ArrowUp),
        (MoveLater, CTRL_SHIFT, Key::ArrowDown),
        (MoveFirst, CTRL_SHIFT, Key::Home),
        (MoveLast, CTRL_SHIFT, Key::End),
        // Before R (random order) and F (full screen), which would also match with Shift.
        (Rotate180, SHIFT, Key::R),
        (Mirror, SHIFT, Key::M),
        (Flip, SHIFT, Key::F),
        (NextFilter, SHIFT, Key::G),
        (RotateCw, NONE, Key::CloseBracket),
        (RotateCcw, NONE, Key::OpenBracket),
        (DeletePermanently, SHIFT, Key::Delete),
        // Only in a collection, where Delete takes the file out of it, as in Geeqie.
        (RemoveFromCollection, NONE, Key::Delete),
        (Delete, NONE, Key::Delete),
        (Delete, CTRL, Key::D),
        (NewFolder, CTRL, Key::F),
        (Rename, CTRL, Key::R),
        (Undo, CTRL, Key::Z),
        // Before Ctrl+C (copy to a folder), which would also match with Shift.
        (CopyImage, CTRL_SHIFT, Key::C),
        (CopyTo, CTRL, Key::C),
        (MoveTo, CTRL, Key::M),
        (CloseWindow, CTRL, Key::W),
        (Quit, CTRL, Key::Q),
        (First, NONE, Key::Home),
        // Before the plain arrows, which would also match with Alt.
        (PaneLeft, ALT, Key::ArrowLeft),
        (PaneRight, ALT, Key::ArrowRight),
        (PaneUp, ALT, Key::ArrowUp),
        (PaneDown, ALT, Key::ArrowDown),
        (Previous, NONE, Key::Backspace),
        (Previous, NONE, Key::PageUp),
        (Previous, NONE, Key::ArrowLeft),
        (Previous, NONE, Key::ArrowUp),
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
        (FitWidth, NONE, Key::H),
        (FitHeight, NONE, Key::W),
        (FullScreen, NONE, Key::F),
        (FullScreen, NONE, Key::V),
        (FullScreen, NONE, Key::F11),
        (HideFileList, CTRL, Key::H),
        (Thumbnails, NONE, Key::T),
        (ThumbnailGrid, NONE, Key::G),
        (FullScreenInfo, NONE, Key::I),
        (Histogram, NONE, Key::K),
        (HistogramLog, NONE, Key::J),
        (Compare, NONE, Key::E),
        (OneImage, NONE, Key::Y),
        (AboveEachOther, NONE, Key::U),
        (NextLayout, NONE, Key::L),
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
        (InfoPanel, CTRL, Key::K),
        (ExifWindow, CTRL, Key::E),
        (FocusKeywords, ALT, Key::K),
        (PanView, CTRL, Key::J),
        (FolderTree, CTRL, Key::T),
        (Refresh, NONE, Key::F5),
        // Before S (slideshow), which would also match with Shift.
        (SortManager, SHIFT, Key::S),
        (Slideshow, NONE, Key::S),
        (Pause, NONE, Key::P),
        (RandomOrder, NONE, Key::R),
        (SlideshowFaster, CTRL, Key::Equals),
        (SlideshowSlower, CTRL, Key::Minus),
        (PlayVideo, NONE, Key::Enter),
        (Escape, NONE, Key::Escape),
        (Escape, NONE, Key::Q),
        (NewWindow, CTRL, Key::N),
        (Print, CTRL, Key::P),
        // Before C (new collection), which would also match with Shift.
        (Crop, SHIFT, Key::C),
        (NewCollection, NONE, Key::C),
        (OpenCollection, NONE, Key::O),
        (FindDuplicates, NONE, Key::D),
        (FocusSearch, NONE, Key::Slash),
        (Help, NONE, Key::F1),
    ]
};

/// The user's own keys (Preferences), which replace the defaults of their actions.
pub static OWN_KEYS: std::sync::RwLock<Vec<(Action, egui::Modifiers, egui::Key)>> =
    std::sync::RwLock::new(Vec::new());

/// Held by the tests that set `OWN_KEYS`, and by every new window of a test, which sets them
/// to its own settings' keys, as tests run at once.
#[cfg(test)]
pub static KEYS_TEST: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The actions a kiosk allows: showing and browsing, not changing files, settings or the
/// window.
pub(crate) const KIOSK_ACTIONS: &[Action] = &[
    Action::Next,
    Action::Previous,
    Action::First,
    Action::Last,
    Action::Slideshow,
    Action::Pause,
    Action::SlideshowFaster,
    Action::SlideshowSlower,
    Action::RandomOrder,
    Action::ZoomIn,
    Action::ZoomOut,
    Action::Zoom1To1,
    Action::ZoomFit,
    Action::FitWidth,
    Action::FitHeight,
    Action::FitFill,
    Action::FullScreenInfo,
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

/// The actions a script may run with `glimt --remote run ACTION`: showing, browsing, selecting,
/// the panels and windows, the settings and the clipboard. None of them changes a file, a
/// sidecar, a mark or star, or a collection, not even through a dialog or panel the user then
/// answers or clicks in, and none starts a program of the user's. So a program that reaches
/// Glimt's socket still cannot delete, move, rename or rewrite anything.
/// `every_action_is_decided_for_scripts` makes each new action be put here or left out on
/// purpose.
pub(crate) const SCRIPT_ACTIONS: &[Action] = &[
    Action::About,
    Action::AboveEachOther,
    Action::Animate,
    Action::Back,
    Action::CheckImages,
    Action::CloseWindow,
    Action::Compare,
    Action::CopyFiles,
    Action::CopyImage,
    Action::CopyPath,
    Action::CopyPathUnquoted,
    Action::Escape,
    Action::ExifRotate,
    Action::ExifWindow,
    Action::First,
    Action::FitFill,
    Action::FitHeight,
    Action::FitWidth,
    Action::FocusSearch,
    Action::FolderTree,
    Action::Forward,
    Action::FullScreen,
    Action::FullScreenInfo,
    Action::GridNames,
    Action::Help,
    Action::HiddenFiles,
    Action::HideFileList,
    Action::Histogram,
    Action::HistogramLog,
    Action::Home,
    Action::Icons,
    Action::InvertSelection,
    Action::KeyboardMap,
    Action::Last,
    Action::NewWindow,
    Action::Next,
    Action::NextFilter,
    Action::NextLayout,
    Action::NextPage,
    Action::OneImage,
    Action::OpenCollection,
    Action::OpenFile,
    Action::OpenFolder,
    Action::OpenFolders,
    Action::OpenRecursive,
    Action::PanView,
    Action::PaneDown,
    Action::PaneLeft,
    Action::PaneRight,
    Action::PaneUp,
    Action::PathBar,
    Action::Pause,
    Action::Preferences,
    Action::PrevPage,
    Action::Previous,
    Action::Projector,
    Action::ProjectorPointer,
    Action::Quit,
    Action::RandomOrder,
    Action::Refresh,
    Action::SelectAll,
    Action::SelectNone,
    Action::Shortcuts,
    Action::Slideshow,
    Action::SlideshowFaster,
    Action::SlideshowSlower,
    Action::Subfolders,
    Action::ThumbnailGrid,
    Action::Thumbnails,
    Action::Toolbar,
    Action::Up,
    Action::WindowTitle,
    Action::Zoom1To1,
    Action::ZoomFit,
    Action::ZoomIn,
    Action::ZoomOut,
    #[cfg(feature = "marks")]
    Action::SelectMark1,
    #[cfg(feature = "marks")]
    Action::SelectMark2,
    #[cfg(feature = "marks")]
    Action::SelectMark3,
    #[cfg(feature = "marks")]
    Action::SelectMark4,
    #[cfg(feature = "marks")]
    Action::SelectMark5,
    #[cfg(feature = "marks")]
    Action::SelectMark6,
];

/// The actions renamed in 0.74.0, as (old name, new name). The keys= setting and
/// `glimt --remote run` take the old names too, forever, so users' settings and scripts keep
/// working, and only the new names are written and listed.
pub(crate) const RENAMED_ACTIONS: [(&str, &str); 11] = [
    ("HideTools", "HideFileList"),
    ("Overlay", "FullScreenInfo"),
    ("Play", "PlayVideo"),
    ("Faster", "SlideshowFaster"),
    ("Slower", "SlideshowSlower"),
    ("Keywords", "FocusKeywords"),
    ("Copy", "CopyTo"),
    ("Move", "MoveTo"),
    ("ViewFilter", "NextFilter"),
    ("ExifPanel", "InfoPanel"),
    ("Prev", "Previous"),
];

/// Everything the user can do from a menu or a shortcut. The variants' names are stored in the
/// settings for the user's own keys (`{a:?}`). A variant is renamed only through
/// `RENAMED_ACTIONS`, because otherwise the users' keys for it would be lost.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Action {
    OpenFile,
    OpenFolder,
    OpenFolders,
    OpenRecursive,
    NewFolder,
    Rename,
    CopyTo,
    MoveTo,
    OpenWith,
    SetWallpaper,
    PlayVideo,
    Delete,
    DeletePermanently,
    Undo,
    CloseWindow,
    Quit,
    First,
    Previous,
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
    NextFilter,
    RotateCw,
    RotateCcw,
    Rotate180,
    Mirror,
    Flip,
    SaveOrientation,
    Crop,
    SaveAs,
    ZoomIn,
    ZoomOut,
    Zoom1To1,
    ZoomFit,
    FitWidth,
    FitHeight,
    FitFill,
    FullScreen,
    HideFileList,
    FullScreenInfo,
    Histogram,
    HistogramLog,
    Compare,
    OneImage,
    AboveEachOther,
    NextLayout,
    PaneLeft,
    PaneRight,
    PaneUp,
    PaneDown,
    Animate,
    Subfolders,
    HiddenFiles,
    NewCollection,
    OpenCollection,
    ImportLightroom,
    RemoveFromCollection,
    RenameCollection,
    TrashCollection,
    MoveEarlier,
    MoveLater,
    MoveFirst,
    MoveLast,
    FindDuplicates,
    CheckImages,
    BackUp,
    FocusSearch,
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
    ThumbnailGrid,
    GridNames,
    InfoPanel,
    ExifWindow,
    FocusKeywords,
    SortManager,
    PanView,
    FolderTree,
    Refresh,
    Slideshow,
    Pause,
    RandomOrder,
    SlideshowFaster,
    SlideshowSlower,
    Escape,
    NewWindow,
    WindowTitle,
    Projector,
    ProjectorPointer,
    Print,
    SaveFrame,
    ExportFrames,
    Help,
    Shortcuts,
    KeyboardMap,
    About,
}

impl Action {
    /// Returns the action a script may run under `name`, its name in `keys=` and
    /// src/keys-1.0.txt, or `None` when there is no such action or it is not in
    /// `SCRIPT_ACTIONS`.
    pub fn for_script(name: &str) -> Option<Action> {
        let name = action_name_now(name);
        (SCRIPT_ACTIONS.iter())
            .find(|a| format!("{a:?}") == name)
            .copied()
    }

    /// Whether this build does something for the action. PlayVideo needs the video feature, so
    /// without it Enter is not taken by an action that does nothing.
    pub fn in_build(self) -> bool {
        self != Action::PlayVideo || cfg!(feature = "video")
    }

    /// For the marks actions: 'm' (toggle mark), 's' (select by mark) or 'r' (stars), and the
    /// number.
    #[cfg(feature = "marks")]
    pub fn mark(self) -> Option<(char, u8)> {
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

    /// The name shown in menus and in the help window.
    pub fn label(self) -> &'static str {
        crate::support::lang::tr(self.english())
    }

    /// The name in English, as Preferences' search also looks for it.
    pub fn english(self) -> &'static str {
        use Action::*;
        match self {
            OpenFile => "Open File…",
            OpenFolder => "Open Folder…",
            OpenFolders => "Open Several Folders…",
            OpenRecursive => "Open Folder Recursive…",
            NewFolder => "New Folder…",
            Rename => "Rename…",
            CopyTo => "Copy…",
            MoveTo => "Move…",
            OpenWith => "Open With Default App",
            SetWallpaper => "Set as Wallpaper",
            PlayVideo => "Play Video",
            Delete => "Delete…",
            DeletePermanently => "Delete Permanently…",
            Undo => "Undo",
            CloseWindow => "Close Window",
            Quit => "Quit",
            First => "First Image",
            Previous => "Previous Image",
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
            NextFilter => "Next Filter",
            RotateCw => "Rotate Clockwise",
            RotateCcw => "Rotate Counterclockwise",
            Rotate180 => "Rotate 180°",
            Mirror => "Mirror",
            Flip => "Flip",
            SaveOrientation => "Save Orientation to File",
            Crop => "Crop…",
            SaveAs => "Save As…",
            ZoomIn => "Zoom In",
            ZoomOut => "Zoom Out",
            Zoom1To1 => "Zoom 1:1",
            ZoomFit => "Zoom to Fit",
            FitWidth => "Fit the Width",
            FitHeight => "Fit the Height",
            FitFill => "Fill the View",
            FullScreen => "Full Screen",
            HideFileList => "Hide File List",
            FullScreenInfo => "Show Image Info in Full Screen",
            Histogram => "Histogram: Off, RGB, Brightness",
            HistogramLog => "Logarithmic Histogram",
            Compare => "Compare Side by Side",
            OneImage => "One Image",
            AboveEachOther => "Two Above Each Other",
            NextLayout => "Next Layout",
            PaneLeft => "Pane to the Left",
            PaneRight => "Pane to the Right",
            PaneUp => "Pane Above",
            PaneDown => "Pane Below",
            Animate => "Play Animations",
            Subfolders => "Include Subfolders",
            HiddenFiles => "Show Hidden Files",
            NewCollection => "New Collection…",
            OpenCollection => "Open Collection…",
            ImportLightroom => "Import Lightroom Catalog…",
            RemoveFromCollection => "Remove from Collection",
            RenameCollection => "Rename Collection…",
            TrashCollection => "Move Collection to Trash",
            MoveEarlier => "Move Earlier in Collection",
            MoveLater => "Move Later in Collection",
            MoveFirst => "Move to Start of Collection",
            MoveLast => "Move to End of Collection",
            FindDuplicates => "Find Duplicates…",
            CheckImages => "Check Images…",
            BackUp => "Back Up Glimt's Data…",
            FocusSearch => "Search File Names",
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
            ThumbnailGrid => "Thumbnails in Image Area",
            GridNames => "Show File Names Under Thumbnails",
            InfoPanel => "Show Info Panel",
            SortManager => "Show Sort Manager",
            ExifWindow => "EXIF Window",
            FocusKeywords => "Keywords and Comment",
            PanView => "Pan View",
            FolderTree => "Show Folder Tree",
            Refresh => "Refresh",
            Slideshow => "Start/Stop Slideshow",
            Pause => "Pause Slideshow",
            RandomOrder => "Random Order",
            SlideshowFaster => "Faster",
            SlideshowSlower => "Slower",
            Escape => "Stop Slideshow, Leave Full Screen",
            NewWindow => "New Window",
            WindowTitle => "Set Window Title…",
            Projector => "Projector",
            ProjectorPointer => "Show the Pointer on the Projector",
            Print => "Print…",
            SaveFrame => "Save This Frame…",
            ExportFrames => "Export Frames…",
            Help => "Help Manual",
            Shortcuts => "Keyboard Shortcuts",
            KeyboardMap => "Keyboard Map",
            About => "About Glimt",
        }
    }

    /// The Phosphor icon before the label in the menus (theme::icon has the others).
    pub fn icon(self) -> &'static str {
        use Action::*;
        match self {
            OpenFile => "\u{e230}",                         // file
            OpenFolder => "\u{e256}",                       // folder-open
            OpenFolders | Subfolders => "\u{e260}",         // folders
            OpenRecursive | FolderTree => "\u{e67c}",       // tree-structure
            NewFolder => "\u{e258}",                        // folder-plus
            Rename | RenameCollection => "\u{e3b4}",        // pencil-simple
            WindowTitle => "\u{e3b4}",                      // pencil-simple
            CopyTo => "\u{e1ca}",                           // copy
            MoveTo | Forward | PaneRight => "\u{e06c}",     // arrow-right
            OpenWith => "\u{e5de}",                         // arrow-square-out
            SetWallpaper => "\u{e32e}",                     // monitor
            PlayVideo => "\u{e792}",                        // film-strip
            Delete | TrashCollection => "\u{e4a6}",         // trash
            DeletePermanently => "\u{e4f8}",                // x-circle
            Undo => "\u{e08a}",                             // arrow-u-up-left
            CloseWindow => "\u{e4fa}",                      // x-square
            Quit => "\u{e3da}",                             // power
            First => "\u{e5a4}",                            // skip-back
            Previous => "\u{e138}",                         // caret-left
            Next => "\u{e13a}",                             // caret-right
            Last => "\u{e5a6}",                             // skip-forward
            Back | PaneLeft => "\u{e058}",                  // arrow-left
            Home => "\u{e2c2}",                             // house
            Up | PaneUp | MoveEarlier => "\u{e08e}",        // arrow-up
            SelectAll => "\u{e746}",                        // selection-all
            SelectNone => "\u{e69e}",                       // selection-slash
            InvertSelection => "\u{e744}",                  // selection-inverse
            CopyPath => "\u{e198}",                         // clipboard-text
            CopyPathUnquoted => "\u{e196}",                 // clipboard
            CopyImage => "\u{e2cc}",                        // image-square
            CopyFiles => "\u{e710}",                        // files
            Preferences => "\u{e270}",                      // gear
            ExifRotate | Rotate180 | Refresh => "\u{e094}", // arrows-clockwise
            NextFilter | NextLayout => "\u{e83c}",          // swap
            RotateCw => "\u{e036}",                         // arrow-clockwise
            RotateCcw => "\u{e038}",                        // arrow-counter-clockwise
            Mirror => "\u{ed6a}",                           // flip-horizontal
            Flip => "\u{ed6c}",                             // flip-vertical
            SaveOrientation => "\u{e248}",                  // floppy-disk
            Crop => "\u{e1d4}",                             // crop
            SaveAs => "\u{e20a}",                           // download
            ZoomIn => "\u{e310}",                           // magnifying-glass-plus
            ZoomOut => "\u{e30e}",                          // magnifying-glass-minus
            Zoom1To1 => "\u{e626}",                         // frame-corners
            ZoomFit => "\u{e1d0}",                          // corners-out
            FitWidth => "\u{eb06}",                         // arrows-horizontal
            FitHeight => "\u{eb04}",                        // arrows-vertical
            FitFill => "\u{e0a2}",                          // arrows-out
            FullScreen => "\u{e0a6}",                       // arrows-out-simple
            HideFileList => "\u{ec24}",                     // sidebar-simple
            FullScreenInfo | GridNames => "\u{e1a8}",       // subtitles
            Histogram => "\u{e150}",                        // chart-bar
            HistogramLog => "\u{ebe4}",                     // function
            Compare => "\u{e546}",                          // columns
            OneImage => "\u{e45e}",                         // square
            AboveEachOther => "\u{e5a2}",                   // rows
            PaneDown | MoveLater => "\u{e03e}",             // arrow-down
            Animate => "\u{e3d2}",                          // play-circle
            HiddenFiles => "\u{e224}",                      // eye-slash
            NewCollection => "\u{edf6}",                    // stack-plus
            OpenCollection => "\u{e466}",                   // stack
            ImportLightroom => "\u{e20c}",                  // download-simple
            RemoveFromCollection => "\u{e32c}",             // minus-circle
            MoveFirst => "\u{e066}",                        // arrow-line-up
            MoveLast => "\u{e05c}",                         // arrow-line-down
            FindDuplicates => "\u{e2d2}",                   // intersect
            CheckImages => "\u{e184}",                      // check-circle
            BackUp => "\u{e00c}",                           // archive
            FocusSearch => "\u{e30c}",                      // magnifying-glass
            NextPage => "\u{e12a}",                         // caret-double-right
            PrevPage => "\u{e128}",                         // caret-double-left
            #[cfg(feature = "marks")]
            Mark1 => "\u{e36a}", // number-circle-one
            #[cfg(feature = "marks")]
            Mark2 => "\u{e382}", // number-circle-two
            #[cfg(feature = "marks")]
            Mark3 => "\u{e37c}", // number-circle-three
            #[cfg(feature = "marks")]
            Mark4 => "\u{e35e}", // number-circle-four
            #[cfg(feature = "marks")]
            Mark5 => "\u{e358}", // number-circle-five
            #[cfg(feature = "marks")]
            Mark6 => "\u{e376}", // number-circle-six
            #[cfg(feature = "marks")]
            SelectMark1 => "\u{e36c}", // number-square-one
            #[cfg(feature = "marks")]
            SelectMark2 => "\u{e384}", // number-square-two
            #[cfg(feature = "marks")]
            SelectMark3 => "\u{e37e}", // number-square-three
            #[cfg(feature = "marks")]
            SelectMark4 => "\u{e360}", // number-square-four
            #[cfg(feature = "marks")]
            SelectMark5 => "\u{e35a}", // number-square-five
            #[cfg(feature = "marks")]
            SelectMark6 => "\u{e378}", // number-square-six
            #[cfg(feature = "marks")]
            Rate0 => "\u{e3de}", // prohibit
            #[cfg(feature = "marks")]
            Rate1 => "\u{e46a}", // star
            #[cfg(feature = "marks")]
            Rate2 => "\u{e46a}", // star
            #[cfg(feature = "marks")]
            Rate3 => "\u{e46a}", // star
            #[cfg(feature = "marks")]
            Rate4 => "\u{e46a}", // star
            #[cfg(feature = "marks")]
            Rate5 => "\u{e46a}", // star
            Icons => "\u{ec8c}",                            // grid-nine
            Toolbar => "\u{eca0}",                          // toolbox
            PathBar => "\u{e89c}",                          // signpost
            Thumbnails => "\u{e836}",                       // images
            ThumbnailGrid => "\u{e464}",                    // squares-four
            InfoPanel | About => "\u{e2ce}",                // info
            ExifWindow | SaveFrame => "\u{e10e}",           // camera
            FocusKeywords => "\u{e478}",                    // tag
            SortManager => "\u{e444}",                      // sort-ascending
            PanView => "\u{e7b4}",                          // calendar-dots
            Slideshow => "\u{e3d0}",                        // play
            Pause => "\u{e39e}",                            // pause
            RandomOrder => "\u{e422}",                      // shuffle
            SlideshowFaster => "\u{e6a6}",                  // fast-forward
            SlideshowSlower => "\u{e6a8}",                  // rewind
            Escape => "\u{e4f6}",                           // x
            NewWindow => "\u{e0f6}",                        // browsers
            Projector => "\u{e658}",                        // projector-screen
            ProjectorPointer => "\u{e1dc}",                 // cursor
            Print => "\u{e3dc}",                            // printer
            ExportFrames => "\u{eaf0}",                     // export
            Help => "\u{e0e6}",                             // book-open
            Shortcuts => "\u{e2d8}",                        // keyboard
            KeyboardMap => "\u{e31a}",                      // map-trifold
        }
    }

    /// What the action acts on: the selection, the shown image or no file. A new action names
    /// its target here, and a menu about the selection offers only actions that act on it.
    pub(crate) fn target(self) -> Target {
        use Action::*;
        match self {
            Rename | CopyTo | MoveTo | Delete | DeletePermanently | CopyPath | CopyPathUnquoted
            | CopyFiles | NewCollection | RemoveFromCollection | MoveEarlier | MoveLater
            | MoveFirst | MoveLast | Print => Target::Selection,
            #[cfg(feature = "marks")]
            Mark1 | Mark2 | Mark3 | Mark4 | Mark5 | Mark6 | Rate0 | Rate1 | Rate2 | Rate3
            | Rate4 | Rate5 => Target::Selection,
            OpenWith | SetWallpaper | PlayVideo | CopyImage | RotateCw | RotateCcw | Rotate180
            | Mirror | Flip | SaveOrientation | Crop | SaveAs | SaveFrame | ExportFrames => {
                Target::Shown
            }
            OpenFile | OpenFolder | OpenFolders | OpenRecursive | NewFolder | Undo
            | CloseWindow | Quit | First | Previous | Next | Last | Back | Forward | Home | Up
            | SelectAll | SelectNone | InvertSelection | Preferences | ExifRotate | NextFilter
            | ZoomIn | ZoomOut | Zoom1To1 | ZoomFit | FitWidth | FitHeight | FitFill
            | FullScreen | HideFileList | FullScreenInfo | Histogram | HistogramLog | Compare
            | OneImage | AboveEachOther | NextLayout | PaneLeft | PaneRight | PaneUp | PaneDown
            | Animate | Subfolders | HiddenFiles | OpenCollection | ImportLightroom
            | RenameCollection | TrashCollection | FindDuplicates | CheckImages | BackUp
            | FocusSearch | NextPage | PrevPage | Icons | Toolbar | PathBar | Thumbnails
            | ThumbnailGrid | GridNames | InfoPanel | ExifWindow | FocusKeywords | SortManager
            | PanView | FolderTree | Refresh | Slideshow | Pause | RandomOrder
            | SlideshowFaster | SlideshowSlower | Escape | NewWindow | WindowTitle | Projector
            | ProjectorPointer | Help | Shortcuts | KeyboardMap | About => Target::NoFile,
            #[cfg(feature = "marks")]
            SelectMark1 | SelectMark2 | SelectMark3 | SelectMark4 | SelectMark5 | SelectMark6 => {
                Target::NoFile
            }
        }
    }
}

/// What an action acts on. It decides which actions a menu about the selection, such as the
/// file list's right-click menu, may offer, and which actions do nothing in an empty list.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Target {
    /// The selected files, or the shown image when none is selected (`App::targets`).
    Selection,
    /// The image shown, even while other files are selected.
    Shown,
    /// No file: the view, the window, the settings, the selection itself, or something to open.
    NoFile,
}

/// A shortcut: the action, and the modifiers and key that run it.
pub type Shortcut = (Action, egui::Modifiers, egui::Key);

/// Own keys in the config: "Next=Ctrl+N;ZoomIn=Plus", by action and egui's key names. The
/// second field holds the items this build cannot use: those for actions it does not know, such
/// as the marks in a build without them or an action of a newer version, and those with a key
/// it cannot read. They are written back unchanged, so saving the keys here does not take them
/// from the build that knows them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Keys(pub Vec<(Action, egui::Modifiers, egui::Key)>, Vec<String>);

impl std::str::FromStr for Keys {
    type Err = ();
    fn from_str(text: &str) -> Result<Self, ()> {
        let combo_of = |combo: &str| {
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
            Some((mods, key))
        };
        let mut keys = Keys::default();
        for item in text.split(';') {
            let Some((name, combo)) = item.split_once('=') else {
                continue;
            };
            // An item this build cannot use, for an action it does not know or with a key it
            // cannot read, is kept as it is for the build that wrote it.
            let name = action_name_now(name);
            match (
                SHORTCUTS.iter().find(|s| format!("{:?}", s.0) == name),
                combo_of(combo),
            ) {
                (Some(&(action, ..)), Some((m, k))) => keys.0.push((action, m, k)),
                _ => keys.1.push(item.to_owned()),
            }
        }
        Ok(keys)
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
            .chain(self.1.iter().cloned())
            .collect();
        write!(f, "{}", items.join(";"))
    }
}

impl crate::App {
    // ---- Keys and actions ----

    /// Preferences or a modal dialog is open: keys go there.
    /// A question about the selection on screen (Delete, Rename): what it acts on must stay as
    /// it was when asked. Preferences and the like do not count: a script would wait on them.
    pub(crate) fn asking_about_selection(&self) -> bool {
        self.dialogs.asks_about_files()
    }

    /// The action for a shortcut pressed this frame, if any.
    /// `delete_key` false: the Delete key does nothing (Ctrl+D still deletes), as in Geeqie.
    pub(crate) fn shortcut(
        ctx: &egui::Context,
        delete_key: bool,
        collection: bool,
    ) -> Option<Action> {
        ctx.input_mut(|i| {
            // egui-winit turns plain Ctrl+C and Ctrl+X, and the keyboard's own Copy and Cut
            // keys, into Copy and Cut events instead of key presses. With Shift or Alt held
            // they come as ordinary keys.
            let all = shortcuts();
            if let Some(key) = i.events.iter().find_map(clipboard_key) {
                i.events.retain(|e| clipboard_key(e).is_none());
                // The event means Ctrl+C or Ctrl+X even when no modifier is held, as it is for
                // the Copy key. It runs Copy, unless the user gave the key to another action.
                return (all.iter())
                    .find(|s| same_keys(s, &(s.0, CTRL, key)))
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
    pub(crate) fn run(&mut self, ctx: &egui::Context, action: Action) {
        // Without remote control (no $XDG_RUNTIME_DIR, the socket not made) --remote quit
        // cannot come to a kiosk, so Ctrl+Q is its way out then, rather than none.
        if self.window.is_kiosk() && action == Action::Quit && !remote::listening() {
            self.window.let_kiosk_quit();
        }
        // A kiosk shows and browses; it does not change files, settings or the window.
        if !self.window.allows(action) {
            return;
        }
        // An action on a file does nothing in an empty list. It is refused here, once, rather than
        // by a guard in each arm, which a new action could leave out and then panic on
        // `self.path(self.list.cur())` with no file.
        if self.list.files().is_empty() && action.target() != Target::NoFile {
            if action == Action::NewCollection {
                self.notice = Some(tr("No images for a collection here").into());
            }
            return;
        }
        let fullscreen = ctx.input(|i| i.viewport().fullscreen).unwrap_or(false);
        match action {
            Action::OpenFile => self.pick(ctx, false, Pick::Open),
            Action::OpenFolder => self.pick(ctx, true, Pick::Open),
            Action::OpenFolders => self.pick(ctx, true, Pick::Folders),
            Action::OpenRecursive => self.pick(ctx, true, Pick::Recursive),
            Action::NewCollection => {
                if let Some(asked) = self.ask_about_targets() {
                    self.dialogs
                        .ask_about(asked, Prompt::NewCollection, String::new());
                }
            }
            Action::OpenCollection => self.pick(ctx, false, Pick::Collection),
            Action::ImportLightroom => self.pick(ctx, false, Pick::Lightroom),
            Action::RemoveFromCollection if self.folder.collection().is_some() => {
                self.remove_from_collection(ctx)
            }
            Action::RemoveFromCollection => {}
            Action::RenameCollection | Action::TrashCollection if self.collection_is_list() => {
                self.notice = Some(tr("A plain list is not renamed or trashed by Glimt").into());
            }
            Action::RenameCollection => {
                if let Some(file) = self.folder.collection().map(|f| f.to_path_buf()) {
                    self.dialogs.ask_rename_collection(&file);
                }
            }
            Action::TrashCollection => {
                if let Some(file) = self.folder.collection().map(|f| f.to_path_buf()) {
                    self.dialogs.ask_trash_collection(&file);
                }
            }
            Action::MoveEarlier | Action::MoveLater | Action::MoveFirst | Action::MoveLast => {
                if let Some(picked) = self.targets() {
                    let picked: HashSet<usize> = picked.into_iter().collect();
                    let order = collections::moved(self.list.files().len(), &picked, action);
                    self.move_in_collection(ctx, picked, order)
                }
            }
            Action::FindDuplicates => drop(self.dups.get_or_insert_with(Dups::new)),
            Action::CheckImages => drop(self.check.get_or_insert(None)),
            Action::BackUp => self.pick(ctx, true, Pick::BackUp),
            // A kiosk shows only the image.
            Action::FocusSearch if self.window.is_kiosk() => {}
            Action::FocusSearch => self.focus_search_field(ctx, fullscreen),
            Action::CopyTo | Action::MoveTo => self.pick_copy_or_move(ctx, action),
            // The default application; the Open With submenu lists the others.
            Action::OpenWith => self.launch(""),
            Action::SetWallpaper => self.set_wallpaper(),
            // Enter: videos play in a player (images have nothing to play).
            Action::PlayVideo => self.play_video(ctx),
            Action::Delete => self.request_delete(ctx, self.config.delete_to_trash),
            Action::DeletePermanently => self.request_delete(ctx, false),
            Action::Undo => self.undo(ctx),
            Action::CloseWindow | Action::Quit => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close)
            }
            Action::First | Action::Previous | Action::Next | Action::Last => {
                self.move_along(ctx, action)
            }
            Action::Back | Action::Forward => self.back_or_forward(ctx, action),
            Action::Home => {
                if let Some(home) = std::env::var_os("HOME") {
                    self.visit(ctx, home.into());
                }
            }
            Action::Up => {
                if let Some(parent) = self.folder.dir().parent() {
                    self.visit(ctx, parent.to_path_buf());
                }
            }
            // Select All and Invert act on the files the list shows: the matches while searching.
            Action::SelectAll => {
                self.set_selection(ctx, self.shown_files().into_iter().collect());
            }
            Action::SelectNone => {
                self.set_selection(ctx, Default::default());
            }
            Action::InvertSelection => {
                let shown = self.shown_files().into_iter();
                let inverted = shown
                    .filter(|i| !self.list.selected().contains(i))
                    .collect();
                self.set_selection(ctx, inverted);
            }
            Action::CopyPath | Action::CopyPathUnquoted => self.copy_paths(ctx, action),
            // Decoded at full size in the background; egui hands it to the clipboard.
            Action::CopyImage => self.copy_image(ctx),
            // As a list of files, which a file manager pastes as a copy.
            Action::CopyFiles => self.copy_files(),
            Action::Preferences if config::locked() => {
                self.notice = Some(tr("The settings are locked (--lock-settings)").into());
            }
            Action::Preferences => self.prefs.show(),
            Action::Help => self.help = Some(help::Page::Manual),
            Action::Shortcuts => self.help = Some(help::Page::Shortcuts),
            Action::KeyboardMap => self.help = Some(help::Page::Keymap),
            Action::About => self.help = Some(help::Page::About),
            Action::Thumbnails => self.set_thumbnails(!self.config.thumbnails),
            Action::GridNames => self.set_grid_names(!self.config.grid_names),
            Action::ThumbnailGrid => {
                // A part chosen for Crop is of the image, and Enter in the grid opens an image.
                self.viewing.stop_crop();
                self.panel.toggle_area_grid();
            }
            Action::SaveOrientation => self.save_orientation(ctx),
            Action::Crop => self.start_crop(),
            Action::SaveAs => self.start_save_as(),
            Action::Subfolders => self.set_subfolders(ctx, !self.config.show_subfolders),
            Action::HiddenFiles => self.toggle_hidden_files(ctx),
            // An animation: stopped at the frame before or after the one shown, which A plays on.
            Action::NextPage | Action::PrevPage if self.animated() => self.step_frame(action),
            Action::NextPage | Action::PrevPage => self.step_page(action),
            Action::ExifWindow => self.info_panel.toggle_exif_window(),
            Action::FocusKeywords => {
                if !self.config.info_panel {
                    self.toggle_setting(Action::InfoPanel);
                }
                self.info_panel.focus_keywords();
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
                self.go_to_pane(ctx, action)
            }
            Action::NextLayout => {
                let at = LAYOUTS.iter().position(|l| l.0 == self.config.layout);
                let next = LAYOUTS[at.map_or(0, |k| (k + 1) % LAYOUTS.len())].0;
                self.set_layout(next);
            }
            Action::Compare => self.toggle_compare(),
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
            | Action::InfoPanel
            | Action::SortManager
            | Action::FullScreenInfo
            | Action::Icons
            | Action::FolderTree
            | Action::HistogramLog
            | Action::Animate => self.toggle_setting(action),
            // A collection, or several folders shown together, is no folder to make one in: it
            // would be made in /.
            Action::NewFolder
                if self.folder.collection().is_some() || !self.folder.folders().is_empty() =>
            {
                self.notice =
                    Some(tr("New Folder makes a folder in the folder shown: open one").into());
            }
            Action::NewFolder => {
                let dir = self.folder.dir().to_path_buf();
                self.dialogs.ask_new_folder(&dir);
            }
            Action::Print => self.open_print(ctx),
            Action::SaveFrame if self.frame_count() > 1 => {
                let k = self.shown_frame();
                let (path, how) = (self.path(self.list.cur()), self.shown_copy());
                self.pick(ctx, false, Pick::SaveFrame(path, k, how));
            }
            Action::ExportFrames if self.frame_count() > 1 => {
                let (path, how) = (self.path(self.list.cur()), self.shown_copy());
                self.pick(ctx, true, Pick::ExportFrames(path, how));
            }
            Action::SaveFrame | Action::ExportFrames => {}
            Action::Rename if self.list.selected().len() > 1 => {
                if let Some(asked) = self.ask_about_targets() {
                    self.ask_rename_many(asked);
                }
            }
            Action::Rename => self.start_rename(ctx),
            // A separate process: windows share nothing but the config file.
            Action::NewWindow => self.new_window(),
            Action::WindowTitle => {
                self.dialogs
                    .ask_window_title(self.window.title().to_owned());
            }
            Action::Projector => {
                self.window.toggle_projector();
            }
            Action::ProjectorPointer => {
                self.config.projector_pointer = !self.config.projector_pointer;
                self.config.save();
            }
            Action::ExifRotate => self.set_exif_rotate(!self.config.exif_rotate),
            Action::NextFilter => {
                self.set_view_filter((self.config.view_filter + 1) % FILTERS.len())
            }
            // Mirroring or flipping what is on screen after a turn: mirror the image first and
            // turn the other way (a flip is a mirror and half a turn).
            Action::RotateCw
            | Action::RotateCcw
            | Action::Rotate180
            | Action::Mirror
            | Action::Flip => self.turn(action),
            Action::ZoomIn => self.zoom_by(1.25),
            Action::ZoomOut => self.zoom_by(0.8),
            Action::Zoom1To1 => self.zoom.one_to_one(),
            // Modes that stay for the next images, as ImageGlass's; X goes back to the whole.
            Action::ZoomFit | Action::FitWidth | Action::FitHeight | Action::FitFill => {
                self.set_fit_mode(action)
            }
            Action::FullScreen => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fullscreen))
            }
            Action::HideFileList => self.panel.toggle_hidden(),
            Action::Refresh => self.refresh(ctx),
            Action::Slideshow if self.slideshow.is_running() => self.stop_slideshow(ctx),
            Action::Slideshow => self.start_slideshow(ctx),
            // Resuming gives the shown image the full delay again.
            Action::Pause if self.slideshow.is_running() => {
                self.slideshow.toggle_pause();
                self.title(ctx);
            }
            Action::Pause => {}
            Action::RandomOrder => self.set_random_order(ctx, !self.config.slideshow_random),
            Action::SlideshowFaster | Action::SlideshowSlower => self.change_delay(ctx, action),
            // As in Geeqie, Escape never quits; it stops the slideshow and leaves full screen.
            Action::Escape => self.escape(ctx, fullscreen),
        }
    }

    /// A mark, a star rating or selecting by mark, from Edit → Marks and Stars or its keys.
    #[cfg(feature = "marks")]
    pub(crate) fn apply_mark(&mut self, ctx: &egui::Context, action: Action) {
        let Some((kind, n)) = action.mark() else {
            return;
        };
        // What another window marked meanwhile counts too.
        self.marks.refresh();
        if kind == 's' {
            let bit = 1 << (n - 1);
            let names = self.marked_names(|(bits, _)| bits & bit != 0);
            let picked = (self.shown_files().into_iter())
                .filter(|&i| names.contains(self.list.files()[i].as_os_str()))
                .collect();
            self.list.set_anchor(None);
            self.set_selection(ctx, picked);
            return;
        }
        let Some(targets) = self.targets() else {
            return;
        };
        let paths: Vec<PathBuf> = targets.iter().map(|&i| self.path(i)).collect();
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
        if self.search.is_about_marks() {
            self.update_search(ctx);
        }
    }

    /// File → New Window (Ctrl+N): another Glimt at this place.
    pub(crate) fn new_window(&mut self) {
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
    pub(crate) fn copy_image(&mut self, ctx: &egui::Context) {
        let (path, how) = (self.path(self.list.cur()), self.shown_copy());
        let (tx, ctx) = (self.notices_tx.clone(), ctx.clone());
        let (clip_tx, clip_rx) = mpsc::channel();
        self.work.copy_image_from(clip_rx);
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
    pub(crate) fn copy_files(&mut self) {
        let Some(targets) = self.targets() else {
            return;
        };
        let paths: Vec<PathBuf> = targets.iter().map(|&i| self.path(i)).collect();
        let result = (self.work.clipboard()).and_then(|c| c.set().file_list(&paths));
        if let Err(e) = result {
            self.notice = Some(tf!("Could not copy to the clipboard: {e}", e));
        }
    }

    /// The action of the key pressed this frame, if any; the arrow keys move a zoomed image first.
    pub(crate) fn read_keys(&mut self, ctx: &egui::Context) -> Option<Action> {
        // Shortcuts go to the dialogs while one is open, and to the widget with the focus (the
        // toolbar's folder path), also in the frame Esc takes the focus from it.
        let typing = self.focus_held(ctx);
        // Choosing a part to crop: Escape stops and Enter saves it. Both are read before the
        // shortcuts could take them, and neither while a widget has the focus, because the key
        // belongs to the widget then.
        if self.viewing.is_cropping() && !self.dialog_open() {
            if self.answers(ctx, egui::Key::Escape) {
                self.viewing.stop_crop();
            } else if self.answers(ctx, egui::Key::Enter) {
                self.save_crop(ctx);
            }
        }
        // Escape closes the pan view. It is read before the shortcuts, and nothing else is done
        // with it then.
        if self.pan_window.is_some() && !self.dialog_open() && self.answers(ctx, egui::Key::Escape)
        {
            self.pan_window = None;
        }
        // And the Check Images window: a check still running is stopped with it.
        if self.check.is_some()
            && !self.dialog_open()
            && self.answers(ctx, egui::Key::Escape)
            && let Some(Some(check)) = self.check.take()
        {
            check.stop();
        }
        // A zoomed image larger than the window: the arrow keys move it, as in Geeqie; along a
        // side where it fits they still browse.
        if !self.dialog_open() && !typing && !self.zoom.is_fitted() && !self.panel.area_grid_shown()
        {
            let step = 100.0;
            let max = self.zoom.pan_room();
            ctx.input_mut(|i| {
                for (key, d) in [
                    (egui::Key::ArrowLeft, egui::vec2(step, 0.0)),
                    (egui::Key::ArrowRight, egui::vec2(-step, 0.0)),
                    (egui::Key::ArrowUp, egui::vec2(0.0, step)),
                    (egui::Key::ArrowDown, egui::vec2(0.0, -step)),
                ] {
                    let along = if d.x != 0.0 { max.x } else { max.y };
                    if along > 0.0 && i.consume_key(egui::Modifiers::NONE, key) {
                        self.zoom.pan_by(d);
                    }
                }
            });
        }
        // Enter in the grid in the image area shows the image there. It is read before PlayVideo
        // takes the key.
        if self.panel.area_grid_shown()
            && !self.dialog_open()
            && self.answers(ctx, egui::Key::Enter)
        {
            self.panel.open_from_grid();
        }
        // In the icon grid ↑ and ↓ move a row; read before the shortcut takes the key.
        let vertical =
            ctx.input(|i| i.key_pressed(egui::Key::ArrowUp) || i.key_pressed(egui::Key::ArrowDown));
        self.panel.set_row_step(vertical);
        (!self.dialog_open() && !typing)
            .then(|| {
                Self::shortcut(
                    ctx,
                    self.config.delete_key,
                    self.folder.collection().is_some(),
                )
            })
            .flatten()
    }

    /// A ring in the accent colour around the widget with the keyboard focus, over everything,
    /// once Tab has moved it there; a click makes it go. egui draws none in menus and on flat
    /// buttons, so without it Tab moved an invisible focus. Text fields show their own.
    pub(crate) fn focus_ring(&mut self, ctx: &egui::Context) {
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
}

/// Makes the user's own keys in `keys` the ones in effect, at start and after each change in
/// Preferences. The settings hold them, and `OWN_KEYS` holds the copy the key handling reads.
/// Every change goes through this function, so the copy cannot fall behind the settings.
pub(crate) fn use_own_keys(keys: &Keys) {
    if let Ok(mut own) = OWN_KEYS.write() {
        own.clone_from(&keys.0);
    }
}

/// Whether two shortcuts are the same keys. Not by `ctrl`: the defaults say Ctrl as `command`
/// alone, where a key pressed (and an own key read back) has both.
pub fn same_keys(a: &Shortcut, b: &Shortcut) -> bool {
    let (x, y) = (a.1, b.1);
    (x.command, x.shift, x.alt, a.2) == (y.command, y.shift, y.alt, b.2)
}

/// The shortcuts in use: the user's own, and the defaults of the other actions. Those with
/// more modifiers come first, as egui ignores an extra Shift or Alt when matching. Made again
/// only when the own keys change: it is asked for many times in every frame.
pub fn shortcuts() -> std::sync::Arc<[Shortcut]> {
    type Made = Option<(Vec<Shortcut>, std::sync::Arc<[Shortcut]>)>;
    static MADE: std::sync::Mutex<Made> = std::sync::Mutex::new(None);
    let own = OWN_KEYS.read().map(|o| o.clone()).unwrap_or_default();
    let mut made = MADE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((from, all)) = made.as_ref()
        && *from == own
    {
        return all.clone();
    }
    // Defaults of actions with own keys go, and so does a default whose key an own key took.
    let mut all = own.clone();
    all.extend((SHORTCUTS.iter()).filter(|s| !own.iter().any(|o| o.0 == s.0 || same_keys(o, s))));
    all.retain(|s| s.0.in_build());
    let weight = |m: &egui::Modifiers| m.shift as u8 + m.alt as u8 + m.command as u8;
    all.sort_by_key(|s| std::cmp::Reverse(weight(&s.1)));
    let all: std::sync::Arc<[Shortcut]> = all.into();
    *made = Some((own, all.clone()));
    all
}

/// The first shortcut of an action, as shown in menus ("Ctrl+K").
pub fn shortcut_text(ctx: &egui::Context, action: Action) -> Option<String> {
    let (_, mods, key) = shortcuts().iter().copied().find(|s| s.0 == action)?;
    Some(ctx.format_shortcut(&egui::KeyboardShortcut::new(mods, key)))
}

/// Returns the name an action has now: the new name of one renamed in `RENAMED_ACTIONS`, and
/// `name` itself for every other.
fn action_name_now(name: &str) -> &str {
    (RENAMED_ACTIONS.iter())
        .find(|r| r.0 == name)
        .map_or(name, |r| r.1)
}

/// Returns the key of a Copy or Cut event (C or X), which egui-winit sends for Ctrl+C and
/// Ctrl+X instead of a key press, or `None` for any other event.
pub(crate) fn clipboard_key(e: &egui::Event) -> Option<egui::Key> {
    match e {
        egui::Event::Copy => Some(egui::Key::C),
        egui::Event::Cut => Some(egui::Key::X),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{Action, Target};

    /// Every action, in name order, for the tests that run each one. Kept whole by
    /// `all_lists_every_action`.
    const ALL: &[Action] = &[
        Action::About,
        Action::AboveEachOther,
        Action::Animate,
        Action::Back,
        Action::BackUp,
        Action::CheckImages,
        Action::CloseWindow,
        Action::Compare,
        Action::CopyFiles,
        Action::CopyImage,
        Action::CopyPath,
        Action::CopyPathUnquoted,
        Action::CopyTo,
        Action::Crop,
        Action::Delete,
        Action::DeletePermanently,
        Action::Escape,
        Action::ExifRotate,
        Action::ExifWindow,
        Action::ExportFrames,
        Action::FindDuplicates,
        Action::First,
        Action::FitFill,
        Action::FitHeight,
        Action::FitWidth,
        Action::Flip,
        Action::FocusKeywords,
        Action::FocusSearch,
        Action::FolderTree,
        Action::Forward,
        Action::FullScreen,
        Action::FullScreenInfo,
        Action::GridNames,
        Action::Help,
        Action::HiddenFiles,
        Action::HideFileList,
        Action::Histogram,
        Action::HistogramLog,
        Action::Home,
        Action::Icons,
        Action::ImportLightroom,
        Action::InfoPanel,
        Action::InvertSelection,
        Action::KeyboardMap,
        Action::Last,
        #[cfg(feature = "marks")]
        Action::Mark1,
        #[cfg(feature = "marks")]
        Action::Mark2,
        #[cfg(feature = "marks")]
        Action::Mark3,
        #[cfg(feature = "marks")]
        Action::Mark4,
        #[cfg(feature = "marks")]
        Action::Mark5,
        #[cfg(feature = "marks")]
        Action::Mark6,
        Action::Mirror,
        Action::MoveEarlier,
        Action::MoveFirst,
        Action::MoveLast,
        Action::MoveLater,
        Action::MoveTo,
        Action::NewCollection,
        Action::NewFolder,
        Action::NewWindow,
        Action::Next,
        Action::NextFilter,
        Action::NextLayout,
        Action::NextPage,
        Action::OneImage,
        Action::OpenCollection,
        Action::OpenFile,
        Action::OpenFolder,
        Action::OpenFolders,
        Action::OpenRecursive,
        Action::OpenWith,
        Action::PanView,
        Action::PaneDown,
        Action::PaneLeft,
        Action::PaneRight,
        Action::PaneUp,
        Action::PathBar,
        Action::Pause,
        Action::PlayVideo,
        Action::Preferences,
        Action::PrevPage,
        Action::Previous,
        Action::Print,
        Action::Projector,
        Action::ProjectorPointer,
        Action::Quit,
        Action::RandomOrder,
        #[cfg(feature = "marks")]
        Action::Rate0,
        #[cfg(feature = "marks")]
        Action::Rate1,
        #[cfg(feature = "marks")]
        Action::Rate2,
        #[cfg(feature = "marks")]
        Action::Rate3,
        #[cfg(feature = "marks")]
        Action::Rate4,
        #[cfg(feature = "marks")]
        Action::Rate5,
        Action::Refresh,
        Action::RemoveFromCollection,
        Action::Rename,
        Action::RenameCollection,
        Action::Rotate180,
        Action::RotateCcw,
        Action::RotateCw,
        Action::SaveAs,
        Action::SaveFrame,
        Action::SaveOrientation,
        Action::SelectAll,
        #[cfg(feature = "marks")]
        Action::SelectMark1,
        #[cfg(feature = "marks")]
        Action::SelectMark2,
        #[cfg(feature = "marks")]
        Action::SelectMark3,
        #[cfg(feature = "marks")]
        Action::SelectMark4,
        #[cfg(feature = "marks")]
        Action::SelectMark5,
        #[cfg(feature = "marks")]
        Action::SelectMark6,
        Action::SelectNone,
        Action::SetWallpaper,
        Action::Shortcuts,
        Action::Slideshow,
        Action::SlideshowFaster,
        Action::SlideshowSlower,
        Action::SortManager,
        Action::Subfolders,
        Action::ThumbnailGrid,
        Action::Thumbnails,
        Action::Toolbar,
        Action::TrashCollection,
        Action::Undo,
        Action::Up,
        Action::WindowTitle,
        Action::Zoom1To1,
        Action::ZoomFit,
        Action::ZoomIn,
        Action::ZoomOut,
    ];

    /// Each action is either one a script may run or one it may not, chosen on purpose: a new
    /// action fails here until it is in `SCRIPT_ACTIONS` or in the list below, which names
    /// those that change a file, a sidecar, a mark or star, or a collection, also through a
    /// dialog. Every name a script may run is found by its frozen name.
    #[test]
    fn every_action_is_decided_for_scripts() {
        use Action::*;
        let changes_files: &[Action] = &[
            // These change files only through a window or panel that the user then clicks or
            // types in, or they start another program. A script may not open the way to that
            // either. InfoPanel shows the keyword tree and the marks, which one click changes.
            InfoPanel,
            FindDuplicates,
            FocusKeywords,
            OpenWith,
            PlayVideo,
            SortManager,
            CopyTo,
            Crop,
            Delete,
            DeletePermanently,
            ExportFrames,
            Flip,
            ImportLightroom,
            Mirror,
            MoveTo,
            MoveEarlier,
            MoveFirst,
            MoveLast,
            MoveLater,
            NewCollection,
            NewFolder,
            Print,
            RemoveFromCollection,
            Rename,
            RenameCollection,
            Rotate180,
            RotateCcw,
            RotateCw,
            SaveAs,
            SaveFrame,
            SaveOrientation,
            SetWallpaper,
            TrashCollection,
            BackUp,
            Undo,
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
        ];
        for action in ALL {
            let script = super::SCRIPT_ACTIONS.contains(action);
            assert!(
                script != changes_files.contains(action),
                "{action:?}: put it in SCRIPT_ACTIONS or in changes_files, in one only"
            );
        }
        for action in super::SCRIPT_ACTIONS {
            assert_eq!(Action::for_script(&format!("{action:?}")), Some(*action));
        }
        assert_eq!(Action::for_script("Delete"), None);
        assert_eq!(Action::for_script("Nothing"), None);
    }

    /// `ALL` names every variant of `Action`, in name order, so a new action is run by the test
    /// below too. A variant behind the marks feature counts only in a build with it.
    #[test]
    fn all_lists_every_action() {
        let variants = variants(cfg!(feature = "marks"));
        let listed: Vec<String> = ALL.iter().map(|a| format!("{a:?}")).collect();
        let missing: Vec<&String> = variants.iter().filter(|v| !listed.contains(v)).collect();
        assert!(missing.is_empty(), "ALL lacks {missing:?}");
        assert_eq!(listed.len(), variants.len(), "ALL names an action twice");
        assert!(listed.is_sorted(), "ALL is not in name order");
    }

    /// Returns the names of `Action`'s variants as its source writes them, those behind the
    /// marks feature only when `marks`.
    fn variants(marks: bool) -> Vec<String> {
        let src = include_str!("actions.rs");
        let start = src.find("pub enum Action {").unwrap();
        let body = &src[start..start + src[start..].find("\n}\n").unwrap()];
        let (mut variants, mut marks_only) = (Vec::new(), false);
        for line in body.lines().skip(1).map(str::trim) {
            if line == "#[cfg(feature = \"marks\")]" {
                marks_only = true;
            } else if line.starts_with(|c: char| c.is_ascii_uppercase()) {
                let name = line.split([',', ' ', '(', '{']).next().unwrap();
                if !marks_only || marks {
                    variants.push(name.to_owned());
                }
                marks_only = false;
            }
        }
        variants
    }

    /// Every action run in an empty folder does nothing harmful, and nor does the frame drawn
    /// after it: none panics on a list with no file. A new action, view or window that reads
    /// the shown file without a guard fails here.
    #[test]
    fn every_action_is_safe_in_an_empty_folder() {
        // Those that open a file dialog or start a program of the desktop are left out, because
        // a test run on a desktop would show the dialog or start the program.
        const OPENS_SOMETHING: &[Action] = &[
            Action::OpenFile,
            Action::OpenFolder,
            Action::OpenFolders,
            Action::OpenRecursive,
            Action::OpenCollection,
            Action::ImportLightroom,
            Action::BackUp,
            Action::NewWindow,
            Action::OpenWith,
        ];
        let dir = std::env::temp_dir().join(format!("glimt-test-empty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = crate::support::test_support::app_on(&dir);
        // View → Filter and colour management are the process's. They are held from the colour
        // tests while changed, and put back.
        let held = (crate::images::colour::COLOUR_TEST.lock()).unwrap_or_else(|e| e.into_inner());
        // A frame is drawn after each action, so every view and window it opens is drawn with
        // no file to show. The second round turns the toggles off again, so each view is also
        // drawn without the others.
        let frame =
            |app: &mut crate::App| drop(ctx.run_ui(Default::default(), |ui| app.frame_ui(ui)));
        for _ in 0..2 {
            for &action in ALL.iter().filter(|a| !OPENS_SOMETHING.contains(a)) {
                app.run(&ctx, action);
                frame(&mut app);
            }
        }
        crate::images::colour::VIEW_FILTER.store(0, std::sync::atomic::Ordering::Relaxed);
        crate::images::colour::COLOR_MANAGEMENT.store(true, std::sync::atomic::Ordering::Relaxed);
        drop(held);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Each view and window opened on the only file is drawn again after that file is gone,
    /// with the list then empty. The projector once read the shown image's name there and
    /// panicked on the next frame.
    #[test]
    fn every_view_draws_after_the_last_file_goes() {
        use crate::support::test_support::{app_on, settle};
        const OPENS_A_VIEW: &[Action] = &[
            Action::Projector,
            Action::ThumbnailGrid,
            Action::PanView,
            Action::ExifWindow,
            Action::FocusKeywords,
            Action::NextLayout,
            Action::FullScreenInfo,
            Action::Histogram,
            Action::Slideshow,
            Action::Crop,
            Action::SaveAs,
            Action::Print,
            Action::FindDuplicates,
            Action::CheckImages,
            Action::Preferences,
            Action::Help,
            Action::FullScreen,
            Action::HideFileList,
        ];
        let dir = std::env::temp_dir().join(format!("glimt-test-view-gone-{}", std::process::id()));
        for &action in OPENS_A_VIEW {
            std::fs::create_dir_all(&dir).unwrap();
            image::RgbImage::new(4, 4).save(dir.join("a.png")).unwrap();
            let (mut app, ctx) = app_on(&dir);
            let frame = |app: &mut crate::App| {
                drop(ctx.run_ui(Default::default(), |ui| app.frame_ui(ui)));
            };
            // Crop and Save As start from the image decoded for the view, which a frame asks
            // for.
            frame(&mut app);
            assert!(settle(&mut app, &ctx, |app| !app.list.slots().is_empty()));
            app.run(&ctx, action);
            frame(&mut app);
            std::fs::remove_dir_all(&dir).unwrap();
            std::fs::create_dir_all(&dir).unwrap();
            app.run(&ctx, Action::Refresh);
            assert!(settle(&mut app, &ctx, |app| app.list.files().is_empty()));
            frame(&mut app);
            frame(&mut app);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The file list's right-click menu acts on the row right-clicked, which it selects. Every
    /// action it offers acts on the selection, and its Open With submenu asks about no other
    /// file. Copy Image there once copied the shown image instead of the file right-clicked.
    #[test]
    fn the_file_menu_acts_on_the_selection() {
        let src = include_str!("menus.rs");
        let body = |name: &str| {
            let start = src.find(&format!("fn {name}(")).unwrap();
            &src[start..start + src[start..].find("\n    }\n").unwrap()]
        };
        let menu = body("file_menu");
        let named: Vec<&str> = (menu.split("Action::").skip(1))
            .map(|rest| {
                rest.split(|c: char| !c.is_ascii_alphanumeric())
                    .next()
                    .unwrap()
            })
            .collect();
        assert!(
            named.len() >= 8,
            "file_menu names its actions as Action::Name"
        );
        for name in named {
            let action = ALL.iter().find(|a| format!("{a:?}") == name).unwrap();
            assert_eq!(action.target(), Target::Selection, "{name} in file_menu");
        }
        assert!(menu.contains("self.open_with_menu(ui)"));
        let open_with = body("open_with_menu");
        assert!(
            open_with.contains("first_target()") && !open_with.contains("list.cur()"),
            "Open With asks about the first selected file (first_target), not the shown one"
        );
    }

    /// Every action on the selection reaches `targets`, `quiet_targets` or `ask_about_targets`
    /// from its arm in `App::run`, in the arm itself or in a function it calls. `Action::target`
    /// is only a label, which a menu about the selection trusts. An arm that took the shown
    /// image instead would act on another file than the ones the user picked.
    #[test]
    fn actions_on_the_selection_reach_their_targets() {
        use std::collections::HashMap;
        /// Returns whether `code`, or a function it calls on `self` up to `depth` calls down,
        /// asks for the selected files through one of the three.
        fn reaches(code: &str, depth: u8, bodies: &HashMap<String, String>) -> bool {
            let called = (code.match_indices("self.")).filter_map(|(at, s)| {
                let rest = &code[at + s.len()..];
                let name = rest
                    .split(|c: char| !c.is_alphanumeric() && c != '_')
                    .next()?;
                rest[name.len()..].starts_with('(').then_some(name)
            });
            let deeper = |n: &str| bodies.get(n).is_some_and(|b| reaches(b, depth - 1, bodies));
            code.contains("targets()") || depth > 0 && { called }.any(deeper)
        }
        // The code of every function outside the tests, by name. Functions of one name are put
        // together, which can only let an arm pass, never fail one that reaches its targets.
        let mut bodies: HashMap<String, String> = HashMap::new();
        let mut run = String::new();
        for (file, text) in crate::support::source_rules::rust_sources() {
            let lines = crate::support::source_rules::code_lines(&text);
            let end =
                (lines.iter().position(|l| l.trim_start() == "mod tests {")).unwrap_or(lines.len());
            for (i, line) in lines[..end].iter().enumerate() {
                let Some((before, after)) = line.split_once("fn ") else {
                    continue;
                };
                let name = after.split(['(', '<']).next().unwrap_or_default();
                if !(before.is_empty() || before.ends_with(' ')) || name.contains(' ') {
                    continue;
                }
                let indent = line.len() - line.trim_start().len();
                let closes = |l: &String| {
                    l.len() - l.trim_start().len() == indent && l.trim_start().starts_with('}')
                };
                if let Some(last) = (i + 1..end).find(|&j| closes(&lines[j])) {
                    let body = lines[i..=last].join("\n");
                    if file == "ui/actions.rs" && name == "run" {
                        run.clone_from(&body);
                    }
                    bodies.entry(name.to_owned()).or_default().push_str(&body);
                }
            }
        }
        // An arm that runs another action is checked as that action, not through `run` itself.
        bodies.remove("run");
        let (_, match_arms) = run.split_once("match action {\n").unwrap();
        // An arm starts at each line of the match's own indent that names an action.
        let mut arms: Vec<String> = Vec::new();
        for line in match_arms.lines() {
            let rest = line.strip_prefix("            ").unwrap_or("");
            if rest.starts_with("Action::") || rest.starts_with("#[") || arms.is_empty() {
                arms.push(String::new());
            }
            arms.last_mut().unwrap().push_str(&format!("{line}\n"));
        }
        let selection = ALL.iter().filter(|a| a.target() == Target::Selection);
        for name in selection.map(|a| format!("{a:?}")) {
            let names = |arm: &&String| {
                (arm.match_indices(&format!("Action::{name}"))).any(|(at, s)| {
                    let after = arm[at + s.len()..].chars().next();
                    !after.is_some_and(|c| c.is_alphanumeric() || c == '_')
                })
            };
            let mine: Vec<&String> = arms.iter().filter(names).collect();
            assert!(!mine.is_empty(), "{name} has no arm in App::run");
            for arm in mine {
                let (_, does) = arm.split_once("=>").unwrap();
                let nothing = does.trim().trim_end_matches(',') == "{}";
                assert!(
                    nothing || reaches(does, 3, &bodies),
                    "{name} acts on the selection (Action::target) but its arm in App::run \
                     reaches no targets(), quiet_targets() or ask_about_targets():\n{arm}"
                );
            }
        }
    }

    /// Every action's name is in the translations: it reaches tr through `english`, where the
    /// translation test, which looks for tr("…"), cannot see it. The stars are the same in
    /// every language.
    #[test]
    fn every_action_name_is_translated() {
        let src = include_str!("actions.rs");
        let start = src.find("pub fn english(").unwrap();
        let body = &src[start..start + src[start..].find("\n    }\n").unwrap()];
        let po = include_str!("../../po/no_nb.po");
        let lacking: Vec<&str> = (body.split("=> \"").skip(1))
            .map(|rest| &rest[..rest.find('"').unwrap()])
            .filter(|name| name.chars().any(char::is_alphabetic))
            .filter(|name| !po.contains(&format!("msgid \"{name}\"\n")))
            .collect();
        assert!(lacking.is_empty(), "not in no_nb.po: {lacking:?}");
    }

    /// The names in keys= of the settings file are frozen since 1.0: an action or a key name
    /// changed would take every user's own keys away. src/keys-1.0.txt holds the names of 1.0;
    /// new ones are added there, never one taken out or changed. Every action is in it, also
    /// one without a key of its own, because a user can give it one. Before, only the actions
    /// with a key were pinned, so one without a key could be renamed unseen.
    #[test]
    fn names_of_1_0_are_kept() {
        let actions = variants(true);
        let mut now: std::collections::HashSet<String> = (super::SHORTCUTS.iter())
            .map(|(_, _, k)| k.name().to_owned())
            .collect();
        now.extend(actions.iter().cloned());
        let frozen: Vec<&str> = (include_str!("../keys-1.0.txt").lines())
            .filter(|l| !l.starts_with('#') && !l.is_empty())
            .collect();
        let unpinned: Vec<&String> = (actions.iter())
            .filter(|a| !frozen.contains(&a.as_str()))
            .collect();
        assert!(
            unpinned.is_empty(),
            "add these at the end of src/keys-1.0.txt: {unpinned:?}"
        );
        let mut sorted: Vec<&String> = now.iter().collect();
        sorted.sort();
        assert!(
            !frozen.is_empty(),
            "the names now:\n{}",
            sorted
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        );
        // An old name passes when RENAMED_ACTIONS maps it to an action's name now.
        for name in frozen {
            assert!(
                now.contains(super::action_name_now(name)),
                "{name} is gone: it is in users' keys= since 1.0"
            );
        }
    }

    /// Each renamed action is read by its old name in keys= and by `--remote run`, and only its
    /// new name is written.
    #[test]
    fn old_action_names_are_read_and_only_new_ones_written() {
        use super::Keys;
        for (old, new) in super::RENAMED_ACTIONS {
            let keys: Keys = format!("{old}=Ctrl+F12").parse().unwrap();
            assert_eq!(keys.to_string(), format!("{new}=Ctrl+F12"), "{old}");
            assert_eq!(Action::for_script(old), Action::for_script(new), "{old}");
        }
        assert_eq!(Action::for_script("Prev"), Some(Action::Previous));
    }

    /// Own keys survive the config file, and replace the defaults of their action.
    #[test]
    fn own_keys() {
        use super::{Action, Keys};
        let keys: Keys = "Next=Ctrl+Shift+N;ZoomIn=Plus;Bogus=X;Prev=Hyper+P"
            .parse()
            .unwrap();
        let _held = super::KEYS_TEST.lock().unwrap_or_else(|e| e.into_inner());
        // An action this build does not know, and a key it cannot read, are kept as they were.
        assert_eq!(
            keys.to_string(),
            "Next=Ctrl+Shift+N;ZoomIn=Plus;Bogus=X;Prev=Hyper+P"
        );
        assert_eq!(keys.to_string().parse::<Keys>().unwrap(), keys);
        *super::OWN_KEYS.write().unwrap() = keys.0.clone();
        let all = super::shortcuts();
        // PlayVideo has no shortcut in a build without video, so Enter is free there.
        let play = all.iter().any(|s| s.0 == Action::PlayVideo);
        assert_eq!(play, cfg!(feature = "video"));
        let next: Vec<_> = all.iter().filter(|s| s.0 == Action::Next).collect();
        assert_eq!(next.len(), 1);
        // More modifiers first, so Shift+R (rotate) is matched before R.
        let pos = |a| all.iter().position(|s| s.0 == a).unwrap();
        assert!(pos(Action::Rotate180) < pos(Action::RandomOrder));
        // An own key that is another action's default takes it from that action.
        let r = "ZoomFit=R".parse::<Keys>().unwrap();
        *super::OWN_KEYS.write().unwrap() = r.0.clone();
        let all = super::shortcuts();
        let on_r: Vec<Action> = (all.iter())
            .filter(|s| s.2 == eframe::egui::Key::R && s.1 == eframe::egui::Modifiers::NONE)
            .map(|s| s.0)
            .collect();
        assert_eq!(on_r, [Action::ZoomFit]);
        // Also a key with Ctrl, which the defaults and own keys say each their way.
        let ctrl_n = "Next=Ctrl+N".parse::<Keys>().unwrap();
        *super::OWN_KEYS.write().unwrap() = ctrl_n.0.clone();
        let all = super::shortcuts();
        assert!(
            !all.iter()
                .any(|s| s.0 == Action::NewWindow && super::same_keys(s, &ctrl_n.0[0]))
        );
        super::OWN_KEYS.write().unwrap().clear();
    }

    /// Plain Ctrl+C comes from egui-winit as a Copy event, and so does the Copy key with no
    /// modifier held. Both copy to a folder. Ctrl+Shift+C comes as an ordinary key, and it
    /// copies the image to the clipboard.
    #[test]
    fn copy_keys_run_what_egui_sends_them_as() {
        use eframe::egui;
        let pressed = |modifiers: egui::Modifiers, event: egui::Event| {
            let ctx = egui::Context::default();
            ctx.begin_pass(egui::RawInput {
                events: vec![egui::Event::ModifiersChanged(modifiers), event],
                ..Default::default()
            });
            crate::App::shortcut(&ctx, true, false)
        };
        let key = |modifiers| egui::Event::Key {
            key: egui::Key::C,
            physical_key: Some(egui::Key::C),
            pressed: true,
            repeat: false,
            modifiers,
        };
        assert_eq!(
            pressed(super::CTRL, egui::Event::Copy),
            Some(super::Action::CopyTo)
        );
        assert_eq!(
            pressed(egui::Modifiers::NONE, egui::Event::Copy),
            Some(super::Action::CopyTo)
        );
        // The Cut key is Ctrl+X, which no action has by default, not X (Zoom to Fit).
        assert_eq!(pressed(egui::Modifiers::NONE, egui::Event::Cut), None);
        assert_eq!(
            pressed(super::CTRL_SHIFT, key(super::CTRL_SHIFT)),
            Some(super::Action::CopyImage)
        );
    }

    /// Esc in a text field only leaves the field, and runs no shortcut. egui takes the field's
    /// focus away at the start of the frame Esc is pressed in, so the shortcuts' Esc also left
    /// full screen, stopped the slideshow or went back to the grid. The next Esc runs it.
    #[test]
    fn esc_in_a_text_field_runs_no_shortcut() {
        use eframe::egui;
        let dir = std::env::temp_dir().join(format!("glimt-test-esc-keys-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = crate::support::test_support::app_on(&dir);
        let field = egui::Id::unique("a text field");
        let mut esc = |focus: bool| {
            let event = |pressed| egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers: Default::default(),
            };
            let input = egui::RawInput {
                events: vec![event(true), event(false)],
                ..Default::default()
            };
            let mut action = None;
            let _ = ctx.run_ui(input, |ui| {
                action = app.read_keys(ui.ctx());
                if focus {
                    ui.memory_mut(|m| m.request_focus(field));
                }
                app.note_focus(ui.ctx());
            });
            action
        };
        esc(true);
        assert_eq!(
            esc(false),
            None,
            "Esc that left a text field ran a shortcut"
        );
        assert_eq!(
            esc(false),
            Some(Action::Escape),
            "Esc with nothing focused did nothing"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
