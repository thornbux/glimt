//! The job log: one file in `$XDG_STATE_HOME/glimt/jobs/` for each job that changes files, which
//! the job writes before and after its steps, so that the next start can put right a job that a
//! kill cut short (docs/design/log-format.md). Every field ends in a NUL byte, as a path can hold
//! any other byte, and an empty field ends a record. The first record is the header, with the
//! format's version, so that a later Glimt can add kinds of record, and an older one leaves a
//! log of a higher version alone.

use crate::files::file_io::{FileFingerprint, TMP_PREFIX};
use crate::support::format::escaped_name;
use std::fs::File;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

/// The first field of every log. A file that does not start with it is no log.
pub(crate) const HEADER: &str = "glimt-job";

/// The version of the format this Glimt writes and reads. A record that changes the meaning of
/// others raises it, and a reader leaves a log of a higher version alone.
pub(crate) const VERSION: u32 = 1;

/// The kinds of job a log is written for. They are pinned in src/frozen-1.0.txt.
pub(crate) const JOB_KINDS: [&str; 7] = [
    "rename",
    "move",
    "delete",
    "trash",
    "undo-trash",
    "rename-folder",
    "trash-folder",
];

/// The kinds of job that Glimt 0.99.0 wrote under a word this Glimt no longer writes, each with
/// the kind it is read as now, so that a log that a kill left under 0.99.0 is put right once. An
/// Undo of a Move wrote "undo-move", which a start read as a Move.
const OLD_JOB_KINDS: [(&str, &str); 1] = [("undo-move", "move")];

/// The kinds of record after the header, as `Record::kind` names them. They are pinned in
/// src/frozen-1.0.txt. "old-log" names the job log that this one was written from
/// (`Record::OldLog`), a log of this format, not one of the old logs of Glimt 0.92 and older.
#[cfg(test)]
pub(crate) const RECORD_KINDS: [&str; 16] = [
    "folder",
    "shown",
    "step",
    "done",
    "tree",
    "marks",
    "place",
    "aside",
    "kept",
    "old-log",
    "found",
    "written",
    "volume",
    "medium",
    "new-marks",
    "in-file-system",
];

/// The name of the folder of the logs in Glimt's state folder, pinned in src/frozen-1.0.txt.
const JOBS_FOLDER: &str = "jobs";

/// The most bytes a field may have. A path has at most 4,096 on Linux, so a longer field is
/// damage, and the read stops there rather than fill the memory.
const MOST_FIELD_BYTES: u64 = 64 << 10;

/// The most fields a record may have. A record with more is passed over.
const MOST_FIELDS: usize = 8;

/// How old an empty log no window holds must be before it is taken for one whose header a kill
/// cut off, and removed. A log is locked a moment after it is made.
const EMPTY_LOG_AGE: std::time::Duration = std::time::Duration::from_secs(60);

/// The field that stands for no path or no fingerprint. No path is "-", as every path is
/// absolute.
const NONE: &str = "-";

/// A record of a job log, after the header. A step's `number` is its place among the log's
/// steps, counted from 0, and the later records of a step name it by that number.
#[derive(Debug, PartialEq)]
pub(crate) enum Record {
    /// A folder that the job's files are in, with its device and inode when the log was
    /// written, so that a start can tell another card at the same mount path.
    Folder {
        path: PathBuf,
        device: u64,
        inode: u64,
    },
    /// The volume that a folder of a `Folder` record is on, where its file system keeps no
    /// inode (FAT, exFAT): the name udev gives the volume in /dev/disk/by-uuid, the serial number
    /// the card was formatted with. It tells the card from another one at the same mount path. It
    /// is a record of its own, as Glimt 0.93.0 and 0.94.0 pass over a kind they do not know but
    /// drop a `folder` record with another number of fields.
    Volume {
        path: PathBuf,
        volume: std::ffi::OsString,
    },
    /// The medium that a folder of a `Folder` record is on, on a block device: the boot and the
    /// disk sequence number of the disk or card, as "boot:sequence"
    /// (`file_io::medium_of_device`). It tells the card from another one in the same reader
    /// where no volume can be read, as in a Flatpak's sandbox, until the computer starts anew.
    Medium { path: PathBuf, medium: String },
    /// Where a folder of a `Folder` record is in its file system, as /proc/self/mountinfo shows
    /// the mount that holds it (`file_io::path_in_file_system`). It tells the folder from an
    /// empty mount point at its path, or above it, on the same disk: a bind mount of another
    /// folder of that disk that is not mounted yet (#270).
    InFileSystem {
        path: PathBuf,
        in_file_system: PathBuf,
    },
    /// The collection shown while the job ran, whose entries follow the files.
    Shown(PathBuf),
    /// A step about to be taken: its file, its new name (`None` for a delete), and the
    /// fingerprint of its file (`None` when there is none to take).
    Step {
        number: usize,
        from: PathBuf,
        to: Option<PathBuf>,
        fingerprint: Option<FileFingerprint>,
    },
    /// The step with this number is done.
    Done(usize),
    /// A folder about to be renamed (`to`) or trashed (`None`), with every file in it.
    Tree { from: PathBuf, to: Option<PathBuf> },
    /// The marks and stars of the step's file, written before the marks file forgets them.
    Marks { number: usize, bits: u8, stars: u8 },
    /// The step's file's place in the collection shown.
    Place { number: usize, index: usize },
    /// The step's photo has its `sidecar` put aside under the name `hidden`.
    Aside {
        number: usize,
        sidecar: PathBuf,
        hidden: PathBuf,
    },
    /// The step must wait for a later start.
    Kept(usize),
    /// The log that this log was written from, which a start removes unread when a kill left it
    /// beside this one in the folder of the job logs. A start removes no other path that it
    /// names: one in a folder of Glimt 0.92 or older, which Glimt 0.93 and 0.94 wrote, is no
    /// longer read. The steps of a log written from such an old log are read as its own: each
    /// has a `done` record, as an old log's name meant a file that may have gone.
    OldLog(PathBuf),
    /// A start found the step of a delete for good, which has no `done`, with its photo gone,
    /// and kept its marks and its hidden sidecar for a later start, which finishes it.
    Found(usize),
    /// Glimt wrote the step's file anew in its place (a turn saved, a copy saved over it) after
    /// the step's record was written, and the file now has this fingerprint. A later record of
    /// a step takes the place of the fingerprint of an earlier one, so that a start does not
    /// take Glimt's own write for another program's file.
    Written {
        number: usize,
        fingerprint: FileFingerprint,
    },
    /// The marks and stars that the window knew at the step's new name when a Rename Many
    /// began, (0, 0) for none. A mark set at the new name while the job ran differs from them,
    /// so it can be told from an old mark that a file gone from that name left behind. A plan
    /// of Glimt 0.98 or older has none, and its marks follow as they did then.
    NewMarks { number: usize, bits: u8, stars: u8 },
}

impl Record {
    /// Returns the record's kind, one of `RECORD_KINDS`, and its fields as they are written.
    fn kind_and_fields(&self) -> (&'static str, Vec<Vec<u8>>) {
        let path = |p: &Path| p.as_os_str().as_bytes().to_vec();
        let maybe_path = |p: &Option<PathBuf>| p.as_deref().map_or(NONE.into(), path);
        let text = |t: &dyn ToString| t.to_string().into_bytes();
        match self {
            Record::Folder {
                path: folder,
                device,
                inode,
            } => ("folder", vec![path(folder), text(device), text(inode)]),
            Record::Volume {
                path: folder,
                volume,
            } => ("volume", vec![path(folder), volume.as_bytes().to_vec()]),
            Record::Medium {
                path: folder,
                medium,
            } => ("medium", vec![path(folder), medium.as_bytes().to_vec()]),
            Record::InFileSystem {
                path: folder,
                in_file_system,
            } => ("in-file-system", vec![path(folder), path(in_file_system)]),
            Record::Shown(collection) => ("shown", vec![path(collection)]),
            Record::Step {
                number,
                from,
                to,
                fingerprint,
            } => {
                let known = crate::files::journal::known_line(*fingerprint);
                let known = known.trim_end().as_bytes().to_vec();
                (
                    "step",
                    vec![text(number), path(from), maybe_path(to), known],
                )
            }
            Record::Done(number) => ("done", vec![text(number)]),
            Record::Tree { from, to } => ("tree", vec![path(from), maybe_path(to)]),
            Record::Marks {
                number,
                bits,
                stars,
            } => ("marks", vec![text(number), text(bits), text(stars)]),
            Record::Place { number, index } => ("place", vec![text(number), text(index)]),
            Record::Aside {
                number,
                sidecar,
                hidden,
            } => ("aside", vec![text(number), path(sidecar), path(hidden)]),
            Record::Kept(number) => ("kept", vec![text(number)]),
            Record::OldLog(log) => ("old-log", vec![path(log)]),
            Record::Found(number) => ("found", vec![text(number)]),
            Record::Written {
                number,
                fingerprint,
            } => {
                let known = crate::files::journal::known_line(Some(*fingerprint));
                let known = known.trim_end().as_bytes().to_vec();
                ("written", vec![text(number), known])
            }
            Record::NewMarks {
                number,
                bits,
                stars,
            } => ("new-marks", vec![text(number), text(bits), text(stars)]),
        }
    }

    /// Returns the record of kind `kind` with `fields`, and `None` for a kind this Glimt does
    /// not know, for the wrong number of fields, and for a field that does not parse.
    fn parse(kind: &[u8], fields: &[Vec<u8>]) -> Option<Record> {
        let record = match (kind, fields) {
            (b"folder", [p, d, i]) => Record::Folder {
                path: absolute(p)?,
                device: number(d)?,
                inode: number(i)?,
            },
            (b"volume", [p, v]) => Record::Volume {
                path: absolute(p)?,
                volume: plain_name(v)?.into_os_string(),
            },
            (b"medium", [p, m]) => Record::Medium {
                path: absolute(p)?,
                medium: medium(m)?,
            },
            (b"in-file-system", [p, f]) => Record::InFileSystem {
                path: absolute(p)?,
                in_file_system: absolute(f)?,
            },
            (b"shown", [p]) => Record::Shown(absolute(p)?),
            (b"step", [n, from, to, known]) => Record::Step {
                number: number(n)?,
                from: absolute(from)?,
                to: maybe_absolute(to)?,
                fingerprint: match std::str::from_utf8(known).ok()? {
                    NONE => None,
                    line => Some(crate::files::journal::parse_known_line(line)?),
                },
            },
            (b"done", [n]) => Record::Done(number(n)?),
            (b"tree", [from, to]) => Record::Tree {
                from: absolute(from)?,
                to: maybe_absolute(to)?,
            },
            (b"marks", [n, bits, stars]) => Record::Marks {
                number: number(n)?,
                bits: number(bits)?,
                stars: number(stars)?,
            },
            (b"place", [n, index]) => Record::Place {
                number: number(n)?,
                index: number(index)?,
            },
            (b"aside", [n, sidecar, hidden]) => Record::Aside {
                number: number(n)?,
                sidecar: absolute(sidecar)?,
                hidden: absolute(hidden)?,
            },
            (b"kept", [n]) => Record::Kept(number(n)?),
            (b"old-log", [p]) => Record::OldLog(absolute(p)?),
            (b"found", [n]) => Record::Found(number(n)?),
            (b"written", [n, known]) => Record::Written {
                number: number(n)?,
                fingerprint: crate::files::journal::parse_known_line(
                    std::str::from_utf8(known).ok()?,
                )?,
            },
            (b"new-marks", [n, bits, stars]) => Record::NewMarks {
                number: number(n)?,
                bits: number(bits)?,
                stars: number(stars)?,
            },
            _ => return None,
        };
        Some(record)
    }

    /// Returns the number of the step this record is about, and `None` for a record that
    /// belongs to no step.
    fn step_number(&self) -> Option<usize> {
        match self {
            Record::Step { number, .. }
            | Record::Done(number)
            | Record::Marks { number, .. }
            | Record::Place { number, .. }
            | Record::Aside { number, .. }
            | Record::Kept(number)
            | Record::Found(number)
            | Record::Written { number, .. }
            | Record::NewMarks { number, .. } => Some(*number),
            Record::Folder { .. }
            | Record::Volume { .. }
            | Record::Medium { .. }
            | Record::InFileSystem { .. }
            | Record::Shown(_)
            | Record::Tree { .. }
            | Record::OldLog(_) => None,
        }
    }

    /// Returns the number of the step this record is about, to be changed, and `None` for a
    /// record that belongs to no step.
    pub(crate) fn step_number_mut(&mut self) -> Option<&mut usize> {
        match self {
            Record::Step { number, .. }
            | Record::Done(number)
            | Record::Marks { number, .. }
            | Record::Place { number, .. }
            | Record::Aside { number, .. }
            | Record::Kept(number)
            | Record::Found(number)
            | Record::Written { number, .. }
            | Record::NewMarks { number, .. } => Some(number),
            Record::Folder { .. }
            | Record::Volume { .. }
            | Record::Medium { .. }
            | Record::InFileSystem { .. }
            | Record::Shown(_)
            | Record::Tree { .. }
            | Record::OldLog(_) => None,
        }
    }
}

/// What a start's look at a path, or a write it made about a job log, gives when it cannot
/// tell: a stick or share not mounted, an empty mount point at a folder's path, a look that
/// failed, or a write that failed. A log read with such an answer stays for a later start.
#[derive(Debug, PartialEq)]
pub(crate) struct CannotTell;

/// The one type of every answer a start gives about a path or a write: `Ok` when it is sure,
/// and `Err(CannotTell)` when it is not.
pub(crate) type AtStart<T> = Result<T, CannotTell>;

/// The proof that a job log may be removed, which `remove_job_log` takes. Its field is private,
/// so it is made only by `Sure::of`, from an answer that was sure: a start's look or write about
/// the log's files (`AtStart`), a write of the log that it depends on, or the start of a job of
/// this window, which tells its own failures. A log that a start read with `CannotTell` has
/// none, and stays for a later start.
pub(crate) struct Sure(());

impl Sure {
    /// Returns the proof that `answer` was sure, and `None` when it was not.
    pub(crate) fn of<T, E>(answer: &Result<T, E>) -> Option<Sure> {
        answer.is_ok().then_some(Sure(()))
    }
}

/// Which way a failed step went: forward, from its record's `from` to its `to`, or back, a file
/// put back from the step's `to` to its `from`.
#[derive(Clone, Copy)]
pub(crate) enum Direction {
    Forward,
    Back,
}

/// A job log being written, held locked while its job runs, so that no other window takes it
/// for one that a kill cut short.
pub(crate) struct JobLog {
    path: PathBuf,
    file: File,
    /// The proof that the log may be removed: there for the log of a job of this window, and for
    /// a log found at the start only once a reader read it surely (`read_surely`).
    sure: Option<Sure>,
    /// The files of the steps whose marks, stars and places the window has given or followed
    /// while the job ran. A log kept for the next start is written anew without their steps at
    /// the job's end (`journal::remove_after_the_writes`), so that the next start does not
    /// give them again over what the user changed since.
    applied: Vec<PathBuf>,
    /// The names, `from` and `to`, of the last `step` or `tree` record written to the log, by
    /// which a failed step is judged (`note_last_step_failed`). `None` when the log has none, or
    /// when the write of the last one failed.
    last_step: Option<(PathBuf, Option<PathBuf>)>,
}

impl JobLog {
    /// Makes a new log of a job of kind `job` (one of `JOB_KINDS`) in `dir`, locked, with its
    /// header and the `first` records (the `folder` and `shown` ones). They are on disk, with the
    /// log's name and its folder's, before this returns, so that the job's first step never
    /// comes before them. Returns the error when the log cannot be written, and no log is left.
    ///
    /// The log is made, locked and written in a hidden folder of its own inside `dir`, and only
    /// then renamed into `dir`. The lock goes with it, as a flock belongs to the open file, also
    /// on NFS, where it is a lock of the whole file. Every reader of `dir` passes over folders,
    /// so another window's start never sees a log that is not locked yet. Before, a start could
    /// open and lock a log made a moment ago, and its job then could not lock it.
    pub(crate) fn start(dir: &Path, job: &str, first: &[Record]) -> std::io::Result<JobLog> {
        use crate::files::file_io::sync_folder_of;
        let job = (JOB_KINDS.iter().find(|kind| **kind == job))
            .ok_or_else(|| std::io::Error::other(format!("no job of kind {job}")))?;
        std::fs::create_dir_all(dir)?;
        // The log is of a job of this window, which tells its own failures, so it may be
        // removed. So may its hidden folder, which holds this log alone or nothing.
        let started: std::io::Result<()> = Ok(());
        let name = crate::files::journal::new_name();
        let path = dir.join(&name);
        let hidden = crate::files::file_io::tmp_path(&path);
        std::fs::create_dir(&hidden)?;
        let made = hidden.join(&name);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let mut bytes = header_bytes(job, nanos, crate::features::remote::instance_id());
        bytes.extend(records_bytes(first));
        let renamed = crate::files::file_io::open_if_unlocked(&made, true).and_then(|mut file| {
            file.write_all(&bytes)?;
            file.sync_all()?;
            // A plain rename, which keeps the open file under its new name. `rename_never_over`
            // would link and unlink on NFS and sshfs, which keep a file unlinked while open as
            // a hidden one, and the hidden folder could not be removed. No file is at `path`,
            // as `new_name` gives a name of its own in each call of this window.
            #[allow(clippy::disallowed_methods)] // Glimt's own: a new log to a name no file has
            std::fs::rename(&made, &path)?;
            Ok(file)
        });
        // The hidden folder goes, and with it a log that was not renamed, as a kill that left
        // it there would leave no log either. A failure is told.
        if let Some(sure) = Sure::of(&started) {
            remove_job_log(&hidden, sure);
        }
        let file = renamed?;
        let sure = Sure::of(&started);
        let mut log = JobLog {
            path,
            file,
            sure,
            applied: Vec::new(),
            last_step: last_step_in(first),
        };
        let written = sync_folder_of(&log.path).and_then(|()| sync_folder_of(dir));
        match written {
            Ok(()) => Ok(log),
            // The job does not start when its log's name may not be on disk. The log names no
            // step yet, so nothing is lost with it, and a failure is told.
            Err(e) => {
                if let Some(sure) = log.end_of_job() {
                    remove_job_log(&log.path, sure);
                }
                Err(e)
            }
        }
    }

    /// Adds `records` to the log in one write, not synced. A kill keeps what was written, and a
    /// power cut that drops it leaves a log that tells less was done than was.
    pub(crate) fn add_records(&mut self, records: &[Record]) -> std::io::Result<()> {
        let written = self.file.write_all(&records_bytes(records));
        if let Some(step) = last_step_in(records) {
            self.last_step = written.is_ok().then_some(step);
        }
        written
    }

    /// Adds `records` to the log in one write, and syncs the log to disk before it returns. A
    /// record that a power cut must not lose (`aside`, `marks`, `place`, the middle of a swap)
    /// is written this way, before the change it is about.
    pub(crate) fn add_records_synced(&mut self, records: &[Record]) -> std::io::Result<()> {
        self.add_records(records)?;
        self.file.sync_all()
    }

    /// Removes the log, when its job is over and what it changed has been written, and lets
    /// go of its lock.
    #[cfg(test)]
    pub(crate) fn remove_log(mut self) -> bool {
        (self.end_of_job()).is_some_and(|sure| remove_job_log(&self.path, sure))
    }

    /// Returns the log at `path`, held locked as `file`, as a start found it, of this format or
    /// of an older one. It is written to no more, and removed as a log of its job is.
    pub(crate) fn found(path: PathBuf, file: File) -> JobLog {
        JobLog {
            path,
            file,
            sure: None,
            applied: Vec::new(),
            last_step: None,
        }
    }

    /// Returns the log with the proof `sure` that the start read it surely, so that it may be
    /// removed once what it says has been written.
    pub(crate) fn read_surely(mut self, sure: Sure) -> JobLog {
        self.sure = Some(sure);
        self
    }

    /// Notes that the last step written to the log failed with `error`, going the way
    /// `direction` says. When the step may have been done all the same
    /// (`file_io::may_have_been_done`), a share that lost its answer say, the log stays on disk
    /// for the next start, which judges the step by what it finds there. The step is judged by
    /// the name it took the file from, which the log takes from its own record, so that no
    /// caller can name another: the record's `from` going forward, its `to` going back. A log
    /// without a step it could write stays too.
    pub(crate) fn note_last_step_failed(&mut self, direction: Direction, error: &std::io::Error) {
        let source = match (&self.last_step, direction) {
            (Some((from, _)), Direction::Forward) => Some(from),
            (Some((_, to)), Direction::Back) => to.as_ref(),
            (None, _) => None,
        };
        if source.is_none_or(|source| crate::files::file_io::may_have_been_done(error, source)) {
            self.keep_for_the_next_start();
        }
    }

    /// Keeps the log on disk for the next start, because the job could not tell what one of its
    /// steps did. The log loses its proof that it may be removed, so no end of the job removes
    /// it, and once it is dropped it is unlocked, as a log that a kill left.
    pub(crate) fn keep_for_the_next_start(&mut self) {
        self.sure = None;
    }

    /// Notes that the window has given or followed the marks, stars and places of the steps on
    /// the files `paths`, and of the files in them when they are folders.
    pub(crate) fn note_steps_applied(&mut self, paths: &[PathBuf]) {
        self.applied.extend_from_slice(paths);
    }

    /// Returns the files of the steps that the window has applied (`note_steps_applied`).
    pub(crate) fn take_steps_applied(&mut self) -> Vec<PathBuf> {
        std::mem::take(&mut self.applied)
    }

    /// Ends the log's job: takes the proof that the log may be removed, which it has no more
    /// after this. This is the one way a log's proof leaves it. Returns `None` when a step of
    /// the job could not tell what it did (`note_last_step_failed`, `keep_for_the_next_start`), and
    /// for a log found at the start that no reader read surely, so that the log stays.
    pub(crate) fn end_of_job(&mut self) -> Option<Sure> {
        self.sure.take()
    }

    /// Returns the log's path.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the log's file, open and locked.
    pub(crate) fn file(&self) -> &File {
        &self.file
    }

    /// Returns the log's path and its file, which holds its lock while it stays open.
    pub(crate) fn into_path_and_file(self) -> (PathBuf, File) {
        (self.path, self.file)
    }
}

/// A job log that no running window holds locked, which a kill cut short: the log, held locked,
/// the kind of its job, how the start reads it, and its records.
pub(crate) struct FoundLog {
    pub(crate) log: JobLog,
    pub(crate) job: &'static str,
    pub(crate) read_as: crate::files::journal::ReadAtStart,
    pub(crate) records: Vec<Record>,
}

/// What reading a job log found.
#[derive(Debug, PartialEq)]
pub(crate) enum LogRead {
    /// A log of this version: the kind of its job, one of `JOB_KINDS`, the time it was started
    /// in nanoseconds since 1970, the instance of the window that wrote it, and its records.
    Log {
        job: &'static str,
        started: u128,
        instance: String,
        records: Vec<Record>,
    },
    /// A log of a higher version, which a later Glimt wrote. It is left alone, on disk.
    Newer(u32),
    /// A file whose header is missing, damaged or of a kind of job this Glimt does not know.
    NotALog,
}

/// Returns the names, `from` and `to`, of the last `step` or `tree` record of `records`, and
/// `None` when they have none.
fn last_step_in(records: &[Record]) -> Option<(PathBuf, Option<PathBuf>)> {
    records.iter().rev().find_map(|record| match record {
        Record::Step { from, to, .. } | Record::Tree { from, to } => {
            Some((from.clone(), to.clone()))
        }
        _ => None,
    })
}

/// Returns the folder of the job logs (`JobLog`) in Glimt's state folder.
pub(crate) fn jobs_dir() -> Option<PathBuf> {
    crate::files::config::xdg_dir("XDG_STATE_HOME", ".local/state", "glimt")
        .map(|d| d.join(JOBS_FOLDER))
}

/// Opens the job log at `path` and reads it, when no running window holds it locked. Returns the
/// log opened, which holds its lock while it stays open, and what was read. Only a regular file
/// is opened, never through a link, and it is read as a stream of at most `MAX_DECODE_BYTES`.
pub(crate) fn read_job_log(path: &Path) -> std::io::Result<(File, LogRead)> {
    let file = crate::files::file_io::open_if_unlocked(path, false)?;
    let bounded = std::io::Read::take(&file, crate::images::decode::MAX_DECODE_BYTES);
    // The parser is fed broken logs in survives_broken_job_logs.
    let read = parse_job_log(&mut std::io::BufReader::new(bounded))?;
    Ok((file, read))
}

/// Reads the job log at `path` as `read_job_log` does, but without taking its lock: for a
/// caller that holds the lock already through a file of its own, or that only looks.
pub(crate) fn read_job_log_without_lock(path: &Path) -> std::io::Result<LogRead> {
    let file = crate::images::decode::open_regular(path)?;
    let bounded = std::io::Read::take(file, crate::images::decode::MAX_DECODE_BYTES);
    // The parser is fed broken logs in survives_broken_job_logs.
    parse_job_log(&mut std::io::BufReader::new(bounded))
}

/// Returns the logs in `dir` that no running window holds locked, left by a Glimt that was
/// killed during their jobs, each held locked until it is removed or let go. A log of a higher
/// version, of a kind of job this Glimt does not know, or of a kind that the start has no reader
/// for (`journal::read_at_start`), is left on disk for a Glimt that can finish it, and the user
/// is told once that it is there. Only an empty file is removed, once
/// it is older than `EMPTY_LOG_AGE`: a kill between the making of a log and its header leaves
/// it, and a log made a moment ago is not locked yet. So is a hidden folder that a kill left
/// inside `JobLog::start`, once it is as old.
pub(crate) fn logs_left_by_a_kill(dir: &Path) -> Vec<FoundLog> {
    let (mut found, mut left_alone) = (Vec::new(), 0);
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        // The hidden folder of a log that `JobLog::start` was making when a kill came. Its log
        // names no step yet, or it is a log written anew from one that is still beside it, as
        // that one is removed only after the new one is renamed in. So nothing is lost with the
        // folder, which goes once it is old enough that no start is still making it. A link is
        // not followed.
        if kind.is_dir()
            && entry
                .file_name()
                .as_bytes()
                .starts_with(TMP_PREFIX.as_bytes())
        {
            let age = (entry.metadata().ok())
                .and_then(|meta| meta.modified().ok())
                .and_then(|made| made.elapsed().ok());
            let is_old = age.filter(|age| *age > EMPTY_LOG_AGE).ok_or(CannotTell);
            if let Some(sure) = Sure::of(&is_old) {
                remove_job_log(&path, sure);
            }
            continue;
        }
        // A pipe or a link planted in the state folder is no log.
        if !kind.is_file() {
            continue;
        }
        let Ok((file, read)) = read_job_log(&path) else {
            continue;
        };
        // A log that another window put right and removed has no name any more. A failed look
        // at the open file says nothing, and the log is left for a later start.
        let Ok(Some(meta)) = metadata_while_named(&file) else {
            continue;
        };
        match read {
            LogRead::Log { job, records, .. } => match crate::files::journal::read_at_start(job) {
                Some(read_as) => found.push(FoundLog {
                    log: JobLog::found(path, file),
                    job,
                    read_as,
                    records,
                }),
                None => left_alone += 1,
            },
            LogRead::NotALog if meta.len() == 0 => {
                let age = meta.modified().ok().and_then(|made| made.elapsed().ok());
                let is_old = age.filter(|age| *age > EMPTY_LOG_AGE).ok_or(CannotTell);
                if let Some(sure) = Sure::of(&is_old) {
                    remove_job_log(&path, sure);
                }
            }
            LogRead::Newer(_) | LogRead::NotALog => left_alone += 1,
        }
    }
    if left_alone > 0 {
        crate::files::file_io::save_failed(crate::tf!(
            "{folder} holds logs that a newer Glimt left, or that this Glimt cannot read. They are kept for a Glimt that can finish them.",
            folder = escaped_name(dir)
        ));
    }
    found
}

/// Returns the metadata of the open log `file`, and `None` when the file has no name any more.
/// A failed look is passed on, as it says nothing about the name.
fn metadata_while_named(file: &File) -> std::io::Result<Option<std::fs::Metadata>> {
    use std::os::unix::fs::MetadataExt;
    let meta = file.metadata()?;
    Ok((meta.nlink() > 0).then_some(meta))
}

/// Reads a job log from `reader`, as `LogRead` tells. A record cut off at the end, by a kill or a
/// full disk, is left out, as its last path may be cut at a `/`. So is a record of a kind this
/// Glimt does not know, one with a field that does not parse, a step whose number does not come
/// next, and a record about a step the log does not have before it.
fn parse_job_log(reader: &mut impl std::io::BufRead) -> std::io::Result<LogRead> {
    let Some(header) = next_record(reader)? else {
        return Ok(LogRead::NotALog);
    };
    let version = header.get(1).and_then(|v| number::<u32>(v));
    match (header.first().map(Vec::as_slice), version) {
        (Some(word), Some(v)) if word == HEADER.as_bytes() && v > VERSION => {
            return Ok(LogRead::Newer(v));
        }
        (Some(word), Some(VERSION)) if word == HEADER.as_bytes() => {}
        _ => return Ok(LogRead::NotALog),
    }
    let [_, _, job, started, instance] = &header[..] else {
        return Ok(LogRead::NotALog);
    };
    let old = (OLD_JOB_KINDS.iter()).find_map(|(old, now)| (old.as_bytes() == job).then_some(now));
    let (Some(job), Some(started)) = (
        (JOB_KINDS.iter().find(|kind| kind.as_bytes() == job)).or(old),
        number(started),
    ) else {
        return Ok(LogRead::NotALog);
    };
    let mut records = Vec::new();
    let mut steps = 0;
    while let Some(fields) = next_record(reader)? {
        let Some((kind, fields)) = fields.split_first() else {
            continue;
        };
        let Some(record) = Record::parse(kind, fields) else {
            continue;
        };
        match (&record, record.step_number()) {
            (Record::Step { .. }, Some(n)) if n == steps => steps += 1,
            (Record::Step { .. }, _) => continue,
            (_, Some(n)) if n >= steps => continue,
            _ => {}
        }
        records.push(record);
    }
    Ok(LogRead::Log {
        job,
        started,
        instance: String::from_utf8_lossy(instance).into_owned(),
        records,
    })
}

/// Reads the next record's fields from `reader`. Returns `None` at the end, and for a record cut
/// off there, and also for a field longer than `MOST_FIELD_BYTES`, after which nothing more is
/// read. A record with more than `MOST_FIELDS` fields is read to its end and given as no fields.
fn next_record(reader: &mut impl std::io::BufRead) -> std::io::Result<Option<Vec<Vec<u8>>>> {
    let mut fields = Vec::new();
    let mut is_too_long = false;
    loop {
        let mut field = Vec::new();
        crate::images::decode::read_until_limited(reader, MOST_FIELD_BYTES, 0, &mut field)?;
        if field.pop() != Some(0) {
            return Ok(None);
        }
        if field.is_empty() {
            return Ok(Some(if is_too_long { Vec::new() } else { fields }));
        }
        if fields.len() == MOST_FIELDS {
            is_too_long = true;
        } else {
            fields.push(field);
        }
    }
}

/// Returns the bytes of a log's header for a job of kind `job` started at `nanos` by the window
/// `instance`. A NUL byte or a line break in the instance's name would end its field or the
/// record early, and the log would be taken for no log. So every control character in the name
/// is written as `_`. The name is only told, never looked up, so nothing needs its exact bytes.
fn header_bytes(job: &str, nanos: u128, instance: &str) -> Vec<u8> {
    let instance: String = (instance.chars())
        .map(|c| if c.is_control() { '_' } else { c })
        .collect();
    let nanos = nanos.to_string();
    let version = VERSION.to_string();
    let header = [HEADER, &version, job, &nanos, &instance];
    let mut bytes = Vec::new();
    add_record(&mut bytes, &header.map(|field| field.as_bytes().to_vec()));
    bytes
}

/// Returns the bytes of `records`, as they are written to a log.
fn records_bytes(records: &[Record]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for record in records {
        let (kind, fields) = record.kind_and_fields();
        add_record(
            &mut bytes,
            &[vec![kind.as_bytes().to_vec()], fields].concat(),
        );
    }
    bytes
}

/// Adds the record of `fields` to `bytes`: each field with its NUL, and the empty field that
/// ends the record.
fn add_record(bytes: &mut Vec<u8>, fields: &[Vec<u8>]) {
    for field in fields {
        bytes.extend(field);
        bytes.push(0);
    }
    bytes.push(0);
}

/// Returns `field` as an absolute path with no `..` in it, and `None` for any other, as a log
/// is read as untrusted text.
fn absolute(field: &[u8]) -> Option<PathBuf> {
    let path = Path::new(std::ffi::OsStr::from_bytes(field));
    let is_plain = path.components().all(|c| c != Component::ParentDir);
    (path.is_absolute() && is_plain).then(|| path.to_path_buf())
}

/// Returns `field` as `absolute` does, or `Some(None)` for "-", which stands for no path.
fn maybe_absolute(field: &[u8]) -> Option<Option<PathBuf>> {
    match field {
        b"-" => Some(None),
        path => absolute(path).map(Some),
    }
}

/// Returns `field` as the name of a file in a folder: not empty, with no `/`, and neither `.`
/// nor `..`. Returns `None` for any other.
fn plain_name(field: &[u8]) -> Option<PathBuf> {
    let is_plain = !field.is_empty() && !field.contains(&b'/') && field != b"." && field != b"..";
    is_plain.then(|| PathBuf::from(std::ffi::OsStr::from_bytes(field)))
}

/// Returns `field` as a medium, "boot:sequence" (`Record::Medium`): a boot's id of hex digits
/// and dashes, and a number. Returns `None` for any other.
fn medium(field: &[u8]) -> Option<String> {
    let (boot, sequence) = std::str::from_utf8(field).ok()?.split_once(':')?;
    let is_boot = !boot.is_empty() && boot.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
    (is_boot && sequence.parse::<u64>().is_ok()).then(|| format!("{boot}:{sequence}"))
}

/// Returns `field` as a number of type `T`, and `None` when it is not one.
fn number<T: std::str::FromStr>(field: &[u8]) -> Option<T> {
    std::str::from_utf8(field).ok()?.parse().ok()
}

/// Removes the job log or plan at `path`, or a folder of the logs of Glimt 0.92 and older that
/// holds none (`journal::clear_old_log_folders`), the one way any of them is removed
/// (`source_rules::job_logs_are_removed_in_one_place`). It takes the proof `Sure`, which only a
/// sure answer makes, so that no log goes after a look or a write that could not tell. A
/// failure is told. Returns whether it is gone.
pub(crate) fn remove_job_log(path: &Path, _: Sure) -> bool {
    crate::files::file_io::remove_own_or_tell(path)
}

#[cfg(test)]
mod tests {
    use super::{LogRead, Record};
    use std::path::PathBuf;

    /// Returns one record of every kind, with the steps their numbers name before them.
    fn every_record() -> Vec<Record> {
        let p = PathBuf::from;
        vec![
            Record::Folder {
                path: p("/photos"),
                device: 2049,
                inode: 77,
            },
            Record::Shown(p("/c/a.gqv")),
            Record::Step {
                number: 0,
                from: p("/photos/a.jpg"),
                to: Some(p("/photos/b.jpg")),
                fingerprint: Some((Some((2049, 78)), 1234, (1_700_000_000, 5))),
            },
            Record::Step {
                number: 1,
                from: p("/photos/c d\n.jpg"),
                to: None,
                fingerprint: None,
            },
            Record::Done(0),
            Record::Tree {
                from: p("/photos/old"),
                to: None,
            },
            Record::Marks {
                number: 1,
                bits: 5,
                stars: 3,
            },
            Record::Place {
                number: 1,
                index: 17,
            },
            Record::Aside {
                number: 1,
                sidecar: p("/photos/c d.xmp"),
                hidden: p("/photos/.glimt-rename-1"),
            },
            Record::Kept(1),
            Record::Tree {
                from: p("/photos/new"),
                to: Some(p("/photos/newer")),
            },
            Record::OldLog(p("/state/glimt/jobs/1-2")),
            Record::Found(1),
            Record::Written {
                number: 0,
                fingerprint: (None, 99, (1_700_000_001, 0)),
            },
            Record::Medium {
                path: p("/card/DCIM"),
                medium: "0f5e8c2a-1b3d-4e6f-8a9b-0c1d2e3f4a5b:42".into(),
            },
            Record::NewMarks {
                number: 1,
                bits: 3,
                stars: 4,
            },
            Record::InFileSystem {
                path: p("/home/u/photos/2024"),
                in_file_system: p("/srv/photos/2024"),
            },
            Record::Volume {
                path: p("/card/DCIM"),
                volume: "1234-ABCD".into(),
            },
        ]
    }

    /// Returns the bytes of a log of a Move in the version `version`, with `records`.
    fn log_bytes(version: &str, records: &[Record]) -> Vec<u8> {
        let mut bytes = format!("glimt-job\0{version}\0move\09\0w1\0\0").into_bytes();
        bytes.extend(super::records_bytes(records));
        bytes
    }

    /// Returns what the parser reads from `bytes`.
    fn parsed(bytes: &[u8]) -> LogRead {
        super::parse_job_log(&mut std::io::Cursor::new(bytes)).unwrap()
    }

    /// Returns the records the parser reads from `bytes`, which must be read as a log.
    fn records_of(bytes: &[u8]) -> Vec<Record> {
        match parsed(bytes) {
            LogRead::Log { records, .. } => records,
            other => panic!("not read as a log: {other:?}"),
        }
    }

    /// Every kind of record is written and read back as it was, through a log on disk. The
    /// writer holds it locked, and the reader reads it once the writer has let it go.
    #[test]
    fn every_record_kind_is_read_back_as_written() {
        let dir = std::env::temp_dir().join(format!("glimt-test-job-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let records = every_record();
        let kinds: Vec<&str> = records.iter().map(|r| r.kind_and_fields().0).collect();
        for kind in super::RECORD_KINDS {
            assert!(kinds.contains(&kind), "no {kind} record in the test");
        }
        let (first, rest) = records.split_at(2);
        let mut log = super::JobLog::start(&dir, "trash", first).unwrap();
        let (steps, synced) = rest.split_at(3);
        log.add_records(steps).unwrap();
        log.add_records_synced(synced).unwrap();
        let path = log.path.clone();
        let refused = super::read_job_log(&path).unwrap_err();
        assert_eq!(refused.kind(), std::io::ErrorKind::WouldBlock);
        drop(log);
        let (held, read) = super::read_job_log(&path).unwrap();
        let LogRead::Log {
            job,
            instance,
            records: read_back,
            ..
        } = read
        else {
            panic!("not read as a log: {read:?}");
        };
        assert_eq!(job, "trash");
        assert_eq!(instance, crate::features::remote::instance_id());
        assert_eq!(read_back, records);
        drop(held);
        let log = super::JobLog::start(&dir, "move", &[]).unwrap();
        let path = log.path.clone();
        assert!(log.remove_log());
        assert!(crate::files::file_io::is_surely_gone(&path));
        assert!(super::JobLog::start(&dir, "copy", &[]).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A log cut off anywhere, as by a kill in the middle of a write, is read with every record
    /// that was whole, and without the one cut off, whose last path may end at a `/`.
    #[test]
    fn a_record_cut_off_by_a_kill_is_left_out() {
        let records = every_record();
        let whole = log_bytes("1", &records);
        let header = log_bytes("1", &[]).len();
        for cut in header..=whole.len() {
            let whole_records = (0..=records.len())
                .filter(|&n| header + super::records_bytes(&records[..n]).len() <= cut)
                .max()
                .unwrap();
            assert_eq!(records_of(&whole[..cut]), records[..whole_records], "{cut}");
        }
        let mut torn = log_bytes("1", &records[..3]);
        torn.extend(b"step\x001\0/photos/b/");
        assert_eq!(records_of(&torn), records[..3]);
        assert_eq!(parsed(b"glimt-job\x001\0move\09\0w1\0"), LogRead::NotALog);
    }

    /// A path that is relative or goes up with `..` makes its record be passed over, and so does
    /// a volume with a `/`. The records around it are kept.
    #[test]
    fn relative_paths_and_dot_dot_are_refused() {
        let good = log_bytes("1", &every_record()[..3]);
        for bad in [
            &b"shown\0c/a.gqv\0\0"[..],
            b"shown\0/c/../a.gqv\0\0",
            b"tree\0/photos/old\0../x\0\0",
            b"step\x001\0/photos/a.jpg\0photos/b.jpg\0-\0\0",
            b"aside\x000\0/p/a.xmp\0/p/../q/.h\0\0",
            b"old-log\0../deletes/1-2\0\0",
            b"volume\0/card\0../1234-ABCD\0\0",
        ] {
            let mut bytes = good.clone();
            bytes.extend(bad);
            bytes.extend(super::records_bytes(&[Record::Done(0)]));
            let mut expected = every_record();
            expected.truncate(3);
            expected.push(Record::Done(0));
            assert_eq!(records_of(&bytes), expected, "{}", bad.escape_ascii());
        }
    }

    /// A log of an Undo of a Move that a kill left under Glimt 0.99.0, of the kind "undo-move",
    /// is read as a Move's, the kind that this Glimt writes for both.
    /// Risk: such a log is kept for good as one of a kind this Glimt does not know, and the marks
    /// of the files that went back stay at their names in the folder they left.
    #[test]
    fn an_undo_move_log_of_0_99_is_read_as_a_move() {
        let mut bytes = b"glimt-job\x001\0undo-move\09\0w1\0\0".to_vec();
        bytes.extend(super::records_bytes(&every_record()[..3]));
        let LogRead::Log { job, records, .. } = parsed(&bytes) else {
            panic!("not read as a log");
        };
        assert_eq!((job, records.len()), ("move", 3));
        assert_eq!(
            crate::files::journal::read_at_start(job),
            Some(crate::files::journal::ReadAtStart::Move)
        );
    }

    /// Every record is written in the shape that Glimt 0.93.0 and 0.94.0 read: a kind they know
    /// has the number of fields their parser matches, and a new kind is one they pass over. Their
    /// parser drops a record of a known kind with another number of fields, so a `folder` record
    /// with a fourth field would be lost to them, and a start of theirs would read its log as one
    /// that noted no folder (#266).
    #[test]
    fn every_record_is_read_by_the_last_releases_or_passed_over() {
        // The kinds and numbers of fields of `Record::parse` in v0.93.0 and v0.94.0.
        let read_by_0_94 = [
            ("folder", 3),
            ("shown", 1),
            ("step", 4),
            ("done", 1),
            ("tree", 2),
            ("trashed", 3),
            ("marks", 3),
            ("place", 2),
            ("aside", 3),
            ("kept", 1),
            ("old-log", 1),
            ("found", 1),
            ("written", 2),
        ];
        for record in every_record() {
            let (kind, fields) = record.kind_and_fields();
            if let Some((_, count)) = read_by_0_94.iter().find(|(old, _)| *old == kind) {
                assert_eq!(fields.len(), *count, "{kind}");
            }
        }
        let volume = every_record().into_iter().last().unwrap();
        assert!(matches!(volume, Record::Volume { .. }));
        assert!(!read_by_0_94.iter().any(|(kind, _)| *kind == "volume"));
        // This parser passes over a kind it does not know, as theirs does, and keeps the rest.
        let mut bytes = log_bytes("1", &every_record()[..3]);
        bytes.extend(b"later-kind\0/card\0x\0\0");
        bytes.extend(super::records_bytes(&[Record::Done(0)]));
        let mut expected = every_record();
        expected.truncate(3);
        expected.push(Record::Done(0));
        assert_eq!(records_of(&bytes), expected);
    }

    /// A log of a higher version is left alone, with its version told, also when its header
    /// has more fields. A file whose header is not this format's is no log.
    #[test]
    fn a_higher_version_is_left_alone() {
        assert_eq!(parsed(&log_bytes("2", &every_record())), LogRead::Newer(2));
        let longer = b"glimt-job\x002\0move\09\0w1\0more\0\0new-kind\0x\0\0";
        assert_eq!(parsed(longer), LogRead::Newer(2));
        for not_a_log in [
            &b""[..],
            b"\0",
            b"glimt-job\x000\0move\09\0w1\0\0",
            b"glimt-job\x001\0copy\09\0w1\0\0",
            b"glimt-jobs\x001\0move\09\0w1\0\0",
        ] {
            assert_eq!(parsed(not_a_log), LogRead::NotALog);
        }
        assert!(matches!(parsed(&log_bytes("1", &[])), LogRead::Log { .. }));
    }

    /// A start finds the logs no window holds, and never removes a log of a higher version or
    /// of a kind of job it does not know: it leaves them, and tells once that they are there. An
    /// empty file older than a minute, whose header a kill cut off, is removed, and so is a
    /// hidden folder older than a minute that a kill left inside `JobLog::start` (#273). A log a
    /// running window holds is passed over, and so is a hidden folder made a moment ago.
    #[test]
    fn a_start_leaves_the_logs_it_cannot_read_and_tells_once() {
        let dir = std::env::temp_dir().join(format!("glimt-test-job-left-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let held = super::JobLog::start(&dir, "move", &[]).unwrap();
        let left = super::JobLog::start(&dir, "trash", &[]).unwrap();
        drop(left);
        let (newer, unknown, empty) = (dir.join("newer"), dir.join("unknown"), dir.join("empty"));
        std::fs::write(&newer, log_bytes("2", &every_record())).unwrap();
        std::fs::write(&unknown, b"glimt-job\x001\0copy\09\0w1\0\0").unwrap();
        let hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        let made = std::fs::File::create(&empty).unwrap();
        made.set_modified(hour_ago).unwrap();
        drop(made);
        let (killed, making) = (dir.join(".glimt-tmp-killed"), dir.join(".glimt-tmp-making"));
        for hidden in [&killed, &making] {
            std::fs::create_dir(hidden).unwrap();
            std::fs::write(hidden.join("log"), log_bytes("1", &[])).unwrap();
        }
        (std::fs::File::open(&killed).unwrap())
            .set_modified(hour_ago)
            .unwrap();
        let _ = crate::files::file_io::take_saves_failed();
        let found = super::logs_left_by_a_kill(&dir);
        assert_eq!(found.iter().map(|f| f.job).collect::<Vec<_>>(), ["trash"]);
        let told = crate::files::file_io::take_saves_failed();
        assert_eq!(told.len(), 1, "{told:?}");
        assert!(newer.exists() && unknown.exists() && !empty.exists());
        assert!(!killed.exists() && making.join("log").exists());
        drop((found, held));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Another window's start that opens and locks every file in the folder of the logs (as
    /// `read_job_log` does) in the moment between the making of a new log and its lock finds no
    /// new log there, and the job gets its log, locked, under its own name. Before, the start
    /// locked the new log first, and the job could not start it.
    #[test]
    fn a_start_in_another_window_never_locks_a_new_log_first() {
        let dir = std::env::temp_dir().join(format!("glimt-test-job-race-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let held = std::rc::Rc::new(std::cell::RefCell::new(None));
        let in_hook = (dir.clone(), held.clone());
        crate::files::file_io::BEFORE_NEW_FILE_IS_LOCKED.set(Some(Box::new(move |_| {
            let (dir, held) = in_hook;
            let files: Vec<std::fs::File> = (std::fs::read_dir(&dir).unwrap().flatten())
                .filter(|e| e.file_type().unwrap().is_file())
                .filter_map(|e| super::read_job_log(&e.path()).ok().map(|(file, _)| file))
                .collect();
            assert!(super::logs_left_by_a_kill(&dir).is_empty());
            *held.borrow_mut() = Some(files);
        })));
        let log = super::JobLog::start(&dir, "rename", &[]);
        let held = held.borrow_mut().take().expect("the hook ran");
        let log = log.unwrap();
        assert!(held.is_empty(), "the start locked {} new log", held.len());
        assert_eq!(log.path().parent(), Some(dir.as_path()));
        let refused = super::read_job_log(log.path()).unwrap_err();
        assert_eq!(refused.kind(), std::io::ErrorKind::WouldBlock);
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            1,
            "the hidden folder is gone"
        );
        drop(log);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A start on a file system without a rename that never writes over (NFS, sshfs), where
    /// a file unlinked while open is kept under a hidden name that cannot be removed (EBUSY),
    /// tells nothing and leaves only its log. Before, the log was linked into the folder of the
    /// logs and its hidden name unlinked, and every start told that it could not be removed.
    #[test]
    fn a_start_on_a_share_without_rename_flags_tells_nothing() {
        use crate::files::file_io::{BEFORE_NEW_FILE_IS_LOCKED, RENAME_WAYS_MISSING, UNLINK_FAILS};
        let dir = std::env::temp_dir().join(format!("glimt-test-job-nfs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        RENAME_WAYS_MISSING.set(1);
        BEFORE_NEW_FILE_IS_LOCKED.set(Some(Box::new(|made| {
            UNLINK_FAILS.set(Some((made.to_path_buf(), libc::EBUSY)));
        })));
        let log = super::JobLog::start(&dir, "rename", &[]);
        RENAME_WAYS_MISSING.set(0);
        UNLINK_FAILS.set(None);
        let log = log.unwrap();
        assert_eq!(crate::files::file_io::take_save_error(), None);
        let names: Vec<_> = (std::fs::read_dir(&dir).unwrap().flatten())
            .map(|e| e.file_name())
            .collect();
        assert_eq!(
            names,
            [log.path().file_name().unwrap()],
            "only the log is left"
        );
        drop(log);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A window whose name holds a NUL byte or a line break writes a header that is read back
    /// as a log, with those characters written as `_`. Before, the NUL ended the name's field
    /// early, and the log was taken for no log.
    #[test]
    fn an_instance_name_cannot_break_the_header() {
        let bytes = super::header_bytes("move", 9, "w\0x\ny\r");
        let LogRead::Log { instance, .. } = parsed(&bytes) else {
            panic!("not read as a log");
        };
        assert_eq!(instance, "w_x_y_");
    }

    /// Damaged logs are read without a panic. Every record read names only absolute paths with
    /// no `..`, and only steps that the log has before it.
    #[test]
    fn survives_broken_job_logs() {
        let seed = log_bytes("1", &every_record());
        for bytes in crate::support::test_support::broken(&seed) {
            let LogRead::Log { records, .. } = parsed(&bytes) else {
                continue;
            };
            let mut steps = 0;
            for record in &records {
                if let Some(n) = record.step_number() {
                    assert!(n <= steps, "{record:?}");
                }
                if let Record::Step { .. } = record {
                    steps += 1;
                }
                let (_, fields) = record.kind_and_fields();
                for field in fields.iter().filter(|f| f.starts_with(b"/")) {
                    assert!(super::absolute(field).is_some(), "{record:?}");
                }
            }
        }
    }
}
