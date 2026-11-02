//! Opening a folder, a collection or several folders, listing them, and following changes.

use crate::external::{Login, gio_mount};
use crate::files::{
    Types, is_listed, list_folder, list_tree, merge_shuffled, shuffle, sort_files, walk,
};
use crate::format::fill_template;
use crate::lang::tr;
use crate::menus::{iconed, scroll_menu};
use crate::threads::{Receiving, Took};
use crate::{List, Slot, Viewing, collections, config, external, files, noted, remote, tf, theme};
use eframe::egui;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, mpsc};
use std::time::Instant;

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
        self.collection = None;
        self.folders.clear();
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
        self.forget_old_list();
        // A file of a type the menu does not list (a RAW with RAW off) is listed with the
        // folder it opens; a folder or several opened later go by the menu again.
        self.opened_type = file.as_deref().map_or(Types::NONE, Types::of);
        // Another folder: its file of the same name is another image, which opens as one, not
        // at the zoom and place of the image before.
        if dir != self.dir {
            self.list.files.clear();
        }
        self.recursive = self.config.show_subfolders;
        self.dir = dir.clone();
        self.remember_recent(&dir);
        self.replace_files(ctx, shown.iter().cloned().collect(), shown);
        self.list_parts = true;
        self.list(ctx);
        self.watch(ctx);
        self.title(ctx);
    }

    /// `path` first in File → Open Recent, also in the file the other windows read when they
    /// start; merged with what they wrote meanwhile.
    pub(crate) fn remember_recent(&mut self, path: &Path) {
        if !self.config.keep_recent || self.recent.first().is_some_and(|p| p == path) {
            return;
        }
        self.recent = config::add_recent(&self.recent, path);
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
        self.recent.clear();
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
                if self.recent.is_empty() {
                    ui.weak(tr("Nothing opened yet"));
                    return;
                }
                let mut chosen = None;
                for path in &self.recent {
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
                            self.recent.retain(|p| *p != path);
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
        self.opened_type = Types::NONE;
        // The old folder's watcher ends; a folder opened next starts its own (watch()).
        self.watching.fetch_add(1, Ordering::Relaxed);
        self.broken_files.clear();
        self.subdirs.clear();
        self.deleting.clear();
        self.shuffled = false;
        self.thumbs.clear();
        self.thumbs_pending.clear();
        self.exif_index = None;
        self.exif_stale.clear();
        self.exif_index_rx = Receiving::none();
        // Its reading of every file ends: no one waits for it now.
        self.exif_index_stop.store(true, Ordering::Relaxed);
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
        let deleting = if self.collection.as_ref() == Some(&file) {
            std::mem::take(&mut self.deleting)
        } else {
            HashSet::new()
        };
        self.forget_old_list();
        self.deleting = deleting;
        self.recursive = false;
        self.dir = root.to_path_buf();
        self.remember_recent(&file);
        self.collection = Some(file);
        self.folders.clear();
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
        self.collection = None;
        // As a folder opened: with the subfolders when the menu says so, whatever was shown.
        self.recursive = self.config.show_subfolders;
        self.forget_old_list();
        self.dir = PathBuf::from("/");
        self.folders = folders;
        self.replace_files(ctx, Vec::new(), None);
        self.list(ctx);
        self.title(ctx);
    }

    /// Where Back comes back to: the collection shown, else the file shown or the folder.
    pub(crate) fn back_step(&self) -> PathBuf {
        self.collection.clone().unwrap_or_else(|| self.location())
    }

    /// Open `path` as a new step in the Back/Forward history.
    pub(crate) fn visit(&mut self, ctx: &egui::Context, path: PathBuf) {
        // A message about the folder before (a share that could not be mounted) goes with it.
        self.notice = None;
        self.back.push(self.back_step());
        // Not without end: a slideshow of folders, or a script's --remote open, visits many.
        if self.back.len() > 200 {
            self.back.remove(0);
        }
        self.forward.clear();
        self.open(ctx, path);
    }

    /// The file shown, or the folder when it has no images.
    pub(crate) fn location(&self) -> PathBuf {
        match self.list.files.get(self.list.cur) {
            Some(name) => self.dir.join(name),
            None => self.dir.clone(),
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
        self.mounting_rx = Receiving::of(rx);
    }

    /// Read the folder (and then the sizes) in the background; `receive_listing` merges it in.
    /// A collection is checked for files that are gone instead.
    pub(crate) fn list(&mut self, ctx: &egui::Context) {
        let (tx, rx) = mpsc::channel();
        let (size_tx, size_rx) = mpsc::channel();
        let (list_dir, ctx2, types) = (self.dir.clone(), ctx.clone(), self.types());
        let c = &self.config;
        let (by, ascending, case) = (c.sort, c.sort_ascending, c.sort_case);
        let (recursive, shuffled) = (self.recursive, self.shuffled && self.whole_shuffle);
        let collection = self.collection.is_some().then(|| self.list.files.clone());
        // A collection's file, which every move writes at once: read again, it has what moved
        // after the list was copied. A plain list is never written: the list as shown.
        let saved = (self.collection.clone()).filter(|f| !collections::is_list(f));
        // A collection keeps its own order unless the Sort menu was used.
        let sort = collection.is_none() || self.sort_collection;
        let folders = self.folders.clone();
        // A folder read again in EXIF date order (mostly after Glimt's own sidecar writes) keeps
        // the order shown when no name is new, instead of reading every file's EXIF again.
        // shortcut: the list copied in the UI thread, 7-9 ms for 500,000 names, once per
        // refresh; share it with the thread (an Arc in List) if folders grow far past that.
        let exif = matches!(by, files::Sort::ExifOriginal | files::Sort::ExifDigitized);
        let same = Some((self.dir.clone(), by, ascending, case, recursive));
        let before = (exif && !self.list_parts && !self.shuffled && collection.is_none())
            .then_some(())
            .filter(|_| folders.is_empty() && self.listed_as == same)
            .map(|_| self.list.files.clone());
        // The listing before stops, if it is still reading a tree.
        self.listing_stop.store(true, Ordering::Relaxed);
        self.listing_stop = Arc::default();
        let stop = self.listing_stop.clone();
        let (subdirs_tx, subdirs_rx) = mpsc::channel();
        self.subdirs_rx = Receiving::of(subdirs_rx);
        // Room for one: a window not drawn (minimized, on another workspace) takes none, and a
        // copy of the whole list so far each time would pile up.
        let (part_tx, part_rx) = mpsc::sync_channel(1);
        self.part_rx = Receiving::of(part_rx);
        // Only for a folder opened, or its tree asked for: a folder read again (a refresh) keeps
        // its whole list on screen until the new one is read, instead of shrinking and growing.
        // Nor in random order: a list in name order would take the shuffled one's place.
        let parts = std::mem::take(&mut self.list_parts) && !self.shuffled;
        self.went_to = None;
        std::thread::spawn(move || {
            // Several folders: their images, each folder (with its subfolders) after the other.
            if !folders.is_empty() {
                let rel = |dir: &Path, names: Vec<OsString>| -> Vec<OsString> {
                    let at = dir.strip_prefix("/").unwrap_or(dir).to_path_buf();
                    (names.into_iter())
                        .map(|n| at.join(n).into_os_string())
                        .collect()
                };
                let mut files: Vec<OsString> = (folders.iter())
                    .flat_map(|d| match recursive {
                        true => rel(d, list_tree(d, types, &stop, |_| {}, |_| {}).1),
                        false => rel(d, list_folder(d, types).1),
                    })
                    .collect();
                // A folder inside another one given has its images listed once.
                let mut seen = HashSet::new();
                files.retain(|f| seen.insert(f.clone()));
                sort_files(&list_dir, &mut files, by, ascending, case);
                if shuffled {
                    shuffle(&mut files);
                }
                // Sizes after, as for a folder: the list shows first.
                let paths: Vec<PathBuf> = files.iter().map(|f| list_dir.join(f)).collect();
                if tx.send((Vec::new(), files)).is_ok() {
                    ctx2.request_repaint();
                    let sizes = paths
                        .into_iter()
                        .filter_map(|p| Some(p.metadata().ok()?.len()));
                    send_sizes(sizes, size_tx, &ctx2);
                }
                return;
            }
            if let Some(mut files) = collection {
                if let Some(Ok(paths)) = saved.map(|f| collections::load(&f)) {
                    files = (paths.iter())
                        .filter_map(|p| Some(p.strip_prefix("/").ok()?.as_os_str().to_owned()))
                        .collect();
                }
                files.retain(|f| list_dir.join(f).exists());
                if sort {
                    // By name with Case on, sort_files keeps the order as listed, a folder's;
                    // a collection's is its own, so it is put in name order here.
                    if by == files::Sort::Name && case {
                        files.sort_unstable();
                    }
                    sort_files(&list_dir, &mut files, by, ascending, case);
                }
                // A random slideshow with its subfolders: read from the file, in its order.
                if shuffled {
                    shuffle(&mut files);
                }
                let sizes: Vec<u64> = (files.iter())
                    .filter_map(|f| Some(list_dir.join(f).metadata().ok()?.len()))
                    .collect();
                if tx.send((Vec::new(), files)).is_ok() {
                    ctx2.request_repaint();
                    send_sizes(sizes.into_iter(), size_tx, &ctx2);
                }
                return;
            }
            // The folder read once, also with the folders below it. Its own subfolders show at
            // once: a large tree takes long to read whole.
            // Its images too, a folder of it at a time, in name order (the sort asked for comes
            // with the whole tree): at most four times a second, as each is a copy of the list.
            // The first images at once; after them, at most four times a second.
            let mut sent: Option<(Instant, std::time::Duration)> = None;
            let (dirs, mut files) = match recursive {
                true => list_tree(
                    &list_dir,
                    types,
                    &stop,
                    |dirs| {
                        if subdirs_tx.send(dirs.to_vec()).is_ok() {
                            ctx2.request_repaint();
                        }
                    },
                    |so_far| {
                        // The next one after four times as long as this one took, a quarter second
                        // at least: copying a long list must not slow the reading down.
                        let soon = sent.is_some_and(|(t, wait)| t.elapsed() < wait);
                        if !parts || shuffled || soon {
                            return;
                        }
                        let began = Instant::now();
                        let mut part = so_far.to_vec();
                        part.sort_unstable();
                        // Full: the one before is not taken yet; this one is left out.
                        if part_tx.try_send(part).is_ok() {
                            ctx2.request_repaint();
                        }
                        let wait = (began.elapsed() * 4).max(std::time::Duration::from_millis(250));
                        sent = Some((Instant::now(), wait));
                    },
                ),
                false => list_folder(&list_dir, types),
            };
            // Stopped: another folder is listed now.
            if stop.load(Ordering::Relaxed) {
                return;
            }
            match before.and_then(|before| files::kept_order(before, &files)) {
                Some(kept) => files = kept,
                None => sort_files(&list_dir, &mut files, by, ascending, case),
            }
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
            // A tree a folder at a time, unsorted.
            if recursive {
                let sizes = walk(&list_dir, types).flat_map(|(rel, _, files)| {
                    let folder = list_dir.join(rel);
                    (files.into_iter())
                        .filter_map(move |f| Some(folder.join(f).metadata().ok()?.len()))
                });
                return send_sizes(sizes, size_tx, &ctx2);
            }
            let images = std::fs::read_dir(&list_dir)
                .into_iter()
                .flatten()
                .flatten()
                .filter(|e| files::kind(e).0 && is_listed(&e.path(), types));
            // Through a link, the size of the file it points to, as the list shows that.
            let sizes = images.filter_map(|e| Some(std::fs::metadata(e.path()).ok()?.len()));
            send_sizes(sizes, size_tx, &ctx2);
        });
        self.listing_rx = Receiving::of(rx);
        self.size_rx = Receiving::of(size_rx);
        self.total_size = 0;
        self.relist = false;
    }

    /// Poll the folder's modification time once a second in the background (one `stat`, no
    /// repaint unless it changed). Adding, removing or renaming a file changes it.
    /// The thread ends when another folder is opened.
    pub(crate) fn watch(&mut self, ctx: &egui::Context) {
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

    /// Take the folder listing if it is ready, waiting up to `wait`.
    pub(crate) fn receive_listing(&mut self, ctx: &egui::Context, wait: std::time::Duration) {
        // A thread that broke leaves no folders, and nothing waiting for it.
        if let Took::Answer(dirs) = self.subdirs_rx.take() {
            self.subdirs = dirs;
        }
        // The tree's images so far: only the last that came, one list a frame at most.
        if let Took::Answer(part) = self.part_rx.latest() {
            self.take_files(ctx, part, false);
        }
        let (dirs, files) = match self.listing_rx.wait(wait) {
            Took::Answer(listed) => listed,
            // The listing thread broke: told once, and nothing (the busy pointer, a parked
            // --remote select, auto refresh) waits for it any more.
            Took::Gone => {
                self.subdirs_rx.clear();
                self.part_rx.clear();
                return self.stopped(tr("Reading the folder"));
            }
            Took::Nothing => return,
        };
        self.subdirs_rx.clear();
        self.part_rx.clear();
        // Several folders shown together are listed as "/" with no subfolders: not the tree's.
        if self.collection.is_none() && self.folders.is_empty() {
            self.tree.insert(self.dir.clone(), Some(dirs.clone()));
        }
        self.subdirs = dirs;
        self.take_files(ctx, files, true);
        let c = &self.config;
        self.listed_as = (self.collection.is_none() && self.folders.is_empty() && !self.shuffled)
            .then(|| {
                (
                    self.dir.clone(),
                    c.sort,
                    c.sort_ascending,
                    c.sort_case,
                    self.recursive,
                )
            });
    }

    /// The images of the folder read in the background as the list: all of them when `whole`,
    /// else those of a tree found so far. The shown image and the selection stay.
    pub(crate) fn take_files(&mut self, ctx: &egui::Context, files: Vec<OsString>, whole: bool) {
        // Files still being deleted in the background must not come back with a refresh.
        let mut files = files;
        if !self.deleting.is_empty() {
            files.retain(|f| !self.deleting.contains(f));
        }
        // A refresh during random order keeps the order, so Previous still goes back
        // through the images that were shown; a list shuffled whole in the thread stays as it is.
        // The flag is for this listing, taken whether or not the order is still random: left,
        // it would shuffle every later listing.
        let fresh = whole && std::mem::take(&mut self.whole_shuffle);
        let mut files = if whole && self.shuffled && !fresh {
            merge_shuffled(&self.list.files, files)
        } else {
            files
        };
        // Shuffled whole for a random slideshow: the image shown first, as R puts it, so a
        // slideshow without Repeat shows all before it ends.
        if fresh
            && self.shuffled
            && let Some(at) = (self.list.files.get(self.list.cur))
                .and_then(|shown| files.iter().position(|f| f == shown))
        {
            files.swap(0, at);
        }
        // Nothing changed (most refreshes, after Glimt's own writes beside the files): the list,
        // the selection and the search stay as they are.
        if files == self.list.files && !self.sort_collection {
            // A file saved over under its own name (an editor writes and renames): its size
            // and date read again, for the rows on screen only.
            self.list.details.clear();
            return;
        }
        // The image Glimt went to itself, in the name order of a tree's first images: the whole
        // list, in the order asked for, starts at its first image instead, unless another one
        // was chosen meanwhile.
        let mut shown = self.list.files.get(self.list.cur).cloned();
        if whole && shown.is_some() && std::mem::take(&mut self.went_to) == shown {
            shown = None;
        }
        let first = self.list.files.is_empty() && !whole;
        // A refresh keeps the selection: Delete or Move after it still acts on what was picked.
        let picked: HashSet<OsString> = (self.list.selected.iter())
            .filter_map(|&i| self.list.files.get(i).cloned())
            .collect();
        self.replace_files(ctx, files, shown);
        if first {
            self.went_to = self.list.files.get(self.list.cur).cloned();
        }
        self.reselect(ctx, &picked);
        if whole && std::mem::take(&mut self.sort_collection) {
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
        self.listed_as = None;
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

    pub(crate) fn title(&self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(self.title_text()));
    }

    /// Window → Set Window Title: this window's own template, until it closes; empty, the one
    /// in Preferences again.
    pub(crate) fn set_window_title(&mut self, ctx: &egui::Context, template: &str) {
        self.window_title = template.to_owned();
        self.title(ctx);
    }

    /// "image name - Glimt" (or "Glimt - image name", Preferences); the folder when it has no
    /// images. Or this window's own template, else the one in Preferences, unless its fields
    /// are all empty.
    pub(crate) fn title_text(&self) -> String {
        let (name, path) = match (self.list.files.get(self.list.cur), &self.collection) {
            (Some(_), _) => (
                self.name(self.list.cur).into_owned(),
                self.path(self.list.cur).display().to_string(),
            ),
            (None, Some(file)) => (file.display().to_string(), file.display().to_string()),
            (None, None) => (
                self.dir.display().to_string(),
                self.dir.display().to_string(),
            ),
        };
        // The shown image's own folder: a collection and several folders are listed as "/".
        let folder = match self.list.files.get(self.list.cur) {
            Some(_) => self.path(self.list.cur).parent().map(Path::to_path_buf),
            None => self
                .collection
                .as_ref()
                .and_then(|f| f.parent())
                .map(Path::to_path_buf),
        };
        let folder = folder
            .unwrap_or_else(|| self.dir.clone())
            .display()
            .to_string();
        // A named Glimt says which it is.
        let app = match remote::given_name() {
            Some(n) => tf!("Glimt ({n})", n),
            None => "Glimt".to_owned(),
        };
        let template = match self.window_title.as_str() {
            "" => &self.config.window_title,
            own => own,
        };
        let own = fill_template(template, |field| match field {
            "name" => name.clone(),
            "path" => path.clone(),
            "folder" => folder.clone(),
            "app" => app.clone(),
            _ => String::new(),
        });
        if !own.trim().is_empty() {
            own
        } else if self.config.app_name_first {
            format!("{app} - {name}")
        } else {
            format!("{name} - {app}")
        }
    }
}
