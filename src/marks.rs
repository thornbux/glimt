//! Optional marks and ratings (`cargo build --features marks`): Geeqie's marks 1–6 and 0–5
//! stars, kept by full path in `$XDG_STATE_HOME/glimt/marks`, one file as "bits\tstars\tpath"
//! lines. Files that are renamed or moved outside Glimt lose them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Default)]
pub struct Marks {
    /// Mark bits (bit 0 = mark 1) and stars, by path; files with neither are left out.
    map: HashMap<PathBuf, (u8, u8)>,
}

fn file() -> Option<PathBuf> {
    crate::xdg_dir("XDG_STATE_HOME", ".local/state", "glimt").map(|d| d.join("marks"))
}

impl Marks {
    pub fn load() -> Self {
        let text = file()
            .and_then(|f| std::fs::read(f).ok())
            .unwrap_or_default();
        Marks {
            map: text.split(|&b| b == b'\n').filter_map(parse_line).collect(),
        }
    }

    /// (mark bits, stars) of `path`.
    pub fn get(&self, path: &Path) -> (u8, u8) {
        self.map.get(path).copied().unwrap_or_default()
    }

    /// Toggle mark `n` (1–6) as Geeqie does: set on all of `paths`, or off if all have it.
    pub fn toggle(&mut self, paths: &[PathBuf], n: u8) {
        let bit = 1 << (n - 1);
        let all = paths.iter().all(|p| self.get(p).0 & bit != 0);
        self.change(paths, Change::Mark(bit, !all));
    }

    pub fn rate(&mut self, paths: &[PathBuf], stars: u8) {
        self.change(paths, Change::Stars(stars));
    }

    fn change(&mut self, paths: &[PathBuf], change: Change) {
        apply(&mut self.map, paths, change);
        save(paths.to_vec(), change);
    }
}

/// A change, rather than the new value, so that applied to the file it keeps what another
/// Glimt window set on the same file meanwhile.
#[derive(Clone, Copy)]
enum Change {
    /// A mark bit set or cleared.
    Mark(u8, bool),
    Stars(u8),
}

/// Apply `change` to each path's marks and stars; a path with neither is removed.
fn apply(map: &mut HashMap<PathBuf, (u8, u8)>, paths: &[PathBuf], change: Change) {
    for path in paths {
        let (bits, stars) = map.get(path).copied().unwrap_or_default();
        let new = match change {
            Change::Mark(bit, true) => (bits | bit, stars),
            Change::Mark(bit, false) => (bits & !bit, stars),
            Change::Stars(stars) => (bits, stars),
        };
        match new {
            (0, 0) => drop(map.remove(path)),
            v => drop(map.insert(path.clone(), v)),
        }
    }
}

fn to_text(map: &HashMap<PathBuf, (u8, u8)>) -> Vec<u8> {
    let mut text = Vec::new();
    for (path, (bits, stars)) in map {
        let path = path.as_os_str().as_encoded_bytes();
        if !path.contains(&b'\n') {
            text.extend(format!("{bits}\t{stars}\t").as_bytes());
            text.extend(path);
            text.push(b'\n');
        }
    }
    text
}

/// Only the changes go to the file: one writer thread, so they land in order, merges them into
/// the file as it is now, so the marks another Glimt window wrote meanwhile are kept. Renamed
/// into place so a crash never leaves half a file.
/// ponytail: the whole file each time; fine for thousands of marked files.
#[cfg(not(test))]
fn save(paths: Vec<PathBuf>, change: Change) {
    use std::sync::{OnceLock, mpsc};
    type Changes = (Vec<PathBuf>, Change);
    static WRITER: OnceLock<mpsc::Sender<Changes>> = OnceLock::new();
    let writer = WRITER.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Changes>();
        std::thread::spawn(move || {
            for (paths, change) in rx.iter() {
                let Some(file) = file() else { continue };
                let mut map = Marks::load().map;
                apply(&mut map, &paths, change);
                for (paths, change) in rx.try_iter() {
                    apply(&mut map, &paths, change);
                }
                // This Glimt's name in it: other Glimt windows write the same file.
                let tmp = file.with_extension(format!("{}.tmp", crate::remote::me()));
                let _ = std::fs::create_dir_all(file.parent().unwrap_or(Path::new(".")));
                if std::fs::write(&tmp, to_text(&map)).is_ok() {
                    let _ = std::fs::rename(tmp, file);
                }
            }
        });
        tx
    });
    let _ = writer.send((paths, change));
}

#[cfg(test)]
fn save(_: Vec<PathBuf>, _: Change) {}

fn parse_line(line: &[u8]) -> Option<(PathBuf, (u8, u8))> {
    let mut parts = line.splitn(3, |&b| b == b'\t');
    let number = |b: &[u8]| std::str::from_utf8(b).ok()?.parse::<u8>().ok();
    let (bits, stars) = (number(parts.next()?)?, number(parts.next()?)?);
    // SAFETY: the bytes were written from an OsStr by `save`, or come from the user's file;
    // on Unix any bytes are a valid OsStr.
    let path = unsafe { std::ffi::OsStr::from_encoded_bytes_unchecked(parts.next()?) };
    Some((PathBuf::from(path), (bits & 0x3f, stars.min(5))))
}

/// Write `stars` as xmp:Rating into the sidecar `<file>.xmp` that darktable and digiKam read.
/// An existing sidecar keeps everything else: only the rating changes, or is added to its first
/// rdf:Description. Without a sidecar, 0 stars writes none.
pub fn write_xmp(path: &Path, stars: u8) -> Result<(), String> {
    crate::meta::update_sidecar(path, stars == 0, |x| set_rating(x, stars))
}

/// `xmp` with its rating set to `stars`, as an attribute or element, or added; `None` if it has
/// no rdf:Description to put it in.
fn set_rating(xmp: &str, stars: u8) -> Option<String> {
    let number_after = |key: &str| {
        let start = xmp.find(key)? + key.len();
        let len = xmp[start..].find(|c: char| !c.is_ascii_digit() && c != '-')?;
        Some((start, start + len))
    };
    if let Some((a, b)) = number_after("xmp:Rating=\"").or_else(|| number_after("<xmp:Rating>")) {
        return Some(format!("{}{stars}{}", &xmp[..a], &xmp[b..]));
    }
    let at = xmp.find("<rdf:Description")? + "<rdf:Description".len();
    let ns = if xmp.contains("xmlns:xmp=") {
        ""
    } else {
        " xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\""
    };
    Some(format!(
        "{}{ns} xmp:Rating=\"{stars}\"{}",
        &xmp[..at],
        &xmp[at..]
    ))
}

/// "★★★" for the stars and "1 4" for the marks, for the file list and overlay.
pub fn text((bits, stars): (u8, u8)) -> String {
    let marks: Vec<String> = (1..=6)
        .filter(|n| bits & (1 << (n - 1)) != 0)
        .map(|n| n.to_string())
        .collect();
    let stars = "★".repeat(stars as usize);
    match (marks.is_empty(), stars.is_empty()) {
        (true, _) => stars,
        (false, true) => marks.join(" "),
        (false, false) => format!("{stars}  {}", marks.join(" ")),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn toggles_rates_and_reads_lines() {
        use std::path::PathBuf;
        let mut m = super::Marks::default();
        let (a, b) = (PathBuf::from("/a.jpg"), PathBuf::from("/b.jpg"));
        let both = [a.clone(), b.clone()];
        m.toggle(&[a.clone()], 2);
        // Mark 2 on only one of them: the toggle sets it on both, the next clears both.
        m.toggle(&both, 2);
        assert_eq!((m.get(&a), m.get(&b)), ((0b10, 0), (0b10, 0)));
        m.rate(&[b.clone()], 3);
        assert_eq!(super::text(m.get(&b)), "★★★  2");
        m.toggle(&both, 2);
        assert_eq!(m.get(&a), (0, 0));
        assert!(!m.map.contains_key(&a));
        assert_eq!(
            super::parse_line(b"5\t9\t/x y.jpg"),
            Some((PathBuf::from("/x y.jpg"), (5, 5)))
        );
        assert_eq!(super::parse_line(b"junk"), None);
        // Another window's file: its marks stay when this one's changes are applied, also on
        // the same file.
        let other = super::to_text(&std::collections::HashMap::from([(a.clone(), (1, 0))]));
        let mut map: std::collections::HashMap<_, _> = other
            .split(|&b| b == b'\n')
            .filter_map(super::parse_line)
            .collect();
        super::apply(&mut map, &[a.clone()], super::Change::Mark(4, true));
        super::apply(&mut map, &[b.clone()], super::Change::Stars(4));
        assert_eq!((map[&a], map[&b]), ((5, 0), (0, 4)));
        // A darktable sidecar keeps its fields; the rating is changed or added.
        let dt = r#"<rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="1" darktable:x="y">"#;
        assert!(
            super::set_rating(dt, 4)
                .unwrap()
                .contains(r#"xmp:Rating="4" darktable:x="y""#)
        );
        let none = r#"<rdf:Description rdf:about="">"#;
        assert_eq!(
            super::set_rating(none, 2).unwrap(),
            r#"<rdf:Description xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="2" rdf:about="">"#
        );
        assert!(
            super::set_rating("<xmp:Rating>5</xmp:Rating><rdf:Description>", 0)
                .unwrap()
                .starts_with("<xmp:Rating>0<")
        );
        assert_eq!(super::set_rating("nothing", 1), None);
    }
}
