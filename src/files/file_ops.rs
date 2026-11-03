//! File operations: delete or move to the trash, copy and move, Rename and Rename Many, saving
//! the orientation, and the jobs that run them in the background with their progress and Stop.
//! Copies are written under a hidden name and get their own once whole; no file is overwritten.

use crate::actions::Action;
use crate::dialogs::Pick;
use crate::dialogs::Prompt;
use crate::files::external;
use crate::files::file_io::{put_file, rename_never_over};
use crate::files::trash::{Thrown, restore, trash_dirs};
use crate::images::decode::survive;
use crate::images::save::save_rotated;
use crate::model::folder::total_size;
use crate::model::listing::pattern_name;
use crate::support::format::{count, thousands};
use crate::support::lang::tr;
use crate::support::threads::{Receiving, Working};
use crate::{
    files::journal, images::decode, metadata::exif, metadata::xmp, model::collections, tf,
};
use eframe::egui;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Instant;

/// A Rename Many running in the background: the list as it was (`App::files_gen`), the files
/// renamed with their new names, and which of them was shown.
pub(crate) struct RenameJob {
    files_gen: usize,
    /// The old names, for the marks and a collection's entries to follow.
    from: Vec<PathBuf>,
    to: Vec<PathBuf>,
    shown: Option<usize>,
    /// Undo giving the old names back: not itself undone.
    undoing: bool,
}

/// What a delete, move, copy or plugin reports, in one message: the files it was asked to
/// delete or move, those gone now (found out in its thread, not one `stat` each in the
/// window's), whether to read the folder again, and the files moved or copied `(from, to)`,
/// whose marks follow before the gone ones are forgotten; the Undo it makes possible, the
/// marks an Undo puts back, and files changed or written over. The one report of what happened
/// to files on disk, and `receive_deleted` the one place that brings everything that follows a
/// file along: marks, the collection, a turn not saved, decoded images, thumbnails, details and
/// the `exif:` index. One message, so nothing depends on the order two would arrive in.
#[derive(Default)]
pub(crate) struct Deleted {
    pub(crate) paths: Vec<PathBuf>,
    pub(crate) gone: Vec<PathBuf>,
    pub(crate) failed: bool,
    pub(crate) moved: Vec<(PathBuf, PathBuf)>,
    pub(crate) copied: bool,
    /// The folder is to be read again: a plugin edited files in it, a repair renamed them.
    pub(crate) relist: bool,
    /// What Undo puts back, when this was a move to the trash or a move.
    pub(crate) undo: Option<Undo>,
    /// Marks and stars to put back: of files Undo brought back from the trash.
    pub(crate) restored_marks: Vec<(PathBuf, (u8, u8))>,
    /// Files a plugin changed in place: what is shown of them is read again. (A turn saved
    /// comes through `saved_rx`, which also ends its guard, into the same `files_changed`.)
    pub(crate) changed: Vec<PathBuf>,
    /// Files written over by another image (Save As, Save This Frame onto a file there): read
    /// again, and their marks and stars go, as their sidecar did.
    pub(crate) overwritten: Vec<PathBuf>,
    /// A folder renamed (old, new), which the window follows.
    pub(crate) folder_renamed: Option<(PathBuf, PathBuf)>,
    /// A folder made (New Folder): the tree reads its parent again, the list its folder.
    pub(crate) folder_made: Option<PathBuf>,
}

/// What Edit → Undo puts back: the last files or folder moved to the trash, the last files
/// moved `(from, to)`, or the last renamed (old names, new names).
pub(crate) enum Undo {
    Trash(Thrown),
    Move(Vec<(PathBuf, PathBuf)>),
    Rename(Vec<PathBuf>, Vec<PathBuf>),
}

/// A file operation running in the background (copy, move, delete, keywords, a Lightroom
/// import), for the status bar: how far it is, and a Stop that takes effect between files.
pub(crate) struct Job {
    /// "Copying", "Deleting" …, before "120 of 3,000".
    pub(crate) doing: &'static str,
    pub(crate) total: usize,
    pub(crate) done: AtomicUsize,
    pub(crate) stop: std::sync::atomic::AtomicBool,
    pub(crate) started: Instant,
    /// Every job writes files: closing waits for it, as long as its thread holds the job.
    _writing: Working,
    /// Writes the sidecars of its files: they must not be moved, renamed or deleted meanwhile.
    pub(crate) sidecars: bool,
    /// The files and folders it changes: no other operation starts on them meanwhile.
    pub(crate) touches: Vec<PathBuf>,
}

impl Job {
    /// A job on `total` files, holding the writing count until it is dropped.
    pub(crate) fn new(
        doing: &'static str,
        total: usize,
        sidecars: bool,
        touches: Vec<PathBuf>,
    ) -> Job {
        Job {
            doing,
            total,
            done: AtomicUsize::new(0),
            stop: Default::default(),
            started: Instant::now(),
            _writing: Working::new(&crate::files::file_io::WRITING),
            sidecars,
            touches,
        }
    }

    /// Whether Stop was pressed.
    pub(crate) fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    pub(crate) fn did(&self, n: usize) {
        self.done.fetch_add(n, Ordering::Relaxed);
    }

    /// How far a stopped one got.
    pub(crate) fn stopped_text(&self) -> String {
        let done = self.done.load(Ordering::Relaxed);
        tf!(
            "Stopped: {done} of {total}",
            done = thousands(done),
            total = count(self.total, "file")
        )
    }

    /// What to tell when it ends: how far a stopped one got; `finished` for one that took long
    /// enough to be lost sight of; nothing for a quick one, which the list already shows.
    pub(crate) fn report(&self, finished: impl FnOnce() -> String) -> Option<String> {
        if self.stopped() {
            Some(self.stopped_text())
        } else if self.started.elapsed() > std::time::Duration::from_secs(2) {
            Some(finished())
        } else {
            None
        }
    }
}

/// File operations in the background: the jobs, the files they touch, what they report, and Undo.
pub(crate) struct Work {
    /// File operations running, for the status bar; one is over when its thread let go of it.
    pub(crate) jobs: Vec<Arc<Job>>,
    /// What `background` work changes, held by its thread as long as it runs.
    pub(crate) touching: Vec<Arc<Vec<PathBuf>>>,
    /// A Rename Many running in the background.
    pub(crate) renaming: Option<RenameJob>,
    pub(crate) renaming_rx: Receiving<Result<(), String>>,
    /// Names being deleted in the background; kept out of refreshed listings until done.
    pub(crate) deleting: HashSet<OsString>,
    /// What deletes, moves and copies report (`Deleted`). A plugin and Move Folder to Trash
    /// send it too, only for the list to be read again.
    pub(crate) deleted_tx: mpsc::Sender<Deleted>,
    pub(crate) deleted_rx: mpsc::Receiver<Deleted>,
    /// Those still being saved.
    pub(crate) saving: HashSet<PathBuf>,
    /// PNG and WebP files saved turned in the background; a plugin sends its files too, as it may
    /// have changed them, so they are decoded again.
    pub(crate) saved_tx: mpsc::Sender<(PathBuf, Result<(), String>)>,
    pub(crate) saved_rx: mpsc::Receiver<(PathBuf, Result<(), String>)>,
    /// What Edit → Undo puts back: the last move to the trash, move or rename.
    pub(crate) undo: Option<Undo>,
    pub(crate) cut_short: Vec<journal::CutShort>,
    /// Renames found cut short at the start (a crash or a power cut during Rename Many), to put
    /// right; read in the background.
    pub(crate) cut_short_rx: Receiving<Vec<journal::CutShort>>,
    /// Our own clipboard for file lists, kept so it can serve them (X11).
    pub(crate) clipboard: Option<arboard::Clipboard>,
    /// An image decoded for the clipboard; egui takes it only during a frame.
    pub(crate) clip_rx: Receiving<egui::ColorImage>,
}

impl Work {
    /// Nothing running; Rename Many's steps cut short by a crash are looked for in the
    /// background.
    pub(crate) fn new() -> Self {
        let (deleted_tx, deleted_rx) = mpsc::channel();
        let (saved_tx, saved_rx) = mpsc::channel();
        let (tx, cut_short_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(journal::dir().map_or_else(Vec::new, |d| journal::interrupted(&d)));
        });
        Work {
            jobs: Vec::new(),
            touching: Vec::new(),
            renaming: None,
            renaming_rx: Receiving::none(),
            deleting: HashSet::new(),
            deleted_tx,
            deleted_rx,
            saving: HashSet::new(),
            saved_tx,
            saved_rx,
            undo: None,
            cut_short: Vec::new(),
            cut_short_rx: Receiving::of(cut_short_rx),
            clipboard: None,
            clip_rx: Receiving::none(),
        }
    }
}

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
            self.dialogs.confirm_delete = Some(trash);
        } else {
            self.delete_now(ctx, trash, self.targets());
        }
    }

    /// Where the list is: another folder or collection opened may hold files of the same names.
    fn list_place(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut place = std::collections::hash_map::DefaultHasher::new();
        (
            &self.folder.dir,
            &self.folder.folders,
            &self.folder.collection,
        )
            .hash(&mut place);
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
        self.dialogs.asked_for = (self.list_place(), names);
    }

    /// The places now of the files the dialog asked about, those still in the list; `None`, with
    /// a notice, when none is, or another folder or collection is shown.
    pub(crate) fn asked_targets(&mut self) -> Option<Vec<usize>> {
        let (place, names) = std::mem::take(&mut self.dialogs.asked_for);
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

    /// No file of `paths`, or in a folder of them, is being saved turned, changed by another
    /// job or background work, and no keywords are being written; else a notice. Deleted, moved
    /// or renamed meanwhile, a PNG or WebP would come back when its save puts it under its name
    /// again, a sidecar written file by file would miss the files moved before their turn, and
    /// an Undo would put files back over a move. `start_job` and `background` ask it themselves,
    /// so no operation can leave it out.
    pub(crate) fn none_saving(&mut self, paths: &[PathBuf]) -> bool {
        let saving = (self.work.saving.iter()).any(|s| paths.iter().any(|p| s.starts_with(p)));
        let writing = (self.work.jobs.iter()).any(|j| j.sidecars && Arc::strong_count(j) > 1);
        // One Rename Many at a time, and nothing else on files while it runs: a step would
        // find its file gone, and its undo could not put the others back.
        let renaming = self.work.renaming.is_some();
        let busy = !saving && !writing && !renaming && self.touched(paths);
        if saving {
            self.notice = Some(tr("Still saving a rotation: try again in a moment").into());
        } else if writing {
            self.notice = Some(tr("Still writing keywords: try again in a moment").into());
        } else if renaming {
            self.notice = Some(tr("Rename Many is still renaming").into());
        } else if busy {
            self.notice = Some(tr("Still working on these files: try again in a moment").into());
        }
        !saving && !writing && !renaming && !busy
    }

    /// Whether a job or background work still running changes a file of `paths`, a folder
    /// above one, or one inside one of them. Work over is not counted, whether or not the frame
    /// has let go of it yet. Looked up by name and by the folders above, not path against path:
    /// a delete of 1,000 files beside a keywords job on 100,000 was 10^8 comparisons.
    fn touched(&self, paths: &[PathBuf]) -> bool {
        let running = (self.work.jobs.iter())
            .filter(|j| Arc::strong_count(j) > 1)
            .flat_map(|j| &j.touches)
            .chain(
                (self.work.touching.iter())
                    .filter(|t| Arc::strong_count(t) > 1)
                    .flat_map(|t| t.iter()),
            );
        let held: HashSet<&Path> = running.map(PathBuf::as_path).collect();
        if held.is_empty() {
            return false;
        }
        let above: HashSet<&Path> = held.iter().flat_map(|t| t.ancestors()).collect();
        (paths.iter())
            .any(|p| above.contains(p.as_path()) || p.ancestors().any(|a| held.contains(a)))
    }

    /// Whether files are still being written (`file_io::WRITING`): file jobs, a turned image, a
    /// print, keywords, the settings. Closing waits for them.
    pub(crate) fn work_left(&self) -> bool {
        crate::files::file_io::WRITING.load(Ordering::Relaxed) > 0
    }

    /// A file operation on `total` files that changes `touches`, shown in the status bar until its
    /// thread drops it. Returns `None`, with a notice, while something else works on them.
    pub(crate) fn start_job(
        &mut self,
        doing: &'static str,
        total: usize,
        touches: Vec<PathBuf>,
    ) -> Option<Arc<Job>> {
        self.start_job_of(doing, total, false, touches)
    }

    /// `start_job`, of the files' `sidecars` or of the files themselves.
    pub(crate) fn start_job_of(
        &mut self,
        doing: &'static str,
        total: usize,
        sidecars: bool,
        touches: Vec<PathBuf>,
    ) -> Option<Arc<Job>> {
        // Keywords and stars are never refused: an edit made in the panel would be lost. Their
        // job holds back every other operation instead (`none_saving`'s `writing`). Nor is a
        // job that changes none of the user's files (a backup).
        if !sidecars && !touches.is_empty() && !self.none_saving(&touches) {
            return None;
        }
        let job = Arc::new(Job::new(doing, total, sidecars, touches));
        self.work.jobs.push(job.clone());
        Some(job)
    }

    /// Delete the files at `targets` in the background, to the trash or for good,
    /// as a job with progress and Stop; the list follows when `receive_deleted` hears back.
    pub(crate) fn delete_now(&mut self, ctx: &egui::Context, trash: bool, targets: Vec<usize>) {
        let paths: Vec<PathBuf> = targets.iter().map(|&i| self.path(i)).collect();
        let Some(job) = self.start_job(tr("Deleting"), paths.len(), paths.clone()) else {
            return;
        };
        self.work
            .deleting
            .extend(targets.iter().map(|&i| self.list.files[i].clone()));
        self.remove_indices(ctx, &targets);
        let (tx, ctx) = (self.notices_tx.clone(), ctx.clone());
        let (freed_tx, deleted_tx) = (self.status.freed_tx.clone(), self.work.deleted_tx.clone());
        let collection = self.folder.collection.clone();
        let pointer = Working::new(&self.busy);
        std::thread::spawn(move || {
            let since = std::time::SystemTime::now();
            // Read the size before deleting, so the status bar can subtract it.
            let _ = freed_tx.send(total_size(&paths));
            ctx.request_repaint();
            let mut errors = Vec::new();
            // Of the files gone, the sidecars that went with them.
            let (mut gone, mut sides) = (Vec::new(), Vec::new());
            // Many files per gio call (and per taking of the locks), so thousands of files do
            // not become thousands of processes; few enough that Stop and the count in the
            // status bar keep up.
            for chunk in paths.chunks(100) {
                if job.stopped() {
                    break;
                }
                // A chunk's sidecars held until they too are gone: a keyword written meanwhile
                // would else make one under the name of a photo no longer there. Only for the
                // chunk, so a sidecar write in another window waits for one chunk, not the job.
                let _held = xmp::held_all(chunk);
                if trash {
                    if let Err(e) = crate::files::trash::trash(chunk) {
                        errors.push(e);
                    }
                } else {
                    for p in chunk {
                        if let Err(e) = std::fs::remove_file(p) {
                            errors.push(format!("{display}: {e}", display = p.display()));
                        }
                    }
                }
                // Their sidecars go the same way; a new file of the same name must not get
                // their keywords.
                let went: Vec<PathBuf> = chunk.iter().filter(|p| !p.exists()).cloned().collect();
                let side: Vec<PathBuf> = (went.iter())
                    .map(|p| xmp::sidecar(p))
                    .filter(|s| s.exists())
                    .collect();
                if trash && !side.is_empty() {
                    let _ = crate::files::trash::trash(&side);
                } else {
                    side.iter().for_each(|s| drop(std::fs::remove_file(s)));
                }
                gone.extend(went);
                sides.extend(side);
                job.did(chunk.len());
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
            let removed = forget_in_collection(collection.as_deref(), &gone).unwrap_or_else(|e| {
                let _ = tx.send(e);
                Vec::new()
            });
            let undo = (trash && !gone.is_empty()).then(|| {
                Undo::Trash(Thrown {
                    files: gone.clone(),
                    // Those that went: a sidecar gio could not move is still there.
                    sidecars: sides.into_iter().filter(|s| !s.exists()).collect(),
                    since,
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
    pub(crate) fn receive_deleted(&mut self, ctx: &egui::Context) {
        for Deleted {
            paths,
            gone,
            failed,
            moved,
            copied,
            relist,
            undo,
            restored_marks,
            changed,
            overwritten,
            folder_renamed,
            folder_made,
        } in self.work.deleted_rx.try_iter().collect::<Vec<_>>()
        {
            if let Some((old, new)) = folder_renamed {
                self.folder_renamed(ctx, old, new);
            }
            if let Some(made) = folder_made {
                self.forget_folder(&made);
                self.folder.relist = true;
            }
            // Moved before their sources are taken for deleted ones: the marks go along first.
            self.paths_moved(moved, copied);
            // Another image under a name: its marks are not this one's, as its sidecar was not.
            #[cfg(feature = "marks")]
            self.marks.forget(&overwritten);
            self.files_changed(&changed);
            self.files_changed(&overwritten);
            // Deleted: their marks go, so that a new file of the same name does not get them;
            // kept with the undo of a move to the trash, and put back by it.
            #[cfg(feature = "marks")]
            let undo = {
                let mut undo = undo;
                let forgotten = self.marks.forget(&gone);
                if let Some(Undo::Trash(thrown)) = &mut undo {
                    thrown.marks = forgotten;
                }
                self.marks.restore(restored_marks);
                undo
            };
            #[cfg(not(feature = "marks"))]
            drop(restored_marks);
            if undo.is_some() {
                self.work.undo = undo;
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
                let Ok(rel) = p.strip_prefix(&self.folder.dir) else {
                    continue;
                };
                unknown |= !self.work.deleting.remove(rel.as_os_str());
            }
            self.folder.relist |= unknown;
            // The files that are still there come back into the list, from its folder or
            // the collection's file. A listing that started before the files went still has
            // them: the folder is read again after it.
            self.folder.relist |=
                failed || relist || (!paths.is_empty() && self.folder.listing_rx.is_some());
        }
    }

    /// Files or folders moved or renamed `(from, to)`, or copied (`copied`): what Glimt keeps
    /// about them follows, the one place every operation reports to. Their marks go along (to a
    /// copy too); moved, a collection's entries of them, and of all in a moved folder, take the
    /// new place, in the list and in its file.
    pub(crate) fn paths_moved(&mut self, pairs: Vec<(PathBuf, PathBuf)>, copied: bool) {
        // Every message of a delete has none: no look at each name in the list for each.
        if pairs.is_empty() {
            return;
        }
        let moved = moves(&pairs);
        // A turn not saved yet goes with its file, saved under the new name.
        if !copied && let Some(to) = new_place(&self.viewing.turned, &moved) {
            self.viewing.turned = to;
        }
        if !copied && self.folder.collection.is_some() {
            let mut any = false;
            for i in 0..self.list.files.len() {
                if let Some(to) = new_place(&self.path(i), &moved)
                    && let Ok(rel) = to.strip_prefix(&self.folder.dir)
                {
                    self.list.files[i] = rel.as_os_str().to_owned();
                    any = true;
                }
            }
            let follow = |all: &mut Vec<PathBuf>| {
                for p in all {
                    if let Some(to) = new_place(p, &moved) {
                        *p = to;
                    }
                }
            };
            if any {
                self.edit_collection(follow);
            } else if let Some(file) = self
                .folder
                .collection
                .as_ref()
                .filter(|f| !collections::is_list(f))
            {
                // None in the list, as a refresh that started before they moved left them out:
                // the file follows still, quietly, as it may hold none of them.
                let _ = collections::edit(file, false, follow);
            }
            // The list again from the file: a refresh that started before they moved brings
            // the old names.
            self.folder.relist = true;
        }
        self.exif_follow_list();
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
        // A copy changes none of the files it reads.
        let touches = if keep { Vec::new() } else { paths.clone() };
        let doing = tr(if keep { "Copying" } else { "Moving" });
        let Some(job) = self.start_job(doing, paths.len(), touches) else {
            return;
        };
        // In a collection, the entries follow the files that got there, in `receive_deleted`.
        if !keep && self.folder.collection.is_none() {
            // Moved out of its folder: out of the list at once, as when deleting. Names in the
            // list are relative to the folder, also those in subfolders.
            let names: HashSet<&std::ffi::OsStr> = paths
                .iter()
                .filter(|p| p.parent() != Some(dest.as_path()))
                .filter_map(|p| p.strip_prefix(&self.folder.dir).ok())
                .map(Path::as_os_str)
                .collect();
            // Kept out of a listing that is running, as files being deleted are.
            self.work
                .deleting
                .extend(names.iter().map(|n| n.to_os_string()));
            let gone: Vec<usize> = (0..self.list.files.len())
                .filter(|&i| names.contains(self.list.files[i].as_os_str()))
                .collect();
            self.remove_indices(ctx, &gone);
        }
        let (tx, freed_tx, ctx) = (
            self.notices_tx.clone(),
            self.status.freed_tx.clone(),
            ctx.clone(),
        );
        let deleted_tx = self.work.deleted_tx.clone();
        let same_dir = paths.first().and_then(|p| p.parent()) == Some(self.folder.dir.as_path());
        // Into the folder, or one below it in the list with subfolders: the files join the list.
        let listed = dest == self.folder.dir
            || (self.folder.recursive && dest.starts_with(&self.folder.dir));
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
                // photo went, its keywords did not. A move holds the sidecar as a write does: one
                // that waited lands on no old name. Not a copy, where the photo stays; a move to
                // another disk holds it while the photo is copied, which only makes writes of
                // the same lock wait.
                let _held = (!keep).then(|| xmp::held(path));
                if let Some(at) = taken(&target, &HashSet::new()) {
                    failed(&at, std::io::ErrorKind::AlreadyExists.into());
                } else if let Err(e) = put_file(path, &target, keep) {
                    failed(&target, e);
                } else {
                    if let Err(e) = xmp::follow(path, &target, keep) {
                        failed(&xmp::sidecar(&target), e);
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
        // A job as every other writer: closing waits for it, the status bar counts it and
        // its Stop puts the names done so far back.
        let touches = from.iter().chain(&to).cloned().collect();
        let Some(steps) = self.start_job(tr("Renaming"), from.len(), touches) else {
            return;
        };
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
        std::thread::spawn(move || {
            let _ = tx.send(rename_files(from, to, &steps));
            drop(steps);
            drop(pointer);
            ctx.request_repaint();
        });
        self.work.renaming = Some(job);
        self.work.renaming_rx = Receiving::of(rx);
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
            self.work.undo = Some(Undo::Rename(job.from.clone(), job.to.clone()));
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
        if self.folder.collection.is_some() {
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
            && let Ok(rel) = job.to[k].strip_prefix(&self.folder.dir)
        {
            self.list.files[self.list.cur] = rel.as_os_str().to_owned();
        }
        self.list(ctx)
    }

    /// Write the view's rotation into the shown file, in the background, and show it as it is
    /// then. A JPEG keeps its pixels and gets an orientation field; PNG and WebP are turned,
    /// which takes seconds for a large one. Both read and write the whole file, slow on a share.
    pub(crate) fn save_orientation(&mut self, ctx: &egui::Context) {
        let (turns, mirrored) = self.viewing.orient;
        if self.list.files.is_empty() || (turns, mirrored) == (0, false) {
            return;
        }
        let path = self.viewing.turned.clone();
        // Removed or renamed by another program meanwhile: nothing to save the turn into.
        if !path.is_file() {
            return;
        }
        // Being saved already, with this rotation: the rotate keys wait for the save.
        if self.work.saving.contains(&path) {
            return;
        }
        // Not while other work changes it: Rename Many may give its name to another photo, which
        // the turn would then be saved over; a move may take it away.
        // Told as not saved: when the image is left, the turn goes with it.
        if self.work.renaming.is_some() || self.touched(std::slice::from_ref(&path)) {
            let held = tr("The rotation was not saved: other work is still changing the file");
            return self.notice = Some(held.into());
        }
        let jpeg = crate::model::listing::is_jpeg(&path);
        let (rotate, quality) = (self.config.exif_rotate, self.config.webp_quality);
        let (saved, notices) = (self.work.saved_tx.clone(), self.notices_tx.clone());
        let ctx = ctx.clone();
        self.work.saving.insert(path.clone());
        let pointer = Working::new(&self.busy);
        let writing = Working::new(&crate::files::file_io::WRITING);
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

    /// `paths` have other contents now (saved turned, edited by a plugin, written over): what
    /// is shown of them, decoded, as thumbnails, their size and date, their EXIF and the
    /// `exif:` index, is read again.
    pub(crate) fn files_changed(&mut self, paths: &[PathBuf]) {
        if paths.is_empty() {
            return;
        }
        for path in paths {
            if self.is_file(self.list.cur, path) {
                self.viewing.orient = (0, false);
                self.list
                    .decodes_pending
                    .retain(|p, _| p.0 != self.list.cur);
                self.info.exif = None;
            }
            self.list.slots.retain(|_, s| s.path != *path);
            if let Ok(name) = path.strip_prefix(&self.folder.dir) {
                let name = name.as_os_str();
                self.thumbs.textures.remove(name);
                self.thumbs.pending.remove(name);
                self.list.details.remove(name);
                // Also while the first index is being read, which may have read it before.
                self.search.exif_stale.insert(name.to_owned());
            }
        }
        // Their size changed: the status bar and the list read it again.
        self.status.cur_size = None;
        self.refill();
    }

    /// Rename `folder_target` within its parent; never over another. The open folder, or one
    /// inside it, is followed to the new name.
    pub(crate) fn rename_folder(&mut self, ctx: &egui::Context, name: &str) {
        let Some(old) = self.dialogs.folder_target.take() else {
            return;
        };
        let new = old.with_file_name(name);
        if new == old {
            return;
        }
        // In the background, as a folder on a share that stopped answering would hold the
        // window until the rename returns; what follows it comes back through `deleted_tx`.
        let deleted_tx = self.work.deleted_tx.clone();
        self.background(ctx, vec![old.clone()], move || {
            // Never over an empty folder of that name, also one made since the look.
            if let Err(e) = rename_never_over(&old, &new) {
                return Some(format!("{display}: {e}", display = new.display()));
            }
            let _ = deleted_tx.send(Deleted {
                folder_renamed: Some((old, new)),
                ..Default::default()
            });
            None
        });
    }

    /// The folder `old` renamed to `new`: the window follows it.
    fn folder_renamed(&mut self, ctx: &egui::Context, old: PathBuf, new: PathBuf) {
        self.forget_folder(&old);
        // Several folders shown together: the renamed one is read under its new name.
        for folder in &mut self.folder.folders {
            if let Ok(rest) = folder.strip_prefix(&old) {
                *folder = new.join(rest);
            }
        }
        // A collection: its own file follows the folder it is in, and its images in the folder
        // follow it to the new name, in its file too.
        if let Some(file) = &mut self.folder.collection
            && let Ok(rest) = file.strip_prefix(&old)
        {
            *file = new.join(rest);
        }
        self.paths_moved(vec![(old.clone(), new.clone())], false);
        match self.folder.dir.strip_prefix(&old) {
            Ok(rest) => {
                // The shown file is under the new name now, also for a turn not saved yet.
                self.folder.dir = new.join(rest);
                self.visit(ctx, self.folder.dir.clone())
            }
            Err(_) => self.list(ctx),
        }
    }

    /// Move `folder_target` and all in it to the trash, in the background; the open folder, or
    /// one inside it, is left for the folder above first.
    pub(crate) fn trash_folder(&mut self, ctx: &egui::Context) {
        let Some(dir) = self.dialogs.folder_target.take() else {
            return;
        };
        if !self.none_saving(std::slice::from_ref(&dir)) {
            return;
        }
        self.forget_folder(&dir);
        self.folder.subdirs.retain(|d| *d != dir);
        if self.folder.dir.starts_with(&dir)
            && let Some(parent) = dir.parent()
        {
            self.visit(ctx, parent.to_path_buf());
        }
        let deleted_tx = self.work.deleted_tx.clone();
        let collection = self.folder.collection.clone();
        self.background(ctx, vec![dir.clone()], move || {
            let since = std::time::SystemTime::now();
            let error = crate::files::trash::trash(std::slice::from_ref(&dir)).err();
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
                    sidecars: Vec::new(),
                    since,
                    marks: Vec::new(),
                    collection: collection.zip(Some(removed)),
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
        // Asked before it is taken: one refused stays to be done again.
        let touches: Vec<PathBuf> = match &self.work.undo {
            Some(Undo::Move(pairs)) => (pairs.iter())
                .flat_map(|(from, to)| [from.clone(), to.clone()])
                .collect(),
            Some(Undo::Trash(thrown)) => thrown.files.clone(),
            Some(Undo::Rename(from, to)) => from.iter().chain(to).cloned().collect(),
            None => Vec::new(),
        };
        if !self.none_saving(&touches) {
            return;
        }
        let Some(undo) = self.work.undo.take() else {
            self.notice = Some(tr("Nothing to undo").into());
            return;
        };
        let deleted_tx = self.work.deleted_tx.clone();
        match undo {
            Undo::Rename(from, to) => {
                self.rename_in_background(ctx, to, from);
                if let Some(job) = &mut self.work.renaming {
                    job.undoing = true;
                }
            }
            Undo::Move(pairs) => drop(self.background(ctx, touches, move || {
                let (mut errors, mut back) = (Vec::new(), Vec::new());
                for (from, to) in pairs {
                    let _held = xmp::held(&to);
                    let result = match taken(&from, &HashSet::new()) {
                        Some(at) => Err((at, std::io::ErrorKind::AlreadyExists.into())),
                        None => put_file(&to, &from, false).map_err(|e| (from.clone(), e)),
                    };
                    match result {
                        Ok(()) => {
                            if let Err(e) = xmp::follow(&to, &from, false) {
                                errors.push(format!("{}: {e}", xmp::sidecar(&from).display()));
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
            })),
            Undo::Trash(thrown) => drop(self.background(ctx, touches, move || {
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
                    restored_marks: marks,
                    ..Default::default()
                });
                Some(match error {
                    None => tf!(
                        "{count} back from the trash",
                        count = count(back.len(), "file")
                    ),
                    Some(e) => tf!("Undo failed: {e}", e),
                })
            })),
        }
        // A folder that came back shows in the tree again.
        self.tree.children.clear();
    }

    /// Forget what the folder tree read of `dir` and its parent, so it is read again.
    pub(crate) fn forget_folder(&mut self, dir: &Path) {
        self.tree.children.retain(|d, _| !d.starts_with(dir));
        if let Some(parent) = dir.parent() {
            self.tree.children.remove(parent);
        }
    }

    // ---- Work in the background: sidecars, frames, the backup and other jobs ----

    /// `work`, which writes files and changes `touches`, in a thread of its own, the busy pointer
    /// showing meanwhile and closing waiting for it; what it returns is told in the window.
    /// Not started, with a notice, while something else works on `touches` (`none_saving`);
    /// whether it started. Work within a job passes no `touches`: its job holds them.
    pub(crate) fn background(
        &mut self,
        ctx: &egui::Context,
        touches: Vec<PathBuf>,
        work: impl FnOnce() -> Option<String> + Send + 'static,
    ) -> bool {
        // Work that changes none of the user's files (within a job that holds them, or
        // Glimt's own collections) waits for nothing: a Lightroom import's collections were
        // refused while its own keywords were being written.
        if !touches.is_empty() && !self.none_saving(&touches) {
            return false;
        }
        let touching = Arc::new(touches);
        self.work.touching.push(touching.clone());
        let (tx, ctx) = (self.notices_tx.clone(), ctx.clone());
        let pointer = Working::new(&self.busy);
        let writing = Working::new(&crate::files::file_io::WRITING);
        std::thread::spawn(move || {
            let _writing = (writing, touching);
            if let Some(text) = work() {
                let _ = tx.send(text);
            }
            drop(pointer);
            ctx.request_repaint();
        });
        true
    }

    /// Write the sidecars of `paths` in the background, through one thread for this window's
    /// keywords, stars and Lightroom import (`sidecar_writer`), so changes to one file land in the
    /// order they were made and never cross. The status bar counts them; `report` words the end
    /// from how many failed. A thread of its own, not `file_io::write_later`'s: settings saved
    /// meanwhile must not wait behind 3,000 sidecars.
    pub(crate) fn write_sidecars(
        &mut self,
        ctx: &egui::Context,
        doing: &'static str,
        paths: Vec<PathBuf>,
        write: impl Fn(&Path) -> bool + Send + 'static,
        report: impl FnOnce(usize, &Job) -> Option<String> + Send + 'static,
    ) {
        use crate::files::file_io::Work;
        let Some(job) = self.start_job_of(doing, paths.len(), true, paths.clone()) else {
            return;
        };
        // "exif:" searches find keywords, comments and stars too: these are read again, also
        // when the first index is still being read (it may have read them before this write).
        let names = paths
            .iter()
            .filter_map(|p| p.strip_prefix(&self.folder.dir).ok());
        self.search
            .exif_stale
            .extend(names.map(|n| n.as_os_str().to_owned()));
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
        let Some(job) = self.start_job(tr("Exporting"), 1, vec![to.clone()]) else {
            return;
        };
        let deleted = self.work.deleted_tx.clone();
        self.background(ctx, Vec::new(), move || {
            // One frame over the animation itself would put a still in its place and take its
            // sidecar, marks and stars with it: refused, as Save As refuses, here in the
            // background, as a file on a share that stalls would hold the window.
            if only.is_some() && crate::images::save::same_file(&path, &to) {
                let e = "choose another name: the image itself is not changed";
                return Some(tf!("Could not save the image: {e}", e));
            }
            // The name's own bytes, also when they are not UTF-8.
            let named = |more: &str| crate::images::save::stem_and(&path, more);
            let folder = match only {
                Some(_) => to.clone(),
                None => (1..)
                    .map(|n| match n {
                        1 => to.join(named("-frames")),
                        n => to.join(named(&format!("-frames-{n}"))),
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
                            None => folder.join(named(&format!("-{:03}.png", k + 1))),
                        };
                        // Whole or not at all, also over a file the dialog let it replace, whose
                        // sidecar is not the frame's: a frame is saved without one.
                        let mut png = std::io::Cursor::new(Vec::new());
                        (img.write_to(&mut png, image::ImageFormat::Png))
                            .map_err(|e| e.to_string())
                            .and_then(|_| {
                                let png = png.into_inner();
                                crate::images::save::land(&path, &file, &png, true, true)
                                    .map_err(|e| e.to_string())
                            })
                            .map_err(|e| format!("{}: {e}", file.display()))
                    })
                })
            };
            // One frame saved over a file the dialog let it replace, maybe one in the list.
            if only.is_some() && saved.as_ref().is_ok_and(|&n| n > 0) {
                let _ = deleted.send(Deleted {
                    overwritten: vec![to.clone()],
                    ..Default::default()
                });
            }
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
        let Some(job) = self.start_job(tr("Backing up"), 1, Vec::new()) else {
            return;
        };
        self.background(ctx, Vec::new(), move || {
            let done = crate::files::backup::back_up_to(&dir).unwrap_or_else(|e| e);
            drop(job);
            Some(done)
        });
    }

    /// Copy or Move: the targets, to a folder the file chooser asks for.
    pub(crate) fn pick_copy_or_move(&mut self, ctx: &egui::Context, action: Action) {
        let paths = self.targets().iter().map(|&i| self.path(i)).collect();
        let purpose = match action {
            Action::Copy => Pick::CopyTo(paths),
            _ => Pick::MoveTo(paths),
        };
        self.pick(ctx, true, purpose);
    }

    /// File → Set as Wallpaper: the shown image, through the desktop portal, in the background.
    pub(crate) fn set_wallpaper(&mut self) {
        let (path, tx) = (self.path(self.list.cur), self.notices_tx.clone());
        std::thread::spawn(move || {
            if let Err(e) = external::set_wallpaper(&path) {
                let _ = tx.send(tf!("Could not set the wallpaper: {e}", e));
            }
        });
    }

    /// Copy Path: the targets' paths as text, quoted for Copy Path, not for Copy Path Unquoted.
    pub(crate) fn copy_paths(&mut self, ctx: &egui::Context, action: Action) {
        let quote = action == Action::CopyPath;
        let paths: Vec<String> = self
            .targets()
            .iter()
            .map(|&i| self.path(i).to_string_lossy().into_owned())
            .map(|p| if quote { format!("\"{p}\"") } else { p })
            .collect();
        ctx.copy_text(paths.join(" "));
    }

    /// Rename: the one file selected, as Delete and Move take it (right-clicked in the list, say),
    /// not the image shown beside it; its name only, without the folders a list with subfolders
    /// gives it.
    pub(crate) fn start_rename(&mut self, ctx: &egui::Context) {
        // The one file selected, as Delete and Move take it (right-clicked in the list,
        // say), not the image shown beside it.
        if let Some(&i) = self.list.selected.first().filter(|&&i| i != self.list.cur) {
            self.go(ctx, i);
        }
        // The name only: in the list with subfolders the entry has its folders too.
        if let Some(name) = self
            .list
            .files
            .get(self.list.cur)
            .and_then(|f| Path::new(f).file_name())
        {
            self.dialogs.prompt = Some((Prompt::Rename, name.to_string_lossy().into_owned()));
            self.ask_about_targets();
        }
    }
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
    [to.to_path_buf(), xmp::sidecar(to)]
        .into_iter()
        .find(|p| p.exists() && !ours.contains(p))
}

/// Rename `from` to `to` with their sidecars, all or none: refused when a new name is another
/// file's, through temporary names when names swap, with the plan on disk while it runs, and
/// undone on a failure. Never replaces a file, also one another program makes meanwhile.
pub(crate) fn rename_files(from: Vec<PathBuf>, to: Vec<PathBuf>, job: &Job) -> Result<(), String> {
    // Held as a sidecar write holds them, from before the look at which have one: a keyword
    // written meanwhile would else land on an old name.
    let _held = xmp::held_all(&from);
    // Sidecars go with their files, through the same steps.
    let (mut from, mut to) = (from, to);
    let sides: Vec<(PathBuf, PathBuf)> = (from.iter().zip(&to))
        .map(|(f, t)| (xmp::sidecar(f), xmp::sidecar(t)))
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
        f.with_file_name(format!(
            ".glimt-rename-{}-{k}",
            crate::files::file_io::own_tag()
        ))
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
            // Every file under its hidden name: synced, as the names on disk are those of the
            // start again once the swap is done, and could not tell a power cut after it from
            // one before it.
            if swap && k + 1 == moves.len() / 2 {
                plan.sync();
            }
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

/// The thread for this window's sidecar writes: keywords, comments, stars and imports land in
/// the order they were made.
fn sidecar_writer() -> &'static mpsc::Sender<crate::files::file_io::Work> {
    static WRITER: std::sync::OnceLock<mpsc::Sender<crate::files::file_io::Work>> =
        std::sync::OnceLock::new();
    WRITER.get_or_init(crate::files::file_io::serial)
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

#[cfg(test)]
mod tests {
    use crate::support::test_support::{app_on, contract_folder, settle};

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
        let job = super::Job::new("Renaming", 2, false, Vec::new());
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

    /// The stars of `path` in the window's marks; none without marks built in.
    fn stars(app: &crate::App, path: &std::path::Path) -> u8 {
        #[cfg(feature = "marks")]
        return app.marks.get(path).1;
        #[cfg(not(feature = "marks"))]
        {
            let _ = (app, path);
            0
        }
    }

    /// The sidecar's text of `path`, or "" when it has none.
    fn side(path: &std::path::Path) -> String {
        std::fs::read_to_string(crate::metadata::xmp::sidecar(path)).unwrap_or_default()
    }

    /// Work on a file holds every other operation on it back, also work started outside a job
    /// (Undo, Save As, Crop): Undo could run in the middle of a Rename Many, and a delete in the
    /// middle of an Undo. A refused Undo stays to be done again.
    #[test]
    fn work_on_a_file_holds_others_back() {
        let dir = std::env::temp_dir().join(format!("glimt-test-guard-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a.png", "b.png"] {
            image::RgbImage::new(4, 4).save(dir.join(name)).unwrap();
        }
        let a = dir.join("a.png");
        let (mut app, ctx) = app_on(&dir);
        // Work that touches none of the user's files is never held back, also while a
        // keywords job holds back everything else (a Lightroom import's collections).
        let held = app
            .start_job_of("Writing", 1, true, vec![dir.join("b.png")])
            .unwrap();
        assert!(app.background(&ctx, Vec::new(), || None));
        drop(held);
        let (go, wait) = std::sync::mpsc::channel::<()>();
        assert!(app.background(&ctx, vec![a.clone()], move || {
            let _ = wait.recv();
            None
        }));
        app.delete_now(&ctx, false, vec![0]);
        assert!(a.exists() && app.notice.is_some());
        assert_eq!(app.list.files.len(), 2);
        app.work.undo = Some(super::Undo::Move(vec![(a.clone(), dir.join("c.png"))]));
        app.undo(&ctx);
        assert!(app.work.undo.is_some());
        // Once that work is over, the delete goes ahead.
        go.send(()).unwrap();
        let wait_for = |done: &dyn Fn() -> bool| {
            (0..250).any(|_| {
                let now = done();
                if !now {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                now
            })
        };
        let free = (0..250).any(|_| {
            let now = app.none_saving(std::slice::from_ref(&a));
            if !now {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            now
        });
        assert!(free);
        app.work.undo = None;
        app.delete_now(&ctx, false, vec![0]);
        assert!(wait_for(&|| !a.exists()));
        // Thousands against thousands are answered at once, not path against path (10^8
        // comparisons froze the window for seconds).
        let many = |top: &str| -> Vec<std::path::PathBuf> {
            (0..50_000)
                .map(|k| dir.join(top).join(format!("{k}.jpg")))
                .collect()
        };
        let held = std::sync::Arc::new(many("busy"));
        app.work.touching.push(held.clone());
        let asked = many("free");
        let start = std::time::Instant::now();
        assert!(app.none_saving(&asked));
        assert!(!app.none_saving(&[dir.join("busy")]));
        assert!(!app.none_saving(&[dir.join("busy/7.jpg")]));
        assert!(start.elapsed() < std::time::Duration::from_secs(1));
        drop(held);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Every file operation brings along what follows a file (its sidecar, its stars, its entry
    /// in the collection shown) and waits while other work changes the file: the classes that
    /// came back release after release, held for every operation at once. A new operation
    /// joins here; `every_file_operation_is_in_the_contract` fails until it does.
    #[test]
    fn file_operations_keep_their_contract() {
        use std::path::Path;
        type Ctx = eframe::egui::Context;
        type Op = fn(&mut crate::App, &Ctx, &Path);
        type Check = fn(&crate::App, &Path) -> bool;
        // Each operation, run on a.png (and b.png), and what must hold after. Held back while
        // other work changes its files, except those that change none: a copy only reads them,
        // and keywords never wait (an edit would be lost; their job holds back the rest).
        let ops: [(&str, Op, Check); 10] = [
            (
                "move",
                |app, ctx, d| app.transfer(ctx, vec![d.join("a.png")], d.join("sub"), false),
                |app, d| {
                    let to = d.join("sub/a.png");
                    to.exists()
                        && !d.join("a.png").exists()
                        && side(&to) == "a"
                        && !crate::metadata::xmp::sidecar(&d.join("a.png")).exists()
                        && (!cfg!(feature = "marks")
                            || (stars(app, &to) == 3 && stars(app, &d.join("a.png")) == 0))
                },
            ),
            (
                "copy",
                |app, ctx, d| app.transfer(ctx, vec![d.join("a.png")], d.join("sub"), true),
                |app, d| {
                    let to = d.join("sub/a.png");
                    to.exists()
                        && d.join("a.png").exists()
                        && side(&to) == "a"
                        && side(&d.join("a.png")) == "a"
                        && (!cfg!(feature = "marks")
                            || (stars(app, &to) == 3 && stars(app, &d.join("a.png")) == 3))
                },
            ),
            (
                "rename",
                |app, ctx, d| {
                    app.rename_in_background(ctx, vec![d.join("a.png")], vec![d.join("c.png")])
                },
                |app, d| {
                    let to = d.join("c.png");
                    to.exists()
                        && !d.join("a.png").exists()
                        && side(&to) == "a"
                        && app.work.renaming.is_none()
                        && (!cfg!(feature = "marks") || stars(app, &to) == 3)
                },
            ),
            (
                "rename many",
                |app, ctx, _| {
                    app.list.selected = [0, 1].into_iter().collect();
                    app.rename_many(ctx, "x#");
                },
                |app, d| {
                    side(&d.join("x1.png")) == "a"
                        && side(&d.join("x2.png")) == "b"
                        && app.work.renaming.is_none()
                        && (!cfg!(feature = "marks")
                            || (stars(app, &d.join("x1.png")) == 3
                                && stars(app, &d.join("x2.png")) == 2))
                },
            ),
            (
                "delete",
                |app, ctx, _| app.delete_now(ctx, false, vec![0]),
                |app, d| {
                    !d.join("a.png").exists()
                        && !crate::metadata::xmp::sidecar(&d.join("a.png")).exists()
                        && (!cfg!(feature = "marks") || stars(app, &d.join("a.png")) == 0)
                },
            ),
            (
                "save as over b",
                |app, ctx, d| {
                    let how = app.shown_copy();
                    app.save_as_to(ctx, d.join("a.png"), how, d.join("b.png"));
                },
                |app, d| {
                    side(&d.join("b.png")) == "a"
                        && (!cfg!(feature = "marks") || stars(app, &d.join("b.png")) == 0)
                },
            ),
            (
                "save frame over b",
                |app, ctx, d| app.export_frames(ctx, d.join("g.gif"), Some(0), d.join("b.png")),
                |app, d| {
                    !crate::metadata::xmp::sidecar(&d.join("b.png")).exists()
                        && (!cfg!(feature = "marks") || stars(app, &d.join("b.png")) == 0)
                },
            ),
            (
                "undo a move",
                |app, ctx, d| {
                    app.transfer(ctx, vec![d.join("a.png")], d.join("sub"), false);
                    // Only when it started: held back, it says so, and there is nothing to wait
                    // for.
                    if app.notice.is_none() {
                        settle(app, ctx, |app| app.work.undo.is_some());
                    }
                    app.undo(ctx);
                },
                |app, d| {
                    let a = d.join("a.png");
                    a.exists()
                        && !d.join("sub/a.png").exists()
                        && side(&a) == "a"
                        && (!cfg!(feature = "marks") || stars(app, &a) == 3)
                },
            ),
            (
                "undo a rename",
                |app, ctx, d| {
                    app.rename_in_background(ctx, vec![d.join("a.png")], vec![d.join("c.png")]);
                    // Only when it started: held back, it says so, and there is nothing to wait
                    // for.
                    if app.notice.is_none() {
                        settle(app, ctx, |app| app.work.undo.is_some());
                    }
                    app.undo(ctx);
                },
                |app, d| {
                    let a = d.join("a.png");
                    a.exists()
                        && !d.join("c.png").exists()
                        && side(&a) == "a"
                        && app.work.renaming.is_none()
                        && (!cfg!(feature = "marks") || stars(app, &a) == 3)
                },
            ),
            (
                "keywords",
                |app, ctx, d| {
                    // The fixture's sidecar says only whose it is: no XMP Glimt writes over.
                    let _ = std::fs::remove_file(crate::metadata::xmp::sidecar(&d.join("a.png")));
                    app.write_sidecars(
                        ctx,
                        "Saving keywords",
                        vec![d.join("a.png")],
                        |p| crate::metadata::xmp::add_tags(p, &["Tag".into()]).is_ok(),
                        |_, _| None,
                    );
                },
                |_, d| side(&d.join("a.png")).contains("Tag"),
            ),
        ];
        for (name, op, check) in ops {
            let d = contract_folder(&name.replace(' ', "-"));
            let gif = d.join("g.gif");
            let mut enc = image::codecs::gif::GifEncoder::new(std::fs::File::create(&gif).unwrap());
            for c in [10u8, 200] {
                let frame = image::Frame::new(image::RgbaImage::from_pixel(
                    4,
                    4,
                    image::Rgba([c, c, c, 255]),
                ));
                enc.encode_frame(frame).unwrap();
            }
            drop(enc);
            let (mut app, ctx) = app_on(&d);
            #[cfg(feature = "marks")]
            {
                app.marks.rate(&[d.join("a.png")], 3);
                app.marks.rate(&[d.join("b.png")], 2);
            }
            // Held back while other work changes the same files: nothing moves.
            let (go, wait) = std::sync::mpsc::channel::<()>();
            let held = vec![d.join("a.png"), d.join("b.png")];
            assert!(app.background(&ctx, held, move || {
                let _ = wait.recv();
                None
            }));
            if !["copy", "keywords"].contains(&name) {
                app.notice = None;
                op(&mut app, &ctx, &d);
                std::thread::sleep(std::time::Duration::from_millis(100));
                app.poll_background(&ctx);
                let before = ["a.png", "b.png"].iter().all(|f| d.join(f).exists())
                    && side(&d.join("a.png")) == "a"
                    && side(&d.join("b.png")) == "b";
                assert!(before && app.notice.is_some(), "{name}: not held back");
            }
            go.send(()).unwrap();
            assert!(settle(&mut app, &ctx, |app| app
                .work
                .touching
                .iter()
                .all(|t| std::sync::Arc::strong_count(t) == 1)));
            // Then it runs, and what follows the file comes along.
            app.notice = None;
            op(&mut app, &ctx, &d);
            assert!(
                settle(&mut app, &ctx, |app| check(app, &d)),
                "{name}: what follows the file"
            );
            std::fs::remove_dir_all(d).unwrap();
        }
    }

    /// Every operation that starts a job or background work is in the contract above, or says
    /// here why it is not: a new one cannot leave out what follows its files unseen.
    #[test]
    fn every_file_operation_is_in_the_contract() {
        const IN_CONTRACT: &[&str] = &[
            "delete_now",
            "transfer",
            "rename_in_background",
            "undo",
            "save_as_to",
            "export_frames",
            "write_sidecars",
            "rename_folder",
        ];
        const OUTSIDE: &[(&str, &str)] = &[
            ("start_job", "the start itself"),
            (
                "save_crop",
                "a new file beside the image: save::land's tests; it needs a decoded image",
            ),
            ("trash_folder", "gio trash; Undo's restore has its own test"),
            (
                "answer_prompt",
                "New Folder: an empty folder, nothing to follow; makes_a_new_folder_in_the_background",
            ),
            ("trash_collection", "gio trash of the collection's own file"),
            ("back_up", "Glimt's own folders, no file of the user's"),
            (
                "import_lightroom",
                "needs sqlite3; a release check imports a catalog",
            ),
            (
                "cut_short_dialog",
                "journal's tests put every cut-short plan right",
            ),
        ];
        let mut found = std::collections::BTreeSet::new();
        for (_, text) in crate::support::source_rules::sources() {
            let code = text.split("\n#[cfg(test)]\nmod tests").next().unwrap();
            let mut function = String::new();
            for line in code.lines() {
                if let Some(at) = line.find("fn ") {
                    let rest = &line[at + 3..];
                    if let Some(end) = rest.find(|c: char| !(c.is_alphanumeric() || c == '_'))
                        && (rest[end..].starts_with('(') || rest[end..].starts_with('<'))
                    {
                        function = rest[..end].to_owned();
                    }
                }
                let starts = ["self.start_job(", "self.start_job_of(", "self.background("];
                if starts.iter().any(|s| line.contains(s)) && !line.trim_start().starts_with("//") {
                    found.insert(function.clone());
                }
            }
        }
        let known: Vec<&str> = IN_CONTRACT
            .iter()
            .copied()
            .chain(OUTSIDE.iter().map(|o| o.0))
            .collect();
        let unknown: Vec<&String> = found
            .iter()
            .filter(|f| !known.contains(&f.as_str()))
            .collect();
        assert!(
            unknown.is_empty(),
            "operations outside the contract: {unknown:?}"
        );
        let gone: Vec<&&str> = known.iter().filter(|k| !found.contains(**k)).collect();
        assert!(gone.is_empty(), "named here but no longer there: {gone:?}");
    }

    /// A move, a rename and a delete wait for a sidecar write of their files that holds the
    /// sidecar's lock, so the write never lands on a name the photo has left.
    #[test]
    fn file_operations_wait_for_a_sidecar_write() {
        let d = contract_folder("held");
        let (mut app, ctx) = app_on(&d);
        let idle =
            |app: &crate::App| (app.work.jobs.iter()).all(|j| std::sync::Arc::strong_count(j) == 1);
        let waits =
            |app: &mut crate::App, file: &std::path::Path, start: &dyn Fn(&mut crate::App)| {
                let held = crate::metadata::xmp::held(file);
                start(app);
                std::thread::sleep(std::time::Duration::from_millis(300));
                assert!(file.exists(), "{} gone while held", file.display());
                drop(held);
                assert!(settle(app, &ctx, |app| idle(app) && !file.exists()));
            };
        let (a, b) = (d.join("a.png"), d.join("b.png"));
        waits(&mut app, &a, &|app| {
            app.transfer(&ctx, vec![a.clone()], d.join("sub"), false)
        });
        assert!(d.join("sub/a.png").exists() && d.join("sub/a.png.xmp").exists());
        let c = d.join("c.png");
        waits(&mut app, &b, &|app| {
            app.rename_in_background(&ctx, vec![b.clone()], vec![c.clone()])
        });
        assert!(c.exists() && d.join("c.png.xmp").exists());
        assert!(settle(&mut app, &ctx, |app| app
            .folder
            .listing_rx
            .is_none()));
        waits(&mut app, &c, &|app| {
            let at = (app.list.files.iter()).position(|f| f == "c.png").unwrap();
            app.delete_now(&ctx, false, vec![at]);
        });
        assert!(!d.join("c.png.xmp").exists());
        // Many files: held a chunk (100) at a time, so one held up in a sidecar write holds up
        // only its own chunk, and those before it go meanwhile. Deleted for good: a chunk of
        // 100 files of other lock slots, then two more, of which the second is held; the first
        // of them waits too, as it is in its chunk (not one gio call and one taking of the locks
        // per file).
        let slot =
            |n: &str| crate::metadata::xmp::lock_slot(&crate::metadata::xmp::sidecar(&d.join(n)));
        let names: Vec<String> = (0..400).map(|k| format!("d{k:03}.png")).collect();
        let y = &names[0];
        let first: Vec<&String> = names
            .iter()
            .filter(|n| slot(n) != slot(y))
            .take(100)
            .collect();
        assert_eq!(first.len(), 100);
        let z = (names.iter())
            .find(|n| slot(n) != slot(y) && !first.contains(n))
            .unwrap();
        for n in first.iter().copied().chain([y, z]) {
            std::fs::write(d.join(n), "").unwrap();
        }
        for n in [first[0], y] {
            std::fs::write(crate::metadata::xmp::sidecar(&d.join(n)), n).unwrap();
        }
        app.list(&ctx);
        assert!(settle(&mut app, &ctx, |app| app
            .folder
            .listing_rx
            .is_none()));
        let at = |app: &crate::App, n: &str| (app.list.files.iter()).position(|f| f == n).unwrap();
        let mut all: Vec<usize> = first.iter().map(|n| at(&app, n)).collect();
        all.extend([at(&app, z), at(&app, y)]);
        let (x, y, z) = (d.join(first[0]), d.join(y), d.join(z));
        let held = crate::metadata::xmp::held(&y);
        app.delete_now(&ctx, false, all);
        assert!(
            settle(&mut app, &ctx, |_| first
                .iter()
                .all(|n| !d.join(n).exists())),
            "the first chunk waits not for the second"
        );
        assert!(y.exists() && crate::metadata::xmp::sidecar(&y).exists());
        assert!(z.exists(), "the second chunk is one");
        drop(held);
        assert!(settle(&mut app, &ctx, |app| idle(app)
            && !y.exists()
            && !z.exists()));
        assert!(
            !crate::metadata::xmp::sidecar(&x).exists()
                && !crate::metadata::xmp::sidecar(&y).exists()
        );
        std::fs::remove_dir_all(d).unwrap();
    }

    /// A turn is not saved while other work changes the file (Rename Many, a move): it could
    /// be saved over the photo that takes its name.
    #[test]
    fn a_turn_waits_for_other_work_on_its_file() {
        let dir = std::env::temp_dir().join(format!("glimt-test-turn-held-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.png");
        image::RgbImage::new(4, 4).save(&a).unwrap();
        let (mut app, ctx) = app_on(&dir);
        app.viewing.orient = (1, false);
        app.viewing.turned = a.clone();
        let held = app
            .start_job_of("Moving", 1, false, vec![a.clone()])
            .unwrap();
        app.notice = None;
        app.save_orientation(&ctx);
        assert!(
            app.work.saving.is_empty() && app.notice.is_some(),
            "saved while held"
        );
        drop(held);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// New Folder makes the folder in the background, and the list and the tree show it.
    #[test]
    fn makes_a_new_folder_in_the_background() {
        let d = contract_folder("new-folder");
        let (mut app, ctx) = app_on(&d);
        assert!(settle(&mut app, &ctx, |a| a.folder.listing_rx.is_none()));
        app.answer_prompt(&ctx, crate::dialogs::Prompt::NewFolder, "trip");
        assert!(settle(&mut app, &ctx, |a| {
            a.folder.listing_rx.is_none() && a.folder.subdirs.iter().any(|s| s.ends_with("trip"))
        }));
        assert!(d.join("trip").is_dir());
        // There already: said, and nothing else made.
        app.answer_prompt(&ctx, crate::dialogs::Prompt::NewFolder, "trip");
        assert!(settle(&mut app, &ctx, |a| a.notice.is_some()));
        std::fs::remove_dir_all(d).unwrap();
    }

    /// What follows a file comes along through the one report of what happened on disk: an
    /// `exif:` search was blind to a renamed file, as the index never read its new name; and a
    /// file written over by Save As kept its old marks and stars, though it lost its sidecar.
    #[test]
    fn what_follows_a_file_comes_along() {
        let dir = std::env::temp_dir().join(format!("glimt-test-follow-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a.png", "b.png"] {
            image::RgbImage::new(4, 4).save(dir.join(name)).unwrap();
        }
        let (mut app, ctx) = app_on(&dir);
        // Keywords written while the first index is still being read are read again after it.
        app.write_sidecars(
            &ctx,
            "Saving keywords",
            vec![dir.join("a.png")],
            |_| true,
            |_, _| None,
        );
        assert!(
            app.search
                .exif_stale
                .contains(std::ffi::OsStr::new("a.png"))
        );
        let names = ["a.png", "b.png"].map(|n| (std::ffi::OsString::from(n), String::new()));
        app.search.exif_index = Some(names.into_iter().collect());
        // A search while keywords are still being written keeps the marks for after: the file
        // would be read before its sidecar landed. Once written, it is read again.
        let writing = app
            .start_job_of("Saving keywords", 1, true, vec![dir.join("a.png")])
            .unwrap();
        app.search.query = "exif:x".into();
        app.update_search(&ctx);
        assert!(
            app.search
                .exif_stale
                .contains(std::ffi::OsStr::new("a.png"))
        );
        assert!(app.search.exif_index_rx.is_none());
        drop(writing);
        // The first keywords job above is over too, once the guard lets everything through.
        let over = (0..250).any(|_| {
            let now = app.none_saving(&[]);
            if !now {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            now
        });
        assert!(over);
        // The frame that sees the jobs end searches again, and the held marks are read.
        app.poll_background(&ctx);
        assert!(app.search.exif_stale.is_empty() && app.search.exif_index_rx.is_some());
        app.search.exif_index_rx = crate::support::threads::Receiving::none();
        app.search.query.clear();
        // Renamed by another program, the folder read again: the new name is read too.
        std::fs::rename(dir.join("a.png"), dir.join("c.png")).unwrap();
        app.replace_files(&ctx, vec!["b.png".into(), "c.png".into()], None);
        assert!(
            app.search
                .exif_stale
                .contains(std::ffi::OsStr::new("c.png"))
        );
        // Written over by another image: read again, and its stars gone.
        let b = dir.join("b.png");
        #[cfg(feature = "marks")]
        app.marks.rate(std::slice::from_ref(&b), 3);
        let over = super::Deleted {
            overwritten: vec![b.clone()],
            ..Default::default()
        };
        app.work.deleted_tx.send(over).unwrap();
        app.receive_deleted(&ctx);
        assert!(
            app.search
                .exif_stale
                .contains(std::ffi::OsStr::new("b.png"))
        );
        #[cfg(feature = "marks")]
        assert_eq!(app.marks.get(&b), (0, 0));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The same in a collection: its file follows a rename, a move, a folder renamed and a
    /// delete.
    #[test]
    fn a_collection_follows_its_files() {
        let d = contract_folder("collection");
        let gqv = d.join("col.gqv");
        let (mut app, ctx) = app_on(&gqv);
        let entries = |gqv: &std::path::Path| std::fs::read_to_string(gqv).unwrap();
        // Each step's work ended too: the next would be held back while it still runs.
        let idle =
            |app: &crate::App| (app.work.jobs.iter()).all(|j| std::sync::Arc::strong_count(j) == 1);
        app.rename_in_background(&ctx, vec![d.join("a.png")], vec![d.join("c.png")]);
        assert!(settle(&mut app, &ctx, |app| idle(app)
            && entries(&gqv).contains("c.png")
            && !entries(&gqv).contains("a.png")));
        app.transfer(&ctx, vec![d.join("c.png")], d.join("sub"), false);
        assert!(settle(&mut app, &ctx, |app| idle(app)
            && entries(&gqv).contains("sub/c.png")));
        // A folder renamed, in the background: its images in the collection follow.
        app.dialogs.folder_target = Some(d.join("sub"));
        app.rename_folder(&ctx, "moved");
        assert!(settle(&mut app, &ctx, |app| idle(app)
            && entries(&gqv).contains("moved/c.png")));
        assert!(d.join("moved/c.png").exists());
        let b = app
            .list
            .files
            .iter()
            .position(|f| std::path::Path::new(f).ends_with("b.png"))
            .unwrap();
        app.delete_now(&ctx, false, vec![b]);
        assert!(settle(&mut app, &ctx, |_| !entries(&gqv).contains("b.png")));
        std::fs::remove_dir_all(d).unwrap();
    }
}
