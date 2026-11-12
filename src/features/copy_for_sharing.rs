//! File → Copy for Sharing…: copies of the selected images in a folder the user chooses, without
//! the metadata the user leaves out (`images::shared_copy`). The dialog lists the tags the images
//! have, in groups, and starts with what was left out last time. The originals are never
//! changed, and a name already in the folder is never written over. A copy whose name is taken,
//! such as two photos of one name from two folders listed with subfolders, is left out or, when
//! the user ticks it, gets a new name, "NAME-2.EXT".

use crate::files::config::sharing_path;
use crate::files::file_io::{PickedFiles, create_file, edit_shared, save_failed, write_later};
use crate::files::file_ops::OperationReport;
use crate::images::shared_copy::{Group, LeftOut, copy, tags_of};
use crate::support::format::count;
use crate::support::lang::tr;
use crate::support::threads::{Receiving, Told, Took, Working, answer_channel};
use crate::tf;
use crate::ui::actions::Action;
use crate::ui::dialogs::Pick;
use crate::ui::menus::{button, button_enabled, checkbox, small_button};
use eframe::egui;
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The Copy for Sharing dialog while it is open, for the images `picked`, whose fingerprints are
/// taken while it is open.
pub(crate) struct SharingDialog {
    picked: PickedFiles,
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
    /// How many images have the name of another image picked, from another folder.
    clashes: usize,
    /// Whether a copy whose name is taken gets a new name instead of being left out.
    renames: bool,
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
            let (done, total) = (
                self.read_count.load(Ordering::Relaxed),
                self.picked.paths().len(),
            );
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
        if self.clashes > 0 {
            let images = count(self.clashes, "image");
            ui.colored_label(
                ui.visuals().warn_fg_color,
                tf!(
                    "Same name as another image picked, from another folder: {images}",
                    images
                ),
            );
        }
        checkbox(
            ui,
            &mut self.renames,
            tr("Give a copy whose name is taken a new name (NAME-2.EXT)"),
        );
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
    /// Files whose name was taken, copied under a new name.
    renamed: usize,
    errors: Vec<String>,
}

impl SharingOutcome {
    /// Copies `path` into `folder` with the tags `kept`, and counts what came of it. A name
    /// the folder has is never written over, also when the folder is the image's own. The copy
    /// is left out then, or with `renames` it gets the first free name of "NAME-2.EXT",
    /// "NAME-3.EXT" ….
    fn copy_into(&mut self, path: &Path, folder: &Path, kept: &HashSet<String>, renames: bool) {
        let name = path.file_name().unwrap_or_default();
        // A name whose look failed counts as taken too.
        if !renames && !crate::files::file_io::is_surely_gone(&folder.join(name)) {
            self.names_taken += 1;
            return;
        }
        let made = crate::images::decode::catch_panic_as_error(|| copy(path, kept));
        let bytes = match made {
            Ok(None) => return self.other_formats += 1,
            Ok(Some(bytes)) => bytes,
            Err(e) => return self.errors.push(format!("{}: {e}", path.display())),
        };
        // A bound, so a folder whose names cannot be looked at does not keep it trying.
        let tries = if renames { 1000 } else { 1 };
        for n in 1..=tries {
            let target = folder.join(match n {
                1 => name.to_owned(),
                _ => numbered(path, n),
            });
            if n > 1 && !crate::files::file_io::is_surely_gone(&target) {
                continue;
            }
            match create_file(&target, &bytes) {
                Ok(()) => {
                    self.copied += 1;
                    self.renamed += usize::from(n > 1);
                    return;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return self.errors.push(format!("{}: {e}", target.display())),
            }
        }
        self.names_taken += 1;
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
        if self.renamed > 0 {
            let files = count(self.renamed, "file");
            text += &tf!("; {files} given new names", files);
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
        let clashes = name_clashes(&paths);
        let (tx, rx) = answer_channel(&self.busy);
        let read_count = Arc::new(AtomicUsize::new(0));
        let (counter, images, ctx) = (read_count.clone(), paths.clone(), ctx.clone());
        let pointer = Working::new(&self.busy);
        std::thread::spawn(move || {
            let _pointer = pointer;
            // Parsed by `LeftOut::parse`, which survives_broken_sharing_file feeds broken input.
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
            picked: PickedFiles::listed(targets.iter().map(|&i| self.listed_file(i)).collect()),
            left_out: LeftOut::default(),
            found: None,
            reading: Receiving::of(rx),
            read_count,
            is_unfolded: [false; 8],
            clashes,
            renames: clashes > 0,
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
            ui.label(count(dialog.picked.paths().len(), "image"));
            go = dialog.draw(ui);
        });
        if go {
            let found = dialog.found.unwrap_or_default();
            let kept = dialog.left_out.kept(&found);
            save_choice(dialog.left_out.text());
            let picked = dialog.picked;
            self.pick(
                ctx,
                true,
                Pick::CopyForSharing(picked, kept, dialog.renames),
            );
        } else if !modal.should_close() {
            self.dialogs.show_sharing(dialog);
        }
    }

    /// Copies the images `picked` into `folder` in the background, each keeping the tags `kept`
    /// and what every copy keeps (`shared_copy::copy`), and a new name when its own is taken and
    /// `renames`, as a job with progress and Stop. The
    /// originals are held back while it runs, as a copy holds them (`transfer`): a Rename Many
    /// meanwhile could hand one photo another photo's sidecar. An image that is no longer the
    /// one picked is not copied, and is told.
    pub(crate) fn copy_for_sharing(
        &mut self,
        picked: PickedFiles,
        folder: PathBuf,
        kept: HashSet<String>,
        renames: bool,
    ) {
        // The folder by its real path, looked up in the background as for a copy (`transfer`).
        let paths = picked.paths().to_vec();
        self.with_real_folder(folder, (paths, false), move |app, ctx, folder, _| {
            app.copy_for_sharing_into_real_folder(ctx, picked, folder, (kept, renames))
        });
    }

    /// Copies the images `picked` into `folder`, by its real path, as `copy_for_sharing` says.
    fn copy_for_sharing_into_real_folder(
        &mut self,
        ctx: &egui::Context,
        picked: PickedFiles,
        folder: PathBuf,
        (kept, renames): (HashSet<String>, bool),
    ) {
        let paths = picked.paths().to_vec();
        let Some(job) = self.start_job(tr("Copying"), paths.len(), paths.clone()) else {
            return;
        };
        // Copies made into the folder shown, or one below it in the list with subfolders, join
        // the list.
        let listed = folder == self.folder.dir()
            || (self.folder.recursive() && folder.starts_with(self.folder.dir()));
        let report_tx = self.file_operations.report_sender();
        self.run_in_background(ctx, Vec::new(), move || {
            let mut outcome = SharingOutcome::default();
            let (paths, picked) = picked.wait_for_fingerprints();
            for path in &paths {
                if job.is_stopped() {
                    break;
                }
                // Another window or program may have given the name to another photo meanwhile.
                match picked.still_the_one_picked(path) {
                    Ok(()) => outcome.copy_into(path, &folder, &kept, renames),
                    Err(e) => outcome.errors.push(format!("{}: {e}", path.display())),
                }
                job.count_done(1);
            }
            let stopped = job.is_stopped().then(|| job.stopped_text());
            if listed && outcome.copied > 0 {
                let _ = report_tx.send(OperationReport {
                    read_folder_again: true,
                    ..Default::default()
                });
            }
            drop(job);
            // A copy not made is a failed write, which keeps a close that waited for it open,
            // so the user does not take the copies for made.
            let text = outcome.text(&folder, stopped);
            Some(match outcome.errors.is_empty() {
                true => Told::Notice(text),
                false => Told::FailedWrite(text),
            })
        });
    }
}

/// Returns the name "NAME-n.EXT" of `path`, or "NAME-n" when it has no extension.
fn numbered(path: &Path, n: usize) -> std::ffi::OsString {
    let mut name = crate::images::save::stem_with_suffix(path, &format!("-{n}"));
    if let Some(ext) = path.extension() {
        name.push(".");
        name.push(ext);
    }
    name
}

/// Returns how many of `paths` have the name of one before them, as images picked from several
/// folders, listed with subfolders, can have.
fn name_clashes(paths: &[PathBuf]) -> usize {
    let mut seen = HashSet::new();
    paths.iter().filter(|p| !seen.insert(p.file_name())).count()
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
    /// An image whose name another photo took while the dialog and the chooser were open is
    /// not copied in its place, and the notice tells why.
    #[test]
    fn copies_only_the_images_picked() {
        use crate::support::test_support::{app_on, contract_folder, run_background_until};
        let dir = contract_folder("sharing-swapped");
        let a = dir.join("a.png");
        let picked = (a.clone(), crate::files::file_io::fingerprint(&a).unwrap());
        let picked = crate::files::file_io::PickedFiles::recorded(vec![picked]);
        std::fs::remove_file(&a).unwrap();
        image::RgbImage::new(3, 3).save(&a).unwrap();
        let (mut app, ctx) = app_on(&dir);
        app.copy_for_sharing(picked, dir.join("sub"), Default::default(), false);
        assert!(run_background_until(&mut app, &ctx, |app| !app.working()
            && app.notice.text().is_some()));
        assert!(!dir.join("sub/a.png").exists());
        let told = app.notice.text().unwrap_or_default().to_owned();
        assert!(told.contains("changed"), "{told}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The notice at the end tells how many were copied where, and why the others were not.
    #[test]
    fn the_notice_says_what_was_not_copied() {
        let outcome = super::SharingOutcome {
            copied: 2,
            other_formats: 1,
            names_taken: 3,
            renamed: 0,
            errors: Vec::new(),
        };
        let text = outcome.text(std::path::Path::new("/out"), None);
        assert!(text.starts_with("2 files copied to /out"), "{text}");
        assert!(text.contains("1 file not copied") && text.contains("3 files not copied"));
    }

    /// Copies that could not be made are told as a failed write, which keeps a close that
    /// waited for them open. Before, they were a plain notice, and the window closed over it.
    #[test]
    fn copies_not_made_are_a_failed_write() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-sharing-fails-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("a.png");
        image::RgbImage::new(4, 4).save(&png).unwrap();
        let (mut app, ctx) = crate::support::test_support::app_on(&dir);
        let folder = dir.join("no such folder");
        let kept = Default::default();
        let picked = crate::files::file_io::PickedFiles::new(vec![png]);
        app.copy_for_sharing_into_real_folder(&ctx, picked, folder, (kept, false));
        let told = app.notices.recv_timeout(std::time::Duration::from_secs(5));
        assert!(matches!(told, Ok(super::Told::FailedWrite(_))), "{told:?}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Two photos of one name from two folders, as listed with subfolders: the second is left
    /// out and told, or with new names ticked, copied as "NAME-2.EXT".
    #[test]
    fn photos_of_one_name_get_new_names() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-sharing-clash-{}", std::process::id()));
        let (a, b, out) = (dir.join("a/x.png"), dir.join("b/x.png"), dir.join("out"));
        for d in [&dir.join("a"), &dir.join("b"), &out] {
            std::fs::create_dir_all(d).unwrap();
        }
        image::RgbImage::new(4, 4).save(&a).unwrap();
        image::RgbImage::new(5, 5).save(&b).unwrap();
        assert_eq!(super::name_clashes(&[a.clone(), b.clone()]), 1);
        let kept = Default::default();
        let mut skipped = super::SharingOutcome::default();
        skipped.copy_into(&a, &out, &kept, false);
        skipped.copy_into(&b, &out, &kept, false);
        assert_eq!((skipped.copied, skipped.names_taken), (1, 1));
        std::fs::remove_file(out.join("x.png")).unwrap();
        let mut renamed = super::SharingOutcome::default();
        renamed.copy_into(&a, &out, &kept, true);
        renamed.copy_into(&b, &out, &kept, true);
        assert_eq!(
            (renamed.copied, renamed.renamed, renamed.names_taken),
            (2, 1, 0)
        );
        assert_eq!(
            image::image_dimensions(out.join("x-2.png")).unwrap(),
            (5, 5)
        );
        assert!(renamed.text(&out, None).contains("1 file given new names"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
