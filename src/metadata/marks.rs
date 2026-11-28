//! Optional marks and ratings (`cargo build --features marks`): Geeqie's marks 1–6 and 0–5
//! stars, kept by full path in `$XDG_DATA_HOME/glimt/marks`, one file as "bits\tstars\tpath"
//! lines. They follow files that Glimt copies, moves and renames, and go with those it deletes;
//! files renamed or moved outside Glimt lose them.

use crate::files::config::fits_on_a_line;
use crate::files::file_io::{ListedFile, Transfer};
use crate::files::journal::MarksAtNewNames;
use crate::files::trash::ForgottenMarks;
use crate::metadata::xmp_packet::Packet;
use crate::support::format::ShownText;
use crate::support::format::escaped_name;
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

/// The marks files that a test makes unreadable, while each keeps its inode and time. Reads
/// are made in the writer's thread, so a thread-local of the test would not reach them.
#[cfg(test)]
static UNREADABLE_IN_TEST: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

/// Whether this window keeps the marks file where Glimt 0.95 and older kept it,
/// `$XDG_STATE_HOME/glimt`, because its move to `$XDG_DATA_HOME/glimt` at start failed.
static MARKS_IN_OLD_FOLDER: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

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
    /// was asked for.
    read_again: crate::support::threads::Receiving<(u64, ReadAgain)>,
    /// The marks and stars that this window's changes replaced, by path, since the marks file
    /// last showed that it holds what this window holds. Each path keeps the value of its first
    /// change. A change whose write failed is not kept, so when the file cannot be read to show
    /// which did, they are all put back. They follow a move and go with a delete, as `by_path`
    /// does, so that a file that later takes the name does not get them.
    not_confirmed: ValuesToPutBack,
    /// The window's context, asked for a frame when the file has been read again, so that
    /// marks another window set show at once, not at the next input.
    repaint: eframe::egui::Context,
    /// Whether the marks this window holds changed since `take_changed` was last asked. Every
    /// change goes through `edit` or `receive`, so a search by marks or stars learns of each
    /// one in one place: a key, an import, a crop, Undo, or another window's marks read again.
    changed: bool,
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
            let read = match is_as_held {
                true => ReadAgain::AsHeld,
                false => read_marks_file(&file).map_or(ReadAgain::Unreadable, ReadAgain::Read),
            };
            let _ = tx.send((asked_at, read));
            repaint.request_repaint();
        });
        self.read_again = crate::support::threads::Receiving::of(rx);
    }

    /// Takes the marks file as `refresh` read it again. When this window made an edit after it
    /// was asked for, what was read lacks that edit and is dropped, so a mark set meanwhile is
    /// not lost. The file is then read again after that edit is written, also when that write
    /// leaves it as this window holds it, so what another window wrote shows all the same.
    /// When the file cannot be read, the window keeps its marks, but for the changes that the
    /// file has not shown to hold (`not_confirmed`), as their writes may have failed.
    pub fn receive(&mut self) {
        let crate::support::threads::Took::Answer((asked_at, read)) = self.read_again.take() else {
            return;
        };
        let is_up_to_date = asked_at == self.edits_made;
        match read {
            ReadAgain::AsHeld if is_up_to_date => self.not_confirmed.0.clear(),
            ReadAgain::AsHeld => {}
            ReadAgain::Read(by_path) if is_up_to_date => {
                self.changed |= self.by_path != by_path;
                self.by_path = by_path;
                self.not_confirmed.0.clear();
            }
            ReadAgain::Unreadable if is_up_to_date => {
                self.changed |= !self.not_confirmed.0.is_empty();
                for (path, value) in self.not_confirmed.0.drain() {
                    match value {
                        (0, 0) => drop(self.by_path.remove(&path)),
                        value => drop(self.by_path.insert(path, value)),
                    }
                }
            }
            ReadAgain::Read(_) | ReadAgain::Unreadable => self.read_file_again(true),
        }
    }

    /// Whether a read of the marks file that `refresh` queued has not come back yet.
    pub fn is_reading(&self) -> bool {
        self.read_again.is_some()
    }

    /// Returns whether the marks or stars this window holds changed since this was last asked,
    /// so that a search by them is run again.
    pub fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }

    /// Every file with a mark or stars, and them.
    pub fn marked(&self) -> impl Iterator<Item = (&Path, (u8, u8))> {
        (self.by_path.iter()).map(|(p, &(bits, stars))| (p.as_path(), (bits, stars.min(5))))
    }

    /// (mark bits, stars) of `path`. Stars above 5, which a later Glimt may write, are shown as
    /// 5 and kept in the file as they are.
    pub fn get(&self, path: &Path) -> (u8, u8) {
        let (bits, stars) = self.by_path.get(path).copied().unwrap_or_default();
        (bits, stars.min(5))
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

    /// Gives `files` the stars `stars`. The file is read back after the write, as for `toggle`,
    /// so that stars the writer refused (`ListedFile::check`) do not stay shown. It reads the
    /// file only when the write did not leave it as this window holds it.
    pub fn rate(&mut self, files: &[ListedFile], stars: u8) {
        self.change(files, Change::Stars(stars));
        self.read_file_again(false);
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

    /// Files copied or moved and renamed `(from, to)`, as `transfer` says: the new ones get
    /// the marks. A folder renamed goes through `follow_folder`, so a report of files never
    /// stats them on the UI thread to find a folder among them. When the moves are known from a
    /// record (`known_from`), they were not made just now, and the writer gives the marks only
    /// to a name where a file is (`follow_only_to_files_there`).
    pub fn follow_files(
        &mut self,
        pairs: Vec<(PathBuf, PathBuf)>,
        transfer: Transfer,
        known_from: MovesKnownFrom,
    ) {
        // From the file as it is: stars set in another window since this one last read it were
        // left behind on the old names, as this window did not know them.
        self.refresh();
        // Every pair goes to the file, also one whose marks this window does not know, for
        // the reason `forget` gives.
        if !pairs.is_empty() {
            self.edit(Edit::Follow(
                pairs,
                transfer,
                known_from,
                Default::default(),
            ));
        }
    }

    /// Files that Rename Many renamed, or put back after it was stopped or failed, `(from, to)`,
    /// with the marks that the window knew at each photo's new name when the job began. A mark
    /// set at a new name while the job ran follows the photo, and an old mark that a file gone
    /// from that name left behind never does (`follow_pairs`). `known_from` is as for
    /// `follow_files`.
    pub fn follow_renames(
        &mut self,
        pairs: Vec<(PathBuf, PathBuf)>,
        known_from: MovesKnownFrom,
        marks_at_new_names: MarksAtNewNames,
    ) {
        self.refresh();
        if !pairs.is_empty() {
            self.edit(Edit::Follow(
                pairs,
                Transfer::Move,
                known_from,
                marks_at_new_names,
            ));
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
        // The next frame runs a search by marks again (`take_changed`).
        self.changed = true;
        self.repaint.request_repaint();
        for edit in refuse_names_with_line_breaks(edit, &self.by_path) {
            // Only a change of a mark or stars is left out when its write fails. The other
            // edits follow the disk, and are made again at the next write.
            match &edit {
                Edit::Change(files, _) => {
                    for file in files {
                        let value = self.by_path.get(file.path()).copied().unwrap_or_default();
                        (self.not_confirmed.0)
                            .entry(file.path().to_path_buf())
                            .or_insert(value);
                    }
                }
                // The values to put back follow the files, so that they are put back on the
                // file they were about, not on another file that takes its name later.
                Edit::Follow(..) | Edit::FollowFolder(..) | Edit::Forget(..) => {
                    self.not_confirmed.follow(&edit)
                }
            }
            apply_edit(&mut self.by_path, &edit, &[]);
            save(edit);
        }
    }
}

/// The marks and stars that a window's changes replaced, by path, to be put back when their
/// writes may have failed (`Marks::not_confirmed`). A path without an entry has no change to put
/// back, which is not a path without marks. So these values follow a move as they are, and are
/// never merged with the marks changed at a new name while a Rename Many ran, as `follow_pairs`
/// merges marks. Before, such a name was taken for marks cleared during the job, and the value
/// to put back on the photo was lost.
#[derive(Default)]
struct ValuesToPutBack(MarksByPath);

impl ValuesToPutBack {
    /// Makes `edit`, a move or a delete, to the values, so that each stays with its file.
    fn follow(&mut self, edit: &Edit) {
        match edit {
            Edit::Follow(pairs, transfer, ..) => {
                follow_pairs(&mut self.0, pairs, *transfer, &MarksAtNewNames::new())
            }
            edit => apply_edit(&mut self.0, edit, &[]),
        }
    }
}

/// What the marks file read again in the writer's thread gave.
enum ReadAgain {
    /// The file is as this window's marks hold it, and was not read.
    AsHeld,
    /// The marks that the file holds.
    Read(MarksByPath),
    /// The file could not be read, which was told.
    Unreadable,
}

/// A marks file told apart from another: every write makes a new file (a new inode), and two
/// writes in the same tick of the clock have the same time.
type FileStamp = (u64, std::time::SystemTime);

// The marks file of one test, on its thread: the tests run side by side, and one that writes
// the shared file would change what the others read back.
#[cfg(test)]
thread_local! {
    pub(crate) static TEST_FILE: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
    static SENT: std::cell::RefCell<Vec<Edit>> = const { std::cell::RefCell::new(Vec::new()) };
    static TEST_FILE_IN_MEMORY: std::cell::RefCell<HashMap<PathBuf, (u8, u8)>> =
        std::cell::RefCell::new(HashMap::new());
    /// Whether a test takes this window's own writes as queued. `WRITING` is shared by the
    /// tests that run side by side, so a test that raised it would skip their refreshes too.
    static TEST_WRITES_QUEUED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// What `move_marks_file` did with the marks file of Glimt 0.95 and older.
#[derive(Debug, PartialEq)]
enum MarksMove {
    /// The old file was moved to its new place, or there was none to move.
    Done,
    /// Both files are there. The new one is used, and the old one is left as it is, which the
    /// text tells the user.
    BothKept(crate::support::format::ShownText),
    /// The old file could not be moved, which was told, and is used where it is.
    Failed,
}

/// What a window did, applied to the file as it is then.
enum Edit {
    /// A change of the files picked, each checked to be the one listed before it is made.
    Change(Vec<ListedFile>, Change),
    /// Files moved or copied from each first path to the second. Moves read from a record,
    /// rather than made by the job that sent them, have each new name looked at before the
    /// marks go there. The last field holds the marks at the new names of a Rename Many when
    /// it began, empty for any other move (`follow_pairs`).
    Follow(
        Vec<(PathBuf, PathBuf)>,
        Transfer,
        MovesKnownFrom,
        MarksAtNewNames,
    ),
    /// A folder renamed from the first path to the second, with every file in it.
    FollowFolder(PathBuf, PathBuf),
    /// Files or folders deleted, with every file in the folders. The marks that the file held
    /// under them are added to the second field, for Undo.
    Forget(Vec<PathBuf>, Option<ForgottenMarks>),
}

/// Where the moves that marks follow are known from: the job that just made them, so each file
/// is at its new name, or a record read after a kill, so a new name may hold nothing now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MovesKnownFrom {
    TheJob,
    ARecord,
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
/// before the window opens (`main`), or in a test. A file that cannot be read gives no marks
/// for now, and the next `refresh` reads it again.
pub fn read_marks() -> Marks {
    let by_path = marks_file()
        .and_then(|f| read_marks_file(&f))
        .unwrap_or_default();
    Marks {
        by_path,
        ..Default::default()
    }
}

/// Reads the marks in the marks file `file` and notes the file as read (`known_stamps`). A file
/// that is not there holds no marks. Returns `None` when the file cannot be read, which is told
/// and not noted, so that the window keeps its marks and the next refresh reads it again.
fn read_marks_file(file: &Path) -> Option<HashMap<PathBuf, (u8, u8)>> {
    // The file is looked at before it is read, so a write in between makes it read again. It is
    // also looked at before the lock is taken, as in `read_file_again`.
    let stamp = file_stamp(file);
    let read = crate::images::decode::read_limited(file);
    #[cfg(test)]
    let read = match lock_even_if_poisoned(&UNREADABLE_IN_TEST).contains(&file.to_path_buf()) {
        true => Err(std::io::Error::other("made unreadable by a test")),
        false => read,
    };
    let text = match read {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => {
            // The stamp of this window's last write is no longer known to be what the window
            // holds, as the window may put back its changes now. Kept, it would make the next
            // refresh take the file as held when the share answers again, unchanged, and never
            // read it.
            known_stamps().remove(file);
            crate::files::file_io::save_failed(crate::tf!(
                "Could not read the marks file {name}: {e}",
                name = escaped_name(file),
                e
            ));
            return None;
        }
    };
    known_stamps().insert(file.to_path_buf(), stamp);
    let mut map = parse(&text);
    for edit in lock_even_if_poisoned(&UNWRITTEN_EDITS)
        .get(file)
        .into_iter()
        .flatten()
    {
        apply_edit(&mut map, edit, &[]);
    }
    Some(map)
}

/// Returns the inode and time of `file`, which show that the file at the path changed since,
/// or `None` when it cannot be looked up.
#[allow(clippy::disallowed_methods)] // never tells two files apart, which is save::file_id's
fn file_stamp(file: &Path) -> Option<FileStamp> {
    use std::os::unix::fs::MetadataExt;
    let m = crate::files::file_io::look_through(file).metadata().ok()?;
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

/// Returns the marks file, `$XDG_DATA_HOME/glimt/marks`, or where Glimt 0.95 and older kept it
/// while it could not be moved from there (`move_old_marks_file`). Returns `None` when neither
/// $HOME nor the XDG folder is set.
pub(crate) fn marks_file() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(file) = TEST_FILE.with(|f| f.borrow().clone()) {
        return Some(file);
    }
    match MARKS_IN_OLD_FOLDER.load(std::sync::atomic::Ordering::Relaxed) {
        true => old_marks_file(),
        false => crate::files::config::xdg_dir("XDG_DATA_HOME", ".local/share", "glimt")
            .map(|d| d.join("marks")),
    }
}

/// Returns where Glimt 0.95 and older kept the marks file, `$XDG_STATE_HOME/glimt/marks`, the
/// folder for state that can be made again, which marks cannot.
fn old_marks_file() -> Option<PathBuf> {
    crate::files::config::xdg_dir("XDG_STATE_HOME", ".local/state", "glimt")
        .map(|d| d.join("marks"))
}

/// Moves the marks file of Glimt 0.95 and older to its place now (`move_marks_file`). It waits
/// for the disk, so `main` calls it before the window opens and before the marks are read. A
/// move that failed leaves this window with the old file, where it is. Returns the notice that
/// both files are there, when they are.
pub fn move_old_marks_file() -> Option<crate::support::format::ShownText> {
    let (old, new) = (old_marks_file()?, marks_file()?);
    match move_marks_file(&old, &new) {
        MarksMove::Done => None,
        MarksMove::BothKept(text) => Some(text),
        MarksMove::Failed => {
            MARKS_IN_OLD_FOLDER.store(true, std::sync::atomic::Ordering::Relaxed);
            None
        }
    }
}

/// Moves the marks file `old` to `new` when only `old` is there, and leaves a link to `new` at
/// `old` (`link_old_name`). It holds the lock that every window takes to write `old`
/// (`marks_lock`), so that no window writes it during the move. It never writes over `new`, so
/// it needs no lock of that. On one disk the move is a rename, and to another a copy that is on
/// disk before the old file is removed (`put_file`), so the marks are never lost. When both are
/// there, the new one is used and the old one is left as it is, unless both hold the same
/// marks. A move that failed is told, and the old file is used where it is.
fn move_marks_file(old: &Path, new: &Path) -> MarksMove {
    move_marks_file_by(old, new, |file| {
        let m = crate::files::file_io::look_through(file)
            .metadata()
            .map_err(|e| ShownText::of_error(&e))?;
        Ok(crate::images::save::file_id_from_metadata(file, &m))
    })
}

/// `move_marks_file`, which tells two files apart by `identity`, the look at a file that
/// returns what tells it from every other, or why the look failed.
fn move_marks_file_by(
    old: &Path,
    new: &Path,
    identity: impl Fn(&Path) -> Result<crate::images::save::FileId, ShownText>,
) -> MarksMove {
    use crate::files::file_io::{is_surely_gone, lock_or_tell};
    // One file under both names has nothing to move.
    if is_surely_gone(old) || is_one_file(old, new) {
        return MarksMove::Done;
    }
    let failed = |e: &dyn crate::support::format::Shown| {
        crate::files::file_io::save_failed(crate::tf!(
            "Could not move the marks file {from} to {to}: {e}. The marks are kept where they are.",
            from = escaped_name(old),
            to = escaped_name(new),
            e
        ));
        // A move that failed after the copy was made may have left the file only at `new`.
        match is_surely_gone(old) {
            true => MarksMove::Done,
            false => MarksMove::Failed,
        }
    };
    if let Err(e) = std::fs::create_dir_all(new.parent().unwrap_or(Path::new("."))) {
        return failed(&e);
    }
    // One lock only: two, of names that cannot be told to be one file, are one lock when they
    // are, and the second would wait for the first for good.
    let _lock = marks_lock(old).and_then(|lock| lock_or_tell(&lock));
    // Another window may have moved it while this one waited for the lock.
    if is_surely_gone(old) || is_one_file(old, new) {
        return MarksMove::Done;
    }
    // A new file that could not be looked at counts as there, so the old one is left alone.
    if !is_surely_gone(new) {
        // The same marks under both names, as a move to another disk that was cut short or a
        // backup copied back leaves them, are one file, and the old name becomes the link. It
        // goes only when both are surely two files: after a failed look they may be one file
        // through a linked folder, whose one name would go with every mark.
        let read = crate::images::decode::read_limited;
        if read(old).is_ok_and(|marks| read(new).is_ok_and(|other| other == marks)) {
            return match identity(old).and_then(|a| Ok(a != identity(new)?)) {
                Ok(true) => match crate::files::file_io::remove_own(old) {
                    Ok(()) => link_old_name(old, new),
                    Err(e) => failed(&e),
                },
                Ok(false) => MarksMove::Done,
                Err(e) => failed(&e),
            };
        }
    }
    if let Some(text) = old_file_left_beside(old, new) {
        return MarksMove::BothKept(text);
    }
    match move_own_file(old, new) {
        Ok(()) => link_old_name(old, new),
        Err(e) => failed(&e),
    }
}

/// Leaves a link to `new`, where the marks file is now, at `old`, its name in Glimt 0.95 and
/// older, which the move has just emptied. A window of those versions that is still open writes
/// through it, as their `replace_file` writes the file a link points to. Without the link, such a
/// window would make a new file at `old` for a mark set, and its moves and deletes, which make
/// no file, would be lost. Glimt itself writes through the link under the lock of `new`
/// (`marks_lock`). A link that cannot be made is told. Returns `Done`, as the marks are at `new`
/// either way.
fn link_old_name(old: &Path, new: &Path) -> MarksMove {
    #[allow(clippy::disallowed_methods)] // Glimt's own: the marks file's old name, under its lock
    let linked = std::os::unix::fs::symlink(new, old);
    if let Err(e) = linked {
        crate::files::file_io::save_failed(crate::tf!(
            "Could not save {name}: {e}",
            name = escaped_name(old),
            e
        ));
    }
    MarksMove::Done
}

/// Returns whether `old` and `new` name one marks file: one file under two names, as when
/// $XDG_STATE_HOME and $XDG_DATA_HOME are one folder or one is a link to the other, or the link
/// to `new` that the move leaves at `old`, also while no file is at `new`.
fn is_one_file(old: &Path, new: &Path) -> bool {
    use crate::images::save::file_id;
    is_link_to(old, new)
        || matches!((file_id(old), file_id(new)), (Some(Ok(a)), Some(Ok(b))) if a == b)
}

/// Returns whether `old` is a link to `new`, as the move of the marks file leaves it.
fn is_link_to(old: &Path, new: &Path) -> bool {
    std::fs::read_link(old).is_ok_and(|target| target == new)
}

/// Returns the lock file of the marks file `file`, beside the file that its name leads to now,
/// through every link and linked folder (`real_dir`). So every window locks the one file: by
/// its new name, through the link the move left at the old one, or at the old name where a
/// move failed. Returns `None` when `file` has no name.
fn marks_lock(file: &Path) -> Option<PathBuf> {
    crate::files::file_io::lock_beside(&crate::model::listing::real_dir(file))
}

/// Returns that the marks file of Glimt 0.95 and older is left beside the marks file in its
/// place now, for `--check-config`, which moves nothing. Returns `None` when they are not both
/// there.
pub fn old_marks_file_left() -> Option<ShownText> {
    old_file_left_beside(&old_marks_file()?, &marks_file()?)
}

/// Returns the notice that the old marks file `old` is left as it is, because the new one `new`
/// is there too, or `None` when either is gone. A file that could not be looked at counts as
/// there, so that the old one is left alone. The link the move leaves at `old` is no file left
/// beside the new one.
fn old_file_left_beside(old: &Path, new: &Path) -> Option<ShownText> {
    use crate::files::file_io::is_surely_gone;
    (!is_surely_gone(old) && !is_surely_gone(new) && !is_one_file(old, new)).then(|| {
        crate::tf!(
            "The marks file of Glimt 0.95 or older is left at {old}, as {new} is there too. The marks of {new} are used: join the two by hand, or remove the old one.",
            old = escaped_name(old),
            new = escaped_name(new)
        )
    })
}

/// Moves `old`, a file of Glimt's own that no window writes meanwhile, to `new`, which is not
/// there, and makes the move lasting: a rename on one disk, and to another a copy that is on
/// disk with its name before the old file is removed. A failure leaves the file at `old`, or
/// under both names.
fn move_own_file(old: &Path, new: &Path) -> std::io::Result<()> {
    use crate::files::file_io::{put_file, remove_own, rename_never_over, sync_folder_of};
    #[cfg(test)]
    let renamed = match crate::files::file_io::MOVES_CROSS_DEVICES.get() {
        true => Err(crate::files::file_io::StepError::in_a_test(
            std::io::ErrorKind::CrossesDevices.into(),
            old,
            new,
        )),
        // Glimt's own: the marks file, under the locks of both places.
        false => rename_never_over(old, new),
    };
    // Glimt's own: the marks file, under the locks of both places.
    #[cfg(not(test))]
    let renamed = rename_never_over(old, new);
    match renamed {
        Err(e) if e.crosses_devices() => (put_file(old, new, Transfer::Copy))
            .map_err(std::io::Error::from)
            .and_then(|()| sync_folder_of(new))
            .and_then(|()| remove_own(old)),
        renamed => (renamed.map_err(std::io::Error::from)).and_then(|()| sync_folder_of(new)),
    }
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
        Edit::Follow(pairs, transfer, known_from, marks_when_begun) => {
            let (kept, refused): (Vec<_>, Vec<_>) =
                (pairs.into_iter()).partition(|(_, to)| fits_on_a_line(to));
            let left_out = (refused.iter())
                .filter(|(from, _)| map.contains_key(from))
                .count();
            let gone: Vec<PathBuf> = (refused.into_iter())
                .filter(|_| transfer == Transfer::Move)
                .map(|(from, _)| from)
                .collect();
            let edits = [
                (!kept.is_empty()).then_some(Edit::Follow(
                    kept,
                    transfer,
                    known_from,
                    marks_when_begun,
                )),
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
/// (`transfer`). All are taken before any is given, because in a Rename Many that swaps or shifts
/// names a `to` is another pair's `from`.
///
/// `marks_when_begun` holds the marks that the window saw at the new names of a Rename Many
/// when it began. Where a pair's new name holds other marks now, they were changed there while
/// the job ran, on the photo shown under that name. Those changes are the photo's: they are made
/// to the photo's own marks at the new name, and to the marks it kept at its old name when its
/// rename was undone (a pair from its new name, in `marks_when_begun`, to its old one). Where
/// the marks are as they were, they are an old mark that a file gone from the name left behind,
/// and never go on the photo: a rename gives the photo its own marks, as any move does, and an
/// undo leaves both names as they are. Before, a mark set during the job was lost (#311), and
/// then it took the place of the photo's own marks and stars.
fn follow_pairs(
    map: &mut HashMap<PathBuf, (u8, u8)>,
    pairs: &[(PathBuf, PathBuf)],
    transfer: Transfer,
    marks_when_begun: &MarksAtNewNames,
) {
    // The marks at the new name `name` when the job began and now, `None` when they are the
    // same or it is no new name of one.
    let set_meanwhile = |map: &HashMap<PathBuf, (u8, u8)>, name: &PathBuf| {
        let now = map.get(name).copied().unwrap_or_default();
        let then = *marks_when_begun.get(name)?;
        (now != then).then_some((then, now))
    };
    // Each pair, with the changes made meanwhile and, for an undo, the marks the photo kept at
    // its old name. Those are read before any marks are taken.
    let pairs: Vec<_> = (pairs.iter())
        .filter_map(|(from, to)| match marks_when_begun.contains_key(from) {
            // An undo: only a change made during the job goes back.
            true => Some((
                from,
                to,
                Some(set_meanwhile(map, from)?),
                Some(map.get(to).copied()),
            )),
            false => Some((from, to, set_meanwhile(map, to), None)),
        })
        .collect();
    let taken: Vec<Option<(u8, u8)>> = (pairs.iter())
        .map(|(from, ..)| match transfer {
            Transfer::Copy => map.get(*from).copied(),
            Transfer::Move => map.remove(*from),
        })
        .collect();
    for ((_, to, meanwhile, kept_at_old_name), marks) in pairs.into_iter().zip(taken) {
        // A rename gives the photo the marks taken from its old name.
        let own = kept_at_old_name.unwrap_or(marks);
        let new = match meanwhile {
            Some(changed) => with_changes_made_meanwhile(own, changed),
            None => own,
        };
        match new {
            Some(v) => drop(map.insert(to.clone(), v)),
            None => drop(map.remove(to)),
        }
    }
}

/// Returns the marks `own` of a photo with the changes made to it while a job ran, under a name
/// whose marks were `then` when the job began and are `now`. Each mark set or cleared meanwhile
/// is set or cleared, and stars given meanwhile take the place of the photo's own. Every other
/// mark and the stars stay the photo's own. Returns `None` for no marks and no stars.
///
/// `own` is `None` for a photo without marks: it is read from a map of marks, the marks file
/// read under its lock or this window's copy of it, where a name with neither marks nor stars
/// has no entry (`apply`). The values to put back, where a name without an entry means no
/// change, never come here (`ValuesToPutBack`).
fn with_changes_made_meanwhile(
    own: Option<(u8, u8)>,
    (then, now): ((u8, u8), (u8, u8)),
) -> Option<(u8, u8)> {
    let (bits, stars) = own.unwrap_or_default();
    let set = now.0 & !then.0;
    let cleared = then.0 & !now.0;
    let bits = (bits | set) & !cleared;
    let stars = if now.1 != then.1 { now.1 } else { stars };
    ((bits, stars) != (0, 0)).then_some((bits, stars))
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
        Edit::Follow(pairs, transfer, _, marks_when_begun) => {
            follow_pairs(map, pairs, *transfer, marks_when_begun)
        }
        Edit::FollowFolder(from, to) => {
            let pairs = [(from.clone(), to.clone())];
            let moved = crate::files::file_ops::moves_by_source(&pairs);
            let inside: Vec<(PathBuf, PathBuf)> = (map.keys())
                .filter_map(|k| {
                    let dir = crate::files::file_ops::new_place(k.parent()?, &moved)?;
                    Some((k.clone(), dir.join(k.file_name()?)))
                })
                .collect();
            follow_pairs(map, &inside, Transfer::Move, &MarksAtNewNames::new());
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
fn names_taken(edits: &[Edit]) -> (Vec<&ListedFile>, Option<ShownText>) {
    let files = edits.iter().flat_map(|edit| match edit {
        Edit::Change(files, _) => files.as_slice(),
        _ => &[],
    });
    let mut first = None;
    let taken: Vec<&ListedFile> = files
        .filter(|f| {
            f.check()
                .map_err(|e| {
                    first.get_or_insert(crate::tf!("{name}: {e}", name = escaped_name(f.path()), e))
                })
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
        write_to(marks_file(), take_queued_edits());
    });
}

/// Makes `edits` to the marks file `file`, in the writer's thread. Marks set where there is no
/// marks file, as neither $HOME nor $XDG_DATA_HOME is set, are told as not saved.
fn write_to(file: Option<PathBuf>, edits: Vec<Edit>) {
    // Only a change can add marks, so the other edits make no marks file where none is.
    let adds = edits.iter().any(|edit| matches!(edit, Edit::Change(..)));
    match file {
        Some(file) if adds || !crate::files::file_io::is_surely_gone(&file) => {
            write(&file, edits);
        }
        None if adds => crate::files::file_io::save_failed(crate::tf!(
            "Could not save {name}: {e}",
            name = crate::support::format::ShownText::untranslated("marks"),
            e = crate::support::lang::tr("no data folder ($HOME is not set)")
        )),
        _ => {}
    }
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
    // Each move not written before keeps its own kind. A job's move is not looked at now either,
    // so that after a failed write of a Move of 500,000 files each later write does not look at
    // every name again. A file that moved on since is followed by the later edit that says so.
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
        // A pair left out, or a change refused, was made in the window's marks, so the window
        // reads the file again and shows what it holds.
        let ours = known_stamps().get(file) == Some(&stamp) && !some_left_out && taken.is_empty();
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
    // Under the lock of the file that the name leads to (`marks_lock`). When it leads to another
    // file once the lock is had, as another window moved it meanwhile, the edit is made again
    // under the lock of that file.
    let real_file = || crate::model::listing::real_dir(file);
    let result = loop {
        let real = real_file();
        let edited = crate::files::file_io::edit_shared(&real, |bytes| match real_file() == real {
            true => {
                let (text, kept) = edit(bytes);
                (text, Some(kept))
            }
            false => (bytes.to_vec(), None),
        });
        if let Some(kept) = edited.transpose() {
            break kept;
        }
    };
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
            name = escaped_name(file),
            e
        ));
        // A file whose stamp changed was written after all (only its folder's sync failed), or
        // by another window, and the edits are not made twice.
        if file_stamp(file) == stamp_before {
            keep_unwritten(file, changes);
        }
    }
}

/// Leaves out of the moves in `changes` that were read from a record each file whose new name
/// holds nothing now, so that its marks stay where they are and never go to an empty name,
/// where a later file of that name would get them. A move that its own job just made is not
/// looked at, because its file was at the new name, and a Move of 500,000 files on a share
/// would otherwise wait for 500,000 looks here. A new name that a later edit names is kept,
/// as the file was there and went on from it. A name that cannot be looked at is kept, and
/// told (`is_followed_to`). It waits for the disk, so it runs in the writer's thread. Returns
/// whether a file was left out.
fn follow_only_to_files_there(changes: &mut [Edit]) -> bool {
    let mut named_later: std::collections::HashSet<PathBuf> = Default::default();
    let mut some_left_out = false;
    for edit in changes.iter_mut().rev() {
        if let Edit::Follow(pairs, _, MovesKnownFrom::ARecord, _) = edit {
            pairs.retain(|(_, to)| {
                let is_there = to.ancestors().any(|a| named_later.contains(a))
                    || crate::files::file_io::is_followed_to(to);
                some_left_out |= !is_there;
                is_there
            });
        }
        let names: Vec<&Path> = match edit {
            Edit::Change(files, _) => files.iter().map(ListedFile::path).collect(),
            Edit::Follow(pairs, ..) => pairs.iter().map(|(from, _)| from.as_path()).collect(),
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
/// changed. The stars are kept as they are too, also a value above 5 that a newer Glimt may
/// write, and they are shown as 5 at most (`Marks::get`). Returns `None` for a line that is not
/// of this form.
fn parse_line(line: &[u8]) -> Option<(PathBuf, (u8, u8))> {
    let mut parts = line.splitn(3, |&b| b == b'\t');
    let number = |b: &[u8]| std::str::from_utf8(b).ok()?.parse::<u8>().ok();
    let (bits, stars) = (number(parts.next()?)?, number(parts.next()?)?);
    // Any bytes are a name on Unix: those `save` wrote, or the user's own.
    let path = <std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(parts.next()?);
    Some((PathBuf::from(path), (bits, stars)))
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
pub fn write_xmp(path: &Path, stars: u8) -> Result<(), ShownText> {
    use crate::files::file_io::IfMissing;
    let if_missing = match stars {
        0 => IfMissing::LeaveMissing,
        _ => IfMissing::Create,
    };
    crate::metadata::xmp::update_sidecar(path, if_missing, |x| set_rating(x, stars))
}

/// `xmp` with its rating set to `stars`, as an attribute of its first rdf:Description. Returns
/// `None` if it has no rdf:Description to put it in.
fn set_rating(xmp: &Packet, stars: u8) -> Option<Packet> {
    // The rating it has goes first, under any prefix bound to XMP's namespace and in whatever
    // form and quotes it was written, so the sidecar never holds two, which darktable and
    // digiKam refuse as broken XML.
    let xmp = xmp.without_property("xmp:Rating");
    // It is written with the prefix that the sidecar binds to XMP's namespace, such as an old
    // packet's xap, so it is the same property as the rating there.
    let uri = "http://ns.adobe.com/xap/1.0/";
    xmp.with_attribute_added("xmp", uri, "Rating", &stars.to_string())
}

/// "★★★" for the stars and "1 4" for the marks, for the file list and overlay.
pub fn text((bits, stars): (u8, u8)) -> ShownText {
    let marks: Vec<ShownText> = (1..=6u8)
        .filter(|n| bits & (1 << (n - 1)) != 0)
        .map(ShownText::number)
        .collect();
    let star = ShownText::untranslated("★");
    let stars = ShownText::concat((0..stars).map(|_| &star));
    let marks = ShownText::join(&marks, &ShownText::untranslated(" "));
    match (marks.is_empty(), stars.is_empty()) {
        (true, _) => stars,
        (false, true) => marks,
        (false, false) => ShownText::join([&stars, &marks], &ShownText::untranslated("  ")),
    }
}

#[cfg(test)]
mod tests {
    use crate::support::threads::lock_even_if_poisoned;

    /// `super::set_rating` of the packet `xmp`, as text.
    fn set_rating(xmp: &str, stars: u8) -> Option<String> {
        let packet = crate::metadata::xmp_packet::Packet::for_test(xmp);
        super::set_rating(&packet, stars).map(|p| p.text_for_test().to_owned())
    }

    /// A mark set during Rename Many at the photo's new name is added to the photo's own marks
    /// and stars, at the new name and back at the old one when the rename is undone. A mark
    /// that the old file of the new name left behind stays off the photo. Before, the new
    /// name's whole value took the place of the photo's own, and its 3 stars were lost.
    /// Risk: the merge after Rename Many loses the photo's own stars or takes an old file's mark.
    #[test]
    fn a_mark_set_meanwhile_is_added_to_the_photos_own() {
        use std::path::PathBuf;
        let (a, c) = (PathBuf::from("/p/a.jpg"), PathBuf::from("/p/c.jpg"));
        // c.jpg held 4 stars of an older file when the job began, and mark 1 was set there.
        let begun = std::collections::HashMap::from([(c.clone(), (0, 4))]);
        let map = || std::collections::HashMap::from([(a.clone(), (0, 3)), (c.clone(), (1, 4))]);
        let mut renamed = map();
        super::follow_pairs(
            &mut renamed,
            &[(a.clone(), c.clone())],
            super::Transfer::Move,
            &begun,
        );
        assert_eq!(renamed.get(&c), Some(&(1, 3)));
        assert_eq!(renamed.get(&a), None);
        let mut undone = map();
        super::follow_pairs(
            &mut undone,
            &[(c.clone(), a.clone())],
            super::Transfer::Move,
            &begun,
        );
        assert_eq!(undone.get(&a), Some(&(1, 3)));
        assert_eq!(undone.get(&c), None);
        // Stars given meanwhile take the place of the photo's own, and a mark cleared goes.
        let changed = super::with_changes_made_meanwhile(Some((3, 3)), ((1, 4), (0, 5)));
        assert_eq!(changed, Some((2, 5)));
    }

    /// The marks file of Glimt 0.95 and older is moved to its new place when only it is there,
    /// also to another disk. When both are there, both are kept as they are and the user is
    /// told. With neither there is nothing to do. A move that fails keeps the old file where it
    /// is, which the window then uses, and is told.
    #[test]
    fn the_old_marks_file_is_moved_and_never_lost() {
        use super::{MarksMove, move_marks_file};
        let dir =
            std::env::temp_dir().join(format!("glimt-test-marks-move-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (old, new) = (dir.join("state/marks"), dir.join("data/glimt/marks"));
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        let line = "1\t3\t/photos/a.jpg\n";
        // Only the old one, on one disk and to another.
        for crosses_devices in [false, true] {
            std::fs::write(&old, line).unwrap();
            crate::files::file_io::MOVES_CROSS_DEVICES.set(crosses_devices);
            assert_eq!(move_marks_file(&old, &new), MarksMove::Done);
            crate::files::file_io::MOVES_CROSS_DEVICES.set(false);
            assert_eq!(std::fs::read_to_string(&new).unwrap(), line);
            assert_eq!(std::fs::read_link(&old).unwrap(), new);
            std::fs::remove_file(&old).unwrap();
            std::fs::remove_file(&new).unwrap();
        }
        // Both: neither is changed, and the notice names the old one.
        std::fs::write(&old, line).unwrap();
        std::fs::write(&new, "2\t0\t/photos/b.jpg\n").unwrap();
        let MarksMove::BothKept(text) = move_marks_file(&old, &new) else {
            panic!("not both kept");
        };
        assert!(text.contains(&old.display().to_string()), "{text}");
        assert_eq!(std::fs::read_to_string(&old).unwrap(), line);
        assert_eq!(
            std::fs::read_to_string(&new).unwrap(),
            "2\t0\t/photos/b.jpg\n"
        );
        // --check-config tells the same while both are there, and moves nothing.
        assert_eq!(super::old_file_left_beside(&old, &new), Some(text));
        assert!(old.exists() && new.exists());
        // Neither.
        std::fs::remove_file(&old).unwrap();
        assert_eq!(super::old_file_left_beside(&old, &new), None);
        std::fs::remove_file(&new).unwrap();
        assert_eq!(move_marks_file(&old, &new), MarksMove::Done);
        assert!(!old.exists() && !new.exists());
        // A move that fails, here as the new folder's place is a file.
        std::fs::write(&old, line).unwrap();
        std::fs::write(dir.join("blocked"), "").unwrap();
        let blocked = dir.join("blocked/glimt/marks");
        crate::files::file_io::take_saves_failed();
        assert_eq!(move_marks_file(&old, &blocked), MarksMove::Failed);
        assert_eq!(std::fs::read_to_string(&old).unwrap(), line);
        let told = crate::files::file_io::take_saves_failed();
        assert!(
            told.len() == 1 && told[0].starts_with("Could not move the marks file"),
            "{told:?}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// After the move, a window of Glimt 0.95 that is still open writes the one marks file
    /// through the link at the old name. A later start takes the link as moved, also while no
    /// file is at the new name, and the same marks under both names become the link.
    #[test]
    fn a_window_of_0_95_writes_the_moved_marks_file_through_the_link() {
        use super::{Edit, MarksMove, move_marks_file};
        let dir =
            std::env::temp_dir().join(format!("glimt-test-marks-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (old, new) = (dir.join("state/marks"), dir.join("data/glimt/marks"));
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        std::fs::write(&old, "1\t3\t/photos/a.jpg\n2\t0\t/photos/b.jpg\n").unwrap();
        assert_eq!(move_marks_file(&old, &new), MarksMove::Done);
        assert_eq!(std::fs::read_link(&old).unwrap(), new);
        // A delete in the 0.95 window, which makes no file where none is, lands in the file.
        super::write(&old, vec![Edit::Forget(vec!["/photos/a.jpg".into()], None)]);
        assert_eq!(
            std::fs::read_to_string(&new).unwrap(),
            "2\t0\t/photos/b.jpg\n"
        );
        assert_eq!(std::fs::read_link(&old).unwrap(), new);
        // A later start, also with no file at the new name.
        assert_eq!(move_marks_file(&old, &new), MarksMove::Done);
        std::fs::remove_file(&new).unwrap();
        assert_eq!(move_marks_file(&old, &new), MarksMove::Done);
        assert_eq!(std::fs::read_link(&old).unwrap(), new);
        assert!(crate::files::file_io::is_surely_gone(&new));
        // The same marks under both names, as a backup copied back leaves them.
        std::fs::write(&new, "2\t0\t/photos/b.jpg\n").unwrap();
        assert_eq!(move_marks_file(&old, &new), MarksMove::Done);
        assert_eq!(std::fs::read_link(&old).unwrap(), new);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// When $XDG_STATE_HOME and $XDG_DATA_HOME are one folder, or one is a link to the other,
    /// the old and the new marks file are one file with one lock. There is nothing to move, and
    /// the start does not wait for the lock that it holds itself.
    #[test]
    fn one_marks_file_under_both_names_is_not_moved() {
        use super::{MarksMove, move_marks_file};
        let dir = std::env::temp_dir().join(format!("glimt-test-marks-one-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let data = dir.join("data/glimt");
        std::fs::create_dir_all(&data).unwrap();
        std::os::unix::fs::symlink(&data, dir.join("state")).unwrap();
        let line = "1\t3\t/photos/a.jpg\n";
        let file = data.join("marks");
        std::fs::write(&file, line).unwrap();
        for old in [file.clone(), dir.join("state/marks")] {
            assert_eq!(move_marks_file(&old, &file), MarksMove::Done);
            assert_eq!(std::fs::read_to_string(&file).unwrap(), line);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The same marks under both names lose the old name only when both are surely two files.
    /// When a look at either fails, as on a share for a moment, they may be one file through a
    /// linked folder, whose one name would go. Both are kept, and the user is told.
    #[test]
    fn the_same_marks_under_both_names_are_kept_when_a_look_fails() {
        use super::{MarksMove, move_marks_file_by};
        let dir =
            std::env::temp_dir().join(format!("glimt-test-marks-both-same-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (old, new) = (dir.join("state/marks"), dir.join("data/glimt/marks"));
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        std::fs::create_dir_all(new.parent().unwrap()).unwrap();
        let line = "1\t3\t/photos/a.jpg\n";
        std::fs::write(&old, line).unwrap();
        std::fs::write(&new, line).unwrap();
        crate::files::file_io::take_saves_failed();
        let failing = |file: &std::path::Path| match file.starts_with(dir.join("state")) {
            true => Err("the share did not answer".into()),
            false => Ok(Ok((1, 2))),
        };
        assert_eq!(move_marks_file_by(&old, &new, failing), MarksMove::Failed);
        assert_eq!(std::fs::read_to_string(&old).unwrap(), line);
        assert_eq!(std::fs::read_to_string(&new).unwrap(), line);
        let told = crate::files::file_io::take_saves_failed();
        assert!(
            told.len() == 1 && told[0].contains("the share did not answer"),
            "{told:?}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// When $XDG_STATE_HOME is a link to $XDG_DATA_HOME and the marks file cannot be looked at
    /// (here a link to itself), the move cannot tell the two names are one file. It takes the
    /// one lock all the same and does not wait for itself, and it changes nothing.
    #[test]
    fn a_marks_file_that_cannot_be_looked_at_does_not_hang_the_move() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-marks-eloop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let data = dir.join("data/glimt");
        std::fs::create_dir_all(&data).unwrap();
        std::os::unix::fs::symlink(&data, dir.join("state")).unwrap();
        let file = data.join("marks");
        std::os::unix::fs::symlink("marks", &file).unwrap();
        let (old, new) = (dir.join("state/marks"), file.clone());
        let (sender, receiver) = std::sync::mpsc::channel();
        crate::support::threads::spawn_in_a_test(move || {
            sender.send(super::move_marks_file(&old, &new)).unwrap()
        });
        let moved = receiver.recv_timeout(std::time::Duration::from_secs(10));
        assert!(moved.is_ok(), "the move waits for its own lock");
        assert_eq!(
            std::fs::read_link(&file).unwrap(),
            std::path::Path::new("marks")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Every window locks the one marks file by the file its name leads to. A window whose move
    /// failed writes it by its old name, and when another window has moved it meanwhile, it
    /// writes through the link under the lock that the windows writing the new name take.
    #[test]
    fn every_window_locks_the_one_marks_file() {
        use crate::files::file_io::{lock_beside, open_and_lock};
        let dir =
            std::env::temp_dir().join(format!("glimt-test-marks-lock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (old, new) = (dir.join("state/marks"), dir.join("data/glimt/marks"));
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        std::fs::create_dir_all(new.parent().unwrap()).unwrap();
        std::fs::write(&old, "2\t0\t/photos/b.jpg\n").unwrap();
        let old_lock = open_and_lock(&lock_beside(&old).unwrap()).unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let written = old.clone();
        crate::support::threads::spawn_in_a_test(move || {
            super::write(
                &written,
                vec![super::Edit::Forget(vec!["/photos/b.jpg".into()], None)],
            );
            sender.send(()).unwrap();
        });
        let wait = std::time::Duration::from_millis(300);
        assert!(
            receiver.recv_timeout(wait).is_err(),
            "written under no lock"
        );
        // Another window moves the file while the write waits, and writes the new name.
        std::fs::rename(&old, &new).unwrap();
        std::os::unix::fs::symlink(&new, &old).unwrap();
        let new_lock = open_and_lock(&lock_beside(&new).unwrap()).unwrap();
        drop(old_lock);
        assert!(
            receiver.recv_timeout(wait).is_err(),
            "written under the old name's lock"
        );
        drop(new_lock);
        assert!(
            receiver
                .recv_timeout(std::time::Duration::from_secs(10))
                .is_ok()
        );
        assert_eq!(std::fs::read_to_string(&new).unwrap(), "");
        assert_eq!(std::fs::read_link(&old).unwrap(), new);
        std::fs::remove_dir_all(dir).unwrap();
    }

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

    /// A marks file that cannot be read is told, and the window keeps its marks and reads the
    /// file again at the next refresh. Before, the window took it for a file without marks and
    /// noted it as read, so it showed no marks until another window wrote the file, and a
    /// Lightroom import that keeps Glimt's stars rated over the stars that the file held.
    #[test]
    fn a_marks_file_that_cannot_be_read_is_told_and_read_again() {
        let id = std::process::id();
        let dir = std::env::temp_dir().join(format!("glimt-test-marks-unreadable-{id}"));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("marks");
        let a = dir.join("a.png");
        let line = format!("0\t3\t{}\n", a.display());
        std::fs::write(&file, &line).unwrap();
        super::TEST_FILE.with(|f| *f.borrow_mut() = Some(file.clone()));
        let mut marks = super::read_marks();
        assert_eq!(marks.get(&a), (0, 3));
        // A folder in the file's place cannot be read, as a share's EIO cannot. The file is
        // kept under another name, so that the folder cannot take its inode and look unchanged.
        std::fs::rename(&file, dir.join("marks-before")).unwrap();
        std::fs::create_dir(&file).unwrap();
        crate::files::file_io::take_saves_failed();
        refreshed(&mut marks);
        let told = crate::files::file_io::take_saves_failed();
        assert_eq!(marks.get(&a), (0, 3), "the window lost its marks");
        assert_eq!(told.len(), 1, "the failed read is not told: {told:?}");
        // Readable again, the file is read at the next refresh.
        std::fs::rename(&file, dir.join("folder-before")).unwrap();
        std::fs::write(&file, line.replace("0\t3", "0\t5")).unwrap();
        refreshed(&mut marks);
        assert_eq!(marks.get(&a), (0, 5), "the file is not read again");
        super::TEST_FILE.with(|f| *f.borrow_mut() = None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A marks file that could not be read for a moment, and then answers again unchanged, is
    /// read again. Before, the stamp of this window's own last write stayed known, so the
    /// refresh took the file as held and never read it, and the window showed the changes it
    /// had put back for the rest of the session.
    #[test]
    fn a_marks_file_that_answers_again_unchanged_is_read_again() {
        let id = std::process::id();
        let dir = std::env::temp_dir().join(format!("glimt-test-marks-answers-again-{id}"));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("marks");
        let a = dir.join("a.png");
        std::fs::write(&file, format!("0\t3\t{}\n", a.display())).unwrap();
        super::TEST_FILE.with(|f| *f.borrow_mut() = Some(file.clone()));
        let mut marks = super::read_marks();
        // The rating as `rate` makes it, without the read back it queues after the write, as
        // the test writes by hand.
        marks.change(
            &crate::support::test_support::unlisted(std::slice::from_ref(&a)),
            super::Change::Stars(4),
        );
        super::write(&file, super::SENT.with(|sent| sent.take()));
        // The share stops answering: the read fails, and the rating, not yet confirmed by a
        // read, is put back. The file keeps its inode and time.
        lock_even_if_poisoned(&super::UNREADABLE_IN_TEST).push(file.clone());
        crate::files::file_io::take_saves_failed();
        marks.read_again();
        settled(&mut marks);
        crate::files::file_io::take_saves_failed();
        assert_eq!(marks.get(&a), (0, 3));
        // The share answers again, and the file holds the rating.
        lock_even_if_poisoned(&super::UNREADABLE_IN_TEST).retain(|f| *f != file);
        refreshed(&mut marks);
        assert_eq!(marks.get(&a), (0, 4), "the file is not read again");
        super::TEST_FILE.with(|f| *f.borrow_mut() = None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The changes put back when the marks file cannot be read follow the files that a delete
    /// or a move changed since, so each is put back on the file it was about. Before, they were
    /// put back by name: a file renamed to the name of a deleted one got that one's stars, and a
    /// file moved kept stars that its write may not have saved. A Rename Many moves them as any
    /// move does: a name without a change to put back holds no marks of its own there. Before,
    /// such a name was read as a photo's marks cleared during the job, and its change was lost.
    #[test]
    fn changes_put_back_follow_their_files() {
        let id = std::process::id();
        let dir = std::env::temp_dir().join(format!("glimt-test-marks-put-back-follow-{id}"));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("marks");
        let [a, b, c, d] = ["a.png", "b.png", "c.png", "d.png"].map(|n| dir.join(n));
        let [e, f] = ["e.png", "f.png"].map(|n| dir.join(n));
        let text = format!(
            "0\t2\t{}\n0\t1\t{}\n0\t1\t{}\n1\t4\t{}\n",
            a.display(),
            c.display(),
            e.display(),
            f.display()
        );
        std::fs::write(&file, text).unwrap();
        super::TEST_FILE.with(|f| *f.borrow_mut() = Some(file.clone()));
        let mut marks = super::read_marks();
        let unlisted = crate::support::test_support::unlisted;
        // a.png is rated, deleted, and b.png, without stars, is renamed to its name.
        marks.rate(&unlisted(std::slice::from_ref(&a)), 3);
        marks.forget(std::slice::from_ref(&a), None);
        marks.follow_files(
            vec![(b.clone(), a.clone())],
            super::Transfer::Move,
            super::MovesKnownFrom::TheJob,
        );
        // c.png is rated and moved to d.png.
        marks.rate(&unlisted(std::slice::from_ref(&c)), 4);
        marks.follow_files(
            vec![(c.clone(), d.clone())],
            super::Transfer::Move,
            super::MovesKnownFrom::TheJob,
        );
        // e.png is rated and renamed by Rename Many to f.png, which held an old mark.
        marks.rate(&unlisted(std::slice::from_ref(&e)), 5);
        let begun = std::collections::HashMap::from([(f.clone(), (1, 4))]);
        marks.follow_renames(
            vec![(e.clone(), f.clone())],
            super::MovesKnownFrom::TheJob,
            begun,
        );
        // None of it is written, and the file cannot be read. The reads that the moves asked
        // for are taken first, as the read below would otherwise wait for them.
        super::SENT.with(|sent| sent.take());
        settled(&mut marks);
        lock_even_if_poisoned(&super::UNREADABLE_IN_TEST).push(file.clone());
        crate::files::file_io::take_saves_failed();
        marks.read_again();
        settled(&mut marks);
        crate::files::file_io::take_saves_failed();
        lock_even_if_poisoned(&super::UNREADABLE_IN_TEST).retain(|f| *f != file);
        let shown = [&a, &c, &d, &f].map(|p| marks.get(p));
        assert_eq!(
            shown,
            [(0, 0), (0, 0), (0, 1), (0, 1)],
            "put back on other files"
        );
        super::TEST_FILE.with(|f| *f.borrow_mut() = None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Select Mark selects the files that another window marked meanwhile too, once the marks
    /// file read again has come back. Before, it selected by the marks held before the read.
    #[test]
    fn select_mark_counts_the_marks_read_again() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-marks-select-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("marks");
        super::TEST_FILE.with(|f| *f.borrow_mut() = Some(file.clone()));
        let (mut app, ctx) = crate::support::test_support::app_on(&dir);
        // Another window marks b.jpg after this one read the marks.
        std::fs::write(&file, format!("1\t0\t{}\n", dir.join("b.jpg").display())).unwrap();
        app.list
            .set_files(["a.jpg", "b.jpg"].iter().map(Into::into).collect());
        app.run(&ctx, crate::ui::actions::Action::SelectMark1);
        crate::files::file_io::flush();
        app.receive_answers(&ctx);
        assert_eq!(app.list.selected(), &[1].into());
        // A selection the user changed before the read came back is left as it is.
        std::fs::write(&file, format!("1\t0\t{}\n", dir.join("a.jpg").display())).unwrap();
        app.run(&ctx, crate::ui::actions::Action::SelectMark1);
        app.list.select([0, 1].into());
        crate::files::file_io::flush();
        app.receive_answers(&ctx);
        assert_eq!(app.list.selected(), &[0, 1].into());
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
        marks.follow_files(
            vec![(a.clone(), odd.join("a.png"))],
            super::Transfer::Move,
            super::MovesKnownFrom::TheJob,
        );
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
        // The file's own writer leaves such a name out too.
        let kept =
            std::collections::HashMap::from([(odd.join("d.png"), (0, 3)), (a.clone(), (0, 1))]);
        assert_eq!(
            super::to_text(&kept),
            format!("0\t1\t{}\n", a.display()).into_bytes()
        );
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
        // This window read the marks before the other one wrote these.
        let mut marks = super::Marks::default();
        let text = format!("0\t3\t{}\n0\t2\t{}\n", a.display(), b.display());
        std::fs::write(&file, text).unwrap();
        marks.follow_files(
            vec![(a.clone(), c.clone())],
            super::Transfer::Move,
            super::MovesKnownFrom::TheJob,
        );
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
        // The window takes the file as known, as when its refresh is skipped.
        super::known_stamps().insert(file.clone(), super::file_stamp(&file));
        let mut marks = super::Marks::default();
        super::SENT.with(|sent| sent.borrow_mut().clear());
        marks.follow_files(
            vec![(a.clone(), c.clone())],
            super::Transfer::Move,
            super::MovesKnownFrom::TheJob,
        );
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

    /// A mark or stars refused for an entry that no listing has checked do not stay shown: the
    /// writer does not take the file as held after the refusal, and a rate asks for the read
    /// back as a toggle does. Risk: the window showing, and Select Mark selecting, a mark the
    /// file does not have, for good when the listing then fails.
    #[test]
    fn a_mark_refused_for_an_unchecked_entry_is_not_shown() {
        let dir = std::env::temp_dir().join(format!("glimt-test-marks-ref-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("marks");
        super::TEST_FILE.with(|f| *f.borrow_mut() = Some(file.clone()));
        let (a, b) = (dir.join("a.png"), dir.join("b.png"));
        // A file with a mark already, so the refused write leaves it as it is.
        std::fs::write(&file, format!("0\t2\t{}\n", b.display())).unwrap();
        let entry = [crate::files::file_io::ListedFile::unchecked_entry(
            a.clone(),
            &Default::default(),
        )];
        let mut marks = super::read_marks();
        // The changes as `rate` and `toggle` make them, without the read back they queue after
        // the write, as the test writes by hand.
        marks.change(&entry, super::Change::Stars(3));
        marks.change(&entry, super::Change::Toggle(1));
        assert_eq!(marks.get(&a), (1, 3));
        crate::files::file_io::take_saves_failed();
        super::write(&file, super::SENT.with(|sent| sent.take()));
        assert_eq!(crate::files::file_io::take_saves_failed().len(), 1);
        refreshed(&mut marks);
        assert_eq!((marks.get(&a), marks.get(&b)), ((0, 0), (0, 2)));
        marks.rate(&entry, 3);
        assert!(marks.is_reading(), "a rate reads the file back");
        settled(&mut marks);
        super::SENT.with(|sent| sent.take());
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
        let other = crate::support::threads::spawn_in_a_test({
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
        super::write(
            &file,
            vec![super::Edit::Follow(
                swap,
                super::Transfer::Move,
                super::MovesKnownFrom::TheJob,
                Default::default(),
            )],
        );
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

    /// Stars above 5, which a later Glimt may write in the marks file's frozen
    /// `bits\tstars\tpath` lines, are kept through a read and a write, and shown as 5. Before,
    /// the first write of this Glimt made them 5 in the file.
    #[test]
    fn keeps_stars_above_five() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-marks-stars7-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("marks");
        super::TEST_FILE.with(|f| *f.borrow_mut() = Some(file.clone()));
        std::fs::write(&file, "0\t7\t/a.jpg\n2\t9\t/b.jpg\n").unwrap();
        let (a, b) = (
            std::path::PathBuf::from("/a.jpg"),
            std::path::PathBuf::from("/b.jpg"),
        );
        let marks = super::read_marks();
        assert_eq!(marks.get(&a), (0, 5));
        assert!(marks.marked().all(|(_, (_, stars))| stars <= 5));
        // A mark set on b.jpg keeps its stars; stars set elsewhere leave a.jpg's alone.
        let mark = super::Edit::Change(
            crate::support::test_support::unlisted(vec![b.clone()]),
            super::Change::Mark(1, true),
        );
        let stars = super::Edit::Change(
            crate::support::test_support::unlisted(["/c.jpg"]),
            super::Change::Stars(2),
        );
        super::write(&file, vec![mark, stars]);
        let text = std::fs::read_to_string(&file).unwrap();
        for line in ["0\t7\t/a.jpg\n", "3\t9\t/b.jpg\n", "0\t2\t/c.jpg\n"] {
            assert!(text.contains(line), "{line:?} not in {text:?}");
        }
        super::TEST_FILE.with(|f| *f.borrow_mut() = None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A mark set where there is no marks file, as neither $HOME nor $XDG_DATA_HOME is set,
    /// is told as not saved. Before, it was dropped without a word.
    #[test]
    fn marks_without_a_marks_file_are_told() {
        crate::files::file_io::take_saves_failed();
        let stars = super::Edit::Change(
            crate::support::test_support::unlisted(["/c.jpg"]),
            super::Change::Stars(2),
        );
        super::write_to(None, vec![stars]);
        let told = crate::files::file_io::take_saves_failed();
        assert!(told.iter().any(|e| e.contains("marks")), "{told:?}");
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
        // A move that its job just made, as a Move of 500,000 unmarked files, so that the pair
        // reaches `follow_pairs` and is not left out first by a look at its new name.
        let pairs = vec![("/b.jpg".into(), "/c.jpg".into())];
        super::write(
            &file,
            vec![super::Edit::Follow(
                pairs,
                super::Transfer::Move,
                super::MovesKnownFrom::TheJob,
                Default::default(),
            )],
        );
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
        // A name on a share that does not answer was there when the record was made, so the
        // marks follow the file there, and that is told.
        std::fs::write(p("unanswered.jpg"), "d").unwrap();
        crate::support::threads::lock_even_if_poisoned(&crate::files::file_io::LOOKS_FAIL)
            .push(p("unanswered.jpg"));
        let line = |stars: u8, n: &str| format!("0\t{stars}\t{}\n", p(n).display());
        let text = [
            line(1, "a.jpg"),
            line(2, "b.jpg"),
            line(3, "c.jpg"),
            line(4, "d.jpg"),
            line(5, "e.jpg"),
        ];
        std::fs::write(&file, text.concat()).unwrap();
        let moves = vec![
            (p("a.jpg"), p("there.jpg")),
            (p("b.jpg"), p("gone.jpg")),
            (p("c.jpg"), p("middle.jpg")),
            (p("d.jpg"), p("unanswered.jpg")),
        ];
        let on = vec![(p("middle.jpg"), p("last.jpg"))];
        // A move that its own job just made is not looked at: its file was there.
        let made = vec![(p("e.jpg"), p("made.jpg"))];
        let edits = vec![
            super::Edit::Follow(
                moves,
                super::Transfer::Move,
                super::MovesKnownFrom::ARecord,
                Default::default(),
            ),
            super::Edit::Follow(
                on,
                super::Transfer::Move,
                super::MovesKnownFrom::TheJob,
                Default::default(),
            ),
            super::Edit::Follow(
                made,
                super::Transfer::Move,
                super::MovesKnownFrom::TheJob,
                Default::default(),
            ),
        ];
        super::write(&file, edits);
        let written = super::parse(&std::fs::read(&file).unwrap());
        assert_eq!(written.get(&p("there.jpg")), Some(&(0, 1)));
        assert_eq!(written.get(&p("b.jpg")), Some(&(0, 2)));
        assert_eq!(written.get(&p("gone.jpg")), None);
        assert_eq!(written.get(&p("last.jpg")), Some(&(0, 3)));
        assert_eq!(written.get(&p("d.jpg")), None);
        assert_eq!(written.get(&p("unanswered.jpg")), Some(&(0, 4)));
        assert_eq!(written.get(&p("made.jpg")), Some(&(0, 5)));
        crate::support::threads::lock_even_if_poisoned(&crate::files::file_io::LOOKS_FAIL)
            .retain(|n| *n != p("unanswered.jpg"));
        let told = crate::files::file_io::take_saves_failed();
        assert!(
            told.iter().any(|t| t.contains("unanswered.jpg")),
            "{told:?}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A swap read from a record, of a with b, whose look at one new name fails, still swaps both
    /// photos' marks. Before, the pair whose look failed was left out and the other was made,
    /// so a got b's marks, and a's own were lost.
    #[test]
    fn a_swap_with_a_failed_look_keeps_both_photos_marks() {
        use crate::files::file_io::LOOKS_FAIL;
        use crate::support::threads::lock_even_if_poisoned;
        let dir =
            std::env::temp_dir().join(format!("glimt-test-marks-swap-eio-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (file, p) = (dir.join("marks"), |n: &str| dir.join(n));
        std::fs::write(p("a.jpg"), "b").unwrap();
        std::fs::write(p("b.jpg"), "a").unwrap();
        let line = |stars: u8, n: &str| format!("0\t{stars}\t{}\n", p(n).display());
        std::fs::write(&file, line(1, "a.jpg") + &line(2, "b.jpg")).unwrap();
        lock_even_if_poisoned(&LOOKS_FAIL).push(p("b.jpg"));
        let swap = vec![(p("a.jpg"), p("b.jpg")), (p("b.jpg"), p("a.jpg"))];
        super::write(
            &file,
            vec![super::Edit::Follow(
                swap,
                super::Transfer::Move,
                super::MovesKnownFrom::ARecord,
                Default::default(),
            )],
        );
        lock_even_if_poisoned(&LOOKS_FAIL).retain(|n| *n != p("b.jpg"));
        let written = super::parse(&std::fs::read(&file).unwrap());
        assert_eq!(written.get(&p("a.jpg")), Some(&(0, 2)));
        assert_eq!(written.get(&p("b.jpg")), Some(&(0, 1)));
        let _ = crate::files::file_io::take_saves_failed();
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A job's move whose write failed is made at the next write as a job's move still, with
    /// no look at its new name. Before, it was made as one read from a record, so after a
    /// failed write of a Move of 500,000 files each later write looked at every name again.
    /// A new name with nothing there shows which kind it was made as.
    #[test]
    fn a_jobs_move_not_written_stays_a_jobs_move() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-marks-unwritten-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (file, p) = (dir.join("marks"), |n: &str| dir.join(n));
        std::fs::write(&file, format!("0\t3\t{}\n", p("a.jpg").display())).unwrap();
        let moved = vec![(p("a.jpg"), p("b.jpg"))];
        super::keep_unwritten(
            &file,
            vec![super::Edit::Follow(
                moved,
                super::Transfer::Move,
                super::MovesKnownFrom::TheJob,
                Default::default(),
            )],
        );
        super::write(&file, Vec::new());
        let written = super::parse(&std::fs::read(&file).unwrap());
        assert_eq!(written.get(&p("b.jpg")), Some(&(0, 3)));
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
            let _ = set_rating(&String::from_utf8_lossy(&bytes), 4);
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
        // Stars above 5 are read as they are, for a write to keep (`keeps_stars_above_five`).
        assert_eq!(
            super::parse_line(b"5\t9\t/x y.jpg"),
            Some((PathBuf::from("/x y.jpg"), (5, 9)))
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
            set_rating(dt, 4).unwrap(),
            r#"<rdf:Description xmp:Rating="4" rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" darktable:x="y">"#
        );
        // A rating in single quotes, as exiftool may write it, is replaced, not written twice,
        // which would make the XML broken.
        let quoted = r#"<rdf:Description xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating='3'>"#;
        assert_eq!(
            set_rating(quoted, 4).unwrap(),
            r#"<rdf:Description xmp:Rating="4" xmlns:xmp="http://ns.adobe.com/xap/1.0/">"#
        );
        let none = r#"<rdf:Description rdf:about="">"#;
        assert_eq!(
            set_rating(none, 2).unwrap(),
            r#"<rdf:Description xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="2" rdf:about="">"#
        );
        assert_eq!(
            set_rating(
                "<rdf:Description>\n <xmp:Rating>5</xmp:Rating></rdf:Description>",
                0
            )
            .unwrap(),
            r#"<rdf:Description xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="0"></rdf:Description>"#
        );
        // An old packet's xap prefix and a declaration with spaces around its equals sign are
        // read as the reader reads them, so the rating is neither written twice nor declared
        // twice.
        let xap = r#"<rdf:Description xmlns:xap = "http://ns.adobe.com/xap/1.0/" xap:Rating="3">"#;
        assert_eq!(
            set_rating(xap, 4).unwrap(),
            r#"<rdf:Description xap:Rating="4" xmlns:xap = "http://ns.adobe.com/xap/1.0/">"#
        );
        let spaced = r#"<rdf:Description xmlns:xmp = "http://ns.adobe.com/xap/1.0/">"#;
        assert_eq!(set_rating(spaced, 1).unwrap().matches("xmlns:").count(), 1);
        assert_eq!(set_rating("nothing", 1), None);
        // A rating under another prefix in a later Description, as exiftool writes one
        // Description for each namespace, is replaced, not left beside the new one.
        let later = r#"<rdf:Description rdf:about=""/><rdf:Description rdf:about="" xmlns:ns2="http://ns.adobe.com/xap/1.0/" ns2:Rating="2"/>"#;
        assert_eq!(set_rating(later, 4).unwrap().matches("Rating=").count(), 1);
        // A Description in a comment is not the one the rating goes into, and a CDATA section
        // with a quote does not hide the rating after it. Before, the rating went into the
        // comment, and the one after the CDATA section stayed beside it.
        let title = "<dc:title><rdf:Alt><rdf:li><![CDATA[It's]]></rdf:li></rdf:Alt></dc:title>";
        let commented = format!(
            "<!-- <rdf:Description xmp:Rating='1'> --><rdf:Description rdf:about=\"\">\
             {title}<xmp:Rating>3</xmp:Rating></rdf:Description>"
        );
        assert_eq!(
            set_rating(&commented, 4).unwrap(),
            format!(
                "<!-- <rdf:Description xmp:Rating='1'> --><rdf:Description \
                 xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\" xmp:Rating=\"4\" rdf:about=\"\">\
                 {title}</rdf:Description>"
            )
        );
        // A declaration of the prefix in a comment, a CDATA section or a processing
        // instruction is none, so the rating's Description declares it. Before, the rating was
        // written with its prefix bound to nothing, and darktable and digiKam refused the
        // whole sidecar.
        for hidden in [
            "<!-- <rdf:Description xmlns:xmp='http://ns.adobe.com/xap/1.0/' xmp:Rating='1'/> -->",
            "<?note xmlns:xmp='http://ns.adobe.com/xap/1.0/'?>",
            "<![CDATA[ xmlns:xmp='http://ns.adobe.com/xap/1.0/' ]]>",
        ] {
            let xmp = format!("<rdf:RDF>{hidden}<rdf:Description rdf:about=\"\"/></rdf:RDF>");
            assert_eq!(
                set_rating(&xmp, 4).unwrap(),
                format!(
                    "<rdf:RDF>{hidden}<rdf:Description xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\" \
                     xmp:Rating=\"4\" rdf:about=\"\"/></rdf:RDF>"
                )
            );
        }
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
        m.follow_files(
            vec![(p("/a.jpg"), p("/to/a.jpg"))],
            super::Transfer::Move,
            super::MovesKnownFrom::TheJob,
        );
        assert_eq!(
            (m.get(&p("/a.jpg")), m.get(&p("/to/a.jpg"))),
            ((0, 0), (1, 4))
        );
        m.follow_files(
            vec![(p("/b.jpg"), p("/b copy.jpg"))],
            super::Transfer::Copy,
            super::MovesKnownFrom::TheJob,
        );
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
        m.follow_files(
            vec![(p("/c.jpg"), p("/b copy.jpg"))],
            super::Transfer::Move,
            super::MovesKnownFrom::TheJob,
        );
        assert_eq!(m.get(&p("/b copy.jpg")), (0, 0));
        // Rename Many swapping names, and shifting them: each keeps its own.
        m.rate(&crate::support::test_support::unlisted(&[p("/x.jpg")]), 1);
        m.rate(&crate::support::test_support::unlisted(&[p("/y.jpg")]), 2);
        m.follow_files(
            vec![(p("/x.jpg"), p("/y.jpg")), (p("/y.jpg"), p("/x.jpg"))],
            super::Transfer::Move,
            super::MovesKnownFrom::TheJob,
        );
        assert_eq!((m.get(&p("/x.jpg")).1, m.get(&p("/y.jpg")).1), (2, 1));
        m.follow_files(
            vec![(p("/x.jpg"), p("/z.jpg")), (p("/y.jpg"), p("/x.jpg"))],
            super::Transfer::Move,
            super::MovesKnownFrom::TheJob,
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
        type Write = fn(&std::path::Path) -> Result<(), crate::support::format::ShownText>;
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
                    let text = xmp::AltTextChange {
                        text: Some("A dog".into()),
                        extended_description: None,
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
