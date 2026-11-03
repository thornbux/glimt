//! The trash: files moved to it (with gio, or by hand as freedesktop.org's trash spec says when
//! the Flatpak's trash portal refuses one), and put back from it for Undo by reading the trash
//! folders' own files.

use crate::files::file_io::rename_never_over;
use crate::{files::config, model::collections, tf};
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// What a move to the trash did, as it did it, for Undo to put back: the files or folder it
/// moved, the sidecars that went with them, when it began (the trash may hold older files of the
/// same names, which are not these), and what went with them elsewhere: their marks and stars,
/// and their places in the collection shown. Recorded as it happens, never found out later: by
/// the move's thread, and the marks by `receive_deleted` as it forgets them, which is the one
/// way an `Undo::Trash` reaches `App::undo`. `since` has no default: one left out would take an
/// older file of the same name from the trash.
pub(crate) struct Thrown {
    pub(crate) files: Vec<PathBuf>,
    pub(crate) sidecars: Vec<PathBuf>,
    pub(crate) since: std::time::SystemTime,
    pub(crate) marks: Vec<(PathBuf, (u8, u8))>,
    pub(crate) collection: Option<(PathBuf, Vec<(usize, PathBuf)>)>,
}

/// A file in a trash folder: where it came from, when (as the trash writes it, which sorts by
/// time), and its file and its .trashinfo there.
#[derive(Clone)]
struct Trashed {
    from: PathBuf,
    date: String,
    file: PathBuf,
    info: PathBuf,
}

/// What `thrown` moved to the trash put back where it was, with the sidecars that went with it: of
/// each, the first of its name put in the trash folders `dirs_of` gives (the home trash, and those
/// at the top of its file system) since the move began, as an older one of the same name is another
/// file's, and a later one too (a new file of that name trashed since); one from the two seconds
/// before only when there is none. Each trash folder is read once, into a map by name. Read from
/// the trash's own folders (files/ and info/*.trashinfo, as freedesktop.org has it): `gio trash
/// --restore` needs gvfs, which KDE and containers often lack. Never over a file that has taken the
/// name since. The files put back, and the first error.
pub(crate) fn restore(
    thrown: &Thrown,
    dirs_of: impl Fn(&Path) -> Vec<(PathBuf, PathBuf)>,
) -> (Vec<PathBuf>, Option<String>) {
    let mut trashes: HashMap<PathBuf, HashMap<PathBuf, Trashed>> = HashMap::new();
    // As the trash writes DeletionDate: local time, from this computer's clock, as `since` is;
    // not the .trashinfo's own time, which a share stamps with its server's clock. Two seconds
    // early, for a move that began in the second before.
    let secs = (thrown.since.duration_since(std::time::UNIX_EPOCH)).map_or(0, |d| d.as_secs());
    let since = crate::support::format::local_time(secs as i64 - 2).replacen(' ', "T", 1);
    // Those from the move's own second on before those two seconds: one of the same name
    // trashed just before the move is another file's.
    let at = crate::support::format::local_time(secs as i64).replacen(' ', "T", 1);
    let rank = |t: &Trashed| (t.date < at, t.date.clone());
    // Nor one dated later than now: a trash folder on a stick from someone else (FAT and exFAT
    // give every file to whoever mounts it) may hold an entry of the same name dated in the
    // future, which would else come back where the user's own is gone. An hour and a minute
    // ahead: local time, which goes back an hour in autumn. The first since the move, so one
    // dated within that hour comes after the user's own.
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
    let until = now.map_or(0, |d| d.as_secs()) as i64 + 3660;
    let until = crate::support::format::local_time(until).replacen(' ', "T", 1);
    let mut find = |path: &Path| {
        let mut first: Option<Trashed> = None;
        for (dir, top) in dirs_of(path) {
            let all = (trashes.entry(dir.clone())).or_insert_with(|| {
                let all = read_trash(&dir, &top).into_iter();
                first_of_each(all.filter(|t| t.date >= since && t.date <= until), rank)
            });
            if let Some(t) = all.get(path)
                && first.as_ref().is_none_or(|f| rank(t) < rank(f))
            {
                first = Some(t.clone());
            }
        }
        first
    };
    let mut found = Vec::new();
    for path in &thrown.files {
        if path.exists() {
            return (
                Vec::new(),
                Some(tf!("{name} exists already", name = path.display())),
            );
        }
        match find(path) {
            Some(t) => found.push(t),
            None => {
                let e = tf!("{name} is not in the trash", name = path.display());
                return (Vec::new(), Some(e));
            }
        }
    }
    // A sidecar no longer there (the trash emptied of it) does not hold its file back; the
    // sidecars put back are not among the files told and counted.
    let files = found.len();
    found.extend(thrown.sidecars.iter().filter_map(|s| find(s)));
    let mut back = Vec::new();
    for (k, t) in found.into_iter().enumerate() {
        if let Err(e) = rename_never_over(&t.file, &t.from) {
            return (
                back,
                Some(format!("{display}: {e}", display = t.from.display())),
            );
        }
        let _ = std::fs::remove_file(&t.info);
        if k < files {
            back.push(t.from);
        }
    }
    (back, None)
}

/// Of each name in a trash folder, the one put there first, by `rank`.
fn first_of_each<K: Ord>(
    all: impl Iterator<Item = Trashed>,
    rank: impl Fn(&Trashed) -> K,
) -> HashMap<PathBuf, Trashed> {
    let mut by_name: HashMap<PathBuf, Trashed> = HashMap::new();
    for t in all {
        match by_name.get(&t.from) {
            Some(had) if rank(had) <= rank(&t) => {}
            _ => drop(by_name.insert(t.from.clone(), t)),
        }
    }
    by_name
}

/// `paths` moved to the trash with gio; in a Flatpak, one the trash portal refused (a file that
/// is not writable: GLib hands the portal a descriptor open for writing, though a move needs only
/// the folder) by hand into the home trash, as gio on the host would.
pub(crate) fn trash(paths: &[PathBuf]) -> Result<(), String> {
    let refused = crate::files::external::trash(paths);
    let Err(e) = refused else {
        return Ok(());
    };
    let home = std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share/Trash"));
    let (Some(_), Some(home)) = (std::env::var_os("FLATPAK_ID"), home) else {
        return Err(e);
    };
    // Each one left tried, and the first that still fails told as it failed.
    let left = paths
        .iter()
        .filter(|p| std::fs::symlink_metadata(p).is_ok());
    let failed: Vec<String> = (left.map(|p| (p, trash_by_hand(p, &home))))
        .filter_map(|(p, done)| done.err().map(|e| format!("{}: {e}", p.display())))
        .collect();
    match failed.into_iter().next() {
        None => Ok(()),
        Some(first) => Err(first),
    }
}

/// `path` into the trash folder `trash` by hand, as freedesktop.org's trash spec says: its
/// .trashinfo made first under a name not taken (name.1.ext …), then the file renamed into
/// files/. Only within one file system: a file on another keeps its error.
fn trash_by_hand(path: &Path, trash: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    let (files, info) = (trash.join("files"), trash.join("info"));
    for dir in [&files, &info] {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
    }
    let dev = |p: &Path| std::fs::metadata(p).map(|m| m.dev()).ok();
    if dev(path.parent().unwrap_or(path)) != dev(&files) {
        return Err(std::io::ErrorKind::CrossesDevices.into());
    }
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
    let date = crate::support::format::local_time(secs.map_or(0, |d| d.as_secs()) as i64)
        .replacen(' ', "T", 1);
    let text = format!(
        "[Trash Info]\nPath={}\nDeletionDate={date}\n",
        crate::images::thumbs::uri_path(path)
    );
    let name = path.file_name().unwrap_or_default();
    let (stem, ext) = (Path::new(name).file_stem(), Path::new(name).extension());
    for n in 0..10_000 {
        let mut taken = OsString::new();
        match n {
            0 => taken.push(name),
            n => {
                taken.push(stem.unwrap_or_default());
                taken.push(format!(".{n}"));
                if let Some(ext) = ext {
                    taken.push(".");
                    taken.push(ext);
                }
            }
        }
        let mut trashinfo = taken.clone();
        trashinfo.push(".trashinfo");
        let trashinfo = info.join(trashinfo);
        #[allow(clippy::disallowed_methods)] // made new, never over another's
        let made = std::fs::File::options()
            .write(true)
            .create_new(true)
            .open(&trashinfo);
        let mut made = match made {
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            made => made?,
        };
        let moved = std::io::Write::write_all(&mut made, text.as_bytes())
            .and_then(|()| rename_never_over(path, &files.join(&taken)));
        match moved {
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let _ = std::fs::remove_file(&trashinfo);
            }
            Err(e) => {
                let _ = std::fs::remove_file(&trashinfo);
                return Err(e);
            }
            Ok(()) => return Ok(()),
        }
    }
    Err(std::io::ErrorKind::AlreadyExists.into())
}

/// The trash folders a file at `path` may have gone to, each with the folder its relative paths
/// start from: the home trash, and the two at the top of the file system `path` is on.
pub(crate) fn trash_dirs(path: &Path) -> Vec<(PathBuf, PathBuf)> {
    use std::os::unix::fs::MetadataExt;
    // In a Flatpak, gio trash goes through the trash portal, into the host's trash.
    let home = match std::env::var_os("FLATPAK_ID") {
        Some(_) => std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share/Trash")),
        None => config::xdg_dir("XDG_DATA_HOME", ".local/share", "Trash"),
    };
    let mut dirs: Vec<(PathBuf, PathBuf)> = home.map(|h| (h, "/".into())).into_iter().collect();
    // The top of its file system: the last folder up on the same device.
    let dev = |p: &Path| std::fs::metadata(p).map(|m| m.dev()).ok();
    let uid = std::fs::metadata("/proc/self").map(|m| m.uid());
    let (Some(mut top), Ok(uid)) = (path.parent(), uid) else {
        return dirs;
    };
    let Some(here) = dev(top) else {
        return dirs;
    };
    while let Some(up) = top.parent().filter(|up| dev(up) == Some(here)) {
        top = up;
    }
    let shared = top.join(".Trash");
    let tops = [
        (shared.join(uid.to_string()), own_dir(&shared, None, true)),
        (top.join(format!(".Trash-{uid}")), true),
    ];
    for (dir, shared_ok) in tops {
        if shared_ok && own_dir(&dir, Some(uid), false) {
            dirs.push((dir, top.to_path_buf()));
        }
    }
    dirs
}

/// Whether `dir` may be read as a trash folder at the top of a file system, as freedesktop.org
/// asks: a folder, not a link, of user `uid` (any user's for `None`), and `sticky` where every
/// user shares it (.Trash), so none can put a folder of another's name in it. Another user who
/// can write at the top (a shared disk, a stick) could else make .Trash-1000 first, with a
/// .trashinfo of a later date for a file just thrown away, and Undo would put their file in its
/// place.
fn own_dir(dir: &Path, uid: Option<u32>, sticky: bool) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(dir).is_ok_and(|m| {
        m.is_dir() && uid.is_none_or(|uid| m.uid() == uid) && (!sticky || m.mode() & 0o1000 != 0)
    })
}

/// What the trash folder `dir` holds, its relative paths read from `top`.
fn read_trash(dir: &Path, top: &Path) -> Vec<Trashed> {
    use std::os::unix::ffi::OsStringExt;
    let entries = std::fs::read_dir(dir.join("info"))
        .into_iter()
        .flatten()
        .flatten();
    (entries)
        .filter_map(|e| {
            let info = e.path();
            let name = info
                .file_name()?
                .to_str()?
                .strip_suffix(".trashinfo")?
                .to_owned();
            let text = crate::images::decode::read_text(&info).ok()?;
            let value = |key: &str| text.lines().find_map(|l| l.strip_prefix(key));
            let from = collections::percent_decoded(value("Path=")?.as_bytes());
            Some(Trashed {
                from: top.join(OsString::from_vec(from)),
                date: value("DeletionDate=").unwrap_or_default().to_owned(),
                file: dir.join("files").join(name),
                info,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    /// A file that is not writable goes into the trash by hand as the trash spec says, a second
    /// of the same name beside it, and the trash reader of Undo finds both where they were.
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
            super::trash_by_hand(f, &trash).unwrap();
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
        assert!(super::own_dir(&mine, Some(uid), false));
        // Another user's, as one made first by someone who can write there.
        assert!(!super::own_dir(&mine, Some(uid + 1), false));
        let link = dir.join(".Trash-link");
        std::os::unix::fs::symlink(&mine, &link).unwrap();
        assert!(!super::own_dir(&link, Some(uid), false));
        assert!(!super::own_dir(&dir.join("none"), Some(uid), false));
        // .Trash: without the sticky bit anyone could remove the user's folder in it.
        let mode = |m| std::fs::Permissions::from_mode(m);
        std::fs::set_permissions(&shared, mode(0o777)).unwrap();
        assert!(!super::own_dir(&shared, None, true));
        std::fs::set_permissions(&shared, mode(0o1777)).unwrap();
        assert!(super::own_dir(&shared, None, true));
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
        let thrown = |files: &[&str], sidecars: &[&str]| super::Thrown {
            files: files.iter().map(|f| photos.join(f)).collect(),
            sidecars: sidecars.iter().map(|f| photos.join(f)).collect(),
            since: now - Duration::from_secs(1),
            marks: Vec::new(),
            collection: None,
        };
        // IMG_0001.jpg went alone: January's sidecar of its name stays in the trash.
        let (back, error) = super::restore(&thrown(&["IMG_0001.jpg"], &[]), dirs);
        assert_eq!((back, error), (vec![photos.join("IMG_0001.jpg")], None));
        assert!(!photos.join("IMG_0001.jpg.xmp").exists());
        // Even when asked for: it is older than the move.
        let old = super::restore(&thrown(&["IMG_0001.jpg.xmp"], &[]), dirs);
        assert!(old.0.is_empty() && old.1.is_some());
        // IMG_0002.jpg went with its sidecar, and both come back; never over a file there.
        std::fs::write(photos.join("IMG_0002.jpg"), "new").unwrap();
        let taken = super::restore(&thrown(&["IMG_0002.jpg"], &["IMG_0002.jpg.xmp"]), dirs);
        assert!(taken.0.is_empty() && taken.1.is_some());
        std::fs::remove_file(photos.join("IMG_0002.jpg")).unwrap();
        let (back, error) = super::restore(&thrown(&["IMG_0002.jpg"], &["IMG_0002.jpg.xmp"]), dirs);
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
        let (back, error) = super::restore(&thrown(&["IMG_0003.jpg"], &[]), both);
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
        let (back, error) = super::restore(&thrown(&["IMG_0004.jpg"], &[]), both);
        assert_eq!((back, error), (vec![photos.join("IMG_0004.jpg")], None));
        let got = std::fs::read_to_string(photos.join("IMG_0004.jpg")).unwrap();
        assert_eq!(got, "mine");
        // One dated half an hour ahead, within the hour let through: still after the user's.
        let soon = crate::support::format::local_time(secs + 1800).replacen(' ', "T", 1);
        let img5 = photos.join("IMG_0005.jpg").display().to_string();
        at("IMG_0005.jpg", &img5, &soon, "planted");
        at("IMG_0005.2.jpg", &img5, &today, "mine");
        let (back, error) = super::restore(&thrown(&["IMG_0005.jpg"], &[]), both);
        assert_eq!((back, error), (vec![photos.join("IMG_0005.jpg")], None));
        let got = std::fs::read_to_string(photos.join("IMG_0005.jpg")).unwrap();
        assert_eq!(got, "mine");
        // One of the same name trashed a second before the move: another file's, the move's own
        // comes back. Alone, it is taken, as the move may have begun in that second.
        let before = crate::support::format::local_time(secs - 1).replacen(' ', "T", 1);
        let img6 = photos.join("IMG_0006.jpg").display().to_string();
        at("IMG_0006.jpg", &img6, &before, "other");
        at("IMG_0006.2.jpg", &img6, &today, "mine");
        let thrown6 = super::Thrown {
            since: now,
            ..thrown(&["IMG_0006.jpg"], &[])
        };
        let (back, error) = super::restore(&thrown6, both);
        assert_eq!((back, error), (vec![photos.join("IMG_0006.jpg")], None));
        let got = std::fs::read_to_string(photos.join("IMG_0006.jpg")).unwrap();
        assert_eq!(got, "mine");
        std::fs::remove_file(photos.join("IMG_0006.jpg")).unwrap();
        let (back, error) = super::restore(&thrown6, both);
        assert_eq!((back, error), (vec![photos.join("IMG_0006.jpg")], None));
        let got = std::fs::read_to_string(photos.join("IMG_0006.jpg")).unwrap();
        assert_eq!(got, "other");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
