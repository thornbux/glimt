//! The slideshow: its timer, its order, the fade and the next image.

use crate::actions::Action;
use crate::collections;
use crate::config::FADES;
use crate::config::MAX_DELAY;
use crate::listing::shuffle;
use crate::selection::round_ends;
use eframe::egui;
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Instant;

/// What a slideshow does once its delay is up.
#[derive(Debug, PartialEq)]
pub(crate) enum Slide {
    /// The round is over and it does not repeat.
    Stop,
    /// A new round of a random order: the list shuffled again.
    Shuffle,
    /// The next image, or the next set.
    Step(usize),
    /// The next image is still being decoded: looked at again soon.
    Hold,
}

/// Where a slideshow is in its order: at `row` of the `shown` rows (`None` when a search hides
/// the image), stepping `delta` (the panes changed at once), in a list of `len` files.
#[derive(Clone, Copy)]
pub(crate) struct Place {
    pub(crate) row: Option<usize>,
    pub(crate) delta: usize,
    pub(crate) shown: usize,
    pub(crate) len: usize,
}

/// The slideshow: whether it runs, its order and the pane it changes next.
#[derive(Default)]
pub(crate) struct Slideshow {
    /// When the image shown last came up; `None`: the slideshow is off.
    pub(crate) shown_at: Option<Instant>,
    /// The file list is shuffled for a random slideshow.
    pub(crate) shuffled: bool,
    /// The next listing is shuffled as a whole instead of merged into the current order.
    pub(crate) whole_shuffle: bool,
    /// Slideshow paused (P): stays on the image until resumed.
    pub(crate) paused: bool,
    /// --slideshow: start it once the folder is read.
    pub(crate) at_start: bool,
    /// A slideshow changing one pane at a time: the pane next (0 is the first), and the image
    /// shown last.
    pub(crate) next_pane: usize,
}

impl crate::App {
    // ---- The slideshow ----

    /// Shuffle the file list (current image first) or sort it back, keeping the shown image.
    pub(crate) fn shuffle_list(&mut self, ctx: &egui::Context, on: bool) {
        if self.list.files.is_empty() {
            return;
        }
        self.leave_image(ctx);
        self.slideshow.shuffled = on;
        // Back on while a random slideshow still reads its list, which random off asked for in
        // sort order: read again, shuffled whole.
        if on && self.slideshow.whole_shuffle && self.folder.listing_rx.is_some() {
            return self.list(ctx);
        }
        if !on {
            // A collection back in its own order, which shuffling left only in its file.
            // shortcut: the file read in the UI thread; collections are small text files.
            if let Some(file) = self
                .folder
                .collection
                .as_ref()
                .filter(|_| !self.folder.sort_collection)
                && let Ok(saved) = collections::load(file)
            {
                let shown = self.list.files.get(self.list.cur).cloned();
                let files: Vec<PathBuf> =
                    (0..self.list.files.len()).map(|i| self.path(i)).collect();
                let files: Vec<OsString> = (collections::in_saved_order(&saved, files).iter())
                    .filter_map(|p| {
                        Some(
                            p.strip_prefix(&self.folder.dir)
                                .ok()?
                                .as_os_str()
                                .to_owned(),
                        )
                    })
                    .collect();
                self.replace_files(ctx, files, shown);
                // A listing on its way (a random slideshow's, a refresh's) would bring the order
                // it was asked for: read again, in the saved order now shown.
                if self.folder.listing_rx.is_some() {
                    self.list(ctx);
                }
                return;
            }
            // Back to the chosen sort order; listing keeps the shown image.
            self.list(ctx);
            return;
        }
        // A copy, not taken: the textures and the shown image's zoom are kept by the names in
        // the list as it is.
        let mut files = self.list.files.clone();
        let shown = files[self.list.cur].clone();
        files.swap(0, self.list.cur);
        shuffle(&mut files[1..]);
        // New panes from the new order: kept, a slideshow one image at a time would still stand
        // at the end of the round and shuffle again on every frame.
        self.list.panes.clear();
        self.replace_files(ctx, files, Some(shown));
    }

    /// Start the slideshow: with the subfolders when set, shuffled for random order.
    pub(crate) fn start_slideshow(&mut self, ctx: &egui::Context) {
        if self.config.slideshow_subfolders {
            // The images of the subfolders too, listed in the background; random order
            // shuffles them all together.
            self.folder.recursive = true;
            self.slideshow.shuffled = self.config.random;
            self.slideshow.whole_shuffle = self.slideshow.shuffled;
            self.list(ctx);
        } else if self.config.random != self.slideshow.shuffled {
            self.shuffle_list(ctx, self.config.random);
        }
        // The first slide comes at once: start as if the delay has already run out.
        let delay = std::time::Duration::from_secs(self.config.delay);
        self.slideshow.shown_at = Some(
            Instant::now()
                .checked_sub(delay)
                .unwrap_or_else(Instant::now),
        );
        self.title(ctx);
        ctx.request_repaint();
    }

    /// A random order ends with the slideshow: the list goes back to the chosen sort order,
    /// keeping the shown image.
    pub(crate) fn stop_slideshow(&mut self, ctx: &egui::Context) {
        self.slideshow.shown_at = None;
        self.slideshow.paused = false;
        if self.folder.recursive != self.config.show_subfolders {
            self.folder.recursive = self.config.show_subfolders;
            self.slideshow.shuffled = false;
            self.list(ctx);
        } else if self.slideshow.shuffled {
            self.shuffle_list(ctx, false);
        }
        self.title(ctx);
    }

    /// Switch image when the time is up. Waits for a preloaded image that is still
    /// decoding, so the slideshow never shows a black screen.
    pub(crate) fn slideshow_tick(&mut self, ctx: &egui::Context) {
        let Some(last) = self.slideshow.shown_at else {
            return;
        };
        // The list to show (with the subfolders, shuffled) is still being read: a step in the
        // list before it would end a round without Repeat at once on the last image.
        if self.slideshow.whole_shuffle && self.folder.listing_rx.is_some() {
            return;
        }
        // Waits while Rename or a delete asks about the shown image.
        if self.list.files.is_empty()
            || self.slideshow.paused
            || self.dialogs.prompt.is_some()
            || self.dialogs.confirm_delete.is_some()
        {
            return;
        }
        // Several panes: the next set together, or the next image into one pane at a time.
        let n = self.list.panes.len() + 1;
        let turns = n > 1 && self.one_at_a_time();
        let (from, delta) = match n {
            1 => (self.list.cur, 1),
            _ if turns => (self.list.last_slide, 1),
            _ => (self.list.cur, n),
        };
        let delay = std::time::Duration::from_secs(self.config.delay);
        let left = delay.saturating_sub(last.elapsed());
        // The slideshow owns its clock: the next frame is asked for here, whatever this one
        // does, so that no move below can leave it standing still; held for a decode, it looks
        // again soon.
        ctx.request_repaint_after(left.max(std::time::Duration::from_millis(50)));
        if !left.is_zero() {
            return;
        }
        let next = self.step_from(from, delta as isize, true);
        let m = self.list.matches.as_deref();
        let row = m.map_or(Some(from), |m| self.row_of(m, from));
        let shown = m.map_or(self.list.files.len(), <[usize]>::len);
        let pending =
            next.is_some_and(|next| self.list.decodes_pending.contains_key(&(next, false)));
        let at = Place {
            row,
            delta,
            shown,
            len: self.list.files.len(),
        };
        match next_slide(
            at,
            next,
            self.config.slideshow_repeat,
            self.slideshow.shuffled,
            pending,
        ) {
            Slide::Stop => self.stop_slideshow(ctx),
            Slide::Shuffle => self.shuffle_list(ctx, true),
            Slide::Hold => {}
            Slide::Step(next) => {
                let before = self.shown_textures();
                if turns && self.slideshow.next_pane > 0 {
                    self.list.panes[self.slideshow.next_pane - 1] = next;
                    self.refill();
                } else {
                    self.go(ctx, next);
                }
                self.decoding.fade = before.map(|b| (Instant::now(), b));
                if turns {
                    (self.list.last_slide, self.slideshow.next_pane) =
                        (next, (self.slideshow.next_pane + 1) % n);
                }
                self.slideshow.shown_at = Some(Instant::now());
            }
        }
    }

    /// The fade's length: none in power saving, where every frame counts.
    pub(crate) fn fade_ms(&self) -> u64 {
        match self.config.cpu_use {
            0 => 0,
            _ => FADES[self.config.fade].2,
        }
    }

    /// Random order changed in the menu or preferences: reshuffle now if a slideshow runs.
    pub(crate) fn set_random_order(&mut self, ctx: &egui::Context, on: bool) {
        self.config.random = on;
        self.config.save();
        if self.slideshow.shown_at.is_some() {
            self.shuffle_list(ctx, on);
            self.title(ctx);
        }
    }

    /// Ctrl+= and Ctrl+-: the slideshow's delay a second shorter or longer.
    pub(crate) fn change_delay(&mut self, ctx: &egui::Context, action: Action) {
        let step = if action == Action::Faster { -1 } else { 1 };
        self.config.delay = self
            .config
            .delay
            .saturating_add_signed(step)
            .clamp(1, MAX_DELAY);
        self.config.save();
        self.title(ctx);
    }
}

/// The slideshow's move from `at`, to `next` if there is one; repeating or not, in a `shuffled`
/// order, with `next` `pending` (still being decoded) or not.
pub(crate) fn next_slide(
    at: Place,
    next: Option<usize>,
    repeat: bool,
    shuffled: bool,
    pending: bool,
) -> Slide {
    let Place {
        row,
        delta,
        shown,
        len,
    } = at;
    let round_over = round_ends(row, delta, shown);
    let Some(next) = next else { return Slide::Hold };
    if round_over && !repeat {
        return Slide::Stop;
    }
    // A new round shuffles again instead of repeating the same order. Not when a search hides
    // the shown image, nor when it shows no more images than one step: the shuffled list starts
    // with the shown image, so the round would be over again, and shuffled on every frame.
    if shuffled && round_over && row.is_some() && len > 2 && !round_ends(Some(0), delta, shown) {
        return Slide::Shuffle;
    }
    match pending {
        true => Slide::Hold,
        false => Slide::Step(next),
    }
}

#[cfg(test)]
mod tests {
    use crate::tests::{app_on, settle};

    /// A random slideshow with subfolders, without Repeat, started on the last image: it does
    /// not end before its list is read, and stopped before that, the list comes back in sort
    /// order with no image moved.
    #[test]
    fn a_random_slideshow_waits_for_its_list() {
        for stop in [false, true] {
            let d = std::env::temp_dir()
                .join(format!("glimt-test-slides-{stop}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(d.join("sub")).unwrap();
            for i in 0..5 {
                image::RgbImage::new(2, 2)
                    .save(d.join(format!("{i}.png")))
                    .unwrap();
                image::RgbImage::new(2, 2)
                    .save(d.join("sub").join(format!("{i}.png")))
                    .unwrap();
            }
            let (mut app, ctx) = app_on(&d);
            let sorted = app.list.files.clone();
            app.go(&ctx, 4);
            app.config.slideshow_subfolders = true;
            app.config.random = true;
            app.config.slideshow_repeat = false;
            app.start_slideshow(&ctx);
            app.slideshow_tick(&ctx);
            assert!(
                app.slideshow.shown_at.is_some(),
                "ended before its list came"
            );
            if stop {
                app.stop_slideshow(&ctx);
            } else {
                // Random off and on again while the list is read: it still comes shuffled.
                app.set_random_order(&ctx, false);
                app.set_random_order(&ctx, true);
            }
            assert!(settle(&mut app, &ctx, |a| a.folder.listing_rx.is_none()));
            if stop {
                assert_eq!(app.list.files, sorted);
            } else {
                // Shuffled, its round starting at the image shown (the first slide may have
                // come since).
                assert_eq!(
                    (app.list.files.len(), app.list.files[0].to_str()),
                    (10, Some("4.png"))
                );
                // Not the sort order with the shown image put first.
                let mut sorted = app.list.files.clone();
                sorted.sort();
                let at = sorted.iter().position(|f| f == "4.png").unwrap();
                sorted.swap(0, at);
                assert_ne!(app.list.files, sorted, "shuffled");
            }
            std::fs::remove_dir_all(&d).unwrap();
        }
    }

    /// A playlist back in its own order when random is turned off while it is read again: the
    /// listing copies the list, which was still shuffled.
    #[test]
    fn a_playlist_random_off_during_a_refresh_is_in_its_order() {
        let d = std::env::temp_dir().join(format!("glimt-test-m3u-off-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let mut m3u = String::new();
        for i in 0..30 {
            let f = d.join(format!("{i:02}.png"));
            image::RgbImage::new(2, 2).save(&f).unwrap();
            m3u.push_str(&format!("{}\n", f.display()));
        }
        let file = d.join("c.m3u");
        std::fs::write(&file, m3u).unwrap();
        let (mut app, ctx) = app_on(&file);
        let order = app.list.files.clone();
        app.config.random = true;
        app.start_slideshow(&ctx);
        assert!(app.slideshow.shuffled && app.list.files != order);
        // A refresh on its way.
        app.list(&ctx);
        app.set_random_order(&ctx, false);
        assert!(settle(&mut app, &ctx, |a| a.folder.listing_rx.is_none()
            && a.status.size_rx.is_none()));
        assert_eq!(app.list.files, order);
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// A random slideshow of a collection with its subfolders plays in random order, also when
    /// the list was shuffled (R) before: the collection is read again from its file for it.
    #[test]
    fn a_random_slideshow_of_a_collection_is_shuffled() {
        for shuffled_first in [false, true] {
            let d = std::env::temp_dir().join(format!(
                "glimt-test-shuffle-{shuffled_first}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            let mut gqv = String::from("#GQview collection\n");
            for i in 0..30 {
                let f = d.join(format!("{i:02}.png"));
                image::RgbImage::new(2, 2).save(&f).unwrap();
                gqv.push_str(&format!("\"{}\"\n", f.display()));
            }
            let file = d.join("c.gqv");
            std::fs::write(&file, gqv).unwrap();
            let (mut app, ctx) = app_on(&file);
            let order = app.list.files.clone();
            app.config.slideshow_subfolders = true;
            app.config.random = true;
            if shuffled_first {
                app.shuffle_list(&ctx, true);
            }
            let shown = app.list.files[app.list.cur].clone();
            app.start_slideshow(&ctx);
            assert!(settle(&mut app, &ctx, |a| a.folder.listing_rx.is_none()));
            assert!(
                app.slideshow.shuffled && app.list.files != order,
                "R first: {shuffled_first}"
            );
            // The round starts at the image shown (the first slide may have come since): without
            // Repeat it shows every image.
            assert_eq!(app.list.files[0], shown);
            // Again, and random off before the list comes: the saved order, and its size.
            app.stop_slideshow(&ctx);
            app.start_slideshow(&ctx);
            app.set_random_order(&ctx, false);
            assert!(settle(&mut app, &ctx, |a| a.folder.listing_rx.is_none()
                && a.status.size_rx.is_none()));
            assert_eq!(app.list.files, order);
            assert!(app.status.total_size > 0, "size");
            std::fs::remove_dir_all(&d).unwrap();
        }
    }

    /// A slideshow goes on, tick after tick, whatever the layout, order and search: the image
    /// shown is another one after every step, a round of a random order shuffles once and goes
    /// on, a search that hides the image or shows one image never holds it, and without repeat
    /// it stops at the end of the round and not before.
    #[test]
    fn slideshow_advances_on_every_tick() {
        use super::{Place, Slide, next_slide};
        use crate::selection::next_row;
        // (files, rows shown by a search or all, step: the panes changed at once)
        let cases: Vec<(usize, Option<Vec<usize>>, usize)> = vec![
            (3, None, 1),
            (7, None, 1),
            (7, None, 4),
            (3, None, 4),
            (7, Some(vec![1, 3, 5]), 1),
            (7, Some(vec![1, 3, 5]), 4),
            (7, Some(vec![6]), 1),
            (7, Some(vec![]), 1),
        ];
        for (len, matches, delta) in cases {
            for (repeat, shuffled) in [(true, false), (true, true), (false, false)] {
                let m = matches.as_deref();
                let shown = m.map_or(len, <[usize]>::len);
                let (mut from, mut steps, mut shuffles, mut stopped) = (0usize, 0, 0, false);
                for _ in 0..30 {
                    let row = m.map_or(Some(from), |m| m.iter().position(|&i| i == from));
                    let next =
                        next_row(row, delta as isize, shown, true).map(|r| m.map_or(r, |m| m[r]));
                    let case = format!(
                        "{len} files, {m:?} shown, step {delta}, repeat {repeat}, shuffled {shuffled}, from {from}"
                    );
                    let at = Place {
                        row,
                        delta,
                        shown,
                        len,
                    };
                    match next_slide(at, next, repeat, shuffled, false) {
                        Slide::Hold => assert_eq!(shown, 0, "held: {case}"),
                        Slide::Stop => {
                            assert!(!repeat, "stopped: {case}");
                            stopped = true;
                            break;
                        }
                        Slide::Shuffle => {
                            shuffles += 1;
                            assert!(shuffles == 1 || steps > 0, "shuffled again at once: {case}");
                            // As shuffle_list: the shown image first in the new order.
                            (from, steps) = (0, 0);
                        }
                        Slide::Step(next) => {
                            assert!(
                                next < len && (next != from || shown == 1),
                                "no advance: {case}"
                            );
                            from = next;
                            steps += 1;
                        }
                    }
                }
                if !repeat && shown > 0 {
                    assert!(stopped, "never stopped: {len} files, {m:?}, step {delta}");
                }
            }
        }
        // Held only while the next image is still being decoded.
        let at = Place {
            row: Some(0),
            delta: 1,
            shown: 3,
            len: 3,
        };
        assert_eq!(next_slide(at, Some(1), true, false, true), Slide::Hold);
        assert_eq!(next_row(None, 1, 3, true), Some(0));
        assert_eq!(next_row(Some(2), 1, 3, false), Some(2));
    }

    #[test]
    fn slideshow_round_ends_past_the_last() {
        // Ten images in three panes: 0, 3, 6, 9, then a new round.
        assert!(!crate::selection::round_ends(Some(6), 3, 10));
        assert!(crate::selection::round_ends(Some(9), 3, 10));
        assert!(crate::selection::round_ends(Some(8), 3, 10));
        assert!(crate::selection::round_ends(Some(0), 1, 1));
        assert!(crate::selection::round_ends(None, 1, 5));
    }
}
