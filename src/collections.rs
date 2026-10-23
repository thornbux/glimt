//! Collections, as in Geeqie: named lists of images from any folders. Each is a Geeqie `.gqv`
//! file, one quoted path per line, kept in `$XDG_DATA_HOME/glimt/collections`; Geeqie's own
//! collections can be opened too.

use crate::actions::Action;
use crate::tf;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// Where new collections go.
pub fn dir() -> Option<PathBuf> {
    crate::config::xdg_dir("XDG_DATA_HOME", ".local/share", "glimt").map(|d| d.join("collections"))
}

/// Plain lists of images, as a script or a music player writes them: one path on each line,
/// relative to the list or absolute, `#` for comments. Opened as a collection, never written.
const LISTS: &[&str] = &["txt", "m3u", "m3u8"];

/// A `.gqv` collection, or a plain list: what opens as a list of images from any folders.
pub fn is_collection(path: &Path) -> bool {
    path.extension().is_some_and(|e| {
        e.eq_ignore_ascii_case("gqv") || LISTS.iter().any(|l| e.eq_ignore_ascii_case(l))
    }) && path.is_file()
}

/// Whether `file` is a plain list, which Glimt shows but never writes: a change in the window
/// (an image taken out, another order) stays there.
pub fn is_list(file: &Path) -> bool {
    (file.extension()).is_some_and(|e| LISTS.iter().any(|l| e.eq_ignore_ascii_case(l)))
}

/// The saved collections: (name, file), by name.
pub fn list() -> Vec<(String, PathBuf)> {
    let entries = dir().and_then(|d| std::fs::read_dir(d).ok());
    let mut all: Vec<(String, PathBuf)> = (entries.into_iter().flatten().flatten())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "gqv"))
        .filter_map(|p| Some((p.file_stem()?.to_string_lossy().into_owned(), p)))
        .collect();
    all.sort();
    all
}

/// `name` trimmed, if it can be the name of a collection's file.
fn valid(name: &str) -> Result<&str, String> {
    let name = name.trim();
    match name.is_empty() || name.contains('/') || name.starts_with('.') {
        true => Err(tf!("Not a valid collection name: \"{name}\"", name)),
        false => Ok(name),
    }
}

/// The file of a new collection called `name`, or why there can be none.
pub fn new_file(name: &str) -> Result<PathBuf, String> {
    let name = valid(name)?;
    let file = dir()
        .ok_or("no data folder ($HOME is not set)")?
        .join(format!("{name}.gqv"));
    match file.exists() {
        true => Err(tf!("There is a collection called \"{name}\" already", name)),
        false => Ok(file),
    }
}

/// Give the collection in `file` the name `name`, in the same folder; the new file.
pub fn rename(file: &Path, name: &str) -> Result<PathBuf, String> {
    let name = valid(name)?;
    // Under the collection's lock, as every edit of it: another window's edit lands before or
    // after the rename, never in between, where it would make the old name again.
    let _held = crate::config::file_lock(file);
    let new = file.with_file_name(format!("{name}.gqv"));
    // Never over one of that name, also one another window makes between the look and the
    // rename.
    match crate::file_ops::rename_never_over(file, &new) {
        Ok(()) => Ok(new),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            Err(tf!("There is a collection called \"{name}\" already", name))
        }
        Err(e) => Err(format!("Could not rename the collection: {e}")),
    }
}

pub fn load(file: &Path) -> std::io::Result<Vec<PathBuf>> {
    let text = crate::decode::read_limited(file)?;
    Ok(match is_list(file) {
        true => parse_list(&text, file.parent().unwrap_or(Path::new("/"))),
        false => parse(&text),
    })
}

/// The paths of a plain list, those relative to it joined to `base`, its folder; each once.
/// Lines may be quoted, end in CR, or be file:// URIs, as players write them; a URI of another
/// computer is left out.
fn parse_list(text: &[u8], base: &Path) -> Vec<PathBuf> {
    let mut seen = std::collections::HashSet::new();
    (text.split(|&b| b == b'\n'))
        .map(|line| line.trim_ascii())
        .filter(|line| !line.is_empty() && !line.starts_with(b"#"))
        .map(|line| match line {
            [b'"', inner @ .., b'"'] => inner,
            line => line,
        })
        .filter_map(|line| match line.strip_prefix(b"file://") {
            Some(uri) => {
                let uri = uri.strip_prefix(b"localhost").unwrap_or(uri);
                let path = PathBuf::from(OsStr::from_bytes(&percent_decoded(uri)));
                path.is_absolute().then_some(path)
            }
            None => Some(lexical(&base.join(OsStr::from_bytes(line)))),
        })
        .filter(|p| seen.insert(p.clone()))
        .collect()
}

/// `path` with its `.` and `..` worked out as written, without looking at the disk: the same
/// image named "../Pictures/a.jpg" in a list is "/home/u/Pictures/a.jpg", the key its stars are
/// kept by when it is shown from its folder.
fn lexical(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => drop(out.pop()),
            part => out.push(part),
        }
    }
    out
}

/// A URI's path with its %XX escapes undone.
pub fn percent_decoded(uri: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(uri.len());
    let mut i = 0;
    while i < uri.len() {
        // Two hex digits: from_str_radix alone takes "+4" too.
        let hex = (uri.get(i + 1..i + 3))
            .filter(|h| h.iter().all(u8::is_ascii_hexdigit))
            .and_then(|h| std::str::from_utf8(h).ok());
        match (uri[i], hex.and_then(|h| u8::from_str_radix(h, 16).ok())) {
            (b'%', Some(b)) => {
                out.push(b);
                i += 3;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    out
}

/// Write `paths` to `file`, through a temporary file so a crash never leaves half of it.
pub fn save(file: &Path, paths: &[PathBuf]) -> std::io::Result<()> {
    crate::config::replace_file(file, to_text(paths))
}

/// Make `change` to the collection in `file` as it is on disk now, under its lock: each window
/// writes only its own change (images taken out, renamed, put in order), never its whole list,
/// so what other windows changed meanwhile stays. With `new` a file not there yet starts empty;
/// else that is an error, so a collection renamed or trashed elsewhere is not made again. One
/// that cannot be read is never written over: what it holds would be lost.
pub fn edit(file: &Path, new: bool, change: impl FnOnce(&mut Vec<PathBuf>)) -> std::io::Result<()> {
    let _held = crate::config::file_lock(file);
    let (mut all, was_new) = match load(file) {
        Err(e) if new && e.kind() == std::io::ErrorKind::NotFound => (Vec::new(), true),
        loaded => (loaded?, false),
    };
    let before = all.clone();
    change(&mut all);
    // Nothing changed (an unrelated folder renamed): the file stays as it is.
    if all == before {
        return Ok(());
    }
    // A new one never lands over a collection another window made meanwhile.
    match was_new {
        true => crate::config::create_file(file, to_text(&all)),
        false => save(file, &all),
    }
}

/// Add `paths` to `all`, leaving out those it has; how many were added.
pub fn add(all: &mut Vec<PathBuf>, paths: &[PathBuf]) -> usize {
    // A set, as Select All in a large folder adds hundreds of thousands at once.
    let mut have: HashSet<PathBuf> = all.iter().cloned().collect();
    let before = all.len();
    all.extend(paths.iter().filter(|p| have.insert((*p).clone())).cloned());
    all.len() - before
}

/// `all` in the order of `ours`, a window's list put in order; those it lacks (added elsewhere
/// since) after them, and those only in `ours` (taken out elsewhere) left out.
pub fn reorder(all: &mut Vec<PathBuf>, ours: &[PathBuf]) {
    let there: HashSet<&PathBuf> = all.iter().collect();
    let placed: HashSet<&PathBuf> = ours.iter().collect();
    let mut ordered: Vec<PathBuf> = ours.iter().filter(|p| there.contains(p)).cloned().collect();
    ordered.extend(all.iter().filter(|p| !placed.contains(p)).cloned());
    *all = ordered;
}

/// `files` in the order of `saved`, a collection's file: shuffled ones put back, those it lacks
/// (added since) after them, in the order they had.
pub fn in_saved_order(saved: &[PathBuf], files: Vec<PathBuf>) -> Vec<PathBuf> {
    let have: HashSet<&PathBuf> = files.iter().collect();
    let mut ordered: Vec<PathBuf> = (saved.iter())
        .filter(|p| have.contains(p))
        .cloned()
        .collect();
    let placed: HashSet<PathBuf> = ordered.iter().cloned().collect();
    ordered.extend(files.into_iter().filter(|p| !placed.contains(p)));
    ordered
}

/// Add `paths` to the collection in `file` (made if new), leaving out those it has.
pub fn append(file: &Path, paths: &[PathBuf]) -> std::io::Result<usize> {
    // The first collection of a new install: its folder is not there yet.
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut added = 0;
    edit(file, true, |all| added = add(all, paths))?;
    Ok(added)
}

/// Geeqie's format: comment lines start with #, a path is in double quotes.
fn parse(text: &[u8]) -> Vec<PathBuf> {
    // A file listed twice (in a .gqv edited by hand) is one image: shown, deleted and found
    // among duplicates once.
    let mut seen = std::collections::HashSet::new();
    text.split(|&b| b == b'\n')
        .filter_map(|line| {
            let rest = line.strip_prefix(b"\"")?;
            let end = rest.iter().rposition(|&b| b == b'"')?;
            Some(PathBuf::from(OsStr::from_bytes(&rest[..end])))
        })
        .filter(|p| p.is_absolute() && seen.insert(p.clone()))
        .collect()
}

fn to_text(paths: &[PathBuf]) -> Vec<u8> {
    // Geeqie still writes the marker of GQview, which it came from.
    let head = format!(
        "#GQview collection\n#created with Glimt version {}\n",
        env!("CARGO_PKG_VERSION")
    );
    let mut text = head.into_bytes();
    for path in paths {
        let bytes = path.as_os_str().as_bytes();
        if !bytes.contains(&b'\n') {
            text.push(b'"');
            text.extend(bytes);
            text.extend(b"\"\n");
        }
    }
    text.extend(b"#end\n");
    text
}

/// The new order of a list of `n` after `action` moves the `picked` places: one earlier or later
/// (each picked passes one that is not, never one another), or all to the start or end, among
/// themselves in the order they had.
pub(crate) fn moved(n: usize, picked: &HashSet<usize>, action: Action) -> Vec<usize> {
    let mut order: Vec<usize> = (0..n).collect();
    let (first, rest): (Vec<usize>, Vec<usize>) = order.iter().partition(|i| picked.contains(i));
    match action {
        Action::MoveFirst => order = [first, rest].concat(),
        Action::MoveLast => order = [rest, first].concat(),
        Action::MoveEarlier => {
            for at in 1..n {
                if picked.contains(&order[at]) && !picked.contains(&order[at - 1]) {
                    order.swap(at - 1, at);
                }
            }
        }
        Action::MoveLater => {
            for at in (0..n.saturating_sub(1)).rev() {
                if picked.contains(&order[at]) && !picked.contains(&order[at + 1]) {
                    order.swap(at, at + 1);
                }
            }
        }
        _ => {}
    }
    order
}

/// The new order of a list of `n` after the `picked` places are dragged before place `to` (`n`:
/// to the end), among themselves in the order they had.
pub(crate) fn dropped(n: usize, picked: &HashSet<usize>, to: usize) -> Vec<usize> {
    let (first, mut rest): (Vec<usize>, Vec<usize>) = (0..n).partition(|i| picked.contains(i));
    let at = rest.iter().take_while(|&&i| i < to).count();
    rest.splice(at..at, first);
    rest
}

#[cfg(test)]
mod tests {
    /// A window's order: images only in the file (added elsewhere) go last, those taken out
    /// elsewhere stay out.
    #[test]
    fn reorders_what_is_there() {
        let p = |names: &[&str]| -> Vec<PathBuf> { names.iter().map(PathBuf::from).collect() };
        let mut all = p(&["/a", "/b", "/c", "/new"]);
        super::reorder(&mut all, &p(&["/c", "/gone", "/a", "/b"]));
        assert_eq!(all, p(&["/c", "/a", "/b", "/new"]));
    }

    /// Shuffled files back in the saved order; one added since goes last.
    #[test]
    fn puts_back_the_saved_order() {
        let p = |names: &[&str]| -> Vec<PathBuf> { names.iter().map(PathBuf::from).collect() };
        let saved = p(&["/a", "/b", "/c", "/gone"]);
        assert_eq!(
            super::in_saved_order(&saved, p(&["/c", "/new", "/a", "/b"])),
            p(&["/a", "/b", "/c", "/new"])
        );
    }

    use std::path::PathBuf;

    /// A plain list: relative to its folder (also with ..) or absolute, quoted or not, file://
    /// URIs (of this computer), comments, blank lines and CR, each image once and in the order
    /// written.
    #[test]
    fn reads_plain_lists() {
        let text = b"#EXTM3U\r\nb.jpg\r\n\n  \"sub/a b.jpg\"\n/abs/c.png\n\
                     file:///abs/with%20space.jpg\n# a comment\nb.jpg\n../Pictures/./d.jpg\n\
                     file://localhost/abs/e.jpg\nfile://nas/abs/f.jpg\nfile:///abs/100%+4.jpg\n";
        let paths = super::parse_list(text, std::path::Path::new("/lists"));
        assert_eq!(
            paths,
            [
                "/lists/b.jpg",
                "/lists/sub/a b.jpg",
                "/abs/c.png",
                "/abs/with space.jpg",
                "/Pictures/d.jpg",
                "/abs/e.jpg",
                "/abs/100%+4.jpg"
            ]
            .map(PathBuf::from)
        );
        assert!(super::is_list(std::path::Path::new("x.M3U")));
        assert!(!super::is_list(std::path::Path::new("x.gqv")));
    }

    /// Plain lists broken in thousands of ways: read without a panic.
    #[test]
    fn survives_broken_lists() {
        let seed = b"#EXTM3U\nb.jpg\n\"sub/a.jpg\"\nfile:///x%2Fy%zz.jpg\n";
        for bytes in crate::broken(seed) {
            super::parse_list(&bytes, std::path::Path::new("/"));
        }
    }

    /// A rename waits for an edit under way in another window, so the edit lands in the file
    /// before the rename, not in a new file of the old name.
    #[test]
    fn rename_waits_for_an_edit() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-rename-lock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("trip.gqv");
        std::fs::write(&file, "#GQview collection\n\"/a.jpg\"\n").unwrap();
        let held = crate::config::file_lock(&file).unwrap();
        let renaming = {
            let file = file.clone();
            std::thread::spawn(move || super::rename(&file, "trip 2024"))
        };
        std::thread::sleep(std::time::Duration::from_millis(150));
        assert!(file.exists(), "renamed while the edit held the lock");
        drop(held);
        let new = renaming.join().unwrap().unwrap();
        assert!(!file.exists() && new.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Renamed in its own folder, never over another collection or to a name that is no file.
    #[test]
    fn renames_in_place() {
        let dir = std::env::temp_dir().join(format!("glimt-test-coll-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a.gqv"), dir.join("b.gqv"));
        std::fs::write(&a, "a").unwrap();
        std::fs::write(&b, "b").unwrap();
        assert!(super::rename(&a, "b").is_err());
        assert!(super::rename(&a, "x/y").is_err());
        assert_eq!(
            super::rename(&a, " Travel ").unwrap(),
            dir.join("Travel.gqv")
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("Travel.gqv")).unwrap(),
            "a"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reads_and_writes_geeqie_collections() {
        let paths = [
            PathBuf::from("/a b/c.jpg"),
            PathBuf::from("/x/\"quoted\".png"),
        ];
        let text = super::to_text(&paths);
        assert!(text.starts_with(b"#GQview collection\n"));
        assert_eq!(super::parse(&text), paths);
        // As Geeqie writes it, with its geometry line; relative paths and a second line of the
        // same file are left out.
        let geeqie =
            b"#GQview collection\n#geometry: 0 0 400 300\n\"/p/1.jpg\"\n\"rel.jpg\"\n\"/p/1.jpg\"\n#end\n";
        assert_eq!(super::parse(geeqie), [PathBuf::from("/p/1.jpg")]);
        // Added to: a new one is made, also in a folder not there yet (a new install), and one
        // that cannot be read is left as it is.
        let dir = std::env::temp_dir().join(format!("glimt-test-gqv-{}", std::process::id()));
        let file = dir.join("a.gqv");
        assert_eq!(super::append(&file, &paths).unwrap(), 2);
        assert_eq!(super::append(&file, &paths[..1]).unwrap(), 0);
        let new = PathBuf::from("/n.jpg");
        assert_eq!(super::append(&file, &[new.clone(), new]).unwrap(), 1);
        // A link to itself: reading fails, though not as missing.
        let unreadable = dir.join("b.gqv");
        std::os::unix::fs::symlink(&unreadable, &unreadable).unwrap();
        assert!(super::append(&unreadable, &paths).is_err());
        assert!(unreadable.symlink_metadata().unwrap().is_symlink());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Two windows' changes to one collection both stay: one takes an image out, the other
    /// renames another; a gone collection is not made again.
    #[test]
    fn keeps_what_other_windows_changed() {
        let p = |s: &str| PathBuf::from(s);
        let dir = std::env::temp_dir().join(format!("glimt-test-edits-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("c.gqv");
        super::save(&file, &[p("/a"), p("/b"), p("/c")]).unwrap();
        super::edit(&file, false, |all| all.retain(|x| x != &p("/a"))).unwrap();
        super::edit(&file, false, |all| {
            (all.iter_mut().filter(|x| **x == p("/c"))).for_each(|x| *x = p("/d"))
        })
        .unwrap();
        assert_eq!(super::load(&file).unwrap(), [p("/b"), p("/d")]);
        // A change that changes nothing leaves the file as it was.
        let written = std::fs::metadata(&file).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        super::edit(&file, false, |all| all.retain(|_| true)).unwrap();
        assert_eq!(
            std::fs::metadata(&file).unwrap().modified().unwrap(),
            written
        );
        let gone = dir.join("gone.gqv");
        assert!(super::edit(&gone, false, |all| all.clear()).is_err());
        assert!(!gone.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Collection files broken in thousands of ways: read without a panic.
    #[test]
    fn survives_broken_files() {
        let paths = [PathBuf::from("/a b/c.jpg"), PathBuf::from("/x/\"q\".png")];
        for text in crate::broken(&super::to_text(&paths)) {
            super::parse(&text);
        }
    }

    #[test]
    fn moves_in_a_collection() {
        use super::{Action, moved};
        let set = |v: &[usize]| v.iter().copied().collect();
        // 0 1 2 3 4 5 with 1, 2 and 5 picked.
        let picked = set(&[1, 2, 5]);
        assert_eq!(moved(6, &picked, Action::MoveEarlier), [1, 2, 0, 3, 5, 4]);
        assert_eq!(moved(6, &picked, Action::MoveLater), [0, 3, 1, 2, 4, 5]);
        assert_eq!(moved(6, &picked, Action::MoveFirst), [1, 2, 5, 0, 3, 4]);
        assert_eq!(moved(6, &picked, Action::MoveLast), [0, 3, 4, 1, 2, 5]);
        // At the edge already: where it was.
        assert_eq!(moved(3, &set(&[0]), Action::MoveEarlier), [0, 1, 2]);
        assert_eq!(moved(0, &set(&[]), Action::MoveLater), Vec::<usize>::new());
        // Dragged before 4, before 0, to the end, and onto a place of their own.
        use super::dropped;
        assert_eq!(dropped(6, &picked, 4), [0, 3, 1, 2, 5, 4]);
        assert_eq!(dropped(6, &picked, 0), [1, 2, 5, 0, 3, 4]);
        assert_eq!(dropped(6, &picked, 6), [0, 3, 4, 1, 2, 5]);
        assert_eq!(dropped(6, &picked, 2), [0, 1, 2, 5, 3, 4]);
    }
}
