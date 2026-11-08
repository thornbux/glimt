//! Pan view (Ctrl+J), as Geeqie's timeline: the images of the list by day, under a heading for
//! each day, oldest first, or as a calendar of the months that have images. The days come from
//! the file dates, or from the EXIF dates.

use crate::support::format::{count, thousands};
use crate::support::lang::tr;
use crate::support::threads::{Receiving, Took, Working};
use crate::{tf, viewer::pan};
use eframe::egui;
use std::borrow::Cow;
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Instant;

/// The months' names, translated where they are shown.
const MONTHS: [&str; 12] = [
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

/// A day ("2024-05-06", or "No date") and the list indices of its images, in time order.
pub type Day = (String, Vec<usize>);

/// What a file's day is read from: its EXIF date with 0, or "" with its modification time.
/// Either sorts the files in time order.
type DateKey = (String, i64);

/// The date of each file name read so far, `None` for a file without one.
type KnownDates = HashMap<OsString, Option<DateKey>>;

/// A row of the view: a day's heading, or a row of its thumbnails from this one on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Row {
    Heading(usize),
    Cells(usize, usize),
}

/// A month of the calendar: the weekday of its first day (Monday 0), its number of days, and for
/// each day with images the index of that day in the timeline.
#[derive(Debug, PartialEq)]
pub struct Month {
    pub year: i32,
    pub month: u32,
    pub first_weekday: u32,
    pub day_count: u32,
    pub images: Vec<(u32, usize)>,
}

impl Month {
    /// Weeks it spans: rows of seven days.
    pub fn weeks(&self) -> u32 {
        (self.first_weekday + self.day_count).div_ceil(7)
    }
}

/// The pan view's window (Ctrl+J). Closed, the days being made for it are not read on.
#[derive(Default)]
pub(crate) struct PanView {
    /// Days from the EXIF dates instead of the file dates.
    uses_exif_dates: bool,
    /// The days, and the version of the file list they are for (`List::version`).
    days: Option<(Vec<pan::Day>, usize)>,
    /// The days read so far, and with the last of them the dates the reading knows.
    days_rx: Receiving<(Vec<pan::Day>, Option<KnownDates>)>,
    /// The dates read, by file name, as the `exif:` index keeps its text. A list that changes
    /// (a rename, a refresh, a sort) then reads only the names that are new to it. While the
    /// days are read, the reading thread holds the dates and gives them back with its last
    /// days. A name that a file operation in Glimt touched loses its date (`forget_date_of`),
    /// and F5 forgets them all.
    /// shortcut: a file changed under its name by another program keeps its date until F5;
    /// follow the folder's changes if that matters.
    known_dates: KnownDates,
    /// The names whose dates were forgotten while the reading thread held the dates. They are
    /// taken out of the dates it gives back, and the days are then made again.
    forgotten_while_reading: Vec<OsString>,
    /// Set when the days being made are no longer wanted: the thread stops reading.
    stop_reading_days: Arc<AtomicBool>,
    /// The files deleted while the days are being read, as sorted places in the list the days
    /// are read from. The second number is the list version the days will be for: the version
    /// read from when the reading starts, and after each delete the version after it. The days
    /// that come are moved through the deletes to the list as it is now, in one pass however
    /// many deletes there were.
    removed_while_reading: (Vec<usize>, usize),
    /// The rows, their height, and the number of columns and the cell size they were laid out
    /// for. `None` makes them again at the next frame.
    rows: Vec<(f32, pan::Row)>,
    height: f32,
    laid_for: Option<(usize, egui::Vec2)>,
    /// The calendar instead of the timeline, its months, and a day picked there for the
    /// timeline to scroll to.
    shows_calendar: bool,
    months: Vec<pan::Month>,
    scroll_to_day: Option<usize>,
}

impl PanView {
    /// The view's tabs (Timeline, Calendar), EXIF date, and how many images and days, for the
    /// list at `version`.
    fn tabs(&mut self, ui: &mut egui::Ui, version: usize) {
        ui.horizontal(|ui| {
            crate::ui::menus::choice(ui, &mut self.shows_calendar, false, tr("Timeline"));
            crate::ui::menus::choice(ui, &mut self.shows_calendar, true, tr("Calendar"));
            ui.separator();
            if crate::ui::menus::checkbox(ui, &mut self.uses_exif_dates, tr("EXIF date")).changed()
            {
                self.forget_dates();
            }
            // Days of an older list are not shown while new ones are made: they point
            // at other images now.
            match self.days.as_ref().filter(|d| d.1 == version) {
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
            if self.days.is_some() && self.days_rx.is_some() {
                ui.spinner();
            }
        });
    }

    /// Follows the list from version `old` to `version`, made without the sorted places
    /// `removed`. The days keep their images at the new places, so a delete does not make the
    /// view read every file's date again. Days of another version are left alone, and they are
    /// made again as before.
    pub(crate) fn follow(&mut self, removed: &[usize], old: usize, version: usize) {
        if self.days_rx.is_some() {
            // A delete while the days are being read lets the reading go on, because in a
            // large folder it takes minutes. The days it sends are moved through the delete.
            if self.removed_while_reading.1 != old {
                return;
            }
            self.removed_while_reading =
                (add_removed(&self.removed_while_reading.0, removed), version);
        }
        let Some((days, made)) = self.days.as_mut().filter(|d| d.1 == old) else {
            return;
        };
        remove_from_days(days, removed);
        *made = version;
        self.months = months(days);
        (self.laid_for, self.scroll_to_day) = (None, None);
    }

    /// Lays the rows out again when the number of columns or the cell size differs from the
    /// one they were laid out for. A thumbnail size or the names under thumbnails can change
    /// the cell's height while the columns stay, and rows of the old height would then put
    /// each cell over the next row.
    fn lay_out(&mut self, columns: usize, heading: f32, cell: egui::Vec2) {
        let Some((days, _)) = self.days.as_ref() else {
            return;
        };
        if self.laid_for != Some((columns, cell)) {
            (self.rows, self.height) = layout(days, columns, heading, cell.y);
            self.laid_for = Some((columns, cell));
        }
    }

    /// Takes the days read so far and moves them through the deletes made since the reading
    /// started. `known` comes with the last days, and it holds the dates the reading knows.
    fn take_days(&mut self, mut days: Vec<Day>, known: Option<KnownDates>) {
        remove_from_days(&mut days, &self.removed_while_reading.0);
        self.months = months(&days);
        self.days = Some((days, self.removed_while_reading.1));
        self.laid_for = None;
        if let Some(mut known) = known {
            let any_forgotten = !self.forgotten_while_reading.is_empty();
            for name in self.forgotten_while_reading.drain(..) {
                known.remove(&name);
            }
            self.known_dates = known;
            self.days_rx.clear();
            // The days were made with the dates that were forgotten meanwhile.
            if any_forgotten {
                self.days = None;
            }
        }
    }

    /// Forgets the date of the file named `name`, because a file operation gave the name
    /// another file, or none, or changed the file. The next days read it again. Before, a
    /// Rename Many that swapped names put each file under the date of the one that had its
    /// name.
    pub(crate) fn forget_date_of(&mut self, name: &OsStr) {
        match self.days_rx.is_some() {
            true => self.forgotten_while_reading.push(name.to_owned()),
            false => drop(self.known_dates.remove(name)),
        }
    }

    /// Makes the days again, because a file in the list was changed under its name, and its
    /// date with it. A list that changed makes them again on its own.
    pub(crate) fn make_days_again(&mut self) {
        if self.days_rx.is_none() {
            self.days = None;
        }
    }

    /// Drops the days and the dates read, and stops the reading. The dates belong to another
    /// folder, or are of the other kind (EXIF or file dates), after this.
    pub(crate) fn forget_dates(&mut self) {
        self.days = None;
        self.days_rx.clear();
        self.stop_reading_days.store(true, Ordering::Relaxed);
        self.known_dates = KnownDates::new();
        self.forgotten_while_reading.clear();
    }
}

impl Drop for PanView {
    fn drop(&mut self) {
        self.stop_reading_days.store(true, Ordering::Relaxed);
    }
}

impl crate::App {
    /// The pan view (Ctrl+J): the images of the list by day. A click shows one, a double click
    /// also closes the view.
    pub(crate) fn pan_view(&mut self, ctx: &egui::Context) {
        // The thumbnail workers make the cells drawn last frame (a frame late is soon enough);
        // this frame's are noted as they are drawn.
        let mut view = self.list.take_pan_thumbs();
        view.sort_unstable();
        view.dedup();
        self.thumbs.pan_view_shows(view);
        let Some(mut pan) = self.pan_view.take() else {
            return;
        };
        self.pan_days(ctx, &mut pan);
        let (mut open, mut show, mut close) = (true, None, false);
        egui::Window::new(tr("Pan View"))
            .open(&mut open)
            .default_size([900.0, 640.0])
            .show(ctx, |ui| {
                pan.tabs(ui, self.list.version());
                ui.separator();
                let Some((days, _)) = pan.days.as_ref().filter(|d| d.1 == self.list.version())
                else {
                    return;
                };
                let cell = self.cell();
                if pan.shows_calendar {
                    if let Some(d) = self.pan_calendar(ui, &pan.months, days) {
                        // The day in the timeline, which has all its images.
                        (pan.shows_calendar, pan.scroll_to_day) = (false, Some(d));
                    }
                    return;
                }
                let (clicked, double) = self.pan_timeline(ui, &mut pan, cell);
                if clicked.is_some() {
                    (show, close) = (clicked, double);
                }
            });
        // Its cells' thumbnails are let go here too: the file list, which also does it, may be
        // hidden (Hide File List). Kept are the same as there, so the two never undo each other.
        self.trim_thumbs(self.thumbs.list_rows());
        if let Some(i) = show {
            self.show_file(ctx, i);
        }
        if open && !close {
            self.pan_view = Some(pan);
        }
    }

    /// The Pan view's days made in the background when the list changed, also only its order,
    /// and taken as they come.
    fn pan_days(&mut self, ctx: &egui::Context, pan: &mut PanView) {
        // The days again when the list changed, also only its order; only the visible rows are
        // drawn.
        let stale = (pan.days.as_ref()).is_none_or(|d| d.1 != self.list.version());
        // Days still being made for a list that changed are stopped, so the new list's start
        // soon instead of after every old file is read. The reading gives back the dates it
        // read, and the new list's reading starts from them.
        if pan.days_rx.is_some() && pan.removed_while_reading.1 != self.list.version() {
            pan.stop_reading_days.store(true, Ordering::Relaxed);
        }
        if stale && pan.days_rx.is_none() {
            let (tx, rx) = mpsc::channel();
            let (dir, files, exif, known) = (
                self.folder.dir().to_path_buf(),
                self.list.files().to_vec(),
                pan.uses_exif_dates,
                std::mem::take(&mut pan.known_dates),
            );
            let ctx = ctx.clone();
            // The days are read from the list as it is now, which no file has left yet.
            pan.removed_while_reading = (Vec::new(), self.list.version());
            let pointer = Working::new(&self.busy);
            pan.stop_reading_days = Arc::default();
            let stop = pan.stop_reading_days.clone();
            std::thread::spawn(move || {
                timeline(&dir, &files, exif, known, &stop, |days, known| {
                    let _ = tx.send((days, known));
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
                Took::Answer((days, known)) => pan.take_days(days, known),
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
            if pan.days.as_ref().is_none_or(|d| d.1 != self.list.version()) {
                pan.days = Some((Vec::new(), self.list.version()));
                pan.months = Vec::new();
            }
            self.tell_background_stopped(tr("The Pan view's days"));
        }
    }

    /// The timeline: each day's heading and its images in rows of cells, only the rows in view
    /// drawn. The image clicked, and whether it was a double click (which closes the view).
    fn pan_timeline(
        &mut self,
        ui: &mut egui::Ui,
        pan: &mut PanView,
        cell: egui::Vec2,
    ) -> (Option<usize>, bool) {
        let (mut show, mut close) = (None, false);
        let heading = 26.0;
        let columns = ((ui.available_width() / cell.x) as usize).max(1);
        pan.lay_out(columns, heading, cell);
        let Some((days, _)) = pan.days.as_ref() else {
            return (None, false);
        };
        let mut area = egui::ScrollArea::vertical().auto_shrink(false);
        if let Some(d) = pan.scroll_to_day.take()
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
                            if i >= self.list.files().len() {
                                continue;
                            }
                            let at = top + egui::vec2(j as f32 * cell.x, y);
                            let rect = egui::Rect::from_min_size(at, cell);
                            self.list.note_pan_thumb(i);
                            let r = self.file_cell(ui, i, rect, i == self.list.shown());
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
        (show, close)
    }

    /// The calendar of the Pan view: the months with images, a week to a row, each day with its
    /// first image and how many it has. Only the months in view are drawn. The day clicked.
    pub(crate) fn pan_calendar(
        &mut self,
        ui: &mut egui::Ui,
        months: &[Month],
        days: &[Day],
    ) -> Option<usize> {
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
                        format!("{} {}", tr(MONTHS[m.month as usize - 1]), m.year),
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
                    for day in 1..=m.day_count {
                        let slot = m.first_weekday + day - 1;
                        let at = egui::vec2(
                            (slot % 7) as f32 * w,
                            y0 + title + names + (slot / 7) as f32 * h,
                        );
                        let rect =
                            egui::Rect::from_min_size(top + at, egui::vec2(w, h)).shrink(2.0);
                        picked = self.calendar_day(ui, rect, day, m, days, &font).or(picked);
                    }
                }
            });
        picked
    }

    /// One day of the calendar in `rect`: a faint square, or with images the first one, the day
    /// and how many. The day's index in `days` if it was clicked.
    fn calendar_day(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        day: u32,
        m: &Month,
        days: &[Day],
        font: &egui::FontId,
    ) -> Option<usize> {
        let weak = ui.visuals().weak_text_color();
        // A faint square for every day, so the weeks read as a grid.
        ui.painter()
            .rect_filled(rect, 3.0, ui.visuals().faint_bg_color);
        let found = (m.images.iter().find(|x| x.0 == day))
            .and_then(|&(_, d)| Some((d, *days.get(d)?.1.first()?)))
            .filter(|&(_, i)| i < self.list.files().len());
        let Some((d, first)) = found else {
            ui.painter().text(
                rect.min + egui::vec2(4.0, 2.0),
                egui::Align2::LEFT_TOP,
                day.to_string(),
                font.clone(),
                weak,
            );
            return None;
        };
        let r = ui.interact(rect, ui.scope_id().with(("day", d)), egui::Sense::click());
        let images = count(days[d].1.len(), "image");
        crate::ui::file_list::speak_item(&r, false, &format!("{day}, {images}"));
        if r.hovered() {
            ui.painter()
                .rect_filled(rect, 3.0, ui.visuals().widgets.hovered.weak_bg_fill);
        }
        self.list.note_pan_thumb(first);
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
        r.on_hover_text(images).clicked().then_some(d)
    }
}

/// The images of `files` in `dir` by day, their dates read on all cores. Only the names that
/// `known` lacks are read. `found` gets the days so far every half second, and at the end all
/// of them with the dates known then, so a large folder shows what is read while the rest is.
/// When `stop` is set, the last call comes with no days.
pub fn timeline(
    dir: &Path,
    files: &[OsString],
    uses_exif_dates: bool,
    known: KnownDates,
    stop: &AtomicBool,
    found: impl FnMut(Vec<Day>, Option<KnownDates>),
) {
    if uses_exif_dates {
        // EXIF writes "2024:05:06 07:08:09", which sorts as it is.
        // A date that is no day (the blank "    :  :  " of a camera that had none) is none.
        let date = |p: &Path| {
            let date = crate::metadata::exif::date(p, false)?;
            parse_date(&date).map(|_| (date, 0))
        };
        let day = |d: &DateKey| d.0.get(..10).unwrap_or(d.0.as_str()).replace(':', "-");
        return read_days(dir, files, known, stop, found, date, day);
    }
    let time = |p: &Path| {
        use std::os::unix::fs::MetadataExt;
        p.metadata().ok().map(|m| (String::new(), m.mtime()))
    };
    // The local date once for each quarter of an hour, where every time zone changes.
    let mut last = (i64::MIN, String::new());
    let day = move |&(_, t): &DateKey| {
        if t.div_euclid(900) != last.0 {
            let local = crate::support::format::local_time(t);
            last = (
                t.div_euclid(900),
                local.get(..10).unwrap_or(&local).to_owned(),
            );
        }
        last.1.clone()
    };
    read_days(dir, files, known, stop, found, time, day);
}

/// Reads `key` of each file that `known` lacks, a few thousand at a time on all cores. It gives
/// `found` the days of the files known so far every half second, and at the end all of them
/// with `known`, or no days when `stop` was set.
fn read_days(
    dir: &Path,
    files: &[OsString],
    mut known: KnownDates,
    stop: &AtomicBool,
    mut found: impl FnMut(Vec<Day>, Option<KnownDates>),
    key: impl Fn(&Path) -> Option<DateKey> + Sync,
    mut day: impl FnMut(&DateKey) -> String,
) {
    // The first reading reads every name, without a copy of the list.
    let missing: Cow<[OsString]> = match known.is_empty() {
        true => Cow::Borrowed(files),
        false => (files.iter())
            .filter(|name| !known.contains_key(*name))
            .cloned()
            .collect(),
    };
    let mut sent = Instant::now();
    for part in missing.chunks(4096) {
        let dates = map_files_in_parallel(dir, part, stop, &key);
        // A part cut short has no date for the files after the stop, which is not "no date".
        if stop.load(Ordering::Relaxed) {
            return found(Vec::new(), Some(known));
        }
        known.extend(part.iter().cloned().zip(dates));
        if sent.elapsed() >= std::time::Duration::from_millis(500) {
            found(by_day(files, &known, &mut day), None);
            sent = Instant::now();
        }
    }
    found(by_day(files, &known, &mut day), Some(known));
}

/// `f` of the path of every file, in parallel. Each thread reuses one path: Glimt's allocator
/// has a single arena, so allocating in every thread would make them wait for each other; for
/// the same reason they are two at most then (small_alloc_threads).
fn map_files_in_parallel<T: Send>(
    dir: &Path,
    files: &[OsString],
    stop: &AtomicBool,
    f: impl Fn(&Path) -> Option<T> + Sync,
) -> Vec<Option<T>> {
    crate::support::threads::in_parts(
        files,
        crate::support::threads::small_alloc_threads(),
        |part, _| {
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
                let found = crate::support::threads::catch_panic(|| f(&path));
                out.push(found.flatten());
                path.pop();
            }
            out
        },
    )
}

/// The files of `files` whose date is known, in time order, those without a date last, cut
/// into days by `day`. A file whose date is not read yet is left out.
fn by_day(
    files: &[OsString],
    known: &KnownDates,
    mut day: impl FnMut(&DateKey) -> String,
) -> Vec<Day> {
    let mut order: Vec<(usize, Option<&DateKey>)> = (files.iter().enumerate())
        .filter_map(|(i, name)| Some((i, known.get(name)?.as_ref())))
        .collect();
    // The sort is stable, so files of one time keep their order in the list.
    order.sort_by(|a, b| (a.1.is_none(), a.1).cmp(&(b.1.is_none(), b.1)));
    let mut days: Vec<Day> = Vec::new();
    for (i, key) in order {
        let d = key.map_or_else(|| "No date".to_owned(), &mut day);
        match days.last_mut() {
            Some((last, list)) if *last == d => list.push(i),
            _ => days.push((d, vec![i])),
        }
    }
    days
}

/// The rows of `days` with `columns` thumbnails to a row, each with its top; and the height of
/// them all. Made again only when the number of columns or the cell size changes
/// (`PanView::lay_out`).
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

/// The year, month and day of a date "2024-05-06" or EXIF's "2024:05:06 07:08:09"; `None` for
/// one that is no day, such as the 0000:00:00 of a camera with its clock never set.
fn parse_date(date: &str) -> Option<(i32, u32, u32)> {
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
        let Some((y, m, day)) = parse_date(date) else {
            continue;
        };
        match out.last_mut() {
            Some(last) if (last.year, last.month) == (y, m) => last.images.push((day, d)),
            _ => out.push(Month {
                year: y,
                month: m,
                first_weekday: weekday(y, m, 1),
                day_count: days_in_month(y, m),
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

/// Removes the sorted places `removed` from the days, moves the images after them to their new
/// places, and drops a day left empty.
fn remove_from_days(days: &mut Vec<Day>, removed: &[usize]) {
    for day in days.iter_mut() {
        day.1.retain(|i| removed.binary_search(i).is_err());
        for i in &mut day.1 {
            *i -= removed.partition_point(|r| r < i);
        }
    }
    days.retain(|d| !d.1.is_empty());
}

/// Adds the sorted places `removed`, in the list left after the files `gone` were removed, to
/// `gone`, which holds sorted places in the list before any of them was. Returns the new `gone`,
/// sorted.
fn add_removed(gone: &[usize], removed: &[usize]) -> Vec<usize> {
    let (mut all, mut j) = (gone.to_vec(), 0);
    for &r in removed {
        // The place before the removals is r plus the number of removed places at or before it.
        while j < gone.len() && gone[j] <= r + j {
            j += 1;
        }
        all.push(r + j);
    }
    all.sort_unstable();
    all
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
    use std::sync::atomic::AtomicUsize;

    /// A taller cell with the same number of columns lays the rows out again at the new pitch,
    /// as names turned on under the thumbnails do.
    #[test]
    fn the_rows_follow_the_cell_size() {
        let mut pan = PanView::default();
        pan.days = Some((vec![("2024-01-01".into(), vec![0, 1, 2, 3])], 1));
        let pitch = |pan: &PanView| pan.rows[2].0 - pan.rows[1].0;
        pan.lay_out(2, 26.0, egui::vec2(100.0, 100.0));
        assert_eq!(pitch(&pan), 100.0);
        pan.lay_out(2, 26.0, egui::vec2(100.0, 118.0));
        assert_eq!(pitch(&pan), 118.0);
        assert_eq!(pan.height, 26.0 + 2.0 * 118.0);
    }

    /// A delete keeps the days of the files left, at their new places, and a day left empty
    /// goes. Days of another version of the list are not touched.
    #[test]
    fn the_days_follow_a_delete() {
        let mut pan = PanView::default();
        let days = vec![
            ("2024-01-01".into(), vec![0, 2]),
            ("2024-01-02".into(), vec![1]),
        ];
        pan.days = Some((days, 5));
        pan.follow(&[1], 5, 6);
        assert_eq!(pan.days, Some((vec![("2024-01-01".into(), vec![0, 1])], 6)));
        pan.follow(&[0], 5, 7);
        assert_eq!(pan.days.as_ref().map(|d| d.1), Some(6));
    }

    /// A delete while the days are still being read keeps the reading going, and the days it
    /// sends afterwards come at the new places, through every delete made meanwhile.
    #[test]
    fn the_days_being_read_follow_a_delete() {
        let mut pan = PanView::default();
        let (_tx, rx) = mpsc::channel();
        (pan.days_rx, pan.removed_while_reading) = (Receiving::of(rx), (Vec::new(), 5));
        pan.days = Some((vec![("2024-01-01".into(), vec![2, 3])], 5));
        pan.follow(&[2], 5, 6);
        pan.follow(&[2], 6, 7);
        assert_eq!(pan.days, Some((vec![], 7)));
        // Places 0 and 2 of the list left are 0 and 4 of the list read, before and after the
        // two deleted first.
        pan.follow(&[0, 2], 7, 8);
        assert_eq!(pan.removed_while_reading, (vec![0, 2, 3, 4], 8));
        let read = vec![
            ("2024-01-01".into(), vec![0, 1]),
            ("2024-01-02".into(), vec![2, 3]),
            ("2024-01-03".into(), vec![4, 5]),
        ];
        pan.take_days(read, None);
        let left = vec![
            ("2024-01-01".into(), vec![0]),
            ("2024-01-03".into(), vec![1]),
        ];
        assert_eq!(pan.days, Some((left, 8)));
        // A delete in a list the reading is not for leaves it alone.
        pan.follow(&[0], 3, 9);
        assert_eq!(pan.removed_while_reading.1, 8);
    }

    /// A delete in the window keeps Pan view's days for the new list, so they are not read
    /// again for every file.
    #[test]
    fn a_delete_keeps_the_days() {
        let name = format!("glimt-test-pan-delete-{}", std::process::id());
        let dir = std::env::temp_dir().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = crate::support::test_support::app_on(&dir);
        app.list
            .set_files((0..3).map(|i| format!("{i}.png").into()).collect());
        let mut pan = PanView::default();
        let days = vec![("2024-01-01".into(), vec![0, 1, 2])];
        pan.days = Some((days, app.list.version()));
        app.pan_view = Some(pan);
        app.remove_indices(&ctx, &[1]);
        let pan = app.pan_view.as_ref().unwrap();
        let days = vec![("2024-01-01".into(), vec![0, 1])];
        assert_eq!(pan.days, Some((days, app.list.version())));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Stopped, nothing more is read: every file after gets no value.
    #[test]
    fn each_stops_when_told() {
        let files: Vec<OsString> = (0..4).map(|i| i.to_string().into()).collect();
        let stop = AtomicBool::new(false);
        let count = AtomicUsize::new(0);
        let got = map_files_in_parallel(Path::new("/"), &files, &stop, |_| {
            // The first file read sets the flag: the rest are left.
            stop.store(true, Ordering::Relaxed);
            Some(count.fetch_add(1, Ordering::Relaxed))
        });
        assert_eq!(got.len(), 4);
        assert!(
            got.iter().filter(|g| g.is_some()).count()
                <= crate::support::threads::small_alloc_threads()
        );
    }

    /// A file whose reader panics gets no value; the others keep their own.
    #[test]
    fn each_keeps_places_past_a_panic() {
        let files: Vec<OsString> = (0..9).map(|i| i.to_string().into()).collect();
        let got =
            map_files_in_parallel(Path::new("/"), &files, &AtomicBool::new(false), |p| match p
                .ends_with("4")
            {
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
        let mut sent = Vec::new();
        let stop = AtomicBool::new(false);
        timeline(&dir, &names, false, KnownDates::new(), &stop, |d, known| {
            sent.push((d, known.is_some()))
        });
        // One call with them all; an empty list gets its call too, so the view never waits.
        assert_eq!(sent.len(), 1);
        let (days, done) = sent.pop().unwrap();
        assert!(done);
        let mut empty = Vec::new();
        timeline(&dir, &[], false, KnownDates::new(), &stop, |d, known| {
            empty.push((d, known.is_some()))
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

    /// A list that changed reads only the names new to it, and the days still have every file.
    /// Before, a rename in a folder of 500,000 files read every file's date again.
    #[test]
    fn a_rename_reads_only_the_new_name() {
        let names = |n: &[&str]| n.iter().map(OsString::from).collect::<Vec<_>>();
        let reads = AtomicUsize::new(0);
        let key = |p: &Path| {
            reads.fetch_add(1, Ordering::Relaxed);
            let time = if p.ends_with("b") { 1 } else { 2 };
            Some((String::new(), time))
        };
        let day = |k: &DateKey| k.1.to_string();
        let stop = AtomicBool::new(false);
        let mut last = None;
        let files = names(&["a", "b", "c"]);
        let take = |d: Vec<Day>, k: Option<KnownDates>| last = Some((d, k));
        read_days(
            Path::new("/"),
            &files,
            KnownDates::new(),
            &stop,
            take,
            key,
            day,
        );
        let (_, known) = last.take().unwrap();
        assert_eq!(reads.swap(0, Ordering::Relaxed), 3);
        // "a" renamed to "x".
        let files = names(&["x", "b", "c"]);
        let take = |d: Vec<Day>, k: Option<KnownDates>| last = Some((d, k));
        read_days(
            Path::new("/"),
            &files,
            known.unwrap(),
            &stop,
            take,
            key,
            day,
        );
        assert_eq!(reads.load(Ordering::Relaxed), 1);
        let (days, known) = last.unwrap();
        assert_eq!(days, [("1".into(), vec![1]), ("2".into(), vec![0, 2])]);
        assert!(known.is_some_and(|k| k.contains_key(&OsString::from("x"))));
    }

    /// The Pan view hands the dates it read to the next reading when the list changes, so a
    /// rename in the window reads one file, and another folder starts without them.
    #[test]
    fn the_dates_read_follow_the_list() {
        let name = format!("glimt-test-pan-rename-{}", std::process::id());
        let dir = std::env::temp_dir().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a.png", "b.png", "c.png"] {
            std::fs::File::create(dir.join(name)).unwrap();
        }
        let (mut app, ctx) = crate::support::test_support::app_on(&dir);
        let read = |app: &mut crate::App, pan: &mut PanView| {
            for _ in 0..2000 {
                app.pan_days(&ctx, pan);
                let fresh = pan.days.as_ref().is_some_and(|d| d.1 == app.list.version());
                if fresh && pan.days_rx.is_none() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            panic!("no days");
        };
        let mut pan = PanView::default();
        read(&mut app, &mut pan);
        assert_eq!(pan.known_dates.len(), 3);
        app.rename_in_list(&ctx, vec![(0, "x.png".into())]);
        read(&mut app, &mut pan);
        // The old name is kept with the new one: the dates were carried, not read anew.
        assert_eq!(pan.known_dates.len(), 4);
        app.pan_view = Some(pan);
        app.forget_old_list();
        assert!(app.pan_view.as_ref().unwrap().known_dates.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A name that a file operation touched loses its date, also while the dates are read, and
    /// a file changed under its name gets its new day. F5 forgets every date. Before, a file
    /// renamed onto another's name, or saved turned, kept the date the name had.
    #[test]
    fn a_file_operation_forgets_the_dates_of_its_names() {
        let name = format!("glimt-test-pan-forget-{}", std::process::id());
        let dir = std::env::temp_dir().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a.png", "b.png", "c.png"] {
            std::fs::File::create(dir.join(name)).unwrap();
        }
        let (mut app, ctx) = crate::support::test_support::app_on(&dir);
        let read = |app: &mut crate::App| {
            let mut pan = app.pan_view.take().unwrap();
            for _ in 0..2000 {
                app.pan_days(&ctx, &mut pan);
                let fresh = pan.days.as_ref().is_some_and(|d| d.1 == app.list.version());
                if fresh && pan.days_rx.is_none() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            let days = pan.days.as_ref().map_or(0, |d| d.0.len());
            app.pan_view = Some(pan);
            days
        };
        fn pan(app: &crate::App) -> &PanView {
            app.pan_view.as_ref().unwrap()
        }
        app.pan_view = Some(PanView::default());
        assert_eq!(read(&mut app), 1);
        // b.png is changed in place, as a turn saved, to a day long ago.
        let long_ago = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
        let b = std::fs::File::options().write(true).open(dir.join("b.png"));
        b.unwrap().set_modified(long_ago).unwrap();
        app.files_changed(&ctx, &[dir.join("b.png")]);
        assert!(!pan(&app).known_dates.contains_key(OsStr::new("b.png")));
        assert_eq!(read(&mut app), 2);
        // a.png renamed to x.png while the dates are being read.
        let mut reading = app.pan_view.take().unwrap();
        reading.forget_dates();
        app.pan_days(&ctx, &mut reading);
        assert!(reading.days_rx.is_some());
        app.pan_view = Some(reading);
        app.follow_moved_paths(vec![(dir.join("a.png"), dir.join("x.png"))], false);
        let mut reading = app.pan_view.take().unwrap();
        while reading.days_rx.is_some() {
            app.pan_days(&ctx, &mut reading);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(!reading.known_dates.contains_key(OsStr::new("a.png")));
        assert!(reading.days.is_none());
        app.pan_view = Some(reading);
        assert_eq!(read(&mut app), 2);
        app.run(&ctx, crate::ui::actions::Action::Refresh);
        assert!(pan(&app).known_dates.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Days in either form; the blank date and the never set clock are none.
    #[test]
    fn reads_days() {
        assert_eq!(parse_date("2024:05:06 07:08:09"), Some((2024, 5, 6)));
        assert_eq!(parse_date("2024-02-29"), Some((2024, 2, 29)));
        assert_eq!(parse_date("    :  :     :  :  "), None);
        assert_eq!(parse_date("0000:00:00 00:00:00"), None);
        assert_eq!(parse_date("No date"), None);
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
        assert_eq!(
            (m[0].first_weekday, m[0].day_count, m[0].weeks()),
            (3, 29, 5)
        );
        assert_eq!((m[1].first_weekday, m[1].day_count), (1, 31));
        assert_eq!(m[1].images, [(1, 1), (15, 2)]);
        assert_eq!(weekday(2000, 1, 1), 5);
        assert_eq!(days_in_month(1900, 2), 28);
    }
}
