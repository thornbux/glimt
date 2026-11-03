//! The window around the image: the actions and their keys, the menus, the dialogs, the side panel
//! with the file list, the info panel, Preferences, the help pages, the window itself, the look,
//! the screen reader, and what each frame collects from the background.

#[cfg(feature = "accessibility")]
pub(crate) mod accessibility;
pub(crate) mod actions;
pub(crate) mod dialogs;
pub(crate) mod file_list;
pub(crate) mod help;
pub(crate) mod info_panel;
pub(crate) mod menus;
pub(crate) mod preferences;
pub(crate) mod theme;
pub(crate) mod window;
pub(crate) mod workers;
