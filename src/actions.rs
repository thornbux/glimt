//! Everything the user can do from a menu or a key: the Action list with menu labels, Geeqie's
//! default keys (SHORTCUTS), and the user's own keys from Preferences.

use crate::colour::FILTERS;
use crate::config::MAX_DELAY;
use crate::decode::{color_image, survive};
use crate::dialogs::{Pick, Prompt};
use crate::duplicates::Dups;
#[cfg(feature = "video")]
use crate::external::is_video;
#[cfg(feature = "marks")]
use crate::format::count;
use crate::lang::tr;
#[cfg(feature = "marks")]
use crate::marks;
use crate::pan::Pan;
use crate::threads::{Receiving, Working};
use crate::view::layout_grid;
use crate::{
    LAYOUTS, POINTER_STILL, collections, config, external, help, own_program, remote, save, tf,
    window_shown,
};
use eframe::egui;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::Instant;

/// Everything the user can do from a menu or a shortcut. The variants' names are stored in the
/// settings for the user's own keys (`{a:?}`): renaming one loses those keys.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Action {
    OpenFile,
    OpenFolder,
    OpenFolders,
    OpenRecursive,
    NewFolder,
    Rename,
    Copy,
    Move,
    OpenWith,
    SetWallpaper,
    Play,
    Delete,
    DeletePermanently,
    Undo,
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
    ViewFilter,
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
    HideTools,
    Overlay,
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
    ExifPanel,
    ExifWindow,
    Keywords,
    SortManager,
    PanView,
    FolderTree,
    Refresh,
    Slideshow,
    Pause,
    RandomOrder,
    Faster,
    Slower,
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
        crate::lang::tr(self.english())
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
            Copy => "Copy…",
            Move => "Move…",
            OpenWith => "Open With Default App",
            SetWallpaper => "Set as Wallpaper",
            Play => "Play Video",
            Delete => "Delete…",
            DeletePermanently => "Delete Permanently…",
            Undo => "Undo",
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
            ViewFilter => "Next Filter",
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
            HideTools => "Hide File List",
            Overlay => "Show Image Info in Full Screen",
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
            ExifPanel => "Show EXIF Panel",
            SortManager => "Show Sort Manager",
            ExifWindow => "EXIF Window",
            Keywords => "Keywords and Comment",
            PanView => "Pan View",
            FolderTree => "Show Folder Tree",
            Refresh => "Refresh",
            Slideshow => "Start/Stop Slideshow",
            Pause => "Pause Slideshow",
            RandomOrder => "Random Order",
            Faster => "Faster",
            Slower => "Slower",
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
            Copy => "\u{e1ca}",                             // copy
            Move | Forward | PaneRight => "\u{e06c}",       // arrow-right
            OpenWith => "\u{e5de}",                         // arrow-square-out
            SetWallpaper => "\u{e32e}",                     // monitor
            Play => "\u{e792}",                             // film-strip
            Delete | TrashCollection => "\u{e4a6}",         // trash
            DeletePermanently => "\u{e4f8}",                // x-circle
            Undo => "\u{e08a}",                             // arrow-u-up-left
            CloseWindow => "\u{e4fa}",                      // x-square
            Quit => "\u{e3da}",                             // power
            First => "\u{e5a4}",                            // skip-back
            Prev => "\u{e138}",                             // caret-left
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
            ViewFilter | NextLayout => "\u{e83c}",          // swap
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
            HideTools => "\u{ec24}",                        // sidebar-simple
            Overlay => "\u{e1a8}",                          // subtitles
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
            ExifPanel | About => "\u{e2ce}",                // info
            ExifWindow | SaveFrame => "\u{e10e}",           // camera
            Keywords => "\u{e478}",                         // tag
            SortManager => "\u{e444}",                      // sort-ascending
            PanView => "\u{e7b4}",                          // calendar-dots
            Slideshow => "\u{e3d0}",                        // play
            Pause => "\u{e39e}",                            // pause
            RandomOrder => "\u{e422}",                      // shuffle
            Faster => "\u{e6a6}",                           // fast-forward
            Slower => "\u{e6a8}",                           // rewind
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
}

pub const NONE: egui::Modifiers = egui::Modifiers::NONE;

pub const SHIFT: egui::Modifiers = egui::Modifiers::SHIFT;

pub const CTRL: egui::Modifiers = egui::Modifiers::COMMAND;

pub const ALT: egui::Modifiers = egui::Modifiers::ALT;

pub const CTRL_SHIFT: egui::Modifiers = egui::Modifiers {
    shift: true,
    ..egui::Modifiers::COMMAND
};

/// Geeqie's default shortcuts, except R (random order; Geeqie: refresh) and F5 (refresh).
/// The first one for an action is shown in the menu.
/// egui ignores an extra Shift or Alt when matching, so those combinations come before the plain key.
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
        (ViewFilter, SHIFT, Key::G),
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
        (Copy, CTRL, Key::C),
        (Move, CTRL, Key::M),
        (CloseWindow, CTRL, Key::W),
        (Quit, CTRL, Key::Q),
        (First, NONE, Key::Home),
        // Before the plain arrows, which would also match with Alt.
        (PaneLeft, ALT, Key::ArrowLeft),
        (PaneRight, ALT, Key::ArrowRight),
        (PaneUp, ALT, Key::ArrowUp),
        (PaneDown, ALT, Key::ArrowDown),
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
        (FitWidth, NONE, Key::H),
        (FitHeight, NONE, Key::W),
        (FullScreen, NONE, Key::F),
        (FullScreen, NONE, Key::V),
        (FullScreen, NONE, Key::F11),
        (HideTools, CTRL, Key::H),
        (Thumbnails, NONE, Key::T),
        (Overlay, NONE, Key::I),
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
        (ExifPanel, CTRL, Key::K),
        (ExifWindow, CTRL, Key::E),
        (Keywords, ALT, Key::K),
        (PanView, CTRL, Key::J),
        (FolderTree, CTRL, Key::T),
        (Refresh, NONE, Key::F5),
        // Before S (slideshow), which would also match with Shift.
        (SortManager, SHIFT, Key::S),
        (Slideshow, NONE, Key::S),
        (Pause, NONE, Key::P),
        (RandomOrder, NONE, Key::R),
        (Faster, CTRL, Key::Equals),
        (Slower, CTRL, Key::Minus),
        (Play, NONE, Key::Enter),
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

/// A shortcut: the action, and the modifiers and key that run it.
pub type Shortcut = (Action, egui::Modifiers, egui::Key);

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
    let weight = |m: &egui::Modifiers| m.shift as u8 + m.alt as u8 + m.command as u8;
    all.sort_by_key(|s| std::cmp::Reverse(weight(&s.1)));
    let all: std::sync::Arc<[Shortcut]> = all.into();
    *made = Some((own, all.clone()));
    all
}

/// Own keys in the config: "Next=Ctrl+N;ZoomIn=Plus", by action and egui's key names.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Keys(pub Vec<(Action, egui::Modifiers, egui::Key)>);

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

/// The first shortcut of an action, as shown in menus ("Ctrl+K").
pub fn shortcut_text(ctx: &egui::Context, action: Action) -> Option<String> {
    let (_, mods, key) = shortcuts().iter().copied().find(|s| s.0 == action)?;
    Some(ctx.format_shortcut(&egui::KeyboardShortcut::new(mods, key)))
}

/// The actions a kiosk allows: showing and browsing, not changing files, settings or the
/// window.
pub(crate) const KIOSK_ACTIONS: &[Action] = &[
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

impl crate::App {
    // ---- Keys and actions ----

    /// Preferences or a modal dialog is open: keys go there.
    /// A question about the selection on screen (Delete, Rename): what it acts on must stay as
    /// it was when asked. Preferences and the like do not count: a script would wait on them.
    pub(crate) fn asking_about_selection(&self) -> bool {
        self.confirm_delete.is_some() || self.prompt.is_some()
    }

    /// Busy pointer while the folder is read (also sorted anew), a share mounted, work done in
    /// the background, or the shown image still decoded.
    pub(crate) fn working(&self) -> bool {
        let decoding = !self.list.files.is_empty()
            && !self.list.slots.contains_key(&self.list.cur)
            && !self.list.decode_errors.contains_key(&self.list.cur);
        decoding || self.working_besides_image()
    }

    /// `working` but for the shown image's decode, which a full-screen slideshow waits on at
    /// every image not preloaded.
    pub(crate) fn working_besides_image(&self) -> bool {
        self.listing_rx.is_some()
            || self.mounting_rx.is_some()
            || self.busy.load(Ordering::Relaxed) > 0
    }

    /// In full screen, with the setting on, the pointer hidden once the mouse has been still for
    /// `POINTER_STILL`; never while work goes on (the busy pointer shows it; the shown image
    /// decoded does not count, or it would flash at each image of a slideshow), nor while a window,
    /// menu or popup shows (Help, the EXIF window, pan view, a dialog): one reads it, and wants
    /// the pointer there at once.
    pub(crate) fn hide_still_pointer(&mut self, ctx: &egui::Context, fullscreen: bool) {
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
    pub(crate) fn pointer_hides(&self, fullscreen: bool) -> bool {
        fullscreen
            && self.config.hide_pointer
            && !self.working_besides_image()
            && !self.dialog_open()
    }

    pub(crate) fn dialog_open(&self) -> bool {
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
    pub(crate) fn shortcut(
        ctx: &egui::Context,
        delete_key: bool,
        collection: bool,
    ) -> Option<Action> {
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
                    .find(|s| same_keys(s, &(s.0, held, egui::Key::C)))
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
    pub(crate) fn copy_files(&mut self) {
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

    /// The action of the key pressed this frame, if any; the arrow keys move a zoomed image first.
    pub(crate) fn read_keys(&mut self, ctx: &egui::Context) -> Option<Action> {
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

#[cfg(test)]
mod tests {
    /// Every action's name is in the translations: it reaches tr through `english`, where the
    /// translation test, which looks for tr("…"), cannot see it. The stars are the same in
    /// every language.
    #[test]
    fn every_action_name_is_translated() {
        let src = include_str!("actions.rs");
        let start = src.find("pub fn english(").unwrap();
        let body = &src[start..start + src[start..].find("\n    }\n").unwrap()];
        let po = include_str!("no_nb.po");
        let lacking: Vec<&str> = (body.split("=> \"").skip(1))
            .map(|rest| &rest[..rest.find('"').unwrap()])
            .filter(|name| name.chars().any(char::is_alphabetic))
            .filter(|name| !po.contains(&format!("msgid \"{name}\"\n")))
            .collect();
        assert!(lacking.is_empty(), "not in no_nb.po: {lacking:?}");
    }

    /// The names in keys= of the settings file are frozen since 1.0: an action or a key name
    /// changed would take every user's own keys away. src/keys-1.0.txt holds the names of 1.0;
    /// new ones are added there, never one taken out or changed.
    #[test]
    fn names_of_1_0_are_kept() {
        let now: std::collections::HashSet<String> = (super::SHORTCUTS.iter())
            .flat_map(|(a, _, k)| [format!("{a:?}"), k.name().to_owned()])
            .collect();
        let frozen: Vec<&str> = (include_str!("keys-1.0.txt").lines())
            .filter(|l| !l.starts_with('#') && !l.is_empty())
            .collect();
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
        for name in frozen {
            assert!(
                now.contains(name),
                "{name} is gone: it is in users' keys= since 1.0"
            );
        }
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
}
