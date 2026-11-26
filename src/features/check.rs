//! File → Check Images: every image of the list decoded in the background, the broken ones listed,
//! and those that could not be checked.

use crate::files::file_io::{FileFingerprint, fingerprint_of_file_read, same_fingerprint};
use crate::images::decode::DecodeFailure;
use crate::support::format::{ShownText, count, escaped_name, thousands};
use crate::support::lang::tr;
use crate::support::threads::{Receiving, Took, Working, answer_channel, background_threads};
use crate::ui::menus::DrawShown;
use crate::{images::decode, tf};
use eframe::egui;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};

/// Check Images: every image of the list opened in the background, as the viewer would; those
/// that cannot be are listed, and marked in the file list.
pub(crate) struct ImageCheck {
    /// Files checked, and the threads still at work.
    done: Arc<AtomicUsize>,
    working: Arc<AtomicUsize>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    total: usize,
    /// Each file that failed: its name in the list, why, whether that is the file's fault, and
    /// its fingerprint, taken before it was read.
    found_rx: Receiving<(OsString, String, bool, Option<FileFingerprint>)>,
    /// The broken ones found: (name in the list, why, fingerprint before it was read).
    broken: Vec<(OsString, String, Option<FileFingerprint>)>,
    /// The broken files that are still as the check read them, which Select Them selects once
    /// they have been looked at in the background (`start_selecting_broken`).
    still_broken: Receiving<(Vec<OsString>, Working)>,
    /// The files that could not be checked, for a reason that says nothing about the file,
    /// such as ffmpeg not installed or a share away for a moment: (name in the list, why).
    /// They are not marked broken, as a user who trusts the mark may delete them.
    unchecked: Vec<(OsString, String)>,
}

impl ImageCheck {
    /// Stops the check, because its window closed or the list it checks is gone.
    pub(crate) fn stop(&self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

impl crate::App {
    // ---- Windows: Check Images ----

    /// The Check Images window: starts the check of every image in the list, shows how far it
    /// is, and lists the broken ones; a click shows one, Select Them selects them in the list.
    pub(crate) fn check_window(&mut self, ctx: &egui::Context) {
        let Some(mut check) = self.check_images.take() else {
            return;
        };
        if let Some(c) = &mut check {
            self.take_found(c);
            self.select_still_broken(ctx, c);
        }
        let (mut open, mut start, mut show, mut select) = (true, false, None, false);
        egui::Window::new(tr("Check Images"))
            .open(&mut open)
            .default_size([560.0, 400.0])
            .show(ctx, |ui| {
                let Some(c) = &check else {
                    ui.show_label(tf!(
                        "Opens each of the {count} in the list, as the viewer would, and lists \
                         those that cannot be: broken files, files cut off in copying, and files \
                         that are no image. They are marked in the list too.",
                        count = count(self.list.files().len(), "image")
                    ));
                    start = crate::ui::menus::button(ui, tr("Check")).clicked();
                    return;
                };
                let (done, working) = (
                    c.done.load(Ordering::Relaxed),
                    c.working.load(Ordering::Relaxed),
                );
                ui.horizontal(|ui| {
                    let progress = done as f32 / c.total.max(1) as f32;
                    let line = tf!(
                        "{done} of {total}",
                        done = thousands(done),
                        total = thousands(c.total)
                    );
                    ui.add(
                        egui::ProgressBar::new(progress)
                            .text(line)
                            .desired_width(260.0),
                    );
                    if working > 0 {
                        if crate::ui::menus::button(ui, tr("Stop")).clicked() {
                            c.stop.store(true, Ordering::Relaxed);
                        }
                        ctx.request_repaint_after(std::time::Duration::from_millis(200));
                    }
                });
                let found = [c.broken.len(), c.unchecked.len()];
                ui.show_label(summary(found, working, done, c.total));
                if !c.broken.is_empty() && crate::ui::menus::button(ui, tr("Select Them")).clicked()
                {
                    select = true;
                }
                let row = ui.text_style_height(&egui::TextStyle::Body) + 4.0;
                let rows = c.broken.len() + c.unchecked.len();
                egui::ScrollArea::vertical().auto_shrink(false).show_rows(
                    ui,
                    row,
                    rows,
                    |ui, rows| {
                        for i in rows {
                            let (name, text) = match c.broken.get(i) {
                                Some((name, why, _)) => {
                                    (name, tf!("{name}: {e}", name = escaped_name(name), e = why))
                                }
                                None => {
                                    let (name, why) = &c.unchecked[i - c.broken.len()];
                                    let text = tf!(
                                        "{name}: could not be checked: {why}",
                                        name = escaped_name(name),
                                        why
                                    );
                                    (name, text)
                                }
                            };
                            if crate::ui::menus::selectable(ui, false, &text).clicked() {
                                show = Some(name.clone());
                            }
                        }
                    },
                );
            });
        if start {
            check = Some(self.start_check(ctx));
        }
        if let Some(name) = show
            && let Some(i) = self.list.files().iter().position(|f| *f == name)
        {
            self.show_file(ctx, i);
        }
        if select && let Some(c) = &mut check {
            self.start_selecting_broken(c);
        }
        if open {
            self.check_images = Some(check);
        } else if let Some(c) = check {
            // Closed while it runs: the threads stop at their next file.
            c.stop.store(true, Ordering::Relaxed);
        }
    }

    /// Check every image of the list in background threads, as many as the CPU setting allows,
    /// each taking the next file until all are done or Stop is pressed.
    pub(crate) fn start_check(&mut self, ctx: &egui::Context) -> ImageCheck {
        self.broken_files.clear();
        let files: Arc<Vec<(OsString, PathBuf)>> = Arc::new(
            (0..self.list.files().len())
                .map(|i| (self.list.files()[i].clone(), self.path(i)))
                .collect(),
        );
        let threads = background_threads().min(files.len().max(1));
        let (tx, found) = mpsc::channel();
        let next = Arc::new(AtomicUsize::new(0));
        let check = ImageCheck {
            done: Arc::default(),
            working: Arc::default(),
            stop: Arc::default(),
            total: files.len(),
            found_rx: Receiving::of(found),
            broken: Vec::new(),
            still_broken: Receiving::none(),
            unchecked: Vec::new(),
        };
        for _ in 0..threads {
            let (files, tx, next, ctx) = (files.clone(), tx.clone(), next.clone(), ctx.clone());
            let (done, stop) = (check.done.clone(), check.stop.clone());
            // Counted down when the thread ends, also by a panic, as every count of work is.
            let working = Working::new(&check.working);
            let pointer = Working::new(&self.busy);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let Some((name, path)) = files.get(next.fetch_add(1, Ordering::Relaxed)) else {
                        break;
                    };
                    let read_as = fingerprint_of_file_read(path);
                    if let Err(why) = decode::check_decodes(path) {
                        let is_broken = decode::failure_of(path, &why) == DecodeFailure::OfTheFile;
                        let _ = tx.send((name.clone(), why, is_broken, read_as));
                    }
                    done.fetch_add(1, Ordering::Relaxed);
                }
                drop(working);
                drop(pointer);
                ctx.request_repaint();
            });
        }
        check
    }

    /// Takes the failures that the check's threads found: a broken file is listed and marked
    /// in the file list, and one that could not be checked is only listed.
    fn take_found(&mut self, check: &mut ImageCheck) {
        while let Took::Answer((name, why, is_broken, read_as)) = check.found_rx.next() {
            match is_broken {
                true => {
                    self.broken_files.insert(name.clone());
                    check.broken.push((name, why, read_as));
                }
                false => check.unchecked.push((name, why)),
            }
        }
    }

    /// Select Them: looks in the background which of the broken files are still as the check
    /// read them, and `select_still_broken` selects those. A file that another program wrote
    /// anew since, also in place under the same inode (`cp -f`), may be a good photo now, so it
    /// is neither selected nor marked broken any more.
    fn start_selecting_broken(&mut self, check: &mut ImageCheck) {
        let dir = self.folder.dir().to_path_buf();
        let read: Vec<(OsString, Option<FileFingerprint>)> = (check.broken.iter())
            .map(|(name, _, read_as)| (name.clone(), *read_as))
            .collect();
        let (tx, rx) = answer_channel(&self.busy);
        let ctx = self.ctx.clone();
        // Its busy pointer: the answer's, until the window has selected the files.
        std::thread::spawn(move || {
            let is_as_read = |name: &OsString, read_as: Option<FileFingerprint>| {
                let now = fingerprint_of_file_read(&dir.join(name));
                matches!((read_as, now), (Some(a), Some(b)) if same_fingerprint(a, b))
            };
            let unchanged = (read.into_iter())
                .filter(|(name, read_as)| is_as_read(name, *read_as))
                .map(|(name, _)| name)
                .collect();
            let _ = tx.send(unchanged);
            ctx.request_repaint();
        });
        check.still_broken = Receiving::of(rx);
    }

    /// Selects the broken files that are still as the check read them, once the look of
    /// `start_selecting_broken` has answered, and forgets the mark of the others.
    fn select_still_broken(&mut self, ctx: &egui::Context, check: &mut ImageCheck) {
        let Took::Answer((unchanged, _working)) = check.still_broken.take() else {
            return;
        };
        let unchanged: std::collections::HashSet<OsString> = unchanged.into_iter().collect();
        check.broken.retain(|(name, ..)| unchanged.contains(name));
        self.broken_files.retain(|name| unchanged.contains(name));
        let broken = (self.list.files().iter().enumerate())
            .filter(|(_, f)| self.broken_files.contains(*f))
            .map(|(i, _)| i)
            .collect();
        self.select_for_action(ctx, broken);
    }
}

/// Says what the check found: the files `broken` and those that could not be checked, with
/// `working` threads still at work after `done` of `total` files. A check that stopped early
/// says so, and never "None broken.", as Find Duplicates gives no result when it was stopped.
fn summary(
    [broken, unchecked]: [usize; 2],
    working: usize,
    done: usize,
    total: usize,
) -> ShownText {
    let stopped = working == 0 && done < total;
    let mut lines = Vec::new();
    if stopped {
        lines.push(tf!(
            "Stopped: {done} of {total}",
            done = thousands(done),
            total = thousands(total)
        ));
    }
    match (broken, working) {
        (0, 0) if !stopped => lines.push(tr("None broken.").to_owned()),
        (0, 0) => {}
        (n, _) => lines.push(tf!("{n} broken:", n = thousands(n))),
    }
    if unchecked > 0 {
        let n = thousands(unchecked);
        lines.push(tf!("{n} could not be checked:", n));
    }
    ShownText::join(&lines, &ShownText::untranslated("\n"))
}

#[cfg(test)]
mod tests {
    use crate::support::threads::lock_even_if_poisoned;

    /// A file that could not be read, here because the look at it failed as on a share that
    /// dropped for a moment, is listed as not checked, and neither marked broken nor selected
    /// by Select Them. A file that is no picture is broken. Before, both were broken, and a
    /// user who trusted the mark could delete good photos.
    #[test]
    fn a_file_that_could_not_be_read_is_not_called_broken() {
        let dir = std::env::temp_dir().join(format!("glimt-test-unchecked-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbImage::new(4, 4)
            .save(dir.join("away.png"))
            .unwrap();
        std::fs::write(dir.join("text.png"), b"not a picture").unwrap();
        let (mut app, ctx) = crate::support::test_support::app_on(&dir);
        let names: Vec<_> = app.list.files().to_vec();
        let away = names.iter().position(|n| n == "away.png").unwrap();
        let unread = app.path(away);
        let looks_fail = &crate::files::file_io::LOOKS_FAIL;
        lock_even_if_poisoned(looks_fail).push(unread.clone());
        let mut check = app.start_check(&ctx);
        let start = std::time::Instant::now();
        while check.working.load(super::Ordering::Relaxed) > 0 {
            assert!(start.elapsed() < std::time::Duration::from_secs(10));
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        lock_even_if_poisoned(looks_fail).retain(|p| *p != unread);
        app.take_found(&mut check);
        let unchecked: Vec<_> = check.unchecked.iter().map(|(name, _)| name).collect();
        assert_eq!(unchecked, ["away.png"]);
        let broken: Vec<_> = check.broken.iter().map(|(name, ..)| name).collect();
        assert_eq!(broken, ["text.png"]);
        assert_eq!(app.broken_files.len(), 1);
        assert!(app.broken_files.contains(std::ffi::OsStr::new("text.png")));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Select Them selects only the broken files that are still as the check read them. One
    /// that another program wrote anew in place since (`cp -f`, the same inode) is neither
    /// selected nor marked broken. Before, the marks were kept by name, and Select Them selected
    /// the good photo, ready to be deleted (#202).
    /// Risk: Select Them picks a good photo that took a broken one's name, ready to be deleted.
    #[test]
    fn select_them_leaves_out_a_file_written_anew_since_the_check() {
        let name = format!("glimt-test-check-rewritten-{}", std::process::id());
        let dir = std::env::temp_dir().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a.png", "b.png"] {
            std::fs::write(dir.join(name), b"not a picture").unwrap();
        }
        let (mut app, ctx) = crate::support::test_support::app_on(&dir);
        let mut check = app.start_check(&ctx);
        let start = std::time::Instant::now();
        while check.working.load(super::Ordering::Relaxed) > 0 {
            assert!(start.elapsed() < std::time::Duration::from_secs(10));
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        app.take_found(&mut check);
        assert_eq!(app.broken_files.len(), 2);
        // b.png is written anew in place, as a good picture.
        let mut good = std::io::Cursor::new(Vec::new());
        image::RgbImage::new(2, 2)
            .write_to(&mut good, image::ImageFormat::Png)
            .unwrap();
        std::fs::write(dir.join("b.png"), good.into_inner()).unwrap();
        app.start_selecting_broken(&mut check);
        let start = std::time::Instant::now();
        while app.list.selected().is_empty() {
            assert!(start.elapsed() < std::time::Duration::from_secs(10));
            std::thread::sleep(std::time::Duration::from_millis(20));
            app.select_still_broken(&ctx, &mut check);
        }
        let a = app.list.files().iter().position(|n| n == "a.png").unwrap();
        assert_eq!(app.list.selected(), &[a].into());
        assert!(!app.broken_files.contains(std::ffi::OsStr::new("b.png")));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A check stopped after 300 of 5,000 files says it stopped, not "None broken.", which it
    /// once said although most files were never checked.
    #[test]
    fn a_stopped_check_says_it_stopped() {
        assert_eq!(super::summary([0, 0], 0, 5_000, 5_000), "None broken.");
        assert_eq!(
            super::summary([0, 0], 0, 300, 5_000),
            "Stopped: 300 of 5,000"
        );
        assert_eq!(
            super::summary([2, 0], 0, 300, 5_000),
            "Stopped: 300 of 5,000\n2 broken:"
        );
        assert_eq!(super::summary([0, 0], 3, 300, 5_000), "0 broken:");
        // Files that could not be checked are counted apart from the broken ones.
        assert_eq!(
            super::summary([0, 2], 0, 5_000, 5_000),
            "None broken.\n2 could not be checked:"
        );
        assert_eq!(
            super::summary([0, 2], 0, 300, 5_000),
            "Stopped: 300 of 5,000\n2 could not be checked:"
        );
    }
}
