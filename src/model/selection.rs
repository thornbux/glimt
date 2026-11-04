//! Moving through the list (next, previous, first, last, a page, random) and the selection.

use crate::model::folder::total_size;
use crate::support::format::thousands;
use crate::ui::actions::Action;
use crate::ui::file_list::SizeDue;
use crate::viewer::view::Viewing;
use eframe::egui;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Instant;

impl crate::App {
    // ---- Moving through the list, and the selection ----

    /// Image `i`'s name as shown: relative to the folder, or in a collection the file name.
    pub(crate) fn name(&self, i: usize) -> std::borrow::Cow<'_, str> {
        let name = self.list.files[i].as_os_str();
        // Several folders: the folder and the file, "Holiday/IMG_0012.jpg".
        if !self.folder.folders.is_empty() {
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
        match self.folder.collection {
            Some(_) => Path::new(name)
                .file_name()
                .unwrap_or(name)
                .to_string_lossy(),
            None => name.to_string_lossy(),
        }
    }

    /// Full path of image `i`.
    pub(crate) fn path(&self, i: usize) -> PathBuf {
        self.folder.dir.join(&self.list.files[i])
    }

    /// Is `path` image number `i` in the list? (Without building the path.)
    pub(crate) fn is_file(&self, i: usize, path: &Path) -> bool {
        // Names are relative to the folder; in a slideshow with subfolders they have folders too.
        self.list.files.get(i).is_some_and(|name| {
            path.strip_prefix(&self.folder.dir)
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
        self.decoding.stop_fade();
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
            self.zoom.fit();
        }
        self.decoding.image_opened();
        self.panel.scroll_to_current();
        // Every image gets the full delay, also when the user moves back or forward by hand.
        if self.slideshow.shown_at.is_some() {
            self.slideshow.shown_at = Some(Instant::now());
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

    /// The row of the shown file among the search matches `m`. Returns `None` if the search hides
    /// it.
    pub(crate) fn cur_row(&self, m: &[usize]) -> Option<usize> {
        self.row_of(m, self.list.cur)
    }

    /// The row of `file` among the search matches `m`, kept for the shown file and one other.
    pub(crate) fn row_of(&self, m: &[usize], file: usize) -> Option<usize> {
        (self.search).row_of(file, self.list.cur, || m.iter().position(|&i| i == file))
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
        let matches = &self.list.matches;
        self.status.place(idx, total, || {
            let row = match matches {
                Some(m) => m.iter().position(|&x| x == idx),
                None => Some(idx),
            };
            let row = row.map_or("–".into(), |r| thousands(r + 1));
            format!("[{row}/{}]", thousands(total))
        })
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
        let wait = self
            .status
            .selection_changed(!self.list.selected.is_empty());
        ctx.request_repaint_after(wait);
    }

    /// The size of the selection, in the background, once it has stayed the same a moment.
    pub(crate) fn selection_size(&mut self, ctx: &egui::Context) {
        match self.status.size_due() {
            SizeDue::No => return,
            // Asked again each frame: a frame drawn before it is due drops the earlier request.
            SizeDue::Later(wait) => return ctx.request_repaint_after(wait),
            SizeDue::Now => {}
        }
        if self.list.selected.is_empty() {
            return;
        }
        // All of them (Select All in a folder of 500,000): the folder's size, summed already.
        if self.list.selected.len() == self.list.files.len()
            && let Some(total) = self.status.folder_total()
        {
            return self.status.selection_is(total);
        }
        // The paths made in the thread: only the names are copied here.
        let names: Vec<OsString> = self
            .list
            .selected
            .iter()
            .map(|&i| self.list.files[i].clone())
            .collect();
        let (dir, (tx, rx), ctx) = (self.folder.dir.clone(), mpsc::channel(), ctx.clone());
        std::thread::spawn(move || {
            let paths: Vec<PathBuf> = names.iter().map(|n| dir.join(n)).collect();
            // The receiver is gone if the selection changed again; then the answer is dropped.
            if tx.send(total_size(&paths)).is_ok() {
                ctx.request_repaint();
            }
        });
        self.status.sum_selection(rx);
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

    /// First, Previous, Next and Last, along the list as shown (the search matches while
    /// searching); with Shift the selection runs from where it started to the new image, as in a
    /// file manager, and moving without Shift clears it.
    pub(crate) fn move_along(&mut self, ctx: &egui::Context, action: Action) {
        // Along the list as shown: the search matches while searching.
        let row = self.panel.row_step();
        let delta = match action {
            Action::First => -isize::MAX / 2,
            Action::Prev => -row,
            Action::Next => row,
            _ => isize::MAX / 2,
        };
        let Some(to) = self.step(delta, false) else {
            return;
        };
        // With Shift the selection runs from where it started to the new image, as in
        // a file manager; moving without Shift clears it.
        let shift = ctx.input(|i| i.modifiers.shift);
        if shift {
            let anchor = *self.list.anchor.get_or_insert(self.list.cur);
            if !self.move_range_end(anchor, self.list.cur, to) {
                self.list.selected.clear();
                self.select_range(anchor, to);
            }
            self.selection_changed(ctx);
        } else {
            self.list.anchor = None;
            if !self.list.selected.is_empty() {
                self.list.selected.clear();
                self.selection_changed(ctx);
            }
        }
        self.go(ctx, to);
        if shift {
            self.list.range_end = Some(to);
        }
    }
}

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

#[cfg(test)]
mod tests {
    use crate::support::test_support::app_on;

    /// Moving the end of a Shift range changes the selection as selecting the range anew does,
    /// in the list and among search matches, also across the anchor.
    #[test]
    fn a_range_end_moved_is_the_range_anew() {
        let dir = std::env::temp_dir().join(format!("glimt-test-range-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = app_on(&dir);
        app.list.files = (0..10).map(|i| format!("{i}.png").into()).collect();
        for matches in [None, Some(vec![1, 3, 4, 6, 8, 9])] {
            app.list.matches = matches.clone();
            let shown = app.shown_files();
            let (anchor, mut end) = (shown[2], shown[2]);
            app.list.selected = [anchor].into();
            app.list.range_end = Some(end);
            for &to in [shown[4], shown[5], shown[0], shown[3], shown[5]].iter() {
                assert!(app.move_range_end(anchor, end, to));
                let moved = std::mem::take(&mut app.list.selected);
                app.select_range(anchor, to);
                assert_eq!(moved, app.list.selected, "{matches:?} to {to}");
                end = to;
                app.list.range_end = Some(end);
            }
        }
        // Ctrl-clicks since (on 9, then 2, the image shown 0): a selection of three that looks
        // like the range 2..=0 but holds 9; it is made anew, not moved.
        app.list.matches = None;
        app.list.selected = [0, 2, 9].into();
        app.selection_changed(&ctx);
        assert!(!app.move_range_end(2, 0, 1));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
