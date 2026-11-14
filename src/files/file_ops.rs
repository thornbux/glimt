//! File operations: deleting or moving to the trash, copying and moving, Rename and Rename
//! Many, saving the orientation, and the jobs that run them in the background with their
//! progress and Stop. A copy is written under a hidden name and gets its own name once it is
//! whole. No file is ever overwritten.

use crate::files::external;
use crate::files::file_io::{
    FileFingerprint, ListedFile, PickedFiles, PickedFingerprints, fingerprint, is_surely_gone,
    is_surely_there, put_file, put_file_until_stopped, rename_never_over, same_fingerprint,
};
use crate::files::trash::{TrashMove, restore, trash_dirs};
use crate::images::decode::catch_panic_as_error;
use crate::images::save::save_rotated;
use crate::model::folder::total_size;
use crate::model::listing::pattern_name;
use crate::support::format::{count, thousands};
use crate::support::lang::tr;
use crate::support::threads::{
    AnswerSender, Receiving, Told, Took, Working, answer_channel, catch_panic,
    lock_even_if_poisoned, told_work,
};
use crate::ui::actions::Action;
use crate::ui::dialogs::Pick;
use crate::ui::dialogs::{Asked, Prompt};
use crate::{
    files::journal, images::decode, metadata::exif, metadata::xmp, model::collections, tf,
};
use eframe::egui;
use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Instant;

/// The characters that FAT and exFAT refuse in a name, as on a camera card.
pub(crate) const NOT_ON_FAT: [char; 8] = [':', '?', '*', '"', '<', '>', '|', '\\'];

/// How long a look at a folder before its job is waited for (`wait_for_look_up_before_the_job`),
/// from its start or from its last sign that it goes on. A share that stopped answering waits for
/// ever, and one that wakes from sleep answers within this. A test waits less for the look-up
/// that it holds back.
#[cfg(not(test))]
const LOOK_UP_GIVEN_UP_AFTER: std::time::Duration = std::time::Duration::from_secs(60);
#[cfg(test)]
const LOOK_UP_GIVEN_UP_AFTER: std::time::Duration = std::time::Duration::from_secs(3);

// The file systems that tests give their folders, as (folder, type), with `test_mount`. No test
// can mount a FAT card. Each test names a folder of its own, so the tests can run side by side,
// and a job's thread sees the same folders as the test that started it.
#[cfg(test)]
static TEST_MOUNTS: std::sync::Mutex<Vec<(PathBuf, String)>> = std::sync::Mutex::new(Vec::new());

// How many real paths `written_anew` looked up on this thread, so that a test can see that it
// looks up only those that may be the files written.
#[cfg(test)]
thread_local! {
    static REAL_PATHS_LOOKED_UP: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// What a look before a job sends `wait_for_look_up_before_the_job`: that it goes on, after each
/// file of a look at many files, or its answer. The wait gives up only when neither has come for
/// `LOOK_UP_GIVEN_UP_AFTER`, so a look at many files on a share that answers is not given up.
pub(crate) enum LookUpStep<T> {
    GoesOn,
    Answered(T),
}

/// A Rename Many running in the background. It holds the files renamed with their new names,
/// and which of them was shown.
pub(crate) struct RenameJob {
    /// The old names, for the marks and a collection's entries to follow.
    from: Vec<PathBuf>,
    to: Vec<PathBuf>,
    /// The deepest folder that holds every file renamed, found once at the start, so that a
    /// frame can tell whether the folder shown holds any of them.
    folder: PathBuf,
    shown: Option<usize>,
    /// Whether this is an Undo giving the old names back. Such a rename is not undone itself.
    undoing: bool,
}

/// What a rename's thread answers: how it went, the places in the job of the files that an
/// Undo left alone because other files have taken their names (in order), and the Undo it
/// leaves. That is the Undo of a rename that went through, or the part that a failed Undo
/// still has to do.
pub(crate) struct RenameOutcome {
    result: Result<(), String>,
    left_alone: Vec<usize>,
    undo: Vec<Moved>,
    /// Whether each file is under a name the rename knows: its new one, or its old one again.
    /// It is false when a failed rename could not put every old name back.
    names_known: bool,
    /// Whether a Stop, also that of a second close, ended the rename with every old name back.
    /// That is told as a stop, not as a failed write that would keep the window open.
    stopped: bool,
}

/// Why `rename_files` failed, and whether every file it renamed has its old name back.
#[derive(Debug)]
pub(crate) struct RenameFailed {
    text: String,
    names_put_back: bool,
    /// Whether a Stop ended it, with every old name back.
    stopped: bool,
}

impl From<String> for RenameFailed {
    /// A failure before the first rename, which leaves every name as it was.
    fn from(text: String) -> Self {
        RenameFailed {
            text,
            names_put_back: true,
            stopped: false,
        }
    }
}

/// What words the end of a sidecar job, from how many files failed (`write_sidecars`).
type SidecarReport = Box<dyn FnOnce(usize, &Job) -> Option<String> + Send>;

/// Writes the sidecar of the file at a place in a sidecar write's list, given by that place and
/// the file's name now (`write_sidecars`). Returns the text of its failure.
type SidecarWrite = Box<dyn Fn(usize, &Path) -> Result<(), String> + Send>;

/// A sidecar write asked for while a rename runs (`write_sidecars`). It is sent once the rename
/// is over, to the names its files have then.
struct WaitingSidecarWrite {
    activity: &'static str,
    files: Vec<ListedFile>,
    write: SidecarWrite,
    report: SidecarReport,
    /// Closing waits for the write while it waits, as for one that was sent.
    _writing: Working,
}

/// What a delete, move, copy or tool reports, in one message. It holds the files it was asked
/// to delete or move, and those that are gone now. The thread finds out which are gone, so the
/// window does not `stat` each one. It also says whether to read the folder again, and holds
/// the files moved or copied `(from, to)`, whose marks follow before the gone ones are
/// forgotten. Last come the Undo it makes possible, the marks an Undo puts back, and the files
/// changed or written over.
///
/// This is the one report of what happened to files on disk. `receive_operation_report` is the one
/// place that brings along everything that follows a file: marks, the collection, a turn not saved,
/// decoded images, thumbnails, sizes and dates, and the `exif:` index. It is one message, so
/// nothing depends on the order in which two messages would arrive.
#[derive(Default)]
pub(crate) struct OperationReport {
    pub(crate) requested: Vec<PathBuf>,
    pub(crate) gone: Vec<PathBuf>,
    pub(crate) any_failed: bool,
    pub(crate) moved: Vec<(PathBuf, PathBuf)>,
    pub(crate) is_copy: bool,
    /// Whether `moved` was read from a record (a plan of Rename Many cut short), not made by
    /// this report's own job. The marks then follow only to a name where a file is now.
    pub(crate) is_moved_from_a_record: bool,
    /// Whether the folder is to be read again, because a tool edited files in it or a repair
    /// renamed them.
    pub(crate) read_folder_again: bool,
    /// What Undo puts back, when this was a move to the trash or a move.
    pub(crate) undo: Option<Undo>,
    /// Whether `undo` is what an Undo from the trash or of a move could not put back. It is kept
    /// only when no operation that finished meanwhile left an Undo of its own, which is newer.
    pub(crate) is_undo_remainder: bool,
    /// The marks and stars to put back, for files that Undo brought back from the trash.
    pub(crate) restored_marks: Vec<(PathBuf, (u8, u8))>,
    /// The files a tool changed in place. What is shown of them is read again. (A turn that
    /// was saved comes through `saved_rx` into the same `files_changed`, and that also ends its
    /// guard.)
    pub(crate) changed: Vec<PathBuf>,
    /// The files written over by another image (Save As, or Save This Frame onto a file there).
    /// They are read again, and their marks and stars go, as their sidecar did.
    pub(crate) overwritten: Vec<PathBuf>,
    /// A folder renamed (old, new), which the window follows.
    pub(crate) folder_renamed: Option<(PathBuf, PathBuf)>,
    /// A collection's file renamed (old, new), which the window follows when it shows it.
    pub(crate) collection_renamed: Option<(PathBuf, PathBuf)>,
    /// A folder made by New Folder. The tree reads its parent again, and the list reads its
    /// folder again.
    pub(crate) folder_made: Option<PathBuf>,
    /// The type of a network or cloud mount whose files the trash refused, and those files,
    /// which are still there, each with the fingerprint it had when it was picked. The window
    /// asks whether to delete them for good instead.
    pub(crate) untrashable: Option<(String, Vec<PickedAs>)>,
    /// The log of a Move, removed once the marks and collections have followed `moved`. One
    /// found at the start comes with the files that went before Glimt was killed.
    pub(crate) move_log: Option<journal::MoveLog>,
    /// The log of a delete, removed once the marks and stars of `gone` are forgotten. One found
    /// at the start comes with the files that went before Glimt was killed.
    pub(crate) delete_log: Option<journal::DeleteLog>,
    /// The plans of Rename Many cut short that a repair could not put right, with the list of
    /// their files not under the names they were to get (`journal::names_left`). They are asked
    /// about again at once, with the way out of leaving the names as they are.
    pub(crate) cut_short_left: Option<(Vec<journal::CutShort>, String)>,
}

/// A file moved or renamed, `(from, to, fingerprint)`, as Undo puts it back. The fingerprint is
/// the file's at `to`. When Glimt itself writes the file anew there (a turn saved), it takes
/// the new fingerprint.
pub(crate) type Moved = (PathBuf, PathBuf, FileFingerprint);

/// What an Undo of a move did: the pairs moved back as `(now, before)`, the files left to be
/// undone again, and the errors to tell.
type MovedBack = (Vec<(PathBuf, PathBuf)>, Vec<Moved>, Vec<String>);

/// What waits for a look at a folder before its job (its real path, its file system), which
/// `App::wait_for_look_up_before_the_job` waits for in the background. It runs in the window's
/// thread once the look has answered or been given up.
pub(crate) type WithRealFolder = Box<dyn FnOnce(&mut crate::App, &egui::Context) + Send>;

/// A file saved in the background (a turned image, or a tool's file), and how it went.
type SavedFile = (PathBuf, Result<(), String>);

/// A file with the fingerprint it had when it was picked.
type PickedAs = (PathBuf, FileFingerprint);

/// What Edit → Undo puts back. It is the last files or folder moved to the trash, the last
/// files moved, or the last files renamed, each `(from, to, fingerprint)`. The fingerprint
/// is the file's `FileFingerprint` at `to`, so an Undo leaves alone a file that another program put
/// there since.
pub(crate) enum Undo {
    Trash(TrashMove),
    Move(Vec<Moved>),
    Rename(Vec<Moved>),
}

/// An Undo taken out of `FileOperations::undo` for a job that has not started yet. When it is
/// dropped before `started` is called, because the job was refused or the look before it was
/// given up, it goes back as a remainder. So Ctrl+Z can try it again, and does not say that
/// there is nothing to undo.
struct UndoUntilStarted {
    undo: Option<Undo>,
    report_tx: AnswerSender<OperationReport>,
}

impl UndoUntilStarted {
    /// Lets the Undo go, because the job that does it has started.
    fn started(mut self) {
        self.undo = None;
    }
}

impl Drop for UndoUntilStarted {
    fn drop(&mut self) {
        if let Some(undo) = self.undo.take() {
            // The send fails only when the window has closed, and the Undo goes with it.
            let _ = self.report_tx.send(OperationReport {
                undo: Some(undo),
                is_undo_remainder: true,
                ..Default::default()
            });
        }
    }
}

/// Files held back for work that is not a job (`App::hold_back_while_writing`). Other
/// operations on them wait while it lives. The work moves it into its thread, so the files are
/// let go when the work ends, also when the thread panics.
pub(crate) struct HeldBackFiles {
    /// The files, counted by `is_changed_by_running_work` while this lives.
    paths: Arc<Mutex<Vec<PathBuf>>>,
}

impl HeldBackFiles {
    /// Lets go of every held file but `files`, once the work knows that it changes only those.
    /// A tool that takes only RAW files then leaves the JPEGs beside them free to change.
    pub(crate) fn hold_only(&self, files: &[PathBuf]) {
        *lock_even_if_poisoned(&self.paths) = files.to_vec();
    }
}

/// A file operation running in the background (copy, move, delete, keywords, a Lightroom
/// import). The status bar shows how far it is, and its Stop takes effect between files.
pub(crate) struct Job {
    /// "Copying", "Deleting" …, before "120 of 3,000".
    pub(crate) activity: &'static str,
    pub(crate) total: usize,
    pub(crate) done: AtomicUsize,
    pub(crate) stop_requested: std::sync::atomic::AtomicBool,
    pub(crate) started: Instant,
    /// Every job writes files, so closing waits for it as long as its thread holds the job.
    _writing: Working,
    /// Whether the job writes the sidecars of its files. Those files must not be moved, renamed
    /// or deleted while it runs.
    pub(crate) writes_sidecars: bool,
    /// The files and folders the job changes. No other operation starts on them while it runs.
    pub(crate) held_back_files: Vec<PathBuf>,
}

impl Job {
    /// A job on `total` files, holding the writing count until it is dropped.
    pub(crate) fn new(
        activity: &'static str,
        total: usize,
        writes_sidecars: bool,
        held_back_files: Vec<PathBuf>,
    ) -> Job {
        Job {
            activity,
            total,
            done: AtomicUsize::new(0),
            stop_requested: Default::default(),
            started: Instant::now(),
            _writing: Working::new(&crate::files::file_io::WRITING),
            writes_sidecars,
            held_back_files,
        }
    }

    /// Whether Stop was pressed.
    pub(crate) fn is_stopped(&self) -> bool {
        self.stop_requested.load(Ordering::Relaxed)
    }

    pub(crate) fn count_done(&self, n: usize) {
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
        if self.is_stopped() {
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
pub(crate) struct FileOperations {
    /// The file operations running, for the status bar. A job is over when its thread has let
    /// go of it.
    jobs: Vec<Arc<Job>>,
    /// The files that `run_in_background` work changes, held back (`HeldBackFiles`) by its thread
    /// as long as it runs.
    held_back_files: Vec<Arc<Mutex<Vec<PathBuf>>>>,
    /// A Rename Many running in the background.
    renaming: Option<RenameJob>,
    renaming_rx: Receiving<(RenameOutcome, Working)>,
    /// The sidecar writes asked for while `renaming` runs, in the order they were asked for.
    sidecar_writes_waiting: Vec<WaitingSidecarWrite>,
    /// The names being deleted in the background. They are kept out of refreshed listings
    /// until the delete is done.
    deleting: HashSet<OsString>,
    /// What deletes, moves and copies report (`OperationReport`). A tool and Move Folder to Trash
    /// send it too, only for the list to be read again.
    report_tx: AnswerSender<OperationReport>,
    report_rx: mpsc::Receiver<(OperationReport, Working)>,
    /// Those still being saved.
    saving: HashSet<PathBuf>,
    /// The PNG and WebP files saved turned in the background. A tool sends its files here
    /// too, because it may have changed them, so they are decoded again.
    saved_tx: AnswerSender<SavedFile>,
    saved_rx: mpsc::Receiver<(SavedFile, Working)>,
    /// What Edit → Undo puts back. It is the last move to the trash, move or rename.
    undo: Option<Undo>,
    /// Held by each running operation that will leave an Undo of its own, until it has sent
    /// its report. While one runs, `undo` is older than the operation the user did last.
    undo_on_the_way: Arc<()>,
    cut_short: Vec<journal::CutShort>,
    /// The list of the files of `cut_short` not under the names they were to get, after a
    /// repair of them was refused (`journal::names_left`), and whether the user has saved or
    /// copied it. Only then are the names left as they are.
    names_left: Option<(String, Arc<AtomicBool>)>,
    /// The renames found cut short at the start, by a crash or a power cut during Rename Many,
    /// so they can be put right. They are read in the background.
    cut_short_rx: Receiving<Vec<journal::CutShort>>,
    /// The type of the mount and the files the trash refused there, with their fingerprints
    /// when picked, to ask about once the folder is read again, and the count of listings
    /// (`Folder::whole_listings`) when they came. The files left the list when the delete
    /// started, and only a newer listing brings them back.
    untrashable: Option<(String, Vec<PickedAs>, u64)>,
    /// Our own clipboard for file lists, kept so it can serve them (X11).
    clipboard: Option<arboard::Clipboard>,
    /// An image decoded for the clipboard. egui takes it only during a frame.
    clip_rx: Receiving<(egui::ColorImage, Working)>,
    /// What waits for a look at a folder, sent once the look has answered or been given up
    /// (`wait_for_look_up_before_the_job`). The sender is kept, so the receiver lasts as long as
    /// the window.
    real_folder_tx: AnswerSender<WithRealFolder>,
    real_folder_found: Receiving<(WithRealFolder, Working)>,
    /// How many of those wait. Each starts a file operation (a Copy, say), so a close waits for
    /// them as for the files being written.
    real_folders_awaited: Arc<AtomicUsize>,
}

impl FileOperations {
    /// A `FileOperations` with nothing running, whose answers keep the busy pointer `busy` on
    /// until the window has handled them. It looks in the background for Rename Many's steps
    /// that a crash cut short.
    pub(crate) fn new(busy: &Arc<AtomicUsize>) -> Self {
        let (report_tx, report_rx) = answer_channel(busy);
        let (saved_tx, saved_rx) = answer_channel(busy);
        let (tx, cut_short_rx) = mpsc::channel();
        let (real_folder_tx, real_folder_found) = answer_channel(busy);
        let moves_tx = report_tx.clone();
        std::thread::spawn(move || {
            // The tests share one test home, so a test's window would otherwise find the plans
            // and Move logs that another test left there, and act on them.
            let found = if cfg!(test) {
                Vec::new()
            } else {
                journal::journal_dir().map_or_else(Vec::new, |d| journal::cut_short_plans(&d))
            };
            let _ = tx.send(found);
            #[cfg(not(test))]
            send_moves_cut_short(&moves_tx);
            #[cfg(test)]
            drop(moves_tx);
        });
        FileOperations {
            jobs: Vec::new(),
            held_back_files: Vec::new(),
            renaming: None,
            renaming_rx: Receiving::none(),
            sidecar_writes_waiting: Vec::new(),
            deleting: HashSet::new(),
            report_tx,
            report_rx,
            saving: HashSet::new(),
            saved_tx,
            saved_rx,
            undo: None,
            undo_on_the_way: Arc::new(()),
            cut_short: Vec::new(),
            names_left: None,
            cut_short_rx: Receiving::of(cut_short_rx),
            untrashable: None,
            clipboard: None,
            clip_rx: Receiving::none(),
            real_folder_tx,
            real_folder_found: Receiving::of(real_folder_found),
            real_folders_awaited: Arc::default(),
        }
    }

    /// Where deletes, moves and copies report what they did.
    pub(crate) fn report_sender(&self) -> AnswerSender<OperationReport> {
        self.report_tx.clone()
    }

    /// Marks an operation that will leave an Undo as running. Its thread holds what this
    /// returns until it has sent its report, and Undo waits for it meanwhile.
    fn undo_on_the_way(&self) -> Arc<()> {
        self.undo_on_the_way.clone()
    }

    /// Lets go of the jobs and the `run_in_background` work that ended. Returns true when the last
    /// job writing sidecars ended now.
    pub(crate) fn drop_ended_jobs(&mut self) -> bool {
        let any_writing = |jobs: &[Arc<Job>]| jobs.iter().any(|j| j.writes_sidecars);
        let were_writing = any_writing(&self.jobs);
        self.jobs.retain(|job| Arc::strong_count(job) > 1);
        self.held_back_files.retain(|t| Arc::strong_count(t) > 1);
        were_writing && !any_writing(&self.jobs)
    }

    /// Returns whether a job writing sidecars runs.
    pub(crate) fn is_writing_sidecars(&self) -> bool {
        (self.jobs.iter()).any(|j| j.writes_sidecars && Arc::strong_count(j) > 1)
    }

    /// Returns the running jobs that write the sidecar of `file`, or of a folder it is in. Each
    /// is over once `Weak::strong_count` is 1 or less, as only `jobs` holds it then. The jobs
    /// are looked through once here, so a wait for them does not go through a Lightroom
    /// import's 100,000 files in every frame.
    pub(crate) fn jobs_writing_sidecar_of(&self, file: &Path) -> Vec<std::sync::Weak<Job>> {
        (self.jobs.iter())
            .filter(|j| j.writes_sidecars && Arc::strong_count(j) > 1)
            .filter(|j| j.held_back_files.iter().any(|t| file.starts_with(t)))
            .map(Arc::downgrade)
            .collect()
    }

    /// The first job running, for the status bar, and how many run.
    pub(crate) fn first_job_and_count(&self) -> Option<(&Job, usize)> {
        Some((self.jobs.first()?, self.jobs.len()))
    }

    /// Every job stops after the file at hand, except a sidecar write of one file. That is an
    /// edit the user made, such as the info panel's save, which may wait behind a long job.
    /// Stop acts between files, so it has nothing to cut short there, and the edit would only
    /// be lost while the panel shows it as saved.
    pub(crate) fn stop_jobs(&self) {
        (self.jobs.iter())
            .filter(|job| !(job.writes_sidecars && job.total == 1))
            .for_each(|job| job.stop_requested.store(true, Ordering::Relaxed));
    }

    /// A turned image saved in the background, or a tool's file, with how it went, and the busy
    /// pointer that the window drops once it has handled it.
    pub(crate) fn take_saved(&mut self) -> Option<(PathBuf, Result<(), String>, Working)> {
        let ((path, result), pointer) = self.saved_rx.try_recv().ok()?;
        self.saving.remove(&path);
        Some((path, result, pointer))
    }

    /// Whether the turn of `path` is still being saved.
    pub(crate) fn is_saving(&self, path: &Path) -> bool {
        self.saving.contains(path)
    }

    /// Takes Rename Many's answer, when it has come: the job it was, how it went, and the busy
    /// pointer that the window drops once it has handled it.
    pub(crate) fn take_rename_outcome(
        &mut self,
    ) -> Took<(Option<RenameJob>, RenameOutcome, Working)> {
        match self.renaming_rx.take() {
            Took::Answer((result, pointer)) => {
                Took::Answer((self.renaming.take(), result, pointer))
            }
            // The thread ended without an answer. Closing does not wait for it, and Rename
            // Many is not refused for ever.
            Took::Gone => {
                self.renaming = None;
                Took::Gone
            }
            Took::Nothing => Took::Nothing,
        }
    }

    /// Takes the renames found cut short at the start, added to those already asked about.
    /// Returns false when the search for them stopped without an answer.
    pub(crate) fn receive_cut_short(&mut self) -> bool {
        match self.cut_short_rx.take() {
            // Added to, not replaced: the answer may come after a refused repair has asked
            // about its plans again, and those plans would be lost from the list.
            Took::Answer(found) => self.cut_short.extend(found),
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
        self.names_left = None;
        std::mem::take(&mut self.cut_short)
    }

    /// Asks again about the renames cut short `plans`, whose repair was refused, with `list`,
    /// the files not under the names they were to get (`journal::names_left`).
    pub(crate) fn ask_again_about_cut_short(
        &mut self,
        plans: Vec<journal::CutShort>,
        list: String,
    ) {
        self.cut_short.extend(plans);
        self.names_left = Some((list, Arc::default()));
    }

    /// Returns the list of the files that a refused repair left not under the names they were to
    /// get, and whether the user has kept it (saved or copied it), or `None` when no repair was
    /// refused.
    pub(crate) fn names_left(&self) -> Option<(&str, bool)> {
        let (list, kept) = self.names_left.as_ref()?;
        Some((list, kept.load(Ordering::Relaxed)))
    }

    /// Returns the flag that tells that the user has kept the list of `names_left`, which a
    /// save raises once the file is written.
    pub(crate) fn names_left_kept(&self) -> Option<Arc<AtomicBool>> {
        Some(self.names_left.as_ref()?.1.clone())
    }

    /// Keeps the files `files` that the trash refused on a mount of type `kind`, to ask about
    /// once the list counted `now` listings is read again. The question names the mount of
    /// the first refusal, and a file refused twice is asked about once, as it was first picked.
    fn keep_untrashable(&mut self, kind: String, files: Vec<PickedAs>, now: u64) {
        match &mut self.untrashable {
            Some((_, held, since)) => {
                held.extend(files);
                held.sort_by(|a, b| a.0.cmp(&b.0));
                held.dedup_by(|later, first| later.0 == first.0);
                *since = now;
            }
            none => *none = Some((kind, files, now)),
        }
    }

    /// Leaves out of `files` those still being deleted, so a refresh does not bring them back.
    pub(crate) fn leave_out_deleting(&self, files: &mut Vec<crate::model::listing::ListedName>) {
        if !self.deleting.is_empty() {
            files.retain(|f| !self.deleting.contains(&f.0));
        }
    }

    /// Whether the running Rename Many may rename files that a list of `dir` shows: the
    /// folder shown, with its subfolders, or `/` for several folders or a collection. It may
    /// when `dir` holds the folder of the renamed files, or that folder holds `dir`. The test
    /// does no work per file, as it runs every frame.
    pub(crate) fn renames_files_in(&self, dir: &Path) -> bool {
        (self.renaming.as_ref())
            .is_some_and(|job| job.folder.starts_with(dir) || dir.starts_with(&job.folder))
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
    pub(crate) fn copy_image_from(&mut self, rx: mpsc::Receiver<(egui::ColorImage, Working)>) {
        self.clip_rx = Receiving::of(rx);
    }

    /// The image decoded for the clipboard, once it came, with the busy pointer that the window
    /// drops once the image is on the clipboard.
    pub(crate) fn take_image(&mut self) -> Option<(egui::ColorImage, Working)> {
        match self.clip_rx.take() {
            Took::Answer(answer) => Some(answer),
            _ => None,
        }
    }

    /// Our own clipboard, made the first time. On X11 the clipboard is served from it, so it is
    /// kept.
    pub(crate) fn clipboard(&mut self) -> Result<&mut arboard::Clipboard, arboard::Error> {
        let clipboard = match self.clipboard.take() {
            Some(clipboard) => clipboard,
            None => arboard::Clipboard::new()?,
        };
        Ok(self.clipboard.insert(clipboard))
    }
}

impl crate::App {
    /// To the trash at once, permanently after confirmation.
    pub(crate) fn request_delete(&mut self, ctx: &egui::Context, trash: bool) {
        self.request_delete_as_seen(ctx, trash, None);
    }

    /// `request_delete`, of files whose fingerprints `seen` a search took when it found them
    /// (Find Duplicates), when it is given. The delete then leaves out a file that is no longer
    /// as the search saw it, not only one changed while the question was open.
    pub(crate) fn request_delete_as_seen(
        &mut self,
        ctx: &egui::Context,
        trash: bool,
        seen: Option<&HashMap<PathBuf, FileFingerprint>>,
    ) {
        if self.list.files().is_empty() {
            return;
        }
        if if trash {
            self.config.confirm_trash
        } else {
            self.config.confirm_delete
        } {
            if let Some(asked) = self.ask_about_targets_as_seen(seen) {
                self.dialogs.confirm_delete(asked, trash);
            }
        } else if let Some(targets) = self.targets() {
            self.delete_targets(ctx, trash, targets, seen);
        }
    }

    /// The files at `targets`, picked now, with the fingerprints `seen` when they are given
    /// (`request_delete_as_seen`), and otherwise with fingerprints taken now.
    fn picked_targets(
        &self,
        targets: &[usize],
        seen: Option<&HashMap<PathBuf, FileFingerprint>>,
    ) -> PickedFiles {
        let files = targets.iter().map(|&i| self.listed_file(i)).collect();
        match seen {
            Some(seen) => PickedFiles::listed_as_seen(files, seen),
            None => PickedFiles::listed(files),
        }
    }

    /// A hash of what the list shows: the folder, the folders or the collection opened. Another
    /// folder or collection that is opened may hold files with the same names.
    fn opened_folder_hash(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (
            self.folder.dir(),
            self.folder.folders(),
            self.folder.collection(),
        )
            .hash(&mut hasher);
        hasher.finish()
    }

    /// Takes the targets' names for a dialog about them, which needs them to open. A folder
    /// that another program writes to is read again every second, and that can give them other
    /// places in the list. Returns `None`, with a notice, when there are no targets, and the
    /// dialog must not open.
    pub(crate) fn ask_about_targets(&mut self) -> Option<Asked> {
        self.ask_about_targets_as_seen(None)
    }

    /// `ask_about_targets`, with the fingerprints `seen` that a search took, when given
    /// (`request_delete_as_seen`).
    fn ask_about_targets_as_seen(
        &mut self,
        seen: Option<&HashMap<PathBuf, FileFingerprint>>,
    ) -> Option<Asked> {
        let targets = self.targets()?;
        Some(self.ask_about_files(&targets, seen))
    }

    /// The files at `targets` for a dialog about them, as `ask_about_targets_as_seen` takes
    /// them. A dialog about one file of the selection (Rename) asks about that one file alone,
    /// so that its answer can act on no other.
    fn ask_about_files(
        &mut self,
        targets: &[usize],
        seen: Option<&HashMap<PathBuf, FileFingerprint>>,
    ) -> Asked {
        let names = targets.iter().map(|&i| self.list.files()[i].clone());
        let picked = self.picked_targets(targets, seen);
        let left_out = self.hidden_selection_notice();
        Asked::new(self.opened_folder_hash(), names.collect(), left_out, picked)
    }

    /// The places now of the files the dialog asked about that are still in the list, and the
    /// files as they were picked, with their fingerprints. Returns `None`, with a notice, when
    /// none of them is, or when another folder or collection is shown.
    pub(crate) fn asked_targets(&mut self) -> Option<(Vec<usize>, PickedFiles)> {
        let (opened_folder_hash, names, picked) = self.dialogs.take_asked()?;
        let names: HashSet<OsString> = names.into_iter().collect();
        let found: Vec<usize> = match opened_folder_hash == self.opened_folder_hash() {
            true => (self.list.files().iter().enumerate())
                .filter(|(_, f)| names.contains(*f))
                .map(|(i, _)| i)
                .collect(),
            false => Vec::new(),
        };
        if found.is_empty() {
            self.notice.give(
                tr("The files asked about are no longer in the list: nothing was done").into(),
            );
            return None;
        }
        Some((found, picked))
    }

    /// Returns whether no file of `paths`, or in a folder of them, is being saved turned or changed
    /// by another job or by background work, and no keywords are being written. Otherwise it shows
    /// a notice. A PNG or WebP that was deleted, moved or renamed meanwhile would come back when
    /// its save puts it under its name again. A sidecar written file by file would miss the files
    /// that were moved before their turn, and an Undo would put files back over a move.
    /// `start_job` and `run_in_background` ask it themselves, so no operation can leave it out.
    pub(crate) fn are_free_to_change(&mut self, paths: &[PathBuf]) -> bool {
        let saving =
            (self.file_operations.saving.iter()).any(|s| paths.iter().any(|p| s.starts_with(p)));
        let writing = (self.file_operations.jobs.iter())
            .any(|j| j.writes_sidecars && Arc::strong_count(j) > 1);
        // Only one Rename Many runs at a time, and nothing else works on files while it runs.
        // Otherwise a step would find its file gone, and its undo could not put the others
        // back.
        let renaming = self.file_operations.renaming.is_some();
        // A list just opened shows the names in its file until they are checked to be files.
        // A folder named there was moved to the trash as one of its entries. A listing that
        // broke checked nothing either, so the list waits for one that comes whole (F5).
        let reading = self.list.has_unchecked_entries();
        let busy =
            !saving && !writing && !renaming && (reading || self.is_changed_by_running_work(paths));
        if saving {
            self.notice
                .give(tr("Still saving a rotation: try again in a moment").into());
        } else if writing {
            self.notice
                .give(tr("Still writing keywords: try again in a moment").into());
        } else if renaming {
            self.notice.give(tr("Rename Many is still renaming").into());
        } else if reading && !self.folder.is_listing() {
            // No listing runs that would check the list in a moment, so waiting would not help.
            self.tell_background_stopped(tr("Reading the folder"));
        } else if busy {
            self.notice
                .give(tr("Still working on these files: try again in a moment").into());
        }
        !saving && !writing && !renaming && !busy
    }

    /// Returns whether a job or background work that still runs changes a file of `paths`, a folder
    /// above one, or a file inside one of them. Work that is over does not count, whether or
    /// not the frame has let go of it yet. The lookup is by name and by the folders above, not
    /// path against path, because a delete of 1,000 files beside a keywords job on 100,000 was
    /// 10^8 comparisons.
    fn is_changed_by_running_work(&self, paths: &[PathBuf]) -> bool {
        let held_back: Vec<_> = (self.file_operations.held_back_files.iter())
            .filter(|t| Arc::strong_count(t) > 1)
            .map(|t| lock_even_if_poisoned(t))
            .collect();
        let running = (self.file_operations.jobs.iter())
            .filter(|j| Arc::strong_count(j) > 1)
            .flat_map(|j| &j.held_back_files)
            .chain(held_back.iter().flat_map(|t| t.iter()));
        let held: HashSet<&Path> = running.map(PathBuf::as_path).collect();
        if held.is_empty() {
            return false;
        }
        let above: HashSet<&Path> = held.iter().flat_map(|t| t.ancestors()).collect();
        (paths.iter())
            .any(|p| above.contains(p.as_path()) || p.ancestors().any(|a| held.contains(a)))
    }

    /// Whether files are still being written (`file_io::WRITING`), by file jobs, a turned
    /// image, a print, keywords or the settings, or a file operation waits for its folder's
    /// real path. Closing waits for them.
    pub(crate) fn is_writing(&self) -> bool {
        crate::files::file_io::WRITING.load(Ordering::Acquire) > 0
            || (self.file_operations.real_folders_awaited).load(Ordering::Acquire) > 0
    }

    /// A file operation on `total` files that changes `held_back_files`, shown in the status bar
    /// until its thread drops it. Returns `None`, with a notice, while something else works on
    /// them.
    pub(crate) fn start_job(
        &mut self,
        activity: &'static str,
        total: usize,
        held_back_files: Vec<PathBuf>,
    ) -> Option<Arc<Job>> {
        self.start_job_of(activity, total, false, held_back_files)
    }

    /// `start_job`, of the files' sidecars (`writes_sidecars`) or of the files themselves.
    pub(crate) fn start_job_of(
        &mut self,
        activity: &'static str,
        total: usize,
        writes_sidecars: bool,
        held_back_files: Vec<PathBuf>,
    ) -> Option<Arc<Job>> {
        // Keywords and stars are never refused, because an edit made in the panel would be
        // lost. Instead, their job holds back every other operation (`are_free_to_change`'s
        // `writing`). A job that changes none of the user's files, such as a backup, is not
        // refused either.
        if !writes_sidecars
            && !held_back_files.is_empty()
            && !self.are_free_to_change(&held_back_files)
        {
            return None;
        }
        let job = Arc::new(Job::new(activity, total, writes_sidecars, held_back_files));
        self.file_operations.jobs.push(job.clone());
        Some(job)
    }

    /// Deletes the files at `targets` at once, with no question between the pick and the
    /// delete, as `delete_now` does. `seen` are the fingerprints a search took of them, when
    /// given (`request_delete_as_seen`).
    pub(crate) fn delete_targets(
        &mut self,
        ctx: &egui::Context,
        trash: bool,
        targets: Vec<usize>,
        seen: Option<&HashMap<PathBuf, FileFingerprint>>,
    ) {
        let picked = self.picked_targets(&targets, seen);
        self.delete_now(ctx, trash, targets, picked);
    }

    /// Deletes the files at `targets` in the background, to the trash or for good, as a job
    /// with progress and Stop. A file that is no longer the one `picked` is left alone and told,
    /// because another program or window may have given its name to another photo while the
    /// question was open. The list follows when `receive_operation_report` hears back.
    pub(crate) fn delete_now(
        &mut self,
        ctx: &egui::Context,
        trash: bool,
        targets: Vec<usize>,
        picked: PickedFiles,
    ) {
        let paths: Vec<PathBuf> = targets.iter().map(|&i| self.path(i)).collect();
        let Some(job) = self.start_job(tr("Deleting"), paths.len(), paths.clone()) else {
            return;
        };
        self.file_operations
            .deleting
            .extend(targets.iter().map(|&i| self.list.files()[i].clone()));
        self.remove_indices(ctx, &targets);
        let freed_tx = self.status_bar.freed_sender();
        let (repaint, report_tx) = (ctx.clone(), self.file_operations.report_sender());
        let collection = self.folder.collection().map(Path::to_path_buf);
        let undo_on_the_way = trash.then(|| self.file_operations.undo_on_the_way());
        // The tests share one state folder, and a test's delete would leave its log there when
        // the test does not take the report. They test the log on folders of their own.
        #[cfg(not(test))]
        let delete_log_dir = journal::delete_log_dir();
        #[cfg(test)]
        let delete_log_dir: Option<PathBuf> = None;
        self.run_in_background(ctx, Vec::new(), move || {
            let since = std::time::SystemTime::now();
            let (_, picked) = picked.wait_for_fingerprints();
            let started = (delete_log_dir.as_deref())
                .map(|d| journal::start_delete_log(d, collection.as_deref()));
            let (mut delete_log, start_failure) = match started.transpose() {
                Ok(log) => (log, None),
                Err(e) => (None, Some(e)),
            };
            // Read the size before deleting, so the status bar can subtract it.
            let _ = freed_tx.send(total_size(&paths));
            repaint.request_repaint();
            let mut errors = Vec::new();
            let how = (trash, delete_log.as_mut());
            let (gone, sides, changed) = delete_in_chunks(&paths, &picked, how, &job, &mut errors);
            let told = match errors.first() {
                Some(first) => Some(Told::FailedWrite(tf!(
                    "Delete failed ({count}): {first}",
                    count = count(errors.len(), "error"),
                    first
                ))),
                None => job
                    .report(|| match trash {
                        true => tf!(
                            "{count} moved to the trash",
                            count = count(paths.len(), "file")
                        ),
                        false => tf!("{count} deleted", count = count(paths.len(), "file")),
                    })
                    .map(Told::Notice),
            };
            // The log's warning goes with the delete's own text, so that neither hides the other.
            let log_failure = start_failure.or_else(|| delete_log.as_mut()?.take_failure());
            let warning = delete_log_dir.zip(log_failure).map(log_not_saved);
            let mut told = with_warning(told, warning);
            let forgotten = catch_panic(|| forget_in_collection(collection.as_deref(), &gone));
            let removed = (forgotten.unwrap_or_else(|| Err(broke_here()))).unwrap_or_else(|e| {
                told = Some(Told::FailedWrite(match told.take() {
                    Some(Told::FailedWrite(text)) => format!("{text}\n{e}"),
                    _ => e,
                }));
                Vec::new()
            });
            let (sides_gone, sidecars_left): (Vec<PathBuf>, Vec<PathBuf>) =
                (sides.into_iter()).partition(|s| is_surely_gone(s));
            let undo = (trash && !gone.is_empty()).then(|| {
                Undo::Trash(TrashMove {
                    files: gone.clone(),
                    // Only the sidecars that are gone count, because a sidecar gio could not
                    // move is still there. Those are kept apart, as they are still the files'.
                    sidecars: sides_gone,
                    sidecars_left,
                    since,
                    marks: Default::default(),
                    collection: collection.zip(Some(removed)),
                })
            });
            let refused = trash && !errors.is_empty() && !job.is_stopped();
            // Those not reached after Stop are still there, as after a failure. A file left
            // alone is another file now, and the question to delete it for good is not asked.
            let refused_paths: Vec<PathBuf> = (paths.iter())
                .filter(|p| !changed.contains(*p))
                .cloned()
                .collect();
            let _ = report_tx.send(OperationReport {
                untrashable: refused
                    .then(|| left_by_trash_as_picked(&refused_paths, &picked))
                    .flatten(),
                requested: paths,
                gone,
                any_failed: !errors.is_empty() || job.is_stopped(),
                undo,
                delete_log,
                ..Default::default()
            });
            drop((undo_on_the_way, job));
            told
        });
    }

    /// Receives what deletes and moves report. Files that moved take their marks along. Files
    /// that are gone leave the list, the marks and Find Duplicates. After a failure, the folder
    /// is read again instead.
    pub(crate) fn receive_operation_report(&mut self, ctx: &egui::Context) {
        // The busy pointer of each report goes once the report has been handled.
        for (
            OperationReport {
                requested,
                gone,
                any_failed,
                moved,
                is_copy,
                read_folder_again,
                undo,
                is_undo_remainder,
                restored_marks,
                changed,
                overwritten,
                folder_renamed,
                collection_renamed,
                folder_made,
                untrashable,
                move_log,
                delete_log,
                cut_short_left,
                is_moved_from_a_record,
            },
            _pointer,
        ) in self
            .file_operations
            .report_rx
            .try_iter()
            .collect::<Vec<_>>()
        {
            if let Some((kind, files)) = untrashable {
                let now = self.folder.whole_listings();
                self.file_operations.keep_untrashable(kind, files, now);
            }
            if let Some((plans, list)) = cut_short_left {
                self.file_operations.ask_again_about_cut_short(plans, list);
            }
            if let Some((old, new)) = folder_renamed {
                self.folder_renamed(ctx, old, new);
            }
            if let Some((old, new)) = collection_renamed {
                self.collection_renamed(ctx, &old, new);
            }
            if let Some(made) = folder_made {
                self.tree.forget(&made);
                self.folder.relist_later();
            }
            // A file written over holds another image, so its marks are not this one's, just as
            // its sidecar was not. They go before a copy's marks are put on it.
            #[cfg(feature = "marks")]
            self.marks.forget(&overwritten, None);
            // The moves come before the deleted files, so the marks go along first.
            self.follow_moves_reported(moved, is_copy, move_log, is_moved_from_a_record);
            self.written_anew(&changed);
            self.files_changed(ctx, &changed);
            self.files_changed(ctx, &overwritten);
            self.forget_names(&gone);
            // Deleted files lose their marks, so that a new file with the same name does not
            // get them. The marks are kept with the undo of a move to the trash, and it puts
            // them back.
            #[cfg(feature = "marks")]
            {
                // Added to, not replaced: an Undo that put nothing back sends its marks back.
                let for_undo = match &undo {
                    Some(Undo::Trash(trash_move)) => Some(trash_move.marks.clone()),
                    _ => None,
                };
                self.marks.forget(&gone, for_undo);
                self.marks.restore(restored_marks);
            }
            #[cfg(not(feature = "marks"))]
            drop(restored_marks);
            // The log of a delete goes once the marks of its files that went are forgotten.
            if let Some(log) = delete_log {
                log.remove_once_forgotten();
            }
            if undo.is_some() && !(is_undo_remainder && self.file_operations.undo.is_some()) {
                self.file_operations.undo = undo;
            }
            if let Some(find_duplicates) = &mut self.find_duplicates {
                // Those that failed are still there.
                find_duplicates.remove(&gone.iter().map(PathBuf::as_path).collect());
            }
            // The files are found by the name the list has, which includes their folders in
            // the list with subfolders and in a collection.
            // A file of the folder shown that the listing did not hold back means the folder
            // was left and opened again meanwhile, so its listing may still have files that are
            // gone now.
            let mut unknown = false;
            for p in &requested {
                let Ok(rel) = p.strip_prefix(self.folder.dir()) else {
                    continue;
                };
                unknown |= !self.file_operations.deleting.remove(rel.as_os_str());
            }
            // The files that are still there come back into the list, from its folder or from
            // the collection's file. A listing that started before the files went still has
            // them, so the folder is read again after it.
            if unknown
                || any_failed
                || read_folder_again
                || (!requested.is_empty() && self.folder.is_listing())
            {
                self.folder.relist_later();
            }
        }
        // The question waits for a listing taken after the refusal came, which brings the files
        // back, and for any other dialog to close, which keeps the keys meant for it. It does
        // not wait for the folder to be still, because a folder that changes all the time, such
        // as a sync folder taking in uploads with auto refresh on, never is.
        let listed = self.folder.whole_listings();
        if !self.dialog_open()
            && (self.file_operations.untrashable.as_ref())
                .is_some_and(|(_, _, since)| listed > *since)
            && let Some((kind, files, _)) = self.file_operations.untrashable.take()
        {
            self.ask_delete_instead(kind, &files);
        }
    }

    /// Asks whether to delete `files` for good, which the trash refused on a network or cloud
    /// mount of type `kind`. gio has no trash there, and a trash in the home folder would mean
    /// a download of each file. It asks only about files the list shows, by their names there.
    /// A notice tells of the others, which are left as they are: another folder was opened
    /// before the question, or the file is in a subfolder the list does not show. The delete
    /// acts only on a file that still has the fingerprint it had when it was first picked, and
    /// the identity the list knows it by now, as another program may have given its name to
    /// another file since.
    fn ask_delete_instead(&mut self, kind: String, files: &[PickedAs]) {
        // Only files the list shows are asked about: a subfolder's file strips the folder's
        // prefix too, but is not in the list with Include Subfolders off.
        let dir = self.folder.dir().to_path_buf();
        let as_picked: HashMap<&OsStr, (&PathBuf, &FileFingerprint)> = (files.iter())
            .filter_map(|(p, picked_as)| {
                Some((p.strip_prefix(&dir).ok()?.as_os_str(), (p, picked_as)))
            })
            .collect();
        let shown: Vec<usize> = (0..self.list.files().len())
            .filter(|&i| as_picked.contains_key(self.list.files()[i].as_os_str()))
            .collect();
        let names: Vec<OsString> = shown
            .iter()
            .map(|&i| self.list.files()[i].clone())
            .collect();
        let seen: HashMap<PathBuf, FileFingerprint> = (as_picked.values())
            .map(|(p, picked_as)| ((*p).clone(), **picked_as))
            .collect();
        let left = files.len() - names.len();
        if left > 0 {
            self.notice.give(tf!(
                "The trash refused {count} ({kind}). Nothing was deleted, because another folder is open now.",
                count = count(left, "file"),
                kind
            ));
        }
        if names.is_empty() {
            return;
        }
        let listed = shown.iter().map(|&i| self.listed_file(i)).collect();
        let asked = Asked::new(
            self.opened_folder_hash(),
            names,
            None,
            PickedFiles::listed_as_seen(listed, &seen),
        );
        self.dialogs.confirm_delete_instead(asked, kind);
    }

    /// Brings along what Glimt keeps about the files a report says were `moved`, or copied
    /// (`is_copy`), as `follow_moved_paths` does. A Move's log goes once they have followed.
    /// When `is_from_a_record`, the moves were read from a record and not made by the job, so
    /// the marks follow only to a name where a file is now. A Move's log found at the start is
    /// such a record too.
    fn follow_moves_reported(
        &mut self,
        moved: Vec<(PathBuf, PathBuf)>,
        is_copy: bool,
        move_log: Option<journal::MoveLog>,
        is_from_a_record: bool,
    ) {
        match &move_log {
            Some(log) if log.is_found_at_start() => {
                // The names follow for every file, the open collection's entries among them.
                self.follow_names_moved(&moved, false);
                #[cfg(feature = "marks")]
                {
                    let not_followed = self.not_followed_before_a_kill(moved);
                    if !not_followed.is_empty() {
                        self.marks.follow_files(not_followed, false, true);
                    }
                }
            }
            _ if is_from_a_record => {
                self.follow_names_moved(&moved, is_copy);
                #[cfg(feature = "marks")]
                self.marks.follow_files(moved, is_copy, true);
            }
            _ => self.follow_moved_paths(moved, is_copy),
        }
        if let Some(log) = move_log {
            log.remove_once_followed();
        }
    }

    /// Returns the files of `went`, moved before Glimt was killed, whose marks have not
    /// followed them yet. A file whose old name holds no marks had them follow it before the
    /// kill, or had none, and following it again would take them off its new name. Its
    /// collections' entries follow all the same (`follow_names_moved`), as entries already
    /// followed are not found again.
    #[cfg(feature = "marks")]
    fn not_followed_before_a_kill(&self, went: Vec<(PathBuf, PathBuf)>) -> Vec<(PathBuf, PathBuf)> {
        (went.into_iter())
            .filter(|(from, _)| self.marks.get(from) != (0, 0))
            .collect()
    }

    /// Brings along what Glimt keeps about files or folders that were moved or renamed
    /// `(from, to)`, or copied (`is_copy`). It is the one place every operation reports to. The
    /// marks go along, to a copy too. When the files moved, a collection's entries of them, and
    /// of everything in a moved folder, take the new place, in the list and in its file, and so
    /// do the entries of every saved collection.
    pub(crate) fn follow_moved_paths(&mut self, pairs: Vec<(PathBuf, PathBuf)>, is_copy: bool) {
        // No message of a delete has pairs, so each one skips the look at every name in the
        // list.
        if pairs.is_empty() {
            return;
        }
        self.follow_names_moved(&pairs, is_copy);
        #[cfg(feature = "marks")]
        // The job that reports them has just moved the files, so their new names hold them.
        self.marks.follow_files(pairs, is_copy, false);
    }

    /// Brings along all that `follow_moved_paths` does but the marks.
    fn follow_names_moved(&mut self, pairs: &[(PathBuf, PathBuf)], is_copy: bool) {
        if pairs.is_empty() {
            return;
        }
        // Both names hold another file now, or none, so a moved file's caches would show under
        // the new name what an old file of that name had.
        let names: Vec<PathBuf> = (pairs.iter())
            .flat_map(|(from, to)| [(!is_copy).then(|| from.clone()), Some(to.clone())])
            .flatten()
            .collect();
        self.forget_names(&names);
        let moved = moves_by_source(pairs);
        // A turn not saved yet goes with its file, saved under the new name.
        if !is_copy && let Some(to) = new_place(self.viewing.turned_file(), &moved) {
            self.viewing.turned_file_moved(to);
        }
        // So does an edit in the panel not saved yet.
        if !is_copy {
            let ctx = self.ctx.clone();
            self.follow_metadata_edit(&ctx, &moved);
        }
        if !is_copy && self.folder.collection().is_some() {
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
            let moves = pairs.to_vec();
            let follow = move |all: &mut Vec<PathBuf>| {
                let moved = moves_by_source(&moves);
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
                // moved left them out. The file still follows, as it may hold none of them, and
                // a failure is told. It is written in the writer thread, as every edit of the
                // collection.
                let file = file.to_path_buf();
                crate::files::file_io::write_later_in_user_folders(move || {
                    if let Err(e) = collections::edit_open_collection(&file, follow) {
                        crate::files::file_io::save_failed(e);
                    }
                });
            }
            // The list is read again from the file, because a refresh that started before they
            // moved brings the old names.
            self.folder.relist_later();
        }
        if !is_copy {
            collections::follow_in_saved_collections(pairs, self.folder.collection());
        }
        self.index_new_names();
    }

    /// Looks up the real path of `folder` and the type of its file system in the background, as
    /// the real path waits for the disk, and then runs `then` with them in the window's thread,
    /// as `wait_for_look_up_before_the_job` says, with `leaves_undo` when its job leaves an Undo.
    pub(crate) fn with_real_folder(
        &mut self,
        folder: PathBuf,
        (held_back_files, leaves_undo): (Vec<PathBuf>, bool),
        then: impl FnOnce(&mut crate::App, &egui::Context, PathBuf, Option<String>) + Send + 'static,
    ) {
        let (found_tx, found) = mpsc::channel();
        let looked_at = folder.clone();
        // No busy pointer: the look-up, whose answer `wait_for_look_up_before_the_job` holds
        // the pointer for. One given up ends when the share answers, or never.
        std::thread::spawn(move || {
            let real = crate::model::listing::real_dir(&looked_at);
            let file_system = file_system_type_or_test(&real);
            drop(found_tx.send(LookUpStep::Answered((real, file_system))));
        });
        self.wait_for_look_up_before_the_job(
            folder,
            (held_back_files, leaves_undo),
            found,
            move |app, ctx, found| then(app, ctx, found.0, found.1),
        );
    }

    /// Waits in the background for `found`, the answer of a look at the folder `folder` that
    /// waits for the disk, and then runs `then` with it in the window's thread. The busy pointer
    /// shows meanwhile. A close waits for it too, as for a file operation, because `then` starts
    /// one (a Copy, say). The files `held_back_files` that its job will change are held back
    /// from now on, and let go just before `then` starts the job that holds them in turn.
    /// Nothing is waited for, and a notice says so, while other work changes them. A look-up
    /// that sends neither its answer nor a sign that it goes on (`LookUpStep`) for
    /// `LOOK_UP_GIVEN_UP_AFTER` (a share that stopped answering) is given up: the files are let
    /// go, `then` never runs, and a notice says so. Before, the files stayed held until Glimt
    /// ended, and a delete or rename of them was refused. When the job
    /// `leaves_undo`, Ctrl+Z waits from now on, as it does while the job runs, so it does not
    /// undo the operation before it. Before, it waited only from the job's start.
    pub(crate) fn wait_for_look_up_before_the_job<T: Send + 'static>(
        &mut self,
        folder: PathBuf,
        (held_back_files, leaves_undo): (Vec<PathBuf>, bool),
        found: mpsc::Receiver<LookUpStep<T>>,
        then: impl FnOnce(&mut crate::App, &egui::Context, T) + Send + 'static,
    ) {
        // Before, the files were held only from the job's start, so a delete and a rename in
        // the moment before could hand a Move another file of the same name.
        let Some(held_back) = self.hold_back_while_writing(held_back_files) else {
            return;
        };
        let tx = self.file_operations.real_folder_tx.clone();
        let (pointer, ctx) = (Working::new(&self.busy), self.ctx.clone());
        let awaited = Working::new(&self.file_operations.real_folders_awaited);
        let undo_on_the_way = leaves_undo.then(|| self.file_operations.undo_on_the_way());
        let wait = move || {
            // Each sign that the look goes on starts the time again. Before, the time counted
            // from the start only, and a look at many files was given up while it answered.
            let found = loop {
                match found.recv_timeout(LOOK_UP_GIVEN_UP_AFTER) {
                    Ok(LookUpStep::GoesOn) => continue,
                    Ok(LookUpStep::Answered(found)) => break Ok(found),
                    Err(e) => break Err(e),
                }
            };
            // The close waits until `then` has run, and the job it starts holds the close
            // from then on.
            let _ = tx.send(Box::new(
                move |app: &mut crate::App, ctx: &egui::Context| {
                    // The window's thread runs both, so no other work comes between them.
                    drop(held_back);
                    match found {
                        Ok(found) => then(app, ctx, found),
                        Err(_) => app.notice.give(tf!(
                            "{folder} did not answer within a minute, so nothing was done",
                            folder = folder.display()
                        )),
                    }
                    // The job that `then` started took its own, so Ctrl+Z waits on.
                    drop((awaited, undo_on_the_way));
                },
            ));
            None
        };
        // Through `told_work`, as the thread holds an Undo on the way.
        std::thread::spawn(told_work(self.notice_sender.clone(), &ctx, pointer, wait));
    }

    /// Runs what waited for a look at a folder, once the look has answered or been given up.
    pub(crate) fn receive_real_folders(&mut self, ctx: &egui::Context) {
        while let Took::Answer((then, _pointer)) = self.file_operations.real_folder_found.next() {
            then(self, ctx);
        }
    }

    /// Copies or moves the files `picked` into `dest` in the background. Existing files are
    /// never overwritten. A move across file systems falls back to a copy and a delete. A file
    /// that is no longer the one picked (Move To and Copy To, while the folder chooser was open)
    /// stays.
    pub(crate) fn transfer(&mut self, picked: PickedFiles, dest: PathBuf, is_copy: bool) {
        let paths = picked.paths().to_vec();
        // The folder by its real path (`~/card` → `/media/u/CARD`, `CARD/..` → `/media/u`), so
        // that its file system is the one the files go to, and a move into the folder shown
        // through a link is seen as one. It is looked up in the background, because it waits
        // for the disk, and checked before the files leave the list, so a refused move keeps
        // the view.
        let leaves_undo = !is_copy;
        self.with_real_folder(
            dest,
            (paths, leaves_undo),
            move |app, ctx, dest, file_system| {
                app.transfer_into_real_folder(ctx, picked, (dest, file_system), is_copy)
            },
        );
    }

    /// Copies or moves the files `picked` into `dest`, a folder by its real path, as `transfer`
    /// says. Its file system's type, `dest_file_system`, was looked up with it in the background.
    fn transfer_into_real_folder(
        &mut self,
        ctx: &egui::Context,
        picked: PickedFiles,
        (dest, dest_file_system): (PathBuf, Option<String>),
        is_copy: bool,
    ) {
        let paths = picked.paths().to_vec();
        if let Some(text) = refused_transfer(&paths, &dest, is_copy, |_| dest_file_system.clone()) {
            self.notice.give(text);
            return;
        }
        // A copy changes none of the files it reads, but it holds them: a Rename Many that ran
        // meanwhile could hand it one photo with another photo's sidecar, and a move or delete
        // could take a file away halfway.
        let held_back_files = paths.clone();
        let activity = tr(if is_copy { "Copying" } else { "Moving" });
        let Some(job) = self.start_job(activity, paths.len(), held_back_files) else {
            return;
        };
        // In a collection, the entries follow the files that got there, in
        // `receive_operation_report`.
        if !is_copy && self.folder.collection().is_none() {
            self.moving_out(ctx, &paths, &dest);
        }
        let freed_tx = self.status_bar.freed_sender();
        let report_tx = self.file_operations.report_tx.clone();
        let same_dir = paths.first().and_then(|p| p.parent()) == Some(self.folder.dir());
        // When the files go into the folder, or into one below it in the list with subfolders,
        // they join the list. Both paths are real, as `listing::walk` lists a folder reached
        // through a link only when its real path is inside the folder's.
        let listed = dest == self.folder.dir()
            || (self.folder.recursive() && dest.starts_with(self.folder.dir()));
        let undo_on_the_way = (!is_copy).then(|| self.file_operations.undo_on_the_way());
        // The tests share one state folder, and a test's Move would leave its log there when
        // the test does not take the report. They test the log on folders of their own.
        #[cfg(not(test))]
        let move_log_dir = (!is_copy).then(journal::move_log_dir).flatten();
        #[cfg(test)]
        let move_log_dir: Option<PathBuf> = None;
        self.run_in_background(ctx, Vec::new(), move || {
            let (paths, picked) = picked.wait_for_fingerprints();
            if !is_copy && same_dir {
                let _ = freed_tx.send(total_size(&paths));
            }
            let started = move_log_dir.as_deref().map(journal::start_move_log);
            let (mut move_log, start_failure) = match started.transpose() {
                Ok(log) => (log, None),
                Err(e) => (None, Some(e)),
            };
            let how = (is_copy, move_log.as_mut());
            let (errors, put) = put_files(&paths, (&picked, &dest), how, &job);
            let log_failure = start_failure.or_else(|| move_log.as_mut()?.take_failure());
            let log_warning = move_log_dir.zip(log_failure).map(log_not_saved);
            let gone: Vec<PathBuf> = match is_copy {
                true => Vec::new(),
                false => put.iter().map(|(from, _)| from.clone()).collect(),
            };
            let failed = !errors.is_empty() || job.is_stopped();
            // The log's warning goes with the Move's own text, so that neither hides the other.
            let with_warning = |text: Option<String>| {
                let texts: Vec<String> = [text, log_warning].into_iter().flatten().collect();
                (!texts.is_empty()).then(|| texts.join("\n"))
            };
            let told = match errors.first() {
                Some(first) => {
                    let verb = tr(if is_copy { "Copy" } else { "Move" });
                    let failed = tf!(
                        "{verb} failed ({count}): {first}",
                        verb,
                        count = count(errors.len(), "error"),
                        first
                    );
                    with_warning(Some(failed)).map(Told::FailedWrite)
                }
                None => with_warning(job.report(|| {
                    let (files, folder) = (count(paths.len(), "file"), dest.display());
                    match is_copy {
                        true => tf!("{files} copied to {folder}", files, folder),
                        false => tf!("{files} moved to {folder}", files, folder),
                    }
                }))
                .map(Told::Notice),
            };
            // A move takes the files out of `deleting`. The folder is read again for files that
            // joined it, or that failed to go, were refused or were not reached after Stop. A
            // copy to another folder does not change the list.
            let read_folder_again = (listed || !is_copy) && (listed || failed);
            let undo = (!is_copy && !put.is_empty())
                .then(|| Undo::Move(put.iter().filter_map(with_fingerprint).collect()));
            let _ = report_tx.send(OperationReport {
                requested: if is_copy { Vec::new() } else { paths },
                gone,
                any_failed: failed,
                moved: put,
                is_copy,
                read_folder_again,
                undo,
                move_log,
                ..Default::default()
            });
            drop((undo_on_the_way, job));
            told
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
        self.file_operations
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
    pub(crate) fn rename_many(&mut self, ctx: &egui::Context, pattern: &str, picked: PickedFiles) {
        let Some(targets) = self.targets() else {
            return;
        };
        let listed: Vec<ListedFile> = targets.iter().map(|&i| self.listed_file(i)).collect();
        let from: Vec<PathBuf> = targets.iter().map(|&i| self.path(i)).collect();
        if !self.are_free_to_change(&from) {
            return;
        }
        let to: Vec<PathBuf> = (from.iter().enumerate())
            .map(|(k, p)| p.with_file_name(pattern_name(pattern, p, k + 1)))
            .collect();
        let unique: HashSet<&PathBuf> = to.iter().collect();
        if unique.len() < to.len() {
            self.notice.give(
                tr("The pattern gives several files the same name; add # for a number").into(),
            );
            return;
        }
        self.rename_in_background(ctx, listed, to, picked);
    }

    /// Renames `from` to `to`, one file or many, in the background, because thousands of files
    /// or a network share that stalls would hold the window. `finish_rename_many` takes the result.
    /// Each file of `from` that is no longer the one `picked` keeps its name, and is told.
    pub(crate) fn rename_in_background(
        &mut self,
        ctx: &egui::Context,
        from: Vec<ListedFile>,
        to: Vec<PathBuf>,
        picked: PickedFiles,
    ) {
        self.rename_files_of(ctx, (from, to), picked, None);
    }

    /// Renames `from` to `to` in the background, as `rename_in_background` says. Each file at
    /// `from` that is no longer the one `picked` is left alone. An Undo (`undoing`) gives the
    /// fingerprints the files had after the rename it undoes, and is put back when its job
    /// does not start.
    fn rename_files_of(
        &mut self,
        ctx: &egui::Context,
        (listed, to): (Vec<ListedFile>, Vec<PathBuf>),
        picked: PickedFiles,
        undoing: Option<UndoUntilStarted>,
    ) {
        // A camera card refuses some characters. Saying so before the job starts names the
        // reason, where a rename that fails halfway would only give the system's error. Only a
        // new name with such a character needs the look at its folder's file system. That look
        // reads only the mount table, which never waits for a share, but it reads a file, and
        // the window's thread reads none (`the_window_thread_never_waits_for_the_disk`). So it
        // is made in the background before the job starts.
        let Some(folder) = (to.iter())
            .find(|t| (t.file_name()).is_some_and(|n| n.to_string_lossy().contains(NOT_ON_FAT)))
            .and_then(|t| t.parent())
            .map(Path::to_path_buf)
        else {
            return self.start_renaming(ctx, (listed, to), picked, undoing);
        };
        let held_back_files = (listed.iter().map(|f| f.path().to_path_buf()))
            .chain(to.iter().cloned())
            .collect();
        let (found_tx, found) = mpsc::channel();
        let names = to.clone();
        // No busy pointer: the look-up, whose answer `wait_for_look_up_before_the_job` holds
        // the pointer for. One given up ends when the share answers, or never.
        std::thread::spawn(move || {
            drop(found_tx.send(LookUpStep::Answered(refused_on_fat(
                &names,
                file_system_type_or_test,
            ))));
        });
        self.wait_for_look_up_before_the_job(
            folder,
            (held_back_files, true),
            found,
            move |app, ctx, refused| match refused {
                Some(refused) => app.notice.give(not_renamed(refused)),
                None => app.start_renaming(ctx, (listed, to), picked, undoing),
            },
        );
    }

    /// Starts the job of `rename_files_of`, once no new name is one that its file system
    /// refuses.
    fn start_renaming(
        &mut self,
        ctx: &egui::Context,
        (listed, to): (Vec<ListedFile>, Vec<PathBuf>),
        picked: PickedFiles,
        undoing: Option<UndoUntilStarted>,
    ) {
        let from: Vec<PathBuf> = listed.iter().map(|f| f.path().to_path_buf()).collect();
        // It is a job like every other writer. Closing waits for it, the status bar counts it,
        // and its Stop puts back the names done so far.
        let held_back_files = from.iter().chain(&to).cloned().collect();
        let Some(steps) = self.start_job(tr("Renaming"), from.len(), held_back_files) else {
            return;
        };
        let undoing = undoing.map(UndoUntilStarted::started).is_some();
        let shown = from.iter().position(|f| self.is_file(self.list.shown(), f));
        let job = RenameJob {
            folder: folder_of_all(&from),
            from: from.clone(),
            to: to.clone(),
            shown,
            undoing,
        };
        let (tx, rx) = answer_channel(&self.busy);
        let undo_on_the_way = self.file_operations.undo_on_the_way();
        // Its failures are told when the window takes the answer (`finish_rename_many`), and a
        // panic by `run_in_background`.
        self.run_in_background(ctx, Vec::new(), move || {
            #[cfg(test)]
            from.iter()
                .for_each(|p| crate::support::threads::panic_in_a_test_at(p));
            let (_, picked) = picked.wait_for_fingerprints();
            let _ = tx.send(rename_undoable((listed, to), (&picked, undoing), &steps));
            drop((undo_on_the_way, steps));
            None
        });
        self.file_operations.renaming = Some(job);
        self.file_operations.renaming_rx = Receiving::of(rx);
    }

    /// Brings the list along after Rename Many has run, with the shown image under its new
    /// name.
    pub(crate) fn finish_rename_many(
        &mut self,
        ctx: &egui::Context,
        job: RenameJob,
        answer: RenameOutcome,
    ) {
        let RenameOutcome {
            result,
            left_alone,
            undo,
            names_known,
            stopped,
        } = answer;
        let failed = result.is_err();
        // The files left alone are told by name, as an Undo of a move tells them.
        let other = match job.undoing {
            true => tr("another file has taken its place"),
            false => tr("the file was changed meanwhile"),
        };
        let mut errors: Vec<String> = (left_alone.iter())
            .map(|&k| format!("{}: {other}", job.from[k].display()))
            .collect();
        let told = if !errors.is_empty() {
            errors.extend(result.err());
            let (count, first) = (count(errors.len(), "error"), &errors[0]);
            Some(match job.undoing {
                true => tf!("Undo failed ({count}): {first}", count, first),
                false => tf!("Rename failed ({count}): {first}", count, first),
            })
        } else if stopped {
            // A Stop that put every name back is no failure, and a second close then closes.
            if let Err(text) = &result {
                self.notice.give(text.clone());
            }
            None
        } else {
            result.err()
        };
        self.set_selection(ctx, Default::default());
        // A rename that was itself an undo is not undone again, so Ctrl+Z twice does not give
        // the new names back. A failed Undo stays to be done again, unless an operation that
        // finished meanwhile left a newer Undo.
        if !undo.is_empty() && !(job.undoing && self.file_operations.undo.is_some()) {
            self.file_operations.undo = Some(Undo::Rename(undo));
        }
        // The files are found by their old names, because the list may have changed while they
        // were renamed. When nothing was renamed, the names stay.
        let pairs: Vec<(PathBuf, PathBuf)> = match failed {
            true => Vec::new(),
            false => (job.from.iter().zip(&job.to).enumerate())
                .filter(|(k, _)| left_alone.binary_search(k).is_err())
                .map(|(_, (f, t))| (f.clone(), t.clone()))
                .collect(),
        };
        // The writes asked for during the rename go first, as they were asked for before
        // anything that follows the files now. When a failed rename left names it does not
        // know, a write could land on another photo, so the writes are dropped.
        let dropped = self.send_waiting_sidecar_writes(ctx, names_known.then_some(&pairs[..]));
        let told = told
            .into_iter()
            .chain(dropped)
            .collect::<Vec<_>>()
            .join("\n");
        // Each text told here is a failure, which a close must not hide.
        if !told.is_empty() {
            self.tell_failed_write(told);
        }
        self.follow_moved_paths(pairs, false);
        if self.folder.collection().is_some() {
            return self.start_listing(ctx);
        }
        // When the image shown at the start is still shown (the user may have moved on), it
        // stays shown under its new name, with any turn not saved yet, while the list is read
        // again. It is read again rather than opened, so the view stays in the folder,
        // subfolders or folders it shows. The shown image is checked by its name, so this holds
        // also when the list changed while the files were renamed. Before, a list changed
        // meanwhile was only read again, and the shown image's old name, gone from it, moved
        // the view to another file.
        if let Some(k) = job.shown
            && !failed
            && left_alone.binary_search(&k).is_err()
            && self.is_file(self.list.shown(), &job.from[k])
            && let Ok(rel) = job.to[k].strip_prefix(self.folder.dir())
        {
            let shown = self.list.shown();
            self.rename_in_list(ctx, vec![(shown, rel.as_os_str().to_owned())]);
        }
        self.start_listing(ctx)
    }

    /// Writes the view's rotation into the shown file in the background, and then shows the
    /// file as it is. A JPEG keeps its pixels and gets an orientation field. PNG and WebP are
    /// turned, which takes seconds for a large one. Both read and write the whole file, which
    /// is slow on a share. Returns whether a save of the file is under way, started now or
    /// before, and `false` when there is no turn to save or the save was refused.
    pub(crate) fn save_orientation(&mut self, ctx: &egui::Context) -> bool {
        let (turns, mirrored) = self.viewing.turn();
        if self.list.files().is_empty() || (turns, mirrored) == (0, false) {
            return false;
        }
        let path = self.viewing.turned_file().to_path_buf();
        // When the file is already being saved with this rotation, the rotate keys wait for
        // the save.
        if self.file_operations.saving.contains(&path) {
            return true;
        }
        // The turn is not saved while other work changes the file. Rename Many may give its
        // name to another photo, and the turn would then be saved over that photo. A move may
        // take the file away.
        // It is told as not saved, so the turn goes with the image when the image is left. The
        // notice is kept until a frame draws it, because a close gives its own notice in the
        // same frame, and that one would hide it.
        if self.file_operations.renaming.is_some()
            || self.is_changed_by_running_work(std::slice::from_ref(&path))
        {
            let held = tr("The rotation was not saved: other work is still changing the file");
            self.notice.keep_until_drawn(held.into());
            return false;
        }
        let jpeg = crate::model::listing::is_jpeg(&path);
        // The turn is meant for the picture decoded and shown. A file that has taken its name
        // since, or was written anew, is not turned with it.
        let decoded = (self.list.loaded_images().values())
            .find(|loaded| loaded.path == path)
            .and_then(|loaded| loaded.read);
        let (rotate, quality) = (self.config.exif_rotate, self.config.webp_quality);
        let saved = self.file_operations.saved_tx.clone();
        self.file_operations.saving.insert(path.clone());
        self.run_in_background(ctx, Vec::new(), move || {
            // When another program removed or renamed the file meanwhile, there is nothing to
            // save the turn into, and the user is told that it was not saved. It is looked at
            // here, not in the window's thread, because it waits for the disk.
            let is_decoded = |now| decoded.is_none_or(|read| same_fingerprint(read, now));
            let result = catch_panic_as_error(|| match jpeg {
                // A look that failed is no answer, and the fingerprint below then tells it.
                _ if is_surely_gone(&path) => Err(tf!("{name} is gone", name = path.display())),
                _ if !fingerprint(&path).is_some_and(is_decoded) => {
                    Err(crate::files::file_io::changed_meanwhile().to_string())
                }
                true => exif::write_orientation(&path, turns, mirrored, rotate),
                false => save_rotated(&path, rotate, turns, mirrored, quality),
            });
            let told = (jpeg && result.is_ok() && !rotate).then(|| {
                Told::Notice(tr("Saved; it shows when Auto Rotate Using EXIF is on").into())
            });
            let _ = saved.send((path, result));
            told
        });
        true
    }

    /// Reads again what is shown of `paths`, because they have other contents now (saved
    /// turned, edited by a tool, written over). That covers the decoded images, thumbnails,
    /// size and date, EXIF and the `exif:` index.
    pub(crate) fn files_changed(&mut self, ctx: &egui::Context, paths: &[PathBuf]) {
        if paths.is_empty() {
            return;
        }
        for path in paths {
            if self.is_file(self.list.shown(), path) {
                self.viewing.drop_turn();
                self.list.forget_decodes_of(self.list.shown());
                self.read_info_again(ctx);
            }
        }
        self.forget_names(paths);
        // Glimt wrote them, so a new inode is no other file. Until they are listed again, they
        // are not told apart by identity.
        let names: HashSet<&std::ffi::OsStr> = (paths.iter())
            .filter_map(|p| Some(p.strip_prefix(self.folder.dir()).ok()?.as_os_str()))
            .collect();
        self.list.forget_identities(&names);
        // Their names stay in the list, so the Pan view's days are made again for their dates.
        if let Some(pan) = &mut self.pan_view {
            pan.make_days_again();
        }
        self.refill_preload();
    }

    /// Keeps `paths` as the files an Undo of a move or rename puts back, although Glimt wrote
    /// them anew in place (a turn saved, a tool's change) and so gave them new fingerprints. A
    /// file written over by another image (Save As, Save This Frame) is not one of them, so it does
    /// not come here.
    pub(crate) fn written_anew(&mut self, paths: &[PathBuf]) {
        if let Some(Undo::Move(pairs) | Undo::Rename(pairs)) = &mut self.file_operations.undo {
            let written: HashSet<&PathBuf> = paths.iter().collect();
            let real_path = |p: &PathBuf| {
                #[cfg(test)]
                REAL_PATHS_LOOKED_UP.set(REAL_PATHS_LOOKED_UP.get() + 1);
                p.canonicalize().ok()
            };
            // A folder opened through a link spells the same file otherwise, so a pair of the
            // same file name is compared by its real path too. Only the paths whose name both a
            // moved file and a file just written have are looked up. A tool's report of 200,000
            // files after a move of one, or one file written after a move of 500,000, so costs
            // no look at the disk for the others.
            let moved: HashSet<&std::ffi::OsStr> = (pairs.iter())
                .filter(|(_, to, _)| !written.contains(to))
                .filter_map(|(_, to, _)| to.file_name())
                .collect();
            // shortcut: the real paths are looked up in the window's thread, but only of the
            // files just written and the moved files that share a name with one, which the write
            // reached a moment ago; look them up with the write if a share is seen to hold the
            // window here.
            let (names, real): (HashSet<&std::ffi::OsStr>, HashSet<PathBuf>) = (paths.iter())
                .filter_map(|p| Some((p.file_name().filter(|n| moved.contains(n))?, p)))
                .filter_map(|(name, p)| Some((name, real_path(p)?)))
                .unzip();
            let same = |to: &PathBuf| {
                written.contains(to)
                    || (to.file_name().is_some_and(|n| names.contains(n))
                        && real_path(to).is_some_and(|t| real.contains(&t)))
            };
            // The file takes its new fingerprint, so a file put there later is still told apart.
            // One that is gone has nothing left to put back.
            pairs.retain_mut(|(_, to, old)| {
                !same(to) || fingerprint(to).map(|now| *old = now).is_some()
            });
        }
    }

    /// Forgets what Glimt keeps of `paths` by their names: their decoded images, thumbnails, size
    /// and date, Check Images' mark, their part of the `exif:` index and their date in the Pan
    /// view. A file of the same name later, renamed or moved there, or written anew, is then
    /// read for itself. Every operation that changes what a name holds calls this: a change in
    /// place, a move, a rename, a delete.
    pub(crate) fn forget_names(&mut self, paths: &[PathBuf]) {
        for path in paths {
            self.list.keep_loaded_images(|_, s| s.path != *path);
            if let Ok(name) = path.strip_prefix(self.folder.dir()) {
                let name = name.as_os_str();
                self.thumbs.forget(name);
                self.list.forget_size_and_date_of(name);
                self.broken_files.remove(name);
                // Also while the first index is being read, which may have read it before.
                self.search.read_exif_again([name.to_owned()]);
                if let Some(pan) = &mut self.pan_view {
                    pan.forget_date_of(name);
                }
            }
        }
    }

    /// Renames `folder_target` within its parent, never over another folder. The open folder,
    /// or one inside it, is followed to the new name.
    pub(crate) fn rename_folder(&mut self, ctx: &egui::Context, name: &str) {
        let Some(picked) = self.dialogs.take_picked_folder() else {
            return;
        };
        let Some(old) = picked.paths().first().cloned() else {
            return;
        };
        let new = old.with_file_name(name);
        if new == old {
            return;
        }
        // This runs in the background, because a folder on a share that stopped answering
        // would hold the window until the rename returns. What follows it comes back through
        // `report_tx`.
        let report_tx = self.file_operations.report_tx.clone();
        self.run_in_background(ctx, vec![old.clone()], move || {
            // A camera card refuses some characters, and its own error would not say which. The
            // look at its file system reads only the mount table, but that is a file, which the
            // window's thread never reads, so the look is made here.
            let new_name = std::slice::from_ref(&new);
            if let Some(refused) = refused_on_fat(new_name, file_system_type_or_test) {
                return Some(Told::Notice(not_renamed(refused)));
            }
            // Another folder put at its path while the dialog was open keeps its name.
            let (_, picked) = picked.wait_for_fingerprints();
            if let Err(e) = picked.still_the_one_picked(&old) {
                // The tree and the list show the folder as it was, so they are read again.
                let _ = report_tx.send(OperationReport {
                    read_folder_again: true,
                    ..Default::default()
                });
                return Some(Told::FailedWrite(format!("{}: {e}", old.display())));
            }
            // Never over an empty folder of that name, also one made since the look.
            if let Err(e) = rename_never_over(&old, &new) {
                return Some(Told::FailedWrite(format!("{}: {e}", new.display())));
            }
            // The marks and collections follow the new name, so it is on disk first. A failed
            // sync is told, but the window still follows the folder, which has its new name.
            let synced = crate::files::file_io::sync_folder_of(&new);
            let error = synced
                .err()
                .map(|e| Told::FailedWrite(format!("{}: {e}", new.display())));
            let _ = report_tx.send(OperationReport {
                folder_renamed: Some((old, new)),
                ..Default::default()
            });
            error
        });
    }

    /// Makes the window follow the folder `old`, which was renamed to `new`.
    fn folder_renamed(&mut self, ctx: &egui::Context, old: PathBuf, new: PathBuf) {
        self.tree.forget(&old);
        // When several folders are shown together, the renamed one is read under its new
        // name. In a collection, its own file follows the folder it is in, and its images in
        // the folder follow it to the new name, in its file too.
        self.folder.follow_rename(&old, &new);
        self.follow_moved_paths(vec![(old.clone(), new.clone())], false);
        #[cfg(feature = "marks")]
        self.marks.follow_folder(old.clone(), new.clone());
        match self.folder.dir_renamed(&old, &new) {
            // The shown file is under the new name now, also for a turn not saved yet.
            Some(dir) => self.visit(ctx, dir),
            None => self.start_listing(ctx),
        }
        // The collection's file or a folder shown together may have moved with the folder, and
        // the watcher looks at the old name.
        if self.folder.collection().is_some() || !self.folder.folders().is_empty() {
            self.start_watching(ctx);
        }
    }

    /// Moves `folder_target` and everything in it to the trash, in the background. When the
    /// open folder is it or inside it, the window first goes to the folder above.
    pub(crate) fn trash_folder(&mut self, ctx: &egui::Context) {
        let Some(picked) = self.dialogs.take_picked_folder() else {
            return;
        };
        let Some(dir) = picked.paths().first().cloned() else {
            return;
        };
        if !self.are_free_to_change(std::slice::from_ref(&dir)) {
            return;
        }
        self.tree.forget(&dir);
        self.folder.forget_subdir(&dir);
        if self.folder.dir().starts_with(&dir)
            && let Some(parent) = dir.parent()
        {
            self.visit(ctx, parent.to_path_buf());
        }
        let report_tx = self.file_operations.report_tx.clone();
        let collection = self.folder.collection().map(Path::to_path_buf);
        let undo_on_the_way = self.file_operations.undo_on_the_way();
        self.run_in_background(ctx, vec![dir.clone()], move || {
            let since = std::time::SystemTime::now();
            // Another folder put at its path while the question was open stays.
            let (_, picked) = picked.wait_for_fingerprints();
            let error = match picked.still_the_one_picked(&dir) {
                Ok(()) => crate::files::trash::trash(std::slice::from_ref(&dir)).err(),
                Err(e) => Some(format!("{}: {e}", dir.display())),
            };
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
                Undo::Trash(TrashMove {
                    files: gone.clone(),
                    sidecars: Vec::new(),
                    sidecars_left: Vec::new(),
                    since,
                    marks: Default::default(),
                    collection: collection.zip(Some(removed)),
                })
            });
            let _ = report_tx.send(OperationReport {
                requested: vec![dir],
                gone,
                any_failed: error.is_some(),
                undo,
                ..Default::default()
            });
            drop(undo_on_the_way);
            error
                .map(|e| tf!("Could not move the folder to the trash: {e}", e))
                .or(forgotten)
                .map(Told::FailedWrite)
        });
    }

    /// Edit → Undo (Ctrl+Z). The last files or folder moved to the trash come back from it, the
    /// last files moved go back, and the last files renamed get their old names. It works once,
    /// so what it put back is not undone again. Nothing is put over a file that has taken the
    /// name meanwhile.
    pub(crate) fn undo(&mut self, ctx: &egui::Context) {
        // A delete or move that is still running leaves a newer Undo when it ends. Taking the
        // older one now would undo an operation the user did not mean. A report sent but not yet
        // taken is taken first, so its Undo is the one done.
        self.receive_operation_report(ctx);
        self.receive_renames(ctx);
        if Arc::strong_count(&self.file_operations.undo_on_the_way) > 1 {
            self.notice
                .give(tr("Still working on these files: try again in a moment").into());
            return;
        }
        // This asks before the undo is taken, so one that is refused stays to be done again.
        // So do the files of a move to the trash that did not come back, which its thread
        // sends back.
        let undone: Vec<PathBuf> = match &self.file_operations.undo {
            Some(Undo::Move(pairs)) => (pairs.iter())
                .flat_map(|(from, to, _)| [from.clone(), to.clone()])
                .collect(),
            Some(Undo::Trash(trash_move)) => trash_move.files.clone(),
            Some(Undo::Rename(pairs)) => (pairs.iter())
                .flat_map(|(from, to, _)| [from.clone(), to.clone()])
                .collect(),
            None => Vec::new(),
        };
        if !self.are_free_to_change(&undone) {
            return;
        }
        let Some(undo) = self.file_operations.undo.take() else {
            self.notice.give(tr("Nothing to undo").into());
            return;
        };
        let report_tx = self.file_operations.report_tx.clone();
        match undo {
            Undo::Rename(pairs) => {
                let (mut recorded, mut before) = (Vec::new(), Vec::new());
                for (from, to, fingerprint) in pairs.clone() {
                    before.push(from);
                    recorded.push((to, fingerprint));
                }
                let now = (recorded.iter())
                    .map(|(to, _)| ListedFile::not_from_the_list(to.clone()))
                    .collect();
                let picked = PickedFiles::recorded(recorded);
                let undo = UndoUntilStarted {
                    undo: Some(Undo::Rename(pairs)),
                    report_tx,
                };
                self.rename_files_of(ctx, (now, before), picked, Some(undo));
            }
            Undo::Move(pairs) => drop(self.run_in_background(ctx, undone, move || {
                let (back, left, errors) = move_back(pairs);
                let files = count(back.len(), "file");
                // Those that did not go back stay to be undone again, as the trash's do.
                let _ = report_tx.send(OperationReport {
                    moved: back,
                    read_folder_again: true,
                    undo: (!left.is_empty()).then_some(Undo::Move(left)),
                    is_undo_remainder: true,
                    ..Default::default()
                });
                Some(match errors.first() {
                    Some(first) => Told::FailedWrite(tf!(
                        "Undo failed ({count}): {first}",
                        count = count(errors.len(), "error"),
                        first
                    )),
                    None => Told::Notice(tf!("{files} moved back", files)),
                })
            })),
            Undo::Trash(trash_move) => self.undo_trash(ctx, trash_move, undone),
        }
        // A folder that came back shows in the tree again.
        self.tree.read_again();
    }

    /// Puts back what `trash_move` moved to the trash, as `undo` says. The marks writer must
    /// first have taken the files' marks out of the marks file, which may take long, so the
    /// status bar shows the Undo with Stop meanwhile. A stopped Undo puts nothing back and stays
    /// to be done again. `undone` are the files it holds back.
    fn undo_trash(&mut self, ctx: &egui::Context, trash_move: TrashMove, undone: Vec<PathBuf>) {
        let report_tx = self.file_operations.report_tx.clone();
        let Some(job) = self.start_job(tr("Undo"), trash_move.files.len(), Vec::new()) else {
            return;
        };
        self.run_in_background(ctx, undone, move || {
            let Some(all_marks) =
                crate::files::trash::take_forgotten_marks(&trash_move.marks, || job.is_stopped())
            else {
                let _ = report_tx.send(OperationReport {
                    undo: Some(Undo::Trash(trash_move)),
                    is_undo_remainder: true,
                    ..Default::default()
                });
                return Some(Told::Notice(job.stopped_text()));
            };
            let (back, mut error) = restore(&trash_move, trash_dirs);
            // What went with the files comes back with them: their places in the
            // collection, their marks and their stars.
            let set: HashSet<&Path> = back.iter().map(PathBuf::as_path).collect();
            let returned = |p: &Path| p.ancestors().any(|a| set.contains(a));
            if let Some((file, removed)) = &trash_move.collection {
                let removed: Vec<_> = (removed.iter())
                    .filter(|(_, p)| returned(p))
                    .cloned()
                    .collect();
                if let Err(e) =
                    collections::edit_open_collection(file, |all| put_back(all, &removed))
                {
                    error.get_or_insert(e);
                }
            }
            // Those not put back (a name taken, a rename that failed) stay to be undone
            // again, with their marks and places, which would be lost with this Undo.
            let (marks, kept) = all_marks.into_iter().partition(|(p, _)| returned(p));
            let left: Vec<PathBuf> = (trash_move.files.into_iter())
                .filter(|p| !returned(p))
                .collect();
            let undo = (!left.is_empty()).then(|| {
                Undo::Trash(TrashMove {
                    files: left,
                    sidecars: trash_move.sidecars,
                    sidecars_left: trash_move.sidecars_left,
                    since: trash_move.since,
                    marks: std::sync::Arc::new(std::sync::Mutex::new(kept)),
                    collection: (trash_move.collection).map(|(file, removed)| {
                        (
                            file,
                            removed.into_iter().filter(|(_, p)| !returned(p)).collect(),
                        )
                    }),
                })
            });
            let _ = report_tx.send(OperationReport {
                read_folder_again: true,
                undo,
                is_undo_remainder: true,
                restored_marks: marks,
                ..Default::default()
            });
            let done = tf!(
                "{count} back from the trash",
                count = count(back.len(), "file")
            );
            // The files that came back are told too when others did not.
            Some(match error {
                None => Told::Notice(done),
                Some(e) if back.is_empty() => Told::FailedWrite(tf!("Undo failed: {e}", e)),
                Some(e) => Told::FailedWrite(format!("{done}; {}", tf!("Undo failed: {e}", e))),
            })
        });
    }

    // ---- Work in the background: sidecars, frames, the backup and other jobs ----

    /// Holds `paths` back for work that is not a job, once they are free to change. Other
    /// operations on them wait while the returned `HeldBackFiles` lives, so the work moves it into
    /// its thread. Returns `None`, with a notice, while something else works on them. Work on none
    /// of the user's files (an empty `paths`) is never held back and holds nothing.
    pub(crate) fn hold_back_while_writing(&mut self, paths: Vec<PathBuf>) -> Option<HeldBackFiles> {
        if !paths.is_empty() && !self.are_free_to_change(&paths) {
            return None;
        }
        let held = Arc::new(Mutex::new(paths));
        self.file_operations.held_back_files.push(held.clone());
        Some(HeldBackFiles { paths: held })
    }

    /// Runs `work`, which writes files and changes `held_back_files`, in a thread of its own. The
    /// busy pointer shows meanwhile, closing waits for it, and what it returns is told in the
    /// window. The work says whether its answer is a notice or a failed write, because a failed
    /// write keeps a close that waited for it open, so the user sees it. It is not started, and a
    /// notice says so, while something else works on `held_back_files` (`are_free_to_change`).
    /// Returns whether it started. Work within a job passes no `held_back_files`, because its
    /// job holds them.
    pub(crate) fn run_in_background(
        &mut self,
        ctx: &egui::Context,
        held_back_files: Vec<PathBuf>,
        work: impl FnOnce() -> Option<Told> + Send + 'static,
    ) -> bool {
        // Work that changes none of the user's files (within a job that holds them, or on
        // Glimt's own collections) waits for nothing. A Lightroom import's collections were
        // refused while its own keywords were being written.
        let Some(held_back) = self.hold_back_while_writing(held_back_files) else {
            return false;
        };
        let pointer = Working::new(&self.busy);
        let writing = Working::new(&crate::files::file_io::WRITING);
        // The busy pointer goes after the files held back. A window that sees no work running
        // starts the next operation on these files, and the files still held back would refuse
        // it. A panic in `work` is told as a failed write (`told_work`).
        let held = (writing, held_back, pointer);
        std::thread::spawn(told_work(self.notice_sender.clone(), ctx, held, work));
        true
    }

    /// Writes the sidecars of `paths` in the background. This window's keywords, stars and
    /// Lightroom import all go through one thread (`sidecar_writer`), so changes to one file
    /// land in the order they were made and never cross. The status bar counts them, and
    /// `report` words the end from how many failed. A failure that `report` gives no words
    /// is told by the text that `write` returned for it, the first of them, so `write` itself
    /// tells nothing. The thread is its own rather than
    /// `file_io::write_later`'s, so settings saved meanwhile do not wait behind 3,000 sidecars.
    ///
    /// Every sidecar write of the user's metadata goes through here, and this is the one rule
    /// for when it runs beside other work on its files. It is never refused, because an edit
    /// would be lost. Other work refuses to start while it runs (`are_free_to_change`). Work
    /// that already runs takes each sidecar under its lock, so a write that comes later finds
    /// the file gone and says so. Only a rename puts another photo under a name, as Rename Many
    /// does when it swaps names. A write asked for while one runs therefore waits until it is
    /// over, and is then sent to the names its files have (`send_waiting_sidecar_writes`).
    /// Stop leaves a write of one file, which is an edit the user made (`stop_jobs`).
    ///
    /// Each file is checked right before its write, under its sidecar's lock, to be the one the
    /// list showed under its name (`xmp::write_for`). One that another file has taken the name
    /// of is left alone, told, and counted as failed.
    ///
    /// Each file in `files` comes with what is written to it, and `write` is given both. So
    /// `write` never looks up its data by the path it is given: after a rename, that path may
    /// be another photo's old name, and the data found under it would be that photo's. Before,
    /// Lightroom's import did, and put each photo's keywords and stars on the other photo of a
    /// swap.
    pub(crate) fn write_sidecars<T: Send + 'static>(
        &mut self,
        ctx: &egui::Context,
        activity: &'static str,
        files: Vec<(ListedFile, T)>,
        write: impl Fn(&Path, &T) -> Result<(), String> + Send + 'static,
        report: impl FnOnce(usize, &Job) -> Option<String> + Send + 'static,
    ) {
        let (files, data): (Vec<ListedFile>, Vec<T>) = files.into_iter().unzip();
        let write = move |k: usize, path: &Path| {
            let d = data.get(k).ok_or_else(broke_here)?;
            write(path, d)
        };
        if self.file_operations.renaming.is_some() {
            let waiting = WaitingSidecarWrite {
                activity,
                files,
                write: Box::new(write),
                report: Box::new(report),
                _writing: Working::new(&crate::files::file_io::WRITING),
            };
            return self.file_operations.sidecar_writes_waiting.push(waiting);
        }
        let paths: Vec<PathBuf> = files.iter().map(|f| f.path().to_path_buf()).collect();
        let Some(job) = self.start_job_of(activity, paths.len(), true, paths.clone()) else {
            return;
        };
        // "exif:" searches find keywords, comments and stars too, so these files are read
        // again. That holds also when the first index is still being read, because it may have
        // read them before this write.
        let names = paths
            .iter()
            .filter_map(|p| p.strip_prefix(self.folder.dir()).ok());
        (self.search).read_exif_again(names.map(|n| n.as_os_str().to_owned()));
        // The job goes after its failure is told, so that a close that waited for it takes the
        // failure first.
        let held = (job.clone(), Working::new(&self.busy));
        self.write_in_sidecar_thread(ctx, held, move || {
            let (mut failed, mut refused, mut first_failure) = (0, None, None);
            for (k, file) in files.iter().enumerate() {
                if job.is_stopped() {
                    break;
                }
                // A file that another file has taken the name of since the list was read is
                // left alone: `write_for` has it checked under the sidecar's lock, and the first
                // refusal is told. A file that makes the writing panic counts as one that
                // failed, and the files after it are still written.
                let written = catch_panic(|| xmp::write_for(file, || write(k, file.path())));
                match written.unwrap_or_else(|| (Err(broke_here()), None)) {
                    (_, Some(refusal)) => {
                        failed += 1;
                        refused.get_or_insert(refusal);
                    }
                    (Err(e), None) => {
                        failed += 1;
                        first_failure.get_or_insert(e);
                    }
                    (Ok(()), None) => {}
                }
                job.count_done(1);
            }
            if let Some(first) = refused {
                crate::files::file_io::save_failed(first);
            }
            let told = report(failed, &job).or(first_failure);
            told.map(|text| match failed > 0 {
                true => Told::FailedWrite(text),
                false => Told::Notice(text),
            })
        });
    }

    /// Sends `work` to the thread of this window's sidecar writes (`sidecar_writer`), to be done
    /// after the writes sent before it. The work runs through `told_work`, which tells what it
    /// returns and a panic in it, and keeps what it `held` until it has told. So no work reaches
    /// that thread that could end with nothing told. When the send fails, the work is dropped,
    /// and what it held goes with it.
    fn write_in_sidecar_thread(
        &self,
        ctx: &egui::Context,
        held: impl Send + 'static,
        work: impl FnOnce() -> Option<Told> + Send + 'static,
    ) {
        let work = told_work(self.notice_sender.clone(), ctx, held, work);
        let _ = sidecar_writer().send(Box::new(work));
    }

    /// Sends the sidecar writes that waited for a rename to the names their files have now, in
    /// the order they were asked for. `renamed` holds the `(from, to)` names that the rename
    /// changed. When it is `None`, the rename stopped without saying which names it changed.
    /// A write could then land on another photo, so the writes are dropped. Returns the text
    /// that tells the user so, or `None` when no write was dropped.
    #[must_use]
    pub(crate) fn send_waiting_sidecar_writes(
        &mut self,
        ctx: &egui::Context,
        renamed: Option<&[(PathBuf, PathBuf)]>,
    ) -> Option<String> {
        let waiting = std::mem::take(&mut self.file_operations.sidecar_writes_waiting);
        let Some(renamed) = renamed else {
            let files: usize = waiting.iter().map(|w| w.files.len()).sum();
            return (files > 0).then(|| {
                tf!(
                    "Rename Many stopped, so the metadata edited for {count} was not saved",
                    count = count(files, "file")
                )
            });
        };
        let moved = moves_by_source(renamed);
        // Each file keeps its place in the write, by which the write finds what it writes, and
        // its identity, which a rename keeps.
        for waiting in waiting {
            let files = (waiting.files.iter().enumerate())
                .map(|(k, f)| {
                    let path = new_place(f.path(), &moved).unwrap_or_else(|| f.path().into());
                    (ListedFile::new(path, f.identity(), f.list_is_old()), k)
                })
                .collect();
            let write = waiting.write;
            let write = move |path: &Path, &k: &usize| write(k, path);
            self.write_sidecars(ctx, waiting.activity, files, write, waiting.report);
        }
        None
    }

    /// Save frame `only` of `path` as the PNG `to`, or every frame as PNG files in a new folder
    /// in `to` (never over a file), in the background, at full size. Each is turned as `how`,
    /// the view's copy when the action started, says. One frame replaces only the file whose
    /// fingerprint the chooser took at the name, `at_pick`, and none when nothing was there.
    pub(crate) fn export_frames(
        &mut self,
        ctx: &egui::Context,
        path: PathBuf,
        only: Option<usize>,
        (to, at_pick): (PathBuf, Option<FileFingerprint>),
        how: crate::images::save::CopyAs,
    ) {
        let rotate = how.rotate;
        // A job, so closing waits for it (or stops it between frames), as for copies.
        let Some(job) = self.start_job(tr("Exporting"), 1, vec![to.clone()]) else {
            return;
        };
        let report_tx = self.file_operations.report_tx.clone();
        self.run_in_background(ctx, Vec::new(), move || {
            // One frame saved over the animation itself would put a still in its place and take
            // the animation's sidecar, marks and stars with it. It is refused, as Save As
            // refuses it. The check is made here in the background, because a file on a share
            // that stalls would hold the window.
            if only.is_some() && crate::images::save::same_file(&path, &to) {
                let e = tr("choose another name: the image itself is not changed");
                return Some(Told::FailedWrite(tf!("Could not save the image: {e}", e)));
            }
            // The name's own bytes, also when they are not UTF-8.
            let named = |more: &str| crate::images::save::stem_with_suffix(&path, more);
            // The frames go into a folder made new for them. A name that is taken, also by a
            // folder another export made a moment ago or by a link, is passed over, so two
            // exports never write into the same folder.
            #[allow(clippy::expect_used)] // find on an endless range of names ends with a name
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
            // A Stop, also that of a second close, is told as a stop and not as a failed write,
            // which would keep the window open after the user asked it to close.
            let stopped = std::sync::atomic::AtomicBool::new(false);
            let saved = if made.is_err() {
                Err(tf!("Could not make {folder}", folder = folder.display()))
            } else {
                decode::catch_panic_as_error(|| {
                    decode::each_frame(&path, only, rotate, |k, img| {
                        if job.is_stopped() {
                            stopped.store(true, Ordering::Relaxed);
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
                        crate::images::save::frame_png(img, &how)
                            .and_then(|png| {
                                let replace = at_pick.filter(|_| only.is_some());
                                crate::images::save::save_copy_with_sidecar(
                                    &path, &file, &png, replace, true,
                                )
                                .map_err(|e| e.to_string())
                            })
                            .map_err(|e| format!("{}: {e}", file.display()))
                    })
                })
            };
            // One frame saved over a file the dialog let it replace, maybe one in the list.
            if only.is_some() && saved.as_ref().is_ok_and(|&n| n > 0) {
                let _ = report_tx.send(OperationReport {
                    overwritten: vec![to.clone()],
                    ..Default::default()
                });
            }
            Some(match (saved, only) {
                (Ok(0), _) => Told::Notice(tr("No such frame").to_owned()),
                (Ok(_), Some(_)) => Told::Notice(tf!("Saved {file}", file = to.display())),
                (Ok(n), None) => Told::Notice(tf!(
                    "{frames} saved in {folder}",
                    frames = count(n, "frame"),
                    folder = folder.display()
                )),
                (Err(e), _) if stopped.load(Ordering::Relaxed) => Told::Notice(e),
                (Err(e), _) => Told::FailedWrite(tf!("Could not save the frames: {e}", e)),
            })
        });
    }

    /// Copies Glimt's own folders (settings, marks and stars, collections, keyword tree,
    /// tools, added formats) to a new folder glimt-backup-DATE in `dir`, in the background. To
    /// restore them, copy them back while Glimt is closed. The README says where.
    pub(crate) fn back_up(&mut self, ctx: &egui::Context, dir: PathBuf) {
        // A job, so closing waits for the copy to be whole.
        let Some(job) = self.start_job(tr("Backing up"), 1, Vec::new()) else {
            return;
        };
        self.run_in_background(ctx, Vec::new(), move || {
            let told = match crate::files::backup::back_up_to(&dir) {
                Ok(done) => Told::Notice(done),
                Err(e) => Told::FailedWrite(e),
            };
            drop(job);
            Some(told)
        });
    }

    /// Copy or Move. It copies or moves the targets to a folder that the file chooser asks
    /// for.
    pub(crate) fn pick_copy_or_move(&mut self, ctx: &egui::Context, action: Action) {
        let Some(targets) = self.targets() else {
            return;
        };
        let paths = PickedFiles::listed(targets.iter().map(|&i| self.listed_file(i)).collect());
        let purpose = match action {
            Action::CopyTo => Pick::CopyTo(paths),
            _ => Pick::MoveTo(paths),
        };
        self.pick(ctx, true, purpose);
    }

    /// File → Set as Wallpaper. It sets the shown image through the desktop portal, in the
    /// background.
    pub(crate) fn set_wallpaper(&mut self) {
        let (path, tx) = (self.path(self.list.shown()), self.notice_sender.clone());
        // No busy pointer: the desktop may ask the user first, which may take long. A
        // failure is told.
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
        let Some(targets) = self.targets() else {
            return;
        };
        let paths: Vec<String> = targets
            .iter()
            .map(|&i| self.path(i).to_string_lossy().into_owned())
            .map(|p| if quote { quoted_for_shell(&p) } else { p })
            .collect();
        ctx.copy_text(paths.join(" "));
    }

    /// Rename. It renames the one file selected, as Delete and Move take it (right-clicked in
    /// the list, say), not the image shown beside it. Only its name is asked for, without the
    /// folders that a list with subfolders gives it.
    pub(crate) fn start_rename(&mut self, ctx: &egui::Context) {
        // The first of the targets, as Delete and Move take them: the file selected
        // (right-clicked in the list, say), not the image shown beside it, which is renamed
        // only when nothing is selected. It is shown, so the user sees what is renamed.
        let Some(targets) = self.targets() else {
            return;
        };
        let Some(&i) = targets.first() else {
            return;
        };
        self.show_file(ctx, i);
        // Only the name, because in the list with subfolders the entry has its folders too.
        if let Some(name) = (self.list.files().get(i))
            .and_then(|f| Path::new(f).file_name())
            .map(|n| n.to_string_lossy().into_owned())
        {
            // The file named, alone. The answer renames it wherever the list has put it since,
            // or nothing, never another selected file that came first meanwhile.
            let asked = self.ask_about_files(&[i], None);
            self.dialogs.ask_about(asked, Prompt::Rename, name);
        }
    }
}

/// Returns `path` in double quotes, as Geeqie's Copy Path gives it, with a backslash before each
/// `$`, `` ` ``, `"` and `\`. A shell still reads those inside double quotes, so a file name
/// such as `$(command).jpg` would run its command when the path is pasted into a terminal. A `!`
/// is put in single quotes between the double-quoted parts, because an interactive bash or zsh
/// expands it to an earlier command line inside double quotes, and a backslash there would stay
/// in the name.
fn quoted_for_shell(path: &str) -> String {
    let mut quoted = String::from('"');
    for c in path.chars() {
        if c == '!' {
            quoted.push_str("\"'!'\"");
            continue;
        }
        if matches!(c, '$' | '`' | '"' | '\\') {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    quoted.push('"');
    quoted
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

/// Returns the text that tells of a file that broke the work on it (a panic caught), whose
/// change may be half done.
fn broke_here() -> String {
    tr("A change could not be saved, because Glimt met an error while writing it").to_owned()
}

/// Takes the files or folders `gone` for good out of the `collection` shown. This happens only
/// once they are gone, in the thread that knows, so the file keeps its order and never lacks a
/// file that is still there. It is the one place for Delete and Move Folder to Trash, and for a
/// delete that Glimt was killed during, at the next start. A plain list is the user's own and
/// is never written. Returns what it took out, each entry with its place, for Undo to put back.
pub(crate) fn forget_in_collection(
    collection: Option<&Path>,
    gone: &[PathBuf],
) -> Result<Vec<(usize, PathBuf)>, String> {
    let Some(file) = collection.filter(|_| !gone.is_empty()) else {
        return Ok(Vec::new());
    };
    // A rename or move that follows its files into the collection may still wait in the writer
    // thread. Otherwise a file renamed and then deleted at once would not be found under its
    // new name here, and the writer would then write that name of a trashed file.
    crate::files::file_io::wait_for_queued_writes();
    // Each entry and the folders above it are looked up, rather than every gone file for each
    // entry, because 10,000 deleted from a collection of 50,000 were half a billion
    // comparisons.
    let gone: HashSet<&Path> = gone.iter().map(PathBuf::as_path).collect();
    let mut removed = Vec::new();
    let edit = collections::edit_open_collection(file, |all| {
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
/// the trash failed in a local folder that cannot be written. `file_system_type` gives a folder's
/// file system, and it is asked once for each folder.
fn left_by_trash(
    paths: &[PathBuf],
    file_system_type: impl Fn(&Path) -> Option<String>,
) -> Option<(String, Vec<PathBuf>)> {
    use crate::files::file_io::is_remote;
    let mut kinds: HashMap<PathBuf, Option<String>> = HashMap::new();
    let mut remote = |p: &Path| {
        let dir = p.parent()?.to_path_buf();
        (kinds.entry(dir))
            .or_insert_with_key(|dir| file_system_type(dir).filter(|k| is_remote(k)))
            .clone()
    };
    let left: Vec<PathBuf> = (paths.iter())
        .filter(|p| !is_surely_gone(p) && remote(p).is_some())
        .cloned()
        .collect();
    Some((remote(left.first()?)?, left))
}

/// Returns the files of `refused` that the trash left on a network or cloud mount, with the
/// mount's type (`left_by_trash`), each with the fingerprint it had when it was `picked`, so that
/// the question to delete it for good acts on that file only. Returns `None` when there are none.
fn left_by_trash_as_picked(
    refused: &[PathBuf],
    picked: &PickedFingerprints,
) -> Option<(String, Vec<PickedAs>)> {
    let (kind, left) = left_by_trash(refused, crate::files::file_io::file_system_type)?;
    let as_picked = |p: PathBuf| {
        let picked_as = picked.picked_as(&p)?;
        Some((p, picked_as))
    };
    Some((kind, left.into_iter().filter_map(as_picked).collect()))
}

/// Deletes `paths` for `delete_now`'s thread, to the trash or for good as `how` says, in chunks
/// until `job` is stopped. Each chunk is written down in the delete's log, when there is one,
/// before its files go, so that the next start forgets the marks of those that went when Glimt
/// is killed before the report. Each failure is added to `errors`. Returns the files gone, the
/// sidecars that went with them, and the files left alone because they are no longer the ones
/// `picked`.
fn delete_in_chunks(
    paths: &[PathBuf],
    picked: &PickedFingerprints,
    (trash, mut delete_log): (bool, Option<&mut journal::DeleteLog>),
    job: &Job,
    errors: &mut Vec<String>,
) -> (Vec<PathBuf>, Vec<PathBuf>, HashSet<PathBuf>) {
    let (mut gone, mut sides, mut changed) = (Vec::new(), Vec::new(), HashSet::new());
    // Each gio call (and each taking of the locks) handles many files, so thousands of files do
    // not become thousands of processes. The chunks are small enough that Stop and the count in
    // the status bar keep up, and each holds only a few of the sidecar locks, which a gio call
    // that hangs on a share would hold for good.
    for chunk in xmp::chunks_by_sidecar_lock(paths, 100) {
        let chunk = &chunk[..];
        if job.is_stopped() {
            break;
        }
        if let Some(log) = delete_log.as_deref_mut() {
            log.note(chunk);
        }
        // A file that breaks the work costs its chunk, not the report of the chunks before it,
        // which the Undo and the marks need. What of the chunk went is still reported.
        let log = delete_log.as_deref_mut();
        let (went, side, left_alone) =
            catch_panic(|| delete_chunk(chunk, picked, (trash, log), errors)).unwrap_or_else(
                || {
                    errors.push(format!("{}: {}", chunk[0].display(), broke_here()));
                    let went: Vec<PathBuf> = (chunk.iter())
                        .filter(|p| is_surely_gone(p))
                        .cloned()
                        .collect();
                    let side = xmp::sidecars_of(&went);
                    (went, side, Vec::new())
                },
            );
        gone.extend(went);
        sides.extend(side);
        changed.extend(left_alone);
        job.count_done(chunk.len());
    }
    (gone, sides, changed)
}

/// Returns what a job tells, `told`, with the `warning` added on a line of its own. The warning
/// alone is a notice.
fn with_warning(told: Option<Told>, warning: Option<String>) -> Option<Told> {
    let Some(warning) = warning else {
        return told;
    };
    Some(match told {
        Some(Told::FailedWrite(text)) => Told::FailedWrite(format!("{text}\n{warning}")),
        Some(Told::Notice(text)) => Told::Notice(format!("{text}\n{warning}")),
        None => Told::Notice(warning),
    })
}

/// Deletes the files of `chunk` that are still the ones `picked`, to the trash when `trash` or
/// for good, and then their sidecars, for `delete_now`'s thread. A delete for good notes the
/// sidecars it puts aside in the delete's `log`, when there is one. Each failure is added to
/// `errors`. Returns the files gone, their sidecars, and the files left alone because they are
/// no longer the ones picked.
fn delete_chunk(
    chunk: &[PathBuf],
    picked: &PickedFingerprints,
    (trash, log): (bool, Option<&mut journal::DeleteLog>),
    errors: &mut Vec<String>,
) -> (Vec<PathBuf>, Vec<PathBuf>, Vec<PathBuf>) {
    #[cfg(test)]
    chunk
        .iter()
        .for_each(|p| crate::support::threads::panic_in_a_test_at(p));
    // A chunk's sidecars are held until they are gone too. Otherwise a keyword written
    // meanwhile would make a sidecar under the name of a photo that is no longer there. They are
    // held only for the chunk, so a sidecar write in another window waits for one chunk rather
    // than the whole job.
    let _locks = xmp::lock_sidecars(chunk);
    if !trash {
        return delete_for_good(chunk, picked, log, errors);
    }
    let since = std::time::SystemTime::now();
    // Only the files that are still the ones picked go, each looked at just before. Another
    // program or window may have given the name to another photo while the question was open.
    let (mut kept, mut left_alone) = (Vec::with_capacity(chunk.len()), Vec::new());
    for p in chunk {
        match picked.still_the_one_picked(p) {
            Ok(()) => kept.push(p.clone()),
            Err(e) => {
                errors.push(format!("{display}: {e}", display = p.display()));
                left_alone.push(p.clone());
            }
        }
    }
    // Their sidecars go the same way, so that a new file with the same name does not get their
    // keywords. Each file's sidecars are named before it goes and go just before it, in the same
    // call, so that Glimt killed halfway never leaves a sidecar at the name of a photo that is
    // gone. Adobe's sidecar of two RAW files of one name stem goes with the last of them, as in
    // `put_files`, so that one of them that stays can take it back below.
    let leaving: HashSet<&PathBuf> = kept.iter().collect();
    let none_leaving = HashSet::new();
    let mut stems_to_come = raw_files_by_stem(kept.iter());
    let (mut names, mut named) = (xmp::AdobeNames::default(), HashSet::new());
    let sidecars_of_file: Vec<(PathBuf, Vec<PathBuf>)> = (kept.iter())
        .map(|p| {
            let leaving = match is_another_of_its_stem_to_come(p, &mut stems_to_come) {
                true => &none_leaving,
                false => &leaving,
            };
            let sides = xmp::sidecars_moving_with(p, p, leaving, &mut names);
            let sides = sides.into_iter().map(|(side, _)| side);
            let sides = sides.filter(|s| named.insert(s.clone())).collect();
            (p.clone(), sides)
        })
        .collect();
    let in_order: Vec<PathBuf> = (sidecars_of_file.iter())
        .flat_map(|(p, sides)| sides.iter().chain([p]))
        .cloned()
        .collect();
    let trashed = match in_order.is_empty() {
        true => Ok(()),
        false => crate::files::trash::trash(&in_order),
    };
    // A look that failed (a share that dropped for a moment) counts the photo as there.
    let (mut went_with, mut stayed): (Vec<_>, Vec<_>) = (sidecars_of_file.into_iter())
        .partition(|(p, _): &(PathBuf, Vec<PathBuf>)| is_surely_gone(p));
    if trashed.is_err() {
        give_adobe_sidecars_to_those_that_stayed(&mut went_with, &mut stayed);
    }
    let mut went: Vec<PathBuf> = went_with.iter().map(|(p, _)| p.clone()).collect();
    let side: Vec<PathBuf> = (went_with.iter().flat_map(|(_, sides)| sides))
        .cloned()
        .collect();
    // A sidecar that stays is told, as the photo's own errors are, because the next file of that
    // name would show its keywords.
    if let Err(e) = trashed {
        take_back_sidecars(stayed, since, errors);
        let back = take_back_from_the_trash(went_with, since, errors);
        went.retain(|p| !back.contains(p));
        errors.push(e);
    }
    (went, side, left_alone)
}

/// Moves Adobe's sidecar of a name stem, which went to the trash with the last RAW file of the
/// stem, from that file in `went_with` to a RAW file of the same stem in `stayed` that is surely
/// still there. The sidecar is that file's alone now, and `take_back_sidecars` brings it back.
fn give_adobe_sidecars_to_those_that_stayed(
    went_with: &mut [(PathBuf, Vec<PathBuf>)],
    stayed: &mut [(PathBuf, Vec<PathBuf>)],
) {
    for (p, sides) in stayed.iter_mut() {
        let Some(stem) = xmp::adobe_stem(p).filter(|_| is_surely_there(p)) else {
            continue;
        };
        for (_, went_sides) in went_with.iter_mut() {
            let (theirs, own): (Vec<_>, Vec<_>) = (std::mem::take(went_sides).into_iter())
                .partition(|side| xmp::adobe_stem(side).as_ref() == Some(&stem));
            *went_sides = own;
            sides.extend(theirs);
        }
    }
}

/// Takes the sidecars that went to the trash of the files of `stayed`, which did not go, back
/// out of it, so that each photo keeps its keywords. Only a photo surely there takes them back:
/// one that could not be looked at may be gone, and its sidecar would then wait at its name for
/// the next file of that name. The move to the trash began at `since`. A sidecar that cannot
/// come back is told in `errors`.
fn take_back_sidecars(
    stayed: Vec<(PathBuf, Vec<PathBuf>)>,
    since: std::time::SystemTime,
    errors: &mut Vec<String>,
) {
    let sidecars: Vec<PathBuf> = (stayed.into_iter())
        .filter(|(p, _)| is_surely_there(p))
        .flat_map(|(_, sides)| sides)
        .filter(|side| is_surely_gone(side))
        .collect();
    if sidecars.is_empty() {
        return;
    }
    let trash_move = TrashMove {
        files: Vec::new(),
        sidecars,
        sidecars_left: Vec::new(),
        since,
        marks: Default::default(),
        collection: None,
    };
    if let (_, Some(error)) = restore(&trash_move, trash_dirs) {
        errors.push(error);
    }
}

/// Deletes each of `chunk` that is still the one `picked` for good, with its sidecars, as
/// `delete_chunk` does with its sidecars held. A file that another program or window has given
/// its name to since the question is left alone. Each sidecar is first put aside under a
/// hidden name, noted first in the delete's `log` when there is one, so that the next start puts
/// it right when Glimt is killed before it is deleted or back. The photo is deleted only when
/// all of them could be put aside. A photo whose sidecar
/// cannot be put aside stays, with its sidecars back, and so does one whose delete did not go
/// through or could not be looked at after it. A sidecar put aside that then cannot be deleted
/// stays under its hidden name, which no later file takes. Each is told in `errors`. Deleted
/// first, the photo left a sidecar that could not go at the old name, where the next file of
/// that name would show its keywords. Adobe's sidecar of two RAW files of one name stem goes
/// only with the last of them that goes, so it never stays behind without one. Returns the
/// files gone, the sidecars gone and the files left alone.
fn delete_for_good(
    chunk: &[PathBuf],
    picked: &PickedFingerprints,
    mut log: Option<&mut journal::DeleteLog>,
    errors: &mut Vec<String>,
) -> (Vec<PathBuf>, Vec<PathBuf>, Vec<PathBuf>) {
    let mut names = xmp::AdobeNames::default();
    // The files gone so far and the one being deleted, which leave its stem's sidecar to it.
    let mut leaving: HashSet<&PathBuf> = HashSet::new();
    let (mut went, mut sides, mut left_alone) = (Vec::new(), Vec::new(), Vec::new());
    let mut failed = |at: &Path, e: std::io::Error| errors.push(format!("{}: {e}", at.display()));
    for p in chunk {
        if let Err(e) = picked.still_the_one_picked(p) {
            failed(p, e);
            left_alone.push(p.clone());
            continue;
        }
        leaving.insert(p);
        let mut aside = Vec::new();
        let mut all_aside = Ok(());
        let to_put_aside: Vec<(PathBuf, PathBuf)> =
            (xmp::sidecars_moving_with(p, p, &leaving, &mut names).into_iter())
                .map(|(side, _)| {
                    let hidden = crate::files::file_io::tmp_path(&side);
                    (side, hidden)
                })
                .collect();
        // Noted on disk before the first rename, which the folder sync below makes lasting.
        if let Some(log) = log.as_deref_mut() {
            log.note_aside(p, &to_put_aside);
        }
        for (side, hidden) in to_put_aside {
            match rename_never_over(&side, &hidden) {
                Ok(()) => aside.push((side, hidden)),
                Err(e) => {
                    all_aside = Err((side, e));
                    break;
                }
            }
        }
        // The sidecars are aside on disk before the photo goes, so that a power cut never
        // leaves a sidecar at the name of a photo that is gone.
        if !aside.is_empty()
            && all_aside.is_ok()
            && let Err(e) = crate::files::file_io::sync_folder_of(p)
        {
            all_aside = Err((p.clone(), e));
        }
        // A test's kill comes here, between the sidecars put aside and the photo deleted.
        #[cfg(test)]
        (aside.iter()).for_each(|(side, _)| crate::support::threads::panic_in_a_test_at(side));
        let deleted = all_aside.map_err(|(at, e)| failed(&at, e)).is_ok() && {
            #[allow(clippy::disallowed_methods)] // Delete without the trash, the one place
            let removed = std::fs::remove_file(p);
            #[cfg(test)]
            let removed = crate::files::file_io::answer_lost_in_a_test(p, removed);
            // A delete that a share did and lost the answer of is done, so its sidecars go too.
            let removed = crate::files::file_io::done_after_all(removed, p, None);
            removed.map_err(|e| failed(p, e)).is_ok()
        };
        // A look that failed (a share that dropped for a moment) counts the file as there.
        if !(deleted && is_surely_gone(p)) {
            for (side, hidden) in aside.into_iter().rev() {
                if let Err(e) = rename_never_over(&hidden, &side) {
                    failed(&side, e);
                }
            }
            leaving.remove(p);
            continue;
        }
        went.push(p.clone());
        for (side, hidden) in aside {
            #[allow(clippy::disallowed_methods)] // the sidecar of a file deleted for good
            match std::fs::remove_file(&hidden) {
                Ok(()) => sides.push(side),
                Err(e) => failed(&side, e),
            }
        }
    }
    (went, sides, left_alone)
}

/// Takes each file of `sidecars_of_file` whose sidecars did not all go to the trash back out of
/// it, with those of its sidecars that went, so that the photo and its keywords stay together.
/// Otherwise the sidecar would wait at the old name for the next file of that name. The move to
/// the trash began at `since`. Returns the files taken back, each told in `errors`. A file that
/// cannot come back stays in the trash, and its Undo puts it back beside the sidecar left.
fn take_back_from_the_trash(
    sidecars_of_file: Vec<(PathBuf, Vec<PathBuf>)>,
    since: std::time::SystemTime,
    errors: &mut Vec<String>,
) -> HashSet<PathBuf> {
    let mut trash_move = TrashMove {
        files: Vec::new(),
        sidecars: Vec::new(),
        sidecars_left: Vec::new(),
        since,
        marks: Default::default(),
        collection: None,
    };
    let mut first_left = Vec::new();
    for (file, sides) in sidecars_of_file {
        // A sidecar that could not be looked at counts as left, as it may still be there.
        let (gone, left): (Vec<PathBuf>, Vec<PathBuf>) =
            sides.into_iter().partition(|s| is_surely_gone(s));
        let Some(side) = left.first() else {
            continue;
        };
        first_left.push((file.clone(), side.clone()));
        trash_move.files.push(file);
        trash_move.sidecars.extend(gone);
        trash_move.sidecars_left.extend(left);
    }
    if trash_move.files.is_empty() {
        return HashSet::new();
    }
    let (back, error) = restore(&trash_move, trash_dirs);
    let back: HashSet<PathBuf> = back.into_iter().collect();
    for (file, side) in first_left.iter().filter(|(file, _)| back.contains(file)) {
        errors.push(tf!(
            "{side} could not go to the trash, so {file} stays where it was, with its sidecar",
            side = side.display(),
            file = file.display()
        ));
    }
    errors.extend(error);
    back
}

/// Returns the type of the file system that `dir` is on, as `file_io::file_system_type` does. A
/// test gives its folder a type with `test_mount` instead, because no test can mount a FAT card.
pub(crate) fn file_system_type_or_test(dir: &Path) -> Option<String> {
    test_mount_of(dir).or_else(|| crate::files::file_io::file_system_type(dir))
}

/// Returns the type of the mount that holds `dir`, as `file_io::mounted_type` does, a disk
/// through FUSE as "fuseblk". A test gives its folder a type with `test_mount` instead.
pub(crate) fn mounted_type_or_test(dir: &Path) -> Option<String> {
    test_mount_of(dir).or_else(|| crate::files::file_io::mounted_type(dir))
}

/// Returns the type that a test gave `dir` or a folder above it with `test_mount`, and `None`
/// outside the tests.
fn test_mount_of(dir: &Path) -> Option<String> {
    #[cfg(test)]
    {
        let mounts = crate::support::threads::lock_even_if_poisoned(&TEST_MOUNTS);
        let kind = mounts.iter().find(|(at, _)| dir.starts_with(at));
        if let Some((_, kind)) = kind {
            return Some(kind.clone());
        }
    }
    #[cfg(not(test))]
    let _ = dir;
    None
}

/// Gives `dir` and the folders below it the file system `kind` in a test, as if a card were
/// mounted there. With `kind` as `None`, the folder is on its real file system again.
#[cfg(test)]
pub(crate) fn test_mount(dir: &Path, kind: Option<&str>) {
    let mut mounts = crate::support::threads::lock_even_if_poisoned(&TEST_MOUNTS);
    mounts.retain(|(at, _)| at != dir);
    mounts.extend(kind.map(|kind| (dir.to_path_buf(), kind.to_owned())));
}

/// Returns the type of the file system that `dir` is on when it is FAT or exFAT, as on a camera
/// card. Returns `None` on any other file system.
pub(crate) fn fat_file_system_type(dir: &Path) -> Option<String> {
    file_system_type_or_test(dir).filter(|kind| is_fat(kind))
}

/// Returns whether `kind`, a type of file system from `file_system_type`, is FAT or exFAT. An exFAT
/// card through FUSE is "fuseblk" where udev's data cannot be read, as in a container or the
/// Flatpak, and so is NTFS through ntfs-3g, which allows : ? * and the others. A fuseblk mount is
/// not taken as FAT, so that no name is refused on NTFS. On such a card the copy or rename fails at
/// that file with the system's error instead, as before 0.63.0.
fn is_fat(kind: &str) -> bool {
    matches!(kind, "vfat" | "msdos" | "exfat")
}

/// Finds a name of `to` with a character that the FAT or exFAT file system of its folder
/// refuses, before anything is renamed, copied or moved. Returns that name with the type of its
/// file system, or `None` when every name is allowed where it goes. `file_system_type` gives a
/// folder's file system. Only the folder of a name with such a character is looked up, and each
/// folder once, because a rename runs this in the window's thread and every lookup reads the
/// mount table.
fn refused_on_fat(
    to: &[PathBuf],
    file_system_type: impl Fn(&Path) -> Option<String>,
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
            .or_insert_with(|| file_system_type(dir))
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
/// `dest` refuses, so that a copy (`is_copy`) or a move into it refuses all before the first file.
/// Returns `None` when every name is allowed there. Every file goes into the one folder, so the
/// first name with such a character decides, and the names after it are not looked at.
fn refused_transfer(
    paths: &[PathBuf],
    dest: &Path,
    is_copy: bool,
    file_system_type: impl Fn(&Path) -> Option<String>,
) -> Option<String> {
    let odd = (paths.iter().filter_map(|p| p.file_name()))
        .find(|n| n.to_string_lossy().contains(NOT_ON_FAT))?;
    let (name, kind) = refused_on_fat(&[dest.join(odd)], file_system_type)?;
    Some(match is_copy {
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
/// photo would show that file's keywords and stars, and leave its own behind.) It is also the
/// file whose sidecar `to` would be (`a.jpg` for `a.jpg.xmp`), because that file would take
/// this one for its sidecar, and delete or move it with itself. For a RAW file it is also
/// Adobe's sidecar at the new name (`IMG_1.xmp` for `IMG_1.CR2`), which Lightroom would read
/// for the photo, unless another RAW file of that stem is or will be there, as it is then
/// neither file's (`xmp::adobe_sidecars_of`). `ours` are the operation's own names, which a swap
/// reuses, and `names` the folders it has read, with the files it brings. This is the one rule
/// for Copy, Move, Rename, Rename Many and their Undo.
pub(crate) fn name_in_the_way(
    to: &Path,
    ours: &HashSet<&PathBuf>,
    names: &mut xmp::AdobeNames,
) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    // A name counts as taken also when it is a link that points nowhere, because nothing may be
    // made in its place, and when the look at it failed.
    let there = |p: &PathBuf| !is_surely_gone(p);
    let sidecar_owner = (to.as_os_str().as_bytes().strip_suffix(b".xmp"))
        .map(|owner| PathBuf::from(std::ffi::OsStr::from_bytes(owner)));
    let adobe = xmp::adobe_sidecars_of(to, ours, names);
    [to.to_path_buf(), xmp::sidecar(to)]
        .into_iter()
        .chain(adobe)
        .chain(sidecar_owner)
        .find(|p| there(p) && !ours.contains(p))
}

/// Renames `from` to `to` as `rename_files` does, for a rename's thread. Each file at `from` that
/// is no longer the one `picked` is left out, so a file that another program put at the name
/// since keeps it. Returns how it went, with the places left out and the Undo it leaves: of a
/// rename that went through, or of an Undo (`undoing`) that failed, what it still has to put
/// back.
fn rename_undoable(
    (listed, to): (Vec<ListedFile>, Vec<PathBuf>),
    (picked, undoing): (&PickedFingerprints, bool),
    job: &Job,
) -> RenameOutcome {
    let from: Vec<PathBuf> = listed.iter().map(|f| f.path().to_path_buf()).collect();
    // The sidecars are locked before the files are checked. Another window's Rename Many holds
    // them while it swaps names, so a check before the lock passed and then renamed the other
    // photo once the swap was done.
    let locks = xmp::lock_sidecars(&from);
    let left_alone: Vec<usize> = (0..from.len())
        .filter(|&k| picked.still_the_one_picked(&from[k]).is_err())
        .collect();
    let pairs: Vec<(PathBuf, PathBuf)> = (from.into_iter().zip(to).enumerate())
        .filter(|(k, _)| left_alone.binary_search(k).is_err())
        .map(|(_, pair)| pair)
        .collect();
    let (kept_from, kept_to) = pairs.iter().cloned().unzip();
    let (result, names_known, stopped) = match pairs.is_empty() {
        true => (Ok(()), true, false),
        false => match rename_files(kept_from, kept_to, job, locks) {
            Ok(()) => (Ok(()), true, false),
            Err(e) => (Err(e.text), e.names_put_back, e.stopped),
        },
    };
    // An Undo that failed put the names back, so what it still has to do is the rename it was
    // asked to undo, from the old name to the name the file has now. A file left under a
    // hidden name has nothing at its name, so it drops out here and the journal repairs it.
    let undo = match (undoing, result.is_ok()) {
        (false, true) => pairs.iter().filter_map(with_fingerprint).collect(),
        (true, false) => (pairs.iter())
            .filter_map(|(now, before)| with_fingerprint(&(before.clone(), now.clone())))
            .collect(),
        _ => Vec::new(),
    };
    RenameOutcome {
        result,
        left_alone,
        undo,
        names_known,
        stopped,
    }
}

/// Renames `from` to `to` with their sidecars, all or none. It is refused when a new name is
/// another file's. When names swap, it goes through temporary names. The plan is on disk while
/// it runs, and a failure is undone. It never replaces a file, also not one that another
/// program makes meanwhile. `locks` are the locks of the sidecars of `from`
/// (`xmp::lock_sidecars`), which the caller took before it checked the files.
pub(crate) fn rename_files(
    from: Vec<PathBuf>,
    to: Vec<PathBuf>,
    job: &Job,
    locks: Vec<xmp::SidecarLock>,
) -> Result<(), RenameFailed> {
    rename_files_planned_in(journal::journal_dir(), from, to, job, locks)
}

/// Renames `from` to `to` as `rename_files` does, with the plan in the folder `plans`. Nothing
/// is renamed when the plan cannot be written there. The sidecars are held as a sidecar write
/// holds them (`_locks`), from before the look at which files have one. Otherwise a keyword
/// written meanwhile would land on an old name.
fn rename_files_planned_in(
    plans: Option<PathBuf>,
    from: Vec<PathBuf>,
    to: Vec<PathBuf>,
    job: &Job,
    _locks: Vec<xmp::SidecarLock>,
) -> Result<(), RenameFailed> {
    // Sidecars go with their files, through the same steps.
    let (mut from, mut to) = (from, to);
    let mut sidecar_names = xmp::AdobeNames::default();
    let sides: Vec<(PathBuf, PathBuf)> = (from.iter().zip(&to))
        .flat_map(|(f, t)| xmp::sidecars_moving_with(f, t, &HashSet::new(), &mut sidecar_names))
        .collect();
    // Adobe's sidecar keeps its name when only the extension's case changes (IMG_1.cr2). It
    // stays where it is, and its name is still one of the rename's own.
    let (staying, sides): (Vec<_>, Vec<_>) = sides.into_iter().partition(|(f, t)| f == t);
    for (f, t) in sides {
        from.push(f);
        to.push(t);
    }
    let staying: Vec<PathBuf> = staying.into_iter().map(|(side, _)| side).collect();
    let sources: HashSet<&PathBuf> = from.iter().chain(&staying).collect();
    let mut names = xmp::AdobeNames::arriving(to.iter().cloned());
    let mut in_the_way = to
        .iter()
        .filter_map(|t| name_in_the_way(t, &sources, &mut names))
        .peekable();
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
            "{}{}-{k}",
            journal::HIDDEN_NAME,
            crate::files::file_io::instance_tag()
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
    // start. Without it, nothing is renamed (`start_journal`).
    let plan = match plans {
        Some(dir) => journal::start_journal(&dir, &moves),
        None => Err(std::io::ErrorKind::NotFound.into()),
    };
    let mut plan = plan.map_err(|e| {
        tf!(
            "Could not save the plan of the renames ({e}), so nothing was renamed",
            e
        )
    })?;
    for (k, (a, b)) in moves.iter().enumerate() {
        // On Stop, the names done so far go back, by the same steps a failure takes.
        let stopped = job.is_stopped();
        let step = match stopped {
            true => Err(std::io::Error::other(tr("stopped"))),
            false => journal::rename_step(a, b),
        };
        // A step is counted in the plan once done. When the middle of a swap cannot be
        // counted, because its names did not reach the disk, it is undone with the rest.
        let is_middle_of_swap = swap && k + 1 == moves.len() / 2;
        let step = step
            .map_err(|e| (e, k))
            .and_then(|()| (plan.count_rename_done(is_middle_of_swap)).map_err(|e| (e, k + 1)));
        if let Err((e, done)) = step {
            // Named by the file and its new name, not a temporary one.
            let j = k % from.len();
            let name = |p: &PathBuf| p.file_name().unwrap_or_default().display().to_string();
            // A step whose answer was lost (a share's EIO or timeout) may have been done.
            // Nothing is undone then, because an undo counts on knowing which steps were done.
            // The plan stays as it is, and the next start finds the step by the names, once the
            // share's cache has expired. Any other failure did nothing, and is undone at once.
            let may_be_done =
                !stopped && done == k && crate::files::file_io::may_have_been_done(&e, a);
            // Otherwise the renames are undone so that no file is left under a hidden
            // temporary name, by the same steps the next start would take.
            let stuck = match may_be_done {
                true => 1,
                false => journal::undo(&moves, done).1,
            };
            let (f, t) = (name(&from[j]), name(&to[j]));
            // When not all were put back (a share that stopped answering, say), the plan stays,
            // and the next start offers to put the files right.
            if stuck > 0 {
                return Err(names_left_for_the_next_start(plan, (&f, &t), &e));
            }
            return Err(RenameFailed {
                text: tf!("Could not rename {f} to {t}: {e}; nothing renamed", f, t, e),
                names_put_back: true,
                stopped,
            });
        }
        // A file is counted when it lands under its name. That is the later half of a swap's
        // steps, and the sidecars do not count.
        let landed = if swap {
            k.checked_sub(moves.len() / 2)
        } else {
            Some(k)
        };
        if landed.is_some_and(|n| n < job.total) {
            job.count_done(1);
        }
    }
    // The plan goes once the renames' folders are on disk (`remove_plan`), before the marks and
    // collections follow the new names.
    drop(plan);
    Ok(())
}

/// Keeps the `plan` of a Rename Many whose names were not all put back after the rename of `f`
/// to `t` failed with `e`, so that the next start offers the repair. Returns the failure to
/// tell.
fn names_left_for_the_next_start(
    plan: journal::Journal,
    (f, t): (&str, &str),
    e: &std::io::Error,
) -> RenameFailed {
    plan.keep_on_disk();
    RenameFailed {
        text: tf!(
            "Could not rename {f} to {t}: {e}; not all names could be put back, Glimt offers to put them right at the next start",
            f,
            t,
            e
        ),
        names_put_back: false,
        stopped: false,
    }
}

/// Sends to the window, through `report_tx`, the Moves and deletes that Glimt was killed during,
/// found at the start. The marks and collections then follow the files that went, and the
/// marks and stars of the files deleted are forgotten, as the jobs' reports would have done.
#[cfg(not(test))]
fn send_moves_cut_short(report_tx: &AnswerSender<OperationReport>) {
    let logs = journal::move_log_dir().map(|d| journal::moves_cut_short(&d));
    for (log, went) in logs.into_iter().flatten() {
        let _ = report_tx.send(OperationReport {
            moved: went,
            move_log: Some(log),
            ..Default::default()
        });
    }
    let logs = journal::delete_log_dir().map(|d| journal::deletes_cut_short(&d));
    for (log, gone) in logs.into_iter().flatten() {
        let _ = report_tx.send(OperationReport {
            gone,
            delete_log: Some(log),
            ..Default::default()
        });
    }
}

/// Words the warning that the log of a Move or a delete in `dir` could not be made or written,
/// `failure`, and prints it. The job goes on without the log, as in Glimt 0.80.0: a Move or a
/// delete is how a full disk is cleared, and a kill halfway then only leaves the marks of the
/// files that went behind. It is no failed write, because the job itself works, so a close does
/// not wait.
fn log_not_saved((dir, failure): (PathBuf, std::io::Error)) -> String {
    let warning = tf!(
        "Could not save {name}: {e}",
        name = dir.display(),
        e = failure
    );
    eprintln!("{warning}");
    warning
}

/// Copies or moves `paths` into the folder `dest` for `transfer`, each with its sidecars, until
/// `job` is stopped. A Move with its log, `move_log`, writes each file down in the log before
/// it goes. A file that is no longer the one `picked` is left where it is. Returns the errors to
/// tell and the files put there, `(from, to)`.
fn put_files(
    paths: &[PathBuf],
    (picked, dest): (&PickedFingerprints, &Path),
    (is_copy, mut move_log): (bool, Option<&mut journal::MoveLog>),
    job: &Job,
) -> (Vec<String>, Vec<(PathBuf, PathBuf)>) {
    let (mut errors, mut put) = (Vec::new(), Vec::new());
    // Each folder is read once for the whole job (`xmp::AdobeNames`).
    let target_of = |path: &PathBuf| dest.join(path.file_name().unwrap_or_default());
    let mut names = xmp::AdobeNames::arriving(paths.iter().map(target_of));
    let mut sidecar_names = xmp::AdobeNames::leaving(paths);
    // The RAW files that leave with this move: those that went and those yet to come. One
    // that could not go is taken out, so that its stem's sidecar stays beside it.
    let mut moving: HashSet<&PathBuf> = paths.iter().filter(|_| !is_copy).collect();
    let mut stems_to_come = raw_files_by_stem(moving.iter().copied());
    let none_leaving = HashSet::new();
    let mut folders = xmp::FolderIds::new();
    for path in paths.iter().take_while(|_| !job.is_stopped()) {
        let target = target_of(path);
        let put_one = || {
            #[cfg(test)]
            crate::support::threads::panic_in_a_test_at(path);
            let another_to_come = is_another_of_its_stem_to_come(path, &mut stems_to_come);
            let mut failed = |at: &Path, e: std::io::Error| {
                errors.push(format!("{display}: {e}", display = at.display()));
            };
            // The file is moved first and then its sidecar, and a sidecar that cannot follow
            // takes the file back (`xmp::follow`, `put_file_back`). A move holds the
            // sidecar as a write does, so a write that waited lands on no old name. A copy
            // does not hold it, because the photo stays. A move to another disk holds it
            // while the photo is copied, which only makes writes under the same lock wait.
            let _lock = (!is_copy).then(|| xmp::lock_sidecar_among(path, &mut folders));
            // Another window or program may have given the name to another photo meanwhile.
            if let Err(e) = picked.still_the_one_picked(path) {
                failed(path, e);
                moving.remove(path);
            } else if let Some(at) = name_in_the_way(&target, &HashSet::new(), &mut names) {
                failed(&at, std::io::ErrorKind::AlreadyExists.into());
                moving.remove(path);
            } else if let Err(e) = {
                // The file is written down before it goes, so that a kill leaves none gone
                // without its line.
                if let Some(log) = move_log.as_deref_mut() {
                    log.note(path, &target);
                }
                put_file_until_stopped(path, &target, is_copy, &|| job.is_stopped())
            } {
                // A file stopped halfway is not there, and the stop itself is told.
                if !(e.kind() == std::io::ErrorKind::Interrupted && job.is_stopped()) {
                    failed(&target, e);
                }
                moving.remove(path);
            } else if let Err((back, told)) = {
                // A RAW file with another of its name stem still to come takes only its own
                // sidecar, at once, so that a kill never leaves the two apart. Adobe's sidecar
                // of the stem goes with the last of them, as only then is it known which went.
                let leaving = match another_to_come {
                    true => &none_leaving,
                    false => &moving,
                };
                follow_or_go_back((path, &target), is_copy, leaving, &mut sidecar_names)
            } {
                // The file and its sidecars go together or not at all, so a later file of the
                // old name never picks up a sidecar left behind.
                errors.extend(told);
                match back {
                    true => drop(moving.remove(path)),
                    false => put.push((path.clone(), target.clone())),
                }
            } else {
                put.push((path.clone(), target.clone()));
            }
        };
        // A file that breaks the work costs only that file. One that went before the panic is
        // still reported, so that its marks follow it and Undo brings it back.
        if catch_panic(put_one).is_none() {
            errors.push(format!("{}: {}", path.display(), broke_here()));
            match !is_copy && is_surely_gone(path) && is_surely_there(&target) {
                true => put.push((path.clone(), target)),
                false => drop(moving.remove(path)),
            }
        }
        job.count_done(1);
    }
    // A move on one disk is a rename, which is on disk only with its folder. The marks
    // and collections follow the new names once the report arrives, so after a stick
    // pulled out the file and its marks must not be under different names.
    if !is_copy
        && !put.is_empty()
        && let Err(e) = crate::files::file_io::sync_folder(dest)
    {
        errors.push(format!("{display}: {e}", display = dest.display()));
    }
    (errors, put)
}

/// Counts the RAW files of each name stem among `paths`. Lightroom's sidecar of a stem belongs to
/// neither of two RAW files, so while a move has another of them yet to come, it does not know
/// whether the sidecar leaves (`is_another_of_its_stem_to_come`).
fn raw_files_by_stem<'a>(
    paths: impl Iterator<Item = &'a PathBuf>,
) -> HashMap<(PathBuf, String), usize> {
    let mut to_come: HashMap<(PathBuf, String), usize> = HashMap::new();
    for (dir, stem) in paths.filter_map(|p| xmp::adobe_stem(p)) {
        *to_come.entry((dir.to_owned(), stem)).or_default() += 1;
    }
    to_come
}

/// Counts `path` as reached in `to_come`, from `raw_files_by_stem`. Returns whether another RAW
/// file of its name stem is still to come. The file then takes only its own sidecar, and the
/// last of them takes the stem's, once the move knows which of them went.
fn is_another_of_its_stem_to_come(
    path: &Path,
    to_come: &mut HashMap<(PathBuf, String), usize>,
) -> bool {
    let stem = xmp::adobe_stem(path).map(|(dir, stem)| (dir.to_owned(), stem));
    stem.and_then(|stem| to_come.get_mut(&stem))
        .is_some_and(|n| {
            *n = n.saturating_sub(1);
            *n > 0
        })
}

/// Moves the files of an Undo of a move back to where they came from, each with its sidecar.
/// A file whose place another file has taken since the move is left where it is, and so is one
/// that cannot go back. Returns the pairs moved back, `(now, before)`, the files left to be
/// undone again (a name taken, a share that did not answer), and the errors to tell. A file
/// that another file has replaced is not among those left, as no later Undo can take it back.
fn move_back(pairs: Vec<Moved>) -> MovedBack {
    let (mut errors, mut back, mut left) = (Vec::new(), Vec::new(), Vec::new());
    let mut names = xmp::AdobeNames::arriving(pairs.iter().map(|(from, _, _)| from.clone()));
    let moving_back: Vec<PathBuf> = pairs.iter().map(|(_, now, _)| now.clone()).collect();
    let mut sidecar_names = xmp::AdobeNames::leaving(&moving_back);
    // The files that go back with this Undo: those that went and those yet to come. One that
    // stays is taken out, so that its stem's sidecar stays beside it.
    let mut moving: HashSet<&PathBuf> = moving_back.iter().collect();
    let mut stems_to_come = raw_files_by_stem(moving_back.iter());
    let none_leaving = HashSet::new();
    let mut folders = xmp::FolderIds::new();
    for (from, to, at_move) in pairs {
        let another_to_come = is_another_of_its_stem_to_come(&to, &mut stems_to_come);
        let _lock = xmp::lock_sidecar_among(&to, &mut folders);
        // Another program may have put another file at `to` since the move.
        if !fingerprint(&to).is_some_and(|now| same_fingerprint(at_move, now)) {
            let other = tr("another file has taken its place");
            errors.push(format!("{}: {other}", to.display()));
            moving.remove(&to);
            continue;
        }
        let result = match name_in_the_way(&from, &HashSet::new(), &mut names) {
            Some(at) => Err((at, std::io::ErrorKind::AlreadyExists.into())),
            None => put_file(&to, &from, false).map_err(|e| (from.clone(), e)),
        };
        // As in `put_files`, Adobe's sidecar of a stem goes with the last of its RAW files.
        let leaving = match another_to_come {
            true => &none_leaving,
            false => &moving,
        };
        match result {
            Ok(()) => {
                match follow_or_go_back((&to, &from), false, leaving, &mut sidecar_names) {
                    Ok(()) => back.push((to, from)),
                    // The file went back to its sidecars at `to`, and the Undo stays to be tried
                    // again. A look at it that failed is no answer that it is gone, so the Undo
                    // keeps the fingerprint of the move, which a file moved back and forth keeps.
                    Err((true, told)) => {
                        errors.extend(told);
                        moving.remove(&to);
                        let now = fingerprint(&to).unwrap_or(at_move);
                        left.push((from, to, now));
                    }
                    // The file is at `from` with the sidecars that followed it.
                    Err((false, told)) => {
                        errors.extend(told);
                        back.push((to, from));
                    }
                }
            }
            Err((at, e)) => {
                errors.push(format!("{}: {e}", at.display()));
                moving.remove(&to);
                left.push((from, to, at_move));
            }
        }
    }
    // The names moved back are on disk with their folders before the marks and collections
    // follow them, once for each folder.
    let folders: HashSet<&Path> = back
        .iter()
        .filter_map(|(_, before)| before.parent())
        .collect();
    for folder in folders {
        if let Err(e) = crate::files::file_io::sync_folder(folder) {
            errors.push(format!("{}: {e}", folder.display()));
        }
    }
    (back, left, errors)
}

/// Lets the sidecars of the file just put from `from` at `to` follow it (`xmp::follow`). When
/// one cannot, the file goes back first, and the sidecars that followed are taken back only once
/// it is there, each of them also after one that failed. When the file cannot go back, those
/// sidecars stay with it at `to`. So the file always ends with every sidecar that could reach
/// it. A sidecar whose move may have been done (a share that lost its answer) may be at both
/// names or at the new one, so it is put where the file ends (`sidecar_where_the_file_is`). A
/// sidecar that was copied, as it could not be moved, is still at its old name, where the next
/// file of that name would take it. It is told too, with the file at `to`, unless the file went
/// back. Returns, when a sidecar could not follow, whether the file is back at `from`, with the
/// errors to tell, each saying where the file and that sidecar are.
fn follow_or_go_back(
    (from, to): (&Path, &Path),
    is_copy: bool,
    moving: &HashSet<&PathBuf>,
    names: &mut xmp::AdobeNames,
) -> Result<(), (bool, Vec<String>)> {
    use crate::files::file_io::{KnownAs, fingerprint_or_error, is_still_the_file, put_file_back};
    // The file is looked at before its sidecars follow, which may take a while, so that only
    // this file goes back, not one that another program put at `to` meanwhile. A look that
    // failed tells nothing, and the file stays.
    let at_put = fingerprint_or_error(to);
    let not = match xmp::follow(from, to, is_copy, moving, names) {
        Ok(left_behind) if left_behind.is_empty() => return Ok(()),
        Ok(left_behind) => return Err((false, left_behind.iter().map(|l| l.told()).collect())),
        Err(not) => not,
    };
    let file = from.display();
    let put_back = at_put
        .and_then(|at_put| is_still_the_file(to, KnownAs::fingerprint(at_put)))
        .and_then(|()| put_file_back(from, to, is_copy));
    let Err(error) = put_back else {
        let mut told = Vec::new();
        for (side, at, was_copy) in not.followed.into_iter().rev() {
            if let Err(e) = crate::files::file_io::put_file_back(&side, &at, was_copy) {
                let (side, at) = (side.display(), at.display());
                told.push(tf!(
                    "{file} went back, but its sidecar {side} could not ({e}), so the sidecar is at {at}",
                    file,
                    side,
                    e,
                    at
                ));
            }
        }
        // The window shows the first error, so a sidecar apart from its file comes first, and
        // then the file is not said to be with its sidecar.
        // Each sidecar is put beside the file, also after one that could not be.
        let beside: Vec<bool> = (not.failed.iter())
            .map(|(side, at, e)| is_copy || sidecar_where_the_file_is((side, at), e, true))
            .collect();
        let is_whole = told.is_empty() && !beside.contains(&false);
        told.extend(not.failed.iter().map(|(_, at, e)| {
            let side = at.display();
            match is_whole {
                true => tf!(
                    "{side}: {e}, so {file} stays where it was, with its sidecar",
                    side,
                    e,
                    file
                ),
                false => format!("{side}: {e}"),
            }
        }));
        return Err((true, told));
    };
    let to = to.display();
    let told = (not.failed.iter())
        .filter(|(side, at, e)| is_copy || !sidecar_where_the_file_is((side, at), e, false))
        .map(|(side, _, e)| {
            let side = side.display();
            tf!(
                "{side} could not follow {file} ({e}), and {file} could not go back ({error}), so the file is at {to} and this sidecar at {side}",
                side,
                file,
                e,
                error,
                to
            )
        })
        .chain(not.left_behind.iter().map(|l| l.told()))
        .collect();
    Err((false, told))
}

/// Puts the sidecar whose move from `side` to `at` failed with `e` beside its file, at `side`
/// when the file went back (`file_is_back`) and otherwise at `at`. A move that may have been done
/// may have left it at both names, one file under two, or only at the new one, which
/// `journal::finish_step` puts right. A move refused for sure left the sidecar at `side`, and it
/// is not touched. Returns whether the sidecar is surely beside the file now.
fn sidecar_where_the_file_is(
    (side, at): (&Path, &Path),
    e: &std::io::Error,
    file_is_back: bool,
) -> bool {
    if !crate::files::file_io::may_have_been_done(e, side) {
        return file_is_back;
    }
    let (away, beside) = match file_is_back {
        true => (at, side),
        false => (side, at),
    };
    journal::finish_step(away, beside)
}

/// Returns a file just moved or renamed from `from` to `to` with its fingerprint, for Undo. Returns
/// `None` when nothing is at `to` any more.
fn with_fingerprint((from, to): &(PathBuf, PathBuf)) -> Option<Moved> {
    Some((from.clone(), to.clone(), fingerprint(to)?))
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
    let folded = |p: &Path| p.to_string_lossy().to_lowercase();
    // It is built once, so a rename of 20,000 files on a card does not compare every pair.
    let ours: HashSet<(String, FileFingerprint)> = from
        .iter()
        .filter_map(|f| Some((folded(f), fingerprint(f)?)))
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
        if own_name || !fingerprint(&at).is_some_and(|i| ours.contains(&(folded(&at), i))) {
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
fn sidecar_writer() -> &'static mpsc::Sender<crate::files::file_io::QueuedWrite> {
    static WRITER: std::sync::OnceLock<mpsc::Sender<crate::files::file_io::QueuedWrite>> =
        std::sync::OnceLock::new();
    WRITER.get_or_init(crate::files::file_io::start_ordered_worker)
}

/// `(from, to)` pairs to look up by the old path.
pub(crate) fn moves_by_source(pairs: &[(PathBuf, PathBuf)]) -> HashMap<&Path, &Path> {
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

/// Returns the deepest folder that holds every file of `paths`, or `/` when they share none.
fn folder_of_all(paths: &[PathBuf]) -> PathBuf {
    let mut folder = (paths.first())
        .and_then(|p| p.parent())
        .map_or_else(|| PathBuf::from("/"), Path::to_path_buf);
    for path in paths {
        while !path.starts_with(&folder) && folder.pop() {}
    }
    folder
}

#[cfg(test)]
mod tests {
    use crate::support::test_support::{app_on, contract_folder, run_background_until};
    use crate::support::threads::{Told, lock_even_if_poisoned};
    use std::path::PathBuf;

    /// The renames found cut short at the start are added to the plans that a refused repair
    /// asked about again before they came. Before, they replaced them, and a test that put
    /// right such a plan found nothing left to ask about when the look-up answered late.
    #[test]
    fn renames_found_at_start_keep_those_asked_about_again() {
        let dir = std::env::temp_dir().join(format!(
            "glimt-test-cut-short-found-late-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = app_on(&dir);
        let moves = vec![(dir.join("a.txt"), dir.join("b.txt"))];
        let plan = (dir.join("plan"), moves, Some(1));
        app.file_operations
            .ask_again_about_cut_short(vec![plan], String::new());
        let found = |a: &crate::App| a.file_operations.cut_short_rx.is_none();
        assert!(run_background_until(&mut app, &ctx, found));
        assert_eq!(app.file_operations.cut_short().len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// `rename_files` with the sidecars' locks taken first, as `rename_undoable` takes them.
    fn rename_files(
        from: Vec<PathBuf>,
        to: Vec<PathBuf>,
        job: &super::Job,
    ) -> Result<(), super::RenameFailed> {
        let locks = crate::metadata::xmp::lock_sidecars(&from);
        super::rename_files(from, to, job, locks)
    }

    /// A copy looks up its folder's real path in the background and starts its job when the
    /// answer comes. Before, the window's thread waited for the disk there, which a share that
    /// stopped answering held for ever.
    #[test]
    fn a_copy_looks_up_its_folder_in_the_background() {
        let dir = contract_folder("copy-looks-up");
        std::os::unix::fs::symlink(dir.join("sub"), dir.join("link")).unwrap();
        let (mut app, ctx) = app_on(&dir);
        app.transfer(
            crate::files::file_io::PickedFiles::new(vec![dir.join("a.png")]),
            dir.join("link"),
            true,
        );
        assert!(app.file_operations.jobs.is_empty());
        let copied = dir.join("sub/a.png");
        assert!(run_background_until(&mut app, &ctx, |_| copied.exists()));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Nothing is deleted while a list is read, as its entries are not yet checked to be files
    /// then. A folder named in the list was moved to the trash as one of its entries.
    #[test]
    fn nothing_is_deleted_while_a_list_is_read() {
        let d = std::env::temp_dir().join(format!("glimt-test-list-read-{}", std::process::id()));
        std::fs::create_dir_all(d.join("sub")).unwrap();
        image::RgbImage::new(2, 2).save(d.join("a.png")).unwrap();
        std::fs::write(d.join("show.m3u"), "sub\na.png\n").unwrap();
        let (mut app, ctx) = app_on(&d);
        crate::support::test_support::open_now(&mut app, &ctx, d.join("show.m3u"));
        assert!(app.folder.is_listing() && app.list.files().len() == 2);
        // Random order puts the same names in a new list, which let them be deleted.
        app.shuffle_list(&ctx, true);
        let sub = (app.list.files().iter()).position(|f| std::path::Path::new(f).ends_with("sub"));
        app.delete_targets(&ctx, false, vec![sub.unwrap()], None);
        assert!(app.notice.text().is_some() && app.list.files().len() == 2);
        assert!(run_background_until(&mut app, &ctx, |a| !a
            .folder
            .is_listing()));
        assert_eq!(app.list.files().len(), 1);
        std::fs::remove_dir_all(d).unwrap();
    }

    /// A list's entries count as unchecked only until a listing of them comes whole. A folder
    /// opened while a list was read was refused file operations until its own listing came,
    /// and a list whose listing broke allowed them unchecked. The refusal then said that the
    /// files were still worked on, which no waiting would end.
    #[test]
    fn only_a_whole_listing_checks_a_list() {
        let d = std::env::temp_dir().join(format!("glimt-test-list-gone-{}", std::process::id()));
        std::fs::create_dir_all(d.join("sub")).unwrap();
        image::RgbImage::new(2, 2).save(d.join("a.png")).unwrap();
        std::fs::write(d.join("show.m3u"), "sub\na.png\n").unwrap();
        let (mut app, ctx) = app_on(&d);
        crate::support::test_support::open_now(&mut app, &ctx, d.join("show.m3u"));
        crate::support::test_support::open_now(&mut app, &ctx, d.clone());
        assert!(!app.list.has_unchecked_entries());
        crate::support::test_support::open_now(&mut app, &ctx, d.join("show.m3u"));
        let (tx, rx) = std::sync::mpsc::channel();
        drop(tx);
        app.folder.hold_listing(rx);
        app.receive_listing(&ctx, std::time::Duration::ZERO);
        assert!(!app.folder.is_listing());
        app.notice.clear();
        assert!(!app.are_free_to_change(&[]));
        // It says that the listing stopped, not that the files are still worked on.
        let what = crate::support::lang::tr("Reading the folder");
        let stopped = crate::tf!("{what} stopped unexpectedly", what);
        assert_eq!(app.notice.text(), Some(stopped.as_str()));
        std::fs::remove_dir_all(d).unwrap();
    }

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

    /// A Copy To of 200,000 files with a ':' at the end of every long name stops at the first
    /// name, because one folder takes them all. It once joined every name to the folder in the
    /// window's thread first, which took 170 ms where 50 ms are allowed here.
    #[test]
    fn a_copy_of_many_refused_names_looks_at_the_first() {
        use std::path::{Path, PathBuf};
        let long = "a".repeat(240);
        let paths: Vec<PathBuf> = (0..200_000)
            .map(|i| PathBuf::from(format!("/photos/{long}{i}:")))
            .collect();
        let on_ext4 = |_: &Path| Some("ext4".to_owned());
        let start = std::time::Instant::now();
        assert!(super::refused_transfer(&paths, Path::new("/card"), true, on_ext4).is_none());
        let took = start.elapsed();
        assert!(took < std::time::Duration::from_millis(50), "{took:?}");
        let on_vfat = |_: &Path| Some("vfat".to_owned());
        let told = super::refused_transfer(&paths, Path::new("/card"), true, on_vfat).unwrap();
        assert!(told.contains(&format!("{long}0:")), "{told}");
    }

    /// A Copy To or Move To onto a FAT card, where one file's name has a character the card
    /// refuses, says so, copies or moves nothing and leaves every file in the list. On another
    /// file system the same copy goes ahead.
    #[test]
    fn a_copy_or_move_onto_fat_with_a_refused_name_starts_nothing() {
        let d = std::env::temp_dir().join(format!("glimt-test-fat-copy-{}", std::process::id()));
        std::fs::create_dir_all(d.join("card")).unwrap();
        for f in ["a.png", "12:30.png"] {
            image::RgbImage::new(2, 2).save(d.join(f)).unwrap();
        }
        let (mut app, ctx) = app_on(&d);
        let listed = |a: &crate::App| a.list.files().len() == 2;
        assert!(run_background_until(&mut app, &ctx, listed));
        let files = vec![d.join("a.png"), d.join("12:30.png")];
        let card = d.join("card").canonicalize().unwrap();
        super::test_mount(&card, Some("vfat"));
        for is_copy in [true, false] {
            app.notice.clear();
            app.transfer(
                crate::files::file_io::PickedFiles::new(files.clone()),
                d.join("card"),
                is_copy,
            );
            // Refused before the files left the list, so the view stays where it was.
            assert!(listed(&app), "the refused files left the list");
            assert!(run_background_until(&mut app, &ctx, |a| a
                .notice
                .text()
                .is_some()
                && !a.working()));
            let told = app.notice.text().unwrap_or_default().to_owned();
            assert!(
                told.contains("\"12:30.png\"") && told.contains("vfat"),
                "{told}"
            );
            assert!(!d.join("card/a.png").exists() && d.join("a.png").exists());
            assert!(run_background_until(&mut app, &ctx, listed));
        }
        super::test_mount(&card, Some("ext4"));
        app.transfer(
            crate::files::file_io::PickedFiles::new(files),
            d.join("card"),
            true,
        );
        assert!(run_background_until(&mut app, &ctx, |_| d
            .join("card/12:30.png")
            .exists()));
        assert!(run_background_until(&mut app, &ctx, |a| !a.working()));
        super::test_mount(&card, None);
        std::fs::remove_dir_all(d).unwrap();
    }

    /// A Copy To, Move To or a sort folder given as a link to a FAT card (`~/card` →
    /// `/media/u/CARD`), where one file's name has a character the card refuses, copies or
    /// moves nothing, and the files stay in the list. The mount is looked up by the folder the
    /// link points to, as 0.63.0 looked it up by the link's own path and copied the other files,
    /// and 0.66.1 took the files off the list until the folder was read again. A folder named
    /// with `..` (`CARD/..`) is the one above the card, not on it.
    #[test]
    fn a_copy_through_a_link_to_fat_with_a_refused_name_copies_nothing() {
        let d = std::env::temp_dir().join(format!("glimt-test-fat-link-{}", std::process::id()));
        std::fs::create_dir_all(d.join("media/CARD")).unwrap();
        std::os::unix::fs::symlink(d.join("media/CARD"), d.join("card")).unwrap();
        for f in ["a.png", "12:30.png"] {
            image::RgbImage::new(2, 2).save(d.join(f)).unwrap();
        }
        let (mut app, ctx) = app_on(&d);
        let listed = |a: &crate::App| a.list.files().len() == 2;
        assert!(run_background_until(&mut app, &ctx, listed));
        let card = d.join("media/CARD").canonicalize().unwrap();
        super::test_mount(&card, Some("vfat"));
        let files = vec![d.join("a.png"), d.join("12:30.png")];
        for is_copy in [true, false] {
            app.notice.clear();
            app.transfer(
                crate::files::file_io::PickedFiles::new(files.clone()),
                d.join("card"),
                is_copy,
            );
            assert!(listed(&app), "the refused files left the list");
            assert!(run_background_until(&mut app, &ctx, |a| a
                .notice
                .text()
                .is_some()
                && !a.working()));
            let told = app.notice.text().unwrap_or_default().to_owned();
            assert!(
                told.contains("\"12:30.png\"") && told.contains("vfat"),
                "{told}"
            );
            assert!(!card.join("a.png").exists() && !card.join("12:30.png").exists());
        }
        app.transfer(
            crate::files::file_io::PickedFiles::new(files),
            d.join("media/CARD/.."),
            true,
        );
        assert!(run_background_until(&mut app, &ctx, |a| !a.working()));
        super::test_mount(&card, None);
        assert!(
            d.join("media/12:30.png").exists(),
            "{:?}",
            app.notice.text()
        );
        std::fs::remove_dir_all(d).unwrap();
    }

    /// With subfolders and no auto refresh, a move into a link in the tree follows the rule the
    /// list is read by. Into a link to a folder inside the tree, the file stays in the list,
    /// named by its real path as the tree names it. Into
    /// a link to a folder outside it, the file leaves the list and is not listed again, as the
    /// tree does not follow that link.
    #[test]
    fn a_move_into_a_link_in_the_tree_follows_the_listing() {
        let d = std::env::temp_dir().join(format!("glimt-test-move-link-{}", std::process::id()));
        std::fs::create_dir_all(d.join("tree/inner")).unwrap();
        std::fs::create_dir_all(d.join("beside")).unwrap();
        for f in ["a.png", "b.png"] {
            image::RgbImage::new(2, 2)
                .save(d.join("tree").join(f))
                .unwrap();
        }
        std::os::unix::fs::symlink("inner", d.join("tree/in")).unwrap();
        std::os::unix::fs::symlink("../beside", d.join("tree/out")).unwrap();
        let tree = d.join("tree").canonicalize().unwrap();
        let (mut app, ctx) = app_on(&tree);
        app.config.auto_refresh = false;
        app.set_subfolders(&ctx, true);
        let names = |a: &crate::App| -> Vec<String> {
            let files = a.list.files().iter();
            let mut names: Vec<String> = files.map(|f| f.to_string_lossy().into_owned()).collect();
            names.sort();
            names
        };
        assert!(run_background_until(&mut app, &ctx, |a| names(a) == ["a.png", "b.png"]));
        app.transfer(
            crate::files::file_io::PickedFiles::new(vec![tree.join("a.png")]),
            tree.join("in"),
            false,
        );
        assert!(run_background_until(&mut app, &ctx, |a| !a.working()
            && names(a) == ["b.png", "inner/a.png"]));
        app.transfer(
            crate::files::file_io::PickedFiles::new(vec![tree.join("b.png")]),
            tree.join("out"),
            false,
        );
        assert!(run_background_until(&mut app, &ctx, |a| !a.working()
            && names(a) == ["inner/a.png"]));
        assert!(d.join("beside/b.png").exists());
        app.refresh(&ctx);
        assert!(run_background_until(&mut app, &ctx, |a| !a
            .folder
            .is_listing()
            && !a.working()));
        assert_eq!(names(&app), ["inner/a.png"]);
        std::fs::remove_dir_all(d).unwrap();
    }

    /// A folder on a FAT card renamed to a name with a character the card refuses stays as it
    /// was, and the notice says why before anything is tried.
    #[test]
    fn a_folder_rename_on_fat_with_a_refused_name_is_told_first() {
        let d = std::env::temp_dir().join(format!("glimt-test-fat-dir-{}", std::process::id()));
        std::fs::create_dir_all(d.join("sub")).unwrap();
        let (mut app, ctx) = app_on(&d);
        super::test_mount(&d, Some("exfat"));
        app.dialogs.act_on_folder(d.join("sub"));
        app.rename_folder(&ctx, "12:30");
        // The file system is looked at in the rename's thread, not in the window's.
        let told = |a: &crate::App| !a.working() && a.notice.text().is_some();
        assert!(run_background_until(&mut app, &ctx, told));
        super::test_mount(&d, None);
        let told = app.notice.text().unwrap_or_default().to_owned();
        assert!(
            told.contains("\"12:30\"") && told.contains("exfat"),
            "{told}"
        );
        assert!(d.join("sub").is_dir() && !d.join("12:30").exists());
        std::fs::remove_dir_all(d).unwrap();
    }

    /// A rename on a FAT card to a name with a character the card refuses renames nothing, and
    /// the notice says why before the job starts, also now that the file system is looked at
    /// in the background. The same name on another file system is renamed.
    #[test]
    fn a_rename_on_fat_with_a_refused_name_is_told_first() {
        let d = std::env::temp_dir().join(format!("glimt-test-fat-ren-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        image::RgbImage::new(2, 2).save(d.join("a.png")).unwrap();
        let (mut app, ctx) = app_on(&d);
        assert!(run_background_until(&mut app, &ctx, |a| a
            .list
            .files()
            .len()
            == 1));
        let rename = |app: &mut crate::App| {
            let picked = crate::files::file_io::PickedFiles::new(vec![d.join("a.png")]);
            let listed = vec![app.listed_file(0)];
            app.rename_in_background(&ctx, listed, vec![d.join("12:30.png")], picked);
            assert!(run_background_until(app, &ctx, |a| !a.working()));
        };
        super::test_mount(&d, Some("vfat"));
        app.notice.clear();
        rename(&mut app);
        let told = app.notice.text().unwrap_or_default().to_owned();
        assert!(
            told.contains("\"12:30.png\"") && told.contains("vfat"),
            "{told}"
        );
        assert!(d.join("a.png").exists() && !d.join("12:30.png").exists());
        super::test_mount(&d, Some("ext4"));
        rename(&mut app);
        super::test_mount(&d, None);
        assert!(!d.join("a.png").exists() && d.join("12:30.png").exists());
        std::fs::remove_dir_all(d).unwrap();
    }

    /// An Undo of a rename whose job does not start stays to be done again. Its old name has a
    /// character that FAT refuses, so its folder's file system is looked at first, and the job
    /// is refused after that look: by the file system, or by a turn being saved meanwhile.
    /// Before, the Undo was taken before the look and lost, and Ctrl+Z then said "Nothing to
    /// undo".
    #[test]
    fn an_undo_of_a_rename_that_does_not_start_is_kept() {
        let d = std::env::temp_dir().join(format!("glimt-test-undo-kept-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let (old, new) = (d.join("12:30.png"), d.join("a.png"));
        image::RgbImage::new(2, 2).save(&old).unwrap();
        let (mut app, ctx) = app_on(&d);
        assert!(run_background_until(&mut app, &ctx, |a| a
            .list
            .files()
            .len()
            == 1));
        let picked = crate::files::file_io::PickedFiles::new(vec![old.clone()]);
        let listed = vec![app.listed_file(0)];
        app.rename_in_background(&ctx, listed, vec![new.clone()], picked);
        let kept = |a: &crate::App| !a.working() && a.file_operations.undo.is_some();
        assert!(run_background_until(&mut app, &ctx, kept));
        // The card refuses the old name.
        super::test_mount(&d, Some("vfat"));
        app.undo(&ctx);
        let kept_after_refusal = run_background_until(&mut app, &ctx, kept);
        super::test_mount(&d, None);
        assert!(kept_after_refusal, "{:?}", app.notice.text());
        // A turn of the file is saved while the look runs.
        app.undo(&ctx);
        app.file_operations.saving.insert(new.clone());
        assert!(run_background_until(&mut app, &ctx, kept));
        assert!(app.notice.text().is_some_and(|t| t.contains("rotation")));
        app.file_operations.saving.clear();
        app.undo(&ctx);
        assert!(run_background_until(&mut app, &ctx, |a| !a.working()));
        assert!(old.exists() && !new.exists());
        std::fs::remove_dir_all(d).unwrap();
    }

    /// Rename asks about the one file it names, not every selected file. Before, its answer
    /// renamed the first of them still in the list, so another selected file got the typed
    /// name when the one named left the list while the dialog was open.
    #[test]
    fn rename_acts_only_on_the_file_it_names() {
        let d = std::env::temp_dir().join(format!("glimt-test-rename-one-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        for name in ["a.png", "b.png"] {
            image::RgbImage::new(2, 2).save(d.join(name)).unwrap();
        }
        let (mut app, ctx) = app_on(&d);
        assert!(run_background_until(&mut app, &ctx, |a| a
            .list
            .files()
            .len()
            == 2));
        app.run(&ctx, crate::ui::actions::Action::SelectAll);
        app.start_rename(&ctx);
        assert_eq!(app.dialogs.asked_names(), ["a.png"]);
        // Another program deletes a.png, and the list follows.
        std::fs::remove_file(d.join("a.png")).unwrap();
        app.remove_indices(&ctx, &[0]);
        let rename = crate::ui::dialogs::Prompt::Rename;
        app.act_on_prompt_answer(&ctx, rename, "holiday.png");
        assert!(run_background_until(&mut app, &ctx, |a| !a.working()));
        assert!(d.join("b.png").exists() && !d.join("holiday.png").exists());
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
        let moved = super::moves_by_source(&pairs);
        let at = |s: &str| super::new_place(std::path::Path::new(s), &moved);
        assert_eq!(at("/a/x.jpg").unwrap().as_os_str(), "/b/y.jpg");
        assert_eq!(at("/trip/day/1.jpg"), Some(p("/trip 2024/day/1.jpg")));
        assert_eq!(at("/trip").unwrap().as_os_str(), "/trip 2024");
        assert_eq!(at("/a/z.jpg"), None);
    }

    /// A swap whose middle does not reach the disk (a USB stick pulled out during the sync) is
    /// undone and told, so that no plan says it was done while the names are those of the start.
    /// Two windows on one folder. The first swaps two names, as its Rename Many does, and the
    /// second still lists them as they were. Its keyword, rename and delete of the photo it
    /// knew under a name are refused, so they do not land on the other photo. They did.
    #[test]
    fn a_window_leaves_alone_a_name_that_another_window_gave_another_photo() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-two-windows-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b, c) = (dir.join("a.png"), dir.join("b.png"), dir.join("c.png"));
        image::RgbImage::new(4, 4).save(&a).unwrap();
        image::RgbImage::new(6, 6).save(&b).unwrap();
        let (mut first, first_ctx) = app_on(&dir);
        // A window of its own for each act, because the refusal of one has its window read the
        // folder again, and the next act would then be on the new list.
        let stale = || {
            let (mut window, ctx) = app_on(&dir);
            // It would read the folder again on its own, a moment after the swap.
            window.config.auto_refresh = false;
            (window, ctx)
        };
        let ((mut keyword, keyword_ctx), (mut rename, rename_ctx)) = (stale(), stale());
        let (mut delete, delete_ctx) = stale();
        let from = vec![first.listed_file(0), first.listed_file(1)];
        let picked = crate::files::file_io::PickedFiles::listed(from.clone());
        first.rename_in_background(&first_ctx, from, vec![b.clone(), a.clone()], picked);
        assert!(run_background_until(&mut first, &first_ctx, |app| app
            .file_operations
            .renaming
            .is_none()));
        let size_of = |p: &std::path::Path| image::image_dimensions(p).unwrap();
        assert_eq!(size_of(&a), (6, 6), "the names were not swapped");
        let idle = |app: &crate::App| {
            (app.file_operations.jobs.iter()).all(|job| std::sync::Arc::strong_count(job) == 1)
        };
        keyword.add_keywords_to_selected(&keyword_ctx, vec!["meant for the first photo".into()]);
        assert!(run_background_until(&mut keyword, &keyword_ctx, |app| !app
            .file_operations
            .is_writing_sidecars()));
        assert!(
            crate::metadata::xmp::read(&a).keywords.is_empty(),
            "a keyword landed"
        );
        let from = vec![rename.listed_file(0)];
        let picked = crate::files::file_io::PickedFiles::listed(from.clone());
        rename.rename_in_background(&rename_ctx, from, vec![c.clone()], picked);
        assert!(run_background_until(&mut rename, &rename_ctx, |app| app
            .file_operations
            .renaming
            .is_none()));
        assert!(!c.exists(), "the other photo was renamed");
        delete.delete_targets(&delete_ctx, false, vec![0], None);
        assert!(run_background_until(&mut delete, &delete_ctx, idle));
        assert!(a.exists() && b.exists(), "the other photo was deleted");
        drop(first);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_swap_whose_middle_did_not_reach_the_disk_is_undone() {
        let dir = std::env::temp_dir().join(format!("glimt-test-swap-eio-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = |n: &str| dir.join(n);
        std::fs::write(p("a.jpg"), "a").unwrap();
        std::fs::write(p("b.jpg"), "b").unwrap();
        let job = super::Job::new("Renaming", 2, false, Vec::new());
        // The renames' folder fails to sync, and the state folder with the plan does not.
        crate::files::file_io::FOLDER_SYNC_FAILS_IN.set(Some(dir.clone()));
        let swapped = rename_files(
            vec![p("a.jpg"), p("b.jpg")],
            vec![p("b.jpg"), p("a.jpg")],
            &job,
        );
        crate::files::file_io::FOLDER_SYNC_FAILS_IN.set(None);
        crate::files::file_io::take_saves_failed();
        assert!(swapped.is_err());
        let read = |n: &str| std::fs::read_to_string(p(n)).unwrap();
        assert_eq!((read("a.jpg"), read("b.jpg")), ("a".into(), "b".into()));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A swap whose step failed in a way that may have been done (a share that lost the answer
    /// to an unlink) undoes nothing and keeps its plan, which the next start puts right. Before,
    /// the steps before it were undone and the plan deleted, with "nothing renamed", which on a
    /// share left a photo only under its hidden name.
    #[test]
    fn a_rename_step_that_may_have_been_done_keeps_the_plan() {
        let dir = std::env::temp_dir().join(format!("glimt-test-swap-lost-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = |n: &str| dir.join(n);
        std::fs::write(p("a.jpg"), "a").unwrap();
        std::fs::write(p("b.jpg"), "b").unwrap();
        let job = super::Job::new("Renaming", 2, false, Vec::new());
        // The second step, b.jpg to its hidden name, is linked and its unlink answers EIO.
        crate::files::file_io::RENAME_WAYS_MISSING.set(1);
        crate::files::file_io::UNLINK_FAILS.set(Some((p("b.jpg"), libc::EIO)));
        let swapped = rename_files(
            vec![p("a.jpg"), p("b.jpg")],
            vec![p("b.jpg"), p("a.jpg")],
            &job,
        );
        crate::files::file_io::UNLINK_FAILS.set(None);
        crate::files::file_io::RENAME_WAYS_MISSING.set(0);
        let error = swapped.unwrap_err();
        assert!(
            !error.names_put_back && error.text.contains("next start"),
            "{}",
            error.text
        );
        let plans = super::journal::journal_dir().unwrap();
        let ours = super::journal::cut_short_plans(&plans)
            .into_iter()
            .find(|(_, moves, _)| moves[0].0 == p("a.jpg"));
        let (plan, moves, done) = ours.expect("the plan is kept");
        let (_, left) = super::journal::repair_cut_short(&plan, &moves, done, true).unwrap();
        assert_eq!(left, 0);
        let read = |n: &str| std::fs::read_to_string(p(n)).unwrap();
        assert_eq!((read("a.jpg"), read("b.jpg")), ("a".into(), "b".into()));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A swap whose step the disk refused (a new name longer than 255 bytes) is undone at once:
    /// "nothing renamed", no file left under a hidden name, and no plan kept for the next
    /// start. In 0.80.0 only five refusals were known, and every other one kept the files under
    /// hidden names until the next start.
    #[test]
    fn a_rename_step_the_disk_refused_is_undone_at_once() {
        let dir = std::env::temp_dir().join(format!("glimt-test-swap-long-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = |n: &str| dir.join(n);
        std::fs::write(p("a.jpg"), "a").unwrap();
        std::fs::write(p("b.jpg"), "b").unwrap();
        let long = format!("{}.jpg", "x".repeat(300));
        let job = super::Job::new("Renaming", 2, false, Vec::new());
        // The last step, b.jpg's hidden name to the long name, fails with ENAMETOOLONG.
        let swapped = rename_files(
            vec![p("a.jpg"), p("b.jpg")],
            vec![p("b.jpg"), p(&long)],
            &job,
        );
        let error = swapped.unwrap_err();
        assert!(error.text.contains("nothing renamed"), "{}", error.text);
        let read = |n: &str| std::fs::read_to_string(p(n)).unwrap();
        assert_eq!((read("a.jpg"), read("b.jpg")), ("a".into(), "b".into()));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
        let plans = super::journal::journal_dir().unwrap();
        let kept = super::journal::cut_short_plans(&plans)
            .into_iter()
            .any(|(_, moves, _)| moves[0].0 == p("a.jpg"));
        assert!(!kept, "a plan was kept");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A Move or Copy whose sidecar cannot follow puts the file back, with the sidecars that
    /// had followed, and says so. Before, the photo was left at the target without its keywords
    /// and the sidecar at the source, where a later file of that name would pick it up.
    #[test]
    fn a_file_goes_only_with_its_sidecars() {
        use crate::files::file_io::{RENAME_WAYS_MISSING, UNLINK_FAILS};
        let dir = std::env::temp_dir().join(format!("glimt-test-split-{}", std::process::id()));
        let (from, to) = (dir.join("from"), dir.join("to"));
        for is_copy in [true, false] {
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&from).unwrap();
            std::fs::create_dir_all(&to).unwrap();
            std::fs::write(from.join("a.cr2"), "raw").unwrap();
            std::fs::write(from.join("a.cr2.xmp"), "own").unwrap();
            // Adobe's sidecar, which follows second, is a socket, which cannot be read to
            // copy it. A move cannot take it either: its old name cannot be unlinked.
            let _socket = std::os::unix::net::UnixListener::bind(from.join("a.xmp")).unwrap();
            RENAME_WAYS_MISSING.set(1);
            UNLINK_FAILS.set(Some((from.join("a.xmp"), libc::EPERM)));
            let job = super::Job::new("Moving", 1, false, Vec::new());
            let paths = [from.join("a.cr2")];
            let picked = crate::files::file_io::PickedFiles::new(paths.to_vec());
            let (_, picked) = picked.wait_for_fingerprints();
            let (errors, put) = super::put_files(&paths, (&picked, &to), (is_copy, None), &job);
            UNLINK_FAILS.set(None);
            RENAME_WAYS_MISSING.set(0);
            assert!(put.is_empty(), "{is_copy}");
            assert_eq!(errors.len(), 1, "{is_copy}");
            assert!(errors[0].contains("stays where it was"), "{}", errors[0]);
            assert_eq!(std::fs::read_to_string(from.join("a.cr2")).unwrap(), "raw");
            assert_eq!(
                std::fs::read_to_string(from.join("a.cr2.xmp")).unwrap(),
                "own"
            );
            assert_eq!(std::fs::read_dir(&to).unwrap().count(), 0, "{is_copy}");
        }
        // The Undo of a Move: the file stays with its sidecars, and the Undo is left to try
        // again.
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&from).unwrap();
        std::fs::create_dir_all(&to).unwrap();
        std::fs::write(to.join("a.cr2"), "raw").unwrap();
        std::fs::write(to.join("a.cr2.xmp"), "own").unwrap();
        let _socket = std::os::unix::net::UnixListener::bind(to.join("a.xmp")).unwrap();
        let now = crate::files::file_io::fingerprint(&to.join("a.cr2")).unwrap();
        RENAME_WAYS_MISSING.set(1);
        UNLINK_FAILS.set(Some((to.join("a.xmp"), libc::EPERM)));
        let (back, left, errors) =
            super::move_back(vec![(from.join("a.cr2"), to.join("a.cr2"), now)]);
        UNLINK_FAILS.set(None);
        RENAME_WAYS_MISSING.set(0);
        assert!(back.is_empty() && left.len() == 1, "{errors:?}");
        assert!(errors[0].contains("stays where it was"), "{}", errors[0]);
        assert_eq!(
            std::fs::read_to_string(to.join("a.cr2.xmp")).unwrap(),
            "own"
        );
        assert_eq!(std::fs::read_dir(&from).unwrap().count(), 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Two RAW files of one name stem move together. The first one's own sidecar follows it at
    /// once, and when it cannot, that file goes back to it before the second file is moved, as
    /// any other file does, also in the Undo of a Move. Adobe's sidecar of the stem then stays
    /// beside the file that went back. Before, the first file's own sidecar waited until the
    /// job's end, so a kill in between left it apart from its photo, and when it then could not
    /// follow, the photo went back without the stem's sidecar, which had gone with the second.
    #[test]
    fn a_raw_file_goes_only_with_its_own_sidecar() {
        use crate::files::file_io::{RENAME_WAYS_MISSING, UNLINK_FAILS};
        let dir =
            std::env::temp_dir().join(format!("glimt-test-stem-split-{}", std::process::id()));
        let (from, to) = (dir.join("from"), dir.join("to"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&from).unwrap();
        std::fs::create_dir_all(&to).unwrap();
        std::fs::write(from.join("a.cr2"), "cr2").unwrap();
        std::fs::write(from.join("a.nef"), "nef").unwrap();
        std::fs::write(from.join("a.xmp"), "stem").unwrap();
        // a.cr2's own sidecar is a socket, which can be neither moved (its old name cannot be
        // unlinked) nor copied.
        let _socket = std::os::unix::net::UnixListener::bind(from.join("a.cr2.xmp")).unwrap();
        let job = super::Job::new("Moving", 2, false, Vec::new());
        let paths = [from.join("a.cr2"), from.join("a.nef")];
        let picked = crate::files::file_io::PickedFiles::new(paths.to_vec());
        let (_, picked) = picked.wait_for_fingerprints();
        RENAME_WAYS_MISSING.set(1);
        UNLINK_FAILS.set(Some((from.join("a.cr2.xmp"), libc::EPERM)));
        let (errors, put) = super::put_files(&paths, (&picked, &to), (false, None), &job);
        UNLINK_FAILS.set(None);
        assert_eq!(put, [(from.join("a.nef"), to.join("a.nef"))], "{errors:?}");
        assert!(errors[0].contains("stays where it was"), "{errors:?}");
        assert_eq!(std::fs::read_to_string(from.join("a.cr2")).unwrap(), "cr2");
        assert_eq!(std::fs::read_to_string(from.join("a.xmp")).unwrap(), "stem");
        // The Undo of the Move, with the socket now beside the moved file.
        std::fs::rename(from.join("a.cr2"), to.join("a.cr2")).unwrap();
        std::fs::rename(from.join("a.xmp"), to.join("a.xmp")).unwrap();
        std::fs::remove_file(from.join("a.cr2.xmp")).unwrap();
        let _socket = std::os::unix::net::UnixListener::bind(to.join("a.cr2.xmp")).unwrap();
        let moved = |n: &str| {
            let now = crate::files::file_io::fingerprint(&to.join(n)).unwrap();
            (from.join(n), to.join(n), now)
        };
        UNLINK_FAILS.set(Some((to.join("a.cr2.xmp"), libc::EPERM)));
        let (back, left, errors) = super::move_back(vec![moved("a.cr2"), moved("a.nef")]);
        UNLINK_FAILS.set(None);
        RENAME_WAYS_MISSING.set(0);
        assert_eq!(back, [(to.join("a.nef"), from.join("a.nef"))], "{errors:?}");
        assert_eq!(left.len(), 1, "{errors:?}");
        assert_eq!(left[0].1, to.join("a.cr2"));
        assert_eq!(std::fs::read_to_string(to.join("a.cr2")).unwrap(), "cr2");
        assert_eq!(std::fs::read_to_string(to.join("a.xmp")).unwrap(), "stem");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A Move of many files of one folder, and its Undo, read the mount table for the sidecars'
    /// locks once each, not once for each file. Before, each file's lock read and parsed
    /// /proc/self/mountinfo again, which for 500,000 files took tens of seconds more.
    #[test]
    fn a_move_and_its_undo_read_the_mount_table_once() {
        use crate::files::file_io::MOUNT_TABLES_PARSED;
        let dir =
            std::env::temp_dir().join(format!("glimt-test-move-mounts-{}", std::process::id()));
        let (from, to) = (dir.join("from"), dir.join("to"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&from).unwrap();
        std::fs::create_dir_all(&to).unwrap();
        let paths: Vec<_> = (0..20).map(|i| from.join(format!("{i}.jpg"))).collect();
        for path in &paths {
            std::fs::write(path, "jpg").unwrap();
        }
        let job = super::Job::new("Moving", paths.len(), false, Vec::new());
        let picked = crate::files::file_io::PickedFiles::new(paths.clone());
        let (_, picked) = picked.wait_for_fingerprints();
        MOUNT_TABLES_PARSED.set(0);
        let (errors, put) = super::put_files(&paths, (&picked, &to), (false, None), &job);
        assert_eq!(put.len(), paths.len(), "{errors:?}");
        assert!(
            MOUNT_TABLES_PARSED.get() <= 2,
            "{}",
            MOUNT_TABLES_PARSED.get()
        );
        let moved = (put.iter())
            .map(|(from, to)| {
                (
                    from.clone(),
                    to.clone(),
                    crate::files::file_io::fingerprint(to).unwrap(),
                )
            })
            .collect();
        MOUNT_TABLES_PARSED.set(0);
        let (back, _, errors) = super::move_back(moved);
        assert_eq!(back.len(), paths.len(), "{errors:?}");
        assert!(
            MOUNT_TABLES_PARSED.get() <= 2,
            "{}",
            MOUNT_TABLES_PARSED.get()
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A file whose sidecar cannot follow goes back first, and the sidecars that followed go
    /// back only once it is there, every one of them. When it cannot go back, it keeps every
    /// sidecar that reached it, and the error says where it and the sidecar left behind are.
    /// Before, the sidecars were taken back first, stopping at the first that could not go,
    /// and a file that then could not go back stayed at the target without them.
    #[test]
    fn a_file_ends_with_every_sidecar_that_could_reach_it() {
        use crate::files::file_io::{RENAME_WAYS_MISSING, UNLINK_FAILS};
        let dir = std::env::temp_dir().join(format!("glimt-test-way-back-{}", std::process::id()));
        let (from, to) = (dir.join("from"), dir.join("to"));
        let read = |p: &std::path::Path| std::fs::read_to_string(p).unwrap();
        for can_go_back in [false, true] {
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&from).unwrap();
            std::fs::create_dir_all(&to).unwrap();
            // The file is at the target already. Its own sidecar cannot follow, as a sidecar
            // of that name is there, and Adobe's, which comes second, can.
            std::fs::write(to.join("a.cr2"), "raw").unwrap();
            std::fs::write(from.join("a.cr2.xmp"), "own").unwrap();
            std::fs::write(to.join("a.cr2.xmp"), "there").unwrap();
            std::fs::write(from.join("a.xmp"), "adobe").unwrap();
            if !can_go_back {
                // Another file has taken the old name.
                std::fs::write(from.join("a.cr2"), "another").unwrap();
            }
            let photo = from.join("a.cr2");
            let moving = std::collections::HashSet::from([&photo]);
            // Adobe's sidecar, once it followed, cannot be taken back.
            RENAME_WAYS_MISSING.set(1);
            UNLINK_FAILS.set(Some((to.join("a.xmp"), libc::EPERM)));
            let result = super::follow_or_go_back(
                (&photo, &to.join("a.cr2")),
                false,
                &moving,
                &mut Default::default(),
            );
            UNLINK_FAILS.set(None);
            RENAME_WAYS_MISSING.set(0);
            let Err((is_back, told)) = result else {
                panic!("every sidecar followed")
            };
            assert_eq!(is_back, can_go_back, "{told:?}");
            assert_eq!(read(&to.join("a.xmp")), "adobe", "{told:?}");
            assert!(!from.join("a.xmp").exists());
            assert_eq!(read(&from.join("a.cr2.xmp")), "own");
            match can_go_back {
                true => {
                    assert_eq!(read(&from.join("a.cr2")), "raw");
                    assert!(told[0].contains("went back, but its sidecar"), "{told:?}");
                    assert!(!told[1].contains("with its sidecar"), "{told:?}");
                }
                false => {
                    assert_eq!(read(&to.join("a.cr2")), "raw");
                    assert_eq!(told.len(), 1, "{told:?}");
                    assert!(told[0].contains("could not go back"), "{told:?}");
                }
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A sidecar whose move may have been done (its old name's unlink lost its answer, so it is
    /// under both names) is not tried again as a copy, and ends beside its file alone: at its
    /// old name when the file went back, else at the new one. Before, the copy found the new
    /// name taken, the file went back "with its sidecar", and the sidecar stayed at the new name
    /// too, for the next file of that name.
    #[test]
    fn a_sidecar_that_may_have_moved_ends_beside_its_file() {
        use crate::files::file_io::{RENAME_WAYS_MISSING, UNLINK_FAILS};
        let dir =
            std::env::temp_dir().join(format!("glimt-test-side-maybe-{}", std::process::id()));
        let (from, to) = (dir.join("from"), dir.join("to"));
        for can_go_back in [true, false] {
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&from).unwrap();
            std::fs::create_dir_all(&to).unwrap();
            std::fs::write(to.join("a.jpg"), "photo").unwrap();
            std::fs::write(from.join("a.jpg.xmp"), "own").unwrap();
            if !can_go_back {
                std::fs::write(from.join("a.jpg"), "another").unwrap();
            }
            let photo = from.join("a.jpg");
            let moving = std::collections::HashSet::from([&photo]);
            RENAME_WAYS_MISSING.set(1);
            UNLINK_FAILS.set(Some((from.join("a.jpg.xmp"), libc::EIO)));
            let result = super::follow_or_go_back(
                (&photo, &to.join("a.jpg")),
                false,
                &moving,
                &mut Default::default(),
            );
            UNLINK_FAILS.set(None);
            RENAME_WAYS_MISSING.set(0);
            let Err((is_back, told)) = result else {
                panic!("the sidecar followed")
            };
            assert_eq!(is_back, can_go_back, "{told:?}");
            let (beside, away) = match can_go_back {
                true => (from.join("a.jpg.xmp"), to.join("a.jpg.xmp")),
                false => (to.join("a.jpg.xmp"), from.join("a.jpg.xmp")),
            };
            assert_eq!(std::fs::read_to_string(&beside).unwrap(), "own");
            assert!(!away.exists(), "{told:?}");
            match can_go_back {
                true => assert!(told[0].contains("with its sidecar"), "{told:?}"),
                false => assert!(told.is_empty(), "{told:?}"),
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A sidecar that cannot be moved (someone else's in a shared folder) is copied with its
    /// file, and the move tells that it is still at the old name, where the next file of that
    /// name would take it. Before, it was left there without a word.
    #[test]
    fn a_sidecar_left_behind_is_told() {
        use crate::files::file_io::{RENAME_WAYS_MISSING, UNLINK_FAILS};
        let dir =
            std::env::temp_dir().join(format!("glimt-test-left-behind-{}", std::process::id()));
        let (from, to) = (dir.join("from"), dir.join("to"));
        std::fs::create_dir_all(&from).unwrap();
        std::fs::create_dir_all(&to).unwrap();
        std::fs::write(to.join("a.jpg"), "photo").unwrap();
        std::fs::write(from.join("a.jpg.xmp"), "keywords").unwrap();
        let photo = from.join("a.jpg");
        RENAME_WAYS_MISSING.set(1);
        UNLINK_FAILS.set(Some((from.join("a.jpg.xmp"), libc::EPERM)));
        let result = super::follow_or_go_back(
            (&photo, &to.join("a.jpg")),
            false,
            &std::collections::HashSet::from([&photo]),
            &mut Default::default(),
        );
        UNLINK_FAILS.set(None);
        RENAME_WAYS_MISSING.set(0);
        let Err((false, told)) = &result else {
            panic!("{result:?}")
        };
        assert!(told[0].contains("is still at its old name"), "{told:?}");
        assert_eq!(std::fs::read(to.join("a.jpg.xmp")).unwrap(), b"keywords");
        assert_eq!(std::fs::read(to.join("a.jpg")).unwrap(), b"photo");
        assert!(from.join("a.jpg.xmp").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// An Undo of a Move whose file went back to its sidecars keeps the Undo when the look at
    /// the file then fails, and does not count the file as moved back. Before, the file was
    /// counted at its old name, where its marks would have followed it, or its Undo was lost.
    #[test]
    fn a_failed_look_keeps_the_undo_of_a_move() {
        use crate::files::file_io::{LOOK_FAILS, RENAME_WAYS_MISSING, UNLINK_FAILS};
        let dir = std::env::temp_dir().join(format!("glimt-test-undo-look-{}", std::process::id()));
        let (from, to) = (dir.join("from"), dir.join("to"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&from).unwrap();
        std::fs::create_dir_all(&to).unwrap();
        std::fs::write(to.join("a.cr2"), "cr2").unwrap();
        std::fs::write(to.join("a.nef"), "nef").unwrap();
        // a.cr2's own sidecar is a socket, which can be neither moved (its old name cannot be
        // unlinked) nor copied.
        let _socket = std::os::unix::net::UnixListener::bind(to.join("a.cr2.xmp")).unwrap();
        let moved = |n: &str| {
            let now = crate::files::file_io::fingerprint(&to.join(n)).unwrap();
            (from.join(n), to.join(n), now)
        };
        // a.cr2 alone, and then with another RAW file of its stem, which takes the stem's sidecar.
        for pairs in [vec![moved("a.cr2")], vec![moved("a.cr2"), moved("a.nef")]] {
            RENAME_WAYS_MISSING.set(1);
            UNLINK_FAILS.set(Some((to.join("a.cr2.xmp"), libc::EPERM)));
            // The look before the move back answers, and the one after it does not.
            LOOK_FAILS.set(Some((to.join("a.cr2"), 1)));
            let at_move = pairs[0].2;
            let (back, left, errors) = super::move_back(pairs);
            LOOK_FAILS.set(None);
            UNLINK_FAILS.set(None);
            RENAME_WAYS_MISSING.set(0);
            assert!(
                back.iter().all(|(now, _)| *now != to.join("a.cr2")),
                "{errors:?}"
            );
            assert_eq!(left.len(), 1, "{errors:?}");
            assert_eq!((&left[0].1, left[0].2), (&to.join("a.cr2"), at_move));
            assert_eq!(std::fs::read_to_string(to.join("a.cr2")).unwrap(), "cr2");
            // The second round starts again from the target.
            if from.join("a.nef").exists() {
                std::fs::rename(from.join("a.nef"), to.join("a.nef")).unwrap();
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A Move to Trash whose sidecar cannot go to the trash takes the photo back out of it, and
    /// says so. Before, the photo stayed in the trash and the sidecar at its old name, where the
    /// next file of that name would pick up its keywords.
    #[test]
    fn a_file_goes_to_the_trash_only_with_its_sidecars() {
        use crate::files::file_io::{RENAME_WAYS_MISSING, UNLINK_FAILS};
        let dir =
            std::env::temp_dir().join(format!("glimt-test-trash-split-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (photo, side) = (dir.join("a.jpg"), dir.join("a.jpg.xmp"));
        std::fs::write(&photo, "photo").unwrap();
        std::fs::write(&side, "own").unwrap();
        let picked = crate::files::file_io::PickedFiles::new(vec![photo.clone()]);
        let (_, picked) = picked.wait_for_fingerprints();
        // The sidecar's old name cannot be unlinked, so it cannot be moved into the trash.
        RENAME_WAYS_MISSING.set(1);
        UNLINK_FAILS.set(Some((side.clone(), libc::EPERM)));
        let mut errors = Vec::new();
        let (went, _, _) = super::delete_chunk(
            std::slice::from_ref(&photo),
            &picked,
            (true, None),
            &mut errors,
        );
        UNLINK_FAILS.set(None);
        RENAME_WAYS_MISSING.set(0);
        assert!(went.is_empty(), "{errors:?}");
        assert!(errors[0].contains("stays where it was"), "{errors:?}");
        assert_eq!(std::fs::read_to_string(&photo).unwrap(), "photo");
        assert_eq!(std::fs::read_to_string(&side).unwrap(), "own");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Glimt killed during a Move to Trash never leaves a sidecar at the name of a photo that
    /// went. Before, a chunk's photos went to the trash in one call and their sidecars in a
    /// second, and a kill between the two left each sidecar for the next file of its photo's
    /// name. The kill here is a panic when the sidecar's turn to go comes.
    #[test]
    fn a_kill_during_a_move_to_trash_leaves_no_sidecar_without_its_photo() {
        use crate::support::threads::{BREAKING_FILES, catch_panic};
        let dir =
            std::env::temp_dir().join(format!("glimt-test-trash-killed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (photo, side) = (dir.join("a.jpg"), dir.join("a.jpg.xmp"));
        std::fs::write(&photo, "photo").unwrap();
        std::fs::write(&side, "own").unwrap();
        let picked = crate::files::file_io::PickedFiles::new(vec![photo.clone()]);
        let (_, picked) = picked.wait_for_fingerprints();
        lock_even_if_poisoned(&BREAKING_FILES).push(side.clone());
        let mut errors = Vec::new();
        let photos = std::slice::from_ref(&photo);
        let cut = catch_panic(|| super::delete_chunk(photos, &picked, (true, None), &mut errors));
        lock_even_if_poisoned(&BREAKING_FILES).retain(|p| *p != side);
        assert!(cut.is_none(), "the kill came");
        assert!(
            photo.exists() || !side.exists(),
            "the sidecar is left at the name of a photo that went"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A Move to Trash that takes a photo's sidecar but not the photo takes the sidecar back
    /// out of the trash, so that the photo keeps its keywords.
    #[test]
    fn a_sidecar_comes_back_out_of_the_trash_when_its_photo_stays() {
        use crate::files::file_io::{RENAME_WAYS_MISSING, UNLINK_FAILS};
        let dir = std::env::temp_dir().join(format!(
            "glimt-test-trash-photo-stays-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (photo, side) = (dir.join("a.jpg"), dir.join("a.jpg.xmp"));
        std::fs::write(&photo, "photo").unwrap();
        std::fs::write(&side, "own").unwrap();
        let picked = crate::files::file_io::PickedFiles::new(vec![photo.clone()]);
        let (_, picked) = picked.wait_for_fingerprints();
        // The photo's name cannot be unlinked, so the photo cannot be moved into the trash.
        RENAME_WAYS_MISSING.set(1);
        UNLINK_FAILS.set(Some((photo.clone(), libc::EPERM)));
        let mut errors = Vec::new();
        let (went, _, _) = super::delete_chunk(
            std::slice::from_ref(&photo),
            &picked,
            (true, None),
            &mut errors,
        );
        UNLINK_FAILS.set(None);
        RENAME_WAYS_MISSING.set(0);
        assert!(went.is_empty(), "{errors:?}");
        assert!(!errors.is_empty());
        assert_eq!(std::fs::read_to_string(&photo).unwrap(), "photo");
        assert_eq!(std::fs::read_to_string(&side).unwrap(), "own", "{errors:?}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A Move to Trash of two RAW files of one name stem, of which one cannot go, leaves
    /// Adobe's sidecar of the stem beside the one that stays, whichever of the two it is. It
    /// goes with the last of them, as a Move and a delete for good give it, and comes back out
    /// of the trash to the one that stayed. In 0.84 before this, it went with the first, and a
    /// second that stayed lost it to the trash.
    #[test]
    fn adobe_sidecar_stays_with_the_raw_file_of_its_stem_that_stays_in_a_trash() {
        use crate::files::file_io::{RENAME_WAYS_MISSING, UNLINK_FAILS};
        let dir = std::env::temp_dir().join(format!(
            "glimt-test-trash-stem-stays-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (cr2, nef, side) = (
            dir.join("IMG_1.CR2"),
            dir.join("IMG_1.NEF"),
            dir.join("IMG_1.XMP"),
        );
        let both = [cr2.clone(), nef.clone()];
        for (stays, goes) in [(&nef, &cr2), (&cr2, &nef)] {
            std::fs::write(&cr2, "cr2").unwrap();
            std::fs::write(&nef, "nef").unwrap();
            std::fs::write(&side, "adobe").unwrap();
            let picked = crate::files::file_io::PickedFiles::new(both.to_vec());
            let (_, picked) = picked.wait_for_fingerprints();
            RENAME_WAYS_MISSING.set(1);
            UNLINK_FAILS.set(Some((stays.clone(), libc::EPERM)));
            let mut errors = Vec::new();
            let (went, _, _) = super::delete_chunk(&both, &picked, (true, None), &mut errors);
            UNLINK_FAILS.set(None);
            RENAME_WAYS_MISSING.set(0);
            assert_eq!(went, std::slice::from_ref(goes), "{errors:?}");
            assert!(stays.exists() && !goes.exists());
            let read = std::fs::read_to_string(&side);
            assert_eq!(read.ok().as_deref(), Some("adobe"), "{stays:?} {errors:?}");
            // The one that went comes back, so the next round and the trash start clean.
            let trash_move = super::TrashMove {
                files: vec![goes.clone()],
                sidecars: Vec::new(),
                sidecars_left: Vec::new(),
                since: std::time::UNIX_EPOCH,
                marks: Default::default(),
                collection: None,
            };
            let (back, error) = super::restore(&trash_move, super::trash_dirs);
            assert_eq!(back, std::slice::from_ref(goes), "{error:?}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A rename whose plan cannot be written renames nothing and says so. Before, Rename Many
    /// ran without its plan, and on a full state folder the marks file could not follow the
    /// swapped names either, so most photos showed another photo's stars (issue #90).
    #[test]
    fn a_rename_without_its_plan_renames_nothing() {
        let dir = std::env::temp_dir().join(format!("glimt-test-no-plan-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = |n: &str| dir.join(n);
        for n in ["a.jpg", "b.jpg", "a.jpg.xmp"] {
            std::fs::write(p(n), n).unwrap();
        }
        // A file where the plans' folder should be, so that no plan can be made in it.
        std::fs::write(p("plans"), "").unwrap();
        let job = super::Job::new("Renaming", 2, false, Vec::new());
        let refused = super::rename_files_planned_in(
            Some(p("plans")),
            vec![p("a.jpg"), p("b.jpg")],
            vec![p("b.jpg"), p("a.jpg")],
            &job,
            crate::metadata::xmp::lock_sidecars(&[p("a.jpg"), p("b.jpg")]),
        );
        let refused = refused.err().unwrap();
        assert!(refused.names_put_back);
        assert!(
            refused.text.contains("nothing was renamed"),
            "{}",
            refused.text
        );
        for n in ["a.jpg", "b.jpg", "a.jpg.xmp"] {
            assert_eq!(std::fs::read_to_string(p(n)).unwrap(), n);
        }
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
        let job = super::Job::new("Renaming", 2, false, Vec::new());
        rename_files(
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
        assert!(rename_files(vec![p("b.jpg")], vec![p("c.jpg")], &job).is_err());
        // This also holds for a file without a sidecar of its own, because it would show c's
        // keywords.
        std::fs::write(p("d.jpg"), "d").unwrap();
        assert!(rename_files(vec![p("d.jpg")], vec![p("c.jpg")], &job).is_err());
        // A sidecar that is a link pointing nowhere goes with its file too, not left behind for
        // the next file of the old name.
        crate::files::file_io::create_file(&p("f.jpg"), "f").unwrap();
        std::os::unix::fs::symlink(p("nowhere"), p("f.jpg.xmp")).unwrap();
        rename_files(vec![p("f.jpg")], vec![p("g.jpg")], &job).unwrap();
        assert!(std::fs::symlink_metadata(p("f.jpg.xmp")).is_err());
        assert!(crate::files::file_io::is_link(&p("g.jpg.xmp")).unwrap());
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
        assert!(rename_files(vec![p("g.jpg")], vec![p("G.JPG")], &job).is_err());
        assert!(read("g.jpg") == "f" && read("G.JPG") == "f");
        std::fs::remove_file(p("G.JPG")).unwrap();
        // A hard link under another name is a name of its own: renaming onto it is refused,
        // and both names stay.
        std::fs::hard_link(p("g.jpg"), p("h.jpg")).unwrap();
        assert!(rename_files(vec![p("g.jpg")], vec![p("h.jpg")], &job).is_err());
        assert!(p("g.jpg").exists() && p("h.jpg").exists());
        std::fs::remove_file(p("h.jpg")).unwrap();
        // Stopped before the first step, nothing is renamed, and the error says it stopped.
        job.stop_requested
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let stopped = rename_files(vec![p("d.jpg")], vec![p("e.jpg")], &job);
        assert!(stopped.unwrap_err().text.contains("stopped") && p("d.jpg").exists());
        assert!(p("d.jpg").exists() && !p("c.jpg").exists());
        // The one rule, by name. The file in the way comes first, then the other file's
        // sidecar, and the operation's own names never count.
        let ours: std::collections::HashSet<&std::path::PathBuf> = Default::default();
        assert_eq!(
            super::name_in_the_way(&p("c.jpg"), &ours, &mut Default::default()),
            Some(p("c.jpg.xmp"))
        );
        assert_eq!(
            super::name_in_the_way(&p("b.jpg"), &ours, &mut Default::default()),
            Some(p("b.jpg"))
        );
        assert_eq!(
            super::name_in_the_way(&p("z.jpg"), &ours, &mut Default::default()),
            None
        );
        let (b, bx) = (p("b.jpg"), p("b.jpg.xmp"));
        let ours = std::collections::HashSet::from([&b, &bx]);
        assert_eq!(
            super::name_in_the_way(&p("b.jpg"), &ours, &mut Default::default()),
            None
        );
        // A file renamed to another file's sidecar name would become that file's sidecar.
        assert_eq!(
            super::name_in_the_way(
                &p("d.jpg.xmp"),
                &Default::default(),
                &mut Default::default()
            ),
            Some(p("d.jpg"))
        );
        let refused = rename_files(vec![p("b.jpg")], vec![p("d.jpg.xmp")], &job);
        assert!(refused.unwrap_err().text.contains("d.jpg exists already"));
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
        assert!(app.run_in_background(&ctx, Vec::new(), || None));
        drop(held);
        let (go, wait) = std::sync::mpsc::channel::<()>();
        assert!(app.run_in_background(&ctx, vec![a.clone()], move || {
            let _ = wait.recv();
            None
        }));
        app.delete_targets(&ctx, false, vec![0], None);
        assert!(a.exists() && app.notice.text().is_some());
        assert_eq!(app.list.files().len(), 2);
        app.file_operations.undo = Some(super::Undo::Move(vec![(
            a.clone(),
            dir.join("c.png"),
            (None, 0, (0, 0)),
        )]));
        app.undo(&ctx);
        assert!(app.file_operations.undo.is_some());
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
            let now = app.are_free_to_change(std::slice::from_ref(&a));
            if !now {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            now
        });
        assert!(free);
        app.file_operations.undo = None;
        app.delete_targets(&ctx, false, vec![0], None);
        assert!(wait_for(&|| !a.exists()));
        // Thousands against thousands are answered at once, not path against path (10^8
        // comparisons froze the window for seconds).
        let many = |top: &str| -> Vec<std::path::PathBuf> {
            (0..50_000)
                .map(|k| dir.join(top).join(format!("{k}.jpg")))
                .collect()
        };
        let held = std::sync::Arc::new(std::sync::Mutex::new(many("busy")));
        app.file_operations.held_back_files.push(held.clone());
        let asked = many("free");
        let start = std::time::Instant::now();
        assert!(app.are_free_to_change(&asked));
        assert!(!app.are_free_to_change(&[dir.join("busy")]));
        assert!(!app.are_free_to_change(&[dir.join("busy/7.jpg")]));
        // Path against path would take minutes. Five seconds leaves room for a machine busy
        // with other builds, and for a debug build, where one second failed.
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
        drop(held);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// An operation of the contract, run on a.png (and b.png) of `contract_folder`.
    type Op = fn(&mut crate::App, &eframe::egui::Context, &std::path::Path);
    /// What must hold after an operation of the contract.
    type Check = fn(&crate::App, &std::path::Path) -> bool;

    /// Returns the operations of the contract, by name, each with what must hold afterwards.
    /// Each is held back while other work changes its files, except those that change none. A
    /// copy only reads them, and keywords never wait, because an edit would be lost. Their job
    /// holds back the rest instead.
    fn contract_ops() -> [(&'static str, Op, Check); 17] {
        [
            (
                "move",
                |app, _, d| {
                    app.transfer(
                        crate::files::file_io::PickedFiles::new(vec![d.join("a.png")]),
                        d.join("sub"),
                        false,
                    )
                },
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
                |app, _, d| {
                    app.transfer(
                        crate::files::file_io::PickedFiles::new(vec![d.join("a.png")]),
                        d.join("sub"),
                        true,
                    )
                },
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
            // A copy for sharing takes neither the sidecar nor the stars along: what it keeps
            // of them is inside the copy. The original and its sidecar stay as they were.
            (
                "copy for sharing",
                |app, _, d| {
                    let paths = crate::files::file_io::PickedFiles::new(vec![d.join("a.png")]);
                    app.copy_for_sharing(paths, d.join("sub"), Default::default(), false)
                },
                |app, d| {
                    let (to, a) = (d.join("sub/a.png"), d.join("a.png"));
                    to.exists()
                        && image::image_dimensions(&to).ok() == Some((8, 8))
                        && a.exists()
                        && side(&a) == "a"
                        && !crate::metadata::xmp::sidecar(&to).exists()
                        && (!cfg!(feature = "marks")
                            || (stars(app, &to) == 0 && stars(app, &a) == 3))
                },
            ),
            (
                "rename",
                |app, ctx, d| {
                    app.rename_in_background(
                        ctx,
                        crate::support::test_support::unlisted(vec![d.join("a.png")]),
                        vec![d.join("c.png")],
                        crate::files::file_io::PickedFiles::new(vec![d.join("a.png")]),
                    )
                },
                |app, d| {
                    let to = d.join("c.png");
                    to.exists()
                        && !d.join("a.png").exists()
                        && side(&to) == "a"
                        && app.file_operations.renaming.is_none()
                        && (!cfg!(feature = "marks") || stars(app, &to) == 3)
                },
            ),
            (
                "rename many",
                |app, ctx, d| {
                    app.list.select([0, 1].into_iter().collect());
                    let picked = vec![d.join("a.png"), d.join("b.png")];
                    app.rename_many(ctx, "x#", crate::files::file_io::PickedFiles::new(picked));
                },
                |app, d| {
                    side(&d.join("x1.png")) == "a"
                        && side(&d.join("x2.png")) == "b"
                        && app.file_operations.renaming.is_none()
                        && (!cfg!(feature = "marks")
                            || (stars(app, &d.join("x1.png")) == 3
                                && stars(app, &d.join("x2.png")) == 2))
                },
            ),
            (
                "delete",
                |app, ctx, _| app.delete_targets(ctx, false, vec![0], None),
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
                    let a = crate::files::file_io::PickedFiles::new(vec![d.join("a.png")]);
                    let b = d.join("b.png");
                    let at_pick = crate::files::file_io::fingerprint(&b);
                    app.save_as_to(ctx, a, how, (b, at_pick));
                },
                |app, d| {
                    side(&d.join("b.png")) == "a"
                        && (!cfg!(feature = "marks") || stars(app, &d.join("b.png")) == 3)
                },
            ),
            (
                "save frame over b",
                |app, ctx, d| {
                    let how = app.shown_copy();
                    let b = d.join("b.png");
                    let at_pick = crate::files::file_io::fingerprint(&b);
                    app.export_frames(ctx, d.join("g.gif"), Some(0), (b, at_pick), how)
                },
                |app, d| {
                    !crate::metadata::xmp::sidecar(&d.join("b.png")).exists()
                        && (!cfg!(feature = "marks") || stars(app, &d.join("b.png")) == 0)
                },
            ),
            (
                "undo a move",
                |app, ctx, d| {
                    app.transfer(
                        crate::files::file_io::PickedFiles::new(vec![d.join("a.png")]),
                        d.join("sub"),
                        false,
                    );
                    // This waits only when the operation started. When it was held back, it
                    // says so, and there is nothing to wait for.
                    if app.notice.text().is_none() {
                        run_background_until(app, ctx, |app| app.file_operations.undo.is_some());
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
                    app.rename_in_background(
                        ctx,
                        crate::support::test_support::unlisted(vec![d.join("a.png")]),
                        vec![d.join("c.png")],
                        crate::files::file_io::PickedFiles::new(vec![d.join("a.png")]),
                    );
                    // This waits only when the operation started. When it was held back, it
                    // says so, and there is nothing to wait for.
                    if app.notice.text().is_none() {
                        run_background_until(app, ctx, |app| app.file_operations.undo.is_some());
                    }
                    app.undo(ctx);
                },
                |app, d| {
                    let a = d.join("a.png");
                    a.exists()
                        && !d.join("c.png").exists()
                        && side(&a) == "a"
                        && app.file_operations.renaming.is_none()
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
                        crate::support::test_support::unlisted_with(vec![(d.join("a.png"), ())]),
                        |p, _| crate::metadata::xmp::add_tags(p, &["Tag".into()]),
                        |_, _| None,
                    );
                },
                |_, d| side(&d.join("a.png")).contains("Tag"),
            ),
            (
                "trash",
                |app, ctx, _| app.delete_targets(ctx, true, vec![0], None),
                |app, d| {
                    !d.join("a.png").exists()
                        && !crate::metadata::xmp::sidecar(&d.join("a.png")).exists()
                        && matches!(app.file_operations.undo, Some(super::Undo::Trash(_)))
                        && (!cfg!(feature = "marks") || stars(app, &d.join("a.png")) == 0)
                },
            ),
            (
                "undo a trash",
                |app, ctx, _| {
                    app.delete_targets(ctx, true, vec![0], None);
                    // This waits only when the operation started. When it was held back, it
                    // says so, and there is nothing to wait for.
                    if app.notice.text().is_none() {
                        run_background_until(app, ctx, |app| app.file_operations.undo.is_some());
                    }
                    app.undo(ctx);
                },
                |app, d| {
                    let a = d.join("a.png");
                    a.exists()
                        && side(&a) == "a"
                        && (!cfg!(feature = "marks") || stars(app, &a) == 3)
                },
            ),
            // The contract's folder itself goes to the trash, with what follows its files.
            (
                "trash folder",
                |app, ctx, d| {
                    app.dialogs.act_on_folder(d.to_path_buf());
                    app.trash_folder(ctx);
                },
                |app, d| {
                    !d.exists()
                        && matches!(app.file_operations.undo, Some(super::Undo::Trash(_)))
                        && (!cfg!(feature = "marks") || stars(app, &d.join("a.png")) == 0)
                },
            ),
            (
                "undo a trash folder",
                |app, ctx, d| {
                    app.dialogs.act_on_folder(d.to_path_buf());
                    app.trash_folder(ctx);
                    // This waits only when the operation started. When it was held back, it
                    // says so, and there is nothing to wait for.
                    if app.notice.text().is_none() {
                        run_background_until(app, ctx, |app| app.file_operations.undo.is_some());
                    }
                    app.undo(ctx);
                },
                |app, d| {
                    let a = d.join("a.png");
                    a.exists()
                        && side(&a) == "a"
                        && side(&d.join("b.png")) == "b"
                        && (!cfg!(feature = "marks") || stars(app, &a) == 3)
                },
            ),
            (
                "rename folder",
                |app, ctx, d| {
                    // A folder left by a run that failed would refuse the new name.
                    let _ = std::fs::remove_dir_all(renamed(d));
                    app.dialogs.act_on_folder(d.to_path_buf());
                    let name = renamed(d)
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned();
                    app.rename_folder(ctx, &name);
                },
                |app, d| {
                    let (a, b) = (renamed(d).join("a.png"), renamed(d).join("b.png"));
                    a.exists()
                        && !d.exists()
                        && side(&a) == "a"
                        && side(&b) == "b"
                        && (!cfg!(feature = "marks")
                            || (stars(app, &a) == 3
                                && stars(app, &b) == 2
                                && stars(app, &d.join("a.png")) == 0))
                },
            ),
            (
                "save orientation",
                |app, ctx, d| {
                    // The fixture's image is square. A wide one shows the turn in its size. The
                    // window is told it changed, as a turn is saved into the picture decoded.
                    image::RgbImage::new(8, 4).save(d.join("a.png")).unwrap();
                    app.files_changed(ctx, &[d.join("a.png")]);
                    app.viewing.set_turn((1, false), d.join("a.png"));
                    app.save_orientation(ctx);
                },
                |app, d| {
                    let a = d.join("a.png");
                    image::image_dimensions(&a).ok() == Some((4, 8))
                        && side(&a) == "a"
                        && (!cfg!(feature = "marks") || stars(app, &a) == 3)
                },
            ),
        ]
    }

    /// Every file operation brings along what follows a file (its sidecar and its stars), and it
    /// waits while other work changes the file. These are the classes of bugs that came back
    /// release after release, held for every operation at once. The contract's folder is no
    /// collection, so the collection's entry is checked apart, for these operations only:
    /// rename, move, rename folder and delete in `a_collection_follows_its_files`, and Rename
    /// Many, trash, a folder moved to the trash and the Undo of each, and the Undo of a move, in
    /// `a_collection_follows_undo_and_the_trash`. A saved collection that is not open follows a
    /// rename in `saved_collections_follow_their_files`.
    /// A new operation joins `contract_ops`, and `every_file_operation_is_in_the_contract` fails
    /// until it does.
    #[test]
    fn file_operations_keep_their_contract() {
        for (name, op, check) in contract_ops() {
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
                app.marks.rate(
                    &crate::support::test_support::unlisted(&[d.join("a.png")]),
                    3,
                );
                app.marks.rate(
                    &crate::support::test_support::unlisted(&[d.join("b.png")]),
                    2,
                );
            }
            // While other work changes the same files, the operation is held back and nothing
            // moves.
            let (go, wait) = std::sync::mpsc::channel::<()>();
            let held = vec![d.join("a.png"), d.join("b.png")];
            assert!(app.run_in_background(&ctx, held, move || {
                let _ = wait.recv();
                None
            }));
            if name != "keywords" {
                app.notice.clear();
                op(&mut app, &ctx, &d);
                std::thread::sleep(std::time::Duration::from_millis(100));
                app.poll_background(&ctx);
                let before = ["a.png", "b.png"].iter().all(|f| d.join(f).exists())
                    && side(&d.join("a.png")) == "a"
                    && side(&d.join("b.png")) == "b";
                assert!(
                    before && app.notice.text().is_some(),
                    "{name}: not held back"
                );
            }
            go.send(()).unwrap();
            assert!(run_background_until(&mut app, &ctx, |app| app
                .file_operations
                .held_back_files
                .iter()
                .all(|t| std::sync::Arc::strong_count(t) == 1)));
            // Then it runs, and what follows the file comes along.
            app.notice.clear();
            op(&mut app, &ctx, &d);
            assert!(
                run_background_until(&mut app, &ctx, |app| check(app, &d)),
                "{name}: what follows the file"
            );
            // After "rename folder" the folder is there only under its new name.
            let _ = std::fs::remove_dir_all(renamed(&d));
            let _ = std::fs::remove_dir_all(&d);
            empty_trash_of(&d);
        }
    }

    /// Lightroom's `r.xmp` follows the RAW file `r.cr2` through every operation of the contract
    /// that moves, copies or deletes a file, and never the `r.jpg` beside it. Before, it stayed
    /// behind for the next `r.cr2` of that folder.
    #[test]
    fn adobe_sidecars_follow_their_raw_file() {
        use eframe::egui::Context;
        use std::path::Path;
        type RawOp = fn(&mut crate::App, &Context, &Path, &str);
        // Each operation on one file, and the name the file has after it, without its extension
        // (`None` when it is gone). Lightroom's sidecar follows r.cr2 there, and stays as r.xmp
        // when r.jpg moves.
        let ops: [(&str, RawOp, Option<&str>); 9] = [
            (
                "move",
                |app, ctx, d, f| move_to_sub(app, ctx, d, f),
                Some("sub/r"),
            ),
            (
                "copy",
                |app, _, d, f| {
                    app.transfer(
                        crate::files::file_io::PickedFiles::new(vec![d.join(f)]),
                        d.join("sub"),
                        true,
                    )
                },
                Some("sub/r"),
            ),
            (
                "rename",
                |app, ctx, d, f| rename_to_s(app, ctx, d, f),
                Some("s"),
            ),
            (
                "rename many",
                |app, ctx, d, f| {
                    let at = (app.list.files().iter()).position(|n| *n == *f);
                    app.list.select(at.into_iter().collect());
                    let picked = crate::files::file_io::PickedFiles::new(vec![d.join(f)]);
                    app.rename_many(ctx, "x#", picked);
                },
                Some("x1"),
            ),
            ("delete", |app, ctx, _, f| delete(app, ctx, f, false), None),
            ("trash", |app, ctx, _, f| delete(app, ctx, f, true), None),
            (
                "undo a move",
                |app, ctx, d, f| {
                    move_to_sub(app, ctx, d, f);
                    undo(app, ctx);
                },
                Some("r"),
            ),
            (
                "undo a rename",
                |app, ctx, d, f| {
                    rename_to_s(app, ctx, d, f);
                    undo(app, ctx);
                },
                Some("r"),
            ),
            (
                "undo a trash",
                |app, ctx, _, f| {
                    delete(app, ctx, f, true);
                    undo(app, ctx);
                },
                Some("r"),
            ),
        ];
        fn move_to_sub(app: &mut crate::App, _: &Context, d: &Path, f: &str) {
            app.transfer(
                crate::files::file_io::PickedFiles::new(vec![d.join(f)]),
                d.join("sub"),
                false,
            );
        }
        fn rename_to_s(app: &mut crate::App, ctx: &Context, d: &Path, f: &str) {
            let to = d.join(f.replacen('r', "s", 1));
            app.rename_in_background(
                ctx,
                crate::support::test_support::unlisted(vec![d.join(f)]),
                vec![to],
                crate::files::file_io::PickedFiles::new(vec![d.join(f)]),
            );
        }
        fn delete(app: &mut crate::App, ctx: &Context, f: &str, trash: bool) {
            let at = (app.list.files().iter()).position(|n| *n == *f);
            app.delete_targets(ctx, trash, at.into_iter().collect(), None);
        }
        fn undo(app: &mut crate::App, ctx: &Context) {
            run_background_until(app, ctx, |app| app.file_operations.undo.is_some());
            app.undo(ctx);
        }
        let mut jpeg = Vec::new();
        image::RgbImage::new(8, 8)
            .write_to(
                &mut std::io::Cursor::new(&mut jpeg),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        for (name, op, name_after) in ops {
            for file in ["r.jpg", "r.cr2"] {
                let d = std::env::temp_dir().join(format!(
                    "glimt-test-adobe-follows-{}-{}-{file}",
                    name.replace(' ', "-"),
                    std::process::id()
                ));
                let _ = std::fs::remove_dir_all(&d);
                std::fs::create_dir_all(d.join("sub")).unwrap();
                // The RAW file's preview is the JPEG, as in a camera's file.
                for f in ["r.jpg", "r.cr2"] {
                    std::fs::write(d.join(f), &jpeg).unwrap();
                }
                std::fs::write(d.join("r.xmp"), "Lightroom's").unwrap();
                let (mut app, ctx) = app_on(&d);
                op(&mut app, &ctx, &d, file);
                let is_where_it_belongs = |app: &crate::App| {
                    let done = app.file_operations.renaming.is_none()
                        && (app.file_operations.held_back_files.iter())
                            .all(|t| std::sync::Arc::strong_count(t) == 1);
                    // The names that hold a file of extension `ext`, in this order.
                    let names_with = |ext: &str| {
                        (["sub/r", "s", "x1", "r"].into_iter())
                            .filter(|n| d.join(format!("{n}.{ext}")).exists())
                            .collect::<Vec<_>>()
                    };
                    let copied = (name == "copy").then_some("r");
                    let file_names: Vec<&str> = name_after.into_iter().chain(copied).collect();
                    let ext = file.trim_start_matches("r.");
                    let sidecar_names = match ext {
                        "cr2" => file_names.clone(),
                        _ => vec!["r"],
                    };
                    done && names_with(ext) == file_names && names_with("xmp") == sidecar_names
                };
                assert!(
                    run_background_until(&mut app, &ctx, is_where_it_belongs),
                    "{name} of {file}: the file or r.xmp went wrong: {:?} {:?}",
                    app.notice.text(),
                    std::fs::read_dir(&d)
                        .unwrap()
                        .map(|e| e.unwrap().file_name())
                        .collect::<Vec<_>>()
                );
                std::fs::remove_dir_all(&d).unwrap();
                empty_trash_of(&d);
            }
        }
    }

    /// Lightroom's sidecar is decided for in one place. A rename of a RAW file that changes only
    /// the case of its extension keeps the sidecar. `r.XMP` is Lightroom's too. When two RAW files
    /// share the stem `r`, `r.xmp` stays where it is through an operation on either, and the
    /// Undo of a move, a trash or a rename is not refused because of it. Two RAW files trashed
    /// together come back with it, two moved together take it along in either order, and no
    /// error is told.
    #[test]
    fn adobe_sidecars_of_one_stem_never_block_a_rename_or_undo() {
        use eframe::egui::Context;
        use std::path::Path;
        // Each case: its name, the files in the folder, what is done, and the names in the
        // folder and in its sub after it.
        type Case = (
            &'static str,
            &'static [&'static str],
            fn(&mut crate::App, &Context, &Path),
            &'static [&'static str],
            &'static [&'static str],
        );
        let cases: [Case; 11] = [
            (
                "move both after another RAW file",
                &["a.cr2", "r.cr2", "r.nef", "r.xmp"],
                |app, _, d| {
                    let paths = ["a.cr2", "r.cr2", "r.nef"].map(|f| d.join(f)).to_vec();
                    app.transfer(
                        crate::files::file_io::PickedFiles::new(paths),
                        d.join("sub"),
                        false,
                    )
                },
                &[],
                &["a.cr2", "r.cr2", "r.nef", "r.xmp"],
            ),
            (
                "move both in the other order",
                &["r.cr2", "r.nef", "r.xmp"],
                |app, _, d| {
                    app.transfer(
                        crate::files::file_io::PickedFiles::new(vec![
                            d.join("r.nef"),
                            d.join("r.cr2"),
                        ]),
                        d.join("sub"),
                        false,
                    )
                },
                &[],
                &["r.cr2", "r.nef", "r.xmp"],
            ),
            (
                "rename the case",
                &["r.cr2", "r.xmp"],
                |app, ctx, d| {
                    app.rename_in_background(
                        ctx,
                        crate::support::test_support::unlisted(vec![d.join("r.cr2")]),
                        vec![d.join("r.CR2")],
                        crate::files::file_io::PickedFiles::new(vec![d.join("r.cr2")]),
                    )
                },
                &["r.CR2", "r.xmp"],
                &[],
            ),
            (
                "move with r.XMP",
                &["r.cr2", "r.XMP"],
                |app, _, d| {
                    app.transfer(
                        crate::files::file_io::PickedFiles::new(vec![d.join("r.cr2")]),
                        d.join("sub"),
                        false,
                    )
                },
                &[],
                &["r.XMP", "r.cr2"],
            ),
            (
                "move one of two",
                &["r.cr2", "r.nef", "r.xmp"],
                |app, _, d| {
                    app.transfer(
                        crate::files::file_io::PickedFiles::new(vec![d.join("r.cr2")]),
                        d.join("sub"),
                        false,
                    )
                },
                &["r.nef", "r.xmp"],
                &["r.cr2"],
            ),
            (
                "move one of two in mixed case",
                &["r.CR2", "r.Nef", "r.xmp"],
                |app, _, d| {
                    app.transfer(
                        crate::files::file_io::PickedFiles::new(vec![d.join("r.CR2")]),
                        d.join("sub"),
                        false,
                    )
                },
                &["r.Nef", "r.xmp"],
                &["r.CR2"],
            ),
            (
                "undo a move",
                &["r.cr2", "r.nef", "r.xmp"],
                |app, ctx, d| {
                    app.transfer(
                        crate::files::file_io::PickedFiles::new(vec![d.join("r.cr2")]),
                        d.join("sub"),
                        false,
                    );
                    undo(app, ctx);
                },
                &["r.cr2", "r.nef", "r.xmp"],
                &[],
            ),
            (
                "undo a rename",
                &["r.cr2", "r.nef", "r.xmp"],
                |app, ctx, d| {
                    app.rename_in_background(
                        ctx,
                        crate::support::test_support::unlisted(vec![d.join("r.cr2")]),
                        vec![d.join("s.cr2")],
                        crate::files::file_io::PickedFiles::new(vec![d.join("r.cr2")]),
                    );
                    undo(app, ctx);
                },
                &["r.cr2", "r.nef", "r.xmp"],
                &[],
            ),
            (
                "undo a rename of both",
                &["r.cr2", "r.nef", "r.xmp"],
                |app, ctx, d| {
                    let (from, to) = (["r.cr2", "r.nef"], ["s.cr2", "s.nef"]);
                    let (from, to) = (from.map(|f| d.join(f)), to.map(|t| d.join(t)));
                    app.rename_in_background(
                        ctx,
                        crate::support::test_support::unlisted(from.to_vec()),
                        to.to_vec(),
                        crate::files::file_io::PickedFiles::new(from.to_vec().to_vec()),
                    );
                    undo(app, ctx);
                },
                &["r.cr2", "r.nef", "r.xmp"],
                &[],
            ),
            (
                "undo a trash",
                &["r.cr2", "r.nef", "r.xmp"],
                |app, ctx, _| {
                    app.delete_targets(ctx, true, [0].into_iter().collect(), None);
                    undo(app, ctx);
                },
                &["r.cr2", "r.nef", "r.xmp"],
                &[],
            ),
            (
                "undo a trash of both",
                &["r.cr2", "r.nef", "r.xmp"],
                |app, ctx, _| {
                    app.delete_targets(ctx, true, [0, 1].into_iter().collect(), None);
                    undo(app, ctx);
                },
                &["r.cr2", "r.nef", "r.xmp"],
                &[],
            ),
        ];
        fn undo(app: &mut crate::App, ctx: &Context) {
            run_background_until(app, ctx, |app| app.file_operations.undo.is_some());
            app.undo(ctx);
        }
        // A RAW file's preview is a JPEG, as in a camera's file, so showing it tells no error.
        let mut jpeg = Vec::new();
        image::RgbImage::new(8, 8)
            .write_to(
                &mut std::io::Cursor::new(&mut jpeg),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        let names = |d: &Path| {
            let mut names: Vec<String> = (std::fs::read_dir(d).unwrap())
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .filter(|n| n != "sub")
                .collect();
            names.sort();
            names
        };
        let mut failed = Vec::new();
        for (name, files, steps, in_folder, in_sub) in cases {
            let d = std::env::temp_dir().join(format!(
                "glimt-test-adobe-stem-{}-{}",
                name.replace(' ', "-"),
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(d.join("sub")).unwrap();
            for f in files {
                std::fs::write(d.join(f), &jpeg).unwrap();
            }
            let (mut app, ctx) = app_on(&d);
            steps(&mut app, &ctx, &d);
            let is_done = |app: &crate::App| {
                !app.working() && names(&d) == in_folder && names(&d.join("sub")) == in_sub
            };
            let done = run_background_until(&mut app, &ctx, is_done);
            // Any error would name a file of the stem.
            let told = app.notice.text().unwrap_or_default().to_owned();
            if !done || told.contains("r.") {
                let (folder, sub) = (names(&d), names(&d.join("sub")));
                failed.push(format!("{name}: {told:?} {folder:?} {sub:?}"));
            }
            std::fs::remove_dir_all(&d).unwrap();
            empty_trash_of(&d);
        }
        assert!(failed.is_empty(), "{failed:#?}");
    }

    /// A RAW file that cannot go, because its name is taken in the folder it was moved to,
    /// keeps Lightroom's sidecar of its stem, although another RAW file of that stem went before
    /// or after it. Before, the sidecar went with the other one when that went first, as the one
    /// still to come was counted as leaving.
    #[test]
    fn an_adobe_sidecar_stays_with_a_raw_file_that_could_not_go() {
        let d = std::env::temp_dir().join(format!("glimt-test-adobe-stays-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("sub")).unwrap();
        for f in ["r.nef", "r.cr2", "r.xmp", "sub/r.nef"] {
            std::fs::write(d.join(f), b"").unwrap();
        }
        let (mut app, ctx) = app_on(&d);
        // In either order: when r.cr2 goes first, r.nef is still to come and may go too.
        for order in [["r.nef", "r.cr2"], ["r.cr2", "r.nef"]] {
            let paths = order.map(|f| d.join(f)).to_vec();
            app.transfer(
                crate::files::file_io::PickedFiles::new(paths),
                d.join("sub"),
                false,
            );
            let moved = |app: &crate::App| !app.working() && d.join("sub/r.cr2").exists();
            assert!(run_background_until(&mut app, &ctx, moved), "{order:?}");
            let stayed = d.join("r.xmp").exists() && !d.join("sub/r.xmp").exists();
            assert!(stayed, "{order:?}: r.xmp went with r.cr2");
            std::fs::rename(d.join("sub/r.cr2"), d.join("r.cr2")).unwrap();
            run_background_until(&mut app, &ctx, |app| !app.working());
        }
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// The Undo of a move of two RAW files of one stem, where one cannot go back because its
    /// name is taken, leaves Lightroom's sidecar with the one that stays, whatever their order.
    /// Before, the sidecar went back with the first when that one went first.
    #[test]
    fn an_adobe_sidecar_stays_with_a_raw_file_that_could_not_go_back() {
        let d = std::env::temp_dir().join(format!("glimt-test-adobe-back-{}", std::process::id()));
        for order in [["r.nef", "r.cr2"], ["r.cr2", "r.nef"]] {
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(d.join("sub")).unwrap();
            for f in ["r.nef", "r.cr2", "r.xmp"] {
                std::fs::write(d.join(f), b"").unwrap();
            }
            let (mut app, ctx) = app_on(&d);
            app.transfer(
                crate::files::file_io::PickedFiles::new(order.map(|f| d.join(f)).to_vec()),
                d.join("sub"),
                false,
            );
            let moved = |app: &crate::App| app.file_operations.undo.is_some() && !app.working();
            assert!(run_background_until(&mut app, &ctx, moved), "{order:?}");
            assert!(d.join("sub/r.xmp").exists(), "{order:?}: r.xmp did not go");
            std::fs::write(d.join("r.nef"), b"taken").unwrap();
            app.undo(&ctx);
            let back = |app: &crate::App| !app.working() && d.join("r.cr2").exists();
            assert!(run_background_until(&mut app, &ctx, back), "{order:?}");
            let stayed = d.join("sub/r.xmp").exists() && !d.join("r.xmp").exists();
            assert!(stayed, "{order:?}: r.xmp went back with r.cr2");
            std::fs::remove_dir_all(&d).unwrap();
        }
    }

    /// Glimt killed during a Delete leaves the delete's log, and the next start forgets the
    /// marks of the files that went, and only those. Before, the marks were forgotten only in
    /// the delete's last report, so a kill (a logout's SIGKILL, a third close) left the stars of
    /// every file already deleted at its name, where the next photo of that name showed them.
    /// The kill here is the log dropped without a report, with one file of four gone. A file at
    /// a logged name keeps its marks, also when it changed since, as it may be the file the kill
    /// came before, written anew, or one marked after the kill. So does a file whose folder is
    /// not there at the start, as on a stick not plugged in yet, and the log stays for a later
    /// start.
    #[test]
    fn the_marks_of_the_files_a_killed_delete_deleted_are_forgotten() {
        let dir = contract_folder("killed-delete");
        let logs = dir.join("logs");
        let (a, b, c) = (dir.join("a.png"), dir.join("b.png"), dir.join("c.png"));
        let stick = dir.join("stick");
        let d = stick.join("d.png");
        std::fs::write(&c, b"c").unwrap();
        let (mut app, ctx) = app_on(&dir);
        let all = [a.clone(), b.clone(), c.clone(), d.clone()];
        #[cfg(feature = "marks")]
        app.marks
            .rate(&crate::support::test_support::unlisted(&all), 3);
        let marked = if cfg!(feature = "marks") { 3 } else { 0 };
        let job = super::Job::new("Deleting", 4, false, Vec::new());
        let mut log = super::journal::start_delete_log(&logs, None).unwrap();
        let picked = crate::files::file_io::PickedFiles::new(vec![a.clone()]);
        let (_, picked) = picked.wait_for_fingerprints();
        let mut errors = Vec::new();
        let how = (false, Some(&mut log));
        let (gone, _, _) =
            super::delete_in_chunks(std::slice::from_ref(&a), &picked, how, &job, &mut errors);
        assert_eq!(gone, std::slice::from_ref(&a), "{errors:?}");
        // b.png, c.png and the stick's d.png were written down, and the kill came before they
        // went.
        log.note(&all[1..]);
        drop(log);
        // Another program wrote c.png anew before the next start, and the stick is not there.
        std::fs::write(&c, b"written anew").unwrap();
        let start = |app: &mut crate::App, expected: &[PathBuf]| {
            let found = super::journal::deletes_cut_short(&logs);
            assert_eq!(found.len(), 1);
            for (log, gone) in found {
                assert_eq!(gone, expected);
                // Another window that starts meanwhile leaves the log alone.
                assert!(super::journal::deletes_cut_short(&logs).is_empty());
                let report = super::OperationReport {
                    gone,
                    delete_log: Some(log),
                    ..Default::default()
                };
                let _ = app.file_operations.report_sender().send(report);
            }
            assert!(run_background_until(app, &ctx, |app| !app.working()));
            crate::files::file_io::wait_for_queued_writes();
        };
        start(&mut app, std::slice::from_ref(&a));
        assert_eq!(stars(&app, &a), 0);
        for kept in &all[1..] {
            assert_eq!(stars(&app, kept), marked, "{kept:?}");
        }
        // At a later start the stick is back, without d.png, which went before the kill.
        std::fs::create_dir(&stick).unwrap();
        start(&mut app, &[a.clone(), d.clone()]);
        let no_log_left = |_: &crate::App| {
            let left = std::fs::read_dir(&logs).unwrap().flatten();
            left.filter(|e| e.path().is_file()).count() == 0
                && std::fs::read_dir(logs.join("files")).unwrap().count() == 0
        };
        assert!(run_background_until(&mut app, &ctx, no_log_left));
        assert_eq!(stars(&app, &d), 0);
        assert_eq!(stars(&app, &b), marked);
        assert_eq!(stars(&app, &c), marked);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Glimt killed during a delete in a collection leaves the collection's entries of the files
    /// that went, and the next start takes them out, as the delete would have at its end. Before,
    /// they stayed, and a later photo of that name showed in the collection. The log's file of
    /// the collection goes with the log.
    #[test]
    fn a_killed_delete_takes_the_files_that_went_out_of_the_collection() {
        let dir = contract_folder("killed-delete-collection");
        let (logs, gqv) = (dir.join("logs"), dir.join("col.gqv"));
        let (a, b) = (dir.join("a.png"), dir.join("b.png"));
        let both = [a.clone(), b.clone()];
        crate::model::collections::edit(&gqv, true, |all| *all = both.to_vec()).unwrap();
        let mut log = super::journal::start_delete_log(&logs, Some(&gqv)).unwrap();
        log.note(&both);
        // The kill came after a.png went, before b.png.
        std::fs::remove_file(&a).unwrap();
        drop(log);
        let found = super::journal::deletes_cut_short(&logs);
        assert_eq!(found.len(), 1);
        assert_eq!(crate::model::collections::load(&gqv).unwrap(), [b]);
        for (log, _) in found {
            log.remove_once_forgotten();
        }
        // The log goes after the writes of both writer threads, one queued after the other.
        crate::files::file_io::wait_for_queued_writes();
        crate::files::file_io::wait_for_queued_writes();
        let left = std::fs::read_dir(logs.join("collections")).unwrap().count();
        assert_eq!(left, 0, "the log's file of the collection is left");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Glimt killed during a Move to Trash between a photo's sidecar and the photo, which go to
    /// the trash in that order, leaves the photo at its name and its sidecar in the trash. The
    /// next start takes the sidecar back. Before, the photo's keywords, comment and alt text
    /// stayed in the trash.
    #[test]
    fn a_killed_move_to_trash_takes_back_the_sidecar_of_a_photo_that_stayed() {
        let dir = contract_folder("killed-trash-sidecar");
        let logs = dir.join("logs");
        let a = dir.join("a.png");
        let side = crate::metadata::xmp::sidecar(&a);
        let mut log = super::journal::start_delete_log(&logs, None).unwrap();
        log.note(std::slice::from_ref(&a));
        // The sidecar went to the trash, and the kill came before the photo.
        crate::files::trash::trash(std::slice::from_ref(&side)).unwrap();
        assert!(!side.exists());
        drop(log);
        let found = super::journal::deletes_cut_short(&logs);
        assert_eq!(found.len(), 1);
        assert!(found[0].1.is_empty(), "a.png did not go");
        assert_eq!(std::fs::read_to_string(&side).unwrap(), "a");
        drop(found);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Glimt killed during a Delete for good after it put a photo's sidecar aside under a hidden
    /// name, and before it deleted the photo or the sidecar, left the sidecar under that name for
    /// good: beside a photo still there without its keywords, comment and alt text, or beside
    /// nothing. The next start puts it back beside a photo that stayed and deletes it when the
    /// photo went. The kill here is a panic just before each photo is deleted, and b.png is then
    /// removed as if the kill came just after it went.
    #[test]
    fn a_killed_delete_for_good_puts_right_the_sidecars_it_put_aside() {
        use crate::support::threads::{BREAKING_FILES, catch_panic};
        let dir = contract_folder("killed-delete-aside");
        let logs = dir.join("logs");
        let (a, b) = (dir.join("a.png"), dir.join("b.png"));
        let sides = [&a, &b].map(|p| crate::metadata::xmp::sidecar(p));
        let both = [a.clone(), b.clone()];
        let picked = crate::files::file_io::PickedFiles::new(both.to_vec());
        let (_, picked) = picked.wait_for_fingerprints();
        let mut log = super::journal::start_delete_log(&logs, None).unwrap();
        log.note(&both);
        lock_even_if_poisoned(&BREAKING_FILES).extend(sides.clone());
        let mut errors = Vec::new();
        for p in &both {
            let photo = std::slice::from_ref(p);
            let how = (false, Some(&mut log));
            let cut = catch_panic(|| super::delete_chunk(photo, &picked, how, &mut errors));
            assert!(cut.is_none(), "the kill came");
        }
        lock_even_if_poisoned(&BREAKING_FILES).retain(|p| !sides.contains(p));
        std::fs::remove_file(&b).unwrap();
        assert!(!sides[0].exists() && !sides[1].exists(), "not put aside");
        drop(log);
        let found = super::journal::deletes_cut_short(&logs);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].1, std::slice::from_ref(&b));
        assert_eq!(std::fs::read_to_string(&sides[0]).unwrap(), "a");
        assert!(!sides[1].exists(), "b.png's sidecar came back without it");
        let hidden: Vec<PathBuf> = (std::fs::read_dir(&dir).unwrap().flatten())
            .filter(|e| e.file_name().to_string_lossy().starts_with(".glimt-tmp-"))
            .map(|e| e.path())
            .collect();
        assert!(hidden.is_empty(), "left hidden: {hidden:?}");
        for (log, _) in found {
            log.remove_once_forgotten();
        }
        // The log goes after the writes of both writer threads, one queued after the other.
        crate::files::file_io::wait_for_queued_writes();
        crate::files::file_io::wait_for_queued_writes();
        let left = std::fs::read_dir(logs.join("aside")).unwrap().count();
        assert_eq!(left, 0, "the log's file of the sidecars put aside is left");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A photo deleted for good with Glimt killed before its hidden sidecar went, and another
    /// photo copied to its name before the next start, as from a card whose numbering began
    /// again. The next start put the sidecar of the photo deleted back beside the new one,
    /// with its keywords, comment and alt text, as it took any file at the name for the photo.
    /// It now asks what the trash take-back asks: the photo must be there as the log's list
    /// knew it.
    #[test]
    fn a_killed_delete_for_good_gives_no_hidden_sidecar_to_a_new_photo_at_the_name() {
        use crate::support::threads::{BREAKING_FILES, catch_panic};
        let dir = contract_folder("killed-delete-new-photo");
        let logs = dir.join("logs");
        let (a, b) = (dir.join("a.png"), dir.join("b.png"));
        let sides = [&a, &b].map(|p| crate::metadata::xmp::sidecar(p));
        let both = [a.clone(), b.clone()];
        let picked = crate::files::file_io::PickedFiles::new(both.to_vec());
        let (_, picked) = picked.wait_for_fingerprints();
        let mut log = super::journal::start_delete_log(&logs, None).unwrap();
        log.note(&both);
        lock_even_if_poisoned(&BREAKING_FILES).extend(sides.clone());
        let mut errors = Vec::new();
        for p in &both {
            let how = (false, Some(&mut log));
            let photo = std::slice::from_ref(p);
            let cut = catch_panic(|| super::delete_chunk(photo, &picked, how, &mut errors));
            assert!(cut.is_none(), "the kill came");
        }
        lock_even_if_poisoned(&BREAKING_FILES).retain(|p| !sides.contains(p));
        // b.png went, and another photo was copied to its name.
        std::fs::remove_file(&b).unwrap();
        std::fs::write(&b, b"another photo, of another size").unwrap();
        drop(log);
        let found = super::journal::deletes_cut_short(&logs);
        assert_eq!(found.len(), 1);
        assert_eq!(std::fs::read_to_string(&sides[0]).unwrap(), "a");
        assert!(
            !sides[1].exists(),
            "the new b.png got the sidecar of the one deleted"
        );
        drop(found);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A Delete for good makes the rename that puts a sidecar aside lasting before the photo
    /// goes. Its note in the log's file of the sidecars put aside, and the log's names and list
    /// of files that the next start reads with it, were not synced, so a power cut there could
    /// leave the sidecar hidden for good with no note of it on disk. They are now on disk, with
    /// their folders, by the time the sidecar is put aside: the test's kill comes after that.
    /// A photo without a sidecar costs no sync of the log.
    #[test]
    fn a_delete_for_good_has_its_note_of_a_sidecar_on_disk_before_it_puts_it_aside() {
        use super::journal::{LOG_FILES_SYNCED, aside_path, known_path};
        use crate::files::file_io::FOLDERS_SYNCED;
        use crate::support::threads::{BREAKING_FILES, catch_panic};
        let dir = contract_folder("aside-note-on-disk");
        let logs = dir.join("logs");
        let (a, b) = (dir.join("a.png"), dir.join("b.png"));
        std::fs::remove_file(crate::metadata::xmp::sidecar(&b)).unwrap();
        let side = crate::metadata::xmp::sidecar(&a);
        let both = [a.clone(), b.clone()];
        let picked = crate::files::file_io::PickedFiles::new(both.to_vec());
        let (_, picked) = picked.wait_for_fingerprints();
        let mut log = super::journal::start_delete_log(&logs, None).unwrap();
        let mut errors = Vec::new();
        LOG_FILES_SYNCED.take();
        log.note(std::slice::from_ref(&b));
        let how = (false, Some(&mut log));
        let went = super::delete_chunk(std::slice::from_ref(&b), &picked, how, &mut errors);
        assert_eq!(went.0, std::slice::from_ref(&b), "{errors:?}");
        assert!(
            LOG_FILES_SYNCED.take().is_empty(),
            "synced without a sidecar"
        );
        log.note(std::slice::from_ref(&a));
        FOLDERS_SYNCED.take();
        lock_even_if_poisoned(&BREAKING_FILES).push(side.clone());
        let how = (false, Some(&mut log));
        let cut = catch_panic(|| {
            super::delete_chunk(std::slice::from_ref(&a), &picked, how, &mut errors)
        });
        lock_even_if_poisoned(&BREAKING_FILES).retain(|p| *p != side);
        assert!(cut.is_none(), "the kill came");
        assert!(!side.exists(), "not put aside");
        let log_path = (std::fs::read_dir(&logs).unwrap().flatten())
            .map(|e| e.path())
            .find(|p| p.is_file())
            .unwrap();
        let synced = LOG_FILES_SYNCED.take();
        for file in [
            known_path(&log_path),
            log_path.clone(),
            aside_path(&log_path),
        ] {
            assert!(
                synced.contains(&file),
                "{} not synced: {synced:?}",
                file.display()
            );
        }
        let folders = FOLDERS_SYNCED.take();
        for folder in [logs.clone(), logs.join("files"), logs.join("aside")] {
            assert!(folders.contains(&folder), "{} not synced", folder.display());
        }
        drop(log);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Glimt killed during a Move leaves the Move's log, and the next start makes the marks
    /// follow the files that went, and only those. Before, the marks followed only the Move's
    /// last report, so a kill (a logout's SIGKILL, Ctrl+C) left the stars of every file already
    /// moved under its old name. The kill here is the log dropped without a report, with one
    /// file of two gone.
    #[test]
    fn the_marks_follow_the_files_a_killed_move_moved() {
        let dir = contract_folder("killed-move");
        let (to, logs) = (dir.join("sub"), dir.join("logs"));
        let (a, b) = (dir.join("a.png"), dir.join("b.png"));
        let (mut app, ctx) = app_on(&dir);
        #[cfg(feature = "marks")]
        app.marks.rate(
            &crate::support::test_support::unlisted(&[a.clone(), b.clone()]),
            3,
        );
        let marked = if cfg!(feature = "marks") { 3 } else { 0 };
        let job = super::Job::new("Moving", 2, false, Vec::new());
        let mut log = super::journal::start_move_log(&logs).unwrap();
        let picked = crate::files::file_io::PickedFiles::new(vec![a.clone()]);
        let (_, picked) = picked.wait_for_fingerprints();
        let moved = super::put_files(
            std::slice::from_ref(&a),
            (&picked, &to),
            (false, Some(&mut log)),
            &job,
        );
        assert_eq!(moved, (Vec::new(), vec![(a.clone(), to.join("a.png"))]));
        // b.png was written down, and the kill came before it went.
        log.note(&b, &to.join("b.png"));
        drop(log);
        // A copy of the log and its list of files, to find again as a log that outlived its Move.
        let kept = dir.join("kept");
        std::fs::create_dir_all(kept.join("files")).unwrap();
        let logs_files = || {
            let names = std::fs::read_dir(&logs).unwrap().flatten();
            let names = names.filter(|e| e.path().is_file()).map(|e| e.file_name());
            names.collect::<Vec<_>>()
        };
        for name in logs_files() {
            for folder in ["", "files"] {
                let (at, copy) = (logs.join(folder).join(&name), kept.join(folder).join(&name));
                std::fs::copy(at, copy).unwrap();
            }
        }
        let send_found = |app: &crate::App, expected: &[(PathBuf, PathBuf)]| {
            let found = super::journal::moves_cut_short(&logs);
            let count = found.len();
            for (log, went) in found {
                assert_eq!(went, expected);
                // Another window that starts meanwhile leaves the log alone.
                assert!(super::journal::moves_cut_short(&logs).is_empty());
                let report = super::OperationReport {
                    moved: went,
                    move_log: Some(log),
                    ..Default::default()
                };
                let _ = app.file_operations.report_sender().send(report);
            }
            count
        };
        let no_log_left = |_: &crate::App| {
            logs_files().is_empty() && std::fs::read_dir(logs.join("files")).unwrap().count() == 0
        };
        let went = [(a.clone(), to.join("a.png"))];
        assert_eq!(send_found(&app, &went), 1);
        assert!(run_background_until(&mut app, &ctx, no_log_left));
        assert_eq!(stars(&app, &to.join("a.png")), marked);
        assert_eq!(stars(&app, &a), 0);
        assert_eq!(stars(&app, &b), marked);
        let found_again = || {
            for entry in std::fs::read_dir(&kept).unwrap().flatten() {
                if entry.path().is_file() {
                    let name = entry.file_name();
                    std::fs::copy(entry.path(), logs.join(&name)).unwrap();
                    let list = kept.join("files").join(&name);
                    std::fs::copy(list, logs.join("files").join(&name)).unwrap();
                }
            }
        };
        // Found again, as after a kill just before the log went, the stars stay on a.png.
        found_again();
        assert_eq!(send_found(&app, &went), 1);
        assert!(run_background_until(&mut app, &ctx, no_log_left));
        assert_eq!(stars(&app, &to.join("a.png")), marked);
        // A later photo took a.png's new name, and another one rated at its old name went
        // outside Glimt. The log found again leaves their stars alone. Before, the stars of the
        // old name went to the later photo.
        std::fs::remove_file(to.join("a.png")).unwrap();
        std::fs::write(to.join("a.png"), b"a later photo").unwrap();
        #[cfg(feature = "marks")]
        app.marks.rate(
            &crate::support::test_support::unlisted(&[to.join("a.png")]),
            0,
        );
        #[cfg(feature = "marks")]
        app.marks.rate(
            &crate::support::test_support::unlisted(std::slice::from_ref(&a)),
            5,
        );
        found_again();
        assert_eq!(send_found(&app, &[]), 1);
        assert!(run_background_until(&mut app, &ctx, no_log_left));
        assert_eq!(stars(&app, &to.join("a.png")), 0);
        assert_eq!(stars(&app, &a), if cfg!(feature = "marks") { 5 } else { 0 });
        let told = crate::files::file_io::take_saves_failed();
        assert!(told.iter().any(|t| t.contains("a.png")), "{told:?}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The collection open at the start follows a file that a killed Move moved, also when
    /// the file has no marks. Before, such a file followed only in the saved collections that
    /// were not open, and stayed under its old name in the open one.
    #[test]
    fn the_open_collection_follows_the_files_a_killed_move_moved() {
        let dir = contract_folder("killed-move-collection");
        let (gqv, logs) = (dir.join("col.gqv"), dir.join("logs"));
        let (a, moved) = (dir.join("a.png"), dir.join("sub/a.png"));
        let (mut app, ctx) = app_on(&gqv);
        let mut log = super::journal::start_move_log(&logs).unwrap();
        log.note(&a, &moved);
        std::fs::rename(&a, &moved).unwrap();
        drop(log);
        for (log, went) in super::journal::moves_cut_short(&logs) {
            assert_eq!(went, vec![(a.clone(), moved.clone())]);
            let report = super::OperationReport {
                moved: went,
                move_log: Some(log),
                ..Default::default()
            };
            let _ = app.file_operations.report_sender().send(report);
        }
        let followed = |_: &crate::App| {
            std::fs::read_to_string(&gqv).unwrap().contains("sub/a.png")
                && std::fs::read_dir(&logs)
                    .unwrap()
                    .all(|e| e.unwrap().path().is_dir())
        };
        assert!(run_background_until(&mut app, &ctx, followed));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A Move whose log cannot be written, on a full home disk say, still moves its files, and
    /// the log keeps the failure to be told once. Before, every file stayed, because its line
    /// could not be written, and a Move is how a full disk is cleared.
    #[test]
    fn a_move_goes_on_when_its_log_cannot_be_written() {
        let dir = contract_folder("move-log-full");
        let to = dir.join("sub");
        let (a, b) = (dir.join("a.png"), dir.join("b.png"));
        let job = super::Job::new("Moving", 2, false, Vec::new());
        let mut log = super::journal::MoveLog::refusing_writes(&dir.join("log"));
        let paths = [a.clone(), b.clone()];
        let picked = crate::files::file_io::PickedFiles::new(paths.to_vec());
        let (_, picked) = picked.wait_for_fingerprints();
        let moved = super::put_files(&paths, (&picked, &to), (false, Some(&mut log)), &job);
        let went = vec![(a, to.join("a.png")), (b, to.join("b.png"))];
        assert_eq!(moved, (Vec::new(), went));
        assert!(log.take_failure().is_some());
        assert!(log.take_failure().is_none(), "told once");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A Move holds its files back from the request, while its folder's real path is looked
    /// up, so a delete of them in that moment waits. Before, they were held only once the job
    /// started, and a delete and a rename in between could hand the Move another file.
    #[test]
    fn a_move_holds_its_files_back_while_its_folder_is_looked_up() {
        let d = std::env::temp_dir().join(format!("glimt-test-move-held-{}", std::process::id()));
        std::fs::create_dir_all(d.join("sub")).unwrap();
        std::fs::write(d.join("a.png"), b"").unwrap();
        let (mut app, ctx) = app_on(&d);
        app.transfer(
            crate::files::file_io::PickedFiles::new(vec![d.join("a.png")]),
            d.join("sub"),
            false,
        );
        assert!(!app.are_free_to_change(&[d.join("a.png")]));
        let moved = |app: &crate::App| !app.working() && d.join("sub/a.png").exists();
        assert!(run_background_until(&mut app, &ctx, moved));
        assert!(app.are_free_to_change(&[d.join("sub/a.png")]));
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// The name that the "rename folder" operation gives the contract's folder `d`.
    fn renamed(d: &std::path::Path) -> std::path::PathBuf {
        let name = d.file_name().unwrap().to_string_lossy();
        d.with_file_name(format!("{name}-renamed"))
    }

    /// Removes what a test moved from the folder `dir` into the tests' own trash.
    fn empty_trash_of(dir: &std::path::Path) {
        let trash = crate::files::config::test_home().join(".local/share/Trash");
        let Ok(infos) = std::fs::read_dir(trash.join("info")) else {
            return;
        };
        // The folder itself is there too, when it went to the trash whole.
        let (inside, itself) = (
            format!("{}/", dir.display()),
            format!("Path={}", dir.display()),
        );
        let is_of_dir = |t: String| t.contains(&inside) || t.lines().any(|l| l == itself);
        for info in infos.flatten().map(|i| i.path()) {
            if std::fs::read_to_string(&info).is_ok_and(is_of_dir)
                && let Some(name) = info.file_stem()
            {
                let file = trash.join("files").join(name);
                let _ = std::fs::remove_dir_all(&file).or_else(|_| std::fs::remove_file(&file));
                let _ = std::fs::remove_file(&info);
            }
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
        let how = app.shown_copy();
        app.export_frames(&ctx, gif, None, (d.clone(), None), how);
        let saved = |_: &crate::App| {
            ["g-001.png", "g-002.png"]
                .iter()
                .all(|f| d.join("g-frames-2").join(f).exists())
        };
        assert!(run_background_until(&mut app, &ctx, saved));
        assert!(!d.join("nowhere").exists());
        std::fs::remove_dir_all(d).unwrap();
    }

    /// Save This Frame turns the frame as the view shows it, as Save As turns its copy.
    /// Before, a frame saved from a view turned a quarter came out lying down.
    #[test]
    fn a_frame_is_saved_turned_as_shown() {
        let d = std::env::temp_dir().join(format!("glimt-turned-frame-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let gif = d.join("g.gif");
        let mut enc = image::codecs::gif::GifEncoder::new(std::fs::File::create(&gif).unwrap());
        for c in [10u8, 200] {
            let pixel = image::Rgba([c, c, c, 255]);
            let frame = image::Frame::new(image::RgbaImage::from_pixel(4, 2, pixel));
            enc.encode_frame(frame).unwrap();
        }
        drop(enc);
        // The GIF is the only file, so it is the one shown.
        let (mut app, ctx) = app_on(&d);
        app.turn(crate::ui::actions::Action::RotateCw);
        let how = app.shown_copy();
        let out = d.join("frame.png");
        app.export_frames(&ctx, gif, Some(1), (out.clone(), None), how);
        assert!(run_background_until(&mut app, &ctx, |_| out.exists()));
        assert_eq!(image::image_dimensions(&out).unwrap(), (2, 4));
        std::fs::remove_dir_all(d).unwrap();
    }

    /// Export Frames of a TIFF whose first page is 32-bit float RGB writes that page in 16 bits,
    /// as Save As does, because PNG holds no floats. Before, the export stopped at that page.
    #[test]
    fn a_float_page_is_exported() {
        use tiff::encoder::colortype::{Gray8, RGB32Float};
        let d = contract_folder("float-frame");
        let tif = d.join("f.tif");
        let mut tiff = tiff::encoder::TiffEncoder::new(std::fs::File::create(&tif).unwrap());
        let tiff = tiff.as_mut().unwrap();
        tiff.write_image::<RGB32Float>(2, 2, &[0.5; 12]).unwrap();
        tiff.write_image::<Gray8>(2, 2, &[9; 4]).unwrap();
        let (mut app, ctx) = app_on(&d);
        let how = app.shown_copy();
        app.export_frames(&ctx, tif, None, (d.clone(), None), how);
        let saved = |_: &crate::App| {
            ["f-001.png", "f-002.png"]
                .iter()
                .all(|f| d.join("f-frames").join(f).exists())
        };
        assert!(run_background_until(&mut app, &ctx, saved));
        std::fs::remove_dir_all(d).unwrap();
    }

    /// Delete for good never leaves a sidecar at the name of a photo that went. One that cannot
    /// be deleted (here a folder) waits under a hidden name, told, and a photo whose sidecar
    /// cannot even be put aside stays, with it. Before, the photo went first, and its sidecar
    /// stayed at the old name for the next file of that name. Of two RAW files of one name
    /// stem, Adobe's sidecar stays beside the one that stays.
    #[test]
    fn a_delete_for_good_leaves_no_sidecar_at_the_old_name() {
        use crate::files::file_io::{RENAME_WAYS_MISSING, UNLINK_FAILS};
        let dir = std::env::temp_dir().join(format!("glimt-test-good-side-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let p = |n: &str| dir.join(n);
        // A folder in a sidecar's place cannot be removed as a file.
        std::fs::create_dir_all(p("a.png.xmp").join("inside")).unwrap();
        for n in [
            "a.png",
            "b.png",
            "b.png.xmp",
            "c.cr2",
            "c.nef",
            "c.nef.xmp",
            "c.xmp",
        ] {
            std::fs::write(p(n), n).unwrap();
        }
        let paths = [p("a.png"), p("b.png"), p("c.cr2"), p("c.nef")];
        let picked = crate::files::file_io::PickedFiles::new(paths.to_vec());
        let (_, picked) = picked.wait_for_fingerprints();
        let mut errors = Vec::new();
        // c.nef's sidecar cannot be renamed: its old name cannot be unlinked.
        RENAME_WAYS_MISSING.set(1);
        UNLINK_FAILS.set(Some((p("c.nef.xmp"), libc::EPERM)));
        let (went, _, _) = super::delete_chunk(&paths, &picked, (false, None), &mut errors);
        UNLINK_FAILS.set(None);
        RENAME_WAYS_MISSING.set(0);
        assert_eq!(went, [p("a.png"), p("b.png"), p("c.cr2")], "{errors:?}");
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert!(errors[0].contains("a.png.xmp"), "{errors:?}");
        for n in ["c.nef", "c.nef.xmp", "c.xmp"] {
            assert!(p(n).exists(), "{n}");
        }
        for n in ["a.png", "a.png.xmp", "b.png", "b.png.xmp", "c.cr2"] {
            assert!(std::fs::symlink_metadata(p(n)).is_err(), "{n}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A Move, the Undo of a Move and a Delete for good whose step a share did and then lost the
    /// answer of count the file as moved or deleted, so its sidecar goes with it. Before, the
    /// file counted as where it was: its sidecar and marks stayed at the old name, where the
    /// next file of that name took them.
    #[test]
    fn a_step_done_whose_answer_was_lost_counts_as_done() {
        use crate::files::file_io::ANSWER_LOST_AT;
        let dir =
            std::env::temp_dir().join(format!("glimt-test-lost-answer-{}", std::process::id()));
        let (from, to) = (dir.join("from"), dir.join("to"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&from).unwrap();
        std::fs::create_dir_all(&to).unwrap();
        std::fs::write(from.join("a.jpg"), "a").unwrap();
        std::fs::write(from.join("a.jpg.xmp"), "own").unwrap();
        let paths = [from.join("a.jpg")];
        let picked = crate::files::file_io::PickedFiles::new(paths.to_vec());
        let (_, picked) = picked.wait_for_fingerprints();
        let job = super::Job::new("Moving", 1, false, Vec::new());
        ANSWER_LOST_AT.set(Some(from.join("a.jpg")));
        let (errors, put) = super::put_files(&paths, (&picked, &to), (false, None), &job);
        ANSWER_LOST_AT.set(None);
        assert_eq!(put, [(from.join("a.jpg"), to.join("a.jpg"))], "{errors:?}");
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(
            std::fs::read_to_string(to.join("a.jpg.xmp")).unwrap(),
            "own"
        );
        // The Undo of the Move.
        let now = crate::files::file_io::fingerprint(&to.join("a.jpg")).unwrap();
        ANSWER_LOST_AT.set(Some(to.join("a.jpg")));
        let (back, left, errors) =
            super::move_back(vec![(from.join("a.jpg"), to.join("a.jpg"), now)]);
        ANSWER_LOST_AT.set(None);
        assert!(
            back.len() == 1 && left.is_empty() && errors.is_empty(),
            "{errors:?}"
        );
        assert_eq!(
            std::fs::read_to_string(from.join("a.jpg.xmp")).unwrap(),
            "own"
        );
        // A Delete for good.
        let picked = crate::files::file_io::PickedFiles::new(paths.to_vec());
        let (_, picked) = picked.wait_for_fingerprints();
        let mut errors = Vec::new();
        ANSWER_LOST_AT.set(Some(from.join("a.jpg")));
        let (went, _, _) = super::delete_chunk(&paths, &picked, (false, None), &mut errors);
        ANSWER_LOST_AT.set(None);
        assert_eq!(went, paths, "{errors:?}");
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(std::fs::read_dir(&from).unwrap().count(), 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A Stop, also that of a second close, during Export Frames or Rename Many is told as a
    /// stop. Before, each was told as a failed write, which kept the window open after the user
    /// had asked it to close and stop, and cost one more Ctrl+Q.
    #[test]
    fn a_stop_is_no_failed_write() {
        let d = contract_folder("stop-told");
        let gif = d.join("g.gif");
        let mut enc = image::codecs::gif::GifEncoder::new(std::fs::File::create(&gif).unwrap());
        for c in 0..40u8 {
            let pixel = image::Rgba([c, c, c, 255]);
            let frame = image::Frame::new(image::RgbaImage::from_pixel(64, 64, pixel));
            enc.encode_frame(frame).unwrap();
        }
        drop(enc);
        let (mut app, ctx) = app_on(&d);
        let how = app.shown_copy();
        app.export_frames(&ctx, gif, None, (d.clone(), None), how);
        app.file_operations.stop_jobs();
        assert!(run_background_until(&mut app, &ctx, |app| !app.working()));
        assert_eq!(app.window.failed_writes_told(), 0);
        assert!(app.notice.text().unwrap_or_default().contains("Stopped"));
        // Rename Many, stopped before its first step, puts every name back.
        let a = app.list.files().iter().position(|f| f == "a.png").unwrap();
        let listed = app.listed_file(a);
        let picked = crate::files::file_io::PickedFiles::listed(vec![listed.clone()]);
        app.rename_in_background(&ctx, vec![listed], vec![d.join("c.png")], picked);
        app.file_operations.stop_jobs();
        assert!(run_background_until(&mut app, &ctx, |app| !app.working()));
        assert_eq!(app.window.failed_writes_told(), 0);
        assert!(app.notice.text().unwrap_or_default().contains("stopped"));
        assert!(d.join("a.png").exists() && !d.join("c.png").exists());
        std::fs::remove_dir_all(d).unwrap();
    }

    /// Every operation that starts a job or background work is in the contract above, or says
    /// here why it is not. So a new one cannot leave out what follows its files unseen.
    #[test]
    fn every_file_operation_is_in_the_contract() {
        // Each function with the operation of `contract_ops` that runs it.
        const IN_CONTRACT: &[(&str, &str)] = &[
            ("delete_now", "delete"),
            ("delete_now", "trash"),
            ("undo_trash", "undo a trash"),
            ("trash_folder", "trash folder"),
            ("undo_trash", "undo a trash folder"),
            ("transfer_into_real_folder", "move"),
            // It holds the files of a Move, a Copy, a Copy for Sharing and a rename to a name
            // with : ? * and the others until their job starts.
            ("wait_for_look_up_before_the_job", "move"),
            // Through rename_in_background and rename_files_of, and the Undo of a rename.
            ("start_renaming", "rename"),
            ("undo", "undo a move"),
            ("save_as_to", "save as over b"),
            ("export_frames", "save frame over b"),
            ("write_sidecars", "keywords"),
            ("save_orientation", "save orientation"),
            ("rename_folder", "rename folder"),
            ("copy_for_sharing_into_real_folder", "copy for sharing"),
        ];
        const OUTSIDE: &[(&str, &str)] = &[
            ("start_job", "the start itself"),
            ("run_in_background", "the start itself"),
            ("new", "Job::new, the start itself"),
            (
                "print",
                "a new PDF of its own, no file of the user's changed: writes_a_pdf",
            ),
            (
                "tools_menu",
                "a program of the user's that holds its files; a_hold_holds_its_files_back",
            ),
            (
                "save_crop",
                "a new file beside the image: save::save_copy_with_sidecar's tests; it needs a decoded image",
            ),
            (
                "act_on_prompt_answer",
                "New Folder: an empty folder, nothing to follow; makes_a_new_folder_in_the_background",
            ),
            (
                "trash_collection",
                "trash::trash of the collection's own file",
            ),
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
                "work_on_cut_short",
                "journal's tests put every cut-short plan right and leave its names; \
                 a_refused_repair_is_asked_about_again_with_a_way_out",
            ),
            (
                "queue_write",
                "the writer threads of write_later and write_later_in_user_folders, which count \
                 each work they are sent",
            ),
        ];
        let mut found = std::collections::BTreeSet::new();
        for (file, text) in crate::support::source_rules::sources() {
            let code = text.split("\n#[cfg(test)]\nmod tests").next().unwrap();
            // A bare `WRITING` is file_io's, unless the file has a counter of its own by
            // that name, as marks.rs has for its own file.
            let own_counter = file != "files/file_io.rs" && code.contains("static WRITING:");
            let mut function = String::new();
            for line in crate::support::source_rules::code_lines(code) {
                if let Some(at) = line.find("fn ") {
                    let rest = &line[at + 3..];
                    if let Some(end) = rest.find(|c: char| !(c.is_alphanumeric() || c == '_'))
                        && (rest[end..].starts_with('(') || rest[end..].starts_with('<'))
                    {
                        function = rest[..end].to_owned();
                    }
                }
                if starts_an_operation(&line) || counts_as_writing(&line, own_counter) {
                    found.insert(function.clone());
                }
            }
        }
        // A name in IN_CONTRACT is checked only through its operation, so one without an
        // operation would pass unchecked.
        let ops: Vec<&str> = contract_ops().iter().map(|o| o.0).collect();
        let no_op: Vec<&(&str, &str)> = (IN_CONTRACT.iter())
            .filter(|(_, op)| !ops.contains(op))
            .collect();
        assert!(no_op.is_empty(), "no operation in the contract: {no_op:?}");
        let known: Vec<&str> = (IN_CONTRACT.iter().map(|o| o.0))
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
        // Work counts itself as writing however file_io's counter is named. Before, only the
        // whole path was seen, so `Working::new(&WRITING)` after a `use` passed unseen.
        for spelling in [
            "let w = Working::new(&crate::files::file_io::WRITING);",
            "let w = Working::new(&file_io::WRITING);",
            "let w = crate::support::threads::Working::new(&WRITING);",
            "_writing: Working::new( &WRITING ),",
        ] {
            assert!(counts_as_writing(spelling, false), "missed: {spelling}");
        }
        assert!(!counts_as_writing("Working::new(&WRITING);", true));
        assert!(!counts_as_writing("Working::new(&self.busy);", false));
        // An operation is started however its receiver is spelled. Before, only `self.` was
        // seen, so one started as `app.` in a closure that is given the App passed unseen.
        for spelling in [
            "app.start_job(ctx, x);",
            "            .start_job_of(\"Moving\", 1, false, paths)",
            "crate::App::run_in_background(app, ctx, held, work);",
            "let held = window.hold_back_while_writing(paths)?;",
            "paths.iter().map(App::run_in_background);",
        ] {
            assert!(starts_an_operation(spelling), "missed: {spelling}");
        }
        for other in [
            "pub(crate) fn start_job(",
            "fn run_in_background<W>(",
            "restart_job(x);",
            "let start_job_count = 1;",
        ] {
            assert!(!starts_an_operation(other), "taken: {other}");
        }
    }

    /// Returns whether `line` starts a job or background work: a call of one of the functions
    /// that do, with any receiver or path, or one of them given as a value. Its own definition
    /// is not a start.
    fn starts_an_operation(line: &str) -> bool {
        const STARTS: [&str; 4] = [
            "start_job",
            "start_job_of",
            "run_in_background",
            "hold_back_while_writing",
        ];
        STARTS.iter().any(|name| {
            line.match_indices(name).any(|(at, _)| {
                let before = &line[..at];
                let after = &line[at + name.len()..];
                !before.ends_with(|c: char| c.is_alphanumeric() || c == '_')
                    && !before.trim_end().ends_with("fn")
                    && (after.starts_with(['(', ')', ',']) || after.starts_with("::<"))
            })
        })
    }

    /// Returns whether `line` counts work as writing the user's files through file_io's
    /// `WRITING`. `own_counter` says that the file has a counter of its own named `WRITING`,
    /// which the bare name then means.
    fn counts_as_writing(line: &str, own_counter: bool) -> bool {
        (line.match_indices("Working::new(")).any(|(at, s)| {
            let argument = line[at + s.len()..].split(')').next().unwrap_or_default();
            let path = argument.trim().trim_start_matches('&').trim();
            path.ends_with("file_io::WRITING") || path == "WRITING" && !own_counter
        })
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
        let fingerprint = super::fingerprint(&dir.join("a.png")).unwrap();
        app.file_operations.undo = Some(super::Undo::Rename(vec![(
            dir.join("b.png"),
            dir.join("a.png"),
            fingerprint,
        )]));
        crate::support::test_support::open_now(&mut app, &ctx, empty.clone());
        app.undo(&ctx);
        assert!(run_background_until(&mut app, &ctx, |a| !a.working()));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Work in the background lets go of its files before the busy pointer goes. The window
    /// starts the next operation on them once no work runs, and before, a thread slowed between
    /// the two had that operation refused ("Still working on these files").
    #[test]
    fn background_work_lets_go_of_its_files_before_the_pointer() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-held-pointer-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = app_on(&dir);
        let file = vec![dir.join("a.png")];
        for _ in 0..500 {
            assert!(app.run_in_background(&ctx, file.clone(), || None));
            // A spin, not a sleep, so the check falls into the moment between the two. It
            // gives up after five seconds, so work that never ends fails the test, not hangs it.
            let start = std::time::Instant::now();
            while app.working() {
                assert!(start.elapsed().as_secs() < 5, "the work never ended");
            }
            assert!(app.are_free_to_change(&file), "{:?}", app.notice.text());
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A turn saved after another program removed the file is told as not saved. Before, it was
    /// taken as saved, and a JPEG with Auto Rotate off was even told "Saved".
    #[test]
    fn a_turn_of_a_file_removed_meanwhile_is_not_saved() {
        let dir = std::env::temp_dir().join(format!("glimt-test-turn-gone-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.jpg");
        image::RgbImage::new(4, 4).save(&a).unwrap();
        let (mut app, ctx) = app_on(&dir);
        app.config.exif_rotate = false;
        app.viewing.set_turn((1, false), a.clone());
        std::fs::remove_file(&a).unwrap();
        app.notice.clear();
        app.save_orientation(&ctx);
        // Another test's failed save may be told in this window too, so the notice is watched
        // as it comes rather than read at the end.
        let not_saved = |app: &crate::App| {
            (app.notice.text()).is_some_and(|t| t.starts_with("Could not save the orientation"))
        };
        assert!(run_background_until(&mut app, &ctx, not_saved));
        assert!(!a.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A turn is saved into the picture that was decoded and shown. Another file put under its
    /// name since (another window's Rename Many that swapped two names) is left as it is. The
    /// turn meant for one photo was written into the other.
    #[test]
    fn a_turn_is_not_saved_into_another_file_under_the_name() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-turn-other-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, other) = (dir.join("a.png"), dir.join("other.png"));
        image::RgbImage::new(4, 2).save(&a).unwrap();
        let (mut app, ctx) = app_on(&dir);
        app.refill_preload();
        assert!(run_background_until(&mut app, &ctx, |app| app
            .list
            .loaded_images()
            .contains_key(&0)));
        // Another photo takes the name, outside the folder so no listing sees it first.
        image::RgbImage::new(8, 2).save(&other).unwrap();
        std::fs::rename(&other, &a).unwrap();
        app.viewing.set_turn((1, false), a.clone());
        app.notice.clear();
        app.save_orientation(&ctx);
        let not_saved = |app: &crate::App| {
            (app.notice.text()).is_some_and(|t| t.starts_with("Could not save the orientation"))
        };
        assert!(run_background_until(&mut app, &ctx, not_saved));
        assert_eq!(
            image::image_dimensions(&a).unwrap(),
            (8, 2),
            "the other photo was turned"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A delete takes its file out of the collection also when the rename that gave the file its
    /// name still waits in the writer thread. Before, the delete found no entry under the new
    /// name, and the writer then wrote that name of a trashed file into the collection.
    #[test]
    fn a_delete_waits_for_a_rename_queued_for_the_collection() {
        let dir = contract_folder("forget-queued");
        let (gqv, a, renamed) = (dir.join("col.gqv"), dir.join("a.png"), dir.join("c.png"));
        let (file, from, to) = (gqv.clone(), a.clone(), renamed.clone());
        crate::files::file_io::write_later_in_user_folders(move || {
            std::thread::sleep(std::time::Duration::from_millis(200));
            let rename = |all: &mut Vec<std::path::PathBuf>| {
                (all.iter_mut().filter(|p| **p == from)).for_each(|p| *p = to.clone())
            };
            crate::model::collections::edit_open_collection(&file, rename).unwrap();
        });
        let removed =
            super::forget_in_collection(Some(gqv.as_path()), std::slice::from_ref(&renamed))
                .unwrap();
        assert_eq!(removed, [(0, renamed)]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A look that sends a sign after each file is waited for as long as it goes on, also
    /// past `LOOK_UP_GIVEN_UP_AFTER` in all. Before, the time counted from the start, so Find
    /// Duplicates' look at many files on a share that answered was given up and nothing was
    /// moved to the trash.
    #[test]
    fn a_look_up_that_goes_on_is_waited_for() {
        use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
        let dir = std::env::temp_dir().join(format!("glimt-test-lookon-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = crate::support::test_support::app_on(&dir);
        let (tx, found) = std::sync::mpsc::channel();
        let step = super::LOOK_UP_GIVEN_UP_AFTER / 2;
        std::thread::spawn(move || {
            for _ in 0..3 {
                std::thread::sleep(step);
                drop(tx.send(super::LookUpStep::GoesOn));
            }
            drop(tx.send(super::LookUpStep::Answered(())));
        });
        let ran = std::sync::Arc::new(AtomicBool::new(false));
        let then = ran.clone();
        app.wait_for_look_up_before_the_job(
            dir.clone(),
            (Vec::new(), false),
            found,
            move |_, _, ()| then.store(true, SeqCst),
        );
        // The look answers after one and a half times the limit, which is longer than one wait.
        let answered = |_: &crate::App| ran.load(SeqCst);
        assert!(
            run_background_until(&mut app, &ctx, answered)
                || run_background_until(&mut app, &ctx, answered)
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A look at a folder before its job that does not answer (a share that stopped answering)
    /// is given up: the files it held are let go, its job never starts, and a notice says so.
    /// Before, they stayed held until Glimt ended, and a delete or rename of them was refused.
    /// Ctrl+Z meanwhile waits, as the job would leave an Undo; before, it undid the operation
    /// before it.
    #[test]
    fn a_look_up_that_does_not_answer_lets_its_files_go() {
        use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
        let dir = std::env::temp_dir().join(format!("glimt-test-lookhang-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.png");
        image::RgbImage::new(4, 4).save(&a).unwrap();
        let (mut app, ctx) = crate::support::test_support::app_on(&dir);
        // A look at a folder on the share, which never answers.
        let (_never_sent, found) = std::sync::mpsc::channel::<super::LookUpStep<()>>();
        let ran = std::sync::Arc::new(AtomicBool::new(false));
        let then = ran.clone();
        app.wait_for_look_up_before_the_job(
            dir.clone(),
            (vec![a.clone()], true),
            found,
            move |_, _, ()| then.store(true, SeqCst),
        );
        assert!(app.is_changed_by_running_work(std::slice::from_ref(&a)));
        app.undo(&ctx);
        let told = app.notice.text().unwrap_or_default().to_owned();
        assert!(told.contains("Still working"), "{told}");
        let let_go = |app: &crate::App| !app.is_changed_by_running_work(std::slice::from_ref(&a));
        assert!(
            run_background_until(&mut app, &ctx, let_go),
            "the file is held for good"
        );
        assert!(!ran.load(SeqCst) && app.notice.text().is_some_and(|t| t.contains("a minute")));
        assert!(app.are_free_to_change(std::slice::from_ref(&a)));
        assert_eq!(
            std::sync::Arc::strong_count(&app.file_operations.undo_on_the_way),
            1
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// An answer that the window must handle keeps the busy pointer on until the window has
    /// handled it. Before, the thread let the pointer go as it sent the answer, so a window that
    /// saw no work running had not yet started the Copy that waited for its folder's real path,
    /// and still held a turn as being saved. A close at that moment dropped the Copy.
    #[test]
    fn answers_keep_the_busy_pointer_until_the_window_handled_them() {
        let dir = std::env::temp_dir().join(format!("glimt-test-answer-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("empty")).unwrap();
        use std::sync::atomic::Ordering::SeqCst;
        let (mut app, ctx) = app_on(&dir.join("empty"));
        // A spin that takes the answers, so the check falls into the moment between the send
        // and the window's handling. It returns whether `handled` holds once no work runs.
        let idle_and = |app: &mut crate::App, handled: &dyn Fn(&crate::App) -> bool| loop {
            if !app.working() {
                return handled(app);
            }
            app.poll_background(&ctx);
        };
        for _ in 0..300 {
            let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let then = ran.clone();
            app.with_real_folder(dir.clone(), (Vec::new(), false), move |_, _, _, _| {
                then.store(true, SeqCst)
            });
            // The pending look-up counts as work that a close waits for.
            assert!(app.is_writing() || ran.load(SeqCst));
            assert!(idle_and(&mut app, &|_| ran.load(SeqCst)));
        }
        let a = dir.join("a.png");
        image::RgbImage::new(4, 4).save(&a).unwrap();
        crate::support::test_support::open_now(&mut app, &ctx, dir.clone());
        for _ in 0..100 {
            app.viewing.set_turn((1, false), a.clone());
            app.save_orientation(&ctx);
            assert!(idle_and(&mut app, &|app| !app
                .file_operations
                .is_saving(&a)));
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Every kind of Undo leaves alone a file that another program put in the place of the one
    /// it changed, also after Glimt wrote that file anew (a turn saved). Before, an Undo of a
    /// rename went by the names alone, and a turn saved after a move dropped the move's check.
    #[test]
    fn every_undo_leaves_a_file_put_in_its_place() {
        // A new kind of Undo fails to build here until it is checked below.
        let _checked = |undo: &super::Undo| match undo {
            // `a_refused_undo_from_the_trash_keeps_its_marks`: nothing is put over a name.
            super::Undo::Trash(_) => (),
            super::Undo::Move(_) | super::Undo::Rename(_) => (),
        };
        let dir =
            std::env::temp_dir().join(format!("glimt-test-undo-every-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let (mut app, ctx) = app_on(&dir);
        // In place, the other file keeps the inode, as a freed inode goes to the next new file.
        let replace = |path: &std::path::Path, in_place: bool| {
            if in_place {
                std::fs::write(path, b"another").unwrap();
                return;
            }
            std::fs::write(dir.join("new"), b"another").unwrap();
            std::fs::rename(dir.join("new"), path).unwrap();
        };
        let refused =
            |app: &crate::App| (app.notice.text()).is_some_and(|n| n.contains("Undo failed"));
        let cases = ["move", "rename"].into_iter().flat_map(|kind| {
            [(false, false), (true, false), (false, true)].map(|(s, i)| (kind, s, i))
        });
        for (kind, saved_first, in_place) in cases {
            let a = dir.join("a.png");
            image::RgbImage::new(2, 2).save(&a).unwrap();
            let now = match kind {
                "move" => {
                    app.transfer(
                        crate::files::file_io::PickedFiles::new(vec![a.clone()]),
                        dir.join("sub"),
                        false,
                    );
                    dir.join("sub/a.png")
                }
                _ => {
                    app.rename_in_background(
                        &ctx,
                        crate::support::test_support::unlisted(vec![a.clone()]),
                        vec![dir.join("b.png")],
                        crate::files::file_io::PickedFiles::new(vec![a.clone()]),
                    );
                    dir.join("b.png")
                }
            };
            assert!(run_background_until(&mut app, &ctx, |app| app
                .file_operations
                .undo
                .is_some()
                && !app.working()));
            if saved_first {
                std::fs::copy(&now, dir.join("turned")).unwrap();
                std::fs::rename(dir.join("turned"), &now).unwrap();
                app.written_anew(std::slice::from_ref(&now));
            }
            replace(&now, in_place);
            app.notice.clear();
            app.undo(&ctx);
            assert!(run_background_until(&mut app, &ctx, |app| !app.working() && refused(app)));
            assert!(
                !a.exists(),
                "{kind}, in place {in_place}: the other file was put back as a.png"
            );
            assert_eq!(
                std::fs::read(&now).unwrap(),
                b"another",
                "{kind}, in place {in_place}"
            );
            std::fs::remove_file(&now).unwrap();
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Copy Path quotes a path so that a shell gives back the name, and runs nothing in it. A `!`
    /// is not expanded to an earlier command line either.
    #[test]
    fn copy_path_quotes_what_a_shell_reads() {
        assert_eq!(super::quoted_for_shell("/p/a b.jpg"), r#""/p/a b.jpg""#);
        assert_eq!(
            super::quoted_for_shell(r#"/p/$(id)`id`"x\.jpg"#),
            r#""/p/\$(id)\`id\`\"x\\.jpg""#
        );
        assert_eq!(
            super::quoted_for_shell("/p/a!-1.jpg"),
            r#""/p/a"'!'"-1.jpg""#
        );
    }

    /// Ctrl+Z while a move to the trash still runs waits for it, rather than undoing the move
    /// before it, and afterwards undoes the move to the trash.
    #[test]
    fn undo_waits_for_the_operation_still_running() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-undo-waits-{}", std::process::id()));
        let sub = dir.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let (a, c) = (dir.join("a.png"), dir.join("c.png"));
        image::RgbImage::new(2, 2).save(&a).unwrap();
        image::RgbImage::new(2, 2).save(&c).unwrap();
        let (mut app, ctx) = app_on(&dir);
        app.transfer(
            crate::files::file_io::PickedFiles::new(vec![c.clone()]),
            sub.clone(),
            false,
        );
        assert!(run_background_until(&mut app, &ctx, |app| !app.working()
            && app.file_operations.undo.is_some()));
        // The delete waits for a.png's sidecar lock, so it is still running at the Ctrl+Z.
        let lock = crate::metadata::xmp::lock_sidecar(&a);
        app.delete_targets(&ctx, true, vec![0], None);
        app.undo(&ctx);
        assert!(
            sub.join("c.png").exists() && !c.exists(),
            "the older move was undone"
        );
        assert!(matches!(
            app.file_operations.undo,
            Some(super::Undo::Move(_))
        ));
        drop(lock);
        assert!(run_background_until(&mut app, &ctx, |app| !app.working()
            && matches!(
                app.file_operations.undo,
                Some(super::Undo::Trash(_))
            )));
        app.undo(&ctx);
        assert!(run_background_until(&mut app, &ctx, |app| !app.working() && a.exists()));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Ctrl+Z while a rename still runs waits for it too, and afterwards gives the old name back.
    #[test]
    fn undo_waits_for_the_rename_still_running() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-undo-rename-{}", std::process::id()));
        let sub = dir.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let (a, b, c) = (dir.join("a.png"), dir.join("b.png"), dir.join("c.png"));
        image::RgbImage::new(2, 2).save(&a).unwrap();
        image::RgbImage::new(2, 2).save(&c).unwrap();
        let (mut app, ctx) = app_on(&dir);
        app.transfer(
            crate::files::file_io::PickedFiles::new(vec![c.clone()]),
            sub.clone(),
            false,
        );
        assert!(run_background_until(&mut app, &ctx, |app| !app.working()
            && app.file_operations.undo.is_some()));
        // The rename waits for a.png's sidecar lock, so it is still running at the Ctrl+Z.
        let lock = crate::metadata::xmp::lock_sidecar(&a);
        app.rename_in_background(
            &ctx,
            crate::support::test_support::unlisted(vec![a.clone()]),
            vec![b.clone()],
            crate::files::file_io::PickedFiles::new(vec![a.clone()]),
        );
        app.undo(&ctx);
        assert!(
            sub.join("c.png").exists() && !c.exists(),
            "the older move was undone"
        );
        assert!(matches!(
            app.file_operations.undo,
            Some(super::Undo::Move(_))
        ));
        drop(lock);
        assert!(run_background_until(&mut app, &ctx, |app| !app.working()
            && matches!(
                app.file_operations.undo,
                Some(super::Undo::Rename(_))
            )));
        app.undo(&ctx);
        assert!(run_background_until(&mut app, &ctx, |app| !app.working() && a.exists()));
        assert!(!b.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// An Undo of a rename that fails, here because a new file has taken the old name, stays to
    /// be done again once the name is free.
    #[test]
    fn a_failed_undo_of_a_rename_stays() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-undo-again-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a.png"), dir.join("b.png"));
        image::RgbImage::new(2, 2).save(&a).unwrap();
        let (mut app, ctx) = app_on(&dir);
        app.rename_in_background(
            &ctx,
            crate::support::test_support::unlisted(vec![a.clone()]),
            vec![b.clone()],
            crate::files::file_io::PickedFiles::new(vec![a.clone()]),
        );
        assert!(run_background_until(&mut app, &ctx, |app| app
            .file_operations
            .undo
            .is_some()
            && !app.working()));
        std::fs::write(&a, b"another").unwrap();
        app.undo(&ctx);
        assert!(run_background_until(&mut app, &ctx, |app| !app.working()
            && app.notice.text().is_some()));
        assert!(
            b.exists() && app.file_operations.undo.is_some(),
            "the Undo was lost"
        );
        std::fs::remove_file(&a).unwrap();
        app.undo(&ctx);
        assert!(run_background_until(&mut app, &ctx, |app| !app.working() && a.exists()));
        assert!(!b.exists() && app.file_operations.undo.is_none());
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
        app.transfer(
            crate::files::file_io::PickedFiles::new(vec![a.clone(), b.clone()]),
            dir.join("sub"),
            false,
        );
        assert!(run_background_until(&mut app, &ctx, |app| app
            .file_operations
            .undo
            .is_some()
            && !app.working()));
        // A file manager replaces sub/a.png with another file, written beside it first.
        let moved = dir.join("sub/a.png");
        std::fs::write(dir.join("sub/new"), b"another").unwrap();
        std::fs::rename(dir.join("sub/new"), &moved).unwrap();
        app.undo(&ctx);
        assert!(run_background_until(&mut app, &ctx, |app| !app.working() && b.exists()));
        assert!(!a.exists(), "the other file was moved back as a.png");
        assert_eq!(std::fs::read(&moved).unwrap(), b"another");
        // A file Glimt itself wrote anew there (a turn saved) is still the one to move back.
        app.transfer(
            crate::files::file_io::PickedFiles::new(vec![b.clone()]),
            dir.join("sub"),
            false,
        );
        assert!(run_background_until(&mut app, &ctx, |app| !b.exists() && !app.working()));
        let saved = dir.join("sub/b.png");
        std::fs::copy(&saved, dir.join("sub/new")).unwrap();
        std::fs::rename(dir.join("sub/new"), &saved).unwrap();
        app.written_anew(std::slice::from_ref(&saved));
        app.undo(&ctx);
        assert!(run_background_until(&mut app, &ctx, |app| !app.working() && b.exists()));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Move To and Copy To act only on the files picked. While the folder chooser was open,
    /// another window swapped the names of two photos. The photo now at the picked name is
    /// neither moved nor copied, and the user is told. Before, the other photo went.
    #[test]
    fn a_file_whose_name_another_took_after_the_pick_stays() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-after-pick-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let (a, b) = (dir.join("a.png"), dir.join("b.png"));
        image::RgbImage::new(2, 2).save(&a).unwrap();
        image::RgbImage::new(3, 3).save(&b).unwrap();
        let (mut app, ctx) = app_on(&dir);
        for is_copy in [true, false] {
            // The fingerprint is taken before the names swap, as while the chooser is open.
            let picked = (a.clone(), crate::files::file_io::fingerprint(&a).unwrap());
            let at_pick = crate::files::file_io::PickedFiles::recorded(vec![picked]);
            std::fs::rename(&a, dir.join("t")).unwrap();
            std::fs::rename(&b, &a).unwrap();
            std::fs::rename(dir.join("t"), &b).unwrap();
            app.notice.clear();
            app.transfer(at_pick, dir.join("sub"), is_copy);
            assert!(run_background_until(&mut app, &ctx, |app| app
                .notice
                .text()
                .is_some()
                && !app.working()));
            let told = app.notice.text().unwrap_or_default().to_owned();
            assert!(told.contains("changed"), "{told}");
            assert!(!dir.join("sub/a.png").exists(), "the other photo went");
            assert!(a.exists() && b.exists());
        }
        // A file that is still the one picked goes.
        let at_pick = crate::files::file_io::PickedFiles::new(vec![a.clone()]);
        app.transfer(at_pick, dir.join("sub"), false);
        assert!(run_background_until(&mut app, &ctx, |app| !a.exists() && !app.working()));
        assert!(dir.join("sub/a.png").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Undo of a move keeps a file that could not go back, here because a new file has taken
    /// its old name, as an Undo to be done again once the name is free. The others go back.
    #[test]
    fn undo_of_a_move_keeps_the_files_that_did_not_go_back() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-undo-move-left-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        for n in ["a.png", "b.png"] {
            image::RgbImage::new(2, 2).save(dir.join(n)).unwrap();
        }
        let (mut app, ctx) = app_on(&dir);
        let (a, b) = (dir.join("a.png"), dir.join("b.png"));
        app.transfer(
            crate::files::file_io::PickedFiles::new(vec![a.clone(), b.clone()]),
            dir.join("sub"),
            false,
        );
        assert!(run_background_until(&mut app, &ctx, |app| app
            .file_operations
            .undo
            .is_some()
            && !app.working()));
        std::fs::write(&a, b"another").unwrap();
        app.undo(&ctx);
        assert!(run_background_until(&mut app, &ctx, |app| !app.working()
            && b.exists()
            && app.file_operations.undo.is_some()));
        assert!(dir.join("sub/a.png").exists());
        std::fs::remove_file(&a).unwrap();
        app.undo(&ctx);
        assert!(run_background_until(&mut app, &ctx, |app| !app.working() && a.exists()));
        assert!(!dir.join("sub/a.png").exists() && app.file_operations.undo.is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Undo of a move syncs the folder that the files went back to, before the marks and
    /// collections follow them there.
    #[test]
    fn undo_of_a_move_syncs_the_folder_it_moved_back_to() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-undo-move-sync-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let (a, moved) = (dir.join("a.png"), dir.join("sub/a.png"));
        std::fs::write(&moved, "x").unwrap();
        let pair = super::with_fingerprint(&(a.clone(), moved)).unwrap();
        crate::files::file_io::FOLDERS_SYNCED.take();
        let (back, left, errors) = super::move_back(vec![pair]);
        assert_eq!((back.len(), left.len(), errors.len()), (1, 0, 0));
        assert!(a.exists());
        assert!(crate::files::file_io::FOLDERS_SYNCED.take().contains(&dir));
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
        app.transfer(
            crate::files::file_io::PickedFiles::new(vec![a.clone()]),
            dir.join("sub"),
            false,
        );
        assert!(run_background_until(&mut app, &ctx, |app| app
            .file_operations
            .undo
            .is_some()
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
        assert!(run_background_until(&mut app, &ctx, |app| !app.working() && a.exists()));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Where the file system gives every file inode 0 (sshfs with `-o use_ino`), Undo of a move
    /// tells the moved file from another put at its name by the size and time alone. Before, the
    /// inodes 0 were equal, so any file there was taken for the moved one and moved back.
    #[test]
    fn undo_of_a_move_tells_files_without_inodes_apart() {
        // `fingerprint` gives no device and inode where `save::file_id_from_metadata` gives a path
        // for inode 0.
        assert!(
            crate::images::save::file_id_from_inode(std::path::Path::new("/a"), (1, 0)).is_err()
        );
        let (moved, other) = ((None, 1, (5, 0)), (None, 7, (9, 0)));
        assert!(
            !super::same_fingerprint(moved, other),
            "another file was taken for the moved one"
        );
        assert!(super::same_fingerprint(moved, moved));
        // An inode that changed (FAT) still leaves the size and time to tell the file.
        assert!(super::same_fingerprint(
            (Some((1, 2)), 1, (5, 0)),
            (Some((1, 3)), 1, (5, 0))
        ));
        assert!(!super::same_fingerprint(
            (Some((1, 2)), 1, (5, 0)),
            (Some((1, 3)), 7, (9, 0))
        ));
        assert!(!super::same_fingerprint(
            (Some((1, 2)), 1, (5, 0)),
            (Some((2, 2)), 1, (5, 0))
        ));
        // A freed inode given to another file at the name is not the moved file.
        assert!(!super::same_fingerprint(
            (Some((1, 2)), 1, (5, 0)),
            (Some((1, 2)), 7, (9, 0))
        ));
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
        app.transfer(
            crate::files::file_io::PickedFiles::new(vec![a.clone()]),
            dir.join("real/sub"),
            false,
        );
        assert!(run_background_until(&mut app, &ctx, |app| app
            .file_operations
            .undo
            .is_some()
            && !app.working()));
        // The turn is saved as a new file put in the old one's place, under the link's name.
        let saved = dir.join("link/sub/a.png");
        std::fs::copy(&saved, dir.join("real/sub/new")).unwrap();
        std::fs::rename(dir.join("real/sub/new"), &saved).unwrap();
        app.written_anew(std::slice::from_ref(&saved));
        app.undo(&ctx);
        assert!(run_background_until(&mut app, &ctx, |app| !app.working() && a.exists()));
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
        app.transfer(
            crate::files::file_io::PickedFiles::new(vec![a.clone()]),
            dir.join("sub"),
            false,
        );
        assert!(run_background_until(&mut app, &ctx, |app| app
            .file_operations
            .undo
            .is_some()
            && !app.working()));
        // Save As writes another image beside sub/a.png and renames it over, then tells so.
        let moved = dir.join("sub/a.png");
        image::RgbImage::new(3, 3)
            .save(dir.join("sub/new.png"))
            .unwrap();
        std::fs::rename(dir.join("sub/new.png"), &moved).unwrap();
        let over = super::OperationReport {
            overwritten: vec![moved.clone()],
            ..Default::default()
        };
        app.file_operations.report_tx.send(over).unwrap();
        app.receive_operation_report(&ctx);
        app.undo(&ctx);
        let refused =
            |app: &crate::App| (app.notice.text()).is_some_and(|n| n.contains("Undo failed"));
        assert!(run_background_until(&mut app, &ctx, |app| !app.working() && refused(app)));
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
        let picked_as = crate::files::file_io::fingerprint(&dir.join("a.png")).unwrap();
        let (mut app, ctx) = app_on(&dir);
        app.file_operations.deleting.insert("a.png".into());
        app.remove_indices(&ctx, &[0]);
        let refused = super::OperationReport {
            requested: vec![dir.join("a.png")],
            any_failed: true,
            untrashable: Some(("fuse.rclone".into(), vec![(dir.join("a.png"), picked_as)])),
            ..Default::default()
        };
        app.file_operations.report_tx.send(refused).unwrap();
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
        assert_eq!(app.asked_targets().map(|(found, _)| found), Some(vec![0]));
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
        let file_system_type = |folder: &Path| {
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
        let left = super::left_by_trash(&paths, file_system_type);
        let kind = "fuse.rclone".to_owned();
        assert_eq!(
            left,
            Some((kind, vec![cloud.join("a.png"), cloud.join("b.png")]))
        );
        assert_eq!(looked.get(), 2);
        assert_eq!(
            super::left_by_trash(&[local.join("c.png")], file_system_type),
            None
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The files the trash refused on a network or cloud mount are asked about, to delete them
    /// for good instead, when the list shows them. Files of another folder, or of a subfolder
    /// the list does not show, are not, because their names could be other files' in this
    /// list, and a notice tells that nothing was deleted. The question waits until the folder
    /// is read again, because the delete took the files out of the list. A file refused twice
    /// is asked about once. The delete for good acts only on the file as it was first picked:
    /// one that another program put at its name since is left alone. Before, its fingerprint
    /// was taken at the second question, which took the other file for the one picked.
    #[test]
    fn asks_to_delete_for_good_what_the_trash_refused_on_a_network_mount() {
        use std::path::PathBuf;
        let dir =
            std::env::temp_dir().join(format!("glimt-test-untrashable-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbImage::new(2, 2).save(dir.join("a.png")).unwrap();
        let picked_as = crate::files::file_io::fingerprint(&dir.join("a.png")).unwrap();
        let (mut app, ctx) = app_on(&dir);
        let refused = |files: Vec<PathBuf>| super::OperationReport {
            untrashable: Some((
                "fuse.rclone".into(),
                files.into_iter().map(|f| (f, picked_as)).collect(),
            )),
            ..Default::default()
        };
        // A refusal comes with the delete failed, which has the folder read again. A file of
        // another folder, and one of a subfolder the list does not show, are told, not asked.
        let elsewhere = super::OperationReport {
            any_failed: true,
            ..refused(vec![
                PathBuf::from("/elsewhere/a.png"),
                dir.join("sub/b.png"),
            ])
        };
        app.file_operations.report_tx.send(elsewhere).unwrap();
        assert!(run_background_until(&mut app, &ctx, |app| app
            .notice
            .text()
            .is_some()));
        assert!(!app.modal_open());
        let told = app.notice.set_aside().unwrap().0;
        assert!(told.contains("2 files (fuse.rclone)"), "{told}");
        // The delete takes the file out of the list as it starts, as `delete_now` does.
        app.file_operations.deleting.insert("a.png".into());
        app.remove_indices(&ctx, &[0]);
        let failed = super::OperationReport {
            requested: vec![dir.join("a.png")],
            any_failed: true,
            ..refused(vec![dir.join("a.png")])
        };
        // Another program puts another photo at the name before the question.
        std::fs::write(dir.join("a.png"), "another photo").unwrap();
        app.file_operations.report_tx.send(failed).unwrap();
        // The same file refused again, by a second Delete, is counted once.
        app.file_operations
            .report_tx
            .send(refused(vec![dir.join("a.png")]))
            .unwrap();
        assert!(run_background_until(&mut app, &ctx, |app| app.modal_open()));
        assert_eq!(app.dialogs.asked_names(), ["a.png"]);
        let (found, picked) = app.asked_targets().unwrap();
        assert_eq!(found, vec![0]);
        app.notice.clear();
        app.delete_now(&ctx, false, found, picked);
        let told = |app: &crate::App| (app.notice.text()).is_some_and(|n| n.contains("changed"));
        assert!(
            run_background_until(&mut app, &ctx, told),
            "{:?}",
            app.notice.text()
        );
        assert_eq!(std::fs::read(dir.join("a.png")).unwrap(), b"another photo");
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
        app.file_operations.undo = Some(super::Undo::Trash(crate::files::trash::TrashMove {
            files: vec![a.clone()],
            sidecars: Vec::new(),
            sidecars_left: Vec::new(),
            since: std::time::SystemTime::now(),
            marks: Default::default(),
            collection: None,
        }));
        app.undo(&ctx);
        app.file_operations.undo = Some(super::Undo::Move(Vec::new()));
        // The thread reports before it tells, so its report is waiting once it told.
        let told = app.notices.recv_timeout(std::time::Duration::from_secs(5));
        assert!(told.is_ok());
        app.receive_operation_report(&ctx);
        assert!(matches!(
            app.file_operations.undo,
            Some(super::Undo::Move(_))
        ));
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
        app.file_operations.undo = Some(super::Undo::Trash(crate::files::trash::TrashMove {
            files: vec![a.clone(), b.clone()],
            sidecars: Vec::new(),
            sidecars_left: Vec::new(),
            since: std::time::SystemTime::now(),
            marks: std::sync::Arc::new(std::sync::Mutex::new(vec![(a.clone(), (1, 3))])),
            collection: Some((gqv.clone(), vec![(0, a.clone())])),
        }));
        app.undo(&ctx);
        assert!(app.file_operations.undo.is_none());
        assert!(run_background_until(&mut app, &ctx, |a| a
            .file_operations
            .undo
            .is_some()));
        let Some(super::Undo::Trash(trash_move)) = &app.file_operations.undo else {
            panic!("the Undo was lost");
        };
        assert_eq!(trash_move.files, [a.clone(), b]);
        assert_eq!(
            *lock_even_if_poisoned(&trash_move.marks),
            [(a.clone(), (1, 3))]
        );
        assert_eq!(trash_move.collection, Some((gqv, vec![(0, a)])));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Undo from the trash waits for the marks writer as long as it is held, and puts no file
    /// back without its marks. Stopped, it puts nothing back and stays to be done again with its
    /// marks. Once the writer goes on, the file comes back.
    #[test]
    fn undo_from_the_trash_waits_for_the_marks_or_stops() {
        let dir = std::env::temp_dir().join(format!("glimt-test-undo-held-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.png");
        image::RgbImage::new(2, 2).save(&a).unwrap();
        let (mut app, ctx) = app_on(&dir);
        let since = std::time::SystemTime::now() - std::time::Duration::from_secs(1);
        crate::files::trash::trash(std::slice::from_ref(&a)).unwrap();
        app.file_operations.undo = Some(super::Undo::Trash(crate::files::trash::TrashMove {
            files: vec![a.clone()],
            sidecars: Vec::new(),
            sidecars_left: Vec::new(),
            since,
            marks: std::sync::Arc::new(std::sync::Mutex::new(vec![(a.clone(), (1, 3))])),
            collection: None,
        }));
        // The writer is held, as by another window's lock, until `release` is dropped.
        let (release, held) = std::sync::mpsc::channel::<()>();
        crate::files::file_io::write_later(move || {
            let _ = held.recv();
        });
        app.undo(&ctx);
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(!a.exists(), "the file came back before its marks");
        app.file_operations.stop_jobs();
        assert!(run_background_until(&mut app, &ctx, |app| app
            .file_operations
            .undo
            .is_some()
            && !app.working()));
        assert!(!a.exists());
        let Some(super::Undo::Trash(trash_move)) = &app.file_operations.undo else {
            panic!("the Undo was lost");
        };
        assert_eq!(
            *lock_even_if_poisoned(&trash_move.marks),
            [(a.clone(), (1, 3))]
        );
        drop(release);
        app.undo(&ctx);
        assert!(run_background_until(&mut app, &ctx, |app| !app.working() && a.exists()));
        assert!(app.file_operations.undo.is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A sidecar write asked for while Rename Many swaps two names waits until the rename is
    /// over, and then lands on the photo it was asked for, under its new name, with what was
    /// asked for that photo. Before, it was written to the old name, which by then held the
    /// other photo. Then Lightroom's import found its keywords by the new name, and wrote the
    /// other photo's. A writer held by another job makes the write come after the rename,
    /// however fast the computer is.
    #[test]
    fn a_sidecar_write_during_a_rename_follows_its_photo() {
        let d = contract_folder("write-during-rename");
        let (a, b) = (d.join("a.png"), d.join("b.png"));
        // The fixture's sidecars only say whose they are, which is not XMP to write into.
        for p in [&a, &b] {
            std::fs::remove_file(crate::metadata::xmp::sidecar(p)).unwrap();
        }
        let (mut app, ctx) = app_on(&d);
        app.rename_in_background(
            &ctx,
            crate::support::test_support::unlisted(vec![a.clone(), b.clone()]),
            vec![b.clone(), a.clone()],
            crate::files::file_io::PickedFiles::new(vec![a.clone(), b.clone()]),
        );
        assert!(app.file_operations.renaming.is_some());
        let (go, released) = std::sync::mpsc::channel::<()>();
        let other = vec![(d.join("sub"), ())];
        app.write_sidecars(
            &ctx,
            "Other",
            crate::support::test_support::unlisted_with(other),
            move |_, _| released.recv().map_or(Ok(()), |()| Err(String::new())),
            |_, _| None,
        );
        // Each photo gets a keyword of its own, as Lightroom's import gives them.
        let keywords = vec![
            (a.clone(), "Oslo".to_string()),
            (b.clone(), "Bergen".into()),
        ];
        app.write_sidecars(
            &ctx,
            "Keywords",
            crate::support::test_support::unlisted_with(keywords),
            |p, tag| crate::metadata::xmp::add_tags(p, std::slice::from_ref(tag)),
            |_, _| None,
        );
        assert!(run_background_until(&mut app, &ctx, |app| app
            .file_operations
            .renaming
            .is_none()));
        drop(go);
        assert!(run_background_until(&mut app, &ctx, |app| !app
            .file_operations
            .is_writing_sidecars()));
        let red = |p: &std::path::Path| image::open(p).unwrap().to_rgb8().get_pixel(0, 0)[0] == 255;
        assert!(red(&b), "the names were not swapped");
        let keywords = |p: &std::path::Path| crate::metadata::xmp::read(p).keywords;
        assert_eq!(
            [keywords(&b), keywords(&a)],
            [["Oslo"], ["Bergen"]],
            "a keyword is on the other photo"
        );
        std::fs::remove_dir_all(d).unwrap();
    }

    /// Swaps the files `a` and `b` in another thread, as another window's Rename Many does:
    /// it holds the locks of their sidecars from before the swap until after it. Returns the
    /// thread once the locks are held, so that what the test starts next waits for them.
    fn swap_in_another_window(
        a: &std::path::Path,
        b: &std::path::Path,
    ) -> std::thread::JoinHandle<()> {
        let (a, b) = (a.to_path_buf(), b.to_path_buf());
        let (held, locks_held) = std::sync::mpsc::channel();
        let other = std::thread::spawn(move || {
            let locks = crate::metadata::xmp::lock_sidecars(&[a.clone(), b.clone()]);
            held.send(()).unwrap();
            // Long enough for a check made before the lock to be made before the swap.
            std::thread::sleep(std::time::Duration::from_millis(300));
            let between = a.with_extension("between");
            std::fs::rename(&a, &between).unwrap();
            std::fs::rename(&b, &a).unwrap();
            std::fs::rename(&between, &b).unwrap();
            drop(locks);
        });
        locks_held.recv().unwrap();
        other
    }

    /// A keyword for a photo whose name another window's Rename Many gives to another photo
    /// while the keyword waits for the sidecar's lock goes on neither photo. Before, the check
    /// was made before the lock, passed, and the keyword went on the other photo.
    #[test]
    fn a_keyword_waiting_for_another_windows_swap_is_checked_after_it() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-swap-keyword-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a.png"), dir.join("b.png"));
        image::RgbImage::new(2, 2).save(&a).unwrap();
        image::RgbImage::new(3, 3).save(&b).unwrap();
        let (mut app, ctx) = app_on(&dir);
        let identity = crate::files::file_io::identity_of(&a);
        let listed =
            crate::files::file_io::ListedFile::new(a.clone(), identity, &Default::default());
        let other = swap_in_another_window(&a, &b);
        app.write_sidecars(
            &ctx,
            "Keywords",
            vec![(listed, ())],
            |p, _| crate::metadata::xmp::add_tags(p, &["Oslo".into()]),
            |_, _| None,
        );
        other.join().unwrap();
        assert!(run_background_until(&mut app, &ctx, |app| !app
            .file_operations
            .is_writing_sidecars()));
        let keywords = |p: &std::path::Path| crate::metadata::xmp::read(p).keywords;
        assert!(keywords(&a).is_empty(), "the keyword is on the other photo");
        assert!(keywords(&b).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A rename of a photo whose name another window's Rename Many gives to another photo while
    /// the rename waits for the sidecars' locks leaves the other photo alone. Before, the check
    /// was made before the lock, passed, and the other photo was renamed.
    #[test]
    fn a_rename_waiting_for_another_windows_swap_is_checked_after_it() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-swap-rename-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b, c) = (dir.join("a.png"), dir.join("b.png"), dir.join("c.png"));
        image::RgbImage::new(2, 2).save(&a).unwrap();
        image::RgbImage::new(3, 3).save(&b).unwrap();
        let (mut app, ctx) = app_on(&dir);
        let picked = crate::files::file_io::PickedFiles::new(vec![a.clone()]);
        let identity = crate::files::file_io::identity_of(&a);
        let listed =
            crate::files::file_io::ListedFile::new(a.clone(), identity, &Default::default());
        let other = swap_in_another_window(&a, &b);
        app.rename_in_background(&ctx, vec![listed], vec![c.clone()], picked);
        other.join().unwrap();
        assert!(run_background_until(&mut app, &ctx, |app| app
            .file_operations
            .renaming
            .is_none()));
        assert!(!c.exists(), "the other photo was renamed");
        assert_eq!(image::image_dimensions(&a).unwrap(), (3, 3));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A listing read while Rename Many renames files of the folder waits for the rename's
    /// report, and the list and the shown image stay as they are until then. After it, the
    /// renamed image is shown under its new name. Before, a listing that had the new name and
    /// not the old one moved the view to the next file, which the next key acted on. The
    /// release tests saw it on an sftp:// folder, whose watcher saw the rename before the
    /// rename's report came.
    #[test]
    fn a_listing_during_a_rename_waits_for_its_report() {
        let d = contract_folder("listing-during-rename");
        let (a, r) = (d.join("a.png"), d.join("r.png"));
        let (mut app, ctx) = app_on(&d);
        assert!(run_background_until(&mut app, &ctx, |app| !app.working()));
        assert!(app.is_file(app.list.shown(), &a));
        let before = app.list.files().to_vec();
        app.file_operations.renaming = Some(super::RenameJob {
            folder: d.clone(),
            from: vec![a.clone()],
            to: vec![r.clone()],
            shown: Some(0),
            undoing: false,
        });
        std::fs::rename(&a, &r).unwrap();
        app.start_listing(&ctx);
        // The listing thread is done long before this, and its answer waits.
        std::thread::sleep(std::time::Duration::from_millis(300));
        app.receive_listing(&ctx, std::time::Duration::from_millis(500));
        let shown = |app: &crate::App| app.list.files().get(app.list.shown()).cloned();
        assert_eq!(
            app.list.files(),
            &before[..],
            "the list changed during the rename"
        );
        assert!(app.is_file(app.list.shown(), &a), "{:?}", shown(&app));
        let job = app.file_operations.renaming.take().unwrap();
        app.finish_rename_many(&ctx, job, renamed_whole());
        assert!(run_background_until(&mut app, &ctx, |app| !app.working()));
        assert!(app.is_file(app.list.shown(), &r), "{:?}", shown(&app));
        std::fs::remove_dir_all(d).unwrap();
    }

    /// A listing waits while Rename Many renames files that it would show, whichever of them
    /// comes first: in several folders shown together (`/`), in a folder that holds the
    /// renamed ones, and in a subfolder of their folder. A folder apart from them is listed.
    /// Before, only the folder of the first file renamed held a listing back, so another of
    /// the folders moved its shown image to another file in the middle of the rename.
    #[test]
    fn a_rename_holds_back_the_listing_of_every_folder_it_renames_in() {
        let d = contract_folder("rename-in-folders");
        let (mut app, _ctx) = app_on(&d);
        let (p, q) = (d.join("p"), d.join("q"));
        let from = vec![p.join("a.png"), q.join("s").join("b.png")];
        app.file_operations.renaming = Some(super::RenameJob {
            folder: super::folder_of_all(&from),
            to: vec![p.join("x.png"), q.join("s").join("y.png")],
            from,
            shown: None,
            undoing: false,
        });
        let holds = |dir: &std::path::Path| app.file_operations.renames_files_in(dir);
        for dir in [&std::path::PathBuf::from("/"), &d, &q, &q.join("s")] {
            assert!(holds(dir), "{dir:?} was listed in the middle of the rename");
        }
        assert!(
            !holds(&d.with_extension("apart")),
            "a folder apart was held"
        );
        assert_eq!(super::folder_of_all(&[p.join("a"), p.join("b")]), p);
        std::fs::remove_dir_all(d).unwrap();
    }

    /// The shown image renamed by Rename Many stays shown under its new name when the list
    /// changed while the files were renamed. Before, a list changed meanwhile was only read
    /// again, without the old name, and the view moved to the next file.
    #[test]
    fn a_list_changed_during_a_rename_keeps_the_renamed_image_shown() {
        let d = contract_folder("changed-during-rename");
        let (a, r) = (d.join("a.png"), d.join("r.png"));
        let (mut app, ctx) = app_on(&d);
        assert!(run_background_until(&mut app, &ctx, |app| !app.working()));
        assert!(app.is_file(app.list.shown(), &a));
        let job = super::RenameJob {
            folder: d.clone(),
            from: vec![a.clone()],
            to: vec![r.clone()],
            shown: Some(0),
            undoing: false,
        };
        // The list changes while the file is renamed, as a refresh would change it.
        app.list.note_names_changed();
        std::fs::rename(&a, &r).unwrap();
        app.finish_rename_many(&ctx, job, renamed_whole());
        assert!(run_background_until(&mut app, &ctx, |app| !app.working()));
        let shown = app.list.files().get(app.list.shown()).cloned();
        assert!(app.is_file(app.list.shown(), &r), "{shown:?}");
        std::fs::remove_dir_all(d).unwrap();
    }

    /// The report of a Rename Many that renamed every file and left an Undo of nothing.
    fn renamed_whole() -> super::RenameOutcome {
        super::RenameOutcome {
            result: Ok(()),
            left_alone: Vec::new(),
            undo: Vec::new(),
            names_known: true,
            stopped: false,
        }
    }

    /// A sidecar write that waited for a Rename Many that failed, and could not put every old
    /// name back, is dropped, and the user is told with the rename's error. Before, it was
    /// written to the old name, which may hold the other photo of a swap by then.
    #[test]
    fn a_sidecar_write_is_dropped_when_a_failed_rename_leaves_names_unknown() {
        let d = contract_folder("write-after-stuck-rename");
        let a = d.join("a.png");
        std::fs::remove_file(crate::metadata::xmp::sidecar(&a)).unwrap();
        let (mut app, ctx) = app_on(&d);
        let job = super::RenameJob {
            folder: d.clone(),
            from: vec![a.clone()],
            to: vec![d.join("b.png")],
            shown: None,
            undoing: false,
        };
        app.file_operations.renaming = Some(job);
        let oslo = |p: &std::path::Path| crate::metadata::xmp::add_tags(p, &["Oslo".into()]);
        let asked = vec![(a.clone(), ())];
        app.write_sidecars(
            &ctx,
            "Keywords",
            crate::support::test_support::unlisted_with(asked),
            move |p, _| oslo(p),
            |_, _| None,
        );
        let job = app.file_operations.renaming.take().unwrap();
        let outcome = super::RenameOutcome {
            result: Err("not all names could be put back".into()),
            left_alone: Vec::new(),
            undo: Vec::new(),
            names_known: false,
            stopped: false,
        };
        app.finish_rename_many(&ctx, job, outcome);
        assert!(run_background_until(&mut app, &ctx, |app| !app
            .file_operations
            .is_writing_sidecars()
            && !app.working()));
        assert!(crate::metadata::xmp::read(&a).keywords.is_empty());
        let notice = app.notice.text().unwrap_or_default();
        assert!(
            notice.contains("put back") && notice.contains("not saved"),
            "{notice}"
        );
        std::fs::remove_dir_all(d).unwrap();
    }

    /// Stop, or a second close, leaves a sidecar write of one file, such as the info panel's
    /// save waiting behind a long job. Before, the save was dropped without a word, while the
    /// panel showed the edit as saved.
    #[test]
    fn stop_leaves_a_sidecar_write_of_one_file() {
        let d = contract_folder("stop-one-sidecar");
        let a = d.join("a.png");
        // The fixture's sidecar only says whose it is, which is not XMP to write into.
        std::fs::remove_file(crate::metadata::xmp::sidecar(&a)).unwrap();
        let (mut app, ctx) = app_on(&d);
        let (go, released) = std::sync::mpsc::channel::<()>();
        let long = vec![(d.join("sub"), ()), (d.join("b.png"), ())];
        app.write_sidecars(
            &ctx,
            "Other",
            crate::support::test_support::unlisted_with(long),
            move |_, _| released.recv().map_or(Ok(()), |()| Err(String::new())),
            |_, _| None,
        );
        let oslo = |p: &std::path::Path| crate::metadata::xmp::add_tags(p, &["Oslo".into()]);
        app.write_sidecars(
            &ctx,
            "Keywords",
            crate::support::test_support::unlisted_with(vec![(a.clone(), ())]),
            move |p, _| oslo(p),
            |_, _| None,
        );
        app.file_operations.stop_jobs();
        drop(go);
        assert!(run_background_until(&mut app, &ctx, |app| !app
            .file_operations
            .is_writing_sidecars()));
        assert_eq!(crate::metadata::xmp::read(&a).keywords, ["Oslo"]);
        std::fs::remove_dir_all(d).unwrap();
    }

    /// A move, a rename and a delete wait for a sidecar write of their files that holds the
    /// sidecar's lock, so they never take the photo away while the write is on its way. A write
    /// asked for while a rename runs is `a_sidecar_write_during_a_rename_follows_its_photo`.
    #[test]
    fn file_operations_wait_for_a_sidecar_write() {
        let d = contract_folder("held");
        let (mut app, ctx) = app_on(&d);
        let idle = |app: &crate::App| {
            (app.file_operations.jobs.iter()).all(|j| std::sync::Arc::strong_count(j) == 1)
        };
        let waits =
            |app: &mut crate::App, file: &std::path::Path, start: &dyn Fn(&mut crate::App)| {
                let lock = crate::metadata::xmp::lock_sidecar(file);
                start(app);
                std::thread::sleep(std::time::Duration::from_millis(300));
                assert!(file.exists(), "{} gone while held", file.display());
                drop(lock);
                assert!(run_background_until(app, &ctx, |app| idle(app) && !file.exists()));
            };
        let (a, b) = (d.join("a.png"), d.join("b.png"));
        waits(&mut app, &a, &|app| {
            app.transfer(
                crate::files::file_io::PickedFiles::new(vec![a.clone()]),
                d.join("sub"),
                false,
            )
        });
        assert!(d.join("sub/a.png").exists() && d.join("sub/a.png.xmp").exists());
        let c = d.join("c.png");
        waits(&mut app, &b, &|app| {
            app.rename_in_background(
                &ctx,
                crate::support::test_support::unlisted(vec![b.clone()]),
                vec![c.clone()],
                crate::files::file_io::PickedFiles::new(vec![b.clone()]),
            )
        });
        assert!(c.exists() && d.join("c.png.xmp").exists());
        assert!(run_background_until(&mut app, &ctx, |app| !app
            .folder
            .is_listing()));
        waits(&mut app, &c, &|app| {
            let at = (app.list.files().iter())
                .position(|f| f == "c.png")
                .unwrap();
            app.delete_targets(&ctx, false, vec![at], None);
        });
        assert!(!d.join("c.png.xmp").exists());
        // With many files, they are held a chunk at a time, so a file held up by a sidecar
        // write holds up only its own chunk, and those before it go meanwhile. Here they are
        // deleted for good. A chunk takes only a few of the sidecar locks, so a chunk that waits
        // holds up few sidecar writes elsewhere. (There is not one gio call and one taking of
        // the locks per file.)
        let lock_number = |p: &PathBuf| {
            crate::metadata::xmp::sidecar_lock_number(&crate::metadata::xmp::sidecar(p))
        };
        let names: Vec<String> = (0..400).map(|k| format!("d{k:03}.png")).collect();
        for n in &names {
            std::fs::write(d.join(n), "").unwrap();
        }
        let paths: Vec<PathBuf> = names.iter().map(|n| d.join(n)).collect();
        let chunks = crate::metadata::xmp::chunks_by_sidecar_lock(&paths, 100);
        let held_up = chunks.last().unwrap().clone();
        let y = held_up[0].clone();
        let locks: std::collections::HashSet<u64> = held_up.iter().map(lock_number).collect();
        assert!(locks.len() <= 4, "{} locks held by one chunk", locks.len());
        let x = paths.iter().find(|p| !held_up.contains(p)).unwrap().clone();
        for p in [&x, &y] {
            std::fs::write(crate::metadata::xmp::sidecar(p), "keywords").unwrap();
        }
        app.start_listing(&ctx);
        assert!(run_background_until(&mut app, &ctx, |app| !app
            .folder
            .is_listing()));
        let all: Vec<usize> = (app.list.files().iter().enumerate())
            .filter(|(_, f)| names.iter().any(|n| f.as_os_str() == n.as_str()))
            .map(|(k, _)| k)
            .collect();
        assert_eq!(all.len(), 400);
        let lock = crate::metadata::xmp::lock_sidecar(&y);
        app.delete_targets(&ctx, false, all, None);
        let others_gone = |_: &crate::App| paths.iter().all(|p| held_up.contains(p) || !p.exists());
        assert!(
            run_background_until(&mut app, &ctx, others_gone),
            "the chunks before wait for the one held up"
        );
        assert!(held_up.iter().all(|p| p.exists()));
        assert!(crate::metadata::xmp::sidecar(&y).exists());
        drop(lock);
        assert!(run_background_until(&mut app, &ctx, |app| idle(app)
            && paths.iter().all(|p| !p.exists())));
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
        let shown = |a: &crate::App| a.list.loaded_images().contains_key(&a.list.shown());
        assert!(run_background_until(&mut app, &ctx, |a| !a.working() && shown(a)));
        let path = app.path(app.list.shown());
        app.forget_names(std::slice::from_ref(&path));
        assert!(!shown(&app));
        app.start_listing(&ctx);
        assert!(run_background_until(&mut app, &ctx, |a| !a.working() && shown(a)));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A file moved, renamed or deleted takes what Glimt keeps of it by name along, so a file of
    /// that name later is read for itself, not shown with the old one's size or Check Images'
    /// mark.
    #[test]
    fn a_name_moved_or_gone_forgets_what_was_kept_of_it() {
        let dir = contract_folder("forget");
        let (mut app, ctx) = app_on(&dir);
        assert!(run_background_until(&mut app, &ctx, |a| !a.working()));
        for name in ["a.png", "b.png", "c.png"] {
            app.list.remember_size_and_date(name.into(), (1, 1));
            app.broken_files.insert(name.into());
        }
        app.follow_moved_paths(vec![(dir.join("a.png"), dir.join("c.png"))], false);
        app.forget_names(&[dir.join("b.png")]);
        for name in ["a.png", "b.png", "c.png"] {
            let name = std::ffi::OsStr::new(name);
            assert!(!app.list.sizes_and_dates().contains_key(name), "{name:?}");
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
        assert_eq!(
            super::name_in_the_way(&to, &Default::default(), &mut Default::default()),
            Some(to.clone())
        );
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
        assert!(run_background_until(&mut app, &ctx, |a| !a
            .folder
            .is_listing()));
        app.notice.clear();
        app.delete_targets(&ctx, false, vec![0], None);
        let told = |app: &crate::App| (app.notice.text()).is_some_and(|n| n.contains("a.png.xmp"));
        assert!(run_background_until(&mut app, &ctx, told));
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
        app.delete_targets(&ctx, false, vec![0], None);
        let gone = |p: &std::path::Path| std::fs::symlink_metadata(p).is_err();
        assert!(run_background_until(&mut app, &ctx, |_| gone(&a) && gone(&side)));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A file written after a big move looks up the real paths only of the moved files that
    /// share its name, in the window's thread. Before, one written file with a moved file's name
    /// looked up the real path of every moved file, 500,000 of them after a move of a big folder.
    /// The moved file it is, reached through a link, still takes its new fingerprint.
    #[test]
    fn a_file_written_after_a_big_move_looks_up_only_its_namesakes() {
        let dir = std::env::temp_dir().join(format!("glimt-test-namesakes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("moved")).unwrap();
        std::fs::write(dir.join("moved/1.png"), "turned").unwrap();
        std::os::unix::fs::symlink(dir.join("moved"), dir.join("link")).unwrap();
        let (mut app, _ctx) = app_on(&dir);
        let none = (None, 0, (0, 0));
        let pairs = (0..10_000).map(|n| {
            let name = format!("{n}.png");
            (
                dir.join("old").join(&name),
                dir.join("moved").join(&name),
                none,
            )
        });
        app.file_operations.undo = Some(super::Undo::Move(pairs.collect()));
        super::REAL_PATHS_LOOKED_UP.set(0);
        app.written_anew(&[dir.join("link/1.png")]);
        assert_eq!(super::REAL_PATHS_LOOKED_UP.get(), 2);
        let Some(super::Undo::Move(pairs)) = &app.file_operations.undo else {
            panic!("the Undo is gone");
        };
        assert_eq!(pairs.len(), 10_000);
        assert!(pairs[1].2 != none && pairs[2].2 == none);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A photo whose look fails after its delete (a share that dropped for a moment) is not taken
    /// for gone, so its sidecar stays, as the photo may still be there. Before, every failed look
    /// counted as gone, and the sidecar, with the photo's keywords and stars, was deleted for good.
    #[test]
    fn a_delete_keeps_the_sidecar_of_a_photo_it_could_not_look_at() {
        use crate::files::file_io::LOOKS_FAIL;
        use crate::support::threads::lock_even_if_poisoned;
        let dir = std::env::temp_dir().join(format!("glimt-test-unseen-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.png");
        image::RgbImage::new(4, 4).save(&a).unwrap();
        let side = crate::metadata::xmp::sidecar(&a);
        std::fs::write(&side, "a's").unwrap();
        let (mut app, ctx) = app_on(&dir);
        assert!(run_background_until(&mut app, &ctx, |a| !a
            .folder
            .is_listing()));
        lock_even_if_poisoned(&LOOKS_FAIL).push(a.clone());
        app.delete_targets(&ctx, false, vec![0], None);
        let done = run_background_until(&mut app, &ctx, |app| !app.working());
        lock_even_if_poisoned(&LOOKS_FAIL).retain(|p| *p != a);
        assert!(done);
        assert_eq!(std::fs::read_to_string(&side).unwrap(), "a's");
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
        app.viewing.set_turn((1, false), a.clone());
        let held = app
            .start_job_of("Moving", 1, false, vec![a.clone()])
            .unwrap();
        app.notice.clear();
        app.save_orientation(&ctx);
        assert!(
            app.file_operations.saving.is_empty() && app.notice.text().is_some(),
            "saved while held"
        );
        drop(held);
        // A Rename Many of other files holds the turn back too, as it may give the file's name
        // to another photo.
        let (b, c) = (dir.join("b.png"), dir.join("c.png"));
        image::RgbImage::new(4, 4).save(&b).unwrap();
        app.rename_in_background(
            &ctx,
            crate::support::test_support::unlisted(vec![b.clone()]),
            vec![c],
            crate::files::file_io::PickedFiles::new(vec![b]),
        );
        app.notice.clear();
        app.save_orientation(&ctx);
        assert!(
            app.file_operations.saving.is_empty() && app.notice.text().is_some(),
            "saved while renaming"
        );
        assert!(run_background_until(&mut app, &ctx, |app| app
            .file_operations
            .renaming
            .is_none()));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// New Folder makes the folder in the background, and the list and the tree show it.
    #[test]
    fn makes_a_new_folder_in_the_background() {
        let d = contract_folder("new-folder");
        let (mut app, ctx) = app_on(&d);
        assert!(run_background_until(&mut app, &ctx, |a| !a
            .folder
            .is_listing()));
        app.run(&ctx, crate::ui::actions::Action::NewFolder);
        // Another folder opened while the dialog is open, by a script, say, does not change
        // where the new folder is made.
        let other = d.join("other");
        std::fs::create_dir_all(&other).unwrap();
        crate::support::test_support::open_now(&mut app, &ctx, other.clone());
        app.act_on_prompt_answer(&ctx, crate::ui::dialogs::Prompt::NewFolder, "trip");
        crate::support::test_support::open_now(&mut app, &ctx, d.clone());
        assert!(run_background_until(&mut app, &ctx, |a| {
            !a.folder.is_listing() && a.folder.subdirs().iter().any(|s| s.ends_with("trip"))
        }));
        assert!(d.join("trip").is_dir() && !other.join("trip").exists());
        // When the folder is there already, a notice says so, and nothing else is made.
        app.run(&ctx, crate::ui::actions::Action::NewFolder);
        app.act_on_prompt_answer(&ctx, crate::ui::dialogs::Prompt::NewFolder, "trip");
        assert!(run_background_until(&mut app, &ctx, |a| a
            .notice
            .text()
            .is_some()));
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
            crate::support::test_support::unlisted_with(vec![(dir.join("a.png"), ())]),
            |_, _| Ok(()),
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
            let now = app.are_free_to_change(&[]);
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
        app.replace_files(
            &ctx,
            vec![("b.png".into(), None), ("c.png".into(), None)],
            None,
        );
        assert!(app.search.reads_exif_again("c.png"));
        // A file written over by another image is read again, and its stars are gone.
        let b = dir.join("b.png");
        #[cfg(feature = "marks")]
        app.marks.rate(
            &crate::support::test_support::unlisted(std::slice::from_ref(&b)),
            3,
        );
        let over = super::OperationReport {
            overwritten: vec![b.clone()],
            ..Default::default()
        };
        app.file_operations.report_tx.send(over).unwrap();
        app.receive_operation_report(&ctx);
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
        let idle = |app: &crate::App| {
            (app.file_operations.jobs.iter()).all(|j| std::sync::Arc::strong_count(j) == 1)
        };
        app.rename_in_background(
            &ctx,
            crate::support::test_support::unlisted(vec![d.join("a.png")]),
            vec![d.join("c.png")],
            crate::files::file_io::PickedFiles::new(vec![d.join("a.png")]),
        );
        assert!(run_background_until(&mut app, &ctx, |app| idle(app)
            && entries(&gqv).contains("c.png")
            && !entries(&gqv).contains("a.png")));
        app.transfer(
            crate::files::file_io::PickedFiles::new(vec![d.join("c.png")]),
            d.join("sub"),
            false,
        );
        assert!(run_background_until(&mut app, &ctx, |app| idle(app)
            && entries(&gqv).contains("sub/c.png")));
        // When a folder is renamed in the background, its images in the collection follow.
        app.dialogs.act_on_folder(d.join("sub"));
        app.rename_folder(&ctx, "moved");
        assert!(run_background_until(&mut app, &ctx, |app| idle(app)
            && entries(&gqv).contains("moved/c.png")));
        assert!(d.join("moved/c.png").exists());
        let b = app
            .list
            .files()
            .iter()
            .position(|f| std::path::Path::new(f).ends_with("b.png"))
            .unwrap();
        app.delete_targets(&ctx, false, vec![b], None);
        assert!(run_background_until(&mut app, &ctx, |_| !entries(&gqv).contains("b.png")));
        std::fs::remove_dir_all(d).unwrap();
    }

    /// A saved collection that is not open follows a rename too, as in Geeqie, so its entry
    /// stays on its image and not on the name. The open one is changed once, and not again
    /// with the others, because a second pass would undo a swap.
    #[test]
    fn saved_collections_follow_their_files() {
        let d = contract_folder("saved-collections");
        let dir = crate::model::collections::collections_dir().unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        let saved = dir.join(format!("follow-{}.gqv", std::process::id()));
        let entries = || -> Vec<String> {
            let text = std::fs::read_to_string(&saved).unwrap();
            (text.lines().filter(|l| !l.starts_with('#')))
                .map(|l| l.trim_matches('"').rsplit('/').next().unwrap().to_owned())
                .collect()
        };
        std::fs::copy(d.join("col.gqv"), &saved).unwrap();
        let (a, b) = (d.join("a.png"), d.join("b.png"));
        let sidecar_of_a = || std::fs::read_to_string(crate::metadata::xmp::sidecar(&a));
        for opened in [d.clone(), saved.clone()] {
            let (mut app, ctx) = app_on(&opened);
            let before = sidecar_of_a().unwrap();
            let idle = |app: &crate::App| {
                (app.file_operations.jobs.iter()).all(|j| std::sync::Arc::strong_count(j) == 1)
            };
            // The two swap names, so the entry of each takes the other's name.
            app.rename_in_background(
                &ctx,
                crate::support::test_support::unlisted(vec![a.clone(), b.clone()]),
                vec![b.clone(), a.clone()],
                crate::files::file_io::PickedFiles::new(vec![a.clone(), b.clone()]),
            );
            assert!(run_background_until(&mut app, &ctx, |app| idle(app)
                && sidecar_of_a().is_ok_and(|s| s != before)));
            crate::files::file_io::flush();
            let swapped = match before.as_str() {
                "a" => ["b.png", "a.png"],
                _ => ["a.png", "b.png"],
            };
            assert_eq!(entries(), swapped, "opened {}", opened.display());
        }
        std::fs::remove_file(&saved).unwrap();
        std::fs::remove_dir_all(d).unwrap();
    }

    /// A collection's entries also follow Rename Many, a move to the trash and the Undo of
    /// each, and the Undo of a move. Each Undo puts the entry back in its place.
    #[test]
    fn a_collection_follows_undo_and_the_trash() {
        let d = contract_folder("collection-undo");
        let gqv = d.join("col.gqv");
        let (mut app, ctx) = app_on(&gqv);
        // The entries as paths inside the folder, in the collection's order.
        let entries = |_: &crate::App| -> Vec<String> {
            let text = std::fs::read_to_string(&gqv).unwrap();
            (text.lines().filter(|l| !l.starts_with('#')))
                .map(|l| {
                    let p = std::path::Path::new(l.trim_matches('"'));
                    p.strip_prefix(&d).unwrap_or(p).display().to_string()
                })
                .collect()
        };
        let idle = |app: &crate::App| {
            (app.file_operations.jobs.iter()).all(|j| std::sync::Arc::strong_count(j) == 1)
                && (app.file_operations.held_back_files.iter())
                    .all(|t| std::sync::Arc::strong_count(t) == 1)
        };
        let has = |app: &crate::App, want: &[&str]| idle(app) && entries(app) == want;
        app.list.select([0, 1].into_iter().collect());
        let picked = vec![d.join("a.png"), d.join("b.png")];
        app.rename_many(&ctx, "x#", crate::files::file_io::PickedFiles::new(picked));
        assert!(run_background_until(&mut app, &ctx, |app| has(
            app,
            &["x1.png", "x2.png"]
        )));
        app.undo(&ctx);
        assert!(run_background_until(&mut app, &ctx, |app| has(
            app,
            &["a.png", "b.png"]
        )));
        app.transfer(
            crate::files::file_io::PickedFiles::new(vec![d.join("a.png")]),
            d.join("sub"),
            false,
        );
        assert!(run_background_until(&mut app, &ctx, |app| has(
            app,
            &["sub/a.png", "b.png"]
        ) && app
            .file_operations
            .undo
            .is_some()));
        app.undo(&ctx);
        // The list is read again after the Undo, so the file is looked for once it is in it.
        let at = |app: &crate::App| {
            (0..app.list.files().len()).find(|&i| app.path(i) == d.join("a.png"))
        };
        assert!(run_background_until(&mut app, &ctx, |app| has(
            app,
            &["a.png", "b.png"]
        ) && at(app)
            .is_some()));
        let a = at(&app).unwrap();
        app.delete_targets(&ctx, true, vec![a], None);
        assert!(run_background_until(&mut app, &ctx, |app| has(
            app,
            &["b.png"]
        ) && app
            .file_operations
            .undo
            .is_some()));
        app.undo(&ctx);
        assert!(run_background_until(&mut app, &ctx, |app| has(
            app,
            &["a.png", "b.png"]
        )));
        // A folder moved to the trash takes the entries of its files along, and its Undo puts
        // them back in their places.
        app.transfer(
            crate::files::file_io::PickedFiles::new(vec![d.join("a.png")]),
            d.join("sub"),
            false,
        );
        assert!(run_background_until(&mut app, &ctx, |app| has(
            app,
            &["sub/a.png", "b.png"]
        )));
        app.dialogs.act_on_folder(d.join("sub"));
        app.trash_folder(&ctx);
        assert!(run_background_until(&mut app, &ctx, |app| has(
            app,
            &["b.png"]
        ) && matches!(
            app.file_operations.undo,
            Some(super::Undo::Trash(_))
        )));
        app.undo(&ctx);
        assert!(run_background_until(&mut app, &ctx, |app| has(
            app,
            &["sub/a.png", "b.png"]
        )));
        empty_trash_of(&d);
        std::fs::remove_dir_all(d).unwrap();
    }

    /// Files held back by `hold_back_while_writing`, as a tool running on them is, hold back
    /// every other operation on them until they are let go.
    #[test]
    fn held_back_files_hold_back_other_operations() {
        let dir = std::env::temp_dir().join(format!("glimt-test-hold-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.png");
        image::RgbImage::new(2, 2).save(&a).unwrap();
        let (mut app, _ctx) = crate::support::test_support::app_on(&dir);
        let held_back = app.hold_back_while_writing(vec![a.clone()]).unwrap();
        assert!(!app.are_free_to_change(std::slice::from_ref(&a)));
        assert!(app.hold_back_while_writing(vec![a.clone()]).is_none());
        drop(held_back);
        assert!(app.are_free_to_change(std::slice::from_ref(&a)));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A tool's thread holds only the files the tool takes, once it has sorted them, so a tool
    /// for RAW files left open does not hold back the JPEGs selected beside them.
    #[test]
    fn held_back_files_let_go_of_what_the_work_does_not_change() {
        let dir = std::env::temp_dir().join(format!("glimt-test-hold-only-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (raw, jpeg) = (dir.join("a.cr2"), dir.join("a.jpg"));
        let (mut app, _ctx) = crate::support::test_support::app_on(&dir);
        let held_back = (app.hold_back_while_writing(vec![raw.clone(), jpeg.clone()])).unwrap();
        held_back.hold_only(std::slice::from_ref(&raw));
        assert!(app.are_free_to_change(std::slice::from_ref(&jpeg)));
        assert!(!app.are_free_to_change(std::slice::from_ref(&raw)));
        drop(held_back);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Returns a folder of its own for a test named `name`, with the 4×4 PNGs `files` in it.
    fn folder_with(name: &str, files: &[&str]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("glimt-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        for file in files {
            image::RgbImage::new(4, 4).save(dir.join(file)).unwrap();
        }
        dir
    }

    /// Returns what the background work told the window within five seconds.
    fn told_to(app: &crate::App) -> Option<Told> {
        app.notices
            .recv_timeout(std::time::Duration::from_secs(5))
            .ok()
    }

    /// Work in the background that panics is told as a failed write, so a close that waited
    /// for it stays open. Before, the thread ended with nothing told.
    #[test]
    fn a_panic_in_background_work_is_told_as_a_failed_write() {
        let dir = folder_with("panic-told", &[]);
        let (mut app, ctx) = app_on(&dir);
        assert!(app.run_in_background(&ctx, Vec::new(), || panic!("a broken file")));
        assert!(matches!(told_to(&app), Some(Told::FailedWrite(_))));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// When the thread of a Rename Many ends without an answer (a file broke it), the folder is
    /// read again, as the rename's report would have had it read. Before, nothing read it, and
    /// a listing held back while the rename ran, read in the middle of it, stood for the list.
    #[test]
    fn a_rename_that_breaks_reads_the_folder_again() {
        let dir = folder_with("rename-thread-ends", &["a.png", "b.png"]);
        let a = dir.join("a.png");
        let (mut app, ctx) = app_on(&dir);
        assert!(run_background_until(&mut app, &ctx, |app| app
            .list
            .files()
            .len()
            == 2
            && !app.working()));
        lock_even_if_poisoned(&crate::support::threads::BREAKING_FILES).push(a.clone());
        let read_before = app.folder.whole_listings();
        let listed = app.listed_file(0);
        let picked = crate::files::file_io::PickedFiles::listed(vec![listed.clone()]);
        app.rename_in_background(&ctx, vec![listed], vec![dir.join("r.png")], picked);
        assert!(run_background_until(&mut app, &ctx, |app| app
            .file_operations
            .renaming
            .is_none()
            && !app.working()));
        // The control: the rename broke, so no report read the folder again.
        assert!(
            a.exists() && !dir.join("r.png").exists(),
            "the rename was not broken"
        );
        assert!(
            app.folder.whole_listings() > read_before,
            "the folder was not read again"
        );
        lock_even_if_poisoned(&crate::support::threads::BREAKING_FILES).retain(|p| *p != a);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A file that breaks a delete is told as a failed write, and the report still comes, so
    /// the list and the marks follow what was done. Before, the thread ended with nothing told
    /// and no report.
    #[test]
    fn a_file_that_breaks_a_delete_is_told_and_reported() {
        let dir = folder_with("delete-breaks", &["a.png", "b.png"]);
        let a = dir.join("a.png");
        lock_even_if_poisoned(&crate::support::threads::BREAKING_FILES).push(a.clone());
        let (mut app, ctx) = app_on(&dir);
        app.delete_targets(&ctx, false, vec![0, 1], None);
        assert!(matches!(told_to(&app), Some(Told::FailedWrite(_))));
        assert!(run_background_until(&mut app, &ctx, |app| app
            .file_operations
            .deleting
            .is_empty()));
        assert!(a.exists());
        lock_even_if_poisoned(&crate::support::threads::BREAKING_FILES).retain(|p| *p != a);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A file that breaks a move costs only that file: the files after it are moved, told as a
    /// failed write, and the report leaves the Undo of those moved. Before, the thread ended at
    /// that file with nothing told, and the files moved before it had no Undo and their marks
    /// stayed under their old names.
    #[test]
    fn a_file_that_breaks_a_move_costs_only_that_file() {
        let dir = folder_with("move-breaks", &["a.png", "b.png"]);
        let (a, b, sub) = (dir.join("a.png"), dir.join("b.png"), dir.join("sub"));
        lock_even_if_poisoned(&crate::support::threads::BREAKING_FILES).push(a.clone());
        let (mut app, ctx) = app_on(&dir);
        let paths = vec![a.clone(), b.clone()];
        app.transfer_into_real_folder(
            &ctx,
            crate::files::file_io::PickedFiles::new(paths),
            (sub.clone(), None),
            false,
        );
        assert!(matches!(told_to(&app), Some(Told::FailedWrite(_))));
        assert!(a.exists() && sub.join("b.png").exists() && !b.exists());
        assert!(run_background_until(&mut app, &ctx, |app| app
            .file_operations
            .undo
            .is_some()));
        lock_even_if_poisoned(&crate::support::threads::BREAKING_FILES).retain(|p| *p != a);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A sidecar whose writing panics counts as one that failed, and the files after it are
    /// still written. Before, the whole job stopped at that file.
    #[test]
    fn a_sidecar_that_breaks_the_writer_costs_only_that_file() {
        let dir = folder_with("sidecar-breaks", &["a.png", "b.png"]);
        let (a, b) = (dir.join("a.png"), dir.join("b.png"));
        let (mut app, ctx) = app_on(&dir);
        let written = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let noted = written.clone();
        let files = vec![(a.clone(), ()), (b.clone(), ())];
        let write = move |path: &std::path::Path, _: &()| {
            assert!(!path.ends_with("a.png"), "a broken sidecar");
            lock_even_if_poisoned(&noted).push(path.to_path_buf());
            Ok(())
        };
        let report =
            |failed: usize, _: &super::Job| (failed > 0).then(|| format!("{failed} failed"));
        app.write_sidecars(
            &ctx,
            "Writing",
            crate::support::test_support::unlisted_with(files),
            write,
            report,
        );
        assert_eq!(told_to(&app), Some(Told::FailedWrite("1 failed".into())));
        assert_eq!(*lock_even_if_poisoned(&written), [b]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A sidecar write that fails, in a job whose report has no words for it (the info
    /// panel's), is told as a failed write by its own text. Before, each such write told its
    /// failure itself through a notice sender of its own, which nothing held it to.
    #[test]
    fn a_sidecar_write_that_fails_is_told_by_its_own_text() {
        let dir = folder_with("sidecar-fails", &["a.png"]);
        let (mut app, ctx) = app_on(&dir);
        app.write_sidecars(
            &ctx,
            "Writing",
            crate::support::test_support::unlisted_with(vec![(dir.join("a.png"), ())]),
            |_, _| Err("Could not save the keywords: full".into()),
            |_, _| None,
        );
        assert_eq!(
            told_to(&app),
            Some(Told::FailedWrite(
                "Could not save the keywords: full".into()
            ))
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A Rename Many whose thread breaks is told as a failed write, as the names may be half
    /// done, and Rename Many is not refused afterwards. Before, it was a plain notice, which a
    /// close that waited for the renames closed over.
    #[test]
    fn a_rename_many_that_breaks_is_told_as_a_failed_write() {
        let dir = folder_with("rename-breaks", &["a.png"]);
        let a = dir.join("a.png");
        lock_even_if_poisoned(&crate::support::threads::BREAKING_FILES).push(a.clone());
        let (mut app, ctx) = app_on(&dir);
        app.rename_in_background(
            &ctx,
            crate::support::test_support::unlisted([&a]),
            vec![dir.join("c.png")],
            crate::files::file_io::PickedFiles::new(vec![a.clone()]),
        );
        assert!(matches!(told_to(&app), Some(Told::FailedWrite(_))));
        assert!(run_background_until(&mut app, &ctx, |app| app
            .file_operations
            .renaming
            .is_none()));
        assert!(a.exists());
        lock_even_if_poisoned(&crate::support::threads::BREAKING_FILES).retain(|p| *p != a);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Makes `a.png` and a larger `b.png` in a new folder named after `name`, and the files
    /// picked, `a.png` with the fingerprint it has now. Then it swaps the two names, as another
    /// window or program may while a question about `a.png` is open.
    fn picked_then_swapped(name: &str) -> (std::path::PathBuf, crate::files::file_io::PickedFiles) {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-picked-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a.png"), dir.join("b.png"));
        image::RgbImage::new(2, 2).save(&a).unwrap();
        image::RgbImage::new(3, 3).save(&b).unwrap();
        let picked = (a.clone(), crate::files::file_io::fingerprint(&a).unwrap());
        let picked = crate::files::file_io::PickedFiles::recorded(vec![picked]);
        std::fs::rename(&a, dir.join("t")).unwrap();
        std::fs::rename(&b, &a).unwrap();
        std::fs::rename(dir.join("t"), &b).unwrap();
        (dir, picked)
    }

    /// A delete for good or a move to the trash, confirmed after another photo took the name of
    /// the one asked about, leaves that photo alone and tells why. Before, the answer acted on
    /// whatever had the name when the dialog closed.
    #[test]
    fn a_delete_confirmed_leaves_another_file_at_the_name_alone() {
        for trash in [false, true] {
            let (dir, picked) = picked_then_swapped(&format!("delete-swapped-{trash}"));
            let (mut app, ctx) = app_on(&dir);
            let asked =
                super::Asked::new(app.opened_folder_hash(), vec!["a.png".into()], None, picked);
            app.dialogs.confirm_delete(asked, trash);
            let (targets, picked) = app.asked_targets().unwrap();
            app.delete_now(&ctx, trash, targets, picked);
            assert!(run_background_until(&mut app, &ctx, |app| !app.working()));
            assert_eq!(image::image_dimensions(dir.join("a.png")).unwrap(), (3, 3));
            assert!(dir.join("b.png").exists());
            let told = app.notice.text().unwrap_or_default().to_owned();
            assert!(told.contains("changed"), "{told}");
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    /// Rename and Rename Many, answered after another photo took the name of the one asked
    /// about, leave that photo's name alone and tell why.
    #[test]
    fn a_rename_answered_leaves_another_file_at_the_name_alone() {
        use crate::ui::dialogs::Prompt;
        for prompt in [Prompt::Rename, Prompt::RenameMany] {
            let many = matches!(prompt, Prompt::RenameMany);
            let (dir, picked) = picked_then_swapped(&format!("rename-swapped-{many}"));
            let (mut app, ctx) = app_on(&dir);
            app.list.select([0].into());
            let asked =
                super::Asked::new(app.opened_folder_hash(), vec!["a.png".into()], None, picked);
            app.dialogs.ask_about(asked, prompt, "a.png".into());
            app.act_on_prompt_answer(&ctx, prompt, if many { "x#" } else { "c.png" });
            assert!(run_background_until(&mut app, &ctx, |app| !app.working()
                && app.file_operations.renaming.is_none()));
            assert_eq!(image::image_dimensions(dir.join("a.png")).unwrap(), (3, 3));
            assert!(!dir.join("c.png").exists() && !dir.join("x1.png").exists());
            let told = app.notice.text().unwrap_or_default().to_owned();
            assert!(told.contains("changed"), "{told}");
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    /// A folder trashed or renamed from the folder tree, after another folder took its path
    /// while the question was open, is left alone, and so is the other folder.
    #[test]
    fn a_folder_answered_about_is_left_alone_when_another_took_its_path() {
        for trash in [false, true] {
            let dir = contract_folder(&format!("folder-swapped-{trash}"));
            let sub = dir.join("sub");
            let (mut app, ctx) = app_on(&dir);
            // The folder is read again for the refusal, not for the change that the watcher saw.
            app.config.auto_refresh = false;
            let before = app.folder.whole_listings();
            match trash {
                true => app.dialogs.ask_trash_folder(&sub),
                false => app.dialogs.ask_rename_folder(&sub),
            }
            // The question takes the folder's device and inode in the background, before the
            // folder is replaced.
            app.dialogs.wait_until_folder_taken();
            std::fs::rename(&sub, dir.join("old")).unwrap();
            std::fs::create_dir(&sub).unwrap();
            match trash {
                true => app.trash_folder(&ctx),
                false => app.rename_folder(&ctx, "renamed"),
            }
            assert!(run_background_until(&mut app, &ctx, |app| !app.working()));
            assert!(sub.is_dir() && dir.join("old").is_dir());
            assert!(!dir.join("renamed").exists());
            let told = app.notice.text().unwrap_or_default().to_owned();
            assert!(told.contains("another file has taken its name"), "{told}");
            let listed = |app: &crate::App| app.folder.whole_listings() > before;
            assert!(
                run_background_until(&mut app, &ctx, listed),
                "not read again"
            );
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
}
