//! Moving through the list (next, previous, first, last, a page, random) and the selection.

use crate::Receiving;
use crate::Took;
use crate::Viewing;
use crate::drain;
use crate::thousands;
use crate::total_size;
use eframe::egui;
use std::ffi::OsString;
use std::path::Path;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Instant;
/// The row `delta` on from `row` of `n` rows, wrapping round or stopping at the ends; from
/// before the first row (`None`, the shown file hidden by a search) the step counts from there.
pub(crate) fn next_row(row: Option<usize>, delta: isize, n: usize, wrap: bool) -> Option<usize> {
    if n == 0 {
        return None;
    }
    let (row, n) = (row.map_or(-1, |r| r as isize), n as isize);
    let r = match wrap {
        true => (row + delta).rem_euclid(n),
        false => (row + delta).clamp(0, n - 1),
    };
    Some(r as usize)
}

/// Whether a slideshow stepping `delta` on from `row` of a list of `len` starts again at the top:
/// also when the shown file is not in the list (hidden by a search). Several panes step by
/// their number, so the round may end past the last row, not on the first.
pub(crate) fn round_ends(row: Option<usize>, delta: usize, len: usize) -> bool {
    row.is_none_or(|r| r + delta >= len)
}

impl crate::App {
    // ---- Moving through the list, and the selection ----

    /// Image `i`'s name as shown: relative to the folder, or in a collection the file name.
    pub(crate) fn name(&self, i: usize) -> std::borrow::Cow<'_, str> {
        let name = self.list.files[i].as_os_str();
        // Several folders: the folder and the file, "Holiday/IMG_0012.jpg".
        if !self.folders.is_empty() {
            let path = Path::new(name);
            let last = |p: &Path| {
                p.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            };
            let folder = path.parent().map(last).unwrap_or_default();
            return format!("{folder}/{last}", last = last(path)).into();
        }
        match self.collection {
            Some(_) => Path::new(name)
                .file_name()
                .unwrap_or(name)
                .to_string_lossy(),
            None => name.to_string_lossy(),
        }
    }

    /// Full path of image `i`.
    pub(crate) fn path(&self, i: usize) -> PathBuf {
        self.dir.join(&self.list.files[i])
    }

    /// Is `path` image number `i` in the list? (Without building the path.)
    pub(crate) fn is_file(&self, i: usize, path: &Path) -> bool {
        // Names are relative to the folder; in a slideshow with subfolders they have folders too.
        self.list.files.get(i).is_some_and(|name| {
            path.strip_prefix(&self.dir)
                .is_ok_and(|rest| rest.as_os_str() == name)
        })
    }

    /// Show file `idx`: the image before is left (its turn saved or dropped), the zoom reset
    /// unless kept, and the images around the new one decoded.
    pub(crate) fn go(&mut self, ctx: &egui::Context, idx: usize) {
        if self.list.files.is_empty() {
            return;
        }
        // Browsing by hand shows the image at once; the slideshow sets a fade after this.
        self.fade = None;
        // Saving the old image's rotation may leave a notice, which must outlive the move.
        let saved = self.config.save_orientation && self.viewing.orient != (0, false);
        self.leave_image(ctx);
        let notice = if saved { self.notice.take() } else { None };
        self.list.cur = idx.min(self.list.files.len() - 1);
        self.notice = notice;
        // A failure of before (a share away for a moment) is tried again now that the image is
        // shown again, not kept for the rest of the session.
        self.list.decode_errors.remove(&self.list.cur);
        // Geeqie's "keep zoom": the next image at the same zoom and place, to compare a series.
        if !self.config.keep_zoom {
            self.zoom = None;
            self.pan_offset = egui::Vec2::ZERO;
        }
        self.opened = Instant::now();
        self.scroll_to_cur = true;
        // Every image gets the full delay, also when the user moves back or forward by hand.
        if self.slideshow.is_some() {
            self.slideshow = Some(Instant::now());
        }
        self.refill();
        self.title(ctx);
    }

    /// The shown image is left: its rotation saved (with Save Orientation on) or dropped, and a
    /// TIFF page other than the first let go. Called while `dir` and `cur` still name the image,
    /// so before the folder or the list changes.
    pub(crate) fn leave_image(&mut self, ctx: &egui::Context) {
        // A TIFF shown at another page than the first: its decoded page must not stay for when
        // it is shown again from the first page.
        if self.viewing.page > 0 {
            self.list.slots.remove(&self.list.cur);
        }
        if self.config.save_orientation && self.viewing.orient != (0, false) {
            self.save_orientation(ctx);
        }
        // Everything that belonged to it, whole.
        self.viewing = Viewing::default();
    }

    /// The file `delta` rows from the shown one, in the order the list shows (the search matches
    /// while searching); clamped at the ends, or going round with `wrap`. A file the search
    /// hides counts as being before the first row.
    pub(crate) fn step(&self, delta: isize, wrap: bool) -> Option<usize> {
        self.step_from(self.list.cur, delta, wrap)
    }

    /// As `step`, from file `from`.
    pub(crate) fn step_from(&self, from: usize, delta: isize, wrap: bool) -> Option<usize> {
        let m = self.list.matches.as_deref();
        let n = m.map_or(self.list.files.len(), <[usize]>::len);
        let row = match m {
            Some(m) => self.row_of(m, from),
            None => Some(from),
        };
        let r = next_row(row, delta, n, wrap)?;
        Some(m.map_or(r, |m| m[r]))
    }

    /// The row of the shown file among the search matches `m`; `None` if the search hides it.
    pub(crate) fn cur_row(&self, m: &[usize]) -> Option<usize> {
        self.row_of(m, self.list.cur)
    }

    /// The row of `file` among the search matches `m`, kept for the shown file and one other.
    pub(crate) fn row_of(&self, m: &[usize], file: usize) -> Option<usize> {
        let mut rows = self.match_rows.get();
        if let Some((_, row)) = rows.iter().flatten().find(|r| r.0 == file) {
            return *row;
        }
        let row = m.iter().position(|&i| i == file);
        rows[(file != self.list.cur) as usize] = Some((file, row));
        self.match_rows.set(rows);
        row
    }

    /// The shown image's place in the list as shown: "[3/16]", "[–/12]" if a search hides it.
    pub(crate) fn place(&self) -> String {
        match &self.list.matches {
            Some(m) => match self.cur_row(m) {
                Some(row) => format!("[{}/{}]", thousands(row + 1), thousands(m.len())),
                None => format!("[–/{}]", thousands(m.len())),
            },
            None => format!(
                "[{}/{}]",
                thousands(self.list.cur + 1),
                thousands(self.list.files.len())
            ),
        }
    }

    /// `place` of image `idx` in another pane; the search matches are looked through once per
    /// image, not every frame.
    pub(crate) fn place_of(&mut self, idx: usize) -> String {
        let total = self
            .list
            .matches
            .as_ref()
            .map_or(self.list.files.len(), Vec::len);
        if let Some((i, t, text)) = &self.status_place
            && (*i, *t) == (idx, total)
        {
            return text.clone();
        }
        let row = match &self.list.matches {
            Some(m) => m.iter().position(|&x| x == idx),
            None => Some(idx),
        };
        let row = row.map_or("–".into(), |r| thousands(r + 1));
        let text = format!("[{row}/{}]", thousands(total));
        self.status_place = Some((idx, total, text.clone()));
        text
    }

    /// The files the list shows: the search matches, or all.
    pub(crate) fn shown_files(&self) -> Vec<usize> {
        (self.list.matches.clone()).unwrap_or_else(|| (0..self.list.files.len()).collect())
    }

    /// Select the list rows from file `a` to file `b`, in the order the list shows them (the
    /// search matches while searching).
    pub(crate) fn select_range(&mut self, a: usize, b: usize) {
        let m = self.list.matches.as_deref();
        let row = |i| m.map_or(Some(i), |m| m.iter().position(|&x| x == i));
        match (row(a), row(b), m) {
            (Some(ra), Some(rb), Some(m)) => self.list.selected.extend(&m[ra.min(rb)..=ra.max(rb)]),
            (Some(ra), Some(rb), None) => self.list.selected.extend(ra.min(rb)..=ra.max(rb)),
            _ => {
                self.list.selected.insert(b);
            }
        }
    }

    /// The selection from file `anchor` to file `from` made to end at `to`, only the rows between
    /// changed: Shift+↓ held in a selection of 500,000. False when the selection is not that range
    /// (a Ctrl-click, a click or a search since).
    pub(crate) fn move_range_end(&mut self, anchor: usize, from: usize, to: usize) -> bool {
        if self.list.range_end != Some(from) {
            return false;
        }
        let m = self.list.matches.as_deref();
        let row = |i| m.map_or(Some(i), |m| m.iter().position(|&x| x == i));
        let (Some(a), Some(f), Some(t)) = (row(anchor), row(from), row(to)) else {
            return false;
        };
        let selected = &mut self.list.selected;
        // Every row that is in one range and not the other lies between the two ends.
        let file = |r: usize| m.map_or(r, |m| m[r]);
        for r in f.min(t)..=f.max(t) {
            if (a.min(t)..=a.max(t)).contains(&r) {
                selected.insert(file(r));
            } else {
                selected.remove(&file(r));
            }
        }
        true
    }

    /// Called when the selection changes: update the title and compute the size in the background.
    pub(crate) fn selection_changed(&mut self, ctx: &egui::Context) {
        self.list.range_end = None;
        self.title(ctx);
        if self.compare {
            self.refill();
        }
        self.selected_size = None;
        self.selected_rx = Receiving::none();
        // Not at once: Shift+↓ held in a selection of 500,000 changes it every frame, and each
        // time a path for every file would be made here.
        let wait = std::time::Duration::from_millis(200);
        self.selected_size_due = (!self.list.selected.is_empty()).then(|| Instant::now() + wait);
        ctx.request_repaint_after(wait);
    }

    /// The size of the selection, in the background, once it has stayed the same a moment.
    pub(crate) fn selection_size(&mut self, ctx: &egui::Context) {
        let Some(due) = self.selected_size_due else {
            return;
        };
        // Asked again each frame: a frame drawn before it is due drops the earlier request.
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            return ctx.request_repaint_after(wait);
        }
        self.selected_size_due = None;
        if self.list.selected.is_empty() {
            return;
        }
        // All of them (Select All in a folder of 500,000): the folder's size, summed already.
        if self.list.selected.len() == self.list.files.len() && self.size_rx.is_none() {
            self.selected_size = Some(self.total_size);
            self.selected_rx = Receiving::none();
            return;
        }
        // The paths made in the thread: only the names are copied here.
        let names: Vec<OsString> = self
            .list
            .selected
            .iter()
            .map(|&i| self.list.files[i].clone())
            .collect();
        let (dir, (tx, rx), ctx) = (self.dir.clone(), mpsc::channel(), ctx.clone());
        std::thread::spawn(move || {
            let paths: Vec<PathBuf> = names.iter().map(|n| dir.join(n)).collect();
            // The receiver is gone if the selection changed again; then the answer is dropped.
            if tx.send(total_size(&paths)).is_ok() {
                ctx.request_repaint();
            }
        });
        self.selected_rx = Receiving::of(rx);
    }

    pub(crate) fn receive_sizes(&mut self) {
        while let Ok(freed) = self.freed_rx.try_recv() {
            // shortcut: a delete while the sum is still being computed can make it slightly off;
            // fine for a status bar figure.
            self.total_size = self.total_size.saturating_sub(freed);
        }
        match self.selected_rx.take() {
            Took::Answer(size) => self.selected_size = Some(size),
            // The summing thread broke: no size, instead of "calculating…" for good.
            Took::Gone => self.selected_size = Some(0),
            Took::Nothing => {}
        }
        drain(&mut self.match_rx, &mut self.list.match_size);
        drain(&mut self.size_rx, &mut self.total_size);
    }

    /// File operations act on the selected files, otherwise on the shown image. Sorted.
    pub(crate) fn targets(&self) -> Vec<usize> {
        if self.list.selected.is_empty() {
            vec![self.list.cur]
        } else {
            self.list.selected.iter().copied().collect()
        }
    }

    /// How many `targets` there are and the first, without listing them: for a dialog drawn in
    /// every frame, with 500,000 selected.
    pub(crate) fn first_target(&self) -> (usize, usize) {
        let first = self.list.selected.first().copied().unwrap_or(self.list.cur);
        (self.list.selected.len().max(1), first)
    }
}
