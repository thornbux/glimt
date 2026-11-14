//! The image area. It decodes the shown image and its neighbours, and holds what belongs to the
//! image shown (`Viewing`). It also does zoom and fit, panes and Compare, the painting, and the
//! wheel.

use crate::files::config::Config;
use crate::files::external::is_video;
use crate::images::decode::{Decoded, catch_panic_as_error, decode, histogram};
use crate::model::list::LoadedImage;
use crate::support::format::fill_template;
use crate::support::lang::tr;
use crate::support::test_hooks::noted;
use crate::support::threads::{STUCK_AFTER, lock_even_if_poisoned};
use crate::ui::actions::{Action, KIOSK_ACTIONS};
use crate::ui::menus::{checkbox, menu_item, scroll_menu, with_icon};
use crate::viewer::crop::Crop;
use crate::{app_id, ui::theme};
use eframe::egui;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Instant;

/// View → Layout, as (the config value, the menu label, the panes as [columns, rows]).
pub(crate) const LAYOUTS: [(&str, &str, [usize; 2]); 4] = [
    ("1", "One Image", [1, 1]),
    ("2x1", "Two Side by Side", [2, 1]),
    ("1x2", "Two Above Each Other", [1, 2]),
    ("2x2", "Four (2 × 2)", [2, 2]),
];

/// A texture shown and its image's size, which a fade goes from.
pub(crate) type ShownTexture = (egui::TextureHandle, [usize; 2]);

/// The image area of the last frame, which the projector follows. It holds where the area was
/// (in points), the screen's pixels per point, the scale (screen pixels per image pixel), the
/// offset of a zoomed image, whether it was zoomed, and the pointer over it, measured from the
/// image's centre.
#[derive(Clone, Copy)]
pub(crate) struct ImageArea {
    area: egui::Rect,
    ppp: f32,
    scale: f32,
    pan: egui::Vec2,
    zoomed: bool,
    pointer: Option<egui::Vec2>,
}

/// An image for the decode workers.
pub(crate) struct DecodeJob {
    /// Whether it is decoded at full resolution (zoomed in), not at the view's size.
    pub(crate) full_resolution: bool,
    pub(crate) path: PathBuf,
    pub(crate) page: usize,
    /// The request's number, as `pending` knows it.
    pub(crate) request_number: u64,
    /// The box to shrink into, sent with the job, so a decode is at the size asked for when it
    /// was asked, whatever the view did since.
    pub(crate) max_box: [u32; 2],
}

/// The decoding of the shown image and its neighbours. It holds the workers, the images they
/// keep, and the size they decode to.
pub(crate) struct Decoding {
    /// The files of the images to keep decoded, the shown one first and then its neighbours in
    /// list order. The workers share it, and they skip jobs outside it. It holds paths, not
    /// places, because a list read again (the parts of a tree with Include Subfolders) moves
    /// the shown image to another place while its job waits. A job judged by its old place was
    /// skipped and asked for again at the new one, and with parts coming faster than a slow
    /// share served the queue, the image was never decoded.
    kept_images: Arc<Mutex<Vec<PathBuf>>>,
    /// How many images at the start of `kept_images` may be decoded at full resolution. They
    /// are the shown one and the compared one beside it.
    full_resolution_count: Arc<AtomicUsize>,
    /// Number of images preloaded on each side (Preferences).
    preload: Arc<AtomicUsize>,
    /// Workers apply EXIF orientation (Preferences, Alt+X).
    auto_rotate: Arc<AtomicBool>,
    /// The view's box for the shown image, unturned, as `decode_size` works it out each frame,
    /// and the view's size. The box depends on the fit mode, and in Fill on the image's own
    /// shape. The decoders shrink into it, through `decode_box` for each job.
    view_box: [u32; 2],
    view_area: [u32; 2],
    /// The longest side a texture may have.
    max_side: u32,
    /// The requests to the decode workers, what they decoded, and the number of the last
    /// request sent.
    decode_requests: mpsc::Sender<DecodeJob>,
    decoded: mpsc::Receiver<Decoded>,
    last_request_number: u64,
    /// The last image drawn. It stays on screen until the next one is decoded, so browsing
    /// never shows a black screen.
    last_drawn: Option<(egui::TextureHandle, [usize; 2])>,
    /// A slideshow's change of image while it fades in. It holds when the fade began, and what
    /// each pane (the first, then the others) showed before, until that has faded out.
    fade: Option<(Instant, Vec<Option<ShownTexture>>)>,
    /// When the shown image was opened. It is the clock of an animation.
    opened: Instant,
    /// Set at startup and taken when the first image is shown, for the "first image: N ms" line
    /// on standard error that the release tests read.
    started: Option<Instant>,
    /// How many frames after the first image took each number of milliseconds of the window
    /// thread's processor time. The last entry counts that many or more. It is for the
    /// "frames:" line at exit that the release tests read. Work that grows with the list makes
    /// every frame slow, while the slowest frame alone varies with one-off work such as Select
    /// All.
    frame_ms: [u32; 256],
    /// The colour profiles chosen, read and set in the background, as decoding goes by them.
    profile_changes: crate::images::colour::ProfileChanges,
}

impl Decoding {
    /// The decoder workers started, `workers` of them, with the settings they read.
    pub(crate) fn new(ctx: &egui::Context, config: &Config, workers: usize) -> Self {
        let kept_images = Arc::new(Mutex::new(Vec::new()));
        let full_resolution_count = Arc::new(AtomicUsize::new(1));
        let preload = Arc::new(AtomicUsize::new(config.preload));
        let auto_rotate = Arc::new(AtomicBool::new(config.exif_rotate));
        let (decode_requests, decoded) = spawn_decoders(
            ctx,
            workers,
            &kept_images,
            &full_resolution_count,
            &auto_rotate,
        );
        Decoding {
            kept_images,
            full_resolution_count,
            preload,
            auto_rotate,
            view_box: [1920, 1080],
            view_area: [1920, 1080],
            max_side: 8192,
            decode_requests,
            decoded,
            last_request_number: 0,
            last_drawn: None,
            fade: None,
            opened: Instant::now(),
            started: Some(Instant::now()),
            frame_ms: [0; 256],
            profile_changes: Default::default(),
        }
    }

    /// The colour profiles chosen and being read in the background.
    pub(crate) fn profile_changes(&mut self) -> &mut crate::images::colour::ProfileChanges {
        &mut self.profile_changes
    }

    /// Whether Auto Rotate is on, shared with the thumbnail workers, which turn by it as well.
    pub(crate) fn auto_rotate(&self) -> &Arc<AtomicBool> {
        &self.auto_rotate
    }

    /// The workers apply the EXIF orientation, or not.
    pub(crate) fn set_auto_rotate(&self, on: bool) {
        self.auto_rotate.store(on, Ordering::Relaxed);
    }

    /// Preload `n` images on each side.
    pub(crate) fn set_preload(&self, n: usize) {
        self.preload.store(n, Ordering::Relaxed);
    }

    /// The longest side a texture may have, as the window's graphics allow.
    pub(crate) fn set_max_side(&mut self, side: u32) {
        self.max_side = side;
    }

    /// The box the shown image is decoded into, unturned.
    pub(crate) fn view_box(&self) -> [u32; 2] {
        self.view_box
    }

    /// Starts the clock of an animation again, because another image is shown.
    pub(crate) fn image_opened(&mut self) {
        self.opened = Instant::now();
    }

    /// The change of image shows at once, with no fade.
    pub(crate) fn stop_fade(&mut self) {
        self.fade = None;
    }

    /// The change of image fades in from what the panes showed `before`, when they showed
    /// anything.
    pub(crate) fn fade_from(&mut self, before: Option<Vec<Option<ShownTexture>>>) {
        self.fade = before.map(|b| (Instant::now(), b));
    }

    /// Until the first image shows, frames are not counted for the "frames:" line.
    pub(crate) fn counts_frames(&self) -> bool {
        self.started.is_none()
    }

    /// A frame that took `ms` milliseconds of the window thread's processor time.
    pub(crate) fn count_frame(&mut self, ms: u64) {
        self.frame_ms[(ms as usize).min(255)] += 1;
    }

    /// How many frames took each number of milliseconds.
    pub(crate) fn frame_times(&self) -> &[u32; 256] {
        &self.frame_ms
    }
}

/// The zoom, and where the zoomed image is in the view.
pub(crate) struct Zoom {
    /// Physical screen pixels per original pixel. `None` means fit to window.
    level: Option<f32>,
    offset: egui::Vec2,
    /// How far the shown image can be panned each way, in points. It is zero where the image
    /// fits.
    max_pan: egui::Vec2,
    /// Scale from the previous frame (physical pixels per image pixel), for the zoom percentage in
    /// the status bar.
    last_scale: f32,
    /// Mouse wheel notches not yet turned into an image step (wheel_zoom off).
    wheel_notches: f32,
}

impl Default for Zoom {
    fn default() -> Self {
        Zoom {
            level: None,
            offset: egui::Vec2::ZERO,
            max_pan: egui::Vec2::ZERO,
            last_scale: 1.0,
            wheel_notches: 0.0,
        }
    }
}

impl Zoom {
    /// The whole image fitted to the window, in the middle.
    pub(crate) fn fit(&mut self) {
        self.level = None;
        self.offset = egui::Vec2::ZERO;
    }

    /// One image pixel to one screen pixel, where the image is.
    pub(crate) fn one_to_one(&mut self) {
        self.level = Some(1.0);
    }

    pub(crate) fn is_fitted(&self) -> bool {
        self.level.is_none()
    }

    /// How far the image can be moved each way, in points. It is zero along a side where the
    /// image fits.
    pub(crate) fn pan_room(&self) -> egui::Vec2 {
        self.max_pan
    }

    /// Moves the image by `d` points, no further than it can go.
    pub(crate) fn pan_by(&mut self, d: egui::Vec2) {
        self.offset = (self.offset + d).clamp(-self.max_pan, self.max_pan);
    }

    /// The scale painted last, in screen pixels per image pixel.
    pub(crate) fn scale_shown(&self) -> f32 {
        self.last_scale
    }

    /// Gives the image `room` to move each way, as painting an image larger than the view does.
    #[cfg(test)]
    pub(crate) fn set_pan_room(&mut self, room: egui::Vec2) {
        self.max_pan = room;
    }
}

/// What belongs to the image shown, beside its place in the list (`List.shown`). That is the page
/// of a TIFF, the frame of an animation stepped to, the turn not saved yet and the file it
/// belongs to, and the part being cropped. It is made anew as a whole when the image is left
/// (`leave_image`), as `List` is when the list is read again, so nothing of one image stays for
/// the next, also not a field added later. The zoom, which Keep Zoom carries over, is not here.
#[derive(Default)]
pub(crate) struct Viewing {
    /// The page of a multi-page TIFF shown (Ctrl+Page Down/Up), from 0.
    page: usize,
    /// The frame of an animation stepped to with Ctrl+Page Up/Down. It is `None` while the
    /// animation plays.
    frame: Option<usize>,
    /// The turn of the shown image, for viewing only, as quarter turns clockwise. They apply
    /// after mirroring when the flag is set. It is reset for each image, as in Geeqie.
    turn: (u8, bool),
    /// The file `turn` belongs to, set when it is turned. The turn is saved into that file
    /// even when the list changed meanwhile and another file is at `shown`.
    turned_file: PathBuf,
    /// The part of the shown image being chosen with Edit → Crop.
    crop: Option<Crop>,
}

impl Viewing {
    /// The turn of the shown image, as quarter turns clockwise, after mirroring when the flag is
    /// set.
    pub(crate) fn turn(&self) -> (u8, bool) {
        self.turn
    }

    /// Whether the shown image is turned or mirrored by hand.
    pub(crate) fn is_turned(&self) -> bool {
        self.turn != (0, false)
    }

    /// Whether it is turned a quarter, so its width and height swap.
    pub(crate) fn is_sideways(&self) -> bool {
        self.turn.0 % 2 == 1
    }

    /// Drops the turn, because the file was written as it is shown, or changed elsewhere.
    pub(crate) fn drop_turn(&mut self) {
        self.turn = (0, false);
    }

    /// The file the turn belongs to.
    pub(crate) fn turned_file(&self) -> &Path {
        &self.turned_file
    }

    /// The file the turn belongs to was moved or renamed to `to`.
    pub(crate) fn turned_file_moved(&mut self, to: PathBuf) {
        self.turned_file = to;
    }

    /// The page of a multi-page TIFF shown, from 0.
    pub(crate) fn page(&self) -> usize {
        self.page
    }

    /// The frame of an animation stepped to. It is `None` while the animation plays.
    pub(crate) fn frame(&self) -> Option<usize> {
        self.frame
    }

    /// Lets an animation stepped to a frame play on from there. Returns false when it was
    /// playing already.
    pub(crate) fn play_on(&mut self) -> bool {
        self.frame.take().is_some()
    }

    pub(crate) fn is_cropping(&self) -> bool {
        self.crop.is_some()
    }

    /// Edit → Crop begins with `crop`.
    pub(crate) fn start_crop(&mut self, crop: Crop) {
        self.crop = Some(crop);
    }

    /// The crop ends without saving.
    pub(crate) fn stop_crop(&mut self) {
        self.crop = None;
    }

    /// The part being chosen, to change as the pointer drags.
    pub(crate) fn crop_mut(&mut self) -> Option<&mut Crop> {
        self.crop.as_mut()
    }

    pub(crate) fn crop(&self) -> Option<&Crop> {
        self.crop.as_ref()
    }

    /// The part chosen, taken to be saved.
    pub(crate) fn take_crop(&mut self) -> Option<Crop> {
        self.crop.take()
    }
}

/// For tests of other modules, which start from an image turned by hand.
#[cfg(test)]
impl Viewing {
    pub(crate) fn set_turn(&mut self, turn: (u8, bool), file: PathBuf) {
        self.turn = turn;
        self.turned_file = file;
    }
}

/// The message shown over the image until the next image, with the number it was given. A
/// screen reader reads a notice out again when it is given again with the same text, and the
/// number tells it that the notice is new.
#[derive(Default)]
pub(crate) struct Notice {
    /// The text shown, and its number.
    shown: Option<(String, u64)>,
    /// How many notices were given, so the next one gets a number of its own.
    given: u64,
    /// The warnings that must reach the user, kept until a frame has drawn them. Every notice
    /// given meanwhile shows them under its own text, so a notice of the same frame cannot
    /// hide them.
    kept_until_drawn: Vec<String>,
}

impl Notice {
    /// Returns a notice that shows `text` from the start, when there is one.
    pub(crate) fn new(text: Option<String>) -> Self {
        let mut notice = Notice::default();
        if let Some(text) = text {
            notice.give(text);
        }
        notice
    }

    /// Shows `text`, as a new notice, also when the same text is shown already. The warnings
    /// that no frame has drawn yet are shown under it.
    pub(crate) fn give(&mut self, text: String) {
        let lines = std::iter::once(text).chain(self.kept_until_drawn.iter().cloned());
        let text = lines
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        self.given += 1;
        self.shown = Some((text, self.given));
    }

    /// Shows `warning` under the notice shown, and under every notice given after it until a
    /// frame has drawn it (`frame_drawn`). A warning given as a plain notice was hidden by
    /// the next notice of the same frame, and so never shown nor read out.
    pub(crate) fn keep_until_drawn(&mut self, warning: String) {
        let shown = self.shown.take().map(|(text, _)| text).unwrap_or_default();
        let lines = [shown, warning.clone()];
        let text = lines
            .into_iter()
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>();
        self.given += 1;
        self.shown = Some((text.join("\n"), self.given));
        self.kept_until_drawn.push(warning);
    }

    /// Lets the warnings kept until a frame has drawn them go, because this frame drew them.
    /// They stay shown until the next notice takes their place.
    pub(crate) fn frame_drawn(&mut self) {
        self.kept_until_drawn.clear();
    }

    /// Takes the notice away. The warnings that no frame has drawn yet stay shown.
    pub(crate) fn clear(&mut self) {
        self.shown = None;
        if !self.kept_until_drawn.is_empty() {
            self.give(String::new());
        }
    }

    /// Returns the text shown, or `None` when no notice is shown.
    pub(crate) fn text(&self) -> Option<&str> {
        self.shown.as_ref().map(|(text, _)| text.as_str())
    }

    /// Returns the number of the notice shown, or `None` when no notice is shown.
    #[cfg(feature = "accessibility")]
    pub(crate) fn number(&self) -> Option<u64> {
        self.shown.as_ref().map(|&(_, number)| number)
    }

    /// Takes the notice away and returns it, with its number, for `put_back`.
    pub(crate) fn set_aside(&mut self) -> Option<(String, u64)> {
        self.shown.take()
    }

    /// Shows again the notice that `set_aside` returned, as the same notice, so a screen
    /// reader that read it does not read it again. `None` takes the notice away. A pair with
    /// a number that this notice never gave was not set aside, so it is given as a new notice.
    pub(crate) fn put_back(&mut self, kept: Option<(String, u64)>) {
        match kept {
            Some((text, number)) if number == 0 || number > self.given => self.give(text),
            None => self.clear(),
            kept => self.shown = kept,
        }
    }
}

impl crate::App {
    // ---- Decoding and painting the image ----

    /// Ask the decoders for file `idx` at the size shown, or at full resolution, unless it is
    /// there, failed or asked for already.
    pub(crate) fn request_decode(&mut self, idx: usize, full: bool) {
        let have = self
            .list
            .loaded_images()
            .get(&idx)
            .is_some_and(|s| s.is_full_resolution || !full);
        if !have
            && !self.list.decode_errors().contains_key(&idx)
            && !self.list.decodes_pending().contains_key(&(idx, full))
        {
            self.decoding.last_request_number += 1;
            let path = self.path(idx);
            let max = match full {
                // The largest texture, also for an SVG, so that a zoom past 100 % draws it
                // sharp.
                true => [self.decoding.max_side; 2],
                false => self.decode_box(idx),
            };
            self.list.ask_decode(
                (idx, full),
                (self.decoding.last_request_number, path.clone(), max),
            );
            // The neighbours are preloaded at their first page.
            let page = if idx == self.list.shown() {
                self.viewing.page
            } else {
                0
            };
            let _ = (self.decoding.decode_requests).send(DecodeJob {
                full_resolution: full,
                path,
                page,
                request_number: self.decoding.last_request_number,
                max_box: max,
            });
        }
    }

    /// Drops textures outside the preload window, and the full-resolution versions of images
    /// other than the shown one, and asks for the missing ones.
    pub(crate) fn refill_preload(&mut self) {
        if self.list.files().is_empty() {
            return;
        }
        self.fill_panes();
        // The shown image first, then its neighbours in the order the list shows.
        let (shown, ahead) = (
            self.list.shown(),
            self.decoding.preload.load(Ordering::Relaxed) as isize,
        );
        let mut window = vec![shown];
        // The compared image or the other panes come next, because they zoom with the shown one
        // and may be loaded at full resolution too.
        for i in self.images_in_other_panes() {
            if !window.contains(&i) {
                window.push(i);
            }
        }
        let full = window.len();
        self.decoding
            .full_resolution_count
            .store(full, Ordering::Relaxed);
        let n = self.list.panes().len() + 1;
        let upcoming =
            (1..=n as isize).filter_map(|d| self.step_from(self.list.last_slide(), d, true));
        for i in self
            .list
            .panes()
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
        self.list.keep_loaded_images(|i, s| {
            window.contains(i) && (!s.is_full_resolution || window[..full].contains(i))
        });
        *lock_even_if_poisoned(&self.decoding.kept_images) =
            window.iter().map(|&i| self.path(i)).collect();
        for i in window {
            self.request_decode(i, false);
        }
    }

    /// Makes textures of the decoded images that are still wanted, and keeps why for those that
    /// could not be decoded.
    pub(crate) fn receive_decoded(&mut self, ctx: &egui::Context) {
        while let Ok(d) = self.decoding.decoded.try_recv() {
            // A result whose job is no longer pending was asked for again, in another page or
            // look, or its file left the list, and it is skipped. Its place in the list is the
            // job's now, because the list may have been read again since.
            let Some(&(idx, full)) = (self.list.decodes_pending().iter())
                .find(|(_, j)| j.0 == d.request_number)
                .map(|(k, _)| k)
            else {
                continue;
            };
            let decoded_for = (self.list.take_decode((idx, full))).map_or([0; 2], |j| j.2);
            // A file that another one has taken the name of since the list was read is not
            // shown under the name. The listing that follows finds it and asks for it again.
            let identity = d.read.and_then(|f| std::num::NonZeroU64::new(f.0?.1));
            let wanted = self.is_file(idx, &d.path)
                && !crate::files::file_io::is_another_file(identity, self.list.identity(idx))
                && d.rotated == self.config.exif_rotate
                && d.page == if idx == self.list.shown() { self.viewing.page } else { 0 }
                && lock_even_if_poisoned(&self.decoding.kept_images).contains(&d.path)
                // Full resolution is kept for each image on screen: the shown one, the compared
                // one and the panes of a layout, which zoom together.
                && (!full
                    || idx == self.list.shown()
                    || self.images_in_other_panes().contains(&idx));
            match d.result {
                Some(Ok((frames, orig, hist, pages))) if wanted => {
                    // A screen-size version must not replace a full-resolution one already loaded.
                    if !full
                        && self
                            .list
                            .loaded_images()
                            .get(&idx)
                            .is_some_and(|s| s.is_full_resolution)
                    {
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
                    self.list.keep_loaded_image(
                        idx,
                        LoadedImage {
                            texture: tex,
                            path: d.path,
                            is_full_resolution: full,
                            original_size: orig,
                            frames: anim,
                            histogram: hist,
                            pages,
                            decoded_for,
                            read: d.read,
                        },
                    );
                    // A full-resolution decode keeps fewer frames of a large animation, because
                    // of its memory budget. The frame stepped to stays among them.
                    let frames = self
                        .list
                        .loaded_images()
                        .get(&idx)
                        .map_or(0, |s| s.frames.len());
                    if idx == self.list.shown()
                        && frames > 0
                        && self.viewing.frame.is_some_and(|k| k >= frames)
                    {
                        self.viewing.frame = Some(frames - 1);
                    }
                    if idx == self.list.shown()
                        && let Some(t) = self.decoding.started.take()
                    {
                        eprintln!("first image: {} ms", t.elapsed().as_millis());
                    }
                }
                Some(Err(e)) if wanted => {
                    self.list.decode_failed(idx, e);
                }
                // A worker skipped the image, because it was not among the images kept when the
                // worker took the job. The user went away from it and came back before the job
                // was taken, so it is kept again now. Nothing else asks for it while this job is
                // pending, so it is asked for again here, or the previous image would stay.
                None if !full
                    && self.is_file(idx, &d.path)
                    && !self.list.loaded_images().contains_key(&idx)
                    && lock_even_if_poisoned(&self.decoding.kept_images).contains(&d.path) =>
                {
                    self.request_decode(idx, false);
                }
                _ => {}
            }
        }
    }

    /// Decode the images and thumbnails again, to be shown in other colours.
    pub(crate) fn redecode(&mut self) {
        self.list.forget_loaded_images();
        self.thumbs_changed();
        self.decoding.last_drawn = None;
        self.refill_preload();
    }

    /// The textures each pane shows now, for a fade to start from. Returns `None` with fading off.
    pub(crate) fn shown_textures(&self) -> Option<Vec<Option<ShownTexture>>> {
        (self.fade_ms() > 0).then(|| {
            (std::iter::once(self.list.shown()).chain(self.list.panes().iter().copied()))
                .map(|i| {
                    self.list
                        .loaded_images()
                        .get(&i)
                        .map(|s| (s.texture.clone(), s.original_size))
                })
                .collect()
        })
    }

    /// Pane `k` fading from another image to `now`. Returns how far the fade is (0 to 1, eased)
    /// and the image it fades from. It ends the fade when it is done, and asks for the next
    /// frame while it runs.
    pub(crate) fn fading(
        &mut self,
        ctx: &egui::Context,
        k: usize,
        now: egui::TextureId,
    ) -> Option<(f32, egui::TextureHandle, [usize; 2])> {
        let (start, from) = self.decoding.fade.as_ref()?;
        let t = start.elapsed().as_millis() as f32 / self.fade_ms().max(1) as f32;
        if t >= 1.0 {
            self.decoding.fade = None;
            return None;
        }
        ctx.request_repaint();
        let (tex, orig) = from.get(k)?.as_ref().filter(|f| f.0.id() != now)?;
        Some((t * t * (3.0 - 2.0 * t), tex.clone(), *orig))
    }

    /// What to show while the shown image is decoded, and whether it is its thumbnail. That is
    /// the thumbnail when there is one, and otherwise the image shown before, so browsing does
    /// not flash black. A video does not get the image before, because ffmpeg takes a while for
    /// its frame, and another photo stood under its play button.
    pub(crate) fn placeholder(&self) -> Option<(egui::TextureHandle, [usize; 2], bool)> {
        let name = self.list.files().get(self.list.shown())?;
        if let Some(t) = (self.thumbs).thumbnail_of(name, self.list.identity(self.list.shown())) {
            return Some((t.clone(), t.size(), true));
        }
        if is_video(Path::new(name)) {
            return None;
        }
        self.decoding
            .last_drawn
            .as_ref()
            .map(|(t, orig)| (t.clone(), *orig, false))
    }

    /// The image shown next to the current one when comparing. It is the other of two selected
    /// files, and otherwise the next image.
    pub(crate) fn compared_image(&self) -> Option<usize> {
        if !self.is_comparing {
            return None;
        }
        // With two selected, it is the other of them. They are not collected first, because
        // Select All in a large folder would copy the whole selection every frame.
        let mut two = self.list.selected().iter().copied();
        match (self.list.selected().len(), two.next(), two.next()) {
            (2, Some(a), Some(b)) => Some(if a == self.list.shown() { b } else { a }),
            _ => self.step(1, false).filter(|&i| i != self.list.shown()),
        }
    }

    /// The images in the panes after the first, in the order of the grid. While comparing, it is
    /// the compared image in the right half. Painting, a click, Alt+arrow and decoding all ask
    /// this one function, so that each names the image the pane shows.
    pub(crate) fn images_in_other_panes(&self) -> Vec<usize> {
        if self.is_comparing {
            self.compared_image().into_iter().collect()
        } else {
            self.list.panes().to_vec()
        }
    }

    /// The columns and rows of panes on screen. Comparing is two side by side in the one pane
    /// of its layout.
    pub(crate) fn pane_grid(&self) -> [usize; 2] {
        if self.is_comparing {
            [2, 1]
        } else {
            layout_grid(&self.config.layout)
        }
    }

    /// The layout's slideshow changes one pane at a time.
    pub(crate) fn changes_one_pane_at_a_time(&self) -> bool {
        let layout = &self.config.layout;
        self.config
            .slideshow_one_image_at_a_time
            .split(',')
            .any(|l| l == layout)
    }

    /// The panes after the first show the images that follow the shown one, except while a
    /// slideshow changes them one at a time.
    pub(crate) fn fill_panes(&mut self) {
        let [c, r] = layout_grid(&self.config.layout);
        let n = c * r;
        let kept = self.slideshow.is_running()
            && self.changes_one_pane_at_a_time()
            && self.list.panes().len() + 1 == n
            && self
                .list
                .panes()
                .iter()
                .all(|&i| i < self.list.files().len());
        if kept {
            return;
        }
        let panes = (1..n as isize).filter_map(|k| self.step(k, true)).collect();
        self.list.set_panes(panes);
        self.list.set_last_slide(
            self.list
                .panes()
                .last()
                .copied()
                .unwrap_or(self.list.shown()),
        );
        self.slideshow.first_pane_next();
    }

    pub(crate) fn set_layout(&mut self, layout: &str) {
        self.config.layout = layout.to_owned();
        self.config.save();
        // Comparing is a layout of its own.
        self.is_comparing = false;
        self.refill_preload();
    }

    /// View → Layout. It shows one image, two or four, and how a slideshow changes them.
    pub(crate) fn layout_menu(&mut self, ui: &mut egui::Ui, chosen: &mut Option<Action>) {
        scroll_menu(ui, with_icon(ui, theme::icon::LAYOUT, tr("Layout")), |ui| {
            menu_item(ui, chosen, Action::NextLayout);
            ui.separator();
            for (key, label, grid) in LAYOUTS {
                let icon = match grid {
                    [1, 1] => theme::icon::SQUARE,
                    [2, 1] => theme::icon::COLUMNS,
                    [1, 2] => theme::icon::ROWS,
                    _ => theme::icon::GRID_FOUR,
                };
                let text = with_icon(ui, icon, tr(label));
                #[allow(clippy::disallowed_methods)] // It has an icon before the text, noted below.
                let radio = ui.radio(self.config.layout == key, text);
                if noted(&radio, tr(label)).clicked() {
                    self.set_layout(key);
                }
            }
            if self.config.layout == "1" {
                return;
            }
            ui.separator();
            if checkbox(ui, &mut self.config.layout_names, tr("Show File Names")).changed() {
                self.config.save();
            }
            ui.weak(tr("Slideshow in this layout:"));
            let turns = self.changes_one_pane_at_a_time();
            let mut chosen = turns;
            for (value, icon, label) in [
                (
                    false,
                    theme::icon::TOGETHER,
                    tr("All Images Change Together"),
                ),
                (true, theme::icon::CURSOR, tr("One Image at a Time")),
            ] {
                let text = with_icon(ui, icon, label);
                #[allow(clippy::disallowed_methods)] // It has an icon before the text, noted here.
                noted(&ui.radio_value(&mut chosen, value, text), label);
            }
            if chosen != turns {
                let layout = self.config.layout.clone();
                let mut list: Vec<&str> = (self.config.slideshow_one_image_at_a_time.split(','))
                    .filter(|l| !l.is_empty() && *l != layout)
                    .collect();
                if chosen {
                    list.push(&layout);
                }
                self.config.slideshow_one_image_at_a_time = list.join(",");
                self.config.save();
                self.refill_preload();
            }
        });
    }

    pub(crate) fn zoom_by(&mut self, factor: f32) {
        self.zoom.level =
            Some((self.zoom.level.unwrap_or(self.zoom.last_scale) * factor).clamp(0.02, 32.0));
    }

    /// Paints image `i` in `rect`, as one of a layout's panes after the first or as the compared
    /// image. When zoomed, it has the same zoom and offset as the shown one, and otherwise it is
    /// fitted. Returns the scale it is drawn at, for the status bar, and `None` while it is
    /// decoded or for the compared image. `k` is the pane's number, for a slideshow fading it.
    pub(crate) fn paint_pane(
        &mut self,
        ui: &egui::Ui,
        rect: egui::Rect,
        i: usize,
        zoom: Option<f32>,
        k: Option<usize>,
    ) -> Option<f32> {
        // When zoomed in further than the screen-size version holds, the full resolution is
        // loaded.
        let sharper = self.list.loaded_images().get(&i).is_some_and(|s| {
            !s.is_full_resolution
                && zoom.is_some_and(|z| {
                    s.texture.size()[0] as f32 + 1.0 < s.original_size[0] as f32 * z.min(1.0)
                })
        });
        if sharper {
            self.request_decode(i, true);
        }
        let now = self.list.loaded_images().get(&i)?.texture.id();
        let fade = k.and_then(|k| self.fading(ui.ctx(), k, now));
        let loaded = self.list.loaded_images().get(&i)?;
        let ppp = ui.ctx().pixels_per_point();
        let look = (self.config.enlarge, self.config.fit_mode);
        let zoom = zoom.map(|z| (z, self.zoom.offset));
        let (at, scale) = placed(rect, loaded.original_size, ppp, look, zoom);
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
        if self.config.checkerboard && loaded.has_transparency() {
            paint_checker(&p, at, 0, alpha(shown));
        }
        p.image(loaded.texture.id(), at, uv, alpha(shown));
        Some(scale)
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
        let Some(loaded) = self.list.loaded_images().get(&self.list.shown()) else {
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
            .flat_map(|c| loaded.histogram[c.0].iter())
            .map(|&v| height(v))
            .fold(1.0, f32::max);
        for &(c, color) in channels {
            let points = (0..256)
                .map(|i| {
                    let x = rect.left() + i as f32 * rect.width() / 255.0;
                    egui::pos2(
                        x,
                        rect.bottom() - height(loaded.histogram[c][i]) / max * rect.height(),
                    )
                })
                .collect();
            p.add(egui::Shape::line(points, egui::Stroke::new(1.5, color)));
        }
    }

    /// Full screen info in the top left corner, from the template in Preferences.
    pub(crate) fn paint_full_screen_info(&mut self, ui: &mut egui::Ui, area: egui::Rect) {
        let text = self.full_screen_info_text(ui.ctx());
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

    /// Returns the full screen info's text for the image shown, from the template in
    /// Preferences. Its EXIF is asked for here, and its fields fill once it is read.
    pub(crate) fn full_screen_info_text(&mut self, ctx: &egui::Context) -> String {
        self.request_and_take_exif(ctx);
        let path = self.path(self.list.shown());
        let meta = self.info_panel.saved_drawn_for(&path);
        fill_template(
            &self.config.full_screen_info_template,
            |field| match field {
                "name" => self.name(self.list.shown()).to_string(),
                "path" => path.display().to_string(),
                "position" => self.position_text(),
                "dimensions" => (self.list.loaded_images().get(&self.list.shown()))
                    .map(|s| format!("{} × {}", s.original_size[0], s.original_size[1]))
                    .unwrap_or_default(),
                "marks" => self.mark_text(self.list.shown()),
                "keywords" => meta.map(|m| m.keywords.join(", ")).unwrap_or_default(),
                "comment" => meta.map(|m| m.comment.clone()).unwrap_or_default(),
                label => self
                    .info_panel
                    .exif_row(&path, label)
                    .map(|value| crate::metadata::exif::shown_value(label, value).to_owned())
                    .unwrap_or_default(),
            },
        )
    }

    /// Window → Projector. It is a second window, for another screen or a projector, with only
    /// the shown image as the main window shows it, fitted or, when zoomed, the same part of it
    /// scaled to this window. It runs in the same process, so the textures are shared and
    /// nothing is decoded twice. Wayland lets no program put a window on a chosen screen, so
    /// the user drags it there, and F or a double-click makes it full screen. Its keys browse
    /// the main window.
    pub(crate) fn projector_window(&mut self, ctx: &egui::Context) {
        let id = egui::ViewportId::from_hash_of("glimt projector");
        let builder = egui::ViewportBuilder::default()
            .with_title("Glimt Projector")
            .with_app_id(app_id())
            .with_inner_size([960.0, 600.0]);
        let mut chosen = None;
        let closed = ctx.show_viewport_immediate(id, builder, |ui, _| {
            let pctx = ui.ctx().clone();
            let full = pctx.input(|i| i.viewport().fullscreen).unwrap_or(false);
            self.window.set_projector_full_screen(full);
            let (f, esc) = pctx.input(|i| {
                let f = i.key_pressed(egui::Key::F) || i.key_pressed(egui::Key::F11);
                (f && i.modifiers.is_none(), i.key_pressed(egui::Key::Escape))
            });
            let key =
                (self.shortcut_unless_dialog_open(&pctx)).filter(|a| KIOSK_ACTIONS.contains(a));
            chosen = key;
            let [r, g, b] = self.config.background.0;
            let frame = egui::Frame::NONE.fill(egui::Color32::from_rgb(r, g, b));
            egui::CentralPanel::default().frame(frame).show(ui, |ui| {
                let area = ui.max_rect();
                // When the projector is larger than the main view (full screen on a big screen),
                // the images are decoded again to its size, because they were too small for it.
                let ppp = pctx.pixels_per_point();
                let size = [(area.width() * ppp) as u32, (area.height() * ppp) as u32];
                if let Some(old) = self.window.projector_resized(size) {
                    let had = [
                        self.decoding.view_box[0].max(old[0]),
                        self.decoding.view_box[1].max(old[1]),
                    ];
                    let grown = size[0] > had[0] || size[1] > had[1];
                    // This happens only then. A projector smaller than the view shows the images
                    // it has, and dragging its edge decodes nothing again.
                    if grown {
                        self.list.keep_full_resolution_only();
                        self.refill_preload();
                    }
                }
                let response = ui.allocate_rect(area, egui::Sense::click());
                let name = self.name(self.list.shown());
                response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Image, true, &name));
                if f || response.double_clicked() || (esc && full) {
                    pctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!full));
                }
                self.paint_projector(ui, area);
            });
            pctx.input(|i| i.viewport().close_requested())
        });
        if closed {
            self.window.close_projector();
        }
        if let Some(action) = chosen {
            self.run(ctx, action);
        }
    }

    /// The shown image in the projector's `area`, as `view` showed it, and the pointer's ring.
    pub(crate) fn paint_projector(&self, ui: &egui::Ui, area: egui::Rect) {
        let Some(loaded) = self.list.loaded_images().get(&self.list.shown()) else {
            return;
        };
        let ppp = ui.ctx().pixels_per_point();
        let turns = self.viewing.turn.0;
        let [w, h] = loaded.original_size.map(|n| n as f32);
        let (uw, uh) = if turns % 2 == 1 { (h, w) } else { (w, h) };
        let fit = fit_scale(area.size() * ppp, [uw, uh], self.config.fit_mode);
        let fit = if self.config.enlarge {
            fit
        } else {
            fit.min(1.0)
        };
        // When zoomed, it shows the same part of the image, as large as this window allows.
        let (scale, pan) = match self.last_image_area {
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
        if self.config.checkerboard && loaded.has_transparency() {
            paint_checker(ui.painter(), rect, turns, egui::Color32::WHITE);
        }
        let tex = self.texture(loaded, ui.ctx());
        paint_turned(ui, tex, rect, self.viewing.turn, egui::Color32::WHITE);
        // Where the pointer is over the main window's image, to point things out.
        if self.config.projector_pointer
            && let Some(v) = self.last_image_area
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
        let loaded = self.list.loaded_images().get(&self.list.shown());
        match self.config.animate {
            true => loaded.map_or(0, |s| s.frame_at(self.decoding.opened.elapsed()).0),
            false => 0,
        }
    }

    /// What `loaded` shows now, in the main window and on the projector alike. It is the frame
    /// stepped to, the frame playing, or the image.
    pub(crate) fn texture<'a>(
        &self,
        loaded: &'a LoadedImage,
        ctx: &egui::Context,
    ) -> &'a egui::TextureHandle {
        match self.viewing.frame {
            Some(k) => loaded.frames.get(k).map_or(&loaded.texture, |f| &f.0),
            None if self.config.animate => loaded.frame(self.decoding.opened.elapsed(), ctx),
            None => &loaded.texture,
        }
    }

    /// The frame (or page) shown now. It is the one stepped to, and otherwise the page.
    pub(crate) fn shown_frame(&self) -> usize {
        let animated = self
            .list
            .loaded_images()
            .get(&self.list.shown())
            .is_some_and(|s| s.frames.len() > 1);
        match self.viewing.frame {
            Some(k) => k,
            None if animated => self.playing_frame(),
            None => self.viewing.page,
        }
    }

    /// The shown image is an animation, of more than one frame.
    pub(crate) fn is_animated(&self) -> bool {
        (self.list.loaded_images().get(&self.list.shown())).is_some_and(|s| s.frames.len() > 1)
    }

    /// The frames, or pages, of the shown file, for the status bar and the File menu's entries.
    pub(crate) fn frame_count(&self) -> usize {
        (self.list.loaded_images().get(&self.list.shown()))
            .map_or(1, |s| s.frames.len().max(s.pages))
    }

    /// The image area. It shows the shown image in one or more panes, compare, zoom and drag,
    /// the overlay, and the film strip in full screen. Returns what a click there asked for.
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
                let [cols, rows] = layout_grid(&self.config.layout);
                let (panes, area, right) =
                    pane_rects(ui.max_rect(), [cols, rows], self.is_comparing);
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
                let mode = self.config.fit_mode;
                let fit = self
                    .list
                    .loaded_images()
                    .get(&self.list.shown())
                    .map_or(1.0, |s| {
                        let [w, h] = self.turned_size(s.original_size);
                        fit_scale(area.size() * ppp, [w as f32, h as f32], mode).min(max_fit)
                    });
                let long = self.decode_size(area, ppp, max_fit);
                if response.hovered()
                    && !self.dialog_open()
                    && let Some(step) = self.wheel_input(ctx, &response, fit, area, &panes, right)
                {
                    later = Some(step);
                }
                if response.double_clicked() && !self.window.is_kiosk() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fullscreen));
                }
                let scale = self.zoom.level.unwrap_or(fit);
                self.zoom.last_scale = scale;
                self.pan_within(&response, area, scale, ppp);
                self.decode_again_if_needed(area, ppp, scale, long);
                self.paint_shown(ctx, ui, area, scale, max_fit);
                if self.viewing.crop.is_some() {
                    self.crop_over(ctx, ui, &response, area, scale, ppp);
                }
                // A video gets a play button over the preview frame (click or Enter). It can be
                // read on any frame, also in the slideshow and in full screen.
                #[cfg(feature = "video")]
                if (self.list.files().get(self.list.shown()))
                    .is_some_and(|f| is_video(Path::new(f)))
                    && play_button(ui, area)
                {
                    later = Some(Action::PlayVideo);
                }
                // With several panes, the name is shown in each pane or in none (after the panes
                // are drawn), rather than the shown image's details in one.
                if panes.len() == 1
                    && bare
                    && self.config.full_screen_info
                    && !self.list.files().is_empty()
                {
                    self.paint_full_screen_info(ui, area);
                }
                if self.config.histogram > 0 {
                    self.paint_histogram(ui, area);
                }
                clicked_pane = self.paint_panes(ui, &panes, area, right, [cols, rows], bare);
                // In full screen, a film strip of the images around this one shows while the
                // pointer is near the bottom.
                if fullscreen && !self.window.is_kiosk() && !self.list.files().is_empty() {
                    clicked_pane = self.filmstrip(ui, ui.max_rect()).or(clicked_pane);
                }
                self.paint_notice(ui, area);
                self.paint_window_id(ui, ui.max_rect());
            });
        if let Some(action) = later {
            self.run(ctx, action);
        }
        if let Some(i) = clicked_pane {
            self.show_file(ctx, i);
        }
    }

    /// A size `[w, h]` of the shown image as it is on screen, after its quarter turns.
    fn turned_size(&self, [w, h]: [usize; 2]) -> [usize; 2] {
        if self.viewing.turn.0 % 2 == 1 {
            [h, w]
        } else {
            [w, h]
        }
    }

    /// A drag pans the image at `scale`, only along a side where it is larger than `area` and
    /// only up to its edge, so an image that fits stays put.
    fn pan_within(&mut self, response: &egui::Response, area: egui::Rect, scale: f32, ppp: f32) {
        let orig = (self
            .list
            .loaded_images()
            .get(&self.list.shown())
            .map(|s| s.original_size))
        .or(self.decoding.last_drawn.as_ref().map(|d| d.1));
        if let Some([w, h]) = orig.map(|o| self.turned_size(o)) {
            // Choosing a part to crop, a drag draws it instead.
            if response.dragged() && self.viewing.crop.is_none() {
                self.zoom.offset += response.drag_delta();
            }
            let size = egui::vec2(w as f32, h as f32) * scale / ppp;
            let max = ((size - area.size()) / 2.0).max(egui::Vec2::ZERO);
            self.zoom.offset = self.zoom.offset.clamp(-max, max);
            self.zoom.max_pan = max;
        }
    }

    /// Decodes the image again when it needs it. When it is not zoomed, it is decoded again at
    /// window size when the box the view decodes into has grown past the one the texture was
    /// made for, and the previous texture is shown meanwhile. The box decides, not the
    /// texture's size, because the decoders round. The same box gives the same texture, so the
    /// image is not decoded again and again. When it is zoomed, it gets full resolution, also
    /// where the size shown is larger than a texture may be (`long`).
    fn decode_again_if_needed(&mut self, area: egui::Rect, ppp: f32, scale: f32, long: u32) {
        let loaded = self.list.loaded_images().get(&self.list.shown());
        let decode_box = self.decode_box(self.list.shown());
        let grown = loaded.is_some_and(|s| {
            !s.is_full_resolution
                && grown(decode_box, s.decoded_for, s.texture.size(), s.original_size)
        });
        let view = area.size() * ppp;
        let view = if self.viewing.turn.0 % 2 == 1 {
            [view.y, view.x]
        } else {
            [view.x, view.y]
        };
        let too_small = loaded.is_some_and(|s| {
            !s.is_full_resolution && too_small(s.texture.size(), s.original_size, scale, view)
        });
        // When not zoomed, the image is decoded again to the size shown, which the decoders'
        // limit now holds. It gets full resolution only where that is larger than a texture may
        // be.
        let fits = loaded.is_some_and(|s| fits_texture(s.original_size, scale, long));
        let fitted = self.zoom.level.is_none() && fits;
        if fitted && grown {
            self.list.forget_loaded_image(self.list.shown());
            self.request_decode(self.list.shown(), false);
        } else if !fitted && too_small {
            self.request_decode(self.list.shown(), true);
        }
    }

    /// Edit → Crop over the shown image in `area` at `scale`. The part is chosen by a drag.
    fn crop_over(
        &mut self,
        ctx: &egui::Context,
        ui: &egui::Ui,
        response: &egui::Response,
        area: egui::Rect,
        scale: f32,
        ppp: f32,
    ) {
        let shown = self
            .list
            .loaded_images()
            .get(&self.list.shown())
            .map(|s| self.turned_size(s.original_size));
        match shown {
            Some([w, h]) => {
                let size = egui::vec2(w as f32, h as f32) * scale / ppp;
                let at = area.center() + self.zoom.offset;
                let bounds = egui::Rect::from_center_size(at, size);
                self.crop_input(ctx, ui, response, bounds, [w, h]);
            }
            None => self.viewing.crop = None,
        }
    }

    /// Paints the other panes and the compared half `right`. It keeps what each pane shows, for
    /// the status bar (the one under the pointer is outlined), and the names in each pane.
    /// Returns the pane clicked, whose image becomes the shown one.
    fn paint_panes(
        &mut self,
        ui: &mut egui::Ui,
        panes: &[egui::Rect],
        area: egui::Rect,
        right: Option<egui::Rect>,
        [cols, rows]: [usize; 2],
        bare: bool,
    ) -> Option<usize> {
        let mut clicked_pane = None;
        // What each pane shows, for the status bar, which tells the one under the pointer.
        let mut shown = vec![(self.list.shown(), Some(self.zoom.last_scale))];
        // The compared half is the one other pane while comparing. It has a line between the
        // halves and always its name, and a slideshow does not fade it.
        let others = match right {
            Some(right) => {
                ui.painter().vline(
                    right.left() - 1.0,
                    right.y_range(),
                    egui::Stroke::new(2.0, egui::Color32::from_gray(60)),
                );
                vec![right]
            }
            None => panes[1..].to_vec(),
        };
        for (k, (rect, i)) in others.iter().zip(self.images_in_other_panes()).enumerate() {
            let fading = right.is_none().then_some(k + 1);
            shown.push((i, self.paint_pane(ui, *rect, i, self.zoom.level, fading)));
            if right.is_some() {
                self.paint_name(ui, *rect, i);
            }
            // A click makes it the shown image, in the first pane.
            let pane = ui.interact(*rect, ui.scope_id().with(("pane", k)), egui::Sense::click());
            let name = self.name(i);
            pane.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Image, true, &name));
            if pane.clicked() {
                clicked_pane = Some(i);
            }
        }
        let rects: Vec<egui::Rect> = match right {
            Some(r) => vec![area, r],
            None => panes.to_vec(),
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
        let pane = hovered.filter(|&k| k < shown.len()).unwrap_or(0);
        let grid = if right.is_some() {
            [2, 1]
        } else {
            [cols, rows]
        };
        self.status_bar.panes_shown(pane, grid, shown);
        if panes.len() > 1 && self.config.layout_names && !self.list.files().is_empty() {
            let shown = std::iter::once(self.list.shown()).chain(self.images_in_other_panes());
            for (rect, i) in panes.iter().zip(shown) {
                self.paint_name(ui, *rect, i);
            }
        }
        clicked_pane
    }

    /// Works out the view's box the shown image is decoded into (`view_box`), from the fit mode
    /// and, in Fill, the image's shape. Returns the longest side a texture may have.
    pub(crate) fn decode_size(&mut self, area: egui::Rect, ppp: f32, max_fit: f32) -> u32 {
        // The image area counts, not the whole screen, so a small window on a 4K screen needs a
        // fraction of the memory (3840×2160 is 33 MB per image).
        let [w, h] = [(area.width() * ppp) as u32, (area.height() * ppp) as u32];
        let quarter = self.viewing.turn.0 % 2 == 1;
        let turned = |[w, h]: [usize; 2]| if quarter { [h, w] } else { [w, h] };
        let mode = self.config.fit_mode;
        // The image is decoded to the size shown, which is the view for a whole image. Fit
        // Width and Fit Height fill one side and may need up to the screen on the other,
        // whatever the image, so the neighbours read ahead are sharp too. Fill depends on the
        // image's shape. It uses the image's size at its scale once that is known (also while
        // it is decoded again, from the image drawn), and otherwise the longer side.
        let long = self.decoding.max_side;
        let orig = (self
            .list
            .loaded_images()
            .get(&self.list.shown())
            .map(|s| s.original_size))
        .or(self.decoding.last_drawn.as_ref().map(|d| d.1));
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
        (self.decoding.view_box, self.decoding.view_area) = (view, [w, h]);
        long
    }

    /// The box file `idx` is shrunk into for the view and the projector, as `decode_box`.
    pub(crate) fn decode_box(&self, idx: usize) -> [u32; 2] {
        decode_box(
            self.decoding.view_box,
            self.decoding.view_area,
            self.config.fit_mode == 3,
            idx == self.list.shown(),
            self.viewing.turn.0 % 2 == 1,
            self.window.projector_size(),
        )
    }

    /// Paints the shown image in `area` at `scale`, turned as the view is, fading from the one
    /// before in a slideshow. While it is decoded, it paints the thumbnail or the image before,
    /// and otherwise why there is no image.
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
        let now = self
            .list
            .loaded_images()
            .get(&self.list.shown())
            .map(|s| s.texture.id());
        let fade = now.and_then(|id| self.fading(ctx, 0, id));
        if let Some(loaded) = self.list.loaded_images().get(&self.list.shown()) {
            let size = egui::vec2(
                loaded.original_size[0] as f32,
                loaded.original_size[1] as f32,
            ) * scale
                / ppp;
            let rect = egui::Rect::from_center_size(area.center() + self.zoom.offset, size);
            let tex = self.texture(loaded, ctx);
            // Within its pane or half, which zoom makes it overflow.
            let clip = ui.clip_rect();
            ui.set_clip_rect(area.intersect(clip));
            // While a slideshow fades, the image before goes as this one comes.
            let shown = match &fade {
                Some((t, old, orig)) => {
                    let look = (self.config.enlarge, mode);
                    let zoom = self.zoom.level.map(|z| (z, self.zoom.offset));
                    let at = placed(area, *orig, ppp, look, zoom).0;
                    let uv1 = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
                    ui.painter()
                        .image(old.id(), at, uv1, alpha(1.0 - t.powi(4)));
                    *t
                }
                None => 1.0,
            };
            if self.config.checkerboard && loaded.has_transparency() {
                paint_checker(ui.painter(), rect, self.viewing.turn.0, alpha(shown));
            }
            paint_turned(ui, tex, rect, self.viewing.turn, alpha(shown));
            ui.set_clip_rect(clip);
            self.decoding.last_drawn = Some((loaded.texture.clone(), loaded.original_size));
            // The projector follows this view, and the pointer measured from the image's
            // centre.
            let centre = area.center() + self.zoom.offset;
            let pointer = (ctx.pointer_hover_pos())
                .filter(|p| area.contains(*p))
                .map(|p| p - centre);
            self.last_image_area = Some(ImageArea {
                area,
                ppp,
                scale,
                pan: self.zoom.offset,
                zoomed: self.zoom.level.is_some(),
                pointer,
            });
        } else if let Some((tex, orig, thumb)) = self.placeholder()
            && !self.list.decode_errors().contains_key(&self.list.shown())
        {
            // A thumbnail fills the area, as the image will, while the image before keeps its
            // size.
            let size = egui::vec2(orig[0] as f32, orig[1] as f32);
            let fit = (area.width() * ppp / size.x).min(area.height() * ppp / size.y);
            let fit = if thumb { fit } else { fit.min(max_fit) };
            let rect = egui::Rect::from_center_size(area.center(), size * fit / ppp);
            egui::Image::new(&tex).paint_at(ui, rect);
        } else {
            let text = if self.list.files().is_empty() {
                if self.folder.is_listing() {
                    tr("Reading folder…")
                } else {
                    tr("No images in folder")
                }
                .to_owned()
            } else if let Some(e) = self.list.decode_errors().get(&self.list.shown()) {
                crate::tf!(
                    "Could not open {name}:\n{e}",
                    name = self.path(self.list.shown()).display(),
                    e
                )
            } else {
                String::new()
            };
            // The text is broken into lines that fit, because an error, such as a decoder's
            // message, can be long.
            let (font, grey) = (egui::FontId::proportional(16.0), egui::Color32::GRAY);
            let width = (area.width() - 32.0).max(80.0);
            let galley = ui.painter().layout(text, font, grey, width);
            let at = area.center() - galley.size() / 2.0;
            ui.painter().galley(at, galley, grey);
        }
    }

    /// Paints the notice at the foot of `area`, wrapped to it, on a dark band so it reads over
    /// any image. This is the one place a notice is shown, so a screen reader hears every
    /// notice from here.
    pub(crate) fn paint_notice(&self, ui: &egui::Ui, area: egui::Rect) {
        #[cfg(feature = "accessibility")]
        crate::ui::accessibility::speak_notice(ui, area, &self.notice);
        if let Some(notice) = self.notice.text() {
            let (font, red) = (egui::FontId::proportional(16.0), egui::Color32::LIGHT_RED);
            let width = (area.width() - 48.0).max(80.0);
            let galley = ui.painter().layout(notice.to_owned(), font, red, width);
            let at =
                area.center_bottom() - egui::vec2(galley.size().x / 2.0, 16.0 + galley.size().y);
            let band = egui::Rect::from_min_size(at, galley.size()).expand(8.0);
            ui.painter()
                .rect_filled(band, 6.0, egui::Color32::from_black_alpha(200));
            ui.painter().galley(at, galley, red);
        }
    }

    /// Paints the window's ID large in the middle of `area` while `--remote identify` asks for
    /// it, so that a user with several windows can see which is which. A frame is asked for at
    /// the end of that time, so the ID goes away without other input.
    pub(crate) fn paint_window_id(&self, ui: &egui::Ui, area: egui::Rect) {
        let Some(left) = self.remote.identify_time_left() else {
            return;
        };
        ui.ctx().request_repaint_after(left);
        let id = crate::features::remote::window_id();
        let size = (area.height() / 6.0).clamp(32.0, 160.0);
        let (font, white) = (egui::FontId::proportional(size), egui::Color32::WHITE);
        let galley = ui.painter().layout(id, font, white, area.width() - 32.0);
        let at = area.center() - galley.size() / 2.0;
        let band = egui::Rect::from_min_size(at, galley.size()).expand(size / 4.0);
        ui.painter()
            .rect_filled(band, size / 6.0, egui::Color32::from_black_alpha(200));
        ui.painter().galley(at, galley, white);
    }

    /// The wheel over the image. It zooms about the point under the pointer (Ctrl+wheel, or
    /// every wheel with wheel_zoom). Otherwise a notch goes to the next or previous image, which
    /// it returns. `fit` is the zoom that fits, `area` the first pane (or compared half), and
    /// `right` the other half when comparing.
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
            // One notch is one image, and touchpad deltas add up to a notch.
            self.zoom.wheel_notches += notches;
            if self.zoom.wheel_notches.abs() >= 1.0 {
                let up = self.zoom.wheel_notches > 0.0;
                step = Some(if up { Action::Previous } else { Action::Next });
                self.zoom.wheel_notches = 0.0;
            }
        }
        if factor != 1.0 {
            let old = self.zoom.level.unwrap_or(fit);
            let new = (old * factor).clamp(0.02, 32.0);
            self.zoom.level = Some(new);
            // Keep the point under the pointer in place, as in other viewers.
            if let Some(p) = response.hover_pos() {
                // The zoom is about the centre of the pane under the pointer, because the panes
                // share the offset.
                let under = (right.iter().chain(panes))
                    .find(|r| r.contains(p))
                    .map_or(area, |r| if *r == panes[0] { area } else { *r });
                let from_centre = p - under.center();
                self.zoom.offset = from_centre - (from_centre - self.zoom.offset) * (new / old);
            }
        }
        step
    }

    /// Enter. A video plays in the player (an image has nothing to play), and a slideshow waits
    /// meanwhile until P resumes it.
    #[cfg_attr(not(feature = "video"), allow(unused_variables))] // ctx plays a video only
    pub(crate) fn play_video(&mut self, ctx: &egui::Context) {
        #[cfg(feature = "video")]
        if is_video(&self.location()) {
            self.open_in(&self.config.video_player, vec![self.location()]);
            // The slideshow waits while the video plays, until P resumes it.
            if self.slideshow.is_running() {
                self.slideshow.pause();
                self.update_window_title(ctx);
            }
        }
    }

    /// Ctrl+Page Up/Down on an animation. It stops at the frame before or after the one shown,
    /// and A plays on from there.
    pub(crate) fn step_frame(&mut self, action: Action) {
        let n = self
            .list
            .loaded_images()
            .get(&self.list.shown())
            .map_or(1, |s| s.frames.len());
        let at = self.viewing.frame.unwrap_or_else(|| self.playing_frame());
        self.viewing.frame = Some(match action {
            Action::NextPage => (at + 1) % n,
            _ => (at + n - 1) % n,
        });
    }

    /// Ctrl+Page Up/Down. It decodes the next or previous page of a multi-page image anew.
    pub(crate) fn step_page(&mut self, action: Action) {
        let pages = self
            .list
            .loaded_images()
            .get(&self.list.shown())
            .map_or(1, |s| s.pages);
        let page = match action {
            Action::NextPage => (self.viewing.page + 1).min(pages - 1),
            _ => self.viewing.page.saturating_sub(1),
        };
        if page != self.viewing.page {
            self.viewing.page = page;
            self.list.forget_loaded_image(self.list.shown());
            self.list.forget_decodes_of(self.list.shown());
            self.refill_preload();
        }
    }

    /// Alt+arrow. As with a click on that pane, its image becomes the shown one, in the first
    /// pane.
    pub(crate) fn go_to_pane(&mut self, ctx: &egui::Context, action: Action) {
        let [cols, rows] = self.pane_grid();
        // In one column there is no pane to the side, and in one row none above or below. The
        // key does nothing there, rather than what another arrow does.
        let across = matches!(action, Action::PaneLeft | Action::PaneRight);
        if cols * rows > 1 && if across { cols == 1 } else { rows == 1 } {
            return;
        }
        let cols = cols as isize;
        let step = match action {
            Action::PaneLeft => -1,
            Action::PaneRight => 1,
            Action::PaneUp => -cols,
            _ => cols,
        };
        // Forward, the panes say which image is there, because a slideshow may change them one
        // at a time. Back, the list says it.
        let pane = (step > 0)
            .then(|| self.images_in_other_panes().get(step as usize - 1).copied())
            .flatten();
        if let Some(i) = pane.or_else(|| self.step(step, false)) {
            self.show_file(ctx, i);
        }
    }

    /// E. It shows two images side by side, in one pane.
    pub(crate) fn toggle_compare(&mut self) {
        self.is_comparing = !self.is_comparing;
        if self.is_comparing && self.config.layout != "1" {
            self.config.layout = "1".into();
            self.config.save();
        }
        self.refill_preload();
    }

    /// Turns or mirrors the shown image for viewing, until it is saved. To mirror or flip what
    /// is on screen after a turn, it mirrors the image first and turns the other way. (A flip
    /// is a mirror and half a turn.)
    pub(crate) fn turn(&mut self, action: Action) {
        // Until the image is saved, the old picture shows. Turned again, it would be saved
        // twice, into the same temporary file.
        if self
            .file_operations
            .is_saving(&self.path(self.list.shown()))
        {
            self.notice
                .give(tr("Still saving the last rotation of this file").into());
            return;
        }
        let (q, m) = self.viewing.turn;
        self.viewing.turned_file = self.path(self.list.shown());
        self.viewing.turn = match action {
            Action::RotateCw => ((q + 1) % 4, m),
            Action::RotateCcw => ((q + 3) % 4, m),
            Action::Rotate180 => ((q + 2) % 4, m),
            Action::Mirror => ((4 - q) % 4, !m),
            _ => ((6 - q) % 4, !m),
        };
    }

    /// Fit, Fit Width, Fit Height and Fill. These modes stay for the next images, as
    /// ImageGlass's do, and X goes back to the whole image.
    pub(crate) fn set_fit_mode(&mut self, action: Action) {
        self.zoom.fit();
        self.config.fit_mode = match action {
            Action::FitWidth => 1,
            Action::FitHeight => 2,
            Action::FitFill => 3,
            _ => 0,
        };
        self.config.save();
    }
}

/// Where an image of `orig` is drawn in `rect`, and its scale. It is drawn at the `zoom` and
/// pan given, and otherwise fitted in fit `mode`, centred, and larger than its size only with
/// `enlarge`.
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

/// Paints a checkerboard under the image at `rect`, turned as the image is, for an image that is
/// transparent. It is one quad of a repeating 2×2 texture, with 8-point squares at any size.
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

/// A tint that fades to `alpha` (0 to 1). A fade draws the image before at 1 - t⁴ under the new
/// one at t. The sum stays near full brightness, where 1 - t would darken the middle.
pub(crate) fn alpha(a: f32) -> egui::Color32 {
    egui::Color32::from_white_alpha((a.clamp(0.0, 1.0) * 255.0) as u8)
}

/// The scale (screen pixels per image pixel) at which an image of `size` fits `view`, both in
/// pixels, in fit `mode`. Mode 0 fits the whole image, 1 its width, 2 its height, and 3 fills
/// the view.
pub(crate) fn fit_scale(view: egui::Vec2, size: [f32; 2], mode: u8) -> f32 {
    let (x, y) = (view.x / size[0], view.y / size[1]);
    match mode {
        1 => x,
        2 => y,
        3 => x.max(y),
        _ => x.min(y),
    }
}

/// Whether a texture of `tex` is too small to show an image of `orig` zoomed to `scale` in a
/// view of `view` pixels (all unturned). Then full resolution is loaded, once.
///
/// The decoders fit an image into a box of whole pixels. The side that fills the view comes out
/// within a pixel or so of it, and the other side is worked out from it, so one pixel there is
/// `orig` side to side here. (4000×2657 into a box 1328 high is 1998 wide, where 1328.9 high
/// shows it 2000.6 wide.) When it is short by more than that on any side, it is decoded again,
/// which then gives a texture within that pixel or so, so it is not decoded again and again. A
/// strip of 10000×5 grown from 1920 to 3840 wide is too small by its width, because its height
/// fills nothing.
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
/// box `now`. That happens when `now` is larger on a side, unless the texture is the whole image
/// already.
pub(crate) fn grown(now: [u32; 2], was: [u32; 2], tex: [usize; 2], orig: [usize; 2]) -> bool {
    let larger = (0..2).any(|i| now[i] > was[i]);
    larger && (0..2).any(|i| tex[i] < orig[i])
}

/// Whether an image of `orig` shown at `scale` fits in a texture whose sides are at most `long`.
/// Then it is decoded again at that size, not at full resolution. `long` is not the decoders'
/// limit, whose free side is `long` itself in Fit Width and Fit Height.
pub(crate) fn fits_texture(orig: [usize; 2], scale: f32, long: u32) -> bool {
    orig.iter().all(|&side| side as f32 * scale < long as f32)
}

/// The box an image is decoded into. For the shown image it is the `view` box, turned with the
/// image when the user turned it a quarter (`quarter`). In Fill the view has the turn in it
/// already. For a neighbour it is the same box unturned, or in Fill the longer side's square,
/// because its shape is not known before it is decoded. (Shown larger, it is decoded again.)
/// The box is at least the projector's size, also turned with the shown image.
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

/// Starts the decode workers, `workers` threads that take jobs and send back what they decoded,
/// in a pool that replaces a worker stuck on a share that stopped answering. They skip images
/// the list has moved past (`kept_images`).
pub(crate) fn spawn_decoders(
    ctx: &egui::Context,
    workers: usize,
    kept_images: &Arc<Mutex<Vec<PathBuf>>>,
    full_resolution_count: &Arc<AtomicUsize>,
    auto_rotate: &Arc<AtomicBool>,
) -> (mpsc::Sender<DecodeJob>, mpsc::Receiver<Decoded>) {
    let (jobs, job_rx) = mpsc::channel::<DecodeJob>();
    let (done_tx, done) = mpsc::channel();
    let (kept_images, full_resolution_count, auto_rotate, ctx) = (
        kept_images.clone(),
        full_resolution_count.clone(),
        auto_rotate.clone(),
        ctx.clone(),
    );
    crate::support::threads::spawn_worker_pool(
        workers,
        STUCK_AFTER,
        job_rx,
        |_| {},
        move |job: DecodeJob| {
            let DecodeJob {
                full_resolution,
                path,
                page,
                request_number,
                max_box,
            } = job;
            // This cancels work, skipping images the user has already moved past.
            let (wanted, is_shown) = {
                let kept = lock_even_if_poisoned(&kept_images);
                let n = full_resolution_count.load(Ordering::Relaxed);
                is_wanted(&kept, n, &path, full_resolution)
            };
            // The shown image takes its bytes of the decodes' budget before the thumbnails
            // and the images read ahead.
            crate::images::decode::let_this_thread_go_first(is_shown);
            if !wanted {
                let _ = done_tx.send(Decoded {
                    rotated: false,
                    path,
                    page,
                    request_number,
                    result: None,
                    read: None,
                });
                return;
            }
            let rotated = auto_rotate.load(Ordering::Relaxed);
            let read = crate::files::file_io::fingerprint(&path);
            // The histogram here too, so the UI thread never walks the pixels.
            let result = Some(
                catch_panic_as_error(|| decode(&path, max_box, rotated, page)).map(
                    |(frames, orig, pages)| {
                        let hist = histogram(&frames[0].0);
                        (frames, orig, hist, pages)
                    },
                ),
            );
            let _ = done_tx.send(Decoded {
                rotated,
                path,
                page,
                request_number,
                result,
                read,
            });
            #[allow(unsafe_code)]
            // glibc does not return freed memory from the threads' malloc arenas on its own,
            // and decoding leaves several image-sized buffers in each thread.
            // SAFETY: malloc_trim has no preconditions beyond a working glibc malloc.
            unsafe {
                libc::malloc_trim(0)
            };
            ctx.request_repaint();
        },
    );
    (jobs, done)
}

/// Returns whether a decoder wants the job for the file `path` (at full resolution when
/// `full`), and whether it is of the shown image: the files `kept` hold it, among their first
/// `full_count` for full resolution, and it is the first of them. A job goes by its file, not
/// by its place in the list, which a listing may have changed while the job waited.
fn is_wanted(kept: &[PathBuf], full_count: usize, path: &Path, full: bool) -> (bool, bool) {
    let wanted = kept.iter().any(|k| k == path)
        && (!full || kept.iter().take(full_count).any(|k| k == path));
    (wanted, kept.first().is_some_and(|k| k == path))
}

/// Draws a play button over a video's preview frame in `area`, readable on any frame. Returns
/// whether it was clicked.
#[cfg(feature = "video")]
pub(crate) fn play_button(ui: &egui::Ui, area: egui::Rect) -> bool {
    let shade = egui::Color32::from_black_alpha(150);
    let button = egui::Rect::from_center_size(area.center(), egui::vec2(96.0, 96.0));
    let play = ui.interact(button, ui.scope_id().with("play"), egui::Sense::click());
    let text = tr("Video: Enter or click to play");
    play.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, text));
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
        text.into(),
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

/// Draws the sign of a file Check Images found broken, an orange disc with "!", at `centre`.
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

/// The most milliseconds that `share` of the counted frames took. It is 0 when no frame was
/// counted.
pub(crate) fn frame_at(frame_ms: &[u32], share: f64) -> usize {
    let total: u32 = frame_ms.iter().sum();
    let mut seen = 0;
    (frame_ms.iter().position(|&n| {
        seen += n;
        total > 0 && seen as f64 >= share * total as f64
    }))
    .unwrap_or(0)
}

/// The panes of a layout `[cols, rows]` in `max`, and the area of the shown image in the first.
/// When comparing, the shown image is on the left half and the other on the right.
fn pane_rects(
    max: egui::Rect,
    [cols, rows]: [usize; 2],
    compare: bool,
) -> (Vec<egui::Rect>, egui::Rect, Option<egui::Rect>) {
    let cell = max.size() / egui::vec2(cols as f32, rows as f32);
    let panes: Vec<egui::Rect> = (0..rows * cols)
        .map(|k| {
            let at = egui::vec2((k % cols) as f32, (k / cols) as f32) * cell;
            let rect = egui::Rect::from_min_size(max.min + at, cell);
            if cols * rows > 1 {
                rect.shrink(1.0)
            } else {
                rect
            }
        })
        .collect();
    let full = panes[0];
    let (area, right) = if compare && panes.len() == 1 {
        let (l, r) = full.split_left_right_at_fraction(0.5);
        (
            l.shrink2(egui::vec2(1.0, 0.0)),
            Some(r.shrink2(egui::vec2(1.0, 0.0))),
        )
    } else {
        (full, None)
    };
    (panes, area, right)
}

#[cfg(test)]
mod tests {
    use crate::support::test_support::app_on;

    /// A notice set aside comes back with its number, and a pair that was never set aside is
    /// given a number of its own, so a screen reader is not told an old number for a new text.
    #[test]
    fn a_notice_put_back_keeps_only_its_own_number() {
        let mut notice = super::Notice::new(Some("a".into()));
        let kept = notice.set_aside();
        notice.put_back(kept.clone());
        assert_eq!(notice.set_aside(), kept);
        notice.put_back(Some(("b".into(), 99)));
        assert_eq!(notice.set_aside(), Some(("b".into(), 2)));
    }

    /// Going to another image leaves nothing of the one before. Its turn, page, frame and crop
    /// are one `Viewing`, made anew, so a field added later cannot be left out of the resets.
    /// (A crop once outlived its image.)
    #[test]
    fn another_image_starts_afresh() {
        let dir = std::env::temp_dir().join(format!("glimt-test-viewing-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a.png", "b.png"] {
            image::RgbImage::new(4, 4).save(dir.join(name)).unwrap();
        }
        let (mut app, ctx) = app_on(&dir);
        app.config.save_orientation = false;
        app.viewing.turn = (1, true);
        app.viewing.page = 2;
        app.viewing.frame = Some(3);
        app.viewing.turned_file = dir.join("a.png");
        app.show_file(&ctx, 1);
        let v = &app.viewing;
        assert_eq!((v.turn, v.page, v.frame), ((0, false), 0, None));
        assert!(v.turned_file.as_os_str().is_empty() && v.crop.is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A decode job waiting while the list was read again, which moved its image to another
    /// place (a tree's parts with Include Subfolders), is still wanted, as it goes by its file.
    /// Before, the workers judged it by its old place and skipped it, and with parts coming
    /// faster than a slow share served the queue, the next image never showed.
    #[test]
    fn a_job_is_wanted_by_its_file_not_its_place() {
        use std::path::{Path, PathBuf};
        let kept: Vec<PathBuf> = ["/p/b.jpg", "/p/c.jpg", "/p/a.jpg"]
            .map(PathBuf::from)
            .into();
        let wanted = |path: &str, full| super::is_wanted(&kept, 1, Path::new(path), full);
        assert_eq!(wanted("/p/b.jpg", false), (true, true));
        assert_eq!(wanted("/p/b.jpg", true), (true, true));
        assert_eq!(wanted("/p/c.jpg", false), (true, false));
        assert_eq!(wanted("/p/c.jpg", true), (false, false));
        assert_eq!(wanted("/p/d.jpg", false), (false, false));
    }

    /// An SVG is asked for at full resolution in the largest texture, as a photo is, so that a
    /// zoom past 100 % draws it sharp. Before, it was asked for at the size the view counts as
    /// its own, 2048 pixels, which a zoom to 400 % stretched four times.
    #[cfg(feature = "svg")]
    #[test]
    fn an_svg_at_full_resolution_fills_the_largest_texture() {
        let dir = std::env::temp_dir().join(format!("glimt-test-svg-full-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let icon = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"100\" height=\"50\"/>";
        std::fs::write(dir.join("a.svg"), icon).unwrap();
        let (mut app, ctx) = app_on(&dir);
        app.refill_preload();
        let shown = |app: &crate::App| app.list.loaded_images().contains_key(&0);
        assert!(crate::support::test_support::run_background_until(
            &mut app, &ctx, shown
        ));
        app.request_decode(0, true);
        let largest = app.decoding.max_side;
        let asked = app.list.decodes_pending().get(&(0, true)).map(|job| job.2);
        assert_eq!(asked, Some([largest; 2]));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Alt+arrow goes to the pane in that direction, and does nothing where the layout has no
    /// pane that way: Right in Two Above Each Other is not Down.
    #[test]
    fn a_pane_key_follows_the_grid() {
        use crate::ui::actions::Action;
        let dir = std::env::temp_dir().join(format!("glimt-test-panes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a.png", "b.png", "c.png"] {
            image::RgbImage::new(4, 4).save(dir.join(name)).unwrap();
        }
        let (mut app, ctx) = app_on(&dir);
        app.config.save_orientation = false;
        for (layout, dead, live) in [
            ("1x2", Action::PaneRight, Action::PaneDown),
            ("2x1", Action::PaneDown, Action::PaneRight),
        ] {
            app.config.layout = layout.into();
            app.show_file(&ctx, 0);
            app.list.set_panes(vec![1]);
            app.go_to_pane(&ctx, dead);
            assert_eq!(app.list.shown(), 0, "{layout}");
            app.go_to_pane(&ctx, live);
            assert_eq!(app.list.shown(), 1, "{layout}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// In Compare with two files selected, Alt+Right shows the compared one, the other selected
    /// file, and not the next in the list. Up and Down do nothing, as in two side by side.
    #[test]
    fn a_pane_key_in_compare_shows_the_compared_image() {
        use crate::ui::actions::Action;
        let name = format!("glimt-test-compare-key-{}", std::process::id());
        let dir = std::env::temp_dir().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a.png", "b.png", "c.png", "d.png", "e.png"] {
            image::RgbImage::new(4, 4).save(dir.join(name)).unwrap();
        }
        let (mut app, ctx) = app_on(&dir);
        app.config.save_orientation = false;
        app.show_file(&ctx, 0);
        app.list.select([0, 4].into());
        app.toggle_compare();
        assert_eq!(app.images_in_other_panes(), [4]);
        app.go_to_pane(&ctx, Action::PaneDown);
        assert_eq!(app.list.shown(), 0);
        app.go_to_pane(&ctx, Action::PaneRight);
        assert_eq!((app.list.shown(), app.compared_image()), (4, Some(0)));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The median, 90 % and the slowest frame from the counts by milliseconds.
    #[test]
    fn frame_shares() {
        let mut ms = [0; 256];
        assert_eq!(super::frame_at(&ms, 0.5), 0);
        ms[2] = 5;
        ms[3] = 4;
        ms[90] = 1;
        let at = |share| super::frame_at(&ms, share);
        assert_eq!((at(0.5), at(0.9), at(1.0)), (2, 3, 90));
    }

    /// The fit modes for a 4:3 image in a 2:1 view: the whole, the width, the height, and
    /// filled.
    #[test]
    fn fits_in_each_mode() {
        let view = eframe::egui::vec2(200.0, 100.0);
        let fits = [0, 1, 2, 3].map(|m| super::fit_scale(view, [400.0, 300.0], m));
        assert_eq!(fits, [1.0 / 3.0, 0.5, 1.0 / 3.0, 0.5]);
    }

    /// A photo shown larger after the window grew is decoded again at the size shown, whatever
    /// the fit mode. Only a photo shown larger than a texture may be needs full resolution.
    #[test]
    fn decodes_again_at_the_size_shown() {
        // 6000×4000 shown at 0.5 fits in a texture of at most 16384 as 3000×2000.
        assert!(super::fits_texture([6000, 4000], 0.5, 16384));
        // A panorama shown 20,000 pixels wide does not.
        assert!(!super::fits_texture([40000, 2000], 0.5, 16384));
    }

    /// A fitted image is decoded again only for a larger box than its texture was made for. The
    /// same box gives the same texture, whatever the decoders round. (A JPEG of 13484×2499 in
    /// Fit Height came out 6014 wide where 6021 were shown, again and again.) A texture of the
    /// whole image needs no larger one.
    #[test]
    fn decodes_again_only_for_a_larger_box() {
        assert!(!super::grown(
            [16384, 1115],
            [16384, 1115],
            [6014, 1115],
            [13484, 2499]
        ));
        assert!(super::grown(
            [16384, 1116],
            [16384, 1115],
            [6014, 1115],
            [13484, 2499]
        ));
        // A strip of 10000×5 fitted 1920 wide, the window then 3840 wide.
        assert!(super::grown(
            [3840, 1000],
            [1920, 1000],
            [1920, 1],
            [10000, 5]
        ));
        // A smaller box, or a small image that is whole already, is not decoded again.
        assert!(!super::grown(
            [1000, 600],
            [1920, 1080],
            [960, 540],
            [4000, 2250]
        ));
        assert!(!super::grown(
            [3840, 2160],
            [1920, 1080],
            [640, 480],
            [640, 480]
        ));
    }

    /// A photo fitted into a window whose height is not whole pixels is not decoded again and
    /// again. Its width comes out two pixels short of the scale shown, and its height within
    /// one. A grown window, 1:1, and a strip whose window grew only along it are decoded again.
    #[test]
    fn a_texture_short_on_one_side_is_the_size_shown() {
        let view = [2614.0, 1328.9];
        let fit = 1328.9 / 2657.0;
        assert!(!super::too_small([1998, 1328], [4000, 2657], fit, view));
        assert!(super::too_small(
            [1998, 1328],
            [4000, 2657],
            0.75,
            [4000.0, 2000.0]
        ));
        assert!(super::too_small([1998, 1328], [4000, 2657], 1.0, view));
        assert!(!super::too_small([4000, 2657], [4000, 2657], 1.0, view));
        // In Fit Width, the width fills, and the height is worked out from it.
        assert!(!super::too_small(
            [2000, 1328],
            [4000, 2657],
            2000.6 / 4000.0,
            [2000.6, 8192.0]
        ));
        // A strip of 10000×5 is fitted 1920 wide, and then the window grows to 3840 wide.
        assert!(!super::too_small(
            [1920, 1],
            [10000, 5],
            0.192,
            [1920.0, 1000.0]
        ));
        assert!(super::too_small(
            [1920, 1],
            [10000, 5],
            0.384,
            [3840.0, 1000.0]
        ));
    }

    /// The box turns with the shown image only, Fill has the turn in its view already, a Fill
    /// neighbour gets the longer side's square, and the projector counts, turned with the image.
    #[test]
    fn decode_box_turns_with_the_shown_image_only() {
        let b = |shown, quarter, fill| {
            super::decode_box([1920, 1080], [1920, 1080], fill, shown, quarter, [0, 0])
        };
        assert_eq!(b(true, false, false), [1920, 1080]);
        assert_eq!(b(true, true, false), [1080, 1920]);
        assert_eq!(b(false, true, false), [1920, 1080]);
        assert_eq!(b(false, false, true), [1920, 1920]);
        assert_eq!(b(true, true, true), [1920, 1080]);
        let with_projector =
            super::decode_box([800, 600], [800, 600], false, true, true, [1920, 1080]);
        assert_eq!(with_projector, [1080, 1920]);
        let neighbour = super::decode_box([800, 600], [800, 600], false, false, true, [1920, 1080]);
        assert_eq!(neighbour, [1920, 1080]);
    }
}
