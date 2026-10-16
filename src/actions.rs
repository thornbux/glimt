//! Everything the user can do from a menu or a key: the Action list with menu labels, Geeqie's
//! default keys (SHORTCUTS), and the user's own keys from Preferences.

use eframe::egui;

/// Everything the user can do from a menu or a shortcut.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Action {
    OpenFile,
    OpenFolder,
    OpenFolders,
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
    ViewFilter,
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
        use Action::*;
        match self {
            OpenFile => "Open File…",
            OpenFolder => "Open Folder…",
            OpenFolders => "Open Several Folders…",
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
            ViewFilter => "Next Filter",
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

/// The shortcuts in use: the user's own, and the defaults of the other actions. Those with
/// more modifiers come first, as egui ignores an extra Shift or Alt when matching.
pub fn shortcuts() -> Vec<(Action, egui::Modifiers, egui::Key)> {
    let own = OWN_KEYS.read().map(|o| o.clone()).unwrap_or_default();
    let mut all = own.clone();
    all.extend(SHORTCUTS.iter().filter(|s| !own.iter().any(|o| o.0 == s.0)));
    let weight = |m: &egui::Modifiers| m.shift as u8 + m.alt as u8 + m.command as u8;
    all.sort_by_key(|s| std::cmp::Reverse(weight(&s.1)));
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
    let (_, mods, key) = shortcuts().into_iter().find(|s| s.0 == action)?;
    Some(ctx.format_shortcut(&egui::KeyboardShortcut::new(mods, key)))
}

#[cfg(test)]
mod tests {
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
}
