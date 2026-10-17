//! File operations: delete or move to the trash, copy and move, Rename and Rename Many, saving
//! the orientation, and the jobs that run them in the background with their progress and Stop.
//! Copies are written under a hidden name and get their own once whole; no file is overwritten.

use crate::*;

impl crate::App {
    /// To the trash at once, permanently after confirmation.
    pub(crate) fn request_delete(&mut self, ctx: &egui::Context, trash: bool) {
        if self.files.is_empty() {
            return;
        }
        if if trash {
            self.config.confirm_trash
        } else {
            self.config.confirm_delete
        } {
            self.asked_for = (self.files_gen, self.cur);
            self.confirm_delete = Some(trash);
        } else {
            self.delete_targets_now(ctx, trash);
        }
    }

    /// The list and the shown image are still those a dialog asked about; else a notice.
    pub(crate) fn still_asked(&mut self) -> bool {
        let same = self.asked_for == (self.files_gen, self.cur);
        if !same {
            self.notice =
                Some("The list changed while the dialog was open: nothing was done".into());
        }
        same
    }

    /// No file of `paths` is being saved turned; else a notice. Deleted, moved or renamed
    /// meanwhile, a PNG or WebP would come back when its save puts it under its name again.
    pub(crate) fn none_saving(&mut self, paths: &[PathBuf]) -> bool {
        let saving = paths.iter().any(|p| self.saving.contains(p));
        if saving {
            self.notice = Some("Still saving a rotation: try again in a moment".into());
        }
        !saving
    }

    /// Files still being written: copies, moves, deletes, sidecars (`jobs`), a Rename Many, a
    /// turned image being saved. Closing waits for them.
    pub(crate) fn work_left(&self) -> bool {
        !self.file_jobs.is_empty() || self.renaming.is_some() || !self.saving.is_empty()
    }

    /// A file operation on `total` files, shown in the status bar until its thread drops it.
    pub(crate) fn start_job(&mut self, doing: &'static str, total: usize) -> Arc<Job> {
        let job = Arc::new(Job {
            doing,
            total,
            done: AtomicUsize::new(0),
            stop: Default::default(),
            started: Instant::now(),
        });
        self.file_jobs.push(job.clone());
        job
    }

    /// Delete the selected files (or the shown one) in the background, to the trash or for good,
    /// as a job with progress and Stop; the list follows when `receive_deleted` hears back.
    pub(crate) fn delete_targets_now(&mut self, ctx: &egui::Context, trash: bool) {
        let targets = self.targets();
        let paths: Vec<PathBuf> = targets.iter().map(|&i| self.path(i)).collect();
        if !self.none_saving(&paths) {
            return;
        }
        self.deleting
            .extend(targets.iter().map(|&i| self.files[i].clone()));
        self.remove_indices(ctx, &targets);
        let (tx, ctx) = (self.notices_tx.clone(), ctx.clone());
        let (freed_tx, deleted_tx) = (self.freed_tx.clone(), self.deleted_tx.clone());
        let busy = self.busy.clone();
        let job = self.start_job("Deleting", paths.len());
        busy.fetch_add(1, Ordering::Relaxed);
        std::thread::spawn(move || {
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
                            errors.push(format!("gio trash (from glib): {e}"));
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
                        errors.push(format!("{}: {e}", path.display()));
                    }
                    job.did(1);
                }
            }
            if let Some(first) = errors.first() {
                let _ = tx.send(format!(
                    "Delete failed ({}): {first}",
                    count(errors.len(), "error")
                ));
            } else if let Some(text) = job.report(|| match trash {
                true => format!("{} moved to the trash", count(paths.len(), "file")),
                false => format!("{} deleted", count(paths.len(), "file")),
            }) {
                let _ = tx.send(text);
            }
            let gone: Vec<PathBuf> = paths.iter().filter(|p| !p.exists()).cloned().collect();
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
            // Those not reached after Stop are still there, as after a failure.
            let _ = deleted_tx.send((paths, gone, !errors.is_empty() || job.stopped()));
            drop(job);
            busy.fetch_sub(1, Ordering::Relaxed);
            ctx.request_repaint();
        });
    }

    /// What deletes and moves report: files moved (their marks go along), and files gone, which
    /// leave the list, the marks and Find Duplicates; or the folder read again after a failure.
    pub(crate) fn receive_deleted(&mut self) {
        // Moved before their sources are taken for deleted ones: the marks go along first.
        for (pairs, keep) in self.put_rx.try_iter() {
            #[cfg(feature = "marks")]
            self.marks.put(pairs, keep);
            #[cfg(not(feature = "marks"))]
            let _ = (pairs, keep);
        }
        for (paths, gone, failed) in self.deleted_rx.try_iter() {
            // Deleted: their marks go, so that a new file of the same name does not get them.
            #[cfg(feature = "marks")]
            self.marks.forget(&gone);
            if let Some(dups) = &mut self.dups {
                // Those that failed are still there.
                dups.remove(&gone.iter().map(PathBuf::as_path).collect());
            }
            // By the name the list has, with its folders in the list with subfolders and in a
            // collection.
            for rel in paths.iter().filter_map(|p| p.strip_prefix(&self.dir).ok()) {
                self.deleting.remove(rel.as_os_str());
            }
            // The files that are still there come back into the list. A listing that started
            // before the files went still has them: the folder is read again after it.
            self.relist |= failed || self.listing_rx.is_some();
        }
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
        if !keep && self.collection.is_some() {
            // The collection follows the files to their new place.
            // shortcut: rewritten before the move is done, so a file that fails to move for
            // another reason than a name taken there drops out at the next refresh; report each
            // result back if moves in collections fail often.
            // The files that will get there, in the order the move takes them: not one whose
            // name is taken there already, nor a second of the same name; those keep their entry.
            let mut taken = HashSet::new();
            let moving: HashSet<&PathBuf> = (paths.iter())
                .filter(|p| {
                    let to = dest.join(p.file_name().unwrap_or_default());
                    !to.exists() && taken.insert(to)
                })
                .collect();
            for i in 0..self.files.len() {
                let path = self.path(i);
                let to = dest.join(path.file_name().unwrap_or_default());
                if moving.contains(&path)
                    && let Ok(rel) = to.strip_prefix(&self.dir)
                {
                    self.files[i] = rel.as_os_str().to_owned();
                }
            }
            self.save_collection();
        } else if !keep {
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
            let gone: Vec<usize> = (0..self.files.len())
                .filter(|&i| names.contains(self.files[i].as_os_str()))
                .collect();
            self.remove_indices(ctx, &gone);
        }
        let (tx, freed_tx, ctx) = (self.notices_tx.clone(), self.freed_tx.clone(), ctx.clone());
        let (deleted_tx, put_tx) = (self.deleted_tx.clone(), self.put_tx.clone());
        let same_dir = paths.first().and_then(|p| p.parent()) == Some(self.dir.as_path());
        // Into the folder, or one below it in the list with subfolders: the files join the list.
        let listed = dest == self.dir || (self.recursive && dest.starts_with(&self.dir));
        let busy = self.busy.clone();
        let job = self.start_job(if keep { "Copying" } else { "Moving" }, paths.len());
        busy.fetch_add(1, Ordering::Relaxed);
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
                match put_file(path, &target, keep) {
                    Ok(()) => {
                        meta::follow(path, &target, keep);
                        put.push((path.clone(), target));
                    }
                    Err(e) => errors.push(format!("{}: {e}", target.display())),
                }
                job.did(1);
            }
            let gone: Vec<PathBuf> = match keep {
                true => Vec::new(),
                false => put.iter().map(|(from, _)| from.clone()).collect(),
            };
            // Before the deleted ones below, whose marks would go otherwise.
            let _ = put_tx.send((put, keep));
            if let Some(first) = errors.first() {
                let verb = if keep { "Copy" } else { "Move" };
                let _ = tx.send(format!(
                    "{verb} failed ({}): {first}",
                    count(errors.len(), "error")
                ));
            } else if let Some(text) = job.report(|| {
                let verb = if keep { "copied" } else { "moved" };
                format!(
                    "{} {verb} to {}",
                    count(paths.len(), "file"),
                    dest.display()
                )
            }) {
                let _ = tx.send(text);
            }
            // Moved: out of `deleting`; read the folder again for files that joined it, or that
            // failed to go or were not reached after Stop.
            if listed || !keep {
                let relist = listed || !errors.is_empty() || job.stopped();
                let _ = deleted_tx.send((if keep { Vec::new() } else { paths }, gone, relist));
            }
            drop(job);
            busy.fetch_sub(1, Ordering::Relaxed);
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
            self.notice =
                Some("The pattern gives several files the same name; add # for a number".into());
            return;
        }
        // The files are looked at and renamed in the background: thousands, or a network
        // share, would hold the window.
        let shown = from.iter().position(|f| *f == self.path(self.cur));
        let job = RenameJob {
            files_gen: self.files_gen,
            from: from.clone(),
            to: to.clone(),
            shown,
        };
        let (tx, rx) = mpsc::channel();
        let (busy, ctx) = (self.busy.clone(), ctx.clone());
        busy.fetch_add(1, Ordering::Relaxed);
        std::thread::spawn(move || {
            let _ = tx.send(rename_files(from, to));
            busy.fetch_sub(1, Ordering::Relaxed);
            ctx.request_repaint();
        });
        self.renaming = Some((rx, job));
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
        self.selected.clear();
        self.selection_changed(ctx);
        #[cfg(feature = "marks")]
        if !failed {
            let pairs = job
                .from
                .iter()
                .cloned()
                .zip(job.to.iter().cloned())
                .collect();
            self.marks.put(pairs, false);
        }
        if self.collection.is_some() {
            // The collection keeps the files, under their new names, found by their old ones:
            // the list may have changed while they were renamed. Nothing renamed: the names stay.
            if !failed {
                let new: HashMap<&PathBuf, &PathBuf> = job.from.iter().zip(&job.to).collect();
                for i in 0..self.files.len() {
                    if let Some(to) = new.get(&self.path(i))
                        && let Ok(rel) = to.strip_prefix(&self.dir)
                    {
                        self.files[i] = rel.as_os_str().to_owned();
                    }
                }
                self.save_collection();
            }
            return self.list(ctx);
        }
        // The list changed meanwhile: its numbers are other files' now, so it is read again.
        if job.files_gen != self.files_gen {
            return self.list(ctx);
        }
        // Still the image shown when it began (the user may have moved on meanwhile): it stays
        // shown under its new name, with a turn not saved yet, as the list is read again. Read
        // again, not opened: the view stays in the folder, subfolders or folders it shows.
        if let Some(k) = job.shown
            && !failed
            && self.path(self.cur) == job.from[k]
            && let Ok(rel) = job.to[k].strip_prefix(&self.dir)
        {
            self.files[self.cur] = rel.as_os_str().to_owned();
        }
        self.list(ctx)
    }

    /// Write the view's rotation into the shown file and show it as it is now.
    /// shortcut: a JPEG in the UI thread, as it is read and written in milliseconds; a thread
    /// like PNG and WebP if network folders make it slow.
    pub(crate) fn save_orientation(&mut self, ctx: &egui::Context) {
        let (turns, mirrored) = self.orient;
        if self.files.is_empty() || (turns, mirrored) == (0, false) {
            return;
        }
        let path = self.path(self.cur);
        // Being saved already, with this rotation: the rotate keys wait for the save.
        if self.saving.contains(&path) {
            return;
        }
        // JPEG keeps its pixels and gets an orientation field; PNG and WebP are turned, which
        // takes seconds for a large one, so in the background.
        if !has_ext(&path, &["jpg", "jpeg"]) {
            let (rotate, quality) = (self.config.exif_rotate, self.config.webp_quality);
            let (saved, busy, ctx) = (self.saved_tx.clone(), self.busy.clone(), ctx.clone());
            self.saving.insert(path.clone());
            busy.fetch_add(1, Ordering::Relaxed);
            std::thread::spawn(move || {
                let result = survive(|| save_rotated(&path, rotate, turns, mirrored, quality));
                let _ = saved.send((path, result));
                busy.fetch_sub(1, Ordering::Relaxed);
                ctx.request_repaint();
            });
            return;
        }
        if let Err(e) = exif::write_orientation(&path, turns, mirrored) {
            self.notice = Some(format!("Could not save the orientation: {e}"));
            return;
        }
        if !self.config.exif_rotate {
            self.notice = Some("Saved; it shows when Auto Rotate Using EXIF is on".into());
        }
        self.saved(&path);
    }

    /// `path` was saved turned: decode it again and make a new thumbnail.
    pub(crate) fn saved(&mut self, path: &Path) {
        if self.is_file(self.cur, path) {
            self.orient = (0, false);
            self.decodes_pending.retain(|p, _| p.0 != self.cur);
            self.exif = None;
        }
        self.slots.retain(|_, s| s.path != path);
        // Its size changed: the status bar and the list read it again.
        self.cur_size = None;
        if let Ok(name) = path.strip_prefix(&self.dir) {
            self.thumbs.remove(name.as_os_str());
            self.thumbs_pending.remove(name.as_os_str());
            self.details.remove(name.as_os_str());
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
        if new == old {
            return;
        }
        // rename() would replace an empty folder of that name.
        let result = match new.exists() {
            true => Err(std::io::ErrorKind::AlreadyExists.into()),
            false => std::fs::rename(&old, &new),
        };
        if let Err(e) = result {
            self.notice = Some(format!("{}: {e}", new.display()));
            return;
        }
        self.forget_folder(&old);
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
        self.forget_folder(&dir);
        self.subdirs.retain(|d| *d != dir);
        if self.dir.starts_with(&dir)
            && let Some(parent) = dir.parent()
        {
            self.visit(ctx, parent.to_path_buf());
        }
        let deleted_tx = self.deleted_tx.clone();
        self.background(ctx, move || {
            let error = match command("gio").arg("trash").arg("--").arg(&dir).output() {
                Ok(o) if o.status.success() => None,
                Ok(o) => Some(String::from_utf8_lossy(&o.stderr).trim().to_owned()),
                Err(e) => Some(format!("gio trash (from glib): {e}")),
            };
            // Failed: listed again, as a failed delete of files is.
            let gone = if error.is_none() {
                vec![dir.clone()]
            } else {
                Vec::new()
            };
            let _ = deleted_tx.send((vec![dir], gone, error.is_some()));
            error.map(|e| format!("Could not move the folder to the trash: {e}"))
        });
    }

    /// Forget what the folder tree read of `dir` and its parent, so it is read again.
    pub(crate) fn forget_folder(&mut self, dir: &Path) {
        self.tree.retain(|d, _| !d.starts_with(dir));
        if let Some(parent) = dir.parent() {
            self.tree.remove(parent);
        }
    }
}

/// Copy (`keep`) or move `from` to `to`, never over a file there, also one another window put
/// there a moment ago: a copy creates its file new, a move links and then unlinks. Where links
/// cannot be made (FAT, some network shares), a move is a rename after looking first.
pub(crate) fn put_file(from: &Path, to: &Path, keep: bool) -> std::io::Result<()> {
    use std::io::ErrorKind::{AlreadyExists, CrossesDevices};
    let copy = || -> std::io::Result<()> {
        if to.exists() {
            return Err(AlreadyExists.into());
        }
        // Written under a hidden name and given its own when whole: a copy cut short (a full
        // disk, Glimt closed, a stick pulled out) never looks like the file.
        static COPIES: AtomicUsize = AtomicUsize::new(0);
        let k = COPIES.fetch_add(1, Ordering::Relaxed);
        let part = to.with_file_name(format!(".glimt-copy-{}-{k}", remote::instance_id()));
        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
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
            // A link never replaces a file; FAT sticks have none, and get a rename.
            // shortcut: a file of that name made between the check and the rename is replaced;
            // use renameat2's RENAME_NOREPLACE if another program ever writes there meanwhile.
            .and_then(|_| match std::fs::hard_link(&part, to) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == AlreadyExists => Err(e),
                Err(_) if to.exists() => Err(AlreadyExists.into()),
                Err(_) => std::fs::rename(&part, to),
            });
        let _ = std::fs::remove_file(&part);
        copied
    };
    if keep {
        return copy();
    }
    match std::fs::hard_link(from, to) {
        Ok(()) => std::fs::remove_file(from),
        Err(e) if e.kind() == AlreadyExists => Err(e),
        Err(e) if e.kind() == CrossesDevices => copy().and_then(|_| std::fs::remove_file(from)),
        Err(_) if to.exists() => Err(AlreadyExists.into()),
        Err(_) => std::fs::rename(from, to),
    }
}

/// Rename `from` to `to` with their sidecars, all or none: refused when a new name is another
/// file's, through temporary names when names swap, with the plan on disk while it runs, and
/// undone on a failure. Never replaces a file, also one another program makes meanwhile.
pub(crate) fn rename_files(from: Vec<PathBuf>, to: Vec<PathBuf>) -> Result<(), String> {
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
    // Checked with the sidecars too: one at a new name belongs to another file.
    let sources: HashSet<&PathBuf> = from.iter().collect();
    if let Some(taken) = to.iter().find(|t| t.exists() && !sources.contains(t)) {
        return Err(format!(
            "{} exists already; nothing renamed",
            taken.display()
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
    let _plan = journal::dir().and_then(|d| journal::begin(&d, &moves));
    for (k, (a, b)) in moves.iter().enumerate() {
        if let Err(e) = put_file(a, b, false) {
            // Named by the file and its new name, not a temporary one.
            let j = k % from.len();
            let name = |p: &PathBuf| p.file_name().unwrap_or_default().display().to_string();
            // Undone, so that no file is left under a hidden temporary name.
            for (a, b) in moves[..k].iter().rev() {
                let _ = std::fs::rename(b, a);
            }
            let (f, t) = (name(&from[j]), name(&to[j]));
            return Err(format!("Could not rename {f} to {t}: {e}; nothing renamed"));
        }
    }
    Ok(())
}

// ---- Work in the background: sidecars, frames, the backup and other jobs ----

impl crate::App {
    /// `work` in a thread of its own, the busy pointer showing meanwhile; what it returns is
    /// told in the window.
    pub(crate) fn background(
        &self,
        ctx: &egui::Context,
        work: impl FnOnce() -> Option<String> + Send + 'static,
    ) {
        let (tx, busy, ctx) = (self.notices_tx.clone(), self.busy.clone(), ctx.clone());
        busy.fetch_add(1, Ordering::Relaxed);
        std::thread::spawn(move || {
            if let Some(text) = work() {
                let _ = tx.send(text);
            }
            busy.fetch_sub(1, Ordering::Relaxed);
            ctx.request_repaint();
        });
    }

    /// Write the sidecars of `paths` in the background, through one thread for this window's
    /// keywords, stars and Lightroom import, so changes to one file land in the order they were
    /// made and never cross. The status bar counts them; `report` words the end from how many
    /// failed. A thread of its own, not `config::write_later`'s: settings saved meanwhile must
    /// not wait behind 3,000 sidecars, and `flush` at exit waits only for Glimt's own files.
    pub(crate) fn write_sidecars(
        &mut self,
        ctx: &egui::Context,
        doing: &'static str,
        paths: Vec<PathBuf>,
        write: impl Fn(&Path) -> bool + Send + 'static,
        report: impl FnOnce(usize, &Job) -> Option<String> + Send + 'static,
    ) {
        type Work = Box<dyn FnOnce() + Send>;
        static WRITER: std::sync::OnceLock<mpsc::Sender<Work>> = std::sync::OnceLock::new();
        let writer = WRITER.get_or_init(|| {
            let (works, rx) = mpsc::channel::<Work>();
            // One that panics must not end the thread, and with it every later write.
            std::thread::spawn(move || {
                for work in rx {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work));
                }
            });
            works
        });
        let job = self.start_job(doing, paths.len());
        let (tx, busy, ctx) = (self.notices_tx.clone(), self.busy.clone(), ctx.clone());
        self.busy.fetch_add(1, Ordering::Relaxed);
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
                Some("Writing the sidecars stopped at a file that broke it".into())
            });
            if let Some(text) = text {
                let _ = tx.send(text);
            }
            drop(job);
            busy.fetch_sub(1, Ordering::Relaxed);
            ctx.request_repaint();
        });
        if writer.send(work).is_err() {
            self.busy.fetch_sub(1, Ordering::Relaxed);
        }
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
        self.background(ctx, move || {
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
                Err(format!("Could not make {}", folder.display()))
            } else {
                decode::survive(|| {
                    decode::each_frame(&path, only, rotate, |k, img| {
                        let file = match only {
                            Some(_) => to.clone(),
                            None => folder.join(format!("{stem}-{:03}.png", k + 1)),
                        };
                        img.save_with_format(&file, image::ImageFormat::Png)
                            .map_err(|e| format!("{}: {e}", file.display()))
                    })
                })
            };
            Some(match (saved, only) {
                (Ok(0), _) => "No such frame".to_owned(),
                (Ok(_), Some(_)) => format!("Saved {}", to.display()),
                (Ok(n), None) => format!("{} saved in {}", count(n, "frame"), folder.display()),
                (Err(e), _) => format!("Could not save the frames: {e}"),
            })
        });
    }

    /// Copy Glimt's own folders (settings, marks and stars, collections, keyword tree, plugins,
    /// decoders) to a new folder glimt-backup-DATE in `dir`, in the background. To restore,
    /// copy them back while Glimt is closed; the README says where.
    pub(crate) fn back_up(&mut self, ctx: &egui::Context, dir: PathBuf) {
        self.background(ctx, move || {
            Some(config::back_up_to(&dir).unwrap_or_else(|e| e))
        });
    }
}

#[cfg(test)]
mod tests {
    /// Copying and moving never replace a file at the destination, and move or copy otherwise;
    /// a copy leaves no half file behind.
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
    /// name's sidecar is another file's.
    #[test]
    fn renames_many_all_or_none() {
        let dir = std::env::temp_dir().join(format!("glimt-test-many-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = |n: &str| dir.join(n);
        let read = |n: &str| std::fs::read_to_string(p(n)).unwrap();
        for (n, t) in [("a.jpg", "a"), ("b.jpg", "b"), ("a.jpg.xmp", "a's")] {
            std::fs::write(p(n), t).unwrap();
        }
        super::rename_files(vec![p("a.jpg"), p("b.jpg")], vec![p("b.jpg"), p("a.jpg")]).unwrap();
        assert_eq!(
            (read("a.jpg"), read("b.jpg"), read("b.jpg.xmp")),
            ("b".into(), "a".into(), "a's".into())
        );
        std::fs::write(p("c.jpg.xmp"), "c's").unwrap();
        assert!(super::rename_files(vec![p("b.jpg")], vec![p("c.jpg")]).is_err());
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
