//! Optional marks and ratings (`cargo build --features marks`): Geeqie's marks 1–6 and 0–5
//! stars, kept by full path in `$XDG_STATE_HOME/glimt/marks`, one file as "bits\tstars\tpath"
//! lines. They follow files that Glimt copies, moves and renames, and go with those it deletes;
//! files renamed or moved outside Glimt lose them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Each marks file as this window's marks hold it: as read, or as its own write left it when
/// no other window had written since. A refresh that finds it so reads nothing.
static KNOWN_STAMPS: std::sync::Mutex<std::collections::BTreeMap<PathBuf, Option<FileStamp>>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// Changes sent to the writer and not yet in the file, each counted by a `Working` held until
/// it is written, also when the write panics.
static WRITING: std::sync::LazyLock<std::sync::Arc<std::sync::atomic::AtomicUsize>> =
    std::sync::LazyLock::new(Default::default);

#[derive(Default)]
pub struct Marks {
    /// Mark bits (bit 0 = mark 1) and stars, by path; files with neither are left out.
    by_path: HashMap<PathBuf, (u8, u8)>,
}

impl Marks {
    pub fn load() -> Self {
        let file = marks_file();
        if let Some(f) = &file {
            known_stamps().insert(f.clone(), file_stamp(f));
        }
        let text = file
            .and_then(|f| crate::images::decode::read_limited(f).ok())
            .unwrap_or_default();
        Marks {
            by_path: parse(&text),
        }
    }

    /// Read the file again unless it is as this window's marks hold it: what another Glimt
    /// window marked meanwhile, so a mark toggled here goes from what is there, and after a write
    /// that failed, what the file kept. Not while this window's own changes are still on their
    /// way to it: those would be read back as they were before.
    pub fn refresh(&mut self) {
        if WRITING.load(std::sync::atomic::Ordering::Relaxed) > 0 {
            return;
        }
        let Some(file) = marks_file() else { return };
        // As read, or changed only by this window's own writes: not read again on every mark key
        // in a file of 500,000 marked.
        if known_stamps().get(&file) != Some(&file_stamp(&file)) {
            *self = Self::load();
        }
    }

    /// Every file with a mark or stars, and them.
    pub fn marked(&self) -> impl Iterator<Item = (&Path, (u8, u8))> {
        self.by_path.iter().map(|(p, &v)| (p.as_path(), v))
    }

    /// (mark bits, stars) of `path`.
    pub fn get(&self, path: &Path) -> (u8, u8) {
        self.by_path.get(path).copied().unwrap_or_default()
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

    /// Takes away the marks of the files deleted, and of every file in the folders deleted, so a
    /// new file of the same name does not get them. Returns the marks this window knew of them,
    /// for Undo to `restore`.
    pub fn forget(&mut self, paths: &[PathBuf]) -> Vec<(PathBuf, (u8, u8))> {
        // Most messages of a file operation have nothing gone: no look at every mark.
        if paths.is_empty() {
            return Vec::new();
        }
        // From the file as it is: stars set in another window since this one last read it
        // would else stay on a name that is gone, for the next file of that name.
        self.refresh();
        let gone: std::collections::HashSet<&Path> = paths.iter().map(PathBuf::as_path).collect();
        // Each marked file and the folders above it, not every path deleted: a few thousand
        // marked against 100 files at a time to the trash.
        let marked: Vec<PathBuf> = (self.by_path.keys())
            .filter(|k| k.ancestors().any(|a| gone.contains(a)))
            .cloned()
            .collect();
        let was: Vec<(PathBuf, (u8, u8))> =
            marked.iter().map(|p| (p.clone(), self.get(p))).collect();
        // The file forgets every mark under the names, not only those this window knows. A
        // refresh is skipped while this window's own writes are queued, and the marks another
        // window set meanwhile would else stay on a name that is gone.
        self.edit(Edit::Forget(paths.to_vec()));
        was
    }

    /// Marks and stars put back as `forget` returned them: one change for each value.
    pub fn restore(&mut self, marks: Vec<(PathBuf, (u8, u8))>) {
        let mut by_value: HashMap<(u8, u8), Vec<PathBuf>> = HashMap::new();
        for (path, value) in marks {
            by_value.entry(value).or_default().push(path);
        }
        for ((bits, stars), paths) in by_value {
            self.change(&paths, Change::Set(bits, stars));
        }
    }

    /// Files copied (`is_copy`) or moved and renamed `(from, to)`: the new ones get the marks. A
    /// folder renamed goes through `follow_folder`, so a report of files never stats them on the UI
    /// thread to find a folder among them.
    pub fn follow_files(&mut self, pairs: Vec<(PathBuf, PathBuf)>, is_copy: bool) {
        // From the file as it is: stars set in another window since this one last read it were
        // left behind on the old names, as this window did not know them.
        self.refresh();
        // Every pair goes to the file, also one whose marks this window does not know, for
        // the reason `forget` gives.
        if !pairs.is_empty() {
            self.edit(Edit::Follow(pairs, is_copy));
        }
    }

    /// The folder `from` renamed to `to` (Rename Folder): the files in it keep their marks under
    /// the new name. Every mark is looked at, so this is only for a folder, not for each rename
    /// of a file with 500,000 marked.
    pub fn follow_folder(&mut self, from: PathBuf, to: PathBuf) {
        self.refresh();
        self.edit(Edit::FollowFolder(from, to));
    }

    fn change(&mut self, paths: &[PathBuf], change: Change) {
        self.edit(Edit::Change(paths.to_vec(), change));
    }

    /// Makes `edit` to this window's marks and sends it to the file, where it is made again to
    /// the marks as they are there, so the two agree on what it did.
    fn edit(&mut self, edit: Edit) {
        apply_edit(&mut self.by_path, &edit);
        save(edit);
    }
}

/// A marks file told apart from another: every write makes a new file (a new inode), and two
/// writes in the same tick of the clock have the same time.
type FileStamp = (u64, std::time::SystemTime);

// The marks file of one test, on its thread: the tests run side by side, and one that writes
// the shared file would change what the others read back.
#[cfg(test)]
thread_local! {
    static TEST_FILE: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
    static SENT: std::cell::RefCell<Vec<Edit>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// What a window did, applied to the file as it is then.
enum Edit {
    Change(Vec<PathBuf>, Change),
    /// Files moved or copied (the flag) from each first path to the second.
    Follow(Vec<(PathBuf, PathBuf)>, bool),
    /// A folder renamed from the first path to the second, with every file in it.
    FollowFolder(PathBuf, PathBuf),
    /// Files or folders deleted, with every file in the folders.
    Forget(Vec<PathBuf>),
}

/// A change, rather than the new value, so that applied to the file it keeps what another
/// Glimt window set on the same file meanwhile.
#[derive(Clone, Copy)]
enum Change {
    /// A mark bit set or cleared.
    Mark(u8, bool),
    Stars(u8),
    /// These marks and stars, as they were.
    Set(u8, u8),
}

/// Returns the inode and time of `file`, which show that the file at the path changed since,
/// or `None` when it cannot be looked up.
#[allow(clippy::disallowed_methods)] // never tells two files apart, which is save::file_id's
fn file_stamp(file: &Path) -> Option<FileStamp> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(file).ok()?;
    Some((m.ino(), m.modified().ok()?))
}

fn known_stamps()
-> std::sync::MutexGuard<'static, std::collections::BTreeMap<PathBuf, Option<FileStamp>>> {
    KNOWN_STAMPS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub(crate) fn marks_file() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(file) = TEST_FILE.with(|f| f.borrow().clone()) {
        return Some(file);
    }
    crate::files::config::xdg_dir("XDG_STATE_HOME", ".local/state", "glimt")
        .map(|d| d.join("marks"))
}

/// Apply `change` to each path's marks and stars; a path with neither is removed.
fn apply(map: &mut HashMap<PathBuf, (u8, u8)>, paths: &[PathBuf], change: Change) {
    for path in paths {
        let (bits, stars) = map.get(path).copied().unwrap_or_default();
        let new = match change {
            Change::Mark(bit, true) => (bits | bit, stars),
            Change::Mark(bit, false) => (bits & !bit, stars),
            Change::Stars(stars) => (bits, stars),
            Change::Set(bits, stars) => (bits, stars),
        };
        match new {
            (0, 0) => drop(map.remove(path)),
            v => drop(map.insert(path.clone(), v)),
        }
    }
}

/// Gives the marks of each `from` to its `to`, and keeps them on `from` too for a copy
/// (`is_copy`). All are taken before any is given, because in a Rename Many that swaps or shifts
/// names a `to` is another pair's `from`.
fn follow_pairs(map: &mut HashMap<PathBuf, (u8, u8)>, pairs: &[(PathBuf, PathBuf)], is_copy: bool) {
    let taken: Vec<Option<(u8, u8)>> = (pairs.iter())
        .map(|(from, _)| match is_copy {
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
        Edit::Follow(pairs, is_copy) => follow_pairs(map, pairs, *is_copy),
        Edit::FollowFolder(from, to) => {
            let pairs = [(from.clone(), to.clone())];
            let moved = crate::files::file_ops::moves_by_source(&pairs);
            let inside: Vec<(PathBuf, PathBuf)> = (map.keys())
                .filter_map(|k| {
                    let dir = crate::files::file_ops::new_place(k.parent()?, &moved)?;
                    Some((k.clone(), dir.join(k.file_name()?)))
                })
                .collect();
            follow_pairs(map, &inside, false);
        }
        Edit::Forget(paths) => {
            let gone: std::collections::HashSet<&Path> =
                paths.iter().map(PathBuf::as_path).collect();
            // Each marked file and the folders above it are looked up, not every path deleted,
            // because a few thousand are marked against 100 files at a time to the trash.
            map.retain(|k, _| !k.ancestors().any(|a| gone.contains(a)));
        }
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
    // Through the thread that writes Glimt's own files, which Glimt waits for before it ends:
    // a mark set, or moved with its file, just before closing is not lost.
    let writing = crate::support::threads::Working::new(&WRITING);
    crate::files::file_io::write_later(move || {
        let _writing = writing;
        // Only a change can add marks, so the other edits make no marks file where none is.
        let adds = matches!(edit, Edit::Change(..));
        if let Some(file) = marks_file().filter(|f| adds || f.exists()) {
            write(&file, &[edit]);
        }
    });
}

/// `changes` applied to the marks file as it is now.
fn write(file: &Path, changes: &[Edit]) {
    // These changes on the file as another window may have left it; one that cannot be read
    // is not written over with these changes alone, as every other mark would be lost.
    let edit = |bytes: &[u8]| {
        // Under the lock: no other window wrote since this one read or wrote it.
        let ours = known_stamps().get(file) == Some(&file_stamp(file));
        let mut map = parse(bytes);
        for change in changes {
            apply_edit(&mut map, change);
        }
        let mut text = to_text(&map);
        text.extend(unread_lines(bytes));
        (text, ours)
    };
    let result = crate::files::file_io::edit_shared(file, edit);
    // Ours alone, the window's marks are the file now; else it is read again.
    // shortcut: the file is looked at just after the lock is let go; a window that wrote in that
    // moment goes unseen until the file changes again.
    let now = matches!(result, Ok(true))
        .then(|| file_stamp(file))
        .flatten();
    match now {
        Some(now) => drop(known_stamps().insert(file.to_path_buf(), Some(now))),
        None => drop(known_stamps().remove(file)),
    }
    if let Err(e) = result {
        crate::files::file_io::save_failed(crate::tf!(
            "Could not save {name}: {e}",
            name = file.display(),
            e
        ));
    }
}

/// In a test, the edits are kept for the test to write, so the tests side by side do not write
/// one shared file.
#[cfg(test)]
fn save(edit: Edit) {
    SENT.with(|sent| sent.borrow_mut().push(edit));
}

/// The marks file: a line for each file.
fn parse(text: &[u8]) -> HashMap<PathBuf, (u8, u8)> {
    text.split(|&b| b == b'\n').filter_map(parse_line).collect()
}

/// Reads one line of the marks file. The mark bits are kept as they are, also those of marks a
/// newer Glimt may add, so that this one writes them back; only marks 1 to 6 are shown and
/// changed. Returns `None` for a line that is not of this form.
fn parse_line(line: &[u8]) -> Option<(PathBuf, (u8, u8))> {
    let mut parts = line.splitn(3, |&b| b == b'\t');
    let number = |b: &[u8]| std::str::from_utf8(b).ok()?.parse::<u8>().ok();
    let (bits, stars) = (number(parts.next()?)?, number(parts.next()?)?);
    // Any bytes are a name on Unix: those `save` wrote, or the user's own.
    let path = <std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(parts.next()?);
    Some((PathBuf::from(path), (bits, stars.min(5))))
}

/// Returns the lines of the marks file `text` that `parse_line` cannot read but that have the
/// form of a marks line, each with its line break. A write keeps them as they were, so a line
/// of a newer Glimt is not lost when this one changes a mark. Other lines are damage or
/// garbage, and they go at the next write, as they did before 0.76.0.
fn unread_lines(text: &[u8]) -> Vec<u8> {
    (text.split(|&b| b == b'\n'))
        .filter(|line| parse_line(line).is_none() && has_form_of_marks_line(line))
        .flat_map(|line| line.iter().copied().chain(*b"\n"))
        .collect()
}

/// Whether `line` has the form of a marks line, in this Glimt's form or a newer one's. Such a
/// line has two fields and then a full path, each after a tab, as `bits\tstars\t/path`. A
/// newer Glimt may write other words in the two fields, but the path stays last, because a
/// path may hold a tab.
fn has_form_of_marks_line(line: &[u8]) -> bool {
    let mut parts = line.splitn(3, |&b| b == b'\t');
    let fields = parts.next().is_some_and(|f| !f.is_empty()) && parts.next().is_some();
    fields && parts.next().is_some_and(|path| path.starts_with(b"/"))
}

/// Write `stars` as xmp:Rating into the sidecar `<file>.xmp` that darktable and digiKam read.
/// An existing sidecar keeps everything else: only the rating changes, or is added to its first
/// rdf:Description. Without a sidecar, 0 stars writes none.
pub fn write_xmp(path: &Path, stars: u8) -> Result<(), String> {
    crate::metadata::xmp::update_sidecar(path, stars == 0, |x| set_rating(x, stars))
}

/// `xmp` with its rating set to `stars`, as an attribute of its first rdf:Description. Returns
/// `None` if it has no rdf:Description to put it in.
fn set_rating(xmp: &str, stars: u8) -> Option<String> {
    use crate::metadata::xmp::{first_description, without_property};
    const XMP: &str = "http://ns.adobe.com/xap/1.0/";
    // The rating is written with the prefix that the sidecar binds to XMP's namespace, such as
    // an old packet's xap, so it is the same property as the rating there.
    let (_, declared) = first_description(xmp)?;
    let prefix = (declared.iter().find(|(_, uri)| *uri == XMP)).map_or("xmp", |(p, _)| *p);
    let rating = format!("{prefix}:Rating");
    // The rating it has goes first, in whatever form and quotes it was written, so the sidecar
    // never holds two, which darktable and digiKam refuse as broken XML.
    let xmp = &without_property(&without_property(xmp, "xmp:Rating"), &rating);
    let (_, declared) = first_description(xmp)?;
    let ns = match declared.iter().any(|(p, _)| *p == prefix) {
        true => String::new(),
        false => format!(" xmlns:xmp=\"{XMP}\""),
    };
    let at = xmp.find("<rdf:Description")? + "<rdf:Description".len();
    Some(format!(
        "{}{ns} {rating}=\"{stars}\"{}",
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
    /// A name in another encoding (Latin-1) keeps its bytes, read from the marks file.
    #[test]
    fn reads_a_name_that_is_not_utf8() {
        use std::os::unix::ffi::OsStrExt;
        let marks = super::parse(b"3\t2\t/p/bl\xe5b\xe6r.jpg\n");
        let (path, value) = marks.into_iter().next().unwrap();
        assert_eq!(path.as_os_str().as_bytes(), b"/p/bl\xe5b\xe6r.jpg");
        assert_eq!(value, (3, 2));
    }

    /// Stars set in another window follow a move and go with a delete made in this one, which
    /// had not read them yet: they stayed on names that were gone.
    #[test]
    fn stars_from_another_window_follow_a_move() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-marks-other-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A marks file of its own: the other tests, side by side, read the shared one.
        let file = dir.join("marks");
        super::TEST_FILE.with(|f| *f.borrow_mut() = Some(file.clone()));
        let (a, b, c) = (dir.join("a.png"), dir.join("b.png"), dir.join("c.png"));
        // This window read the marks before the other one wrote these.
        let mut marks = super::Marks::default();
        let text = format!("0\t3\t{}\n0\t2\t{}\n", a.display(), b.display());
        std::fs::write(&file, text).unwrap();
        marks.follow_files(vec![(a.clone(), c.clone())], false);
        assert_eq!((marks.get(&c), marks.get(&a)), ((0, 3), (0, 0)));
        let was = marks.forget(std::slice::from_ref(&b));
        assert_eq!((marks.get(&b), was), ((0, 0), vec![(b.clone(), (0, 2))]));
        super::TEST_FILE.with(|f| *f.borrow_mut() = None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Stars that another window set while this one did not read the file (its own writes were
    /// queued, so the refresh was skipped) follow a move and go with a delete in the file. The
    /// window sent nothing for marks it did not know, so they stayed on names that were gone.
    #[test]
    fn marks_not_read_yet_follow_in_the_file() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-marks-unread-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("marks");
        super::TEST_FILE.with(|f| *f.borrow_mut() = Some(file.clone()));
        let (a, b, c) = (dir.join("a.png"), dir.join("b.png"), dir.join("c.png"));
        let text = format!("0\t3\t{}\n0\t2\t{}\n", a.display(), b.display());
        std::fs::write(&file, text).unwrap();
        // The window takes the file as known, as when its refresh is skipped.
        super::known_stamps().insert(file.clone(), super::file_stamp(&file));
        let mut marks = super::Marks::default();
        super::SENT.with(|sent| sent.borrow_mut().clear());
        marks.follow_files(vec![(a.clone(), c.clone())], false);
        marks.forget(std::slice::from_ref(&b));
        let sent: Vec<super::Edit> = super::SENT.with(|sent| sent.take());
        super::write(&file, &sent);
        let now = super::parse(&std::fs::read(&file).unwrap());
        assert_eq!(now, std::collections::HashMap::from([(c.clone(), (0, 3))]));
        super::TEST_FILE.with(|f| *f.borrow_mut() = None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// After this window's own write, a refresh takes what it wrote instead of the whole file.
    #[test]
    fn own_writes_are_not_read_again() {
        let dir = std::env::temp_dir().join(format!("glimt-test-marks-own-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("marks");
        super::TEST_FILE.with(|f| *f.borrow_mut() = Some(file.clone()));
        let (a, b) = (dir.join("a.png"), dir.join("b.png"));
        let mut marks = super::Marks::load();
        let stars =
            |p: &std::path::Path, n| super::Edit::Change(vec![p.into()], super::Change::Stars(n));
        marks.rate(std::slice::from_ref(&a), 4);
        super::write(&file, &[stars(&a, 4)]);
        // The file's bytes gone but the file and its time kept: read again, it would have no
        // stars.
        let t = std::fs::metadata(&file).unwrap().modified().unwrap();
        let f = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&file)
            .unwrap();
        f.set_modified(t).unwrap();
        marks.refresh();
        assert_eq!(marks.get(&a), (0, 4));
        // Another window's write just after, maybe in the same tick of the clock: read.
        super::write(&file, &[stars(&a, 4)]);
        crate::files::file_io::edit_shared(&file, |old| {
            let mut text = old.to_vec();
            text.extend(format!("0\t2\t{}\n", b.display()).as_bytes());
            (text, ())
        })
        .unwrap();
        marks.refresh();
        assert_eq!((marks.get(&a), marks.get(&b)), ((0, 4), (0, 2)));
        super::TEST_FILE.with(|f| *f.borrow_mut() = None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A mark whose write failed is not kept in the window as if it were in the file.
    #[test]
    fn a_mark_not_written_is_not_kept() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-marks-failed-{}", std::process::id()));
        // A folder where the marks file would be: it cannot be read or written over.
        let file = dir.join("marks");
        std::fs::create_dir_all(&file).unwrap();
        super::TEST_FILE.with(|f| *f.borrow_mut() = Some(file.clone()));
        let a = dir.join("a.png");
        let mut marks = super::Marks::load();
        marks.rate(std::slice::from_ref(&a), 3);
        let stars = super::Edit::Change(vec![a.clone()], super::Change::Stars(3));
        super::write(&file, &[stars]);
        let _ = crate::files::file_io::take_save_error();
        marks.refresh();
        assert_eq!(marks.get(&a), (0, 0));
        super::TEST_FILE.with(|f| *f.borrow_mut() = None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The marks file keeps what this Glimt does not understand: mark bits above mark 6 stay on
    /// their file, also when mark 1 is set beside them, and a line it cannot read is written
    /// back as it was. Before, the bits were cut to marks 1 to 6 and such a line was dropped, so
    /// an older Glimt that changed one mark deleted what a newer one had added. Only a line in
    /// the form of a marks line is kept, and garbage still goes, so it does not pile up.
    #[test]
    fn keeps_what_it_does_not_understand() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-marks-unknown-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("marks");
        super::TEST_FILE.with(|f| *f.borrow_mut() = Some(file.clone()));
        let newer = "192\t3\t/a.jpg\nrejected\t1\t/b.jpg\ngarbage\nx\ty\tz\n\t\t/d.jpg\n";
        std::fs::write(&file, newer).unwrap();
        let a = std::path::PathBuf::from("/a.jpg");
        let mut marks = super::Marks::load();
        assert_eq!(marks.get(&a), (192, 3));
        marks.toggle(std::slice::from_ref(&a), 1);
        assert_eq!(marks.get(&a), (193, 3));
        let mark = super::Edit::Change(vec![a.clone()], super::Change::Mark(1, true));
        let stars = super::Edit::Change(vec!["/c.jpg".into()], super::Change::Stars(2));
        super::write(&file, &[mark, stars]);
        let text = std::fs::read_to_string(&file).unwrap();
        for line in [
            "193\t3\t/a.jpg\n",
            "rejected\t1\t/b.jpg\n",
            "0\t2\t/c.jpg\n",
        ] {
            assert!(text.contains(line), "{line:?} not in {text:?}");
        }
        // Lines without the form of a marks line are damage, and go.
        for line in ["garbage", "x\ty\tz", "\t\t/d.jpg"] {
            assert!(!text.contains(line), "{line:?} in {text:?}");
        }
        super::TEST_FILE.with(|f| *f.borrow_mut() = None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A damaged marks file is read as far as it goes, never a panic.
    #[test]
    fn survives_broken_marks() {
        let seed = b"3\t4\t/a/b c.jpg\n0\t0\t/d.png\n255\t9\tbroken\n";
        for bytes in crate::support::test_support::broken(seed) {
            let _ = super::parse(&bytes);
        }
        // The rating set in a sidecar broken in thousands of ways, as an attribute and an element.
        let xmp = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF><rdf:Description rdf:about=\"\" \
                   xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\" xmp:Rating=\"3\"><xmp:Rating>-1</xmp:Rating>\
                   </rdf:Description></rdf:RDF></x:xmpmeta>";
        for bytes in crate::support::test_support::broken(xmp.as_bytes()) {
            let _ = super::set_rating(&String::from_utf8_lossy(&bytes), 4);
        }
    }

    #[test]
    fn toggles_rates_and_reads_lines() {
        use std::path::PathBuf;
        // A marks file of its own, which is not there: not the one of whoever runs the tests.
        let none =
            std::env::temp_dir().join(format!("glimt-test-marks-toggle-{}", std::process::id()));
        super::TEST_FILE.with(|f| *f.borrow_mut() = Some(none.join("marks")));
        let mut m = super::Marks::default();
        let (a, b) = (PathBuf::from("/a.jpg"), PathBuf::from("/b.jpg"));
        let both = [a.clone(), b.clone()];
        m.toggle(std::slice::from_ref(&a), 2);
        // Mark 2 on only one of them: the toggle sets it on both, the next clears both.
        m.toggle(&both, 2);
        assert_eq!((m.get(&a), m.get(&b)), ((0b10, 0), (0b10, 0)));
        m.rate(std::slice::from_ref(&b), 3);
        assert_eq!(super::text(m.get(&b)), "★★★  2");
        m.toggle(&both, 2);
        assert_eq!(m.get(&a), (0, 0));
        assert!(!m.by_path.contains_key(&a));
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
        super::apply(
            &mut map,
            std::slice::from_ref(&a),
            super::Change::Mark(4, true),
        );
        super::apply(&mut map, std::slice::from_ref(&b), super::Change::Stars(4));
        assert_eq!((map[&a], map[&b]), ((5, 0), (0, 4)));
        // A darktable sidecar keeps its fields; the rating is changed or added.
        let dt = r#"<rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="1" darktable:x="y">"#;
        assert_eq!(
            super::set_rating(dt, 4).unwrap(),
            r#"<rdf:Description xmp:Rating="4" rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" darktable:x="y">"#
        );
        // A rating in single quotes, as exiftool may write it, is replaced, not written twice,
        // which would make the XML broken.
        let quoted = r#"<rdf:Description xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating='3'>"#;
        assert_eq!(
            super::set_rating(quoted, 4).unwrap(),
            r#"<rdf:Description xmp:Rating="4" xmlns:xmp="http://ns.adobe.com/xap/1.0/">"#
        );
        let none = r#"<rdf:Description rdf:about="">"#;
        assert_eq!(
            super::set_rating(none, 2).unwrap(),
            r#"<rdf:Description xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="2" rdf:about="">"#
        );
        assert_eq!(
            super::set_rating("<xmp:Rating>5</xmp:Rating><rdf:Description>", 0).unwrap(),
            r#"<rdf:Description xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="0">"#
        );
        // An old packet's xap prefix and a declaration with spaces around its equals sign are
        // read as the reader reads them, so the rating is neither written twice nor declared
        // twice.
        let xap = r#"<rdf:Description xmlns:xap = "http://ns.adobe.com/xap/1.0/" xap:Rating="3">"#;
        assert_eq!(
            super::set_rating(xap, 4).unwrap(),
            r#"<rdf:Description xap:Rating="4" xmlns:xap = "http://ns.adobe.com/xap/1.0/">"#
        );
        let spaced = r#"<rdf:Description xmlns:xmp = "http://ns.adobe.com/xap/1.0/">"#;
        assert_eq!(
            super::set_rating(spaced, 1)
                .unwrap()
                .matches("xmlns:")
                .count(),
            1
        );
        assert_eq!(super::set_rating("nothing", 1), None);
        super::TEST_FILE.with(|f| *f.borrow_mut() = None);
    }

    /// Marks follow a move and a rename, are kept by a copy, go with a delete, and a new file
    /// where a deleted one was does not get them.
    #[test]
    fn follow_their_files() {
        use std::path::PathBuf;
        let p = PathBuf::from;
        // A marks file of its own, which is not there: the shared one may hold other tests'.
        let none =
            std::env::temp_dir().join(format!("glimt-test-marks-none-{}", std::process::id()));
        super::TEST_FILE.with(|f| *f.borrow_mut() = Some(none.join("marks")));
        let mut m = super::Marks::load();
        m.rate(&[p("/a.jpg"), p("/b.jpg")], 4);
        m.toggle(&[p("/a.jpg")], 1);
        m.follow_files(vec![(p("/a.jpg"), p("/to/a.jpg"))], false);
        assert_eq!(
            (m.get(&p("/a.jpg")), m.get(&p("/to/a.jpg"))),
            ((0, 0), (1, 4))
        );
        m.follow_files(vec![(p("/b.jpg"), p("/b copy.jpg"))], true);
        assert_eq!(
            (m.get(&p("/b.jpg")), m.get(&p("/b copy.jpg"))),
            ((0, 4), (0, 4))
        );
        m.forget(&[p("/b.jpg")]);
        assert_eq!(m.get(&p("/b.jpg")), (0, 0));
        // A folder renamed: its files' marks go along; trashed: they go. The folders are only
        // names, as nothing on the disk is looked at to tell a folder.
        m.rate(&[p("/trip/old/a.jpg")], 5);
        m.follow_folder(p("/trip/old"), p("/trip/2024"));
        assert_eq!(m.get(&p("/trip/2024/a.jpg")), (0, 5));
        m.forget(&[p("/trip/2024")]);
        assert_eq!(m.get(&p("/trip/2024/a.jpg")), (0, 0));
        // An old entry where an unmarked file lands is cleared.
        m.follow_files(vec![(p("/c.jpg"), p("/b copy.jpg"))], false);
        assert_eq!(m.get(&p("/b copy.jpg")), (0, 0));
        // Rename Many swapping names, and shifting them: each keeps its own.
        m.rate(&[p("/x.jpg")], 1);
        m.rate(&[p("/y.jpg")], 2);
        m.follow_files(
            vec![(p("/x.jpg"), p("/y.jpg")), (p("/y.jpg"), p("/x.jpg"))],
            false,
        );
        assert_eq!((m.get(&p("/x.jpg")).1, m.get(&p("/y.jpg")).1), (2, 1));
        m.follow_files(
            vec![(p("/x.jpg"), p("/z.jpg")), (p("/y.jpg"), p("/x.jpg"))],
            false,
        );
        assert_eq!((m.get(&p("/x.jpg")).1, m.get(&p("/z.jpg")).1), (1, 2));
        super::TEST_FILE.with(|f| *f.borrow_mut() = None);
    }

    /// Marks files broken in thousands of ways: read without a panic.
    #[test]
    fn survives_broken_files() {
        let none =
            std::env::temp_dir().join(format!("glimt-test-marks-broken-{}", std::process::id()));
        super::TEST_FILE.with(|f| *f.borrow_mut() = Some(none.join("marks")));
        let mut m = super::Marks::default();
        let a = std::path::PathBuf::from("/a.jpg");
        m.toggle(std::slice::from_ref(&a), 2);
        m.rate(&[a], 3);
        for text in crate::support::test_support::broken(&super::to_text(&m.by_path)) {
            super::parse(&text);
        }
        super::TEST_FILE.with(|f| *f.borrow_mut() = None);
    }
}
