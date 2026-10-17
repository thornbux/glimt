//! The file list: which files are images (by extension and file type), reading a folder or a tree
//! of them, and sorting them (by name as numbers, date, size, EXIF date or shuffled).

use crate::external::{is_heif, is_video};
use crate::{exif, meta, raw, small_alloc_threads};
use std::collections::{HashMap, HashSet};
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

pub fn has_ext(path: &Path, exts: &[&str]) -> bool {
    let ext = path.extension().and_then(|e| e.to_str());
    ext.is_some_and(|e| exts.contains(&e.to_ascii_lowercase().as_str()))
}

/// The EXIF fields of every file as one lowercase line, read in parallel (small_alloc_threads).
pub fn exif_index(dir: &Path, files: &[OsString]) -> HashMap<OsString, String> {
    let threads = small_alloc_threads();
    let chunk = files.len().div_ceil(threads).max(1);
    std::thread::scope(|s| {
        let parts: Vec<_> = files
            .chunks(chunk)
            .map(|part| {
                s.spawn(move || {
                    part.iter()
                        .map(|name| {
                            let path = dir.join(name);
                            let rows = exif::read(&path);
                            let mut text: Vec<&str> = rows.iter().map(|r| r.1.as_str()).collect();
                            let meta = meta::read(&path).text();
                            text.push(&meta);
                            (name.clone(), text.join(" ").to_lowercase())
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        parts
            .into_iter()
            .flat_map(|p| p.join().unwrap_or_default())
            .collect()
    })
}

/// A new file name from `pattern`: * is the old name without extension, a run of # the number
/// `n` with that many digits; the old extension is kept.
pub fn pattern_name(pattern: &str, old: &Path, n: usize) -> String {
    let stem = old
        .file_stem()
        .map_or(String::new(), |s| s.to_string_lossy().into_owned());
    let (mut out, mut chars) = (String::new(), pattern.chars().peekable());
    while let Some(c) = chars.next() {
        match c {
            '*' => out += &stem,
            '#' => {
                let mut width = 1;
                while chars.next_if_eq(&'#').is_some() {
                    width += 1;
                }
                out += &format!("{n:0width$}");
            }
            c => out.push(c),
        }
    }
    match old.extension() {
        Some(ext) => format!("{out}.{}", ext.to_string_lossy()),
        None => out,
    }
}

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

/// Shown in the file list: an image, a RAW file or a video (video feature), of the `types` on.
pub fn is_listed(path: &Path, types: Types) -> bool {
    (types.hidden || !is_hidden(path)) && is_listed_type(path, types)
}

/// A name starting with a dot, which Geeqie and file managers hide.
pub fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|n| n.as_encoded_bytes().starts_with(b"."))
}

pub fn is_listed_type(path: &Path, types: Types) -> bool {
    (types.images
        && (has_ext(path, EXTS)
            || is_heif(path)
            || has_ext(path, JXL_EXTS)
            || has_ext(path, SVG_EXTS)
            || crate::decoders::for_path(path).is_some()))
        || (types.raw && has_ext(path, raw::EXTS))
        || (types.videos && is_video(path))
}

/// Whether Glimt reads the format of `path` itself, as built: a decoder plugin is not asked.
pub fn own_ext(path: &Path) -> bool {
    has_ext(path, EXTS)
        || is_heif(path)
        || has_ext(path, JXL_EXTS)
        || has_ext(path, SVG_EXTS)
        || has_ext(path, raw::EXTS)
        || is_video(path)
}

/// `dir` and every folder below it, one at a time: (its path relative to `dir`, its folders,
/// its images). A folder reached again through a link is read once.
pub fn walk(
    dir: &Path,
    types: Types,
) -> impl Iterator<Item = (PathBuf, Vec<PathBuf>, Vec<OsString>)> {
    let (dir, mut seen, mut todo) = (dir.to_path_buf(), HashSet::new(), vec![PathBuf::new()]);
    std::iter::from_fn(move || {
        loop {
            let rel = todo.pop()?;
            let full = dir.join(&rel);
            if !seen.insert(full.canonicalize().unwrap_or_else(|_| full.clone())) {
                continue;
            }
            let (dirs, files) = list_folder(&full, types);
            todo.extend(
                dirs.iter()
                    .filter_map(|d| d.file_name())
                    .map(|n| rel.join(n)),
            );
            return Some((rel, dirs, files));
        }
    })
}

/// The folders in `dir`, as `list_folder` gives them, and the images in it and every folder
/// below it, as paths relative to `dir`: `dir` read once.
pub fn list_tree(dir: &Path, types: Types) -> (Vec<PathBuf>, Vec<OsString>) {
    let mut top = Vec::new();
    let mut out: Vec<OsString> = walk(dir, types)
        .flat_map(|(rel, dirs, files)| {
            if rel.as_os_str().is_empty() {
                top = dirs;
            }
            files.into_iter().map(move |f| rel.join(f).into_os_string())
        })
        .collect();
    // In name order, as list_folder gives a single folder; sort_files builds on that.
    out.sort_unstable();
    (top, out)
}

/// Whether a folder entry is a file and whether a folder, from the entry's own type (no `stat`),
/// a link as what it points to. Only files are images: a folder named 2019.jpg is a folder, and
/// a pipe or a link to /dev/zero named a.jpg would block or fill memory when read.
pub fn kind(entry: &std::fs::DirEntry) -> (bool, bool) {
    match entry.file_type() {
        Ok(t) if t.is_symlink() => {
            std::fs::metadata(entry.path()).map_or((false, false), |m| (m.is_file(), m.is_dir()))
        }
        Ok(t) => (t.is_file(), t.is_dir()),
        Err(_) => (false, false),
    }
}

/// One pass over the folder: (subfolders, image file names), both sorted by name.
/// The file type comes from the directory entry, so there is no `stat` per file (only symlinks are resolved).
pub fn list_folder(dir: &Path, types: Types) -> (Vec<PathBuf>, Vec<OsString>) {
    let (mut dirs, mut images) = (Vec::new(), Vec::new());
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        let (file, folder) = kind(&entry);
        if file && is_listed(&path, types) {
            images.push(entry.file_name());
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
    (dirs, images)
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

impl Sort {
    pub fn entry(self) -> (Sort, &'static str, &'static str) {
        SORTS.into_iter().find(|s| s.0 == self).unwrap_or(SORTS[0])
    }
}

/// Sort file names in `dir`; `case`: case-sensitive names ("B" before "a"). Everything but name
/// and number needs a stat (or EXIF) per file, so this runs in the listing thread; ties fall
/// back to the name.
pub fn sort_files(dir: &Path, files: &mut [OsString], by: Sort, ascending: bool, case: bool) {
    let name = |f: &OsString| {
        let s = f.to_string_lossy();
        if case {
            s.into_owned()
        } else {
            s.to_lowercase()
        }
    };
    let meta = |f: &OsString| std::fs::metadata(dir.join(f)).ok();
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
            let mut keyed: Vec<(String, OsString)> = (files.iter_mut())
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
                let date = exif::date(&dir.join(f), digitized);
                (date.is_none(), date, name(f))
            })
        }
    }
    if !ascending {
        files.reverse();
    }
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
        f.write_str(self.entry().1)
    }
}

/// The shuffled `old` order without files that are gone, followed by new files in random order.
pub fn merge_shuffled(old: &[OsString], new: Vec<OsString>) -> Vec<OsString> {
    let old_set: HashSet<&OsString> = old.iter().collect();
    let new_set: HashSet<&OsString> = new.iter().collect();
    let mut added: Vec<OsString> = new
        .iter()
        .filter(|f| !old_set.contains(f))
        .cloned()
        .collect();
    shuffle(&mut added);
    let mut out: Vec<OsString> = old
        .iter()
        .filter(|f| new_set.contains(f))
        .cloned()
        .collect();
    out.extend(added);
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
        let (dirs, images) = super::list_folder(&dir, types);
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
        let mut v: Vec<std::ffi::OsString> =
            ["b10", "B2", "a1", "b9"].iter().map(Into::into).collect();
        super::sort_files(
            std::path::Path::new("."),
            &mut v,
            super::Sort::Number,
            false,
            false,
        );
        assert_eq!(v, ["b10", "b9", "B2", "a1"]);
        // Equal numbers in name order.
        let mut v: Vec<std::ffi::OsString> = ["a1", "a01"].iter().map(Into::into).collect();
        let dot = std::path::Path::new(".");
        super::sort_files(dot, &mut v, super::Sort::Number, true, false);
        assert_eq!(v, ["a01", "a1"]);
        let mut v: Vec<std::ffi::OsString> = ["b.png", "c.JPG", "a.mp4", "d.jpg", "e"]
            .iter()
            .map(Into::into)
            .collect();
        super::sort_files(dot, &mut v, super::Sort::Extension, true, false);
        assert_eq!(v, ["e", "c.JPG", "d.jpg", "a.mp4", "b.png"]);
    }

    #[test]
    fn merge_keeps_shuffled_order() {
        let names = |v: &[&str]| v.iter().map(std::ffi::OsString::from).collect::<Vec<_>>();
        let merged = super::merge_shuffled(&names(&["c", "a", "b"]), names(&["a", "b", "d"]));
        assert_eq!(merged, names(&["a", "b", "d"]));
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
