//! The background workers: decoding thumbnails, and what the window collects from every background
//! job each frame.

use crate::images::colour::srgb_to_display;
use crate::images::decode::color_image;
use crate::support::lang::tr;
use crate::support::threads::{CPU_CHANGED, Took, cores, thumb_threads};
use crate::ui::file_list::LARGE_THUMBS;
use crate::ui::file_list::ThumbView;
use crate::{features::remote, images::thumbs, malloc_trim, metadata::lightroom, tf};
use eframe::egui;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};

impl crate::App {
    /// What the background threads and the rest of the desktop sent since the last frame: decoded
    /// images and thumbnails, the folder listing, saves, remote commands, dropped files, mounts,
    /// notices and the file dialog's answer.
    pub(crate) fn poll_background(&mut self, ctx: &egui::Context) {
        if let Some(error) = crate::files::file_io::take_save_error() {
            self.notice = Some(error);
        }
        self.receive_desktop(ctx);
        self.decoding
            .set_max_side(ctx.input(|i| i.max_texture_side) as u32);
        self.receive_decoded(ctx);
        self.receive_thumbs(ctx);
        self.receive_listing(ctx, std::time::Duration::ZERO);
        self.receive_sizes();
        self.receive_exif_index(ctx);
        self.receive_deleted(ctx);
        self.selection_size(ctx);
        self.match_size(ctx);
        self.receive_file_work(ctx);
        // --slideshow: once the folder is read, so random order shuffles all of it.
        if self.slideshow.starts_now(!self.folder.is_listing()) {
            self.start_slideshow(ctx);
        }
        self.receive_focus(ctx);
        self.receive_remote(ctx);
        if let Some(img) = self.work.take_image() {
            ctx.copy_image(img);
        }
        self.receive_dropped(ctx);
        self.receive_mount(ctx);
        if self.working() {
            ctx.set_cursor_icon(egui::CursorIcon::Progress);
        }
        if self.folder.changed() && self.config.auto_refresh {
            self.folder.relist_later();
        }
        if self.folder.wants_relist() && !self.folder.is_listing() {
            self.list(ctx);
        }
        self.receive_lightroom();
        self.receive_renames(ctx);
        while let Ok(msg) = self.notices_rx.try_recv() {
            self.notice = Some(msg);
            ctx.request_repaint();
        }
        self.slideshow_tick(ctx);
        if let Some(all) = self.folder.take_together() {
            self.open_folders(ctx, all);
        }
        self.receive_picked(ctx);
    }

    /// The desktop's look and screen profile, once read.
    fn receive_desktop(&mut self, ctx: &egui::Context) {
        if let Took::Answer(look) = self.desktop.look_rx.take() {
            self.desktop.look = look;
            self.apply_theme(ctx);
        }
        if let Took::Answer(path) = self.desktop.screen_rx.take() {
            self.desktop.screen = path;
            if self.desktop.screen.is_some() {
                self.use_screen();
            }
        }
    }

    /// File operations: the jobs and the files they touch let go when done (an `exif:` search
    /// reads what it held back for keywords written), closing once they are, and saved turns.
    fn receive_file_work(&mut self, ctx: &egui::Context) {
        // Keywords written: an `exif:` search reads what it held back for them.
        if self.work.let_go_of_ended() && self.search.holds_exif_reads() {
            self.update_search(ctx);
        }
        if self.work_left() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        } else if self.window.closes_asked() > 0 {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        while let Some((path, result)) = self.work.take_saved() {
            match result {
                Ok(()) => self.files_changed(&[path]),
                Err(e) => self.notice = Some(tf!("Could not save the orientation: {e}", e)),
            }
        }
    }

    /// The window's focus: this Glimt becomes the one `--remote` talks to, and marks another
    /// window made meanwhile are read.
    fn receive_focus(&mut self, ctx: &egui::Context) {
        let focus = ctx.input(|i| i.viewport().focused.unwrap_or(false));
        let gained = self.window.focus(focus);
        if gained && self.config.remote_control {
            remote::mark_focused();
        }
        // Back from another window, which may have marked files meanwhile.
        #[cfg(feature = "marks")]
        if gained {
            self.marks.refresh();
        }
    }

    /// A file or folder dropped on the window opens there, as File → Open would.
    fn receive_dropped(&mut self, ctx: &egui::Context) {
        let dropped = ctx.input(|i| {
            let mut paths = i.raw.dropped_files.iter().map(|f| f.path().to_path_buf());
            paths.find(|p| !p.as_os_str().is_empty())
        });
        if let Some(path) = dropped.filter(|_| !self.window.is_kiosk()) {
            self.visit(ctx, path);
        }
    }

    /// A Lightroom catalog read: the import dialog opens.
    fn receive_lightroom(&mut self) {
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
    }

    /// Rename Many done, and the renames a crash cut short, found at start.
    fn receive_renames(&mut self, ctx: &egui::Context) {
        match self.work.take_renamed() {
            Took::Answer((Some(job), result)) => self.renamed(ctx, job, result),
            Took::Answer((None, _)) | Took::Nothing => {}
            Took::Gone => self.stopped(tr("Rename Many")),
        }
        if !self.work.receive_cut_short() {
            self.stopped(tr("Looking for renames cut short"));
        }
    }

    /// A background thread ended without its answer (it panicked on something): told, so the
    /// user knows why `what` did not come, instead of a pointer or a dialog waiting for ever.
    pub(crate) fn stopped(&mut self, what: &str) {
        self.notice = Some(tf!("{what} stopped unexpectedly", what));
    }

    /// Busy pointer while the folder is read (also sorted anew), a share mounted, work done in
    /// the background, or the shown image still decoded.
    pub(crate) fn working(&self) -> bool {
        let decoding = !self.list.files.is_empty()
            && !self.list.slots.contains_key(&self.list.cur)
            && !self.list.decode_errors.contains_key(&self.list.cur);
        decoding || self.working_besides_image()
    }

    /// `working` but for the shown image's decode, which a full-screen slideshow waits on at
    /// every image not preloaded.
    pub(crate) fn working_besides_image(&self) -> bool {
        self.folder.is_listing()
            || self.folder.is_mounting()
            || self.busy.load(Ordering::Relaxed) > 0
    }
}

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
