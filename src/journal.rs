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
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .ok()?;
    file.try_lock().ok()?;
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
    // One window at a time, and only while the plan is there: a second window that asked about
    // it too would do the same steps again, on files already put right.
    let Some(_held) = File::open(plan).ok().filter(|f| f.try_lock().is_ok()) else {
        return (0, 0);
    };
    let (mut renamed, mut left) = (0, 0);
    // Put back: the step cut short between its link and its unlink, the first not counted, has
    // its new name taken away first, so the steps before it can be undone.
    if undo
        && let Some((a, b)) = done.and_then(|d| moves.get(d))
        && same_file(a, b)
        && std::fs::remove_file(b).is_ok()
    {
        renamed += 1;
    }
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
        // Cut short between the link and the unlink of a move: both names are the file, and
        // only the old one is to go.
        if same_file(a, b) {
            match std::fs::remove_file(a) {
                Ok(()) => renamed += 1,
                Err(_) => left += 1,
            }
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
            (2, 0)
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
            (3, 0)
        );
        assert_eq!((text("a.jpg"), text("b.jpg")), ("a".into(), "b".into()));
        std::fs::rename(p("a.jpg"), p(".tmp-0")).unwrap();
        std::fs::rename(p("b.jpg"), p(".tmp-1")).unwrap();
        // While another window puts it right, this one does nothing.
        let other = std::fs::File::open(&path).unwrap();
        other.lock().unwrap();
        assert_eq!(super::settle(&path, &read, done, true), (0, 0));
        drop(other);
        assert_eq!(super::settle(&path, &read, done, true), (2, 0));
        assert_eq!((text("a.jpg"), text("b.jpg")), ("a".into(), "b".into()));
        assert!(!path.exists());
        // A second window that asked about it too finds it gone, and does nothing.
        assert_eq!(super::settle(&path, &read, done, true), (0, 0));
        assert_eq!((text("a.jpg"), text("b.jpg")), ("a".into(), "b".into()));
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

    /// Every place a Rename Many can be cut short, also between the link and the unlink of one
    /// step, for a swap, a cycle of three, a shift and plain new names: put back gives every
    /// old name, finished every new one, no hidden name is left, and a second settle does
    /// nothing.
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
                        let (_, left) = super::settle(path, read, *done, undo);
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
                        assert_eq!(super::settle(path, read, *done, undo), (0, 0), "{case}");
                    }
                }
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
