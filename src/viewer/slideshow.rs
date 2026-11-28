//! The slideshow: its timer, the images it picks, its round and history, and the fade. It
//! never changes the order of the file list, which stays in its sort order: the slideshow
//! keeps its own state, by the names of the images.

use crate::files::config::FADES;
use crate::files::config::MAX_SLIDESHOW_DELAY;
use crate::model::listing::name_hash;
use crate::ui::actions::Action;
use eframe::egui;
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::OsString;
use std::time::Instant;

/// How many images the slideshow's history holds, which Left goes back through. The oldest one
/// goes when a new one comes, so the history's memory is the same in a folder of any size.
const HISTORY: usize = 1_000;

/// A name that the slideshow keeps, with its place in the list. The place is `None` when the
/// list no longer holds the name.
struct PlacedName {
    name: OsString,
    place: Option<usize>,
}

/// What the slideshow's plan (the images planned, the pool and whether a pick found none) was
/// made from. Before each look at the plan, the slideshow compares these with what they are now
/// and makes the plan again when one differs (`Slideshow::make_plan`), so that no change of the
/// list, the search, the image shown, the order or the round can leave an old plan behind.
#[derive(Clone, Copy, Debug, PartialEq)]
struct PlanMadeFrom {
    /// The list's version (`List::version`).
    list: usize,
    /// The version of the search's matches (`List::matches_version`).
    search: usize,
    /// How many images came on screen that the slideshow did not put there, such as a click,
    /// a pane or `--remote show`. The plan then goes on from the image shown.
    shown_elsewhere: u64,
    /// Whether the order is random (R).
    random: bool,
    /// The number of the round.
    round: u64,
}

/// The slideshow: whether it runs, the images shown in its round, the images it shows next, and
/// the images it showed, which Left and Right go through.
#[derive(Default)]
pub(crate) struct Slideshow {
    /// When the image shown last came up. It is `None` while the slideshow is off.
    shown_at: Option<Instant>,
    /// Slideshow paused (P): stays on the image until resumed.
    paused: bool,
    /// --slideshow: start it once the folder is read.
    start_after_listing: bool,
    /// A slideshow changing one pane at a time: the pane next (0 is the first).
    next_pane: usize,
    /// The hashes of the names shown in this round (`listing::name_hash`). A round ends when
    /// every name the list shows is among them. A hash costs 8 bytes a name, where a copy of
    /// the names would cost about 50.
    round: HashSet<u64>,
    /// The number of the round, which goes up each time a round begins.
    round_number: u64,
    /// The images shown, the oldest first, at most `HISTORY` of them.
    history: VecDeque<PlacedName>,
    /// How many steps Left has gone back from the newest image of `history`. The next slide
    /// and Right go forward through the history again before they pick a new image.
    back: usize,
    /// The name of the image that the slideshow put in the first pane last. Another image
    /// there was put there in another way, and the plan then goes on from it.
    showing: Option<OsString>,
    /// How many images came on screen that the slideshow did not put there.
    shown_elsewhere: u64,
    /// The images picked to come next, so that they are decoded before their slide.
    planned: VecDeque<PlacedName>,
    /// How many images the slideshow shows next and reads ahead: the panes times one more than
    /// the images read ahead on each side (`plan_next`). That is the next slide in every pane
    /// and as many slides after it as browsing reads ahead on one side. `next_places` gives no
    /// more than these, the history after the image shown first, so the images kept decoded do
    /// not grow however far Left goes back. This is the slideshow's own bound, not the memory
    /// level's budget for browsing. With the images shown and the one that Left shows, it keeps
    /// more than browsing at some levels: 4 images against 3 at Low with one pane, and 29
    /// against about 18 in 2×2 at High.
    plan_length: usize,
    /// In random order, the places of the images that the search shows and that this round
    /// has not shown or planned. A pick takes one at random and swaps the last one into its
    /// place, so each pick costs the same at any size of folder. An image shown meanwhile in
    /// another way is passed over when it is drawn.
    pool: Vec<usize>,
    /// In order, the row of the last image picked among the rows the list shows (the search's
    /// matches, or every place), or `None` before the first row.
    cursor: Option<usize>,
    /// What the plan was made from, or `None` before the first plan.
    made_from: Option<PlanMadeFrom>,
    /// Whether a pick found no image since the plan was made, so that the next pick does not
    /// look through the rows again.
    found_none: bool,
    /// The state of the random numbers (xorshift), seeded at the first random pick.
    random_state: u64,
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

    /// Moves the time the image shown came up `passed` earlier, as if that much time went by,
    /// so that a test can run a slideshow on a clock of its own.
    #[cfg(test)]
    pub(crate) fn let_time_pass(&mut self, passed: std::time::Duration) {
        self.shown_at = self.shown_at.and_then(|at| at.checked_sub(passed));
    }

    /// Seeds the random order with `seed`, so that a test gets the same picks in every run.
    #[cfg(test)]
    pub(crate) fn seed_random_order(&mut self, seed: u64) {
        self.random_state = seed | 1;
    }

    /// Whether --slideshow starts it now, once the folder is `listed`: only once.
    pub(crate) fn starts_now(&mut self, listed: bool) -> bool {
        let now = self.start_after_listing && listed;
        self.start_after_listing &= !now;
        now
    }

    /// A slideshow of one pane at a time changes the first pane next.
    pub(crate) fn first_pane_next(&mut self) {
        self.next_pane = 0;
    }

    /// Forgets the round, the history and the image on screen, because another folder opens,
    /// where the same names are other images. The new round has the plan made again.
    pub(crate) fn forget_names(&mut self) {
        self.begin_round([]);
        self.history.clear();
        self.back = 0;
        self.showing = None;
    }

    /// Returns whether this round has shown the image named `name`.
    #[cfg(test)]
    pub(crate) fn has_shown(&self, name: &OsString) -> bool {
        self.round.contains(&name_hash(name.as_encoded_bytes()))
    }

    /// Begins a new round, in which only the images `on_screen` count as shown. This is the one
    /// place a round begins (`source_rules::a_round_begins_in_one_place`): at the start, when
    /// every image listed was shown and Repeat is on, and at Home. The new round's number has
    /// the plan made again.
    fn begin_round<'a>(&mut self, on_screen: impl IntoIterator<Item = &'a OsString>) {
        self.round.clear();
        self.round_number += 1;
        for name in on_screen {
            self.round.insert(name_hash(name.as_encoded_bytes()));
        }
    }

    /// Notes that the images `on_screen` were shown in this round, also those that came on
    /// screen in another way than by the slideshow (a click, or the next image after the one
    /// shown was deleted), so that it does not plan them again.
    fn note_on_screen<'a>(&mut self, on_screen: impl IntoIterator<Item = (usize, &'a OsString)>) {
        for (place, name) in on_screen {
            self.round.insert(name_hash(name.as_encoded_bytes()));
            self.planned.retain(|kept| kept.place != Some(place));
        }
    }

    /// Notes that the image `name`, at `place`, is shown in this round, as the newest of the
    /// history.
    fn note_shown(&mut self, name: &OsString, place: usize) {
        self.round.insert(name_hash(name.as_encoded_bytes()));
        self.history.push_back(PlacedName {
            name: name.clone(),
            place: Some(place),
        });
        if self.history.len() > HISTORY {
            self.history.pop_front();
        }
        self.back = 0;
    }

    /// Notes that the slideshow puts the image `name` in the first pane.
    fn note_showing(&mut self, name: &OsString) {
        self.showing = Some(name.clone());
    }

    /// Notes that the image `name`, at `place`, came into the first pane in another way than by
    /// the slideshow: a click in the file list or on a pane, `--remote show`, Home or End, or
    /// the next image after the one shown was deleted. It joins the round and the history, and
    /// the plan is made again to go on from it.
    fn note_shown_elsewhere(&mut self, name: &OsString, place: usize) {
        self.note_shown(name, place);
        self.note_showing(name);
        self.shown_elsewhere += 1;
    }

    /// Gives the names that were renamed in place, `renames` as (old, new), their new names
    /// in the round, the history, the images planned and the image on screen, so that an image
    /// renamed is not shown again in its round.
    pub(crate) fn follow_renames(&mut self, renames: &[(OsString, OsString)]) {
        for (old, new) in renames {
            if self.round.remove(&name_hash(old.as_encoded_bytes())) {
                self.round.insert(name_hash(new.as_encoded_bytes()));
            }
            for kept in self.history.iter_mut().chain(self.planned.iter_mut()) {
                if kept.name == *old {
                    kept.name = new.clone();
                }
            }
            if self.showing.as_ref() == Some(old) {
                self.showing = Some(new.clone());
            }
        }
    }

    /// Makes the plan again when `inputs`, what it would be made from now, differ from what it
    /// was made from. This is the one place the plan is made
    /// (`source_rules::the_slideshows_plan_is_made_in_one_place`).
    ///
    /// - A new version of the list `files` has the places of the history and of the images
    ///   planned found again, by their names, in one pass over the list.
    /// - The images planned stay when only the list or the search changed and `shows` still
    ///   shows them, so that their decodes are not lost while the parts of a tree come in.
    ///   Otherwise they are planned anew: in order after the place `from`, and in random order
    ///   from the pool.
    /// - In random order, the pool is made anew of the places among `rows` (the search's
    ///   matches, or every place when it is `None`) that this round has not shown or planned.
    ///   When only the image shown changed, the images planned go back into it instead.
    fn make_plan(
        &mut self,
        (files, rows): (&[OsString], Option<&[usize]>),
        inputs: PlanMadeFrom,
        from: Option<usize>,
        shows: impl Fn(usize) -> bool,
    ) {
        let old = self.made_from;
        if old == Some(inputs) {
            return;
        }
        self.made_from = Some(inputs);
        self.found_none = false;
        if old.is_none_or(|old| old.list != inputs.list) {
            self.find_places(files);
        }
        let same_order = old.is_some_and(|old| {
            (old.shown_elsewhere, old.random, old.round)
                == (inputs.shown_elsewhere, inputs.random, inputs.round)
        });
        let mut dropped = Vec::new();
        self.planned.retain(|kept| {
            let keep = same_order && kept.place.is_some_and(|i| i < files.len() && shows(i));
            if !keep {
                dropped.extend(kept.place);
            }
            keep
        });
        let pool_holds_the_rows = old.is_some_and(|old| {
            old.random
                && (old.list, old.search, old.round) == (inputs.list, inputs.search, inputs.round)
        });
        if !inputs.random {
            self.pool.clear();
        } else if pool_holds_the_rows {
            self.pool.extend(dropped);
        } else {
            self.pool.clear();
            let planned: HashSet<usize> = self.planned.iter().filter_map(|p| p.place).collect();
            let round = &self.round;
            let unshown = |i: &usize| {
                *i < files.len()
                    && !planned.contains(i)
                    && !round.contains(&name_hash(files[*i].as_encoded_bytes()))
            };
            match rows {
                Some(rows) => self.pool.extend(rows.iter().copied().filter(unshown)),
                None => self.pool.extend((0..files.len()).filter(unshown)),
            }
        }
        let last = self.planned.back().and_then(|kept| kept.place).or(from);
        self.cursor = last.and_then(|i| match rows {
            None => Some(i),
            Some(rows) => rows.iter().position(|&row| row == i),
        });
    }

    /// Finds the places of the names it keeps (the history and the images planned) again in
    /// `files`, a new version of the list, in one pass over it.
    fn find_places(&mut self, files: &[OsString]) {
        let mut found: HashMap<u64, Option<usize>> = (self.history.iter())
            .chain(&self.planned)
            .map(|kept| (name_hash(kept.name.as_encoded_bytes()), None))
            .collect();
        if !found.is_empty() {
            for (i, name) in files.iter().enumerate() {
                if let Some(place @ None) = found.get_mut(&name_hash(name.as_encoded_bytes())) {
                    *place = Some(i);
                }
            }
        }
        for kept in self.history.iter_mut().chain(self.planned.iter_mut()) {
            let hash = name_hash(kept.name.as_encoded_bytes());
            let place = found.get(&hash).copied().flatten();
            kept.place = place.filter(|&i| files[i] == kept.name);
        }
    }

    /// Returns the place of the next image to plan in `files`, among the places of `rows` (the
    /// search's matches, or every place when it is `None`): in random order one drawn from the
    /// pool, and otherwise the next row after the last one picked that this round has not shown
    /// or planned, going on from the first row after the last. Returns `None` when there is
    /// none, and then looks no more until the plan is made again. A pick goes through the rows
    /// the search shows, not through the whole list, so a search with few matches costs little.
    /// Each pick costs the same at any size of folder, but for the rows an order passes over,
    /// which it passes over once in a round.
    fn pick(&mut self, files: &[OsString], rows: Option<&[usize]>) -> Option<usize> {
        if self.found_none || files.is_empty() {
            return None;
        }
        let unshown = |slideshow: &Slideshow, i: usize| {
            i < files.len()
                && !(slideshow.round).contains(&name_hash(files[i].as_encoded_bytes()))
                && !(slideshow.planned.iter()).any(|kept| kept.place == Some(i))
        };
        let random = self.made_from.is_some_and(|made| made.random);
        let picked = match random {
            true => loop {
                if self.pool.is_empty() {
                    break None;
                }
                let drawn = (self.random() % self.pool.len() as u64) as usize;
                let i = self.pool.swap_remove(drawn);
                if unshown(self, i) {
                    break Some(i);
                }
            },
            false => {
                let n = rows.map_or(files.len(), <[usize]>::len);
                let place = |row: usize| rows.map_or(row, |rows| rows[row]);
                let start = self.cursor.map_or(0, |row| row + 1);
                let row = (0..n)
                    .map(|d| (start + d) % n)
                    .find(|&row| unshown(self, place(row)));
                if row.is_some() {
                    self.cursor = row;
                }
                row.map(place)
            }
        };
        self.found_none = picked.is_none();
        picked
    }

    /// Plans images to come next in `files`, among `rows`, until `length` are planned or none
    /// is left.
    fn plan_next(&mut self, files: &[OsString], rows: Option<&[usize]>, length: usize) {
        self.plan_length = length;
        while self.planned.len() < length {
            let Some(i) = self.pick(files, rows) else {
                return;
            };
            self.planned.push_back(PlacedName {
                name: files[i].clone(),
                place: Some(i),
            });
        }
    }

    /// Returns the places of the images planned, the next one first.
    pub(crate) fn planned_places(&self) -> impl Iterator<Item = usize> + '_ {
        self.planned.iter().filter_map(|kept| kept.place)
    }

    /// Returns the places of the images that the slideshow shows next, the next one first. When
    /// Left went back, these are first the images of the history after the one shown, which the
    /// next slide and Right show again, and then the images planned. This one list is what the
    /// next slide shows (`next_slide`), what the tick waits for, and what is read ahead
    /// (`kept_places`) and awaited on a share (`awaited_places`). Before, the images read ahead
    /// and awaited were only the images planned, so after Left the image that the next slide
    /// showed was decoded again, and on a share its read waited behind the walk. The list holds
    /// at most the plan's length (`plan_length`), the history first. Before, it held the whole
    /// history after the image shown, and each press of Left kept one more image decoded.
    pub(crate) fn next_places(&self) -> impl Iterator<Item = usize> + '_ {
        let after_shown = self.history.len() - self.back.min(self.history.len());
        (self.history.range(after_shown..))
            .filter_map(|kept| kept.place)
            .chain(self.planned_places())
            .take(self.plan_length)
    }

    /// Returns the places of the pool, for the tests to check that the search shows each.
    #[cfg(test)]
    pub(crate) fn pool(&self) -> &[usize] {
        &self.pool
    }

    /// Returns the place of the image before the one shown in the history, which Left shows.
    pub(crate) fn previous_place(&self) -> Option<usize> {
        let newest = self.history.len().checked_sub(1 + self.back)?;
        (self.history.range(..newest).rev()).find_map(|kept| kept.place)
    }

    /// Takes up to `n` of the images planned, the next one first, by their places.
    fn take_planned(&mut self, n: usize) -> Vec<usize> {
        let n = n.min(self.planned.len());
        (self.planned.drain(..n))
            .filter_map(|kept| kept.place)
            .collect()
    }

    /// Steps through the history, one image back when `older` and otherwise one forward, past
    /// the images the list no longer holds. Returns the place of the image to show, which the
    /// slideshow puts in the first pane, or `None` when the history has no image there.
    fn step_through_history(&mut self, older: bool) -> Option<usize> {
        let mut back = self.back;
        loop {
            back = match older {
                true => back + 1,
                false => back.checked_sub(1)?,
            };
            let at = self.history.len().checked_sub(1 + back)?;
            if let Some(place) = self.history[at].place {
                self.back = back;
                self.showing = Some(self.history[at].name.clone());
                return Some(place);
            }
        }
    }

    /// Returns the next random number (xorshift), seeded from std's random hash key the first
    /// time.
    fn random(&mut self) -> u64 {
        if self.random_state == 0 {
            use std::hash::{BuildHasher, Hasher};
            let seed = std::collections::hash_map::RandomState::new().build_hasher();
            self.random_state = seed.finish() | 1;
        }
        let x = &mut self.random_state;
        *x ^= *x << 13;
        *x ^= *x >> 7;
        *x ^= *x << 17;
        *x
    }
}

impl crate::App {
    // ---- The slideshow ----

    /// Start the slideshow: with the subfolders when set, which it plays as they are read.
    pub(crate) fn start_slideshow(&mut self, ctx: &egui::Context) {
        // A slideshow shows images, so one started from the grid in the image area opens the
        // image shown, and Esc goes back to the grid.
        self.panel.open_from_grid();
        // The first slide comes at once: start as if the delay has already run out. The
        // slideshow runs from here, so every listing of a tree started below sends only the
        // images new to the list (`start_listing`).
        let delay = std::time::Duration::from_secs(self.config.slideshow_delay);
        self.slideshow.shown_at = Some(
            Instant::now()
                .checked_sub(delay)
                .unwrap_or_else(Instant::now),
        );
        self.slideshow.forget_names();
        self.begin_round();
        if let Some(name) = self.list.files().get(self.list.shown()).cloned() {
            self.slideshow.note_shown(&name, self.list.shown());
            self.slideshow.note_showing(&name);
        }
        if self.config.slideshow_subfolders {
            // The images of the subfolders too, listed in the background. The images already
            // listed play at once, while the tree is read, which on a share can take minutes,
            // and the tree's images join the list in parts as they are read. A tree already
            // listed whole as it would be read now is not read again, and a listing that runs
            // starts anew as before. Before, the tree was read again at every start, also just
            // after it had been read whole.
            let listed = self.lists_the_tree_whole();
            self.folder.set_recursive(true);
            if !listed {
                self.start_listing(ctx);
            }
        }
        self.follow_list_in_slideshow();
        self.refill_preload();
        self.update_window_title(ctx);
        ctx.request_repaint();
    }

    /// Stops the slideshow. The image shown stays the one shown, and the file list, which is
    /// in its sort order all along, scrolls to it. A slideshow that read the subfolders goes
    /// back to the folder's own images when the image shown is one of them. Otherwise the list
    /// keeps the tree, so that the image shown stays in it, until another folder opens or
    /// Subfolders is chosen.
    pub(crate) fn stop_slideshow(&mut self, ctx: &egui::Context) {
        self.slideshow.shown_at = None;
        self.slideshow.paused = false;
        self.slideshow.forget_names();
        // The image shown is the one the user stopped on, which no whole list that comes after
        // moves (`Folder::forget_image_shown_by_itself`).
        self.folder.forget_image_shown_by_itself();
        let shown = self.list.files().get(self.list.shown());
        let in_the_folder = shown.is_some_and(|name| self.folder.holds_as_its_own(name));
        if self.folder.recursive() != self.config.include_subfolders
            && (self.config.include_subfolders || in_the_folder || shown.is_none())
        {
            self.folder.set_recursive(self.config.include_subfolders);
            self.start_listing(ctx);
        }
        self.panel.scroll_to_shown();
        self.refill_preload();
        self.update_window_title(ctx);
    }

    /// Begins a new round of the slideshow, in which the images on screen count as shown.
    fn begin_round(&mut self) {
        let mut on_screen = vec![self.list.shown()];
        on_screen.extend(self.images_in_other_panes());
        let files = self.list.files();
        self.slideshow
            .begin_round(on_screen.into_iter().filter_map(|i| files.get(i)));
    }

    /// Makes the slideshow's plan again when what it was made from changed, and plans the
    /// images to come next. Every look at the plan comes after it: each pick, and the images
    /// read ahead (`refill_preload`). So an image shown by hand, a search changed, the Types or
    /// Show Hidden Files changed, a rename, a part of a tree, R and a new round each have the
    /// plan made again, with no caller to forget. An image in the first pane that the slideshow
    /// did not put there is noted as shown, and the plan goes on from it. The changes of the
    /// list call it too (`replace_files`, `remove_indices`, `rename_in_list`), so that the one
    /// pass over the list it makes is paid by the change, and a slide costs the same at any size
    /// of folder.
    pub(crate) fn follow_list_in_slideshow(&mut self) {
        if !self.slideshow.is_running() {
            return;
        }
        let shown = self.list.shown();
        let on_screen = self.list.files().get(shown);
        if let Some(name) = on_screen.filter(|&name| self.slideshow.showing.as_ref() != Some(name))
        {
            let name = name.clone();
            self.slideshow.note_shown_elsewhere(&name, shown);
        }
        let n = self.list.panes().len() + 1;
        let ahead = self.decoding.preload_count();
        let slideshow = &mut self.slideshow;
        let inputs = PlanMadeFrom {
            list: self.list.version(),
            search: self.list.matches_version(),
            shown_elsewhere: slideshow.shown_elsewhere,
            random: self.config.slideshow_random,
            round: slideshow.round_number,
        };
        let from = match slideshow.back {
            0 => Some(shown),
            _ => slideshow.history.back().and_then(|kept| kept.place),
        };
        let list = &self.list;
        let (files, rows) = (list.files(), list.matches());
        slideshow.make_plan((files, rows), inputs, from, |i| list.shows_file(i));
        slideshow.plan_next(files, rows, n * (1 + ahead));
    }

    /// Switch image when the time is up. Waits while the next image (`Slideshow::next_places`)
    /// is still decoding, so the slideshow never shows a black screen.
    pub(crate) fn slideshow_tick(&mut self, ctx: &egui::Context) {
        let Some(last) = self.slideshow.shown_at else {
            return;
        };
        // Waits while Rename or a delete asks about the shown image.
        if self.list.files().is_empty() || self.slideshow.paused || self.dialogs.asks_about_files()
        {
            return;
        }
        let delay = std::time::Duration::from_secs(self.config.slideshow_delay);
        let left = delay.saturating_sub(last.elapsed());
        // The slideshow owns its clock. The next frame is asked for here, whatever this one
        // does, so that no move below can leave it standing still. While it waits for a decode,
        // it looks again soon.
        ctx.request_repaint_after(left.max(std::time::Duration::from_millis(50)));
        if !left.is_zero() {
            return;
        }
        self.follow_list_in_slideshow();
        let next = self.slideshow.next_places().next();
        let pending = next.is_some_and(|i| self.list.decodes_pending().contains_key(&(i, false)));
        if pending {
            return;
        }
        self.next_slide(ctx);
    }

    /// Shows the next slide, the first of `Slideshow::next_places`: the image after the one
    /// shown in the history when Left went back, and otherwise the next images planned. When
    /// every image listed has been shown, Repeat begins a new round at once and the slideshow
    /// stops without it. It never waits for the listing of a tree, whose images join the round
    /// as they come.
    fn next_slide(&mut self, ctx: &egui::Context) {
        if let Some(place) = self.slideshow.step_through_history(false) {
            self.show_file(ctx, place);
            self.slideshow.shown_at = Some(Instant::now());
            return;
        }
        // Several panes: the next set together, or the next image into one pane at a time.
        let n = self.list.panes().len() + 1;
        let turns = n > 1 && self.changes_one_pane_at_a_time();
        let wanted = if turns { 1 } else { n };
        self.follow_list_in_slideshow();
        let mut on_screen = vec![self.list.shown()];
        on_screen.extend(self.images_in_other_panes());
        let files = self.list.files();
        let on_screen = on_screen
            .into_iter()
            .filter_map(|i| Some((i, files.get(i)?)));
        self.slideshow.note_on_screen(on_screen);
        self.follow_list_in_slideshow();
        let mut picks = self.slideshow.take_planned(wanted);
        if picks.is_empty() {
            if !self.config.slideshow_repeat {
                return self.stop_slideshow(ctx);
            }
            self.begin_round();
            self.follow_list_in_slideshow();
            picks = self.slideshow.take_planned(wanted);
        }
        let Some(&first) = picks.first() else {
            // The list holds only the images on screen.
            self.slideshow.shown_at = Some(Instant::now());
            return;
        };
        for &i in &picks {
            let name = self.list.files()[i].clone();
            self.slideshow.note_shown(&name, i);
        }
        self.follow_list_in_slideshow();
        let before = self.shown_textures();
        if turns && self.slideshow.next_pane > 0 {
            self.list.set_pane(self.slideshow.next_pane - 1, first);
            self.refill_preload();
        } else {
            let name = self.list.files()[first].clone();
            self.slideshow.note_showing(&name);
            self.show_file(ctx, first);
        }
        if !turns && n > 1 {
            for (k, &i) in picks.iter().enumerate().skip(1) {
                self.list.set_pane(k - 1, i);
            }
            self.refill_preload();
        }
        self.decoding.fade_from(before);
        if turns {
            self.list.set_last_slide(first);
            self.slideshow.next_pane = (self.slideshow.next_pane + 1) % n;
        }
        self.slideshow.shown_at = Some(Instant::now());
    }

    /// First, Previous, Next and Last while the slideshow runs. Left goes back through the
    /// images shown, and Right forward through them, and past the newest one Right shows the
    /// next slide. Home begins a new round at the first image of the list, and End shows the
    /// last one, which joins the round, so the next slide goes on from the start of the list.
    /// Both show their image as a click does, so the plan goes on from it
    /// (`follow_list_in_slideshow`). Returns whether the slideshow took the key.
    pub(crate) fn move_in_slideshow(&mut self, ctx: &egui::Context, action: Action) -> bool {
        if !self.slideshow.is_running() {
            return false;
        }
        match action {
            Action::Previous => {
                if let Some(place) = self.slideshow.step_through_history(true) {
                    self.show_file(ctx, place);
                }
            }
            Action::Next => self.next_slide(ctx),
            Action::First | Action::Last => {
                let delta = match action {
                    Action::First => -isize::MAX / 2,
                    _ => isize::MAX / 2,
                };
                let Some(to) = self.step(delta, false) else {
                    return true;
                };
                self.show_file(ctx, to);
                if action == Action::First {
                    self.begin_round();
                }
                self.refill_preload();
            }
            _ => return false,
        }
        true
    }

    /// The fade's length: none in power saving, where every frame counts.
    pub(crate) fn fade_ms(&self) -> u64 {
        match self.config.cpu_use {
            0 => 0,
            _ => FADES[self.config.slideshow_fade].2,
        }
    }

    /// Random order changed in the menu, the preferences or by `--remote random`. In a slideshow
    /// that runs it changes only how the next images are picked, at once: the round and the
    /// images shown stay, and the list keeps its order.
    pub(crate) fn set_random_order(&mut self, ctx: &egui::Context, on: bool) {
        self.config.slideshow_random = on;
        self.config.save();
        // The order is among what the plan was made from, so the images read ahead are picked
        // again here.
        if self.slideshow.is_running() {
            self.refill_preload();
        }
        self.update_window_title(ctx);
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

#[cfg(test)]
mod tests {
    use crate::support::test_support::{app_on, run_background_until};
    use std::collections::HashSet;
    use std::ffi::OsString;

    /// Makes a folder of its own named after `name` with the images `names`, and returns it.
    fn folder_of(name: &str, names: &[&str]) -> std::path::PathBuf {
        let pid = std::process::id();
        let d = std::env::temp_dir().join(format!("glimt-test-slideshow-{name}-{pid}"));
        let _ = std::fs::remove_dir_all(&d);
        for name in names {
            std::fs::create_dir_all(d.join(name).parent().unwrap()).unwrap();
            image::RgbImage::new(2, 2).save(d.join(name)).unwrap();
        }
        d
    }

    /// The name of the image shown.
    fn shown(app: &crate::App) -> OsString {
        app.list.files()[app.list.shown()].clone()
    }

    /// Lets the delay run out, takes the decodes in flight and gives the slideshow its tick.
    fn tick(app: &mut crate::App, ctx: &eframe::egui::Context) {
        let start = std::time::Instant::now();
        while !app.list.decodes_pending().is_empty()
            && start.elapsed() < std::time::Duration::from_secs(5)
        {
            app.receive_decoded(ctx);
        }
        app.slideshow.shown_at =
            Some(std::time::Instant::now() - std::time::Duration::from_secs(9999));
        app.slideshow_tick(ctx);
    }

    /// A slideshow of the subfolders started on a tree that is listed whole, as it would be
    /// read now, plays that list and does not read the tree again. One started after Show Hidden
    /// Files changed reads it, as the list is not what a listing would read now. Before, every
    /// start read the whole tree again, which on a share takes minutes.
    #[test]
    fn a_slideshow_of_a_tree_listed_whole_does_not_read_it_again() {
        let d = folder_of("listed-whole", &["a.png", "s1/b.png", "s2/c.png"]);
        let (mut app, ctx) = app_on(&d);
        // View → Subfolders on too, so that the stop below keeps the tree.
        (
            app.config.slideshow_subfolders,
            app.config.include_subfolders,
        ) = (true, true);
        app.folder.include_subfolders(true);
        app.start_listing(&ctx);
        let listings = app.folder.whole_listings();
        assert!(run_background_until(&mut app, &ctx, |app| app
            .folder
            .whole_listings()
            > listings));
        assert_eq!(app.list.files().len(), 3);
        app.start_slideshow(&ctx);
        assert!(!app.folder.is_listing(), "the tree was read again");
        app.stop_slideshow(&ctx);
        assert!(!run_background_until(&mut app, &ctx, |app| app
            .folder
            .is_listing()));
        app.config.show_hidden = !app.config.show_hidden;
        app.start_slideshow(&ctx);
        assert!(app.folder.is_listing(), "a list of other files was played");
        std::fs::remove_dir_all(d).unwrap();
    }

    /// A random slideshow with subfolders, without Repeat, started on the last image of the
    /// folder: the file list stays in its sort order all along, also once the tree is read,
    /// and the slideshow shows every image of the tree once and then stops on the last one
    /// shown, which stays shown. Before, the list was shuffled for the slideshow and put back in
    /// the sort order when it stopped.
    #[test]
    fn a_random_slideshow_keeps_the_list_sorted_and_shows_every_image_once() {
        let names: Vec<String> = (0..5)
            .flat_map(|i| [format!("{i}.png"), format!("sub/{i}.png")])
            .collect();
        let d = folder_of(
            "slides-sorted",
            &names.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        let (mut app, ctx) = app_on(&d);
        app.show_file(&ctx, 4);
        app.config.slideshow_subfolders = true;
        app.config.slideshow_random = true;
        app.config.slideshow_repeat = false;
        app.config.slideshow_delay = 3600;
        app.start_slideshow(&ctx);
        // The first slide would come at once, while the tree is read.
        app.slideshow.shown_at = Some(std::time::Instant::now());
        let mut seen = vec![shown(&app)];
        assert!(run_background_until(&mut app, &ctx, |a| !a
            .folder
            .is_listing()));
        let mut sorted = app.list.files().to_vec();
        sorted.sort();
        assert_eq!(app.list.files(), sorted, "the list in its sort order");
        assert_eq!(sorted.len(), 10);
        while app.slideshow.is_running() && seen.len() < 20 {
            tick(&mut app, &ctx);
            if app.slideshow.is_running() {
                seen.push(shown(&app));
            }
        }
        let once: HashSet<&OsString> = seen.iter().collect();
        assert_eq!(
            (seen.len(), once.len()),
            (10, 10),
            "each image once: {seen:?}"
        );
        assert!(
            !app.slideshow.is_running(),
            "it stops once every image was shown"
        );
        assert_eq!(shown(&app), seen[9], "the last image shown stays shown");
        assert_eq!(app.list.files(), sorted, "the list in its sort order");
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// A stop keeps the image shown as the image shown, also in random order, and the file list
    /// scrolls to it. A slideshow that read the subfolders while View → Subfolders is off goes
    /// back to the folder's own images when the image shown is one of them, and otherwise keeps
    /// the tree, so that the image shown stays in the list. Before, the list was read again
    /// in the sort order at the stop, and the image shown was the first of the folder when the
    /// image of the stop was in a subfolder.
    #[test]
    fn a_stop_keeps_the_image_shown() {
        let d = folder_of("slides-stop", &["a.png", "b.png", "sub/c.png"]);
        for in_sub in [false, true] {
            let (mut app, ctx) = app_on(&d);
            app.config.slideshow_subfolders = true;
            app.config.slideshow_random = true;
            app.config.slideshow_delay = 3600;
            app.start_slideshow(&ctx);
            app.slideshow.shown_at = Some(std::time::Instant::now());
            assert!(run_background_until(&mut app, &ctx, |a| !a
                .folder
                .is_listing()));
            let name: OsString = if in_sub { "sub/c.png" } else { "b.png" }.into();
            let at = app.list.files().iter().position(|f| *f == name).unwrap();
            app.show_file(&ctx, at);
            app.stop_slideshow(&ctx);
            assert_eq!(
                shown(&app),
                name,
                "the image shown, in a subfolder: {in_sub}"
            );
            assert!(app.panel.scrolls_to_shown(), "scrolled to it");
            assert!(run_background_until(&mut app, &ctx, |a| !a
                .folder
                .is_listing()));
            let list: Vec<OsString> = match in_sub {
                true => ["a.png", "b.png", "sub/c.png"].map(Into::into).to_vec(),
                false => ["a.png", "b.png"].map(Into::into).to_vec(),
            };
            assert_eq!(app.list.files(), list, "in a subfolder: {in_sub}");
            assert_eq!(
                shown(&app),
                name,
                "the image shown, in a subfolder: {in_sub}"
            );
        }
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// A random slideshow with subfolders steps through the images already listed while the
    /// whole tree is read again, instead of waiting for it (#256).
    #[test]
    fn a_random_slideshow_steps_while_its_list_is_read() {
        let d = folder_of(
            "slides-step",
            &["0.png", "1.png", "2.png", "3.png", "4.png"],
        );
        let (mut app, ctx) = app_on(&d);
        app.config.slideshow_subfolders = true;
        app.config.slideshow_random = true;
        app.config.slideshow_repeat = false;
        app.start_slideshow(&ctx);
        let first = shown(&app);
        // The listing's answer stays in its channel, as only the decodes are taken here. The
        // wait of 5 s, as `run_background_until`'s, leaves room for a decode on a loaded machine.
        let stepped = (0..250).any(|_| {
            app.receive_decoded(&ctx);
            app.slideshow.shown_at =
                Some(std::time::Instant::now() - std::time::Duration::from_secs(99));
            app.slideshow_tick(&ctx);
            std::thread::sleep(std::time::Duration::from_millis(20));
            shown(&app) != first
        });
        assert!(app.folder.is_listing(), "the list is still being read");
        assert!(stepped, "no slide while the list is read");
        assert!(
            app.slideshow.shown_at.is_some(),
            "ended before its list came"
        );
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// A slideshow of a tree in name order, without Repeat, plays the images listed so far
    /// while the tree is read. A part of the tree puts its images into the list in the sort
    /// order, and the slideshow goes on from the image shown to the next one not shown, and
    /// from the start of the list after its end, so every image listed is shown once. An image
    /// planned stays the next one when a part comes, so that its decode is not lost. The end
    /// of the round does not wait for the walk: once every image listed so far was shown, it
    /// ends, also while the tree is still read. Before, only a random slideshow took the tree in
    /// parts, so this one ended on the folder's own image before the subfolders' images were
    /// read, and then the end of every round waited for the walk, which on a share took minutes
    /// with the last image standing still (#256).
    #[test]
    fn a_slideshow_of_a_tree_in_name_order_plays_it_as_it_is_read() {
        let d = folder_of("slides-name", &["b.png", "a/1.png", "c/3.png"]);
        let (mut app, ctx) = app_on(&d);
        // View → Subfolders on, but the folder was listed before without them.
        app.config.include_subfolders = true;
        app.folder.set_recursive(true);
        app.config.slideshow_subfolders = true;
        app.config.slideshow_random = false;
        app.config.slideshow_repeat = false;
        app.config.slideshow_delay = 3600;
        app.start_slideshow(&ctx);
        let mut played = vec![shown(&app)];
        // The part waits in its channel, beside the whole list, until it is taken here.
        let part = app.folder.wait_for_part().expect("no part of the tree");
        app.take_listed_files(&ctx, part, false);
        // The next part, as the walk would send it after the subfolders on a share: the images
        // new to the list, in name order.
        let part: Vec<_> = (["a/1.png", "c/3.png"].into_iter())
            .filter(|name| !app.list.files().iter().any(|f| f == name))
            .map(|name| (name.into(), None))
            .collect();
        app.take_listed_files(&ctx, part, false);
        let sorted: Vec<OsString> = ["a/1.png", "b.png", "c/3.png"].map(Into::into).to_vec();
        assert_eq!(app.list.files(), sorted, "the parts in the sort order");
        while played.len() < 3 && app.slideshow.is_running() {
            tick(&mut app, &ctx);
            played.push(shown(&app));
        }
        // The first part brings a/1.png, the next one after b.png from the start of the list,
        // and then c/3.png, after it.
        let order: Vec<OsString> = ["b.png", "a/1.png", "c/3.png"].map(Into::into).to_vec();
        assert_eq!(played, order, "on from the start, then the image after it");
        assert!(app.folder.is_listing(), "the whole tree has not come yet");
        tick(&mut app, &ctx);
        assert!(
            !app.slideshow.is_running(),
            "ended once every image listed was shown, without waiting for the walk"
        );
        assert!(run_background_until(&mut app, &ctx, |a| !a
            .folder
            .is_listing()));
        assert_eq!(app.list.files(), sorted, "the sort order");
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// R changes only how the next image is picked, at once and in either direction, while the
    /// round goes on: the list keeps its sort order, no image is shown twice in the round, and
    /// after R off the next image is the next one in the list that the round has not shown.
    /// Before, R put the images not played in another order in the list itself, which took a
    /// thread for most sort orders and lost images in four rounds of audits.
    #[test]
    fn r_changes_only_how_the_next_image_is_picked() {
        let names: Vec<String> = (0..40).map(|i| format!("{i:02}.png")).collect();
        let d = folder_of(
            "slides-r",
            &names.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        let (mut app, ctx) = app_on(&d);
        let sorted = app.list.files().to_vec();
        app.config.slideshow_subfolders = false;
        app.config.slideshow_random = false;
        app.config.slideshow_repeat = false;
        app.config.slideshow_delay = 3600;
        app.start_slideshow(&ctx);
        let mut seen = vec![shown(&app)];
        for k in 0..60 {
            if k % 7 == 3 {
                let on = !app.config.slideshow_random;
                app.set_random_order(&ctx, on);
                assert_eq!(app.list.files(), sorted, "R keeps the list as it is");
            }
            let random = app.config.slideshow_random;
            let at = app.list.shown();
            tick(&mut app, &ctx);
            if !app.slideshow.is_running() {
                break;
            }
            if !random {
                let next = (1..sorted.len())
                    .map(|d| (at + d) % sorted.len())
                    .find(|&i| !seen.contains(&sorted[i]));
                assert_eq!(Some(app.list.shown()), next, "the next image not shown");
            }
            seen.push(shown(&app));
        }
        let once: HashSet<&OsString> = seen.iter().collect();
        assert_eq!((seen.len(), once.len()), (40, 40), "each image once");
        assert_eq!(app.list.files(), sorted);
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// Left goes back through the images the slideshow showed, and Right forward through them
    /// again, and past the newest one Right shows a new one. The next slide after Left goes
    /// forward through the history too. Home begins a new round at the first image of the list,
    /// and End shows its last image, after which the slideshow goes on from the start.
    #[test]
    fn left_and_right_go_through_the_images_shown() {
        use crate::ui::actions::Action;
        let names: Vec<String> = (0..12).map(|i| format!("{i:02}.png")).collect();
        let d = folder_of(
            "slides-history",
            &names.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        let (mut app, ctx) = app_on(&d);
        app.config.slideshow_random = true;
        app.config.slideshow_repeat = true;
        app.config.slideshow_delay = 3600;
        app.start_slideshow(&ctx);
        let mut seen = vec![shown(&app)];
        for _ in 0..4 {
            tick(&mut app, &ctx);
            seen.push(shown(&app));
        }
        for back in (1..=3).rev() {
            app.move_along(&ctx, Action::Previous);
            assert_eq!(shown(&app), seen[back], "Left");
        }
        app.move_along(&ctx, Action::Next);
        assert_eq!(shown(&app), seen[2], "Right");
        tick(&mut app, &ctx);
        assert_eq!(shown(&app), seen[3], "the next slide after Left");
        app.move_along(&ctx, Action::Next);
        app.move_along(&ctx, Action::Next);
        assert!(!seen.contains(&shown(&app)), "a new image past the newest");
        app.move_along(&ctx, Action::First);
        assert_eq!(app.list.shown(), 0, "Home");
        // Only the image Home shows counts as shown in the new round. Before, this asked only
        // about one image, which the round before had seldom shown (#287).
        let first = shown(&app);
        let still_shown: Vec<&OsString> = (app.list.files().iter())
            .filter(|f| **f != first && app.slideshow.has_shown(f))
            .collect();
        assert!(
            still_shown.is_empty() && app.slideshow.has_shown(&first),
            "Home begins a new round: {still_shown:?}"
        );
        app.set_random_order(&ctx, false);
        app.move_along(&ctx, Action::Last);
        assert_eq!(app.list.shown(), 11, "End");
        tick(&mut app, &ctx);
        assert_eq!(
            app.list.shown(),
            1,
            "on from the start, past the first shown at Home"
        );
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// After Left, the image that the next slide and Right show, the one shown before Left, is
    /// the next image read ahead and awaited, also on a share, and the tick waits for its
    /// decode as for an image planned. Before, only the images planned were read ahead and
    /// awaited, so after Left that image was dropped and decoded again, the tick showed it
    /// before its decode came, and on a share its read waited behind the walk of the tree.
    #[test]
    fn after_left_the_next_slide_is_read_ahead_and_awaited() {
        use crate::ui::actions::Action;
        let names: Vec<String> = (0..12).map(|i| format!("{i:02}.png")).collect();
        let d = folder_of(
            "after-left",
            &names.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        let (mut app, ctx) = app_on(&d);
        app.config.slideshow_random = true;
        app.config.slideshow_delay = 3600;
        app.start_slideshow(&ctx);
        for _ in 0..3 {
            tick(&mut app, &ctx);
        }
        let newest = app.list.shown();
        app.move_along(&ctx, Action::Previous);
        let back_at = app.list.shown();
        assert_ne!(back_at, newest, "Left");
        // The decoded images go, so that each image read ahead is asked for again.
        app.list.keep_loaded_images(|_, _| false);
        app.refill_preload();
        let (kept, _) = app.kept_places();
        assert_eq!(
            kept.get(1),
            Some(&newest),
            "the next image after Left, read ahead"
        );
        let awaited = app.decoding.images_awaited().paths();
        assert!(awaited.contains(&app.path(newest)), "awaited: {awaited:?}");
        // Its decode is not taken yet, so the tick waits for it.
        app.slideshow.shown_at =
            Some(std::time::Instant::now() - std::time::Duration::from_secs(9999));
        app.slideshow_tick(&ctx);
        assert_eq!(
            app.list.shown(),
            back_at,
            "the tick waits for the next image"
        );
        tick(&mut app, &ctx);
        assert_eq!(app.list.shown(), newest, "the next slide after Left");
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// However many times Left goes back, the images kept decoded stay within the slideshow's
    /// bound: the image shown, the one before it, and the plan's length of images shown next.
    /// Before, every image of the history after the one shown was kept decoded, so each press
    /// of Left held one more image, 33 MB at 4K.
    #[test]
    fn left_many_times_keeps_the_images_decoded_within_the_bound() {
        use crate::ui::actions::Action;
        let names: Vec<String> = (0..60).map(|i| format!("{i:02}.png")).collect();
        let d = folder_of(
            "left-bound",
            &names.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        let (mut app, ctx) = app_on(&d);
        app.config.slideshow_delay = 3600;
        app.start_slideshow(&ctx);
        for _ in 0..40 {
            tick(&mut app, &ctx);
        }
        let bound = 2 + (1 + app.decoding.preload_count());
        for press in 0..30 {
            app.move_along(&ctx, Action::Previous);
            let start = std::time::Instant::now();
            while !app.list.decodes_pending().is_empty()
                && start.elapsed() < std::time::Duration::from_secs(5)
            {
                app.receive_decoded(&ctx);
            }
            let (kept, _) = app.kept_places();
            let decoded = app.list.loaded_images().len();
            assert!(
                kept.len() <= bound && decoded <= bound,
                "after {} presses: {} kept, {decoded} decoded, bound {bound}",
                press + 1,
                kept.len()
            );
        }
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// A slideshow goes on, tick after tick, whatever the layout, order and search: the image
    /// shown is another one after every step, a round with Repeat begins the next at once, a
    /// search that hides the image shows its matches only, and without Repeat it stops once
    /// every image shown by the search was shown, and not before.
    #[test]
    fn slideshow_advances_on_every_tick() {
        let names: Vec<String> = (0..7).map(|i| format!("{i}.png")).collect();
        let d = folder_of(
            "slides-ticks",
            &names.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        for layout in ["1x1", "2x2"] {
            for search in ["", "[135]"] {
                for (repeat, random) in [(true, false), (true, true), (false, false), (false, true)]
                {
                    let case = format!("{layout}, {search:?}, repeat {repeat}, random {random}");
                    let (mut app, ctx) = app_on(&d);
                    app.config.layout = layout.into();
                    app.config.slideshow_one_image_at_a_time = String::new();
                    app.config.slideshow_repeat = repeat;
                    app.config.slideshow_random = random;
                    app.config.slideshow_delay = 3600;
                    if !search.is_empty() {
                        app.search.set_query(&format!("re:{search}"));
                        app.update_search(&ctx);
                    }
                    let rows = app.list.matches().map_or(7, <[usize]>::len);
                    app.refill_preload();
                    app.start_slideshow(&ctx);
                    let on_screen = |app: &crate::App| {
                        let mut all = vec![app.list.shown()];
                        all.extend(app.list.panes());
                        all
                    };
                    let mut shown_in_round: HashSet<usize> = on_screen(&app).into_iter().collect();
                    let mut ticks = 0;
                    for _ in 0..20 {
                        let before = on_screen(&app);
                        tick(&mut app, &ctx);
                        if !app.slideshow.is_running() {
                            break;
                        }
                        ticks += 1;
                        // Only when the images on screen are not all the search shows.
                        if rows > before.len() {
                            assert_ne!(on_screen(&app), before, "no advance: {case}");
                            assert!(
                                app.list.shows_file(app.list.shown()),
                                "a match only: {case}"
                            );
                        }
                        shown_in_round.extend(on_screen(&app));
                    }
                    if !repeat {
                        assert!(!app.slideshow.is_running(), "never stopped: {case}");
                        let panes = app.list.panes().len() + 1;
                        assert!(ticks * panes + panes >= rows, "stopped early: {case}");
                        let shown_rows = (shown_in_round.iter())
                            .filter(|&&i| app.list.shows_file(i))
                            .count();
                        assert_eq!(shown_rows, rows, "every match shown: {case}");
                    } else {
                        assert!(app.slideshow.is_running(), "stopped: {case}");
                    }
                }
            }
        }
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// A slideshow in name order goes on from an image shown by hand, a click in the file list,
    /// to the next image after it. A search changed while it runs reaches its plan at once: a
    /// narrower one leaves no image it hides among those planned, and a random slideshow
    /// without Repeat shows every image of a wider one before it stops. Before, the plan was made
    /// again only when the list changed, so the next slide after a click was the one after the
    /// image shown before it, the images planned before a search were still shown, and a random
    /// slideshow stopped after the matches of the search it began with.
    #[test]
    fn the_plan_follows_an_image_shown_by_hand_and_the_search() {
        let names: Vec<String> = (0..10).map(|i| format!("{i}.png")).collect();
        let d = folder_of(
            "slides-plan",
            &names.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        let (mut app, ctx) = app_on(&d);
        app.config.slideshow_random = false;
        app.config.slideshow_repeat = false;
        app.config.slideshow_delay = 3600;
        app.start_slideshow(&ctx);
        tick(&mut app, &ctx);
        assert_eq!(shown(&app), OsString::from("1.png"));
        app.show_file(&ctx, 6);
        tick(&mut app, &ctx);
        assert_eq!(shown(&app), OsString::from("7.png"), "after a click");
        app.search.set_query("re:[0-3]");
        app.update_search(&ctx);
        app.refill_preload();
        let hidden: Vec<usize> = (app.slideshow.planned_places())
            .filter(|&i| !app.list.shows_file(i))
            .collect();
        assert!(hidden.is_empty(), "planned, the search hides {hidden:?}");
        tick(&mut app, &ctx);
        assert_eq!(shown(&app), OsString::from("2.png"), "the next match");
        app.stop_slideshow(&ctx);
        // Random, with a search of two matches, which is cleared after the first slide.
        app.search.set_query("re:[45]");
        app.update_search(&ctx);
        app.show_file(&ctx, 4);
        app.config.slideshow_random = true;
        app.start_slideshow(&ctx);
        tick(&mut app, &ctx);
        app.search.set_query("");
        app.update_search(&ctx);
        let mut seen: HashSet<OsString> = ["4.png", "5.png"].map(OsString::from).into();
        while app.slideshow.is_running() && seen.len() < 20 {
            tick(&mut app, &ctx);
            seen.insert(shown(&app));
        }
        assert_eq!(seen.len(), 10, "every image of the wider search: {seen:?}");
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// A pick costs the same at any size of folder: a slide of a slideshow over 500,000 names
    /// takes no look at each of them, in either order. Only a change of the list makes the one
    /// pass over it. With a search of one match and Repeat, each new round, which finds nothing
    /// to pick but the image on screen, goes through the matches only, not through the whole
    /// list. Before, every such tick went through every name of the list twice.
    #[test]
    fn a_pick_costs_the_same_at_any_size() {
        let mut slideshow = super::Slideshow::default();
        let files: Vec<OsString> = (0..500_000).map(|i| format!("{i:06}.png").into()).collect();
        let inputs = |slideshow: &super::Slideshow, random| super::PlanMadeFrom {
            list: 1,
            search: 0,
            shown_elsewhere: 0,
            random,
            round: slideshow.round_number,
        };
        for random in [false, true] {
            slideshow.forget_names();
            slideshow.make_plan((&files, None), inputs(&slideshow, random), Some(0), |_| {
                true
            });
            let start = std::time::Instant::now();
            for _ in 0..1_000 {
                let shown = slideshow.pick(&files, None).unwrap();
                slideshow.note_shown(&files[shown].clone(), shown);
            }
            let took = start.elapsed();
            assert!(
                took < std::time::Duration::from_millis(50),
                "1,000 picks in {took:?}, random {random}"
            );
            let one_match: &[usize] = &[7];
            let start = std::time::Instant::now();
            for _ in 0..1_000 {
                slideshow.begin_round([&files[7]]);
                let made = super::PlanMadeFrom {
                    search: 1,
                    ..inputs(&slideshow, random)
                };
                slideshow.make_plan((&files, Some(one_match)), made, Some(7), |i| i == 7);
                assert_eq!(slideshow.pick(&files, Some(one_match)), None);
            }
            let took = start.elapsed();
            assert!(
                took < std::time::Duration::from_millis(50),
                "1,000 rounds of one match in {took:?}, random {random}"
            );
        }
    }
}
