//! Opening a folder, a collection or several folders, listing them, and following changes.

use crate::files::config::Config;
use crate::files::config::sort_folders_from;
use crate::files::config::sort_folders_path;
use crate::files::external::Mounted;
use crate::files::external::{Login, gio_mount};
use crate::files::file_io::{ListIsOld, forget_passing_identities, is_another_file};
use crate::model::list::{List, LoadedImage};
use crate::model::listing::{
    FolderReadWhole, FoldersRead, ListedName, NameHashes, ReadWhole, Rows, SortKey, Types, Unread,
    is_listed, list_folder, walk, walk_tree,
};
use crate::support::format::{NOT_COUNTED_ON_A_SHARE, escaped_name};
use crate::support::lang::tr;
use crate::support::threads::{Receiving, Took};
use crate::ui::actions::Action;
use crate::ui::menus::{scroll_menu, with_icon};
use crate::viewer::view::{Failure, Viewing};
use crate::{files::config, files::external, model::collections, model::listing, tf, ui::theme};
use eframe::egui;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

/// How many of a tree's subfolders the folder watcher looks at each second, besides the open
/// folders. A tree of 10,000 subfolders is looked through in ten seconds, at 1,000 stats a
/// second, instead of 10,000 stats every second.
const SUBFOLDERS_A_SECOND: usize = 1_000;

/// How long the folder watcher's looks at a tree's subfolders on a share may take in each
/// second, besides the open folders. A share such as gvfsd-smb serves its calls one at a time,
/// so the watcher then holds at most a tenth of the share's time, and the image the window
/// waits for has the rest. Before, it looked at up to 1,000 subfolders a second there, which
/// filled the share's queue, and a slideshow's next image stood behind them for many seconds.
const SHARE_LOOKS_A_SECOND: Duration = Duration::from_millis(100);

/// How many times as long as the last listing took the window rests from it before a change
/// that the watcher saw lists the folder again. A sync client that writes into a tree of
/// 500,000 files every second then has it read at most a fifth of the time, while a small
/// folder, listed in milliseconds, is read again at once.
const REST_AFTER_LISTING: u32 = 4;

/// How long a look-up of a place to open runs before a notice says that it is still opening.
const STILL_OPENING_AFTER: std::time::Duration = std::time::Duration::from_secs(3);

/// How many `Folder`s were made, which numbers each one. A window has one, and tests make many
/// in one process.
static FOLDERS_MADE: AtomicUsize = AtomicUsize::new(0);

/// What is open (a folder, a collection or several folders), its listing in the background, the way
/// back and forward, and the folders sorted into.
pub(crate) struct Folder {
    /// The number of this window's folder in the process, which its changes to a plain list
    /// are kept by (`collections::keep_window_list_of`).
    number: usize,
    dir: PathBuf,
    subdirs: Vec<PathBuf>,
    /// The folder path in the toolbar, as typed.
    path_text: String,
    /// The type of the file opened directly (not the folder), listed even when the Types menu
    /// leaves it out.
    opened_type: Types,
    /// The list holds the images of the subfolders too (slideshow with subfolders).
    recursive: bool,
    /// The open collection's file. The list is then its images, with `dir` "/" and the paths
    /// without their leading "/" as names, so `path()` works unchanged.
    collection: Option<PathBuf>,
    /// The collection's next listing is sorted (Sort menu), and saved in that order.
    sort_collection: bool,
    /// Several folders shown as one list (glimt DIR DIR …, File → Open Folders…). The names
    /// are relative to /, as in a collection, and the folders are read again on refresh.
    folders: Vec<PathBuf>,
    /// The folders to show together, chosen in a folder's menu. They are opened after the
    /// frame.
    together: Option<Vec<PathBuf>>,
    /// The paths asked to be opened, looked up in the background before anything changes:
    /// whether each is a folder, a file or a collection, and its real path. A share that stopped
    /// answering holds only that thread, not the window.
    looking_up: Receiving<(Opening, Found)>,
    /// When the look-up that runs started, and the path it looks up. It is forgotten once the
    /// notice that the place is still opening has been given (`tell_still_opening`).
    looking_up_since: Option<(Instant, PathBuf)>,
    /// The notice that the place is still opening, as given. It is taken away when the look-up
    /// answers or another one replaces it.
    still_opening_notice: Option<String>,
    /// The place that Remember Last Folder keeps while the one shown cannot be kept: the place
    /// remembered at the start, until the first look-up answers, and the folder or collection
    /// shown before several folders were shown together.
    place_before: Option<PathBuf>,
    /// The folder listing in the background, as (subfolders, images).
    listing_rx: Receiving<Listed>,
    /// The parts left out of a listing because they could not be read, which have been told
    /// since the place was opened or F5 read it again.
    told_left_out: HashSet<PathBuf>,
    /// The list's version when the listing that runs started. A collection's listing works
    /// from the list or file as they were then.
    listing_version: usize,
    /// The subfolders of a folder listed with its tree. They are sent at once, because the
    /// tree may take long.
    subdirs_rx: Receiving<Vec<PathBuf>>,
    /// The images of the tree found so far, as it is read, a folder of it at a time.
    part_rx: Receiving<TreePart>,
    /// How long the window took to take the last part of the tree, in microseconds. The
    /// listing thread waits for four times as long before it sends the next part.
    part_taken_in: Arc<AtomicU64>,
    /// The next listing is of a folder just opened or of its tree just asked for, which keeps
    /// no order of the list before it (`listing::kept_order`).
    lists_tree_in_parts: bool,
    /// The listing that runs is of a tree, which sends its images in parts as they are found,
    /// each of the images found since the part before that the list lacks. Every listing of a
    /// tree does (`source_rules::every_listing_of_a_tree_sends_parts`).
    listing_sends_parts: bool,
    /// What the list shown was read and sorted as (`ListedAs`).
    listed_as: Option<ListedAs>,
    /// The image the first part of a tree showed by itself. The whole list then goes to its
    /// first image instead.
    first_part_shown: Option<OsString>,
    /// Whether the list is a part of the tree being read, whose files' sizes and dates the
    /// next part, and the whole list after the parts, carry over by name.
    list_is_a_part: bool,
    /// Set to stop the listing of the tree before, when another one starts.
    listing_stop: Arc<AtomicBool>,
    /// Whether the folder changed while a listing was running. It is listed again when the
    /// listing is done.
    relist_when_done: bool,
    /// The watcher saw the folder change, and the folder is read again once the window has
    /// rested from the last listing (`relist_after_rest`).
    change_waits: bool,
    /// What the watcher saw change since the last listing began.
    changes_seen: ChangesSeen,
    /// The folder is to be read whole when it is read again, as something other than the
    /// watcher asked for it (`relist_later`).
    relist_whole: bool,
    /// The subfolder of the tree, relative to it, that the next listing reads again in place of
    /// the whole tree, or empty for the whole tree (`App::relist_changes`).
    next_listing_from: PathBuf,
    /// The subfolder of the tree that the listing that runs reads again, or empty when it reads
    /// the whole tree.
    listing_from: PathBuf,
    /// The folders that the watcher watches, as the last listings read them, so that a subfolder
    /// read again replaces only its own folders in it.
    watched: FoldersRead,
    /// Until when the window rests from the last listing before a change lists it again.
    rest_until: Instant,
    /// How many whole listings were taken since the start. Work that waits for the folder to be
    /// read again waits for this to grow.
    whole_listings: u64,
    /// The generation of the folder watcher, which each new watcher counts up. Opening another
    /// folder ends the old thread.
    watcher_generation: Arc<AtomicUsize>,
    /// The watcher saw these folders change.
    changed_tx: mpsc::Sender<Vec<PathBuf>>,
    changed_rx: mpsc::Receiver<Vec<PathBuf>>,
    /// File → Open Recent's folders and collections, those opened last, newest first.
    recent: Vec<PathBuf>,
    /// Go → Back / Forward's earlier locations. Each is the file shown, or the folder if it had
    /// no images.
    back: Vec<PathBuf>,
    forward: Vec<PathBuf>,
    /// A network location (smb:// …) being mounted. It answers with its local path, or with
    /// why it failed.
    mounting_rx: Receiving<(Mounted, Login)>,
    /// The login dialog for a network share that needs one.
    login: Option<Login>,
    /// The sort manager's folders, kept in `$XDG_CONFIG_HOME/glimt/sort-folders`.
    sort_folders: Vec<PathBuf>,
    /// The saved collections that the File menu lists.
    saved_collections: crate::model::collections::SavedCollections,
    /// The sort manager's folders as their file has them once this window's last change is
    /// written, with what other windows wrote there meanwhile.
    sort_folders_written: Receiving<Vec<PathBuf>>,
    /// Raised by a write that found another file at a name this window's list showed
    /// (`ListedFile::check`). Each file the window picks carries it.
    list_is_old: ListIsOld,
}

impl Folder {
    /// A `Folder` with nothing open yet, with the `recent` list and the Sort Manager's
    /// `sort_folders` as `main` read them (`read_recent`, `read_sort_folders`). `remembered` is
    /// the last folder kept from the window before, if any.
    pub(crate) fn new(
        remembered: Option<PathBuf>,
        recent: Vec<PathBuf>,
        sort_folders: Vec<PathBuf>,
    ) -> Self {
        let (changed_tx, changed_rx) = mpsc::channel();
        Folder {
            number: FOLDERS_MADE.fetch_add(1, Ordering::Relaxed),
            dir: PathBuf::new(),
            subdirs: Vec::new(),
            path_text: String::new(),
            opened_type: Types::NONE,
            recursive: false,
            collection: None,
            sort_collection: false,
            folders: Vec::new(),
            together: None,
            looking_up: Receiving::none(),
            looking_up_since: None,
            still_opening_notice: None,
            place_before: remembered,
            listing_rx: Receiving::none(),
            told_left_out: HashSet::new(),
            listing_version: 0,
            subdirs_rx: Receiving::none(),
            part_rx: Receiving::none(),
            part_taken_in: Arc::default(),
            lists_tree_in_parts: false,
            listing_sends_parts: false,
            listed_as: None,
            first_part_shown: None,
            list_is_a_part: false,
            listing_stop: Arc::default(),
            relist_when_done: false,
            change_waits: false,
            changes_seen: ChangesSeen::Nothing,
            relist_whole: false,
            next_listing_from: PathBuf::new(),
            listing_from: PathBuf::new(),
            watched: Vec::new(),
            rest_until: Instant::now(),
            whole_listings: 0,
            watcher_generation: Arc::new(AtomicUsize::new(0)),
            changed_tx,
            changed_rx,
            recent,
            back: Vec::new(),
            forward: Vec::new(),
            mounting_rx: Receiving::none(),
            login: None,
            sort_folders,
            saved_collections: Default::default(),
            sort_folders_written: Receiving::none(),
            list_is_old: ListIsOld::default(),
        }
    }

    /// The flag of this window's list, for the files it picks (`ListedFile::new`).
    pub(crate) fn list_is_old(&self) -> &ListIsOld {
        &self.list_is_old
    }

    /// Returns whether a write found, since this was last asked, that another file has taken
    /// a name this window's list showed, so that the folder must be read again.
    pub(crate) fn take_list_is_old(&self) -> bool {
        self.list_is_old.take()
    }

    /// The saved collections that the File menu lists, read in the background.
    pub(crate) fn saved_collections(&mut self) -> &mut crate::model::collections::SavedCollections {
        &mut self.saved_collections
    }

    /// The folder open, or "/" for a collection or several folders, whose names are relative
    /// to it.
    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    /// Returns the folders whose files the list shows: the folders shown together, or else the
    /// folder (`dir`), which for a collection is "/".
    pub(crate) fn places(&self) -> Vec<PathBuf> {
        match self.folders.is_empty() {
            true => vec![self.dir.clone()],
            false => self.folders.clone(),
        }
    }

    /// Returns the place that Remember Last Folder keeps: the collection or the one folder
    /// shown. The "/" that stands for several folders shown together is never kept, and neither
    /// is the empty folder before the first look-up answers. Then it is the place kept before
    /// (`place_before`), and `None` when there is none.
    pub(crate) fn place_to_remember(&self) -> Option<&Path> {
        if let Some(file) = &self.collection {
            return Some(file);
        }
        match self.folders.is_empty() && !self.dir.as_os_str().is_empty() {
            true => Some(&self.dir),
            false => self.place_before.as_deref(),
        }
    }

    /// The open collection's file, or `None` when no collection is open.
    pub(crate) fn collection(&self) -> Option<&Path> {
        self.collection.as_deref()
    }

    /// Follows the collection to its new file, after Rename Collection.
    pub(crate) fn collection_renamed(&mut self, new: PathBuf) {
        self.collection = Some(new);
    }

    /// Whether the list holds the images of the subfolders too.
    pub(crate) fn recursive(&self) -> bool {
        self.recursive
    }

    /// Returns whether the list walks the trees of `places`, so that a share mounted below one
    /// of them is read too. The "/" of a collection is no tree walked, even with `recursive`
    /// set, and asked about the mounts below it, it would see every mount on the system.
    pub(crate) fn walks_subfolders(&self) -> bool {
        self.recursive && self.collection.is_none()
    }

    /// Returns whether files put in `folder` join the list when the places shown are read again:
    /// `folder` is one of the places, or below one while their trees are walked. Before, files put
    /// anywhere counted with several folders or a collection shown with Subfolders on, as their
    /// `dir` is "/", and the folders shown were read again for nothing. Both paths are real, as
    /// `listing::walk` lists a folder reached through a link only when its real path is inside.
    pub(crate) fn takes_files_put_in(&self, folder: &Path) -> bool {
        let walks = self.walks_subfolders();
        (self.places().iter()).any(|place| folder == place || (walks && folder.starts_with(place)))
    }

    /// Lists the subfolders' images too, or not, from the next listing on.
    pub(crate) fn set_recursive(&mut self, on: bool) {
        self.recursive = on;
    }

    /// Turns View → Subfolders on or off. The next listing shows a tree's images as they are
    /// found, when it is on.
    pub(crate) fn show_subfolders(&mut self, on: bool) {
        self.recursive = on;
        self.lists_tree_in_parts = on;
    }

    /// The folders shown together as one list. It is empty when one folder or a collection is
    /// open.
    pub(crate) fn folders(&self) -> &[PathBuf] {
        &self.folders
    }

    /// The subfolders of the open folder, for the folder list.
    pub(crate) fn subdirs(&self) -> &[PathBuf] {
        &self.subdirs
    }

    /// Takes `dir` out of the folder list, because it was moved to the trash.
    pub(crate) fn forget_subdir(&mut self, dir: &Path) {
        self.subdirs.retain(|d| d != dir);
    }

    /// Makes the folders shown together, and the collection's own file, follow the folder
    /// `old`, which was renamed to `new`.
    pub(crate) fn follow_rename(&mut self, old: &Path, new: &Path) {
        for folder in &mut self.folders {
            if let Ok(rest) = folder.strip_prefix(old) {
                *folder = new.join(rest);
            }
        }
        if let Some(file) = &mut self.collection
            && let Ok(rest) = file.strip_prefix(old)
        {
            *file = new.join(rest);
        }
    }

    /// Makes the open folder follow the folder `old`, which was renamed to `new`. Returns the
    /// open folder's new place when it was `old` or inside it, and `None` otherwise.
    pub(crate) fn dir_renamed(&mut self, old: &Path, new: &Path) -> Option<PathBuf> {
        let rest = self.dir.strip_prefix(old).ok()?;
        self.dir = new.join(rest);
        Some(self.dir.clone())
    }

    /// Whether the folder is being read in the background, or the place to open looked up
    /// before it is read.
    pub(crate) fn is_listing(&self) -> bool {
        self.listing_rx.is_some() || self.looking_up.is_some()
    }

    /// Forgets that Glimt showed the first image of a tree's first part by itself
    /// (`first_part_shown`), because an image was shown since, by the user, the slideshow or a
    /// new list, or the slideshow stopped. A whole list that comes later then keeps the image
    /// shown. Before, a stop on that image while the tree was read had the whole list go to its
    /// first image, minutes later on a share.
    pub(crate) fn forget_image_shown_by_itself(&mut self) {
        self.first_part_shown = None;
    }

    /// Returns whether `name`, a name of the list, is one of the own images of the folder
    /// shown, or of the folders shown together, and not of a subfolder. A collection's images
    /// are all its own. The names of several folders are their paths without the leading "/",
    /// so a name of the folder itself is no name without "/" there.
    pub(crate) fn holds_as_its_own(&self, name: &OsString) -> bool {
        if self.collection.is_some() {
            return true;
        }
        let parent = Path::new(name).parent().unwrap_or(Path::new(""));
        match self.folders.is_empty() {
            true => parent.as_os_str().is_empty(),
            false => (self.folders.iter()).any(|d| d.strip_prefix("/").unwrap_or(d) == parent),
        }
    }

    /// Whether a tree is being read in parts, whose images come after those listed so far: the
    /// tree of a folder opened, or of a slideshow with its subfolders.
    pub(crate) fn reads_tree_in_parts(&self) -> bool {
        self.listing_rx.is_some() && self.listing_sends_parts
    }

    /// Waits up to five seconds for the next part of the tree being read, and takes its names.
    /// The whole list stays in its channel.
    #[cfg(test)]
    pub(crate) fn wait_for_part(&mut self) -> Option<Vec<ListedName>> {
        (0..250).find_map(|_| match self.part_rx.next() {
            Took::Answer(part) => Some(part.rows.listed_names()),
            _ => {
                std::thread::sleep(std::time::Duration::from_millis(20));
                None
            }
        })
    }

    /// Whether the place to open is still being looked up, so that the window still shows the
    /// list from before it.
    pub(crate) fn is_looking_up(&self) -> bool {
        self.looking_up.is_some()
    }

    /// Whether the place to open is still being looked up or mounted, so that the window still
    /// shows the list from before it.
    pub(crate) fn is_opening(&self) -> bool {
        self.is_looking_up() || self.is_mounting()
    }

    /// Whether a network location is being mounted.
    pub(crate) fn is_mounting(&self) -> bool {
        self.mounting_rx.is_some()
    }

    /// Takes the answer of the mount, when it has come.
    pub(crate) fn take_mounted(&mut self) -> Took<(Mounted, Login)> {
        self.mounting_rx.take()
    }

    /// Has the folder read again once the listing that runs is done, or at once when none
    /// runs.
    pub(crate) fn relist_later(&mut self) {
        self.relist_when_done = true;
        self.relist_whole = true;
    }

    /// Notes, when `changed`, that the watcher saw the folder change. A change has the folder
    /// read again once no listing runs and the window has rested from the last one
    /// `REST_AFTER_LISTING` times as long as it took. Returns how long the change still waits
    /// for that rest, when it waits for it.
    pub(crate) fn relist_after_rest(&mut self, changed: bool) -> Option<std::time::Duration> {
        self.change_waits |= changed;
        // A listing that runs sets a new rest when it is done.
        if !self.change_waits || self.listing_rx.is_some() {
            return None;
        }
        let left = self.rest_until.saturating_duration_since(Instant::now());
        if !left.is_zero() {
            return Some(left);
        }
        self.change_waits = false;
        self.relist_when_done = true;
        None
    }

    /// Whether the folder is to be read again.
    pub(crate) fn wants_relist(&self) -> bool {
        self.relist_when_done
    }

    /// Returns how many whole listings were taken since the start, so a later count means the
    /// list was read again since.
    pub(crate) fn whole_listings(&self) -> u64 {
        self.whole_listings
    }

    /// Whether the folder watcher saw the folder change since this was last asked. The folders
    /// it saw change are kept for the next listing (`App::relist_changes`).
    pub(crate) fn changed_on_disk(&mut self) -> bool {
        self.receive_changes();
        match std::mem::take(&mut self.changes_seen) {
            ChangesSeen::NotTold(common) => {
                self.changes_seen = ChangesSeen::Told(common);
                true
            }
            seen => {
                self.changes_seen = seen;
                false
            }
        }
    }

    /// Takes what the folder watcher saw change, keeping the folders for the next listing, so
    /// that a minimized window, which draws no frame, takes them too (`App::receive_answers`).
    /// Before, only a drawn frame took them, and they piled up in the channel while the window
    /// was minimized (#188).
    pub(crate) fn receive_changes(&mut self) {
        for folder in self.changed_rx.try_iter().flatten() {
            let common = match self.changes_seen.folder() {
                None => folder,
                Some(seen) => (seen.components().zip(folder.components()))
                    .take_while(|(a, b)| a == b)
                    .map(|(a, _)| a)
                    .collect(),
            };
            self.changes_seen = ChangesSeen::NotTold(common);
        }
    }

    /// Returns the subfolder of the tree, relative to it, that holds every change the watcher
    /// saw since the last listing began, when reading it again is enough: the whole tree is
    /// listed (`listed_whole_as` gives how), nothing else asked for it to be read whole, and
    /// the open folder itself did not change. Returns `None` when the tree is read whole.
    fn subfolder_changed(&self, listed_whole_as: &ListedAs) -> Option<PathBuf> {
        let one_tree = self.collection.is_none() && self.folders.is_empty() && self.recursive;
        let listed = self.listed_as.as_ref() == Some(listed_whole_as);
        if self.relist_whole || !one_tree || !listed {
            return None;
        }
        // A change outside the open folder leaves a common folder above it, or none.
        let rel = self.changes_seen.folder()?.strip_prefix(&self.dir).ok()?;
        (!rel.as_os_str().is_empty()).then(|| rel.to_path_buf())
    }

    /// Opens `all` as folders shown together, after the frame.
    pub(crate) fn show_together(&mut self, all: Vec<PathBuf>) {
        self.together = Some(all);
    }

    /// Takes the folders to show together, when some were chosen.
    pub(crate) fn take_together(&mut self) -> Option<Vec<PathBuf>> {
        self.together.take()
    }

    /// The sort manager's folders.
    pub(crate) fn sort_folders(&self) -> &[PathBuf] {
        &self.sort_folders
    }

    /// Shows the sort manager's folders `dirs` at once, and those that `written` sends once
    /// they are written to their file, with another window's folders.
    pub(crate) fn set_sort_folders(
        &mut self,
        dirs: Vec<PathBuf>,
        written: std::sync::mpsc::Receiver<Vec<PathBuf>>,
    ) {
        self.sort_folders = dirs;
        self.sort_folders_written = Receiving::of(written);
    }

    /// Takes the sort manager's folders as their file has them once this window's last change
    /// is written. So another window's folders show here after this window's next change, as
    /// they did when the window wrote the file itself.
    pub(crate) fn receive_sort_folders(&mut self) {
        if let Took::Answer(dirs) = self.sort_folders_written.take() {
            self.sort_folders = dirs;
        }
    }

    /// The type of the file opened directly, which is listed even when the Types menu leaves
    /// it out.
    pub(crate) fn opened_type(&self) -> Types {
        self.opened_type
    }

    /// Has the next listing sorted anew, because the sort order changed. A collection is then
    /// saved in the new order.
    pub(crate) fn sort_again(&mut self) {
        self.sort_collection = self.collection.is_some();
        self.listed_as = None;
    }

    /// The folder path in the toolbar, as typed.
    pub(crate) fn path_text(&self) -> &str {
        &self.path_text
    }

    /// The folder path in the toolbar, as it is being typed.
    pub(crate) fn path_text_mut(&mut self) -> &mut String {
        &mut self.path_text
    }

    /// Puts what is open into the folder path in the toolbar, while it is not being typed in.
    pub(crate) fn show_path(&mut self) {
        let shown = self.collection.as_ref().unwrap_or(&self.dir);
        self.path_text = match self.folders.is_empty() {
            true => shown.to_string_lossy().into_owned(),
            // With several folders, it is each of them.
            false => (self.folders.iter())
                .map(|f| f.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(" + "),
        };
    }

    /// Opens the login dialog for a network share, with `login` filled in.
    pub(crate) fn ask_login(&mut self, login: Login) {
        self.login = Some(login);
    }

    /// Takes the login dialog's fields while it is drawn. It stays open only when they are
    /// given back with `ask_login`.
    pub(crate) fn take_login(&mut self) -> Option<Login> {
        self.login.take()
    }

    /// Whether the login dialog is open.
    pub(crate) fn asks_login(&self) -> bool {
        self.login.is_some()
    }
}

/// For tests of other modules, which hold a listing back to see what waits for it.
#[cfg(test)]
impl Folder {
    /// Makes `listing` the listing in the background.
    pub(crate) fn hold_listing(&mut self, listing: mpsc::Receiver<Listed>) {
        self.listing_rx = Receiving::of(listing);
    }

    /// Drops the listing in the background, as when it is done.
    pub(crate) fn drop_listing(&mut self) {
        self.listing_rx = Receiving::none();
    }

    /// Starts a look-up that never answers, as on a share that stopped answering, until the
    /// value returned is dropped.
    pub(crate) fn hold_look_up(&mut self) -> impl Sized + use<> {
        let (tx, rx) = mpsc::channel();
        self.looking_up = Receiving::of(rx);
        tx
    }
}

/// What a list was read and sorted as: the folder, the types shown, the order, ascending, case
/// and subfolders.
pub(crate) type ListedAs = (PathBuf, Types, listing::Sort, bool, bool, bool);

/// A part of a tree as a listing thread sends it.
pub(crate) struct TreePart {
    /// The images found since the part before that the list lacks, in the listing's order,
    /// each with what it is sorted by in an order that looks at the files.
    rows: Rows,
    /// The folders read whole where the list may hold a name that is gone.
    read: ReadWhole,
    /// In the first part of a listing that began while the list was in another order than the
    /// listing's, the list's rows as the listing began, put in the listing's order with what
    /// each is sorted by in it (`PartsFrom`'s `to_sort_anew`). The list takes them before the
    /// part goes in, so every part goes into a list of the listing's own order.
    list_sorted_anew: Option<Rows>,
}

/// What a listing thread sends when it has read the folder.
pub(crate) struct Listed {
    /// The subfolders of the folder listed.
    dirs: Vec<PathBuf>,
    /// The images, or an error with what could not be read when the listing failed.
    files: Result<ListedRows, Unread>,
    /// The parts that could not be read and are left out of `files`: folders of a tree or of
    /// several folders, and entries of a collection.
    left_out: Vec<Unread>,
    /// The folders read, with their modification times, for the folder watcher.
    read: FoldersRead,
    /// How long the reading took in the listing thread.
    read_in: Duration,
}

/// What the folder watcher saw change since the last listing began: the deepest folder that
/// holds every folder it saw change, and whether `Folder::changed_on_disk` has told it. Only
/// this one path is kept, so a tree that a sync client writes into every second does not grow
/// it during a long rest. Before, each folder seen was kept, also every time again, until the
/// next listing.
#[derive(Default)]
enum ChangesSeen {
    /// The watcher saw no change.
    #[default]
    Nothing,
    /// It saw changes below this folder that `changed_on_disk` has not told yet.
    NotTold(PathBuf),
    /// It saw changes below this folder, and `changed_on_disk` has told them.
    Told(PathBuf),
}

impl ChangesSeen {
    /// Returns the folder that holds every change seen, or `None` when none was seen.
    fn folder(&self) -> Option<&Path> {
        match self {
            ChangesSeen::Nothing => None,
            ChangesSeen::NotTold(folder) | ChangesSeen::Told(folder) => Some(folder),
        }
    }
}

/// The images of a listing as its thread sends them.
pub(crate) enum ListedRows {
    /// The images in the listing's order, or in a collection's own.
    Rows(Rows),
    /// The names of the list as the listing began, in the list's order, with the identities
    /// the listing gave them, when no name is new and the listing kept that order
    /// (`listing::kept_order`). No file was looked at, so the window gives each name what it is
    /// sorted by from its own rows (`Rows::in_step`).
    InListOrder(Vec<ListedName>),
    /// A subfolder of the tree was read again, and its parts brought the names new there. These
    /// are every name the subfolder holds now, with the identities it was found with, which the
    /// names that stayed take (`App::rows_with_subfolder_read_again`). The rest of the list
    /// stays as it is.
    InParts(Vec<ListedName>),
}

/// What a listing of a tree makes its parts from, as the window gives it to the listing thread
/// when the listing starts (`App::parts_from`). The thread makes what it sends of the parts
/// from it in one way (`PartsFrom::parts_sent`), and so do the tests that feed a listing, so
/// the two cannot differ.
struct PartsFrom {
    /// The folder listed, or the one the paths of several folders are relative to.
    dir: PathBuf,
    /// The places listed, the folder or the several folders, which tell whether the looks at
    /// the files wait for the reads on a share.
    places: Vec<PathBuf>,
    recursive: bool,
    /// The listing's order, as (order, ascending, case), which its parts go into the list in.
    order: (listing::Sort, bool, bool),
    /// Set when another listing starts, which ends the looks' waits.
    stop: Arc<AtomicBool>,
    /// The images that the window waits for, which the looks on a share wait for.
    awaited: crate::model::listing::ImagesAwaited,
    /// The hashes of the names that the list held when the listing began, for a listing that
    /// sends parts, which hold only the images new to the list, and none otherwise. Glimt's own
    /// changes to the list's names start another listing, so the list holds no other name of
    /// the tree until the whole list comes.
    listed_before: NameHashes,
    /// The list's names when the listing began, for a listing that sends parts while the list
    /// is in another order than the listing's, and none otherwise. The thread puts them in the
    /// listing's order, with a look at each file in an order that looks at the files, and
    /// sends them with the first part, so that every part goes into a list of its own order.
    to_sort_anew: Vec<ListedName>,
}

impl PartsFrom {
    /// Returns what the listing sends of a tree's parts before it has sent any. Its looks at the
    /// files wait for the reads that go first when one of the places listed is on a share.
    fn parts_sent(&mut self) -> PartsSent {
        let (by, ascending, case) = self.order;
        let on_a_share = (self.places.iter()).any(|d| listing::is_on_a_share(d, self.recursive));
        PartsSent {
            reads_first: listing::ReadsFirst::new(
                self.stop.clone(),
                self.awaited.clone(),
                on_a_share,
            ),
            last: None,
            up_to: 0,
            listed_before: std::mem::take(&mut self.listed_before).sorted(),
            read_whole: ReadWhole::default(),
            order: (self.dir.clone(), by, ascending, case),
            keys: Vec::new(),
            to_sort_anew: std::mem::take(&mut self.to_sort_anew),
            list_sorted_anew: None,
            keys_sorted_anew: Vec::new(),
        }
    }
}

/// What a listing thread reads with, and where it sends the list and then the sizes.
struct Listing {
    /// The folder listed, or the one the paths of a collection or of several folders are
    /// relative to.
    dir: PathBuf,
    types: Types,
    by: listing::Sort,
    ascending: bool,
    case: bool,
    recursive: bool,
    /// The subfolder of the tree, relative to it, that is read again in place of the whole
    /// tree, or empty for the whole tree.
    from: PathBuf,
    /// Set when another listing starts, so this one stops reading a tree.
    stop: Arc<AtomicBool>,
    /// The images that the window waits for, which the walk of a tree on a share waits for.
    awaited: crate::model::listing::ImagesAwaited,
    /// What the parts of a tree are made from.
    parts: PartsFrom,
    /// How long the window took to take the last part, in microseconds (`Folder`'s
    /// `part_taken_in`).
    part_taken_in: Arc<AtomicU64>,
    tx: mpsc::Sender<Listed>,
    size_tx: mpsc::Sender<u64>,
    /// When the thread started reading. The time it took is measured here, in the thread,
    /// because a window that is not drawn takes the list late, and that wait is no listing.
    started: Instant,
    ctx: egui::Context,
}

impl Listing {
    /// Lists several folders, each folder (with its subfolders) after the other, as one list in
    /// the sort order, which in the default order by name is the names' byte order across the
    /// folders. A folder that cannot be read is left out, and sent with the list, so the others
    /// are still listed.
    /// With their subfolders, their images come in parts as they are found, as a tree's do, by
    /// their names in the list: the folder's path without its leading "/" and the name below it.
    fn folders(mut self, folders: &[PathBuf], part_tx: mpsc::SyncSender<TreePart>) {
        let (mut read, mut left_out) = (Vec::new(), Vec::new());
        let mut files: Vec<ListedName> = Vec::new();
        let nested = (folders.iter()).any(|d| folders.iter().any(|o| o != d && d.starts_with(o)));
        let mut sent = self.parts.parts_sent();
        // The images found so far, by their names in the list, which the parts are made of.
        let mut so_far: Vec<ListedName> = Vec::new();
        // A folder inside another one given is read twice, and its second read is no part.
        let mut folders_read: HashSet<PathBuf> = HashSet::new();
        for d in folders {
            let at = d.strip_prefix("/").unwrap_or(d).to_path_buf();
            let mut part = |read: &FolderReadWhole| {
                let folder = at.join(read.folder);
                if !folders_read.insert(folder.clone()) {
                    return;
                }
                let first = so_far.len();
                let images = read.so_far[read.first..].iter();
                so_far.extend(images.map(|(n, id)| (at.join(n).into_os_string(), *id)));
                let read = FolderReadWhole {
                    so_far: &so_far,
                    first,
                    folder: &folder,
                    dirs: read.dirs,
                    types: read.types,
                };
                self.send_part(&read, &mut sent, &part_tx);
            };
            // The images of a tree are those of its parts, in the order they were found, as
            // what the parts looked at is kept by that order.
            let listed = match self.recursive {
                true => {
                    let whole = (d.as_path(), Path::new(""));
                    walk_tree(
                        whole,
                        self.types,
                        &self.stop,
                        &self.awaited,
                        |_| {},
                        &mut part,
                    )
                }
                .map(|(_, _, tree, unread)| (Vec::new(), tree, unread)),
                false => {
                    let tree = vec![(d.clone(), modified(d))];
                    (list_folder(d, self.types).map(|(_, files)| (files, tree, Vec::new())))
                        .map_err(|e| (d.clone(), e))
                }
            };
            match listed {
                Ok((names, tree, unread)) => {
                    read.extend(tree);
                    left_out.extend(unread);
                    let at = d.strip_prefix("/").unwrap_or(d).to_path_buf();
                    files.extend(
                        (names.into_iter()).map(|(n, id)| (at.join(n).into_os_string(), id)),
                    );
                }
                // It stays watched, so a change in it lists the folders again.
                Err(unread) => {
                    read.push((d.clone(), modified(d)));
                    left_out.push(unread);
                }
            }
        }
        let mut known = Vec::new();
        if self.recursive {
            files = so_far;
            known = sent.take_keys();
        }
        // A folder inside another one given has its images listed once.
        if nested || !self.recursive {
            let mut seen = HashSet::new();
            let known_too = known.into_iter().chain(std::iter::repeat_with(|| None));
            let kept = (files.into_iter().zip(known_too)).filter(|(f, _)| seen.insert(f.0.clone()));
            (files, known) = kept.unzip();
        }
        forget_passing_identities(&self.dir, &mut files);
        // The whole list is in the order its parts go in by (`listing::in_order`): byte order of
        // the names for the default order, by name with Case, and the sort order otherwise. So
        // the rows never move when the whole list comes after the parts. Before, each folder's
        // images came in name order one folder after the other, in the order the folders were
        // given, while the parts went in by byte order across them.
        let rows = sent.sorted(files, known);
        // The sizes come after, as for a folder, so the list shows first.
        let paths: Vec<PathBuf> = rows.files().iter().map(|f| self.dir.join(f)).collect();
        let listed = Listed {
            dirs: Vec::new(),
            files: Ok(ListedRows::Rows(rows)),
            left_out,
            read,
            read_in: self.started.elapsed(),
        };
        if self.tx.send(listed).is_ok() {
            self.ctx.request_repaint();
            let sizes = paths
                .into_iter()
                .filter_map(|p| Some(p.metadata().ok()?.len()));
            send_sizes_unless_on_a_share(folders, self.recursive, sizes, self.size_tx, &self.ctx);
        }
    }

    /// Sends that the listing failed on `unread`, with the folders `read` for the folder
    /// watcher, the one that failed among them. The window keeps the list it shows, and a
    /// change that the watcher sees lists the folder again.
    fn send_unread(self, unread: Unread, read: FoldersRead) {
        let failed = Listed {
            dirs: Vec::new(),
            files: Err(unread),
            left_out: Vec::new(),
            read,
            read_in: self.started.elapsed(),
        };
        if self.tx.send(failed).is_ok() {
            self.ctx.request_repaint();
        }
    }

    /// A collection's files that still exist, from its file when it has one (`saved`), in
    /// its own order unless `sort`.
    fn collection(self, files: Vec<OsString>, saved: Option<PathBuf>, sort: bool) {
        // The window's edits of the file are written in the writer thread, and those asked for
        // before this listing are read with it.
        if saved.is_some() {
            crate::files::file_io::wait_for_queued_writes();
        }
        let paths = match saved.map(|f| collections::load_with_window_changes(&f)) {
            Some(Ok(paths)) => paths,
            _ => files.iter().map(|f| self.dir.join(f)).collect(),
        };
        // Each entry is listed by its real path, as a folder's files are, so that an entry named
        // through a link has its marks and stars and is found by `--remote select`.
        let mut files: Vec<ListedName> = (collections::by_real_path(&paths).into_iter())
            .filter_map(|(p, _)| Some(p.strip_prefix(&self.dir).ok()?.as_os_str().to_owned()))
            .map(|f| (f, None))
            .collect();
        // A list keeps only files, as `listing::kind` does in a folder. A folder or `/` named
        // in a downloaded list was kept, shown as broken, and Delete sent the whole folder to
        // the trash. Until this listing has come, `are_free_to_change` holds every file operation
        // back. Each file's identity is that of its own name, a link's and not its target's, as
        // a folder entry gives it. An entry that is gone is left out. One that could not be
        // looked at for another reason is left out too, and sent with the list, so that the
        // window tells it, or keeps the list when it shows the entry (a share that failed for a
        // moment).
        let mut left_out: Vec<Unread> = Vec::new();
        // Each look at an entry waits on a share for the image that the window waits for, as
        // a folder's looks do (`ReadsFirst`). Before, a sorted collection on a share read the
        // EXIF of every entry in a row, and the next image of a slideshow waited behind them.
        let mut gate =
            listing::ReadsFirst::for_files_anywhere(&self.awaited).ended_by(self.stop.clone());
        let sizes = keep_files_with_size(&mut files, |(f, identity)| {
            let path = self.dir.join(f);
            gate.before_a_look(&path);
            let m = looked_at(std::fs::symlink_metadata(&path), &path, &mut left_out)?;
            *identity = crate::files::file_io::identity_from_metadata(&path, &m);
            let meta = match m.is_symlink() {
                true => looked_at(path.metadata(), &path, &mut left_out)?,
                false => m,
            };
            // The file types and hidden names shown in a folder are those shown in a list too.
            // A `.m3u` or downloaded list named music, documents and dot files.
            (meta.is_file() && is_listed(&path, self.types)).then_some(meta.len())
        });
        forget_passing_identities(&self.dir, &mut files);
        // Sorted, each row carries what it is sorted by in an order that looks at the files, as
        // a folder's rows do, and otherwise the collection keeps its own order.
        let (by, dir) = (self.by, &self.dir);
        let rows = match sort {
            true => Rows::sorted(files, (by, self.ascending, self.case), |_, name| {
                gate.sort_key(&dir.join(name), by)
            }),
            false => Rows::unsorted(files),
        };
        let listed = Listed {
            dirs: Vec::new(),
            files: Ok(ListedRows::Rows(rows)),
            left_out,
            read: Vec::new(),
            read_in: self.started.elapsed(),
        };
        if self.tx.send(listed).is_ok() {
            self.ctx.request_repaint();
            send_sizes(sizes.into_iter(), self.size_tx, &self.ctx);
        }
    }

    /// Notes the folder of the tree just read whole, `read`, and sends the window a part of
    /// the tree read so far, unless the part before was sent too short a time ago, as `sent`
    /// holds.
    fn send_part(
        &self,
        read: &FolderReadWhole,
        sent: &mut PartsSent,
        part_tx: &mpsc::SyncSender<TreePart>,
    ) {
        sent.listed_before.note_folder(read, &mut sent.read_whole);
        if sent.last.is_some_and(|(t, wait)| t.elapsed() < wait) {
            return;
        }
        let began = Instant::now();
        let Some(part) = sent.next_part(read.so_far) else {
            return;
        };
        // When the channel is full, the part before is not taken yet, so this one is left out,
        // and its new images and the folders it read go with the next one. What was looked at
        // of its images stays, so they are not looked at again.
        match part_tx.try_send(part) {
            Ok(()) => {
                sent.part_sent(read.so_far.len());
                self.ctx.request_repaint();
            }
            Err(mpsc::TrySendError::Full(part) | mpsc::TrySendError::Disconnected(part)) => {
                sent.read_whole = part.read;
                sent.list_sorted_anew = part.list_sorted_anew;
            }
        }
        let in_window = self.part_taken_in.load(Ordering::Relaxed);
        let wait = next_part_after(began.elapsed(), Duration::from_micros(in_window));
        sent.last = Some((Instant::now(), wait));
    }

    /// Sends what the walk of the subfolder `from` found, `files`, as the last part, and then
    /// the listing as done with the folders it `read` and those `left_out`, and with every name
    /// of the subfolder and the identity it was found with. The list takes the parts in, which
    /// replace the subfolder's names with what it holds now, and keeps the rest of the tree as
    /// it is.
    fn send_subfolder_read_again(
        &self,
        mut files: Vec<ListedName>,
        (read, left_out): (FoldersRead, Vec<Unread>),
        mut sent: PartsSent,
        part_tx: &mpsc::SyncSender<TreePart>,
    ) {
        // The last part is sent whatever the time since the part before, and the listing waits
        // until the window has room for it, so that the list takes it before the listing ends.
        if let Some(part) = sent.next_part(&files) {
            if part_tx.send(part).is_err() {
                return;
            }
            self.ctx.request_repaint();
        }
        forget_passing_identities(&self.dir, &mut files);
        let listed = Listed {
            dirs: Vec::new(),
            files: Ok(ListedRows::InParts(files)),
            left_out,
            read,
            read_in: self.started.elapsed(),
        };
        if self.tx.send(listed).is_ok() {
            self.ctx.request_repaint();
        }
    }

    /// Lists a folder, or its tree, whose images it sends as they are read. `before` is the
    /// list in EXIF date order, which is kept if no name is new.
    fn folder(
        mut self,
        before: Option<Vec<OsString>>,
        part_tx: mpsc::SyncSender<TreePart>,
        subdirs_tx: mpsc::Sender<Vec<PathBuf>>,
    ) {
        // The folder is read once, also with the folders below it. Its own subfolders show at
        // once, because a large tree takes long to read whole.
        // Its images come too, a folder of the tree at a time, each part in the order the list
        // is in. The first images are sent at once, and after them at most four times a second,
        // because the window merges each part into the whole list.
        let mut sent = self.parts.parts_sent();
        // The images of a tree come in the order the walk found them, which is the order that
        // what the parts looked at is kept by, for the sort of the whole list.
        let listed = match self.recursive {
            true => walk_tree(
                (&self.dir, &self.from),
                self.types,
                &self.stop,
                &self.awaited,
                |dirs| {
                    if subdirs_tx.send(dirs.to_vec()).is_ok() {
                        self.ctx.request_repaint();
                    }
                },
                |read| self.send_part(read, &mut sent, &part_tx),
            ),
            false => {
                // The time is taken before the folder is read, so a change made while it is
                // read is seen too.
                let read = vec![(self.dir.clone(), modified(&self.dir))];
                (list_folder(&self.dir, self.types))
                    .map(|(dirs, files)| (dirs, files, read, Vec::new()))
                    .map_err(|e| (self.dir.clone(), e))
            }
        };
        // It stops when another folder is listed.
        if self.stop.load(Ordering::Relaxed) {
            return;
        }
        let (dirs, files, read, left_out) = match listed {
            Ok(listed) => listed,
            Err(unread) => {
                let read = vec![(self.dir.clone(), modified(&self.dir))];
                return self.send_unread(unread, read);
            }
        };
        if !self.from.as_os_str().is_empty() {
            return self.send_subfolder_read_again(files, (read, left_out), sent, &part_tx);
        }
        // In an order that looks at the files, each image that a part looked at is not looked
        // at again, so each is looked at once in the walk.
        let rows = match before.and_then(|before| listing::kept_order(before, &files)) {
            Some(mut kept) => {
                forget_passing_identities(&self.dir, &mut kept);
                ListedRows::InListOrder(kept)
            }
            None => {
                let mut files = files;
                forget_passing_identities(&self.dir, &mut files);
                let known = sent.take_keys();
                ListedRows::Rows(sent.sorted(files, known))
            }
        };
        // The receiver is gone if another folder was opened meanwhile; then we stop.
        let listed = Listed {
            dirs,
            files: Ok(rows),
            left_out,
            read,
            read_in: self.started.elapsed(),
        };
        if self.tx.send(listed).is_err() {
            return;
        }
        self.ctx.request_repaint();
        // The sizes come afterwards. The folder is read once more and summed from the directory
        // entries in chunks, so the status bar fills in gradually without a copy of the whole
        // list in memory. A tree is read a folder at a time, unsorted. On a share they are not
        // summed, as that is a look at each file and the tree read again.
        let places = [self.dir.clone()];
        if self.recursive {
            let sizes = walk(&self.dir, self.types)
                .flatten()
                .flat_map(|(rel, _, files, _)| {
                    let folder = self.dir.join(rel);
                    (files.into_iter())
                        .filter_map(move |f| Some(folder.join(f.0).metadata().ok()?.len()))
                });
            return send_sizes_unless_on_a_share(&places, true, sizes, self.size_tx, &self.ctx);
        }
        let images = std::fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| listing::kind(e).0 && is_listed(&e.path(), self.types));
        // Through a link, the size of the file it points to, as the list shows that.
        let sizes = images.filter_map(|e| Some(std::fs::metadata(e.path()).ok()?.len()));
        send_sizes_unless_on_a_share(&places, false, sizes, self.size_tx, &self.ctx);
    }
}

/// What a listing thread sent of a tree's parts.
struct PartsSent {
    /// When it sent the last part, and how long it waits after it before the next.
    last: Option<(Instant, Duration)>,
    /// How many of the images found so far the parts sent have held.
    up_to: usize,
    /// The hashes of the names that the list held when the listing began, sorted, which a part
    /// of new names only leaves out (`Listing`'s `listed_before`).
    listed_before: NameHashes,
    /// The folders read whole since the last part sent where the list may hold a name that
    /// is gone, which the next part carries.
    read_whole: ReadWhole,
    /// The folder listed, and the listing's order, which the parts go into the list in: (order,
    /// ascending, case).
    order: (PathBuf, listing::Sort, bool, bool),
    /// In an order that looks at the files, what each image found so far is sorted by, at its
    /// place among them, once a part has looked at it. A part that could not be sent keeps
    /// what it looked at here, and the whole list is sorted by it, so each image is looked at
    /// once in the walk. The thread keeps no names of the list beside it: a copy of them with
    /// their keys held 56 to 64 MB at 500,000 names.
    keys: Vec<Option<SortKey>>,
    /// The list's names to put in the listing's order with the first part, when the list was
    /// in another order (`PartsFrom`'s `to_sort_anew`), until the first part puts them in it.
    to_sort_anew: Vec<ListedName>,
    /// The list's rows put in the listing's order, until a part that holds them is sent.
    list_sorted_anew: Option<Rows>,
    /// What the list's names put in the listing's order are sorted by, by the hashes of the
    /// names (`listing::name_hash`), sorted, for the sort of the whole list, so that each of
    /// them is looked at once in the walk. A hash that two names share is left out, and both
    /// names are looked at again.
    keys_sorted_anew: Vec<(u64, SortKey)>,
    /// The reads on a share that each look at a file waits for, the parts' looks, the list's
    /// rows put in another order and the sort of the whole list alike. Before, a part looked at
    /// the new images of every folder since the part before, 600 to 900 EXIF reads in a row,
    /// after one wait before the folder, and the image awaited queued behind them (#256).
    reads_first: listing::ReadsFirst,
}

impl PartsSent {
    /// Returns the next part of the tree read so far, `so_far`: the images found since the
    /// part before that the list did not hold, in the listing's order, with the folders read
    /// whole since the part before where a name may be gone, and in an order that looks at the
    /// files what each image is sorted by. The first part also brings the list's rows put in the
    /// listing's order, when the list was in another. Each image is looked at once, also when
    /// a part that held it could not be sent. Returns `None` when it would hold nothing.
    fn next_part(&mut self, so_far: &[ListedName]) -> Option<TreePart> {
        // A part holds only the images found since the part before. A copy of the whole tree
        // so far cost the window a look at every name of it for each part, about half a second
        // at 500,000 names.
        let before = &self.listed_before;
        let new: Vec<usize> = (self.up_to..so_far.len())
            .filter(|&i| before.is_empty() || !before.holds(&so_far[i].0))
            .collect();
        let nothing_anew = self.to_sort_anew.is_empty() && self.list_sorted_anew.is_none();
        if new.is_empty() && self.read_whole.is_empty() && nothing_anew {
            self.up_to = so_far.len();
            return None;
        }
        let (dir, by, ascending, case) = self.order.clone();
        let order = (by, ascending, case);
        if by.looks_at_files() {
            self.keys.resize(so_far.len(), None);
        }
        // Each look waits for the reads that go first (`ReadsFirst`).
        let reads_first = &mut self.reads_first;
        let mut look = |name: &OsString| reads_first.sort_key(&dir.join(name), by);
        // The list's own rows are put in the listing's order once, before the walk's parts go
        // into them.
        if !self.to_sort_anew.is_empty() {
            let names = std::mem::take(&mut self.to_sort_anew);
            let rows = Rows::sorted(names, order, |_, name| look(name));
            self.keys_sorted_anew = keys_by_name_hash(&rows);
            self.list_sorted_anew = Some(rows);
        }
        let keys = &mut self.keys;
        let mut part: Vec<ListedName> = new.iter().map(|&i| so_far[i].clone()).collect();
        forget_passing_identities(&dir, &mut part);
        let rows = Rows::sorted(part, order, |j, name| {
            (keys[new[j]].get_or_insert_with(|| look(name))).clone()
        });
        Some(TreePart {
            rows,
            read: std::mem::take(&mut self.read_whole),
            list_sorted_anew: self.list_sorted_anew.take(),
        })
    }

    /// Notes that the part made of the first `so_far` images found was sent, so the next one
    /// holds only the images found after them.
    fn part_sent(&mut self, so_far: usize) {
        self.up_to = so_far;
    }

    /// Takes what the parts looked at of each image found so far, by its place among them, for
    /// the sort of the whole list.
    fn take_keys(&mut self) -> Vec<Option<SortKey>> {
        std::mem::take(&mut self.keys)
    }

    /// Returns `files`, the images of the whole list, as rows in the listing's order. Each image
    /// is sorted by what `known` holds at its place (what a part looked at), or else by what it
    /// was sorted by when the list's names were put in the listing's order, and the others are
    /// looked at now, each look after the wait for the reads that go first (`ReadsFirst`). So
    /// each image is looked at once in the walk.
    fn sorted(&mut self, files: Vec<ListedName>, mut known: Vec<Option<SortKey>>) -> Rows {
        let (dir, by, ascending, case) = self.order.clone();
        let anew = std::mem::take(&mut self.keys_sorted_anew);
        let sorted_anew = |name: &OsString| {
            let hash = listing::name_hash(name.as_encoded_bytes());
            let at = anew.binary_search_by_key(&hash, |k| k.0).ok()?;
            Some(anew[at].1.clone())
        };
        Rows::sorted(files, (by, ascending, case), |i, name| {
            (known.get_mut(i).and_then(Option::take))
                .or_else(|| sorted_anew(name))
                .unwrap_or_else(|| self.reads_first.sort_key(&dir.join(name), by))
        })
    }
}

/// What was asked to be opened, and how it opens once its paths are looked up.
struct Opening {
    /// One path (a folder, a file or a collection), or several folders shown as one list.
    paths: Vec<PathBuf>,
    /// How it changes Go → Back and Forward's history, once its look-up answers.
    history: HistoryStep,
    /// What happens when the path is not there.
    if_gone: IfGone,
}

/// How opening a place changes Go → Back and Forward's history. The change is made when the
/// look-up of the place answers, not when it is asked for, so a look-up that another replaces
/// changes nothing.
#[derive(Clone, Copy)]
enum HistoryStep {
    /// The history stays as it is.
    None,
    /// The place is a new step (`visit`): the place before goes onto Back, and Forward is
    /// emptied.
    New,
    /// Go → Back: the last place of Back is taken off, and the place before goes onto Forward.
    Back,
    /// Go → Forward: the last place of Forward is taken off, and the place before goes onto
    /// Back.
    Forward,
}

/// What happens when the one path asked to be opened is not there.
#[derive(Clone, Copy)]
enum IfGone {
    /// Its folder opens, as for a file named on the command line.
    OpenItsFolder,
    /// The notice made from the path says so. The path is not opened, and it leaves Open
    /// Recent.
    Tell(fn(&Path) -> Failure),
    /// The current folder opens instead, as for the last folder shown, which may be gone since.
    OpenCurrentFolder,
}

/// What the paths asked to be opened are on disk, as `look_up` found them in the background.
enum Found {
    /// A folder by its real path, with the file to show in it and that file's type, and
    /// whether the path asked for is there at all. The type is found here too, because an added
    /// format's folder is read for it.
    Folder {
        dir: PathBuf,
        file: Option<PathBuf>,
        file_type: Types,
        is_there: bool,
    },
    /// A collection's file, made absolute, with its entries by their real paths relative to
    /// "/", or why the file could not be read.
    Collection {
        file: PathBuf,
        entries: std::io::Result<Vec<OsString>>,
    },
    /// Several folders, each by its real path, to show as one list.
    Folders(Vec<PathBuf>),
}

impl crate::App {
    // ---- Opening a folder, a collection or several folders, and listing it ----

    /// Opens a folder, a collection, or the folder of a file with that file shown, once the path
    /// is looked up in the background.
    pub(crate) fn open(&mut self, ctx: &egui::Context, path: PathBuf) {
        let opening = Opening {
            paths: vec![path],
            history: HistoryStep::None,
            if_gone: IfGone::OpenItsFolder,
        };
        self.look_up(ctx, opening);
    }

    /// Opens the folder or collection shown last, at start, once it is looked up in the
    /// background. When it is gone, the current folder opens instead. It is looked up as any
    /// other path, because a share that stopped answering would otherwise hold the start for
    /// ever, before the window opens.
    pub(crate) fn open_last_folder(&mut self, ctx: &egui::Context, path: PathBuf) {
        let opening = Opening {
            paths: vec![path],
            history: HistoryStep::None,
            if_gone: IfGone::OpenCurrentFolder,
        };
        self.look_up(ctx, opening);
    }

    /// Looks up in the background what `opening`'s paths are, and opens them when the answer
    /// comes (`receive_looked_up`). Nothing changes in the window until then. A look-up asked
    /// for later replaces this one, so the place the user went to last is the one that opens.
    fn look_up(&mut self, ctx: &egui::Context, opening: Opening) {
        // A share that answers late is not opened over the place the user goes to now. The
        // share's own success opens it through here too, after its answer was taken.
        if self.folder.is_mounting() {
            self.folder.mounting_rx = Receiving::none();
            self.notice.clear();
        }
        self.clear_still_opening();
        let path = opening.paths.first().cloned().unwrap_or_default();
        self.folder.looking_up_since = Some((Instant::now(), path));
        ctx.request_repaint_after(STILL_OPENING_AFTER);
        let (tx, rx) = mpsc::channel();
        self.folder.looking_up = Receiving::of(rx);
        let ctx = ctx.clone();
        // Its busy pointer: `is_listing`, while `looking_up` waits for this answer.
        std::thread::spawn(move || {
            let found = look_up(&opening.paths);
            let _ = tx.send((opening, found));
            ctx.request_repaint();
        });
    }

    /// Opens what a look-up found, when it has come. It waits for it up to `wait`.
    pub(crate) fn receive_looked_up(&mut self, ctx: &egui::Context, wait: std::time::Duration) {
        let (opening, found) = match self.folder.looking_up.wait(wait) {
            Took::Answer(answer) => answer,
            Took::Gone => {
                self.clear_still_opening();
                return self.tell_background_stopped(tr("Reading the folder"));
            }
            Took::Nothing => return self.tell_still_opening(ctx),
        };
        self.clear_still_opening();
        if let Found::Folder {
            is_there: false, ..
        } = &found
            && let Some(path) = opening.paths.first()
        {
            match opening.if_gone {
                IfGone::OpenItsFolder => {}
                // The path is gone since it was named (deleted, or a stick taken out), so it
                // leaves Open Recent too.
                IfGone::Tell(gone) => {
                    self.notice.give_error(gone(path));
                    return self.forget_recent(path);
                }
                IfGone::OpenCurrentFolder => return self.open(ctx, ".".into()),
            }
        }
        // A collection that could not be read opens nothing, so it is told and the history
        // stays as it is. Stepped, Back went to the place still shown and Forward was lost.
        if let Found::Collection {
            file,
            entries: Err(e),
        } = &found
        {
            let failure = Failure::because(escaped_name(file), e.to_string());
            return self.notice.give_error(failure);
        }
        self.step_history(opening.history);
        match found {
            Found::Folder {
                dir,
                file,
                file_type,
                ..
            } => self.open_folder_found(ctx, dir, file, file_type),
            Found::Collection {
                file,
                entries: Ok(files),
            } => self.open_collection(ctx, file, files),
            // Told above.
            Found::Collection {
                entries: Err(_), ..
            } => {}
            Found::Folders(folders) => self.open_folders_found(ctx, folders),
        }
    }

    /// Tells once that the place being looked up has not answered after `STILL_OPENING_AFTER`,
    /// as on a share that stopped answering, and how to stop waiting for it. Before, only the
    /// busy pointer showed it.
    fn tell_still_opening(&mut self, ctx: &egui::Context) {
        let Some((since, path)) = &self.folder.looking_up_since else {
            return;
        };
        // Each frame forgets the repaint asked for before it, so a frame that came between
        // (the pointer moved, a thumbnail came) asks again here for the notice to come in time.
        if let Some(left) = STILL_OPENING_AFTER.checked_sub(since.elapsed()) {
            return ctx.request_repaint_after(left);
        }
        let text = tf!(
            "Still opening {name}… Open another folder to stop waiting.",
            name = escaped_name(path)
        );
        self.folder.looking_up_since = None;
        self.folder.still_opening_notice = Some(text.clone());
        self.notice.give(text);
    }

    /// Takes away the notice that a place is still opening while it is shown, and forgets when
    /// its look-up started.
    fn clear_still_opening(&mut self) {
        self.folder.looking_up_since = None;
        let Some(told) = self.folder.still_opening_notice.take() else {
            return;
        };
        // A warning kept under the notice adds lines after its own, which stay after the clear.
        let is_shown = |text: &str| text == told || text.starts_with(&format!("{told}\n"));
        if self.notice.text().is_some_and(is_shown) {
            self.notice.clear();
        }
    }

    /// Changes Go → Back and Forward's history as `step` says, when the place it opens has been
    /// looked up.
    fn step_history(&mut self, step: HistoryStep) {
        let here = self.back_step();
        let folder = &mut self.folder;
        match step {
            HistoryStep::None => {}
            HistoryStep::New => {
                folder.back.push(here);
                // The history does not grow without end, because a slideshow of folders, or a
                // script's --remote open, visits many.
                if folder.back.len() > 200 {
                    folder.back.remove(0);
                }
                folder.forward.clear();
            }
            HistoryStep::Back if folder.back.pop().is_some() => folder.forward.push(here),
            HistoryStep::Forward if folder.forward.pop().is_some() => folder.back.push(here),
            HistoryStep::Back | HistoryStep::Forward => {}
        }
    }

    /// Opens the folder `dir`, by its real path, with `file`, of the type `file_type`, shown
    /// when there is one.
    fn open_folder_found(
        &mut self,
        ctx: &egui::Context,
        dir: PathBuf,
        file: Option<PathBuf>,
        file_type: Types,
    ) {
        // A file of the folder shown is only gone to. The folder is not read again, and its
        // thumbnails and EXIF index stay, which matters in a folder of 500,000 images.
        if let Some(i) = (file.as_deref()).and_then(|f| self.listed_here(&dir, f)) {
            return self.show_file(ctx, i);
        }
        self.leave_image(ctx);
        self.folder.collection = None;
        self.folder.folders.clear();
        // A given file is shown at once. The folder is listed in the background, and the list
        // is merged in afterwards.
        let shown = file
            .as_deref()
            .and_then(Path::file_name)
            .map(OsString::from);
        self.forget_old_list();
        // A file of a type the menu does not list (a RAW with RAW off) is listed with the
        // folder it opens. A folder or several folders opened later go by the menu again.
        self.folder.opened_type = file_type;
        // In another folder, a file of the same name is another image. It opens as one, not at
        // the zoom and place of the image before.
        if dir != self.folder.dir {
            self.list.forget_files();
        }
        self.folder.recursive = self.config.show_subfolders;
        self.folder.dir = dir.clone();
        self.remember_recent(&dir);
        let files = shown.iter().map(|name| (name.clone(), None)).collect();
        self.replace_files(ctx, files, shown);
        self.folder.lists_tree_in_parts = true;
        self.start_listing(ctx);
        self.start_watching(ctx);
        self.update_window_title(ctx);
    }

    /// Puts `path` first in File → Open Recent, also in the file the other windows read when
    /// they start. The change is merged with what they wrote meanwhile.
    pub(crate) fn remember_recent(&mut self, path: &Path) {
        if !self.config.keep_recent || self.folder.recent.first().is_some_and(|p| p == path) {
            return;
        }
        self.folder.recent = config::add_recent(&self.folder.recent, path);
        let path = path.to_path_buf();
        edit_recent(move |all| config::add_recent(&all, &path));
    }

    /// Takes `path` out of Open Recent, in its file too, so other windows and the next start
    /// do not list it either. It is taken out also when Open Recent is not kept, because an
    /// entry from before that setting would otherwise stay in the file.
    pub(crate) fn forget_recent(&mut self, path: &Path) {
        self.folder.recent.retain(|p| p != path);
        let path = path.to_path_buf();
        edit_recent(move |mut all| {
            all.retain(|p| *p != path);
            all
        });
    }

    /// Open Recent emptied, in its file too.
    pub(crate) fn clear_recent(&mut self) {
        self.folder.recent.clear();
        if let Some(file) = config::recent_path() {
            crate::files::file_io::write_later(move || crate::files::file_io::remove_shared(&file));
        }
    }

    /// File → Open Recent, with the folders and collections opened last, and Clear Recent.
    pub(crate) fn recent_menu(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        scroll_menu(
            ui,
            with_icon(ui, theme::icon::RECENT, tr("Open Recent")),
            |ui| {
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                if self.folder.recent.is_empty() {
                    ui.weak(tr("Nothing opened yet"));
                    return;
                }
                let mut chosen = None;
                for path in &self.folder.recent {
                    let text = escaped_name(path);
                    if crate::ui::menus::button(ui, &text).clicked() {
                        chosen = Some(path.clone());
                    }
                }
                ui.separator();
                if crate::ui::menus::button(ui, tr("Clear Recent")).clicked() {
                    self.clear_recent();
                }
                // When the path is gone since (deleted, or a stick taken out), a notice says so,
                // and it leaves the list.
                if let Some(path) = chosen {
                    let gone =
                        |p: &Path| Failure::from(tf!("{name} is gone", name = escaped_name(p)));
                    self.visit_if_there(&ctx, path, gone);
                }
            },
        );
    }

    /// What belongs to the list shown before, which a new folder, collection or set of folders
    /// starts without.
    pub(crate) fn forget_old_list(&mut self) {
        // A check still running would find names of the old list, so it is stopped and its
        // window closed.
        if let Some(Some(check)) = self.check_images.take() {
            check.stop();
        }
        self.folder.opened_type = Types::NONE;
        // The old folder's watcher ends. A folder opened next starts its own (watch()).
        self.folder
            .watcher_generation
            .fetch_add(1, Ordering::Relaxed);
        self.broken_files.clear();
        self.folder.subdirs.clear();
        self.folder.told_left_out.clear();
        self.file_operations.forget_deleting();
        self.slideshow.forget_names();
        // A sort asked of the old list's listing is not asked of the new one. A sort carried
        // over would put another collection in order and rewrite its file.
        self.folder.sort_collection = false;
        self.thumbs.forget_all();
        self.status_bar.forget_sizes_and_dates_asked();
        self.search.forget_exif_index();
        // The Pan view's dates are by name in the old folder.
        if let Some(pan) = &mut self.pan_view {
            pan.forget_dates();
        }
        // A list's entries not yet checked are the old list's. A folder opened now is listed
        // whole, and open_collection marks its own entries again. An edit that waits for the
        // old list to be checked is dropped and told. Sent once the new list is checked, it
        // went by the new list's names and could write over a newer edit of the same file.
        self.drop_sidecar_writes_waiting_for_the_list();
        self.list.set_unchecked_entries(false);
    }

    /// Shows the collection in `file` as the list, with the entries `files` that `look_up` read
    /// from it.
    fn open_collection(&mut self, ctx: &egui::Context, file: PathBuf, files: Vec<OsString>) {
        self.leave_image(ctx);
        let root = Path::new("/");
        // When the same collection is read again (F5), a delete still running keeps its names,
        // so a failed delete brings its images back.
        let deleting = if self.folder.collection.as_ref() == Some(&file) {
            self.file_operations.take_deleting()
        } else {
            HashSet::new()
        };
        self.forget_old_list();
        self.file_operations.keep_deleting(deleting);
        self.folder.recursive = false;
        self.folder.dir = root.to_path_buf();
        self.remember_recent(&file);
        collections::start_window_list(self.folder.number, &file);
        self.folder.collection = Some(file);
        self.folder.folders.clear();
        let first = files.first().cloned();
        let files = files.into_iter().map(|f| (f, None)).collect();
        self.replace_files(ctx, files, first);
        self.list.set_unchecked_entries(true);
        self.start_listing(ctx);
        self.start_watching(ctx);
        self.update_window_title(ctx);
    }

    /// Shows the images of several folders as one list, for a slideshow across them, once they
    /// are looked up in the background. One folder is opened as usual. A file stands for its
    /// folder.
    pub(crate) fn open_folders(&mut self, ctx: &egui::Context, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        let opening = Opening {
            paths,
            history: HistoryStep::None,
            if_gone: IfGone::OpenItsFolder,
        };
        self.look_up(ctx, opening);
    }

    /// Shows the images of `folders`, each by its real path, as one list.
    fn open_folders_found(&mut self, ctx: &egui::Context, folders: Vec<PathBuf>) {
        self.folder.place_before = self.folder.place_to_remember().map(Path::to_path_buf);
        self.leave_image(ctx);
        self.folder.collection = None;
        // As when a folder is opened, the subfolders come with it when the menu says so,
        // whatever was shown.
        self.folder.recursive = self.config.show_subfolders;
        self.forget_old_list();
        self.folder.dir = PathBuf::from("/");
        self.folder.folders = folders;
        self.replace_files(ctx, Vec::new(), None);
        self.start_listing(ctx);
        self.start_watching(ctx);
        self.update_window_title(ctx);
    }

    /// Where Back comes back to. It is the collection shown, and otherwise the file shown or
    /// the folder.
    pub(crate) fn back_step(&self) -> PathBuf {
        self.folder
            .collection
            .clone()
            .unwrap_or_else(|| self.location())
    }

    /// Opens `path` as a new step in the Back/Forward history.
    pub(crate) fn visit(&mut self, ctx: &egui::Context, path: PathBuf) {
        // A message about the folder before (a share that could not be mounted) goes with it.
        self.notice.clear();
        let opening = Opening {
            paths: vec![path],
            history: HistoryStep::New,
            if_gone: IfGone::OpenItsFolder,
        };
        self.look_up(ctx, opening);
    }

    /// Opens `path` as `visit` does when it is there. When it is not, the notice `gone` makes
    /// says so, and the path leaves Open Recent.
    pub(crate) fn visit_if_there(
        &mut self,
        ctx: &egui::Context,
        path: PathBuf,
        gone: fn(&Path) -> Failure,
    ) {
        self.notice.clear();
        let opening = Opening {
            paths: vec![path],
            history: HistoryStep::New,
            if_gone: IfGone::Tell(gone),
        };
        self.look_up(ctx, opening);
    }

    /// The file shown, or the folder when it has no images.
    pub(crate) fn location(&self) -> PathBuf {
        match self.list.files().get(self.list.shown()) {
            Some(name) => self.folder.dir.join(name),
            None => self.folder.dir.clone(),
        }
    }

    /// Open a network location such as smb://server/share/folder, mounting it in the background.
    pub(crate) fn mount(&mut self, ctx: &egui::Context, uri: String) {
        // A password typed into the address is gio's answer to its question, not a part of its
        // command line, which other users can read.
        let (uri, password) = external::without_password(&uri);
        let answer = password.is_some();
        let login = Login {
            uri,
            password: password.unwrap_or_default(),
            ..Login::default()
        };
        self.mount_as(ctx, login, answer);
    }

    /// Mount `login.uri`, answering gio's questions from `login` if `answer`.
    pub(crate) fn mount_as(&mut self, ctx: &egui::Context, login: Login, answer: bool) {
        // This says which address is tried, and replaces what an attempt before left.
        self.notice
            .give(tf!("Connecting to {uri}…", uri = login.uri));
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        // Its busy pointer: `is_mounting`, while `mounting_rx` waits for this answer.
        std::thread::spawn(move || {
            let result = gio_mount(&login.uri, answer.then_some(&login));
            let _ = tx.send((result, login));
            ctx.request_repaint();
        });
        self.folder.mounting_rx = Receiving::of(rx);
    }

    /// Reads the folder (and then the sizes) in the background, and `receive_listing` merges it
    /// in. A collection is checked for files that are gone instead.
    pub(crate) fn start_listing(&mut self, ctx: &egui::Context) {
        let (tx, rx) = mpsc::channel();
        let (size_tx, size_rx) = mpsc::channel();
        let (list_dir, ctx2, types) = (self.folder.dir.clone(), ctx.clone(), self.shown_types());
        let c = &self.config;
        let (by, ascending, case) = (c.sort, c.sort_ascending, c.sort_case_sensitive);
        let recursive = self.folder.recursive;
        // A subfolder read again, which only `relist_changes` asks for.
        let from = std::mem::take(&mut self.folder.next_listing_from);
        self.folder.listing_from = from.clone();
        let collection = self
            .folder
            .collection
            .is_some()
            .then(|| self.list.files().to_vec());
        // Every move writes a collection's file at once, so reading it again gives what moved
        // after the list was copied. A plain list is never written, and is read with this
        // window's own changes laid over it (`collections::load_with_window_changes`). The
        // window's changes to a list it showed before go.
        let saved = self.folder.collection.clone();
        collections::keep_window_list_of(self.folder.number, saved.as_deref());
        // A collection keeps its own order unless the Sort menu was used.
        let sort = collection.is_none() || self.folder.sort_collection;
        let folders = self.folder.folders.clone();
        // A folder read again in EXIF date order (mostly after Glimt's own sidecar writes) keeps
        // the order shown when no name is new, instead of reading every file's EXIF again.
        // shortcut: the list copied in the UI thread, 7-9 ms for 500,000 names, once per
        // refresh; share it with the thread (an Arc in List) if folders grow far past that.
        let exif = matches!(
            by,
            listing::Sort::ExifOriginal | listing::Sort::ExifDigitized
        );
        let same = Some((
            self.folder.dir.clone(),
            types,
            by,
            ascending,
            case,
            recursive,
        ));
        let before = (exif && !self.folder.lists_tree_in_parts && collection.is_none())
            .then_some(())
            .filter(|_| folders.is_empty() && self.folder.listed_as == same)
            .map(|_| self.list.files().to_vec());
        // The listing before stops, if it is still reading a tree.
        self.folder.listing_stop.store(true, Ordering::Relaxed);
        self.folder.listing_stop = Arc::default();
        let stop = self.folder.listing_stop.clone();
        let (subdirs_tx, subdirs_rx) = mpsc::channel();
        self.folder.subdirs_rx = Receiving::of(subdirs_rx);
        // The channel has room for one part. A window that is not drawn (minimized, or on
        // another workspace) takes none, and a copy of the whole list so far each time would
        // pile up.
        let (part_tx, part_rx) = mpsc::sync_channel(1);
        self.folder.part_rx = Receiving::of(part_rx);
        self.folder.part_taken_in = Arc::default();
        // Every listing of a tree sends parts, of one folder or of several, whatever started
        // it: a folder opened, the slideshow, which plays the images as they are found, a
        // refresh, the watcher, a rename, a change of the Types or of the order. Each part holds
        // only the images new to the list, and the names gone from the folders it read, so a
        // list read again does not shrink and grow. The parts go into the list in the listing's
        // order (`take_listing`). After another order is chosen, the first part brings the
        // list's own rows put in the new order in the listing's thread (`PartsFrom`), so the
        // list is in the new order from the first part on and grows in it. Before, a listing
        // after another order was chosen and one of several folders sent no parts, and the list
        // stood still until the whole tree was read, which on a share is minutes.
        self.folder.lists_tree_in_parts = false;
        self.folder.listing_sends_parts = recursive && collection.is_none();
        let parts = self.parts_from((by, ascending, case), stop.clone());
        self.folder.first_part_shown = None;
        self.folder.list_is_a_part = false;
        let listing = Listing {
            dir: list_dir,
            types,
            by,
            ascending,
            case,
            recursive,
            from: from.clone(),
            stop,
            awaited: self.decoding.images_awaited().clone(),
            parts,
            part_taken_in: self.folder.part_taken_in.clone(),
            tx,
            size_tx,
            ctx: ctx2,
            started: Instant::now(),
        };
        // Its busy pointer: `is_listing`, while `listing_rx` waits for this answer.
        std::thread::spawn(move || {
            if !folders.is_empty() {
                listing.folders(&folders, part_tx);
            } else if let Some(files) = collection {
                listing.collection(files, saved, sort);
            } else {
                listing.folder(before, part_tx, subdirs_tx);
            }
        });
        self.folder.listing_rx = Receiving::of(rx);
        self.folder.listing_version = self.list.version();
        // A subfolder read again leaves the folder's size as it is.
        if from.as_os_str().is_empty() {
            self.status_bar.sum_folder(size_rx);
        }
        self.folder.relist_when_done = false;
        // This listing reads every change seen before it.
        self.folder.change_waits = false;
        self.folder.changes_seen = ChangesSeen::Nothing;
        self.folder.relist_whole = false;
    }

    /// Reads the folder again after the folder watcher saw it change: only the subfolder of the
    /// tree that holds every change seen (`Folder::subfolder_changed`), when that is enough,
    /// and the whole folder or tree otherwise. A change in the open folder itself, or one the
    /// watcher cannot place below it, reads the whole tree. Before, a change in any one
    /// subfolder read the whole tree again, which on a share takes minutes.
    pub(crate) fn relist_changes(&mut self, ctx: &egui::Context) {
        let listed_whole_as = self.tree_listed_as();
        self.folder.next_listing_from = (self.folder)
            .subfolder_changed(&listed_whole_as)
            .unwrap_or_default();
        self.start_listing(ctx);
    }

    /// Returns what a whole listing of the open folder's tree is read and sorted as now
    /// (`ListedAs`), with the types shown and the order chosen.
    pub(crate) fn tree_listed_as(&self) -> ListedAs {
        let c = &self.config;
        let (dir, types) = (self.folder.dir.clone(), self.shown_types());
        (
            dir,
            types,
            c.sort,
            c.sort_ascending,
            c.sort_case_sensitive,
            true,
        )
    }

    /// Returns whether the list is the whole tree of the open folder as `tree_listed_as` gives
    /// it now, read whole by a listing that is done, and nothing asked for it to be read whole
    /// again. A slideshow of the subfolders then plays it without reading the tree again. A
    /// change that the watcher saw is read afterwards as it is otherwise (`relist_changes`).
    pub(crate) fn lists_the_tree_whole(&self) -> bool {
        let f = &self.folder;
        let listed = f.listed_as == Some(self.tree_listed_as());
        f.recursive && !f.is_listing() && !f.relist_whole && listed
    }

    /// Returns what the parts of the listing that starts, in `order`, are made from, with its
    /// stop flag `stop`. A listing that sends parts gets the hashes of the list's names, which
    /// its parts leave out, and when the list is in another order than `order`, the list's
    /// names to put in it in the listing's thread: a list made in another way than by a
    /// listing (the one image opened while its folder is read, or a slideshow of the
    /// subfolders started before the folder was read), or one listed in another order before
    /// another was chosen. Before, the parts of such a list went in by the order before, or by
    /// keys the list lacked or had of another order, and landed in the wrong places.
    fn parts_from(&self, order: (listing::Sort, bool, bool), stop: Arc<AtomicBool>) -> PartsFrom {
        let sends_parts = self.folder.listing_sends_parts;
        // shortcut: the list's names hashed in the UI thread, about 10 ms and 4 MB for 500,000
        // names, once for each listing of a tree that sends parts; share the names with the
        // thread (an Arc in List) if folders grow far past that.
        let listed_before = match sends_parts {
            true => NameHashes::of(self.list.files()),
            false => NameHashes::default(),
        };
        // shortcut: the list's names copied in the UI thread, about 10 ms for 500,000 names,
        // once for a listing of a tree after another order was chosen; share them with the
        // thread (an Arc in List) if folders grow far past that.
        let in_another_order = self.list.rows().order() != Some(order);
        let to_sort_anew = match sends_parts && in_another_order {
            true => self.list.listed_names(),
            false => Vec::new(),
        };
        let places = match self.folder.folders.is_empty() {
            true => vec![self.folder.dir.clone()],
            false => self.folder.folders.clone(),
        };
        PartsFrom {
            dir: self.folder.dir.clone(),
            places,
            recursive: self.folder.recursive,
            order,
            stop,
            awaited: self.decoding.images_awaited().clone(),
            listed_before,
            to_sort_anew,
        }
    }

    /// Polls the folder's modification time once a second in the background (one `stat`, no
    /// repaint unless it changed). Adding, removing or renaming a file changes it. For a
    /// collection, its file is watched instead, so a move that another window follows in it
    /// is read here too, and for folders shown together, each of them. Each listing of a folder
    /// then watches the folders it read (`watch_folders_read`), with a tree's subfolders. The
    /// thread ends when another folder is opened.
    pub(crate) fn start_watching(&mut self, ctx: &egui::Context) {
        let watched: Vec<PathBuf> = match (&self.folder.collection, &self.folder.folders) {
            (Some(file), _) => vec![file.clone()],
            (None, folders) if !folders.is_empty() => folders.clone(),
            (None, _) => vec![self.folder.dir.clone()],
        };
        // Their times are taken in the watcher's thread, as a share that stopped answering
        // would hold the window.
        let read = watched.into_iter().map(|path| (path, None)).collect();
        self.watch_folders_read(ctx, read, true);
    }

    /// Watches the folders `read` in a thread of its own in place of the watcher before, from
    /// the times they had when they were read, or from their times now when `times_unknown`.
    /// Every second the open folders (or the collection's file) are looked at, and of a tree's
    /// subfolders the next `SUBFOLDERS_A_SECOND`, or on a share as many as fit in
    /// `SHARE_LOOKS_A_SECOND`, each through the gate (`listing::ReadsFirst`). The folders that
    /// changed in a second go in one message. A change in subfolders of a tree reads only the
    /// subfolder that holds them again (`relist_changes`), and that listing waits for the
    /// images awaited as every walk does. The folders read are kept (`Folder`'s `watched`).
    fn watch_folders_read(
        &mut self,
        ctx: &egui::Context,
        mut read: FoldersRead,
        times_unknown: bool,
    ) {
        if !times_unknown {
            self.folder.watched = read.clone();
        }
        let generation = self
            .folder
            .watcher_generation
            .fetch_add(1, Ordering::Relaxed)
            + 1;
        let mut open_paths = self.folder.folders.clone();
        open_paths.extend(self.folder.collection.clone());
        open_paths.push(self.folder.dir.clone());
        // The share is asked about for the places the list shows, never for the "/" of several
        // folders shown together, whose mounts below are every mount on the system.
        let mut places = self.folder.places();
        places.extend(self.folder.collection.clone());
        let with_subfolders = self.folder.walks_subfolders();
        let (watcher_generation, tx, ctx, awaited) = (
            self.folder.watcher_generation.clone(),
            self.folder.changed_tx.clone(),
            ctx.clone(),
            self.decoding.images_awaited().clone(),
        );
        // No busy pointer: it follows the folder's changes, and nobody waits for it.
        std::thread::spawn(move || {
            // Whether the places are on a share is asked through a gate for files anywhere, as
            // each later look goes through the gate made from its answer (#294).
            let mut asks_first = listing::ReadsFirst::for_files_anywhere(&awaited);
            let on_a_share = (places.iter()).any(|place| {
                asks_first.before_a_look(place);
                listing::is_on_a_share(place, with_subfolders)
            });
            let mut gate = listing::ReadsFirst::new(Arc::default(), awaited, on_a_share);
            // The first modification times go through the gate too, as every look after them
            // does, so that on a share the image the window waits for goes first (#294).
            if times_unknown {
                let is_wanted = || watcher_generation.load(Ordering::Relaxed) == generation;
                for (path, time) in read.iter_mut() {
                    let Some(_turn) = gate.before_work_nobody_waits_for(path, is_wanted) else {
                        break;
                    };
                    *time = modified(path);
                }
            }
            // The open folders come first, and the tree's subfolders after them. A tree of many
            // folders is put in this order here rather than in the window's thread.
            let is_open = |p: &Path| open_paths.iter().any(|o| o == p);
            read.sort_by_key(|(p, _)| !is_open(p));
            let open = read.iter().take_while(|(p, _)| is_open(p)).count();
            let mut next = 0;
            while watcher_generation.load(Ordering::Relaxed) == generation {
                std::thread::sleep(std::time::Duration::from_secs(1));
                // A program started on a file of the share reads its start first.
                if on_a_share && listing::a_program_is_starting() {
                    continue;
                }
                let is_wanted = || watcher_generation.load(Ordering::Relaxed) == generation;
                let (changed, looked) = look_this_second(
                    &mut read,
                    (open, next),
                    on_a_share,
                    (&mut gate, &is_wanted),
                    modified,
                );
                next = (next + looked) % (read.len() - open).max(1);
                if !changed.is_empty() && watcher_generation.load(Ordering::Relaxed) == generation {
                    let _ = tx.send(changed);
                    ctx.request_repaint();
                }
            }
        });
    }

    /// Take the folder listing if it is ready, waiting up to `wait`.
    pub(crate) fn receive_listing(&mut self, ctx: &egui::Context, wait: std::time::Duration) {
        // The place to open comes first, as its listing starts when it is opened.
        self.receive_looked_up(ctx, wait);
        // While Rename Many renames files of the folder shown, a listing (the watcher saw a
        // rename) waits in its channel, untaken. It may hold a name in the middle of the
        // rename, such as the new name of the shown image, or a name between two steps of a
        // swap, and the shown image would then move to another file, which the next key would
        // act on. The rename's report reads the folder again, and that listing takes this one's
        // place.
        if self.file_operations.renames_files_in(&self.folder.dir) {
            return;
        }
        // A thread that broke leaves no folders, and nothing waiting for it.
        if let Took::Answer(dirs) = self.folder.subdirs_rx.take() {
            self.folder.subdirs = dirs;
        }
        // Every part that came is taken, each of the images found since the part before.
        let began = Instant::now();
        let mut took_parts = false;
        while let Took::Answer(part) = self.folder.part_rx.next() {
            let anew = part.list_sorted_anew;
            self.take_listing(ctx, part.rows, false, (&part.read, anew));
            took_parts = true;
        }
        if took_parts {
            let took = u64::try_from(began.elapsed().as_micros()).unwrap_or(u64::MAX);
            self.folder.part_taken_in.store(took, Ordering::Relaxed);
        }
        let listed = match self.folder.listing_rx.wait(wait) {
            Took::Answer(listed) => listed,
            // When the listing thread broke, a notice says so once, and nothing (the busy
            // pointer, a parked --remote select, auto refresh) waits for it any more.
            Took::Gone => {
                self.folder.subdirs_rx.clear();
                self.folder.part_rx.clear();
                return self.tell_background_stopped(tr("Reading the folder"));
            }
            Took::Nothing => return,
        };
        // A collection's listing works from the list (or its file) as it was when it started.
        // An image moved or taken out of the list since would be put back by it, so the list
        // as it is now is read instead. This holds for every edit, because each one changes
        // the list's version.
        if self.folder.collection.is_some() && self.list.version() != self.folder.listing_version {
            return self.start_listing(ctx);
        }
        self.folder.subdirs_rx.clear();
        self.folder.part_rx.clear();
        let Listed {
            mut dirs,
            files,
            left_out,
            mut read,
            read_in,
        } = listed;
        // A folder that could not be read is told, and the list shown stays with its selection.
        // Taken as empty, a share that failed for a moment emptied the list, and the files came
        // back unselected with the next listing. A part left out (a folder of a tree or of
        // several, an entry of a collection) keeps the files of it that the list shows, with
        // their selection, and the rest is listed anew. Holding the whole listing for it froze
        // the list for good while the part stayed unreadable (a folder locked after its files
        // were shown).
        // A whole list that kept the list's order takes what each name is sorted by from the
        // list's own rows. A name of it that left the list after the listing began (a delete or
        // a move) has none there, and then the list stays as it is and the folder is read
        // again, as a delete reads it again anyway. Before, the list took such a whole list
        // without its keys.
        let rows = match files {
            Ok(ListedRows::Rows(rows)) => Some(self.with_shown_files_of_parts(rows, &left_out)),
            Ok(ListedRows::InListOrder(kept)) => self.list.rows().in_step(kept),
            // A subfolder read again ends as a whole one does (`rows_with_subfolder_read_again`).
            Ok(ListedRows::InParts(names)) => {
                (dirs, read) = self.folders_with_subfolder_read_again(read);
                self.rows_with_subfolder_read_again(names)
            }
            // A subfolder that could not be read again has the whole tree read.
            Err(_) if !self.folder.listing_from.as_os_str().is_empty() => {
                self.folder.listing_from = PathBuf::new();
                return self.folder.relist_later();
            }
            Err(unread) => return self.listing_failed(ctx, unread, read),
        };
        self.folder.listing_from = PathBuf::new();
        self.folder.whole_listings += 1;
        // Several folders shown together are listed as "/" with no subfolders, which are not
        // the tree's.
        if self.folder.collection.is_none() && self.folder.folders.is_empty() {
            self.tree
                .add_subfolders(self.folder.dir.clone(), dirs.clone());
        }
        self.folder.subdirs = dirs;
        // A collection's listing reads no folder, and its file stays watched.
        if !read.is_empty() {
            self.watch_folders_read(ctx, read, false);
        }
        let listed_from = Instant::now();
        match rows {
            Some(rows) => self.take_listing(ctx, rows, true, (&ReadWhole::default(), None)),
            None => self.folder.relist_later(),
        }
        // The time taken counts the list made from it in this thread too.
        let took = read_in + listed_from.elapsed();
        self.folder.rest_until = Instant::now() + took * REST_AFTER_LISTING;
        self.list.set_unchecked_entries(false);
        self.settle_sidecar_writes_waiting_for_the_list(ctx);
        let types = self.shown_types();
        let c = &self.config;
        self.folder.listed_as =
            (self.folder.collection.is_none() && self.folder.folders.is_empty()).then(|| {
                (
                    self.folder.dir.clone(),
                    types,
                    c.sort,
                    c.sort_ascending,
                    c.sort_case_sensitive,
                    self.folder.recursive,
                )
            });
        self.tell_left_out(left_out);
    }

    /// Returns the list's rows, whose parts brought the names new in the subfolder read again,
    /// with the identities that its listing found, `names`, given to the names that stayed. The
    /// other names keep theirs. This is the whole list of the tree as it is now, each row with
    /// what it is sorted by (`Rows::in_step`), which gives `None` only for names out of step.
    /// A subfolder read again then ends as a whole listing does, so that what the last step of
    /// a whole listing does is done for it too: a name that another file has taken keeps
    /// nothing of the file it held, the sizes and dates are read again, the preload is refilled
    /// and the list is noted as listed whole. Before, its end kept the identities of the names
    /// that stayed, and the decodes of a file saved over under its own name were dropped as
    /// another file's until F5.
    fn rows_with_subfolder_read_again(&self, names: Vec<ListedName>) -> Option<Rows> {
        let found: HashMap<OsString, crate::files::file_io::Identity> = names.into_iter().collect();
        let mut rows = self.list.rows().listed_names();
        for (name, identity) in &mut rows {
            if let Some(&now) = found.get(name) {
                *identity = now;
            }
        }
        self.list.rows().in_step(rows)
    }

    /// Returns the open folder's own folders, which a subfolder read again leaves as they are,
    /// and the folders watched with those of the subfolder `listing_from` replaced by the
    /// folders that its listing `read`.
    fn folders_with_subfolder_read_again(&self, read: FoldersRead) -> (Vec<PathBuf>, FoldersRead) {
        let from = self.folder.dir.join(&self.folder.listing_from);
        let mut watched = self.folder.watched.clone();
        watched.retain(|(path, _)| !path.starts_with(&from));
        watched.extend(read);
        (self.folder.subdirs.clone(), watched)
    }

    /// Tells that the folder could not be read, `unread`, and keeps the list shown, with the
    /// folders `read` watched, so that a change lists it again.
    fn listing_failed(&mut self, ctx: &egui::Context, unread: Unread, read: FoldersRead) {
        let (path, e) = unread;
        let heading = tf!("Could not open {name}", name = escaped_name(path));
        self.notice
            .give_error(Failure::because(heading, e.to_string()));
        if !read.is_empty() {
            self.watch_folders_read(ctx, read, false);
        }
    }

    /// Adds to a listing's `files` the files of the list shown that lie in a part `left_out`
    /// of the listing: in a folder left out, or an entry of a collection left out. Each one
    /// goes after the file it followed in the list shown, and keeps its identity, so an action
    /// on it still checks that it is the file listed. In an order that looks at the files, each
    /// one has the key of the file it follows (`Rows::with_rows_put_in`), so the keys stay in
    /// their order for the parts of the next listing. Returns `rows` as they are when the list
    /// shows none of them, or holds only a collection's entries that no listing has checked
    /// yet.
    fn with_shown_files_of_parts(&self, rows: Rows, left_out: &[Unread]) -> Rows {
        if left_out.is_empty() || self.list.has_unchecked_entries() {
            return rows;
        }
        let parts: HashSet<&[u8]> = (left_out.iter())
            .filter_map(|(path, _)| {
                let part = path.strip_prefix(&self.folder.dir).ok()?;
                Some(part.as_os_str().as_encoded_bytes())
            })
            .collect();
        // Each name is looked up whole, and up to each "/" in it, as a hash lookup each. That
        // is a few lookups per file once per listing that left a part out, and no system call.
        let is_in_a_part = |name: &OsString| {
            let name = name.as_encoded_bytes();
            let slashes = (name.iter().enumerate()).filter(|(_, b)| **b == b'/');
            (slashes.map(|(end, _)| end).chain([name.len()]))
                .any(|end| parts.contains(&name[..end]))
        };
        let place: HashMap<&OsString, usize> = (rows.files().iter().enumerate())
            .map(|(i, f)| (f, i))
            .collect();
        // The files kept, by the number of the listing's files before them.
        let mut kept: HashMap<usize, Vec<ListedName>> = HashMap::new();
        let mut listed_before = 0;
        for (i, name) in self.list.files().iter().enumerate() {
            match place.get(name) {
                Some(&at) => listed_before = at + 1,
                None if is_in_a_part(name) => {
                    let file = (name.clone(), self.list.identity(i));
                    kept.entry(listed_before).or_default().push(file);
                }
                None => {}
            }
        }
        rows.with_rows_put_in(kept)
    }

    /// Tells the parts `left_out` of a listing that could not be read, each the first time it
    /// is left out since the place was opened or F5 read it again, so a folder watched does not
    /// tell a locked subfolder at every change.
    fn tell_left_out(&mut self, left_out: Vec<Unread>) {
        let told = &mut self.folder.told_left_out;
        let new: Vec<Unread> = (left_out.into_iter())
            .filter(|(path, _)| told.insert(path.clone()))
            .collect();
        let Some((path, e)) = new.first() else {
            return;
        };
        let mut cause = e.to_string();
        if new.len() > 1 {
            let n = new.len() - 1;
            cause += &tf!(", and {n} more", n);
        }
        let heading = tf!("Could not open {name}", name = escaped_name(path));
        self.notice.give_error(Failure::because(heading, cause));
    }

    /// Takes the images of the folder read in the background as the list, as `take_listing`
    /// does, from a listing that read no folder whole and looked at no file, as the tests give
    /// them, in the order of the settings, an order by the names.
    #[cfg(test)]
    pub(crate) fn take_listed_files(
        &mut self,
        ctx: &egui::Context,
        files: Vec<ListedName>,
        whole: bool,
    ) {
        let c = &self.config;
        let order = (c.sort, c.sort_ascending, c.sort_case_sensitive);
        assert!(!order.0.looks_at_files(), "a test's list in {order:?}");
        let rows = Rows::sorted(files, order, |_, _| SortKey::Name);
        self.take_listing(ctx, rows, whole, (&ReadWhole::default(), None));
    }

    /// Takes the images of the folder read in the background as the list: all of them when
    /// `whole`, and otherwise a part of a tree, the images new to the list, which go into it in
    /// its order and leave out the names that the folders it `read` whole no longer hold. In an
    /// order that looks at the files, each row comes with what it is sorted by, and a part goes
    /// in by them, among the keys of the list's rows, with no look at a file. The first part of a
    /// listing that began while the list was in another order brings the list's rows put in
    /// the listing's order (`list_sorted_anew`), which the list takes first. The shown image and
    /// the selection stay. The list keeps the order the listing gives it, also while a
    /// slideshow runs, which keeps its own state by the names (`Slideshow`).
    fn take_listing(
        &mut self,
        ctx: &egui::Context,
        rows: Rows,
        whole: bool,
        (read, list_sorted_anew): (&ReadWhole, Option<Rows>),
    ) {
        let shown_before = self.list.shown();
        // Where the names of the list went, for a part merged into it, which carries what is
        // held by place through it with no look at a name.
        let mut moved = None;
        // A part goes into the list in the list's order, so that the tree's images are shown
        // while the tree is read, which on a share can take minutes (#256), and the whole list
        // is taken as it came, in the listing's order.
        let mut rows = match whole {
            true => rows,
            false => match self.merged_with_part(rows, read, list_sorted_anew) {
                Some((rows, places)) => {
                    moved = places;
                    rows
                }
                // A part that cannot go into the list in the list's order waits for another
                // listing, which puts the list in the listing's order first. Before, such a part
                // was left out in silence, and so was every later part of the walk.
                None => return self.folder.relist_later(),
            },
        };
        // Files still being deleted in the background must not come back with a refresh.
        let listed = rows.len();
        self.file_operations.leave_out_deleting(&mut rows);
        if rows.len() != listed {
            moved = None;
        }
        // The sizes and dates of the rows go from one part of the tree to the next, and to the
        // whole list after it, as the files are the same ones, read by the same walk. Before,
        // each part read them again, and an answer still on its way was dropped and asked
        // again, which on a slow share kept the list busy until the tree was read.
        let after_a_part = std::mem::replace(&mut self.folder.list_is_a_part, !whole);
        // When nothing changed (most refreshes, after Glimt's own writes beside the files), the
        // list, the selection and the search stay as they are, and the list takes the rows'
        // order. A name that another file has taken since is a change, as what was read of the
        // file it held is that file's.
        if !self.folder.sort_collection {
            match self.list.take_order_of(rows) {
                Ok(()) => {
                    // A file may have been saved over under its own name (an editor writes and
                    // renames), so its size and date are read again, for the rows on screen
                    // only. A tree's whole list that is its last part keeps them, as the same
                    // walk read them.
                    if !after_a_part {
                        self.list.forget_sizes_and_dates();
                    }
                    // A rename keeps the list as it is but forgot the file's decoded image,
                    // which is asked for again here.
                    self.refill_preload();
                    return;
                }
                Err(other) => rows = other,
            }
        }
        // Glimt went to an image by itself, the first of a tree's first images. The whole list
        // starts at its first image instead, unless the user chose another one meanwhile.
        let mut shown = self.list.files().get(self.list.shown()).cloned();
        let plays = self.slideshow.is_running();
        if whole
            && shown.is_some()
            && std::mem::take(&mut self.folder.first_part_shown) == shown
            && !plays
        {
            shown = None;
        }
        // An image shown in a slideshow that is gone gives its place to the next one of the
        // list that stays, and not to the first image.
        if plays {
            shown = shown.map(|name| self.next_name_that_stays(name, shown_before, &rows));
        }
        let first = self.list.files().is_empty() && !whole;
        // A refresh keeps the selection, so Delete or Move after it still acts on what was
        // picked. The selected files that the search hides stay selected too.
        let picked = (self.list).names_with_identities(&self.list.selected_with_hidden());
        let read = after_a_part.then(|| self.list.take_sizes_and_dates());
        self.replace_list(ctx, rows, shown, moved);
        if let Some(read) = read {
            self.list.keep_sizes_and_dates(read);
        }
        if first {
            self.folder.first_part_shown = self.list.files().get(self.list.shown()).cloned();
        }
        self.reselect(ctx, &picked);
        if whole && std::mem::take(&mut self.folder.sort_collection) {
            self.save_collection_order();
        }
    }

    /// Returns the list with the part `rows` of a tree merged into it, in the list's order, and
    /// where the list's rows went when they stayed in their order. The first part of a listing
    /// that began while the list was in another order than the listing's brings the list's rows
    /// as the listing began, put in the listing's order (`list_sorted_anew`). Those of them that
    /// the list still holds then take the list's place, and the part goes into them. Returns
    /// `None` when the part is in another order than the list, or when the list holds a name
    /// that the rows put in order lack.
    fn merged_with_part(
        &self,
        rows: Rows,
        read: &ReadWhole,
        list_sorted_anew: Option<Rows>,
    ) -> Option<(Rows, Option<listing::PlacesMoved>)> {
        let Some(mut anew) = list_sorted_anew else {
            let (merged, places) = self.list.rows().merged(rows, read).ok()?;
            return Some((merged, Some(places)));
        };
        // The list changed since the listing began (a delete or a move took a name out), so
        // the rows put in order keep only the names that it still holds.
        if self.list.version() != self.folder.listing_version {
            let holds: HashSet<&OsString> = self.list.files().iter().collect();
            anew.retain(|name| holds.contains(name));
            if anew.len() != holds.len() {
                return None;
            }
        }
        Some((anew.merged_into(rows, read).ok()?, None))
    }

    /// Returns `shown`, the name of the image shown at place `at`, when `rows`, the list merged
    /// from a listing, still holds it. Returns otherwise the first name after it in the list
    /// shown that `rows` holds, or the last one before it, or `shown` when `rows` holds none of
    /// them. The names are looked up in a set only when the shown one is gone.
    fn next_name_that_stays(&self, shown: OsString, at: usize, rows: &Rows) -> OsString {
        if rows.files().contains(&shown) {
            return shown;
        }
        let stays: HashSet<&OsString> = rows.files().iter().collect();
        let old = self.list.files();
        let following = old.iter().skip(at + 1).find(|f| stays.contains(f));
        let preceding = || {
            old[..at.min(old.len())]
                .iter()
                .rev()
                .find(|f| stays.contains(f))
        };
        following.or_else(preceding).cloned().unwrap_or(shown)
    }

    /// Gives files in the list new names, for a file moved or renamed. A file whose new name
    /// keeps its place in the list's order keeps its place. Otherwise the list is put in its
    /// order again by the rows' own comparison (`Rows::rename`), with no look at a file, and the
    /// image shown, the selection and what else is held by place follow the files by name. The
    /// list's version goes up and the search runs again, so the matches follow the new names.
    /// The listing that follows finds the same names and would change nothing.
    pub(crate) fn rename_in_list(&mut self, ctx: &egui::Context, names: Vec<(usize, OsString)>) {
        if names.is_empty() {
            return;
        }
        let renames: Vec<(OsString, OsString)> = (names.iter())
            .filter_map(|(i, name)| Some((self.list.files().get(*i)?.clone(), name.clone())))
            .collect();
        let in_order = self.list.rename(&names);
        self.list.note_names_changed();
        self.slideshow.follow_renames(&renames);
        if let Some(rows) = in_order {
            let shown = self.list.files().get(self.list.shown()).cloned();
            let selected = self.list.selected_with_hidden();
            let selected = self.list.names_with_identities(&selected);
            // The list is still the one listed, in its sort order, so a listing that follows
            // may keep that order as before.
            let listed_as = self.folder.listed_as.take();
            self.replace_list(ctx, rows, shown, None);
            self.folder.listed_as = listed_as;
            self.reselect(ctx, &selected);
            return;
        }
        self.update_search(ctx);
        self.follow_list_in_slideshow();
    }

    /// Returns the place of `file` in the list when its folder, `dir` by its real path, is the
    /// folder shown, and the list is that folder's as it is listed now: not a collection or
    /// several folders, read whole, with subfolders as the setting says. Returns `None`
    /// otherwise.
    fn listed_here(&self, dir: &Path, file: &Path) -> Option<usize> {
        let folder = &self.folder;
        let as_listed = folder.collection.is_none()
            && folder.folders.is_empty()
            && !folder.is_listing()
            && folder.recursive == self.config.show_subfolders;
        if !as_listed || dir != folder.dir {
            return None;
        }
        let name = file.file_name()?;
        self.list.files().iter().position(|f| f == name)
    }

    /// Lets go of what holds places in the list, after files were taken out of it and its
    /// places shifted. This is the one place for it. (A list read again gets a new `List`
    /// instead.) It covers the selection (told to the status bar), the anchor, a rectangle or
    /// a collection drag, and the decodes in flight. Those have old places, so they are
    /// rejected by path when they arrive, and `refill_preload` asks for them again.
    pub(crate) fn list_changed(&mut self, ctx: &egui::Context) {
        self.list.forget_places();
        self.selection_changed(ctx);
    }

    /// Selects again by name, after the list was read again or put in another order, so a
    /// Delete or Move after it still acts on what was picked. A name that another file has
    /// taken since is not selected, because the user picked the file it held, not the one
    /// that holds it now (another window's Rename Many gave the name to the next photo).
    pub(crate) fn reselect(
        &mut self,
        ctx: &egui::Context,
        names: &HashMap<OsString, crate::files::file_io::Identity>,
    ) {
        if names.is_empty() {
            return;
        }
        let places = (self.list.files().iter().enumerate())
            .filter(|&(i, f)| {
                (names.get(f)).is_some_and(|&old| !is_another_file(old, self.list.identity(i)))
            })
            .map(|(i, _)| i)
            .collect();
        self.set_selection(ctx, places);
    }

    /// Replaces the file list and goes to `shown` (otherwise the first image). Textures are
    /// kept for the files they were made from, at their new places, by name and identity, and so
    /// are the decodes still being made. All other state based on places is reset. When the
    /// same file is still shown (a refresh, or the list put in another order), it keeps its
    /// zoom, turn and place. A name that another file has taken since (another
    /// window's Rename Many swapped two names) keeps nothing of the file it held.
    pub(crate) fn replace_files(
        &mut self,
        ctx: &egui::Context,
        files: Vec<ListedName>,
        shown: Option<OsString>,
    ) {
        self.replace_list(ctx, Rows::unsorted(files), shown, None);
    }

    /// Replaces the file list with `rows` as `replace_files` does. `moved` says where the names
    /// of the list went when a part of a tree was merged into it (`Rows::merged`). What is held
    /// by place then goes to its new place through it, with no look at a name, and no name can
    /// have been taken by another file, as the part brought only names the list lacked. Without
    /// it, what is held by place goes by name.
    fn replace_list(
        &mut self,
        ctx: &egui::Context,
        rows: Rows,
        shown: Option<OsString>,
        moved: Option<listing::PlacesMoved>,
    ) {
        // The list may have another order (a collection's), so only a listing in sort order sets
        // this again.
        self.folder.listed_as = None;
        let (old_shown, later_page) = (self.list.shown(), self.viewing.page() > 0);
        let shown_stays = shown.is_some() && self.list.files().get(old_shown) == shown.as_ref();
        // The file counts as found when it is shown now and is in the new list too. It does not
        // when another program removed or renamed it, because its turn not saved would
        // otherwise go to the image shown in its place.
        let new = rows.files();
        let found = match shown_stays {
            true => places_in_new_list(self.list.files(), new, &[old_shown], moved.as_ref())[0],
            false => (shown.as_ref()).and_then(|p| new.iter().position(|f| f == p)),
        };
        let still = shown_stays
            && found
                .is_some_and(|k| !is_another_file(rows.identity(k), self.list.identity(old_shown)));
        // The decodes still being made are kept, as the decoded images below, so no file is
        // decoded twice while the parts of a tree come in. The shown file's decodes of a later
        // page are not kept, because the file is shown again from its first page.
        let mut decodes: Vec<_> = (self.list.take_decodes().into_iter())
            .filter(|((i, _), _)| !later_page || *i != old_shown)
            .collect();
        if !still {
            self.leave_image(ctx);
        }
        // The decoded images are kept too, so the neighbours read ahead stay, after a refresh as
        // after a delete.
        let loaded_images: Vec<(usize, LoadedImage)> =
            self.list.take_loaded_images().into_iter().collect();
        // The panes are kept too, because their places belong to other files in the new list,
        // but their files do not change. So is the image that a slideshow of one pane at a time
        // showed last, because it goes on from there, while the panes it keeps stay as they are.
        let panes = self.list.panes().to_vec();
        // A name that another file has taken since keeps nothing that was read of the file it
        // held: its thumbnail, size and date, its part of the exif: index and its Pan view date.
        let taken = match moved {
            Some(_) => Vec::new(),
            None => self.list.names_another_file_took(&rows),
        };
        if !taken.is_empty() {
            let taken: HashSet<&OsString> = taken.iter().collect();
            let old = self.list.files();
            decodes.retain(|((i, _), _)| old.get(*i).is_none_or(|name| !taken.contains(name)));
        }
        // Where each of them goes in the new list, in this order: the decodes, the decoded
        // images, the panes and the last slide.
        let old_places: Vec<usize> = (decodes.iter().map(|((i, _), _)| *i))
            .chain(loaded_images.iter().map(|(i, _)| *i))
            .chain(panes.iter().copied())
            .chain([self.list.last_slide()])
            .collect();
        let new_places =
            places_in_new_list(self.list.files(), rows.files(), &old_places, moved.as_ref());
        let mut new_places = new_places.into_iter();
        // A new List means every place held in the old one goes with it, whatever field holds
        // it. Only what is carried over above comes back.
        self.list = List::replacing(&self.list, rows);
        if !taken.is_empty() {
            let dir = self.folder.dir().to_path_buf();
            self.forget_names(&taken.iter().map(|name| dir.join(name)).collect::<Vec<_>>());
        }
        // The shown file is set before anything refills the list, because with Compare on a
        // `refill_preload` at place 0 would drop the decode of the image shown.
        let shown = found.unwrap_or(0);
        self.list.set_shown(shown);
        self.index_new_names();
        let asked: Vec<_> = decodes.into_iter().zip(&mut new_places).collect();
        let kept: Vec<_> = loaded_images.into_iter().zip(&mut new_places).collect();
        let panes: Vec<usize> = (&mut new_places).take(panes.len()).flatten().collect();
        let last = new_places.next().flatten();
        let last = last.or(panes.last().copied()).unwrap_or(shown);
        self.list.set_panes(panes);
        self.list.set_last_slide(last);
        for ((_, loaded), i) in kept {
            if let Some(i) = i.filter(|&i| {
                self.is_file(i, &loaded.path)
                    && !is_another_file(loaded.identity(), self.list.identity(i))
            }) {
                self.list.keep_loaded_image(i, loaded);
            }
        }
        for (((_, full), job), i) in asked {
            if let Some(i) = i.filter(|&i| self.is_file(i, &job.1)) {
                self.list.ask_decode((i, full), job);
            }
        }
        // The search runs first, after the decoded images are carried over, because with Compare on
        // the selection refills, and the compared image and the neighbours are counted among the
        // matches, not in the whole folder.
        self.update_search(ctx);
        self.selection_changed(ctx);
        // The slideshow finds its images again in the new list, before they are read ahead.
        self.follow_list_in_slideshow();
        if still {
            self.refill_preload();
            self.update_window_title(ctx);
            return;
        }
        // `show_file` clears the notice for a new image. A new list is not one, and a notice is
        // often why the list was read again.
        let notice = self.notice.set_aside();
        self.show_file_anew(ctx, shown);
        self.notice.put_back(notice);
    }

    /// Remove the (sorted) indices from the list in one pass and shift index-based state
    /// accordingly. Neighbours' textures are kept, so the next image shows without decoding again.
    pub(crate) fn remove_indices(&mut self, ctx: &egui::Context, removed: &[usize]) {
        let gone = removed.binary_search(&self.list.shown()).is_ok();
        if gone {
            // The shown image is gone, and all that belonged to it.
            self.viewing = Viewing::default();
        }
        let version = self.list.version();
        // The next remaining file after the shown one, or the last. It is set in the list before
        // anything reads it, because with Compare on `refill_preload` at the old place would drop
        // the decode of the image shown.
        let shown = self.list.shown() - removed.partition_point(|&r| r < self.list.shown());
        // The panes' places belong to other files now, so they are filled again from the list
        // as it is.
        self.list.remove(removed);
        self.list
            .set_shown(shown.min(self.list.files().len().saturating_sub(1)));
        // The search runs first, because with Compare on the selection refills, and the compared
        // image and the neighbours are counted among the matches, not in the whole folder.
        self.update_search(ctx);
        self.list_changed(ctx);
        self.follow_list_in_slideshow();
        if let Some(pan) = &mut self.pan_view {
            pan.follow(removed, version, self.list.version());
        }
        if gone && !self.list.files().is_empty() {
            self.show_file_anew(ctx, self.list.shown());
        } else {
            // The shown image stays, so it keeps its zoom, turn and page, as in replace_files.
            self.refill_preload();
            self.update_window_title(ctx);
        }
    }

    pub(crate) fn receive_sizes(&mut self) {
        self.status_bar.receive_sizes();
        let mut size = self.list.match_size();
        self.search.receive_match_size(&mut size);
        self.list.set_match_size(size);
    }

    /// Go → Back and Forward. They go to the folder opened before or after this one. The
    /// history changes once the place is looked up (`step_history`), so Back pressed twice
    /// before the answer goes back one step, and a place opened meanwhile leaves it as it is.
    pub(crate) fn back_or_forward(&mut self, ctx: &egui::Context, action: Action) {
        let (from, history) = if action == Action::Back {
            (&self.folder.back, HistoryStep::Back)
        } else {
            (&self.folder.forward, HistoryStep::Forward)
        };
        if let Some(path) = from.last().cloned() {
            self.notice.clear();
            let opening = Opening {
                paths: vec![path],
                history,
                if_gone: IfGone::OpenItsFolder,
            };
            self.look_up(ctx, opening);
        }
    }

    /// View → Show Hidden Files. The list and the tree are read again, scrolled back to the open
    /// folder. Turned off, the hidden images leave the list at once.
    pub(crate) fn toggle_hidden_files(&mut self, ctx: &egui::Context) {
        self.config.show_hidden = !self.config.show_hidden;
        self.config.save();
        // Read again, and scrolled back to the open folder.
        self.tree.read_again_and_reopen();
        self.leave_out_names_not_shown(ctx);
        self.start_listing(ctx);
    }

    /// Takes out of the list at once, by their names, the images that the types shown no
    /// longer list: those of a type that the Types menu leaves out, and while hidden files are
    /// not shown, a hidden image, and in a folder's list one in a hidden folder below it. The
    /// listing that follows brings back those that the walk of a tree reaches through a link,
    /// as their names are new to it then. Before, they stayed until the whole list came, which
    /// for a tree on a share was minutes, and a slideshow went on showing the images just
    /// hidden. It is one pass over the names and no look at a file.
    pub(crate) fn leave_out_names_not_shown(&mut self, ctx: &egui::Context) {
        let types = self.shown_types();
        // The names of several folders and of a collection start at "/", where a hidden folder
        // above the places shown does not count.
        let folder = self.folder.collection.is_none() && self.folder.folders.is_empty();
        let in_hidden_folder = |name: &Path| {
            let folders = name.parent().into_iter().flat_map(Path::components);
            folders
                .into_iter()
                .any(|step| step.as_os_str().as_encoded_bytes().starts_with(b"."))
        };
        // The type by the name's ending alone: a RAW, a video, or else an image, also of an
        // added format, which is not looked up here, as that reads the formats' files.
        let listed_type = |name: &Path| {
            let raw = listing::has_ext(name, crate::images::raw::EXTS);
            let video = crate::files::external::is_video(name);
            (raw && types.raw) || (video && types.videos) || (!raw && !video && types.images)
        };
        let removed: Vec<usize> = (self.list.files().iter().enumerate())
            .filter(|(_, name)| {
                let name = Path::new(name);
                let hidden = !types.hidden
                    && (listing::is_hidden(name) || (folder && in_hidden_folder(name)));
                hidden || !listed_type(name)
            })
            .map(|(i, _)| i)
            .collect();
        if !removed.is_empty() {
            self.remove_indices(ctx, &removed);
        }
    }

    /// F5. The folder is read and sorted anew (also in an EXIF date order whose names stayed),
    /// and the shown image, the thumbnails and the size of a file edited elsewhere are made
    /// again.
    pub(crate) fn refresh(&mut self, ctx: &egui::Context) {
        // It was asked for, so the list is sorted anew, also in an EXIF date order whose names
        // stayed.
        self.folder.listed_as = None;
        // A part that still cannot be read is told again.
        self.folder.told_left_out.clear();
        self.tree.read_again();

        self.list.forget_loaded_images();
        // The image is decoded again now, because a folder read again unchanged keeps its list
        // as it is and asks for nothing. The thumbnails and the size of a file edited elsewhere
        // are made again too.
        self.refill_preload();
        self.thumbs_changed();
        self.list.forget_sizes_and_dates();
        // The Pan view reads every date again too, as a file edited elsewhere may have a new one.
        if let Some(pan) = &mut self.pan_view {
            pan.forget_dates();
        }
        self.start_listing(ctx);
    }
}

/// Returns the place in `new`, the new list, of the name at each of `places` of `old`, the list
/// before, or `None` for one that `new` lacks. Through `moved`, where the names went when a part
/// was merged into the list, each takes a few comparisons. Otherwise the names are looked for in
/// one pass over `new`, which ends when every one is found.
fn places_in_new_list(
    old: &[OsString],
    new: &[OsString],
    places: &[usize],
    moved: Option<&listing::PlacesMoved>,
) -> Vec<Option<usize>> {
    let same = |i: usize, k: usize| {
        old.get(i)
            .is_some_and(|name| new.get(k).is_some_and(|f| f == name))
    };
    if let Some(moved) = moved {
        let place = |&i: &usize| moved.new_place(i).filter(|&k| same(i, k));
        return places.iter().map(place).collect();
    }
    let mut found = vec![None; places.len()];
    let mut wanted: HashMap<&OsString, Vec<usize>> = HashMap::new();
    for (j, name) in (places.iter().enumerate()).filter_map(|(j, &i)| Some((j, old.get(i)?))) {
        wanted.entry(name).or_default().push(j);
    }
    for (k, f) in new.iter().enumerate() {
        if wanted.is_empty() {
            break;
        }
        for j in wanted.remove(f).unwrap_or_default() {
            found[j] = Some(k);
        }
    }
    found
}

/// Returns what each of `rows` is sorted by, in an order that looks at the files, by the hash
/// of its name (`listing::name_hash`), sorted by the hashes, for the whole list's sort to take
/// it from. A hash that two names share is left out, so that neither takes the other's.
fn keys_by_name_hash(rows: &Rows) -> Vec<(u64, SortKey)> {
    let mut keys: Vec<(u64, SortKey)> = (0..rows.len())
        .filter_map(|i| {
            let hash = listing::name_hash(rows.files()[i].as_encoded_bytes());
            Some((hash, rows.key(i)?.clone()))
        })
        .collect();
    keys.sort_unstable_by_key(|k| k.0);
    let shared: HashSet<u64> = (keys.windows(2))
        .filter(|w| w[0].0 == w[1].0)
        .map(|w| w[0].0)
        .collect();
    keys.retain(|k| !shared.contains(&k.0));
    keys
}

/// Returns how long a listing thread waits after it sent a part of a tree before it sends the
/// next one: four times as long as the part took `in_thread` to copy and sort or `in_window` to
/// take, whichever is longer, and at least a quarter second. Copying a long list then does not
/// slow the reading down, and the window spends at most a fifth of its time on the parts. The
/// window's merge of a part into a long list can take four times as long as the thread's copy
/// of the part (0.4 s for 500,000 names). Paced by the thread's time alone, the window then did
/// little else, and the slideshow's next image waited behind it.
fn next_part_after(in_thread: Duration, in_window: Duration) -> Duration {
    (in_thread.max(in_window) * 4).max(Duration::from_millis(250))
}

/// Looks up on disk what `paths` are, for `open`: a collection, a folder, a file in its folder,
/// or several folders. It runs in a thread of its own, because each look-up waits for the disk,
/// which on a share that stopped answering is for ever.
fn look_up(paths: &[PathBuf]) -> Found {
    let mut folders: Vec<PathBuf> = Vec::new();
    let mut first_there = None;
    for p in paths.iter().filter(|_| paths.len() > 1) {
        // A path that is not there is left out. Taken for a file, it stood for its parent
        // folder, whose images the user never asked for.
        let Some(is_dir) = is_dir_if_there(p) else {
            continue;
        };
        first_there.get_or_insert(p);
        // Each folder by its real path, as one folder opened alone, so the list names a file as
        // it does then: marks and stars are kept by that name. A file stands for its folder.
        let dir = match is_dir {
            true => listing::real_dir(p),
            false => {
                let real = listing::real_path(p);
                real.parent().map_or(real.clone(), Path::to_path_buf)
            }
        };
        if !folders.contains(&dir) {
            folders.push(dir);
        }
    }
    if folders.len() > 1 {
        return Found::Folders(folders);
    }
    // One folder (several of its files, from a file manager) opens at the first file that is
    // there.
    let path = first_there.or(paths.first()).cloned().unwrap_or_default();
    if collections::is_collection(&path) {
        // The path is absolute from here on, because it is kept in Open Recent and as the last
        // folder. A relative name was another file, or none, for a window started elsewhere.
        let file = std::path::absolute(&path).unwrap_or(path);
        // Each entry is named by its real path from the start, as the listing names it. Named as
        // written until then, an entry through a link was another name to the listing: the
        // image gone to and the selection were lost, and the view went back to the first image.
        let entries = collections::load(&file).map(|paths| {
            let real: Vec<PathBuf> = (collections::by_real_path(&paths).into_iter())
                .map(|(p, _)| p)
                .collect();
            (real.iter())
                .filter_map(|p| Some(p.strip_prefix("/").ok()?.as_os_str().to_owned()))
                .collect()
        });
        return Found::Collection { file, entries };
    }
    let is_dir = is_dir_if_there(&path);
    let (is_there, is_dir) = (is_dir.is_some(), is_dir.unwrap_or(false));
    let (dir, file) = match is_dir {
        true => (path, None),
        false => (
            path.parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."))
                .to_path_buf(),
            Some(path),
        ),
    };
    // The real path once here, so ⬆ .. and "remember folder" need no system calls later, and
    // marks, stars and `--remote select` find the files whichever way the folder was named.
    Found::Folder {
        dir: listing::real_dir(&dir),
        file_type: file.as_deref().map_or(Types::NONE, Types::of),
        file,
        is_there,
    }
}

/// Changes the recent file, which every window reads when it starts, in the writer thread:
/// `change` gets its paths as they are now and gives back the new ones. This works as for
/// every file the windows share. Two windows that change it at once keep both changes, and a
/// file that cannot be read is not written over.
fn edit_recent(change: impl FnOnce(Vec<PathBuf>) -> Vec<PathBuf> + Send + 'static) {
    crate::files::file_io::write_later(move || {
        let Some(file) = config::recent_path() else {
            return;
        };
        let changed = |old: &[u8]| (config::recent_text(&change(config::parse_recent(old))), ());
        if let Err(e) = crate::files::file_io::edit_shared(&file, changed) {
            crate::files::file_io::save_failed(tf!(
                "Could not save {name}: {e}",
                name = escaped_name(file),
                e
            ));
        }
    });
}

/// Returns whether `path` is a folder, through a link, and `None` when it is surely not there.
/// A path that could not be looked at (a share that dropped for a moment) counts as a folder
/// that is there. Opened, its failure to be read is told. Counted as gone, it was taken out of
/// Open Recent, and taken for a file, its parent folder was opened in its place.
fn is_dir_if_there(path: &Path) -> Option<bool> {
    match std::fs::metadata(path) {
        Ok(meta) => Some(meta.is_dir()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => Some(true),
    }
}

/// Keeps the `files` that `size` finds, and returns their sizes in the order kept. `size` is
/// called once for each file, because a collection of 500,000 entries on a share would
/// otherwise take a second system call for each.
fn keep_files_with_size(
    files: &mut Vec<ListedName>,
    mut size: impl FnMut(&mut ListedName) -> Option<u64>,
) -> Vec<u64> {
    let mut sizes = Vec::new();
    files.retain_mut(|f| size(f).map(|n| sizes.push(n)).is_some());
    sizes
}

/// Returns the metadata that a look at `path` gave, or `None` when the look failed. A failure
/// other than a file that cannot be reached (`file_io::names_no_file`) is kept in `left_out`, to
/// be told. Before, an entry under a folder replaced by a file, or a loop of links, was kept
/// as a broken row until the collection was opened again.
fn looked_at(
    look: std::io::Result<std::fs::Metadata>,
    path: &Path,
    left_out: &mut Vec<Unread>,
) -> Option<std::fs::Metadata> {
    match look {
        Ok(meta) => Some(meta),
        Err(e) if crate::files::file_io::names_no_file(&e) => None,
        Err(e) => {
            left_out.push((path.to_path_buf(), e));
            None
        }
    }
}

/// Returns the modification time of `path`, or `None` when it cannot be read.
fn modified(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Returns the places in the watched list to look at in a second: the `open` folders first in
/// it, and `SUBFOLDERS_A_SECOND` of the subfolders after them, from the `next`th on and round
/// to the first again.
fn folders_this_second(open: usize, all: usize, next: usize) -> impl Iterator<Item = usize> {
    let subfolders = all - open;
    let turn = subfolders.min(SUBFOLDERS_A_SECOND);
    (0..open).chain((0..turn).map(move |k| open + (next + k) % subfolders))
}

/// Looks at the folders of `read` that the watcher looks at in a second, from the `next`th
/// subfolder on, after the `open` folders first in it (`folders_this_second`), each with `look`
/// and through `gate` while `is_wanted` says the watcher still runs, and keeps the times it
/// found. On a share it looks at the subfolders only
/// while the looks of this second have taken less than `SHARE_LOOKS_A_SECOND`, the time waited
/// at the gate not counted. Returns the folders whose times changed, and how many subfolders it
/// looked at.
fn look_this_second(
    read: &mut FoldersRead,
    (open, next): (usize, usize),
    on_a_share: bool,
    (gate, is_wanted): (&mut listing::ReadsFirst, &dyn Fn() -> bool),
    look: impl Fn(&Path) -> Option<std::time::SystemTime>,
) -> (Vec<PathBuf>, usize) {
    let (mut changed, mut looked, mut spent) = (Vec::new(), 0, Duration::ZERO);
    for i in folders_this_second(open, read.len(), next) {
        if i >= open {
            if on_a_share && spent >= SHARE_LOOKS_A_SECOND {
                break;
            }
            looked += 1;
        }
        // A watcher that a newer one replaced looks at nothing more.
        let Some(_turn) = gate.before_work_nobody_waits_for(&read[i].0, is_wanted) else {
            break;
        };
        let began = Instant::now();
        let now = look(&read[i].0);
        // Only the subfolders' looks count, as the open folders are looked at every second.
        // Before, the open folders' looks took the subfolders' time, and on a share the watcher
        // came round to a subfolder only every few seconds.
        if i >= open {
            spent += began.elapsed();
        }
        if now != read[i].1 {
            changed.push(read[i].0.clone());
        }
        read[i].1 = now;
    }
    (changed, looked)
}

/// Sends sizes as partial sums of 10,000 files, so the status bar fills in gradually. It stops
/// when the receiver is gone. The closed channel tells the window that the sum is complete.
pub(crate) fn send_sizes(
    sizes: impl Iterator<Item = u64>,
    tx: mpsc::Sender<u64>,
    ctx: &egui::Context,
) {
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

/// Sends the sizes that `sizes` looks up a file at a time, as `send_sizes` does, unless a folder
/// of `places` is on a share, or with `with_subfolders` has one mounted below it
/// (`listing::is_on_a_share`). Then it sends `NOT_COUNTED_ON_A_SHARE` alone and looks at no file,
/// because each look is a round trip over the network, which a program the user started on the
/// share, such as a video player, would wait behind.
pub(crate) fn send_sizes_unless_on_a_share(
    places: &[PathBuf],
    with_subfolders: bool,
    sizes: impl Iterator<Item = u64>,
    tx: mpsc::Sender<u64>,
    ctx: &egui::Context,
) {
    match is_any_on_a_share(places, with_subfolders) {
        true => send_sizes(std::iter::once(NOT_COUNTED_ON_A_SHARE), tx, ctx),
        false => send_sizes(sizes, tx, ctx),
    }
}

/// Returns the sum of the sizes of `paths`, as `total_size` does, or `NOT_COUNTED_ON_A_SHARE`
/// without a look at any file when a folder of `places` is on a share, or with
/// `with_subfolders` has one mounted below it. Each look goes through the gate
/// (`gate_for_sizes`), which waits for the images `awaited`, and is made only while
/// `is_wanted` says that the sum is still waited for.
pub(crate) fn total_size_unless_on_a_share(
    places: &[PathBuf],
    with_subfolders: bool,
    paths: &[PathBuf],
    (awaited, is_wanted): (&listing::ImagesAwaited, &dyn Fn() -> bool),
) -> u64 {
    if is_any_on_a_share(places, with_subfolders) {
        return NOT_COUNTED_ON_A_SHARE;
    }
    let mut gate = gate_for_sizes(places, awaited);
    (paths.iter())
        .filter_map(|path| size_through(&mut gate, path, is_wanted))
        .sum()
}

/// Returns the gate for the looks at the sizes of the files of a list shown from `places`
/// (`Folder::places`), which waits for the images `awaited`. A collection's list is under "/",
/// so each of its files is asked about on its own, and one on a share waits at the gate.
/// Elsewhere `places` were asked about, and a list on a share has no size summed. Before, the
/// files of a collection on a share were looked at without the gate, before the slideshow's
/// next image.
pub(crate) fn gate_for_sizes(
    places: &[PathBuf],
    awaited: &listing::ImagesAwaited,
) -> listing::ReadsFirst {
    match places {
        [only] if only == Path::new("/") => listing::ReadsFirst::for_files_anywhere(awaited),
        _ => listing::ReadsFirst::new(Arc::default(), awaited.clone(), false),
    }
}

/// Returns the size of the file at `path`, looked at through `gate` while `is_wanted` says that
/// the sum is still waited for. Returns `None` when it cannot be looked at, or is not wanted.
/// Before, every size was looked at, also those of a sum that a new selection or search had
/// replaced, and on a share each took the turn ahead of the thumbnails and the images read
/// ahead.
pub(crate) fn size_through(
    gate: &mut listing::ReadsFirst,
    path: &Path,
    is_wanted: &dyn Fn() -> bool,
) -> Option<u64> {
    let _turn = gate.before_work_nobody_waits_for(path, is_wanted)?;
    Some(path.metadata().ok()?.len())
}

/// Returns whether a folder of `places` is on a share, or with `with_subfolders` has one mounted
/// below it, where the status bar sums no sizes. A collection's list is under "/", so its files
/// are summed, each through the gate (`gate_for_sizes`).
fn is_any_on_a_share(places: &[PathBuf], with_subfolders: bool) -> bool {
    (places.iter()).any(|place| listing::is_on_a_share(place, with_subfolders))
}

/// Adds the partial sums from `send_sizes` that have arrived. `rx` becomes `None` when the sum
/// is done.
pub(crate) fn add_sent_sizes(rx: &mut Receiving<u64>, total: &mut u64) {
    while let Took::Answer(sum) = rx.next() {
        // A sum not counted on a share stays as it is.
        *total = total.saturating_add(sum);
    }
}

/// The sum of the file sizes, with one stat for each file. Files that cannot be read are not
/// counted.
pub(crate) fn total_size(paths: &[PathBuf]) -> u64 {
    paths
        .iter()
        .filter_map(|p| p.metadata().ok())
        .map(|m| m.len())
        .sum()
}
/// Returns File → Open Recent's places as the recent file has them. When no list is kept, one
/// from before goes too, also when the setting was changed in the file. It waits for the disk,
/// so `main` calls it before the window opens.
pub(crate) fn read_recent(config: &Config) -> Vec<PathBuf> {
    match config::recent_path() {
        Some(file) if !config.keep_recent => {
            crate::files::file_io::write_later(move || crate::files::file_io::remove_shared(&file));
            Vec::new()
        }
        // Parsed by `parse_recent`, which config's survives_broken_files feeds broken input.
        Some(file) => crate::images::decode::read_limited(file)
            .map_or_else(|_| Vec::new(), |t| config::parse_recent(&t)),
        None => Vec::new(),
    }
}

/// Returns the Sort Manager's folders as their file has them. It waits for the disk, so `main`
/// calls it before the window opens.
pub(crate) fn read_sort_folders() -> Vec<PathBuf> {
    // Parsed by `sort_folders_from`, which config's survives_broken_files feeds broken input.
    (sort_folders_path().and_then(|p| crate::images::decode::read_limited(p).ok()))
        .map_or_else(Vec::new, |t| sort_folders_from(&t))
}

#[cfg(test)]
mod tests {
    /// A collection's entry that no file can be reached at is left out as gone, not kept to be
    /// told: one under a folder replaced by a file (ENOTDIR), and a loop of links (ELOOP).
    /// Before, either stayed as a broken row until the collection was opened again.
    #[test]
    fn an_entry_that_names_no_file_is_gone() {
        let dir = std::env::temp_dir().join(format!("glimt-test-no-file-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("f"), "").unwrap();
        std::os::unix::fs::symlink(dir.join("loop"), dir.join("loop")).unwrap();
        let mut left_out = Vec::new();
        let under_a_file = dir.join("f").join("a.png");
        let look = std::fs::symlink_metadata(&under_a_file);
        assert!(super::looked_at(look, &under_a_file, &mut left_out).is_none());
        let link = dir.join("loop");
        assert!(super::looked_at(link.metadata(), &link, &mut left_out).is_none());
        assert!(left_out.is_empty(), "{left_out:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    use crate::support::test_support::{app_on, contract_folder, run_background_until};

    /// A listing that finds another file under a name the list showed (another window's Rename
    /// Many swapped two names) reads that name's EXIF text again, and forgets what else was read
    /// of the file it held. Before, the name kept the old photo's `exif:` entry, so a search
    /// found the other photo, until another folder was opened.
    #[test]
    fn a_name_another_file_took_is_read_again() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-names-swapped-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a.png", "b.png", "c.png"] {
            std::fs::write(dir.join(name), name).unwrap();
        }
        let (mut app, ctx) = app_on(&dir);
        assert!(run_background_until(&mut app, &ctx, |a| !a
            .folder
            .is_listing()));
        let listing = |names: &[&str]| -> Vec<crate::model::listing::ListedName> {
            let id = |n: &str| crate::files::file_io::identity_of(&dir.join(n));
            names.iter().map(|n| (n.into(), id(n))).collect()
        };
        app.replace_files(&ctx, listing(&["a.png", "b.png", "c.png"]), None);
        let index = [("a.png", "canon"), ("b.png", "nikon"), ("c.png", "pentax")];
        (app.search).set_exif_index(index.map(|(n, t)| (n.into(), t.into())).into());
        // Another window swaps a.png and c.png, and the listing comes in another order.
        std::fs::rename(dir.join("a.png"), dir.join("t.png")).unwrap();
        std::fs::rename(dir.join("c.png"), dir.join("a.png")).unwrap();
        std::fs::rename(dir.join("t.png"), dir.join("c.png")).unwrap();
        app.replace_files(&ctx, listing(&["a.png", "c.png", "b.png"]), None);
        assert!(app.search.reads_exif_again("a.png") && app.search.reads_exif_again("c.png"));
        assert!(!app.search.reads_exif_again("b.png"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A refresh selects again the files that were selected, but not a name that another file
    /// has taken since. Before, it selected the name, and a Delete after it trashed the photo
    /// that another window's Rename Many had given that name, which the user never picked.
    /// The selected files that the search hides go the same way.
    #[test]
    fn a_name_another_file_took_is_not_selected_again() {
        let dir = std::env::temp_dir().join(format!(
            "glimt-test-taken-not-reselected-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a.png", "b.png", "c.png", "d.png"] {
            std::fs::write(dir.join(name), name).unwrap();
        }
        let (mut app, ctx) = app_on(&dir);
        assert!(run_background_until(&mut app, &ctx, |a| !a
            .folder
            .is_listing()));
        let listing = |names: &[&str]| -> Vec<crate::model::listing::ListedName> {
            let id = |n: &str| crate::files::file_io::identity_of(&dir.join(n));
            names.iter().map(|n| (n.into(), id(n))).collect()
        };
        app.take_listed_files(&ctx, listing(&["a.png", "b.png", "c.png", "d.png"]), true);
        // a.png and b.png are selected, and so is d.png, which the search hides.
        app.list.set_matches(Some(vec![0, 1, 2]));
        app.list.select([0, 1, 3].into());
        assert_eq!(app.list.hidden_selected(), &[3].into());
        // Another window gives a.png's name to c.png, c.png's to d.png and d.png's to a.png.
        for (from, to) in [("a", "t"), ("c", "a"), ("d", "c"), ("t", "d")] {
            std::fs::rename(
                dir.join(format!("{from}.png")),
                dir.join(format!("{to}.png")),
            )
            .unwrap();
        }
        app.take_listed_files(&ctx, listing(&["a.png", "b.png", "c.png", "d.png"]), true);
        let selected: Vec<_> = (app.list.selected_with_hidden().iter())
            .map(|&i| app.list.files()[i].clone())
            .collect();
        assert_eq!(selected, ["b.png"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The decodes still being made when the list is read again (a tree's next part, which moves
    /// the files) go with their files to their new places, and are neither sent again nor
    /// dropped. Before, only the shown file's went: each neighbour was asked for again at every
    /// part, and its job before was decoded and thrown away, which on a slow share queued the
    /// next image behind them all. Before that, the workers judged a job by its old place.
    #[test]
    fn decodes_being_made_follow_their_files_to_a_new_list() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-decodes-follow-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let names = ["a.png", "b.png", "c.png", "d.png"];
        for name in names {
            image::RgbImage::new(8, 8).save(dir.join(name)).unwrap();
        }
        let (mut app, ctx) = app_on(&dir);
        app.refill_preload();
        let asked = |app: &crate::App| -> std::collections::BTreeMap<_, _> {
            (app.list.decodes_pending().iter())
                .map(|(&(i, full), job)| ((app.list.files()[i].clone(), full), job.0))
                .collect()
        };
        let before = asked(&app);
        assert_eq!(before.len(), 3, "{before:?}");
        let identity = |n: &str| crate::files::file_io::identity_of(&dir.join(n));
        let reversed = names
            .iter()
            .rev()
            .map(|n| (n.into(), identity(n)))
            .collect();
        app.replace_files(&ctx, reversed, Some("a.png".into()));
        assert_eq!(app.list.shown(), 3);
        app.refill_preload();
        // d.png is asked for too, now the slideshow's next image after a.png.
        let asked_now = asked(&app);
        assert!(
            before.iter().all(|(k, n)| asked_now.get(k) == Some(n)),
            "{asked_now:?}"
        );
        let decoded = |app: &crate::App| {
            app.list.decodes_pending().is_empty()
                && [1, 2, 3]
                    .iter()
                    .all(|i| app.list.loaded_images().contains_key(i))
        };
        assert!(run_background_until(&mut app, &ctx, decoded));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A tree's whole list that is the same as its last part keeps the sizes and dates read for
    /// the part, as the same walk read both. Before, the rows on screen read them again.
    #[test]
    fn a_whole_list_like_its_last_part_keeps_the_sizes_read() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-whole-as-part-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a.png", "b.png"] {
            std::fs::write(dir.join(name), name).unwrap();
        }
        let (mut app, ctx) = app_on(&dir);
        assert!(run_background_until(&mut app, &ctx, |a| !a
            .folder
            .is_listing()));
        let part = || {
            vec![(
                "a.png".into(),
                crate::files::file_io::identity_of(&dir.join("a.png")),
            )]
        };
        // The first part of a tree, into a list still empty.
        app.replace_files(&ctx, Vec::new(), None);
        app.take_listed_files(&ctx, part(), false);
        let read =
            std::collections::HashMap::from([(std::ffi::OsString::from("a.png"), Some((5, 6)))]);
        app.list.keep_sizes_and_dates(read.clone());
        app.take_listed_files(&ctx, part(), true);
        assert_eq!(app.list.sizes_and_dates(), &read);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The folder shown last is looked up in the background at start, as any path, and when it
    /// is gone the current folder opens instead. Before, the window's thread looked at it before
    /// the window opened, which a share that stopped answering held for ever.
    #[test]
    fn a_last_folder_that_is_gone_opens_the_current_one() {
        let dir = contract_folder("last-folder");
        let (mut app, ctx) = app_on(&dir);
        let wait = std::time::Duration::from_secs(5);
        app.open_last_folder(&ctx, dir.join("gone"));
        // The first answer says it is gone and asks for the current folder.
        app.receive_looked_up(&ctx, wait);
        app.receive_looked_up(&ctx, wait);
        let here = std::env::current_dir().unwrap().canonicalize().unwrap();
        assert_eq!(app.folder.dir(), here);
        app.open_last_folder(&ctx, dir.join("sub"));
        app.receive_looked_up(&ctx, wait);
        assert_eq!(app.folder.dir(), dir.join("sub").canonicalize().unwrap());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A collection open in this window follows a move that another window wrote into the
    /// collection's file, because the window watches that file as it watches a folder. Before,
    /// it kept the old names until it was opened again.
    #[test]
    fn a_collection_follows_its_file_changed_by_another_window() {
        let dir = contract_folder("collection-watched");
        let (mut app, ctx) = app_on(&dir.join("col.gqv"));
        assert_eq!(app.list.files().len(), 2);
        // The watcher reads the file's time when it starts, which is to be before the change.
        std::thread::sleep(std::time::Duration::from_millis(100));
        // Another window moves a.png to c.png, and its writer follows it in the collection.
        let (a, c) = (dir.join("a.png"), dir.join("c.png"));
        std::fs::rename(&a, &c).unwrap();
        let follow = |all: &mut Vec<std::path::PathBuf>| {
            for p in all.iter_mut().filter(|p| p.ends_with("a.png")) {
                *p = c.clone();
            }
        };
        crate::model::collections::edit(&dir.join("col.gqv"), false, follow).unwrap();
        let has_c = |app: &crate::App| {
            (app.list.files().iter()).any(|f| std::path::Path::new(f).ends_with("c.png"))
        };
        assert!(run_background_until(&mut app, &ctx, has_c));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Folders shown together are watched, each of them, so an image added to one is listed.
    /// A change in a folder that is not shown lists nothing. Before, no watcher ran for them.
    #[test]
    fn folders_shown_together_are_watched() {
        let dir = std::env::temp_dir().join(format!("glimt-test-together-{}", std::process::id()));
        let (x, y, z) = (dir.join("x"), dir.join("y"), dir.join("z"));
        for d in [&x, &y, &z] {
            std::fs::create_dir_all(d).unwrap();
            image::RgbImage::new(2, 2).save(d.join("a.png")).unwrap();
        }
        let (mut app, ctx) = app_on(&x);
        app.open_folders(&ctx, vec![x.clone(), y.clone()]);
        app.receive_looked_up(&ctx, std::time::Duration::from_secs(5));
        let count = |n| move |app: &crate::App| app.list.files().len() == n;
        assert!(run_background_until(&mut app, &ctx, count(2)));
        // The watcher reads the folders' times when it starts, which is to be before the change.
        std::thread::sleep(std::time::Duration::from_millis(100));
        image::RgbImage::new(2, 2).save(y.join("b.png")).unwrap();
        assert!(run_background_until(&mut app, &ctx, count(3)));
        let listings_before = app.folder.whole_listings();
        image::RgbImage::new(2, 2).save(z.join("b.png")).unwrap();
        // The watcher looks once a second.
        std::thread::sleep(std::time::Duration::from_millis(1500));
        app.poll_background(&ctx);
        app.receive_listing(&ctx, std::time::Duration::from_millis(500));
        assert_eq!(app.folder.whole_listings(), listings_before);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Files put in a folder join the list of folders shown together only in one of them, or
    /// below one with Subfolders on, and never in a collection's "/". Before, files put in any
    /// folder counted with Subfolders on, and files put in one of the folders shown with it off
    /// did not.
    #[test]
    fn files_join_the_list_only_in_the_places_shown() {
        let dir = std::env::temp_dir().join(format!("glimt-test-joined-{}", std::process::id()));
        let (x, y) = (dir.join("x"), dir.join("y"));
        for d in [&x, &y] {
            std::fs::create_dir_all(d.join("sub")).unwrap();
            image::RgbImage::new(2, 2).save(d.join("a.png")).unwrap();
        }
        let dir = crate::model::listing::real_dir(&dir);
        let (x, y, z) = (dir.join("x"), dir.join("y"), dir.join("z"));
        let (mut app, ctx) = app_on(&x);
        app.open_folders(&ctx, vec![x.clone(), y.clone()]);
        app.receive_looked_up(&ctx, std::time::Duration::from_secs(5));
        assert!(app.folder.collection().is_none() && app.folder.places().len() == 2);
        for on in [false, true] {
            app.folder.set_recursive(on);
            assert!(app.folder.takes_files_put_in(&y), "{on}");
            assert_eq!(app.folder.takes_files_put_in(&x.join("sub")), on);
            assert!(!app.folder.takes_files_put_in(&z), "{on}");
        }
        let collection = dir.join("c.gqv");
        let entry = format!("#GQview collection\n\"{}\"\n", x.join("a.png").display());
        std::fs::write(&collection, entry).unwrap();
        crate::support::test_support::open_now(&mut app, &ctx, collection);
        assert!(app.folder.collection().is_some());
        app.folder.show_subfolders(true);
        assert!(!app.folder.takes_files_put_in(&z));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A path that is not there, among several, is left out. Before, it was taken for a file
    /// and stood for its parent folder, which was shown with the others.
    #[test]
    fn a_missing_path_among_several_is_left_out() {
        let dir = std::env::temp_dir().join(format!("glimt-test-typo-{}", std::process::id()));
        let (x, y) = (dir.join("x"), dir.join("y"));
        for d in [&x, &y] {
            std::fs::create_dir_all(d).unwrap();
        }
        let typo = dir.join("typo");
        let real = |p: &std::path::Path| crate::model::listing::real_dir(p);
        let found = super::look_up(&[x.clone(), typo.clone(), y.clone()]);
        assert!(matches!(found, super::Found::Folders(f) if f == [real(&x), real(&y)]));
        // With one left, it opens alone.
        let found = super::look_up(&[typo.clone(), x.clone()]);
        assert!(
            matches!(found, super::Found::Folder { dir, is_there: true, .. } if dir == real(&x))
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A path that could not be looked at (here one below a link that loops) is no path that is
    /// gone: it stays in Open Recent and opens as a folder, whose failure to be read is told.
    /// Before, it counted as gone, and Open Recent forgot it.
    #[test]
    fn a_path_that_cannot_be_looked_at_is_not_gone() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-unseen-path-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::os::unix::fs::symlink(dir.join("loop"), dir.join("loop")).unwrap();
        let unseen = dir.join("loop").join("photos");
        let found = super::look_up(std::slice::from_ref(&unseen));
        assert!(matches!(
            found,
            super::Found::Folder {
                is_there: true,
                file: None,
                ..
            }
        ));
        // Among the several paths of a file manager, it is kept too.
        let other = dir.join("other");
        std::fs::create_dir_all(&other).unwrap();
        let found = super::look_up(&[unseen, other]);
        assert!(matches!(found, super::Found::Folders(f) if f.len() == 2));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A folder that cannot be read is told, and a listing that fails leaves the list shown and
    /// its selection as they were. Before, the folder was listed as empty without a word, and
    /// a refresh on a share that failed for a moment emptied the list, so the files came back
    /// unselected with the next listing.
    #[test]
    fn a_folder_that_cannot_be_read_is_told_and_the_list_stays() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-unread-folder-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a.png", "b.png"] {
            std::fs::write(dir.join(name), name).unwrap();
        }
        let (mut app, ctx) = app_on(&dir);
        // A link to itself cannot be read through, also by root.
        std::os::unix::fs::symlink(dir.join("loop"), dir.join("loop")).unwrap();
        let unread = dir.join("loop").join("photos");
        app.open(&ctx, unread.clone());
        assert!(run_background_until(&mut app, &ctx, |a| !a
            .folder
            .is_listing()));
        let told = app.notice.text().unwrap_or_default().to_owned();
        let start = format!("Could not open {}\n", unread.display());
        assert!(told.starts_with(&start), "{told}");
        // A refresh that fails keeps the list and the selection.
        crate::support::test_support::open_now(&mut app, &ctx, dir.clone());
        assert!(run_background_until(&mut app, &ctx, |a| !a
            .folder
            .is_listing()));
        app.set_selection(&ctx, [0, 1].into());
        let (tx, rx) = std::sync::mpsc::channel();
        app.folder.hold_listing(rx);
        let failed = Err((dir.clone(), std::io::Error::other("the share failed")));
        let no_time = std::time::Duration::ZERO;
        let listed = super::Listed {
            dirs: Vec::new(),
            files: failed,
            left_out: Vec::new(),
            read: Vec::new(),
            read_in: no_time,
        };
        tx.send(listed).unwrap();
        app.receive_listing(&ctx, no_time);
        assert_eq!(app.list.files(), ["a.png", "b.png"]);
        assert_eq!(app.list.selected(), &[0, 1].into());
        let told = app.notice.text().unwrap_or_default();
        assert!(told.ends_with("the share failed"), "{told}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// An edit that waits for a collection's listing is dropped and told as a failed write
    /// when the listing fails, so that a `--remote … --wait` answers "not done". Before, it
    /// waited for F5 with nothing told, and the `--wait` answered done.
    #[test]
    fn a_failed_listing_tells_the_edits_that_waited_for_it() {
        let dir = contract_folder("unchecked-listing-fails");
        std::fs::write(dir.join("show.m3u"), "a.png\n").unwrap();
        let (mut app, ctx) = app_on(&dir);
        crate::support::test_support::open_now(&mut app, &ctx, dir.join("show.m3u"));
        assert!(app.list.has_unchecked_entries());
        let files = vec![(app.listed_file(0), ())];
        app.write_sidecars(&ctx, "Keywords", files, |_, ()| Ok(()), |_, _| None);
        let (tx, rx) = std::sync::mpsc::channel();
        app.folder.hold_listing(rx);
        let failed = Err((dir.clone(), std::io::Error::other("the share failed")));
        let no_time = std::time::Duration::ZERO;
        let listed = super::Listed {
            dirs: Vec::new(),
            files: failed,
            left_out: Vec::new(),
            read: Vec::new(),
            read_in: no_time,
        };
        tx.send(listed).unwrap();
        app.receive_answers(&ctx);
        assert_eq!(app.window.failed_writes_told(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A folder among several, or an entry of a collection, that cannot be read is left out
    /// and told, once until F5, and the rest is listed. Before, it failed the whole listing at
    /// every listing: no image of the other folders was shown, and a collection held every
    /// file operation back for good.
    #[test]
    fn a_part_that_cannot_be_read_is_left_out_and_told_once() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-unread-part-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for sub in ["a", "b"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
            image::RgbImage::new(2, 2)
                .save(dir.join(sub).join("1.png"))
                .unwrap();
        }
        // A link to itself cannot be read through, also by root.
        std::os::unix::fs::symlink(dir.join("loop"), dir.join("loop")).unwrap();
        let (mut app, ctx) = app_on(&dir);
        let real = crate::model::listing::real_dir(&dir);
        let folders = ["a", "b", "loop"].map(|f| real.join(f)).to_vec();
        app.open_folders_found(&ctx, folders);
        let listed = |app: &mut crate::App| {
            assert!(run_background_until(app, &ctx, |a| !a.folder.is_listing()));
        };
        listed(&mut app);
        let in_root = |f: &str| real.join(f).strip_prefix("/").unwrap().to_owned();
        assert_eq!(app.list.files(), [in_root("a/1.png"), in_root("b/1.png")]);
        let start = format!("Could not open {}\n", real.join("loop").display());
        let told = app.notice.text().unwrap_or_default().to_owned();
        assert!(told.starts_with(&start), "{told}");
        // A change that lists the folders again does not tell it again, and F5 does.
        app.notice.clear();
        app.start_listing(&ctx);
        listed(&mut app);
        assert_eq!(app.notice.text(), None);
        app.refresh(&ctx);
        listed(&mut app);
        assert!(app.notice.text().unwrap_or_default().starts_with(&start));
        // An entry of a collection that names a path through a file is left out as gone,
        // without a word, as no file can be there, and the collection's files can be worked on.
        let list = dir.join("show.m3u");
        std::fs::write(&list, "a/1.png\na/1.png/x.png\n").unwrap();
        app.notice.clear();
        crate::support::test_support::open_now(&mut app, &ctx, list);
        listed(&mut app);
        assert_eq!(app.list.files(), [in_root("a/1.png")]);
        assert!(!app.list.has_unchecked_entries());
        assert_eq!(app.notice.text(), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A part left out of a listing keeps the files of it that the list shows, with their
    /// selection, because a share that failed for a moment would take them away unselected.
    /// The rest of the list is listed anew, and the part is told once. Before, such a part held
    /// the whole listing for as long as it stayed unreadable: new files never came, and the
    /// part was told at every listing.
    #[test]
    fn a_part_left_out_keeps_its_shown_files_and_the_rest_is_listed() {
        let dir = std::env::temp_dir().join(format!("glimt-test-held-part-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        for name in ["a.png", "sub/b.png"] {
            image::RgbImage::new(2, 2).save(dir.join(name)).unwrap();
        }
        let (mut app, ctx) = app_on(&dir);
        let dir = crate::model::listing::real_dir(&dir);
        app.config.show_subfolders = true;
        crate::support::test_support::open_now(&mut app, &ctx, dir.clone());
        assert!(run_background_until(&mut app, &ctx, |a| !a
            .folder
            .is_listing()));
        assert_eq!(app.list.files(), ["a.png", "sub/b.png"]);
        app.set_selection(&ctx, [0, 1].into());
        let send = |app: &mut crate::App, part: &str| {
            let (tx, rx) = std::sync::mpsc::channel();
            app.folder.hold_listing(rx);
            let listed = super::Listed {
                dirs: Vec::new(),
                files: Ok(super::ListedRows::Rows(super::Rows::unsorted(vec![
                    ("a.png".into(), None),
                    ("c.png".into(), None),
                ]))),
                left_out: vec![(dir.join(part), std::io::Error::other("the share failed"))],
                read: Vec::new(),
                read_in: std::time::Duration::ZERO,
            };
            tx.send(listed).unwrap();
            app.receive_listing(&ctx, std::time::Duration::ZERO);
        };
        send(&mut app, "sub");
        assert_eq!(app.list.files(), ["a.png", "sub/b.png", "c.png"]);
        assert_eq!(app.list.selected(), &[0, 1].into());
        let told = app.notice.text().unwrap_or_default();
        assert!(told.ends_with("the share failed"), "{told}");
        // The next listing that leaves it out does not tell it again.
        app.notice.clear();
        send(&mut app, "sub");
        assert_eq!(app.list.files(), ["a.png", "sub/b.png", "c.png"]);
        assert_eq!(app.notice.text(), None);
        // "su" is no folder of "sub/b.png".
        send(&mut app, "su");
        assert_eq!(app.list.files(), ["a.png", "c.png"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Remember Last Folder keeps a place that opened. Before the first look-up answers, it
    /// is the place remembered at the start, and while folders are shown together, the place
    /// shown before them. Before, it kept "" (the remembered folder lost) or "/" (the next start
    /// listed the whole disk).
    #[test]
    fn the_place_remembered_is_one_that_opened() {
        let dir = std::env::temp_dir().join(format!("glimt-test-remember-{}", std::process::id()));
        let (x, y) = (dir.join("x"), dir.join("y"));
        for d in [&x, &y] {
            std::fs::create_dir_all(d).unwrap();
        }
        let kept = Some(std::path::PathBuf::from("/kept"));
        let not_opened = super::Folder::new(kept.clone(), Vec::new(), Vec::new());
        assert_eq!(not_opened.place_to_remember(), kept.as_deref());
        let nothing_kept = super::Folder::new(None, Vec::new(), Vec::new());
        assert_eq!(nothing_kept.place_to_remember(), None);
        let (mut app, ctx) = app_on(&x);
        let x = crate::model::listing::real_dir(&x);
        assert_eq!(app.folder.place_to_remember(), Some(x.as_path()));
        app.open_folders(&ctx, vec![x.clone(), y.clone()]);
        app.receive_looked_up(&ctx, std::time::Duration::from_secs(5));
        assert_eq!(app.folder.folders().len(), 2);
        assert_eq!(app.folder.place_to_remember(), Some(x.as_path()));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// With Include Subfolders on, the tree's subfolders are watched too, so an image added to
    /// one is listed. A change in a folder outside the tree reads nothing again. Before, only
    /// the top folder was watched, and the image stayed out of the list.
    #[test]
    fn subfolders_of_a_tree_are_watched() {
        let base =
            std::env::temp_dir().join(format!("glimt-test-tree-watch-{}", std::process::id()));
        let (top, sub, outside) = (base.join("top"), base.join("top/sub"), base.join("outside"));
        for d in [&sub, &outside] {
            std::fs::create_dir_all(d).unwrap();
        }
        image::RgbImage::new(2, 2).save(sub.join("a.png")).unwrap();
        let (mut app, ctx) = app_on(&top);
        app.folder.show_subfolders(true);
        app.start_listing(&ctx);
        let count = |n| move |app: &crate::App| app.list.files().len() == n;
        assert!(run_background_until(&mut app, &ctx, count(1)));
        // The folders' times are read with the listing, which is to be before the change.
        std::thread::sleep(std::time::Duration::from_millis(100));
        image::RgbImage::new(2, 2).save(sub.join("b.png")).unwrap();
        assert!(run_background_until(&mut app, &ctx, count(2)));
        let listings_before = app.folder.whole_listings();
        image::RgbImage::new(2, 2)
            .save(outside.join("c.png"))
            .unwrap();
        // The watcher looks once a second.
        std::thread::sleep(std::time::Duration::from_millis(1500));
        app.poll_background(&ctx);
        app.receive_listing(&ctx, std::time::Duration::from_millis(500));
        assert_eq!(app.folder.whole_listings(), listings_before);
        std::fs::remove_dir_all(base).unwrap();
    }

    /// A burst of changes, as a sync client writes, lists a folder whose last listing took long
    /// once, after the window has rested from that listing. Before, each change that the
    /// watcher saw listed it again at once, and a large tree was read about once a second.
    #[test]
    fn a_burst_of_changes_is_listed_once_after_a_rest() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-change-burst-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbImage::new(2, 2).save(dir.join("a.png")).unwrap();
        let (mut app, ctx) = app_on(&dir);
        // As after a listing of a large tree that took a quarter of a second.
        app.folder.rest_until = std::time::Instant::now() + std::time::Duration::from_secs(1);
        let before = app.folder.whole_listings();
        for _ in 0..3 {
            app.folder
                .changed_tx
                .send(vec![app.folder.dir.clone()])
                .unwrap();
            app.poll_background(&ctx);
            assert!(!app.folder.is_listing(), "listed during the rest");
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let listed = |app: &crate::App| app.folder.whole_listings() > before;
        assert!(run_background_until(&mut app, &ctx, listed));
        assert!(!run_background_until(&mut app, &ctx, |app| app
            .folder
            .is_listing()));
        assert_eq!(app.folder.whole_listings(), before + 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A change that the watcher saw in one subfolder of a tree reads that subfolder again, and
    /// not the whole tree: its rows are what it holds now, also with a folder below it added and
    /// one removed, and the rows of the other subfolders stay as they were. A change in the open
    /// folder itself still reads the whole tree. Before, any change read the whole tree again,
    /// which on a share takes minutes.
    #[test]
    fn a_change_in_one_subfolder_reads_only_that_subfolder_again() {
        let dir = std::env::temp_dir().join(format!("glimt-test-subfolder-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for sub in ["s1", "s2/deep"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        for name in ["a.png", "s1/b.png", "s2/c.png", "s2/deep/d.png"] {
            image::RgbImage::new(2, 2).save(dir.join(name)).unwrap();
        }
        let (mut app, ctx) = app_on(&dir);
        app.folder.show_subfolders(true);
        app.start_listing(&ctx);
        let whole_listings = |app: &crate::App| app.folder.whole_listings();
        let before = whole_listings(&app);
        assert!(run_background_until(&mut app, &ctx, |app| whole_listings(
            app
        ) > before));
        let names = |app: &crate::App| -> Vec<String> {
            let files = app.list.files().iter();
            files.map(|f| f.to_string_lossy().into_owned()).collect()
        };
        assert_eq!(
            names(&app),
            ["a.png", "s1/b.png", "s2/c.png", "s2/deep/d.png"]
        );
        // The test sends the change itself, so the real watcher is silenced. Seen by it, the
        // change of s1 read the whole tree, when the watcher looked between these changes and
        // the change sent.
        (app.folder.watcher_generation).fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // s1 loses its image unseen: only a walk of the whole tree would take it out.
        std::fs::remove_file(dir.join("s1/b.png")).unwrap();
        std::fs::remove_file(dir.join("s2/c.png")).unwrap();
        std::fs::remove_dir_all(dir.join("s2/deep")).unwrap();
        std::fs::create_dir_all(dir.join("s2/new")).unwrap();
        for name in ["s2/e.png", "s2/new/f.png"] {
            image::RgbImage::new(2, 2).save(dir.join(name)).unwrap();
        }
        // The watcher looks every second, so it would have seen s1 change by now.
        std::thread::sleep(std::time::Duration::from_millis(1500));
        let changed = vec![dir.join("s2"), dir.join("s2/deep")];
        app.folder.changed_tx.send(changed).unwrap();
        app.folder.rest_until = std::time::Instant::now();
        let before = whole_listings(&app);
        assert!(run_background_until(&mut app, &ctx, |app| whole_listings(
            app
        ) > before));
        assert_eq!(
            names(&app),
            ["a.png", "s1/b.png", "s2/e.png", "s2/new/f.png"]
        );
        // The list is still the tree listed whole, so the next change reads only its subfolder.
        assert!(
            app.folder.listed_as.is_some(),
            "the list forgot how it was listed"
        );
        // A change of the open folder itself reads the whole tree.
        app.folder.changed_tx.send(vec![dir.clone()]).unwrap();
        app.folder.rest_until = std::time::Instant::now();
        let before = whole_listings(&app);
        assert!(run_background_until(&mut app, &ctx, |app| whole_listings(
            app
        ) > before));
        assert_eq!(names(&app), ["a.png", "s2/e.png", "s2/new/f.png"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A subfolder read again gives a name that another file has taken the new file's identity,
    /// as a whole listing does, so the name keeps nothing of the file it held: the image decoded
    /// of it is dropped and the new file's image comes. Before, the names that stayed kept the
    /// identities of the files they held, and every decode of the new file was dropped as
    /// another file's, until F5.
    #[test]
    fn a_subfolder_read_again_gives_a_name_its_new_file() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-subfolder-taken-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("s2")).unwrap();
        image::RgbImage::new(4, 4)
            .save(dir.join("s2/a.png"))
            .unwrap();
        image::RgbImage::new(6, 6)
            .save(dir.join("s2/b.png"))
            .unwrap();
        let (mut app, ctx) = app_on(&dir);
        app.folder.show_subfolders(true);
        app.start_listing(&ctx);
        let whole_listings = |app: &crate::App| app.folder.whole_listings();
        let before = whole_listings(&app);
        assert!(run_background_until(&mut app, &ctx, |a| whole_listings(a) > before));
        // The test sends the change itself, so the real watcher is silenced.
        (app.folder.watcher_generation).fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(app.list.files(), ["s2/a.png", "s2/b.png"]);
        let identity = app.list.identity(0);
        assert!(identity.is_some(), "the listing gave no identity");
        app.refill_preload();
        let size = |a: &crate::App| a.list.loaded_images().get(&0).map(|s| s.original_size);
        assert!(run_background_until(&mut app, &ctx, |a| size(a) == Some([4, 4])));
        for (from, to) in [("a.png", "c.png"), ("b.png", "a.png"), ("c.png", "b.png")] {
            std::fs::rename(dir.join("s2").join(from), dir.join("s2").join(to)).unwrap();
        }
        app.folder.changed_tx.send(vec![dir.join("s2")]).unwrap();
        app.folder.rest_until = std::time::Instant::now();
        let before = whole_listings(&app);
        assert!(run_background_until(&mut app, &ctx, |a| whole_listings(a) > before));
        assert_ne!(
            app.list.identity(0),
            identity,
            "s2/a.png keeps the old file's identity"
        );
        assert_ne!(size(&app), Some([4, 4]), "s2/a.png shows the photo it held");
        assert!(run_background_until(&mut app, &ctx, |a| size(a) == Some([6, 6])));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A write refused because another file has taken a name makes only the window whose list
    /// showed the name read its folder again. Before, the flag was the whole process's, so one
    /// test's refusal had another test's window list during its rest.
    #[test]
    fn a_name_taken_in_one_window_has_only_that_window_read_its_folder() {
        let base = std::env::temp_dir().join(format!("glimt-test-list-old-{}", std::process::id()));
        let (one, other) = (base.join("one"), base.join("other"));
        for dir in [&one, &other] {
            std::fs::create_dir_all(dir).unwrap();
            image::RgbImage::new(2, 2).save(dir.join("a.png")).unwrap();
        }
        let (mut first, ctx) = app_on(&one);
        let (mut second, other_ctx) = app_on(&other);
        let listed = first.listed_file(0);
        // Another file takes a.png's name, made before the old one goes, so it has another
        // inode.
        std::fs::write(one.join("new"), "new").unwrap();
        std::fs::rename(one.join("new"), one.join("a.png")).unwrap();
        assert!(listed.check().is_err());
        second.poll_background(&other_ctx);
        assert!(
            !second.folder.is_listing(),
            "the other window read its folder"
        );
        first.poll_background(&ctx);
        assert!(first.folder.is_listing());
        std::fs::remove_dir_all(base).unwrap();
    }

    /// A window that is not drawn (minimized, or on another workspace) takes the list late.
    /// That wait is no listing, so the rest after it is measured from the time the thread took,
    /// not from the start to the frame that took the list. Before, a relist that a window on
    /// another workspace took an hour later rested four hours.
    #[test]
    fn the_rest_after_a_listing_does_not_count_the_time_the_window_was_not_drawn() {
        let dir = std::env::temp_dir().join(format!("glimt-test-late-rest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbImage::new(2, 2).save(dir.join("a.png")).unwrap();
        let (mut app, ctx) = app_on(&dir);
        let before = app.folder.whole_listings();
        app.start_listing(&ctx);
        // The thread is done in a few milliseconds, and the window takes it half a second later.
        std::thread::sleep(std::time::Duration::from_millis(500));
        let listed = |app: &crate::App| app.folder.whole_listings() > before;
        assert!(run_background_until(&mut app, &ctx, listed));
        let rest = app
            .folder
            .rest_until
            .saturating_duration_since(std::time::Instant::now());
        assert!(
            rest < std::time::Duration::from_millis(400),
            "rests {rest:?}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The changes that the watcher sees between listings keep one folder, the deepest that
    /// holds them all, however often they come. Before, each folder seen was kept, every time
    /// again, and a tree that a sync client wrote into grew the list during a long rest (#291).
    #[test]
    fn changes_seen_between_listings_keep_one_folder() {
        let dir = contract_folder("changes-seen");
        let (mut app, _ctx) = app_on(&dir);
        (app.folder.watcher_generation).fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        for _ in 0..1000 {
            let changed = vec![dir.join("s2/a"), dir.join("s2/b/c")];
            app.folder.changed_tx.send(changed).unwrap();
            assert!(app.folder.changed_on_disk());
        }
        let seen: Vec<&std::path::Path> = app.folder.changes_seen.folder().into_iter().collect();
        assert_eq!(seen, [dir.join("s2").as_path()]);
        // A change outside the open folder leaves nothing below it to read alone.
        let elsewhere = dir.parent().unwrap().join("elsewhere");
        app.folder.changed_tx.send(vec![elsewhere]).unwrap();
        app.folder.changed_on_disk();
        let seen: Vec<&std::path::Path> = app.folder.changes_seen.folder().into_iter().collect();
        assert!(seen.len() == 1 && !seen[0].starts_with(&dir), "{seen:?}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The changes that the watcher sees are taken by `receive_answers`, which a minimized
    /// window runs without a frame, and told once by the next `changed_on_disk`. Before, only a
    /// drawn frame took them, and they piled up while the window was minimized (#188).
    /// Risk: a minimized window piles up the watcher's changes in its channel.
    #[test]
    fn the_watchers_changes_are_taken_without_a_frame() {
        let dir = contract_folder("changes-without-a-frame");
        let (mut app, ctx) = app_on(&dir);
        (app.folder.watcher_generation).fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        app.folder.changed_tx.send(vec![dir.join("sub")]).unwrap();
        app.receive_answers(&ctx);
        assert!(
            app.folder.changed_rx.try_recv().is_err(),
            "left in the channel"
        );
        assert!(app.folder.changed_on_disk());
        assert!(!app.folder.changed_on_disk());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A change taken before a listing starts is read by that listing, so `changed_on_disk` no
    /// longer tells it. Before, it still told it after the listing had forgotten which folder
    /// changed, and the whole tree was read again, as when a minimized window took a change and
    /// a `--remote` command then started a listing.
    /// Risk: a change already read starts a listing of the whole tree again.
    #[test]
    fn a_listing_reads_the_changes_taken_before_it() {
        let dir = contract_folder("changes-before-a-listing");
        let (mut app, ctx) = app_on(&dir);
        (app.folder.watcher_generation).fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        app.folder.changed_tx.send(vec![dir.join("sub")]).unwrap();
        app.receive_answers(&ctx);
        app.start_listing(&ctx);
        assert!(
            !app.folder.changed_on_disk(),
            "told after the listing read it"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The watcher looks at the open folders every second, and at the tree's subfolders a
    /// thousand at a time, so in a few seconds it has looked at every one of them.
    #[test]
    fn the_watcher_looks_through_every_subfolder_in_turn() {
        let (open, all) = (2, 2 + 2_500);
        let (mut seen, mut next) = (std::collections::HashSet::new(), 0);
        for _ in 0..3 {
            let now: Vec<usize> = super::folders_this_second(open, all, next).collect();
            assert_eq!(now.len(), open + super::SUBFOLDERS_A_SECOND);
            assert!(now.starts_with(&[0, 1]));
            seen.extend(now);
            next = (next + super::SUBFOLDERS_A_SECOND) % (all - open);
        }
        assert_eq!(seen.len(), all);
        // Without subfolders, only the open folders are looked at.
        assert_eq!(super::folders_this_second(1, 1, 0).collect::<Vec<_>>(), [0]);
    }

    /// On a share the watcher's looks at the subfolders take at most `SHARE_LOOKS_A_SECOND`
    /// of each second, however many there are, and the open folders are always looked at.
    /// Elsewhere it looks at `SUBFOLDERS_A_SECOND` of them. Before, it looked at a thousand
    /// subfolders a second on a share too, which filled a share that serves one call at a time.
    #[test]
    fn the_watcher_takes_a_small_share_of_a_shares_time() {
        use std::time::Duration;
        let look_ms = 20;
        let read = |n: usize| -> crate::model::listing::FoldersRead {
            (0..n)
                .map(|k| (format!("/watched-{k}").into(), None))
                .collect()
        };
        let look = |_: &std::path::Path| {
            std::thread::sleep(Duration::from_millis(look_ms));
            Some(std::time::SystemTime::UNIX_EPOCH)
        };
        let gate =
            || crate::model::listing::ReadsFirst::new(Default::default(), Default::default(), true);
        let mut on_share = read(2 + 500);
        let (changed, looked) =
            super::look_this_second(&mut on_share, (2, 0), true, (&mut gate(), &|| true), look);
        let most = (super::SHARE_LOOKS_A_SECOND.as_millis() / look_ms as u128) as usize + 1;
        assert_eq!(changed.len(), 2 + looked);
        // The open folders' looks take none of the subfolders' time, also when they are slow.
        let slow_open = |p: &std::path::Path| {
            let open = p.ends_with("watched-0") || p.ends_with("watched-1");
            std::thread::sleep(Duration::from_millis(if open { 150 } else { look_ms }));
            None
        };
        let (_, looked) = super::look_this_second(
            &mut read(2 + 500),
            (2, 0),
            true,
            (&mut gate(), &|| true),
            slow_open,
        );
        assert!(
            looked >= 2,
            "looked at {looked} subfolders after slow open folders"
        );
        assert!((1..=most).contains(&looked), "looked at {looked}");
        assert!(
            on_share[..2].iter().all(|(_, t)| t.is_some()),
            "the open folders"
        );
        let mut local = read(2 + 1_500);
        let (_, looked) =
            super::look_this_second(&mut local, (2, 0), false, (&mut gate(), &|| true), |_| None);
        assert_eq!(looked, super::SUBFOLDERS_A_SECOND);
    }

    /// Another program, or another window's Rename Many, that swaps two names leaves the same
    /// names in the folder. The image decoded for a name is dropped when the folder is read
    /// again, as the name holds the other photo now. It stayed, and the window showed the
    /// photo that had left the name.
    #[test]
    fn a_name_another_file_has_taken_keeps_nothing_of_the_file_before() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-name-taken-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbImage::new(4, 4).save(dir.join("a.png")).unwrap();
        image::RgbImage::new(6, 6).save(dir.join("b.png")).unwrap();
        let (mut app, ctx) = app_on(&dir);
        let before = app.list.identity(0);
        assert!(before.is_some(), "the listing gave no identity");
        app.refill_preload();
        let size = |a: &crate::App| a.list.loaded_images().get(&0).map(|s| s.original_size);
        assert!(run_background_until(&mut app, &ctx, |a| size(a) == Some([4, 4])));
        for (from, to) in [("a.png", "c.png"), ("b.png", "a.png"), ("c.png", "b.png")] {
            std::fs::rename(dir.join(from), dir.join(to)).unwrap();
        }
        app.start_listing(&ctx);
        assert!(run_background_until(&mut app, &ctx, |a| a.list.identity(0) != before));
        assert_ne!(size(&app), Some([4, 4]), "a.png shows the photo it held");
        assert!(run_background_until(&mut app, &ctx, |a| size(a) == Some([6, 6])));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A slideshow of one pane at a time goes on from the image it showed last after the list
    /// is read again or put in another order. It started from the top after every change.
    #[test]
    fn a_new_list_keeps_the_last_slide() {
        let dir = std::env::temp_dir().join(format!("glimt-test-last-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a.png", "b.png", "c.png", "d.png", "e.png"] {
            image::RgbImage::new(4, 4).save(dir.join(name)).unwrap();
        }
        let (mut app, ctx) = app_on(&dir);
        assert!(run_background_until(&mut app, &ctx, |a| a
            .list
            .files()
            .len()
            == 5));
        app.config.layout = "2x1".into();
        app.config.slideshow_one_image_at_a_time = "2x1".into();
        app.start_slideshow(&ctx);
        app.refill_preload();
        assert_eq!(app.list.panes(), [1]);
        app.list.set_last_slide(3);
        let mut files = app.list.listed_names();
        files.reverse();
        let shown = app.list.files().first().cloned();
        app.replace_files(&ctx, files, shown);
        let last = &app.list.files()[app.list.last_slide()];
        assert_eq!(last, "d.png");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A folder read again selects again by name the selected files a search hides, so they come
    /// back when the search is cleared, as they do without the new list.
    #[test]
    fn a_new_list_keeps_the_selected_files_a_search_hides() {
        let dir = std::env::temp_dir().join(format!("glimt-test-held-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a.png", "b.png", "c.png"] {
            image::RgbImage::new(4, 4).save(dir.join(name)).unwrap();
        }
        let (mut app, ctx) = app_on(&dir);
        assert!(run_background_until(&mut app, &ctx, |a| a
            .list
            .files()
            .len()
            == 3));
        app.set_selection(&ctx, [0, 1].into());
        app.search.set_query("b.png");
        app.update_search(&ctx);
        assert_eq!(app.list.selected(), &[1].into());
        let files = ["0.png", "a.png", "b.png", "c.png"]
            .map(|f| (f.into(), None))
            .to_vec();
        app.take_listed_files(&ctx, files, true);
        app.search.set_query("");
        app.update_search(&ctx);
        assert_eq!(app.list.selected(), &[1, 2].into());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The sizes and dates read for the rows go from one part of a tree to the next, and to
    /// the whole list after the parts, so a slow share is not asked again for each part. A
    /// whole list read again after a whole one reads them anew.
    #[test]
    fn the_parts_of_a_tree_keep_the_sizes_read() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-part-sizes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = app_on(&dir);
        assert!(run_background_until(&mut app, &ctx, |a| !a
            .folder
            .is_listing()));
        let listed = |names: &[&str]| names.iter().map(|f| ((*f).into(), None)).collect();
        let known = |app: &crate::App| {
            app.list
                .sizes_and_dates()
                .get(std::ffi::OsStr::new("b/x.png"))
                .copied()
        };
        app.take_listed_files(&ctx, listed(&["b/x.png"]), false);
        app.list.remember_size_and_date("b/x.png".into(), (7, 1));
        app.take_listed_files(&ctx, listed(&["a/w.png"]), false);
        assert_eq!(known(&app), Some(Some((7, 1))));
        app.take_listed_files(&ctx, listed(&["a/w.png", "b/x.png", "c/y.png"]), true);
        assert_eq!(known(&app), Some(Some((7, 1))));
        app.take_listed_files(&ctx, listed(&["a/w.png", "b/x.png"]), true);
        assert_eq!(known(&app), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The next part of a tree waits for four times as long as the window took to take the
    /// part before, when that is longer than the thread's own copy of it, and the window notes
    /// that time. Before, only the thread's time counted, and in random order the window took
    /// four times as long as that for each part of a tree of 500,000 images (#256).
    #[test]
    fn the_parts_of_a_tree_wait_for_the_window() {
        let ms = std::time::Duration::from_millis;
        assert_eq!(super::next_part_after(ms(90), ms(400)), ms(1600));
        assert_eq!(super::next_part_after(ms(90), ms(20)), ms(360));
        assert_eq!(super::next_part_after(ms(1), ms(1)), ms(250));
        let dir =
            std::env::temp_dir().join(format!("glimt-test-part-pacing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        (image::RgbImage::new(2, 2))
            .save(dir.join("sub").join("a.png"))
            .unwrap();
        let (mut app, ctx) = app_on(&dir);
        app.folder.show_subfolders(true);
        app.start_listing(&ctx);
        assert!(run_background_until(&mut app, &ctx, |a| !a
            .folder
            .is_listing()));
        assert_eq!(app.list.files(), ["sub/a.png"]);
        assert!(app.folder.part_taken_in.load(super::Ordering::Relaxed) > 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A random slideshow of a tree gets the tree's images while the tree is read, and the list
    /// keeps its sort order: a part puts its images into it in that order, and the whole list
    /// after the parts keeps the image shown. Before, no part of the tree came in random order,
    /// so a slideshow of a folder with few images of its own and many in its subfolders stood
    /// still until the whole tree on a share was read (#256), and then the parts were shuffled
    /// into the list itself.
    #[test]
    fn a_random_slideshow_takes_its_tree_in_parts() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-random-parts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        for i in 0..6 {
            (image::RgbImage::new(2, 2))
                .save(dir.join("sub").join(format!("{i}.png")))
                .unwrap();
        }
        let (mut app, ctx) = app_on(&dir);
        assert!(app.list.files().is_empty());
        app.config.slideshow_subfolders = true;
        app.config.slideshow_random = true;
        app.config.slideshow_repeat = false;
        app.config.slideshow_delay = 3600;
        app.start_slideshow(&ctx);
        // The part waits in its channel, beside the whole list, until it is taken here.
        let part = (0..250).find_map(|_| match app.folder.part_rx.next() {
            super::Took::Answer(part) => Some(part.rows.listed_names()),
            _ => {
                std::thread::sleep(std::time::Duration::from_millis(20));
                None
            }
        });
        let part = part.expect("no part of the tree in random order");
        app.take_listed_files(&ctx, part, false);
        let all: Vec<std::ffi::OsString> = (0..6).map(|i| format!("sub/{i}.png").into()).collect();
        assert_eq!(
            app.list.files(),
            all,
            "every image of the part, once, in the sort order"
        );
        app.show_file(&ctx, 2);
        assert!(run_background_until(&mut app, &ctx, |a| !a
            .folder
            .is_listing()));
        assert_eq!(app.list.files(), all);
        assert_eq!(app.list.shown(), 2, "the image shown");
        assert!(app.slideshow.is_running());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A part of a tree that comes while a slideshow runs keeps the image that the slideshow
    /// planned next, decoded already, as its next slide, so that the slide needs no new decode,
    /// and Left and Right still go through the images shown. The part's images go into the list
    /// in its order, also before the images planned. Before, a part shuffled the images not
    /// shown in with the new ones, and took the next image's decode away, so that on a share the
    /// slide waited for a new one (#256).
    #[test]
    fn a_part_keeps_the_next_slide_and_the_images_shown() {
        let dir = std::env::temp_dir().join(format!("glimt-test-part-kept-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let add = |names: std::ops::Range<usize>, end: &str| {
            for i in names {
                (image::RgbImage::new(2, 2))
                    .save(dir.join(format!("{i:02}{end}.png")))
                    .unwrap();
            }
        };
        // A part of a tree, as the walk sends it: the images new to the list, in name order.
        let part = |app: &crate::App| -> Vec<super::ListedName> {
            let mut names: Vec<super::ListedName> = (std::fs::read_dir(&dir).unwrap())
                .map(|e| (e.unwrap().file_name(), None))
                .filter(|(name, _)| !app.list.files().contains(name))
                .collect();
            names.sort();
            names
        };
        add(0..20, "");
        let (mut app, ctx) = app_on(&dir);
        app.config.slideshow_subfolders = false;
        app.config.slideshow_random = true;
        app.config.slideshow_delay = 3600;
        app.start_slideshow(&ctx);
        let shown = |app: &crate::App| app.list.files()[app.list.shown()].clone();
        let tick = |app: &mut crate::App| {
            decode_all(app, &ctx);
            app.move_along(&ctx, crate::ui::actions::Action::Next);
        };
        let mut seen = vec![shown(&app)];
        for _ in 0..9 {
            tick(&mut app);
            seen.push(shown(&app));
        }
        for _ in 0..3 {
            app.move_along(&ctx, crate::ui::actions::Action::Previous);
        }
        assert_eq!(shown(&app), seen[6]);
        add(0..10, "a");
        app.take_listed_files(&ctx, part(&app), false);
        for _ in 0..3 {
            app.move_along(&ctx, crate::ui::actions::Action::Next);
        }
        assert_eq!(shown(&app), seen[9], "the images shown");
        // The next image is decoded, and a part comes right before its slide.
        let next = app.slideshow.planned_places().next().unwrap();
        let start = std::time::Instant::now();
        while !app.list.loaded_images().contains_key(&next)
            && start.elapsed() < std::time::Duration::from_secs(5)
        {
            app.receive_decoded(&ctx);
        }
        let name = app.list.files()[next].clone();
        add(10..20, "a");
        app.take_listed_files(&ctx, part(&app), false);
        assert_eq!(app.list.files().len(), 40);
        let next = app.slideshow.planned_places().next().unwrap();
        assert_eq!(app.list.files()[next], name, "the next image");
        assert!(app.list.loaded_images().contains_key(&next), "decoded");
        assert!(!app.list.decodes_pending().contains_key(&(next, false)));
        let mut sorted = app.list.files().to_vec();
        sorted.sort();
        assert_eq!(app.list.files(), sorted, "the list in its order");
        app.move_along(&ctx, crate::ui::actions::Action::Next);
        assert_eq!(shown(&app), name, "the next slide");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The parts of a slideshow's own listing of its tree hold only the images found since the
    /// part before that the list does not hold, and the window puts each into the list in its
    /// order, every image once. Before, each part was the whole tree so far, and the window
    /// looked at every name of it in random order, 0.65 s for each part of 500,000 names (#256).
    #[test]
    fn the_parts_of_a_slideshows_tree_hold_only_the_new_images() {
        let dir = std::env::temp_dir().join(format!("glimt-test-new-names-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for name in ["top.png", "a/1.png", "b/2.png"] {
            std::fs::create_dir_all(dir.join(name).parent().unwrap()).unwrap();
            (image::RgbImage::new(2, 2)).save(dir.join(name)).unwrap();
        }
        let (mut app, ctx) = app_on(&dir);
        assert_eq!(app.list.files(), ["top.png"]);
        // On a share, with an image awaited, the walk waits a second between folders, so each
        // folder comes in a part of its own.
        crate::files::file_ops::test_mount(&dir, Some("cifs"));
        (app.decoding.images_awaited()).set(vec![dir.join("awaited.png")]);
        app.config.slideshow_subfolders = true;
        app.config.slideshow_random = false;
        app.config.slideshow_delay = 3600;
        app.start_slideshow(&ctx);
        // The parts are taken as they come, and merged after, so that no image the window
        // asks for meanwhile changes the images awaited. The top folder's image is in the list
        // already, so its folder sends no part.
        let mut parts = Vec::new();
        while parts.len() < 2
            && let Some(part) = app.folder.wait_for_part()
        {
            parts.push(part);
        }
        crate::files::file_ops::test_mount(&dir, None);
        let names_of =
            |part: &[super::ListedName]| part.iter().map(|f| f.0.clone()).collect::<Vec<_>>();
        let parts_names: Vec<Vec<std::ffi::OsString>> = parts.iter().map(|p| names_of(p)).collect();
        for part in parts {
            app.take_listed_files(&ctx, part, false);
        }
        let names =
            |names: &[&str]| -> Vec<std::ffi::OsString> { names.iter().map(Into::into).collect() };
        let all = names(&["a/1.png", "b/2.png", "top.png"]);
        let one_each: Vec<Vec<std::ffi::OsString>> =
            all[..2].iter().map(|n| vec![n.clone()]).collect();
        assert_eq!(parts_names, one_each, "each part the images new to it");
        assert_eq!(app.list.files(), all, "every image once, in the sort order");
        assert!(run_background_until(&mut app, &ctx, |a| !a
            .folder
            .is_listing()));
        assert_eq!(app.list.files(), all);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Other files taken out of the list leave the shown image as it was. Its turn not saved
    /// yet and its zoom stay, also when it moves up to another place.
    #[test]
    fn removing_other_files_keeps_the_shown_image_as_it_is() {
        let dir = std::env::temp_dir().join(format!("glimt-test-remove-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a.png", "b.png", "c.png"] {
            image::RgbImage::new(4, 4).save(dir.join(name)).unwrap();
        }
        let (mut app, ctx) = app_on(&dir);
        (app.config.save_orientation, app.config.keep_zoom) = (false, false);
        app.show_file(&ctx, 2);
        app.viewing.set_turn((1, false), dir.join("c.png"));
        app.zoom.one_to_one();
        app.remove_indices(&ctx, &[0]);
        assert_eq!(app.list.files(), ["b.png", "c.png"]);
        assert_eq!(app.list.shown(), 1);
        assert_eq!(
            (app.viewing.turn(), app.zoom.is_fitted()),
            ((1, false), false)
        );
        // When the shown image itself is taken out, the next one shows afresh.
        app.remove_indices(&ctx, &[1]);
        assert_eq!((app.list.shown(), app.viewing.turn()), (0, (0, false)));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// With Compare on and a search, a delete of other files or a refresh keeps the decodes of
    /// the shown image and of the compared match. The list was refilled before the shown file's
    /// new place was set and before its search ran, and each dropped one of them.
    #[test]
    fn a_delete_in_compare_keeps_the_shown_decode() {
        let name = format!("glimt-test-remove-cmp-{}", std::process::id());
        let dir = std::env::temp_dir().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        for k in 0..12 {
            image::RgbImage::new(4, 4)
                .save(dir.join(format!("{k:02}.png")))
                .unwrap();
        }
        let (mut app, ctx) = app_on(&dir);
        app.show_file(&ctx, 5);
        app.is_comparing = true;
        app.search.set_query("re:0[59]");
        app.update_search(&ctx);
        app.refill_preload();
        let both = |a: &crate::App, x, y| {
            a.list.loaded_images().contains_key(&x) && a.list.loaded_images().contains_key(&y)
        };
        assert!(run_background_until(&mut app, &ctx, |a| both(a, 5, 9)));
        app.remove_indices(&ctx, &[0]);
        assert_eq!((app.list.shown(), app.compared_image()), (4, Some(8)));
        assert!(both(&app, 4, 8));
        // A refresh that finds a new file first moves both one place on, and keeps them too.
        image::RgbImage::new(4, 4)
            .save(dir.join("00a.png"))
            .unwrap();
        let mut files = app.list.listed_names();
        files.insert(0, ("00a.png".into(), None));
        let shown = app.list.files().get(app.list.shown()).cloned();
        app.replace_files(&ctx, files, shown);
        assert_eq!((app.list.shown(), app.compared_image()), (5, Some(9)));
        assert!(both(&app, 5, 9));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A file renamed in a collection leaves the search's matches it no longer fits. The list's
    /// names changed in place, and the listing that followed found nothing new.
    #[test]
    fn a_rename_in_a_collection_runs_the_search_again() {
        let name = format!("glimt-test-col-rename-{}", std::process::id());
        let dir = std::env::temp_dir().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("apple.png"), dir.join("banana.png"));
        image::RgbImage::new(2, 2).save(&a).unwrap();
        let file = dir.join("c.gqv");
        std::fs::write(&file, format!("#GQview collection\n\"{}\"\n", a.display())).unwrap();
        let (mut app, ctx) = app_on(&dir);
        crate::support::test_support::open_now(&mut app, &ctx, file);
        assert!(run_background_until(&mut app, &ctx, |a| !a
            .folder
            .is_listing()
            && !a.list.files().is_empty()));
        app.search.set_query("apple");
        app.update_search(&ctx);
        assert_eq!(app.list.matches().map(<[usize]>::len), Some(1));
        std::fs::rename(&a, &b).unwrap();
        app.follow_moved_paths(vec![(a, b)], crate::files::file_io::Transfer::Move);
        assert_eq!(app.list.matches().map(<[usize]>::len), Some(0));
        // The collection follows in the writer thread, which is let finish before the folder goes.
        crate::files::file_io::wait_for_queued_writes();
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A file of the folder shown is gone to without reading the folder again.
    #[test]
    fn opening_a_file_of_the_folder_shown_keeps_the_list() {
        let dir = std::env::temp_dir().join(format!("glimt-test-reopen-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a.png", "b.png"] {
            image::RgbImage::new(4, 4).save(dir.join(name)).unwrap();
        }
        let (mut app, ctx) = app_on(&dir);
        assert!(run_background_until(&mut app, &ctx, |a| !a
            .folder
            .is_listing()));
        let version = app.list.version();
        crate::support::test_support::open_now(&mut app, &ctx, dir.join("b.png"));
        assert_eq!((app.list.shown(), app.list.version()), (1, version));
        assert!(!app.folder.is_listing());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// An image moved or taken out of a collection while it is read stays so when the listing
    /// comes. Before, the listing put back the list it had copied when it started, and in a
    /// plain list a removal was lost for good.
    #[test]
    fn a_collection_edited_while_it_is_read_keeps_the_edit() {
        let d = contract_folder("collection-edit");
        let txt = d.join("list.txt");
        let text = format!(
            "{}\n{}\n",
            d.join("a.png").display(),
            d.join("b.png").display()
        );
        std::fs::write(&txt, text).unwrap();
        let (mut app, ctx) = app_on(&txt);
        let names = |app: &crate::App| -> Vec<String> {
            (app.list.files().iter())
                .map(|f| {
                    std::path::Path::new(f)
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into()
                })
                .collect()
        };
        assert_eq!(names(&app), ["a.png", "b.png"]);
        app.start_listing(&ctx);
        app.run(&ctx, crate::ui::actions::Action::MoveLast);
        assert!(run_background_until(&mut app, &ctx, |app| !app
            .folder
            .is_listing()));
        assert_eq!(names(&app), ["b.png", "a.png"]);
        app.start_listing(&ctx);
        app.remove_from_collection(&ctx);
        assert!(run_background_until(&mut app, &ctx, |app| !app
            .folder
            .is_listing()));
        assert_eq!(names(&app), ["b.png"]);
        std::fs::remove_dir_all(d).unwrap();
    }

    /// Several folders opened, one of them through a link, list its files by their real path,
    /// as when that folder is opened alone: marks and stars are kept by that name, and
    /// `--remote select`, whose client follows the links, finds them.
    #[test]
    fn several_folders_through_a_link_are_listed_by_their_real_paths() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-folders-link-{}", std::process::id()));
        for sub in ["a", "b"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
            image::RgbImage::new(4, 4)
                .save(dir.join(sub).join("x.png"))
                .unwrap();
        }
        std::os::unix::fs::symlink(dir.join("a"), dir.join("link")).unwrap();
        let (mut app, ctx) = app_on(&dir);
        app.open_folders(&ctx, vec![dir.join("link"), dir.join("b/x.png")]);
        app.receive_looked_up(&ctx, std::time::Duration::from_secs(5));
        assert!(run_background_until(&mut app, &ctx, |a| a
            .list
            .files()
            .len()
            == 2));
        let real = dir.canonicalize().unwrap();
        let want: Vec<std::path::PathBuf> = vec![real.join("a/x.png"), real.join("b/x.png")];
        let mut got: Vec<std::path::PathBuf> = (0..2).map(|i| app.path(i)).collect();
        got.sort();
        assert_eq!(got, want);
        let link = crate::model::listing::real_path(&dir.join("link/x.png"));
        let missing = app.remote_select(&ctx, &format!("select {}\0", link.display()));
        assert!(missing.is_empty(), "{}", String::from_utf8_lossy(&missing));
        // The same folder named twice, once through the link, is one folder.
        app.open_folders(&ctx, vec![dir.join("link"), dir.join("a")]);
        app.receive_looked_up(&ctx, std::time::Duration::from_secs(5));
        assert!(app.folder.folders.is_empty() && app.folder.dir == real.join("a"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Several folders given out of name order, with and without their subfolders, are one
    /// list in the order their parts go in by: byte order in the default order by name with
    /// Case, and the sort order otherwise. So the rows never move when the whole list comes after
    /// the parts. Before, each folder's images came in name order one folder after the other,
    /// in the order the folders were given, while the parts went in by byte order across them.
    #[test]
    fn several_folders_are_one_list_in_the_order_of_their_parts() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-folders-order-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for name in ["b/1.png", "b/s/2.png", "a/3.png", "a/s/4.png"] {
            std::fs::create_dir_all(dir.join(name).parent().unwrap()).unwrap();
            image::RgbImage::new(2, 2).save(dir.join(name)).unwrap();
        }
        let real = dir.canonicalize().unwrap();
        let at = real.strip_prefix("/").unwrap().to_path_buf();
        for subfolders in [false, true] {
            let (mut app, ctx) = app_on(&dir);
            app.config.show_subfolders = subfolders;
            app.open_folders(&ctx, vec![real.join("b"), real.join("a")]);
            app.receive_looked_up(&ctx, std::time::Duration::from_secs(5));
            assert!(run_background_until(&mut app, &ctx, |a| !a
                .folder
                .is_listing()));
            let mut want: Vec<OsString> = match subfolders {
                true => vec!["a/3.png", "a/s/4.png", "b/1.png", "b/s/2.png"],
                false => vec!["a/3.png", "b/1.png"],
            }
            .into_iter()
            .map(|n| at.join(n).into_os_string())
            .collect();
            assert_eq!(app.list.files(), want, "with subfolders {subfolders}");
            // Descending, the other way round.
            app.config.sort_ascending = false;
            app.folder.sort_again();
            app.start_listing(&ctx);
            assert!(run_background_until(&mut app, &ctx, |a| !a
                .folder
                .is_listing()));
            want.reverse();
            assert_eq!(
                app.list.files(),
                want,
                "descending, with subfolders {subfolders}"
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A refresh of a collection that started before a rename landed (auto refresh saw the
    /// folder change first) loses neither the image in the list nor its entry in the file.
    #[test]
    fn a_collection_refreshed_before_a_rename_landed_keeps_it() {
        let d = contract_folder("collection-race");
        let gqv = d.join("col.gqv");
        let (mut app, ctx) = app_on(&gqv);
        let (a, c) = (d.join("a.png"), d.join("c.png"));
        // Renamed on disk, the refresh reads, and only then the rename is reported.
        std::fs::rename(&a, &c).unwrap();
        app.start_listing(&ctx);
        assert!(run_background_until(&mut app, &ctx, |app| app
            .folder
            .listing_rx
            .is_none()));
        app.follow_moved_paths(vec![(a, c)], crate::files::file_io::Transfer::Move);
        let has_c = |app: &crate::App| {
            (app.list.files().iter()).any(|f| std::path::Path::new(f).ends_with("c.png"))
        };
        assert!(run_background_until(&mut app, &ctx, |app| app
            .folder
            .listing_rx
            .is_none()
            && has_c(app)));
        assert!(std::fs::read_to_string(&gqv).unwrap().contains("c.png"));
        std::fs::remove_dir_all(d).unwrap();
    }

    /// A folder in EXIF date order that is read again keeps its order, but Refresh sorts it
    /// anew, so a date changed by another program under the same name takes its place.
    #[test]
    fn refresh_sorts_an_exif_date_order_anew() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-exif-order-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a.png", "b.png"] {
            image::RgbImage::new(2, 2).save(dir.join(name)).unwrap();
        }
        let (mut app, ctx) = app_on(&dir);
        app.config.sort = crate::model::listing::Sort::ExifOriginal;
        app.start_listing(&ctx);
        assert!(run_background_until(&mut app, &ctx, |app| app
            .folder
            .listing_rx
            .is_none()));
        assert!(app.folder.listed_as.is_some());
        app.run(&ctx, crate::ui::actions::Action::Refresh);
        assert!(app.folder.listed_as.is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Refresh in a folder that has not changed decodes the image shown again, because an editor
    /// may have saved it under its own name. It asked for nothing, and the busy pointer stayed.
    #[test]
    fn refresh_decodes_the_shown_image_again() {
        let dir = std::env::temp_dir().join(format!("glimt-test-refresh-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbImage::new(4, 4).save(dir.join("a.png")).unwrap();
        let (mut app, ctx) = app_on(&dir);
        let shown = app.list.shown();
        assert!(run_background_until(&mut app, &ctx, |a| a
            .list
            .loaded_images()
            .contains_key(&shown)));
        app.run(&ctx, crate::ui::actions::Action::Refresh);
        assert!(run_background_until(&mut app, &ctx, |a| a
            .folder
            .listing_rx
            .is_none()));
        let asked = |a: &crate::App| {
            a.list.loaded_images().contains_key(&shown)
                || a.list.decodes_pending().keys().any(|k| k.0 == shown)
        };
        assert!(asked(&app), "the image shown decoded again");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Sort by name with Case on (the default) puts a collection in name order. It kept the
    /// collection's own order.
    #[test]
    fn sort_by_name_orders_a_collection() {
        let dir = std::env::temp_dir().join(format!("glimt-test-col-sort-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut gqv = String::from("#GQview collection\n");
        for name in ["b.png", "a.png"] {
            image::RgbImage::new(2, 2).save(dir.join(name)).unwrap();
            gqv.push_str(&format!("\"{}\"\n", dir.join(name).display()));
        }
        let file = dir.join("c.gqv");
        std::fs::write(&file, gqv).unwrap();
        let (mut app, ctx) = app_on(&file);
        app.config.sort = crate::model::listing::Sort::Name;
        app.config.sort_case_sensitive = true;
        app.folder.sort_collection = true;
        app.start_listing(&ctx);
        assert!(run_background_until(&mut app, &ctx, |a| a
            .folder
            .listing_rx
            .is_none()));
        let names: Vec<String> = (app.list.files().iter())
            .map(|f| {
                std::path::Path::new(f)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(names, ["a.png", "b.png"]);
        // The order is saved in the writer thread, which is let finish before the folder goes.
        crate::files::file_io::wait_for_queued_writes();
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A sort asked of one collection whose listing is still on its way is not carried to the
    /// next collection opened. It sorted that one and rewrote its file in the new order.
    #[test]
    fn a_sort_asked_of_one_collection_does_not_reach_the_next() {
        let dir = std::env::temp_dir().join(format!("glimt-test-col-leak-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let collection = |name: &str, files: &[&str]| {
            let mut gqv = String::from("#GQview collection\n");
            for f in files {
                image::RgbImage::new(2, 2).save(dir.join(f)).unwrap();
                gqv.push_str(&format!("\"{}\"\n", dir.join(f).display()));
            }
            let file = dir.join(name);
            crate::files::file_io::create_file(&file, gqv).unwrap();
            file
        };
        let first = collection("first.gqv", &["c.png", "a.png"]);
        let second = collection("second.gqv", &["d.png", "b.png"]);
        let before = std::fs::read_to_string(&second).unwrap();
        let (mut app, ctx) = app_on(&first);
        app.config.sort = crate::model::listing::Sort::Name;
        app.folder.sort_again();
        app.start_listing(&ctx);
        crate::support::test_support::open_now(&mut app, &ctx, second.clone());
        assert!(run_background_until(&mut app, &ctx, |a| !a
            .folder
            .is_listing()));
        assert_eq!(std::fs::read_to_string(&second).unwrap(), before);
        let names: Vec<&std::ffi::OsStr> = (app.list.files().iter())
            .map(|f| std::path::Path::new(f).file_name().unwrap())
            .collect();
        assert_eq!(names, ["d.png", "b.png"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A list opened by a relative name is the collection by its absolute path, as Open Recent
    /// and the last folder keep it. A window started elsewhere opened another list, or none.
    #[test]
    fn a_list_opened_by_a_relative_name_is_kept_absolute() {
        let dir = std::path::PathBuf::from(format!("glimt-test-app-list-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbImage::new(4, 4).save(dir.join("a.png")).unwrap();
        std::fs::write(dir.join("show.m3u"), "a.png\n").unwrap();
        let (app, _ctx) = app_on(&dir.join("show.m3u"));
        let here = std::env::current_dir().unwrap().join(&dir);
        assert_eq!(app.folder.collection, Some(here.join("show.m3u")));
        assert_eq!(app.list.files().len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A list that names a folder, its own folder, `..` or `/` lists only the files in it, so
    /// no folder can be deleted as an entry of the list.
    #[test]
    fn a_list_keeps_only_files() {
        let dir = std::env::temp_dir().join(format!("glimt-test-list-dirs-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        image::RgbImage::new(4, 4).save(dir.join("a.png")).unwrap();
        std::fs::write(dir.join("show.m3u"), "sub\n.\n..\n/\na.png\n").unwrap();
        let (app, _ctx) = app_on(&dir.join("show.m3u"));
        let files: Vec<&std::path::Path> = (app.list.files().iter())
            .map(std::path::Path::new)
            .collect();
        assert_eq!(files, [dir.join("a.png").strip_prefix("/").unwrap()]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A plain list lists only the files a folder would: a `.m3u` or a downloaded list that names
    /// a document or a dot file shows neither, for Select All and Delete to act on.
    #[test]
    fn a_list_shows_only_the_files_a_folder_would() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-list-types-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a.png", ".b.png"] {
            image::RgbImage::new(4, 4).save(dir.join(name)).unwrap();
        }
        std::fs::write(dir.join("thesis.odt"), "x").unwrap();
        std::fs::write(dir.join("show.m3u"), "a.png\n.b.png\nthesis.odt\n").unwrap();
        let (app, _ctx) = app_on(&dir.join("show.m3u"));
        let real = dir.canonicalize().unwrap().join("a.png");
        assert_eq!(app.list.files().len(), 1);
        assert_eq!(app.path(0), real);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A collection's or a plain list's entry named through a link to a folder is listed by its
    /// real path, the name its marks and stars are kept by and `--remote select`'s client gives.
    /// Listed as written, the entry showed none of them and the select missed it.
    #[test]
    fn collection_entries_through_a_link_are_listed_by_their_real_paths() {
        let dir = std::env::temp_dir().join(format!("glimt-test-gqv-real-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("real")).unwrap();
        image::RgbImage::new(4, 4)
            .save(dir.join("real/a.png"))
            .unwrap();
        std::os::unix::fs::symlink(dir.join("real"), dir.join("link")).unwrap();
        let entry = dir.join("link/a.png");
        std::fs::write(
            dir.join("c.gqv"),
            format!("#GQview collection\n\"{}\"\n", entry.display()),
        )
        .unwrap();
        std::fs::write(dir.join("show.m3u"), "link/a.png\n").unwrap();
        let real = dir.canonicalize().unwrap().join("real/a.png");
        for file in ["c.gqv", "show.m3u"] {
            let (mut app, ctx) = app_on(&dir.join(file));
            assert_eq!(app.list.files().len(), 1, "{file}");
            assert_eq!(app.path(0), real, "{file}");
            let missing = app.remote_select(&ctx, &format!("select {}\0", real.display()));
            assert!(missing.is_empty(), "{file}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A collection's entries through a link are named by their real paths as soon as it opens,
    /// so the image gone to before the listing comes stays shown. Named as written until then,
    /// the listing's real names were other files, and the view went back to the first image.
    #[test]
    fn a_collection_through_a_link_keeps_the_image_gone_to_when_listed() {
        let dir = std::env::temp_dir().join(format!("glimt-test-gqv-keep-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("real")).unwrap();
        for name in ["a.png", "b.png"] {
            image::RgbImage::new(4, 4)
                .save(dir.join("real").join(name))
                .unwrap();
        }
        std::os::unix::fs::symlink(dir.join("real"), dir.join("link")).unwrap();
        let link = dir.join("link");
        std::fs::write(
            dir.join("c.gqv"),
            format!(
                "#GQview collection\n\"{}\"\n\"{}\"\n",
                link.join("a.png").display(),
                link.join("b.png").display()
            ),
        )
        .unwrap();
        let (mut app, ctx) = app_on(&dir);
        crate::support::test_support::open_now(&mut app, &ctx, dir.join("c.gqv"));
        app.show_file(&ctx, 1);
        assert!(run_background_until(&mut app, &ctx, |a| !a
            .folder
            .is_listing()));
        assert_eq!(app.list.shown(), 1);
        let real = dir.canonicalize().unwrap().join("real/b.png");
        assert_eq!(app.path(1), real);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A collection's entries are looked at with one call each: those found are kept with
    /// their sizes, and those gone leave the list.
    #[test]
    fn a_collection_is_checked_with_one_call_per_entry() {
        let mut files = vec![("a.jpg".into(), None), ("gone.jpg".into(), None)];
        let mut calls = 0;
        let sizes = super::keep_files_with_size(&mut files, |f| {
            calls += 1;
            (f.0 != "gone.jpg").then_some(7)
        });
        assert_eq!(
            (files, sizes, calls),
            (vec![("a.jpg".into(), None)], vec![7], 2)
        );
    }

    /// A folder is looked up (whether it is one, and its real path) in the background, and
    /// nothing in the window changes until the answer comes. Before, the window's thread waited
    /// for the disk there, which a share that stopped answering held for ever.
    #[test]
    fn opening_waits_for_its_look_up_in_the_background() {
        let dir = std::env::temp_dir().join(format!("glimt-test-look-up-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("a")).unwrap();
        std::fs::create_dir_all(dir.join("b")).unwrap();
        std::os::unix::fs::symlink(dir.join("b"), dir.join("link")).unwrap();
        let (mut app, ctx) = app_on(&dir.join("a"));
        let a = dir.join("a").canonicalize().unwrap();
        assert_eq!(app.folder.dir(), a);
        app.open(&ctx, dir.join("link"));
        assert!(app.folder.dir() == a && app.folder.is_listing());
        let b = dir.join("b").canonicalize().unwrap();
        assert!(run_background_until(&mut app, &ctx, |app| app.folder.dir() == b));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A look-up that takes long, as on a share that stopped answering, tells that the place is
    /// still opening and how to stop waiting. Opening another place takes the notice away.
    /// Before, only the busy pointer showed that something was waited for.
    #[test]
    fn a_look_up_that_takes_long_is_told() {
        let dir = std::env::temp_dir().join(format!("glimt-test-slow-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("share")).unwrap();
        let (mut app, ctx) = app_on(&dir);
        let share = dir.join("share");
        app.open(&ctx, share.clone());
        let _held = app.folder.hold_look_up();
        app.receive_looked_up(&ctx, std::time::Duration::ZERO);
        assert!(app.notice.text().is_none(), "told at once");
        // A frame between the open and the notice forgets the repaint that the open asked for,
        // so the look in that frame asks for it again. Before, the notice came only at the
        // next input after three seconds.
        for _ in 0..3 {
            ctx.begin_pass(Default::default());
            let _ = ctx.end_pass();
        }
        ctx.begin_pass(Default::default());
        app.receive_looked_up(&ctx, std::time::Duration::ZERO);
        assert!(ctx.has_requested_repaint(), "no repaint asked for");
        let _ = ctx.end_pass();
        let long_ago = std::time::Instant::now().checked_sub(std::time::Duration::from_secs(60));
        app.folder.looking_up_since.as_mut().unwrap().0 = long_ago.unwrap();
        app.receive_looked_up(&ctx, std::time::Duration::ZERO);
        let told = app.notice.text().unwrap_or_default().to_owned();
        assert!(
            told.starts_with(&format!("Still opening {}", share.display())),
            "{told}"
        );
        app.open(&ctx, dir.clone());
        assert!(app.notice.text().is_none());
        // A warning kept under the notice leaves only itself shown when the place opens.
        // Before, the notice that the place is still opening stayed up with it.
        app.open(&ctx, share.clone());
        app.folder.looking_up_since.as_mut().unwrap().0 = long_ago.unwrap();
        app.receive_looked_up(&ctx, std::time::Duration::ZERO);
        app.notice.keep_until_drawn("a warning".into());
        app.open(&ctx, dir.clone());
        assert_eq!(app.notice.text(), Some("a warning"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Back pressed twice before the look-up of the place answers goes back one step, and puts
    /// the place it left onto Forward once. Back followed by a click on another folder leaves
    /// the history as that click makes it. Before, the history changed at each request.
    #[test]
    fn back_changes_the_history_at_the_answer() {
        let dir = std::env::temp_dir().join(format!("glimt-test-back-{}", std::process::id()));
        for sub in ["a", "b", "c"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        let (mut app, ctx) = app_on(&dir.join("a"));
        let real = |sub: &str| dir.join(sub).canonicalize().unwrap();
        let visit = |app: &mut crate::App, sub: &str| {
            app.visit(&ctx, dir.join(sub));
            app.receive_looked_up(&ctx, std::time::Duration::from_secs(5));
        };
        visit(&mut app, "b");
        visit(&mut app, "c");
        app.back_or_forward(&ctx, crate::ui::actions::Action::Back);
        app.back_or_forward(&ctx, crate::ui::actions::Action::Back);
        app.receive_looked_up(&ctx, std::time::Duration::from_secs(5));
        assert_eq!(app.folder.dir(), real("b"));
        assert_eq!(app.folder.back, [real("a")]);
        assert_eq!(app.folder.forward, [real("c")]);
        app.back_or_forward(&ctx, crate::ui::actions::Action::Back);
        app.visit(&ctx, dir.join("c"));
        app.receive_looked_up(&ctx, std::time::Duration::from_secs(5));
        assert_eq!(app.folder.dir(), real("c"));
        assert_eq!(app.folder.back, [real("a"), real("b")]);
        assert!(app.folder.forward.is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A path chosen from Open Recent or typed in the path field that is gone is told, and
    /// leaves Open Recent, without opening anything. It is looked at in the background too.
    /// It leaves the recent file as well, which other windows and the next start read. Before,
    /// it left only this window's menu, and came back after a restart.
    #[test]
    fn a_path_gone_is_told_and_leaves_open_recent() {
        use crate::files::config;
        let dir = std::env::temp_dir().join(format!("glimt-test-path-gone-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = app_on(&dir);
        let gone = dir.join("gone");
        let kept = dir.join("kept");
        let recent_file = config::recent_path().unwrap();
        crate::files::file_io::edit_shared(&recent_file, |old| {
            let all = config::add_recent(&config::parse_recent(old), &kept);
            (config::recent_text(&config::add_recent(&all, &gone)), ())
        })
        .unwrap();
        app.folder.recent = vec![gone.clone()];
        app.visit_if_there(&ctx, gone.clone(), |p| {
            format!("{} is gone", p.display()).into()
        });
        assert!(app.notice.text().is_none());
        assert!(run_background_until(&mut app, &ctx, |a| a
            .notice
            .text()
            .is_some()));
        let told = format!("{} is gone", gone.display());
        assert_eq!(app.notice.text(), Some(told.as_str()));
        assert!(app.folder.recent.is_empty() && app.folder.back.is_empty());
        assert_eq!(app.folder.dir(), dir.canonicalize().unwrap());
        crate::files::file_io::wait_for_queued_writes();
        let read = config::parse_recent(&std::fs::read(&recent_file).unwrap());
        assert!(!read.contains(&gone) && read.contains(&kept), "{read:?}");
        // The test's own entry is taken out of the file again.
        crate::files::file_io::edit_shared(&recent_file, |old| {
            let mut all = config::parse_recent(old);
            all.retain(|p| *p != kept);
            (config::recent_text(&all), ())
        })
        .unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A collection that cannot be read is told, opens nothing, and leaves Go → Back and
    /// Forward as they were. Before, the place shown went onto Back and Forward was emptied, so
    /// Back went to the place still shown.
    #[test]
    fn a_collection_that_cannot_be_read_leaves_the_history() {
        let dir = std::env::temp_dir().join(format!(
            "glimt-test-unread-collection-{}",
            std::process::id()
        ));
        // A collection larger than any file Glimt reads cannot be read, also by root, who
        // reads a file of mode 000. It is sparse, so it takes no room on the disk.
        let unreadable = dir.join("bad.gqv");
        std::fs::create_dir_all(&dir).unwrap();
        (std::fs::File::create(&unreadable).unwrap())
            .set_len(crate::images::decode::MAX_DECODE_BYTES + 1)
            .unwrap();
        let (mut app, ctx) = app_on(&dir);
        let ahead = dir.join("ahead");
        app.folder.forward = vec![ahead.clone()];
        app.visit(&ctx, unreadable.clone());
        app.receive_looked_up(&ctx, std::time::Duration::from_secs(5));
        let told = app.notice.text().unwrap_or_default().to_owned();
        assert!(
            told.starts_with(&unreadable.display().to_string()),
            "{told}"
        );
        assert!(app.folder.back.is_empty(), "{:?}", app.folder.back);
        assert_eq!(app.folder.forward, [ahead]);
        assert!(app.folder.collection().is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A share mounted after the user opened another folder is not opened over it. Before, a
    /// mount that stalled for half a minute took the window back to the share, and the files
    /// selected meanwhile were gone.
    #[test]
    fn a_late_mount_leaves_the_folder_opened_since() {
        let dir = std::env::temp_dir().join(format!("glimt-test-mount-{}", std::process::id()));
        let (here, share) = (dir.join("here"), dir.join("share"));
        std::fs::create_dir_all(&here).unwrap();
        std::fs::create_dir_all(&share).unwrap();
        let (mut app, ctx) = app_on(&dir);
        let (tx, rx) = std::sync::mpsc::channel();
        app.folder.mounting_rx = crate::support::threads::Receiving::of(rx);
        app.visit(&ctx, here.clone());
        app.receive_looked_up(&ctx, std::time::Duration::from_secs(5));
        let mounted = (Ok(share), crate::files::external::Login::default());
        // The window no longer waits for it, so the answer may find no one to take it.
        let _ = tx.send(mounted);
        app.receive_mount(&ctx);
        assert_eq!(app.folder.dir(), here.canonicalize().unwrap());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A file of the same name opened in another folder is another image, so it opens fitted,
    /// not at the zoom of the one before. The same file opened again keeps its zoom. Both are
    /// decided when the look-up's answer is applied, not when the open is asked for.
    #[test]
    fn the_same_name_in_another_folder_opens_fitted() {
        let name = format!("glimt-test-same-name-{}", std::process::id());
        let dir = std::env::temp_dir().join(name);
        for sub in ["a", "b"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
            image::RgbImage::new(4, 4)
                .save(dir.join(sub).join("x.png"))
                .unwrap();
        }
        let (mut app, ctx) = app_on(&dir.join("a/x.png"));
        app.config.keep_zoom = false;
        let visit = |app: &mut crate::App, path: std::path::PathBuf| {
            app.visit(&ctx, path);
            app.receive_looked_up(&ctx, std::time::Duration::from_secs(5));
            assert!(run_background_until(app, &ctx, |a| !a.folder.is_listing()));
        };
        app.zoom.one_to_one();
        visit(&mut app, dir.join("a/x.png"));
        assert!(!app.zoom.is_fitted(), "the same file keeps its zoom");
        visit(&mut app, dir.join("b/x.png"));
        assert!(app.folder.dir().ends_with("b"));
        assert!(app.zoom.is_fitted(), "another folder's x.png opens fitted");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// On a share the status bar sums no sizes, of the folder, its tree, the selection or the
    /// search's matches, and looks at no file for them. Each sum was a look at every file over
    /// the network, and a tree's was the tree read again after every listing, so a video player
    /// started on the share waited behind 500,000 round trips that nobody waited for.
    #[test]
    fn sizes_on_a_share_are_not_summed() {
        use crate::files::file_ops::test_mount;
        use crate::support::format::{NOT_COUNTED_ON_A_SHARE, size_text};
        let dir =
            std::env::temp_dir().join(format!("glimt-test-share-sizes-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        for name in ["a.png", "sub/b.png"] {
            image::RgbImage::new(2, 2).save(dir.join(name)).unwrap();
        }
        let dir = crate::model::listing::real_dir(&dir);
        test_mount(&dir, Some("cifs"));
        let (mut app, ctx) = app_on(&dir);
        app.config.show_subfolders = true;
        crate::support::test_support::open_now(&mut app, &ctx, dir.clone());
        assert!(run_background_until(&mut app, &ctx, |a| !a
            .folder
            .is_listing()
            && a.status_bar.folder_total().is_some()));
        assert_eq!(app.list.files(), ["a.png", "sub/b.png"]);
        assert_eq!(app.status_bar.folder_total(), Some(NOT_COUNTED_ON_A_SHARE));
        assert_eq!(
            size_text(Some(NOT_COUNTED_ON_A_SHARE)),
            "not counted on a network folder"
        );
        // The sums of the selection and of the matches look at no file either.
        let looked = std::cell::Cell::new(0);
        let sizes = std::iter::repeat_with(|| {
            looked.set(looked.get() + 1);
            1
        })
        .take(3);
        let (tx, rx) = std::sync::mpsc::channel();
        super::send_sizes_unless_on_a_share(&app.folder.places(), true, sizes, tx, &ctx);
        assert_eq!(rx.iter().collect::<Vec<_>>(), [NOT_COUNTED_ON_A_SHARE]);
        assert_eq!(looked.get(), 0);
        let paths = [dir.join("a.png")];
        let total = super::total_size_unless_on_a_share(
            &app.folder.places(),
            true,
            &paths,
            (&Default::default(), &|| true),
        );
        assert_eq!(total, NOT_COUNTED_ON_A_SHARE);
        // On a disk of its own they are summed.
        test_mount(&dir, None);
        assert!(
            super::total_size_unless_on_a_share(
                &app.folder.places(),
                true,
                &paths,
                (&Default::default(), &|| true)
            ) > 0
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A share mounted in a folder of a tree on a disk of its own, such as a NAS at
    /// ~/Pictures/nas, has its tree's sizes not summed either, while the folder alone, without
    /// its subfolders, is summed. Before, only the top folder was looked at, and the sum read
    /// the whole share again after each listing.
    #[test]
    fn a_share_mounted_inside_the_tree_is_not_summed() {
        use crate::files::file_ops::test_mount;
        use crate::support::format::NOT_COUNTED_ON_A_SHARE;
        let dir =
            std::env::temp_dir().join(format!("glimt-test-share-inside-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("nas")).unwrap();
        for name in ["a.png", "nas/b.png"] {
            image::RgbImage::new(2, 2).save(dir.join(name)).unwrap();
        }
        let dir = crate::model::listing::real_dir(&dir);
        test_mount(&dir.join("nas"), Some("cifs"));
        let (mut app, ctx) = app_on(&dir);
        app.config.show_subfolders = true;
        crate::support::test_support::open_now(&mut app, &ctx, dir.clone());
        assert!(run_background_until(&mut app, &ctx, |a| !a
            .folder
            .is_listing()
            && a.status_bar.folder_total().is_some()));
        assert_eq!(app.list.files(), ["a.png", "nas/b.png"]);
        assert_eq!(app.status_bar.folder_total(), Some(NOT_COUNTED_ON_A_SHARE));
        let (places, paths) = (app.folder.places(), [dir.join("a.png")]);
        let total = super::total_size_unless_on_a_share(
            &places,
            true,
            &paths,
            (&Default::default(), &|| true),
        );
        assert_eq!(total, NOT_COUNTED_ON_A_SHARE);
        assert!(
            super::total_size_unless_on_a_share(
                &places,
                false,
                &paths,
                (&Default::default(), &|| true)
            ) > 0
        );
        test_mount(&dir.join("nas"), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A collection's list is under "/", which no walk reads as a tree, also with View →
    /// Subfolders on. So a share mounted anywhere, such as GNOME's gvfs folder, leaves the sums
    /// of its selection and matches alone. Before, "/" was asked about every mount below it, and
    /// the sizes of local files were said to be on a network folder.
    #[test]
    fn a_collection_with_subfolders_on_is_not_taken_for_a_tree() {
        use crate::files::file_ops::test_mount;
        let dir = std::env::temp_dir().join(format!(
            "glimt-test-share-collection-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(dir.join("nas")).unwrap();
        image::RgbImage::new(2, 2).save(dir.join("a.png")).unwrap();
        let dir = crate::model::listing::real_dir(&dir);
        let collection = dir.join("c.gqv");
        let entry = format!("#GQview collection\n\"{}\"\n", dir.join("a.png").display());
        std::fs::write(&collection, entry).unwrap();
        test_mount(&dir.join("nas"), Some("cifs"));
        let (mut app, ctx) = app_on(&dir);
        crate::support::test_support::open_now(&mut app, &ctx, collection);
        assert!(run_background_until(&mut app, &ctx, |a| !a
            .folder
            .is_listing()));
        assert!(app.folder.collection().is_some());
        app.folder.show_subfolders(true);
        assert!(!app.folder.walks_subfolders());
        let (places, paths) = (app.folder.places(), [dir.join("a.png")]);
        let total = super::total_size_unless_on_a_share(
            &places,
            app.folder.walks_subfolders(),
            &paths,
            (&Default::default(), &|| true),
        );
        assert!(total > 0 && total != crate::support::format::NOT_COUNTED_ON_A_SHARE);
        test_mount(&dir.join("nas"), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A tree opened with View → Subfolders gets each part into the list in the order chosen,
    /// as its whole list comes, also by date and descending, where each image goes in by what
    /// the listing thread looked at of it, among what the list keeps of its own images, and by
    /// name without Case and by number. Before, a
    /// part was merged by its bytes in every order, so in an order by date the tree's images
    /// were in the wrong places until the whole tree was read.
    #[test]
    fn the_parts_of_a_tree_go_into_the_list_in_its_order() {
        use crate::model::listing::Sort;
        let dir =
            std::env::temp_dir().join(format!("glimt-test-part-order-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let names = [
            "c.png", "B.png", "a/1.png", "a/10.png", "a/2.png", "b/x.png", "b/Y.png",
        ];
        for (k, name) in names.iter().enumerate() {
            std::fs::create_dir_all(dir.join(name).parent().unwrap()).unwrap();
            image::RgbImage::new(2, 2).save(dir.join(name)).unwrap();
            let when =
                std::time::UNIX_EPOCH + std::time::Duration::from_secs(9_000 - k as u64 * 7 % 5);
            let file = std::fs::File::options().write(true).open(dir.join(name));
            file.unwrap().set_modified(when).unwrap();
        }
        let (mut app, ctx) = app_on(&dir);
        let orders = [
            (Sort::Date, true, true),
            (Sort::Date, false, true),
            (Sort::Name, true, false),
            (Sort::Number, false, true),
        ];
        for order in orders {
            let c = &mut app.config;
            (c.sort, c.sort_ascending, c.sort_case_sensitive) = order;
            // A folder opened, with its tree.
            app.replace_files(&ctx, Vec::new(), None);
            app.folder.show_subfolders(true);
            app.start_listing(&ctx);
            let mut fed = feed(&mut app);
            let mut parts = 0;
            while send_part(&mut app, &ctx, &mut fed) {
                let files: Vec<&OsString> = app.list.files().iter().collect();
                assert_in_order(&dir, &files, order, &format!("part {parts}"));
                parts += 1;
            }
            assert!(
                parts > 2 && app.list.files().len() == names.len(),
                "{order:?}"
            );
            let before = app.list.files().to_vec();
            send_whole(&mut app, &ctx, &fed);
            assert_eq!(
                app.list.files(),
                before,
                "the whole list as the parts: {order:?}"
            );
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// In an order by date, the listing of a tree looks at each image once: a part looks at its
    /// new images, a part that could not be sent keeps what it looked at for the next one, and
    /// the whole list is sorted by what the parts looked at, so there only the images that the
    /// list held before are looked at. The window puts each part into the list by what the list
    /// keeps of its own images, with no look at a file, here into a list already in that order.
    /// Before, the listing thread looked at each new image three times, when its part was sorted,
    /// when it was placed among a copy of the list's names and when the whole list was sorted,
    /// and once more after a part could not be sent.
    #[test]
    fn a_tree_in_date_order_looks_at_each_image_once() {
        use crate::model::listing::{FILES_LOOKED_AT, Sort};
        let dir =
            std::env::temp_dir().join(format!("glimt-test-looked-once-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // The dates go the other way to the names, so a part put in by its names would be in
        // the wrong place.
        let names: Vec<String> = (["x.png", "y.png"].map(String::from).into_iter())
            .chain((0..12).map(|k| format!("d{}/{k:02}.png", k % 3)))
            .collect();
        for (k, name) in names.iter().enumerate() {
            std::fs::create_dir_all(dir.join(name).parent().unwrap()).unwrap();
            image::RgbImage::new(2, 2).save(dir.join(name)).unwrap();
            let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(9_000 - k as u64);
            let file = std::fs::File::options().write(true).open(dir.join(name));
            file.unwrap().set_modified(when).unwrap();
        }
        let (mut app, ctx) = app_on(&dir);
        let order = (Sort::Date, true, true);
        let c = &mut app.config;
        (c.sort, c.sort_ascending, c.sort_case_sensitive) = order;
        // The folder's own images, in that order, and then its tree.
        app.start_listing(&ctx);
        assert!(run_background_until(&mut app, &ctx, |a| !a
            .folder
            .is_listing()));
        assert_eq!(app.list.files(), ["y.png", "x.png"].map(OsString::from));
        app.folder.show_subfolders(true);
        app.start_listing(&ctx);
        let mut fed = feed(&mut app);
        FILES_LOOKED_AT.set(0);
        let mut parts = 0;
        while send_part(&mut app, &ctx, &mut fed) {
            // The test's own looks are not counted.
            let looks = FILES_LOOKED_AT.get();
            let files: Vec<&OsString> = app.list.files().iter().collect();
            assert_in_order(&dir, &files, order, &format!("part {parts}"));
            FILES_LOOKED_AT.set(looks);
            parts += 1;
            if parts == 2 {
                // A part that could not be sent, as the window had not taken the one before:
                // its images and the folder it read go with the next one.
                let Some(Ok((rel, _, files, _))) = fed.walk.next() else {
                    panic!("no folder left to read");
                };
                let images = files.into_iter().map(|(n, id)| (rel.join(n).into(), id));
                fed.so_far.extend(images);
                let dropped = fed.sent.next_part(&fed.so_far).expect("a part");
                fed.sent.read_whole = dropped.read;
            }
        }
        assert_eq!(app.list.files().len(), names.len());
        assert_eq!(FILES_LOOKED_AT.get(), names.len() - 2, "the parts' looks");
        let known = fed.sent.take_keys();
        let whole = fed.sent.sorted(fed.so_far.clone(), known);
        assert_eq!(
            FILES_LOOKED_AT.get(),
            names.len(),
            "one look for each image"
        );
        assert_eq!(
            app.list.files(),
            whole.files(),
            "the parts in the order of the whole list"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Show Hidden Files turned off takes the hidden images out of the list at once, also those
    /// in a hidden folder of a tree, before the tree is read again, and the Types menu takes out
    /// the images of a type it no longer lists. Before, they stayed until the whole list came,
    /// minutes later for a tree on a share, and a slideshow went on showing the images just
    /// hidden.
    #[test]
    fn images_no_longer_shown_leave_the_list_at_once() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-hidden-out-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for name in ["a.png", ".b.png", ".h/c.png", "s/d.png"] {
            std::fs::create_dir_all(dir.join(name).parent().unwrap()).unwrap();
            image::RgbImage::new(2, 2).save(dir.join(name)).unwrap();
        }
        std::fs::write(dir.join("s/e.raf"), "raw").unwrap();
        let (mut app, ctx) = app_on(&dir);
        app.config.show_hidden = true;
        app.config.list_raw = true;
        app.folder.show_subfolders(true);
        app.start_listing(&ctx);
        assert!(run_background_until(&mut app, &ctx, |a| !a
            .folder
            .is_listing()));
        let names = |names: &[&str]| -> Vec<OsString> { names.iter().map(Into::into).collect() };
        let all = names(&[".b.png", ".h/c.png", "a.png", "s/d.png", "s/e.raf"]);
        assert_eq!(app.list.files(), all);
        app.toggle_hidden_files(&ctx);
        assert!(app.folder.is_listing(), "the tree is read again");
        assert_eq!(
            app.list.files(),
            names(&["a.png", "s/d.png", "s/e.raf"]),
            "at once"
        );
        app.config.list_raw = false;
        app.leave_out_names_not_shown(&ctx);
        assert_eq!(app.list.files(), names(&["a.png", "s/d.png"]));
        assert!(run_background_until(&mut app, &ctx, |a| !a
            .folder
            .is_listing()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// On a share, in an order that looks at the files, a part's looks at its images wait for
    /// the image that the window waits for, before each look and not only before the folder,
    /// and a hundred looks wait for it at most `WALK_WAITS_FOR_IMAGES_AT_MOST` in all, so the
    /// walk never stops for good. On a disk of its own they never wait. Before, a part looked at
    /// the new images of every folder since the part before in a row, 600 to 900 EXIF reads
    /// after one wait, and the image awaited queued behind them (#256).
    #[test]
    fn a_parts_looks_wait_for_the_image_awaited() {
        use std::time::{Duration, Instant};
        let dir =
            std::env::temp_dir().join(format!("glimt-test-looks-wait-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("s")).unwrap();
        image::RgbImage::new(2, 2).save(dir.join("a.png")).unwrap();
        let (mut app, ctx) = app_on(&dir);
        app.config.sort = crate::model::listing::Sort::Date;
        app.folder.show_subfolders(true);
        // The list in Date order, whose parts go in by the files' dates.
        app.start_listing(&ctx);
        assert!(run_background_until(&mut app, &ctx, |a| !a
            .folder
            .is_listing()));
        // A part of 40 new images, found in a folder after the walk's wait before it.
        let part_of = |fed: &mut FedListing, from: usize| {
            fed.so_far = (0..from + 40)
                .map(|i| (format!("s/{i}.png").into(), None))
                .collect();
            fed.sent.up_to = from;
            let began = Instant::now();
            assert!(fed.sent.next_part(&fed.so_far).is_some());
            began.elapsed()
        };
        let awaited = app.decoding.images_awaited().clone();
        awaited.set(vec![dir.join("a.png")]);
        app.start_listing(&ctx);
        let mut fed = feed(&mut app);
        let took = part_of(&mut fed, 0);
        assert!(
            took < Duration::from_millis(200),
            "a disk of its own: {took:?}"
        );
        crate::files::file_ops::test_mount(&dir, Some("cifs"));
        app.start_listing(&ctx);
        let mut fed = feed(&mut app);
        // The image became awaited after the walk's wait before the folder, and is decoded while
        // the part looks at its images.
        awaited.set(vec![dir.join("a.png")]);
        let decoded = awaited.clone();
        let later = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(400));
            decoded.set(Vec::new());
        });
        let took = part_of(&mut fed, 0);
        later.join().unwrap();
        assert!(
            took >= Duration::from_millis(350),
            "the looks waited: {took:?}"
        );
        // An image awaited all along holds a part's 40 looks for a second in all, not for each,
        // as they are fewer than the hundred looks that share one budget.
        app.start_listing(&ctx);
        let mut fed = feed(&mut app);
        awaited.set(vec![dir.join("a.png")]);
        let took = part_of(&mut fed, 0);
        assert!(
            took >= Duration::from_millis(900) && took < Duration::from_millis(2500),
            "a second in all: {took:?}"
        );
        crate::files::file_ops::test_mount(&dir, None);
        awaited.set(Vec::new());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// On a share, in an order that looks at the files, the looks that put the list's rows in
    /// another order after a sort change, and the looks of the sort of the whole list, wait for
    /// the image that the window waits for, as a part's looks do: up to a second in every
    /// hundred looks, so that a long run of them pauses again and again and never stops for
    /// good. Before, the list put in another order shared one second with the part's own looks,
    /// and the sort of the whole list never waited, so on a share minutes of EXIF reads went
    /// before the slideshow's next image.
    #[test]
    fn every_look_of_a_walk_waits_for_the_image_awaited() {
        use std::time::{Duration, Instant};
        let dir =
            std::env::temp_dir().join(format!("glimt-test-rekey-waits-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("s")).unwrap();
        image::RgbImage::new(2, 2).save(dir.join("a.png")).unwrap();
        let (mut app, ctx) = app_on(&dir);
        app.config.sort = crate::model::listing::Sort::Date;
        app.folder.show_subfolders(true);
        app.start_listing(&ctx);
        assert!(run_background_until(&mut app, &ctx, |a| !a
            .folder
            .is_listing()));
        crate::files::file_ops::test_mount(&dir, Some("cifs"));
        let awaited = app.decoding.images_awaited().clone();
        let names = |prefix: &str, n: usize| -> Vec<super::ListedName> {
            (0..n)
                .map(|i| (format!("{prefix}/{i}.png").into(), None))
                .collect()
        };
        // The list's 250 names put in the listing's order with the first part: three budgets.
        app.start_listing(&ctx);
        let mut fed = feed(&mut app);
        fed.sent.to_sort_anew = names("s", 250);
        awaited.set(vec![dir.join("a.png")]);
        let began = Instant::now();
        assert!(fed.sent.next_part(&[]).is_some());
        let took = began.elapsed();
        assert!(
            took >= Duration::from_millis(2700) && took < Duration::from_millis(6000),
            "the list put in another order paused in each hundred looks: {took:?}"
        );
        // The sort of a whole list of 150 names: two budgets. The image is awaited anew, as one
        // awaited for longer than `IMAGE_AWAITED_FOR` no longer holds the walk.
        app.start_listing(&ctx);
        let mut fed = feed(&mut app);
        awaited.set(Vec::new());
        awaited.set(vec![dir.join("a.png")]);
        let began = Instant::now();
        let whole = fed.sent.sorted(names("w", 150), Vec::new());
        let took = began.elapsed();
        assert_eq!(whole.len(), 150);
        assert!(
            took >= Duration::from_millis(1800) && took < Duration::from_millis(4500),
            "the whole list's sort paused in each hundred looks: {took:?}"
        );
        crate::files::file_ops::test_mount(&dir, None);
        awaited.set(Vec::new());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A rename moves the renamed row to its place in the list's order, by name in Name order
    /// and in an EXIF order among images with no EXIF date, so that the parts of a tree read
    /// after it go in at their places, and the image shown and the selection stay on the renamed
    /// file. Before, the row kept its place under its new name, and a later part was placed by a
    /// binary search over rows out of order.
    #[test]
    fn a_rename_keeps_the_rows_in_their_order() {
        use crate::model::listing::Sort;
        for order in [(Sort::Name, true, true), (Sort::ExifOriginal, true, true)] {
            let dir = std::env::temp_dir().join(format!(
                "glimt-test-rename-order-{:?}-{}",
                order.0,
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            for name in ["a.png", "b.png", "c.png", "d.png", "m/1.png", "m/2.png"] {
                std::fs::create_dir_all(dir.join(name).parent().unwrap()).unwrap();
                image::RgbImage::new(2, 2).save(dir.join(name)).unwrap();
            }
            let (mut app, ctx) = app_on(&dir);
            let c = &mut app.config;
            (c.sort, c.sort_ascending, c.sort_case_sensitive) = order;
            app.start_listing(&ctx);
            assert!(run_background_until(&mut app, &ctx, |a| !a
                .folder
                .is_listing()));
            // The tree is read a folder at a time, and the folder m is not read yet.
            app.folder.show_subfolders(true);
            app.start_listing(&ctx);
            let mut fed = feed(&mut app);
            assert!(send_part(&mut app, &ctx, &mut fed));
            app.show_file(&ctx, 0);
            app.set_selection(&ctx, [0, 2].into());
            std::fs::rename(dir.join("a.png"), dir.join("z.png")).unwrap();
            app.rename_in_list(&ctx, vec![(0, "z.png".into())]);
            let at = format!("after the rename, {order:?}");
            let files: Vec<&OsString> = app.list.files().iter().collect();
            assert_in_order(&dir, &files, order, &at);
            assert_eq!(app.list.rows().order(), Some(order), "{at}");
            let name = |i: usize| app.list.files()[i].to_str().unwrap().to_owned();
            assert_eq!(name(app.list.shown()), "z.png", "the image shown: {at}");
            let selected: Vec<String> = app.list.selected().iter().map(|&i| name(i)).collect();
            assert_eq!(selected, ["c.png", "z.png"], "the selection: {at}");
            while send_part(&mut app, &ctx, &mut fed) {}
            let names = ["b.png", "c.png", "d.png", "m/1.png", "m/2.png", "z.png"];
            assert_eq!(
                app.list.files(),
                names.map(OsString::from),
                "the folder read after the rename: {order:?}"
            );
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }

    /// The listing that `start_listing` started, read by the test a folder at a time in place of
    /// its thread, so that its parts and its whole list come between the other events in an
    /// order that the test chooses.
    struct FedListing {
        listing: std::sync::mpsc::Sender<super::Listed>,
        parts: std::sync::mpsc::SyncSender<super::TreePart>,
        /// What its parts sent, with the names that the list held when the listing started,
        /// as the listing thread holds it.
        sent: super::PartsSent,
        /// The images that the walk found so far.
        so_far: Vec<super::ListedName>,
        /// The walk of the tree, the listing thread's own (`walk_of`), a folder at each part.
        walk: Walk,
        /// The listing's stop flag, which another listing replaces.
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    use std::collections::HashSet;
    use std::ffi::OsString;
    use std::path::PathBuf;

    /// A walk of a tree, a folder at a time.
    type Walk =
        Box<dyn Iterator<Item = Result<crate::model::listing::WalkedFolder, super::Unread>>>;

    /// The walk of the folder shown as the listing thread walks it (`listing::walk`), with its
    /// links followed inside the tree, each folder read once by its file id and the hidden ones
    /// left out, except that a folder named `locked` cannot be read, as one without permission
    /// on a share (the tests run as root, which reads every folder).
    fn walk_of(app: &crate::App) -> Walk {
        let list = |path: &std::path::Path, types| match path.file_name() == Some("locked".as_ref())
        {
            true => Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
            false => super::list_folder(path, types),
        };
        Box::new(crate::model::listing::walk_by(
            app.folder.dir(),
            app.shown_types(),
            crate::images::save::file_id,
            |p| std::fs::canonicalize(p),
            list,
        ))
    }

    /// Returns the images that the tree on disk holds as `walk_of` reads it, by their names in
    /// the list, and the folders it could not read.
    fn tree_on_disk(app: &crate::App) -> (Vec<super::ListedName>, Vec<super::Unread>) {
        let (mut files, mut left_out) = (Vec::new(), Vec::new());
        for walked in walk_of(app) {
            match walked {
                Ok((rel, _, images, _)) => files
                    .extend((images.into_iter()).map(|(n, id)| (rel.join(n).into_os_string(), id))),
                Err(unread) => left_out.push(unread),
            }
        }
        (files, left_out)
    }

    /// Asserts that the list holds every image of the tree on disk and no other name, each
    /// once.
    fn assert_list_holds_the_tree(app: &crate::App, at: &str) {
        let disk: HashSet<OsString> = tree_on_disk(app).0.into_iter().map(|f| f.0).collect();
        let list: HashSet<&OsString> = app.list.files().iter().collect();
        assert_eq!(list.len(), app.list.files().len(), "a name twice: {at}");
        let lacks: Vec<&OsString> = disk.iter().filter(|f| !list.contains(f)).collect();
        let gone: Vec<&&OsString> = list.iter().filter(|f| !disk.contains(**f)).collect();
        assert!(
            lacks.is_empty() && gone.is_empty(),
            "lacks {lacks:?}, gone {gone:?}: {at}"
        );
    }

    /// Takes the listing that runs from its thread, which stops, and feeds it from the test.
    /// It is a listing of a tree, so it sends parts, in the listing's order. What it sends of
    /// them is made as the listing's thread makes it (`App::parts_from`, `PartsFrom::parts_sent`),
    /// from the list as it is when the listing has just started.
    fn feed(app: &mut crate::App) -> FedListing {
        assert!(
            app.folder.listing_sends_parts,
            "a listing of a tree that sends no parts"
        );
        let c = &app.config;
        let order = (c.sort, c.sort_ascending, c.sort_case_sensitive);
        // The thread's own stop is set below, and the test's parts wait on.
        let sent = app
            .parts_from(order, std::sync::Arc::default())
            .parts_sent();
        app.folder
            .listing_stop
            .store(true, super::Ordering::Relaxed);
        let (listing, rx) = std::sync::mpsc::channel();
        let (parts, part_rx) = std::sync::mpsc::sync_channel(64);
        app.folder.listing_rx = super::Receiving::of(rx);
        app.folder.part_rx = super::Receiving::of(part_rx);
        FedListing {
            listing,
            parts,
            sent,
            so_far: Vec::new(),
            walk: walk_of(app),
            stop: app.folder.listing_stop.clone(),
        }
    }

    /// Feeds a listing that started since `fed` was made, after the window's frame has started
    /// the one that a change asked for (`relist_later`). A listing that no longer runs leaves
    /// none to feed.
    fn feed_what_runs(
        app: &mut crate::App,
        ctx: &eframe::egui::Context,
        fed: &mut Option<FedListing>,
    ) {
        if app.folder.wants_relist() && !app.folder.is_listing() {
            app.start_listing(ctx);
        }
        let same = (fed.as_ref())
            .is_some_and(|f| std::sync::Arc::ptr_eq(&f.stop, &app.folder.listing_stop));
        if !app.folder.is_listing() {
            *fed = None;
        } else if !same {
            *fed = Some(feed(app));
        }
    }

    /// Reads the next folder of the fed listing's walk and sends the images in it that are new
    /// to the list as a part, with the folders read whole where a name may be gone, as the
    /// listing thread does, and the window takes it. A folder that cannot be read is left out,
    /// as `list_tree` leaves it out. Afterwards the list holds every image of that folder, and
    /// no name of it that it lacks now, nor one below a folder of it that is gone, judged by
    /// the walk's rules: a hidden folder the walk reached through a link is not gone from its
    /// parent. Returns false when the walk has read every folder.
    fn send_part(app: &mut crate::App, ctx: &eframe::egui::Context, fed: &mut FedListing) -> bool {
        let (rel, dirs, files) = loop {
            match fed.walk.next() {
                None => return false,
                Some(Ok((rel, dirs, files, _))) => break (rel, dirs, files),
                Some(Err(_)) => continue,
            }
        };
        let types = app.shown_types();
        let first = fed.so_far.len();
        let images: Vec<OsString> = (files.into_iter())
            .map(|(name, _)| rel.join(name).into_os_string())
            .collect();
        (fed.so_far).extend(images.iter().map(|name| (name.clone(), None)));
        let read = crate::model::listing::FolderReadWhole {
            so_far: &fed.so_far,
            first,
            folder: &rel,
            dirs: &dirs,
            types,
        };
        fed.sent
            .listed_before
            .note_folder(&read, &mut fed.sent.read_whole);
        if let Some(part) = fed.sent.next_part(&fed.so_far) {
            fed.parts.send(part).unwrap();
            fed.sent.part_sent(fed.so_far.len());
        }
        app.receive_listing(ctx, std::time::Duration::ZERO);
        // Nothing of the folder read is in the list that the folder no longer holds, by the tree
        // on disk: a folder below it is there when the walk of the tree reaches it.
        let subfolders: Vec<PathBuf> = (dirs.iter())
            .filter_map(|d| d.file_name())
            .map(|n| rel.join(n))
            .collect();
        let walked: HashSet<PathBuf> = (walk_of(app).flatten()).map(|(rel, ..)| rel).collect();
        for name in app.list.files() {
            let Ok(below) = std::path::Path::new(name).strip_prefix(&rel) else {
                continue;
            };
            let mut steps = below.components();
            let Some(step) = steps.next() else {
                continue;
            };
            let step = step.as_os_str();
            let there = match steps.next() {
                None => images.contains(name),
                // A hidden folder that its parent's read leaves out is not judged by it: only its
                // own read through a link, or the whole list, tells which of its names are gone.
                Some(_) => {
                    let folder = app.folder.dir().join(&rel).join(step);
                    let hidden_there = step.as_encoded_bytes().starts_with(b".")
                        && crate::files::file_io::is_surely_dir(&folder);
                    subfolders.contains(&rel.join(step))
                        || walked.contains(&rel.join(step))
                        || hidden_there
                }
            };
            assert!(there, "{name:?} is gone from {rel:?}, which was read whole");
        }
        // And every image it holds is in the list.
        let lacks: Vec<&OsString> = (images.iter())
            .filter(|f| !app.list.files().contains(f))
            .collect();
        assert!(
            lacks.is_empty(),
            "{lacks:?} of {rel:?}, read whole, not in the list"
        );
        true
    }

    /// Sends the fed listing's whole list, the tree as it is now in the sort order, with the
    /// folders it could not read, and the window takes it.
    fn send_whole(app: &mut crate::App, ctx: &eframe::egui::Context, fed: &FedListing) {
        let dir = app.folder.dir().to_path_buf();
        let (files, left_out) = tree_on_disk(app);
        let c = &app.config;
        let order = (c.sort, c.sort_ascending, c.sort_case_sensitive);
        let key = |_, name: &OsString| sort_key_of(&dir, name, order.0);
        let rows = super::Rows::sorted(files, order, key);
        let listed = super::Listed {
            dirs: Vec::new(),
            files: Ok(super::ListedRows::Rows(rows)),
            left_out,
            read: Vec::new(),
            read_in: std::time::Duration::ZERO,
        };
        fed.listing.send(listed).unwrap();
        app.receive_listing(ctx, std::time::Duration::ZERO);
    }

    /// Takes the decodes in flight, so that the slideshow's next step does not wait for one.
    fn decode_all(app: &mut crate::App, ctx: &eframe::egui::Context) {
        let start = std::time::Instant::now();
        while !app.list.decodes_pending().is_empty()
            && start.elapsed() < std::time::Duration::from_secs(5)
        {
            app.receive_decoded(ctx);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    /// A slideshow of a tree never changes the order of the file list, whatever comes in
    /// whatever order: parts of its walk, whole lists, F5, a delete, a move and a rename of a
    /// file and of a subfolder, another sort order (by name with and without Case, descending,
    /// by number, and each order that looks at the files) also in the middle of a walk, Show
    /// Hidden Files on and off, R on and off, Left, Right, Home and End, also with Shift, its own
    /// steps, an image shown by hand, a search narrowed, widened and cleared, Repeat on and off,
    /// a stop and a start, one image opened in an order that looks at the files and then its
    /// tree read (also as `--slideshow photo.jpg` with Include Subfolders), a sidecar's
    /// relisting in each order that looks at the files, with a file deleted between its start
    /// and its whole list, which in an EXIF date order keeps the list's order, and one to three
    /// names listed in such an order before another one is chosen and the parts come in it,
    /// with a file moved away before the first part. Images picked with Ctrl+click and then an
    /// arrow key, Home or End, with or without Shift, leave the image on screen as an action's
    /// only target, a stop on an image of one of several folders given goes back to their own
    /// images, and a whole list that comes after a stop keeps the image stopped on. The tree has
    /// a link into a hidden folder (hidden files not shown at first), a folder reached by two
    /// paths and a folder that cannot be read, its images have dates in another order than their
    /// names, and the walk is the listing thread's own. After each event no name is twice in the
    /// list, the list is in the order that its rows hold, every row carries a key of that order
    /// in an order that looks at the files, and none is gone from the list that the tree on disk
    /// still holds. After each part the list is in the listing's order, which the first part
    /// after another order was chosen puts it in, and after each whole list and each stop the
    /// list is the tree on disk in its sort order. Every listing of the tree sends parts, so the
    /// list grows while it is read, also a `--slideshow` start with several folders of the tree.
    /// The slideshow shows no image twice in a round, and neither shows nor plans an image that
    /// the search hides. Without Repeat it ends at the step after every image the search shows
    /// was shown, and not before, and with Repeat the next round begins only then. No tick holds
    /// the slideshow. After R off, and in order after an image shown by hand, the next image is
    /// the next row after the one shown that the round has not shown, Left goes back through the
    /// images shown and Right forward again, and a stop keeps the image shown, scrolled to in the
    /// list. The events come from a seeded xorshift, in both orders, with Repeat on and off at
    /// the start. Before, the slideshow put its own order into the list and merged every listing
    /// into it, and four rounds of audits found images lost from the list or shown twice there.
    #[test]
    fn a_slideshow_keeps_its_order_through_every_listing() {
        for seed in [0x9e37_79b9_u64, 0x85eb_ca6b, 0xc2b2_ae35] {
            for random in [false, true] {
                for repeat in [false, true] {
                    play_a_tree_through_events(seed, random, repeat);
                }
            }
        }
    }

    /// What the test knows of the slideshow of `play_a_tree_through_events`, from what it showed:
    /// the names it showed in this round, the names it showed, the oldest first, and how many
    /// steps Left has gone back from the newest one.
    #[derive(Default)]
    struct SlidesSeen {
        round: HashSet<OsString>,
        history: Vec<OsString>,
        back: usize,
    }

    impl SlidesSeen {
        /// A round begins with `on_screen` shown, and `on_screen` is the newest in the history.
        fn begin(&mut self, on_screen: OsString) {
            self.round = [on_screen.clone()].into();
            self.history.push(on_screen);
            self.back = 0;
        }

        /// The image the history shows `back` steps back, past the images the list `files` no
        /// longer holds, or `None` when the history has none there.
        fn step(&self, files: &[OsString], older: bool) -> Option<(usize, OsString)> {
            let mut back = self.back;
            loop {
                back = match older {
                    true => back + 1,
                    false => back.checked_sub(1)?,
                };
                let name = &self.history[self.history.len().checked_sub(1 + back)?];
                if files.contains(name) {
                    return Some((back, name.clone()));
                }
            }
        }

        /// Notes the image on screen in `app` as shown in this round, as the newest of the
        /// history, when it is not `same_image` and so came into the first pane in another way
        /// than by the slideshow: the image shown was deleted or left the list, and the next one
        /// took its place. The slideshow notes it too (`follow_list_in_slideshow`). Then
        /// `same_image` is that image.
        fn note_shown_elsewhere(&mut self, app: &crate::App, same_image: &mut Option<OsString>) {
            let now = app.list.files().get(app.list.shown()).cloned();
            if app.slideshow.is_running() && now.is_some() && now != *same_image {
                self.round.extend(now.clone());
                self.history.extend(now.clone());
                self.back = 0;
                *same_image = now;
            }
        }
    }

    /// Returns what `name` in `dir` is sorted by in `by`, looked at through a gate of a disk of
    /// its own, which never waits.
    fn sort_key_of(
        dir: &std::path::Path,
        name: &OsString,
        by: crate::model::listing::Sort,
    ) -> crate::model::listing::SortKey {
        let mut gate =
            crate::model::listing::ReadsFirst::new(Default::default(), Default::default(), false);
        gate.sort_key(&dir.join(name), by)
    }

    /// Asserts that `names`, files in `dir`, are in the order `order` (`listing::in_order`).
    fn assert_in_order(
        dir: &std::path::Path,
        names: &[&OsString],
        order: (crate::model::listing::Sort, bool, bool),
        at: &str,
    ) {
        let key = |name: &OsString| sort_key_of(dir, name, order.0);
        let keys: Vec<_> = names.iter().map(|name| key(name)).collect();
        for k in 1..names.len() {
            let (a, b) = ((&keys[k - 1], names[k - 1]), (&keys[k], names[k]));
            assert!(
                crate::model::listing::in_order(a, b, order).is_le(),
                "{:?} before {:?}, in {order:?}: {at}",
                names[k - 1],
                names[k]
            );
        }
    }

    /// Returns the folders of the tree at `dir` with their images, as `listing::walk` reads
    /// them.
    fn walk_by_folder(dir: &std::path::Path) -> Vec<crate::model::listing::WalkedFolder> {
        let types = crate::model::listing::Types {
            images: true,
            raw: false,
            videos: false,
            hidden: true,
        };
        let walked = crate::model::listing::walk(dir, types).flatten();
        walked
            .map(|(rel, d, images, m)| (dir.join(rel), d, images, m))
            .collect()
    }

    /// Returns the name of the next row after the image shown that `round` lacks, among the
    /// rows that the list shows (the search's matches, or every image), going on from the first
    /// row after the last. A shown image that the search hides counts as before the first row.
    /// Returns `None` when the round has shown every row.
    fn next_unshown(app: &crate::App, round: &HashSet<OsString>) -> Option<OsString> {
        let files = app.list.files();
        let rows: Vec<usize> = match app.list.matches() {
            Some(m) => m.to_vec(),
            None => (0..files.len()).collect(),
        };
        let start = (rows.iter().position(|&i| i == app.list.shown())).map_or(0, |r| r + 1);
        (0..rows.len())
            .map(|d| &files[rows[(start + d) % rows.len()]])
            .find(|f| !round.contains(*f))
            .cloned()
    }

    /// A `--slideshow` start with the first two folders of `subfolders` in the tree `dir`, each
    /// with its subfolders, as the step `at` of `play_a_tree_through_events` makes it: the
    /// slideshow starts while their trees are read, as their listing sends parts, and once they
    /// are read the list holds every image of both. Then `dir` is opened again.
    fn several_folders_with_slideshow(
        app: &mut crate::App,
        ctx: &eframe::egui::Context,
        dir: &std::path::Path,
        subfolders: &[String],
        at: &str,
    ) {
        app.stop_slideshow(ctx);
        // The slideshow takes its first step at once and then none, and with Repeat it does
        // not end when the search shows few images, so that it runs at the end of the listing.
        let delay = std::mem::replace(&mut app.config.slideshow_delay, 3600);
        let repeat = std::mem::replace(&mut app.config.slideshow_repeat, true);
        let both: Vec<PathBuf> = subfolders[..2].iter().map(|s| dir.join(s)).collect();
        app.slideshow = crate::viewer::slideshow::Slideshow::new(true);
        app.open_folders(ctx, both.clone());
        app.receive_looked_up(ctx, std::time::Duration::from_secs(5));
        assert!(
            app.folder.reads_tree_in_parts(),
            "several folders read whole, without parts, so --slideshow waits: {at}"
        );
        assert!(run_background_until(app, ctx, |a| !a.folder.is_listing()));
        assert!(
            app.slideshow.is_running(),
            "--slideshow did not start: {at}"
        );
        let types = app.shown_types();
        let walked = |folder: &PathBuf| {
            let name = folder.strip_prefix("/").unwrap().to_path_buf();
            let images = (crate::model::listing::walk(folder, types).flatten())
                .flat_map(|(rel, _, images, _)| images.into_iter().map(move |f| rel.join(f.0)));
            images
                .map(move |f| name.join(f).into_os_string())
                .collect::<Vec<_>>()
        };
        let want: HashSet<OsString> = both.iter().flat_map(walked).collect();
        let list: HashSet<OsString> = app.list.files().iter().cloned().collect();
        assert_eq!(list, want, "the images of both folders: {at}");
        app.stop_slideshow(ctx);
        app.config.slideshow_delay = delay;
        app.config.slideshow_repeat = repeat;
        crate::support::test_support::open_now(app, ctx, dir.to_path_buf());
    }

    /// Opens the image `name` of the tree shown, in the order `order`, as the step `at` of
    /// `play_a_tree_through_events` makes it: while a listing runs, so that the image's folder is
    /// read anew and the list is that image alone, and then the slideshow starts. With
    /// `slideshow_first`, View → Subfolders is off, and the slideshow of the subfolders starts
    /// before the folder is read, as `--slideshow photo.jpg` with Include Subfolders starts it.
    fn open_one_image(
        app: &mut crate::App,
        ctx: &eframe::egui::Context,
        name: &OsString,
        order: (crate::model::listing::Sort, bool, bool),
        slideshow_first: bool,
        at: &str,
    ) {
        app.stop_slideshow(ctx);
        let c = &mut app.config;
        (c.sort, c.sort_ascending, c.sort_case_sensitive) = order;
        if !app.folder.is_listing() {
            app.start_listing(ctx);
        }
        app.config.show_subfolders = !slideshow_first;
        let path = app.folder.dir().join(name);
        crate::support::test_support::open_now(app, ctx, path);
        assert_eq!(
            app.list.files(),
            std::slice::from_ref(name),
            "the image alone: {at}"
        );
        app.start_slideshow(ctx);
        app.config.show_subfolders = true;
        assert!(app.folder.reads_tree_in_parts(), "its tree in parts: {at}");
    }

    /// A `--slideshow` with the first two folders of `subfolders` in the tree `dir`, each with
    /// its subfolders, stopped on an image of the first folder itself, as the step `at` of
    /// `play_a_tree_through_events` makes it: the list goes back to the own images of both
    /// folders, and the image stays shown. Before, a stop decided by a "/" in the name, which
    /// every name of several folders holds, and the list kept the subfolders. Then `dir` is
    /// opened again.
    fn several_folders_stopped_on_their_own(
        app: &mut crate::App,
        ctx: &eframe::egui::Context,
        dir: &std::path::Path,
        subfolders: &[String],
        at: &str,
    ) {
        app.stop_slideshow(ctx);
        // As in `several_folders_with_slideshow`, the slideshow does not end before the stop.
        let delay = std::mem::replace(&mut app.config.slideshow_delay, 3600);
        let repeat = std::mem::replace(&mut app.config.slideshow_repeat, true);
        let show_subfolders = std::mem::replace(&mut app.config.show_subfolders, false);
        let both: Vec<PathBuf> = subfolders[..2].iter().map(|s| dir.join(s)).collect();
        app.slideshow = crate::viewer::slideshow::Slideshow::new(true);
        app.open_folders(ctx, both.clone());
        app.receive_looked_up(ctx, std::time::Duration::from_secs(5));
        assert!(run_background_until(app, ctx, |a| !a.folder.is_listing()));
        assert!(
            app.slideshow.is_running(),
            "--slideshow did not start: {at}"
        );
        let own = |folder: &PathBuf| {
            let name = folder.strip_prefix("/").unwrap().to_path_buf();
            let images = super::list_folder(folder, app.shown_types()).unwrap().1;
            (images.into_iter()).map(move |f| name.join(f.0).into_os_string())
        };
        let want: HashSet<OsString> = both.iter().flat_map(own).collect();
        let first = app.list.files().iter().position(|f| want.contains(f));
        if let Some(i) = first {
            app.show_file(ctx, i);
            let stopped_on = app.list.files().get(i).cloned();
            app.stop_slideshow(ctx);
            assert!(run_background_until(app, ctx, |a| !a.folder.is_listing()));
            let list: HashSet<OsString> = app.list.files().iter().cloned().collect();
            assert_eq!(list, want, "the folders' own images after a stop: {at}");
            let now = app.list.files().get(app.list.shown()).cloned();
            assert_eq!(now, stopped_on, "the image stopped on: {at}");
        }
        app.stop_slideshow(ctx);
        app.config.slideshow_delay = delay;
        app.config.slideshow_repeat = repeat;
        app.config.show_subfolders = show_subfolders;
        crate::support::test_support::open_now(app, ctx, dir.to_path_buf());
    }

    /// One run of `a_slideshow_keeps_its_order_through_every_listing`.
    fn play_a_tree_through_events(seed: u64, random: bool, repeat: bool) {
        use crate::ui::actions::Action;
        let case = format!("seed {seed:x}, random {random}, repeat {repeat}");
        let dir = std::env::temp_dir().join(format!(
            "glimt-test-order-kept-{seed:x}-{random}-{repeat}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let mut subfolders: Vec<String> = (0..4).map(|k| format!("s{k}")).collect();
        for name in ["t0.png", "t1.png", "t2.png", "U.png", "a.png"] {
            std::fs::create_dir_all(&dir).unwrap();
            image::RgbImage::new(2, 2).save(dir.join(name)).unwrap();
        }
        for sub in subfolders
            .iter()
            .chain(&[".hidden".into(), "locked".into()])
        {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
            for i in 0..5 {
                image::RgbImage::new(2, 2)
                    .save(dir.join(sub).join(format!("{i}.png")))
                    .unwrap();
            }
        }
        // The hidden folder is reached through a link only, and s2 also through s1/twice.
        std::os::unix::fs::symlink("../.hidden", dir.join("s0/in")).unwrap();
        std::os::unix::fs::symlink("../s2", dir.join("s1/twice")).unwrap();
        // Each image has a date of its own, in another order than their names.
        let mut day = seed;
        for (folder, _, images, _) in walk_by_folder(&dir) {
            for (name, _) in images {
                day = day.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(day >> 34);
                let file = std::fs::File::options().write(true).open(folder.join(name));
                file.unwrap().set_modified(when).unwrap();
            }
        }
        let few: Vec<PathBuf> = (1..=3).map(|k| few_names_tree(seed, k, &case)).collect();
        let (mut app, ctx) = app_on(&dir);
        app.config.sort = crate::model::listing::Sort::Name;
        app.config.sort_ascending = true;
        app.config.sort_case_sensitive = true;
        // View → Subfolders is on, so that a stop keeps the tree that the test's walk reads.
        app.config.show_subfolders = true;
        app.config.slideshow_subfolders = true;
        app.config.slideshow_random = random;
        app.config.slideshow_repeat = repeat;
        // Each step of the slideshow comes when the test asks for it.
        app.config.slideshow_delay = 0;
        let mut x = seed;
        let mut next = |n: usize| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x % n as u64) as usize
        };
        let shown_file = |app: &crate::App| app.list.files().get(app.list.shown()).cloned();
        app.show_file(&ctx, 0);
        app.start_slideshow(&ctx);
        let mut fed = None;
        feed_what_runs(&mut app, &ctx, &mut fed);
        let mut seen = SlidesSeen::default();
        seen.begin(shown_file(&app).unwrap());
        let mut renamed = 0;
        let in_config = |app: &crate::App| {
            let c = &app.config;
            (c.sort, c.sort_ascending, c.sort_case_sensitive)
        };
        for step in 0..200 {
            let mut event = next(33);
            let at = format!("{case}, step {step}, event {event}");
            // Images picked with Ctrl+click, and then an arrow key, Home or End in the slideshow,
            // as events 12, 13, 15 and 16 make them, half of the time with Shift held: as
            // anywhere, a move without Shift clears the selection, so a Delete, Move, keyword or
            // mark after it acts on the image on screen. In a slideshow a move with Shift is a
            // plain move too, as its moves are not along the rows.
            let rows: Vec<usize> = (0..app.list.files().len())
                .filter(|&i| app.list.shows_file(i))
                .collect();
            let picked_first = event == 29 && rows.len() > 2;
            let with_shift = picked_first && next(2) == 0;
            if picked_first {
                let picked = [rows[next(rows.len())], rows[next(rows.len())]];
                app.set_selection(&ctx, picked.into_iter().collect());
                app.panel.set_extends_selection(with_shift);
                event = [12, 13, 15, 16][next(4)];
            }
            let order_before = app.list.files().to_vec();
            let mut shown_before = shown_file(&app);
            // The names that the list shows before the event: the search's matches, or all.
            let rows_before: Vec<OsString> = (order_before.iter().enumerate())
                .filter(|&(i, _)| app.list.shows_file(i))
                .map(|(_, name)| name.clone())
                .collect();
            // The image shown before the event, under its new name when the event renamed it.
            let mut same_image = shown_before.clone();
            // The step of the slideshow that this event made: a new image picked (true), or a
            // step through the history (false).
            let mut picked = None;
            match event {
                0..=2 => {
                    if let Some(f) = fed.as_mut()
                        && !send_part(&mut app, &ctx, f)
                    {
                        send_whole(&mut app, &ctx, f);
                        assert_list_holds_the_tree(&app, &at);
                    }
                    // A part goes in only into a list of the listing's order, which the first
                    // part after another order was chosen puts the list in.
                    if fed.is_some() {
                        let order = Some(in_config(&app));
                        assert_eq!(app.list.rows().order(), order, "after a part: {at}");
                    }
                }
                3 => {
                    if let Some(f) = fed.as_ref() {
                        send_whole(&mut app, &ctx, f);
                        assert_list_holds_the_tree(&app, &at);
                    }
                }
                4 => app.refresh(&ctx),
                5 => {
                    let on = !app.config.slideshow_random;
                    app.set_random_order(&ctx, on);
                    assert_eq!(app.list.files(), order_before, "R changed the list: {at}");
                    // After R off, the next image is the next one in the list after the one
                    // shown that the round has not shown.
                    if !on && seen.back == 0 && app.list.files().len() > 1 {
                        seen.round.extend(shown_before.clone());
                        let expected = next_unshown(&app, &seen.round);
                        decode_all(&mut app, &ctx);
                        let before = seen.round.clone();
                        app.slideshow_tick(&ctx);
                        picked = Some(true);
                        if let Some(expected) = expected.filter(|_| app.slideshow.is_running()) {
                            assert_eq!(shown_file(&app), Some(expected), "after R off: {at}");
                        }
                        assert_eq!(seen.round, before);
                    }
                }
                6 | 7 if !app.list.files().is_empty() => {
                    // A delete, or a move to another subfolder, of an image of the list. Both
                    // take it out of the list and read the folder again once no listing runs.
                    // An entry whose file is gone (under a subfolder renamed) is left alone.
                    let i = next(app.list.files().len());
                    let from = dir.join(&app.list.files()[i]);
                    let to = dir
                        .join(&subfolders[next(subfolders.len())])
                        .join(format!("moved{step}.png"));
                    let done = match event {
                        6 => std::fs::remove_file(&from),
                        _ => std::fs::rename(&from, &to),
                    };
                    if done.is_ok() {
                        app.remove_indices(&ctx, &[i]);
                        app.folder.relist_later();
                    }
                }
                8 if !app.list.files().is_empty() => {
                    // A rename of a file, in its folder or to a name at the top that comes after
                    // every subfolder, also those not read yet: the row goes to its place in the
                    // list's order, and the folder is read again.
                    let i = next(app.list.files().len());
                    let old = app.list.files()[i].clone();
                    let new = match next(2) {
                        0 => PathBuf::from(&old).with_file_name(format!("renamed{step}.png")),
                        _ => PathBuf::from(format!("zz{step}.png")),
                    };
                    if std::fs::rename(dir.join(&old), dir.join(&new)).is_err() {
                        continue;
                    }
                    let new = new.into_os_string();
                    if same_image.as_ref() == Some(&old) {
                        same_image = Some(new.clone());
                    }
                    if seen.round.remove(&old) {
                        seen.round.insert(new.clone());
                    }
                    for name in seen.history.iter_mut().filter(|n| **n == old) {
                        *name = new.clone();
                    }
                    app.rename_in_list(&ctx, vec![(i, new)]);
                    app.start_listing(&ctx);
                }
                9 | 10 => {
                    // A rename of a subfolder, or a move of one into another, or a delete of
                    // one: the folder is read again.
                    let k = next(subfolders.len());
                    renamed += 1;
                    let from = subfolders[k].clone();
                    let old = dir.join(&from);
                    // The subfolders listed below it go with it.
                    let below = |s: &String| *s == from || s.starts_with(&format!("{from}/"));
                    if event == 10 && subfolders.iter().any(|s| !below(s)) {
                        std::fs::remove_dir_all(&old).unwrap();
                        subfolders.retain(|s| !below(s));
                        app.folder.relist_later();
                    } else {
                        let into = subfolders.iter().find(|s| !s.contains('/') && !below(s));
                        let new = match (step % 2, into) {
                            (1, Some(into)) => format!("{into}/m{renamed}"),
                            _ => format!("{from}r{renamed}"),
                        };
                        std::fs::rename(&old, dir.join(&new)).unwrap();
                        for s in subfolders.iter_mut().filter(|s| below(s)) {
                            *s = format!("{new}{}", &s[from.len()..]);
                        }
                        app.follow_moved_paths(
                            vec![(old, dir.join(&new))],
                            crate::files::file_io::Transfer::Move,
                        );
                        app.start_listing(&ctx);
                    }
                }
                11 => {
                    app.toggle_hidden_files(&ctx);
                    // Turned off, no image in a hidden folder is left in the list but those the
                    // walk brings again through a link, at once and not when the whole list
                    // comes.
                    if !app.config.show_hidden {
                        let hidden: Vec<&OsString> = (app.list.files().iter())
                            .filter(|f| f.as_encoded_bytes().starts_with(b"."))
                            .collect();
                        assert!(hidden.is_empty(), "hidden images left: {hidden:?}, {at}");
                    }
                }
                12 | 13 => {
                    let older = event == 12;
                    let expected = seen.step(app.list.files(), older);
                    let key = if older {
                        Action::Previous
                    } else {
                        Action::Next
                    };
                    decode_all(&mut app, &ctx);
                    app.move_along(&ctx, key);
                    match expected {
                        Some((back, name)) => {
                            assert_eq!(shown_file(&app), Some(name), "the history: {at}");
                            seen.back = back;
                            picked = Some(false);
                        }
                        // Left past the oldest image stays.
                        None if older => assert_eq!(shown_file(&app), shown_before, "{at}"),
                        None => picked = Some(true),
                    }
                }
                14 => {
                    let expected = seen.step(app.list.files(), false);
                    decode_all(&mut app, &ctx);
                    app.slideshow_tick(&ctx);
                    if let Some((back, name)) = expected {
                        assert_eq!(shown_file(&app), Some(name), "the history: {at}");
                        seen.back = back;
                        picked = Some(false);
                    } else {
                        picked = Some(true);
                    }
                }
                15 | 16 if !app.list.files().is_empty() => {
                    let key = if event == 15 {
                        Action::First
                    } else {
                        Action::Last
                    };
                    app.move_along(&ctx, key);
                    let now = shown_file(&app).unwrap();
                    // The first or the last row that the list shows, of the search's matches.
                    let files = app.list.files();
                    let expected = match (app.list.matches(), event) {
                        (None, 15) => files.first(),
                        (None, _) => files.last(),
                        (Some(m), 15) => m.first().map(|&i| &files[i]),
                        (Some(m), _) => m.last().map(|&i| &files[i]),
                    };
                    // When the search shows no image, Home and End do nothing.
                    if let Some(expected) = expected {
                        assert_eq!(&now, expected, "Home or End: {at}");
                        // Shown as a click shows it, it joins the history when it is another
                        // image.
                        if Some(&now) != same_image.as_ref() {
                            seen.history.push(now.clone());
                            seen.back = 0;
                        }
                        match event {
                            15 => seen.round = [now].into(),
                            _ => _ = seen.round.insert(now),
                        }
                    }
                }
                17 => app.config.slideshow_repeat = !app.config.slideshow_repeat,
                18 => {
                    // Another order, as the sort menu chooses it: the list is read again in it.
                    use crate::model::listing::Sort;
                    let orders = [
                        (Sort::Name, true, true),
                        (Sort::Name, true, false),
                        (Sort::Name, false, true),
                        (Sort::Date, true, true),
                        (Sort::Date, false, false),
                        (Sort::Number, true, false),
                        (Sort::Created, true, true),
                        (Sort::Size, false, true),
                        (Sort::ExifOriginal, true, true),
                        (Sort::ExifDigitized, false, false),
                    ];
                    let (by, ascending, case) = orders[next(orders.len())];
                    let c = &mut app.config;
                    (c.sort, c.sort_ascending, c.sort_case_sensitive) = (by, ascending, case);
                    app.folder.sort_again();
                    app.start_listing(&ctx);
                }
                21 | 22 if !app.list.files().is_empty() => {
                    // An image shown by hand: a click in the file list, on a pane or
                    // `--remote show`. It is shown in this round, and in order the next slide is
                    // the next row after it that the round has not shown.
                    let i = next(app.list.files().len());
                    app.show_file(&ctx, i);
                    let now = shown_file(&app).unwrap();
                    let another = Some(&now) != same_image.as_ref();
                    if another {
                        seen.round.insert(now.clone());
                        seen.history.push(now.clone());
                        seen.back = 0;
                        same_image = Some(now.clone());
                    }
                    if another && !app.config.slideshow_random {
                        let expected = next_unshown(&app, &seen.round);
                        shown_before = Some(now);
                        decode_all(&mut app, &ctx);
                        app.slideshow_tick(&ctx);
                        picked = Some(true);
                        if let Some(expected) = expected.filter(|_| app.slideshow.is_running()) {
                            let what = "after an image shown by hand";
                            assert_eq!(shown_file(&app), Some(expected), "{what}: {at}");
                        }
                    }
                }
                25 if subfolders.len() > 1 => {
                    several_folders_with_slideshow(&mut app, &ctx, &dir, &subfolders, &at);
                    // The tree again, read whole by the test's walk, and its slideshow anew.
                    app.start_slideshow(&ctx);
                    fed = None;
                    feed_what_runs(&mut app, &ctx, &mut fed);
                    if let Some(f) = fed.as_ref() {
                        send_whole(&mut app, &ctx, f);
                    }
                    feed_what_runs(&mut app, &ctx, &mut fed);
                    seen = SlidesSeen::default();
                    seen.begin(shown_file(&app).unwrap());
                    same_image = shown_file(&app);
                }
                26 | 27 => {
                    // One image opened in an order that looks at the files, from the file
                    // manager or as `glimt photo.jpg`, and then its tree read in parts. Or
                    // `--slideshow photo.jpg` with Include Subfolders: the slideshow of the
                    // subfolders starts before the folder is read. The list is that image alone,
                    // with no key, and every part goes in by the keys.
                    let top = app
                        .list
                        .files()
                        .iter()
                        .find(|f| !f.as_encoded_bytes().contains(&b'/'));
                    let Some(name) = top.cloned() else {
                        continue;
                    };
                    use crate::model::listing::Sort;
                    let orders = [
                        (Sort::Date, true, true),
                        (Sort::Date, false, false),
                        (Sort::Created, true, true),
                        (Sort::Size, false, true),
                        (Sort::ExifOriginal, true, true),
                    ];
                    let order = orders[next(orders.len())];
                    open_one_image(&mut app, &ctx, &name, order, event == 27, &at);
                    fed = None;
                    feed_what_runs(&mut app, &ctx, &mut fed);
                    seen = SlidesSeen::default();
                    seen.begin(shown_file(&app).unwrap());
                    same_image = shown_file(&app);
                }
                30 if subfolders.len() > 1 => {
                    // A stop on an image of one of several folders given, itself and not in a
                    // subfolder: the list goes back to the folders' own images.
                    several_folders_stopped_on_their_own(&mut app, &ctx, &dir, &subfolders, &at);
                    app.start_slideshow(&ctx);
                    fed = None;
                    feed_what_runs(&mut app, &ctx, &mut fed);
                    if let Some(f) = fed.as_ref() {
                        send_whole(&mut app, &ctx, f);
                    }
                    feed_what_runs(&mut app, &ctx, &mut fed);
                    seen = SlidesSeen::default();
                    seen.begin(shown_file(&app).unwrap());
                    same_image = shown_file(&app);
                }
                31 => {
                    // The tree opened anew in an order by date, and stopped on the image that its
                    // first part showed by itself. The whole list that comes after the stop keeps
                    // the image shown.
                    app.stop_slideshow(&ctx);
                    let c = &mut app.config;
                    (c.sort, c.sort_ascending, c.sort_case_sensitive) =
                        (crate::model::listing::Sort::Date, next(2) == 0, true);
                    crate::support::test_support::open_now(&mut app, &ctx, dir.clone());
                    app.start_slideshow(&ctx);
                    fed = None;
                    feed_what_runs(&mut app, &ctx, &mut fed);
                    let Some(f) = fed.as_mut() else {
                        continue;
                    };
                    send_part(&mut app, &ctx, f);
                    app.stop_slideshow(&ctx);
                    let stopped_on = shown_file(&app);
                    if let Some(f) = fed.as_ref() {
                        send_whole(&mut app, &ctx, f);
                    }
                    // A list still empty after the first part shows no image to keep.
                    if stopped_on.is_some() {
                        let now = shown_file(&app);
                        assert_eq!(now, stopped_on, "a whole list after a stop: {at}");
                    }
                    feed_what_runs(&mut app, &ctx, &mut fed);
                    app.start_slideshow(&ctx);
                    feed_what_runs(&mut app, &ctx, &mut fed);
                    seen = SlidesSeen::default();
                    if let Some(name) = shown_file(&app) {
                        seen.begin(name);
                    }
                    same_image = shown_file(&app);
                }
                28 => {
                    // A sidecar written in an order that looks at the files: the folder is read
                    // again. In an EXIF date order, as no name is new, its whole list keeps the
                    // list's order and comes without keys, as no file was looked at
                    // (`listing::kept_order`). Half of the time a file is deleted between the
                    // start of that listing and its whole list, which still holds it, as the
                    // listing read it before it went.
                    use crate::model::listing::Sort;
                    let orders = [
                        (Sort::ExifOriginal, true, true),
                        (Sort::ExifDigitized, false, true),
                        (Sort::Date, true, true),
                        (Sort::Created, false, false),
                        (Sort::Size, true, false),
                    ];
                    let order = orders[next(orders.len())];
                    let c = &mut app.config;
                    (c.sort, c.sort_ascending, c.sort_case_sensitive) = order;
                    app.folder.sort_again();
                    app.start_listing(&ctx);
                    feed_what_runs(&mut app, &ctx, &mut fed);
                    if let Some(f) = fed.as_ref() {
                        send_whole(&mut app, &ctx, f);
                    }
                    // The whole list may have taken the image shown out, and the next one is
                    // shown before the image deleted below, which may be that one.
                    seen.note_shown_elsewhere(&app, &mut same_image);
                    app.start_listing(&ctx);
                    feed_what_runs(&mut app, &ctx, &mut fed);
                    let before = app.list.files().to_vec();
                    let (tree, left_out) = tree_on_disk(&app);
                    if next(2) == 0 && !before.is_empty() {
                        let i = next(before.len());
                        if std::fs::remove_file(dir.join(&before[i])).is_ok() {
                            app.remove_indices(&ctx, &[i]);
                            app.folder.relist_later();
                        }
                    }
                    let exif = matches!(order.0, Sort::ExifOriginal | Sort::ExifDigitized);
                    let kept = (exif)
                        .then(|| crate::model::listing::kept_order(before, &tree))
                        .flatten();
                    let rows = match kept {
                        Some(kept) => super::ListedRows::InListOrder(kept),
                        None => {
                            super::ListedRows::Rows(super::Rows::sorted(tree, order, |_, name| {
                                sort_key_of(&dir, name, order.0)
                            }))
                        }
                    };
                    if let Some(f) = fed.as_ref() {
                        let listed = super::Listed {
                            dirs: Vec::new(),
                            files: Ok(rows),
                            left_out,
                            read: Vec::new(),
                            read_in: std::time::Duration::ZERO,
                        };
                        f.listing.send(listed).unwrap();
                        app.receive_listing(&ctx, std::time::Duration::ZERO);
                    }
                }
                32 => {
                    // One to three names listed in an order that looks at the files, then another
                    // such order chosen, and the parts of the walk in it.
                    use crate::model::listing::Sort;
                    let orders = [
                        (Sort::Date, true, true),
                        (Sort::Date, false, false),
                        (Sort::Created, true, true),
                        (Sort::Size, true, true),
                        (Sort::Size, false, true),
                        (Sort::ExifOriginal, true, true),
                        (Sort::ExifDigitized, false, true),
                    ];
                    let a = next(orders.len());
                    let b = (a + 1 + next(orders.len() - 1)) % orders.len();
                    let k = next(few.len());
                    let few = (few[k].as_path(), k + 1, next(2) == 0);
                    let orders = (orders[a], orders[b]);
                    few_names_then_another_order(&mut app, &ctx, few, orders, &dir, &at);
                    app.start_slideshow(&ctx);
                    fed = None;
                    feed_what_runs(&mut app, &ctx, &mut fed);
                    if let Some(f) = fed.as_ref() {
                        send_whole(&mut app, &ctx, f);
                    }
                    feed_what_runs(&mut app, &ctx, &mut fed);
                    seen = SlidesSeen::default();
                    seen.begin(shown_file(&app).unwrap());
                    same_image = shown_file(&app);
                }
                23 | 24 => {
                    // A search narrowed, widened or cleared while the slideshow runs.
                    let queries = ["", "re:^s", "re:^s[01]/", r"re:[0-2]\.png$", "re:^t"];
                    app.search.set_query(queries[next(queries.len())]);
                    app.update_search(&ctx);
                }
                _ => {
                    app.stop_slideshow(&ctx);
                    assert_eq!(
                        shown_file(&app),
                        shown_before,
                        "a stop keeps the image: {at}"
                    );
                    assert!(app.panel.scrolls_to_shown(), "scrolled to it: {at}");
                }
            }
            // An image that came into the first pane in another way than by the slideshow is
            // shown in this round.
            if picked.is_none() && !matches!(event, 15 | 16) {
                seen.note_shown_elsewhere(&app, &mut same_image);
            }
            let now = shown_file(&app);
            if picked == Some(true) {
                // A new image: one the round has not shown, or the first of a new round with
                // Repeat once every image that the search shows was shown. Without Repeat, the
                // slideshow stops at that step instead. The image on screen counts as shown,
                // also when it came there because the one before it was deleted.
                seen.round.extend(shown_before.clone());
                let all_shown = rows_before.iter().all(|f| seen.round.contains(f));
                if !app.slideshow.is_running() {
                    assert!(all_shown, "stopped before every match was shown: {at}");
                    assert!(!app.config.slideshow_repeat, "stopped with Repeat: {at}");
                } else if now != shown_before {
                    let now = now.clone().unwrap();
                    assert!(
                        app.list.shows_file(app.list.shown()),
                        "a slide that the search hides: {now:?}, {at}"
                    );
                    if all_shown {
                        assert!(app.config.slideshow_repeat, "went on without Repeat: {at}");
                        seen.round = shown_before.iter().cloned().collect();
                    }
                    assert!(
                        !seen.round.contains(&now),
                        "shown twice in a round: {now:?}, {at}"
                    );
                    seen.round.insert(now.clone());
                    seen.history.push(now);
                    seen.back = 0;
                } else {
                    // It stays only when the search shows no other image than the one on
                    // screen.
                    let others = rows_before.iter().any(|f| Some(f) != shown_before.as_ref());
                    assert!(!others, "a tick held the slideshow: {at}");
                    // Every image the search shows was shown, and with Repeat the tick began a
                    // new round, in which only the image on screen counts as shown. An empty
                    // list has no tick.
                    if !order_before.is_empty() {
                        seen.round = shown_before.iter().cloned().collect();
                    }
                }
            }
            if picked_first {
                app.panel.set_extends_selection(false);
                let on_screen = Some(vec![app.list.shown()]);
                let shift = match with_shift {
                    true => "Shift and ",
                    false => "",
                };
                assert_eq!(
                    app.targets(),
                    on_screen,
                    "the targets after {shift}a key: {at}"
                );
                // Nothing is selected but images that were on screen.
                let never_shown: Vec<&OsString> = (app.list.selected().iter())
                    .map(|&i| &app.list.files()[i])
                    .filter(|name| !seen.history.contains(name))
                    .collect();
                assert!(
                    never_shown.is_empty(),
                    "selected, never shown: {never_shown:?}, {at}"
                );
            }
            feed_what_runs(&mut app, &ctx, &mut fed);
            // Nothing that the slideshow plans to show next is an image the search hides, as
            // the slideshow sees its plan at its next look.
            if app.slideshow.is_running() {
                app.follow_list_in_slideshow();
                let hidden: Vec<usize> = (app.slideshow.planned_places())
                    .chain(app.slideshow.pool().iter().copied())
                    .filter(|&i| !app.list.shows_file(i))
                    .collect();
                assert!(
                    hidden.is_empty(),
                    "planned, the search hides {hidden:?}: {at}"
                );
            }
            // The round the test keeps is the slideshow's own, for every image of the list.
            // Before, the test missed the new round that a tick with Repeat begins when the
            // search shows no image it has not shown, and failed later, only in the runs whose
            // order by creation time led there.
            if app.slideshow.is_running() {
                let differ: Vec<(&OsString, bool)> = (app.list.files().iter())
                    .map(|f| (f, seen.round.contains(f)))
                    .filter(|&(f, in_ours)| in_ours != app.slideshow.has_shown(f))
                    .collect();
                assert!(
                    differ.is_empty(),
                    "the round differs, (name, in ours): {differ:?}, {at}"
                );
            }
            let files = app.list.files().to_vec();
            let unique: HashSet<&OsString> = files.iter().collect();
            assert_eq!(unique.len(), files.len(), "a name twice in the list: {at}");
            // No image the list held is gone from it while the tree on disk holds it.
            let disk: HashSet<OsString> = tree_on_disk(&app).0.into_iter().map(|f| f.0).collect();
            // The list is in its order, but for the names that are gone, whose files the order
            // cannot look at.
            let in_place: Vec<&OsString> = files.iter().filter(|f| disk.contains(*f)).collect();
            // The list is in the order that its rows hold, and in an order that looks at the
            // files every row carries a key of that order.
            if let Some(order) = app.list.rows().order() {
                assert_in_order(&dir, &in_place, order, &at);
            }
            assert_keys_of_the_order(&app, &at);
            // Show Hidden Files turned off takes out the images of a hidden folder by their
            // names, also those that the walk reaches through a link, and brings those back.
            let hidden_off = event == 11 && !app.config.show_hidden;
            // One image opened is the list alone until its tree is read, and so is a tree
            // opened anew until its walk is read.
            let one_opened = matches!(event, 26 | 27 | 31);
            let lost: Vec<&OsString> = (order_before.iter())
                .filter(|f| !one_opened && disk.contains(*f) && !unique.contains(f))
                .filter(|f| !(hidden_off && f.as_encoded_bytes().starts_with(b".")))
                .collect();
            assert!(
                lost.is_empty(),
                "gone from the list, still on disk: {lost:?}, {at}"
            );
            if !app.slideshow.is_running() {
                // Stopped by the test or at the end of a round without Repeat. The list is the
                // tree on disk once it is read, and the slideshow starts again where it stopped.
                let kept = shown_file(&app);
                if let Some(f) = fed.as_ref() {
                    send_whole(&mut app, &ctx, f);
                }
                feed_what_runs(&mut app, &ctx, &mut fed);
                assert_list_holds_the_tree(&app, &at);
                let all: Vec<&OsString> = app.list.files().iter().collect();
                let order = in_config(&app);
                assert_eq!(app.list.rows().order(), Some(order), "a whole list: {at}");
                assert_in_order(&dir, &all, order, &at);
                if kept.as_ref().is_some_and(|k| disk.contains(k)) {
                    assert_eq!(shown_file(&app), kept, "the image kept after a stop: {at}");
                }
                app.config.slideshow_repeat = repeat;
                app.start_slideshow(&ctx);
                feed_what_runs(&mut app, &ctx, &mut fed);
                seen = SlidesSeen::default();
                if let Some(name) = shown_file(&app) {
                    seen.begin(name);
                }
            }
        }
        app.stop_slideshow(&ctx);
        std::fs::remove_dir_all(&dir).unwrap();
        for few in few {
            std::fs::remove_dir_all(&few).unwrap();
        }
    }

    /// Makes a tree of its own for the run `case` of `play_a_tree_through_events`, with `k`
    /// images at its top and two folders below, and returns it. Its images have sizes and dates
    /// in other orders than their names.
    fn few_names_tree(seed: u64, k: usize, case: &str) -> PathBuf {
        let case = case.replace([' ', ','], "");
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("glimt-test-few-names-{k}-{case}-{pid}"));
        let _ = std::fs::remove_dir_all(&dir);
        let mut x = seed ^ k as u64;
        let mut next = || {
            x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            x >> 34
        };
        let names = (0..k).map(|i| format!("f{i}.png"));
        let below = (0..4).map(|i| format!("deeper/{i}.png"));
        let further = (0..3).map(|i| format!("deeper/more/{i}.png"));
        for name in names.chain(below).chain(further) {
            let path = dir.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            image::RgbImage::new(1 + (next() % 9) as u32, 2)
                .save(&path)
                .unwrap();
            let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(next());
            let file = std::fs::File::options().write(true).open(&path);
            file.unwrap().set_modified(when).unwrap();
        }
        dir
    }

    /// Opens the tree `few`, which has the number of images at its top that it is given with,
    /// in the first of `orders`, and takes the first part of its walk, that folder alone, as
    /// the step `at` of `play_a_tree_through_events` makes it. Then the second order is chosen,
    /// and the parts of the walk in it and its whole list come. When `moved`, an image of the
    /// list is moved into a subfolder before the first of them, which takes it out of the list.
    /// After each part the list is in its order. Then `dir` is opened again.
    fn few_names_then_another_order(
        app: &mut crate::App,
        ctx: &eframe::egui::Context,
        (few, k, moved): (&std::path::Path, usize, bool),
        (first, then): (
            (crate::model::listing::Sort, bool, bool),
            (crate::model::listing::Sort, bool, bool),
        ),
        dir: &std::path::Path,
        at: &str,
    ) {
        app.stop_slideshow(ctx);
        let c = &mut app.config;
        (c.sort, c.sort_ascending, c.sort_case_sensitive) = first;
        crate::support::test_support::open_now(app, ctx, few.to_path_buf());
        let mut fed = None;
        feed_what_runs(app, ctx, &mut fed);
        assert!(send_part(app, ctx, fed.as_mut().unwrap()), "{at}");
        assert_eq!(app.list.files().len(), k, "the names at the top: {at}");
        let c = &mut app.config;
        (c.sort, c.sort_ascending, c.sort_case_sensitive) = then;
        app.folder.sort_again();
        app.start_listing(ctx);
        feed_what_runs(app, ctx, &mut fed);
        let (from, to) = (few.join("f0.png"), few.join("deeper/f0-moved.png"));
        if moved {
            std::fs::rename(&from, &to).unwrap();
            let i = app.list.files().iter().position(|f| f == "f0.png").unwrap();
            app.remove_indices(ctx, &[i]);
            app.folder.relist_later();
        }
        // From the first part on, the list is in the order chosen, and every part goes in by it.
        while let Some(f) = fed.as_mut() {
            if !send_part(app, ctx, f) {
                send_whole(app, ctx, f);
                break;
            }
            assert_eq!(app.list.rows().order(), Some(then), "after a part, {at}");
            let names: Vec<&OsString> = app.list.files().iter().collect();
            assert_in_order(few, &names, then, &format!("after a part, {at}"));
            assert_keys_of_the_order(app, at);
        }
        let names: Vec<&OsString> = app.list.files().iter().collect();
        assert_in_order(few, &names, then, &format!("the whole list, {at}"));
        if moved {
            std::fs::rename(&to, &from).unwrap();
        }
        crate::support::test_support::open_now(app, ctx, dir.to_path_buf());
    }

    /// Asserts that in an order that looks at the files every row of the list carries a key,
    /// and one of that order: a time by date, a size by size, and an EXIF date by an EXIF date.
    /// The types make this so (`Rows`), and this checks that they do.
    fn assert_keys_of_the_order(app: &crate::App, at: &str) {
        use crate::model::listing::{Sort, SortKey};
        let rows = app.list.rows();
        let Some((by, ..)) = rows.order().filter(|o| o.0.looks_at_files()) else {
            return;
        };
        for i in 0..rows.len() {
            let of_the_order = matches!(
                (by, rows.key(i)),
                (Sort::Date | Sort::Created, Some(SortKey::Time(_)))
                    | (Sort::Size, Some(SortKey::Size(_)))
                    | (
                        Sort::ExifOriginal | Sort::ExifDigitized,
                        Some(SortKey::Exif(..))
                    )
            );
            let (name, key) = (&rows.files()[i], rows.key(i));
            assert!(of_the_order, "{name:?} has the key {key:?} in {by:?}: {at}");
        }
    }

    /// The sizes of a collection's files on a share are looked at after the images that the
    /// window waits for, through the gate. A collection's list is under "/", which is on no
    /// share. Before, its files on a share were looked at at once, before the slideshow's next
    /// image.
    #[test]
    fn a_collections_sizes_on_a_share_wait_for_the_images_awaited() {
        use crate::files::file_ops::test_mount;
        use std::time::{Duration, Instant};
        let dir = std::env::temp_dir().join(format!(
            "glimt-test-collection-sizes-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbImage::new(2, 2).save(dir.join("a.png")).unwrap();
        test_mount(&dir, Some("cifs"));
        let awaited = crate::model::listing::ImagesAwaited::default();
        awaited.set(vec![dir.join("shown.jpg")]);
        let given_up = awaited.clone();
        let decoded = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            given_up.set(Vec::new());
        });
        let began = Instant::now();
        let places = [std::path::PathBuf::from("/")];
        let total = super::total_size_unless_on_a_share(
            &places,
            false,
            &[dir.join("a.png")],
            (&awaited, &|| true),
        );
        assert!(
            began.elapsed() >= Duration::from_millis(250),
            "{:?}",
            began.elapsed()
        );
        assert!(total > 0 && total != crate::support::format::NOT_COUNTED_ON_A_SHARE);
        decoded.join().unwrap();
        // A sum whose receiver is dropped, as a new selection or search drops it, looks at no
        // more files, also while it waits at the gate. Before, it waited out the image and
        // looked at every file, each through the turn, for nobody.
        awaited.set(vec![dir.join("shown.jpg")]);
        let (tx, rx) = std::sync::mpsc::channel();
        let mut sum = crate::support::threads::Receiving::of(rx);
        let (is_wanted, paths) = (sum.is_waited_for(), [dir.join("a.png")]);
        let given = awaited.clone();
        let began = Instant::now();
        let summing = std::thread::spawn(move || {
            let total =
                super::total_size_unless_on_a_share(&places, false, &paths, (&given, &is_wanted));
            tx.send(total).unwrap_or_default();
            began.elapsed()
        });
        std::thread::sleep(Duration::from_millis(100));
        sum.clear();
        let took = summing.join().unwrap();
        assert!(took < Duration::from_secs(1), "{took:?}");
        awaited.set(Vec::new());
        test_mount(&dir, None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The sort keys of a sorted collection on a share are looked at after the images that the
    /// window waits for, as its sizes are, and another listing ends the wait. A hundred files
    /// use up one budget of looks with their sizes (`LOOKS_PER_PAUSE`), so the keys wait with a
    /// budget of their own. Before, the gate was asked about the collection's folder, "/", which
    /// is on no share, so the keys did not wait, and the listing's stop did not end the wait.
    #[test]
    fn a_sorted_collections_keys_on_a_share_wait_for_the_images_awaited() {
        use crate::files::file_ops::test_mount;
        use crate::support::threads::Took;
        use std::sync::atomic::Ordering;
        use std::time::{Duration, Instant};
        let dir =
            std::env::temp_dir().join(format!("glimt-test-collection-keys-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut gqv = String::from("#GQview collection\n");
        for k in 0..100 {
            let path = dir.join(format!("{k:03}.png"));
            image::RgbImage::new(2, 2).save(&path).unwrap();
            gqv.push_str(&format!("\"{}\"\n", path.display()));
        }
        let file = dir.join("c.gqv");
        std::fs::write(&file, gqv).unwrap();
        let (mut app, ctx) = app_on(&file);
        app.config.sort = crate::model::listing::Sort::Date;
        app.folder.sort_collection = true;
        test_mount(&dir, Some("cifs"));
        let awaited = app.decoding.images_awaited().clone();
        awaited.set(vec![dir.join("shown.jpg")]);
        let began = Instant::now();
        app.start_listing(&ctx);
        let listed = app.folder.listing_rx.wait(Duration::from_secs(10));
        let took = began.elapsed();
        assert!(matches!(listed, Took::Answer(_)));
        // One second of waits for the sizes and one for the keys.
        assert!(took >= Duration::from_millis(1700), "{took:?}");
        // Another listing ends the wait.
        awaited.set(vec![dir.join("shown.jpg")]);
        let began = Instant::now();
        app.start_listing(&ctx);
        app.folder.listing_stop.store(true, Ordering::Relaxed);
        let listed = app.folder.listing_rx.wait(Duration::from_secs(10));
        let took = began.elapsed();
        assert!(matches!(listed, Took::Answer(_)));
        assert!(took < Duration::from_millis(700), "{took:?}");
        awaited.set(Vec::new());
        test_mount(&dir, None);
        crate::files::file_io::wait_for_queued_writes();
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
