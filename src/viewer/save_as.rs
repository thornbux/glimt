//! File → Save As: the shown image saved again, smaller or in another format, through the file
//! dialog; never over the image itself.

#[cfg(feature = "video")]
use crate::files::external::is_video;
use crate::files::file_io::{FileFingerprint, PickedFiles};
use crate::files::file_ops::OperationReport;
use crate::images::decode;
use crate::images::save;
use crate::images::save::{CopyAs, Format};
use crate::support::lang::tr;
use crate::support::test_hooks::noted;
use crate::support::threads::{Receiving, Took, Working, answer_channel};
use crate::tf;
use crate::ui::dialogs::Pick;
use eframe::egui;
#[cfg(feature = "video")]
use std::path::Path;
use std::path::PathBuf;

/// What the Save As dialog asks; kept from one use to the next while Glimt runs.
#[derive(Clone, Copy)]
pub struct SaveAsChoices {
    /// `None`: as the original.
    format: Option<Format>,
    /// 0 the original size, 1 the longest side in pixels, 2 a percentage.
    size_mode: u8,
    longest_side: u32,
    percent: u32,
    quality: u8,
    strip_metadata: bool,
}

impl Default for SaveAsChoices {
    fn default() -> Self {
        SaveAsChoices {
            format: None,
            size_mode: 0,
            longest_side: 1920,
            percent: 50,
            quality: 90,
            strip_metadata: false,
        }
    }
}

impl SaveAsChoices {
    /// The longest side asked for, if smaller than the image's `long` side.
    fn long_side(&self, long: usize) -> Option<u32> {
        let side = match self.size_mode {
            1 => self.longest_side,
            2 => (long as f64 * self.percent as f64 / 100.0).round() as u32,
            _ => return None,
        };
        ((side as usize) < long).then_some(side.max(1))
    }
}

/// The dialog open, for the image `path` as shown (`copy_as`), `original_size` pixels large.
pub struct SaveAs {
    path: PathBuf,
    /// The image shown, with its fingerprint taken while the dialog and the chooser are open.
    picked: PickedFiles,
    copy_as: CopyAs,
    original_size: [usize; 2],
    choice: SaveAsChoices,
    /// The format the image is saved in "as the original", or `None` until it is looked up.
    original_format: Option<Format>,
    /// The look-up of `original_format`, in the background, because a WebP file is read for it,
    /// and the image may be on a share that stopped answering. Save… waits for it, so it keeps
    /// the busy pointer until the dialog has taken its answer.
    original_format_answer: Receiving<(Format, Working)>,
}

impl crate::App {
    /// File → Save As (Ctrl+Shift+S): the dialog for the shown image.
    pub(crate) fn start_save_as(&mut self, ctx: &egui::Context) {
        let shown = self.list.shown();
        #[cfg(feature = "video")]
        if (self.list.files().get(shown)).is_some_and(|f| is_video(Path::new(f))) {
            self.notice
                .give(tr("A video is not saved as an image").into());
            return;
        }
        let Some(loaded) = self.list.loaded_images().get(&shown) else {
            return;
        };
        let [w, h] = loaded.original_size;
        let original_size = if self.viewing.is_sideways() {
            [h, w]
        } else {
            [w, h]
        };
        let mut choice = self.save_as_choices;
        // The quality starts at the one in Preferences for its format, as soon as it is known.
        if let Some(format) = choice.format {
            choice.quality = self.quality_in_preferences(format);
        }
        let path = self.path(shown);
        let (tx, rx) = answer_channel(&self.busy);
        let (looked_up, ctx) = (path.clone(), ctx.clone());
        let pointer = Working::new(&self.busy);
        std::thread::spawn(move || {
            let _pointer = pointer;
            let _ = tx.send(Format::of(&looked_up));
            ctx.request_repaint();
        });
        let copy_as = self.shown_copy();
        self.save_as = Some(SaveAs {
            picked: PickedFiles::listed(vec![self.listed_file(shown)]),
            path,
            copy_as,
            original_size,
            choice,
            original_format: None,
            original_format_answer: Receiving::of(rx),
        });
    }

    /// Returns the quality that Preferences gives for saving in `format`.
    fn quality_in_preferences(&self, format: Format) -> u8 {
        match format {
            Format::WebPLossy => self.config.webp_quality,
            _ => self.config.jpeg_quality,
        }
    }

    /// The shown image as a copy saves it: turned as it is shown (by hand, and by its EXIF with
    /// Auto Rotate), the page shown, at the qualities in Preferences; whole, in its own format,
    /// with its metadata. Crop and Save As start from it, so neither can leave out a part of how
    /// the image is shown, as the page was.
    pub(crate) fn shown_copy(&self) -> CopyAs {
        let (turns, mirrored) = self.viewing.turn();
        CopyAs {
            page: self.viewing.page(),
            rotate: self.config.exif_rotate,
            turns,
            mirrored,
            crop: None,
            long_side: None,
            format: None,
            jpeg_quality: self.config.jpeg_quality,
            webp_quality: self.config.webp_quality,
            strip: false,
        }
    }

    /// The Save As dialog: format, size, quality and metadata; Save… asks where.
    pub(crate) fn save_as_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.save_as.take() else {
            return;
        };
        if let Took::Answer((format, _pointer)) = dialog.original_format_answer.take() {
            dialog.original_format = Some(format);
            if dialog.choice.format.is_none() {
                dialog.choice.quality = self.quality_in_preferences(format);
            }
        }
        let mut go = false;
        let modal = egui::Modal::new(egui::Id::unique("save as")).show(ctx, |ui| {
            ui.strong(tr("Save As"));
            ui.label(
                dialog
                    .path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy(),
            );
            save_as_fields(ui, &mut dialog);
            crate::ui::menus::checkbox(
                ui,
                &mut dialog.choice.strip_metadata,
                tr("Remove EXIF and GPS (camera, date, place), XMP and the sidecar"),
            );
            ui.horizontal(|ui| {
                go = crate::ui::menus::button(ui, tr("Save…")).clicked();
                if crate::ui::menus::button(ui, tr("Cancel (Esc)")).clicked() {
                    ui.close();
                }
            });
        });
        self.save_as_choices = dialog.choice;
        // Until the original's format is known, Save… waits for it.
        let format = dialog.choice.format.or(dialog.original_format);
        if let Some(format) = format.filter(|_| go) {
            let c = dialog.choice;
            let [w, h] = dialog.original_size;
            let copy_as = CopyAs {
                long_side: c.long_side(w.max(h)),
                format: c.format,
                jpeg_quality: c.quality,
                webp_quality: c.quality,
                strip: c.strip_metadata,
                ..dialog.copy_as.clone()
            };
            // "a-1920.jpg" when smaller, "a.webp" in another format, else "a-copy.jpg".
            let path = &dialog.path;
            let mut name = match copy_as.long_side {
                Some(long) => save::stem_with_suffix(path, &format!("-{long}.{}", format.ext())),
                None => save::stem_with_suffix(path, &format!(".{}", format.ext())),
            };
            if path.with_file_name(&name) == *path {
                name = save::stem_with_suffix(path, &format!("-copy.{}", format.ext()));
            }
            let start = path.with_file_name(name);
            self.pick(ctx, false, Pick::SaveAs(dialog.picked, copy_as, start));
        } else if !modal.should_close() {
            self.save_as = Some(dialog);
        }
    }

    /// Save As's file chosen, `to`, with the fingerprint of the file there when it was picked:
    /// the copy of the image `picked` saved there in the background, the folder read again. An
    /// image that is no longer the one picked is not saved, nor is a file at `to` that is no
    /// longer the one the chooser let it replace written over.
    pub(crate) fn save_as_to(
        &mut self,
        ctx: &egui::Context,
        picked: PickedFiles,
        copy_as: CopyAs,
        (to, at_pick): (PathBuf, Option<FileFingerprint>),
    ) {
        let report_tx = self.file_operations.report_sender();
        let Some(path) = picked.paths().first().cloned() else {
            return;
        };
        self.run_in_background(ctx, vec![path.clone(), to.clone()], move || {
            let (_, picked) = picked.wait_for_fingerprints();
            let saved = picked
                .still_the_one_picked(&path)
                .map_err(|e| format!("{}: {e}", path.display()))
                .and_then(|()| {
                    decode::catch_panic_as_error(|| save::save_as(&path, &copy_as, &to, at_pick))
                });
            Some(match saved {
                Ok(new) => {
                    // Saved over a file the dialog let it replace, maybe one in the list. The
                    // copy gets the marks and stars of the original, as it got its sidecar,
                    // unless it was stripped.
                    let _ = report_tx.send(OperationReport {
                        read_folder_again: true,
                        overwritten: vec![new.clone()],
                        moved: (!copy_as.strip)
                            .then(|| (path.clone(), new.clone()))
                            .into_iter()
                            .collect(),
                        is_copy: true,
                        ..Default::default()
                    });
                    crate::support::threads::Told::Notice(tf!("Saved {file}", file = new.display()))
                }
                Err(e) => crate::support::threads::Told::FailedWrite(tf!(
                    "Could not save the image: {e}",
                    e
                )),
            })
        });
    }
}

/// Save As's format, size and quality, in a grid.
fn save_as_fields(ui: &mut egui::Ui, dialog: &mut SaveAs) {
    // "…" while the original's format is still looked up.
    let original_format = dialog.original_format;
    let original = original_format.map_or("…", format_name);
    let [w, h] = dialog.original_size;
    let c = &mut dialog.choice;
    egui::Grid::new("save as grid")
        .num_columns(2)
        .show(ui, |ui| {
            ui.label(tr("Format:"));
            let shown = match c.format {
                None => tf!("As the original ({format})", format = original),
                Some(f) => format_name(f).to_owned(),
            };
            let combo = egui::ComboBox::from_id_salt("save as format")
                .selected_text(&shown)
                .show_ui(ui, |ui| {
                    let text = tf!("As the original ({format})", format = original);
                    crate::ui::menus::choice(ui, &mut c.format, None, &text);
                    for f in [Format::Jpeg, Format::Png, Format::WebP, Format::WebPLossy] {
                        crate::ui::menus::choice(ui, &mut c.format, Some(f), format_name(f));
                    }
                });
            noted(&combo.response, &shown);
            ui.end_row();
            ui.label(tr("Size:"));
            ui.vertical(|ui| {
                crate::ui::menus::radio_choice(
                    ui,
                    &mut c.size_mode,
                    0,
                    &tf!("Original ({w} × {h})", w, h),
                );
                ui.horizontal(|ui| {
                    crate::ui::menus::radio_choice(ui, &mut c.size_mode, 1, tr("Longest side"));
                    let drag = egui::DragValue::new(&mut c.longest_side).range(1..=65535);
                    if ui.add(drag.suffix(" px")).changed() {
                        c.size_mode = 1;
                    }
                });
                ui.horizontal(|ui| {
                    crate::ui::menus::radio_choice(ui, &mut c.size_mode, 2, tr("Percent"));
                    let drag = egui::DragValue::new(&mut c.percent).range(1..=100);
                    if ui.add(drag.suffix(" %")).changed() {
                        c.size_mode = 2;
                    }
                });
            });
            ui.end_row();
            ui.label("");
            let (nw, nh) = match c.long_side(w.max(h)) {
                Some(long) => {
                    let s = long as f64 / w.max(h) as f64;
                    let side = |n: usize| ((n as f64 * s).round() as usize).max(1);
                    (side(w), side(h))
                }
                None => (w, h),
            };
            ui.weak(tf!("Saved {w} × {h}; never enlarged", w = nw, h = nh));
            ui.end_row();
            let format = c.format.or(original_format);
            if matches!(format, Some(Format::Jpeg | Format::WebPLossy)) {
                ui.label(tr("Quality:"));
                ui.add(egui::Slider::new(&mut c.quality, 50..=100));
                ui.end_row();
            }
        });
}

/// A format's name in Save As.
fn format_name(f: Format) -> &'static str {
    match f {
        Format::Jpeg => "JPEG",
        Format::Png => "PNG",
        Format::WebP => tr("WebP, lossless"),
        Format::WebPLossy => tr("WebP, lossy"),
    }
}

#[cfg(test)]
mod tests {
    /// Save As does not save a copy of a photo that took the name of the image shown while its
    /// dialog and the chooser were open, and tells why.
    #[test]
    fn saves_only_the_image_picked() {
        use crate::support::test_support::{app_on, contract_folder, run_background_until};
        let dir = contract_folder("save-as-swapped");
        let a = dir.join("a.png");
        let picked = (a.clone(), crate::files::file_io::fingerprint(&a).unwrap());
        let picked = crate::files::file_io::PickedFiles::recorded(vec![picked]);
        std::fs::remove_file(&a).unwrap();
        image::RgbImage::new(3, 3).save(&a).unwrap();
        let (mut app, ctx) = app_on(&dir);
        let (how, to) = (app.shown_copy(), dir.join("copy.png"));
        app.save_as_to(&ctx, picked, how, (to.clone(), None));
        assert!(run_background_until(&mut app, &ctx, |app| !app.working()
            && app.notice.text().is_some()));
        assert!(!to.exists());
        let told = app.notice.text().unwrap_or_default().to_owned();
        assert!(told.contains("changed"), "{told}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The look-up of the image's format for Save As keeps the busy pointer until the dialog
    /// has taken its answer, as Save… waits for it. Before, it had none, so `--remote … --wait`
    /// could answer while it still ran.
    #[test]
    fn the_format_look_up_keeps_the_busy_pointer() {
        use crate::support::test_support::{app_on, contract_folder, run_background_until};
        let dir = contract_folder("save-as-pointer");
        let (mut app, ctx) = app_on(&dir);
        assert!(run_background_until(&mut app, &ctx, |app| !app.working()));
        app.start_save_as(&ctx);
        // The answer comes in a moment, and the dialog, not drawn here, never takes it.
        std::thread::sleep(std::time::Duration::from_millis(100));
        app.poll_background(&ctx);
        assert!(app.working());
        drop(app);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
