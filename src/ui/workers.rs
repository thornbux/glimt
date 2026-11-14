//! The background workers: decoding thumbnails, and what the window collects from every background
//! job each frame.

use crate::images::colour::srgb_to_display;
use crate::images::decode::color_image;
use crate::support::lang::tr;
use crate::support::threads::lock_even_if_poisoned;
use crate::support::threads::{
    CPU_CHANGED, STUCK_AFTER, Told, Took, cores, spawn_worker_pool, thumb_threads,
};
use crate::ui::file_list::LARGE_THUMBS;
use crate::ui::file_list::OnScreenFiles;
use crate::{features::remote, images::thumbs, tf};
use eframe::egui;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, PoisonError, mpsc};

impl crate::App {
    /// What the frame does with the background work and the rest of the desktop: it takes the
    /// answers (`receive_answers`), and then what needs a frame: dropped files, the focus, the
    /// clipboard, the busy pointer, the sizes and listings to start, the slideshow's step and the
    /// failed writes to show. The remote commands are taken in `logic` (main.rs) instead.
    pub(crate) fn poll_background(&mut self, ctx: &egui::Context) {
        self.decoding
            .set_max_side(ctx.input(|i| i.max_texture_side) as u32);
        self.receive_answers(ctx);
        // A write found a name that another file has taken since the folder was read, and left
        // that file alone. The folder is read again, and the marks, which showed the change
        // before it was written.
        if self.folder.take_list_is_old() {
            #[cfg(feature = "marks")]
            self.marks.read_again();
            self.start_listing(ctx);
        }
        self.sum_selection_size(ctx);
        self.sum_match_size(ctx);
        // --slideshow: once the folder is read, so random order shuffles all of it.
        if self.slideshow.starts_now(!self.folder.is_listing()) {
            self.start_slideshow(ctx);
        }
        self.follow_focus(ctx);
        if let Some((img, _pointer)) = self.file_operations.take_image() {
            ctx.copy_image(img);
        }
        self.open_dropped_file(ctx);
        if self.working() {
            ctx.set_cursor_icon(egui::CursorIcon::Progress);
        }
        let changed = self.folder.changed_on_disk() && self.config.auto_refresh;
        if let Some(left) = self.folder.relist_after_rest(changed) {
            ctx.request_repaint_after(left);
        }
        if self.folder.wants_relist() && !self.folder.is_listing() {
            self.start_listing(ctx);
        }
        self.slideshow_tick(ctx);
        if let Some(all) = self.folder.take_together() {
            self.open_folders(ctx, all);
        }
        // The failed writes go last, so that no other notice of this frame hides them.
        if let Some(failed) = self.window.take_failed_writes_to_show() {
            self.notice.give(failed);
        }
    }

    /// Takes every answer that background threads and the desktop sent: decoded images and
    /// thumbnails, the place looked up, the folder listing, the mount, saves, reports, the
    /// window's name and the file dialog's answer. `receive_remote` calls this first, and
    /// `logic` calls that also while the window is minimized or hidden and runs no `ui`. So a
    /// busy pointer that an answer holds goes then too, and a `--remote` command that waits
    /// for the work (`--wait`, a select, `name`) is answered. Before, these were taken only in
    /// `ui`, and such a command to a hidden window got no answer until it was shown again.
    pub(crate) fn receive_answers(&mut self, ctx: &egui::Context) {
        if let Some(note) = crate::files::file_io::take_other_names_note() {
            self.notice.give(note);
        }
        self.receive_desktop(ctx);
        self.receive_window_name(ctx);
        #[cfg(feature = "marks")]
        self.receive_marks(ctx);
        self.folder.receive_sort_folders();
        self.info_panel.receive_keyword_tree();
        self.receive_decoded(ctx);
        self.receive_thumbs(ctx);
        self.receive_listing(ctx, std::time::Duration::ZERO);
        self.receive_sizes();
        self.receive_sizes_and_dates();
        self.receive_saved_order(ctx);
        self.receive_real_folders(ctx);
        self.receive_exif_index(ctx);
        self.receive_operation_report(ctx);
        self.receive_file_work(ctx);
        self.receive_mount(ctx);
        self.receive_lightroom();
        self.receive_renames(ctx);
        self.receive_picked(ctx);
        self.receive_saved_theme(ctx);
    }

    /// Takes the marks file read again, and runs a search by marks or stars again when the
    /// marks changed, by this window or another. This is the one place a search follows them,
    /// so a term anywhere in the query (`sunset stars:3`, `-mark:1`) and every way of changing
    /// marks (a key, a Lightroom import, another window) show the new matches.
    #[cfg(feature = "marks")]
    fn receive_marks(&mut self, ctx: &egui::Context) {
        self.marks.receive();
        if self.marks.take_changed() && self.search.is_about_marks() {
            self.update_search(ctx);
        }
    }

    /// The desktop's look and screen profile, once read, and the colour profiles set.
    fn receive_desktop(&mut self, ctx: &egui::Context) {
        let (look, screen) = self.desktop_settings.receive();
        if look {
            self.apply_theme(ctx);
        }
        if screen {
            self.choose_screen_profile(ctx);
        }
        self.receive_profile_changes(ctx);
    }

    /// Takes what file operations finished: their jobs and the files they touch are let go (an
    /// `exif:` search then reads the files it waited for), and the saves that worked or failed
    /// are taken. `guard_close` decides a close that waits for the files.
    pub(crate) fn receive_file_work(&mut self, ctx: &egui::Context) {
        // The keywords are written, so an `exif:` search reads the files it waited for.
        if self.file_operations.drop_ended_jobs() && self.search.exif_search_waits_for_writes() {
            self.update_search(ctx);
        }
        if self.is_writing() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
        self.take_saves(ctx);
    }

    /// Takes every answer that can tell of a failed write: the error that
    /// `file_io::save_failed` told from any thread, the notices and failures that background
    /// threads sent, and the turns saved. This is the one place they are taken, and
    /// `guard_close` calls it, so each failure is seen before the window closes. It also takes
    /// the warnings of edits made without their lock, which are shown but are not failures.
    pub(crate) fn take_saves(&mut self, ctx: &egui::Context) {
        // `save_failed` and `send_failed_write` printed theirs already.
        if let Some(error) = crate::files::file_io::take_save_error() {
            self.window.keep_failed_write(error);
        }
        // An edit that went on without its lock was made, so its warning is not a failed write:
        // a `--wait` answers done, and a close that waited for the edit goes on. It stays under
        // every notice until a frame has drawn it, so that the notice of the same work ("Saved")
        // or a failed write of the same frame does not hide it.
        if let Some(warning) = crate::files::file_io::take_lock_warning() {
            self.notice.keep_until_drawn(warning);
            ctx.request_repaint();
        }
        while let Ok(told) = self.notices.try_recv() {
            match told {
                Told::Notice(text) => self.notice.give(text),
                Told::FailedWrite(text) => self.window.keep_failed_write(text),
            }
            ctx.request_repaint();
        }
        while let Some((path, result, _pointer)) = self.file_operations.take_saved() {
            match result {
                Ok(()) => {
                    self.window.forget_turn_tried_at_close();
                    self.written_anew(std::slice::from_ref(&path));
                    self.files_changed(ctx, &[path]);
                }
                Err(e) => self.tell_failed_write(tf!("Could not save the orientation: {e}", e)),
            }
        }
    }

    /// The window's focus: this Glimt becomes the one `--remote` talks to, and marks another
    /// window made meanwhile are read.
    fn follow_focus(&mut self, ctx: &egui::Context) {
        let focus = ctx.input(|i| i.viewport().focused.unwrap_or(false));
        let gained = self.window.set_focused(focus);
        // The file that says so is Glimt's own, written in the writer's thread.
        if gained {
            crate::files::file_io::write_later(remote::mark_this_window_focused);
        }
        // Back from another window, which may have marked files meanwhile.
        #[cfg(feature = "marks")]
        if gained {
            self.marks.refresh();
        }
    }

    /// A file or folder dropped on the window opens there, as File → Open would.
    fn open_dropped_file(&mut self, ctx: &egui::Context) {
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
        match self.lightroom_catalog_answer.take() {
            Took::Nothing => {}
            Took::Gone => self.tell_background_stopped(tr("Reading the Lightroom catalog")),
            Took::Answer((Ok(import), _pointer)) => self.lightroom = Some(import),
            Took::Answer((Err(e), _pointer)) => self.notice.give(e),
        }
    }

    /// Rename Many done, and the renames a crash cut short, found at start.
    pub(crate) fn receive_renames(&mut self, ctx: &egui::Context) {
        match self.file_operations.take_rename_outcome() {
            Took::Answer((Some(job), result, _pointer)) => {
                self.finish_rename_many(ctx, job, result)
            }
            Took::Answer((None, ..)) | Took::Nothing => {}
            // The thread broke, which `run_in_background` told as a failed write, because the
            // names may be half done.
            Took::Gone => {
                if let Some(dropped) = self.send_waiting_sidecar_writes(ctx, None) {
                    self.tell_failed_write(dropped);
                }
                // A listing held while the rename ran may have been read in the middle of it,
                // so the folder is read again, as the rename's report would have had it read.
                self.start_listing(ctx);
            }
        }
        if !self.file_operations.receive_cut_short() {
            self.tell_background_stopped(tr("Looking for renames cut short"));
        }
    }

    /// A background thread ended without its answer (it panicked on something): told, so the
    /// user knows why `what` did not come, instead of a pointer or a dialog waiting for ever.
    pub(crate) fn tell_background_stopped(&mut self, what: &str) {
        self.notice.give(tf!("{what} stopped unexpectedly", what));
    }

    /// Busy pointer while the folder is read (also sorted anew), a share mounted, work done in
    /// the background, or the shown image still decoded.
    pub(crate) fn working(&self) -> bool {
        let decoding = !self.list.files().is_empty()
            && !self.list.loaded_images().contains_key(&self.list.shown())
            && !self.list.decode_errors().contains_key(&self.list.shown());
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
/// take jobs: so scrolling the list never delays the shown image. They are a pool that replaces
/// a worker stuck on a share that stopped answering (`spawn_worker_pool`).
#[allow(clippy::type_complexity)] // the two channels' types, each used once, read best whole
pub(crate) fn spawn_thumbnailers(
    ctx: &egui::Context,
    workers: usize,
    rotate: &Arc<AtomicBool>,
    on_screen: &OnScreenFiles,
    save: &Arc<AtomicBool>,
) -> (
    mpsc::Sender<(PathBuf, u64)>,
    mpsc::Receiver<(PathBuf, u64, Option<Option<egui::ColorImage>>)>,
) {
    let (thumb_jobs, thumb_rx) = mpsc::channel::<(PathBuf, u64)>();
    let (tx, thumbs_done) = mpsc::channel();
    let (on_screen, rotate, save, ctx) =
        (on_screen.clone(), rotate.clone(), save.clone(), ctx.clone());
    spawn_worker_pool(
        cores(),
        STUCK_AFTER,
        thumb_rx,
        // As many as there are cores; CPU use (thumb_threads) says how many take jobs, so it
        // changes without a restart.
        move |place| {
            let mut idle = lock_even_if_poisoned(&CPU_CHANGED.0);
            while place >= thumb_threads(workers) {
                idle = (CPU_CHANGED.1.wait(idle)).unwrap_or_else(PoisonError::into_inner);
            }
        },
        // A job goes by its file, as the decoders' do, because a listing (a tree's next part)
        // moves the files while the jobs wait.
        move |(path, n): (PathBuf, u64)| {
            let wanted =
                (lock_even_if_poisoned(&on_screen).iter()).any(|v| v.binary_search(&path).is_ok());
            let result = wanted.then(|| {
                let (rotate, save) = (rotate.load(Ordering::Relaxed), save.load(Ordering::Relaxed));
                let large = LARGE_THUMBS.load(Ordering::Relaxed);
                thumbs::thumbnail(&path, rotate, save, large)
                    .ok()
                    // sRGB in the shared cache; to the screen only to be shown.
                    .map(|img| color_image(srgb_to_display(img.into())))
            });
            let _ = tx.send((path, n, result));
            #[allow(unsafe_code)]
            // SAFETY: as for the image workers above.
            unsafe {
                libc::malloc_trim(0)
            };
            ctx.request_repaint();
        },
    );
    (thumb_jobs, thumbs_done)
}

#[cfg(test)]
mod tests {
    use crate::support::test_support::{app_on, contract_folder};

    /// A minimized window runs `logic` but no `ui`, and takes there every answer of the
    /// background work, such as the bytes a delete freed, so their busy pointer goes. Before,
    /// only `ui` took most of them, and the pointer stayed until the window was shown again.
    /// `poll_background` takes no answer of its own beside `receive_answers`, so a new one is
    /// taken in `logic` too.
    #[test]
    fn answers_that_need_no_frame_are_taken_without_one() {
        let dir = contract_folder("answers-without-a-frame");
        let (mut app, ctx) = app_on(&dir);
        app.status_bar.freed_sender().send(1).unwrap();
        assert!(app.working_besides_image());
        app.receive_remote(&ctx);
        assert!(!app.working_besides_image());
        let body_of = |text: &'static str, name: &str| {
            let body = &text[text.find(&format!("fn {name}(")).unwrap()..];
            body[..body.find("\n    }\n").unwrap()].to_owned()
        };
        let logic = body_of(include_str!("../main.rs"), "logic");
        assert!(logic.contains("self.receive_remote(ctx);"));
        let remote = body_of(include_str!("../features/remote.rs"), "receive_remote");
        assert!(remote.contains("self.receive_answers(ctx);"));
        let poll = body_of(include_str!("workers.rs"), "poll_background");
        assert_eq!(
            answers_taken_in(&poll),
            ["        self.receive_answers(ctx);"]
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The check above finds an answer taken in each way a `Receiving` gives one, and through a
    /// `take_` method of a part, but not the takes that need the frame.
    #[test]
    fn finds_every_spelling_of_an_answer_taken() {
        let body = "self.a.receive(x);\nlet b = self.b.latest();\nself.c.wait(t);\n\
            while let Some(d) = self.d.next() {}\nlet e = self.e.take_answer();\n\
            if let Some(i) = self.f.take_image() {}\nif self.g.take_list_is_old() {}\n";
        assert_eq!(
            answers_taken_in(body),
            [
                "self.a.receive(x);",
                "let b = self.b.latest();",
                "self.c.wait(t);",
                "while let Some(d) = self.d.next() {}",
                "let e = self.e.take_answer();",
            ]
        );
    }

    /// Returns the lines of `body` that take an answer of background work. The takes that need
    /// the frame are left out: the clipboard's image, a listing started for a list gone old,
    /// the folders opened together, and the failed writes shown last.
    fn answers_taken_in(body: &str) -> Vec<&str> {
        const TAKES: [&str; 7] = [
            ".receive",
            "try_recv",
            ".take()",
            ".latest()",
            ".wait(",
            ".next()",
            ".take_",
        ];
        const THE_FRAMES_OWN: [&str; 4] = [
            ".take_image()",
            ".take_list_is_old()",
            ".take_together()",
            ".take_failed_writes_to_show()",
        ];
        (body.lines())
            .filter(|l| TAKES.iter().any(|take| l.contains(take)))
            .filter(|l| !THE_FRAMES_OWN.iter().any(|own| l.contains(own)))
            .collect()
    }

    /// A failed write stays on screen when another notice comes in the same frame. Before, the
    /// failure was given first, and the notice took its place at once.
    #[test]
    fn a_failed_write_is_not_hidden_by_a_later_notice() {
        let dir = contract_folder("failure-not-hidden");
        let (mut app, ctx) = app_on(&dir);
        let failed = "Could not save the keywords: test";
        app.tell_failed_write(failed.into());
        app.notice_sender.send("3 files moved".into()).unwrap();
        app.poll_background(&ctx);
        assert_eq!(app.notice.text(), Some(failed));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The warning of an edit without its lock stays under every notice of the frame that takes
    /// it, the "Saved" of the same work, a failed write and a key's notice after them among
    /// them, until that frame is drawn. Before, it was given as a plain notice, and the notice
    /// that came with it in the same frame took its place before it was ever shown.
    #[test]
    fn a_lock_warning_is_not_hidden_by_a_notice_of_the_same_frame() {
        let dir = contract_folder("lock-warning-not-hidden");
        let (mut app, ctx) = app_on(&dir);
        // A folder where the lock file goes cannot be opened as one, as on a share without locks.
        let lock = dir.join(".marks.lock");
        std::fs::create_dir_all(&lock).unwrap();
        assert!(crate::files::file_io::lock_or_tell(&lock).is_none());
        app.notice_sender
            .send("Saved a-cropped.png".into())
            .unwrap();
        app.poll_background(&ctx);
        let shown = app.notice.text().unwrap_or_default().to_owned();
        assert!(shown.contains("Saved a-cropped.png"), "{shown:?}");
        assert!(
            shown.contains("Could not lock"),
            "hidden by the notice: {shown:?}"
        );
        assert_eq!(app.window.failed_writes_told(), 0, "told as a failed write");
        app.tell_failed_write("Could not save the keywords: test".into());
        app.poll_background(&ctx);
        app.notice.give("A key's notice".into());
        let shown = app.notice.text().unwrap_or_default().to_owned();
        assert!(shown.contains("Could not lock"), "hidden later: {shown:?}");
        app.notice.frame_drawn();
        app.notice.give("The next frame's notice".into());
        assert_eq!(app.notice.text(), Some("The next frame's notice"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
