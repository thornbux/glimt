//! File operations: deleting or moving to the trash, copying and moving, Rename and Rename
//! Many, saving the orientation, and the jobs that run them in the background with their
//! progress and Stop. A copy is written under a hidden name and gets its own name once it is
//! whole. No file is ever overwritten.

use crate::files::external;
use crate::files::file_io::{put_file, rename_never_over};
use crate::files::trash::{Thrown, restore, trash_dirs};
use crate::images::decode::survive;
use crate::images::save::save_rotated;
use crate::model::folder::total_size;
use crate::model::listing::pattern_name;
use crate::support::format::{count, thousands};
use crate::support::lang::tr;
use crate::support::threads::{Receiving, Took, Working};
use crate::ui::actions::Action;
use crate::ui::dialogs::Pick;
use crate::ui::dialogs::Prompt;
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

/// The characters that FAT and exFAT refuse in a name, as on a camera card.
pub(crate) const NOT_ON_FAT: [char; 8] = [':', '?', '*', '"', '<', '>', '|', '\\'];

// The file system that a test gives every folder, on its thread. No test can mount a FAT card,
// and the tests run side by side, so one test's card is not another's.
#[cfg(test)]
thread_local! {
    pub(crate) static TEST_MOUNT: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// A Rename Many running in the background. It holds the list as it was (`App::files_gen`),
/// the files renamed with their new names, and which of them was shown.
pub(crate) struct RenameJob {
    files_gen: usize,
    /// The old names, for the marks and a collection's entries to follow.
    from: Vec<PathBuf>,
    to: Vec<PathBuf>,
    shown: Option<usize>,
    /// Whether this is an Undo giving the old names back. Such a rename is not undone itself.
    undoing: bool,
}

/// What a delete, move, copy or plugin reports, in one message. It holds the files it was asked
/// to delete or move, and those that are gone now. The thread finds out which are gone, so the
/// window does not `stat` each one. It also says whether to read the folder again, and holds
/// the files moved or copied `(from, to)`, whose marks follow before the gone ones are
/// forgotten. Last come the Undo it makes possible, the marks an Undo puts back, and the files
/// changed or written over.
///
/// This is the one report of what happened to files on disk. `receive_deleted` is the one place
/// that brings along everything that follows a file: marks, the collection, a turn not saved,
/// decoded images, thumbnails, details and the `exif:` index. It is one message, so nothing
/// depends on the order in which two messages would arrive.
#[derive(Default)]
pub(crate) struct Deleted {
    pub(crate) paths: Vec<PathBuf>,
    pub(crate) gone: Vec<PathBuf>,
    pub(crate) failed: bool,
    pub(crate) moved: Vec<(PathBuf, PathBuf)>,
    pub(crate) copied: bool,
    /// Whether the folder is to be read again, because a plugin edited files in it or a repair
    /// renamed them.
    pub(crate) relist: bool,
    /// What Undo puts back, when this was a move to the trash or a move.
    pub(crate) undo: Option<Undo>,
    /// Whether `undo` is what an Undo from the trash could not put back. It is kept only when no
    /// operation that finished meanwhile left an Undo of its own, which is newer.
    pub(crate) undo_left: bool,
    /// The marks and stars to put back, for files that Undo brought back from the trash.
    pub(crate) restored_marks: Vec<(PathBuf, (u8, u8))>,
    /// The files a plugin changed in place. What is shown of them is read again. (A turn that
    /// was saved comes through `saved_rx` into the same `files_changed`, and that also ends its
    /// guard.)
    pub(crate) changed: Vec<PathBuf>,
    /// The files written over by another image (Save As, or Save This Frame onto a file there).
    /// They are read again, and their marks and stars go, as their sidecar did.
    pub(crate) overwritten: Vec<PathBuf>,
    /// A folder renamed (old, new), which the window follows.
    pub(crate) folder_renamed: Option<(PathBuf, PathBuf)>,
    /// A folder made by New Folder. The tree reads its parent again, and the list reads its
    /// folder again.
    pub(crate) folder_made: Option<PathBuf>,
    /// The type of a network or cloud mount whose files the trash refused, and those files,
    /// which are still there. The window asks whether to delete them for good instead.
    pub(crate) untrashable: Option<(String, Vec<PathBuf>)>,
}

/// A file's (device, inode, size, modification time in seconds and nanoseconds). The inode
/// stays the same through a rename on one disk. FAT, exFAT and FUSE without use_ino may give the
/// same file another inode later, so there the size and time tell the file instead.
pub(crate) type FileId = (u64, u64, u64, (i64, i64));

/// A file moved, `(from, to, id)`, as Undo moves it back. The id is the file's at `to`, or
/// `None` once Glimt itself wrote the file anew there (a turn saved), which gives it a new id.
pub(crate) type Moved = (PathBuf, PathBuf, Option<FileId>);

/// What Edit → Undo puts back. It is the last files or folder moved to the trash, the last
/// files moved `(from, to, id)`, or the last files renamed (old names, new names). The id is
/// the moved file's `FileId`, so an Undo leaves alone a file put at `to` after the move.
pub(crate) enum Undo {
    Trash(Thrown),
    Move(Vec<Moved>),
    Rename(Vec<PathBuf>, Vec<PathBuf>),
}

/// Files held for work that is not a job (`App::hold`). Other operations on them wait while it
/// lives. The work moves it into its thread, so the hold ends when the work does, also when the
/// thread panics.
pub(crate) struct Hold {
    /// The files, counted by `touched` while this lives. It is never read, only held.
    _paths: Arc<Vec<PathBuf>>,
}

/// A file operation running in the background (copy, move, delete, keywords, a Lightroom
/// import). The status bar shows how far it is, and its Stop takes effect between files.
pub(crate) struct Job {
    /// "Copying", "Deleting" …, before "120 of 3,000".
    pub(crate) doing: &'static str,
    pub(crate) total: usize,
    pub(crate) done: AtomicUsize,
    pub(crate) stop: std::sync::atomic::AtomicBool,
    pub(crate) started: Instant,
    /// Every job writes files, so closing waits for it as long as its thread holds the job.
    _writing: Working,
    /// Whether the job writes the sidecars of its files. Those files must not be moved, renamed
    /// or deleted while it runs.
    pub(crate) sidecars: bool,
    /// The files and folders the job changes. No other operation starts on them while it runs.
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

    /// What to tell when the job ends. A stopped job tells how far it got. A job that took long
    /// enough for the user to lose sight of it tells `finished`. A quick job tells nothing,
    /// because the list already shows what it did.
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

/// File operations in the background. It holds the jobs, the files they touch, what they
/// report, and Undo.
pub(crate) struct Work {
    /// The file operations running, for the status bar. A job is over when its thread has let
    /// go of it.
    jobs: Vec<Arc<Job>>,
    /// What `background` work changes, held by its thread as long as it runs.
    touching: Vec<Arc<Vec<PathBuf>>>,
    /// A Rename Many running in the background.
    renaming: Option<RenameJob>,
    renaming_rx: Receiving<Result<(), String>>,
    /// The names being deleted in the background. They are kept out of refreshed listings
    /// until the delete is done.
    deleting: HashSet<OsString>,
    /// What deletes, moves and copies report (`Deleted`). A plugin and Move Folder to Trash
    /// send it too, only for the list to be read again.
    deleted_tx: mpsc::Sender<Deleted>,
    deleted_rx: mpsc::Receiver<Deleted>,
    /// Those still being saved.
    saving: HashSet<PathBuf>,
    /// The PNG and WebP files saved turned in the background. A plugin sends its files here
    /// too, because it may have changed them, so they are decoded again.
    saved_tx: mpsc::Sender<(PathBuf, Result<(), String>)>,
    saved_rx: mpsc::Receiver<(PathBuf, Result<(), String>)>,
    /// What Edit → Undo puts back. It is the last move to the trash, move or rename.
    undo: Option<Undo>,
    cut_short: Vec<journal::CutShort>,
    /// The renames found cut short at the start, by a crash or a power cut during Rename Many,
    /// so they can be put right. They are read in the background.
    cut_short_rx: Receiving<Vec<journal::CutShort>>,
    /// The type of the mount and the files the trash refused there, to ask about once the
    /// folder is read again, and the count of listings (`Folder::listings`) when they came.
    /// The files left the list when the delete started, and only a newer listing brings them
    /// back.
    untrashable: Option<(String, Vec<PathBuf>, u64)>,
    /// Our own clipboard for file lists, kept so it can serve them (X11).
    clipboard: Option<arboard::Clipboard>,
    /// An image decoded for the clipboard. egui takes it only during a frame.
    clip_rx: Receiving<egui::ColorImage>,
}

impl Work {
    /// A `Work` with nothing running. It looks in the background for Rename Many's steps that a
    /// crash cut short.
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
            untrashable: None,
            clipboard: None,
            clip_rx: Receiving::none(),
        }
    }

    /// Where deletes, moves and copies report what they did.
    pub(crate) fn deleted_sender(&self) -> mpsc::Sender<Deleted> {
        self.deleted_tx.clone()
    }

    /// Lets go of the jobs and the `background` work that ended. True when the last job
    /// writing keywords ended now.
    pub(crate) fn let_go_of_ended(&mut self) -> bool {
        let sidecars = |jobs: &[Arc<Job>]| jobs.iter().any(|j| j.sidecars);
        let were_writing = sidecars(&self.jobs);
        self.jobs.retain(|job| Arc::strong_count(job) > 1);
        self.touching.retain(|t| Arc::strong_count(t) > 1);
        were_writing && !sidecars(&self.jobs)
    }

    /// Whether a job writing keywords runs.
    pub(crate) fn writes_keywords(&self) -> bool {
        (self.jobs.iter()).any(|j| j.sidecars && Arc::strong_count(j) > 1)
    }

    /// Returns the running jobs that write the sidecar of `file`, or of a folder it is in. Each
    /// is over once `Weak::strong_count` is 1 or less, as only `jobs` holds it then. The jobs
    /// are looked through once here, so a wait for them does not go through a Lightroom
    /// import's 100,000 files in every frame.
    pub(crate) fn writes_of(&self, file: &Path) -> Vec<std::sync::Weak<Job>> {
        (self.jobs.iter())
            .filter(|j| j.sidecars && Arc::strong_count(j) > 1)
            .filter(|j| j.touches.iter().any(|t| file.starts_with(t)))
            .map(Arc::downgrade)
            .collect()
    }

    /// The first job running, for the status bar, and how many run.
    pub(crate) fn jobs_shown(&self) -> Option<(&Job, usize)> {
        Some((self.jobs.first()?, self.jobs.len()))
    }

    /// Every job stops after the file at hand.
    pub(crate) fn stop_jobs(&self) {
        (self.jobs.iter()).for_each(|job| job.stop.store(true, Ordering::Relaxed));
    }

    /// A turned image saved in the background, or a plugin's file, with how it went.
    pub(crate) fn take_saved(&mut self) -> Option<(PathBuf, Result<(), String>)> {
        let (path, result) = self.saved_rx.try_recv().ok()?;
        self.saving.remove(&path);
        Some((path, result))
    }

    /// Whether the turn of `path` is still being saved.
    pub(crate) fn is_saving(&self, path: &Path) -> bool {
        self.saving.contains(path)
    }

    /// Takes Rename Many's answer, when it has come: the job it was and how it went.
    pub(crate) fn take_renamed(&mut self) -> Took<(Option<RenameJob>, Result<(), String>)> {
        match self.renaming_rx.take() {
            Took::Answer(result) => Took::Answer((self.renaming.take(), result)),
            // The thread ended without an answer. Closing does not wait for it, and Rename
            // Many is not refused for ever.
            Took::Gone => {
                self.renaming = None;
                Took::Gone
            }
            Took::Nothing => Took::Nothing,
        }
    }

    /// Takes the renames found cut short at the start. Returns false when the search for them
    /// stopped without an answer.
    pub(crate) fn receive_cut_short(&mut self) -> bool {
        match self.cut_short_rx.take() {
            Took::Answer(found) => self.cut_short = found,
            Took::Gone => return false,
            Took::Nothing => {}
        }
        true
    }

    /// The renames cut short, to put right.
    pub(crate) fn cut_short(&self) -> &[journal::CutShort] {
        &self.cut_short
    }

    /// The renames cut short, taken to be put right, or (Later) left for the next start.
    pub(crate) fn take_cut_short(&mut self) -> Vec<journal::CutShort> {
        std::mem::take(&mut self.cut_short)
    }

    /// Leaves out of `files` those still being deleted, so a refresh does not bring them back.
    pub(crate) fn leave_out_deleting(&self, files: &mut Vec<OsString>) {
        if !self.deleting.is_empty() {
            files.retain(|f| !self.deleting.contains(f));
        }
    }

    /// The names being deleted, taken to keep across another list of the same collection.
    pub(crate) fn take_deleting(&mut self) -> HashSet<OsString> {
        std::mem::take(&mut self.deleting)
    }

    /// The names being deleted, kept again.
    pub(crate) fn keep_deleting(&mut self, names: HashSet<OsString>) {
        self.deleting = names;
    }

    /// Forgets the names being deleted, because another folder opens.
    pub(crate) fn forget_deleting(&mut self) {
        self.deleting.clear();
    }

    /// The image for the clipboard, decoded in the background from `rx`.
    pub(crate) fn copy_image_from(&mut self, rx: mpsc::Receiver<egui::ColorImage>) {
        self.clip_rx = Receiving::of(rx);
    }

    /// The image decoded for the clipboard, once it came.
    pub(crate) fn take_image(&mut self) -> Option<egui::ColorImage> {
        match self.clip_rx.take() {
            Took::Answer(img) => Some(img),
            _ => None,
        }
    }

    /// Our own clipboard, made the first time. On X11 the clipboard is served from it, so it is
    /// kept.
    pub(crate) fn clipboard(&mut self) -> Result<&mut arboard::Clipboard, arboard::Error> {
        if self.clipboard.is_none() {
            self.clipboard = Some(arboard::Clipboard::new()?);
        }
        Ok(self.clipboard.as_mut().unwrap())
    }
}

impl crate::App {
    /// To the trash at once, permanently after confirmation.
    pub(crate) fn request_delete(&mut self, ctx: &egui::Context, trash: bool) {
        if self.list.files().is_empty() {
            return;
        }
        if if trash {
            self.config.confirm_trash
        } else {
            self.config.confirm_delete
        } {
            self.ask_about_targets();
            self.dialogs.confirm_delete(trash);
        } else {
            self.delete_now(ctx, trash, self.targets());
        }
    }

    /// Where the list is. Another folder or collection that is opened may hold files with the
    /// same names.
    fn list_place(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut place = std::collections::hash_map::DefaultHasher::new();
        (
            self.folder.dir(),
            self.folder.folders(),
            self.folder.collection(),
        )
            .hash(&mut place);
        place.finish()
    }

    /// Keeps the targets' names when a dialog about them opens. A folder that another program
    /// writes to is read again every second, and that can give them other places in the list.
    pub(crate) fn ask_about_targets(&mut self) {
        let names = self
            .targets()
            .iter()
            .map(|&i| self.list.files()[i].clone())
            .collect();
        self.dialogs.asked_about(self.list_place(), names);
    }

    /// The places now of the files the dialog asked about that are still in the list. Returns
    /// `None`, with a notice, when none of them is, or when another folder or collection is
    /// shown.
    pub(crate) fn asked_targets(&mut self) -> Option<Vec<usize>> {
        let (place, names) = self.dialogs.take_asked();
        let names: HashSet<OsString> = names.into_iter().collect();
        let found: Vec<usize> = match place == self.list_place() {
            true => (self.list.files().iter().enumerate())
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

    /// Whether no file of `paths`, or in a folder of them, is being saved turned or changed by
    /// another job or by background work, and no keywords are being written. Otherwise it shows
    /// a notice. A PNG or WebP that was deleted, moved or renamed meanwhile would come back when
    /// its save puts it under its name again. A sidecar written file by file would miss the
    /// files that were moved before their turn, and an Undo would put files back over a move.
    /// `start_job` and `background` ask it themselves, so no operation can leave it out.
    pub(crate) fn none_saving(&mut self, paths: &[PathBuf]) -> bool {
        let saving = (self.work.saving.iter()).any(|s| paths.iter().any(|p| s.starts_with(p)));
        let writing = (self.work.jobs.iter()).any(|j| j.sidecars && Arc::strong_count(j) > 1);
        // Only one Rename Many runs at a time, and nothing else works on files while it runs.
        // Otherwise a step would find its file gone, and its undo could not put the others
        // back.
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

    /// Whether a job or background work that still runs changes a file of `paths`, a folder
    /// above one, or a file inside one of them. Work that is over does not count, whether or
    /// not the frame has let go of it yet. The lookup is by name and by the folders above, not
    /// path against path, because a delete of 1,000 files beside a keywords job on 100,000 was
    /// 10^8 comparisons.
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

    /// Whether files are still being written (`file_io::WRITING`), by file jobs, a turned
    /// image, a print, keywords or the settings. Closing waits for them.
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
        // Keywords and stars are never refused, because an edit made in the panel would be
        // lost. Instead, their job holds back every other operation (`none_saving`'s
        // `writing`). A job that changes none of the user's files, such as a backup, is not
        // refused either.
        if !sidecars && !touches.is_empty() && !self.none_saving(&touches) {
            return None;
        }
        let job = Arc::new(Job::new(doing, total, sidecars, touches));
        self.work.jobs.push(job.clone());
        Some(job)
    }

    /// Deletes the files at `targets` in the background, to the trash or for good, as a job
    /// with progress and Stop. The list follows when `receive_deleted` hears back.
    pub(crate) fn delete_now(&mut self, ctx: &egui::Context, trash: bool, targets: Vec<usize>) {
        let paths: Vec<PathBuf> = targets.iter().map(|&i| self.path(i)).collect();
        let Some(job) = self.start_job(tr("Deleting"), paths.len(), paths.clone()) else {
            return;
        };
        self.work
            .deleting
            .extend(targets.iter().map(|&i| self.list.files()[i].clone()));
        self.remove_indices(ctx, &targets);
        let (tx, ctx) = (self.notices_tx.clone(), ctx.clone());
        let (freed_tx, deleted_tx) = (self.status.freed_sender(), self.work.deleted_tx.clone());
        let collection = self.folder.collection().map(Path::to_path_buf);
        let pointer = Working::new(&self.busy);
        std::thread::spawn(move || {
            let since = std::time::SystemTime::now();
            // Read the size before deleting, so the status bar can subtract it.
            let _ = freed_tx.send(total_size(&paths));
            ctx.request_repaint();
            let mut errors = Vec::new();
            // Of the files gone, the sidecars that went with them.
            let (mut gone, mut sides) = (Vec::new(), Vec::new());
            // Each gio call (and each taking of the locks) handles many files, so thousands of
            // files do not become thousands of processes. The chunks are small enough that Stop
            // and the count in the status bar keep up.
            for chunk in paths.chunks(100) {
                if job.stopped() {
                    break;
                }
                // A chunk's sidecars are held until they are gone too. Otherwise a keyword
                // written meanwhile would make a sidecar under the name of a photo that is no
                // longer there. They are held only for the chunk, so a sidecar write in another
                // window waits for one chunk rather than the whole job.
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
                // Their sidecars go the same way, so that a new file with the same name does
                // not get their keywords. A file is gone when its name is, so a link whose target
                // went is not taken for gone (`exists` follows the link).
                let is_gone = |p: &&PathBuf| std::fs::symlink_metadata(p).is_err();
                let went: Vec<PathBuf> = chunk.iter().filter(is_gone).cloned().collect();
                let side: Vec<PathBuf> = went.iter().filter_map(|p| xmp::sidecar_of(p)).collect();
                // A sidecar that stays is told, as the photo's own errors are, because the next
                // file of that name would show its keywords.
                if trash && !side.is_empty() {
                    if let Err(e) = crate::files::trash::trash(&side) {
                        errors.push(e);
                    }
                } else {
                    for s in &side {
                        if let Err(e) = std::fs::remove_file(s) {
                            errors.push(format!("{display}: {e}", display = s.display()));
                        }
                    }
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
            let (sides_gone, sidecars_left): (Vec<PathBuf>, Vec<PathBuf>) =
                (sides.into_iter()).partition(|s| std::fs::symlink_metadata(s).is_err());
            let undo = (trash && !gone.is_empty()).then(|| {
                Undo::Trash(Thrown {
                    files: gone.clone(),
                    // Only the sidecars that are gone count, because a sidecar gio could not
                    // move is still there. Those are kept apart, as they are still the files'.
                    sidecars: sides_gone,
                    sidecars_left,
                    since,
                    marks: Vec::new(),
                    collection: collection.zip(Some(removed)),
                })
            });
            let refused = trash && !errors.is_empty() && !job.stopped();
            // Those not reached after Stop are still there, as after a failure.
            let _ = deleted_tx.send(Deleted {
                untrashable: refused
                    .then(|| left_by_trash(&paths, crate::files::file_io::mount_type))
                    .flatten(),
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

    /// Receives what deletes and moves report. Files that moved take their marks along. Files
    /// that are gone leave the list, the marks and Find Duplicates. After a failure, the folder
    /// is read again instead.
    pub(crate) fn receive_deleted(&mut self, ctx: &egui::Context) {
        for Deleted {
            paths,
            gone,
            failed,
            moved,
            copied,
            relist,
            undo,
            undo_left,
            restored_marks,
            changed,
            overwritten,
            folder_renamed,
            folder_made,
            untrashable,
        } in self.work.deleted_rx.try_iter().collect::<Vec<_>>()
        {
            if let Some((kind, files)) = untrashable {
                let now = self.folder.listings();
                match &mut self.work.untrashable {
                    // The question names the mount of the first refusal. A file refused twice
                    // is asked about once.
                    Some((_, held, since)) => {
                        held.extend(files);
                        held.sort_unstable();
                        held.dedup();
                        *since = now;
                    }
                    none => *none = Some((kind, files, now)),
                }
            }
            if let Some((old, new)) = folder_renamed {
                self.folder_renamed(ctx, old, new);
            }
            if let Some(made) = folder_made {
                self.tree.forget(&made);
                self.folder.relist_later();
            }
            // A file written over holds another image, so its marks are not this one's, just as
            // its sidecar was not. They go before a copy's marks are put on it.
            #[cfg(feature = "marks")]
            self.marks.forget(&overwritten);
            // The moves are handled before their sources are taken for deleted files, so the
            // marks go along first.
            self.paths_moved(moved, copied);
            self.written_anew(&changed);
            self.files_changed(ctx, &changed);
            self.files_changed(ctx, &overwritten);
            self.forget_names(&gone);
            // Deleted files lose their marks, so that a new file with the same name does not
            // get them. The marks are kept with the undo of a move to the trash, and it puts
            // them back.
            #[cfg(feature = "marks")]
            let undo = {
                let mut undo = undo;
                let forgotten = self.marks.forget(&gone);
                // Added to, not replaced: an Undo that put nothing back sends its marks back.
                if let Some(Undo::Trash(thrown)) = &mut undo {
                    thrown.marks.extend(forgotten);
                }
                self.marks.restore(restored_marks);
                undo
            };
            #[cfg(not(feature = "marks"))]
            drop(restored_marks);
            if undo.is_some() && !(undo_left && self.work.undo.is_some()) {
                self.work.undo = undo;
            }
            if let Some(dups) = &mut self.dups {
                // Those that failed are still there.
                dups.remove(&gone.iter().map(PathBuf::as_path).collect());
            }
            // The files are found by the name the list has, which includes their folders in
            // the list with subfolders and in a collection.
            // A file of the folder shown that the listing did not hold back means the folder
            // was left and opened again meanwhile, so its listing may still have files that are
            // gone now.
            let mut unknown = false;
            for p in &paths {
                let Ok(rel) = p.strip_prefix(self.folder.dir()) else {
                    continue;
                };
                unknown |= !self.work.deleting.remove(rel.as_os_str());
            }
            // The files that are still there come back into the list, from its folder or from
            // the collection's file. A listing that started before the files went still has
            // them, so the folder is read again after it.
            if unknown || failed || relist || (!paths.is_empty() && self.folder.is_listing()) {
                self.folder.relist_later();
            }
        }
        // The question waits for a listing taken after the refusal came, which brings the files
        // back, and for any other dialog to close, which keeps the keys meant for it. It does
        // not wait for the folder to be still, because a folder that changes all the time, such
        // as a sync folder taking in uploads with auto refresh on, never is.
        let listed = self.folder.listings();
        if !self.dialog_open()
            && (self.work.untrashable.as_ref()).is_some_and(|(_, _, since)| listed > *since)
            && let Some((kind, files, _)) = self.work.untrashable.take()
        {
            self.ask_delete_instead(kind, &files);
        }
    }

    /// Asks whether to delete `files` for good, which the trash refused on a network or cloud
    /// mount of type `kind`. gio has no trash there, and a trash in the home folder would mean
    /// a download of each file. It asks only about files of the folder shown, by their names in
    /// the list. A notice tells of the others, which are left as they are, because another
    /// folder was opened before the question.
    fn ask_delete_instead(&mut self, kind: String, files: &[PathBuf]) {
        let dir = self.folder.dir();
        let names: Vec<OsString> = (files.iter())
            .filter_map(|p| Some(p.strip_prefix(dir).ok()?.as_os_str().to_owned()))
            .collect();
        let left = files.len() - names.len();
        if left > 0 {
            self.notice = Some(tf!(
                "The trash refused {count} ({kind}). They are still there, because another folder is open now.",
                count = count(left, "file"),
                kind
            ));
        }
        if names.is_empty() {
            return;
        }
        self.dialogs.asked_about(self.list_place(), names);
        self.dialogs.confirm_delete_instead(kind);
    }

    /// Brings along what Glimt keeps about files or folders that were moved or renamed
    /// `(from, to)`, or copied (`copied`). It is the one place every operation reports to. The
    /// marks go along, to a copy too. When the files moved, a collection's entries of them, and
    /// of everything in a moved folder, take the new place, in the list and in its file.
    pub(crate) fn paths_moved(&mut self, pairs: Vec<(PathBuf, PathBuf)>, copied: bool) {
        // No message of a delete has pairs, so each one skips the look at every name in the
        // list.
        if pairs.is_empty() {
            return;
        }
        // Both names hold another file now, or none, so a moved file's caches would show under
        // the new name what an old file of that name had.
        let names: Vec<PathBuf> = (pairs.iter())
            .flat_map(|(from, to)| [(!copied).then(|| from.clone()), Some(to.clone())])
            .flatten()
            .collect();
        self.forget_names(&names);
        let moved = moves(&pairs);
        // A turn not saved yet goes with its file, saved under the new name.
        if !copied && let Some(to) = new_place(self.viewing.turned_file(), &moved) {
            self.viewing.turned_file_moved(to);
        }
        if !copied && self.folder.collection().is_some() {
            let renamed: Vec<(usize, OsString)> = (0..self.list.files().len())
                .filter_map(|i| {
                    let to = new_place(&self.path(i), &moved)?;
                    let rel = to.strip_prefix(self.folder.dir()).ok()?;
                    Some((i, rel.as_os_str().to_owned()))
                })
                .collect();
            let any = !renamed.is_empty();
            let ctx = self.ctx.clone();
            self.rename_in_list(&ctx, renamed);
            let follow = |all: &mut Vec<PathBuf>| {
                for p in all {
                    if let Some(to) = new_place(p, &moved) {
                        *p = to;
                    }
                }
            };
            if any {
                self.edit_collection(follow);
            } else if let Some(file) = self.folder.collection() {
                // None of them are in the list, because a refresh that started before they
                // moved left them out. The file still follows, quietly, as it may hold none of
                // them.
                let _ = collections::edit_told(file, follow);
            }
            // The list is read again from the file, because a refresh that started before they
            // moved brings the old names.
            self.folder.relist_later();
        }
        self.exif_follow_list();
        #[cfg(feature = "marks")]
        self.marks.put(pairs, copied);
    }

    /// Copies or moves `paths` into `dest` in the background. Existing files are never
    /// overwritten. A move across file systems falls back to a copy and a delete.
    pub(crate) fn transfer(
        &mut self,
        ctx: &egui::Context,
        paths: Vec<PathBuf>,
        dest: PathBuf,
        keep: bool,
    ) {
        // A copy changes none of the files it reads, but it holds them: a Rename Many that ran
        // meanwhile could hand it one photo with another photo's sidecar, and a move or delete
        // could take a file away halfway.
        // A camera card refuses some characters. Saying so before the job starts names the
        // reason, where a copy that failed halfway would leave some files on the card and give
        // only the system's error for the others.
        if let Some(refused) = refused_transfer(&paths, &dest, keep, fs_type) {
            self.notice = Some(refused);
            return;
        }
        let touches = paths.clone();
        let doing = tr(if keep { "Copying" } else { "Moving" });
        let Some(job) = self.start_job(doing, paths.len(), touches) else {
            return;
        };
        // In a collection, the entries follow the files that got there, in `receive_deleted`.
        if !keep && self.folder.collection().is_none() {
            self.moving_out(ctx, &paths, &dest);
        }
        let (tx, freed_tx, ctx) = (
            self.notices_tx.clone(),
            self.status.freed_sender(),
            ctx.clone(),
        );
        let deleted_tx = self.work.deleted_tx.clone();
        let same_dir = paths.first().and_then(|p| p.parent()) == Some(self.folder.dir());
        // When the files go into the folder, or into one below it in the list with subfolders,
        // they join the list.
        let listed = dest == self.folder.dir()
            || (self.folder.recursive() && dest.starts_with(self.folder.dir()));
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
                // The file is moved first and then its sidecar, and a failed sidecar is told by
                // its own name, because the photo went and its keywords did not. A move holds the
                // sidecar as a write does, so a write that waited lands on no old name. A copy
                // does not hold it, because the photo stays. A move to another disk holds it
                // while the photo is copied, which only makes writes under the same lock wait.
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
            // A move takes the files out of `deleting`. The folder is read again for files that
            // joined it, or that failed to go or were not reached after Stop. A copy to another
            // folder does not change the list.
            let relist = (listed || !keep) && (listed || !errors.is_empty() || job.stopped());
            let undo =
                (!keep && !put.is_empty()).then(|| Undo::Move(put.iter().map(moved).collect()));
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

    /// Takes the files of `paths` that a move takes out of the open folder off the list at once,
    /// as a delete does, and keeps them out of a listing that is running.
    fn moving_out(&mut self, ctx: &egui::Context, paths: &[PathBuf], dest: &Path) {
        // A file moved out of its folder leaves the list at once, as when deleting. The
        // names in the list are relative to the folder, also those in subfolders.
        let names: HashSet<&std::ffi::OsStr> = paths
            .iter()
            .filter(|p| p.parent() != Some(dest))
            .filter_map(|p| p.strip_prefix(self.folder.dir()).ok())
            .map(Path::as_os_str)
            .collect();
        // Kept out of a listing that is running, as files being deleted are.
        self.work
            .deleting
            .extend(names.iter().map(|n| n.to_os_string()));
        let gone: Vec<usize> = (0..self.list.files().len())
            .filter(|&i| names.contains(self.list.files()[i].as_os_str()))
            .collect();
        self.remove_indices(ctx, &gone);
    }

    /// Renames the selected files by `pattern`, numbered in list order. Nothing is overwritten.
    /// If a new name is taken by a file outside the selection, nothing is renamed. When new
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

    /// Renames `from` to `to`, one file or many, in the background, because thousands of files
    /// or a network share that stalls would hold the window. `renamed` takes the result.
    pub(crate) fn rename_in_background(
        &mut self,
        ctx: &egui::Context,
        from: Vec<PathBuf>,
        to: Vec<PathBuf>,
    ) {
        // A camera card refuses some characters. Saying so before the job starts names the
        // reason, where a rename that fails halfway would only give the system's error.
        if let Some(refused) = refused_on_fat(&to, fs_type) {
            self.notice = Some(not_renamed(refused));
            return;
        }
        // It is a job like every other writer. Closing waits for it, the status bar counts it,
        // and its Stop puts back the names done so far.
        let touches = from.iter().chain(&to).cloned().collect();
        let Some(steps) = self.start_job(tr("Renaming"), from.len(), touches) else {
            return;
        };
        let shown = from.iter().position(|f| self.is_file(self.list.cur(), f));
        let job = RenameJob {
            files_gen: self.list.version(),
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

    /// Brings the list along after Rename Many has run, with the shown image under its new
    /// name.
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
        self.set_selection(ctx, Default::default());
        // The files are found by their old names, because the list may have changed while they
        // were renamed. When nothing was renamed, the names stay.
        // A rename that was itself an undo is not undone again, so Ctrl+Z twice does not give
        // the new names back.
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
        if self.folder.collection().is_some() {
            return self.list(ctx);
        }
        // When the list changed meanwhile, its numbers now point at other files, so it is read
        // again.
        if job.files_gen != self.list.version() {
            return self.list(ctx);
        }
        // When the image shown at the start is still shown (the user may have moved on), it
        // stays shown under its new name, with any turn not saved yet, while the list is read
        // again. It is read again rather than opened, so the view stays in the folder,
        // subfolders or folders it shows.
        if let Some(k) = job.shown
            && !failed
            && self.is_file(self.list.cur(), &job.from[k])
            && let Ok(rel) = job.to[k].strip_prefix(self.folder.dir())
        {
            let cur = self.list.cur();
            self.rename_in_list(ctx, vec![(cur, rel.as_os_str().to_owned())]);
        }
        self.list(ctx)
    }

    /// Writes the view's rotation into the shown file in the background, and then shows the
    /// file as it is. A JPEG keeps its pixels and gets an orientation field. PNG and WebP are
    /// turned, which takes seconds for a large one. Both read and write the whole file, which
    /// is slow on a share.
    pub(crate) fn save_orientation(&mut self, ctx: &egui::Context) {
        let (turns, mirrored) = self.viewing.orient();
        if self.list.files().is_empty() || (turns, mirrored) == (0, false) {
            return;
        }
        let path = self.viewing.turned_file().to_path_buf();
        // When another program removed or renamed the file meanwhile, there is nothing to save
        // the turn into.
        if !path.is_file() {
            return;
        }
        // When the file is already being saved with this rotation, the rotate keys wait for
        // the save.
        if self.work.saving.contains(&path) {
            return;
        }
        // The turn is not saved while other work changes the file. Rename Many may give its
        // name to another photo, and the turn would then be saved over that photo. A move may
        // take the file away.
        // It is told as not saved, so the turn goes with the image when the image is left.
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

    /// Reads again what is shown of `paths`, because they have other contents now (saved
    /// turned, edited by a plugin, written over). That covers the decoded images, thumbnails,
    /// size and date, EXIF and the `exif:` index.
    pub(crate) fn files_changed(&mut self, ctx: &egui::Context, paths: &[PathBuf]) {
        if paths.is_empty() {
            return;
        }
        for path in paths {
            if self.is_file(self.list.cur(), path) {
                self.viewing.drop_turn();
                self.list.forget_decodes_of(self.list.cur());
                self.read_info_again(ctx);
            }
        }
        self.forget_names(paths);
        // Their size changed, so the status bar and the list read it again.
        self.status.forget_size();
        self.refill();
    }

    /// Keeps `paths` as the files an Undo of a move moves back, although Glimt wrote them anew
    /// in place (a turn saved, a plugin's change) and so gave them new ids. A file written over
    /// by another image (Save As, Save This Frame) is not one of them, so it does not come here.
    pub(crate) fn written_anew(&mut self, paths: &[PathBuf]) {
        if let Some(Undo::Move(pairs)) = &mut self.work.undo {
            let written: HashSet<&PathBuf> = paths.iter().collect();
            // A folder opened through a link spells the same file otherwise, so a pair of the
            // same file name is compared by its real path too. Only the paths whose name a
            // moved file has are looked up, so a plugin's report of 200,000 files after a move
            // of one costs no look at the disk for the others.
            let moved: HashSet<std::ffi::OsString> = (pairs.iter())
                .filter(|(_, to, _)| !written.contains(to))
                .filter_map(|(_, to, _)| to.file_name().map(|n| n.to_owned()))
                .collect();
            let real: HashSet<PathBuf> = (paths.iter())
                .filter(|p| p.file_name().is_some_and(|n| moved.contains(n)))
                .filter_map(|p| p.canonicalize().ok())
                .collect();
            let same = |to: &PathBuf| {
                written.contains(to)
                    || (!real.is_empty()
                        && to.file_name().is_some_and(|n| moved.contains(n))
                        && to.canonicalize().is_ok_and(|t| real.contains(&t)))
            };
            (pairs.iter_mut())
                .filter(|(_, to, _)| same(to))
                .for_each(|(_, _, id)| *id = None);
        }
    }

    /// Forgets what Glimt keeps of `paths` by their names: their decoded images, thumbnails, size
    /// and date, Check Images' mark and their part of the `exif:` index. A file of the same name
    /// later, renamed or moved there, or written anew, is then read for itself. Every operation
    /// that changes what a name holds calls this: a change in place, a move, a rename, a delete.
    pub(crate) fn forget_names(&mut self, paths: &[PathBuf]) {
        for path in paths {
            self.list.keep_slots(|_, s| s.path != *path);
            if let Ok(name) = path.strip_prefix(self.folder.dir()) {
                let name = name.as_os_str();
                self.thumbs.forget(name);
                self.list.forget_details_of(name);
                self.broken_files.remove(name);
                // Also while the first index is being read, which may have read it before.
                self.search.read_exif_again([name.to_owned()]);
            }
        }
    }

    /// Renames `folder_target` within its parent, never over another folder. The open folder,
    /// or one inside it, is followed to the new name.
    pub(crate) fn rename_folder(&mut self, ctx: &egui::Context, name: &str) {
        let Some(old) = self.dialogs.take_folder() else {
            return;
        };
        let new = old.with_file_name(name);
        if new == old {
            return;
        }
        // A camera card refuses some characters, and its own error would not say which.
        if let Some(refused) = refused_on_fat(std::slice::from_ref(&new), fs_type) {
            self.notice = Some(not_renamed(refused));
            return;
        }
        // This runs in the background, because a folder on a share that stopped answering
        // would hold the window until the rename returns. What follows it comes back through
        // `deleted_tx`.
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

    /// Makes the window follow the folder `old`, which was renamed to `new`.
    fn folder_renamed(&mut self, ctx: &egui::Context, old: PathBuf, new: PathBuf) {
        self.tree.forget(&old);
        // When several folders are shown together, the renamed one is read under its new
        // name. In a collection, its own file follows the folder it is in, and its images in
        // the folder follow it to the new name, in its file too.
        self.folder.follow_rename(&old, &new);
        self.paths_moved(vec![(old.clone(), new.clone())], false);
        #[cfg(feature = "marks")]
        self.marks.put_folder(old.clone(), new.clone());
        match self.folder.dir_renamed(&old, &new) {
            // The shown file is under the new name now, also for a turn not saved yet.
            Some(dir) => self.visit(ctx, dir),
            None => self.list(ctx),
        }
    }

    /// Moves `folder_target` and everything in it to the trash, in the background. When the
    /// open folder is it or inside it, the window first goes to the folder above.
    pub(crate) fn trash_folder(&mut self, ctx: &egui::Context) {
        let Some(dir) = self.dialogs.take_folder() else {
            return;
        };
        if !self.none_saving(std::slice::from_ref(&dir)) {
            return;
        }
        self.tree.forget(&dir);
        self.folder.forget_subdir(&dir);
        if self.folder.dir().starts_with(&dir)
            && let Some(parent) = dir.parent()
        {
            self.visit(ctx, parent.to_path_buf());
        }
        let deleted_tx = self.work.deleted_tx.clone();
        let collection = self.folder.collection().map(Path::to_path_buf);
        self.background(ctx, vec![dir.clone()], move || {
            let since = std::time::SystemTime::now();
            let error = crate::files::trash::trash(std::slice::from_ref(&dir)).err();
            // After a failure, the folder is listed again, as after a failed delete of files.
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
                    sidecars_left: Vec::new(),
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

    /// Edit → Undo (Ctrl+Z). The last files or folder moved to the trash come back from it, the
    /// last files moved go back, and the last files renamed get their old names. It works once,
    /// so what it put back is not undone again. Nothing is put over a file that has taken the
    /// name meanwhile.
    pub(crate) fn undo(&mut self, ctx: &egui::Context) {
        // This asks before the undo is taken, so one that is refused stays to be done again.
        // So do the files of a move to the trash that did not come back, which its thread
        // sends back.
        let touches: Vec<PathBuf> = match &self.work.undo {
            Some(Undo::Move(pairs)) => (pairs.iter())
                .flat_map(|(from, to, _)| [from.clone(), to.clone()])
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
                let (back, errors) = move_back(pairs);
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
                // What went with the files comes back with them: their places in the
                // collection, their marks and their stars.
                let set: HashSet<&Path> = back.iter().map(PathBuf::as_path).collect();
                let returned = |p: &Path| p.ancestors().any(|a| set.contains(a));
                if let Some((file, removed)) = &thrown.collection {
                    let removed: Vec<_> = (removed.iter())
                        .filter(|(_, p)| returned(p))
                        .cloned()
                        .collect();
                    if let Err(e) = collections::edit_told(file, |all| put_back(all, &removed)) {
                        error.get_or_insert(e);
                    }
                }
                // Those not put back (a name taken, a rename that failed) stay to be undone
                // again, with their marks and places, which would be lost with this Undo.
                let (marks, kept) = (thrown.marks.into_iter()).partition(|(p, _)| returned(p));
                let left: Vec<PathBuf> = (thrown.files.into_iter())
                    .filter(|p| !returned(p))
                    .collect();
                let undo = (!left.is_empty()).then(|| {
                    Undo::Trash(Thrown {
                        files: left,
                        sidecars: thrown.sidecars,
                        sidecars_left: thrown.sidecars_left,
                        since: thrown.since,
                        marks: kept,
                        collection: (thrown.collection).map(|(file, removed)| {
                            (
                                file,
                                removed.into_iter().filter(|(_, p)| !returned(p)).collect(),
                            )
                        }),
                    })
                });
                let _ = deleted_tx.send(Deleted {
                    relist: true,
                    undo,
                    undo_left: true,
                    restored_marks: marks,
                    ..Default::default()
                });
                let done = tf!(
                    "{count} back from the trash",
                    count = count(back.len(), "file")
                );
                // The files that came back are told too when others did not.
                Some(match error {
                    None => done,
                    Some(e) if back.is_empty() => tf!("Undo failed: {e}", e),
                    Some(e) => format!("{done}; {}", tf!("Undo failed: {e}", e)),
                })
            })),
        }
        // A folder that came back shows in the tree again.
        self.tree.read_again();
    }

    // ---- Work in the background: sidecars, frames, the backup and other jobs ----

    /// Holds `paths` for work that is not a job, once they are free to change. Other operations
    /// on them wait while the returned `Hold` lives, so the work moves it into its thread.
    /// Returns `None`, with a notice, while something else works on them. Work on none of the
    /// user's files (an empty `paths`) is never held back and holds nothing.
    pub(crate) fn hold(&mut self, paths: Vec<PathBuf>) -> Option<Hold> {
        if !paths.is_empty() && !self.none_saving(&paths) {
            return None;
        }
        let held = Arc::new(paths);
        self.work.touching.push(held.clone());
        Some(Hold { _paths: held })
    }

    /// Runs `work`, which writes files and changes `touches`, in a thread of its own. The busy
    /// pointer shows meanwhile, closing waits for it, and what it returns is told in the window.
    /// It is not started, and a notice says so, while something else works on `touches`
    /// (`none_saving`). Returns whether it started. Work within a job passes no `touches`,
    /// because its job holds them.
    pub(crate) fn background(
        &mut self,
        ctx: &egui::Context,
        touches: Vec<PathBuf>,
        work: impl FnOnce() -> Option<String> + Send + 'static,
    ) -> bool {
        // Work that changes none of the user's files (within a job that holds them, or on
        // Glimt's own collections) waits for nothing. A Lightroom import's collections were
        // refused while its own keywords were being written.
        let Some(hold) = self.hold(touches) else {
            return false;
        };
        let (tx, ctx) = (self.notices_tx.clone(), ctx.clone());
        let pointer = Working::new(&self.busy);
        let writing = Working::new(&crate::files::file_io::WRITING);
        std::thread::spawn(move || {
            let _writing = (writing, hold);
            if let Some(text) = work() {
                let _ = tx.send(text);
            }
            drop(pointer);
            ctx.request_repaint();
        });
        true
    }

    /// Writes the sidecars of `paths` in the background. This window's keywords, stars and
    /// Lightroom import all go through one thread (`sidecar_writer`), so changes to one file
    /// land in the order they were made and never cross. The status bar counts them, and
    /// `report` words the end from how many failed. The thread is its own rather than
    /// `file_io::write_later`'s, so settings saved meanwhile do not wait behind 3,000 sidecars.
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
        // "exif:" searches find keywords, comments and stars too, so these files are read
        // again. That holds also when the first index is still being read, because it may have
        // read them before this write.
        let names = paths
            .iter()
            .filter_map(|p| p.strip_prefix(self.folder.dir()).ok());
        (self.search).read_exif_again(names.map(|n| n.as_os_str().to_owned()));
        let (tx, ctx) = (self.notices_tx.clone(), ctx.clone());
        let pointer = Working::new(&self.busy);
        let work: Work = Box::new(move || {
            // A file that makes the writing panic ends this job, not the count or the busy
            // pointer.
            let text = crate::support::threads::caught(|| {
                let mut failed = 0;
                for path in &paths {
                    if job.stopped() {
                        break;
                    }
                    failed += !write(path) as usize;
                    job.did(1);
                }
                report(failed, &job)
            })
            .unwrap_or_else(|| {
                Some(tr("Writing the sidecars stopped at a file that broke it").into())
            });
            if let Some(text) = text {
                let _ = tx.send(text);
            }
            drop(job);
            drop(pointer);
            ctx.request_repaint();
        });
        // When the send fails, the work is dropped, and the busy pointer goes with it.
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
            // One frame saved over the animation itself would put a still in its place and take
            // the animation's sidecar, marks and stars with it. It is refused, as Save As
            // refuses it. The check is made here in the background, because a file on a share
            // that stalls would hold the window.
            if only.is_some() && crate::images::save::same_file(&path, &to) {
                let e = tr("choose another name: the image itself is not changed");
                return Some(tf!("Could not save the image: {e}", e));
            }
            // The name's own bytes, also when they are not UTF-8.
            let named = |more: &str| crate::images::save::stem_and(&path, more);
            // The frames go into a folder made new for them. A name that is taken, also by a
            // folder another export made a moment ago or by a link, is passed over, so two
            // exports never write into the same folder.
            let (made, folder) = match only {
                Some(_) => (Ok(()), to.clone()),
                None => (1..)
                    .map(|n| match n {
                        1 => to.join(named("-frames")),
                        n => to.join(named(&format!("-frames-{n}"))),
                    })
                    .map(|p| (std::fs::create_dir(&p), p))
                    .find(|(made, _)| {
                        !(made.as_ref())
                            .is_err_and(|e| e.kind() == std::io::ErrorKind::AlreadyExists)
                    })
                    .expect("the names never run out"),
            };
            let saved = if made.is_err() {
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
                        // The frame is written whole or not at all, also over a file the dialog
                        // let it replace. That file's sidecar is not the frame's, so a frame is
                        // saved without one. Every frame goes into the new folder, where it never
                        // replaces a file.
                        let mut png = std::io::Cursor::new(Vec::new());
                        (img.write_to(&mut png, image::ImageFormat::Png))
                            .map_err(|e| e.to_string())
                            .and_then(|_| {
                                let png = png.into_inner();
                                let replace = only.is_some();
                                crate::images::save::land(&path, &file, &png, replace, true)
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
                (Ok(0), _) => tr("No such frame").to_owned(),
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

    /// Copies Glimt's own folders (settings, marks and stars, collections, keyword tree,
    /// plugins, decoders) to a new folder glimt-backup-DATE in `dir`, in the background. To
    /// restore them, copy them back while Glimt is closed. The README says where.
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

    /// Copy or Move. It copies or moves the targets to a folder that the file chooser asks
    /// for.
    pub(crate) fn pick_copy_or_move(&mut self, ctx: &egui::Context, action: Action) {
        let paths = self.targets().iter().map(|&i| self.path(i)).collect();
        let purpose = match action {
            Action::Copy => Pick::CopyTo(paths),
            _ => Pick::MoveTo(paths),
        };
        self.pick(ctx, true, purpose);
    }

    /// File → Set as Wallpaper. It sets the shown image through the desktop portal, in the
    /// background.
    pub(crate) fn set_wallpaper(&mut self) {
        let (path, tx) = (self.path(self.list.cur()), self.notices_tx.clone());
        std::thread::spawn(move || {
            if let Err(e) = external::set_wallpaper(&path) {
                let _ = tx.send(tf!("Could not set the wallpaper: {e}", e));
            }
        });
    }

    /// Copy Path. It copies the targets' paths as text, quoted for Copy Path and not quoted for
    /// Copy Path Unquoted.
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

    /// Rename. It renames the one file selected, as Delete and Move take it (right-clicked in
    /// the list, say), not the image shown beside it. Only its name is asked for, without the
    /// folders that a list with subfolders gives it.
    pub(crate) fn start_rename(&mut self, ctx: &egui::Context) {
        // The one file selected, as Delete and Move take it (right-clicked in the list,
        // say), not the image shown beside it.
        if let Some(&i) = self.list.selected().first() {
            self.go(ctx, i);
        }
        // Only the name, because in the list with subfolders the entry has its folders too.
        if let Some(name) = self
            .list
            .files()
            .get(self.list.cur())
            .and_then(|f| Path::new(f).file_name())
        {
            self.dialogs
                .ask(Prompt::Rename, name.to_string_lossy().into_owned());
            self.ask_about_targets();
        }
    }
}

/// Puts the entries `removed` from a collection back into `all`, which is what is left, each at
/// the place it had then. It does this in one pass, also for thousands. An entry that is there
/// again is not added twice.
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

/// Takes the files or folders `gone` for good out of the `collection` shown. This happens only
/// once they are gone, in the thread that knows, so the file keeps its order and never lacks a
/// file that is still there. It is the one place for Delete and Move Folder to Trash. A plain
/// list is the user's own and is never written. Returns what it took out, each entry with its
/// place, for Undo to put back.
fn forget_in_collection(
    collection: Option<&Path>,
    gone: &[PathBuf],
) -> Result<Vec<(usize, PathBuf)>, String> {
    let Some(file) = collection.filter(|_| !gone.is_empty()) else {
        return Ok(Vec::new());
    };
    // Each entry and the folders above it are looked up, rather than every gone file for each
    // entry, because 10,000 deleted from a collection of 50,000 were half a billion
    // comparisons.
    let gone: HashSet<&Path> = gone.iter().map(PathBuf::as_path).collect();
    let mut removed = Vec::new();
    let edit = collections::edit_told(file, |all| {
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
    edit.map(|()| removed)
}

/// Returns the files of `paths` that are still there on a network or cloud mount, with the
/// mount's type, after the trash refused them. Returns `None` when there are none, such as when
/// the trash failed in a local folder that cannot be written. `mount_type` gives a folder's file
/// system, and it is asked once for each folder.
fn left_by_trash(
    paths: &[PathBuf],
    mount_type: impl Fn(&Path) -> Option<String>,
) -> Option<(String, Vec<PathBuf>)> {
    use crate::files::file_io::is_remote;
    let mut kinds: HashMap<PathBuf, Option<String>> = HashMap::new();
    let mut remote = |p: &Path| {
        let dir = p.parent()?.to_path_buf();
        (kinds.entry(dir))
            .or_insert_with_key(|dir| mount_type(dir).filter(|k| is_remote(k)))
            .clone()
    };
    let left: Vec<PathBuf> = (paths.iter())
        .filter(|p| std::fs::symlink_metadata(p).is_ok() && remote(p).is_some())
        .cloned()
        .collect();
    Some((remote(left.first()?)?, left))
}

/// Returns the type of the file system that `dir` is on, as `file_io::mount_type` does. A test
/// can set it instead, because no test can mount a FAT card.
fn fs_type(dir: &Path) -> Option<String> {
    #[cfg(test)]
    if let Some(kind) = TEST_MOUNT.with_borrow(Clone::clone) {
        return Some(kind);
    }
    crate::files::file_io::mount_type(dir)
}

/// Returns the type of the file system that `dir` is on when it is FAT or exFAT, as on a camera
/// card. Returns `None` on any other file system.
pub(crate) fn fat_type(dir: &Path) -> Option<String> {
    fs_type(dir).filter(|kind| is_fat(kind))
}

/// Returns whether `kind`, a type of file system from `mount_type`, is FAT or exFAT.
fn is_fat(kind: &str) -> bool {
    matches!(kind, "vfat" | "msdos" | "exfat")
}

/// Finds a name of `to` with a character that the FAT or exFAT file system of its folder
/// refuses, before anything is renamed, copied or moved. Returns that name with the type of its
/// file system, or `None` when every name is allowed where it goes. `mount_type` gives a
/// folder's file system. Only the folder of a name with such a character is looked up, and each
/// folder once, because this runs in the window's thread and every lookup reads the mount table.
fn refused_on_fat(
    to: &[PathBuf],
    mount_type: impl Fn(&Path) -> Option<String>,
) -> Option<(String, String)> {
    let mut kinds: HashMap<&Path, Option<String>> = HashMap::new();
    to.iter().find_map(|path| {
        let name = path.file_name()?.to_string_lossy();
        if !name.contains(NOT_ON_FAT) {
            return None;
        }
        let dir = path.parent()?;
        let kind = kinds
            .entry(dir)
            .or_insert_with(|| mount_type(dir))
            .clone()?;
        is_fat(&kind).then(|| (name.into_owned(), kind))
    })
}

/// Returns the notice for a rename that `refused_on_fat` refused, naming the file and its file
/// system.
fn not_renamed((name, kind): (String, String)) -> String {
    tf!(
        "\"{name}\": a {kind} file system, as on a camera card, allows none of : ? * \" < > | \\ in a name. Nothing was renamed.",
        name,
        kind
    )
}

/// Returns a notice when a file of `paths` has a name that the FAT or exFAT file system of
/// `dest` refuses, so that a copy (`keep`) or a move into it refuses all before it starts.
/// Returns `None` when every name is allowed there. Only the names with such a character are
/// joined to `dest`, so a copy of thousands of plain names makes no new paths.
fn refused_transfer(
    paths: &[PathBuf],
    dest: &Path,
    keep: bool,
    mount_type: impl Fn(&Path) -> Option<String>,
) -> Option<String> {
    let odd: Vec<PathBuf> = (paths.iter().filter_map(|p| p.file_name()))
        .filter(|n| n.to_string_lossy().contains(NOT_ON_FAT))
        .map(|n| dest.join(n))
        .collect();
    let (name, kind) = refused_on_fat(&odd, mount_type)?;
    Some(match keep {
        true => tf!(
            "\"{name}\": a {kind} file system, as on a camera card, allows none of : ? * \" < > | \\ in a name. Nothing was copied.",
            name,
            kind
        ),
        false => tf!(
            "\"{name}\": a {kind} file system, as on a camera card, allows none of : ? * \" < > | \\ in a name. Nothing was moved.",
            name,
            kind
        ),
    })
}

/// The name in the way of a file going to `to`, if any. It is `to` itself, or a sidecar at the
/// new name that belongs to another file, whether or not the file has one of its own. (The
/// photo would show that file's keywords and stars, and leave its own behind.) `ours` are the
/// operation's own names, which a swap reuses. This is the one rule for Copy, Move, Rename and
/// Rename Many.
pub(crate) fn taken(to: &Path, ours: &HashSet<&PathBuf>) -> Option<PathBuf> {
    // A name counts as taken also when it is a link that points nowhere, because nothing may be
    // made in its place.
    let there = |p: &PathBuf| std::fs::symlink_metadata(p).is_ok();
    [to.to_path_buf(), xmp::sidecar(to)]
        .into_iter()
        .find(|p| there(p) && !ours.contains(p))
}

/// Renames `from` to `to` with their sidecars, all or none. It is refused when a new name is
/// another file's. When names swap, it goes through temporary names. The plan is on disk while
/// it runs, and a failure is undone. It never replaces a file, also not one that another
/// program makes meanwhile.
pub(crate) fn rename_files(from: Vec<PathBuf>, to: Vec<PathBuf>, job: &Job) -> Result<(), String> {
    // The sidecars are held as a sidecar write holds them, from before the look at which files
    // have one. Otherwise a keyword written meanwhile would land on an old name.
    let _held = xmp::held_all(&from);
    // Sidecars go with their files, through the same steps.
    let (mut from, mut to) = (from, to);
    let sides: Vec<(PathBuf, PathBuf)> = (from.iter().zip(&to))
        .filter_map(|(f, t)| Some((xmp::sidecar_of(f)?, xmp::sidecar(t))))
        .collect();
    for (f, t) in sides {
        from.push(f);
        to.push(t);
    }
    let sources: HashSet<&PathBuf> = from.iter().collect();
    let mut in_the_way = to.iter().filter_map(|t| taken(t, &sources)).peekable();
    let case_only = in_the_way.peek().is_some();
    if case_only {
        ours_but_case(&from, in_the_way, names_in)?;
    }
    let swap = case_only || to.iter().any(|t| sources.contains(t));
    // The temporary name is next to the file, because in a collection the files are in
    // different folders.
    let tmp = |f: &Path, k: usize| {
        // It has this Glimt's name, because Flatpak windows can share a process ID.
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
    // The plan is on disk while the renames run, so a crash halfway is put right at the next
    // start.
    let mut plan = journal::dir().and_then(|d| journal::begin(&d, &moves));
    for (k, (a, b)) in moves.iter().enumerate() {
        // On Stop, the names done so far go back, by the same steps a failure takes.
        let step = match job.stopped() {
            true => Err(std::io::Error::other(tr("stopped"))),
            false => journal::step(a, b),
        };
        if let Err(e) = step {
            // Named by the file and its new name, not a temporary one.
            let j = k % from.len();
            let name = |p: &PathBuf| p.file_name().unwrap_or_default().display().to_string();
            // The renames are undone so that no file is left under a hidden temporary name,
            // by the same steps the next start would take.
            let (_, stuck) = journal::undo(&moves, k);
            let (f, t) = (name(&from[j]), name(&to[j]));
            // When not all were put back (a share that stopped answering, say), the plan stays,
            // and the next start offers to put the files right.
            if stuck > 0 {
                // With a plan kept, the next start offers the repair. Without one (no state
                // folder, or a full disk), the error tells where the files are, under hidden
                // names.
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
            // When every file is under its hidden name, the disk is synced. Once the swap is
            // done, the names on disk are those of the start again, and the plan could not
            // tell a power cut after the swap from one before it.
            if swap && k + 1 == moves.len() / 2 {
                plan.sync();
            }
        }
        // A file is counted when it lands under its name. That is the later half of a swap's
        // steps, and the sidecars do not count.
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

/// Moves the files of an Undo of a move back to where they came from, each with its sidecar.
/// A file whose place another file has taken since the move is left where it is. Returns the
/// pairs moved back, `(now, before)`, and the errors to tell.
fn move_back(pairs: Vec<Moved>) -> (Vec<(PathBuf, PathBuf)>, Vec<String>) {
    let (mut errors, mut back) = (Vec::new(), Vec::new());
    for (from, to, id) in pairs {
        let _held = xmp::held(&to);
        // Another program may have put another file at `to` since the move.
        if id.is_some_and(|id| !file_id(&to).is_some_and(|now| same_id(id, now))) {
            let other = tr("another file has taken its place");
            errors.push(format!("{}: {other}", to.display()));
            continue;
        }
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
    (back, errors)
}

/// Returns a file just moved from `from` to `to` with its id, for Undo.
fn moved((from, to): &(PathBuf, PathBuf)) -> Moved {
    (from.clone(), to.clone(), file_id(to))
}

/// Returns the id of the file or link at `path`, or `None` when nothing is there.
fn file_id(path: &Path) -> Option<FileId> {
    use std::os::unix::fs::MetadataExt;
    (std::fs::symlink_metadata(path).ok())
        .map(|m| (m.dev(), m.ino(), m.len(), (m.mtime(), m.mtime_nsec())))
}

/// Whether ids `a` and `b` are of the same file. It is so on the same device when the inode is
/// the same, or when the size and modification time are, as an inode may change on FAT.
fn same_id(a: FileId, b: FileId) -> bool {
    a.0 == b.0 && (a.1 == b.1 || (a.2, a.3) == (b.2, b.3))
}

/// Checks that every name in the way of a rename is one of the files being renamed, under a
/// name that differs only in case. On a file system that ignores case (FAT and exFAT on a camera
/// card), IMG.JPG is also found as img.jpg. Such a name is freed by the step through hidden
/// names. A hard link under another name is the same file too, but a name of its own, which a
/// rename must not take away. On a disk that keeps case, img.jpg and IMG.JPG can be two links of
/// one file, so a name counts as ours only when `listed` (the names in its folder) does not hold
/// it as it is written. Returns the error to tell for the first other name. `from` holds the
/// sidecars being renamed too, so their names count as ours.
fn ours_but_case(
    from: &[PathBuf],
    in_the_way: impl Iterator<Item = PathBuf>,
    listed: impl Fn(&Path) -> Option<HashSet<OsString>>,
) -> Result<(), String> {
    let id = file_id;
    let folded = |p: &Path| p.to_string_lossy().to_lowercase();
    // It is built once, so a rename of 20,000 files on a card does not compare every pair.
    let ours: HashSet<(String, FileId)> = from
        .iter()
        .filter_map(|f| Some((folded(f), id(f)?)))
        .collect();
    // Each folder is read once, and only on this rare path. One that cannot be read counts as
    // listing every name, so nothing is taken for ours that might not be.
    let mut folders: HashMap<PathBuf, Option<HashSet<OsString>>> = HashMap::new();
    for at in in_the_way {
        let dir = at.parent().unwrap_or(Path::new("")).to_owned();
        let names = folders.entry(dir).or_insert_with_key(|d| listed(d));
        let own_name = names
            .as_ref()
            .is_none_or(|n| at.file_name().is_none_or(|f| n.contains(f)));
        if own_name || !id(&at).is_some_and(|i| ours.contains(&(folded(&at), i))) {
            return Err(tf!(
                "{name} exists already; nothing renamed",
                name = at.display()
            ));
        }
    }
    Ok(())
}

/// Returns the names in the folder `dir`, or `None` when it cannot be read.
fn names_in(dir: &Path) -> Option<HashSet<OsString>> {
    let entries = std::fs::read_dir(dir).ok()?;
    Some(entries.flatten().map(|e| e.file_name()).collect())
}

/// The thread for this window's sidecar writes, so keywords, comments, stars and imports land
/// in the order they were made.
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

/// Where `path` is after the `moved` ones have moved. It has moved itself, or it is in a folder
/// that has.
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

    /// A name with a character that FAT refuses is refused before anything is renamed on a FAT
    /// or exFAT folder, and allowed on another file system. Only the folder of such a name is
    /// looked up, so a rename to plain names reads no mount table.
    #[test]
    fn refuses_names_that_fat_does_not_allow_before_renaming() {
        use std::path::{Path, PathBuf};
        let to = |names: &[&str]| -> Vec<PathBuf> {
            names.iter().map(|n| Path::new("/card").join(n)).collect()
        };
        let on = |kind: &'static str| move |_: &Path| Some(kind.to_owned());
        let told = super::refused_on_fat(&to(&["a.jpg", "12:30.jpg"]), on("vfat")).unwrap();
        assert_eq!(told, ("12:30.jpg".into(), "vfat".into()));
        let told = super::not_renamed(told);
        assert!(
            told.contains("\"12:30.jpg\"") && told.contains("vfat"),
            "{told}"
        );
        assert!(super::refused_on_fat(&to(&["a?.jpg"]), on("exfat")).is_some());
        assert!(super::refused_on_fat(&to(&["back\\slash.jpg"]), on("msdos")).is_some());
        assert!(super::refused_on_fat(&to(&["12:30.jpg"]), on("ext4")).is_none());
        let looked = std::cell::Cell::new(0);
        let counted = |_: &Path| {
            looked.set(looked.get() + 1);
            Some("vfat".to_owned())
        };
        assert!(super::refused_on_fat(&to(&["a.jpg", "b c.jpg"]), counted).is_none());
        assert_eq!(looked.get(), 0);
        // A Rename Many to times of day on ext4 looks the folder up once, not once a name.
        let times: Vec<String> = (0..1000).map(|i| format!("12:{i}.jpg")).collect();
        let times: Vec<&str> = times.iter().map(String::as_str).collect();
        let on_ext4 = |_: &Path| {
            looked.set(looked.get() + 1);
            Some("ext4".to_owned())
        };
        assert!(super::refused_on_fat(&to(&times), on_ext4).is_none());
        assert_eq!(looked.get(), 1);
    }

    /// A Copy To or Move To onto a FAT card, where one file's name has a character the card
    /// refuses, says so before the job starts and leaves every file where it was. On another
    /// file system the same copy goes ahead.
    #[test]
    fn a_copy_or_move_onto_fat_with_a_refused_name_starts_nothing() {
        let d = std::env::temp_dir().join(format!("glimt-test-fat-copy-{}", std::process::id()));
        std::fs::create_dir_all(d.join("card")).unwrap();
        for f in ["a.png", "12:30.png"] {
            image::RgbImage::new(2, 2).save(d.join(f)).unwrap();
        }
        let (mut app, ctx) = app_on(&d);
        let files = vec![d.join("a.png"), d.join("12:30.png")];
        super::TEST_MOUNT.set(Some("vfat".into()));
        for keep in [true, false] {
            app.notice = None;
            app.transfer(&ctx, files.clone(), d.join("card"), keep);
            let told = app.notice.clone().unwrap_or_default();
            assert!(
                told.contains("\"12:30.png\"") && told.contains("vfat"),
                "{told}"
            );
            assert!(settle(&mut app, &ctx, |a| !a.working()));
            assert!(!d.join("card/a.png").exists() && d.join("a.png").exists());
        }
        super::TEST_MOUNT.set(Some("ext4".into()));
        app.transfer(&ctx, files, d.join("card"), true);
        assert!(settle(&mut app, &ctx, |_| d
            .join("card/12:30.png")
            .exists()));
        assert!(settle(&mut app, &ctx, |a| !a.working()));
        super::TEST_MOUNT.set(None);
        std::fs::remove_dir_all(d).unwrap();
    }

    /// A folder on a FAT card renamed to a name with a character the card refuses stays as it
    /// was, and the notice says why before anything is tried.
    #[test]
    fn a_folder_rename_on_fat_with_a_refused_name_is_told_first() {
        let d = std::env::temp_dir().join(format!("glimt-test-fat-dir-{}", std::process::id()));
        std::fs::create_dir_all(d.join("sub")).unwrap();
        let (mut app, ctx) = app_on(&d);
        super::TEST_MOUNT.set(Some("exfat".into()));
        app.dialogs.act_on_folder(d.join("sub"));
        app.rename_folder(&ctx, "12:30");
        super::TEST_MOUNT.set(None);
        let told = app.notice.clone().unwrap_or_default();
        assert!(
            told.contains("\"12:30\"") && told.contains("exfat"),
            "{told}"
        );
        assert!(settle(&mut app, &ctx, |a| !a.working()));
        assert!(d.join("sub").is_dir() && !d.join("12:30").exists());
        std::fs::remove_dir_all(d).unwrap();
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
        // This also holds for a file without a sidecar of its own, because it would show c's
        // keywords.
        std::fs::write(p("d.jpg"), "d").unwrap();
        assert!(super::rename_files(vec![p("d.jpg")], vec![p("c.jpg")], &job).is_err());
        // A sidecar that is a link pointing nowhere goes with its file too, not left behind for
        // the next file of the old name.
        crate::files::file_io::create_file(&p("f.jpg"), "f").unwrap();
        std::os::unix::fs::symlink(p("nowhere"), p("f.jpg.xmp")).unwrap();
        super::rename_files(vec![p("f.jpg")], vec![p("g.jpg")], &job).unwrap();
        assert!(std::fs::symlink_metadata(p("f.jpg.xmp")).is_err());
        assert!(crate::metadata::xmp::linked(&p("g.jpg.xmp")));
        // A new name that is the file itself, as G.JPG is g.jpg on a camera card, is not taken,
        // because the card's folder lists only g.jpg. A hard link of that name stands in for
        // the card here, with the listing a card gives.
        std::fs::hard_link(p("g.jpg"), p("G.JPG")).unwrap();
        let card = |_: &std::path::Path| {
            Some(std::collections::HashSet::from([std::ffi::OsString::from(
                "g.jpg",
            )]))
        };
        assert!(super::ours_but_case(&[p("g.jpg")], [p("G.JPG")].into_iter(), card).is_ok());
        // On a disk that keeps case, the folder lists both, so G.JPG is a name of its own: the
        // rename is refused, and both names stay.
        assert!(super::rename_files(vec![p("g.jpg")], vec![p("G.JPG")], &job).is_err());
        assert!(read("g.jpg") == "f" && read("G.JPG") == "f");
        std::fs::remove_file(p("G.JPG")).unwrap();
        // A hard link under another name is a name of its own: renaming onto it is refused,
        // and both names stay.
        std::fs::hard_link(p("g.jpg"), p("h.jpg")).unwrap();
        assert!(super::rename_files(vec![p("g.jpg")], vec![p("h.jpg")], &job).is_err());
        assert!(p("g.jpg").exists() && p("h.jpg").exists());
        std::fs::remove_file(p("h.jpg")).unwrap();
        // Stopped before the first step, nothing is renamed, and the error says it stopped.
        job.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let stopped = super::rename_files(vec![p("d.jpg")], vec![p("e.jpg")], &job);
        assert!(stopped.unwrap_err().contains("stopped") && p("d.jpg").exists());
        assert!(p("d.jpg").exists() && !p("c.jpg").exists());
        // The one rule, by name. The file in the way comes first, then the other file's
        // sidecar, and the operation's own names never count.
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

    /// The stars of `path` in the window's marks. There are none without marks built in.
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

    /// Work on a file holds back every other operation on it, also work started outside a job
    /// (Undo, Save As, Crop). Otherwise Undo could run in the middle of a Rename Many, and a
    /// delete in the middle of an Undo. A refused Undo stays to be done again.
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
        assert_eq!(app.list.files().len(), 2);
        app.work.undo = Some(super::Undo::Move(vec![(
            a.clone(),
            dir.join("c.png"),
            None,
        )]));
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
        // Path against path would take minutes. Five seconds leaves room for a machine busy
        // with other builds, and for a debug build, where one second failed.
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
        drop(held);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Every file operation brings along what follows a file (its sidecar, its stars, its entry
    /// in the collection shown), and it waits while other work changes the file. These are the
    /// classes of bugs that came back release after release, held for every operation at once.
    /// A new operation joins here, and `every_file_operation_is_in_the_contract` fails until it
    /// does.
    #[test]
    fn file_operations_keep_their_contract() {
        use std::path::Path;
        type Ctx = eframe::egui::Context;
        type Op = fn(&mut crate::App, &Ctx, &Path);
        type Check = fn(&crate::App, &Path) -> bool;
        // Each operation is run on a.png (and b.png), with what must hold afterwards. Each is
        // held back while other work changes its files, except those that change none. A copy
        // only reads them, and keywords never wait, because an edit would be lost. Their job
        // holds back the rest instead.
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
                    app.list.select([0, 1].into_iter().collect());
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
                        && (!cfg!(feature = "marks") || stars(app, &d.join("b.png")) == 3)
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
                    // This waits only when the operation started. When it was held back, it
                    // says so, and there is nothing to wait for.
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
                    // This waits only when the operation started. When it was held back, it
                    // says so, and there is nothing to wait for.
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
                    // The fixture's sidecar only says whose it is. It is not XMP that Glimt
                    // could write over, so it is removed first.
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
            // While other work changes the same files, the operation is held back and nothing
            // moves.
            let (go, wait) = std::sync::mpsc::channel::<()>();
            let held = vec![d.join("a.png"), d.join("b.png")];
            assert!(app.background(&ctx, held, move || {
                let _ = wait.recv();
                None
            }));
            if name != "keywords" {
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

    /// Save Every Frame passes over a name that is taken, also by a link that points nowhere,
    /// and saves the frames into a new folder of their own.
    #[test]
    fn every_frame_goes_into_a_new_folder() {
        let d = contract_folder("every-frame");
        let gif = d.join("g.gif");
        let mut enc = image::codecs::gif::GifEncoder::new(std::fs::File::create(&gif).unwrap());
        for c in [10u8, 200] {
            let pixel = image::Rgba([c, c, c, 255]);
            let frame = image::Frame::new(image::RgbaImage::from_pixel(4, 4, pixel));
            enc.encode_frame(frame).unwrap();
        }
        drop(enc);
        std::os::unix::fs::symlink(d.join("nowhere"), d.join("g-frames")).unwrap();
        let (mut app, ctx) = app_on(&d);
        app.export_frames(&ctx, gif, None, d.clone());
        let saved = |_: &crate::App| {
            ["g-001.png", "g-002.png"]
                .iter()
                .all(|f| d.join("g-frames-2").join(f).exists())
        };
        assert!(settle(&mut app, &ctx, saved));
        assert!(!d.join("nowhere").exists());
        std::fs::remove_dir_all(d).unwrap();
    }

    /// Every operation that starts a job or background work is in the contract above, or says
    /// here why it is not. So a new one cannot leave out what follows its files unseen.
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
            ("background", "the start itself"),
            (
                "plugins_menu",
                "a program of the user's that holds its files; a_hold_holds_its_files_back",
            ),
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
                "add_to_collection",
                "Glimt's own collection file: adds_to_a_collection_in_the_background",
            ),
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
                let starts = [
                    "self.start_job(",
                    "self.start_job_of(",
                    "self.background(",
                    "self.hold(",
                ];
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

    /// Undo of a rename after an empty folder was opened does not crash. The shown file is looked
    /// up by its name, and an empty list has none, where making its path took a place that is not
    /// there.
    #[test]
    fn undo_of_a_rename_in_an_empty_folder() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-undo-empty-{}", std::process::id()));
        let empty = dir.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        image::RgbImage::new(2, 2).save(dir.join("a.png")).unwrap();
        let (mut app, ctx) = app_on(&dir);
        app.work.undo = Some(super::Undo::Rename(
            vec![dir.join("b.png")],
            vec![dir.join("a.png")],
        ));
        app.open(&ctx, empty.clone());
        app.undo(&ctx);
        assert!(settle(&mut app, &ctx, |a| !a.working()));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Undo of a move leaves alone a file that another program put in the moved file's place
    /// meanwhile, and still moves back the others.
    #[test]
    fn undo_of_a_move_leaves_a_file_put_in_its_place() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-undo-swapped-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        for n in ["a.png", "b.png"] {
            image::RgbImage::new(2, 2).save(dir.join(n)).unwrap();
        }
        let (mut app, ctx) = app_on(&dir);
        let (a, b) = (dir.join("a.png"), dir.join("b.png"));
        app.transfer(&ctx, vec![a.clone(), b.clone()], dir.join("sub"), false);
        assert!(settle(&mut app, &ctx, |app| app.work.undo.is_some()
            && !app.working()));
        // A file manager replaces sub/a.png with another file, written beside it first.
        let moved = dir.join("sub/a.png");
        std::fs::write(dir.join("sub/new"), b"another").unwrap();
        std::fs::rename(dir.join("sub/new"), &moved).unwrap();
        app.undo(&ctx);
        assert!(settle(&mut app, &ctx, |app| !app.working() && b.exists()));
        assert!(!a.exists(), "the other file was moved back as a.png");
        assert_eq!(std::fs::read(&moved).unwrap(), b"another");
        // A file Glimt itself wrote anew there (a turn saved) is still the one to move back.
        app.transfer(&ctx, vec![b.clone()], dir.join("sub"), false);
        assert!(settle(&mut app, &ctx, |app| !b.exists() && !app.working()));
        let saved = dir.join("sub/b.png");
        std::fs::copy(&saved, dir.join("sub/new")).unwrap();
        std::fs::rename(dir.join("sub/new"), &saved).unwrap();
        app.written_anew(std::slice::from_ref(&saved));
        app.undo(&ctx);
        assert!(settle(&mut app, &ctx, |app| !app.working() && b.exists()));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Undo of a move still moves back the moved file when the file system gave it another
    /// inode since, as FAT and exFAT do once the kernel let go of it.
    #[test]
    fn undo_of_a_move_takes_a_new_inode_of_the_same_file() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-undo-inode-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let a = dir.join("a.png");
        image::RgbImage::new(2, 2).save(&a).unwrap();
        let (mut app, ctx) = app_on(&dir);
        app.transfer(&ctx, vec![a.clone()], dir.join("sub"), false);
        assert!(settle(&mut app, &ctx, |app| app.work.undo.is_some()
            && !app.working()));
        // A copy with the same size and time in its place stands for the inode that changed.
        let moved = dir.join("sub/a.png");
        let time = std::fs::metadata(&moved).unwrap().modified().unwrap();
        std::fs::copy(&moved, dir.join("sub/new")).unwrap();
        let copy = std::fs::File::options()
            .write(true)
            .open(dir.join("sub/new"))
            .unwrap();
        copy.set_modified(time).unwrap();
        std::fs::rename(dir.join("sub/new"), &moved).unwrap();
        app.undo(&ctx);
        assert!(settle(&mut app, &ctx, |app| !app.working() && a.exists()));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A turn saved through another spelling of the moved file's path, as in a folder opened
    /// through a link, still keeps that file as the one Undo moves back.
    #[test]
    fn undo_of_a_move_follows_a_turn_saved_through_a_link() {
        let dir = std::env::temp_dir().join(format!("glimt-test-undo-link-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("real/sub")).unwrap();
        std::os::unix::fs::symlink(dir.join("real"), dir.join("link")).unwrap();
        let a = dir.join("real/a.png");
        image::RgbImage::new(2, 2).save(&a).unwrap();
        let (mut app, ctx) = app_on(&dir.join("real"));
        app.transfer(&ctx, vec![a.clone()], dir.join("real/sub"), false);
        assert!(settle(&mut app, &ctx, |app| app.work.undo.is_some()
            && !app.working()));
        // The turn is saved as a new file put in the old one's place, under the link's name.
        let saved = dir.join("link/sub/a.png");
        std::fs::copy(&saved, dir.join("real/sub/new")).unwrap();
        std::fs::rename(dir.join("real/sub/new"), &saved).unwrap();
        app.written_anew(std::slice::from_ref(&saved));
        app.undo(&ctx);
        assert!(settle(&mut app, &ctx, |app| !app.working() && a.exists()));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Undo of a move leaves alone the moved file once Save As wrote another image over it,
    /// because that image is not the file that was moved.
    #[test]
    fn undo_of_a_move_leaves_another_image_saved_over_it() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-undo-saved-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let a = dir.join("a.png");
        image::RgbImage::new(2, 2).save(&a).unwrap();
        let (mut app, ctx) = app_on(&dir);
        app.transfer(&ctx, vec![a.clone()], dir.join("sub"), false);
        assert!(settle(&mut app, &ctx, |app| app.work.undo.is_some()
            && !app.working()));
        // Save As writes another image beside sub/a.png and renames it over, then tells so.
        let moved = dir.join("sub/a.png");
        image::RgbImage::new(3, 3)
            .save(dir.join("sub/new.png"))
            .unwrap();
        std::fs::rename(dir.join("sub/new.png"), &moved).unwrap();
        let over = super::Deleted {
            overwritten: vec![moved.clone()],
            ..Default::default()
        };
        app.work.deleted_tx.send(over).unwrap();
        app.receive_deleted(&ctx);
        app.undo(&ctx);
        let refused =
            |app: &crate::App| (app.notice.as_deref()).is_some_and(|n| n.contains("Undo failed"));
        assert!(settle(&mut app, &ctx, |app| !app.working() && refused(app)));
        assert!(!a.exists(), "the other image was moved back as a.png");
        assert!(moved.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The question about what the trash refused opens after the first listing taken after the
    /// refusal, also in a folder that changes all the time and is read again and again.
    #[test]
    fn asks_about_what_the_trash_refused_in_a_folder_that_keeps_changing() {
        let dir = std::env::temp_dir().join(format!("glimt-test-busy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbImage::new(2, 2).save(dir.join("a.png")).unwrap();
        let (mut app, ctx) = app_on(&dir);
        app.work.deleting.insert("a.png".into());
        app.remove_indices(&ctx, &[0]);
        let refused = super::Deleted {
            paths: vec![dir.join("a.png")],
            failed: true,
            untrashable: Some(("fuse.rclone".into(), vec![dir.join("a.png")])),
            ..Default::default()
        };
        app.work.deleted_tx.send(refused).unwrap();
        let asked = (0..250).any(|_| {
            app.poll_background(&ctx);
            // Auto refresh sees a change in every frame, as in a sync folder taking uploads.
            app.folder.relist_later();
            let open = app.modal_open();
            if !open {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            open
        });
        assert!(asked);
        assert_eq!(app.asked_targets(), Some(vec![0]));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Of the files the trash refused, only those still there on a network or cloud mount are
    /// left to ask about, with that mount's type. A file that went after all and a file of a
    /// local folder are not. Each folder's mount is looked up once.
    #[test]
    fn leaves_to_ask_about_only_files_still_on_a_network_mount() {
        use std::path::{Path, PathBuf};
        let dir = std::env::temp_dir().join(format!("glimt-test-left-{}", std::process::id()));
        let (cloud, local) = (dir.join("cloud"), dir.join("local"));
        std::fs::create_dir_all(&cloud).unwrap();
        std::fs::create_dir_all(&local).unwrap();
        for file in [
            cloud.join("a.png"),
            cloud.join("b.png"),
            local.join("c.png"),
        ] {
            crate::files::file_io::create_file(&file, "").unwrap();
        }
        let looked = std::cell::Cell::new(0);
        let mount_type = |folder: &Path| {
            looked.set(looked.get() + 1);
            Some(
                if folder == cloud {
                    "fuse.rclone"
                } else {
                    "ext4"
                }
                .to_owned(),
            )
        };
        let paths: Vec<PathBuf> = ["a.png", "gone.png", "b.png"]
            .map(|n| cloud.join(n))
            .into_iter()
            .chain([local.join("c.png")])
            .collect();
        let left = super::left_by_trash(&paths, mount_type);
        let kind = "fuse.rclone".to_owned();
        assert_eq!(
            left,
            Some((kind, vec![cloud.join("a.png"), cloud.join("b.png")]))
        );
        assert_eq!(looked.get(), 2);
        assert_eq!(
            super::left_by_trash(&[local.join("c.png")], mount_type),
            None
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The files the trash refused on a network or cloud mount are asked about, to delete them
    /// for good instead, when they are in the folder shown. Files of another folder are not,
    /// because their names could be other files' in this list, and a notice tells that they
    /// are still there. The question waits until the folder is read again, because the delete
    /// took the files out of the list. A file refused twice is asked about once.
    #[test]
    fn asks_to_delete_for_good_what_the_trash_refused_on_a_network_mount() {
        use std::path::PathBuf;
        let dir =
            std::env::temp_dir().join(format!("glimt-test-untrashable-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbImage::new(2, 2).save(dir.join("a.png")).unwrap();
        let (mut app, ctx) = app_on(&dir);
        let refused = |files: Vec<PathBuf>| super::Deleted {
            untrashable: Some(("fuse.rclone".into(), files)),
            ..Default::default()
        };
        // A refusal comes with the delete failed, which has the folder read again.
        let elsewhere = super::Deleted {
            failed: true,
            ..refused(vec![PathBuf::from("/elsewhere/a.png")])
        };
        app.work.deleted_tx.send(elsewhere).unwrap();
        assert!(settle(&mut app, &ctx, |app| app.notice.is_some()));
        assert!(!app.modal_open());
        let told = app.notice.take().unwrap();
        assert!(told.contains("1 file (fuse.rclone)"), "{told}");
        // The delete takes the file out of the list as it starts, as `delete_now` does.
        app.work.deleting.insert("a.png".into());
        app.remove_indices(&ctx, &[0]);
        let failed = super::Deleted {
            paths: vec![dir.join("a.png")],
            failed: true,
            ..refused(vec![dir.join("a.png")])
        };
        app.work.deleted_tx.send(failed).unwrap();
        // The same file refused again, by a second Delete, is counted once.
        app.work
            .deleted_tx
            .send(refused(vec![dir.join("a.png")]))
            .unwrap();
        assert!(settle(&mut app, &ctx, |app| app.modal_open()));
        assert_eq!(app.dialogs.asked_names(), ["a.png"]);
        assert_eq!(app.asked_targets(), Some(vec![0]));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The files an Undo from the trash could not put back do not replace the Undo of a delete
    /// that finished while it ran, because that one is newer and holds its own files' marks.
    #[test]
    fn an_undo_sent_back_keeps_a_newer_one() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-undo-newer-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.png");
        image::RgbImage::new(2, 2).save(&a).unwrap();
        let (mut app, ctx) = app_on(&dir);
        app.work.undo = Some(super::Undo::Trash(crate::files::trash::Thrown {
            files: vec![a.clone()],
            sidecars: Vec::new(),
            sidecars_left: Vec::new(),
            since: std::time::SystemTime::now(),
            marks: Vec::new(),
            collection: None,
        }));
        app.undo(&ctx);
        app.work.undo = Some(super::Undo::Move(Vec::new()));
        // The thread reports before it tells, so its report is waiting once it told.
        let told = app
            .notices_rx
            .recv_timeout(std::time::Duration::from_secs(5));
        assert!(told.is_ok());
        app.receive_deleted(&ctx);
        assert!(matches!(app.work.undo, Some(super::Undo::Move(_))));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// An Undo of a move to the trash that is refused, because a new file has taken the first
    /// name, stays to be done again with the marks and collection places it holds.
    #[test]
    fn a_refused_undo_from_the_trash_keeps_its_marks() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-undo-taken-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbImage::new(2, 2).save(dir.join("a.png")).unwrap();
        let (a, b) = (dir.join("a.png"), dir.join("b.png"));
        let (mut app, ctx) = app_on(&dir);
        let gqv = dir.join("c.gqv");
        app.work.undo = Some(super::Undo::Trash(crate::files::trash::Thrown {
            files: vec![a.clone(), b.clone()],
            sidecars: Vec::new(),
            sidecars_left: Vec::new(),
            since: std::time::SystemTime::now(),
            marks: vec![(a.clone(), (1, 3))],
            collection: Some((gqv.clone(), vec![(0, a.clone())])),
        }));
        app.undo(&ctx);
        assert!(app.work.undo.is_none());
        assert!(settle(&mut app, &ctx, |a| a.work.undo.is_some()));
        let Some(super::Undo::Trash(thrown)) = &app.work.undo else {
            panic!("the Undo was lost");
        };
        assert_eq!(thrown.files, [a.clone(), b]);
        assert_eq!(thrown.marks, [(a.clone(), (1, 3))]);
        assert_eq!(thrown.collection, Some((gqv, vec![(0, a)])));
        std::fs::remove_dir_all(dir).unwrap();
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
        assert!(settle(&mut app, &ctx, |app| !app.folder.is_listing()));
        waits(&mut app, &c, &|app| {
            let at = (app.list.files().iter())
                .position(|f| f == "c.png")
                .unwrap();
            app.delete_now(&ctx, false, vec![at]);
        });
        assert!(!d.join("c.png.xmp").exists());
        // With many files, they are held a chunk (100) at a time, so a file held up by a
        // sidecar write holds up only its own chunk, and those before it go meanwhile. Here
        // they are deleted for good: a chunk of 100 files in other lock slots, and then two
        // more, of which the second is held. The first of the two waits too, because it is in
        // the same chunk. (There is not one gio call and one taking of the locks per file.)
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
        assert!(settle(&mut app, &ctx, |app| !app.folder.is_listing()));
        let at =
            |app: &crate::App, n: &str| (app.list.files().iter()).position(|f| f == n).unwrap();
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

    /// The shown image renamed in place is decoded again under its new name, also when the list
    /// keeps its order, because forget_names dropped its decoded copy.
    #[test]
    fn a_renamed_shown_image_is_decoded_again() {
        let dir = contract_folder("decode-again");
        let (mut app, ctx) = app_on(&dir);
        let shown = |a: &crate::App| a.list.slots().contains_key(&a.list.cur());
        assert!(settle(&mut app, &ctx, |a| !a.working() && shown(a)));
        let path = app.path(app.list.cur());
        app.forget_names(std::slice::from_ref(&path));
        assert!(!shown(&app));
        app.list(&ctx);
        assert!(settle(&mut app, &ctx, |a| !a.working() && shown(a)));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A file moved, renamed or deleted takes what Glimt keeps of it by name along, so a file of
    /// that name later is read for itself, not shown with the old one's size or Check Images'
    /// mark.
    #[test]
    fn a_name_moved_or_gone_forgets_what_was_kept_of_it() {
        let dir = contract_folder("forget");
        let (mut app, ctx) = app_on(&dir);
        assert!(settle(&mut app, &ctx, |a| !a.working()));
        for name in ["a.png", "b.png", "c.png"] {
            app.list.remember_details(name.into(), (1, 1));
            app.broken_files.insert(name.into());
        }
        app.paths_moved(vec![(dir.join("a.png"), dir.join("c.png"))], false);
        app.forget_names(&[dir.join("b.png")]);
        for name in ["a.png", "b.png", "c.png"] {
            let name = std::ffi::OsStr::new(name);
            assert!(!app.list.details().contains_key(name), "{name:?}");
            assert!(!app.broken_files.contains(name), "{name:?}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A name in the way of a rename counts as taken also when it is a link that points nowhere,
    /// because nothing may be made in its place.
    #[test]
    fn a_dangling_link_is_a_name_taken() {
        let dir = std::env::temp_dir().join(format!("glimt-test-taken-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let to = dir.join("b.jpg");
        std::os::unix::fs::symlink(dir.join("nowhere"), &to).unwrap();
        assert_eq!(super::taken(&to, &Default::default()), Some(to.clone()));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A sidecar that cannot be removed with its photo is told, not left without a word, because
    /// the next file of that name would show its keywords.
    #[test]
    fn a_sidecar_left_by_a_delete_is_told() {
        let dir = std::env::temp_dir().join(format!("glimt-test-side-left-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.png");
        image::RgbImage::new(4, 4).save(&a).unwrap();
        // A folder by the sidecar's name cannot be removed as a file.
        std::fs::create_dir(crate::metadata::xmp::sidecar(&a)).unwrap();
        let (mut app, ctx) = app_on(&dir);
        assert!(settle(&mut app, &ctx, |a| !a.folder.is_listing()));
        app.notice = None;
        app.delete_now(&ctx, false, vec![0]);
        let told =
            |app: &crate::App| (app.notice.as_deref()).is_some_and(|n| n.contains("a.png.xmp"));
        assert!(settle(&mut app, &ctx, told));
        assert!(!a.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A permanent delete takes the photo's sidecar along also when the sidecar is a link that
    /// points nowhere, so no stray link is left under the name for a later file.
    #[test]
    fn a_delete_takes_a_dangling_sidecar_link_along() {
        let dir = std::env::temp_dir().join(format!("glimt-test-dangling-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.png");
        image::RgbImage::new(4, 4).save(&a).unwrap();
        let side = crate::metadata::xmp::sidecar(&a);
        std::os::unix::fs::symlink(dir.join("nowhere"), &side).unwrap();
        let (mut app, ctx) = app_on(&dir);
        app.delete_now(&ctx, false, vec![0]);
        let gone = |p: &std::path::Path| std::fs::symlink_metadata(p).is_err();
        assert!(settle(&mut app, &ctx, |_| gone(&a) && gone(&side)));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A turn is not saved while other work changes the file (Rename Many, a move), because it
    /// could be saved over the photo that takes its name.
    #[test]
    fn a_turn_waits_for_other_work_on_its_file() {
        let dir = std::env::temp_dir().join(format!("glimt-test-turn-held-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.png");
        image::RgbImage::new(4, 4).save(&a).unwrap();
        let (mut app, ctx) = app_on(&dir);
        app.viewing.turn((1, false), a.clone());
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
        assert!(settle(&mut app, &ctx, |a| !a.folder.is_listing()));
        app.run(&ctx, crate::ui::actions::Action::NewFolder);
        // Another folder opened while the dialog is open, by a script, say, does not change
        // where the new folder is made.
        let other = d.join("other");
        std::fs::create_dir_all(&other).unwrap();
        app.open(&ctx, other.clone());
        app.answer_prompt(&ctx, crate::ui::dialogs::Prompt::NewFolder, "trip");
        app.open(&ctx, d.clone());
        assert!(settle(&mut app, &ctx, |a| {
            !a.folder.is_listing() && a.folder.subdirs().iter().any(|s| s.ends_with("trip"))
        }));
        assert!(d.join("trip").is_dir() && !other.join("trip").exists());
        // When the folder is there already, a notice says so, and nothing else is made.
        app.run(&ctx, crate::ui::actions::Action::NewFolder);
        app.answer_prompt(&ctx, crate::ui::dialogs::Prompt::NewFolder, "trip");
        assert!(settle(&mut app, &ctx, |a| a.notice.is_some()));
        std::fs::remove_dir_all(d).unwrap();
    }

    /// What follows a file comes along through the one report of what happened on disk. An
    /// `exif:` search was blind to a renamed file, because the index never read its new name.
    /// A file written over by Save As kept its old marks and stars, though it lost its sidecar.
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
        assert!(app.search.reads_exif_again("a.png"));
        let names = ["a.png", "b.png"].map(|n| (std::ffi::OsString::from(n), String::new()));
        app.search.set_exif_index(names.into_iter().collect());
        // A search while keywords are still being written keeps the marks for later, because
        // the file would be read before its sidecar landed. Once it is written, it is read
        // again.
        let writing = app
            .start_job_of("Saving keywords", 1, true, vec![dir.join("a.png")])
            .unwrap();
        app.search.set_query("exif:x");
        app.update_search(&ctx);
        assert!(app.search.reads_exif_again("a.png"));
        assert!(!app.search.reads_index());
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
        assert!(app.search.has_nothing_to_read_again() && app.search.reads_index());
        app.search.drop_index_reading();
        app.search.clear_query();
        // When another program renamed the file and the folder is read again, the new name is
        // read too.
        std::fs::rename(dir.join("a.png"), dir.join("c.png")).unwrap();
        app.replace_files(&ctx, vec!["b.png".into(), "c.png".into()], None);
        assert!(app.search.reads_exif_again("c.png"));
        // A file written over by another image is read again, and its stars are gone.
        let b = dir.join("b.png");
        #[cfg(feature = "marks")]
        app.marks.rate(std::slice::from_ref(&b), 3);
        let over = super::Deleted {
            overwritten: vec![b.clone()],
            ..Default::default()
        };
        app.work.deleted_tx.send(over).unwrap();
        app.receive_deleted(&ctx);
        assert!(app.search.reads_exif_again("b.png"));
        #[cfg(feature = "marks")]
        assert_eq!(app.marks.get(&b), (0, 0));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The same holds in a collection. Its file follows a rename, a move, a folder renamed and
    /// a delete.
    #[test]
    fn a_collection_follows_its_files() {
        let d = contract_folder("collection");
        let gqv = d.join("col.gqv");
        let (mut app, ctx) = app_on(&gqv);
        let entries = |gqv: &std::path::Path| std::fs::read_to_string(gqv).unwrap();
        // Each step's work has ended too, because the next step would be held back while it
        // still runs.
        let idle =
            |app: &crate::App| (app.work.jobs.iter()).all(|j| std::sync::Arc::strong_count(j) == 1);
        app.rename_in_background(&ctx, vec![d.join("a.png")], vec![d.join("c.png")]);
        assert!(settle(&mut app, &ctx, |app| idle(app)
            && entries(&gqv).contains("c.png")
            && !entries(&gqv).contains("a.png")));
        app.transfer(&ctx, vec![d.join("c.png")], d.join("sub"), false);
        assert!(settle(&mut app, &ctx, |app| idle(app)
            && entries(&gqv).contains("sub/c.png")));
        // When a folder is renamed in the background, its images in the collection follow.
        app.dialogs.act_on_folder(d.join("sub"));
        app.rename_folder(&ctx, "moved");
        assert!(settle(&mut app, &ctx, |app| idle(app)
            && entries(&gqv).contains("moved/c.png")));
        assert!(d.join("moved/c.png").exists());
        let b = app
            .list
            .files()
            .iter()
            .position(|f| std::path::Path::new(f).ends_with("b.png"))
            .unwrap();
        app.delete_now(&ctx, false, vec![b]);
        assert!(settle(&mut app, &ctx, |_| !entries(&gqv).contains("b.png")));
        std::fs::remove_dir_all(d).unwrap();
    }

    /// Files held by `hold`, as a plugin running on them holds them, hold back every other
    /// operation on them until the hold ends.
    #[test]
    fn a_hold_holds_its_files_back() {
        let dir = std::env::temp_dir().join(format!("glimt-test-hold-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.png");
        image::RgbImage::new(2, 2).save(&a).unwrap();
        let (mut app, _ctx) = crate::support::test_support::app_on(&dir);
        let hold = app.hold(vec![a.clone()]).unwrap();
        assert!(!app.none_saving(std::slice::from_ref(&a)));
        assert!(app.hold(vec![a.clone()]).is_none());
        drop(hold);
        assert!(app.none_saving(std::slice::from_ref(&a)));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
