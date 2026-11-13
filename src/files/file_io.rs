//! The few ways Glimt changes files on disk, each kept to one function that clippy.toml makes
//! the only way. A file is written whole or not at all (`replace_file`, `create_file`,
//! `write_atomic`), and a file the windows share is locked, read, changed and written as one
//! (`edit_shared`). A file is renamed never over another (`rename_never_over`), and moved or
//! copied whole or not at all (`put_file`). A rename that later steps count on is on disk with
//! its folder (`sync_folder_of`) before they run. Glimt's own files are written in one thread, in
//! order (`write_later`), and the collections in another (`write_later_in_user_folders`), so a
//! share that stopped answering does not hold the settings. `flush` waits for both before Glimt
//! ends.

use crate::support::threads::lock_even_if_poisoned;
use std::path::{Path, PathBuf};

/// The last save that failed, of a file of the user's or of Glimt's own, for the window to tell
/// as a failed write. Any thread sets it, through `save_failed`. Tests keep their own instead
/// (`ToldInTest`), because the tests share this one.
#[cfg(not(test))]
static SAVE_ERROR: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// The folder in /proc where each descriptor that Glimt holds open is a link to its file
/// (`path_through`).
const DESCRIPTORS: &str = "/proc/self/fd";

/// The warnings that an edit went on without its lock, and the folders of the locks told so,
/// from any thread (`lock_or_tell`). In tests each test has its own (`told_here`).
#[cfg(not(test))]
static LOCKS_TOLD: std::sync::Mutex<LocksTold> = std::sync::Mutex::new(LocksTold::new());

/// How long an act on files picked before a wait waits, once it starts, for each next
/// fingerprint of the files picked (`PickedFiles`). A file that a share has not answered about by
/// then counts as changed, and is left alone.
#[cfg(not(test))]
const FINGERPRINT_WAIT: std::time::Duration = std::time::Duration::from_secs(5);
#[cfg(test)]
const FINGERPRINT_WAIT: std::time::Duration = std::time::Duration::from_millis(200);

/// The modification time that some FUSE and MTP file systems give every file, as they do not
/// know it. A folder is not told apart by this time (`folder_time`).
const TIME_NOT_KNOWN: (i64, i64) = (0, 0);

/// In a test, a file picked whose fingerprint never comes, as on a share that stopped
/// answering, until the test takes it away.
#[cfg(test)]
pub(crate) static FINGERPRINT_STALLS: std::sync::Mutex<Option<PathBuf>> =
    std::sync::Mutex::new(None);

/// The thread that writes Glimt's own files (`write_later`), and the thread for the writes that
/// reach the user's folders (`write_later_in_user_folders`).
static OWN_FILES_WRITER: std::sync::OnceLock<std::sync::mpsc::Sender<QueuedWrite>> =
    std::sync::OnceLock::new();
static USER_FOLDERS_WRITER: std::sync::OnceLock<std::sync::mpsc::Sender<QueuedWrite>> =
    std::sync::OnceLock::new();

/// The names whose look fails in tests, as on a share that dropped for a moment (`look`). It is
/// shared by every thread, as the work that looks runs in threads of its own, and each test puts
/// only names in its own folder here.
#[cfg(test)]
pub(crate) static LOOKS_FAIL: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

/// The saves that replaced a file with other names (hard links) since the window last told of
/// them, from any thread.
#[cfg(not(test))]
static OTHER_NAMES: std::sync::Mutex<OtherNames> = std::sync::Mutex::new(OtherNames::new());

/// Everything being written for the user, counted by a `Working` held while it runs: the
/// settings and marks (`write_later`), sidecars, file jobs, a turned image, a print. The one
/// count closing waits for, so that no writer can be forgotten.
#[cfg(not(test))]
pub static WRITING: std::sync::LazyLock<std::sync::Arc<std::sync::atomic::AtomicUsize>> =
    std::sync::LazyLock::new(Default::default);

/// In tests, each test's thread has a count of its own, because the tests run side by side in
/// one process. A test that waited for its own writes waited for the other tests' writes too,
/// and it timed out under load. A write stays counted for the thread that asked for it, also
/// while the writer thread does it, because its `Working` holds that thread's count.
#[cfg(test)]
pub static WRITING: WritesOfThisThread = WritesOfThisThread;

// The folders that `sync_folder` synced on this thread, so that a test can see that a
// rename was made to last. A power cut cannot be simulated in a test.
#[cfg(test)]
thread_local! {
    pub(crate) static FOLDERS_SYNCED: std::cell::RefCell<Vec<PathBuf>> =
        const { std::cell::RefCell::new(Vec::new()) };
    // Makes `sync_folder_of` on this thread fail as a disk that fails would, for the tests of
    // what follows such a failure.
    pub(crate) static FOLDER_SYNC_FAILS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    // Makes `sync_folder_of` on this thread fail for this one folder only, so that a test can
    // let Glimt's own state folder sync while the folder of the renames fails.
    pub(crate) static FOLDER_SYNC_FAILS_IN: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
    // Takes ways to rename away from `rename_never_over` on this thread, as a file system
    // without them would: 1 takes RENAME_NOREPLACE, 2 takes hard links too.
    pub(crate) static RENAME_WAYS_MISSING: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
    // Makes a rename by `rename_never_over` from this name, or a delete for good of it, on this
    // thread be done and then fail with EIO (`answer_lost_in_a_test`).
    pub(crate) static ANSWER_LOST_AT: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
    // Makes a move by `put_file_until_stopped` on this thread a copy and an unlink, as a move
    // to another disk is, so that a test needs no second disk.
    pub(crate) static MOVES_CROSS_DEVICES: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    // How many names `look` looked at on this thread, so that a test can see that a check that
    // can tell nothing does not wait for the disk.
    pub(crate) static LOOKS_MADE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    // How many files `write_beside_without_folder_sync` synced on this thread, so that a test
    // can see that files written in a batch are put on disk together, not one by one.
    pub(crate) static FILES_SYNCED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    // How many times this thread parsed a mount table (`mounts_in`), so that a test can see
    // that a listing parses it once, not once for each folder.
    pub(crate) static MOUNT_TABLES_PARSED: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    // Makes `unlink_linked` on this thread fail for this name with this error number while the
    // name stays, as a share does that lost its answer.
    pub(crate) static UNLINK_FAILS: std::cell::RefCell<Option<(PathBuf, i32)>> =
        const { std::cell::RefCell::new(None) };
    // Makes `fingerprint` on this thread fail at this name once it has been looked at this many
    // times, as a share does that stopped answering for a moment.
    pub(crate) static LOOK_FAILS: std::cell::RefCell<Option<(PathBuf, usize)>> =
        const { std::cell::RefCell::new(None) };
    // The errors that `save_failed` told for this thread's test. They do not go to the window's
    // own slot in tests, where another test's window would take them as its notice. Each list is
    // leaked, so that the writer thread can tell a test's errors to that test's list.
    static SAVES_FAILED: &'static std::sync::Mutex<ToldInTest> = Box::leak(Box::default());
    // The list of the test whose work the writer thread is doing now, so that what that work
    // tells reaches the test that queued it. It is `None` on every other thread.
    static TOLD_FOR: std::cell::Cell<Option<&'static std::sync::Mutex<ToldInTest>>> =
        const { std::cell::Cell::new(None) };
    // The saves over a file with other names (hard links) on this thread. They do not go to
    // the window's own count in tests, where another test's window would take them as its
    // notice.
    pub(crate) static OTHER_NAMES_NOTED: std::cell::RefCell<OtherNames> =
        const { std::cell::RefCell::new(OtherNames::new()) };
}

/// The saves that replaced a file with other names (hard links): how many, and the name of the
/// first. A replace gives the name a new file, so the other names keep the old version.
pub(crate) struct OtherNames {
    count: usize,
    first: Option<String>,
}

/// The warnings that an edit went on without its lock, not yet shown, and the folders whose
/// lock has been told. Each folder is told once, so a share without locks warns once for its
/// folder, and a lock planted beside one collection does not keep the warning for another
/// folder, or for another kind of lock, from being told.
struct LocksTold {
    warnings: Vec<String>,
    folders: std::collections::BTreeSet<PathBuf>,
}

impl LocksTold {
    const fn new() -> Self {
        LocksTold {
            warnings: Vec::new(),
            folders: std::collections::BTreeSet::new(),
        }
    }

    /// Keeps `warning` to show and prints it, unless the lock's `folder` was told already.
    fn tell(&mut self, folder: PathBuf, warning: String) {
        if self.folders.insert(folder) {
            eprintln!("{warning}");
            self.warnings.push(warning);
        }
    }
}

impl Default for LocksTold {
    fn default() -> Self {
        Self::new()
    }
}

/// What a test's work told: the failed writes, and the edits that went on without their lock.
/// `taken_by_window` counts the failed writes that the test's window has taken already, as
/// `take_save_error` takes the window's own slot outside tests.
#[cfg(test)]
#[derive(Default)]
struct ToldInTest {
    saves_failed: Vec<String>,
    taken_by_window: usize,
    locks: LocksTold,
}

/// The count behind `WRITING` in tests: the count of the thread that reads it.
#[cfg(test)]
pub struct WritesOfThisThread;

#[cfg(test)]
impl std::ops::Deref for WritesOfThisThread {
    type Target = std::sync::Arc<std::sync::atomic::AtomicUsize>;

    /// Returns this thread's count. Each count is leaked, so that it lives as long as the
    /// `Working`s that hold it. That is one small allocation for each test.
    fn deref(&self) -> &Self::Target {
        thread_local! {
            static COUNT: &'static std::sync::Arc<std::sync::atomic::AtomicUsize> =
                Box::leak(Box::default());
        }
        COUNT.with(|count| *count)
    }
}

impl OtherNames {
    /// Returns a count of none.
    const fn new() -> Self {
        Self {
            count: 0,
            first: None,
        }
    }

    /// Counts a save that replaced the file at `path`.
    fn add(&mut self, path: &Path) {
        self.count += 1;
        let name = || {
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        };
        self.first.get_or_insert_with(name);
    }

    /// Returns the note for the window, naming the file, or how many there were, and counts
    /// anew. Returns `None` when no such save was counted.
    pub(crate) fn take_note(&mut self) -> Option<String> {
        let note = match (self.count, self.first.take()) {
            (1, Some(name)) => crate::tf!(
                "{name} has other names (hard links), which keep the old version",
                name
            ),
            (0, _) | (_, None) => return None,
            (n, _) => crate::tf!(
                "Other names (hard links) keep the old version: {files}",
                files = crate::support::format::count(n, "file")
            ),
        };
        self.count = 0;
        Some(note)
    }
}

/// A file's fingerprint: (device and inode, size, modification time in seconds and nanoseconds).
/// The inode stays the same through a rename on one disk. FAT, exFAT and FUSE without use_ino may
/// give the same file another inode later, so there the size and time tell the file instead. The
/// device and inode are `None` where the file system gives every file inode 0
/// (`save::file_id_from_metadata`), and then only the size and time tell the file.
pub(crate) type FileFingerprint = (Option<(u64, u64)>, u64, (i64, i64));

/// A file's identity as its folder listed it: the inode of its folder entry, which comes with its
/// name, so no file is looked at for it. It is `None` where it is not known: a name the list got
/// otherwise than from a listing, a file Glimt itself wrote anew since, or a file system that
/// does not keep a file's inode (`has_lasting_inodes`). An unknown identity is never taken for
/// another file's (`is_another_file`).
pub(crate) type Identity = Option<std::num::NonZeroU64>;

/// Whether a write found another file under a name than the one a window's list showed there
/// (`ListedFile::check`). Each window has its own (`Folder::list_is_old`), given to every file
/// it picks, so only the window whose list is old reads its folder again. A flag for the whole
/// process had one test's refusal make another test's window read its folder.
#[derive(Clone, Debug, Default)]
pub(crate) struct ListIsOld(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl ListIsOld {
    /// Tells the window that its list is old.
    fn raise(&self) {
        self.0.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Returns whether a write found that the list is old since this was last asked, and the
    /// folder must be read again.
    pub(crate) fn take(&self) -> bool {
        self.0.swap(false, std::sync::atomic::Ordering::Relaxed)
    }
}

/// A file the user picked in the list: its path, and the identity the list knew it by then.
/// Every write of such a file (its sidecar, its marks, a delete, a rename, a tool) takes one,
/// and checks with `check` right before it changes the file that no other file has taken the
/// name since. A path alone cannot be given, so a new write cannot leave the check out.
#[derive(Clone, Debug)]
pub(crate) struct ListedFile {
    path: PathBuf,
    identity: Identity,
    /// The flag of the window whose list showed the file, raised when `check` finds another
    /// file at its name.
    list_is_old: ListIsOld,
}

/// Two are the same file when their path and identity are, whichever window's flag they carry.
impl PartialEq for ListedFile {
    fn eq(&self, other: &Self) -> bool {
        (&self.path, self.identity) == (&other.path, other.identity)
    }
}

impl ListedFile {
    /// The file at `path`, which the list knew by `identity` (`App::listed_file`). A refusal
    /// raises `list_is_old`, the flag of the window whose list it is.
    pub(crate) fn new(path: PathBuf, identity: Identity, list_is_old: &ListIsOld) -> Self {
        ListedFile {
            path,
            identity,
            list_is_old: list_is_old.clone(),
        }
    }

    /// A file that was not picked in the list, so no identity tells it apart: a Lightroom
    /// catalog's, an Undo's (which has fingerprints of its own), a test's.
    pub(crate) fn not_from_the_list(path: PathBuf) -> Self {
        ListedFile {
            path,
            identity: None,
            list_is_old: ListIsOld::default(),
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn identity(&self) -> Identity {
        self.identity
    }

    /// The flag of the window whose list showed the file (`ListIsOld`).
    pub(crate) fn list_is_old(&self) -> &ListIsOld {
        &self.list_is_old
    }

    /// Refuses to change the file when another file has taken its name since the list showed
    /// it: another window's Rename Many, or another program, swapped or shifted the names. What
    /// the user meant for one photo (a keyword, stars, a delete) would else go to the other. The
    /// window whose list showed it is then told to read its folder again (`ListIsOld`). A file
    /// that is gone is left to the operation, which tells that itself. It waits for the disk, so
    /// it is called in the thread that makes the change, right before it.
    /// shortcut: a file deleted and another made under its name may get the freed inode and
    /// pass; the size and time of the listing would tell them apart, but cost a look at every
    /// file.
    pub(crate) fn check(&self) -> std::io::Result<()> {
        is_still_the_file(
            &self.path,
            KnownAs::listed(self.identity, &self.list_is_old),
        )
    }
}

/// What a file was known by, for `is_still_the_file`: the inode its folder listed, and its
/// fingerprint when it was picked before a wait or read for a save. Either may be unknown.
/// With the inode comes the flag of the window whose list it is (`ListIsOld`).
#[derive(Clone, Copy)]
pub(crate) struct KnownAs<'a> {
    fingerprint: Option<FileFingerprint>,
    /// How the fingerprint is compared with the file's fingerprint now.
    compared: Compared,
    listed: Identity,
    list_is_old: Option<&'a ListIsOld>,
}

impl<'a> KnownAs<'a> {
    /// A file known by the fingerprint it had when it was read or picked.
    pub(crate) fn fingerprint(fingerprint: FileFingerprint) -> KnownAs<'a> {
        KnownAs {
            fingerprint: Some(fingerprint),
            compared: Compared::Whole,
            listed: None,
            list_is_old: None,
        }
    }

    /// A file known by the size and time of the fingerprint it had before a reboot: a Rename
    /// Many's plan, read again at the next start. A share, a FUSE mount or a card reader may have
    /// come back with another device number, so the device is left out. The time is compared to
    /// the nanosecond, as a rename keeps it and a file system gives the same file the same time
    /// again after a reboot.
    pub(crate) fn size_and_time((_, size, time): FileFingerprint) -> KnownAs<'a> {
        KnownAs {
            fingerprint: Some((None, size, time)),
            compared: Compared::WithoutDevice,
            listed: None,
            list_is_old: None,
        }
    }

    /// A file known by the size and time of the fingerprint it had before a Move, which may have
    /// copied it to another disk: a Move's log, read again at the next start. The device is left
    /// out, as the copy is on another device. A Move to another disk sets the time of the copy,
    /// which that disk keeps only to its own resolution, so the times are compared as any file
    /// system keeps them (`same_time_as_kept`).
    pub(crate) fn size_and_time_as_kept((_, size, time): FileFingerprint) -> KnownAs<'a> {
        KnownAs {
            fingerprint: Some((None, size, time)),
            compared: Compared::TimeAsKept,
            listed: None,
            list_is_old: None,
        }
    }

    /// A file known only by the inode its folder listed, in the list whose flag is
    /// `list_is_old`.
    pub(crate) fn listed(identity: Identity, list_is_old: &'a ListIsOld) -> KnownAs<'a> {
        KnownAs {
            fingerprint: None,
            compared: Compared::Whole,
            listed: identity,
            list_is_old: Some(list_is_old),
        }
    }
}

/// How `is_still_the_file` compares a fingerprint with the file's fingerprint now.
#[derive(Clone, Copy)]
enum Compared {
    /// The device, size and time, each to the last digit (`same_fingerprint`).
    Whole,
    /// The size and time to the nanosecond, without the device (`KnownAs::size_and_time`).
    WithoutDevice,
    /// The size, and the time as any file system keeps it (`same_time_as_kept`), without the
    /// device (`KnownAs::size_and_time_as_kept`).
    TimeAsKept,
}

/// The identity that the list knew each picked file by, in the order of the files, and the
/// flag of that list's window (`ListIsOld`).
type Listed = (Vec<Identity>, ListIsOld);

/// Files the user picked before a wait: a confirmation, a prompt or the file chooser. Their
/// fingerprints are taken in a thread of their own while the user waits, as 500,000 files on a
/// share would hold the window. The act runs in a thread too, and it leaves alone each file that
/// is no longer the one picked (`PickedFingerprints::still_the_one_picked`), because another
/// program or window may have given its name to another photo meanwhile. A delete, trash, move
/// or rename of files picked earlier takes this type, never bare paths, so none can act by name
/// alone. The paths are kept once, and shared with the thread that takes their fingerprints, as
/// 500,000 of them take tens of megabytes.
pub(crate) struct PickedFiles {
    paths: std::sync::Arc<Vec<PathBuf>>,
    /// The inode that the folder listed for each file picked in the list, in the order of
    /// `paths`, which a file swapped before the question (and so before its fingerprint) is
    /// told by. It is empty for files not picked in the list.
    listed: Vec<Identity>,
    /// The flag of the window whose list showed the files (`ListIsOld`).
    list_is_old: ListIsOld,
    fingerprints: crate::support::threads::Receiving<Option<FileFingerprint>>,
    /// Whether the paths are folders, which are told apart by their inode or their time
    /// (`is_still_the_folder`), not by their size, which changes with every file put into them.
    are_folders: bool,
}

impl PickedFiles {
    /// Starts taking the fingerprints of the files `paths`, picked now, none of them from the
    /// list.
    pub(crate) fn new(paths: Vec<PathBuf>) -> PickedFiles {
        PickedFiles::taken_in_background(paths, Listed::default(), false)
    }

    /// Starts taking the fingerprints of the files `files`, picked now in the list, which keep
    /// the inodes the list knew them by, and the flag of that list's window.
    pub(crate) fn listed(files: Vec<ListedFile>) -> PickedFiles {
        let (paths, listed) = paths_and_identities(files);
        PickedFiles::taken_in_background(paths, listed, false)
    }

    /// The files `files`, picked now in the list, with the fingerprints `seen` that a search
    /// took when it found them (Find Duplicates). A file changed since, or one the search has
    /// no fingerprint of, is left alone, as it may no longer be what the search found.
    pub(crate) fn listed_as_seen(
        files: Vec<ListedFile>,
        seen: &std::collections::HashMap<PathBuf, FileFingerprint>,
    ) -> PickedFiles {
        let (paths, listed) = paths_and_identities(files);
        let seen = paths.iter().map(|path| seen.get(path).copied()).collect();
        PickedFiles::with_fingerprints(paths, listed, seen)
    }

    /// Starts taking the device and inode of the folder `dir`, picked now.
    pub(crate) fn folder(dir: PathBuf) -> PickedFiles {
        PickedFiles::taken_in_background(vec![dir], Listed::default(), true)
    }

    /// The files of `recorded`, with the fingerprints Glimt noted after its own operation on
    /// them. An Undo gives them, so that it leaves alone a file another program put at the name.
    pub(crate) fn recorded(recorded: Vec<(PathBuf, FileFingerprint)>) -> PickedFiles {
        let (paths, fingerprints): (Vec<PathBuf>, Vec<Option<FileFingerprint>>) = (recorded
            .into_iter())
        .map(|(path, f)| (path, Some(f)))
        .unzip();
        PickedFiles::with_fingerprints(paths, Listed::default(), fingerprints)
    }

    /// The files `paths`, with their identities `listed`, and the `fingerprints` known of them
    /// already, in their order.
    fn with_fingerprints(
        paths: Vec<PathBuf>,
        (listed, list_is_old): Listed,
        fingerprints: Vec<Option<FileFingerprint>>,
    ) -> PickedFiles {
        let (tx, rx) = std::sync::mpsc::channel();
        for fingerprint in fingerprints {
            let _ = tx.send(fingerprint);
        }
        PickedFiles {
            paths: std::sync::Arc::new(paths),
            listed,
            list_is_old,
            fingerprints: crate::support::threads::Receiving::of(rx),
            are_folders: false,
        }
    }

    /// Starts taking the fingerprints of `paths`, folders when `are_folders`, in their order, in
    /// a thread of its own, which shares the paths. `listed` are their identities in the list.
    /// It shows no busy pointer, as the user does not wait for it while a dialog is open, and a
    /// file on a share that stopped answering would keep it on for good.
    fn taken_in_background(
        paths: Vec<PathBuf>,
        (listed, list_is_old): Listed,
        are_folders: bool,
    ) -> PickedFiles {
        let (tx, rx) = std::sync::mpsc::channel();
        let paths = std::sync::Arc::new(paths);
        let taken = paths.clone();
        // No busy pointer: as said above, nobody waits for the fingerprints.
        std::thread::spawn(move || {
            for path in taken.iter() {
                // Once nobody waits for them, the rest are not taken.
                if tx.send(picked_fingerprint(path)).is_err() {
                    break;
                }
            }
        });
        PickedFiles {
            paths,
            listed,
            list_is_old,
            fingerprints: crate::support::threads::Receiving::of(rx),
            are_folders,
        }
    }

    /// The files picked.
    pub(crate) fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    /// Waits for the fingerprints, at most `FINGERPRINT_WAIT` for each next one, in the thread
    /// that acts on the files, never the window's. Returns the files picked and their
    /// fingerprints. A file whose fingerprint did not come in time has none, and counts as
    /// changed.
    pub(crate) fn wait_for_fingerprints(mut self) -> (Vec<PathBuf>, PickedFingerprints) {
        use crate::support::threads::Took;
        let mut taken = std::collections::HashMap::with_capacity(self.paths.len());
        for (k, path) in self.paths.iter().enumerate() {
            match self.fingerprints.wait_next(FINGERPRINT_WAIT) {
                Took::Answer(Some(fingerprint)) => {
                    let listed = self.listed.get(k).copied().flatten();
                    taken.insert(path.clone(), (fingerprint, listed));
                }
                Took::Answer(None) => {}
                Took::Nothing | Took::Gone => break,
            }
        }
        let fingerprints = PickedFingerprints {
            taken,
            list_is_old: self.list_is_old,
            are_folders: self.are_folders,
        };
        // The thread that took them has let go of the paths once it sent the last one, or it
        // is about to, and only then are they copied.
        (std::sync::Arc::unwrap_or_clone(self.paths), fingerprints)
    }

    /// In a test, waits until the fingerprints are taken, so that a change made after it is one
    /// made while the question was open.
    #[cfg(test)]
    pub(crate) fn wait_until_taken(&mut self) {
        use crate::support::threads::Took;
        let (tx, rx) = std::sync::mpsc::channel();
        for _ in self.paths.iter() {
            if let Took::Answer(fingerprint) = self.fingerprints.wait_next(FINGERPRINT_WAIT * 25) {
                let _ = tx.send(fingerprint);
            }
        }
        self.fingerprints = crate::support::threads::Receiving::of(rx);
    }
}

/// The fingerprints of files picked before a wait, as `PickedFiles::wait_for_fingerprints`
/// took them, each with the identity its list knew it by.
pub(crate) struct PickedFingerprints {
    taken: std::collections::HashMap<PathBuf, (FileFingerprint, Identity)>,
    list_is_old: ListIsOld,
    are_folders: bool,
}

impl PickedFingerprints {
    /// Returns the fingerprint that the file at `path` had when it was picked. Returns `None`
    /// when it was not taken in time, or nothing was there.
    pub(crate) fn picked_as(&self, path: &Path) -> Option<FileFingerprint> {
        self.taken.get(path).map(|(picked, _)| *picked)
    }

    /// Looks whether the file at `path` is still the one picked there. Returns the error to tell
    /// when it is not: another file has its name, it was written since, it is gone, or its
    /// fingerprint did not come in time.
    pub(crate) fn still_the_one_picked(&self, path: &Path) -> std::io::Result<()> {
        let (picked, listed) = *self.taken.get(path).ok_or_else(changed_meanwhile)?;
        if self.are_folders {
            let kind = file_system_type(path);
            let keeps_inodes = kind.is_some_and(|kind| has_lasting_inodes(&kind));
            return is_still_the_folder(path, picked, keeps_inodes, &self.list_is_old);
        }
        let known = KnownAs {
            fingerprint: Some(picked),
            compared: Compared::Whole,
            listed,
            list_is_old: Some(&self.list_is_old),
        };
        is_still_the_file(path, known)
    }
}

/// A write queued for a `start_ordered_worker` thread.
pub type QueuedWrite = Box<dyn FnOnce() + Send>;

/// A line of /proc/self/mountinfo: one mount.
struct Mount<'a> {
    id: &'a [u8],
    /// The id of the mount this one is on.
    parent: &'a [u8],
    /// The device's numbers, as major:minor.
    dev: &'a [u8],
    /// The folder of its file system that it shows: "/" for the whole file system, and a
    /// folder in it for a bind mount of that folder.
    root: &'a [u8],
    /// Where it is mounted, as a path.
    at: PathBuf,
    /// Its file system's type, such as ext4, vfat or fuse.rclone.
    kind: &'a [u8],
    /// Its file system's options, such as btrfs's subvol=/@data.
    options: &'a [u8],
}

/// The text of `unlink_linked`'s error when the old name may be gone and the file may be only
/// under its new one. The error number is not kept in an error made with a text of its own, so
/// this type is what tells `may_have_been_done` that the step may have been done.
#[derive(Debug)]
struct MayBeUnderBothNames(String);

impl std::fmt::Display for MayBeUnderBothNames {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for MayBeUnderBothNames {}

/// Returns whether nothing is at `path`: the system said that the name is not there (NotFound).
/// Any other failed look, such as a share that dropped for a moment, an error of the disk or no
/// permission, says nothing, so the name counts as there. A step that removes, renames over or
/// counts a file as deleted is then not taken on a look that failed. A link counts by its own
/// name, not by its target. This, `is_surely_there`, `is_surely_file`, `is_surely_dir` and
/// `is_link` are the one way Glimt asks whether a name is there, and what it is
/// (`source_rules::gone_means_not_found`).
pub(crate) fn is_surely_gone(path: &Path) -> bool {
    look(path, false).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
}

/// Returns whether something is surely at `path`: the look at its name answered. A failed look
/// counts as not there, so this is for a step that needs the name to be there before it is
/// taken, such as a sidecar written only beside its photo.
pub(crate) fn is_surely_there(path: &Path) -> bool {
    look(path, false).is_ok()
}

/// Returns whether the marks and collection entries of a file that a record says was moved to
/// `path` follow it there. Only a look that surely finds nothing at the name says no. A look
/// that fails says yes, because whoever made the record saw the file there, and it is told.
/// A pair left out on a failed look let the other pairs of a swap give the other photo's
/// marks to its name.
pub(crate) fn is_followed_to(path: &Path) -> bool {
    match look(path, false) {
        Ok(_) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => {
            save_failed(crate::tf!(
                "Could not look at {name}: {e}. Its marks and collection entries followed it \
                 there all the same, as the file was there before.",
                name = shown_path(path),
                e
            ));
            true
        }
    }
}

/// Returns whether `path` is surely a regular file, also through a link: the look answered that
/// it is one. A failed look counts as no file, so this is for a step that needs a file, such as
/// one read or run. A pipe would block a reader for good, and a device such as /dev/zero would
/// fill memory. A step that would take the name's place uses `is_surely_gone` instead.
pub(crate) fn is_surely_file(path: &Path) -> bool {
    look(path, true).is_ok_and(|meta| meta.is_file())
}

/// Returns whether `path` is surely a folder, also through a link: the look answered that it is
/// one. A failed look counts as no folder, so this is for a step that needs a folder, such as
/// one opened or read. A step that would leave a folder out uses `is_surely_gone` instead.
pub(crate) fn is_surely_dir(path: &Path) -> bool {
    look(path, true).is_ok_and(|meta| meta.is_dir())
}

/// Returns whether `path` is a link, by the look at its own name. A name that is surely not
/// there is no link. Any other failed look says nothing either way, and its error is returned,
/// so that the caller does not take the step it would take for a file that is no link, such as
/// a sidecar read or copied through a link to another file of the user's.
pub(crate) fn is_link(path: &Path) -> std::io::Result<bool> {
    match look(path, false) {
        Ok(meta) => Ok(meta.file_type().is_symlink()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// Looks at the name `path`, through a link when `follow`. In tests, a name in `LOOKS_FAIL`
/// fails with EIO, as a share that dropped for a moment does.
fn look(path: &Path, follow: bool) -> std::io::Result<std::fs::Metadata> {
    #[cfg(test)]
    LOOKS_MADE.set(LOOKS_MADE.get() + 1);
    #[cfg(test)]
    if lock_even_if_poisoned(&LOOKS_FAIL).iter().any(|p| p == path) {
        return Err(std::io::Error::from_raw_os_error(libc::EIO));
    }
    match follow {
        true => std::fs::metadata(path),
        false => std::fs::symlink_metadata(path),
    }
}

/// Rename `from` to `to`, never over a file or folder there, also one another window put there
/// a moment ago: in one step where the file system can (RENAME_NOREPLACE); where it cannot (FAT,
/// NTFS or exFAT through FUSE, some shares), a link, which never replaces a file, and then the
/// old name unlinked; where links cannot be made either, a rename after looking first. The one
/// way Glimt renames: a file moved, a copy or a new file given its name, a folder, a collection,
/// a file back from the trash. A rename that failed but was done all the same (`done_after_all`)
/// counts as done. `CrossesDevices` is the caller's to copy instead. The rename
/// is not synced to disk here. One rename alone is one step, so a power cut leaves the file
/// under its old name or its new one. A caller whose next step counts on the new name being on
/// disk (an unlink of the source, a plan removed) syncs the folder with `sync_folder_of` first.
#[allow(clippy::disallowed_methods)] // its last fallback, a rename after looking
pub(crate) fn rename_never_over(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::io::ErrorKind::{AlreadyExists, CrossesDevices, Unsupported};
    #[cfg(test)]
    let missing = RENAME_WAYS_MISSING.get();
    #[cfg(not(test))]
    let missing = 0;
    let one_step = match missing {
        0 => rename_noreplace(from, to),
        _ => Err(Unsupported.into()),
    };
    #[cfg(test)]
    let one_step = answer_lost_in_a_test(from, one_step);
    let link = || match missing {
        0 | 1 => std::fs::hard_link(from, to),
        _ => Err(Unsupported.into()),
    };
    let renamed = match one_step {
        Err(e) if e.kind() == Unsupported => match link() {
            Ok(()) => unlink_linked(from, to),
            Err(e) if matches!(e.kind(), AlreadyExists | CrossesDevices) => Err(e),
            // A name whose look failed counts as there, as does a link that points nowhere.
            Err(_) if !is_surely_gone(to) => Err(AlreadyExists.into()),
            // shortcut: a file that another program puts at `to` between the look and the
            // rename is written over. rclone, s3fs and MTP offer no way to rename without that
            // race; do more if one of them gains RENAME_NOREPLACE or hard links.
            Err(_) => std::fs::rename(from, to).map_err(|e| explain_rename_error(e, to)),
        },
        done => done,
    };
    done_after_all(renamed, from, Some(to))
}

/// Unlinks `from`, which is now also at `to`. A move is all or nothing, so a file that surely
/// was not unlinked (`unlink_was_refused`: a shared folder with the sticky bit, someone else's
/// file, a read-only disk) has its new name taken away again. Every other failure keeps both
/// names, also an error that Glimt does not know, because a share or a FUSE file system may
/// have done the unlink and lost its answer while its cache still says the old name is there.
/// Taking the new name away then would remove the only copy left. So here an unknown error
/// counts as done, the other way round from `may_have_been_done`, whose callers only undo.
/// The error then says that the file may be under both names or only the new one, and that both
/// are to be checked before either is removed. A file gone after all (deleted or moved in
/// another window, or a share that said no but did it) is done, as the new name may be the only
/// one left. This is the one place where a name is taken back after a failed unlink.
fn unlink_linked(from: &Path, to: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    let unlinked = match UNLINK_FAILS.with_borrow(|fails| fails.clone()) {
        Some((name, errno)) if name == from => Err(std::io::Error::from_raw_os_error(errno)),
        _ => unlink(from),
    };
    #[cfg(not(test))]
    let unlinked = unlink(from);
    let Err(e) = unlinked else {
        return Ok(());
    };
    if std::fs::symlink_metadata(from).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) {
        return Ok(());
    }
    if unlink_was_refused(&e) {
        #[allow(clippy::disallowed_methods)] // the new name, taken back
        let taken_back = std::fs::remove_file(to);
        // A new name that stays is told below, as the file is then under both names.
        if taken_back.is_ok() {
            return Err(e);
        }
    }
    let (from, to) = (shown_path(from), shown_path(to));
    let text = crate::tf!(
        "{from} could not be removed after it was put at {to}, so the file may be under both names or only at {to}; check both before removing either ({e})",
        from,
        to,
        e
    );
    Err(std::io::Error::new(e.kind(), MayBeUnderBothNames(text)))
}

/// Returns whether the unlink that gave the error `e` surely removed nothing, as the kernel
/// answers so before it looks any further: no right to it, a read-only disk, a busy name or a
/// folder, or a path it cannot follow. A name is taken back only after such an answer.
fn unlink_was_refused(e: &std::io::Error) -> bool {
    use libc::{EACCES, EBUSY, EINVAL, EISDIR, ELOOP, ENAMETOOLONG, ENOTDIR, EPERM, EROFS};
    matches!(
        e.raw_os_error(),
        Some(EPERM | EACCES | EROFS | EBUSY | EISDIR | ENAMETOOLONG | EINVAL | ENOTDIR | ELOOP)
    )
}

/// Returns whether the failed rename, link or unlink that gave the error `e` may have been done
/// all the same. It is for the callers whose answer to a refusal only undoes the steps before
/// it and removes no name, Rename Many and the trash by hand. `unlink_linked`, which removes a
/// name, asks `unlink_was_refused` instead. A share or a FUSE file system whose answer was lost
/// on the way (EIO, a timeout, a lost connection, an interrupted call) may have done the step.
/// Then no other name may be removed on the belief that this one is as it was. Every other
/// error is the file system's answer that it did nothing: a name too long, a full disk, a file
/// that is gone, no right to it. So is an error of Glimt's own without an error number, except
/// `unlink_linked`'s. A file that is gone (ENOENT) is the exception on a network or cloud mount,
/// where `from`, the name the step took the file from, is: NFS and SMB send a step again when its
/// answer is lost, and a rename done the first time answers ENOENT the second. A mount whose type
/// cannot be read counts as such a mount.
pub(crate) fn may_have_been_done(e: &std::io::Error, from: &Path) -> bool {
    may_have_been_done_on(e, || file_system_type(from))
}

/// Returns `may_have_been_done` for the error `e` of a step on a mount whose type `kind` gives.
/// `kind` is asked only for ENOENT, as it reads the mount table.
fn may_have_been_done_on(e: &std::io::Error, kind: impl FnOnce() -> Option<String>) -> bool {
    use libc::{
        EAGAIN, ECOMM, ECONNABORTED, ECONNREFUSED, ECONNRESET, EHOSTDOWN, EHOSTUNREACH, EINTR, EIO,
        ENETDOWN, ENETRESET, ENETUNREACH, ENOBUFS, ENOENT, ENOLINK, ENOMEM, ENOTCONN, EPROTO,
        EREMOTEIO, ESTALE, ETIMEDOUT,
    };
    if e.raw_os_error() == Some(ENOENT) {
        return kind().is_none_or(|kind| is_remote(&kind));
    }
    e.get_ref()
        .is_some_and(|inner| inner.is::<MayBeUnderBothNames>())
        || matches!(
            e.raw_os_error(),
            Some(
                EIO | ETIMEDOUT
                    | ENOTCONN
                    | ESTALE
                    | ECONNRESET
                    | ECONNABORTED
                    | ECONNREFUSED
                    | EHOSTDOWN
                    | EHOSTUNREACH
                    | ENETDOWN
                    | ENETUNREACH
                    | ENETRESET
                    | EREMOTEIO
                    | ECOMM
                    | EPROTO
                    | ENOLINK
                    | EINTR
                    | EAGAIN
                    | ENOMEM
                    | ENOBUFS
            )
        )
}

/// Returns `result`, of a step that moved the file at `from` to `to`, or removed it when `to` is
/// `None`, as done when the step failed in a way that may have been done (`may_have_been_done`)
/// and the names show that it was: `from` is surely gone, and `to` is surely there. A share
/// that did the step and lost its answer otherwise had the file counted as where it was, so its
/// sidecars and marks stayed at the old name for the next file of that name. A look that fails
/// leaves the error as it is.
pub(crate) fn done_after_all(
    result: std::io::Result<()>,
    from: &Path,
    to: Option<&Path>,
) -> std::io::Result<()> {
    match result {
        Err(e) if may_have_been_done(&e, from) && is_surely_gone(from) => match to {
            Some(to) if !is_surely_there(to) => Err(e),
            _ => Ok(()),
        },
        result => result,
    }
}

/// Returns `result` as a lost answer, EIO, when the step on `path` was done and `path` is the
/// name in `ANSWER_LOST_AT`, as a share does that did the step and lost its answer.
#[cfg(test)]
pub(crate) fn answer_lost_in_a_test(
    path: &Path,
    result: std::io::Result<()>,
) -> std::io::Result<()> {
    match result {
        Ok(()) if ANSWER_LOST_AT.with_borrow(|at| at.as_deref() == Some(path)) => {
            Err(std::io::Error::from_raw_os_error(libc::EIO))
        }
        result => result,
    }
}

/// Unlinks `path`, the old name of a file that `unlink_linked` has linked or copied.
fn unlink(path: &Path) -> std::io::Result<()> {
    #[allow(clippy::disallowed_methods)] // the old name, linked or copied at its new one
    std::fs::remove_file(path)
}

/// Syncs the folder that holds `path` to disk, as `sync_folder` does.
pub(crate) fn sync_folder_of(path: &Path) -> std::io::Result<()> {
    match path.parent() {
        Some(folder) if !folder.as_os_str().is_empty() => sync_folder(folder),
        _ => sync_folder(Path::new(".")),
    }
}

/// Syncs `folder` to disk, with the names in it. A rename is on disk only when the folder that
/// holds the name is, not when the file is. A power cut or a USB stick pulled out without
/// unmounting it may otherwise bring the old name back. A folder that cannot be opened (no read
/// bit) or a file system that cannot sync one (some shares) is passed over, as nothing more can
/// be done there. Returns an error only when the disk says that the names did not get there.
pub(crate) fn sync_folder(folder: &Path) -> std::io::Result<()> {
    sync_folder_or_its_file_system(folder, false)
}

/// Syncs the whole file system that holds `folder` to disk: every file written to it and every
/// name given in it, `folder`'s among them. A batch of files is so put on disk with one sync,
/// where a sync of each file would have the disk empty its cache once for each, which takes tens
/// of milliseconds on a USB stick. Returns an error as `sync_folder` does.
pub(crate) fn sync_file_system_of(folder: &Path) -> std::io::Result<()> {
    sync_folder_or_its_file_system(folder, true)
}

/// Syncs `folder` as `sync_folder` does, or its whole file system when `whole_file_system` is
/// true, as `sync_file_system_of` does.
fn sync_folder_or_its_file_system(folder: &Path, whole_file_system: bool) -> std::io::Result<()> {
    #[cfg(test)]
    FOLDERS_SYNCED.with_borrow_mut(|synced| synced.push(folder.to_owned()));
    #[cfg(test)]
    if FOLDER_SYNC_FAILS.get()
        || FOLDER_SYNC_FAILS_IN.with_borrow(|failing| failing.as_deref() == Some(folder))
    {
        return Err(std::io::Error::from_raw_os_error(libc::EIO));
    }
    #[allow(clippy::disallowed_methods)] // a folder, opened only to sync it
    let synced = std::fs::File::open(folder).and_then(|f| match whole_file_system {
        false => f.sync_all(),
        true => {
            use std::os::fd::AsRawFd;
            #[allow(unsafe_code)]
            // SAFETY: `f` keeps the descriptor open for the call, and syncfs only reads it.
            // The standard library has no syncfs.
            let r = unsafe { libc::syncfs(f.as_raw_fd()) };
            match r {
                0 => Ok(()),
                _ => Err(std::io::Error::last_os_error()),
            }
        }
    });
    match synced {
        Err(e)
            if matches!(
                e.raw_os_error(),
                Some(libc::EIO | libc::ENOSPC | libc::EDQUOT)
            ) =>
        {
            Err(e)
        }
        _ => Ok(()),
    }
}

/// Returns `e` told in plain words when it says that the file system cannot rename a file to
/// `to` as Glimt does, which some shares, cloud folders and phones cannot. The words name the
/// type of the mount (fuse.rclone, cifs …), and the system's own words follow in brackets. The
/// kind stays, so callers still match on it.
fn explain_rename_error(e: std::io::Error, to: &Path) -> std::io::Error {
    // EXDEV, ENOSYS and EOPNOTSUPP are what such file systems answer to a rename. EPERM is not
    // among them, because a folder with the sticky bit or a file made immutable gives it on any
    // file system.
    match e.raw_os_error() {
        Some(18 | 38 | 95) => {
            let kind = file_system_type(to.parent().unwrap_or(to)).unwrap_or_else(|| "?".into());
            std::io::Error::new(
                e.kind(),
                crate::tf!(
                    "the file system here ({kind}) does not allow this rename, as on some shares, cloud folders and phones ({e})",
                    kind,
                    e
                ),
            )
        }
        _ => e,
    }
}

/// Returns the type of the file system that `path` is on, as /proc/self/mountinfo names it:
/// vfat, cifs, nfs4, fuse.rclone, fuse.gvfsd-fuse and so on. Returns `None` when that cannot be
/// read. The path is taken as it is, not with its links resolved, as that would wait on a share
/// that does not answer. A disk through FUSE, such as exFAT through exfat-fuse or NTFS through
/// ntfs-3g, is only "fuseblk" there, so its type is the one udev found on the disk when udev
/// knows it (not in a container or a Flatpak without the devices).
pub(crate) fn file_system_type(path: &Path) -> Option<String> {
    use crate::images::decode::{read_limited, read_text};
    let mounts = read_limited("/proc/self/mountinfo").ok()?;
    file_system_type_in(&mounts, path, |dev| {
        let udev = read_text(format!("/run/udev/data/b{}", String::from_utf8_lossy(dev))).ok()?;
        file_system_type_in_udev(&udev)
    })
}

/// Returns the file system's type in `udev`, a device's file in /run/udev/data, as blkid
/// found it on the disk. Returns `None` when blkid found none.
fn file_system_type_in_udev(udev: &str) -> Option<String> {
    let kind = (udev.lines().filter_map(|l| l.split_once('=')))
        .find_map(|(key, value)| (key == "E:ID_FS_TYPE").then_some(value))?;
    (!kind.is_empty()).then(|| kind.to_owned())
}

/// Returns whether a mount of type `kind` (from `file_system_type`) is on another computer or in a
/// cloud, through FUSE, SMB or NFS. A fuseblk mount (NTFS or exFAT on a local disk) is not. Every
/// other FUSE mount counts, also a local one such as gocryptfs, so the delete dialog does not call
/// the folder a network folder.
pub(crate) fn is_remote(kind: &str) -> bool {
    kind.starts_with("fuse.") || matches!(kind, "cifs" | "smb3" | "nfs" | "nfs4" | "9p")
}

/// Returns the type of the mount in `mountinfo`, the bytes of /proc/self/mountinfo, that holds
/// `path`. It goes down from the top mount through the mounts on each one, as the kernel walks
/// a path. Of the mounts on one mount that start the path, the one with the shortest mount
/// point hides the others, because a longer one could only have been put there before it. Of
/// two at the same place, the later one is on top. The text is read as bytes, because a mount
/// point may have a name that is not UTF-8. `device_type` gives the type of a fuseblk mount's
/// disk from its device's numbers.
fn file_system_type_in(
    mountinfo: &[u8],
    path: &Path,
    device_type: impl Fn(&[u8]) -> Option<String>,
) -> Option<String> {
    file_system_type_among(&mounts_in(mountinfo), path, device_type)
}

/// Returns the mounts of `mountinfo`, the bytes of /proc/self/mountinfo, each line that is whole.
fn mounts_in(mountinfo: &[u8]) -> Vec<Mount<'_>> {
    #[cfg(test)]
    MOUNT_TABLES_PARSED.set(MOUNT_TABLES_PARSED.get() + 1);
    (mountinfo.split(|&b| b == b'\n'))
        .filter_map(mount_line)
        .collect()
}

/// Returns the type of the mount among `mounts` that holds `path`, as `file_system_type_in`
/// does, for a caller that looks up many paths in one table parsed once.
fn file_system_type_among(
    mounts: &[Mount],
    path: &Path,
    device_type: impl Fn(&[u8]) -> Option<String>,
) -> Option<String> {
    let found = mount_holding(mounts, path)?;
    (found.kind == b"fuseblk")
        .then(|| device_type(found.dev))
        .flatten()
        .or_else(|| Some(String::from_utf8_lossy(found.kind).into_owned()))
}

/// Returns the mount of `mounts` that holds `path`, as `file_system_type_in` finds it. A path
/// through a descriptor (`path_through`) is looked up by the opened folder's own path. Returns
/// `None` when no mount holds it, when a chroot leaves it unknown, or when the descriptor's link
/// cannot be read.
fn mount_holding<'a>(mounts: &'a [Mount<'a>], path: &Path) -> Option<&'a Mount<'a>> {
    // A path through a descriptor is on the opened folder's mount, not on /proc.
    let path = &*named_without_descriptor(path)?;
    let ids: std::collections::HashSet<&[u8]> = mounts.iter().map(|m| m.id).collect();
    // The top mounts are those whose parent is not in the table, or which are their own parent.
    let on = |m: &Mount, under: Option<&[u8]>| match under {
        Some(id) => m.parent == id && m.id != id,
        None => !ids.contains(m.parent) || m.parent == m.id,
    };
    let (mut under, mut found) = (None, None);
    // Each step goes one mount further down, so a table that loops ends too.
    for _ in 0..mounts.len() {
        // Backwards, so that of two at the same place the later one is the first found.
        let mut here = (mounts.iter().rev()).filter(|m| on(m, under) && path.starts_with(&m.at));
        let Some(next) = here.clone().min_by_key(|m| m.at.as_os_str().len()) else {
            break;
        };
        // A chroot shows several top mounts when they are on mounts outside it. Which of them a
        // path reaches depends on whether each was mounted before the chroot was made, which
        // the table does not tell, so the type is not known.
        if under.is_none() && here.any(|m| m.at != next.at) {
            return None;
        }
        under = Some(next.id);
        found = Some(next);
    }
    found
}

/// Returns whether the mount that holds `path` is one that the host may have made itself, so
/// that the host's gio finds the same top there: a whole file system, a whole btrfs subvolume,
/// or a folder mounted at a place of another name. Returns `false` for the mount that a Flatpak
/// makes of a host's folder at its own path, and also when /proc/self/mountinfo cannot be read
/// or does not tell. The path is taken as it is, as in `file_system_type`.
pub(crate) fn is_mounted_whole(path: &Path) -> bool {
    let mounts = crate::images::decode::read_limited("/proc/self/mountinfo").unwrap_or_default();
    is_mounted_whole_in(&mounts, path)
}

/// Returns whether the mount in `mountinfo`, the bytes of /proc/self/mountinfo, that holds
/// `path` is one that the host may have made itself (`is_mounted_whole`).
fn is_mounted_whole_in(mountinfo: &[u8], path: &Path) -> bool {
    let mounts = mounts_in(mountinfo);
    let Some(mount) = mount_holding(&mounts, path) else {
        return false;
    };
    let root = unescape_mount(mount.root);
    // The root of a btrfs mount starts with its subvolume, which the host mounted whole.
    let subvol = (mount.kind == b"btrfs")
        .then(|| (mount.options.split(|&b| b == b',')).find_map(|o| o.strip_prefix(b"subvol=")))
        .flatten()
        .map(unescape_mount);
    let folder = (subvol.as_deref()).and_then(|subvol| root.strip_prefix(subvol).ok());
    // A Flatpak shows a host's folder at the folder's own path, so the folder it shows always
    // ends with the name it is mounted at: /srv at /srv, /root/srv at /srv on a btrfs root
    // subvolume, /u at /home/u when /home is a disk of its own. A folder that the host mounted
    // at a place of the same name is refused too, and the window asks to delete for good.
    (folder.unwrap_or(&root).file_name()).is_none_or(|name| Some(name) != mount.at.file_name())
}

/// Returns a line of /proc/self/mountinfo as a `Mount`. Returns `None` for a line that is not
/// whole. The fields are the mount's id, its parent's id, the device, the folder of the file
/// system that is mounted, the mount point and the mount's options. Optional fields follow,
/// ended by a lone "-", and then the type, the source and the file system's options.
fn mount_line(line: &[u8]) -> Option<Mount<'_>> {
    let mut fields = line.split(|&b| b == b' ');
    let (id, parent, dev) = (fields.next()?, fields.next()?, fields.next()?);
    let (root, at) = (fields.next()?, unescape_mount(fields.next()?));
    let mut after = fields.skip_while(|f| *f != b"-").skip(1);
    let (kind, options) = (after.next()?, after.nth(1).unwrap_or_default());
    Some(Mount {
        id,
        parent,
        dev,
        root,
        at,
        kind,
        options,
    })
}

/// Returns the mount point `at` of /proc/self/mountinfo as a path. The kernel writes a space, a
/// tab, a line break and a backslash in it as a backslash and three octal digits.
fn unescape_mount(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    let (mut out, mut i) = (Vec::new(), 0);
    while i < bytes.len() {
        match bytes.get(i..i + 4) {
            Some([b'\\', digits @ ..]) if digits.iter().all(|d| (b'0'..=b'7').contains(d)) => {
                out.push(
                    digits
                        .iter()
                        .fold(0u8, |n, d| n.wrapping_mul(8) + (d - b'0')),
                );
                i += 4;
            }
            _ => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    PathBuf::from(std::ffi::OsString::from_vec(out))
}

/// Give `from` the name `to` in one step, never over a file there: renameat2 with
/// RENAME_NOREPLACE (Linux 3.15; glibc 2.28, the release's baseline). `Unsupported` where the
/// file system has no such flag (FAT, some shares, old kernels). Only for `rename_never_over`,
/// which falls back where it is not to be had.
fn rename_noreplace(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let (from, to) = (
        std::ffi::CString::new(from.as_os_str().as_bytes())?,
        std::ffi::CString::new(to.as_os_str().as_bytes())?,
    );
    #[allow(unsafe_code)]
    // SAFETY: two valid C strings for the call's duration; renameat2 reads them and touches
    // nothing else of ours.
    let r = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
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

/// Copy (`is_copy`) or move `from` to `to`, never over a file there, also one another window put
/// there a moment ago: a copy is written under a hidden name and then renamed, a move is a
/// rename, both with `rename_never_over`; a move to another file system is a copy and an unlink.
/// Only that move syncs the copy's folder, before its source is removed. A copy alone leaves
/// the source, so nothing is lost when its name does not reach the disk, and a folder sync for
/// each file would make a copy of many small files to a USB stick up to twice as slow.
pub(crate) fn put_file(from: &Path, to: &Path, is_copy: bool) -> std::io::Result<()> {
    put_file_until_stopped(from, to, is_copy, &|| false)
}

/// Takes back a `put_file` of `from` to `to` whose sidecar could not follow, so that the file
/// and its sidecar stay together. A move goes back to `from`, on disk with its folder, as the
/// window then counts the file as never moved. A copy is removed, but only while `from` is
/// surely there, so that the copy is never the only one left.
pub(crate) fn put_file_back(from: &Path, to: &Path, was_copy: bool) -> std::io::Result<()> {
    if !was_copy {
        return put_file(to, from, false).and_then(|()| sync_folder_of(from));
    }
    if !is_surely_there(from) {
        return Err(std::io::ErrorKind::NotFound.into());
    }
    #[allow(clippy::disallowed_methods)] // a copy just made, whose original is still there
    std::fs::remove_file(to)
}

/// Copies or moves `from` to `to` as `put_file` does, and stops inside a large file when
/// `is_stopped` says so. A copy, also that of a move to another disk, is made in parts of a few
/// megabytes, and `is_stopped` is asked before each. A stopped copy leaves no part of the file
/// at `to`, and a stopped move leaves the file where it was. Returns `Interrupted` when stopped.
pub(crate) fn put_file_until_stopped(
    from: &Path,
    to: &Path,
    is_copy: bool,
    is_stopped: &dyn Fn() -> bool,
) -> std::io::Result<()> {
    use std::io::ErrorKind::{AlreadyExists, CrossesDevices};
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    // Returns the fingerprint of the file at `from` as it was when it was opened to be copied.
    let copy = || -> std::io::Result<FileFingerprint> {
        if !is_surely_gone(to) {
            return Err(AlreadyExists.into());
        }
        // Written under a hidden name and given its own when whole: a copy cut short (a full
        // disk, Glimt closed, a stick pulled out) never looks like the file.
        // shortcut: a third close (which waits for nothing), a kill, a power cut or a stick
        // pulled out leaves the part under this hidden name, and nothing removes it later; a
        // sweep of .glimt-tmp-* at the next start if those are ever seen in numbers.
        let part = tmp_path(to);
        // Private while written: a private photo copied into a shared folder is not readable
        // there until it gets its own permissions below.
        #[allow(clippy::disallowed_methods)] // a copy's hidden part, made new
        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&part)?;
        // Its time kept, as mv does (sorting by date), and on disk before it takes its name.
        let copied = crate::images::decode::open_regular(from)
            .and_then(|mut f| {
                // The mode and time are those of the file opened, not of whatever has the name
                // after the copy. The name is looked at just after the open, so that a move
                // can tell before it unlinks the name that it still holds the file copied.
                let meta = f.metadata()?;
                let at_open = fingerprint_or_error(from)?;
                copy_until_stopped(&mut f, &mut out, is_stopped)?;
                Ok((meta, at_open))
            })
            .and_then(|(meta, at_open)| {
                // Its read, write and run bits, as cp gives them: not set-user-ID, set-group-ID
                // or sticky, which a copy owned by this user would carry for its owner.
                let mode = std::fs::Permissions::from_mode(meta.mode() & 0o777);
                // Through the open file, so that a link put at the part's name meanwhile is
                // not followed to a file of the user's.
                out.set_permissions(mode)?;
                #[allow(clippy::disallowed_methods)] // the copy's time, as mv keeps it
                out.set_modified(meta.modified()?)?;
                // A copy leaves the ACL, as cp does without -p, and a move keeps it, as mv does.
                copy_extended_attributes(from, &out, !is_copy);
                out.sync_all().map(|()| at_open)
            })
            .and_then(|at_open| rename_never_over(&part, to).map(|()| at_open));
        #[allow(clippy::disallowed_methods)] // the copy's hidden part
        // Not told: a copy that failed is told by its own error, and a copy that took its name
        // has no part left to remove.
        let _ = std::fs::remove_file(&part);
        copied
    };
    if is_copy {
        return copy().map(|_| ());
    }
    #[cfg(test)]
    let moved = match MOVES_CROSS_DEVICES.get() {
        true => Err(CrossesDevices.into()),
        false => rename_never_over(from, to),
    };
    #[cfg(not(test))]
    let moved = rename_never_over(from, to);
    match moved {
        // The copy is on disk with its name before the source goes: a stick pulled out right
        // after must not lose both.
        Err(e) if e.kind() == CrossesDevices => (copy())
            .and_then(|at_open| sync_folder_of(to).map(|()| at_open))
            .and_then(|at_open| still_the_file_copied(from, to, at_open))
            .and_then(|()| unlink_linked(from, to)),
        done => done,
    }
}

/// Looks, right before a move to another disk unlinks `from`, whether `from` still holds the
/// file that was copied to `to`, whose fingerprint was `at_open` when it was opened. A copy
/// takes minutes for a video, and a sync client or an editor may save a new version at the name
/// meanwhile. Unlinking the name then would lose that version, so both names are kept, and the
/// error says so. A look that fails keeps both names too.
fn still_the_file_copied(from: &Path, to: &Path, at_open: FileFingerprint) -> std::io::Result<()> {
    let Err(e) = is_still_the_file(from, KnownAs::fingerprint(at_open)) else {
        return Ok(());
    };
    let (from, to) = (shown_path(from), shown_path(to));
    Err(std::io::Error::other(crate::tf!(
        "{from} was left where it was after it was copied to {to}, so the file is under both names: {e}",
        from,
        to,
        e
    )))
}

/// Copies all of `from` to `to` in parts, and stops before the next part when `is_stopped`
/// says so. Each part is an `io::copy`, which the kernel does without passing the bytes
/// through Glimt where it can. Returns `Interrupted` when stopped.
fn copy_until_stopped(
    from: &mut std::fs::File,
    to: &mut std::fs::File,
    is_stopped: &dyn Fn() -> bool,
) -> std::io::Result<()> {
    use std::io::Read;
    // Small in the tests, so that a stop inside a file needs no file of many megabytes.
    const PART: u64 = if cfg!(test) { 4 << 10 } else { 8 << 20 };
    loop {
        if is_stopped() {
            return Err(std::io::ErrorKind::Interrupted.into());
        }
        if std::io::copy(&mut from.by_ref().take(PART), to)? == 0 {
            return Ok(());
        }
    }
}

/// A file the windows share, changed as it is now: under its lock, read whole (one not there
/// yet is empty), `change` gives its new contents and what the caller keeps, and it is written
/// whole or not at all. One that is there but cannot be read is not written over, as all it
/// holds would be lost: the error says why. The one way the settings, the marks, the keyword
/// tree, the sort manager's folders and the recent list are written again, so another window's
/// change made meanwhile stays; clippy refuses `file_lock` elsewhere (collections keep their
/// own `collections::edit` and `rename`, which must not make one that went).
pub fn edit_shared<T>(
    path: &Path,
    change: impl FnOnce(&[u8]) -> (Vec<u8>, T),
) -> std::io::Result<T> {
    // The folder first: without it there is no lock file, and the first write had no lock.
    // Not told: without the folder the write below fails, and that failure is returned.
    let _ = std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")));
    #[allow(clippy::disallowed_methods)] // the one place, with collections::edit and rename
    let _held = file_lock(path);
    let old = read_or_none(path)?.unwrap_or_default();
    let (new, keep) = change(&old);
    // A change that changes nothing is not written, so the file keeps its stamp and the other
    // windows do not read it again.
    if new != old {
        replace_file(path, new)?;
    }
    Ok(keep)
}

/// A file the windows share removed under its lock, as `edit_shared` changes one: another
/// window's change cannot land after it and bring back what was cleared. A failure is told,
/// because the list cleared would come back at the next start.
pub fn remove_shared(path: &Path) {
    if !is_surely_there(path) {
        return;
    }
    #[allow(clippy::disallowed_methods)] // as edit_shared
    let _held = file_lock(path);
    remove_own_or_tell(path);
}

/// Removes `path`, a file or folder of Glimt's own: a Rename Many's plan, a Move's log, a state
/// file, or the copy of one in a backup. A name that is gone already counts as removed. Its
/// result is never dropped (`source_rules::write_results_are_not_dropped`), because a plan or a
/// log left behind is acted on again at the next start.
pub fn remove_own(path: &Path) -> std::io::Result<()> {
    #[allow(clippy::disallowed_methods)] // the one way Glimt's own files are removed
    let removed = match std::fs::symlink_metadata(path) {
        // Glimt's own: a folder, such as the copy of a state folder in a backup.
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(path),
        // Glimt's own: a plan, a log or a state file.
        _ => std::fs::remove_file(path),
    };
    match removed {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        removed => removed,
    }
}

/// `remove_own`, with a failure told through `save_failed`. Returns whether `path` is gone.
pub fn remove_own_or_tell(path: &Path) -> bool {
    let Err(e) = remove_own(path) else {
        return true;
    };
    save_failed(crate::tf!(
        "Could not remove {name}: {e}",
        name = shown_path(path),
        e
    ));
    false
}

/// A file Glimt is about to write again, as it is: `None` when there is none yet, and an error
/// when there is one that cannot be read (no permission, a folder), which the caller must not
/// write over, as everything it holds would be lost. The one rule for the settings, the marks,
/// the keyword tree, the sort manager's folders and a sidecar.
pub fn read_or_none(path: &Path) -> std::io::Result<Option<Vec<u8>>> {
    match crate::images::decode::read_limited(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Takes the last error that `save_failed` told. Returns `None` when no save failed since the
/// last call.
pub fn take_save_error() -> Option<String> {
    #[cfg(not(test))]
    {
        lock_even_if_poisoned(&SAVE_ERROR).take()
    }
    // In tests the window takes the last error told for its own test since its last call, as
    // it takes the one slot outside tests. Before, it took none in tests, so no test saw the
    // way from `save_failed` to a failed write that the window counts.
    #[cfg(test)]
    {
        let mut told = lock_even_if_poisoned(told_here());
        let last = (told.saves_failed.len() > told.taken_by_window)
            .then(|| told.saves_failed.last().cloned())
            .flatten();
        told.taken_by_window = told.saves_failed.len();
        last
    }
}

/// Takes the warnings that edits went on without their lock, one line for each. They are not
/// failed writes, as each edit was made. Returns `None` when there is no new one.
pub fn take_lock_warning() -> Option<String> {
    #[cfg(not(test))]
    let warnings = std::mem::take(&mut lock_even_if_poisoned(&LOCKS_TOLD).warnings);
    #[cfg(test)]
    let warnings = std::mem::take(&mut lock_even_if_poisoned(told_here()).locks.warnings);
    (!warnings.is_empty()).then(|| warnings.join("\n"))
}

/// Takes the note of the saves over files with other names (hard links), once no file is being
/// written any more. A job over many files then gives one note with their count, rather than a
/// new one for each file every frame. Returns `None` while files are written, or when there is
/// nothing to note.
pub fn take_other_names_note() -> Option<String> {
    let is_writing = WRITING.load(std::sync::atomic::Ordering::Relaxed) > 0;
    (!is_writing).then(other_names_note).flatten()
}

/// Returns the note of the saves over files with other names (hard links), and counts anew.
/// Returns `None` in tests, which read their own thread's count.
fn other_names_note() -> Option<String> {
    #[cfg(not(test))]
    return lock_even_if_poisoned(&OTHER_NAMES).take_note();
    #[cfg(test)]
    None
}

/// `work` in the one thread that writes Glimt's own files (the settings, marks, Open Recent), in
/// the order given.
pub fn write_later(work: impl FnOnce() + Send + 'static) {
    queue_write(&OWN_FILES_WRITER, work);
}

/// `work` in the one thread for the writes that read or write the user's folders, in the order
/// given: an edit of a collection, which looks up the folders of its entries. A share that
/// stopped answering then holds only these writes, and not the settings and marks behind them.
pub fn write_later_in_user_folders(work: impl FnOnce() + Send + 'static) {
    queue_write(&USER_FOLDERS_WRITER, work);
}

/// Sends `work` to the thread of `writer`, counted in `WRITING` until it is done.
fn queue_write(
    writer: &'static std::sync::OnceLock<std::sync::mpsc::Sender<QueuedWrite>>,
    work: impl FnOnce() + Send + 'static,
) {
    // Dropped with the work, also when it panics or is never run: Glimt would wait for it at exit.
    let writing = crate::support::threads::Working::new(&WRITING);
    #[cfg(test)]
    let told = told_here();
    let counted = move || {
        #[cfg(test)]
        TOLD_FOR.set(Some(told));
        // The count goes down after a panic is told, so that `flush` does not end before that.
        let _writing = writing;
        run_and_tell_a_panic(work);
    };
    let _ = writer
        .get_or_init(start_ordered_worker)
        .send(Box::new(counted));
}

/// A thread that does the work sent to it one after another, in that order. One that panics
/// does not end it, or every later work sent would be lost.
pub fn start_ordered_worker() -> std::sync::mpsc::Sender<QueuedWrite> {
    let (works, rx) = std::sync::mpsc::channel::<QueuedWrite>();
    // No busy pointer: the writer, which lasts as long as Glimt. Each write it
    // runs counts as writing (`WRITING`) from when it is sent.
    std::thread::spawn(move || {
        for work in rx {
            run_and_tell_a_panic(work);
            #[cfg(test)]
            TOLD_FOR.set(None);
        }
    });
    works
}

/// Runs a queued write, and tells the user when it panics. Its change is then lost like that of
/// any failed write.
fn run_and_tell_a_panic(work: impl FnOnce()) {
    if crate::support::threads::catch_panic(work).is_none() {
        save_failed(
            crate::support::lang::tr(
                "A change could not be saved, because Glimt met an error while writing it",
            )
            .to_owned(),
        );
    }
}

/// Wait for everything being written, a few seconds at most, before Glimt ends: the last
/// change of a setting or a keyword must not be lost with its thread.
pub fn flush() {
    flush_within(std::time::Duration::from_secs(5));
}

/// Waits for everything being written, for `limit` at most. A write still running then is
/// told on the terminal, as the window is gone. A Rename Many cut short so keeps its plan on
/// disk, because Glimt ends without dropping it, and the next start puts it right.
fn flush_within(limit: std::time::Duration) {
    let until = std::time::Instant::now() + limit;
    let is_writing = || WRITING.load(std::sync::atomic::Ordering::Relaxed) > 0;
    while is_writing() && std::time::Instant::now() < until {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    if is_writing() {
        save_failed(
            crate::support::lang::tr(
                "Glimt ended before every write was done: the last change of the settings, marks, a collection or a file may be lost",
            )
            .to_owned(),
        );
    }
}

/// Waits until the writes queued before this call in both writer threads are done, five
/// seconds at most. Unlike `flush`, it does not also wait for the writes that other work
/// queues after it.
pub fn wait_for_queued_writes() {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let (done, wait) = std::sync::mpsc::channel();
    let done_too = done.clone();
    write_later(move || {
        let _ = done.send(());
    });
    write_later_in_user_folders(move || {
        let _ = done_too.send(());
    });
    for _ in 0..2 {
        let _ = wait.recv_timeout(until.saturating_duration_since(std::time::Instant::now()));
    }
}

/// The lock beside `path`, held while it lives: another Glimt window's reading and writing of
/// the same file does not fall between ours, which would lose its change. The lock is a file of
/// its own, hidden beside it, as `path` is renamed over. Returns `None`, and the caller goes on
/// without the lock, when the lock file cannot be made or is not a regular file.
pub fn file_lock(path: &Path) -> Option<std::fs::File> {
    lock_or_tell(&lock_beside(path)?)
}

/// Returns the name of the lock file of `path` (`file_lock`), `.NAME.lock` beside it. Every
/// window of every version takes the same one, so the names are pinned in src/frozen-1.0.txt.
pub(crate) fn lock_beside(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_string_lossy();
    Some(path.with_file_name(format!(".{name}.lock")))
}

/// `open_and_lock` for an edit that goes on without the lock when it cannot be had, as on a
/// share without file locks (NFS without lockd). The user is told so, once for each folder of
/// locks, because a change that another window makes at the same moment may then be lost. It
/// is a warning, not a failed write, because the edit itself is made: a `--wait` answers done,
/// and a close that waited for the edit goes on. Returns `None` when the lock cannot be had.
pub fn lock_or_tell(path: &Path) -> Option<std::fs::File> {
    let e = match open_and_lock(path) {
        Ok(lock) => return Some(lock),
        Err(e) => e,
    };
    let folder = path.parent().unwrap_or(Path::new("")).to_path_buf();
    tell_unlocked(folder, &shown_path(path), e);
    None
}

/// Tells once for the folder of locks `folder` that an edit goes on without its lock, the lock
/// `name`, which could not be had for the reason `e`.
pub fn tell_unlocked(folder: PathBuf, name: &str, e: impl std::fmt::Display) {
    let warning = crate::tf!(
        "Could not lock {name}: {e}. A change that another Glimt window makes at the same moment may be lost.",
        name,
        e
    );
    #[cfg(not(test))]
    lock_even_if_poisoned(&LOCKS_TOLD).tell(folder, warning);
    #[cfg(test)]
    lock_even_if_poisoned(told_here())
        .locks
        .tell(folder, warning);
}

/// Opens the lock file `path`, making it when it is not there, and waits for its lock, which
/// is held while the file stays open. A collection may come with a download, and so may a lock
/// file beside it, and a link or a pipe may be planted in Glimt's own folders. So the lock file
/// is opened as `open_to_lock` says: never truncated, never through a link and never waited on.
/// This and `open_if_unlocked` are the only ways Glimt locks a file
/// (`source_rules::files_are_locked_in_one_place`).
pub fn open_and_lock(path: &Path) -> std::io::Result<std::fs::File> {
    let lock = open_to_lock(path, std::fs::OpenOptions::new().create(true))?;
    lock.lock()?;
    Ok(lock)
}

/// Opens `path` as `open_to_lock` says, and takes its lock if no other window holds it. A plan
/// of Rename Many and a log of Move are each locked as the file itself this way. With `is_new`
/// the file is made new, and the open fails when the name is there. A file made new that cannot
/// be locked is removed again. Returns the file, whose lock is held while it stays open, or the
/// error `WouldBlock` when another window holds the lock. The lock is tried for a moment,
/// because a program that another thread starts holds a copy of the descriptor, and its lock,
/// until it runs.
pub fn open_if_unlocked(path: &Path, is_new: bool) -> std::io::Result<std::fs::File> {
    let file = open_to_lock(path, std::fs::OpenOptions::new().create_new(is_new))?;
    let mut tries = 1;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) if tries < 10 => tries += 1,
            Err(e) => {
                let e = std::io::Error::from(e);
                if is_new && let Err(left) = remove_own(path) {
                    return Err(std::io::Error::other(format!("{e}, and {left}")));
                }
                return Err(e);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// Opens `path` with `options` to be locked, for reading and for writing. The Linux NFS client
/// takes a flock as a lock of the whole file, and it gives that only to a file open for
/// writing. Opened to be read only, every plan and log on an NFS home looked held by another
/// window, so none was put right after a kill. The file is never truncated and never opened
/// through a link, so a planted link neither empties nor makes the file it points to. It is
/// opened without waiting, so that a planted pipe does not hold the thread for good, and what
/// was opened must be a regular file.
fn open_to_lock(path: &Path, options: &mut std::fs::OpenOptions) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    #[allow(clippy::disallowed_methods)] // a file to lock, never truncated and never a link
    let file = (options.read(true).append(true))
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other(format!(
            "{}: not a regular file",
            shown_path(path)
        )));
    }
    Ok(file)
}

/// Opens the folder `dir` without following a link at its name. What is then checked through
/// the folder opened (its owner, its mode) is the folder that is used, also when another user
/// swaps the name for a link or another folder after the check. This is the one way Glimt opens
/// a folder in a place that other users can write, to check it and then work in it.
pub(crate) fn open_folder_not_through_link(dir: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    #[allow(clippy::disallowed_methods)] // a folder, never through a link
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
        .open(dir)
}

/// Returns a path that reaches the folder `opened` itself, whatever its name is now: the
/// kernel's link to the open descriptor in /proc. Every step taken through this path (a folder
/// made in it, a file written or renamed into it) acts in the folder that was opened and
/// checked, so it never needs `openat` and its kin. The path holds while `opened` stays open.
/// Its name is Glimt's own and means nothing to the user, and the mount table knows it as a
/// path on /proc. So a question about the file system it is on, and a text that shows it, take
/// it through `named_without_descriptor`.
pub(crate) fn path_through(opened: &std::fs::File) -> PathBuf {
    use std::os::fd::AsRawFd;
    Path::new(DESCRIPTORS).join(opened.as_raw_fd().to_string())
}

/// Returns `path` by the name that the user and the mount table know it by. A path that
/// `path_through` made, `/proc/self/fd/N/…`, gets the opened folder's own path in place of the
/// descriptor's link, and any other path is given back as it is. The link is read from /proc,
/// which reads no disk. Returns `None` when the descriptor's link cannot be read.
fn named_without_descriptor(path: &Path) -> Option<std::borrow::Cow<'_, Path>> {
    let Ok(rest) = path.strip_prefix(DESCRIPTORS) else {
        return Some(std::borrow::Cow::Borrowed(path));
    };
    let mut parts = rest.components();
    let descriptor = parts.next()?;
    let folder = std::fs::read_link(Path::new(DESCRIPTORS).join(descriptor)).ok()?;
    Some(std::borrow::Cow::Owned(folder.join(parts.as_path())))
}

/// Returns `path` as a text for the user, by its own name also when it was reached through a
/// descriptor (`named_without_descriptor`). It is the one way file_io and the trash show a path,
/// as both work in folders through `path_through`
/// (`source_rules::paths_through_a_descriptor_are_shown_by_their_names`).
pub(crate) fn shown_path(path: &Path) -> String {
    let named = named_without_descriptor(path);
    named.as_deref().unwrap_or(path).display().to_string()
}

/// Write to a temporary file and rename it, so the file is never half-written after a crash.
/// This Glimt's name is in it (in a Flatpak more than the process ID, which windows can share),
/// so concurrent sessions never write to the same temporary file.
pub fn replace_file(path: &Path, bytes: impl AsRef<[u8]>) -> std::io::Result<()> {
    replace(path, bytes, None, || Ok(()))
}

/// `replace_file` for a file of the user's that must still be there: a turned image, whose
/// save takes a while. Deleted, moved or renamed meanwhile, it is not made again under its old
/// name by the save landing late; the save is refused instead. The one place this holds for
/// every operation, whether or not it asked `are_free_to_change` first. The step `before` runs
/// after the file is found writable and still there and the new version is written whole, just
/// before the rename. The image's sidecar is changed there, so a write that fails (a read-only
/// file, a full disk) leaves the file and its sidecar as they were. `read` is the fingerprint of
/// the file that `bytes` were made from, taken before it was read (`fingerprint_before_reading`).
/// A file that changed since (a copy still being written into it, another file given its name)
/// is not written over.
pub fn replace_existing(
    path: &Path,
    bytes: impl AsRef<[u8]>,
    read: FileFingerprint,
    before: impl FnOnce() -> std::io::Result<()>,
) -> std::io::Result<()> {
    replace(path, bytes, Some(read), before)
}

/// Replaces the file named `path` with `bytes`, written whole beside it first. Unlike
/// `replace_file`, a link at `path` is itself replaced, and the file it points to is left alone.
/// A downloaded folder may hold a link to any file of the user's under a photo's name, so a copy
/// saved over a file there (Save As) and a sidecar are written this way.
pub fn replace_name(path: &Path, bytes: impl AsRef<[u8]>) -> std::io::Result<()> {
    use std::io::Write;
    replace_name_with(path, |f| f.write_all(bytes.as_ref()), || Ok(()))
}

/// Replaces the file named `path` as `replace_name` does, with the contents written by `write`
/// (a PDF streamed page by page). The step `before` runs after the new file is written whole and
/// before it takes the name. Save As removes the replaced file's sidecar there, so a write that
/// fails (a full disk) leaves the file and its sidecar as they were, and a sidecar that cannot be
/// removed leaves the file.
pub fn replace_name_with(
    path: &Path,
    write: impl FnOnce(&mut std::fs::File) -> std::io::Result<()>,
    before: impl FnOnce() -> std::io::Result<()>,
) -> std::io::Result<()> {
    let has_other_names = check_writable(path)?;
    #[allow(clippy::disallowed_methods)] // replaces on purpose, the name and never a link's target
    let give_name = |tmp: &Path| {
        before().and_then(|_| std::fs::rename(tmp, path).map_err(|e| explain_rename_error(e, path)))
    };
    write_beside(&tmp_path(path), path, write, give_name)?;
    tell_other_names(path, has_other_names);
    Ok(())
}

/// Writes the file named `path`, which the user picked in the file chooser, with the contents
/// written by `write`, as `replace_name_with` does. `at_pick` is the fingerprint of the file that
/// was at the name when it was picked. Only that file is replaced, and when nothing was there, a
/// file put at the name since is never written over.
pub fn write_picked_name(
    path: &Path,
    at_pick: Option<FileFingerprint>,
    write: impl FnOnce(&mut std::fs::File) -> std::io::Result<()>,
) -> std::io::Result<()> {
    match at_pick {
        Some(picked) => {
            replace_name_with(path, write, || {
                is_still_the_file(path, KnownAs::fingerprint(picked))
            })?;
            crate::files::journal::note_written_anew(path, picked);
            Ok(())
        }
        None => write_beside(&tmp_path(path), path, write, |tmp| {
            rename_never_over(tmp, path)
        }),
    }
}

/// Writes `bytes` to `path` beside it and renames it, through a link to the file it points to.
/// Glimt's own files and a turned image are written so. A name the user chose for a new file,
/// and a collection's file, go through `replace_name`, which never follows a link.
fn replace(
    path: &Path,
    bytes: impl AsRef<[u8]>,
    existing: Option<FileFingerprint>,
    before: impl FnOnce() -> std::io::Result<()>,
) -> std::io::Result<()> {
    use std::io::Write;
    let write = |f: &mut std::fs::File| f.write_all(bytes.as_ref());
    // Through a link (dotfiles kept in a repository), the file it points to: the link stays.
    let path = &std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    let there = || {
        existing.map_or(Ok(()), |read| {
            is_still_the_file(path, KnownAs::fingerprint(read))
        })
    };
    // shortcut: `there` is looked at just before the rename, not in the same step; a file that
    // goes in those microseconds comes back. renameat2 with RENAME_EXCHANGE if that is ever seen.
    there()?;
    let has_other_names = existing.is_some() && check_writable(path)?;
    #[allow(clippy::disallowed_methods)] // replaces on purpose: the one writer of a file again
    write_beside(&tmp_path(path), path, write, |tmp| {
        // `before` may take a while (a sidecar written under its lock), so the file is looked
        // at again after it: one deleted meanwhile must not come back under its old name.
        there()
            .and_then(|_| before())
            .and_then(|_| there())
            .and_then(|_| std::fs::rename(tmp, path).map_err(|e| explain_rename_error(e, path)))
    })?;
    tell_other_names(path, has_other_names);
    Ok(())
}

/// Returns the fingerprint of the file at `path`, taken before a save reads it, for
/// `replace_existing`. Returns an error when nothing is there.
pub(crate) fn fingerprint_before_reading(path: &Path) -> std::io::Result<FileFingerprint> {
    fingerprint(path).ok_or_else(gone_meanwhile)
}

/// Looks whether the file at `path` is still the file `known`: the one rule by which Glimt
/// tells, right before a change, that no other file has taken the name since the list showed
/// it, the user picked it or Glimt read it. Another inode than the one its folder listed is
/// refused, and an unknown one passes. With a fingerprint, the device, size and time must also
/// be the same (`same_fingerprint`), for `size_and_time` the size and the time, and for
/// `size_and_time_as_kept` the size and the time as any file system keeps it
/// (`same_time_as_kept`). A file that is gone is then refused. Without a fingerprint, a file
/// that is gone is left to the operation to tell. Another file found has the window whose list
/// it is read its folder again (`ListIsOld`). It waits for the disk, so it is called in the
/// thread that makes the change. Returns the error to tell. A look that failed returns its own
/// error, never NotFound, so a caller that leaves a gone file to its operation does not take a
/// share that dropped for a moment for a file that is gone.
pub(crate) fn is_still_the_file(path: &Path, known: KnownAs) -> std::io::Result<()> {
    // With nothing known, nothing could be refused, and a look would only wait for the disk:
    // a round trip for each file on a share whose identities were forgotten.
    if known.listed.is_none() && known.fingerprint.is_none() {
        return Ok(());
    }
    let another = if is_another_file(identity_of(path), known.listed) {
        std::io::Error::other(crate::support::lang::tr(
            "another file has taken its name: the folder is read again",
        ))
    } else {
        let Some(before) = known.fingerprint else {
            return Ok(());
        };
        let is_same = |now: FileFingerprint| match known.compared {
            Compared::Whole => same_fingerprint(before, now),
            Compared::WithoutDevice => (before.1, before.2) == (now.1, now.2),
            Compared::TimeAsKept => before.1 == now.1 && same_time_as_kept(before.2, now.2),
        };
        match fingerprint_or_error(path) {
            Ok(now) if is_same(now) => return Ok(()),
            Ok(_) => changed_meanwhile(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(gone_meanwhile()),
            Err(e) => return Err(e),
        }
    };
    if let Some(list_is_old) = known.list_is_old {
        list_is_old.raise();
    }
    Err(another)
}

/// Returns the error for a file read or picked that is no longer there.
fn gone_meanwhile() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::NotFound,
        crate::support::lang::tr("the file was moved, renamed or deleted meanwhile"),
    )
}

/// Returns the error for a file that is no longer the one read or picked: another file has its
/// name, or it was written since.
pub(crate) fn changed_meanwhile() -> std::io::Error {
    std::io::Error::other(crate::support::lang::tr("the file was changed meanwhile"))
}

/// Returns whether identities `a` and `b` are known, and are of two files.
pub(crate) fn is_another_file(a: Identity, b: Identity) -> bool {
    matches!((a, b), (Some(a), Some(b)) if a != b)
}

/// Returns the identity of the file or link at `path`, as its folder entry gives it: the inode
/// of the name itself, not of a link's target. Returns `None` when nothing is there. It waits
/// for the disk.
pub(crate) fn identity_of(path: &Path) -> Identity {
    identity_from_metadata(path, &look(path, false).ok()?)
}

/// Returns the identity of the file at `path` from its metadata `meta`, which the caller looked
/// up without following a link. Returns `None` for inode 0.
pub(crate) fn identity_from_metadata(path: &Path, meta: &std::fs::Metadata) -> Identity {
    let id = crate::images::save::file_id_from_metadata(path, meta).ok()?;
    std::num::NonZeroU64::new(id.1)
}

/// Returns the paths of those of `files` that no other file has taken the name of since they
/// were listed (`ListedFile::check`), and an error for each of the others, naming its file.
pub(crate) fn still_listed(files: Vec<ListedFile>) -> (Vec<PathBuf>, Vec<String>) {
    let (mut kept, mut refused) = (Vec::new(), Vec::new());
    for file in files {
        match file.check() {
            Ok(()) => kept.push(file.path),
            Err(e) => refused.push(format!("{}: {e}", shown_path(&file.path))),
        }
    }
    (kept, refused)
}

/// Forgets the identities in `files`, names relative to `dir`, of the files on a file system
/// that does not keep a file's inode for as long as the file is there. The mount table is read
/// and parsed once, and each folder of the names is looked up once in it.
pub(crate) fn forget_passing_identities(
    dir: &Path,
    files: &mut [crate::model::listing::ListedName],
) {
    use crate::images::decode::read_limited;
    let mountinfo = read_limited("/proc/self/mountinfo").unwrap_or_default();
    let mounts = mounts_in(&mountinfo);
    let mut lasting: std::collections::HashMap<&Path, bool> = Default::default();
    let mut last: Option<(&Path, bool)> = None;
    let keeps: Vec<bool> = (files.iter())
        .map(|(name, _)| {
            let folder = Path::new(name).parent().unwrap_or(Path::new(""));
            // The names of one folder come one after the other, so most are not looked up.
            if let Some((before, keeps)) = last
                && before.as_os_str() == folder.as_os_str()
            {
                return keeps;
            }
            let keeps = *lasting.entry(folder).or_insert_with(|| {
                let kind = file_system_type_among(&mounts, &dir.join(folder), |_| None);
                kind.is_some_and(|kind| has_lasting_inodes(&kind))
            });
            last = Some((folder, keeps));
            keeps
        })
        .collect();
    for ((_, identity), keeps) in files.iter_mut().zip(keeps) {
        if !keeps {
            *identity = None;
        }
    }
}

/// Returns whether a file system of type `kind` (from `file_system_type`) keeps a file's inode
/// for as long as the file is there, and gives its folder entries the inode the file has. FAT
/// and exFAT make up an inode each time a file is read into memory, and FUSE and SMB mounts may
/// too, so a file there would be taken for another one. Any type not known to keep it counts
/// as one that does not, and its files are not told apart by identity.
pub(crate) fn has_lasting_inodes(kind: &str) -> bool {
    matches!(
        kind,
        "ext2"
            | "ext3"
            | "ext4"
            | "xfs"
            | "btrfs"
            | "tmpfs"
            | "f2fs"
            | "zfs"
            | "bcachefs"
            | "jfs"
            | "reiserfs"
            | "nilfs2"
            | "overlay"
            | "nfs"
            | "nfs4"
    )
}

/// Returns the fingerprint of the file or link at `path`, or `None` when nothing is there or
/// the look failed.
pub(crate) fn fingerprint(path: &Path) -> Option<FileFingerprint> {
    fingerprint_or_error(path).ok()
}

/// Returns the fingerprint of the file or link at `path`, or the error of the look: NotFound
/// when nothing is there.
pub(crate) fn fingerprint_or_error(path: &Path) -> std::io::Result<FileFingerprint> {
    use std::os::unix::fs::MetadataExt;
    #[cfg(test)]
    if LOOK_FAILS.with_borrow_mut(|fails| match fails {
        Some((name, looks_left)) if name == path => {
            let fails = *looks_left == 0;
            *looks_left = looks_left.saturating_sub(1);
            fails
        }
        _ => false,
    }) {
        return Err(std::io::Error::from_raw_os_error(libc::EIO));
    }
    let m = look(path, false)?;
    let key = crate::images::save::file_id_from_metadata(path, &m).ok();
    Ok((key, m.len(), (m.mtime(), m.mtime_nsec())))
}

/// Returns the paths of `files`, picked in the list, and the identities the list knew them by
/// (`Listed`), in their order, without a second copy of the paths.
fn paths_and_identities(files: Vec<ListedFile>) -> (Vec<PathBuf>, Listed) {
    let list_is_old = files
        .first()
        .map(|f| f.list_is_old.clone())
        .unwrap_or_default();
    let listed = files.iter().map(|f| f.identity).collect();
    let paths = files.into_iter().map(|f| f.path).collect();
    (paths, (listed, list_is_old))
}

/// Returns the fingerprint of the file at `path`, picked before a wait, or `None` when nothing is
/// there.
fn picked_fingerprint(path: &Path) -> Option<FileFingerprint> {
    #[cfg(test)]
    loop {
        let stalls = lock_even_if_poisoned(&FINGERPRINT_STALLS).as_deref() == Some(path);
        if !stalls {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    fingerprint(path)
}

/// Looks whether the folder at `path` is still the folder whose fingerprint was `picked`, on a
/// file system that keeps a file's inode or not (`keeps_inodes`). A folder is told by its inode
/// alone, which it keeps while files come and go, or else by its device and time
/// (`folder_time`). Another inode found has the window whose list is `list_is_old` read its
/// folder again. A folder that is gone is left to the act. Returns the error to tell.
fn is_still_the_folder(
    path: &Path,
    picked: FileFingerprint,
    keeps_inodes: bool,
    list_is_old: &ListIsOld,
) -> std::io::Result<()> {
    let Some(time) = folder_time(picked, keeps_inodes) else {
        let inode = picked
            .0
            .and_then(|(_, inode)| std::num::NonZeroU64::new(inode));
        return is_still_the_file(path, KnownAs::listed(inode, list_is_old));
    };
    let device = |(id, _, _): FileFingerprint| id.map(|(device, _)| device);
    match fingerprint_or_error(path) {
        Ok(now) if device(now) == device(picked) && now.2 == time => Ok(()),
        Ok(_) => Err(changed_meanwhile()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Returns the modification time by which the folder whose fingerprint is `found` is told apart,
/// with its device, where its file system does not keep a file's inode (`keeps_inodes`), as FAT,
/// exFAT and FUSE may give the inode to another folder later. The time then refuses a folder
/// that files came into or left since. Returns `None` where the inode tells the folder instead:
/// where the file system keeps it, and where the file system does not know the time
/// (`TIME_NOT_KNOWN`), as nothing else tells the folder there.
fn folder_time((_, _, time): FileFingerprint, keeps_inodes: bool) -> Option<(i64, i64)> {
    Some(time).filter(|&time| !keeps_inodes && time != TIME_NOT_KNOWN)
}

/// Returns whether fingerprints `a` and `b` are of the same file, the one rule Glimt tells a file
/// by: Undo, and a save or move of a file read or picked earlier. The size and modification time
/// must be the same, and the device too, or both have no inode. The inode alone says nothing: a
/// file system gives a freed inode to the next new file, so a file copied in at the name after the
/// first one was deleted may have the same inode. The inode itself may differ, as FAT gives the
/// same file another one later. Glimt's own rewrites of the
/// file (a turn saved) update the fingerprint through `written_anew`, so they still match. A
/// fingerprint taken before a Move that may have copied the file to another disk is compared
/// by `same_time_as_kept` instead.
pub(crate) fn same_fingerprint(a: FileFingerprint, b: FileFingerprint) -> bool {
    (a.1, a.2) == (b.1, b.2)
        && match (a.0, b.0) {
            (Some(x), Some(y)) => x.0 == y.0,
            (None, None) => true,
            _ => false,
        }
}

/// Returns whether the modification times `a` and `b`, in seconds and nanoseconds, are the same
/// time as any file system keeps it. A time set on a file is cut to the resolution its file
/// system keeps: 2 seconds on FAT, 10 ms on exFAT, 100 ns on NTFS and SMB, and whole seconds on
/// some FUSE file systems. So the time a Move to another disk gave the copy can be up to 2
/// seconds before the time the file had, and a time within that of the other counts as the
/// same. Glimt tells a file by this rule only where the fingerprint was taken before such a
/// Move, in a Move's log (`KnownAs::size_and_time_as_kept`). Everywhere else, a Rename Many's
/// plan among them, the file was only renamed or not moved at all, so its time is compared to
/// the nanosecond, as another file written there in the same 2 seconds would else pass.
pub(crate) fn same_time_as_kept(a: (i64, i64), b: (i64, i64)) -> bool {
    let nanoseconds = |(seconds, nanoseconds): (i64, i64)| {
        i128::from(seconds) * 1_000_000_000 + i128::from(nanoseconds)
    };
    (nanoseconds(a) - nanoseconds(b)).abs() < 2_000_000_000
}

/// Refuses to replace the file at `path` when this user may not write it: a photo made
/// read-only (0444), or another user's file. A replace renames a new file over the old one,
/// which needs only the folder's write bit, so without this check a read-only file would be
/// written over, where Geeqie, which writes in place, fails. A file without a write bit is
/// refused also for root, who may write any file, as its owner marked it so. A link at the name
/// passes, because the link itself is replaced, and so does a name that is not there yet. A
/// look that fails returns its error, as the file may be there and read-only.
/// Glimt's own files (`replace_file`) are not asked, as before. A turn saved asks it first too,
/// before the slow work of encoding the image. Returns whether the file has other names (hard
/// links), which the replace will not change.
pub(crate) fn check_writable(path: &Path) -> std::io::Result<bool> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    // Only a name that is surely not there is a new file. A look that failed is no answer.
    let meta = match look(path, false) {
        Ok(meta) if meta.is_file() => meta,
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
        _ => return Ok(false),
    };
    let name = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    #[allow(unsafe_code)]
    // SAFETY: `name` is a C string that lives through the call, and access only reads it.
    let allowed = unsafe { libc::access(name.as_ptr(), libc::W_OK) } == 0;
    match allowed && !meta.permissions().readonly() {
        true => Ok(meta.nlink() > 1),
        false => Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            crate::support::lang::tr("the file is read-only"),
        )),
    }
}

/// Counts a save that replaced the file at `path`, for the window's note, when the file had
/// other names (hard links). It is called after the save went through, so a save that failed
/// gives no note. The write is not refused, as the user asked for it. After the replace the name
/// has one link, so the same file is counted again only after a new hard link to it.
fn tell_other_names(path: &Path, has_other_names: bool) {
    if !has_other_names {
        return;
    }
    #[cfg(test)]
    OTHER_NAMES_NOTED.with_borrow_mut(|noted| noted.add(path));
    #[cfg(not(test))]
    lock_even_if_poisoned(&OTHER_NAMES).add(path);
}

/// A file written whole at `tmp` beside `path` (`create_temp_beside`), on disk, and then given its
/// name by `give_name`; `tmp` removed when anything fails. On disk before the rename: after a crash
/// the old file or the new one, never an empty one. The new name is on disk when this returns
/// (`sync_folder_of`), so an older version cannot come back after a power cut and leave an
/// image and its sidecar out of step. No folder made: a sidecar or a collection whose
/// folder was renamed meanwhile is not written into a new one of the old name. The one
/// temp-and-rename for every file Glimt writes: settings, sidecars, collections, a PDF, and
/// a thumbnail through `write_beside_without_folder_sync`.
pub fn write_beside(
    tmp: &Path,
    path: &Path,
    write: impl FnOnce(&mut std::fs::File) -> std::io::Result<()>,
    give_name: impl FnOnce(&Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    write_beside_without_folder_sync(tmp, path, write, give_name).and_then(|_| sync_folder_of(path))
}

/// Writes a file as `write_beside` does, without syncing its folder after the rename. This is
/// for a file whose loss after a power cut costs nothing, a thumbnail made again when it is
/// missing, where a folder sync for each would only slow the thumbnail workers down.
pub fn write_beside_without_folder_sync(
    tmp: &Path,
    path: &Path,
    write: impl FnOnce(&mut std::fs::File) -> std::io::Result<()>,
    give_name: impl FnOnce(&Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let write_and_sync = |f: &mut std::fs::File| {
        write(f)?;
        #[cfg(test)]
        FILES_SYNCED.set(FILES_SYNCED.get() + 1);
        f.sync_all()
    };
    write_beside_without_sync(tmp, path, write_and_sync, give_name)
}

/// Writes a file as `write_beside` does, without syncing the file or its folder. This is for
/// files written in a batch, which `sync_file_system_of` then puts on disk with one sync, before
/// anything counts on them.
pub fn write_beside_without_sync(
    tmp: &Path,
    path: &Path,
    write: impl FnOnce(&mut std::fs::File) -> std::io::Result<()>,
    give_name: impl FnOnce(&Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let result = create_temp_beside(tmp, path)
        .and_then(|mut f| write(&mut f))
        .and_then(|_| give_name(tmp));
    if result.is_err() {
        #[allow(clippy::disallowed_methods)] // Glimt's own: the temporary file, not finished
        // Not told: the write's own failure is returned, and a hidden name left takes no file's
        // place.
        let _ = std::fs::remove_file(tmp);
    }
    result
}

/// A file that is not there yet, written whole beside it and given its name in one step, never
/// over one another window made meanwhile (a collection of the same name): `AlreadyExists` then.
pub fn create_file(path: &Path, bytes: impl AsRef<[u8]>) -> std::io::Result<()> {
    use std::io::Write;
    write_beside(
        &tmp_path(path),
        path,
        |f| f.write_all(bytes.as_ref()),
        // Glimt's own: the new file under its temporary name, given its name never over a file.
        |tmp| crate::files::file_io::rename_never_over(tmp, path),
    )
}

/// Returns the name that a new version of `path`, or a copy, is written under before it is
/// renamed to its own: ".glimt-tmp-", this Glimt's instance ID, a number of its own and a count,
/// beside `path`. So "a.png" and "a.webp" saved at the same time never share one, nor does one
/// file saved by two windows at once. Two Glimts with the same process ID on two computers (a
/// shared home) or in two containers do not share one either, where one would remove the
/// other's file. It is not the file's own name with more after it, because a name near the
/// 255-byte limit could then not be saved turned, nor its sidecar written. The name is hidden,
/// so a file manager does not show one left by a crash. It is the one pattern of every
/// temporary file Glimt leaves, apart from Rename Many's, which its journal puts right.
pub fn tmp_path(path: &Path) -> PathBuf {
    static MADE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let k = MADE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    path.with_file_name(format!(".glimt-tmp-{}-{k}", instance_tag()))
}

/// This Glimt's instance ID and a random number of its own, for the names of its temporary
/// files: two Glimts with one process ID (two computers with one home, two containers) differ.
pub fn instance_tag() -> &'static str {
    use std::hash::{BuildHasher, Hasher};
    static OWN: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        // Random for each process: the standard library seeds its hash keys from the system.
        let n = std::collections::hash_map::RandomState::new()
            .build_hasher()
            .finish() as u32;
        format!("{}-{n:08x}", crate::features::remote::instance_id())
    });
    &OWN
}

/// The temporary file `tmp` for writing `path` again, with the permissions of `path` (a sidecar
/// or a list kept private, 0600, stays so). Made private first when `path` is there, and given
/// those before a byte is written: no one else can open it meanwhile. A new file as usual, but
/// only when nothing is surely at `path`. A look at `path` that fails returns its error, as a
/// new file would lose the mode, the user's attributes and the ACL of the file it replaces.
#[allow(clippy::disallowed_methods)] // the one way a new file is made, under its temporary name
pub fn create_temp_beside(tmp: &Path, path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    // Glimt's own: the temporary name. The file is made new, never opened: a link left or
    // planted at `tmp` would be followed and the file it points to emptied. Not told: a name
    // still at `tmp` makes `create_new` below fail, and that failure is returned.
    let _ = std::fs::remove_file(tmp);
    let new = || {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .clone()
    };
    let old = match look(path, true) {
        Ok(old) => old,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return new().open(tmp),
        Err(e) => return Err(e),
    };
    let file = new().mode(0o600).open(tmp)?;
    file.set_permissions(old.permissions())?;
    copy_extended_attributes(path, &file, true);
    Ok(file)
}

/// Copies to `to` the extended attributes of the file named `from` that a user may set: the
/// user's own (`user.*`, where KDE's Dolphin and Gwenview keep tags, ratings and comments) and
/// the file's ACL (`system.posix_acl_access`) when `keeps_acl` says so. A file replaced by a new
/// one (a turned image, a sidecar, Save As over a file) and a file moved to another disk keep
/// them all, as with `mv`. A copy keeps the user's own but not the ACL, as with `cp` without
/// `-p`, so that a grant to another user does not reach the user's own copy.
/// Others are left: `security.*` (SELinux) is given to the new file by the system, and
/// `trusted.*` needs root. The owner is not kept either, because only root may give a file to
/// another user, so a file replaced by its group's member becomes that user's. A link at `from`
/// gives nothing, so a replaced link does not take the attributes of the file it points to. An
/// attribute that cannot be read or set (a file system without them, ENOTSUP) is passed over,
/// as the file itself is written.
fn copy_extended_attributes(from: &Path, to: &std::fs::File, keeps_acl: bool) {
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    let Ok(from) = std::ffi::CString::new(from.as_os_str().as_bytes()) else {
        return;
    };
    // Linux's limits for the list of names (XATTR_LIST_MAX) and for one value (XATTR_SIZE_MAX).
    let (mut names, mut value) = (vec![0u8; 65_536], vec![0u8; 65_536]);
    #[allow(unsafe_code)]
    // SAFETY: `from` is a C string and `names` a buffer of the length given, both alive here.
    let listed = unsafe { libc::llistxattr(from.as_ptr(), names.as_mut_ptr().cast(), names.len()) };
    let Ok(listed) = usize::try_from(listed) else {
        return;
    };
    for name in names[..listed].split(|&b| b == 0) {
        if !(name.starts_with(b"user.") || (keeps_acl && name == b"system.posix_acl_access")) {
            continue;
        }
        let Ok(name) = std::ffi::CString::new(name) else {
            continue;
        };
        #[allow(unsafe_code)]
        // SAFETY: both C strings and `value`, of the length given, live through the call.
        let size = unsafe {
            libc::lgetxattr(
                from.as_ptr(),
                name.as_ptr(),
                value.as_mut_ptr().cast(),
                value.len(),
            )
        };
        let Ok(size) = usize::try_from(size) else {
            continue;
        };
        #[allow(unsafe_code)]
        // SAFETY: `to` is an open file, and `name` and the first `size` bytes of `value` live
        // through the call.
        unsafe {
            libc::fsetxattr(
                to.as_raw_fd(),
                name.as_ptr(),
                value.as_ptr().cast(),
                size,
                0,
            )
        };
    }
}

/// `replace_file`, with a failure on stderr: for files Glimt keeps for itself.
pub fn write_atomic(path: &Path, bytes: impl AsRef<[u8]>) {
    // Not told: without the folder `replace_file` below fails, and that failure is told.
    let _ = std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")));
    if let Err(e) = replace_file(path, bytes) {
        save_failed(crate::tf!(
            "Could not save {name}: {e}",
            name = shown_path(path),
            e
        ));
    }
}

/// A file that could not be saved, told on the terminal and, once, in the window.
pub fn save_failed(error: String) {
    eprintln!("{error}");
    // In tests the error stays on this thread. The window's own slot is shared by every test,
    // and another test's window took the error as its notice in place of its own.
    #[cfg(test)]
    lock_even_if_poisoned(told_here()).saves_failed.push(error);
    #[cfg(not(test))]
    {
        *lock_even_if_poisoned(&SAVE_ERROR) = Some(error);
    }
}

/// Returns what was told for this thread's test, also on the writer thread while
/// it does that test's work.
#[cfg(test)]
fn told_here() -> &'static std::sync::Mutex<ToldInTest> {
    TOLD_FOR
        .get()
        .unwrap_or_else(|| SAVES_FAILED.with(|told| *told))
}

/// Returns the errors that `save_failed` told for this thread's test, and empties the list.
#[cfg(test)]
pub(crate) fn take_saves_failed() -> Vec<String> {
    let mut told = lock_even_if_poisoned(told_here());
    told.taken_by_window = 0;
    std::mem::take(&mut told.saves_failed)
}

#[cfg(test)]
mod tests {
    /// A check that knows neither the listed inode nor a fingerprint looks at nothing, as it
    /// could refuse nothing. A star key on 500,000 files of a share, whose identities are
    /// forgotten, looked at each of them in the thread of Glimt's own files.
    #[test]
    fn a_check_that_knows_nothing_does_not_look() {
        use super::{KnownAs, LOOKS_MADE, is_still_the_file};
        let path = std::path::Path::new("/");
        let flag = super::ListIsOld::default();
        let before = LOOKS_MADE.get();
        assert!(is_still_the_file(path, KnownAs::listed(None, &flag)).is_ok());
        assert_eq!(
            LOOKS_MADE.get(),
            before,
            "a check that knows nothing looked"
        );
        let known = super::identity_of(path);
        assert!(is_still_the_file(path, KnownAs::listed(known, &flag)).is_ok());
        assert!(
            LOOKS_MADE.get() > before + 1,
            "a check that knows the inode did not look"
        );
    }

    /// The system's list of mounts and udev's file of a disk, damaged in any way, are read
    /// without a panic. A mount point's name is the user's, or comes with a stick.
    #[test]
    fn survives_broken_mountinfo() {
        let seed = b"36 35 98:0 / /media/a\\040b rw,noatime master:1 - fuseblk /dev/sdb1 rw\n\
            22 1 0:21 / / rw - ext4 /dev/root rw\n";
        for bytes in crate::support::test_support::broken(seed) {
            let path = std::path::Path::new("/media/a b/x.jpg");
            super::file_system_type_in(&bytes, path, |_| Some("exfat".into()));
            super::is_mounted_whole_in(&bytes, path);
        }
        for bytes in crate::support::test_support::broken(b"S:disk/by-uuid/1\nE:ID_FS_TYPE=exfat\n")
        {
            super::file_system_type_in_udev(&String::from_utf8_lossy(&bytes));
        }
    }

    /// A save that fails in one test is not told in another test's window. Before, a window
    /// took a save error of a test beside it as its notice, in place of the notice its own
    /// test waited for (`every_undo_leaves_a_file_put_in_its_place`).
    #[test]
    fn a_save_error_in_a_test_stays_with_its_test() {
        std::thread::spawn(|| super::save_failed("another test's error".into()))
            .join()
            .unwrap();
        assert_eq!(super::take_save_error(), None);
    }

    /// Saves over many files with other names (hard links), as a keywords job over a tree
    /// copied with `cp -al`, give one note with their count. Before, each file gave a note of
    /// its own, and the notice changed on every frame for the whole job.
    #[test]
    fn saves_over_hard_links_give_one_note_with_their_count() {
        let dir = std::env::temp_dir().join(format!("glimt-test-links-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let noted = || super::OTHER_NAMES_NOTED.with_borrow_mut(|n| n.take_note());
        noted();
        for name in ["a.xmp", "b.xmp", "c.xmp"] {
            std::fs::write(dir.join(name), "old").unwrap();
            std::fs::hard_link(dir.join(name), dir.join(format!("{name}.link"))).unwrap();
            super::replace_name(&dir.join(name), "new").unwrap();
        }
        let told = noted().unwrap_or_default();
        assert!(told.contains('3') && !told.contains("a.xmp"), "{told}");
        assert_eq!(noted(), None);
        // A name replaced once has one link, so it is not counted again.
        super::replace_name(&dir.join("a.xmp"), "newer").unwrap();
        assert_eq!(noted(), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A rename the file system cannot do is told in plain words with the system's own after
    /// them, and keeps its kind. Another error, such as a permission denied, stays as it was.
    #[test]
    fn tells_a_rename_the_file_system_cannot_do_in_plain_words() {
        let to = std::env::temp_dir().join("b.jpg");
        let kind = super::file_system_type(&std::env::temp_dir()).unwrap();
        for errno in [18, 38, 95] {
            let raw = std::io::Error::from_raw_os_error(errno);
            let told = super::explain_rename_error(std::io::Error::from_raw_os_error(errno), &to);
            assert_eq!(told.kind(), raw.kind());
            let text = told.to_string();
            assert!(text.contains("does not allow this rename") && text.contains(&raw.to_string()));
            assert!(text.contains(&format!("({kind})")), "{text}");
        }
        // Permission denied, and EPERM, which a sticky folder or an immutable file gives.
        for errno in [13, 1] {
            let denied = super::explain_rename_error(std::io::Error::from_raw_os_error(errno), &to);
            assert_eq!(denied.raw_os_error(), Some(errno));
        }
    }

    /// A Flatpak with --filesystem=host shows a folder of the host's own file system (/srv) by
    /// a bind mount of that folder alone, over a tmpfs of its own. Such a mount is not a whole
    /// file system, so the trash by hand does not put a `.Trash-UID` at its top where the host's
    /// file managers never look (#108). A disk mounted on its own (a stick) is whole.
    #[test]
    fn tells_a_bind_mount_of_a_folder_from_a_whole_file_system() {
        let mounts = b"1 0 0:30 / / rw - tmpfs tmpfs rw\n\
            2 1 8:1 /srv /srv rw - ext4 /dev/sda1 rw\n\
            3 1 8:1 /run/media /run/media rw - ext4 /dev/sda1 rw\n\
            4 3 8:17 / /run/media/me/STICK rw - vfat /dev/sdb1 rw\n\
            5 1 8:33 /a\\040b /mnt/a\\040b rw - ext4 /dev/sdc1 rw\n";
        let whole = |p: &str| super::is_mounted_whole_in(mounts, std::path::Path::new(p));
        assert!(!whole("/srv"));
        assert!(!whole("/srv/photos/a.jpg"));
        assert!(whole("/run/media/me/STICK"));
        assert!(!whole("/run/media/me"));
        assert!(!whole("/mnt/a b"));
        assert!(whole("/tmp"));
        assert!(!super::is_mounted_whole_in(b"", std::path::Path::new("/")));
    }

    /// A mount that the host made itself, at whose top the host's gio puts a trash too, is not
    /// taken for a Flatpak's bind of a folder, though its root is not "/": a btrfs subvolume, also
    /// one named as its mount point, and a folder the host mounts at a place of another name.
    /// Before, each of them was refused, and the window asked to delete for good. A Flatpak's
    /// bind of a folder is still told, through --filesystem=host, =home and =/some/dir, on ext4,
    /// btrfs, overlay and vfat. The document portal (fuse.portal) is refused as remote before.
    #[test]
    fn tells_a_hosts_own_mount_from_a_flatpaks_bind_of_a_folder() {
        let whole =
            |mounts: &[u8], p: &str| super::is_mounted_whole_in(mounts, std::path::Path::new(p));
        // --filesystem=host on Fedora's btrfs, with a second disk of a subvolume of its own.
        let host = b"1 0 0:30 / / rw - tmpfs tmpfs rw\n\
            2 1 0:40 /root/srv /srv rw - btrfs /dev/sda3 rw,seclabel,subvolid=256,subvol=/root\n\
            3 1 0:40 /root/mnt /mnt rw - btrfs /dev/sda3 rw,seclabel,subvolid=256,subvol=/root\n\
            4 3 0:41 /@data /mnt/data rw - btrfs /dev/sdd1 rw,subvolid=257,subvol=/@data\n\
            5 1 0:42 /home /home rw - btrfs /dev/sda3 rw,seclabel,subvolid=258,subvol=/home\n\
            6 3 8:49 /photos /mnt/pictures rw - ext4 /dev/sdd2 rw\n\
            7 1 0:43 /opt /opt rw - overlay overlay rw,lowerdir=/l,upperdir=/u,workdir=/w\n\
            8 3 0:44 / /mnt/ov rw - overlay overlay rw,lowerdir=/l,upperdir=/u,workdir=/w\n";
        assert!(!whole(host, "/srv"));
        assert!(!whole(host, "/mnt"));
        assert!(whole(host, "/mnt/data"));
        assert!(whole(host, "/home"));
        assert!(whole(host, "/mnt/pictures"));
        assert!(!whole(host, "/opt"));
        assert!(whole(host, "/mnt/ov"));
        // --filesystem=home with /home a disk of its own, and --filesystem=/some/dir in a folder
        // of a subvolume, of a stick and of a host's bind mount of a folder.
        let dirs = b"1 0 0:30 / / rw - tmpfs tmpfs rw\n\
            2 1 8:2 /u /home/u rw - ext4 /dev/sda2 rw\n\
            3 1 0:41 /@data/photos /mnt/data/photos rw - btrfs /dev/sdd1 rw,subvol=/@data\n\
            4 1 8:17 /DCIM /run/media/u/STICK/DCIM rw - vfat /dev/sdb1 rw\n\
            5 1 8:49 /photos/2024 /mnt/pictures/2024 rw - ext4 /dev/sdd2 rw\n";
        for top in ["/home/u", "/mnt/data/photos", "/run/media/u/STICK/DCIM"] {
            assert!(!whole(dirs, top), "{top}");
        }
        assert!(!whole(dirs, "/mnt/pictures/2024"));
        assert!(super::is_remote("fuse.portal"));
    }

    /// The mount that holds a path is the one the kernel finds going down through the mounts:
    /// the later one where two are at the same place, and a mount added later at /mnt that hides
    /// one at /mnt/card added before it. A mount point with a space in it is read as the kernel
    /// writes it, and one whose name is not UTF-8 is found too. A mount point that only starts
    /// with the same letters does not hold the path.
    #[test]
    fn finds_the_mount_that_holds_a_path() {
        use std::os::unix::ffi::OsStrExt;
        let mounts = b"1 0 8:1 / / rw - ext4 /dev/sda1 rw\n\
            2 1 8:33 / /media/me/K\xf8RT rw shared:5 - vfat /dev/sdc1 rw\n\
            3 1 0:50 / /run/user/1000/gvfs rw - fuse.gvfsd-fuse gvfsd-fuse rw\n\
            4 1 8:17 / /media/me/CARD\\040ONE rw - vfat /dev/sdb1 rw\n\
            5 1 0:51 / /mnt/cloud rw - fuse.rclone remote: rw\n\
            6 5 0:52 / /mnt/cloud rw - cifs //nas/photos rw\n\
            7 1 8:49 / /mnt/old/card rw - vfat /dev/sdd1 rw\n\
            8 1 0:53 / /mnt/old rw - tmpfs tmpfs rw\n\
            9 8 8:65 / /mnt/old/new rw - exfat /dev/sde1 rw\n\
            10 11 8:81 / /srv/card rw - vfat /dev/sdf1 rw\n\
            11 1 0:54 / /srv rw - tmpfs tmpfs rw\n\
            12 1 8:97 / /media/me/EXFAT rw - fuseblk /dev/sdg1 rw,user_id=0\n\
            13 1 8:113 / /media/me/NTFS rw - fuseblk /dev/sdh1 rw,user_id=0\n\
            broken\n";
        let udev = |dev: &[u8]| (dev == b"8:97").then(|| "exfat".to_owned());
        let kind = |p: &str| super::file_system_type_in(mounts, std::path::Path::new(p), udev);
        assert_eq!(kind("/home/me/a.jpg").as_deref(), Some("ext4"));
        let gvfs = "/run/user/1000/gvfs/sftp:host=nas/a.jpg";
        assert_eq!(kind(gvfs).as_deref(), Some("fuse.gvfsd-fuse"));
        assert_eq!(kind("/media/me/CARD ONE/DCIM").as_deref(), Some("vfat"));
        assert_eq!(kind("/media/me/CARD").as_deref(), Some("ext4"));
        assert_eq!(kind("/mnt/cloud/a.jpg").as_deref(), Some("cifs"));
        assert_eq!(kind("/mnt/cloudy").as_deref(), Some("ext4"));
        assert_eq!(kind("/mnt/old/card/a.jpg").as_deref(), Some("tmpfs"));
        assert_eq!(kind("/mnt/old/new/a.jpg").as_deref(), Some("exfat"));
        // A mount moved onto another keeps its place in the table, before the one it is on.
        assert_eq!(kind("/srv/card/a.jpg").as_deref(), Some("vfat"));
        // exFAT through FUSE is known by the type udev found on its disk, when udev knows it.
        assert_eq!(kind("/media/me/EXFAT/a.jpg").as_deref(), Some("exfat"));
        assert_eq!(kind("/media/me/NTFS/a.jpg").as_deref(), Some("fuseblk"));
        let udev_file = "S:disk/by-uuid/1234-ABCD\nE:ID_FS_USAGE=filesystem\nE:ID_FS_TYPE=exfat\n";
        assert_eq!(
            super::file_system_type_in_udev(udev_file).as_deref(),
            Some("exfat")
        );
        assert_eq!(super::file_system_type_in_udev("E:ID_FS_TYPE=\n"), None);
        let latin1 = std::path::Path::new(std::ffi::OsStr::from_bytes(b"/media/me/K\xf8RT/a.jpg"));
        assert_eq!(
            super::file_system_type_in(mounts, latin1, udev).as_deref(),
            Some("vfat")
        );
        assert!(super::is_remote("fuse.rclone") && super::is_remote("cifs"));
        assert!(!super::is_remote("fuseblk") && !super::is_remote("vfat"));
    }

    /// In a chroot, the top mounts are those whose parents are outside it. One that alone
    /// starts a path holds it. Where several at different places start it, the one the path
    /// reaches is not known, and neither is a path that no top mount starts.
    #[test]
    fn a_chroot_with_several_top_mounts_answers_only_what_it_knows() {
        let mounts = b"30 20 0:60 / / rw - tmpfs tmpfs rw\n\
            31 20 0:5 / /proc rw - proc proc rw\n\
            32 30 8:17 / /card rw - vfat /dev/sdb1 rw\n";
        let none = |_: &[u8]| None;
        let kind = |m: &[u8], p: &str| super::file_system_type_in(m, std::path::Path::new(p), none);
        assert_eq!(kind(mounts, "/proc/1/status"), None);
        assert_eq!(kind(mounts, "/card/a.jpg").as_deref(), Some("vfat"));
        assert_eq!(kind(mounts, "/home/a.jpg").as_deref(), Some("tmpfs"));
        let no_root = b"31 20 0:5 / /proc rw - proc proc rw\n\
            33 20 0:6 / /dev rw - devtmpfs udev rw\n";
        assert_eq!(kind(no_root, "/proc/1").as_deref(), Some("proc"));
        assert_eq!(kind(no_root, "/home/a.jpg"), None);
    }

    /// A replace whose write fails never runs the step before the rename, so Save As over a file
    /// keeps that file's sidecar when the disk is full. A step that fails leaves the file as it
    /// was, with no temporary file beside it.
    #[test]
    fn replace_name_runs_its_step_only_after_a_whole_write() {
        let dir = std::env::temp_dir().join(format!("glimt-test-name-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("b.png");
        super::create_file(&file, "old").unwrap();
        let full = || Err(std::io::Error::other("no space left"));
        let mut ran = false;
        let step = || {
            ran = true;
            Ok(())
        };
        assert!(super::replace_name_with(&file, |_| full(), step).is_err());
        assert!(!ran);
        use std::io::Write;
        assert!(super::replace_name_with(&file, |f| f.write_all(b"new"), full).is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "old");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        std::fs::remove_dir_all(dir).unwrap();
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

    /// Where the file system cannot rename in one step without replacing (FAT, exFAT, ntfs-3g,
    /// NFS, many FUSE shares), a rename is a link and an unlink, and where it cannot link
    /// either, a rename after a look. Neither replaces a file. A source whose unlink is refused
    /// keeps its name and loses the new one, and a source gone meanwhile counts as moved.
    #[test]
    fn renames_without_one_step_never_over_a_file() {
        use super::RENAME_WAYS_MISSING;
        let dir = std::env::temp_dir().join(format!("glimt-test-fallback-{}", std::process::id()));
        let (a, b, c) = (dir.join("a"), dir.join("b"), dir.join("c"));
        let text = |p: &std::path::Path| std::fs::read_to_string(p).unwrap();
        for missing in [1, 2] {
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(&a, "a").unwrap();
            std::fs::write(&b, "b").unwrap();
            RENAME_WAYS_MISSING.set(missing);
            let taken = super::rename_never_over(&a, &b);
            let renamed = super::rename_never_over(&a, &c);
            RENAME_WAYS_MISSING.set(0);
            assert_eq!(taken.unwrap_err().kind(), std::io::ErrorKind::AlreadyExists);
            assert_eq!(text(&b), "b");
            renamed.unwrap();
            assert!(!a.exists() && text(&c) == "a");
        }
        // A folder cannot be unlinked as a file, as a file in another user's sticky folder
        // cannot.
        let (folder, linked) = (dir.join("folder"), dir.join("linked"));
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(&linked, "x").unwrap();
        assert!(super::unlink_linked(&folder, &linked).is_err());
        assert!(folder.is_dir() && !linked.exists());
        std::fs::write(&linked, "x").unwrap();
        super::unlink_linked(&dir.join("gone"), &linked).unwrap();
        assert_eq!(text(&linked), "x");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A move whose old name a share failed to unlink without saying that it did nothing (EIO
    /// after a lost answer, while its cache still shows the old name) keeps both names and says
    /// so, because the new name may be the only copy left. One refused (no right to it, a name
    /// too long) takes the new name back, so that the move is all or nothing.
    #[test]
    fn an_unlink_that_may_have_been_done_keeps_both_names() {
        use super::{RENAME_WAYS_MISSING, UNLINK_FAILS};
        let dir = std::env::temp_dir().join(format!("glimt-test-unlink-{}", std::process::id()));
        let (a, b) = (dir.join("a.jpg"), dir.join("b.jpg"));
        let answers = [
            (libc::EIO, true),
            (libc::ETIMEDOUT, true),
            (libc::EPERM, false),
            (libc::ENAMETOOLONG, false),
            (libc::EINVAL, false),
            // An error that Glimt does not know may hide an unlink done, so both names stay.
            (libc::EBADE, true),
        ];
        for (errno, keeps_both) in answers {
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(&a, "a").unwrap();
            RENAME_WAYS_MISSING.set(1);
            UNLINK_FAILS.set(Some((a.clone(), errno)));
            let renamed = super::rename_never_over(&a, &b);
            UNLINK_FAILS.set(None);
            RENAME_WAYS_MISSING.set(0);
            let error = renamed.unwrap_err().to_string();
            assert!(a.exists(), "{errno}");
            assert_eq!(b.exists(), keeps_both, "{errno}");
            // The text says what is known: the old name may be gone, so the user checks both.
            assert_eq!(
                error.contains("may be under both names"),
                keeps_both,
                "{error}"
            );
            assert_eq!(error.contains("check both"), keeps_both, "{error}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A listing of files in several folders parses the mount table once, however many folders
    /// it looks up there. Before, it parsed the whole table again for each folder, which a list
    /// with subfolders of 10,000 folders did 10,000 times.
    #[test]
    fn a_listing_parses_the_mount_table_once() {
        let names = ["a/1.jpg", "b/2.jpg", "c/3.jpg", "c/4.jpg"];
        let mut files: Vec<crate::model::listing::ListedName> =
            names.iter().map(|n| (n.into(), None)).collect();
        super::MOUNT_TABLES_PARSED.set(0);
        super::forget_passing_identities(std::path::Path::new("/tmp"), &mut files);
        assert_eq!(super::MOUNT_TABLES_PARSED.get(), 1);
    }

    /// A time cut to what FAT (2 s), exFAT (10 ms), NTFS (100 ns) or a FUSE mount (1 s) keeps
    /// is the same time as kept, either way round, and a time 2 seconds or more off is not.
    #[test]
    fn a_time_is_the_same_as_any_file_system_keeps_it() {
        let set = (1_700_000_001, 999_999_999);
        for kept in [
            (1_700_000_000, 0),
            (1_700_000_001, 990_000_000),
            (1_700_000_001, 0),
        ] {
            assert!(super::same_time_as_kept(set, kept) && super::same_time_as_kept(kept, set));
        }
        assert!(!super::same_time_as_kept(
            (1_700_000_002, 0),
            (1_700_000_000, 0)
        ));
        assert!(!super::same_time_as_kept(
            (1_700_000_000, 0),
            (1_699_999_997, 999_999_999)
        ));
    }

    /// A folder picked before a wait on a file system that may give its inode to another folder
    /// (FAT, exFAT, FUSE) is told apart by its device and time, not by that inode. Before, a
    /// folder made since under the picked one's name could pass with the freed inode. Where the
    /// file system keeps a folder's inode, the inode tells it, also when files came into it.
    #[test]
    fn a_folder_without_a_lasting_inode_is_told_by_its_time() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-folder-time-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let picked = super::fingerprint(&dir).unwrap();
        assert_eq!(super::folder_time(picked, true), None);
        assert_eq!(super::folder_time(picked, false), Some(picked.2));
        super::is_still_the_folder(&dir, picked, false, &Default::default()).unwrap();
        // Another folder in its place, as a folder that files came into, has another time.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(dir.join("new.jpg"), "x").unwrap();
        assert!(super::is_still_the_folder(&dir, picked, false, &Default::default()).is_err());
        super::is_still_the_folder(&dir, picked, true, &Default::default()).unwrap();
        let fingerprints = super::PickedFingerprints {
            taken: [(dir.clone(), (picked, None))].into(),
            list_is_old: Default::default(),
            are_folders: true,
        };
        if super::file_system_type(&dir).is_some_and(|kind| super::has_lasting_inodes(&kind)) {
            fingerprints.still_the_one_picked(&dir).unwrap();
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A folder picked on a FUSE or MTP mount that gives every file the time 0 is told by its
    /// inode, as nothing else tells it. Before, the time 0 was taken for the mark of a folder
    /// told by its inode, and the inode kept was 0, so any folder at the name passed unlooked.
    #[test]
    fn a_folder_without_a_known_time_is_told_by_its_inode() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-folder-no-time-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (picked_dir, other) = (dir.join("picked"), dir.join("other"));
        std::fs::create_dir_all(&picked_dir).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        for folder in [&picked_dir, &other] {
            let opened = std::fs::File::open(folder).unwrap();
            opened.set_modified(std::time::UNIX_EPOCH).unwrap();
        }
        let picked = super::fingerprint(&picked_dir).unwrap();
        assert_eq!(picked.2, super::TIME_NOT_KNOWN);
        super::is_still_the_folder(&picked_dir, picked, false, &Default::default()).unwrap();
        // Another folder, with the same time 0, takes the picked one's name.
        std::fs::rename(&picked_dir, dir.join("gone")).unwrap();
        std::fs::rename(&other, &picked_dir).unwrap();
        assert!(
            super::is_still_the_folder(&picked_dir, picked, false, &Default::default()).is_err()
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A path through an opened folder (`path_through`) is asked about and shown by the folder's
    /// own name. Before, the mount table found it on /proc, so a trash at the top of an NFS or
    /// SMB mount was taken for a local one, and ENOENT there for nothing done. A sidecar whose
    /// rename was sent again then went back into the trash without its photo. The text of an
    /// unlink that may have been done named /proc/self/fd/N, which the user cannot open, and the
    /// text of a rename the file system refuses called it "proc".
    #[test]
    fn a_path_through_an_opened_folder_is_asked_and_shown_by_its_name() {
        use super::{RENAME_WAYS_MISSING, UNLINK_FAILS};
        let dir = std::env::temp_dir().join(format!("glimt-test-through-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let opened = super::open_folder_not_through_link(&dir).unwrap();
        let through = super::path_through(&opened);
        let shown = |p: &std::path::Path| super::shown_path(p);
        assert_eq!(shown(&through.join("a.jpg")), shown(&dir.join("a.jpg")));
        assert_eq!(
            super::file_system_type(&through.join("a.jpg")),
            super::file_system_type(&dir.join("a.jpg"))
        );
        assert_eq!(
            super::is_mounted_whole(&through),
            super::is_mounted_whole(&dir)
        );
        // The folder on an NFS mount, with /proc as every system has it.
        let mounts = b"1 0 0:40 / / rw - nfs4 nas:/ rw\n2 1 0:5 / /proc rw - proc proc rw\n";
        let kind = super::file_system_type_in(mounts, &through.join("a.jpg"), |_| None);
        assert_eq!(kind.as_deref(), Some("nfs4"));
        let enoent = std::io::Error::from_raw_os_error(libc::ENOENT);
        assert!(super::may_have_been_done_on(&enoent, || kind));
        // The unlink of the old name fails with an error that may hide one done.
        std::fs::write(dir.join("a.jpg"), "a").unwrap();
        RENAME_WAYS_MISSING.set(1);
        UNLINK_FAILS.set(Some((through.join("a.jpg"), libc::EBADE)));
        let renamed = super::rename_never_over(&through.join("a.jpg"), &dir.join("b.jpg"));
        UNLINK_FAILS.set(None);
        RENAME_WAYS_MISSING.set(0);
        let told = renamed.unwrap_err().to_string();
        assert!(told.contains(&shown(&dir.join("a.jpg"))), "{told}");
        assert!(!told.contains("/proc/self/fd"), "{told}");
        let refused = std::io::Error::from_raw_os_error(libc::EXDEV);
        let told = super::explain_rename_error(refused, &through.join("b.jpg")).to_string();
        let kind = super::file_system_type(&dir).unwrap();
        assert!(told.contains(&format!("({kind})")), "{told}");
        drop(opened);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A step that answers ENOENT on a network or cloud mount may have been done, as NFS and SMB
    /// send a step again when its answer is lost, and a rename done the first time answers
    /// ENOENT the second. Before, it counted as nothing done, so Rename Many left a file under
    /// its hidden name and the trash by hand dropped its .trashinfo. On a local disk ENOENT
    /// still says that nothing was done.
    #[test]
    fn enoent_on_a_network_mount_may_have_been_done() {
        let enoent = std::io::Error::from_raw_os_error(libc::ENOENT);
        let on =
            |kind: Option<&str>| super::may_have_been_done_on(&enoent, || kind.map(Into::into));
        assert!(on(Some("nfs4")) && on(Some("cifs")) && on(Some("fuse.sshfs")));
        assert!(on(None));
        assert!(!on(Some("ext4")) && !on(Some("vfat")));
        let refused = std::io::Error::from_raw_os_error(libc::ENAMETOOLONG);
        assert!(!super::may_have_been_done_on(&refused, || Some(
            "nfs4".into()
        )));
    }

    /// Where neither RENAME_NOREPLACE nor links can be made (rclone, s3fs, MTP), the rename after
    /// looking takes a name for free only when the look said NotFound. A link that points
    /// nowhere, which `exists` took for no file, and a name whose look failed are not renamed
    /// over.
    #[test]
    fn the_last_way_to_rename_goes_over_no_name_it_could_not_see() {
        use super::{LOOKS_FAIL, RENAME_WAYS_MISSING};
        let dir =
            std::env::temp_dir().join(format!("glimt-test-look-first-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (a, link, unseen) = (dir.join("a.jpg"), dir.join("link.jpg"), dir.join("c.jpg"));
        std::fs::write(&a, "a").unwrap();
        std::fs::write(&unseen, "c").unwrap();
        std::os::unix::fs::symlink(dir.join("nowhere"), &link).unwrap();
        crate::support::threads::lock_even_if_poisoned(&LOOKS_FAIL).push(unseen.clone());
        RENAME_WAYS_MISSING.set(2);
        let over_link = super::rename_never_over(&a, &link);
        let over_unseen = super::rename_never_over(&a, &unseen);
        RENAME_WAYS_MISSING.set(0);
        crate::support::threads::lock_even_if_poisoned(&LOOKS_FAIL).retain(|p| *p != unseen);
        let kind = |r: std::io::Result<()>| r.unwrap_err().kind();
        let text = |p: &std::path::Path| std::fs::read_to_string(p).unwrap();
        assert_eq!(kind(over_link), std::io::ErrorKind::AlreadyExists);
        assert_eq!(kind(over_unseen), std::io::ErrorKind::AlreadyExists);
        assert!(std::fs::symlink_metadata(&link).unwrap().is_symlink());
        assert_eq!(text(&unseen), "c");
        assert_eq!(text(&a), "a");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A new file and a file replaced are on disk with their names when the call returns: the
    /// folder that holds each is synced. A copy is not, as nothing counts on its name. A move to
    /// another disk syncs the copy's folder before it unlinks the source, which a test cannot
    /// reach on one file system.
    #[test]
    fn names_given_are_synced_with_their_folder() {
        let dir = std::env::temp_dir().join(format!("glimt-test-synced-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("to")).unwrap();
        let synced = |work: &dyn Fn()| {
            super::FOLDERS_SYNCED.take();
            work();
            super::FOLDERS_SYNCED.take()
        };
        let made = synced(&|| super::create_file(&dir.join("a.jpg"), "a").unwrap());
        assert_eq!(made, [dir.as_path()]);
        let copied =
            synced(&|| super::put_file(&dir.join("a.jpg"), &dir.join("to/a.jpg"), true).unwrap());
        assert!(copied.is_empty());
        let replaced = synced(&|| super::replace_name(&dir.join("a.jpg"), "b").unwrap());
        assert_eq!(replaced, [dir.as_path()]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A file replaced by name and a file copied keep the user's extended attributes, where KDE
    /// keeps its tags and ratings. Before, both lost them, as the new file was made empty.
    #[test]
    fn a_replaced_or_copied_file_keeps_its_extended_attributes() {
        use crate::support::test_support::{attribute, set_attribute};
        let dir = std::env::temp_dir().join(format!("glimt-test-xattr-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("to")).unwrap();
        let a = dir.join("a.jpg");
        std::fs::write(&a, "a").unwrap();
        if set_attribute(&a, "user.xdg.tags", b"holiday") {
            super::replace_name(&a, "b").unwrap();
            assert_eq!(
                attribute(&a, "user.xdg.tags").as_deref(),
                Some(&b"holiday"[..])
            );
            super::put_file(&a, &dir.join("to/a.jpg"), true).unwrap();
            let copied = attribute(&dir.join("to/a.jpg"), "user.xdg.tags");
            assert_eq!(copied.as_deref(), Some(&b"holiday"[..]));
        } else {
            eprintln!("no extended attributes on this file system: not tested");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A file's ACL stays with it when the file is replaced or moved to another disk, as `mv`
    /// keeps it, but a copy does not take it, as `cp` without `-p` does not. A grant to another
    /// user on the source would else reach the user's own copy, in place of the target folder's
    /// default. The move to another disk needs /dev/shm on another file system than the
    /// temporary folder, as in Docker and on GitHub's runners, and is passed over elsewhere.
    #[test]
    fn a_copy_leaves_the_acl_that_a_move_keeps() {
        use crate::support::test_support::{attribute, set_attribute};
        use std::os::unix::fs::MetadataExt;
        const ACL: &str = "system.posix_acl_access";
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("glimt-test-acl-{pid}"));
        std::fs::create_dir_all(dir.join("to")).unwrap();
        let a = dir.join("a.jpg");
        std::fs::write(&a, "a").unwrap();
        // Version 2, then each entry as tag, permissions and ID: the owner rw, user 4321 r, the
        // group r, the mask r and others nothing.
        let mut acl = 2u32.to_le_bytes().to_vec();
        for (tag, perm, id) in [(1u16, 6u16, u32::MAX), (2, 4, 4321), (4, 4, u32::MAX)] {
            acl.extend(
                [
                    &tag.to_le_bytes()[..],
                    &perm.to_le_bytes(),
                    &id.to_le_bytes(),
                ]
                .concat(),
            );
        }
        for (tag, perm) in [(0x10u16, 4u16), (0x20, 0)] {
            let entry = [
                &tag.to_le_bytes()[..],
                &perm.to_le_bytes(),
                &u32::MAX.to_le_bytes(),
            ];
            acl.extend(entry.concat());
        }
        if !set_attribute(&a, ACL, &acl) {
            eprintln!("no ACLs on this file system: not tested");
            std::fs::remove_dir_all(dir).unwrap();
            return;
        }
        super::replace_name(&a, "b").unwrap();
        assert!(attribute(&a, ACL).is_some(), "a replaced file lost its ACL");
        super::put_file(&a, &dir.join("to/a.jpg"), true).unwrap();
        assert_eq!(attribute(&dir.join("to/a.jpg"), ACL), None);
        let shm = std::path::Path::new("/dev/shm");
        let dev = |p: &std::path::Path| std::fs::metadata(p).map(|m| m.dev()).ok();
        if dev(shm).is_some() && dev(shm) != dev(&dir) {
            let moved = shm.join(format!("glimt-test-acl-{pid}.jpg"));
            super::put_file(&a, &moved, false).unwrap();
            let kept = attribute(&moved, ACL);
            std::fs::remove_file(&moved).unwrap();
            assert!(kept.is_some(), "a file moved to another disk lost its ACL");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Stop acts inside a large file: a copy stopped halfway leaves nothing at the destination,
    /// not even its hidden part, and the source as it was.
    #[test]
    fn a_copy_stops_inside_a_large_file() {
        let dir = std::env::temp_dir().join(format!("glimt-test-put-stop-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("to")).unwrap();
        let a = dir.join("a.mp4");
        std::fs::write(&a, vec![7u8; 1 << 20]).unwrap();
        let asked = std::cell::Cell::new(0);
        // Stopped after the first part.
        let is_stopped = || {
            asked.set(asked.get() + 1);
            asked.get() > 1
        };
        let stopped = super::put_file_until_stopped(&a, &dir.join("to/a.mp4"), true, &is_stopped);
        assert_eq!(stopped.unwrap_err().kind(), std::io::ErrorKind::Interrupted);
        assert_eq!(asked.get(), 2, "not stopped inside the file");
        assert_eq!(std::fs::read_dir(dir.join("to")).unwrap().count(), 0);
        assert_eq!(std::fs::read(&a).unwrap().len(), 1 << 20);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A move to another disk whose source gets a new version while it is copied keeps that
    /// version, and says so. Before, the name was unlinked after the copy, and the new version,
    /// which a sync client had put there meanwhile, was gone.
    #[test]
    fn a_move_to_another_disk_keeps_a_source_changed_meanwhile() {
        let dir = std::env::temp_dir().join(format!("glimt-test-move-new-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("to")).unwrap();
        let (a, newer) = (dir.join("a.mp4"), dir.join("newer.mp4"));
        std::fs::write(&a, vec![1u8; 64 << 10]).unwrap();
        let asked = std::cell::Cell::new(0);
        // A new version is put at the name while the second part is copied.
        let is_stopped = || {
            asked.set(asked.get() + 1);
            if asked.get() == 2 {
                std::fs::write(&newer, "the new version").unwrap();
                std::fs::rename(&newer, &a).unwrap();
            }
            false
        };
        super::MOVES_CROSS_DEVICES.set(true);
        let moved = super::put_file_until_stopped(&a, &dir.join("to/a.mp4"), false, &is_stopped);
        super::MOVES_CROSS_DEVICES.set(false);
        let e = moved.unwrap_err().to_string();
        assert!(e.contains("under both names"), "{e}");
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "the new version");
        assert_eq!(std::fs::read(dir.join("to/a.mp4")).unwrap().len(), 64 << 10);
        // A source that stays as it was is moved.
        std::fs::remove_file(dir.join("to/a.mp4")).unwrap();
        super::MOVES_CROSS_DEVICES.set(true);
        let moved = super::put_file(&a, &dir.join("to/a.mp4"), false);
        super::MOVES_CROSS_DEVICES.set(false);
        moved.unwrap();
        assert!(super::is_surely_gone(&a));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A file written again whose look fails, as on a share that dropped for a moment, is not
    /// written as a new file. Before, it lost its mode (a private sidecar became readable by
    /// all), its user attributes and its ACL.
    #[test]
    fn a_failed_look_at_a_file_written_again_is_no_new_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("glimt-test-temp-look-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (path, tmp) = (dir.join("a.xmp"), dir.join(".a.xmp.tmp"));
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        crate::support::threads::lock_even_if_poisoned(&super::LOOKS_FAIL).push(path.clone());
        let made = super::create_temp_beside(&tmp, &path);
        let checked = super::check_writable(&path);
        crate::support::threads::lock_even_if_poisoned(&super::LOOKS_FAIL).retain(|p| *p != path);
        assert!(made.is_err() && checked.is_err());
        assert!(super::is_surely_gone(&tmp));
        let made = super::create_temp_beside(&tmp, &path).unwrap();
        assert_eq!(made.metadata().unwrap().permissions().mode() & 0o777, 0o600);
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

    /// A copy keeps the file's time and permissions, without set-user-ID and set-group-ID; it
    /// never replaces a file.
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
        std::fs::set_permissions(&a, std::fs::Permissions::from_mode(0o6750)).unwrap();
        super::put_file(&a, &b, true).unwrap();
        let meta = std::fs::metadata(&b).unwrap();
        assert_eq!(
            (meta.modified().unwrap(), meta.permissions().mode() & 0o7777),
            (old, 0o750)
        );
        assert!(super::put_file(&a, &b, true).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Two windows changing one shared file at the same moment both keep their change, and one
    /// that cannot be read is not written over: Open Recent lost an entry so, having copied
    /// the steps by hand and left two out.
    #[test]
    fn edit_shared_keeps_every_change() {
        let dir = std::env::temp_dir().join(format!("glimt-test-shared-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("recent");
        std::thread::scope(|s| {
            for k in 0..8 {
                let file = &file;
                s.spawn(move || {
                    super::edit_shared(file, |old| {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                        let mut new = old.to_vec();
                        new.extend(format!("{k}\n").bytes());
                        (new, ())
                    })
                    .unwrap();
                });
            }
        });
        let mut lines: Vec<String> = (std::fs::read_to_string(&file).unwrap().lines())
            .map(str::to_owned)
            .collect();
        lines.sort();
        assert_eq!(lines, (0..8).map(|k| k.to_string()).collect::<Vec<_>>());
        // A folder in its place cannot be read: refused, and left as it was.
        let folder = dir.join("folder");
        std::fs::create_dir(&folder).unwrap();
        assert!(super::edit_shared(&folder, |_| (b"x".to_vec(), ())).is_err());
        assert!(folder.is_dir());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A write that panics leaves the writer running: the next one is done, and flush waits.
    /// The change it lost is told once, as every failed write is. Before, it was lost unseen.
    #[test]
    fn writer_survives_a_panic() {
        use std::sync::atomic::{AtomicBool, Ordering};
        static DONE: AtomicBool = AtomicBool::new(false);
        super::take_saves_failed();
        super::write_later(|| panic!("a broken write"));
        super::write_later(|| DONE.store(true, Ordering::Relaxed));
        super::flush();
        assert!(DONE.load(Ordering::Relaxed));
        assert_eq!(super::take_saves_failed().len(), 1);
        // A write that panics in another ordered worker, as the sidecars' one, is told too.
        let worker = super::start_ordered_worker();
        let (done, wait) = std::sync::mpsc::channel();
        let told = super::told_here();
        worker
            .send(Box::new(move || {
                super::TOLD_FOR.set(Some(told));
                panic!("a broken sidecar")
            }))
            .unwrap();
        worker
            .send(Box::new(move || done.send(()).unwrap()))
            .unwrap();
        wait.recv().unwrap();
        assert_eq!(super::take_saves_failed().len(), 1);
    }

    /// A write in the user's folders that waits for a share that stopped answering does not
    /// hold the settings and marks written after it (#103). Before, one thread did both.
    #[test]
    fn a_stalled_write_in_the_user_folders_does_not_hold_glimts_own_files() {
        let (release, stalled) = std::sync::mpsc::channel::<()>();
        super::write_later_in_user_folders(move || {
            let _ = stalled.recv_timeout(std::time::Duration::from_secs(5));
        });
        let (done, own_write) = std::sync::mpsc::channel();
        super::write_later(move || {
            let _ = done.send(());
        });
        let own_write_is_done = own_write
            .recv_timeout(std::time::Duration::from_secs(2))
            .is_ok();
        release.send(()).unwrap();
        super::flush();
        assert!(own_write_is_done);
    }

    /// A write still running when `flush` stops waiting is told. Before, Glimt ended without
    /// a word, and the change was lost unseen.
    #[test]
    fn a_write_that_flush_does_not_wait_for_is_told() {
        super::take_saves_failed();
        let (release, wait) = std::sync::mpsc::channel::<()>();
        super::write_later(move || {
            let _ = wait.recv_timeout(std::time::Duration::from_secs(5));
        });
        super::flush_within(std::time::Duration::from_millis(50));
        assert_eq!(super::take_saves_failed().len(), 1);
        release.send(()).unwrap();
        super::flush();
        assert!(super::take_saves_failed().is_empty());
    }

    /// A file reached through a link is written where the link points; the link stays.
    #[test]
    fn writes_through_links() {
        let dir = std::env::temp_dir().join(format!(
            "glimt-test-writes-through-links-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let (real, link) = (dir.join("real"), dir.join("config"));
        std::fs::write(&real, "old").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        super::replace_file(&link, "new").unwrap();
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "new");
        // A file kept private stays private when written again.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
        super::replace_file(&real, "newer").unwrap();
        let mode = std::fs::metadata(&real).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A write asked for is counted until its thread has done it, and `flush` waits for it;
    /// one that panics is counted down too.
    #[test]
    fn counts_writes_until_done() {
        use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
        // The work is done, and `flush` is back long before its 5 s. It is not back while the
        // writer that panics is still counted.
        let done = std::sync::Arc::new(AtomicBool::new(false));
        let d = done.clone();
        super::write_later(move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            d.store(true, Relaxed);
        });
        super::write_later(|| panic!("a writer that breaks"));
        assert!(super::WRITING.load(Relaxed) >= 1);
        let t = std::time::Instant::now();
        super::flush();
        assert!(done.load(Relaxed));
        assert!(t.elapsed() < std::time::Duration::from_secs(4));
    }

    /// A write that another test's thread asked for is not counted on this test's thread.
    /// Before, the tests shared one count, and a test that waited for its own writes to end
    /// (`flush`, a close) also waited for the writes of the tests beside it, and timed out.
    #[test]
    fn a_write_of_another_test_is_not_counted_here() {
        use std::sync::atomic::Ordering::Relaxed;
        let (counted, wait) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel::<()>();
        let other = std::thread::spawn(move || {
            let writing = crate::support::threads::Working::new(&super::WRITING);
            counted.send(super::WRITING.load(Relaxed)).unwrap();
            let _ = released.recv();
            drop(writing);
        });
        assert_eq!(wait.recv().unwrap(), 1);
        assert_eq!(super::WRITING.load(Relaxed), 0);
        release.send(()).unwrap();
        other.join().unwrap();
    }

    /// A file deleted while the step before the rename runs (a sidecar written under its lock)
    /// stays deleted. Before, the new version was renamed in under its old name.
    #[test]
    fn a_file_gone_during_the_step_before_stays_gone() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-gone-before-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.png");
        std::fs::write(&file, b"old").unwrap();
        let gone = || std::fs::remove_file(&file);
        let read = super::fingerprint(&file).unwrap();
        assert!(super::replace_existing(&file, b"new", read, gone).is_err());
        assert!(!file.exists());
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            0,
            "a temporary file was left"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A read-only file, or one of another user, is not replaced by a saved turn, a sidecar or
    /// Save As, though the folder may be written. A link at the name is still replaced, and the
    /// read-only file it points to is left alone.
    #[test]
    fn a_read_only_file_is_not_replaced() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("glimt-test-ro-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.jpg");
        std::fs::write(&file, "old").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o444)).unwrap();
        let read = super::fingerprint(&file).unwrap();
        for result in [
            super::replace_existing(&file, "new", read, || Ok(())),
            super::replace_name(&file, "new"),
        ] {
            let refused = result.unwrap_err().kind();
            assert_eq!(refused, std::io::ErrorKind::PermissionDenied);
        }
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "old");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "no tmp left");
        let link = dir.join("b.jpg");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        super::replace_name(&link, "new").unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "old");
        assert_eq!(std::fs::read_to_string(&link).unwrap(), "new");
        // Glimt's own files take a change as before.
        super::replace_file(&file, "own").unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "own");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A file deleted (or moved) while its new version was being made is not made again under
    /// its old name; one still there is replaced as usual.
    #[test]
    fn a_late_save_lands_only_where_the_file_still_is() {
        let dir = std::env::temp_dir().join(format!("glimt-test-late-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.png");
        std::fs::write(&file, "old").unwrap();
        let read = super::fingerprint(&file).unwrap();
        assert!(super::replace_existing(&file, "new", read, || Ok(())).is_ok());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "new");
        let read = super::fingerprint(&file).unwrap();
        std::fs::remove_file(&file).unwrap();
        let late = super::replace_existing(&file, "late", read, || Ok(()));
        assert_eq!(late.unwrap_err().kind(), std::io::ErrorKind::NotFound);
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            0,
            "nothing made, no tmp left"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A file that changed after it was read is not written over, and the save says why. A
    /// JPEG still being copied in grows, and another window may give the name to another file.
    /// Before, the turn of the part read was renamed over it, and the copier wrote on into the
    /// file taken away, which left the folder a cut-off photo.
    #[test]
    fn a_file_changed_after_its_read_is_not_written_over() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("glimt-test-changed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.jpg");
        let other = dir.join("b.jpg");
        std::fs::write(&file, "part").unwrap();
        std::fs::write(&other, "else").unwrap();
        let read = super::fingerprint(&file).unwrap();
        let grows = || {
            let mut copier = std::fs::OpenOptions::new().append(true).open(&file)?;
            copier.write_all(b" and the rest")
        };
        let refused = super::replace_existing(&file, "turned", read, grows).unwrap_err();
        assert!(refused.to_string().contains("changed"), "{refused}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "part and the rest");
        let read = super::fingerprint(&file).unwrap();
        let swapped = || std::fs::rename(&other, &file);
        assert!(super::replace_existing(&file, "turned", read, swapped).is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "else");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "no tmp left");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn writes_atomically() {
        let dir = std::env::temp_dir().join(format!("glimt-test-atomic-{}", std::process::id()));
        let path = dir.join("config");
        super::write_atomic(&path, "a=1\n");
        super::write_atomic(&path, "a=2\n");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a=2\n");
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            1,
            "no .tmp files left"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Links planted where the lock and the temporary file go empty nothing they point to.
    #[test]
    fn planted_links_empty_nothing() {
        use std::os::unix::fs::symlink;
        let dir = std::env::temp_dir().join(format!("glimt-test-planted-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (victim, photo) = (dir.join("victim"), dir.join("trip.gqv"));
        std::fs::write(&victim, "keep").unwrap();
        std::fs::write(&photo, "old").unwrap();
        symlink(&victim, dir.join(".trip.gqv.lock")).unwrap();
        assert!(super::file_lock(&photo).is_none(), "locked through a link");
        let tmp = super::tmp_path(&photo);
        symlink(&victim, &tmp).unwrap();
        drop(super::create_temp_beside(&tmp, &photo).unwrap());
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A shared file whose lock cannot be had (NFS without lockd) is still edited, and the user
    /// is told that the edit runs unlocked. Before, the edit went on without a word, and a
    /// change that another window made at the same moment could be lost unseen.
    #[test]
    fn an_edit_without_its_lock_is_told() {
        let dir = std::env::temp_dir().join(format!("glimt-test-no-lock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("marks");
        // A folder where the lock file goes cannot be opened as one, as a share without locks.
        std::fs::create_dir_all(dir.join(".marks.lock")).unwrap();
        super::take_saves_failed();
        let edited = super::edit_shared(&file, |_| (b"new".to_vec(), ()));
        assert!(edited.is_ok());
        assert_eq!(std::fs::read(&file).unwrap(), b"new");
        let failed = super::take_saves_failed();
        assert!(
            !failed.iter().any(|e| e.starts_with("Could not lock")),
            "an edit that was made is told as a failed write: {failed:?}"
        );
        let warning = super::take_lock_warning().unwrap_or_default();
        assert!(warning.starts_with("Could not lock"), "{warning:?}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The warning of an edit without its lock is told once for each folder of locks, so a lock
    /// planted beside a downloaded collection does not keep the warning of Glimt's own files, or
    /// of another folder, from being told. Before, one warning for each Glimt covered every kind
    /// of lock, and the planted one used it up.
    #[test]
    fn a_planted_lock_does_not_hide_the_warning_of_another() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-lock-folders-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (downloads, own) = (dir.join("downloads"), dir.join("state"));
        std::fs::create_dir_all(downloads.join(".trip.gqv.lock")).unwrap();
        std::fs::create_dir_all(downloads.join(".other.gqv.lock")).unwrap();
        std::fs::create_dir_all(own.join(".marks.lock")).unwrap();
        super::take_lock_warning();
        assert!(super::file_lock(&downloads.join("trip.gqv")).is_none());
        let first = super::take_lock_warning().unwrap_or_default();
        assert!(first.contains("trip.gqv"), "{first:?}");
        assert!(super::file_lock(&downloads.join("other.gqv")).is_none());
        assert_eq!(
            super::take_lock_warning(),
            None,
            "told twice for one folder"
        );
        assert!(super::file_lock(&own.join("marks")).is_none());
        let other = super::take_lock_warning().unwrap_or_default();
        assert!(other.contains(".marks.lock"), "{other:?}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A lock file planted beside a collection that came with a download is never waited on,
    /// and never made where a dangling link points. Before, a pipe there held the thread for
    /// good, and the link made an empty file at its target.
    #[test]
    fn a_planted_pipe_or_dangling_link_is_no_lock() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-planted-lock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (with_pipe, with_link) = (dir.join("a.gqv"), dir.join("b.gqv"));
        let made = std::process::Command::new("mkfifo")
            .arg(dir.join(".a.gqv.lock"))
            .status()
            .unwrap();
        assert!(made.success());
        let target = dir.join("never-made");
        std::os::unix::fs::symlink(&target, dir.join(".b.gqv.lock")).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let pipe_locked = super::file_lock(&with_pipe).is_some();
            let link_locked = super::file_lock(&with_link).is_some();
            tx.send((pipe_locked, link_locked)).unwrap();
        });
        let locked = rx.recv_timeout(std::time::Duration::from_secs(5));
        assert_eq!(
            locked,
            Ok((false, false)),
            "waited on the pipe or locked through the link"
        );
        assert!(!target.exists(), "the link's target was made");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A plan or log is locked open for writing, which the Linux NFS client needs for a flock.
    /// Before, it was opened to be read only, and on an NFS home every plan and log looked held
    /// by another window. A length set needs a file open for writing. The file is not changed,
    /// a second open finds it held, and a name that is not there is not made.
    #[test]
    fn a_file_is_locked_open_for_writing() {
        let dir = std::env::temp_dir().join(format!("glimt-test-locked-rw-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let plan = dir.join("plan");
        std::fs::write(&plan, "steps").unwrap();
        let held = super::open_if_unlocked(&plan, false).unwrap();
        held.set_len(5).expect("the file is not open for writing");
        assert_eq!(std::fs::read(&plan).unwrap(), b"steps");
        let again = super::open_if_unlocked(&plan, false).map(|_| ());
        assert_eq!(again.unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
        let gone = super::open_if_unlocked(&dir.join("gone"), false).map(|_| ());
        assert_eq!(gone.unwrap_err().kind(), std::io::ErrorKind::NotFound);
        assert!(!dir.join("gone").exists());
        drop(held);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A temporary name holds a number of this process's own after its ID: two Glimts with the
    /// same process ID (on two computers with one home) never take each other's.
    #[test]
    fn temporary_names_are_this_process_own() {
        let path = std::path::Path::new("/x/a.png");
        let (a, b) = (super::tmp_path(path), super::tmp_path(path));
        assert_ne!(a, b);
        let name = a.file_name().unwrap().to_str().unwrap();
        let rest = name.strip_prefix(".glimt-tmp-").unwrap();
        let rest = rest
            .strip_prefix(crate::features::remote::instance_id())
            .unwrap();
        let own = rest.strip_prefix('-').unwrap().split('-').next().unwrap();
        assert!(
            own.len() == 8 && own.chars().all(|c| c.is_ascii_hexdigit()),
            "{name}"
        );
        assert_eq!(a.parent(), path.parent());
    }
}
