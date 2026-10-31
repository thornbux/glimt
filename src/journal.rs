//! Rename Many's steps on disk while they run. A crash or a power cut halfway leaves files under
//! their new names, their old ones, or hidden temporary ones; the next start finds the plan and
//! puts the old names back or finishes. The plan is locked while it runs, so a plan that another
//! window is still carrying out is left alone.

use std::ffi::OsStr;
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// Renames in the order they are done: (from, to).
pub type Moves = Vec<(PathBuf, PathBuf)>;

/// A plan left by a Rename Many that was cut short: its file, the renames, and how many of them
/// were done; `None` for a plan of Glimt 0.34 or older, which did not count them.
pub type CutShort = (PathBuf, Moves, Option<usize>);

/// The first part of a plan that counts the renames done.
const COUNTED: &[u8] = b"counted";

pub fn dir() -> Option<PathBuf> {
    crate::config::xdg_dir("XDG_STATE_HOME", ".local/state", "glimt").map(|d| d.join("renames"))
}

/// The plan of a Rename Many being carried out; removed when dropped, as the renames are done
/// or undone. Left on disk only when Glimt stops halfway.
pub struct Journal {
    path: PathBuf,
    file: File,
}

impl Journal {
    /// One more rename done: a NUL byte after the plan, so that the next start knows which. The
    /// file names cannot tell, as a swap has the same names before and after.
    // Not synced, which a crash of Glimt does not need; a power cut may lose the last steps'
    // bytes, which `settle` finds again by the names (`done_by_names`).
    pub fn done(&mut self) {
        let _ = self.file.write_all(&[0]);
    }
}

impl Journal {
    /// Left on disk: the renames could not all be undone, so the next start offers to put the
    /// files right. Its lock goes with this Glimt.
    pub fn keep(mut self) {
        // An empty path: the remove in `drop` finds nothing.
        self.path = PathBuf::new();
    }
}

impl Drop for Journal {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Write `moves` down in `dir`, locked and on disk, before the first of them is done. `None`
/// if that fails: the renames then run as they did before, without a plan.
pub fn begin(dir: &Path, moves: &[(PathBuf, PathBuf)]) -> Option<Journal> {
    std::fs::create_dir_all(dir).ok()?;
    // A name of its own, never one there: a plan left by a crash, of a Glimt that had the same
    // process ID before a restart, is not written over and lost.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let path = dir.join(format!("{}-{nanos}", crate::remote::instance_id()));
    #[allow(clippy::disallowed_methods)] // a plan made new, in Glimt's own state folder
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .ok()?;
    // Removed when dropped, also when what follows fails (a full disk): a plan left half written
    // would be offered at the next start, and finishing it would undo renames done without it.
    let mut plan = Journal { path, file };
    plan.file.try_lock().ok()?;
    let mut bytes = [COUNTED, b"\0"].concat();
    for (a, b) in moves {
        for p in [a, b] {
            bytes.extend(p.as_os_str().as_bytes());
            bytes.push(0);
        }
    }
    (plan.file.write_all(&bytes))
        .and_then(|_| plan.file.sync_all())
        .ok()?;
    Some(plan)
}

/// Whether `file` can be locked: tried for a moment, as a program another thread starts holds a
/// copy of the descriptor, and its lock, until it runs.
fn free(file: &File) -> bool {
    (0..10).any(|k| {
        if k > 0 {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        file.try_lock().is_ok()
    })
}

/// The plans in `dir` of renames that were cut short: those no running window holds locked.
pub fn interrupted(dir: &Path) -> Vec<CutShort> {
    let entries = std::fs::read_dir(dir).into_iter().flatten().flatten();
    entries
        .filter_map(|e| {
            // A pipe or a link planted in the state folder is no plan: opening a pipe would hold
            // the start for good. Read no more than a decode would.
            e.file_type().ok().filter(|t| t.is_file())?;
            let file = crate::decode::open(e.path()).ok()?;
            free(&file).then_some(())?;
            let mut bytes = Vec::new();
            #[allow(clippy::disallowed_methods)] // bounded by take, and only a file
            let read = (&file)
                .take(crate::decode::MAX_DECODE)
                .read_to_end(&mut bytes);
            read.ok()?;
            let (moves, done) = parse(&bytes)?;
            Some((e.path(), moves, done))
        })
        .collect()
}

/// A plan's renames and how many were done; `None` for one with no paths.
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
    // Made a moment ago and not yet locked or written: not one left by a crash.
    if paths.is_empty() {
        return None;
    }
    let moves = (paths.as_chunks::<2>().0.iter()).map(|[a, b]| (a.clone(), b.clone()));
    let moves: Moves = moves.collect();
    let done = counted.then(|| done.min(moves.len()));
    Some((moves, done))
}

/// One step of a plan, `a` to `b`: never over another file (`put_file`). A step cut short
/// between its link and its unlink, both names the file, is finished by taking the old one
/// away. The one way a step is done, whether as Rename Many runs, is undone, or is put right
/// at the next start.
pub fn step(a: &Path, b: &Path) -> std::io::Result<()> {
    if same_file(a, b) {
        return std::fs::remove_file(a);
    }
    crate::file_ops::put_file(a, b, false)
}

/// The first `done` steps of `moves` undone, last first; (renamed, left). A name no longer
/// there is nothing of ours to put back, and is left out of both.
pub fn undo(moves: &[(PathBuf, PathBuf)], done: usize) -> (usize, usize) {
    let back = moves[..done.min(moves.len())].iter().rev();
    steps(back.map(|(a, b)| (b.as_path(), a.as_path())))
}

/// The steps of `moves` after the first `done` done; (renamed, left).
pub fn finish(moves: &[(PathBuf, PathBuf)], done: usize) -> (usize, usize) {
    let rest = moves[done.min(moves.len())..].iter();
    steps(rest.map(|(a, b)| (a.as_path(), b.as_path())))
}

fn steps<'a>(steps: impl Iterator<Item = (&'a Path, &'a Path)>) -> (usize, usize) {
    let (mut renamed, mut left) = (0, 0);
    for (a, b) in steps {
        if !a.exists() {
            continue;
        }
        match step(a, b) {
            Ok(()) => renamed += 1,
            Err(_) => left += 1,
        }
    }
    (renamed, left)
}

/// The files a plan's `moves` take from their old names to their new ones, (from, to): the
/// hidden steps of a swap (a → .t0, b → a, .t0 → b) followed to their ends, so marks and
/// collections can follow the files as after a finished Rename Many.
pub fn pairs(moves: &[(PathBuf, PathBuf)]) -> Vec<(PathBuf, PathBuf)> {
    // A file's own name is never the hidden name an earlier step gave another file; its chain
    // runs forward through the later steps only, as a swap's steps lead round in a ring.
    let starts =
        (moves.iter().enumerate()).filter(|(k, (a, _))| !moves[..*k].iter().any(|(_, b)| b == a));
    starts
        .map(|(k, (a, b))| {
            let (mut at, mut end): (usize, &Path) = (k, b);
            while let Some((n, (_, next))) =
                (moves.iter().enumerate()).find(|(n, (from, _))| *n > at && from == end)
            {
                (at, end) = (n, next);
            }
            (a.clone(), end.to_path_buf())
        })
        .collect()
}

/// Put the old names back, or finish the renames: the `done` steps undone, or the rest done; for
/// a plan that did not count them, every step, by the names alone. (renamed, left); the plan
/// goes when nothing is left, else it is asked about again at the next start. `None` when
/// another window has it or has put it right already: nothing was done here, and the caller
/// must not tell marks and collections of moves a second time (a swap's marks swapped back).
pub fn settle(
    plan: &Path,
    moves: &[(PathBuf, PathBuf)],
    done: Option<usize>,
    undo_them: bool,
) -> Option<(usize, usize)> {
    // One window at a time, and only while the plan is there: a second window that asked about
    // it too would do the same steps again, on files already put right. Also one that opened it
    // just before the first deleted it, and got the lock once that one let go.
    let _held = crate::decode::open(plan).ok().filter(free).filter(linked)?;
    let done = done.map(|d| done_by_names(moves, d));
    let mut first = 0;
    // Put back: the step cut short between its link and its unlink, the first not counted, has
    // its new name taken away first, so the steps before it can be undone.
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
        // Not counted: every step, by the names alone.
        (true, None) => undo(moves, moves.len()),
        (false, None) => finish(moves, 0),
    };
    if left == 0 {
        let _ = std::fs::remove_file(plan);
    }
    Some((renamed + first, left))
}

/// How many steps were done: `done`, or more where the names on disk show it, as a power cut
/// may lose the last counts. The names each step leaves are followed from the start, and the
/// first count from `done` on whose names are all as on disk is taken. Undone from the count
/// alone, a swap cut short after a → .t0 and b → .t1 but counted at one put a back and left b's
/// photo under its hidden name for good, the plan deleted. Where no count fits (a step cut
/// short between its link and its unlink has both names; a file changed by hand since), the
/// count stays. One look at each name, so thousands of steps take no longer than their names.
fn done_by_names(moves: &[(PathBuf, PathBuf)], done: usize) -> usize {
    // Each name once, by number; whether it is there after the steps so far, and on disk.
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
    // Before the first step: each step's old name that no earlier step made.
    let mut there = vec![false; number.len()];
    for (k, [a, _]) in steps.iter().enumerate() {
        if !steps[..k].iter().any(|[_, b]| b == a) {
            there[*a] = true;
        }
    }
    let mut wrong = (0..there.len()).filter(|&n| there[n] != on_disk[n]).count();
    for (k, &[a, b]) in steps.iter().enumerate() {
        if k >= done && wrong == 0 {
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

/// Whether `file` still has a name: a plan another window put right and deleted has none.
fn linked(file: &File) -> bool {
    use std::os::unix::fs::MetadataExt;
    file.metadata().is_ok_and(|m| m.nlink() > 0)
}

/// Whether `a` and `b` are names of the same file.
fn same_file(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(x), Ok(y)) => (x.dev(), x.ino()) == (y.dev(), y.ino()),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    /// A power cut lost the count of the last step done: undone by the names too, so no photo
    /// stays under a hidden name; finished all the same.
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
        // Three steps done, one counted: b is there again, made by the third.
        std::fs::write(p("b.jpg"), "a").unwrap();
        std::fs::write(p(".tmp-1"), "b").unwrap();
        let plan = plans.join("cut");
        let mut cut = super::begin(&plans, &moves).unwrap();
        cut.done();
        std::fs::copy(&cut.path, &plan).unwrap();
        drop(cut);
        assert_eq!(super::settle(&plan, &moves, Some(1), true), Some((3, 0)));
        assert_eq!((text("a.jpg"), text("b.jpg")), ("a".into(), "b".into()));
        assert!(!p(".tmp-0").exists() && !p(".tmp-1").exists() && !plan.exists());
        std::fs::remove_file(p("a.jpg")).unwrap();
        std::fs::remove_file(p("b.jpg")).unwrap();
        for undo in [true, false] {
            // Two steps done, one counted.
            std::fs::write(p(".tmp-0"), "a").unwrap();
            std::fs::write(p(".tmp-1"), "b").unwrap();
            let plan = plans.join("cut");
            let mut cut = super::begin(&plans, &moves).unwrap();
            cut.done();
            std::fs::copy(&cut.path, &plan).unwrap();
            drop(cut);
            let (a, b) = match undo {
                true => ("a", "b"),
                false => ("b", "a"),
            };
            assert_eq!(super::settle(&plan, &moves, Some(1), undo), Some((2, 0)));
            assert_eq!((text("a.jpg"), text("b.jpg")), (a.into(), b.into()));
            assert!(!p(".tmp-0").exists() && !p(".tmp-1").exists() && !plan.exists());
            std::fs::remove_file(p("a.jpg")).unwrap();
            std::fs::remove_file(p("b.jpg")).unwrap();
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A pipe in the plans folder is passed over, not opened: opening it would hold Glimt's
    /// start for good.
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
        assert!(super::interrupted(&dir).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A plan locked by a running rename is not taken for a cut-short one, nor written over by a
    /// second; one left by a crash is, and its files get their old names back, or their new ones.
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
        let running = super::begin(&plans, &moves).unwrap();
        // A second one has a plan of its own; neither is taken for one cut short.
        let second = super::begin(&plans, &moves[..1]).unwrap();
        assert_ne!(running.path, second.path);
        assert!(super::interrupted(&plans).is_empty());
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
        // Cut short after two steps: both files under hidden names.
        std::fs::write(p(".tmp-0"), "a").unwrap();
        std::fs::write(p(".tmp-1"), "b").unwrap();
        let mut cut = super::begin(&plans, &moves).unwrap();
        cut.done();
        cut.done();
        let plan = cut.path.clone();
        std::mem::forget(cut);
        // As after a crash: the lock is gone with the process.
        let found = {
            std::fs::copy(&plan, plans.join("crashed")).unwrap();
            super::interrupted(&plans)
        };
        let (path, read, done) = found
            .into_iter()
            .find(|f| f.0.ends_with("crashed"))
            .unwrap();
        assert_eq!((&read, done), (&moves, Some(2)));
        let text = |n| std::fs::read_to_string(p(n)).unwrap();
        // Cut short between the link and the unlink of the third step: b.jpg and .tmp-0 are one
        // file. Finished, the old name goes; nothing is left to ask about again.
        std::fs::hard_link(p(".tmp-0"), p("b.jpg")).unwrap();
        std::fs::copy(&path, plans.join("linked")).unwrap();
        assert_eq!(
            super::settle(&plans.join("linked"), &read, Some(2), false),
            Some((2, 0))
        );
        assert_eq!((text("a.jpg"), text("b.jpg")), ("b".into(), "a".into()));
        assert!(!p(".tmp-0").exists());
        std::fs::rename(p("a.jpg"), p(".tmp-1")).unwrap();
        std::fs::rename(p("b.jpg"), p(".tmp-0")).unwrap();
        // The same, put back: the new name of the half-done step goes, then the two before.
        std::fs::hard_link(p(".tmp-0"), p("b.jpg")).unwrap();
        std::fs::copy(&path, plans.join("linked")).unwrap();
        assert_eq!(
            super::settle(&plans.join("linked"), &read, Some(2), true),
            Some((3, 0))
        );
        assert_eq!((text("a.jpg"), text("b.jpg")), ("a".into(), "b".into()));
        std::fs::rename(p("a.jpg"), p(".tmp-0")).unwrap();
        std::fs::rename(p("b.jpg"), p(".tmp-1")).unwrap();
        // While another window puts it right, this one does nothing.
        let other = std::fs::File::open(&path).unwrap();
        other.lock().unwrap();
        assert_eq!(super::settle(&path, &read, done, true), None);
        drop(other);
        assert_eq!(super::settle(&path, &read, done, true), Some((2, 0)));
        assert_eq!((text("a.jpg"), text("b.jpg")), ("a".into(), "b".into()));
        assert!(!path.exists());
        // One that opened it before it was deleted finds it has no name left.
        std::fs::write(&path, "x").unwrap();
        let opened = std::fs::File::open(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert!(!super::linked(&opened));
        // A second window that asked about it too finds it gone, and does nothing.
        assert_eq!(super::settle(&path, &read, done, true), None);
        assert_eq!((text("a.jpg"), text("b.jpg")), ("a".into(), "b".into()));
        // Finishing instead gives the new names.
        std::fs::rename(p("a.jpg"), p(".tmp-0")).unwrap();
        std::fs::rename(p("b.jpg"), p(".tmp-1")).unwrap();
        std::fs::copy(&plan, plans.join("crashed")).unwrap();
        assert_eq!(
            super::settle(&plans.join("crashed"), &moves, Some(2), false),
            Some((2, 0))
        );
        assert_eq!((text("a.jpg"), text("b.jpg")), ("b".into(), "a".into()));
        // Cut short after three steps, where the names alone look like after one: finished, the
        // swap is whole and the plan gone, so it is not done a second time; or undone.
        let after_three = || {
            std::fs::write(p(".tmp-1"), "b").unwrap();
            std::fs::write(p("b.jpg"), "a").unwrap();
            std::fs::remove_file(p("a.jpg")).unwrap();
            std::fs::copy(&plan, plans.join("crashed")).unwrap();
        };
        after_three();
        assert_eq!(
            super::settle(&plans.join("crashed"), &moves, Some(3), false),
            Some((1, 0))
        );
        assert_eq!((text("a.jpg"), text("b.jpg")), ("b".into(), "a".into()));
        assert!(!plans.join("crashed").exists());
        after_three();
        assert_eq!(
            super::settle(&plans.join("crashed"), &moves, Some(3), true),
            Some((3, 0))
        );
        assert_eq!((text("a.jpg"), text("b.jpg")), ("a".into(), "b".into()));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Every place a Rename Many can be cut short, also between the link and the unlink of one
    /// step, for a swap, a cycle of three, a shift and plain new names: put back gives every
    /// old name, finished every new one, no hidden name is left, and a second settle does
    /// nothing.
    /// Damaged plans are read without a panic, and never count more steps done than they have.
    #[test]
    fn survives_broken_plans() {
        let seed = b"counted\0/p/a\0/p/.t-0\0/p/b\0/p/.t-1\0/p/.t-0\0/p/b\0\0\0";
        for bytes in crate::broken(seed) {
            if let Some((moves, Some(done))) = super::parse(&bytes) {
                assert!(done <= moves.len());
            }
        }
    }

    /// A plan kept when its renames could not be undone is there at the next start, with the
    /// steps it had done.
    #[test]
    fn a_kept_plan_stays() {
        let dir = std::env::temp_dir().join(format!("glimt-test-kept-{}", std::process::id()));
        let moves = vec![(dir.join("a"), dir.join("b"))];
        let mut plan = super::begin(&dir, &moves).unwrap();
        plan.done();
        plan.keep();
        let left = super::interrupted(&dir);
        assert_eq!((left.len(), left[0].2), (1, Some(1)));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The failing path and the recovery share one undo: two steps of a swap done, undone last
    /// A swap's steps followed to their ends: a goes to b and b to a; a plain rename is itself.
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
    }

    /// first; one of them put back by hand already is left out, not counted as stuck.
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
        // Two done, one of them put back by hand: only the other is undone.
        std::fs::rename(p("a"), p(".t0")).unwrap();
        std::fs::rename(p("b"), p(".t1")).unwrap();
        std::fs::rename(p(".t1"), p("b")).unwrap();
        assert_eq!(super::undo(&moves, 2), (1, 0));
        assert!(p("a").exists() && p("b").exists() && !p(".t0").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn settles_every_cut() {
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
            // As rename_many plans it: through hidden names when a new name is an old one.
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
                        let mut plan = super::begin(&plans, &moves).unwrap();
                        for (a, b) in &moves[..cut] {
                            std::fs::rename(a, b).unwrap();
                            plan.done();
                        }
                        if half {
                            std::fs::hard_link(&moves[cut].0, &moves[cut].1).unwrap();
                        }
                        // As after a crash: the plan on disk, no longer locked.
                        let bytes = std::fs::read(&plan.path).unwrap();
                        drop(plan);
                        std::fs::write(plans.join("crashed"), bytes).unwrap();
                        let [(path, read, done)] = &super::interrupted(&plans)[..] else {
                            panic!("one plan");
                        };
                        assert_eq!((read, *done), (&moves, Some(cut)));
                        let (_, left) = super::settle(path, read, *done, undo).unwrap();
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
                        assert_eq!(super::settle(path, read, *done, undo), None, "{case}");
                    }
                }
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
