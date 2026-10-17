//! Collections, as in Geeqie: named lists of images from any folders. Each is a Geeqie `.gqv`
//! file, one quoted path per line, kept in `$XDG_DATA_HOME/glimt/collections`; Geeqie's own
//! collections can be opened too.

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
        true => Err(format!("Not a valid collection name: \"{name}\"")),
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
        true => Err(format!("There is a collection called \"{name}\" already")),
        false => Ok(file),
    }
}

/// Give the collection in `file` the name `name`, in the same folder; the new file.
pub fn rename(file: &Path, name: &str) -> Result<PathBuf, String> {
    let name = valid(name)?;
    let new = file.with_file_name(format!("{name}.gqv"));
    if new.exists() {
        return Err(format!("There is a collection called \"{name}\" already"));
    }
    std::fs::rename(file, &new).map_err(|e| format!("Could not rename the collection: {e}"))?;
    Ok(new)
}

pub fn load(file: &Path) -> std::io::Result<Vec<PathBuf>> {
    let text = std::fs::read(file)?;
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
fn percent_decoded(uri: &[u8]) -> Vec<u8> {
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

/// What a window writes for a collection it shows as `ours` and read as `read`, while the file
/// now holds `on_disk`: its own list, then what was added there since (by another window, or
/// Add to Collection), which would be lost otherwise.
pub fn merged(
    mut ours: Vec<PathBuf>,
    read: &std::collections::HashSet<PathBuf>,
    on_disk: Vec<PathBuf>,
) -> Vec<PathBuf> {
    let mut have: std::collections::HashSet<PathBuf> = ours.iter().cloned().collect();
    for path in on_disk {
        if !read.contains(&path) && have.insert(path.clone()) {
            ours.push(path);
        }
    }
    ours
}

/// `files` in the order of `saved`, a collection's file: shuffled ones put back, those it lacks
/// (added since) after them, in the order they had.
pub fn in_saved_order(saved: &[PathBuf], files: Vec<PathBuf>) -> Vec<PathBuf> {
    let have: std::collections::HashSet<&PathBuf> = files.iter().collect();
    let mut ordered: Vec<PathBuf> = (saved.iter())
        .filter(|p| have.contains(p))
        .cloned()
        .collect();
    let placed: std::collections::HashSet<PathBuf> = ordered.iter().cloned().collect();
    ordered.extend(files.into_iter().filter(|p| !placed.contains(p)));
    ordered
}

/// Add `paths` to the collection in `file` (made if new), leaving out those it has.
pub fn append(file: &Path, paths: &[PathBuf]) -> std::io::Result<usize> {
    let _held = crate::config::file_lock(file);
    // Only a collection that is not there yet starts empty: one that cannot be read would be
    // written over.
    let mut all = match load(file) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        loaded => loaded?,
    };
    let before = all.len();
    // A set, as Select All in a large folder adds hundreds of thousands at once.
    let mut have: std::collections::HashSet<PathBuf> = all.iter().cloned().collect();
    for path in paths {
        if have.insert(path.clone()) {
            all.push(path.clone());
        }
    }
    save(file, &all)?;
    Ok(all.len() - before)
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

#[cfg(test)]
mod tests {
    /// Shuffled files back in the saved order; one added since goes last.
    #[test]
    fn puts_back_the_saved_order() {
        use std::path::PathBuf;
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
        // Added to: a new one is made, one that cannot be read is left as it is.
        let dir = std::env::temp_dir().join(format!("glimt-test-gqv-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
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

    /// A window's save keeps its own removals and what others added meanwhile.
    #[test]
    fn merges_with_what_others_added() {
        let p = |s: &str| PathBuf::from(s);
        let read = [p("/a"), p("/b"), p("/c")].into_iter().collect();
        // This window removed /b; another added /d; /c is in both.
        let ours = vec![p("/a"), p("/c")];
        let on_disk = vec![p("/a"), p("/b"), p("/c"), p("/d")];
        assert_eq!(
            super::merged(ours, &read, on_disk),
            [p("/a"), p("/c"), p("/d")]
        );
    }

    /// Collection files broken in thousands of ways: read without a panic.
    #[test]
    fn survives_broken_files() {
        let paths = [PathBuf::from("/a b/c.jpg"), PathBuf::from("/x/\"q\".png")];
        for text in crate::broken(&super::to_text(&paths)) {
            super::parse(&text);
        }
    }
}
