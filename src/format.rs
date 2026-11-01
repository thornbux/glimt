//! Text for the user: sizes, counts, times, a text cut to fit, and templates filled in.

use crate::lang::{self, tr};

/// A size for the status bar, or "calculating…" while it is summed (`None`).
pub(crate) fn size_text(bytes: Option<u64>) -> String {
    bytes.map_or(tr("calculating…").to_owned(), human_size)
}

/// Size as in the file manager: "850.0 kB", "2.3 MB", "1.2 GB" (decimal units).
pub(crate) fn human_size(bytes: u64) -> String {
    let (value, unit) = match bytes {
        b if b >= 1_000_000_000 => (b as f64 / 1e9, "GB"),
        b if b >= 1_000_000 => (b as f64 / 1e6, "MB"),
        b if b >= 1_000 => (b as f64 / 1e3, "kB"),
        b => return format!("{b} B"),
    };
    format!("{value:.1} {unit}")
}

/// Integer with comma thousands separators: 300000 → "300,000".
pub(crate) fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `n` and `noun` in the singular or plural, with thousands separated: "1 image", "1,200 images".
pub(crate) fn count(n: usize, noun: &'static str) -> String {
    format!("{} {}", thousands(n), lang::plural(noun, n))
}

/// `text` as it is if it `fits`, else its start and end around "…", as long as fits; found by
/// halving, so a row lays out a few texts, not one for each letter.
pub(crate) fn middle_cut(text: &str, fits: impl Fn(&str) -> bool) -> String {
    if fits(text) {
        return text.to_owned();
    }
    let chars: Vec<char> = text.chars().collect();
    let cut = |keep: usize| -> String {
        let tail = keep / 2;
        let head: String = chars[..keep - tail].iter().collect();
        let end: String = chars[chars.len() - tail..].iter().collect();
        format!("{head}…{end}")
    };
    let (mut lo, mut hi) = (0, chars.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if fits(&cut(mid)) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    cut(lo)
}

/// `template` (see OSD_TEMPLATE) with each `%field%` replaced by `value(field)`.
pub(crate) fn fill_template(template: &str, value: impl Fn(&str) -> String) -> String {
    let lines = template.split("\\n").filter_map(|line| {
        let parts: Vec<String> = (line.split('|'))
            .filter_map(|part| {
                let (mut out, mut rest, mut fields, mut filled) = (String::new(), part, 0, 0);
                while let Some(a) = rest.find('%')
                    && let Some(b) = rest[a + 1..].find('%')
                {
                    let v = value(&rest[a + 1..a + 1 + b]);
                    fields += 1;
                    filled += !v.is_empty() as usize;
                    out += &rest[..a];
                    out += &v;
                    rest = &rest[a + b + 2..];
                }
                out += rest;
                (fields == 0 || filled > 0).then_some(out)
            })
            .map(|p| p.trim().to_owned())
            .filter(|p| !p.is_empty())
            .collect();
        (!parts.is_empty()).then(|| parts.join(" · "))
    });
    lines.collect::<Vec<_>>().join("\n")
}

#[repr(C)]
struct Tm {
    sec: i32,
    min: i32,
    hour: i32,
    mday: i32,
    mon: i32,
    year: i32,
    wday: i32,
    yday: i32,
    isdst: i32,
    gmtoff: i64,
    zone: *const std::ffi::c_char,
}

unsafe extern "C" {
    fn localtime_r(time: *const i64, tm: *mut Tm) -> *mut Tm;
}

/// Seconds since 1970 as local "2026-10-11 00:34:12". Undo compares this form, with a T for
/// the space, with the trash's DeletionDate: keep it so.
pub(crate) fn local_time(secs: i64) -> String {
    // SAFETY: Tm matches glibc's struct tm on 64-bit Linux, and both pointers are valid.
    let tm = unsafe {
        let mut tm: Tm = std::mem::zeroed();
        if localtime_r(&secs, &mut tm).is_null() {
            return String::new();
        }
        tm
    };
    format!(
        "{}-{:02}-{:02} {:02}:{:02}:{:02}",
        tm.year + 1900,
        tm.mon + 1,
        tm.mday,
        tm.hour,
        tm.min,
        tm.sec
    )
}

#[cfg(test)]
mod tests {
    /// The full screen info's and the print's template broken in thousands of ways: no panic.
    #[test]
    fn survives_broken_templates() {
        let seed = b"%name% | %exif:DateTimeOriginal%\\n[%keywords%] %stars% %%";
        for bytes in crate::broken(seed) {
            super::fill_template(&String::from_utf8_lossy(&bytes), |_| "x".into());
        }
    }

    /// The overlay template: empty fields take their part along, an empty line goes.
    #[test]
    fn fills_templates() {
        let value = |f: &str| match f {
            "name" => "a.jpg".to_owned(),
            "ISO" => "200".to_owned(),
            _ => String::new(),
        };
        let fill = |t| super::fill_template(t, value);
        assert_eq!(fill(crate::config::OSD_TEMPLATE), "a.jpg\nISO 200");
        assert_eq!(
            fill("%Taken%|x %name%|%comment%\\n%keywords%\\n50%"),
            "x a.jpg\n50%"
        );
        assert_eq!(fill(""), "");
    }

    #[test]
    fn formats_sizes() {
        assert_eq!(super::human_size(512), "512 B");
        assert_eq!(super::human_size(850_000), "850.0 kB");
        assert_eq!(super::human_size(2_345_678), "2.3 MB");
        assert_eq!(super::human_size(1_200_000_000), "1.2 GB");
    }

    #[test]
    fn formats_thousands() {
        assert_eq!(super::thousands(7), "7");
        assert_eq!(super::thousands(1000), "1,000");
        assert_eq!(super::thousands(300000), "300,000");
        assert_eq!(super::thousands(1234567), "1,234,567");
        assert_eq!(super::count(1, "image"), "1 image");
        // One letter is one unit wide here.
        let fits = |n: usize| move |s: &str| s.chars().count() <= n;
        assert_eq!(
            super::middle_cut("fjord_sunrise.jpg", fits(20)),
            "fjord_sunrise.jpg"
        );
        assert_eq!(
            super::middle_cut("fjord_sunrise.jpg", fits(11)),
            "fjord…e.jpg"
        );
        assert_eq!(super::middle_cut("fjørd_sunrise.jpg", fits(5)), "fj…pg");
        assert_eq!(super::middle_cut("abc", fits(0)), "…");
        assert_eq!(super::count(0, "image"), "0 images");
        assert_eq!(super::count(1200, "file"), "1,200 files");
    }

    #[test]
    fn formats_local_time() {
        let t = super::local_time(1_700_000_000);
        assert_eq!(t.len(), "2023-11-14 22:13:20".len(), "{t}");
        assert!(t.starts_with("2023-11-1"), "{t}");
    }
}
