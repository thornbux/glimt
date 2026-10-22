//! Edit → Crop: a part of the shown image dragged out over it, saved as a new file beside it.

use crate::*;

/// The part being chosen of image `file` of the list, as fractions (0–1) of the image as shown,
/// turned; `None` until a drag.
pub struct Crop {
    file: usize,
    anchor: egui::Pos2,
    part: Option<egui::Rect>,
}

impl crate::App {
    /// Edit → Crop: choose a part of the shown image. Not for a video, nor before the image
    /// is decoded.
    pub(crate) fn start_crop(&mut self) {
        let cur = self.list.cur;
        #[cfg(feature = "video")]
        if (self.list.files.get(cur)).is_some_and(|f| is_video(Path::new(f))) {
            self.notice = Some(tr("A video is not cropped").into());
            return;
        }
        if self.list.slots.contains_key(&cur) {
            self.crop = Some(Crop {
                file: cur,
                anchor: egui::Pos2::ZERO,
                part: None,
            });
        }
    }

    /// Over the shown image at `bounds` on screen, `pixels` large as shown: a drag chooses the
    /// part, drawn with the rest darkened, its size in pixels, and a bar to save or cancel.
    pub(crate) fn crop_input(
        &mut self,
        ctx: &egui::Context,
        ui: &egui::Ui,
        response: &egui::Response,
        bounds: egui::Rect,
        pixels: [usize; 2],
    ) {
        let Some(crop) = &mut self.crop else {
            return;
        };
        if crop.file != self.list.cur {
            self.crop = None;
            return;
        }
        let at = |p: egui::Pos2| {
            let p = bounds.clamp(p);
            egui::pos2(
                (p.x - bounds.left()) / bounds.width(),
                (p.y - bounds.top()) / bounds.height(),
            )
        };
        if response.drag_started()
            && let Some(p) = ctx.input(|i| i.pointer.press_origin())
        {
            crop.anchor = at(p);
        }
        if response.dragged()
            && let Some(p) = response.interact_pointer_pos()
        {
            let part = egui::Rect::from_two_pos(crop.anchor, at(p));
            crop.part = (part.width() * bounds.width() >= 2.0
                && part.height() * bounds.height() >= 2.0)
                .then_some(part);
        }
        let p = ui.painter();
        let shade = egui::Color32::from_black_alpha(150);
        if let Some(part) = crop.part {
            let to_screen = |q: egui::Pos2| bounds.min + q.to_vec2() * bounds.size();
            let sel = egui::Rect::from_min_max(to_screen(part.min), to_screen(part.max));
            for r in [
                egui::Rect::from_x_y_ranges(bounds.x_range(), bounds.top()..=sel.top()),
                egui::Rect::from_x_y_ranges(bounds.x_range(), sel.bottom()..=bounds.bottom()),
                egui::Rect::from_x_y_ranges(bounds.left()..=sel.left(), sel.y_range()),
                egui::Rect::from_x_y_ranges(sel.right()..=bounds.right(), sel.y_range()),
            ] {
                p.rect_filled(r, 0.0, shade);
            }
            let stroke = egui::Stroke::new(1.5, egui::Color32::WHITE);
            p.rect_stroke(sel, 0.0, stroke, egui::StrokeKind::Outside);
            let [_, _, w, h] = part_pixels(part, pixels);
            let text = format!("{w} × {h}");
            let font = egui::FontId::proportional(14.0);
            let galley = p.layout_no_wrap(text, font, egui::Color32::WHITE);
            let at = sel.left_top() + egui::vec2(6.0, 6.0);
            p.rect_filled(
                egui::Rect::from_min_size(at, galley.size()).expand(3.0),
                3.0,
                shade,
            );
            p.galley(at, galley, egui::Color32::WHITE);
        }
        let chosen = crop.part.is_some();
        let (mut save, mut cancel) = (false, false);
        egui::Area::new(egui::Id::unique("crop bar"))
            .order(egui::Order::Foreground)
            .fixed_pos(response.rect.center_top() + egui::vec2(0.0, 8.0))
            .pivot(egui::Align2::CENTER_TOP)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(tr("Drag over the image to choose the part to keep"));
                        save = ui
                            .add_enabled(chosen, egui::Button::new(tr("Save Copy (Enter)")))
                            .clicked();
                        cancel = ui.button(tr("Cancel (Esc)")).clicked();
                    });
                });
            });
        if save {
            self.save_crop(ctx);
        } else if cancel {
            self.crop = None;
        }
    }

    /// Save the part chosen as a new file beside the image, in the background; the folder is
    /// read again to show it.
    pub(crate) fn save_crop(&mut self, ctx: &egui::Context) {
        let Some(Crop {
            file,
            part: Some(part),
            ..
        }) = self.crop.take()
        else {
            return;
        };
        let Some(slot) = self.list.slots.get(&file) else {
            return;
        };
        let (turns, mirrored) = self.orient;
        let [w, h] = slot.orig;
        let pixels = if turns % 2 == 1 { [h, w] } else { [w, h] };
        let rect = part_pixels(part, pixels);
        let path = self.path(file);
        let (rotate, jpeg, webp) = (
            self.config.exif_rotate,
            self.config.jpeg_quality,
            self.config.webp_quality,
        );
        let deleted = self.deleted_tx.clone();
        self.background(ctx, true, move || {
            let saved = crate::decode::survive(|| {
                crate::save::save_cropped(&path, rotate, turns, mirrored, rect, jpeg, webp)
            });
            Some(match saved {
                Ok(new) => {
                    let _ = deleted.send(Deleted {
                        paths: Vec::new(),
                        gone: Vec::new(),
                        failed: false,
                        moved: Vec::new(),
                        copied: false,
                        relist: true,
                    });
                    tf!("Saved {file}", file = new.display())
                }
                Err(e) => tf!("Could not crop the image: {e}", e),
            })
        });
    }
}

/// `part` (fractions of the image) in whole pixels of an image `pixels` large: x, y, width,
/// height, at least one pixel, within the image.
fn part_pixels(part: egui::Rect, [w, h]: [usize; 2]) -> [u32; 4] {
    let (w, h) = (w as f32, h as f32);
    let x = (part.left() * w).round().clamp(0.0, w - 1.0);
    let y = (part.top() * h).round().clamp(0.0, h - 1.0);
    let right = (part.right() * w).round().clamp(x + 1.0, w);
    let bottom = (part.bottom() * h).round().clamp(y + 1.0, h);
    [x as u32, y as u32, (right - x) as u32, (bottom - y) as u32]
}

#[cfg(test)]
mod tests {
    use eframe::egui;

    /// A part in whole pixels, inside the image and never empty.
    #[test]
    fn rounds_the_part_to_pixels() {
        let r =
            |a: [f32; 4]| egui::Rect::from_min_max(egui::pos2(a[0], a[1]), egui::pos2(a[2], a[3]));
        assert_eq!(
            super::part_pixels(r([0.0, 0.0, 1.0, 1.0]), [300, 200]),
            [0, 0, 300, 200]
        );
        assert_eq!(
            super::part_pixels(r([0.25, 0.5, 0.75, 1.0]), [300, 200]),
            [75, 100, 150, 100]
        );
        assert_eq!(
            super::part_pixels(r([1.0, 1.0, 1.0, 1.0]), [300, 200]),
            [299, 199, 1, 1]
        );
    }
}
