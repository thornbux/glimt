//! The side panel: the folder list, the file list or icon grid with its thumbnails, dragging and the rubber band, and the status bar under it.

use crate::Action;
use crate::CELL_EXTRA;
use crate::DATE_WIDTH;
use crate::DETAILS_WIDTH;
use crate::LARGE_THUMBS;
use crate::MAX_THUMBS;
use crate::Pick;
use crate::Prompt;
use crate::Receiving;
use crate::SORTS;
use crate::THUMB_MARGIN;
use crate::THUMBS;
use crate::Types;
use crate::broken_mark;
use crate::collections;
use crate::config;
use crate::count;
use crate::is_video;
use crate::list_folder;
use crate::local_time;
use crate::middle_cut;
use crate::noted;
use crate::pane_map;
use crate::scroll_menu;
use crate::size_text;
use crate::sort_folders_from;
use crate::sort_folders_path;
use crate::tf;
use crate::theme;
use crate::thousands;
use crate::tool_button;
use crate::tr;
use eframe::egui;
use std::collections::HashSet;
use std::ffi::OsString;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
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

/// The names whose thumbnails stay: rows `rows` of the list as shown (`matches`, or all of
/// `files`) and a third of `max` rows on each side. Rows, not file indices: search matches are
/// spread over the whole folder, so a range of indices would keep almost every file.
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

/// The places along a row or column of items `size` long, one every `step`, that the span
/// `lo..hi` (from the first item's start) overlaps.
pub(crate) fn touched(lo: f32, hi: f32, step: f32, size: f32) -> std::ops::Range<usize> {
    let first = ((lo - size) / step).floor() + 1.0;
    first.max(0.0) as usize..(hi / step).ceil().max(0.0) as usize
}

impl crate::App {
    // ---- The file list, folders and thumbnails ----

    /// The thumbnail box in the file list, by the size chosen in Preferences.
    pub(crate) fn thumb(&self) -> egui::Vec2 {
        THUMBS[self.config.thumb_size.min(2) as usize]
    }

    /// Thumbnails from the freedesktop "large" cache when the box is wider than a "normal" one
    /// on the screen: at 150 % scaling even the smallest box is 144 pixels wide. Read again when
    /// the scale or the size changes.
    pub(crate) fn sync_thumb_source(&mut self, ppp: f32) {
        let large = self.thumb().x * ppp > 128.0;
        if LARGE_THUMBS.swap(large, Ordering::Relaxed) != large {
            self.thumbs_changed();
        }
    }

    /// The thumbnails look otherwise now (their size, source or colours): the made ones go, and
    /// those being made are marked stale, so the results are made again.
    pub(crate) fn thumbs_changed(&mut self) {
        self.thumbs.clear();
        self.stale_thumbs.extend(self.thumbs_pending.drain());
    }

    /// A cell of the icon grid: the thumbnail with its name below.
    pub(crate) fn cell(&self) -> egui::Vec2 {
        self.thumb() + CELL_EXTRA
    }

    pub(crate) fn set_thumb_size(&mut self, size: u8) {
        self.config.thumb_size = size;
        self.config.save();
        self.thumbs_changed();
        self.scroll_to_cur = true;
    }

    /// Thumbnails in the file list, or none.
    pub(crate) fn set_thumbnails(&mut self, on: bool) {
        self.config.thumbnails = on;
        self.config.save();
        if !on {
            self.thumbs.clear();
            self.thumbs_pending.clear();
        }
        self.scroll_to_cur = true;
    }

    /// List the images of all subfolders too, or only the folder's own.
    pub(crate) fn set_subfolders(&mut self, ctx: &egui::Context, on: bool) {
        self.config.show_subfolders = on;
        self.config.save();
        self.recursive = on;
        self.list_parts = on;
        self.list(ctx);
    }

    pub(crate) fn receive_thumbs(&mut self, ctx: &egui::Context) {
        while let Ok((path, result)) = self.thumbs_rx.try_recv() {
            // The name as the list has it: with its folders in a slideshow with subfolders.
            let Ok(name) = path.strip_prefix(&self.dir).map(Path::as_os_str) else {
                continue;
            };
            self.thumbs_pending.remove(name);
            // Made before a setting changed its look: asked for again, in the new one.
            if self.stale_thumbs.remove(name) {
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
                self.thumbs.insert(name.to_owned(), tex);
            }
        }
    }

    /// Drop thumbnails far from view once there are many, so memory stays flat in huge folders.
    /// Over the budget, drop the thumbnails of all but the rows `rows` of the list as shown (the
    /// search matches, or all files) and those near them.
    pub(crate) fn trim_thumbs(&mut self, rows: std::ops::Range<usize>) {
        // Big thumbnails take four times the memory: keep a quarter as many.
        let max = if LARGE_THUMBS.load(Ordering::Relaxed) {
            MAX_THUMBS / 4
        } else {
            MAX_THUMBS
        };
        if self.thumbs.len() <= max {
            return;
        }
        let mut keep = thumbs_to_keep(&self.list.files, self.list.matches.as_deref(), rows, max);
        keep.extend(
            self.list
                .pan_thumbs
                .iter()
                .filter_map(|&i| self.list.files.get(i)),
        );
        self.thumbs.retain(|name, _| keep.contains(name));
    }

    /// A file list row with a thumbnail; true when clicked.
    pub(crate) fn file_row(
        &mut self,
        ui: &mut egui::Ui,
        i: usize,
        height: f32,
        marked: bool,
    ) -> egui::Response {
        let size = egui::vec2(ui.available_width(), height);
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
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
        let thumbs = self.config.thumbnails;
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
            // Size and date columns at the right, as in Geeqie; the name is cut off before them.
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
        // Found broken by Check Images: a warning sign at the right.
        if self.broken_files.contains(&self.list.files[i]) {
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

    /// A cell of the thumbnail grid: the thumbnail with the name below, cut off to fit.
    pub(crate) fn file_cell(
        &mut self,
        ui: &mut egui::Ui,
        i: usize,
        rect: egui::Rect,
        marked: bool,
    ) -> egui::Response {
        let response = ui.interact(rect, ui.scope_id().with(("cell", i)), egui::Sense::click());
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
        let font = egui::TextStyle::Small.resolve(ui.style());
        let name = self.name(i).into_owned();
        let mut job = egui::text::LayoutJob::simple_singleline(name, font, color);
        job.wrap = egui::text::TextWrapping::truncate_at_width(self.cell().x - 6.0);
        let galley = ui.painter().layout_job(job);
        let at = egui::pos2(
            rect.center().x - galley.size().x / 2.0,
            frame.bottom() + 4.0,
        );
        ui.painter().galley(at, galley, color);
        // Found broken by Check Images: the warning sign on the thumbnail's top left.
        if self.broken_files.contains(&self.list.files[i]) {
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
        response.on_hover_text_at_pointer(match self.collection {
            Some(_) => self.path(i).display().to_string(),
            None => self.name(i).into_owned(),
        })
    }

    /// Thumbnail `i` fitted into `frame`, or a request for it.
    pub(crate) fn paint_thumb(&mut self, ui: &egui::Ui, i: usize, frame: egui::Rect) {
        let name = &self.list.files[i];
        match self.thumbs.get(name) {
            Some(Some(tex)) => {
                // Fit the box, but never enlarge a small image. One the cache cut down to its
                // size (256 pixels) does fill a box larger on a scaled screen.
                let px = tex.size_vec2() / ui.ctx().pixels_per_point();
                let cut = tex.size().into_iter().max() >= Some(256);
                // Fit the frame given: a narrow calendar day has less room than a list row.
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
                if self.thumbs_pending.insert(name.clone()) {
                    let _ = self.thumb_jobs.send((i, self.path(i)));
                }
            }
        }
    }

    /// The film strip along the bottom of `area` in full screen, shown while the pointer is
    /// near the bottom: thumbnails of the images before and after the shown one, which is
    /// outlined. Returns the image clicked.
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
            self.thumb_view.lock().unwrap()[0] = view;
            let row = match self.list.matches.as_deref() {
                Some(m) => self.cur_row(m).unwrap_or(0),
                None => self.list.cur,
            };
            let k = k as usize;
            self.trim_thumbs(row.saturating_sub(k)..row + k + 1);
        }
        ui.painter()
            .rect_filled(strip, 0.0, egui::Color32::from_black_alpha(190));
        // The shown image in the middle, where it is when there are images on both sides.
        let at = shown.iter().position(|&i| i == self.list.cur).unwrap_or(0) as f32;
        let x0 = strip.center().x - (at + 0.5) * cell.x;
        let mut clicked = None;
        for (n, &i) in shown.iter().enumerate() {
            let min = egui::pos2(x0 + n as f32 * cell.x, strip.top() + 8.0);
            let frame = egui::Rect::from_min_size(min, cell).shrink(4.0);
            self.paint_thumb(ui, i, frame);
            if i == self.list.cur {
                let stroke = egui::Stroke::new(2.0, ui.visuals().selection.bg_fill);
                ui.painter()
                    .rect_stroke(frame.expand(2.0), 3.0, stroke, egui::StrokeKind::Outside);
            }
            let id = ui.scope_id().with(("strip", i));
            if ui.interact(frame, id, egui::Sense::click()).clicked() {
                clicked = Some(i);
            }
        }
        clicked
    }

    /// Size and modification time of file `i`, from the cache or one stat.
    /// shortcut: stat in the UI thread, once per visible file; move to a thread if network
    /// folders make scrolling stutter.
    pub(crate) fn file_details(&mut self, i: usize) -> (u64, i64) {
        if let Some(&d) = self.list.details.get(&self.list.files[i]) {
            return d;
        }
        let d = std::fs::metadata(self.path(i)).map_or((0, 0), |m| {
            use std::os::unix::fs::MetadataExt;
            (m.len(), m.mtime())
        });
        // Bounded: scrolling through a folder of 500,000 would keep some 50 MB. Only the rows on
        // screen are read again.
        if self.list.details.len() >= 20_000 {
            self.list.details.clear();
        }
        self.list.details.insert(self.list.files[i].clone(), d);
        d
    }

    /// Folders as a tree from /, opened up to the current folder. Subfolders are read in the
    /// background when a folder is opened in the tree.
    pub(crate) fn folder_tree(&mut self, ui: &mut egui::Ui, open_dir: &mut Option<PathBuf>) {
        while let Ok((dir, subdirs)) = self.tree_rx.try_recv() {
            self.tree.insert(dir, Some(subdirs));
        }
        // Opened up to the folder shown when that changes.
        let follow = self.tree_shown != self.dir;
        if follow {
            self.tree_open
                .extend(self.dir.ancestors().map(Path::to_path_buf));
        }
        // The open folders as rows, so only those on screen are drawn: a folder of 100,000
        // folders opened. An empty path is a folder still being read.
        // shortcut: the rows are made again each frame, a lookup for each; keep them until the
        // tree changes if trees of millions of folders are opened.
        let tree = std::mem::take(&mut self.tree);
        let mut rows: Vec<(usize, &Path)> = Vec::new();
        let mut unread = Vec::new();
        let mut stack = vec![(0, Path::new("/"))];
        while let Some((depth, path)) = stack.pop() {
            rows.push((depth, path));
            if !self.tree_open.contains(path) {
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
        if follow && let Some(i) = rows.iter().position(|r| r.1 == self.dir) {
            // A row and the space below it, as show_rows lays them out.
            let step = row_h + ui.spacing().item_spacing.y;
            let top = i as f32 * step - ui.available_height() / 2.0;
            area = area.vertical_scroll_offset(top.max(0.0));
            self.tree_shown = self.dir.clone();
        }
        let mut toggled = None;
        area.show_rows(ui, row_h, rows.len(), |ui, range| {
            for &(depth, path) in &rows[range] {
                ui.horizontal(|ui| {
                    ui.add_space(depth as f32 * ui.spacing().indent);
                    if path.as_os_str().is_empty() {
                        return drop(ui.label("…"));
                    }
                    let open = self.tree_open.contains(path);
                    let caret = match open {
                        true => theme::icon::CARET_DOWN,
                        false => theme::icon::CARET_RIGHT,
                    };
                    let name = path.file_name().map_or("/".into(), |n| n.to_string_lossy());
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
                    let label = ui.selectable_label(path == self.dir, name.as_ref());
                    let label = noted(&label, &name);
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
        self.tree = tree;
        if let Some(path) = toggled
            && !self.tree_open.remove(&path)
        {
            self.tree_open.insert(path);
        }
        let folders = Types {
            hidden: self.config.show_hidden,
            ..Types::NONE
        };
        for dir in unread {
            self.tree.insert(dir.clone(), None);
            let (tx, ctx) = (self.tree_tx.clone(), ui.ctx().clone());
            std::thread::spawn(move || {
                let subdirs = list_folder(&dir, folders).0;
                let _ = tx.send((dir, subdirs));
                ctx.request_repaint();
            });
        }
    }

    /// Right-click on a folder in the list or the tree: rename it, or move it to the trash.
    pub(crate) fn folder_menu(&mut self, response: &egui::Response, dir: &Path) {
        response.context_menu(|ui| {
            // Its images in the list together with those shown now.
            let shown = self.folders.contains(&dir.to_path_buf()) || dir == self.dir;
            if !shown
                && self.collection.is_none()
                && ui.button(tr("Show Together with These Images")).clicked()
            {
                let mut all = match self.folders.is_empty() {
                    true => vec![self.dir.clone()],
                    false => self.folders.clone(),
                };
                all.push(dir.to_path_buf());
                self.together = Some(all);
            }
            if ui.button(tr("Rename Folder…")).clicked() {
                let name = dir.file_name().unwrap_or_default().to_string_lossy();
                self.prompt = Some((Prompt::RenameFolder, name.into_owned()));
                self.folder_target = Some(dir.to_path_buf());
            }
            if ui.button(tr("Move Folder to Trash…")).clicked() {
                self.folder_target = Some(dir.to_path_buf());
                self.confirm_folder = true;
            }
        });
    }

    /// The sort manager (Shift+S), as in Geeqie: a click on a folder copies or moves the selected
    /// files, or the shown one, there.
    pub(crate) fn sort_manager(&mut self, ui: &mut egui::Ui) {
        let mut remove = None;
        egui::Panel::right("sort manager")
            .resizable(true)
            .default_size(200.0)
            .show(ui, |ui| {
                ui.strong(tr("Sort Manager"));
                ui.horizontal(|ui| {
                    let c = &mut self.config;
                    let changed = ui
                        .radio_value(&mut c.sort_move, false, tr("Copy"))
                        .changed()
                        | ui.radio_value(&mut c.sort_move, true, tr("Move")).changed();
                    if changed {
                        c.save();
                    }
                });
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (k, dir) in self.sort_folders.clone().into_iter().enumerate() {
                        ui.horizontal(|ui| {
                            if ui
                                .small_button(theme::icon::CLOSE)
                                .on_hover_text(tr("Remove"))
                                .clicked()
                            {
                                remove = Some(k);
                            }
                            let name = dir.file_name().unwrap_or(dir.as_os_str());
                            let here = dir == self.dir;
                            let button = ui.add_enabled(
                                !here && !self.list.files.is_empty(),
                                egui::Button::new(name.to_string_lossy()),
                            );
                            if button.on_hover_text(dir.display().to_string()).clicked() {
                                let paths = self.targets().iter().map(|&i| self.path(i)).collect();
                                self.transfer(ui.ctx(), paths, dir, !self.config.sort_move);
                            }
                        });
                    }
                    ui.horizontal(|ui| {
                        if ui.button(tr("Add This Folder")).clicked() {
                            self.add_sort_folder(self.dir.clone());
                        }
                        if ui.button(tr("Add…")).clicked() {
                            self.pick(ui.ctx(), true, Pick::SortFolder);
                        }
                    });
                });
            });
        if let Some(k) = remove {
            let dir = self.sort_folders[k].clone();
            self.change_sort_folders(None, Some(&dir));
        }
    }

    pub(crate) fn add_sort_folder(&mut self, dir: PathBuf) {
        if !self.sort_folders.contains(&dir) {
            self.change_sort_folders(Some(&dir), None);
        }
    }

    /// Add a folder to the sort manager or take one out: the change goes to the file as it is
    /// now, so another window's folders stay, and is written whole or not at all.
    pub(crate) fn change_sort_folders(&mut self, add: Option<&Path>, remove: Option<&Path>) {
        if config::locked() {
            return self.notice = Some(tr("The settings are locked (--lock-settings)").into());
        }
        let Some(path) = sort_folders_path() else {
            return self.notice =
                Some(tr("Could not save the sort manager's folders: no home folder").into());
        };
        // This change on the file as it is now, so another window's folders stay; one that
        // cannot be read is not written over.
        let change = |old: &[u8]| {
            let dirs = config::sort_folders_changed(sort_folders_from(old), add, remove);
            (config::sort_folders_text(&dirs), dirs)
        };
        match config::edit_shared(&path, change) {
            Ok(dirs) => self.sort_folders = dirs,
            Err(e) => self.notice = Some(tf!("Could not save the sort manager's folders: {e}", e)),
        }
    }

    /// The file types the list shows, from the Types menu.
    /// The type of a file opened directly is listed too, in its folder.
    pub(crate) fn types(&self) -> Types {
        let o = self.opened_type;
        Types {
            images: self.config.list_images || o.images,
            raw: self.config.list_raw || o.raw,
            videos: self.config.videos || o.videos,
            hidden: self.config.show_hidden || o.hidden,
        }
    }

    /// The file types to list, as the file classes of Geeqie's file filter; beside the sort menu.
    pub(crate) fn types_menu(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        let c = &mut self.config;
        let mut all = vec![(tr("Images"), &mut c.list_images), ("RAW", &mut c.list_raw)];
        if cfg!(feature = "video") {
            all.push((tr("Videos"), &mut c.videos));
        }
        let on: Vec<&str> = all.iter().filter(|t| *t.1).map(|t| t.0).collect();
        let title = match on.len() {
            n if n == all.len() => "All types".to_owned(),
            0 => "No types".to_owned(),
            _ => on.join(", "),
        };
        let mut changed = false;
        ui.menu_button(format!("{title} {}", theme::icon::CARET_DOWN), |ui| {
            for (label, on) in all {
                changed |= ui.checkbox(on, label).changed();
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
        let title = format!("{} {}", self.config.sort.row().2, theme::icon::CARET_DOWN);
        scroll_menu(ui, title, |ui| {
            for (sort, _, label) in SORTS {
                if ui
                    .selectable_label(self.config.sort == sort, label)
                    .clicked()
                {
                    self.config.sort = sort;
                    changed = true;
                }
            }
            ui.separator();
            changed |= ui
                .checkbox(&mut self.config.sort_ascending, tr("Ascending"))
                .changed();
            changed |= ui
                .checkbox(&mut self.config.sort_case, tr("Case"))
                .changed();
        });
        if changed {
            self.config.save();
            // A chosen order replaces a shuffled one (kept after a random slideshow).
            self.shuffled = false;
            self.sort_collection = self.collection.is_some();
            self.listed_as = None;
            self.list(ctx);
        }
    }

    /// The folder path above the folder list, as in Geeqie; Enter opens what was typed.
    pub(crate) fn path_field(&mut self, ui: &mut egui::Ui) {
        let edit = egui::TextEdit::singleline(&mut self.path_text).desired_width(f32::INFINITY);
        let response = ui.add(edit);
        if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            let text = self.path_text.trim();
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
            let shown = self.collection.as_ref().unwrap_or(&self.dir);
            self.path_text = match self.folders.is_empty() {
                true => shown.display().to_string(),
                // Several folders: each of them.
                false => (self.folders.iter())
                    .map(|f| f.display().to_string())
                    .collect::<Vec<_>>()
                    .join(" + "),
            };
        }
    }

    /// Geeqie's toolbar: navigation, zoom, thumbnails, slideshow and preferences.
    pub(crate) fn toolbar(&mut self, ui: &mut egui::Ui, act: &mut Option<Action>) {
        let size = [14.0, 20.0, 28.0][self.config.toolbar_icons.min(2)];
        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.style_mut().override_font_id = Some(egui::FontId::proportional(size));
            ui.horizontal(|ui| {
                tool_button(ui, act, theme::icon::BACK, Action::Back, false);
                tool_button(ui, act, theme::icon::FORWARD, Action::Forward, false);
                tool_button(ui, act, theme::icon::UP, Action::Up, false);
                tool_button(ui, act, theme::icon::HOME, Action::Home, false);
                tool_button(ui, act, theme::icon::REFRESH, Action::Refresh, false);
                ui.separator();
                tool_button(ui, act, theme::icon::ZOOM_IN, Action::ZoomIn, false);
                tool_button(ui, act, theme::icon::ZOOM_OUT, Action::ZoomOut, false);
                tool_button(ui, act, "1:1", Action::Zoom1To1, false);
                tool_button(
                    ui,
                    act,
                    theme::icon::FIT,
                    Action::ZoomFit,
                    self.zoom.is_none(),
                );
                ui.separator();
                tool_button(
                    ui,
                    act,
                    theme::icon::THUMBNAILS,
                    Action::Thumbnails,
                    self.config.thumbnails,
                );
                let show = if self.slideshow.is_some() {
                    theme::icon::STOP
                } else {
                    theme::icon::PLAY
                };
                tool_button(ui, act, show, Action::Slideshow, self.slideshow.is_some());
                tool_button(ui, act, theme::icon::SETTINGS, Action::Preferences, false);
            });
        });
    }

    /// The left side as in Geeqie: the path, the folders at the top, the search and the files
    /// below; then what a click there asked for.
    pub(crate) fn side_panel(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        let (mut open_dir, mut go_to, mut list_act) = (None, None, None);
        // Room for a name next to the size and date columns, and the thumbnail before it.
        let thumb = if self.config.thumbnails {
            self.thumb().x + 10.0
        } else {
            0.0
        };
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
                self.file_list(ctx, ui, &mut go_to, &mut list_act);
            });
        self.scroll_to_cur = false;
        if let Some(d) = open_dir {
            self.visit(ctx, d);
        } else if let Some(i) = go_to {
            self.go(ctx, i);
        }
        if let Some(action) = list_act {
            self.run(ctx, action);
        }
    }

    /// The folder list: ⬆ .. for the parent folder, then the subfolders.
    pub(crate) fn folder_list(&mut self, ui: &mut egui::Ui, open_dir: &mut Option<PathBuf>) {
        // Row 0 is ⬆ .. (when there is a parent folder), the rest are subfolders.
        let up = self.dir.parent().map(Path::to_path_buf);
        let rows = up.is_some() as usize + self.subdirs.len();
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
                            let d = &self.subdirs[row - up.is_some() as usize];
                            let name = d.file_name().unwrap_or_default();
                            (
                                format!("{}  {}", theme::icon::FOLDER, name.to_string_lossy()),
                                d,
                            )
                        }
                    };
                    let target = target.clone();
                    let response = ui.selectable_label(false, label);
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
    pub(crate) fn file_list(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        go_to: &mut Option<usize>,
        list_act: &mut Option<Action>,
    ) {
        // The rows shown: the search matches, or all files.
        let count = self
            .list
            .matches
            .as_ref()
            .map_or(self.list.files.len(), Vec::len);
        let row_of = |i: usize, m: &Option<Vec<usize>>| match m {
            Some(m) => m.iter().position(|&x| x == i),
            None => Some(i),
        };
        // Only visible rows are drawn, so the list is as fast with 10 files as with 300,000.
        let icons = self.config.icons;
        let thumbs = self.config.thumbnails || icons;
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
        self.grid_cols = cols;
        let mut area = egui::ScrollArea::vertical()
            .id_salt("files")
            .auto_shrink(false);
        if self.scroll_to_cur
            && let Some(row) = row_of(self.list.cur, &self.list.matches).map(|r| r / cols)
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
            let shown: Vec<usize> = match &self.list.matches {
                Some(m) => m[rows.clone()].to_vec(),
                None => rows.clone().collect(),
            };

            if thumbs && !shown.is_empty() {
                // The rows shown and some around them, as files: kept being made while near.
                let near =
                    rows.start.saturating_sub(THUMB_MARGIN)..(rows.end + THUMB_MARGIN).min(count);
                let mut view: Vec<usize> = match &self.list.matches {
                    Some(m) => m[near].to_vec(),
                    None => near.collect(),
                };
                view.sort_unstable();
                self.thumb_view.lock().unwrap()[0] = view;
                self.list_rows = rows.clone();
                self.trim_thumbs(rows);
            }
            let mut line = egui::Rect::NOTHING;
            let pointer = ctx.input(|i| i.pointer.interact_pos());
            if let Some((_, to)) = &mut self.list.dragging {
                *to = None;
                // Near the top or bottom edge, the list scrolls on.
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
            let mut cells = Vec::new();
            for (k, i) in shown.into_iter().enumerate() {
                let marked = i == self.list.cur || self.list.selected.contains(&i);
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
                    self.file_row(ui, i, row_h, marked)
                } else {
                    let name = self.name(i);
                    let marks = self.mark_text(i);
                    if marks.is_empty() {
                        ui.selectable_label(marked, name)
                    } else {
                        ui.selectable_label(marked, format!("{name}   {marks}"))
                    }
                };
                if let Some(i) = self.row_clicked(ctx, &row, i) {
                    *go_to = Some(i);
                }
                row.context_menu(|ui| self.file_menu(ui, list_act));
                if self.collection.is_some() {
                    self.drag_row(ui, &row, i, icons, pointer);
                }
            }
            if icons {
                self.drag_rubber_band(ui, &cells, row_h, first_row);
            }
            // Dropped: the dragged images before the row the line was at.
            if ctx.input(|i| i.pointer.any_released())
                && let Some((picked, Some(to))) = self.list.dragging.take()
            {
                let order = collections::dropped(self.list.files.len(), &picked, to);
                self.move_in_collection(ctx, picked, order);
            } else if !ctx.input(|i| i.pointer.any_down()) {
                self.list.dragging = None;
            }
        });
    }

    /// Selecting in the icon grid by dragging a rectangle, as in a file manager: every image
    /// it touches, added to the selection with Ctrl or Shift. It starts anywhere but in a
    /// collection, where dragging an image moves it, from the space between them; it counts once
    /// the pointer has moved a little, so a click stays a click. `cells` are the shown ones.
    pub(crate) fn drag_rubber_band(
        &mut self,
        ui: &egui::Ui,
        cells: &[(usize, egui::Rect)],
        row_h: f32,
        first_row: usize,
    ) {
        let ctx = ui.ctx().clone();
        // The grid's top, above the rows drawn: the rectangle is kept in the grid, so it moves
        // with the images as they scroll.
        let pitch = row_h + ui.spacing().item_spacing.y;
        let origin = ui.max_rect().min - egui::vec2(0.0, first_row as f32 * pitch);
        let (cell, gap) = (self.cell(), ui.spacing().item_spacing.x);
        let cols = self.grid_cols.max(1);
        let count = self
            .list
            .matches
            .as_ref()
            .map_or(self.list.files.len(), Vec::len);
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
            && self.list.rubber_band.is_none()
            && self.list.dragging.is_none()
            && !self.dialog_open()
            // Not on the scroll bar, which is dragged too.
            && let Some(p) = pos.filter(|p| clip.contains(*p))
            && p.x < clip.right() - ui.spacing().scroll.allocated_width()
            && (self.collection.is_none() || !cells.iter().any(|c| c.1.contains(p)))
        {
            let before = if add {
                self.list.selected.clone()
            } else {
                Default::default()
            };
            self.list.rubber_band = Some(RubberBand {
                start: p - origin,
                before,
                moved: false,
                touched: None,
            });
        }
        let Some(RubberBand {
            start,
            before,
            moved,
            touched: last,
        }) = &mut self.list.rubber_band
        else {
            return;
        };
        let start = origin + *start;
        let Some(p) = pos.filter(|_| down) else {
            // Released: the size of what is selected, once.
            if std::mem::take(moved) {
                self.selection_changed(&ctx);
            }
            self.list.rubber_band = None;
            return;
        };
        // Something else is being dragged (the panel's edge, a scroll bar): not a rectangle.
        if !*moved && ctx.dragged_id().is_some() {
            self.list.rubber_band = None;
            return;
        }
        *moved |= p.distance(start) > 6.0;
        if !*moved {
            return;
        }
        let band = egui::Rect::from_two_pos(start, p);
        // Every cell under it, also those scrolled out of view, from where they are in the grid.
        let rel = band.translate(-origin.to_vec2());
        let rows = touched(rel.top(), rel.bottom(), pitch, row_h);
        let columns = touched(rel.left(), rel.right(), cell.x + gap, cell.x);
        // Made again only when it touches other cells: the selection before it may be large
        // (Ctrl+A in a big folder, then Ctrl and a rectangle), and frames come many a second.
        let touches = [rows.start, rows.end, columns.start, columns.end];
        if *last != Some(touches) {
            *last = Some(touches);
            let mut selected = before.clone();
            for r in rows {
                for c in columns.start..columns.end.min(cols) {
                    let k = r * cols + c;
                    if k < count {
                        selected.insert(self.list.matches.as_ref().map_or(k, |m| m[k]));
                    }
                }
            }
            self.list.selected = selected;
        }
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

    /// Row `i` of a collection dragged to another place: the selection when it is in it, else
    /// the row; a line in the accent colour shows where the images go.
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
            let picked = if self.list.selected.contains(&i) {
                self.list.selected.iter().copied().collect()
            } else {
                HashSet::from([i])
            };
            self.list.dragging = Some((picked, None));
        }
        let Some((_, to)) = &mut self.list.dragging else {
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

    /// A click on row `i`: Ctrl and Shift change the selection, a plain click gives the image to
    /// show; a right-click outside the selection selects just that row.
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
                // Ctrl+click: toggle selection (the shown image is included the first time).
                if self.list.selected.is_empty() {
                    self.list.selected.insert(self.list.cur);
                }
                if !self.list.selected.remove(&i) {
                    self.list.selected.insert(i);
                }
                self.list.anchor = Some(i);
                self.selection_changed(ctx);
            } else if m.shift {
                // Shift+click: select the rows from where the selection started.
                let anchor = *self.list.anchor.get_or_insert(self.list.cur);
                self.list.selected.clear();
                self.select_range(anchor, i);
                self.selection_changed(ctx);
            } else {
                self.list.selected.clear();
                self.selected_size = None;
                self.selected_rx = Receiving::none();
                self.list.anchor = None;
                go_to = Some(i);
            }
        }
        // Right-click on a file outside the selection selects just it, as in
        // a file manager; the menu acts on the selection.
        if row.secondary_clicked()
            && !self.list.selected.contains(&i)
            && (i != self.list.cur || !self.list.selected.is_empty())
        {
            self.list.selected.clear();
            self.list.selected.insert(i);
            self.list.anchor = Some(i);
            self.selection_changed(ctx);
        }
        go_to
    }

    /// The status bar as in Geeqie: the counts and sizes on the left, the shown image (or the pane
    /// under the pointer) on the right.
    pub(crate) fn status_bar(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                let n = self.list.files.len();
                let size = size_text(self.size_rx.is_none().then_some(self.total_size));
                let mut left = match &self.list.matches {
                    _ if self.listing_rx.is_some() => tr("Reading folder…").to_owned(),
                    // A search shows the matches: count and size of what the list shows.
                    Some(found) => {
                        let size = size_text(
                            (self.match_rx.is_none() && self.match_size_due.is_none())
                                .then_some(self.list.match_size),
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
                if !self.list.selected.is_empty() {
                    let m = thousands(self.list.selected.len());
                    let size = size_text(self.selected_size);
                    left += &tf!(", {m} selected ({size})", m, size);
                }
                self.sort_menu(ctx, ui);
                self.types_menu(ctx, ui);
                ui.separator();
                ui.label(left);
                if let Some(job) = self.file_jobs.first() {
                    ui.separator();
                    let more = match self.file_jobs.len() - 1 {
                        0 => String::new(),
                        n => tf!(", and {n} more", n),
                    };
                    ui.label(tf!(
                        "{doing} {done} of {total}…{more}",
                        doing = tr(job.doing),
                        done = thousands(job.done.load(Ordering::Relaxed)),
                        total = thousands(job.total),
                        more
                    ));
                    if ui.small_button(tr("Stop")).clicked() {
                        self.file_jobs
                            .iter()
                            .for_each(|j| j.stop.store(true, Ordering::Relaxed));
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if n == 0 {
                        return;
                    }
                    // As in Geeqie: "( 4000 x 3000 ) 2,345,678 bytes [3/16]", then the zoom;
                    // with several panes, of the one under the pointer.
                    let (idx, scale) = (self.pane_info.get(self.status_pane).copied())
                        .filter(|p| p.0 < n)
                        .unwrap_or((self.list.cur, Some(self.last_scale)));
                    let path = self.path(idx);
                    if self.cur_size.as_ref().is_none_or(|c| c.0 != path) {
                        // shortcut: one stat in the UI thread per image change; move it to
                        // a thread if slow network folders make browsing stutter.
                        let bytes = std::fs::metadata(&path).map_or(0, |m| m.len());
                        self.cur_size = Some((path, bytes));
                    }
                    let bytes = self.cur_size.as_ref().map_or(0, |c| c.1);
                    let place = if idx == self.list.cur {
                        self.place()
                    } else {
                        self.place_of(idx)
                    };
                    let bytes = tf!("{n} bytes", n = thousands(bytes as usize));
                    match self.list.slots.get(&idx) {
                        Some(slot) => {
                            let [w, h] = slot.orig;
                            if let Some(scale) = scale {
                                ui.label(format!("{} %", (scale * 100.0).round()));
                                ui.separator();
                            }
                            let page = if slot.pages > 1 && idx == self.list.cur {
                                tf!(
                                    " page {n}/{pages}",
                                    n = self.viewing.page + 1,
                                    pages = slot.pages
                                )
                            } else if let Some(k) =
                                self.viewing.frame.filter(|_| idx == self.list.cur)
                            {
                                tf!(" frame {n}/{frames}", n = k + 1, frames = slot.anim.len())
                            } else {
                                String::new()
                            };
                            ui.label(format!("( {w} x {h} ) {bytes} {place}{page}"));
                        }
                        None => {
                            ui.label(format!("{bytes} {place}"));
                        }
                    }
                    if self.status_grid != [1, 1] {
                        pane_map(ui, self.status_grid, self.status_pane);
                    }
                });
            });
        });
    }
}
