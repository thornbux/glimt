//! Rename Many's steps on disk while they run, its plan, a job log (`JobLog`). A crash or a power
//! cut halfway leaves files under their new names, their old ones, or hidden temporary ones. The
//! next start finds the plan and puts the old names back or finishes. The plan is locked while
//! it runs, so a plan that another window is still carrying out is left alone. The logs of Glimt
//! 0.92 or older, in folders of their own, are no longer read (`clear_old_log_folders`).
//!
//! A Move keeps a log of the same kind (`MoveLog`), so that the marks and collections of the
//! files that went before Glimt was killed follow them at the next start. A Delete and a Move to
//! Trash keep one too (`DeleteLog`), so that the marks and stars of the files that went before a
//! kill are forgotten at the next start. A name whose folder is not there at that start, as on a
//! stick not plugged in yet, keeps its marks, and its log stays for a later start. The files
//! that went also leave the collection that was shown during the delete, and a photo that
//! stayed takes back its sidecars that the kill left in the trash, or put aside under a hidden
//! name.

use crate::files::file_io::{
    FileFingerprint, is_surely_dir, is_surely_gone, is_surely_there, may_have_been_done,
};
use crate::files::job_log::{AtStart, CannotTell, Direction, JobLog, Record, Sure, remove_job_log};
use crate::images::save::{same_file, same_file_by_opening};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// The folders, in Glimt's state folder, that Glimt 0.92 and older kept their logs in: Rename
/// Many's plans, the Moves' logs and the deletes' logs. Glimt 0.93 and 0.94 put every log there
/// right once and wrote the job logs in `jobs/` instead. This Glimt no longer reads them
/// (`clear_old_log_folders`).
pub(crate) const OLD_LOG_FOLDERS: [&str; 3] = ["renames", "moves", "deletes"];

/// The folders that Glimt 0.92 and older kept beside their logs, in a folder of `OLD_LOG_FOLDERS`,
/// with the lists of each log's files, the collection shown, the sidecars put aside and the
/// photos kept.
const OLD_LISTS_FOLDERS: [&str; 4] = ["files", "collections", "aside", "kept"];

/// The lock file that every window took to change a list in a folder of `OLD_LISTS_FOLDERS`.
/// It is left even when no list is, so it says nothing of a log.
const OLD_LISTS_LOCK: &str = ".lock";

/// The start of the hidden name that a swap gives a file between its two steps
/// (`file_ops::rename_files`). No other file takes such a name, and the plan's own steps fill it.
pub const HIDDEN_NAME: &str = ".glimt-rename-";

// The files of the deletes' logs that `DeleteLog::note_aside` synced on this thread, so that a
// test can see that a sidecar is noted on disk before it is put aside. A power cut cannot be
// simulated in a test.
#[cfg(test)]
thread_local! {
    pub(crate) static LOG_FILES_SYNCED: std::cell::RefCell<Vec<PathBuf>> =
        const { std::cell::RefCell::new(Vec::new()) };
    // Each sidecar that a delete for good on this thread put aside, with the files of the log
    // and the folders synced on this thread by the moment of its rename, so that a test can see
    // that the note came before the rename and not only before the test's kill.
    pub(crate) static SYNCED_AT_PUT_ASIDE: std::cell::RefCell<Vec<(PathBuf, Vec<PathBuf>)>> =
        const { std::cell::RefCell::new(Vec::new()) };
    // The folders that a start on this thread sees on another device than the one they are
    // on, as an empty mount point is while its share or stick is not mounted.
    pub(crate) static ON_ANOTHER_DEVICE: std::cell::RefCell<Vec<PathBuf>> =
        const { std::cell::RefCell::new(Vec::new()) };
    // The volume that a test gives a folder and the folders below it on this thread, as if a
    // card with that serial number were mounted there (`volume_of`). A test never looks in the
    // real /dev/disk/by-uuid.
    pub(crate) static TEST_VOLUMES: std::cell::RefCell<Vec<(PathBuf, std::ffi::OsString)>> =
        const { std::cell::RefCell::new(Vec::new()) };
    // How many times `volume_of` was asked on this thread, so that a test can see that a start
    // looks up a folder's volume once and not once for each file.
    pub(crate) static VOLUME_LOOKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    // Whether a start on this thread fails to read the volumes, as when /dev/disk/by-uuid
    // cannot be read.
    pub(crate) static VOLUMES_UNREADABLE: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    // The folders that a start on this thread sees on a block device, with the folders below
    // them, as a card in a reader or a stick is (`is_on_a_block_device`). A test's own folder
    // is not on one.
    pub(crate) static ON_A_BLOCK_DEVICE: std::cell::RefCell<Vec<PathBuf>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// What the `folder` and `volume` records of a log noted of a folder: its device, its inode (0
/// where its file system keeps none), and the volume it is on where its device has a name in
/// /dev/disk/by-uuid. Only `is_on_the_volume_noted` and `is_the_folder_noted` compare them with a
/// folder now (`source_rules::one_rule_tells_the_folder_a_log_noted`).
#[derive(Debug, Clone)]
struct FolderNoted {
    noted_device: u64,
    noted_inode: u64,
    noted_volume: Option<std::ffi::OsString>,
    /// The volumes found now, shared by every folder of the log. /dev/disk/by-uuid is read once
    /// for the start that reads the log, not once for each folder or file, and its answer is
    /// kept whatever it is: a volume, no volume for a device, or a read that failed (`None`).
    /// A volume found there is looked up afresh before it counts (`is_still_on_device`).
    volumes_now: std::sync::Arc<std::sync::OnceLock<Option<VolumesNow>>>,
}

/// The volumes that /dev/disk/by-uuid names now, by their devices. A test gives them by folder
/// with `TEST_VOLUMES` instead.
#[derive(Debug)]
struct VolumesNow {
    #[cfg(not(test))]
    by_device: HashMap<u64, Option<std::ffi::OsString>>,
    #[cfg(test)]
    by_folder: Vec<(PathBuf, std::ffi::OsString)>,
}

impl VolumesNow {
    /// Reads the volumes once. Returns `None` when they cannot be read. It waits for the disk.
    fn read() -> Option<Self> {
        #[cfg(test)]
        {
            VOLUME_LOOKS.set(VOLUME_LOOKS.get() + 1);
            let by_folder = TEST_VOLUMES.with_borrow(|volumes| volumes.clone());
            (!VOLUMES_UNREADABLE.get()).then_some(Self { by_folder })
        }
        #[cfg(not(test))]
        {
            let by_device = crate::files::file_io::volumes_by_device()?;
            Some(Self { by_device })
        }
    }

    /// Returns the volume that `folder`, on the device `device`, is on, and `None` when it is
    /// on none.
    fn volume_of_folder(&self, folder: &Path, device: u64) -> Option<std::ffi::OsString> {
        #[cfg(test)]
        {
            let _ = device;
            (self.by_folder.iter().find(|(at, _)| folder.starts_with(at))).map(|(_, v)| v.clone())
        }
        #[cfg(not(test))]
        {
            let _ = folder;
            self.by_device.get(&device).cloned().flatten()
        }
    }

    /// Returns whether the volume `volume` is on the device `device` of `folder` now, looked up
    /// afresh (`file_io::device_of_volume_now`), and `false` when the look fails. A test gives
    /// the volume with `TEST_VOLUMES` as it is at the moment of the look.
    fn is_still_on_device(volume: &std::ffi::OsStr, folder: &Path, device: u64) -> bool {
        #[cfg(test)]
        {
            let _ = device;
            let is_there = TEST_VOLUMES.with_borrow(|volumes| {
                let now = volumes.iter().find(|(at, _)| folder.starts_with(at));
                now.is_some_and(|(_, now)| now == volume)
            });
            is_there && !VOLUMES_UNREADABLE.get()
        }
        #[cfg(not(test))]
        {
            let _ = folder;
            crate::files::file_io::device_of_volume_now(volume) == Some(device)
        }
    }
}

/// The job logs a start found: the Moves with the files that went, the deletes with the files
/// whose marks are to be forgotten, and the other jobs with what each did (`jobs_cut_short`).
pub type JobsCutShort = (
    Vec<(MoveLog, Moves)>,
    Vec<(DeleteLog, Vec<PathBuf>)>,
    Vec<(JobLog, JobDoneBeforeAKill)>,
);

/// What a job that is neither a Move nor a delete had done when Glimt was killed during it, as
/// the start reads it from the job's log. The window follows it as the job's report would have,
/// and the log goes after the writes that follow (`remove_once_written`).
pub enum JobDoneBeforeAKill {
    /// An Undo from the trash put these files back: each with the marks and stars it had.
    BackFromTrash(Vec<(PathBuf, (u8, u8))>),
    /// Rename Folder gave the folder (first) its new name (second).
    FolderRenamed(PathBuf, PathBuf),
    /// Move Folder to Trash took the folder away.
    FolderTrashed(PathBuf),
    /// The job had not changed the folder yet, or the start cannot tell what it did.
    NothingToFollow,
}

/// How the start reads a job log that a kill cut short, for each kind of job in `JOB_KINDS`
/// (`read_at_start`). `jobs_cut_short` has an arm for each, so a new kind of job gets a reader
/// or the build fails.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ReadAtStart {
    /// A Move, or an Undo of one: the marks follow the files that went.
    Move,
    /// A delete or a move to the trash: the marks of the files that went are forgotten.
    Delete,
    /// An Undo from the trash: the files put back get their marks again.
    UndoTrash,
    /// A Rename Folder or a Move Folder to Trash.
    Folder,
    /// A Rename Many, which `cut_short_plans` puts right after the others.
    Rename,
}

/// The renames in the order they are done, as (from, to).
pub type Moves = Vec<(PathBuf, PathBuf)>;

/// What a plan of Rename Many knows of its files: the fingerprint of each step's photo, and the
/// folders it noted (`folders_noted`).
type PlanKnown = (Vec<Option<FileFingerprint>>, HashMap<PathBuf, FolderNoted>);

/// A plan left by a Rename Many that was cut short. It holds its file, the renames, and how
/// many of them were done, which is `None` for a plan of Glimt 0.34 or older, because those did
/// not count them.
pub type CutShort = (PathBuf, Moves, Option<usize>);

/// The plan of a Rename Many being carried out, a job log (`JobLog`) in `jobs/` of the kind
/// "rename": a `step` record for each rename, with the fingerprint of its photo, all written
/// and synced before the first rename, and a `done` record after each rename. It is removed
/// when dropped, as the renames are done or undone, and it is left on disk only when Glimt
/// stops halfway.
pub struct Journal {
    log: JobLog,
    /// The number of renames done so far, which is the number of the next `done` record.
    done: usize,
    /// One name in each folder that the renames touch. Those folders are synced before the
    /// plan is removed, so that the plan never goes before the renames are on disk.
    one_name_per_folder: Vec<PathBuf>,
}

impl Journal {
    /// Counts one more rename done with a `done` record, so that the next start knows which
    /// were done. The file names cannot tell, because a swap has the same names before and
    /// after.
    // It is not synced, because a crash of Glimt does not need that. A power cut may lose the
    // last steps' records, and `repair_cut_short` finds them again by the names
    // (`done_by_names`).
    pub fn count_step_done(&mut self) {
        // Not told: a count that did not reach the plan is found again by the names, as above.
        let _ = self.log.add_records(&[Record::Done(self.done)]);
        self.done += 1;
    }

    /// Counts the step that puts the last file of a swap under its hidden name, with the disk
    /// synced before and after. Once the swap is done, the names on disk are those of the start
    /// again, and the plan could not tell a power cut after the swap from one before it. The
    /// names reach the disk before the count does, so a count of half is never there without
    /// them, and a count below half tells a swap not begun (`done_by_names`). Returns an error,
    /// so that the caller undoes the swap, when the disk says that the names did not get there
    /// (a USB stick pulled out during the sync), and also when the count itself did not (a full
    /// state disk). A swap left done without its count would be read at the next start after a
    /// kill as one not begun, and finishing it would swap the photos back.
    pub fn count_middle_of_swap_done(&mut self) -> std::io::Result<()> {
        sync_folders(&self.one_name_per_folder).map_err(|(_, e)| e)?;
        self.log.add_records_synced(&[Record::Done(self.done)])?;
        self.done += 1;
        Ok(())
    }

    /// Counts one more rename done, through `count_middle_of_swap_done` when `is_middle_of_swap`
    /// and otherwise `count_step_done`. Returns an error when the middle did not reach the disk.
    pub fn count_rename_done(&mut self, is_middle_of_swap: bool) -> std::io::Result<()> {
        if is_middle_of_swap {
            return self.count_middle_of_swap_done();
        }
        self.count_step_done();
        Ok(())
    }
}

impl Journal {
    /// Makes every later write of the plan fail, as on a full disk.
    #[cfg(test)]
    pub fn refuse_writes(&mut self) {
        let path = self.log.path().to_path_buf();
        let mut read_only = JobLog::found(path.clone(), File::open(&path).unwrap());
        if let Some(sure) = self.log.end_of_job() {
            read_only = read_only.read_surely(sure);
        }
        // The file that holds the lock is forgotten open, so that the lock stays.
        std::mem::forget(std::mem::replace(&mut self.log, read_only));
    }

    /// Returns the plan's path.
    #[cfg(test)]
    pub fn path(&self) -> &Path {
        self.log.path()
    }

    /// Leaves the plan on disk, because the renames could not all be undone, so the next start
    /// offers to put the files right. Its lock goes with this Glimt.
    pub fn keep_on_disk(mut self) {
        self.log.keep_for_the_next_start();
    }
}

impl Drop for Journal {
    fn drop(&mut self) {
        if let Some(sure) = self.log.end_of_job() {
            remove_plan(self.log.path(), &self.one_name_per_folder, sure);
        }
    }
}

/// The log of a Move while it runs, a job log (`JobLog`) in `jobs/`: a `step` record for each
/// file, with its old and new name and its fingerprint, written down before the file goes. The
/// marks and collections follow the files only once the Move has ended, so Glimt killed halfway
/// (a logout's SIGKILL, Ctrl+C, a third close) left the marks and stars of every file that went
/// under the old names. The log is removed once they have followed (`remove_once_followed`).
/// Left on disk by a kill, it is found at the next start (`move_jobs_cut_short`). It is locked
/// while in use, so the start of another window leaves it alone.
pub struct MoveLog {
    log: JobLog,
    /// The number of steps written down so far, which is the number of the next.
    steps: usize,
    /// Whether the log was found at the start, left by a Glimt that was killed.
    is_found_at_start: bool,
    /// The folders that the files go to, each with a `folder` record before its first step.
    folders: HashSet<PathBuf>,
    /// The first write of the log that failed, on a full disk say. The log takes no more steps
    /// after it, and the Move goes on without them.
    failure: Option<std::io::Error>,
}

impl MoveLog {
    /// Writes down that the file `from` goes to `to`, before it goes, with the file's
    /// fingerprint. It is not synced, because a kill of Glimt does not need that. A write that
    /// fails is kept for `take_failure`, and the file goes all the same: a Move is how a full
    /// disk is cleared, and without the step a kill only leaves the file's marks behind, as
    /// before there was a log.
    pub fn note(&mut self, from: &Path, to: &Path) {
        if self.is_found_at_start || self.failure.is_some() {
            return;
        }
        let mut records = Vec::new();
        // The folder it leaves is noted too, so that a start can tell a file at its old name on
        // another card at the same path from the one that stayed (`moves_that_went`).
        for path in [from, to] {
            let folder = path.parent().unwrap_or(Path::new("/"));
            if self.folders.insert(folder.to_path_buf()) {
                records.extend(folder_record(folder));
            }
        }
        records.push(Record::Step {
            number: self.steps,
            from: from.to_path_buf(),
            to: Some(to.to_path_buf()),
            fingerprint: crate::files::file_io::fingerprint(from),
        });
        self.failure = self.log.add_records(&records).err();
        self.steps += 1;
    }

    /// Notes that the last step written down failed with `error`, going the way `direction`
    /// says (`JobLog::note_last_step_failed`): forward, or back when the file was put back from
    /// the step's new name. A step that may have been done counts as not done here, so its marks
    /// stay at the old name, and the log stays for the next start, which makes them follow when
    /// the file is at its new name with its fingerprint.
    pub fn note_last_step_failed(&mut self, direction: Direction, error: &std::io::Error) {
        self.log.note_last_step_failed(direction, error);
    }

    /// Returns the first write of the log that failed, and `None` when every one worked.
    pub fn take_failure(&mut self) -> Option<std::io::Error> {
        self.failure.take()
    }

    /// Returns a log at `path` that refuses every write, as on a full disk.
    #[cfg(test)]
    pub fn refusing_writes(path: &Path) -> MoveLog {
        std::fs::write(path, b"").unwrap();
        MoveLog {
            log: JobLog::found(path.to_path_buf(), File::open(path).unwrap()),
            steps: 0,
            is_found_at_start: false,
            folders: HashSet::new(),
            failure: None,
        }
    }

    /// Whether the log was found at the start, left by a Glimt that was killed during its Move.
    pub fn is_found_at_start(&self) -> bool {
        self.is_found_at_start
    }

    /// Removes the log once the marks and collections have followed its files. The window sent
    /// their edits before, to the thread of Glimt's own files and to that of the user's folders,
    /// and the log goes after both. A log that is dropped instead stays on disk, and the next
    /// start makes them follow.
    pub fn remove_once_followed(self) {
        remove_after_the_writes(self.log);
    }
}

/// The log of an Undo from the trash while it runs, a job log (`JobLog`) in `jobs/` of the kind
/// "undo-trash". A Move to Trash takes the files' marks and stars out of the marks file, and the
/// Undo holds them, with the files' places in the collection shown, only in memory until the
/// files are back. So before the first file comes back, the log holds, synced, a `step` record
/// for each file or folder put back and for each file in such a folder that had marks or a
/// place, with its `marks` and `place` records. Just before each file or folder comes back, a
/// `written` record gives its step the fingerprint it has in the trash, which the rename keeps,
/// and a `done` record follows once it is back. The next start after a kill gives the marks and
/// places back to each file whose step, or whose folder's step, is done, or has a file with
/// that fingerprint at its path (`back_from_the_trash`).
pub struct UndoTrashLog {
    log: JobLog,
    /// The number of the step of each file or folder put back.
    steps: HashMap<PathBuf, usize>,
    /// The first write of the log that failed. The Undo goes on without the later records.
    failure: Option<std::io::Error>,
}

impl UndoTrashLog {
    /// Writes down the fingerprint of a file or folder that the Undo is about to put back, or
    /// that it is back.
    pub fn note(&mut self, step: crate::files::trash::Restoring) {
        use crate::files::trash::Restoring;
        let (path, coming) = match step {
            Restoring::Coming(path, fingerprint) => (path, Some(fingerprint)),
            Restoring::Back(path) => (path, None),
            // The step is judged by the name its record holds, the file's own, whichever way
            // the rename went. A rename stays on one file system, so the trash's name is on a
            // mount of the same type.
            Restoring::Failed(source, destination, error) => {
                let step = [source, destination]
                    .into_iter()
                    .find(|path| self.steps.contains_key(*path));
                if step.is_none_or(|step| may_have_been_done(error, step)) {
                    self.log.keep_for_the_next_start();
                }
                return;
            }
        };
        let Some(&number) = self.steps.get(path).filter(|_| self.failure.is_none()) else {
            return;
        };
        let record = match coming {
            None => Record::Done(number),
            Some(None) => return,
            Some(Some(fingerprint)) => Record::Written {
                number,
                fingerprint,
            },
        };
        self.failure = self.log.add_records(&[record]).err();
    }

    /// Returns the first write of the log that failed, and `None` when every one worked.
    pub fn take_failure(&mut self) -> Option<std::io::Error> {
        self.failure.take()
    }

    /// Returns the log, to be removed once the marks and places have been written
    /// (`remove_once_written`). The window gives the marks and places back to the files
    /// `given_back` and to those in them. When the log stays for the next start, because a step
    /// could not tell what it did, it is written anew without their steps.
    pub fn into_log(mut self, given_back: &[PathBuf]) -> JobLog {
        self.log.note_steps_applied(given_back);
        self.log
    }
}

/// The log of a Delete or a Move to Trash while it runs, a job log (`JobLog`) in `jobs/` of the
/// kind "delete" or "trash": a `step` record for each file, with its fingerprint, written down
/// before the file goes, and a `done` record once it went. The marks and stars of the files
/// that went are forgotten only once the job has ended, so Glimt killed halfway left them at the
/// names of files that are gone, where the next file of that name showed them. The log is
/// removed once they are forgotten (`remove_once_forgotten`). Left on disk by a kill, it is
/// found at the next start (`jobs_cut_short`). It is locked while in use, so the start of
/// another window leaves it alone.
pub struct DeleteLog {
    log: JobLog,
    /// The kind of the log's job, "delete" or "trash".
    job: &'static str,
    /// The number of each file written down, by which its later records name it.
    steps: HashMap<PathBuf, usize>,
    /// The folders of the files written down so far, each with a `folder` record before the
    /// first step in it.
    folders: HashSet<PathBuf>,
    /// The first write of the log that failed, on a full disk say. The log takes no more records
    /// after it, and the delete goes on without them, as a Move does without its log.
    failure: Option<std::io::Error>,
    /// Whether the log stays on disk for a later start: it names a file that the start could not
    /// tell went or stayed, or a sidecar that may still be under its hidden name.
    has_names_left: bool,
    /// For a log found at the start, the log that is written in its place when it stays for a
    /// later start: the folder of the job logs, the time of the log, and the records of the steps
    /// left. A later start then does not look again at every file that this one could tell
    /// about.
    written_anew: Option<(PathBuf, std::time::SystemTime, Vec<Record>)>,
}

impl DeleteLog {
    /// Notes that the delete of `from` failed with `error`, so that the log stays for the next
    /// start when the delete may have been done (`file_io::may_have_been_done`). It stays too
    /// when the log has no step of `from`, as then no record tells which name the step took.
    pub fn note_step_failed(&mut self, from: &Path, error: &std::io::Error) {
        if !self.steps.contains_key(from) || may_have_been_done(error, from) {
            self.log.keep_for_the_next_start();
        }
    }

    /// Keeps the log for the next start, as the job cannot tell whether a file went.
    pub fn keep_for_the_next_start(&mut self) {
        self.log.keep_for_the_next_start();
    }

    /// Writes down that the files `paths` go, before they go, each with its fingerprint. It is
    /// not synced, because a kill of Glimt does not need that. A write that fails is kept for
    /// `take_failure`, and the files go all the same: a delete is how a full disk is cleared.
    pub fn note(&mut self, paths: &[PathBuf]) {
        let mut steps = Vec::new();
        for path in paths {
            let folder = path.parent().unwrap_or(Path::new("/"));
            if self.folders.insert(folder.to_path_buf()) {
                steps.extend(folder_record(folder));
            }
            let number = self.steps.len();
            self.steps.insert(path.clone(), number);
            steps.push(Record::Step {
                number,
                from: path.clone(),
                to: None,
                fingerprint: crate::files::file_io::fingerprint(path),
            });
        }
        self.add(&steps, false);
    }

    /// Writes down that the files `paths`, which the log names, are gone. It is not synced: a
    /// power cut that loses it leaves a step without `done`, which the next starts read as a
    /// delete that may not have happened (`put_right_a_delete`).
    pub fn note_done(&mut self, paths: &[PathBuf]) {
        let done: Vec<Record> = (paths.iter())
            .filter_map(|path| self.steps.get(path))
            .map(|number| Record::Done(*number))
            .collect();
        self.add(&done, false);
    }

    /// Writes down, before it is done, that a delete for good puts the sidecars of the file
    /// `photo`, which the log names, aside under hidden names, given as pairs
    /// `(sidecar, hidden)`. Glimt killed before the delete deleted them or took them back left
    /// them under those names, which nothing else finds, and the photo, if still there, without
    /// its keywords, comment and alt text. The next start puts them right
    /// (`sidecars_back_from_hiding`). The records are synced, with the steps before them, as the
    /// rename that puts a sidecar aside is made lasting before the photo goes, and a power cut
    /// could otherwise leave the sidecar hidden with no note of it on disk. A delete of files
    /// without sidecars has no sync of the log at all.
    pub fn note_aside(&mut self, photo: &Path, aside: &[(PathBuf, PathBuf)]) {
        let Some(&number) = self.steps.get(photo) else {
            return;
        };
        let records: Vec<Record> = (aside.iter())
            .map(|(sidecar, hidden)| Record::Aside {
                number,
                sidecar: sidecar.clone(),
                hidden: hidden.clone(),
            })
            .collect();
        if !records.is_empty() {
            self.add(&records, true);
        }
    }

    /// Adds `records` to the log, synced when `is_synced`, unless a write failed before.
    fn add(&mut self, records: &[Record], is_synced: bool) {
        if self.failure.is_some() || records.is_empty() {
            return;
        }
        let added = match is_synced {
            true => self.log.add_records_synced(records),
            false => self.log.add_records(records),
        };
        #[cfg(test)]
        if is_synced && added.is_ok() {
            let path = self.log.path().to_path_buf();
            LOG_FILES_SYNCED.with_borrow_mut(|synced| synced.push(path));
        }
        self.failure = added.err();
    }

    /// Returns the first write of the log that failed, and `None` when every one worked.
    pub fn take_failure(&mut self) -> Option<std::io::Error> {
        self.failure.take()
    }

    /// Keeps the log on disk after the job for a later start, because a sidecar of `photo`, a
    /// photo that stays, may still be under its hidden name. The rename that hides it or gives
    /// it back failed, and on a share that lost its answer it may have been done all the same.
    /// The next start gives it back when the photo is still there as the log knew it. The photo
    /// is written down as kept, synced, so that a later start does not take it for one that the
    /// delete deleted when the user has moved it away since, and does not delete its hidden
    /// sidecar. A write that fails is kept, as in `note`.
    pub fn keep_for_a_later_start(&mut self, photo: &Path) {
        self.keep_log_for_a_later_start();
        if let Some(&number) = self.steps.get(photo) {
            self.add(&[Record::Kept(number)], true);
        }
    }

    /// Writes down `photo`, which the log names, as kept, synced, without keeping the log after
    /// the job: its sidecars stay under their hidden names, and a start after a kill tells them
    /// rather than delete them. A write that fails is kept, as in `note`.
    pub fn note_kept(&mut self, photo: &Path) {
        if let Some(&number) = self.steps.get(photo) {
            self.add(&[Record::Kept(number)], true);
        }
    }

    /// Keeps the log on disk after the job for a later start, without writing down a photo as
    /// kept. A delete for good does this for a photo that it deleted when the look after the
    /// delete failed and the rename back of a sidecar failed too, so that the sidecar may still
    /// be hidden.
    pub fn keep_log_for_a_later_start(&mut self) {
        self.log.keep_for_the_next_start();
        self.has_names_left = true;
    }

    /// Removes the log once the marks and stars of its files that went are forgotten. The window
    /// sent their edit before, to the thread of Glimt's own files, and the log goes after it. A
    /// log that is dropped instead stays on disk, and the next start forgets them. A log found
    /// at the start that names a file the start could not tell about is written anew in
    /// `jobs/` with only those steps, and goes once the new one is on disk (`write_anew`).
    pub fn remove_once_forgotten(self) {
        if !self.has_names_left {
            remove_after_the_writes(self.log);
            return;
        }
        let (Some((dir, time, records)), job) = (self.written_anew, self.job) else {
            return;
        };
        let (path, file) = self.log.into_path_and_file();
        crate::files::file_io::write_later(move || {
            if let Err(e) = write_anew(&dir, job, time, &records, &path) {
                crate::files::file_io::save_failed(format!("{}: {e}", dir.display()));
            }
            drop(file);
        });
    }
}

/// What a delete's log says, read at the start (`put_right_a_delete`).
#[derive(Default)]
struct DeleteRecords {
    /// The kind of the log's job, "delete" or "trash".
    job: &'static str,
    /// The files the log names, in its order.
    names: Vec<PathBuf>,
    /// The fingerprint of each of `names`, taken before it went.
    known: Vec<Option<FileFingerprint>>,
    /// The collection shown during the delete.
    collection: Option<PathBuf>,
    /// The sidecars put aside, as (photo, sidecar, hidden name).
    aside: Vec<[PathBuf; 3]>,
    /// The photos kept while their sidecar may still be hidden.
    kept: HashSet<PathBuf>,
    /// The files written down as gone (`done`). A log written anew from one of Glimt 0.92 or
    /// older has a `done` for each name, as the old logs read a name that is gone as a file that
    /// went.
    done: HashSet<PathBuf>,
    /// The files of a delete for good without `done` that an earlier start found gone.
    found: HashSet<PathBuf>,
    /// The folders of the files, each with its device and inode as the log saw it.
    folders: HashMap<PathBuf, FolderNoted>,
    /// The logs that the log's `old-log` records name, which it was written anew from.
    old_logs: Vec<PathBuf>,
}

/// Makes a new job log in `dir` of a Move, or of the Undo of one, as `job` says ("move" or
/// "undo-move"), locked and with its header on disk.
pub fn start_move_log(dir: &Path, job: &str) -> std::io::Result<MoveLog> {
    Ok(MoveLog {
        log: JobLog::start(dir, job, &[])?,
        steps: 0,
        is_found_at_start: false,
        folders: HashSet::new(),
        failure: None,
    })
}

/// Makes a new job log in `dir` of a delete for good ("delete") or a Move to Trash ("trash"),
/// as `job` says, locked and with its header on disk, with the path of the `collection` shown,
/// so that the next start can take the files that went out of it when Glimt is killed during
/// the delete.
pub fn start_delete_log(
    dir: &Path,
    job: &'static str,
    collection: Option<&Path>,
) -> std::io::Result<DeleteLog> {
    let shown: Vec<Record> = collection
        .map(|c| Record::Shown(c.to_path_buf()))
        .into_iter()
        .collect();
    Ok(DeleteLog {
        log: JobLog::start(dir, job, &shown)?,
        job,
        steps: HashMap::new(),
        folders: HashSet::new(),
        failure: None,
        has_names_left: false,
        written_anew: None,
    })
}

/// Makes a new job log in `dir` of an Undo from the trash that puts `files` back, with the
/// `marks` and stars that the Move to Trash took from them, and the places in the `collection`
/// shown that it took them out of, `places`. Every record is on disk before this returns.
pub fn start_undo_trash_log(
    dir: &Path,
    files: &[PathBuf],
    marks: &[(PathBuf, (u8, u8))],
    collection: Option<(&Path, &[(usize, PathBuf)])>,
) -> std::io::Result<UndoTrashLog> {
    let mut records: Vec<Record> = (collection.iter())
        .map(|(c, _)| Record::Shown(c.to_path_buf()))
        .collect();
    let mut steps: HashMap<PathBuf, usize> = HashMap::new();
    let mut number_of = |path: &Path, records: &mut Vec<Record>| {
        let next = steps.len();
        let number = *steps.entry(path.to_path_buf()).or_insert(next);
        if number == next {
            records.push(Record::Step {
                number,
                from: path.to_path_buf(),
                to: None,
                fingerprint: None,
            });
        }
        number
    };
    let mut folders = HashSet::new();
    for file in files {
        let folder = file.parent().unwrap_or(Path::new("/"));
        if folders.insert(folder) {
            records.extend(folder_record(folder));
        }
        number_of(file, &mut records);
    }
    for (path, (bits, stars)) in marks {
        let number = number_of(path, &mut records);
        records.push(Record::Marks {
            number,
            bits: *bits,
            stars: *stars,
        });
    }
    for (index, path) in collection.into_iter().flat_map(|(_, places)| places) {
        let number = number_of(path, &mut records);
        records.push(Record::Place {
            number,
            index: *index,
        });
    }
    let put_back: HashSet<&PathBuf> = files.iter().collect();
    steps.retain(|path, _| put_back.contains(path));
    Ok(UndoTrashLog {
        log: JobLog::start(dir, "undo-trash", &records)?,
        steps,
        failure: None,
    })
}

/// Makes a new job log in `dir` of a Rename Folder of `folder` to `to` ("rename-folder"), or of
/// a Move Folder to Trash of it ("trash-folder", `to` is `None`), with the folder's device and
/// inode and the `collection` shown. Every record is on disk before this returns, so that the
/// next start after a kill can make the marks and collection entries of the files in the folder
/// follow it, or forget them (`folder_job_done`).
pub fn start_folder_log(
    dir: &Path,
    folder: &Path,
    to: Option<&Path>,
    collection: Option<&Path>,
) -> std::io::Result<JobLog> {
    let mut records: Vec<Record> = collection
        .map(|c| Record::Shown(c.to_path_buf()))
        .into_iter()
        .collect();
    // The folders that hold it before and after too, so that a start can tell them from an
    // empty mount point at their path (`is_gone_at_start`).
    let mut noted: Vec<&Path> = vec![folder];
    for path in std::iter::once(folder).chain(to) {
        noted.extend(path.parent().filter(|p| !noted.contains(p)));
    }
    records.extend(noted.into_iter().flat_map(folder_record));
    records.push(Record::Tree {
        from: folder.to_path_buf(),
        to: to.map(Path::to_path_buf),
    });
    let job = if to.is_some() {
        "rename-folder"
    } else {
        "trash-folder"
    };
    JobLog::start(dir, job, &records)
}

/// Returns what a Rename Folder or a Move Folder to Trash whose log says `records` had done when
/// Glimt was killed, and `CannotTell` when a look or the write to the collection shown failed,
/// so that the log stays for a later start.
/// The folder counts as still where it was when its old path holds a folder with the inode the
/// log noted (or any folder, where the log could note none). A renamed folder counts as renamed
/// only when its old path is surely free and its new one holds it. A new path that cannot be
/// told, as on a stick not mounted at this start, keeps the log. A trashed folder counts as
/// trashed when its old path holds another folder, or is free in a folder that surely is there
/// (`is_gone_at_start`), and its files leave the collection shown here.
fn folder_job_done(records: &[Record]) -> AtStart<JobDoneBeforeAKill> {
    let (mut tree, mut collection) = (None, None);
    for record in records {
        match record {
            Record::Tree { from, to } => tree = Some((from.clone(), to.clone())),
            Record::Shown(c) => collection = Some(c.clone()),
            _ => {}
        }
    }
    let Some((from, to)) = tree else {
        return Ok(JobDoneBeforeAKill::NothingToFollow);
    };
    let folders = folders_noted(records);
    let noted = folders.get(&from);
    // Whether the folder noted is at `path` (`is_the_folder_noted`). A failed look, or a folder
    // of another card at the same path or of a disk that cannot be told, cannot tell.
    let holds_the_folder = |path: &Path| -> AtStart<bool> {
        let found = crate::files::file_io::fingerprint_if_there(path).map_err(|_| CannotTell)?;
        let Some((key, _, _)) = found else {
            return Ok(false);
        };
        match (key, noted) {
            (Some((device, inode)), Some(noted)) if is_surely_dir(path) => {
                // Another card at the path does not tell where the folder noted went.
                if !is_on_the_volume_noted(path, noted, device, Some(inode))? {
                    return Err(CannotTell);
                }
                is_the_folder_noted(path, noted, device, Some(inode))
            }
            _ => Ok(is_surely_dir(path)),
        }
    };
    if holds_the_folder(&from)? {
        return Ok(JobDoneBeforeAKill::NothingToFollow);
    }
    Ok(match to {
        Some(_) if !is_gone_at_start(&from, &folders)? => JobDoneBeforeAKill::NothingToFollow,
        Some(to) if holds_the_folder(&to)? => JobDoneBeforeAKill::FolderRenamed(from, to),
        Some(to) => {
            // A stick not mounted at this start would else lose the marks of every file under
            // the folder.
            is_gone_at_start(&to, &folders)?;
            JobDoneBeforeAKill::NothingToFollow
        }
        None => {
            // A free path counts only in a folder that surely is there. A stick or share not
            // mounted at this start would else have every file under the folder taken for
            // trashed.
            is_gone_at_start(&from, &folders)?;
            forget_in_the_collection_shown(collection.as_deref(), std::slice::from_ref(&from))?;
            JobDoneBeforeAKill::FolderTrashed(from)
        }
    })
}

/// Removes the job log `log` once what the window sent before to the thread of the user's
/// folders and to that of Glimt's own files has been written. A log that is dropped instead
/// stays on disk, and the next start reads it again.
pub fn remove_once_written(log: JobLog) {
    remove_after_the_writes(log);
}

/// Returns what an Undo from the trash whose log says `records` had put back when Glimt was
/// killed: the files whose step is done, or that are in a folder whose step is done, each with
/// its marks and stars. The places of those files in the collection shown are given back there
/// first. A step without its `done` counts as done too when its path holds a file with the
/// fingerprint it had in the trash, which came back in the moment between its rename and its
/// record (the rule of a Move, `moves_that_went`). Any other step not done keeps no marks: its
/// file is still in the trash, or another file has its name. Returns `CannotTell` when a step's
/// path cannot be told (`is_gone_at_start`), or the places cannot be written to the collection,
/// so that the log stays for a later start.
fn back_from_the_trash(records: &[Record]) -> AtStart<JobDoneBeforeAKill> {
    let (mut paths, mut done, mut marks, mut places) =
        (Vec::new(), HashSet::new(), Vec::new(), Vec::new());
    let mut coming = Vec::new();
    let mut collection = None;
    for record in records {
        match record {
            Record::Shown(c) => collection = Some(c.clone()),
            Record::Step { from, .. } => paths.push(from.clone()),
            Record::Done(number) => {
                done.insert(paths[*number].clone());
            }
            Record::Marks {
                number,
                bits,
                stars,
            } => marks.push((*number, (*bits, *stars))),
            Record::Place { number, index } => places.push((*index, *number)),
            Record::Written {
                number,
                fingerprint,
            } => coming.push((*number, *fingerprint)),
            _ => {}
        }
    }
    let folders = folders_noted(records);
    for (number, fingerprint) in coming {
        let path = &paths[number];
        if !done.contains(path)
            && !is_gone_at_start(path, &folders)?
            && is_as_known(path, Some(fingerprint))
        {
            done.insert(path.clone());
        }
    }
    let is_back = |path: &PathBuf| path.ancestors().any(|a| done.contains(a));
    let places: Vec<(usize, PathBuf)> = (places.into_iter())
        .map(|(index, number)| (index, paths[number].clone()))
        .filter(|(_, path)| is_back(path))
        .collect();
    if let Some(collection) = collection.filter(|_| !places.is_empty())
        && !is_surely_gone(&collection)
    {
        let put_back = |all: &mut Vec<PathBuf>| crate::files::file_ops::put_back(all, &places);
        if let Err(e) = crate::model::collections::edit_open_collection(&collection, put_back) {
            crate::files::file_io::save_failed(e);
            // The log stays, so that a later start gives the places back.
            return Err(CannotTell);
        }
    }
    let marks = (marks.into_iter())
        .map(|(number, value)| (paths[number].clone(), value))
        .filter(|(path, _)| is_back(path))
        .collect();
    Ok(JobDoneBeforeAKill::BackFromTrash(marks))
}

/// Removes the job log `log`, held locked, after the writes the window has sent
/// so far: to the thread of the user's folders (collections), and then to that of Glimt's own
/// files (marks). A log found at the start that no reader read surely has no proof that it may
/// go (`Sure`), and stays for a later start.
///
/// A log that stays for the next start, because a step could not tell what it did, is written
/// anew there instead, without the steps whose marks, stars and places the window applied
/// (`JobLog::note_steps_applied`). The next start would else give them again, over what the
/// user changed since, or to another file that took the name.
fn remove_after_the_writes(mut log: JobLog) {
    let sure = log.end_of_job();
    let applied = log.take_steps_applied();
    if sure.is_none() && applied.is_empty() {
        return;
    }
    let (path, file) = log.into_path_and_file();
    crate::files::file_io::write_later_in_user_folders(move || {
        // A log left behind is read again at every start, so a failure is told.
        crate::files::file_io::write_later(move || {
            match sure {
                Some(sure) => {
                    remove_job_log(&path, sure);
                }
                None => {
                    if let Err(e) = written_anew_without(&path, &applied) {
                        crate::files::file_io::save_failed(format!("{}: {e}", path.display()));
                    }
                }
            }
            drop(file);
        });
    });
}

/// Writes the job log at `path` anew beside it without the steps on the files `applied`, or in
/// folders among them, and then removes it. A kill in between leaves both, and the next start
/// removes the old one unread, as the new one names it (`jobs_cut_short`). The old log stays
/// whole when the new one cannot be written, and the error is returned.
fn written_anew_without(path: &Path, applied: &[PathBuf]) -> std::io::Result<()> {
    let crate::files::job_log::LogRead::Log { job, records, .. } =
        crate::files::job_log::read_job_log_without_lock(path)?
    else {
        return Err(std::io::Error::other("not a job log of this version"));
    };
    let dir = path.parent().unwrap_or(Path::new("/"));
    let mut left = records_without_steps(records, applied);
    // The `old-log` record comes last, so that a new log cut off by a kill while it is written
    // never names the old log, which then stays and is read again.
    left.push(Record::OldLog(path.to_path_buf()));
    let started = JobLog::start(dir, job, &left);
    if let Some(sure) = Sure::of(&started) {
        remove_job_log(path, sure);
    }
    started.map(drop)
}

/// Returns `records` without the steps on the files `applied`, or in folders among them, and
/// without the records of those steps. The other steps are numbered anew in their order. An
/// `old-log` record is kept, as the start may have failed to remove the log it names, which
/// would then be read again as a log of its own and give its marks and places a second time.
fn records_without_steps(records: Vec<Record>, applied: &[PathBuf]) -> Vec<Record> {
    let applied: HashSet<&Path> = applied.iter().map(PathBuf::as_path).collect();
    let mut new_numbers = HashMap::new();
    for record in &records {
        if let Record::Step { number, from, .. } = record
            && !from.ancestors().any(|a| applied.contains(a))
        {
            new_numbers.insert(*number, new_numbers.len());
        }
    }
    (records.into_iter())
        .filter_map(|mut record| {
            if let Some(number) = record.step_number_mut() {
                *number = *new_numbers.get(number)?;
            }
            Some(record)
        })
        .collect()
}

/// Gives `file`, one of a delete log's own, the time `time` that its delete wrote it last, on
/// disk.
fn keep_time(file: &File, time: std::time::SystemTime) -> std::io::Result<()> {
    #[allow(clippy::disallowed_methods)] // a delete log's own file, which keeps its delete's time
    file.set_modified(time)?;
    file.sync_all()
}

/// Returns the Move log `log` as a start found it and read it surely, as `sure` proves.
fn found_move_log(log: JobLog, sure: Sure) -> MoveLog {
    MoveLog {
        log: log.read_surely(sure),
        steps: 0,
        is_found_at_start: true,
        folders: HashSet::new(),
        failure: None,
    }
}

/// Returns the pairs of a Move's log, `pairs`, whose files went before Glimt was killed: those
/// whose old name is surely not there any more and whose new one is and holds the file that
/// went (`is_the_file_that_went`, by the fingerprints `known`, one for each pair). A file that
/// had not gone yet is still at its old name, and a pair cut off by the kill has names that are
/// not there, so both are left out. A sidecar that the kill left at a file's old name is moved
/// to the file (`sidecars_follow_the_files_that_went`). Returns `CannotTell` when a file may
/// have left its old name and its new one cannot be told (`is_gone_at_start`), as on a stick
/// not mounted at this start, and when its new name holds a file while its old name cannot be
/// told (`has_left`), so that the log stays for a later start.
fn moves_that_went(
    pairs: Moves,
    known: &[Option<FileFingerprint>],
    folders: &HashMap<PathBuf, FolderNoted>,
) -> AtStart<Moves> {
    let mut went = Vec::new();
    for (step, (from, to)) in pairs.into_iter().enumerate() {
        let left = has_left(&from);
        // A file at the old name in another folder than the log noted there, as on another card
        // at the same mount path, says nothing about the file that went: only its new name tells.
        let is_still_there = left == Ok(false) && !is_another_folder(folders, &from)?;
        if is_still_there || is_gone_at_start(&to, folders)? {
            continue;
        }
        left?;
        if is_the_file_that_went(&to, known, step)? {
            went.push((from, to));
        }
    }
    sidecars_follow_the_files_that_went(&went);
    Ok(went)
}

/// Returns whether a file left the old name `from` of a Move's step, which a failed look cannot
/// tell. A name in a folder that is gone counts as left, as a folder emptied by a Move is often
/// removed after it, and only the new name tells where the file went.
fn has_left(from: &Path) -> AtStart<bool> {
    match (is_surely_there(from), is_surely_gone(from)) {
        (true, _) => Ok(false),
        (_, true) => Ok(true),
        _ => Err(CannotTell),
    }
}

/// The job logs in `dir` that Glimt was killed during, which are those no running window holds
/// locked: the Moves (as `move_jobs_cut_short` gives them) and the deletes (as
/// `put_right_a_delete` gives them). A log in `dir` that another log there names in its
/// `old-log` record was written anew as that one, which a kill left beside it, and is removed
/// unread. So is a log of Glimt 0.92 or older that such a record names directly in one of the old
/// folders beside `dir` (`old_log_written_anew`), with its lists, as Glimt 0.93 and 0.94 wrote
/// it anew and a kill kept it. A path that such a record names anywhere else is no log, and
/// nothing is done to it: the folders of Glimt 0.92 and older are no longer read
/// (`clear_old_log_folders`).
pub fn jobs_cut_short(dir: &Path) -> JobsCutShort {
    // A log of a kind that `read_at_start` has no reader for is not found here: it is kept on
    // disk and told by `logs_left_by_a_kill`.
    let found = crate::files::job_log::logs_left_by_a_kill(dir);
    let written_anew: HashSet<PathBuf> = (found.iter())
        .flat_map(|found| &found.records)
        .filter_map(|record| match record {
            Record::OldLog(path) => Some(path.clone()),
            _ => None,
        })
        .collect();
    for path in &written_anew {
        if let Some(sure) = Sure::of(&old_log_written_anew(dir, path)) {
            remove_old_log_with_its_lists(path, sure);
        }
    }
    let (mut moves, mut deletes, mut others) = (Vec::new(), Vec::new(), Vec::new());
    // Whether a log stays because this start could not tell what its job had done.
    let mut is_any_kept = false;
    for found in found {
        if let Some(sure) = Sure::of(&is_named_anew(&written_anew, found.log.path())) {
            remove_job_log(found.log.path(), sure);
            continue;
        }
        match found.read_as {
            ReadAtStart::Move => match move_job_cut_short(found) {
                Some(done) => moves.push(done),
                None => is_any_kept = true,
            },
            ReadAtStart::Delete => {
                let time = found.log.file().metadata().and_then(|m| m.modified()).ok();
                let records = delete_records(found.job, &found.records);
                let jobs = dir.to_path_buf();
                let (log, gone, is_kept) = put_right_a_delete(found.log, records, time, jobs);
                is_any_kept |= is_kept;
                deletes.push((log, gone));
            }
            ReadAtStart::UndoTrash => {
                // A log whose files cannot be looked at now stays for a later start.
                let done = back_from_the_trash(&found.records);
                if let (Some(sure), Ok(done)) = (Sure::of(&done), done) {
                    others.push((found.log.read_surely(sure), done));
                } else {
                    is_any_kept = true;
                }
            }
            ReadAtStart::Folder => {
                // A log whose folder cannot be looked at now stays for a later start.
                let done = folder_job_done(&found.records);
                if let (Some(sure), Ok(done)) = (Sure::of(&done), done) {
                    others.push((found.log.read_surely(sure), done));
                } else {
                    is_any_kept = true;
                }
            }
            ReadAtStart::Rename => {}
        }
    }
    // A log kept without a word would stay for ever unseen, as on a card that the start can
    // never tell, with the marks of its files at their old names.
    if is_any_kept {
        crate::files::file_io::save_failed(crate::tf!(
            "{folder} holds logs of jobs that Glimt stopped during, which this start could not finish yet. They are kept, with the marks and stars of their files, for a later start.",
            folder = dir.display()
        ));
    }
    (moves, deletes, others)
}

/// Returns how the start reads a job log of the kind `job`, and `None` for a kind it has no
/// reader for. `every_job_kind_is_read_at_start` fails when a kind of `JOB_KINDS` has none.
pub(crate) fn read_at_start(job: &str) -> Option<ReadAtStart> {
    match job {
        "move" | "undo-move" => Some(ReadAtStart::Move),
        "delete" | "trash" => Some(ReadAtStart::Delete),
        "undo-trash" => Some(ReadAtStart::UndoTrash),
        "rename-folder" | "trash-folder" => Some(ReadAtStart::Folder),
        "rename" => Some(ReadAtStart::Rename),
        _ => None,
    }
}

/// The job logs in `dir` of Moves, and of Undos of Moves, that Glimt was killed during, as
/// `jobs_cut_short` finds them.
#[cfg(test)]
pub fn move_jobs_cut_short(dir: &Path) -> Vec<(MoveLog, Moves)> {
    jobs_cut_short(dir).0
}

/// The job logs in `dir` of deletes that Glimt was killed during, as `jobs_cut_short` finds
/// them.
#[cfg(test)]
pub fn delete_jobs_cut_short(dir: &Path) -> Vec<(DeleteLog, Vec<PathBuf>)> {
    jobs_cut_short(dir).1
}

/// Returns the Move's log `found`, with the files that went: each step whose new name holds
/// the file with the step's fingerprint (`moves_that_went`). Returns `None` when the start
/// cannot tell, and the log stays.
fn move_job_cut_short(found: crate::files::job_log::FoundLog) -> Option<(MoveLog, Moves)> {
    let (pairs, known): (Moves, Vec<Option<FileFingerprint>>) = (found.records.iter())
        .filter_map(|record| match record {
            Record::Step {
                from,
                to: Some(to),
                fingerprint,
                ..
            } => Some(((from.clone(), to.clone()), *fingerprint)),
            _ => None,
        })
        .unzip();
    let went = moves_that_went(pairs, &known, &folders_noted(&found.records));
    let sure = Sure::of(&went)?;
    Some((found_move_log(found.log, sure), went.ok()?))
}

/// Returns whether `path`, which a job log in `jobs` names in its `old-log` record, is a log of
/// Glimt 0.92 or older that may go: a file, or no name, directly in a folder of
/// `OLD_LOG_FOLDERS` beside `jobs`. Returns `CannotTell` for any other path, for a link, and when
/// the name cannot be looked at. The record is written after the whole log, so it is read only
/// from a log written anew whole.
fn old_log_written_anew(jobs: &Path, path: &Path) -> AtStart<()> {
    let state = jobs.parent().ok_or(CannotTell)?;
    let (Some(_), Some(folder)) = (path.file_name(), path.parent()) else {
        return Err(CannotTell);
    };
    if !OLD_LOG_FOLDERS
        .iter()
        .any(|name| state.join(name) == folder)
    {
        return Err(CannotTell);
    }
    if !matches!(crate::files::file_io::is_link(folder), Ok(false)) {
        return Err(CannotTell);
    }
    is_file_or_gone(path)
}

/// Returns `Ok` when `path`, not followed through a link, is a file or surely not there.
fn is_file_or_gone(path: &Path) -> AtStart<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_file() => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        _ => Err(CannotTell),
    }
}

/// Removes the log of Glimt 0.92 or older at `path` and the lists that it kept beside it, in
/// the folders of `OLD_LISTS_FOLDERS` of its own folder, by the log's name.
fn remove_old_log_with_its_lists(path: &Path, sure: Sure) {
    let (Some(name), Some(folder)) = (path.file_name(), path.parent()) else {
        return;
    };
    remove_job_log(path, sure);
    for lists in OLD_LISTS_FOLDERS {
        // A link where a folder of lists was is not followed, and a folder is no list: either
        // stays, and `clear_old_log_folders` tells it.
        let list = folder.join(lists).join(name);
        let is_list = matches!(
            crate::files::file_io::is_link(&folder.join(lists)),
            Ok(false)
        );
        if let (true, Some(sure)) = (is_list, Sure::of(&is_file_or_gone(&list))) {
            remove_job_log(&list, sure);
        }
    }
}

/// Returns whether a job log in `written_anew` names the log at `path` in its `old-log` record:
/// it was written anew as that one, whole, before it named it, so it may go unread.
fn is_named_anew(written_anew: &HashSet<PathBuf>, path: &Path) -> AtStart<()> {
    written_anew.contains(path).then_some(()).ok_or(CannotTell)
}

/// Returns what the records of a delete's job log, of kind `job`, say. The parser leaves out a
/// record about a step that the log does not have before it, so each number is a step's.
fn delete_records(job: &'static str, records: &[Record]) -> DeleteRecords {
    let mut read = DeleteRecords {
        job,
        ..Default::default()
    };
    for record in records {
        match record {
            Record::Step {
                from, fingerprint, ..
            } => {
                read.names.push(from.clone());
                read.known.push(*fingerprint);
            }
            Record::Shown(collection) => read.collection = Some(collection.clone()),
            Record::Aside {
                number,
                sidecar,
                hidden,
            } => (read.aside).push([read.names[*number].clone(), sidecar.clone(), hidden.clone()]),
            Record::Kept(number) => {
                read.kept.insert(read.names[*number].clone());
            }
            Record::Done(number) => {
                read.done.insert(read.names[*number].clone());
            }
            Record::Found(number) => {
                read.found.insert(read.names[*number].clone());
            }
            Record::OldLog(path) => read.old_logs.push(path.clone()),
            _ => {}
        }
    }
    read.folders = folders_noted(records);
    read
}

/// Puts right the delete whose log `log` says `records`, found at the start, last written at
/// `time`. Returns the log, held locked until it is removed or written anew in `jobs`, with the
/// files whose marks and stars are to be forgotten: those whose name is surely not there any
/// more in a folder that surely is (`is_gone_at_start`). Returns also whether the log stays for
/// a name that this start could not finish, other than a photo found gone for the first time,
/// which is told on its own.
///
/// A file that had not gone yet is still at its name, and keeps them. A file still there as
/// the log knew it takes back its sidecars, from a hidden name that a delete for good put them
/// aside under (`sidecars_back_from_hiding`) and from the trash (`sidecars_back_from_the_trash`).
/// A sidecar put aside of a file that went goes too. A photo that the delete kept
/// (`DeleteLog::keep_for_a_later_start`) never counts as one that went, because the user may
/// have moved it away after the delete.
///
/// A delete for good whose step has no `done` may have been cut by the kill before its photo
/// went or after. When its photo is surely gone, the start that first finds it keeps its marks,
/// its hidden sidecar and the log, writes down that it found it (`found`), and tells once. A
/// later start finishes the step: when the photo is still surely gone, its marks are forgotten
/// and its hidden sidecar deleted, and when it is there, it takes its sidecar back. Each is told
/// once. A look that fails keeps everything for the next start.
fn put_right_a_delete(
    log: JobLog,
    records: DeleteRecords,
    time: Option<std::time::SystemTime>,
    jobs: PathBuf,
) -> (DeleteLog, Vec<PathBuf>, bool) {
    use crate::files::file_io::save_failed;
    let r = &records;
    let is_unfinished = |name: &PathBuf| r.job == "delete" && !r.done.contains(name);
    let (mut gone, mut stayed, mut not_as_left) = (Vec::new(), Vec::new(), Vec::new());
    let (mut untold, mut found_now) = (HashSet::new(), HashSet::new());
    for (name, known) in r.names.iter().zip(&r.known) {
        let was_found = r.found.contains(name);
        // Another card at the same mount path holds none of this log's files, and a folder that
        // cannot be told may be such a card.
        if is_another_folder(&r.folders, name) != Ok(false) {
            untold.insert(name.clone());
            continue;
        }
        match is_gone_at_start(name, &r.folders) {
            Ok(true) if r.kept.contains(name) => not_as_left.push(name.clone()),
            Ok(true) if is_unfinished(name) && !was_found => {
                save_failed(crate::tf!(
                    "{photo} was being deleted for good when Glimt stopped. It is gone, but its marks and stars, and any sidecar put aside under a hidden name beside it, are kept until the next start.",
                    photo = name.display()
                ));
                untold.insert(name.clone());
                found_now.insert(name.clone());
            }
            Ok(true) => {
                if is_unfinished(name) {
                    save_failed(crate::tf!(
                        "{photo} is still gone, so its delete is finished: its marks and stars are forgotten, and its sidecar put aside is deleted.",
                        photo = name.display()
                    ));
                }
                gone.push(name.clone());
            }
            Ok(false) if is_as_known(name, *known) => {
                if is_unfinished(name) && was_found {
                    save_failed(crate::tf!(
                        "{photo} is there again, so it keeps its marks and stars, and its sidecar put aside gets its name back.",
                        photo = name.display()
                    ));
                }
                stayed.push(name.clone());
            }
            Ok(false) => not_as_left.push(name.clone()),
            Err(CannotTell) => {
                untold.insert(name.clone());
            }
        }
    }
    // The files that went stay in the log while the collection cannot be written, so that a
    // later start takes them out of it.
    if forget_in_the_collection_shown(r.collection.as_deref(), &gone).is_err() {
        untold.extend(gone.iter().cloned());
    }
    // Before the trash is looked in, where a sidecar put aside is not.
    let (still_hidden, unseen) = sidecars_back_from_hiding(&r.aside, &gone, &stayed, &not_as_left);
    untold.extend(still_hidden.iter().chain(&unseen).cloned());
    // A log whose time cannot be read takes no sidecar back, as it cannot tell which went
    // after it.
    (time.into_iter()).for_each(|since| sidecars_back_from_the_trash(&stayed, since));
    let kept: HashSet<&PathBuf> = r.kept.iter().chain(&still_hidden).collect();
    let found: HashSet<&PathBuf> = r.found.iter().chain(&found_now).collect();
    let is_kept = untold.len() > found_now.len();
    let records_left = steps_left(r, &untold, &kept, &found, log.path());
    // A log whose every name the start could tell about goes once the marks are forgotten.
    let is_told = untold.is_empty().then_some(()).ok_or(CannotTell);
    let log = match Sure::of(&is_told) {
        Some(sure) => log.read_surely(sure),
        None => log,
    };
    let log = DeleteLog {
        log,
        job: r.job,
        steps: HashMap::new(),
        folders: HashSet::new(),
        failure: None,
        has_names_left: !untold.is_empty(),
        written_anew: Some((
            jobs,
            time.unwrap_or_else(std::time::SystemTime::now),
            records_left,
        )),
    };
    (log, gone, is_kept)
}

/// Returns the records of the log written anew for the steps of `r` that are `untold`, numbered
/// anew: each step with its fingerprint, its sidecars put aside, and whether it is `kept`,
/// `done` or `found`. They come after the collection shown and before the `old-log` records:
/// those of `r`, and the one that names the log at `old`, which a kill may leave beside the new
/// one.
fn steps_left(
    r: &DeleteRecords,
    untold: &HashSet<PathBuf>,
    kept: &HashSet<&PathBuf>,
    found: &HashSet<&PathBuf>,
    old: &Path,
) -> Vec<Record> {
    let mut records: Vec<Record> = r
        .collection
        .clone()
        .map(Record::Shown)
        .into_iter()
        .collect();
    let left = (r.names.iter().zip(&r.known)).filter(|(name, _)| untold.contains(*name));
    let folders_left: HashSet<&Path> = left.clone().filter_map(|(n, _)| n.parent()).collect();
    for (path, noted) in &r.folders {
        if folders_left.contains(path.as_path()) {
            records.push(Record::Folder {
                path: path.clone(),
                device: noted.noted_device,
                inode: noted.noted_inode,
            });
            records.extend(noted.noted_volume.clone().map(|volume| Record::Volume {
                path: path.clone(),
                volume,
            }));
        }
    }
    for (number, (name, known)) in left.enumerate() {
        records.push(Record::Step {
            number,
            from: name.clone(),
            to: None,
            fingerprint: *known,
        });
        for [_, sidecar, hidden] in r.aside.iter().filter(|[photo, ..]| photo == name) {
            records.push(Record::Aside {
                number,
                sidecar: sidecar.clone(),
                hidden: hidden.clone(),
            });
        }
        let marks = [
            (kept.contains(name), Record::Kept(number)),
            (r.done.contains(name), Record::Done(number)),
            (found.contains(name), Record::Found(number)),
        ];
        records.extend(marks.into_iter().filter(|(is, _)| *is).map(|(_, r)| r));
    }
    // The logs that the old log names stay named, as the start may have failed to remove one,
    // which would then be read again as a log of its own.
    records.extend(r.old_logs.iter().cloned().map(Record::OldLog));
    // The `old-log` record of the old log comes last, so that a new log cut off by a kill while
    // it is written never names the old log, which then stays and is read again.
    records.push(Record::OldLog(old.to_path_buf()));
    records
}

/// Writes the log of a delete's steps left, `records`, anew in the folder of the job logs
/// `dir` as a job of kind `job`, on disk with the time `time` of the log at `old` that it takes
/// the place of, and then removes that one. A kill in between leaves both, and
/// the next start removes the old one unread, as the new one names it (`jobs_cut_short`).
fn write_anew(
    dir: &Path,
    job: &str,
    time: std::time::SystemTime,
    records: &[Record],
    old: &Path,
) -> std::io::Result<()> {
    let (_path, file) = JobLog::start(dir, job, records)?.into_path_and_file();
    // The old log goes only once the new one is whole on disk, with its time.
    let written = keep_time(&file, time);
    if let Some(sure) = Sure::of(&written) {
        remove_job_log(old, sure);
    }
    drop(file);
    written
}

/// Takes the files `gone` out of the `collection` that was shown during a delete, as the delete
/// would have at its end. A log without one, or of a Glimt before this
/// was written down, leaves the entries. So does a collection that is surely gone, as there is
/// nothing to take them out of. A failure to write it is told, and returns `CannotTell`, so
/// that the log stays for a later start.
fn forget_in_the_collection_shown(collection: Option<&Path>, gone: &[PathBuf]) -> AtStart<()> {
    let Some(collection) = collection else {
        return Ok(());
    };
    if gone.is_empty() || is_surely_gone(collection) {
        return Ok(());
    }
    let forgotten = crate::files::file_ops::forget_in_collection(Some(collection), gone);
    forgotten.map(drop).map_err(|e| {
        crate::files::file_io::save_failed(e);
        CannotTell
    })
}

/// Puts right each sidecar that a delete for good put aside under a hidden name, as its log
/// notes them, `noted`, as (photo, sidecar, hidden name) (`DeleteLog::note_aside`), when
/// Glimt was killed before the delete deleted it or took it back. It comes back beside its
/// photo, never over a file, when
/// the photo is among those that `stayed`: still there as the log's list knew it, by the same
/// look that takes a sidecar back from the trash. It is deleted when the photo is among those
/// `gone`, as the delete would have done. Any other stays hidden: a photo that cannot be told
/// about yet, whose log stays too, and a different file that took the photo's name since,
/// which must not get the keywords of the photo deleted. A photo that is not there as the
/// delete left it any more, among those `not_as_left`, keeps its sidecar hidden, and the hidden
/// name is told, once, as the log goes: a photo the delete kept and the user moved away since,
/// and a photo changed or written anew since. Only a hidden name that `file_io::tmp_path`
/// gives, beside the photo, is touched. A failure is told. Returns the photos whose sidecar may
/// still be hidden, as its rename back failed in a way that a later start may not meet, so that
/// the log stays for them. Returns too the photos, among those that went or are not as left,
/// whose hidden name could not be looked at, as on a share that stopped answering for a moment,
/// so that the log stays for a later start that can.
fn sidecars_back_from_hiding(
    noted: &[[PathBuf; 3]],
    gone: &[PathBuf],
    stayed: &[PathBuf],
    not_as_left: &[PathBuf],
) -> (Vec<PathBuf>, Vec<PathBuf>) {
    use crate::files::file_io::{TMP_PREFIX, save_failed};
    use std::collections::HashSet;
    let (mut still_hidden, mut unseen) = (Vec::new(), Vec::new());
    let (gone, stayed, not_as_left): (HashSet<&PathBuf>, HashSet<&PathBuf>, HashSet<&PathBuf>) = (
        gone.iter().collect(),
        stayed.iter().collect(),
        not_as_left.iter().collect(),
    );
    let photos: Vec<PathBuf> = noted.iter().map(|[photo, _, _]| photo.clone()).collect();
    let _locks = crate::metadata::xmp::lock_sidecars(&photos);
    for [photo, side, hidden] in noted {
        let folder = photo.parent();
        let is_hidden_beside = (hidden.file_name())
            .is_some_and(|name| name.as_bytes().starts_with(TMP_PREFIX.as_bytes()))
            && hidden.parent() == folder
            && side.parent() == folder;
        if !is_hidden_beside {
            continue;
        }
        if !is_surely_there(hidden) {
            // A look that failed is no answer. A photo that stayed is written down as kept, so
            // that a later start that finds it moved away does not delete its sidecar.
            if !is_surely_gone(hidden) {
                if stayed.contains(photo) {
                    still_hidden.push(photo.clone());
                } else if gone.contains(photo) || not_as_left.contains(photo) {
                    unseen.push(photo.clone());
                }
            }
            continue;
        }
        let put_right = if stayed.contains(photo) {
            if give_back_from_hiding(photo, side, hidden) {
                still_hidden.push(photo.clone());
            }
            continue;
        } else if gone.contains(photo) {
            #[allow(clippy::disallowed_methods)] // Glimt's own: put aside, of a photo deleted
            std::fs::remove_file(hidden)
        } else if not_as_left.contains(photo) {
            save_failed(crate::tf!(
                "{photo} is not there as the delete left it, so its sidecar {sidecar} stays under the hidden name {hidden}",
                photo = photo.display(),
                sidecar = side.display(),
                hidden = hidden.display()
            ));
            continue;
        } else {
            continue;
        };
        if let Err(e) = put_right {
            save_failed(format!("{}: {e}", side.display()));
        }
    }
    (still_hidden, unseen)
}

/// Gives the sidecar `side` of `photo`, a photo that stayed, back its own name from the name
/// `hidden` that a delete for good put it aside under, never over a file. It is on disk before
/// the log goes, as a power cut would otherwise leave it hidden. A sidecar under both names,
/// which a share that linked it and lost the answer of the unlink leaves, loses its hidden name.
/// One whose own name another file has taken since, such as a sidecar written for the photo
/// after the delete, stays hidden, and both names are told. Returns whether the sidecar may
/// still be hidden after an error that a later start may not meet, such as a share that did not
/// answer (`may_have_been_done`), so that the log stays for it. A lasting refusal, such as a
/// disk mounted read-only, is told once with the hidden name, as every later start would only
/// tell it again until the user gives the sidecar back by hand. Each failure is told.
fn give_back_from_hiding(photo: &Path, side: &Path, hidden: &Path) -> bool {
    use crate::files::file_io::{rename_never_over, save_failed, sync_folder_of};
    use std::io::ErrorKind::AlreadyExists;
    // Glimt's own: the sidecar it put aside under its temporary name, given back.
    let given_back = match rename_never_over(hidden, side) {
        Ok(()) => sync_folder_of(side),
        // A second name of the sidecar, which `rename_step` takes away as Rename Many's does.
        Err(e) if e.kind() == AlreadyExists && same_file_by_opening(hidden, side) => {
            rename_step(hidden, side)
        }
        Err(e) if e.kind() == AlreadyExists => {
            save_failed(crate::tf!(
                "The name {sidecar} is taken by another file now, so the sidecar put aside stays under the hidden name {hidden}",
                sidecar = side.display(),
                hidden = hidden.display()
            ));
            return false;
        }
        Err(e) => {
            let may_have_been_done = crate::files::file_io::may_have_been_done(&e, hidden);
            let e = may_be_hidden(e, photo, hidden);
            save_failed(format!("{}: {e}", side.display()));
            return may_have_been_done;
        }
    };
    if let Err(e) = given_back {
        save_failed(format!("{}: {e}", side.display()));
    }
    false
}

/// Returns the error `e` of a rename that was to hide a sidecar of `photo` under the name
/// `hidden`, or to give it back its own name from there, with that hidden name in it. The error
/// may mean that the rename was done all the same, on a share that lost its answer, so the
/// sidecar may be under either name, and nothing guesses which.
pub(crate) fn may_be_hidden(e: std::io::Error, photo: &Path, hidden: &Path) -> std::io::Error {
    std::io::Error::new(
        e.kind(),
        crate::tf!(
            "it may be under the hidden name {hidden} now; if {photo} is still there and this sidecar is not at its own name, give it back that name ({e})",
            hidden = hidden.display(),
            photo = photo.display(),
            e
        ),
    )
}

/// Returns whether the file at `path` has the size and time of the fingerprint `known`, which a
/// delete's log took before the file was to go. The device is left out, as after a reboot.
/// Returns false when there is no fingerprint or the look fails.
fn is_as_known(path: &Path, known: Option<FileFingerprint>) -> bool {
    let now = crate::files::file_io::fingerprint_or_error(path);
    now.is_ok_and(|(_, size, time)| known.is_some_and(|(_, was, at)| (size, time) == (was, at)))
}

/// Takes back from the trash the sidecars of the files `stayed`, which a delete's log names and
/// which are still there as they were, when they went to the trash about the time `since`, when
/// the log was last written. A Move to Trash sends each photo's sidecars just before the photo,
/// so Glimt killed between the two left the photo without its keywords, comment and alt text.
/// One trashed later is one the user trashed on purpose, also when the log is read again at a
/// start weeks later (`trash::restore_until`). Only a sidecar whose name is surely free is
/// looked for, and none comes back over a file. Adobe's sidecar comes back only to a RAW file
/// that owns it alone (`xmp::shares_its_adobe_stem`). One that cannot come back is told.
fn sidecars_back_from_the_trash(stayed: &[PathBuf], since: std::time::SystemTime) {
    use crate::metadata::xmp;
    let mut names = xmp::AdobeNames::default();
    // Adobe's sidecar has the RAW file's name stem, with the extension in either case.
    let sidecars: Vec<PathBuf> = (stayed.iter())
        .flat_map(|p| {
            let adobe = (xmp::adobe_stem(p))
                .filter(|_| !xmp::shares_its_adobe_stem(p, &mut names))
                .map(|_| ["xmp", "XMP"].map(|e| p.with_extension(e)));
            std::iter::once(xmp::sidecar(p)).chain(adobe.into_iter().flatten())
        })
        .filter(|side| is_surely_gone(side))
        .collect();
    if sidecars.is_empty() {
        return;
    }
    let _locks = xmp::lock_sidecars(stayed);
    let trash_move = crate::files::trash::TrashMove {
        out_without_answer: Default::default(),
        files: Vec::new(),
        sidecars,
        sidecars_left: Vec::new(),
        since,
        marks: Default::default(),
        collection: None,
    };
    let trash_dirs = crate::files::trash::trash_dirs;
    let restored = crate::files::trash::restore_until(&trash_move, trash_dirs, since);
    if let (_, Some(error)) = restored {
        crate::files::file_io::save_failed(error);
    }
}

/// Returns the `folder` record of `folder`, with its device and inode now, and `None` where the
/// look fails. The inode is 0, which no folder has, where its file system does not keep a
/// folder's inode across mounts (FAT, exFAT, `has_lasting_inodes`) or its type is not known.
/// Then only the device is compared.
///
/// A `volume` record follows it with the volume the folder is on (`volume_of`), on any file
/// system on a block device that has a name in /dev/disk/by-uuid (FAT, ext4, NTFS, ...), so that
/// a start can tell the card or stick from another one at the same path, and find it again under
/// another device number. A share, FUSE without a block device and tmpfs have no such name, get
/// no `volume` record, and are told by their device, as before.
fn folder_record(folder: &Path) -> Vec<Record> {
    let kind = crate::files::file_ops::file_system_type_or_test(folder);
    let is_lasting = (kind.as_deref()).is_some_and(crate::files::file_io::has_lasting_inodes);
    let Some((Some((device, inode)), _, _)) =
        crate::files::file_io::fingerprint_of_file_read(folder)
    else {
        return Vec::new();
    };
    let mut records = vec![Record::Folder {
        path: folder.to_path_buf(),
        device,
        inode: if is_lasting { inode } else { 0 },
    }];
    if let Some(volume) = volume_of(folder, device) {
        let path = folder.to_path_buf();
        records.push(Record::Volume { path, volume });
    }
    records
}

/// Returns the volume that `folder`, on the device `device`, is on
/// (`file_io::volume_of_device`). A test gives it with `TEST_VOLUMES` instead, and gets `None`
/// for a folder it gave none. It waits for the disk.
fn volume_of(folder: &Path, device: u64) -> Option<std::ffi::OsString> {
    #[cfg(test)]
    {
        let _ = device;
        VOLUME_LOOKS.set(VOLUME_LOOKS.get() + 1);
        TEST_VOLUMES.with_borrow(|volumes| {
            (volumes.iter().find(|(at, _)| folder.starts_with(at))).map(|(_, v)| v.clone())
        })
    }
    #[cfg(not(test))]
    {
        let _ = folder;
        crate::files::file_io::volume_of_device(device)
    }
}

/// Returns the folders that the `folder` records among `records` noted, each with the volume
/// that a `volume` record of the same folder noted.
fn folders_noted(records: &[Record]) -> HashMap<PathBuf, FolderNoted> {
    let volumes_now = std::sync::Arc::new(std::sync::OnceLock::new());
    let mut folders: HashMap<PathBuf, FolderNoted> = (records.iter())
        .filter_map(|record| match record {
            Record::Folder {
                path,
                device,
                inode,
            } => Some((
                path.clone(),
                FolderNoted {
                    noted_device: *device,
                    noted_inode: *inode,
                    noted_volume: None,
                    volumes_now: volumes_now.clone(),
                },
            )),
            _ => None,
        })
        .collect();
    for record in records {
        if let Record::Volume { path, volume } = record
            && let Some(noted) = folders.get_mut(path)
        {
            noted.noted_volume = Some(volume.clone());
        }
    }
    folders
}

/// Returns whether `folder`, on the device `device` now and with the inode `inode`, is on the
/// disk or card that its records `noted` saw: the one rule by which a start tells a volume.
/// `inode` is `None` for a folder above the one noted, whose own inode was never noted. Returns
/// `Ok(false)` only for surely another disk, and `CannotTell` where it cannot be told, so that
/// the log and the marks stay for a later start, never forgotten on a guess.
///
/// A noted volume decides, not the device, as a stick plugged in again may get another device
/// number and another card in the same reader gets the same one. It is surely the same only
/// when its block device is looked up afresh and is the folder's now. The volumes were read once
/// for the log, so a card swapped since under the same device number would else pass for the
/// one noted (#284). A volume not found now cannot tell.
///
/// Without a volume, another device cannot tell, as then the folder noted is not here (an empty
/// mount point) or came back under another device number. On the noted device, the folder's own
/// lasting inode, when it is the one noted, tells the disk. Without it, the device number tells
/// it only off a block device: a share's or tmpfs's number is its own, while a block device's
/// is given to the next card in the same reader or stick at the same port, so that it alone
/// cannot tell (#179, #266, #270). Where no volume can be read on this system, as in the
/// Flatpak's sandbox, which has no /dev/disk/by-uuid, and no lasting inode tells the folder (FAT,
/// exFAT, or a folder above the one noted), the device tells it, as in Glimt 0.96. Else a job
/// killed on such a card would never finish, and its files' marks would never follow them.
fn is_on_the_volume_noted(
    folder: &Path,
    noted: &FolderNoted,
    device: u64,
    inode: Option<u64>,
) -> AtStart<bool> {
    if let Some(volume) = &noted.noted_volume {
        let volumes = noted.volumes_now.get_or_init(VolumesNow::read).as_ref();
        let found = (volumes.and_then(|volumes| volumes.volume_of_folder(folder, device)))
            .ok_or(CannotTell)?;
        if found != *volume {
            return Ok(false);
        }
        return match VolumesNow::is_still_on_device(volume, folder, device) {
            true => Ok(true),
            false => Err(CannotTell),
        };
    }
    let is_the_inode_noted = noted.noted_inode != 0 && inode == Some(noted.noted_inode);
    let has_no_inode_to_tell = noted.noted_inode == 0 || inode.is_none();
    let has_no_volumes = || noted.volumes_now.get_or_init(VolumesNow::read).is_none();
    match device == noted.noted_device
        && (is_the_inode_noted
            || !is_on_a_block_device(folder, device)
            || (has_no_inode_to_tell && has_no_volumes()))
    {
        true => Ok(true),
        false => Err(CannotTell),
    }
}

/// Returns whether `folder`, on the device `device` now and with the inode `inode`, is the
/// folder that its records `noted` saw: the one rule by which a start tells it. `inode` is `None`
/// for a folder above the one noted, whose own inode was never noted.
///
/// It must be on the disk noted (`is_on_the_volume_noted`), and have the inode noted where one
/// was (not 0), so that a folder made again at the name is not taken for it. Returns `Ok(false)`
/// for another disk, surely another card at the same path, and for another folder: one with
/// another lasting inode on the noted volume or device, made at the name after the job. Returns
/// `CannotTell` where the disk cannot be told.
fn is_the_folder_noted(
    folder: &Path,
    noted: &FolderNoted,
    device: u64,
    inode: Option<u64>,
) -> AtStart<bool> {
    let is_another_inode = inode.is_some_and(|i| noted.noted_inode != 0 && i != noted.noted_inode);
    match is_on_the_volume_noted(folder, noted, device, inode) {
        Ok(is_the_disk) => Ok(is_the_disk && !is_another_inode),
        // On the noted device, another lasting inode is surely another folder, whichever disk
        // holds it.
        Err(CannotTell) if device == noted.noted_device && is_another_inode => Ok(false),
        Err(e) => Err(e),
    }
}

/// Returns whether the device `device` of `folder` is a block device, a disk, card or stick,
/// whose number the next one in the same reader or port gets. A test gives a folder one with
/// `ON_A_BLOCK_DEVICE`.
fn is_on_a_block_device(folder: &Path, device: u64) -> bool {
    #[cfg(test)]
    {
        let _ = device;
        ON_A_BLOCK_DEVICE.with_borrow(|on| on.iter().any(|at| folder.starts_with(at)))
    }
    #[cfg(not(test))]
    {
        let _ = folder;
        // A share, FUSE without a disk, tmpfs, overlay and btrfs get numbers of major 0.
        libc::major(device) != 0
    }
}

/// Returns the device and the inode that `folder` has now, and `None` when the look fails.
fn device_and_inode_now(folder: &Path) -> Option<(u64, u64)> {
    let (key, _, _) = crate::files::file_io::fingerprint_of_file_read(folder)?;
    let (device, inode) = key?;
    #[cfg(test)]
    if ON_ANOTHER_DEVICE.with_borrow(|moved| moved.iter().any(|m| m == folder)) {
        return Some((device.wrapping_add(1), inode));
    }
    Some((device, inode))
}

/// Returns whether the folder of `path` is another folder than the one that the log's `folders`
/// saw there, made at its name after the job (`is_the_folder_noted`). A folder the log has no
/// record of, one that is surely gone, and one whose file system gives no inode, are not counted
/// as another, and a folder on another volume, another card at the same path, is. Returns
/// `CannotTell` when the look at the folder fails, when its volume cannot be found, or when it is
/// on another device without a noted volume, so that the log stays for a later start. Before,
/// both counted as the same folder, and a file of another card at the path as the one that
/// stayed.
fn is_another_folder(folders: &HashMap<PathBuf, FolderNoted>, path: &Path) -> AtStart<bool> {
    let Some((folder, noted)) = path.parent().and_then(|f| Some((f, folders.get(f)?))) else {
        return Ok(false);
    };
    let Some((now, _, _)) = crate::files::file_io::fingerprint_of_file_read(folder) else {
        return match is_surely_gone(folder) {
            true => Ok(false),
            false => Err(CannotTell),
        };
    };
    Ok(match now {
        Some((device, inode)) => !is_the_folder_noted(folder, noted, device, Some(inode))?,
        None => false,
    })
}

/// Returns whether nothing is at `path` now, the one rule by which a start reads a path that a
/// job log names, whose `folder` records noted `folders`. Returns `Ok(true)` when its name is
/// surely not there while its folder surely is, on the disk the log noted, and `Ok(false)`
/// when something is at its name. A folder that is gone counts as deleted, and its files as
/// gone, when the nearest folder above it is on the disk the log noted
/// (`removed_from_its_device`). Returns `CannotTell` when the looks cannot tell: the folder is
/// not there, as on a stick or a share not back yet at this start, or it is not surely on the
/// disk the log noted (`is_on_the_volume_noted`), or the log noted none of it, or a look failed.
/// Such a log stays for a later start, as a path taken for gone or for not there would lose the
/// marks of its file.
///
/// The device tells the folder that the job wrote to from an empty mount point at its path, such
/// as `/mnt/nas` while its share is not mounted, which is a folder too. A stick plugged in again
/// with another device number cannot be told either, and its log waits for a later start: its
/// marks are kept, never lost. A disk with a volume noted is told by it instead, whatever its
/// device, and a block device without one only by the folder's own lasting inode. A log that
/// noted no folder at all is of Glimt 0.92, or written anew from one, and only the folder's
/// being there is asked of it, as before.
///
/// For a delete's log, a file at the name keeps its marks also when it changed since. It may be
/// the file the kill came before, written by another program since, or a file that another
/// window marked after the kill. The marks of a deleted file then stay with a later photo that
/// took its name, as they did before there was a log, which is the lesser harm.
fn is_gone_at_start(path: &Path, folders: &HashMap<PathBuf, FolderNoted>) -> AtStart<bool> {
    if is_surely_there(path) {
        return Ok(false);
    }
    if !folders.is_empty() {
        let folder = path.parent().ok_or(CannotTell)?;
        let noted = folders.get(folder).ok_or(CannotTell)?;
        if is_surely_gone(folder) {
            return removed_from_its_device(folder, noted);
        }
        let (device, inode) = device_and_inode_now(folder).ok_or(CannotTell)?;
        if !is_on_the_volume_noted(folder, noted, device, Some(inode))? {
            return Err(CannotTell);
        }
    }
    // The folder is looked at first, so that a stick mounted between the two looks counts as
    // not there yet.
    let has_folder = path.parent().is_some_and(is_surely_dir);
    match has_folder && is_surely_gone(path) {
        true => Ok(true),
        false => Err(CannotTell),
    }
}

/// Tells whether `folder`, which is surely gone, was removed from the disk it was noted on.
/// Returns `Ok(true)` when the nearest folder above it that surely is there is surely on that
/// disk (`is_on_the_volume_noted`), as then the folder was deleted, not taken away with a stick
/// or share. Returns `CannotTell` when that folder is on another device, as an empty mount point
/// is, on another volume, as another card in the same reader is, on a block device without a
/// noted volume, which another stick at the same port shares (#270), or when a look fails.
fn removed_from_its_device(folder: &Path, noted: &FolderNoted) -> AtStart<bool> {
    let mut above = folder.parent();
    while let Some(ancestor) = above {
        if is_surely_dir(ancestor) {
            let (device, _) = device_and_inode_now(ancestor).ok_or(CannotTell)?;
            let same = is_on_the_volume_noted(ancestor, noted, device, None)?;
            return same.then_some(true).ok_or(CannotTell);
        }
        if !is_surely_gone(ancestor) {
            return Err(CannotTell);
        }
        above = ancestor.parent();
    }
    Err(CannotTell)
}

/// Returns whether the file at `to` is the file that the pair `step` of a Move's log moved
/// there, by the fingerprints of its steps, `known`. Another program may have put another photo
/// there since, and a log that outlived its Move (its removal failed) is found again at the
/// next start, so the marks and the sidecar of the file that went would else go to a later
/// photo of that name. A file is told by its size and time, which a move to another disk keeps
/// (`put_file`) to the resolution of that disk's file system, so the times are compared as any
/// file system keeps them (`KnownAs::size_and_time_as_kept`). A pair left alone is told.
/// Returns `CannotTell` when the look at `to` fails, so that the log stays for a later start.
fn is_the_file_that_went(
    to: &Path,
    known: &[Option<FileFingerprint>],
    step: usize,
) -> AtStart<bool> {
    use crate::files::file_io::{KnownAs, changed_meanwhile, is_still_the_file};
    let is_it = match known.get(step).copied().flatten() {
        Some(fingerprint) => is_still_the_file(to, KnownAs::size_and_time_as_kept(fingerprint)),
        None => Err(changed_meanwhile()),
    };
    if let Err(e) = &is_it {
        can_be_looked_at(to)?;
        crate::files::file_io::save_failed(format!("{}: {e}", to.display()));
    }
    Ok(is_it.is_ok())
}

/// Looks at `path`. Returns `CannotTell` when the look fails: nothing can be told of it,
/// neither that it is gone nor its fingerprint.
fn can_be_looked_at(path: &Path) -> AtStart<()> {
    match crate::files::file_io::fingerprint(path).is_some() || is_surely_gone(path) {
        true => Ok(()),
        false => Err(CannotTell),
    }
}

/// Moves each sidecar that a Move killed between a photo and its sidecars left at the photo's
/// old name to the photo, for the files that `went`, as the Move would have. Otherwise the next
/// file of that old name would pick up the keywords. A sidecar never goes over one at the new
/// name (`xmp::follow`), and one that cannot follow is told as a failed write.
fn sidecars_follow_the_files_that_went(went: &[(PathBuf, PathBuf)]) {
    use crate::metadata::xmp;
    let moving: std::collections::HashSet<&PathBuf> = went.iter().map(|(from, _)| from).collect();
    let mut names = xmp::AdobeNames::default();
    let mut folders = xmp::FolderIds::new();
    for (from, to) in went {
        let _lock = xmp::lock_sidecar_among(from, &mut folders);
        // The file stays at `to`, so the sidecars that followed stay with it. One copied, as it
        // could not be moved, is still at the old name too, and is told.
        let (failed, left_behind) = match xmp::follow(from, to, false, &moving, &mut names) {
            Ok(left_behind) => (Vec::new(), left_behind),
            Err(not) => (not.failed, not.left_behind),
        };
        for (_, at, error) in failed {
            crate::files::file_io::save_failed(format!("{}: {error}", at.display()));
        }
        for left in left_behind {
            crate::files::file_io::save_failed(left.told());
        }
    }
}

/// Writes `moves` down in `dir`, the folder of the job logs, as a job log of the kind "rename",
/// locked and on disk with its name, before the first of them is done. Each step has the
/// fingerprint of the photo it moves (`known_fingerprints`). Returns the plan, or the error
/// when it cannot be written, and the renames are then not started: without the plan, a crash
/// halfway would leave names that no start puts right, and the marks file, in the same folder,
/// could not follow the new names either. A plan not written whole is removed
/// (`JobLog::start`), because finishing a plan left half written would undo renames done
/// without it.
pub fn start_journal(dir: &Path, moves: &[(PathBuf, PathBuf)]) -> std::io::Result<Journal> {
    let names = one_name_per_folder(moves);
    // The folders of the renames come first, so that a start can tell them from an empty mount
    // point at their path (`is_gone_at_start`).
    let mut records: Vec<Record> = (names.iter())
        .filter_map(|name| name.parent())
        .flat_map(folder_record)
        .collect();
    records.extend(rename_steps(moves, &known_fingerprints(moves)));
    Ok(Journal {
        log: JobLog::start(dir, "rename", &records)?,
        done: 0,
        one_name_per_folder: names,
    })
}

/// Returns the `step` records of the renames `moves`, each with the fingerprint at its place
/// in `known`, and with none where `known` has none.
fn rename_steps(moves: &[(PathBuf, PathBuf)], known: &[Option<FileFingerprint>]) -> Vec<Record> {
    (moves.iter().enumerate())
        .map(|(number, (from, to))| Record::Step {
            number,
            from: from.clone(),
            to: Some(to.clone()),
            fingerprint: known.get(number).copied().flatten(),
        })
        .collect()
}

/// Returns the photo each of `moves` moves, by its fingerprint now: one for each step, and
/// `None` where no file is there and for a sidecar. A sidecar follows its photo by name and is
/// not tied to its file, because Glimt itself writes it anew when a keyword is saved after
/// "Later", and the repair would then be refused at every start. A step that moves on a file
/// that an earlier step moved (the second step of a swap) names that file.
fn known_fingerprints(moves: &[(PathBuf, PathBuf)]) -> Vec<Option<FileFingerprint>> {
    let is_sidecar = |p: &Path| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("xmp"));
    let mut made: HashMap<&Path, Option<FileFingerprint>> = HashMap::new();
    let mut fingerprints = Vec::new();
    for (a, b) in moves {
        let known = match made.get(a.as_path()) {
            Some(known) => *known,
            None if is_sidecar(a) => None,
            None => crate::files::file_io::fingerprint(a),
        };
        made.insert(b, known);
        fingerprints.push(known);
    }
    fingerprints
}

/// Returns the fingerprint `known` of a step's photo as a line of text, with its line break, as
/// the lists of Glimt 0.92 and older had it. A job log writes a fingerprint as this line without
/// its line break.
pub(crate) fn known_line(known: Option<FileFingerprint>) -> String {
    match known {
        Some((Some((dev, ino)), size, (s, ns))) => format!("{dev} {ino} {size} {s} {ns}\n"),
        Some((None, size, (s, ns))) => format!("- - {size} {s} {ns}\n"),
        None => "-\n".to_owned(),
    }
}

/// Notes in the plans cut short that the photo at `path`, which Glimt has just written anew in
/// its place (a turn saved, a copy saved over it), now has the fingerprint it has after the
/// write, where their steps (`known_fingerprints`) knew it by `before`. The repair would else
/// take Glimt's own edit for another program's, and refuse to rename the photo at every start.
/// A step's photo is known by its size and time, and by its name before or after the step. A
/// list that cannot be written is told.
pub fn note_written_anew(path: &Path, before: FileFingerprint) {
    if let Some(dir) = crate::files::job_log::jobs_dir() {
        note_written_anew_in_job_logs(&dir, path, before);
    }
}

/// `note_written_anew` for the plans cut short in the folder of the job logs `dir`. A `written`
/// record is added to each plan whose step moves the photo known by `before`, as a job log is
/// never written over. A plan that a window holds locked is passed over: a running Rename Many
/// took its fingerprints after the save, which held the photo's sidecar lock that Rename Many
/// waits for, and one being put right has read them already. A failed write is told.
fn note_written_anew_in_job_logs(dir: &Path, path: &Path, before: FileFingerprint) {
    use crate::files::job_log::{LogRead, read_job_log_without_lock};
    let mut now = None;
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        // A pipe or a link planted in the state folder is no plan.
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        // It is looked at without its lock first, so that a save waits for no running job's.
        let Ok(LogRead::Log { job, records, .. }) = read_job_log_without_lock(&entry.path()) else {
            continue;
        };
        let Some((moves, _, known)) = rename_records(job, &records) else {
            continue;
        };
        let is_it =
            |k: &Option<FileFingerprint>| k.is_some_and(|k| (k.1, k.2) == (before.1, before.2));
        let numbers: Vec<usize> = (moves.iter().zip(&known).enumerate())
            .filter(|(_, ((a, b), known))| (a == path || b == path) && is_it(known))
            .map(|(number, _)| number)
            .collect();
        if numbers.is_empty() {
            continue;
        }
        // The photo is looked at only when a plan moves it, and once.
        let fingerprint = *now.get_or_insert_with(|| crate::files::file_io::fingerprint(path));
        let Some(fingerprint) = fingerprint else {
            return;
        };
        let written: Vec<Record> = (numbers.into_iter())
            .map(|number| Record::Written {
                number,
                fingerprint,
            })
            .collect();
        // A plan that a window holds now, running or being put right, is passed over.
        let Ok(file) = crate::files::file_io::open_if_unlocked(&entry.path(), false) else {
            continue;
        };
        if let Err(e) = JobLog::found(entry.path(), file).add_records(&written) {
            crate::files::file_io::save_failed(crate::tf!(
                "Could not save {name}: {e}",
                name = entry.path().display(),
                e
            ));
        }
    }
}

/// Returns what the records of the job log of a job of kind `job` say, when it is a Rename
/// Many's: its renames, how many of them were done, and the fingerprint of each step's photo,
/// that of its last `written` record where it has one. Returns `None` for a log of another
/// kind of job, and for one with a step with no new name, which no Rename Many writes.
fn rename_records(
    job: &str,
    records: &[Record],
) -> Option<(Moves, usize, Vec<Option<FileFingerprint>>)> {
    if job != "rename" {
        return None;
    }
    let (mut moves, mut known, mut done) = (Vec::new(), Vec::new(), HashSet::new());
    for record in records {
        match record {
            Record::Step {
                from,
                to,
                fingerprint,
                ..
            } => {
                moves.push((from.clone(), to.clone()?));
                known.push(*fingerprint);
            }
            Record::Done(number) => {
                done.insert(*number);
            }
            // The parser leaves out a record about a step that the log does not have before it.
            Record::Written {
                number,
                fingerprint,
            } => known[*number] = Some(*fingerprint),
            _ => {}
        }
    }
    // The renames are done in order, so the count is that of the first ones done.
    let done = (0..moves.len()).take_while(|n| done.contains(n)).count();
    Some((moves, done, known))
}

/// Returns the fingerprint that a line of a list of files (`known_line`) names, and `None` for
/// "-" and for a damaged line.
pub(crate) fn parse_known_line(line: &str) -> Option<FileFingerprint> {
    let parts: Vec<&str> = line.split(' ').collect();
    let [dev, ino, size, s, ns] = parts[..] else {
        return None;
    };
    let id = match (dev, ino) {
        ("-", "-") => None,
        _ => Some((dev.parse().ok()?, ino.parse().ok()?)),
    };
    Some((id, size.parse().ok()?, (s.parse().ok()?, ns.parse().ok()?)))
}

/// Returns the first name of `steps` (from, the step's file) that holds another file than the
/// step's own, with the error to tell. A file is told by its size and time, not by its device,
/// which a share, a FUSE mount or a card reader gets anew after the reboot the plan is kept
/// for. Its time is compared to the nanosecond, as a rename keeps it, so a file of the same
/// size written a moment later is still another file. A name that is not there is left to the
/// step itself (`repair_step`), and so is a swap's
/// hidden name. A name whose look failed is refused, as it may be another file all the same.
/// Each name is looked at before any step is taken, as no earlier step of the repair fills a
/// name of the user's that a later step takes a file from.
fn another_file_at<'a>(
    mut steps: impl Iterator<Item = (&'a Path, Option<FileFingerprint>)>,
) -> Option<(&'a Path, std::io::Error)> {
    use crate::files::file_io::{KnownAs, is_still_the_file};
    steps.find_map(|(from, known)| {
        let hidden = (from.file_name()?.as_bytes()).starts_with(HIDDEN_NAME.as_bytes());
        let error = is_still_the_file(from, KnownAs::size_and_time(known.filter(|_| !hidden)?));
        error
            .err()
            .filter(|e| e.kind() != std::io::ErrorKind::NotFound)
            .map(|e| (from, e))
    })
}

/// Reads the fingerprints of the `steps` files of the plan `plan`, a job log, without its lock,
/// which the caller may hold. Returns the error when the plan cannot be read, or is no longer
/// one of `steps` renames. A plan that a share could not read is no reason to rename by the
/// names alone, without the check of each file.
fn read_plan_known(plan: &Path, steps: usize) -> std::io::Result<PlanKnown> {
    use crate::files::job_log::{LogRead, read_job_log_without_lock};
    let LogRead::Log { job, records, .. } = read_job_log_without_lock(plan)? else {
        return Err(std::io::ErrorKind::InvalidData.into());
    };
    match rename_records(job, &records) {
        Some((moves, _, known)) if moves.len() == steps => Ok((known, folders_noted(&records))),
        _ => Err(std::io::ErrorKind::InvalidData.into()),
    }
}

/// Returns a name for a new plan or log. It is a name of its own, never one that is there, so a
/// plan left by a crash, of a Glimt that had the same process ID before a restart, is not
/// written over and lost. It is this window's name and a time in nanoseconds, and no two calls
/// in one window give the same time, also when the clock does not move between them or goes
/// back. `JobLog::start` counts on that, as it renames a new log to this name with a rename
/// that would write over a file there.
pub(crate) fn new_name() -> String {
    static LAST: std::sync::Mutex<u128> = std::sync::Mutex::new(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let nanos = later_than_last(
        &mut crate::support::threads::lock_even_if_poisoned(&LAST),
        now,
    );
    format!("{}-{nanos}", crate::features::remote::instance_id())
}

/// Returns `now`, or one more than `last` when `now` is not later, and keeps it in `last`. So
/// every call gives a time of its own, also from two threads, which take turns at the lock.
fn later_than_last(last: &mut u128, now: u128) -> u128 {
    *last = now.max(*last + 1);
    *last
}

/// The plans in `dir`, the folder of the job logs, of renames that were cut short, which are
/// those no running window holds locked. Each is let go of again, and locked anew when it is
/// put right (`repair_cut_short`).
pub fn cut_short_plans(dir: &Path) -> Vec<CutShort> {
    use crate::files::job_log::{LogRead, read_job_log};
    let entries = std::fs::read_dir(dir).into_iter().flatten().flatten();
    entries
        .filter_map(|e| {
            // A pipe or a link planted in the state folder is no plan, and opening a pipe would
            // hold the start for good. It reads no more than a decode would.
            e.file_type().ok().filter(|kind| kind.is_file())?;
            let (file, read) = read_job_log(&e.path()).ok()?;
            // A plan another window was finishing is skipped. That window deletes the plan
            // before it lets go of the lock, so a file opened just before then is no longer linked.
            is_linked(&file).ok().filter(|is_linked| *is_linked)?;
            let LogRead::Log { job, records, .. } = read else {
                return None;
            };
            let (moves, done, _) = rename_records(job, &records)?;
            (!moves.is_empty()).then(|| (e.path(), moves, Some(done)))
        })
        .collect()
}

/// Does one step of a plan, `a` to `b`, never over another file (`put_file`). A step cut short
/// between its link and its unlink, where both names are the file, is finished by taking the
/// old name away. This is the one way a step is done, whether as Rename Many runs, as it is
/// undone, or as it is put right at the next start.
pub fn rename_step(a: &Path, b: &Path) -> std::io::Result<()> {
    if same_file_by_opening(a, b) {
        #[allow(clippy::disallowed_methods)] // a second name of the same file
        return std::fs::remove_file(a);
    }
    crate::files::file_io::put_file(a, b, false)
}

/// Finishes a step from `a` to `b` that failed in a way that may have been done (a share that
/// lost its answer), so that the file is at `b` alone. It may be at both names, one file under
/// two, and then the name `a` goes (`rename_step`), or still only at `a`, and then it is moved.
/// Returns `Ok` when nothing is left at `a` and the file is at `b`, and `CannotTell` otherwise,
/// as a failed look tells nothing, and a file at neither name was lost on the way.
pub fn finish_step(a: &Path, b: &Path) -> AtStart<()> {
    // A step that failed again, also one that may have been done, is judged by sure looks at
    // both names, so its error is not needed. Both are looked at, as `done_after_all` does,
    // because a share may lose the file in a failed rename.
    match rename_step(a, b) {
        Ok(()) => Ok(()),
        Err(_) if is_surely_gone(a) && is_surely_there(b) => Ok(()),
        Err(_) => Err(CannotTell),
    }
}

/// Undoes the first `done` steps of `moves`, last first. Returns (renamed, left). A name that is
/// no longer there is nothing of ours to put back, and it is left out of both.
pub fn undo(moves: &[(PathBuf, PathBuf)], done: usize) -> (usize, usize) {
    let back = moves[..done.min(moves.len())].iter().rev();
    do_steps(back.map(|(a, b)| (b.as_path(), a.as_path())))
}

/// Does the steps of `moves` after the first `done`. Returns (renamed, left). At the first step
/// that fails, it stops and undoes the steps it did, so the names on disk stay those of the
/// count that the plan keeps. Gone on, it left files under hidden names that no count
/// described, and putting the old names back later missed them.
pub fn finish(moves: &[(PathBuf, PathBuf)], done: usize) -> (usize, usize) {
    let mut done_here: Vec<(&Path, &Path)> = Vec::new();
    for (a, b) in &moves[done.min(moves.len())..] {
        match repair_step(a, b) {
            Some(true) => done_here.push((b, a)),
            Some(false) => {
                let (_, stuck) = do_steps(done_here.into_iter().rev());
                return (0, 1 + stuck);
            }
            None => {}
        }
    }
    (done_here.len(), 0)
}

/// Does each of `steps`, as `repair_step` does. Returns (renamed, left).
fn do_steps<'a>(steps: impl Iterator<Item = (&'a Path, &'a Path)>) -> (usize, usize) {
    let (mut renamed, mut left) = (0, 0);
    for (a, b) in steps {
        match repair_step(a, b) {
            Some(true) => renamed += 1,
            Some(false) => left += 1,
            None => {}
        }
    }
    (renamed, left)
}

/// Does one step of a repair, `a` to `b`. Returns whether it was done, and `None` when `a` is
/// surely not there, so there is nothing of ours to move.
fn repair_step(a: &Path, b: &Path) -> Option<bool> {
    // Only a name that is surely not there is passed over. One that cannot be looked at (a
    // share that answers EIO) may still hold a file, so the plan is kept for it.
    match std::fs::symlink_metadata(a) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => Some(false),
        // A step that failed, also one that may have been done, counts as not done, which keeps
        // the plan for the next start, so its error is not needed.
        Ok(_) => match rename_step(a, b) {
            Ok(()) => Some(true),
            Err(_) => Some(false),
        },
    }
}

/// The files a plan's `moves` take from their old names to their new ones, as (from, to). The
/// hidden steps of a swap (a → .t0, b → a, .t0 → b) are followed to their ends, so marks and
/// collections can follow the files as after a finished Rename Many.
pub fn pairs(moves: &[(PathBuf, PathBuf)]) -> Vec<(PathBuf, PathBuf)> {
    // A file's own name is never the hidden name an earlier step gave another file. Its chain
    // runs forward through the later steps only, because a swap's steps lead round in a ring.
    // Both are looked up in maps, so a swap of 100,000 files is followed in one pass.
    let mut first_to: HashMap<&Path, usize> = HashMap::new();
    let mut steps_from: HashMap<&Path, Vec<usize>> = HashMap::new();
    for (k, (a, b)) in moves.iter().enumerate() {
        first_to.entry(b).or_insert(k);
        steps_from.entry(a).or_default().push(k);
    }
    let starts = (moves.iter().enumerate())
        .filter(|(k, (a, _))| first_to.get(a.as_path()).is_none_or(|first| first >= k));
    starts
        .map(|(k, (a, b))| {
            let (mut at, mut end): (usize, &Path) = (k, b);
            // The first step after `at` that takes the file on from `end`.
            let next = |at: usize, end: &Path| {
                let ks = steps_from.get(end)?;
                ks.get(ks.partition_point(|&n| n <= at)).copied()
            };
            while let Some(n) = next(at, end) {
                (at, end) = (n, &moves[n].1);
            }
            (a.clone(), end.to_path_buf())
        })
        .collect()
}

/// Puts the old names back or finishes the renames, by undoing the `done` steps or doing the
/// rest. For a plan that did not count them, it takes every step, by the names alone. Returns
/// (renamed, left). The plan goes when nothing is left, and otherwise it is asked about again
/// at the next start. Returns `None` when another window has the plan or has put it right
/// already. Then nothing was done here, and the caller must not tell marks and collections of
/// the moves a second time (a swap's marks would be swapped back).
pub fn repair_cut_short(
    plan: &Path,
    moves: &[(PathBuf, PathBuf)],
    done: Option<usize>,
    undo_them: bool,
) -> Option<(usize, usize)> {
    // Only one window at a time works on the plan, and only while it is there. A second window
    // that asked about it too would do the same steps again, on files already put right. That
    // also holds for one that opened it just before the first deleted it, and got the lock once
    // that one let go.
    let _held = crate::files::file_io::open_if_unlocked(plan, false)
        .ok()
        .filter(|file| is_linked(file).unwrap_or(false))?;
    let done = done.map(|d| done_by_names(moves, d));
    // A step whose photo's name another file has taken since the plan was made (while the
    // question waited for "Later") is not taken. Nothing is renamed then, so that a photo and its
    // sidecar are not split, and the plan is asked about again. A sidecar is not looked at, as it
    // follows its photo by name (`known_fingerprints`).
    let (known, folders) = match read_plan_known(plan, moves.len()) {
        Ok(read) => read,
        Err(e) => {
            crate::files::file_io::save_failed(format!("{}: {e}", plan.display()));
            return Some((0, 1));
        }
    };
    // A name that the start cannot tell about, as on a stick not mounted or an empty mount point
    // at its folder, keeps the plan. Its file counts as left, and the user is asked again with
    // the names left. A plan of Glimt 0.92, which noted no folders, is read as before.
    let mut names = moves.iter().flat_map(|(a, b)| [a, b]);
    if !folders.is_empty() && names.any(|p| is_gone_at_start(p, &folders).is_err()) {
        return Some((0, 1));
    }
    let taken = match (undo_them, done) {
        (_, _) if known.is_empty() => Vec::new(),
        (true, done) => (0..done.unwrap_or(moves.len()).min(moves.len()))
            .rev()
            .map(|k| (moves[k].1.as_path(), known[k]))
            .collect(),
        (false, done) => (done.unwrap_or(0).min(moves.len())..moves.len())
            .map(|k| (moves[k].0.as_path(), known[k]))
            .collect(),
    };
    if let Some((name, e)) = another_file_at(taken.into_iter()) {
        crate::files::file_io::save_failed(format!("{}: {e}", name.display()));
        return Some((0, 1));
    }
    let mut first = 0;
    // When the names are put back, the step cut short between its link and its unlink, the
    // first one not counted, has its new name taken away first, so the steps before it can be
    // undone.
    // A remove that failed, also one that may have been done, only leaves the step uncounted:
    // the file stays at `a`, under one or two names, so its error is not needed.
    #[allow(clippy::disallowed_methods)] // a second name of the same file
    if undo_them
        && let Some((a, b)) = done.and_then(|d| moves.get(d))
        && same_file_by_opening(a, b)
        && let Ok(()) = std::fs::remove_file(b)
    {
        first = 1;
    }
    let (renamed, left) = match (undo_them, done) {
        (true, Some(done)) => undo(moves, done),
        (false, Some(done)) => finish(moves, done),
        // When the steps were not counted, every step is undone or finished, by the names
        // alone.
        (true, None) => undo(moves, moves.len()),
        (false, None) => finish(moves, 0),
    };
    // Every step was done, or surely had nothing of the plan's to rename.
    let is_put_right = (left == 0).then_some(()).ok_or(CannotTell);
    if let Some(sure) = Sure::of(&is_put_right) {
        remove_plan(plan, &one_name_per_folder(moves), sure);
    }
    Some((renamed + first, left))
}

/// Returns the list of the files of the plan `plan`, of the renames `moves` of which `done` were
/// done, that are not under the names they were to get: their old names when `undo_them`, and
/// otherwise their new ones. Each line names a file as it is now, a swap's hidden name
/// included, then the name it was to get, and why the repair refused it where it did. It is
/// what the user is given to save or copy before the names are left as they are
/// (`leave_names`), to rename those files by hand. Returns an empty text when every file is
/// under the name it was to get.
pub fn names_left(
    plan: &Path,
    moves: &[(PathBuf, PathBuf)],
    done: Option<usize>,
    undo_them: bool,
) -> String {
    use crate::files::file_io::{KnownAs, is_still_the_file};
    let (known, unread) = match read_plan_known(plan, moves.len()) {
        Ok((known, _)) => (known, String::new()),
        Err(e) => (Vec::new(), format!("{}: {e}\n", plan.display())),
    };
    let mut text = unread;
    for (first, now, last, known) in files_of(moves, done, &known) {
        let meant = if undo_them { first } else { last };
        if now == meant || is_surely_gone(now) {
            continue;
        }
        text += &format!("{} → {}", now.display(), meant.display());
        let hidden =
            (now.file_name().unwrap_or_default().as_bytes()).starts_with(HIDDEN_NAME.as_bytes());
        let refused = known
            .filter(|_| !hidden)
            .and_then(|known| is_still_the_file(now, KnownAs::size_and_time(known)).err());
        if let Some(e) = refused {
            text += &format!(": {e}");
        }
        text.push('\n');
    }
    text
}

/// Leaves the files of the plan `plan`, of the renames `moves` of which `done` were done, under
/// the names they have now, and removes the plan. Nothing is renamed or deleted. Returns the
/// renames done, `(old name, name now)`, which the marks and collections follow. Returns
/// `Err(None)` when the plan is gone, as another window has put it right already, and
/// `Err(Some(why))` when it could not be opened or another window has it open now. The plan
/// then stays.
pub fn leave_names(
    plan: &Path,
    moves: &[(PathBuf, PathBuf)],
    done: Option<usize>,
) -> Result<Vec<(PathBuf, PathBuf)>, Option<std::io::Error>> {
    // Only one window at a time, and only while the plan is there, as `repair_cut_short`.
    let held =
        crate::files::file_io::open_if_unlocked(plan, false).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => None,
            std::io::ErrorKind::WouldBlock => Some(std::io::Error::other(
                crate::support::lang::tr("another window has them open"),
            )),
            _ => Some(e),
        })?;
    if !is_linked(&held).map_err(Some)? {
        return Err(None);
    }
    // Each file is followed to the name the count says it has now, and a pair is given only
    // where the file may be there (`is_followed`). Taken from the count alone, a repair that
    // stopped half way gave a photo that went back to its old name the marks of a name where
    // nothing was. The files' fingerprints of the plan tell a photo that went back.
    // The user chose to leave the names, and the plan was read whole, so it may go.
    let read = read_plan_known(plan, moves.len());
    let mut sure = Sure::of(&read);
    let (known, _) = read.map_err(Some)?;
    // A file whose names cannot be told follows, as when its first name was not there, and
    // the plan stays for a later start.
    let mut is_followed_here = |first, now, known| {
        is_followed((first, now), known).unwrap_or_else(|CannotTell| {
            sure = None;
            crate::files::file_io::is_followed_to(now)
        })
    };
    let followed = (files_of(moves, done, &known).into_iter())
        .filter(|&(first, now, _, known)| first != now && is_followed_here(first, now, known))
        .map(|(first, now, ..)| (first.to_path_buf(), now.to_path_buf()))
        .collect();
    if let Some(sure) = sure {
        remove_plan(plan, &one_name_per_folder(moves), sure);
    }
    drop(held);
    Ok(followed)
}

/// Returns whether the marks and collection entries of the file first named `first` follow it to
/// `now`, the name the plan's count gives it, when the names are left as they are. They do where
/// a file is at `now`. Where the look at `now` fails, they follow too (`is_followed_to`), unless
/// the file is back at `first`: there with the size and time it had when the plan was made,
/// `known`. A repair that stopped half way may have put it back, and a failed look at a name
/// where nothing is would else give that name its marks. Returns `CannotTell` when the look at
/// `first` fails, so that the plan stays for a later start.
fn is_followed((first, now): (&Path, &Path), known: Option<FileFingerprint>) -> AtStart<bool> {
    use crate::files::file_io::{KnownAs, is_followed_to, is_still_the_file};
    if is_surely_there(now) {
        return Ok(true);
    }
    let is_back_at_first = match known {
        Some(known) => match is_still_the_file(first, KnownAs::size_and_time(known)) {
            Ok(()) => true,
            Err(_) => {
                can_be_looked_at(first)?;
                false
            }
        },
        None => false,
    };
    Ok(!is_back_at_first && is_followed_to(now))
}

/// Each file of the renames `moves` of which `done` were done, as (its first name, its name
/// now, its last name, its fingerprint in `known`). A step that moves on a file an earlier step
/// moved (a swap's second step) moves the same file. How many steps were done is taken from
/// the names on disk where they show more than `done` (`done_by_names`).
fn files_of<'a>(
    moves: &'a [(PathBuf, PathBuf)],
    done: Option<usize>,
    known: &[Option<FileFingerprint>],
) -> Vec<(&'a Path, &'a Path, &'a Path, Option<FileFingerprint>)> {
    let done = done_by_names(moves, done.unwrap_or(0)).min(moves.len());
    let mut files: Vec<(&Path, &Path, &Path, Option<FileFingerprint>)> = Vec::new();
    let mut file_at: HashMap<&Path, usize> = HashMap::new();
    for (k, (a, b)) in moves.iter().enumerate() {
        let (a, b) = (a.as_path(), b.as_path());
        let n = file_at.remove(a).unwrap_or_else(|| {
            files.push((a, a, a, known.get(k).copied().flatten()));
            files.len() - 1
        });
        files[n].2 = b;
        if k < done {
            files[n].1 = b;
        }
        file_at.insert(b, n);
    }
    files
}

/// How many steps were done. It is `done`, or more where the names on disk show it, because a
/// power cut may lose the last counts. The names each step leaves are followed from the start,
/// and the first count from `done` on whose names are all as on disk is taken. Undone from the
/// count alone, a swap cut short after a → .t0 and b → .t1 but counted at one put a back and
/// left b's photo under its hidden name for good, with the plan deleted. A step cut short
/// between its link and its unlink has both names for one file, and then that count is taken,
/// with the step as the first not done. When no count from `done` on fits, the last earlier
/// count that fits is taken, because the count may also have reached the disk before the names
/// did (photos on a USB stick pulled out, the plan on another disk). Where no count fits at all
/// (a file changed by hand since), the count stays. A swap's names at its start are also those
/// of its end, and below half its count tells that it was not begun. It looks once at each
/// name.
fn done_by_names(moves: &[(PathBuf, PathBuf)], done: usize) -> usize {
    // Each name gets a number, once. It records whether the name is there after the steps so
    // far, and whether it is on disk.
    let mut number: std::collections::HashMap<&Path, usize> = Default::default();
    let steps: Vec<[usize; 2]> = (moves.iter())
        .map(|(a, b)| {
            [a, b].map(|p| {
                let n = number.len();
                *number.entry(p.as_path()).or_insert(n)
            })
        })
        .collect();
    let mut on_disk = vec![false; number.len()];
    for (p, &n) in &number {
        on_disk[n] = !is_surely_gone(p);
    }
    // Before the first step, each step's old name that no earlier step made is there.
    let (mut there, mut made) = (vec![false; number.len()], vec![false; number.len()]);
    for &[a, b] in &steps {
        there[a] |= !made[a];
        made[b] = true;
    }
    let mut wrong = (0..there.len()).filter(|&n| there[n] != on_disk[n]).count();
    let mut earlier = None;
    for (k, &[a, b]) in steps.iter().enumerate() {
        // When cut short between this step's link and its unlink, the names are those of the
        // count before it, and its new name too, the same file as its old one. `repair_cut_short`
        // takes that name away.
        let (from, to) = (&moves[k].0, &moves[k].1);
        let fits = wrong == 0
            || (wrong == 1 && !there[b] && on_disk[b] && same_file(from, to).unwrap_or(false));
        match (fits, k >= done) {
            (true, true) => return k,
            (true, false) => earlier = Some(k),
            (false, _) => {}
        }
        for (n, now) in [(a, false), (b, true)] {
            if there[n] != now {
                wrong =
                    wrong + usize::from(now != on_disk[n]) - usize::from(there[n] != on_disk[n]);
                there[n] = now;
            }
        }
    }
    // A swap ends under the names it started with. Its count passes half only after its
    // middle is on disk (`rename_files`), so with a count below half those names say that it
    // was not begun.
    match (wrong == 0 && steps.len() >= done, earlier) {
        (true, Some(k)) if done < steps.len() / 2 => k,
        (true, _) => steps.len(),
        (false, _) => earlier.unwrap_or(done),
    }
}

/// Returns one name in each folder that `moves` rename in, for `sync_folders`. A Rename Many in
/// one folder gives one name, however many files it renames.
fn one_name_per_folder(moves: &[(PathBuf, PathBuf)]) -> Vec<PathBuf> {
    let mut seen = std::collections::HashSet::new();
    (moves.iter().flat_map(|(a, b)| [a, b]))
        .filter(|p| seen.insert(p.parent()))
        .cloned()
        .collect()
}

/// Syncs the folder of each of `names` to disk, so that the renames in them are there before
/// the plan that could put them right is changed or removed. The renames themselves are not
/// synced one by one: a Rename Many of 500 files on a slow USB stick would wait for 1,000
/// syncs instead of one. Returns the first name whose folder the disk said it did not take, with
/// the error. A folder that cannot be synced at all is passed over by `sync_folder_of`.
fn sync_folders(names: &[PathBuf]) -> Result<(), (&Path, std::io::Error)> {
    let mut first_failed = Ok(());
    // Every folder is synced, also after one that failed, so the loop does not stop at the first.
    for name in names {
        if let Err(e) = crate::files::file_io::sync_folder_of(name)
            && first_failed.is_ok()
        {
            first_failed = Err((name.as_path(), e));
        }
    }
    first_failed
}

/// Tells the user when `synced`, the answer of `sync_folders`, says that a folder did not get
/// to the disk.
fn tell_when_not_synced(synced: Result<(), (&Path, std::io::Error)>) {
    if let Err((name, e)) = synced {
        let folder = name.parent().unwrap_or(name);
        crate::files::file_io::save_failed(crate::tf!(
            "Could not save {name}: {e}",
            name = folder.display(),
            e
        ));
    }
}

/// Removes `plan`, as `sure` proves it may go, after syncing the folders of `names`. The plan
/// goes also when the disk says a folder did not get there, and the user is told. The marks and
/// collections follow the new names as soon as the renames are done, and a plan kept beside
/// them would move them again at the next start, or swap a swap's marks back.
fn remove_plan(plan: &Path, names: &[PathBuf], sure: Sure) {
    let renames_synced = sync_folders(names);
    // A plan left behind would be offered again at the next start, and putting its old names
    // back would undo a finished rename, so a failure is told.
    remove_job_log(plan, sure);
    // The plan's folder is synced too, because a plan brought back by a power cut, after the
    // marks followed, would move them again. A disk that fails both is told once.
    let plan_name = [plan.to_path_buf()];
    tell_when_not_synced(renames_synced.and(sync_folders(&plan_name)));
}

/// Removes each folder of the logs of Glimt 0.92 and older (`OLD_LOG_FOLDERS`) in Glimt's state
/// folder `state` that holds no log: one that is empty, or that holds only empty folders of
/// lists and their lock file. This Glimt no longer reads those logs, so a folder that still
/// holds anything else is left as it is, and the user is told once at this start which Glimt
/// puts it right. A folder that cannot be looked at is left too, for a later start.
pub fn clear_old_log_folders(state: &Path) {
    for name in OLD_LOG_FOLDERS {
        let folder = state.join(name);
        if is_surely_gone(&folder) {
            continue;
        }
        let holds = holds_an_old_log(&folder);
        match (Sure::of(&holds), holds) {
            (Some(sure), Ok(false)) => {
                remove_job_log(&folder, sure);
            }
            (_, Ok(true)) => crate::files::file_io::save_failed(crate::tf!(
                "{folder} holds logs that Glimt 0.92 or older left when it was stopped during a Rename Many, a Move or a delete. This Glimt no longer reads them: start Glimt 0.94 once to put them right. Until then they are kept.",
                folder = folder.display()
            )),
            _ => {}
        }
    }
}

/// Returns whether the folder of old logs `folder` holds anything but folders of lists
/// (`OLD_LISTS_FOLDERS`) with nothing in them but their lock file: a log, or a list that a log
/// left. Returns `CannotTell` when the folder, or one of its folders of lists, cannot be read.
/// No entry is followed through a link, as a link is neither a folder of lists nor a lock file.
fn holds_an_old_log(folder: &Path) -> AtStart<bool> {
    let entries = |dir: &Path| -> AtStart<Vec<std::fs::DirEntry>> {
        let read = std::fs::read_dir(dir).map_err(|_| CannotTell)?;
        read.collect::<std::io::Result<_>>().map_err(|_| CannotTell)
    };
    for entry in entries(folder)? {
        let kind = entry.file_type().map_err(|_| CannotTell)?;
        let name = entry.file_name();
        if !kind.is_dir() || !OLD_LISTS_FOLDERS.iter().any(|lists| name == *lists) {
            return Ok(true);
        }
        for list in entries(&entry.path())? {
            let kind = list.file_type().map_err(|_| CannotTell)?;
            if !kind.is_file() || list.file_name() != OLD_LISTS_LOCK {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Returns whether `file` still has a name. A plan that another window put right and deleted
/// has none. A failed look is passed on, because it says nothing about the name. The listings
/// skip such a file, as one they cannot read, and `leave_names` keeps the plan and says why.
fn is_linked(file: &File) -> std::io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    Ok(file.metadata()?.nlink() > 0)
}

#[cfg(test)]
mod tests {
    /// A step that failed counts as finished only when its file is at its new name and gone
    /// from its old one. Before, a gone old name was enough, and a sidecar that a share lost in
    /// a failed move counted as beside its file, so the loss was not told.
    #[test]
    fn a_step_is_finished_only_when_its_new_name_holds_the_file() {
        let dir = std::env::temp_dir().join(format!("glimt-test-finish-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a.jpg.xmp"), dir.join("b.jpg.xmp"));
        assert!(super::finish_step(&a, &b).is_err());
        std::fs::write(&b, "b").unwrap();
        assert!(super::finish_step(&a, &b).is_ok());
        std::fs::write(&a, "a").unwrap();
        assert!(super::finish_step(&a, &b).is_err());
        assert!(a.exists() && b.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Two names asked for at one moment of the clock, or after the clock went back, differ.
    /// Before, a name was the clock's time alone, and two logs of one moment had one name.
    #[test]
    fn two_new_names_at_one_moment_differ() {
        let mut last = 0;
        assert_eq!(super::later_than_last(&mut last, 5), 5);
        assert_eq!(super::later_than_last(&mut last, 5), 6);
        assert_eq!(super::later_than_last(&mut last, 3), 7);
        assert_eq!(super::later_than_last(&mut last, 10), 10);
    }

    /// A log of a kind of job that the start has no reader for would stay in `jobs/` for good.
    /// So every kind in `JOB_KINDS` must have one, and a kind added without its reader fails
    /// here.
    #[test]
    fn every_job_kind_is_read_at_start() {
        for kind in crate::files::job_log::JOB_KINDS {
            assert!(
                super::read_at_start(kind).is_some(),
                "{kind} has no reader at start"
            );
        }
        assert_eq!(super::read_at_start("later-kind"), None);
    }

    /// A Move killed after a photo went and before its sidecar did leaves the sidecar at the old
    /// name. The next start moves it to the photo, but never over a sidecar at the new name.
    /// Before, the sidecar stayed behind, and the photo had no keywords.
    #[test]
    fn a_sidecar_left_by_a_killed_move_follows_its_photo() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-killed-side-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (from, to, logs) = (dir.join("from"), dir.join("to"), dir.join("logs"));
        std::fs::create_dir_all(&from).unwrap();
        std::fs::create_dir_all(&to).unwrap();
        for n in ["a.jpg", "a.jpg.xmp", "b.jpg", "b.jpg.xmp"] {
            std::fs::write(from.join(n), n).unwrap();
        }
        std::fs::write(to.join("b.jpg.xmp"), "another's").unwrap();
        let mut log = super::start_move_log(&logs, "move").unwrap();
        // The kill came after each photo went and before its sidecar did.
        for n in ["a.jpg", "b.jpg"] {
            log.note(&from.join(n), &to.join(n));
            std::fs::rename(from.join(n), to.join(n)).unwrap();
        }
        drop(log);
        let found = super::move_jobs_cut_short(&logs);
        assert_eq!(found.len(), 1);
        let read = |p: std::path::PathBuf| std::fs::read_to_string(p).unwrap();
        assert_eq!(read(to.join("a.jpg.xmp")), "a.jpg.xmp");
        assert!(!from.join("a.jpg.xmp").exists());
        assert_eq!(read(to.join("b.jpg.xmp")), "another's");
        assert_eq!(read(from.join("b.jpg.xmp")), "b.jpg.xmp");
        drop(found);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A Move killed after a file went to a stick that is not mounted at the next start leaves
    /// a pair whose new name cannot be told. The log stays for a later start. Before, the pair
    /// counted as not gone, the log went, and the file's marks never followed it.
    #[test]
    fn a_killed_move_to_a_stick_not_mounted_keeps_its_log() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-killed-stick-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (from, stick, logs) = (dir.join("from"), dir.join("stick"), dir.join("logs"));
        std::fs::create_dir_all(&from).unwrap();
        std::fs::create_dir_all(&stick).unwrap();
        std::fs::write(from.join("a.jpg"), "a").unwrap();
        let mut log = super::start_move_log(&logs, "move").unwrap();
        log.note(&from.join("a.jpg"), &stick.join("a.jpg"));
        std::fs::rename(from.join("a.jpg"), stick.join("a.jpg")).unwrap();
        drop(log);
        // The stick is out: its mount point is an empty folder on another device.
        std::fs::remove_dir_all(&stick).unwrap();
        std::fs::create_dir(&stick).unwrap();
        super::ON_ANOTHER_DEVICE.with_borrow_mut(|folders| folders.push(stick.clone()));
        assert!(super::move_jobs_cut_short(&logs).is_empty());
        super::ON_ANOTHER_DEVICE.with_borrow_mut(Vec::clear);
        std::fs::write(stick.join("a.jpg"), "a").unwrap();
        assert_eq!(super::move_jobs_cut_short(&logs).len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A Move's log found at the start gives only the pairs whose new name still holds the file
    /// that went, and tells the others. Before, a photo that another program put at a new name
    /// since got the marks and the sidecar of the file that went.
    #[test]
    fn a_killed_move_follows_only_the_files_that_went() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-killed-who-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (from, to, logs) = (dir.join("from"), dir.join("to"), dir.join("logs"));
        std::fs::create_dir_all(&from).unwrap();
        std::fs::create_dir_all(&to).unwrap();
        for n in ["a.jpg", "b.jpg", "b.jpg.xmp"] {
            std::fs::write(from.join(n), n).unwrap();
        }
        let mut log = super::start_move_log(&logs, "move").unwrap();
        for n in ["a.jpg", "b.jpg"] {
            log.note(&from.join(n), &to.join(n));
            std::fs::rename(from.join(n), to.join(n)).unwrap();
        }
        drop(log);
        // Another program put another photo at b.jpg's new name since.
        std::fs::write(to.join("b.jpg"), "another photo").unwrap();
        crate::files::file_io::take_saves_failed();
        let found = super::move_jobs_cut_short(&logs);
        let a = (from.join("a.jpg"), to.join("a.jpg"));
        assert_eq!(found[0].1, vec![a]);
        // The sidecar left at b.jpg's old name stays, and the pair left alone is told.
        assert!(from.join("b.jpg.xmp").exists() && !to.join("b.jpg.xmp").exists());
        assert_eq!(crate::files::file_io::take_saves_failed().len(), 1);
        drop(found);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A file that a killed Move put on a disk whose file system keeps coarser times (FAT,
    /// exFAT, NTFS, SMB) is still the file that went, and its sidecar follows it. Before, the
    /// time cut to the disk's resolution was taken for another file, so no marks or sidecar
    /// followed. A file whose time is further off is still another file.
    #[test]
    fn a_killed_move_to_a_disk_of_coarser_times_follows_the_files() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-killed-coarse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (from, to, logs) = (dir.join("from"), dir.join("to"), dir.join("logs"));
        std::fs::create_dir_all(&from).unwrap();
        std::fs::create_dir_all(&to).unwrap();
        let at = |seconds, nanoseconds| {
            std::time::UNIX_EPOCH + std::time::Duration::new(seconds, nanoseconds)
        };
        let set_time = |path: &std::path::Path, time| {
            let file = std::fs::File::options().write(true).open(path).unwrap();
            file.set_modified(time).unwrap();
        };
        for n in ["a.jpg", "b.jpg", "a.jpg.xmp"] {
            std::fs::write(from.join(n), "photo").unwrap();
        }
        let mut log = super::start_move_log(&logs, "move").unwrap();
        for n in ["a.jpg", "b.jpg"] {
            set_time(&from.join(n), at(1_700_000_001, 234_567_891));
            log.note(&from.join(n), &to.join(n));
            std::fs::rename(from.join(n), to.join(n)).unwrap();
        }
        drop(log);
        // FAT keeps a.jpg's time to 2 seconds, and b.jpg's name holds a later photo.
        set_time(&to.join("a.jpg"), at(1_700_000_000, 0));
        set_time(&to.join("b.jpg"), at(1_700_000_004, 0));
        crate::files::file_io::take_saves_failed();
        let found = super::move_jobs_cut_short(&logs);
        assert_eq!(found[0].1, vec![(from.join("a.jpg"), to.join("a.jpg"))]);
        assert!(to.join("a.jpg.xmp").exists() && !from.join("a.jpg.xmp").exists());
        assert_eq!(crate::files::file_io::take_saves_failed().len(), 1);
        drop(found);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A power cut lost the count of the last step done. The plan is undone by the names too, so
    /// no photo stays under a hidden name, and it is finished all the same.
    #[test]
    fn puts_back_the_steps_whose_count_was_lost() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-lost-count-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let plans = dir.join("plans");
        std::fs::create_dir_all(&plans).unwrap();
        let p = |n: &str| dir.join(n);
        let text = |n| std::fs::read_to_string(p(n)).unwrap();
        let moves = vec![
            (p("a.jpg"), p(".tmp-0")),
            (p("b.jpg"), p(".tmp-1")),
            (p(".tmp-0"), p("b.jpg")),
            (p(".tmp-1"), p("a.jpg")),
        ];
        // The plan is made with the photos under their old names, as rename_many makes it, so
        // that it holds their fingerprints, and then `steps` of its renames are done.
        let start = |moves: &super::Moves, steps: usize| {
            let cut = super::start_journal(&plans, moves).unwrap();
            for (from, to) in &moves[..steps] {
                std::fs::rename(from, to).unwrap();
            }
            cut
        };
        // Three steps are done and one counted, and b is there again, made by the third.
        write_with_time(&p("a.jpg"), "a", 0);
        write_with_time(&p("b.jpg"), "b", 1);
        let plan = plans.join("cut");
        let mut cut = start(&moves, 3);
        cut.count_step_done();
        std::fs::copy(cut.path(), &plan).unwrap();
        drop(cut);
        assert_eq!(
            super::repair_cut_short(&plan, &moves, Some(1), true),
            Some((3, 0))
        );
        assert_eq!((text("a.jpg"), text("b.jpg")), ("a".into(), "b".into()));
        assert!(!p(".tmp-0").exists() && !p(".tmp-1").exists() && !plan.exists());
        std::fs::remove_file(p("a.jpg")).unwrap();
        std::fs::remove_file(p("b.jpg")).unwrap();
        for undo in [true, false] {
            // Two steps are done and the third linked, with one counted, so b and .tmp-0 are one
            // file. When put back, the link goes first. When finished, the third step is the
            // link's unlink.
            write_with_time(&p("a.jpg"), "a", 0);
            write_with_time(&p("b.jpg"), "b", 1);
            let plan = plans.join("cut");
            let mut cut = start(&moves, 2);
            std::fs::hard_link(p(".tmp-0"), p("b.jpg")).unwrap();
            cut.count_step_done();
            std::fs::copy(cut.path(), &plan).unwrap();
            drop(cut);
            let (a, b, steps) = match undo {
                true => ("a", "b", 3),
                false => ("b", "a", 2),
            };
            assert_eq!(
                super::repair_cut_short(&plan, &moves, Some(1), undo),
                Some((steps, 0))
            );
            assert_eq!((text("a.jpg"), text("b.jpg")), (a.into(), b.into()));
            assert!(!p(".tmp-0").exists() && !p(".tmp-1").exists() && !plan.exists());
            std::fs::remove_file(p("a.jpg")).unwrap();
            std::fs::remove_file(p("b.jpg")).unwrap();
            // A chain without swaps, a → A … d → D, with three done, the fourth linked, and one
            // counted.
            let chain: Vec<_> = ["a", "b", "c", "d"]
                .map(|n| {
                    (
                        p(&format!("{n}.png")),
                        p(&format!("{}.png", n.to_uppercase())),
                    )
                })
                .into();
            for (k, n) in ["a", "b", "c", "d"].into_iter().enumerate() {
                write_with_time(&p(&format!("{n}.png")), n, k);
            }
            let mut cut = start(&chain, 3);
            std::fs::hard_link(p("d.png"), p("D.png")).unwrap();
            cut.count_step_done();
            std::fs::copy(cut.path(), &plan).unwrap();
            drop(cut);
            let steps = if undo { 4 } else { 1 };
            assert_eq!(
                super::repair_cut_short(&plan, &chain, Some(1), undo),
                Some((steps, 0))
            );
            for n in ["a", "b", "c", "d"] {
                let (keep, gone) = match undo {
                    true => (n.to_string(), n.to_uppercase()),
                    false => (n.to_uppercase(), n.to_string()),
                };
                assert_eq!(
                    std::fs::read_to_string(p(&format!("{keep}.png"))).unwrap(),
                    n
                );
                assert!(!p(&format!("{gone}.png")).exists());
                std::fs::remove_file(p(&format!("{keep}.png"))).unwrap();
            }
        }
        for undo in [true, false] {
            // Two steps done, one counted.
            write_with_time(&p("a.jpg"), "a", 0);
            write_with_time(&p("b.jpg"), "b", 1);
            let plan = plans.join("cut");
            let mut cut = start(&moves, 2);
            cut.count_step_done();
            std::fs::copy(cut.path(), &plan).unwrap();
            drop(cut);
            let (a, b) = match undo {
                true => ("a", "b"),
                false => ("b", "a"),
            };
            assert_eq!(
                super::repair_cut_short(&plan, &moves, Some(1), undo),
                Some((2, 0))
            );
            assert_eq!((text("a.jpg"), text("b.jpg")), (a.into(), b.into()));
            assert!(!p(".tmp-0").exists() && !p(".tmp-1").exists() && !plan.exists());
            std::fs::remove_file(p("a.jpg")).unwrap();
            std::fs::remove_file(p("b.jpg")).unwrap();
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A finish that fails halfway undoes its own steps, so no photo stays under a hidden name
    /// that the plan's count does not describe.
    #[test]
    fn a_finish_that_fails_undoes_its_own_steps() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-finish-fails-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = |n: &str| dir.join(n);
        std::fs::write(p("a.jpg"), "a").unwrap();
        // A file put at the new name by hand stops the second step.
        std::fs::write(p("b.jpg"), "other").unwrap();
        let moves = vec![(p("a.jpg"), p(".tmp-0")), (p(".tmp-0"), p("b.jpg"))];
        assert_eq!(super::finish(&moves, 0), (0, 1));
        assert_eq!(std::fs::read_to_string(p("a.jpg")).unwrap(), "a");
        assert_eq!(std::fs::read_to_string(p("b.jpg")).unwrap(), "other");
        assert!(!p(".tmp-0").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A swap done to the end with only its first half counted (synced there) is finished, and
    /// the files stay swapped. With nothing counted, the names alone would take it for not
    /// begun, and finishing it would swap them back.
    #[test]
    fn a_whole_swap_counted_at_half_stays_swapped() {
        let dir = std::env::temp_dir().join(format!("glimt-test-half-swap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let plans = dir.join("plans");
        std::fs::create_dir_all(&plans).unwrap();
        let p = |n: &str| dir.join(n);
        let moves = vec![
            (p("a.jpg"), p(".tmp-0")),
            (p("b.jpg"), p(".tmp-1")),
            (p(".tmp-0"), p("b.jpg")),
            (p(".tmp-1"), p("a.jpg")),
        ];
        std::fs::write(p("a.jpg"), "b").unwrap();
        std::fs::write(p("b.jpg"), "a").unwrap();
        let plan = plans.join("cut");
        let mut cut = super::start_journal(&plans, &moves).unwrap();
        cut.count_step_done();
        cut.count_step_done();
        std::fs::copy(cut.path(), &plan).unwrap();
        drop(cut);
        assert_eq!(
            super::repair_cut_short(&plan, &moves, Some(2), false),
            Some((0, 0))
        );
        let text = |n| std::fs::read_to_string(p(n)).unwrap();
        assert_eq!((text("a.jpg"), text("b.jpg")), ("b".into(), "a".into()));
        assert!(!plan.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A pipe in the plans folder is passed over, not opened, because opening it would hold
    /// Glimt's start for good.
    #[test]
    fn passes_over_a_pipe_among_the_plans() {
        let dir = std::env::temp_dir().join(format!("glimt-test-plan-pipe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pipe = dir.join("rename-pipe");
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&pipe)
                .status()
                .unwrap()
                .success()
        );
        assert!(super::cut_short_plans(&dir).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A plan locked by a running rename is not taken for a cut-short one, nor written over by a
    /// second. One left by a crash is, and its files get their old names back, or their new ones.
    #[test]
    fn puts_cut_short_renames_right() {
        let dir = std::env::temp_dir().join(format!("glimt-test-journal-{}", std::process::id()));
        let plans = dir.join("plans");
        std::fs::create_dir_all(&dir).unwrap();
        let p = |n: &str| dir.join(n);
        // Rename Many swapping a and b through temporary names.
        let moves = vec![
            (p("a.jpg"), p(".tmp-0")),
            (p("b.jpg"), p(".tmp-1")),
            (p(".tmp-0"), p("b.jpg")),
            (p(".tmp-1"), p("a.jpg")),
        ];
        let running = super::start_journal(&plans, &moves).unwrap();
        // A second rename has a plan of its own, and neither is taken for one cut short.
        let second = super::start_journal(&plans, &moves[..1]).unwrap();
        assert_ne!(running.path(), second.path());
        assert!(super::cut_short_plans(&plans).is_empty());
        drop(second);
        // The plan is a job log of a rename, with a step for each rename and nothing done yet.
        let plan_now = crate::files::job_log::read_job_log_without_lock(running.path()).unwrap();
        let crate::files::job_log::LogRead::Log { job, records, .. } = plan_now else {
            panic!("not a job log");
        };
        assert_eq!(
            super::rename_records(job, &records).map(|(m, done, _)| (m, done)),
            Some((moves.clone(), 0))
        );
        drop(running);
        // Nothing is left.
        assert!(std::fs::read_dir(&plans).unwrap().next().is_none());
        // A plan finished while another window waits for its lock is not taken for one cut short.
        let finishing = super::start_journal(&plans, &moves).unwrap();
        let ends = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(30));
            drop(finishing);
        });
        assert!(super::cut_short_plans(&plans).is_empty());
        ends.join().unwrap();
        // Cut short after two steps: both files under hidden names.
        std::fs::write(p(".tmp-0"), "a").unwrap();
        std::fs::write(p(".tmp-1"), "b").unwrap();
        let mut cut = super::start_journal(&plans, &moves).unwrap();
        cut.count_step_done();
        cut.count_step_done();
        let plan = cut.path().to_path_buf();
        std::mem::forget(cut);
        // As after a crash, the lock is gone with the process.
        let found = {
            std::fs::copy(&plan, plans.join("crashed")).unwrap();
            super::cut_short_plans(&plans)
        };
        let (path, read, done) = found
            .into_iter()
            .find(|f| f.0.ends_with("crashed"))
            .unwrap();
        assert_eq!((&read, done), (&moves, Some(2)));
        let text = |n| std::fs::read_to_string(p(n)).unwrap();
        // Cut short between the link and the unlink of the third step, b.jpg and .tmp-0 are one
        // file. When finished, the old name goes, and nothing is left to ask about again.
        std::fs::hard_link(p(".tmp-0"), p("b.jpg")).unwrap();
        std::fs::copy(&path, plans.join("linked")).unwrap();
        assert_eq!(
            super::repair_cut_short(&plans.join("linked"), &read, Some(2), false),
            Some((2, 0))
        );
        assert_eq!((text("a.jpg"), text("b.jpg")), ("b".into(), "a".into()));
        assert!(!p(".tmp-0").exists());
        std::fs::rename(p("a.jpg"), p(".tmp-1")).unwrap();
        std::fs::rename(p("b.jpg"), p(".tmp-0")).unwrap();
        // The same is put back. The new name of the half-done step goes, then the two before.
        std::fs::hard_link(p(".tmp-0"), p("b.jpg")).unwrap();
        std::fs::copy(&path, plans.join("linked")).unwrap();
        assert_eq!(
            super::repair_cut_short(&plans.join("linked"), &read, Some(2), true),
            Some((3, 0))
        );
        assert_eq!((text("a.jpg"), text("b.jpg")), ("a".into(), "b".into()));
        std::fs::rename(p("a.jpg"), p(".tmp-0")).unwrap();
        std::fs::rename(p("b.jpg"), p(".tmp-1")).unwrap();
        // While another window puts it right, this one does nothing.
        let other = std::fs::File::open(&path).unwrap();
        other.lock().unwrap();
        assert_eq!(super::repair_cut_short(&path, &read, done, true), None);
        drop(other);
        assert_eq!(
            super::repair_cut_short(&path, &read, done, true),
            Some((2, 0))
        );
        assert_eq!((text("a.jpg"), text("b.jpg")), ("a".into(), "b".into()));
        assert!(!path.exists());
        // One that opened it before it was deleted finds it has no name left.
        std::fs::write(&path, "x").unwrap();
        let opened = std::fs::File::open(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert!(!super::is_linked(&opened).unwrap());
        // A second window that asked about it too finds it gone, and does nothing.
        assert_eq!(super::repair_cut_short(&path, &read, done, true), None);
        assert_eq!((text("a.jpg"), text("b.jpg")), ("a".into(), "b".into()));
        // Finishing instead gives the new names.
        std::fs::rename(p("a.jpg"), p(".tmp-0")).unwrap();
        std::fs::rename(p("b.jpg"), p(".tmp-1")).unwrap();
        std::fs::copy(&plan, plans.join("crashed")).unwrap();
        assert_eq!(
            super::repair_cut_short(&plans.join("crashed"), &moves, Some(2), false),
            Some((2, 0))
        );
        assert_eq!((text("a.jpg"), text("b.jpg")), ("b".into(), "a".into()));
        // Cut short after three steps, where the names alone look like after one, the plan is
        // finished. The swap is whole and the plan gone, so it is not done a second time. It
        // can also be undone.
        let after_three = || {
            std::fs::write(p(".tmp-1"), "b").unwrap();
            std::fs::write(p("b.jpg"), "a").unwrap();
            std::fs::remove_file(p("a.jpg")).unwrap();
            std::fs::copy(&plan, plans.join("crashed")).unwrap();
        };
        after_three();
        assert_eq!(
            super::repair_cut_short(&plans.join("crashed"), &moves, Some(3), false),
            Some((1, 0))
        );
        assert_eq!((text("a.jpg"), text("b.jpg")), ("b".into(), "a".into()));
        assert!(!plans.join("crashed").exists());
        after_three();
        assert_eq!(
            super::repair_cut_short(&plans.join("crashed"), &moves, Some(3), true),
            Some((3, 0))
        );
        assert_eq!((text("a.jpg"), text("b.jpg")), ("a".into(), "b".into()));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// "Put the old names back", chosen after "Later", renames nothing while another file has
    /// taken the name of a step's file since the plan was made, and the plan stays. Once that
    /// file has gone, the old names come back. Before, the other file was given the old name.
    #[test]
    fn a_name_another_file_took_after_later_is_left() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-plan-known-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (plans, p) = (dir.join("plans"), |n: &str| dir.join(n));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(p("a.jpg"), "a").unwrap();
        std::fs::write(p("c.jpg"), "c").unwrap();
        let moves = vec![(p("a.jpg"), p("b.jpg")), (p("c.jpg"), p("d.jpg"))];
        let mut plan = super::start_journal(&plans, &moves).unwrap();
        for (a, b) in &moves {
            super::rename_step(a, b).unwrap();
            plan.count_step_done();
        }
        plan.keep_on_disk();
        // After "Later", the photo at d.jpg goes, and another file takes its name.
        std::fs::remove_file(p("d.jpg")).unwrap();
        std::fs::write(p("d.jpg"), "another").unwrap();
        let (plan, moves, done) = super::cut_short_plans(&plans).remove(0);
        assert_eq!(
            super::repair_cut_short(&plan, &moves, done, true),
            Some((0, 1))
        );
        let read = |n: &str| std::fs::read_to_string(p(n)).unwrap();
        assert_eq!(
            (read("b.jpg"), read("d.jpg")),
            ("a".into(), "another".into())
        );
        assert!(!p("a.jpg").exists() && !p("c.jpg").exists());
        assert_eq!(super::cut_short_plans(&plans).len(), 1);
        std::fs::remove_file(p("d.jpg")).unwrap();
        assert_eq!(
            super::repair_cut_short(&plan, &moves, done, true),
            Some((1, 0))
        );
        assert_eq!(read("a.jpg"), "a");
        assert!(super::cut_short_plans(&plans).is_empty());
        assert!(!plan.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A file of the same size whose time is 1 second off the plan's file is another file, and
    /// the repair leaves it alone. A rename keeps a file's time to the nanosecond, so the plan's
    /// checks compare it exactly. Before, the 2 seconds that a Move to another disk allows
    /// reached them too, and a photo of a batch export written a second later was taken for the
    /// plan's photo and given its old name.
    #[test]
    fn a_file_of_the_same_size_a_second_off_is_another_file() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-plan-second-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (plans, p) = (dir.join("plans"), |n: &str| dir.join(n));
        std::fs::create_dir_all(&dir).unwrap();
        let set_time = |path: &std::path::Path, seconds| {
            let file = std::fs::File::options().write(true).open(path).unwrap();
            let time = std::time::UNIX_EPOCH + std::time::Duration::new(seconds, 500_000_000);
            file.set_modified(time).unwrap();
        };
        std::fs::write(p("a.jpg"), "a").unwrap();
        set_time(&p("a.jpg"), 1_700_000_000);
        let moves = vec![(p("a.jpg"), p("b.jpg"))];
        let mut plan = super::start_journal(&plans, &moves).unwrap();
        super::rename_step(&moves[0].0, &moves[0].1).unwrap();
        plan.count_step_done();
        plan.keep_on_disk();
        // Another photo of the batch, of the same size and a second later, takes b.jpg's name.
        std::fs::remove_file(p("b.jpg")).unwrap();
        std::fs::write(p("b.jpg"), "x").unwrap();
        set_time(&p("b.jpg"), 1_700_000_001);
        let (plan, moves, done) = super::cut_short_plans(&plans).remove(0);
        assert!(super::names_left(&plan, &moves, done, true).contains(": "));
        assert_eq!(
            super::repair_cut_short(&plan, &moves, done, true),
            Some((0, 1))
        );
        assert_eq!(std::fs::read_to_string(p("b.jpg")).unwrap(), "x");
        assert!(!p("a.jpg").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A sidecar that Glimt itself rewrote after "Later" (a keyword saved) does not stop "Put
    /// the old names back". A sidecar follows its photo by name, and only the photos are tied to
    /// their files. Before, every start refused the repair.
    #[test]
    fn a_sidecar_rewritten_after_later_still_goes_back() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-plan-sidecar-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (plans, p) = (dir.join("plans"), |n: &str| dir.join(n));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(p("a.jpg"), "a").unwrap();
        std::fs::write(p("a.jpg.xmp"), "old").unwrap();
        let moves = vec![(p("a.jpg"), p("b.jpg")), (p("a.jpg.xmp"), p("b.jpg.xmp"))];
        let mut plan = super::start_journal(&plans, &moves).unwrap();
        for (a, b) in &moves {
            super::rename_step(a, b).unwrap();
            plan.count_step_done();
        }
        plan.keep_on_disk();
        // After "Later", a keyword is saved: the sidecar is written anew, as a new file.
        std::fs::remove_file(p("b.jpg.xmp")).unwrap();
        std::fs::write(p("b.jpg.xmp"), "new keyword").unwrap();
        let (plan, moves, done) = super::cut_short_plans(&plans).remove(0);
        assert_eq!(
            super::repair_cut_short(&plan, &moves, done, true),
            Some((2, 0))
        );
        let read = |n: &str| std::fs::read_to_string(p(n)).unwrap();
        assert_eq!(
            (read("a.jpg"), read("a.jpg.xmp")),
            ("a".into(), "new keyword".into())
        );
        assert!(super::cut_short_plans(&plans).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A photo of the plan whose look fails, as on a share that dropped for a moment, is not
    /// taken for a photo that is gone: the repair renames nothing, and the plan stays. Before,
    /// the failed look was told as NotFound, and the step ran without its check.
    #[test]
    fn a_photo_that_cannot_be_looked_at_stops_the_repair() {
        use crate::files::file_io::LOOKS_FAIL;
        use crate::support::threads::lock_even_if_poisoned;
        let dir = std::env::temp_dir().join(format!("glimt-test-plan-eio-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (plans, p) = (dir.join("plans"), |n: &str| dir.join(n));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(p("a.jpg"), "a").unwrap();
        let moves = vec![(p("a.jpg"), p("b.jpg"))];
        let mut plan = super::start_journal(&plans, &moves).unwrap();
        super::rename_step(&moves[0].0, &moves[0].1).unwrap();
        plan.count_step_done();
        plan.keep_on_disk();
        let (plan, moves, done) = super::cut_short_plans(&plans).remove(0);
        lock_even_if_poisoned(&LOOKS_FAIL).push(p("b.jpg"));
        let repaired = super::repair_cut_short(&plan, &moves, done, true);
        lock_even_if_poisoned(&LOOKS_FAIL).retain(|f| *f != p("b.jpg"));
        assert_eq!(repaired, Some((0, 1)));
        assert_eq!(std::fs::read_to_string(p("b.jpg")).unwrap(), "a");
        assert_eq!(super::cut_short_plans(&plans).len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A plan that was listed but cannot be read when it is put right (damaged here, a share's
    /// EIO alike) renames nothing and is left as it is, as the files cannot be checked. Before,
    /// a plan's list that could not be read counted as none, and the steps were taken by the
    /// names alone.
    #[test]
    fn a_plan_that_cannot_be_read_stops_the_repair() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-plan-unread-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (plans, p) = (dir.join("plans"), |n: &str| dir.join(n));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(p("a.jpg"), "a").unwrap();
        let moves = vec![(p("a.jpg"), p("b.jpg"))];
        let mut plan = super::start_journal(&plans, &moves).unwrap();
        super::rename_step(&moves[0].0, &moves[0].1).unwrap();
        plan.count_step_done();
        plan.keep_on_disk();
        let (plan, moves, done) = super::cut_short_plans(&plans).remove(0);
        std::fs::write(&plan, [0xff, 0xfe]).unwrap();
        assert_eq!(
            super::repair_cut_short(&plan, &moves, done, true),
            Some((0, 1))
        );
        assert_eq!(std::fs::read_to_string(p("b.jpg")).unwrap(), "a");
        let shown = plan.display().to_string();
        assert!(super::names_left(&plan, &moves, done, true).starts_with(&shown));
        assert!(matches!(
            super::leave_names(&plan, &moves, done),
            Err(Some(_))
        ));
        assert!(plan.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A photo whose device number changed since the plan was made, as a share's, a FUSE
    /// mount's or a card reader's does at a reboot, still goes back. Before, every start refused
    /// the repair, for good.
    #[test]
    fn a_photo_on_a_device_numbered_anew_still_goes_back() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-plan-device-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (plans, p) = (dir.join("plans"), |n: &str| dir.join(n));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(p("a.jpg"), "a").unwrap();
        let moves = vec![(p("a.jpg"), p("b.jpg"))];
        let mut plan = super::start_journal(&plans, &moves).unwrap();
        super::rename_step(&moves[0].0, &moves[0].1).unwrap();
        plan.count_step_done();
        plan.keep_on_disk();
        let (plan, moves, done) = super::cut_short_plans(&plans).remove(0);
        // The plan names the photo by another device, as after a reboot.
        let (Some((device, inode)), size, time) =
            crate::files::file_io::fingerprint(&p("b.jpg")).unwrap()
        else {
            panic!("no device");
        };
        let written = super::Record::Written {
            number: 0,
            fingerprint: (Some((device + 1, inode)), size, time),
        };
        let file = std::fs::File::options().append(true).open(&plan).unwrap();
        (super::JobLog::found(plan.clone(), file).add_records(&[written])).unwrap();
        assert_eq!(
            super::repair_cut_short(&plan, &moves, done, true),
            Some((1, 0))
        );
        assert_eq!(std::fs::read_to_string(p("a.jpg")).unwrap(), "a");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A photo that Glimt itself turns and saves after "Later", and then saves a copy over (Save
    /// As, Crop), still goes back. Its plan takes the photo's new size and time with each save,
    /// so the repair does not take Glimt's own edit for another program's. Before, every start
    /// refused the repair.
    #[test]
    fn a_photo_glimt_rewrote_after_later_still_goes_back() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-plan-turned-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let plans = crate::files::job_log::jobs_dir().unwrap();
        let p = |n: &str| dir.join(n);
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbImage::new(2, 3).save(p("a.png")).unwrap();
        let moves = vec![(p("a.png"), p("b.png"))];
        let mut plan = super::start_journal(&plans, &moves).unwrap();
        super::rename_step(&moves[0].0, &moves[0].1).unwrap();
        plan.count_step_done();
        plan.keep_on_disk();
        let png = |w, h| {
            let mut bytes = std::io::Cursor::new(Vec::new());
            let image = image::RgbImage::new(w, h);
            image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
            bytes.into_inner()
        };
        // After "Later", the photo at b.png is turned and saved, then a copy is saved over it.
        let read = crate::files::file_io::fingerprint_before_reading(&p("b.png")).unwrap();
        let turned = crate::images::save::Derived::Turned;
        crate::images::save::replace_with_sidecar(
            &p("b.png"),
            &p("b.png"),
            read,
            png(3, 2),
            turned,
        )
        .unwrap();
        let asked = crate::files::file_io::fingerprint(&p("b.png"));
        crate::images::save::save_copy_with_sidecar(
            &p("x.png"),
            &p("b.png"),
            &png(4, 4),
            asked,
            true,
        )
        .unwrap();
        let (plan, moves, done) = (super::cut_short_plans(&plans).into_iter())
            .find(|(_, moves, _)| moves[0].0 == p("a.png"))
            .expect("the plan is kept");
        assert_eq!(
            super::repair_cut_short(&plan, &moves, done, true),
            Some((1, 0))
        );
        assert_eq!(image::image_dimensions(p("a.png")).unwrap(), (4, 4));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A repair refused for good (another program changed a photo) has a way out: the list of
    /// the files not under the names they were to get, a swap's hidden names among them, and
    /// then the names left as they are, with the plan gone and nothing renamed or deleted. The
    /// marks follow the renames that were done.
    #[test]
    fn a_refused_repair_can_leave_the_names_as_they_are() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-plan-leave-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (plans, p) = (dir.join("plans"), |n: &str| dir.join(n));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(p("a.jpg"), "a").unwrap();
        std::fs::write(p("b.jpg"), "b").unwrap();
        std::fs::write(p("c.jpg"), "c").unwrap();
        let hidden = |n: &str| p(&format!("{}x-{n}", super::HIDDEN_NAME));
        // A swap of a.jpg and b.jpg, then c.jpg to d.jpg, cut short with a.jpg's photo under
        // its hidden name and c.jpg's at d.jpg.
        let moves = vec![
            (p("a.jpg"), hidden("0")),
            (p("b.jpg"), p("a.jpg")),
            (hidden("0"), p("b.jpg")),
            (p("c.jpg"), p("d.jpg")),
        ];
        let mut plan = super::start_journal(&plans, &moves).unwrap();
        for k in [0, 1] {
            super::rename_step(&moves[k].0, &moves[k].1).unwrap();
            plan.count_step_done();
        }
        plan.keep_on_disk();
        // Another program changes the photo now at a.jpg (b's), so the repair is refused.
        std::fs::write(p("a.jpg"), "b, edited").unwrap();
        let (plan, moves, done) = super::cut_short_plans(&plans).remove(0);
        assert_eq!(
            super::repair_cut_short(&plan, &moves, done, true),
            Some((0, 1))
        );
        let list = super::names_left(&plan, &moves, done, true);
        let line = |now: &str, meant: &str| {
            (list.lines()).find(|l| l.starts_with(&format!("{now} → {meant}")))
        };
        let (a, b, c, d, h) = (p("a.jpg"), p("b.jpg"), p("c.jpg"), p("d.jpg"), hidden("0"));
        let shown = |p: &std::path::PathBuf| p.display().to_string();
        assert!(line(&shown(&h), &shown(&a)).is_some(), "{list}");
        assert!(
            line(&shown(&a), &shown(&b)).is_some_and(|l| l.contains(": ")),
            "{list}"
        );
        assert!(line(&shown(&c), &shown(&c)).is_none(), "{list}");
        assert!(!list.contains(&shown(&d)), "{list}");
        let followed = super::leave_names(&plan, &moves, done).unwrap();
        assert_eq!(
            followed,
            vec![(a.clone(), h.clone()), (b.clone(), a.clone())]
        );
        assert!(super::cut_short_plans(&plans).is_empty());
        assert!(!plan.exists());
        let read = |p: &std::path::PathBuf| std::fs::read_to_string(p).unwrap();
        assert_eq!(
            (read(&h), read(&a), read(&c)),
            ("a".into(), "b, edited".into(), "c".into())
        );
        assert!(!b.exists() && !d.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Leaving the names after a repair that stopped half way gives the marks and collections
    /// only the files that are at their new names now. Before, the count gave IMG_2.jpg's
    /// marks to Trip_2.jpg, where nothing was, after IMG_2.jpg had gone back to its old name.
    #[test]
    fn leaving_the_names_after_a_repair_half_done_follows_each_file() {
        let dir = std::env::temp_dir().join(format!("glimt-test-plan-half-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (plans, p) = (dir.join("plans"), |n: &str| dir.join(n));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(p("IMG_1.jpg"), "1").unwrap();
        std::fs::write(p("IMG_2.jpg"), "2").unwrap();
        let moves = vec![
            (p("IMG_1.jpg"), p("Trip_1.jpg")),
            (p("IMG_2.jpg"), p("Trip_2.jpg")),
        ];
        let mut plan = super::start_journal(&plans, &moves).unwrap();
        for (a, b) in &moves {
            super::rename_step(a, b).unwrap();
            plan.count_step_done();
        }
        plan.keep_on_disk();
        // The card is imported again, so IMG_1.jpg's name is taken: only IMG_2.jpg goes back.
        std::fs::write(p("IMG_1.jpg"), "new").unwrap();
        let (plan, moves, done) = super::cut_short_plans(&plans).remove(0);
        assert_eq!(
            super::repair_cut_short(&plan, &moves, done, true),
            Some((1, 1))
        );
        let followed = super::leave_names(&plan, &moves, done).unwrap();
        assert_eq!(followed, vec![(p("IMG_1.jpg"), p("Trip_1.jpg"))]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Leaving the names gives the marks and collections a file whose name now cannot be looked
    /// at, as the file was there, and tells it. Before, the failed look left the pair out
    /// without a word, and the plan was removed, so the photo lost its marks for good.
    #[test]
    fn leaving_the_names_follows_a_file_that_cannot_be_looked_at() {
        use crate::files::file_io::LOOKS_FAIL;
        use crate::support::threads::lock_even_if_poisoned;
        let dir =
            std::env::temp_dir().join(format!("glimt-test-plan-leave-eio-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (plans, p) = (dir.join("plans"), |n: &str| dir.join(n));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(p("a.jpg"), "a").unwrap();
        let moves = vec![(p("a.jpg"), p("b.jpg"))];
        let mut plan = super::start_journal(&plans, &moves).unwrap();
        super::rename_step(&moves[0].0, &moves[0].1).unwrap();
        plan.count_step_done();
        plan.keep_on_disk();
        let (plan, moves, done) = super::cut_short_plans(&plans).remove(0);
        lock_even_if_poisoned(&LOOKS_FAIL).push(p("b.jpg"));
        let _ = crate::files::file_io::take_saves_failed();
        let followed = super::leave_names(&plan, &moves, done).ok();
        lock_even_if_poisoned(&LOOKS_FAIL).retain(|f| *f != p("b.jpg"));
        assert_eq!(followed, Some(vec![(p("a.jpg"), p("b.jpg"))]));
        let told = crate::files::file_io::take_saves_failed();
        assert!(told.iter().any(|t| t.contains("b.jpg")), "{told:?}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Leaving the names after a repair that stopped half way gives no marks to a name that
    /// cannot be looked at when the photo is back at its old name, as the plan's fingerprint of
    /// it shows. Before, the failed look at Trip_2.jpg gave it IMG_2.jpg's marks, though
    /// nothing was there.
    #[test]
    fn leaving_the_names_keeps_the_marks_of_a_photo_back_at_its_old_name() {
        use crate::files::file_io::LOOKS_FAIL;
        use crate::support::threads::lock_even_if_poisoned;
        let dir =
            std::env::temp_dir().join(format!("glimt-test-plan-back-eio-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (plans, p) = (dir.join("plans"), |n: &str| dir.join(n));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(p("IMG_1.jpg"), "1").unwrap();
        std::fs::write(p("IMG_2.jpg"), "2").unwrap();
        let moves = vec![
            (p("IMG_1.jpg"), p("Trip_1.jpg")),
            (p("IMG_2.jpg"), p("Trip_2.jpg")),
        ];
        let mut plan = super::start_journal(&plans, &moves).unwrap();
        for (a, b) in &moves {
            super::rename_step(a, b).unwrap();
            plan.count_step_done();
        }
        plan.keep_on_disk();
        // IMG_1.jpg's name is taken, so only IMG_2.jpg goes back.
        std::fs::write(p("IMG_1.jpg"), "new").unwrap();
        let (plan, moves, done) = super::cut_short_plans(&plans).remove(0);
        assert_eq!(
            super::repair_cut_short(&plan, &moves, done, true),
            Some((1, 1))
        );
        lock_even_if_poisoned(&LOOKS_FAIL).push(p("Trip_2.jpg"));
        let followed = super::leave_names(&plan, &moves, done).ok();
        lock_even_if_poisoned(&LOOKS_FAIL).retain(|f| *f != p("Trip_2.jpg"));
        assert_eq!(followed, Some(vec![(p("IMG_1.jpg"), p("Trip_1.jpg"))]));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The plan is on disk with its name before the first rename, and the folder of the renames
    /// is synced before the plan is removed, once however many files it renames. A plan kept on
    /// disk needs no such sync.
    #[test]
    fn the_plan_goes_only_after_the_renames_are_on_disk() {
        let dir = std::env::temp_dir().join(format!("glimt-test-plan-sync-{}", std::process::id()));
        let plans = dir.join("plans");
        let p = |n: &str| dir.join("photos").join(n);
        let moves = vec![(p("a.jpg"), p("b.jpg")), (p("c.jpg"), p("d.jpg"))];
        crate::files::file_io::FOLDERS_SYNCED.take();
        let plan = super::start_journal(&plans, &moves).unwrap();
        assert_eq!(
            crate::files::file_io::FOLDERS_SYNCED.take(),
            [plans.clone(), dir.clone()]
        );
        drop(plan);
        assert_eq!(
            crate::files::file_io::FOLDERS_SYNCED.take(),
            [dir.join("photos"), plans.clone()]
        );
        let kept = super::start_journal(&plans, &moves).unwrap();
        crate::files::file_io::FOLDERS_SYNCED.take();
        kept.keep_on_disk();
        assert!(crate::files::file_io::FOLDERS_SYNCED.take().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// When the disk says that the renames' folder did not get there, the user is told, and the
    /// plan goes all the same, both after a Rename Many and after a repair. The marks and
    /// collections have followed the new names by then, and a plan kept beside them would move
    /// them again at the next start, or swap a swap's marks back.
    #[test]
    fn the_plan_goes_and_the_user_is_told_when_the_renames_did_not_reach_the_disk() {
        use crate::files::file_io::{FOLDER_SYNC_FAILS, take_saves_failed};
        let dir =
            std::env::temp_dir().join(format!("glimt-test-sync-fails-{}", std::process::id()));
        let plans = dir.join("plans");
        let moves = vec![(dir.join("a.jpg"), dir.join("b.jpg"))];
        // What this thread told, which no other test can take.
        let told_once = || take_saves_failed().len() == 1;
        let plan = super::start_journal(&plans, &moves).unwrap();
        let path = plan.path().to_path_buf();
        take_saves_failed();
        FOLDER_SYNC_FAILS.set(true);
        drop(plan);
        FOLDER_SYNC_FAILS.set(false);
        assert!(!path.exists() && told_once());
        let plan = super::start_journal(&plans, &moves).unwrap();
        let path = plan.path().to_path_buf();
        plan.keep_on_disk();
        FOLDER_SYNC_FAILS.set(true);
        assert_eq!(
            super::repair_cut_short(&path, &moves, Some(1), false),
            Some((0, 0))
        );
        FOLDER_SYNC_FAILS.set(false);
        assert!(!path.exists() && told_once());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// When the middle of a swap does not reach the disk, it is not counted. A count of half
    /// would otherwise tell the next start that the swap was done while the names are those
    /// of the start.
    #[test]
    fn the_middle_of_a_swap_is_not_counted_when_its_names_did_not_reach_the_disk() {
        use crate::files::file_io::FOLDER_SYNC_FAILS;
        let dir = std::env::temp_dir().join(format!("glimt-test-middle-{}", std::process::id()));
        let moves = vec![(dir.join("a"), dir.join("b"))];
        let mut plan = super::start_journal(&dir, &moves).unwrap();
        FOLDER_SYNC_FAILS.set(true);
        let counted = plan.count_middle_of_swap_done();
        FOLDER_SYNC_FAILS.set(false);
        assert!(counted.is_err());
        plan.keep_on_disk();
        assert_eq!(super::cut_short_plans(&dir)[0].2, Some(0));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The middle of a swap whose count cannot be written (a full state disk) is not taken as
    /// counted, so that the caller undoes the swap. Before, the failure was dropped, and a kill
    /// just after the swap left a plan that the next start read as a swap not begun, whose
    /// Finish swapped the photos back and put the marks on the other photo.
    #[test]
    fn the_middle_of_a_swap_is_not_counted_when_its_count_did_not_reach_the_disk() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-middle-count-{}", std::process::id()));
        let moves = vec![(dir.join("a"), dir.join("b"))];
        let mut plan = super::start_journal(&dir, &moves).unwrap();
        plan.refuse_writes();
        assert!(plan.count_middle_of_swap_done().is_err());
        drop(plan);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A plan kept when its renames could not be undone is there at the next start, with the
    /// steps it had done.
    #[test]
    fn a_kept_plan_stays() {
        let dir = std::env::temp_dir().join(format!("glimt-test-kept-{}", std::process::id()));
        let moves = vec![(dir.join("a"), dir.join("b"))];
        let mut plan = super::start_journal(&dir, &moves).unwrap();
        plan.count_step_done();
        plan.keep_on_disk();
        let left = super::cut_short_plans(&dir);
        assert_eq!((left.len(), left[0].2), (1, Some(1)));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A swap's steps are followed to their ends. a goes to b and b to a, and a plain rename is
    /// itself.
    #[test]
    fn pairs_follow_the_steps() {
        let p = |n: &str| std::path::PathBuf::from(n);
        let moves = vec![
            (p("a"), p(".t0")),
            (p("b"), p(".t1")),
            (p(".t0"), p("b")),
            (p(".t1"), p("a")),
        ];
        assert_eq!(
            super::pairs(&moves),
            vec![(p("a"), p("b")), (p("b"), p("a"))]
        );
        assert_eq!(super::pairs(&[(p("c"), p("d"))]), vec![(p("c"), p("d"))]);
        // A swap of 40,000 files is followed in a moment, not in minutes.
        let n = 20_000;
        let mut moves = Vec::new();
        for k in 0..n {
            moves.push((p(&format!("a{k}")), p(&format!(".t{k}"))));
            moves.push((p(&format!("b{k}")), p(&format!("a{k}"))));
        }
        moves.extend((0..n).map(|k| (p(&format!(".t{k}")), p(&format!("b{k}")))));
        let start = std::time::Instant::now();
        let pairs = super::pairs(&moves);
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
        assert_eq!(pairs.len(), 2 * n);
        assert_eq!(pairs[0], (p("a0"), p("b0")));
        assert_eq!(pairs[1], (p("b0"), p("a0")));
    }

    /// A step whose old name cannot be looked at counts as left, not as gone, so the plan is
    /// kept. Here the error is "not a folder"; on a share it is EIO.
    #[test]
    fn a_name_that_cannot_be_read_is_left() {
        let dir = std::env::temp_dir().join(format!("glimt-test-eio-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("f"), b"").unwrap();
        let moves = [(dir.join("f/a.jpg"), dir.join("b.jpg"))];
        assert_eq!(super::finish(&moves, 0), (0, 1));
        assert_eq!(
            super::finish(&[(dir.join("gone"), dir.join("b.jpg"))], 0),
            (0, 0)
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A step left under both names, where one of them may be gone from a share whose cache
    /// still shows it, keeps the other name. It is removed only when an open of each name finds
    /// one file, both as the step is done and as the plan is put back.
    #[test]
    fn a_second_name_goes_only_when_the_other_opens() {
        use crate::images::save::GONE_FROM_THE_SERVER;
        let dir = std::env::temp_dir().join(format!("glimt-test-cached-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let plans = dir.join("plans");
        std::fs::create_dir_all(&plans).unwrap();
        let p = |n: &str| dir.join(n);
        let moves = vec![(p("a.jpg"), p("b.jpg"))];
        std::fs::write(p("a.jpg"), "a").unwrap();
        std::fs::hard_link(p("a.jpg"), p("b.jpg")).unwrap();
        GONE_FROM_THE_SERVER.set(Some(p("b.jpg")));
        let step = super::rename_step(&p("a.jpg"), &p("b.jpg"));
        GONE_FROM_THE_SERVER.set(None);
        assert!(step.is_err() && p("a.jpg").exists());
        let plan = plans.join("cut");
        let cut = super::start_journal(&plans, &moves).unwrap();
        std::fs::copy(cut.path(), &plan).unwrap();
        drop(cut);
        GONE_FROM_THE_SERVER.set(Some(p("a.jpg")));
        super::repair_cut_short(&plan, &moves, Some(0), true);
        GONE_FROM_THE_SERVER.set(None);
        assert!(p("b.jpg").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The failing path and the recovery share one undo. Two steps of a swap are done and then
    /// undone, last first. One of them put back by hand already is left out, and not counted as
    /// stuck.
    #[test]
    fn undoes_the_steps_done() {
        let dir = std::env::temp_dir().join(format!("glimt-test-undo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = |n: &str| dir.join(n);
        std::fs::write(p("a"), "a").unwrap();
        std::fs::write(p("b"), "b").unwrap();
        let moves = vec![(p("a"), p(".t0")), (p("b"), p(".t1")), (p(".t0"), p("b"))];
        assert_eq!(super::finish(&moves, 0), (3, 0));
        assert!(!p("a").exists() && p(".t1").exists());
        assert_eq!(std::fs::read_to_string(p("b")).unwrap(), "a");
        assert_eq!(super::undo(&moves, 3), (3, 0));
        assert_eq!(std::fs::read_to_string(p("a")).unwrap(), "a");
        assert_eq!(std::fs::read_to_string(p("b")).unwrap(), "b");
        // Two are done, and one of them is put back by hand, so only the other is undone.
        std::fs::rename(p("a"), p(".t0")).unwrap();
        std::fs::rename(p("b"), p(".t1")).unwrap();
        std::fs::rename(p(".t1"), p("b")).unwrap();
        assert_eq!(super::undo(&moves, 2), (1, 0));
        assert!(p("a").exists() && p("b").exists() && !p(".t0").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Every place a Rename Many can be cut short is tried, also between the link and the
    /// unlink of one step, for a swap, a cycle of three, a shift and plain new names. Putting
    /// back gives every old name, finishing gives every new one, no hidden name is left, and a
    /// second repair does nothing.
    #[test]
    fn repairs_every_cut() {
        let dir = std::env::temp_dir().join(format!("glimt-test-cuts-{}", std::process::id()));
        let plans = dir.join("plans");
        let p = |n: &str| dir.join(n);
        let cases: [(&[&str], &[&str]); 4] = [
            (&["a", "b"], &["b", "a"]),
            (&["a", "b", "c"], &["b", "c", "a"]),
            (&["a", "b"], &["b", "c"]),
            (&["a", "b"], &["x", "y"]),
        ];
        for (from, to) in cases {
            // This is the plan as rename_many makes it, through hidden names when a new name is
            // an old one.
            let mut moves: super::Moves = Vec::new();
            if to.iter().any(|t| from.contains(t)) {
                let tmp = |k: usize| p(&format!(".tmp-{k}"));
                moves.extend(from.iter().enumerate().map(|(k, f)| (p(f), tmp(k))));
                moves.extend(to.iter().enumerate().map(|(k, t)| (tmp(k), p(t))));
            } else {
                moves.extend(from.iter().zip(to).map(|(f, t)| (p(f), p(t))));
            }
            for cut in 0..=moves.len() {
                for half in [false, true] {
                    if half && cut == moves.len() {
                        continue;
                    }
                    for undo in [false, true] {
                        let _ = std::fs::remove_dir_all(&dir);
                        std::fs::create_dir_all(&dir).unwrap();
                        for (k, f) in from.iter().enumerate() {
                            write_with_time(&p(f), f, k);
                        }
                        let mut plan = super::start_journal(&plans, &moves).unwrap();
                        for (a, b) in &moves[..cut] {
                            std::fs::rename(a, b).unwrap();
                            plan.count_step_done();
                        }
                        if half {
                            std::fs::hard_link(&moves[cut].0, &moves[cut].1).unwrap();
                        }
                        // As after a crash, the plan is on disk and no longer locked.
                        let bytes = std::fs::read(plan.path()).unwrap();
                        drop(plan);
                        std::fs::write(plans.join("crashed"), bytes).unwrap();
                        let [(path, read, done)] = &super::cut_short_plans(&plans)[..] else {
                            panic!("one plan");
                        };
                        assert_eq!((read, *done), (&moves, Some(cut)));
                        let (_, left) = super::repair_cut_short(path, read, *done, undo).unwrap();
                        let case = format!("{from:?} -> {to:?}, cut {cut}, half {half}");
                        assert_eq!(left, 0, "{case}");
                        assert!(!path.exists(), "{case}");
                        let names = if undo { from } else { to };
                        for (f, n) in from.iter().zip(names) {
                            assert_eq!(std::fs::read_to_string(p(n)).unwrap(), *f, "{case}");
                        }
                        let mut there: Vec<String> = (std::fs::read_dir(&dir).unwrap())
                            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                            .filter(|n| n != "plans")
                            .collect();
                        there.sort();
                        let mut want: Vec<String> = names.iter().map(|n| n.to_string()).collect();
                        want.sort();
                        assert_eq!(there, want, "{case}");
                        assert_eq!(
                            super::repair_cut_short(path, read, *done, undo),
                            None,
                            "{case}"
                        );
                    }
                }
            }
            for cut in 0..moves.len() {
                for counted in cut + 1..=moves.len() {
                    for undo in [false, true] {
                        let case = format!("{from:?} -> {to:?}, cut {cut}, counted {counted}");
                        let cuts = (cut, counted);
                        repair_counted_ahead(&dir, &moves, (from, to), cuts, undo, &case);
                    }
                }
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Writes `text` to `path` with a time of its own, `k` seconds after a fixed one. Files written
    /// a moment apart often get the same time, and the repair's check of each file by its size
    /// and time then passed also on the wrong file, so a test passed or failed by chance.
    fn write_with_time(path: &std::path::Path, text: &str, k: usize) {
        std::fs::write(path, text).unwrap();
        let time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000 + k as u64);
        let file = std::fs::File::options().write(true).open(path).unwrap();
        file.set_modified(time).unwrap();
    }

    /// Cuts `moves` short after `cut` steps on disk, with `counted` of them counted in the plan,
    /// as when the count reached its disk and the photos' folder did not. Then it puts them
    /// right, and checks that no photo is left under a hidden name or lost.
    fn repair_counted_ahead(
        dir: &std::path::Path,
        moves: &super::Moves,
        (from, to): (&[&str], &[&str]),
        (cut, counted): (usize, usize),
        undo: bool,
        case: &str,
    ) {
        let plans = dir.join("plans");
        let _ = std::fs::remove_dir_all(dir);
        std::fs::create_dir_all(dir).unwrap();
        for (k, f) in from.iter().enumerate() {
            write_with_time(&dir.join(f), f, k);
        }
        let mut plan = super::start_journal(&plans, moves).unwrap();
        for (a, b) in &moves[..cut] {
            std::fs::rename(a, b).unwrap();
        }
        for _ in 0..counted {
            plan.count_step_done();
        }
        let bytes = std::fs::read(plan.path()).unwrap();
        drop(plan);
        std::fs::write(plans.join("crashed"), bytes).unwrap();
        let [(path, read, done)] = &super::cut_short_plans(&plans)[..] else {
            panic!("one plan");
        };
        // A swap not begun has the names of one done. Its count is below half then, because the
        // middle of a swap is on disk, names first, before its count (`rename_files`). A count
        // of half or more with no step on disk cannot happen, and which of the two it was
        // cannot be told. The repair then takes the swap as done, and an undo is refused when
        // a photo's check by its size and time finds the other photo's name holding it, which
        // leaves every name as it is. It passed only when both files had the same time.
        let cannot_be_told =
            cut == 0 && counted >= moves.len() / 2 && to.iter().all(|t| from.contains(t));
        let (_, left) = super::repair_cut_short(path, read, *done, undo).unwrap();
        if !cannot_be_told {
            assert_eq!(left, 0, "{case}");
            assert!(!path.exists(), "{case}");
        }
        // Each photo is under a name it is meant to have, and none under a hidden one.
        let mut there: Vec<(String, String)> = (std::fs::read_dir(dir).unwrap())
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n != "plans")
            .map(|n| (std::fs::read_to_string(dir.join(&n)).unwrap(), n))
            .collect();
        there.sort();
        assert!(
            there.iter().all(|(_, n)| !n.starts_with('.')),
            "{case}: {there:?}"
        );
        let photos: Vec<&str> = there.iter().map(|(photo, _)| photo.as_str()).collect();
        assert_eq!(photos, from, "{case}");
        if cannot_be_told {
            return;
        }
        let names = if undo { from } else { to };
        for (f, n) in from.iter().zip(names) {
            assert_eq!(std::fs::read_to_string(dir.join(n)).unwrap(), *f, "{case}");
        }
    }

    /// A plan that could not be removed is told as a failed write, as the next start would
    /// offer it again and could undo the finished rename. Before, the failure was dropped.
    #[test]
    fn a_plan_not_removed_is_told() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-plan-stays-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A plan below a file, which no file system removes.
        std::fs::write(dir.join("file"), "").unwrap();
        let _ = crate::files::file_io::take_saves_failed();
        let sure = crate::files::job_log::Sure::of(&Ok::<(), ()>(())).unwrap();
        super::remove_plan(&dir.join("file").join("plan"), &[], sure);
        let failed = crate::files::file_io::take_saves_failed();
        assert!(
            failed.iter().any(|e| e.contains("Could not remove")),
            "{failed:?}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Leaves in `logs` the log of a Delete for good of the photo `a` that kept the photo, as a
    /// share that lost an answer leaves it: the photo's sidecar is at the name `hidden`, put
    /// there by `hide`, and the log stays for a later start.
    fn leave_a_kept_log(
        logs: &std::path::Path,
        a: &std::path::Path,
        hidden: &std::path::Path,
        hide: impl FnOnce(&std::path::Path, &std::path::Path),
    ) {
        let side = crate::metadata::xmp::sidecar(a);
        let mut log = super::start_delete_log(logs, "delete", None).unwrap();
        log.note(&[a.to_path_buf()]);
        log.note_aside(a, &[(side.clone(), hidden.to_path_buf())]);
        hide(&side, hidden);
        log.keep_for_a_later_start(a);
        log.remove_once_forgotten();
        crate::files::file_io::wait_for_queued_writes();
        crate::files::file_io::wait_for_queued_writes();
    }

    /// Lets go of each log that a start found, and returns how many there were.
    fn let_go(found: Vec<(super::DeleteLog, Vec<std::path::PathBuf>)>) -> usize {
        let count = found.len();
        found
            .into_iter()
            .for_each(|(log, _)| log.remove_once_forgotten());
        crate::files::file_io::wait_for_queued_writes();
        crate::files::file_io::wait_for_queued_writes();
        count
    }

    /// A log that a Delete for good kept for a later start, because the photo stayed while its
    /// sidecar may still be hidden, does not take the photo for one that the delete deleted
    /// when the user has moved it away since. Its hidden sidecar stays, and its name is told
    /// once, as the log goes. Before, the start took the photo for one deleted, and deleted its
    /// hidden sidecar with its keywords for good.
    #[test]
    fn a_kept_log_keeps_the_hidden_sidecar_of_a_photo_moved_away_since() {
        let dir = crate::support::test_support::contract_folder("start-kept-moved");
        let logs = dir.join("logs");
        let a = dir.join("a.png");
        let hidden = crate::files::file_io::tmp_path(&crate::metadata::xmp::sidecar(&a));
        leave_a_kept_log(&logs, &a, &hidden, |side, hidden| {
            std::fs::rename(side, hidden).unwrap();
        });
        std::fs::rename(&a, dir.join("moved.png")).unwrap();
        let _ = crate::files::file_io::take_saves_failed();
        let found = super::delete_jobs_cut_short(&logs);
        assert!(
            found.iter().all(|(_, gone)| gone.is_empty()),
            "{:?}",
            found[0].1
        );
        assert_eq!(std::fs::read_to_string(&hidden).unwrap(), "a");
        let told = crate::files::file_io::take_saves_failed().join("\n");
        assert!(told.contains(&hidden.display().to_string()), "{told}");
        assert_eq!(let_go(found), 1);
        assert_eq!(
            let_go(super::delete_jobs_cut_short(&logs)),
            0,
            "the kept log stayed"
        );
        assert!(crate::files::file_io::take_saves_failed().is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A kept log whose sidecar is under both its own name and the hidden name, as a share that
    /// linked it and lost the answer of the unlink leaves it, loses the hidden name, and the log
    /// goes. Before, every start failed to give the sidecar back over its own name, told it, and
    /// kept the log.
    #[test]
    fn a_kept_log_of_a_sidecar_under_both_names_takes_the_hidden_name_away() {
        let dir = crate::support::test_support::contract_folder("start-kept-both-names");
        let logs = dir.join("logs");
        let a = dir.join("a.png");
        let side = crate::metadata::xmp::sidecar(&a);
        let hidden = crate::files::file_io::tmp_path(&side);
        leave_a_kept_log(&logs, &a, &hidden, |side, hidden| {
            std::fs::hard_link(side, hidden).unwrap();
        });
        let _ = crate::files::file_io::take_saves_failed();
        assert_eq!(let_go(super::delete_jobs_cut_short(&logs)), 1);
        let told = crate::files::file_io::take_saves_failed();
        assert!(told.is_empty(), "{told:?}");
        assert!(!hidden.exists());
        assert_eq!(std::fs::read_to_string(&side).unwrap(), "a");
        assert_eq!(
            let_go(super::delete_jobs_cut_short(&logs)),
            0,
            "the kept log stayed"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A kept log whose sidecar cannot be given back its own name, because another sidecar was
    /// written at that name since, tells both names once, and the log goes. Before, the log
    /// stayed and every start told the same failure again.
    #[test]
    fn a_kept_log_of_a_sidecar_whose_name_is_taken_tells_it_once() {
        let dir = crate::support::test_support::contract_folder("start-kept-taken");
        let logs = dir.join("logs");
        let a = dir.join("a.png");
        let side = crate::metadata::xmp::sidecar(&a);
        let hidden = crate::files::file_io::tmp_path(&side);
        leave_a_kept_log(&logs, &a, &hidden, |side, hidden| {
            std::fs::rename(side, hidden).unwrap();
        });
        std::fs::write(&side, "another").unwrap();
        let _ = crate::files::file_io::take_saves_failed();
        assert_eq!(let_go(super::delete_jobs_cut_short(&logs)), 1);
        let told = crate::files::file_io::take_saves_failed().join("\n");
        assert!(told.contains(&hidden.display().to_string()), "{told}");
        assert!(told.contains(&side.display().to_string()), "{told}");
        assert_eq!(
            let_go(super::delete_jobs_cut_short(&logs)),
            0,
            "the kept log stayed"
        );
        let told = crate::files::file_io::take_saves_failed();
        assert!(told.is_empty(), "told again: {told:?}");
        assert_eq!(std::fs::read_to_string(&hidden).unwrap(), "a");
        assert_eq!(std::fs::read_to_string(&side).unwrap(), "another");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A delete's log whose folder the user has since deleted, with the folder above it, gives
    /// its files as gone, and their marks are forgotten. A folder gone under an empty mount point
    /// on another device, as on a stick taken out, keeps the log. Before, a deleted folder counted
    /// as a stick taken out, the log stayed for ever, and the marks waited for a later photo at
    /// the name.
    #[test]
    fn a_log_whose_folder_was_deleted_forgets_its_files() {
        let dir = crate::support::test_support::contract_folder("start-folder-deleted");
        let logs = dir.join("logs");
        let (album, stick) = (dir.join("new/album"), dir.join("stick"));
        let (deleted, on_stick) = (album.join("a.png"), stick.join("photos/b.png"));
        for file in [&deleted, &on_stick] {
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, "a").unwrap();
        }
        let mut log = super::start_delete_log(&logs, "trash", None).unwrap();
        log.note(&[deleted.clone(), on_stick.clone()]);
        drop(log);
        std::fs::remove_dir_all(dir.join("new")).unwrap();
        std::fs::remove_dir_all(stick.join("photos")).unwrap();
        super::ON_ANOTHER_DEVICE.with_borrow_mut(|folders| folders.push(stick.clone()));
        let found = super::delete_jobs_cut_short(&logs);
        super::ON_ANOTHER_DEVICE.with_borrow_mut(Vec::clear);
        assert_eq!(found[0].1, std::slice::from_ref(&deleted));
        assert_eq!(let_go(found), 1, "the stick's file keeps the log");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A log let go for a later start is written anew in the folder of the job logs with only
    /// the steps that the start could not tell about, each with its fingerprint, and keeps its
    /// time. Before, every later start looked again at every name of the log, three looks for
    /// each of the 500,000 files of a killed delete. A kill after the new log is on disk and
    /// before the old one went leaves both, and the next start removes the old one unread.
    #[test]
    fn a_log_let_go_for_a_later_start_keeps_only_the_names_left() {
        use crate::files::job_log::{LogRead, Record};
        let dir = crate::support::test_support::contract_folder("start-names-left");
        let logs = dir.join("logs");
        let (a, b) = (dir.join("a.png"), dir.join("b.png"));
        let stick = dir.join("stick");
        std::fs::create_dir(&stick).unwrap();
        let on_stick = stick.join("c.png");
        std::fs::write(&on_stick, "c").unwrap();
        let known = crate::files::file_io::fingerprint(&on_stick);
        let (device, inode) = crate::files::file_io::fingerprint(&stick)
            .unwrap()
            .0
            .unwrap();
        let mut log = super::start_delete_log(&logs, "trash", None).unwrap();
        log.note(&[a.clone(), b.clone(), on_stick.clone()]);
        drop(log);
        std::fs::remove_file(&b).unwrap();
        // The stick is not plugged in at the start: its mount point is an empty folder on
        // another device.
        std::fs::rename(&stick, dir.join("stick-away")).unwrap();
        std::fs::create_dir(&stick).unwrap();
        super::ON_ANOTHER_DEVICE.with_borrow_mut(|folders| folders.push(stick.clone()));
        let old = the_log_in(&logs);
        let old_bytes = std::fs::read(&old).unwrap();
        let written = std::fs::metadata(&old).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let found = super::delete_jobs_cut_short(&logs);
        assert_eq!(found[0].1, std::slice::from_ref(&b));
        assert_eq!(let_go(found), 1);
        let new = the_log_in(&logs);
        assert_ne!(new, old);
        assert!(std::fs::metadata(&new).unwrap().modified().unwrap() <= written);
        let (held, read) = crate::files::job_log::read_job_log(&new).unwrap();
        drop(held);
        let LogRead::Log { job, records, .. } = read else {
            panic!("not a log: {read:?}");
        };
        assert_eq!(job, "trash");
        let step = Record::Step {
            number: 0,
            from: on_stick.clone(),
            to: None,
            fingerprint: known,
        };
        let folder = Record::Folder {
            path: stick.clone(),
            device,
            inode,
        };
        assert_eq!(records, [folder, step, Record::OldLog(old.clone())]);
        // A kill cut the new log off while it was written: it does not name the old one, which
        // the next start then reads again instead of removing it unread.
        let new_bytes = std::fs::read(&new).unwrap();
        let cut = dir.join("cut-off");
        std::fs::write(&cut, &new_bytes[..new_bytes.len() - 1]).unwrap();
        let (held, read) = crate::files::job_log::read_job_log(&cut).unwrap();
        drop(held);
        let LogRead::Log { records, .. } = read else {
            panic!("not a log: {read:?}");
        };
        assert!(!records.contains(&Record::OldLog(old.clone())));
        std::fs::remove_file(&cut).unwrap();
        // A kill came before the old log went: the next start removes it unread.
        std::fs::write(&old, &old_bytes).unwrap();
        let found = super::delete_jobs_cut_short(&logs);
        assert_eq!(found.len(), 1);
        assert!(found[0].1.is_empty(), "{:?}", found[0].1);
        assert!(!old.exists(), "the old log was read again");
        assert_eq!(let_go(found), 1);
        // The stick is back, without c.png, which went before the kill: the log goes.
        super::ON_ANOTHER_DEVICE.with_borrow_mut(Vec::clear);
        std::fs::remove_dir(&stick).unwrap();
        std::fs::rename(dir.join("stick-away"), &stick).unwrap();
        std::fs::remove_file(&on_stick).unwrap();
        let found = super::delete_jobs_cut_short(&logs);
        assert_eq!(found[0].1, std::slice::from_ref(&on_stick));
        assert_eq!(let_go(found), 1);
        let left = std::fs::read_dir(&logs).unwrap().flatten();
        assert_eq!(left.filter(|e| e.path().is_file()).count(), 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Returns the path of the one log in the folder of logs `logs`.
    fn the_log_in(logs: &std::path::Path) -> std::path::PathBuf {
        (std::fs::read_dir(logs).unwrap().flatten())
            .map(|e| e.path())
            .find(|p| p.is_file())
            .unwrap()
    }

    /// Leaves in `logs` the log of a Delete for good of the photo `a` that Glimt was killed
    /// during, after it put the photo's sidecar aside at the name `hidden` and before it deleted
    /// the photo.
    fn leave_a_killed_log(logs: &std::path::Path, a: &std::path::Path, hidden: &std::path::Path) {
        let side = crate::metadata::xmp::sidecar(a);
        let mut log = super::start_delete_log(logs, "delete", None).unwrap();
        log.note(&[a.to_path_buf()]);
        log.note_aside(a, &[(side.clone(), hidden.to_path_buf())]);
        std::fs::rename(&side, hidden).unwrap();
        drop(log);
    }

    /// A start with another card mounted at the path of the folder of a killed delete, as two
    /// cards of one camera are, keeps the marks of the files that are not on it, and the log,
    /// and a start with the card back finds the files there. Before, the files not on the other
    /// card counted as deleted, and their marks were forgotten (#179).
    #[test]
    fn a_killed_delete_keeps_the_marks_while_another_card_is_at_its_path() {
        let dir = crate::support::test_support::contract_folder("start-other-card");
        crate::files::file_ops::test_mount(&dir, Some("ext4"));
        let logs = dir.join("logs");
        let (card, card_a) = (dir.join("card"), dir.join("card-a"));
        std::fs::create_dir(&card).unwrap();
        let a = card.join("a.png");
        std::fs::write(&a, "a").unwrap();
        let mut log = super::start_delete_log(&logs, "trash", None).unwrap();
        log.note(std::slice::from_ref(&a));
        drop(log);
        // Card A comes out, and card B, without a.png, is mounted at the same path.
        std::fs::rename(&card, &card_a).unwrap();
        std::fs::create_dir(&card).unwrap();
        let found = super::delete_jobs_cut_short(&logs);
        assert!(found[0].1.is_empty(), "a.png's marks were forgotten");
        assert_eq!(let_go(found), 1);
        // Card A is back.
        std::fs::remove_dir(&card).unwrap();
        std::fs::rename(&card_a, &card).unwrap();
        let found = super::delete_jobs_cut_short(&logs);
        assert_eq!(found.len(), 1, "the log did not stay for card A");
        assert!(found[0].1.is_empty());
        assert_eq!(let_go(found), 1);
        assert_eq!(let_go(super::delete_jobs_cut_short(&logs)), 0);
        crate::files::file_ops::test_mount(&dir, None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A start long after the delete of a log takes back from the trash only a sidecar that went
    /// there about the time of the delete. Before, it took back any sidecar of a photo that
    /// stayed trashed since then, so one the user trashed on purpose weeks after the kill came
    /// back, also at every start of a log kept for a later one.
    #[test]
    fn a_late_start_takes_back_no_sidecar_trashed_after_the_delete() {
        let dir = crate::support::test_support::contract_folder("start-late-trash");
        let logs = dir.join("logs");
        let a = dir.join("a.png");
        let side = crate::metadata::xmp::sidecar(&a);
        let mut log = super::start_delete_log(&logs, "delete", None).unwrap();
        log.note(std::slice::from_ref(&a));
        drop(log);
        // The kill was two hours ago, and the user trashed the sidecar now.
        let two_hours_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(7200);
        let opened = std::fs::File::options().write(true).open(the_log_in(&logs));
        opened.unwrap().set_modified(two_hours_ago).unwrap();
        crate::files::trash::trash(std::slice::from_ref(&side)).unwrap();
        assert_eq!(let_go(super::delete_jobs_cut_short(&logs)), 1);
        assert!(!side.exists(), "the sidecar trashed on purpose came back");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A start after a killed Move to Trash takes back Adobe's sidecar of a RAW file that stayed
    /// only when no other RAW file of its name stem is in the folder, as the move itself asks
    /// (`xmp::adobe_sidecars_of`). Before, IMG_1.xmp came back beside IMG_1.CR2 also with an
    /// IMG_1.NEF beside it, whose sidecar it may be.
    #[test]
    fn a_start_takes_back_no_adobe_sidecar_that_two_raw_files_share() {
        let dir = crate::support::test_support::contract_folder("start-adobe-shared");
        let logs = dir.join("logs");
        let (cr2, nef) = (dir.join("IMG_1.CR2"), dir.join("IMG_1.NEF"));
        std::fs::write(&cr2, "cr2").unwrap();
        std::fs::write(&nef, "nef").unwrap();
        let side = dir.join("IMG_1.xmp");
        std::fs::write(&side, "whose").unwrap();
        let mut log = super::start_delete_log(&logs, "delete", None).unwrap();
        log.note(std::slice::from_ref(&cr2));
        crate::files::trash::trash(std::slice::from_ref(&side)).unwrap();
        drop(log);
        let _ = crate::files::file_io::take_saves_failed();
        assert_eq!(let_go(super::delete_jobs_cut_short(&logs)), 1);
        assert!(
            !side.exists(),
            "the sidecar of two RAW files came back to one"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A start after a killed Delete for good, whose photo stayed but was changed before it, as
    /// a JPEG turned in another program, keeps the photo's sidecar under its hidden name and
    /// tells that name, once. Before, the log went without a word, and the sidecar stayed
    /// hidden.
    #[test]
    fn a_killed_delete_for_good_tells_the_hidden_sidecar_of_a_photo_changed_since() {
        let dir = crate::support::test_support::contract_folder("start-changed-hidden");
        let logs = dir.join("logs");
        let a = dir.join("a.png");
        let hidden = crate::files::file_io::tmp_path(&crate::metadata::xmp::sidecar(&a));
        leave_a_killed_log(&logs, &a, &hidden);
        std::fs::write(&a, "turned, of another size").unwrap();
        let _ = crate::files::file_io::take_saves_failed();
        assert_eq!(let_go(super::delete_jobs_cut_short(&logs)), 1);
        assert_eq!(std::fs::read_to_string(&hidden).unwrap(), "a");
        let told = crate::files::file_io::take_saves_failed().join("\n");
        assert!(told.contains(&hidden.display().to_string()), "{told}");
        assert_eq!(let_go(super::delete_jobs_cut_short(&logs)), 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A start whose look at a hidden sidecar fails, as on a share that stopped answering for a
    /// moment, keeps the log, and a later start gives the sidecar back. Before, the start passed
    /// over the sidecar and the log went, so it stayed hidden for good without a word.
    #[test]
    fn a_hidden_sidecar_that_cannot_be_looked_at_keeps_its_log() {
        use crate::files::file_io::LOOKS_FAIL;
        use crate::support::threads::lock_even_if_poisoned;
        let dir = crate::support::test_support::contract_folder("start-hidden-unseen");
        let logs = dir.join("logs");
        let a = dir.join("a.png");
        let side = crate::metadata::xmp::sidecar(&a);
        let hidden = crate::files::file_io::tmp_path(&side);
        leave_a_killed_log(&logs, &a, &hidden);
        lock_even_if_poisoned(&LOOKS_FAIL).push(hidden.clone());
        let found = super::delete_jobs_cut_short(&logs);
        lock_even_if_poisoned(&LOOKS_FAIL).retain(|p| *p != hidden);
        assert_eq!(let_go(found), 1);
        assert_eq!(
            let_go(super::delete_jobs_cut_short(&logs)),
            1,
            "the log went"
        );
        assert_eq!(std::fs::read_to_string(&side).unwrap(), "a");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A kept log whose sidecar's rename back the disk refuses for good, as one mounted
    /// read-only, tells the hidden name once, and the log goes. Before, the log stayed, and every
    /// start told the same failure again until the user gave the sidecar back by hand.
    #[test]
    fn a_hidden_sidecar_refused_for_good_is_told_once() {
        use crate::files::file_io::{RENAME_WAYS_MISSING, UNLINK_FAILS};
        let dir = crate::support::test_support::contract_folder("start-kept-refused");
        let logs = dir.join("logs");
        let a = dir.join("a.png");
        let hidden = crate::files::file_io::tmp_path(&crate::metadata::xmp::sidecar(&a));
        leave_a_kept_log(&logs, &a, &hidden, |side, hidden| {
            std::fs::rename(side, hidden).unwrap();
        });
        // The link at the sidecar's name is made, and the unlink of the hidden one refused.
        RENAME_WAYS_MISSING.set(1);
        UNLINK_FAILS.set(Some((hidden.clone(), libc::EACCES)));
        let _ = crate::files::file_io::take_saves_failed();
        let found = super::delete_jobs_cut_short(&logs);
        UNLINK_FAILS.set(None);
        RENAME_WAYS_MISSING.set(0);
        assert_eq!(let_go(found), 1);
        let told = crate::files::file_io::take_saves_failed().join("\n");
        assert!(told.contains(&hidden.display().to_string()), "{told}");
        assert_eq!(std::fs::read_to_string(&hidden).unwrap(), "a");
        assert_eq!(
            let_go(super::delete_jobs_cut_short(&logs)),
            0,
            "the log stayed to tell it again"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A planted job log whose `old-log` record names a file outside the folder of the job logs
    /// does not get that file removed: neither a photo, also by a path through `..`, nor a file
    /// deeper in a folder of Glimt 0.92 or older. The start removes only a log of the job logs'
    /// own folder that such a record names, as a kill leaves one beside the log written from it,
    /// and an old log directly in an old folder (`an_old_log_written_anew_goes_after_a_kill`).
    #[test]
    fn an_old_log_record_removes_nothing_outside_the_log_folders() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-old-log-out-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (jobs, deletes, photo) = (dir.join("jobs"), dir.join("deletes"), dir.join("a.jpg"));
        std::fs::create_dir_all(&jobs).unwrap();
        std::fs::create_dir_all(&deletes).unwrap();
        std::fs::write(&photo, "a").unwrap();
        let old = deletes.join("sub").join("w-1");
        std::fs::create_dir_all(deletes.join("sub")).unwrap();
        std::fs::write(&old, b"/p/a.jpg\0").unwrap();
        let through_parent = deletes.join("..").join("a.jpg");
        let (written_from, _) = super::JobLog::start(&jobs, "move", &[])
            .unwrap()
            .into_path_and_file();
        for job in ["rename", "delete", "move"] {
            for named in [&photo, &through_parent, &old, &written_from] {
                let planted = [super::Record::OldLog(named.clone())];
                drop(super::JobLog::start(&jobs, job, &planted).unwrap());
            }
        }
        drop(super::jobs_cut_short(&jobs));
        drop(super::cut_short_plans(&jobs));
        assert!(photo.exists() && old.exists());
        assert!(
            !written_from.exists(),
            "the log named in jobs/ was not removed"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Glimt 0.93 and 0.94 wrote an old log anew as a job log with an `old-log` record, and then
    /// removed the old one. A kill between the two leaves both. The start puts the job log right
    /// and removes the old log with its lists, so the old folder goes and nothing is told. Before,
    /// the old log stayed, and the user was sent to Glimt 0.94, which put it right a second time.
    #[test]
    fn an_old_log_written_anew_goes_after_a_kill() {
        let state =
            std::env::temp_dir().join(format!("glimt-test-old-anew-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&state);
        let (jobs, deletes) = (state.join("jobs"), state.join("deletes"));
        std::fs::create_dir_all(&jobs).unwrap();
        std::fs::create_dir_all(deletes.join("files")).unwrap();
        std::fs::write(deletes.join("files").join(".lock"), "").unwrap();
        let old = deletes.join("w-1");
        std::fs::write(&old, b"/p/a.jpg\0").unwrap();
        std::fs::write(deletes.join("files").join("w-1"), "-\n").unwrap();
        let planted = [super::Record::OldLog(old.clone())];
        drop(super::JobLog::start(&jobs, "delete", &planted).unwrap());
        crate::files::file_io::take_saves_failed();
        drop(super::jobs_cut_short(&jobs));
        assert!(!old.exists() && !deletes.join("files").join("w-1").exists());
        super::clear_old_log_folders(&state);
        assert!(!deletes.exists(), "the old folder stayed");
        let told = crate::files::file_io::take_saves_failed();
        assert!(told.is_empty(), "{told:?}");
        std::fs::remove_dir_all(state).unwrap();
    }

    /// A folder of the logs of Glimt 0.92 or older that holds no log, only folders of lists
    /// with their lock file or nothing at all, is removed at the start. One that still holds a
    /// log, or a link where a folder of lists was, is left as it is and told once, and nothing
    /// is read or removed through the link.
    #[test]
    fn old_log_folders_are_removed_when_empty_and_told_when_not() {
        let state =
            std::env::temp_dir().join(format!("glimt-test-old-log-folders-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&state);
        let (renames, moves, deletes) = (
            state.join("renames"),
            state.join("moves"),
            state.join("deletes"),
        );
        std::fs::create_dir_all(renames.join("files")).unwrap();
        std::fs::write(renames.join("files").join(".lock"), "").unwrap();
        std::fs::create_dir_all(deletes.join("files")).unwrap();
        std::fs::create_dir_all(deletes.join("kept")).unwrap();
        std::fs::write(deletes.join("w-1"), b"/p/a.jpg\0").unwrap();
        std::fs::write(deletes.join("files").join("w-1"), "-\n").unwrap();
        let outside = state.join("outside");
        std::fs::create_dir_all(&moves).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("photo.jpg"), "a").unwrap();
        std::os::unix::fs::symlink(&outside, moves.join("files")).unwrap();
        crate::files::file_io::take_saves_failed();
        super::clear_old_log_folders(&state);
        assert!(
            !renames.exists(),
            "an old folder with only a lock file stayed"
        );
        assert!(deletes.join("w-1").exists() && deletes.join("files").join("w-1").exists());
        assert!(moves.join("files").exists() && outside.join("photo.jpg").exists());
        let told = crate::files::file_io::take_saves_failed();
        assert_eq!(told.len(), 2, "{told:?}");
        // Once the log has gone, as a start of Glimt 0.94 removes it, the folder goes too.
        std::fs::remove_file(deletes.join("w-1")).unwrap();
        std::fs::remove_file(deletes.join("files").join("w-1")).unwrap();
        std::fs::remove_file(moves.join("files")).unwrap();
        super::clear_old_log_folders(&state);
        assert!(!deletes.exists() && !moves.exists());
        assert!(crate::files::file_io::take_saves_failed().is_empty());
        std::fs::remove_dir_all(state).unwrap();
    }

    /// An Undo from the trash keeps a log of the marks, stars and places of what it puts back,
    /// with the fingerprint of each file or folder before it comes back. Glimt killed between a
    /// file's rename and its `done` leaves the log, and the next start gives the marks back to
    /// a file at its name with that fingerprint, and to the files of a folder whose step is
    /// done, and their places to the collection. Another file at the name, and a file that
    /// stayed in the trash, get none. Before, only a step with its `done` got its marks back.
    #[test]
    fn an_undo_from_the_trash_keeps_its_marks_through_a_kill() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-undo-trash-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (trash, x, jobs) = (dir.join("trash"), dir.join("x"), dir.join("jobs"));
        for d in [trash.join("files/f"), trash.join("info"), x.clone()] {
            std::fs::create_dir_all(d).unwrap();
        }
        let now = std::time::SystemTime::now();
        let secs = now.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
        let date = crate::support::format::local_time(secs).replacen(' ', "T", 1);
        std::fs::write(trash.join("files/a.jpg"), "a").unwrap();
        std::fs::write(trash.join("files/f/in.jpg"), "in").unwrap();
        for name in ["a.jpg", "f"] {
            let text = format!(
                "[Trash Info]\nPath={}\nDeletionDate={date}\n",
                x.join(name).display()
            );
            std::fs::write(trash.join("info").join(format!("{name}.trashinfo")), text).unwrap();
        }
        let collection = dir.join("c.gqv");
        let kept = x.join("kept.jpg");
        std::fs::write(&collection, format!("\"{}\"\n", kept.display())).unwrap();
        let (a, f) = (x.join("a.jpg"), x.join("f"));
        let (inside, stayed) = (x.join("f/in.jpg"), x.join("b.jpg"));
        let marks = vec![
            (a.clone(), (1, 2)),
            (inside.clone(), (4, 0)),
            (stayed.clone(), (8, 5)),
        ];
        let places = vec![(0, a.clone()), (2, inside.clone())];
        let files = vec![a.clone(), f.clone(), stayed.clone()];
        let shown = Some((collection.as_path(), places.as_slice()));
        let mut log = super::start_undo_trash_log(&jobs, &files, &marks, shown).unwrap();
        let trash_move = crate::files::trash::TrashMove {
            out_without_answer: Default::default(),
            files,
            sidecars: Vec::new(),
            sidecars_left: Vec::new(),
            since: now - std::time::Duration::from_secs(1),
            marks: Default::default(),
            collection: None,
        };
        let dirs = |_: &std::path::Path| vec![(trash.clone(), "/".into(), None)];
        // Killed between a.jpg's rename and its `done`: its `done` is not written.
        let mut note = |step: crate::files::trash::Restoring| {
            if !matches!(step, crate::files::trash::Restoring::Back(p) if p == a) {
                log.note(step);
            }
        };
        let (back, _) = crate::files::trash::restore_noting(&trash_move, dirs, &mut note);
        assert_eq!(back, vec![a.clone(), f.clone()]);
        // A log whose file's name holds another file than the one in the trash gives no marks.
        let written = |size| super::Record::Written {
            number: 0,
            fingerprint: (
                None,
                size,
                crate::files::file_io::fingerprint(&a).unwrap().2,
            ),
        };
        let records = |size| {
            vec![
                super::Record::Step {
                    number: 0,
                    from: a.clone(),
                    to: None,
                    fingerprint: None,
                },
                super::Record::Marks {
                    number: 0,
                    bits: 1,
                    stars: 0,
                },
                written(size),
            ]
        };
        let given_for = |size| match super::back_from_the_trash(&records(size)) {
            Ok(super::JobDoneBeforeAKill::BackFromTrash(given)) => given.len(),
            _ => panic!("not an Undo from the trash"),
        };
        assert_eq!((given_for(1), given_for(2)), (1, 0));
        // A file whose folder is not there at this start, as on a stick not mounted, cannot be
        // told: the log stays rather than its marks being forgotten.
        let mut unmounted = records(1);
        if let super::Record::Step { from, .. } = &mut unmounted[0] {
            *from = x.join("stick/a.jpg");
        }
        let told = super::back_from_the_trash(&unmounted);
        assert!(told.is_err());
        // Killed before the report was taken: the log is let go, not removed.
        drop(log);
        let (_, _, found) = super::jobs_cut_short(&jobs);
        let [(log, super::JobDoneBeforeAKill::BackFromTrash(given))] = &found[..] else {
            panic!("{} logs found", found.len());
        };
        assert_eq!(given, &vec![(a.clone(), (1, 2)), (inside.clone(), (4, 0))]);
        let entries = crate::model::collections::load(&collection).unwrap();
        assert_eq!(entries, vec![a, kept, inside]);
        let path = log.path().to_path_buf();
        found
            .into_iter()
            .for_each(|(log, _)| super::remove_once_written(log));
        crate::files::file_io::wait_for_queued_writes();
        crate::files::file_io::wait_for_queued_writes();
        assert!(!path.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// An Undo from the trash that puts a.jpg back and cannot tell about b.jpg keeps its log for
    /// b.jpg only. The window gave a.jpg its stars back, and the user may change them before
    /// the next start, which must not set them back. Before, the start gave both again.
    #[test]
    fn a_kept_undo_from_the_trash_gives_back_only_what_the_window_did_not() {
        use crate::files::file_io::{ANSWER_LOST_AT, LOOK_FAILS};
        let dir = std::env::temp_dir().join(format!(
            "glimt-test-undo-trash-kept-applied-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let (trash, x, jobs) = (dir.join("trash"), dir.join("x"), dir.join("jobs"));
        for d in [trash.join("files"), trash.join("info"), x.clone()] {
            std::fs::create_dir_all(d).unwrap();
        }
        let now = std::time::SystemTime::now();
        let secs = now.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
        let date = crate::support::format::local_time(secs).replacen(' ', "T", 1);
        let (a, b) = (x.join("a.jpg"), x.join("b.jpg"));
        for (name, path) in [("a.jpg", &a), ("b.jpg", &b)] {
            std::fs::write(trash.join("files").join(name), name).unwrap();
            let text = format!(
                "[Trash Info]\nPath={}\nDeletionDate={date}\n",
                path.display()
            );
            let info = trash.join("info").join(format!("{name}.trashinfo"));
            std::fs::write(info, text).unwrap();
        }
        let marks = vec![(a.clone(), (0, 3)), (b.clone(), (0, 5))];
        let files = vec![a.clone(), b.clone()];
        let mut log = super::start_undo_trash_log(&jobs, &files, &marks, None).unwrap();
        let trash_move = crate::files::trash::TrashMove {
            out_without_answer: Default::default(),
            files,
            sidecars: Vec::new(),
            sidecars_left: Vec::new(),
            since: now - std::time::Duration::from_secs(1),
            marks: Default::default(),
            collection: None,
        };
        let dirs = |_: &std::path::Path| vec![(trash.clone(), "/".into(), None)];
        ANSWER_LOST_AT.set(Some(trash.join("files/b.jpg")));
        LOOK_FAILS.set(Some((b.clone(), 0)));
        let mut note = |step: crate::files::trash::Restoring| log.note(step);
        let (back, error) = crate::files::trash::restore_noting(&trash_move, dirs, &mut note);
        ANSWER_LOST_AT.set(None);
        LOOK_FAILS.set(None);
        assert!(back == [a.clone()] && error.is_some());
        super::remove_once_written(log.into_log(&back));
        crate::files::file_io::wait_for_queued_writes();
        crate::files::file_io::wait_for_queued_writes();
        let (_, _, found) = super::jobs_cut_short(&jobs);
        let [(_, super::JobDoneBeforeAKill::BackFromTrash(given))] = &found[..] else {
            panic!("{} logs found", found.len());
        };
        assert_eq!(given, &vec![(b, (0, 5))]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A log of any kind of job that stays for the next start while the window runs is written
    /// anew without the steps that the window applied, so that a new kind gets the rule too. It
    /// still names the log that it was itself written anew from, whose removal at the start may
    /// have failed. Before, that `old-log` record was left out, and the next start read the
    /// older log as one of its own and gave its marks and places again (#279).
    #[test]
    fn a_kept_log_of_every_kind_leaves_out_the_steps_the_window_applied() {
        let dir = std::env::temp_dir().join(format!(
            "glimt-test-kept-log-applied-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let (a, b) = (dir.join("x/a.jpg"), dir.join("x/b.jpg"));
        let step = |number, from: &std::path::PathBuf| super::Record::Step {
            number,
            from: from.clone(),
            to: None,
            fingerprint: None,
        };
        for job in crate::files::job_log::JOB_KINDS {
            assert!(super::read_at_start(job).is_some(), "{job}");
            let jobs = dir.join(job);
            std::fs::create_dir_all(&jobs).unwrap();
            let records = [
                step(0, &a),
                super::Record::Done(0),
                step(1, &b),
                super::Record::Marks {
                    number: 1,
                    bits: 0,
                    stars: 5,
                },
                super::Record::OldLog(dir.join("older")),
            ];
            let mut log = super::JobLog::start(&jobs, job, &records).unwrap();
            log.note_steps_applied(std::slice::from_ref(&a));
            log.keep_for_the_next_start();
            super::remove_once_written(log);
            crate::files::file_io::wait_for_queued_writes();
            crate::files::file_io::wait_for_queued_writes();
            let logs: Vec<_> = std::fs::read_dir(&jobs).unwrap().flatten().collect();
            assert_eq!(logs.len(), 1, "{job}");
            let read = crate::files::job_log::read_job_log_without_lock(&logs[0].path());
            let Ok(crate::files::job_log::LogRead::Log { records: left, .. }) = read else {
                panic!("{job}: no log");
            };
            let marks = super::Record::Marks {
                number: 0,
                bits: 0,
                stars: 5,
            };
            assert_eq!(left[..2], [step(0, &b), marks], "{job}");
            let older = super::Record::OldLog(dir.join("older"));
            assert!(
                left.contains(&older),
                "{job}: the older log no longer named"
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A delete's log that a start writes anew for the names it could not tell about still names
    /// the logs that it was itself written anew from, before the one it takes the place of, as
    /// the start may have failed to remove them (#279).
    #[test]
    fn a_delete_log_written_anew_keeps_naming_its_older_logs() {
        let p = std::path::PathBuf::from;
        let read = super::delete_records(
            "trash",
            &[
                super::Record::Step {
                    number: 0,
                    from: p("/x/a.jpg"),
                    to: None,
                    fingerprint: None,
                },
                super::Record::OldLog(p("/jobs/older")),
            ],
        );
        let untold = [p("/x/a.jpg")].into_iter().collect();
        let none = Default::default();
        let records = super::steps_left(&read, &untold, &none, &none, &p("/jobs/old"));
        let named: Vec<&super::Record> = (records.iter())
            .filter(|r| matches!(r, super::Record::OldLog(_)))
            .collect();
        assert_eq!(
            named,
            [
                &super::Record::OldLog(p("/jobs/older")),
                &super::Record::OldLog(p("/jobs/old"))
            ]
        );
    }

    /// An Undo from the trash whose rename a share did and lost the answer of, with the look at
    /// the file's own name failing too, keeps its log, and the next start gives the file its
    /// marks back. Before, the log went with the report, and the marks were held only in the
    /// Undo's memory, which a quit lost.
    #[test]
    fn an_undo_from_the_trash_that_cannot_tell_keeps_its_log() {
        use crate::files::file_io::{ANSWER_LOST_AT, LOOK_FAILS};
        let dir = std::env::temp_dir().join(format!(
            "glimt-test-undo-trash-cannot-tell-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let (trash, x, jobs) = (dir.join("trash"), dir.join("x"), dir.join("jobs"));
        for d in [trash.join("files"), trash.join("info"), x.clone()] {
            std::fs::create_dir_all(d).unwrap();
        }
        let now = std::time::SystemTime::now();
        let secs = now.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
        let date = crate::support::format::local_time(secs).replacen(' ', "T", 1);
        let (in_trash, a) = (trash.join("files/a.jpg"), x.join("a.jpg"));
        std::fs::write(&in_trash, "a").unwrap();
        let text = format!("[Trash Info]\nPath={}\nDeletionDate={date}\n", a.display());
        std::fs::write(trash.join("info/a.jpg.trashinfo"), text).unwrap();
        let marks = vec![(a.clone(), (1, 2))];
        let files = vec![a.clone()];
        let mut log = super::start_undo_trash_log(&jobs, &files, &marks, None).unwrap();
        let trash_move = crate::files::trash::TrashMove {
            out_without_answer: Default::default(),
            files,
            sidecars: Vec::new(),
            sidecars_left: Vec::new(),
            since: now - std::time::Duration::from_secs(1),
            marks: Default::default(),
            collection: None,
        };
        let dirs = |_: &std::path::Path| vec![(trash.clone(), "/".into(), None)];
        ANSWER_LOST_AT.set(Some(in_trash.clone()));
        LOOK_FAILS.set(Some((a.clone(), 0)));
        let mut note = |step: crate::files::trash::Restoring| log.note(step);
        let (back, error) = crate::files::trash::restore_noting(&trash_move, dirs, &mut note);
        ANSWER_LOST_AT.set(None);
        LOOK_FAILS.set(None);
        assert!(back.is_empty() && error.is_some());
        super::remove_once_written(log.into_log(&back));
        crate::files::file_io::wait_for_queued_writes();
        crate::files::file_io::wait_for_queued_writes();
        let (_, _, found) = super::jobs_cut_short(&jobs);
        let [(_, super::JobDoneBeforeAKill::BackFromTrash(given))] = &found[..] else {
            panic!("{} logs found", found.len());
        };
        assert_eq!(given, &vec![(a, (1, 2))]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// An Undo again of a file whose rename out of the trash may have been done counts it as
    /// back when its own name holds a file with the fingerprint it had in the trash, and the
    /// trash's name is gone. Before, every Undo again failed on the name it had taken, and the
    /// file's marks stayed only in the Undo's memory.
    #[test]
    fn an_undo_again_takes_a_file_out_without_answer_as_back() {
        let dir = std::env::temp_dir().join(format!(
            "glimt-test-undo-trash-again-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let (trash, x) = (dir.join("trash"), dir.join("x"));
        for d in [trash.join("files"), trash.join("info"), x.clone()] {
            std::fs::create_dir_all(d).unwrap();
        }
        let now = std::time::SystemTime::now();
        let secs = now.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
        let date = crate::support::format::local_time(secs).replacen(' ', "T", 1);
        let (in_trash, a) = (trash.join("files/a.jpg"), x.join("a.jpg"));
        let info = trash.join("info/a.jpg.trashinfo");
        std::fs::write(&in_trash, "a").unwrap();
        let text = format!("[Trash Info]\nPath={}\nDeletionDate={date}\n", a.display());
        std::fs::write(&info, text).unwrap();
        let fingerprint = crate::files::file_io::fingerprint(&in_trash).unwrap();
        // The first Undo's rename was done, and its answer lost.
        std::fs::rename(&in_trash, &a).unwrap();
        let trash_move = crate::files::trash::TrashMove {
            out_without_answer: [(a.clone(), fingerprint)].into(),
            files: vec![a.clone()],
            sidecars: Vec::new(),
            sidecars_left: Vec::new(),
            since: now - std::time::Duration::from_secs(1),
            marks: Default::default(),
            collection: None,
        };
        let dirs = |_: &std::path::Path| vec![(trash.clone(), "/".into(), None)];
        let (back, error) = crate::files::trash::restore_noting(&trash_move, dirs, &mut |_| {});
        assert_eq!((back, error), (vec![a], None));
        assert!(crate::files::file_io::is_surely_gone(&info));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Rename Folder and Move Folder to Trash keep a log of the folder, with its inode. Glimt
    /// killed after the folder was renamed or trashed, before the window took the report,
    /// leaves the log, and the next start makes the marks follow the folder's new name, or
    /// forgets them and takes its files out of the collection shown. A folder still where it
    /// was is left alone. Before, these jobs kept no log, and the marks stayed under the old
    /// path, where a later folder of that name showed them.
    #[test]
    fn a_folder_renamed_or_trashed_is_followed_after_a_kill() {
        use super::JobDoneBeforeAKill::{FolderRenamed, FolderTrashed, NothingToFollow};
        let dir =
            std::env::temp_dir().join(format!("glimt-test-folder-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (old, new, jobs) = (dir.join("old"), dir.join("new"), dir.join("jobs"));
        std::fs::create_dir_all(&old).unwrap();
        // A file system that keeps a folder's inode, so that the log notes it.
        crate::files::file_ops::test_mount(&dir, Some("ext4"));
        let found_after_a_kill = |log: crate::files::job_log::JobLog| {
            drop(log);
            let (_, _, mut found) = super::jobs_cut_short(&jobs);
            assert_eq!(found.len(), 1);
            let (log, done) = found.remove(0);
            super::remove_once_written(log);
            crate::files::file_io::wait_for_queued_writes();
            crate::files::file_io::wait_for_queued_writes();
            done
        };
        // Killed before the rename: nothing follows.
        let log = super::start_folder_log(&jobs, &old, Some(&new), None).unwrap();
        assert!(matches!(found_after_a_kill(log), NothingToFollow));
        // Killed after it.
        let log = super::start_folder_log(&jobs, &old, Some(&new), None).unwrap();
        std::fs::rename(&old, &new).unwrap();
        let done = found_after_a_kill(log);
        assert!(matches!(&done, FolderRenamed(from, to) if *from == old && *to == new));
        // A trashed folder, with another folder made at its path since.
        let collection = dir.join("c.gqv");
        let (inside, kept) = (new.join("a.jpg"), dir.join("kept.jpg"));
        let text = format!("\"{}\"\n\"{}\"\n", inside.display(), kept.display());
        std::fs::write(&collection, text).unwrap();
        let log = super::start_folder_log(&jobs, &new, None, Some(&collection)).unwrap();
        std::fs::rename(&new, dir.join("in-the-trash")).unwrap();
        std::fs::create_dir(&new).unwrap();
        let done = found_after_a_kill(log);
        assert!(matches!(&done, FolderTrashed(folder) if *folder == new));
        let entries = crate::model::collections::load(&collection).unwrap();
        assert_eq!(entries, vec![kept]);
        assert_eq!(std::fs::read_dir(&jobs).unwrap().count(), 0);
        // A trashed folder whose parent is not there at this start, as on a stick not
        // mounted: the start cannot tell, and leaves the log for a later one.
        let unmounted = dir.join("stick/photos");
        std::fs::create_dir_all(&unmounted).unwrap();
        let log = super::start_folder_log(&jobs, &unmounted, None, None).unwrap();
        std::fs::remove_dir_all(dir.join("stick")).unwrap();
        std::fs::create_dir(dir.join("stick")).unwrap();
        super::ON_ANOTHER_DEVICE.with_borrow_mut(|folders| folders.push(dir.join("stick")));
        drop(log);
        let (_, _, found) = super::jobs_cut_short(&jobs);
        assert!(found.is_empty());
        assert_eq!(std::fs::read_dir(&jobs).unwrap().count(), 1);
        // A folder renamed onto a stick that is not mounted at this start: its old path is
        // free, and its new one cannot be told, so the log stays too.
        let (moved, on_stick) = (dir.join("moved"), dir.join("stick2/moved"));
        std::fs::create_dir_all(&moved).unwrap();
        let log = super::start_folder_log(&jobs, &moved, Some(&on_stick), None).unwrap();
        std::fs::remove_dir(&moved).unwrap();
        drop(log);
        let (_, _, found) = super::jobs_cut_short(&jobs);
        assert!(found.is_empty());
        assert_eq!(std::fs::read_dir(&jobs).unwrap().count(), 2);
        super::ON_ANOTHER_DEVICE.with_borrow_mut(Vec::clear);
        crate::files::file_ops::test_mount(&dir, None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A start with an empty mount point at the folder of a killed job, as `/mnt/nas` is while
    /// its share is not mounted, cannot tell what the job did there, and keeps the log until the
    /// share is back. Before, the empty folder counted as the one the job wrote to: a folder
    /// trashed from it counted as trashed while it was not seen, and a Move into it as one whose
    /// file never arrived, so that the marks of their files were lost.
    #[test]
    fn a_killed_job_waits_while_its_folder_is_an_empty_mount_point() {
        use super::JobDoneBeforeAKill::FolderTrashed;
        let dir = crate::support::test_support::contract_folder("start-empty-mount-point");
        let (nas, jobs, away) = (dir.join("nas"), dir.join("jobs"), dir.join("away"));
        let (album, a, moved) = (nas.join("Album"), dir.join("a.jpg"), nas.join("a.jpg"));
        std::fs::create_dir_all(&album).unwrap();
        std::fs::create_dir(&away).unwrap();
        std::fs::write(&a, "a").unwrap();
        // Move Folder to Trash of nas/Album, and a Move of a.jpg into nas, both done and killed.
        drop(super::start_folder_log(&jobs, &album, None, None).unwrap());
        std::fs::rename(&album, dir.join("in-the-trash")).unwrap();
        let mut log = super::start_move_log(&jobs, "move").unwrap();
        log.note(&a, &moved);
        drop(log);
        std::fs::rename(&a, &moved).unwrap();
        // The share is not mounted: nas is an empty folder on another device.
        std::fs::rename(&moved, away.join("a.jpg")).unwrap();
        super::ON_ANOTHER_DEVICE.with_borrow_mut(|folders| folders.push(nas.clone()));
        let (moves, _, others) = super::jobs_cut_short(&jobs);
        assert!(moves.is_empty(), "the Move was put right without its file");
        assert!(others.is_empty(), "the folder counted as trashed");
        drop(moves);
        assert_eq!(std::fs::read_dir(&jobs).unwrap().count(), 2);
        // The share is back.
        super::ON_ANOTHER_DEVICE.with_borrow_mut(Vec::clear);
        std::fs::rename(away.join("a.jpg"), &moved).unwrap();
        let (moves, _, others) = super::jobs_cut_short(&jobs);
        assert_eq!(moves[0].1, vec![(a.clone(), moved.clone())]);
        assert!(matches!(&others[0].1, FolderTrashed(folder) if *folder == album));
        drop((moves, others));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A start that cannot give the files of a killed Undo from the trash their places back in
    /// the collection shown, which cannot be written, keeps the log for a later start. Before,
    /// the failure was told but the log went, and the places were lost.
    #[test]
    fn an_undo_from_the_trash_whose_collection_cannot_be_written_keeps_its_log() {
        let dir = crate::support::test_support::contract_folder("start-undo-collection");
        // A folder at the collection's name, which no edit can write.
        let collection = dir.join("c.gqv");
        std::fs::create_dir(&collection).unwrap();
        let records = vec![
            super::Record::Shown(collection),
            super::Record::Step {
                number: 0,
                from: dir.join("a.png"),
                to: None,
                fingerprint: None,
            },
            super::Record::Place {
                number: 0,
                index: 0,
            },
            super::Record::Done(0),
        ];
        assert!(super::back_from_the_trash(&records).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A start that cannot look at the old name of a killed Move's file, and cannot tell its
    /// new one either, as on a share that stopped answering and a stick not mounted, keeps the
    /// log. Before, the failed look at the old name counted as "not gone", the pair was left
    /// out, and the log went with the marks of a file that may have moved.
    #[test]
    fn a_killed_move_whose_old_name_cannot_be_looked_at_keeps_its_log() {
        use crate::files::file_io::LOOKS_FAIL;
        use crate::support::threads::lock_even_if_poisoned;
        let dir = crate::support::test_support::contract_folder("start-old-name-unread");
        let (nas, jobs, away) = (dir.join("nas"), dir.join("jobs"), dir.join("away"));
        let (a, moved) = (dir.join("a.jpg"), nas.join("a.jpg"));
        std::fs::create_dir_all(&nas).unwrap();
        std::fs::create_dir(&away).unwrap();
        std::fs::write(&a, "a").unwrap();
        let mut log = super::start_move_log(&jobs, "move").unwrap();
        log.note(&a, &moved);
        drop(log);
        std::fs::rename(&a, &moved).unwrap();
        std::fs::rename(&moved, away.join("a.jpg")).unwrap();
        super::ON_ANOTHER_DEVICE.with_borrow_mut(|folders| folders.push(nas.clone()));
        lock_even_if_poisoned(&LOOKS_FAIL).push(a.clone());
        let (moves, _, _) = super::jobs_cut_short(&jobs);
        lock_even_if_poisoned(&LOOKS_FAIL).retain(|p| *p != a);
        super::ON_ANOTHER_DEVICE.with_borrow_mut(Vec::clear);
        assert!(
            moves.is_empty(),
            "the Move was put right without a look at its names"
        );
        drop(moves);
        assert_eq!(std::fs::read_dir(&jobs).unwrap().count(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A start that cannot take a killed delete's files out of the collection shown, which
    /// cannot be written, keeps them in the log for a later start. Before, the failure was told
    /// but the log went, and the collection kept the entries of files that are gone.
    #[test]
    fn a_delete_whose_collection_cannot_be_written_keeps_its_files_in_the_log() {
        let dir = crate::support::test_support::contract_folder("start-delete-collection");
        let (jobs, a, collection) = (dir.join("jobs"), dir.join("a.png"), dir.join("c.gqv"));
        // A folder at the collection's name, which no edit can write.
        std::fs::create_dir(&collection).unwrap();
        std::fs::write(&a, "a").unwrap();
        let mut log = super::start_delete_log(&jobs, "delete", Some(&collection)).unwrap();
        log.note(std::slice::from_ref(&a));
        std::fs::remove_file(&a).unwrap();
        log.note_done(std::slice::from_ref(&a));
        drop(log);
        let (_, deletes, _) = super::jobs_cut_short(&jobs);
        assert_eq!(deletes[0].1, vec![a.clone()]);
        deletes
            .into_iter()
            .for_each(|(log, _)| log.remove_once_forgotten());
        crate::files::file_io::wait_for_queued_writes();
        crate::files::file_io::wait_for_queued_writes();
        let _ = crate::files::file_io::take_saves_failed();
        let (_, deletes, _) = super::jobs_cut_short(&jobs);
        assert_eq!(
            deletes.len(),
            1,
            "the log went with the collection not written"
        );
        assert_eq!(deletes[0].1, vec![a.clone()]);
        drop(deletes);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A start that cannot take a folder that a killed Move Folder to Trash trashed out of the
    /// collection shown, which cannot be written, keeps the log for a later start. Before, the
    /// failure was told but the log went.
    #[test]
    fn a_folder_trashed_whose_collection_cannot_be_written_keeps_its_log() {
        let dir = crate::support::test_support::contract_folder("start-folder-collection");
        let (jobs, album, collection) = (dir.join("jobs"), dir.join("Album"), dir.join("c.gqv"));
        // A folder at the collection's name, which no edit can write.
        std::fs::create_dir(&collection).unwrap();
        std::fs::create_dir(&album).unwrap();
        drop(super::start_folder_log(&jobs, &album, None, Some(&collection)).unwrap());
        std::fs::remove_dir(&album).unwrap();
        let (_, _, others) = super::jobs_cut_short(&jobs);
        let _ = crate::files::file_io::take_saves_failed();
        assert!(
            others.is_empty(),
            "the log went with the collection not written"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A Rename Many cut short on a stick that is not mounted at the next start, where its
    /// mount point is an empty folder, keeps its plan when the user asks to put it right. Before,
    /// each name there was not found, counted as nothing of the plan's to rename, and the plan
    /// went, so that the files under hidden names on the stick had no plan to put them right.
    #[test]
    fn a_rename_cut_short_on_an_empty_mount_point_keeps_its_plan() {
        let dir = crate::support::test_support::contract_folder("start-rename-mount-point");
        let (stick, jobs, away) = (dir.join("stick"), dir.join("jobs"), dir.join("away"));
        let (a, b) = (stick.join("a.jpg"), stick.join("b.jpg"));
        std::fs::create_dir_all(&stick).unwrap();
        std::fs::create_dir(&away).unwrap();
        std::fs::write(&a, "a").unwrap();
        let moves = vec![(a.clone(), b.clone())];
        let journal = super::start_journal(&jobs, &moves).unwrap();
        let plan = journal.path().to_path_buf();
        std::fs::rename(&a, &b).unwrap();
        journal.keep_on_disk();
        // The stick is not mounted: its mount point is an empty folder on another device.
        std::fs::rename(&b, away.join("b.jpg")).unwrap();
        super::ON_ANOTHER_DEVICE.with_borrow_mut(|folders| folders.push(stick.clone()));
        let found = super::cut_short_plans(&jobs);
        let repaired = super::repair_cut_short(&plan, &found[0].1, found[0].2, false);
        super::ON_ANOTHER_DEVICE.with_borrow_mut(Vec::clear);
        assert_eq!(repaired, Some((0, 1)));
        assert!(
            plan.exists(),
            "the plan went while its names could not be told"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A log found at the start that no reader read surely has no proof that it may go
    /// (`Sure`), so the removal after the writes leaves it on disk for a later start.
    #[test]
    fn a_found_log_not_read_surely_stays() {
        let dir = crate::support::test_support::contract_folder("start-not-read-surely");
        let log = super::JobLog::start(&dir.join("jobs"), "move", &[]).unwrap();
        let (path, file) = log.into_path_and_file();
        super::remove_once_written(super::JobLog::found(path.clone(), file));
        crate::files::file_io::wait_for_queued_writes();
        crate::files::file_io::wait_for_queued_writes();
        assert!(path.exists(), "a log not read surely was removed");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Makes a folder of its own for a test about a FAT card, with `card` in it as a card mounted
    /// there, and returns the folder, the card and the folder of the logs.
    fn fat_card(name: &str) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
        let temporary = format!("glimt-test-fat-card-{name}-{}", std::process::id());
        let dir = std::env::temp_dir().join(temporary);
        let _ = std::fs::remove_dir_all(&dir);
        let (card, logs) = (dir.join("card"), dir.join("logs"));
        std::fs::create_dir_all(&card).unwrap();
        crate::files::file_ops::test_mount(&card, Some("vfat"));
        (dir, card, logs)
    }

    /// Gives the card at `card` the volume `volume` in this test, or none.
    fn card_volume(card: &std::path::Path, volume: Option<&str>) {
        super::TEST_VOLUMES.with_borrow_mut(|volumes| {
            volumes.clear();
            volumes.extend(volume.map(|v| (card.to_path_buf(), v.into())));
        })
    }

    /// Takes the test's card and its folder away.
    fn remove_fat_card(dir: &std::path::Path, card: &std::path::Path) {
        card_volume(card, None);
        crate::files::file_ops::test_mount(card, None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A killed delete on a FAT card, whose folder has no lasting inode, is told from another
    /// card mounted at the same path by the card's volume. The start forgets the marks of the
    /// file that went only while the card the log noted is there, and keeps the log while
    /// another card is, or one whose volume cannot be found. Before, the device alone was
    /// compared, which two cards in one reader share, so the marks of another card's file at the
    /// same path were forgotten (#266).
    #[test]
    fn a_killed_delete_on_a_fat_card_is_told_from_another_card_by_its_volume() {
        let (dir, card, logs) = fat_card("start-fat-card");
        let (a, b) = (card.join("a.png"), card.join("b.png"));
        std::fs::write(&b, "b").unwrap();
        card_volume(&card, Some("1111-AAAA"));
        let mut log = super::start_delete_log(&logs, "trash", None).unwrap();
        log.note(&[a.clone(), b.clone()]);
        drop(log);
        for volume in [Some("2222-BBBB"), None] {
            card_volume(&card, volume);
            let found = super::delete_jobs_cut_short(&logs);
            assert!(found[0].1.is_empty(), "{volume:?}: {:?}", found[0].1);
            assert_eq!(let_go(found), 1);
        }
        card_volume(&card, Some("1111-AAAA"));
        let found = super::delete_jobs_cut_short(&logs);
        assert_eq!(found[0].1, std::slice::from_ref(&a));
        assert_eq!(let_go(found), 1);
        assert!(super::delete_jobs_cut_short(&logs).is_empty());
        remove_fat_card(&dir, &card);
    }

    /// A start that reads a killed delete of many files on one card reads the volumes once,
    /// not once or twice for each file, whether it finds the card's volume, finds none or cannot
    /// read them. Before, /dev/disk/by-uuid was read for each name, about a million times for a
    /// log of 500,000 files, and later still for each name where no volume was found.
    #[test]
    fn a_start_looks_up_a_folders_volume_once() {
        let (dir, card, logs) = fat_card("start-volume-once");
        let files: Vec<_> = (0..200).map(|i| card.join(format!("{i}.png"))).collect();
        card_volume(&card, Some("1111-AAAA"));
        let mut log = super::start_delete_log(&logs, "trash", None).unwrap();
        log.note(&files);
        drop(log);
        for (volume, unreadable) in [(None, false), (Some("1111-AAAA"), true)] {
            card_volume(&card, volume);
            super::VOLUMES_UNREADABLE.set(unreadable);
            super::VOLUME_LOOKS.set(0);
            let found = super::delete_jobs_cut_short(&logs);
            assert!(found[0].1.is_empty(), "{volume:?} {unreadable}");
            assert_eq!(super::VOLUME_LOOKS.get(), 1, "{volume:?} {unreadable}");
            assert_eq!(let_go(found), 1);
        }
        super::VOLUMES_UNREADABLE.set(false);
        card_volume(&card, Some("1111-AAAA"));
        super::VOLUME_LOOKS.set(0);
        let found = super::delete_jobs_cut_short(&logs);
        assert_eq!(found[0].1.len(), files.len());
        assert_eq!(super::VOLUME_LOOKS.get(), 1);
        assert_eq!(let_go(found), 1);
        remove_fat_card(&dir, &card);
    }

    /// A killed delete on a FAT card plugged in again under another device number, with the
    /// same volume, finishes its log, while another card under the old device number still keeps
    /// it. Before, the device was compared beside the volume, so such a log stayed until the card
    /// got its old device number again.
    #[test]
    fn a_fat_card_back_under_another_device_number_finishes_its_log() {
        let (dir, card, logs) = fat_card("start-fat-card-device");
        let (a, b) = (card.join("a.png"), card.join("b.png"));
        std::fs::write(&b, "b").unwrap();
        card_volume(&card, Some("1111-AAAA"));
        let mut log = super::start_delete_log(&logs, "trash", None).unwrap();
        log.note(&[a.clone(), b.clone()]);
        drop(log);
        card_volume(&card, Some("2222-BBBB"));
        let found = super::delete_jobs_cut_short(&logs);
        assert!(found[0].1.is_empty(), "{:?}", found[0].1);
        assert_eq!(let_go(found), 1);
        card_volume(&card, Some("1111-AAAA"));
        super::ON_ANOTHER_DEVICE.with_borrow_mut(|folders| folders.push(card.clone()));
        let found = super::delete_jobs_cut_short(&logs);
        super::ON_ANOTHER_DEVICE.with_borrow_mut(Vec::clear);
        assert_eq!(found[0].1, std::slice::from_ref(&a));
        assert_eq!(let_go(found), 1);
        assert!(super::delete_jobs_cut_short(&logs).is_empty());
        remove_fat_card(&dir, &card);
    }

    /// A killed delete of a folder's files on a FAT card, whose folder is gone at the next start
    /// while another card is at the same path in the same reader, cannot tell: the folder may
    /// only be missing from the other card. Before, the device alone was asked, and the files
    /// counted as gone (#266).
    #[test]
    fn a_folder_gone_from_a_fat_card_is_told_from_another_card_by_its_volume() {
        let (dir, card, logs) = fat_card("start-fat-card-gone");
        let sub = card.join("sub");
        std::fs::create_dir(&sub).unwrap();
        let a = sub.join("a.png");
        card_volume(&card, Some("1111-AAAA"));
        let mut log = super::start_delete_log(&logs, "trash", None).unwrap();
        log.note(std::slice::from_ref(&a));
        drop(log);
        std::fs::remove_dir(&sub).unwrap();
        card_volume(&card, Some("2222-BBBB"));
        let found = super::delete_jobs_cut_short(&logs);
        assert!(found[0].1.is_empty(), "{:?}", found[0].1);
        assert_eq!(let_go(found), 1);
        card_volume(&card, Some("1111-AAAA"));
        let found = super::delete_jobs_cut_short(&logs);
        assert_eq!(found[0].1, std::slice::from_ref(&a));
        assert_eq!(let_go(found), 1);
        remove_fat_card(&dir, &card);
    }

    /// A Move killed after a photo left a FAT card, read while another card with a photo of the
    /// same name is at the same path, follows the photo by its new name. Before, the photo at
    /// the old name counted as the one that stayed, and its marks never followed the photo that
    /// went (#266).
    #[test]
    fn a_killed_move_from_a_fat_card_is_told_from_another_card_by_its_volume() {
        let (dir, card, logs) = fat_card("start-fat-card-move");
        let to = dir.join("to");
        std::fs::create_dir(&to).unwrap();
        let (from, moved) = (card.join("a.jpg"), to.join("a.jpg"));
        std::fs::write(&from, "the card's").unwrap();
        card_volume(&card, Some("1111-AAAA"));
        let mut log = super::start_move_log(&logs, "move").unwrap();
        log.note(&from, &moved);
        std::fs::rename(&from, &moved).unwrap();
        drop(log);
        card_volume(&card, Some("2222-BBBB"));
        std::fs::write(&from, "another card's").unwrap();
        let found = super::move_jobs_cut_short(&logs);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].1, [(from.clone(), moved.clone())]);
        drop(found);
        remove_fat_card(&dir, &card);
    }

    /// A killed Move or delete on a FAT card, read while a card whose volume cannot be found is
    /// at the same path with a photo of the same name, keeps its log, and the start reads it
    /// again once the card it noted is back. Before, a volume not found counted as the same
    /// card, so the other card's photo counted as the one that stayed, and the log went with
    /// the marks of the photo that left (#266).
    #[test]
    fn a_fat_card_whose_volume_cannot_be_found_keeps_the_log() {
        let (dir, card, logs) = fat_card("start-fat-card-no-volume");
        let to = dir.join("to");
        std::fs::create_dir(&to).unwrap();
        let (from, moved, deleted) = (card.join("a.jpg"), to.join("a.jpg"), card.join("b.jpg"));
        std::fs::write(&from, "the card's").unwrap();
        card_volume(&card, Some("1111-AAAA"));
        let mut log = super::start_move_log(&logs, "move").unwrap();
        log.note(&from, &moved);
        std::fs::rename(&from, &moved).unwrap();
        drop(log);
        let mut log = super::start_delete_log(&logs, "trash", None).unwrap();
        log.note(std::slice::from_ref(&deleted));
        drop(log);
        card_volume(&card, None);
        std::fs::write(&from, "another card's").unwrap();
        std::fs::write(&deleted, "another card's").unwrap();
        assert!(super::move_jobs_cut_short(&logs).is_empty());
        assert_eq!(let_go(super::delete_jobs_cut_short(&logs)), 1);
        std::fs::remove_file(&from).unwrap();
        std::fs::remove_file(&deleted).unwrap();
        card_volume(&card, Some("1111-AAAA"));
        let found = super::move_jobs_cut_short(&logs);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].1, [(from.clone(), moved.clone())]);
        drop(found);
        let found = super::delete_jobs_cut_short(&logs);
        assert_eq!(found[0].1, std::slice::from_ref(&deleted));
        assert_eq!(let_go(found), 1);
        remove_fat_card(&dir, &card);
    }

    /// A Move Folder to Trash killed on a FAT card, read while another card with a folder of the
    /// same name is at the same path, keeps its log, and follows the folder once its own card is
    /// back. Before, any folder at the path counted as the one that stayed, and the log went
    /// (#266).
    #[test]
    fn a_folder_trashed_on_a_fat_card_is_told_from_another_card_by_its_volume() {
        use super::JobDoneBeforeAKill::FolderTrashed;
        let (dir, card, logs) = fat_card("start-fat-card-folder");
        let album = card.join("album");
        std::fs::create_dir(&album).unwrap();
        card_volume(&card, Some("1111-AAAA"));
        drop(super::start_folder_log(&logs, &album, None, None).unwrap());
        std::fs::remove_dir(&album).unwrap();
        card_volume(&card, Some("2222-BBBB"));
        std::fs::create_dir(&album).unwrap();
        let (_, _, found) = super::jobs_cut_short(&logs);
        assert!(found.is_empty());
        assert_eq!(std::fs::read_dir(&logs).unwrap().count(), 1);
        std::fs::remove_dir(&album).unwrap();
        card_volume(&card, Some("1111-AAAA"));
        let (_, _, mut found) = super::jobs_cut_short(&logs);
        assert_eq!(found.len(), 1);
        let (log, done) = found.remove(0);
        assert!(matches!(&done, FolderTrashed(folder) if *folder == album));
        super::remove_once_written(log);
        crate::files::file_io::wait_for_queued_writes();
        crate::files::file_io::wait_for_queued_writes();
        remove_fat_card(&dir, &card);
    }

    /// A killed delete on an ext4 stick, plugged in again under another device number with the
    /// same volume, finishes its log. Before, only FAT and exFAT noted a volume, so the device
    /// was compared, and the log stayed until the stick got its old device number again.
    #[test]
    fn an_ext4_stick_back_under_another_device_number_finishes_its_log() {
        let (dir, card, logs) = fat_card("start-ext4-device");
        crate::files::file_ops::test_mount(&card, Some("ext4"));
        let a = card.join("a.png");
        card_volume(&card, Some("aaaa-1111"));
        let mut log = super::start_delete_log(&logs, "trash", None).unwrap();
        log.note(std::slice::from_ref(&a));
        drop(log);
        super::ON_ANOTHER_DEVICE.with_borrow_mut(|folders| folders.push(card.clone()));
        let found = super::delete_jobs_cut_short(&logs);
        super::ON_ANOTHER_DEVICE.with_borrow_mut(Vec::clear);
        assert_eq!(found[0].1, std::slice::from_ref(&a));
        assert_eq!(let_go(found), 1);
        remove_fat_card(&dir, &card);
    }

    /// A Move killed after a photo left a folder on an ext4 stick, whose folder was deleted and
    /// made again with a photo of the same name before the stick came back under another device
    /// number, follows the photo by its new name: the volume is the noted one, but the folder's
    /// inode is not. The volume alone would take the new folder for the noted one, and its
    /// later photo for the one that stayed.
    #[test]
    fn a_folder_made_again_on_the_noted_volume_is_another_folder() {
        let (dir, card, logs) = fat_card("start-ext4-made-again");
        crate::files::file_ops::test_mount(&card, Some("ext4"));
        let (sub, to) = (card.join("sub"), dir.join("to"));
        std::fs::create_dir(&sub).unwrap();
        std::fs::create_dir(&to).unwrap();
        let (from, moved) = (sub.join("a.jpg"), to.join("a.jpg"));
        std::fs::write(&from, "the stick's").unwrap();
        card_volume(&card, Some("aaaa-1111"));
        let mut log = super::start_move_log(&logs, "move").unwrap();
        log.note(&from, &moved);
        std::fs::rename(&from, &moved).unwrap();
        drop(log);
        // The old folder is kept under another name, so that the new one cannot get its inode.
        std::fs::rename(&sub, card.join("old-sub")).unwrap();
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(&from, "a later photo").unwrap();
        super::ON_ANOTHER_DEVICE.with_borrow_mut(|folders| folders.push(sub.clone()));
        let found = super::move_jobs_cut_short(&logs);
        super::ON_ANOTHER_DEVICE.with_borrow_mut(Vec::clear);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].1, [(from.clone(), moved.clone())]);
        drop(found);
        remove_fat_card(&dir, &card);
    }

    /// A killed delete on an ext4 stick, read while another stick with another volume is at the
    /// same path under the old device number, keeps the marks and the log. Before, the device
    /// was compared alone, and the other stick's missing file counted as deleted.
    #[test]
    fn another_volume_under_the_old_device_number_is_another_disk() {
        let (dir, card, logs) = fat_card("start-ext4-other");
        crate::files::file_ops::test_mount(&card, Some("ext4"));
        let a = card.join("a.png");
        card_volume(&card, Some("aaaa-1111"));
        let mut log = super::start_delete_log(&logs, "trash", None).unwrap();
        log.note(std::slice::from_ref(&a));
        drop(log);
        card_volume(&card, Some("bbbb-2222"));
        let found = super::delete_jobs_cut_short(&logs);
        assert!(found[0].1.is_empty(), "{:?}", found[0].1);
        assert_eq!(let_go(found), 1);
        remove_fat_card(&dir, &card);
    }

    /// A killed delete on a card in a reader, or a stick, whose volume could not be noted, keeps
    /// the marks and the log while the folder's device number is the only thing that tells it,
    /// whether the file's folder is there or gone: another card in the same reader, or another
    /// stick at the same port, gets that number too. Off a block device, as on a share, the
    /// number is the share's own, and the start finishes the log. Before, the number alone was
    /// compared, and the marks of the files missing from another card were forgotten (#179,
    /// #266, #270).
    #[test]
    fn a_block_device_number_alone_tells_no_card_from_another() {
        let (dir, card, logs) = fat_card("start-block-device");
        let (sub, b) = (card.join("sub"), card.join("b.png"));
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(&b, "b").unwrap();
        let (a, c) = (card.join("a.png"), sub.join("c.png"));
        let mut log = super::start_delete_log(&logs, "trash", None).unwrap();
        log.note(&[a.clone(), b.clone(), c.clone()]);
        drop(log);
        std::fs::remove_dir(&sub).unwrap();
        super::ON_A_BLOCK_DEVICE.with_borrow_mut(|on| on.push(card.clone()));
        let found = super::delete_jobs_cut_short(&logs);
        super::ON_A_BLOCK_DEVICE.with_borrow_mut(Vec::clear);
        assert!(found[0].1.is_empty(), "{:?}", found[0].1);
        assert_eq!(let_go(found), 1);
        let found = super::delete_jobs_cut_short(&logs);
        assert_eq!(found[0].1, [a, c]);
        assert_eq!(let_go(found), 1);
        remove_fat_card(&dir, &card);
    }

    /// A Move, a delete and a Move Folder to Trash killed on a FAT card, where no volume can be
    /// read, as in the Flatpak's sandbox, finish at the next start while the card is on the
    /// device noted: the Move follows its photo that went and leaves the one still on the card,
    /// the delete forgets the marks of its files, also of one in a folder gone since, and the
    /// folder counts as trashed. Before, the card's folders, which keep no lasting inode, could
    /// never be told on a block device, and each log stayed for ever, with the marks at the
    /// card's old names.
    #[test]
    fn a_job_killed_on_a_fat_card_finishes_where_no_volume_can_be_read() {
        use super::JobDoneBeforeAKill::FolderTrashed;
        let (dir, card, logs) = fat_card("start-fat-card-no-volumes");
        let (sub, album, to) = (card.join("sub"), card.join("album"), dir.join("to"));
        for folder in [&sub, &album, &to] {
            std::fs::create_dir(folder).unwrap();
        }
        let (from, moved) = (card.join("a.jpg"), to.join("a.jpg"));
        let (stayed, not_moved) = (card.join("d.jpg"), to.join("d.jpg"));
        std::fs::write(&from, "the card's").unwrap();
        std::fs::write(&stayed, "not moved yet").unwrap();
        let mut log = super::start_move_log(&logs, "move").unwrap();
        log.note(&from, &moved);
        std::fs::rename(&from, &moved).unwrap();
        // The kill came before this step's file went, so the start must tell its folder.
        log.note(&stayed, &not_moved);
        drop(log);
        let (b, c) = (card.join("b.png"), sub.join("c.png"));
        let mut log = super::start_delete_log(&logs, "trash", None).unwrap();
        log.note(&[b.clone(), c.clone()]);
        drop(log);
        drop(super::start_folder_log(&logs, &album, None, None).unwrap());
        std::fs::remove_dir(&sub).unwrap();
        std::fs::remove_dir(&album).unwrap();
        super::ON_A_BLOCK_DEVICE.with_borrow_mut(|on| on.push(card.clone()));
        super::VOLUMES_UNREADABLE.set(true);
        let (moves, deletes, mut others) = super::jobs_cut_short(&logs);
        super::VOLUMES_UNREADABLE.set(false);
        super::ON_A_BLOCK_DEVICE.with_borrow_mut(Vec::clear);
        assert_eq!(moves.len(), 1);
        let (log, went) = moves.into_iter().next().unwrap();
        assert_eq!(went, [(from, moved)]);
        log.remove_once_followed();
        assert_eq!(deletes[0].1, [b, c]);
        assert_eq!(let_go(deletes), 1);
        assert_eq!(others.len(), 1);
        let (log, done) = others.remove(0);
        assert!(matches!(&done, FolderTrashed(folder) if *folder == album));
        super::remove_once_written(log);
        crate::files::file_io::wait_for_queued_writes();
        crate::files::file_io::wait_for_queued_writes();
        remove_fat_card(&dir, &card);
    }

    /// A start that keeps logs because it cannot tell what their jobs had done says so once,
    /// with the folder of the logs, and a start that finishes them says nothing. Before, such a
    /// log stayed unseen, for ever where the start could never tell.
    #[test]
    fn logs_kept_for_a_later_start_are_told_once() {
        let (dir, card, logs) = fat_card("start-kept-told");
        let a = card.join("a.png");
        for _ in 0..2 {
            let mut log = super::start_delete_log(&logs, "trash", None).unwrap();
            log.note(std::slice::from_ref(&a));
            drop(log);
        }
        crate::files::file_io::take_saves_failed();
        super::ON_A_BLOCK_DEVICE.with_borrow_mut(|on| on.push(card.clone()));
        let found = super::delete_jobs_cut_short(&logs);
        super::ON_A_BLOCK_DEVICE.with_borrow_mut(Vec::clear);
        assert_eq!(let_go(found), 2);
        let told = crate::files::file_io::take_saves_failed();
        assert_eq!(told.len(), 1, "{told:?}");
        assert!(told[0].contains("could not finish"), "{told:?}");
        let found = super::delete_jobs_cut_short(&logs);
        assert_eq!(let_go(found), 2);
        let told = crate::files::file_io::take_saves_failed();
        assert!(told.is_empty(), "{told:?}");
        remove_fat_card(&dir, &card);
    }

    /// A start that reads a log of a card with a volume noted looks up the card's volume afresh
    /// before it takes a folder for the one noted, so that a card swapped during the start, with
    /// the same device number, is not taken for it. Before, the volumes were read once for the
    /// log, and the swapped card passed for the one they named (#284).
    #[test]
    fn a_card_swapped_during_a_start_is_not_taken_for_the_one_noted() {
        use std::os::unix::fs::MetadataExt;
        let (dir, card, _) = fat_card("start-card-swapped");
        let device = std::fs::metadata(&card).unwrap().dev();
        let records = [
            super::Record::Folder {
                path: card.clone(),
                device,
                inode: 0,
            },
            super::Record::Volume {
                path: card.clone(),
                volume: "1111-AAAA".into(),
            },
        ];
        let folders = super::folders_noted(&records);
        card_volume(&card, Some("1111-AAAA"));
        let noted = &folders[&card];
        assert_eq!(
            super::is_the_folder_noted(&card, noted, device, None),
            Ok(true)
        );
        card_volume(&card, Some("2222-BBBB"));
        let swapped = super::is_the_folder_noted(&card, noted, device, None);
        assert_eq!(swapped, Err(super::CannotTell));
        remove_fat_card(&dir, &card);
    }
}
