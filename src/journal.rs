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
    // shortcut: not synced, which a crash of Glimt does not need; a power cut may lose the last
    // steps' bytes, and then the existence checks in `settle` are what is left.
    pub fn done(&mut self) {
        let _ = self.file.write_all(&[0]);
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
    let path = dir.join(crate::remote::instance_id());
    // Emptied only once locked: a plan this window is still carrying out is never wiped.
    let mut file = (std::fs::OpenOptions::new().write(true).create(true))
        .truncate(false)
        .open(&path)
        .ok()?;
    file.try_lock().ok()?;
    file.set_len(0).ok()?;
    let mut bytes = [COUNTED, b"\0"].concat();
    for (a, b) in moves {
        for p in [a, b] {
            bytes.extend(p.as_os_str().as_bytes());
            bytes.push(0);
        }
    }
    file.write_all(&bytes).and_then(|_| file.sync_all()).ok()?;
    Some(Journal { path, file })
}

/// The plans in `dir` of renames that were cut short: those no running window holds locked.
pub fn interrupted(dir: &Path) -> Vec<CutShort> {
    let entries = std::fs::read_dir(dir).into_iter().flatten().flatten();
    entries
        .filter_map(|e| {
            let mut file = File::open(e.path()).ok()?;
            file.try_lock().ok()?;
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).ok()?;
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
            Some((e.path(), moves, done))
        })
        .collect()
}

/// Put the old names back, or finish the renames: the `done` steps undone, or the rest done, each
/// whose target name is free; for a plan that did not count them, every step, by the names
/// alone. (renamed, left); the plan goes when nothing is left, else it is asked about again at
/// the next start.
pub fn settle(
    plan: &Path,
    moves: &[(PathBuf, PathBuf)],
    done: Option<usize>,
    undo: bool,
) -> (usize, usize) {
    let (mut renamed, mut left) = (0, 0);
    let (before, after) = match done {
        Some(done) => moves.split_at(done.min(moves.len())),
        None => (moves, moves),
    };
    let steps: Vec<(&PathBuf, &PathBuf)> = match undo {
        true => before.iter().rev().map(|(a, b)| (b, a)).collect(),
        false => after.iter().map(|(a, b)| (a, b)).collect(),
    };
    for (a, b) in steps {
        if !a.exists() {
            continue;
        }
        match !b.exists() && std::fs::rename(a, b).is_ok() {
            true => renamed += 1,
            false => left += 1,
        }
    }
    if left == 0 {
        let _ = std::fs::remove_file(plan);
    }
    (renamed, left)
}

#[cfg(test)]
mod tests {
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
        assert!(super::interrupted(&plans).is_empty());
        // A second one while it runs is refused, and the plan stays as it was.
        assert!(super::begin(&plans, &moves[..1]).is_none());
        let plan_now = std::fs::read(plans.join(crate::remote::instance_id())).unwrap();
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
        std::mem::forget(cut);
        let plan = plans.join(crate::remote::instance_id());
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
        assert_eq!(super::settle(&path, &read, done, true), (2, 0));
        let text = |n| std::fs::read_to_string(p(n)).unwrap();
        assert_eq!((text("a.jpg"), text("b.jpg")), ("a".into(), "b".into()));
        assert!(!path.exists());
        // Finishing instead gives the new names.
        std::fs::rename(p("a.jpg"), p(".tmp-0")).unwrap();
        std::fs::rename(p("b.jpg"), p(".tmp-1")).unwrap();
        std::fs::copy(&plan, plans.join("crashed")).unwrap();
        assert_eq!(
            super::settle(&plans.join("crashed"), &moves, Some(2), false),
            (2, 0)
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
            (1, 0)
        );
        assert_eq!((text("a.jpg"), text("b.jpg")), ("b".into(), "a".into()));
        assert!(!plans.join("crashed").exists());
        after_three();
        assert_eq!(
            super::settle(&plans.join("crashed"), &moves, Some(3), true),
            (3, 0)
        );
        assert_eq!((text("a.jpg"), text("b.jpg")), ("a".into(), "b".into()));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
