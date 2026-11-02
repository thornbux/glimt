//! The slideshow: its timer, its order, the fade and the next image.

use crate::collections;
use crate::config::FADES;
use crate::files::shuffle;
use crate::selection::round_ends;
use eframe::egui;
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Instant;

impl crate::App {
    // ---- The slideshow ----

    /// Shuffle the file list (current image first) or sort it back, keeping the shown image.
    pub(crate) fn shuffle_list(&mut self, ctx: &egui::Context, on: bool) {
        if self.list.files.is_empty() {
            return;
        }
        self.leave_image(ctx);
        self.shuffled = on;
        // Back on while a random slideshow still reads its list, which random off asked for in
        // sort order: read again, shuffled whole.
        if on && self.whole_shuffle && self.listing_rx.is_some() {
            return self.list(ctx);
        }
        if !on {
            // A collection back in its own order, which shuffling left only in its file.
            // shortcut: the file read in the UI thread; collections are small text files.
            if let Some(file) = self.collection.as_ref().filter(|_| !self.sort_collection)
                && let Ok(saved) = collections::load(file)
            {
                let shown = self.list.files.get(self.list.cur).cloned();
                let files: Vec<PathBuf> =
                    (0..self.list.files.len()).map(|i| self.path(i)).collect();
                let files: Vec<OsString> = (collections::in_saved_order(&saved, files).iter())
                    .filter_map(|p| Some(p.strip_prefix(&self.dir).ok()?.as_os_str().to_owned()))
                    .collect();
                self.replace_files(ctx, files, shown);
                // A listing on its way (a random slideshow's, a refresh's) would bring the order
                // it was asked for: read again, in the saved order now shown.
                if self.listing_rx.is_some() {
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
            self.recursive = true;
            self.shuffled = self.config.random;
            self.whole_shuffle = self.shuffled;
            self.list(ctx);
        } else if self.config.random != self.shuffled {
            self.shuffle_list(ctx, self.config.random);
        }
        // The first slide comes at once: start as if the delay has already run out.
        let delay = std::time::Duration::from_secs(self.config.delay);
        self.slideshow = Some(
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
        self.slideshow = None;
        self.paused = false;
        if self.recursive != self.config.show_subfolders {
            self.recursive = self.config.show_subfolders;
            self.shuffled = false;
            self.list(ctx);
        } else if self.shuffled {
            self.shuffle_list(ctx, false);
        }
        self.title(ctx);
    }

    /// Switch image when the time is up. Waits for a preloaded image that is still
    /// decoding, so the slideshow never shows a black screen.
    pub(crate) fn slideshow_tick(&mut self, ctx: &egui::Context) {
        let Some(last) = self.slideshow else { return };
        // The list to show (with the subfolders, shuffled) is still being read: a step in the
        // list before it would end a round without Repeat at once on the last image.
        if self.whole_shuffle && self.listing_rx.is_some() {
            return;
        }
        // Waits while Rename or a delete asks about the shown image.
        if self.list.files.is_empty()
            || self.paused
            || self.prompt.is_some()
            || self.confirm_delete.is_some()
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
            self.shuffled,
            pending,
        ) {
            Slide::Stop => self.stop_slideshow(ctx),
            Slide::Shuffle => self.shuffle_list(ctx, true),
            Slide::Hold => {}
            Slide::Step(next) => {
                let before = self.shown_textures();
                if turns && self.next_pane > 0 {
                    self.list.panes[self.next_pane - 1] = next;
                    self.refill();
                } else {
                    self.go(ctx, next);
                }
                self.fade = before.map(|b| (Instant::now(), b));
                if turns {
                    (self.list.last_slide, self.next_pane) = (next, (self.next_pane + 1) % n);
                }
                self.slideshow = Some(Instant::now());
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
        if self.slideshow.is_some() {
            self.shuffle_list(ctx, on);
            self.title(ctx);
        }
    }
}

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
