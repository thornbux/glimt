//! Optional marks and ratings (`cargo build --features marks`): Geeqie's marks 1–6 and 0–5
//! stars, kept by full path in `$XDG_STATE_HOME/glimt/marks`, one file as "bits\tstars\tpath"
//! lines. They follow files that Glimt copies, moves and renames, and go with those it deletes;
//! files renamed or moved outside Glimt lose them.

use crate::files::config::fits_on_a_line;
use crate::files::file_io::ListedFile;
use crate::files::trash::ForgottenMarks;
use crate::support::threads::lock_even_if_poisoned;
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

/// The edits sent to the writer thread and not yet taken by it. The writer takes all of them
/// for one write, so the mark keys pressed while a write waits or runs cost one more write of
/// the whole file, not one each.
static QUEUED_EDITS: std::sync::Mutex<Vec<Edit>> = std::sync::Mutex::new(Vec::new());

/// The edits of each marks file that follow what happened on disk (files moved, renamed or
/// deleted) and could not be written. They are made again before the next edits, and over the
/// file each time it is read, so the marks stay on the files whose names changed. A failed
/// change of a mark or of stars is told instead and not kept, as the user can make it again.
static UNWRITTEN_EDITS: std::sync::Mutex<std::collections::BTreeMap<PathBuf, Vec<Edit>>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// Mark bits (bit 0 = mark 1) and stars, by path.
type MarksByPath = HashMap<PathBuf, (u8, u8)>;

#[derive(Default)]
pub struct Marks {
    /// Mark bits (bit 0 = mark 1) and stars, by path; files with neither are left out.
    by_path: HashMap<PathBuf, (u8, u8)>,
    /// How many edits this window has made, so that a read of the file asked for before one of
    /// them is not taken over it.
    edits_made: u64,
    /// The marks file read again in the writer's thread (`refresh`), with `edits_made` when it
    /// was asked for. The answer is `None` when the file is as this window holds it.
    read_again: crate::support::threads::Receiving<(u64, Option<MarksByPath>)>,
    /// The window's context, asked for a frame when the file has been read again, so that
    /// marks another window set show at once, not at the next input.
    repaint: eframe::egui::Context,
}

impl Marks {
    /// Returns these marks, which ask `ctx` for a frame when the file has been read again.
    pub fn repainting(self, ctx: &eframe::egui::Context) -> Self {
        Marks {
            repaint: ctx.clone(),
            ..self
        }
    }

    /// Reads the file again in the writer's thread, unless it is as this window's marks hold
    /// it: what another Glimt window marked meanwhile, and after a write that failed, what the
    /// file kept. Not while this window's own changes are still on their way to it: those would
    /// be read back as they were before. The marks read are taken by `receive`, at the next
    /// frame, as the file may be on a share that stopped answering.
    pub fn refresh(&mut self) {
        if !own_writes_queued() {
            self.read_file_again(false);
        }
    }

    /// Reads the marks file again in the writer's thread, as `refresh` says, unless a read
    /// runs already. The read comes after this window's writes queued before it, as they go
    /// through the same thread. A `forced` read reads the file also when it is as held.
    fn read_file_again(&mut self, forced: bool) {
        if self.read_again.is_some() {
            return;
        }
        // A test without a marks file of its own writes its edits to a file in memory (`save`),
        // so the file on disk, which the tests side by side share, holds none of them.
        #[cfg(test)]
        if TEST_FILE.with(|f| f.borrow().is_none()) {
            return;
        }
        let Some(file) = marks_file() else { return };
        let (tx, rx) = std::sync::mpsc::channel();
        let asked_at = self.edits_made;
        let repaint = self.repaint.clone();
        crate::files::file_io::write_later(move || {
            // As read, or changed only by this window's own writes: not read again on every
            // mark key in a file of 500,000 marked. The file is looked at before the lock is
            // taken, so the lock is never held while the disk is waited for.
            let stamp = file_stamp(&file);
            let is_as_held = !forced && known_stamps().get(&file) == Some(&stamp);
            let read = (!is_as_held).then(|| read_marks_file(&file));
            let _ = tx.send((asked_at, read));
            repaint.request_repaint();
        });
        self.read_again = crate::support::threads::Receiving::of(rx);
    }

    /// Takes the marks file as `refresh` read it again. When this window made an edit after it
    /// was asked for, what was read lacks that edit and is dropped, so a mark set meanwhile is
    /// not lost. The file is then read again after that edit is written, also when that write
    /// leaves it as this window holds it, so what another window wrote shows all the same.
    pub fn receive(&mut self) {
        let crate::support::threads::Took::Answer((asked_at, Some(by_path))) =
            self.read_again.take()
        else {
            return;
        };
        if asked_at == self.edits_made {
            self.by_path = by_path;
            return;
        }
        self.read_file_again(true);
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
    /// Whether all have it is decided on the marks file as it is when the edit is written, not
    /// on this window's copy, which may lack a mark that another window set meanwhile. So the
    /// file is read back after the write, and this window then shows what the file decided.
    /// The read back reads the file only when another window wrote it, because the write keeps
    /// the file's stamp as known only when no other window wrote in between. So a key pressed
    /// with 500,000 files marked does not cost a read of the whole file.
    pub fn toggle(&mut self, files: &[ListedFile], n: u8) {
        self.change(files, Change::Toggle(1 << (n - 1)));
        self.read_file_again(false);
    }

    /// Gives `files` the stars `stars`.
    pub fn rate(&mut self, files: &[ListedFile], stars: u8) {
        self.change(files, Change::Stars(stars));
    }

    /// Reads the marks file again after this window's writes queued before, also when it is as
    /// held: a write found a name that another file had taken, and left its mark out.
    pub fn read_again(&mut self) {
        self.read_file_again(true);
    }

    /// Takes away the marks of the files deleted, and of every file in the folders deleted, so a
    /// new file of the same name does not get them. The file forgets every mark under the names,
    /// also those that another window set and this window has not read. When `for_undo` is
    /// given, the writer thread adds to it the marks that the file held under the names. It
    /// does so after this window's queued edits, so a mark cleared here is not brought back by
    /// Undo, and the file is read there rather than on the UI thread.
    pub fn forget(&mut self, paths: &[PathBuf], for_undo: Option<ForgottenMarks>) {
        // Most messages of a file operation have nothing gone: no look at every mark.
        if paths.is_empty() {
            return;
        }
        self.edit(Edit::Forget(paths.to_vec(), for_undo));
    }

    /// Marks and stars put back as `forget` gave them for Undo: one change for each value.
    pub fn restore(&mut self, marks: Vec<(PathBuf, (u8, u8))>) {
        let mut by_value: HashMap<(u8, u8), Vec<PathBuf>> = HashMap::new();
        for (path, value) in marks {
            by_value.entry(value).or_default().push(path);
        }
        for ((bits, stars), paths) in by_value {
            let files: Vec<ListedFile> = (paths.into_iter())
                .map(ListedFile::not_from_the_list)
                .collect();
            self.change(&files, Change::Set(bits, stars));
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

    fn change(&mut self, files: &[ListedFile], change: Change) {
        self.edit(Edit::Change(files.to_vec(), change));
    }

    /// Makes `edit` to this window's marks and sends it to the file, where it is made again to
    /// the marks as they are there, so the two agree on what it did.
    fn edit(&mut self, edit: Edit) {
        self.edits_made += 1;
        for edit in refuse_names_with_line_breaks(edit, &self.by_path) {
            apply_edit(&mut self.by_path, &edit, &[]);
            save(edit);
        }
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
    static TEST_FILE_IN_MEMORY: std::cell::RefCell<HashMap<PathBuf, (u8, u8)>> =
        std::cell::RefCell::new(HashMap::new());
    /// Whether a test takes this window's own writes as queued. `WRITING` is shared by the
    /// tests that run side by side, so a test that raised it would skip their refreshes too.
    static TEST_WRITES_QUEUED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// What a window did, applied to the file as it is then.
enum Edit {
    /// A change of the files picked, each checked to be the one listed before it is made.
    Change(Vec<ListedFile>, Change),
    /// Files moved or copied (the flag) from each first path to the second.
    Follow(Vec<(PathBuf, PathBuf)>, bool),
    /// A folder renamed from the first path to the second, with every file in it.
    FollowFolder(PathBuf, PathBuf),
    /// Files or folders deleted, with every file in the folders. The marks that the file held
    /// under them are added to the second field, for Undo.
    Forget(Vec<PathBuf>, Option<ForgottenMarks>),
}

/// A change, rather than the new value, so that applied to the file it keeps what another
/// Glimt window set on the same file meanwhile.
#[derive(Clone, Copy)]
enum Change {
    /// A mark bit set or cleared.
    Mark(u8, bool),
    /// A mark bit set on every path, or cleared when every path has it already.
    Toggle(u8),
    Stars(u8),
    /// These marks and stars, as they were.
    Set(u8, u8),
}

/// Returns this window's marks, read from the marks file. It waits for the disk, so it is called
/// before the window opens (`main`), or in a test.
pub fn read_marks() -> Marks {
    let by_path = marks_file()
        .map(|f| read_marks_file(&f))
        .unwrap_or_default();
    Marks {
        by_path,
        ..Default::default()
    }
}

/// Returns the marks in the marks file `file`, none when it cannot be read, and notes the file
/// as read (`known_stamps`).
fn read_marks_file(file: &Path) -> HashMap<PathBuf, (u8, u8)> {
    // The file is looked at before the lock is taken, as in `read_file_again`.
    let stamp = file_stamp(file);
    known_stamps().insert(file.to_path_buf(), stamp);
    let text = crate::images::decode::read_limited(file).unwrap_or_default();
    let mut map = parse(&text);
    for edit in lock_even_if_poisoned(&UNWRITTEN_EDITS)
        .get(file)
        .into_iter()
        .flatten()
    {
        apply_edit(&mut map, edit, &[]);
    }
    map
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
    lock_even_if_poisoned(&KNOWN_STAMPS)
}

/// Returns whether this window's own changes are still on their way to the marks file.
fn own_writes_queued() -> bool {
    #[cfg(test)]
    if TEST_WRITES_QUEUED.with(std::cell::Cell::get) {
        return true;
    }
    WRITING.load(std::sync::atomic::Ordering::Relaxed) > 0
}

pub(crate) fn marks_file() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(file) = TEST_FILE.with(|f| f.borrow().clone()) {
        return Some(file);
    }
    crate::files::config::xdg_dir("XDG_STATE_HOME", ".local/state", "glimt")
        .map(|d| d.join("marks"))
}

/// Leaves out of `edit` the names that a line of the marks file cannot hold, and tells the user
/// how many files lost their marks and stars so. A file moved, or a folder renamed, to such a
/// name has the marks of its old name forgotten, so that a new file of that name does not get
/// them. Returns the edits to make in place of `edit`, which `map`, this window's marks, counts
/// the files of.
fn refuse_names_with_line_breaks(edit: Edit, map: &HashMap<PathBuf, (u8, u8)>) -> Vec<Edit> {
    let (edits, left_out) = match edit {
        Edit::Change(mut paths, change) => {
            let before = paths.len();
            paths.retain(|p| fits_on_a_line(p.path()));
            let left_out = before - paths.len();
            let edits = (!paths.is_empty()).then_some(Edit::Change(paths, change));
            (edits.into_iter().collect(), left_out)
        }
        Edit::Follow(pairs, is_copy) => {
            let (kept, refused): (Vec<_>, Vec<_>) =
                (pairs.into_iter()).partition(|(_, to)| fits_on_a_line(to));
            let left_out = (refused.iter())
                .filter(|(from, _)| map.contains_key(from))
                .count();
            let gone: Vec<PathBuf> = (refused.into_iter())
                .filter(|_| !is_copy)
                .map(|(from, _)| from)
                .collect();
            let edits = [
                (!kept.is_empty()).then_some(Edit::Follow(kept, is_copy)),
                (!gone.is_empty()).then_some(Edit::Forget(gone, None)),
            ];
            (edits.into_iter().flatten().collect(), left_out)
        }
        Edit::FollowFolder(from, to) if !fits_on_a_line(&to) => {
            let left_out = map.keys().filter(|p| p.starts_with(&from)).count();
            (vec![Edit::Forget(vec![from], None)], left_out)
        }
        edit => (vec![edit], 0),
    };
    if let Some(notice) = crate::files::config::left_out_for_line_breaks(left_out) {
        crate::files::file_io::save_failed(notice);
    }
    edits
}

/// Apply `change` to each path's marks and stars; a path with neither is removed.
fn apply(map: &mut HashMap<PathBuf, (u8, u8)>, paths: &[&Path], change: Change) {
    let change = match change {
        Change::Toggle(bit) => {
            let all = (paths.iter()).all(|p| map.get(*p).is_some_and(|(bits, _)| bits & bit != 0));
            Change::Mark(bit, !all)
        }
        change => change,
    };
    for &path in paths {
        let (bits, stars) = map.get(path).copied().unwrap_or_default();
        let new = match change {
            Change::Mark(bit, true) => (bits | bit, stars),
            Change::Mark(bit, false) => (bits & !bit, stars),
            Change::Stars(stars) => (bits, stars),
            Change::Set(bits, stars) => (bits, stars),
            // A toggle was made a set or a clear above.
            Change::Toggle(_) => (bits, stars),
        };
        match new {
            (0, 0) => drop(map.remove(path)),
            v => drop(map.insert(path.to_path_buf(), v)),
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

/// Makes `edit` to the marks `map`, leaving out the files in `taken`, whose names other files
/// have taken since the list showed them.
fn apply_edit(map: &mut HashMap<PathBuf, (u8, u8)>, edit: &Edit, taken: &[&ListedFile]) {
    match edit {
        Edit::Change(files, change) => {
            let paths: Vec<&Path> = (files.iter())
                .filter(|f| !taken.contains(f))
                .map(ListedFile::path)
                .collect();
            apply(map, &paths, *change)
        }
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
        Edit::Forget(paths, _) => {
            let gone: std::collections::HashSet<&Path> =
                paths.iter().map(PathBuf::as_path).collect();
            // Each marked file and the folders above it are looked up, not every path deleted,
            // because a few thousand are marked against 100 files at a time to the trash.
            map.retain(|k, _| !k.ancestors().any(|a| gone.contains(a)));
        }
    }
}

/// Makes `edit` to the marks as the file holds them, leaving out the files in `taken`. A delete
/// first adds the marks it takes away to its list for Undo, so that list holds what the file
/// held, not what the window knew.
fn apply_edit_to_file(map: &mut HashMap<PathBuf, (u8, u8)>, edit: &Edit, taken: &[&ListedFile]) {
    if let Edit::Forget(paths, Some(for_undo)) = edit {
        let gone: std::collections::HashSet<&Path> = paths.iter().map(PathBuf::as_path).collect();
        let forgotten = (map.iter())
            .filter(|(k, _)| k.ancestors().any(|a| gone.contains(a)))
            .map(|(k, &v)| (k.clone(), v));
        lock_even_if_poisoned(for_undo).extend(forgotten);
    }
    apply_edit(map, edit, taken);
}

/// Returns the files of the changes in `edits` whose names other files have taken since the
/// list showed them (`ListedFile::check`), and the error of the first, to tell. Their marks are
/// left out of the file, and the window, which set them already, reads the file again
/// (`read_again`). It waits for the disk, so it runs in the writer's thread, under the file's
/// lock.
/// shortcut: the files of a file system that keeps its inodes (NFS among them) are looked at in
/// the thread of Glimt's own files, so an NFS share that stopped answering holds the settings
/// behind a mark set there; move the marks to the thread of the user's folders (keeping
/// `read_file_again` in the same thread) if that is seen.
fn names_taken(edits: &[Edit]) -> (Vec<&ListedFile>, Option<String>) {
    let files = edits.iter().flat_map(|edit| match edit {
        Edit::Change(files, _) => files.as_slice(),
        _ => &[],
    });
    let mut first = None;
    let taken: Vec<&ListedFile> = files
        .filter(|f| {
            f.check()
                .map_err(|e| first.get_or_insert(format!("{}: {e}", f.path().display())))
                .is_err()
        })
        .collect();
    (taken, first)
}

fn to_text(map: &HashMap<PathBuf, (u8, u8)>) -> Vec<u8> {
    let mut text = Vec::new();
    for (path, (bits, stars)) in map {
        if fits_on_a_line(path) {
            text.extend(format!("{bits}\t{stars}\t").as_bytes());
            text.extend(path.as_os_str().as_encoded_bytes());
            text.push(b'\n');
        }
    }
    text
}

/// Sends `edit` to the marks file. Only the changes go to the file. The one writer thread makes
/// them in order and merges them into the file as it is now, so the marks that another Glimt
/// window wrote meanwhile are kept. The file is renamed into place, so a crash never leaves half
/// of it. The edits that wait for the writer are made in one write.
/// shortcut: each write rewrites the whole file, about 0.4 s with 500,000 marked; write only the
/// changed lines if a single change must be quick at that scale.
#[cfg(not(test))]
fn save(edit: Edit) {
    // The count is held from now until the edit is written, so a refresh does not read the
    // file back as it was before. An edit that joins a waiting write is counted by that write.
    let writing = crate::support::threads::Working::new(&WRITING);
    if !queue_for_writer(edit) {
        return;
    }
    // Through the thread that writes Glimt's own files, which Glimt waits for before it ends:
    // a mark set, or moved with its file, just before closing is not lost.
    crate::files::file_io::write_later(move || {
        let _writing = writing;
        let edits = take_queued_edits();
        // Only a change can add marks, so the other edits make no marks file where none is.
        let adds = edits.iter().any(|edit| matches!(edit, Edit::Change(..)));
        if let Some(file) =
            marks_file().filter(|f| adds || !crate::files::file_io::is_surely_gone(f))
        {
            write(&file, edits);
        }
    });
}

/// Adds `edit` to the edits that wait for the writer thread. Returns whether the writer must be
/// asked to write them, which is so only when no edit was waiting: a write asked for earlier and
/// not yet started takes this edit too.
fn queue_for_writer(edit: Edit) -> bool {
    let mut queued = lock_even_if_poisoned(&QUEUED_EDITS);
    queued.push(edit);
    queued.len() == 1
}

/// Takes every edit that waits for the writer thread, in the order they were made.
fn take_queued_edits() -> Vec<Edit> {
    std::mem::take(&mut *lock_even_if_poisoned(&QUEUED_EDITS))
}

/// `changes` applied to the marks file as it is now, after the edits of it that could not be
/// written before (`UNWRITTEN_EDITS`).
fn write(file: &Path, changes: Vec<Edit>) {
    let unwritten = lock_even_if_poisoned(&UNWRITTEN_EDITS).remove(file);
    let mut changes: Vec<Edit> = unwritten.into_iter().flatten().chain(changes).collect();
    let some_left_out = follow_only_to_files_there(&mut changes);
    let stamp_before = file_stamp(file);
    // These changes on the file as another window may have left it; one that cannot be read
    // is not written over with these changes alone, as every other mark would be lost.
    let edit = |bytes: &[u8]| {
        // The files are checked under the file's lock, which another window's Rename Many
        // takes to make the marks follow its swap. Checked before it, stars waited for that
        // swap and then went on the other photo.
        let (taken, refused) = names_taken(&changes);
        // Under the file's lock: no other window wrote since this one read or wrote it. The file
        // is looked at before `KNOWN_STAMPS` is taken, as in `read_file_again`.
        let stamp = file_stamp(file);
        // A pair left out was made in the window's marks, so the window reads the file again.
        let ours = known_stamps().get(file) == Some(&stamp) && !some_left_out;
        let mut map = parse(bytes);
        let before = map.clone();
        for change in &changes {
            apply_edit_to_file(&mut map, change, &taken);
        }
        // An edit that changes no mark, as a move of files without marks, leaves the file as
        // it is. So it is not written again, and the other windows do not read it again.
        if map == before {
            return (bytes.to_vec(), (ours, refused));
        }
        let mut text = to_text(&map);
        text.extend(unread_lines(bytes));
        (text, (ours, refused))
    };
    let result = crate::files::file_io::edit_shared(file, edit);
    // The first file refused is told once the lock is let go.
    let result = result.map(|(ours, refused)| {
        if let Some(refused) = refused {
            crate::files::file_io::save_failed(refused);
        }
        ours
    });
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
        // A file whose stamp changed was written after all (only its folder's sync failed), or
        // by another window, and the edits are not made twice.
        if file_stamp(file) == stamp_before {
            keep_unwritten(file, changes);
        }
    }
}

/// Leaves out of the moves in `changes` each file whose new name holds nothing now, so that its
/// marks stay where they are and never go to an empty name, where a later file of that name
/// would get them. A new name that a later edit names is kept, as the file was there and went
/// on from it. A name that cannot be looked at is left out too, and told. It waits for the
/// disk, so it runs in the writer's thread. Returns whether a file was left out.
fn follow_only_to_files_there(changes: &mut [Edit]) -> bool {
    let mut named_later: std::collections::HashSet<PathBuf> = Default::default();
    let mut some_left_out = false;
    for edit in changes.iter_mut().rev() {
        if let Edit::Follow(pairs, _) = edit {
            pairs.retain(|(_, to)| {
                let is_there = match to.ancestors().any(|a| named_later.contains(a)) {
                    true => true,
                    false => crate::files::file_io::is_something_at(to).unwrap_or_else(|e| {
                        crate::files::file_io::save_failed(format!("{}: {e}", to.display()));
                        false
                    }),
                };
                some_left_out |= !is_there;
                is_there
            });
        }
        let names: Vec<&Path> = match edit {
            Edit::Change(files, _) => files.iter().map(ListedFile::path).collect(),
            Edit::Follow(pairs, _) => pairs.iter().map(|(from, _)| from.as_path()).collect(),
            Edit::FollowFolder(from, _) => vec![from.as_path()],
            Edit::Forget(paths, _) => paths.iter().map(PathBuf::as_path).collect(),
        };
        named_later.extend(names.into_iter().map(Path::to_path_buf));
    }
    some_left_out
}

/// Keeps the edits among `changes` that follow what happened on disk, to make them again
/// (`UNWRITTEN_EDITS`). A move whose marks were not saved would otherwise leave each file with
/// the stars of the file that had its name before.
fn keep_unwritten(file: &Path, changes: Vec<Edit>) {
    let following = changes
        .into_iter()
        .filter(|edit| !matches!(edit, Edit::Change(..)));
    let mut unwritten = lock_even_if_poisoned(&UNWRITTEN_EDITS);
    unwritten
        .entry(file.to_path_buf())
        .or_default()
        .extend(following);
}

/// In a test, the edits are kept for the test to write, so the tests side by side do not write
/// one shared file. A test without a marks file of its own has its edits made to a file in
/// memory instead, as the writer thread would make them, so a delete's marks reach Undo.
#[cfg(test)]
fn save(edit: Edit) {
    if TEST_FILE.with(|f| f.borrow().is_none()) {
        TEST_FILE_IN_MEMORY.with(|m| apply_edit_to_file(&mut m.borrow_mut(), &edit, &[]));
    }
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
    use crate::support::threads::lock_even_if_poisoned;

    /// Reads the marks file again as `refresh` does, and takes what it read, as the window
    /// does at its next frames.
    fn refreshed(marks: &mut super::Marks) {
        marks.refresh();
        settled(marks);
    }

    /// Takes what the marks file's reads in the background give, until none runs.
    fn settled(marks: &mut super::Marks) {
        for _ in 0..10 {
            crate::files::file_io::flush();
            marks.receive();
            if marks.read_again.is_none() {
                break;
            }
        }
    }

    /// A mark set while the file is read again in the background is not lost: what was read
    /// lacks it, so the window keeps its own marks, and reads the file again at the next
    /// refresh.
    #[test]
    fn a_mark_set_while_the_file_is_read_again_is_kept() {
        let id = std::process::id();
        let dir = std::env::temp_dir().join(format!("glimt-test-marks-meanwhile-{id}"));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("marks");
        super::TEST_FILE.with(|f| *f.borrow_mut() = Some(file.clone()));
        let (a, b) = (dir.join("a.png"), dir.join("b.png"));
        let mut marks = super::read_marks();
        // Another window rates b.png.
        crate::files::file_io::edit_shared(&file, |_| {
            (format!("0\t3\t{}\n", b.display()).into_bytes(), ())
        })
        .unwrap();
        // The file is read before this window rates a.png, and the rating is written after
        // that read, as the writer's thread does them in that order.
        marks.refresh();
        crate::files::file_io::flush();
        marks.rate(
            &crate::support::test_support::unlisted(std::slice::from_ref(&a)),
            4,
        );
        super::write(&file, super::SENT.with(|sent| sent.take()));
        marks.receive();
        assert_eq!(marks.get(&a), (0, 4), "the mark set meanwhile is lost");
        // The file is read again after the rating was written, and has both.
        settled(&mut marks);
        let both = (marks.get(&a), marks.get(&b));
        assert_eq!(
            both,
            ((0, 4), (0, 3)),
            "the other window's stars are not read"
        );
        super::TEST_FILE.with(|f| *f.borrow_mut() = None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The marks file read again in the writer's thread asks the window for a frame, so that
    /// another window's marks show at once. Before, they showed only at the next input.
    #[test]
    fn a_read_again_asks_for_a_frame() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let dir =
            std::env::temp_dir().join(format!("glimt-test-marks-repaint-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        super::TEST_FILE.with(|f| *f.borrow_mut() = Some(dir.join("marks")));
        let ctx = eframe::egui::Context::default();
        let asked = std::sync::Arc::new(AtomicBool::new(false));
        let set = asked.clone();
        ctx.set_request_repaint_callback(move |_| set.store(true, Ordering::Relaxed));
        let mut marks = super::read_marks().repainting(&ctx);
        marks.refresh();
        crate::files::file_io::flush();
        assert!(asked.load(Ordering::Relaxed), "no frame asked for");
        settled(&mut marks);
        super::TEST_FILE.with(|f| *f.borrow_mut() = None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A name in another encoding (Latin-1) keeps its bytes, read from the marks file.
    #[test]
    fn reads_a_name_that_is_not_utf8() {
        use std::os::unix::ffi::OsStrExt;
        let marks = super::parse(b"3\t2\t/p/bl\xe5b\xe6r.jpg\n");
        let (path, value) = marks.into_iter().next().unwrap();
        assert_eq!(path.as_os_str().as_bytes(), b"/p/bl\xe5b\xe6r.jpg");
        assert_eq!(value, (3, 2));
    }

    /// Stars set on a name with a line break, which a line of the marks file cannot hold, are
    /// refused with a notice. A file moved or a folder renamed to such a name loses its marks
    /// with a notice too, and the old name forgets them. Before, they were shown and then left
    /// out of the file at its next save without a word.
    #[test]
    fn a_name_with_a_line_break_is_refused_with_a_notice() {
        let dir = std::env::temp_dir().join(format!("glimt-test-marks-nl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("marks");
        super::TEST_FILE.with(|f| *f.borrow_mut() = Some(file.clone()));
        // The file is taken as read, so a follow does not read it again over these marks.
        super::known_stamps().insert(file.clone(), super::file_stamp(&file));
        let (a, b, odd) = (
            dir.join("a.png"),
            dir.join("sub/b.png"),
            dir.join("new\nline"),
        );
        let mut marks = super::Marks::default();
        marks.rate(
            &crate::support::test_support::unlisted(&[a.clone(), b.clone()]),
            3,
        );
        crate::files::file_io::take_saves_failed();
        marks.rate(
            &crate::support::test_support::unlisted(&[odd.join("c.png")]),
            2,
        );
        marks.follow_files(vec![(a.clone(), odd.join("a.png"))], false);
        marks.follow_folder(dir.join("sub"), odd.clone());
        let lost = [
            &odd.join("c.png"),
            &a,
            &odd.join("a.png"),
            &b,
            &odd.join("b.png"),
        ];
        assert!(lost.iter().all(|p| marks.get(p) == (0, 0)));
        let told = crate::files::file_io::take_saves_failed();
        assert_eq!(told.len(), 3, "{told:?}");
        super::SENT.with(|sent| sent.borrow_mut().clear());
        super::TEST_FILE.with(|f| *f.borrow_mut() = None);
        std::fs::remove_dir_all(dir).unwrap();
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
        // The photo moved to c.png, where its marks follow it.
        std::fs::write(&c, "c").unwrap();
        // This window read the marks before the other one wrote these.
        let mut marks = super::Marks::default();
        let text = format!("0\t3\t{}\n0\t2\t{}\n", a.display(), b.display());
        std::fs::write(&file, text).unwrap();
        marks.follow_files(vec![(a.clone(), c.clone())], false);
        // The file read before the move lacks it, so it is read again once the move is written.
        super::write(&file, super::SENT.with(|sent| sent.take()));
        settled(&mut marks);
        assert_eq!((marks.get(&c), marks.get(&a)), ((0, 3), (0, 0)));
        let for_undo = crate::files::trash::ForgottenMarks::default();
        marks.forget(std::slice::from_ref(&b), Some(for_undo.clone()));
        super::write(&file, super::SENT.with(|sent| sent.take()));
        let was = lock_even_if_poisoned(&for_undo).clone();
        assert_eq!((marks.get(&b), was), ((0, 0), vec![(b.clone(), (0, 2))]));
        super::TEST_FILE.with(|f| *f.borrow_mut() = None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A mark's key toggles the mark as the file has it, also when another window set the mark
    /// after this one read the file. Before, the toggle was decided on this window's copy, which
    /// lacked the mark, so the key set it again where the user saw it and meant to clear it.
    #[test]
    fn a_toggle_goes_from_the_mark_another_window_set() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-marks-other-set-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("marks");
        super::TEST_FILE.with(|f| *f.borrow_mut() = Some(file.clone()));
        let (a, b) = (dir.join("a.png"), dir.join("b.png"));
        // This window read the marks before the other one set mark 1 on a.png.
        let mut marks = super::read_marks();
        std::fs::write(&file, format!("1\t0\t{}\n", a.display())).unwrap();
        marks.toggle(
            &crate::support::test_support::unlisted(std::slice::from_ref(&a)),
            1,
        );
        super::write(&file, super::SENT.with(|sent| sent.take()));
        assert_eq!(
            super::read_marks().get(&a),
            (0, 0),
            "the mark was set again"
        );
        // With one of the files without it, the mark is set on both.
        marks.toggle(
            &crate::support::test_support::unlisted(&[a.clone(), b.clone()]),
            1,
        );
        super::write(&file, super::SENT.with(|sent| sent.take()));
        let file_now = super::read_marks();
        assert_eq!((file_now.get(&a), file_now.get(&b)), ((1, 0), (1, 0)));
        settled(&mut marks);
        super::TEST_FILE.with(|f| *f.borrow_mut() = None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A toggle with no other window writing the marks file does not read the whole file back.
    /// The file's bytes are taken away here with its stamp kept, so a read would lose the
    /// marks. Before, every mark key read the file again, also with 500,000 files marked.
    #[test]
    fn a_toggle_alone_does_not_read_the_file_again() {
        let dir = std::env::temp_dir().join(format!(
            "glimt-test-marks-toggle-alone-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("marks");
        super::TEST_FILE.with(|f| *f.borrow_mut() = Some(file.clone()));
        let (a, b) = (dir.join("a.png"), dir.join("b.png"));
        std::fs::write(&file, format!("1\t0\t{}\n", a.display())).unwrap();
        let mut marks = super::read_marks();
        let t = std::fs::metadata(&file).unwrap().modified().unwrap();
        let f = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&file)
            .unwrap();
        f.set_modified(t).unwrap();
        marks.toggle(
            &crate::support::test_support::unlisted(std::slice::from_ref(&b)),
            1,
        );
        settled(&mut marks);
        assert_eq!(
            (marks.get(&a), marks.get(&b)),
            ((1, 0), (1, 0)),
            "read again"
        );
        super::SENT.with(|sent| sent.borrow_mut().clear());
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
        // The photo moved to c.png, where its marks follow it.
        std::fs::write(&c, "c").unwrap();
        // The window takes the file as known, as when its refresh is skipped.
        super::known_stamps().insert(file.clone(), super::file_stamp(&file));
        let mut marks = super::Marks::default();
        super::SENT.with(|sent| sent.borrow_mut().clear());
        marks.follow_files(vec![(a.clone(), c.clone())], false);
        marks.forget(std::slice::from_ref(&b), None);
        let sent: Vec<super::Edit> = super::SENT.with(|sent| sent.take());
        super::write(&file, sent);
        let now = super::parse(&std::fs::read(&file).unwrap());
        assert_eq!(now, std::collections::HashMap::from([(c.clone(), (0, 3))]));
        super::TEST_FILE.with(|f| *f.borrow_mut() = None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A delete while this window's own writes are queued keeps for Undo the marks that the
    /// file holds after those writes. Those are the marks that another window set meanwhile,
    /// which this window has not read, but not a mark that this window cleared. Before, Undo
    /// lost the first, and then, as the file was read before the clear was in it, it brought
    /// back the second.
    #[test]
    fn a_delete_keeps_for_undo_the_marks_not_read_yet() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-marks-undo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("marks");
        super::TEST_FILE.with(|f| *f.borrow_mut() = Some(file.clone()));
        let (a, b, c) = (dir.join("a.png"), dir.join("b.png"), dir.join("c.png"));
        std::fs::write(&file, format!("1\t0\t{}\n", c.display())).unwrap();
        let mut marks = super::read_marks();
        marks.rate(
            &crate::support::test_support::unlisted(std::slice::from_ref(&a)),
            4,
        );
        // Another window marked b.png while this window's stars of a.png were still queued.
        let mut text = std::fs::read(&file).unwrap();
        text.extend(format!("0\t2\t{}\n", b.display()).as_bytes());
        std::fs::write(&file, text).unwrap();
        super::TEST_WRITES_QUEUED.with(|q| q.set(true));
        // The user clears c.png's only mark, and then deletes the three files.
        marks.toggle(
            &crate::support::test_support::unlisted(std::slice::from_ref(&c)),
            1,
        );
        let for_undo = crate::files::trash::ForgottenMarks::default();
        marks.forget(&[a.clone(), b.clone(), c.clone()], Some(for_undo.clone()));
        super::TEST_WRITES_QUEUED.with(|q| q.set(false));
        // The writer thread then writes the queued edits in order.
        super::write(&file, super::SENT.with(|sent| sent.take()));
        let mut was = lock_even_if_poisoned(&for_undo).clone();
        was.sort();
        assert_eq!(was, [(a, (0, 4)), (b, (0, 2))]);
        assert!(super::parse(&std::fs::read(&file).unwrap()).is_empty());
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
        let mut marks = super::read_marks();
        let stars = |p: &std::path::Path, n| {
            super::Edit::Change(
                crate::support::test_support::unlisted([p]),
                super::Change::Stars(n),
            )
        };
        marks.rate(
            &crate::support::test_support::unlisted(std::slice::from_ref(&a)),
            4,
        );
        super::write(&file, vec![stars(&a, 4)]);
        // The file's bytes gone but the file and its time kept: read again, it would have no
        // stars.
        let t = std::fs::metadata(&file).unwrap().modified().unwrap();
        let f = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&file)
            .unwrap();
        f.set_modified(t).unwrap();
        refreshed(&mut marks);
        assert_eq!(marks.get(&a), (0, 4));
        // Another window's write just after, maybe in the same tick of the clock: read.
        super::write(&file, vec![stars(&a, 4)]);
        crate::files::file_io::edit_shared(&file, |old| {
            let mut text = old.to_vec();
            text.extend(format!("0\t2\t{}\n", b.display()).as_bytes());
            (text, ())
        })
        .unwrap();
        refreshed(&mut marks);
        assert_eq!((marks.get(&a), marks.get(&b)), ((0, 4), (0, 2)));
        super::TEST_FILE.with(|f| *f.borrow_mut() = None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Stars for a file whose name another file has taken since the list showed it are left
    /// out of the marks file, and those of a file still listed are written. Another window's
    /// Rename Many that swapped two names had the stars land on the other photo.
    #[test]
    fn stars_for_a_name_another_file_has_taken_are_not_written() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-marks-taken-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("marks");
        super::TEST_FILE.with(|f| *f.borrow_mut() = Some(file.clone()));
        let (a, b) = (dir.join("a.png"), dir.join("b.png"));
        std::fs::write(&a, "a").unwrap();
        std::fs::write(&b, "b").unwrap();
        let listed = |p: &std::path::Path| {
            let identity = crate::files::file_io::identity_of(p);
            crate::files::file_io::ListedFile::new(p.to_path_buf(), identity, &Default::default())
        };
        let (listed_a, listed_b) = (listed(&a), listed(&b));
        // Another file takes a.png's name, made before the old one goes, so it has another
        // inode.
        std::fs::write(dir.join("new"), "new").unwrap();
        std::fs::rename(dir.join("new"), &a).unwrap();
        let mut marks = super::read_marks();
        marks.rate(&[listed_a, listed_b], 3);
        super::write(&file, super::SENT.with(|sent| sent.take()));
        let now = super::parse(&std::fs::read(&file).unwrap());
        assert_eq!((now.get(&a), now.get(&b)), (None, Some(&(0, 3))));
        super::TEST_FILE.with(|f| *f.borrow_mut() = None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Stars for a photo whose name another window's Rename Many gives to another photo, while
    /// the stars wait for the marks file's lock, go on neither photo. That window holds the lock
    /// to make the marks follow its swap. Before, the check was made before the lock, passed,
    /// and the stars went on the other photo.
    #[test]
    fn stars_waiting_for_another_windows_swap_are_checked_after_it() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-marks-swap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("marks");
        super::TEST_FILE.with(|f| *f.borrow_mut() = Some(file.clone()));
        let (a, b) = (dir.join("a.png"), dir.join("b.png"));
        std::fs::write(&a, "a").unwrap();
        std::fs::write(&b, "b").unwrap();
        let identity = crate::files::file_io::identity_of(&a);
        let listed =
            crate::files::file_io::ListedFile::new(a.clone(), identity, &Default::default());
        let (held, lock_held) = std::sync::mpsc::channel();
        let other = std::thread::spawn({
            let (file, a, b) = (file.clone(), a.clone(), b.clone());
            move || {
                crate::files::file_io::edit_shared(&file, |old| {
                    held.send(()).unwrap();
                    std::thread::sleep(std::time::Duration::from_millis(300));
                    std::fs::rename(&a, a.with_extension("between")).unwrap();
                    std::fs::rename(&b, &a).unwrap();
                    std::fs::rename(a.with_extension("between"), &b).unwrap();
                    (old.to_vec(), ())
                })
                .unwrap();
            }
        });
        lock_held.recv().unwrap();
        super::read_marks().rate(&[listed], 3);
        super::write(&file, super::SENT.with(|sent| sent.take()));
        other.join().unwrap();
        let now = super::parse(&std::fs::read(&file).unwrap_or_default());
        assert_eq!((now.get(&a), now.get(&b)), (None, None));
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
        let mut marks = super::read_marks();
        marks.rate(
            &crate::support::test_support::unlisted(std::slice::from_ref(&a)),
            3,
        );
        let stars = super::Edit::Change(
            crate::support::test_support::unlisted(vec![a.clone()]),
            super::Change::Stars(3),
        );
        super::write(&file, vec![stars]);
        refreshed(&mut marks);
        assert_eq!(marks.get(&a), (0, 0));
        super::TEST_FILE.with(|f| *f.borrow_mut() = None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Names that two files swapped, whose marks could not be saved, keep their marks on the
    /// right file: in the window, when the file is read again, and in the file at the next
    /// write. Before, the window took back the marks file as it was, so each photo showed the
    /// other's stars, and the next write left them so.
    #[test]
    fn marks_that_follow_files_are_written_later_when_they_could_not_be() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-marks-follow-{}", std::process::id()));
        // A folder where the marks file would be: it cannot be written.
        let file = dir.join("marks");
        std::fs::create_dir_all(&file).unwrap();
        super::TEST_FILE.with(|f| *f.borrow_mut() = Some(file.clone()));
        let (a, b) = (dir.join("a.jpg"), dir.join("b.jpg"));
        // The photos are there, as marks follow only to a name where a file is.
        std::fs::write(&a, "a").unwrap();
        std::fs::write(&b, "b").unwrap();
        let swap = vec![(a.clone(), b.clone()), (b.clone(), a.clone())];
        super::write(&file, vec![super::Edit::Follow(swap, false)]);
        // The file as it was before, readable again.
        std::fs::remove_dir(&file).unwrap();
        let text = format!("0\t1\t{}\n0\t2\t{}\n", a.display(), b.display());
        std::fs::write(&file, text).unwrap();
        let mut marks = super::read_marks();
        assert_eq!((marks.get(&a), marks.get(&b)), ((0, 2), (0, 1)));
        let c = dir.join("c.jpg");
        super::write(
            &file,
            vec![super::Edit::Change(
                crate::support::test_support::unlisted(vec![c.clone()]),
                super::Change::Stars(5),
            )],
        );
        refreshed(&mut marks);
        let written = super::parse(&std::fs::read(&file).unwrap());
        for map in [&marks.by_path, &written] {
            assert_eq!((map.get(&a), map.get(&b)), (Some(&(0, 2)), Some(&(0, 1))));
        }
        assert_eq!(written.get(&c), Some(&(0, 5)));
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
        let mut marks = super::read_marks();
        assert_eq!(marks.get(&a), (192, 3));
        marks.toggle(
            &crate::support::test_support::unlisted(std::slice::from_ref(&a)),
            1,
        );
        assert_eq!(marks.get(&a), (193, 3));
        let mark = super::Edit::Change(
            crate::support::test_support::unlisted(vec![a.clone()]),
            super::Change::Mark(1, true),
        );
        let stars = super::Edit::Change(
            crate::support::test_support::unlisted(["/c.jpg"]),
            super::Change::Stars(2),
        );
        super::write(&file, vec![mark, stars]);
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

    /// A move of a file without marks leaves the marks file alone: it is not written again,
    /// so its stamp stays and the other windows do not read it again. Before, every move,
    /// rename and delete wrote the whole file.
    #[test]
    fn an_edit_that_changes_nothing_writes_nothing() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-marks-same-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("marks");
        std::fs::write(&file, "1\t0\t/a.jpg\n").unwrap();
        let stamp = || {
            use std::os::unix::fs::MetadataExt;
            let meta = std::fs::metadata(&file).unwrap();
            (meta.ino(), meta.mtime_nsec())
        };
        let before = stamp();
        let pairs = vec![("/b.jpg".into(), "/c.jpg".into())];
        super::write(&file, vec![super::Edit::Follow(pairs, false)]);
        super::write(
            &file,
            vec![super::Edit::Forget(vec!["/d.jpg".into()], None)],
        );
        assert_eq!(stamp(), before);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "1\t0\t/a.jpg\n");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Marks never follow a file to a name where nothing is, where a later file of that name
    /// would get them. They stay on the old name. A file that went on from the new name in a
    /// later edit still takes its marks along.
    #[test]
    fn marks_follow_only_to_a_name_where_a_file_is() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-marks-empty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("marks");
        let p = |n: &str| dir.join(n);
        std::fs::write(p("there.jpg"), "a").unwrap();
        std::fs::write(p("last.jpg"), "c").unwrap();
        // A name on a share that does not answer may hold nothing, so its marks stay too.
        std::fs::write(p("unanswered.jpg"), "d").unwrap();
        crate::support::threads::lock_even_if_poisoned(&crate::files::file_io::LOOKS_FAIL)
            .push(p("unanswered.jpg"));
        let line = |stars: u8, n: &str| format!("0\t{stars}\t{}\n", p(n).display());
        let text = [
            line(1, "a.jpg"),
            line(2, "b.jpg"),
            line(3, "c.jpg"),
            line(4, "d.jpg"),
        ];
        std::fs::write(&file, text.concat()).unwrap();
        let moves = vec![
            (p("a.jpg"), p("there.jpg")),
            (p("b.jpg"), p("gone.jpg")),
            (p("c.jpg"), p("middle.jpg")),
            (p("d.jpg"), p("unanswered.jpg")),
        ];
        let on = vec![(p("middle.jpg"), p("last.jpg"))];
        let edits = vec![
            super::Edit::Follow(moves, false),
            super::Edit::Follow(on, false),
        ];
        super::write(&file, edits);
        let written = super::parse(&std::fs::read(&file).unwrap());
        assert_eq!(written.get(&p("there.jpg")), Some(&(0, 1)));
        assert_eq!(written.get(&p("b.jpg")), Some(&(0, 2)));
        assert_eq!(written.get(&p("gone.jpg")), None);
        assert_eq!(written.get(&p("last.jpg")), Some(&(0, 3)));
        assert_eq!(written.get(&p("d.jpg")), Some(&(0, 4)));
        crate::support::threads::lock_even_if_poisoned(&crate::files::file_io::LOOKS_FAIL)
            .retain(|n| *n != p("unanswered.jpg"));
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
        m.toggle(
            &crate::support::test_support::unlisted(std::slice::from_ref(&a)),
            2,
        );
        // Mark 2 on only one of them: the toggle sets it on both, the next clears both.
        m.toggle(&crate::support::test_support::unlisted(&both), 2);
        assert_eq!((m.get(&a), m.get(&b)), ((0b10, 0), (0b10, 0)));
        m.rate(
            &crate::support::test_support::unlisted(std::slice::from_ref(&b)),
            3,
        );
        assert_eq!(super::text(m.get(&b)), "★★★  2");
        m.toggle(&crate::support::test_support::unlisted(&both), 2);
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
        super::apply(&mut map, &[a.as_path()], super::Change::Mark(4, true));
        super::apply(&mut map, &[b.as_path()], super::Change::Stars(4));
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
        let mut m = super::read_marks();
        m.rate(
            &crate::support::test_support::unlisted(&[p("/a.jpg"), p("/b.jpg")]),
            4,
        );
        m.toggle(&crate::support::test_support::unlisted(&[p("/a.jpg")]), 1);
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
        m.forget(&[p("/b.jpg")], None);
        assert_eq!(m.get(&p("/b.jpg")), (0, 0));
        // A folder renamed: its files' marks go along; trashed: they go. The folders are only
        // names, as nothing on the disk is looked at to tell a folder.
        m.rate(
            &crate::support::test_support::unlisted(&[p("/trip/old/a.jpg")]),
            5,
        );
        m.follow_folder(p("/trip/old"), p("/trip/2024"));
        assert_eq!(m.get(&p("/trip/2024/a.jpg")), (0, 5));
        m.forget(&[p("/trip/2024")], None);
        assert_eq!(m.get(&p("/trip/2024/a.jpg")), (0, 0));
        // An old entry where an unmarked file lands is cleared.
        m.follow_files(vec![(p("/c.jpg"), p("/b copy.jpg"))], false);
        assert_eq!(m.get(&p("/b copy.jpg")), (0, 0));
        // Rename Many swapping names, and shifting them: each keeps its own.
        m.rate(&crate::support::test_support::unlisted(&[p("/x.jpg")]), 1);
        m.rate(&crate::support::test_support::unlisted(&[p("/y.jpg")]), 2);
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
        m.toggle(
            &crate::support::test_support::unlisted(std::slice::from_ref(&a)),
            2,
        );
        m.rate(&crate::support::test_support::unlisted(&[a]), 3);
        for text in crate::support::test_support::broken(&super::to_text(&m.by_path)) {
            super::parse(&text);
        }
        super::TEST_FILE.with(|f| *f.borrow_mut() = None);
    }

    /// Every writer of a sidecar keeps what the others wrote, in either order: the stars, the
    /// keywords and tree paths, the comment, the alt text and another program's property. Two
    /// windows that write one sidecar in turn each keep the other's change.
    #[test]
    fn every_sidecar_writer_keeps_what_the_others_wrote() {
        use crate::metadata::xmp;
        type Write = fn(&std::path::Path) -> Result<(), String>;
        let writers: [(&str, Write, &str); 6] = [
            ("stars", |p| super::write_xmp(p, 3), "Rating=\"3\""),
            (
                "tag",
                |p| xmp::tag_file(p, "Animals", true),
                "<rdf:li>Animals</rdf:li>",
            ),
            (
                "comment",
                |p| xmp::write_changed(p, None, Some("Trip".into())).map(drop),
                ">Trip<",
            ),
            (
                "alt text",
                |p| {
                    let text = xmp::AltText {
                        text: "A dog".into(),
                        extended_description: String::new(),
                    };
                    xmp::write_alt_text(p, &text)
                },
                ">A dog<",
            ),
            (
                "keywords",
                |p| xmp::add_keywords(p, &["Oslo".into()]),
                "<rdf:li>Oslo</rdf:li>",
            ),
            (
                "tree path",
                |p| xmp::add_tags(p, &["Places|Norway".into()]),
                "Places|Norway",
            ),
        ];
        let dir = std::env::temp_dir().join(format!("glimt-test-writers-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let image = dir.join("a.jpg");
        image::RgbImage::new(4, 2).save(&image).unwrap();
        let side = xmp::sidecar(&image);
        for reversed in [false, true] {
            let theirs = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF><rdf:Description \
                          rdf:about=\"\" xmlns:darktable=\"http://darktable.sf.net/\" \
                          darktable:history_end=\"7\"/></rdf:RDF></x:xmpmeta>";
            std::fs::write(&side, theirs).unwrap();
            let mut order: Vec<_> = writers.iter().collect();
            if reversed {
                order.reverse();
            }
            for (done, (name, write, _)) in order.iter().enumerate() {
                write(&image).unwrap();
                let text = std::fs::read_to_string(&side).unwrap();
                for (kept, _, mark) in &order[..=done] {
                    assert!(text.contains(mark), "{kept} lost after {name}: {text}");
                }
                assert!(
                    text.contains("darktable:history_end=\"7\""),
                    "after {name}: {text}"
                );
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A hundred edits made while a write waits go to the file in one write. Only the first asks
    /// the writer thread for a write, which rewrites the whole file, about 0.4 s with 500,000
    /// marked. Before, each edit asked for a write of its own.
    #[test]
    fn edits_waiting_for_the_writer_go_in_one_write() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-marks-batch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("marks");
        let paths: Vec<std::path::PathBuf> =
            (0..100).map(|i| dir.join(format!("{i}.jpg"))).collect();
        let writes_asked = (paths.iter())
            .map(|p| {
                super::Edit::Change(
                    crate::support::test_support::unlisted(vec![p.clone()]),
                    super::Change::Stars(3),
                )
            })
            .map(super::queue_for_writer)
            .filter(|&asked| asked)
            .count();
        let edits = super::take_queued_edits();
        assert_eq!((writes_asked, edits.len()), (1, 100));
        super::write(&file, edits);
        let now = super::parse(&std::fs::read(&file).unwrap());
        let expected: std::collections::HashMap<_, _> =
            paths.into_iter().map(|p| (p, (0, 3))).collect();
        assert_eq!(now, expected);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
