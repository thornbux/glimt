//! The backup (File → Back Up, glimt --back-up): Glimt's own folders of settings, marks and
//! stars, collections, plugins and decoders, copied into a new folder.

use crate::files::config::xdg_dir;
use crate::images::save::{FileId, file_id};
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

/// Copies the folder `from` with everything in it to `to`, made. Returns how many files were
/// copied. A link is copied as the file or folder it points to, so a themes folder that is a
/// link into a dotfiles repository is backed up too. A link that leads nowhere is left out.
pub fn copy_tree(from: &Path, to: &Path) -> std::io::Result<usize> {
    std::fs::create_dir_all(to)?;
    let mut above = Vec::from_iter(file_id(from));
    copy_into(from, to, &ids_above(to), &mut above)
}

/// Copies what is in `from` to `to`, made. `target` holds the folders the copy goes into, and
/// `above` the folders being copied that hold `from`, by `file_id`.
#[allow(clippy::disallowed_methods)] // copies into a backup folder made new for it
fn copy_into(
    from: &Path,
    to: &Path,
    target: &[FileId],
    above: &mut Vec<FileId>,
) -> std::io::Result<usize> {
    std::fs::create_dir_all(to)?;
    let mut files = 0;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let (source, dest) = (entry.path(), to.join(entry.file_name()));
        if source.is_dir() {
            let Some(id) = file_id(&source) else {
                continue;
            };
            // A link to a folder that holds the backup would copy the backup into itself.
            if target.contains(&id) {
                return Err(std::io::Error::other(crate::tf!(
                    "A backup cannot go inside {folder}, which it copies",
                    folder = source.display()
                )));
            }
            // A link back up to a folder being copied would copy it inside itself forever.
            if above.contains(&id) {
                continue;
            }
            above.push(id);
            let copied = copy_into(&source, &dest, target, above);
            above.pop();
            files += copied?;
        } else if source.is_file() {
            // Named, as the files after it are not copied.
            std::fs::copy(&source, &dest)
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
    let target = ids_above(dir);
    let inside_own = |d: &Path| file_id(d).is_some_and(|id| target.contains(&id));
    if let Some((_, inside)) = own.iter().find(|(_, d)| inside_own(d)) {
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
    #[allow(clippy::disallowed_methods)] // the backup's own copy, just made
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

/// Returns the folders that hold `path`, itself among them, by `file_id`: those above it
/// as it is written, and those above the place it really is when a link leads there.
fn ids_above(path: &Path) -> Vec<FileId> {
    let written = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
    let real = path.canonicalize().unwrap_or_else(|_| written.clone());
    (written.ancestors().chain(real.ancestors()))
        .filter_map(file_id)
        .collect()
}

#[cfg(test)]
mod tests {
    /// A backup copy: every file of the tree, at the same place below the new folder, a folder
    /// through a link too, but never a folder above it again.
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
        // A link to a file is copied as its file, a link to a folder outside as its folder, and
        // a link to a folder above it is left out.
        std::os::unix::fs::symlink(&from, from.join("collections/sub/up")).unwrap();
        std::os::unix::fs::symlink("../a.gqv", from.join("collections/sub/a.gqv")).unwrap();
        let _ = std::fs::remove_dir_all(dir.join("to"));
        assert_eq!(super::copy_tree(&from, &dir.join("to")).unwrap(), 4);
        assert!(dir.join("to/collections/sub/a.gqv").is_file());
        assert!(!dir.join("to/collections/sub/up").exists());
        std::fs::create_dir_all(dir.join("dotfiles/themes")).unwrap();
        std::fs::write(dir.join("dotfiles/themes/my.theme"), "").unwrap();
        std::os::unix::fs::symlink(dir.join("dotfiles/themes"), from.join("themes")).unwrap();
        let _ = std::fs::remove_dir_all(dir.join("to"));
        assert_eq!(super::copy_tree(&from, &dir.join("to")).unwrap(), 5);
        assert!(dir.join("to/themes/my.theme").is_file());
        // A backup into the folder a link leads to would copy itself, and is refused.
        assert!(super::copy_tree(&from, &dir.join("dotfiles/themes/backup")).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
