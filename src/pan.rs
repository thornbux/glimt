//! Pan view (Ctrl+J), as Geeqie's timeline: the images of the list by day, under a heading for
//! each day, oldest first, or as a calendar of the months that have images. The days come from
//! the file dates, or from the EXIF dates.

use crate::*;
use std::ffi::OsString;
use std::path::Path;

/// A day ("2024-05-06", or "No date") and the list indices of its images, in time order.
pub type Day = (String, Vec<usize>);

/// The images of `files` in `dir` by day, read on all cores. `found` gets the days so far every
/// half second and all of them at the end (`true`), so a large folder shows what is read while
/// the rest is.
pub fn timeline(
    dir: &Path,
    files: &[OsString],
    exif: bool,
    stop: &AtomicBool,
    found: impl FnMut(Vec<Day>, bool),
) {
    if exif {
        // EXIF writes "2024:05:06 07:08:09", which sorts as it is.
        // A date that is no day (the blank "    :  :  " of a camera that had none) is none.
        let date = |p: &Path| crate::exif::date(p, false).filter(|d| ymd(d).is_some());
        let day = |d: &String| d.get(..10).unwrap_or(d).replace(':', "-");
        return as_read(dir, files, stop, found, date, day);
    }
    let time = |p: &Path| {
        use std::os::unix::fs::MetadataExt;
        p.metadata().ok().map(|m| m.mtime())
    };
    // The local date once for each quarter of an hour, where every time zone changes.
    let mut last = (i64::MIN, String::new());
    let day = move |&t: &i64| {
        if t.div_euclid(900) != last.0 {
            let local = crate::local_time(t);
            last = (
                t.div_euclid(900),
                local.get(..10).unwrap_or(&local).to_owned(),
            );
        }
        last.1.clone()
    };
    as_read(dir, files, stop, found, time, day);
}

/// `key` of every file, a few thousand at a time on all cores, and the days of those read so
/// far to `found` every half second; the last call has them all.
fn as_read<K: Ord + Clone + Send>(
    dir: &Path,
    files: &[OsString],
    stop: &AtomicBool,
    mut found: impl FnMut(Vec<Day>, bool),
    key: impl Fn(&Path) -> Option<K> + Sync,
    mut day: impl FnMut(&K) -> String,
) {
    let mut keys = Vec::with_capacity(files.len());
    let mut sent = Instant::now();
    let mut chunks = files.chunks(4096);
    loop {
        keys.extend(each(dir, chunks.next().unwrap_or(&[]), stop, &key));
        let done = keys.len() == files.len();
        if done || sent.elapsed() >= std::time::Duration::from_millis(500) {
            found(by_day(keys.clone(), &mut day), done);
            sent = Instant::now();
        }
        if done {
            return;
        }
    }
}

/// `f` of the path of every file, in parallel. Each thread reuses one path: Glimt's allocator
/// has a single arena, so allocating in every thread would make them wait for each other; for
/// the same reason they are two at most then (small_alloc_threads).
fn each<T: Send>(
    dir: &Path,
    files: &[OsString],
    stop: &AtomicBool,
    f: impl Fn(&Path) -> Option<T> + Sync,
) -> Vec<Option<T>> {
    crate::in_parts(files, crate::small_alloc_threads(), |part, _| {
        let mut path = dir.to_path_buf();
        let mut out = Vec::with_capacity(part.len());
        for name in part {
            // Left (the window closed, another folder): nothing more is read for no one.
            if stop.load(Ordering::Relaxed) {
                out.push(None);
                continue;
            }
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

/// The year, month and day of a date "2024-05-06" or EXIF's "2024:05:06 07:08:09"; `None` for
/// one that is no day, such as the 0000:00:00 of a camera with its clock never set.
fn ymd(date: &str) -> Option<(i32, u32, u32)> {
    let mut parts = date.get(..10)?.split(['-', ':']).map(str::parse::<u32>);
    let (Some(Ok(y)), Some(Ok(m)), Some(Ok(day))) = (parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    let ok = (1..=12).contains(&m) && y < 10_000 && (1..=days_in_month(y as i32, m)).contains(&day);
    ok.then_some((y as i32, m, day))
}

/// The months of `days` that have images, oldest first; "No date" has none.
pub fn months(days: &[Day]) -> Vec<Month> {
    let mut out: Vec<Month> = Vec::new();
    for (d, (date, _)) in days.iter().enumerate() {
        let Some((y, m, day)) = ymd(date) else {
            continue;
        };
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

// ---- The pan view's window: the timeline and the calendar ----

impl crate::App {
    /// The pan view (Ctrl+J): the images of the list by day. A click shows one, a double click
    /// also closes the view.
    pub(crate) fn pan_view(&mut self, ctx: &egui::Context) {
        // The thumbnail workers make the cells drawn last frame (a frame late is soon enough);
        // this frame's are noted as they are drawn.
        let mut view = std::mem::take(&mut self.list.pan_thumbs);
        view.sort_unstable();
        view.dedup();
        self.thumb_view.lock().unwrap()[1] = view;
        let Some(mut pan) = self.pan_window.take() else {
            return;
        };
        // The days again when the list changed, also only its order; only the visible rows are
        // drawn.
        let stale = (pan.days.as_ref()).is_none_or(|d| d.1 != self.list.version);
        // Days still being made for a list that changed: stopped, so the new list's start now
        // instead of after every old file is read.
        if pan.days_rx.is_some() && pan.days_for != self.list.version {
            pan.days_stop.store(true, Ordering::Relaxed);
            pan.days_rx.clear();
        }
        if stale && pan.days_rx.is_none() {
            let (tx, rx) = mpsc::channel();
            let (dir, files, exif) = (self.dir.clone(), self.list.files.clone(), pan.exif);
            let (ctx, made) = (ctx.clone(), self.list.version);
            pan.days_for = made;
            let pointer = Working::new(&self.busy);
            pan.days_stop = Arc::default();
            let stop = pan.days_stop.clone();
            std::thread::spawn(move || {
                timeline(&dir, &files, exif, &stop, |days, done| {
                    let _ = tx.send((days, made, done));
                    ctx.request_repaint();
                });
                drop(pointer);
            });
            pan.days_rx = Receiving::of(rx);
        }
        // The days so far, the last ones sent; days for a list that changed meanwhile are taken
        // as old, and made again.
        let mut gone = false;
        loop {
            match pan.days_rx.next() {
                Took::Answer((days, made, done)) => {
                    pan.months = months(&days);
                    pan.days = Some((days, made));
                    pan.columns = 0;
                    if done {
                        pan.days_rx.clear();
                    }
                }
                Took::Nothing => break,
                Took::Gone => {
                    gone = true;
                    break;
                }
            }
        }
        // The thread broke: what it had found stays, or no days for this list, so they are not
        // made again at once; the user told.
        if gone {
            if pan.days.as_ref().is_none_or(|d| d.1 != self.list.version) {
                pan.days = Some((Vec::new(), self.list.version));
                pan.months = Vec::new();
            }
            self.stopped(tr("The Pan view's days"));
        }
        let (mut open, mut show, mut close) = (true, None, false);
        egui::Window::new(tr("Pan View"))
            .open(&mut open)
            .default_size([900.0, 640.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let tab = ui.selectable_value(&mut pan.calendar, false, tr("Timeline"));
                    crate::noted(&tab, tr("Timeline"));
                    let tab = ui.selectable_value(&mut pan.calendar, true, tr("Calendar"));
                    crate::noted(&tab, tr("Calendar"));
                    ui.separator();
                    let exif = ui.checkbox(&mut pan.exif, tr("EXIF date"));
                    if crate::noted(&exif, tr("EXIF date")).changed() {
                        pan.days = None;
                        pan.days_rx.clear();
                        pan.days_stop.store(true, Ordering::Relaxed);
                    }
                    // Days of an older list are not shown while new ones are made: they point
                    // at other images now.
                    match pan.days.as_ref().filter(|d| d.1 == self.list.version) {
                        Some((days, _)) => {
                            let n = days.iter().filter(|d| d.0 != "No date").count();
                            let total: usize = days.iter().map(|d| d.1.len()).sum();
                            ui.weak(tf!(
                                "{images} on {days}",
                                images = count(total, "image"),
                                days = count(n, "day")
                            ))
                        }
                        None => ui.spinner(),
                    };
                    // Still being read: the days shown are those read so far.
                    if pan.days.is_some() && pan.days_rx.is_some() {
                        ui.spinner();
                    }
                });
                ui.separator();
                let Some((days, _)) = pan.days.as_ref().filter(|d| d.1 == self.list.version) else {
                    return;
                };
                let cell = self.cell();
                if pan.calendar {
                    if let Some(d) = self.pan_calendar(ui, &pan.months, days) {
                        // The day in the timeline, which has all its images.
                        (pan.calendar, pan.jump) = (false, Some(d));
                    }
                    return;
                }
                let heading = 26.0;
                let columns = ((ui.available_width() / cell.x) as usize).max(1);
                if columns != pan.columns {
                    (pan.rows, pan.height) = layout(days, columns, heading, cell.y);
                    pan.columns = columns;
                }
                let mut area = egui::ScrollArea::vertical().auto_shrink(false);
                if let Some(d) = pan.jump.take()
                    && let Some(row) = pan.rows.iter().find(|r| r.1 == Row::Heading(d))
                {
                    area = area.vertical_scroll_offset(row.0);
                }
                area.show_viewport(ui, |ui, view| {
                    ui.set_height(pan.height);
                    let top = ui.min_rect().min;
                    let first = pan.rows.partition_point(|r| r.0 + cell.y < view.min.y);
                    for &(y, row) in &pan.rows[first..] {
                        if y > view.max.y {
                            break;
                        }
                        match row {
                            Row::Heading(d) => {
                                let (day, list) = &days[d];
                                ui.painter().text(
                                    top + egui::vec2(4.0, y + heading - 6.0),
                                    egui::Align2::LEFT_BOTTOM,
                                    format!(
                                        "{}   {}",
                                        if day == "No date" { tr("No date") } else { day },
                                        count(list.len(), "image")
                                    ),
                                    egui::TextStyle::Body.resolve(ui.style()),
                                    ui.visuals().strong_text_color(),
                                );
                            }
                            Row::Cells(d, start) => {
                                let list = &days[d].1;
                                let end = (start + columns).min(list.len());
                                for (j, &i) in list[start..end].iter().enumerate() {
                                    if i >= self.list.files.len() {
                                        continue;
                                    }
                                    let at = top + egui::vec2(j as f32 * cell.x, y);
                                    let rect = egui::Rect::from_min_size(at, cell);
                                    self.list.pan_thumbs.push(i);
                                    let r = self.file_cell(ui, i, rect, i == self.list.cur);
                                    if r.clicked() {
                                        show = Some(i);
                                    }
                                    if r.double_clicked() {
                                        (show, close) = (Some(i), true);
                                    }
                                }
                            }
                        }
                    }
                });
            });
        // Its cells' thumbnails are let go here too: the file list, which also does it, may be
        // hidden (Hide File List). Kept are the same as there, so the two never undo each other.
        self.trim_thumbs(self.list_rows.clone());
        if let Some(i) = show {
            self.go(ctx, i);
        }
        if open && !close {
            self.pan_window = Some(pan);
        }
    }

    /// The calendar of the Pan view: the months with images, a week to a row, each day with its
    /// first image and how many it has. Only the months in view are drawn. The day clicked.
    pub(crate) fn pan_calendar(
        &mut self,
        ui: &mut egui::Ui,
        months: &[Month],
        days: &[Day],
    ) -> Option<usize> {
        const NAMES: [&str; 12] = [
            "January",
            "February",
            "March",
            "April",
            "May",
            "June",
            "July",
            "August",
            "September",
            "October",
            "November",
            "December",
        ];
        let cell = self.cell();
        let w = (ui.available_width() / 7.0).min(cell.x);
        let h = w * cell.y / cell.x;
        let (title, names) = (30.0, 22.0);
        let tall = |m: &Month| title + names + m.weeks() as f32 * h;
        let total: f32 = months.iter().map(tall).sum();
        let mut picked = None;
        egui::ScrollArea::vertical()
            .id_salt("calendar")
            .auto_shrink(false)
            .show_viewport(ui, |ui, view| {
                ui.set_height(total);
                let top = ui.min_rect().min;
                let font = egui::TextStyle::Small.resolve(ui.style());
                let mut y = 0.0;
                for m in months {
                    let (y0, height) = (y, tall(m));
                    y += height;
                    if y0 + height < view.min.y {
                        continue;
                    }
                    if y0 > view.max.y {
                        break;
                    }
                    let strong = ui.visuals().strong_text_color();
                    let weak = ui.visuals().weak_text_color();
                    ui.painter().text(
                        top + egui::vec2(4.0, y0 + title - 6.0),
                        egui::Align2::LEFT_BOTTOM,
                        format!("{} {}", tr(NAMES[m.month as usize - 1]), m.year),
                        egui::TextStyle::Body.resolve(ui.style()),
                        strong,
                    );
                    for (k, day) in ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]
                        .iter()
                        .enumerate()
                    {
                        ui.painter().text(
                            top + egui::vec2((k as f32 + 0.5) * w, y0 + title + names - 4.0),
                            egui::Align2::CENTER_BOTTOM,
                            tr(day),
                            font.clone(),
                            weak,
                        );
                    }
                    for day in 1..=m.days {
                        let slot = m.first + day - 1;
                        let at = egui::vec2(
                            (slot % 7) as f32 * w,
                            y0 + title + names + (slot / 7) as f32 * h,
                        );
                        let rect =
                            egui::Rect::from_min_size(top + at, egui::vec2(w, h)).shrink(2.0);
                        // A faint square for every day, so the weeks read as a grid.
                        ui.painter()
                            .rect_filled(rect, 3.0, ui.visuals().faint_bg_color);
                        let found = (m.images.iter().find(|x| x.0 == day))
                            .and_then(|&(_, d)| Some((d, *days.get(d)?.1.first()?)))
                            .filter(|&(_, i)| i < self.list.files.len());
                        let Some((d, first)) = found else {
                            ui.painter().text(
                                rect.min + egui::vec2(4.0, 2.0),
                                egui::Align2::LEFT_TOP,
                                day.to_string(),
                                font.clone(),
                                weak,
                            );
                            continue;
                        };
                        let r = ui.interact(rect, ui.id().with(("day", d)), egui::Sense::click());
                        if r.hovered() {
                            ui.painter().rect_filled(
                                rect,
                                3.0,
                                ui.visuals().widgets.hovered.weak_bg_fill,
                            );
                        }
                        self.list.pan_thumbs.push(first);
                        self.paint_thumb(ui, first, rect.shrink(4.0));
                        // The day and the number of images, on dark so they read on any picture.
                        let p = ui.painter();
                        let dark = egui::Color32::from_black_alpha(170);
                        for (text, align, corner) in [
                            (
                                day.to_string(),
                                egui::Align2::LEFT_TOP,
                                rect.min + egui::vec2(6.0, 6.0),
                            ),
                            (
                                thousands(days[d].1.len()),
                                egui::Align2::RIGHT_BOTTOM,
                                rect.max - egui::vec2(6.0, 6.0),
                            ),
                        ] {
                            let g = p.layout_no_wrap(text, font.clone(), egui::Color32::WHITE);
                            let at = align.anchor_size(corner, g.size());
                            p.rect_filled(at.expand(2.0), 3.0, dark);
                            p.galley(at.min, g, egui::Color32::WHITE);
                        }
                        if r.on_hover_text(count(days[d].1.len(), "image")).clicked() {
                            picked = Some(d);
                        }
                    }
                }
            });
        picked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stopped, nothing more is read: every file after gets no value.
    #[test]
    fn each_stops_when_told() {
        let files: Vec<OsString> = (0..4).map(|i| i.to_string().into()).collect();
        let stop = AtomicBool::new(false);
        let count = AtomicUsize::new(0);
        let got = each(Path::new("/"), &files, &stop, |_| {
            // The first file read sets the flag: the rest are left.
            stop.store(true, Ordering::Relaxed);
            Some(count.fetch_add(1, Ordering::Relaxed))
        });
        assert_eq!(got.len(), 4);
        assert!(got.iter().filter(|g| g.is_some()).count() <= crate::small_alloc_threads());
    }

    /// A file whose reader panics gets no value; the others keep their own.
    #[test]
    fn each_keeps_places_past_a_panic() {
        let files: Vec<OsString> = (0..9).map(|i| i.to_string().into()).collect();
        let got = each(
            Path::new("/"),
            &files,
            &AtomicBool::new(false),
            |p| match p.ends_with("4") {
                true => panic!("broken file"),
                false => Some(p.to_path_buf()),
            },
        );
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
        let mut sent = Vec::new();
        timeline(&dir, &names, false, &AtomicBool::new(false), |d, done| {
            sent.push((d, done))
        });
        // One call with them all; an empty list gets its call too, so the view never waits.
        assert_eq!(sent.len(), 1);
        let (days, done) = sent.pop().unwrap();
        assert!(done);
        let mut empty = Vec::new();
        timeline(&dir, &[], false, &AtomicBool::new(false), |d, done| {
            empty.push((d, done))
        });
        assert_eq!(empty, [(Vec::new(), true)]);
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

    /// Days in either form; the blank date and the never set clock are none.
    #[test]
    fn reads_days() {
        assert_eq!(ymd("2024:05:06 07:08:09"), Some((2024, 5, 6)));
        assert_eq!(ymd("2024-02-29"), Some((2024, 2, 29)));
        assert_eq!(ymd("    :  :     :  :  "), None);
        assert_eq!(ymd("0000:00:00 00:00:00"), None);
        assert_eq!(ymd("No date"), None);
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
