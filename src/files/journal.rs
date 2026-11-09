//! Rename Many's steps on disk while they run. A crash or a power cut halfway leaves files under
//! their new names, their old ones, or hidden temporary ones. The next start finds the plan and
//! puts the old names back or finishes. The plan is locked while it runs, so a plan that another
//! window is still carrying out is left alone.

use crate::images::save::same_file;
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

    /// The steps done and counted so far on the disk, past a power cut: the renames' folders
    /// first, so that the count never says more than the names on disk show.
    pub fn sync(&mut self) {
        if sync_folders(&self.one_name_per_folder) {
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
        // A plan kept on disk (`keep_on_disk`) has no path, and nothing need be on disk first.
        if self.path.as_os_str().is_empty() {
            return;
        }
        // When the disk says the renames did not get there, the plan stays, so that the next
        // start offers to put the files right.
        if sync_folders(&self.one_name_per_folder) {
            #[allow(clippy::disallowed_methods)] // the journal, Glimt's own file
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

pub fn journal_dir() -> Option<PathBuf> {
    crate::files::config::xdg_dir("XDG_STATE_HOME", ".local/state", "glimt")
        .map(|d| d.join("renames"))
}

/// Writes `moves` down in `dir`, locked and on disk with its name, before the first of them is
/// done. Returns `None` if that fails, and the renames then run as they did before, without a
/// plan.
pub fn start_journal(dir: &Path, moves: &[(PathBuf, PathBuf)]) -> Option<Journal> {
    std::fs::create_dir_all(dir).ok()?;
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
        .open(&path)
        .ok()?;
    // The plan is removed when dropped, also when what follows fails (a full disk). A plan left
    // half written would be offered at the next start, and finishing it would undo renames done
    // without it.
    let one_name_per_folder = one_name_per_folder(moves);
    let mut plan = Journal {
        path,
        file,
        one_name_per_folder,
    };
    plan.file.try_lock().ok()?;
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
        .and_then(|_| crate::files::file_io::sync_folder_of(dir))
        .ok()?;
    Some(plan)
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
    if same_file(a, b) {
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

/// Does the steps of `moves` after the first `done`. Returns (renamed, left).
pub fn finish(moves: &[(PathBuf, PathBuf)], done: usize) -> (usize, usize) {
    let rest = moves[done.min(moves.len())..].iter();
    do_steps(rest.map(|(a, b)| (a.as_path(), b.as_path())))
}

fn do_steps<'a>(steps: impl Iterator<Item = (&'a Path, &'a Path)>) -> (usize, usize) {
    let (mut renamed, mut left) = (0, 0);
    for (a, b) in steps {
        // Only a name that is surely not there is passed over. One that cannot be looked at
        // (a share that answers EIO) may still hold a file, so the plan is kept for it.
        match std::fs::symlink_metadata(a) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                left += 1;
                continue;
            }
            Ok(_) => {}
        }
        match rename_step(a, b) {
            Ok(()) => renamed += 1,
            Err(_) => left += 1,
        }
    }
    (renamed, left)
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
        && same_file(a, b)
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
    // When the disk says the renames did not get there, the plan stays and is asked about again
    // at the next start.
    if left == 0 && sync_folders(&one_name_per_folder(moves)) {
        #[allow(clippy::disallowed_methods)] // the plan, Glimt's own file
        let _ = std::fs::remove_file(plan);
    }
    Some((renamed + first, left))
}

/// How many steps were done. It is `done`, or more where the names on disk show it, because a
/// power cut may lose the last counts. The names each step leaves are followed from the start,
/// and the first count from `done` on whose names are all as on disk is taken. Undone from the
/// count alone, a swap cut short after a → .t0 and b → .t1 but counted at one put a back and
/// left b's photo under its hidden name for good, with the plan deleted. A step cut short
/// between its link and its unlink has both names for one file, and then that count is taken,
/// with the step as the first not done. Where no count fits (a file changed by hand since), the
/// count stays. It looks once at each name.
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
    for (k, &[a, b]) in steps.iter().enumerate() {
        if k >= done && wrong == 0 {
            return k;
        }
        // When cut short between this step's link and its unlink, the names are those of the
        // count before it, and its new name too, the same file as its old one. `repair_cut_short`
        // takes that name away.
        let (from, to) = (&moves[k].0, &moves[k].1);
        if k >= done && wrong == 1 && !there[b] && on_disk[b] && same_file(from, to) {
            return k;
        }
        for (n, now) in [(a, false), (b, true)] {
            if there[n] != now {
                wrong =
                    wrong + usize::from(now != on_disk[n]) - usize::from(there[n] != on_disk[n]);
                there[n] = now;
            }
        }
    }
    match wrong == 0 && steps.len() >= done {
        true => steps.len(),
        false => done,
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
/// syncs instead of one. Returns whether every folder got to the disk; a folder that cannot be
/// synced at all is passed over by `sync_folder_of`, so `false` means the disk failed.
fn sync_folders(names: &[PathBuf]) -> bool {
    // Every folder is synced, also after one that failed, so no `all` that stops at the first.
    let mut every_one_synced = true;
    for name in names {
        every_one_synced &= crate::files::file_io::sync_folder_of(name).is_ok();
    }
    every_one_synced
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
            [dir.join("photos")]
        );
        let kept = super::start_journal(&plans, &moves).unwrap();
        crate::files::file_io::FOLDERS_SYNCED.take();
        kept.keep_on_disk();
        assert!(crate::files::file_io::FOLDERS_SYNCED.take().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// When the disk says that the renames' folder did not get there, the plan stays on disk,
    /// both after a Rename Many and after a repair, so the next start offers to put it right.
    #[test]
    fn the_plan_stays_when_the_renames_did_not_reach_the_disk() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-sync-fails-{}", std::process::id()));
        let plans = dir.join("plans");
        let moves = vec![(dir.join("a.jpg"), dir.join("b.jpg"))];
        let plan = super::start_journal(&plans, &moves).unwrap();
        let path = plan.path.clone();
        crate::files::file_io::FOLDER_SYNC_FAILS.set(true);
        drop(plan);
        assert!(path.exists());
        assert_eq!(
            super::repair_cut_short(&path, &moves, Some(1), false),
            Some((0, 0))
        );
        crate::files::file_io::FOLDER_SYNC_FAILS.set(false);
        assert!(path.exists());
        assert_eq!(
            super::repair_cut_short(&path, &moves, Some(1), false),
            Some((0, 0))
        );
        assert!(!path.exists());
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
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
