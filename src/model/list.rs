//! The model of what is open: the file list with everything that holds a place in it
//! (`List`), and the decoded images (`Slot`).

use crate::images::decode::Hist;
use crate::ui::file_list::RubberBand;
use eframe::egui;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::PathBuf;

/// An image decoded for the view: the shown one or a neighbour read ahead, at the size shown or
/// (`full`) at full resolution.
pub(crate) struct Slot {
    pub(crate) tex: egui::TextureHandle,
    /// The file the texture was made from; decides whether it can be kept when the file list
    /// changes.
    pub(crate) path: PathBuf,
    pub(crate) full: bool,
    /// Original size after EXIF rotation.
    pub(crate) orig: [usize; 2],
    /// Frames of an animated GIF or WebP with their delays in ms; empty for a still image.
    pub(crate) anim: Vec<(egui::TextureHandle, u32)>,
    /// Of the decoded image (the first frame of an animation).
    pub(crate) hist: Box<Hist>,
    /// Pages of a multi-page TIFF; 1 for other files.
    pub(crate) pages: usize,
    /// The box it was decoded into (`decode_box`): decoded again only for a larger one.
    pub(crate) decoded_for: [u32; 2],
}

impl Slot {
    /// Whether any pixel of the (first) frame is less than opaque.
    pub(crate) fn transparent(&self) -> bool {
        self.hist[4][..255].iter().any(|&n| n > 0)
    }

    /// The frame to show `t` after the image was opened; asks for a repaint when the next is due.
    pub(crate) fn frame(
        &self,
        t: std::time::Duration,
        ctx: &egui::Context,
    ) -> &egui::TextureHandle {
        let (k, left) = self.frame_at(t);
        if let Some(left) = left {
            ctx.request_repaint_after(std::time::Duration::from_millis(left as u64));
        }
        self.anim.get(k).map_or(&self.tex, |f| &f.0)
    }

    /// The frame shown `t` after the start, and the milliseconds it still has.
    pub(crate) fn frame_at(&self, t: std::time::Duration) -> (usize, Option<u32>) {
        let total: u32 = self.anim.iter().map(|f| f.1).sum();
        let mut at = (t.as_millis() % total.max(1) as u128) as u32;
        for (k, (_, ms)) in self.anim.iter().enumerate() {
            if at < *ms {
                return (k, Some(ms - at));
            }
            at -= ms;
        }
        (0, None)
    }
}

/// The file list and everything that holds a place in it: made anew when the list is another
/// (`replace_files`), so nothing indexed into one list can outlive it, whatever field comes
/// next. What follows a file from one list to the next (the shown image, the selection, the
/// panes, the decoded slots) is carried over by name there, in the one place.
#[derive(Default)]
pub(crate) struct List {
    /// The images in `dir`, file name only (the full path is built on demand by `path`): saves
    /// memory in large folders, where the path would otherwise be repeated for every file.
    files: Vec<OsString>,
    cur: usize,
    /// Selected files (Ctrl/Shift+click in the file list) that Delete acts on. They are always
    /// files the list shows, so while searching they are matches only. No action then acts on
    /// a file the search hides.
    selected: std::collections::BTreeSet<usize>,
    /// The selected files that the search hides. They are held back from `selected`, so no
    /// action acts on them, and they come back when the search shows them again. A query typed
    /// letter by letter (`re:` before `re:IMG`) would otherwise lose them for good.
    held_back: std::collections::BTreeSet<usize>,
    /// Where a Shift selection started (file index).
    anchor: Option<usize>,
    /// Where the Shift selection made by the keys last ended (file index), while nothing else
    /// changed the selection or the search since: the selection is then that range.
    range_end: Option<usize>,
    /// The images of a layout's panes after the first, which shows `cur`.
    panes: Vec<usize>,
    slots: HashMap<usize, Slot>,
    decode_errors: HashMap<usize, String>,
    /// Decodes asked for, with the number of the job: only its result is taken, so one asked
    /// for before the list, the page, the look or the file changed is dropped; and the box it
    /// was asked for in.
    decodes_pending: HashMap<(usize, bool), (u64, PathBuf, [u32; 2])>,
    matches: Option<Vec<usize>>,
    /// Whether each file is among `matches`, by its place, so the selection can be kept to the
    /// files shown in one step per selected file. Empty when there is no search.
    is_match: Vec<bool>,
    /// Total size of the search matches, summed in the background like `total_size`.
    match_size: u64,
    /// A rectangle being dragged in the icon grid to select.
    rubber_band: Option<RubberBand>,
    /// Images of a collection being dragged in the file list, and the place they would go.
    dragging: Option<(HashSet<usize>, Option<usize>)>,
    last_slide: usize,
    /// The files whose cells pan view draws this frame: their thumbnails are made and kept,
    /// also while the file list shows others.
    pan_thumbs: Vec<usize>,
    /// File list size and date columns: (bytes, mtime) by file name.
    details: HashMap<OsString, (u64, i64)>,
    /// Goes up whenever the file list changes, so what was worked out from its indices can tell
    /// that it is old, also when the list kept its length (another order).
    version: usize,
    /// The entries are the names in a list's file, not yet checked to be files, so no file
    /// operation may start. A new `List` keeps it, because a shuffle or a move in the
    /// collection puts the same names in another order. The whole listing clears it, and so
    /// does another folder opened.
    unchecked: bool,
}

impl List {
    /// The images in `dir`, by file name only.
    pub(crate) fn files(&self) -> &[OsString] {
        &self.files
    }

    /// The place of the shown image in the list.
    pub(crate) fn cur(&self) -> usize {
        self.cur
    }

    /// The selected files, by their places in the list.
    pub(crate) fn selected(&self) -> &std::collections::BTreeSet<usize> {
        &self.selected
    }

    /// The selected files that the search hides, held back until it shows them again.
    pub(crate) fn held_back(&self) -> &std::collections::BTreeSet<usize> {
        &self.held_back
    }

    /// Returns whether every selected file is one the search hides. An action on the
    /// selection then acts on nothing, not on the shown image in their place.
    pub(crate) fn only_held_back(&self) -> bool {
        self.selected.is_empty() && !self.held_back.is_empty()
    }

    /// The selected files and those the search holds back, which a new list selects again by
    /// name and a rectangle drawn with Ctrl adds to.
    pub(crate) fn selected_and_held(&self) -> std::collections::BTreeSet<usize> {
        self.selected.union(&self.held_back).copied().collect()
    }

    /// Where the Shift selection made by the keys last ended.
    pub(crate) fn range_end(&self) -> Option<usize> {
        self.range_end
    }

    /// The images of a layout's panes after the first.
    pub(crate) fn panes(&self) -> &[usize] {
        &self.panes
    }

    /// The decoded images, by their places in the list.
    pub(crate) fn slots(&self) -> &HashMap<usize, Slot> {
        &self.slots
    }

    /// Why images could not be decoded, by their places in the list.
    pub(crate) fn decode_errors(&self) -> &HashMap<usize, String> {
        &self.decode_errors
    }

    /// The decodes asked for and not yet answered.
    pub(crate) fn decodes_pending(&self) -> &HashMap<(usize, bool), (u64, PathBuf, [u32; 2])> {
        &self.decodes_pending
    }

    /// The search matches, by their places in the list. Returns `None` when there is no
    /// search.
    pub(crate) fn matches(&self) -> Option<&[usize]> {
        self.matches.as_deref()
    }

    /// The total size of the search matches, as far as it is summed.
    pub(crate) fn match_size(&self) -> u64 {
        self.match_size
    }

    /// The rectangle being dragged in the icon grid, if one is.
    pub(crate) fn rubber_band(&self) -> Option<&RubberBand> {
        self.rubber_band.as_ref()
    }

    /// The images of a collection being dragged, and the place they would go.
    pub(crate) fn dragging(&self) -> Option<&(HashSet<usize>, Option<usize>)> {
        self.dragging.as_ref()
    }

    /// The image a slideshow of several panes showed last.
    pub(crate) fn last_slide(&self) -> usize {
        self.last_slide
    }

    /// The files whose cells pan view draws this frame.
    pub(crate) fn pan_thumbs(&self) -> &[usize] {
        &self.pan_thumbs
    }

    /// The size and date columns, as (bytes, mtime) by file name.
    pub(crate) fn details(&self) -> &HashMap<OsString, (u64, i64)> {
        &self.details
    }

    /// The list's version, which goes up whenever the list changes.
    pub(crate) fn version(&self) -> usize {
        self.version
    }

    /// A new list of `files`. Nothing of the list before is in it but its version, which goes
    /// up, so what was worked out from the old places can tell that it is old, and whether its
    /// entries are still unchecked.
    pub(crate) fn after(old: &List, files: Vec<OsString>) -> List {
        List {
            files,
            version: old.version + 1,
            unchecked: old.unchecked,
            ..Default::default()
        }
    }

    /// Whether the entries are a list's names not yet checked to be files.
    pub(crate) fn unchecked(&self) -> bool {
        self.unchecked
    }

    /// Marks the entries as a list's names not yet checked to be files, or as checked.
    pub(crate) fn set_unchecked(&mut self, unchecked: bool) {
        self.unchecked = unchecked;
    }

    /// Shows the image at place `i`.
    pub(crate) fn set_cur(&mut self, i: usize) {
        self.cur = i;
    }

    /// Gives the file at place `i` the name `name`, because it was renamed or moved within the
    /// list.
    pub(crate) fn rename(&mut self, i: usize, name: OsString) {
        if let Some(file) = self.files.get_mut(i) {
            *file = name;
        }
    }

    /// Marks the list as another, because names in it changed in place. What was made for the
    /// old names (the matches, Pan view's days) is then made again.
    pub(crate) fn renamed_in_place(&mut self) {
        self.version += 1;
    }

    /// Forgets the files, because another folder opens, where a file of the same name is
    /// another image.
    pub(crate) fn forget_files(&mut self) {
        self.files.clear();
    }

    /// Takes the files at the sorted places `removed` out of the list in one pass. The decoded
    /// images and the decode errors of the others move to their new places, and the panes are
    /// cleared, so `refill` fills them again from the list as it is.
    pub(crate) fn remove(&mut self, removed: &[usize]) {
        let mut idx = 0;
        self.files.retain(|_| {
            idx += 1;
            removed.binary_search(&(idx - 1)).is_err()
        });
        self.slots = without_indices(std::mem::take(&mut self.slots), removed);
        self.decode_errors = without_indices(std::mem::take(&mut self.decode_errors), removed);
        self.panes.clear();
    }

    /// Lets go of everything that holds a place in the list, because files were taken out of
    /// it and its places shifted: the selection, the anchor, a rectangle or a collection drag,
    /// and the decodes in flight. The version goes up.
    pub(crate) fn places_changed(&mut self) {
        self.version += 1;
        self.selected.clear();
        self.held_back.clear();
        self.anchor = None;
        self.rubber_band = None;
        self.dragging = None;
        self.decodes_pending.clear();
    }

    /// Returns whether the list shows the file at `i`. While searching, that is whether the
    /// file is a match.
    fn shows(&self, i: usize) -> bool {
        self.matches.is_none() || self.is_match.get(i).copied().unwrap_or(false)
    }

    /// Selects the files at `places` and nothing else. Those that the search hides are held
    /// back until it shows them.
    pub(crate) fn select(&mut self, places: std::collections::BTreeSet<usize>) {
        (self.selected, self.held_back) = places.into_iter().partition(|&i| self.shows(i));
    }

    /// Selects the file at `i` alone, and starts a Shift selection there.
    pub(crate) fn select_one(&mut self, i: usize) {
        self.clear_selection();
        self.set_selected(i, true);
        self.anchor = Some(i);
    }

    /// A right-click on the file at `i`. It selects that file alone when it is outside the
    /// selection, as in a file manager, so the menu acts on it. The shown image counts as
    /// selected only while nothing else is, also among the files the search hides. Returns
    /// whether the selection changed.
    pub(crate) fn right_click(&mut self, i: usize) -> bool {
        let others = !self.selected.is_empty() || !self.held_back.is_empty();
        let outside = !self.selected.contains(&i) && (i != self.cur || others);
        if outside {
            self.select_one(i);
        }
        outside
    }

    /// Selects the file at `i`, or takes it out of the selection when it was in it.
    pub(crate) fn toggle_selected(&mut self, i: usize) {
        if !self.selected.remove(&i) {
            self.set_selected(i, true);
        }
    }

    /// Selects the file at `i` too when `on` is true and the list shows it, and takes it out of
    /// the selection otherwise.
    pub(crate) fn set_selected(&mut self, i: usize, on: bool) {
        match on && self.shows(i) {
            true => self.selected.insert(i),
            false => self.selected.remove(&i),
        };
    }

    /// Adds those of the files at `places` that the list shows to the selection.
    pub(crate) fn add_to_selection(&mut self, places: impl IntoIterator<Item = usize>) {
        for i in places {
            self.set_selected(i, true);
        }
    }

    /// Selects nothing, also among the files the search hides.
    pub(crate) fn clear_selection(&mut self) {
        self.selected.clear();
        self.held_back.clear();
    }

    /// Starts a Shift selection at `i`, or at nothing.
    pub(crate) fn set_anchor(&mut self, i: Option<usize>) {
        self.anchor = i;
    }

    /// Where a Shift selection started. When none has, it starts at the shown image.
    pub(crate) fn anchor_or_cur(&mut self) -> usize {
        *self.anchor.get_or_insert(self.cur)
    }

    /// Notes where the Shift selection made by the keys ended, or that the selection is no
    /// longer such a range.
    pub(crate) fn set_range_end(&mut self, i: Option<usize>) {
        self.range_end = i;
    }

    /// Sets the images of a layout's panes after the first.
    pub(crate) fn set_panes(&mut self, panes: Vec<usize>) {
        self.panes = panes;
    }

    /// Notes the image a slideshow of several panes showed last.
    pub(crate) fn set_last_slide(&mut self, i: usize) {
        self.last_slide = i;
    }

    /// Shows the image at place `i` in pane `k` after the first.
    pub(crate) fn set_pane(&mut self, k: usize, i: usize) {
        self.panes[k] = i;
    }

    /// Forgets the panes' images, so they are filled again from the list as it is.
    pub(crate) fn clear_panes(&mut self) {
        self.panes.clear();
    }

    /// Takes the decoded images, by their places, to carry them into a new list by name.
    pub(crate) fn take_slots(&mut self) -> HashMap<usize, Slot> {
        std::mem::take(&mut self.slots)
    }

    /// Keeps the decoded image `slot` at place `i`.
    pub(crate) fn keep_slot(&mut self, i: usize, slot: Slot) {
        self.slots.insert(i, slot);
    }

    /// Lets go of the decoded image at place `i`.
    pub(crate) fn forget_slot(&mut self, i: usize) {
        self.slots.remove(&i);
    }

    /// Keeps only the decoded images for which `keep` says so.
    pub(crate) fn keep_slots(&mut self, mut keep: impl FnMut(&usize, &Slot) -> bool) {
        self.slots.retain(|i, s| keep(i, s));
    }

    /// Keeps only the images decoded at full resolution, and the decodes at full resolution
    /// asked for, so the others are decoded again to a new size.
    pub(crate) fn keep_full_only(&mut self) {
        self.slots.retain(|_, s| s.full);
        self.decodes_pending.retain(|&(_, full), _| full);
    }

    /// Lets go of every decoded image, every decode asked for and every decode error, so all
    /// are decoded again.
    pub(crate) fn forget_decoded(&mut self) {
        self.slots.clear();
        self.decodes_pending.clear();
        self.decode_errors.clear();
    }

    /// Notes that decode `key` was asked for, as `job`.
    pub(crate) fn ask_decode(&mut self, key: (usize, bool), job: (u64, PathBuf, [u32; 2])) {
        self.decodes_pending.insert(key, job);
    }

    /// Takes decode `key` as answered. Returns the job it was asked as, or `None` when it is no
    /// longer asked for.
    pub(crate) fn take_decode(&mut self, key: (usize, bool)) -> Option<(u64, PathBuf, [u32; 2])> {
        self.decodes_pending.remove(&key)
    }

    /// Forgets the decodes asked for of the image at place `i`.
    pub(crate) fn forget_decodes_of(&mut self, i: usize) {
        self.decodes_pending.retain(|p, _| p.0 != i);
    }

    /// Notes why the image at place `i` could not be decoded.
    pub(crate) fn decode_failed(&mut self, i: usize, why: String) {
        self.decode_errors.insert(i, why);
    }

    /// Forgets why the image at place `i` could not be decoded, so it is tried again.
    pub(crate) fn forget_decode_error(&mut self, i: usize) {
        self.decode_errors.remove(&i);
    }

    /// Sets the search matches, or `None` when there is no search. The selected files that the
    /// search hides are held back, and those it shows again are selected again. Returns whether
    /// the selection changed.
    pub(crate) fn set_matches(&mut self, matches: Option<Vec<usize>>) -> bool {
        self.is_match.clear();
        if let Some(m) = &matches {
            self.is_match.resize(self.files.len(), false);
            for &i in m {
                self.is_match[i] = true;
            }
        }
        self.matches = matches;
        // One step per selected file, not per file, as this runs at every letter typed.
        let hidden: Vec<usize> = (self.selected.iter().copied())
            .filter(|&i| !self.shows(i))
            .collect();
        let shown: Vec<usize> = (self.held_back.iter().copied())
            .filter(|&i| self.shows(i))
            .collect();
        for &i in &hidden {
            self.selected.remove(&i);
            self.held_back.insert(i);
        }
        for &i in &shown {
            self.held_back.remove(&i);
            self.selected.insert(i);
        }
        !hidden.is_empty() || !shown.is_empty()
    }

    /// Sets the total size of the search matches, as far as it is summed.
    pub(crate) fn set_match_size(&mut self, size: u64) {
        self.match_size = size;
    }

    /// Starts dragging a rectangle in the icon grid.
    pub(crate) fn start_rubber_band(&mut self, band: RubberBand) {
        self.rubber_band = Some(band);
    }

    /// Takes the rectangle being dragged, to change it as the pointer moves. It ends unless it
    /// is given back with `start_rubber_band`.
    pub(crate) fn take_rubber_band(&mut self) -> Option<RubberBand> {
        self.rubber_band.take()
    }

    /// Starts dragging the images of a collection at `picked`.
    pub(crate) fn start_drag(&mut self, picked: HashSet<usize>) {
        self.dragging = Some((picked, None));
    }

    /// The place the dragged images would go, to change it as the pointer moves. Returns
    /// `None` when nothing is dragged.
    pub(crate) fn drag_target_mut(&mut self) -> Option<&mut Option<usize>> {
        self.dragging.as_mut().map(|(_, to)| to)
    }

    /// Ends the drag, and returns the images dragged and where they would go. Returns `None`
    /// when nothing is dragged.
    pub(crate) fn take_drag(&mut self) -> Option<(HashSet<usize>, Option<usize>)> {
        self.dragging.take()
    }

    /// Takes the files whose cells pan view drew, so it can draw them again.
    pub(crate) fn take_pan_thumbs(&mut self) -> Vec<usize> {
        std::mem::take(&mut self.pan_thumbs)
    }

    /// Notes that pan view draws the cell of the file at place `i` this frame.
    pub(crate) fn pan_thumb(&mut self, i: usize) {
        self.pan_thumbs.push(i);
    }

    /// Keeps the size and date `d` of the file `name` for the list's columns.
    pub(crate) fn remember_details(&mut self, name: OsString, d: (u64, i64)) {
        self.details.insert(name, d);
    }

    /// Forgets the size and date of the file `name`, because it changed.
    pub(crate) fn forget_details_of(&mut self, name: &std::ffi::OsStr) {
        self.details.remove(name);
    }

    /// Forgets every file's size and date, so the rows on screen read them again.
    pub(crate) fn forget_details(&mut self) {
        self.details.clear();
    }
}

/// For tests of other modules, which set up a list without reading a folder.
#[cfg(test)]
impl List {
    /// Makes `files` the list.
    pub(crate) fn set_files(&mut self, files: Vec<OsString>) {
        self.files = files;
    }
}

/// A file of the list and its row among the search matches (`None`: not among them).
pub(crate) type FileRow = (usize, Option<usize>);

/// `map` with the entries at the sorted places `removed` left out, and the others moved down to
/// their places once those are gone.
fn without_indices<T>(map: HashMap<usize, T>, removed: &[usize]) -> HashMap<usize, T> {
    map.into_iter()
        .filter_map(|(i, v)| Some((i - removed.binary_search(&i).err()?, v)))
        .collect()
}

#[cfg(test)]
mod tests {
    /// Every field of `List` says what becomes of it when a new list replaces the old one
    /// (`List::after` and `App::replace_files`). A slideshow's last slide was dropped, because
    /// nobody chose for it. A new field fails here until it is carried by name or its reset is
    /// meant, and written in this table.
    #[test]
    fn every_field_is_carried_or_reset_on_a_new_list() {
        const FATE: &[(&str, &str)] = &[
            ("files", "the new list"),
            ("cur", "carried by name: the file shown"),
            (
                "selected",
                "carried by name by the callers that keep it (reselect)",
            ),
            (
                "held_back",
                "carried by name with the selection (selected_and_held)",
            ),
            ("anchor", "reset"),
            ("range_end", "reset"),
            ("panes", "carried by name"),
            ("slots", "carried by name"),
            ("decode_errors", "reset: decoded again"),
            (
                "decodes_pending",
                "the shown file's carried, the others reset",
            ),
            ("matches", "worked out again: update_search"),
            ("is_match", "worked out again with the matches"),
            ("match_size", "worked out again: update_search"),
            ("rubber_band", "reset"),
            ("dragging", "reset"),
            ("last_slide", "carried by name"),
            (
                "pan_thumbs",
                "reset: pan view names them again in its next frame",
            ),
            ("details", "reset: read again as the rows are drawn"),
            ("version", "goes up in List::after"),
            ("unchecked", "carried by List::after"),
        ];
        let sources = crate::support::source_rules::sources();
        let text = &sources
            .iter()
            .find(|(n, _)| n == "model/list.rs")
            .unwrap()
            .1;
        let body = text.split("pub(crate) struct List {").nth(1).unwrap();
        let body = body.split("\n}").next().unwrap();
        let fields: Vec<&str> = (body.lines())
            .map(str::trim)
            .filter(|l| !l.starts_with("//") && l.contains(':'))
            .map(|l| l.split(':').next().unwrap())
            .collect();
        let named: Vec<&str> = FATE.iter().map(|f| f.0).collect();
        assert_eq!(fields, named);
    }

    #[test]
    fn without_indices_shifts_down() {
        let map = std::collections::HashMap::from([(1, 'a'), (2, 'b'), (4, 'c'), (6, 'd')]);
        let expected = std::collections::HashMap::from([(1, 'a'), (3, 'd')]);
        assert_eq!(super::without_indices(map, &[2, 4, 5]), expected);
    }
}
