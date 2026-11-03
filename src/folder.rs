//! Opening a folder, a collection or several folders, listing them, and following changes.

use crate::actions::Action;
use crate::config::Config;
use crate::config::sort_folders_from;
use crate::config::sort_folders_path;
use crate::external::Mounted;
use crate::external::{Login, gio_mount};
use crate::lang::tr;
use crate::list::{List, Slot, Viewing};
use crate::listing::{
    Types, is_listed, list_folder, list_tree, merge_shuffled, shuffle, sort_files, walk,
};
use crate::menus::{iconed, scroll_menu};
use crate::threads::{Receiving, Took};
use crate::{collections, config, external, listing, noted, tf, theme};
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
    pub(crate) dir: PathBuf,
    pub(crate) subdirs: Vec<PathBuf>,
    /// The folder path in the toolbar, as typed.
    pub(crate) path_text: String,
    /// The type of the file opened directly (not the folder), listed even when the Types menu
    /// leaves it out.
    pub(crate) opened_type: Types,
    /// The list holds the images of the subfolders too (slideshow with subfolders).
    pub(crate) recursive: bool,
    /// The open collection's file. The list is then its images, with `dir` "/" and the paths
    /// without their leading "/" as names, so `path()` works unchanged.
    pub(crate) collection: Option<PathBuf>,
    /// The collection's next listing is sorted (Sort menu), and saved in that order.
    pub(crate) sort_collection: bool,
    /// Several folders shown as one list (glimt DIR DIR …, File → Open Folders…): the names
    /// are relative to /, as in a collection, and the folders are read again on refresh.
    pub(crate) folders: Vec<PathBuf>,
    /// Folders to show together, chosen in a folder's menu; opened after the frame.
    pub(crate) together: Option<Vec<PathBuf>>,
    /// Folder listing in the background: (subfolders, images).
    pub(crate) listing_rx: Receiving<(Vec<PathBuf>, Vec<OsString>)>,
    /// The subfolders of a folder listed with its tree, sent at once: the tree may take long.
    pub(crate) subdirs_rx: Receiving<Vec<PathBuf>>,
    /// The images of the tree found so far, as it is read, a folder of it at a time.
    pub(crate) part_rx: Receiving<Vec<OsString>>,
    /// The next listing shows a tree's images as they are found (a folder opened).
    pub(crate) list_parts: bool,
    /// What the list shown was read and sorted as: folder, order, ascending, case, subfolders.
    pub(crate) listed_as: Option<(PathBuf, listing::Sort, bool, bool, bool)>,
    /// The image the first part of a tree went to by itself: the whole list goes to its first.
    pub(crate) went_to: Option<OsString>,
    /// Set to stop the listing of the tree before, when another one starts.
    pub(crate) listing_stop: Arc<AtomicBool>,
    /// The folder changed while a listing was running; list again when it is done.
    pub(crate) relist: bool,
    /// Generation of the folder watcher; opening another folder ends the old thread.
    pub(crate) watching: Arc<AtomicUsize>,
    /// The watcher saw the folder change.
    pub(crate) changed_tx: mpsc::Sender<()>,
    pub(crate) changed_rx: mpsc::Receiver<()>,
    /// File → Open Recent: the folders and collections opened last, newest first.
    pub(crate) recent: Vec<PathBuf>,
    /// Go → Back / Forward: earlier locations (file shown, or folder if it had no images).
    pub(crate) back: Vec<PathBuf>,
    pub(crate) forward: Vec<PathBuf>,
    /// A network location (smb:// …) being mounted: its local path, or why it failed.
    pub(crate) mounting_rx: Receiving<(Mounted, Login)>,
    /// The login dialog for a network share that needs one.
    pub(crate) login: Option<Login>,
    /// The sort manager's folders, kept in `$XDG_CONFIG_HOME/glimt/sort-folders`.
    pub(crate) sort_folders: Vec<PathBuf>,
}

impl Folder {
    /// Nothing open yet; the recent list and the sort manager's folders read from their files.
    pub(crate) fn new(config: &Config) -> Self {
        // No list kept: one of before goes too, also when the setting was changed in the file.
        let recent = match config::recent_path() {
            Some(file) if !config.keep_recent => {
                config::write_later(move || config::remove_shared(&file));
                Vec::new()
            }
            Some(file) => crate::decode::read_limited(file)
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
            sort_folders: (sort_folders_path().and_then(|p| crate::decode::read_limited(p).ok()))
                .map_or_else(Vec::new, |t| sort_folders_from(&t)),
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
    shuffled: bool,
    /// Set when another listing starts: this one stops reading a tree.
    stop: Arc<AtomicBool>,
    tx: mpsc::Sender<(Vec<PathBuf>, Vec<OsString>)>,
    size_tx: mpsc::Sender<u64>,
    ctx: egui::Context,
}

impl Listing {
    /// Several folders: their images, each folder (with its subfolders) after the other.
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
        // Sizes after, as for a folder: the list shows first.
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
            // By name with Case on, sort_files keeps the order as listed, a folder's;
            // a collection's is its own, so it is put in name order here.
            if self.by == listing::Sort::Name && self.case {
                files.sort_unstable();
            }
            sort_files(&self.dir, &mut files, self.by, self.ascending, self.case);
        }
        // A random slideshow with its subfolders: read from the file, in its order.
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

    /// A folder, or its tree; `before`, the list in EXIF date order kept if no name is new,
    /// and `parts` sends a tree's images as they are read.
    fn folder(
        self,
        before: Option<Vec<OsString>>,
        parts: bool,
        part_tx: mpsc::SyncSender<Vec<OsString>>,
        subdirs_tx: mpsc::Sender<Vec<PathBuf>>,
    ) {
        // The folder read once, also with the folders below it. Its own subfolders show at
        // once: a large tree takes long to read whole.
        // Its images too, a folder of it at a time, in name order (the sort asked for comes
        // with the whole tree): at most four times a second, as each is a copy of the list.
        // The first images at once; after them, at most four times a second.
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
                    // The next one after four times as long as this one took, a quarter second
                    // at least: copying a long list must not slow the reading down.
                    let soon = sent.is_some_and(|(t, wait)| t.elapsed() < wait);
                    if !parts || self.shuffled || soon {
                        return;
                    }
                    let began = Instant::now();
                    let mut part = so_far.to_vec();
                    part.sort_unstable();
                    // Full: the one before is not taken yet; this one is left out.
                    if part_tx.try_send(part).is_ok() {
                        self.ctx.request_repaint();
                    }
                    let wait = (began.elapsed() * 4).max(std::time::Duration::from_millis(250));
                    sent = Some((Instant::now(), wait));
                },
            ),
            false => list_folder(&self.dir, self.types),
        };
        // Stopped: another folder is listed now.
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
        // Sizes afterwards: read the folder once more and sum from the directory entries in
        // chunks, so the status bar fills in gradually without a copy of the whole list in
        // memory. A tree a folder at a time, unsorted.
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

    /// Open a folder, or the folder of a file with that file shown.
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
        // Show a given file at once; the folder is listed in the background and the list merged in
        // afterwards.
        let shown = file
            .as_deref()
            .and_then(Path::file_name)
            .map(OsString::from);
        self.forget_old_list();
        // A file of a type the menu does not list (a RAW with RAW off) is listed with the
        // folder it opens; a folder or several opened later go by the menu again.
        self.folder.opened_type = file.as_deref().map_or(Types::NONE, Types::of);
        // Another folder: its file of the same name is another image, which opens as one, not
        // at the zoom and place of the image before.
        if dir != self.folder.dir {
            self.list.files.clear();
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

    /// `path` first in File → Open Recent, also in the file the other windows read when they
    /// start; merged with what they wrote meanwhile.
    pub(crate) fn remember_recent(&mut self, path: &Path) {
        if !self.config.keep_recent || self.folder.recent.first().is_some_and(|p| p == path) {
            return;
        }
        self.folder.recent = config::add_recent(&self.folder.recent, path);
        let path = path.to_path_buf();
        config::write_later(move || {
            let Some(file) = config::recent_path() else {
                return;
            };
            // As every file the windows share: two windows opening at once keep both, and one
            // that cannot be read is not written over.
            let add = |old: &[u8]| {
                let all = config::add_recent(&config::parse_recent(old), &path);
                (config::recent_text(&all), ())
            };
            if let Err(e) = config::edit_shared(&file, add) {
                config::save_failed(format!("Could not save {}: {e}", file.display()));
            }
        });
    }

    /// Open Recent emptied, in its file too.
    pub(crate) fn clear_recent(&mut self) {
        self.folder.recent.clear();
        if let Some(file) = config::recent_path() {
            config::write_later(move || config::remove_shared(&file));
        }
    }

    /// File → Open Recent: the folders and collections opened last, and Clear Recent.
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
                // Gone since (deleted, a stick taken out): said so, and out of the list.
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
        // A check still running finds names of the old list: stopped, its window closed.
        if let Some(Some(check)) = self.check.take() {
            check.stop.store(true, Ordering::Relaxed);
        }
        self.folder.opened_type = Types::NONE;
        // The old folder's watcher ends; a folder opened next starts its own (watch()).
        self.folder.watching.fetch_add(1, Ordering::Relaxed);
        self.broken_files.clear();
        self.folder.subdirs.clear();
        self.work.deleting.clear();
        self.slideshow.shuffled = false;
        self.thumbs.textures.clear();
        self.thumbs.pending.clear();
        self.search.exif_index = None;
        self.search.exif_stale.clear();
        self.search.exif_index_rx = Receiving::none();
        // Its reading of every file ends: no one waits for it now.
        self.search.exif_index_stop.store(true, Ordering::Relaxed);
    }

    /// Show the collection in `file` as the list.
    pub(crate) fn open_collection(&mut self, ctx: &egui::Context, file: PathBuf) {
        // Absolute from here on: kept in Open Recent and as the last folder, a relative name
        // was another file, or none, for a window started elsewhere.
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
        // The same collection read again (F5): a delete still running keeps its names, so a
        // failed one brings its images back.
        let deleting = if self.folder.collection.as_ref() == Some(&file) {
            std::mem::take(&mut self.work.deleting)
        } else {
            HashSet::new()
        };
        self.forget_old_list();
        self.work.deleting = deleting;
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

    /// Show the images of several folders as one list, for a slideshow across them; one folder
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
        // One folder (several files of it, from a file manager): opened at the first file.
        if folders.len() < 2 {
            if let Some(first) = first {
                self.open(ctx, first);
            }
            return;
        }
        self.leave_image(ctx);
        self.folder.collection = None;
        // As a folder opened: with the subfolders when the menu says so, whatever was shown.
        self.folder.recursive = self.config.show_subfolders;
        self.forget_old_list();
        self.folder.dir = PathBuf::from("/");
        self.folder.folders = folders;
        self.replace_files(ctx, Vec::new(), None);
        self.list(ctx);
        self.title(ctx);
    }

    /// Where Back comes back to: the collection shown, else the file shown or the folder.
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
        // Not without end: a slideshow of folders, or a script's --remote open, visits many.
        if self.folder.back.len() > 200 {
            self.folder.back.remove(0);
        }
        self.folder.forward.clear();
        self.open(ctx, path);
    }

    /// The file shown, or the folder when it has no images.
    pub(crate) fn location(&self) -> PathBuf {
        match self.list.files.get(self.list.cur) {
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
        // Says which address is tried; replaces what an attempt before left.
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

    /// Read the folder (and then the sizes) in the background; `receive_listing` merges it in.
    /// A collection is checked for files that are gone instead.
    pub(crate) fn list(&mut self, ctx: &egui::Context) {
        let (tx, rx) = mpsc::channel();
        let (size_tx, size_rx) = mpsc::channel();
        let (list_dir, ctx2, types) = (self.folder.dir.clone(), ctx.clone(), self.types());
        let c = &self.config;
        let (by, ascending, case) = (c.sort, c.sort_ascending, c.sort_case);
        let (recursive, shuffled) = (
            self.folder.recursive,
            self.slideshow.shuffled && self.slideshow.whole_shuffle,
        );
        let collection = self
            .folder
            .collection
            .is_some()
            .then(|| self.list.files.clone());
        // A collection's file, which every move writes at once: read again, it has what moved
        // after the list was copied. A plain list is never written: the list as shown.
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
        let before =
            (exif && !self.folder.list_parts && !self.slideshow.shuffled && collection.is_none())
                .then_some(())
                .filter(|_| folders.is_empty() && self.folder.listed_as == same)
                .map(|_| self.list.files.clone());
        // The listing before stops, if it is still reading a tree.
        self.folder.listing_stop.store(true, Ordering::Relaxed);
        self.folder.listing_stop = Arc::default();
        let stop = self.folder.listing_stop.clone();
        let (subdirs_tx, subdirs_rx) = mpsc::channel();
        self.folder.subdirs_rx = Receiving::of(subdirs_rx);
        // Room for one: a window not drawn (minimized, on another workspace) takes none, and a
        // copy of the whole list so far each time would pile up.
        let (part_tx, part_rx) = mpsc::sync_channel(1);
        self.folder.part_rx = Receiving::of(part_rx);
        // Only for a folder opened, or its tree asked for: a folder read again (a refresh) keeps
        // its whole list on screen until the new one is read, instead of shrinking and growing.
        // Nor in random order: a list in name order would take the shuffled one's place.
        let parts = std::mem::take(&mut self.folder.list_parts) && !self.slideshow.shuffled;
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
        self.status.size_rx = Receiving::of(size_rx);
        self.status.total_size = 0;
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
        // The tree's images so far: only the last that came, one list a frame at most.
        if let Took::Answer(part) = self.folder.part_rx.latest() {
            self.take_files(ctx, part, false);
        }
        let (dirs, files) = match self.folder.listing_rx.wait(wait) {
            Took::Answer(listed) => listed,
            // The listing thread broke: told once, and nothing (the busy pointer, a parked
            // --remote select, auto refresh) waits for it any more.
            Took::Gone => {
                self.folder.subdirs_rx.clear();
                self.folder.part_rx.clear();
                return self.stopped(tr("Reading the folder"));
            }
            Took::Nothing => return,
        };
        self.folder.subdirs_rx.clear();
        self.folder.part_rx.clear();
        // Several folders shown together are listed as "/" with no subfolders: not the tree's.
        if self.folder.collection.is_none() && self.folder.folders.is_empty() {
            self.tree
                .children
                .insert(self.folder.dir.clone(), Some(dirs.clone()));
        }
        self.folder.subdirs = dirs;
        self.take_files(ctx, files, true);
        let c = &self.config;
        self.folder.listed_as = (self.folder.collection.is_none()
            && self.folder.folders.is_empty()
            && !self.slideshow.shuffled)
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

    /// The images of the folder read in the background as the list: all of them when `whole`,
    /// else those of a tree found so far. The shown image and the selection stay.
    pub(crate) fn take_files(&mut self, ctx: &egui::Context, files: Vec<OsString>, whole: bool) {
        // Files still being deleted in the background must not come back with a refresh.
        let mut files = files;
        if !self.work.deleting.is_empty() {
            files.retain(|f| !self.work.deleting.contains(f));
        }
        // A refresh during random order keeps the order, so Previous still goes back
        // through the images that were shown; a list shuffled whole in the thread stays as it is.
        // The flag is for this listing, taken whether or not the order is still random: left,
        // it would shuffle every later listing.
        let fresh = whole && std::mem::take(&mut self.slideshow.whole_shuffle);
        let mut files = if whole && self.slideshow.shuffled && !fresh {
            merge_shuffled(&self.list.files, files)
        } else {
            files
        };
        // Shuffled whole for a random slideshow: the image shown first, as R puts it, so a
        // slideshow without Repeat shows all before it ends.
        if fresh
            && self.slideshow.shuffled
            && let Some(at) = (self.list.files.get(self.list.cur))
                .and_then(|shown| files.iter().position(|f| f == shown))
        {
            files.swap(0, at);
        }
        // Nothing changed (most refreshes, after Glimt's own writes beside the files): the list,
        // the selection and the search stay as they are.
        if files == self.list.files && !self.folder.sort_collection {
            // A file saved over under its own name (an editor writes and renames): its size
            // and date read again, for the rows on screen only.
            self.list.details.clear();
            return;
        }
        // The image Glimt went to itself, in the name order of a tree's first images: the whole
        // list, in the order asked for, starts at its first image instead, unless another one
        // was chosen meanwhile.
        let mut shown = self.list.files.get(self.list.cur).cloned();
        if whole && shown.is_some() && std::mem::take(&mut self.folder.went_to) == shown {
            shown = None;
        }
        let first = self.list.files.is_empty() && !whole;
        // A refresh keeps the selection: Delete or Move after it still acts on what was picked.
        let picked: HashSet<OsString> = (self.list.selected.iter())
            .filter_map(|&i| self.list.files.get(i).cloned())
            .collect();
        self.replace_files(ctx, files, shown);
        if first {
            self.folder.went_to = self.list.files.get(self.list.cur).cloned();
        }
        self.reselect(ctx, &picked);
        if whole && std::mem::take(&mut self.folder.sort_collection) {
            self.save_collection_order();
        }
    }

    /// Files taken out of the list, its places shifted: what holds places in it is let go, in
    /// the one place (a list read again gets a new `List` instead). The selection (told to the
    /// status bar), the anchor, a rectangle or a collection drag, and the decodes in flight,
    /// which have old places and are rejected by path when they arrive; refill asks for them
    /// again.
    pub(crate) fn list_changed(&mut self, ctx: &egui::Context) {
        self.list.version += 1;
        self.list.selected.clear();
        self.selection_changed(ctx);
        self.list.anchor = None;
        self.list.rubber_band = None;
        self.list.dragging = None;
        self.list.decodes_pending.clear();
    }

    /// The selection again by name, after the list was read again or put in another order: a
    /// Delete or Move after it still acts on what was picked.
    pub(crate) fn reselect(&mut self, ctx: &egui::Context, names: &HashSet<OsString>) {
        if names.is_empty() {
            return;
        }
        self.list.selected = (self.list.files.iter().enumerate())
            .filter(|(_, f)| names.contains(*f))
            .map(|(i, _)| i)
            .collect();
        self.selection_changed(ctx);
    }

    /// Replace the file list and go to `shown` (otherwise the first image). Textures are kept
    /// for the files they were made from, at their new places. All other index-based state is
    /// reset; jobs in flight are rejected by path when they arrive. The same file still shown (a
    /// refresh, the list put in another order) keeps its zoom, turn and place.
    pub(crate) fn replace_files(
        &mut self,
        ctx: &egui::Context,
        files: Vec<OsString>,
        shown: Option<OsString>,
    ) {
        // A new order (shuffled, a collection's): set again only by a listing in sort order.
        self.folder.listed_as = None;
        // Shown now and in the new list too: not when another program removed or renamed it,
        // whose turn not saved would else go to the image shown in its place.
        let found = (shown.as_ref()).and_then(|p| files.iter().position(|f| f == p));
        let still = found.is_some() && self.list.files.get(self.list.cur) == shown.as_ref();
        // The shown file's decodes still being made: kept, so it is not decoded twice. Not
        // those of a later page, as the file is shown again from its first.
        let jobs: Vec<_> = ([false, true].into_iter())
            .filter(|_| self.viewing.page == 0)
            .filter_map(|full| {
                Some((
                    full,
                    self.list.decodes_pending.remove(&(self.list.cur, full))?,
                ))
            })
            .collect();
        if !still {
            self.leave_image(ctx);
        }
        // By name: the neighbours read ahead stay, after a refresh as after a delete.
        let mut slots: HashMap<OsString, Slot> = (self.list.slots.drain())
            .filter_map(|(i, s)| Some((self.list.files.get(i)?.clone(), s)))
            .collect();
        // The panes too: their places are other files' in the new list, their files are not.
        let panes: Vec<OsString> = (self.list.panes.iter())
            .filter_map(|&i| self.list.files.get(i).cloned())
            .collect();
        // A new List: every place held in the old one goes with it, whatever field holds it;
        // only what is carried over by name above comes back.
        self.list = List {
            files,
            version: self.list.version + 1,
            ..Default::default()
        };
        self.exif_follow_list();
        self.selection_changed(ctx);
        self.list.panes = (panes.iter())
            .filter_map(|n| self.list.files.iter().position(|f| f == n))
            .collect();
        let cur = found.unwrap_or(0);
        // One pass over the list for a handful of textures.
        let mut kept = Vec::new();
        for (i, name) in self.list.files.iter().enumerate() {
            if slots.is_empty() {
                break;
            }
            if let Some(slot) = slots.remove(name) {
                kept.push((i, slot));
            }
        }
        for (i, slot) in kept {
            if self.is_file(i, &slot.path) {
                self.list.slots.insert(i, slot);
            }
        }
        for (full, job) in jobs {
            if self.is_file(cur, &job.1) {
                self.list.decodes_pending.insert((cur, full), job);
            }
        }
        self.update_search(ctx);
        if still {
            self.list.cur = cur;
            self.refill();
            self.title(ctx);
            return;
        }
        // go() clears the notice for a new image; a new list is not one, and a notice is often
        // why the list was read again.
        let notice = self.notice.take();
        self.go(ctx, cur);
        self.notice = notice;
    }

    /// Remove the (sorted) indices from the list in one pass and shift index-based state
    /// accordingly. Neighbours' textures are kept, so the next image shows without decoding again.
    pub(crate) fn remove_indices(&mut self, ctx: &egui::Context, removed: &[usize]) {
        if removed.binary_search(&self.list.cur).is_ok() {
            // The shown image is gone, and all that belonged to it.
            self.viewing = Viewing::default();
        } else {
            self.leave_image(ctx);
        }
        let mut idx = 0;
        self.list.files.retain(|_| {
            idx += 1;
            removed.binary_search(&(idx - 1)).is_err()
        });
        self.list_changed(ctx);
        self.list.slots = without_indices(std::mem::take(&mut self.list.slots), removed);
        self.list.decode_errors =
            without_indices(std::mem::take(&mut self.list.decode_errors), removed);
        // The panes' places are other files' now: filled again from the list as it is.
        self.list.panes.clear();
        self.update_search(ctx);
        // The next remaining file after the shown one (or the last).
        let cur = self.list.cur - removed.partition_point(|&r| r < self.list.cur);
        if self.list.files.is_empty() {
            self.list.cur = 0;
            self.title(ctx);
        } else {
            self.go(ctx, cur.min(self.list.files.len() - 1));
        }
    }

    pub(crate) fn receive_sizes(&mut self) {
        while let Ok(freed) = self.status.freed_rx.try_recv() {
            // shortcut: a delete while the sum is still being computed can make it slightly off;
            // fine for a status bar figure.
            self.status.total_size = self.status.total_size.saturating_sub(freed);
        }
        match self.status.selected_rx.take() {
            Took::Answer(size) => self.status.selected_size = Some(size),
            // The summing thread broke: no size, instead of "calculating…" for good.
            Took::Gone => self.status.selected_size = Some(0),
            Took::Nothing => {}
        }
        drain(&mut self.search.match_rx, &mut self.list.match_size);
        drain(&mut self.status.size_rx, &mut self.status.total_size);
    }

    /// Go → Back and Forward: the folder opened before or after this one.
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

    /// View → Show Hidden Files: the list and the tree read again, scrolled back to the open
    /// folder.
    pub(crate) fn toggle_hidden_files(&mut self, ctx: &egui::Context) {
        self.config.show_hidden = !self.config.show_hidden;
        self.config.save();
        // Read again, and scrolled back to the open folder.
        self.tree.children.clear();
        self.tree.shown = PathBuf::new();
        self.list(ctx);
    }

    /// F5: the folder read and sorted anew (also in an EXIF date order whose names stayed), and the
    /// shown image, thumbnails and size of a file edited elsewhere made again.
    pub(crate) fn refresh(&mut self, ctx: &egui::Context) {
        // Asked for: sorted anew, also in an EXIF date order whose names stayed.
        self.folder.listed_as = None;
        self.tree.children.clear();
        self.list.slots.clear();
        self.list.decodes_pending.clear();
        self.list.decode_errors.clear();
        // Decoded again now: a folder read again unchanged keeps its list as it is and
        // asks for nothing. Thumbnails and the size too, of a file edited elsewhere.
        self.refill();
        self.thumbs_changed();
        self.status.cur_size = None;
        self.list(ctx);
    }
}

/// Drop the entries for `removed` (sorted) and shift the rest down by the number removed
/// before them.
pub(crate) fn without_indices<T>(map: HashMap<usize, T>, removed: &[usize]) -> HashMap<usize, T> {
    map.into_iter()
        .filter_map(|(i, v)| Some((i - removed.binary_search(&i).err()?, v)))
        .collect()
}

/// Send sizes as partial sums of 10,000 files, so the status bar fills in gradually; stops
/// when the receiver is gone. The closed channel tells the UI the sum is complete.
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

/// Add the partial sums from `send_sizes` that have arrived; `rx` becomes `None` when done.
pub(crate) fn drain(rx: &mut Receiving<u64>, total: &mut u64) {
    while let Took::Answer(sum) = rx.next() {
        *total += sum;
    }
}

/// Sum of the file sizes (one stat per file); unreadable files are not counted.
pub(crate) fn total_size(paths: &[PathBuf]) -> u64 {
    paths
        .iter()
        .filter_map(|p| p.metadata().ok())
        .map(|m| m.len())
        .sum()
}

#[cfg(test)]
mod tests {
    use crate::tests::{app_on, contract_folder, settle};

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
            (app.list.files.iter()).any(|f| std::path::Path::new(f).ends_with("c.png"))
        };
        assert!(settle(&mut app, &ctx, |app| app
            .folder
            .listing_rx
            .is_none()
            && has_c(app)));
        assert!(std::fs::read_to_string(&gqv).unwrap().contains("c.png"));
        std::fs::remove_dir_all(d).unwrap();
    }

    /// A folder in EXIF date order read again keeps its order, but Refresh sorts it anew: a date
    /// changed by another program under the same name takes its place.
    #[test]
    fn refresh_sorts_an_exif_date_order_anew() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-exif-order-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a.png", "b.png"] {
            image::RgbImage::new(2, 2).save(dir.join(name)).unwrap();
        }
        let (mut app, ctx) = app_on(&dir);
        app.config.sort = crate::listing::Sort::ExifOriginal;
        app.list(&ctx);
        assert!(settle(&mut app, &ctx, |app| app
            .folder
            .listing_rx
            .is_none()));
        assert!(app.folder.listed_as.is_some());
        app.run(&ctx, crate::actions::Action::Refresh);
        assert!(app.folder.listed_as.is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Refresh in a folder that has not changed decodes the image shown again (an editor may
    /// have saved it under its own name): it asked for nothing, and the busy pointer stayed.
    #[test]
    fn refresh_decodes_the_shown_image_again() {
        let dir = std::env::temp_dir().join(format!("glimt-test-refresh-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbImage::new(4, 4).save(dir.join("a.png")).unwrap();
        let (mut app, ctx) = app_on(&dir);
        let cur = app.list.cur;
        assert!(settle(&mut app, &ctx, |a| a.list.slots.contains_key(&cur)));
        app.run(&ctx, crate::actions::Action::Refresh);
        assert!(settle(&mut app, &ctx, |a| a.folder.listing_rx.is_none()));
        let asked = |a: &crate::App| {
            a.list.slots.contains_key(&cur) || a.list.decodes_pending.keys().any(|k| k.0 == cur)
        };
        assert!(asked(&app), "the image shown decoded again");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Sort by name with Case on (the default) puts a collection in name order: it kept the
    /// collection's own.
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
        app.config.sort = crate::listing::Sort::Name;
        app.config.sort_case = true;
        app.folder.sort_collection = true;
        app.list(&ctx);
        assert!(settle(&mut app, &ctx, |a| a.folder.listing_rx.is_none()));
        let names: Vec<String> = (app.list.files.iter())
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

    /// A list opened by a relative name is the collection by its absolute path, as Open Recent
    /// and the last folder keep it: a window started elsewhere opened another list or none.
    #[test]
    fn a_list_opened_by_a_relative_name_is_kept_absolute() {
        let dir = std::path::PathBuf::from(format!("glimt-test-app-list-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbImage::new(4, 4).save(dir.join("a.png")).unwrap();
        std::fs::write(dir.join("show.m3u"), "a.png\n").unwrap();
        let (app, _ctx) = app_on(&dir.join("show.m3u"));
        let here = std::env::current_dir().unwrap().join(&dir);
        assert_eq!(app.folder.collection, Some(here.join("show.m3u")));
        assert_eq!(app.list.files.len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn without_indices_shifts_down() {
        let map = std::collections::HashMap::from([(1, 'a'), (2, 'b'), (4, 'c'), (6, 'd')]);
        let expected = std::collections::HashMap::from([(1, 'a'), (3, 'd')]);
        assert_eq!(super::without_indices(map, &[2, 4, 5]), expected);
    }
}
