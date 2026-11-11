//! Text for the user: sizes, counts, times, a text cut to fit, templates filled in, and names
//! made from a file's bytes.

use crate::support::lang::{self, tr};

/// A piece of a text with `%` codes, as `pieces` reads it.
#[derive(Debug, PartialEq)]
pub(crate) enum Piece<'a> {
    /// Text as it stands. A `%%` is the text "%".
    Text(&'a str),
    /// A code without its `%` signs, such as `name` of `%name%` or `f` of `%f`.
    Code(&'a str),
}

/// A size for the status bar, or "calculating…" while it is summed (`None`).
pub(crate) fn size_text(bytes: Option<u64>) -> String {
    bytes.map_or(tr("calculating…").to_owned(), human_size)
}

/// Size as in the file manager: "850.0 kB", "2.3 MB", "1.2 GB" (decimal units), with the
/// decimal mark of the language in use.
pub(crate) fn human_size(bytes: u64) -> String {
    // A unit starts where the one below would round to 1000.0, so 999,950 bytes are "1.0 MB".
    let (value, unit) = match bytes {
        b if b >= 999_950_000 => (b as f64 / 1e9, "GB"),
        b if b >= 999_950 => (b as f64 / 1e6, "MB"),
        b if b >= 1_000 => (b as f64 / 1e3, "kB"),
        b => return format!("{b} B"),
    };
    format!("{value:.1} {unit}").replacen('.', lang::number_separators().1, 1)
}

/// Integer with its thousands grouped as the language in use writes them: 300000 → "300,000"
/// in English, "300.000" in German.
pub(crate) fn thousands(n: usize) -> String {
    grouped(n, lang::number_separators().0)
}

/// Returns `n` with `separator` between each group of three digits.
fn grouped(n: usize, separator: &str) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push_str(separator);
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

/// Returns the pieces of `text` from left to right: the text, and the codes in it. With
/// `named_codes`, as in the templates, a code is `%name%` for a name that `named_codes` takes.
/// A `%` before another name is text, and the `%` after that name starts the next code, so
/// "100% of %pages%" keeps its page count. Without it, as in a desktop file's Exec, a code is a
/// `%` with the one character after it. A `%%` is a `%` of its own in both, never the start
/// or the end of a code, so `%page%%pages%` is two codes and `%%f` is the text "%f". A `%`
/// that starts no code is text. Every template and Exec word is read here, so the rule for
/// `%%` is held in one place.
pub(crate) fn pieces(
    text: &str,
    named_codes: Option<fn(&str) -> bool>,
) -> impl Iterator<Item = Piece<'_>> {
    let mut rest = text;
    let closed = named_codes.is_some();
    std::iter::from_fn(move || {
        let at = rest.find('%');
        if at != Some(0) {
            let end = at.unwrap_or(rest.len());
            let (piece, after) = rest.split_at(end);
            rest = after;
            return (!piece.is_empty()).then_some(Piece::Text(piece));
        }
        let after = &rest[1..];
        let length = match closed {
            true => after.find('%'),
            false => after.chars().next().map(char::len_utf8),
        };
        let Some(length) = length else {
            return Some(Piece::Text(std::mem::take(&mut rest)));
        };
        let code = &after[..length];
        if named_codes.is_some_and(|is_code| !code.is_empty() && !is_code(code)) {
            let (piece, after) = rest.split_at(1 + length);
            rest = after;
            return Some(Piece::Text(piece));
        }
        rest = &after[length + closed as usize..];
        Some(match code {
            "" | "%" => Piece::Text("%"),
            code => Piece::Code(code),
        })
    })
}

/// `template` (see FULL_SCREEN_INFO_TEMPLATE) with each `%field%` replaced by `value(field)`.
/// A `%%` is a `%` of its own, not a field, so it never takes its part along.
pub(crate) fn fill_template(template: &str, value: impl Fn(&str) -> String) -> String {
    let lines = template.split("\\n").filter_map(|line| {
        let parts: Vec<String> = (line.split('|'))
            .filter_map(|part| {
                let (mut fields, mut filled) = (0, 0);
                // Any name is a field, as an EXIF label is one.
                let out: String = (pieces(part, Some(|_| true)))
                    .map(|piece| match piece {
                        Piece::Text(text) => text.to_owned(),
                        Piece::Code(field) => {
                            let v = value(field);
                            fields += 1;
                            filled += !v.is_empty() as usize;
                            v
                        }
                    })
                    .collect();
                (fields == 0 || filled > 0).then_some(out)
            })
            .map(|p| p.trim().to_owned())
            .filter(|p| !p.is_empty())
            .collect();
        (!parts.is_empty()).then(|| parts.join(" · "))
    });
    lines.collect::<Vec<_>>().join("\n")
}

/// Seconds since 1970 as local "2026-10-11 00:34:12". Undo compares this form, with a T for
/// the space, with the trash's DeletionDate: keep it so.
pub(crate) fn local_time(secs: i64) -> String {
    // A time that the system's time_t cannot hold (after 2038 with a 32-bit one) has no date.
    let Some(time) = libc::time_t::try_from(secs).ok() else {
        return String::new();
    };
    #[allow(unsafe_code)]
    // SAFETY: libc's tm is the system's struct tm, all of whose fields may be zero, and both
    // pointers are valid for the call.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&time, &mut tm).is_null() {
            return String::new();
        }
        tm
    };
    format!(
        "{}-{:02}-{:02} {:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

/// Seconds since 1970 as UTC in RFC 3339, such as "2026-11-10T08:05:09Z", for scripts to read.
pub(crate) fn utc_time(secs: i64) -> String {
    let (days, second_of_day) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // The calendar date of a day count, by Howard Hinnant's civil_from_days. Its years start
    // in March, so that the leap day falls at their end.
    let from_year_0 = days + 719_468;
    let (era, day_of_era) = (
        from_year_0.div_euclid(146_097),
        from_year_0.rem_euclid(146_097),
    );
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_from_march = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_from_march + 2) / 5 + 1;
    let month = if month_from_march < 10 {
        month_from_march + 3
    } else {
        month_from_march - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        second_of_day / 3600,
        second_of_day / 60 % 60,
        second_of_day % 60
    )
}

/// Returns `text` with each control character, such as a line break, turned into "?". A name
/// made from a file's bytes goes through it, so it shows on one line and cannot add a line, and
/// with it a word, to a file that holds one name to a line.
pub(crate) fn without_control_characters(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    /// The full screen info's and the print's template broken in thousands of ways: no panic.
    #[test]
    fn survives_broken_templates() {
        let seed = b"%name% | %exif:DateTimeOriginal%\\n[%keywords%] %stars% %%";
        for bytes in crate::support::test_support::broken(seed) {
            super::fill_template(&String::from_utf8_lossy(&bytes), |_| "x".into());
        }
    }

    /// The one reader of `%` codes: `%%` is a `%` and never part of a code, also beside one.
    #[test]
    fn reads_percent_codes() {
        use super::Piece::{Code, Text};
        let any: Option<fn(&str) -> bool> = Some(|_| true);
        let read = |text, named| super::pieces(text, named).collect::<Vec<_>>();
        assert_eq!(read("%page%%pages%", any), [Code("page"), Code("pages")]);
        assert_eq!(
            read("a %% %b", any),
            [Text("a "), Text("%"), Text(" "), Text("%b")]
        );
        assert_eq!(
            read("%%f %f[0]", None),
            [Text("%"), Text("f "), Code("f"), Text("[0]")]
        );
        assert_eq!(read("100%", None), [Text("100"), Text("%")]);
        assert!(read("", any).is_empty());
        // A name that is no code is text, and its closing `%` starts the next code.
        let pages: Option<fn(&str) -> bool> = Some(|name| name == "pages");
        assert_eq!(
            read("1% of %pages%", pages),
            [Text("1"), Text("% of "), Code("pages")]
        );
    }

    /// The templates of the full screen info, the caption and the title are filled as 0.75.0
    /// filled them, wherever they have no `%%`, which 0.76.0 made a `%`.
    #[test]
    fn templates_are_filled_as_before() {
        // 0.75.0's fill_template, as it was.
        let old = |template: &str, value: &dyn Fn(&str) -> String| {
            let lines = template.split("\\n").filter_map(|line| {
                let parts: Vec<String> = (line.split('|'))
                    .filter_map(|part| {
                        let (mut out, mut rest, mut fields, mut filled) =
                            (String::new(), part, 0, 0);
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
        };
        let value = |f: &str| match f {
            "name" => "a.jpg".to_owned(),
            "ISO" => "200".to_owned(),
            _ => String::new(),
        };
        for template in [
            crate::files::config::FULL_SCREEN_INFO_TEMPLATE,
            "Scale 100% · %name% | %ISO%",
            "50% %name% 20%",
            "%name%%ISO%|%comment%\\n%keywords% 5%",
            "%ISO",
            "a % b % c %name%",
        ] {
            assert_eq!(super::fill_template(template, value), old(template, &value));
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
        assert_eq!(
            fill(crate::files::config::FULL_SCREEN_INFO_TEMPLATE),
            "a.jpg\nISO 200"
        );
        assert_eq!(
            fill("%Taken%|x %name%|%comment%\\n%keywords%\\n50%"),
            "x a.jpg\n50%"
        );
        assert_eq!(fill(""), "");
        // "%%" is a "%", which a part keeps, and not an empty field that takes it along.
        assert_eq!(fill("50%% of 100%%"), "50% of 100%");
        assert_eq!(
            fill("%name% 100%%|%Taken% %%|%%%name%"),
            "a.jpg 100% · %a.jpg"
        );
    }

    #[test]
    fn formats_sizes() {
        assert_eq!(super::human_size(512), "512 B");
        assert_eq!(super::human_size(850_000), "850.0 kB");
        assert_eq!(super::human_size(2_345_678), "2.3 MB");
        assert_eq!(super::human_size(1_200_000_000), "1.2 GB");
        assert_eq!(super::human_size(999_949), "999.9 kB");
        assert_eq!(super::human_size(999_950), "1.0 MB");
        assert_eq!(super::human_size(999_950_000), "1.0 GB");
    }

    #[test]
    fn formats_thousands() {
        assert_eq!(super::thousands(7), "7");
        assert_eq!(super::thousands(1000), "1,000");
        assert_eq!(super::thousands(300000), "300,000");
        assert_eq!(super::thousands(1234567), "1,234,567");
        assert_eq!(super::grouped(1234567, "."), "1.234.567");
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
    fn formats_utc_time() {
        assert_eq!(super::utc_time(0), "1970-01-01T00:00:00Z");
        assert_eq!(super::utc_time(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(super::utc_time(1_762_790_709), "2025-11-10T16:05:09Z");
        assert_eq!(super::utc_time(-1), "1969-12-31T23:59:59Z");
    }

    #[test]
    fn formats_local_time() {
        let t = super::local_time(1_700_000_000);
        assert_eq!(t.len(), "2023-11-14 22:13:20".len(), "{t}");
        assert!(t.starts_with("2023-11-1"), "{t}");
    }
}
