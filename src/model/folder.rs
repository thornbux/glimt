//! Opening a folder, a collection or several folders, listing them, and following changes.

use crate::files::config::Config;
use crate::files::config::sort_folders_from;
use crate::files::config::sort_folders_path;
use crate::files::external::Mounted;
use crate::files::external::{Login, gio_mount};
use crate::model::list::{List, Slot};
use crate::model::listing::{
    Types, is_listed, list_folder, list_tree, merge_shuffled, shuffle, sort_files, walk,
};
use crate::support::lang::tr;
use crate::support::test_hooks::noted;
use crate::support::threads::{Receiving, Took};
use crate::ui::actions::Action;
use crate::ui::menus::{iconed, scroll_menu};
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
use std::time::Instant;

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
    /// The folder listing in the background, as (subfolders, images).
    listing_rx: Receiving<(Vec<PathBuf>, Vec<OsString>)>,
    /// The subfolders of a folder listed with its tree. They are sent at once, because the
    /// tree may take long.
    subdirs_rx: Receiving<Vec<PathBuf>>,
    /// The images of the tree found so far, as it is read, a folder of it at a time.
    part_rx: Receiving<Vec<OsString>>,
    /// The next listing shows a tree's images as they are found (a folder opened).
    list_parts: bool,
    /// What the list shown was read and sorted as: the folder, the order, ascending, case and
    /// subfolders.
    listed_as: Option<(PathBuf, listing::Sort, bool, bool, bool)>,
    /// The image that the first part of a tree went to by itself. The whole list goes to its
    /// first image instead.
    went_to: Option<OsString>,
    /// Set to stop the listing of the tree before, when another one starts.
    listing_stop: Arc<AtomicBool>,
    /// Whether the folder changed while a listing was running. It is listed again when the
    /// listing is done.
    relist: bool,
    /// The generation of the folder watcher. Opening another folder ends the old thread.
    watching: Arc<AtomicUsize>,
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
}

impl Folder {
    /// A `Folder` with nothing open yet. The recent list and the sort manager's folders are read
    /// from their files.
    pub(crate) fn new(config: &Config) -> Self {
        // When no list is kept, one from before goes too, also when the setting was changed in
        // the file.
        let recent = match config::recent_path() {
            Some(file) if !config.keep_recent => {
                crate::files::file_io::write_later(move || {
                    crate::files::file_io::remove_shared(&file)
                });
                Vec::new()
            }
            Some(file) => crate::images::decode::read_limited(file)
                .map_or_else(|_| Vec::new(), |t| config::parse_recent(&t)),
            None => Vec::new(),
        };
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
            listing_rx: Receiving::none(),
            subdirs_rx: Receiving::none(),
            part_rx: Receiving::none(),
            list_parts: false,
            listed_as: None,
            went_to: None,
            listing_stop: Arc::default(),
            relist: false,
            watching: Arc::new(AtomicUsize::new(0)),
            changed_tx,
            changed_rx,
            recent,
            back: Vec::new(),
            forward: Vec::new(),
            mounting_rx: Receiving::none(),
            login: None,
            sort_folders: (sort_folders_path()
                .and_then(|p| crate::images::decode::read_limited(p).ok()))
            .map_or_else(Vec::new, |t| sort_folders_from(&t)),
        }
    }

    /// The folder open, or "/" for a collection or several folders, whose names are relative
    /// to it.
    pub(crate) fn dir(&self) -> &Path {
        &self.dir
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
        self.list_parts = on;
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

    /// Whether the folder is being read in the background.
    pub(crate) fn is_listing(&self) -> bool {
        self.listing_rx.is_some()
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
        self.relist = true;
    }

    /// Whether the folder is to be read again.
    pub(crate) fn wants_relist(&self) -> bool {
        self.relist
    }

    /// Whether the folder watcher saw the folder change since this was last asked.
    pub(crate) fn changed(&self) -> bool {
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

    /// Takes the sort manager's folders as its file has them now.
    pub(crate) fn set_sort_folders(&mut self, dirs: Vec<PathBuf>) {
        self.sort_folders = dirs;
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
    pub(crate) fn hold_listing(&mut self, listing: mpsc::Receiver<(Vec<PathBuf>, Vec<OsString>)>) {
        self.listing_rx = Receiving::of(listing);
    }

    /// Drops the listing in the background, as when it is done.
    pub(crate) fn drop_listing(&mut self) {
        self.listing_rx = Receiving::none();
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
    shuffled: bool,
    /// Set when another listing starts, so this one stops reading a tree.
    stop: Arc<AtomicBool>,
    tx: mpsc::Sender<(Vec<PathBuf>, Vec<OsString>)>,
    size_tx: mpsc::Sender<u64>,
    ctx: egui::Context,
}

impl Listing {
    /// Lists several folders, each folder (with its subfolders) after the other.
    fn folders(self, folders: &[PathBuf]) {
        let rel = |dir: &Path, names: Vec<OsString>| -> Vec<OsString> {
            let at = dir.strip_prefix("/").unwrap_or(dir).to_path_buf();
            (names.into_iter())
                .map(|n| at.join(n).into_os_string())
                .collect()
        };
        let mut files: Vec<OsString> = (folders.iter())
            .flat_map(|d| match self.recursive {
                true => rel(d, list_tree(d, self.types, &self.stop, |_| {}, |_| {}).1),
                false => rel(d, list_folder(d, self.types).1),
            })
            .collect();
        // A folder inside another one given has its images listed once.
        let mut seen = HashSet::new();
        files.retain(|f| seen.insert(f.clone()));
        sort_files(&self.dir, &mut files, self.by, self.ascending, self.case);
        if self.shuffled {
            shuffle(&mut files);
        }
        // The sizes come after, as for a folder, so the list shows first.
        let paths: Vec<PathBuf> = files.iter().map(|f| self.dir.join(f)).collect();
        if self.tx.send((Vec::new(), files)).is_ok() {
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
        let mut files = files;
        if let Some(Ok(paths)) = saved.map(|f| collections::load(&f)) {
            files = (paths.iter())
                .filter_map(|p| Some(p.strip_prefix("/").ok()?.as_os_str().to_owned()))
                .collect();
        }
        files.retain(|f| self.dir.join(f).exists());
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
        let sizes: Vec<u64> = (files.iter())
            .filter_map(|f| Some(self.dir.join(f).metadata().ok()?.len()))
            .collect();
        if self.tx.send((Vec::new(), files)).is_ok() {
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
        part_tx: mpsc::SyncSender<Vec<OsString>>,
        subdirs_tx: mpsc::Sender<Vec<PathBuf>>,
    ) {
        // The folder is read once, also with the folders below it. Its own subfolders show at
        // once, because a large tree takes long to read whole.
        // Its images come too, a folder of the tree at a time, in name order. (The sort asked
        // for comes with the whole tree.) The first images are sent at once, and after them at
        // most four times a second, because each part is a copy of the list.
        let mut sent: Option<(Instant, std::time::Duration)> = None;
        let (dirs, mut files) = match self.recursive {
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
                    // When the channel is full, the part before is not taken yet, so this one is
                    // left out.
                    if part_tx.try_send(part).is_ok() {
                        self.ctx.request_repaint();
                    }
                    let wait = (began.elapsed() * 4).max(std::time::Duration::from_millis(250));
                    sent = Some((Instant::now(), wait));
                },
            ),
            false => list_folder(&self.dir, self.types),
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
        // The receiver is gone if another folder was opened meanwhile; then we stop.
        if self.tx.send((dirs, files)).is_err() {
            return;
        }
        self.ctx.request_repaint();
        // The sizes come afterwards. The folder is read once more and summed from the directory
        // entries in chunks, so the status bar fills in gradually without a copy of the whole
        // list in memory. A tree is read a folder at a time, unsorted.
        if self.recursive {
            let sizes = walk(&self.dir, self.types).flat_map(|(rel, _, files)| {
                let folder = self.dir.join(rel);
                (files.into_iter()).filter_map(move |f| Some(folder.join(f).metadata().ok()?.len()))
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

impl crate::App {
    // ---- Opening a folder, a collection or several folders, and listing it ----

    /// Opens a folder, or the folder of a file with that file shown.
    // shortcut: is_dir and canonicalize here, in the window's thread, are what hold it on a share
    // that stopped answering (the listing is in the background already); move them to the
    // listing thread if a stalled mount makes opening hang in use.
    pub(crate) fn open(&mut self, ctx: &egui::Context, path: PathBuf) {
        if collections::is_collection(&path) {
            return self.open_collection(ctx, path);
        }
        self.leave_image(ctx);
        self.folder.collection = None;
        self.folder.folders.clear();
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
        // A given file is shown at once. The folder is listed in the background, and the list
        // is merged in afterwards.
        let shown = file
            .as_deref()
            .and_then(Path::file_name)
            .map(OsString::from);
        self.forget_old_list();
        // A file of a type the menu does not list (a RAW with RAW off) is listed with the
        // folder it opens. A folder or several folders opened later go by the menu again.
        self.folder.opened_type = file.as_deref().map_or(Types::NONE, Types::of);
        // In another folder, a file of the same name is another image. It opens as one, not at
        // the zoom and place of the image before.
        if dir != self.folder.dir {
            self.list.forget_files();
        }
        self.folder.recursive = self.config.show_subfolders;
        self.folder.dir = dir.clone();
        self.remember_recent(&dir);
        self.replace_files(ctx, shown.iter().cloned().collect(), shown);
        self.folder.list_parts = true;
        self.list(ctx);
        self.watch(ctx);
        self.title(ctx);
    }

    /// Puts `path` first in File → Open Recent, also in the file the other windows read when
    /// they start. The change is merged with what they wrote meanwhile.
    pub(crate) fn remember_recent(&mut self, path: &Path) {
        if !self.config.keep_recent || self.folder.recent.first().is_some_and(|p| p == path) {
            return;
        }
        self.folder.recent = config::add_recent(&self.folder.recent, path);
        let path = path.to_path_buf();
        crate::files::file_io::write_later(move || {
            let Some(file) = config::recent_path() else {
                return;
            };
            // This works as for every file the windows share. Two windows opening at once keep
            // both paths, and a file that cannot be read is not written over.
            let add = |old: &[u8]| {
                let all = config::add_recent(&config::parse_recent(old), &path);
                (config::recent_text(&all), ())
            };
            if let Err(e) = crate::files::file_io::edit_shared(&file, add) {
                crate::files::file_io::save_failed(format!(
                    "Could not save {}: {e}",
                    file.display()
                ));
            }
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
            iconed(ui, theme::icon::RECENT, tr("Open Recent")),
            |ui| {
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                if self.folder.recent.is_empty() {
                    ui.weak(tr("Nothing opened yet"));
                    return;
                }
                let mut chosen = None;
                for path in &self.folder.recent {
                    let text = path.display().to_string();
                    if noted(&ui.button(&text), &text).clicked() {
                        chosen = Some(path.clone());
                    }
                }
                ui.separator();
                if noted(&ui.button(tr("Clear Recent")), tr("Clear Recent")).clicked() {
                    self.clear_recent();
                }
                // When the path is gone since (deleted, or a stick taken out), a notice says so,
                // and it leaves the list.
                if let Some(path) = chosen {
                    match path.exists() {
                        true => self.visit(&ctx, path),
                        false => {
                            self.notice = Some(tf!("{name} is gone", name = path.display()));
                            self.folder.recent.retain(|p| *p != path);
                        }
                    }
                }
            },
        );
    }

    /// What belongs to the list shown before, which a new folder, collection or set of folders
    /// starts without.
    pub(crate) fn forget_old_list(&mut self) {
        // A check still running would find names of the old list, so it is stopped and its
        // window closed.
        if let Some(Some(check)) = self.check.take() {
            check.stop();
        }
        self.folder.opened_type = Types::NONE;
        // The old folder's watcher ends. A folder opened next starts its own (watch()).
        self.folder.watching.fetch_add(1, Ordering::Relaxed);
        self.broken_files.clear();
        self.folder.subdirs.clear();
        self.work.forget_deleting();
        self.slideshow.unshuffle();
        // A sort or a whole shuffle asked of the old list's listing is not asked of the new
        // one. A sort carried over would put another collection in order and rewrite its file.
        self.folder.sort_collection = false;
        self.slideshow.take_whole_shuffle();
        self.thumbs.forget_all();
        self.search.forget_exif_index();
    }

    /// Show the collection in `file` as the list.
    pub(crate) fn open_collection(&mut self, ctx: &egui::Context, file: PathBuf) {
        // The path is absolute from here on, because it is kept in Open Recent and as the last
        // folder. A relative name was another file, or none, for a window started elsewhere.
        let file = std::path::absolute(&file).unwrap_or(file);
        let paths = match collections::load(&file) {
            Ok(paths) => paths,
            Err(e) => {
                return self.notice = Some(format!("{display}: {e}", display = file.display()));
            }
        };
        self.leave_image(ctx);
        let root = Path::new("/");
        let files: Vec<OsString> = (paths.iter())
            .filter_map(|p| Some(p.strip_prefix(root).ok()?.as_os_str().to_owned()))
            .collect();
        // When the same collection is read again (F5), a delete still running keeps its names,
        // so a failed delete brings its images back.
        let deleting = if self.folder.collection.as_ref() == Some(&file) {
            self.work.take_deleting()
        } else {
            HashSet::new()
        };
        self.forget_old_list();
        self.work.keep_deleting(deleting);
        self.folder.recursive = false;
        self.folder.dir = root.to_path_buf();
        self.remember_recent(&file);
        self.folder.collection = Some(file);
        self.folder.folders.clear();
        let first = files.first().cloned();
        self.replace_files(ctx, files, first);
        self.list(ctx);
        self.title(ctx);
    }

    /// Shows the images of several folders as one list, for a slideshow across them. One folder
    /// is opened as usual. A file stands for its folder.
    pub(crate) fn open_folders(&mut self, ctx: &egui::Context, paths: Vec<PathBuf>) {
        let first = paths.first().cloned();
        let mut folders: Vec<PathBuf> = Vec::new();
        for p in paths {
            let p = std::path::absolute(&p).unwrap_or(p);
            let dir = if p.is_dir() {
                p
            } else {
                p.parent().map_or(p.clone(), Path::to_path_buf)
            };
            if !folders.contains(&dir) {
                folders.push(dir);
            }
        }
        // One folder (several of its files, from a file manager) is opened at the first file.
        if folders.len() < 2 {
            if let Some(first) = first {
                self.open(ctx, first);
            }
            return;
        }
        self.leave_image(ctx);
        self.folder.collection = None;
        // As when a folder is opened, the subfolders come with it when the menu says so,
        // whatever was shown.
        self.folder.recursive = self.config.show_subfolders;
        self.forget_old_list();
        self.folder.dir = PathBuf::from("/");
        self.folder.folders = folders;
        self.replace_files(ctx, Vec::new(), None);
        self.list(ctx);
        self.title(ctx);
    }

    /// Where Back comes back to. It is the collection shown, and otherwise the file shown or
    /// the folder.
    pub(crate) fn back_step(&self) -> PathBuf {
        self.folder
            .collection
            .clone()
            .unwrap_or_else(|| self.location())
    }

    /// Open `path` as a new step in the Back/Forward history.
    pub(crate) fn visit(&mut self, ctx: &egui::Context, path: PathBuf) {
        // A message about the folder before (a share that could not be mounted) goes with it.
        self.notice = None;
        self.folder.back.push(self.back_step());
        // The history does not grow without end, because a slideshow of folders, or a script's
        // --remote open, visits many.
        if self.folder.back.len() > 200 {
            self.folder.back.remove(0);
        }
        self.folder.forward.clear();
        self.open(ctx, path);
    }

    /// The file shown, or the folder when it has no images.
    pub(crate) fn location(&self) -> PathBuf {
        match self.list.files().get(self.list.cur()) {
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
        self.notice = Some(tf!("Connecting to {uri}…", uri = login.uri));
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = gio_mount(&login.uri, answer.then_some(&login));
            let _ = tx.send((result, login));
            ctx.request_repaint();
        });
        self.folder.mounting_rx = Receiving::of(rx);
    }

    /// Reads the folder (and then the sizes) in the background, and `receive_listing` merges it
    /// in. A collection is checked for files that are gone instead.
    pub(crate) fn list(&mut self, ctx: &egui::Context) {
        let (tx, rx) = mpsc::channel();
        let (size_tx, size_rx) = mpsc::channel();
        let (list_dir, ctx2, types) = (self.folder.dir.clone(), ctx.clone(), self.types());
        let c = &self.config;
        let (by, ascending, case) = (c.sort, c.sort_ascending, c.sort_case);
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
            && !self.folder.list_parts
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
        let parts = std::mem::take(&mut self.folder.list_parts) && !self.slideshow.is_shuffled();
        self.folder.went_to = None;
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
        };
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
        self.status.sum_folder(size_rx);
        self.folder.relist = false;
    }

    /// Poll the folder's modification time once a second in the background (one `stat`, no
    /// repaint unless it changed). Adding, removing or renaming a file changes it.
    /// The thread ends when another folder is opened.
    pub(crate) fn watch(&mut self, ctx: &egui::Context) {
        let generation = self.folder.watching.fetch_add(1, Ordering::Relaxed) + 1;
        let (watching, tx, ctx, dir) = (
            self.folder.watching.clone(),
            self.folder.changed_tx.clone(),
            ctx.clone(),
            self.folder.dir.clone(),
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

    /// Take the folder listing if it is ready, waiting up to `wait`.
    pub(crate) fn receive_listing(&mut self, ctx: &egui::Context, wait: std::time::Duration) {
        // A thread that broke leaves no folders, and nothing waiting for it.
        if let Took::Answer(dirs) = self.folder.subdirs_rx.take() {
            self.folder.subdirs = dirs;
        }
        // Of the tree's images so far, only the last part that came is used, one list a frame
        // at most.
        if let Took::Answer(part) = self.folder.part_rx.latest() {
            self.take_files(ctx, part, false);
        }
        let (dirs, files) = match self.folder.listing_rx.wait(wait) {
            Took::Answer(listed) => listed,
            // When the listing thread broke, a notice says so once, and nothing (the busy
            // pointer, a parked --remote select, auto refresh) waits for it any more.
            Took::Gone => {
                self.folder.subdirs_rx.clear();
                self.folder.part_rx.clear();
                return self.stopped(tr("Reading the folder"));
            }
            Took::Nothing => return,
        };
        self.folder.subdirs_rx.clear();
        self.folder.part_rx.clear();
        // Several folders shown together are listed as "/" with no subfolders, which are not
        // the tree's.
        if self.folder.collection.is_none() && self.folder.folders.is_empty() {
            self.tree.read(self.folder.dir.clone(), dirs.clone());
        }
        self.folder.subdirs = dirs;
        self.take_files(ctx, files, true);
        let c = &self.config;
        self.folder.listed_as = (self.folder.collection.is_none()
            && self.folder.folders.is_empty()
            && !self.slideshow.is_shuffled())
        .then(|| {
            (
                self.folder.dir.clone(),
                c.sort,
                c.sort_ascending,
                c.sort_case,
                self.folder.recursive,
            )
        });
    }

    /// Takes the images of the folder read in the background as the list. That is all of them
    /// when `whole`, and otherwise those of a tree found so far. The shown image and the
    /// selection stay.
    pub(crate) fn take_files(&mut self, ctx: &egui::Context, files: Vec<OsString>, whole: bool) {
        // Files still being deleted in the background must not come back with a refresh.
        let mut files = files;
        self.work.leave_out_deleting(&mut files);
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
            && let Some(at) = (self.list.files().get(self.list.cur()))
                .and_then(|shown| files.iter().position(|f| f == shown))
        {
            files.swap(0, at);
        }
        // When nothing changed (most refreshes, after Glimt's own writes beside the files), the
        // list, the selection and the search stay as they are.
        if files == self.list.files() && !self.folder.sort_collection {
            // A file may have been saved over under its own name (an editor writes and
            // renames), so its size and date are read again, for the rows on screen only.
            self.list.forget_details();
            // A rename keeps the list as it is but forgot the file's decoded image, which is
            // asked for again here.
            self.refill();
            return;
        }
        // Glimt went to an image by itself, in the name order of a tree's first images. The
        // whole list, in the order asked for, starts at its first image instead, unless the
        // user chose another one meanwhile.
        let mut shown = self.list.files().get(self.list.cur()).cloned();
        if whole && shown.is_some() && std::mem::take(&mut self.folder.went_to) == shown {
            shown = None;
        }
        let first = self.list.files().is_empty() && !whole;
        // A refresh keeps the selection, so Delete or Move after it still acts on what was
        // picked.
        let picked: HashSet<OsString> = (self.list.selected().iter())
            .filter_map(|&i| self.list.files().get(i).cloned())
            .collect();
        self.replace_files(ctx, files, shown);
        if first {
            self.folder.went_to = self.list.files().get(self.list.cur()).cloned();
        }
        self.reselect(ctx, &picked);
        if whole && std::mem::take(&mut self.folder.sort_collection) {
            self.save_collection_order();
        }
    }

    /// Lets go of what holds places in the list, after files were taken out of it and its
    /// places shifted. This is the one place for it. (A list read again gets a new `List`
    /// instead.) It covers the selection (told to the status bar), the anchor, a rectangle or
    /// a collection drag, and the decodes in flight. Those have old places, so they are
    /// rejected by path when they arrive, and refill asks for them again.
    pub(crate) fn list_changed(&mut self, ctx: &egui::Context) {
        self.list.places_changed();
        self.selection_changed(ctx);
    }

    /// Selects again by name, after the list was read again or put in another order, so a
    /// Delete or Move after it still acts on what was picked.
    pub(crate) fn reselect(&mut self, ctx: &egui::Context, names: &HashSet<OsString>) {
        if names.is_empty() {
            return;
        }
        let places = (self.list.files().iter().enumerate())
            .filter(|(_, f)| names.contains(*f))
            .map(|(i, _)| i)
            .collect();
        self.set_selection(ctx, places);
    }

    /// Replaces the file list and goes to `shown` (otherwise the first image). Textures are
    /// kept for the files they were made from, at their new places. All other state based on
    /// places is reset, and jobs in flight are rejected by path when they arrive. When the same
    /// file is still shown (a refresh, or the list put in another order), it keeps its zoom,
    /// turn and place.
    pub(crate) fn replace_files(
        &mut self,
        ctx: &egui::Context,
        files: Vec<OsString>,
        shown: Option<OsString>,
    ) {
        // The list has a new order (shuffled, or a collection's), so only a listing in sort
        // order sets this again.
        self.folder.listed_as = None;
        // The file counts as found when it is shown now and is in the new list too. It does not
        // when another program removed or renamed it, because its turn not saved would
        // otherwise go to the image shown in its place.
        let found = (shown.as_ref()).and_then(|p| files.iter().position(|f| f == p));
        let still = found.is_some() && self.list.files().get(self.list.cur()) == shown.as_ref();
        // The shown file's decodes still being made are kept, so it is not decoded twice. Those
        // of a later page are not kept, because the file is shown again from its first page.
        let jobs: Vec<_> = ([false, true].into_iter())
            .filter(|_| self.viewing.page() == 0)
            .filter_map(|full| Some((full, self.list.take_decode((self.list.cur(), full))?)))
            .collect();
        if !still {
            self.leave_image(ctx);
        }
        // The slots are kept by name, so the neighbours read ahead stay, after a refresh as
        // after a delete.
        let mut slots: HashMap<OsString, Slot> = (self.list.take_slots().into_iter())
            .filter_map(|(i, s)| Some((self.list.files().get(i)?.clone(), s)))
            .collect();
        // The panes are kept by name too, because their places belong to other files in the
        // new list, but their files do not change.
        let panes: Vec<OsString> = (self.list.panes().iter())
            .filter_map(|&i| self.list.files().get(i).cloned())
            .collect();
        // A new List means every place held in the old one goes with it, whatever field holds
        // it. Only what is carried over by name above comes back.
        self.list = List::after(&self.list, files);
        self.exif_follow_list();
        self.selection_changed(ctx);
        let panes = (panes.iter())
            .filter_map(|n| self.list.files().iter().position(|f| f == n))
            .collect();
        self.list.set_panes(panes);
        let cur = found.unwrap_or(0);
        // One pass over the list for a handful of textures.
        let mut kept = Vec::new();
        for (i, name) in self.list.files().iter().enumerate() {
            if slots.is_empty() {
                break;
            }
            if let Some(slot) = slots.remove(name) {
                kept.push((i, slot));
            }
        }
        for (i, slot) in kept {
            if self.is_file(i, &slot.path) {
                self.list.keep_slot(i, slot);
            }
        }
        for (full, job) in jobs {
            if self.is_file(cur, &job.1) {
                self.list.ask_decode((cur, full), job);
            }
        }
        self.update_search(ctx);
        if still {
            self.list.set_cur(cur);
            self.refill();
            self.title(ctx);
            return;
        }
        // go() clears the notice for a new image. A new list is not one, and a notice is often
        // why the list was read again.
        let notice = self.notice.take();
        self.go(ctx, cur);
        self.notice = notice;
    }

    /// Remove the (sorted) indices from the list in one pass and shift index-based state
    /// accordingly. Neighbours' textures are kept, so the next image shows without decoding again.
    pub(crate) fn remove_indices(&mut self, ctx: &egui::Context, removed: &[usize]) {
        if removed.binary_search(&self.list.cur()).is_ok() {
            // The shown image is gone, and all that belonged to it.
            self.viewing = Viewing::default();
        } else {
            self.leave_image(ctx);
        }
        // The panes' places belong to other files now, so they are filled again from the list
        // as it is.
        self.list.remove(removed);
        self.list_changed(ctx);
        self.update_search(ctx);
        // The next remaining file after the shown one (or the last).
        let cur = self.list.cur() - removed.partition_point(|&r| r < self.list.cur());
        if self.list.files().is_empty() {
            self.list.set_cur(0);
            self.title(ctx);
        } else {
            self.go(ctx, cur.min(self.list.files().len() - 1));
        }
    }

    pub(crate) fn receive_sizes(&mut self) {
        self.status.receive();
        let mut size = self.list.match_size();
        self.search.receive_match_size(&mut size);
        self.list.set_match_size(size);
    }

    /// Go → Back and Forward. They go to the folder opened before or after this one.
    pub(crate) fn back_or_forward(&mut self, ctx: &egui::Context, action: Action) {
        let here = self.back_step();
        let (from, to) = if action == Action::Back {
            (&mut self.folder.back, &mut self.folder.forward)
        } else {
            (&mut self.folder.forward, &mut self.folder.back)
        };
        if let Some(path) = from.pop() {
            to.push(here);
            self.notice = None;
            self.open(ctx, path);
        }
    }

    /// View → Show Hidden Files. The list and the tree are read again, scrolled back to the open
    /// folder.
    pub(crate) fn toggle_hidden_files(&mut self, ctx: &egui::Context) {
        self.config.show_hidden = !self.config.show_hidden;
        self.config.save();
        // Read again, and scrolled back to the open folder.
        self.tree.read_again_and_reopen();
        self.list(ctx);
    }

    /// F5. The folder is read and sorted anew (also in an EXIF date order whose names stayed),
    /// and the shown image, the thumbnails and the size of a file edited elsewhere are made
    /// again.
    pub(crate) fn refresh(&mut self, ctx: &egui::Context) {
        // It was asked for, so the list is sorted anew, also in an EXIF date order whose names
        // stayed.
        self.folder.listed_as = None;
        self.tree.read_again();
        self.list.forget_decoded();
        // The image is decoded again now, because a folder read again unchanged keeps its list
        // as it is and asks for nothing. The thumbnails and the size of a file edited elsewhere
        // are made again too.
        self.refill();
        self.thumbs_changed();
        self.status.forget_size();
        self.list(ctx);
    }
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
pub(crate) fn drain(rx: &mut Receiving<u64>, total: &mut u64) {
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

#[cfg(test)]
mod tests {
    use crate::support::test_support::{app_on, contract_folder, settle};

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
        app.list(&ctx);
        assert!(settle(&mut app, &ctx, |app| app
            .folder
            .listing_rx
            .is_none()));
        app.paths_moved(vec![(a, c)], false);
        let has_c = |app: &crate::App| {
            (app.list.files().iter()).any(|f| std::path::Path::new(f).ends_with("c.png"))
        };
        assert!(settle(&mut app, &ctx, |app| app
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
        app.list(&ctx);
        assert!(settle(&mut app, &ctx, |app| app
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
        let cur = app.list.cur();
        assert!(settle(&mut app, &ctx, |a| a
            .list
            .slots()
            .contains_key(&cur)));
        app.run(&ctx, crate::ui::actions::Action::Refresh);
        assert!(settle(&mut app, &ctx, |a| a.folder.listing_rx.is_none()));
        let asked = |a: &crate::App| {
            a.list.slots().contains_key(&cur) || a.list.decodes_pending().keys().any(|k| k.0 == cur)
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
        app.config.sort_case = true;
        app.folder.sort_collection = true;
        app.list(&ctx);
        assert!(settle(&mut app, &ctx, |a| a.folder.listing_rx.is_none()));
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
        app.list(&ctx);
        app.open_collection(&ctx, second.clone());
        assert!(settle(&mut app, &ctx, |a| !a.folder.is_listing()));
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
}
