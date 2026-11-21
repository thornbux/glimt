//! The file list: which files are images (by extension and file type), reading a folder or a tree
//! of them, and sorting them (by name as numbers, date, size, EXIF date or shuffled).

use crate::files::external::{is_heif, is_video};
use crate::files::file_io::Identity;
use crate::images::save::FileId;
use crate::support::threads::lock_even_if_poisoned;
use crate::{images::raw, metadata::exif};
use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub const EXTS: &[&str] = &[
    "jpg", "jpeg", "png", "webp", "gif", "tif", "tiff", "bmp", "ico", "qoi", "pbm", "pgm", "ppm",
    "pnm", "pam", "hdr", "lsm", "gel",
];

/// TIFF under other names: Zeiss LSM microscopy and GE Typhoon gels (square-root coded, shown as
/// stored). Only their first page: an LSM file's others are thumbnails.
pub const TIFF_LIKE: &[&str] = &["tif", "tiff", "lsm", "gel"];

/// HEIC/HEIF photos (iPhone), decoded by ffmpeg with the heic feature.
pub const HEIC_EXTS: &[&str] = if cfg!(feature = "heic") {
    &["heic", "heif", "hif"]
} else {
    &[]
};

/// AVIF photos, decoded by ffmpeg (dav1d) with the avif feature.
pub const AVIF_EXTS: &[&str] = if cfg!(feature = "avif") {
    &["avif"]
} else {
    &[]
};

/// JPEG XL photos, with the jxl feature.
pub const JXL_EXTS: &[&str] = if cfg!(feature = "jxl") { &["jxl"] } else { &[] };

/// SVG images, with the svg feature.
pub const SVG_EXTS: &[&str] = if cfg!(feature = "svg") {
    &["svg", "svgz"]
} else {
    &[]
};

/// Each order with its config value and menu label.
pub const SORTS: [(Sort, &str, &str); 8] = [
    (Sort::Name, "name", "Sort by name"),
    (Sort::Number, "number", "Sort by number"),
    (Sort::Date, "date", "Sort by date"),
    (Sort::Created, "created", "Sort by file creation date"),
    (
        Sort::ExifOriginal,
        "exif_original",
        "Sort by EXIF date original",
    ),
    (
        Sort::ExifDigitized,
        "exif_digitized",
        "Sort by EXIF date digitized",
    ),
    (Sort::Size, "size", "Sort by size"),
    (Sort::Extension, "extension", "Sort by extension"),
];

/// How long Glimt's own reading of a share waits after a program was started on a file
/// (`program_started`). A video player on a share reads the start of its file then, and the
/// share serves Glimt's look at each file of a tree between the player's reads.
const PROGRAM_STARTS_FOR: std::time::Duration = std::time::Duration::from_secs(3);

/// How long the walk of a tree on a share waits for the decode of an image that the window
/// waits for (`ImageAwaited`). A decode that takes longer, on a share that stopped answering,
/// no longer holds the walk.
const IMAGE_AWAITED_FOR: std::time::Duration = std::time::Duration::from_secs(5);

/// The folders where a listing found a link to a file (`holds_links`).
/// shortcut: it only grows, by one path for each such folder listed; forget a folder at its
/// next listing when a session lists very many of them.
static FOLDERS_WITH_LINKS: std::sync::Mutex<std::collections::BTreeSet<PathBuf>> =
    std::sync::Mutex::new(std::collections::BTreeSet::new());

/// When a program was last started on the user's files: Play Video, Open With or a tool.
static PROGRAM_STARTED_AT: std::sync::Mutex<Option<std::time::Instant>> =
    std::sync::Mutex::new(None);

/// When each decode of an image that the window waits for began (`ImageAwaited`).
static IMAGES_AWAITED: std::sync::Mutex<Vec<std::time::Instant>> =
    std::sync::Mutex::new(Vec::new());

#[cfg(test)]
thread_local! {
    // How many programs this thread noted as started (`program_started`), so that a test can see
    // that a way to start a program on the user's files notes it, whatever other tests start.
    pub(crate) static PROGRAMS_STARTED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The folders a listing read, each with the modification time it had when it was read, so the
/// folder watcher sees a change made after the reading. The time is `None` when it could not be
/// read.
pub type FoldersRead = Vec<(PathBuf, Option<std::time::SystemTime>)>;

/// A tree as `list_tree` reads it: the top folder's own folders, the images of the tree, the
/// folders read, and the folders below the top that could not be read.
pub type ListedTree = (Vec<PathBuf>, Vec<ListedName>, FoldersRead, Vec<Unread>);

/// A folder as `walk` reads it: its path relative to the tree, its folders, its images, and
/// its change time, taken before it was read.
pub type WalkedFolder = (
    PathBuf,
    Vec<PathBuf>,
    Vec<ListedName>,
    Option<std::time::SystemTime>,
);

/// A folder or file that a listing could not read, with why. When it is the one folder listed,
/// the listing fails, and the window tells it and keeps the list it shows. A part of the listing
/// (a folder of a tree or of several folders, an entry of a collection) is left out instead,
/// and the window tells it, unless the list shown holds files of that part.
pub type Unread = (PathBuf, std::io::Error);

/// An image's name in the list, with the identity its folder entry gave it when it was read.
/// The list keeps both, so that a write can tell when another file has taken the name since.
pub type ListedName = (OsString, Identity);

/// The file types the list shows, as Geeqie's file classes in its file filter.
#[derive(Clone, Copy)]
pub struct Types {
    pub images: bool,
    pub raw: bool,
    /// With the video feature.
    pub videos: bool,
    /// Names starting with a dot, files and folders.
    pub hidden: bool,
}

impl Types {
    /// None: for reading only the folders.
    pub const NONE: Types = Types {
        images: false,
        raw: false,
        videos: false,
        hidden: false,
    };

    /// The type of the file at `path`.
    pub fn of(path: &Path) -> Types {
        let only = |t: Types| is_listed(path, t);
        Types {
            images: only(Types {
                images: true,
                ..Types::NONE
            }),
            raw: only(Types {
                raw: true,
                ..Types::NONE
            }),
            videos: only(Types {
                videos: true,
                ..Types::NONE
            }),
            hidden: is_hidden(path),
        }
    }
}

/// File list order, as in Geeqie's sort menu.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Sort {
    Name,
    /// Numbers in names compared by value: "img2" before "img10".
    Number,
    /// Modification time.
    Date,
    Created,
    ExifOriginal,
    ExifDigitized,
    Size,
    /// File type: "jpg" before "mp4" before "png"; then by name.
    Extension,
}

impl Sort {
    pub fn row(self) -> (Sort, &'static str, &'static str) {
        SORTS.into_iter().find(|s| s.0 == self).unwrap_or(SORTS[0])
    }
}

impl std::str::FromStr for Sort {
    type Err = ();
    fn from_str(text: &str) -> Result<Self, ()> {
        SORTS
            .into_iter()
            .find(|s| s.1 == text)
            .map(|s| s.0)
            .ok_or(())
    }
}

impl std::fmt::Display for Sort {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(self.row().1)
    }
}

/// A decode of an image that the window waits for: the shown image, or the next one that the
/// slideshow or the arrow key goes to. While one lives, and for at most `IMAGE_AWAITED_FOR`,
/// the walk of a tree on a share waits between folders, so that the share reads the image
/// first. Before, the image's reads were queued behind the walk's on the same connection.
pub(crate) struct ImageAwaited {
    began: std::time::Instant,
}

impl ImageAwaited {
    /// Notes that a decode of an image that the window waits for begins now.
    pub(crate) fn begin() -> ImageAwaited {
        let began = std::time::Instant::now();
        lock_even_if_poisoned(&IMAGES_AWAITED).push(began);
        ImageAwaited { began }
    }
}

impl Drop for ImageAwaited {
    fn drop(&mut self) {
        let mut awaited = lock_even_if_poisoned(&IMAGES_AWAITED);
        if let Some(at) = awaited.iter().position(|began| *began == self.began) {
            awaited.swap_remove(at);
        }
    }
}

/// A JPEG by its name: the one test, for the scaled decode, Check Images, duplicates, the
/// orientation field and the EXIF read.
pub fn is_jpeg(path: &Path) -> bool {
    has_ext(path, &["jpg", "jpeg"])
}

pub fn has_ext(path: &Path, exts: &[&str]) -> bool {
    let ext = path.extension().and_then(|e| e.to_str());
    ext.is_some_and(|e| exts.contains(&e.to_ascii_lowercase().as_str()))
}

/// A new file name from `pattern`: * is the old name without extension, a run of # the number
/// `n` with that many digits; the old extension is kept. As bytes: an old name that is not
/// UTF-8 (Latin-1 from an old camera) keeps its letters.
pub fn pattern_name(pattern: &str, old: &Path, n: usize) -> OsString {
    let (mut out, mut chars) = (OsString::new(), pattern.chars().peekable());
    while let Some(c) = chars.next() {
        match c {
            '*' => out.push(old.file_stem().unwrap_or_default()),
            '#' => {
                let mut width = 1;
                while chars.next_if_eq(&'#').is_some() {
                    width += 1;
                }
                out.push(format!("{n:0width$}"));
            }
            c => out.push(c.encode_utf8(&mut [0; 4])),
        }
    }
    if let Some(ext) = old.extension() {
        out.push(".");
        out.push(ext);
    }
    out
}

/// Shown in the file list: an image, a RAW file or a video (video feature), of the `types` on.
pub fn is_listed(path: &Path, types: Types) -> bool {
    (types.hidden || !is_hidden(path)) && is_listed_type(path, types)
}

/// A name starting with a dot, which Geeqie and file managers hide.
pub fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|n| n.as_encoded_bytes().starts_with(b"."))
}

/// Returns why `name` cannot be the new name of a file, a folder or a collection, or `None` when
/// it can. It is refused when it is empty, `.`, `..` or has a `/`, and when it starts with a dot
/// unless `hidden_shown`, because the list hides such a name and the file would vanish from it.
/// A theme, which is not in the list, is named with `hidden_shown`.
pub fn refused_name(name: &str, hidden_shown: bool) -> Option<String> {
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
        return Some(crate::tf!("Not a valid name: \"{text}\"", text = name));
    }
    (name.starts_with('.') && !hidden_shown).then(|| {
        crate::tf!(
            "{text} starts with a dot and would be hidden: choose another name",
            text = name
        )
    })
}

fn is_listed_type(path: &Path, types: Types) -> bool {
    (types.images
        && (built_in_image(path) || crate::images::added_formats::for_path(path).is_some()))
        || (types.raw && has_ext(path, raw::EXTS))
        || (types.videos && is_video(path))
}

/// The image formats Glimt reads itself, as built.
fn built_in_image(path: &Path) -> bool {
    has_ext(path, EXTS) || is_heif(path) || has_ext(path, JXL_EXTS) || has_ext(path, SVG_EXTS)
}

/// Whether Glimt reads the format of `path` itself, as built: an added format is not asked.
pub fn built_in_format(path: &Path) -> bool {
    built_in_image(path) || has_ext(path, raw::EXTS) || is_video(path)
}

/// `dir` and every folder below it, one at a time, as a `WalkedFolder`. A folder reached again
/// through a link or a bind mount is read once. A link to a folder is followed only when the
/// folder it leads to is inside `dir`'s real path. A downloaded folder with `up -> ../..`,
/// `pics -> ~/Pictures` or `root -> /` would else list the user's own photos or the whole disk,
/// for Select All and Delete to act on.
///
/// It gives an error for a folder that it could not read. After an error for `dir` itself it
/// stops, and after one for a folder below `dir` it goes on with the rest of the tree, so a
/// damaged or locked subfolder costs only its own images. A folder below `dir` that is gone is
/// walked as empty.
pub fn walk(dir: &Path, types: Types) -> impl Iterator<Item = Result<WalkedFolder, Unread>> {
    walk_by(
        dir,
        types,
        crate::images::save::file_id,
        |p| std::fs::canonicalize(p),
        list_folder,
    )
}

/// Walks the tree as `walk` does, with `file_id` telling the folders apart, `canonical`
/// giving a link's real path and `list` reading a folder. A test can then give the ids that a
/// file system without unique inodes has, a real path that cannot be worked out, and a file
/// made while a folder is read.
fn walk_by(
    dir: &Path,
    types: Types,
    file_id: impl Fn(&Path) -> Option<FileId>,
    canonical: impl Fn(&Path) -> std::io::Result<PathBuf>,
    list: impl Fn(&Path, Types) -> std::io::Result<(Vec<PathBuf>, Vec<ListedName>)>,
) -> impl Iterator<Item = Result<WalkedFolder, Unread>> {
    let (dir, mut todo) = (dir.to_path_buf(), vec![PathBuf::new()]);
    let mut failed = false;
    let mut seen: HashSet<FileId> = HashSet::new();
    let folder_id = move |full: &Path| file_id(full).unwrap_or_else(|| Err(real_dir(full)));
    let real = real_dir(&dir);
    std::iter::from_fn(move || {
        loop {
            if failed {
                return None;
            }
            let mut rel = todo.pop()?;
            let full = dir.join(&rel);
            let is_top = rel.as_os_str().is_empty();
            // A followed link's folder is named by its real path in the tree, so every file has
            // one name, the one `--remote select`'s client gives and marks and stars are kept
            // by. Named through the link, the file was another name than its own. A folder that
            // could not be looked at may be a link, so its real path is worked out too.
            // The top folder whose real path cannot be worked out is not read, and that is
            // told, as when it cannot be read.
            if crate::files::file_io::is_link(&full).unwrap_or(true) {
                match canonical(&full) {
                    Err(e) if is_top => {
                        failed = true;
                        return Some(Err((full, e)));
                    }
                    real_path => match inside(real_path, &real) {
                        Some(real_rel) => rel = real_rel,
                        None => continue,
                    },
                }
            }
            if !seen.insert(folder_id(&full)) {
                continue;
            }
            // The time is taken before the folder is read, so the folder watcher sees a file
            // made while it is read, which the read itself may have missed.
            let modified = std::fs::metadata(&full).and_then(|m| m.modified()).ok();
            let (dirs, files) = match list(&full, types) {
                Ok(listed) => listed,
                Err(e) if !is_top && e.kind() == std::io::ErrorKind::NotFound => Default::default(),
                // The folder is named by its path in the tree, as its files would be named, so
                // the window can tell whether the list shown holds files of it.
                Err(e) => {
                    failed = is_top;
                    let at = if is_top { full } else { dir.join(&rel) };
                    return Some(Err((at, e)));
                }
            };
            // Last in, first out: pushed backwards, the folders are read in name order.
            todo.extend(
                dirs.iter()
                    .rev()
                    .filter_map(|d| d.file_name())
                    .map(|n| rel.join(n)),
            );
            return Some(Ok((rel, dirs, files, modified)));
        }
    })
}

/// Notes that a program was just started on a file, so that the reading of a share that nobody
/// waits for waits a moment for it (`wait_for_reads_that_go_first`). A program of the user's is
/// started through `external::start_users_program`, which calls it.
pub(crate) fn program_started() {
    #[cfg(test)]
    PROGRAMS_STARTED.set(PROGRAMS_STARTED.get() + 1);
    *crate::support::threads::lock_even_if_poisoned(&PROGRAM_STARTED_AT) =
        Some(std::time::Instant::now());
}

/// Returns whether a program was started on a file in the last `PROGRAM_STARTS_FOR`, as the
/// time `started` that `program_started` noted says at `now`.
fn is_starting(started: Option<std::time::Instant>, now: std::time::Instant) -> bool {
    started.is_some_and(|at| now.saturating_duration_since(at) < PROGRAM_STARTS_FOR)
}

/// Returns whether a program started on a file may still be reading its start
/// (`program_started`).
pub(crate) fn a_program_is_starting() -> bool {
    let started = *crate::support::threads::lock_even_if_poisoned(&PROGRAM_STARTED_AT);
    is_starting(started, std::time::Instant::now())
}

/// Returns whether a decode of an image that the window waits for, one of those that began
/// at the times `began`, is still running at `now` and began less than `IMAGE_AWAITED_FOR`
/// before it.
fn is_an_image_awaited(began: &[std::time::Instant], now: std::time::Instant) -> bool {
    (began.iter()).any(|at| now.saturating_duration_since(*at) < IMAGE_AWAITED_FOR)
}

/// Waits while a program started on a file may still be reading its start, or while the window
/// waits for the decode of an image (`ImageAwaited`), or until `stop` is set. The reading of a
/// tree on a share calls it between folders, so that neither a video player started on the
/// same share nor the next image is queued behind a look at each file of the tree.
fn wait_for_reads_that_go_first(stop: &std::sync::atomic::AtomicBool) {
    let now = std::time::Instant::now;
    let awaited = || is_an_image_awaited(&lock_even_if_poisoned(&IMAGES_AWAITED), now());
    while (a_program_is_starting() || awaited()) && !stop.load(std::sync::atomic::Ordering::Relaxed)
    {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Returns whether `dir` is on a share (`file_io::is_network`), where each look at a file is a
/// round trip over the network. With `with_subfolders`, it is also whether a share is mounted
/// in a folder below `dir`, such as a NAS at ~/Pictures/nas, as a walk of the tree goes into
/// it. That is one read of the mount table, not a look at each folder. A mount whose type
/// cannot be read counts as none.
// shortcut: a share mounted in a hidden folder, which the walk leaves out, counts too; look at
// the folders the walk reads if that leaves a tree's sizes uncounted for nothing.
pub(crate) fn is_on_a_share(dir: &Path, with_subfolders: bool) -> bool {
    use crate::files::{file_io::is_network, file_ops};
    file_ops::file_system_type_or_test(dir).is_some_and(|kind| is_network(&kind))
        || (with_subfolders
            && (file_ops::mounted_types_below_or_test(dir).iter()).any(|kind| is_network(kind)))
}

/// Returns the path relative to the tree whose real path is `top` of a link's folder whose real
/// path is `real`, which the walk follows. Returns `None` when the walk does not follow it: the
/// folder is outside the tree, or its real path cannot be worked out. Made absolute instead, as
/// `real_dir` does, it would always lie inside the tree, and a chain of short links to a folder
/// whose real path is longer than realpath takes would lead anywhere.
fn inside(real: std::io::Result<PathBuf>, top: &Path) -> Option<PathBuf> {
    Some(real.ok()?.strip_prefix(top).ok()?.to_path_buf())
}

/// The folders in `dir`, as `list_folder` gives them, and the images in it and every folder
/// below it, as paths relative to `dir`, in name order: `dir` read once. `top` gets `dir`'s own
/// folders as soon as they are read, and `part` the images found so far after each folder that
/// had some, to show them as the tree is read: a large tree takes long to read whole. Cut short
/// when `stop` is set: another folder was opened, and the tree would be read for nothing.
/// Returns, third, every folder of the tree that was read, `dir` first, for the folder watcher,
/// and fourth, the folders below `dir` that could not be read, which are left out. Returns an
/// error when `dir` itself could not be read.
pub fn list_tree(
    dir: &Path,
    types: Types,
    stop: &std::sync::atomic::AtomicBool,
    top: impl FnOnce(&[PathBuf]),
    mut part: impl FnMut(&[ListedName]),
) -> Result<ListedTree, Unread> {
    use std::sync::atomic::Ordering::Relaxed;
    let (mut own, mut top, mut out) = (Vec::new(), Some(top), Vec::new());
    let (mut read, mut left_out) = (Vec::new(), Vec::new());
    let on_a_share = is_on_a_share(dir, true);
    for walked in walk(dir, types).take_while(|_| !stop.load(Relaxed)) {
        let (rel, dirs, files, modified) = match walked {
            Ok(walked) => walked,
            // The walk stops after an error for `dir`, so that one is the only error that
            // comes before `dir` itself was read.
            Err(unread) if read.is_empty() => return Err(unread),
            // The folder stays watched, so a change in it lists the tree again. Its time is
            // taken as it is now, as a time unknown would be a change every second.
            Err(unread) => {
                let modified = std::fs::metadata(&unread.0).and_then(|m| m.modified());
                read.push((unread.0.clone(), modified.ok()));
                left_out.push(unread);
                continue;
            }
        };
        read.push((dir.join(&rel), modified));
        if rel.as_os_str().is_empty() {
            if let Some(top) = top.take() {
                top(&dirs);
            }
            own = dirs;
        }
        if !files.is_empty() {
            out.extend((files.into_iter()).map(|(f, id)| (rel.join(f).into_os_string(), id)));
            part(&out);
        }
        if on_a_share {
            wait_for_reads_that_go_first(stop);
        }
    }
    // In name order, as list_folder gives a single folder; sort_files builds on that.
    out.sort_unstable();
    Ok((own, out, read, left_out))
}

/// Returns `dir` in the form Glimt keeps and compares folders by: its real path, with its links
/// followed and its `..` worked out, as other programs (the trash, `--remote select`'s client)
/// record it too. A folder that is not there (renamed, or a link to it renamed) gets the real
/// path of the deepest folder above it that is, with the rest as written.
// It waits for the disk, and on a share that stopped answering it waits for ever, so it is
// called only from background threads. The window-thread check in source_rules refuses it there.
pub fn real_dir(dir: &Path) -> PathBuf {
    let Ok(dir) = std::path::absolute(dir) else {
        return dir.to_path_buf();
    };
    // Made absolute only, a folder renamed under a link (/home on /var/home) kept the link's
    // name, and the rename's old path, its real one, was another folder to a collection's entry.
    for above in dir.ancestors() {
        if let Ok(real) = std::fs::canonicalize(above) {
            let rest = dir.strip_prefix(above).unwrap_or(Path::new(""));
            return match rest.as_os_str().is_empty() {
                true => real,
                false => real.join(rest),
            };
        }
    }
    dir
}

/// Returns `path` in the form Glimt keeps and compares files by: its folder's real path
/// (`real_dir`) with its own name. The file's own link is not followed, because the list shows
/// a link under its own name.
pub fn real_path(path: &Path) -> PathBuf {
    let name = path.file_name();
    match (path.parent().filter(|p| !p.as_os_str().is_empty()), name) {
        (parent, Some(name)) => real_dir(parent.unwrap_or(Path::new("."))).join(name),
        (_, None) => real_dir(path),
    }
}

/// Returns whether a listing found a link to a file in `folder`. Its thumbnails are then never
/// set aside (`spawn_worker_pool`), because the folder's name does not tell where a link leads.
pub fn holds_links(folder: &Path) -> bool {
    lock_even_if_poisoned(&FOLDERS_WITH_LINKS).contains(folder)
}

/// Whether a folder entry is a file and whether a folder, from the entry's own type (no `stat`),
/// a link as what it points to. Only files are images: a folder named 2019.jpg is a folder, and
/// a pipe or a link to /dev/zero named a.jpg would block or fill memory when read.
pub fn kind(entry: &std::fs::DirEntry) -> (bool, bool) {
    match entry.file_type() {
        // A link that cannot be followed is neither, so it is not read as an image or folder.
        Ok(kind) if kind.is_symlink() => {
            let path = entry.path();
            let is_file = crate::files::file_io::is_surely_file(&path);
            if is_file && let Some(folder) = path.parent() {
                lock_even_if_poisoned(&FOLDERS_WITH_LINKS).insert(folder.to_owned());
            }
            (
                is_file,
                !is_file && crate::files::file_io::is_surely_dir(&path),
            )
        }
        Ok(kind) => (kind.is_file(), kind.is_dir()),
        Err(_) => (false, false),
    }
}

/// One pass over the folder: (subfolders, image file names with their identities), both sorted
/// by name. The file type and the inode come from the directory entry, so there is no `stat`
/// per file (only symlinks are resolved). A folder that is gone has nothing in it.
///
/// Returns an error when the folder could not be opened or its reading failed partway. Taken
/// as empty, or as the part read, a folder on a share that failed for a moment emptied the
/// list, and the selection with it.
pub fn list_folder(dir: &Path, types: Types) -> std::io::Result<(Vec<PathBuf>, Vec<ListedName>)> {
    use crate::images::save::identity_of_entry;
    let (mut dirs, mut images) = (Vec::new(), Vec::new());
    let entries = match std::fs::read_dir(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((dirs, images)),
        entries => entries?,
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let (file, folder) = kind(&entry);
        if file && is_listed(&path, types) {
            images.push((entry.file_name(), identity_of_entry(&entry)));
        } else if folder
            && (types.hidden || !entry.file_name().as_encoded_bytes().starts_with(b"."))
        {
            dirs.push(path);
        }
    }
    // Byte-wise comparison of the whole path gives name order for paths in one folder, and is
    // ~9× faster than Path::cmp, which re-parses the path in every comparison.
    dirs.sort_unstable_by(|a, b| a.as_os_str().cmp(b.as_os_str()));
    images.sort_unstable(); // byte-wise on Unix
    Ok((dirs, images))
}

/// Sort file names in `dir`; `case`: case-sensitive names ("B" before "a"). Everything but name
/// and number needs a stat (or EXIF) per file, so this runs in the listing thread; ties fall
/// back to the name.
pub fn sort_files(dir: &Path, files: &mut [ListedName], by: Sort, ascending: bool, case: bool) {
    let name = |(f, _): &ListedName| {
        let s = f.to_string_lossy();
        if case {
            s.into_owned()
        } else {
            s.to_lowercase()
        }
    };
    let meta = |f: &ListedName| std::fs::metadata(dir.join(&f.0)).ok();
    let secs = |t: std::io::Result<std::time::SystemTime>| {
        t.ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos())
    };
    match by {
        // Files are listed byte-wise, which is case-sensitive name order.
        Sort::Name if case => {}
        Sort::Name => files.sort_by_cached_key(name),
        Sort::Number => {
            // Each name made once, not twice in each of millions of comparisons.
            let mut keyed: Vec<(String, ListedName)> = (files.iter_mut())
                .map(|f| (name(f), std::mem::take(f)))
                .collect();
            // Equal numbers ("img010", "img10") in name order.
            keyed.sort_by(|a, b| natural_cmp(&a.0, &b.0).then_with(|| a.0.cmp(&b.0)));
            for (f, (_, k)) in files.iter_mut().zip(keyed) {
                *f = k;
            }
        }
        Sort::Date => files.sort_by_cached_key(|f| (meta(f).map(|m| secs(m.modified())), name(f))),
        Sort::Created => files.sort_by_cached_key(|f| {
            // Not every file system records creation time; then the modification time.
            let m = meta(f);
            let t = m
                .as_ref()
                .map(|m| secs(m.created().or_else(|_| m.modified())));
            (t, name(f))
        }),
        Sort::Size => files.sort_by_cached_key(|f| (meta(f).map(|m| m.len()), name(f))),
        Sort::Extension => files.sort_by_cached_key(|f| {
            let n = name(f);
            let ext = n.rsplit_once('.').map_or("", |e| e.1).to_owned();
            (ext, n)
        }),
        Sort::ExifOriginal | Sort::ExifDigitized => {
            let digitized = by == Sort::ExifDigitized;
            // Files without the date last.
            files.sort_by_cached_key(|f| {
                let date = exif::date(&dir.join(&f.0), digitized);
                (date.is_none(), date, name(f))
            })
        }
    }
    if !ascending {
        files.reverse();
    }
}

/// `before` in its order without the names gone, if `listed` has no name it lacks: a folder read
/// again after a sidecar was written keeps its EXIF date order instead of reading every file.
/// shortcut: a file whose date changed under the same name keeps its place until Refresh (F5),
/// the Sort menu or opening the folder again sorts it anew.
pub fn kept_order(before: Vec<OsString>, listed: &[ListedName]) -> Option<Vec<ListedName>> {
    let identities: std::collections::HashMap<&OsString, Identity> =
        listed.iter().map(|(f, id)| (f, *id)).collect();
    let kept: Vec<ListedName> = (before.into_iter())
        .filter_map(|f| Some((identities.get(&f).copied()?, f)))
        .map(|(id, f)| (f, id))
        .collect();
    (kept.len() == listed.len()).then_some(kept)
}

/// Compare with runs of digits by value, so "img2" comes before "img10".
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut a, mut b) = (a, b);
    loop {
        let (Some(ca), Some(cb)) = (a.chars().next(), b.chars().next()) else {
            return a.len().cmp(&b.len());
        };
        if ca.is_ascii_digit() && cb.is_ascii_digit() {
            let end_a = a.find(|c: char| !c.is_ascii_digit()).unwrap_or(a.len());
            let end_b = b.find(|c: char| !c.is_ascii_digit()).unwrap_or(b.len());
            let (na, nb) = (
                a[..end_a].trim_start_matches('0'),
                b[..end_b].trim_start_matches('0'),
            );
            let ord = na.len().cmp(&nb.len()).then(na.cmp(nb));
            if ord.is_ne() {
                return ord;
            }
            (a, b) = (&a[end_a..], &b[end_b..]);
        } else {
            if ca != cb {
                return ca.cmp(&cb);
            }
            (a, b) = (&a[ca.len_utf8()..], &b[cb.len_utf8()..]);
        }
    }
}

/// The shuffled `old` order without files that are gone, followed by new files in random order.
/// Each file has the identity `new` lists it with.
pub fn merge_shuffled(old: &[OsString], new: Vec<ListedName>) -> Vec<ListedName> {
    let old_set: HashSet<&OsString> = old.iter().collect();
    let new_identities: std::collections::HashMap<&OsString, Identity> =
        new.iter().map(|(f, id)| (f, *id)).collect();
    let mut added: Vec<ListedName> = new
        .iter()
        .filter(|f| !old_set.contains(&f.0))
        .cloned()
        .collect();
    shuffle(&mut added);
    let mut out: Vec<ListedName> = (old.iter())
        .filter_map(|f| Some((f.clone(), *new_identities.get(f)?)))
        .collect();
    out.extend(added);
    out
}

/// Merges `new`, a tree's images read so far or all of them, into `old`, a list in random
/// order whose files up to and with place `shown` have been shown. Those files keep their
/// places, so Previous goes back through them. The new names are shuffled in among the files
/// of `old` not shown yet, so the images of the folders read later come up as soon as the
/// others do. When `whole` is false, `new` is a part of the tree and the files of `old` that it
/// lacks are kept, as the walk may still find them. When `whole` is true, the files that `new`
/// lacks are gone and are left out, and every file has the identity that `new` gives it.
/// Returns `old` as it is when no name is new and none is gone.
pub fn merge_shuffled_after(
    old: Vec<ListedName>,
    shown: usize,
    new: Vec<ListedName>,
    whole: bool,
) -> Vec<ListedName> {
    let old_set: HashSet<&OsString> = old.iter().map(|f| &f.0).collect();
    let mut added: Vec<ListedName> = (new.iter())
        .filter(|f| !old_set.contains(&f.0))
        .cloned()
        .collect();
    if added.is_empty() && !whole {
        return old;
    }
    let new_identities: std::collections::HashMap<&OsString, Identity> =
        new.iter().map(|(f, id)| (f, *id)).collect();
    let still_listed = |(name, identity): ListedName| match whole {
        true => new_identities.get(&name).map(|&id| (name, id)),
        false => Some((name, identity)),
    };
    let shown_count = (shown + 1).min(old.len());
    let mut old = old.into_iter();
    let mut out: Vec<ListedName> = (old.by_ref().take(shown_count))
        .filter_map(still_listed)
        .collect();
    let mut not_shown: Vec<ListedName> = old.filter_map(still_listed).collect();
    if !added.is_empty() {
        not_shown.append(&mut added);
        shuffle(&mut not_shown);
    }
    out.append(&mut not_shown);
    out
}

/// Fisher–Yates with xorshift seeded from std's random hash key (no rand dependency).
pub fn shuffle<T>(v: &mut [T]) {
    use std::hash::{BuildHasher, Hasher};
    let mut x = std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish()
        | 1;
    for i in (1..v.len()).rev() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        v.swap(i, (x % (i as u64 + 1)) as usize);
    }
}

#[cfg(test)]
mod tests {
    /// A tree on a share is read on only once a program started on a file (Play Video, Open
    /// With) has had a moment to read the start of its file. Before, the player's reads waited
    /// behind a look at each file of the tree. A tree on a disk of its own does not wait.
    #[test]
    fn a_tree_on_a_share_waits_while_a_program_starts() {
        use super::{PROGRAM_STARTS_FOR, is_starting};
        use std::sync::atomic::AtomicBool;
        use std::time::{Duration, Instant};
        let now = Instant::now();
        assert!(is_starting(Some(now), now) && !is_starting(None, now));
        assert!(!is_starting(Some(now), now + PROGRAM_STARTS_FOR));
        let dir =
            std::env::temp_dir().join(format!("glimt-test-share-gives-way-{}", std::process::id()));
        for sub in ["a", "b"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
            std::fs::write(dir.join(sub).join("1.jpg"), "x").unwrap();
        }
        let list = || {
            let began = Instant::now();
            let types = super::Types {
                images: true,
                raw: false,
                videos: false,
                hidden: false,
            };
            super::list_tree(&dir, types, &AtomicBool::new(false), |_| {}, |_| {}).unwrap();
            began.elapsed()
        };
        // The program was started long enough ago that 400 ms of its time are left.
        let started = Instant::now().checked_sub(PROGRAM_STARTS_FOR - Duration::from_millis(400));
        *crate::support::threads::lock_even_if_poisoned(&super::PROGRAM_STARTED_AT) = started;
        assert!(list() < Duration::from_millis(300), "a disk of its own");
        crate::files::file_ops::test_mount(&dir, Some("fuse.gvfsd-fuse"));
        assert!(list() >= Duration::from_millis(200), "a share");
        crate::files::file_ops::test_mount(&dir, None);
        // A share mounted in a folder of a tree on a disk of its own, such as ~/Pictures/nas.
        // Before, only the tree's top folder was looked at.
        let started = Instant::now().checked_sub(PROGRAM_STARTS_FOR - Duration::from_millis(400));
        *crate::support::threads::lock_even_if_poisoned(&super::PROGRAM_STARTED_AT) = started;
        crate::files::file_ops::test_mount(&dir.join("b"), Some("cifs"));
        assert!(
            list() >= Duration::from_millis(200),
            "a share inside the tree"
        );
        crate::files::file_ops::test_mount(&dir.join("b"), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A tree on a share is read on only once the decode of an image that the window waits for
    /// is done, and a decode that has run for `IMAGE_AWAITED_FOR` no longer holds it, so a
    /// share that stopped answering never stops the walk for good. A tree on a disk of its own
    /// never waits. Before, the walk read on, and the next image's reads waited behind its
    /// looks at the share (#256).
    #[test]
    fn a_tree_on_a_share_waits_for_the_image_awaited() {
        use super::{IMAGE_AWAITED_FOR, ImageAwaited, is_an_image_awaited};
        use std::sync::atomic::AtomicBool;
        use std::time::{Duration, Instant};
        let now = Instant::now();
        assert!(is_an_image_awaited(&[now], now));
        assert!(!is_an_image_awaited(&[now], now + IMAGE_AWAITED_FOR));
        assert!(!is_an_image_awaited(&[], now));
        let dir = std::env::temp_dir().join(format!(
            "glimt-test-share-awaits-image-{}",
            std::process::id()
        ));
        for sub in ["a", "b"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
            std::fs::write(dir.join(sub).join("1.jpg"), "x").unwrap();
        }
        let list = || {
            let began = Instant::now();
            let types = super::Types {
                images: true,
                raw: false,
                videos: false,
                hidden: false,
            };
            super::list_tree(&dir, types, &AtomicBool::new(false), |_| {}, |_| {}).unwrap();
            began.elapsed()
        };
        let awaited = ImageAwaited::begin();
        assert!(list() < Duration::from_millis(300), "a disk of its own");
        crate::files::file_ops::test_mount(&dir, Some("cifs"));
        let decoded = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(400));
            drop(awaited);
        });
        let took = list();
        decoded.join().unwrap();
        crate::files::file_ops::test_mount(&dir, None);
        assert!(took >= Duration::from_millis(300), "a share: {took:?}");
        assert!(
            took < Duration::from_secs(3),
            "on once the image is decoded: {took:?}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A local FUSE folder, such as a KDE Vault, a mergerfs pool or the Flatpak's document
    /// portal, is not a share, and keeps the sizes the status bar sums. Before, every FUSE mount
    /// was one, and the status bar called such a folder a network folder.
    #[test]
    fn a_local_fuse_folder_is_not_a_share() {
        use crate::files::file_ops::test_mount;
        let dir =
            std::env::temp_dir().join(format!("glimt-test-local-fuse-{}", std::process::id()));
        for (kind, is_share) in [
            ("fuse.gocryptfs", false),
            ("fuse.cryfs", false),
            ("fuse.mergerfs", false),
            ("fuse.bindfs", false),
            ("fuse.portal", false),
            ("fuseblk", false),
            ("ext4", false),
            ("fuse.sshfs", true),
            ("fuse.rclone", true),
            ("fuse.gvfsd-fuse", true),
            ("fuse.simple-mtpfs", true),
            ("fuse.jmtpfs", true),
            ("fuse.onedriver", true),
            ("fuse.google-drive-ocamlfuse", true),
            ("fuse.goofys", true),
            ("fuse.juicefs", true),
            ("cifs", true),
            ("nfs4", true),
        ] {
            test_mount(&dir, Some(kind));
            assert_eq!(super::is_on_a_share(&dir, false), is_share, "{kind}");
        }
        test_mount(&dir, None);
    }

    /// The images of a tree told after each folder with images, read in name order: the
    /// first told are a's own, before any of b.
    #[test]
    fn tree_told_a_folder_at_a_time() {
        use std::sync::atomic::AtomicBool;
        let dir =
            std::env::temp_dir().join(format!("glimt-test-tree-parts-{}", std::process::id()));
        for (sub, name) in [
            ("a/deep", "1.jpg"),
            ("a", "2.jpg"),
            ("b", "3.jpg"),
            ("c", "4.jpg"),
        ] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
            std::fs::write(dir.join(sub).join(name), "x").unwrap();
        }
        let types = super::Types {
            images: true,
            raw: true,
            videos: true,
            hidden: false,
        };
        let mut parts: Vec<Vec<std::ffi::OsString>> = Vec::new();
        let (_, all, _, _) = super::list_tree(
            &dir,
            types,
            &AtomicBool::new(false),
            |_| {},
            |p| parts.push(p.iter().map(|f| f.0.clone()).collect()),
        )
        .unwrap();
        assert_eq!(all.len(), 4);
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0], [std::ffi::OsString::from("a/2.jpg")]);
        assert_eq!(parts[1].last().unwrap(), "a/deep/1.jpg");
        assert_eq!(parts[2].last().unwrap(), "b/3.jpg");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A link in the tree is followed only to a folder inside the tree. A link to a folder
    /// above it (`up -> ../..`), beside it (`beside -> ../../beside`) or anywhere else is not:
    /// it listed the folders around the tree, the user's own photos, or from `/` the whole disk.
    #[test]
    fn a_tree_follows_a_link_only_inside_it() {
        use std::os::unix::fs::symlink;
        let base = std::env::temp_dir().join(format!("glimt-test-tree-up-{}", std::process::id()));
        let tree = base.join("tree");
        std::fs::create_dir_all(tree.join("sub")).unwrap();
        std::fs::create_dir_all(tree.join(".inner")).unwrap();
        std::fs::create_dir_all(base.join("beside")).unwrap();
        for f in [
            "outside.jpg",
            "tree/a.jpg",
            "beside/b.jpg",
            "tree/.inner/c.jpg",
        ] {
            std::fs::write(base.join(f), "x").unwrap();
        }
        symlink("../..", tree.join("sub/up")).unwrap();
        symlink("../../beside", tree.join("sub/beside")).unwrap();
        symlink(base.join("beside"), tree.join("sub/far")).unwrap();
        symlink("../.inner", tree.join("sub/in")).unwrap();
        let types = super::Types {
            images: true,
            raw: true,
            videos: true,
            hidden: false,
        };
        let stop = std::sync::atomic::AtomicBool::new(false);
        // Also when the tree is named through a link, as its real path is what counts.
        symlink(&tree, base.join("named")).unwrap();
        for top in [tree.clone(), base.join("named")] {
            let (_, files, _, _) = super::list_tree(&top, types, &stop, |_| {}, |_| {}).unwrap();
            let files: Vec<_> = files.into_iter().map(|f| f.0).collect();
            // The hidden folder is not listed itself, so its image is seen only through the link,
            // and it is named by its real path in the tree.
            assert_eq!(files, [".inner/c.jpg", "a.jpg"], "{top:?}");
        }
        std::fs::remove_dir_all(base).unwrap();
    }

    /// A file reached through a link to a folder in the tree is named by its real path, the
    /// name `--remote select`'s client gives it, whichever way the walk came first. Named
    /// through the link, `--remote select` missed it in a tree with Include Subfolders.
    #[test]
    fn a_file_through_a_link_in_the_tree_has_its_real_name() {
        let dir = std::env::temp_dir().join(format!("glimt-test-tree-name-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("real")).unwrap();
        std::fs::write(dir.join("real/x.jpg"), "x").unwrap();
        // Before "real" in name order, so the walk reaches the folder through the link first.
        std::os::unix::fs::symlink("real", dir.join("a-link")).unwrap();
        let types = super::Types {
            images: true,
            raw: true,
            videos: true,
            hidden: false,
        };
        let stop = std::sync::atomic::AtomicBool::new(false);
        let (_, files, _, _) = super::list_tree(&dir, types, &stop, |_| {}, |_| {}).unwrap();
        assert_eq!(files[0].0, "real/x.jpg");
        assert_eq!(files.len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// On a file system whose folders all have inode 0 (sshfs with `-o use_ino`), the whole
    /// tree is listed, and a link back into it is still read once. Told apart by inode alone,
    /// every folder below the one opened counted as read already, and the tree was cut short.
    #[test]
    fn a_tree_without_unique_inodes_is_listed_whole() {
        let dir = std::env::temp_dir().join(format!("glimt-test-tree-ino-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("a/deep")).unwrap();
        for f in ["1.jpg", "a/2.jpg", "a/deep/3.jpg"] {
            std::fs::write(dir.join(f), "x").unwrap();
        }
        std::os::unix::fs::symlink("..", dir.join("a/again")).unwrap();
        let types = super::Types {
            images: true,
            raw: true,
            videos: true,
            hidden: false,
        };
        let file_id =
            |p: &std::path::Path| Some(crate::images::save::file_id_from_inode(p, (1, 0)));
        let real = |p: &std::path::Path| std::fs::canonicalize(p);
        let mut files: Vec<std::path::PathBuf> =
            super::walk_by(&dir, types, file_id, real, super::list_folder)
                .map(Result::unwrap)
                .flat_map(|(rel, _, files, _)| files.into_iter().map(move |f| rel.join(f.0)))
                .collect();
        files.sort();
        assert_eq!(
            files,
            ["1.jpg", "a/2.jpg", "a/deep/3.jpg"].map(std::path::PathBuf::from)
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The top folder that cannot be read is an error of the walk, which stops there, so the
    /// listing fails and the list shown stays. Before, it was walked as empty, and a share that
    /// failed for a moment emptied the list and its selection. A folder below the top that
    /// cannot be read, for any reason, is an error too, after which the walk goes on with the
    /// rest of the tree. Before, any error but two stopped the walk, and a damaged subfolder
    /// left the whole tree unlisted at every listing. A folder that is gone has nothing in it.
    #[test]
    fn only_the_top_folder_that_cannot_be_read_fails_the_walk() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-walk-unread-{}", std::process::id()));
        for sub in ["a", "b", "c"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
            std::fs::write(dir.join(sub).join("1.jpg"), "x").unwrap();
        }
        let types = super::Types {
            images: true,
            raw: false,
            videos: false,
            hidden: false,
        };
        let real = |p: &std::path::Path| std::fs::canonicalize(p);
        let walked = |failing: std::path::PathBuf, kind: std::io::ErrorKind| {
            let list = move |folder: &std::path::Path, types| match folder == failing {
                true => Err(std::io::Error::from(kind)),
                false => super::list_folder(folder, types),
            };
            let found: Vec<_> =
                super::walk_by(&dir, types, crate::images::save::file_id, real, list)
                    .map(|w| w.map(|(rel, _, files, _)| (rel, files.len())))
                    .map(|w| w.map_err(|(path, e)| (path, e.kind())))
                    .collect();
            found
        };
        let top = walked(dir.clone(), std::io::ErrorKind::PermissionDenied);
        assert_eq!(
            top,
            [Err((dir.clone(), std::io::ErrorKind::PermissionDenied))]
        );
        for kind in [
            std::io::ErrorKind::Other,
            std::io::ErrorKind::PermissionDenied,
        ] {
            let broken = walked(dir.join("b"), kind);
            let ok = |r: &str, n| Ok((std::path::PathBuf::from(r), n));
            assert_eq!(
                broken,
                [
                    ok("", 0),
                    ok("a", 1),
                    Err((dir.join("b"), kind)),
                    ok("c", 1)
                ]
            );
        }
        let gone = walked(dir.join("b"), std::io::ErrorKind::NotFound);
        assert_eq!(gone.len(), 4);
        assert!(gone.iter().all(Result::is_ok), "{gone:?}");

        let gone = super::list_folder(&dir.join("gone"), types).unwrap();
        assert_eq!(gone, (Vec::new(), Vec::new()));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A link to a folder is followed only when its real path is known and inside the tree.
    /// The walk itself is checked, with a link whose real path is too long to work out: taken
    /// from `real_dir`, which makes such a path absolute instead, it was followed.
    #[test]
    fn a_link_whose_real_path_is_unknown_is_not_followed() {
        let top = std::path::Path::new("/home/a/pics");
        assert_eq!(super::inside(Ok(top.join("sub")), top), Some("sub".into()));
        assert_eq!(super::inside(Ok("/home/a".into()), top), None);
        let dir = std::env::temp_dir().join(format!("glimt-test-walk-long-{}", std::process::id()));
        // The folder is hidden, so it is reached only through the link.
        std::fs::create_dir_all(dir.join(".in")).unwrap();
        std::fs::write(dir.join(".in/b.jpg"), "x").unwrap();
        std::fs::write(dir.join("a.jpg"), "x").unwrap();
        std::os::unix::fs::symlink(".in", dir.join("long")).unwrap();
        let types = super::Types {
            images: true,
            raw: true,
            videos: true,
            hidden: false,
        };
        let real = |p: &std::path::Path| match p.ends_with("long") {
            true => Err(std::io::Error::from_raw_os_error(36)),
            false => std::fs::canonicalize(p),
        };
        let files: Vec<std::path::PathBuf> = super::walk_by(
            &dir,
            types,
            crate::images::save::file_id,
            real,
            super::list_folder,
        )
        .map(Result::unwrap)
        .flat_map(|(rel, _, files, _)| files.into_iter().map(move |f| rel.join(f.0)))
        .collect();
        assert_eq!(files, [std::path::PathBuf::from("a.jpg")]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A path is compared by its folder's real path and its own name: `..` and the folder's
    /// links are worked out, and a link to a file keeps its own name.
    #[test]
    fn real_path_follows_the_folder_not_the_file() {
        let dir = std::env::temp_dir().join(format!("glimt-test-real-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("real")).unwrap();
        std::os::unix::fs::symlink(dir.join("real"), dir.join("link")).unwrap();
        std::os::unix::fs::symlink(dir.join("real/a.jpg"), dir.join("real/b.jpg")).unwrap();
        let real = dir.canonicalize().unwrap().join("real");
        assert_eq!(
            super::real_path(&dir.join("link/b.jpg")),
            real.join("b.jpg")
        );
        assert_eq!(
            super::real_path(&dir.join("link/../link/a.jpg")),
            real.join("a.jpg")
        );
        assert_eq!(
            super::real_dir(&dir.join("link/..")),
            real.parent().unwrap()
        );
        // A folder that is not there keeps the real path of the folders above it that are
        // there, and the rest as written: here only / is there.
        assert_eq!(
            super::real_dir(std::path::Path::new("/no/such")),
            std::path::Path::new("/no/such")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A folder's time is taken before it is read. A file made while the folder is read, which
    /// the read may miss, then changes the folder's time from the one recorded, and the folder
    /// watcher lists the folder again. Taken after the read, the time already held the file,
    /// and the file stayed out of the list.
    #[test]
    fn a_folder_is_timed_before_it_is_read() {
        let dir = std::env::temp_dir().join(format!("glimt-test-walk-time-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        // The folder's own changes are left a clock tick behind, so the file made below is
        // the only change in its tick.
        std::thread::sleep(std::time::Duration::from_millis(50));
        let types = super::Types {
            images: true,
            raw: true,
            videos: true,
            hidden: false,
        };
        let list = |folder: &std::path::Path, types| {
            let listed = super::list_folder(folder, types);
            std::fs::write(folder.join("late.jpg"), "x").unwrap();
            listed
        };
        let real = |p: &std::path::Path| std::fs::canonicalize(p);
        for (rel, _, files, modified) in
            super::walk_by(&dir, types, crate::images::save::file_id, real, list)
                .map(Result::unwrap)
        {
            assert!(files.is_empty());
            let now = std::fs::metadata(dir.join(&rel))
                .unwrap()
                .modified()
                .unwrap();
            assert_ne!(modified, Some(now), "{rel:?}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn tree_listing_stops() {
        use std::sync::atomic::AtomicBool;
        let dir = std::env::temp_dir().join(format!("glimt-test-tree-stop-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/a.jpg"), "x").unwrap();
        let types = super::Types {
            images: true,
            raw: true,
            videos: true,
            hidden: false,
        };
        assert_eq!(
            super::list_tree(&dir, types, &AtomicBool::new(false), |_| {}, |_| {})
                .unwrap()
                .1
                .len(),
            1
        );
        let stopped =
            super::list_tree(&dir, types, &AtomicBool::new(true), |_| {}, |_| {}).unwrap();
        let (own, files, read, left_out) = stopped;
        assert!(own.is_empty() && files.is_empty() && read.is_empty() && left_out.is_empty());
        // The top folder's own folders are told before any folder below is read: stopped
        // there, nothing of sub is listed.
        let stop = AtomicBool::new(false);
        let mut told = Vec::new();
        let (_, files, _, _) = super::list_tree(
            &dir,
            types,
            &stop,
            |dirs| {
                told = dirs.to_vec();
                stop.store(true, std::sync::atomic::Ordering::Relaxed);
            },
            |_| {},
        )
        .unwrap();
        assert_eq!(told, [dir.join("sub")]);
        assert!(files.is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Images are files, or links to files: a folder named like an image is a folder, and a
    /// pipe or a link to a device is left out.
    #[test]
    fn lists_only_files_as_images() {
        use std::os::unix::fs::symlink;
        let dir = std::env::temp_dir().join(format!("glimt-test-kinds-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("2019.jpg")).unwrap();
        std::fs::write(dir.join("a.jpg"), "").unwrap();
        symlink(dir.join("a.jpg"), dir.join("b.jpg")).unwrap();
        symlink("/dev/zero", dir.join("zero.jpg")).unwrap();
        symlink(dir.join("gone"), dir.join("broken.jpg")).unwrap();
        let fifo = std::process::Command::new("mkfifo")
            .arg(dir.join("pipe.jpg"))
            .status();
        assert!(fifo.is_ok_and(|s| s.success()));
        let types = super::Types {
            images: true,
            raw: false,
            videos: false,
            hidden: false,
        };
        let (dirs, images) = super::list_folder(&dir, types).unwrap();
        let images: Vec<_> = images.into_iter().map(|f| f.0).collect();
        assert_eq!(images, ["a.jpg", "b.jpg"]);
        assert_eq!(dirs, [dir.join("2019.jpg")]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The Types menu: images, RAW and videos each on their own; an opened file's type.
    #[test]
    fn lists_by_type() {
        use super::{Types, is_listed};
        use std::path::Path;
        let raw = Types {
            raw: true,
            ..Types::NONE
        };
        let images = Types {
            images: true,
            ..Types::NONE
        };
        assert!(is_listed(Path::new("a.CR2"), raw) && !is_listed(Path::new("a.cr2"), images));
        assert!(is_listed(Path::new("a.jpg"), images) && !is_listed(Path::new("a.jpg"), raw));
        assert!(!is_listed(
            Path::new("a.txt"),
            Types {
                videos: true,
                images: true,
                raw: true,
                hidden: false
            }
        ));
        let t = Types::of(Path::new("x/a.nef"));
        assert!(t.raw && !t.images && !t.videos && !t.hidden);
        assert!(Types::of(Path::new("x/.a.nef")).hidden);
        assert!(!Types::of(Path::new("a.txt")).images);
    }

    #[test]
    fn pattern_names() {
        let old = std::path::Path::new("/p/IMG_4711.JPG");
        assert_eq!(super::pattern_name("trip_###", old, 7), "trip_007.JPG");
        // Latin-1 "r\xf8ntgen.jpg": its ø kept as the byte it is, not made "\u{fffd}".
        use std::os::unix::ffi::OsStrExt;
        let latin = std::path::Path::new(std::ffi::OsStr::from_bytes(b"/p/r\xf8ntgen.jpg"));
        assert_eq!(
            super::pattern_name("*-#", latin, 1).as_bytes(),
            b"r\xf8ntgen-1.jpg"
        );
        assert_eq!(super::pattern_name("*-#", old, 12), "IMG_4711-12.JPG");
        assert_eq!(
            super::pattern_name("x", std::path::Path::new("/p/noext"), 1),
            "x"
        );
    }

    #[test]
    fn sorts_numbers_by_value() {
        use std::cmp::Ordering::*;
        assert_eq!(super::natural_cmp("img2.jpg", "img10.jpg"), Less);
        assert_eq!(super::natural_cmp("img010", "img10"), Equal);
        assert_eq!(super::natural_cmp("a", "a1"), Less);
        // Numbers in brackets too, as a browser names downloads: a(2) before a(10) before a(100).
        assert_eq!(super::natural_cmp("a(2).jpg", "a(10).jpg"), Less);
        assert_eq!(super::natural_cmp("a(10).jpg", "a(100).jpg"), Less);
        let mut v = listed(&["b10", "B2", "a1", "b9"]);
        super::sort_files(
            std::path::Path::new("."),
            &mut v,
            super::Sort::Number,
            false,
            false,
        );
        assert_eq!(v, listed(&["b10", "b9", "B2", "a1"]));
        // Equal numbers in name order.
        let mut v = listed(&["a1", "a01"]);
        let dot = std::path::Path::new(".");
        super::sort_files(dot, &mut v, super::Sort::Number, true, false);
        let os = |v: &[&str]| v.iter().map(std::ffi::OsString::from).collect::<Vec<_>>();
        let kept = super::kept_order(os(&["c", "a", "gone", "b"]), &listed(&["a", "b", "c"]));
        assert_eq!(kept, Some(listed(&["c", "a", "b"])));
        assert_eq!(
            super::kept_order(os(&["c", "a"]), &listed(&["a", "b", "c"])),
            None
        );
        assert_eq!(v, listed(&["a01", "a1"]));
        let mut v = listed(&["b.png", "c.JPG", "a.mp4", "d.jpg", "e"]);
        super::sort_files(dot, &mut v, super::Sort::Extension, true, false);
        assert_eq!(v, listed(&["e", "c.JPG", "d.jpg", "a.mp4", "b.png"]));
    }

    /// The names `names`, each with an identity of its own, as a listing gives them.
    fn listed(names: &[&str]) -> Vec<super::ListedName> {
        let identity = |n: &str| std::num::NonZeroU64::new(n.bytes().map(u64::from).sum());
        (names.iter()).map(|n| ((*n).into(), identity(n))).collect()
    }

    /// The order kept and a file read again keep the identity the new listing gives each name,
    /// so a name that another file has taken since is told apart.
    #[test]
    fn merge_keeps_shuffled_order() {
        let names = |v: &[&str]| v.iter().map(std::ffi::OsString::from).collect::<Vec<_>>();
        let mut new = listed(&["a", "b", "d"]);
        new[0].1 = std::num::NonZeroU64::new(1000);
        let merged = super::merge_shuffled(&names(&["c", "a", "b"]), new.clone());
        assert_eq!(merged, new);
        let kept = super::kept_order(names(&["b", "d", "a"]), &new).unwrap();
        assert_eq!(kept[2], new[0]);
    }

    /// A part of a tree merged into a list in random order keeps the images shown so far in
    /// their places and gets every new name once, after them, while the names the part lacks
    /// stay. The whole list after it leaves out the names that are gone, with the identities
    /// it gives. A part with no new name leaves the list as it is.
    #[test]
    fn a_part_merged_into_a_random_order_keeps_the_images_shown() {
        let old = listed(&["c", "a", "b", "d"]);
        let merged = super::merge_shuffled_after(old.clone(), 1, listed(&["a", "e", "f"]), false);
        assert_eq!(merged[..2], old[..2]);
        let mut rest = merged[2..].to_vec();
        rest.sort();
        assert_eq!(rest, listed(&["b", "d", "e", "f"]));
        let mut new = listed(&["a", "c", "e"]);
        new[0].1 = std::num::NonZeroU64::new(1000);
        let whole = super::merge_shuffled_after(merged.clone(), 1, new.clone(), true);
        assert_eq!(whole[..2], [new[1].clone(), new[0].clone()]);
        assert_eq!(whole[2..], new[2..]);
        let unchanged = super::merge_shuffled_after(old.clone(), 3, listed(&["a", "d"]), false);
        assert_eq!(unchanged, old);
        // A list still empty, as before the first part of a tree, takes the part whole.
        let first = super::merge_shuffled_after(Vec::new(), 0, listed(&["a", "b"]), false);
        assert_eq!(first.len(), 2);
    }

    #[test]
    fn shuffle_keeps_all_elements() {
        let mut v: Vec<u32> = (0..100).collect();
        super::shuffle(&mut v);
        assert_ne!(v, (0..100).collect::<Vec<_>>());
        v.sort();
        assert_eq!(v, (0..100).collect::<Vec<_>>());
    }
}
