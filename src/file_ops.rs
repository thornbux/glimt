//! File operations: delete or move to the trash, copy and move, Rename and Rename Many, saving
//! the orientation, and the jobs that run them in the background with their progress and Stop.
//! Copies are written under a hidden name and get their own once whole; no file is overwritten.

use crate::*;

impl crate::App {
    /// To the trash at once, permanently after confirmation.
    pub(crate) fn request_delete(&mut self, ctx: &egui::Context, trash: bool) {
        if self.list.files.is_empty() {
            return;
        }
        if if trash {
            self.config.confirm_trash
        } else {
            self.config.confirm_delete
        } {
            self.ask_about_targets();
            self.confirm_delete = Some(trash);
        } else {
            self.delete_now(ctx, trash, self.targets());
        }
    }

    /// Where the list is: another folder or collection opened may hold files of the same names.
    fn list_place(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut place = std::collections::hash_map::DefaultHasher::new();
        (&self.dir, &self.folders, &self.collection).hash(&mut place);
        place.finish()
    }

    /// A dialog about the targets opens: their names are kept, as a folder another program writes
    /// to is read again every second and gives them other places in the list.
    pub(crate) fn ask_about_targets(&mut self) {
        let names = self
            .targets()
            .iter()
            .map(|&i| self.list.files[i].clone())
            .collect();
        self.asked_for = (self.list_place(), names);
    }

    /// The places now of the files the dialog asked about, those still in the list; `None`, with
    /// a notice, when none is, or another folder or collection is shown.
    pub(crate) fn asked_targets(&mut self) -> Option<Vec<usize>> {
        let (place, names) = std::mem::take(&mut self.asked_for);
        let names: HashSet<OsString> = names.into_iter().collect();
        let found: Vec<usize> = match place == self.list_place() {
            true => (self.list.files.iter().enumerate())
                .filter(|(_, f)| names.contains(*f))
                .map(|(i, _)| i)
                .collect(),
            false => Vec::new(),
        };
        if found.is_empty() {
            self.notice = Some(
                tr("The files asked about are no longer in the list: nothing was done").into(),
            );
            return None;
        }
        Some(found)
    }

    /// No file of `paths`, or in a folder of them, is being saved turned, and no keywords are
    /// being written; else a notice. Deleted, moved or renamed meanwhile, a PNG or WebP would
    /// come back when its save puts it under its name again, and a sidecar written file by file
    /// would miss the files moved before their turn.
    pub(crate) fn none_saving(&mut self, paths: &[PathBuf]) -> bool {
        let saving = (self.saving.iter()).any(|s| paths.iter().any(|p| s.starts_with(p)));
        let writing = self.file_jobs.iter().any(|j| j.sidecars);
        // One Rename Many at a time, and nothing else on files while it runs: a step would
        // find its file gone, and its undo could not put the others back.
        let renaming = self.renaming.is_some();
        if saving {
            self.notice = Some(tr("Still saving a rotation: try again in a moment").into());
        } else if writing {
            self.notice = Some(tr("Still writing keywords: try again in a moment").into());
        } else if renaming {
            self.notice = Some(tr("Rename Many is still renaming").into());
        }
        !saving && !writing && !renaming
    }

    /// Whether files are still being written (`config::WRITING`): file jobs, a turned image, a
    /// print, keywords, the settings. Closing waits for them.
    pub(crate) fn work_left(&self) -> bool {
        config::WRITING.load(Ordering::Relaxed) > 0
    }

    /// A file operation on `total` files, shown in the status bar until its thread drops it.
    pub(crate) fn start_job(&mut self, doing: &'static str, total: usize) -> Arc<Job> {
        self.start_job_of(doing, total, false)
    }

    /// `start_job`, of the files' `sidecars` or of the files themselves.
    fn start_job_of(&mut self, doing: &'static str, total: usize, sidecars: bool) -> Arc<Job> {
        let job = Arc::new(Job::new(doing, total, sidecars));
        self.file_jobs.push(job.clone());
        job
    }

    /// Delete the files at `targets` in the background, to the trash or for good,
    /// as a job with progress and Stop; the list follows when `receive_deleted` hears back.
    pub(crate) fn delete_now(&mut self, ctx: &egui::Context, trash: bool, targets: Vec<usize>) {
        let paths: Vec<PathBuf> = targets.iter().map(|&i| self.path(i)).collect();
        if !self.none_saving(&paths) {
            return;
        }
        self.deleting
            .extend(targets.iter().map(|&i| self.list.files[i].clone()));
        self.remove_indices(ctx, &targets);
        let (tx, ctx) = (self.notices_tx.clone(), ctx.clone());
        let (freed_tx, deleted_tx) = (self.freed_tx.clone(), self.deleted_tx.clone());
        let collection = self.collection.clone();
        let job = self.start_job(tr("Deleting"), paths.len());
        let pointer = Working::new(&self.busy);
        std::thread::spawn(move || {
            let since = std::time::SystemTime::now();
            // Read the size before deleting, so the status bar can subtract it.
            let _ = freed_tx.send(total_size(&paths));
            ctx.request_repaint();
            let mut errors = Vec::new();
            if trash {
                // Many files per gio call, so thousands of files do not become thousands of
                // processes; few enough that Stop and the count in the status bar keep up.
                for chunk in paths.chunks(100) {
                    if job.stopped() {
                        break;
                    }
                    match command("gio").arg("trash").arg("--").args(chunk).output() {
                        Ok(o) if o.status.success() => {}
                        Ok(o) => errors.push(String::from_utf8_lossy(&o.stderr).trim().to_owned()),
                        Err(e) => {
                            errors.push(tf!("gio trash (from glib): {e}", e));
                            break;
                        }
                    }
                    job.did(chunk.len());
                }
            } else {
                for path in &paths {
                    if job.stopped() {
                        break;
                    }
                    if let Err(e) = std::fs::remove_file(path) {
                        errors.push(format!("{display}: {e}", display = path.display()));
                    }
                    job.did(1);
                }
            }
            if let Some(first) = errors.first() {
                let _ = tx.send(tf!(
                    "Delete failed ({count}): {first}",
                    count = count(errors.len(), "error"),
                    first
                ));
            } else if let Some(text) = job.report(|| match trash {
                true => tf!(
                    "{count} moved to the trash",
                    count = count(paths.len(), "file")
                ),
                false => tf!("{count} deleted", count = count(paths.len(), "file")),
            }) {
                let _ = tx.send(text);
            }
            let gone: Vec<PathBuf> = paths.iter().filter(|p| !p.exists()).cloned().collect();
            let removed = forget_in_collection(collection.as_deref(), &gone).unwrap_or_else(|e| {
                let _ = tx.send(e);
                Vec::new()
            });
            // Their sidecars go the same way; a new file of the same name must not get their
            // keywords.
            let sides: Vec<PathBuf> = (gone.iter())
                .map(|p| meta::sidecar(p))
                .filter(|s| s.exists())
                .collect();
            for chunk in sides.chunks(500) {
                if trash {
                    let _ = command("gio").arg("trash").arg("--").args(chunk).output();
                } else {
                    chunk.iter().for_each(|s| drop(std::fs::remove_file(s)));
                }
            }
            let undo = (trash && !gone.is_empty()).then(|| {
                Undo::Trash(Thrown {
                    files: gone.clone(),
                    // Those that went: a sidecar gio could not move is still there.
                    sidecars: sides.into_iter().filter(|s| !s.exists()).collect(),
                    since: Some(since),
                    marks: Vec::new(),
                    collection: collection.zip(Some(removed)),
                })
            });
            // Those not reached after Stop are still there, as after a failure.
            let _ = deleted_tx.send(Deleted {
                paths,
                gone,
                failed: !errors.is_empty() || job.stopped(),
                undo,
                ..Default::default()
            });
            drop(job);
            drop(pointer);
            ctx.request_repaint();
        });
    }

    /// What deletes and moves report: files moved (their marks go along), and files gone, which
    /// leave the list, the marks and Find Duplicates; or the folder read again after a failure.
    pub(crate) fn receive_deleted(&mut self) {
        for Deleted {
            paths,
            gone,
            failed,
            moved,
            copied,
            relist,
            undo,
            marks,
        } in self.deleted_rx.try_iter().collect::<Vec<_>>()
        {
            // Moved before their sources are taken for deleted ones: the marks go along first.
            self.paths_moved(moved, copied);
            // Deleted: their marks go, so that a new file of the same name does not get them;
            // kept with the undo of a move to the trash, and put back by it.
            #[cfg(feature = "marks")]
            let undo = {
                let mut undo = undo;
                let forgotten = self.marks.forget(&gone);
                if let Some(Undo::Trash(thrown)) = &mut undo {
                    thrown.marks = forgotten;
                }
                self.marks.restore(marks);
                undo
            };
            #[cfg(not(feature = "marks"))]
            drop(marks);
            if undo.is_some() {
                self.undo = undo;
            }
            if let Some(dups) = &mut self.dups {
                // Those that failed are still there.
                dups.remove(&gone.iter().map(PathBuf::as_path).collect());
            }
            // By the name the list has, with its folders in the list with subfolders and in a
            // collection.
            // One of the folder shown that it did not hold back: it was left and opened again
            // meanwhile, and its listing may still have files that are gone now.
            let mut unknown = false;
            for p in &paths {
                let Ok(rel) = p.strip_prefix(&self.dir) else {
                    continue;
                };
                unknown |= !self.deleting.remove(rel.as_os_str());
            }
            self.relist |= unknown;
            // The files that are still there come back into the list, from its folder or
            // the collection's file. A listing that started before the files went still has
            // them: the folder is read again after it.
            self.relist |= failed || relist || (!paths.is_empty() && self.listing_rx.is_some());
        }
    }

    /// Files or folders moved or renamed `(from, to)`, or copied (`copied`): what Glimt keeps
    /// about them follows, the one place every operation reports to. Their marks go along (to a
    /// copy too); moved, a collection's entries of them, and of all in a moved folder, take the
    /// new place, in the list and in its file.
    pub(crate) fn paths_moved(&mut self, pairs: Vec<(PathBuf, PathBuf)>, copied: bool) {
        let moved = moves(&pairs);
        // A turn not saved yet goes with its file, saved under the new name.
        if !copied && let Some(to) = new_place(&self.turned, &moved) {
            self.turned = to;
        }
        if !copied && self.collection.is_some() {
            let mut any = false;
            for i in 0..self.list.files.len() {
                if let Some(to) = new_place(&self.path(i), &moved)
                    && let Ok(rel) = to.strip_prefix(&self.dir)
                {
                    self.list.files[i] = rel.as_os_str().to_owned();
                    any = true;
                }
            }
            // None of its images moved: the file is not touched, nor said to be gone.
            if any {
                self.edit_collection(|all| {
                    for p in all {
                        if let Some(to) = new_place(p, &moved) {
                            *p = to;
                        }
                    }
                });
            }
        }
        #[cfg(feature = "marks")]
        self.marks.put(pairs, copied);
    }

    /// Copy or move `paths` into `dest` in the background. Existing files are never
    /// overwritten; a move across file systems falls back to copy + delete.
    pub(crate) fn transfer(
        &mut self,
        ctx: &egui::Context,
        paths: Vec<PathBuf>,
        dest: PathBuf,
        keep: bool,
    ) {
        if !keep && !self.none_saving(&paths) {
            return;
        }
        // In a collection, the entries follow the files that got there, in `receive_deleted`.
        if !keep && self.collection.is_none() {
            // Moved out of its folder: out of the list at once, as when deleting. Names in the
            // list are relative to the folder, also those in subfolders.
            let names: HashSet<&std::ffi::OsStr> = paths
                .iter()
                .filter(|p| p.parent() != Some(dest.as_path()))
                .filter_map(|p| p.strip_prefix(&self.dir).ok())
                .map(Path::as_os_str)
                .collect();
            // Kept out of a listing that is running, as files being deleted are.
            self.deleting.extend(names.iter().map(|n| n.to_os_string()));
            let gone: Vec<usize> = (0..self.list.files.len())
                .filter(|&i| names.contains(self.list.files[i].as_os_str()))
                .collect();
            self.remove_indices(ctx, &gone);
        }
        let (tx, freed_tx, ctx) = (self.notices_tx.clone(), self.freed_tx.clone(), ctx.clone());
        let deleted_tx = self.deleted_tx.clone();
        let same_dir = paths.first().and_then(|p| p.parent()) == Some(self.dir.as_path());
        // Into the folder, or one below it in the list with subfolders: the files join the list.
        let listed = dest == self.dir || (self.recursive && dest.starts_with(&self.dir));
        let job = self.start_job(tr(if keep { "Copying" } else { "Moving" }), paths.len());
        let pointer = Working::new(&self.busy);
        std::thread::spawn(move || {
            if !keep && same_dir {
                let _ = freed_tx.send(total_size(&paths));
            }
            let (mut errors, mut put) = (Vec::new(), Vec::new());
            for path in &paths {
                if job.stopped() {
                    break;
                }
                let target = dest.join(path.file_name().unwrap_or_default());
                let mut failed = |at: &Path, e: std::io::Error| {
                    errors.push(format!("{display}: {e}", display = at.display()));
                };
                // The file, then its sidecar, which is told by its own name when it fails: the
                // photo went, its keywords did not.
                if let Some(at) = taken(&target, &HashSet::new()) {
                    failed(&at, std::io::ErrorKind::AlreadyExists.into());
                } else if let Err(e) = put_file(path, &target, keep) {
                    failed(&target, e);
                } else {
                    if let Err(e) = meta::follow(path, &target, keep) {
                        failed(&meta::sidecar(&target), e);
                    }
                    put.push((path.clone(), target));
                }
                job.did(1);
            }
            let gone: Vec<PathBuf> = match keep {
                true => Vec::new(),
                false => put.iter().map(|(from, _)| from.clone()).collect(),
            };
            if let Some(first) = errors.first() {
                let verb = tr(if keep { "Copy" } else { "Move" });
                let _ = tx.send(tf!(
                    "{verb} failed ({count}): {first}",
                    verb,
                    count = count(errors.len(), "error"),
                    first
                ));
            } else if let Some(text) = job.report(|| {
                let (files, folder) = (count(paths.len(), "file"), dest.display());
                match keep {
                    true => tf!("{files} copied to {folder}", files, folder),
                    false => tf!("{files} moved to {folder}", files, folder),
                }
            }) {
                let _ = tx.send(text);
            }
            // Moved: out of `deleting`; read the folder again for files that joined it, or that
            // failed to go or were not reached after Stop. A copy elsewhere changes the list not.
            let relist = (listed || !keep) && (listed || !errors.is_empty() || job.stopped());
            let undo = (!keep && !put.is_empty()).then(|| Undo::Move(put.clone()));
            let _ = deleted_tx.send(Deleted {
                paths: if keep { Vec::new() } else { paths },
                gone,
                failed: !errors.is_empty() || job.stopped(),
                moved: put,
                copied: keep,
                relist,
                undo,
                ..Default::default()
            });
            drop(job);
            drop(pointer);
            ctx.request_repaint();
        });
    }

    /// Rename the selected files by `pattern`, numbered in list order. Nothing is overwritten:
    /// if a new name is taken by a file outside the selection, nothing is renamed. When new
    /// names swap with old ones, the files go through temporary names.
    pub(crate) fn rename_many(&mut self, ctx: &egui::Context, pattern: &str) {
        let targets = self.targets();
        let from: Vec<PathBuf> = targets.iter().map(|&i| self.path(i)).collect();
        if !self.none_saving(&from) {
            return;
        }
        let to: Vec<PathBuf> = (from.iter().enumerate())
            .map(|(k, p)| p.with_file_name(pattern_name(pattern, p, k + 1)))
            .collect();
        let unique: HashSet<&PathBuf> = to.iter().collect();
        if unique.len() < to.len() {
            self.notice = Some(
                tr("The pattern gives several files the same name; add # for a number").into(),
            );
            return;
        }
        self.rename_in_background(ctx, from, to);
    }

    /// Rename `from` to `to`, one file or many, in the background: thousands, or a network
    /// share that stalls, would hold the window. `renamed` takes the result.
    pub(crate) fn rename_in_background(
        &mut self,
        ctx: &egui::Context,
        from: Vec<PathBuf>,
        to: Vec<PathBuf>,
    ) {
        let shown = from.iter().position(|f| *f == self.path(self.list.cur));
        let job = RenameJob {
            files_gen: self.list.version,
            from: from.clone(),
            to: to.clone(),
            shown,
            undoing: false,
        };
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        let pointer = Working::new(&self.busy);
        // A job as every other writer: closing waits for it, the status bar counts it and
        // its Stop puts the names done so far back.
        let steps = self.start_job(tr("Renaming"), from.len());
        std::thread::spawn(move || {
            let _ = tx.send(rename_files(from, to, &steps));
            drop(steps);
            drop(pointer);
            ctx.request_repaint();
        });
        self.renaming = Some(job);
        self.renaming_rx = Receiving::of(rx);
    }

    /// Rename Many has run: the list follows, the shown image under its new name.
    pub(crate) fn renamed(
        &mut self,
        ctx: &egui::Context,
        job: RenameJob,
        result: Result<(), String>,
    ) {
        let failed = result.is_err();
        if let Err(e) = result {
            self.notice = Some(e);
        }
        self.list.selected.clear();
        self.selection_changed(ctx);
        // Found by their old names: the list may have changed while they were renamed. Nothing
        // renamed: the names stay.
        // Undone, it is not undone again: Ctrl+Z twice does not give the new names back.
        if !failed && !job.undoing {
            self.undo = Some(Undo::Rename(job.from.clone(), job.to.clone()));
        }
        if !failed {
            self.paths_moved(
                job.from
                    .iter()
                    .cloned()
                    .zip(job.to.iter().cloned())
                    .collect(),
                false,
            );
        }
        if self.collection.is_some() {
            return self.list(ctx);
        }
        // The list changed meanwhile: its numbers are other files' now, so it is read again.
        if job.files_gen != self.list.version {
            return self.list(ctx);
        }
        // Still the image shown when it began (the user may have moved on meanwhile): it stays
        // shown under its new name, with a turn not saved yet, as the list is read again. Read
        // again, not opened: the view stays in the folder, subfolders or folders it shows.
        if let Some(k) = job.shown
            && !failed
            && self.path(self.list.cur) == job.from[k]
            && let Ok(rel) = job.to[k].strip_prefix(&self.dir)
        {
            self.list.files[self.list.cur] = rel.as_os_str().to_owned();
        }
        self.list(ctx)
    }

    /// Write the view's rotation into the shown file, in the background, and show it as it is
    /// then. A JPEG keeps its pixels and gets an orientation field; PNG and WebP are turned,
    /// which takes seconds for a large one. Both read and write the whole file, slow on a share.
    pub(crate) fn save_orientation(&mut self, ctx: &egui::Context) {
        let (turns, mirrored) = self.orient;
        if self.list.files.is_empty() || (turns, mirrored) == (0, false) {
            return;
        }
        let path = self.turned.clone();
        // Removed or renamed by another program meanwhile: nothing to save the turn into.
        if !path.is_file() {
            return;
        }
        // Being saved already, with this rotation: the rotate keys wait for the save.
        if self.saving.contains(&path) {
            return;
        }
        let jpeg = crate::files::is_jpeg(&path);
        let (rotate, quality) = (self.config.exif_rotate, self.config.webp_quality);
        let (saved, notices) = (self.saved_tx.clone(), self.notices_tx.clone());
        let ctx = ctx.clone();
        self.saving.insert(path.clone());
        let pointer = Working::new(&self.busy);
        let writing = Working::new(&config::WRITING);
        std::thread::spawn(move || {
            let _writing = writing;
            let result = survive(|| match jpeg {
                true => exif::write_orientation(&path, turns, mirrored, rotate),
                false => save_rotated(&path, rotate, turns, mirrored, quality),
            });
            if jpeg && result.is_ok() && !rotate {
                let _ =
                    notices.send(tr("Saved; it shows when Auto Rotate Using EXIF is on").into());
            }
            let _ = saved.send((path, result));
            drop(pointer);
            ctx.request_repaint();
        });
    }

    /// `path` was saved turned: decode it again and make a new thumbnail.
    pub(crate) fn saved(&mut self, path: &Path) {
        if self.is_file(self.list.cur, path) {
            self.orient = (0, false);
            self.list
                .decodes_pending
                .retain(|p, _| p.0 != self.list.cur);
            self.exif = None;
        }
        self.list.slots.retain(|_, s| s.path != path);
        // Its size changed: the status bar and the list read it again.
        self.cur_size = None;
        if let Ok(name) = path.strip_prefix(&self.dir) {
            self.thumbs.remove(name.as_os_str());
            self.thumbs_pending.remove(name.as_os_str());
            self.list.details.remove(name.as_os_str());
        }
        self.refill();
    }

    /// Rename `folder_target` within its parent; never over another. The open folder, or one
    /// inside it, is followed to the new name.
    pub(crate) fn rename_folder(&mut self, ctx: &egui::Context, name: &str) {
        let Some(old) = self.folder_target.take() else {
            return;
        };
        let new = old.with_file_name(name);
        if new == old || !self.none_saving(std::slice::from_ref(&old)) {
            return;
        }
        // Never over an empty folder of that name, also one made since the look.
        if let Err(e) = rename_never_over(&old, &new) {
            self.notice = Some(format!("{display}: {e}", display = new.display()));
            return;
        }
        self.forget_folder(&old);
        // Several folders shown together: the renamed one is read under its new name.
        for folder in &mut self.folders {
            if let Ok(rest) = folder.strip_prefix(&old) {
                *folder = new.join(rest);
            }
        }
        // A collection: its own file follows the folder it is in, and its images in the folder
        // follow it to the new name, in its file too.
        if let Some(file) = &mut self.collection
            && let Ok(rest) = file.strip_prefix(&old)
        {
            *file = new.join(rest);
        }
        self.paths_moved(vec![(old.clone(), new.clone())], false);
        match self.dir.strip_prefix(&old) {
            Ok(rest) => {
                // The shown file is under the new name now, also for a turn not saved yet.
                self.dir = new.join(rest);
                self.visit(ctx, self.dir.clone())
            }
            Err(_) => self.list(ctx),
        }
    }

    /// Move `folder_target` and all in it to the trash, in the background; the open folder, or
    /// one inside it, is left for the folder above first.
    pub(crate) fn trash_folder(&mut self, ctx: &egui::Context) {
        let Some(dir) = self.folder_target.take() else {
            return;
        };
        if !self.none_saving(std::slice::from_ref(&dir)) {
            return;
        }
        self.forget_folder(&dir);
        self.subdirs.retain(|d| *d != dir);
        if self.dir.starts_with(&dir)
            && let Some(parent) = dir.parent()
        {
            self.visit(ctx, parent.to_path_buf());
        }
        let deleted_tx = self.deleted_tx.clone();
        let collection = self.collection.clone();
        self.background(ctx, true, move || {
            let since = std::time::SystemTime::now();
            let error = match command("gio").arg("trash").arg("--").arg(&dir).output() {
                Ok(o) if o.status.success() => None,
                Ok(o) => Some(String::from_utf8_lossy(&o.stderr).trim().to_owned()),
                Err(e) => Some(tf!("gio trash (from glib): {e}", e)),
            };
            // Failed: listed again, as a failed delete of files is.
            let gone = if error.is_none() {
                vec![dir.clone()]
            } else {
                Vec::new()
            };
            let (removed, forgotten) = match forget_in_collection(collection.as_deref(), &gone) {
                Ok(removed) => (removed, None),
                Err(e) => (Vec::new(), Some(e)),
            };
            let undo = (!gone.is_empty()).then(|| {
                Undo::Trash(Thrown {
                    files: gone.clone(),
                    since: Some(since),
                    collection: collection.zip(Some(removed)),
                    ..Default::default()
                })
            });
            let _ = deleted_tx.send(Deleted {
                paths: vec![dir],
                gone,
                failed: error.is_some(),
                undo,
                ..Default::default()
            });
            error
                .map(|e| tf!("Could not move the folder to the trash: {e}", e))
                .or(forgotten)
        });
    }

    /// Edit → Undo (Ctrl+Z): the last files or folder moved to the trash come back from it, the
    /// last files moved go back, the last renamed get their old names. Once: what it put back
    /// is not undone again. Nothing is put over a file that has taken the name meanwhile.
    pub(crate) fn undo(&mut self, ctx: &egui::Context) {
        let Some(undo) = self.undo.take() else {
            self.notice = Some(tr("Nothing to undo").into());
            return;
        };
        let deleted_tx = self.deleted_tx.clone();
        match undo {
            Undo::Rename(from, to) => {
                if self.none_saving(&to) {
                    self.rename_in_background(ctx, to, from);
                    if let Some(job) = &mut self.renaming {
                        job.undoing = true;
                    }
                }
            }
            Undo::Move(pairs) => self.background(ctx, true, move || {
                let (mut errors, mut back) = (Vec::new(), Vec::new());
                for (from, to) in pairs {
                    let result = match taken(&from, &HashSet::new()) {
                        Some(at) => Err((at, std::io::ErrorKind::AlreadyExists.into())),
                        None => put_file(&to, &from, false).map_err(|e| (from.clone(), e)),
                    };
                    match result {
                        Ok(()) => {
                            if let Err(e) = meta::follow(&to, &from, false) {
                                errors.push(format!("{}: {e}", meta::sidecar(&from).display()));
                            }
                            back.push((to, from));
                        }
                        Err((at, e)) => errors.push(format!("{}: {e}", at.display())),
                    }
                }
                let files = count(back.len(), "file");
                let _ = deleted_tx.send(Deleted {
                    moved: back,
                    relist: true,
                    ..Default::default()
                });
                Some(match errors.first() {
                    Some(first) => tf!(
                        "Undo failed ({count}): {first}",
                        count = count(errors.len(), "error"),
                        first
                    ),
                    None => tf!("{files} moved back", files),
                })
            }),
            Undo::Trash(thrown) => self.background(ctx, true, move || {
                let (back, mut error) = restore(&thrown, trash_dirs);
                // What went with them comes back with those back: their places in the
                // collection, their marks and stars.
                let set: HashSet<&Path> = back.iter().map(PathBuf::as_path).collect();
                let returned = |p: &Path| p.ancestors().any(|a| set.contains(a));
                if let Some((file, removed)) = &thrown.collection {
                    let removed: Vec<_> = (removed.iter())
                        .filter(|(_, p)| returned(p))
                        .cloned()
                        .collect();
                    let edit = collections::edit(file, false, |all| put_back(all, &removed));
                    if let Err(e) = edit {
                        error.get_or_insert(tf!("Could not save the collection: {e}", e));
                    }
                }
                let marks = (thrown.marks.into_iter())
                    .filter(|(p, _)| returned(p))
                    .collect();
                let _ = deleted_tx.send(Deleted {
                    relist: true,
                    marks,
                    ..Default::default()
                });
                Some(match error {
                    None => tf!(
                        "{count} back from the trash",
                        count = count(back.len(), "file")
                    ),
                    Some(e) => tf!("Undo failed: {e}", e),
                })
            }),
        }
        // A folder that came back shows in the tree again.
        self.tree.clear();
    }

    /// Forget what the folder tree read of `dir` and its parent, so it is read again.
    pub(crate) fn forget_folder(&mut self, dir: &Path) {
        self.tree.retain(|d, _| !d.starts_with(dir));
        if let Some(parent) = dir.parent() {
            self.tree.remove(parent);
        }
    }
}

/// Rename `from` to `to`, never over a file or folder there, also one another window put there
/// a moment ago: in one step where the file system can (RENAME_NOREPLACE); where it cannot (FAT,
/// NTFS or exFAT through FUSE, some shares), a link, which never replaces a file, and then the
/// old name unlinked; where links cannot be made either, a rename after looking first. The one
/// way Glimt renames: a file moved, a copy or a new file given its name, a folder, a collection,
/// a file back from the trash. `CrossesDevices` is the caller's to copy instead.
pub(crate) fn rename_never_over(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::io::ErrorKind::{AlreadyExists, CrossesDevices, Unsupported};
    match rename_noreplace(from, to) {
        Err(e) if e.kind() == Unsupported => match std::fs::hard_link(from, to) {
            Ok(()) => unlink_linked(from, to),
            Err(e) if matches!(e.kind(), AlreadyExists | CrossesDevices) => Err(e),
            Err(_) if to.exists() => Err(AlreadyExists.into()),
            Err(_) => std::fs::rename(from, to),
        },
        done => done,
    }
}

/// `from`, now also at `to`, unlinked: a move is all or nothing, so one that cannot be removed
/// (a shared folder with the sticky bit, someone else's file) has its new name taken away
/// again. One gone after all (deleted or moved in another window, or a share that said no but
/// did it) is done: the new name may be the only one left.
fn unlink_linked(from: &Path, to: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(from) {
        Err(e) if std::fs::symlink_metadata(from).is_ok() => {
            let _ = std::fs::remove_file(to);
            Err(e)
        }
        _ => Ok(()),
    }
}

/// Give `from` the name `to` in one step, never over a file there: renameat2 with
/// RENAME_NOREPLACE (Linux 3.15; glibc 2.28, the release's baseline). `Unsupported` where the
/// file system has no such flag (FAT, some shares, old kernels). Only for `rename_never_over`,
/// which falls back where it is not to be had.
fn rename_noreplace(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    unsafe extern "C" {
        fn renameat2(
            olddirfd: std::ffi::c_int,
            oldpath: *const std::ffi::c_char,
            newdirfd: std::ffi::c_int,
            newpath: *const std::ffi::c_char,
            flags: std::ffi::c_uint,
        ) -> std::ffi::c_int;
    }
    const AT_FDCWD: std::ffi::c_int = -100;
    const RENAME_NOREPLACE: std::ffi::c_uint = 1;
    let (from, to) = (
        std::ffi::CString::new(from.as_os_str().as_bytes())?,
        std::ffi::CString::new(to.as_os_str().as_bytes())?,
    );
    // SAFETY: two valid C strings for the call's duration; renameat2 reads them and touches
    // nothing else of ours.
    let r = unsafe {
        renameat2(
            AT_FDCWD,
            from.as_ptr(),
            AT_FDCWD,
            to.as_ptr(),
            RENAME_NOREPLACE,
        )
    };
    if r == 0 {
        return Ok(());
    }
    let e = std::io::Error::last_os_error();
    // EINVAL and ENOSYS: the flag or the call is not there; ENOTSUP: not on this file system.
    match e.raw_os_error() {
        Some(22 | 38 | 95) => Err(std::io::ErrorKind::Unsupported.into()),
        _ => Err(e),
    }
}

/// Copy (`keep`) or move `from` to `to`, never over a file there, also one another window put
/// there a moment ago: a copy is written under a hidden name and then renamed, a move is a
/// rename, both with `rename_never_over`; a move to another file system is a copy and an unlink.
pub(crate) fn put_file(from: &Path, to: &Path, keep: bool) -> std::io::Result<()> {
    use std::io::ErrorKind::{AlreadyExists, CrossesDevices};
    use std::os::unix::fs::OpenOptionsExt;
    let copy = || -> std::io::Result<()> {
        if to.exists() {
            return Err(AlreadyExists.into());
        }
        // Written under a hidden name and given its own when whole: a copy cut short (a full
        // disk, Glimt closed, a stick pulled out) never looks like the file.
        static COPIES: AtomicUsize = AtomicUsize::new(0);
        let k = COPIES.fetch_add(1, Ordering::Relaxed);
        // shortcut: a third close (which waits for nothing) or a stick pulled out leaves the
        // part under this hidden name, and nothing removes it later; a sweep of
        // .glimt-copy-* at the next start if those are ever seen in numbers.
        let part = to.with_file_name(format!(".glimt-copy-{}-{k}", remote::instance_id()));
        // Private while written: a private photo copied into a shared folder is not readable
        // there until it gets its own permissions below.
        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&part)?;
        // Its time kept, as mv does (sorting by date), and on disk before a move removes the
        // source: a stick pulled out right after must not lose both.
        let copied = std::fs::File::open(from)
            .and_then(|mut f| std::io::copy(&mut f, &mut out))
            .and_then(|_| {
                let meta = std::fs::metadata(from)?;
                std::fs::set_permissions(&part, meta.permissions())?;
                out.set_modified(meta.modified()?)?;
                out.sync_all()
            })
            .and_then(|_| rename_never_over(&part, to));
        let _ = std::fs::remove_file(&part);
        copied
    };
    if keep {
        return copy();
    }
    match rename_never_over(from, to) {
        Err(e) if e.kind() == CrossesDevices => copy().and_then(|_| unlink_linked(from, to)),
        done => done,
    }
}

/// What `thrown` moved to the trash put back where it was, with the sidecars that went with
/// it: of each, the newest of its name in the trash folders `dirs_of` gives (the home trash, and
/// those at the top of its file system), and only one put there since the move began, as an
/// older one of the same name is another file's. Each trash folder is read once, into a map by
/// name. Read from the trash's own folders (files/ and info/*.trashinfo, as freedesktop.org has
/// it): `gio trash --restore` needs gvfs, which KDE and containers often lack. Never over a file
/// that has taken the name since. The files put back, and the first error.
fn restore(
    thrown: &Thrown,
    dirs_of: impl Fn(&Path) -> Vec<(PathBuf, PathBuf)>,
) -> (Vec<PathBuf>, Option<String>) {
    let mut trashes: HashMap<PathBuf, HashMap<PathBuf, Trashed>> = HashMap::new();
    let mut find = |path: &Path| {
        let mut latest: Option<Trashed> = None;
        for (dir, top) in dirs_of(path) {
            let all =
                (trashes.entry(dir.clone())).or_insert_with(|| newest(read_trash(&dir, &top)));
            if let Some(t) = all.get(path)
                && latest.as_ref().is_none_or(|l| t.date > l.date)
            {
                latest = Some(t.clone());
            }
        }
        // Its .trashinfo is written as the file goes; FAT keeps times to two seconds.
        let ours = |t: &Trashed| {
            let written = std::fs::metadata(&t.info).and_then(|m| m.modified());
            let since = thrown.since.unwrap_or(std::time::UNIX_EPOCH);
            written.is_ok_and(|w| w + std::time::Duration::from_secs(2) >= since)
        };
        latest.filter(ours)
    };
    let mut found = Vec::new();
    for path in &thrown.files {
        if path.exists() {
            return (
                Vec::new(),
                Some(tf!("{name} exists already", name = path.display())),
            );
        }
        match find(path) {
            Some(t) => found.push(t),
            None => {
                let e = tf!("{name} is not in the trash", name = path.display());
                return (Vec::new(), Some(e));
            }
        }
    }
    // A sidecar no longer there (the trash emptied of it) does not hold its file back.
    found.extend(thrown.sidecars.iter().filter_map(|s| find(s)));
    let mut back = Vec::new();
    for t in found {
        if let Err(e) = rename_never_over(&t.file, &t.from) {
            return (
                back,
                Some(format!("{display}: {e}", display = t.from.display())),
            );
        }
        let _ = std::fs::remove_file(&t.info);
        back.push(t.from);
    }
    (back, None)
}

/// Of each name in a trash folder, the one put there last.
fn newest(all: Vec<Trashed>) -> HashMap<PathBuf, Trashed> {
    let mut by_name: HashMap<PathBuf, Trashed> = HashMap::new();
    for t in all {
        match by_name.get(&t.from) {
            Some(had) if had.date >= t.date => {}
            _ => drop(by_name.insert(t.from.clone(), t)),
        }
    }
    by_name
}

/// Entries `removed` from a collection, each with its place then, put back at those places in
/// `all`, which is what is left: in one pass, also for thousands. One there again is not added
/// twice.
fn put_back(all: &mut Vec<PathBuf>, removed: &[(usize, PathBuf)]) {
    let have: HashSet<PathBuf> = all.iter().cloned().collect();
    let mut left = std::mem::take(all).into_iter();
    for (at, path) in removed.iter().filter(|(_, p)| !have.contains(p)) {
        while all.len() < *at
            && let Some(next) = left.next()
        {
            all.push(next);
        }
        all.push(path.clone());
    }
    all.extend(left);
}

/// What a move to the trash did, as it did it, for Undo to put back: the files or folder it
/// moved, the sidecars that went with them, when it began (the trash may hold older files of the
/// same names, which are not these), and what went with them elsewhere: their marks and stars,
/// and their places in the collection shown. Recorded as it happens, never found out later.
#[derive(Default)]
pub(crate) struct Thrown {
    files: Vec<PathBuf>,
    sidecars: Vec<PathBuf>,
    since: Option<std::time::SystemTime>,
    pub(crate) marks: Vec<(PathBuf, (u8, u8))>,
    collection: Option<(PathBuf, Vec<(usize, PathBuf)>)>,
}

/// A file in a trash folder: where it came from, when (as the trash writes it, which sorts by
/// time), and its file and its .trashinfo there.
#[derive(Clone)]
struct Trashed {
    from: PathBuf,
    date: String,
    file: PathBuf,
    info: PathBuf,
}

/// The trash folders a file at `path` may have gone to, each with the folder its relative paths
/// start from: the home trash, and the two at the top of the file system `path` is on.
fn trash_dirs(path: &Path) -> Vec<(PathBuf, PathBuf)> {
    use std::os::unix::fs::MetadataExt;
    // In a Flatpak, gio trash runs on the host, into the host's trash.
    let home = match std::env::var_os("FLATPAK_ID") {
        Some(_) => std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share/Trash")),
        None => config::xdg_dir("XDG_DATA_HOME", ".local/share", "Trash"),
    };
    let mut dirs: Vec<(PathBuf, PathBuf)> = home.map(|h| (h, "/".into())).into_iter().collect();
    // The top of its file system: the last folder up on the same device.
    let dev = |p: &Path| std::fs::metadata(p).map(|m| m.dev()).ok();
    let uid = std::fs::metadata("/proc/self").map(|m| m.uid());
    let (Some(mut top), Ok(uid)) = (path.parent(), uid) else {
        return dirs;
    };
    let Some(here) = dev(top) else {
        return dirs;
    };
    while let Some(up) = top.parent().filter(|up| dev(up) == Some(here)) {
        top = up;
    }
    dirs.push((top.join(".Trash").join(uid.to_string()), top.to_path_buf()));
    dirs.push((top.join(format!(".Trash-{uid}")), top.to_path_buf()));
    dirs
}

/// What the trash folder `dir` holds, its relative paths read from `top`.
fn read_trash(dir: &Path, top: &Path) -> Vec<Trashed> {
    use std::os::unix::ffi::OsStringExt;
    let entries = std::fs::read_dir(dir.join("info"))
        .into_iter()
        .flatten()
        .flatten();
    (entries)
        .filter_map(|e| {
            let info = e.path();
            let name = info
                .file_name()?
                .to_str()?
                .strip_suffix(".trashinfo")?
                .to_owned();
            let text = crate::decode::read_text(&info).ok()?;
            let value = |key: &str| text.lines().find_map(|l| l.strip_prefix(key));
            let from = collections::percent_decoded(value("Path=")?.as_bytes());
            Some(Trashed {
                from: top.join(OsString::from_vec(from)),
                date: value("DeletionDate=").unwrap_or_default().to_owned(),
                file: dir.join("files").join(name),
                info,
            })
        })
        .collect()
}

/// Files or folders `gone` for good leave the `collection` shown: only once they are gone, in
/// the thread that knows, so the file keeps its order and never lacks a file still there. The
/// one place for Delete and Move Folder to Trash. A plain list is the user's own, never written.
/// What it took out, each with its place, for Undo to put back.
fn forget_in_collection(
    collection: Option<&Path>,
    gone: &[PathBuf],
) -> Result<Vec<(usize, PathBuf)>, String> {
    let Some(file) = collection.filter(|_| !gone.is_empty()) else {
        return Ok(Vec::new());
    };
    if collections::is_list(file) {
        return Ok(Vec::new());
    }
    // Each entry and the folders above it looked up, not every file gone for each entry: 10,000
    // deleted from a collection of 50,000 were half a billion comparisons.
    let gone: HashSet<&Path> = gone.iter().map(PathBuf::as_path).collect();
    let mut removed = Vec::new();
    let edit = collections::edit(file, false, |all| {
        let mut at = 0;
        all.retain(|p| {
            let out = p.ancestors().any(|a| gone.contains(a));
            if out {
                removed.push((at, p.clone()));
            }
            at += 1;
            !out
        });
    });
    match edit {
        Ok(()) => Ok(removed),
        // Renamed or moved to the trash in another window since it was opened: not made again
        // under its old name.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(tf!(
            "The collection {file} is gone (renamed or removed in another window): not saved",
            file = file.display()
        )),
        Err(e) => Err(tf!("Could not save the collection: {e}", e)),
    }
}

/// The name in the way of a file going to `to`, if any: `to` itself, or a sidecar at the new
/// name that is another file's, whether or not the file has one of its own (the photo would
/// show that file's keywords and stars, and leave its own behind). `ours` are the operation's
/// own names, which a swap reuses. The one rule for Copy, Move, Rename and Rename Many.
pub(crate) fn taken(to: &Path, ours: &HashSet<&PathBuf>) -> Option<PathBuf> {
    [to.to_path_buf(), meta::sidecar(to)]
        .into_iter()
        .find(|p| p.exists() && !ours.contains(p))
}

/// Rename `from` to `to` with their sidecars, all or none: refused when a new name is another
/// file's, through temporary names when names swap, with the plan on disk while it runs, and
/// undone on a failure. Never replaces a file, also one another program makes meanwhile.
pub(crate) fn rename_files(from: Vec<PathBuf>, to: Vec<PathBuf>, job: &Job) -> Result<(), String> {
    // Sidecars go with their files, through the same steps.
    let (mut from, mut to) = (from, to);
    let sides: Vec<(PathBuf, PathBuf)> = (from.iter().zip(&to))
        .map(|(f, t)| (meta::sidecar(f), meta::sidecar(t)))
        .filter(|(f, _)| f.exists())
        .collect();
    for (f, t) in sides {
        from.push(f);
        to.push(t);
    }
    let sources: HashSet<&PathBuf> = from.iter().collect();
    if let Some(at) = to.iter().find_map(|t| taken(t, &sources)) {
        return Err(tf!(
            "{name} exists already; nothing renamed",
            name = at.display()
        ));
    }
    let swap = to.iter().any(|t| sources.contains(t));
    // Next to the file: in a collection the files are in different folders.
    let tmp = |f: &Path, k: usize| {
        // This Glimt's name: Flatpak windows can share a process ID.
        f.with_file_name(format!(".glimt-rename-{}-{k}", remote::instance_id()))
    };
    let mut moves: Vec<(PathBuf, PathBuf)> = Vec::new();
    if swap {
        moves.extend(from.iter().enumerate().map(|(k, f)| (f.clone(), tmp(f, k))));
        moves.extend(to.iter().enumerate().map(|(k, t)| (tmp(t, k), t.clone())));
    } else {
        moves.extend(from.iter().cloned().zip(to.iter().cloned()));
    }
    // On disk while the renames run: a crash halfway is put right at the next start.
    let mut plan = journal::dir().and_then(|d| journal::begin(&d, &moves));
    for (k, (a, b)) in moves.iter().enumerate() {
        // Stop: the names done so far go back, by the same steps a failure takes.
        let step = match job.stopped() {
            true => Err(std::io::Error::other(tr("stopped"))),
            false => journal::step(a, b),
        };
        if let Err(e) = step {
            // Named by the file and its new name, not a temporary one.
            let j = k % from.len();
            let name = |p: &PathBuf| p.file_name().unwrap_or_default().display().to_string();
            // Undone, so that no file is left under a hidden temporary name: the same steps
            // the next start would take.
            let (_, stuck) = journal::undo(&moves, k);
            let (f, t) = (name(&from[j]), name(&to[j]));
            // Not all put back (a share that stopped answering): the plan stays, and the next
            // start offers to put the files right.
            if stuck > 0 {
                // With a plan kept, the next start offers the repair; without one (no state
                // folder, a full disk) the files are told where they are, under hidden names.
                return Err(match plan.take() {
                    Some(plan) => {
                        plan.keep();
                        tf!(
                            "Could not rename {f} to {t}: {e}; not all names could be put back, Glimt offers to put them right at the next start",
                            f,
                            t,
                            e
                        )
                    }
                    None => tf!(
                        "Could not rename {f} to {t}: {e}; not all names could be put back, and no plan could be kept: the files under .glimt-rename-* names next to them are theirs",
                        f,
                        t,
                        e
                    ),
                });
            }
            return Err(tf!(
                "Could not rename {f} to {t}: {e}; nothing renamed",
                f,
                t,
                e
            ));
        }
        if let Some(plan) = plan.as_mut() {
            plan.done();
        }
        // Counted as a file lands under its name: the later half of a swap's steps, the
        // sidecars not.
        let landed = if swap {
            k.checked_sub(moves.len() / 2)
        } else {
            Some(k)
        };
        if landed.is_some_and(|n| n < job.total) {
            job.did(1);
        }
    }
    Ok(())
}

// ---- Work in the background: sidecars, frames, the backup and other jobs ----

/// The thread for this window's sidecar writes: keywords, comments, stars and imports land in
/// the order they were made.
fn sidecar_writer() -> &'static mpsc::Sender<config::Work> {
    static WRITER: std::sync::OnceLock<mpsc::Sender<config::Work>> = std::sync::OnceLock::new();
    WRITER.get_or_init(config::serial)
}

impl crate::App {
    /// `work` in a thread of its own, the busy pointer showing meanwhile; what it returns is
    /// told in the window. Work that `writes` files (the trash, renames, collections) is waited
    /// for at closing.
    pub(crate) fn background(
        &self,
        ctx: &egui::Context,
        writes: bool,
        work: impl FnOnce() -> Option<String> + Send + 'static,
    ) {
        let (tx, ctx) = (self.notices_tx.clone(), ctx.clone());
        let pointer = Working::new(&self.busy);
        let writing = writes.then(|| Working::new(&config::WRITING));
        std::thread::spawn(move || {
            let _writing = writing;
            if let Some(text) = work() {
                let _ = tx.send(text);
            }
            drop(pointer);
            ctx.request_repaint();
        });
    }

    /// Write the sidecars of `paths` in the background, through one thread for this window's
    /// keywords, stars and Lightroom import (`sidecar_writer`), so changes to one file land in the
    /// order they were made and never cross. The status bar counts them; `report` words the end from how many
    /// failed. A thread of its own, not `config::write_later`'s: settings saved meanwhile must
    /// not wait behind 3,000 sidecars.
    pub(crate) fn write_sidecars(
        &mut self,
        ctx: &egui::Context,
        doing: &'static str,
        paths: Vec<PathBuf>,
        write: impl Fn(&Path) -> bool + Send + 'static,
        report: impl FnOnce(usize, &Job) -> Option<String> + Send + 'static,
    ) {
        use config::Work;
        let job = self.start_job_of(doing, paths.len(), true);
        let (tx, ctx) = (self.notices_tx.clone(), ctx.clone());
        let pointer = Working::new(&self.busy);
        let work: Work = Box::new(move || {
            // A file that makes the writing panic ends this job, not the count or the busy
            // pointer.
            let text = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut failed = 0;
                for path in &paths {
                    if job.stopped() {
                        break;
                    }
                    failed += !write(path) as usize;
                    job.did(1);
                }
                report(failed, &job)
            }))
            .unwrap_or_else(|_| {
                Some(tr("Writing the sidecars stopped at a file that broke it").into())
            });
            if let Some(text) = text {
                let _ = tx.send(text);
            }
            drop(job);
            drop(pointer);
            ctx.request_repaint();
        });
        // Not sent: the work is dropped, and the busy pointer with it.
        let _ = sidecar_writer().send(work);
    }

    /// Save frame `only` of `path` as the PNG `to`, or every frame as PNG files in a new folder
    /// in `to` (never over a file), in the background, at full size.
    pub(crate) fn export_frames(
        &mut self,
        ctx: &egui::Context,
        path: PathBuf,
        only: Option<usize>,
        to: PathBuf,
    ) {
        let rotate = self.config.exif_rotate;
        // A job, so closing waits for it (or stops it between frames), as for copies.
        let job = self.start_job(tr("Exporting"), 1);
        self.background(ctx, true, move || {
            let stem = path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let folder = match only {
                Some(_) => to.clone(),
                None => (1..)
                    .map(|n| match n {
                        1 => to.join(format!("{stem}-frames")),
                        n => to.join(format!("{stem}-frames-{n}")),
                    })
                    .find(|p| !p.exists())
                    .unwrap_or_default(),
            };
            let saved = if only.is_none() && std::fs::create_dir_all(&folder).is_err() {
                Err(tf!("Could not make {folder}", folder = folder.display()))
            } else {
                decode::survive(|| {
                    decode::each_frame(&path, only, rotate, |k, img| {
                        if job.stopped() {
                            return Err(job.stopped_text());
                        }
                        let file = match only {
                            Some(_) => to.clone(),
                            None => folder.join(format!("{stem}-{:03}.png", k + 1)),
                        };
                        // Whole or not at all, also over a file the dialog let it replace.
                        let mut png = std::io::Cursor::new(Vec::new());
                        (img.write_to(&mut png, image::ImageFormat::Png))
                            .map_err(|e| e.to_string())
                            .and_then(|_| {
                                config::replace_file(&file, png.into_inner())
                                    .map_err(|e| e.to_string())
                            })
                            .map_err(|e| format!("{}: {e}", file.display()))
                    })
                })
            };
            Some(match (saved, only) {
                (Ok(0), _) => "No such frame".to_owned(),
                (Ok(_), Some(_)) => tf!("Saved {file}", file = to.display()),
                (Ok(n), None) => tf!(
                    "{frames} saved in {folder}",
                    frames = count(n, "frame"),
                    folder = folder.display()
                ),
                (Err(e), _) => tf!("Could not save the frames: {e}", e),
            })
        });
    }

    /// Copy Glimt's own folders (settings, marks and stars, collections, keyword tree, plugins,
    /// decoders) to a new folder glimt-backup-DATE in `dir`, in the background. To restore,
    /// copy them back while Glimt is closed; the README says where.
    pub(crate) fn back_up(&mut self, ctx: &egui::Context, dir: PathBuf) {
        // A job, so closing waits for the copy to be whole.
        let job = self.start_job(tr("Backing up"), 1);
        self.background(ctx, true, move || {
            let done = config::back_up_to(&dir).unwrap_or_else(|e| e);
            drop(job);
            Some(done)
        });
    }
}

#[cfg(test)]
mod tests {
    /// A moved file, a file in a moved folder, and one not moved; no trailing slash.
    #[test]
    fn reads_the_trash() {
        let dir = std::env::temp_dir().join(format!("glimt-test-trash-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("info")).unwrap();
        let info = |name: &str, path: &str, date: &str| {
            let text = format!("[Trash Info]\nPath={path}\nDeletionDate={date}\n");
            std::fs::write(dir.join("info").join(format!("{name}.trashinfo")), text).unwrap();
        };
        info(
            "bl\u{e5}b\u{e6}r x.jpg",
            "/p/bl%C3%A5b%C3%A6r%20x.jpg",
            "2026-10-21T10:00:00",
        );
        info("a.jpg", "p/a.jpg", "2026-10-21T11:00:00");
        std::fs::write(dir.join("info").join("stray"), "").unwrap();
        let mut all = super::read_trash(&dir, std::path::Path::new("/mnt"));
        all.sort_by(|a, b| a.date.cmp(&b.date));
        let got: Vec<_> = all
            .iter()
            .map(|t| (t.from.clone(), t.file.clone()))
            .collect();
        let file = |n: &str| dir.join("files").join(n);
        assert_eq!(
            got,
            [
                (
                    "/p/bl\u{e5}b\u{e6}r x.jpg".into(),
                    file("bl\u{e5}b\u{e6}r x.jpg")
                ),
                ("/mnt/p/a.jpg".into(), file("a.jpg")),
            ]
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Undo of a move to the trash puts back what that move put there and nothing else: not an
    /// older sidecar of the same name, which belonged to another file (January's IMG_0001.jpg.xmp
    /// came back on today's IMG_0001.jpg), and not over a file there; with the sidecar it moved.
    #[test]
    fn restores_only_what_the_move_trashed() {
        use std::time::{Duration, SystemTime};
        let dir = std::env::temp_dir().join(format!("glimt-test-restore-{}", std::process::id()));
        let (trash, photos) = (dir.join("trash"), dir.join("photos"));
        for d in [trash.join("files"), trash.join("info"), photos.clone()] {
            std::fs::create_dir_all(d).unwrap();
        }
        let trashed = |name: &str, kept: &str, at: SystemTime| {
            std::fs::write(trash.join("files").join(kept), name).unwrap();
            let info = trash.join("info").join(format!("{kept}.trashinfo"));
            let from = photos.join(name);
            let text = format!("[Trash Info]\nPath={}\nDeletionDate=x\n", from.display());
            std::fs::write(&info, text).unwrap();
            std::fs::File::options()
                .write(true)
                .open(&info)
                .unwrap()
                .set_modified(at)
                .unwrap();
        };
        let now = SystemTime::now();
        let january = now - Duration::from_secs(90 * 24 * 3600);
        trashed("IMG_0001.jpg.xmp", "IMG_0001.jpg.xmp", january);
        trashed("IMG_0001.jpg", "IMG_0001.jpg", now);
        trashed("IMG_0002.jpg", "IMG_0002.jpg", now);
        trashed("IMG_0002.jpg.xmp", "IMG_0002.jpg.xmp", now);
        let dirs = |_: &std::path::Path| vec![(trash.clone(), std::path::PathBuf::from("/"))];
        let thrown = |files: &[&str], sidecars: &[&str]| super::Thrown {
            files: files.iter().map(|f| photos.join(f)).collect(),
            sidecars: sidecars.iter().map(|f| photos.join(f)).collect(),
            since: Some(now - Duration::from_secs(1)),
            ..Default::default()
        };
        // IMG_0001.jpg went alone: January's sidecar of its name stays in the trash.
        let (back, error) = super::restore(&thrown(&["IMG_0001.jpg"], &[]), dirs);
        assert_eq!((back, error), (vec![photos.join("IMG_0001.jpg")], None));
        assert!(!photos.join("IMG_0001.jpg.xmp").exists());
        // Even when asked for: it is older than the move.
        let old = super::restore(&thrown(&["IMG_0001.jpg.xmp"], &[]), dirs);
        assert!(old.0.is_empty() && old.1.is_some());
        // IMG_0002.jpg went with its sidecar, and both come back; never over a file there.
        std::fs::write(photos.join("IMG_0002.jpg"), "new").unwrap();
        let taken = super::restore(&thrown(&["IMG_0002.jpg"], &["IMG_0002.jpg.xmp"]), dirs);
        assert!(taken.0.is_empty() && taken.1.is_some());
        std::fs::remove_file(photos.join("IMG_0002.jpg")).unwrap();
        let (back, error) = super::restore(&thrown(&["IMG_0002.jpg"], &["IMG_0002.jpg.xmp"]), dirs);
        assert_eq!(error, None);
        assert_eq!(
            back,
            [photos.join("IMG_0002.jpg"), photos.join("IMG_0002.jpg.xmp")]
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Entries taken out of a collection go back to their places, also when some are there again.
    #[test]
    fn puts_collection_entries_back_in_place() {
        let p = std::path::PathBuf::from;
        let mut all = vec![p("/b"), p("/d"), p("/e")];
        super::put_back(
            &mut all,
            &[(0, p("/a")), (2, p("/c")), (5, p("/f")), (6, p("/d"))],
        );
        assert_eq!(all, [p("/a"), p("/b"), p("/c"), p("/d"), p("/e"), p("/f")]);
    }

    #[test]
    fn finds_the_new_place() {
        let p = std::path::PathBuf::from;
        let pairs = vec![
            (p("/a/x.jpg"), p("/b/y.jpg")),
            (p("/trip"), p("/trip 2024")),
        ];
        let moved = super::moves(&pairs);
        let at = |s: &str| super::new_place(std::path::Path::new(s), &moved);
        assert_eq!(at("/a/x.jpg").unwrap().as_os_str(), "/b/y.jpg");
        assert_eq!(at("/trip/day/1.jpg"), Some(p("/trip 2024/day/1.jpg")));
        assert_eq!(at("/trip").unwrap().as_os_str(), "/trip 2024");
        assert_eq!(at("/a/z.jpg"), None);
    }

    /// Copying and moving never replace a file at the destination, and move or copy otherwise;
    /// a copy leaves no half file behind.
    /// One step: the new name taken by nothing else, or refused; a source that is not there
    /// is told so.
    #[test]
    fn renames_in_one_step_never_over_a_file() {
        let dir = std::env::temp_dir().join(format!("glimt-test-noreplace-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b, c) = (dir.join("a"), dir.join("b"), dir.join("c"));
        std::fs::write(&a, "a").unwrap();
        std::fs::write(&b, "b").unwrap();
        let taken = super::rename_noreplace(&a, &b).unwrap_err();
        assert_eq!(taken.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "b");
        super::rename_noreplace(&a, &c).unwrap();
        assert!(!a.exists() && c.exists());
        let gone = super::rename_noreplace(&a, &dir.join("d")).unwrap_err();
        assert_eq!(gone.kind(), std::io::ErrorKind::NotFound);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn puts_files_without_overwriting() {
        let dir = std::env::temp_dir().join(format!("glimt-test-put-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("to")).unwrap();
        let (a, b) = (dir.join("a.jpg"), dir.join("b.jpg"));
        std::fs::write(&a, "a").unwrap();
        std::fs::write(&b, "b").unwrap();
        std::fs::write(dir.join("to/b.jpg"), "there").unwrap();
        assert!(super::put_file(&b, &dir.join("to/b.jpg"), true).is_err());
        assert!(super::put_file(&b, &dir.join("to/b.jpg"), false).is_err());
        assert_eq!(
            std::fs::read_to_string(dir.join("to/b.jpg")).unwrap(),
            "there"
        );
        assert!(b.exists());
        super::put_file(&a, &dir.join("to/a.jpg"), true).unwrap();
        assert!(a.exists());
        assert_eq!(std::fs::read_to_string(dir.join("to/a.jpg")).unwrap(), "a");
        // Copied through a hidden name, which is gone, also after a refused copy.
        let mut names: Vec<_> = (std::fs::read_dir(dir.join("to")).unwrap())
            .map(|e| e.unwrap().file_name())
            .collect();
        names.sort();
        assert_eq!(names, ["a.jpg", "b.jpg"]);
        std::fs::remove_file(dir.join("to/a.jpg")).unwrap();
        super::put_file(&a, &dir.join("to/a.jpg"), false).unwrap();
        assert!(!a.exists());
        assert_eq!(std::fs::read_to_string(dir.join("to/a.jpg")).unwrap(), "a");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A copy keeps the file's time and permissions; it never replaces a file.
    #[test]
    fn copies_keep_the_time() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("glimt-test-copy-time-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a.jpg"), dir.join("b.jpg"));
        std::fs::write(&a, "a").unwrap();
        let old = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000);
        std::fs::File::options()
            .write(true)
            .open(&a)
            .unwrap()
            .set_modified(old)
            .unwrap();
        std::fs::set_permissions(&a, std::fs::Permissions::from_mode(0o640)).unwrap();
        super::put_file(&a, &b, true).unwrap();
        let meta = std::fs::metadata(&b).unwrap();
        assert_eq!(
            (meta.modified().unwrap(), meta.permissions().mode() & 0o777),
            (old, 0o640)
        );
        assert!(super::put_file(&a, &b, true).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Rename Many swaps names with their sidecars, and refuses, renaming nothing, when a new
    /// name's sidecar is another file's, also for a file without a sidecar of its own.
    #[test]
    fn renames_many_all_or_none() {
        let dir = std::env::temp_dir().join(format!("glimt-test-many-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = |n: &str| dir.join(n);
        let read = |n: &str| std::fs::read_to_string(p(n)).unwrap();
        for (n, t) in [("a.jpg", "a"), ("b.jpg", "b"), ("a.jpg.xmp", "a's")] {
            std::fs::write(p(n), t).unwrap();
        }
        let job = crate::Job::new("Renaming", 2, false);
        super::rename_files(
            vec![p("a.jpg"), p("b.jpg")],
            vec![p("b.jpg"), p("a.jpg")],
            &job,
        )
        .unwrap();
        assert_eq!(job.done.load(std::sync::atomic::Ordering::Relaxed), 2);
        assert_eq!(
            (read("a.jpg"), read("b.jpg"), read("b.jpg.xmp")),
            ("b".into(), "a".into(), "a's".into())
        );
        std::fs::write(p("c.jpg.xmp"), "c's").unwrap();
        assert!(super::rename_files(vec![p("b.jpg")], vec![p("c.jpg")], &job).is_err());
        // Also for a file without a sidecar of its own: it would show c's keywords.
        std::fs::write(p("d.jpg"), "d").unwrap();
        assert!(super::rename_files(vec![p("d.jpg")], vec![p("c.jpg")], &job).is_err());
        // Stopped before the first step: nothing renamed, told as stopped.
        job.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let stopped = super::rename_files(vec![p("d.jpg")], vec![p("e.jpg")], &job);
        assert!(stopped.unwrap_err().contains("stopped") && p("d.jpg").exists());
        assert!(p("d.jpg").exists() && !p("c.jpg").exists());
        // The one rule, by name: the file in the way first, else the stranger's sidecar, and
        // the operation's own names never.
        let ours: std::collections::HashSet<&std::path::PathBuf> = Default::default();
        assert_eq!(super::taken(&p("c.jpg"), &ours), Some(p("c.jpg.xmp")));
        assert_eq!(super::taken(&p("b.jpg"), &ours), Some(p("b.jpg")));
        assert_eq!(super::taken(&p("z.jpg"), &ours), None);
        let (b, bx) = (p("b.jpg"), p("b.jpg.xmp"));
        let ours = std::collections::HashSet::from([&b, &bx]);
        assert_eq!(super::taken(&p("b.jpg"), &ours), None);
        assert_eq!(
            (read("b.jpg"), read("c.jpg.xmp")),
            ("a".into(), "c's".into())
        );
        let left = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains("glimt-rename"))
            .count();
        assert_eq!(left, 0);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

/// `(from, to)` pairs to look up by the old path.
pub(crate) fn moves(pairs: &[(PathBuf, PathBuf)]) -> HashMap<&Path, &Path> {
    pairs
        .iter()
        .map(|(a, b)| (a.as_path(), b.as_path()))
        .collect()
}

/// Where `path` is after the `moved` ones: moved itself, or in a folder that was.
pub(crate) fn new_place(path: &Path, moved: &HashMap<&Path, &Path>) -> Option<PathBuf> {
    path.ancestors().find_map(|a| {
        let to = moved.get(a)?;
        let rest = path.strip_prefix(a).ok()?;
        Some(match rest.as_os_str().is_empty() {
            true => to.to_path_buf(),
            false => to.join(rest),
        })
    })
}
