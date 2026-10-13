//! Pan view (Ctrl+J), as Geeqie's timeline: the images of the list by day, under a heading for
//! each day, oldest first. The days come from the file dates, or from the EXIF dates.

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

/// `f` of the path of every file, on all cores. Each thread reuses one path: Glimt's allocator
/// has a single arena, so allocating in every thread would make them wait for each other.
fn each<T: Send>(
    dir: &Path,
    files: &[OsString],
    f: impl Fn(&Path) -> Option<T> + Sync,
) -> Vec<Option<T>> {
    let threads = crate::background_threads();
    let chunk = files.len().div_ceil(threads).max(1);
    std::thread::scope(|s| {
        let parts: Vec<_> = (files.chunks(chunk))
            .map(|part| {
                let f = &f;
                s.spawn(move || {
                    let mut path = dir.to_path_buf();
                    let mut out = Vec::with_capacity(part.len());
                    for name in part {
                        path.push(name);
                        out.push(f(&path));
                        path.pop();
                    }
                    out
                })
            })
            .collect();
        parts
            .into_iter()
            .flat_map(|p| p.join().unwrap_or_default())
            .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
