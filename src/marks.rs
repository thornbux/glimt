//! Optional marks and ratings (`cargo build --features marks`): Geeqie's marks 1–6 and 0–5
//! stars, kept by full path in `$XDG_STATE_HOME/glimt/marks`, one file as "bits\tstars\tpath"
//! lines. They follow files that Glimt copies, moves and renames, and go with those it deletes;
//! files renamed or moved outside Glimt lose them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Default)]
pub struct Marks {
    /// Mark bits (bit 0 = mark 1) and stars, by path; files with neither are left out.
    map: HashMap<PathBuf, (u8, u8)>,
    /// When the file was written, as it was read: another window's changes since are read again.
    read: Option<std::time::SystemTime>,
}

fn file() -> Option<PathBuf> {
    crate::config::xdg_dir("XDG_STATE_HOME", ".local/state", "glimt").map(|d| d.join("marks"))
}

impl Marks {
    pub fn load() -> Self {
        let file = file();
        let read = file
            .as_ref()
            .and_then(|f| std::fs::metadata(f).ok()?.modified().ok());
        let text = file.and_then(|f| std::fs::read(f).ok()).unwrap_or_default();
        Marks {
            map: parse(&text),
            read,
        }
    }

    /// Read the file again if it changed since: what another Glimt window marked meanwhile, so
    /// a mark toggled here goes from what is there. Not while this window's own changes are
    /// still on their way to it: those would be read back as they were before.
    pub fn refresh(&mut self) {
        if WRITING.load(std::sync::atomic::Ordering::Relaxed) > 0 {
            return;
        }
        let changed = file().and_then(|f| std::fs::metadata(f).ok()?.modified().ok());
        if changed != self.read {
            *self = Self::load();
        }
    }

    /// Every file with a mark or stars, and them.
    pub fn marked(&self) -> impl Iterator<Item = (&Path, (u8, u8))> {
        self.map.iter().map(|(p, &v)| (p.as_path(), v))
    }

    /// (mark bits, stars) of `path`.
    pub fn get(&self, path: &Path) -> (u8, u8) {
        self.map.get(path).copied().unwrap_or_default()
    }

    /// Toggle mark `n` (1–6) as Geeqie does: set on all of `paths`, or off if all have it.
    pub fn toggle(&mut self, paths: &[PathBuf], n: u8) {
        self.refresh();
        let bit = 1 << (n - 1);
        let all = paths.iter().all(|p| self.get(p).0 & bit != 0);
        self.change(paths, Change::Mark(bit, !all));
    }

    pub fn rate(&mut self, paths: &[PathBuf], stars: u8) {
        self.change(paths, Change::Stars(stars));
    }

    /// Files deleted, or folders with all in them: their marks go, so a new file of the same name
    /// does not get them.
    pub fn forget(&mut self, paths: &[PathBuf]) {
        let gone: std::collections::HashSet<&Path> = paths.iter().map(PathBuf::as_path).collect();
        // Each marked file and the folders above it, not every path deleted: a few thousand
        // marked against 100 files at a time to the trash.
        let marked: Vec<PathBuf> = (self.map.keys())
            .filter(|k| k.ancestors().any(|a| gone.contains(a)))
            .cloned()
            .collect();
        if !marked.is_empty() {
            self.change(&marked, Change::Clear);
        }
    }

    /// Folder `old` renamed to `new`: the marks of the files in it go along.
    pub fn put_folder(&mut self, old: &Path, new: &Path) {
        let pairs = (self.map.keys())
            .filter_map(|k| Some((k.clone(), new.join(k.strip_prefix(old).ok()?))))
            .collect();
        self.put(pairs, false);
    }

    /// Files copied (`keep`) or moved and renamed `(from, to)`: the new ones get the marks.
    pub fn put(&mut self, pairs: Vec<(PathBuf, PathBuf)>, keep: bool) {
        let pairs: Vec<(PathBuf, PathBuf)> = (pairs.into_iter())
            .filter(|(from, to)| self.map.contains_key(from) || self.map.contains_key(to))
            .collect();
        if !pairs.is_empty() {
            put(&mut self.map, &pairs, keep);
            save(Edit::Put(pairs, keep));
        }
    }

    fn change(&mut self, paths: &[PathBuf], change: Change) {
        apply(&mut self.map, paths, change);
        save(Edit::Change(paths.to_vec(), change));
    }
}

/// Changes sent to the writer and not yet in the file.
static WRITING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// What a window did, applied to the file as it is then.
enum Edit {
    Change(Vec<PathBuf>, Change),
    Put(Vec<(PathBuf, PathBuf)>, bool),
}

/// A change, rather than the new value, so that applied to the file it keeps what another
/// Glimt window set on the same file meanwhile.
#[derive(Clone, Copy)]
enum Change {
    /// A mark bit set or cleared.
    Mark(u8, bool),
    Stars(u8),
    /// No marks and no stars.
    Clear,
}

/// Apply `change` to each path's marks and stars; a path with neither is removed.
fn apply(map: &mut HashMap<PathBuf, (u8, u8)>, paths: &[PathBuf], change: Change) {
    for path in paths {
        let (bits, stars) = map.get(path).copied().unwrap_or_default();
        let new = match change {
            Change::Mark(bit, true) => (bits | bit, stars),
            Change::Mark(bit, false) => (bits & !bit, stars),
            Change::Stars(stars) => (bits, stars),
            Change::Clear => (0, 0),
        };
        match new {
            (0, 0) => drop(map.remove(path)),
            v => drop(map.insert(path.clone(), v)),
        }
    }
}

/// The marks of each `from` to its `to`, kept on `from` too when copied. All are taken before
/// any is put: in a Rename Many that swaps or shifts names, a `to` is another pair's `from`.
fn put(map: &mut HashMap<PathBuf, (u8, u8)>, pairs: &[(PathBuf, PathBuf)], keep: bool) {
    let taken: Vec<Option<(u8, u8)>> = (pairs.iter())
        .map(|(from, _)| match keep {
            true => map.get(from).copied(),
            false => map.remove(from),
        })
        .collect();
    for ((_, to), marks) in pairs.iter().zip(taken) {
        match marks {
            Some(v) => drop(map.insert(to.clone(), v)),
            None => drop(map.remove(to)),
        }
    }
}

fn apply_edit(map: &mut HashMap<PathBuf, (u8, u8)>, edit: &Edit) {
    match edit {
        Edit::Change(paths, change) => apply(map, paths, *change),
        Edit::Put(pairs, keep) => put(map, pairs, *keep),
    }
}

fn to_text(map: &HashMap<PathBuf, (u8, u8)>) -> Vec<u8> {
    let mut text = Vec::new();
    for (path, (bits, stars)) in map {
        let path = path.as_os_str().as_encoded_bytes();
        if !path.contains(&b'\n') {
            text.extend(format!("{bits}\t{stars}\t").as_bytes());
            text.extend(path);
            text.push(b'\n');
        }
    }
    text
}

/// Only the changes go to the file: one writer thread, so they land in order, merges them into
/// the file as it is now, so the marks another Glimt window wrote meanwhile are kept. Renamed
/// into place so a crash never leaves half a file.
/// shortcut: the whole file each time, fine for thousands of marked files; write changes only
/// if hundreds of thousands are marked.
#[cfg(not(test))]
fn save(edit: Edit) {
    use std::sync::atomic::Ordering::Relaxed;
    // Through the thread that writes Glimt's own files, which Glimt waits for before it ends:
    // a mark set, or moved with its file, just before closing is not lost.
    WRITING.fetch_add(1, Relaxed);
    crate::config::write_later(move || {
        if let Some(file) = file() {
            write(&file, &[edit]);
        }
        WRITING.fetch_sub(1, Relaxed);
    });
}

/// `changes` applied to the marks file as it is now.
#[cfg(not(test))]
fn write(file: &Path, changes: &[Edit]) {
    // Another window's read and write must not fall between ours, or its change is lost.
    let _locked = crate::config::file_lock(file);
    // A file that is there but cannot be read is not written over with these changes alone:
    // every other mark would be lost.
    let mut map = match std::fs::read(file) {
        Ok(text) => parse(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
        Err(e) => {
            return crate::config::save_failed(format!(
                "Marks and stars not saved: {} cannot be read: {e}",
                file.display()
            ));
        }
    };
    for change in changes {
        apply_edit(&mut map, change);
    }
    crate::config::write_atomic(file, to_text(&map));
}

#[cfg(test)]
fn save(_: Edit) {}

/// The marks file: a line for each file.
fn parse(text: &[u8]) -> HashMap<PathBuf, (u8, u8)> {
    text.split(|&b| b == b'\n').filter_map(parse_line).collect()
}

fn parse_line(line: &[u8]) -> Option<(PathBuf, (u8, u8))> {
    let mut parts = line.splitn(3, |&b| b == b'\t');
    let number = |b: &[u8]| std::str::from_utf8(b).ok()?.parse::<u8>().ok();
    let (bits, stars) = (number(parts.next()?)?, number(parts.next()?)?);
    // SAFETY: the bytes were written from an OsStr by `save`, or come from the user's file;
    // on Unix any bytes are a valid OsStr.
    let path = unsafe { std::ffi::OsStr::from_encoded_bytes_unchecked(parts.next()?) };
    Some((PathBuf::from(path), (bits & 0x3f, stars.min(5))))
}

/// Write `stars` as xmp:Rating into the sidecar `<file>.xmp` that darktable and digiKam read.
/// An existing sidecar keeps everything else: only the rating changes, or is added to its first
/// rdf:Description. Without a sidecar, 0 stars writes none.
pub fn write_xmp(path: &Path, stars: u8) -> Result<(), String> {
    crate::meta::update_sidecar(path, stars == 0, |x| set_rating(x, stars))
}

/// `xmp` with its rating set to `stars`, as an attribute or element, or added; `None` if it has
/// no rdf:Description to put it in.
fn set_rating(xmp: &str, stars: u8) -> Option<String> {
    let number_after = |key: &str| {
        let start = xmp.find(key)? + key.len();
        let len = xmp[start..].find(|c: char| !c.is_ascii_digit() && c != '-')?;
        Some((start, start + len))
    };
    if let Some((a, b)) = number_after("xmp:Rating=\"").or_else(|| number_after("<xmp:Rating>")) {
        return Some(format!("{}{stars}{}", &xmp[..a], &xmp[b..]));
    }
    let at = xmp.find("<rdf:Description")? + "<rdf:Description".len();
    // Declared on this Description or above it, as in meta::set.
    let tag_end = at + xmp[at..].find('>')?;
    let ns = if xmp[..tag_end].contains("xmlns:xmp=") {
        ""
    } else {
        " xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\""
    };
    Some(format!(
        "{}{ns} xmp:Rating=\"{stars}\"{}",
        &xmp[..at],
        &xmp[at..]
    ))
}

/// "★★★" for the stars and "1 4" for the marks, for the file list and overlay.
pub fn text((bits, stars): (u8, u8)) -> String {
    let marks: Vec<String> = (1..=6)
        .filter(|n| bits & (1 << (n - 1)) != 0)
        .map(|n| n.to_string())
        .collect();
    let stars = "★".repeat(stars as usize);
    match (marks.is_empty(), stars.is_empty()) {
        (true, _) => stars,
        (false, true) => marks.join(" "),
        (false, false) => format!("{stars}  {}", marks.join(" ")),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn toggles_rates_and_reads_lines() {
        use std::path::PathBuf;
        let mut m = super::Marks::default();
        let (a, b) = (PathBuf::from("/a.jpg"), PathBuf::from("/b.jpg"));
        let both = [a.clone(), b.clone()];
        m.toggle(&[a.clone()], 2);
        // Mark 2 on only one of them: the toggle sets it on both, the next clears both.
        m.toggle(&both, 2);
        assert_eq!((m.get(&a), m.get(&b)), ((0b10, 0), (0b10, 0)));
        m.rate(&[b.clone()], 3);
        assert_eq!(super::text(m.get(&b)), "★★★  2");
        m.toggle(&both, 2);
        assert_eq!(m.get(&a), (0, 0));
        assert!(!m.map.contains_key(&a));
        assert_eq!(
            super::parse_line(b"5\t9\t/x y.jpg"),
            Some((PathBuf::from("/x y.jpg"), (5, 5)))
        );
        assert_eq!(super::parse_line(b"junk"), None);
        // Another window's file: its marks stay when this one's changes are applied, also on
        // the same file.
        let other = super::to_text(&std::collections::HashMap::from([(a.clone(), (1, 0))]));
        let mut map: std::collections::HashMap<_, _> = other
            .split(|&b| b == b'\n')
            .filter_map(super::parse_line)
            .collect();
        super::apply(&mut map, &[a.clone()], super::Change::Mark(4, true));
        super::apply(&mut map, &[b.clone()], super::Change::Stars(4));
        assert_eq!((map[&a], map[&b]), ((5, 0), (0, 4)));
        // A darktable sidecar keeps its fields; the rating is changed or added.
        let dt = r#"<rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="1" darktable:x="y">"#;
        assert!(
            super::set_rating(dt, 4)
                .unwrap()
                .contains(r#"xmp:Rating="4" darktable:x="y""#)
        );
        let none = r#"<rdf:Description rdf:about="">"#;
        assert_eq!(
            super::set_rating(none, 2).unwrap(),
            r#"<rdf:Description xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="2" rdf:about="">"#
        );
        assert!(
            super::set_rating("<xmp:Rating>5</xmp:Rating><rdf:Description>", 0)
                .unwrap()
                .starts_with("<xmp:Rating>0<")
        );
        assert_eq!(super::set_rating("nothing", 1), None);
    }

    /// Marks follow a move and a rename, are kept by a copy, go with a delete, and a new file
    /// where a deleted one was does not get them.
    #[test]
    fn follow_their_files() {
        use std::path::PathBuf;
        let p = PathBuf::from;
        let mut m = super::Marks::default();
        m.rate(&[p("/a.jpg"), p("/b.jpg")], 4);
        m.toggle(&[p("/a.jpg")], 1);
        m.put(vec![(p("/a.jpg"), p("/to/a.jpg"))], false);
        assert_eq!(
            (m.get(&p("/a.jpg")), m.get(&p("/to/a.jpg"))),
            ((0, 0), (1, 4))
        );
        m.put(vec![(p("/b.jpg"), p("/b copy.jpg"))], true);
        assert_eq!(
            (m.get(&p("/b.jpg")), m.get(&p("/b copy.jpg"))),
            ((0, 4), (0, 4))
        );
        m.forget(&[p("/b.jpg")]);
        assert_eq!(m.get(&p("/b.jpg")), (0, 0));
        // A folder renamed: its files' marks go along; trashed: they go.
        m.rate(&[p("/trip/a.jpg")], 5);
        m.put_folder(&p("/trip"), &p("/trip 2024"));
        assert_eq!(m.get(&p("/trip 2024/a.jpg")), (0, 5));
        m.forget(&[p("/trip 2024")]);
        assert_eq!(m.get(&p("/trip 2024/a.jpg")), (0, 0));
        // An old entry where an unmarked file lands is cleared.
        m.put(vec![(p("/c.jpg"), p("/b copy.jpg"))], false);
        assert_eq!(m.get(&p("/b copy.jpg")), (0, 0));
        // Rename Many swapping names, and shifting them: each keeps its own.
        m.rate(&[p("/x.jpg")], 1);
        m.rate(&[p("/y.jpg")], 2);
        m.put(
            vec![(p("/x.jpg"), p("/y.jpg")), (p("/y.jpg"), p("/x.jpg"))],
            false,
        );
        assert_eq!((m.get(&p("/x.jpg")).1, m.get(&p("/y.jpg")).1), (2, 1));
        m.put(
            vec![(p("/x.jpg"), p("/z.jpg")), (p("/y.jpg"), p("/x.jpg"))],
            false,
        );
        assert_eq!((m.get(&p("/x.jpg")).1, m.get(&p("/z.jpg")).1), (1, 2));
    }

    /// Marks files broken in thousands of ways: read without a panic.
    #[test]
    fn survives_broken_files() {
        let mut m = super::Marks::default();
        let a = std::path::PathBuf::from("/a.jpg");
        m.toggle(std::slice::from_ref(&a), 2);
        m.rate(&[a], 3);
        for text in crate::broken(&super::to_text(&m.map)) {
            super::parse(&text);
        }
    }
}
