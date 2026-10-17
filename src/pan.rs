//! Pan view (Ctrl+J), as Geeqie's timeline: the images of the list by day, under a heading for
//! each day, oldest first, or as a calendar of the months that have images. The days come from
//! the file dates, or from the EXIF dates.

use std::ffi::OsString;
use std::path::Path;

/// A day ("2024-05-06", or "No date") and the list indices of its images, in time order.
pub type Day = (String, Vec<usize>);

/// The images of `files` in `dir` by day, read on all cores.
pub fn timeline(dir: &Path, files: &[OsString], exif: bool) -> Vec<Day> {
    if exif {
        // EXIF writes "2024:05:06 07:08:09", which sorts as it is.
        let dates = each(dir, files, |p| crate::exif::date(p, false));
        return by_day(dates, |d| d.get(..10).unwrap_or(d).replace(':', "-"));
    }
    let times = each(dir, files, |p| {
        use std::os::unix::fs::MetadataExt;
        p.metadata().ok().map(|m| m.mtime())
    });
    // The local date once for each quarter of an hour, where every time zone changes.
    let mut last = (i64::MIN, String::new());
    by_day(times, |&t| {
        if t.div_euclid(900) != last.0 {
            let local = crate::local_time(t);
            last = (
                t.div_euclid(900),
                local.get(..10).unwrap_or(&local).to_owned(),
            );
        }
        last.1.clone()
    })
}

/// `f` of the path of every file, in parallel. Each thread reuses one path: Glimt's allocator
/// has a single arena, so allocating in every thread would make them wait for each other; for
/// the same reason they are two at most then (small_alloc_threads).
fn each<T: Send>(
    dir: &Path,
    files: &[OsString],
    f: impl Fn(&Path) -> Option<T> + Sync,
) -> Vec<Option<T>> {
    crate::in_parts(files, crate::small_alloc_threads(), |part, _| {
        let mut path = dir.to_path_buf();
        let mut out = Vec::with_capacity(part.len());
        for name in part {
            path.push(name);
            // A file that breaks its reader has no date: a whole part lost would give the
            // files after it the dates of others.
            let found = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&path)));
            out.push(found.ok().flatten());
            path.pop();
        }
        out
    })
}

/// The files in order of `keys`, those without one last, cut into days by `day`.
fn by_day<K: Ord>(keys: Vec<Option<K>>, mut day: impl FnMut(&K) -> String) -> Vec<Day> {
    let mut order: Vec<usize> = (0..keys.len()).collect();
    order.sort_by(|&a, &b| (keys[a].is_none(), &keys[a]).cmp(&(keys[b].is_none(), &keys[b])));
    let mut days: Vec<Day> = Vec::new();
    for i in order {
        let d = keys[i]
            .as_ref()
            .map_or_else(|| "No date".to_owned(), &mut day);
        match days.last_mut() {
            Some((last, list)) if *last == d => list.push(i),
            _ => days.push((d, vec![i])),
        }
    }
    days
}

/// A row of the view: a day's heading, or a row of its thumbnails from this one on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Row {
    Heading(usize),
    Cells(usize, usize),
}

/// The rows of `days` with `columns` thumbnails to a row, each with its top; and the height of
/// them all. Made again only when the number of columns changes.
pub fn layout(days: &[Day], columns: usize, heading: f32, cell: f32) -> (Vec<(f32, Row)>, f32) {
    let (mut rows, mut y) = (Vec::new(), 0.0);
    for (d, (_, list)) in days.iter().enumerate() {
        rows.push((y, Row::Heading(d)));
        y += heading;
        for start in (0..list.len()).step_by(columns.max(1)) {
            rows.push((y, Row::Cells(d, start)));
            y += cell;
        }
    }
    (rows, y)
}

/// A month of the calendar: the weekday of its first day (Monday 0), its number of days, and for
/// each day with images the index of that day in the timeline.
#[derive(Debug, PartialEq)]
pub struct Month {
    pub year: i32,
    pub month: u32,
    pub first: u32,
    pub days: u32,
    pub images: Vec<(u32, usize)>,
}

impl Month {
    /// Weeks it spans: rows of seven days.
    pub fn weeks(&self) -> u32 {
        (self.first + self.days).div_ceil(7)
    }
}

/// The months of `days` that have images, oldest first; "No date" has none.
pub fn months(days: &[Day]) -> Vec<Month> {
    let mut out: Vec<Month> = Vec::new();
    for (d, (date, _)) in days.iter().enumerate() {
        let mut parts = date.splitn(3, '-').map(str::parse::<u32>);
        let (Some(Ok(y)), Some(Ok(m)), Some(Ok(day))) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        // A camera with its clock never set writes 0000:00:00; such a day is in no month.
        if !(1..=12).contains(&m) || y >= 10_000 || !(1..=days_in_month(y as i32, m)).contains(&day)
        {
            continue;
        }
        let y = y as i32;
        match out.last_mut() {
            Some(last) if (last.year, last.month) == (y, m) => last.images.push((day, d)),
            _ => out.push(Month {
                year: y,
                month: m,
                first: weekday(y, m, 1),
                days: days_in_month(y, m),
                images: vec![(day, d)],
            }),
        }
    }
    out
}

/// The weekday of a date, Monday 0 to Sunday 6 (Sakamoto's method).
fn weekday(y: i32, m: u32, d: u32) -> u32 {
    const T: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let y = if m < 3 { y - 1 } else { y };
    let sunday0 = (y + y / 4 - y / 100 + y / 400 + T[m as usize - 1] + d as i32).rem_euclid(7);
    ((sunday0 + 6) % 7) as u32
}

fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file whose reader panics gets no value; the others keep their own.
    #[test]
    fn each_keeps_places_past_a_panic() {
        let files: Vec<OsString> = (0..9).map(|i| i.to_string().into()).collect();
        let got = each(Path::new("/"), &files, |p| match p.ends_with("4") {
            true => panic!("broken file"),
            false => Some(p.to_path_buf()),
        });
        assert_eq!(got.len(), 9);
        assert_eq!(got[4], None);
        assert_eq!(got[5].as_deref(), Some(Path::new("/5")));
    }

    #[test]
    fn groups_by_day() {
        let dir = std::env::temp_dir().join(format!("glimt-test-pan-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let names: Vec<OsString> = ["a", "b", "c", "gone"].map(OsString::from).to_vec();
        for (name, time) in [
            ("a", 1_700_000_000),
            ("b", 1_600_000_000),
            ("c", 1_700_000_100),
        ] {
            let f = std::fs::File::create(dir.join(name)).unwrap();
            let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(time);
            f.set_modified(t).unwrap();
        }
        let days = timeline(&dir, &names, false);
        // b is years older; a and c on one day, in time order; the missing file last.
        let lists: Vec<&[usize]> = days.iter().map(|d| d.1.as_slice()).collect();
        assert_eq!(lists, [&[1][..], &[0, 2], &[3]]);
        assert_eq!(days[2].0, "No date");
        let (rows, height) = layout(&days, 1, 20.0, 100.0);
        assert_eq!(rows[2], (120.0, Row::Heading(1)));
        assert_eq!(rows[4], (240.0, Row::Cells(1, 1)));
        assert_eq!(height, 3.0 * 20.0 + 4.0 * 100.0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Months with their weekdays and leap years; days in two months, and one without a date.
    #[test]
    fn makes_months() {
        let day = |s: &str| (s.to_owned(), vec![0]);
        let days = [
            day("2024-02-29"),
            day("2025-07-01"),
            day("2025-07-15"),
            day("No date"),
            // A clock never set, a day that does not exist, a month 13, a year of overflow.
            day("0000-00-00"),
            day("2023-02-29"),
            day("2025-13-01"),
            day("2147483647-01-01"),
        ];
        let m = months(&days);
        assert_eq!(m.len(), 2);
        // 1 February 2024 was a Thursday, 1 July 2025 a Tuesday.
        assert_eq!((m[0].first, m[0].days, m[0].weeks()), (3, 29, 5));
        assert_eq!((m[1].first, m[1].days), (1, 31));
        assert_eq!(m[1].images, [(1, 1), (15, 2)]);
        assert_eq!(weekday(2000, 1, 1), 5);
        assert_eq!(days_in_month(1900, 2), 28);
    }
}
