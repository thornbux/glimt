//! File → Copy for Sharing…: copies of the selected images in a folder the user chooses, without
//! the metadata the user leaves out (`images::shared_copy`). The dialog lists the tags the images
//! have, in groups, and starts with what was left out last time. The originals are never
//! changed, and a name already in the folder is never written over. A copy keeps the original's
//! file name, unless the user leaves the name out: then the copies are named "image-1.EXT",
//! "image-2.EXT" …. A copy whose own name is taken, such as two photos of one name from two
//! folders listed with subfolders, is left out or, when the user ticks it, gets a new name,
//! "NAME-2.EXT".

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

/// How many looks at a copy's names may fail before the copy is given up, and the last look's
/// error told.
const MOST_FAILED_LOOKS: usize = 1000;

/// How the copies are named.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Naming {
    /// The original's file name. A copy whose name is taken is left out.
    Own,
    /// The original's file name, or "NAME-2.EXT", "NAME-3.EXT" … when it is taken.
    OwnOrNumbered,
    /// "image-1.EXT", "image-2.EXT" …, without the original's name, which can hold the date.
    Neutral,
}

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
    /// Returns how the copies are named, as the dialog's boxes say.
    fn naming(&self) -> Naming {
        match (self.left_out.is_file_name_left_out(), self.renames) {
            (true, _) => Naming::Neutral,
            (false, true) => Naming::OwnOrNumbered,
            (false, false) => Naming::Own,
        }
    }

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
                    // The name can hold the date and the photographer's name, so its box is
                    // under theirs.
                    if group == Group::Photographer {
                        let mut is_left_out = self.left_out.is_file_name_left_out();
                        if checkbox(ui, &mut is_left_out, tr("File name")).changed() {
                            self.left_out.set_file_name(is_left_out);
                        }
                    }
                }
            });
        let always_kept = tr(
            "Always kept: the orientation, so the image is not shown on its side, the colour \
             profile, so its colours do not change, and an HDR photo's gain map, so an HDR \
             screen still brightens it. An iPhone's gain map also keeps two brightness values \
             of the maker note.",
        );
        let name = tr(
            "A copy keeps the file's name, which can hold the date, unless File name is ticked: \
             then the copies are named image-1, image-2 and so on.",
        );
        ui.weak(format!("{always_kept} {name}"));
        // Neutral names are never taken, so the clashes of the files' own names matter only
        // while those are kept.
        if !self.left_out.is_file_name_left_out() {
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
        }
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

/// Looks whether the file at a path is still the one the user picked, and returns the error to
/// tell when it is not (`PickedFingerprints::still_the_one_picked`).
type StillTheOnePicked<'a> = dyn Fn(&Path) -> std::io::Result<()> + 'a;

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
    /// The number of the last neutral name tried (`Naming::Neutral`), so that each copy of a
    /// job looks on from there. It is `None` until the job's first neutral copy, which starts
    /// past the highest "image-N" in the folder (`highest_neutral_number`).
    last_neutral_number: Option<usize>,
}

impl SharingOutcome {
    /// Copies `path` into `folder` with the tags `kept`, named as `naming` says, and counts
    /// what came of it. A name the folder has is never written over, also when the folder is
    /// the image's own. With `Naming::Own` the copy is left out then, and with
    /// `OwnOrNumbered` it gets the first free name of "NAME-2.EXT", "NAME-3.EXT" …. Every name,
    /// its own too, is free only when no sidecar of that name is left there either, which would
    /// give the copy a stranger's keywords and comment. With `Naming::Own`, such a sidecar and a
    /// look that failed are told as errors that say so, not as a file of that name.
    ///
    /// `still_the_one_picked` looks whether the file at `path` is still the one the user picked.
    /// It is asked after the last read of the file, its sidecar's too, and before the copy is
    /// written. Asked before the reads, a photo that took the name in between gave the copy its
    /// EXIF and XMP.
    fn copy_into(
        &mut self,
        (path, still_the_one_picked): (&Path, &StillTheOnePicked),
        folder: &Path,
        kept: &HashSet<String>,
        naming: Naming,
    ) {
        let name = path.file_name().unwrap_or_default();
        if naming == Naming::Own {
            let target = folder.join(name);
            match is_name_free(&target, &crate::metadata::xmp::lock_sidecar(&target)) {
                Ok(true) => {}
                Ok(false) => return self.names_taken += 1,
                Err(e) => return self.errors.push(e.to_string()),
            }
        }
        let made = crate::images::decode::catch_panic_as_error(|| copy(path, kept));
        let bytes = match made {
            Ok(None) => return self.other_formats += 1,
            Ok(Some(bytes)) => bytes,
            Err(e) => return self.errors.push(format!("{}: {e}", path.display())),
        };
        // Another window or program may have given the name to another photo meanwhile.
        if let Err(e) = still_the_one_picked(path) {
            return self.errors.push(format!("{}: {e}", path.display()));
        }
        // Only the looks that fail count against a bound, so a folder whose names cannot be
        // looked at does not keep it trying. Names surely taken do not, as the copies are
        // numbered past every name in the folder. Before, a folder with image-1 to image-1000
        // left the first copy of every later job out.
        let mut failed_looks = 0;
        let last = if naming == Naming::Own { 1 } else { usize::MAX };
        for n in 1..=last {
            let target = folder.join(match naming {
                Naming::Neutral => {
                    let last = (self.last_neutral_number)
                        .get_or_insert_with(|| highest_neutral_number(folder, path));
                    *last += 1;
                    neutral_name(path, *last)
                }
                _ if n == 1 => name.to_owned(),
                _ => numbered(path, n),
            });
            // The sidecar's lock is held from the look until the copy has its name, so that no
            // sidecar is written at that name in between.
            let sidecar_lock = crate::metadata::xmp::lock_sidecar(&target);
            match is_name_free(&target, &sidecar_lock) {
                Ok(true) => {}
                Ok(false) => continue,
                // A sidecar left without its image keeps its name taken.
                Err(e)
                    if e.kind() == std::io::ErrorKind::AlreadyExists && naming != Naming::Own =>
                {
                    continue;
                }
                Err(e) => {
                    failed_looks += 1;
                    if naming == Naming::Own || failed_looks == MOST_FAILED_LOOKS {
                        return self.errors.push(e.to_string());
                    }
                    continue;
                }
            }
            match create_file(&target, &bytes) {
                Ok(()) => {
                    self.copied += 1;
                    self.renamed += usize::from(naming == Naming::OwnOrNumbered && n > 1);
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
            let naming = dialog.naming();
            let found = dialog.found.unwrap_or_default();
            let kept = dialog.left_out.kept(&found);
            save_choice(dialog.left_out.text());
            let picked = dialog.picked;
            self.pick(ctx, true, Pick::CopyForSharing(picked, kept, naming));
        } else if !modal.should_close() {
            self.dialogs.show_sharing(dialog);
        }
    }

    /// Copies the images `picked` into `folder` in the background, each keeping the tags `kept`
    /// and what every copy keeps (`shared_copy::copy`), named as `naming` says, as a job with
    /// progress and Stop. The
    /// originals are held back while it runs, as a copy holds them (`transfer`): a Rename Many
    /// meanwhile could hand one photo another photo's sidecar. An image that is no longer the
    /// one picked is not copied, and is told.
    pub(crate) fn copy_for_sharing(
        &mut self,
        picked: PickedFiles,
        folder: PathBuf,
        kept: HashSet<String>,
        naming: Naming,
    ) {
        // The folder by its real path, looked up in the background as for a copy (`transfer`).
        let paths = picked.paths().to_vec();
        self.with_real_folder(folder, (paths, false), move |app, ctx, folder, _| {
            app.copy_for_sharing_into_real_folder(ctx, picked, folder, (kept, naming))
        });
    }

    /// Copies the images `picked` into `folder`, by its real path, as `copy_for_sharing` says.
    fn copy_for_sharing_into_real_folder(
        &mut self,
        ctx: &egui::Context,
        picked: PickedFiles,
        folder: PathBuf,
        (kept, naming): (HashSet<String>, Naming),
    ) {
        let paths = picked.paths().to_vec();
        let Some(job) = self.start_job(tr("Copying"), paths.len(), paths.clone()) else {
            return;
        };
        let listed = self.folder.takes_files_put_in(&folder);
        let report_tx = self.file_operations.report_sender();
        self.run_in_background(ctx, Vec::new(), move || {
            let mut outcome = SharingOutcome::default();
            let (paths, picked) = picked.wait_for_fingerprints();
            for path in &paths {
                if job.is_stopped() {
                    break;
                }
                let still_the_one_picked = |p: &Path| picked.still_the_one_picked(p);
                outcome.copy_into((path, &still_the_one_picked), &folder, &kept, naming);
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
                false => Told::FailedWrite(text.into()),
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

/// Looks whether a copy may take the name `target`: no file is there, nor a sidecar left
/// without its image (`xmp::check_no_sidecar`). Returns `Ok(false)` when a file is there, and the
/// error to tell when only such a sidecar is there (`AlreadyExists`) or a look failed. The
/// caller holds the sidecar's lock `sidecar_lock` (`xmp::check_no_sidecar`).
fn is_name_free(
    target: &Path,
    sidecar_lock: &crate::metadata::xmp::SidecarLock,
) -> std::io::Result<bool> {
    match crate::files::file_io::check_free(target) {
        Ok(()) => {
            crate::metadata::xmp::check_no_sidecar(target, false, sidecar_lock).map(|()| true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(std::io::Error::new(
            e.kind(),
            tf!("could not look at {name}: {e}", name = target.display(), e),
        )),
    }
}

/// Returns the neutral name "image-n.EXT" for a copy of `path`, or "image-n" when it has no
/// extension. The extension stays, as it says the format and nothing about the photo.
fn neutral_name(path: &Path, n: usize) -> std::ffi::OsString {
    let mut name = std::ffi::OsString::from(format!("image-{n}"));
    if let Some(ext) = path.extension() {
        name.push(".");
        name.push(ext);
    }
    name
}

/// Returns the highest n of the names "image-n…" in `folder`, its copies' and their sidecars',
/// from one reading of the folder, so that a job's neutral names start past them. A look at each
/// taken name took minutes in a folder of 200,000 earlier copies on a share, and Stop waited for
/// it. Returns 0 when there is none. A folder that cannot be read is searched by looks at the
/// names of copies of `path` instead (`highest_neutral_number_by_looks`). A number past
/// `u32::MAX` is left out, so that no planted name can make the count overflow.
fn highest_neutral_number(folder: &Path, path: &Path) -> usize {
    let read = std::fs::read_dir(folder);
    // In tests, a folder whose looks fail cannot be read either, as the tests run as root,
    // who can read a drop box.
    #[cfg(test)]
    let read =
        match (crate::support::threads::lock_even_if_poisoned(&crate::files::file_io::LOOKS_FAIL))
            .contains(&folder.to_path_buf())
        {
            true => Err(std::io::Error::from_raw_os_error(libc::EACCES)),
            false => read,
        };
    let Ok(entries) = read else {
        return highest_neutral_number_by_looks(folder, path);
    };
    let number = |name: &[u8]| {
        let digits = name.strip_prefix(b"image-")?;
        let end = (digits.iter().position(|b| !b.is_ascii_digit())).unwrap_or(digits.len());
        let n: u32 = std::str::from_utf8(digits.get(..end)?).ok()?.parse().ok()?;
        usize::try_from(n).ok()
    };
    (entries.flatten())
        .filter_map(|entry| number(entry.file_name().as_encoded_bytes()))
        .max()
        .unwrap_or(0)
}

/// Returns a number n whose neutral name for a copy of `path` (`neutral_name`) is taken in
/// `folder` while the next one is free, or 0 when image-1 is free. It looks at the names in
/// steps that double and then halve, about two looks for each doubling, so 36 looks find it
/// past 200,000 copies. A folder that can be looked at but not read (a drop box, mode 0733)
/// gives no names to `highest_neutral_number`, and a look at each taken name from image-1 took
/// minutes there.
/// shortcut: a gap among the names (a copy deleted) can end the search below the highest, and
/// the taken names after the gap are then looked at one by one; search past gaps if that is met.
fn highest_neutral_number_by_looks(folder: &Path, path: &Path) -> usize {
    let is_taken =
        |n: usize| crate::files::file_io::is_surely_there(&folder.join(neutral_name(path, n)));
    let most = 1_usize << 31;
    let (mut low, mut high) = (0, 1);
    while high < most && is_taken(high) {
        (low, high) = (high, high * 2);
    }
    while high - low > 1 {
        let middle = low + (high - low) / 2;
        match is_taken(middle) {
            true => low = middle,
            false => high = middle,
        }
    }
    low
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
        app.copy_for_sharing(
            picked,
            dir.join("sub"),
            Default::default(),
            super::Naming::Own,
        );
        assert!(run_background_until(&mut app, &ctx, |app| !app.working()
            && app.notice.text().is_some()));
        assert!(!dir.join("sub/a.png").exists());
        let told = app.notice.text().unwrap_or_default().to_owned();
        assert!(told.contains("changed"), "{told}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The look that the image is still the one picked comes after the last read of it and of
    /// its sidecar, and a refusal leaves no copy. Before, it came before the reads, and a photo
    /// that took the name in between gave the copy its EXIF and XMP.
    #[test]
    fn the_image_is_looked_at_after_it_is_read() {
        use crate::metadata::xmp::SIDECAR_READS;
        let dir = std::env::temp_dir().join(format!(
            "glimt-test-sharing-read-first-{}",
            std::process::id()
        ));
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();
        let a = dir.join("a.png");
        image::RgbImage::new(4, 4).save(&a).unwrap();
        SIDECAR_READS.set(0);
        let reads_before_the_look = std::cell::Cell::new(None);
        let refuse = |_: &std::path::Path| {
            reads_before_the_look.set(Some(SIDECAR_READS.get()));
            Err(crate::files::file_io::changed_meanwhile())
        };
        let mut outcome = super::SharingOutcome::default();
        outcome.copy_into((&a, &refuse), &out, &Default::default(), super::Naming::Own);
        assert_eq!(reads_before_the_look.get(), Some(1));
        assert_eq!((outcome.copied, outcome.errors.len()), (0, 1));
        assert!(!out.join("a.png").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A copy takes its name only under the lock of the sidecar of that name, held from the look
    /// that no sidecar is there. A sidecar written meanwhile, under that lock, keeps the name
    /// taken. Before, the look and the copy were made without the lock, and a sidecar written
    /// in between went to the copy.
    #[test]
    fn a_copy_takes_its_name_under_the_sidecars_lock() {
        let dir = std::env::temp_dir().join(format!(
            "glimt-test-sharing-sidecar-lock-{}",
            std::process::id()
        ));
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();
        let a = dir.join("a.png");
        image::RgbImage::new(4, 4).save(&a).unwrap();
        let target = out.join("a.png");
        let sidecar_lock = crate::metadata::xmp::lock_sidecar(&target);
        let copying = std::thread::spawn({
            let (a, out) = (a.clone(), out.clone());
            move || {
                let mut outcome = super::SharingOutcome::default();
                let kept = Default::default();
                outcome.copy_into((&a, &|_| Ok(())), &out, &kept, super::Naming::Own);
                (outcome.copied, outcome.errors.len())
            }
        });
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(!target.exists(), "copied while the sidecar's lock was held");
        std::fs::write(crate::metadata::xmp::sidecar(&target), "another photo's").unwrap();
        drop(sidecar_lock);
        assert_eq!(copying.join().unwrap(), (0, 1));
        assert!(!target.exists());
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
            last_neutral_number: None,
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
        app.copy_for_sharing_into_real_folder(&ctx, picked, folder, (kept, super::Naming::Own));
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
        skipped.copy_into((&a, &|_| Ok(())), &out, &kept, super::Naming::Own);
        skipped.copy_into((&b, &|_| Ok(())), &out, &kept, super::Naming::Own);
        assert_eq!((skipped.copied, skipped.names_taken), (1, 1));
        std::fs::remove_file(out.join("x.png")).unwrap();
        let mut renamed = super::SharingOutcome::default();
        renamed.copy_into((&a, &|_| Ok(())), &out, &kept, super::Naming::OwnOrNumbered);
        renamed.copy_into((&b, &|_| Ok(())), &out, &kept, super::Naming::OwnOrNumbered);
        assert_eq!(
            (renamed.copied, renamed.renamed, renamed.names_taken),
            (2, 1, 0)
        );
        assert_eq!(
            image::image_dimensions(out.join("x-2.png")).unwrap(),
            (5, 5)
        );
        assert!(renamed.text(&out, None).contains("1 file given new names"));
        // A sidecar left at the next name without its image keeps that name taken, so its
        // keywords never show on the copy.
        std::fs::write(out.join("x-3.png.xmp"), "left").unwrap();
        renamed.copy_into((&b, &|_| Ok(())), &out, &kept, super::Naming::OwnOrNumbered);
        assert!(out.join("x-4.png").exists() && !out.join("x-3.png").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Neutral names are numbered past every name in the folder, also past a thousand copies
    /// made by earlier jobs, from one reading of the folder without a look at each taken name.
    /// Before, each copy gave up after a thousand names taken, so the first copy of every later
    /// job was left out as if its name were taken, and then each job looked at every taken name,
    /// which took minutes on a share.
    #[test]
    fn neutral_names_go_past_a_thousand_names_taken() {
        let dir = std::env::temp_dir().join(format!(
            "glimt-test-sharing-thousand-{}",
            std::process::id()
        ));
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();
        let a = dir.join("a.png");
        image::RgbImage::new(4, 4).save(&a).unwrap();
        for n in 1..=1000 {
            std::fs::write(out.join(format!("image-{n}.png")), b"").unwrap();
        }
        std::fs::write(out.join("image-99999999999.png"), b"").unwrap();
        for job in [1001, 1002] {
            let mut outcome = super::SharingOutcome::default();
            crate::files::file_io::LOOKS_MADE.set(0);
            outcome.copy_into(
                (&a, &|_| Ok(())),
                &out,
                &Default::default(),
                super::Naming::Neutral,
            );
            let looks = crate::files::file_io::LOOKS_MADE.get();
            assert!(looks < 10, "{looks} looks");
            assert_eq!((outcome.copied, outcome.names_taken), (1, 0));
            assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
            let made = out.join(format!("image-{job}.png"));
            assert_eq!(image::image_dimensions(made).unwrap(), (4, 4));
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Neutral names are numbered past a thousand copies made by earlier jobs also in a folder
    /// that can be looked at but not read (a drop box, mode 0733), with a few dozen looks.
    /// Before, the reading of the folder failed, and each job looked at every taken name from
    /// image-1, which took minutes on a share (#220).
    #[test]
    fn neutral_names_go_past_names_taken_in_a_folder_that_cannot_be_read() {
        use crate::files::file_io::LOOKS_FAIL;
        use crate::support::threads::lock_even_if_poisoned;
        let dir =
            std::env::temp_dir().join(format!("glimt-test-sharing-drop-{}", std::process::id()));
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();
        let a = dir.join("a.png");
        image::RgbImage::new(4, 4).save(&a).unwrap();
        for n in 1..=1000 {
            std::fs::write(out.join(format!("image-{n}.png")), b"").unwrap();
        }
        lock_even_if_poisoned(&LOOKS_FAIL).push(out.clone());
        for job in [1001, 1002] {
            let mut outcome = super::SharingOutcome::default();
            crate::files::file_io::LOOKS_MADE.set(0);
            outcome.copy_into(
                (&a, &|_| Ok(())),
                &out,
                &Default::default(),
                super::Naming::Neutral,
            );
            let looks = crate::files::file_io::LOOKS_MADE.get();
            assert!(looks < 40, "{looks} looks");
            assert_eq!((outcome.copied, outcome.names_taken), (1, 0));
            assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
            let made = out.join(format!("image-{job}.png"));
            assert_eq!(image::image_dimensions(made).unwrap(), (4, 4));
        }
        lock_even_if_poisoned(&LOOKS_FAIL).retain(|p| p != &out);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A copy left out for a sidecar at its own name without its image, or for a look that
    /// failed, is told by that reason, with the sidecar's path and what to do about it. Before,
    /// both were told as "the folder has files of those names", which the user could not find.
    #[test]
    fn a_name_with_only_a_sidecar_is_told_by_that_sidecar() {
        use crate::files::file_io::LOOKS_FAIL;
        use crate::support::threads::lock_even_if_poisoned;
        let dir = std::env::temp_dir().join(format!(
            "glimt-test-sharing-leftover-{}",
            std::process::id()
        ));
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();
        let (a, b) = (dir.join("a.png"), dir.join("b.png"));
        image::RgbImage::new(4, 4).save(&a).unwrap();
        image::RgbImage::new(4, 4).save(&b).unwrap();
        let leftover = out.join("a.png.xmp");
        std::fs::write(&leftover, "left").unwrap();
        lock_even_if_poisoned(&LOOKS_FAIL).push(out.join("b.png"));
        let mut outcome = super::SharingOutcome::default();
        outcome.copy_into(
            (&a, &|_| Ok(())),
            &out,
            &Default::default(),
            super::Naming::Own,
        );
        outcome.copy_into(
            (&b, &|_| Ok(())),
            &out,
            &Default::default(),
            super::Naming::Own,
        );
        lock_even_if_poisoned(&LOOKS_FAIL).retain(|p| !p.starts_with(&out));
        assert_eq!((outcome.copied, outcome.names_taken), (0, 0));
        let told = outcome.errors.join("\n");
        assert!(
            told.contains(&format!("{} is a sidecar", leftover.display())),
            "{told}"
        );
        assert!(told.contains("move or delete it"), "{told}");
        assert!(told.contains("could not look at"), "{told}");
        assert_eq!(std::fs::read_to_string(&leftover).unwrap(), "left");
        assert!(!out.join("a.png").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// With the file name left out, the copies are named "image-1.EXT", "image-2.EXT" …,
    /// numbered past every name in the folder and every leftover sidecar's name, and never
    /// over a file. Before, a copy of PXL_20231004_142355.png kept that name, and the date in
    /// it, whatever the user left out.
    #[test]
    fn copies_without_their_file_names_get_neutral_names() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-sharing-neutral-{}", std::process::id()));
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();
        let (a, b) = (dir.join("PXL_20231004_142355.png"), dir.join("b.png"));
        image::RgbImage::new(4, 4).save(&a).unwrap();
        image::RgbImage::new(5, 5).save(&b).unwrap();
        std::fs::write(out.join("image-1.png"), b"someone else's").unwrap();
        std::fs::write(out.join("image-2.png.xmp"), b"a leftover sidecar").unwrap();
        let mut outcome = super::SharingOutcome::default();
        outcome.copy_into(
            (&a, &|_| Ok(())),
            &out,
            &Default::default(),
            super::Naming::Neutral,
        );
        outcome.copy_into(
            (&b, &|_| Ok(())),
            &out,
            &Default::default(),
            super::Naming::Neutral,
        );
        assert_eq!(
            (outcome.copied, outcome.renamed, outcome.names_taken),
            (2, 0, 0)
        );
        assert_eq!(
            std::fs::read(out.join("image-1.png")).unwrap(),
            b"someone else's"
        );
        assert!(!out.join("image-2.png").exists());
        assert_eq!(
            image::image_dimensions(out.join("image-3.png")).unwrap(),
            (4, 4)
        );
        assert_eq!(
            image::image_dimensions(out.join("image-4.png")).unwrap(),
            (5, 5)
        );
        assert!(!out.join("PXL_20231004_142355.png").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
