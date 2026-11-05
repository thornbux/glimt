//! The backup (File → Back Up, glimt --back-up): Glimt's own folders of settings, marks and
//! stars, collections, plugins and decoders, copied into a new folder.

use crate::files::config::xdg_dir;
use crate::support::format::{count, local_time};
use crate::support::lang::tr;
use std::path::{Path, PathBuf};

/// Glimt's own folders, to back up, with their names in a backup: its settings, keyword tree,
/// sort folders and themes (config), marks and stars (state), and collections, plugins and
/// decoders (data). Thumbnails are not: they are made again.
pub fn own_folders() -> Vec<(&'static str, PathBuf)> {
    [
        ("config", xdg_dir("XDG_CONFIG_HOME", ".config", "glimt")),
        ("state", xdg_dir("XDG_STATE_HOME", ".local/state", "glimt")),
        ("data", xdg_dir("XDG_DATA_HOME", ".local/share", "glimt")),
    ]
    .into_iter()
    .filter_map(|(name, dir)| Some((name, dir?)))
    .filter(|(_, dir)| dir.is_dir())
    .collect()
}

/// Copy the folder `from` with everything in it to `to`, made; how many files. Links are
/// copied as the files they point to; one that leads nowhere is left out.
#[allow(clippy::disallowed_methods)] // copies into a backup folder made new for it
pub fn copy_tree(from: &Path, to: &Path) -> std::io::Result<usize> {
    std::fs::create_dir_all(to)?;
    let mut files = 0;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let (source, target) = (entry.path(), to.join(entry.file_name()));
        if source.is_dir() {
            files += copy_tree(&source, &target)?;
        } else if source.is_file() {
            // Named, as the files after it are not copied.
            std::fs::copy(&source, &target)
                .map_err(|e| std::io::Error::new(e.kind(), format!("{}: {e}", source.display())))?;
            files += 1;
        }
    }
    Ok(files)
}

/// Glimt's own folders copied to a new folder glimt-backup-DATE in `dir`: what was done, or
/// what went wrong.
pub(crate) fn back_up_to(dir: &Path) -> Result<String, String> {
    // A mistyped or unmounted folder is not made: the backup would land where no one looks.
    if !dir.is_dir() {
        return Err(crate::tf!(
            "{folder} is not a folder",
            folder = dir.display()
        ));
    }
    let own = own_folders();
    if own.is_empty() {
        return Err(
            tr("Nothing to back up: Glimt has no settings, marks or collections here").into(),
        );
    }
    // Inside one of the folders it copies, the backup would copy itself until paths are too long.
    let real = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_owned());
    if let Some((_, inside)) = own.iter().find(|(_, d)| real(dir).starts_with(real(d))) {
        return Err(crate::tf!(
            "A backup cannot go inside {folder}, which it copies",
            folder = inside.display()
        ));
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
    let date: String = local_time(now.map_or(0, |d| d.as_secs() as i64))
        .chars()
        .take(10)
        .collect();
    // A second backup the same day goes beside the first, never over it.
    let to = (1..)
        .map(|n| match n {
            1 => dir.join(format!("glimt-backup-{date}")),
            n => dir.join(format!("glimt-backup-{date}-{n}")),
        })
        .find(|p| !p.exists())
        .unwrap_or_default();
    let copied: std::io::Result<usize> = (own.into_iter())
        .map(|(name, from)| copy_tree(&from, &to.join(name)))
        .sum();
    // Not the plan of a Rename Many running now: restored later, it would rename files again.
    let _ = std::fs::remove_dir_all(to.join("state/renames"));
    match copied {
        Ok(n) => Ok(crate::tf!(
            "Glimt's data backed up to {folder}: {files}",
            folder = to.display(),
            files = count(n, "file")
        )),
        Err(e) => Err(crate::tf!(
            "The backup to {folder} is not whole: {e}",
            folder = to.display(),
            e
        )),
    }
}

#[cfg(test)]
mod tests {
    /// A backup copy: every file of the tree, at the same place below the new folder.
    #[test]
    fn copies_a_tree() {
        let dir = std::env::temp_dir().join(format!("glimt-test-tree-{}", std::process::id()));
        let from = dir.join("from");
        std::fs::create_dir_all(from.join("collections/sub")).unwrap();
        std::fs::write(from.join("config"), "theme=Dark").unwrap();
        std::fs::write(from.join("collections/a.gqv"), "#GQview").unwrap();
        std::fs::write(from.join("collections/sub/b.gqv"), "").unwrap();
        assert_eq!(super::copy_tree(&from, &dir.join("to")).unwrap(), 3);
        let text = |p: &str| std::fs::read_to_string(dir.join("to").join(p)).unwrap();
        assert_eq!(
            (text("config"), text("collections/a.gqv")),
            ("theme=Dark".into(), "#GQview".into())
        );
        assert!(dir.join("to/collections/sub/b.gqv").is_file());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
