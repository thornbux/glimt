//! The job log: one file in `$XDG_STATE_HOME/glimt/jobs/` for each job that changes files, which
//! the job writes before and after its steps, so that the next start can put right a job that a
//! kill cut short (docs/design/log-format.md). Every field ends in a NUL byte, as a path can hold
//! any other byte, and an empty field ends a record. The first record is the header, with the
//! format's version, so that a later Glimt can add kinds of record, and an older one leaves a
//! log of a higher version alone.

use crate::files::file_io::FileFingerprint;
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
pub(crate) const JOB_KINDS: [&str; 8] = [
    "rename",
    "move",
    "delete",
    "trash",
    "undo-move",
    "undo-trash",
    "rename-folder",
    "trash-folder",
];

/// The kinds of record after the header, as `Record::kind` names them. They are pinned in
/// src/frozen-1.0.txt.
#[cfg(test)]
pub(crate) const RECORD_KINDS: [&str; 13] = [
    "folder", "shown", "step", "done", "tree", "trashed", "marks", "place", "aside", "kept",
    "old-log", "found", "written",
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

/// The field of a `trashed` record that names the trash in the home folder.
const HOME_TRASH: &str = "home";

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
    /// The step's file is in the trash at `top` (`None` for the trash in the home folder),
    /// under the `.trashinfo` file named `info_name`.
    Trashed {
        number: usize,
        top: Option<PathBuf>,
        info_name: PathBuf,
    },
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
            Record::Trashed {
                number,
                top,
                info_name,
            } => {
                let top = top.as_deref().map_or(HOME_TRASH.into(), path);
                ("trashed", vec![text(number), top, path(info_name)])
            }
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
            (b"trashed", [n, top, info]) => Record::Trashed {
                number: number(n)?,
                top: match &top[..] {
                    b"home" => None,
                    top => Some(absolute(top)?),
                },
                info_name: plain_name(info)?,
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
            | Record::Trashed { number, .. }
            | Record::Marks { number, .. }
            | Record::Place { number, .. }
            | Record::Aside { number, .. }
            | Record::Kept(number)
            | Record::Found(number)
            | Record::Written { number, .. } => Some(*number),
            Record::Folder { .. } | Record::Shown(_) | Record::Tree { .. } | Record::OldLog(_) => {
                None
            }
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

/// A job log being written, held locked while its job runs, so that no other window takes it
/// for one that a kill cut short.
pub(crate) struct JobLog {
    path: PathBuf,
    file: File,
    /// The proof that the log may be removed: there for the log of a job of this window, and for
    /// a log found at the start only once a reader read it surely (`read_surely`).
    sure: Option<Sure>,
}

impl JobLog {
    /// Makes a new log of a job of kind `job` (one of `JOB_KINDS`) in `dir`, locked, with its
    /// header and the `first` records (the `folder` and `shown` ones). They are on disk, with the
    /// log's name and its folder's, before this returns, so that the job's first step never
    /// comes before them. Returns the error when the log cannot be written, and no log is left.
    pub(crate) fn start(dir: &Path, job: &str, first: &[Record]) -> std::io::Result<JobLog> {
        let job = (JOB_KINDS.iter().find(|kind| **kind == job))
            .ok_or_else(|| std::io::Error::other(format!("no job of kind {job}")))?;
        std::fs::create_dir_all(dir)?;
        let path = dir.join(crate::files::journal::new_name());
        let file = crate::files::file_io::open_if_unlocked(&path, true)?;
        let started: std::io::Result<()> = Ok(());
        let sure = Sure::of(&started);
        let mut log = JobLog { path, file, sure };
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let mut bytes = header_bytes(job, nanos, crate::features::remote::instance_id());
        bytes.extend(records_bytes(first));
        let written = (log.file.write_all(&bytes))
            .and_then(|()| log.file.sync_all())
            .and_then(|()| crate::files::file_io::sync_folder_of(&log.path))
            .and_then(|()| crate::files::file_io::sync_folder_of(dir));
        match written {
            Ok(()) => Ok(log),
            // A log left half written would be read at the next start as one a kill cut short.
            // It names no step yet, so nothing is lost with it, and a failure is told.
            Err(e) => {
                if let Some(sure) = log.sure.take() {
                    remove_job_log(&log.path, sure);
                }
                Err(e)
            }
        }
    }

    /// Adds `records` to the log in one write, not synced. A kill keeps what was written, and a
    /// power cut that drops it leaves a log that tells less was done than was.
    pub(crate) fn add_records(&mut self, records: &[Record]) -> std::io::Result<()> {
        self.file.write_all(&records_bytes(records))
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
    pub(crate) fn remove_log(self) -> bool {
        (self.sure).is_some_and(|sure| remove_job_log(&self.path, sure))
    }

    /// Returns the log at `path`, held locked as `file`, as a start found it, of this format or
    /// of an older one. It is written to no more, and removed as a log of its job is.
    pub(crate) fn found(path: PathBuf, file: File) -> JobLog {
        JobLog {
            path,
            file,
            sure: None,
        }
    }

    /// Returns the log with the proof `sure` that the start read it surely, so that it may be
    /// removed once what it says has been written.
    pub(crate) fn read_surely(mut self, sure: Sure) -> JobLog {
        self.sure = Some(sure);
        self
    }

    /// Takes the proof that the log may be removed, which it has no more after this.
    pub(crate) fn take_sure(&mut self) -> Option<Sure> {
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

    /// Returns the log's path, its file and the proof that it may be removed, and `None` when it
    /// has no such proof, as a log found at the start that no reader read surely.
    pub(crate) fn into_removable(self) -> Option<(PathBuf, File, Sure)> {
        Some((self.path, self.file, self.sure?))
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
/// it, and a log made a moment ago is not locked yet.
pub(crate) fn logs_left_by_a_kill(dir: &Path) -> Vec<FoundLog> {
    let (mut found, mut left_alone) = (Vec::new(), 0);
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        // A pipe or a link planted in the state folder is no log.
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        let path = entry.path();
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
            folder = dir.display()
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
    let (Some(job), Some(started)) = (
        JOB_KINDS.iter().find(|kind| kind.as_bytes() == job),
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
            Record::Trashed {
                number: 1,
                top: None,
                info_name: p("c d.jpg.trashinfo"),
            },
            Record::Trashed {
                number: 0,
                top: Some(p("/media/stick")),
                info_name: p("a.jpg.trashinfo"),
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
    /// a `.trashinfo` name with a `/`. The records around it are kept.
    #[test]
    fn relative_paths_and_dot_dot_are_refused() {
        let good = log_bytes("1", &every_record()[..3]);
        for bad in [
            &b"shown\0c/a.gqv\0\0"[..],
            b"shown\0/c/../a.gqv\0\0",
            b"tree\0/photos/old\0../x\0\0",
            b"step\x001\0/photos/a.jpg\0photos/b.jpg\0-\0\0",
            b"trashed\x000\0home\0../a.trashinfo\0\0",
            b"trashed\x000\0media\0a.trashinfo\0\0",
            b"aside\x000\0/p/a.xmp\0/p/../q/.h\0\0",
            b"old-log\0../deletes/1-2\0\0",
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
    /// empty file older than a minute, whose header a kill cut off, is removed. A log a running
    /// window holds is passed over.
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
        let _ = crate::files::file_io::take_saves_failed();
        let found = super::logs_left_by_a_kill(&dir);
        assert_eq!(found.iter().map(|f| f.job).collect::<Vec<_>>(), ["trash"]);
        let told = crate::files::file_io::take_saves_failed();
        assert_eq!(told.len(), 1, "{told:?}");
        assert!(newer.exists() && unknown.exists() && !empty.exists());
        drop((found, held));
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
