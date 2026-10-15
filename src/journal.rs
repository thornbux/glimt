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

pub fn dir() -> Option<PathBuf> {
    crate::config::xdg_dir("XDG_STATE_HOME", ".local/state", "glimt").map(|d| d.join("renames"))
}

/// The plan of a Rename Many being carried out; removed when dropped, as the renames are done
/// or undone. Left on disk only when Glimt stops halfway.
pub struct Journal {
    path: PathBuf,
    _file: File,
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
    let mut file = File::create(&path).ok()?;
    file.try_lock().ok()?;
    let mut bytes = Vec::new();
    for (a, b) in moves {
        for p in [a, b] {
            bytes.extend(p.as_os_str().as_bytes());
            bytes.push(0);
        }
    }
    file.write_all(&bytes).and_then(|_| file.sync_all()).ok()?;
    Some(Journal { path, _file: file })
}

/// The plans in `dir` of renames that were cut short: those no running window holds locked.
pub fn interrupted(dir: &Path) -> Vec<(PathBuf, Moves)> {
    let entries = std::fs::read_dir(dir).into_iter().flatten().flatten();
    entries
        .filter_map(|e| {
            let mut file = File::open(e.path()).ok()?;
            file.try_lock().ok()?;
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).ok()?;
            let paths: Vec<PathBuf> = (bytes.split(|&b| b == 0))
                .filter(|p| !p.is_empty())
                .map(|p| PathBuf::from(OsStr::from_bytes(p)))
                .collect();
            let moves = (paths.as_chunks::<2>().0.iter()).map(|[a, b]| (a.clone(), b.clone()));
            Some((e.path(), moves.collect()))
        })
        .collect()
}

/// Put the old names back, or finish the renames: every step not yet done (or undone) whose
/// target name is free, in the order that works. (renamed, left); the plan goes when nothing is
/// left, else it is asked about again at the next start.
pub fn settle(plan: &Path, moves: &[(PathBuf, PathBuf)], undo: bool) -> (usize, usize) {
    let (mut renamed, mut left) = (0, 0);
    let steps: Vec<(&PathBuf, &PathBuf)> = match undo {
        true => moves.iter().rev().map(|(a, b)| (b, a)).collect(),
        false => moves.iter().map(|(a, b)| (a, b)).collect(),
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
    /// A plan locked by a running rename is not taken for a cut-short one; one left by a crash
    /// is, and its files get their old names back, or their new ones.
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
        drop(running);
        assert!(std::fs::read_dir(&plans).unwrap().next().is_none());
        // Cut short after two steps: both files under hidden names.
        std::fs::write(p(".tmp-0"), "a").unwrap();
        std::fs::write(p(".tmp-1"), "b").unwrap();
        std::mem::forget(super::begin(&plans, &moves).unwrap());
        let plan = plans.join(crate::remote::instance_id());
        // As after a crash: the lock is gone with the process.
        let found = {
            std::fs::copy(&plan, plans.join("crashed")).unwrap();
            super::interrupted(&plans)
        };
        let (path, read) = found
            .into_iter()
            .find(|f| f.0.ends_with("crashed"))
            .unwrap();
        assert_eq!(read, moves);
        assert_eq!(super::settle(&path, &read, true), (2, 0));
        let text = |n| std::fs::read_to_string(p(n)).unwrap();
        assert_eq!((text("a.jpg"), text("b.jpg")), ("a".into(), "b".into()));
        assert!(!path.exists());
        // Finishing instead gives the new names.
        std::fs::rename(p("a.jpg"), p(".tmp-0")).unwrap();
        std::fs::rename(p("b.jpg"), p(".tmp-1")).unwrap();
        std::fs::copy(&plan, plans.join("crashed")).unwrap();
        assert_eq!(super::settle(&plans.join("crashed"), &moves, false), (2, 0));
        assert_eq!((text("a.jpg"), text("b.jpg")), ("b".into(), "a".into()));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
