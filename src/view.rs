//! The image area: decoding the shown image and its neighbours, zoom and fit, panes and Compare, painting, the wheel.

use crate::Action;
use crate::Decoded;
use crate::KIOSK_ACTIONS;
use crate::LAYOUTS;
use crate::Slot;
use crate::app_id;
use crate::decode;
use crate::fill_template;
use crate::histogram;
use crate::is_video;
use crate::malloc_trim;
use crate::menus::iconed;
use crate::menus::menu_item;
use crate::menus::scroll_menu;
use crate::noted;
use crate::survive;
use crate::theme;
use crate::tr;
use eframe::egui;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
/// Where an image of `orig` is drawn in `rect`, and its scale: at the `zoom` and pan given, else
/// fitted in fit `mode`, centred, and larger than its size only with `enlarge`.
pub(crate) fn placed(
    rect: egui::Rect,
    orig: [usize; 2],
    ppp: f32,
    (enlarge, mode): (bool, u8),
    zoom: Option<(f32, egui::Vec2)>,
) -> (egui::Rect, f32) {
    let size = egui::vec2(orig[0] as f32, orig[1] as f32);
    let (scale, pan) = zoom.unwrap_or_else(|| {
        let fit = fit_scale(rect.size() * ppp, size.into(), mode);
        (if enlarge { fit } else { fit.min(1.0) }, egui::Vec2::ZERO)
    });
    let at = egui::Rect::from_center_size(rect.center() + pan, size * scale / ppp);
    (at, scale)
}

/// A texture shown and its image's size, which a fade goes from.
pub(crate) type Shown = (egui::TextureHandle, [usize; 2]);

/// `tex` drawn in `rect` (its size before turning), turned by quarter `turns` and mirrored as
/// the view is, tinted.
pub(crate) fn paint_turned(
    ui: &egui::Ui,
    tex: &egui::TextureHandle,
    rect: egui::Rect,
    (turns, mirrored): (u8, bool),
    tint: egui::Color32,
) {
    let uv = match mirrored {
        true => egui::Rect::from_min_max(egui::pos2(1.0, 0.0), egui::pos2(0.0, 1.0)),
        false => egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
    };
    let angle = turns as f32 * std::f32::consts::FRAC_PI_2;
    egui::Image::new(tex)
        .uv(uv)
        .rotate(angle, egui::Vec2::splat(0.5))
        .tint(tint)
        .paint_at(ui, rect);
}

/// A checkerboard under the image at `rect`, turned as the image is, for one that is
/// transparent: one quad of a repeating 2×2 texture, 8-point squares at any size.
pub(crate) fn paint_checker(p: &egui::Painter, rect: egui::Rect, turns: u8, tint: egui::Color32) {
    let ctx = p.ctx();
    let id = egui::Id::unique("checkerboard");
    let tex = ctx.data(|d| d.get_temp::<egui::TextureHandle>(id));
    let tex = tex.unwrap_or_else(|| {
        let (a, b) = (egui::Color32::from_gray(102), egui::Color32::from_gray(153));
        let img = egui::ColorImage::new([2, 2], vec![a, b, b, a]);
        let tex = ctx.load_texture("checkerboard", img, egui::TextureOptions::NEAREST_REPEAT);
        ctx.data_mut(|d| d.insert_temp(id, tex.clone()));
        tex
    });
    let size = match turns % 2 {
        1 => rect.size().yx(),
        _ => rect.size(),
    };
    let rect = egui::Rect::from_center_size(rect.center(), size);
    let uv = egui::Rect::from_min_size(egui::Pos2::ZERO, size / 16.0);
    p.image(tex.id(), rect, uv, tint);
}

/// Faded to `alpha` (0 to 1), as a tint. A fade draws the image before at 1 - t⁴ under the new
/// one at t: the sum stays near full brightness, where 1 - t would darken the middle.
pub(crate) fn alpha(a: f32) -> egui::Color32 {
    egui::Color32::from_white_alpha((a.clamp(0.0, 1.0) * 255.0) as u8)
}

/// The image area of the last frame, which the projector follows: where it was (points), the
/// screen's pixels per point, the scale (screen pixels per image pixel), the offset of a zoomed
/// image, whether it was zoomed, and the pointer over it, from the image's centre.
#[derive(Clone, Copy)]
pub(crate) struct View {
    area: egui::Rect,
    ppp: f32,
    scale: f32,
    pan: egui::Vec2,
    zoomed: bool,
    pointer: Option<egui::Vec2>,
}

/// The scale (screen pixels per image pixel) at which an image of `size` fits `view`, both in
/// pixels, in fit `mode`: 0 the whole image, 1 its width, 2 its height, 3 filling the view.
pub(crate) fn fit_scale(view: egui::Vec2, size: [f32; 2], mode: u8) -> f32 {
    let (x, y) = (view.x / size[0], view.y / size[1]);
    match mode {
        1 => x,
        2 => y,
        3 => x.max(y),
        _ => x.min(y),
    }
}

/// Whether a texture of `tex` is too small to show an image of `orig` at `scale` in a view of
/// `view` pixels (all unturned), zoomed: then full resolution is loaded, once. The decoders fit an image into a box of whole pixels: the side
/// that fills the view comes out within a pixel or so of it, and the other is worked out from
/// it, so one pixel there is `orig` side to side here (4000×2657 into a box 1328 high is 1998
/// wide, where 1328.9 high shows it 2000.6 wide). Short by more than that on any side: decoded
/// again, which then gives a texture within it, so never again and again. A strip of 10000×5
/// grown from 1920 to 3840 wide is too small by its width, as its height fills nothing.
pub(crate) fn too_small(tex: [usize; 2], orig: [usize; 2], scale: f32, view: [f32; 2]) -> bool {
    (0..2).any(|i| {
        let j = 1 - i;
        let [o, other] = [orig[i] as f32, orig[j] as f32];
        let fills = other * scale >= view[j] - 1.0;
        let slack = 1.5 + if fills { o / other.max(1.0) } else { 0.0 };
        tex[i] as f32 + slack < (o * scale).min(o)
    })
}

/// Whether an image decoded into the box `was`, to a texture of `tex`, is decoded again for the
/// box `now`: when that is larger on a side, unless the texture is the whole image already.
pub(crate) fn grown(now: [u32; 2], was: [u32; 2], tex: [usize; 2], orig: [usize; 2]) -> bool {
    let larger = (0..2).any(|i| now[i] > was[i]);
    larger && (0..2).any(|i| tex[i] < orig[i])
}

/// Whether an image of `orig` shown at `scale` fits in a texture whose sides are at most `long`:
/// then it is decoded again at that size, not at full resolution. Not the decoders' limit, whose
/// free side is `long` itself in Fit Width and Fit Height.
pub(crate) fn fits_texture(orig: [usize; 2], scale: f32, long: u32) -> bool {
    orig.iter().all(|&side| side as f32 * scale < long as f32)
}

/// The box an image is decoded into: the `view` box for the shown image, turned with it when
/// it is turned a quarter by hand (`quarter`; Fill's view has the turn in it already); for a
/// neighbour the same box, unturned, or in Fill the longer side's square, as its shape is not
/// known before it is decoded (shown larger, it is decoded again); and at least the projector's
/// size, turned with the shown image too.
pub(crate) fn decode_box(
    view: [u32; 2],
    area: [u32; 2],
    fill: bool,
    shown: bool,
    quarter: bool,
    projector: [u32; 2],
) -> [u32; 2] {
    let [w, h] = match (fill, shown) {
        (true, false) => [area[0].max(area[1]); 2],
        _ => view,
    };
    let turned = quarter && shown;
    let [w, h] = if turned && !fill { [h, w] } else { [w, h] };
    let [pw, ph] = if turned {
        [projector[1], projector[0]]
    } else {
        projector
    };
    [w.max(pw), h.max(ph)]
}

/// A small map of the panes in the status bar, with the one it tells about filled in.
pub(crate) fn pane_map(ui: &mut egui::Ui, [cols, rows]: [usize; 2], on: usize) {
    let cell = egui::vec2(9.0, 7.0);
    let size = egui::vec2(cols as f32, rows as f32) * (cell + egui::vec2(2.0, 2.0));
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
    let v = ui.visuals();
    for k in 0..cols * rows {
        let at = egui::vec2((k % cols) as f32, (k / cols) as f32) * (cell + egui::vec2(2.0, 2.0));
        let r = egui::Rect::from_min_size(rect.min + at + egui::vec2(1.0, 1.0), cell);
        if k == on {
            ui.painter().rect_filled(r, 1.0, v.selection.bg_fill);
        } else {
            let stroke = egui::Stroke::new(1.0, v.weak_text_color());
            ui.painter()
                .rect_stroke(r, 1.0, stroke, egui::StrokeKind::Inside);
        }
    }
    response.on_hover_text(tr(
        "The pane these details are of: the one under the pointer",
    ));
}

pub(crate) fn layout_grid(layout: &str) -> [usize; 2] {
    LAYOUTS
        .iter()
        .find(|l| l.0 == layout)
        .map_or([1, 1], |l| l.2)
}

/// (index, full resolution, path, page, the job's number in `pending`).
/// Index, full resolution, path, page, job number, and the box to shrink into (sent with the
/// job, so a decode is at the size asked for when it was asked, whatever the view did since).
pub(crate) type DecodeJob = (usize, bool, PathBuf, usize, u64, [u32; 2]);

/// The decode workers: `workers` threads that take jobs and send back what they decoded,
/// skipping images the list has moved past (`keep`).
pub(crate) fn spawn_decoders(
    ctx: &egui::Context,
    workers: usize,
    keep: &Arc<Mutex<Vec<usize>>>,
    full_ok: &Arc<AtomicUsize>,
    rotate: &Arc<AtomicBool>,
) -> (mpsc::Sender<DecodeJob>, mpsc::Receiver<Decoded>) {
    let (jobs, job_rx) = mpsc::channel::<DecodeJob>();
    let (done_tx, done) = mpsc::channel();
    let job_rx = Arc::new(Mutex::new(job_rx));
    for _ in 0..workers {
        let (keep, full_ok, rotate, job_rx, done_tx, ctx) = (
            keep.clone(),
            full_ok.clone(),
            rotate.clone(),
            job_rx.clone(),
            done_tx.clone(),
            ctx.clone(),
        );
        std::thread::spawn(move || {
            loop {
                let Ok((idx, full, path, page, job, max)) = job_rx.lock().unwrap().recv() else {
                    return;
                };
                // Cancellation: skip images the user has already moved past.
                let wanted = {
                    let w = keep.lock().unwrap();
                    let n = full_ok.load(Ordering::Relaxed);
                    w.contains(&idx) && (!full || w.iter().take(n).any(|&i| i == idx))
                };
                if !wanted {
                    let _ = done_tx.send(Decoded {
                        rotated: false,
                        path,
                        page,
                        job,
                        result: None,
                    });
                    continue;
                }
                let rotated = rotate.load(Ordering::Relaxed);
                // The histogram here too, so the UI thread never walks the pixels.
                let result = Some(survive(|| decode(&path, max, rotated, page)).map(
                    |(frames, orig, pages)| {
                        let hist = histogram(&frames[0].0);
                        (frames, orig, hist, pages)
                    },
                ));
                let _ = done_tx.send(Decoded {
                    rotated,
                    path,
                    page,
                    job,
                    result,
                });
                // glibc does not return freed memory from the threads' malloc arenas on its own;
                // decoding leaves several image-sized buffers per thread.
                // SAFETY: malloc_trim has no preconditions beyond a working glibc malloc.
                unsafe { malloc_trim(0) };
                ctx.request_repaint();
            }
        });
    }
    (jobs, done)
}

/// A play button over a video's preview frame in `area`, readable on any frame; whether it
/// was clicked.
#[cfg(feature = "video")]
pub(crate) fn play_button(ui: &egui::Ui, area: egui::Rect) -> bool {
    let shade = egui::Color32::from_black_alpha(150);
    let button = egui::Rect::from_center_size(area.center(), egui::vec2(96.0, 96.0));
    let play = ui.interact(button, ui.scope_id().with("play"), egui::Sense::click());
    let p = ui.painter();
    p.circle_filled(button.center(), 48.0, shade);
    let c = button.center();
    let triangle = vec![
        c + egui::vec2(-14.0, -22.0),
        c + egui::vec2(24.0, 0.0),
        c + egui::vec2(-14.0, 22.0),
    ];
    p.add(egui::Shape::convex_polygon(
        triangle,
        egui::Color32::WHITE,
        egui::Stroke::NONE,
    ));
    let label = p.layout_no_wrap(
        tr("Video: Enter or click to play").into(),
        egui::FontId::proportional(16.0),
        egui::Color32::WHITE,
    );
    let at = egui::Rect::from_center_size(
        button.center_bottom() + egui::vec2(0.0, 28.0),
        label.size() + egui::vec2(20.0, 10.0),
    );
    p.rect_filled(at, 6.0, shade);
    p.galley(at.min + egui::vec2(10.0, 5.0), label, egui::Color32::WHITE);
    play.clicked()
}

/// The sign of a file Check Images found broken: an orange disc with "!", at `centre`.
pub(crate) fn broken_mark(p: &egui::Painter, centre: egui::Pos2, text_size: f32) {
    p.circle_filled(centre, 7.0, egui::Color32::from_rgb(240, 140, 0));
    let bold = egui::FontId::proportional(text_size * 0.85);
    p.text(
        centre,
        egui::Align2::CENTER_CENTER,
        "!",
        bold,
        egui::Color32::BLACK,
    );
}

impl crate::App {
    // ---- Decoding and painting the image ----

    /// Ask the decoders for file `idx` at the size shown, or at full resolution, unless it is
    /// there, failed or asked for already.
    pub(crate) fn request(&mut self, idx: usize, full: bool) {
        let have = self.list.slots.get(&idx).is_some_and(|s| s.full || !full);
        if !have
            && !self.list.decode_errors.contains_key(&idx)
            && !self.list.decodes_pending.contains_key(&(idx, full))
        {
            self.decodes_sent += 1;
            let path = self.path(idx);
            let max = match full {
                true => [self.max_side; 2],
                false => self.decode_box(idx),
            };
            self.list
                .decodes_pending
                .insert((idx, full), (self.decodes_sent, path.clone(), max));
            // The neighbours are preloaded at their first page.
            let page = if idx == self.list.cur {
                self.viewing.page
            } else {
                0
            };
            let _ = (self.decode_jobs).send((idx, full, path, page, self.decodes_sent, max));
        }
    }

    /// Drop textures outside the preload window, and full-resolution versions of images other
    /// than the shown one; request the missing ones.
    pub(crate) fn refill(&mut self) {
        if self.list.files.is_empty() {
            return;
        }
        self.fill_panes();
        // The shown image first, then its neighbours in the order the list shows.
        let (cur, ahead) = (self.list.cur, self.preload.load(Ordering::Relaxed) as isize);
        let mut window = vec![cur];
        // The compared image second: it may be loaded at full resolution too.
        let other = self.compare_other().filter(|&i| i != cur);
        window.extend(other);
        // The other panes, which may be zoomed too, and the images a slideshow shows in them next.
        window.extend(
            self.list
                .panes
                .iter()
                .filter(|&&i| i != cur && Some(i) != other),
        );
        let full = window.len();
        self.full_ok.store(full, Ordering::Relaxed);
        let n = self.list.panes.len() + 1;
        let upcoming =
            (1..=n as isize).filter_map(|d| self.step_from(self.list.last_slide, d, true));
        for i in self
            .list
            .panes
            .iter()
            .copied()
            .chain(upcoming.collect::<Vec<_>>())
        {
            if !window.contains(&i) {
                window.push(i);
            }
        }
        for d in (1..=ahead).flat_map(|d| [d, -d]) {
            if let Some(i) = self.step(d, false).filter(|i| !window.contains(i)) {
                window.push(i);
            }
        }
        self.list
            .slots
            .retain(|i, s| window.contains(i) && (!s.full || window[..full].contains(i)));
        *self.keep.lock().unwrap() = window.clone();
        for i in window {
            self.request(i, false);
        }
    }

    /// Decoded images into textures, those still wanted; why for those that could not be.
    pub(crate) fn receive_decoded(&mut self, ctx: &egui::Context) {
        while let Ok(d) = self.decoded_rx.try_recv() {
            // Not a job still asked for: asked for again, in another list, page or look. Its
            // place in the list is the job's now, as the list may have been read again since.
            let Some(&(idx, full)) = (self.list.decodes_pending.iter())
                .find(|(_, j)| j.0 == d.job)
                .map(|(k, _)| k)
            else {
                continue;
            };
            let decoded_for =
                (self.list.decodes_pending.remove(&(idx, full))).map_or([0; 2], |j| j.2);
            let wanted = self.is_file(idx, &d.path)
                && d.rotated == self.config.exif_rotate
                && d.page == if idx == self.list.cur { self.viewing.page } else { 0 }
                && self.keep.lock().unwrap().contains(&idx)
                // Full resolution for each image on screen: the shown one, the compared one and
                // the panes of a layout, zoomed together.
                && (!full
                    || idx == self.list.cur
                    || Some(idx) == self.compare_other()
                    || self.list.panes.contains(&idx));
            match d.result {
                Some(Ok((frames, orig, hist, pages))) if wanted => {
                    // A screen-size version must not replace a full-resolution one already loaded.
                    if !full && self.list.slots.get(&idx).is_some_and(|s| s.full) {
                        continue;
                    }
                    let mut anim: Vec<_> = (frames.into_iter().enumerate())
                        .map(|(n, (img, ms))| {
                            let name = format!("img{} {n}", idx);
                            (
                                ctx.load_texture(name, img, egui::TextureOptions::LINEAR),
                                ms,
                            )
                        })
                        .collect();
                    let tex = anim[0].0.clone();
                    if anim.len() == 1 {
                        anim.clear();
                    }
                    self.list.slots.insert(
                        idx,
                        Slot {
                            tex,
                            path: d.path,
                            full,
                            orig,
                            anim,
                            hist,
                            pages,
                            decoded_for,
                        },
                    );
                    // A full-resolution decode keeps fewer frames of a large animation (its
                    // memory budget): the frame stepped to stays among them.
                    let frames = self.list.slots.get(&idx).map_or(0, |s| s.anim.len());
                    if idx == self.list.cur
                        && frames > 0
                        && self.viewing.frame.is_some_and(|k| k >= frames)
                    {
                        self.viewing.frame = Some(frames - 1);
                    }
                    if idx == self.list.cur
                        && let Some(t) = self.started.take()
                    {
                        eprintln!("first image: {} ms", t.elapsed().as_millis());
                    }
                }
                Some(Err(e)) if wanted => {
                    self.list.decode_errors.insert(idx, e);
                }
                // Skipped by a worker that went by the place the job had when sent; the list
                // was read again meanwhile and moved the file to `idx`, still wanted there.
                // Asked for again, else nothing would: the previous image stayed on screen.
                None if !full
                    && self.is_file(idx, &d.path)
                    && !self.list.slots.contains_key(&idx)
                    && self.keep.lock().unwrap().contains(&idx) =>
                {
                    self.request(idx, false);
                }
                _ => {}
            }
        }
    }

    /// Decode the images and thumbnails again, to be shown in other colours.
    pub(crate) fn redecode(&mut self) {
        self.list.slots.clear();
        self.list.decodes_pending.clear();
        self.list.decode_errors.clear();
        self.thumbs_changed();
        self.last_drawn = None;
        self.refill();
    }

    /// The textures each pane shows now, for a fade to start from; `None` with fading off.
    pub(crate) fn shown_textures(&self) -> Option<Vec<Option<Shown>>> {
        (self.fade_ms() > 0).then(|| {
            (std::iter::once(self.list.cur).chain(self.list.panes.iter().copied()))
                .map(|i| self.list.slots.get(&i).map(|s| (s.tex.clone(), s.orig)))
                .collect()
        })
    }

    /// Pane `k` fading from another image to `now`: how far (0 to 1, eased), and the image it
    /// fades from. Ends the fade when it is done; asks for the next frame while it runs.
    pub(crate) fn fading(
        &mut self,
        ctx: &egui::Context,
        k: usize,
        now: egui::TextureId,
    ) -> Option<(f32, egui::TextureHandle, [usize; 2])> {
        let (start, from) = self.fade.as_ref()?;
        let t = start.elapsed().as_millis() as f32 / self.fade_ms().max(1) as f32;
        if t >= 1.0 {
            self.fade = None;
            return None;
        }
        ctx.request_repaint();
        let (tex, orig) = from.get(k)?.as_ref().filter(|f| f.0.id() != now)?;
        Some((t * t * (3.0 - 2.0 * t), tex.clone(), *orig))
    }

    /// What to show while the shown image is decoded, and whether it is its thumbnail: that
    /// thumbnail when there is one, else the image shown before, so browsing does not flash
    /// black. Not the image before for a video, whose frame takes ffmpeg a while: another photo
    /// stood under its play button.
    pub(crate) fn placeholder(&self) -> Option<(egui::TextureHandle, [usize; 2], bool)> {
        let name = self.list.files.get(self.list.cur)?;
        if let Some(Some(t)) = self.thumbs.get(name) {
            return Some((t.clone(), t.size(), true));
        }
        if is_video(Path::new(name)) {
            return None;
        }
        self.last_drawn
            .as_ref()
            .map(|(t, orig)| (t.clone(), *orig, false))
    }

    /// The image shown next to the current one when comparing: the other of two selected
    /// files, else the next image.
    pub(crate) fn compare_other(&self) -> Option<usize> {
        if !self.compare {
            return None;
        }
        // Two selected: the other of them. Not collected first: Select All in a large folder
        // would copy the whole selection every frame.
        let mut two = self.list.selected.iter().copied();
        match (self.list.selected.len(), two.next(), two.next()) {
            (2, Some(a), Some(b)) => Some(if a == self.list.cur { b } else { a }),
            _ => self.step(1, false).filter(|&i| i != self.list.cur),
        }
    }

    /// The layout's slideshow changes one pane at a time.
    pub(crate) fn one_at_a_time(&self) -> bool {
        let layout = &self.config.layout;
        self.config.layout_turns.split(',').any(|l| l == layout)
    }

    /// The panes after the first show the images that follow the shown one, except while a
    /// slideshow changes them one at a time.
    pub(crate) fn fill_panes(&mut self) {
        let [c, r] = layout_grid(&self.config.layout);
        let n = c * r;
        let kept = self.slideshow.is_some()
            && self.one_at_a_time()
            && self.list.panes.len() + 1 == n
            && self.list.panes.iter().all(|&i| i < self.list.files.len());
        if kept {
            return;
        }
        self.list.panes = (1..n as isize).filter_map(|k| self.step(k, true)).collect();
        self.list.last_slide = self.list.panes.last().copied().unwrap_or(self.list.cur);
        self.next_pane = 0;
    }

    pub(crate) fn set_layout(&mut self, layout: &str) {
        self.config.layout = layout.to_owned();
        self.config.save();
        // Comparing is a layout of its own.
        self.compare = false;
        self.refill();
    }

    /// View → Layout: one image, two or four, and how a slideshow changes them.
    pub(crate) fn layout_menu(&mut self, ui: &mut egui::Ui, act: &mut Option<Action>) {
        scroll_menu(ui, iconed(ui, theme::icon::LAYOUT, tr("Layout")), |ui| {
            menu_item(ui, act, Action::NextLayout);
            ui.separator();
            for (key, label, grid) in LAYOUTS {
                let icon = match grid {
                    [1, 1] => theme::icon::SQUARE,
                    [2, 1] => theme::icon::COLUMNS,
                    [1, 2] => theme::icon::ROWS,
                    _ => theme::icon::GRID_FOUR,
                };
                let radio = ui.radio(self.config.layout == key, iconed(ui, icon, tr(label)));
                if noted(&radio, tr(label)).clicked() {
                    self.set_layout(key);
                }
            }
            if self.config.layout == "1" {
                return;
            }
            ui.separator();
            if ui
                .checkbox(&mut self.config.layout_names, tr("Show File Names"))
                .changed()
            {
                self.config.save();
            }
            ui.weak(tr("Slideshow in this layout:"));
            let turns = self.one_at_a_time();
            let mut chosen = turns;
            let together = iconed(ui, theme::icon::TOGETHER, tr("All Images Change Together"));
            ui.radio_value(&mut chosen, false, together);
            let one = iconed(ui, theme::icon::CURSOR, tr("One Image at a Time"));
            ui.radio_value(&mut chosen, true, one);
            if chosen != turns {
                let layout = self.config.layout.clone();
                let mut list: Vec<&str> = (self.config.layout_turns.split(','))
                    .filter(|l| !l.is_empty() && *l != layout)
                    .collect();
                if chosen {
                    list.push(&layout);
                }
                self.config.layout_turns = list.join(",");
                self.config.save();
                self.refill();
            }
        });
    }

    pub(crate) fn zoom_by(&mut self, factor: f32) {
        self.zoom = Some((self.zoom.unwrap_or(self.last_scale) * factor).clamp(0.02, 32.0));
    }

    /// Image `i` in `rect`, one of a layout's panes after the first or the compared image: at
    /// the same zoom and offset as the shown one when zoomed, else fitted.
    /// Returns the scale it is drawn at, for the status bar; `None` while it is decoded.
    /// `k` is the pane's number, for a slideshow fading it; `None` for the compared image.
    pub(crate) fn paint_pane(
        &mut self,
        ui: &egui::Ui,
        rect: egui::Rect,
        i: usize,
        zoom: Option<f32>,
        k: Option<usize>,
    ) -> Option<f32> {
        // Zoomed in further than the screen-size version holds: load the full resolution.
        let sharper = self.list.slots.get(&i).is_some_and(|s| {
            !s.full
                && zoom
                    .is_some_and(|z| s.tex.size()[0] as f32 + 1.0 < s.orig[0] as f32 * z.min(1.0))
        });
        if sharper {
            self.request(i, true);
        }
        let now = self.list.slots.get(&i)?.tex.id();
        let fade = k.and_then(|k| self.fading(ui.ctx(), k, now));
        let slot = self.list.slots.get(&i)?;
        let ppp = ui.ctx().pixels_per_point();
        let look = (self.config.enlarge, self.config.fit_mode);
        let zoom = zoom.map(|z| (z, self.pan_offset));
        let (at, scale) = placed(rect, slot.orig, ppp, look, zoom);
        let uv = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
        let p = ui.painter().with_clip_rect(rect);
        let shown = match &fade {
            Some((t, old, orig)) => {
                let from = placed(rect, *orig, ppp, look, zoom).0;
                p.image(old.id(), from, uv, alpha(1.0 - t.powi(4)));
                *t
            }
            None => 1.0,
        };
        if self.config.checkerboard && slot.transparent() {
            paint_checker(&p, at, 0, alpha(shown));
        }
        p.image(slot.tex.id(), at, uv, alpha(shown));
        Some(scale)
    }

    /// The compared image in `rect`, with its name above; which it is and its scale.
    pub(crate) fn paint_other(
        &mut self,
        ui: &egui::Ui,
        rect: egui::Rect,
        zoom: Option<f32>,
    ) -> Option<(usize, Option<f32>)> {
        ui.painter().vline(
            rect.left() - 1.0,
            rect.y_range(),
            egui::Stroke::new(2.0, egui::Color32::from_gray(60)),
        );
        let other = self.compare_other()?;
        let scale = self.paint_pane(ui, rect, other, zoom, None);
        self.paint_name(ui, rect, other);
        Some((other, scale))
    }

    /// The name of image `i` in the top left corner of `rect`.
    pub(crate) fn paint_name(&self, ui: &egui::Ui, rect: egui::Rect, i: usize) {
        let p = ui.painter();
        let font = egui::TextStyle::Body.resolve(ui.style());
        let galley = p.layout_no_wrap(self.name(i).into_owned(), font, egui::Color32::WHITE);
        let text = egui::Rect::from_min_size(rect.left_top() + egui::vec2(8.0, 8.0), galley.size());
        p.rect_filled(text.expand(4.0), 3.0, egui::Color32::from_black_alpha(160));
        p.galley(text.min, galley, egui::Color32::WHITE);
    }

    /// The shown image's histogram in the bottom right corner of `area`.
    pub(crate) fn paint_histogram(&self, ui: &egui::Ui, area: egui::Rect) {
        let Some(slot) = self.list.slots.get(&self.list.cur) else {
            return;
        };
        let size = egui::vec2(256.0, 100.0);
        let rect =
            egui::Rect::from_min_size(area.right_bottom() - size - egui::vec2(16.0, 16.0), size);
        let p = ui.painter();
        p.rect_filled(rect.expand(6.0), 4.0, egui::Color32::from_black_alpha(170));
        use egui::Color32 as C;
        let channels: &[(usize, C)] = if self.config.histogram == 1 {
            &[
                (0, C::from_rgb(255, 80, 80)),
                (1, C::from_rgb(80, 220, 80)),
                (2, C::from_rgb(90, 140, 255)),
            ]
        } else {
            &[(3, C::from_gray(230))]
        };
        let log = self.config.histogram_log;
        let height = |v: u32| if log { (v as f32).ln_1p() } else { v as f32 };
        let max = channels
            .iter()
            .flat_map(|c| slot.hist[c.0].iter())
            .map(|&v| height(v))
            .fold(1.0, f32::max);
        for &(c, color) in channels {
            let points = (0..256)
                .map(|i| {
                    let x = rect.left() + i as f32 * rect.width() / 255.0;
                    egui::pos2(
                        x,
                        rect.bottom() - height(slot.hist[c][i]) / max * rect.height(),
                    )
                })
                .collect();
            p.add(egui::Shape::line(points, egui::Stroke::new(1.5, color)));
        }
    }

    /// Full screen info in the top left corner, from the template in Preferences.
    pub(crate) fn overlay(&mut self, ui: &mut egui::Ui, area: egui::Rect) {
        self.want_exif(ui.ctx());
        let path = self.path(self.list.cur);
        let meta = self
            .meta
            .as_ref()
            .filter(|m| m.path == path)
            .map(|m| &m.saved);
        let text = fill_template(&self.config.osd_template, |field| match field {
            "name" => self.name(self.list.cur).to_string(),
            "path" => path.display().to_string(),
            "position" => self.place(),
            "dimensions" => (self.list.slots.get(&self.list.cur))
                .map(|s| format!("{} × {}", s.orig[0], s.orig[1]))
                .unwrap_or_default(),
            "marks" => self.mark_text(self.list.cur),
            "keywords" => meta.map(|m| m.keywords.join(", ")).unwrap_or_default(),
            "comment" => meta.map(|m| m.comment.clone()).unwrap_or_default(),
            label => (self.exif.as_ref().filter(|e| e.0 == path))
                .and_then(|(_, rows)| rows.iter().find(|r| r.0 == label))
                .map(|r| r.1.clone())
                .unwrap_or_default(),
        });
        if text.is_empty() {
            return;
        }
        let p = ui.painter();
        let galley = p.layout_no_wrap(text, egui::FontId::proportional(15.0), egui::Color32::WHITE);
        let at = egui::Rect::from_min_size(
            area.min + egui::vec2(12.0, 12.0),
            galley.size() + egui::vec2(20.0, 12.0),
        );
        p.rect_filled(at, 6.0, egui::Color32::from_black_alpha(150));
        p.galley(at.min + egui::vec2(10.0, 6.0), galley, egui::Color32::WHITE);
    }

    /// Window → Projector: a second window, for another screen or a projector, with only the
    /// shown image, as the main window shows it: fitted, or when zoomed the same part of it,
    /// scaled to this window. Same process, so the textures are shared and nothing is decoded
    /// twice. Wayland lets no program put a window on a chosen screen: it is dragged there, and
    /// F or a double-click makes it full screen. Its keys browse the main window.
    pub(crate) fn projector_window(&mut self, ctx: &egui::Context) {
        let id = egui::ViewportId::from_hash_of("glimt projector");
        let builder = egui::ViewportBuilder::default()
            .with_title("Glimt Projector")
            .with_app_id(app_id())
            .with_inner_size([960.0, 600.0]);
        let mut act = None;
        let closed = ctx.show_viewport_immediate(id, builder, |ui, _| {
            let pctx = ui.ctx().clone();
            let full = pctx.input(|i| i.viewport().fullscreen).unwrap_or(false);
            let (f, esc) = pctx.input(|i| {
                let f = i.key_pressed(egui::Key::F) || i.key_pressed(egui::Key::F11);
                (f && i.modifiers.is_none(), i.key_pressed(egui::Key::Escape))
            });
            let key = Self::shortcut(&pctx, self.config.delete_key, self.collection.is_some())
                .filter(|a| KIOSK_ACTIONS.contains(a));
            act = key;
            let [r, g, b] = self.config.background.0;
            let frame = egui::Frame::NONE.fill(egui::Color32::from_rgb(r, g, b));
            egui::CentralPanel::default().frame(frame).show(ui, |ui| {
                let area = ui.max_rect();
                // A projector larger than the main view (full screen on a big screen): the
                // images decoded again to its size, which they were too small for.
                let ppp = pctx.pixels_per_point();
                let size = [(area.width() * ppp) as u32, (area.height() * ppp) as u32];
                if size != self.projector_size {
                    let old = std::mem::replace(&mut self.projector_size, size);
                    let had = [self.view_box[0].max(old[0]), self.view_box[1].max(old[1])];
                    let grown = size[0] > had[0] || size[1] > had[1];
                    // Only then: a projector smaller than the view shows the images it has, and
                    // dragging its edge decodes nothing again.
                    if grown {
                        self.list.slots.retain(|_, s| s.full);
                        self.list.decodes_pending.retain(|&(_, full), _| full);
                        self.refill();
                    }
                }
                let response = ui.allocate_rect(area, egui::Sense::click());
                if f || response.double_clicked() || (esc && full) {
                    pctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!full));
                }
                self.paint_projector(ui, area);
            });
            pctx.input(|i| i.viewport().close_requested())
        });
        if closed {
            self.projector = false;
            self.projector_size = [0, 0];
        }
        if let Some(action) = act {
            self.run(ctx, action);
        }
    }

    /// The shown image in the projector's `area`, as `view` showed it, and the pointer's ring.
    pub(crate) fn paint_projector(&self, ui: &egui::Ui, area: egui::Rect) {
        let Some(slot) = self.list.slots.get(&self.list.cur) else {
            return;
        };
        let ppp = ui.ctx().pixels_per_point();
        let turns = self.viewing.orient.0;
        let [w, h] = slot.orig.map(|n| n as f32);
        let (uw, uh) = if turns % 2 == 1 { (h, w) } else { (w, h) };
        let fit = fit_scale(area.size() * ppp, [uw, uh], self.config.fit_mode);
        let fit = if self.config.enlarge {
            fit
        } else {
            fit.min(1.0)
        };
        // Zoomed: the same part of the image, as large as this window allows.
        let (scale, pan) = match self.last_view {
            Some(v) if v.zoomed => {
                let k = (area.width() * ppp / (v.area.width() * v.ppp))
                    .min(area.height() * ppp / (v.area.height() * v.ppp));
                let scale = v.scale * k;
                (scale, v.pan * (scale / ppp) / (v.scale / v.ppp))
            }
            _ => (fit, egui::Vec2::ZERO),
        };
        let rect =
            egui::Rect::from_center_size(area.center() + pan, egui::vec2(w, h) * scale / ppp);
        if self.config.checkerboard && slot.transparent() {
            paint_checker(ui.painter(), rect, turns, egui::Color32::WHITE);
        }
        let tex = self.texture(slot, ui.ctx());
        paint_turned(ui, tex, rect, self.viewing.orient, egui::Color32::WHITE);
        // Where the pointer is over the main window's image, to point things out.
        if self.config.projector_pointer
            && let Some(v) = self.last_view
            && let Some(at) = v.pointer
        {
            let at = area.center() + pan + at * (scale / ppp) / (v.scale / v.ppp);
            let p = ui.painter();
            p.circle_stroke(
                at,
                22.0,
                egui::Stroke::new(5.0, egui::Color32::from_black_alpha(160)),
            );
            p.circle_stroke(
                at,
                22.0,
                egui::Stroke::new(3.0, egui::Color32::from_rgb(255, 210, 0)),
            );
        }
    }

    /// The frame of the shown animation playing now.
    pub(crate) fn playing_frame(&self) -> usize {
        let slot = self.list.slots.get(&self.list.cur);
        match self.config.animate {
            true => slot.map_or(0, |s| s.frame_at(self.opened.elapsed()).0),
            false => 0,
        }
    }

    /// What `slot` shows now, in the main window and on the projector alike: the frame stepped
    /// to, the frame playing, or the image.
    pub(crate) fn texture<'a>(
        &self,
        slot: &'a Slot,
        ctx: &egui::Context,
    ) -> &'a egui::TextureHandle {
        match self.viewing.frame {
            Some(k) => slot.anim.get(k).map_or(&slot.tex, |f| &f.0),
            None if self.config.animate => slot.frame(self.opened.elapsed(), ctx),
            None => &slot.tex,
        }
    }

    /// The frame (or page) shown now: the one stepped to, else the page.
    pub(crate) fn shown_frame(&self) -> usize {
        let animated = self
            .list
            .slots
            .get(&self.list.cur)
            .is_some_and(|s| s.anim.len() > 1);
        match self.viewing.frame {
            Some(k) => k,
            None if animated => self.playing_frame(),
            None => self.viewing.page,
        }
    }

    /// Frames, or pages, of the shown file: for the status bar and the File menu's entries.
    pub(crate) fn frame_count(&self) -> usize {
        (self.list.slots.get(&self.list.cur)).map_or(1, |s| s.anim.len().max(s.pages))
    }

    /// The image area: the shown image in one or more panes, compare, zoom and drag, the overlay,
    /// the film strip in full screen; and what a click there asked for.
    pub(crate) fn image_area(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        fullscreen: bool,
        bare: bool,
    ) {
        let ppp = ctx.pixels_per_point();
        // Actions from the image area, run after drawing it.
        let mut later = None;
        let mut clicked_pane = None;
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill({
                let [r, g, b] = self.config.background.0;
                egui::Color32::from_rgb(r, g, b)
            }))
            .show(ui, |ui| {
                // A layout: the shown image in the first pane, the following ones in the rest.
                let [cols, rows] = layout_grid(&self.config.layout);
                let cell = ui.max_rect().size() / egui::vec2(cols as f32, rows as f32);
                let panes: Vec<egui::Rect> = (0..rows * cols)
                    .map(|k| {
                        let at = egui::vec2((k % cols) as f32, (k / cols) as f32) * cell;
                        let rect = egui::Rect::from_min_size(ui.max_rect().min + at, cell);
                        if cols * rows > 1 {
                            rect.shrink(1.0)
                        } else {
                            rect
                        }
                    })
                    .collect();
                // Comparing: the shown image on the left, the other on the right.
                let full = panes[0];
                let (area, right) = if self.compare && panes.len() == 1 {
                    let (l, r) = full.split_left_right_at_fraction(0.5);
                    (
                        l.shrink2(egui::vec2(1.0, 0.0)),
                        Some(r.shrink2(egui::vec2(1.0, 0.0))),
                    )
                } else {
                    (full, None)
                };
                // The wheel and dragging work over every pane, and the compared half.
                let several = panes.len() > 1 || right.is_some();
                let input = if several { ui.max_rect() } else { area };
                let response = ui.allocate_rect(input, egui::Sense::click_and_drag());
                #[cfg(feature = "accessibility")]
                self.speak_image(ui, &response);
                let max_fit = if self.config.enlarge {
                    f32::INFINITY
                } else {
                    1.0
                };
                // Size on screen after a quarter turn.
                let quarter = self.viewing.orient.0 % 2 == 1;
                let turned = |[w, h]: [usize; 2]| if quarter { [h, w] } else { [w, h] };
                let mode = self.config.fit_mode;
                let fit = self.list.slots.get(&self.list.cur).map_or(1.0, |s| {
                    let [w, h] = turned(s.orig);
                    fit_scale(area.size() * ppp, [w as f32, h as f32], mode).min(max_fit)
                });
                let long = self.decode_size(area, ppp, max_fit);
                if response.hovered()
                    && !self.dialog_open()
                    && let Some(step) = self.wheel_input(ctx, &response, fit, area, &panes, right)
                {
                    later = Some(step);
                }
                if response.double_clicked() && !self.kiosk {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fullscreen));
                }
                let scale = self.zoom.unwrap_or(fit);
                self.last_scale = scale;
                // Panning only along a side where the image is larger than the view, and only up
                // to its edge, so an image that fits stays put.
                let orig = (self.list.slots.get(&self.list.cur).map(|s| s.orig))
                    .or(self.last_drawn.as_ref().map(|d| d.1));
                if let Some([w, h]) = orig.map(turned) {
                    // Choosing a part to crop, a drag draws it instead.
                    if response.dragged() && self.viewing.crop.is_none() {
                        self.pan_offset += response.drag_delta();
                    }
                    let size = egui::vec2(w as f32, h as f32) * scale / ppp;
                    let max = ((size - area.size()) / 2.0).max(egui::Vec2::ZERO);
                    self.pan_offset = self.pan_offset.clamp(-max, max);
                    self.max_pan = max;
                }

                // Not zoomed: decoded again at window size when the box the view decodes into has
                // grown past the one the texture was made for (the previous texture is shown
                // meanwhile). By the box, not the texture's size: the decoders round, and the
                // same box gives the same texture, so never again and again. Zoomed: full
                // resolution, also where the size shown is larger than a texture may be.
                let slot = self.list.slots.get(&self.list.cur);
                let decode_box = self.decode_box(self.list.cur);
                let grown = slot.is_some_and(|s| {
                    !s.full && grown(decode_box, s.decoded_for, s.tex.size(), s.orig)
                });
                let view = area.size() * ppp;
                let view = if quarter {
                    [view.y, view.x]
                } else {
                    [view.x, view.y]
                };
                let too_small =
                    slot.is_some_and(|s| !s.full && too_small(s.tex.size(), s.orig, scale, view));
                // Not zoomed: decoded again to the size shown, which the decoders' limit now
                // holds; full resolution only where that is larger than a texture may be.
                let fits = slot.is_some_and(|s| fits_texture(s.orig, scale, long));
                let fitted = self.zoom.is_none() && fits;
                if fitted && grown {
                    self.list.slots.remove(&self.list.cur);
                    self.request(self.list.cur, false);
                } else if !fitted && too_small {
                    self.request(self.list.cur, true);
                }
                self.paint_shown(ctx, ui, area, scale, max_fit);
                if self.viewing.crop.is_some() {
                    let shown = self.list.slots.get(&self.list.cur).map(|s| turned(s.orig));
                    match shown {
                        Some([w, h]) => {
                            let size = egui::vec2(w as f32, h as f32) * scale / ppp;
                            let at = area.center() + self.pan_offset;
                            let bounds = egui::Rect::from_center_size(at, size);
                            self.crop_input(ctx, ui, &response, bounds, [w, h]);
                        }
                        None => self.viewing.crop = None,
                    }
                }
                // A video: a play button over the preview frame (click or Enter), readable on
                // any frame, also in the slideshow and full screen.
                #[cfg(feature = "video")]
                if (self.list.files.get(self.list.cur)).is_some_and(|f| is_video(Path::new(f)))
                    && play_button(ui, area)
                {
                    later = Some(Action::Play);
                }
                // Several panes: the name in each or in none (after the panes are drawn), not the
                // shown image's details in one.
                if panes.len() == 1 && bare && self.config.osd && !self.list.files.is_empty() {
                    self.overlay(ui, area);
                }
                if self.config.histogram > 0 {
                    self.paint_histogram(ui, area);
                }
                // What each pane shows, for the status bar: the one under the pointer.
                let mut shown = vec![(self.list.cur, Some(self.last_scale))];
                if let Some(right) = right {
                    shown.extend(self.paint_other(ui, right, self.zoom));
                }
                for (k, (rect, i)) in panes[1..].iter().zip(self.list.panes.clone()).enumerate() {
                    shown.push((i, self.paint_pane(ui, *rect, i, self.zoom, Some(k + 1))));
                    // A click makes it the shown image, in the first pane.
                    if ui
                        .interact(*rect, ui.scope_id().with(("pane", k)), egui::Sense::click())
                        .clicked()
                    {
                        clicked_pane = Some(i);
                    }
                }
                let rects: Vec<egui::Rect> = match right {
                    Some(r) => vec![area, r],
                    None => panes.clone(),
                };
                let pointer = ui.ctx().pointer_hover_pos();
                let hovered = pointer.and_then(|p| rects.iter().position(|r| r.contains(p)));
                // The pane the status bar tells about gets a thin outline.
                if let Some(k) = hovered.filter(|_| rects.len() > 1 && !bare)
                    && k < shown.len()
                {
                    let stroke = egui::Stroke::new(1.5, ui.visuals().selection.bg_fill);
                    ui.painter()
                        .rect_stroke(rects[k], 0.0, stroke, egui::StrokeKind::Inside);
                }
                self.status_pane = hovered.filter(|&k| k < shown.len()).unwrap_or(0);
                self.status_grid = if right.is_some() {
                    [2, 1]
                } else {
                    [cols, rows]
                };
                self.pane_info = shown;
                if panes.len() > 1 && self.config.layout_names && !self.list.files.is_empty() {
                    let shown =
                        std::iter::once(self.list.cur).chain(self.list.panes.iter().copied());
                    for (rect, i) in panes.iter().zip(shown) {
                        self.paint_name(ui, *rect, i);
                    }
                }
                // Full screen: a film strip of the images around this one, while the pointer
                // is near the bottom.
                if fullscreen && !self.kiosk && !self.list.files.is_empty() {
                    clicked_pane = self.filmstrip(ui, ui.max_rect()).or(clicked_pane);
                }
                self.paint_notice(ui, area);
            });
        if let Some(action) = later {
            self.run(ctx, action);
        }
        if let Some(i) = clicked_pane {
            self.go(ctx, i);
        }
    }

    /// The view's box the shown image is decoded into (`view_box`), from the fit mode and, in
    /// Fill, the image's shape; returns the longest side a texture may have.
    pub(crate) fn decode_size(&mut self, area: egui::Rect, ppp: f32, max_fit: f32) -> u32 {
        // The image area, not the whole screen: a small window on a 4K screen needs a fraction
        // of the memory (3840×2160 is 33 MB per image).
        let [w, h] = [(area.width() * ppp) as u32, (area.height() * ppp) as u32];
        let quarter = self.viewing.orient.0 % 2 == 1;
        let turned = |[w, h]: [usize; 2]| if quarter { [h, w] } else { [w, h] };
        let mode = self.config.fit_mode;
        // Decoded to the size shown: the view for a whole image. Fit Width and Fit Height
        // fill one side and may need up to the screen on the other, whatever the image,
        // so the neighbours read ahead are sharp too. Fill depends on the image's shape:
        // its size at its scale once that is known (also while it is decoded again, from
        // the image drawn), else the longer side.
        let long = self.max_side;
        let orig = (self.list.slots.get(&self.list.cur).map(|s| s.orig))
            .or(self.last_drawn.as_ref().map(|d| d.1));
        let view = match (mode, orig) {
            (0, _) => [w, h],
            (3, Some([ow, oh])) => {
                let [tw, th] = turned([ow, oh]);
                let s = fit_scale(area.size() * ppp, [tw as f32, th as f32], mode);
                let size = |side: usize| (side as f32 * s.min(max_fit)).ceil() as u32;
                [size(ow).clamp(1, long), size(oh).clamp(1, long)]
            }
            (1, _) => [w, long],
            (2, _) => [long, h],
            _ => [w.max(h); 2],
        };
        (self.view_box, self.view_area) = (view, [w, h]);
        long
    }

    /// The box file `idx` is shrunk into for the view and the projector, as `decode_box`.
    pub(crate) fn decode_box(&self, idx: usize) -> [u32; 2] {
        decode_box(
            self.view_box,
            self.view_area,
            self.config.fit_mode == 3,
            idx == self.list.cur,
            self.viewing.orient.0 % 2 == 1,
            self.projector_size,
        )
    }

    /// The shown image in `area` at `scale`, turned as the view is, fading from the one before
    /// in a slideshow; while it is decoded its thumbnail or the image before, else why not.
    pub(crate) fn paint_shown(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        area: egui::Rect,
        scale: f32,
        max_fit: f32,
    ) {
        let ppp = ctx.pixels_per_point();
        let mode = self.config.fit_mode;
        let now = self.list.slots.get(&self.list.cur).map(|s| s.tex.id());
        let fade = now.and_then(|id| self.fading(ctx, 0, id));
        if let Some(slot) = self.list.slots.get(&self.list.cur) {
            let size = egui::vec2(slot.orig[0] as f32, slot.orig[1] as f32) * scale / ppp;
            let rect = egui::Rect::from_center_size(area.center() + self.pan_offset, size);
            let tex = self.texture(slot, ctx);
            // Within its pane or half, which zoom makes it overflow.
            let clip = ui.clip_rect();
            ui.set_clip_rect(area.intersect(clip));
            // A slideshow fading: the image before goes as this one comes.
            let shown = match &fade {
                Some((t, old, orig)) => {
                    let look = (self.config.enlarge, mode);
                    let zoom = self.zoom.map(|z| (z, self.pan_offset));
                    let at = placed(area, *orig, ppp, look, zoom).0;
                    let uv1 = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
                    ui.painter()
                        .image(old.id(), at, uv1, alpha(1.0 - t.powi(4)));
                    *t
                }
                None => 1.0,
            };
            if self.config.checkerboard && slot.transparent() {
                paint_checker(ui.painter(), rect, self.viewing.orient.0, alpha(shown));
            }
            paint_turned(ui, tex, rect, self.viewing.orient, alpha(shown));
            ui.set_clip_rect(clip);
            self.last_drawn = Some((slot.tex.clone(), slot.orig));
            // For the projector: this view, and the pointer from the image's centre.
            let centre = area.center() + self.pan_offset;
            let pointer = (ctx.pointer_hover_pos())
                .filter(|p| area.contains(*p))
                .map(|p| p - centre);
            self.last_view = Some(View {
                area,
                ppp,
                scale,
                pan: self.pan_offset,
                zoomed: self.zoom.is_some(),
                pointer,
            });
        } else if let Some((tex, orig, thumb)) = self.placeholder()
            && !self.list.decode_errors.contains_key(&self.list.cur)
        {
            // A thumbnail fills the area, as the image will; the image before keeps
            // its size.
            let size = egui::vec2(orig[0] as f32, orig[1] as f32);
            let fit = (area.width() * ppp / size.x).min(area.height() * ppp / size.y);
            let fit = if thumb { fit } else { fit.min(max_fit) };
            let rect = egui::Rect::from_center_size(area.center(), size * fit / ppp);
            egui::Image::new(&tex).paint_at(ui, rect);
        } else {
            let text = if self.list.files.is_empty() {
                if self.listing_rx.is_some() {
                    tr("Reading folder…")
                } else {
                    tr("No images in folder")
                }
                .to_owned()
            } else if let Some(e) = self.list.decode_errors.get(&self.list.cur) {
                format!(
                    "Could not open {}:\n{e}",
                    self.path(self.list.cur).display()
                )
            } else {
                String::new()
            };
            // Broken into lines that fit, as an error can be long: a decoder's message.
            let (font, grey) = (egui::FontId::proportional(16.0), egui::Color32::GRAY);
            let width = (area.width() - 32.0).max(80.0);
            let galley = ui.painter().layout(text, font, grey, width);
            let at = area.center() - galley.size() / 2.0;
            ui.painter().galley(at, galley, grey);
        }
    }

    /// The notice at the foot of `area`, wrapped to it, on a dark band so it reads over any image.
    pub(crate) fn paint_notice(&self, ui: &egui::Ui, area: egui::Rect) {
        if let Some(notice) = &self.notice {
            let (font, red) = (egui::FontId::proportional(16.0), egui::Color32::LIGHT_RED);
            let width = (area.width() - 48.0).max(80.0);
            let galley = ui.painter().layout(notice.clone(), font, red, width);
            let at =
                area.center_bottom() - egui::vec2(galley.size().x / 2.0, 16.0 + galley.size().y);
            let band = egui::Rect::from_min_size(at, galley.size()).expand(8.0);
            ui.painter()
                .rect_filled(band, 6.0, egui::Color32::from_black_alpha(200));
            ui.painter().galley(at, galley, red);
        }
    }

    /// The wheel over the image: zoom about the point under the pointer (Ctrl+wheel, or every
    /// wheel with wheel_zoom), else a notch is the next or previous image, which it returns.
    /// `fit` is the zoom that fits, `area` the first pane (or compared half), `right` the
    /// other half when comparing.
    pub(crate) fn wheel_input(
        &mut self,
        ctx: &egui::Context,
        response: &egui::Response,
        fit: f32,
        area: egui::Rect,
        panes: &[egui::Rect],
        right: Option<egui::Rect>,
    ) -> Option<Action> {
        let mut step = None;
        // egui turns Ctrl+wheel into zoom_delta, so it zooms in both modes.
        let (mut factor, scroll, notches) = ctx.input(|i| {
            let notches: f32 = i
                .events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::MouseWheel {
                        unit,
                        delta,
                        modifiers,
                        ..
                    } if !modifiers.command => Some(match unit {
                        egui::MouseWheelUnit::Point => delta.y / 50.0,
                        _ => delta.y,
                    }),
                    _ => None,
                })
                .sum();
            (i.zoom_delta(), i.smooth_scroll_delta.y, notches)
        });
        if self.config.wheel_zoom {
            factor *= 1.0 + scroll / 400.0;
        } else {
            // One notch is one image; touchpad deltas add up to a notch.
            self.wheel += notches;
            if self.wheel.abs() >= 1.0 {
                let up = self.wheel > 0.0;
                step = Some(if up { Action::Prev } else { Action::Next });
                self.wheel = 0.0;
            }
        }
        if factor != 1.0 {
            let old = self.zoom.unwrap_or(fit);
            let new = (old * factor).clamp(0.02, 32.0);
            self.zoom = Some(new);
            // Keep the point under the pointer in place, as in other viewers.
            if let Some(p) = response.hover_pos() {
                // From the centre of the pane under the pointer: they share the offset.
                let under = (right.iter().chain(panes))
                    .find(|r| r.contains(p))
                    .map_or(area, |r| if *r == panes[0] { area } else { *r });
                let from_centre = p - under.center();
                self.pan_offset = from_centre - (from_centre - self.pan_offset) * (new / old);
            }
        }
        step
    }
}
