//! Rename Many's steps on disk while they run. A crash or a power cut halfway leaves files under
//! their new names, their old ones, or hidden temporary ones. The next start finds the plan and
//! puts the old names back or finishes. The plan is locked while it runs, so a plan that another
//! window is still carrying out is left alone.

use crate::images::save::{same_file, same_file_by_opening};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// The first part of a plan that counts the renames done.
const COUNTED: &[u8] = b"counted";

/// The renames in the order they are done, as (from, to).
pub type Moves = Vec<(PathBuf, PathBuf)>;

/// A plan left by a Rename Many that was cut short. It holds its file, the renames, and how
/// many of them were done, which is `None` for a plan of Glimt 0.34 or older, because those did
/// not count them.
pub type CutShort = (PathBuf, Moves, Option<usize>);

/// The plan of a Rename Many being carried out. It is removed when dropped, as the renames are
/// done or undone, and it is left on disk only when Glimt stops halfway.
pub struct Journal {
    path: PathBuf,
    file: File,
    /// One name in each folder that the renames touch. Those folders are synced before the
    /// plan is removed, so that the plan never goes before the renames are on disk.
    one_name_per_folder: Vec<PathBuf>,
}

impl Journal {
    /// Counts one more rename done, with a NUL byte after the plan, so that the next start knows
    /// which were done. The file names cannot tell, because a swap has the same names before
    /// and after.
    // It is not synced, because a crash of Glimt does not need that. A power cut may lose the
    // last steps' bytes, and `repair_cut_short` finds them again by the names (`done_by_names`).
    pub fn count_step_done(&mut self) {
        let _ = self.file.write_all(&[0]);
    }

    /// Counts the step that puts the last file of a swap under its hidden name, with the disk
    /// synced before and after. Once the swap is done, the names on disk are those of the start
    /// again, and the plan could not tell a power cut after the swap from one before it. The
    /// names reach the disk before the count does, so a count of half is never there without
    /// them, and a count below half tells a swap not begun (`done_by_names`). Returns an error,
    /// with nothing counted, when the disk says that the names did not get there, so that the
    /// caller undoes the swap (a USB stick pulled out during the sync).
    pub fn count_middle_of_swap_done(&mut self) -> std::io::Result<()> {
        sync_folders(&self.one_name_per_folder).map_err(|(_, e)| e)?;
        self.count_step_done();
        self.sync();
        Ok(())
    }

    /// Counts one more rename done, through `count_middle_of_swap_done` when `is_middle_of_swap`
    /// and otherwise `count_step_done`. Returns an error when the middle did not reach the disk.
    pub fn count_rename_done(&mut self, is_middle_of_swap: bool) -> std::io::Result<()> {
        if is_middle_of_swap {
            return self.count_middle_of_swap_done();
        }
        self.count_step_done();
        Ok(())
    }

    /// The steps done and counted so far on the disk, past a power cut: the renames' folders
    /// first, so that the count never says more than the names on disk show.
    pub fn sync(&mut self) {
        if sync_folders(&self.one_name_per_folder).is_ok() {
            // A count that does not reach the disk costs nothing: `repair_cut_short` finds the
            // steps done by the names (`done_by_names`).
            let _ = self.file.sync_data();
        }
    }
}

impl Journal {
    /// Leaves the plan on disk, because the renames could not all be undone, so the next start
    /// offers to put the files right. Its lock goes with this Glimt.
    pub fn keep_on_disk(mut self) {
        // With an empty path, the remove in `drop` finds nothing.
        self.path = PathBuf::new();
    }
}

impl Drop for Journal {
    fn drop(&mut self) {
        // A plan kept on disk (`keep_on_disk`) has no path.
        if !self.path.as_os_str().is_empty() {
            remove_plan(&self.path, &self.one_name_per_folder);
        }
    }
}

pub fn journal_dir() -> Option<PathBuf> {
    crate::files::config::xdg_dir("XDG_STATE_HOME", ".local/state", "glimt")
        .map(|d| d.join("renames"))
}

/// Writes `moves` down in `dir`, locked and on disk with its name, before the first of them is
/// done. Returns `None` if that fails, and the renames then run without a plan. The user is told
/// so, because a crash halfway is then not put right at the next start.
pub fn start_journal(dir: &Path, moves: &[(PathBuf, PathBuf)]) -> Option<Journal> {
    let started = write_plan(dir, moves);
    if let Err(e) = &started {
        crate::files::file_io::save_failed(crate::tf!(
            "Rename Many runs without its plan ({e}), so a crash halfway is not put right at the next start",
            e
        ));
    }
    started.ok()
}

/// Writes the plan of `moves` in `dir` for `start_journal`, and returns it.
fn write_plan(dir: &Path, moves: &[(PathBuf, PathBuf)]) -> std::io::Result<Journal> {
    std::fs::create_dir_all(dir)?;
    // The plan gets a name of its own, never one that is there, so a plan left by a crash, of a
    // Glimt that had the same process ID before a restart, is not written over and lost.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let path = dir.join(format!(
        "{}-{nanos}",
        crate::features::remote::instance_id()
    ));
    #[allow(clippy::disallowed_methods)] // a plan made new, in Glimt's own state folder
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    // The plan is removed when dropped, also when what follows fails (a full disk). A plan left
    // half written would be offered at the next start, and finishing it would undo renames done
    // without it.
    let one_name_per_folder = one_name_per_folder(moves);
    let mut plan = Journal {
        path,
        file,
        one_name_per_folder,
    };
    plan.file.try_lock().map_err(std::io::Error::from)?;
    let mut bytes = [COUNTED, b"\0"].concat();
    for (a, b) in moves {
        for p in [a, b] {
            bytes.extend(p.as_os_str().as_bytes());
            bytes.push(0);
        }
    }
    // The plan's name is on disk too, and the folder's own on a first Rename Many, which made it.
    (plan.file.write_all(&bytes))
        .and_then(|_| plan.file.sync_all())
        .and_then(|_| crate::files::file_io::sync_folder_of(&plan.path))
        .and_then(|_| crate::files::file_io::sync_folder_of(dir))?;
    Ok(plan)
}

/// Takes `file`'s lock if no other window holds it, and returns whether it got it. The lock is
/// kept for as long as `file` stays open, which is what keeps a second window from working on
/// the same plan, so a caller keeps `file` open while it works. It is tried for a moment,
/// because a program that another thread starts holds a copy of the descriptor, and its lock,
/// until it runs.
fn take_lock_if_free(file: &File) -> bool {
    (0..10).any(|k| {
        if k > 0 {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        file.try_lock().is_ok()
    })
}

/// The plans in `dir` of renames that were cut short, which are those no running window holds
/// locked.
pub fn cut_short_plans(dir: &Path) -> Vec<CutShort> {
    let entries = std::fs::read_dir(dir).into_iter().flatten().flatten();
    entries
        .filter_map(|e| {
            // A pipe or a link planted in the state folder is no plan, and opening a pipe would
            // hold the start for good. It reads no more than a decode would.
            e.file_type().ok().filter(|t| t.is_file())?;
            // A plan another window was finishing is skipped. That window deletes the plan
            // before it lets go of the lock, so a file opened just before then is no longer linked.
            let file = crate::images::decode::open_regular(e.path())
                .ok()
                .filter(take_lock_if_free)
                .filter(is_linked)?;
            let mut bytes = Vec::new();
            #[allow(clippy::disallowed_methods)] // bounded by take, and only a file
            let read = (&file)
                .take(crate::images::decode::MAX_DECODE_BYTES)
                .read_to_end(&mut bytes);
            read.ok()?;
            let (moves, done) = parse(&bytes)?;
            Some((e.path(), moves, done))
        })
        .collect()
}

/// A plan's renames and how many were done. Returns `None` for one with no paths.
fn parse(bytes: &[u8]) -> Option<(Moves, Option<usize>)> {
    let mut parts: Vec<&[u8]> = bytes.split(|&b| b == 0).collect();
    let counted = parts.first() == Some(&COUNTED);
    if counted {
        parts.remove(0);
    }
    // After the paths, each ending in a NUL, a NUL for each rename done.
    let done = parts
        .iter()
        .filter(|p| p.is_empty())
        .count()
        .saturating_sub(1);
    let paths: Vec<PathBuf> = (parts.iter())
        .filter(|p| !p.is_empty())
        .map(|p| PathBuf::from(OsStr::from_bytes(p)))
        .collect();
    // A plan made a moment ago and not yet locked or written is not one left by a crash.
    if paths.is_empty() {
        return None;
    }
    let moves = (paths.as_chunks::<2>().0.iter()).map(|[a, b]| (a.clone(), b.clone()));
    let moves: Moves = moves.collect();
    let done = counted.then(|| done.min(moves.len()));
    Some((moves, done))
}

/// Does one step of a plan, `a` to `b`, never over another file (`put_file`). A step cut short
/// between its link and its unlink, where both names are the file, is finished by taking the
/// old name away. This is the one way a step is done, whether as Rename Many runs, as it is
/// undone, or as it is put right at the next start.
pub fn rename_step(a: &Path, b: &Path) -> std::io::Result<()> {
    if same_file_by_opening(a, b) {
        #[allow(clippy::disallowed_methods)] // a second name of the same file
        return std::fs::remove_file(a);
    }
    crate::files::file_io::put_file(a, b, false)
}

/// Undoes the first `done` steps of `moves`, last first. Returns (renamed, left). A name that is
/// no longer there is nothing of ours to put back, and it is left out of both.
pub fn undo(moves: &[(PathBuf, PathBuf)], done: usize) -> (usize, usize) {
    let back = moves[..done.min(moves.len())].iter().rev();
    do_steps(back.map(|(a, b)| (b.as_path(), a.as_path())))
}

/// Does the steps of `moves` after the first `done`. Returns (renamed, left). At the first step
/// that fails, it stops and undoes the steps it did, so the names on disk stay those of the
/// count that the plan keeps. Gone on, it left files under hidden names that no count
/// described, and putting the old names back later missed them.
pub fn finish(moves: &[(PathBuf, PathBuf)], done: usize) -> (usize, usize) {
    let mut done_here: Vec<(&Path, &Path)> = Vec::new();
    for (a, b) in &moves[done.min(moves.len())..] {
        match repair_step(a, b) {
            Some(true) => done_here.push((b, a)),
            Some(false) => {
                let (_, stuck) = do_steps(done_here.into_iter().rev());
                return (0, 1 + stuck);
            }
            None => {}
        }
    }
    (done_here.len(), 0)
}

/// Does each of `steps`, as `repair_step` does. Returns (renamed, left).
fn do_steps<'a>(steps: impl Iterator<Item = (&'a Path, &'a Path)>) -> (usize, usize) {
    let (mut renamed, mut left) = (0, 0);
    for (a, b) in steps {
        match repair_step(a, b) {
            Some(true) => renamed += 1,
            Some(false) => left += 1,
            None => {}
        }
    }
    (renamed, left)
}

/// Does one step of a repair, `a` to `b`. Returns whether it was done, and `None` when `a` is
/// surely not there, so there is nothing of ours to move.
fn repair_step(a: &Path, b: &Path) -> Option<bool> {
    // Only a name that is surely not there is passed over. One that cannot be looked at (a
    // share that answers EIO) may still hold a file, so the plan is kept for it.
    match std::fs::symlink_metadata(a) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => Some(false),
        Ok(_) => Some(rename_step(a, b).is_ok()),
    }
}

/// The files a plan's `moves` take from their old names to their new ones, as (from, to). The
/// hidden steps of a swap (a → .t0, b → a, .t0 → b) are followed to their ends, so marks and
/// collections can follow the files as after a finished Rename Many.
pub fn pairs(moves: &[(PathBuf, PathBuf)]) -> Vec<(PathBuf, PathBuf)> {
    // A file's own name is never the hidden name an earlier step gave another file. Its chain
    // runs forward through the later steps only, because a swap's steps lead round in a ring.
    // Both are looked up in maps, so a swap of 100,000 files is followed in one pass.
    let mut first_to: HashMap<&Path, usize> = HashMap::new();
    let mut steps_from: HashMap<&Path, Vec<usize>> = HashMap::new();
    for (k, (a, b)) in moves.iter().enumerate() {
        first_to.entry(b).or_insert(k);
        steps_from.entry(a).or_default().push(k);
    }
    let starts = (moves.iter().enumerate())
        .filter(|(k, (a, _))| first_to.get(a.as_path()).is_none_or(|first| first >= k));
    starts
        .map(|(k, (a, b))| {
            let (mut at, mut end): (usize, &Path) = (k, b);
            // The first step after `at` that takes the file on from `end`.
            let next = |at: usize, end: &Path| {
                let ks = steps_from.get(end)?;
                ks.get(ks.partition_point(|&n| n <= at)).copied()
            };
            while let Some(n) = next(at, end) {
                (at, end) = (n, &moves[n].1);
            }
            (a.clone(), end.to_path_buf())
        })
        .collect()
}

/// Puts the old names back or finishes the renames, by undoing the `done` steps or doing the
/// rest. For a plan that did not count them, it takes every step, by the names alone. Returns
/// (renamed, left). The plan goes when nothing is left, and otherwise it is asked about again
/// at the next start. Returns `None` when another window has the plan or has put it right
/// already. Then nothing was done here, and the caller must not tell marks and collections of
/// the moves a second time (a swap's marks would be swapped back).
pub fn repair_cut_short(
    plan: &Path,
    moves: &[(PathBuf, PathBuf)],
    done: Option<usize>,
    undo_them: bool,
) -> Option<(usize, usize)> {
    // Only one window at a time works on the plan, and only while it is there. A second window
    // that asked about it too would do the same steps again, on files already put right. That
    // also holds for one that opened it just before the first deleted it, and got the lock once
    // that one let go.
    let _held = crate::images::decode::open_regular(plan)
        .ok()
        .filter(take_lock_if_free)
        .filter(is_linked)?;
    let done = done.map(|d| done_by_names(moves, d));
    let mut first = 0;
    // When the names are put back, the step cut short between its link and its unlink, the
    // first one not counted, has its new name taken away first, so the steps before it can be
    // undone.
    #[allow(clippy::disallowed_methods)] // a second name of the same file
    if undo_them
        && let Some((a, b)) = done.and_then(|d| moves.get(d))
        && same_file_by_opening(a, b)
        && std::fs::remove_file(b).is_ok()
    {
        first = 1;
    }
    let (renamed, left) = match (undo_them, done) {
        (true, Some(done)) => undo(moves, done),
        (false, Some(done)) => finish(moves, done),
        // When the steps were not counted, every step is undone or finished, by the names
        // alone.
        (true, None) => undo(moves, moves.len()),
        (false, None) => finish(moves, 0),
    };
    if left == 0 {
        remove_plan(plan, &one_name_per_folder(moves));
    }
    Some((renamed + first, left))
}

/// How many steps were done. It is `done`, or more where the names on disk show it, because a
/// power cut may lose the last counts. The names each step leaves are followed from the start,
/// and the first count from `done` on whose names are all as on disk is taken. Undone from the
/// count alone, a swap cut short after a → .t0 and b → .t1 but counted at one put a back and
/// left b's photo under its hidden name for good, with the plan deleted. A step cut short
/// between its link and its unlink has both names for one file, and then that count is taken,
/// with the step as the first not done. When no count from `done` on fits, the last earlier
/// count that fits is taken, because the count may also have reached the disk before the names
/// did (photos on a USB stick pulled out, the plan on another disk). Where no count fits at all
/// (a file changed by hand since), the count stays. A swap's names at its start are also those
/// of its end, and below half its count tells that it was not begun. It looks once at each
/// name.
fn done_by_names(moves: &[(PathBuf, PathBuf)], done: usize) -> usize {
    // Each name gets a number, once. It records whether the name is there after the steps so
    // far, and whether it is on disk.
    let mut number: std::collections::HashMap<&Path, usize> = Default::default();
    let steps: Vec<[usize; 2]> = (moves.iter())
        .map(|(a, b)| {
            [a, b].map(|p| {
                let n = number.len();
                *number.entry(p.as_path()).or_insert(n)
            })
        })
        .collect();
    let mut on_disk = vec![false; number.len()];
    for (p, &n) in &number {
        on_disk[n] = p.exists();
    }
    // Before the first step, each step's old name that no earlier step made is there.
    let (mut there, mut made) = (vec![false; number.len()], vec![false; number.len()]);
    for &[a, b] in &steps {
        there[a] |= !made[a];
        made[b] = true;
    }
    let mut wrong = (0..there.len()).filter(|&n| there[n] != on_disk[n]).count();
    let mut earlier = None;
    for (k, &[a, b]) in steps.iter().enumerate() {
        // When cut short between this step's link and its unlink, the names are those of the
        // count before it, and its new name too, the same file as its old one. `repair_cut_short`
        // takes that name away.
        let (from, to) = (&moves[k].0, &moves[k].1);
        let fits = wrong == 0 || (wrong == 1 && !there[b] && on_disk[b] && same_file(from, to));
        match (fits, k >= done) {
            (true, true) => return k,
            (true, false) => earlier = Some(k),
            (false, _) => {}
        }
        for (n, now) in [(a, false), (b, true)] {
            if there[n] != now {
                wrong =
                    wrong + usize::from(now != on_disk[n]) - usize::from(there[n] != on_disk[n]);
                there[n] = now;
            }
        }
    }
    // A swap ends under the names it started with. Its count passes half only after its
    // middle is on disk (`rename_files`), so with a count below half those names say that it
    // was not begun.
    match (wrong == 0 && steps.len() >= done, earlier) {
        (true, Some(k)) if done < steps.len() / 2 => k,
        (true, _) => steps.len(),
        (false, _) => earlier.unwrap_or(done),
    }
}

/// Returns one name in each folder that `moves` rename in, for `sync_folders`. A Rename Many in
/// one folder gives one name, however many files it renames.
fn one_name_per_folder(moves: &[(PathBuf, PathBuf)]) -> Vec<PathBuf> {
    let mut seen = std::collections::HashSet::new();
    (moves.iter().flat_map(|(a, b)| [a, b]))
        .filter(|p| seen.insert(p.parent()))
        .cloned()
        .collect()
}

/// Syncs the folder of each of `names` to disk, so that the renames in them are there before
/// the plan that could put them right is changed or removed. The renames themselves are not
/// synced one by one: a Rename Many of 500 files on a slow USB stick would wait for 1,000
/// syncs instead of one. Returns the first name whose folder the disk said it did not take, with
/// the error. A folder that cannot be synced at all is passed over by `sync_folder_of`.
fn sync_folders(names: &[PathBuf]) -> Result<(), (&Path, std::io::Error)> {
    let mut first_failed = Ok(());
    // Every folder is synced, also after one that failed, so the loop does not stop at the first.
    for name in names {
        if let Err(e) = crate::files::file_io::sync_folder_of(name)
            && first_failed.is_ok()
        {
            first_failed = Err((name.as_path(), e));
        }
    }
    first_failed
}

/// Makes the renames of `moves` last on disk before the marks and collections follow them. The
/// plan goes after the renames' folders are synced, and without a plan (no state folder, or a
/// full disk) the folders are synced all the same. Otherwise a power cut could bring the old
/// names back while the marks are on the new ones.
pub fn renames_done(plan: Option<Journal>, moves: &[(PathBuf, PathBuf)]) {
    match plan {
        Some(plan) => drop(plan),
        None => tell_when_not_synced(sync_folders(&one_name_per_folder(moves))),
    }
}

/// Tells the user when `synced`, the answer of `sync_folders`, says that a folder did not get
/// to the disk.
fn tell_when_not_synced(synced: Result<(), (&Path, std::io::Error)>) {
    if let Err((name, e)) = synced {
        let folder = name.parent().unwrap_or(name);
        crate::files::file_io::save_failed(crate::tf!(
            "Could not save {name}: {e}",
            name = folder.display(),
            e
        ));
    }
}

/// Removes `plan` after syncing the folders of `names`. The plan goes also when the disk says a
/// folder did not get there, and the user is told. The marks and collections follow the new
/// names as soon as the renames are done, and a plan kept beside them would move them again
/// at the next start, or swap a swap's marks back.
fn remove_plan(plan: &Path, names: &[PathBuf]) {
    let renames_synced = sync_folders(names);
    #[allow(clippy::disallowed_methods)] // the plan, Glimt's own file
    let _ = std::fs::remove_file(plan);
    // The plan's folder is synced too, because a plan brought back by a power cut, after the
    // marks followed, would move them again. A disk that fails both is told once.
    let plan_name = [plan.to_path_buf()];
    tell_when_not_synced(renames_synced.and(sync_folders(&plan_name)));
}

/// Whether `file` still has a name. A plan that another window put right and deleted has none.
fn is_linked(file: &File) -> bool {
    use std::os::unix::fs::MetadataExt;
    file.metadata().is_ok_and(|m| m.nlink() > 0)
}

#[cfg(test)]
mod tests {
    /// A power cut lost the count of the last step done. The plan is undone by the names too, so
    /// no photo stays under a hidden name, and it is finished all the same.
    #[test]
    fn puts_back_the_steps_whose_count_was_lost() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-lost-count-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let plans = dir.join("plans");
        std::fs::create_dir_all(&plans).unwrap();
        let p = |n: &str| dir.join(n);
        let text = |n| std::fs::read_to_string(p(n)).unwrap();
        let moves = vec![
            (p("a.jpg"), p(".tmp-0")),
            (p("b.jpg"), p(".tmp-1")),
            (p(".tmp-0"), p("b.jpg")),
            (p(".tmp-1"), p("a.jpg")),
        ];
        // Three steps are done and one counted, and b is there again, made by the third.
        std::fs::write(p("b.jpg"), "a").unwrap();
        std::fs::write(p(".tmp-1"), "b").unwrap();
        let plan = plans.join("cut");
        let mut cut = super::start_journal(&plans, &moves).unwrap();
        cut.count_step_done();
        std::fs::copy(&cut.path, &plan).unwrap();
        drop(cut);
        assert_eq!(
            super::repair_cut_short(&plan, &moves, Some(1), true),
            Some((3, 0))
        );
        assert_eq!((text("a.jpg"), text("b.jpg")), ("a".into(), "b".into()));
        assert!(!p(".tmp-0").exists() && !p(".tmp-1").exists() && !plan.exists());
        std::fs::remove_file(p("a.jpg")).unwrap();
        std::fs::remove_file(p("b.jpg")).unwrap();
        for undo in [true, false] {
            // Two steps are done and the third linked, with one counted, so b and .tmp-0 are one
            // file. When put back, the link goes first. When finished, the third step is the
            // link's unlink.
            std::fs::write(p(".tmp-0"), "a").unwrap();
            std::fs::write(p(".tmp-1"), "b").unwrap();
            std::fs::hard_link(p(".tmp-0"), p("b.jpg")).unwrap();
            let plan = plans.join("cut");
            let mut cut = super::start_journal(&plans, &moves).unwrap();
            cut.count_step_done();
            std::fs::copy(&cut.path, &plan).unwrap();
            drop(cut);
            let (a, b, steps) = match undo {
                true => ("a", "b", 3),
                false => ("b", "a", 2),
            };
            assert_eq!(
                super::repair_cut_short(&plan, &moves, Some(1), undo),
                Some((steps, 0))
            );
            assert_eq!((text("a.jpg"), text("b.jpg")), (a.into(), b.into()));
            assert!(!p(".tmp-0").exists() && !p(".tmp-1").exists() && !plan.exists());
            std::fs::remove_file(p("a.jpg")).unwrap();
            std::fs::remove_file(p("b.jpg")).unwrap();
            // A chain without swaps, a → A … d → D, with three done, the fourth linked, and one
            // counted.
            let chain: Vec<_> = ["a", "b", "c", "d"]
                .map(|n| {
                    (
                        p(&format!("{n}.png")),
                        p(&format!("{}.png", n.to_uppercase())),
                    )
                })
                .into();
            for n in ["A", "B", "C", "d"] {
                std::fs::write(p(&format!("{n}.png")), n.to_lowercase()).unwrap();
            }
            std::fs::hard_link(p("d.png"), p("D.png")).unwrap();
            let mut cut = super::start_journal(&plans, &chain).unwrap();
            cut.count_step_done();
            std::fs::copy(&cut.path, &plan).unwrap();
            drop(cut);
            let steps = if undo { 4 } else { 1 };
            assert_eq!(
                super::repair_cut_short(&plan, &chain, Some(1), undo),
                Some((steps, 0))
            );
            for n in ["a", "b", "c", "d"] {
                let (keep, gone) = match undo {
                    true => (n.to_string(), n.to_uppercase()),
                    false => (n.to_uppercase(), n.to_string()),
                };
                assert_eq!(
                    std::fs::read_to_string(p(&format!("{keep}.png"))).unwrap(),
                    n
                );
                assert!(!p(&format!("{gone}.png")).exists());
                std::fs::remove_file(p(&format!("{keep}.png"))).unwrap();
            }
        }
        for undo in [true, false] {
            // Two steps done, one counted.
            std::fs::write(p(".tmp-0"), "a").unwrap();
            std::fs::write(p(".tmp-1"), "b").unwrap();
            let plan = plans.join("cut");
            let mut cut = super::start_journal(&plans, &moves).unwrap();
            cut.count_step_done();
            std::fs::copy(&cut.path, &plan).unwrap();
            drop(cut);
            let (a, b) = match undo {
                true => ("a", "b"),
                false => ("b", "a"),
            };
            assert_eq!(
                super::repair_cut_short(&plan, &moves, Some(1), undo),
                Some((2, 0))
            );
            assert_eq!((text("a.jpg"), text("b.jpg")), (a.into(), b.into()));
            assert!(!p(".tmp-0").exists() && !p(".tmp-1").exists() && !plan.exists());
            std::fs::remove_file(p("a.jpg")).unwrap();
            std::fs::remove_file(p("b.jpg")).unwrap();
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A finish that fails halfway undoes its own steps, so no photo stays under a hidden name
    /// that the plan's count does not describe.
    #[test]
    fn a_finish_that_fails_undoes_its_own_steps() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-finish-fails-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = |n: &str| dir.join(n);
        std::fs::write(p("a.jpg"), "a").unwrap();
        // A file put at the new name by hand stops the second step.
        std::fs::write(p("b.jpg"), "other").unwrap();
        let moves = vec![(p("a.jpg"), p(".tmp-0")), (p(".tmp-0"), p("b.jpg"))];
        assert_eq!(super::finish(&moves, 0), (0, 1));
        assert_eq!(std::fs::read_to_string(p("a.jpg")).unwrap(), "a");
        assert_eq!(std::fs::read_to_string(p("b.jpg")).unwrap(), "other");
        assert!(!p(".tmp-0").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A swap done to the end with only its first half counted (synced there) is finished, and
    /// the files stay swapped. With nothing counted, the names alone would take it for not
    /// begun, and finishing it would swap them back.
    #[test]
    fn a_whole_swap_counted_at_half_stays_swapped() {
        let dir = std::env::temp_dir().join(format!("glimt-test-half-swap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let plans = dir.join("plans");
        std::fs::create_dir_all(&plans).unwrap();
        let p = |n: &str| dir.join(n);
        let moves = vec![
            (p("a.jpg"), p(".tmp-0")),
            (p("b.jpg"), p(".tmp-1")),
            (p(".tmp-0"), p("b.jpg")),
            (p(".tmp-1"), p("a.jpg")),
        ];
        std::fs::write(p("a.jpg"), "b").unwrap();
        std::fs::write(p("b.jpg"), "a").unwrap();
        let plan = plans.join("cut");
        let mut cut = super::start_journal(&plans, &moves).unwrap();
        cut.count_step_done();
        cut.count_step_done();
        std::fs::copy(&cut.path, &plan).unwrap();
        drop(cut);
        assert_eq!(
            super::repair_cut_short(&plan, &moves, Some(2), false),
            Some((0, 0))
        );
        let text = |n| std::fs::read_to_string(p(n)).unwrap();
        assert_eq!((text("a.jpg"), text("b.jpg")), ("b".into(), "a".into()));
        assert!(!plan.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A pipe in the plans folder is passed over, not opened, because opening it would hold
    /// Glimt's start for good.
    #[test]
    fn passes_over_a_pipe_among_the_plans() {
        let dir = std::env::temp_dir().join(format!("glimt-test-plan-pipe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pipe = dir.join("rename-pipe");
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&pipe)
                .status()
                .unwrap()
                .success()
        );
        assert!(super::cut_short_plans(&dir).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A plan locked by a running rename is not taken for a cut-short one, nor written over by a
    /// second. One left by a crash is, and its files get their old names back, or their new ones.
    #[test]
    fn puts_cut_short_renames_right() {
        let dir = std::env::temp_dir().join(format!("glimt-test-journal-{}", std::process::id()));
        let plans = dir.join("plans");
        std::fs::create_dir_all(&dir).unwrap();
        let p = |n: &str| dir.join(n);
        // Rename Many swapping a and b through temporary names.
        let moves = vec![
            (p("a.jpg"), p(".tmp-0")),
            (p("b.jpg"), p(".tmp-1")),
            (p(".tmp-0"), p("b.jpg")),
            (p(".tmp-1"), p("a.jpg")),
        ];
        let running = super::start_journal(&plans, &moves).unwrap();
        // A second rename has a plan of its own, and neither is taken for one cut short.
        let second = super::start_journal(&plans, &moves[..1]).unwrap();
        assert_ne!(running.path, second.path);
        assert!(super::cut_short_plans(&plans).is_empty());
        drop(second);
        let plan_now = std::fs::read(&running.path).unwrap();
        assert_eq!(
            plan_now
                .split(|&b| b == 0)
                .filter(|p| !p.is_empty())
                .count(),
            9
        );
        drop(running);
        assert!(std::fs::read_dir(&plans).unwrap().next().is_none());
        // A plan finished while another window waits for its lock is not taken for one cut short.
        let finishing = super::start_journal(&plans, &moves).unwrap();
        let ends = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(30));
            drop(finishing);
        });
        assert!(super::cut_short_plans(&plans).is_empty());
        ends.join().unwrap();
        // Cut short after two steps: both files under hidden names.
        std::fs::write(p(".tmp-0"), "a").unwrap();
        std::fs::write(p(".tmp-1"), "b").unwrap();
        let mut cut = super::start_journal(&plans, &moves).unwrap();
        cut.count_step_done();
        cut.count_step_done();
        let plan = cut.path.clone();
        std::mem::forget(cut);
        // As after a crash, the lock is gone with the process.
        let found = {
            std::fs::copy(&plan, plans.join("crashed")).unwrap();
            super::cut_short_plans(&plans)
        };
        let (path, read, done) = found
            .into_iter()
            .find(|f| f.0.ends_with("crashed"))
            .unwrap();
        assert_eq!((&read, done), (&moves, Some(2)));
        let text = |n| std::fs::read_to_string(p(n)).unwrap();
        // Cut short between the link and the unlink of the third step, b.jpg and .tmp-0 are one
        // file. When finished, the old name goes, and nothing is left to ask about again.
        std::fs::hard_link(p(".tmp-0"), p("b.jpg")).unwrap();
        std::fs::copy(&path, plans.join("linked")).unwrap();
        assert_eq!(
            super::repair_cut_short(&plans.join("linked"), &read, Some(2), false),
            Some((2, 0))
        );
        assert_eq!((text("a.jpg"), text("b.jpg")), ("b".into(), "a".into()));
        assert!(!p(".tmp-0").exists());
        std::fs::rename(p("a.jpg"), p(".tmp-1")).unwrap();
        std::fs::rename(p("b.jpg"), p(".tmp-0")).unwrap();
        // The same is put back. The new name of the half-done step goes, then the two before.
        std::fs::hard_link(p(".tmp-0"), p("b.jpg")).unwrap();
        std::fs::copy(&path, plans.join("linked")).unwrap();
        assert_eq!(
            super::repair_cut_short(&plans.join("linked"), &read, Some(2), true),
            Some((3, 0))
        );
        assert_eq!((text("a.jpg"), text("b.jpg")), ("a".into(), "b".into()));
        std::fs::rename(p("a.jpg"), p(".tmp-0")).unwrap();
        std::fs::rename(p("b.jpg"), p(".tmp-1")).unwrap();
        // While another window puts it right, this one does nothing.
        let other = std::fs::File::open(&path).unwrap();
        other.lock().unwrap();
        assert_eq!(super::repair_cut_short(&path, &read, done, true), None);
        drop(other);
        assert_eq!(
            super::repair_cut_short(&path, &read, done, true),
            Some((2, 0))
        );
        assert_eq!((text("a.jpg"), text("b.jpg")), ("a".into(), "b".into()));
        assert!(!path.exists());
        // One that opened it before it was deleted finds it has no name left.
        std::fs::write(&path, "x").unwrap();
        let opened = std::fs::File::open(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert!(!super::is_linked(&opened));
        // A second window that asked about it too finds it gone, and does nothing.
        assert_eq!(super::repair_cut_short(&path, &read, done, true), None);
        assert_eq!((text("a.jpg"), text("b.jpg")), ("a".into(), "b".into()));
        // Finishing instead gives the new names.
        std::fs::rename(p("a.jpg"), p(".tmp-0")).unwrap();
        std::fs::rename(p("b.jpg"), p(".tmp-1")).unwrap();
        std::fs::copy(&plan, plans.join("crashed")).unwrap();
        assert_eq!(
            super::repair_cut_short(&plans.join("crashed"), &moves, Some(2), false),
            Some((2, 0))
        );
        assert_eq!((text("a.jpg"), text("b.jpg")), ("b".into(), "a".into()));
        // Cut short after three steps, where the names alone look like after one, the plan is
        // finished. The swap is whole and the plan gone, so it is not done a second time. It
        // can also be undone.
        let after_three = || {
            std::fs::write(p(".tmp-1"), "b").unwrap();
            std::fs::write(p("b.jpg"), "a").unwrap();
            std::fs::remove_file(p("a.jpg")).unwrap();
            std::fs::copy(&plan, plans.join("crashed")).unwrap();
        };
        after_three();
        assert_eq!(
            super::repair_cut_short(&plans.join("crashed"), &moves, Some(3), false),
            Some((1, 0))
        );
        assert_eq!((text("a.jpg"), text("b.jpg")), ("b".into(), "a".into()));
        assert!(!plans.join("crashed").exists());
        after_three();
        assert_eq!(
            super::repair_cut_short(&plans.join("crashed"), &moves, Some(3), true),
            Some((3, 0))
        );
        assert_eq!((text("a.jpg"), text("b.jpg")), ("a".into(), "b".into()));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Damaged plans are read without a panic, and never count more steps done than they have.
    #[test]
    fn survives_broken_plans() {
        let seed = b"counted\0/p/a\0/p/.t-0\0/p/b\0/p/.t-1\0/p/.t-0\0/p/b\0\0\0";
        for bytes in crate::support::test_support::broken(seed) {
            if let Some((moves, Some(done))) = super::parse(&bytes) {
                assert!(done <= moves.len());
            }
        }
    }

    /// A Rename Many whose plan cannot be written is told so, as it then runs without the repair
    /// at the next start. Before, the user was not told that this protection was gone.
    #[test]
    fn a_plan_that_cannot_be_written_is_told() {
        use crate::files::file_io::take_saves_failed;
        let dir = std::env::temp_dir().join(format!("glimt-test-no-plan-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A file where the plans' folder should be, so that no plan can be made in it.
        let plans = dir.join("plans");
        std::fs::write(&plans, "").unwrap();
        take_saves_failed();
        let moves = vec![(dir.join("a.jpg"), dir.join("b.jpg"))];
        assert!(super::start_journal(&plans, &moves).is_none());
        assert_eq!(take_saves_failed().len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The plan is on disk with its name before the first rename, and the folder of the renames
    /// is synced before the plan is removed, once however many files it renames. A plan kept on
    /// disk needs no such sync.
    #[test]
    fn the_plan_goes_only_after_the_renames_are_on_disk() {
        let dir = std::env::temp_dir().join(format!("glimt-test-plan-sync-{}", std::process::id()));
        let plans = dir.join("plans");
        let p = |n: &str| dir.join("photos").join(n);
        let moves = vec![(p("a.jpg"), p("b.jpg")), (p("c.jpg"), p("d.jpg"))];
        crate::files::file_io::FOLDERS_SYNCED.take();
        let plan = super::start_journal(&plans, &moves).unwrap();
        assert_eq!(
            crate::files::file_io::FOLDERS_SYNCED.take(),
            [plans.clone(), dir.clone()]
        );
        drop(plan);
        assert_eq!(
            crate::files::file_io::FOLDERS_SYNCED.take(),
            [dir.join("photos"), plans.clone()]
        );
        // Without a plan, the renames' folders are synced all the same.
        super::renames_done(None, &moves);
        assert_eq!(
            crate::files::file_io::FOLDERS_SYNCED.take(),
            [dir.join("photos")]
        );
        let kept = super::start_journal(&plans, &moves).unwrap();
        crate::files::file_io::FOLDERS_SYNCED.take();
        kept.keep_on_disk();
        assert!(crate::files::file_io::FOLDERS_SYNCED.take().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// When the disk says that the renames' folder did not get there, the user is told, and the
    /// plan goes all the same, both after a Rename Many and after a repair. The marks and
    /// collections have followed the new names by then, and a plan kept beside them would move
    /// them again at the next start, or swap a swap's marks back.
    #[test]
    fn the_plan_goes_and_the_user_is_told_when_the_renames_did_not_reach_the_disk() {
        use crate::files::file_io::{FOLDER_SYNC_FAILS, take_saves_failed};
        let dir =
            std::env::temp_dir().join(format!("glimt-test-sync-fails-{}", std::process::id()));
        let plans = dir.join("plans");
        let moves = vec![(dir.join("a.jpg"), dir.join("b.jpg"))];
        // What this thread told, which no other test can take.
        let told_once = || take_saves_failed().len() == 1;
        let plan = super::start_journal(&plans, &moves).unwrap();
        let path = plan.path.clone();
        take_saves_failed();
        FOLDER_SYNC_FAILS.set(true);
        drop(plan);
        FOLDER_SYNC_FAILS.set(false);
        assert!(!path.exists() && told_once());
        let plan = super::start_journal(&plans, &moves).unwrap();
        let path = plan.path.clone();
        plan.keep_on_disk();
        FOLDER_SYNC_FAILS.set(true);
        assert_eq!(
            super::repair_cut_short(&path, &moves, Some(1), false),
            Some((0, 0))
        );
        FOLDER_SYNC_FAILS.set(false);
        assert!(!path.exists() && told_once());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// When the middle of a swap does not reach the disk, it is not counted. A count of half
    /// would otherwise tell the next start that the swap was done while the names are those
    /// of the start.
    #[test]
    fn the_middle_of_a_swap_is_not_counted_when_its_names_did_not_reach_the_disk() {
        use crate::files::file_io::FOLDER_SYNC_FAILS;
        let dir = std::env::temp_dir().join(format!("glimt-test-middle-{}", std::process::id()));
        let moves = vec![(dir.join("a"), dir.join("b"))];
        let mut plan = super::start_journal(&dir, &moves).unwrap();
        FOLDER_SYNC_FAILS.set(true);
        let counted = plan.count_middle_of_swap_done();
        FOLDER_SYNC_FAILS.set(false);
        assert!(counted.is_err());
        plan.keep_on_disk();
        assert_eq!(super::cut_short_plans(&dir)[0].2, Some(0));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A plan kept when its renames could not be undone is there at the next start, with the
    /// steps it had done.
    #[test]
    fn a_kept_plan_stays() {
        let dir = std::env::temp_dir().join(format!("glimt-test-kept-{}", std::process::id()));
        let moves = vec![(dir.join("a"), dir.join("b"))];
        let mut plan = super::start_journal(&dir, &moves).unwrap();
        plan.count_step_done();
        plan.keep_on_disk();
        let left = super::cut_short_plans(&dir);
        assert_eq!((left.len(), left[0].2), (1, Some(1)));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A swap's steps are followed to their ends. a goes to b and b to a, and a plain rename is
    /// itself.
    #[test]
    fn pairs_follow_the_steps() {
        let p = |n: &str| std::path::PathBuf::from(n);
        let moves = vec![
            (p("a"), p(".t0")),
            (p("b"), p(".t1")),
            (p(".t0"), p("b")),
            (p(".t1"), p("a")),
        ];
        assert_eq!(
            super::pairs(&moves),
            vec![(p("a"), p("b")), (p("b"), p("a"))]
        );
        assert_eq!(super::pairs(&[(p("c"), p("d"))]), vec![(p("c"), p("d"))]);
        // A swap of 40,000 files is followed in a moment, not in minutes.
        let n = 20_000;
        let mut moves = Vec::new();
        for k in 0..n {
            moves.push((p(&format!("a{k}")), p(&format!(".t{k}"))));
            moves.push((p(&format!("b{k}")), p(&format!("a{k}"))));
        }
        moves.extend((0..n).map(|k| (p(&format!(".t{k}")), p(&format!("b{k}")))));
        let start = std::time::Instant::now();
        let pairs = super::pairs(&moves);
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
        assert_eq!(pairs.len(), 2 * n);
        assert_eq!(pairs[0], (p("a0"), p("b0")));
        assert_eq!(pairs[1], (p("b0"), p("a0")));
    }

    /// A step whose old name cannot be looked at counts as left, not as gone, so the plan is
    /// kept. Here the error is "not a folder"; on a share it is EIO.
    #[test]
    fn a_name_that_cannot_be_read_is_left() {
        let dir = std::env::temp_dir().join(format!("glimt-test-eio-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("f"), b"").unwrap();
        let moves = [(dir.join("f/a.jpg"), dir.join("b.jpg"))];
        assert_eq!(super::finish(&moves, 0), (0, 1));
        assert_eq!(
            super::finish(&[(dir.join("gone"), dir.join("b.jpg"))], 0),
            (0, 0)
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A step left under both names, where one of them may be gone from a share whose cache
    /// still shows it, keeps the other name. It is removed only when an open of each name finds
    /// one file, both as the step is done and as the plan is put back.
    #[test]
    fn a_second_name_goes_only_when_the_other_opens() {
        use crate::images::save::GONE_FROM_THE_SERVER;
        let dir = std::env::temp_dir().join(format!("glimt-test-cached-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let plans = dir.join("plans");
        std::fs::create_dir_all(&plans).unwrap();
        let p = |n: &str| dir.join(n);
        let moves = vec![(p("a.jpg"), p("b.jpg"))];
        std::fs::write(p("a.jpg"), "a").unwrap();
        std::fs::hard_link(p("a.jpg"), p("b.jpg")).unwrap();
        GONE_FROM_THE_SERVER.set(Some(p("b.jpg")));
        let step = super::rename_step(&p("a.jpg"), &p("b.jpg"));
        GONE_FROM_THE_SERVER.set(None);
        assert!(step.is_err() && p("a.jpg").exists());
        let plan = plans.join("cut");
        let cut = super::start_journal(&plans, &moves).unwrap();
        std::fs::copy(&cut.path, &plan).unwrap();
        drop(cut);
        GONE_FROM_THE_SERVER.set(Some(p("a.jpg")));
        super::repair_cut_short(&plan, &moves, Some(0), true);
        GONE_FROM_THE_SERVER.set(None);
        assert!(p("b.jpg").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The failing path and the recovery share one undo. Two steps of a swap are done and then
    /// undone, last first. One of them put back by hand already is left out, and not counted as
    /// stuck.
    #[test]
    fn undoes_the_steps_done() {
        let dir = std::env::temp_dir().join(format!("glimt-test-undo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = |n: &str| dir.join(n);
        std::fs::write(p("a"), "a").unwrap();
        std::fs::write(p("b"), "b").unwrap();
        let moves = vec![(p("a"), p(".t0")), (p("b"), p(".t1")), (p(".t0"), p("b"))];
        assert_eq!(super::finish(&moves, 0), (3, 0));
        assert!(!p("a").exists() && p(".t1").exists());
        assert_eq!(std::fs::read_to_string(p("b")).unwrap(), "a");
        assert_eq!(super::undo(&moves, 3), (3, 0));
        assert_eq!(std::fs::read_to_string(p("a")).unwrap(), "a");
        assert_eq!(std::fs::read_to_string(p("b")).unwrap(), "b");
        // Two are done, and one of them is put back by hand, so only the other is undone.
        std::fs::rename(p("a"), p(".t0")).unwrap();
        std::fs::rename(p("b"), p(".t1")).unwrap();
        std::fs::rename(p(".t1"), p("b")).unwrap();
        assert_eq!(super::undo(&moves, 2), (1, 0));
        assert!(p("a").exists() && p("b").exists() && !p(".t0").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Every place a Rename Many can be cut short is tried, also between the link and the
    /// unlink of one step, for a swap, a cycle of three, a shift and plain new names. Putting
    /// back gives every old name, finishing gives every new one, no hidden name is left, and a
    /// second repair does nothing.
    #[test]
    fn repairs_every_cut() {
        let dir = std::env::temp_dir().join(format!("glimt-test-cuts-{}", std::process::id()));
        let plans = dir.join("plans");
        let p = |n: &str| dir.join(n);
        let cases: [(&[&str], &[&str]); 4] = [
            (&["a", "b"], &["b", "a"]),
            (&["a", "b", "c"], &["b", "c", "a"]),
            (&["a", "b"], &["b", "c"]),
            (&["a", "b"], &["x", "y"]),
        ];
        for (from, to) in cases {
            // This is the plan as rename_many makes it, through hidden names when a new name is
            // an old one.
            let mut moves: super::Moves = Vec::new();
            if to.iter().any(|t| from.contains(t)) {
                let tmp = |k: usize| p(&format!(".tmp-{k}"));
                moves.extend(from.iter().enumerate().map(|(k, f)| (p(f), tmp(k))));
                moves.extend(to.iter().enumerate().map(|(k, t)| (tmp(k), p(t))));
            } else {
                moves.extend(from.iter().zip(to).map(|(f, t)| (p(f), p(t))));
            }
            for cut in 0..=moves.len() {
                for half in [false, true] {
                    if half && cut == moves.len() {
                        continue;
                    }
                    for undo in [false, true] {
                        let _ = std::fs::remove_dir_all(&dir);
                        std::fs::create_dir_all(&dir).unwrap();
                        for f in from {
                            std::fs::write(p(f), f).unwrap();
                        }
                        let mut plan = super::start_journal(&plans, &moves).unwrap();
                        for (a, b) in &moves[..cut] {
                            std::fs::rename(a, b).unwrap();
                            plan.count_step_done();
                        }
                        if half {
                            std::fs::hard_link(&moves[cut].0, &moves[cut].1).unwrap();
                        }
                        // As after a crash, the plan is on disk and no longer locked.
                        let bytes = std::fs::read(&plan.path).unwrap();
                        drop(plan);
                        std::fs::write(plans.join("crashed"), bytes).unwrap();
                        let [(path, read, done)] = &super::cut_short_plans(&plans)[..] else {
                            panic!("one plan");
                        };
                        assert_eq!((read, *done), (&moves, Some(cut)));
                        let (_, left) = super::repair_cut_short(path, read, *done, undo).unwrap();
                        let case = format!("{from:?} -> {to:?}, cut {cut}, half {half}");
                        assert_eq!(left, 0, "{case}");
                        assert!(!path.exists(), "{case}");
                        let names = if undo { from } else { to };
                        for (f, n) in from.iter().zip(names) {
                            assert_eq!(std::fs::read_to_string(p(n)).unwrap(), *f, "{case}");
                        }
                        let mut there: Vec<String> = (std::fs::read_dir(&dir).unwrap())
                            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                            .filter(|n| n != "plans")
                            .collect();
                        there.sort();
                        let mut want: Vec<String> = names.iter().map(|n| n.to_string()).collect();
                        want.sort();
                        assert_eq!(there, want, "{case}");
                        assert_eq!(
                            super::repair_cut_short(path, read, *done, undo),
                            None,
                            "{case}"
                        );
                    }
                }
            }
            for cut in 0..moves.len() {
                for counted in cut + 1..=moves.len() {
                    for undo in [false, true] {
                        let case = format!("{from:?} -> {to:?}, cut {cut}, counted {counted}");
                        let cuts = (cut, counted);
                        repair_counted_ahead(&dir, &moves, (from, to), cuts, undo, &case);
                    }
                }
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Cuts `moves` short after `cut` steps on disk, with `counted` of them counted in the plan,
    /// as when the count reached its disk and the photos' folder did not. Then it puts them
    /// right, and checks that no photo is left under a hidden name or lost.
    fn repair_counted_ahead(
        dir: &std::path::Path,
        moves: &super::Moves,
        (from, to): (&[&str], &[&str]),
        (cut, counted): (usize, usize),
        undo: bool,
        case: &str,
    ) {
        let plans = dir.join("plans");
        let _ = std::fs::remove_dir_all(dir);
        std::fs::create_dir_all(dir).unwrap();
        for f in from {
            std::fs::write(dir.join(f), f).unwrap();
        }
        let mut plan = super::start_journal(&plans, moves).unwrap();
        for (a, b) in &moves[..cut] {
            std::fs::rename(a, b).unwrap();
        }
        for _ in 0..counted {
            plan.count_step_done();
        }
        let bytes = std::fs::read(&plan.path).unwrap();
        drop(plan);
        std::fs::write(plans.join("crashed"), bytes).unwrap();
        let [(path, read, done)] = &super::cut_short_plans(&plans)[..] else {
            panic!("one plan");
        };
        let (_, left) = super::repair_cut_short(path, read, *done, undo).unwrap();
        assert_eq!(left, 0, "{case}");
        assert!(!path.exists(), "{case}");
        // Each photo is under a name it is meant to have, and none under a hidden one. A swap
        // not begun has the names of one done. Its count is below half then, because the
        // middle of a swap is on disk, names first, before its count (`rename_files`). A count
        // of half or more with no step on disk cannot happen, and which of the two it was
        // cannot be told.
        let mut there: Vec<(String, String)> = (std::fs::read_dir(dir).unwrap())
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n != "plans")
            .map(|n| (std::fs::read_to_string(dir.join(&n)).unwrap(), n))
            .collect();
        there.sort();
        assert!(
            there.iter().all(|(_, n)| !n.starts_with('.')),
            "{case}: {there:?}"
        );
        let photos: Vec<&str> = there.iter().map(|(photo, _)| photo.as_str()).collect();
        assert_eq!(photos, from, "{case}");
        if cut == 0 && counted >= moves.len() / 2 && to.iter().all(|t| from.contains(t)) {
            return;
        }
        let names = if undo { from } else { to };
        for (f, n) in from.iter().zip(names) {
            assert_eq!(std::fs::read_to_string(dir.join(n)).unwrap(), *f, "{case}");
        }
    }
}
