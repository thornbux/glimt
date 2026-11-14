//! Opening a folder, a collection or several folders, listing them, and following changes.

use crate::files::config::Config;
use crate::files::config::sort_folders_from;
use crate::files::config::sort_folders_path;
use crate::files::external::Mounted;
use crate::files::external::{Login, gio_mount};
use crate::files::file_io::{ListIsOld, forget_passing_identities, is_another_file};
use crate::model::list::{List, LoadedImage};
use crate::model::listing::{
    FoldersRead, ListedName, Types, is_listed, list_folder, list_tree, merge_shuffled, shuffle,
    sort_files, walk,
};
use crate::support::lang::tr;
use crate::support::threads::{Receiving, Took};
use crate::ui::actions::Action;
use crate::ui::menus::{scroll_menu, with_icon};
use crate::viewer::view::Viewing;
use crate::{files::config, files::external, model::collections, model::listing, tf, ui::theme};
use eframe::egui;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

/// How many of a tree's subfolders the folder watcher looks at each second, besides the open
/// folders. A tree of 10,000 subfolders is looked through in ten seconds, at 1,000 stats a
/// second, instead of 10,000 stats every second.
const SUBFOLDERS_A_SECOND: usize = 1_000;

/// How many times as long as the last listing took the window rests from it before a change
/// that the watcher saw lists the folder again. A sync client that writes into a tree of
/// 500,000 files every second then has it read at most a fifth of the time, while a small
/// folder, listed in milliseconds, is read again at once.
const REST_AFTER_LISTING: u32 = 4;

/// How long a look-up of a place to open runs before a notice says that it is still opening.
const STILL_OPENING_AFTER: std::time::Duration = std::time::Duration::from_secs(3);

/// What is open (a folder, a collection or several folders), its listing in the background, the way
/// back and forward, and the folders sorted into.
pub(crate) struct Folder {
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
    /// The list's version when the listing that runs started. A collection's listing works
    /// from the list or file as they were then.
    listing_version: usize,
    /// The subfolders of a folder listed with its tree. They are sent at once, because the
    /// tree may take long.
    subdirs_rx: Receiving<Vec<PathBuf>>,
    /// The images of the tree found so far, as it is read, a folder of it at a time.
    part_rx: Receiving<Vec<ListedName>>,
    /// The next listing shows a tree's images as they are found (a folder opened).
    lists_tree_in_parts: bool,
    /// What the list shown was read and sorted as: the folder, the order, ascending, case and
    /// subfolders.
    listed_as: Option<(PathBuf, listing::Sort, bool, bool, bool)>,
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
    /// Until when the window rests from the last listing before a change lists it again.
    rest_until: Instant,
    /// How many whole listings were taken since the start. Work that waits for the folder to be
    /// read again waits for this to grow.
    whole_listings: u64,
    /// The generation of the folder watcher, which each new watcher counts up. Opening another
    /// folder ends the old thread.
    watcher_generation: Arc<AtomicUsize>,
    /// The watcher saw the folder change.
    changed_tx: mpsc::Sender<()>,
    changed_rx: mpsc::Receiver<()>,
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
            listing_version: 0,
            subdirs_rx: Receiving::none(),
            part_rx: Receiving::none(),
            lists_tree_in_parts: false,
            listed_as: None,
            first_part_shown: None,
            list_is_a_part: false,
            listing_stop: Arc::default(),
            relist_when_done: false,
            change_waits: false,
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

    /// Whether the folder watcher saw the folder change since this was last asked.
    pub(crate) fn changed_on_disk(&self) -> bool {
        self.changed_rx.try_iter().count() > 0
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

    /// Whether the collection's next listing is sorted, and saved in that order.
    pub(crate) fn sorts_collection(&self) -> bool {
        self.sort_collection
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
            true => shown.display().to_string(),
            // With several folders, it is each of them.
            false => (self.folders.iter())
                .map(|f| f.display().to_string())
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

/// What a listing thread sends when it has read the folder: its subfolders, its files, the
/// folders it read with their modification times, and how long the reading took in that
/// thread.
type Listed = (Vec<PathBuf>, Vec<ListedName>, FoldersRead, Duration);

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
    shuffled: bool,
    /// Set when another listing starts, so this one stops reading a tree.
    stop: Arc<AtomicBool>,
    tx: mpsc::Sender<Listed>,
    size_tx: mpsc::Sender<u64>,
    /// When the thread started reading. The time it took is measured here, in the thread,
    /// because a window that is not drawn takes the list late, and that wait is no listing.
    started: Instant,
    ctx: egui::Context,
}

impl Listing {
    /// Lists several folders, each folder (with its subfolders) after the other.
    fn folders(self, folders: &[PathBuf]) {
        let rel = |dir: &Path, names: Vec<ListedName>| -> Vec<ListedName> {
            let at = dir.strip_prefix("/").unwrap_or(dir).to_path_buf();
            (names.into_iter())
                .map(|(n, id)| (at.join(n).into_os_string(), id))
                .collect()
        };
        let mut read = Vec::new();
        let mut files: Vec<ListedName> = (folders.iter())
            .flat_map(|d| match self.recursive {
                true => {
                    let (_, files, tree) = list_tree(d, self.types, &self.stop, |_| {}, |_| {});
                    read.extend(tree);
                    rel(d, files)
                }
                false => {
                    read.push((d.clone(), modified(d)));
                    rel(d, list_folder(d, self.types).1)
                }
            })
            .collect();
        // A folder inside another one given has its images listed once.
        let mut seen = HashSet::new();
        files.retain(|f| seen.insert(f.0.clone()));
        sort_files(&self.dir, &mut files, self.by, self.ascending, self.case);
        if self.shuffled {
            shuffle(&mut files);
        }
        forget_passing_identities(&self.dir, &mut files);
        // The sizes come after, as for a folder, so the list shows first.
        let paths: Vec<PathBuf> = files.iter().map(|f| self.dir.join(&f.0)).collect();
        if self
            .tx
            .send((Vec::new(), files, read, self.started.elapsed()))
            .is_ok()
        {
            self.ctx.request_repaint();
            let sizes = paths
                .into_iter()
                .filter_map(|p| Some(p.metadata().ok()?.len()));
            send_sizes(sizes, self.size_tx, &self.ctx);
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
        let paths = match saved.map(|f| collections::load(&f)) {
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
        // a folder entry gives it.
        let sizes = keep_files_with_size(&mut files, |(f, identity)| {
            let path = self.dir.join(f);
            let own = std::fs::symlink_metadata(&path).ok()?;
            *identity = crate::files::file_io::identity_from_metadata(&path, &own);
            let meta = match own.is_symlink() {
                true => path.metadata().ok()?,
                false => own,
            };
            meta.is_file().then_some(meta.len())
        });
        if sort {
            // Sorted by name with Case on, sort_files keeps the order as listed. For a folder
            // that is name order, but a collection has its own, so it is put in name order here.
            if self.by == listing::Sort::Name && self.case {
                files.sort_unstable();
            }
            sort_files(&self.dir, &mut files, self.by, self.ascending, self.case);
        }
        // A random slideshow with its subfolders is read from the file, in its order.
        if self.shuffled {
            shuffle(&mut files);
        }
        forget_passing_identities(&self.dir, &mut files);
        if self
            .tx
            .send((Vec::new(), files, Vec::new(), self.started.elapsed()))
            .is_ok()
        {
            self.ctx.request_repaint();
            send_sizes(sizes.into_iter(), self.size_tx, &self.ctx);
        }
    }

    /// Lists a folder, or its tree. `before` is the list in EXIF date order, which is kept if no
    /// name is new. `parts` sends a tree's images as they are read.
    fn folder(
        self,
        before: Option<Vec<OsString>>,
        parts: bool,
        part_tx: mpsc::SyncSender<Vec<ListedName>>,
        subdirs_tx: mpsc::Sender<Vec<PathBuf>>,
    ) {
        // The folder is read once, also with the folders below it. Its own subfolders show at
        // once, because a large tree takes long to read whole.
        // Its images come too, a folder of the tree at a time, in name order. (The sort asked
        // for comes with the whole tree.) The first images are sent at once, and after them at
        // most four times a second, because each part is a copy of the list.
        let mut sent: Option<(Instant, std::time::Duration)> = None;
        let (dirs, mut files, read) = match self.recursive {
            true => list_tree(
                &self.dir,
                self.types,
                &self.stop,
                |dirs| {
                    if subdirs_tx.send(dirs.to_vec()).is_ok() {
                        self.ctx.request_repaint();
                    }
                },
                |so_far| {
                    // The next part comes after four times as long as this one took, and at
                    // least a quarter second, so copying a long list does not slow the reading
                    // down.
                    let soon = sent.is_some_and(|(t, wait)| t.elapsed() < wait);
                    if !parts || self.shuffled || soon {
                        return;
                    }
                    let began = Instant::now();
                    let mut part = so_far.to_vec();
                    part.sort_unstable();
                    forget_passing_identities(&self.dir, &mut part);
                    // When the channel is full, the part before is not taken yet, so this one is
                    // left out.
                    if part_tx.try_send(part).is_ok() {
                        self.ctx.request_repaint();
                    }
                    let wait = (began.elapsed() * 4).max(std::time::Duration::from_millis(250));
                    sent = Some((Instant::now(), wait));
                },
            ),
            false => {
                // The time is taken before the folder is read, so a change made while it is
                // read is seen too.
                let read = vec![(self.dir.clone(), modified(&self.dir))];
                let (dirs, files) = list_folder(&self.dir, self.types);
                (dirs, files, read)
            }
        };
        // It stops when another folder is listed.
        if self.stop.load(Ordering::Relaxed) {
            return;
        }
        match before.and_then(|before| listing::kept_order(before, &files)) {
            Some(kept) => files = kept,
            None => sort_files(&self.dir, &mut files, self.by, self.ascending, self.case),
        }
        if self.shuffled {
            shuffle(&mut files);
        }
        forget_passing_identities(&self.dir, &mut files);
        // The receiver is gone if another folder was opened meanwhile; then we stop.
        if self
            .tx
            .send((dirs, files, read, self.started.elapsed()))
            .is_err()
        {
            return;
        }
        self.ctx.request_repaint();
        // The sizes come afterwards. The folder is read once more and summed from the directory
        // entries in chunks, so the status bar fills in gradually without a copy of the whole
        // list in memory. A tree is read a folder at a time, unsorted.
        if self.recursive {
            let sizes = walk(&self.dir, self.types).flat_map(|(rel, _, files, _)| {
                let folder = self.dir.join(rel);
                (files.into_iter())
                    .filter_map(move |f| Some(folder.join(f.0).metadata().ok()?.len()))
            });
            return send_sizes(sizes, self.size_tx, &self.ctx);
        }
        let images = std::fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| listing::kind(e).0 && is_listed(&e.path(), self.types));
        // Through a link, the size of the file it points to, as the list shows that.
        let sizes = images.filter_map(|e| Some(std::fs::metadata(e.path()).ok()?.len()));
        send_sizes(sizes, self.size_tx, &self.ctx);
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
    Tell(fn(&Path) -> String),
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
                    self.notice.give(gone(path));
                    return self.forget_recent(path);
                }
                IfGone::OpenCurrentFolder => return self.open(ctx, ".".into()),
            }
        }
        self.step_history(opening.history);
        match found {
            Found::Folder {
                dir,
                file,
                file_type,
                ..
            } => self.open_folder_found(ctx, dir, file, file_type),
            Found::Collection { file, entries } => self.open_collection(ctx, file, entries),
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
            name = path.display()
        );
        self.folder.looking_up_since = None;
        self.folder.still_opening_notice = Some(text.clone());
        self.notice.give(text);
    }

    /// Takes away the notice that a place is still opening while it is shown, and forgets when
    /// its look-up started.
    fn clear_still_opening(&mut self) {
        self.folder.looking_up_since = None;
        let told = self.folder.still_opening_notice.take();
        if told.is_some() && self.notice.text() == told.as_deref() {
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
                    let text = path.display().to_string();
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
                    let gone = |p: &Path| tf!("{name} is gone", name = p.display());
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
        self.file_operations.forget_deleting();
        self.slideshow.unshuffle();
        // A sort or a whole shuffle asked of the old list's listing is not asked of the new
        // one. A sort carried over would put another collection in order and rewrite its file.
        self.folder.sort_collection = false;
        self.slideshow.take_whole_shuffle();
        self.thumbs.forget_all();
        self.status_bar.forget_sizes_and_dates_asked();
        self.search.forget_exif_index();
        // The Pan view's dates are by name in the old folder.
        if let Some(pan) = &mut self.pan_view {
            pan.forget_dates();
        }
        // A list's entries not yet checked are the old list's. A folder opened now is listed
        // whole, and open_collection marks its own entries again.
        self.list.set_unchecked_entries(false);
    }

    /// Shows the collection in `file` as the list, with the `entries` that `look_up` read from
    /// it, or tells why it could not be read.
    fn open_collection(
        &mut self,
        ctx: &egui::Context,
        file: PathBuf,
        entries: std::io::Result<Vec<OsString>>,
    ) {
        let files = match entries {
            Ok(files) => files,
            Err(e) => {
                return self
                    .notice
                    .give(format!("{display}: {e}", display = file.display()));
            }
        };
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
        gone: fn(&Path) -> String,
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
        let (recursive, shuffled) = (self.folder.recursive, self.slideshow.shuffles_whole());
        let collection = self
            .folder
            .collection
            .is_some()
            .then(|| self.list.files().to_vec());
        // Every move writes a collection's file at once, so reading it again gives what moved
        // after the list was copied. A plain list is never written, so the list as shown is
        // used.
        let saved = (self.folder.collection.clone()).filter(|f| !collections::is_list(f));
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
        let same = Some((self.folder.dir.clone(), by, ascending, case, recursive));
        let before = (exif
            && !self.folder.lists_tree_in_parts
            && !self.slideshow.is_shuffled()
            && collection.is_none())
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
        // Parts are sent only for a folder opened, or when its tree is asked for. A folder read
        // again (a refresh) keeps its whole list on screen until the new one is read, instead of
        // shrinking and growing. They are not sent in random order either, because a list in
        // name order would take the shuffled one's place.
        let parts =
            std::mem::take(&mut self.folder.lists_tree_in_parts) && !self.slideshow.is_shuffled();
        self.folder.first_part_shown = None;
        self.folder.list_is_a_part = false;
        let listing = Listing {
            dir: list_dir,
            types,
            by,
            ascending,
            case,
            recursive,
            shuffled,
            stop,
            tx,
            size_tx,
            ctx: ctx2,
            started: Instant::now(),
        };
        // Its busy pointer: `is_listing`, while `listing_rx` waits for this answer.
        std::thread::spawn(move || {
            if !folders.is_empty() {
                listing.folders(&folders);
            } else if let Some(files) = collection {
                listing.collection(files, saved, sort);
            } else {
                listing.folder(before, parts, part_tx, subdirs_tx);
            }
        });
        self.folder.listing_rx = Receiving::of(rx);
        self.folder.listing_version = self.list.version();
        self.status_bar.sum_folder(size_rx);
        self.folder.relist_when_done = false;
        // This listing reads every change seen before it.
        self.folder.change_waits = false;
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
    /// subfolders the next `SUBFOLDERS_A_SECOND`. One change sends one message, which reads the
    /// folder again.
    fn watch_folders_read(
        &mut self,
        ctx: &egui::Context,
        mut read: FoldersRead,
        times_unknown: bool,
    ) {
        let generation = self
            .folder
            .watcher_generation
            .fetch_add(1, Ordering::Relaxed)
            + 1;
        let mut open_paths = self.folder.folders.clone();
        open_paths.extend(self.folder.collection.clone());
        open_paths.push(self.folder.dir.clone());
        let (watcher_generation, tx, ctx) = (
            self.folder.watcher_generation.clone(),
            self.folder.changed_tx.clone(),
            ctx.clone(),
        );
        // No busy pointer: it follows the folder's changes, and nobody waits for it.
        std::thread::spawn(move || {
            if times_unknown {
                read.iter_mut()
                    .for_each(|(path, time)| *time = modified(path));
            }
            // The open folders come first, and the tree's subfolders after them. A tree of many
            // folders is put in this order here rather than in the window's thread.
            let is_open = |p: &Path| open_paths.iter().any(|o| o == p);
            read.sort_by_key(|(p, _)| !is_open(p));
            let open = read.iter().take_while(|(p, _)| is_open(p)).count();
            let mut next = 0;
            while watcher_generation.load(Ordering::Relaxed) == generation {
                std::thread::sleep(std::time::Duration::from_secs(1));
                let mut changed = false;
                for i in folders_this_second(open, read.len(), next) {
                    let now = modified(&read[i].0);
                    changed |= now != read[i].1;
                    read[i].1 = now;
                }
                next = (next + SUBFOLDERS_A_SECOND) % (read.len() - open).max(1);
                if changed && watcher_generation.load(Ordering::Relaxed) == generation {
                    let _ = tx.send(());
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
        // Of the tree's images so far, only the last part that came is used, one list a frame
        // at most.
        if let Took::Answer(part) = self.folder.part_rx.latest() {
            self.take_listed_files(ctx, part, false);
        }
        let (dirs, files, read, read_in) = match self.folder.listing_rx.wait(wait) {
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
        self.take_listed_files(ctx, files, true);
        // The time taken counts the list made from it in this thread too.
        let took = read_in + listed_from.elapsed();
        self.folder.rest_until = Instant::now() + took * REST_AFTER_LISTING;
        self.list.set_unchecked_entries(false);
        let c = &self.config;
        self.folder.listed_as = (self.folder.collection.is_none()
            && self.folder.folders.is_empty()
            && !self.slideshow.is_shuffled())
        .then(|| {
            (
                self.folder.dir.clone(),
                c.sort,
                c.sort_ascending,
                c.sort_case_sensitive,
                self.folder.recursive,
            )
        });
    }

    /// Takes the images of the folder read in the background as the list. That is all of them
    /// when `whole`, and otherwise those of a tree found so far. The shown image and the
    /// selection stay.
    pub(crate) fn take_listed_files(
        &mut self,
        ctx: &egui::Context,
        files: Vec<ListedName>,
        whole: bool,
    ) {
        // Files still being deleted in the background must not come back with a refresh.
        let mut files = files;
        self.file_operations.leave_out_deleting(&mut files);
        // A refresh during random order keeps the order, so Previous still goes back through
        // the images that were shown. A list shuffled whole in the thread stays as it is. The
        // flag is for this listing, and it is taken whether or not the order is still random,
        // because a flag left behind would shuffle every later listing.
        let fresh = whole && self.slideshow.take_whole_shuffle();
        let mut files = if whole && self.slideshow.is_shuffled() && !fresh {
            merge_shuffled(self.list.files(), files)
        } else {
            files
        };
        // When the list was shuffled whole for a random slideshow, the image shown comes first,
        // as R puts it, so a slideshow without Repeat shows all images before it ends.
        if fresh
            && self.slideshow.is_shuffled()
            && let Some(at) = (self.list.files().get(self.list.shown()))
                .and_then(|shown| files.iter().position(|f| f.0 == *shown))
        {
            files.swap(0, at);
        }
        // The sizes and dates of the rows go from one part of the tree to the next, and to the
        // whole list after it, as the files are the same ones, read by the same walk. Before,
        // each part read them again, and an answer still on its way was dropped and asked
        // again, which on a slow share kept the list busy until the tree was read.
        let after_a_part = std::mem::replace(&mut self.folder.list_is_a_part, !whole);
        // When nothing changed (most refreshes, after Glimt's own writes beside the files), the
        // list, the selection and the search stay as they are. A name that another file has
        // taken since is a change, as what was read of the file it held is that file's.
        if self.list.is_listed_as(&files) && !self.folder.sort_collection {
            // A file may have been saved over under its own name (an editor writes and
            // renames), so its size and date are read again, for the rows on screen only. A
            // tree's whole list that is its last part keeps them, as the same walk read them.
            if !after_a_part {
                self.list.forget_sizes_and_dates();
            }
            // A rename keeps the list as it is but forgot the file's decoded image, which is
            // asked for again here.
            self.refill_preload();
            return;
        }
        // Glimt went to an image by itself, in the name order of a tree's first images. The
        // whole list, in the order asked for, starts at its first image instead, unless the
        // user chose another one meanwhile.
        let mut shown = self.list.files().get(self.list.shown()).cloned();
        if whole && shown.is_some() && std::mem::take(&mut self.folder.first_part_shown) == shown {
            shown = None;
        }
        let first = self.list.files().is_empty() && !whole;
        // A refresh keeps the selection, so Delete or Move after it still acts on what was
        // picked. The selected files that the search hides stay selected too.
        let picked = (self.list).names_with_identities(&self.list.selected_with_hidden());
        let read = after_a_part.then(|| self.list.take_sizes_and_dates());
        self.replace_files(ctx, files, shown);
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

    /// Gives files in the list new names in place, for a file moved or renamed that keeps its
    /// place. The list's version goes up and the search runs again, so the matches follow the
    /// new names. The listing that follows finds the same names and would change nothing.
    pub(crate) fn rename_in_list(&mut self, ctx: &egui::Context, names: Vec<(usize, OsString)>) {
        if names.is_empty() {
            return;
        }
        for (i, name) in names {
            self.list.rename(i, name);
        }
        self.list.note_names_changed();
        self.update_search(ctx);
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
        // The list has a new order (shuffled, or a collection's), so only a listing in sort
        // order sets this again.
        self.folder.listed_as = None;
        // The file counts as found when it is shown now and is in the new list too. It does not
        // when another program removed or renamed it, because its turn not saved would
        // otherwise go to the image shown in its place.
        let found = (shown.as_ref()).and_then(|p| files.iter().position(|f| f.0 == *p));
        let still = found.is_some_and(|k| {
            self.list.files().get(self.list.shown()) == shown.as_ref()
                && !is_another_file(files[k].1, self.list.identity(self.list.shown()))
        });
        // The decodes still being made are kept by name, as the decoded images below, so no
        // file is decoded twice while the parts of a tree come in. The shown file's decodes of a
        // later page are not kept, because the file is shown again from its first page.
        let (old_shown, later_page) = (self.list.shown(), self.viewing.page() > 0);
        let mut decodes: HashMap<OsString, Vec<_>> = HashMap::new();
        for ((i, full), job) in self.list.take_decodes() {
            if let Some(name) = self
                .list
                .files()
                .get(i)
                .filter(|_| !later_page || i != old_shown)
            {
                decodes.entry(name.clone()).or_default().push((full, job));
            }
        }
        if !still {
            self.leave_image(ctx);
        }
        // The decoded images are kept by name, so the neighbours read ahead stay, after a refresh
        // as after a delete.
        let mut loaded_images: HashMap<OsString, LoadedImage> =
            (self.list.take_loaded_images().into_iter())
                .filter_map(|(i, s)| Some((self.list.files().get(i)?.clone(), s)))
                .collect();
        // The panes are kept by name too, because their places belong to other files in the
        // new list, but their files do not change.
        let panes: Vec<OsString> = (self.list.panes().iter())
            .filter_map(|&i| self.list.files().get(i).cloned())
            .collect();
        // So is the image that a slideshow of one pane at a time showed last, because it goes
        // on from there, while the panes it keeps stay as they are.
        let last_slide = self.list.files().get(self.list.last_slide()).cloned();
        // A name that another file has taken since keeps nothing that was read of the file it
        // held: its thumbnail, size and date, its part of the exif: index and its Pan view date.
        let taken = self.list.names_another_file_took(&files);
        for name in &taken {
            decodes.remove(name);
        }
        // A new List means every place held in the old one goes with it, whatever field holds
        // it. Only what is carried over by name above comes back.
        self.list = List::replacing(&self.list, files);
        if !taken.is_empty() {
            let dir = self.folder.dir().to_path_buf();
            self.forget_names(&taken.iter().map(|name| dir.join(name)).collect::<Vec<_>>());
        }
        // The shown file is set before anything refills the list, because with Compare on a
        // `refill_preload` at place 0 would drop the decode of the image shown.
        let shown = found.unwrap_or(0);
        self.list.set_shown(shown);
        self.index_new_names();
        let panes = (panes.iter())
            .filter_map(|n| self.list.files().iter().position(|f| f == n))
            .collect();
        self.list.set_panes(panes);
        let last = last_slide.and_then(|n| self.list.files().iter().position(|f| *f == n));
        let last = last.or(self.list.panes().last().copied()).unwrap_or(shown);
        self.list.set_last_slide(last);
        // One pass over the list for a handful of textures and decodes.
        let (mut kept, mut asked) = (Vec::new(), Vec::new());
        for (i, name) in self.list.files().iter().enumerate() {
            if loaded_images.is_empty() && decodes.is_empty() {
                break;
            }
            if let Some(loaded) = loaded_images.remove(name) {
                kept.push((i, loaded));
            }
            for (full, job) in decodes.remove(name).unwrap_or_default() {
                asked.push((i, full, job));
            }
        }
        for (i, loaded) in kept {
            if self.is_file(i, &loaded.path)
                && !is_another_file(loaded.identity(), self.list.identity(i))
            {
                self.list.keep_loaded_image(i, loaded);
            }
        }
        for (i, full, job) in asked {
            if self.is_file(i, &job.1) {
                self.list.ask_decode((i, full), job);
            }
        }
        // The search runs first, after the decoded images are carried over, because with Compare on
        // the selection refills, and the compared image and the neighbours are counted among the
        // matches, not in the whole folder.
        self.update_search(ctx);
        self.selection_changed(ctx);
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
    /// folder.
    pub(crate) fn toggle_hidden_files(&mut self, ctx: &egui::Context) {
        self.config.show_hidden = !self.config.show_hidden;
        self.config.save();
        // Read again, and scrolled back to the open folder.
        self.tree.read_again_and_reopen();
        self.start_listing(ctx);
    }

    /// F5. The folder is read and sorted anew (also in an EXIF date order whose names stayed),
    /// and the shown image, the thumbnails and the size of a file edited elsewhere are made
    /// again.
    pub(crate) fn refresh(&mut self, ctx: &egui::Context) {
        // It was asked for, so the list is sorted anew, also in an EXIF date order whose names
        // stayed.
        self.folder.listed_as = None;
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
            (collections::by_real_path(&paths).into_iter())
                .filter_map(|(p, _)| Some(p.strip_prefix("/").ok()?.as_os_str().to_owned()))
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
                name = file.display(),
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

/// Adds the partial sums from `send_sizes` that have arrived. `rx` becomes `None` when the sum
/// is done.
pub(crate) fn add_sent_sizes(rx: &mut Receiving<u64>, total: &mut u64) {
    while let Took::Answer(sum) = rx.next() {
        *total += sum;
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
            app.folder.changed_tx.send(()).unwrap();
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
        app.take_listed_files(&ctx, listed(&["a/w.png", "b/x.png"]), false);
        assert_eq!(known(&app), Some(Some((7, 1))));
        app.take_listed_files(&ctx, listed(&["a/w.png", "b/x.png", "c/y.png"]), true);
        assert_eq!(known(&app), Some(Some((7, 1))));
        app.take_listed_files(&ctx, listed(&["a/w.png", "b/x.png"]), true);
        assert_eq!(known(&app), None);
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
        app.follow_moved_paths(vec![(a, b)], false);
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
        app.follow_moved_paths(vec![(a, c)], false);
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
        app.visit_if_there(&ctx, gone.clone(), |p| format!("{} is gone", p.display()));
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
}
