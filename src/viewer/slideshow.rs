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
    /// every name the list holds is among them. A hash costs 8 bytes a name, where a copy of
    /// the names would cost about 50.
    round: HashSet<u64>,
    /// The images shown, the oldest first, at most `HISTORY` of them.
    history: VecDeque<PlacedName>,
    /// How many steps Left has gone back from the newest image of `history`. The next slide
    /// and Right go forward through the history again before they pick a new image.
    back: usize,
    /// The images picked to come next, so that they are decoded before their slide. They are
    /// picked again when R changes the order, or when one of them leaves the list.
    planned: VecDeque<PlacedName>,
    /// In random order, the places of the images that this round has not shown or planned. A
    /// pick takes one at random and swaps the last one into its place, so each pick costs the
    /// same at any size of folder. An image shown meanwhile in another way is passed over when
    /// it is drawn.
    pool: Vec<usize>,
    /// The list's version that the places in `history`, `planned` and `pool` are of, or `None`
    /// when they are to be found again.
    places_of: Option<usize>,
    /// Whether a pick found no image since the list, the round or the order last changed, so
    /// that the next pick does not look through the list again.
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

    /// Forgets the round, the history and the images planned, because another folder opens,
    /// where the same names are other images.
    pub(crate) fn forget_names(&mut self) {
        self.begin_round([]);
        self.history.clear();
        self.back = 0;
    }

    /// Forgets the images planned and the pool, so that the next pick picks them in the order
    /// as it is now. The round and the history stay.
    fn forget_order(&mut self) {
        self.planned.clear();
        self.pool.clear();
        self.places_of = None;
        self.found_none = false;
    }

    /// Returns whether this round has shown the image named `name`.
    #[cfg(test)]
    pub(crate) fn has_shown(&self, name: &OsString) -> bool {
        self.round.contains(&name_hash(name.as_encoded_bytes()))
    }

    /// Begins a new round, in which only the images `on_screen` count as shown. This is the one
    /// place a round begins (`source_rules::a_round_begins_in_one_place`): at the start, when
    /// every image listed was shown and Repeat is on, and at Home.
    fn begin_round<'a>(&mut self, on_screen: impl IntoIterator<Item = &'a OsString>) {
        self.round.clear();
        for name in on_screen {
            self.round.insert(name_hash(name.as_encoded_bytes()));
        }
        self.forget_order();
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

    /// Gives the names that were renamed in place, `renames` as (old, new), their new names
    /// in the round, the history and the images planned, so that an image renamed is not shown
    /// again in its round.
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
        }
    }

    /// Finds the places of the names it keeps again in `files`, the list of version `version`,
    /// and in random order makes the pool anew of the images that `shows` and that this round
    /// has not shown or planned. A planned image that is gone has the images planned picked
    /// again. It is one pass over the list, once for each version, which the change of the
    /// list itself makes (`App::follow_list_in_slideshow`), so that no slide pays for it.
    fn find_places(
        &mut self,
        files: &[OsString],
        version: usize,
        random: bool,
        shows: impl Fn(usize) -> bool,
    ) {
        if self.places_of == Some(version) {
            return;
        }
        self.places_of = Some(version);
        self.found_none = false;
        let mut found: HashMap<u64, Option<usize>> = (self.history.iter())
            .chain(&self.planned)
            .map(|kept| (name_hash(kept.name.as_encoded_bytes()), None))
            .collect();
        self.pool.clear();
        for (i, name) in files.iter().enumerate() {
            let hash = name_hash(name.as_encoded_bytes());
            if let Some(place @ None) = found.get_mut(&hash) {
                *place = Some(i);
            }
            if random && shows(i) && !self.round.contains(&hash) {
                self.pool.push(i);
            }
        }
        for kept in self.history.iter_mut().chain(self.planned.iter_mut()) {
            let hash = name_hash(kept.name.as_encoded_bytes());
            let place = found.get(&hash).copied().flatten();
            kept.place = place.filter(|&i| files[i] == kept.name);
        }
        if self.planned.iter().any(|kept| kept.place.is_none()) {
            // The pool holds every image not shown, so those planned are picked anew from it.
            self.planned.clear();
        } else {
            let planned: HashSet<usize> = self.planned.iter().filter_map(|p| p.place).collect();
            self.pool.retain(|i| !planned.contains(i));
        }
    }

    /// Returns the place of the next image to plan in `files`, among those that `shows`: in
    /// random order one drawn from the pool, and otherwise the next one in the list that this
    /// round has not shown or planned after the last one planned, or else after the image
    /// `shown` (the newest of the history when Left went back), going on from the start of the
    /// list after its end. Returns `None` when there is none, and then looks no
    /// more until the list, the round or the order changes. Each pick costs the same at any size
    /// of folder, but for the images an order by name passes over, which it passes over once in
    /// a round.
    // shortcut: a search that hides most of a large list has each pick in name order look past
    // the names it hides; keep the rows' places in order if such searches play slideshows.
    fn pick(
        &mut self,
        files: &[OsString],
        random: bool,
        shown: usize,
        shows: impl Fn(usize) -> bool,
    ) -> Option<usize> {
        if self.found_none || files.is_empty() {
            return None;
        }
        let planned = |slideshow: &Slideshow, i: usize| {
            (slideshow.planned.iter()).any(|kept| kept.place == Some(i))
        };
        let unshown = |slideshow: &Slideshow, i: usize| {
            shows(i)
                && !slideshow
                    .round
                    .contains(&name_hash(files[i].as_encoded_bytes()))
                && !planned(slideshow, i)
        };
        let picked = match random {
            true => loop {
                if self.pool.is_empty() {
                    break None;
                }
                let drawn = (self.random() % self.pool.len() as u64) as usize;
                let i = self.pool.swap_remove(drawn);
                if i < files.len() && unshown(self, i) {
                    break Some(i);
                }
            },
            false => {
                let newest = match self.back {
                    0 => Some(shown),
                    _ => self.history.back().and_then(|kept| kept.place),
                };
                let from = (self.planned.back().and_then(|kept| kept.place))
                    .or(newest)
                    .unwrap_or(files.len() - 1)
                    .min(files.len() - 1);
                let n = files.len();
                (1..=n).map(|d| (from + d) % n).find(|&i| unshown(self, i))
            }
        };
        self.found_none = picked.is_none();
        picked
    }

    /// Plans images to come next in `files`, after the image `shown`, until `length` are
    /// planned or none is left.
    fn plan_next(
        &mut self,
        files: &[OsString],
        random: bool,
        (shown, length): (usize, usize),
        shows: impl Fn(usize) -> bool + Copy,
    ) {
        while self.planned.len() < length {
            let Some(i) = self.pick(files, random, shown, shows) else {
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
    /// the images the list no longer holds. Returns the place of the image to show, or `None`
    /// when the history has no image there.
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
        }
        if self.config.slideshow_subfolders {
            // The images of the subfolders too, listed in the background. The images already
            // listed play at once, while the whole tree is read again, which on a share can take
            // minutes, and the tree's images join the list in parts as they are read.
            self.folder.set_recursive(true);
            self.start_listing(ctx);
        } else if self.folder.reads_tree_in_parts() && !self.folder.parts_are_new_names() {
            // A tree still read for the folder opened sends the whole tree so far in each part.
            // It is read again as the slideshow's, whose parts hold only the images new to it.
            self.start_listing(ctx);
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
        let shown = self.list.files().get(self.list.shown());
        let in_the_folder = shown.is_some_and(|name| !name.as_encoded_bytes().contains(&b'/'));
        if self.folder.recursive() != self.config.show_subfolders
            && (self.config.show_subfolders || in_the_folder || shown.is_none())
        {
            self.folder.set_recursive(self.config.show_subfolders);
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

    /// Finds the places of what the slideshow keeps in the list again, after the list changed,
    /// and plans the images to come next. Each change of the list calls it (`replace_files`,
    /// `remove_indices`, `rename_in_list`), so that the one pass over the list it makes is paid
    /// by the change, and a slide costs the same at any size of folder.
    pub(crate) fn follow_list_in_slideshow(&mut self) {
        if !self.slideshow.is_running() {
            return;
        }
        let n = self.list.panes().len() + 1;
        let ahead = self.decoding.preload_count();
        let random = self.config.slideshow_random;
        let list = &self.list;
        let shows = |i| list.shows_file(i);
        let slideshow = &mut self.slideshow;
        slideshow.find_places(list.files(), list.version(), random, shows);
        let (shown, length) = (list.shown(), n * (1 + ahead));
        slideshow.plan_next(list.files(), random, (shown, length), shows);
    }

    /// Switch image when the time is up. Waits for a planned image that is still decoding, so
    /// the slideshow never shows a black screen.
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
        let next = self.slideshow.planned_places().next();
        let pending = next.is_some_and(|i| self.list.decodes_pending().contains_key(&(i, false)));
        if pending && self.slideshow.back == 0 {
            return;
        }
        self.next_slide(ctx);
    }

    /// Shows the next slide: the image after the one shown in the history when Left went back,
    /// and otherwise the next images planned. When every image listed has been shown, Repeat
    /// begins a new round at once and the slideshow stops without it. It never waits for the
    /// listing of a tree, whose images join the round as they come.
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
    /// Returns whether the slideshow took the key.
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
                let name = self.list.files()[to].clone();
                self.slideshow.note_shown(&name, to);
                self.slideshow.forget_order();
                self.follow_list_in_slideshow();
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
        if self.slideshow.is_running() {
            self.slideshow.forget_order();
            self.follow_list_in_slideshow();
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
        app.config.show_subfolders = true;
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
        assert!(
            !app.slideshow.has_shown(&app.list.files()[5].clone()) || seen.contains(&shown(&app)),
            "Home begins a new round"
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

    /// A pick costs the same at any size of folder: a slide of a slideshow over 500,000 names
    /// takes no look at each of them, in either order. Only a change of the list makes the one
    /// pass over it.
    #[test]
    fn a_pick_costs_the_same_at_any_size() {
        let mut slideshow = super::Slideshow::default();
        let files: Vec<OsString> = (0..500_000).map(|i| format!("{i:06}.png").into()).collect();
        for random in [false, true] {
            slideshow.forget_names();
            slideshow.find_places(&files, 1, random, |_| true);
            let (start, mut shown) = (std::time::Instant::now(), 0);
            for _ in 0..1_000 {
                shown = slideshow.pick(&files, random, shown, |_| true).unwrap();
                slideshow.note_shown(&files[shown].clone(), shown);
            }
            let took = start.elapsed();
            assert!(
                took < std::time::Duration::from_millis(50),
                "1,000 picks in {took:?}, random {random}"
            );
        }
    }
}
