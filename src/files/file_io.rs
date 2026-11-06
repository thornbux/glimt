//! The few ways Glimt changes files on disk, each kept to one function that clippy.toml makes
//! the only way. A file is written whole or not at all (`replace_file`, `create_file`,
//! `write_atomic`), and a file the windows share is locked, read, changed and written as one
//! (`edit_shared`). A file is renamed never over another (`rename_never_over`), and moved or
//! copied whole or not at all (`put_file`). Glimt's own files are written in one thread, in
//! order (`write_later`), and `flush` waits for them before Glimt ends.

use std::path::{Path, PathBuf};

/// A file Glimt could not save, for the window to tell: from any thread.
static SAVE_ERROR: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Everything being written for the user, counted by a `Working` held while it runs: the
/// settings and marks (`write_later`), sidecars, file jobs, a turned image, a print. The one
/// count closing waits for, so that no writer can be forgotten.
pub static WRITING: std::sync::LazyLock<std::sync::Arc<std::sync::atomic::AtomicUsize>> =
    std::sync::LazyLock::new(Default::default);

/// Work for a `serial` thread.
pub type Work = Box<dyn FnOnce() + Send>;

/// Rename `from` to `to`, never over a file or folder there, also one another window put there
/// a moment ago: in one step where the file system can (RENAME_NOREPLACE); where it cannot (FAT,
/// NTFS or exFAT through FUSE, some shares), a link, which never replaces a file, and then the
/// old name unlinked; where links cannot be made either, a rename after looking first. The one
/// way Glimt renames: a file moved, a copy or a new file given its name, a folder, a collection,
/// a file back from the trash. `CrossesDevices` is the caller's to copy instead.
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
            Err(_) => std::fs::rename(from, to).map_err(|e| plain(e, to)),
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

/// Returns `e` told in plain words when it says that the file system cannot rename a file to
/// `to` as Glimt does, which some shares, cloud folders and phones cannot. The words name the
/// type of the mount (fuse.rclone, cifs …), and the system's own words follow in brackets. The
/// kind stays, so callers still match on it.
fn plain(e: std::io::Error, to: &Path) -> std::io::Error {
    // EXDEV, ENOSYS and EOPNOTSUPP are what such file systems answer to a rename. EPERM is not
    // among them, because a folder with the sticky bit or a file made immutable gives it on any
    // file system.
    match e.raw_os_error() {
        Some(18 | 38 | 95) => {
            let kind = mount_type(to.parent().unwrap_or(to)).unwrap_or_else(|| "?".into());
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

/// Returns the type of the file system that `path` is on, as /proc/self/mounts names it: vfat,
/// cifs, nfs4, fuse.rclone, fuse.gvfsd-fuse and so on. Returns `None` when that cannot be read.
/// The path is taken as it is, not with its links resolved, as that would wait on a share that
/// does not answer.
pub(crate) fn mount_type(path: &Path) -> Option<String> {
    let mounts = crate::images::decode::read_limited("/proc/self/mounts").ok()?;
    mount_type_in(&mounts, path)
}

/// Returns whether a mount of type `kind` (from `mount_type`) is on another computer or in a
/// cloud, through FUSE, SMB or NFS. A fuseblk mount (NTFS or exFAT on a local disk) is not.
/// Every other FUSE mount counts, also a local one such as gocryptfs, so the delete dialog does
/// not call the folder a network folder.
pub(crate) fn is_remote(kind: &str) -> bool {
    kind.starts_with("fuse.") || matches!(kind, "cifs" | "smb3" | "nfs" | "nfs4" | "9p")
}

/// Returns the type of the mount in `mounts`, the bytes of /proc/self/mounts, that holds `path`.
/// That is the last mount whose mount point is the longest start of the path, as a later mount
/// hides an earlier one at the same place. The text is read as bytes, because a mount point may
/// have a name that is not UTF-8.
fn mount_type_in(mounts: &[u8], path: &Path) -> Option<String> {
    let mut found: Option<(usize, &[u8])> = None;
    for line in mounts.split(|&b| b == b'\n') {
        let mut fields = line.split(|&b| b == b' ').skip(1);
        let (Some(at), Some(kind)) = (fields.next(), fields.next()) else {
            continue;
        };
        let at = unescape_mount(at);
        let len = at.as_os_str().len();
        if path.starts_with(&at) && found.is_none_or(|(longest, _)| len >= longest) {
            found = Some((len, kind));
        }
    }
    found.map(|(_, kind)| String::from_utf8_lossy(kind).into_owned())
}

/// Returns the mount point `at` of /proc/self/mounts as a path. The kernel writes a space, a tab,
/// a line break and a backslash in it as a backslash and three octal digits.
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
        // Its time kept, as mv does (sorting by date), and on disk before a move removes the
        // source: a stick pulled out right after must not lose both.
        let copied = crate::images::decode::open(from)
            .and_then(|mut f| std::io::copy(&mut f, &mut out))
            .and_then(|_| {
                // Its read, write and run bits, as cp gives them: not set-user-ID, set-group-ID
                // or sticky, which a copy owned by this user would carry for its owner.
                let meta = std::fs::metadata(from)?;
                let mode = std::fs::Permissions::from_mode(meta.mode() & 0o777);
                std::fs::set_permissions(&part, mode)?;
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
    replace_file(path, new)?;
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

/// The last file Glimt could not save, once.
pub fn take_save_error() -> Option<String> {
    SAVE_ERROR.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// `work` in the one thread that writes Glimt's own files, in the order given.
pub fn write_later(work: impl FnOnce() + Send + 'static) {
    static WRITER: std::sync::OnceLock<std::sync::mpsc::Sender<Work>> = std::sync::OnceLock::new();
    // Dropped with the work, also when it panics or is never run: Glimt would wait for it at exit.
    let writing = crate::support::threads::Working::new(&WRITING);
    let counted = move || {
        let _writing = writing;
        let _ = crate::support::threads::caught(work);
    };
    let _ = WRITER.get_or_init(serial).send(Box::new(counted));
}

/// A thread that does the work sent to it one after another, in that order. One that panics
/// does not end it, or every later work sent would be lost.
pub fn serial() -> std::sync::mpsc::Sender<Work> {
    let (works, rx) = std::sync::mpsc::channel::<Work>();
    std::thread::spawn(move || {
        for work in rx {
            let _ = crate::support::threads::caught(work);
        }
    });
    works
}

/// Wait for everything being written, a few seconds at most, before Glimt ends: the last
/// change of a setting or a keyword must not be lost with its thread.
pub fn flush() {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while WRITING.load(std::sync::atomic::Ordering::Relaxed) > 0
        && std::time::Instant::now() < until
    {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
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
    replace(path, bytes, false)
}

/// `replace_file` for a file of the user's that must still be there: a turned image, whose
/// save takes a while. Deleted, moved or renamed meanwhile, it is not made again under its old
/// name by the save landing late; the save is refused instead. The one place this holds for
/// every operation, whether or not it asked `none_saving` first.
pub fn replace_existing(path: &Path, bytes: impl AsRef<[u8]>) -> std::io::Result<()> {
    replace(path, bytes, true)
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
    #[allow(clippy::disallowed_methods)] // replaces on purpose, the name and never a link's target
    let land =
        |tmp: &Path| before().and_then(|_| std::fs::rename(tmp, path).map_err(|e| plain(e, path)));
    write_beside(&tmp_path(path), path, write, land)
}

/// Writes `bytes` to `path` beside it and renames it, through a link to the file it points to.
/// Glimt's own files and a turned image are written so. A name the user chose for a new file,
/// and a collection's file, go through `replace_name`, which never follows a link.
fn replace(path: &Path, bytes: impl AsRef<[u8]>, existing: bool) -> std::io::Result<()> {
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
    #[allow(clippy::disallowed_methods)] // replaces on purpose: the one writer of a file again
    write_beside(&tmp_path(path), path, write, |tmp| {
        there().and_then(|_| std::fs::rename(tmp, path).map_err(|e| plain(e, path)))
    })
}

/// A file written whole at `tmp` beside `path` (`new_beside`), on disk, and then given its name
/// by `land`; `tmp` removed when anything fails. On disk before the rename: after a crash the
/// old file or the new one, never an empty one. No folder made: a sidecar or a collection whose
/// folder was renamed meanwhile is not written into a new one of the old name. The one
/// temp-and-rename for every file Glimt writes: settings, sidecars, collections, a PDF, a
/// thumbnail.
pub fn write_beside(
    tmp: &Path,
    path: &Path,
    write: impl FnOnce(&mut std::fs::File) -> std::io::Result<()>,
    land: impl FnOnce(&Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let result = new_beside(tmp, path)
        .and_then(|mut f| write(&mut f).and_then(|_| f.sync_all()))
        .and_then(|_| land(tmp));
    if result.is_err() {
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
    path.with_file_name(format!(".glimt-tmp-{}-{k}", own_tag()))
}

/// This Glimt's instance ID and a random number of its own, for the names of its temporary
/// files: two Glimts with one process ID (two computers with one home, two containers) differ.
pub fn own_tag() -> &'static str {
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
pub fn new_beside(tmp: &Path, path: &Path) -> std::io::Result<std::fs::File> {
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
    *SAVE_ERROR.lock().unwrap_or_else(|e| e.into_inner()) = Some(error);
}

#[cfg(test)]
mod tests {
    /// A rename the file system cannot do is told in plain words with the system's own after
    /// them, and keeps its kind. Another error, such as a permission denied, stays as it was.
    #[test]
    fn tells_a_rename_the_file_system_cannot_do_in_plain_words() {
        let to = std::env::temp_dir().join("b.jpg");
        let kind = super::mount_type(&std::env::temp_dir()).unwrap();
        for errno in [18, 38, 95] {
            let raw = std::io::Error::from_raw_os_error(errno);
            let told = super::plain(std::io::Error::from_raw_os_error(errno), &to);
            assert_eq!(told.kind(), raw.kind());
            let text = told.to_string();
            assert!(text.contains("does not allow this rename") && text.contains(&raw.to_string()));
            assert!(text.contains(&format!("({kind})")), "{text}");
        }
        // Permission denied, and EPERM, which a sticky folder or an immutable file gives.
        for errno in [13, 1] {
            let denied = super::plain(std::io::Error::from_raw_os_error(errno), &to);
            assert_eq!(denied.raw_os_error(), Some(errno));
        }
    }

    /// The mount that holds a path is the longest mount point that starts it, the later one
    /// where two are at the same place, and a mount point with a space in it is read as the
    /// kernel writes it. A mount point that only starts with the same letters does not hold it,
    /// and one whose name is not UTF-8 is found too.
    #[test]
    fn finds_the_mount_that_holds_a_path() {
        use std::os::unix::ffi::OsStrExt;
        let mounts = b"/dev/sda1 / ext4 rw 0 0\n\
            /dev/sdc1 /media/me/K\xf8RT vfat rw 0 0\n\
            gvfsd-fuse /run/user/1000/gvfs fuse.gvfsd-fuse rw 0 0\n\
            /dev/sdb1 /media/me/CARD\\040ONE vfat rw 0 0\n\
            remote: /mnt/cloud fuse.rclone rw 0 0\n\
            //nas/photos /mnt/cloud cifs rw 0 0\n\
            broken\n";
        let kind = |p: &str| super::mount_type_in(mounts, std::path::Path::new(p));
        assert_eq!(kind("/home/me/a.jpg").as_deref(), Some("ext4"));
        let gvfs = "/run/user/1000/gvfs/sftp:host=nas/a.jpg";
        assert_eq!(kind(gvfs).as_deref(), Some("fuse.gvfsd-fuse"));
        assert_eq!(kind("/media/me/CARD ONE/DCIM").as_deref(), Some("vfat"));
        assert_eq!(kind("/media/me/CARD").as_deref(), Some("ext4"));
        assert_eq!(kind("/mnt/cloud/a.jpg").as_deref(), Some("cifs"));
        assert_eq!(kind("/mnt/cloudy").as_deref(), Some("ext4"));
        let latin1 = std::path::Path::new(std::ffi::OsStr::from_bytes(b"/media/me/K\xf8RT/a.jpg"));
        assert_eq!(
            super::mount_type_in(mounts, latin1).as_deref(),
            Some("vfat")
        );
        assert!(super::is_remote("fuse.rclone") && super::is_remote("cifs"));
        assert!(!super::is_remote("fuseblk") && !super::is_remote("vfat"));
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
    #[test]
    fn writer_survives_a_panic() {
        use std::sync::atomic::{AtomicBool, Ordering};
        static DONE: AtomicBool = AtomicBool::new(false);
        super::write_later(|| panic!("a broken write"));
        super::write_later(|| DONE.store(true, Ordering::Relaxed));
        super::flush();
        assert!(DONE.load(Ordering::Relaxed));
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
        // Other tests write at the same time, so the count itself proves little after `flush`:
        // the work done, and `flush` back long before its 5 s, which it is not while the
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

    /// A file deleted (or moved) while its new version was being made is not made again under
    /// its old name; one still there is replaced as usual.
    #[test]
    fn a_late_save_lands_only_where_the_file_still_is() {
        let dir = std::env::temp_dir().join(format!("glimt-test-late-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.png");
        std::fs::write(&file, "old").unwrap();
        assert!(super::replace_existing(&file, "new").is_ok());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "new");
        std::fs::remove_file(&file).unwrap();
        let late = super::replace_existing(&file, "late");
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
        drop(super::new_beside(&tmp, &photo).unwrap());
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
