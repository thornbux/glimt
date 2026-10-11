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
        self.change(paths, |(bits, stars)| {
            (if all { bits & !bit } else { bits | bit }, stars)
        });
    }

    pub fn rate(&mut self, paths: &[PathBuf], stars: u8) {
        self.change(paths, |(bits, _)| (bits, stars));
    }

    fn change(&mut self, paths: &[PathBuf], f: impl Fn((u8, u8)) -> (u8, u8)) {
        for path in paths {
            match f(self.get(path)) {
                (0, 0) => drop(self.map.remove(path)),
                v => drop(self.map.insert(path.clone(), v)),
            }
        }
        self.save();
    }

    /// Written in the background, renamed into place so a crash never leaves half a file.
    /// ponytail: the whole file each time; fine for thousands of marked files.
    #[cfg(not(test))]
    fn save(&self) {
        let Some(file) = file() else { return };
        let mut text = Vec::new();
        for (path, (bits, stars)) in &self.map {
            let path = path.as_os_str().as_encoded_bytes();
            if !path.contains(&b'\n') {
                text.extend(format!("{bits}\t{stars}\t").as_bytes());
                text.extend(path);
                text.push(b'\n');
            }
        }
        std::thread::spawn(move || {
            let tmp = file.with_extension("tmp");
            let _ = std::fs::create_dir_all(file.parent().unwrap_or(Path::new(".")));
            if std::fs::write(&tmp, text).is_ok() {
                let _ = std::fs::rename(tmp, file);
            }
        });
    }
}

#[cfg(test)]
impl Marks {
    fn save(&self) {}
}

fn parse_line(line: &[u8]) -> Option<(PathBuf, (u8, u8))> {
    let mut parts = line.splitn(3, |&b| b == b'\t');
    let number = |b: &[u8]| std::str::from_utf8(b).ok()?.parse::<u8>().ok();
    let (bits, stars) = (number(parts.next()?)?, number(parts.next()?)?);
    // SAFETY: the bytes were written from an OsStr by `save`, or come from the user's file;
    // on Unix any bytes are a valid OsStr.
    let path = unsafe { std::ffi::OsStr::from_encoded_bytes_unchecked(parts.next()?) };
    Some((PathBuf::from(path), (bits & 0x3f, stars.min(5))))
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
    }
}
