//! The trash: files moved to it (with gio, or by hand as freedesktop.org's trash spec says when
//! gio is not there or the Flatpak's trash portal refuses one), and put back from it for Undo by
//! reading the trash folders' own files.

use crate::files::file_io::rename_never_over;
use crate::{files::config, model::collections, tf};
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The marks and stars that a delete took away, for its Undo. The marks writer thread adds them
/// as it takes them out of the marks file, after the window's earlier edits are in the file.
pub(crate) type ForgottenMarks = std::sync::Arc<std::sync::Mutex<Vec<(PathBuf, (u8, u8))>>>;

/// What a move to the trash did, as it did it, for Undo to put back: the files or folder it
/// moved, the sidecars that went with them, when it began (the trash may hold older files of the
/// same names, which are not these), and what went with them elsewhere: their marks and stars,
/// and their places in the collection shown. Recorded as it happens, never found out later: by
/// the move's thread, and the marks by the marks writer when `receive_operation_report` forgets
/// them, which is the one way an `Undo::Trash` reaches `App::undo`. `since` has no default: one
/// left out would take an older file of the same name from the trash.
pub(crate) struct TrashMove {
    pub(crate) files: Vec<PathBuf>,
    pub(crate) sidecars: Vec<PathBuf>,
    /// The files' sidecars that the move could not take to the trash. They stay beside the
    /// files' names, and Undo puts each file back beside its own.
    pub(crate) sidecars_left: Vec<PathBuf>,
    pub(crate) since: std::time::SystemTime,
    pub(crate) marks: ForgottenMarks,
    pub(crate) collection: Option<(PathBuf, Vec<(usize, PathBuf)>)>,
}

/// A file in a trash folder: where it came from, when (as the trash writes it, which sorts by
/// time), and its file and its .trashinfo there.
#[derive(Clone)]
struct TrashEntry {
    from: PathBuf,
    date: String,
    file: PathBuf,
    info: PathBuf,
}

/// The trash that the files of one folder go to by hand, and how their .trashinfo names them.
struct TrashPlace {
    trash: PathBuf,
    /// The folder's real path from the top of its file system, which a trash there names its
    /// files from. It is `None` for the home trash, which names them by their absolute path.
    folder_from_top: Option<PathBuf>,
}

/// Takes the marks that a delete took away out of `marks`, for its Undo. The marks writer adds
/// them as it takes them out of the marks file, so this first waits for the writes queued
/// before it, as long as they take. A writer held by another window's lock or a share that
/// stalls must not let Undo bring the files back without their marks. Returns `None`, with
/// `marks` left as they are, when `is_stopped` says that the user stopped the wait.
pub(crate) fn take_forgotten_marks(
    marks: &ForgottenMarks,
    is_stopped: impl Fn() -> bool,
) -> Option<Vec<(PathBuf, (u8, u8))>> {
    use std::sync::mpsc::RecvTimeoutError::Timeout;
    let (done, wait) = std::sync::mpsc::channel();
    crate::files::file_io::write_later(move || {
        let _ = done.send(());
    });
    // A wait that ends without an answer (the writer thread is gone) waits no longer.
    while let Err(Timeout) = wait.recv_timeout(std::time::Duration::from_millis(100)) {
        if is_stopped() {
            return None;
        }
    }
    Some(std::mem::take(
        &mut *crate::support::threads::lock_even_if_poisoned(marks),
    ))
}

/// What `trash_move` moved to the trash put back where it was, with the sidecars that went with it:
/// of each, the first of its name put in the trash folders `dirs_of` gives (the home trash, and
/// those at the top of its file system) since the move began, as an older one of the same name is
/// another file's, and a later one too (a new file of that name trashed since); one from the two
/// seconds before only when there is none. Each trash folder is read once, into a map by name. Read
/// from the trash's own folders (files/ and info/*.trashinfo, as freedesktop.org has it): `gio
/// trash --restore` needs gvfs, which KDE and containers often lack. Never over a file that has
/// taken the name since. A file that cannot come back stays in the trash with its sidecar, and
/// holds none of the others back.
/// Returns the files put back, and the first error.
pub(crate) fn restore(
    trash_move: &TrashMove,
    dirs_of: impl Fn(&Path) -> Vec<(PathBuf, PathBuf)>,
) -> (Vec<PathBuf>, Option<String>) {
    let mut trashes: HashMap<PathBuf, HashMap<PathBuf, TrashEntry>> = HashMap::new();
    // As the trash writes DeletionDate: local time, from this computer's clock, as `since` is;
    // not the .trashinfo's own time, which a share stamps with its server's clock. Two seconds
    // early, for a move that began in the second before.
    let secs = (trash_move.since.duration_since(std::time::UNIX_EPOCH)).map_or(0, |d| d.as_secs());
    let since = crate::support::format::local_time(secs as i64 - 2).replacen(' ', "T", 1);
    // Those from the move's own second on before those two seconds: one of the same name
    // trashed just before the move is another file's.
    let at = crate::support::format::local_time(secs as i64).replacen(' ', "T", 1);
    let rank = |t: &TrashEntry| (t.date < at, t.date.clone());
    // Nor one dated later than now: a trash folder on a stick from someone else (FAT and exFAT
    // give every file to whoever mounts it) may hold an entry of the same name dated in the
    // future, which would else come back where the user's own is gone. An hour and a minute
    // ahead: local time, which goes back an hour in autumn. The first since the move, so one
    // dated within that hour comes after the user's own.
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
    let until = now.map_or(0, |d| d.as_secs()) as i64 + 3660;
    let until = crate::support::format::local_time(until).replacen(' ', "T", 1);
    // A file is looked up as the list named it, and by its real path. The home trash keeps the
    // path as it was given, but gio records one in a trash at the top of a file system by its
    // real path, from the real top: an entry of a collection spelled through a link to another
    // disk is there only by its real path. Found either way, it comes back as the list named it.
    let mut find = |path: &Path| {
        let mut first: Option<TrashEntry> = None;
        let real = crate::model::listing::real_path(path);
        for at in [Some(path), Some(real.as_path()).filter(|r| *r != path)]
            .into_iter()
            .flatten()
        {
            for (dir, top) in dirs_of(at) {
                let all = (trashes.entry(dir.clone())).or_insert_with(|| {
                    let all = read_trash(&dir, &top).into_iter();
                    first_of_each(all.filter(|t| t.date >= since && t.date <= until), rank)
                });
                if let Some(t) = all.get(at)
                    && first.as_ref().is_none_or(|f| rank(t) < rank(f))
                {
                    first = Some(TrashEntry {
                        from: path.to_path_buf(),
                        ..t.clone()
                    });
                }
            }
        }
        first
    };
    // A file that cannot come back is told, and left to be undone again with its sidecar.
    let (mut found, mut error): (Vec<TrashEntry>, Option<String>) = (Vec::new(), None);
    let left: HashSet<&PathBuf> = trash_move.sidecars_left.iter().collect();
    let mut names = crate::metadata::xmp::AdobeNames::arriving(trash_move.files.iter().cloned());
    for path in &trash_move.files {
        // The rule of Copy, Move and Rename: a sidecar alone at the name (written by another
        // program, or synced in) takes it too, as the file would come back with its keywords.
        // The file's own sidecar, which the move could not take along, is no other file's.
        if let Some(at) = crate::files::file_ops::name_in_the_way(path, &left, &mut names) {
            error.get_or_insert(tf!("{name} exists already", name = at.display()));
            continue;
        }
        match find(path) {
            Some(t) => found.push(t),
            None => {
                error.get_or_insert(tf!("{name} is not in the trash", name = path.display()));
            }
        }
    }
    // Each file is put back with its sidecar right after it, so a failure at a later file
    // never leaves a restored file's sidecar in the trash. A sidecar that is no longer there
    // (the trash was emptied of it) does not hold its file back. The sidecars put back are not
    // among the files told and counted.
    let (mut sidecars_of_file, alone) = sidecars_by_file(&trash_move.files, &trash_move.sidecars);
    // Each step is a file with its own sidecars, or a sidecar alone.
    let mut steps: Vec<(TrashEntry, bool, Vec<TrashEntry>)> = Vec::new();
    for t in found {
        let side = (sidecars_of_file.remove(&t.from).into_iter().flatten())
            .filter_map(|s| find(s))
            .collect();
        steps.push((t, true, side));
    }
    let rest = alone.into_iter().filter_map(|s| find(s));
    steps.extend(rest.map(|s| (s, false, Vec::new())));
    let back = put_back(steps, &mut error);
    (back, error)
}

/// Returns the `sidecars` that a move to the trash took along, each given to one of `files`, and
/// those that belong to none of them. A file's sidecars are Glimt's own, by its name, and
/// Adobe's, by the name stem of a RAW file (`xmp::adobe_stem`, so also `IMG_1.XMP`). Each
/// sidecar goes to one file only: two RAW files of one stem trashed together took one Adobe
/// sidecar along, which only one of them can put back. Sets are used rather than searches, so a
/// delete of 50,000 photos with sidecars is undone in one pass.
fn sidecars_by_file<'a>(
    files: &'a [PathBuf],
    sidecars: &'a [PathBuf],
) -> (HashMap<&'a PathBuf, Vec<&'a PathBuf>>, Vec<&'a PathBuf>) {
    use crate::metadata::xmp;
    let mut unclaimed: HashSet<&PathBuf> = sidecars.iter().collect();
    let own: HashSet<PathBuf> = files.iter().map(|f| xmp::sidecar(f)).collect();
    // Glimt's own sidecar (IMG_1.CR2.xmp) has the name of an Adobe one too, of a stem with the
    // extension in it, so it is left out here.
    let mut adobe: HashMap<(&Path, String), Vec<&PathBuf>> = HashMap::new();
    for side in sidecars.iter().filter(|s| !own.contains(*s)) {
        if let Some(key) = xmp::adobe_stem(side) {
            adobe.entry(key).or_default().push(side);
        }
    }
    let mut by_file = HashMap::new();
    for file in files {
        let own = unclaimed.take(&xmp::sidecar(file));
        let of_stem = xmp::adobe_stem(file).and_then(|key| adobe.get(&key));
        let of_stem = of_stem
            .into_iter()
            .flatten()
            .filter_map(|s| unclaimed.take(*s));
        by_file.insert(file, own.into_iter().chain(of_stem).collect());
    }
    let alone = sidecars.iter().filter(|s| unclaimed.contains(s)).collect();
    (by_file, alone)
}

/// Puts the files of `steps` back from the trash, each with its sidecars right after it, and
/// tells the first failure in `error`. A file that cannot come back stays in the trash with its
/// sidecars, and the next files still come back. So does a file one of whose sidecars cannot
/// come back: it goes back into the trash with the sidecars that came, to be undone again,
/// because the next file of its name would else pick up the sidecar left in the trash. A sidecar
/// whose rename failed but may have been done counts as back, so that its file stays out with
/// it. Returns the files put back, without the sidecars.
fn put_back(
    steps: Vec<(TrashEntry, bool, Vec<TrashEntry>)>,
    error: &mut Option<String>,
) -> Vec<PathBuf> {
    let rename = |t: &TrashEntry| {
        rename_never_over(&t.file, &t.from)
            .map_err(|e| format!("{display}: {e}", display = t.from.display()))
    };
    let (mut back, mut renamed) = (Vec::new(), Vec::new());
    for (t, is_file, sides) in steps {
        if let Err(e) = rename(&t) {
            error.get_or_insert(e);
            continue;
        }
        // The file first, and then each sidecar that came back with it.
        let mut out = vec![t];
        for side in sides {
            if let Err(e) = rename_never_over(&side.file, &side.from) {
                // A sidecar whose rename may have been done is taken to be at its name, so that
                // its file stays out with it. Its .trashinfo is kept, as it may be in the trash
                // too, and the error says to check both names.
                let told = format!("{display}: {e}", display = side.from.display());
                if crate::files::file_io::may_have_been_done(&e)
                    && !crate::files::file_io::is_surely_gone(&side.from)
                {
                    error.get_or_insert(told);
                    continue;
                }
                let file = out[0].from.display().to_string();
                while let Some(last) = out.last()
                    && rename_never_over(&last.from, &last.file).is_ok()
                {
                    out.pop();
                }
                error.get_or_insert(match out.is_empty() {
                    true => tf!(
                        "{e}, so {file} stays in the trash, with its sidecar",
                        e = told,
                        file
                    ),
                    false => told,
                });
                break;
            }
            out.push(side);
        }
        // A file that could not go back into the trash is out, as are the sidecars that came.
        if is_file && let Some(file) = out.first() {
            back.push(file.from.clone());
        }
        renamed.extend(out);
    }
    // The names put back are on disk before their .trashinfo files go, with one sync for each
    // folder. Otherwise a power cut could leave a file in the trash's files folder with no
    // .trashinfo, where no trash view lists it and Undo cannot find it. A folder the disk did
    // not take keeps its files' .trashinfo, which is harmless beside a file that came back.
    let mut synced: HashMap<&Path, bool> = HashMap::new();
    for t in &renamed {
        let folder = t.from.parent().unwrap_or(Path::new("."));
        let is_on_disk = *synced.entry(folder).or_insert_with(|| {
            let result = crate::files::file_io::sync_folder_of(&t.from);
            if let Err(e) = &result {
                error.get_or_insert(format!("{}: {e}", folder.display()));
            }
            result.is_ok()
        });
        if is_on_disk {
            #[allow(clippy::disallowed_methods)] // the .trashinfo of a file put back
            // Not told: the file is back, and a .trashinfo left lists nothing, as its file is
            // no longer in the trash.
            let _ = std::fs::remove_file(&t.info);
        }
    }
    back
}

/// Of each name in a trash folder, the one put there first, by `rank`.
fn first_of_each<K: Ord>(
    all: impl Iterator<Item = TrashEntry>,
    rank: impl Fn(&TrashEntry) -> K,
) -> HashMap<PathBuf, TrashEntry> {
    let mut by_name: HashMap<PathBuf, TrashEntry> = HashMap::new();
    for t in all {
        match by_name.get(&t.from) {
            Some(had) if rank(had) <= rank(&t) => {}
            _ => drop(by_name.insert(t.from.clone(), t)),
        }
    }
    by_name
}

/// Moves `paths` to the trash with gio. Without gio (a .deb installed without its recommended
/// packages, or a tar.gz or AppImage on a small system), every file goes by hand, as gio would
/// move it. In a Flatpak, a file that the trash portal refused goes by hand too. The portal
/// refuses a file that is not writable, because GLib hands it a descriptor open for writing,
/// though a move needs only the folder.
pub(crate) fn trash(paths: &[PathBuf]) -> Result<(), String> {
    use crate::files::external::{has_program, in_flatpak};
    // Tests trash by hand into their own home's trash, where Undo looks. gio would put the
    // files into the user's own trash.
    let has_gio = !cfg!(test) && has_program("gio");
    let refused = match has_gio {
        false => Err("gio is not there".to_owned()),
        true => {
            // Within two minutes, because a share that stopped answering once left the delete
            // running for good, with no Stop. One gio for every file, so thousands do not
            // become thousands of processes.
            let mut gio = crate::files::external::stoppable_command("gio");
            gio.args(["trash", "--"]).args(paths);
            let limit = crate::files::external::PROGRAM_TIME_LIMIT * 4;
            crate::files::external::run_limited(gio, 64 << 10, limit).map(drop)
        }
    };
    let Err(e) = refused else {
        return Ok(());
    };
    // The host's trash, also where the host has its own XDG_DATA_HOME.
    let home = config::host_xdg_dir("XDG_DATA_HOME", ".local/share", "Trash");
    let (true, Some(home)) = (!has_gio || in_flatpak(), home) else {
        return Err(e);
    };
    // Each one left is tried, and the first that still fails is told as it failed. One that
    // could not be looked at is tried too, so that its failure is told rather than taken for done.
    let left: Vec<&Path> = (paths.iter())
        .filter(|p| !crate::files::file_io::is_surely_gone(p))
        .map(PathBuf::as_path)
        .collect();
    match trash_by_hand_anywhere(&left, &home).into_iter().next() {
        None => Ok(()),
        Some((path, e)) => Err(format!("{}: {e}", path.display())),
    }
}

/// Moves `paths` into a trash by hand, the one that gio would choose for each. A file on the file
/// system of the home trash `home` goes there, named by its path as given. A file on another (a
/// USB stick, a second disk) goes into the trash at the top of its own file system, as the trash
/// spec says, named by its real path from that top. The top is found from the real path of the
/// file's folder, so a file reached through a link to another disk goes to that disk's trash,
/// where file managers look. A file on a network share is refused, as gio refuses it, so the
/// window asks whether to delete it for good instead. Returns the files that did not go, each
/// with its error.
fn trash_by_hand_anywhere(paths: &[&Path], home: &Path) -> Vec<(PathBuf, std::io::Error)> {
    use crate::images::thumbs::uri_path;
    // Each folder's trash is looked up once, because its real path, its disk's top and the
    // file system's type take several system calls and a read of the mount table.
    let mut trash_of_folder: HashMap<&Path, std::io::Result<TrashPlace>> = HashMap::new();
    let mut by_trash: Vec<(PathBuf, Vec<(&Path, String)>)> = Vec::new();
    let mut failed = Vec::new();
    for &path in paths {
        let folder = path.parent().unwrap_or(path);
        let place = (trash_of_folder.entry(folder)).or_insert_with(|| trash_place(folder, home));
        let place = match place {
            Ok(place) => place,
            Err(e) => {
                failed.push((
                    path.to_path_buf(),
                    std::io::Error::new(e.kind(), e.to_string()),
                ));
                continue;
            }
        };
        // The spec asks for a path relative to the top in a trash there, so that a stick
        // mounted elsewhere next time still restores to its own folders.
        let path_in_trashinfo = match &place.folder_from_top {
            None => uri_path(path),
            Some(folder) => uri_path(&folder.join(path.file_name().unwrap_or_default())),
        };
        match by_trash.iter_mut().find(|(trash, _)| *trash == place.trash) {
            Some((_, files)) => files.push((path, path_in_trashinfo)),
            None => by_trash.push((place.trash.clone(), vec![(path, path_in_trashinfo)])),
        }
    }
    for (trash, files) in by_trash {
        failed.extend(trash_by_hand(&trash, &files));
    }
    failed
}

/// Returns the trash that the files of `folder` go to by hand (`trash_by_hand_anywhere`): the
/// home trash `home` for a folder on its file system, and otherwise the user's trash at the top
/// of the folder's own. Returns an error for a folder on a network share, or on a disk whose top
/// has no trash that the user may use.
fn trash_place(folder: &Path, home: &Path) -> std::io::Result<TrashPlace> {
    use std::io::ErrorKind::{CrossesDevices, Unsupported};
    use std::os::unix::fs::MetadataExt;
    let dev = |p: &Path| std::fs::metadata(p).map(|m| m.dev()).ok();
    // The home trash may not be there yet, so the nearest folder of it that is tells its disk.
    if dev(folder) == home.ancestors().find_map(dev) {
        return Ok(TrashPlace {
            trash: home.to_path_buf(),
            folder_from_top: None,
        });
    }
    let real_folder = crate::model::listing::real_dir(folder);
    let (Some(top), Some(uid)) = (top_of_file_system(&real_folder), own_uid()) else {
        return Err(CrossesDevices.into());
    };
    let kind = crate::files::file_io::file_system_type(&top);
    if kind.is_some_and(|kind| crate::files::file_io::is_remote(&kind)) {
        return Err(Unsupported.into());
    }
    let folder_from_top = real_folder.strip_prefix(&top).unwrap_or(Path::new(""));
    Ok(TrashPlace {
        trash: top_trash(&top, uid)?,
        folder_from_top: Some(folder_from_top.to_path_buf()),
    })
}

/// Returns the user's trash folder at `top`, the top of a file system, as the trash spec says:
/// `.Trash/UID` when the administrator made `.Trash` there with the sticky bit, and otherwise
/// `.Trash-UID`. Either is made when it is not there yet. A folder that another user made first,
/// or made a link, is not used.
fn top_trash(top: &Path, uid: u32) -> std::io::Result<PathBuf> {
    use std::io::ErrorKind::{AlreadyExists, PermissionDenied};
    use std::os::unix::fs::DirBuilderExt;
    let make = |dir: &Path| match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Err(e) if e.kind() != AlreadyExists => Err(e),
        _ => Ok(()),
    };
    let shared = top.join(".Trash");
    if is_usable_top_trash(&shared, None, true) {
        let own = shared.join(uid.to_string());
        // The spec's fallback: a folder that cannot be made there, or that is not the user's
        // own, sends the file to .Trash-UID.
        if make(&own).is_ok() && is_usable_top_trash(&own, Some(uid), false) {
            return Ok(own);
        }
    }
    let trash = top.join(format!(".Trash-{uid}"));
    make(&trash)?;
    // Another user who can write at the top may have made it first, or made it a link.
    match is_usable_top_trash(&trash, Some(uid), false) {
        true => Ok(trash),
        false => Err(PermissionDenied.into()),
    }
}

/// Moves `files` into the trash folder `trash` by hand, as freedesktop.org's trash spec says, each
/// with the text for its .trashinfo's `Path`. The .trashinfo files are made first, each under a
/// name not taken (name.1.ext …), and are put on disk with one sync of the info/ folder. Then
/// each file is renamed into files/. So a power cut never leaves a file in the trash that no
/// trash view lists and Undo cannot find, and a hundred files cost one folder sync rather than a
/// hundred. A move works only within one file system, and a file on another keeps its error.
/// Returns the files that did not go, each with its error.
fn trash_by_hand(trash: &Path, files: &[(&Path, String)]) -> Vec<(PathBuf, std::io::Error)> {
    use std::io::ErrorKind::AlreadyExists;
    use std::os::unix::fs::DirBuilderExt;
    let (files_folder, info) = (trash.join("files"), trash.join("info"));
    let all_failed = |e: &std::io::Error| {
        let failed = files.iter().map(|(path, _)| path.to_path_buf());
        failed
            .map(|path| (path, std::io::Error::new(e.kind(), e.to_string())))
            .collect()
    };
    for dir in [&files_folder, &info] {
        let made = std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir);
        if let Err(e) = made {
            return all_failed(&e);
        }
    }
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
    let date = crate::support::format::local_time(secs.map_or(0, |d| d.as_secs()) as i64)
        .replacen(' ', "T", 1);
    let (mut made, mut failed) = (Vec::new(), Vec::new());
    for &(path, ref path_in_trashinfo) in files {
        let text = format!("[Trash Info]\nPath={path_in_trashinfo}\nDeletionDate={date}\n");
        let name = path.file_name().unwrap_or_default();
        let mut written = Err(AlreadyExists.into());
        for n in 0..10_000 {
            let taken = name_in_trash(name, n);
            // A name taken in files/ is passed over too, so that the move below seldom fails.
            if !crate::files::file_io::is_surely_gone(&files_folder.join(&taken)) {
                continue;
            }
            let mut trashinfo = taken.clone();
            trashinfo.push(".trashinfo");
            let trashinfo = info.join(trashinfo);
            // Made new, never over another's, and on disk whole. Its name is synced below, once
            // for the whole folder.
            written = crate::files::file_io::write_beside_without_folder_sync(
                &crate::files::file_io::tmp_path(&trashinfo),
                &trashinfo,
                |f| std::io::Write::write_all(f, text.as_bytes()),
                |tmp| rename_never_over(tmp, &trashinfo),
            )
            .map(|()| (taken, trashinfo));
            match &written {
                Err(e) if e.kind() == AlreadyExists => continue,
                _ => break,
            }
        }
        match written {
            Ok((taken, trashinfo)) => made.push((path, path_in_trashinfo, taken, trashinfo)),
            Err(e) => failed.push((path.to_path_buf(), e)),
        }
    }
    let remove = |trashinfo: &Path| {
        #[allow(clippy::disallowed_methods)] // a .trashinfo made just now, of a file not moved
        // Not told: the file's own failure is told, and a .trashinfo left lists no file.
        let _ = std::fs::remove_file(trashinfo);
    };
    if let Err(e) = crate::files::file_io::sync_folder(&info) {
        for (path, _, _, trashinfo) in made {
            remove(&trashinfo);
            failed.push((
                path.to_path_buf(),
                std::io::Error::new(e.kind(), e.to_string()),
            ));
        }
        return failed;
    }
    let mut taken_meanwhile = Vec::new();
    for (path, path_in_trashinfo, taken, trashinfo) in made {
        if let Err(e) = rename_never_over(path, &files_folder.join(&taken)) {
            // A move that may have been done keeps its .trashinfo, without which no trash view
            // lists the file and Undo cannot find it.
            if !crate::files::file_io::may_have_been_done(&e) {
                remove(&trashinfo);
            }
            match e.kind() {
                AlreadyExists => taken_meanwhile.push((path, path_in_trashinfo.clone())),
                _ => failed.push((path.to_path_buf(), e)),
            }
        }
    }
    // A name that another program took in files/ after the look above is tried again, under a
    // name that is free now.
    if !taken_meanwhile.is_empty() {
        failed.extend(trash_by_hand(trash, &taken_meanwhile));
    }
    failed
}

/// Returns the name that a file called `name` takes in the trash's files/ folder at try `n`: the
/// name itself, then name.1.ext, name.2.ext and so on. Its .trashinfo adds ten bytes to it, and
/// a name may have no more than 255 bytes. So a long name loses the end of its stem, and also its
/// extension when that is too long to leave any of the stem. The trash spec allows any name here,
/// because the .trashinfo keeps the file's own path.
fn name_in_trash(name: &std::ffi::OsStr, n: u32) -> OsString {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    const MAX: usize = 255 - ".trashinfo".len();
    if n == 0 && name.len() <= MAX {
        return name.to_owned();
    }
    let path = Path::new(name);
    let stem = path.file_stem().unwrap_or_default().as_bytes();
    let mut tail = if n == 0 {
        Vec::new()
    } else {
        format!(".{n}").into_bytes()
    };
    if let Some(ext) = path.extension().filter(|e| tail.len() + e.len() + 1 < MAX) {
        tail.push(b'.');
        tail.extend(ext.as_bytes());
    }
    let mut cut = stem.len().min(MAX - tail.len());
    // The cut moves back to the start of a character, so that a UTF-8 name stays UTF-8.
    while cut > 0 && cut < stem.len() && stem[cut] & 0xC0 == 0x80 {
        cut -= 1;
    }
    OsString::from_vec([&stem[..cut], &tail[..]].concat())
}

/// The trash folders a file at `path` may have gone to, each with the folder its relative paths
/// start from: the home trash, and the two at the top of the file system `path` is on. That top
/// is found from the real path of the file's folder, as the trash by hand finds it.
pub(crate) fn trash_dirs(path: &Path) -> Vec<(PathBuf, PathBuf)> {
    // In a Flatpak, gio trash goes through the trash portal, into the host's trash.
    let home = config::host_xdg_dir("XDG_DATA_HOME", ".local/share", "Trash");
    let mut dirs: Vec<(PathBuf, PathBuf)> = home.map(|h| (h, "/".into())).into_iter().collect();
    let folder = crate::model::listing::real_dir(path.parent().unwrap_or(path));
    let (Some(top), Some(uid)) = (top_of_file_system(&folder), own_uid()) else {
        return dirs;
    };
    let shared = top.join(".Trash");
    let tops = [
        (
            shared.join(uid.to_string()),
            is_usable_top_trash(&shared, None, true),
        ),
        (top.join(format!(".Trash-{uid}")), true),
    ];
    for (dir, shared_ok) in tops {
        if shared_ok && is_usable_top_trash(&dir, Some(uid), false) {
            dirs.push((dir, top.clone()));
        }
    }
    dirs
}

/// Returns the top of the file system that the folder `real_folder` is on: the last folder up
/// from it on the same device. The folder is given by its real path, because a walk up from a
/// link's name would stop at the link. Returns `None` when the folder cannot be read.
fn top_of_file_system(real_folder: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let dev = |p: &Path| std::fs::metadata(p).map(|m| m.dev()).ok();
    let mut top = real_folder;
    let here = dev(top)?;
    while let Some(up) = top.parent().filter(|up| dev(up) == Some(here)) {
        top = up;
    }
    Some(top.to_path_buf())
}

/// Returns the user ID that Glimt runs as, or `None` when it cannot be read.
fn own_uid() -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self").map(|m| m.uid()).ok()
}

/// Returns whether `dir` may be read as a trash folder at the top of a file system, as
/// freedesktop.org asks: a folder, not a link, of user `uid` (any user's for `None`), and `sticky`
/// where every user shares it (.Trash), so none can put a folder of another's name in it. Another
/// user who can write at the top (a shared disk, a stick) could else make .Trash-1000 first, with a
/// .trashinfo of a later date for a file just thrown away, and Undo would put their file in its
/// place.
fn is_usable_top_trash(dir: &Path, uid: Option<u32>, sticky: bool) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(dir).is_ok_and(|m| {
        m.is_dir() && uid.is_none_or(|uid| m.uid() == uid) && (!sticky || m.mode() & 0o1000 != 0)
    })
}

/// What the trash folder `dir` holds, its relative paths read from `top`.
fn read_trash(dir: &Path, top: &Path) -> Vec<TrashEntry> {
    use std::os::unix::ffi::OsStrExt;
    let entries = std::fs::read_dir(dir.join("info"))
        .into_iter()
        .flatten()
        .flatten();
    (entries)
        .filter_map(|e| {
            let info = e.path();
            // The name as bytes, since gio names the entry with the file's own name, which need
            // not be UTF-8.
            let name = info.file_name()?.as_bytes().strip_suffix(b".trashinfo")?;
            let name = std::ffi::OsStr::from_bytes(name);
            let text = crate::images::decode::read_text(&info).ok()?;
            let (from, date) = read_trash_info(&text, top)?;
            Some(TrashEntry {
                from,
                date,
                file: dir.join("files").join(name),
                info,
            })
        })
        .collect()
}

/// Reads a `.trashinfo` file's `text`, its path read from `top`. Returns where the file was
/// and when it was trashed, or `None` without a path. A trash on a stick or a shared disk is
/// outside data, so the text may hold anything.
fn read_trash_info(text: &str, top: &Path) -> Option<(PathBuf, String)> {
    use std::os::unix::ffi::OsStringExt;
    let value = |key: &str| text.lines().find_map(|l| l.strip_prefix(key));
    let from = collections::percent_decoded(value("Path=")?.as_bytes());
    let date = value("DeletionDate=").unwrap_or_default().to_owned();
    Some((top.join(OsString::from_vec(from)), date))
}

#[cfg(test)]
mod tests {
    /// A `.trashinfo` file damaged in any way is read without a panic, and a whole one gives
    /// its path, percent-decoded, and its date.
    #[test]
    fn survives_broken_trash_info() {
        let top = std::path::Path::new("/media/stick");
        let seed = b"[Trash Info]\nPath=photos/a%20b.jpg\nDeletionDate=2026-11-11T10:00:00\n";
        let whole = super::read_trash_info(&String::from_utf8_lossy(seed), top);
        let expected = (top.join("photos/a b.jpg"), "2026-11-11T10:00:00".to_owned());
        assert_eq!(whole, Some(expected));
        for bytes in crate::support::test_support::broken(seed) {
            super::read_trash_info(&String::from_utf8_lossy(&bytes), top);
        }
    }

    /// Held by the tests that trash into /dev/shm's top trash, as each removes that trash's
    /// folders when it is done, which would take them away under another such test.
    static SHM_TRASH: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Each sidecar trashed with its files goes back with one file only: Glimt's own with its
    /// file, and Adobe's (any case of `.xmp`) with one RAW file of its stem. A sidecar of no
    /// file there comes back alone.
    #[test]
    fn each_sidecar_goes_back_with_one_file() {
        let p = |name: &str| std::path::PathBuf::from(format!("/pics/{name}"));
        let files = [p("r.cr2"), p("r.nef"), p("a.jpg"), p("b.cr2")];
        let sidecars = [
            p("r.cr2.xmp"),
            p("r.xmp"),
            p("a.jpg.xmp"),
            p("B.XMP"),
            p("c.xmp"),
        ];
        let (by_file, alone) = super::sidecars_by_file(&files, &sidecars);
        let of = |name: &str| by_file[&p(name)].clone();
        assert_eq!(of("r.cr2"), [&p("r.cr2.xmp"), &p("r.xmp")]);
        assert!(of("r.nef").is_empty());
        assert_eq!(of("a.jpg"), [&p("a.jpg.xmp")]);
        assert_eq!(of("b.cr2"), [&p("B.XMP")]);
        assert_eq!(alone, [&p("c.xmp")]);
    }

    /// A file that is not writable goes into the trash by hand as the trash spec says, a second
    /// of the same name beside it, and the trash reader of Undo finds both where they were. Each
    /// .trashinfo is on disk with its folder before its file moves.
    #[test]
    fn trashes_by_hand() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("glimt-test-byhand-{}", std::process::id()));
        let (pics, trash) = (dir.join("pics"), dir.join("Trash"));
        std::fs::create_dir_all(pics.join("sub")).unwrap();
        let (a, b) = (pics.join("a.png"), pics.join("sub/a.png"));
        for f in [&a, &b] {
            std::fs::write(f, "x").unwrap();
            std::fs::set_permissions(f, std::fs::Permissions::from_mode(0o444)).unwrap();
            crate::files::file_io::FOLDERS_SYNCED.take();
            let uri = crate::images::thumbs::uri_path(f);
            assert!(super::trash_by_hand(&trash, &[(f.as_path(), uri)]).is_empty());
            let synced = crate::files::file_io::FOLDERS_SYNCED.take();
            assert!(synced.contains(&trash.join("info")), "{synced:?}");
            assert!(!f.exists());
        }
        let mut names: Vec<_> = std::fs::read_dir(trash.join("files"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(names, ["a.1.png", "a.png"]);
        let mut back: Vec<_> = (super::read_trash(&trash, std::path::Path::new("/")).into_iter())
            .map(|t| (t.from, t.file.file_name().unwrap().to_owned()))
            .collect();
        back.sort();
        let want: [(std::path::PathBuf, std::ffi::OsString); 2] =
            [(a, "a.png".into()), (b, "a.1.png".into())];
        assert_eq!(back, want);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A move into the trash whose result the disk did not give (EIO after the file was linked
    /// into files/) keeps its .trashinfo, so that the trash lists the file and Undo finds it.
    #[test]
    fn a_move_into_the_trash_that_may_have_been_done_keeps_its_trashinfo() {
        use crate::files::file_io::{RENAME_WAYS_MISSING, UNLINK_FAILS};
        let dir = std::env::temp_dir().join(format!("glimt-test-lostmove-{}", std::process::id()));
        let (pics, trash) = (dir.join("pics"), dir.join("Trash"));
        std::fs::create_dir_all(&pics).unwrap();
        let a = pics.join("a.png");
        std::fs::write(&a, "x").unwrap();
        let uri = crate::images::thumbs::uri_path(&a);
        RENAME_WAYS_MISSING.set(1);
        UNLINK_FAILS.set(Some((a.clone(), libc::EIO)));
        let failed = super::trash_by_hand(&trash, &[(a.as_path(), uri)]);
        UNLINK_FAILS.set(None);
        RENAME_WAYS_MISSING.set(0);
        assert_eq!(failed.len(), 1);
        assert!(a.exists() && trash.join("files/a.png").exists());
        let listed = super::read_trash(&trash, std::path::Path::new("/"));
        assert_eq!(listed.iter().map(|t| &t.from).collect::<Vec<_>>(), [&a]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Without gio, a file on another file system than the home trash (a USB stick) goes by
    /// hand into `.Trash-UID` at the top of its own file system, and Undo puts it back from
    /// there. The test needs /dev/shm on another file system than the temporary folder, as in
    /// Docker and on GitHub's runners, and passes over the rest where it is not.
    #[test]
    fn trashes_a_file_on_another_disk_by_hand() {
        let _shm = crate::support::threads::lock_even_if_poisoned(&SHM_TRASH);
        use std::os::unix::fs::MetadataExt;
        let shm = std::path::Path::new("/dev/shm");
        let dev = |p: &std::path::Path| std::fs::metadata(p).map(|m| m.dev()).ok();
        if dev(shm).is_none() || dev(shm) == dev(&std::env::temp_dir()) {
            eprintln!("/dev/shm is not another file system here: not tested");
            return;
        }
        let dir = shm.join(format!("glimt-test-other-disk-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.png");
        std::fs::write(&file, "x").unwrap();
        let since = std::time::SystemTime::now() - std::time::Duration::from_secs(1);
        super::trash(std::slice::from_ref(&file)).unwrap();
        assert!(!file.exists());
        let uid = super::own_uid().unwrap();
        let trash = super::top_of_file_system(&dir)
            .unwrap()
            .join(format!(".Trash-{uid}"));
        assert!(trash.join("info").read_dir().unwrap().next().is_some());
        let trash_move = super::TrashMove {
            files: vec![file.clone()],
            sidecars: Vec::new(),
            sidecars_left: Vec::new(),
            since,
            marks: Default::default(),
            collection: None,
        };
        let (back, error) = super::restore(&trash_move, super::trash_dirs);
        assert_eq!((back, error), (vec![file], None));
        std::fs::remove_dir_all(dir).unwrap();
        // The trash folders stay when another test or program has put something there.
        for d in [trash.join("files"), trash.join("info"), trash] {
            let _ = std::fs::remove_dir(d);
        }
    }

    /// A file reached through a link to another disk goes by hand into the trash at the top of
    /// that disk, where gio and KIO look, not into a `.Trash-UID` in the folder the link points
    /// to. Its .trashinfo names it by its real path relative to that top, as the trash spec
    /// asks of a trash under a disk's top, and Undo puts it back under the name it was given.
    /// The test needs /dev/shm on another file system than the temporary folder, as in Docker
    /// and on GitHub's runners, and passes over the rest where it is not.
    #[test]
    fn trashes_a_file_reached_through_a_link_into_its_disks_top_trash() {
        let _shm = crate::support::threads::lock_even_if_poisoned(&SHM_TRASH);
        use std::os::unix::fs::MetadataExt;
        let shm = std::path::Path::new("/dev/shm");
        let dev = |p: &std::path::Path| std::fs::metadata(p).map(|m| m.dev()).ok();
        if dev(shm).is_none() || dev(shm) == dev(&std::env::temp_dir()) {
            eprintln!("/dev/shm is not another file system here: not tested");
            return;
        }
        let pid = std::process::id();
        let real = shm.join(format!("glimt-test-linked-{pid}"));
        let pictures = real.join("pictures");
        std::fs::create_dir_all(&pictures).unwrap();
        let link = std::env::temp_dir().join(format!("glimt-test-link-{pid}"));
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&pictures, &link).unwrap();
        let name = format!("linked {pid}.png");
        let file = link.join(&name);
        std::fs::write(&file, "x").unwrap();
        let since = std::time::SystemTime::now() - std::time::Duration::from_secs(1);
        super::trash(std::slice::from_ref(&file)).unwrap();
        assert!(!file.exists());
        let uid = super::own_uid().unwrap();
        assert!(!pictures.join(format!(".Trash-{uid}")).exists());
        let top = super::top_of_file_system(&pictures).unwrap();
        let trash = top.join(format!(".Trash-{uid}"));
        let info = trash.join("info").join(format!("{name}.trashinfo"));
        let text = std::fs::read_to_string(&info).unwrap();
        let relative = pictures.strip_prefix(&top).unwrap().join(&name);
        let want = crate::images::thumbs::uri_path(&relative);
        assert!(!want.starts_with('/') && want.contains("%20"), "{want}");
        assert!(text.contains(&format!("\nPath={want}\n")), "{text}");
        let trash_move = super::TrashMove {
            files: vec![file.clone()],
            sidecars: Vec::new(),
            sidecars_left: Vec::new(),
            since,
            marks: Default::default(),
            collection: None,
        };
        let (back, error) = super::restore(&trash_move, super::trash_dirs);
        assert_eq!((back, error), (vec![file], None));
        std::fs::remove_file(link).unwrap();
        std::fs::remove_dir_all(real).unwrap();
        for d in [trash.join("files"), trash.join("info"), trash] {
            let _ = std::fs::remove_dir(d);
        }
    }

    /// A file on the disk of the home trash is named in its .trashinfo by its absolute path,
    /// with the bytes that a URL may not hold escaped, as the trash spec asks.
    #[test]
    fn names_a_file_in_the_home_trash_by_its_absolute_path() {
        let dir = std::env::temp_dir().join(format!("glimt-test-home-path-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a b%.png");
        std::fs::write(&file, "x").unwrap();
        let home = crate::files::config::host_xdg_dir("XDG_DATA_HOME", ".local/share", "Trash");
        let home = home.unwrap();
        let dev = |p: &std::path::Path| {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(p).map(|m| m.dev()).ok()
        };
        if dev(&dir) != home.ancestors().find_map(dev) {
            eprintln!("the home trash is on another file system here: not tested");
            std::fs::remove_dir_all(&dir).unwrap();
            return;
        }
        super::trash(std::slice::from_ref(&file)).unwrap();
        let entries = super::read_trash(&home, std::path::Path::new("/"));
        let entry = entries.iter().find(|t| t.from == file).unwrap();
        let text = std::fs::read_to_string(&entry.info).unwrap();
        let want = format!(
            "\nPath={}/a%20b%25.png\n",
            crate::images::thumbs::uri_path(&dir)
        );
        assert!(want.contains("=/") && text.contains(&want), "{text}");
        std::fs::remove_file(&entry.file).unwrap();
        std::fs::remove_file(&entry.info).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Files trashed by hand together have their .trashinfo files put on disk with one sync of
    /// the trash's info/ folder, not one for each file, before any of them moves. A sync takes
    /// tens of milliseconds on a USB stick, so one for each file made a large delete take hours.
    #[test]
    fn syncs_the_trash_once_for_files_trashed_together() {
        let dir = std::env::temp_dir().join(format!("glimt-test-one-sync-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let files: Vec<_> = (0..3).map(|n| dir.join(format!("{n}.png"))).collect();
        for f in &files {
            std::fs::write(f, "x").unwrap();
        }
        let home = crate::files::config::host_xdg_dir("XDG_DATA_HOME", ".local/share", "Trash");
        let home = home.unwrap();
        let dev = |p: &std::path::Path| {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(p).map(|m| m.dev()).ok()
        };
        if dev(&dir) != home.ancestors().find_map(dev) {
            eprintln!("the home trash is on another file system here: not tested");
            std::fs::remove_dir_all(&dir).unwrap();
            return;
        }
        crate::files::file_io::FOLDERS_SYNCED.take();
        super::trash(&files).unwrap();
        let synced = crate::files::file_io::FOLDERS_SYNCED.take();
        assert_eq!(synced, [home.join("info")]);
        assert!(files.iter().all(|f| !f.exists()));
        let entries = super::read_trash(&home, std::path::Path::new("/"));
        for t in entries.iter().filter(|t| t.from.starts_with(&dir)) {
            std::fs::remove_file(&t.file).unwrap();
            std::fs::remove_file(&t.info).unwrap();
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A file whose name leaves no room for ".trashinfo" goes into the trash by hand under a
    /// shorter name, a second of the same name beside it, and Undo finds both where they were.
    #[test]
    fn trashes_a_long_name_by_hand() {
        let dir = std::env::temp_dir().join(format!("glimt-test-longhand-{}", std::process::id()));
        let (pics, trash) = (dir.join("pics"), dir.join("Trash"));
        std::fs::create_dir_all(pics.join("sub")).unwrap();
        let long = format!("{}.jpg", "ø".repeat(124));
        let (a, b) = (pics.join(&long), pics.join("sub").join(&long));
        for f in [&a, &b] {
            std::fs::write(f, "x").unwrap();
            let uri = crate::images::thumbs::uri_path(f);
            assert!(super::trash_by_hand(&trash, &[(f.as_path(), uri)]).is_empty());
            assert!(!f.exists());
        }
        let mut names: Vec<_> = std::fs::read_dir(trash.join("info"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert!(names.iter().all(|n| n.len() <= 255), "{names:?}");
        assert!(names[0].ends_with(".1.jpg.trashinfo"), "{names:?}");
        let mut back: Vec<_> = (super::read_trash(&trash, std::path::Path::new("/")).into_iter())
            .map(|t| t.from)
            .collect();
        back.sort();
        assert_eq!(back, [b, a]);
        assert_eq!(super::name_in_trash("a.png".as_ref(), 0), "a.png");
        let ext = format!("a.{}", "e".repeat(250));
        assert_eq!(super::name_in_trash(ext.as_ref(), 1), "a.1");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A moved file, a file in a moved folder, and one not moved; no trailing slash.
    #[test]
    fn reads_the_trash() {
        let dir = std::env::temp_dir().join(format!("glimt-test-trash-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("info")).unwrap();
        let info = |name: &str, path: &str, date: &str| {
            let text = format!("[Trash Info]\nPath={path}\nDeletionDate={date}\n");
            std::fs::write(dir.join("info").join(format!("{name}.trashinfo")), text).unwrap();
        };
        info(
            "bl\u{e5}b\u{e6}r x.jpg",
            "/p/bl%C3%A5b%C3%A6r%20x.jpg",
            "2026-10-21T10:00:00",
        );
        info("a.jpg", "p/a.jpg", "2026-10-21T11:00:00");
        std::fs::write(dir.join("info").join("stray"), "").unwrap();
        let mut all = super::read_trash(&dir, std::path::Path::new("/mnt"));
        all.sort_by(|a, b| a.date.cmp(&b.date));
        let got: Vec<_> = all
            .iter()
            .map(|t| (t.from.clone(), t.file.clone()))
            .collect();
        let file = |n: &str| dir.join("files").join(n);
        assert_eq!(
            got,
            [
                (
                    "/p/bl\u{e5}b\u{e6}r x.jpg".into(),
                    file("bl\u{e5}b\u{e6}r x.jpg")
                ),
                ("/mnt/p/a.jpg".into(), file("a.jpg")),
            ]
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A trash folder at the top of a file system is read only when it is the user's own folder
    /// (not a link, not another user's), and one in .Trash only when .Trash has the sticky bit.
    #[test]
    fn reads_only_the_users_own_trash_folders() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let dir = std::env::temp_dir().join(format!("glimt-test-own-trash-{}", std::process::id()));
        let (mine, shared) = (dir.join(".Trash-1000"), dir.join(".Trash"));
        std::fs::create_dir_all(&mine).unwrap();
        std::fs::create_dir_all(&shared).unwrap();
        let uid = std::fs::metadata(&mine).unwrap().uid();
        assert!(super::is_usable_top_trash(&mine, Some(uid), false));
        // Another user's, as one made first by someone who can write there.
        assert!(!super::is_usable_top_trash(&mine, Some(uid + 1), false));
        let link = dir.join(".Trash-link");
        std::os::unix::fs::symlink(&mine, &link).unwrap();
        assert!(!super::is_usable_top_trash(&link, Some(uid), false));
        assert!(!super::is_usable_top_trash(
            &dir.join("none"),
            Some(uid),
            false
        ));
        // .Trash: without the sticky bit anyone could remove the user's folder in it.
        let mode = |m| std::fs::Permissions::from_mode(m);
        std::fs::set_permissions(&shared, mode(0o777)).unwrap();
        assert!(!super::is_usable_top_trash(&shared, None, true));
        std::fs::set_permissions(&shared, mode(0o1777)).unwrap();
        assert!(super::is_usable_top_trash(&shared, None, true));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// At a disk's top, the user's trash is `.Trash/UID` when the administrator's `.Trash` has
    /// the sticky bit, and `.Trash-UID` otherwise, as the trash spec says.
    #[test]
    fn chooses_the_top_trash_as_the_spec_says() {
        use std::os::unix::fs::PermissionsExt;
        let top = std::env::temp_dir().join(format!("glimt-test-top-trash-{}", std::process::id()));
        std::fs::create_dir_all(top.join(".Trash")).unwrap();
        let uid = super::own_uid().unwrap();
        let mode = |m| std::fs::Permissions::from_mode(m);
        std::fs::set_permissions(top.join(".Trash"), mode(0o777)).unwrap();
        let own = top.join(format!(".Trash-{uid}"));
        assert_eq!(super::top_trash(&top, uid).unwrap(), own);
        std::fs::set_permissions(top.join(".Trash"), mode(0o1777)).unwrap();
        let shared = top.join(".Trash").join(uid.to_string());
        assert_eq!(super::top_trash(&top, uid).unwrap(), shared);
        assert!(super::is_usable_top_trash(&shared, Some(uid), false));
        std::fs::remove_dir_all(top).unwrap();
    }

    /// A trash entry whose name is not UTF-8, as gio makes for such a file, is read with the
    /// name's own bytes.
    #[test]
    fn reads_a_name_that_is_not_utf8() {
        use std::os::unix::ffi::OsStrExt;
        let dir = std::env::temp_dir().join(format!("glimt-test-latin1-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("info")).unwrap();
        let name = std::ffi::OsStr::from_bytes(b"caf\xe9.jpg");
        let mut info = name.to_owned();
        info.push(".trashinfo");
        let text = "[Trash Info]\nPath=/photos/caf%E9.jpg\nDeletionDate=2026-11-03T12:00:00\n";
        std::fs::write(dir.join("info").join(info), text).unwrap();
        let read = super::read_trash(&dir, std::path::Path::new("/"));
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].file, dir.join("files").join(name));
        assert_eq!(read[0].from, std::path::Path::new("/photos").join(name));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Undo of a move to the trash puts back what that move put there and nothing else: not an
    /// older sidecar of the same name, which belonged to another file (January's IMG_0001.jpg.xmp
    /// came back on today's IMG_0001.jpg), and not over a file there; with the sidecar it moved.
    #[test]
    fn restores_only_what_the_move_trashed() {
        use std::time::{Duration, SystemTime};
        let dir = std::env::temp_dir().join(format!("glimt-test-restore-{}", std::process::id()));
        let (trash, photos) = (dir.join("trash"), dir.join("photos"));
        for d in [trash.join("files"), trash.join("info"), photos.clone()] {
            std::fs::create_dir_all(d).unwrap();
        }
        // DeletionDate as gio writes it, in local time; the .trashinfo's own time, which a share
        // stamps with its server's clock, an hour off and so of no use.
        let trashed = |name: &str, kept: &str, at: SystemTime| {
            std::fs::write(trash.join("files").join(kept), name).unwrap();
            let info = trash.join("info").join(format!("{kept}.trashinfo"));
            let from = photos.join(name);
            let secs = at.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
            let date = crate::support::format::local_time(secs).replacen(' ', "T", 1);
            let text = format!(
                "[Trash Info]\nPath={}\nDeletionDate={date}\n",
                from.display()
            );
            std::fs::write(&info, text).unwrap();
            let skewed = SystemTime::now() - Duration::from_secs(3600);
            let file = std::fs::File::options().write(true).open(&info).unwrap();
            file.set_modified(skewed).unwrap();
        };
        let now = SystemTime::now();
        let january = now - Duration::from_secs(90 * 24 * 3600);
        trashed("IMG_0001.jpg.xmp", "IMG_0001.jpg.xmp", january);
        trashed("IMG_0001.jpg", "IMG_0001.jpg", now);
        trashed("IMG_0002.jpg", "IMG_0002.jpg", now);
        trashed("IMG_0002.jpg.xmp", "IMG_0002.jpg.xmp", now);
        let dirs = |_: &std::path::Path| vec![(trash.clone(), std::path::PathBuf::from("/"))];
        let trash_move = |files: &[&str], sidecars: &[&str]| super::TrashMove {
            files: files.iter().map(|f| photos.join(f)).collect(),
            sidecars: sidecars.iter().map(|f| photos.join(f)).collect(),
            sidecars_left: Vec::new(),
            since: now - Duration::from_secs(1),
            marks: Default::default(),
            collection: None,
        };
        // IMG_0001.jpg went alone: January's sidecar of its name stays in the trash.
        let (back, error) = super::restore(&trash_move(&["IMG_0001.jpg"], &[]), dirs);
        assert_eq!((back, error), (vec![photos.join("IMG_0001.jpg")], None));
        assert!(!photos.join("IMG_0001.jpg.xmp").exists());
        // Even when asked for: it is older than the move.
        let old = super::restore(&trash_move(&["IMG_0001.jpg.xmp"], &[]), dirs);
        assert!(old.0.is_empty() && old.1.is_some());
        // IMG_0002.jpg went with its sidecar, and both come back; never over a file there.
        std::fs::write(photos.join("IMG_0002.jpg"), "new").unwrap();
        let taken = super::restore(&trash_move(&["IMG_0002.jpg"], &["IMG_0002.jpg.xmp"]), dirs);
        assert!(taken.0.is_empty() && taken.1.is_some());
        std::fs::remove_file(photos.join("IMG_0002.jpg")).unwrap();
        crate::files::file_io::FOLDERS_SYNCED.take();
        let (back, error) =
            super::restore(&trash_move(&["IMG_0002.jpg"], &["IMG_0002.jpg.xmp"]), dirs);
        // The names put back are on disk before their .trashinfo files go, with one sync for
        // the folder of both.
        assert_eq!(
            crate::files::file_io::FOLDERS_SYNCED.take(),
            std::slice::from_ref(&photos)
        );
        assert_eq!(error, None);
        assert_eq!(back, [photos.join("IMG_0002.jpg")]);
        assert!(photos.join("IMG_0002.jpg.xmp").exists());
        // Another trash folder (a stick from someone else) with one of the same name dated next
        // year: the user's own comes back, not that one.
        trashed("IMG_0003.jpg", "IMG_0003.jpg", now);
        let stick = dir.join("stick");
        for d in [stick.join("files"), stick.join("info")] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(stick.join("files/IMG_0003.jpg"), "planted").unwrap();
        let secs = now.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
        let next_year =
            crate::support::format::local_time(secs + 365 * 24 * 3600).replacen(' ', "T", 1);
        let info = format!(
            "[Trash Info]\nPath={}\nDeletionDate={next_year}\n",
            photos.join("IMG_0003.jpg").display()
        );
        std::fs::write(stick.join("info/IMG_0003.jpg.trashinfo"), info).unwrap();
        let both = |_: &std::path::Path| {
            let root = std::path::PathBuf::from("/");
            vec![(trash.clone(), root.clone()), (stick.clone(), root)]
        };
        let (back, error) = super::restore(&trash_move(&["IMG_0003.jpg"], &[]), both);
        assert_eq!((back, error), (vec![photos.join("IMG_0003.jpg")], None));
        let got = std::fs::read_to_string(photos.join("IMG_0003.jpg")).unwrap();
        assert_eq!(got, "IMG_0003.jpg");
        // The same in one trash folder, the stick's own, where the user's file went too.
        let at = |kept: &str, from: &str, date: &str, text: &str| {
            std::fs::write(stick.join("files").join(kept), text).unwrap();
            let info = format!("[Trash Info]\nPath={from}\nDeletionDate={date}\n");
            std::fs::write(stick.join("info").join(format!("{kept}.trashinfo")), info).unwrap();
        };
        let today = crate::support::format::local_time(secs).replacen(' ', "T", 1);
        let img4 = photos.join("IMG_0004.jpg").display().to_string();
        at("IMG_0004.jpg", &img4, &next_year, "planted");
        at("IMG_0004.2.jpg", &img4, &today, "mine");
        let (back, error) = super::restore(&trash_move(&["IMG_0004.jpg"], &[]), both);
        assert_eq!((back, error), (vec![photos.join("IMG_0004.jpg")], None));
        let got = std::fs::read_to_string(photos.join("IMG_0004.jpg")).unwrap();
        assert_eq!(got, "mine");
        // One dated half an hour ahead, within the hour let through: still after the user's.
        let soon = crate::support::format::local_time(secs + 1800).replacen(' ', "T", 1);
        let img5 = photos.join("IMG_0005.jpg").display().to_string();
        at("IMG_0005.jpg", &img5, &soon, "planted");
        at("IMG_0005.2.jpg", &img5, &today, "mine");
        let (back, error) = super::restore(&trash_move(&["IMG_0005.jpg"], &[]), both);
        assert_eq!((back, error), (vec![photos.join("IMG_0005.jpg")], None));
        let got = std::fs::read_to_string(photos.join("IMG_0005.jpg")).unwrap();
        assert_eq!(got, "mine");
        // One of the same name trashed a second before the move: another file's, the move's own
        // comes back. Alone, it is taken, as the move may have begun in that second.
        let before = crate::support::format::local_time(secs - 1).replacen(' ', "T", 1);
        let img6 = photos.join("IMG_0006.jpg").display().to_string();
        at("IMG_0006.jpg", &img6, &before, "other");
        at("IMG_0006.2.jpg", &img6, &today, "mine");
        let thrown6 = super::TrashMove {
            since: now,
            ..trash_move(&["IMG_0006.jpg"], &[])
        };
        let (back, error) = super::restore(&thrown6, both);
        assert_eq!((back, error), (vec![photos.join("IMG_0006.jpg")], None));
        let got = std::fs::read_to_string(photos.join("IMG_0006.jpg")).unwrap();
        assert_eq!(got, "mine");
        std::fs::remove_file(photos.join("IMG_0006.jpg")).unwrap();
        // One file emptied from the trash meanwhile: the others still come back.
        trashed("IMG_0007.jpg", "IMG_0007.jpg", now);
        let (back, error) =
            super::restore(&trash_move(&["IMG_0099.jpg", "IMG_0007.jpg"], &[]), dirs);
        assert_eq!(back, [photos.join("IMG_0007.jpg")]);
        assert!(error.is_some_and(|e| e.contains("IMG_0099.jpg")));
        let (back, error) = super::restore(&thrown6, both);
        assert_eq!((back, error), (vec![photos.join("IMG_0006.jpg")], None));
        let got = std::fs::read_to_string(photos.join("IMG_0006.jpg")).unwrap();
        assert_eq!(got, "other");
        // A sidecar alone at the name holds its file back, unless it is the file's own, which
        // the move could not take to the trash. Then the file comes back beside it.
        trashed("IMG_0008.jpg", "IMG_0008.jpg", now);
        std::fs::write(photos.join("IMG_0008.jpg.xmp"), "own").unwrap();
        let alone = super::restore(&trash_move(&["IMG_0008.jpg"], &[]), dirs);
        assert!(alone.0.is_empty() && alone.1.is_some());
        let own = super::TrashMove {
            sidecars_left: vec![photos.join("IMG_0008.jpg.xmp")],
            ..trash_move(&["IMG_0008.jpg"], &[])
        };
        let (back, error) = super::restore(&own, dirs);
        assert_eq!((back, error), (vec![photos.join("IMG_0008.jpg")], None));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Undo from the trash puts each file back with its sidecar, so a file that cannot come back
    /// never leaves a restored file's sidecar in the trash.
    #[test]
    fn a_failed_restore_never_splits_a_file_from_its_sidecar() {
        let dir = std::env::temp_dir().join(format!("glimt-test-pairs-{}", std::process::id()));
        let (trash, x, y) = (dir.join("trash"), dir.join("x"), dir.join("y"));
        for d in [trash.join("files"), trash.join("info"), x.clone()] {
            std::fs::create_dir_all(d).unwrap();
        }
        let now = std::time::SystemTime::now();
        let secs = now.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
        let date = crate::support::format::local_time(secs).replacen(' ', "T", 1);
        for (from, kept) in [
            (x.join("a.jpg"), "a.jpg"),
            (y.join("b.jpg"), "b.jpg"),
            (x.join("a.jpg.xmp"), "a.jpg.xmp"),
        ] {
            crate::files::file_io::create_file(&trash.join("files").join(kept), kept).unwrap();
            let text = format!(
                "[Trash Info]\nPath={}\nDeletionDate={date}\n",
                from.display()
            );
            let info = trash.join("info").join(format!("{kept}.trashinfo"));
            crate::files::file_io::create_file(&info, text).unwrap();
        }
        // The folder y is gone, so b.jpg cannot come back, but a.jpg comes back with its sidecar.
        let trash_move = super::TrashMove {
            files: vec![x.join("a.jpg"), y.join("b.jpg")],
            sidecars: vec![x.join("a.jpg.xmp")],
            sidecars_left: Vec::new(),
            since: now - std::time::Duration::from_secs(1),
            marks: Default::default(),
            collection: None,
        };
        let dirs = |_: &std::path::Path| vec![(trash.clone(), std::path::PathBuf::from("/"))];
        let (back, error) = super::restore(&trash_move, dirs);
        assert_eq!(back, [x.join("a.jpg")]);
        assert!(error.is_some());
        assert!(x.join("a.jpg.xmp").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Undo from the trash leaves a file in the trash when its sidecar cannot come back, and says
    /// so. Before, the file came back without its sidecar, which waited in the trash.
    #[test]
    fn a_file_comes_back_from_the_trash_only_with_its_sidecar() {
        use crate::files::file_io::{RENAME_WAYS_MISSING, UNLINK_FAILS};
        let dir = std::env::temp_dir().join(format!("glimt-test-back-pair-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (trash, x) = (dir.join("trash"), dir.join("x"));
        for d in [trash.join("files"), trash.join("info"), x.clone()] {
            std::fs::create_dir_all(d).unwrap();
        }
        let now = std::time::SystemTime::now();
        let secs = now.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
        let date = crate::support::format::local_time(secs).replacen(' ', "T", 1);
        for kept in ["a.jpg", "a.jpg.xmp"] {
            crate::files::file_io::create_file(&trash.join("files").join(kept), kept).unwrap();
            let text = format!(
                "[Trash Info]\nPath={}\nDeletionDate={date}\n",
                x.join(kept).display()
            );
            let info = trash.join("info").join(format!("{kept}.trashinfo"));
            crate::files::file_io::create_file(&info, text).unwrap();
        }
        let trash_move = super::TrashMove {
            files: vec![x.join("a.jpg")],
            sidecars: vec![x.join("a.jpg.xmp")],
            sidecars_left: Vec::new(),
            since: now - std::time::Duration::from_secs(1),
            marks: Default::default(),
            collection: None,
        };
        let dirs = |_: &std::path::Path| vec![(trash.clone(), std::path::PathBuf::from("/"))];
        // The sidecar's name in the trash cannot be unlinked, so it cannot come back.
        RENAME_WAYS_MISSING.set(1);
        UNLINK_FAILS.set(Some((trash.join("files/a.jpg.xmp"), libc::EPERM)));
        let (back, error) = super::restore(&trash_move, dirs);
        UNLINK_FAILS.set(None);
        RENAME_WAYS_MISSING.set(0);
        assert!(back.is_empty(), "{error:?}");
        assert!(error.unwrap().contains("stays in the trash"));
        assert_eq!(std::fs::read_dir(&x).unwrap().count(), 0);
        assert!(trash.join("files/a.jpg").exists() && trash.join("info/a.jpg.trashinfo").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A sidecar whose way back from the trash failed with an error that may hide a rename done
    /// (an unlink whose answer was lost) counts as back, and its photo stays out with it. Before,
    /// the photo went back into the trash while the sidecar was also at its name.
    #[test]
    fn a_file_stays_out_of_the_trash_when_its_sidecar_may_be_back() {
        use crate::files::file_io::{RENAME_WAYS_MISSING, UNLINK_FAILS};
        let dir =
            std::env::temp_dir().join(format!("glimt-test-back-maybe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (trash, x) = (dir.join("trash"), dir.join("x"));
        for d in [trash.join("files"), trash.join("info"), x.clone()] {
            std::fs::create_dir_all(d).unwrap();
        }
        let now = std::time::SystemTime::now();
        let secs = now.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
        let date = crate::support::format::local_time(secs).replacen(' ', "T", 1);
        for kept in ["a.jpg", "a.jpg.xmp"] {
            crate::files::file_io::create_file(&trash.join("files").join(kept), kept).unwrap();
            let text = format!(
                "[Trash Info]\nPath={}\nDeletionDate={date}\n",
                x.join(kept).display()
            );
            let info = trash.join("info").join(format!("{kept}.trashinfo"));
            crate::files::file_io::create_file(&info, text).unwrap();
        }
        let trash_move = super::TrashMove {
            files: vec![x.join("a.jpg")],
            sidecars: vec![x.join("a.jpg.xmp")],
            sidecars_left: Vec::new(),
            since: now - std::time::Duration::from_secs(1),
            marks: Default::default(),
            collection: None,
        };
        let dirs = |_: &std::path::Path| vec![(trash.clone(), std::path::PathBuf::from("/"))];
        // The sidecar is linked at its name, and the unlink of its name in the trash fails
        // with an error that says nothing of what was done.
        RENAME_WAYS_MISSING.set(1);
        UNLINK_FAILS.set(Some((trash.join("files/a.jpg.xmp"), libc::EIO)));
        let (back, error) = super::restore(&trash_move, dirs);
        UNLINK_FAILS.set(None);
        RENAME_WAYS_MISSING.set(0);
        assert_eq!(back, [x.join("a.jpg")], "{error:?}");
        assert!(error.is_some());
        assert_eq!(std::fs::read_to_string(x.join("a.jpg")).unwrap(), "a.jpg");
        assert_eq!(
            std::fs::read_to_string(x.join("a.jpg.xmp")).unwrap(),
            "a.jpg.xmp"
        );
        assert!(trash.join("info/a.jpg.xmp.trashinfo").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A file that cannot come back from the trash stays there with its sidecar, and the files
    /// after it still come back. Undo stopped at the first one.
    #[test]
    fn a_file_that_cannot_come_back_holds_none_back() {
        let dir = std::env::temp_dir().join(format!("glimt-test-holds-{}", std::process::id()));
        let (trash, x) = (dir.join("trash"), dir.join("x"));
        for d in [trash.join("files"), trash.join("info"), x.clone()] {
            std::fs::create_dir_all(d).unwrap();
        }
        let now = std::time::SystemTime::now();
        let secs = now.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
        let date = crate::support::format::local_time(secs).replacen(' ', "T", 1);
        // b.jpg's own file is gone from the trash, so it cannot come back.
        for kept in ["a.jpg", "a.jpg.xmp", "b.jpg", "b.jpg.xmp"] {
            if kept != "b.jpg" {
                crate::files::file_io::create_file(&trash.join("files").join(kept), kept).unwrap();
            }
            let text = format!(
                "[Trash Info]\nPath={}\nDeletionDate={date}\n",
                x.join(kept).display()
            );
            let info = trash.join("info").join(format!("{kept}.trashinfo"));
            crate::files::file_io::create_file(&info, text).unwrap();
        }
        let trash_move = super::TrashMove {
            files: vec![x.join("b.jpg"), x.join("a.jpg")],
            sidecars: vec![x.join("b.jpg.xmp"), x.join("a.jpg.xmp")],
            sidecars_left: Vec::new(),
            since: now - std::time::Duration::from_secs(1),
            marks: Default::default(),
            collection: None,
        };
        let dirs = |_: &std::path::Path| vec![(trash.clone(), std::path::PathBuf::from("/"))];
        let (back, error) = super::restore(&trash_move, dirs);
        assert_eq!(back, [x.join("a.jpg")]);
        assert!(error.is_some());
        assert!(x.join("a.jpg.xmp").exists() && !x.join("b.jpg.xmp").exists());
        assert!(trash.join("files/b.jpg.xmp").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Undo from the trash leaves a file in the trash when a sidecar of its name has appeared
    /// since, so the photo neither gets that sidecar's keywords nor loses its own.
    #[test]
    fn a_restore_is_refused_by_a_sidecar_that_took_the_name() {
        let dir = std::env::temp_dir().join(format!("glimt-test-stray-{}", std::process::id()));
        let (trash, x) = (dir.join("trash"), dir.join("x"));
        for d in [trash.join("files"), trash.join("info"), x.clone()] {
            std::fs::create_dir_all(d).unwrap();
        }
        let now = std::time::SystemTime::now();
        let secs = now.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
        let date = crate::support::format::local_time(secs).replacen(' ', "T", 1);
        for kept in ["a.jpg", "a.jpg.xmp"] {
            crate::files::file_io::create_file(&trash.join("files").join(kept), kept).unwrap();
            let text = format!(
                "[Trash Info]\nPath={}\nDeletionDate={date}\n",
                x.join(kept).display()
            );
            let info = trash.join("info").join(format!("{kept}.trashinfo"));
            crate::files::file_io::create_file(&info, text).unwrap();
        }
        crate::files::file_io::create_file(&x.join("a.jpg.xmp"), "stranger").unwrap();
        let trash_move = super::TrashMove {
            files: vec![x.join("a.jpg")],
            sidecars: vec![x.join("a.jpg.xmp")],
            sidecars_left: Vec::new(),
            since: now - std::time::Duration::from_secs(1),
            marks: Default::default(),
            collection: None,
        };
        let dirs = |_: &std::path::Path| vec![(trash.clone(), std::path::PathBuf::from("/"))];
        let (back, error) = super::restore(&trash_move, dirs);
        assert!(back.is_empty() && error.is_some());
        assert!(!x.join("a.jpg").exists());
        assert!(trash.join("files/a.jpg").exists() && trash.join("files/a.jpg.xmp").exists());
        let stranger = std::fs::read_to_string(x.join("a.jpg.xmp")).unwrap();
        assert_eq!(stranger, "stranger");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A file named through a link to another disk (a collection's entry `~/link/a.jpg` with
    /// `~/link` → `/media/disk/photos`) goes to that disk's trash, where gio records it by its
    /// real path from the disk's top. Undo finds it there, with its sidecar, and puts it back
    /// under the name the list gave it.
    #[test]
    fn restores_a_file_named_through_a_link_to_another_disk() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-restore-link-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("disk/photos")).unwrap();
        let disk = dir.join("disk").canonicalize().unwrap();
        let trash = disk.join(".Trash-1000");
        for d in [trash.join("files"), trash.join("info")] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::os::unix::fs::symlink(disk.join("photos"), dir.join("link")).unwrap();
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
        let date = crate::support::format::local_time(secs.unwrap().as_secs() as i64);
        for name in ["a.jpg", "a.jpg.xmp"] {
            std::fs::write(trash.join("files").join(name), name).unwrap();
            let info = format!(
                "[Trash Info]\nPath=photos/{name}\nDeletionDate={}\n",
                date.replacen(' ', "T", 1)
            );
            std::fs::write(trash.join("info").join(format!("{name}.trashinfo")), info).unwrap();
        }
        // As trash_dirs gives them: the disk's trash only for a path on the disk itself.
        let dirs = |p: &std::path::Path| match p.starts_with(&disk) {
            true => vec![(trash.clone(), disk.clone())],
            false => Vec::new(),
        };
        let file = dir.join("link/a.jpg");
        let trash_move = super::TrashMove {
            files: vec![file.clone()],
            sidecars: vec![dir.join("link/a.jpg.xmp")],
            sidecars_left: Vec::new(),
            since: std::time::SystemTime::now() - std::time::Duration::from_secs(1),
            marks: Default::default(),
            collection: None,
        };
        let (back, error) = super::restore(&trash_move, dirs);
        assert_eq!((back, error), (vec![file], None));
        assert!(disk.join("photos/a.jpg").exists() && disk.join("photos/a.jpg.xmp").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
