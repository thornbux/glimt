//! Moving through the list (next, previous, first, last, a page, random) and the selection.

use crate::model::folder::total_size;
use crate::support::format::{count, thousands};
use crate::support::lang::tr;
use crate::tf;
use crate::ui::actions::Action;
use crate::ui::file_list::SizeDue;
use crate::viewer::view::Viewing;
use eframe::egui;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

impl crate::App {
    // ---- Moving through the list, and the selection ----

    /// Image `i`'s name as shown: relative to the folder, or in a collection the file name.
    /// Returns an empty text when there is no image `i`. Every caller only shows the name, and a
    /// frame drawn while the list is empty (an empty folder, the last file just deleted) asks
    /// for the shown image's name too.
    pub(crate) fn name(&self, i: usize) -> std::borrow::Cow<'_, str> {
        (self.list.files().get(i)).map_or("".into(), |name| self.name_shown(name))
    }

    /// Returns how the list shows `name`, an entry of the list's files.
    pub(crate) fn name_shown<'a>(&self, name: &'a std::ffi::OsStr) -> std::borrow::Cow<'a, str> {
        // Several folders: the folder and the file, "Holiday/IMG_0012.jpg".
        if !self.folder.folders().is_empty() {
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
        match self.folder.collection() {
            Some(_) => Path::new(name)
                .file_name()
                .unwrap_or(name)
                .to_string_lossy(),
            None => name.to_string_lossy(),
        }
    }

    /// Full path of image `i`.
    pub(crate) fn path(&self, i: usize) -> PathBuf {
        self.folder.dir().join(&self.list.files()[i])
    }

    /// Is `path` image number `i` in the list? (Without building the path.)
    pub(crate) fn is_file(&self, i: usize, path: &Path) -> bool {
        // Names are relative to the folder; in a slideshow with subfolders they have folders too.
        self.list.files().get(i).is_some_and(|name| {
            path.strip_prefix(self.folder.dir())
                .is_ok_and(|rest| rest.as_os_str() == name)
        })
    }

    /// Shows file `idx`. When it is the image shown already, its zoom, turn and page stay. This
    /// holds for every caller: a click, a pane, a slide or a remote command. Only a decode that
    /// failed (a share away for a moment) is tried again, as when it is shown anew.
    pub(crate) fn show_file(&mut self, ctx: &egui::Context, idx: usize) {
        if self.list.files().is_empty() {
            return;
        }
        let idx = idx.min(self.list.files().len() - 1);
        if idx != self.list.shown() {
            self.show_file_anew(ctx, idx);
        } else if self.list.decode_errors().contains_key(&idx) {
            self.list.forget_decode_error(idx);
            self.refill_preload();
        }
    }

    /// Shows file `idx`, also when it is the image shown. The image before is left, with its
    /// turn saved or dropped, the zoom is reset unless kept, and the images around the new one
    /// are decoded. A new list calls it after it has set the shown file itself.
    pub(crate) fn show_file_anew(&mut self, ctx: &egui::Context, idx: usize) {
        if self.list.files().is_empty() {
            return;
        }
        // Browsing by hand shows the image at once; the slideshow sets a fade after this.
        self.decoding.stop_fade();
        // Saving the old image's rotation may leave a notice, which must outlive the move.
        let saved = self.config.save_orientation && self.viewing.is_turned();
        self.leave_image(ctx);
        let notice = if saved { self.notice.set_aside() } else { None };
        self.list.set_shown(idx.min(self.list.files().len() - 1));
        self.notice.put_back(notice);
        // A failure of before (a share away for a moment) is tried again now that the image is
        // shown again, not kept for the rest of the session.
        self.list.forget_decode_error(self.list.shown());
        // Geeqie's "keep zoom": the next image at the same zoom and place, to compare a series.
        if !self.config.keep_zoom {
            self.zoom.fit();
        }
        self.decoding.image_opened();
        self.panel.scroll_to_shown();
        // Every image gets the full delay, also when the user moves back or forward by hand.
        self.slideshow.restart_delay();
        self.refill_preload();
        self.update_window_title(ctx);
    }

    /// The shown image is left: its rotation saved (with Save Orientation on) or dropped, and a
    /// TIFF page other than the first let go. Called while `dir` and `shown` still name the image,
    /// so before the folder or the list changes.
    pub(crate) fn leave_image(&mut self, ctx: &egui::Context) {
        // A TIFF shown at another page than the first: its decoded page must not stay for when
        // it is shown again from the first page.
        if self.viewing.page() > 0 {
            self.list.forget_loaded_image(self.list.shown());
        }
        if self.config.save_orientation && self.viewing.is_turned() {
            self.save_orientation(ctx);
        }
        // Everything that belonged to it, whole.
        self.viewing = Viewing::default();
    }

    /// The file `delta` rows from the shown one, in the order the list shows (the search matches
    /// while searching); clamped at the ends, or going round with `wrap`. A file the search
    /// hides counts as being before the first row.
    pub(crate) fn step(&self, delta: isize, wrap: bool) -> Option<usize> {
        self.step_from(self.list.shown(), delta, wrap)
    }

    /// As `step`, from file `from`.
    pub(crate) fn step_from(&self, from: usize, delta: isize, wrap: bool) -> Option<usize> {
        let m = self.list.matches();
        let n = m.map_or(self.list.files().len(), <[usize]>::len);
        let row = match m {
            Some(m) => self.row_of(m, from),
            None => Some(from),
        };
        let r = next_row(row, delta, n, wrap)?;
        Some(m.map_or(r, |m| m[r]))
    }

    /// The row of the shown file among the search matches `m`. Returns `None` if the search hides
    /// it.
    pub(crate) fn shown_row(&self, m: &[usize]) -> Option<usize> {
        self.row_of(m, self.list.shown())
    }

    /// The row of `file` among the search matches `m`, kept for the shown file and one other.
    pub(crate) fn row_of(&self, m: &[usize], file: usize) -> Option<usize> {
        (self.search).row_of(file, self.list.shown(), || {
            m.iter().position(|&i| i == file)
        })
    }

    /// The shown image's position in the list as shown: "[3/16]", or "[–/12]" when a search
    /// hides it.
    pub(crate) fn position_text(&self) -> String {
        match self.list.matches() {
            Some(m) => match self.shown_row(m) {
                Some(row) => format!("[{}/{}]", thousands(row + 1), thousands(m.len())),
                None => format!("[–/{}]", thousands(m.len())),
            },
            None => format!(
                "[{}/{}]",
                thousands(self.list.shown() + 1),
                thousands(self.list.files().len())
            ),
        }
    }

    /// The `position_text` of image `idx` in another pane. The search matches are looked through
    /// once for each image, not in every frame.
    pub(crate) fn position_text_of(&mut self, idx: usize) -> String {
        let total = self
            .list
            .matches()
            .map_or(self.list.files().len(), <[usize]>::len);
        let matches = self.list.matches();
        self.status_bar.position_text(idx, total, || {
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
        (self.list.matches().map(<[usize]>::to_vec))
            .unwrap_or_else(|| (0..self.list.files().len()).collect())
    }

    /// Select the list rows from file `a` to file `b`, in the order the list shows them (the
    /// search matches while searching).
    pub(crate) fn select_range(&mut self, a: usize, b: usize) {
        let m = self.list.matches();
        let row = |i| m.map_or(Some(i), |m| m.iter().position(|&x| x == i));
        match (row(a), row(b), m) {
            (Some(ra), Some(rb), Some(m)) => {
                let rows = m[ra.min(rb)..=ra.max(rb)].to_vec();
                self.list.add_to_selection(rows);
            }
            (Some(ra), Some(rb), None) => self.list.add_to_selection(ra.min(rb)..=ra.max(rb)),
            _ => self.list.add_to_selection([b]),
        }
    }

    /// The selection from file `anchor` to file `from` made to end at `to`, only the rows between
    /// changed: Shift+↓ held in a selection of 500,000. False when the selection is not that range
    /// (a Ctrl-click, a click or a search since).
    pub(crate) fn move_range_end(&mut self, anchor: usize, from: usize, to: usize) -> bool {
        if self.list.range_end() != Some(from) {
            return false;
        }
        let m = self.list.matches();
        let row = |i| m.map_or(Some(i), |m| m.iter().position(|&x| x == i));
        let (Some(a), Some(f), Some(t)) = (row(anchor), row(from), row(to)) else {
            return false;
        };
        // Every row that is in one range and not the other lies between the two ends.
        let file = |r: usize| m.map_or(r, |m| m[r]);
        let rows: Vec<(usize, bool)> = (f.min(t)..=f.max(t))
            .map(|r| (file(r), (a.min(t)..=a.max(t)).contains(&r)))
            .collect();
        for (i, on) in rows {
            self.list.set_selected(i, on);
        }
        true
    }

    /// Selects the files at `places` and nothing else, and tells what follows the selection
    /// (`selection_changed`). This is the one way a whole selection is set, so no writer can
    /// leave out the title, the range's end or the size in the status bar.
    pub(crate) fn set_selection(
        &mut self,
        ctx: &egui::Context,
        places: std::collections::BTreeSet<usize>,
    ) {
        self.list.select(places);
        self.selection_changed(ctx);
    }

    /// Selects the files at `places` for an action to act on, as Find Duplicates, Check
    /// Images, a dialog's answer and `--remote select` do. Returns false, with a notice and the
    /// selection left as it was, when the search hides any of them. The search would keep those
    /// apart, and the action would then act on fewer files than meant, or on the shown image.
    pub(crate) fn select_for_action(
        &mut self,
        ctx: &egui::Context,
        places: std::collections::BTreeSet<usize>,
    ) -> bool {
        if !places.iter().all(|&i| self.list.shows_file(i)) {
            self.notice
                .give(tr("The search hides files this would act on: clear it first").into());
            return false;
        }
        self.set_selection(ctx, places);
        true
    }

    /// Called when the selection changes: update the title and compute the size in the background.
    pub(crate) fn selection_changed(&mut self, ctx: &egui::Context) {
        self.list.set_range_end(None);
        self.update_window_title(ctx);
        if self.is_comparing {
            self.refill_preload();
        }
        let wait = self
            .status_bar
            .selection_changed(!self.list.selected().is_empty());
        ctx.request_repaint_after(wait);
    }

    /// The size of the selection, in the background, once it has stayed the same a moment.
    pub(crate) fn sum_selection_size(&mut self, ctx: &egui::Context) {
        match self.status_bar.size_due() {
            SizeDue::No => return,
            // Asked again each frame: a frame drawn before it is due drops the earlier request.
            SizeDue::Later(wait) => return ctx.request_repaint_after(wait),
            SizeDue::Now => {}
        }
        if self.list.selected().is_empty() {
            return;
        }
        // All of them (Select All in a folder of 500,000): the folder's size, summed already.
        if self.list.selected().len() == self.list.files().len()
            && let Some(total) = self.status_bar.folder_total()
        {
            return self.status_bar.set_selection_size(total);
        }
        // The paths made in the thread: only the names are copied here.
        let names: Vec<OsString> = self
            .list
            .selected()
            .iter()
            .map(|&i| self.list.files()[i].clone())
            .collect();
        let (dir, (tx, rx), ctx) = (
            self.folder.dir().to_path_buf(),
            mpsc::channel(),
            ctx.clone(),
        );
        std::thread::spawn(move || {
            let paths: Vec<PathBuf> = names.iter().map(|n| dir.join(n)).collect();
            // The receiver is gone if the selection changed again; then the answer is dropped.
            if tx.send(total_size(&paths)).is_ok() {
                ctx.request_repaint();
            }
        });
        self.status_bar.sum_selection(rx);
    }

    /// The files that an action on the selection acts on: the selected files, otherwise the
    /// shown image, sorted. This is the one place that decides it. Returns `None`, with a
    /// notice, when every selected file is one the search hides, because the shown image
    /// would then stand in for files the user picked. When the search hides only some of
    /// them, the action acts on the others, and the status bar counts the ones it leaves out.
    pub(crate) fn targets(&mut self) -> Option<Vec<usize>> {
        let targets = self.targets_without_notice();
        if targets.is_none() {
            self.notice
                .give(tr("The search hides files this would act on: clear it first").into());
        }
        targets
    }

    /// Says how many selected files the search hides, which an action on the selection
    /// leaves out. Returns `None` when the search hides none of them.
    pub(crate) fn hidden_selection_notice(&self) -> Option<String> {
        let files = match self.list.hidden_selected().len() {
            0 => return None,
            n => count(n, "file"),
        };
        Some(tf!(
            "Not included, as the search hides them: {files}",
            files
        ))
    }

    /// The files of `targets()`, without the notice, for a query that changes nothing.
    pub(crate) fn targets_without_notice(&self) -> Option<Vec<usize>> {
        if self.list.only_hidden_selected() {
            return None;
        }
        Some(match self.list.selected().is_empty() {
            true => vec![self.list.shown()],
            false => self.list.selected().iter().copied().collect(),
        })
    }

    /// The first of `targets()`, without copying a selection that may hold every file.
    pub(crate) fn first_target(&self) -> usize {
        self.list
            .selected()
            .first()
            .copied()
            .unwrap_or(self.list.shown())
    }

    /// First, Previous, Next and Last, along the list as shown (the search matches while
    /// searching); with Shift the selection runs from where it started to the new image, as in a
    /// file manager, and moving without Shift clears it.
    pub(crate) fn move_along(&mut self, ctx: &egui::Context, action: Action) {
        // Along the list as shown: the search matches while searching.
        let row = self.panel.row_step();
        let delta = match action {
            Action::First => -isize::MAX / 2,
            Action::Previous => -row,
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
            let anchor = self.list.anchor_or_shown();
            if !self.move_range_end(anchor, self.list.shown(), to) {
                self.list.clear_selection();
                self.select_range(anchor, to);
            }
            self.selection_changed(ctx);
        } else {
            self.list.set_anchor(None);
            if !self.list.selected().is_empty() {
                self.list.clear_selection();
                self.selection_changed(ctx);
            }
        }
        self.show_file(ctx, to);
        if shift {
            self.list.set_range_end(Some(to));
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

    /// Next on the last image, a click on the shown image's row, a pane step that clamps to it
    /// and a shuffle keep the shown image as it is. Its turn is not dropped, or written into the
    /// file, as when another image is shown.
    #[test]
    fn the_shown_image_stays_as_it_is_when_it_stays() {
        let dir = std::env::temp_dir().join(format!("glimt-test-stays-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = app_on(&dir);
        app.list.set_files(vec!["a.png".into(), "b.png".into()]);
        app.list.set_shown(1);
        app.viewing.set_turn((1, false), dir.join("b.png"));
        app.move_along(&ctx, crate::ui::actions::Action::Next);
        assert_eq!((app.list.shown(), app.viewing.turn()), (1, (1, false)));
        app.show_file(&ctx, 1);
        assert_eq!(app.viewing.turn(), (1, false));
        app.list.set_shown(0);
        app.viewing.set_turn((1, false), dir.join("a.png"));
        app.go_to_pane(&ctx, crate::ui::actions::Action::PaneLeft);
        assert_eq!((app.list.shown(), app.viewing.turn()), (0, (1, false)));
        app.shuffle_list(&ctx, true);
        assert_eq!(app.viewing.turn(), (1, false));
        // A decode of the shown image that failed is tried again when it is gone to again.
        app.list.decode_failed(0, "the share is away".into());
        app.show_file(&ctx, 0);
        assert!(!app.list.decode_errors().contains_key(&0));
        assert_eq!(app.viewing.turn(), (1, false));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// When the search hides every selected file, an action on the selection acts on none of
    /// them, and not on the shown image in their place: it is refused with a notice. A
    /// right-click on the shown image selects it, so its menu acts on it.
    #[test]
    fn a_selection_the_search_hides_is_not_the_shown_image() {
        let dir = std::env::temp_dir().join(format!("glimt-test-hidden-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = app_on(&dir);
        let names = ["apple.jpg", "banana.jpg", "cherry.jpg"];
        app.list.set_files(names.iter().map(Into::into).collect());
        app.list.select([1].into());
        app.search.set_query("cherry");
        app.update_search(&ctx);
        assert!(app.list.only_hidden_selected());
        assert_eq!(app.targets(), None);
        assert!(app.notice.set_aside().is_some());
        // Neither the delete's question nor a query names the shown image.
        app.run(&ctx, crate::ui::actions::Action::Delete);
        assert!(!app.asking_about_selection());
        assert!(app.notice.set_aside().is_some());
        assert!(app.remote_query("selection").is_empty());
        let shown = app.list.shown();
        assert!(app.list.select_on_right_click(shown));
        assert_eq!(app.targets(), Some(vec![shown]));
        assert!(app.list.hidden_selected().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// When the search hides some of the selected files, an action acts on the others, and the
    /// delete's question says how many it leaves out: the count when it asked, whatever changes
    /// while it is open. Without hidden files it says nothing of them.
    #[test]
    fn a_partly_hidden_selection_says_what_it_leaves_out() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-part-hidden-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = app_on(&dir);
        let names = ["apple.jpg", "banana.jpg", "cherry.jpg", "cherry2.jpg"];
        app.list.set_files(names.iter().map(Into::into).collect());
        app.list.select([0, 1, 2].into());
        app.search.set_query("cherry");
        app.update_search(&ctx);
        assert_eq!(app.targets(), Some(vec![2]));
        let left_out = app.hidden_selection_notice().unwrap();
        assert!(left_out.contains("2 files"), "{left_out}");
        app.config.confirm_trash = true;
        app.request_delete(&ctx, true);
        app.list.select([0, 2].into());
        assert_eq!(app.dialogs.asked_left_out(), Some(left_out.as_str()));
        assert_eq!(app.asked_targets().map(|(found, _)| found), Some(vec![2]));
        assert_eq!(app.dialogs.asked_left_out(), None);
        app.list.select([2, 3].into());
        assert_eq!(app.targets(), Some(vec![2, 3]));
        assert_eq!(app.hidden_selection_notice(), None);
        app.request_delete(&ctx, true);
        assert_eq!(app.dialogs.asked_left_out(), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Moving the end of a Shift range changes the selection as selecting the range anew does,
    /// in the list and among search matches, also across the anchor.
    #[test]
    fn a_range_end_moved_is_the_range_anew() {
        let dir = std::env::temp_dir().join(format!("glimt-test-range-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = app_on(&dir);
        app.list
            .set_files((0..10).map(|i| format!("{i}.png").into()).collect());
        for matches in [None, Some(vec![1, 3, 4, 6, 8, 9])] {
            app.list.set_matches(matches.clone());
            let shown = app.shown_files();
            let (anchor, mut end) = (shown[2], shown[2]);
            app.list.select([anchor].into());
            app.list.set_range_end(Some(end));
            for &to in [shown[4], shown[5], shown[0], shown[3], shown[5]].iter() {
                assert!(app.move_range_end(anchor, end, to));
                let moved = app.list.selected().clone();
                app.list.clear_selection();
                app.select_range(anchor, to);
                assert_eq!(&moved, app.list.selected(), "{matches:?} to {to}");
                end = to;
                app.list.set_range_end(Some(end));
            }
        }
        // Ctrl-clicks since (on 9, then 2, the image shown 0): a selection of three that looks
        // like the range 2..=0 but holds 9; it is made anew, not moved.
        app.list.set_matches(None);
        app.list.select([0, 2, 9].into());
        app.selection_changed(&ctx);
        assert!(!app.move_range_end(2, 0, 1));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
