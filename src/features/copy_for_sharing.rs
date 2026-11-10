//! File → Copy for Sharing…: copies of the selected images in a folder the user chooses, without
//! the metadata the user leaves out (`images::shared_copy`). The dialog lists the tags the images
//! have, in groups, and starts with what was left out last time. The originals are never
//! changed, and a name already in the folder is never written over.

use crate::files::config::sharing_path;
use crate::files::file_io::{create_file, edit_shared, save_failed, write_later};
use crate::files::file_ops::OperationReport;
use crate::images::shared_copy::{Group, LeftOut, copy, tags_of};
use crate::support::format::count;
use crate::support::lang::tr;
use crate::support::threads::{Receiving, Took, Working, answer_channel};
use crate::tf;
use crate::ui::actions::Action;
use crate::ui::dialogs::Pick;
use crate::ui::menus::{button, button_enabled, checkbox, small_button};
use eframe::egui;
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The Copy for Sharing dialog while it is open, for the images `paths`.
pub(crate) struct SharingDialog {
    paths: Vec<PathBuf>,
    left_out: LeftOut,
    /// The tags found in the images, sorted, once every image has been read. Copy… waits for
    /// them, because the copies keep only tags the dialog showed.
    found: Option<Vec<String>>,
    /// The choice made last time and the tags of the images, read in the background.
    reading: Receiving<((LeftOut, Vec<String>), Working)>,
    /// How many images the background has read. The background stops when it holds the only
    /// count left, as the dialog that held the other is closed.
    read_count: Arc<AtomicUsize>,
    /// Whether each group's tags are shown, in the order of `Group::ALL`.
    is_unfolded: [bool; 8],
}

impl SharingDialog {
    /// Takes what the background read, when it is done.
    fn receive_tags(&mut self) -> Took<()> {
        match self.reading.take() {
            Took::Answer(((left_out, found), _pointer)) => {
                (self.left_out, self.found) = (left_out, Some(found));
                Took::Answer(())
            }
            Took::Gone => Took::Gone,
            Took::Nothing => Took::Nothing,
        }
    }

    /// Draws the dialog's choices and buttons. Returns whether Copy… was pressed.
    fn draw(&mut self, ui: &mut egui::Ui) -> bool {
        let Some(found) = self.found.clone() else {
            let (done, total) = (self.read_count.load(Ordering::Relaxed), self.paths.len());
            ui.label(tf!("Reading the tags… {done} of {total}", done, total));
            ui.horizontal(|ui| {
                let _ = button_enabled(ui, false, tr("Copy…"));
                if button(ui, tr("Cancel (Esc)")).clicked() {
                    ui.close();
                }
            });
            return false;
        };
        if button(ui, tr("Remove All")).clicked() {
            self.left_out.remove_all();
        }
        ui.weak(tr("Ticked tags are left out of the copies."));
        ui.weak(tr(
            "A block that can hold tags of several groups, such as the extra images or the maker \
             note, is left out when any tag of those groups is.",
        ));
        ui.weak(tr(
            "A tag that Glimt does not know is listed under More tags, and is left out when any \
             group is ticked, unless it is unticked there.",
        ));
        egui::ScrollArea::vertical()
            .max_height(400.0)
            .show(ui, |ui| {
                for (i, group) in Group::ALL.into_iter().enumerate() {
                    self.draw_group(ui, i, group, &found);
                }
            });
        ui.weak(tr(
            "Always kept: the orientation, so the image is not shown on its side, the colour \
             profile, so its colours do not change, and an HDR photo's gain map, so an HDR \
             screen still brightens it. An iPhone's gain map also keeps two brightness values \
             of the maker note.",
        ));
        let mut go = false;
        ui.horizontal(|ui| {
            go = button(ui, tr("Copy…")).clicked();
            if button(ui, tr("Cancel (Esc)")).clicked() {
                ui.close();
            }
        });
        go
    }

    /// Draws the group `group`, number `i` of `Group::ALL`: its checkbox, which leaves out the
    /// whole group, and Show or Hide for its tags of `found`, each with a checkbox of its own.
    fn draw_group(&mut self, ui: &mut egui::Ui, i: usize, group: Group, found: &[String]) {
        let tags: Vec<&String> = (found.iter()).filter(|t| group.lists(t)).collect();
        ui.horizontal(|ui| {
            let mut is_left_out = self.left_out.is_group_left_out(group);
            if checkbox(ui, &mut is_left_out, group.name()).changed() {
                self.left_out.set_group(group, is_left_out);
            }
            let fold = if self.is_unfolded[i] {
                tr("Hide")
            } else {
                tr("Show")
            };
            if !tags.is_empty() && small_button(ui, fold).clicked() {
                self.is_unfolded[i] = !self.is_unfolded[i];
            }
        });
        if !self.is_unfolded[i] || tags.is_empty() {
            return;
        }
        ui.indent(group.name(), |ui| {
            for tag in tags {
                let mut is_left_out = self.left_out.is_left_out(tag);
                if checkbox(ui, &mut is_left_out, tag).changed() {
                    self.left_out.set_tag(tag, is_left_out);
                }
            }
        });
    }
}

/// What a Copy for Sharing job did, counted file by file, for its notice at the end.
#[derive(Default)]
struct SharingOutcome {
    copied: usize,
    /// Files that are not JPEG, PNG or WebP, which are not copied.
    other_formats: usize,
    /// Files whose name the folder already has, which are not copied.
    names_taken: usize,
    errors: Vec<String>,
}

impl SharingOutcome {
    /// Copies `path` into `folder` with the tags `kept`, and counts what came of it. A name
    /// the folder has is never written over, also when the folder is the image's own.
    fn copy_into(&mut self, path: &Path, folder: &Path, kept: &HashSet<String>) {
        let target = folder.join(path.file_name().unwrap_or_default());
        if target.symlink_metadata().is_ok() {
            self.names_taken += 1;
            return;
        }
        let made = crate::images::decode::catch_panic_as_error(|| copy(path, kept));
        match made {
            Ok(None) => self.other_formats += 1,
            Ok(Some(bytes)) => match create_file(&target, bytes) {
                Ok(()) => self.copied += 1,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => self.names_taken += 1,
                Err(e) => self.errors.push(format!("{}: {e}", target.display())),
            },
            Err(e) => self.errors.push(format!("{}: {e}", path.display())),
        }
    }

    /// The notice at the end: the first error, or how many were copied into `folder` (or how
    /// far a stopped job got, `stopped`), and how many were not copied and why.
    fn text(&self, folder: &Path, stopped: Option<String>) -> String {
        let mut text = match (self.errors.first(), stopped) {
            (Some(first), _) => tf!(
                "{verb} failed ({count}): {first}",
                verb = tr("Copy"),
                count = count(self.errors.len(), "error"),
                first
            ),
            (None, Some(stopped)) => stopped,
            (None, None) => tf!(
                "{files} copied to {folder}",
                files = count(self.copied, "file"),
                folder = folder.display()
            ),
        };
        if self.other_formats > 0 {
            let files = count(self.other_formats, "file");
            text += &tf!("; {files} not copied: only JPEG, PNG and WebP are", files);
        }
        if self.names_taken > 0 {
            let files = count(self.names_taken, "file");
            text += &tf!(
                "; {files} not copied: the folder has files of those names",
                files
            );
        }
        text
    }
}

impl crate::App {
    /// File → Copy for Sharing…: opens its dialog for the targets, and reads the choice made
    /// last time and the tags of the images in the background.
    pub(crate) fn start_copy_for_sharing(&mut self, ctx: &egui::Context) {
        let Some(targets) = self.targets() else {
            return;
        };
        let paths: Vec<PathBuf> = targets.iter().map(|&i| self.path(i)).collect();
        let (tx, rx) = answer_channel(&self.busy);
        let read_count = Arc::new(AtomicUsize::new(0));
        let (counter, images, ctx) = (read_count.clone(), paths.clone(), ctx.clone());
        let pointer = Working::new(&self.busy);
        std::thread::spawn(move || {
            let _pointer = pointer;
            let last = (sharing_path())
                .and_then(|file| crate::images::decode::read_text(file).ok())
                .map_or_else(LeftOut::default, |text| LeftOut::parse(&text));
            let mut found = BTreeSet::new();
            for path in &images {
                // The dialog is closed, so nobody waits for the rest.
                if Arc::strong_count(&counter) == 1 {
                    return;
                }
                // A file that cannot be read has no tags to list, and its copy tells why.
                let tags = crate::support::threads::catch_panic(|| tags_of(path));
                found.extend(tags.and_then(Result::ok).flatten().unwrap_or_default());
                counter.fetch_add(1, Ordering::Relaxed);
                ctx.request_repaint();
            }
            let _ = tx.send((last, found.into_iter().collect()));
            ctx.request_repaint();
        });
        self.dialogs.show_sharing(SharingDialog {
            paths,
            left_out: LeftOut::default(),
            found: None,
            reading: Receiving::of(rx),
            read_count,
            is_unfolded: [false; 8],
        });
    }

    /// Draws the Copy for Sharing dialog while it is open. Copy… keeps the choice for next time
    /// and asks for the folder.
    pub(crate) fn sharing_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.dialogs.take_sharing() else {
            return;
        };
        if let Took::Gone = dialog.receive_tags() {
            return self.tell_background_stopped(tr("Reading the tags"));
        }
        let mut go = false;
        let modal = egui::Modal::new(egui::Id::unique("copy for sharing")).show(ctx, |ui| {
            ui.strong(Action::CopyForSharing.label());
            ui.label(count(dialog.paths.len(), "image"));
            go = dialog.draw(ui);
        });
        if go {
            let found = dialog.found.unwrap_or_default();
            let kept = dialog.left_out.kept(&found);
            save_choice(dialog.left_out.text());
            self.pick(ctx, true, Pick::CopyForSharing(dialog.paths, kept));
        } else if !modal.should_close() {
            self.dialogs.show_sharing(dialog);
        }
    }

    /// Copies `paths` into `folder` in the background, each keeping the tags `kept` and what
    /// every copy keeps (`shared_copy::copy`), as a job with progress and Stop. The originals
    /// are held back while it runs, as a copy holds them (`transfer`): a Rename Many meanwhile
    /// could hand one photo another photo's sidecar.
    pub(crate) fn copy_for_sharing(
        &mut self,
        paths: Vec<PathBuf>,
        folder: PathBuf,
        kept: HashSet<String>,
    ) {
        // The folder by its real path, looked up in the background as for a copy (`transfer`).
        self.with_real_folder(folder, paths.clone(), move |app, ctx, folder| {
            app.copy_for_sharing_into_real_folder(ctx, paths, folder, kept)
        });
    }

    /// Copies `paths` into `folder`, by its real path, as `copy_for_sharing` says.
    fn copy_for_sharing_into_real_folder(
        &mut self,
        ctx: &egui::Context,
        paths: Vec<PathBuf>,
        folder: PathBuf,
        kept: HashSet<String>,
    ) {
        let Some(job) = self.start_job(tr("Copying"), paths.len(), paths.clone()) else {
            return;
        };
        // Copies made into the folder shown, or one below it in the list with subfolders, join
        // the list.
        let listed = folder == self.folder.dir()
            || (self.folder.recursive() && folder.starts_with(self.folder.dir()));
        let (tx, report_tx) = (
            self.notice_sender.clone(),
            self.file_operations.report_sender(),
        );
        let (ctx, pointer) = (ctx.clone(), Working::new(&self.busy));
        std::thread::spawn(move || {
            let mut outcome = SharingOutcome::default();
            for path in &paths {
                if job.is_stopped() {
                    break;
                }
                outcome.copy_into(path, &folder, &kept);
                job.count_done(1);
            }
            let stopped = job.is_stopped().then(|| job.stopped_text());
            let _ = tx.send(outcome.text(&folder, stopped));
            if listed && outcome.copied > 0 {
                let _ = report_tx.send(OperationReport {
                    read_folder_again: true,
                    ..Default::default()
                });
            }
            drop(job);
            drop(pointer);
            ctx.request_repaint();
        });
    }
}

/// Keeps `text`, what Copy for Sharing left out, for the next time and the other windows.
fn save_choice(text: String) {
    let Some(file) = sharing_path() else {
        return;
    };
    write_later(move || {
        if let Err(e) = edit_shared(&file, |_| (text.into_bytes(), ())) {
            save_failed(tf!("Could not save {name}: {e}", name = file.display(), e));
        }
    });
}

#[cfg(test)]
mod tests {
    /// The notice at the end tells how many were copied where, and why the others were not.
    #[test]
    fn the_notice_says_what_was_not_copied() {
        let outcome = super::SharingOutcome {
            copied: 2,
            other_formats: 1,
            names_taken: 3,
            errors: Vec::new(),
        };
        let text = outcome.text(std::path::Path::new("/out"), None);
        assert!(text.starts_with("2 files copied to /out"), "{text}");
        assert!(text.contains("1 file not copied") && text.contains("3 files not copied"));
    }
}
