//! The background workers: decoding thumbnails, and what the window collects from every background
//! job each frame.

use crate::colour::srgb_to_display;
use crate::decode::color_image;
use crate::lang::tr;
use crate::threads::{CPU_CHANGED, Took, cores, thumb_threads};
use crate::{Job, LARGE_THUMBS, ThumbView, config, lightroom, malloc_trim, remote, tf, thumbs};
use eframe::egui;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};

/// The thumbnail workers, one for each core of which CPU use lets `thumb_threads(workers)`
/// take jobs: so scrolling the list never delays the shown image.
#[allow(clippy::type_complexity)]
pub(crate) fn spawn_thumbnailers(
    ctx: &egui::Context,
    workers: usize,
    rotate: &Arc<AtomicBool>,
    view: &ThumbView,
    save: &Arc<AtomicBool>,
) -> (
    mpsc::Sender<(usize, PathBuf)>,
    mpsc::Receiver<(PathBuf, Option<Option<egui::ColorImage>>)>,
) {
    let (thumb_jobs, thumb_rx) = mpsc::channel::<(usize, PathBuf)>();
    let (thumbs_tx, thumbs_done) = mpsc::channel();
    let thumb_rx = Arc::new(Mutex::new(thumb_rx));
    // As many as there are cores; CPU use (thumb_threads) says how many take jobs, so it
    // changes without a restart.
    for k in 0..cores() {
        let (rx, tx, view, rotate, save, ctx) = (
            thumb_rx.clone(),
            thumbs_tx.clone(),
            view.clone(),
            rotate.clone(),
            save.clone(),
            ctx.clone(),
        );
        std::thread::spawn(move || {
            loop {
                let mut idle = CPU_CHANGED.0.lock().unwrap();
                while k >= thumb_threads(workers) {
                    idle = CPU_CHANGED.1.wait(idle).unwrap();
                }
                drop(idle);
                let Ok((idx, path)) = rx.lock().unwrap().recv() else {
                    return;
                };
                let wanted = (view.lock().unwrap().iter()).any(|v| v.binary_search(&idx).is_ok());
                let result = wanted.then(|| {
                    let (rotate, save) =
                        (rotate.load(Ordering::Relaxed), save.load(Ordering::Relaxed));
                    let large = LARGE_THUMBS.load(Ordering::Relaxed);
                    thumbs::thumbnail(&path, rotate, save, large)
                        .ok()
                        // sRGB in the shared cache; to the screen only to be shown.
                        .map(|img| color_image(srgb_to_display(img.into())))
                });
                let _ = tx.send((path, result));
                // SAFETY: as for the image workers above.
                unsafe { malloc_trim(0) };
                ctx.request_repaint();
            }
        });
    }
    (thumb_jobs, thumbs_done)
}

impl crate::App {
    /// What the background threads and the rest of the desktop sent since the last frame: decoded
    /// images and thumbnails, the folder listing, saves, remote commands, dropped files, mounts,
    /// notices and the file dialog's answer.
    pub(crate) fn poll_background(&mut self, ctx: &egui::Context) {
        if let Some(error) = config::take_save_error() {
            self.notice = Some(error);
        }
        if let Took::Answer(look) = self.desktop_rx.take() {
            self.desktop_look = look;
            self.apply_theme(ctx);
        }
        if let Took::Answer(path) = self.desktop_screen_rx.take() {
            self.desktop_screen = path;
            if self.desktop_screen.is_some() {
                self.use_screen();
            }
        }
        self.max_side = ctx.input(|i| i.max_texture_side) as u32;
        self.receive_decoded(ctx);
        self.receive_thumbs(ctx);
        self.receive_listing(ctx, std::time::Duration::ZERO);
        self.receive_sizes();
        self.receive_exif_index(ctx);
        self.receive_deleted(ctx);
        self.selection_size(ctx);
        self.match_size(ctx);
        let sidecars = |jobs: &[Arc<Job>]| jobs.iter().any(|j| j.sidecars);
        let were_writing = sidecars(&self.file_jobs);
        self.file_jobs.retain(|job| Arc::strong_count(job) > 1);
        // Keywords written: an `exif:` search reads what it held back for them.
        let held = !self.exif_stale.is_empty() && self.search.contains("exif:");
        if were_writing && !sidecars(&self.file_jobs) && held {
            self.update_search(ctx);
        }
        self.touching.retain(|t| Arc::strong_count(t) > 1);
        if self.work_left() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        } else if self.closing > 0 {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        while let Ok((path, result)) = self.saved_rx.try_recv() {
            self.saving.remove(&path);
            match result {
                Ok(()) => self.files_changed(&[path]),
                Err(e) => self.notice = Some(tf!("Could not save the orientation: {e}", e)),
            }
        }
        // --slideshow: once the folder is read, so random order shuffles all of it.
        if self.slideshow_at_start && self.listing_rx.is_none() {
            self.slideshow_at_start = false;
            self.start_slideshow(ctx);
        }
        let focus = ctx.input(|i| i.viewport().focused.unwrap_or(false));
        if focus && !self.has_focus && self.config.remote_control {
            remote::mark_focused();
        }
        // Back from another window, which may have marked files meanwhile.
        #[cfg(feature = "marks")]
        if focus && !self.has_focus {
            self.marks.refresh();
        }
        self.has_focus = focus;
        self.receive_remote(ctx);
        if let Took::Answer(img) = self.clip_rx.take() {
            ctx.copy_image(img);
        }
        // A file or folder dropped on the window opens there, as File → Open would.
        let dropped = ctx.input(|i| {
            let mut paths = i.raw.dropped_files.iter().map(|f| f.path().to_path_buf());
            paths.find(|p| !p.as_os_str().is_empty())
        });
        if let Some(path) = dropped.filter(|_| !self.kiosk) {
            self.visit(ctx, path);
        }
        self.receive_mount(ctx);
        if self.working() {
            ctx.set_cursor_icon(egui::CursorIcon::Progress);
        }
        if self.changed_rx.try_iter().count() > 0 && self.config.auto_refresh {
            self.relist = true;
        }
        if self.relist && self.listing_rx.is_none() {
            self.list(ctx);
        }
        match self.lightroom_rx.take() {
            Took::Nothing => {}
            Took::Gone => self.stopped(tr("Reading the Lightroom catalog")),
            Took::Answer(Ok(catalog)) => {
                let now = lightroom::roots_now(&catalog);
                self.lightroom = Some(lightroom::Import {
                    catalog,
                    now,
                    plan: None,
                });
            }
            Took::Answer(Err(e)) => self.notice = Some(e),
        }
        match self.renaming_rx.take() {
            Took::Answer(result) => {
                if let Some(job) = self.renaming.take() {
                    self.renamed(ctx, job, result);
                }
            }
            // Gone without a word: not waited for at closing, Rename Many not refused for ever.
            Took::Gone => {
                self.renaming = None;
                self.stopped(tr("Rename Many"));
            }
            Took::Nothing => {}
        }
        match self.cut_short_rx.take() {
            Took::Answer(found) => self.cut_short = found,
            Took::Gone => self.stopped(tr("Looking for renames cut short")),
            Took::Nothing => {}
        }
        while let Ok(msg) = self.notices_rx.try_recv() {
            self.notice = Some(msg);
            ctx.request_repaint();
        }
        self.slideshow_tick(ctx);
        if let Some(all) = self.together.take() {
            self.open_folders(ctx, all);
        }
        self.receive_picked(ctx);
    }

    /// A background thread ended without its answer (it panicked on something): told, so the
    /// user knows why `what` did not come, instead of a pointer or a dialog waiting for ever.
    pub(crate) fn stopped(&mut self, what: &str) {
        self.notice = Some(tf!("{what} stopped unexpectedly", what));
    }
}
