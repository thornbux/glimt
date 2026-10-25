//! File → Save As: the shown image saved again, smaller or in another format, through the file
//! dialog; never over the image itself.

use crate::save::{CopyAs, Format};
use crate::*;

/// What the Save As dialog asks; kept from one use to the next while Glimt runs.
#[derive(Clone, Copy)]
pub struct Choice {
    /// `None`: as the original.
    format: Option<Format>,
    /// 0 the original size, 1 the longest side in pixels, 2 a percentage.
    size: u8,
    long: u32,
    percent: u32,
    quality: u8,
    strip: bool,
}

impl Default for Choice {
    fn default() -> Self {
        Choice {
            format: None,
            size: 0,
            long: 1920,
            percent: 50,
            quality: 90,
            strip: false,
        }
    }
}

/// The dialog open, for the image `path` as shown (`how`), `orig` pixels large.
pub struct SaveAs {
    path: PathBuf,
    how: CopyAs,
    orig: [usize; 2],
    choice: Choice,
}

impl Choice {
    /// The longest side asked for, if smaller than the image's `long` side.
    fn long_side(&self, long: usize) -> Option<u32> {
        let side = match self.size {
            1 => self.long,
            2 => (long as f64 * self.percent as f64 / 100.0).round() as u32,
            _ => return None,
        };
        ((side as usize) < long).then_some(side.max(1))
    }
}

impl crate::App {
    /// File → Save As (Ctrl+Shift+S): the dialog for the shown image.
    pub(crate) fn start_save_as(&mut self) {
        let cur = self.list.cur;
        if self.list.files.is_empty() {
            return;
        }
        #[cfg(feature = "video")]
        if (self.list.files.get(cur)).is_some_and(|f| is_video(Path::new(f))) {
            self.notice = Some(tr("A video is not saved as an image").into());
            return;
        }
        let Some(slot) = self.list.slots.get(&cur) else {
            return;
        };
        let [w, h] = slot.orig;
        let orig = if self.orient.0 % 2 == 1 {
            [h, w]
        } else {
            [w, h]
        };
        let mut choice = self.save_as_last;
        // The quality starts at the one in Preferences for its format.
        let path = self.path(cur);
        choice.quality = match choice.format.unwrap_or_else(|| Format::of(&path)) {
            Format::WebPLossy => self.config.webp_quality,
            _ => self.config.jpeg_quality,
        };
        let how = self.shown_copy();
        self.save_as = Some(SaveAs {
            path,
            how,
            orig,
            choice,
        });
    }

    /// The shown image as a copy saves it: turned as it is shown (by hand, and by its EXIF with
    /// Auto Rotate), the page shown, at the qualities in Preferences; whole, in its own format,
    /// with its metadata. Crop and Save As start from it, so neither can leave out a part of how
    /// the image is shown, as the page was.
    pub(crate) fn shown_copy(&self) -> CopyAs {
        let (turns, mirrored) = self.orient;
        CopyAs {
            page: self.page,
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
        let mut go = false;
        let modal = egui::Modal::new(egui::Id::unique("save as")).show(ctx, |ui| {
            let c = &mut dialog.choice;
            let original = Format::of(&dialog.path);
            ui.strong(tr("Save As"));
            ui.label(
                dialog
                    .path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy(),
            );
            let name = |f: Format| match f {
                Format::Jpeg => "JPEG",
                Format::Png => "PNG",
                Format::WebP => tr("WebP, lossless"),
                Format::WebPLossy => tr("WebP, lossy"),
            };
            egui::Grid::new("save as grid")
                .num_columns(2)
                .show(ui, |ui| {
                    ui.label(tr("Format:"));
                    let shown = match c.format {
                        None => tf!("As the original ({format})", format = name(original)),
                        Some(f) => name(f).to_owned(),
                    };
                    let combo = egui::ComboBox::from_id_salt("save as format")
                        .selected_text(&shown)
                        .show_ui(ui, |ui| {
                            let text = tf!("As the original ({format})", format = name(original));
                            ui.selectable_value(&mut c.format, None, text);
                            for f in [Format::Jpeg, Format::Png, Format::WebP, Format::WebPLossy] {
                                noted(
                                    &ui.selectable_value(&mut c.format, Some(f), name(f)),
                                    name(f),
                                );
                            }
                        });
                    noted(&combo.response, &shown);
                    ui.end_row();
                    let [w, h] = dialog.orig;
                    ui.label(tr("Size:"));
                    ui.vertical(|ui| {
                        ui.radio_value(&mut c.size, 0, tf!("Original ({w} × {h})", w, h));
                        ui.horizontal(|ui| {
                            noted(
                                &ui.radio_value(&mut c.size, 1, tr("Longest side")),
                                tr("Longest side"),
                            );
                            let drag = egui::DragValue::new(&mut c.long).range(1..=65535);
                            if ui.add(drag.suffix(" px")).changed() {
                                c.size = 1;
                            }
                        });
                        ui.horizontal(|ui| {
                            noted(
                                &ui.radio_value(&mut c.size, 2, tr("Percent")),
                                tr("Percent"),
                            );
                            let drag = egui::DragValue::new(&mut c.percent).range(1..=100);
                            if ui.add(drag.suffix(" %")).changed() {
                                c.size = 2;
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
                    let format = c.format.unwrap_or(original);
                    if matches!(format, Format::Jpeg | Format::WebPLossy) {
                        ui.label(tr("Quality:"));
                        ui.add(egui::Slider::new(&mut c.quality, 50..=100));
                        ui.end_row();
                    }
                });
            ui.checkbox(
                &mut c.strip,
                tr("Remove EXIF and GPS (camera, date, place), XMP and the sidecar"),
            );
            ui.horizontal(|ui| {
                go = noted(&ui.button(tr("Save…")), tr("Save…")).clicked();
                if ui.button(tr("Cancel (Esc)")).clicked() {
                    ui.close();
                }
            });
        });
        self.save_as_last = dialog.choice;
        if go {
            let c = dialog.choice;
            let [w, h] = dialog.orig;
            let format = c.format.unwrap_or_else(|| Format::of(&dialog.path));
            let how = CopyAs {
                long_side: c.long_side(w.max(h)),
                format: c.format,
                jpeg_quality: c.quality,
                webp_quality: c.quality,
                strip: c.strip,
                ..dialog.how.clone()
            };
            // "a-1920.jpg" when smaller, "a.webp" in another format, else "a-copy.jpg".
            let path = &dialog.path;
            let stem = path.file_stem().unwrap_or_default().to_string_lossy();
            let mut name = match how.long_side {
                Some(long) => format!("{stem}-{long}.{}", format.ext()),
                None => format!("{stem}.{}", format.ext()),
            };
            if path.with_file_name(&name) == *path {
                name = format!("{stem}-copy.{}", format.ext());
            }
            let start = path.with_file_name(name);
            self.pick(ctx, false, Pick::SaveAs(dialog.path, how, start));
        } else if !modal.should_close() {
            self.save_as = Some(dialog);
        }
    }

    /// Save As's file chosen: the copy saved there in the background, the folder read again.
    pub(crate) fn save_as_to(
        &mut self,
        ctx: &egui::Context,
        path: PathBuf,
        how: CopyAs,
        to: PathBuf,
    ) {
        let deleted = self.deleted_tx.clone();
        self.background(ctx, vec![path.clone(), to.clone()], move || {
            Some(match decode::survive(|| save::save_as(&path, &how, &to)) {
                Ok(new) => {
                    let _ = deleted.send(Deleted {
                        relist: true,
                        ..Default::default()
                    });
                    tf!("Saved {file}", file = new.display())
                }
                Err(e) => tf!("Could not save the image: {e}", e),
            })
        });
    }
}
