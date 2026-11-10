//! The few ways Glimt changes files on disk, each kept to one function that clippy.toml makes
//! the only way. A file is written whole or not at all (`replace_file`, `create_file`,
//! `write_atomic`), and a file the windows share is locked, read, changed and written as one
//! (`edit_shared`). A file is renamed never over another (`rename_never_over`), and moved or
//! copied whole or not at all (`put_file`). A rename that later steps count on is on disk with
//! its folder (`sync_folder_of`) before they run. Glimt's own files are written in one thread, in
//! order (`write_later`), and `flush` waits for them before Glimt ends.

use crate::support::threads::lock_even_if_poisoned;
use std::path::{Path, PathBuf};

/// A file Glimt could not save, or a note about a save, for the window to tell: from any thread.
static SAVE_ERROR: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

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
    // The errors that `save_failed` told for this thread's test. They do not go to the window's
    // own slot in tests, where another test's window would take them as its notice. Each list is
    // leaked, so that the writer thread can tell a test's errors to that test's list.
    static SAVES_FAILED: &'static std::sync::Mutex<Vec<String>> = Box::leak(Box::default());
    // The list of the test whose work the writer thread is doing now, so that what that work
    // tells reaches the test that queued it. It is `None` on every other thread.
    static TOLD_FOR: std::cell::Cell<Option<&'static std::sync::Mutex<Vec<String>>>> =
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

/// A write queued for a `start_ordered_worker` thread.
pub type QueuedWrite = Box<dyn FnOnce() + Send>;

/// A line of /proc/self/mountinfo: one mount.
struct Mount<'a> {
    id: &'a [u8],
    /// The id of the mount this one is on.
    parent: &'a [u8],
    /// The device's numbers, as major:minor.
    dev: &'a [u8],
    /// Where it is mounted, as a path.
    at: PathBuf,
    /// Its file system's type, such as ext4, vfat or fuse.rclone.
    kind: &'a [u8],
}

/// Rename `from` to `to`, never over a file or folder there, also one another window put there
/// a moment ago: in one step where the file system can (RENAME_NOREPLACE); where it cannot (FAT,
/// NTFS or exFAT through FUSE, some shares), a link, which never replaces a file, and then the
/// old name unlinked; where links cannot be made either, a rename after looking first. The one
/// way Glimt renames: a file moved, a copy or a new file given its name, a folder, a collection,
/// a file back from the trash. `CrossesDevices` is the caller's to copy instead. The rename
/// is not synced to disk here. One rename alone is one step, so a power cut leaves the file
/// under its old name or its new one. A caller whose next step counts on the new name being on
/// disk (an unlink of the source, a plan removed) syncs the folder with `sync_folder_of` first.
#[allow(clippy::disallowed_methods)] // its last fallback, a rename after looking
pub(crate) fn rename_never_over(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::io::ErrorKind::{AlreadyExists, CrossesDevices, Unsupported};
    match rename_noreplace(from, to) {
        Err(e) if e.kind() == Unsupported => match std::fs::hard_link(from, to) {
            Ok(()) => unlink_linked(from, to),
            Err(e) if matches!(e.kind(), AlreadyExists | CrossesDevices) => Err(e),
            Err(_) if to.exists() => Err(AlreadyExists.into()),
            // shortcut: a file that another program puts at `to` between the look and the
            // rename is written over. rclone, s3fs and MTP offer no way to rename without that
            // race; do more if one of them gains RENAME_NOREPLACE or hard links.
            Err(_) => std::fs::rename(from, to).map_err(|e| explain_rename_error(e, to)),
        },
        done => done,
    }
}

/// `from`, now also at `to`, unlinked: a move is all or nothing, so one that cannot be removed
/// (a shared folder with the sticky bit, someone else's file) has its new name taken away
/// again. One gone after all (deleted or moved in another window, or a share that said no but
/// did it) is done: the new name may be the only one left.
fn unlink_linked(from: &Path, to: &Path) -> std::io::Result<()> {
    #[allow(clippy::disallowed_methods)] // the old name, linked at its new one
    match std::fs::remove_file(from) {
        Err(e) if std::fs::symlink_metadata(from).is_ok() => {
            #[allow(clippy::disallowed_methods)] // the new name, taken back
            let _ = std::fs::remove_file(to);
            Err(e)
        }
        _ => Ok(()),
    }
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
    #[cfg(test)]
    FOLDERS_SYNCED.with_borrow_mut(|synced| synced.push(folder.to_owned()));
    #[cfg(test)]
    if FOLDER_SYNC_FAILS.get() {
        return Err(std::io::Error::from_raw_os_error(libc::EIO));
    }
    #[allow(clippy::disallowed_methods)] // a folder, opened only to sync it
    let synced = std::fs::File::open(folder).and_then(|f| f.sync_all());
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
    let kind = udev.lines().find_map(|l| l.strip_prefix("E:ID_FS_TYPE="))?;
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
    let mounts: Vec<Mount> = (mountinfo.split(|&b| b == b'\n'))
        .filter_map(mount_line)
        .collect();
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
    let found = found?;
    (found.kind == b"fuseblk")
        .then(|| device_type(found.dev))
        .flatten()
        .or_else(|| Some(String::from_utf8_lossy(found.kind).into_owned()))
}

/// Returns a line of /proc/self/mountinfo as a `Mount`. Returns `None` for a line that is not
/// whole. The fields are the mount's id, its parent's id, the device, the folder of the file
/// system that is mounted, the mount point and the mount's options. Optional fields follow,
/// ended by a lone "-", and then the type.
fn mount_line(line: &[u8]) -> Option<Mount<'_>> {
    let mut fields = line.split(|&b| b == b' ');
    let (id, parent, dev) = (fields.next()?, fields.next()?, fields.next()?);
    let at = unescape_mount(fields.nth(1)?);
    let kind = fields.skip_while(|f| *f != b"-").nth(1)?;
    Some(Mount {
        id,
        parent,
        dev,
        at,
        kind,
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
    #[allow(unsafe_code)] // declarations only: each call is an unsafe block
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
    #[allow(unsafe_code)]
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

/// Copy (`is_copy`) or move `from` to `to`, never over a file there, also one another window put
/// there a moment ago: a copy is written under a hidden name and then renamed, a move is a
/// rename, both with `rename_never_over`; a move to another file system is a copy and an unlink.
/// Only that move syncs the copy's folder, before its source is removed. A copy alone leaves
/// the source, so nothing is lost when its name does not reach the disk, and a folder sync for
/// each file would make a copy of many small files to a USB stick up to twice as slow.
pub(crate) fn put_file(from: &Path, to: &Path, is_copy: bool) -> std::io::Result<()> {
    use std::io::ErrorKind::{AlreadyExists, CrossesDevices};
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    let copy = || -> std::io::Result<()> {
        if to.exists() {
            return Err(AlreadyExists.into());
        }
        // Written under a hidden name and given its own when whole: a copy cut short (a full
        // disk, Glimt closed, a stick pulled out) never looks like the file.
        // shortcut: a third close (which waits for nothing) or a stick pulled out leaves the
        // part under this hidden name, and nothing removes it later; a sweep of
        // .glimt-tmp-* at the next start if those are ever seen in numbers.
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
            .and_then(|mut f| std::io::copy(&mut f, &mut out))
            .and_then(|_| {
                // Its read, write and run bits, as cp gives them: not set-user-ID, set-group-ID
                // or sticky, which a copy owned by this user would carry for its owner.
                let meta = std::fs::metadata(from)?;
                let mode = std::fs::Permissions::from_mode(meta.mode() & 0o777);
                #[allow(clippy::disallowed_methods)] // the copy's hidden part, made new
                std::fs::set_permissions(&part, mode)?;
                #[allow(clippy::disallowed_methods)] // the copy's time, as mv keeps it
                out.set_modified(meta.modified()?)?;
                out.sync_all()
            })
            .and_then(|_| rename_never_over(&part, to));
        #[allow(clippy::disallowed_methods)] // the copy's hidden part
        let _ = std::fs::remove_file(&part);
        copied
    };
    if is_copy {
        return copy();
    }
    match rename_never_over(from, to) {
        // The copy is on disk with its name before the source goes: a stick pulled out right
        // after must not lose both.
        Err(e) if e.kind() == CrossesDevices => (copy())
            .and_then(|_| sync_folder_of(to))
            .and_then(|_| unlink_linked(from, to)),
        done => done,
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
/// window's change cannot land after it and bring back what was cleared.
pub fn remove_shared(path: &Path) {
    if std::fs::symlink_metadata(path).is_err() {
        return;
    }
    #[allow(clippy::disallowed_methods)] // as edit_shared
    let _held = file_lock(path);
    #[allow(clippy::disallowed_methods)] // a shared file of Glimt's own
    let _ = std::fs::remove_file(path);
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

/// The last file Glimt could not save, once. Otherwise the note of the saves over files with
/// other names (hard links), once no file is being written any more. A job over many files then
/// gives one note with their count, rather than a new one for each file every frame.
pub fn take_save_error() -> Option<String> {
    let error = lock_even_if_poisoned(&SAVE_ERROR).take();
    let is_writing = WRITING.load(std::sync::atomic::Ordering::Relaxed) > 0;
    error.or_else(|| (!is_writing).then(other_names_note).flatten())
}

/// Returns the note of the saves over files with other names (hard links), and counts anew.
/// Returns `None` in tests, which read their own thread's count.
fn other_names_note() -> Option<String> {
    #[cfg(not(test))]
    return lock_even_if_poisoned(&OTHER_NAMES).take_note();
    #[cfg(test)]
    None
}

/// `work` in the one thread that writes Glimt's own files, in the order given.
pub fn write_later(work: impl FnOnce() + Send + 'static) {
    static WRITER: std::sync::OnceLock<std::sync::mpsc::Sender<QueuedWrite>> =
        std::sync::OnceLock::new();
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
    let _ = WRITER
        .get_or_init(start_ordered_worker)
        .send(Box::new(counted));
}

/// A thread that does the work sent to it one after another, in that order. One that panics
/// does not end it, or every later work sent would be lost.
pub fn start_ordered_worker() -> std::sync::mpsc::Sender<QueuedWrite> {
    let (works, rx) = std::sync::mpsc::channel::<QueuedWrite>();
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

/// Waits until the writes queued before this call are done, five seconds at most. Unlike
/// `flush`, it does not also wait for the writes that other work queues after it.
pub fn wait_for_queued_writes() {
    let (done, wait) = std::sync::mpsc::channel();
    write_later(move || {
        let _ = done.send(());
    });
    let _ = wait.recv_timeout(std::time::Duration::from_secs(5));
}

/// The lock beside `path`, held while it lives: another Glimt window's reading and writing of
/// the same file does not fall between ours, which would lose its change. The lock is a file of
/// its own, hidden beside it, as `path` is renamed over.
pub fn file_lock(path: &Path) -> Option<std::fs::File> {
    let name = path.file_name()?.to_string_lossy();
    // Never truncated: a lock file that is a link planted in a shared folder must not empty
    // the file it points to.
    #[allow(clippy::disallowed_methods)] // a lock file, never truncated
    let lock = (std::fs::OpenOptions::new().create(true).append(true))
        .open(path.with_file_name(format!(".{name}.lock")))
        .ok()?;
    lock.lock().ok()?;
    Some(lock)
}

/// Write to a temporary file and rename it, so the file is never half-written after a crash.
/// This Glimt's name is in it (in a Flatpak more than the process ID, which windows can share),
/// so concurrent sessions never write to the same temporary file.
pub fn replace_file(path: &Path, bytes: impl AsRef<[u8]>) -> std::io::Result<()> {
    replace(path, bytes, false, || Ok(()))
}

/// `replace_file` for a file of the user's that must still be there: a turned image, whose
/// save takes a while. Deleted, moved or renamed meanwhile, it is not made again under its old
/// name by the save landing late; the save is refused instead. The one place this holds for
/// every operation, whether or not it asked `are_free_to_change` first. The step `before` runs
/// after the file is found writable and still there and the new version is written whole, just
/// before the rename. The image's sidecar is changed there, so a write that fails (a read-only
/// file, a full disk) leaves the file and its sidecar as they were.
pub fn replace_existing(
    path: &Path,
    bytes: impl AsRef<[u8]>,
    before: impl FnOnce() -> std::io::Result<()>,
) -> std::io::Result<()> {
    replace(path, bytes, true, before)
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

/// Writes `bytes` to `path` beside it and renames it, through a link to the file it points to.
/// Glimt's own files and a turned image are written so. A name the user chose for a new file,
/// and a collection's file, go through `replace_name`, which never follows a link.
fn replace(
    path: &Path,
    bytes: impl AsRef<[u8]>,
    existing: bool,
    before: impl FnOnce() -> std::io::Result<()>,
) -> std::io::Result<()> {
    use std::io::Write;
    let write = |f: &mut std::fs::File| f.write_all(bytes.as_ref());
    // Through a link (dotfiles kept in a repository), the file it points to: the link stays.
    let path = &std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    let there = || match !existing || std::fs::symlink_metadata(path).is_ok() {
        true => Ok(()),
        false => Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            crate::support::lang::tr("the file was moved, renamed or deleted meanwhile"),
        )),
    };
    // shortcut: `there` is looked at just before the rename, not in the same step; a file that
    // goes in those microseconds comes back. renameat2 with RENAME_EXCHANGE if that is ever seen.
    there()?;
    let has_other_names = existing && check_writable(path)?;
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

/// Refuses to replace the file at `path` when this user may not write it: a photo made
/// read-only (0444), or another user's file. A replace renames a new file over the old one,
/// which needs only the folder's write bit, so without this check a read-only file would be
/// written over, where Geeqie, which writes in place, fails. A file without a write bit is
/// refused also for root, who may write any file, as its owner marked it so. A link at the name
/// passes, because the link itself is replaced, and so does a name that is not there yet.
/// Glimt's own files (`replace_file`) are not asked, as before. A turn saved asks it first too,
/// before the slow work of encoding the image. Returns whether the file has other names (hard
/// links), which the replace will not change.
pub(crate) fn check_writable(path: &Path) -> std::io::Result<bool> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    let Some(meta) = std::fs::symlink_metadata(path).ok().filter(|m| m.is_file()) else {
        return Ok(false);
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
    let result = create_temp_beside(tmp, path)
        .and_then(|mut f| write(&mut f).and_then(|_| f.sync_all()))
        .and_then(|_| give_name(tmp));
    if result.is_err() {
        #[allow(clippy::disallowed_methods)] // the temporary file, not finished
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
/// those before a byte is written: no one else can open it meanwhile. A new file as usual.
#[allow(clippy::disallowed_methods)] // the one way a new file is made, under its temporary name
pub fn create_temp_beside(tmp: &Path, path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    // Made new, never opened: a link left or planted at `tmp` would be followed and the file
    // it points to emptied.
    let _ = std::fs::remove_file(tmp);
    let new = || {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .clone()
    };
    let Ok(old) = std::fs::metadata(path) else {
        return new().open(tmp);
    };
    let file = new().mode(0o600).open(tmp)?;
    file.set_permissions(old.permissions())?;
    Ok(file)
}

/// `replace_file`, with a failure on stderr: for files Glimt keeps for itself.
pub fn write_atomic(path: &Path, bytes: impl AsRef<[u8]>) {
    let _ = std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")));
    if let Err(e) = replace_file(path, bytes) {
        save_failed(crate::tf!(
            "Could not save {name}: {e}",
            name = path.display(),
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
    lock_even_if_poisoned(told_here()).push(error);
    #[cfg(not(test))]
    {
        *lock_even_if_poisoned(&SAVE_ERROR) = Some(error);
    }
}

/// Returns the list of the errors told for this thread's test, also on the writer thread while
/// it does that test's work.
#[cfg(test)]
fn told_here() -> &'static std::sync::Mutex<Vec<String>> {
    TOLD_FOR
        .get()
        .unwrap_or_else(|| SAVES_FAILED.with(|told| *told))
}

/// Returns the errors that `save_failed` told for this thread's test, and empties the list.
#[cfg(test)]
pub(crate) fn take_saves_failed() -> Vec<String> {
    std::mem::take(&mut *lock_even_if_poisoned(told_here()))
}

#[cfg(test)]
mod tests {
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
        let dir = std::env::temp_dir().join(format!("glimt-test-link-{}", std::process::id()));
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
        assert!(super::replace_existing(&file, b"new", gone).is_err());
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
        for result in [
            super::replace_existing(&file, "new", || Ok(())),
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
        assert!(super::replace_existing(&file, "new", || Ok(())).is_ok());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "new");
        std::fs::remove_file(&file).unwrap();
        let late = super::replace_existing(&file, "late", || Ok(()));
        assert_eq!(late.unwrap_err().kind(), std::io::ErrorKind::NotFound);
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            0,
            "nothing made, no tmp left"
        );
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
        drop(super::file_lock(&photo));
        let tmp = super::tmp_path(&photo);
        symlink(&victim, &tmp).unwrap();
        drop(super::create_temp_beside(&tmp, &photo).unwrap());
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
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
