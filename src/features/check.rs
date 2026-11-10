//! File → Check Images: every image of the list decoded in the background, the broken ones listed.

use crate::support::format::{count, thousands};
use crate::support::lang::tr;
use crate::support::threads::{Receiving, Took, Working, background_threads};
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
    found_rx: Receiving<(OsString, String)>,
    /// The broken ones found: (name in the list, why).
    broken: Vec<(OsString, String)>,
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
            while let Took::Answer((name, why)) = c.found_rx.next() {
                self.broken_files.insert(name.clone());
                c.broken.push((name, why));
            }
        }
        let (mut open, mut start, mut show, mut select) = (true, false, None, false);
        egui::Window::new(tr("Check Images"))
            .open(&mut open)
            .default_size([560.0, 400.0])
            .show(ctx, |ui| {
                let Some(c) = &check else {
                    ui.label(tf!(
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
                ui.label(match (c.broken.len(), working) {
                    (0, 0) => tr("None broken.").to_owned(),
                    (n, _) => tf!("{n} broken:", n = thousands(n)),
                });
                if !c.broken.is_empty() && crate::ui::menus::button(ui, tr("Select Them")).clicked()
                {
                    select = true;
                }
                let row = ui.text_style_height(&egui::TextStyle::Body) + 4.0;
                egui::ScrollArea::vertical().auto_shrink(false).show_rows(
                    ui,
                    row,
                    c.broken.len(),
                    |ui, rows| {
                        for (name, why) in &c.broken[rows] {
                            let text = format!("{}: {why}", name.to_string_lossy());
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
        if select {
            let broken = (self.list.files().iter().enumerate())
                .filter(|(_, f)| self.broken_files.contains(*f))
                .map(|(i, _)| i)
                .collect();
            self.select_for_action(ctx, broken);
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
                    if let Err(why) = decode::check_decodes(path) {
                        let _ = tx.send((name.clone(), why));
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
}
