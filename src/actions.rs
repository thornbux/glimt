//! Everything the user can do from a menu or a key: the Action list with menu labels, Geeqie's
//! default keys (SHORTCUTS), and the user's own keys from Preferences.

use eframe::egui;

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
