//! The backup (File → Back Up, glimt --back-up): Glimt's own folders of settings, marks and
//! stars, collections, tools and added formats, copied into a new folder.

use crate::files::config::xdg_dir;
use crate::files::file_io::{is_surely_dir, is_surely_gone};
use crate::images::save::{FileId, file_id};
use crate::support::format::{count, local_time};
use crate::support::lang::tr;
use std::path::{Path, PathBuf};

/// Glimt's own folders, to back up, with their names in a backup: its settings, keyword tree,
/// sort folders and themes (config), marks and stars (state), and collections, tools and
/// added formats (data). Thumbnails are not: they are made again. A folder that could not be
/// looked at is kept, so that the backup fails and says so rather than leave it out.
pub fn own_folders() -> Vec<(&'static str, PathBuf)> {
    [
        ("config", xdg_dir("XDG_CONFIG_HOME", ".config", "glimt")),
        ("state", xdg_dir("XDG_STATE_HOME", ".local/state", "glimt")),
        ("data", xdg_dir("XDG_DATA_HOME", ".local/share", "glimt")),
    ]
    .into_iter()
    .filter_map(|(name, dir)| Some((name, dir?)))
    .filter(|(_, dir)| !is_surely_gone(dir))
    .collect()
}

/// Copies the folder `from` with everything in it to `to`, made, and on disk when it returns:
/// a backup is often made to a USB stick that is pulled out right after. Returns how many files
/// were copied. A link is copied as the file or folder it points to, so a themes folder that is a
/// link into a dotfiles repository is backed up too. A link that leads nowhere is left out.
pub fn copy_tree(from: &Path, to: &Path) -> std::io::Result<usize> {
    std::fs::create_dir_all(to)?;
    let mut above = Vec::from_iter(file_id(from));
    let copied = copy_into(from, to, &ids_above(to), &mut above)?;
    // The name of `to` itself is on disk too.
    crate::files::file_io::sync_folder_of(to)?;
    Ok(copied)
}

/// Copies what is in `from` to `to`, made. `target` holds the folders the copy goes into, and
/// `above` the folders being copied that hold `from`, by `file_id`.
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
        // Through a link. One that leads nowhere is left out, and a look that failed otherwise
        // fails the backup, which would else miss the file or folder without a word.
        let meta = match std::fs::metadata(&source) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                return Err(std::io::Error::new(
                    e.kind(),
                    format!("{}: {e}", source.display()),
                ));
            }
            Ok(meta) => meta,
        };
        if meta.is_dir() {
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
        } else if meta.is_file() {
            // Named, as the files after it are not copied.
            crate::files::file_io::put_file(&source, &dest, true)
                .map_err(|e| std::io::Error::new(e.kind(), format!("{}: {e}", source.display())))?;
            files += 1;
        }
    }
    // The names of the files and folders in it are on disk, with one sync for the folder.
    crate::files::file_io::sync_folder(to)?;
    Ok(files)
}

/// Glimt's own folders copied to a new folder glimt-backup-DATE in `dir`: what was done, or
/// what went wrong.
pub(crate) fn back_up_to(dir: &Path) -> Result<String, String> {
    // A mistyped or unmounted folder is not made: the backup would land where no one looks.
    if !is_surely_dir(dir) {
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
    let to = new_backup_folder(dir, &date).map_err(|e| {
        crate::tf!(
            "The backup to {folder} is not whole: {e}",
            folder = dir.display(),
            e
        )
    })?;
    let copied: std::io::Result<usize> = (own.into_iter())
        .map(|(name, from)| copy_tree(&from, &to.join(name)))
        .sum();
    // The backup folder's name in `dir` is on disk too.
    let copied = copied.and_then(|n| crate::files::file_io::sync_folder_of(&to).map(|_| n));
    // Not the plan of a Rename Many running now: restored later, it would rename files again.
    // So a copy of it that could not be removed makes the backup not whole.
    let removed = crate::files::file_io::remove_own(&to.join("state/renames"));
    let copied = copied.and_then(|n| removed.map(|()| n));
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

/// Makes the folder glimt-backup-DATE in `dir`, or glimt-backup-DATE-2 and so on when the name
/// is taken. A second backup the same day goes beside the first, never over it. The name is
/// taken by making the folder, so two backups at the same moment get a folder each. Returns
/// the folder made.
fn new_backup_folder(dir: &Path, date: &str) -> std::io::Result<PathBuf> {
    for n in 1.. {
        let to = match n {
            1 => dir.join(format!("glimt-backup-{date}")),
            n => dir.join(format!("glimt-backup-{date}-{n}")),
        };
        match std::fs::create_dir(&to) {
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            made => return made.map(|_| to),
        }
    }
    unreachable!("a backup folder is found before the numbers run out")
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
    /// A backup fails, and says what it could not look at, rather than leave a file or folder
    /// out without a word. A link that loops stands for a share that did not answer, as both
    /// fail with another error than NotFound. A link that leads nowhere is still left out.
    #[test]
    fn a_backup_fails_on_what_it_cannot_look_at() {
        let dir = std::env::temp_dir().join(format!("glimt-test-tree-look-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let from = dir.join("from");
        std::fs::create_dir_all(&from).unwrap();
        std::os::unix::fs::symlink("nowhere", from.join("dangling")).unwrap();
        assert_eq!(super::copy_tree(&from, &dir.join("to")).unwrap(), 0);
        std::os::unix::fs::symlink("loop", from.join("loop")).unwrap();
        let error = super::copy_tree(&from, &dir.join("to2")).unwrap_err();
        assert!(error.to_string().contains("loop"), "{error}");
        std::fs::remove_dir_all(dir).unwrap();
    }

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
        crate::files::file_io::FOLDERS_SYNCED.take();
        assert_eq!(super::copy_tree(&from, &dir.join("to")).unwrap(), 3);
        // Every file and folder copied is on disk with its name, as a USB stick may be pulled
        // out right after. Each folder is synced once, not once for each file in it.
        let synced = crate::files::file_io::FOLDERS_SYNCED.take();
        let folders = ["to/collections/sub", "to/collections", "to", ""];
        assert_eq!(synced, folders.map(|f| dir.join(f)));
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

    /// Two backups on the same day each get a folder of their own, also when neither has
    /// copied a file into its folder yet.
    #[test]
    fn each_backup_gets_a_folder_of_its_own() {
        let dir = std::env::temp_dir().join(format!("glimt-test-backups-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = super::new_backup_folder(&dir, "2026-11-08").unwrap();
        let second = super::new_backup_folder(&dir, "2026-11-08").unwrap();
        assert_eq!(first, dir.join("glimt-backup-2026-11-08"));
        assert_eq!(second, dir.join("glimt-backup-2026-11-08-2"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
