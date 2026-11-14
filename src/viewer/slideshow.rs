//! The slideshow: its timer, its order, the fade and the next image.

use crate::files::config::FADES;
use crate::files::config::MAX_SLIDESHOW_DELAY;
use crate::model::collections;
use crate::model::listing::shuffle;
use crate::model::selection::round_ends;
use crate::support::lang::tr;
use crate::support::threads::{Receiving, Took, Working, answer_channel};
use crate::ui::actions::Action;
use eframe::egui;
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Instant;

/// What a slideshow does once its delay is up.
#[derive(Debug, PartialEq)]
pub(crate) enum SlideStep {
    /// The round is over and it does not repeat.
    Stop,
    /// A new round of a random order: the list shuffled again.
    Shuffle,
    /// The next image, or the next set.
    Step(usize),
    /// The next image is still being decoded: looked at again soon.
    Wait,
}

/// Where a slideshow is in its order: at `row` of the `shown_rows` rows (`None` when a search
/// hides the image), stepping `step` rows (the panes changed at once), in a list of
/// `file_count` files.
#[derive(Clone, Copy)]
pub(crate) struct SlideshowPosition {
    pub(crate) row: Option<usize>,
    pub(crate) step: usize,
    pub(crate) shown_rows: usize,
    pub(crate) file_count: usize,
}

/// The slideshow: whether it runs, its order and the pane it changes next.
#[derive(Default)]
pub(crate) struct Slideshow {
    /// When the image shown last came up; `None`: the slideshow is off.
    shown_at: Option<Instant>,
    /// The file list is shuffled for a random slideshow.
    shuffled: bool,
    /// The next listing is shuffled as a whole instead of merged into the current order.
    shuffle_next_listing_whole: bool,
    /// Slideshow paused (P): stays on the image until resumed.
    paused: bool,
    /// --slideshow: start it once the folder is read.
    start_after_listing: bool,
    /// A slideshow changing one pane at a time: the pane next (0 is the first), and the image
    /// shown last.
    next_pane: usize,
    /// The open collection's entries in their saved order, by their real paths, read in the
    /// background when random order is turned off.
    saved_order: Receiving<(Option<Vec<PathBuf>>, Working)>,
}

impl Slideshow {
    /// The slideshow at start. With `start_after_listing` (--slideshow), it begins once the
    /// folder is read.
    pub(crate) fn new(start_after_listing: bool) -> Self {
        Slideshow {
            start_after_listing,
            ..Default::default()
        }
    }

    pub(crate) fn is_running(&self) -> bool {
        self.shown_at.is_some()
    }

    pub(crate) fn is_paused(&self) -> bool {
        self.paused
    }

    /// P: paused or resumed; resuming gives the shown image the full delay again.
    pub(crate) fn toggle_pause(&mut self) {
        self.paused = !self.paused;
        self.shown_at = Some(Instant::now());
    }

    /// Stays on the image until resumed, as while a video plays.
    #[cfg(feature = "video")]
    pub(crate) fn pause(&mut self) {
        self.paused = true;
    }

    /// Another image is shown: when the slideshow runs, it gets the full delay.
    pub(crate) fn restart_delay(&mut self) {
        if self.shown_at.is_some() {
            self.shown_at = Some(Instant::now());
        }
    }

    /// Whether --slideshow starts it now, once the folder is `listed`: only once.
    pub(crate) fn starts_now(&mut self, listed: bool) -> bool {
        let now = self.start_after_listing && listed;
        self.start_after_listing &= !now;
        now
    }

    /// The file list is in a random order for the slideshow.
    pub(crate) fn is_shuffled(&self) -> bool {
        self.shuffled
    }

    /// The order chosen replaces the random one, or another folder opens. A collection's saved
    /// order still being read is not put on the list that comes next.
    pub(crate) fn unshuffle(&mut self) {
        self.shuffled = false;
        self.saved_order.clear();
    }

    /// Whether the next listing is shuffled whole in the listing thread.
    pub(crate) fn shuffles_whole(&self) -> bool {
        self.shuffled && self.shuffle_next_listing_whole
    }

    /// Whether this listing was shuffled whole. The flag is for this listing, taken whether or
    /// not the order is still random: left, it would shuffle every later listing.
    pub(crate) fn take_whole_shuffle(&mut self) -> bool {
        std::mem::take(&mut self.shuffle_next_listing_whole)
    }

    /// A slideshow of one pane at a time changes the first pane next.
    pub(crate) fn first_pane_next(&mut self) {
        self.next_pane = 0;
    }
}

impl crate::App {
    // ---- The slideshow ----

    /// Shuffle the file list (current image first) or sort it back, keeping the shown image.
    pub(crate) fn shuffle_list(&mut self, ctx: &egui::Context, on: bool) {
        if self.list.files().is_empty() {
            return;
        }
        // The shown image is not left, because replace_files and the listing keep its turn and
        // zoom when it stays.
        self.slideshow.shuffled = on;
        // Back on while a random slideshow still reads its list, which random off asked for in
        // sort order: read again, shuffled whole.
        if on && self.slideshow.shuffle_next_listing_whole && self.folder.is_listing() {
            return self.start_listing(ctx);
        }
        if !on {
            // A collection goes back to its own order, which shuffling left only in its file.
            // The file is read in the background, and `receive_saved_order` takes the order.
            if let Some(file) = self
                .folder
                .collection()
                .filter(|_| !self.folder.sorts_collection())
            {
                let file = file.to_path_buf();
                let (tx, rx) = answer_channel(&self.busy);
                let (ctx, pointer) = (ctx.clone(), Working::new(&self.busy));
                std::thread::spawn(move || {
                    // A move in the collection may still wait in the writer thread.
                    crate::files::file_io::wait_for_queued_writes();
                    // The saved entries by their real paths, the names the list has.
                    let saved = collections::load(&file).ok().map(|saved| {
                        (collections::by_real_path(&saved).into_iter())
                            .map(|(real, _)| real)
                            .collect()
                    });
                    let _ = tx.send(saved);
                    drop(pointer);
                    ctx.request_repaint();
                });
                self.slideshow.saved_order = Receiving::of(rx);
                return;
            }
            // Back to the chosen sort order; listing keeps the shown image.
            self.start_listing(ctx);
            return;
        }
        // A copy, not taken: the textures and the shown image's zoom are kept by the names in
        // the list as it is.
        let mut files = self.list.listed_names();
        let shown = files[self.list.shown()].0.clone();
        files.swap(0, self.list.shown());
        shuffle(&mut files[1..]);
        // New panes from the new order: kept, a slideshow one image at a time would still stand
        // at the end of the round and shuffle again on every frame.
        self.list.clear_panes();
        self.replace_files(ctx, files, Some(shown));
    }

    /// Puts the open collection back in its saved order, once `shuffle_list` read it. A file
    /// that could not be read leaves the order to the listing, in the chosen sort order.
    pub(crate) fn receive_saved_order(&mut self, ctx: &egui::Context) {
        // The busy pointer goes when this returns, once the order is in the list.
        let (saved, _pointer) = match self.slideshow.saved_order.take() {
            Took::Answer((Some(saved), pointer)) => (saved, pointer),
            Took::Answer((None, _)) => return self.start_listing(ctx),
            Took::Gone => return self.tell_background_stopped(tr("Reading the folder")),
            Took::Nothing => return,
        };
        // Shuffled again meanwhile, the list keeps its new order.
        if self.slideshow.shuffled {
            return;
        }
        let shown = self.list.files().get(self.list.shown()).cloned();
        let files: Vec<PathBuf> = (0..self.list.files().len()).map(|i| self.path(i)).collect();
        let identities: std::collections::HashMap<OsString, _> =
            self.list.listed_names().into_iter().collect();
        let files = (collections::in_saved_order(&saved, files).iter())
            .filter_map(|p| {
                let name = p.strip_prefix(self.folder.dir()).ok()?.as_os_str();
                Some((name.to_owned(), *identities.get(name)?))
            })
            .collect();
        self.replace_files(ctx, files, shown);
        // A listing on its way (a random slideshow's, a refresh's) would bring the order it was
        // asked for. So the folder is read again, in the saved order now shown.
        if self.folder.is_listing() {
            self.start_listing(ctx);
        }
    }

    /// Start the slideshow: with the subfolders when set, shuffled for random order.
    pub(crate) fn start_slideshow(&mut self, ctx: &egui::Context) {
        // A slideshow shows images, so one started from the grid in the image area opens the
        // image shown, and Esc goes back to the grid.
        self.panel.open_from_grid();
        if self.config.slideshow_subfolders {
            // The images of the subfolders too, listed in the background; random order
            // shuffles them all together.
            self.folder.set_recursive(true);
            self.slideshow.shuffled = self.config.slideshow_random;
            self.slideshow.shuffle_next_listing_whole = self.slideshow.shuffled;
            self.start_listing(ctx);
        } else if self.config.slideshow_random != self.slideshow.shuffled {
            self.shuffle_list(ctx, self.config.slideshow_random);
        }
        // The first slide comes at once: start as if the delay has already run out.
        let delay = std::time::Duration::from_secs(self.config.slideshow_delay);
        self.slideshow.shown_at = Some(
            Instant::now()
                .checked_sub(delay)
                .unwrap_or_else(Instant::now),
        );
        self.update_window_title(ctx);
        ctx.request_repaint();
    }

    /// A random order ends with the slideshow: the list goes back to the chosen sort order,
    /// keeping the shown image.
    pub(crate) fn stop_slideshow(&mut self, ctx: &egui::Context) {
        self.slideshow.shown_at = None;
        self.slideshow.paused = false;
        if self.folder.recursive() != self.config.show_subfolders {
            self.folder.set_recursive(self.config.show_subfolders);
            self.slideshow.shuffled = false;
            self.start_listing(ctx);
        } else if self.slideshow.shuffled {
            self.shuffle_list(ctx, false);
        }
        self.update_window_title(ctx);
    }

    /// Switch image when the time is up. Waits for a preloaded image that is still
    /// decoding, so the slideshow never shows a black screen.
    pub(crate) fn slideshow_tick(&mut self, ctx: &egui::Context) {
        let Some(last) = self.slideshow.shown_at else {
            return;
        };
        // The list to show (with the subfolders, shuffled) is still being read: a step in the
        // list before it would end a round without Repeat at once on the last image.
        if self.slideshow.shuffle_next_listing_whole && self.folder.is_listing() {
            return;
        }
        // Waits while Rename or a delete asks about the shown image.
        if self.list.files().is_empty() || self.slideshow.paused || self.dialogs.asks_about_files()
        {
            return;
        }
        // Several panes: the next set together, or the next image into one pane at a time.
        let n = self.list.panes().len() + 1;
        let turns = n > 1 && self.changes_one_pane_at_a_time();
        let (from, delta) = match n {
            1 => (self.list.shown(), 1),
            _ if turns => (self.list.last_slide(), 1),
            _ => (self.list.shown(), n),
        };
        let delay = std::time::Duration::from_secs(self.config.slideshow_delay);
        let left = delay.saturating_sub(last.elapsed());
        // The slideshow owns its clock. The next frame is asked for here, whatever this one
        // does, so that no move below can leave it standing still. While it waits for a decode,
        // it looks again soon.
        ctx.request_repaint_after(left.max(std::time::Duration::from_millis(50)));
        if !left.is_zero() {
            return;
        }
        let next = self.step_from(from, delta as isize, true);
        let m = self.list.matches();
        let row = m.map_or(Some(from), |m| self.row_of(m, from));
        let shown = m.map_or(self.list.files().len(), <[usize]>::len);
        let pending =
            next.is_some_and(|next| self.list.decodes_pending().contains_key(&(next, false)));
        let at = SlideshowPosition {
            row,
            step: delta,
            shown_rows: shown,
            file_count: self.list.files().len(),
        };
        match next_slide(
            at,
            next,
            self.config.slideshow_repeat,
            self.slideshow.shuffled,
            pending,
        ) {
            SlideStep::Stop => self.stop_slideshow(ctx),
            SlideStep::Shuffle => self.shuffle_list(ctx, true),
            SlideStep::Wait => {}
            SlideStep::Step(next) => {
                let before = self.shown_textures();
                if turns && self.slideshow.next_pane > 0 {
                    self.list.set_pane(self.slideshow.next_pane - 1, next);
                    self.refill_preload();
                } else {
                    self.show_file(ctx, next);
                }
                self.decoding.fade_from(before);
                if turns {
                    self.list.set_last_slide(next);
                    self.slideshow.next_pane = (self.slideshow.next_pane + 1) % n;
                }
                self.slideshow.shown_at = Some(Instant::now());
            }
        }
    }

    /// The fade's length: none in power saving, where every frame counts.
    pub(crate) fn fade_ms(&self) -> u64 {
        match self.config.cpu_use {
            0 => 0,
            _ => FADES[self.config.slideshow_fade].2,
        }
    }

    /// Random order changed in the menu or preferences: reshuffle now if a slideshow runs.
    pub(crate) fn set_random_order(&mut self, ctx: &egui::Context, on: bool) {
        self.config.slideshow_random = on;
        self.config.save();
        if self.slideshow.shown_at.is_some() {
            self.shuffle_list(ctx, on);
            self.update_window_title(ctx);
        }
    }

    /// Ctrl+= and Ctrl+-: the slideshow's delay a second shorter or longer.
    pub(crate) fn change_delay(&mut self, ctx: &egui::Context, action: Action) {
        let step = if action == Action::SlideshowFaster {
            -1
        } else {
            1
        };
        self.config.slideshow_delay = self
            .config
            .slideshow_delay
            .saturating_add_signed(step)
            .clamp(1, MAX_SLIDESHOW_DELAY);
        self.config.save();
        self.update_window_title(ctx);
    }
}

/// The slideshow's move from `at`, to `next` if there is one; repeating or not, in a `shuffled`
/// order, with `next` `pending` (still being decoded) or not.
pub(crate) fn next_slide(
    at: SlideshowPosition,
    next: Option<usize>,
    repeat: bool,
    shuffled: bool,
    pending: bool,
) -> SlideStep {
    let SlideshowPosition {
        row,
        step,
        shown_rows,
        file_count,
    } = at;
    let round_over = round_ends(row, step, shown_rows);
    let Some(next) = next else {
        return SlideStep::Wait;
    };
    if round_over && !repeat {
        return SlideStep::Stop;
    }
    // A new round shuffles again instead of repeating the same order. Not when a search hides
    // the shown image, nor when it shows no more images than one step: the shuffled list starts
    // with the shown image, so the round would be over again, and shuffled on every frame.
    if shuffled
        && round_over
        && row.is_some()
        && file_count > 2
        && !round_ends(Some(0), step, shown_rows)
    {
        return SlideStep::Shuffle;
    }
    match pending {
        true => SlideStep::Wait,
        false => SlideStep::Step(next),
    }
}

#[cfg(test)]
mod tests {
    use crate::support::test_support::{app_on, run_background_until};

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
            let sorted = app.list.files().to_vec();
            app.show_file(&ctx, 4);
            app.config.slideshow_subfolders = true;
            app.config.slideshow_random = true;
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
            assert!(run_background_until(&mut app, &ctx, |a| !a
                .folder
                .is_listing()));
            if stop {
                assert_eq!(app.list.files(), sorted);
            } else {
                // Shuffled, its round starting at the image shown (the first slide may have
                // come since).
                assert_eq!(
                    (app.list.files().len(), app.list.files()[0].to_str()),
                    (10, Some("4.png"))
                );
                // Not the sort order with the shown image put first.
                let mut sorted = app.list.files().to_vec();
                sorted.sort();
                let at = sorted.iter().position(|f| f == "4.png").unwrap();
                sorted.swap(0, at);
                assert_ne!(app.list.files(), sorted, "shuffled");
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
        let order = app.list.files().to_vec();
        app.config.slideshow_random = true;
        app.start_slideshow(&ctx);
        assert!(app.slideshow.shuffled && app.list.files() != order);
        // A refresh on its way.
        app.start_listing(&ctx);
        app.set_random_order(&ctx, false);
        assert!(run_background_until(&mut app, &ctx, |a| !a
            .folder
            .is_listing()
            && a.slideshow.saved_order.is_none()
            && a.status_bar.folder_total().is_some()));
        assert_eq!(app.list.files(), order);
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
            let order = app.list.files().to_vec();
            app.config.slideshow_subfolders = true;
            app.config.slideshow_random = true;
            if shuffled_first {
                app.shuffle_list(&ctx, true);
            }
            let shown = app.list.files()[app.list.shown()].clone();
            app.start_slideshow(&ctx);
            assert!(run_background_until(&mut app, &ctx, |a| !a
                .folder
                .is_listing()));
            assert!(
                app.slideshow.shuffled && app.list.files() != order,
                "R first: {shuffled_first}"
            );
            // The round starts at the image shown (the first slide may have come since): without
            // Repeat it shows every image.
            assert_eq!(app.list.files()[0], shown);
            // Again, and random off before the list comes: the saved order, and its size.
            app.stop_slideshow(&ctx);
            app.start_slideshow(&ctx);
            app.set_random_order(&ctx, false);
            assert!(run_background_until(&mut app, &ctx, |a| !a
                .folder
                .is_listing()
                && a.slideshow.saved_order.is_none()
                && a.status_bar.folder_total().is_some()));
            assert_eq!(app.list.files(), order);
            assert!(app.status_bar.folder_total() > Some(0), "size");
            std::fs::remove_dir_all(&d).unwrap();
        }
    }

    /// Random order turned off puts a collection back in its saved order, also when an entry
    /// names its image through a link. Compared as written, that entry went last.
    #[test]
    fn random_off_keeps_the_place_of_an_entry_through_a_link() {
        let d = std::env::temp_dir().join(format!("glimt-test-unshuffle-{}", std::process::id()));
        std::fs::create_dir_all(d.join("real")).unwrap();
        std::os::unix::fs::symlink(d.join("real"), d.join("link")).unwrap();
        let mut gqv = String::from("#GQview collection\n");
        for (i, at) in ["link", "real", "real"].iter().enumerate() {
            image::RgbImage::new(2, 2)
                .save(d.join(format!("real/{i}.png")))
                .unwrap();
            gqv.push_str(&format!(
                "\"{}\"\n",
                d.join(format!("{at}/{i}.png")).display()
            ));
        }
        std::fs::write(d.join("c.gqv"), gqv).unwrap();
        let (mut app, ctx) = app_on(&d.join("c.gqv"));
        let order = app.list.files().to_vec();
        app.shuffle_list(&ctx, true);
        app.shuffle_list(&ctx, false);
        // The saved order is read in the background, not in the window's thread. The busy
        // pointer stays until the list has it.
        assert!(app.slideshow.saved_order.is_some());
        while app.working() {
            app.poll_background(&ctx);
        }
        assert_eq!(app.list.files(), order);
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// A slideshow goes on, tick after tick, whatever the layout, order and search: the image
    /// shown is another one after every step, a round of a random order shuffles once and goes
    /// on, a search that hides the image or shows one image never holds it, and without repeat
    /// it stops at the end of the round and not before.
    #[test]
    fn slideshow_advances_on_every_tick() {
        use super::{SlideStep, SlideshowPosition, next_slide};
        use crate::model::selection::next_row;
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
                    let at = SlideshowPosition {
                        row,
                        step: delta,
                        shown_rows: shown,
                        file_count: len,
                    };
                    match next_slide(at, next, repeat, shuffled, false) {
                        SlideStep::Wait => assert_eq!(shown, 0, "waits: {case}"),
                        SlideStep::Stop => {
                            assert!(!repeat, "stopped: {case}");
                            stopped = true;
                            break;
                        }
                        SlideStep::Shuffle => {
                            shuffles += 1;
                            assert!(shuffles == 1 || steps > 0, "shuffled again at once: {case}");
                            // As shuffle_list: the shown image first in the new order.
                            (from, steps) = (0, 0);
                        }
                        SlideStep::Step(next) => {
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
                    // It stops at the first step that would show no row it has not shown. The
                    // start is before the first row when the search hides it.
                    let start = m.map_or(Some(0), |m| m.iter().position(|&i| i == 0));
                    let start = start.map_or(-1, |r| r as isize);
                    let rounds = (0..).find(|k| start + (k + 1) * delta as isize >= shown as isize);
                    assert_eq!(
                        Some(steps),
                        rounds,
                        "stopped early or late: {len} files, {m:?}, step {delta}"
                    );
                }
            }
        }
        // Held only while the next image is still being decoded.
        let at = SlideshowPosition {
            row: Some(0),
            step: 1,
            shown_rows: 3,
            file_count: 3,
        };
        assert_eq!(next_slide(at, Some(1), true, false, true), SlideStep::Wait);
        assert_eq!(next_row(None, 1, 3, true), Some(0));
        assert_eq!(next_row(Some(2), 1, 3, false), Some(2));
    }

    #[test]
    fn slideshow_round_ends_past_the_last() {
        // Ten images in three panes: 0, 3, 6, 9, then a new round.
        assert!(!crate::model::selection::round_ends(Some(6), 3, 10));
        assert!(crate::model::selection::round_ends(Some(9), 3, 10));
        assert!(crate::model::selection::round_ends(Some(8), 3, 10));
        assert!(crate::model::selection::round_ends(Some(0), 1, 1));
        // A hidden shown file is before the first row, so the round goes on to the last.
        assert!(!crate::model::selection::round_ends(None, 1, 5));
        assert!(!crate::model::selection::round_ends(None, 5, 5));
        assert!(crate::model::selection::round_ends(None, 6, 5));
        assert!(crate::model::selection::round_ends(None, 1, 0));
    }
}
