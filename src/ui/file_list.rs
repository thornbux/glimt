//! The side panel. It holds the folder list, the file list or icon grid with its thumbnails,
//! dragging and the rubber band, and the status bar under it.

use crate::files::config::{sort_folders_from, sort_folders_path};
use crate::files::external::is_video;
use crate::model::folder::drain;
use crate::model::listing::{SORTS, Types, list_folder};
use crate::support::format::{count, local_time, middle_cut, size_text, thousands};
use crate::support::lang::tr;
use crate::support::threads::{Receiving, Took};
use crate::ui::actions::Action;
use crate::ui::dialogs::{Pick, entered};
use crate::ui::menus::{checkbox, radio_choice, scroll_menu, selectable, submenu};
use crate::ui::workers::spawn_thumbnailers;
use crate::viewer::view::{broken_mark, pane_map};
use crate::{files::config, model::collections, tf, ui::theme};
use eframe::egui;
use std::collections::HashMap;
use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Thumbnail box in the file list, in points (Geeqie's default 96×72).
pub(crate) const THUMBS: [egui::Vec2; 3] = [
    egui::vec2(96.0, 72.0),
    egui::vec2(128.0, 96.0),
    egui::vec2(192.0, 144.0),
];

/// What a cell of the thumbnail grid adds to the thumbnail, which has its name below.
pub(crate) const CELL_EXTRA: egui::Vec2 = egui::vec2(8.0, 26.0);

/// What a cell adds to the thumbnail without the name: the same margin all round.
const CELL_EXTRA_NO_NAME: egui::Vec2 = egui::vec2(8.0, 8.0);

/// Whether thumbnails come from the freedesktop "large" cache, for big ones or a scaled
/// screen. The workers read it.
pub(crate) static LARGE_THUMBS: AtomicBool = AtomicBool::new(false);

/// Thumbnail jobs further than this from the visible rows are skipped.
pub(crate) const THUMB_MARGIN: usize = 20;

/// Width of the size and date columns in the file list (points).
pub(crate) const DATE_WIDTH: f32 = 140.0;

pub(crate) const DETAILS_WIDTH: f32 = DATE_WIDTH + 90.0;

/// Thumbnails kept in memory (≈ 64 kB each) before those far from view are dropped.
pub(crate) const MAX_THUMBS: usize = 600;

/// A rectangle being dragged in the icon grid to select.
pub(crate) struct RubberBand {
    /// Where it started, from the top left of the grid's content, so it scrolls along.
    start: egui::Vec2,
    /// The selection before it, kept with Ctrl or Shift.
    before: std::collections::BTreeSet<usize>,
    /// Whether the pointer has moved far enough to be a rectangle and not a click.
    moved: bool,
    /// The rows and columns it touched when the selection was last made from it.
    touched: Option<[usize; 4]>,
}

/// The files whose thumbnails are on screen, for the thumbnail workers to make first. `[0]` is
/// for the file list, and `[1]` for pan view.
pub(crate) type ThumbView = Arc<Mutex<[Vec<usize>; 2]>>;

/// The thumbnails for the file list. It holds those made, those being made, and the workers
/// that make them.
pub(crate) struct Thumbs {
    /// The file list's thumbnails by file name. `None` means it could not be made.
    textures: HashMap<OsString, Option<egui::TextureHandle>>,
    /// The thumbnails being made, by file name, with the number of the last request. An answer
    /// to an earlier request, made before the file changed or a setting changed the look, is
    /// dropped, also when it comes after the later one's: the workers run side by side.
    pending: HashMap<OsString, u64>,
    /// The number of the last request made.
    asked: u64,
    /// (image index, file, request number)
    jobs: mpsc::Sender<(usize, PathBuf, u64)>,
    /// The answers as (file, request number, thumbnail). The outer `None` means it was skipped
    /// because it scrolled out of view.
    rx: mpsc::Receiver<(PathBuf, u64, Option<Option<egui::ColorImage>>)>,
    /// The files the file list (or film strip) shows, with some rows around them, and those pan
    /// view shows, both sorted. A thumbnail job for another file is skipped. It holds files
    /// rather than a range, because the matches of a search, or pan view's days, are spread
    /// over the whole folder.
    view: ThumbView,
    /// The rows the file list showed last, in the list as shown. Thumbnails near them are kept.
    list_rows: std::ops::Range<usize>,
    /// Workers write new thumbnails to the shared cache (Preferences).
    save: Arc<AtomicBool>,
}

impl Thumbs {
    /// Thumbnails with workers of their own, so scrolling the list never delays the shown
    /// image. `rotate` is the decoders' Auto Rotate.
    pub(crate) fn new(
        ctx: &egui::Context,
        workers: usize,
        rotate: &Arc<AtomicBool>,
        save: bool,
    ) -> Self {
        let view: ThumbView = Arc::default();
        let save = Arc::new(AtomicBool::new(save));
        let (jobs, rx) = spawn_thumbnailers(ctx, workers, rotate, &view, &save);
        Thumbs {
            textures: HashMap::new(),
            pending: HashMap::new(),
            asked: 0,
            jobs,
            rx,
            view,
            list_rows: 0..0,
            save,
        }
    }

    /// The thumbnail of `name`, if one was made.
    pub(crate) fn made(&self, name: &OsStr) -> Option<&egui::TextureHandle> {
        self.textures.get(name)?.as_ref()
    }

    /// Forgets the thumbnail of `name`, made or being made, so it is made again. The answer of
    /// one being made is dropped when it comes, because it shows the file's old contents.
    pub(crate) fn forget(&mut self, name: &OsStr) {
        self.textures.remove(name);
        self.pending.remove(name);
    }

    /// Forgets every thumbnail, as another folder opens.
    pub(crate) fn forget_all(&mut self) {
        self.textures.clear();
        self.pending.clear();
    }

    /// Asks the workers for the thumbnail of file `i` at `path`, named `name` in the list,
    /// unless it is being made already.
    fn ask(&mut self, name: &OsStr, i: usize, path: PathBuf) {
        if !self.pending.contains_key(name) {
            self.asked += 1;
            self.pending.insert(name.to_owned(), self.asked);
            let _ = self.jobs.send((i, path, self.asked));
        }
    }

    /// Returns whether the answer to request `n` for `name` is the one to show, and if so takes
    /// the request as answered.
    fn answered(&mut self, name: &OsStr, n: u64) -> bool {
        let last = self.pending.get(name) == Some(&n);
        if last {
            self.pending.remove(name);
        }
        last
    }

    /// Whether the workers write new thumbnails to the shared cache.
    pub(crate) fn save_to_cache(&self, save: bool) {
        self.save.store(save, Ordering::Relaxed);
    }

    /// The files pan view shows, sorted, for the workers to make.
    pub(crate) fn pan_view_shows(&self, files: Vec<usize>) {
        self.view.lock().unwrap()[1] = files;
    }

    /// The rows the file list showed last.
    pub(crate) fn list_rows(&self) -> std::ops::Range<usize> {
        self.list_rows.clone()
    }
}

/// The file list in the side panel. It knows whether it is hidden, the shape of the icon grid,
/// and whether it scrolls to the shown image.
pub(crate) struct Panel {
    /// View → Hide File List. It shows only the image, as in full screen but in a window.
    hidden: bool,
    /// The columns of the icon grid in the last frame. It is 1 when the list is rows or hidden.
    grid_cols: usize,
    /// How far Previous and Next move this frame. It is a row of the icon grid for ↑ and ↓, and
    /// otherwise 1.
    row_step: isize,
    /// Scroll the file list to the current image on the next frame.
    scroll_to_shown: bool,
    /// The scroll asked for before this frame, which every list drawn in it makes: the side
    /// panel and the grid in the image area both, and neither while the list is hidden.
    scroll_now: bool,
    /// View → Thumbnails in Image Area (G): whether the image area shows the files as a grid.
    area_grid: AreaGrid,
}

/// What the image area shows with View → Thumbnails in Image Area.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
enum AreaGrid {
    /// The image, as without the grid.
    #[default]
    Off,
    /// The grid of thumbnails.
    Shown,
    /// An image opened from the grid with Enter or a double-click. Esc goes back to the grid.
    Opened,
}

impl Default for Panel {
    fn default() -> Self {
        Panel {
            hidden: false,
            grid_cols: 1,
            row_step: 1,
            scroll_to_shown: true,
            scroll_now: false,
            area_grid: AreaGrid::Off,
        }
    }
}

impl Panel {
    /// View → Hide File List, or showing it again.
    pub(crate) fn toggle_hidden(&mut self) {
        self.hidden = !self.hidden;
    }

    /// The file list shows again, as for a search.
    pub(crate) fn show(&mut self) {
        self.hidden = false;
    }

    pub(crate) fn is_hidden(&self) -> bool {
        self.hidden
    }

    /// The file list scrolls to the shown image on the next frame.
    pub(crate) fn scroll_to_shown(&mut self) {
        self.scroll_to_shown = true;
    }

    /// One column until the icon grid, drawn later in the frame, says how many it has. The
    /// scroll asked for so far is made by the lists this frame draws, and then it is done.
    /// When the frame draws no list (`lists` is false in full screen or with the file list
    /// hidden), the scroll waits for the first frame that draws one. The list then shows the
    /// image shown when it comes back.
    pub(crate) fn start_frame(&mut self, lists: bool) {
        self.grid_cols = 1;
        self.scroll_now = lists && std::mem::take(&mut self.scroll_to_shown);
    }

    /// Sets how far Previous and Next move, a row of the icon grid for ↑ and ↓, and otherwise
    /// one image.
    pub(crate) fn set_row_step(&mut self, vertical: bool) {
        self.row_step = if vertical { self.grid_cols as isize } else { 1 };
    }

    /// Whether the lists drawn now scroll to the shown image. A scroll asked for while this
    /// frame draws is made at once, and again in the next frame for the lists drawn before it
    /// was asked.
    fn scrolls(&self) -> bool {
        self.scroll_now || self.scroll_to_shown
    }

    pub(crate) fn row_step(&self) -> isize {
        self.row_step
    }

    /// View → Thumbnails in Image Area. It shows the grid, and hides it again, but from an image
    /// opened from the grid it goes back to the grid, as Esc does.
    pub(crate) fn toggle_area_grid(&mut self) {
        self.area_grid = match self.area_grid {
            AreaGrid::Shown => AreaGrid::Off,
            AreaGrid::Off | AreaGrid::Opened => AreaGrid::Shown,
        };
        self.scroll_to_shown = true;
    }

    /// Whether the image area shows the grid of thumbnails instead of the image.
    pub(crate) fn area_grid_shown(&self) -> bool {
        self.area_grid == AreaGrid::Shown
    }

    /// Whether the grid is on, shown or behind an image opened from it.
    pub(crate) fn area_grid_on(&self) -> bool {
        self.area_grid != AreaGrid::Off
    }

    /// Shows the image in place of the grid, after Enter or a double-click in it.
    pub(crate) fn open_from_grid(&mut self) {
        if self.area_grid == AreaGrid::Shown {
            self.area_grid = AreaGrid::Opened;
        }
    }

    /// Goes back to the grid from an image opened from it, as Esc does. Returns whether it did.
    pub(crate) fn back_to_grid(&mut self) -> bool {
        let opened = self.area_grid == AreaGrid::Opened;
        if opened {
            self.area_grid = AreaGrid::Shown;
            self.scroll_to_shown = true;
        }
        opened
    }
}

/// The folder tree. It holds the subfolders read so far, the folders opened in it and the one
/// shown.
pub(crate) struct Tree {
    /// The subfolders of each folder for the folder tree. It is `None` while they are read.
    children: HashMap<PathBuf, Option<Vec<PathBuf>>>,
    tx: mpsc::Sender<(PathBuf, Vec<PathBuf>)>,
    rx: mpsc::Receiver<(PathBuf, Vec<PathBuf>)>,
    /// The folder the tree was last opened up to. The tree opens again when the folder changes.
    shown: PathBuf,
    /// The folders open in the tree.
    open: HashSet<PathBuf>,
}

impl Default for Tree {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Tree {
            children: HashMap::new(),
            tx,
            rx,
            shown: PathBuf::new(),
            open: HashSet::new(),
        }
    }
}

impl Tree {
    /// Every folder is read again when the tree shows it, as after a folder came back from the
    /// trash.
    pub(crate) fn read_again(&mut self) {
        self.children.clear();
    }

    /// Every folder is read again, and the tree opens up to the open folder once more.
    pub(crate) fn read_again_and_reopen(&mut self) {
        self.children.clear();
        self.shown = PathBuf::new();
    }

    /// The subfolders of `dir`, read with the folder's listing.
    pub(crate) fn read(&mut self, dir: PathBuf, subfolders: Vec<PathBuf>) {
        self.children.insert(dir, Some(subfolders));
    }

    /// Forgets what was read of `dir`, its subfolders and its parent, so they are read again.
    pub(crate) fn forget(&mut self, dir: &Path) {
        self.children.retain(|d, _| !d.starts_with(dir));
        if let Some(parent) = dir.parent() {
            self.children.remove(parent);
        }
    }
}

/// The status bar's numbers. It holds the shown image's size and position, the folder's total, and
/// the size of the selection.
pub(crate) struct StatusBar {
    /// Size in bytes of the shown file, for the status bar.
    shown_size: Option<(PathBuf, u64)>,
    /// Image and scale in each pane of the last frame, the pane the status bar tells about (the
    /// one under the pointer, else the first) and the panes as [columns, rows].
    pane_info: Vec<(usize, Option<f32>)>,
    pane: usize,
    grid: [usize; 2],
    /// The position "[n/m]" of an image in another pane than the first, by image and list length.
    position_text: Option<(usize, usize, String)>,
    /// Total size of the images in the folder, summed in the background after listing.
    /// Only sums are sent (no per-file table), so memory use does not grow with the folder.
    total_size: u64,
    /// The partial sums from the background. It is `None` once all have arrived.
    size_rx: Receiving<u64>,
    /// Bytes freed by deletions in the background.
    freed_tx: mpsc::Sender<u64>,
    freed_rx: mpsc::Receiver<u64>,
    /// The size of the selected files. It is worked out in the background when the selection
    /// changes.
    selected_size: Option<u64>,
    selected_rx: Receiving<u64>,
    /// When to work out that size, once the selection has stayed the same for a moment.
    selected_size_due: Option<Instant>,
}

impl Default for StatusBar {
    fn default() -> Self {
        let (freed_tx, freed_rx) = mpsc::channel();
        StatusBar {
            shown_size: None,
            pane_info: Vec::new(),
            pane: 0,
            grid: [1, 1],
            position_text: None,
            total_size: 0,
            size_rx: Receiving::none(),
            freed_tx,
            freed_rx,
            selected_size: None,
            selected_rx: Receiving::none(),
            selected_size_due: None,
        }
    }
}

/// Whether the selection's size is to be worked out now.
pub(crate) enum SizeDue {
    /// The size is not asked for, because nothing is selected, or it is known.
    No,
    /// The selection may change again within this time.
    Later(std::time::Duration),
    Now,
}

impl StatusBar {
    /// Where the background tells of bytes freed by a delete.
    pub(crate) fn freed_sender(&self) -> mpsc::Sender<u64> {
        self.freed_tx.clone()
    }

    /// The shown file's size is read again, as it changed.
    pub(crate) fn forget_size(&mut self) {
        self.shown_size = None;
    }

    /// The folder's total, summed anew from `sums`.
    pub(crate) fn sum_folder(&mut self, sums: mpsc::Receiver<u64>) {
        self.size_rx = Receiving::of(sums);
        self.total_size = 0;
    }

    /// The folder's total, once every sum has come.
    pub(crate) fn folder_total(&self) -> Option<u64> {
        self.size_rx.is_none().then_some(self.total_size)
    }

    /// Takes what the background sent: the bytes freed, the selection's size and the folder's
    /// sums.
    pub(crate) fn receive(&mut self) {
        while let Ok(freed) = self.freed_rx.try_recv() {
            // shortcut: a delete while the sum is still being computed can make it slightly off;
            // fine for a status bar figure.
            self.total_size = self.total_size.saturating_sub(freed);
        }
        match self.selected_rx.take() {
            Took::Answer(size) => self.selected_size = Some(size),
            // When the summing thread broke, the size is none, instead of "calculating…" for
            // good.
            Took::Gone => self.selected_size = Some(0),
            Took::Nothing => {}
        }
        drain(&mut self.size_rx, &mut self.total_size);
    }

    /// The position "[n/m]" of image `idx` among `total`. `make` makes it only when another image
    /// or another total asks, so the search matches are looked through once for each image.
    pub(crate) fn position_text(
        &mut self,
        idx: usize,
        total: usize,
        make: impl FnOnce() -> String,
    ) -> String {
        if let Some((i, t, text)) = &self.position_text
            && (*i, *t) == (idx, total)
        {
            return text.clone();
        }
        let text = make();
        self.position_text = Some((idx, total, text.clone()));
        text
    }

    /// Makes the position told for another pane again, because another search may have as many
    /// matches.
    pub(crate) fn forget_position_text(&mut self) {
        self.position_text = None;
    }

    /// Marks the selection's size as unknown, because the selection changed. It is worked out
    /// once the selection has stayed the same for a moment, which this returns. It is not
    /// worked out at once, because Shift+↓ held in a selection of 500,000 changes it every
    /// frame, and each time a path for every file would be made.
    pub(crate) fn selection_changed(&mut self, any: bool) -> std::time::Duration {
        let wait = std::time::Duration::from_millis(200);
        self.selected_size = None;
        self.selected_rx = Receiving::none();
        self.selected_size_due = any.then(|| Instant::now() + wait);
        wait
    }

    /// Whether the selection's size is due. Once it is, it is no longer asked for.
    pub(crate) fn size_due(&mut self) -> SizeDue {
        let Some(due) = self.selected_size_due else {
            return SizeDue::No;
        };
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            return SizeDue::Later(wait);
        }
        self.selected_size_due = None;
        SizeDue::Now
    }

    /// The selection's size, known already.
    pub(crate) fn selection_is(&mut self, size: u64) {
        self.selected_size = Some(size);
        self.selected_rx = Receiving::none();
    }

    /// The selection's size, summed in the background.
    pub(crate) fn sum_selection(&mut self, sum: mpsc::Receiver<u64>) {
        self.selected_rx = Receiving::of(sum);
    }

    /// Keeps what the image area showed: the pane the status bar tells about, the panes as
    /// [columns, rows], and each pane's image and scale.
    pub(crate) fn panes_shown(
        &mut self,
        pane: usize,
        grid: [usize; 2],
        panes: Vec<(usize, Option<f32>)>,
    ) {
        self.pane = pane;
        self.grid = grid;
        self.pane_info = panes;
    }
}

impl crate::App {
    // ---- The file list, folders and thumbnails ----

    /// The thumbnail box in the file list, by the size chosen in Preferences.
    pub(crate) fn thumb(&self) -> egui::Vec2 {
        THUMBS[self.config.thumb_size.min(2) as usize]
    }

    /// Takes thumbnails from the freedesktop "large" cache when the box is wider than a
    /// "normal" one on the screen. At 150 % scaling even the smallest box is 144 pixels wide.
    /// It is read again when the scale or the size changes.
    pub(crate) fn sync_thumb_source(&mut self, ppp: f32) {
        let large = self.thumb().x * ppp > 128.0;
        if LARGE_THUMBS.swap(large, Ordering::Relaxed) != large {
            self.thumbs_changed();
        }
    }

    /// Lets go of the thumbnails, because they look otherwise now (their size, source or
    /// colours). The made ones go, and those being made are marked stale, so the results are
    /// made again.
    pub(crate) fn thumbs_changed(&mut self) {
        self.thumbs.textures.clear();
        self.thumbs.pending.clear();
    }

    /// The size of a cell of a grid, the thumbnail with its name below, or without the name
    /// when View → Show File Names Under Thumbnails is off. Every grid shows the names or none,
    /// so the setting is read here and not passed by each view as its form.
    pub(crate) fn cell(&self) -> egui::Vec2 {
        let extra = if self.config.grid_names {
            CELL_EXTRA
        } else {
            CELL_EXTRA_NO_NAME
        };
        self.thumb() + extra
    }

    pub(crate) fn set_thumb_size(&mut self, size: u8) {
        self.config.thumb_size = size;
        self.config.save();
        self.thumbs_changed();
        self.panel.scroll_to_shown = true;
    }

    /// Shows the file names under the thumbnails of every grid, or none.
    pub(crate) fn set_grid_names(&mut self, on: bool) {
        self.config.grid_names = on;
        self.config.save();
        // The rows change height, so the shown image is scrolled to again.
        self.panel.scroll_to_shown = true;
    }

    /// Thumbnails in the file list, or none.
    pub(crate) fn set_thumbnails(&mut self, on: bool) {
        self.config.thumbnails = on;
        self.config.save();
        if !on {
            self.thumbs.textures.clear();
            self.thumbs.pending.clear();
        }
        self.panel.scroll_to_shown = true;
    }

    /// List the images of all subfolders too, or only the folder's own.
    pub(crate) fn set_subfolders(&mut self, ctx: &egui::Context, on: bool) {
        self.config.show_subfolders = on;
        self.config.save();
        self.folder.show_subfolders(on);
        self.list(ctx);
    }

    pub(crate) fn receive_thumbs(&mut self, ctx: &egui::Context) {
        while let Ok((path, n, result)) = self.thumbs.rx.try_recv() {
            // The name is the one the list has, with its folders in a slideshow with
            // subfolders.
            let Ok(name) = path.strip_prefix(self.folder.dir()).map(Path::as_os_str) else {
                continue;
            };
            // A thumbnail asked for before the file or the look changed is asked for again.
            if !self.thumbs.answered(name, n) {
                continue;
            }
            if let Some(img) = result {
                let tex = img.map(|img| {
                    ctx.load_texture(
                        format!("thumb {}", path.display()),
                        img,
                        egui::TextureOptions::LINEAR,
                    )
                });
                self.thumbs.textures.insert(name.to_owned(), tex);
            }
        }
    }

    /// Drop thumbnails far from view once there are many, so memory stays flat in huge folders.
    /// Over the budget, drop the thumbnails of all but the rows `rows` of the list as shown (the
    /// search matches, or all files) and those near them.
    pub(crate) fn trim_thumbs(&mut self, rows: std::ops::Range<usize>) {
        // Big thumbnails take four times the memory, so a quarter as many are kept.
        let max = if LARGE_THUMBS.load(Ordering::Relaxed) {
            MAX_THUMBS / 4
        } else {
            MAX_THUMBS
        };
        if self.thumbs.textures.len() <= max {
            return;
        }
        let mut keep = thumbs_to_keep(self.list.files(), self.list.matches(), rows, max);
        keep.extend(
            self.list
                .pan_thumbs()
                .iter()
                .filter_map(|&i| self.list.files().get(i)),
        );
        self.thumbs.textures.retain(|name, _| keep.contains(name));
    }

    /// A file list row, with a thumbnail when `thumbs` is set. The list that draws it decides
    /// that, not the setting, as the side panel has names while the grid in the image area
    /// shows.
    pub(crate) fn file_row(
        &mut self,
        ui: &mut egui::Ui,
        i: usize,
        height: f32,
        marked: bool,
        thumbs: bool,
    ) -> egui::Response {
        let size = egui::vec2(ui.available_width(), height);
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
        speak_item(&response, marked, &self.name(i));
        let visuals = ui.visuals();
        if marked || response.hovered() {
            let fill = if marked {
                visuals.selection.bg_fill
            } else {
                visuals.widgets.hovered.weak_bg_fill
            };
            ui.painter().rect_filled(rect, 2.0, fill);
        }
        let text_color = if marked {
            visuals.selection.stroke.color
        } else {
            visuals.text_color()
        };
        let frame = egui::Rect::from_min_size(
            rect.min + egui::vec2(2.0, 2.0),
            if thumbs {
                self.thumb()
            } else {
                egui::vec2(0.0, height)
            },
        );
        if thumbs {
            self.paint_thumb(ui, i, frame);
        }
        let font = egui::TextStyle::Body.resolve(ui.style());
        let gap = if thumbs { 8.0 } else { 2.0 };
        let mut name_rect = rect.with_min_x(frame.right() + gap);
        if self.config.file_details {
            // The size and date columns are at the right, as in Geeqie, and the name is cut off
            // before them.
            name_rect.max.x -= DETAILS_WIDTH;
            let (bytes, mtime) = self.file_details(i);
            let y = rect.center().y;
            let size_right = name_rect.max.x + DETAILS_WIDTH - DATE_WIDTH - 8.0;
            let p = ui.painter();
            p.text(
                egui::pos2(size_right, y),
                egui::Align2::RIGHT_CENTER,
                thousands(bytes as usize),
                font.clone(),
                text_color,
            );
            p.text(
                egui::pos2(rect.max.x - DATE_WIDTH, y),
                egui::Align2::LEFT_CENTER,
                local_time(mtime),
                font.clone(),
                text_color,
            );
        }
        // Stars and marks at the right of the name, which is cut off before them.
        let marks = self.mark_text(i);
        if !marks.is_empty() {
            let gold = egui::Color32::from_rgb(255, 190, 40);
            let at = egui::pos2(name_rect.max.x - 6.0, rect.center().y);
            let r = ui
                .painter()
                .text(at, egui::Align2::RIGHT_CENTER, marks, font.clone(), gold);
            name_rect.max.x = r.left() - 8.0;
        }
        // A file Check Images found broken gets a warning sign at the right.
        if self.broken_files.contains(&self.list.files()[i]) {
            let c = egui::pos2(name_rect.max.x - 12.0, rect.center().y);
            broken_mark(ui.painter(), c, font.size);
            name_rect.max.x = c.x - 14.0;
        }
        // Cut in the middle, so the end of the name and its extension stay.
        let width = name_rect.width() - 4.0;
        let p = ui.painter();
        let fits = |s: &str| {
            p.layout_no_wrap(s.to_owned(), font.clone(), text_color)
                .size()
                .x
                <= width
        };
        let name = middle_cut(&self.name(i), fits);
        p.with_clip_rect(name_rect).text(
            egui::pos2(name_rect.min.x, rect.center().y),
            egui::Align2::LEFT_CENTER,
            name,
            font,
            text_color,
        );
        response
    }

    /// A cell of the thumbnail grid. It shows the thumbnail with the name below, cut off to
    /// fit, unless the names are turned off. The name is still read to a screen reader and shown
    /// when the pointer rests on the cell.
    pub(crate) fn file_cell(
        &mut self,
        ui: &mut egui::Ui,
        i: usize,
        rect: egui::Rect,
        marked: bool,
    ) -> egui::Response {
        let response = ui.interact(rect, ui.scope_id().with(("cell", i)), egui::Sense::click());
        let name = self.name(i).into_owned();
        speak_item(&response, marked, &name);
        let visuals = ui.visuals();
        if marked || response.hovered() {
            let fill = if marked {
                visuals.selection.bg_fill
            } else {
                visuals.widgets.hovered.weak_bg_fill
            };
            ui.painter().rect_filled(rect, 2.0, fill);
        }
        let color = if marked {
            visuals.selection.stroke.color
        } else {
            visuals.text_color()
        };
        let frame = egui::Rect::from_min_size(rect.min + egui::vec2(4.0, 4.0), self.thumb());
        self.paint_thumb(ui, i, frame);
        if self.config.grid_names {
            let font = egui::TextStyle::Small.resolve(ui.style());
            let mut job = egui::text::LayoutJob::simple_singleline(name, font, color);
            job.wrap = egui::text::TextWrapping::truncate_at_width(self.cell().x - 6.0);
            let galley = ui.painter().layout_job(job);
            let at = egui::pos2(
                rect.center().x - galley.size().x / 2.0,
                frame.bottom() + 4.0,
            );
            ui.painter().galley(at, galley, color);
        }
        // A file Check Images found broken gets the warning sign on the thumbnail's top left.
        if self.broken_files.contains(&self.list.files()[i]) {
            let size = egui::TextStyle::Body.resolve(ui.style()).size;
            broken_mark(
                ui.painter(),
                frame.left_top() + egui::vec2(10.0, 10.0),
                size,
            );
        }
        let marks = self.mark_text(i);
        if !marks.is_empty() {
            let p = ui.painter();
            let gold = egui::Color32::from_rgb(255, 190, 40);
            let galley = p.layout_no_wrap(marks, egui::TextStyle::Small.resolve(ui.style()), gold);
            let at = egui::Rect::from_min_size(
                frame.right_top() + egui::vec2(-galley.size().x - 4.0, 3.0),
                galley.size(),
            );
            p.rect_filled(at.expand(2.0), 3.0, egui::Color32::from_black_alpha(170));
            p.galley(at.min, galley, gold);
        }
        response.on_hover_text_at_pointer(match self.folder.collection() {
            Some(_) => self.path(i).display().to_string(),
            None => self.name(i).into_owned(),
        })
    }

    /// Thumbnail `i` fitted into `frame`, or a request for it.
    pub(crate) fn paint_thumb(&mut self, ui: &egui::Ui, i: usize, frame: egui::Rect) {
        let name = &self.list.files()[i];
        match self.thumbs.textures.get(name) {
            Some(Some(tex)) => {
                // Fit the box, but never enlarge a small image. One the cache cut down to its
                // size (256 pixels) does fill a box larger on a scaled screen.
                let px = tex.size_vec2() / ui.ctx().pixels_per_point();
                let cut = tex.size().into_iter().max() >= Some(256);
                // The thumbnail fits the frame given, because a narrow calendar day has less
                // room than a list row.
                let fit = (frame.width() / px.x).min(frame.height() / px.y);
                let scale = if cut { fit } else { fit.min(1.0) };
                let uv = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
                let at = egui::Rect::from_center_size(frame.center(), px * scale);
                ui.painter().image(tex.id(), at, uv, egui::Color32::WHITE);
                // Videos get a small play badge in the corner.
                if is_video(Path::new(name)) {
                    let c = at.left_bottom() + egui::vec2(13.0, -13.0);
                    let p = ui.painter();
                    p.circle_filled(c, 10.0, egui::Color32::from_black_alpha(170));
                    let triangle = vec![
                        c + egui::vec2(-3.0, -5.5),
                        c + egui::vec2(6.0, 0.0),
                        c + egui::vec2(-3.0, 5.5),
                    ];
                    p.add(egui::Shape::convex_polygon(
                        triangle,
                        egui::Color32::WHITE,
                        egui::Stroke::NONE,
                    ));
                }
            }
            Some(None) => {}
            None => {
                let path = self.path(i);
                self.thumbs.ask(name, i, path);
            }
        }
    }

    /// The film strip along the bottom of `area` in full screen, shown while the pointer is
    /// near the bottom. It has thumbnails of the images before and after the shown one, which
    /// is outlined. Returns the image clicked.
    pub(crate) fn filmstrip(&mut self, ui: &egui::Ui, area: egui::Rect) -> Option<usize> {
        let cell = self.thumb() + egui::vec2(8.0, 8.0);
        let height = cell.y + 16.0;
        let strip =
            egui::Rect::from_min_max(egui::pos2(area.left(), area.bottom() - height), area.max);
        if !ui.rect_contains_pointer(strip) {
            return None;
        }
        let k = ((area.width() / cell.x) as isize - 1).max(0) / 2;
        let mut shown: Vec<usize> = (-k..=k).filter_map(|d| self.step(d, false)).collect();
        shown.dedup();
        // Thumbnails for these, as for the rows of the file list.
        if !shown.is_empty() {
            let mut view = shown.clone();
            view.sort_unstable();
            self.thumbs.view.lock().unwrap()[0] = view;
            let row = match self.list.matches() {
                Some(m) => self.shown_row(m).unwrap_or(0),
                None => self.list.shown(),
            };
            let k = k as usize;
            self.trim_thumbs(row.saturating_sub(k)..row + k + 1);
        }
        ui.painter()
            .rect_filled(strip, 0.0, egui::Color32::from_black_alpha(190));
        // The shown image in the middle, where it is when there are images on both sides.
        let at = shown
            .iter()
            .position(|&i| i == self.list.shown())
            .unwrap_or(0) as f32;
        let x0 = strip.center().x - (at + 0.5) * cell.x;
        let mut clicked = None;
        for (n, &i) in shown.iter().enumerate() {
            let min = egui::pos2(x0 + n as f32 * cell.x, strip.top() + 8.0);
            let frame = egui::Rect::from_min_size(min, cell).shrink(4.0);
            self.paint_thumb(ui, i, frame);
            if i == self.list.shown() {
                let stroke = egui::Stroke::new(2.0, ui.visuals().selection.bg_fill);
                ui.painter()
                    .rect_stroke(frame.expand(2.0), 3.0, stroke, egui::StrokeKind::Outside);
            }
            let id = ui.scope_id().with(("strip", i));
            let response = ui.interact(frame, id, egui::Sense::click());
            speak_item(&response, i == self.list.shown(), &self.name(i));
            if response.clicked() {
                clicked = Some(i);
            }
        }
        clicked
    }

    /// Size and modification time of file `i`, from the cache or one stat.
    /// shortcut: stat in the UI thread, once per visible file; move to a thread if network
    /// folders make scrolling stutter.
    pub(crate) fn file_details(&mut self, i: usize) -> (u64, i64) {
        if let Some(&d) = self.list.details().get(&self.list.files()[i]) {
            return d;
        }
        let d = std::fs::metadata(self.path(i)).map_or((0, 0), |m| {
            use std::os::unix::fs::MetadataExt;
            (m.len(), m.mtime())
        });
        // The cache is bounded, because scrolling through a folder of 500,000 would keep some
        // 50 MB. Only the rows on screen are read again.
        if self.list.details().len() >= 20_000 {
            self.list.forget_details();
        }
        self.list.remember_details(self.list.files()[i].clone(), d);
        d
    }

    /// Folders as a tree from /, opened up to the current folder. Subfolders are read in the
    /// background when a folder is opened in the tree.
    pub(crate) fn folder_tree(&mut self, ui: &mut egui::Ui, open_dir: &mut Option<PathBuf>) {
        while let Ok((dir, subdirs)) = self.tree.rx.try_recv() {
            self.tree.children.insert(dir, Some(subdirs));
        }
        // Opened up to the folder shown when that changes.
        let follow = self.tree.shown != self.folder.dir();
        if follow {
            self.tree
                .open
                .extend(self.folder.dir().ancestors().map(Path::to_path_buf));
        }
        // The open folders are rows, so only those on screen are drawn, also when a folder of
        // 100,000 folders is opened. An empty path is a folder still being read.
        // shortcut: the rows are made again each frame, a lookup for each; keep them until the
        // tree changes if trees of millions of folders are opened.
        let tree = std::mem::take(&mut self.tree.children);
        let mut rows: Vec<(usize, &Path)> = Vec::new();
        let mut unread = Vec::new();
        let mut stack = vec![(0, Path::new("/"))];
        while let Some((depth, path)) = stack.pop() {
            rows.push((depth, path));
            if !self.tree.open.contains(path) {
                continue;
            }
            match tree.get(path) {
                Some(Some(subdirs)) => {
                    stack.extend(subdirs.iter().rev().map(|d| (depth + 1, d.as_path())))
                }
                Some(None) => rows.push((depth + 1, Path::new(""))),
                None => unread.push(path.to_path_buf()),
            }
        }
        let row_h = ui.spacing().interact_size.y;
        let mut area = egui::ScrollArea::both().id_salt("tree").auto_shrink(false);
        // Shown once the folders above it are read.
        if follow && let Some(i) = rows.iter().position(|r| r.1 == self.folder.dir()) {
            // A row and the space below it, as show_rows lays them out.
            let step = row_h + ui.spacing().item_spacing.y;
            let top = i as f32 * step - ui.available_height() / 2.0;
            area = area.vertical_scroll_offset(top.max(0.0));
            self.tree.shown = self.folder.dir().to_path_buf();
        }
        let mut toggled = None;
        area.show_rows(ui, row_h, rows.len(), |ui, range| {
            for &(depth, path) in &rows[range] {
                ui.horizontal(|ui| {
                    ui.add_space(depth as f32 * ui.spacing().indent);
                    if path.as_os_str().is_empty() {
                        return drop(ui.label("…"));
                    }
                    let open = self.tree.open.contains(path);
                    let caret = match open {
                        true => theme::icon::CARET_DOWN,
                        false => theme::icon::CARET_RIGHT,
                    };
                    let name = path.file_name().map_or("/".into(), |n| n.to_string_lossy());
                    #[allow(clippy::disallowed_methods)] // A caret has no text to note.
                    let toggle = ui.add(egui::Button::new(caret).frame(false));
                    // Told to a screen reader by the folder's name and whether it is open, not
                    // by the icon's glyph.
                    toggle.widget_info(|| {
                        let role = egui::Role::DisclosureTriangle;
                        egui::WidgetInfo::selected(role, true, open, &name)
                    });
                    if toggle.clicked() {
                        toggled = Some(path.to_path_buf());
                    }
                    let label = selectable(ui, path == self.folder.dir(), name.as_ref());
                    if label.clicked() {
                        *open_dir = Some(path.to_path_buf());
                    }
                    if path.parent().is_some() {
                        self.folder_menu(&label, path);
                    }
                });
            }
        });
        drop(rows);
        self.tree.children = tree;
        if let Some(path) = toggled
            && !self.tree.open.remove(&path)
        {
            self.tree.open.insert(path);
        }
        let folders = Types {
            hidden: self.config.show_hidden,
            ..Types::NONE
        };
        for dir in unread {
            self.tree.children.insert(dir.clone(), None);
            let (tx, ctx) = (self.tree.tx.clone(), ui.ctx().clone());
            std::thread::spawn(move || {
                let subdirs = list_folder(&dir, folders).0;
                let _ = tx.send((dir, subdirs));
                ctx.request_repaint();
            });
        }
    }

    /// The menu of a right-click on a folder in the list or the tree. It renames the folder, or
    /// moves it to the trash.
    pub(crate) fn folder_menu(&mut self, response: &egui::Response, dir: &Path) {
        response.context_menu(|ui| {
            // Its images in the list together with those shown now.
            let shown =
                self.folder.folders().contains(&dir.to_path_buf()) || dir == self.folder.dir();
            if !shown
                && self.folder.collection().is_none()
                && crate::ui::menus::button(ui, tr("Show Together with These Images")).clicked()
            {
                let mut all = match self.folder.folders().is_empty() {
                    true => vec![self.folder.dir().to_path_buf()],
                    false => self.folder.folders().to_vec(),
                };
                all.push(dir.to_path_buf());
                self.folder.show_together(all);
            }
            if crate::ui::menus::button(ui, tr("Rename Folder…")).clicked() {
                self.dialogs.ask_rename_folder(dir);
            }
            if crate::ui::menus::button(ui, tr("Move Folder to Trash…")).clicked() {
                self.dialogs.ask_trash_folder(dir);
            }
        });
    }

    /// The sort manager (Shift+S), as in Geeqie. A click on a folder copies or moves the
    /// selected files, or the shown one, there.
    pub(crate) fn sort_manager(&mut self, ui: &mut egui::Ui) {
        let mut remove = None;
        egui::Panel::right("sort manager")
            .resizable(true)
            .default_size(200.0)
            .show(ui, |ui| {
                ui.strong(tr("Sort Manager"));
                ui.horizontal(|ui| {
                    let c = &mut self.config;
                    let changed = radio_choice(ui, &mut c.sort_move, false, tr("Copy")).changed()
                        | radio_choice(ui, &mut c.sort_move, true, tr("Move")).changed();
                    if changed {
                        c.save();
                    }
                });
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (k, dir) in self.folder.sort_folders().to_vec().into_iter().enumerate() {
                        ui.horizontal(|ui| {
                            if crate::ui::menus::small_button(ui, theme::icon::CLOSE)
                                .on_hover_text(tr("Remove"))
                                .clicked()
                            {
                                remove = Some(k);
                            }
                            let name = dir.file_name().unwrap_or(dir.as_os_str());
                            let here = dir == self.folder.dir();
                            let on = !here && !self.list.files().is_empty();
                            let button =
                                crate::ui::menus::button_enabled(ui, on, &name.to_string_lossy());
                            if button.on_hover_text(dir.display().to_string()).clicked()
                                && let Some(targets) = self.targets()
                            {
                                let paths = targets.iter().map(|&i| self.path(i)).collect();
                                self.transfer(ui.ctx(), paths, dir, !self.config.sort_move);
                            }
                        });
                    }
                    ui.horizontal(|ui| {
                        if crate::ui::menus::button(ui, tr("Add This Folder")).clicked() {
                            self.add_sort_folder(self.folder.dir().to_path_buf());
                        }
                        if crate::ui::menus::button(ui, tr("Add…")).clicked() {
                            self.pick(ui.ctx(), true, Pick::SortFolder);
                        }
                    });
                });
            });
        if let Some(k) = remove {
            let dir = self.folder.sort_folders()[k].clone();
            self.change_sort_folders(None, Some(&dir));
        }
    }

    pub(crate) fn add_sort_folder(&mut self, dir: PathBuf) {
        if !self.folder.sort_folders().contains(&dir) {
            self.change_sort_folders(Some(&dir), None);
        }
    }

    /// Adds a folder to the sort manager or takes one out. The change goes to the file as it is
    /// now, so another window's folders stay, and it is written whole or not at all.
    pub(crate) fn change_sort_folders(&mut self, add: Option<&Path>, remove: Option<&Path>) {
        if config::locked() {
            return self.notice = Some(tr("The settings are locked (--lock-settings)").into());
        }
        let Some(path) = sort_folders_path() else {
            return self.notice =
                Some(tr("Could not save the sort manager's folders: no home folder").into());
        };
        // The change is made on the file as it is now, so another window's folders stay. A file
        // that cannot be read is not written over.
        let change = |old: &[u8]| {
            let dirs = config::sort_folders_changed(sort_folders_from(old), add, remove);
            (config::sort_folders_text(&dirs), dirs)
        };
        match crate::files::file_io::edit_shared(&path, change) {
            Ok(dirs) => self.folder.set_sort_folders(dirs),
            Err(e) => self.notice = Some(tf!("Could not save the sort manager's folders: {e}", e)),
        }
    }

    /// The file types the list shows, from the Types menu.
    /// The type of a file opened directly is listed too, in its folder.
    pub(crate) fn types(&self) -> Types {
        let o = self.folder.opened_type();
        Types {
            images: self.config.list_images || o.images,
            raw: self.config.list_raw || o.raw,
            videos: self.config.list_videos || o.videos,
            hidden: self.config.show_hidden || o.hidden,
        }
    }

    /// The file types to list, as the file classes of Geeqie's file filter, beside the sort
    /// menu.
    pub(crate) fn types_menu(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        let c = &mut self.config;
        let mut all = vec![(tr("Images"), &mut c.list_images), ("RAW", &mut c.list_raw)];
        if cfg!(feature = "video") {
            all.push((tr("Videos"), &mut c.list_videos));
        }
        let on: Vec<&str> = all.iter().filter(|t| *t.1).map(|t| t.0).collect();
        let title = match on.len() {
            n if n == all.len() => tr("All types").to_owned(),
            0 => tr("No types").to_owned(),
            _ => on.join(", "),
        };
        let mut changed = false;
        submenu(ui, format!("{title} {}", theme::icon::CARET_DOWN), |ui| {
            for (label, on) in all {
                changed |= checkbox(ui, on, label).changed();
            }
        });
        if changed {
            self.config.save();
            self.list(ctx);
        }
    }

    /// Geeqie's sort menu in the status bar.
    pub(crate) fn sort_menu(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        let mut changed = false;
        let title = format!(
            "{} {}",
            tr(self.config.sort.row().2),
            theme::icon::CARET_DOWN
        );
        scroll_menu(ui, title, |ui| {
            for (sort, _, label) in SORTS {
                if selectable(ui, self.config.sort == sort, tr(label)).clicked() {
                    self.config.sort = sort;
                    changed = true;
                }
            }
            ui.separator();
            changed |= checkbox(ui, &mut self.config.sort_ascending, tr("Ascending")).changed();
            changed |= checkbox(ui, &mut self.config.sort_case_sensitive, tr("Case")).changed();
        });
        if changed {
            self.config.save();
            // A chosen order replaces a shuffled one (kept after a random slideshow).
            self.slideshow.unshuffle();
            self.folder.sort_again();
            self.list(ctx);
        }
    }

    /// The folder path above the folder list, as in Geeqie. Enter opens what was typed.
    pub(crate) fn path_field(&mut self, ui: &mut egui::Ui) {
        let edit =
            egui::TextEdit::singleline(self.folder.path_text_mut()).desired_width(f32::INFINITY);
        let response = ui.add(edit);
        if entered(&response) {
            let text = self.folder.path_text().trim().to_owned();
            let text = text.as_str();
            let path = match (text.strip_prefix('~'), std::env::var_os("HOME")) {
                (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') => {
                    PathBuf::from(home).join(rest.trim_start_matches('/'))
                }
                _ => PathBuf::from(text),
            };
            if text.contains("://") {
                self.mount(ui.ctx(), text.to_owned());
            } else if path.exists() {
                self.visit(ui.ctx(), path);
            } else {
                self.notice = Some(tf!("No such folder: {folder}", folder = path.display()));
            }
        }
        if !response.has_focus() {
            self.folder.show_path();
        }
    }

    /// The left side, as in Geeqie, with the path, the folders at the top, the search, and the
    /// files below. Returns what a click there asked for.
    pub(crate) fn side_panel(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        let (mut open_dir, mut go_to, mut list_act, mut opened) = (None, None, None, None);
        // The grid in the image area has the thumbnails, so the list has names.
        let form = if self.panel.area_grid_on() {
            [false, false]
        } else {
            [self.config.icons, self.config.thumbnails]
        };
        // Room for a name next to the size and date columns, and the thumbnail before it.
        let thumb = if form[1] { self.thumb().x + 10.0 } else { 0.0 };
        let min = if self.config.file_details {
            DETAILS_WIDTH + 140.0 + thumb
        } else {
            80.0 + thumb
        };
        egui::Panel::left("left")
            .resizable(true)
            .default_size(240.0)
            .min_size(min)
            .show(ui, |ui| {
                if self.config.path_bar {
                    self.path_field(ui);
                }
                egui::Panel::top("folders")
                    .resizable(true)
                    .default_size(200.0)
                    .show(ui, |ui| {
                        if self.config.folder_tree {
                            self.folder_tree(ui, &mut open_dir);
                        } else {
                            self.folder_list(ui, &mut open_dir);
                        }
                    });
                if self.config.search_bar {
                    self.search_field(ui);
                }
                opened = self.file_list(ctx, ui, &mut go_to, &mut list_act, form);
            });
        // A double-click opens the file clicked, also with Ctrl or Shift, whose clicks change
        // the selection and go to no file. A plain click before it has already gone there.
        if let Some(d) = open_dir {
            self.visit(ctx, d);
        } else if let Some(i) = opened.or(go_to) {
            self.go(ctx, i);
        }
        if let Some(action) = list_act {
            self.run(ctx, action);
        }
    }

    /// View → Thumbnails in Image Area: the files as a grid of thumbnails where the image is.
    /// The arrow keys move in it as in the icon grid, and Enter or a double-click shows the
    /// image there.
    pub(crate) fn area_grid(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        let (mut go_to, mut list_act) = (None, None);
        let opened = egui::CentralPanel::default()
            .show(ui, |ui| {
                // The grid takes the image's place for a screen reader too. The image shown is
                // read out as the arrows move in the grid, as it is over the image.
                #[cfg(feature = "accessibility")]
                {
                    let id = ui.scope_id().with("grid");
                    let response = ui.interact(ui.max_rect(), id, egui::Sense::click());
                    self.speak_image(ui, &response);
                }
                let opened = self.file_list(ctx, ui, &mut go_to, &mut list_act, [true, true]);
                // A failed copy or delete says so here too, as over the image.
                self.paint_notice(ui, ui.max_rect());
                opened
            })
            .inner;
        // A double-click opens the file clicked, also with Ctrl or Shift, whose clicks change
        // the selection and go to no file.
        if let Some(i) = opened.or(go_to) {
            self.go(ctx, i);
        }
        if opened.is_some() {
            self.panel.open_from_grid();
        }
        if let Some(action) = list_act {
            self.run(ctx, action);
        }
    }

    /// The folder list, with ⬆ .. for the parent folder and then the subfolders.
    pub(crate) fn folder_list(&mut self, ui: &mut egui::Ui, open_dir: &mut Option<PathBuf>) {
        // Row 0 is ⬆ .. (when there is a parent folder), the rest are subfolders.
        let up = self.folder.dir().parent().map(Path::to_path_buf);
        let rows = up.is_some() as usize + self.folder.subdirs().len();
        let row_h = ui.spacing().interact_size.y;
        egui::ScrollArea::vertical()
            .id_salt("folders")
            .auto_shrink(false)
            .show_rows(ui, row_h, rows, |ui, range| {
                for row in range {
                    let is_up = row == 0 && up.is_some();
                    let (label, target) = match (row, &up) {
                        (0, Some(parent)) => (format!("{}  ..", theme::icon::UP), parent),
                        _ => {
                            let d = &self.folder.subdirs()[row - up.is_some() as usize];
                            let name = d.file_name().unwrap_or_default();
                            (
                                format!("{}  {}", theme::icon::FOLDER, name.to_string_lossy()),
                                d,
                            )
                        }
                    };
                    let target = target.clone();
                    let response = selectable(ui, false, &label);
                    if response.clicked() {
                        *open_dir = Some(target.clone());
                    }
                    if !is_up {
                        self.folder_menu(&response, &target);
                    }
                }
            });
    }

    /// The file list (names, details, thumbnails or icons), drawn only where it is visible.
    /// Returns the file double-clicked, if any.
    #[must_use]
    pub(crate) fn file_list(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        go_to: &mut Option<usize>,
        list_act: &mut Option<Action>,
        [icons, thumbs]: [bool; 2],
    ) -> Option<usize> {
        // The rows shown are the search matches, or all files.
        let count = self
            .list
            .matches()
            .map_or(self.list.files().len(), <[usize]>::len);
        let row_of = |i: usize, m: Option<&[usize]>| match m {
            Some(m) => m.iter().position(|&x| x == i),
            None => Some(i),
        };
        // Only visible rows are drawn, so the list is as fast with 10 files as with 300,000.
        let thumbs = thumbs || icons;
        let mut opened = None;
        let row_h = if icons {
            self.cell().y
        } else if thumbs {
            self.thumb().y + 4.0
        } else {
            ui.spacing().interact_size.y
        };
        let pitch = row_h + ui.spacing().item_spacing.y;
        // As icons, a row holds as many cells as fit the width.
        let gap = ui.spacing().item_spacing.x;
        let cols = if icons {
            (((ui.available_width() + gap) / (self.cell().x + gap)) as usize).max(1)
        } else {
            1
        };
        self.panel.grid_cols = cols;
        let mut area = egui::ScrollArea::vertical()
            .id_salt("files")
            .auto_shrink(false);
        if self.panel.scrolls()
            && let Some(row) = row_of(self.list.shown(), self.list.matches()).map(|r| r / cols)
        {
            // Scroll only when the current image is out of view, and centre it.
            let (top, view) = (row as f32 * pitch, ui.available_height());
            let off = egui::scroll_area::State::load(ctx, ui.make_persistent_id("files"))
                .map_or(0.0, |s| s.clamped_offset().y);
            if top < off || top + pitch > off + view {
                area = area.vertical_scroll_offset((top - view / 2.0).max(0.0));
            }
        }
        area.show_rows(ui, row_h, count.div_ceil(cols), |ui, rows| {
            let first_row = rows.start;
            let rows = rows.start * cols..(rows.end * cols).min(count);
            let shown: Vec<usize> = match self.list.matches() {
                Some(m) => m[rows.clone()].to_vec(),
                None => rows.clone().collect(),
            };

            if thumbs && !shown.is_empty() {
                self.keep_thumbs_near(rows.clone(), count);
            }
            let mut line = egui::Rect::NOTHING;
            let pointer = ctx.input(|i| i.pointer.interact_pos());
            // While the grid in the image area shows, the side panel and the grid both take a
            // drag in a collection. Each scrolls at its edge only while the pointer is not over
            // the other.
            let clip = ui.clip_rect();
            let mine = match pointer {
                Some(p) if self.panel.area_grid_shown() && icons => p.x >= clip.left(),
                Some(p) if self.panel.area_grid_shown() => p.x <= clip.right(),
                _ => true,
            };
            if mine {
                self.drag_scroll(ctx, ui, row_h, pointer);
            }
            let mut cells = Vec::new();
            for (k, i) in shown.into_iter().enumerate() {
                let marked = i == self.list.shown() || self.list.selected().contains(&i);
                let row = if icons {
                    if k % cols == 0 {
                        let size = egui::vec2(ui.available_width(), self.cell().y);
                        line = ui.allocate_exact_size(size, egui::Sense::hover()).0;
                    }
                    let x = (k % cols) as f32 * (self.cell().x + gap);
                    let cell =
                        egui::Rect::from_min_size(line.min + egui::vec2(x, 0.0), self.cell());
                    cells.push((i, cell));
                    self.file_cell(ui, i, cell, marked)
                } else if thumbs || self.config.file_details {
                    self.file_row(ui, i, row_h, marked, thumbs)
                } else {
                    let name = self.name(i);
                    let marks = self.mark_text(i);
                    if marks.is_empty() {
                        selectable(ui, marked, &name)
                    } else {
                        selectable(ui, marked, &format!("{name}   {marks}"))
                    }
                };
                // The release tests click a row by its file's name.
                let row = crate::support::test_hooks::noted(&row, &self.name(i));
                if let Some(i) = self.row_clicked(ctx, &row, i) {
                    *go_to = Some(i);
                }
                opened = row.double_clicked().then_some(i).or(opened);
                row.context_menu(|ui| self.file_menu(ui, list_act));
                if self.folder.collection().is_some() {
                    self.drag_row(ui, &row, i, icons, pointer);
                }
            }
            if icons {
                self.drag_rubber_band(ui, &cells, row_h, first_row);
            }
            // The list drawn last drops, once the row under the pointer has set the target.
            if icons || !self.panel.area_grid_shown() {
                self.drop_dragged(ctx);
            }
        });
        opened
    }

    /// Keeps the thumbnails of the list's `rows` shown, and some around them, being made while
    /// they are near, and lets the others go.
    fn keep_thumbs_near(&mut self, rows: std::ops::Range<usize>, count: usize) {
        // The rows shown and some around them, as files, are kept being made while they are
        // near.
        let near = rows.start.saturating_sub(THUMB_MARGIN)..(rows.end + THUMB_MARGIN).min(count);
        let mut view: Vec<usize> = match self.list.matches() {
            Some(m) => m[near].to_vec(),
            None => near.collect(),
        };
        view.sort_unstable();
        self.thumbs.view.lock().unwrap()[0] = view;
        self.thumbs.list_rows = rows.clone();
        self.trim_thumbs(rows);
    }

    /// Scrolls the list on while rows are dragged in a collection near its top or bottom edge.
    fn drag_scroll(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        row_h: f32,
        pointer: Option<egui::Pos2>,
    ) {
        if self.list.drag_target_mut().is_some() {
            let clip = ui.clip_rect();
            match pointer {
                Some(p) if p.y < clip.top() + row_h / 2.0 => {
                    ui.scroll_with_delta(egui::vec2(0.0, 10.0))
                }
                Some(p) if p.y > clip.bottom() - row_h / 2.0 => {
                    ui.scroll_with_delta(egui::vec2(0.0, -10.0))
                }
                _ => {}
            }
            ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
            ctx.request_repaint();
        }
    }

    /// Drops the dragged images before the row the line was at. A drag let go elsewhere ends.
    fn drop_dragged(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.pointer.any_released())
            && let Some((picked, Some(to))) = self.list.take_drag()
        {
            let order = collections::dropped(self.list.files().len(), &picked, to);
            self.move_in_collection(ctx, picked, order);
        } else if !ctx.input(|i| i.pointer.any_down()) {
            self.list.take_drag();
        }
    }

    /// Selects in the icon grid by dragging a rectangle, as in a file manager. Every image it
    /// touches is selected, and added to the selection with Ctrl or Shift. It starts anywhere,
    /// but in a collection, where dragging an image moves it, it starts from the space between
    /// the images. It counts once the pointer has moved a little, so a click stays a click.
    /// `cells` are the cells shown.
    pub(crate) fn drag_rubber_band(
        &mut self,
        ui: &egui::Ui,
        cells: &[(usize, egui::Rect)],
        row_h: f32,
        first_row: usize,
    ) {
        let ctx = ui.ctx().clone();
        // The rectangle is kept from the grid's top, above the rows drawn, so it moves with the
        // images as they scroll.
        let pitch = row_h + ui.spacing().item_spacing.y;
        let origin = ui.max_rect().min - egui::vec2(0.0, first_row as f32 * pitch);
        let (cell, gap) = (self.cell(), ui.spacing().item_spacing.x);
        let cols = self.panel.grid_cols.max(1);
        let count = self
            .list
            .matches()
            .map_or(self.list.files().len(), <[usize]>::len);
        let clip = ui.clip_rect();
        let (pressed, down, pos, add) = ctx.input(|i| {
            let m = i.modifiers;
            (
                i.pointer.primary_pressed(),
                i.pointer.primary_down(),
                i.pointer.interact_pos(),
                m.command || m.shift,
            )
        });
        if pressed
            && self.list.rubber_band().is_none()
            && self.list.dragging().is_none()
            && !self.dialog_open()
            // Not on the scroll bar, which is dragged too.
            && let Some(p) = pos.filter(|p| clip.contains(*p))
            && p.x < clip.right() - ui.spacing().scroll.allocated_width()
            && (self.folder.collection().is_none() || !cells.iter().any(|c| c.1.contains(p)))
        {
            let before = if add {
                self.list.selected_with_hidden()
            } else {
                Default::default()
            };
            self.list.start_rubber_band(RubberBand {
                start: p - origin,
                before,
                moved: false,
                touched: None,
            });
        }
        // The rectangle is taken while it is worked on, and given back unless it ends.
        let Some(mut band_now) = self.list.take_rubber_band() else {
            return;
        };
        let start = origin + band_now.start;
        let Some(p) = pos.filter(|_| down) else {
            // When it is released, the size of what is selected is worked out, once.
            if band_now.moved {
                self.selection_changed(&ctx);
            }
            return;
        };
        // When something else is being dragged (the panel's edge, a scroll bar), it is not a
        // rectangle.
        if !band_now.moved && ctx.dragged_id().is_some() {
            return;
        }
        band_now.moved |= p.distance(start) > 6.0;
        if !band_now.moved {
            self.list.start_rubber_band(band_now);
            return;
        }
        let band = egui::Rect::from_two_pos(start, p);
        // Every cell under it, also those scrolled out of view, from where they are in the grid.
        let rel = band.translate(-origin.to_vec2());
        let rows = touched(rel.top(), rel.bottom(), pitch, row_h);
        let columns = touched(rel.left(), rel.right(), cell.x + gap, cell.x);
        // The selection is made again only when the rectangle touches other cells, because the
        // selection before it may be large (Ctrl+A in a big folder, then Ctrl and a rectangle),
        // and frames come many times a second.
        let touches = [rows.start, rows.end, columns.start, columns.end];
        if band_now.touched != Some(touches) {
            band_now.touched = Some(touches);
            let mut selected = band_now.before.clone();
            for r in rows {
                for c in columns.start..columns.end.min(cols) {
                    let k = r * cols + c;
                    if k < count {
                        selected.insert(self.list.matches().map_or(k, |m| m[k]));
                    }
                }
            }
            self.list.select(selected);
        }
        self.list.start_rubber_band(band_now);
        let v = ui.visuals();
        let painter = ui.painter().with_clip_rect(clip);
        painter.rect_filled(band, 2.0, v.selection.bg_fill.gamma_multiply(0.25));
        let stroke = egui::Stroke::new(1.0, v.selection.bg_fill);
        painter.rect_stroke(band, 2.0, stroke, egui::StrokeKind::Inside);
        // Near the top or bottom edge, the grid scrolls on.
        if p.y < clip.top() + row_h / 2.0 {
            ui.scroll_with_delta(egui::vec2(0.0, 10.0));
        } else if p.y > clip.bottom() - row_h / 2.0 {
            ui.scroll_with_delta(egui::vec2(0.0, -10.0));
        }
        ctx.request_repaint();
    }

    /// Row `i` of a collection, dragged to another place. It drags the selection when the row is
    /// in it, and otherwise the row. A line in the accent colour shows where the images go.
    pub(crate) fn drag_row(
        &mut self,
        ui: &egui::Ui,
        row: &egui::Response,
        i: usize,
        icons: bool,
        pointer: Option<egui::Pos2>,
    ) {
        let row = row.interact(egui::Sense::drag());
        if row.drag_started() {
            let picked = if self.list.selected().contains(&i) {
                self.list.selected().iter().copied().collect()
            } else {
                HashSet::from([i])
            };
            self.list.start_drag(picked);
        }
        let Some(to) = self.list.drag_target_mut() else {
            return;
        };
        let r = row.rect.expand(ui.spacing().item_spacing.y / 2.0);
        let Some(p) = pointer.filter(|p| r.contains(*p)) else {
            return;
        };
        let accent = egui::Stroke::new(2.0, ui.visuals().selection.bg_fill);
        let before = if icons {
            p.x < r.center().x
        } else {
            p.y < r.center().y
        };
        *to = Some(i + !before as usize);
        let p = ui.painter();
        let _ = match (icons, before) {
            (true, true) => p.vline(r.left(), r.y_range(), accent),
            (true, false) => p.vline(r.right(), r.y_range(), accent),
            (false, true) => p.hline(r.x_range(), r.top(), accent),
            (false, false) => p.hline(r.x_range(), r.bottom(), accent),
        };
    }

    /// A click on row `i`. Ctrl and Shift change the selection, and a plain click gives the
    /// image to show. A right-click outside the selection selects just that row.
    pub(crate) fn row_clicked(
        &mut self,
        ctx: &egui::Context,
        row: &egui::Response,
        i: usize,
    ) -> Option<usize> {
        let mut go_to = None;
        if row.clicked() {
            let m = row.ctx.input(|i| i.modifiers);
            if m.command {
                // Ctrl+click toggles the selection (the shown image is included the first time).
                if self.list.selected().is_empty() {
                    self.list.add_to_selection([self.list.shown()]);
                }
                self.list.toggle_selected(i);
                self.list.set_anchor(Some(i));
                self.selection_changed(ctx);
            } else if m.shift {
                // Shift+click selects the rows from where the selection started.
                let anchor = self.list.anchor_or_shown();
                self.list.clear_selection();
                self.select_range(anchor, i);
                self.selection_changed(ctx);
            } else {
                self.list.set_anchor(None);
                self.set_selection(ctx, Default::default());
                go_to = Some(i);
            }
        }
        // A right-click on a file outside the selection selects just that file, as in a file
        // manager, and the menu acts on the selection.
        if row.secondary_clicked() && self.list.right_click(i) {
            self.selection_changed(ctx);
        }
        go_to
    }

    /// The status bar, as in Geeqie. The counts and sizes are on the left, and the shown image
    /// (or the pane under the pointer) on the right.
    pub(crate) fn status_bar(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                let n = self.list.files().len();
                let size = size_text(
                    self.status
                        .size_rx
                        .is_none()
                        .then_some(self.status.total_size),
                );
                let mut left = match self.list.matches() {
                    _ if self.folder.is_listing() => tr("Reading folder…").to_owned(),
                    // A search shows the matches, with the count and size of what the list
                    // shows.
                    Some(found) => {
                        let size = size_text(
                            (self.search.match_size_known()).then_some(self.list.match_size()),
                        );
                        tf!(
                            "{found} of {images} ({size})",
                            found = thousands(found.len()),
                            images = count(n, "image"),
                            size
                        )
                    }
                    None => format!("{count} ({size})", count = count(n, "image")),
                };
                if !self.list.selected().is_empty() {
                    let m = thousands(self.list.selected().len());
                    let size = size_text(self.status.selected_size);
                    left += &tf!(", {m} selected ({size})", m, size);
                }
                let mut hidden = String::new();
                if !self.list.hidden_selected().is_empty() {
                    let h = thousands(self.list.hidden_selected().len());
                    hidden = tf!(", {h} selected but hidden by the search", h);
                    left += &hidden;
                }
                self.sort_menu(ctx, ui);
                self.types_menu(ctx, ui);
                ui.separator();
                // The release tests look for the hidden count on its own.
                crate::support::test_hooks::noted(&ui.label(left), &hidden);
                if let Some((job, jobs)) = self.file_operations.first_job_and_count() {
                    ui.separator();
                    let more = match jobs - 1 {
                        0 => String::new(),
                        n => tf!(", and {n} more", n),
                    };
                    ui.label(tf!(
                        "{doing} {done} of {total}…{more}",
                        doing = tr(job.activity),
                        done = thousands(job.done.load(Ordering::Relaxed)),
                        total = thousands(job.total),
                        more
                    ));
                    if crate::ui::menus::small_button(ui, tr("Stop")).clicked() {
                        self.file_operations.stop_jobs();
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if n == 0 {
                        return;
                    }
                    // As in Geeqie, "( 4000 x 3000 ) 2,345,678 bytes [3/16]" comes first, then
                    // the zoom. With several panes, it tells about the one under the pointer.
                    let (idx, scale) = (self.status.pane_info.get(self.status.pane).copied())
                        .filter(|p| p.0 < n)
                        .unwrap_or((self.list.shown(), Some(self.zoom.scale_shown())));
                    let path = self.path(idx);
                    if self.status.shown_size.as_ref().is_none_or(|c| c.0 != path) {
                        // shortcut: one stat in the UI thread per image change; move it to
                        // a thread if slow network folders make browsing stutter.
                        let bytes = std::fs::metadata(&path).map_or(0, |m| m.len());
                        self.status.shown_size = Some((path, bytes));
                    }
                    let bytes = self.status.shown_size.as_ref().map_or(0, |c| c.1);
                    let position = if idx == self.list.shown() {
                        self.position_text()
                    } else {
                        self.position_text_of(idx)
                    };
                    let bytes = tf!("{n} bytes", n = thousands(bytes as usize));
                    match self.list.loaded_images().get(&idx) {
                        Some(loaded) => {
                            let [w, h] = loaded.original_size;
                            if let Some(scale) = scale {
                                ui.label(format!("{} %", (scale * 100.0).round()));
                                ui.separator();
                            }
                            let page = if loaded.pages > 1 && idx == self.list.shown() {
                                tf!(
                                    " page {n}/{pages}",
                                    n = self.viewing.page() + 1,
                                    pages = loaded.pages
                                )
                            } else if let Some(k) =
                                self.viewing.frame().filter(|_| idx == self.list.shown())
                            {
                                tf!(
                                    " frame {n}/{frames}",
                                    n = k + 1,
                                    frames = loaded.frames.len()
                                )
                            } else {
                                String::new()
                            };
                            ui.label(format!("( {w} x {h} ) {bytes} {position}{page}"));
                        }
                        None => {
                            ui.label(format!("{bytes} {position}"));
                        }
                    }
                    if self.status.grid != [1, 1] {
                        pane_map(ui, self.status.grid, self.status.pane);
                    }
                });
            });
        });
    }
}

/// The names whose thumbnails stay, for rows `rows` of the list as shown (`matches`, or all of
/// `files`) and a third of `max` rows on each side. It goes by rows rather than file indices,
/// because search matches are spread over the whole folder, and a range of indices would keep
/// almost every file.
pub(crate) fn thumbs_to_keep<'a>(
    files: &'a [OsString],
    matches: Option<&[usize]>,
    rows: std::ops::Range<usize>,
    max: usize,
) -> HashSet<&'a OsString> {
    let count = matches.map_or(files.len(), <[usize]>::len);
    let around = rows.start.saturating_sub(max / 3)..(rows.end + max / 3).min(count);
    around
        .map(|row| &files[matches.map_or(row, |m| m[row])])
        .collect()
}

/// Tells a screen reader what a file's row or cell is: an item of the list with the file's name,
/// and whether it is selected. The rows of names get this from egui's own widget.
pub(crate) fn speak_item(response: &egui::Response, marked: bool, name: &str) {
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ListItem, true, name));
    (response.ctx).accesskit_node_builder(response.id, |node| node.set_selected(marked));
}

/// The places along a row or column of items `size` long, one every `step`, that the span
/// `lo..hi` (from the first item's start) overlaps.
pub(crate) fn touched(lo: f32, hi: f32, step: f32, size: f32) -> std::ops::Range<usize> {
    let first = ((lo - size) / step).floor() + 1.0;
    first.max(0.0) as usize..(hi / step).ceil().max(0.0) as usize
}

#[cfg(test)]
mod tests {
    use super::Panel;

    /// A scroll to the shown image is made by every list of the frame after it was asked, the
    /// side panel and the grid both, and then it is done. One asked while a frame draws is made
    /// in that frame and in the next, so the lists drawn before it get it too.
    #[test]
    fn a_scroll_reaches_every_list_of_one_frame() {
        let mut panel = Panel::default();
        panel.start_frame(true);
        assert!(
            panel.scrolls(),
            "the first frame scrolls to the first image"
        );
        panel.start_frame(true);
        assert!(!panel.scrolls());
        panel.scroll_to_shown();
        assert!(panel.scrolls());
        panel.start_frame(true);
        assert!(panel.scrolls());
        panel.start_frame(true);
        assert!(!panel.scrolls());
        // Frames without a list (the list hidden, full screen) keep the scroll for the first
        // frame that draws one again.
        panel.scroll_to_shown();
        panel.start_frame(false);
        panel.start_frame(false);
        assert_eq!(panel.grid_cols, 1);
        panel.start_frame(true);
        assert!(panel.scrolls(), "the list scrolls once it is drawn again");
        panel.start_frame(true);
        assert!(!panel.scrolls());
    }

    /// G shows the grid in the image area and Enter an image from it. Esc goes back to the
    /// grid, G from there too, and a second G hides the grid. Esc without the grid keeps it off.
    #[test]
    fn the_image_area_grid_opens_and_goes_back() {
        let dir = std::env::temp_dir().join(format!("glimt-test-area-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = crate::support::test_support::app_on(&dir);
        use crate::ui::actions::Action::{Escape, ThumbnailGrid};
        app.run(&ctx, Escape);
        assert!(!app.panel.area_grid_on());
        app.run(&ctx, ThumbnailGrid);
        assert!(app.panel.area_grid_shown());
        app.panel.open_from_grid();
        assert!(app.panel.area_grid_on() && !app.panel.area_grid_shown());
        app.run(&ctx, Escape);
        assert!(app.panel.area_grid_shown());
        app.panel.open_from_grid();
        app.run(&ctx, ThumbnailGrid);
        assert!(app.panel.area_grid_shown());
        app.run(&ctx, ThumbnailGrid);
        assert!(!app.panel.area_grid_on());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Without file names a cell of the grid is as wide, and shorter by the name's line, so more
    /// rows fit. The toggle turns them off and on again.
    #[test]
    fn a_cell_without_its_name_is_shorter() {
        let dir = std::env::temp_dir().join(format!("glimt-test-names-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = crate::support::test_support::app_on(&dir);
        let named = app.cell();
        app.run(&ctx, crate::ui::actions::Action::GridNames);
        let bare = app.cell();
        assert_eq!(bare.x, named.x);
        assert_eq!(bare.y, app.thumb().y + 8.0);
        assert!(bare.y < named.y);
        app.run(&ctx, crate::ui::actions::Action::GridNames);
        assert_eq!(app.cell(), named);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A slideshow started from the grid in the image area shows the images, and Esc, which
    /// stops it, goes back to the grid.
    #[test]
    fn a_slideshow_from_the_grid_shows_the_images() {
        let dir = std::env::temp_dir().join(format!("glimt-test-area-show-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = crate::support::test_support::app_on(&dir);
        use crate::ui::actions::Action::{Escape, Slideshow, ThumbnailGrid};
        app.run(&ctx, ThumbnailGrid);
        app.run(&ctx, Slideshow);
        assert!(app.panel.area_grid_on() && !app.panel.area_grid_shown());
        app.run(&ctx, Escape);
        assert!(app.panel.area_grid_shown() && !app.slideshow.is_running());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// With cells 100 high every 110, a band from 50 to 230 touches rows 0, 1 and 2. One in the
    /// gap from 101 to 109 touches none, and one above the grid none.
    #[test]
    fn finds_the_cells_a_rectangle_touches() {
        use super::touched;
        assert_eq!(touched(50.0, 230.0, 110.0, 100.0), 0..3);
        assert_eq!(touched(101.0, 109.0, 110.0, 100.0), 1..1);
        assert_eq!(touched(-80.0, -10.0, 110.0, 100.0), 0..0);
    }

    /// Thumbnails are kept by rows of the list as shown. With search matches spread over a
    /// large folder, that is a few hundred names, not the whole folder.
    #[test]
    fn keeps_thumbnails_near_the_rows() {
        use std::ffi::OsString;
        let files: Vec<OsString> = (0..100_000)
            .map(|i| OsString::from(i.to_string()))
            .collect();
        let matches = [12, 99_000, 50_000, 7, 80_000];
        let keep = super::thumbs_to_keep(&files, Some(&matches), 0..2, 6);
        let mut names: Vec<_> = keep.into_iter().map(|n| n.to_str().unwrap()).collect();
        names.sort();
        assert_eq!(names, ["12", "50000", "7", "99000"]);
        assert_eq!(
            super::thumbs_to_keep(&files, None, 500..510, 600).len(),
            410
        );
    }

    /// A thumbnail forgotten while a worker makes it is dropped when it comes, because it
    /// shows the old contents, also when the answer to the new request comes first.
    #[test]
    fn a_thumbnail_forgotten_while_made_is_stale() {
        let ctx = eframe::egui::Context::default();
        let mut thumbs = super::Thumbs::new(&ctx, 0, &Default::default(), false);
        let name = std::ffi::OsStr::new("a.jpg");
        thumbs.ask(name, 0, "a.jpg".into());
        let old = thumbs.asked;
        thumbs.forget(name);
        thumbs.ask(name, 0, "a.jpg".into());
        let new = thumbs.asked;
        assert!(thumbs.answered(name, new), "the new answer is dropped");
        assert!(
            !thumbs.answered(name, old),
            "the old answer is shown after the new one"
        );
        // In the other order, the old answer neither shows nor ends the new request.
        thumbs.forget(name);
        thumbs.ask(name, 0, "a.jpg".into());
        assert!(!thumbs.answered(name, new));
        assert!(thumbs.answered(name, thumbs.asked));
    }
}
